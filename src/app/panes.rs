//! Pane/dock layout: refreshing, persisting and drag targets.

use super::*;

/// Resolve a session's configured SSH jump host to the saved session it points
/// at, ignoring a missing / dangling / self reference (#211).
pub(super) fn save_layout(
    win: &AppWindow,
    store: &Rc<RefCell<ConfigStore>>,
    dock_stacks: &Rc<RefCell<DockStacks>>,
) {
    let scale = win.window().scale_factor().max(0.01);
    let size = win.window().size();
    let w = size.width as f32 / scale;
    let h = size.height as f32 / scale;
    let mut s = store.borrow_mut();
    s.set_sidebar_width(win.get_sidebar_width());
    s.set_sidebar_height(win.get_sidebar_height());
    s.set_sidebar_dock(win.get_sidebar_dock().to_string());
    s.set_sidebar_collapsed(win.get_sidebar_collapsed());
    s.set_sftp_panel_width(win.get_sftp_panel_width());
    s.set_sftp_panel_height(win.get_sftp_panel_height());
    s.set_sftp_dock(win.get_sftp_dock().to_string());
    // Split view: whether it was open, the last local folder and the divider
    // share (#532).
    s.set_sftp_split_enabled(win.get_sftp_split_enabled());
    s.set_sftp_local_split(win.get_sftp_local_split());
    s.set_sftp_local_dir(&win.get_sftp_last_local_dir());
    // Local bookmarks / recent folders (#532); the Slint models are the live
    // copy, the config only needs them on close.
    let places = crate::config::LocalPlaces {
        bookmarks: sftp_local::model_strings(&win.get_local_bookmarks()),
        recent: sftp_local::model_strings(&win.get_local_recent()),
    };
    s.update_local_places(|stored| *stored = places);
    s.set_quick_panel_open(win.get_quick_panel_open());
    s.set_quick_panel_collapsed(win.get_quick_panel_collapsed());
    s.set_quick_panel_width(win.get_quick_panel_width());
    s.set_quick_panel_height(win.get_quick_panel_height());
    s.set_quick_panel_dock(win.get_quick_panel_dock().to_string());
    // Per-edge stacked panels (#dock-stack), ratios included.
    s.set_dock_stacks(dock_stacks.borrow().to_saved());
    s.set_welcome_sidebar_width(win.get_welcome_sidebar_width());
    s.set_welcome_sidebar_dock(win.get_welcome_sidebar_dock().to_string());
    s.set_welcome_collapsed(win.get_welcome_collapsed());
    // A maximized size isn't a useful "preferred" size to restore to, so only
    // remember the windowed size. Ask the native window too, because the Slint
    // property can lag during startup/shutdown on frameless Windows (#234).
    let native_maximized = win
        .window()
        .with_winit_window(|ww| ww.is_maximized() || ww.fullscreen().is_some())
        .unwrap_or_else(|| win.get_window_maximized() || win.get_term_fullscreen());
    if !native_maximized && w > 200.0 && h > 200.0 {
        // Resize events normally keep this cache current. Persist the final
        // valid native geometry as well, because a close can arrive before the
        // last resize callback has reached the UI store.
        s.set_window_size(w, h);
    }
    let _ = s.save();
}

/// Zen is a transient per-window state. Closing a window while it is on must
/// not leak the persisted flag into the next window, which would open stuck in
/// zen mode (#zen). Called on every confirmed-close path right after
/// `save_layout`.
pub(crate) fn clear_zen_on_close(win: &AppWindow, store: &Rc<RefCell<ConfigStore>>) {
    if !win.get_zen_mode() {
        return;
    }
    let mut s = store.borrow_mut();
    s.set_zen_mode(false);
    let _ = s.save();
}

