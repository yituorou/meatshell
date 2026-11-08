use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use crate::terminal::charset::CharsetTracker;
use crate::ui::TermSpan;

#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CtrlKeySide {
    Left,
    Right,
}

/// Per-terminal state used by normal and alternate-screen rendering.
pub(crate) struct TermBuffer {
    pub(crate) parser: vt100::Parser,
    pub(crate) find_query: String,
    /// The find match Enter/▲/▼ is on, as (combined-buffer row, char index)
    /// (#535). Combined rows keep their index as output scrolls into history.
    pub(crate) find_cursor: Option<(usize, usize)>,
    pub(crate) is_dark: bool,
    pub(crate) output_highlight: OutputHighlightPreset,
    pub(crate) custom_highlight_rules: Vec<CompiledOutputRule>,
    pub(crate) json_format_output: bool,
    /// Honor VT100 line-drawing (DEC Special Graphics) via SCS designators and
    /// SO/SI even when the session encoding is UTF-8 (#376, PuTTY's option).
    pub(crate) vt100_drawing: bool,
    pub(crate) charset: CharsetTracker,
    pub(crate) interactive_echo_until: std::time::Instant,
    pub(crate) sel_anchor: Option<(usize, u16)>,
    pub(crate) sel_focus: Option<(usize, u16)>,
    pub(crate) sel_ranges: Vec<((usize, u16), (usize, u16))>,
    /// Whether the remote application is tracking the mouse (vt100
    /// `mouse_protocol_mode() != None`). When true, clicks and drags are
    /// forwarded to the PTY instead of starting a local drag-selection, so
    /// btop/htop/mc can be operated with the mouse (#terminal-mouse).
    pub(crate) mouse_tracked: bool,
    pub(crate) history: VecDeque<HistLine>,
    pub(crate) prev: Vec<Line>,
    pub(crate) view_offset: usize,
    /// Fractional scrollback rows not yet applied. Wheel deltas arrive as
    /// pixel fractions of a row (trackpad + macOS momentum decay); keeping
    /// the remainder here lets the momentum tail glide to a stop instead of
    /// stepping a fixed amount per event.
    pub(crate) scroll_accum: f32,
    pub(crate) displayed_text: Vec<String>,
    pub(crate) csi_state: CsiState,
    pub(crate) csi_pending: Vec<u8>,
    pub(crate) raw: VecDeque<u8>,
    /// Plain-text transcript of this tab's output, when session logging is
    /// on for it (#265). Dropping the buffer (tab close) closes the file.
    pub(crate) session_log: Option<crate::terminal::SessionLogger>,
    /// How to open that log; `None` for buffers that can never be logged.
    pub(crate) session_log_spec: Option<crate::terminal::SessionLogSpec>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum CsiState {
    Normal,
    Esc,
    Csi,
    /// Buffering an SCS designator (`ESC ( X`, `ESC ) X`, …); payload holds the
    /// designator intro byte. Bytes still pass through to the display verbatim.
    Designate(u8),
    /// Buffering an OSC string (`ESC ] … BEL/ST`) so its payload is never
    /// charset-translated. Bytes still pass through to the display verbatim.
    Osc,
    OscEsc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputHighlightPreset {
    Off,
    Log,
    DevOps,
}

#[derive(Clone)]
pub(crate) struct CompiledOutputRule {
    pub(crate) matcher: regex::Regex,
    pub(crate) whole_line: bool,
    pub(crate) ansi_index: u8,
}

pub(crate) type TermBufferHandle = Arc<Mutex<TermBuffer>>;
pub(crate) type TermBuffers = Arc<Mutex<HashMap<String, TermBufferHandle>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenderWaitResult {
    Settled,
    Closed,
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RenderGatePhase {
    Idle,
    Scheduled,
    Flushing,
}

pub(super) struct RenderGateState {
    pub(super) requested: u64,
    pub(super) settled: u64,
    pub(super) phase: RenderGatePhase,
    pub(super) closed: bool,
    pub(super) last_visible_flush: std::time::Instant,
}

/// Coalesces and acknowledges UI snapshot flushes for one terminal tab.
pub(crate) struct TabRenderGate {
    pub(super) state: Mutex<RenderGateState>,
    pub(super) settled_cv: Condvar,
}

pub(crate) type RenderGates = Arc<Mutex<HashMap<String, Arc<TabRenderGate>>>>;

/// A coloured, cursor-annotated snapshot ready for the Slint terminal grid.
pub(crate) struct BuiltScreen {
    pub(crate) spans: Vec<TermSpan>,
    pub(crate) cursor_row: i32,
    pub(crate) cursor_col: i32,
    pub(crate) rows_used: i32,
    pub(crate) is_alt: bool,
    pub(crate) mouse_tracked: bool,
    pub(crate) scroll_max: i32,
    pub(crate) scroll_offset: i32,
}

/// One coloured run within a terminal line.
#[derive(Clone)]
pub(crate) struct HistSpan {
    pub(crate) text: String,
    pub(crate) fg: vt100::Color,
    pub(crate) bg: vt100::Color,
    pub(crate) bold: bool,
    pub(crate) inverse: bool,
    /// SGR 4. Kept per run so underlined blanks (input-field placeholders,
    /// login-form lines) still draw their underline (#444).
    pub(crate) underline: bool,
    pub(crate) col: i32,
    pub(crate) cells: i32,
}

pub(crate) type Line = (String, Vec<HistSpan>, bool);

/// One styled run of a scrollback line: a byte range of [`HistLine::text`]
/// plus its grid position and style. Fixed-size and allocation-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HistRun {
    pub(crate) start: u32,
    pub(crate) len: u32,
    pub(crate) col: u16,
    pub(crate) cells: u16,
    pub(crate) fg: vt100::Color,
    pub(crate) bg: vt100::Color,
    pub(crate) bold: bool,
    pub(crate) inverse: bool,
    pub(crate) underline: bool,
}

/// Compact scrollback line (up to `MAX_HISTORY` of these per tab).
///
/// A live [`Line`] stores its plain text and then every run's text again as a
/// separate `String` — about seven allocations per line, and the plain text
/// padded with spaces to the full terminal width. Every run's text is a
/// contiguous slice of the plain text, so history keeps the text once (minus
/// trailing padding) and small fixed-size run records: two allocations and
/// well under half the memory for a typical log line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistLine {
    text: Box<str>,
    /// Length of the plain line within `text`. Any bytes after it hold run
    /// text that could not be found in the plain line (never expected).
    plain_len: u32,
    runs: Box<[HistRun]>,
    pub(crate) wrapped: bool,
}

impl HistLine {
    /// The line's visible text, without trailing padding spaces.
    pub(crate) fn plain(&self) -> &str {
        &self.text[..self.plain_len as usize]
    }

