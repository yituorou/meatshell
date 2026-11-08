use super::*;

fn choose_download_conflict(
    owner: DialogOwner,
    remote: &str,
    local_dir: &str,
) -> Option<DownloadConflict> {
    let target = download_target_path(remote, local_dir);
    if !target.is_file() {
        return Some(DownloadConflict::Replace);
    }
    let replace = t("替换", "Replace");
    let keep_both = t("共存", "Keep both");
    let cancel = t("取消", "Cancel");
    let result = owner
        .message()
        .set_title(t("文件已存在", "File already exists"))
        .set_description(format!(
            "{}\n{}",
            target.display(),
            t(
                "请选择替换现有文件，或保留两份文件。",
                "Choose whether to replace the existing file or keep both files."
            )
        ))
        .set_level(rfd::MessageLevel::Warning)
        .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
            replace.to_string(),
            keep_both.to_string(),
            cancel.to_string(),
        ))
        .show();
    match result {
        rfd::MessageDialogResult::Yes => Some(DownloadConflict::Replace),
        rfd::MessageDialogResult::No => Some(DownloadConflict::KeepBoth),
        rfd::MessageDialogResult::Custom(value) if value == replace => {
            Some(DownloadConflict::Replace)
        }
        rfd::MessageDialogResult::Custom(value) if value == keep_both => {
            Some(DownloadConflict::KeepBoth)
        }
        _ => None,
    }
}

/// Ask once for a whole batch of downloads over the same local folder: does the
/// user want the existing files replaced, kept side by side, or the batch
/// cancelled? Returns `None` when the dialog was dismissed / cancelled.
fn choose_batch_download_conflict(
    owner: DialogOwner,
    count: usize,
    local_dir: &str,
) -> Option<DownloadConflict> {
    let replace = t("替换", "Replace");
    let keep_both = t("共存", "Keep both");
    let cancel = t("取消", "Cancel");
    let result = owner
        .message()
        .set_title(t("文件已存在", "File already exists"))
        .set_description(format!(
            "{}\n{}",
            local_dir,
            if count == 1 {
                t(
                    "目标文件夹里已有同名文件。请选择替换现有文件，或保留两份文件。",
                    "A file with the same name already exists in the target folder. Choose whether to replace it or keep both."
                )
                .to_string()
            } else {
                format!(
                    "{} {}",
                    count,
                    t(
                        "项在目标文件夹里已有同名文件。请选择替换现有文件，或保留两份文件。",
                        "items already exist in the target folder. Choose whether to replace them or keep both."
                    )
                )
            }
        ))
        .set_level(rfd::MessageLevel::Warning)
        .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
            replace.to_string(),
            keep_both.to_string(),
            cancel.to_string(),
        ))
        .show();
    match result {
        rfd::MessageDialogResult::Yes => Some(DownloadConflict::Replace),
        rfd::MessageDialogResult::No => Some(DownloadConflict::KeepBoth),
        rfd::MessageDialogResult::Custom(value) if value == replace => {
            Some(DownloadConflict::Replace)
        }
        rfd::MessageDialogResult::Custom(value) if value == keep_both => {
            Some(DownloadConflict::KeepBoth)
        }
        _ => None,
    }
}

/// Copy local paths into the tab's current remote directory.
fn upload_local_paths(
    window: &AppWindow,
    tab_id: &str,
    sftp_handles: &SftpHandles,
    paths: Vec<String>,
) {
    if paths.is_empty() {
        return;
    }
    let remote_dir = active_sftp_path(window, tab_id);
    if remote_dir.is_empty() {
        return;
    }
    if let Ok(handles) = sftp_handles.lock() {
        let Some(handle) = handles.get(tab_id) else {
            return;
        };
        for path in paths {
            handle.upload(std::path::PathBuf::from(path), remote_dir.clone());
        }
        window.set_download_open(true);
    }
}

/// Copy remote entries into the folder the tab's local pane is showing.
fn download_into_local_pane(
    window: &AppWindow,
    tab_id: &str,
    sftp_handles: &SftpHandles,
    remotes: Vec<String>,
) {
    if remotes.is_empty() {
        return;
    }
    let local_dir = current_path(window, tab_id);
    if local_dir.is_empty() {
        return;
    }
    // One conflict prompt for the whole batch — asking per file would be
    // miserable for a multi-selection of a hundred items.
    let overwrites = remotes
        .iter()
        .filter(|remote| download_target_path(remote, &local_dir).exists())
        .count();
    let conflict = if overwrites == 0 {
        DownloadConflict::Replace
    } else {
        match choose_batch_download_conflict(
            DialogOwner::of(window.window()),
            overwrites,
            &local_dir,
        ) {
            Some(conflict) => conflict,
            None => return,
        }
    };
    if let Ok(handles) = sftp_handles.lock() {
        if let Some(handle) = handles.get(tab_id) {
            handle.download_into(remotes, local_dir, conflict);
            window.set_download_open(true);
        }
    }
}

