use super::*;

fn hist_line(s: &str) -> Line {
    (s.to_string(), Vec::new(), false)
}

fn wrapped_hist_line(s: &str) -> Line {
    (s.to_string(), Vec::new(), true)
}

fn make_buf(
    rows: u16,
    cols: u16,
    history: &[&str],
    live_lines: &[&str],
    view_offset: usize,
) -> TermBuffer {
    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(live_lines.join("\r\n").as_bytes());
    TermBuffer {
        parser,
        find_query: String::new(),
        is_dark: false,
        output_highlight: OutputHighlightPreset::Log,
        custom_highlight_rules: Vec::new(),
        json_format_output: false,
        vt100_drawing: true,
        charset: crate::terminal::CharsetTracker::default(),
        interactive_echo_until: std::time::Instant::now(),
        sel_anchor: None,
        sel_focus: None,
        sel_ranges: Vec::new(),
        mouse_tracked: false,
        history: history.iter().map(|s| hist_line(s)).collect(),
        prev: Vec::new(),
        view_offset,
        scroll_accum: 0.0,
        displayed_text: Vec::new(),
        csi_state: CsiState::Normal,
        csi_pending: Vec::new(),
        raw: std::collections::VecDeque::new(),
    }
}

#[test]
fn settings_modal_yields_macos_wheel_to_its_own_scroll_view() {
    assert!(macos_terminal_wheel_can_target_terminal(false));
    assert!(!macos_terminal_wheel_can_target_terminal(true));
}

#[test]
fn releasing_scrollback_drops_retained_history_and_replay_bytes() {
    let mut buffer = make_buf(5, 20, &["old-1", "old-2"], &["live"], 2);
    buffer.prev = vec![hist_line("previous")];
    buffer.raw.extend([1, 2, 3, 4]);
    buffer.displayed_text.push("visible".to_string());
    buffer.sel_anchor = Some((0, 0));
    buffer.sel_focus = Some((1, 1));
    buffer.sel_ranges.push(((0, 0), (1, 1)));

    buffer.release_scrollback();

    assert!(buffer.history.is_empty());
    assert_eq!(buffer.history.capacity(), 0);
    assert!(buffer.prev.is_empty());
    assert_eq!(buffer.prev.capacity(), 0);
    assert!(buffer.raw.is_empty());
    assert_eq!(buffer.raw.capacity(), 0);
    assert!(buffer.displayed_text.is_empty());
    assert!(buffer.sel_anchor.is_none());
    assert!(buffer.sel_focus.is_none());
    assert!(buffer.sel_ranges.is_empty());
    assert_eq!(buffer.view_offset, 0);
}

#[test]
fn snap_to_live_returns_the_viewport_to_the_bottom() {
    let mut buffer = make_buf(3, 20, &["old-1", "old-2"], &["live"], 2);
    buffer.scroll_accum = -2.5;
    buffer.render();
    let scrolled = buffer.displayed_text.clone();

    // The snap reports the change so the caller knows a repaint is due: the
    // displayed rows still belong to the scrolled view until render() runs.
    assert!(buffer.snap_to_live());
    assert_eq!(buffer.view_offset, 0);
    assert_eq!(buffer.scroll_accum, 0.0, "banked wheel fraction must not re-scroll");

    buffer.render();
    assert_ne!(buffer.displayed_text, scrolled);
    // Back to exactly what an untouched live view renders.
    let mut live = make_buf(3, 20, &["old-1", "old-2"], &["live"], 0);
    live.render();
    assert_eq!(buffer.displayed_text, live.displayed_text);
}

#[test]
fn snap_to_live_is_a_no_op_at_the_live_bottom() {
    let mut buffer = make_buf(3, 20, &["old-1"], &["live"], 0);
    buffer.scroll_accum = 1.5;
    buffer.render();
    let live = buffer.displayed_text.clone();

    // Nothing moved, so nothing needs repainting (each keystroke takes this
    // path) and a pending wheel fraction is left alone.
    assert!(!buffer.snap_to_live());
    assert_eq!(buffer.scroll_accum, 1.5);

    buffer.render();
    assert_eq!(buffer.displayed_text, live);
}

mod charset;
mod colors;
mod protocol;
mod selection;
mod sftp_sorting;