/// Every quick-command group name (used to start with all groups collapsed, #55):
/// "default" when any ungrouped command exists, plus explicit quick-groups and any
/// group referenced by a command.
pub(super) fn tabs_eq(a: &ModelRc<TabInfo>, b: &ModelRc<TabInfo>) -> bool {
    if a.row_count() != b.row_count() {
        return false;
    }
    (0..a.row_count()).all(|i| match (a.row_data(i), b.row_data(i)) {
        (Some(x), Some(y)) => x.id == y.id,
        _ => false,
    })
}

/// Find the terminal row with `tab_id`, apply `mutator`, and write it back.
pub(super) fn update_terminal_row(
    model: &VecModel<TerminalState>,
    tab_id: &str,
    mutator: impl FnOnce(&mut TerminalState),
) {
    for i in 0..model.row_count() {
        if let Some(mut row) = model.row_data(i) {
            if row.id.as_str() == tab_id {
                mutator(&mut row);
                model.set_row_data(i, row);
                return;
            }
        }
    }
}

pub(super) fn update_welcome_tab(layout: &mut crate::layout::Layout, as_sidebar: bool) {
    if as_sidebar {
        layout.remove_tab("welcome");
    } else if layout.leaf_of_tab("welcome").is_none() {
        layout.add_tab("welcome".into());
    }
}

pub(super) fn refresh_panes(
    window: &AppWindow,
    layout: &crate::layout::Layout,
    content: (f32, f32),
    tabs_model: &VecModel<TabInfo>,
    panes_model: &VecModel<PaneInfo>,
    splitters_model: &VecModel<SplitterInfo>,
) {
    let (cw, ch) = (content.0.max(1.0), content.1.max(1.0));
    let (panes, splits) = layout.flatten(0.0, 0.0, cw, ch);

    let pane_infos: Vec<PaneInfo> = panes
        .iter()
        .map(|p| {
            // Map this pane's tab ids to their TabInfo rows (skipping any not yet
            // in the model).
            let tabs: Vec<TabInfo> = p
                .tabs
                .iter()
                .filter_map(|tid| {
                    (0..tabs_model.row_count()).find_map(|i| {
                        let row = tabs_model.row_data(i)?;
                        (row.id.as_str() == tid.as_str()).then_some(row)
                    })
                })
                .collect();
            // Only the pane touching the top-right corner keeps room for the
            // floating toolbar icons (#122).
            let top_right = p.x + p.w >= cw - 0.5 && p.y <= 0.5;
            PaneInfo {
                id: p.id as i32,
                x: p.x,
                y: p.y,
                w: p.w,
                h: p.h,
                active_id: p.active.clone().into(),
                focused: p.focused,
                // Room for the seven top-right toolbar icons (30px each, incl. the
                // pin #450 and Updates).
                reserve_right: if top_right { 240.0 } else { 0.0 },
                tabs: ModelRc::from(Rc::new(VecModel::from(tabs))),
            }
        })
        .collect();

    // Update the models IN PLACE rather than replacing them, so the `for pane` /
    // `for sp` elements are reused: this keeps terminals from being recreated on
    // every refresh AND preserves the splitter's pointer-grab during a drag (a
    // fresh model would destroy the element mid-drag and drop the grab). When the
    // structure changes (split/close → different row count) a full rebuild is fine
    // since no drag is in flight.
    if panes_model.row_count() == pane_infos.len() {
        for (i, mut r) in pane_infos.into_iter().enumerate() {
            if let Some(old) = panes_model.row_data(i) {
                // Reuse the existing tab sub-model when the tabs are unchanged so a
                // geometry-only refresh doesn't churn the tab strips.
                let same_tabs = old.id == r.id && tabs_eq(&old.tabs, &r.tabs);
                let unchanged = same_tabs
                    && old.x == r.x
                    && old.y == r.y
                    && old.w == r.w
                    && old.h == r.h
                    && old.active_id == r.active_id
                    && old.focused == r.focused
                    && old.reserve_right == r.reserve_right;
                if same_tabs {
                    r.tabs = old.tabs;
                }
                if unchanged {
                    continue;
                }
            }
            panes_model.set_row_data(i, r);
        }
    } else {
        panes_model.set_vec(pane_infos);
    }

    let split_infos: Vec<SplitterInfo> = splits
        .iter()
        .map(|s| SplitterInfo {
            split_id: s.split_id as i32,
            x: s.x,
            y: s.y,
            w: s.w,
            h: s.h,
            vertical: s.vertical,
        })
        .collect();
    if splitters_model.row_count() == split_infos.len() {
        for (i, r) in split_infos.into_iter().enumerate() {
            let unchanged = splitters_model.row_data(i).is_some_and(|old| {
                old.split_id == r.split_id
                    && old.x == r.x
                    && old.y == r.y
                    && old.w == r.w
                    && old.h == r.h
                    && old.vertical == r.vertical
            });
            if !unchanged {
                splitters_model.set_row_data(i, r);
            }
        }
    } else {
        splitters_model.set_vec(split_infos);
    }

    if let Some(fp) = panes.iter().find(|p| p.focused) {
        if window.get_active_tab_id().as_str() != fp.active.as_str() {
            window.set_active_tab_id(fp.active.clone().into());
        }
    }
}

