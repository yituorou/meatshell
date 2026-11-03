use super::*;

#[test]
fn plain_click_has_no_selection_extent() {
    let mut buffer = make_buf(2, 20, &[], &["one"], 0);
    buffer.sel_anchor = Some((0, 1));
    buffer.sel_focus = Some((0, 1));
    buffer.sel_ranges.push(((0, 1), (0, 1)));
    assert!(!buffer.selection_has_extent());

    buffer.sel_focus = Some((0, 2));
    buffer.sel_ranges[0].1 = (0, 2);
    assert!(buffer.selection_has_extent());
}

#[test]
fn double_click_selects_shell_word_and_keeps_paths_together() {
    let mut buffer = make_buf(2, 80, &[], &["ssh user@host /var/log/app.log"], 0);
    buffer.render();
    let selected = buffer.select_word_at(0, 20).expect("word under cursor");
    assert_eq!(selected, "/var/log/app.log");
    assert_eq!(buffer.extract_selection_text(), "/var/log/app.log");
}

#[test]
fn vis_to_abs_maps_live_and_scrolled_consistently() {
    // history H0..H2 (3 lines), live LIVE0/LIVE1 → combined len 5.
    let live = make_buf(5, 20, &["H0", "H1", "H2"], &["LIVE0", "LIVE1"], 0);
    assert_eq!(live.vis_to_abs(0), 3, "live row 0 is first live line");
    assert_eq!(live.vis_to_abs(1), 4);

    // Scrolled to the very top (offset = history len).
    let top = make_buf(5, 20, &["H0", "H1", "H2"], &["LIVE0", "LIVE1"], 3);
    assert_eq!(top.vis_to_abs(0), 0, "top row 0 is oldest history line");
    assert_eq!(top.vis_to_abs(2), 2);
    assert_eq!(top.vis_to_abs(3), 3, "row 3 crosses into live content");
}

#[test]
fn extract_spans_history_and_live() {
    let mut buf = make_buf(5, 20, &["HIST0", "HIST1", "HIST2"], &["LIVE0", "LIVE1"], 3);
    buf.sel_anchor = Some((0, 0)); // top of history
    buf.sel_focus = Some((4, 19)); // end of last live line
    assert_eq!(
        buf.extract_selection_text(),
        "HIST0\nHIST1\nHIST2\nLIVE0\nLIVE1"
    );
}

#[test]
fn extract_is_view_independent() {
    // The same absolute selection copies identically whether the view is
    // scrolled to the top or sitting at the live bottom — this is the whole
    // point of the fix (a top-to-bottom selection survives auto-scrolling).
    let sel = |off| {
        let mut b = make_buf(
            5,
            20,
            &["HIST0", "HIST1", "HIST2"],
            &["LIVE0", "LIVE1"],
            off,
        );
        b.sel_anchor = Some((0, 0));
        b.sel_focus = Some((4, 19));
        b.extract_selection_text()
    };
    assert_eq!(sel(3), sel(0));
    assert_eq!(sel(3), "HIST0\nHIST1\nHIST2\nLIVE0\nLIVE1");
}

#[test]
fn extract_joins_soft_wrapped_rows() {
    let mut buf = make_buf(5, 10, &[], &["x"], 0);
    buf.history = VecDeque::from([
        wrapped_hist_line("0123456789").into(),
        wrapped_hist_line("abcdefghij").into(),
        hist_line("klmnop").into(),
        hist_line("next").into(),
    ]);
    buf.sel_anchor = Some((0, 0));
    buf.sel_focus = Some((3, 9));
    assert_eq!(
        buf.extract_selection_text(),
        "0123456789abcdefghijklmnop\nnext"
    );
}

#[test]
fn highlight_clipped_to_current_view() {
    // Scrolled to the top: a history selection is on-screen and highlighted.
    let mut top = make_buf(5, 20, &["HIST0", "HIST1", "HIST2"], &["LIVE0", "LIVE1"], 3);
    top.sel_anchor = Some((0, 2));
    top.sel_focus = Some((2, 4));
    let rects = top.selection_rects_visible(20);
    assert_eq!(
        rects.len(),
        3,
        "rows 0,1,2 (the 3 history lines) highlighted"
    );
    assert_eq!(rects[0].row, 0);
    assert_eq!(rects[2].row, 2);

    // At the live bottom the same history selection is scrolled off → none.
    let mut live = make_buf(5, 20, &["HIST0", "HIST1", "HIST2"], &["LIVE0", "LIVE1"], 0);
    live.sel_anchor = Some((0, 2));
    live.sel_focus = Some((2, 4));
    assert!(live.selection_rects_visible(20).is_empty());
}

#[test]
fn extract_handles_wide_cjk_columns() {
    // Regression for #132: copying after CJK glyphs drifted right by the
    // number of wide chars before the selection (e.g. selecting "1pctl"
    // yielded "ctl…"). The history line lays out on the grid as:
    //   提(0-1) 示(2-3) :(4) space(5) 1(6) p(7) c(8) t(9) l(10)
    let mut buf = make_buf(5, 20, &["提示: 1pctl"], &["x"], 0);

    // The "1pctl" run sits at grid cols 6..=10.
    buf.sel_anchor = Some((0, 6));
    buf.sel_focus = Some((0, 10));
    assert_eq!(buf.extract_selection_text(), "1pctl");

    // Selecting from the second CJK glyph through the end.
    buf.sel_anchor = Some((0, 2));
    buf.sel_focus = Some((0, 10));
    assert_eq!(buf.extract_selection_text(), "示: 1pctl");

    // Anchoring on the *second* cell of a wide glyph still grabs the whole
    // glyph — you can't half-select a CJK char.
    buf.sel_anchor = Some((0, 3));
    buf.sel_focus = Some((0, 10));
    assert_eq!(buf.extract_selection_text(), "示: 1pctl");
}

#[test]
fn find_matches_report_grid_columns_past_cjk() {
    // Highlight rects must sit at the GRID column, not the char index, so
    // they line up over the text after CJK glyphs (#132).
    let rows = vec!["提示: 1pctl".to_string()];
    let m = compute_find_matches(&rows, "1pctl");
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].col, 6, "grid column 6, not char index 4");
    assert_eq!(m[0].len, 5);

    // A CJK query spans two grid cells per glyph.
    let m2 = compute_find_matches(&rows, "提示");
    assert_eq!(m2.len(), 1);
    assert_eq!(m2[0].col, 0);
    assert_eq!(m2[0].len, 4, "two wide glyphs span four grid cells");
}
