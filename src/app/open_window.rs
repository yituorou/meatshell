//! Building one application window and wiring its UI callbacks.

use super::*;

/// Window activity, for idle-CPU throttling (#127): the winit event hook
/// updates it and the resource sampler / cursor blink read it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WinActivity {
    Active,     // focused & visible → full rate
    Background, // visible but unfocused → throttled
    Hidden,     // minimized / occluded → paused
}

/// Shared per-window state handed to the `wire_*` helpers that
/// `open_window` calls; every field is a cheap handle clone.
pub(super) struct WinCtx {
    pub(super) core: Rc<AppCore>,
    pub(super) store: Rc<RefCell<ConfigStore>>,
    pub(super) registry: Rc<WindowRegistry<slint::Weak<AppWindow>>>,
    pub(super) handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    pub(super) sftp_handles: SftpHandles,
    pub(super) bufs: TermBuffers,
    pub(super) window: Rc<AppWindow>,
    pub(super) proc_win: Rc<ProcWindow>,
    pub(super) sys_win: Rc<SystemInfoWindow>,
    pub(super) editor_win: Rc<EditorWindow>,
    pub(super) window_id: u64,
    pub(super) pending_window_size_restore: Rc<Cell<Option<(f32, f32)>>>,
    pub(super) window_size_tracking_ready: Rc<Cell<bool>>,
}

