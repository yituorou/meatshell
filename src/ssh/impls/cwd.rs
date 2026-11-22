//! Directory notifications from complete terminal control strings.
//!
//! `sudo` can discard the login shell's OSC 7 hook. Standard Bash configurations
//! may still report `root@host: /absolute/path` through an OSC 0/2 window title.
//! Accept that bounded title convention as a fallback, never visible prompt
//! text: a prompt has no end marker and cannot be distinguished from command
//! output or the prefix of a longer path. Nothing here writes to the PTY.

pub(super) struct CwdTracker {
    osc: OscReader,
    host: Option<String>,
    authoritative: bool,
    title_fallback: bool,
    /// The last directory reported.
    last: Option<String>,
    /// The latest prompt cycle was cancelled (Ctrl+C), not submitted.
    cancelled: bool,
}

impl CwdTracker {
    pub(super) fn new(title_fallback: bool) -> Self {
        Self {
            osc: OscReader::default(),
            host: None,
            authoritative: false,
            title_fallback,
            last: None,
            cancelled: false,
        }
    }

    pub(super) fn input(&mut self, bytes: &[u8]) {
        // Do not retain or interpret commands/passwords. A submission or cancel
        // starts a new prompt cycle; until then a real OSC 7 remains authoritative.
        // Only the last such key counts: Ctrl+C then Enter is a submission.
        if let Some(&key) = bytes.iter().rev().find(|byte| matches!(byte, b'\r' | b'\n' | 3)) {
            self.authoritative = false;
            self.cancelled = key == 3;
        }
    }

    pub(super) fn feed(&mut self, text: &str) -> Option<String> {
        let mut reported = None;
        for byte in text.bytes() {
            let path = match self.osc.feed(byte) {
                Some(Notification::Cwd { host, path }) => {
                    // The first login hook identifies the SFTP host. A later
                    // nested SSH must not rebind the title fallback's host.
                    if self.host.is_none() && !host.is_empty() {
                        self.host = Some(host);
                    }
                    self.authoritative = true;
                    path
                }
                Some(Notification::RootTitle { host, path })
                    if self.title_fallback
                        && !self.authoritative
                        && self
                            .host
                            .as_deref()
                            .is_some_and(|expected| same_host(&host, expected)) =>
                {
                    path
                }
                _ => continue,
            };
            // Every prompt re-reports the directory, and after the user browsed
            // the SFTP panel by hand the next report snaps the panel back to the
            // shell (#59). A prompt redrawn by Ctrl+C ran nothing, so it must
            // not move the panel (#577); a submitted line or a real cd still does.
            if self.cancelled && self.last.as_deref() == Some(path.as_str()) {
                continue;
            }
            self.last = Some(path.clone());
            reported = Some(path);
        }
        reported
    }
}

enum Notification {
    Cwd { host: String, path: String },
    RootTitle { host: String, path: String },
}

fn parse_root_title(title: &str) -> Option<Notification> {
    let (host, path) = title.strip_prefix("root@")?.split_once(':')?;
    // Ubuntu's Bash title uses one separator space. The path itself is literal,
    // not a URI; do not trim it or percent-decode valid file names.
    let path = path.strip_prefix(' ').unwrap_or(path);
    if !path.starts_with('/')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|part| part == "..." || part.contains('…'))
    {
        return None;
    }
    // sudo -s may preserve HOME, so neither ~ nor relative titles identify a
    // directory. A custom root home need not be /root either.
    Some(Notification::RootTitle {
        host: host.to_string(),
        path: path.to_string(),
    })
}

fn same_host(actual: &str, expected: &str) -> bool {
    if actual.is_empty()
        || !actual
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
    {
        return false;
    }
    let actual = actual.trim_end_matches('.');
    let expected = expected.trim_end_matches('.');
    actual.eq_ignore_ascii_case(expected)
        || ((!actual.contains('.') || !expected.contains('.'))
            && actual
                .split('.')
                .next()
                .unwrap_or("")
                .eq_ignore_ascii_case(expected.split('.').next().unwrap_or("")))
}

#[derive(Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Osc,
    OscEscape,
    IgnoredString,
    IgnoredStringEscape,
}

/// Bounded streaming reader. No notification is complete until BEL or ESC \\,
/// even if an SSH packet ends at a plausible prompt. Other control strings
/// (DCS/SOS/PM/APC) cannot contribute nested, apparent OSC notifications.
#[derive(Default)]
struct OscReader {
    state: State,
    overflow: bool,
    content: Vec<u8>,
}

impl OscReader {
    fn feed(&mut self, byte: u8) -> Option<Notification> {
        // CAN/SUB cancel an in-progress terminal control string.
        if matches!(byte, 0x18 | 0x1a) {
            self.state = State::Ground;
            self.content.clear();
            return None;
        }
        match self.state {
            State::Ground if byte == 0x1b => self.state = State::Escape,
            State::Escape | State::OscEscape => {
                if matches!(self.state, State::OscEscape) && byte == b'\\' {
                    return self.finish();
                }
                self.content.clear();
                self.state = match byte {
                    b']' => {
                        self.overflow = false;
                        State::Osc
                    }
                    b'P' | b'X' | b'^' | b'_' => State::IgnoredString,
                    0x1b => State::Escape,
                    _ => State::Ground,
                };
            }
            State::Osc => match byte {
                7 => return self.finish(),
                0x1b => self.state = State::OscEscape,
                _ if self.content.len() < 16 * 1024 => self.content.push(byte),
                _ => self.overflow = true,
            },
            State::IgnoredString if byte == 0x1b => self.state = State::IgnoredStringEscape,
            State::IgnoredStringEscape => {
                self.state = match byte {
                    b'\\' => State::Ground,
                    0x1b => State::IgnoredStringEscape,
                    _ => State::IgnoredString,
                };
            }
            _ => {}
        }
        None
    }

    fn finish(&mut self) -> Option<Notification> {
        let notification = (!self.overflow)
            .then(|| parse_notification(&self.content))
            .flatten();
        self.state = State::Ground;
        self.content.clear();
        notification
    }
}

fn parse_notification(content: &[u8]) -> Option<Notification> {
    let text = std::str::from_utf8(content).ok()?;
    if let Some(title) = text.strip_prefix("0;").or_else(|| text.strip_prefix("2;")) {
        parse_root_title(title)
    } else {
        let (host, path) = parse_osc7(content)?;
        Some(Notification::Cwd { host, path })
    }
}

fn parse_osc7(content: &[u8]) -> Option<(String, String)> {
    let uri = std::str::from_utf8(content)
        .ok()?
        .strip_prefix("7;file://")?;
    let slash = uri.find('/')?;
    let (host, encoded) = uri.split_at(slash);
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut remaining = encoded.as_bytes();
    while let Some((&byte, rest)) = remaining.split_first() {
        if byte == b'%' && rest.len() >= 2 {
            if let (Some(hi), Some(lo)) = (
                (rest[0] as char).to_digit(16),
                (rest[1] as char).to_digit(16),
            ) {
                bytes.push((hi * 16 + lo) as u8);
                remaining = &rest[2..];
                continue;
            }
        }
        bytes.push(byte);
        remaining = rest;
    }
    let path = String::from_utf8(bytes).ok()?;
    if !path.starts_with('/') || path.chars().any(char::is_control) {
        return None;
    }
    Some((host.to_string(), path))
}

#[cfg(test)]
#[path = "cwd_tests.rs"]
mod tests;
