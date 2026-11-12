//! Path completion for the SFTP panel's path bars.
//!
//! The path fields ask Rust for suggestions on every edit (`complete-path` for
//! the remote side, `local-complete-path` for the local pane). Both sides use
//! the same model: split the typed text into the folder to list plus the
//! fragment being completed, cache the folder's folder names in the tab, and
//! filter them into the rows the popup shows. The local side reads the folder on
//! a worker thread; the remote side asks the SFTP worker for a listing that does
//! not navigate the panel (`SftpCommand::PeekDir`).

use super::*;

use std::path::Path;

use crate::sftp::SftpHandle;

/// At most this many folders are offered at once; typing narrows the list.
pub(super) const MAX_SUGGESTIONS: usize = 8;

/// The typed text split into the folder to list and the part being completed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PathEdit {
    /// Path handed to the lister (filesystem path or remote SFTP path).
    pub dir: String,
    /// Literal text before `fragment`, kept verbatim so a completed path reads
    /// exactly like what the user was typing (`~/`, `C:\Users\`, ...).
    pub prefix: String,
    /// Separator appended to a completed folder name.
    pub sep: char,
    /// Text after the last separator: the part matched against folder names.
    pub fragment: String,
}

#[cfg(windows)]
fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

#[cfg(not(windows))]
fn is_sep(c: char) -> bool {
    c == '/'
}

fn default_sep() -> char {
    std::path::MAIN_SEPARATOR
}

/// A bare drive (`C:`) is a root, not a drive-relative folder.
#[cfg(windows)]
fn is_drive(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(not(windows))]
fn is_drive(_text: &str) -> bool {
    false
}

/// Split the local path field's text. `current` is the folder the pane shows,
/// used for relative input and when the field is empty.
pub(super) fn split_local_path(text: &str, current: &str) -> PathEdit {
    let text = text.trim_end();
    let current = current.trim();
    if text.is_empty() {
        let base = if current.is_empty() {
            display_path(&home_dir())
        } else {
            current.to_string()
        };
        let trimmed = base.trim_end_matches(|c| is_sep(c));
        let sep = if trimmed.is_empty() {
            '/'
        } else {
            base.chars()
                .rev()
                .find(|c| is_sep(*c))
                .unwrap_or_else(default_sep)
        };
        // Keep a root's separator (`/`, `C:\`) and add one to an ordinary
        // folder, so the next completion lists the children.
        let dir = if trimmed.is_empty() || is_drive(trimmed) {
            format!("{trimmed}{sep}")
        } else {
            trimmed.to_string()
        };
        return PathEdit {
            prefix: format!("{trimmed}{sep}"),
            dir,
            sep,
            fragment: String::new(),
        };
    }

    // `~` and `~/` complete inside the home folder, keeping the tilde in the
    // text so the field still reads like what the user typed.
    if text == "~" || text == "~/" || text == "~\\" {
        let sep = if text.ends_with('/') {
            '/'
        } else if text.ends_with('\\') {
            '\\'
        } else {
            default_sep()
        };
        return PathEdit {
            dir: display_path(&home_dir()),
            prefix: format!("~{sep}"),
            sep,
            fragment: String::new(),
        };
    }
    if is_drive(text) {
        let sep = default_sep();
        return PathEdit {
            dir: format!("{text}{sep}"),
            prefix: format!("{text}{sep}"),
            sep,
            fragment: String::new(),
        };
    }

    let last = text
        .char_indices()
        .filter(|(_, c)| is_sep(*c))
        .last()
        .map(|(i, _)| i);
    let (prefix, fragment) = match last {
        Some(i) => (&text[..=i], &text[i + 1..]),
        None => ("", text),
    };
    let sep = prefix
        .chars()
        .rev()
        .find(|c| is_sep(*c))
        .unwrap_or_else(default_sep);
    let dir = if prefix.is_empty() {
        if current.is_empty() {
            display_path(&home_dir())
        } else {
            current.to_string()
        }
    } else if let Some(rest) = prefix
        .strip_prefix("~/")
        .or_else(|| prefix.strip_prefix("~\\"))
    {
        display_path(&home_dir().join(rest))
    } else if Path::new(prefix).is_absolute() || is_drive(prefix.trim_end_matches(|c| is_sep(c))) {
        prefix.to_string()
    } else if current.is_empty() {
        prefix.to_string()
    } else {
        display_path(&Path::new(current).join(prefix))
    };
    PathEdit {
        dir,
        prefix: prefix.to_string(),
        sep,
        fragment: fragment.to_string(),
    }
}

