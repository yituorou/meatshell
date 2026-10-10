//! Settings-page callbacks: appearance, interface, terminal and layout preferences.

use super::*;

/// Apply saved language, theme, fonts, wallpaper and command-bar seeds to a new window.
pub(super) fn apply_saved_appearance(ctx: &WinCtx) {
    let WinCtx {
        store,
        bufs,
        window,
        ..
    } = ctx;
    // Apply the saved UI language.  The Rust-side flag drives `i18n::t(...)`;
    // `apply_to_slint` selects the bundled `.po` for the static `@tr(...)` text
    // (must run after the first component exists, which it now does).
    crate::i18n::set_language(store.borrow().language());
    crate::i18n::apply_to_slint();
    window.set_lang_en(crate::i18n::is_en());

    // Apply the saved (or system-detected) theme.
    // "dark" / "light" → use that directly; "system" or unset → ask the OS;
    // OS unknown → fall back to dark.
    {
        let is_dark = theme_pref_is_dark(&store.borrow());
        window.set_dark_mode(is_dark);
    }
    // On macOS, app shortcuts use Cmd (⌘) so physical Ctrl stays free for the
    // shell (#158); on Windows/Linux they stay Ctrl-based.
    window.set_is_mac(cfg!(target_os = "macos"));
    window.set_is_windows(cfg!(windows));

    // Apply the saved terminal font (Interface settings). An empty family keeps
    // the built-in default; the size always applies (defaults to 13).
    {
        let s = store.borrow();
        let fam = s.font_family().to_string();
        refresh_glyph_fallback(&fam, window, bufs);
        if !fam.is_empty() {
            window.set_term_font_family(fam.into());
        }
        window.set_term_font_size(s.font_size() as f32);
        window.set_terminal_line_spacing(s.terminal_line_spacing());
        window.set_term_font_bold(s.terminal_bold());
        window.set_term_cursor_style(s.terminal_cursor_style().into());
        if let Some(color) = parse_hex_color(s.terminal_cursor_color()) {
            window.set_term_cursor_color_hex(s.terminal_cursor_color().into());
            window.set_term_cursor_color(color);
        }
        window.set_output_highlight_enabled(s.output_highlight_enabled());
        window.set_json_format_output(s.json_format_output());
        window.set_session_log_enabled(s.session_log_enabled());
        window.set_session_log_dir(s.session_log_dir().to_string_lossy().to_string().into());
        window.set_session_log_dir_custom(!s.session_log_dir_setting().is_empty());
        window.set_output_highlight_preset(s.output_highlight_preset().into());
        window.set_output_highlight_rules(output_highlight_rule_model(&s));
        window.set_ui_scale(s.ui_scale() as f32 / 100.0); // global UI zoom (#100)
        window.set_panel_font(s.panel_font() as f32 / 100.0); // settings-panel font scale
        window.set_renderer_mode(s.renderer_mode().into());
        window.set_mcp_enabled(s.mcp_enabled());
        window.set_mcp_use_saved_credentials(s.mcp_use_saved_credentials());
        window.set_mcp_allow_commands(s.mcp_allow_commands());
        window.set_mcp_allow_file_transfers(s.mcp_allow_file_transfers());
    }

    {
        let store = store.clone();
        window.on_set_mcp_permissions(move |enabled, credentials, commands, files| {
            let mut settings = store.borrow_mut();
            settings.set_mcp_enabled(enabled);
            settings.set_mcp_use_saved_credentials(credentials);
            settings.set_mcp_allow_commands(commands);
            settings.set_mcp_allow_file_transfers(files);
            let _ = settings.save();
        });
    }

    // Apply the saved immersive wallpaper (overrides dark/light when set; a
    // missing custom file falls back to the plain theme).
    {
        let id = store.borrow().wallpaper().to_string();
        // Restoring a saved wallpaper must not override the user's persisted
        // light/dark preference. Built-in wallpapers only suggest their paired
        // theme when the user actively selects them (#theme-persistence).
        apply_wallpaper(&window, &store.borrow(), &bufs, &id, false);
    }
    // Editable inputs (e.g. the SFTP path bar) need a CJK-capable font: the
    // embedded mono font has no Chinese glyphs and native TextInput doesn't
    // glyph-fallback like Text does, so typed Chinese would render as tofu (#54).
    //
    // We must NOT hard-code one system font name: on macOS 26 (Tahoe) fontdb
    // failed to register "PingFang SC", so the UI default font resolved to nothing
    // and *all* text vanished (#129) — icons survived only because they use an
    // embedded font. Instead probe what fontdb actually loaded and pick the first
    // resolvable CJK family, falling back to the embedded "Meatshell Mono" so the
    // window is never fully blank even when the system font DB is unreadable.
    window.set_ui_font_family(resolve_ui_font_family());
    // Populate the Interface font picker with installed monospace families.
    window.set_term_fonts(ModelRc::from(Rc::new(VecModel::from(
        system_monospace_fonts(),
    ))));

    // Command bar (#55): seed quick commands + history from the config. Groups
    // start collapsed by default (#55).
    window.set_quick_commands(quick_cmd_model(
        &store.borrow(),
        &all_quick_group_names(&store.borrow()),
    ));
    window.set_command_history(history_model(&store.borrow()));
    set_history_view(&window, &store.borrow(), ""); // #101, #419
}

