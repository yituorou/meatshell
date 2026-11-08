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
        find_cursor: None,
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
        history: history.iter().map(|s| hist_line(s).into()).collect(),
        prev: Vec::new(),
        view_offset,
        scroll_accum: 0.0,
        displayed_text: Vec::new(),
        csi_state: CsiState::Normal,
        csi_pending: Vec::new(),
        raw: std::collections::VecDeque::new(),
        session_log: None,
        session_log_spec: None,
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
fn scrollback_navigation_keys_only_handle_a_scrolled_normal_screen() {
    let mut buffer = make_buf(
        3,
        20,
        &["old-1", "old-2", "old-3", "old-4", "old-5"],
        &["live"],
        2,
    );

    assert!(handle_scrollback_key(&mut buffer, ScrollbackKey::PageUp));
    assert_eq!(buffer.view_offset, 5);
    assert!(handle_scrollback_key(&mut buffer, ScrollbackKey::PageDown));
    assert_eq!(buffer.view_offset, 2);
    assert!(handle_scrollback_key(&mut buffer, ScrollbackKey::Home));
    assert_eq!(buffer.view_offset, 5);
    assert!(handle_scrollback_key(&mut buffer, ScrollbackKey::End));
    assert_eq!(buffer.view_offset, 0);

    assert!(!handle_scrollback_key(&mut buffer, ScrollbackKey::PageUp));

    buffer.parser.process(b"\x1b[?1049h");
    buffer.view_offset = 1;
    assert!(!handle_scrollback_key(&mut buffer, ScrollbackKey::PageDown));
    assert_eq!(buffer.view_offset, 1);
}

mod charset;
mod colors;
mod protocol;
mod selection;
mod sftp_sorting;

