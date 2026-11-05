//! Per-tab session logging (#265).
//!
//! Records what the terminal *shows* — remote output, including the remote
//! echo of typed commands — as plain text with a timestamp per line, so users
//! who need an audit trail of their operations get a readable file. Keystrokes
//! are deliberately not recorded: passwords typed at no-echo prompts (sudo,
//! su, ssh) therefore never reach the log.
//!
//! ANSI/VT escape sequences are stripped; carriage return and backspace are
//! applied the way the screen applies them, so progress bars and readline
//! edits collapse into the line the user finally saw.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

/// Hard cap on one unterminated line, so a stream that never emits a newline
/// cannot grow the pending buffer without bound.
const MAX_LINE_CHARS: usize = 16 * 1024;
/// Longest control sequence we keep swallowing before giving up on it.
const MAX_SEQUENCE_CHARS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EscState {
    Normal,
    Esc,
    /// `ESC ( 0`-style designators: exactly one more char follows.
    EscOne,
    Csi,
    /// OSC / DCS / SOS / PM / APC payload, ended by BEL or ST.
    Str,
    StrEsc,
}

/// Plain-text transcript builder, independent of any file so it can be tested.
#[derive(Debug)]
pub(crate) struct TranscriptLines {
    state: EscState,
    seq_len: usize,
    line: String,
    line_chars: usize,
    line_started: Option<DateTime<Local>>,
    pending_cr: bool,
}

impl Default for TranscriptLines {
    fn default() -> Self {
        Self {
            state: EscState::Normal,
            seq_len: 0,
            line: String::new(),
            line_chars: 0,
            line_started: None,
            pending_cr: false,
        }
    }
}

impl TranscriptLines {
    /// Feed decoded terminal output. Every completed line is passed to `emit`
    /// with the time its first character arrived.
    pub(crate) fn feed(
        &mut self,
        text: &str,
        now: DateTime<Local>,
        emit: &mut dyn FnMut(DateTime<Local>, &str),
    ) {
        for ch in text.chars() {
            match self.state {
                EscState::Normal => self.normal(ch, now, emit),
                EscState::Esc => {
                    self.state = match ch {
                        '[' => EscState::Csi,
                        ']' | 'P' | 'X' | '^' | '_' => EscState::Str,
                        '(' | ')' | '*' | '+' | '-' | '.' | '/' | '#' | '%' | ' ' => {
                            EscState::EscOne
                        }
                        '\u{1b}' => EscState::Esc,
                        _ => EscState::Normal,
                    };
                    self.seq_len = 0;
                }
                EscState::EscOne => self.state = EscState::Normal,
                EscState::Csi => {
                    self.seq_len += 1;
                    if ('\u{40}'..='\u{7e}').contains(&ch) || self.seq_len > MAX_SEQUENCE_CHARS {
                        self.state = EscState::Normal;
                    } else if ch == '\u{1b}' {
                        self.state = EscState::Esc;
                    }
                }
                EscState::Str => {
                    self.seq_len += 1;
                    if ch == '\u{07}' || self.seq_len > MAX_SEQUENCE_CHARS {
                        self.state = EscState::Normal;
                    } else if ch == '\u{1b}' {
                        self.state = EscState::StrEsc;
                    }
                }
                EscState::StrEsc => {
                    self.state = if ch == '\\' {
                        EscState::Normal
                    } else {
                        EscState::Str
                    };
                }
            }
        }
    }

    fn normal(
        &mut self,
        ch: char,
        now: DateTime<Local>,
        emit: &mut dyn FnMut(DateTime<Local>, &str),
    ) {
        match ch {
            '\u{1b}' => {
                self.state = EscState::Esc;
                self.seq_len = 0;
            }
            '\n' => {
                self.pending_cr = false;
                self.finish_line(now, emit);
            }
            '\r' => self.pending_cr = true,
            '\u{08}' => {
                if self.line.pop().is_some() {
                    self.line_chars -= 1;
                }
            }
            // C1 CSI (8-bit form) — treat like ESC [.
            '\u{9b}' => {
                self.state = EscState::Csi;
                self.seq_len = 0;
            }
            '\t' => self.push(ch, now, emit),
            c if (c as u32) < 0x20 || c == '\u{7f}' || ('\u{80}'..='\u{9f}').contains(&c) => {}
            c => self.push(c, now, emit),
        }
    }

    fn push(&mut self, ch: char, now: DateTime<Local>, emit: &mut dyn FnMut(DateTime<Local>, &str)) {
        if self.pending_cr {
            // A bare CR followed by text overwrites the line (progress bars,
            // shell prompt redraws): keep only what the screen ends up showing.
            self.pending_cr = false;
            self.line.clear();
            self.line_chars = 0;
        }
        if self.line_started.is_none() {
            self.line_started = Some(now);
        }
        self.line.push(ch);
        self.line_chars += 1;
        if self.line_chars >= MAX_LINE_CHARS {
            self.finish_line(now, emit);
        }
    }