/// Interface toggles: SFTP follows cd, command bar, download prompt, paste and zen settings.
pub(super) fn wire_interface_toggles(
    ctx: &WinCtx,
    sftp_follow_cd: &Arc<std::sync::atomic::AtomicBool>,
) {
    let WinCtx {
        store,
        registry,
        window,
        ..
    } = ctx;
    window.set_sftp_follow_cd(store.borrow().sftp_follow_cd());
    {
        let store = store.clone();
        let flag = sftp_follow_cd.clone();
        window.on_set_sftp_follow_cd(move |follow| {
            flag.store(follow, std::sync::atomic::Ordering::Relaxed);
            let mut s = store.borrow_mut();
            s.set_sftp_follow_cd(follow);
            let _ = s.save();
        });
    }

    // Toolbar toggle: hide/show the quick-command bar (persisted globally).
    window.set_cmd_bar_hidden(store.borrow().cmd_bar_hidden());
    {
        let store = store.clone();
        let registry = registry.clone();
        window.on_set_cmd_bar_hidden(move |hidden| {
            let mut s = store.borrow_mut();
            s.set_cmd_bar_hidden(hidden);
            let _ = s.save();
            drop(s);
            registry.broadcast_config_changed();
        });
    }

    // Interface setting: always ask where to save on download (#87). Read live
    // by the download handler from the window property, so just set + persist.
    window.set_download_always_ask(store.borrow().download_always_ask());
    window.set_paste_confirm_enabled(store.borrow().paste_confirm_enabled());
    window.set_extra_paste_shortcuts_enabled(store.borrow().extra_paste_shortcuts_enabled());
    window.set_zen_mode(store.borrow().zen_mode());
    {
        let store = store.clone();
        window.on_set_download_always_ask(move |ask| {
            let mut s = store.borrow_mut();
            s.set_download_always_ask(ask);
            let _ = s.save();
        });
    }
}