#[test]
fn enabling_session_log_mid_session_seeds_screen_and_keeps_prompt_spacing() {
    let dir = std::env::temp_dir().join(format!("meatshell-log-test-{}", uuid::Uuid::new_v4()));
    let mut buffer = make_buf(5, 40, &[], &[], 0);
    buffer.parser.process(b"line one\r\nuser@host:~$ ");
    buffer.session_log_spec = Some(crate::terminal::SessionLogSpec {
        name: "web".into(),
        target: "ssh root@h:22".into(),
        mode: crate::config::SessionLogMode::Default,
    });

    // Global off: nothing is opened.
    buffer.apply_session_log(false, &dir).unwrap();
    assert!(buffer.session_log.is_none());

    // Turning it on for an open tab starts logging right away.
    buffer.apply_session_log(true, &dir).unwrap();
    let path = buffer.session_log.as_ref().unwrap().path().to_path_buf();
    let _ = buffer.ingest(b"ls\r\n");

    // Turning it off again closes the file.
    buffer.apply_session_log(false, &dir).unwrap();
    assert!(buffer.session_log.is_none());

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.lines().any(|l| l.ends_with("] line one")), "{text}");
    assert!(text.lines().any(|l| l.ends_with("] user@host:~$ ls")), "{text}");
    assert!(text.lines().last().unwrap().starts_with("=== session log closed"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn session_override_off_wins_over_global_on() {
    let dir = std::env::temp_dir().join(format!("meatshell-log-test-{}", uuid::Uuid::new_v4()));
    let mut buffer = make_buf(5, 40, &[], &[], 0);
    buffer.session_log_spec = Some(crate::terminal::SessionLogSpec {
        name: "web".into(),
        target: "local".into(),
        mode: crate::config::SessionLogMode::Off,
    });
    buffer.apply_session_log(true, &dir).unwrap();
    assert!(buffer.session_log.is_none());
    assert!(!dir.exists());
}

/// #490: shrinking the window while an alternate-screen program (nano, vim)
/// runs must not leave the cursor saved by `CSI ? 1049 h` outside the grid,
/// or the first character printed after `CSI ? 1049 l` panics inside vt100.
/// vt100 0.16.2 clamps the saved cursor on resize; keep this when upgrading.
#[test]
fn leaving_alternate_screen_after_shrinking_keeps_cursor_in_bounds() {
    for (start, rows, cols) in [("\x1b[40;1H", 20, 120), ("\x1b[40;100H", 20, 60)] {
        let mut parser = vt100::Parser::new(40, 120, 0);
        parser.process(start.as_bytes());
        parser.process(b"\x1b[?1049h");
        parser.screen_mut().set_size(rows, cols);
        parser.process(b"\x1b[?1049l");
        parser.process("x\u{4e2d}".as_bytes());
        let (row, col) = parser.screen().cursor_position();
        assert!(row < rows && col <= cols, "cursor {row},{col} outside {rows}x{cols}");
    }
}

/// Log-flood throughput probe (not a correctness test). Run with:
/// `cargo test --release --bin meatshell log_flood_bench -- --ignored --nocapture`
#[test]
#[ignore]
fn log_flood_bench() {
    let mut stream = String::new();
    let levels = [
        "\x1b[32mINFO\x1b[0m",
        "\x1b[33mWARN\x1b[0m",
        "\x1b[31mERROR\x1b[0m",
        "\x1b[36mDEBUG\x1b[0m",
    ];
    for i in 0..200_000u32 {
        stream.push_str(&format!(
            "2026-11-03 01:{:02}:{:02}.{:03} {} 12345 --- [nio-8080-exec-{}] \x1b[35mc.e.logflood.FloodService\x1b[0m : #{:09} processed order id={} status=OK latency={}ms user=alice@example.com\r\n",
            (i / 60000) % 60, (i / 1000) % 60, i % 1000, levels[(i % 4) as usize], i % 16, i, i * 7, i % 300
        ));
    }
    let bytes = stream.as_bytes();
    let mut buf = make_buf(50, 200, &[], &[], 0);
    let chunk = 16 * 1024;
    let started = std::time::Instant::now();
    let mut renders = std::time::Duration::ZERO;
    let mut render_count = 0u32;
    let mut span_total = 0usize;
    for (n, piece) in bytes.chunks(chunk).enumerate() {
        buf.ingest(piece);
        if n % 4 == 0 {
            let t = std::time::Instant::now();
            let b = buf.render();
            renders += t.elapsed();
            render_count += 1;
            span_total += b.spans.len();
        }
    }
    let total = started.elapsed();
    let ingest = total - renders;
    let mb = bytes.len() as f64 / 1_048_576.0;
    eprintln!(
        "BENCH bytes={:.1}MB lines=200000 ingest={:?} ({:.1} MB/s, {:.0} lines/s) renders={} avg_render={:?} avg_spans={}",
        mb,
        ingest,
        mb / ingest.as_secs_f64(),
        200_000.0 / ingest.as_secs_f64(),
        render_count,
        renders / render_count.max(1),
        span_total / render_count.max(1) as usize
    );
}

fn run_tuples(runs: &[HistSpan]) -> Vec<(String, i32, i32, bool, bool, bool, String, String)> {
    runs.iter()
        .map(|r| {
            (
                r.text.clone(),
                r.col,
                r.cells,
                r.bold,
                r.inverse,
                r.underline,
                format!("{:?}", r.fg),
                format!("{:?}", r.bg),
            )
        })
        .collect()
}

#[test]
fn compact_history_line_round_trips_styled_runs() {
    let mut parser = vt100::Parser::new(2, 60, 0);
    parser.process(
        "12:00 \x1b[32mINFO\x1b[0m svc \x1b[1;35mFlood\x1b[0m 中文 ok \x1b[44m   \x1b[0m"
            .as_bytes(),
    );
    let line = build_row(parser.screen(), 0, 60);
    let compact = HistLine::from(&line);
    assert_eq!(run_tuples(&compact.spans()), run_tuples(&line.1));
    // Trailing padding is dropped, but the coloured-background blank run at
    // the end (and the plain space before it) keeps its spaces.
    assert!(compact.plain().ends_with("ok    "));
    assert_eq!(compact.plain(), &line.0[..compact.plain().len()]);
    assert!(compact.plain().len() < line.0.len());
}

#[test]
fn compact_history_line_keeps_wrap_flag_and_plain_text() {
    let line: Line = ("hello".to_string() + &" ".repeat(75), Vec::new(), true);
    let compact = HistLine::from(&line);
    assert_eq!(compact.plain(), "hello");
    assert!(compact.wrapped);
    assert!(compact.spans().is_empty());
}

#[test]
fn inline_tui_insert_before_keeps_live_view_at_bottom() {
    // #542: 10-row screen; an inline TUI owns the bottom rows (input box) and
    // inserts transcript lines above it via a scroll region, like ratatui's
    // insert_before (Codex CLI). Following output must show the newest line
    // and the input box; a reader scrolled back must stay anchored.
    for start_offset in [0usize, 2] {
        let mut b = make_buf(10, 30, &[], &[], 0);
        let _ = b.ingest(b"\x1b[8;1H> input box\r\n  status\r\n");
        let _ = b.render();
        b.prev.clear();
        b.view_offset = start_offset;
        for i in 0..20 {
            let line = format!("\x1b[1;7r\x1b[7;1H\x1b[1S\x1b[7;1Hline {i:02}\x1b[r\x1b[8;1H> input box");
            let _ = b.ingest(line.as_bytes());
        }
        let _ = b.render();
        let shown: Vec<String> = b.displayed_text.iter().map(|r| r.trim_end().to_string()).collect();
        assert_eq!(b.history.len(), 19, "scrolled-off transcript lines reach history");
        if start_offset == 0 {
            assert_eq!(b.view_offset, 0, "following output stays at the bottom");
            assert_eq!(shown[6], "line 19", "{shown:?}");
            assert_eq!(shown[7], "> input box", "{shown:?}");
            assert_eq!(shown[8], "  status", "{shown:?}");
        } else {
            assert!(b.view_offset > start_offset, "scrolled-back view is anchored");
            assert!(shown.iter().any(|r| r == "line 00"), "{shown:?}");
            assert!(!shown.iter().any(|r| r == "line 19"), "{shown:?}");
        }
    }
}

// ---- Find navigation and tab-menu session logs (#535) ----------------------

#[test]
fn find_steps_through_scrollback_wrapping_both_ways() {
    // 3-row screen, 5 history rows: matches at combined rows 0, 2 and 5.
    let mut buffer = make_buf(3, 20, &["err 1", "ok", "Err 2", "ok", "ok"], &["ERR 3", "$ "], 0);
    buffer.find_query = "err".into();
    assert_eq!(buffer.find_all("err"), vec![(0, 0), (2, 0), (5, 0)]);

    // A new query settles on the match already on screen.
    assert_eq!(buffer.find_step("err", 0), (3, 3));
    assert_eq!(buffer.view_offset, 0, "no scroll for a visible match");

    // Next wraps to the oldest match and scrolls it into view.
    assert_eq!(buffer.find_step("err", 1), (1, 3));
    assert_eq!(buffer.vis_to_abs(0), 0);
    let rect = buffer.find_current_rect().expect("current match on screen");
    assert_eq!((rect.row, rect.col, rect.len), (0, 0, 3));

    assert_eq!(buffer.find_step("err", 1), (2, 3));
    assert_eq!(buffer.find_step("err", -1), (1, 3));
    // Previous wraps to the newest match, back on the live rows.
    assert_eq!(buffer.find_step("err", -1), (3, 3));
    assert!((buffer.vis_to_abs(0)..buffer.vis_to_abs(2) + 1).contains(&5));
}

#[test]
fn find_without_matches_reports_zero_and_clears_the_cursor() {
    let mut buffer = make_buf(3, 20, &["a"], &["b"], 0);
    buffer.find_cursor = Some((0, 0));
    assert_eq!(buffer.find_step("zzz", 1), (0, 0));
    assert!(buffer.find_cursor.is_none());
    assert!(buffer.find_current_rect().is_none());
}

#[test]
fn find_current_rect_counts_wide_characters_as_two_cells() {
    let mut buffer = make_buf(3, 20, &["中文 err"], &["$ "], 0);
    buffer.find_query = "err".into();
    assert_eq!(buffer.find_step("err", 0), (1, 1));
    let rect = buffer.find_current_rect().unwrap();
    assert_eq!((rect.col, rect.len), (5, 3));
}

#[test]
fn tab_menu_session_log_starts_and_stops_independently_of_the_setting() {
    let dir = std::env::temp_dir().join(format!("ms-tablog-{}", uuid::Uuid::new_v4()));
    let path = dir.join("picked.log");
    let mut buffer = make_buf(3, 20, &[], &["Router> "], 0);
    buffer.session_log_spec = Some(crate::terminal::SessionLogSpec {
        name: "core switch".into(),
        target: "serial COM3 @9600".into(),
        mode: crate::config::SessionLogMode::Default,
    });
    let name = buffer.default_session_log_file_name().unwrap();
    assert!(name.starts_with("core switch_") && name.ends_with(".log"), "{name}");

    buffer.start_session_log_at(&path).unwrap();
    assert_eq!(buffer.session_log_path().as_deref(), Some(path.as_path()));
    // Turning the global setting off must not stop a manual recording.
    buffer.apply_session_log(false, &dir).unwrap();
    assert!(buffer.session_log.is_some());
    let _ = buffer.ingest(b"show version\r\n");

    assert_eq!(buffer.stop_session_log().as_deref(), Some(path.as_path()));
    // Nor may turning it on restart a recording the user stopped.
    buffer.apply_session_log(true, &dir).unwrap();
    assert!(buffer.session_log.is_none());

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("core switch (serial COM3 @9600)"));
    assert!(text.contains("show version"));
    assert!(text.contains("session log closed"));
    let _ = std::fs::remove_dir_all(dir);
}
