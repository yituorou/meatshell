use super::CwdTracker;

const LOGIN_CWD: &str = "\x1b]7;file://host.local/home/asus\x07";

fn connected() -> CwdTracker {
    let mut tracker = CwdTracker::new(true);
    assert_eq!(tracker.feed(LOGIN_CWD), Some("/home/asus".into()));
    tracker
}

fn ready_for_title() -> CwdTracker {
    let mut tracker = connected();
    tracker.input(b"sudo -s\r");
    tracker
}

fn title(path: &str) -> String {
    format!("\x1b]0;root@host: {path}\x07")
}

/// SSH decoding supplies valid UTF-8 chunks. Check every two-chunk boundary,
/// including empty chunks, and then split at every Unicode scalar.
fn assert_every_utf8_split(text: &str, expected: &str) {
    for split in (0..=text.len()).filter(|index| text.is_char_boundary(*index)) {
        let mut tracker = ready_for_title();
        let events: Vec<_> = [tracker.feed(&text[..split]), tracker.feed(&text[split..])]
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(events, vec![expected.to_string()], "split at {split}");
    }
    let mut tracker = ready_for_title();
    let mut events = Vec::new();
    for character in text.chars() {
        let mut bytes = [0; 4];
        if let Some(path) = tracker.feed(character.encode_utf8(&mut bytes)) {
            events.push(path);
        }
    }
    assert_eq!(events, vec![expected.to_string()], "character chunks");
}

#[test]
fn follows_root_title_after_sudo_and_returns_to_login_osc7() {
    let mut tracker = connected();
    tracker.input(b"sudo -s\r");
    assert_eq!(tracker.feed("sudo -s\r\n[sudo] password for asus: "), None);
    tracker.input(b"test-only-password\r");
    assert_eq!(
        tracker.feed("\r\n\x1b]0;root@host: /home/asus\x07root@host:/home/asus# "),
        Some("/home/asus".into())
    );
    for path in ["/media/asus/12TB11/docker", "/etc", "/"] {
        tracker.input(format!("cd {path}\r").as_bytes());
        assert_eq!(
            tracker.feed(&format!("cd {path}\r\n{}root@host:{path}# ", title(path))),
            Some(path.into())
        );
    }
    tracker.input(b"exit\r");
    assert_eq!(
        tracker.feed(&format!("exit\r\n{LOGIN_CWD}asus@host:~$ ")),
        Some("/home/asus".into())
    );
}

#[test]
fn osc0_and_osc2_accept_root_titles_with_or_without_one_separator_space() {
    for selector in [0, 2] {
        for separator in ["", " "] {
            for terminator in ["\x07", "\x1b\\"] {
                let mut tracker = ready_for_title();
                assert_eq!(
                    tracker.feed(&format!(
                        "\x1b]{selector};root@host:{separator}/tmp{terminator}"
                    )),
                    Some("/tmp".into())
                );
            }
        }
    }
}

#[test]
fn title_paths_are_literal_and_keep_unicode_spaces_percent_and_hashes() {
    for path in [
        "/目录/文档 folder",
        "/literal%20directory/%0A/%FF",
        "/a#b/# folder/# ",
        "/trailing space ",
        "/path:with:colons",
        "/",
    ] {
        let mut tracker = ready_for_title();
        assert_eq!(tracker.feed(&title(path)), Some(path.into()));
    }
}

#[test]
fn titles_are_identical_at_every_valid_utf8_split_with_bel_or_st() {
    let path = "/目录/space %20/# folder";
    for selector in [0, 2] {
        for terminator in ["\x07", "\x1b\\"] {
            assert_every_utf8_split(
                &format!("\x1b]{selector};root@host: {path}{terminator}"),
                path,
            );
        }
    }
}

#[test]
fn bracketed_paste_state_and_its_packet_boundaries_do_not_gate_titles() {
    let path = "/媒体/asus/# folder";
    for before in ["", "\x1b[?2004h", "\x1b[?2004l", "\x1b[?2004h\x1b[?2004l"] {
        for after in ["", "\x1b[?2004h", "\x1b[?2004l"] {
            assert_every_utf8_split(
                &format!("{before}{}root@host:{path}# {after}", title(path)),
                path,
            );
        }
    }
}

