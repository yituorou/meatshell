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
    pub(crate) history: VecDeque<Line>,
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
