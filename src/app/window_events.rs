//! Raw winit event handling for a main window.

use super::*;

/// Raw winit event hook: file drop, wheel, focus/activity, close and window-size tracking.
pub(super) fn install_window_event_hook(
    ctx: &WinCtx,
    dock_stacks: &Rc<RefCell<DockStacks>>,
    activity: &Rc<Cell<WinActivity>>,
    exit_confirmed: &Rc<Cell<bool>>,
) {
    let WinCtx {
        core,
        store,
        registry,
        handles,
        sftp_handles,
        bufs,
        window,
        proc_win,
        sys_win,
        editor_win,
        pending_window_size_restore,
        window_size_tracking_ready,
        ..
    } = ctx;
    let window_id = ctx.window_id;
    // but only when the file is dropped over the file-list area.
    {
        use i_slint_backend_winit::winit::event::{
            MouseScrollDelta, TouchPhase, WindowEvent as WEvent,
        };
        use i_slint_backend_winit::EventResult;
        let weak = window.as_weak();
        let sh = sftp_handles.clone();
        let wheel_bufs = bufs.clone();
        let close_handles = handles.clone();
        let close_sftp_handles = sftp_handles.clone();
        let ev_proc_weak = proc_win.as_weak();
        let ev_sys_weak = sys_win.as_weak();
        let ev_editor_weak = editor_win.as_weak();
        let ev_store = store.clone();
        let ev_activity = activity.clone();
        let ev_exit_confirmed = exit_confirmed.clone();
        let ev_registry = registry.clone();
        let ev_core = core.clone();
        let ev_ds = dock_stacks.clone();
        let ev_window_size_tracking_ready = window_size_tracking_ready.clone();
        let ev_pending_window_size_restore = pending_window_size_restore.clone();
        let mut last_cursor_logical: Option<(f32, f32)> = None;
        let mut macos_wheel_accum = 0.0_f32;
        let hovered_terminal = Rc::new(HoveredTerminal::default());
        {
            let hovered = hovered_terminal.clone();
            window.on_terminal_output_hover(move |tab_id: SharedString, on: bool| {
                hovered.set(tab_id.as_str(), on);
            });
        }
        // Track the inputs that make up WinActivity; recompute on each change.
        let mut focused = true;
        let mut minimized = false;
        let mut occluded = false;
        // Apply the Win11 rounded-corner hint once, on the first event (the HWND
        // reliably exists by then, unlike a pre-run timer) (#166).
        let mut chrome_done = false;
        window
            .window()
            .on_winit_window_event(move |slint_window, event| {
                if !chrome_done {
                    chrome_done = true;
                    if let Some(win) = weak.upgrade() {
                        apply_window_chrome(win.window());
                    }
                }
                // Recompute window activity, push it to the shared cell, and update
                // Theme.window-focused (gates the cursor blink) (#127).
                let apply_activity = |focused: bool, minimized: bool, occluded: bool| {
                    let act = if minimized || occluded {
                        WinActivity::Hidden
                    } else if focused {
                        WinActivity::Active
                    } else {
                        WinActivity::Background
                    };
                    let prev = ev_activity.get();
                    ev_activity.set(act);
                    if let Some(win) = weak.upgrade() {
                        win.set_window_focused(act == WinActivity::Active);
                        win.set_dynamic_ui_active(act == WinActivity::Active);
                        if prev == WinActivity::Hidden && act != WinActivity::Hidden {
                            win.set_terminal_restore_cover(true);
                            let weak2 = weak.clone();
                            slint::Timer::single_shot(
                                std::time::Duration::from_millis(120),
                                move || {
                                    if let Some(w) = weak2.upgrade() {
                                        w.set_terminal_restore_cover(false);
                                    }
                                },
                            );
                        }
                    }
                };
                match event {
                    #[cfg(target_os = "windows")]
                    WEvent::KeyboardInput { event, .. } => {
                        // Microsoft IME can relabel a Ctrl key-up as Process while
                        // retaining the physical Ctrl scan code. Slint drops Process,
                        // so deliver the missing modifier release directly.
                        if let Some(side) = windows_process_ctrl_release(
                            event.state,
                            &event.logical_key,
                            &event.physical_key,
                        ) {
                            let key = match side {
                                CtrlKeySide::Left => slint::platform::Key::Control,
                                CtrlKeySide::Right => slint::platform::Key::ControlR,
                            };
                            slint_window.dispatch_event(
                                slint::platform::WindowEvent::KeyReleased { text: key.into() },
                            );
                            tracing::debug!(
                                "restored Windows IME Process-key Ctrl release side={side:?}"
                            );
                            return EventResult::PreventDefault;
                        }
                    }
                    #[cfg(target_os = "windows")]
                    WEvent::Ime(i_slint_backend_winit::winit::event::Ime::Disabled) => {
                        // Windows emits Ime::Disabled when a composition ends, including
                        // while switching between Chinese and English input methods. The
                        // Slint winit backend intentionally ignores this notification, so
                        // after several switches the native input context can remain
                        // detached and every TextInput appears to stop accepting keys
                        // (#236). Re-associate the window with its current default IME;
                        // the focused Slint TextInput keeps owning text input as before.
                        slint_window.with_winit_window(|window| window.set_ime_allowed(true));
                    }
                    WEvent::DroppedFile(path) => {
                        if let Some(win) = weak.upgrade() {
                            // On Windows the handler queries the OS cursor
                            // position itself (see `cursor_pos`), since OLE
                            // drag-and-drop hover suppresses WM_MOUSEMOVE.
                            // On macOS/X11/Wayland the compositor delivers
                            // pointer motion during drag-hover as ordinary
                            // CursorMoved events, so the last one we saw is
                            // the drop point (#356).
                            handle_file_drop(&win, &sh, path.clone(), last_cursor_logical);
                        }
                    }
                    WEvent::CursorMoved { position, .. } => {
                        if let Some(win) = weak.upgrade() {
                            let scale = win.window().scale_factor().max(0.01) as f64;
                            let p = position.to_logical::<f64>(scale);
                            last_cursor_logical = Some((p.x as f32, p.y as f32));
                        }
                    }
                    WEvent::MouseWheel { delta, phase, .. } if cfg!(target_os = "macos") => {
                        let Some((x, y)) = last_cursor_logical else {
                            return EventResult::Propagate;
                        };
                        let Some(win) = weak.upgrade() else {
                            return EventResult::Propagate;
                        };
                        if !macos_terminal_wheel_can_target_terminal(win.get_modal_open()) {
                            // Do not carry a partially accumulated modal gesture
                            // into the terminal after the modal closes.
                            macos_wheel_accum = 0.0;
                            return EventResult::Propagate;
                        }
                        // Only over the active tab's terminal output, by Slint's
                        // own hit testing. Over the SFTP file list (or anything
                        // else), let Slint scroll it natively (#540).
                        if !hovered_terminal.is(win.get_active_tab_id().as_str()) {
                            macos_wheel_accum = 0.0;
                            return EventResult::Propagate;
                        }
                        let hit = terminal_wheel_hit(&win, &wheel_bufs, x, y);
                        let wheel_lines = match delta {
                            MouseScrollDelta::LineDelta(_, dy) => dy * 3.0,
                            MouseScrollDelta::PixelDelta(p) => {
                                let scale = win.window().scale_factor().max(0.01) as f64;
                                let p = p.to_logical::<f64>(scale);
                                p.y as f32 / 18.0
                            }
                        };
                        match hit {
                            Some(hit) => {
                                if hit.is_alt {
                                    // PTY forwarding needs whole-notch steps:
                                    // bank fractional frames locally and send one
                                    // arrow burst per crossed notch.
                                    macos_wheel_accum += wheel_lines;
                                    let whole = macos_wheel_accum.trunc() as i32;
                                    if whole != 0 {
                                        macos_wheel_accum -= whole as f32;
                                        win.invoke_terminal_wheel(
                                            hit.tab_id.into(),
                                            whole.signum(),
                                            hit.col,
                                            hit.row,
                                        );
                                    }
                                } else {
                                    // Normal scrollback: forward the raw
                                    // fraction. TermBuffer.scroll_accum banks
                                    // the sub-line remainder so a decaying
                                    // macOS momentum tail glides to a stop
                                    // instead of stepping fixed amounts.
                                    win.invoke_terminal_scroll(hit.tab_id.into(), wheel_lines);
                                }
                                // A finished gesture must not leave its sub-line
                                // remainder behind to be tipped over by an
                                // unrelated touch later on.
                                if matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                                    macos_wheel_accum = 0.0;
                                }
                                // Claim every frame above the terminal — including
                                // zero and sub-line ones. Precise devices (trackpad,
                                // Magic Mouse) emit tiny PixelDelta frames, and mere
                                // finger contact on a Magic Mouse produces jitter
                                // frames without any real gesture. Letting those
                                // propagate would hand wheel events to the Slint
                                // TouchArea scroll-event path as well, double-
                                // feeding the same accumulator. Regular mice send
                                // LineDelta notches that always cross a whole line,
                                // which is why they never showed the problem.
                                //
                                // NOTE: arms of the outer `match event` below
                                // are statement position (the closure ends with
                                // its own `EventResult::Propagate`), so claim /
                                // pass decisions here must use explicit returns.
                                return EventResult::PreventDefault;
                            }
                            // Outside the terminal, native Slint scrolling
                            // (sidebars, dialogs) keeps working as before.
                            None => return EventResult::Propagate,
                        }
                    }
                    WEvent::Focused(f) => {
                        focused = *f;
                        apply_activity(focused, minimized, occluded);
                        if *f {
                            #[cfg(target_os = "windows")]
                            slint_window.with_winit_window(|window| window.set_ime_allowed(true));

                            // Some window managers deliver the first Resized event
                            // before the native window belongs to a monitor. Focus
                            // is a reliable second opportunity to seed restoration;
                            // request_inner_size will produce the Resized event that
                            // verifies the native window actually reached the target.
                            if !ev_window_size_tracking_ready.get() {
                                if let Some(win) = weak.upgrade() {
                                    if is_wayland_window(&win.window()) {
                                        ev_pending_window_size_restore.set(None);
                                        ev_window_size_tracking_ready.set(true);
                                        tracing::info!(
                                        "[WINDOW_SIZE] skipped persisted-size restore on Wayland"
                                    );
                                    } else if let Some(preferred) =
                                        ev_pending_window_size_restore.get()
                                    {
                                        if let Some(target) = clamp_window_size_to_monitor(
                                            &win.window(),
                                            Some(preferred),
                                        ) {
                                            tracing::info!(
                                                "[WINDOW_SIZE] focus retry saved={:.0}x{:.0} \
                                             target={:.0}x{:.0}",
                                                preferred.0,
                                                preferred.1,
                                                target.0,
                                                target.1,
                                            );
                                        }
                                    }
                                }
                            }
                            refresh_revealed_main_window(weak.clone());
                        }
                    }
                    WEvent::Occluded(o) => {
                        occluded = *o;
                        apply_activity(focused, minimized, occluded);
                        if !*o {
                            refresh_revealed_main_window(weak.clone());
                        }
                    }
                    WEvent::ScaleFactorChanged { .. } => {
                        // Moving a maximized frameless window between mixed-DPI
                        // monitors can leave Win11 reporting "maximized" while the
                        // native rectangle/render surface still has the old size.
                        refresh_revealed_main_window(weak.clone());
                    }
                    WEvent::Resized(size) => {
                        // A 0-sized resize is how Windows reports a minimize; track it
                        // so we pause the sampler while minimized (#127).
                        minimized = size.width == 0 || size.height == 0;
                        apply_activity(focused, minimized, occluded);
                        // Keep the maximize/restore icon (and resize-edge gating) in
                        // sync when the OS changes the window state (#119).
                        if let Some(win) = weak.upgrade() {
                            let (maxed, fullscreen) = win
                                .window()
                                .with_winit_window(|ww| {
                                    (ww.is_maximized(), ww.fullscreen().is_some())
                                })
                                .unwrap_or((false, false));
                            win.set_window_maximized(maxed);
                            // The OS can leave fullscreen on its own (macOS
                            // green button / gesture): drop the chromeless
                            // terminal-fullscreen layout with it (#495).
                            if win.get_term_fullscreen() && !fullscreen {
                                set_term_fullscreen(&win, false);
                            }
                            if !ev_window_size_tracking_ready.get()
                                && is_wayland_window(&win.window())
                            {
                                // The configure size in this event is authoritative
                                // on Wayland. Accept and persist that actual size;
                                // never chase the advisory saved size (#286).
                                ev_pending_window_size_restore.set(None);
                                ev_window_size_tracking_ready.set(true);
                                tracing::info!(
                                    "[WINDOW_SIZE] accepted compositor size {}x{} on Wayland",
                                    size.width,
                                    size.height
                                );
                            }
                            if !ev_window_size_tracking_ready.get() {
                                if let Some(preferred) = ev_pending_window_size_restore.get() {
                                    let scale = win.window().scale_factor().max(0.01);
                                    let actual =
                                        (size.width as f32 / scale, size.height as f32 / scale);
                                    if let Some(target) =
                                        clamp_window_size_to_monitor(&win.window(), Some(preferred))
                                    {
                                        tracing::info!(
                                            "[WINDOW_SIZE] restore requested saved={:.0}x{:.0} \
                                         target={:.0}x{:.0} actual={:.0}x{:.0} scale={:.2}",
                                            preferred.0,
                                            preferred.1,
                                            target.0,
                                            target.1,
                                            actual.0,
                                            actual.1,
                                            scale,
                                        );
                                        if (actual.0 - target.0).abs() <= 2.0
                                            && (actual.1 - target.1).abs() <= 2.0
                                        {
                                            ev_pending_window_size_restore.set(None);
                                            ev_window_size_tracking_ready.set(true);
                                            tracing::info!(
                                                "[WINDOW_SIZE] restore settled at {:.0}x{:.0}",
                                                actual.0,
                                                actual.1
                                            );
                                        }
                                    } else {
                                        tracing::warn!(
                                            "[WINDOW_SIZE] restore deferred: no monitor available \
                                         saved={:.0}x{:.0}",
                                            preferred.0,
                                            preferred.1,
                                        );
                                    }
                                } else {
                                    // First run: accept the initialized size as the
                                    // baseline, but do not persist this startup event.
                                    ev_window_size_tracking_ready.set(true);
                                }
                                return EventResult::Propagate;
                            }
                            // Record the last user-adjusted windowed size while the
                            // resize event still carries authoritative native
                            // geometry. Persisting only during CloseRequested can
                            // observe an installer/minimize transition instead
                            // (#278). Keep writes in memory here; save_layout flushes
                            // the config on exit.
                            if ev_window_size_tracking_ready.get()
                                && !maxed
                                && !fullscreen
                                && !minimized
                            {
                                let scale = win.window().scale_factor().max(0.01);
                                let width = size.width as f32 / scale;
                                let height = size.height as f32 / scale;
                                if width > 200.0 && height > 200.0 {
                                    ev_store.borrow_mut().set_window_size(width, height);
                                    tracing::debug!(
                                        "[WINDOW_SIZE] recorded user size {:.0}x{:.0}",
                                        width,
                                        height
                                    );
                                }
                            }
                        }
                    }
                    WEvent::CloseRequested => {
                        // Native close requests can come from the OS shutdown
                        // manager. On desktop platforms with our custom X,
                        // only that explicit button hides to the tray.
                        if cfg!(target_os = "macos") && tray::available() {
                            if let Some(win) = weak.upgrade() {
                                save_layout(&win, &ev_store, &ev_ds);
                                tray::hide_window(&ev_core, window_id, &win);
                            }
                            return EventResult::PreventDefault;
                        }
                        // Confirm before closing if there are open session tabs (#88),
                        // so a stray double-click on the title-bar icon / X / Alt+F4
                        // doesn't silently drop live sessions. Installer/Restart
                        // Manager may send repeated requests, so never intercept
                        // again after the user has confirmed shutdown (#267).
                        if should_block_close(
                            ev_exit_confirmed.get(),
                            !close_handles.borrow().is_empty(),
                        ) {
                            if let Some(win) = weak.upgrade() {
                                win.set_confirm_close_open(true);
                            }
                            return EventResult::PreventDefault;
                        }
                        ev_exit_confirmed.set(true);
                        // No sessions → the window is about to close; persist layout.
                        if let Some(win) = weak.upgrade() {
                            save_layout(&win, &ev_store, &ev_ds);
                            clear_zen_on_close(&win, &ev_store);
                        }
                        // The event is not prevented, so Slint will destroy this
                        // window. Mirror the confirmed custom-close path: tear down
                        // this window's workers and unregister it, quitting the
                        // shared event loop if it was the last one — otherwise a
                        // stale registry entry blocks the quit of a later window.
                        teardown_window(
                            window_id,
                            &close_handles,
                            &close_sftp_handles,
                            &ev_proc_weak,
                            &ev_sys_weak,
                            &ev_editor_weak,
                        );
                        if ev_registry.unregister(window_id) {
                            let _ = slint::quit_event_loop();
                        }
                        forget_window_state(&ev_core, window_id);
                    }
                    _ => {}
                }
                EventResult::Propagate
            });
    }
}