/// Wire the split (dual-pane) SFTP view: local folder browsing plus the two
/// transfer directions. The remote side keeps using the existing SFTP worker.
fn wire_sftp_split_callbacks(window: &AppWindow, sftp_handles: &SftpHandles) {
    // Toggle the split view; the first open reads the local folder.
    {
        let weak = window.as_weak();
        window.on_sftp_toggle_dual(move |tab_id: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            let terminals = w.get_terminals();
            let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            let tab = tab_id.to_string();
            let mut enable = false;
            let mut path = String::new();
            let mut loaded = false;
            for i in 0..model.row_count() {
                let Some(row) = model.row_data(i) else { continue };
                if row.id.as_str() != tab {
                    continue;
                }
                enable = !row.sftp_dual;
                path = row.sftp_local_path.to_string();
                loaded = row.sftp_local_entries.row_count() > 0;
                break;
            }
            update_terminal_row(model, &tab, |row| row.sftp_dual = enable);
            // Remember the choice so the next launch opens the same view (#532).
            w.set_sftp_split_enabled(enable);
            if enable {
                refresh_places(&w, &tab);
            }
            if enable && !loaded {
                // `is_dir` used to run here, on the UI thread; a dead network
                // drive would freeze the window. The listing thread decides and
                // falls back to the home folder instead (#532).
                let start = if path.is_empty() {
                    home_dir()
                } else {
                    std::path::PathBuf::from(&path)
                };
                load_dir_or_fallback(&w, &tab, start, home_dir());
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_sftp_local_navigate(move |tab_id: SharedString, path: SharedString| {
            if let Some(w) = weak.upgrade() {
                navigate(&w, tab_id.as_str(), path.as_str());
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_sftp_local_up(move |tab_id: SharedString| {
            if let Some(w) = weak.upgrade() {
                navigate_up(&w, tab_id.as_str());
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_sftp_local_home(move |tab_id: SharedString| {
            if let Some(w) = weak.upgrade() {
                load_dir(&w, tab_id.as_str(), home_dir());
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_sftp_local_refresh(move |tab_id: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            let path = current_path(&w, tab_id.as_str());
            let dir = if path.is_empty() {
                home_dir()
            } else {
                std::path::PathBuf::from(path)
            };
            load_dir(&w, tab_id.as_str(), dir);
        });
    }
    {
        window.on_sftp_local_open(move |_tab_id: SharedString, path: SharedString| {
            // No shell involved: the path is handed to the OS as one opaque
            // argument (the same helper remote "Open externally" uses).
            crate::sftp::open_with_os(path.as_str());
        });
    }
    {
        let weak = window.as_weak();
        window.on_sftp_local_toggle_select(move |tab_id: SharedString, index: i32| {
            let Some(w) = weak.upgrade() else { return };
            let terminals = w.get_terminals();
            let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            toggle_selected(model, tab_id.as_str(), index);
        });
    }
    {
        let weak = window.as_weak();
        window.on_sftp_local_select_range(
            move |tab_id: SharedString, from: i32, to: i32| {
                let Some(w) = weak.upgrade() else { return };
                let terminals = w.get_terminals();
                let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>()
                else {
                    return;
                };
                select_range(model, tab_id.as_str(), from, to);
            },
        );
    }
    {
        window.on_sftp_local_copy_path(move |path: SharedString| {
            // Clipboard writes must never run on the UI thread (#sftp-copy-path-freeze).
            std::thread::spawn(move || clipboard_set_text(path.to_string()));
        });
    }
    {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_local_upload(move |tab_id: SharedString, path: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            upload_local_paths(&w, tab_id.as_str(), &sftp_handles, vec![path.to_string()]);
        });
    }
    {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_local_upload_selected(move |tab_id: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            let terminals = w.get_terminals();
            let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            let paths = selected_paths(model, tab_id.as_str());
            upload_local_paths(&w, tab_id.as_str(), &sftp_handles, paths);
        });
    }
    {
        // Divider moved: remember it on the tab so the OS-drop hit test uses the
        // same local/remote boundary the user sees.
        let weak = window.as_weak();
        window.on_persist_sftp_local_pane_width(move |tab_id: SharedString, width: f32| {
            let Some(w) = weak.upgrade() else { return };
            let terminals = w.get_terminals();
            let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            update_terminal_row(model, tab_id.as_str(), |row| {
                row.sftp_local_pane_width = width;
            });
        });
    }
    {
        // The share of the panel is also stored globally (0 = even split) so the
        // split view comes back the way it was left (#532).
        let weak = window.as_weak();
        window.on_persist_sftp_local_split(move |ratio: f32| {
            let Some(w) = weak.upgrade() else { return };
            w.set_sftp_local_split(ratio);
        });
    }
    {
        // Places menu opened: re-read drives / user folders off the UI thread.
        let weak = window.as_weak();
        window.on_sftp_local_places_refresh(move |tab_id: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            refresh_places(&w, tab_id.as_str());
        });
    }
    {
        // Local bookmarks / recent folders (#532): machine-wide, mirrored into
        // every tab so the star and the menu stay in sync.
        let weak = window.as_weak();
        window.on_sftp_local_toggle_bookmark(move |tab_id: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            toggle_bookmark(&w, tab_id.as_str());
        });
    }
    {
        let weak = window.as_weak();
        window.on_sftp_local_remove_bookmark(move |_tab: SharedString, path: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            remove_bookmark(&w, path.as_str());
        });
    }
    {
        let weak = window.as_weak();
        window.on_sftp_local_clear_recent(move |_tab: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            clear_recent(&w);
        });
    }
    {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_download_to_local(
            move |tab_id: SharedString, remote: SharedString| {
                let Some(w) = weak.upgrade() else { return };
                download_into_local_pane(
                    &w,
                    tab_id.as_str(),
                    &sftp_handles,
                    vec![remote.to_string()],
                );
            },
        );
    }
    {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_download_selected_to_local(move |tab_id: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            let terminals = w.get_terminals();
            let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            let remotes = collect_sftp_selected(model, tab_id.as_str());
            download_into_local_pane(&w, tab_id.as_str(), &sftp_handles, remotes);
            clear_sftp_selection(model, tab_id.as_str());
        });
    }
}

pub(super) fn wire_sftp_callbacks(
    window: &AppWindow,
    editor: &EditorWindow,
    sftp_handles: SftpHandles,
    sftp_last_cwd: SftpLastCwd,
) {
    // Navigate to a remote path (or ".." to go up one level).
    {
        let sftp_handles = sftp_handles.clone();
        let sftp_last_cwd = sftp_last_cwd.clone();
        let weak = window.as_weak();
        window.on_sftp_navigate(move |tab_id: SharedString, path: SharedString| {
            let tab_id = tab_id.to_string();
            // A pasted path may carry trailing whitespace / newline (#54).
            let path = path.trim();
            let resolved = if path == ".." {
                let current = weak.upgrade().and_then(|w| {
                    let terminals_rc = w.get_terminals();
                    let terminals = terminals_rc
                        .as_any()
                        .downcast_ref::<VecModel<TerminalState>>()?;
                    for i in 0..terminals.row_count() {
                        if let Some(row) = terminals.row_data(i) {
                            if row.id.as_str() == tab_id {
                                return Some(row.sftp_path.to_string());
                            }
                        }
                    }
                    None
                });
                parent_path(&current.unwrap_or_else(|| "/".to_string()))
            } else {
                path.to_string()
            };
            // Forget the followed cwd so the next OSC 7 — even at an unchanged
            // directory — snaps the panel back to the shell's cwd; manual
            // navigation never permanently disables cd-follow.
            sftp_last_cwd.lock().unwrap().remove(&tab_id);
            if let Ok(handles) = sftp_handles.lock() {
                if let Some(h) = handles.get(&tab_id) {
                    h.list_dir(resolved);
                }
            }
        });
    }

    // Download a remote file.  If a download folder is preset in settings, save
    // straight there; otherwise fall back to a native folder picker.
    {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_download(move |tab_id: SharedString, remote_path: SharedString| {
            let tab_id = tab_id.to_string();
            let remote_path = remote_path.to_string();
            // If the user has checked 2+ entries, ANY download (right-click,
            // row button or the toolbar) packs the whole checked set into one
            // archive (#100) — this matches "download these together". A single
            // checked item (or none) downloads the clicked file as-is.
            let (arc_dir, arc_names) = weak
                .upgrade()
                .and_then(|w| {
                    let terminals = w.get_terminals();
                    let tm = terminals
                        .as_any()
                        .downcast_ref::<VecModel<TerminalState>>()?;
                    let paths = collect_sftp_selected(tm, &tab_id);
                    if paths.len() >= 2 {
                        let dir = active_sftp_path(&w, &tab_id);
                        let names: Vec<String> = paths
                            .iter()
                            .map(|p| {
                                p.trim_end_matches('/')
                                    .rsplit(['/', '\\'])
                                    .next()
                                    .unwrap_or(p)
                                    .to_string()
                            })
                            .collect();
                        clear_sftp_selection(tm, &tab_id);
                        Some((dir, names))
                    } else {
                        None
                    }
                })
                .map(|(d, n)| (Some(d), n))
                .unwrap_or((None, Vec::new()));
            let owner = weak
                .upgrade()
                .map(|w| DialogOwner::of(w.window()))
                .unwrap_or_default();
            // "Always ask" (#87) forces the folder picker, ignoring the preset.
            let (preset, always_ask) = weak
                .upgrade()
                .map(|w| {
                    (
                        w.get_download_dir().to_string(),
                        w.get_download_always_ask(),
                    )
                })
                .unwrap_or_default();
            if !always_ask && !preset.is_empty() {
                if let Ok(handles) = sftp_handles.lock() {
                    if let Some(h) = handles.get(&tab_id) {
                        if let Some(ref dir) = arc_dir {
                            h.download_archive(dir.clone(), arc_names.clone(), preset);
                        } else if let Some(conflict) =
                            choose_download_conflict(owner, &remote_path, &preset)
                        {
                            h.download(remote_path, preset, conflict);
                        }
                        // Pop the transfers panel so progress is visible (user
                        // request: any download opens the download popup).
                        if let Some(w) = weak.upgrade() {
                            w.set_download_open(true);
                        }
                    }
                }
                return;
            }
            let sftp_handles = sftp_handles.clone();
            let weak = weak.clone();
            std::thread::spawn(move || {
                if let Some(dir) = owner.file().pick_folder() {
                    let local_dir = dir.to_string_lossy().to_string();
                    if let Ok(handles) = sftp_handles.lock() {
                        if let Some(h) = handles.get(&tab_id) {
                            if let Some(ref rdir) = arc_dir {
                                h.download_archive(rdir.clone(), arc_names.clone(), local_dir);
                            } else if let Some(conflict) =
                                choose_download_conflict(owner, &remote_path, &local_dir)
                            {
                                h.download(remote_path, local_dir, conflict);
                            }
                        }
                    }
                    let _ = weak.upgrade_in_event_loop(|w| w.set_download_open(true));
                }
            });
        });
    }

    // Upload a local file into the current remote directory.
    {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_upload_clicked(
            move |tab_id: SharedString, remote_dir: SharedString, folder: bool| {
                let tab_id = tab_id.to_string();
                let remote_dir = remote_dir.to_string();
                let sftp_handles = sftp_handles.clone();
                // Gather sync targets on the UI thread (Slint models aren't Send).
                let sync_targets = weak
                    .upgrade()
                    .map(|w| upload_sync_targets(&w, &sftp_handles, &tab_id))
                    .unwrap_or_default();
                let owner = weak
                    .upgrade()
                    .map(|w| DialogOwner::of(w.window()))
                    .unwrap_or_default();
                std::thread::spawn(move || {
                    // The remote SFTP upload handles a file or a whole directory;
                    // only the local picker differs (#85). Folder uploads one dir;
                    // file mode allows selecting several at once.
                    let locals: Vec<std::path::PathBuf> = if folder {
                        owner
                            .file()
                            .pick_folder()
                            .map(|p| vec![p])
                            .unwrap_or_default()
                    } else {
                        owner
                            .file()
                            .pick_files()
                            .map(|v| v.into_iter().collect())
                            .unwrap_or_default()
                    };
                    if locals.is_empty() {
                        return;
                    }
                    queue_uploads(&sftp_handles, &tab_id, &remote_dir, &locals, &sync_targets);
                });
            },
        );
    }

    // Paste files / folders copied in the OS file manager or an IDE to upload
    // them into the remote directory (#508). Right-click › Paste, or the paste
    // chord while the file list has focus.
    {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_paste_upload(move |tab_id: SharedString, remote_dir: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            let tab_id = tab_id.to_string();
            let remote_dir = remote_dir.to_string();
            if remote_dir.is_empty() {
                return;
            }
            let locals = clipboard_upload_paths();
            if locals.is_empty() {
                set_sftp_status(
                    &w,
                    &tab_id,
                    t(
                        "剪贴板里没有文件。请先在文件管理器中复制文件或文件夹，再粘贴到这里。",
                        "No files on the clipboard. Copy files or folders in your file manager, then paste them here.",
                    ),
                );
                return;
            }
            let sync_targets = upload_sync_targets(&w, &sftp_handles, &tab_id);
            queue_uploads(&sftp_handles, &tab_id, &remote_dir, &locals, &sync_targets);
            w.set_download_open(true);
            set_sftp_status(
                &w,
                &tab_id,
                &format!(
                    "{} {} → {remote_dir}",
                    t("正在上传", "Uploading"),
                    upload_summary(&locals)
                ),
            );
        });
    }

    // Refresh the current directory listing.
    {
        let sftp_handles = sftp_handles.clone();
        // Added (#sftp-refresh-selection): also need `weak` so we can drop the
        // selection counter back to zero after the file list is rebuilt. Without
        // this, the toolbar kept showing the old `sftp_selected_count` (e.g. "2")
        // even though the freshly loaded entries were all unchecked, and the
        // batch-download / batch-delete buttons stayed visible until the user
        // manually toggled a checkbox to force a recount.
        let weak = window.as_weak();
        window.on_sftp_refresh(move |tab_id: SharedString, path: SharedString| {
            let tab_id = tab_id.to_string();
            let path = path.to_string();
            if let Ok(handles) = sftp_handles.lock() {
                if let Some(h) = handles.get(&tab_id) {
                    // Refresh re-syncs the left tree too, not just the file list (#189).
                    h.refresh_dir(path);
                }
            }
            // Reset the multi-select counter to match the new (empty) selection.
            // The list itself gets replaced by `refresh_dir` with fresh entries
            // whose `selected=false`, so this only really needs to clamp the
            // cached count, but reusing `clear_sftp_selection` keeps the
            // post-refresh state identical to what other actions (navigate,
            // download-selected, delete-selected, copy-selected-to-target) leave
            // behind — single source of truth (#sftp-refresh-selection).
            if let Some(w) = weak.upgrade() {
                let terminals = w.get_terminals();
                if let Some(tm) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() {
                    clear_sftp_selection(tm, &tab_id);
                }
            }
        });
    }

    // Toggle tree node expand/collapse and navigate to that directory.
    {
        let sftp_handles = sftp_handles.clone();
        let sftp_last_cwd = sftp_last_cwd.clone();
        window.on_sftp_tree_expand(move |tab_id: SharedString, path: SharedString| {
            let tab_id = tab_id.to_string();
            let path = path.to_string();
            // Forget the followed cwd (see on_sftp_navigate): tree navigation
            // must never permanently disable cd-follow.
            sftp_last_cwd.lock().unwrap().remove(&tab_id);
            if let Ok(handles) = sftp_handles.lock() {
                if let Some(h) = handles.get(&tab_id) {
                    h.toggle_tree_node(path.clone());
                    h.list_dir(path);
                }
            }
        });
    }

    // Context menu → 删除 a remote file. The irreversible-delete confirmation
    // (#28) is handled by the in-app ConfirmDialog in the UI layer, so by the
    // time this fires the user has already confirmed.
    {
        let sftp_handles = sftp_handles.clone();
        window.on_sftp_delete(move |tab_id: SharedString, path: SharedString| {
            if let Ok(handles) = sftp_handles.lock() {
                if let Some(h) = handles.get(tab_id.as_str()) {
                    h.delete(path.to_string());
                }
            }
        });
    }

    // SFTP file-list sorting (#248): click a header to cycle asc -> desc -> default.
    {
        let weak = window.as_weak();
        window.on_sftp_sort_request(move |tab_id: SharedString, key: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            let terminals = w.get_terminals();
            let Some(tm) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            update_terminal_row(tm, tab_id.as_str(), |row| {
                let key = key.to_string();
                let next_dir = if row.sftp_sort_key.as_str() != key || row.sftp_sort_dir == 0 {
                    1
                } else if row.sftp_sort_dir > 0 {
                    -1
                } else {
                    0
                };
                let next_key = if next_dir == 0 { String::new() } else { key };
                row.sftp_entries =
                    sorted_sftp_entries_from_model(&row.sftp_entries, &next_key, next_dir);
                row.sftp_sort_key = next_key.into();
                row.sftp_sort_dir = next_dir;
            });
        });
    }
    {
        let weak = window.as_weak();
        window.on_sftp_clear_sort(move |tab_id: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            let terminals = w.get_terminals();
            let Some(tm) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            update_terminal_row(tm, tab_id.as_str(), |row| {
                row.sftp_entries = sorted_sftp_entries_from_model(&row.sftp_entries, "", 0);
                row.sftp_sort_key = "".into();
                row.sftp_sort_dir = 0;
            });
        });
    }

    // SFTP multi-select: toggle a row's checkbox + recount (#100).
    {
        let weak = window.as_weak();
        window.on_sftp_toggle_select(move |tab_id: SharedString, idx: i32| {
            let Some(w) = weak.upgrade() else { return };
            let terminals = w.get_terminals();
            let Some(tm) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            for ti in 0..tm.row_count() {
                let Some(row) = tm.row_data(ti) else { continue };
                if row.id.as_str() != tab_id.as_str() {
                    continue;
                }
                if let Some(em) = row
                    .sftp_entries
                    .as_any()
                    .downcast_ref::<VecModel<SftpEntry>>()
                {
                    let i = idx as usize;
                    if let Some(mut e) = em.row_data(i) {
                        e.selected = !e.selected;
                        em.set_row_data(i, e);
                    }
                    let mut n = 0;
                    for ei in 0..em.row_count() {
                        if em.row_data(ei).map(|x| x.selected).unwrap_or(false) {
                            n += 1;
                        }
                    }
                    let mut r = row.clone();
                    r.sftp_selected_count = n;
                    tm.set_row_data(ti, r);
                }
                break;
            }
        });
    }
    // SFTP multi-select: Shift+click range. Like a file manager, the checked
    // set becomes exactly rows `from..=to` (either order); everything else is
    // unchecked.
    {
        let weak = window.as_weak();
        window.on_sftp_select_range(move |tab_id: SharedString, from: i32, to: i32| {
            let Some(w) = weak.upgrade() else { return };
            let terminals = w.get_terminals();
            let Some(tm) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            for ti in 0..tm.row_count() {
                let Some(row) = tm.row_data(ti) else { continue };
                if row.id.as_str() != tab_id.as_str() {
                    continue;
                }
                if let Some(em) = row
                    .sftp_entries
                    .as_any()
                    .downcast_ref::<VecModel<SftpEntry>>()
                {
                    let n = apply_range_selection(em, from, to);
                    let mut r = row.clone();
                    r.sftp_selected_count = n as i32;
                    tm.set_row_data(ti, r);
                }
                break;
            }
        });
    }
    // Drag rows out of the file list into the OS file manager (#504).
    // Windows and macOS; on Linux the gesture is ignored.
    if super::drag_out::SUPPORTED {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_drag_out(move |tab_id: SharedString, row: i32| {
            let Some(w) = weak.upgrade() else { return };
            let items = drag_out_items(&w, tab_id.as_str(), row);
            let commands = sftp_handles
                .lock()
                .ok()
                .and_then(|h| h.get(tab_id.as_str()).map(|h| h.commands.clone()));
            let Some(commands) = commands.filter(|_| !items.is_empty()) else {
                return;
            };
            // The OS drag consumed the button-up; release Slint's pointer
            // grab so the row doesn't stay "pressed". Off-window, so it
            // never counts as a click.
            let release = |weak: slint::Weak<AppWindow>| {
                if let Some(w) = weak.upgrade() {
                    w.window().dispatch_event(slint::platform::WindowEvent::PointerReleased {
                        position: slint::LogicalPosition::new(-1.0, -1.0),
                        button: slint::platform::PointerEventButton::Left,
                    });
                }
            };
            let weak = w.as_weak();
            if cfg!(target_os = "macos") {
                // AppKit starts the session from the mouse event being
                // handled right now and returns at once; the drag then runs
                // on its own.
                if super::drag_out::start(w.window(), commands, items) {
                    slint::Timer::single_shot(std::time::Duration::ZERO, move || release(weak));
                }
            } else {
                // Start on a fresh event-loop turn rather than inside Slint's
                // pointer-move dispatch: DoDragDrop runs its own modal loop.
                slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                    if let Some(w) = weak.upgrade() {
                        super::drag_out::start(w.window(), commands, items);
                    }
                    release(weak);
                });
            }
        });
    }
    // SFTP multi-select: download all checked entries into one folder (#100).
    {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_download_selected(move |tab_id: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            let terminals = w.get_terminals();
            let Some(tm) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            let paths = collect_sftp_selected(tm, tab_id.as_str());
            if paths.is_empty() {
                return;
            }
            // Single selection downloads as a plain file (no compression, #100.3);
            // multiple selections are tar-packed into one archive on the remote
            // (#100.2) — this also avoids the concurrent-transfer races (#100.1).
            let single = paths.len() == 1;
            let remote_dir = active_sftp_path(&w, tab_id.as_str());
            let names: Vec<String> = paths
                .iter()
                .map(|p| {
                    p.trim_end_matches('/')
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(p)
                        .to_string()
                })
                .collect();
            let owner = DialogOwner::of(w.window());
            let preset = w.get_download_dir().to_string();
            let always_ask = w.get_download_always_ask();
            if !always_ask && !preset.is_empty() {
                if let Ok(handles) = sftp_handles.lock() {
                    if let Some(h) = handles.get(tab_id.as_str()) {
                        if single {
                            if let Some(conflict) =
                                choose_download_conflict(owner, &paths[0], &preset)
                            {
                                h.download(paths[0].clone(), preset.clone(), conflict);
                            }
                        } else {
                            h.download_archive(remote_dir.clone(), names.clone(), preset.clone());
                        }
                    }
                }
                w.set_download_open(true);
            } else {
                let sftp_handles = sftp_handles.clone();
                let weak2 = weak.clone();
                let tab = tab_id.to_string();
                std::thread::spawn(move || {
                    if let Some(dir) = owner.file().pick_folder() {
                        let dir = dir.to_string_lossy().to_string();
                        if let Ok(handles) = sftp_handles.lock() {
                            if let Some(h) = handles.get(&tab) {
                                if single {
                                    if let Some(conflict) =
                                        choose_download_conflict(owner, &paths[0], &dir)
                                    {
                                        h.download(paths[0].clone(), dir.clone(), conflict);
                                    }
                                } else {
                                    h.download_archive(
                                        remote_dir.clone(),
                                        names.clone(),
                                        dir.clone(),
                                    );
                                }
                            }
                        }
                        let _ = weak2.upgrade_in_event_loop(|w| w.set_download_open(true));
                    }
                });
            }
            clear_sftp_selection(tm, tab_id.as_str());
        });
    }
    // SFTP multi-select: delete all checked entries (confirmed in the UI) (#100).
    {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_delete_selected(move |tab_id: SharedString| {
            let Some(w) = weak.upgrade() else { return };
            let terminals = w.get_terminals();
            let Some(tm) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            let paths = collect_sftp_selected(tm, tab_id.as_str());
            if paths.is_empty() {
                return;
            }
            if let Ok(handles) = sftp_handles.lock() {
                if let Some(h) = handles.get(tab_id.as_str()) {
                    for p in &paths {
                        h.delete(p.clone());
                    }
                }
            }
            clear_sftp_selection(tm, tab_id.as_str());
        });
    }

    // Context menu → 查看 (read-only) / 编辑 (editable). Both load the file's
    // text into the built-in editor instead of an external app (#70).
    // SFTP remote-to-remote copy (#203): stage through a local temp directory,
    // then upload into the target session's current SFTP directory.
    {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_copy_to_target(
            move |tab_id: SharedString, remote_path: SharedString, target_id: SharedString| {
                let Some(w) = weak.upgrade() else { return };
                let paths = vec![remote_path.to_string()];
                let dirs = terminal_sftp_paths(&w);
                let target_dir = dirs
                    .get(target_id.as_str())
                    .cloned()
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| "/".to_string());
                if let Ok(handles) = sftp_handles.lock() {
                    let Some(src) = handles.get(tab_id.as_str()) else {
                        return;
                    };
                    let Some(dst) = handles.get(target_id.as_str()) else {
                        return;
                    };
                    src.copy_to(paths, dst.commands.clone(), target_dir);
                    w.set_download_open(true);
                }
            },
        );
    }
    {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_copy_selected_to_target(
            move |tab_id: SharedString, target_id: SharedString| {
                let Some(w) = weak.upgrade() else { return };
                let terminals = w.get_terminals();
                let Some(tm) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                    return;
                };
                let paths = collect_sftp_selected(tm, tab_id.as_str());
                if paths.is_empty() {
                    return;
                }
                let dirs = terminal_sftp_paths(&w);
                let target_dir = dirs
                    .get(target_id.as_str())
                    .cloned()
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| "/".to_string());
                if let Ok(handles) = sftp_handles.lock() {
                    let Some(src) = handles.get(tab_id.as_str()) else {
                        return;
                    };
                    let Some(dst) = handles.get(target_id.as_str()) else {
                        return;
                    };
                    src.copy_to(paths, dst.commands.clone(), target_dir);
                    w.set_download_open(true);
                }
                clear_sftp_selection(tm, tab_id.as_str());
            },
        );
    }
    {
        let sftp_handles = sftp_handles.clone();
        window.on_sftp_view(move |tab_id: SharedString, path: SharedString| {
            if let Ok(handles) = sftp_handles.lock() {
                if let Some(h) = handles.get(tab_id.as_str()) {
                    h.read_text(path.to_string(), false);
                }
            }
        });
    }
    // Double-click on a file (#493): edit it if it is text, else download.
    {
        let sftp_handles = sftp_handles.clone();
        window.on_sftp_open_file(move |tab_id: SharedString, path: SharedString| {
            if let Ok(handles) = sftp_handles.lock() {
                if let Some(h) = handles.get(tab_id.as_str()) {
                    h.open_or_download(path.to_string());
                }
            }
        });
    }
    {
        let sftp_handles = sftp_handles.clone();
        window.on_sftp_edit(move |tab_id: SharedString, path: SharedString| {
            if let Ok(handles) = sftp_handles.lock() {
                if let Some(h) = handles.get(tab_id.as_str()) {
                    h.read_text(path.to_string(), true);
                }
            }
        });
    }
    // Open / edit with an external program (#81): download to a temp file and
    // hand it to the OS default app. Edit mode watches the temp copy and
    // re-uploads on every change.
    {
        let sftp_handles = sftp_handles.clone();
        window.on_sftp_open_external(move |tab_id: SharedString, path: SharedString| {
            if let Ok(handles) = sftp_handles.lock() {
                if let Some(h) = handles.get(tab_id.as_str()) {
                    h.open_temp(path.to_string(), false);
                }
            }
        });
    }
    {
        let sftp_handles = sftp_handles.clone();
        window.on_sftp_edit_external(move |tab_id: SharedString, path: SharedString| {
            if let Ok(handles) = sftp_handles.lock() {
                if let Some(h) = handles.get(tab_id.as_str()) {
                    h.open_temp(path.to_string(), true);
                }
            }
        });
    }

    // Context-menu extensions (#69): one prompt dialog covers rename / chmod /
    // mkdir / touch; copy-path goes straight to the system clipboard.
    {
        let sftp_handles = sftp_handles.clone();
        window.on_sftp_prompt_submit(
            move |tab_id: SharedString,
                  kind: SharedString,
                  target: SharedString,
                  value: SharedString| {
                let value = value.to_string();
                let value = value.trim();
                if value.is_empty() {
                    return;
                }
                let target = target.to_string();
                let handles = match sftp_handles.lock() {
                    Ok(h) => h,
                    Err(_) => return,
                };
                let Some(h) = handles.get(tab_id.as_str()) else {
                    return;
                };
                match kind.as_str() {
                    "rename" => {
                        let to =
                            format!("{}/{}", parent_path(&target).trim_end_matches('/'), value);
                        h.rename(target, to);
                    }
                    "mkdir" => {
                        h.mkdir(format!("{}/{}", target.trim_end_matches('/'), value));
                    }
                    "touch" => {
                        h.touch(format!("{}/{}", target.trim_end_matches('/'), value));
                    }
                    _ => {}
                }
            },
        );
    }
    {
        // Run on a spawned thread, NOT the Slint UI thread. clipboard_set_text
        // (arboard) can block when the OS clipboard is held by another process
        // (Windows OpenClipboard / Wayland wait()); doing it synchronously here
        // freezes the whole UI event loop (#sftp-copy-path-freeze). Every other
        // caller in app.rs wraps clipboard writes in std::thread::spawn — this
        // one was the only outlier.
        window.on_sftp_copy_path(move |path: SharedString| {
            std::thread::spawn(move || clipboard_set_text(path.to_string()));
        });
    }

    // Visual chmod dialog (#84): decompose the current mode into nine bools on
    // open, recompose on apply (Slint has no bitwise ops).
    {
        let weak = window.as_weak();
        window.on_sftp_chmod_open(
            move |tab: SharedString, path: SharedString, name: SharedString, mode: i32| {
                let Some(w) = weak.upgrade() else { return };
                let m = mode as u32;
                w.set_chmod_tab(tab);
                w.set_chmod_path(path);
                w.set_chmod_name(name);
                w.set_chmod_or(m & 0o400 != 0);
                w.set_chmod_ow(m & 0o200 != 0);
                w.set_chmod_ox(m & 0o100 != 0);
                w.set_chmod_gr(m & 0o040 != 0);
                w.set_chmod_gw(m & 0o020 != 0);
                w.set_chmod_gx(m & 0o010 != 0);
                w.set_chmod_tr(m & 0o004 != 0);
                w.set_chmod_tw(m & 0o002 != 0);
                w.set_chmod_tx(m & 0o001 != 0);
                w.set_chmod_open(true);
            },
        );
    }
    {
        let sftp_handles = sftp_handles.clone();
        let weak = window.as_weak();
        window.on_sftp_chmod_apply(move || {
            let Some(w) = weak.upgrade() else { return };
            let mode = (w.get_chmod_or() as u32) << 8
                | (w.get_chmod_ow() as u32) << 7
                | (w.get_chmod_ox() as u32) << 6
                | (w.get_chmod_gr() as u32) << 5
                | (w.get_chmod_gw() as u32) << 4
                | (w.get_chmod_gx() as u32) << 3
                | (w.get_chmod_tr() as u32) << 2
                | (w.get_chmod_tw() as u32) << 1
                | (w.get_chmod_tx() as u32);
            let path = w.get_chmod_path().to_string();
            let tab = w.get_chmod_tab().to_string();
            if let Ok(handles) = sftp_handles.lock() {
                if let Some(h) = handles.get(&tab) {
                    h.chmod(path, mode);
                }
            }
        });
    }

    // Split (dual-pane) view: local browsing + two-way transfers.
    wire_sftp_split_callbacks(window, &sftp_handles);

    // Built-in editor (#70), detached-window mode. Editor tabs (#494) are
    // wired in editor_tabs::wire_editor_tabs.
    wire_editor_window(editor, sftp_handles);
}