/// Session logging settings (#265).
pub(super) fn wire_session_log_settings(ctx: &WinCtx) {
    let WinCtx {
        store,
        handles,
        bufs,
        window,
        ..
    } = ctx;
    // --- Session logging (#265) -------------------------------------------
    {
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_set_session_log_enabled(move |enabled| {
            let dir = {
                let mut s = store.borrow_mut();
                s.set_session_log_enabled(enabled);
                let _ = s.save();
                s.session_log_dir()
            };
            // Apply to tabs that are already open, not only to new ones.
            let handles: Vec<TermBufferHandle> = bufs.lock().unwrap().values().cloned().collect();
            for handle in handles {
                apply_session_log_to_buffer(&mut handle.lock().unwrap(), enabled, &dir);
            }
        });
    }
    {
        let store = store.clone();
        let weak = window.as_weak();
        window.on_pick_session_log_dir(move || {
            let start = store.borrow().session_log_dir();
            let Some(folder) = DialogOwner::of_weak(&weak)
                .file()
                .set_directory(&start)
                .pick_folder()
            else {
                return;
            };
            let effective = {
                let mut s = store.borrow_mut();
                s.set_session_log_dir(folder.to_string_lossy().to_string());
                let _ = s.save();
                s.session_log_dir()
            };
            if let Some(w) = weak.upgrade() {
                w.set_session_log_dir(effective.to_string_lossy().to_string().into());
                w.set_session_log_dir_custom(true);
            }
        });
    }
    {
        let store = store.clone();
        let weak = window.as_weak();
        window.on_reset_session_log_dir(move || {
            let effective = {
                let mut s = store.borrow_mut();
                s.set_session_log_dir(String::new());
                let _ = s.save();
                s.session_log_dir()
            };
            if let Some(w) = weak.upgrade() {
                w.set_session_log_dir(effective.to_string_lossy().to_string().into());
                w.set_session_log_dir_custom(false);
            }
        });
    }
    {
        let store = store.clone();
        window.on_open_session_log_dir(move || {
            let dir = store.borrow().session_log_dir();
            if std::fs::create_dir_all(&dir).is_err() {
                return;
            }
            #[cfg(windows)]
            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
            #[cfg(target_os = "macos")]
            let _ = std::process::Command::new("open").arg(&dir).spawn();
            #[cfg(all(not(windows), not(target_os = "macos")))]
            let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
        });
    }
    {
        let store = store.clone();
        window.on_set_paste_confirm_enabled(move |enabled| {
            let mut s = store.borrow_mut();
            s.set_paste_confirm_enabled(enabled);
            let _ = s.save();
        });
    }
    {
        let store = store.clone();
        window.on_set_extra_paste_shortcuts_enabled(move |enabled| {
            let mut s = store.borrow_mut();
            s.set_extra_paste_shortcuts_enabled(enabled);
            let _ = s.save();
        });
    }
    {
        let store = store.clone();
        let handles = handles.clone();
        let weak = window.as_weak();
        window.on_set_zen_mode(move |enabled| {
            let mut s = store.borrow_mut();
            s.set_zen_mode(enabled);
            let _ = s.save();
            let sidebar_visible = weak
                .upgrade()
                .map(|window| !window.get_sidebar_collapsed())
                .unwrap_or(false);
            for handle in handles.borrow().values() {
                handle.set_resource_monitoring(!enabled && sidebar_visible);
            }
        });
    }
}

