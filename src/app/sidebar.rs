use super::*;

fn dynamic_sidebar_visible(active: bool, collapsed: bool) -> bool {
    active && !collapsed
}

pub(super) fn sidebar_updates_visible(win: &AppWindow) -> bool {
    let collapsed = win.get_effective_sidebar_collapsed() && win.get_terminals().row_count() == 0;
    dynamic_sidebar_visible(win.get_dynamic_ui_active(), collapsed)
}

pub(super) fn refresh_process_model(win: &AppWindow, statuses: &TabStatuses) {
    // The detached process window can be focused while the main window is not,
    // so its own open state—not main-window activity—controls live updates.
    if !win.get_process_window_open() {
        return;
    }
    let active = win.get_active_tab_id().to_string();
    let rows = statuses
        .lock()
        .unwrap()
        .get(&active)
        .filter(|status| status.state == 1)
        .map(|status| proc_rows(&status.procs, &status.user, &active))
        .unwrap_or_default();
    if let Some(model) = win
        .get_proc_list()
        .as_any()
        .downcast_ref::<VecModel<ProcRow>>()
    {
        model.set_vec(rows);
    }
}

#[cfg(test)]
mod activity_tests {
    use super::dynamic_sidebar_visible;

    #[test]
    fn dynamic_sidebar_updates_only_while_active_and_expanded() {
        assert!(dynamic_sidebar_visible(true, false));
        assert!(!dynamic_sidebar_visible(false, false));
        assert!(!dynamic_sidebar_visible(true, true));
        assert!(!dynamic_sidebar_visible(false, true));
    }
}