// ---------------------------------------------------------------------------
// Raw keystroke forwarding and PTY resize
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Upload helpers shared by the upload button and paste-to-upload (#508)
// ---------------------------------------------------------------------------

/// Session-sync upload (#sync): when both the sync toggle and the "sync
/// upload" setting are on, every other online session also receives the
/// upload, each into *its own* current SFTP directory (paths differ between
/// sessions, e.g. /home/jeff vs /home/root). Empty otherwise.
fn upload_sync_targets(
    w: &AppWindow,
    sftp_handles: &SftpHandles,
    tab_id: &str,
) -> Vec<(String, String)> {
    if !(w.get_sync_input() && w.get_sync_upload_enabled()) {
        return Vec::new();
    }
    let paths = terminal_sftp_paths(w);
    let Ok(handles) = sftp_handles.lock() else {
        return Vec::new();
    };
    handles
        .keys()
        .filter(|id| id.as_str() != tab_id)
        .filter_map(|id| paths.get(id).map(|dir| (id.clone(), dir.clone())))
        .filter(|(_, dir)| !dir.is_empty())
        .collect()
}

/// Queue `locals` (files or folders) for upload into `remote_dir` of
/// `tab_id`, and into each sync target's own directory.
fn queue_uploads(
    sftp_handles: &SftpHandles,
    tab_id: &str,
    remote_dir: &str,
    locals: &[std::path::PathBuf],
    sync_targets: &[(String, String)],
) {
    let Ok(handles) = sftp_handles.lock() else {
        return;
    };
    if let Some(h) = handles.get(tab_id) {
        for local in locals {
            h.upload(local.clone(), remote_dir.to_string());
        }
    }
    for (id, dir) in sync_targets {
        if let Some(h) = handles.get(id) {
            for local in locals {
                h.upload(local.clone(), dir.clone());
            }
        }
    }
}

/// The files and folders on the OS clipboard (Explorer / Finder / an IDE's
/// "Copy"), or nothing when it holds no file list.
fn clipboard_upload_paths() -> Vec<std::path::PathBuf> {
    let listed = arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.get().file_list())
        .unwrap_or_default();
    upload_candidates(listed, |path| path.exists())
}

