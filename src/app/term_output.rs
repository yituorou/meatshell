//! Terminal output ingest, session-log wiring and per-tab render pacing.

use crate::terminal::ScreenDims as _;
use super::*;

/// Max bytes merged into one Output event before starting a fresh chunk (#209).
/// The pump holds the tab's buffer lock for one whole event, and a UI frame
/// (or a scrollbar drag) waits on that lock: 16 KiB keeps each hold around a
/// millisecond even for dense truecolor logs, instead of ~5 ms at 64 KiB.
pub(super) const OUTPUT_MERGE_BYTE_CAP: usize = 16 * 1024;

/// Output parsed between UI-flush checkpoints during sustained traffic.
///
/// The pump waits for a UI flush after each budget, so budget × frame rate is
/// the most output the terminal can show per second while paced. At 64 KiB and
/// 60 Hz that was ~3.9 MB/s: a log flood just above it built a backlog, and
/// past `PACED_LOCAL_BACKLOG_LIMIT` the pump stopped pacing and ingested in a
/// burst, so the view froze and then jumped — a stutter every few lines. At
/// 256 KiB and 120 Hz the paced ceiling is ~31 MB/s; frames still come at the
/// render interval and simply show however much arrived since the last one.
pub(super) const INGEST_FRAME_BUDGET: usize = 256 * 1024;

/// A busy or closing UI must never block a session pump indefinitely.
pub(super) const UI_FLUSH_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(50);

/// Do not deliberately pace a pump while a large unbounded-channel backlog is
/// already present. It catches up first, then paces the tail of the stream.
pub(super) const PACED_LOCAL_BACKLOG_LIMIT: usize = 1024 * 1024;
pub(super) const PACED_QUEUE_EVENT_LIMIT: usize = 256;

/// Max UI renders per second for a tab under sustained output (#209). 120 Hz:
/// building a frame costs well under a millisecond (span model updated in
/// place), and 60 Hz looked steppy on high-refresh displays next to terminals
/// that follow the monitor (FinalShell runs at 120 fps).
pub(super) const RENDER_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(8);
/// Echo produced shortly after a physical keypress should feel immediate. Kept
/// as its own ceiling (now equal to the normal one) so typing stays at least
/// this responsive if the normal rate is ever lowered again.
pub(super) const INTERACTIVE_RENDER_MIN_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(8);
pub(super) const INTERACTIVE_ECHO_WINDOW: std::time::Duration =
    std::time::Duration::from_millis(180);
/// A scrolled-back viewport is content-anchored, so sustained output only
/// needs occasional model refreshes for its scrollbar metadata (#306).
pub(super) const SCROLLED_RENDER_MIN_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);

/// UI-thread lock requests currently waiting for a terminal buffer.
///
/// The OS mutex is not fair: during a log flood a session pump releases a
/// buffer after one chunk and re-locks it for the next before a waiting UI
/// thread wakes, so a scrollbar drag could wait seconds for one frame
/// (measured: 3 drag steps in 4 s, worst wait 3.8 s). Pumps check this before
/// each chunk and step aside, so the UI gets the next turn and the pump the
/// one after it.
static UI_LOCK_WAITERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How long a pump yields to waiting UI requests before taking its turn
/// anyway (it never blocks a pump for longer than one UI critical section
/// plus this).
const PUMP_YIELD_LIMIT: std::time::Duration = std::time::Duration::from_millis(50);

/// Lock a tab's buffer from a session pump (ingest), letting any UI thread
/// that is waiting for a buffer go first.
pub(super) fn with_pump_lock<R>(
    handle: &std::sync::Mutex<TermBuffer>,
    f: impl FnOnce(&mut TermBuffer) -> R,
) -> R {
    use std::sync::atomic::Ordering;
    if UI_LOCK_WAITERS.load(Ordering::Acquire) > 0 {
        let deadline = std::time::Instant::now() + PUMP_YIELD_LIMIT;
        while UI_LOCK_WAITERS.load(Ordering::Acquire) > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
    }
    let mut guard = handle.lock().unwrap();
    f(&mut guard)
}

/// Lock a tab's buffer from the UI thread; pumps hold off until it has it.
pub(super) fn lock_for_ui(
    handle: &std::sync::Mutex<TermBuffer>,
) -> std::sync::MutexGuard<'_, TermBuffer> {
    use std::sync::atomic::Ordering;
    UI_LOCK_WAITERS.fetch_add(1, Ordering::AcqRel);
    let guard = handle.lock();
    UI_LOCK_WAITERS.fetch_sub(1, Ordering::AcqRel);
    guard.unwrap()
}

pub(super) fn term_buf(bufs: &TermBuffers, tab_id: &str) -> Option<TermBufferHandle> {
    bufs.lock().unwrap().get(tab_id).cloned()
}