/// Split the remote path field's text. Remote paths are POSIX; relative input
/// is resolved against the folder the pane shows so the suggestions (and the
/// completed text) stay absolute. `None` means there is nothing to list (`~` has
/// no SFTP meaning).
pub(super) fn split_remote_path(text: &str, current: &str) -> Option<PathEdit> {
    let text = text.trim_end();
    let current = current.trim();
    if text.is_empty() {
        let base = if current.is_empty() { "/" } else { current };
        let base = base.trim_end_matches('/');
        let prefix = format!("{base}/");
        return Some(PathEdit {
            dir: prefix.clone(),
            prefix,
            sep: '/',
            fragment: String::new(),
        });
    }
    if text.starts_with('~') {
        return None;
    }
    let base = if current.is_empty() { "/" } else { current };
    let base = base.trim_end_matches('/');
    let combined = if text.starts_with('/') {
        text.to_string()
    } else {
        format!("{base}/{text}")
    };
    let last = combined.rfind('/');
    let (prefix, fragment) = match last {
        Some(i) => (&combined[..=i], &combined[i + 1..]),
        None => ("/", &combined[..]),
    };
    Some(PathEdit {
        dir: prefix.to_string(),
        prefix: prefix.to_string(),
        sep: '/',
        fragment: fragment.to_string(),
    })
}

/// Folder names from `names` that continue `fragment`, turned into the rows the
/// popup shows. Hidden folders only appear when the fragment starts with a dot,
/// like shell completion.
pub(super) fn names_to_suggestions(
    names: &[String],
    prefix: &str,
    sep: char,
    fragment: &str,
) -> Vec<SftpPathSuggestion> {
    let needle = fragment.to_lowercase();
    let include_hidden = needle.starts_with('.');
    names
        .iter()
        .filter(|name| include_hidden || !name.starts_with('.'))
        .filter(|name| name.to_lowercase().starts_with(&needle))
        .take(MAX_SUGGESTIONS)
        .map(|name| SftpPathSuggestion {
            name: name.as_str().into(),
            path: format!("{prefix}{name}{sep}").into(),
        })
        .collect()
}

/// Folder names in one local folder, for completion. Runs on a worker thread:
/// a dead network drive must not freeze the window.
pub(super) fn list_local_dirs(dir: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        names.push(name);
        if names.len() >= 2000 {
            break;
        }
    }
    names.sort_by(|a, b| natural_name_cmp(a, b));
    names
}

