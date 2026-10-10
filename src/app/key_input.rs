//! Keyboard input routing from the terminal view to the PTY.

use super::*;

pub(super) fn wire_key_input(
    window: &AppWindow,
    handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    bufs: TermBuffers,
    last_term_size: Arc<Mutex<(u32, u32)>>,
    store: Rc<RefCell<ConfigStore>>,
    ctx: ConnectCtx,
) {
    wire_tunnel_callbacks(
        window,
        handles.clone(),
        store.clone(),
        ctx.tab_statuses.clone(),
    );

    // --- Command bar (#55): run command + quick-command management ---------
    {
        let handles_rc = handles.clone();
        let store_rc = store.clone();
        let weak = window.as_weak();
        window.on_run_command(
            move |tab_id: SharedString, cmd: SharedString, to_all: bool| {
                let (history_line, bytes) = encode_command_bar_input(&cmd);
                {
                    let h = handles_rc.borrow();
                    if to_all {
                        for handle in h.values() {
                            handle.send_raw(bytes.clone());
                        }
                    } else if let Some(handle) = h.get(tab_id.as_str()) {
                        handle.send_raw(bytes);
                    }
                }
                if let Some(line) = history_line {
                    let mut s = store_rc.borrow_mut();
                    s.push_command_history(line);
                    let _ = s.save();
                    if let Some(w) = weak.upgrade() {
                        w.set_command_history(history_model(&s));
                    }
                }
            },
        );
    }
    // Copy a history command to the clipboard (#96).
    {
        window.on_copy_text(move |text: SharedString| {
            let t = text.to_string();
            std::thread::spawn(move || clipboard_set_text(t));
        });
    }
    // Delete a history entry (#96). The command-history model remains in
    // storage order, so this legacy row index still maps straight through.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        window.on_delete_history(move |i: i32| {
            {
                let mut s = store_rc.borrow_mut();
                let idx = i as usize;
                if idx < s.command_history().len() {
                    s.remove_command_history(idx);
                    let _ = s.save();
                }
            }
            if let Some(w) = weak.upgrade() {
                w.set_command_history(history_model(&store_rc.borrow()));
            }
        });
    }
    // History search (#101): filter the dropdown by a case-insensitive substring.
    // The current query is shared so a delete from a filtered view re-filters.
    let hist_query: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let hist_query = hist_query.clone();
        window.on_search_history(move |query: SharedString| {
            *hist_query.borrow_mut() = query.to_string();
            if let Some(w) = weak.upgrade() {
                set_history_view(&w, &store_rc.borrow(), &query);
            }
        });
    }
    // Delete a history entry by its command text (#101) — index-free so it works
    // from the filtered dropdown view.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let hist_query = hist_query.clone();
        window.on_delete_history_cmd(move |cmd: SharedString| {
            {
                let mut s = store_rc.borrow_mut();
                if let Some(idx) = s.command_history().iter().position(|c| c == cmd.as_str()) {
                    s.remove_command_history(idx);
                    let _ = s.save();
                }
            }
            if let Some(w) = weak.upgrade() {
                let s = store_rc.borrow();
                w.set_command_history(history_model(&s));
                set_history_view(&w, &s, &hist_query.borrow());
            }
        });
    }
    // Runtime-only collapse state for quick-command groups (#55) — like the
    // welcome session groups, this is not persisted across restarts. Starts with
    // every group collapsed (default-collapsed view).
    let collapsed_quick_groups: Rc<RefCell<std::collections::HashSet<String>>> =
        Rc::new(RefCell::new(all_quick_group_names(&store.borrow())));
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        window.on_add_quick_command(
            move |name: SharedString,
                  command: SharedString,
                  group: SharedString,
                  send_enter: bool| {
                let name = name.trim().to_string();
                let command = command.to_string();
                let group = group.trim().to_string();
                if name.is_empty() || command.trim().is_empty() {
                    return;
                }
                {
                    let mut s = store_rc.borrow_mut();
                    let mut v = s.quick_commands().to_vec();
                    v.push(crate::config::QuickCommand {
                        name,
                        command,
                        group,
                        send_enter,
                    });
                    s.set_quick_commands(v);
                    let _ = s.save();
                }
                if let Some(w) = weak.upgrade() {
                    w.set_quick_commands(quick_cmd_model(&store_rc.borrow(), &collapsed.borrow()));
                }
            },
        );
    }
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        window.on_delete_quick_command(move |index: i32| {
            {
                let mut s = store_rc.borrow_mut();
                let mut v = s.quick_commands().to_vec();
                let i = index as usize;
                if i < v.len() {
                    v.remove(i);
                }
                s.set_quick_commands(v);
                let _ = s.save();
            }
            if let Some(w) = weak.upgrade() {
                // Deleting the entry being edited resets the form; deleting one
                // above it shifts the index so Save still hits the right entry.
                let edit = edit_index_after_delete(w.get_qcm_edit_index(), index);
                if edit < 0 && w.get_qcm_edit_index() >= 0 {
                    w.set_qcm_name("".into());
                    w.set_qcm_command("".into());
                    w.set_qcm_group("".into());
                    w.set_qcm_send_enter(true);
                }
                w.set_qcm_edit_index(edit);
                w.set_quick_commands(quick_cmd_model(&store_rc.borrow(), &collapsed.borrow()));
            }
        });
    }
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        window.on_toggle_quick_group(move |group: SharedString| {
            let g = group.to_string();
            {
                let mut set = collapsed.borrow_mut();
                if !set.remove(&g) {
                    set.insert(g);
                }
            }
            if let Some(w) = weak.upgrade() {
                w.set_quick_commands(quick_cmd_model(&store_rc.borrow(), &collapsed.borrow()));
            }
        });
    }
    // Edit (#55): load the entry into the manage form in edit mode.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        window.on_edit_quick_command(move |index: i32| {
            let i = index as usize;
            let cmd = store_rc.borrow().quick_commands().get(i).cloned();
            if let (Some(c), Some(w)) = (cmd, weak.upgrade()) {
                w.set_qcm_name(c.name.into());
                w.set_qcm_command(c.command.into());
                w.set_qcm_group(c.group.into());
                w.set_qcm_send_enter(c.send_enter);
                w.set_qcm_edit_index(index);
                w.set_quick_cmd_manage_open(true);
            }
        });
    }
    // Save an edited entry (#55).
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        window.on_save_quick_command(
            move |index: i32,
                  name: SharedString,
                  command: SharedString,
                  group: SharedString,
                  send_enter: bool| {
                let name = name.trim().to_string();
                let command = command.to_string();
                let group = group.trim().to_string();
                if name.is_empty() || command.trim().is_empty() {
                    return;
                }
                {
                    let mut s = store_rc.borrow_mut();
                    s.update_quick_command(
                        index as usize,
                        crate::config::QuickCommand {
                            name,
                            command,
                            group,
                            send_enter,
                        },
                    );
                    let _ = s.save();
                }
                if let Some(w) = weak.upgrade() {
                    w.set_quick_commands(quick_cmd_model(&store_rc.borrow(), &collapsed.borrow()));
                }
            },
        );
    }
    // Duplicate (#55): clone the entry as a starting point.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        window.on_duplicate_quick_command(move |index: i32| {
            {
                let mut s = store_rc.borrow_mut();
                let mut v = s.quick_commands().to_vec();
                if let Some(c) = v.get(index as usize).cloned() {
                    let dup = crate::config::QuickCommand {
                        name: format!("{} (copy)", c.name),
                        command: c.command,
                        group: c.group,
                        send_enter: c.send_enter,
                    };
                    v.insert(index as usize + 1, dup);
                    s.set_quick_commands(v);
                    let _ = s.save();
                }
            }
            if let Some(w) = weak.upgrade() {
                // The copy lands right after `index`; later entries shift down.
                let edit = w.get_qcm_edit_index();
                if edit > index {
                    w.set_qcm_edit_index(edit + 1);
                }
                w.set_quick_commands(quick_cmd_model(&store_rc.borrow(), &collapsed.borrow()));
            }
        });
    }
    // Move to a group (#55): "default" maps to the empty (ungrouped) group.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        window.on_move_quick_command(move |index: i32, group: SharedString| {
            let target = group.to_string();
            let target = if target == "default" {
                String::new()
            } else {
                target
            };
            {
                let mut s = store_rc.borrow_mut();
                let mut v = s.quick_commands().to_vec();
                if let Some(c) = v.get_mut(index as usize) {
                    c.group = target;
                }
                s.set_quick_commands(v);
                let _ = s.save();
            }
            if let Some(w) = weak.upgrade() {
                w.set_quick_commands(quick_cmd_model(&store_rc.borrow(), &collapsed.borrow()));
            }
        });
    }
    // Reorder inside the current group (#310). The stored Vec remains the
    // source of truth; the grouped display model preserves this relative order.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        window.on_reorder_quick_command(move |index: i32, move_up: bool| {
            let swapped = {
                let mut s = store_rc.borrow_mut();
                let mut commands = s.quick_commands().to_vec();
                let swapped = reorder_quick_command(&mut commands, index as usize, move_up);
                if swapped.is_some() {
                    s.set_quick_commands(commands);
                    let _ = s.save();
                }
                swapped
            };
            if let Some(target) = swapped {
                if let Some(w) = weak.upgrade() {
                    // Keep the edit form bound to the entry it was loaded from.
                    w.set_qcm_edit_index(edit_index_after_swap(
                        w.get_qcm_edit_index(),
                        index as usize,
                        target,
                    ));
                    w.set_quick_commands(quick_cmd_model(&store_rc.borrow(), &collapsed.borrow()));
                }
            }
        });
    }
    // Quick-group create / rename (#55).
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        window.on_submit_quick_group(move |orig: SharedString, name: SharedString| {
            {
                let mut s = store_rc.borrow_mut();
                if orig.is_empty() {
                    s.add_quick_group(name.to_string());
                } else {
                    s.rename_quick_group(&orig.to_string(), name.to_string());
                }
                let _ = s.save();
            }
            if let Some(w) = weak.upgrade() {
                w.set_quick_commands(quick_cmd_model(&store_rc.borrow(), &collapsed.borrow()));
            }
        });
    }
    // Quick-group delete (#55) — UI only offers this on empty groups.
    {
        let store_rc = store.clone();
        let weak = window.as_weak();
        let collapsed = collapsed_quick_groups.clone();
        window.on_delete_quick_group(move |name: SharedString| {
            {
                let mut s = store_rc.borrow_mut();
                s.remove_quick_group(&name.to_string());
                let _ = s.save();
            }
            if let Some(w) = weak.upgrade() {
                w.set_quick_commands(quick_cmd_model(&store_rc.borrow(), &collapsed.borrow()));
            }
        });
    }

    // Session sync / broadcast input: when on, a keystroke in any terminal is
    // mirrored to every online session (Xshell-style; #78 pt.4). Read on the hot
    // keystroke path, so use an AtomicBool rather than a window-property lookup.
    let sync_input = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let flag = sync_input.clone();
        window.on_set_sync_input(move |on| {
            flag.store(on, std::sync::atomic::Ordering::Relaxed);
        });
    }

    // Forward each keystroke as raw bytes to the SSH PTY. The server's bash /
    // readline handles echo, history (↑↓), Tab completion, Ctrl+C, etc.
    {
        // Capture Slint's raw modifier mapping before app-shortcut routing.
        // WARN is deliberate: packaged builds persist WARN+ to error.log.
        window.on_diagnose_key_event(
            move |tab_id: SharedString,
                  key: SharedString,
                  raw_control: bool,
                  raw_meta: bool,
                  alt: bool,
                  shift: bool| {
                if cfg!(target_os = "macos")
                    && (raw_control
                        || raw_meta
                        || key.chars().any(|c| (0x10..=0x18).contains(&(c as u32))))
                {
                    tracing::warn!(
                        "[KEY_DIAG_312] stage=slint tab={} key={} raw_control={} raw_meta={} alt={} shift={}",
                        tab_id,
                        redact_key(key.as_str()),
                        raw_control,
                        raw_meta,
                        alt,
                        shift
                    );
                }
            },
        );

        let handles = handles.clone();
        let bufs = bufs.clone();
        let sync_input = sync_input.clone();
        // Shared timestamp: the last time the Shift key alone was pressed
        // (key="", shift=true).  Used by the time-based Backspace filter below.
        let last_shift_time: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
        window.on_send_key(move |tab_id: SharedString, key: SharedString, ctrl: bool, alt: bool, shift: bool| {
            // ── Enter on a disconnected tab → reconnect in place (#79) ──────
            // FinalShell-style: the tab shows "连接已断开,按 Enter 重新连接";
            // pressing Enter re-spawns the shell + SFTP workers in the SAME tab
            // with a fresh screen instead of forcing the user to open a new one.
            if key.as_str() == "\n" && !ctrl && !alt {
                let dead_session = {
                    let statuses = ctx.tab_statuses.lock().unwrap();
                    statuses
                        .get(tab_id.as_str())
                        .filter(|st| st.state == 2)
                        .map(|st| st.session_id.clone())
                };
                if let Some(session_id) = dead_session {
                    let Some(session) = store.borrow().get(&session_id).cloned() else {
                        return;
                    };
                    // Drop the dead shell/SFTP handles for this tab.
                    ctx.handles.borrow_mut().remove(tab_id.as_str());
                    if let Some(h) =
                        ctx.sftp_handles.lock().unwrap().remove(tab_id.as_str())
                    {
                        h.close();
                    }
                    // Fresh screen: new parser, cleared history/selection.
                    {
                        if let Some(h) = term_buf(&ctx.bufs, tab_id.as_str()) {
                            let mut b = h.lock().unwrap();

                            let is_alt = b.parser.screen().alternate_screen();

                            if is_alt {
                                b.ingest("\x1b[?1049l".to_string().as_bytes());
                            }

                            let (rows, cols) = b.parser.screen().size();
                            let cursor_row = b.parser.screen().cursor_position().0;

                            let curr: Vec<crate::terminal::Line> = {
                                let s = b.parser.screen();
                                (0..rows).map(|r| crate::terminal::build_row(s, r, cols)).collect()
                            };

                            let k = cursor_row as usize;
                            for line in curr.iter().take(k) {
                                b.history.push_back(line.clone());
                            }
                            while b.history.len() > crate::terminal::MAX_HISTORY {
                                b.history.pop_front();
                            }
                            b.view_offset = 0;

                            b.prev = Vec::new();
                            b.displayed_text = Vec::new();
                            b.csi_state = CsiState::Normal;
                            b.csi_pending = Vec::new();
                            b.charset = crate::terminal::CharsetTracker::default();
                            b.mouse_tracked = false;

                            b.parser = vt100::Parser::new(rows, cols, 5000);

                            if let Some(log) = b.session_log.as_mut() {
                                log.note("reconnecting");
                            }
                        }
                    }
                    if let Some(st) =
                        ctx.tab_statuses.lock().unwrap().get_mut(tab_id.as_str())
                    {
                        st.state = 0;
                    }
                    // Fresh session: the first OSC 7 after reconnect follows.
                    ctx.sftp_last_cwd.lock().unwrap().remove(tab_id.as_str());
                    if let Some(w) = ctx.weak.upgrade() {
                        set_terminal_row(&w, tab_id.as_str(), |t| {
                            t.status =
                                crate::i18n::t("重连中...", "Reconnecting...").into();
                        });
                    }
                    start_session_in_tab(tab_id.as_str(), session, &ctx);
                    return;
                }
            }
            // Check whether the remote PTY switched to application cursor mode
            // (DECCKM, set by nano/vim via \x1b[?1h). In that mode the terminal
            // must send \x1bOA/B/C/D instead of \x1b[A/B/C/D.
            let app_cursor = if let Some(h) = term_buf(&bufs, tab_id.as_str()) {
                let mut b = h.lock().unwrap();
                // Typing snaps the view back to the live bottom so the
                // user always sees what they're entering.
                let cursor_row = b.parser.screen().cursor_position().0 as usize + b.view_offset;
                let rows = b.parser.screen().size().0 as usize;
                if cursor_row >= rows {
                    b.view_offset = 0;
                }
                b.parser.screen().application_cursor()
            } else {
                false
            };
            // Never log the raw key string — it can be a password character
            // (#15). redact_key keeps control codes but masks printable text.
            tracing::debug!(
                "send_key tab={} key={} ctrl={} alt={} shift={} app_cursor={}",
                tab_id, redact_key(key.as_str()), ctrl, alt, shift, app_cursor
            );

            // ── Shift / Backspace 诊断日志 (info 级, 无需 RUST_LOG=debug) ─────
            // 每个 Shift 相关事件都打印 key 的 Unicode 码位，方便对比
            // 左Shift / 右Shift 是否产生不同的 key 字符串。
            if shift || key.as_str() == "\u{0008}" {
                // INFO level (no RUST_LOG needed) — must not leak the key text.
                // redact_key reveals only control code points (the IME markers
                // this diagnostic cares about), masking any printable char that
                // could be part of a Shift-typed password symbol (#15).
                let codepoints = redact_key(key.as_str());
                let elapsed_ms = last_shift_time
                    .lock()
                    .unwrap()
                    .map(|t| format!("{}ms ago", t.elapsed().as_millis()))
                    .unwrap_or_else(|| "never".to_string());
                tracing::info!(
                    "[KEY_DIAG] key={} shift={} ctrl={} alt={} | last_shift={}",
                    codepoints, shift, ctrl, alt, elapsed_ms
                );
            }

            // ── Track lone-Shift presses for the time-based Backspace filter ──
            // Slint sends key="" (empty string) when a bare modifier key (Shift,
            // Ctrl, Alt) is pressed.  We record the timestamp whenever Shift
            // alone fires so the filter below can catch IME-injected Backspace
            // events even if they arrive with shift=false.
            if key.as_str().is_empty() && shift && !ctrl && !alt {
                *last_shift_time.lock().unwrap() = Some(std::time::Instant::now());
                tracing::info!("[KEY_DIAG] lone-Shift recorded → timestamp saved");
            }

            // ── 拦截百度拼音注入的 Shift 标记字符（核心修复）────────────────────
            // 诊断日志证实，百度拼音通过 WH_KEYBOARD_LL 钩子，在 Shift 键按下时
            // 向消息队列注入一个 C0 控制字符，而非空字符串：
            //
            //   左 Shift → U+0015 (Ctrl+U / NAK), shift=true, ctrl=false
            //   右 Shift → U+0010 (Ctrl+P / DLE), shift=true, ctrl=false
            //              紧接着注入: U+0008 (Backspace), shift=false
            //
            // 这些字符绝对不应送入 PTY：
            //   0x15 (Ctrl+U) 在 bash/vim 中会清空当前输入行 → "左Shift替换字符"
            //   0x10 (Ctrl+P) 在 vim 中翻历史/触发补全     → "右Shift乱跳"
            //   0x08 (Backspace) 紧随其后                   → "右Shift删除字符"
            //
            // 合法独立 C0 键（Backspace=0x08, Tab=0x09, LF=0x0A, CR=0x0D,
            // ESC=0x1B）不受此过滤影响，由下方代码单独处理。
            //
            // 检测到 IME Shift 标记后，记录时间戳，让 Layer 2 在 1500ms 内
            // 拦截随后可能到来的 Backspace（右Shift场景，日志显示间隔约 914ms）。
            // Shift+Tab → back-tab (ESC [ Z). Slint's Key.Backtab is U+0019,
            // which the IME C0-marker filter below would otherwise drop.
            let back_tab = is_back_tab(key.as_str(), ctrl, alt, shift);
            if !ctrl && !alt && !back_tab {
                if let Some(c) = key.as_str().chars().next() {
                    let cp = c as u32;
                    let is_standalone = matches!(cp, 0x08 | 0x09 | 0x0A | 0x0D | 0x1B)
                        || is_terminal_interrupt(key.as_str());
                    if key.as_str().chars().count() == 1
                        && (0x01..=0x1f).contains(&cp)
                        && !is_standalone
                    {
                        *last_shift_time.lock().unwrap() = Some(std::time::Instant::now());
                        tracing::info!(
                            "[KEY_DIAG] DROPPED IME C0 marker U+{:04X} (shift={}) → timestamp saved",
                            cp, shift
                        );
                        return;
                    }
                }
            }

            // ── Windows: filter synthetic Ctrl+char injections ──────────────
            // Some keyboards / IME drivers (e.g. Aula F99 + Baidu Pinyin)
            // inject a synthetic WM_CHAR 0x11 (Ctrl+Q) when Left Ctrl is
            // briefly tapped, WITHOUT sending a WM_KEYDOWN VK_Q beforehand.
            //
            // FinalShell avoids this because it builds Ctrl+letter from
            // WM_KEYDOWN (virtual-key codes).  Slint uses WM_CHAR, so it
            // sees the injected byte and forwards it straight to us.
            //
            // Fix: for C0 control chars (Ctrl+A…Ctrl+Z, i.e. 0x01–0x1A),
            // use GetKeyState — which returns the key state *as of the last
            // processed message*, not the live hardware state — to verify
            // the corresponding letter VK was actually queued as a keydown
            // before this WM_CHAR arrived.  If Q was never keyed down,
            // GetKeyState(VK_Q) = 0 → the event is synthetic → drop it.
            #[cfg(windows)]
            if ctrl {
                if let Some(ch) = key.as_str().chars().next() {
                    let cp = ch as u32;
                    // Always let Enter / Tab pass through regardless of Ctrl
                    // state.  These C0 codes (0x09 Tab, 0x0a LF, 0x0d CR) are
                    // "double-duty" keys: pressing Enter while Ctrl is still
                    // physically held (e.g. just after Ctrl+O in nano) generates
                    // Ctrl+M (0x0d) with ctrl=true — but GetKeyState(VK_M) is 0
                    // because the user never pressed M.  Without this exemption
                    // the filter would silently drop the Enter, making it
                    // impossible to confirm nano's "File Name to Write:" prompt.
                    let always_pass = matches!(cp, 0x09 | 0x0a | 0x0d)
                        || is_terminal_interrupt(key.as_str());
                    if !always_pass
                        && key.as_str().chars().count() == 1
                        && (0x01..=0x1a).contains(&cp)
                        && !c0_letter_key_down(cp)
                    {
                        tracing::debug!(
                            "send_key: dropped synthetic Ctrl+{} \
                             (VK_{:02X} not down per GetKeyState)",
                            (0x40u8 + cp as u8) as char,
                            cp + 0x40
                        );
                        return;
                    }
                }
            }

            // ── Filter synthetic Backspace injected by Chinese IME ────────────
            // Baidu Pinyin (and similar Chinese IMEs) hooks the keyboard at the
            // driver level via WH_KEYBOARD_LL, below Win32's ImmDisableIME.
            // When the user presses Shift to switch from Chinese to English mode
            // while a pinyin syllable is in-flight, the IME:
            //   1. Cancels the composition (discards the syllable).
            //   2. Posts WM_KEYDOWN VK_BACK + WM_CHAR 0x08 to erase whatever
            //      character it had already forwarded to the app.
            //
            // Two-layer defence:
            //
            //   Layer 1 – shift=true guard.
            //     The synthetic Backspace arrives during Shift keydown, so
            //     GetKeyState(VK_SHIFT) is still "down" → Slint reports shift=true.
            //     Drop any Backspace (0x08) arriving while Shift is flagged.
            //
            //   Layer 2 – time-based guard.
            //     Baidu Pinyin posts WM_CHAR 0x08 asynchronously, so by the time
            //     the message is dequeued Shift may already read as "up"
            //     → shift=false defeats Layer 1.
            //     Mitigation: we recorded the timestamp when the Shift key alone
            //     was pressed (key="", shift=true) a few lines above. Drop a
            //     Backspace arriving within the guarded interval unless a real
            //     intervening key has already cleared the marker.
            // Any real intervening key proves a previous Shift/IME marker is no
            // longer paired with this Backspace. Without clearing it, the broad
            // safety window drops legitimate Vim insert-mode Backspace (#319).
            if key.as_str() != "\u{0008}" && !key.as_str().is_empty() {
                *last_shift_time.lock().unwrap() = None;
            }

            if key.as_str() == "\u{0008}" && !ctrl && !alt {
                // Layer 1
                if shift {
                    tracing::info!("[KEY_DIAG] Backspace DROPPED by layer-1 (shift=true)");
                    return;
                }
                // Layer 2 — 时间窗口 1500ms
                // 日志显示百度拼音注入 U+0010(右Shift标记) 到 Backspace 之间
                // 间隔约 914ms，因此窗口设为 1500ms 以覆盖该场景。
                let (shift_just_pressed, elapsed_ms) = {
                    let guard = last_shift_time.lock().unwrap();
                    match *guard {
                        Some(t) => {
                            let ms = t.elapsed().as_millis();
                            (ms < 1500, ms)
                        }
                        None => (false, 0),
                    }
                };
                if shift_just_pressed {
                    tracing::info!(
                        "[KEY_DIAG] Backspace DROPPED by layer-2 ({}ms after IME Shift marker)",
                        elapsed_ms
                    );
                    return;
                }
                // Layer 3
                // Do not consult the live VK_BACK state here. Under UI/SSH
                // backlog the key-up can be processed before this callback, so
                // that test drops a genuine queued Backspace (#319).
                tracing::info!("[KEY_DIAG] Backspace PASSED all filters → sent to PTY");
            }

            if should_drop_bare_ctrl_marker(
                key.as_str(),
                ctrl,
                bare_ctrl_marker_workaround_enabled(),
            ) || should_drop_macos_bare_ctrl_marker(key.as_str(), ctrl, cfg!(target_os = "macos"))
            {
                tracing::debug!(
                    "send_key: dropped Slint bare Ctrl modifier marker {}",
                    redact_key(key.as_str())
                );
                if cfg!(target_os = "macos") {
                    tracing::warn!(
                        "[KEY_DIAG_312] stage=filter tab={} key={} ctrl={} alt={} shift={} action=drop_bare_marker",
                        tab_id, redact_key(key.as_str()), ctrl, alt, shift
                    );
                }
                return;
            }

            let bytes = if back_tab {
                BACK_TAB_BYTES.to_vec()
            } else {
                key_to_pty_bytes(key.as_str(), ctrl, alt, app_cursor)
            };
            if cfg!(target_os = "macos")
                && (ctrl || key.chars().any(|c| (0x10..=0x18).contains(&(c as u32))))
            {
                tracing::warn!(
                    "[KEY_DIAG_312] stage=pty tab={} key={} ctrl={} alt={} shift={} app_cursor={} encoded={} action={}",
                    tab_id,
                    redact_key(key.as_str()),
                    ctrl,
                    alt,
                    shift,
                    app_cursor,
                    redact_key(&String::from_utf8_lossy(&bytes)),
                    if bytes.is_empty() { "drop_empty" } else { "send" }
                );
            }
            // Log only the length — never the keystroke bytes, which can be
            // password characters (#15).
            tracing::debug!(
                "send_key len={} handle_exists={}",
                bytes.len(),
                handles.borrow().contains_key(tab_id.as_str()),
            );
            if !bytes.is_empty() {
                let h = handles.borrow();
                if sync_input.load(std::sync::atomic::Ordering::Relaxed) {
                    // Broadcast the same bytes to every online session (#78 pt.4).
                    for (target_id, handle) in h.iter() {
                        if let Some(buffer) = term_buf(&bufs, target_id) {
                            buffer.lock().unwrap().interactive_echo_until =
                                std::time::Instant::now() + INTERACTIVE_ECHO_WINDOW;
                        }
                        handle.send_raw(bytes.clone());
                    }
                } else if let Some(handle) = h.get(tab_id.as_str()) {
                    if let Some(buffer) = term_buf(&bufs, tab_id.as_str()) {
                        buffer.lock().unwrap().interactive_echo_until =
                            std::time::Instant::now() + INTERACTIVE_ECHO_WINDOW;
                    }
                    handle.send_raw(bytes);
                }
            }
        });
    }

    // Propagate PTY resize to the SSH worker and vt100 parser. Pixel
    // dimensions come from Slint; we approximate col/row counts using
    // Consolas 13px metrics.
    //
    // terminal_view.slint now passes the FocusScope height (not the full
    // TerminalView height), so the SFTP panel is already excluded.
    // Layout breakdown for the FocusScope:
    //   16 px  – bottom strip (TouchArea for focus-regain)
    //    8 px  – y-offset of the output Text element inside the Flickable
    // = 24 px  total vertical chrome within FocusScope
    //
    // Consolas 13 px renders at ≈ 8 px wide × 16 px tall per cell.
    {
        let handles = handles.clone();
        let bufs_resize = bufs.clone(); // keep bufs alive for the copy handler below
        let weak_resize = window.as_weak();
        // The Slint side now measures the real Consolas cell size (via a hidden
        // probe Text) and passes whole column/row counts directly, so there is
        // no pixel→cell guesswork here.  This keeps full-screen programs like
        // nano from over-counting rows and clipping their bottom shortcut bar.
        // Debounce PTY resizes (#163): a layout reflow (a tab becoming visible,
        // the SFTP panel docking, a window drag) can momentarily report a
        // near-zero width, which collapses term-cols to its 10-col floor.
        // Applying that to the remote PTY immediately resizes the server to 10
        // columns and reflows vt100 — garbling running output (e.g. a `git clone`
        // progress meter wraps at 10 chars). Coalesce rapid changes and apply
        // only the size that's still set after a short quiet period, so a
        // transient bad value never reaches the server.
        let pending_size: Rc<RefCell<HashMap<String, (u32, u32)>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let resize_debounce = Rc::new(slint::Timer::default());
        window.on_terminal_resize(move |tab_id: SharedString, cols_f: f32, rows_f: f32| {
            // A hidden terminal (inactive tab, or a split sibling not currently
            // shown) reports 0 width/height. Ignore those: flooring 0 to the 10-col
            // minimum and applying it would shrink that tab's PTY *and* poison
            // `last_term_size`, so the next connection (e.g. "Duplicate connection")
            // would start at 10 cols and wrap its first output to ~10 chars (#v0.5).
            // Only genuine, visible sizes drive a resize.
            if cols_f < 1.0 || rows_f < 1.0 {
                return;
            }
            let cols = (cols_f as u32).max(10);
            let rows = (rows_f as u32).max(5);
            pending_size
                .borrow_mut()
                .insert(tab_id.to_string(), (cols, rows));
            let pending = pending_size.clone();
            let handles = handles.clone();
            let bufs = bufs_resize.clone();
            let last = last_term_size.clone();
            let weak = weak_resize.clone();
            // (Re)arm the single-shot timer; rapid changes keep resetting it so
            // only the final, settled size is applied.
            resize_debounce.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(150),
                move || {
                    let settled: Vec<(String, (u32, u32))> = pending.borrow_mut().drain().collect();
                    for (tab, (cols, rows)) in settled {
                        tracing::debug!("terminal_resize tab={} cols={} rows={}", tab, cols, rows);
                        apply_terminal_resize(&handles, &bufs, &last, &tab, cols, rows);
                        // Re-render so the reflowed (or resized) grid shows at once
                        // instead of waiting for the next remote output (#169).
                        if let Some(win) = weak.upgrade() {
                            rebuild_tab_display(&win, &bufs, &tab);
                        }
                    }
                },
            );
        });
    }

    // Ctrl+Shift+C: copy current terminal screen to clipboard.
    {
        let bufs = bufs.clone();
        window.on_copy_terminal_text(move |tab_id: SharedString| {
            let text = term_buf(&bufs, tab_id.as_str())
                .map(|h| {
                    let buf = h.lock().unwrap();
                    // Copy the drag-selection when there is one, else the
                    // whole displayed screen.
                    let sel = buf.extract_selection_text();
                    if sel.is_empty() {
                        buf.displayed_text.join("\n")
                    } else {
                        sel
                    }
                })
                .unwrap_or_default();
            // Run the clipboard write on a dedicated OS thread.  arboard's
            // Windows backend opens the clipboard and pumps Win32 messages;
            // doing that on the Slint/winit event-loop thread re-enters the
            // message loop and dead-locks the whole UI.
            std::thread::spawn(move || clipboard_set_text(text));
        });
    }

    // Middle-click / Ctrl+Shift+V: paste clipboard text into PTY.
    //
    // Full clipboard payload for an open multi-line review dialog lives only
    // in `pending_paste` (not in a Slint string property): the software
    // renderer panics when a huge Text layout overflows i16 coordinates
    // (#434 / slint#12985).
    let pending_paste: Arc<PendingPaste> = Arc::new(Mutex::new(None));
    {
        let handles = handles.clone();
        let bufs = bufs.clone();
        let weak = window.as_weak();
        let pending_paste = pending_paste.clone();
        window.on_paste_from_clipboard(move |tab_id: SharedString| {
            // Clone the (Send) command sender for this tab so the clipboard read
            // can run off the UI thread.  Reading arboard on the event-loop
            // thread is what froze the app on middle-click / paste — see the
            // copy handler above for the deadlock explanation.
            let sender = handles
                .borrow()
                .get(tab_id.as_str())
                .map(|h| h.commands.clone());
            let Some(sender) = sender else { return };
            let bracketed = terminal_uses_bracketed_paste(&bufs, tab_id.as_str());
            let confirm_multiline = weak
                .upgrade()
                .map(|w| w.get_paste_confirm_enabled())
                .unwrap_or(true);
            let weak = weak.clone();
            let pending_paste = pending_paste.clone();
            let tab_id = tab_id.to_string();
            std::thread::spawn(move || {
                match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
                    Ok(text) => {
                        let force_review = text.len() > 100 * 1024;
                        if text.contains(['\r', '\n']) && (confirm_multiline || force_review) {
                            let large = paste_requires_large_review(&text);
                            // Only a bounded preview enters the UI tree. Confirm
                            // reads the full payload via take_pending_paste.
                            let preview = store_pending_paste(&pending_paste, tab_id.clone(), text);
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(w) = weak.upgrade() {
                                    w.set_paste_confirm_tab(tab_id.into());
                                    w.set_paste_confirm_preview(preview.into());
                                    w.set_paste_confirm_large(large);
                                    w.set_paste_confirm_open(true);
                                }
                            });
                        } else {
                            let bytes = encode_pasted_text(&text, bracketed);
                            let _ = sender.send(SessionCommand::RawInput(bytes));
                        }
                    }
                    Err(e) => tracing::warn!("paste_from_clipboard: clipboard error: {}", e),
                }
            });
        });
    }

    // Accept a previously reviewed multi-line paste (#262).
    {
        let handles_paste = handles.clone();
        let bufs_paste = bufs.clone();
        let pending_paste = pending_paste.clone();
        let weak = window.as_weak();
        window.on_paste_confirmed(move |tab_id: SharedString| {
            let Some(sender) = handles_paste
                .borrow()
                .get(tab_id.as_str())
                .map(|h| h.commands.clone())
            else {
                return;
            };
            let text = take_pending_paste(&pending_paste, tab_id.as_str());
            if let Some(w) = weak.upgrade() {
                w.set_paste_confirm_open(false);
            }
            let Some(text) = text else {
                tracing::warn!("paste_confirmed: no pending paste payload for tab {tab_id}");
                return;
            };
            let bracketed = terminal_uses_bracketed_paste(&bufs_paste, tab_id.as_str());
            let _ = sender.send(SessionCommand::RawInput(encode_pasted_text(
                &text, bracketed,
            )));
        });
    }

    {
        let pending_paste = pending_paste.clone();
        window.on_paste_confirm_cancelled(move || {
            clear_pending_paste(&pending_paste);
        });
    }

    // Context menu → 清空缓存: reset the local vt100 buffer (drops scrollback),
    // wipe the displayed screen, then nudge the remote to redraw a fresh prompt.
    {
        let bufs_clear = bufs.clone();
        let handles_clear = handles.clone();
        let weak = window.as_weak();
        window.on_clear_terminal(move |tab_id: SharedString| {
            let tid = tab_id.to_string();
            if let Some(h) = term_buf(&bufs_clear, &tid) {
                let mut buf = h.lock().unwrap();
                buf.release_scrollback();
            }
            if let Some(win) = weak.upgrade() {
                set_terminal_row(&win, &tid, |row| {
                    row.spans = ModelRc::from(Rc::new(VecModel::<TermSpan>::default()));
                    row.find_matches = ModelRc::from(Rc::new(VecModel::<TermMatch>::default()));
                    row.selection = ModelRc::from(Rc::new(VecModel::<TermMatch>::default()));
                    row.cursor_row = 0;
                    row.cursor_col = 0;
                    row.rows_used = 0;
                    row.scroll_max = 0;
                    row.scroll_offset = 0;
                });
            }
            if let Some(h) = handles_clear.borrow().get(&tid) {
                h.send_raw(vec![0x0c]); // Ctrl+L → shell clears + redraws prompt
            }
        });
    }

    // Context menu → 查找: store the query and recompute highlight rectangles.
    {
        let bufs_find = bufs.clone();
        let weak = window.as_weak();
        window.on_find_query_changed(move |tab_id: SharedString, query: SharedString| {
            let tid = tab_id.to_string();
            let q = query.to_string();
            let (matches, jumped) = with_term_buf(&bufs_find, &tid, |buf| {
                buf.find_query = q.clone();
                let mut matches = compute_find_matches(&buf.displayed_text, &q);
                let jumped = matches.is_empty() && buf.scroll_to_first_find_match(&q);
                if jumped {
                    buf.render();
                    matches = compute_find_matches(&buf.displayed_text, &q);
                }
                (matches, jumped)
            })
            .unwrap_or_default();
            if let Some(win) = weak.upgrade() {
                if jumped {
                    rebuild_tab_display(&win, &bufs_find, &tid);
                    return;
                }
                let model = ModelRc::from(Rc::new(VecModel::from(matches)));
                set_terminal_row(&win, &tid, |row| {
                    row.find_matches = model.clone();
                });
            }
        });
    }

    // Home / End / PageUp / PageDown while viewing normal-screen history.
    // Return false unless the buffer is already scrolled back; Slint then
    // forwards the original key to the PTY for live terminals and TUI apps.
    {
        let bufs_scrollback_key = bufs.clone();
        let weak = window.as_weak();
        window.on_terminal_scrollback_key(move |tab_id: SharedString, key: SharedString| {
            let key = match key.as_str() {
                "\u{F729}" => ScrollbackKey::Home,
                "\u{F72B}" => ScrollbackKey::End,
                "\u{F72C}" => ScrollbackKey::PageUp,
                "\u{F72D}" => ScrollbackKey::PageDown,
                _ => return false,
            };
            let tid = tab_id.to_string();
            let handled = with_term_buf(&bufs_scrollback_key, &tid, |buf| {
                handle_scrollback_key(buf, key)
            })
            .unwrap_or(false);
            if handled {
                if let Some(win) = weak.upgrade() {
                    rebuild_tab_display(&win, &bufs_scrollback_key, &tid);
                }
            }
            handled
        });
    }

    // Mouse-wheel → scroll the scrollback history.
    {
        let bufs_scroll = bufs.clone();
        let weak = window.as_weak();
        window.on_terminal_scroll(move |tab_id: SharedString, delta: f32| {
            let tid = tab_id.to_string();
            let changed = with_term_buf(&bufs_scroll, &tid, |buf| {
                // Scroll within our own session scrollback (history lines above
                // the live screen).  Offset 0 = live bottom.
                //
                // `delta` is fractional lines (wheel pixels / cell height). The
                // integer part moves the offset; the fraction stays banked in
                // `scroll_accum` so a decaying macOS momentum tail converges to
                // a stop instead of repeating full-line steps. Cap each event
                // so a stray huge pixel delta can't teleport the view.
                let max_off = buf.history.len() as i64;
                buf.scroll_accum += delta.clamp(-24.0, 24.0);
                let whole = buf.scroll_accum.trunc();
                if whole != 0.0 {
                    buf.scroll_accum -= whole;
                }
                let cur = buf.view_offset as i64;
                let raw_new = cur + whole as i64;
                let new_off = raw_new.clamp(0, max_off);
                // Only a step that was actually CLIPPED by a boundary drops the
                // banked remainder (stale fractions must not fire after a
                // direction flip or fresh output). Resting AT a boundary must
                // keep banking: clearing whenever `new_off` merely equals a
                // boundary wiped the accumulation every tick and made slow
                // scrolling dead until it was fast enough to cross a whole line
                // per event.
                if new_off != raw_new {
                    buf.scroll_accum = 0.0;
                }
                let changed = new_off != cur;
                buf.view_offset = new_off as usize;
                changed
            });
            // Scrolling at a boundary (or an empty scrollback) leaves the offset
            // unchanged; skip the full rebuild since the screen is identical.
            if changed == Some(true) {
                if let Some(win) = weak.upgrade() {
                    rebuild_tab_display(&win, &bufs_scroll, &tid);
                }
            }
        });
    }

    // Wheel inside an alt-screen program (tmux / less / vim): forward it to the PTY
    // so the program scrolls, instead of doing nothing (#170 — FinalShell /
    // MobaXterm behave this way). If the app is tracking the mouse (e.g. tmux with
    // `mouse on`), send a real wheel mouse-event in the encoding it asked for;
    // otherwise fall back to arrow keys (xterm "alternate scroll"), which scrolls
    // less / man / vim.
    {
        let bufs_wheel = bufs.clone();
        let handles_wheel = handles.clone();
        window.on_terminal_wheel(move |tab_id: SharedString, dir: i32, col: i32, row: i32| {
            let tid = tab_id.to_string();
            let bytes = term_buf(&bufs_wheel, &tid).map(|h| {
                let buf = h.lock().unwrap();
                let screen = buf.parser.screen();
                if screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None {
                    // 1-based cell under the cursor, clamped to the screen.
                    let (rows, cols) = screen.size();
                    let c = (col.clamp(0, cols.saturating_sub(1) as i32) as u16) + 1;
                    let r = (row.clamp(0, rows.saturating_sub(1) as i32) as u16) + 1;
                    let btn: u16 = if dir > 0 { 64 } else { 65 }; // wheel up / down
                    if screen.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr {
                        format!("\x1b[<{btn};{c};{r}M").into_bytes()
                    } else {
                        // Legacy X10 encoding: ESC [ M  Cb Cx Cy  (each value + 32).
                        let cb = (btn + 32) as u8;
                        let cx = (c.min(223) + 32) as u8;
                        let cy = (r.min(223) + 32) as u8;
                        vec![0x1b, b'[', b'M', cb, cx, cy]
                    }
                } else {
                    // alternate-scroll: 3 arrow presses per notch, app-cursor aware.
                    let one: &[u8] = if dir > 0 {
                        if screen.application_cursor() {
                            b"\x1bOA"
                        } else {
                            b"\x1b[A"
                        }
                    } else if screen.application_cursor() {
                        b"\x1bOB"
                    } else {
                        b"\x1b[B"
                    };
                    one.repeat(3)
                }
            });
            if let (Some(bytes), Some(h)) = (bytes, handles_wheel.borrow().get(&tid)) {
                h.send_raw(bytes);
            }
        });
    }

    // Scrollbar drag → jump to an absolute scrollback offset (#103).
    {
        let bufs_scroll = bufs.clone();
        let weak = window.as_weak();
        window.on_terminal_scroll_to(move |tab_id: SharedString, offset: i32| {
            let tid = tab_id.to_string();
            let changed = with_term_buf(&bufs_scroll, &tid, |buf| {
                let max_off = buf.history.len() as i64;
                let new_off = (offset as i64).clamp(0, max_off);
                let changed = new_off != buf.view_offset as i64;
                buf.view_offset = new_off as usize;
                changed
            });
            // Same guard as on_terminal_scroll: an unchanged clamped offset
            // must not pay for a full grid rebuild.
            if changed == Some(true) {
                if let Some(win) = weak.upgrade() {
                    rebuild_tab_display(&win, &bufs_scroll, &tid);
                }
            }
        });
    }

    // Drag-selection lifecycle.
    {
        let bufs_sel = bufs.clone();
        let weak = window.as_weak();
        window.on_term_select_start(
            move |tab_id: SharedString, row: i32, col: i32, ctrl: bool, shift: bool| {
                let tid = tab_id.to_string();
                with_term_buf(&bufs_sel, &tid, |buf| {
                    let (rows, cols) = buf.parser.screen().size();
                    let r = row.clamp(0, rows.saturating_sub(1) as i32) as u16;
                    let c = col.clamp(0, cols.saturating_sub(1) as i32) as u16;
                    // Anchor + focus in absolute scrollback coordinates.
                    let abs = buf.vis_to_abs(r);
                    let point = (abs, c);
                    if ctrl && !shift {
                        buf.sel_ranges.push((point, point));
                    } else if shift && !buf.sel_ranges.is_empty() {
                        let anchor = buf.sel_ranges.last().map(|range| range.0).unwrap_or(point);
                        if let Some(range) = buf.sel_ranges.last_mut() {
                            *range = (anchor, point);
                        }
                    } else {
                        buf.sel_ranges.clear();
                        buf.sel_ranges.push((point, point));
                    }
                    let (anchor, focus) = buf.sel_ranges.last().copied().unwrap_or((point, point));
                    buf.sel_anchor = Some(anchor);
                    buf.sel_focus = Some(focus);
                });
                if let Some(win) = weak.upgrade() {
                    refresh_terminal_selection(&win, &bufs_sel, &tid);
                }
            },
        );
    }
    {
        let bufs_sel = bufs.clone();
        let weak = window.as_weak();
        window.on_term_select_update(move |tab_id: SharedString, row: i32, col: i32| {
            let tid = tab_id.to_string();
            with_term_buf(&bufs_sel, &tid, |buf| {
                let (rows, cols) = buf.parser.screen().size();
                let r = row.clamp(0, rows.saturating_sub(1) as i32) as u16;
                let c = col.clamp(0, cols.saturating_sub(1) as i32) as u16;
                if buf.sel_anchor.is_some() {
                    let abs = buf.vis_to_abs(r);
                    buf.sel_focus = Some((abs, c));
                    if let Some(range) = buf.sel_ranges.last_mut() {
                        range.1 = (abs, c);
                    }
                }
            });
            if let Some(win) = weak.upgrade() {
                refresh_terminal_selection(&win, &bufs_sel, &tid);
            }
        });
    }
    {
        let bufs_sel = bufs.clone();
        let weak = window.as_weak();
        window.on_term_select_end(move |tab_id: SharedString| {
            let tid = tab_id.to_string();
            // Extract the selected text; a zero-area selection (a plain click)
            // is cleared instead of copied.
            let text = with_term_buf(&bufs_sel, &tid, |buf| {
                // Selection endpoints are inclusive, so extracting an
                // anchor-only range returns the character under a plain click.
                // Compare coordinates instead of using extracted text as the
                // click-vs-drag signal (#319).
                if !buf.selection_has_extent() {
                    buf.sel_anchor = None;
                    buf.sel_focus = None;
                    buf.sel_ranges.clear();
                    return None;
                }
                let extracted = buf.extract_selection_text();
                if extracted.is_empty() {
                    // Zero-area selection (a plain click) → clear it.
                    buf.sel_anchor = None;
                    buf.sel_focus = None;
                    buf.sel_ranges.clear();
                    None
                } else {
                    Some(extracted)
                }
            })
            .flatten();
            match text {
                Some(t) if !t.is_empty() => {
                    // Auto-copy on release (select-to-copy, PuTTY style).
                    std::thread::spawn(move || clipboard_set_text(t));
                }
                _ => {}
            }
            if let Some(win) = weak.upgrade() {
                refresh_terminal_selection(&win, &bufs_sel, &tid);
            }
        });
    }
    {
        let bufs_sel = bufs.clone();
        let weak = window.as_weak();
        window.on_term_select_word(move |tab_id: SharedString, row: i32, col: i32| {
            let tid = tab_id.to_string();
            let text = with_term_buf(&bufs_sel, &tid, |buf| {
                let (rows, cols) = buf.parser.screen().size();
                let row = row.clamp(0, rows.saturating_sub(1) as i32) as u16;
                let col = col.clamp(0, cols.saturating_sub(1) as i32) as u16;
                buf.select_word_at(row, col)
            })
            .flatten();
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                std::thread::spawn(move || clipboard_set_text(text));
            }
            if let Some(win) = weak.upgrade() {
                refresh_terminal_selection(&win, &bufs_sel, &tid);
            }
        });
    }
    // Auto-scroll while drag-selecting past the visible top/bottom edge.  The
    // anchor is in absolute coordinates so it stays pinned no matter how far the
    // view moves; we only advance the scrollback view and re-point the focus at
    // the absolute row now sitting on the edge the mouse is parked against.
    {
        let bufs_sel = bufs.clone();
        let weak = window.as_weak();
        window.on_term_select_autoscroll(move |tab_id: SharedString, dir: i32| {
            let tid = tab_id.to_string();
            let Some(h) = term_buf(&bufs_sel, &tid) else {
                return;
            };
            {
                let mut buf = h.lock().unwrap();
                // No scrollback on the alternate screen (vim/btop own the view).
                if buf.parser.screen().alternate_screen() {
                    return;
                }
                if buf.sel_anchor.is_none() {
                    return;
                }
                let rows = buf.parser.screen().size().0;
                let last = rows.saturating_sub(1);
                let max_off = buf.history.len();
                let step = 2usize;
                // Keep the focus column the user last dragged to.
                let focus_col = buf.sel_focus.map(|f| f.1).unwrap_or(0);
                let edge_vis = if dir < 0 {
                    // Mouse above the top → reveal older lines.
                    let new_off = (buf.view_offset + step).min(max_off);
                    if new_off == buf.view_offset {
                        return; // already at the oldest line
                    }
                    buf.view_offset = new_off;
                    0u16
                } else if dir > 0 {
                    // Mouse below the bottom → move toward the live tail.
                    let new_off = buf.view_offset.saturating_sub(step);
                    if new_off == buf.view_offset {
                        return; // already at the live bottom
                    }
                    buf.view_offset = new_off;
                    last
                } else {
                    return;
                };
                let abs = buf.vis_to_abs(edge_vis);
                buf.sel_focus = Some((abs, focus_col));
                if let Some(range) = buf.sel_ranges.last_mut() {
                    range.1 = (abs, focus_col);
                }
            }
            if let Some(win) = weak.upgrade() {
                rebuild_tab_display(&win, &bufs_sel, &tid);
            }
        });
    }

    // Mouse events forwarded to the PTY for mouse-tracking TUI apps (btop,
    // htop, mc). The Slint side only calls this when the remote enabled a mouse
    // protocol (mouse_protocol_mode != None), so a click inside e.g. btop
    // highlights/activates the widget under the pointer instead of starting a
    // local drag-selection. Returns true when bytes were actually written, so
    // the caller can skip its own local handling.
    {
        let bufs_mouse = bufs.clone();
        let handles_mouse = handles.clone();
        window.on_terminal_mouse(
            move |tab_id: SharedString, kind: i32, button: i32, row: i32, col: i32| -> bool {
                let tid = tab_id.to_string();
                let Some(bytes) = term_buf(&bufs_mouse, &tid).map(|h| {
                    let buf = h.lock().unwrap();
                    let screen = buf.parser.screen();
                    let (rows, cols) = screen.size();
                    if buf.mouse_tracked {
                        let encoding = screen.mouse_protocol_encoding();
                        let (btn, release) = match kind {
                            1 => (button as u8, true),  // release
                            2 => (35, false),           // drag motion with button held
                            _ => (button as u8, false), // press
                        };
                        Some(encode_mouse_event(
                            btn, release, col, row, cols, rows, encoding,
                        ))
                    } else {
                        None
                    }
                }) else {
                    return false;
                };
                let Some(bytes) = bytes else {
                    return false;
                };
                if let Some(h) = handles_mouse.borrow().get(&tid) {
                    h.send_raw(bytes);
                    true
                } else {
                    false
                }
            },
        );
    }
}

