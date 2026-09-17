#[test]
fn terminal_tab_close_button_participates_in_content_width() {
    let source = include_str!("../ui/tabs.slint");
    let layout_start = source.find("HorizontalLayout {").unwrap();
    let close_start = source.find("glyph: \"×\";").unwrap();
    let close_end = source[close_start..]
        .find("clicked => { root.closed(); }")
        .unwrap()
        + close_start;

    assert!(close_start > layout_start);
    assert!(source[layout_start..close_start].contains("title-text := Text"));
    assert!(!source[close_start..close_end].contains("x: parent.width - self.width - 6px;"));
}

#[test]
fn tab_slot_uses_its_content_preferred_width() {
    let source = include_str!("../ui/tabs.slint");
    let slot_start = source.find("for tab[i] in tabs : Rectangle {").unwrap();
    let tab_start = source[slot_start..].find("SingleTab {").unwrap() + slot_start;
    let slot = &source[slot_start..tab_start];

    assert!(slot.contains("width: tab-view.preferred-width;"));
}

#[test]
fn terminal_tab_close_button_remains_vertically_centered() {
    let source = include_str!("../ui/tabs.slint");
    let close_start = source.find("glyph: \"×\";").unwrap();
    let close_end = source[close_start..]
        .find("clicked => { root.closed(); }")
        .unwrap()
        + close_start;

    assert!(source[close_start..close_end].contains("y: (parent.height - self.height) / 2;"));
}