pub(super) fn string_model(values: &[String]) -> ModelRc<SharedString> {
    let rows: Vec<SharedString> = values.iter().map(|s| s.as_str().into()).collect();
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

pub(super) fn suggestion_model(values: Vec<SftpPathSuggestion>) -> ModelRc<SftpPathSuggestion> {
    ModelRc::from(Rc::new(VecModel::from(values)))
}

pub(super) fn empty_suggestions() -> SftpPathSuggestions {
    SftpPathSuggestions {
        dir: "".into(),
        prefix: "".into(),
        sep: "/".into(),
        fragment: "".into(),
        names: string_model(&[]),
        entries: suggestion_model(Vec::new()),
    }
}

fn string_vec(model: &ModelRc<SharedString>) -> Vec<String> {
    (0..model.row_count())
        .filter_map(|i| model.row_data(i))
        .map(|s| s.to_string())
        .collect()
}

fn sep_of(state: &SftpPathSuggestions) -> char {
    state.sep.chars().next().unwrap_or('/')
}

/// A peek listing arrived: publish the matches for the fragment typed since the
/// request went out. A listing for another folder is stale and dropped.
pub(super) fn apply_remote_peek(row: &mut TerminalState, path: &str, names: &[String]) {
    if row.sftp_suggest.dir.as_str() != path {
        return;
    }
    let mut names = names.to_vec();
    names.sort_by(|a, b| natural_name_cmp(a, b));
    let sep = sep_of(&row.sftp_suggest);
    row.sftp_suggest.entries = suggestion_model(names_to_suggestions(
        &names,
        row.sftp_suggest.prefix.as_str(),
        sep,
        row.sftp_suggest.fragment.as_str(),
    ));
    row.sftp_suggest.names = string_model(&names);
}

/// The local folder listing arrived (worker thread -> UI thread).
pub(super) fn apply_local_peek(row: &mut TerminalState, dir: &str, names: &[String]) {
    if row.sftp_local_suggest.dir.as_str() != dir {
        return;
    }
    let sep = sep_of(&row.sftp_local_suggest);
    row.sftp_local_suggest.entries = suggestion_model(names_to_suggestions(
        names,
        row.sftp_local_suggest.prefix.as_str(),
        sep,
        row.sftp_local_suggest.fragment.as_str(),
    ));
    row.sftp_local_suggest.names = string_model(names);
}

/// Remote path field changed: answer from the cached folder listing, or ask the
/// SFTP worker for a non-navigating listing of the folder being completed.
pub(super) fn request_remote(window: &AppWindow, handle: &SftpHandle, tab_id: &str, text: &str) {
    let terminals = window.get_terminals();
    let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
        return;
    };
    update_terminal_row(model, tab_id, |row| {
        let Some(edit) = split_remote_path(text, row.sftp_path.as_str()) else {
            row.sftp_suggest = empty_suggestions();
            return;
        };
        let same = row.sftp_suggest.dir.as_str() == edit.dir;
        let names = if same {
            string_vec(&row.sftp_suggest.names)
        } else {
            Vec::new()
        };
        if !same {
            row.sftp_suggest.names = string_model(&[]);
            handle.peek_dir(edit.dir.clone());
        }
        row.sftp_suggest.dir = edit.dir.as_str().into();
        row.sftp_suggest.prefix = edit.prefix.as_str().into();
        row.sftp_suggest.sep = edit.sep.to_string().into();
        row.sftp_suggest.fragment = edit.fragment.as_str().into();
        row.sftp_suggest.entries = suggestion_model(names_to_suggestions(
            &names,
            &edit.prefix,
            edit.sep,
            &edit.fragment,
        ));
    });
}

/// Local path field changed: answer from the cached folder listing, or read the
/// folder on a worker thread and publish the matches back on the UI thread.
pub(super) fn request_local(window: &AppWindow, tab_id: &str, text: &str) {
    let terminals = window.get_terminals();
    let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
        return;
    };
    let mut list: Option<String> = None;
    update_terminal_row(model, tab_id, |row| {
        let edit = split_local_path(text, row.sftp_local_path.as_str());
        let same = row.sftp_local_suggest.dir.as_str() == edit.dir;
        let names = if same {
            string_vec(&row.sftp_local_suggest.names)
        } else {
            Vec::new()
        };
        if !same {
            row.sftp_local_suggest.names = string_model(&[]);
            list = Some(edit.dir.clone());
        }
        row.sftp_local_suggest.dir = edit.dir.as_str().into();
        row.sftp_local_suggest.prefix = edit.prefix.as_str().into();
        row.sftp_local_suggest.sep = edit.sep.to_string().into();
        row.sftp_local_suggest.fragment = edit.fragment.as_str().into();
        row.sftp_local_suggest.entries = suggestion_model(names_to_suggestions(
            &names,
            &edit.prefix,
            edit.sep,
            &edit.fragment,
        ));
    });
    let Some(dir) = list else { return };
    let weak = window.as_weak();
    let tab = tab_id.to_string();
    std::thread::spawn(move || {
        let names = list_local_dirs(&dir);
        let _ = weak.upgrade_in_event_loop(move |window| {
            let terminals = window.get_terminals();
            let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            update_terminal_row(model, &tab, |row| apply_local_peek(row, &dir, &names));
        });
    });
}

#[cfg(test)]
#[path = "../../tests/app/sftp_complete/mod.rs"]
mod tests;