/// Convert a Slint `KeyEvent.text` + modifier flags into the byte sequence
/// that the remote PTY expects.
///
/// Slint uses Unicode Private Use Area (`\u{F700}`…) for special keys.
/// Regular printable characters and C0 control characters are passed as-is.
///
/// Render a key string for diagnostic logs WITHOUT leaking its content (#15).
///
/// Any printable character could be a password character, so we never emit it.
/// Only C0/C1 control code points (Backspace, Esc, the IME-injected 0x10/0x15
/// markers, …) are revealed — those are exactly what the Shift/Backspace IME
/// diagnostics need and are never password material. Printable characters are
/// collapsed to a count, so the logs stay useful without exposing keystrokes.
pub(super) fn redact_key(key: &str) -> String {
    if key.is_empty() {
        return "(empty)".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    let mut printable = 0usize;
    for c in key.chars() {
        let cp = c as u32;
        if cp < 0x20 || (0x7f..=0x9f).contains(&cp) {
            parts.push(format!("U+{cp:04X}"));
        } else {
            printable += 1;
        }
    }
    if printable > 0 {
        parts.push(format!("<{printable} printable redacted>"));
    }
    parts.join(",")
}

/// macOS/IME combinations may report bare physical Control as a C0 character:
/// U+0017 opens nano search before Ctrl+X (#312), while U+0008 is encoded as
/// Backspace and deletes the preceding character during Ctrl+Space (#348).
pub(super) fn should_drop_macos_bare_ctrl_marker(key: &str, ctrl: bool, is_macos: bool) -> bool {
    is_macos
        && ctrl
        && matches!(
            key.chars().collect::<Vec<_>>().as_slice(),
            ['\u{0008}'] | ['\u{0017}']
        )
}
