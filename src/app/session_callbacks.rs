//! Saved-session list, session dialog and connection callbacks.

use super::*;

/// Drops the second connect of a double-click (#511). Session rows connect on
/// each click release, and users coming from Xshell / FinalShell double-click.
#[derive(Default)]
struct DoubleClickGuard {
    last: Option<(String, std::time::Instant)>,
}

impl DoubleClickGuard {
    /// The default double-click time on Windows and macOS.
    const WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

    /// False when `id` was connected less than [`Self::WINDOW`] ago.
    fn allow(&mut self, id: &str, now: std::time::Instant) -> bool {
        let repeat = matches!(&self.last, Some((last_id, at))
            if last_id == id && now.saturating_duration_since(*at) < Self::WINDOW);
        if !repeat {
            self.last = Some((id.to_string(), now));
        }
        !repeat
    }
}

#[cfg(test)]
mod double_click_tests {
    use super::DoubleClickGuard;
    use std::time::{Duration, Instant};

    #[test]
    fn the_second_click_of_a_double_click_does_not_connect_again() {
        let mut guard = DoubleClickGuard::default();
        let t = Instant::now();
        assert!(guard.allow("rdp", t));
        assert!(!guard.allow("rdp", t + Duration::from_millis(180)));
        // The ignored click does not extend the window: a deliberate third
        // click after it connects again.
        assert!(guard.allow("rdp", t + Duration::from_millis(600)));
    }

    #[test]
    fn other_sessions_and_later_clicks_still_connect() {
        let mut guard = DoubleClickGuard::default();
        let t = Instant::now();
        assert!(guard.allow("a", t));
        assert!(guard.allow("b", t + Duration::from_millis(50)));
        assert!(guard.allow("a", t + Duration::from_millis(100)));
        assert!(guard.allow("a", t + Duration::from_secs(2)));
    }
}

pub(super) fn sync_sessions_for_window(
    window: &slint::Weak<AppWindow>,
    store: &ConfigStore,
    model: &VecModel<SessionInfo>,
) {
    let Some(window) = window.upgrade() else {
        return;
    };
    let query = window.get_host_search_query().to_string();
    // Prefer in-place row updates: they keep the list's scroll position and
    // any running drag alive and skip the reallocation. A full set_vec
    // rebuild — which destroys the dragging row's pointer grab — happens
    // only when the row count changed, and the revision bump tells Welcome
    // to clear its stale drag state in exactly that case.
    if !refresh_session_rows_in_place(store, model, &query) {
        window.set_sessions_revision(window.get_sessions_revision() + 1);
    }
}

