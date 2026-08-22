//! Tab detach / merge (#tab-detach).
//!
//! Interaction: while a tab is dragged, the source window keeps the tab —
//! the gesture must survive until the pointer is released. Cross-window
//! feedback is live: whichever window sits under the cursor lights up as
//! the merge target and is raised to the front. The actual move happens on
//! DROP:
//!
//! * over another window  → the tab merges into it;
//! * back over the source → the usual pane split/move logic applies;
//! * over empty desktop   → the tab tears off into a new window created at
//!   the drop point (clamped onto the monitor so it never opens half
//!   off-screen), and a source window left without tabs closes itself.
//!
//! Session pumps are retargeted through the per-tab `TabRoute` (see
//! `core.rs`), so a moved tab keeps its live connection.

use super::*;

/// Move one tab's entry from the source window's per-tab map into the
/// destination's during a detach/merge. Poisoned locks are skipped, matching
/// every other lock site in this module.
fn move_locked_entry<V>(
    src: &Arc<Mutex<HashMap<String, V>>>,
    dst: &Arc<Mutex<HashMap<String, V>>>,
    tab_id: &str,
) {
    if let Ok(mut m) = src.lock() {
        if let Some(v) = m.remove(tab_id) {
            if let Ok(mut d) = dst.lock() {
                d.insert(tab_id.to_string(), v);
            }
        }
    }
}