/// Keep the paths that still exist, in order, without duplicates.
fn upload_candidates(
    paths: Vec<std::path::PathBuf>,
    exists: impl Fn(&std::path::Path) -> bool,
) -> Vec<std::path::PathBuf> {
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .filter(|path| exists(path) && seen.insert(path.clone()))
        .collect()
}

/// The first path's name and how many more paths follow.
fn upload_summary_parts(paths: &[std::path::PathBuf]) -> Option<(String, usize)> {
    let first = paths.first()?;
    let name = first
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| first.to_string_lossy().into_owned());
    Some((name, paths.len() - 1))
}

/// "app.jar" for one path; "app.jar and 2 more" / "app.jar 等 3 项" for several.
fn upload_summary(paths: &[std::path::PathBuf]) -> String {
    match upload_summary_parts(paths) {
        None => String::new(),
        Some((name, 0)) => name,
        Some((name, more)) if crate::i18n::is_en() => format!("{name} and {more} more"),
        Some((name, more)) => format!("{name} 等 {} 项", more + 1),
    }
}

/// Show `message` in the SFTP status line of `tab_id`.
fn set_sftp_status(w: &AppWindow, tab_id: &str, message: &str) {
    use slint::Model as _;
    let terminals = w.get_terminals();
    for i in 0..terminals.row_count() {
        if let Some(mut row) = terminals.row_data(i) {
            if row.id.as_str() == tab_id {
                row.sftp_status = message.into();
                terminals.set_row_data(i, row);
                return;
            }
        }
    }
}