/// Parse the batch-import textarea (#150). Each non-empty, non-`#` line is
/// `host|port|user|password|name`; trailing fields are optional (port → 22,
/// user → root, password → none, name → user@host). A leading header row such as
/// `host|port|username|password|name` is skipped. Dedup happens at the call site.
pub(super) fn wire_session_callbacks(
    window: &AppWindow,
    // Registry id of `window`; connect-time prompts are tagged with it so
    // their dialogs open (and abort on close) in this window (#multi-window).
    window_id: u64,
    store: Rc<RefCell<ConfigStore>>,
    registry: Rc<WindowRegistry<slint::Weak<AppWindow>>>,
    sessions_model: Rc<VecModel<SessionInfo>>,
    tabs_model: Rc<VecModel<TabInfo>>,
    terminals_model: Rc<VecModel<TerminalState>>,
    layout: Rc<RefCell<crate::layout::Layout>>,
    content_size: Rc<std::cell::Cell<(f32, f32)>>,
    panes_model: Rc<VecModel<PaneInfo>>,
    splitters_model: Rc<VecModel<SplitterInfo>>,
    handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    bufs: TermBuffers,
    render_gates: RenderGates,
    runtime: Arc<Runtime>,
    last_term_size: Arc<Mutex<(u32, u32)>>,
    sftp_handles: SftpHandles,
    sftp_last_cwd: SftpLastCwd,
    tab_statuses: TabStatuses,
    local_snap: LocalSnap,
    local_net_hist: NetHist,
    sftp_follow_cd: Arc<std::sync::atomic::AtomicBool>,
    tab_routes: TabRoutes,
    tab_titles: Rc<RefCell<HashMap<String, String>>>,
    editor_win: Rc<EditorWindow>,
) {
    // Working set of port forwards (#56) for the session being created/edited.
    // The forward add/delete callbacks mutate it; saving reads it into
    // Session.forwards; opening the dialog (new/edit) resets it.
    let edit_forwards: Rc<RefCell<Vec<PortFwd>>> =
        Rc::new(RefCell::new(vec![blank_forward_draft()]));
    let edit_triggers: Rc<RefCell<Vec<TriggerDraft>>> =
        Rc::new(RefCell::new(vec![blank_trigger_draft()]));
    let edit_trigger_secrets: Rc<RefCell<Vec<Secret>>> =
        Rc::new(RefCell::new(vec![Secret::default()]));
    // on_connect_session moves the panes_model binding into its closure; the
    // rename handler below needs its own handle, so clone up front.
    let panes_model_rename = panes_model.clone();

    // Rebuild the session list as the user edits the Quick Connect search.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        window.on_host_search_changed(move |query| {
            if let Some(window) = weak.upgrade() {
                let query = if query.trim().is_empty() {
                    SharedString::new()
                } else {
                    query
                };
                window.set_host_search_query(query.clone());
                sync_sessions_to_model_with_filter(
                    &store.borrow(),
                    &sessions_model,
                    query.as_str(),
                );
            }
        });
    }

    let editor_test = Rc::new(RefCell::new(crate::session_test::EditorTest::default()));
    session_editor::register(&window, store.clone(), window_id, editor_test.clone());
    {
        let weak = window.as_weak();
        let active_test = editor_test.clone();
        window.on_session_dialog_closed(move || {
            if let Some(w) = weak.upgrade() {
                session_editor::stop_test(&w, window_id, &active_test);
            }
        });
    }

    {
        let weak = window.as_weak();
        let active_test = editor_test.clone();
        window.on_session_dialog_stop_test(move || {
            if let Some(w) = weak.upgrade() {
                session_editor::stop_test(&w, window_id, &active_test);
            }
        });
    }

    {
        let weak = window.as_weak();
        let active_test = editor_test.clone();
        window.on_session_dialog_changed(move || {
            if let Some(w) = weak.upgrade() {
                let snapshot = session_editor::connection_snapshot(&w);
                let same_snapshot = active_test.borrow().matches_snapshot(snapshot);
                if !same_snapshot && session_editor::stop_test(&w, window_id, &active_test) {
                    w.set_dialog_test_status("".into());
                }
            }
        });
    }

    // New session -> open dialog with blank draft.
    let weak = window.as_weak();
    let ef_new = edit_forwards.clone();
    let et_new = edit_triggers.clone();
    let ets_new = edit_trigger_secrets.clone();
    let store_ng = store.clone();
    let active_test = editor_test.clone();
    window.on_new_session_clicked(move || {
        if let Some(w) = weak.upgrade() {
            session_editor::stop_test(&w, window_id, &active_test);
        }
        if let Some(w) = weak.upgrade() {
            *ef_new.borrow_mut() = vec![blank_forward_draft()];
            *et_new.borrow_mut() = vec![blank_trigger_draft()];
            *ets_new.borrow_mut() = vec![Secret::default()];
            w.set_session_groups(session_groups_model(&store_ng.borrow()));
            w.set_dialog_forwards(forward_model(&ef_new.borrow()));
            w.set_dialog_triggers(trigger_model(&et_new.borrow()));
            let empty = Session::new_empty();
            let (jump_labels, jump_ids) = jump_candidates(&store_ng.borrow(), &empty.id);
            w.set_jump_choices(jump_labels);
            w.set_jump_ids(jump_ids);
            w.set_dialog_jumps(ModelRc::default());
            w.set_dialog_allow_secret_reveal(false);
            w.set_dialog_allow_mcp_access(true);
            w.set_dialog_id(empty.id.into());
            w.set_dialog_name("".into());
            w.set_dialog_host("".into());
            w.set_dialog_port("22".into());
            // No default username (#110): leaving it blank makes the connect-time
            // prompt ask for it, Xshell-style.
            w.set_dialog_user("".into());
            w.set_dialog_auth("password".into());
            w.set_dialog_password("".into());
            w.set_dialog_key_path("".into());
            w.set_dialog_key_inline("".into());
            w.set_dialog_key_inline_mode(false);
            w.set_dialog_test_status("".into());
            w.set_dialog_proxy_type("none".into());
            w.set_dialog_proxy_hostport("".into());
            w.set_dialog_group("".into());
            w.set_dialog_kind("ssh".into());
            w.set_dialog_serial_port("".into());
            w.set_session_serial_ports(session_serial_ports_model());
            w.set_dialog_baud("115200".into());
            w.set_dialog_data_bits("8".into());
            w.set_dialog_stop_bits("1".into());
            w.set_dialog_parity("none".into());
            w.set_dialog_flow("none".into());
            w.set_dialog_rdp_domain("".into());
            w.set_dialog_rdp_resolution(RDP_RESOLUTION_DEFAULT.into());
            w.set_dialog_rdp_width("1280".into());
            w.set_dialog_rdp_height("720".into());
            w.set_dialog_rdp_legacy_tls(false);
            w.set_dialog_rdp_admin(false);
            w.set_dialog_rdp_smart_sizing(false);
            w.set_dialog_rdp_cert_policy("ignore".into());
            w.set_dialog_rdp_network("auto".into());
            w.set_dialog_rdp_sound(false);
            w.set_dialog_rdp_clipboard(true);
            w.set_dialog_rdp_gfx(false);
            w.set_dialog_rdp_extra_args("".into());
            w.set_dialog_rdp_show_advanced(false);
            w.set_dialog_rdp_preview("".into());
            w.set_dialog_encoding("UTF-8".into());
            w.set_dialog_vt100_drawing(false);
            w.set_dialog_term_type(crate::config::DEFAULT_TERM_TYPE.into());
            w.set_dialog_session_log("default".into());
            w.set_dialog_disable_shell_integration(false);
            w.set_dialog_sftp_mode("".into());
            w.set_dialog_sftp_command("".into());
            w.set_dialog_note("".into());
            w.set_dialog_editing(false);
            w.set_dialog_open(true);
        }
    });

    // Import hosts from ~/.ssh/config -> add them as sessions (skipping dups).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let registry = registry.clone();
        window.on_import_ssh_config(move || {
            let hosts = crate::ssh::ssh_config::parse_default();
            let mut added = 0usize;
            if hosts.is_empty() {
                if let Some(w) = weak.upgrade() {
                    w.set_ssh_import_hint(
                        t("未找到 ~/.ssh/config", "no ~/.ssh/config found").into(),
                    );
                }
                return;
            }
            {
                let mut s = store.borrow_mut();
                for h in hosts {
                    // Skip if a session already has this alias, or the same
                    // host + user pair.
                    let dup = s
                        .sessions()
                        .iter()
                        .any(|x| x.name == h.alias || (x.host == h.hostname && x.user == h.user));
                    if dup {
                        continue;
                    }
                    let auth = if h.identity_file.is_empty() {
                        AuthMethod::Password
                    } else {
                        AuthMethod::Key
                    };
                    s.upsert(Session {
                        name: h.alias,
                        host: h.hostname,
                        port: h.port,
                        user: if h.user.is_empty() {
                            "root".into()
                        } else {
                            h.user
                        },
                        auth,
                        private_key_path: h.identity_file,
                        ..Session::new_empty()
                    });
                    added += 1;
                }
                if added > 0 {
                    let _ = s.save();
                }
            }
            sync_sessions_for_window(&weak, &store.borrow(), &sessions_model);
            if added > 0 {
                registry.broadcast_config_changed();
            }
            if let Some(w) = weak.upgrade() {
                let hint = if added > 0 {
                    format!("{} {}", t("已导入", "imported"), added)
                } else {
                    t("没有新主机可导入", "no new hosts to import").to_string()
                };
                w.set_ssh_import_hint(hint.into());
            }
        });
    }

    // Export all sessions to a portable JSON file (issue #46). Passwords are
    // obfuscated with the built-in export key; host/user/port stay plaintext.
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_export_sessions(move || {
            if let Some(path) = DialogOwner::of_weak(&weak)
                .file()
                .set_file_name("meatshell-connections.json")
                .add_filter("JSON", &["json"])
                .save_file()
            {
                let res = store.borrow().export_to(&path);
                if let Some(w) = weak.upgrade() {
                    let hint = match res {
                        Ok(n) => format!("{} {}", t("已导出连接", "exported"), n),
                        Err(e) => format!("{}: {}", t("导出失败", "export failed"), e),
                    };
                    w.set_ssh_import_hint(hint.into());
                }
            }
        });
    }

    // Batch-import connections from pasted text (#150). One per line:
    // `host|port|user|password|name` (trailing fields optional).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let registry = registry.clone();
        window.on_batch_import_confirm(move |text: SharedString| {
            let parsed = parse_batch_import(text.as_str());
            let total = parsed.len();
            let mut added = 0usize;
            {
                let mut s = store.borrow_mut();
                for sess in parsed {
                    // Skip a host/user/port we already have.
                    let dup = s
                        .sessions()
                        .iter()
                        .any(|x| x.host == sess.host && x.user == sess.user && x.port == sess.port);
                    if dup {
                        continue;
                    }
                    s.upsert(sess);
                    added += 1;
                }
                if added > 0 {
                    let _ = s.save();
                }
            }
            sync_sessions_for_window(&weak, &store.borrow(), &sessions_model);
            if added > 0 {
                registry.broadcast_config_changed();
            }
            if let Some(w) = weak.upgrade() {
                let hint = if total == 0 {
                    t("没有可导入的连接", "nothing to import").to_string()
                } else if added > 0 {
                    format!("{} {}/{}", t("已导入", "imported"), added, total)
                } else {
                    t("没有新连接可导入(已存在)", "no new connections (all exist)").to_string()
                };
                w.set_ssh_import_hint(hint.into());
            }
        });
    }

    // Import sessions from a portable JSON file (issue #46).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let registry = registry.clone();
        window.on_import_sessions(move || {
            if let Some(path) = DialogOwner::of_weak(&weak)
                .file()
                .add_filter("JSON", &["json"])
                .pick_file()
            {
                let res = store.borrow_mut().import_from(&path);
                if let Some(w) = weak.upgrade() {
                    let hint = match res {
                        Ok((added, skipped)) => {
                            sync_sessions_for_window(&weak, &store.borrow(), &sessions_model);
                            registry.broadcast_config_changed();
                            format!(
                                "{} {} / {} {}",
                                t("已导入", "imported"),
                                added,
                                t("跳过重复", "skipped"),
                                skipped
                            )
                        }
                        Err(e) => format!("{}: {}", t("导入失败", "import failed"), e),
                    };
                    w.set_ssh_import_hint(hint.into());
                }
            }
        });
    }

    // Edit -> open dialog prefilled.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let ef_edit = edit_forwards.clone();
        let et_edit = edit_triggers.clone();
        let ets_edit = edit_trigger_secrets.clone();
        let active_test = editor_test.clone();
        window.on_edit_session(move |id: SharedString| {
            if let Some(w) = weak.upgrade() {
                session_editor::stop_test(&w, window_id, &active_test);
            }
            let id = id.to_string();
            let store = store.borrow();
            let Some(session) = store.get(&id) else {
                return;
            };
            *ef_edit.borrow_mut() = forward_drafts(&session.forwards);
            if ef_edit.borrow().is_empty() {
                ef_edit.borrow_mut().push(blank_forward_draft());
            }
            *et_edit.borrow_mut() = trigger_drafts(&session.triggers);
            *ets_edit.borrow_mut() = session
                .triggers
                .iter()
                .map(|t| t.response.clone())
                .collect();
            if et_edit.borrow().is_empty() {
                et_edit.borrow_mut().push(blank_trigger_draft());
                ets_edit.borrow_mut().push(Secret::default());
            }
            if let Some(w) = weak.upgrade() {
                w.set_session_groups(session_groups_model(&store));
                w.set_dialog_forwards(forward_model(&ef_edit.borrow()));
                w.set_dialog_triggers(trigger_model(&et_edit.borrow()));
                w.set_dialog_id(session.id.clone().into());
                w.set_dialog_name(session.name.clone().into());
                w.set_dialog_host(session.host.clone().into());
                w.set_dialog_port(session.port.to_string().into());
                w.set_dialog_user(session.user.clone().into());
                w.set_dialog_auth(session.auth.as_str().into());
                // Start blank; the dialog only loads saved credentials after
                // opt-in, initially masked. Blank on save still retains them (#10).
                w.set_dialog_password("".into());
                w.set_dialog_key_path(session.private_key_path.clone().into());
                w.set_dialog_key_inline("".into());
                w.set_dialog_key_inline_mode(!session.private_key_inline.is_empty());
                w.set_dialog_test_status("".into());
                let (proxy_type, proxy_hostport) = split_proxy(&session.proxy);
                w.set_dialog_proxy_type(proxy_type.into());
                w.set_dialog_proxy_hostport(proxy_hostport.into());
                let (jump_labels, jump_ids) = jump_candidates(&store, &session.id);
                let route = store.resolve_jump_chain(session);
                let ids = match route {
                    Ok(hops) => hops.into_iter().rev().map(|hop| hop.id).collect(),
                    Err(err) => {
                        w.set_dialog_test_status(err.to_string().into());
                        if session.jump_session_ids.is_empty() {
                            // Require an explicit repair: flattening only the immediate hop
                            // would silently discard a broken inherited route.
                            vec![String::new(), session.jump_session_id.clone()]
                        } else {
                            session.jump_session_ids.clone()
                        }
                    }
                };
                w.set_dialog_jumps(session_editor::jump_rows(ids, &jump_ids));
                w.set_dialog_allow_secret_reveal(session.allow_secret_reveal);
                w.set_dialog_allow_mcp_access(session.mcp_access);
                w.set_jump_choices(jump_labels);
                w.set_jump_ids(jump_ids);
                w.set_dialog_group(session.group.clone().into());
                w.set_dialog_kind(session.kind.as_str().into());
                w.set_dialog_serial_port(session.serial_port.clone().into());
                w.set_session_serial_ports(session_serial_ports_model());
                w.set_dialog_baud(session.baud_rate.to_string().into());
                w.set_dialog_data_bits(session.data_bits.to_string().into());
                w.set_dialog_stop_bits(session.stop_bits.to_string().into());
                w.set_dialog_parity(session.parity.clone().into());
                w.set_dialog_flow(session.flow_control.clone().into());
                w.set_dialog_rdp_domain(session.rdp_domain.clone().into());
                w.set_dialog_rdp_resolution(
                    rdp_resolution_choice(
                        session.rdp_fullscreen,
                        session.rdp_width,
                        session.rdp_height,
                    )
                    .into(),
                );
                w.set_dialog_rdp_width(session.rdp_width.to_string().into());
                w.set_dialog_rdp_height(session.rdp_height.to_string().into());
                w.set_dialog_rdp_legacy_tls(session.rdp_legacy_tls);
                w.set_dialog_rdp_admin(session.rdp_admin);
                w.set_dialog_rdp_smart_sizing(session.rdp_smart_sizing);
                w.set_dialog_rdp_cert_policy(session.rdp_cert_policy.clone().into());
                w.set_dialog_rdp_network(session.rdp_network.clone().into());
                w.set_dialog_rdp_sound(session.rdp_sound);
                w.set_dialog_rdp_clipboard(session.rdp_clipboard);
                w.set_dialog_rdp_gfx(session.rdp_gfx);
                w.set_dialog_rdp_extra_args(session.rdp_extra_args.clone().into());
                w.set_dialog_rdp_show_advanced(!session.rdp_extra_args.is_empty());
                w.set_dialog_rdp_preview("".into());
                w.set_dialog_encoding(session.encoding.clone().into());
                w.set_dialog_vt100_drawing(session.vt100_drawing);
                w.set_dialog_term_type(session.effective_term_type().into());
                w.set_dialog_session_log(session.session_log.as_str().into());
                w.set_dialog_disable_shell_integration(session.disable_shell_integration);
                w.set_dialog_sftp_mode(session.sftp_mode.clone().into());
                w.set_dialog_sftp_command(session.sftp_command.clone().into());
                w.set_dialog_note(session.note.clone().into());
                w.set_dialog_editing(true);
                w.set_dialog_open(true);
            }
        });
    }

    // Remove session.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let registry = registry.clone();
        window.on_remove_session(move |id: SharedString| {
            {
                let mut s = store.borrow_mut();
                s.remove(&id.to_string());
                if let Err(err) = s.save() {
                    tracing::warn!("failed to save config: {err:#}");
                }
            }
            sync_sessions_for_window(&weak, &store.borrow(), &sessions_model);
            registry.broadcast_config_changed();
            if let Some(w) = weak.upgrade() {
                // Touch a property so the list re-renders reliably.
                let _ = w.get_sessions();
            }
        });
    }

    // Duplicate a session: clone it with a fresh id and a " (copy)" name (#41).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let registry = registry.clone();
        window.on_duplicate_session(move |id: SharedString| {
            let mut duplicated = false;
            {
                let mut s = store.borrow_mut();
                if let Some(orig) = s.get(&id.to_string()).cloned() {
                    let mut copy = orig;
                    copy.id = uuid::Uuid::new_v4().to_string();
                    copy.name = format!("{} (copy)", copy.name);
                    copy.last_used = None;
                    s.upsert(copy);
                    if let Err(err) = s.save() {
                        tracing::warn!("failed to save config: {err:#}");
                    }
                    duplicated = true;
                }
            }
            sync_sessions_for_window(&weak, &store.borrow(), &sessions_model);
            if duplicated {
                registry.broadcast_config_changed();
            }
            if let Some(w) = weak.upgrade() {
                let _ = w.get_sessions();
            }
        });
    }

    // Move a session to another group (#41).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let registry = registry.clone();
        window.on_move_session(move |id: SharedString, group: SharedString| {
            let mut moved = false;
            {
                let mut s = store.borrow_mut();
                if let Some(orig) = s.get(&id.to_string()).cloned() {
                    let mut target = orig;
                    // "default" is the display label for ungrouped → store empty.
                    target.group = if group.as_str().eq_ignore_ascii_case("default") {
                        String::new()
                    } else if is_reserved_session_group(group.as_str().trim()) {
                        // `system` belongs exclusively to built-in local shells.
                        return;
                    } else {
                        group.to_string()
                    };
                    s.upsert(target);
                    if let Err(err) = s.save() {
                        tracing::warn!("failed to save config: {err:#}");
                    }
                    moved = true;
                }
            }
            sync_sessions_for_window(&weak, &store.borrow(), &sessions_model);
            if moved {
                registry.broadcast_config_changed();
            }
            if let Some(w) = weak.upgrade() {
                let _ = w.get_sessions();
            }
        });
    }

    // Drag-to-reorder a host card among its same-group siblings. The stored
    // Vec order is the display order (no alphabetical sort), so a swap plus
    // re-sync is all it takes. Reordering while the list is filtered would map
    // visible hops onto the wrong stored neighbours, so bail out then — the
    // Slint side already disables the gesture while searching.
    //
    // Per-hop updates mutate the model IN PLACE (set_row_data): a full set_vec
    // rebuild would recreate the rows and drop the dragging row's pointer grab,
    // ending the drag after one hop. Saving + broadcasting are deferred to
    // reorder-session-end (pointer release).
    let sessions_dirty = Rc::new(std::cell::Cell::new(false));
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let registry = registry.clone();
        let sessions_dirty = sessions_dirty.clone();
        window.on_reorder_session(move |id: SharedString, dir: i32| {
            if weak
                .upgrade()
                .map(|window| !window.get_host_search_query().trim().is_empty())
                .unwrap_or(false)
            {
                return false;
            }
            let moved = {
                let mut s = store.borrow_mut();
                s.reorder_session(id.as_str(), dir as isize)
            };
            if moved {
                sessions_dirty.set(true);
                let query = weak
                    .upgrade()
                    .map(|w| w.get_host_search_query().to_string())
                    .unwrap_or_default();
                let in_place =
                    refresh_session_rows_in_place(&store.borrow(), &sessions_model, &query);
                if !in_place {
                    // The hop changed the row count (e.g. a cross-group hop
                    // emptied the ungrouped section): the set_vec rebuild
                    // dropped the dragging row's pointer grab, so the
                    // release's reorder-end may never arrive — finalise now.
                    sessions_dirty.set(false);
                    if let Err(err) = store.borrow_mut().save() {
                        tracing::warn!("failed to save config: {err:#}");
                    }
                    sync_sessions_for_window(&weak, &store.borrow(), &sessions_model);
                    registry.broadcast_config_changed();
                    if let Some(w) = weak.upgrade() {
                        let _ = w.get_sessions();
                    }
                }
            }
            moved
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let registry = registry.clone();
        let sessions_dirty = sessions_dirty.clone();
        window.on_reorder_session_end(move || {
            if !sessions_dirty.replace(false) {
                return;
            }
            if let Err(err) = store.borrow_mut().save() {
                tracing::warn!("failed to save config: {err:#}");
            }
            sync_sessions_for_window(&weak, &store.borrow(), &sessions_model);
            registry.broadcast_config_changed();
            if let Some(w) = weak.upgrade() {
                let _ = w.get_sessions();
            }
        });
    }

    // Collapse / expand a group in the welcome list (#41). Toggling flips the
    // `collapsed` flag on every row of that group in place — no full re-sync —
    // so the open/closed state stays put until the list is actually rebuilt.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        window.on_toggle_group(move |group: SharedString| {
            if weak
                .upgrade()
                .map(|window| !window.get_host_search_query().trim().is_empty())
                .unwrap_or(false)
            {
                return;
            }
            use slint::Model as _;
            let target = group.to_string();
            let n = sessions_model.row_count();
            // New state = the opposite of the group's first row.
            let mut new_state = false;
            for i in 0..n {
                if let Some(row) = sessions_model.row_data(i) {
                    if row.group.as_str() == target {
                        new_state = !row.collapsed;
                        break;
                    }
                }
            }
            for i in 0..n {
                if let Some(mut row) = sessions_model.row_data(i) {
                    if row.group.as_str() == target {
                        row.collapsed = new_state;
                        sessions_model.set_row_data(i, row);
                    }
                }
            }
            {
                let mut store = store.borrow_mut();
                store.set_session_group_collapsed(&target, new_state);
                if let Err(err) = store.save() {
                    tracing::warn!("failed to save Quick Connect folder state: {err:#}");
                }
            }
            if let Some(w) = weak.upgrade() {
                let _ = w.get_sessions();
            }
        });
    }

    // Group create / rename (#41).
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let registry = registry.clone();
        window.on_submit_group(move |orig: SharedString, name: SharedString| {
            let trimmed = name.trim();
            let error = {
                let s = store.borrow();
                if trimmed.is_empty() {
                    Some(t("请输入分组名称", "Enter a group name"))
                } else if is_reserved_session_group(trimmed) {
                    Some(t("该名称为系统保留分组", "This group name is reserved"))
                } else if (orig.is_empty() || !trimmed.eq_ignore_ascii_case(orig.as_str()))
                    && s.session_group_exists(trimmed)
                {
                    Some(t("分组已存在", "Group already exists"))
                } else {
                    None
                }
            };
            if let Some(message) = error {
                return SharedString::from(message);
            }
            {
                let mut s = store.borrow_mut();
                if orig.is_empty() {
                    s.add_group(trimmed.to_string());
                } else {
                    s.rename_group(orig.as_str(), trimmed.to_string());
                }
                if let Err(err) = s.save() {
                    tracing::warn!("failed to save config: {err:#}");
                }
            }
            sync_sessions_for_window(&weak, &store.borrow(), &sessions_model);
            registry.broadcast_config_changed();
            if let Some(w) = weak.upgrade() {
                let _ = w.get_sessions();
            }
            SharedString::new()
        });
    }
    // Group delete (#41) — UI only offers this on empty groups.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let registry = registry.clone();
        window.on_delete_group(move |name: SharedString| {
            {
                let mut s = store.borrow_mut();
                s.remove_group(&name.to_string());
                if let Err(err) = s.save() {
                    tracing::warn!("failed to save config: {err:#}");
                }
            }
            sync_sessions_for_window(&weak, &store.borrow(), &sessions_model);
            registry.broadcast_config_changed();
            if let Some(w) = weak.upgrade() {
                let _ = w.get_sessions();
            }
        });
    }

    // Dialog submit -> persist + (optionally) connect.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let edit_forwards = edit_forwards.clone();
        let edit_triggers = edit_triggers.clone();
        let edit_trigger_secrets = edit_trigger_secrets.clone();
        let registry = registry.clone();
        let active_test = editor_test.clone();
        window.on_session_dialog_submit(move |draft: SessionDraft| {
            if let Some(w) = weak.upgrade() {
                session_editor::stop_test(&w, window_id, &active_test);
            }
            let id = draft.id.to_string();
            let forwards = match validated_port_forwards(&edit_forwards.borrow()) {
                Ok(forwards) => forwards,
                Err(message) => {
                    if let Some(w) = weak.upgrade() {
                        w.set_dialog_test_status(message.into());
                    }
                    return;
                }
            };
            let triggers =
                match validated_triggers(&edit_triggers.borrow(), &edit_trigger_secrets.borrow()) {
                    Ok(triggers) => triggers,
                    Err(message) => {
                        if let Some(w) = weak.upgrade() {
                            w.set_dialog_test_status(message.into());
                        }
                        return;
                    }
                };
            // Saving and testing use the same draft conversion, including blank-secret retention.
            let existing = store.borrow().get(&id).cloned();
            let new_session = session_from_draft(&draft, existing.as_ref(), forwards, triggers);
            if new_session.kind == crate::config::SessionKind::Rdp {
                if let Err(message) = crate::rdp::validate_options(&new_session) {
                    if let Some(w) = weak.upgrade() {
                        w.set_dialog_test_status(message.into());
                    }
                    return;
                }
            }
            if let Err(err) = store.borrow().resolve_jump_chain(&new_session) {
                if let Some(w) = weak.upgrade() {
                    w.set_dialog_test_status(err.to_string().into());
                }
                return;
            }
            {
                let mut s = store.borrow_mut();
                s.upsert(new_session);
                if let Err(err) = s.save() {
                    tracing::warn!("failed to save config: {err:#}");
                    if let Some(w) = weak.upgrade() {
                        w.set_dialog_test_status(err.to_string().into());
                    }
                    return;
                }
            }
            sync_sessions_for_window(&weak, &store.borrow(), &sessions_model);
            registry.broadcast_config_changed();
            if let Some(w) = weak.upgrade() {
                w.set_dialog_open(false);
            }
        });
    }

    // Preview from unsaved draft; the saved password is never rendered.
    window.on_session_dialog_preview_rdp(move |draft: SessionDraft| {
        let session = session_from_draft(&draft, None, Vec::new(), Vec::new());
        if session.kind != crate::config::SessionKind::Rdp {
            return String::new().into();
        }
        crate::rdp::preview_args(&session).unwrap_or_else(|message| message).into()
    });

    // Test connection from the session dialog. SSH tests use the same handshake,
    // host-key verification, proxy/jump routing, and authentication as a real
    // terminal connection (#276). Telnet and serial retain reachability tests.
    {
        let weak = window.as_weak();
        let runtime = runtime.clone();
        let store = store.clone();
        let edit_forwards = edit_forwards.clone();
        let edit_triggers = edit_triggers.clone();
        let edit_trigger_secrets = edit_trigger_secrets.clone();
        let active_test = editor_test.clone();
        window.on_session_dialog_test(move |draft: SessionDraft| {
            if let Some(w) = weak.upgrade() {
                session_editor::stop_test(&w, window_id, &active_test);
            }
            let snapshot = weak.upgrade().map(|w| session_editor::connection_snapshot(&w)).unwrap_or_default();
            let ticket = active_test.borrow_mut().begin(snapshot);
            let kind = draft.kind.to_string();
            if kind == "serial" {
                let port_name = draft.serial_port.to_string();
                let baud = if draft.baud_rate <= 0 {
                    115_200
                } else {
                    draft.baud_rate as u32
                };
                let weak_done = weak.clone();
                let task = runtime.spawn(async move {
                    let message = match tokio::task::spawn_blocking(move || {
                        serialport::new(&port_name, baud)
                            .timeout(std::time::Duration::from_millis(800))
                            .open()
                    })
                    .await
                    {
                        Ok(Ok(_)) => t("连接正常", "Connection OK").to_string(),
                        Ok(Err(e)) => format!("{}: {e}", t("连接失败", "Connection failed")),
                        Err(e) => format!("{}: {e}", t("连接失败", "Connection failed")),
                    };
                    session_editor::finish_test(weak_done, window_id, ticket, message);
                });
                active_test.borrow_mut().attach(task.abort_handle());
                return;
            }

            let existing = store.borrow().get(draft.id.as_str()).cloned();
            let forwards = match validated_port_forwards(&edit_forwards.borrow()) {
                Ok(forwards) => forwards,
                Err(message) => {
                    if let Some(w) = weak.upgrade() {
                        w.set_dialog_test_status(message.into());
                    }
                    return;
                }
            };
            let triggers =
                match validated_triggers(&edit_triggers.borrow(), &edit_trigger_secrets.borrow()) {
                    Ok(triggers) => triggers,
                    Err(message) => {
                        if let Some(w) = weak.upgrade() {
                            w.set_dialog_test_status(message.into());
                        }
                        return;
                    }
                };
            let session = session_from_draft(&draft, existing.as_ref(), forwards, triggers);
            let weak_done = weak.clone();

            if kind == "ssh" {
                let jump = match resolve_jump(&store, &session) {
                    Ok(jump) => jump,
                    Err(error) => {
                        if let Some(w) = weak.upgrade() {
                            w.set_dialog_test_status(error.to_string().into());
                        }
                        return;
                    }
                };
                let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
                let task = runtime.spawn(async move {
                    let mut test = Box::pin(test_session_auth(session, jump, events_tx));
                    let mut events_open = true;
                    let result = loop {
                        tokio::select! {
                            result = &mut test => break result,
                            event = events_rx.recv(), if events_open => {
                                let Some(event) = event else { events_open = false; continue };
                                if let SessionEvent::Status(ref status) = event {
                                    let weak_status = weak_done.clone();
                                    let status_ticket = ticket.clone();
                                    let status = status.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if !status_ticket.is_active() { return; }
                                        if let Some(w) = weak_status.upgrade() {
                                            if w.get_dialog_open()
                                                && status_ticket.matches_snapshot(session_editor::connection_snapshot(&w))
                                            {
                                                w.set_dialog_test_status(status.into());
                                            }
                                        }
                                    });
                                }
                                if matches!(
                                    event,
                                    SessionEvent::HostKeyPrompt { .. }
                                        | SessionEvent::CredentialPrompt { .. }
                                        | SessionEvent::MfaPrompt { .. }
                                ) {
                                    let weak_prompt = weak_done.clone();
                                    let prompt_ticket = ticket.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if !prompt_ticket.is_active() { return; }
                                        let Some(w) = weak_prompt.upgrade() else { return };
                                        if !w.get_dialog_open()
                                            || !prompt_ticket.matches_snapshot(session_editor::connection_snapshot(&w))
                                        { return; }
                                        match event {
                                            SessionEvent::HostKeyPrompt {
                                                host,
                                                port,
                                                key_type,
                                                fingerprint,
                                                changed,
                                                responder,
                                            } => enqueue_hostkey_prompt_scoped(
                                                &w,
                                                window_id,
                                                Some(prompt_ticket.id),
                                                host,
                                                port,
                                                key_type,
                                                fingerprint,
                                                changed,
                                                responder,
                                            ),
                                            SessionEvent::CredentialPrompt {
                                                session_id,
                                                host,
                                                user,
                                                need_user,
                                                need_password,
                                                responder,
                                            } => enqueue_cred_prompt_scoped(
                                                &w,
                                                window_id,
                                                Some(prompt_ticket.id),
                                                session_id,
                                                host,
                                                user,
                                                need_user,
                                                need_password,
                                                responder,
                                            ),
                                            SessionEvent::MfaPrompt {
                                                session_id,
                                                host,
                                                prompt,
                                                echo,
                                                responder,
                                            } => enqueue_mfa_prompt_scoped(
                                                &w,
                                                window_id,
                                                Some(prompt_ticket.id),
                                                session_id,
                                                host,
                                                prompt,
                                                echo,
                                                responder,
                                            ),
                                            _ => {}
                                        }
                                    });
                                }
                            }
                        }
                    };
                    let message = match result {
                        Ok(()) => t("连接正常", "Connection OK").to_string(),
                        Err(e) => format!("{}: {e:#}", t("连接失败", "Connection failed")),
                    };
                    session_editor::finish_test(weak_done, window_id, ticket, message);
                });
                active_test.borrow_mut().attach(task.abort_handle());
                return;
            }

            let host = session.host;
            let port = session.port;
            let task = runtime.spawn(async move {
                let target = format!("{host}:{port}");
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    tokio::net::TcpStream::connect((host.as_str(), port)),
                )
                .await;
                let message = match result {
                    Ok(Ok(_)) => t("连接正常", "Connection OK").to_string(),
                    Ok(Err(e)) => format!("{}: {e}", t("连接失败", "Connection failed")),
                    Err(_) => format!("{}: {target}", t("连接超时", "Connection timed out")),
                };
                session_editor::finish_test(weak_done, window_id, ticket, message);
            });
            active_test.borrow_mut().attach(task.abort_handle());
        });
    }

    // Cancel dialog.
    {
        let weak = window.as_weak();
        let active_test = editor_test.clone();
        window.on_session_dialog_cancel(move || {
            if let Some(w) = weak.upgrade() {
                session_editor::stop_test(&w, window_id, &active_test);
            }
            if let Some(w) = weak.upgrade() {
                w.set_dialog_open(false);
            }
        });
    }

    // Private-key file picker: pick the private key and store its path with
    // forward-slash separators (uniform across Windows/Linux; russh accepts them).
    {
        let weak = window.as_weak();
        window.on_session_dialog_pick_key(move || {
            let mut dialog = DialogOwner::of_weak(&weak)
                .file()
                .set_title(t("选择私钥文件", "Choose private key file"));
            // OpenSSH's standard key names (id_ed25519, id_rsa, …) usually
            // have no extension. Extension filters hide or disable those files
            // in native pickers, so show every file on every platform (#393).
            // Start in ~/.ssh if it exists.
            if let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().join(".ssh")) {
                if home.is_dir() {
                    dialog = dialog.set_directory(home);
                }
            }
            if let Some(file) = dialog.pick_file() {
                let path = file.to_string_lossy().replace('\\', "/");
                if let Some(w) = weak.upgrade() {
                    w.set_dialog_key_path(path.into());
                }
            }
        });
    }

    // Add another editable port-forward row (#56, #277).
    {
        let weak = window.as_weak();
        let ef = edit_forwards.clone();
        window.on_add_forward(move || {
            ef.borrow_mut().push(blank_forward_draft());
            if let Some(w) = weak.upgrade() {
                w.set_dialog_forwards(forward_model(&ef.borrow()));
            }
        });
    }
    // Keep each editable row in the Rust-side working set. Saving validates and
    // converts all non-empty rows together, so no separate "added" state exists.
    {
        let ef = edit_forwards.clone();
        window.on_update_forward(move |index: i32, forward: PortFwd| {
            let i = index as usize;
            let mut forwards = ef.borrow_mut();
            if i < forwards.len() {
                forwards[i] = forward;
            }
        });
    }
    // Delete a port forward by index (#56).
    {
        let weak = window.as_weak();
        let ef = edit_forwards.clone();
        window.on_delete_forward(move |index: i32| {
            let i = index as usize;
            {
                let mut v = ef.borrow_mut();
                if i < v.len() {
                    v.remove(i);
                }
                if v.is_empty() {
                    v.push(blank_forward_draft());
                }
            }
            if let Some(w) = weak.upgrade() {
                w.set_dialog_forwards(forward_model(&ef.borrow()));
            }
        });
    }

    // Session expect/send trigger editor (#212).
    {
        let weak = window.as_weak();
        let triggers = edit_triggers.clone();
        let secrets = edit_trigger_secrets.clone();
        window.on_add_trigger(move || {
            triggers.borrow_mut().push(blank_trigger_draft());
            secrets.borrow_mut().push(Secret::default());
            if let Some(w) = weak.upgrade() {
                w.set_dialog_triggers(trigger_model(&triggers.borrow()));
            }
        });
    }
    {
        let triggers = edit_triggers.clone();
        window.on_update_trigger(move |index: i32, trigger: TriggerDraft| {
            let i = index as usize;
            let mut values = triggers.borrow_mut();
            if i < values.len() {
                values[i] = trigger;
            }
        });
    }
    {
        let weak = window.as_weak();
        let triggers = edit_triggers.clone();
        let secrets = edit_trigger_secrets.clone();
        window.on_delete_trigger(move |index: i32| {
            let i = index as usize;
            let mut values = triggers.borrow_mut();
            let mut saved = secrets.borrow_mut();
            if i < values.len() {
                values.remove(i);
            }
            if i < saved.len() {
                saved.remove(i);
            }
            if values.is_empty() {
                values.push(blank_trigger_draft());
                saved.push(Secret::default());
            }
            if let Some(w) = weak.upgrade() {
                w.set_dialog_triggers(trigger_model(&values));
            }
        });
    }

    // Connect session -> open a new terminal tab.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let tabs_model = tabs_model.clone();
        let terminals_model = terminals_model.clone();
        let layout = layout.clone();
        let content_size = content_size.clone();
        let handles = handles.clone();
        let bufs = bufs.clone();
        let render_gates = render_gates.clone();
        let runtime = runtime.clone();
        let last_term_size = last_term_size.clone();
        let sftp_handles = sftp_handles.clone();
        let sftp_last_cwd = sftp_last_cwd.clone();
        let tab_statuses = tab_statuses.clone();
        let local_snap = local_snap.clone();
        let local_net_hist = local_net_hist.clone();
        let sftp_follow_cd = sftp_follow_cd.clone();
        let tab_routes = tab_routes.clone();
        let repeat_guard = RefCell::new(DoubleClickGuard::default());
        window.on_connect_session(move |id: SharedString| {
            let id = id.to_string();
            // A session row connects on every click release, so a double-click
            // arrives here twice: two tabs, or two stacked RDP windows that
            // kick each other off (#511). Only the first click connects.
            if !repeat_guard
                .borrow_mut()
                .allow(&id, std::time::Instant::now())
            {
                return;
            }
            let builtin =
                id.starts_with("system:") || id.starts_with(super::agents::AGENT_ID_PREFIX);
            let session = if builtin {
                match resolve_builtin_session(&store.borrow(), &id) {
                    Some(s) => s,
                    None => {
                        // An agent uninstalled since meatshell detected it.
                        if let (Some(name), Some(w)) = (
                            super::agents::agent_name(&store.borrow(), &id),
                            weak.upgrade(),
                        ) {
                            w.set_ssh_import_hint(
                                format!("{} {name}", t("未找到", "Not found:")).into(),
                            );
                        }
                        return;
                    }
                }
            } else {
                match store.borrow().get(&id).cloned() {
                    Some(s) => s,
                    None => return,
                }
            };
            // ── RDP: hand the saved account to the system client, open no tab ──
            // FinalShell does the same thing: meatshell stores host / port /
            // user / password and starts the operating system's own remote
            // desktop client (mstsc on Windows), so the session lives in a
            // native window rather than in one of our tabs.
            if session.kind == SessionKind::Rdp {
                let message = match crate::rdp::launch(&session) {
                    Ok(started) => format!(
                        "{} {}",
                        t(
                            "已用系统远程桌面打开",
                            "Opened with the system remote desktop client"
                        ),
                        started
                    ),
                    Err(err) => format!("{}: {err}", t("RDP 启动失败", "Failed to start RDP")),
                };
                tracing::info!("{message}");
                if let Some(w) = weak.upgrade() {
                    w.set_ssh_import_hint(message.into());
                }
                return;
            }
            let tab_id = format!("term-{}", uuid::Uuid::new_v4());
            let tab_title = session.name.clone();

            // Connection label shown in the sidebar / status line, per transport.
            let conn_label = match session.kind {
                SessionKind::Ssh => format!("{}@{}", session.user, session.host),
                SessionKind::Serial => {
                    format!("{} @{}", session.serial_port, session.baud_rate)
                }
                SessionKind::Telnet => format!("telnet {}:{}", session.host, session.port),
                SessionKind::Local => format!("local {}", session.name),
                // RDP opens in the system client instead of a tab (see the
                // early return above); this label only exists for completeness.
                SessionKind::Rdp => format!("rdp {}:{}", session.host, session.port),
            };
            // Compatibility mode also suppresses the SFTP side-channel so
            // bastions that only permit one proxied PTY connection stay alive.
            let has_sftp = should_start_sftp(&session);

            // Seed the per-tab status so the sidebar shows "连接中 host" the
            // moment this tab becomes active (the `changed active-tab-id`
            // handler fires refresh-sidebar right after set_active_tab_id below).
            tab_statuses.lock().unwrap().insert(
                tab_id.clone(),
                TabStatus {
                    host: conn_label.clone(),
                    user: session.user.clone(),
                    session_id: id.clone(),
                    state: 0,
                    is_local: session.kind == SessionKind::Local,
                    ..Default::default()
                },
            );

            // Register tab + terminal state (SFTP fields start empty/loading).
            tabs_model.push(TabInfo {
                id: tab_id.clone().into(),
                title_len: tab_title_len(&tab_title),
                title: tab_title.into(),
                kind: "terminal".into(),
                connected: false,
                // Local shells offer "Add to session list" (#501).
                local: session.kind == SessionKind::Local,
            });
            // Each session keeps its own SFTP collapse state + sizes, seeded from
            // the global defaults (the "collapse SFTP by default" pref and the
            // persisted panel sizes) so they no longer bleed across panes (#v0.5).
            let (sftp_collapsed_default, sftp_h_default, sftp_w_default) = weak
                .upgrade()
                .map(|w| {
                    (
                        w.get_collapse_sftp_default(),
                        w.get_sftp_panel_height(),
                        w.get_sftp_panel_width(),
                    )
                })
                .unwrap_or((false, 220.0, 380.0));
            terminals_model.push(TerminalState {
                id: tab_id.clone().into(),
                status: t("连接中...", "Connecting...").into(),
                spans: ModelRc::from(std::rc::Rc::new(VecModel::<TermSpan>::default())),
                cursor_row: 0,
                cursor_col: 0,
                rows_used: 0,
                scroll_max: 0,
                scroll_offset: 0,
                is_alt_screen: false,
                mouse_tracked: false,
                find_matches: ModelRc::from(std::rc::Rc::new(VecModel::<TermMatch>::default())),
                selection: ModelRc::from(std::rc::Rc::new(VecModel::<TermMatch>::default())),
                sftp_path: "/".into(),
                sftp_entries: ModelRc::from(std::rc::Rc::new(VecModel::<SftpEntry>::default())),
                sftp_status: if has_sftp {
                    t("SFTP 连接中...", "SFTP connecting...").into()
                } else {
                    t(
                        "此会话类型不支持 SFTP",
                        "SFTP not available for this session",
                    )
                    .into()
                },
                sftp_loading: has_sftp,
                sftp_tree_nodes: ModelRc::from(std::rc::Rc::new(
                    VecModel::<SftpTreeNode>::default(),
                )),
                sftp_selected_count: 0,
                sftp_sort_key: "".into(),
                sftp_sort_dir: 0,
                sftp_available: has_sftp,
                sftp_bookmarks: {
                    let places = store.borrow().sftp_places(&id);
                    super::sftp_places::string_model(&places.bookmarks)
                },
                sftp_recent: {
                    let places = store.borrow().sftp_places(&id);
                    super::sftp_places::string_model(&places.recent)
                },
                sftp_bookmarked: false,
                // Split view starts off; the local pane remembers the home
                // folder until it is first opened (listing is lazy).
                sftp_dual: false,
                sftp_local_path: home_display().into(),
                sftp_local_entries: ModelRc::from(std::rc::Rc::new(
                    VecModel::<SftpEntry>::default(),
                )),
                sftp_local_status: "".into(),
                sftp_local_loading: false,
                sftp_local_selected_count: 0,
                // 0 = an even local/remote split; set when the divider is dragged.
                sftp_local_pane_width: 0.0,
                font_size: 0,
                tunnels: ModelRc::from(std::rc::Rc::new(VecModel::<TunnelInfo>::default())),
                sftp_collapsed: !has_sftp || sftp_collapsed_default,
                sftp_panel_height: sftp_h_default,
                sftp_panel_width: sftp_w_default,
                sftp_saved_height: sftp_h_default,
            });
            // Create vt100 parser for this tab (default 24×80; resized on first
            // terminal-resize callback). 5000-line scrollback is stored for
            // future scroll-navigation support.
            let is_dark_now = weak.upgrade().map(|w| w.get_dark_mode()).unwrap_or(true);
            let (output_highlight, custom_highlight_rules) = {
                let settings = store.borrow();
                (
                    OutputHighlightPreset::from_settings(
                        settings.output_highlight_enabled(),
                        settings.output_highlight_preset(),
                    ),
                    compile_output_rules(settings.output_highlight_rules()),
                )
            };
            bufs.lock().unwrap().insert(
                tab_id.clone(),
                Arc::new(Mutex::new(TermBuffer {
                    parser: vt100::Parser::new(24, 80, 5000),
                    find_query: String::new(),
                    is_dark: is_dark_now,
                    output_highlight,
                    custom_highlight_rules,
                    json_format_output: store.borrow().json_format_output(),
                    vt100_drawing: session.vt100_drawing,
                    charset: crate::terminal::CharsetTracker::default(),
                    interactive_echo_until: std::time::Instant::now(),
                    sel_anchor: None,
                    sel_focus: None,
                    sel_ranges: Vec::new(),
                    mouse_tracked: false,
                    history: VecDeque::new(),
                    prev: Vec::new(),
                    view_offset: 0,
                    scroll_accum: 0.0,
                    displayed_text: Vec::new(),
                    csi_state: CsiState::Normal,
                    csi_pending: Vec::new(),
                    raw: std::collections::VecDeque::new(),
                    session_log: None,
                    session_log_spec: session_log_spec(&session),
                })),
            );
            // Session logging (#265): open the transcript before the first
            // byte arrives.
            {
                let (enabled, dir) = {
                    let settings = store.borrow();
                    (settings.session_log_enabled(), settings.session_log_dir())
                };
                with_term_buf(&bufs, &tab_id, |b| {
                    apply_session_log_to_buffer(b, enabled, &dir)
                });
            }
            render_gates.lock().unwrap().insert(
                tab_id.clone(),
                Arc::new(TabRenderGate::new(RENDER_MIN_INTERVAL)),
            );
            // No followed-cwd yet: the first OSC 7 always triggers a follow.
            sftp_last_cwd.lock().unwrap().remove(&tab_id);
            // Add the new tab to the focused pane and re-flatten (this also sets
            // active-tab-id to the new tab via refresh_panes).
            layout.borrow_mut().add_tab(tab_id.clone());
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

            // Spawn the shell (+ SFTP) workers and their event-pump threads.
            // Shared with in-place reconnect (#79) via start_session_in_tab.
            let ctx = ConnectCtx {
                weak: weak.clone(),
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
                tab_routes: tab_routes.clone(),
            };
            start_session_in_tab(&tab_id, session, &ctx);
        });
    }

    // Duplicate a tab's connection (#v0.5): open a fresh tab to the same saved
    // session, landing in the same pane as the source tab.
    {
        let weak = window.as_weak();
        let tab_statuses = tab_statuses.clone();
        let layout = layout.clone();
        window.on_tab_duplicate(move |tab_id: SharedString| {
            let tab_id = tab_id.to_string();
            let session_id = tab_statuses
                .lock()
                .unwrap()
                .get(&tab_id)
                .map(|s| s.session_id.clone())
                .unwrap_or_default();
            if session_id.is_empty() {
                return;
            }
            // Land the new tab in the same pane as the source. Read the pane id
            // into a local first so the immutable borrow is dropped before the
            // borrow_mut (else RefCell panics on the overlapping borrow).
            let pane = layout.borrow().leaf_of_tab(&tab_id);
            if let Some(pane) = pane {
                layout.borrow_mut().focused = pane;
            }
            if let Some(w) = weak.upgrade() {
                w.invoke_connect_session(session_id.into());
            }
        });
    }

    // Rename session (tab context menu): open the dialog pre-filled with the
    // tab's current title.
    {
        let weak = window.as_weak();
        let tabs_model = tabs_model.clone();
        window.on_tab_rename_request(move |tab_id: SharedString| {
            let tab_id = tab_id.to_string();
            if tab_id.is_empty() || tab_id == "welcome" {
                return;
            }
            use slint::Model as _;
            let title = (0..tabs_model.row_count())
                .find_map(|i| {
                    let row = tabs_model.row_data(i)?;
                    (row.id.as_str() == tab_id).then(|| row.title.to_string())
                })
                .unwrap_or_default();
            if let Some(w) = weak.upgrade() {
                w.set_tab_rename_id(tab_id.into());
                w.set_tab_rename_value(title.into());
                w.set_tab_rename_open(true);
            }
        });
    }

    // Apply the new display name. An empty name clears the override and
    // restores the saved session's name. Display-only: the config is untouched.
    {
        let weak = window.as_weak();
        let tabs_model = tabs_model.clone();
        let panes_model = panes_model_rename.clone();
        let tab_statuses = tab_statuses.clone();
        let tab_titles = tab_titles.clone();
        let store = store.clone();
        window.on_rename_tab(move |tab_id: SharedString, name: SharedString| {
            use slint::Model as _;
            if let Some(w) = weak.upgrade() {
                w.set_tab_rename_open(false);
            }
            let tab_id = tab_id.to_string();
            let name = name.trim().to_string();
            let title = if name.is_empty() {
                tab_titles.borrow_mut().remove(&tab_id);
                let session_id = tab_statuses
                    .lock()
                    .unwrap()
                    .get(&tab_id)
                    .map(|s| s.session_id.clone())
                    .unwrap_or_default();
                // Two separate borrows: the builtin fallback must not run while
                // the store lookup's RefCell borrow is still alive.
                let saved = store.borrow().get(&session_id).map(|s| s.name.clone());
                saved
                    .or_else(|| {
                        session_models::builtin_local_sessions(store.borrow().wsl_profiles())
                            .into_iter()
                            .find(|s| s.id == session_id)
                            .map(|s| s.name)
                    })
                    .or_else(|| super::agents::agent_name(&store.borrow(), &session_id))
            } else {
                tab_titles.borrow_mut().insert(tab_id.clone(), name.clone());
                Some(name)
            };
            let Some(title) = title else {
                return;
            };
            for i in 0..tabs_model.row_count() {
                if let Some(mut row) = tabs_model.row_data(i) {
                    if row.id.as_str() == tab_id {
                        row.title_len = tab_title_len(&title);
                        row.title = title.clone().into();
                        tabs_model.set_row_data(i, row);
                        break;
                    }
                }
            }
            // The tab strips render pane.tabs — per-pane snapshot sub-models
            // built by refresh_panes — not tabs_model itself, so update them
            // in place too.
            for pi in 0..panes_model.row_count() {
                if let Some(pane) = panes_model.row_data(pi) {
                    for ti in 0..pane.tabs.row_count() {
                        if let Some(mut tab) = pane.tabs.row_data(ti) {
                            if tab.id.as_str() == tab_id {
                                tab.title_len = tab_title_len(&title);
                                tab.title = title.clone().into();
                                pane.tabs.set_row_data(ti, tab);
                            }
                        }
                    }
                }
            }
        });
    }
}