pub(super) fn refresh_sidebar(
    win: &AppWindow,
    statuses: &TabStatuses,
    local: &LocalSnap,
    local_net_hist: &NetHist,
) {
    let pct = |used: u64, total: u64| -> f32 {
        if total > 0 {
            used as f32 / total as f32
        } else {
            0.0
        }
    };
    let snap = local.lock().unwrap().clone();
    let sidebar_expanded = !win.get_effective_sidebar_collapsed();

    // --- Bottom network graph: always the local machine --------------------
    if sidebar_expanded {
        win.set_net_bot_up(format_bytes_per_sec(snap.net_tx_per_sec).into());
        win.set_net_bot_down(format_bytes_per_sec(snap.net_rx_per_sec).into());
        win.set_net_bot_history(normalized_model(&local_net_hist.lock().unwrap()));
    }

    let set_top_local = |win: &AppWindow| {
        if !sidebar_expanded {
            return;
        }
        win.set_net_top_up(format_bytes_per_sec(snap.net_tx_per_sec).into());
        win.set_net_top_down(format_bytes_per_sec(snap.net_rx_per_sec).into());
        win.set_net_top_history(normalized_model(&local_net_hist.lock().unwrap()));
        win.set_net_show_selector(false);
        win.set_net_selected("".into());
        win.set_net_ifaces(ModelRc::from(Rc::new(VecModel::<SharedString>::default())));
        // Non-connected tabs show the local machine's filesystems.
        win.set_disks(disk_model(&snap.disks));
    };
    let show_local_res = |win: &AppWindow| {
        win.set_resource_title(t("本机资源", "Local resources").into());
        win.set_cpu_percent(snap.cpu_percent);
        win.set_mem_percent(snap.mem_percent);
        win.set_swap_percent(snap.swap_percent);
        if sidebar_expanded {
            win.set_mem_detail(format_mem(snap.mem_used_mib, snap.mem_total_mib).into());
            win.set_swap_detail(format_mem(snap.swap_used_mib, snap.swap_total_mib).into());
        }
    };
    let clear_stats = |win: &AppWindow| {
        win.set_cpu_percent(0.0);
        win.set_mem_percent(0.0);
        win.set_swap_percent(0.0);
        win.set_mem_detail("".into());
        win.set_swap_detail("".into());
    };
    let clear_remote_top = |win: &AppWindow| {
        if !sidebar_expanded {
            return;
        }
        win.set_net_top_up("—".into());
        win.set_net_top_down("—".into());
        win.set_net_top_history(normalized_model(&vec![0.0; NET_HISTORY_LEN]));
        win.set_net_show_selector(false);
        win.set_net_selected("".into());
        win.set_net_ifaces(ModelRc::from(Rc::new(VecModel::<SharedString>::default())));
        win.set_disks(ModelRc::from(Rc::new(VecModel::<DiskInfo>::default())));
    };

    // Process monitor (#23) lives in a shared model (the AppWindow and the
    // detachable ProcWindow point at the same VecModel), so mutate it in place
    // instead of replacing it — replacing would break the sharing. Only a live
    // remote session has process data; default to empty and let the connected
    // branch below fill it in.
    let set_procs = |win: &AppWindow, procs: &[ProcInfo], current_user: &str, tab_id: &str| {
        if !win.get_process_window_open() {
            return;
        }
        if let Some(vm) = win
            .get_proc_list()
            .as_any()
            .downcast_ref::<VecModel<ProcRow>>()
        {
            vm.set_vec(proc_rows(procs, current_user, tab_id));
        }
    };
    let set_system_models = |win: &AppWindow,
                             cpu: f32,
                             mem: f32,
                             swap: f32,
                             mem_detail: SharedString,
                             swap_detail: SharedString,
                             nets: Vec<SysNetRow>,
                             disks: Vec<DiskInfo>,
                             sys: SystemDetails| {
        if !win.get_system_info_window_open() {
            return;
        }
        if let Some(vm) = win
            .get_sys_metrics()
            .as_any()
            .downcast_ref::<VecModel<SysMetricRow>>()
        {
            vm.set_vec(metric_rows(cpu, mem, swap, mem_detail, swap_detail));
        }
        if let Some(vm) = win
            .get_sys_net_rows()
            .as_any()
            .downcast_ref::<VecModel<SysNetRow>>()
        {
            vm.set_vec(nets);
        }
        if let Some(vm) = win
            .get_sys_disks()
            .as_any()
            .downcast_ref::<VecModel<DiskInfo>>()
        {
            vm.set_vec(disks);
        }
        if let Some(vm) = win
            .get_sys_overview_rows()
            .as_any()
            .downcast_ref::<VecModel<SysInfoRow>>()
        {
            vm.set_vec(pairs_to_overview_rows(&sys.overview));
        }
        if let Some(vm) = win
            .get_sys_cpu_info_rows()
            .as_any()
            .downcast_ref::<VecModel<SysInfoRow>>()
        {
            vm.set_vec(pairs_to_one_row(&sys.cpu_info));
        }
        if let Some(vm) = win
            .get_sys_gpu_info_rows()
            .as_any()
            .downcast_ref::<VecModel<SysInfoRow>>()
        {
            vm.set_vec(pairs_to_rows(&sys.gpu_info, 4));
        }
        if let Some(vm) = win
            .get_sys_cpu_usage_rows()
            .as_any()
            .downcast_ref::<VecModel<SysInfoRow>>()
        {
            vm.set_vec(cpu_usage_detail_rows(&sys.cpu_usage));
        }
        if let Some(vm) = win
            .get_sys_memory_rows()
            .as_any()
            .downcast_ref::<VecModel<SysInfoRow>>()
        {
            vm.set_vec(pairs_to_one_row(&sys.memory));
        }
        if let Some(vm) = win
            .get_sys_swap_rows()
            .as_any()
            .downcast_ref::<VecModel<SysInfoRow>>()
        {
            vm.set_vec(pairs_to_one_row(&sys.swap));
        }
        if let Some(vm) = win
            .get_sys_network_rows()
            .as_any()
            .downcast_ref::<VecModel<SysInfoRow>>()
        {
            vm.set_vec(tuple5_rows(&sys.networks));
        }
        if let Some(vm) = win
            .get_sys_filesystem_rows()
            .as_any()
            .downcast_ref::<VecModel<SysInfoRow>>()
        {
            vm.set_vec(tuple5_rows(&sys.filesystems));
        }
    };
    let show_local_system_models = |win: &AppWindow| {
        if !win.get_system_info_window_open() {
            return;
        }
        set_system_models(
            win,
            snap.cpu_percent,
            snap.mem_percent,
            snap.swap_percent,
            format_mem(snap.mem_used_mib, snap.mem_total_mib).into(),
            format_mem(snap.swap_used_mib, snap.swap_total_mib).into(),
            vec![SysNetRow {
                name: t("本机", "Local").into(),
                up: format_bytes_per_sec(snap.net_tx_per_sec).into(),
                down: format_bytes_per_sec(snap.net_rx_per_sec).into(),
            }],
            Vec::new(),
            SystemDetails::default(),
        );
    };
    win.set_proc_available(false);
    win.set_system_info_available(false);
    set_procs(win, &[], "", "");

    let active = win.get_active_tab_id().to_string();
    let status = if active == "welcome" {
        None
    } else {
        statuses.lock().unwrap().get(&active).cloned()
    };

    match status {
        // Local shell tabs: keep the connection status line, but show the local
        // machine's resources (their TabStatus carries no remote CPU/mem stats).
        Some(st) if st.is_local => {
            win.set_is_local_mode(true);
            win.set_metrics_available(true);
            win.set_status_heading(t("本机概览", "Local overview").into());
            win.set_conn_state(if st.state == 1 {
                1
            } else if st.state == 2 {
                2
            } else {
                0
            });
            win.set_connection_state(if st.state == 1 {
                format!("{} · {}", t("本机终端", "Local shell"), st.host)
            } else if st.state == 2 {
                format!("{} {}", st.host, t("已断开", "disconnected"))
            } else {
                format!("{} {}", t("连接中", "Connecting"), st.host)
            }
            .into());
            win.set_conn_host(conn_ip(&st.host).into());
            show_local_res(win);
            set_top_local(win);
            show_local_system_models(win);
        }
        // A live remote session tab → remote resources + remote NIC on top.
        Some(st) if st.state == 1 => {
            win.set_is_local_mode(false);
            win.set_metrics_available(true);
            win.set_status_heading(t("服务器资源", "Server resources").into());
            win.set_conn_state(1);
            win.set_connection_state(st.host.clone().into());
            win.set_conn_host(conn_ip(&st.host).into());
            win.set_resource_title(t("服务器资源", "Server resources").into());
            win.set_cpu_percent(st.cpu);
            win.set_mem_percent(pct(st.mem_used_kib, st.mem_total_kib));
            win.set_swap_percent(pct(st.swap_used_kib, st.swap_total_kib));
            if sidebar_expanded {
                win.set_mem_detail(
                    format_mem(st.mem_used_kib / 1024, st.mem_total_kib / 1024).into(),
                );
                win.set_swap_detail(
                    format_mem(st.swap_used_kib / 1024, st.swap_total_kib / 1024).into(),
                );
                let (name, rx, tx) = selected_iface(&st);
                win.set_net_top_up(format_bytes_per_sec(tx).into());
                win.set_net_top_down(format_bytes_per_sec(rx).into());
                win.set_net_top_history(normalized_model(&st.net_hist));
                win.set_net_show_selector(!st.net.is_empty());
                win.set_net_selected(name.into());
                let ifaces: Vec<SharedString> = st.net.iter().map(|e| e.0.clone().into()).collect();
                win.set_net_ifaces(ModelRc::from(Rc::new(VecModel::from(ifaces))));
                win.set_disks(disk_model(&st.disks));
            }
            win.set_proc_available(true);
            win.set_system_info_available(true);
            set_procs(win, &st.procs, &st.user, &active);
            if win.get_system_info_window_open() {
                set_system_models(
                    win,
                    st.cpu,
                    pct(st.mem_used_kib, st.mem_total_kib),
                    pct(st.swap_used_kib, st.swap_total_kib),
                    format_mem(st.mem_used_kib / 1024, st.mem_total_kib / 1024).into(),
                    format_mem(st.swap_used_kib / 1024, st.swap_total_kib / 1024).into(),
                    net_rows(&st.net),
                    disk_rows(&st.disks),
                    st.sys.clone(),
                );
            }
        }
        // Disconnected / timed-out session: never fall back to local disks/network
        // pretending to be server metrics.
        Some(st) if st.state == 2 => {
            win.set_is_local_mode(false);
            win.set_metrics_available(false);
            win.set_status_heading(t("连接已断开", "Disconnected").into());
            win.set_conn_state(2);
            win.set_connection_state(
                format!("{} · {}", st.host, t("重连后恢复采集", "Reconnect to resume metrics")).into(),
            );
            win.set_conn_host(conn_ip(&st.host).into());
            win.set_resource_title(t("服务器资源", "Server resources").into());
            clear_stats(win);
            clear_remote_top(win);
            set_system_models(
                win,
                0.0,
                0.0,
                0.0,
                "".into(),
                "".into(),
                Vec::new(),
                Vec::new(),
                SystemDetails::default(),
            );
        }
        // Still connecting: never fall back to local disks/network pretending to be server metrics.
        Some(st) => {
            win.set_is_local_mode(false);
            win.set_metrics_available(false);
            win.set_status_heading(t("正在连接", "Connecting").into());
            win.set_conn_state(0);
            win.set_connection_state(format!("{} {}", t("连接中", "Connecting"), st.host).into());
            win.set_conn_host(conn_ip(&st.host).into());
            win.set_resource_title(t("服务器资源", "Server resources").into());
            clear_stats(win);
            clear_remote_top(win);
            set_system_models(
                win,
                0.0,
                0.0,
                0.0,
                "".into(),
                "".into(),
                Vec::new(),
                Vec::new(),
                SystemDetails::default(),
            );
        }
        // Welcome / startup screen (no active session) → Local overview (never "Not connected").
        None => {
            win.set_is_local_mode(true);
            win.set_metrics_available(true);
            win.set_status_heading(t("本机概览", "Local overview").into());
            win.set_conn_state(1);
            win.set_connection_state(t("本机系统资源实时概览", "Real-time local system resources").into());
            win.set_conn_host("".into());
            show_local_res(win);
            set_top_local(win);
            show_local_system_models(win);
        }
    }
}