#[test]
fn terminal_screen_modes_do_not_change_explicit_title_parsing() {
    let mut tracker = ready_for_title();
    assert_eq!(
        tracker.feed(&format!(
            "\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[2J\x1b[H{}",
            title("/reported")
        )),
        Some("/reported".into())
    );
}

#[test]
fn plain_root_prompt_text_never_reports_a_directory() {
    let mut tracker = ready_for_title();
    for text in [
        "root@host:/fake# ",
        "\r\nroot@host:/fake# \r\n",
        "log: root@host:/fake# ",
        "[root@host /fake]# ",
        "\x1b[01;32mroot@host\x1b[0m:\x1b[01;34m/fake\x1b[0m# ",
        "\x1b[?2004hroot@host:/fake# ",
        "\x1b[?2004lroot@host:/fake# ",
    ] {
        assert_eq!(tracker.feed(text), None);
    }
}

#[test]
fn command_echo_and_printed_prompt_text_cannot_impersonate_a_title() {
    let mut tracker = ready_for_title();
    assert_eq!(tracker.feed(&title("/real")), Some("/real".into()));
    tracker.input(b"printf 'root@host:/fake# '\r");
    for text in [
        "printf 'root@host:/fake# '",
        "\r\n\x1b[?2004l",
        "root@host:/fake# ",
        "\r\n\x1b[?2004hroot@host:/real# ",
    ] {
        assert_eq!(tracker.feed(text), None);
    }
    assert_eq!(tracker.feed(&title("/real")), Some("/real".into()));
}

#[test]
fn plain_prompt_text_remains_ignored_at_every_character_boundary() {
    let mut tracker = ready_for_title();
    for character in "root@host:/目录/false# ".chars() {
        let mut bytes = [0; 4];
        assert_eq!(tracker.feed(character.encode_utf8(&mut bytes)), None);
    }
}

#[test]
fn titles_need_a_hostname_observed_in_a_valid_osc7() {
    let mut tracker = CwdTracker::new(true);
    assert_eq!(tracker.feed(&title("/unverified")), None);
    assert_eq!(
        tracker.feed("\x1b]7;file:///home/asus\x07"),
        Some("/home/asus".into())
    );
    tracker.input(b"\r");
    assert_eq!(tracker.feed(&title("/still-unverified")), None);
    assert_eq!(tracker.feed(LOGIN_CWD), Some("/home/asus".into()));
    tracker.input(b"\r");
    assert_eq!(tracker.feed(&title("/verified")), Some("/verified".into()));
}

#[test]
fn hostname_matching_accepts_case_short_names_and_trailing_dns_dots() {
    for hostname in ["host", "HOST", "host.local", "HOST.LOCAL.", "host."] {
        let mut tracker = ready_for_title();
        assert_eq!(
            tracker.feed(&format!("\x1b]0;root@{hostname}: /same-host\x07")),
            Some("/same-host".into())
        );
    }
    let mut tracker = CwdTracker::new(true);
    assert_eq!(
        tracker.feed("\x1b]7;file://host/home/asus\x07"),
        Some("/home/asus".into())
    );
    tracker.input(b"\r");
    assert_eq!(
        tracker.feed("\x1b]2;root@host.local: /fqdn\x07"),
        Some("/fqdn".into())
    );
}

#[test]
fn foreign_or_malformed_title_hosts_are_rejected() {
    for hostname in [
        "other",
        "host.other-domain",
        "host/local",
        "host user",
        "",
        "host@host",
    ] {
        let mut tracker = ready_for_title();
        assert_eq!(
            tracker.feed(&format!("\x1b]0;root@{hostname}: /wrong\x07")),
            None
        );
    }
}

