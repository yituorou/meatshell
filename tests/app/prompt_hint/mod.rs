//! Prompt-line recognition and the completion popup (#583 follow-up).
//!
//! The expensive part (ranking `$PATH` names) is covered in
//! `tests/app/completion`; these cases only pin down *when* a hint is allowed
//! to appear, which is where a wrong guess would be visible to the user.

use super::*;

fn commands() -> Vec<String> {
    ["cat", "cd", "clear", "clears", "chrome"]
        .iter()
        .map(|name| (*name).to_string())
        .collect()
}

fn parser(text: &str) -> vt100::Parser {
    let mut parser = crate::terminal::vt_parser(6, 80, 0);
    parser.process(text.as_bytes());
    parser
}

#[test]
fn bash_prompt_yields_the_typed_command_word() {
    assert_eq!(
        typed_after_prompt("user@host:~$ cle"),
        Some("cle"),
        "the `$ ` prompt ends where the input begins"
    );
    assert_eq!(typed_after_prompt("root@box:/var# apt"), Some("apt"));
    assert_eq!(typed_after_prompt("❯ git"), Some("git"));
    // Column-zero `%` is a vi command line, not a prompt; zsh's real prompt is
    // `host% ` and that form is covered by the guarded-mark rule.
    assert_eq!(typed_after_prompt("% vim"), None);
    assert_eq!(typed_after_prompt("box% vim"), Some("vim"));
}

#[test]
fn prompt_without_input_yields_an_empty_word() {
    // Not `None`: the caller still needs to know a prompt was found, and drops
    // the empty word on its own.
    assert_eq!(typed_after_prompt("user@host:~$ "), Some(""));
}

#[test]
fn column_zero_hash_is_a_shell_comment_not_a_prompt() {
    assert_eq!(typed_after_prompt("# cle"), None);
    assert_eq!(typed_after_prompt("  # cle"), None);
}

#[test]
fn redirection_and_variables_are_not_prompt_terminators() {
    // `>` is never treated as a prompt mark: `ls > out` would otherwise offer a
    // completion for `out`.
    assert_eq!(typed_after_prompt("cat file > out"), None);
    assert_eq!(typed_after_prompt("ls 2> err"), None);
    // `$NAME` has no space after the mark, so it is not a terminator either.
    assert_eq!(typed_after_prompt("echo $HOME"), None);
    // An escaped dollar at the end of a prompt-looking prefix.
    assert_eq!(typed_after_prompt("echo \\$ x"), None);
}

#[test]
fn decorated_prompts_are_left_alone() {
    // oh-my-zsh default theme: cwd + git state sit between the mark and the
    // input, so there is no safe way to tell where the command starts.
    assert_eq!(
        typed_after_prompt("➜  meatshell git:(main) ✗ cle"),
        None,
        "a decorated prompt must not be guessed at"
    );
    // `>` is deliberately not a prompt mark (see the redirection case above),
    // so a bare continuation line offers nothing.
    assert_eq!(typed_after_prompt("> "), None);
}

#[test]
fn plain_output_lines_are_not_hints() {
    assert_eq!(typed_after_prompt("total 24"), None);
    assert_eq!(typed_after_prompt("top - 10:22:33 up 3 days"), None);
    assert_eq!(typed_after_prompt(""), None);
}

#[test]
fn the_popup_lists_every_prefix_match_best_first() {
    let parser = parser("user@host:~$ cle");
    let hint = hint_with(parser.screen(), &commands()).expect("a popup for `cle`");
    assert_eq!(
        hint.items,
        vec!["clear".to_string(), "clears".to_string()],
        "shortest match first, so the popup's first row is the best one"
    );
    assert_eq!(hint.typed, "cle");
    assert_eq!((hint.row, hint.col), (0, 16));
}

#[test]
fn accepting_a_row_types_only_the_missing_suffix() {
    let parser = parser("user@host:~$ cle");
    let hint = hint_with(parser.screen(), &commands()).expect("a popup for `cle`");
    assert_eq!(hint.suffix(0).as_deref(), Some("ar"));
    assert_eq!(hint.suffix(1).as_deref(), Some("ars"));
    assert_eq!(hint.suffix(9), None, "an out-of-range row sends nothing");
}

#[test]
fn exact_command_names_get_no_hint() {
    let parser = parser("user@host:~$ cd");
    assert!(hint_with(parser.screen(), &commands()).is_none());
}

#[test]
fn a_word_with_an_argument_is_left_to_the_shell() {
    let parser = parser("user@host:~$ git checkout");
    assert!(hint_with(parser.screen(), &commands()).is_none());
}

#[test]
fn text_right_of_the_cursor_blocks_the_hint() {
    // Full line typed, then the cursor moved back into it — a suffix drawn at
    // the cursor would overlap the existing glyphs.
    let parser = parser("user@host:~$ clear\x1b[1;14H");
    assert!(hint_with(parser.screen(), &commands()).is_none());
}

#[test]
fn alternate_screen_never_hints() {
    let parser = parser("\x1b[?1049huser@host:~$ cle");
    assert!(hint_with(parser.screen(), &commands()).is_none());
}

#[test]
fn a_line_without_a_known_prompt_never_hints() {
    let parser = parser("cle");
    assert!(hint_with(parser.screen(), &commands()).is_none());
}

#[test]
fn powerlevel10k_style_prompt_is_recognised() {
    // The two-line p10k prompt this project's own users run: a box-drawn header,
    // then `╰─❯ ` with the input after it.
    let parser = parser("╭─[far@MacBook-Air]──[~]──[java:21]\r\n╰─❯ cle");
    let hint = hint_with(parser.screen(), &commands()).expect("a p10k prompt still ends at ❯");
    assert_eq!(hint.items.first().map(String::as_str), Some("clear"));
    assert_eq!(hint.suffix(0).as_deref(), Some("ar"));
    assert_eq!(hint.row, 1, "the hint belongs to the input line, not the header");
}