pub(super) fn tab_render_interval(bufs: &TermBuffers, tab_id: &str) -> std::time::Duration {
    let Some(handle) = term_buf(bufs, tab_id) else {
        return RENDER_MIN_INTERVAL;
    };
    let interval = match handle.try_lock() {
        Ok(buf) if buf.view_offset > 0 => SCROLLED_RENDER_MIN_INTERVAL,
        Ok(buf) if std::time::Instant::now() < buf.interactive_echo_until => {
            INTERACTIVE_RENDER_MIN_INTERVAL
        }
        // A busy ingest lock just means output is streaming in. Falling back
        // to the 10 Hz scrolled-back rate here made every log flood render at
        // ~10 fps; the pump already yields to each UI flush (see
        // `wait_for_ui_flush`), so the normal rate is safe.
        Ok(_) | Err(_) => RENDER_MIN_INTERVAL,
    };
    interval
}

pub(super) fn with_term_buf<R>(
    bufs: &TermBuffers,
    tab_id: &str,
    f: impl FnOnce(&mut TermBuffer) -> R,
) -> Option<R> {
    let h = term_buf(bufs, tab_id)?;
    let mut guard = lock_for_ui(&h);
    Some(f(&mut guard))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScrollbackKey {
    Home,
    End,
    PageUp,
    PageDown,
}

/// Handle a local scrollback-navigation key.
///
/// Only a normal terminal that is already scrolled away from the live bottom
/// owns these keys. Alternate-screen programs (less, vim, tmux, …) and the
/// live terminal must receive them through the existing PTY input path.
pub(super) fn handle_scrollback_key(buf: &mut TermBuffer, key: ScrollbackKey) -> bool {
    if buf.view_offset == 0 || buf.parser.screen().alternate_screen() {
        return false;
    }

    let max_offset = buf.history.len();
    let page_rows = usize::from(buf.parser.screen().dims().0).max(1);
    buf.scroll_accum = 0.0;
    buf.view_offset = match key {
        ScrollbackKey::Home => max_offset,
        ScrollbackKey::End => 0,
        ScrollbackKey::PageUp => buf.view_offset.saturating_add(page_rows).min(max_offset),
        ScrollbackKey::PageDown => buf.view_offset.saturating_sub(page_rows),
    };
    true
}

pub(super) fn ingest_terminal_output(bufs: &TermBuffers, tab_id: &str, chunk: &[u8]) -> Vec<u8> {
    if let Some(h) = term_buf(bufs, tab_id) {
        with_pump_lock(&h, |buf| buf.ingest(chunk))
    } else {
        Vec::new()
    }
}

/// Header details and override for a tab's session log (#265); `None` for
/// session kinds without a terminal.
pub(super) fn session_log_spec(session: &Session) -> Option<crate::terminal::SessionLogSpec> {
    if session.kind == SessionKind::Rdp {
        return None;
    }
    let target = match session.kind {
        SessionKind::Serial => format!("serial {} @{}", session.serial_port, session.baud_rate),
        SessionKind::Local => "local".to_string(),
        kind => {
            let user = if session.user.trim().is_empty() {
                String::new()
            } else {
                format!("{}@", session.user.trim())
            };
            format!(
                "{} {}{}:{}",
                kind.as_str(),
                user,
                session.host,
                session.port
            )
        }
    };
    let name = if session.name.trim().is_empty() {
        session.host.clone()
    } else {
        session.name.clone()
    };
    Some(crate::terminal::SessionLogSpec {
        name,
        target,
        mode: session.session_log,
    })
}

/// Bring one tab's session log in line with the current settings; a failure
/// is printed into that terminal rather than interrupting the session.
pub(super) fn apply_session_log_to_buffer(
    buffer: &mut TermBuffer,
    enabled: bool,
    dir: &std::path::Path,
) {
    if let Err(err) = buffer.apply_session_log(enabled, dir) {
        tracing::warn!(
            "session log: cannot create file in {}: {err}",
            dir.display()
        );
        let notice = format!(
            "\r\n\x1b[33m{} {} ({err})\x1b[0m\r\n",
            t(
                "[会话日志] 无法创建日志文件:",
                "[session log] could not create log file in"
            ),
            dir.display()
        );
        let _ = buffer.ingest(notice.as_bytes());
    }
}

pub(super) fn record_ingested_chunk(
    chunk_len: usize,
    ingested_since_checkpoint: &mut usize,
) -> bool {
    debug_assert!(*ingested_since_checkpoint < INGEST_FRAME_BUDGET);
    if chunk_len == 0 {
        return false;
    }

    let remaining = INGEST_FRAME_BUDGET - *ingested_since_checkpoint;
    if chunk_len < remaining {
        *ingested_since_checkpoint += chunk_len;
        false
    } else {
        *ingested_since_checkpoint = (chunk_len - remaining) % INGEST_FRAME_BUDGET;
        true
    }
}

pub(super) fn event_requires_immediate_ui(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Connected
            | SessionEvent::Closed(_)
            | SessionEvent::HostKeyPrompt { .. }
            | SessionEvent::CredentialPrompt { .. }
            | SessionEvent::MfaPrompt { .. }
    )
}

