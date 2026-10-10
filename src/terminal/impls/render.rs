use unicode_width::UnicodeWidthChar;

use crate::terminal::{HistSpan, Line};

/// How much terminal byte history is retained for resize reflow.
pub(crate) const RAW_CAP: usize = 2 * 1024 * 1024;
/// Per-session rendered scrollback cap.
pub(crate) const MAX_HISTORY: usize = 100_000;

pub(crate) fn cell_prefix(chars: &[char]) -> Vec<usize> {
    let mut prefix = Vec::with_capacity(chars.len() + 1);
    let mut accumulated = 0usize;
    for &character in chars {
        prefix.push(accumulated);
        accumulated += character.width().unwrap_or(0);
    }
    prefix.push(accumulated);
    prefix
}

pub(crate) fn char_at_cell_start(prefix: &[usize], target: usize) -> usize {
    let char_count = prefix.len().saturating_sub(1);
    for index in 0..char_count {
        if prefix[index] <= target && target < prefix[index + 1] {
            return index;
        }
    }
    char_count
}

pub(crate) fn char_after_cell_end(prefix: &[usize], target: usize) -> usize {
    let char_count = prefix.len().saturating_sub(1);
    for index in 0..char_count {
        if prefix[index] > target {
            return index;
        }
    }
    char_count
}

struct CellAttrs {
    contents: String,
    fg: vt100::Color,
    bg: vt100::Color,
    bold: bool,
    wide: bool,
    inverse: bool,
    underline: bool,
}

impl CellAttrs {
    /// Attributes that must match for two cells to share one rendered run.
    fn same_style(&self, other: &CellAttrs) -> bool {
        self.fg == other.fg
            && self.bg == other.bg
            && self.bold == other.bold
            && self.inverse == other.inverse
            && self.underline == other.underline
    }
}

fn cell_attrs(screen: &vt100::Screen, row: u16, column: u16) -> CellAttrs {
    match screen.cell(row, column) {
        Some(cell) => {
            let contents = cell.contents();
            let contents = if cell.is_wide_continuation() {
                String::new()
            } else if contents.is_empty() {
                " ".to_string()
            } else {
                contents.to_string()
            };
            CellAttrs {
                contents,
                fg: cell.fgcolor(),
                bg: cell.bgcolor(),
                bold: cell.bold(),
                wide: cell.is_wide(),
                inverse: cell.inverse(),
                underline: cell.underline(),
            }
        }
        None => CellAttrs {
            contents: " ".to_string(),
            fg: vt100::Color::Default,
            bg: vt100::Color::Default,
            bold: false,
            wide: false,
            inverse: false,
            underline: false,
        },
    }
}

pub(crate) fn build_row(screen: &vt100::Screen, row: u16, columns: u16) -> Line {
    let mut plain = String::with_capacity(columns as usize);
    let mut runs = Vec::new();
    let mut column = 0u16;
    while column < columns {
        let first = cell_attrs(screen, row, column);
        if first.wide {
            plain.push_str(&first.contents);
            runs.push(HistSpan {
                text: first.contents,
                fg: first.fg,
                bg: first.bg,
                bold: first.bold,
                inverse: first.inverse,
                underline: first.underline,
                col: column as i32,
                cells: 2,
            });
            column += 2;
            continue;
        }

        let start_column = column;
        let mut text = first.contents.clone();
        plain.push_str(&first.contents);
        column += 1;
        while column < columns {
            let next = cell_attrs(screen, row, column);
            if next.wide || !next.same_style(&first) {
                break;
            }
            plain.push_str(&next.contents);
            text.push_str(&next.contents);
            column += 1;
        }

        let cells = (column - start_column) as i32;
        // Plain default blanks need no element. An underlined blank is NOT
        // invisible: apps draw input fields as `ESC[4m` + spaces (#444).
        let invisible_default_blank = text.chars().all(|character| character == ' ')
            && matches!(first.bg, vt100::Color::Default)
            && !first.inverse
            && !first.underline;
        if !invisible_default_blank {
            runs.push(HistSpan {
                text,
                fg: first.fg,
                bg: first.bg,
                bold: first.bold,
                inverse: first.inverse,
                underline: first.underline,
                col: start_column as i32,
                cells,
            });
        }
    }
    (plain, runs, screen.row_wrapped(row))
}

pub(crate) fn detect_scroll(previous: &[Line], current: &[Line]) -> usize {
    let mut best_shift = 0usize;
    let mut best_length = 0usize;
    for shift in 0..previous.len() {
        let mut length = 0usize;
        while shift + length < previous.len()
            && length < current.len()
            && previous[shift + length].0 == current[length].0
        {
            length += 1;
        }
        if length > best_length {
            best_length = length;
            best_shift = shift;
        }
    }
    best_shift
}