#[cfg(test)]
mod paste_upload_tests {
    use super::{upload_candidates, upload_summary_parts};
    use std::path::PathBuf;

    /// Real OS clipboard: copy two files as a file list (what Explorer /
    /// Finder do) and read them back. Touches the clipboard, so ignored by
    /// default: `cargo test -- --ignored clipboard_file_list_round_trip`.
    #[test]
    #[ignore]
    fn clipboard_file_list_round_trip() {
        let dir = std::env::temp_dir().join(format!("meatshell-paste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("app.jar");
        let b = dir.join("conf");
        std::fs::write(&a, b"jar").unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let mut clipboard = arboard::Clipboard::new().unwrap();
        let saved_text = clipboard.get_text().ok();
        clipboard.set().file_list(&[&a, &b]).unwrap();
        let read = super::clipboard_upload_paths();
        // Put the user's clipboard text back before asserting.
        if let Some(text) = saved_text {
            let _ = clipboard.set_text(text);
        }
        let _ = std::fs::remove_dir_all(&dir);
        let names: Vec<_> = read.iter().filter_map(|p| p.file_name()).collect();
        assert_eq!(names, ["app.jar", "conf"], "{read:?}");
    }

    #[test]
    fn candidates_keep_existing_paths_in_order_without_duplicates() {
        let paths = vec![
            PathBuf::from("C:/build/app.jar"),
            PathBuf::from("C:/gone.txt"),
            PathBuf::from("C:/build/conf"),
            PathBuf::from("C:/build/app.jar"),
        ];
        let kept = upload_candidates(paths, |p| !p.ends_with("gone.txt"));
        assert_eq!(
            kept,
            [PathBuf::from("C:/build/app.jar"), PathBuf::from("C:/build/conf")]
        );
        assert!(upload_candidates(Vec::new(), |_| true).is_empty());
    }

    #[test]
    fn summary_names_the_first_file_and_counts_the_rest() {
        assert_eq!(upload_summary_parts(&[]), None);
        assert_eq!(
            upload_summary_parts(&[PathBuf::from("/a/app.jar")]),
            Some(("app.jar".to_string(), 0))
        );
        assert_eq!(
            upload_summary_parts(&[
                PathBuf::from("/a/app.jar"),
                PathBuf::from("/a/b"),
                PathBuf::from("c"),
            ]),
            Some(("app.jar".to_string(), 2))
        );
    }
}