/// Tab ids currently shown in a pane (`term.id == pane.active-id` in Slint).
pub(super) fn visible_tab_ids(win: &AppWindow) -> HashSet<String> {
    use slint::Model as _;
    let mut out = HashSet::new();
    let panes = win.get_panes();
    if let Some(pm) = panes.as_any().downcast_ref::<VecModel<PaneInfo>>() {
        for i in 0..pm.row_count() {
            if let Some(pane) = pm.row_data(i) {
                out.insert(pane.active_id.to_string());
            }
        }
    }
    out
}

pub(super) struct TabRenderTicket {
    gate: Arc<TabRenderGate>,
    generation: u64,
}

pub(super) fn register_tab_render_request(
    tab_id: &str,
    gates: &RenderGates,
) -> Option<(Arc<TabRenderGate>, TabRenderTicket, bool)> {
    let gate = {
        let map = gates.lock().unwrap();
        map.get(tab_id).cloned()
    }?;
    let (generation, should_schedule) = gate.request()?;
    let ticket = TabRenderTicket {
        gate: gate.clone(),
        generation,
    };
    Some((gate, ticket, should_schedule))
}

pub(super) fn request_tab_render(
    weak: slint::Weak<AppWindow>,
    tab_id: &str,
    bufs: &TermBuffers,
    gates: &RenderGates,
) -> Option<TabRenderTicket> {
    let (gate, ticket, should_schedule) = register_tab_render_request(tab_id, gates)?;
    if !should_schedule {
        return Some(ticket);
    }

    let weak2 = weak.clone();
    let tid = tab_id.to_string();
    let bufs2 = bufs.clone();
    let gate2 = gate.clone();
    // Always bounce through the event loop from pump / worker threads.
    // Never call invoke_from_event_loop from inside a UI callback — that
    // deadlocks Slint (opening a second tab then froze the whole app).
    if slint::invoke_from_event_loop(move || {
        run_coalesced_tab_render(&weak2, &tid, &bufs2, gate2);
    })
    .is_err()
    {
        // The event loop is gone. Wake any pump waiting on this ticket and
        // reject future requests instead of leaving the gate scheduled forever.
        gate.close();
    }
    Some(ticket)
}

/// UI-thread variant for synthetic Output events. It shares the same gate but
/// enters the throttle directly because invoking Slint from its own callback
/// can deadlock.
pub(super) fn request_tab_render_from_ui(
    weak: slint::Weak<AppWindow>,
    tab_id: &str,
    bufs: &TermBuffers,
    gates: &RenderGates,
) {
    let Some((gate, _, should_schedule)) = register_tab_render_request(tab_id, gates) else {
        return;
    };
    if should_schedule {
        run_coalesced_tab_render(&weak, tab_id, bufs, gate);
    }
}

pub(super) fn wait_for_ui_flush(ticket: Option<TabRenderTicket>) {
    if let Some(ticket) = ticket {
        let _ = ticket
            .gate
            .wait_for(ticket.generation, UI_FLUSH_ACK_TIMEOUT);
    }
}

/// UI-thread entry: honour the throttle, then render. Timer must be created
/// here — not on pump threads (#209).
pub(super) fn run_coalesced_tab_render(
    weak: &slint::Weak<AppWindow>,
    tab_id: &str,
    bufs: &TermBuffers,
    gate: Arc<TabRenderGate>,
) {
    let delay = gate.flush_delay(tab_render_interval(bufs, tab_id));

    let weak2 = weak.clone();
    let tid = tab_id.to_string();
    let bufs2 = bufs.clone();

    if delay.is_zero() {
        do_tab_render_flush(&weak2, &tid, &bufs2, gate);
    } else {
        slint::Timer::single_shot(delay, move || {
            do_tab_render_flush(&weak2, &tid, &bufs2, gate);
        });
    }
}

/// UI-thread only: commit the vt100 snapshot to Slint's model, then reschedule
/// if output arrived after this snapshot began. `request_redraw` is asynchronous,
/// so completion acknowledges a model flush rather than GPU presentation.
pub(super) fn do_tab_render_flush(
    weak: &slint::Weak<AppWindow>,
    tab_id: &str,
    bufs: &TermBuffers,
    gate: Arc<TabRenderGate>,
) {
    let Some(through) = gate.begin_flush() else {
        return;
    };

    let visible = if let Some(win) = weak.upgrade() {
        if visible_tab_ids(&win).contains(tab_id) {
            rebuild_tab_display(&win, bufs, tab_id);
            true
        } else {
            false
        }
    } else {
        false
    };

    if gate.finish_flush(through, visible) {
        let weak2 = weak.clone();
        let tid = tab_id.to_string();
        let bufs2 = bufs.clone();
        // Defer the continuation to avoid recursive flushes for hidden tabs,
        // whose last-visible timestamp intentionally does not throttle them.
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            run_coalesced_tab_render(&weak2, &tid, &bufs2, gate);
        });
    }
}
