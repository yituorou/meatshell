use super::*;

pub(super) fn wire_tab_callbacks(
    window: &AppWindow,
    window_id: u64,
    core: Rc<AppCore>,
    tabs_model: Rc<VecModel<TabInfo>>,
    terminals_model: Rc<VecModel<TerminalState>>,
    layout: Rc<RefCell<crate::layout::Layout>>,
    content_size: Rc<std::cell::Cell<(f32, f32)>>,
    panes_model: Rc<VecModel<PaneInfo>>,
    splitters_model: Rc<VecModel<SplitterInfo>>,
    handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    bufs: TermBuffers,
    render_gates: RenderGates,
    sftp_handles: SftpHandles,
    sftp_last_cwd: SftpLastCwd,
    tab_titles: Rc<RefCell<HashMap<String, String>>>,
) {
    // Ctrl+Tab / Ctrl+Shift+Tab cycle within the currently focused pane (#294).
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        let bufs_cycle = bufs.clone();
        window.on_cycle_tab(move |reverse: bool| {
            let next = layout.borrow_mut().cycle_focused_tab(reverse);
            let Some(id) = next else {
                return;
            };
            if let Some(w) = weak.upgrade() {
                refresh_panes(
                    &w,
                    &layout.borrow(),
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
                rebuild_tab_display(&w, &bufs_cycle, &id);
            }
        });
    }

    // Select a tab inside a pane: make it that pane's active tab and focus the
    // pane. refresh_panes propagates active-tab-id (→ sidebar refresh).
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        let bufs_tab_sel = bufs.clone();
        window.on_pane_tab_selected(move |pane_id: i32, id: SharedString| {
            let id = id.to_string();
            {
                let mut lay = layout.borrow_mut();
                lay.focused = pane_id as u64;
                if let Some(l) = lay.leaf_mut(pane_id as u64) {
                    if l.tabs.iter().any(|t| t == &id) {
                        l.active = id.clone();
                    }
                }
            }
            if let Some(w) = weak.upgrade() {
                refresh_panes(
                    &w,
                    &layout.borrow(),
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
                // Tab just became visible — render any output ingested while it
                // was in the background (e.g. another session was unzipping).
                rebuild_tab_display(&w, &bufs_tab_sel, &id);
            }
        });
    }

    // Drag-to-reorder within a pane's strip: move the tab at `from` one slot in
    // `dir`. Only the pane's own tab order changes; content shows by active id.
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_pane_tab_reorder(move |pane_id: i32, from: i32, dir: i32| {
            {
                let mut lay = layout.borrow_mut();
                if let Some(l) = lay.leaf_mut(pane_id as u64) {
                    let n = l.tabs.len() as i32;
                    if n <= 1 {
                        return;
                    }
                    let from = from.clamp(0, n - 1);
                    let to = (from + dir).clamp(0, n - 1);
                    if from == to {
                        return;
                    }
                    let item = l.tabs.remove(from as usize);
                    l.tabs.insert(to as usize, item);
                }
            }
            if let Some(w) = weak.upgrade() {
                // Reordering refreshes the tab model and replaces the original
                // drag source before it can receive pointer-up. Clear the
                // insertion caret on this same-pane path before refreshing.
                w.set_drag_active(false);
                refresh_panes(
                    &w,
                    &layout.borrow(),
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
            }
        });
    }

    // Close a tab: tear down its session / buffers, drop it from the models, then
    // remove it from the split tree (which re-homes the pane's active tab and
    // collapses the pane if it becomes empty).
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let terminals_model = terminals_model.clone();
        let handles = handles.clone();
        let bufs = bufs.clone();
        let render_gates = render_gates.clone();
        let sftp_handles = sftp_handles.clone();
        let sftp_last_cwd = sftp_last_cwd.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        let tab_titles = tab_titles.clone();
        let tab_routes = core.tab_routes.clone();
        window.on_pane_tab_closed(move |_pane_id: i32, id: SharedString| {
            let id = id.to_string();
            if id == "welcome" {
                return;
            }
            tab_titles.borrow_mut().remove(&id);
            if let Some(handle) = handles.borrow_mut().remove(&id) {
                handle.close();
            }
            if let Some(sftp) = sftp_handles.lock().unwrap().remove(&id) {
                sftp.close();
            }
            sftp_last_cwd.lock().unwrap().remove(&id);
            if let Some(gate) = render_gates.lock().unwrap().remove(&id) {
                gate.close();
            }
            bufs.lock().unwrap().remove(&id);
            if let Ok(mut routes) = tab_routes.lock() {
                routes.remove(&id);
            }

            // Remove from tabs + terminals models.
            let mut idx = None;
            for i in 0..tabs_model.row_count() {
                if tabs_model
                    .row_data(i)
                    .map(|r| r.id.as_str() == id)
                    .unwrap_or(false)
                {
                    idx = Some(i);
                    break;
                }
            }
            if let Some(i) = idx {
                tabs_model.remove(i);
            }
            let mut tidx = None;
            for i in 0..terminals_model.row_count() {
                if terminals_model
                    .row_data(i)
                    .map(|r| r.id.as_str() == id)
                    .unwrap_or(false)
                {
                    tidx = Some(i);
                    break;
                }
            }
            if let Some(i) = tidx {
                terminals_model.remove(i);
            }

            layout.borrow_mut().remove_tab(&id);
            if let Some(w) = weak.upgrade() {
                refresh_panes(
                    &w,
                    &layout.borrow(),
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
            }
        });
    }

    // "+" in a pane's strip: focus the welcome page (there is a single welcome
    // tab; move focus to whichever pane owns it and make it active).
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_pane_new_tab(move |pane_id: i32| {
            // In welcome-as-sidebar mode there is no welcome tab — the session list
            // lives in the left panel, so "+" has nothing to open.
            if weak
                .upgrade()
                .map(|w| w.get_welcome_as_sidebar())
                .unwrap_or(false)
            {
                return;
            }
            {
                let mut lay = layout.borrow_mut();
                if let Some(owner) = lay.leaf_of_tab("welcome") {
                    lay.focused = owner;
                    if let Some(l) = lay.leaf_mut(owner) {
                        l.active = "welcome".into();
                    }
                } else {
                    lay.focused = pane_id as u64;
                    lay.add_tab("welcome".into());
                }
            }
            if let Some(w) = weak.upgrade() {
                refresh_panes(
                    &w,
                    &layout.borrow(),
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
            }
        });
    }

    // Click anywhere in a pane → focus it (drives which terminal the sidebar and
    // key routing follow). A single pane is always focused, so this is a no-op
    // until splits exist.
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_pane_focus(move |pane_id: i32| {
            {
                let mut lay = layout.borrow_mut();
                if lay.leaf(pane_id as u64).is_some() {
                    lay.focused = pane_id as u64;
                }
            }
            if let Some(w) = weak.upgrade() {
                refresh_panes(
                    &w,
                    &layout.borrow(),
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
            }
        });
    }

    // Drag a splitter to re-balance the two panes it divides. `pos` is the new
    // boundary position in content coordinates along the split's axis; we look
    // the split's axis window up from a fresh flatten and convert it to a ratio.
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_splitter_drag(move |split_id: i32, pos: f32, _vertical: bool| {
            {
                let mut lay = layout.borrow_mut();
                let (cw, ch) = content_size.get();
                let extent = {
                    let (_, splits) = lay.flatten(0.0, 0.0, cw.max(1.0), ch.max(1.0));
                    splits
                        .iter()
                        .find(|s| s.split_id == split_id as u64)
                        .map(|s| (s.axis_start, s.axis_len))
                };
                if let Some((start, len)) = extent {
                    lay.set_ratio(split_id as u64, start, len, pos);
                }
            }
            if let Some(w) = weak.upgrade() {
                refresh_panes(
                    &w,
                    &layout.borrow(),
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
            }
        });
    }

    // Split a pane: peel `tab-id` out of pane `pane-id` into a new pane on the
    // given side ("left"/"right"/"up"/"down"). Needs >1 tab so the source pane
    // doesn't empty and immediately collapse back.
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_pane_split(
            move |pane_id: i32, tab_id: SharedString, dir: SharedString| {
                let tab_id = tab_id.to_string();
                {
                    let mut lay = layout.borrow_mut();
                    let can = lay
                        .leaf(pane_id as u64)
                        .map(|l| l.tabs.len() > 1 && l.tabs.iter().any(|t| t == &tab_id))
                        .unwrap_or(false);
                    if !can {
                        return;
                    }
                    let (d, before) = match dir.as_str() {
                        "left" => (crate::layout::Dir::Horizontal, true),
                        "right" => (crate::layout::Dir::Horizontal, false),
                        "up" => (crate::layout::Dir::Vertical, true),
                        _ => (crate::layout::Dir::Vertical, false), // "down"
                    };
                    lay.split(pane_id as u64, d, &tab_id, before);
                }
                if let Some(w) = weak.upgrade() {
                    refresh_panes(
                        &w,
                        &layout.borrow(),
                        content_size.get(),
                        &tabs_model,
                        &panes_model,
                        &splitters_model,
                    );
                }
            },
        );
    }

    // Merge a split pane back into another pane. The source pane's tabs are
    // appended to the first remaining pane, then the emptied source collapses.
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_pane_merge(move |pane_id: i32| {
            {
                let mut lay = layout.borrow_mut();
                lay.merge_leaf_into_other(pane_id as u64);
            }
            if let Some(w) = weak.upgrade() {
                refresh_panes(
                    &w,
                    &layout.borrow(),
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
            }
        });
    }

    // Drag-to-split: while a tab is dragged over the pane area, highlight the
    // drop zone the cursor is in (an edge band → split, the middle → move).
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let core = core.clone();
        window.on_tab_drag_move(move |tab_id: SharedString, x: f32, y: f32| {
            let tab_id = tab_id.to_string();
            // Cross-window detach/merge owns the feedback once the cursor
            // leaves the window (the torn-off window follows the pointer,
            // the window under the cursor lights up) (#tab-detach).
            if handle_global_tab_drag_move(&core, window_id, &tab_id, x, y) {
                return;
            }
            if let Some(w) = weak.upgrade() {
                match drag_target(&layout.borrow(), content_size.get(), x, y) {
                    Some((_, _, (hx, hy, hw, hh))) => {
                        w.set_drag_active(true);
                        w.set_drag_hl_x(hx);
                        w.set_drag_hl_y(hy);
                        w.set_drag_hl_w(hw);
                        w.set_drag_hl_h(hh);
                    }
                    None => w.set_drag_active(false),
                }
            }
        });
    }

    // Drop: split the target pane toward the dropped-on edge (peeling the tab
    // into the new pane), or drop into another pane's tab group from the middle
    // / tab strip (IDEA-style merge by dragging onto the tab row).
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        window.on_tab_drag_drop(move |tab_id: SharedString, x: f32, y: f32| {
            let tab_id = tab_id.to_string();
            // A cross-window drag (torn-off window following the cursor) is
            // settled here: merged into the window under the pointer, or
            // kept where it is (#tab-detach).
            if handle_global_tab_drag_drop(&core, window_id, &tab_id, x, y) {
                return;
            }
            let target = drag_target(&layout.borrow(), content_size.get(), x, y);
            if let Some((pane, zone, _)) = target {
                let mut lay = layout.borrow_mut();
                let src = lay.leaf_of_tab(&tab_id);
                match zone {
                    "left" => {
                        lay.split(pane, crate::layout::Dir::Horizontal, &tab_id, true);
                    }
                    "right" => {
                        lay.split(pane, crate::layout::Dir::Horizontal, &tab_id, false);
                    }
                    "up" => {
                        lay.split(pane, crate::layout::Dir::Vertical, &tab_id, true);
                    }
                    "down" => {
                        lay.split(pane, crate::layout::Dir::Vertical, &tab_id, false);
                    }
                    "tabstrip" => {
                        if src != Some(pane) {
                            lay.move_tab(&tab_id, pane);
                        }
                    }
                    _ => {
                        if src != Some(pane) {
                            lay.move_tab(&tab_id, pane);
                        }
                    }
                }
            }
            if let Some(w) = weak.upgrade() {
                w.set_drag_active(false);
                refresh_panes(
                    &w,
                    &layout.borrow(),
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
            }
        });
    }
}

// ---------------------------------------------------------------------------
// SFTP callbacks
// ---------------------------------------------------------------------------