    fn finish_line(&mut self, now: DateTime<Local>, emit: &mut dyn FnMut(DateTime<Local>, &str)) {
        let started = self.line_started.take().unwrap_or(now);
        emit(started, self.line.trim_end());
        self.line.clear();
        self.line_chars = 0;
    }

    /// Emit a trailing unterminated line (e.g. the last prompt) if any.
    pub(crate) fn flush_partial(
        &mut self,
        now: DateTime<Local>,
        emit: &mut dyn FnMut(DateTime<Local>, &str),
    ) {
        if !self.line.trim_end().is_empty() {
            self.finish_line(now, emit);
        }
    }
}

/// What a tab needs to (re)open its session log at any time: the session's
/// own On/Off/Default choice plus the header details. Kept on the terminal
/// buffer so toggling the global setting applies to already-open tabs.
#[derive(Clone, Debug)]
pub(crate) struct SessionLogSpec {
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) mode: crate::config::SessionLogMode,
}

/// Session-log file for one terminal tab.
pub(crate) struct SessionLogger {
    writer: Option<BufWriter<File>>,
    path: PathBuf,
    lines: TranscriptLines,
}

fn timestamp(t: DateTime<Local>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// `<name>_<YYYYMMDD-HHMMSS>`: the automatic log's file stem, also offered as
/// the default name when a log is started from the tab menu (#535).
pub(crate) fn default_log_stem(session_name: &str, now: DateTime<Local>) -> String {
    format!(
        "{}_{}",
        sanitize_file_stem(session_name),
        now.format("%Y%m%d-%H%M%S")
    )
}

/// Replace characters that are invalid in file names on any platform.
pub(crate) fn sanitize_file_stem(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c| c == '.' || c == ' ').to_string();
    if cleaned.is_empty() {
        "session".to_string()
    } else {
        cleaned.chars().take(80).collect()
    }
}