/// Sidebar refresh on tab change, language switch and theme toggle.
pub(super) fn wire_sidebar_refresh_and_theme(
    ctx: &WinCtx,
    tabs_model: &Rc<VecModel<TabInfo>>,
    tab_statuses: &TabStatuses,
    local_snap: &LocalSnap,
    local_net_hist: &NetHist,
) {
    let WinCtx {
        store,
        registry,
        bufs,
        window,
        proc_win,
        editor_win,
        ..
    } = ctx;
    // Recompute the sidebar whenever the active tab changes (fired from Slint's
    // `changed active-tab-id`).
    {
        let weak = window.as_weak();
        let statuses = tab_statuses.clone();
        let local = local_snap.clone();
        let net = local_net_hist.clone();
        window.on_refresh_sidebar(move || {
            if let Some(w) = weak.upgrade() {
                refresh_sidebar(&w, &statuses, &local, &net);
            }
        });
    }

    // Switch UI language at runtime.  Static `@tr(...)` text updates live via
    // select_bundled_translation; we additionally refresh the Rust-driven
    // dynamic strings (sidebar status + the welcome tab title).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let tabs_model = tabs_model.clone();
        let registry = registry.clone();
        window.on_set_language(move |code| {
            crate::i18n::set_language(&code.to_string());
            {
                let mut s = store.borrow_mut();
                s.set_language(crate::i18n::current_code().to_string());
                let _ = s.save();
            }
            registry.broadcast_config_changed();
            // Re-translate the welcome tab's dynamic title.
            for i in 0..tabs_model.row_count() {
                if let Some(mut row) = tabs_model.row_data(i) {
                    if row.id.as_str() == "welcome" {
                        row.title_len = tab_title_len(&t("新标签页", "New tab"));
                        row.title = t("新标签页", "New tab").into();
                        tabs_model.set_row_data(i, row);
                    }
                }
            }
            if let Some(w) = weak.upgrade() {
                w.set_lang_en(crate::i18n::is_en());
                w.invoke_refresh_sidebar();
                w.invoke_shortcuts_refresh();
            }
        });
    }

    // Theme toggle: flip dark ↔ light, persist the preference, and re-render
    // every open terminal with the new ANSI palette so historical output is
    // also recoloured (not just new output).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs_theme = bufs.clone();
        let proc_weak = proc_win.as_weak();
        let editor_weak = editor_win.as_weak();
        let registry = registry.clone();
        window.on_toggle_theme(move || {
            let Some(w) = weak.upgrade() else { return };
            let next_dark = !w.get_dark_mode();
            // Flip theme + every terminal buffer + re-render (shared with wallpaper).
            apply_dark_mode(&w, &bufs_theme, next_dark);
            apply_terminal_color_scheme(&w, &store.borrow(), &bufs_theme);
            refresh_terminal_scheme_list(&w, &store.borrow(), TERMINAL_SCHEME_PAGE_STEP);
            // Mirror the flip onto the detached process window (its Theme global
            // is a separate instance) so an open process window follows.
            if let Some(p) = proc_weak.upgrade() {
                sync_proc_theme(&w, &p);
            }
            if let Some(editor) = editor_weak.upgrade() {
                sync_editor_theme(&w, &editor);
                if editor.get_editor_open() {
                    let content = editor.get_editor_content();
                    editor_syntax::refresh(&editor, content.as_str());
                }
            }
            let pref = if next_dark { "dark" } else { "light" };
            {
                let mut s = store.borrow_mut();
                s.set_theme_pref(pref.to_string());
                let _ = s.save();
            }
            registry.broadcast_config_changed();
        });
    }
}
