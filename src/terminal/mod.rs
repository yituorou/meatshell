#[path = "struct/state.rs"]
mod state;

#[path = "impls/input.rs"]
mod input;
#[path = "impls/json_output.rs"]
mod json_output;
#[path = "impls/charset.rs"]
mod charset;
#[path = "impls/encoding.rs"]
mod encoding;
#[path = "impls/glyph_fallback.rs"]
mod glyph_fallback;
#[path = "impls/local.rs"]
pub(crate) mod local;
#[path = "impls/output_highlight.rs"]
mod output_highlight;
#[path = "impls/presentation.rs"]
mod presentation;
#[path = "impls/render.rs"]
mod render;
#[path = "impls/render_gate.rs"]
mod render_gate;
#[path = "impls/serial.rs"]
pub(crate) mod serial;
#[path = "impls/telnet.rs"]
pub(crate) mod telnet;
#[path = "impls/session_log.rs"]
mod session_log;
#[path = "impls/term_buffer.rs"]
mod term_buffer;
#[path = "impls/zmodem.rs"]
pub(crate) mod zmodem;

#[cfg(windows)]
pub(crate) use input::c0_letter_key_down;
#[cfg(test)]
pub(crate) use input::build_paste_preview;
#[cfg(test)]
pub(crate) use input::normalize_pasted_newlines;
pub(crate) use input::{
    bare_ctrl_marker_workaround_enabled, clear_pending_paste,
    encode_command_bar_input, encode_mouse_event, encode_pasted_text, is_back_tab,
    is_terminal_interrupt, key_to_pty_bytes, BACK_TAB_BYTES, paste_requires_large_review, should_drop_bare_ctrl_marker,
    store_pending_paste, take_pending_paste, terminal_uses_bracketed_paste, PendingPaste,
};
pub(crate) use charset::CharsetTracker;
pub(crate) use session_log::{SessionLogSpec, SessionLogger};
pub(crate) use encoding::TerminalEncoding;
pub(crate) use glyph_fallback::{
    install_glyph_fallback, Coverage, GlyphFallback, BUNDLED_TERM_FONT,
};
pub(crate) use json_output::format_json_output;
#[cfg(any(target_os = "windows", test))]
pub(crate) use input::windows_process_ctrl_release;
#[cfg(any(target_os = "windows", test))]
pub(crate) use state::CtrlKeySide;
pub(crate) use output_highlight::compile_output_rules;
pub(crate) use presentation::{highlight_plain_output, render_term_span};
#[cfg(test)]
pub(crate) use presentation::{log_level_marker, text_cell_width, vt_span_colors};
pub(crate) use render::{
    build_row, cell_prefix, char_after_cell_end, char_at_cell_start, detect_scroll, MAX_HISTORY,
    RAW_CAP,
};
pub(crate) use state::{
    BuiltScreen, CompiledOutputRule, CsiState, HistSpan, Line, OutputHighlightPreset, RenderGates,
    TabRenderGate, TermBuffer, TermBufferHandle, TermBuffers,
};