/// Re-sync this window when another window persists sessions, theme or language.
pub(super) fn listen_for_config_changes(ctx: &WinCtx, sessions_model: &Rc<VecModel<SessionInfo>>) {
    let WinCtx {
        store,
        registry,
        bufs,
        window,
        editor_win,
        ..
    } = ctx;
    let window_id = ctx.window_id;
    // Cross-window propagation: when another window persists sessions / theme /
    // language it broadcasts; re-sync what this window shows. The listener only
    // READS the store and updates this window's models (never saves), so a
    // broadcast can never re-enter the broadcast path.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let bufs = bufs.clone();
        let editor_weak = editor_win.as_weak();
        registry.add_config_listener(
            window_id,
            Rc::new(move || {
                let Some(w) = weak.upgrade() else { return };
                // Rebuild the list with the window's current search filter.
                sync_sessions_for_window(&weak, &store.borrow(), &sessions_model);
                // Custom agents (#427): launch menu and Settings list.
                w.invoke_refresh_agent_launchers();
                // Re-apply the theme to the chrome AND every open terminal buffer.
                apply_dark_mode(&w, &bufs, theme_pref_is_dark(&store.borrow()));
                // Colour theme preset + immersive switch (Wallpaper & theme).
                super::color_theme::apply_color_theme(&w, &store.borrow());
                // Language translations are process-global; refresh our flag only.
                w.set_lang_en(crate::i18n::is_en());
                // Command-bar visibility is a global preference.
                w.set_cmd_bar_hidden(store.borrow().cmd_bar_hidden());
                // Persisted terminal font size (settings stepper) is global too.
                w.set_term_font_size(store.borrow().font_size() as f32);
                // SSH keepalive (#515) is a client-wide setting.
                super::settings_callbacks::sync_ssh_keepalive(&w, &store.borrow());
                // Docked vs detached editor (#494) is a global preference.
                w.set_editor_embedded(store.borrow().editor_embedded());
                if let Some(editor) = editor_weak.upgrade() {
                    sync_editor_theme(&w, &editor);
                    if editor.get_editor_open() {
                        let content = editor.get_editor_content();
                        editor_syntax::refresh(&editor, content.as_str());
                    }
                }
            }),
        );
    }
}

