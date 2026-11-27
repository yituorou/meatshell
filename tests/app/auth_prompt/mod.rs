//! Password-prompt recognition for the credential picker.
//!
//! These pin down *when* the picker may appear. A missed prompt costs one
//! click; a false positive puts a list of real secrets over unrelated output,
//! so the negative cases matter as much as the positive ones.

use super::*;

fn parser(text: &str) -> vt100::Parser {
    let mut parser = crate::terminal::vt_parser(6, 80, 0);
    parser.process(text.as_bytes());
    parser
}

#[test]
fn sudo_and_ssh_prompts_are_recognised() {
    for line in [
        "[sudo] password for far: ",
        "Password:",
        "far@box's password:",
        "Enter passphrase for key '/Users/far/.ssh/id_ed25519':",
        "Password for 'https://a-noble@github.com':",
        "Enter password: ",
    ] {
        assert!(
            looks_like_password_prompt(line),
            "should read as a password prompt: {line:?}"
        );
    }
}

#[test]
fn chinese_prompts_with_a_full_width_colon_are_recognised() {
    assert!(looks_like_password_prompt("[sudo] far 的密码："));
    assert!(looks_like_password_prompt("密码："));
    assert!(looks_like_password_prompt("请输入密码:"));
}

#[test]
fn commands_and_output_are_not_prompts() {
    for line in [
        // The user is typing this on the shell prompt.
        "far@box:~$ Password:",
        "❯ passwd:",
        // A colon-separated passwd line.
        "far:x:501:20::/Users/far:/bin/zsh",
        // Errors carry no trailing colon.
        "sudo: 1 incorrect password attempt",
        "Sorry, try again.",
        // Prose, not a label.
        "Reminder: change your password: it expires soon",
        // No password word at all.
        "Are you sure you want to continue connecting (yes/no/[fingerprint])?",
    ] {
        assert!(
            !looks_like_password_prompt(line),
            "should NOT read as a password prompt: {line:?}"
        );
    }
}

#[test]
fn the_cursor_row_is_the_only_row_that_counts() {
    // A prompt printed above the cursor belongs to output that has scrolled on;
    // only the line the cursor sits on is live.
    let screen = parser("Password: \r\ndone\r\n$ ");
    assert_eq!(prompt_text(screen.screen()), None);

    let live = parser("done\r\nPassword: ");
    assert_eq!(prompt_text(live.screen()).as_deref(), Some("Password:"));
}

#[test]
fn alt_screen_never_reports_a_prompt() {
    let mut screen = parser("Password: ");
    screen.process(b"\x1b[?1049h");
    assert!(screen.screen().alternate_screen());
    assert_eq!(prompt_text(screen.screen()), None);
}

#[test]
fn dismissed_prompt_stays_hidden_until_it_changes() {
    let screen = parser("[sudo] password for far: ");
    assert!(prompt_for_cached("tab-dismiss", screen.screen()).is_some());
    dismiss_current("tab-dismiss", screen.screen(), None);
    assert!(
        prompt_for_cached("tab-dismiss", screen.screen()).is_none(),
        "Esc must keep the picker shut for the same prompt"
    );
    // A re-prompt (sudo after a typo) carries different text and reopens it.
    let reprompt = parser("[sudo] password for root: ");
    assert!(prompt_for_cached("tab-dismiss", reprompt.screen()).is_some());
}

#[test]
fn a_filled_prompt_reopens_so_a_rejected_password_can_be_retried() {
    let screen = parser("[sudo] password for far: ");
    assert!(prompt_for_cached("tab-retry", screen.screen()).is_some());
    // What a fill does: hide, but only for a moment.
    dismiss_current("tab-retry", screen.screen(), Some(std::time::Duration::ZERO));
    assert!(
        prompt_for_cached("tab-retry", screen.screen()).is_some(),
        "sudo re-prompts with the same text after a wrong password; the picker must come back"
    );
}
