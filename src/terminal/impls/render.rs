use unicode_segmentation::UnicodeSegmentation;
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

/// Borrowed view of one cell. `contents` points into the vt100 grid (or a
/// static blank) so building a row allocates per run, not per cell — this runs
/// for every row of every half-screen batch during log floods.
struct CellAttrs<'a> {
    contents: &'a str,
    fg: vt100::Color,
    bg: vt100::Color,
    bold: bool,
    wide: bool,
    inverse: bool,
    underline: bool,
}

impl CellAttrs<'_> {
    /// Attributes that must match for two cells to share one rendered run.
    fn same_style(&self, other: &CellAttrs<'_>) -> bool {
        self.fg == other.fg
            && self.bg == other.bg
            && self.bold == other.bold
            && self.inverse == other.inverse
            && self.underline == other.underline
    }
}

fn cell_attrs(screen: &vt100::Screen, row: u16, column: u16) -> CellAttrs<'_> {
    match screen.cell(row, column) {
        Some(cell) => {
            let contents = cell.contents();
            let contents = if cell.is_wide_continuation() {
                ""
            } else if contents.is_empty() {
                " "
            } else {
                contents
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
            contents: " ",
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
            // vt100 gives every wide code point its own cells, so an emoji
            // cluster such as 👍🏽 (base + skin tone) or 👨‍💻 (ZWJ sequence)
            // lands in several cells. Keep a cluster in one run so it is drawn
            // as one glyph; the extra cells stay reserved as blank columns.
            let start_column = column;
            let mut text = first.contents.to_string();
            column += 2;
            while column < columns {
                let next = cell_attrs(screen, row, column);
                if next.contents.is_empty() || !next.same_style(&first) {
                    break;
                }
                let mut joined = text.clone();
                joined.push_str(next.contents);
                if joined.graphemes(true).nth(1).is_some() {
                    break;
                }
                text = joined;
                column += if next.wide { 2 } else { 1 };
            }
            plain.push_str(&text);
            runs.push(HistSpan {
                text,
                fg: first.fg,
                bg: first.bg,
                bold: first.bold,
                inverse: first.inverse,
                underline: first.underline,
                col: start_column as i32,
                cells: (column - start_column) as i32,
            });
            continue;
        }

        let start_column = column;
        let mut text = first.contents.to_string();
        plain.push_str(first.contents);
        column += 1;
        while column < columns {
            let next = cell_attrs(screen, row, column);
            if next.wide || !next.same_style(&first) {
                break;
            }
            plain.push_str(next.contents);
            text.push_str(next.contents);
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

#[cfg(test)]
mod emoji_cluster_tests {
    fn spans(sample: &str) -> Vec<(i32, i32, String)> {
        let mut parser = vt100::Parser::new(2, 20, 0);
        parser.process(sample.as_bytes());
        let (_, runs, _) = super::build_row(parser.screen(), 0, 20);
        runs.into_iter().map(|r| (r.col, r.cells, r.text)).collect()
    }

    #[test]
    fn skin_tone_and_zwj_clusters_stay_in_one_run() {
        // vt100 puts each wide code point in its own cells; the cluster must
        // still reach the renderer whole, or it draws 👍 + 🏽 separately.
        assert_eq!(spans("A👍🏽B")[1], (1, 4, "👍🏽".to_string()));
        assert_eq!(spans("A👨\u{200d}💻B")[1], (1, 4, "👨\u{200d}💻".to_string()));
        assert_eq!(
            spans("👨\u{200d}👩\u{200d}👧 x")[0],
            (0, 6, "👨\u{200d}👩\u{200d}👧".to_string())
        );
    }

    #[test]
    fn separate_wide_characters_are_not_merged() {
        // Two emoji, CJK, and an emoji followed by a blank stay separate.
        let s = spans("😀😀中文");
        assert_eq!(s[0], (0, 2, "😀".to_string()));
        assert_eq!(s[1], (2, 2, "😀".to_string()));
        assert_eq!(s[2], (4, 2, "中".to_string()));
        assert_eq!(s[3], (6, 2, "文".to_string()));
        assert_eq!(spans("😀 B")[0], (0, 2, "😀".to_string()));
    }

    #[test]
    fn differently_styled_cells_are_not_merged() {
        let s = spans("👍\u{1b}[31m🏽\u{1b}[0m");
        assert_eq!(s[0], (0, 2, "👍".to_string()));
        assert_eq!(s[1], (2, 2, "🏽".to_string()));
    }
}