/// Sidebar / welcome / SFTP layout preferences, update check, renderer and sync-upload settings.
pub(super) fn wire_layout_prefs(ctx: &WinCtx) {
    let WinCtx {
        store,
        handles,
        window,
        pending_window_size_restore,
        ..
    } = ctx;
    // checkboxes, apply the collapsed state once at startup, and persist toggles.
    {
        let s = store.borrow();
        let collapse_sidebar = s.collapse_sidebar_default();
        let collapse_sftp = s.collapse_sftp_default();
        let sidebar_dock = s.sidebar_dock();
        let welcome_as_sidebar = s.welcome_as_sidebar();
        let quick_commands_as_sidebar = s.quick_commands_as_sidebar();
        let quick_panel_open = quick_commands_as_sidebar && s.quick_panel_open();
        let quick_panel_collapsed = s.quick_panel_collapsed();
        let quick_panel_dock = s.quick_panel_dock();
        let welcome_sidebar_dock = s.welcome_sidebar_dock();
        let mut sidebar_collapsed = s.sidebar_collapsed().unwrap_or(collapse_sidebar);
        let mut welcome_collapsed = s.welcome_collapsed().unwrap_or(false);
        if welcome_as_sidebar
            && sidebar_dock == welcome_sidebar_dock
            && !sidebar_collapsed
            && !welcome_collapsed
        {
            sidebar_collapsed = true;
        }
        if quick_panel_open && !quick_panel_collapsed {
            if sidebar_dock == quick_panel_dock {
                sidebar_collapsed = true;
            }
            if welcome_as_sidebar && welcome_sidebar_dock == quick_panel_dock {
                welcome_collapsed = true;
            }
        }
        window.set_collapse_sidebar_default(collapse_sidebar);
        window.set_collapse_sftp_default(collapse_sftp);
        // Restore the persisted panel docking layout (#dock).
        window.set_sidebar_width(s.sidebar_width());
        window.set_sidebar_height(s.sidebar_height());
        window.set_sidebar_dock(sidebar_dock.into());
        window.set_sftp_panel_width(s.sftp_panel_width());
        window.set_sftp_panel_height(s.sftp_panel_height());
        window.set_sftp_tree_width(s.sftp_tree_width());
        let columns = s.sftp_visible_columns();
        window.set_sftp_show_type(columns.iter().any(|column| column == "type"));
        window.set_sftp_show_size(columns.iter().any(|column| column == "size"));
        window.set_sftp_show_modified(columns.iter().any(|column| column == "modified"));
        window.set_sftp_show_permissions(columns.iter().any(|column| column == "permissions"));
        window.set_sftp_show_owner(columns.iter().any(|column| column == "owner"));
        window.set_sftp_show_group(columns.iter().any(|column| column == "group"));
        window.set_sftp_dock(s.sftp_dock().into());
        window.set_quick_commands_as_sidebar(quick_commands_as_sidebar);
        window.set_quick_panel_open(quick_panel_open);
        window.set_quick_panel_collapsed(quick_panel_collapsed);
        window.set_quick_panel_width(s.quick_panel_width());
        window.set_quick_panel_height(s.quick_panel_height());
        window.set_quick_panel_dock(quick_panel_dock.into());
        window.set_welcome_as_sidebar(welcome_as_sidebar);
        window.set_welcome_sidebar_width(s.welcome_sidebar_width());
        window.set_welcome_sidebar_dock(welcome_sidebar_dock.into());
        window.set_welcome_collapsed(welcome_collapsed);
        window.set_sidebar_collapsed(sidebar_collapsed);
        window.set_wallpaper_overlay(s.wallpaper_overlay());
        window.set_update_check_enabled(s.update_check_enabled()); // #184
        if collapse_sftp {
            window.set_sftp_collapsed(true);
            window.set_sftp_saved_height(s.sftp_panel_height());
        }
        // Capture the user's preferred size. The first native Resized event
        // drives restoration below; this is deterministic and avoids guessing
        // how long Slint/window-manager initialization takes (#278).
        let (ww, wh) = s.window_size();
        let preferred = (ww > 0.0 && wh > 0.0).then_some((ww, wh));
        pending_window_size_restore.set(preferred);
    }
    {
        let store = store.clone();
        window.on_set_collapse_sidebar_default(move |v| {
            let mut s = store.borrow_mut();
            s.set_collapse_sidebar_default(v);
            let _ = s.save();
        });
    }
    {
        let store = store.clone();
        window.on_set_quick_commands_as_sidebar(move |v| {
            let mut s = store.borrow_mut();
            s.set_quick_commands_as_sidebar(v);
            let _ = s.save();
        });
    }
    {
        // Toggle the startup new-version check (#184). Takes effect next launch
        // for the check itself; the banner just won't appear once it's off.
        let store = store.clone();
        window.on_set_update_check_enabled(move |v| {
            let mut s = store.borrow_mut();
            s.set_update_check_enabled(v);
            let _ = s.save();
        });
    }
    {
        // Renderer selection is consumed before the first native window exists,
        // so persist it now and apply it on the next launch (#280).
        let store = store.clone();
        window.on_set_renderer_mode(move |mode: SharedString| {
            let mut s = store.borrow_mut();
            s.set_renderer_mode(mode.to_string());
            let _ = s.save();
        });
    }
    {
        let store = store.clone();
        window.on_persist_sidebar_width(move |w| {
            let mut s = store.borrow_mut();
            s.set_sidebar_width(w);
            let _ = s.save();
        });
    }
    {
        let store = store.clone();
        let handles = handles.clone();
        let weak = window.as_weak();
        window.on_set_sidebar_collapsed(move |v| {
            let mut s = store.borrow_mut();
            s.set_sidebar_collapsed(v);
            let _ = s.save();
            let zen = weak
                .upgrade()
                .map(|window| window.get_zen_mode())
                .unwrap_or(false);
            for handle in handles.borrow().values() {
                handle.set_resource_monitoring(!v && !zen);
            }
        });
    }
    {
        let store = store.clone();
        window.on_persist_welcome_sidebar_width(move |w| {
            let mut s = store.borrow_mut();
            s.set_welcome_sidebar_width(w);
            let _ = s.save();
        });
    }
    {
        let store = store.clone();
        window.on_persist_welcome_sidebar_dock(move |dock| {
            let mut s = store.borrow_mut();
            s.set_welcome_sidebar_dock(dock.to_string());
            let _ = s.save();
        });
    }
    {
        let store = store.clone();
        window.on_set_welcome_collapsed(move |v| {
            let mut s = store.borrow_mut();
            s.set_welcome_collapsed(v);
            let _ = s.save();
        });
    }
    {
        let store = store.clone();
        window.on_persist_wallpaper_overlay(move |v| {
            let mut s = store.borrow_mut();
            s.set_wallpaper_overlay(v);
            let _ = s.save();
        });
    }
    {
        let store = store.clone();
        window.on_set_collapse_sftp_default(move |v| {
            let mut s = store.borrow_mut();
            s.set_collapse_sftp_default(v);
            let _ = s.save();
        });
    }

    // Session-sync upload setting (#sync). Persisted; only has effect while the
    // session-sync toggle is on. Read live from the window in the upload handler.
    window.set_sync_upload_enabled(store.borrow().sync_upload());
    {
        let store = store.clone();
        window.on_set_sync_upload_enabled(move |v| {
            let mut s = store.borrow_mut();
            s.set_sync_upload(v);
            let _ = s.save();
        });
    }
}