impl SessionLogger {
    /// Create `<dir>/<name>_<YYYYMMDD-HHMMSS>.log` (with a numeric suffix if
    /// that name is taken) and write the header line.
    pub(crate) fn create(dir: &Path, session_name: &str, target: &str) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let now = Local::now();
        let stem = default_log_stem(session_name, now);
        let mut attempt = 0u32;
        let (file, path) = loop {
            let name = if attempt == 0 {
                format!("{stem}.log")
            } else {
                format!("{stem}-{attempt}.log")
            };
            let path = dir.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => break (file, path),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists && attempt < 100 => {
                    attempt += 1;
                }
                Err(err) => return Err(err),
            }
        };
        Self::start(file, path, session_name, target, now)
    }

    /// Record into a file the user picked (#535). The save dialog has already
    /// asked before replacing an existing file, so this truncates it.
    pub(crate) fn create_at(path: &Path, session_name: &str, target: &str) -> io::Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        Self::start(file, path.to_path_buf(), session_name, target, Local::now())
    }

    fn start(
        file: File,
        path: PathBuf,
        session_name: &str,
        target: &str,
        now: DateTime<Local>,
    ) -> io::Result<Self> {
        let mut writer = BufWriter::new(file);
        writeln!(
            writer,
            "=== meatshell session log: {} ({}) — started {} ===",
            session_name.trim(),
            target,
            timestamp(now)
        )?;
        writer.flush()?;
        Ok(Self {
            writer: Some(writer),
            path,
            lines: TranscriptLines::default(),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Append a marker line (connect / disconnect / reconnect).
    pub(crate) fn note(&mut self, text: &str) {
        let now = Local::now();
        let writer = &mut self.writer;
        self.lines.flush_partial(now, &mut |t, line| {
            write_line(writer, t, line);
        });
        if let Some(w) = writer.as_mut() {
            if writeln!(w, "=== {} — {} ===", text, timestamp(now)).is_err() {
                *writer = None;
            }
        }
        self.flush();
    }

    /// Seed a log started mid-session with the text currently on screen, so
    /// the transcript has the context the user was looking at. The last row
    /// (usually the prompt, up to the cursor) stays open and continues with the next output.
    pub(crate) fn write_screen_snapshot(&mut self, screen_text: &str) {
        // Drop blank rows below the text but keep the last row's trailing
        // spaces: they separate the prompt from the command typed next.
        let text = screen_text.trim_end_matches(['\n', '\r']);
        if text.trim().is_empty() {
            return;
        }
        self.note("logging started mid-session; current screen follows");
        self.write_output(text.replace('\n', "\r\n").as_bytes());
    }

    /// Record a chunk of terminal output.
    pub(crate) fn write_output(&mut self, bytes: &[u8]) {
        if self.writer.is_none() || bytes.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(bytes);
        let writer = &mut self.writer;
        self.lines.feed(&text, Local::now(), &mut |t, line| {
            write_line(writer, t, line);
        });
        self.flush();
    }

    fn flush(&mut self) {
        if let Some(w) = self.writer.as_mut() {
            if let Err(err) = w.flush() {
                tracing::warn!("session log {}: write failed: {err}", self.path.display());
                self.writer = None;
            }
        }
    }
}

fn write_line(writer: &mut Option<BufWriter<File>>, t: DateTime<Local>, line: &str) {
    if let Some(w) = writer.as_mut() {
        if writeln!(w, "[{}] {}", timestamp(t), line).is_err() {
            *writer = None;
        }
    }
}

impl Drop for SessionLogger {
    fn drop(&mut self) {
        if self.writer.is_some() {
            self.note("session log closed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(chunks: &[&str]) -> Vec<String> {
        let mut t = TranscriptLines::default();
        let mut out = Vec::new();
        let now = Local::now();
        for chunk in chunks {
            t.feed(chunk, now, &mut |_, l| out.push(l.to_string()));
        }
        t.flush_partial(now, &mut |_, l| out.push(l.to_string()));
        out
    }

    #[test]
    fn strips_sgr_and_osc() {
        let out = lines_of(&["\x1b]0;user@host: ~\x07\x1b[01;32muser@host\x1b[00m:~$ ls\r\n"]);
        assert_eq!(out, vec!["user@host:~$ ls"]);
    }

    #[test]
    fn osc_terminated_by_st() {
        let out = lines_of(&["\x1b]7;file://h/tmp\x1b\\ok\r\n"]);
        assert_eq!(out, vec!["ok"]);
    }

    #[test]
    fn crlf_split_across_chunks() {
        let out = lines_of(&["first\r", "\nsecond\r\n"]);
        assert_eq!(out, vec!["first", "second"]);
    }

    #[test]
    fn bare_cr_overwrites_line() {
        let out = lines_of(&["progress 10%\rprogress 50%\rprogress 100%\r\n"]);
        assert_eq!(out, vec!["progress 100%"]);
    }

    #[test]
    fn backspace_edits_apply() {
        // readline echo of "lss", Backspace, Enter
        let out = lines_of(&["$ lss\x08 \x08\x08\x1b[K", "s\r\n"]);
        assert_eq!(out, vec!["$ ls"]);
    }

    #[test]
    fn charset_designators_and_escape_split_across_chunks() {
        let out = lines_of(&["a\x1b", "(0b\x1b[3", "1mc\r\n"]);
        assert_eq!(out, vec!["abc"]);
    }

    #[test]
    fn partial_prompt_flushed_at_end() {
        let out = lines_of(&["done\r\n$ "]);
        assert_eq!(out, vec!["done", "$"]);
    }

    #[test]
    fn keeps_unicode_and_tabs() {
        let out = lines_of(&["名称\t大小\r\n"]);
        assert_eq!(out, vec!["名称\t大小"]);
    }

    #[test]
    fn unterminated_line_is_capped() {
        let long = "x".repeat(MAX_LINE_CHARS + 10);
        let out = lines_of(&[&long]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), MAX_LINE_CHARS);
    }

    #[test]
    fn sanitizes_file_names() {
        assert_eq!(sanitize_file_stem("prod: web/01"), "prod_ web_01");
        assert_eq!(sanitize_file_stem("  "), "session");
        assert_eq!(sanitize_file_stem("..hidden."), "hidden");
    }

    #[test]
    fn screen_snapshot_keeps_prompt_open() {
        let dir = std::env::temp_dir().join(format!("meatshell-log-test-{}", uuid::Uuid::new_v4()));
        let path = {
            let mut log = SessionLogger::create(&dir, "s", "local").unwrap();
            log.write_screen_snapshot("line one\nuser@host:~$ \n\n\n");
            log.write_output(b"ls\r\n");
            log.path().to_path_buf()
        };
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[1].starts_with("=== logging started mid-session"));
        assert!(lines[2].ends_with("] line one"));
        assert!(lines[3].ends_with("] user@host:~$ ls"));
        assert!(lines[4].starts_with("=== session log closed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logger_writes_header_lines_and_footer() {
        let dir = std::env::temp_dir().join(format!("meatshell-log-test-{}", uuid::Uuid::new_v4()));
        let path = {
            let mut log = SessionLogger::create(&dir, "web 01", "ssh root@10.0.0.1:22").unwrap();
            log.write_output(b"\x1b[32mhello\x1b[0m\r\nworld");
            log.path().to_path_buf()
        };
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("=== meatshell session log: web 01 (ssh root@10.0.0.1:22)"));
        assert!(lines[1].ends_with("] hello"));
        assert!(lines[2].ends_with("] world"));
        assert!(lines[3].starts_with("=== session log closed"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