/// One window's rectangle in global logical coordinates.
struct WinGeo {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

fn win_geo(win: &AppWindow) -> WinGeo {
    let scale = win.window().scale_factor().max(0.01);
    let pos = win.window().position();
    let size = win.window().size();
    WinGeo {
        x: pos.x as f32 / scale,
        y: pos.y as f32 / scale,
        w: size.width as f32 / scale,
        h: size.height as f32 / scale,
    }
}

fn contains(g: &WinGeo, x: f32, y: f32) -> bool {
    x >= g.x && x < g.x + g.w && y >= g.y && y < g.y + g.h
}

/// Pane-area-relative point → global logical coordinates.
fn global_point(core: &Rc<AppCore>, window_id: u64, x: f32, y: f32) -> Option<(f32, f32)> {
    let st = core.window_states.borrow().get(&window_id).cloned()?;
    let win = st.weak.upgrade()?;
    let g = win_geo(&win);
    Some((
        g.x + win.get_content_origin_x() + x,
        g.y + win.get_content_origin_y() + y,
    ))
}

/// The other window under the point, if any.
fn window_over(core: &Rc<AppCore>, exclude: u64, gx: f32, gy: f32) -> Option<u64> {
    let states = core.window_states.borrow();
    for (id, st) in states.iter() {
        if *id == exclude {
            continue;
        }
        let Some(win) = st.weak.upgrade() else {
            continue;
        };
        let g = win_geo(&win);
        if contains(&g, gx, gy) {
            return Some(*id);
        }
    }
    None
}

fn find_tab_row(st: &WindowState, tab_id: &str) -> Option<TabInfo> {
    use slint::Model as _;
    st.tabs_model.iter().find(|t| t.id.to_string() == tab_id)
}

/// Can this tab be moved? Any terminal tab that owns a session handle, in
/// any connection state: connected tabs keep their live connection,
/// disconnected tabs (state 2) reconnect in the new window (Enter-to-
/// reconnect, #79), and mid-connect tabs keep connecting because every
/// event (including auth prompts) is delivered through the per-tab
/// `TabRoute`, which the move retargets at the new window. Excluded: the
/// welcome tab (no session).
fn movable(core: &Rc<AppCore>, window_id: u64, tab_id: &str) -> bool {
    let states = core.window_states.borrow();
    let Some(st) = states.get(&window_id) else {
        return false;
    };
    let Some(row) = find_tab_row(st, tab_id) else {
        return false;
    };
    row.kind.to_string() == "terminal" && st.handles.borrow().contains_key(tab_id)
}

/// Highlight a window's content area as a merge target.
fn highlight_window(core: &Rc<AppCore>, window_id: u64) {
    let states = core.window_states.borrow();
    let Some(st) = states.get(&window_id) else {
        return;
    };
    let Some(win) = st.weak.upgrade() else {
        return;
    };
    let g = win_geo(&win);
    let ox = win.get_content_origin_x();
    let oy = win.get_content_origin_y();
    win.set_drag_active(true);
    win.set_drag_hl_x(-ox);
    win.set_drag_hl_y(-oy);
    win.set_drag_hl_w(g.w);
    win.set_drag_hl_h(g.h);
}

fn clear_highlight(core: &Rc<AppCore>, window_id: u64) {
    if let Some(st) = core.window_states.borrow().get(&window_id).cloned() {
        if let Some(w) = st.weak.upgrade() {
            w.set_drag_active(false);
        }
    }
}

fn clear_all_highlights(core: &Rc<AppCore>) {
    let ids: Vec<u64> = core.window_states.borrow().keys().copied().collect();
    for id in ids {
        clear_highlight(core, id);
    }
}

thread_local! {
    /// Window highlighted on the previous move event (raise-on-hover only
    /// fires when the hover target changes).
    static LAST_TARGET: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Per-move cross-window handling. Returns true when the cursor is outside
/// the source window (caller then skips the in-window pane highlight).
pub(super) fn handle_global_tab_drag_move(
    core: &Rc<AppCore>,
    window_id: u64,
    tab_id: &str,
    x: f32,
    y: f32,
) -> bool {
    let Some((gx, gy)) = global_point(core, window_id, x, y) else {
        return false;
    };
    let over_src = {
        let states = core.window_states.borrow();
        states
            .get(&window_id)
            .and_then(|st| st.weak.upgrade())
            .map(|w| contains(&win_geo(&w), gx, gy))
            .unwrap_or(true)
    };
    if over_src {
        LAST_TARGET.with(|t| t.set(None));
        clear_all_highlights(core);
        return false; // plain in-window drag: pane highlight logic applies
    }

    // Cross-window: nothing is moved until the drop. Light up (and raise)
    // whichever window would receive the tab; a non-movable tab gets no
    // merge preview.
    let target = if movable(core, window_id, tab_id) {
        window_over(core, window_id, gx, gy)
    } else {
        None
    };
    if let Some(t) = target {
        highlight_window(core, t);
        let changed = LAST_TARGET.with(|c| {
            let prev = c.get();
            c.set(Some(t));
            prev != Some(t)
        });
        if changed {
            // Bring the hovered window to the front so the user sees where
            // the tab would land (Chrome-style drag).
            if let Some(st) = core.window_states.borrow().get(&t).cloned() {
                if let Some(w) = st.weak.upgrade() {
                    w.window().with_winit_window(|ww| ww.focus_window());
                }
            }
        }
    } else {
        LAST_TARGET.with(|t| t.set(None));
        clear_all_highlights(core);
    }
    clear_highlight(core, window_id);
    true
}

/// Drop handling. Returns true when the drop was consumed here (caller skips
/// the in-window split logic).
pub(super) fn handle_global_tab_drag_drop(
    core: &Rc<AppCore>,
    window_id: u64,
    tab_id: &str,
    x: f32,
    y: f32,
) -> bool {
    LAST_TARGET.with(|t| t.set(None));
    clear_all_highlights(core);

    let Some((gx, gy)) = global_point(core, window_id, x, y) else {
        tracing::warn!("tab-drop: global_point failed (window {window_id}, local {x},{y})");
        return false;
    };
    let over_src = {
        let states = core.window_states.borrow();
        states
            .get(&window_id)
            .and_then(|st| st.weak.upgrade())
            .map(|w| contains(&win_geo(&w), gx, gy))
            .unwrap_or(true)
    };
    tracing::warn!(
        "tab-drop: tab={tab_id} global=({gx},{gy}) over_src={over_src} movable={}",
        movable(core, window_id, tab_id)
    );

    // A drop inside the source window always keeps the in-window pane
    // split/move logic, even if another window's rect overlaps underneath
    // (maximized or stacked windows share the same pixels).
    if over_src {
        return false;
    }
    // Merge into whichever other window received the drop.
    if let Some(dst) = window_over(core, window_id, gx, gy) {
        tracing::warn!("tab-drop: merge into window {dst}");
        if movable(core, window_id, tab_id) {
            let moved = move_tab_between_windows(core, window_id, dst, tab_id);
            // A window emptied by the merge closes itself, exactly like the
            // desktop-detach path below.
            if moved && window_tab_count(core, window_id) == 0 {
                close_window_now(core, window_id);
            }
        }
        return true;
    }
    // Empty desktop: tear off into a new window at the drop point.
    if !movable(core, window_id, tab_id) {
        tracing::warn!("tab-drop: not movable, detach skipped");
        return true;
    }
    let Some(src_win) = core
        .window_states
        .borrow()
        .get(&window_id)
        .and_then(|st| st.weak.upgrade())
    else {
        tracing::warn!("tab-drop: source window gone");
        return true;
    };
    let at = clamped_detach_position(&src_win, gx, gy);
    match open_window(core.clone(), false, Some(at)) {
        Ok(new_id) => {
            tracing::warn!("tab-drop: detach into new window {new_id}");
            {
                // The fresh window opens with a welcome tab; the detached
                // session takes its place.
                {
                    let states = core.window_states.borrow();
                    if let Some(st) = states.get(&new_id) {
                        use slint::Model as _;
                        if let Some(i) = st
                            .tabs_model
                            .iter()
                            .position(|t| t.id.to_string() == "welcome")
                        {
                            st.tabs_model.remove(i);
                        }
                        st.layout.borrow_mut().remove_tab("welcome");
                    }
                }
                let moved = move_tab_between_windows(core, window_id, new_id, tab_id);
                let left = window_tab_count(core, window_id);
                tracing::warn!("tab-drop: moved={moved} source tabs left={left}");
                if moved && left == 0 {
                    close_window_now(core, window_id);
                }
            }
        }
        Err(err) => {
            tracing::warn!("tab-drop: open_window failed: {err:#}");
        }
    }
    true
}

/// Drop point → top-left for the torn-off window, clamped so the window
/// lands fully on the source window's monitor (a window created half
/// off-screen renders with a missing strip on some systems).
fn clamped_detach_position(src_win: &AppWindow, gx: f32, gy: f32) -> slint::PhysicalPosition {
    let scale = src_win.window().scale_factor().max(0.01);
    let src_geo = win_geo(src_win);
    // Estimate the new window's size with the source window's size.
    let (est_w, est_h) = (src_geo.w, src_geo.h);
    let mut left = gx - 24.0;
    let mut top = gy - 8.0;
    let clamped = src_win.window().with_winit_window(|ww| {
        let Some(monitor) = ww.current_monitor().or_else(|| ww.primary_monitor()) else {
            return None;
        };
        let mscale = monitor.scale_factor().max(0.01);
        let mx = monitor.position().x as f32 / mscale as f32;
        let my = monitor.position().y as f32 / mscale as f32;
        let mw = monitor.size().width as f32 / mscale as f32;
        let mh = monitor.size().height as f32 / mscale as f32;
        Some((mx, my, mw, mh))
    });
    if let Some(Some((mx, my, mw, mh))) = clamped {
        left = left.clamp(mx + 8.0, (mx + mw - est_w - 8.0).max(mx + 8.0));
        top = top.clamp(my + 8.0, (my + mh - est_h - 8.0).max(my + 8.0));
    }
    slint::PhysicalPosition::new((left * scale) as i32, (top * scale) as i32)
}

/// Terminal-tab count of a window (welcome tab excluded): a window torn empty
/// by a tab detach should close even though its welcome tab remains.
fn window_tab_count(core: &Rc<AppCore>, window_id: u64) -> usize {
    use slint::Model as _;
    core.window_states
        .borrow()
        .get(&window_id)
        .map(|st| {
            st.tabs_model
                .iter()
                .filter(|t| t.id.as_str() != "welcome")
                .count()
        })
        .unwrap_or(0)
}

/// Move one connected tab (session, buffers, layout membership, UI rows)
/// from window `src_id` to window `dst_id` and retarget its pumps.
pub(super) fn move_tab_between_windows(
    core: &Rc<AppCore>,
    src_id: u64,
    dst_id: u64,
    tab_id: &str,
) -> bool {
    use slint::Model as _;
    let (src, dst) = {
        let states = core.window_states.borrow();
        let Some(src) = states.get(&src_id).cloned() else {
            return false;
        };
        let Some(dst) = states.get(&dst_id).cloned() else {
            return false;
        };
        let Some(row) = find_tab_row(&src, tab_id) else {
            return false;
        };
        if row.kind.to_string() != "terminal" || !src.handles.borrow().contains_key(tab_id) {
            return false;
        }
        (src, dst)
    };

    // UI rows travel with the tab.
    let Some(tab_i) = src
        .tabs_model
        .iter()
        .position(|t| t.id.to_string() == tab_id)
    else {
        return false;
    };
    let tab_row = src.tabs_model.row_data(tab_i).unwrap();
    src.tabs_model.remove(tab_i);
    let term_row = {
        let mut row = None;
        let mut idx = None;
        for (i, r) in src.terminals_model.iter().enumerate() {
            if r.id.to_string() == tab_id {
                row = Some(r);
                idx = Some(i);
                break;
            }
        }
        if let Some(i) = idx {
            src.terminals_model.remove(i);
        }
        row
    };

    // Layout membership.
    src.layout.borrow_mut().remove_tab(tab_id);
    dst.layout.borrow_mut().add_tab(tab_id.to_string());

    // Session worker + per-tab state maps move wholesale.
    if let Some(h) = src.handles.borrow_mut().remove(tab_id) {
        dst.handles.borrow_mut().insert(tab_id.to_string(), h);
    }
    move_locked_entry(&src.bufs, &dst.bufs, tab_id);
    move_locked_entry(&src.gates, &dst.gates, tab_id);
    move_locked_entry(&src.statuses, &dst.statuses, tab_id);
    move_locked_entry(&src.sftp_handles, &dst.sftp_handles, tab_id);
    move_locked_entry(&src.sftp_last_cwd, &dst.sftp_last_cwd, tab_id);

    // Retarget the running pumps at the new window.
    if let Ok(routes) = core.tab_routes.lock() {
        if let Some(route) = routes.get(tab_id) {
            if let Ok(mut r) = route.lock() {
                *r = TabRoute {
                    window: dst.weak.clone(),
                    window_id: dst_id,
                    bufs: dst.bufs.clone(),
                    gates: dst.gates.clone(),
                    statuses: dst.statuses.clone(),
                    local_snap: dst.local_snap.clone(),
                    net_hist: dst.net_hist.clone(),
                    sftp_handles: dst.sftp_handles.clone(),
                    sftp_last_cwd: dst.sftp_last_cwd.clone(),
                    follow_cd: dst.follow_cd.clone(),
                };
            }
        }
    }

    // Land the rows in the target window, select the tab, and paint the
    // terminal immediately instead of waiting for the next output burst.
    dst.tabs_model.push(tab_row);
    if let Some(row) = term_row {
        dst.terminals_model.push(row);
    }
    if let Some(w) = dst.weak.upgrade() {
        refresh_panes(
            &w,
            &dst.layout.borrow(),
            dst.content_size.get(),
            &dst.tabs_model,
            &dst.panes_model,
            &dst.splitters_model,
        );
        rebuild_tab_display(&w, &dst.bufs, tab_id);
    }
    if let Some(w) = src.weak.upgrade() {
        refresh_panes(
            &w,
            &src.layout.borrow(),
            src.content_size.get(),
            &src.tabs_model,
            &src.panes_model,
            &src.splitters_model,
        );
    }
    true
}

/// Programmatic close for a window emptied by a tab detach. Mirrors the
/// confirmed-close path: persist layout, tear workers down, unregister,
/// quit when it was the last window.
pub(super) fn close_window_now(core: &Rc<AppCore>, window_id: u64) {
    let st = core.window_states.borrow().get(&window_id).cloned();
    let Some(st) = st else {
        return;
    };
    if let Some(win) = st.weak.upgrade() {
        save_layout(&win, &core.store);
        clear_zen_on_close(&win, &core.store);
        teardown_window(
            window_id,
            &st.handles,
            &st.sftp_handles,
            &st.proc_weak,
            &st.sys_weak,
        );
        let _ = win.hide();
    }
    if core.registry.unregister(window_id) {
        let _ = slint::quit_event_loop();
    }
    forget_window_state(core, window_id);
}

/// Drop a closed window's shared state and any routes still pointing at it.
pub(super) fn forget_window_state(core: &Rc<AppCore>, window_id: u64) {
    if let Some(st) = core.window_states.borrow_mut().remove(&window_id) {
        // Deterministic teardown: stop this window's repeated timers and hide
        // its monitor windows now rather than relying on the handle drops.
        st.timers.borrow_mut().clear();
        let _ = st.proc_win.hide();
        let _ = st.sys_win.hide();
    }
    if let Ok(mut routes) = core.tab_routes.lock() {
        routes.retain(|_, route| {
            route
                .lock()
                .map(|r| r.window_id != window_id)
                .unwrap_or(false)
        });
    }
}