/// Build and wire one application window. Called for the first window by
/// `run()` and for every subsequent window by the new-window entry points.
/// `at` pins the window to a physical screen position (tab detach); without
/// it the window cascades from the newest one or centers.
/// Returns the registry id used to unregister on close.
pub(super) fn open_window(
    core: Rc<AppCore>,
    cascade: bool,
    at: Option<slint::PhysicalPosition>,
) -> Result<u64> {
    let runtime = core.runtime.clone();
    let store = core.store.clone();
    let registry = core.registry.clone();

    // Per-tab SSH handles (shell only; lives on Slint thread via Rc).
    let handles: Rc<RefCell<HashMap<String, SessionHandle>>> =
        Rc::new(RefCell::new(HashMap::new()));

    // Per-tab SFTP handles — Arc<Mutex> so the event-pump OS thread and the
    // Slint UI thread can both post SftpCommands.
    let sftp_handles: SftpHandles = Arc::new(Mutex::new(HashMap::new()));
    // Per-tab cwd the SFTP panel last followed (see SftpLastCwd).
    let sftp_last_cwd: SftpLastCwd = Arc::new(Mutex::new(HashMap::new()));

    // Per-tab vt100 parsers + history logs (Arc<Mutex> so they can be cloned
    // into the thread that pumps session events into invoke_from_event_loop).
    let bufs: TermBuffers = Arc::new(Mutex::new(HashMap::new()));
    let render_gates: RenderGates = Arc::new(Mutex::new(HashMap::new()));

    // Last-known terminal pixel dimensions, updated by every terminal-resize
    // callback.  Shared so on_connect_session can pass a sensible initial PTY
    // size to spawn_session before the first resize callback fires.
    // Default: 80 cols × 24 rows (SSH spec minimum).
    let last_term_size: Arc<Mutex<(u32, u32)>> = Arc::new(Mutex::new((80, 24)));

    // --- Build window + models ------------------------------------------
    let window = Rc::new(AppWindow::new().context("failed to build Slint window")?);
    // Cascade origin must be captured *before* registering: once registered,
    // registry.newest() is this window itself. Registration itself is deferred
    // until after the last fallible construction below, so a failed monitor
    // window never leaves a stale registry entry.
    let cascade_origin = if cascade { registry.newest() } else { None };
    // Slint applies preferred-width/height while the native window is being
    // created. Do not treat those startup Resized events as user adjustments;
    // otherwise they overwrite the persisted size before restoration (#278).
    let window_size_tracking_ready = Rc::new(Cell::new(false));
    let pending_window_size_restore = Rc::new(Cell::new(None::<(f32, f32)>));

    // Show the crate version (from Cargo.toml at compile time) in the sidebar,
    // so the footer never drifts out of sync with the actual build.
    window.set_app_version(env!("CARGO_PKG_VERSION").into());

    // Set the window icon from the PNG embedded in the binary so the dock
    // shows the correct icon even without a system-installed .desktop entry
    // (e.g. AppImage without AppImageLauncher, or plain binary in ~/bin).
    #[cfg(target_os = "linux")]
    set_window_icon(&window);

    // The window defaults to frameless + custom title bar (#119). macOS keeps
    // its native decorations, so turn the custom bar off there.
    #[cfg(target_os = "macos")]
    window.set_custom_titlebar(false);

    // --- Detachable process monitor window (#23) -----------------------------
    // The process table is its own top-level OS window so it can be dragged
    // outside the main window (or onto a second monitor). Both windows render
    // the *same* VecModel, so the table stays live wherever it's parked; closing
    // it just hides it, so reopening is instant.
    let proc_rows_model: Rc<VecModel<ProcRow>> = Rc::new(VecModel::default());
    window.set_proc_list(ModelRc::from(proc_rows_model.clone()));
    let sys_metrics_model: Rc<VecModel<SysMetricRow>> = Rc::new(VecModel::default());
    let sys_net_rows_model: Rc<VecModel<SysNetRow>> = Rc::new(VecModel::default());
    let sys_disks_model: Rc<VecModel<DiskInfo>> = Rc::new(VecModel::default());
    let sys_overview_model: Rc<VecModel<SysInfoRow>> = Rc::new(VecModel::default());
    let sys_cpu_info_model: Rc<VecModel<SysInfoRow>> = Rc::new(VecModel::default());
    let sys_gpu_info_model: Rc<VecModel<SysInfoRow>> = Rc::new(VecModel::default());
    let sys_cpu_usage_model: Rc<VecModel<SysInfoRow>> = Rc::new(VecModel::default());
    let sys_memory_model: Rc<VecModel<SysInfoRow>> = Rc::new(VecModel::default());
    let sys_swap_model: Rc<VecModel<SysInfoRow>> = Rc::new(VecModel::default());
    let sys_network_model: Rc<VecModel<SysInfoRow>> = Rc::new(VecModel::default());
    let sys_filesystem_model: Rc<VecModel<SysInfoRow>> = Rc::new(VecModel::default());
    window.set_sys_metrics(ModelRc::from(sys_metrics_model.clone()));
    window.set_sys_net_rows(ModelRc::from(sys_net_rows_model.clone()));
    window.set_sys_disks(ModelRc::from(sys_disks_model.clone()));
    window.set_sys_overview_rows(ModelRc::from(sys_overview_model.clone()));
    window.set_sys_cpu_info_rows(ModelRc::from(sys_cpu_info_model.clone()));
    window.set_sys_gpu_info_rows(ModelRc::from(sys_gpu_info_model.clone()));
    window.set_sys_cpu_usage_rows(ModelRc::from(sys_cpu_usage_model.clone()));
    window.set_sys_memory_rows(ModelRc::from(sys_memory_model.clone()));
    window.set_sys_swap_rows(ModelRc::from(sys_swap_model.clone()));
    window.set_sys_network_rows(ModelRc::from(sys_network_model.clone()));
    window.set_sys_filesystem_rows(ModelRc::from(sys_filesystem_model.clone()));
    let proc_win = Rc::new(ProcWindow::new().context("failed to build process window")?);
    proc_win.set_custom_titlebar(cfg!(not(target_os = "macos")));
    proc_win.set_is_mac(cfg!(target_os = "macos"));
    proc_win.set_proc_list(ModelRc::from(proc_rows_model.clone()));
    let sys_win = Rc::new(SystemInfoWindow::new().context("failed to build system info window")?);
    let editor_win = Rc::new(EditorWindow::new().context("failed to build editor window")?);
    editor_win.set_custom_titlebar(cfg!(not(target_os = "macos")));
    editor_win.set_is_mac(cfg!(target_os = "macos"));
    sync_editor_theme(&window, &editor_win);
    // Every fallible construction has now succeeded — register the window.
    // (cascade_origin above was captured before this point, as required.)
    let window_id = registry.register(window.as_weak());
    let ctx = WinCtx {
        core: core.clone(),
        store: store.clone(),
        registry: registry.clone(),
        handles: handles.clone(),
        sftp_handles: sftp_handles.clone(),
        bufs: bufs.clone(),
        window: window.clone(),
        proc_win: proc_win.clone(),
        sys_win: sys_win.clone(),
        editor_win: editor_win.clone(),
        window_id,
        pending_window_size_restore: pending_window_size_restore.clone(),
        window_size_tracking_ready: window_size_tracking_ready.clone(),
    };
    sys_win.set_custom_titlebar(cfg!(not(target_os = "macos")));
    sys_win.set_is_mac(cfg!(target_os = "macos"));
    sys_win.set_metrics(ModelRc::from(sys_metrics_model.clone()));
    sys_win.set_nets(ModelRc::from(sys_net_rows_model.clone()));
    sys_win.set_disks(ModelRc::from(sys_disks_model.clone()));
    sys_win.set_overview_rows(ModelRc::from(sys_overview_model.clone()));
    sys_win.set_cpu_info_rows(ModelRc::from(sys_cpu_info_model.clone()));
    sys_win.set_gpu_info_rows(ModelRc::from(sys_gpu_info_model.clone()));
    sys_win.set_cpu_usage_rows(ModelRc::from(sys_cpu_usage_model.clone()));
    sys_win.set_memory_rows(ModelRc::from(sys_memory_model.clone()));
    sys_win.set_swap_rows(ModelRc::from(sys_swap_model.clone()));
    sys_win.set_network_rows(ModelRc::from(sys_network_model.clone()));
    sys_win.set_filesystem_rows(ModelRc::from(sys_filesystem_model.clone()));
    wire_monitor_windows(&ctx);

    apply_saved_appearance(&ctx);

    // Interface setting: SFTP follows the terminal's cd. The shell event pumps
    // read this AtomicBool on every CwdChanged, so toggling applies live to
    // already-open sessions too.
    let sftp_follow_cd = Arc::new(std::sync::atomic::AtomicBool::new(
        store.borrow().sftp_follow_cd(),
    ));
    wire_interface_toggles(&ctx, &sftp_follow_cd);
    super::shortcuts::wire_shortcuts(&ctx);
    wire_session_log_settings(&ctx);

    // Interface setting: collapse the sidebars by default (#78). Seed the
    wire_layout_prefs(&ctx);

    wire_webdav_upload(&ctx);
    wire_terminal_settings(&ctx);
    wire_ui_scale_and_wallpaper(&ctx);
    super::color_theme::wire(&ctx);

    let sessions_model: Rc<VecModel<SessionInfo>> = Rc::new(VecModel::default());
    window.set_sessions(ModelRc::from(sessions_model.clone()));
    sync_sessions_to_model(&store.borrow(), &sessions_model);
    window.set_wsl_profiles(wsl_profile_model(&store.borrow()));
    window.set_launch_shells(launch_shell_model(&store.borrow()));
    super::agents::wire_agent_launchers(&ctx, &sessions_model);
    listen_for_config_changes(&ctx, &sessions_model);
    wire_wsl_profiles(&ctx, &sessions_model);
    wire_webdav_download(&ctx, &sessions_model);

    let tabs_model: Rc<VecModel<TabInfo>> = Rc::new(VecModel::default());
    tabs_model.push(TabInfo {
        id: "welcome".into(),
        title_len: tab_title_len(&t("新标签页", "New tab")),
        title: t("新标签页", "New tab").into(),
        kind: "welcome".into(),
        connected: false,
        local: false,
    });
    window.set_tabs(ModelRc::from(tabs_model.clone()));
    window.set_active_tab_id("welcome".into());

    let terminals_model: Rc<VecModel<TerminalState>> = Rc::new(VecModel::default());
    window.set_terminals(ModelRc::from(terminals_model.clone()));
    // Files open as editor tabs (#494); see editor_tabs.rs.
    let editor_docs_model: Rc<VecModel<EditorDoc>> = Rc::new(VecModel::default());
    window.set_editor_docs(ModelRc::from(editor_docs_model.clone()));

    // Split-pane layout tree (v0.5). Starts as a single pane owning the welcome
    // tab; tab opens/closes/moves mutate it and re-flatten into the `panes`
    // model. `content_size` is the pane-area px size reported from Slint.
    // In welcome-as-sidebar mode the session list lives in a left panel, so the
    // layout starts empty (no "welcome" tab); otherwise it owns the welcome tab.
    let welcome_sidebar = store.borrow().welcome_as_sidebar();
    let layout: Rc<RefCell<crate::layout::Layout>> = Rc::new(RefCell::new(if welcome_sidebar {
        crate::layout::Layout::new(Vec::new(), String::new())
    } else {
        crate::layout::Layout::new(vec!["welcome".into()], "welcome".into())
    }));
    let content_size: Rc<std::cell::Cell<(f32, f32)>> =
        Rc::new(std::cell::Cell::new((1200.0, 800.0)));

    // Docked-panel edge stacks (#dock-stack): which window panels share an edge
    // at the same time. Restored from config and applied to the panel dock /
    // collapse state below, so a persisted stacked layout survives restart
    // even before the stacked renderer lights up.
    let dock_stacks: Rc<RefCell<DockStacks>> = Rc::new(RefCell::new(DockStacks::default()));
    {
        let saved = store.borrow().dock_stacks();
        {
            let mut ds = dock_stacks.borrow_mut();
            ds.from_saved(&saved);
        }
        for e in &saved {
            for s in &e.slots {
                let edge: slint::SharedString = e.edge.clone().into();
                match s.kind.as_str() {
                    "sidebar" => {
                        window.set_sidebar_dock(edge.clone());
                        window.set_sidebar_collapsed(false);
                    }
                    "welcome" => {
                        window.set_welcome_sidebar_dock(edge.clone());
                        window.set_welcome_collapsed(false);
                    }
                    "quick" => {
                        window.set_quick_panel_dock(edge.clone());
                        window.set_quick_panel_collapsed(false);
                    }
                    _ => {}
                }
            }
        }
    }
    // Persistent pane / splitter models. refresh_panes updates these IN PLACE so
    // the rendered `for pane` / `for sp` elements are reused (terminals survive,
    // and the splitter keeps its pointer-grab during a drag).
    let panes_model: Rc<VecModel<PaneInfo>> = Rc::new(VecModel::default());
    window.set_panes(ModelRc::from(panes_model.clone()));
    let splitters_model: Rc<VecModel<SplitterInfo>> = Rc::new(VecModel::default());
    window.set_splitters(ModelRc::from(splitters_model.clone()));
    // The dock-area frame in logical px (window minus title bar), reported by
    // Slint via dock-area-resized. Rust must lay panels out in THIS frame; the
    // default matches the legacy dock-central-w/h defaults so the very first
    // pre-show pass already yields sane geometry.
    let da_size: Rc<std::cell::Cell<(f32, f32)>> = Rc::new(std::cell::Cell::new((1200.0, 800.0)));
    // Docked-panel stack models (#dock-stack): absolute rects for every
    // expanded panel + the dividers between stacked ones. Kept IN PLACE so the
    // panel components reuse their instances (draft/scroll state survives).
    let dock_panels_model: Rc<VecModel<PanelGeomInfo>> = Rc::new(VecModel::default());
    window.set_dock_panels(ModelRc::from(dock_panels_model.clone()));
    let dock_dividers_model: Rc<VecModel<DividerGeomInfo>> = Rc::new(VecModel::default());
    window.set_dock_dividers(ModelRc::from(dock_dividers_model.clone()));
    // First dock-geometry pass after restore; `dock-layout-changed()` keeps it
    // fresh whenever a panel docks, resizes or collapses.
    refresh_dock(
        &window,
        &dock_stacks,
        &dock_panels_model,
        &dock_dividers_model,
        da_size.get(),
    );
    // Any dock/collapse/size change anywhere wants the geometry recomputed.
    {
        let weak = window.as_weak();
        let ds = dock_stacks.clone();
        let pm = dock_panels_model.clone();
        let dm = dock_dividers_model.clone();
        let da = da_size.clone();
        window.on_dock_layout_changed(move || {
            if let Some(w) = weak.upgrade() {
                refresh_dock(&w, &ds, &pm, &dm, da.get());
            }
        });
        // The dock-area frame itself: window resizes change it without any
        // panel event, and `rest` no longer tracks parent sizes, so this is
        // the trigger that keeps the whole layout following the window. (Zen
        // toggles come through `dock-layout-changed` instead — the frame does
        // not resize there.)
        let weak4 = window.as_weak();
        let ds4 = dock_stacks.clone();
        let pm4 = dock_panels_model.clone();
        let dm4 = dock_dividers_model.clone();
        let da4 = da_size.clone();
        window.on_dock_area_resized(move |w: f32, h: f32| {
            let next = (w.max(1.0), h.max(1.0));
            if da4.get() == next {
                return;
            }
            da4.set(next);
            if let Some(win) = weak4.upgrade() {
                refresh_dock(&win, &ds4, &pm4, &dm4, next);
            }
        });
        // Dragging a divider between two stacked panels updates their ratio.
        let weak2 = window.as_weak();
        let ds2 = dock_stacks.clone();
        let pm2 = dock_panels_model.clone();
        let dm2 = dock_dividers_model.clone();
        let da2 = da_size.clone();
        window.on_stack_split_drag(move |edge: SharedString, index: i32, pos: f32| {
            let edge = edge.to_string();
            // Slint reports divider drags in dock-area coordinates, so the
            // axis must be the dock-area extent, not the window's.
            let (w, h) = da2.get();
            let axis = match edge.as_str() {
                "left" | "right" => h,
                _ => w,
            };
            let ratio = if axis > 0.0 {
                (pos / axis).clamp(0.02, 0.98)
            } else {
                0.5
            };
            {
                let mut lay = ds2.borrow_mut();
                lay.set_ratio(&edge, index as usize, ratio);
            }
            if let Some(w) = weak2.upgrade() {
                refresh_dock(&w, &ds2, &pm2, &dm2, da2.get());
            }
        });
        // Dragging a stacked panel's in-edge handle resizes it along the dock
        // normal and re-runs the geometry pass.
        let weak3 = window.as_weak();
        let ds3 = dock_stacks.clone();
        let pm3 = dock_panels_model.clone();
        let dm3 = dock_dividers_model.clone();
        let da3 = da_size.clone();
        let panels_model_for_extent = dock_panels_model.clone();
        window.on_panel_extent_drag(move |_panel_index: i32, pos: f32| {
            let panel = panels_model_for_extent.row_data(_panel_index as usize);
            if let Some(p) = panel {
                let kind = p.kind.to_string();
                let edge = p.edge.to_string();
                let horizontal_edge = matches!(edge.as_str(), "left" | "right");
                let thickness = pos.clamp(MIN_THICK, 2600.0);
                if let Some(w) = weak3.upgrade() {
                    match (kind.as_str(), horizontal_edge) {
                        ("sidebar", true) => w.set_sidebar_width(thickness),
                        ("sidebar", false) => w.set_sidebar_height(thickness),
                        ("welcome", _) => w.set_welcome_sidebar_width(thickness),
                        ("quick", true) => w.set_quick_panel_width(thickness),
                        ("quick", false) => w.set_quick_panel_height(thickness),
                        _ => {}
                    }
                    refresh_dock(&w, &ds3, &pm3, &dm3, da3.get());
                }
            }
        });
    }
    // Snapshot the layout and drop the RefCell guard before mutating Slint
    // models: a model change can synchronously run binding callbacks, and one
    // of those re-entering `layout.borrow*()` while this shared guard is still
    // alive would panic (RefCell already borrowed) → abort in release.
    let lay = (*layout.borrow()).clone();
    refresh_panes(
        &window,
        &lay,
        content_size.get(),
        &tabs_model,
        &panes_model,
        &splitters_model,
    );
    {
        let weak = window.as_weak();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        let cr_dock = dock_stacks.clone();
        let cr_pm = dock_panels_model.clone();
        let cr_dm = dock_dividers_model.clone();
        let cr_da = da_size.clone();
        window.on_content_resized(move |w: f32, h: f32| {
            let next = (w.max(1.0), h.max(1.0));
            if content_size.get() == next {
                return;
            }
            content_size.set(next);
            if let Some(win) = weak.upgrade() {
                let lay = (*layout.borrow()).clone();
                refresh_panes(
                    &win,
                    &lay,
                    content_size.get(),
                    &tabs_model,
                    &panes_model,
                    &splitters_model,
                );
                refresh_dock(&win, &cr_dock, &cr_pm, &cr_dm, cr_da.get());
            }
        });
    }
    // Toggle welcome-as-sidebar at runtime: persist, then move the welcome tab in
    // or out of the split-tree (sidebar mode = no welcome tab) and re-flatten.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model.clone();
        let splitters_model = splitters_model.clone();
        let wds_dock = dock_stacks.clone();
        let wds_pm = dock_panels_model.clone();
        let wds_dm = dock_dividers_model.clone();
        let wds_da = da_size.clone();
        window.on_set_welcome_as_sidebar(move |v| {
            // The property is two-way-bound through InterfacePanel and changing
            // it destroys/recreates the Welcome subtree that owns the Switch.
            // Persist first: saving config does not touch the Slint tree, and
            // doing it synchronously means an immediate window close cannot
            // lose the preference. Only the property/layout transition needs
            // to wait until this Switch callback has returned (#323).
            {
                let mut s = store.borrow_mut();
                s.set_welcome_as_sidebar(v);
                if let Err(error) = s.save() {
                    tracing::warn!("failed to persist welcome sidebar preference: {error:#}");
                }
            }
            let weak = weak.clone();
            let layout = layout.clone();
            let content_size = content_size.clone();
            let tabs_model = tabs_model.clone();
            let panes_model = panes_model.clone();
            let splitters_model = splitters_model.clone();
            let wds_dock = wds_dock.clone();
            let wds_pm = wds_pm.clone();
            let wds_dm = wds_dm.clone();
            let wds_da = wds_da.clone();
            slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                if let Some(w) = weak.upgrade() {
                    w.set_welcome_as_sidebar(v);
                    {
                        let mut lay = layout.borrow_mut();
                        update_welcome_tab(&mut lay, v);
                    }
                    refresh_panes(
                        &w,
                        &layout.borrow(),
                        content_size.get(),
                        &tabs_model,
                        &panes_model,
                        &splitters_model,
                    );
                    refresh_dock(&w, &wds_dock, &wds_pm, &wds_dm, wds_da.get());
                }
            });
        });
    }
    wire_pane_sftp_and_font_zoom(&ctx, &terminals_model);

    // Per-tab connection status + remote resources, the latest local sample,
    // and the local machine's network history (bottom sparkline).
    let tab_statuses: TabStatuses = Arc::new(Mutex::new(HashMap::new()));
    let local_snap: LocalSnap = Arc::new(Mutex::new(SystemSnapshot::default()));
    let local_net_hist: NetHist = Arc::new(Mutex::new(vec![0.0; NET_HISTORY_LEN]));

    // Per-tab display-name overrides set via "Rename session" (tab context
    // menu). Display only — the saved session keeps its own name.
    let tab_titles: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));

    // Repeated timers created below (system sampler). Parked in WindowState
    // so closing the window stops them instead of leaking.
    let window_timers: Rc<RefCell<Vec<slint::Timer>>> = Rc::new(RefCell::new(Vec::new()));

    // Expose this window's state to the rest of the process so tabs can be
    // dragged out into a new window or merged into another one (#tab-detach).
    core.window_states.borrow_mut().insert(
        window_id,
        WindowState {
            main_win: window.clone(),
            weak: window.as_weak(),
            handles: handles.clone(),
            bufs: bufs.clone(),
            gates: render_gates.clone(),
            statuses: tab_statuses.clone(),
            sftp_handles: sftp_handles.clone(),
            sftp_last_cwd: sftp_last_cwd.clone(),
            local_snap: local_snap.clone(),
            net_hist: local_net_hist.clone(),
            follow_cd: sftp_follow_cd.clone(),
            layout: layout.clone(),
            dock_stacks: dock_stacks.clone(),
            tabs_model: tabs_model.clone(),
            terminals_model: terminals_model.clone(),
            panes_model: panes_model.clone(),
            splitters_model: splitters_model.clone(),
            timers: window_timers.clone(),
            content_size: content_size.clone(),
            proc_win: proc_win.clone(),
            sys_win: sys_win.clone(),
            editor_win: editor_win.clone(),
            proc_weak: proc_win.as_weak(),
            sys_weak: sys_win.as_weak(),
        },
    );

    wire_editor_window_chrome(&ctx);
    wire_pin_on_top(&ctx);
    {
        let proc_weak = proc_win.as_weak();
        let handles = handles.clone();
        let statuses = tab_statuses.clone();
        let runtime = runtime.clone();
        proc_win.on_terminate_process(
            move |tab_id: SharedString, pid: SharedString, password: SharedString| {
                let tab_id = tab_id.to_string();
                let Ok(pid) = pid.parse::<u32>() else {
                    set_process_action_error(&proc_weak, t("无效的 PID", "Invalid PID"));
                    return;
                };

                // Re-check the source tab, PID, and owner against the latest sample;
                // the main window may have switched tabs since the menu was opened.
                let ownership = {
                    let states = statuses.lock().unwrap();
                    states.get(&tab_id).map_or_else(
                        || Err(t("当前会话不可用", "The current session is unavailable")),
                        |status| {
                            status
                                .procs
                                .iter()
                                .find(|p| p.pid == pid)
                                .map(|process| process_needs_root(&status.user, &process.user))
                                .ok_or_else(|| t("进程已退出", "The process has already exited"))
                        },
                    )
                };
                let needs_root = match ownership {
                    Ok(value) => value,
                    Err(message) => {
                        set_process_action_error(&proc_weak, message);
                        return;
                    }
                };
                if needs_root && password.is_empty() {
                    set_process_action_error(
                        &proc_weak,
                        t(
                            "请输入管理员（sudo）密码",
                            "Enter the administrator (sudo) password",
                        ),
                    );
                    return;
                }

                let root_password =
                    needs_root.then(|| crate::config::Secret::new(password.to_string()));
                let response = handles
                    .borrow()
                    .get(&tab_id)
                    .map(|handle| handle.kill_process(pid, root_password));
                let Some(response) = response else {
                    set_process_action_error(
                        &proc_weak,
                        t("SSH 会话不可用", "The SSH session is unavailable"),
                    );
                    return;
                };

                let done_weak = proc_weak.clone();
                runtime.spawn(async move {
                    let result = response
                        .await
                        .unwrap_or_else(|_| crate::ssh::ProcessKillResult {
                            success: false,
                            message: t("SSH 会话已关闭", "The SSH session has closed").to_string(),
                        });
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(pw) = done_weak.upgrade() {
                            pw.set_action_busy(false);
                            pw.set_action_error(!result.success);
                            pw.set_action_status(result.message.into());
                        }
                    });
                });
            },
        );
    }

    // --- Wire callbacks --------------------------------------------------
    wire_session_callbacks(
        &window,
        window_id,
        store.clone(),
        registry.clone(),
        sessions_model.clone(),
        tabs_model.clone(),
        terminals_model.clone(),
        layout.clone(),
        content_size.clone(),
        panes_model.clone(),
        splitters_model.clone(),
        handles.clone(),
        bufs.clone(),
        render_gates.clone(),
        runtime.clone(),
        last_term_size.clone(),
        sftp_handles.clone(),
        sftp_last_cwd.clone(),
        tab_statuses.clone(),
        local_snap.clone(),
        local_net_hist.clone(),
        sftp_follow_cd.clone(),
        core.tab_routes.clone(),
        tab_titles.clone(),
        editor_win.clone(),
    );
    super::save_tab_session::wire(
        &window,
        store.clone(),
        registry.clone(),
        sessions_model.clone(),
    );
    super::tab_session_log::wire(&window, store.clone(), bufs.clone());

    wire_sidebar_refresh_and_theme(
        &ctx,
        &tabs_model,
        &tab_statuses,
        &local_snap,
        &local_net_hist,
    );

    wire_auth_prompts(&ctx);

    wire_downloads_and_links(&ctx, &tab_statuses, &local_snap, &local_net_hist);
    super::update::wire(&core, &window);

    // Transfer records (download/upload progress + history) shown in the popup.
    let transfers_model: Rc<VecModel<TransferInfo>> = Rc::new(VecModel::default());
    window.set_transfers(ModelRc::from(transfers_model.clone()));
    {
        let tm = transfers_model.clone();
        window.on_clear_transfers(move || tm.set_vec(Vec::<TransferInfo>::new()));
    }
    {
        // Cancel a transfer by id. The id is a UUID unique across sessions, so we
        // broadcast to every SFTP handle — only the owning one has it registered
        // and will act on it (#100).
        let sftp_handles = sftp_handles.clone();
        window.on_cancel_transfer(move |id: SharedString| {
            if let Ok(handles) = sftp_handles.lock() {
                for h in handles.values() {
                    h.cancel_transfer(id.to_string());
                }
            }
        });
    }

    set_about_libraries(&ctx);

    // New-window entry points: the TabBar button and the Ctrl+Shift+N /
    // ⌘⇧N shortcut both route here. Runs on the UI thread (Slint callback),
    // so open_window can build the window directly. A failed open must not
    // be silent — the click/keystroke already consumed (#multi-window).
    {
        let core = core.clone();
        window.on_new_window_clicked(move || {
            if let Err(e) = open_window(core.clone(), true, None) {
                tracing::warn!("failed to open new window: {e:#}");
            }
        });
    }

    wire_tab_callbacks(
        &window,
        window_id,
        core.clone(),
        tabs_model.clone(),
        terminals_model.clone(),
        layout.clone(),
        content_size.clone(),
        panes_model.clone(),
        splitters_model.clone(),
        handles.clone(),
        bufs.clone(),
        render_gates.clone(),
        sftp_handles.clone(),
        sftp_last_cwd.clone(),
        tab_titles.clone(),
    );
    wire_sftp_callbacks(
        &window,
        &editor_win,
        sftp_handles.clone(),
        sftp_last_cwd.clone(),
    );
    super::sftp_places::wire_sftp_places(&window, store.clone(), tab_statuses.clone());
    wire_editor_tabs(
        &window,
        EditorTabModels {
            docs: editor_docs_model.clone(),
            tabs: tabs_model.clone(),
            layout: layout.clone(),
            content_size: content_size.clone(),
            panes: panes_model.clone(),
            splitters: splitters_model.clone(),
        },
        sftp_handles.clone(),
    );
    wire_key_input(
        &window,
        handles.clone(),
        bufs.clone(),
        last_term_size.clone(),
        store.clone(),
        ConnectCtx {
            weak: window.as_weak(),
            editor: editor_win.as_weak(),
            window_id,
            runtime: runtime.clone(),
            handles: handles.clone(),
            sftp_handles: sftp_handles.clone(),
            sftp_last_cwd: sftp_last_cwd.clone(),
            bufs: bufs.clone(),
            render_gates: render_gates.clone(),
            tab_statuses: tab_statuses.clone(),
            local_snap: local_snap.clone(),
            local_net_hist: local_net_hist.clone(),
            last_term_size: last_term_size.clone(),
            sftp_follow_cd: sftp_follow_cd.clone(),
            store: store.clone(),
            tab_routes: core.tab_routes.clone(),
        },
    );

    // --- Window activity, for idle-CPU throttling (#127) ----------------
    // Idle terminals shouldn't burn CPU: pause the sampler when the window is
    // minimized / occluded, throttle it when it's merely unfocused, and stop the
    // cursor blink whenever the window isn't focused (mirrors what Tabby / Windows
    // Terminal do). The winit event handler below updates this; the blink reads
    // Theme.window-focused.
    let activity = Rc::new(std::cell::Cell::new(WinActivity::Active));
    // Once the user confirms shutdown, every subsequent native/custom close
    // request must pass through without reopening the modal. Windows Installer
    // and Restart Manager may issue more than one close request while replacing
    // the executable (#267).
    let exit_confirmed = Rc::new(Cell::new(false));

    // --- System sampler (1 Hz) ------------------------------------------
    let sampler = Rc::new(Mutex::new(SystemSampler::new()));
    let weak = window.as_weak();
    let tick_sampler = sampler.clone();
    let tick_statuses = tab_statuses.clone();
    let tick_local = local_snap.clone();
    let tick_net = local_net_hist.clone();
    let tick_activity = activity.clone();
    let mut bg_tick = 0u32;
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        SystemSampler::recommended_interval(),
        move || {
            let Some(window) = weak.upgrade() else { return };
            if window.get_sidebar_collapsed() || window.get_zen_mode() {
                return;
            }
            // Skip the (non-trivial) sysinfo refresh + sidebar repaint when no one
            // is looking, and back off to ~5 s when the window is in the background.
            match tick_activity.get() {
                WinActivity::Hidden => return,
                WinActivity::Background => {
                    bg_tick = bg_tick.wrapping_add(1);
                    if bg_tick % 5 != 0 {
                        return;
                    }
                }
                WinActivity::Active => {}
            }
            let snap = {
                let mut s = tick_sampler.lock().expect("sampler poisoned");
                s.sample()
            };
            // Append the raw local throughput to the bottom-graph ring buffer
            // (normalisation happens at display time so the graph auto-scales).
            push_ring(&mut tick_net.lock().unwrap(), snap.net_bytes_per_sec as f32);
            // Stash the local sample; the sidebar shows it on the welcome tab
            // and in the bottom network graph.
            *tick_local.lock().unwrap() = snap.clone();

            // Everything (status, CPU/mem/swap, both graphs) follows the
            // active tab; refresh_sidebar reads the stores we just updated.
            if sidebar_updates_visible(&window) {
                refresh_sidebar(&window, &tick_statuses, &tick_local, &tick_net);
            }
        },
    );
    // Keep the timer alive as long as this window exists: it lives in the
    // WindowState timers vec, which forget_window_state drops on close
    // (Slint timers stop when dropped) — no leaking needed.
    window_timers.borrow_mut().push(timer);

    // OS file drag-and-drop → upload to the active session's SFTP directory,
    install_window_event_hook(&ctx, &dock_stacks, &activity, &exit_confirmed);
    wire_window_controls(&ctx, &dock_stacks, &exit_confirmed);

    // Position once shown (size is only known after the first frame). The
    // first window centers on the primary monitor; later windows cascade
    // ~40 px from the newest existing window instead of stacking exactly on
    // top of it. The origin was captured above, before this window was
    // registered (registry.newest() would return this window itself).
    {
        let weak = window.as_weak();
        let origin = cascade_origin
            .and_then(|w| w.upgrade())
            .map(|w| w.window().position());
        slint::Timer::single_shot(std::time::Duration::from_millis(30), move || {
            let Some(w) = weak.upgrade() else { return };
            if let Some(pos) = at {
                // Tab detach pinned the window under the cursor (#tab-detach).
                w.window().set_position(pos);
                return;
            }
            match origin {
                Some(pos) => {
                    w.window()
                        .set_position(slint::PhysicalPosition::new(pos.x + 40, pos.y + 40));
                }
                None => center_window(&w),
            }
        });
    }

    // The old entry point was window.run(), which shows the window before
    // spinning the loop. run_event_loop() does not, so display it here —
    // without this the app starts but no window ever appears (#multi-window).
    window.show().context("failed to show window")?;

    Ok(window_id)
}