#[test]
fn titles_and_later_foreign_osc7_cannot_rebind_the_initial_hostname() {
    let mut tracker = ready_for_title();
    assert_eq!(tracker.feed("\x1b]0;root@other: /wrong\x07"), None);
    assert_eq!(
        tracker.feed("\x1b]7;file://other.local/explicit\x07"),
        Some("/explicit".into())
    );
    tracker.input(b"\r");
    assert_eq!(tracker.feed("\x1b]2;root@other.local: /wrong\x07"), None);
    assert_eq!(
        tracker.feed(&title("/original-host")),
        Some("/original-host".into())
    );
}

#[test]
fn title_matching_requires_the_exact_root_title_structure() {
    for content in [
        "asus@host: /ordinary-user",
        "admin@host: /not-root",
        "Root@host: /case-sensitive-user",
        "log root@host: /prefixed",
        "[root@host /bracketed]",
        "root@host /missing-colon",
        "root@host:  /two-separator-spaces",
        "root@host:\t/tab-separator",
    ] {
        let mut tracker = ready_for_title();
        assert_eq!(tracker.feed(&format!("\x1b]0;{content}\x07")), None);
    }
}

#[test]
fn icon_and_unrelated_osc_selectors_never_enable_title_fallback() {
    for selector in ["1", "3", "8", "9", "52", "133", "00", "02"] {
        let mut tracker = ready_for_title();
        assert_eq!(
            tracker.feed(&format!("\x1b]{selector};root@host: /not-a-title\x07")),
            None
        );
    }
}

#[test]
fn home_relative_and_abbreviated_title_paths_are_not_guessed() {
    for path in [
        "~",
        "~/docker",
        "relative",
        ".../docker",
        "…/docker",
        "/.../docker",
        "/a/…/b",
    ] {
        let mut tracker = ready_for_title();
        assert_eq!(tracker.feed(&title(path)), None, "path {path}");
    }
}

#[test]
fn literal_controls_in_title_paths_are_rejected() {
    for path in [
        "/newline\nname",
        "/return\rname",
        "/tab\tname",
        "/nul\0name",
        "/del\x7fname",
    ] {
        let mut tracker = ready_for_title();
        assert_eq!(tracker.feed(&title(path)), None);
    }
}

#[test]
fn neither_bel_nor_st_frames_emit_before_the_complete_terminator() {
    for selector in ["0;root@host: ", "2;root@host:", "7;file://host.local"] {
        for terminator in ["\x07", "\x1b\\"] {
            let mut tracker = ready_for_title();
            assert_eq!(tracker.feed(&format!("\x1b]{selector}/complete")), None);
            if terminator == "\x1b\\" {
                assert_eq!(tracker.feed("\x1b"), None);
                assert_eq!(tracker.feed("\\"), Some("/complete".into()));
            } else {
                assert_eq!(tracker.feed(terminator), Some("/complete".into()));
            }
        }
    }
}

#[test]
fn osc7_decodes_percent_encoding_at_every_valid_utf8_split() {
    for terminator in ["\x07", "\x1b\\"] {
        assert_every_utf8_split(
            &format!("\x1b]7;file://host.local/目录/space%20dir/%E4%B8%AD%E6%96%87{terminator}"),
            "/目录/space dir/中文",
        );
    }
}

#[test]
fn latest_complete_osc7_in_a_chunk_wins_over_all_titles() {
    for content in [
        format!(
            "{}\x1b]7;file://host.local/first\x07{}\x1b]7;file://host.local/latest\x1b\\{}",
            title("/before"),
            title("/between"),
            title("/after")
        ),
        format!(
            "\x1b]7;file://host.local/first\x07{}\x1b]7;file://host.local/latest\x07",
            title("/between")
        ),
    ] {
        let mut tracker = ready_for_title();
        assert_eq!(tracker.feed(&content), Some("/latest".into()));
    }
}