/// Terminal appearance settings: cursor, fonts, spacing and output highlighting.
pub(super) fn wire_terminal_settings(ctx: &WinCtx) {
    let WinCtx {
        store,
        registry,
        bufs,
        window,
        ..
    } = ctx;
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_term_cursor_color(move |value: SharedString| {
            let Some(color) = parse_hex_color(value.as_str()) else {
                return false;
            };
            {
                let mut s = store.borrow_mut();
                if !s.set_terminal_cursor_color(value.as_str()) {
                    return false;
                }
                let _ = s.save();
            }
            if let Some(w) = weak.upgrade() {
                w.set_term_cursor_color(color);
            }
            true
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_add_output_highlight_rule(
            move |pattern: SharedString,
                  is_regex,
                  case_sensitive,
                  whole_line,
                  color: SharedString| {
                let pattern = pattern.trim().to_string();
                let validation = validate_output_highlight_rule(&pattern, is_regex, case_sensitive);
                let Some(w) = weak.upgrade() else {
                    return false;
                };
                if let Err(message) = validation {
                    w.set_output_highlight_rule_status(message.into());
                    return false;
                }
                if store.borrow().output_highlight_rules().len() >= 128 {
                    w.set_output_highlight_rule_status(
                        t("自定义规则最多 128 条", "Custom rules are limited to 128").into(),
                    );
                    return false;
                }
                {
                    let mut s = store.borrow_mut();
                    s.add_output_highlight_rule(OutputHighlightRule {
                        pattern,
                        regex: is_regex,
                        case_sensitive,
                        whole_line,
                        color: color.to_string(),
                        enabled: true,
                    });
                    let _ = s.save();
                    w.set_output_highlight_rules(output_highlight_rule_model(&s));
                    apply_custom_output_rules(&w, &bufs, s.output_highlight_rules());
                }
                w.set_output_highlight_rule_status("".into());
                true
            },
        );
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_remove_output_highlight_rule(move |index| {
            let Some(w) = weak.upgrade() else { return };
            let mut s = store.borrow_mut();
            s.remove_output_highlight_rule(index.max(0) as usize);
            let _ = s.save();
            w.set_output_highlight_rules(output_highlight_rule_model(&s));
            apply_custom_output_rules(&w, &bufs, s.output_highlight_rules());
            w.set_output_highlight_rule_status("".into());
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_set_output_highlight_rule_enabled(move |index, enabled| {
            let Some(w) = weak.upgrade() else { return };
            let mut s = store.borrow_mut();
            s.set_output_highlight_rule_enabled(index.max(0) as usize, enabled);
            let _ = s.save();
            w.set_output_highlight_rules(output_highlight_rule_model(&s));
            apply_custom_output_rules(&w, &bufs, s.output_highlight_rules());
        });
    }
    // Interface settings: apply + persist the terminal font family / size.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_set_term_font(move |family: SharedString| {
            {
                let mut s = store.borrow_mut();
                s.set_font_family(family.to_string());
                let _ = s.save();
            }
            if let Some(w) = weak.upgrade() {
                // The new font covers a different glyph set (#461).
                refresh_glyph_fallback(family.as_str(), &w, &bufs);
                w.set_term_font_family(family);
            }
        });
    }
    // Output highlighting: persist the switch/preset and immediately rebuild
    // every open terminal, including scrollback captured before the change.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_set_output_highlight(move |enabled, preset: SharedString| {
            let preset = preset.to_string();
            {
                let mut s = store.borrow_mut();
                s.set_output_highlight_enabled(enabled);
                s.set_output_highlight_preset(preset.clone());
                let _ = s.save();
            }
            if let Some(w) = weak.upgrade() {
                apply_output_highlight(&w, &bufs, enabled, &preset);
            }
        });
    }
    {
        let store = store.clone();
        let bufs = bufs.clone();
        window.on_set_json_format_output(move |enabled| {
            {
                let mut settings = store.borrow_mut();
                settings.set_json_format_output(enabled);
                let _ = settings.save();
            }
            for buffer in bufs.lock().unwrap().values() {
                buffer.lock().unwrap().json_format_output = enabled;
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let registry = registry.clone();
        window.on_set_term_font_size(move |size: i32| {
            {
                let mut s = store.borrow_mut();
                s.set_font_size(size as u32);
                let _ = s.save();
            }
            if let Some(w) = weak.upgrade() {
                w.set_term_font_size(size as f32);
            }
            registry.broadcast_config_changed();
        });
    }
    {
        let store = store.clone();
        window.on_persist_sftp_tree_width(move |width| {
            let mut s = store.borrow_mut();
            s.set_sftp_tree_width(width);
            let _ = s.save();
        });
    }
    {
        let store = store.clone();
        let weak = window.as_weak();
        window.on_toggle_sftp_column(move |column: SharedString| {
            let columns = {
                let mut s = store.borrow_mut();
                let mut columns = s.sftp_visible_columns();
                if column != "name" {
                    if let Some(index) = columns.iter().position(|value| value == column.as_str()) {
                        columns.remove(index);
                    } else {
                        columns.push(column.to_string());
                    }
                    s.set_sftp_visible_columns(columns);
                }
                let _ = s.save();
                s.sftp_visible_columns()
            };
            if let Some(w) = weak.upgrade() {
                w.set_sftp_show_type(columns.iter().any(|value| value == "type"));
                w.set_sftp_show_size(columns.iter().any(|value| value == "size"));
                w.set_sftp_show_modified(columns.iter().any(|value| value == "modified"));
                w.set_sftp_show_permissions(columns.iter().any(|value| value == "permissions"));
                w.set_sftp_show_owner(columns.iter().any(|value| value == "owner"));
                w.set_sftp_show_group(columns.iter().any(|value| value == "group"));
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_terminal_line_spacing(move |spacing: f32| {
            let normalized = {
                let mut s = store.borrow_mut();
                s.set_terminal_line_spacing(spacing);
                let normalized = s.terminal_line_spacing();
                let _ = s.save();
                normalized
            };
            if let Some(w) = weak.upgrade() {
                w.set_terminal_line_spacing(normalized);
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_term_font_bold(move |bold: bool| {
            {
                let mut s = store.borrow_mut();
                s.set_terminal_bold(bold);
                let _ = s.save();
            }
            if let Some(w) = weak.upgrade() {
                w.set_term_font_bold(bold);
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_term_cursor_style(move |style: SharedString| {
            let normalized = {
                let mut s = store.borrow_mut();
                s.set_terminal_cursor_style(style.to_string());
                let normalized = s.terminal_cursor_style().to_string();
                let _ = s.save();
                normalized
            };
            if let Some(w) = weak.upgrade() {
                w.set_term_cursor_style(normalized.into());
            }
        });
    }
}

/// Global UI scale (#100), panel font and wallpaper selection.
pub(super) fn wire_ui_scale_and_wallpaper(ctx: &WinCtx) {
    let WinCtx {
        store,
        registry,
        bufs,
        window,
        proc_win,
        editor_win,
        ..
    } = ctx;
    // Global UI scale (#100): persist the percent and apply it live.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let editor_weak = editor_win.as_weak();
        window.on_set_ui_scale(move |percent: i32| {
            let clamped = (percent.max(0) as u32).clamp(80, 200);
            {
                let mut s = store.borrow_mut();
                s.set_ui_scale(clamped);
                let _ = s.save();
            }
            if let Some(w) = weak.upgrade() {
                w.set_ui_scale(clamped as f32 / 100.0);
            }
            if let Some(editor) = editor_weak.upgrade() {
                editor.set_ui_scale(clamped as f32 / 100.0);
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        window.on_set_panel_font(move |percent: i32| {
            let clamped = (percent.max(0) as u32).clamp(80, 160);
            {
                let mut s = store.borrow_mut();
                s.set_panel_font(clamped);
                let _ = s.save();
            }
            if let Some(w) = weak.upgrade() {
                w.set_panel_font(clamped as f32 / 100.0);
            }
        });
    }

    // Wallpaper: pick a built-in / none, or open the file dialog for a custom one.
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs_wp = bufs.clone();
        let proc_weak = proc_win.as_weak();
        let registry = registry.clone();
        window.on_set_wallpaper(move |id: SharedString| {
            let id = id.to_string();
            let mut selected_builtin_theme = None;
            if let Some(w) = weak.upgrade() {
                apply_wallpaper(&w, &store.borrow(), &bufs_wp, &id, true);
                if crate::wallpaper::is_builtin(&id) {
                    selected_builtin_theme = Some(w.get_dark_mode());
                }
                // Keep an already-open process window in sync with the change.
                if let Some(p) = proc_weak.upgrade() {
                    sync_proc_theme(&w, &p);
                }
            }
            {
                let mut s = store.borrow_mut();
                s.set_wallpaper(id);
                // Choosing a built-in wallpaper applies its recommended palette once;
                // persist that result so it too survives the next launch. A later
                // manual theme toggle will overwrite this preference as expected.
                if let Some(dark) = selected_builtin_theme {
                    s.set_theme_pref(if dark { "dark" } else { "light" }.to_string());
                }
                let _ = s.save();
            }
            // Only the theme flip needs cross-window propagation; the wallpaper
            // image itself is not synced to other windows (YAGNI).
            if selected_builtin_theme.is_some() {
                registry.broadcast_config_changed();
            }
        });
    }
    {
        let weak = window.as_weak();
        let store = store.clone();
        let bufs_wp = bufs.clone();
        let proc_weak = proc_win.as_weak();
        window.on_pick_wallpaper_file(move || {
            let picked = DialogOwner::of_weak(&weak)
                .file()
                .set_title("选择壁纸 / Choose wallpaper")
                .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp"])
                .pick_file();
            if let Some(path) = picked {
                let id = path.to_string_lossy().to_string();
                if let Some(w) = weak.upgrade() {
                    apply_wallpaper(&w, &store.borrow(), &bufs_wp, &id, false);
                    if let Some(p) = proc_weak.upgrade() {
                        sync_proc_theme(&w, &p);
                    }
                }
                let mut s = store.borrow_mut();
                s.set_wallpaper(id);
                let _ = s.save();
            }
        });
    }
}