/// WSL working-directory picker and generated WSL profiles.
pub(super) fn wire_wsl_profiles(ctx: &WinCtx, sessions_model: &Rc<VecModel<SessionInfo>>) {
    let WinCtx {
        store,
        registry,
        window,
        ..
    } = ctx;
    {
        let weak = window.as_weak();
        window.on_pick_wsl_directory(move || {
            if let Some(folder) = DialogOwner::of_weak(&weak).file().pick_folder() {
                if let Some(w) = weak.upgrade() {
                    w.set_wsl_new_directory(folder.to_string_lossy().to_string().into());
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let registry = registry.clone();
        window.on_add_wsl_profile(move |name, distribution, directory| {
            {
                let mut s = store.borrow_mut();
                s.add_wsl_profile(
                    name.to_string(),
                    distribution.to_string(),
                    directory.to_string(),
                );
                let _ = s.save();
                if let Some(w) = weak.upgrade() {
                    w.set_wsl_profiles(wsl_profile_model(&s));
                    w.set_launch_shells(launch_shell_model(&s));
                    sync_sessions_for_window(&weak, &s, &sessions_model);
                }
            }
            registry.broadcast_config_changed();
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let sessions_model = sessions_model.clone();
        let registry = registry.clone();
        window.on_remove_wsl_profile(move |id| {
            {
                let mut s = store.borrow_mut();
                s.remove_wsl_profile(id.as_str());
                let _ = s.save();
                if let Some(w) = weak.upgrade() {
                    w.set_wsl_profiles(wsl_profile_model(&s));
                    w.set_launch_shells(launch_shell_model(&s));
                    sync_sessions_for_window(&weak, &s, &sessions_model);
                }
            }
            registry.broadcast_config_changed();
        });
    }
}

/// Host-key, credential and MFA prompt callbacks for this window.
pub(super) fn wire_auth_prompts(ctx: &WinCtx) {
    let WinCtx {
        registry, window, ..
    } = ctx;
    let window_id = ctx.window_id;
    // Host-key confirmation dialog (#109-5): the user trusts or rejects the
    // presented server key; the decision fans back out to the blocked SSH/SFTP
    // handler(s) and the next queued prompt for THIS window (if any) is shown.
    {
        let weak = window.as_weak();
        window.on_hostkey_accept(move || {
            if let Some(w) = weak.upgrade() {
                resolve_front_hostkey(&w, window_id, true);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_hostkey_reject(move || {
            if let Some(w) = weak.upgrade() {
                resolve_front_hostkey(&w, window_id, false);
            }
        });
    }

    // Connect-time credential prompt (#110): the user supplies the missing
    // username/password (or cancels); the answer unblocks the SSH/SFTP auth.
    {
        let weak = window.as_weak();
        let registry = registry.clone();
        window.on_cred_accept(move || {
            if let Some(w) = weak.upgrade() {
                let remember = w.get_cred_remember();
                resolve_front_cred(&w, window_id, true);
                // "Remember" persisted new credentials onto the saved session
                // (auth_dialogs::persist_credentials); the registry is not
                // reachable there, so broadcast from this owning callback.
                if remember {
                    registry.broadcast_config_changed();
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_cred_reject(move || {
            if let Some(w) = weak.upgrade() {
                resolve_front_cred(&w, window_id, false);
            }
        });
    }

    // MFA / keyboard-interactive prompt (#86-MFA): the user enters the
    // verification code (or cancels); the answer unblocks the SSH/SFTP auth.
    {
        let weak = window.as_weak();
        window.on_mfa_submit(move || {
            if let Some(w) = weak.upgrade() {
                resolve_front_mfa(&w, window_id, true);
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_mfa_cancel(move || {
            if let Some(w) = weak.upgrade() {
                resolve_front_mfa(&w, window_id, false);
            }
        });
    }
}