#[test]
fn osc7_remains_authoritative_over_titles_in_later_packets() {
    let mut tracker = ready_for_title();
    assert_eq!(
        tracker.feed(&title("/before-osc7")),
        Some("/before-osc7".into())
    );
    assert_eq!(
        tracker.feed("\x1b]7;file://host.local/actual\x07"),
        Some("/actual".into())
    );
    for chunk in [
        "\x1b]0;root@host: /misleading",
        "\x07",
        "\x1b[?2004l",
        "\x1b]2;root@host:/later\x1b",
        "\\",
    ] {
        assert_eq!(tracker.feed(chunk), None);
    }
    tracker.input(b"cd /next\r");
    assert_eq!(tracker.feed(&title("/next")), Some("/next".into()));
}

#[test]
fn submission_or_control_c_resets_osc7_authority_but_typing_does_not() {
    for input in [b"\r".as_slice(), b"\n", b"\x03", b"command\r\n"] {
        let mut tracker = connected();
        tracker.input(input);
        assert_eq!(
            tracker.feed(&title("/new-round")),
            Some("/new-round".into())
        );
    }
    for input in [b"typing".as_slice(), b"\x1b[D", b"\x7f", b"\x04", b""] {
        let mut tracker = connected();
        tracker.input(input);
        assert_eq!(tracker.feed(&title("/same-round")), None);
    }
}

#[test]
fn output_newlines_and_control_c_do_not_reset_osc7_authority() {
    let mut tracker = connected();
    assert_eq!(tracker.feed("\r\n\x03\r\n"), None);
    assert_eq!(tracker.feed(&title("/same-input-round")), None);
}

#[test]
fn invalid_osc7_does_not_bind_a_host_or_suppress_a_valid_title() {
    let mut tracker = CwdTracker::new(true);
    assert_eq!(tracker.feed("\x1b]7;file://foreign/invalid%FF\x07"), None);
    assert_eq!(tracker.feed(LOGIN_CWD), Some("/home/asus".into()));
    tracker.input(b"\r");
    for content in [
        "7;file://host.local/invalid%FF",
        "7;file://host.local/newline%0Aname",
        "7;file://host.local/nul%00name",
        "7;https://host.local/path",
        "7;file://host.local",
    ] {
        assert_eq!(tracker.feed(&format!("\x1b]{content}\x07")), None);
    }
    assert_eq!(tracker.feed(&title("/valid")), Some("/valid".into()));
}

#[test]
fn disabled_title_compatibility_still_accepts_streaming_osc7() {
    let mut tracker = CwdTracker::new(false);
    assert_eq!(tracker.feed(LOGIN_CWD), Some("/home/asus".into()));
    tracker.input(b"sudo -s\r");
    assert_eq!(tracker.feed(&title("/disabled")), None);
    assert_eq!(tracker.feed("\x1b]2;root@host: /disabled\x1b\\"), None);
    assert_eq!(tracker.feed("\x1b]7;file://host.local/explicit\x1b"), None);
    assert_eq!(tracker.feed("\\"), Some("/explicit".into()));
}

#[test]
fn oversized_osc_frames_are_discarded_and_next_frame_recovers() {
    for prefix in ["7;file://host.local/", "0;root@host: /", "2;root@host:/"] {
        for terminator in ["\x07", "\x1b\\"] {
            let mut tracker = ready_for_title();
            assert_eq!(tracker.feed(&format!("\x1b]{prefix}")), None);
            for _ in 0..32 {
                assert_eq!(tracker.feed(&"a".repeat(1024)), None);
            }
            assert_eq!(tracker.feed(terminator), None);
            assert_eq!(
                tracker.feed(&title("/recovered")),
                Some("/recovered".into())
            );
        }
    }
}

#[test]
fn interrupted_osc_frames_recover_without_reporting_the_partial_path() {
    for prefix in [
        "\x1b]7;file://host.local/incomplete",
        "\x1b]0;root@host: /incomplete",
    ] {
        for interruption in ["\x1b[0m", "\x1b", ""] {
            let mut tracker = ready_for_title();
            assert_eq!(tracker.feed(prefix), None);
            assert_eq!(tracker.feed(interruption), None);
            assert_eq!(
                tracker.feed(&title("/recovered")),
                Some("/recovered".into())
            );
        }
    }
}

