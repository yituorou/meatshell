use super::*;

#[test]
fn inverse_default_colours_paint_a_visible_background() {
    let (fg, bg) = vt_span_colors(
        vt100::Color::Default,
        vt100::Color::Default,
        false,
        true,
        true,
    );
    assert_eq!(fg.as_argb_encoded(), 0xff0e0f13);
    assert_eq!(bg.as_argb_encoded(), 0xffd4d4d4);

    let mut parser = vt100::Parser::new(3, 30, 0);
    parser.process(b"abc \x1b[7m20260705\x1b[27m end");
    let (_plain, runs, _wrapped) = build_row(parser.screen(), 0, 30);
    let hit = runs
        .iter()
        .find(|span| span.text.contains("20260705"))
        .expect("reverse-video search hit should be a separate span");
    assert!(hit.inverse);
    assert!(matches!(hit.fg, vt100::Color::Default));
    assert!(matches!(hit.bg, vt100::Color::Default));
}

#[test]
fn underlined_blanks_are_kept_as_visible_spans() {
    // #444: `printf '\033[4m \033[0m'` must still draw an underline, and
    // input fields drawn as underlined spaces must survive row building.
    let mut parser = vt100::Parser::new(3, 30, 0);
    parser.process(b"\x1b[4m \x1b[0m\r\nName: \x1b[4m          \x1b[24m!");

    let (_plain, runs, _wrapped) = build_row(parser.screen(), 0, 30);
    let blank = runs
        .iter()
        .find(|span| span.underline)
        .expect("a lone underlined space should produce a span");
    assert_eq!((blank.col, blank.cells), (0, 1));
    assert_eq!(blank.text, " ");

    let (_plain, runs, _wrapped) = build_row(parser.screen(), 1, 30);
    let field = runs
        .iter()
        .find(|span| span.underline)
        .expect("an underlined input field should produce a span");
    assert_eq!((field.col, field.cells), (6, 10));
    assert!(field.text.chars().all(|c| c == ' '));
    // SGR 24 ends the underline: the following text is a separate span.
    let bang = runs
        .iter()
        .find(|span| span.text.contains('!'))
        .expect("text after the field should render");
    assert!(!bang.underline);

    let rendered = crate::terminal::render_term_span(field, 1, true);
    assert!(rendered.iter().all(|span| span.underline));
}

#[test]
fn plain_default_blanks_still_produce_no_span() {
    let mut parser = vt100::Parser::new(3, 30, 0);
    parser.process(b"a    b");
    let (_plain, runs, _wrapped) = build_row(parser.screen(), 0, 30);
    assert!(runs.iter().all(|span| !span.underline));
    assert!(runs.iter().all(|span| !span.text.trim().is_empty()));
}