// --- Docked-panel edge stacks (#dock-stack) --------------------------------

/// The edge (left|right|top|bottom) a window panel is currently expanded on,
/// `None` when folded, closed, or off (welcome not in sidebar mode).
pub(super) fn panel_edge(window: &AppWindow, kind: &str) -> Option<&'static str> {
    let norm = |edge: &str| -> &'static str {
        match edge {
            "right" => "right",
            "top" => "top",
            "bottom" => "bottom",
            _ => "left",
        }
    };
    match kind {
        "sidebar" => {
            (!window.get_sidebar_collapsed()).then(|| norm(window.get_sidebar_dock().as_str()))
        }
        "welcome" => (window.get_welcome_as_sidebar() && !window.get_welcome_collapsed())
            .then(|| norm(window.get_welcome_sidebar_dock().as_str())),
        "quick" => (window.get_quick_panel_open() && !window.get_quick_panel_collapsed())
            .then(|| norm(window.get_quick_panel_dock().as_str())),
        _ => None,
    }
}

/// A panel's preferred thickness along its edge's normal: width on a
/// left/right edge, height on a top/bottom one (logical px).
pub(super) fn panel_extent(window: &AppWindow, kind: &str) -> f32 {
    let horizontal_edge = matches!(panel_edge(window, kind), Some("left" | "right"));
    match kind {
        "sidebar" if horizontal_edge => window.get_sidebar_width(),
        "sidebar" => window.get_sidebar_height(),
        "welcome" => window.get_welcome_sidebar_width(),
        "quick" if horizontal_edge => window.get_quick_panel_width(),
        "quick" => window.get_quick_panel_height(),
        _ => 220.0,
    }
}

/// Whether the edge shows a 36px collapsed-panel ToolStrip band: any panel
/// whose folded form docks there (welcome counts only in sidebar mode, where
/// its strip actually renders). Multiple folded panels on one edge merge
/// into a single band, hence the boolean.
pub(super) fn strip_on_edge(window: &AppWindow, edge: &str) -> bool {
    let docks = |s: slint::SharedString| s.as_str() == edge;
    (window.get_sidebar_collapsed() && docks(window.get_sidebar_dock()))
        || (window.get_welcome_as_sidebar()
            && window.get_welcome_collapsed()
            && docks(window.get_welcome_sidebar_dock()))
        || (window.get_quick_panel_open()
            && window.get_quick_panel_collapsed()
            && docks(window.get_quick_panel_dock()))
}

