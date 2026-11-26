use super::{rank_commands, suggest};

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn texts(rows: &[super::Suggestion]) -> Vec<String> {
    rows.iter().map(|row| row.text.clone()).collect()
}

#[test]
fn ranks_path_commands_shortest_first() {
    let commands = names(&["chromium-browser", "cd", "cat", "clear", "cp", "ls"]);
    assert_eq!(
        rank_commands(&commands, "c", 8),
        ["cd", "cp", "cat", "clear", "chromium-browser"]
    );
}

#[test]
fn exact_match_is_not_a_completion() {
    let commands = names(&["cd", "cdargs"]);
    assert_eq!(rank_commands(&commands, "cd", 8), ["cdargs"]);
}

#[test]
fn no_command_completion_once_arguments_are_typed() {
    let commands = names(&["git", "gitui"]);
    assert!(rank_commands(&commands, "git checkout ", 8).is_empty());
    assert!(rank_commands(&commands, "   ", 8).is_empty());
    assert!(rank_commands(&commands, "g", 0).is_empty());
}

#[test]
fn history_comes_first_and_best_command_sits_last() {
    let history = names(&["git status", "cargo check"]);
    let commands = names(&["cd", "cat", "clear"]);
    let rows = suggest(&history, &commands, "c", 8);
    // Newest history match first; the command block is reversed so the best
    // match (shortest) ends up on the last row, next to the input box.
    assert_eq!(
        texts(&rows),
        [
            "cargo check",
            "clear",
            "cat",
            "cd",
        ]
    );
    assert_eq!(
        rows.iter().map(|row| row.command).collect::<Vec<_>>(),
        [false, true, true, true]
    );
}

#[test]
fn history_entry_wins_over_the_same_command_name() {
    let history = names(&["clear"]);
    let commands = names(&["clear", "clang"]);
    let rows = suggest(&history, &commands, "cl", 8);
    // `clear` is already in the history; only the extra command is appended.
    assert_eq!(texts(&rows), ["clear", "clang"]);
}

#[test]
fn empty_query_offers_nothing() {
    let history = names(&["ls"]);
    let commands = names(&["ls"]);
    assert!(suggest(&history, &commands, "", 8).is_empty());
    assert!(suggest(&history, &commands, "   ", 8).is_empty());
}

#[test]
fn history_matches_are_capped_to_the_newest_rows() {
    let history = names(&["c1", "c2", "c3", "c4"]);
    let rows = suggest(&history, &[], "c", 2);
    assert_eq!(texts(&rows), ["c3", "c4"]);
}
