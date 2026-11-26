//! Command completion on the shell's own prompt line (#583 follow-up).
//!
//! The command bar already completes `$PATH` commands, but that only helps
//! people who type there. Inside the terminal the shell owns the input line, so
//! the candidate word has to be read back off the screen: the cursor sits where
//! the user is typing, so the text between the shell prompt and the cursor *is*
//! the command being typed.
//!
//! The popup is an IDEA-style list drawn over the grid. Accepting a row sends
//! only the missing suffix to the PTY, so the shell receives exactly the text it
//! would have produced from its own completion, and no readline/zle state is
//! disturbed.
//!
//! Recognition is deliberately conservative: a wrong popup is worse than none,
//! so anything that does not look like "prompt, then one bare word, with a blank
//! line tail" is left alone.

use std::cell::RefCell;

use super::completion::{command_names, rank_commands};

/// Rows the popup shows. The grid is the constraint, not the ranking.
pub(super) const HINT_LIMIT: usize = 8;

/// A completion popup anchored at the cursor.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct PromptHint {
    /// Candidates, best first.
    pub items: Vec<String>,
    /// The command word already typed; candidates extend it.
    pub typed: String,
    /// Grid cell of the cursor the popup hangs from.
    pub row: i32,
    pub col: i32,
}

impl PromptHint {
    /// Text to send to the PTY when `items[index]` is accepted.
    ///
    /// Only the missing tail is sent, so the shell's own line editing stays in
    /// charge and the accepted row reads exactly like a manual completion.
    pub(super) fn suffix(&self, index: usize) -> Option<String> {
        let item = self.items.get(index)?;
        item.strip_prefix(self.typed.as_str()).map(str::to_string)
    }
}

/// Prompt terminators that are unambiguous on their own.
///
/// `>` is deliberately absent: `cat file > out` and `2> err` would read as a
/// PowerShell-style prompt and then offer completions for `out`/`err`.
const SAFE_MARKS: [char; 5] = ['❯', '➜', 'λ', '»', '➤'];

/// Terminators that are also legal inside a command, so they only count when the
/// text in front of them still looks like a prompt.
const GUARDED_MARKS: [char; 3] = ['$', '#', '%'];

/// The command word typed after the shell prompt, from a single prompt line.
///
/// `line` must be the text left of the cursor, already stripped of cells the
/// cursor sits past. Returns `None` when no prompt terminator is found — an
/// unknown prompt yields no popup rather than a guess.
pub(super) fn typed_after_prompt(line: &str) -> Option<&str> {
    let mut start = None;
    for (index, mark) in line.char_indices() {
        let after = index + mark.len_utf8();
        let rest = &line[after..];
        // A prompt ends with the mark alone, or with the mark plus a space.
        let terminated = rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\u{a0}');
        if !terminated {
            continue;
        }
        if SAFE_MARKS.contains(&mark) {
            // oh-my-zsh's `➜  dir git:(main) ✗ ` carries the cwd and git state
            // between the mark and the input. Rather than guess where the
            // decoration ends, only accept a bare input tail.
            start = Some(after);
        } else if GUARDED_MARKS.contains(&mark) {
            let prefix = &line[..index];
            if prefix.trim().is_empty() {
                // `# comment` / `$HOME/bin` at column 0 is not a prompt.
                continue;
            }
            if prefix.contains(['>', '<', '|', '&', '\\']) {
                // `echo a > b $ c` style quoting and redirection.
                continue;
            }
            start = Some(after);
        }
    }
    let start = start?;
    let tail = line[start..].trim_start();
    // Give up on decorated prompts (cwd / branch / custom segments): anything
    // with a space in it is not the command word any more.
    if tail.contains(' ') || tail.contains('\u{a0}') || tail.contains('\t') {
        return None;
    }
    Some(tail)
}

