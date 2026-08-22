use super::*;

pub(super) fn resolve_jump(store: &Rc<RefCell<ConfigStore>>, session: &Session) -> Option<Session> {
    if session.kind != SessionKind::Ssh || session.jump_session_id.trim().is_empty() {
        return None;
    }
    if session.jump_session_id == session.id {
        return None;
    }
    store.borrow().get(&session.jump_session_id).cloned()
}

/// Spawn the shell (+ SFTP) workers and their event-pump threads for an
/// already-registered tab. Used by the initial connect and by in-place
/// reconnect (#79); the tab/terminal/parser must already exist.
pub(super) fn start_session_in_tab(tab_id: &str, session: Session, ctx: &ConnectCtx) {
    let has_sftp = session.kind == SessionKind::Ssh;
    let (initial_cols, initial_rows) = *ctx.last_term_size.lock().unwrap();
    // Resolve the optional SSH jump host now (on the UI thread, where the store
    // lives) so the owned Session can be handed to the worker threads (#211).
    let jump = resolve_jump(&ctx.store, &session);
    let (handle, rx) = match session.kind {
        SessionKind::Ssh => spawn_session(
            ctx.runtime.handle(),
            tab_id.to_string(),
            session.clone(),
            jump.clone(),
            initial_cols,
            initial_rows,
        ),
        SessionKind::Serial => crate::terminal::serial::spawn_serial_session(
            ctx.runtime.handle(),
            tab_id.to_string(),
            session.clone(),
        ),
        SessionKind::Telnet => crate::terminal::telnet::spawn_telnet_session(
            ctx.runtime.handle(),
            tab_id.to_string(),
            session.clone(),
            initial_cols,
            initial_rows,
        ),
        SessionKind::Local => crate::terminal::local::spawn_local_session(
            ctx.runtime.handle(),
            tab_id.to_string(),
            session.clone(),
            initial_cols,
            initial_rows,
        ),
    };
    let terminal_reply_tx = handle.commands.clone();
    let monitoring_enabled = ctx
        .weak
        .upgrade()
        .map(|window| !window.get_sidebar_collapsed() && !window.get_zen_mode())
        .unwrap_or(true);
    handle.set_resource_monitoring(monitoring_enabled);
    ctx.handles.borrow_mut().insert(tab_id.to_string(), handle);

    // Delivery route for this tab. Both pump threads hold the Arc and
    // re-read it on every batch, so a later detach/merge only rewrites the
    // route — the pumps keep running and target the new window (#tab-detach).
    let route = Arc::new(Mutex::new(TabRoute {
        window: ctx.weak.clone(),
        window_id: ctx.window_id,
        bufs: ctx.bufs.clone(),
        gates: ctx.render_gates.clone(),
        statuses: ctx.tab_statuses.clone(),
        local_snap: ctx.local_snap.clone(),
        net_hist: ctx.local_net_hist.clone(),
        sftp_handles: ctx.sftp_handles.clone(),
        sftp_last_cwd: ctx.sftp_last_cwd.clone(),
        follow_cd: ctx.sftp_follow_cd.clone(),
    }));
    if let Ok(mut routes) = ctx.tab_routes.lock() {
        routes.insert(tab_id.to_string(), route.clone());
    }

    // Separate SFTP connection for the same session (SSH only). It waits for
    // the interactive PTY to report Connected so a second SSH handshake cannot
    // contend with terminal startup on the same host/network path.
    let (sftp_evt_tx, sftp_ready_tx) = if has_sftp {
        let (sftp_tx, sftp_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
        let sftp_runtime = ctx.runtime.clone();
        let sftp_task_runtime = sftp_runtime.clone();
        // Read the handle map through the route at insertion time: if the tab
        // is dragged to another window while we connect, the route already
        // points at the destination and the handle must land there.
        let sftp_route = route.clone();
        let sftp_tab_id = tab_id.to_string();
        sftp_runtime.spawn(async move {
            // The interactive PTY may never report Connected (stalled
            // handshake); bound the wait so this bootstrap task cannot
            // outlive the tab forever.
            if !matches!(
                tokio::time::timeout(std::time::Duration::from_secs(30), ready_rx).await,
                Ok(Ok(()))
            ) {
                return;
            }
            tokio::task::yield_now().await;
            let sftp_handle = spawn_sftp(sftp_task_runtime.handle(), session, jump, sftp_tx);
            let handles = sftp_route
                .lock()
                .ok()
                .map(|r| r.sftp_handles.clone());
            if let Some(handles) = handles {
                if let Ok(mut handles) = handles.lock() {
                    handles.insert(sftp_tab_id, sftp_handle);
                }
            }
        });
        (Some(sftp_rx), Some(ready_tx))
    } else {
        (None, None)
    };

    // --- Shell event pump (dedicated thread) ---
    {
        let route_pump = route.clone();
        let rt_pump = ctx.runtime.clone();
        let tab_id_pump = tab_id.to_string();
        std::thread::spawn(move || {
            let mut shell_rx = rx;
            let mut sftp_ready_tx = sftp_ready_tx;
            let mut cwd_debounce: Option<tokio::task::JoinHandle<()>> = None;
            // Reusable scratch so a fast firehose doesn't reallocate every batch.
            let mut drained: Vec<SessionEvent> = Vec::new();
            // This survives drain batches, so a stream of small events cannot
            // evade the frame checkpoint merely because of thread timing.
            let mut ingested_since_checkpoint = 0usize;
            loop {
                // Block for the first event, then sweep up everything else that's
                // already queued. A burst — e.g. `tail -f` on a busy log (#171) —
                // then collapses into ONE invoke_from_event_loop and (after merging
                // adjacent Output below) ONE vt100 ingest + render, instead of one
                // UI task per chunk flooding the event loop and freezing the app.
                match shell_rx.blocking_recv() {
                    None => break,
                    Some(first) => drained.push(first),
                }
                // Cap the sweep so an unending stream still yields to the renderer
                // between batches (keeps the UI live rather than starved).
                const DRAIN_CAP: usize = 2048;
                while drained.len() < DRAIN_CAP {
                    match shell_rx.try_recv() {
                        Ok(evt) => drained.push(evt),
                        Err(_) => break,
                    }
                }

                // Resolve the current delivery route once per batch: a tab that
                // was detached/merged mid-stream simply delivers this batch
                // to its new window (#tab-detach).
                let Ok(rt) = route_pump.lock().map(|g| g.clone()) else {
                    continue;
                };

                // Run CwdChanged side-effects here (off the UI thread), drop the
                // swallowed ones, and concatenate runs of Output into a single chunk
                // so the UI parses + renders the whole burst once.
                let mut ui_batch: Vec<SessionEvent> = Vec::with_capacity(drained.len());
                for evt in drained.drain(..) {
                    match evt {
                        SessionEvent::Connected => {
                            if let Some(ready) = sftp_ready_tx.take() {
                                let _ = ready.send(());
                            }
                            ui_batch.push(SessionEvent::Connected);
                        }
                        SessionEvent::CwdChanged(cwd) => {
                            // Shared map (not a thread-local) so manual SFTP
                            // navigation can clear the entry — then the very next
                            // OSC 7, same directory or not, snaps the panel back to
                            // the shell's cwd. Unchanged repeats (every prompt
                            // re-emits OSC 7) are ignored (#59).
                            let changed = match rt.sftp_last_cwd.lock() {
                                Ok(mut m) => {
                                    m.insert(tab_id_pump.clone(), cwd.clone()).as_deref()
                                        != Some(cwd.as_str())
                                }
                                Err(_) => false,
                            };
                            // Swallow when follow-cd is off: forwarding it would set
                            // sftp_loading without any ListDir to clear it (the #59
                            // stuck-"loading" trap).
                            if !changed
                                || !rt.follow_cd.load(std::sync::atomic::Ordering::Relaxed)
                            {
                                continue;
                            }
                            if let Some(prev) = cwd_debounce.take() {
                                prev.abort();
                            }
                            let cwd_spawn = cwd.clone();
                            let sftp_h = rt.sftp_handles.clone();
                            let tid = tab_id_pump.clone();
                            cwd_debounce = Some(rt_pump.spawn(async move {
                                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                if let Ok(handles) = sftp_h.lock() {
                                    if let Some(h) = handles.get(&tid) {
                                        h.list_dir(cwd_spawn);
                                    }
                                }
                            }));
                            ui_batch.push(SessionEvent::CwdChanged(cwd));
                        }
                        SessionEvent::Output(chunk) => {
                            // Merge with the immediately preceding Output so the
                            // whole run is one vt100 ingest + one render. Only
                            // *adjacent* chunks merge, so byte order (and any
                            // interleaved event) is preserved exactly. Cap the
                            // merged size so one batch can't monopolize the UI
                            // thread for hundreds of ms (#209).
                            if let Some(SessionEvent::Output(prev)) = ui_batch.last_mut() {
                                if prev.len() + chunk.len() <= OUTPUT_MERGE_BYTE_CAP {
                                    prev.push_str(&chunk);
                                } else {
                                    ui_batch.push(SessionEvent::Output(chunk));
                                }
                            } else {
                                ui_batch.push(SessionEvent::Output(chunk));
                            }
                        }
                        other => ui_batch.push(other),
                    }
                }
                if ui_batch.is_empty() {
                    continue;
                }

                // Ingest terminal output on this pump thread (not the UI thread).
                // Keep each Output event atomic: TermBuffer detects full-screen
                // redraw sequences within one ingest call, so artificial byte
                // splits could corrupt scrollback when they bisect such a refresh.
                let mut remaining_output_bytes: usize = ui_batch
                    .iter()
                    .map(|event| match event {
                        SessionEvent::Output(chunk) => chunk.len(),
                        _ => 0,
                    })
                    .sum();
                let has_immediate_ui_events = ui_batch.iter().any(event_requires_immediate_ui);
                let mut dirty_since_request = false;
                let mut ui_only: Vec<SessionEvent> = Vec::with_capacity(ui_batch.len());
                for evt in ui_batch {
                    match evt {
                        SessionEvent::Output(chunk) => {
                            let chunk_len = chunk.len();
                            let reply = ingest_terminal_output(
                                &rt.bufs,
                                &tab_id_pump,
                                chunk.as_bytes(),
                            );
                            if !reply.is_empty() {
                                let _ = terminal_reply_tx.send(SessionCommand::RawInput(reply));
                            }
                            remaining_output_bytes =
                                remaining_output_bytes.saturating_sub(chunk_len);
                            dirty_since_request = true;

                            if record_ingested_chunk(chunk_len, &mut ingested_since_checkpoint) {
                                let ticket = request_tab_render(
                                    rt.window.clone(),
                                    &tab_id_pump,
                                    &rt.bufs,
                                    &rt.gates,
                                );
                                dirty_since_request = false;

                                // The event channel is intentionally unbounded
                                // today. Waiting while a large backlog exists would
                                // only move bytes from the terminal buffer into that
                                // channel and inflate memory, so catch up first and
                                // pace once the stream's tail is within reach.
                                if !has_immediate_ui_events
                                    && remaining_output_bytes <= PACED_LOCAL_BACKLOG_LIMIT
                                    && shell_rx.len() <= PACED_QUEUE_EVENT_LIMIT
                                {
                                    wait_for_ui_flush(ticket);
                                }
                            }
                        }
                        other => ui_only.push(other),
                    }
                }

                if dirty_since_request {
                    let _ = request_tab_render(
                        rt.window.clone(),
                        &tab_id_pump,
                        &rt.bufs,
                        &rt.gates,
                    );
                }

                if ui_only.is_empty() {
                    continue;
                }

                let rt_evt = rt.clone();
                let tid = tab_id_pump.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = rt_evt.window.upgrade() {
                        for evt in ui_only {
                            apply_session_event_to_window(
                                &win,
                                rt_evt.window_id,
                                &tid,
                                evt,
                                &rt_evt.bufs,
                                &rt_evt.gates,
                                &rt_evt.statuses,
                                &rt_evt.local_snap,
                                &rt_evt.net_hist,
                            );
                        }
                    }
                });
            }
        });
    }

    // --- SFTP event pump (separate thread, SSH only) ---
    if let Some(sftp_evt_tx) = sftp_evt_tx {
        let route_sftp = route.clone();
        let tab_id_sftp = tab_id.to_string();
        std::thread::spawn(move || {
            let mut sftp_rx = sftp_evt_tx;
            let mut drained: Vec<SessionEvent> = Vec::new();
            loop {
                match sftp_rx.blocking_recv() {
                    None => break,
                    Some(first) => drained.push(first),
                }
                const SFTP_DRAIN_CAP: usize = 256;
                while drained.len() < SFTP_DRAIN_CAP {
                    match sftp_rx.try_recv() {
                        Ok(evt) => drained.push(evt),
                        Err(_) => break,
                    }
                }
                let ui_batch: Vec<SessionEvent> = drained.drain(..).collect();
                if ui_batch.is_empty() {
                    continue;
                }
                let Ok(rt_s) = route_sftp.lock().map(|g| g.clone()) else {
                    continue;
                };
                let tid = tab_id_sftp.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(win) = rt_s.window.upgrade() {
                        for sftp_evt in ui_batch {
                            apply_session_event_to_window(
                                &win,
                                rt_s.window_id,
                                &tid,
                                sftp_evt,
                                &rt_s.bufs,
                                &rt_s.gates,
                                &rt_s.statuses,
                                &rt_s.local_snap,
                                &rt_s.net_hist,
                            );
                        }
                    }
                });
            }
        });
    }
}
