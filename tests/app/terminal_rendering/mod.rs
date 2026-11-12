use crate::terminal::ScreenDims as _;
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
    let mut parser = crate::terminal::vt_parser(rows, cols, 0);
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
        let mut parser = crate::terminal::vt_parser(40, 120, 0);
        parser.process(start.as_bytes());
        parser.process(b"\x1b[?1049h");
        parser.screen_mut().set_dims(rows, cols);
        parser.process(b"\x1b[?1049l");
        parser.process("x\u{4e2d}".as_bytes());
        let (row, col) = parser.screen().cursor_position();
        assert!(row < rows && col <= cols, "cursor {row},{col} outside {rows}x{cols}");
    }
}

/// #544: inside tmux (alternate screen) a resize goes straight to vt100's
/// `set_size`. When a shrink cut a wide CJK glyph in half at the right edge,
/// vt100 0.16.2 kept the orphaned half and the next write there panicked
/// (`screen.rs` unwrap / `row.rs` index out of bounds) — with
/// `panic = "abort"` the app just vanished. Fixed upstream in vt100 0.17+.
#[test]
fn shrinking_through_a_wide_glyph_at_the_right_edge_does_not_panic() {
    let mut parser = crate::terminal::vt_parser(5, 10, 0);
    parser.process(b"\x1b[?1049h");
    parser.process("\x1b[1;9H\u{4e2d}".as_bytes()); // 中 in the last two columns
    parser.screen_mut().set_dims(5, 9); // cut it in half
    parser.process(b"\x1b[1;9Hx\x1b[1;1H\x1b[K");
    parser.screen_mut().set_dims(5, 12);
    parser.process("\x1b[1;11H\u{6587}\x1b[?1049l".as_bytes());
    assert_eq!(parser.screen().dims(), (5, 12));
}

