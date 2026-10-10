use super::*;

fn plain_run(text: &str, col: i32) -> HistSpan {
    HistSpan {
        text: text.to_string(),
        fg: vt100::Color::Default,
        bg: vt100::Color::Default,
        bold: false,
        inverse: false,
        underline: false,
        col,
        cells: text.chars().count() as i32,
    }
}

fn custom_rule(
    pattern: &str,
    regex: bool,
    case_sensitive: bool,
    whole_line: bool,
    color: &str,
) -> CompiledOutputRule {
    compile_output_rules(&[OutputHighlightRule {
        pattern: pattern.to_string(),
        regex,
        case_sensitive,
        whole_line,
        color: color.to_string(),
        enabled: true,
    }])
    .pop()
    .expect("test rule should compile")
}

#[test]
fn highlights_uppercase_level_and_preserves_columns() {
    let runs = highlight_plain_output(
        vec![plain_run("2026-07-14T10:20:30Z ERROR request failed", 0)],
        OutputHighlightPreset::Log,
        &[],
    );
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[1].text, "ERROR");
    assert_eq!(runs[1].col, 21);
    assert_eq!(runs[1].cells, 5);
    assert!(runs[1].bold);
    assert!(matches!(runs[1].fg, vt100::Color::Idx(9)));
    assert_eq!(runs[2].col, 26);
}

#[test]
fn highlights_structured_lowercase_level_only() {
    let json = r#"{"level":"warn","message":"disk nearly full"}"#;
    let runs = highlight_plain_output(vec![plain_run(json, 4)], OutputHighlightPreset::Log, &[]);
    let level = runs
        .iter()
        .find(|run| run.text == "warn")
        .expect("structured level should be highlighted");
    assert!(matches!(level.fg, vt100::Color::Idx(11)));

    assert!(log_level_marker("an error occurred", 96).is_none());
    assert!(log_level_marker("ERROR_CODE=5", 96).is_none());
}

#[test]
fn preserves_existing_ansi_styles() {
    let mut coloured = plain_run("ERROR", 0);
    coloured.fg = vt100::Color::Idx(2);
    let runs = highlight_plain_output(vec![coloured], OutputHighlightPreset::Log, &[]);
    assert_eq!(runs.len(), 1);
    assert!(matches!(runs[0].fg, vt100::Color::Idx(2)));
    assert!(!runs[0].bold);
}

#[test]
fn alternate_screen_does_not_add_log_colours() {
    let mut parser = vt100::Parser::new(3, 30, 0);
    parser.process(b"\x1b[?1049hERROR");
    assert!(parser.screen().alternate_screen());
    let (_plain, runs, _wrapped) = build_row(parser.screen(), 0, 30);
    let level = runs
        .iter()
        .find(|run| run.text.contains("ERROR"))
        .expect("alternate-screen text should still render");
    assert!(matches!(level.fg, vt100::Color::Default));
    assert!(!level.bold);
}

#[test]
fn off_preset_leaves_plain_levels_untouched() {
    let runs = highlight_plain_output(
        vec![plain_run("ERROR request failed", 0)],
        OutputHighlightPreset::Off,
        &[],
    );
    assert_eq!(runs.len(), 1);
    assert!(matches!(runs[0].fg, vt100::Color::Default));
    assert!(!runs[0].bold);
}

#[test]
fn devops_preset_adds_deployment_and_structured_states() {
    let success = highlight_plain_output(
        vec![plain_run("deploy SUCCESS", 0)],
        OutputHighlightPreset::DevOps,
        &[],
    );
    let token = success
        .iter()
        .find(|run| run.text == "SUCCESS")
        .expect("DevOps success should be highlighted");
    assert!(matches!(token.fg, vt100::Color::Idx(10)));

    let json = highlight_plain_output(
        vec![plain_run(r#"{"status":"failed"}"#, 0)],
        OutputHighlightPreset::DevOps,
        &[],
    );
    let token = json
        .iter()
        .find(|run| run.text == "failed")
        .expect("structured DevOps state should be highlighted");
    assert!(matches!(token.fg, vt100::Color::Idx(9)));

    let conservative = highlight_plain_output(
        vec![plain_run("deploy SUCCESS", 0)],
        OutputHighlightPreset::Log,
        &[],
    );
    assert_eq!(conservative.len(), 1);
}

#[test]
fn custom_literal_is_case_insensitive_and_overrides_builtin_colour() {
    let rule = custom_rule("error", false, false, false, "green");
    let runs = highlight_plain_output(
        vec![plain_run("ERROR then error", 0)],
        OutputHighlightPreset::Log,
        &[rule],
    );
    let hits: Vec<_> = runs
        .iter()
        .filter(|run| matches!(run.fg, vt100::Color::Idx(10)))
        .collect();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].text, "ERROR");
    assert_eq!(hits[1].text, "error");
    assert!(!runs
        .iter()
        .any(|run| matches!(run.fg, vt100::Color::Idx(9))));
}

#[test]
fn custom_regex_can_highlight_whole_line_without_overwriting_ansi() {
    let rule = custom_rule(r"timeout|denied", true, false, true, "magenta");
    let mut ansi = plain_run(" ANSI", 18);
    ansi.fg = vt100::Color::Idx(2);
    let runs = highlight_plain_output(
        vec![plain_run("request timeout   ", 0), ansi],
        OutputHighlightPreset::Log,
        &[rule],
    );
    assert!(matches!(runs[0].fg, vt100::Color::Idx(13)));
    assert!(runs[0].bold);
    assert!(matches!(runs[1].fg, vt100::Color::Idx(2)));
}

#[test]
fn custom_unicode_match_preserves_terminal_grid_columns() {
    let rule = custom_rule("错误", false, true, false, "red");
    let text = "前缀错误 done";
    let mut run = plain_run(text, 0);
    run.cells = text_cell_width(text);
    let runs = highlight_plain_output(vec![run], OutputHighlightPreset::Log, &[rule]);
    let hit = runs
        .iter()
        .find(|run| run.text == "错误")
        .expect("CJK keyword should be highlighted");
    assert_eq!(hit.col, 4);
    assert_eq!(hit.cells, 4);
}

#[test]
fn invalid_regex_is_rejected_before_persistence() {
    assert!(validate_output_highlight_rule("([", true, false).is_err());
    assert!(validate_output_highlight_rule("literal", false, false).is_ok());
}