/// Rebuild the edge stacks from the current window panel state, recompute the
/// dock geometry and push the result into the UI (models updated in place so
/// the rendered panel components keep their state). Call whenever a panel
/// docks, resizes or collapses — the close path then only has to persist the
/// already-synced stacks.
/// `area` is the dock-area frame in logical px (Slint reports it through
/// `dock-area-resized`) — panels lay out in that frame, not the window's.
///
/// While a panel drag is live the push is deferred: `set_vec` mid-drag can
/// re-key `for` rows and destroy the very component holding the pointer grab,
/// which strands the drag (its end handler never runs). The deferred run
/// retries until the flag drops, so a release commit still lands ~100ms
/// later.
pub(super) fn refresh_dock(
    window: &AppWindow,
    dock_stacks: &Rc<RefCell<DockStacks>>,
    panels_model: &Rc<VecModel<PanelGeomInfo>>,
    dividers_model: &Rc<VecModel<DividerGeomInfo>>,
    area: (f32, f32),
) {
    refresh_dock_inner(
        window.as_weak(),
        dock_stacks.clone(),
        panels_model.clone(),
        dividers_model.clone(),
        area,
    );
}

pub(super) fn refresh_dock_inner(
    window: slint::Weak<AppWindow>,
    dock_stacks: Rc<RefCell<DockStacks>>,
    panels_model: Rc<VecModel<PanelGeomInfo>>,
    dividers_model: Rc<VecModel<DividerGeomInfo>>,
    area: (f32, f32),
) {
    let Some(w) = window.upgrade() else {
        return;
    };
    let (cw, ch) = area;
    // The deferral itself: any refresh while a panel drag holds the pointer
    // is postponed (and re-postponed until the drag ends), so a mid-drag
    // relayout can never rebuild the dragged panel out from under the grab.
    if w.get_panel_dragging() {
        slint::Timer::single_shot(std::time::Duration::from_millis(100), move || {
            refresh_dock_inner(window, dock_stacks, panels_model, dividers_model, area);
        });
        return;
    }
    // Zen mode (and terminal fullscreen, #495) hides every docked panel and
    // the terminal fills the window.
    if panels_hidden(&w) {
        panels_model.set_vec(Vec::new());
        dividers_model.set_vec(Vec::new());
        w.set_dock_central_x(0.0);
        w.set_dock_central_y(0.0);
        w.set_dock_central_w(cw);
        w.set_dock_central_h(ch);
        return;
    }
    let saved = dock_stacks.borrow().clone();
    let geom = {
        let mut cur = dock_stacks.borrow_mut();
        cur.rebuild_from(&saved, &|k| panel_edge(&w, k));
        cur.compute_geom(
            &|k| panel_extent(&w, k),
            &|edge| strip_on_edge(&w, edge),
            cw,
            ch,
        )
    };
    let panels: Vec<PanelGeomInfo> = geom
        .panels
        .iter()
        .map(|p| PanelGeomInfo {
            kind: p.kind.into(),
            edge: p.edge.into(),
            x: p.rect.x,
            y: p.rect.y,
            w: p.rect.w,
            h: p.rect.h,
        })
        .collect();
    // Update the rows in place whenever the list keeps its shape. `set_vec`
    // resets the repeater and rebuilds every panel component, which would
    // destroy the resize handle a drag currently holds (and renumber the items
    // the pointer grab points at, stranding a divider drag) — so a resize would
    // stop following the cursor after its first event. A changing panel count
    // (dock / collapse / zen) is the only thing that needs the reset.
    if panels_model.row_count() == panels.len() {
        for (i, p) in panels.into_iter().enumerate() {
            let unchanged = panels_model.row_data(i).is_some_and(|old| {
                old.kind == p.kind
                    && old.edge == p.edge
                    && old.x == p.x
                    && old.y == p.y
                    && old.w == p.w
                    && old.h == p.h
            });
            if !unchanged {
                panels_model.set_row_data(i, p);
            }
        }
    } else {
        panels_model.set_vec(panels);
    }
    let dividers: Vec<DividerGeomInfo> = geom
        .dividers
        .iter()
        .map(|d| DividerGeomInfo {
            edge: d.edge.into(),
            index: d.index as i32,
            x: d.rect.x,
            y: d.rect.y,
            w: d.rect.w,
            h: d.rect.h,
            vertical: d.vertical,
        })
        .collect();
    if dividers_model.row_count() == dividers.len() {
        for (i, d) in dividers.into_iter().enumerate() {
            let unchanged = dividers_model.row_data(i).is_some_and(|old| {
                old.edge == d.edge
                    && old.index == d.index
                    && old.x == d.x
                    && old.y == d.y
                    && old.w == d.w
                    && old.h == d.h
                    && old.vertical == d.vertical
            });
            if !unchanged {
                dividers_model.set_row_data(i, d);
            }
        }
    } else {
        dividers_model.set_vec(dividers);
    }
    w.set_dock_central_x(geom.central.x);
    w.set_dock_central_y(geom.central.y);
    w.set_dock_central_w(geom.central.w);
    w.set_dock_central_h(geom.central.h);
}