/// #544: vt100 0.18's `set_size` pulls rows back out of its own scrollback
/// when the screen grows. MeatShell already moved scrolled-off rows into
/// `history`, so growing the window inside tmux and leaving it must not put
/// those rows on the live screen a second time.
#[test]
fn growing_inside_alt_screen_does_not_duplicate_history_rows() {
    let mut buf = make_buf(10, 20, &[], &[], 0);
    buf.parser = crate::terminal::vt_parser(10, 20, 5000);
    for i in 0..30 {
        let _ = buf.ingest(format!("line{i}\r\n").as_bytes());
    }
    let _ = buf.ingest(b"\x1b[?1049h");
    buf.parser.screen_mut().set_dims(15, 20);
    let _ = buf.ingest(b"\x1b[?1049l");
    let live = buf.parser.screen().contents();
    for h in &buf.history {
        let text = h.plain().trim_end();
        if text.is_empty() {
            continue;
        }
        assert!(
            !live.lines().any(|l| l.trim_end() == text),
            "{text:?} is both in history and on the live screen"
        );
    }
    // A shrink-then-grow round trip still restores the hidden screen.
    let _ = buf.ingest(b"\x1b[?1049h");
    buf.parser.screen_mut().set_dims(5, 20);
    buf.parser.screen_mut().set_dims(15, 20);
    let _ = buf.ingest(b"\x1b[?1049l");
    assert!(buf.parser.screen().contents().contains("line29"));
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
    let mut parser = crate::terminal::vt_parser(2, 60, 0);
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

/// Replays a captured log flood (`target/flood.typescript.gz`, recorded with
/// `script` from the log-flood heavy profile) through ingest + render + the
/// span-model sync the way the session pump and UI flush do, and prints how
/// the cost develops as history fills. `FLOOD_MB` limits the replay. Run with
/// `cargo test --bin meatshell flood_replay_profile -- --ignored --nocapture`.
#[test]
#[ignore]
fn flood_replay_profile() {
    use slint::Model as _;
    use std::io::Read as _;
    use std::time::{Duration, Instant};

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/flood.typescript.gz");
    let mut data = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(&path).expect("capture"))
        .read_to_end(&mut data)
        .unwrap();
    let limit = std::env::var("FLOOD_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map_or(data.len(), |mb| (mb << 20).min(data.len()));
    let data = &data[..limit];

    let mut buf = make_buf(40, 180, &[], &[], 0);
    buf.is_dark = true;
    buf.output_highlight = OutputHighlightPreset::DevOps;
    buf.json_format_output = true;
    let model = slint::VecModel::<crate::ui::TermSpan>::default();

    const CHUNK: usize = 64 * 1024;
    let total = data.len().div_ceil(CHUNK);
    let every = (total / 16).max(1);
    let (mut ingest, mut ingest_max, mut render) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    let (mut spans, mut spans_max, mut changed, mut changed_max, mut bytes) = (0usize, 0, 0, 0, 0);
    println!(
        "{:>6} {:>8} {:>8} {:>10} {:>9} {:>7} {:>9} {:>7} {:>8}",
        "MiB", "history", "MB/s", "ingestmax", "render", "spans", "spansmax", "changed", "chgmax"
    );
    for (i, chunk) in data.chunks(CHUNK).enumerate() {
        let t = Instant::now();
        let _ = buf.ingest(chunk);
        let d = t.elapsed();
        ingest += d;
        ingest_max = ingest_max.max(d);
        bytes += chunk.len();

        let t = Instant::now();
        let built = buf.render();
        render += t.elapsed();
        let before: Vec<_> = model.iter().collect();
        let n = built.spans.len();
        crate::app::terminal_ui::sync_span_model(&model, built.spans);
        let c = (0..model.row_count())
            .filter(|&r| before.get(r) != model.row_data(r).as_ref())
            .count();
        spans += n;
        spans_max = spans_max.max(n);
        changed += c;
        changed_max = changed_max.max(c);

        if (i + 1) % every == 0 || i + 1 == total {
            let n = ((i % every) + 1) as u32;
            println!(
                "{:>6.1} {:>8} {:>8.1} {:>8.1}ms {:>7.2}ms {:>7} {:>9} {:>7} {:>8}",
                ((i + 1) * CHUNK) as f64 / 1048576.0,
                buf.history.len(),
                bytes as f64 / 1e6 / ingest.as_secs_f64().max(1e-9),
                ingest_max.as_secs_f64() * 1e3,
                (render / n).as_secs_f64() * 1e3,
                spans / n as usize,
                spans_max,
                changed / n as usize,
                changed_max,
            );
            (ingest, ingest_max, render) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
            (spans, spans_max, changed, changed_max, bytes) = (0, 0, 0, 0, 0);
        }
    }
}

/// While a pump thread ingests the captured flood as fast as it can (a
/// backlog), the UI thread does what one scrollbar-drag step does every 8 ms:
/// lock the buffer, move `view_offset`, render. Prints how long each step
/// waited for the lock. Run with
/// `cargo test --release --bin meatshell flood_scrollbar_lock_wait -- --ignored --nocapture`.
#[test]
#[ignore]
fn flood_scrollbar_lock_wait() {
    use std::io::Read as _;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/flood.typescript.gz");
    let mut data = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(&path).expect("capture"))
        .read_to_end(&mut data)
        .unwrap();
    data.truncate(96 << 20);

    let mut buf = make_buf(40, 180, &[], &[], 0);
    buf.is_dark = true;
    buf.output_highlight = OutputHighlightPreset::DevOps;
    buf.json_format_output = true;
    // Start with full history so drags have somewhere to go.
    for chunk in data[..(24 << 20)].chunks(16 * 1024) {
        let _ = buf.ingest(chunk);
    }
    let handle = Arc::new(Mutex::new(buf));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let pump = {
        let handle = handle.clone();
        let stop = stop.clone();
        let rest = data[(24 << 20)..].to_vec();
        std::thread::spawn(move || {
            let mut bytes = 0usize;
            for chunk in rest.chunks(16 * 1024) {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let _ = crate::app::term_output::with_pump_lock(&handle, |b| b.ingest(chunk));
                bytes += chunk.len();
            }
            bytes
        })
    };

    let mut waits = Vec::new();
    let mut renders = Vec::new();
    let started = Instant::now();
    let mut step = 0usize;
    while started.elapsed() < Duration::from_secs(4) {
        let t = Instant::now();
        let mut b = crate::app::term_output::lock_for_ui(&handle);
        waits.push(t.elapsed());
        let max = b.history.len().max(1);
        b.view_offset = 1 + (step * 37) % max;
        let t = Instant::now();
        let _ = b.render();
        renders.push(t.elapsed());
        drop(b);
        step += 1;
        std::thread::sleep(Duration::from_millis(8));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let pumped = pump.join().unwrap();

    waits.sort();
    renders.sort();
    let pct = |v: &[Duration], p: f64| v[((v.len() - 1) as f64 * p) as usize].as_secs_f64() * 1e3;
    println!(
        "drag steps {} | pump ingested {:.1} MiB in 4 s",
        waits.len(),
        pumped as f64 / 1048576.0
    );
    println!(
        "lock wait  p50 {:.2}ms  p90 {:.2}ms  p99 {:.2}ms  max {:.2}ms",
        pct(&waits, 0.5),
        pct(&waits, 0.9),
        pct(&waits, 0.99),
        pct(&waits, 1.0)
    );
    println!(
        "render     p50 {:.2}ms  p99 {:.2}ms  max {:.2}ms",
        pct(&renders, 0.5),
        pct(&renders, 0.99),
        pct(&renders, 1.0)
    );
}

#[test]
fn selection_stays_on_its_text_while_a_full_scrollback_evicts_lines() {
    let history: Vec<String> = (0..crate::terminal::MAX_HISTORY)
        .map(|i| format!("line {i:06}"))
        .collect();
    let refs: Vec<&str> = history.iter().map(String::as_str).collect();
    let mut buf = make_buf(5, 20, &refs, &["live"], 1000);
    // Prime the previous-screen snapshot so later output is captured.
    let _ = buf.ingest(b"\r\nprime\r\n");

    // Select one whole history line while scrolled back.
    let row = buf
        .history
        .iter()
        .position(|line| line.plain().trim_end() == "line 090000")
        .expect("line in history");
    buf.sel_ranges = vec![((row, 0), (row, 10))];
    buf.sel_anchor = Some((row, 0));
    buf.sel_focus = Some((row, 10));
    let before = buf.extract_selection_text();
    assert_eq!(before.trim_end(), "line 090000");
    let top_before = buf.vis_to_abs(0);
    let first_visible_before = buf.history[top_before].plain().to_string();

    // A burst of output evicts lines from the front of the full history.
    for i in 0..60 {
        let _ = buf.ingest(format!("new {i}\r\n").as_bytes());
    }
    assert_eq!(buf.history.len(), crate::terminal::MAX_HISTORY);

    // The view and the selection both still show the same text.
    assert_eq!(buf.history[buf.vis_to_abs(0)].plain(), first_visible_before);
    assert_eq!(buf.extract_selection_text(), before);
}