#[test]
fn oversized_unterminated_osc_can_be_interrupted_by_a_new_osc() {
    let mut tracker = ready_for_title();
    assert_eq!(
        tracker.feed(&format!("\x1b]7;file://host.local/{}", "x".repeat(32_000))),
        None
    );
    assert_eq!(
        tracker.feed(&title("/recovered")),
        Some("/recovered".into())
    );
}

#[test]
fn can_and_sub_cancel_osc_frames_including_a_pending_escape() {
    for cancel in ["\x18", "\x1a"] {
        for pending_escape in ["", "\x1b"] {
            for prefix in ["7;file://host.local", "0;root@host: "] {
                let mut tracker = ready_for_title();
                assert_eq!(
                    tracker.feed(&format!("\x1b]{prefix}/cancelled{pending_escape}")),
                    None
                );
                assert_eq!(tracker.feed(&format!("{cancel}\\\x07")), None);
                assert_eq!(
                    tracker.feed(&title("/recovered")),
                    Some("/recovered".into())
                );
            }
        }
    }
}

#[test]
fn dcs_sos_pm_and_apc_do_not_parse_embedded_osc_frames() {
    for start in ["\x1bP", "\x1bX", "\x1b^", "\x1b_"] {
        let mut tracker = ready_for_title();
        let payload = format!(
            "{start}opaque\x1b]7;file://host.local/fake-osc7\x07{}",
            title("/fake-title")
        );
        for character in payload.chars() {
            let mut bytes = [0; 4];
            assert_eq!(tracker.feed(character.encode_utf8(&mut bytes)), None);
        }
        // BEL belongs to the opaque payload; only ST ends these strings.
        assert_eq!(tracker.feed("\x07\x1b"), None);
        assert_eq!(tracker.feed("\\"), None);
        assert_eq!(tracker.feed(&title("/outside")), Some("/outside".into()));
    }
}

#[test]
fn opaque_string_embedded_osc7_cannot_establish_the_trusted_hostname() {
    for start in ["\x1bP", "\x1bX", "\x1b^", "\x1b_"] {
        let mut tracker = CwdTracker::new(true);
        assert_eq!(tracker.feed(&format!("{start}{LOGIN_CWD}\x1b\\")), None);
        tracker.input(b"\r");
        assert_eq!(tracker.feed(&title("/unverified")), None);
    }
}

#[test]
fn can_and_sub_cancel_opaque_strings_and_allow_following_titles() {
    for start in ["\x1bP", "\x1bX", "\x1b^", "\x1b_"] {
        for cancel in ["\x18", "\x1a"] {
            let mut tracker = ready_for_title();
            assert_eq!(tracker.feed(&format!("{start}opaque{cancel}")), None);
            assert_eq!(
                tracker.feed(&title("/after-cancel")),
                Some("/after-cancel".into())
            );
        }
    }
}

#[test]
fn a_prompt_redrawn_by_ctrl_c_does_not_report_the_same_directory_again() {
    // #577: browsing the SFTP panel, then Ctrl+C in the terminal, must not
    // snap the panel back to the shell's directory.
    let mut tracker = connected();
    let prompt = |path: &str| format!("^C\r\n\x1b]7;file://host.local{path}\x07$ ");
    tracker.input(b"\x03");
    assert_eq!(tracker.feed(&prompt("/home/asus")), None);
    // Repeated cancels stay quiet too.
    tracker.input(b"\x03");
    assert_eq!(tracker.feed(&prompt("/home/asus")), None);

    // A submitted line still re-reports it, so the panel can snap back (#59).
    tracker.input(b"\r");
    assert_eq!(tracker.feed(&prompt("/home/asus")), Some("/home/asus".into()));
    // Ctrl+C then Enter is a submission.
    tracker.input(b"\x03\r");
    assert_eq!(tracker.feed(&prompt("/home/asus")), Some("/home/asus".into()));

    // A different directory after a cancel is still followed.
    tracker.input(b"\x03");
    assert_eq!(tracker.feed(&prompt("/srv")), Some("/srv".into()));
}