    /// Expand back into renderable runs.
    pub(crate) fn spans(&self) -> Vec<HistSpan> {
        self.runs
            .iter()
            .map(|run| HistSpan {
                text: self.text[run.start as usize..(run.start + run.len) as usize].to_string(),
                fg: run.fg,
                bg: run.bg,
                bold: run.bold,
                inverse: run.inverse,
                underline: run.underline,
                col: i32::from(run.col),
                cells: i32::from(run.cells),
            })
            .collect()
    }
}

impl From<&Line> for HistLine {
    fn from((plain, spans, wrapped): &Line) -> Self {
        // Keep plain text up to its last non-space, or further if a styled
        // run (e.g. a coloured-background blank) extends past that.
        let mut keep = plain.trim_end_matches(' ').len();
        let mut extra = String::new();
        let mut cursor = 0;
        let mut runs = Vec::with_capacity(spans.len());
        for span in spans {
            // Runs are built left to right from the same cells as the plain
            // text, so each is found at or after the previous one. Even if an
            // identical earlier substring were matched, its text is the same.
            let start = match plain.get(cursor..).and_then(|rest| rest.find(&span.text)) {
                Some(offset) => {
                    let start = cursor + offset;
                    cursor = start + span.text.len();
                    keep = keep.max(cursor);
                    start
                }
                None => {
                    let start = plain.len() + extra.len();
                    extra.push_str(&span.text);
                    start
                }
            };
            runs.push(HistRun {
                start: start as u32,
                len: span.text.len() as u32,
                col: span.col.clamp(0, i32::from(u16::MAX)) as u16,
                cells: span.cells.clamp(0, i32::from(u16::MAX)) as u16,
                fg: span.fg,
                bg: span.bg,
                bold: span.bold,
                inverse: span.inverse,
                underline: span.underline,
            });
        }
        let mut text = String::with_capacity(keep + extra.len());
        text.push_str(&plain[..keep]);
        if !extra.is_empty() {
            // Unmatched runs were addressed past the full plain line; shift
            // them onto the trimmed text.
            let shift = (plain.len() - keep) as u32;
            for run in runs
                .iter_mut()
                .filter(|run| run.start as usize >= plain.len())
            {
                run.start -= shift;
            }
            text.push_str(&extra);
        }
        HistLine {
            text: text.into_boxed_str(),
            plain_len: keep as u32,
            runs: runs.into_boxed_slice(),
            wrapped: *wrapped,
        }
    }
}

impl From<Line> for HistLine {
    fn from(line: Line) -> Self {
        HistLine::from(&line)
    }
}
