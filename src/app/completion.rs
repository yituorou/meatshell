//! OneKey-style completion for the command bar (#583).
//!
//! WindTerm's "OneKey" popup mixes two kinds of candidates while you type:
//! commands installed on `$PATH` (`clear`, `cat`, `chgrp`, …) and commands you
//! already ran (the history). This module builds that merged list. It is kept
//! free of Slint types apart from `SharedString` so the ranking rules can be
//! unit-tested without a window.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

/// Shell builtins that never show up as files on `$PATH`. Without these typing
/// `cd` would offer no completion on a machine where `cd` is only a builtin.
const BUILTINS: &[&str] = &[
    "alias", "bg", "bind", "break", "builtin", "cd", "command", "continue", "declare", "dirs",
    "disown", "echo", "eval", "exec", "exit", "export", "false", "fc", "fg", "getopts", "hash",
    "history", "jobs", "kill", "let", "local", "logout", "popd", "printf", "pushd", "pwd", "read",
    "readonly", "return", "set", "shift", "shopt", "source", "suspend", "test", "times", "trap",
    "true", "type", "typeset", "ulimit", "umask", "unalias", "unset", "wait", "which",
];

/// Upper bound on indexed executables. A PATH full of SDK bins can hold tens of
/// thousands of names; the popup only ever shows a handful, and a smaller index
/// keeps the per-keystroke scan cheap.
const MAX_INDEX: usize = 20_000;

/// How many candidates the live popup shows. The dropdown is ~180px tall and
/// scrolls, so a dozen is already more than it can display at once.
pub(super) const SUGGEST_LIMIT: usize = 12;

/// One autocomplete row: the text to insert plus whether it came from `$PATH`
/// (as opposed to the command history).
pub(super) struct Suggestion {
    pub text: String,
    pub command: bool,
}

/// Every executable name reachable through `$PATH`, plus the shell builtins.
///
/// Scanning happens once per process: the result is cached in a `OnceLock`, so
/// the first keystroke that needs it pays the `read_dir` cost and later ones
/// are a binary search away.
pub(super) fn command_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names: BTreeSet<String> = BTreeSet::new();
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                collect_from_dir(&dir, &mut names);
                if names.len() >= MAX_INDEX {
                    break;
                }
            }
        }
        for builtin in BUILTINS {
            names.insert((*builtin).to_string());
        }
        names.into_iter().collect()
    })
}

#[cfg(unix)]
fn collect_from_dir(dir: &Path, out: &mut BTreeSet<String>) {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        // Only regular files, and only ones we could actually run.
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
            continue;
        }
        insert_name(entry.file_name().to_str(), out);
    }
}

#[cfg(windows)]
fn collect_from_dir(dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // `git.exe` should complete as `git`, the way the shell resolves it.
        let stem = match Path::new(name).extension().and_then(|e| e.to_str()) {
            Some(ext) if matches!(ext.to_ascii_lowercase().as_str(), "exe" | "bat" | "cmd" | "com") => {
                Path::new(name).file_stem().and_then(|s| s.to_str())
            }
            _ => Some(name),
        };
        insert_name(stem, out);
    }
}

fn insert_name(name: Option<&str>, out: &mut BTreeSet<String>) {
    let Some(name) = name else { return };
    // Hidden helpers (`.DS_Store`, `/usr/bin/.foo`) are noise, never commands.
    if name.is_empty() || name.starts_with('.') {
        return;
    }
    out.insert(name.to_string());
}

/// `$PATH` commands whose name starts with the word being typed, best first.
///
/// Only the command word is completed — once the input has an argument
/// (`git checkout …`) command names are no longer candidates, the shell would be
/// completing paths instead. `limit == 0` disables the block entirely.
pub(super) fn rank_commands(commands: &[String], query: &str, limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let trimmed = query.trim();
    if trimmed.is_empty() || trimmed.split_whitespace().count() > 1 {
        return Vec::new();
    }
    let needle = trimmed.to_lowercase();
    let mut hits: Vec<&String> = commands
        .iter()
        .filter(|name| {
            let lower = name.to_lowercase();
            // An exact match is not a completion; keep it out of the popup.
            lower.starts_with(&needle) && lower != needle
        })
        .collect();
    // Shortest first: typing `c` should offer `cd`/`cp` before `chromium-browser`.
    hits.sort_by(|a, b| {
        a.chars()
            .count()
            .cmp(&b.chars().count())
            .then_with(|| a.cmp(b))
    });
    hits.truncate(limit);
    hits.into_iter().cloned().collect()
}

/// History rows matching `query`: case-insensitive substring, same rule the
/// Ctrl+R dropdown uses (#101), so both lists agree on what "matches" means.
pub(super) fn matches_history(query: &str, command: &str) -> bool {
    let needle = query.trim().to_lowercase();
    needle.is_empty() || command.to_lowercase().contains(&needle)
}

/// Rows for the live autocomplete popup: history matches followed by `$PATH`
/// commands, deduplicated.
///
/// Ordering is not cosmetic. The dropdown opens *upward* and is drawn top to
/// bottom, so the last row sits right above the command input — and the existing
/// default selection is the last row. Reversing the command block therefore puts
/// the best match closest to what the user is typing, which is exactly where
/// Tab-completion should land.
pub(super) fn suggest(
    history: &[String],
    commands: &[String],
    query: &str,
    limit: usize,
) -> Vec<Suggestion> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut rows: Vec<Suggestion> = history
        .iter()
        .filter(|command| matches_history(trimmed, command))
        .map(|command| Suggestion {
            text: command.clone(),
            command: false,
        })
        .collect();
    // Keep the most recent matches; the list is oldest → newest.
    if rows.len() > limit {
        rows.drain(..rows.len() - limit);
    }
    let mut extra = rank_commands(commands, trimmed, limit);
    // A command that is also in the history would otherwise show up twice.
    extra.retain(|name| !rows.iter().any(|row| &row.text == name));
    rows.extend(extra.into_iter().rev().map(|text| Suggestion {
        text,
        command: true,
    }));
    rows
}

#[cfg(test)]
#[path = "../../tests/app/completion/mod.rs"]
mod completion_tests;