/// Hit-test a drag point (pane-area coords) to a target pane + drop zone, plus
/// the highlight rect the dropped tab would affect. Zone is one of
/// "tabstrip"/"left"/"right"/"up"/"down"/"center"; `None` when the point is
/// outside every pane. The 30% edge bands trigger a split; the tab strip and
/// middle drop into the pane's tab group.
pub(super) fn drag_target(
    layout: &crate::layout::Layout,
    content: (f32, f32),
    x: f32,
    y: f32,
) -> Option<(u64, &'static str, (f32, f32, f32, f32))> {
    const STRIP: f32 = 36.0;
    const EDGE: f32 = 0.30;
    let (cw, ch) = (content.0.max(1.0), content.1.max(1.0));
    let (panes, _) = layout.flatten(0.0, 0.0, cw, ch);
    let p = panes
        .iter()
        .find(|p| x >= p.x && x < p.x + p.w && y >= p.y && y < p.y + p.h)?;
    let body_top = p.y + STRIP;
    if y < body_top {
        let ix = x.clamp(p.x + 3.0, p.x + p.w - 3.0) - 3.0;
        return Some((p.id, "tabstrip", (ix, p.y + 4.0, 6.0, STRIP - 8.0)));
    }
    let bw = p.w.max(1.0);
    let bh = (p.h - STRIP).max(1.0);
    let rx = (x - p.x) / bw;
    let ry = (y - body_top) / bh;
    let (dl, dr, dt, db) = (rx, 1.0 - rx, ry, 1.0 - ry);
    let m = dl.min(dr).min(dt).min(db);
    let (zone, rect) = if m > EDGE {
        ("center", (p.x, p.y, p.w, p.h))
    } else if m == dl {
        ("left", (p.x, p.y, p.w * 0.5, p.h))
    } else if m == dr {
        ("right", (p.x + p.w * 0.5, p.y, p.w * 0.5, p.h))
    } else if m == dt {
        ("up", (p.x, p.y, p.w, p.h * 0.5))
    } else {
        ("down", (p.x, p.y + p.h * 0.5, p.w, p.h * 0.5))
    };
    Some((p.id, zone, rect))
}

pub(super) fn set_terminal_row(
    win: &AppWindow,
    tab_id: &str,
    mutator: impl Fn(&mut TerminalState),
) {
    let terminals = win.get_terminals();
    let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
        return;
    };
    for i in 0..model.row_count() {
        if let Some(mut row) = model.row_data(i) {
            if row.id.as_str() == tab_id {
                mutator(&mut row);
                model.set_row_data(i, row);
                break;
            }
        }
    }
}

