//! Password prompts on the terminal's own line → pick a saved credential.
//!
//! `sudo`, `ssh`, `git` and `passwd` all print their prompt and then stop
//! echoing, so the only thing on screen is the label followed by a colon. That
//! makes the prompt readable from the grid: take the cursor row, and accept it
//! as a password prompt only when the line *ends* with a colon and the text in
//! front of it names a password.
//!
//! Recognition is deliberately narrow. A popup over a log line is worse than no
//! popup at all, and the popup offers real secrets, so anything ambiguous (a
//! line the user is still typing, a `--password:` column header, a prose
//! sentence) is rejected. Clicking a row is the only way to fill, so a stray
//! false positive can never type a password on its own.

use std::cell::RefCell;

/// Longer than this and it is a sentence, not a prompt label.
const MAX_PROMPT_CHARS: usize = 96;

/// Words that name a password. Matched against the lowercased label, so
/// `PASSWORD`, `Password` and `password` all count.
const NAMES: [&str; 5] = ["password", "passphrase", "passwd", "密码", "口令"];

/// The prompt text on the cursor row, or `None`.
///
/// Read with the cursor row rather than the last non-blank row: a shell that
/// repaints its prompt in place (`sudo` re-prompting after a wrong password)
/// keeps the cursor on it, and a prompt scrolled up into the log is not live.
pub(super) fn prompt_text(screen: &vt100::Screen) -> Option<String> {
    if screen.alternate_screen() {
        // vim/htop own the screen; their status lines are not prompts.
        return None;
    }
    let line = cursor_line(screen);
    let line = line.trim_end();
    if line.is_empty() || line.chars().count() > MAX_PROMPT_CHARS {
        return None;
    }
    looks_like_password_prompt(line).then(|| line.to_string())
}

/// The cursor row as text, one char per cell.
fn cursor_line(screen: &vt100::Screen) -> String {
    let (row, _) = screen.cursor_position();
    let cols = screen.size().1.get();
    let mut line = String::with_capacity(cols as usize);
    for column in 0..cols {
        let Some(cell) = screen.cell(row, column) else {
            continue;
        };
        // A wide glyph owns its first cell; the continuation cell is empty.
        if cell.is_wide_continuation() {
            continue;
        }
        if cell.contents().is_empty() {
            line.push(' ');
        } else {
            line.push_str(cell.contents());
        }
    }
    line
}

/// Does this single line read as a password prompt label?
fn looks_like_password_prompt(line: &str) -> bool {
    let trimmed = line.trim_end();
    // Both an ASCII colon and the full-width one Chinese locales print.
    let Some(label) = trimmed
        .strip_suffix(':')
        .or_else(|| trimmed.strip_suffix('：'))
    else {
        return false;
    };
    let lowered = label.to_lowercase();
    if !NAMES.iter().any(|name| lowered.contains(name)) {
        return false;
    }
    // `user@host:~$ Password:` is the user typing a command, not a prompt. The
    // completion detector already knows where a shell prompt ends, so reuse it
    // instead of keeping a second list of prompt markers.
    if super::prompt_hint::typed_after_prompt(line).is_some_and(|typed| !typed.is_empty()) {
        return false;
    }
    // A prompt is a label, not a sentence: `Reset your password: …` and
    // `Warning: the password: change it` carry too much prose to be one.
    if label.split_whitespace().count() > 8 {
        return false;
    }
    true
}

thread_local! {
    /// Last answer, keyed by (tab, cursor line).
    ///
    /// `prompt_for_cached` runs in the per-frame render flush: during a log
    /// flood that is hundreds of calls a second, and rebuilding the row is
    /// O(cols) while the decision never changes until the row does.
    static CACHE: RefCell<Option<(String, String, Option<String>)>> = const { RefCell::new(None) };

    /// The prompt a picker was dismissed for, per tab, and until when.
    ///
    /// Esc dismisses for good while that prompt stays on screen. Filling a
    /// credential only hides it for a moment: if the password was rejected,
    /// `sudo` re-prompts with the *same* text and the next credential should be
    /// one click away instead of never coming back.
    static DISMISSED: RefCell<Option<(String, String, Option<std::time::Instant>)>> =
        const { RefCell::new(None) };
}

fn dismissed(tab: &str, text: &str) -> bool {
    DISMISSED.with(|slot| {
        let slot = slot.borrow();
        let Some((tab_id, prompt, until)) = slot.as_ref() else {
            return false;
        };
        if tab_id != tab || prompt != text {
            return false;
        }
        match until {
            None => true,
            Some(deadline) => std::time::Instant::now() < *deadline,
        }
    })
}

/// Hide the picker for the prompt currently on screen.
///
/// `hold_for` = `None` keeps it shut until the prompt changes (Esc);
/// `Some(d)` reopens it after `d` (a fill that may have been rejected).
pub(super) fn dismiss(tab: &str, text: &str, hold_for: Option<std::time::Duration>) {
    let until = hold_for.map(|d| std::time::Instant::now() + d);
    DISMISSED.with(|slot| *slot.borrow_mut() = Some((tab.to_string(), text.to_string(), until)));
}

/// `dismiss` for the live screen (the caller knows the tab, not the text).
pub(super) fn dismiss_current(tab: &str, screen: &vt100::Screen, hold_for: Option<std::time::Duration>) {
    if let Some(text) = prompt_text(screen) {
        dismiss(tab, &text, hold_for);
    }
}

/// The prompt behind a one-entry memo, for the per-frame render path.
///
/// Returns the prompt text; `None` means no picker should be drawn.
pub(super) fn prompt_for_cached(tab: &str, screen: &vt100::Screen) -> Option<String> {
    let key = if screen.alternate_screen() {
        String::new()
    } else {
        cursor_line(screen)
    };
    let found = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((tab_id, cached_key, prompt)) = cache.as_ref() {
            if tab_id == tab && *cached_key == key {
                return prompt.clone();
            }
        }
        let prompt = prompt_text(screen);
        *cache = Some((tab.to_string(), key, prompt.clone()));
        prompt
    });
    found.filter(|text| !dismissed(tab, text))
}

#[cfg(test)]
#[path = "../../tests/app/auth_prompt/mod.rs"]
mod auth_prompt_tests;
