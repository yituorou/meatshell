use super::*;

#[test]
fn esc_paren_zero_draws_box_lines_even_in_utf8_mode() {
    let mut buffer = make_buf(4, 40, &[], &[], 0);
    let _ = buffer.ingest(b"\x1b(0lqqk\x1b(B");
    buffer.render();

    assert_eq!(buffer.displayed_text[0], "┌──┐");
    // The reflow replay ring stores the already-translated UTF-8, so a resize
    // re-renders the same glyphs without charset state.
    assert_eq!(
        buffer.raw.iter().copied().collect::<Vec<_>>(),
        "\x1b(0┌──┐\x1b(B".as_bytes()
    );
}

#[test]
fn so_si_shifts_output_to_g1_and_back() {
    let mut buffer = make_buf(4, 40, &[], &[], 0);
    let _ = buffer.ingest(b"\x1b)0\x0eq\x0f q");
    buffer.render();

    assert_eq!(buffer.displayed_text[0], "─ q");
}

#[test]
fn charset_designator_survives_split_output_chunks() {
    let mut buffer = make_buf(4, 40, &[], &[], 0);
    let _ = buffer.ingest(b"\x1b");
    let _ = buffer.ingest(b"(0");
    let _ = buffer.ingest(b"lqk");
    buffer.render();

    assert_eq!(buffer.displayed_text[0], "┌─┐");
}

#[test]
fn esc_paren_b_restores_plain_ascii() {
    let mut buffer = make_buf(4, 40, &[], &[], 0);
    let _ = buffer.ingest(b"\x1b(0q\x1b(Bq");
    buffer.render();

    assert_eq!(buffer.displayed_text[0], "─q");
}

#[test]
fn disabled_vt100_drawing_passes_letters_through() {
    let mut buffer = make_buf(4, 40, &[], &[], 0);
    buffer.vt100_drawing = false;
    let _ = buffer.ingest(b"\x1b(0lqqk\x1b(B");
    buffer.render();

    assert_eq!(buffer.displayed_text[0], "lqqk");
}

#[test]
fn issue_543_box_and_degree_sign_render_with_session_defaults() {
    // The reporter's repro, plus nvtop's `35°C` (0x66 = degree sign), with
    // the line-drawing flag taken from a default session.
    let mut buffer = make_buf(4, 40, &[], &[], 0);
    buffer.vt100_drawing = crate::config::Session::new_empty().vt100_drawing;
    let _ = buffer.ingest(b"\x1b(0lqqqqqqk\r\nmqqqqqqj\x1b(B\r\n35\x1b(0f\x1b(BC");
    buffer.render();

    assert_eq!(buffer.displayed_text[0], "┌──────┐");
    assert_eq!(buffer.displayed_text[1], "└──────┘");
    assert_eq!(buffer.displayed_text[2], "35°C");
}

#[test]
fn reset_restores_ascii_after_a_tui_dies_in_line_drawing_mode() {
    let mut buffer = make_buf(4, 40, &[], &[], 0);
    let _ = buffer.ingest(b"\x1b(0\x1b)0\x0e");
    let _ = buffer.ingest(b"\x1bcls -l");
    buffer.render();

    assert_eq!(buffer.displayed_text[0], "ls -l");
}

#[test]
fn line_drawing_defaults_on_even_for_configs_saved_while_it_was_opt_in() {
    use crate::config::Session;
    let mut json = serde_json::to_value(Session::new_empty()).unwrap();
    let obj = json.as_object_mut().unwrap();
    assert_eq!(obj.get("vt100_line_drawing"), Some(&serde_json::Value::Bool(true)));

    // A pre-#543 config carries the old key, usually as the old default.
    obj.remove("vt100_line_drawing");
    obj.insert("vt100_drawing".into(), serde_json::Value::Bool(false));
    let old: Session = serde_json::from_value(json.clone()).unwrap();
    assert!(old.vt100_drawing);

    // Turning it off now sticks.
    let obj = json.as_object_mut().unwrap();
    obj.insert("vt100_line_drawing".into(), serde_json::Value::Bool(false));
    let off: Session = serde_json::from_value(json).unwrap();
    assert!(!off.vt100_drawing);
}

#[test]
fn osc_payload_is_never_charset_translated() {
    let mut buffer = make_buf(4, 40, &[], &[], 0);
    // Window-title OSC emitted while DEC Special Graphics is active: its
    // payload must reach the parser verbatim; only the trailing q maps.
    let _ = buffer.ingest(b"\x1b(0\x1b]2;top - user\x07q");
    buffer.render();

    assert_eq!(buffer.displayed_text[0], "─");
}

#[test]
fn resize_reflow_replay_keeps_translated_box_lines() {
    let mut buffer = make_buf(4, 40, &[], &[], 0);
    let _ = buffer.ingest(b"\x1b(0lqqk\x1b(B\r\nsecond line");
    buffer.reflow(4, 20);
    buffer.render();

    assert_eq!(buffer.displayed_text[0], "┌──┐");
    assert_eq!(buffer.displayed_text[1], "second line");
}