/// Per-pane SFTP collapse / size state and per-tab terminal font zoom.
pub(super) fn wire_pane_sftp_and_font_zoom(
    ctx: &WinCtx,
    terminals_model: &Rc<VecModel<TerminalState>>,
) {
    let WinCtx { store, window, .. } = ctx;
    // Per-session SFTP state: collapse + sizes live in each tab's TerminalState so
    // split panes / other tabs each keep their own (resizing/collapsing one no
    // longer bleeds onto the rest) (#v0.5).
    {
        let terminals_model = terminals_model.clone();
        window.on_set_pane_sftp_collapsed(move |tab_id: SharedString, v: bool| {
            update_terminal_row(&terminals_model, &tab_id, |r| r.sftp_collapsed = v);
        });
    }
    {
        // Font zoom shortcuts. Ctrl+=/-/0 zooms one session: a per-tab px
        // override; Ctrl+0 resets that session to the size chosen in
        // Settings. Ctrl+Shift+=/-/0 zooms every session in this window
        // (shared term-font-size, cleared per-tab overrides so it visibly
        // applies to all); Ctrl+Shift+0 resets the window to the Settings
        // size. Zoom never persists globally — the settings stepper owns
        // that. Changing the size re-measures the cell grid and triggers the
        // PTY resize on its own.
        let weak = window.as_weak();
        let store = store.clone();
        let terminals_model = terminals_model.clone();
        window.on_zoom_term_font(
            move |tab_id: SharedString, direction: i32, window_wide: bool| {
                let Some(w) = weak.upgrade() else {
                    return;
                };
                let settings_size = store.borrow().font_size() as i32;
                if window_wide {
                    let next = if direction == 0 {
                        settings_size
                    } else {
                        w.get_term_font_size() as i32 + direction
                    };
                    w.set_term_font_size(next.clamp(8, 32) as f32);
                    // Per-tab overrides would pin sessions at their old size and
                    // defeat "zoom everything", so drop them.
                    use slint::Model as _;
                    for row in terminals_model.iter() {
                        if row.font_size > 0 {
                            let id = row.id.to_string();
                            update_terminal_row(&terminals_model, &id, |r| r.font_size = 0);
                        }
                    }
                    return;
                }
                let tab_id = tab_id.to_string();
                if tab_id.is_empty() || tab_id == "welcome" {
                    return;
                }
                use slint::Model as _;
                let Some(row) = terminals_model.iter().find(|r| r.id.to_string() == tab_id) else {
                    return;
                };
                if direction == 0 {
                    // Back to the Settings size, regardless of the window base.
                    update_terminal_row(&terminals_model, &tab_id, |r| {
                        r.font_size = settings_size.clamp(8, 32)
                    });
                    return;
                }
                let base = if row.font_size > 0 {
                    row.font_size
                } else {
                    w.get_term_font_size() as i32
                };
                let next = (base + direction).clamp(8, 32);
                update_terminal_row(&terminals_model, &tab_id, |r| r.font_size = next);
            },
        );
    }
    {
        let terminals_model = terminals_model.clone();
        let weak = window.as_weak();
        window.on_set_pane_sftp_height(move |tab_id: SharedString, v: f32| {
            update_terminal_row(&terminals_model, &tab_id, |r| r.sftp_panel_height = v);
            // Mirror to the global default so it persists (saved on close) and
            // seeds new sessions; other open tabs use their own field, unaffected.
            if let Some(w) = weak.upgrade() {
                w.set_sftp_panel_height(v);
            }
        });
    }
    {
        let terminals_model = terminals_model.clone();
        let weak = window.as_weak();
        window.on_set_pane_sftp_width(move |tab_id: SharedString, v: f32| {
            update_terminal_row(&terminals_model, &tab_id, |r| r.sftp_panel_width = v);
            if let Some(w) = weak.upgrade() {
                w.set_sftp_panel_width(v);
            }
        });
    }
    {
        let terminals_model = terminals_model.clone();
        window.on_set_pane_sftp_saved_height(move |tab_id: SharedString, v: f32| {
            update_terminal_row(&terminals_model, &tab_id, |r| r.sftp_saved_height = v);
        });
    }
}