/// Rebuild the text of `row` up to `col`, as the screen shows it.
fn line_before_cursor(screen: &vt100::Screen, row: u16, col: u16) -> String {
    let mut line = String::with_capacity(col as usize);
    for column in 0..col {
        let Some(cell) = screen.cell(row, column) else {
            continue;
        };
        // A wide glyph owns its first cell; the continuation cell is empty.
        if cell.is_wide_continuation() {
            continue;
        }
        // vt100 reports an untouched cell as ""; the grid draws it as a space,
        // and prompt detection depends on those separators being real.
        if cell.contents().is_empty() {
            line.push(' ');
        } else {
            line.push_str(cell.contents());
        }
    }
    line
}

/// Everything to the right of the cursor must be blank.
///
/// This rejects mid-line editing (moving the cursor back over typed text) and
/// shells that paint a right-hand prompt, where the popup would be anchored to
/// a cursor that is not at the end of the input.
fn rest_of_line_is_blank(screen: &vt100::Screen, row: u16, col: u16) -> bool {
    let cols = screen.size().1.get();
    (col..cols).all(|column| match screen.cell(row, column) {
        Some(cell) => cell.contents().trim().is_empty(),
        None => true,
    })
}

/// The popup for the live screen, or `None`.
pub(super) fn hint_for(screen: &vt100::Screen) -> Option<PromptHint> {
    hint_with(screen, command_names())
}

/// `hint_for` against an explicit command table, so tests need no `$PATH`.
fn hint_with(screen: &vt100::Screen, commands: &[String]) -> Option<PromptHint> {
    if screen.alternate_screen() {
        return None;
    }
    let (row, col) = screen.cursor_position();
    if col == 0 || !rest_of_line_is_blank(screen, row, col) {
        return None;
    }
    let line = line_before_cursor(screen, row, col);
    let typed = typed_after_prompt(&line)?;
    if typed.is_empty() || typed.len() > 64 {
        return None;
    }
    // Exact matches are not completions; `rank_commands` already drops them.
    let items = rank_commands(commands, typed, HINT_LIMIT);
    if items.is_empty() {
        return None;
    }
    Some(PromptHint {
        items,
        typed: typed.to_string(),
        row: row as i32,
        col: col as i32,
    })
}

thread_local! {
    /// Last answer, keyed by (tab, prompt line).
    ///
    /// `hint_for_cached` is called from the render flush, which runs on every
    /// frame of a log flood; ranking ~20k command names per frame would be
    /// wasteful when the typed word has not changed. Rebuilding the line is
    /// O(cells) and cheap, so the cache only skips the ranking step.
    static CACHE: RefCell<Option<(String, String, Option<PromptHint>)>> = const { RefCell::new(None) };

    /// The word a popup was dismissed for, per tab. Esc must stay dismissed
    /// while the same word is still on screen, otherwise the next repaint would
    /// pop the list straight back up; typing anything new clears it.
    static DISMISSED: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

fn dismissed(tab: &str, typed: &str) -> bool {
    DISMISSED.with(|slot| {
        matches!(slot.borrow().as_ref(), Some((tab_id, word)) if tab_id == tab && word == typed)
    })
}

/// Hide the popup for the word currently on screen, until the user types more.
pub(super) fn dismiss(tab: &str, typed: &str) {
    DISMISSED.with(|slot| *slot.borrow_mut() = Some((tab.to_string(), typed.to_string())));
}

/// `hint_for` behind a one-entry memo, for the per-frame render path.
pub(super) fn hint_for_cached(tab: &str, screen: &vt100::Screen) -> Option<PromptHint> {
    let (row, col) = screen.cursor_position();
    let key = if screen.alternate_screen() || col == 0 {
        String::new()
    } else {
        line_before_cursor(screen, row, col)
    };
    let hit: Option<PromptHint> = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((tab_id, cached_key, hint)) = cache.as_ref() {
            if tab_id == tab && *cached_key == key {
                return hint.clone();
            }
        }
        let hint = hint_for(screen);
        *cache = Some((tab.to_string(), key, hint.clone()));
        hint
    });
    match hit {
        Some(hint) if !dismissed(tab, &hint.typed) => Some(hint),
        _ => None,
    }
}

#[cfg(test)]
#[path = "../../tests/app/prompt_hint/mod.rs"]
mod prompt_hint_tests;
