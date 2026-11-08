//! OS file drag-and-drop onto terminal / SFTP panels (uploads).

use super::*;

/// Current mouse cursor position in physical screen pixels (Windows).
#[cfg(windows)]
pub(super) fn cursor_pos() -> Option<(i32, i32)> {
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    extern "system" {
        fn GetCursorPos(p: *mut Point) -> i32;
    }
    let mut p = Point { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut p) } != 0 {
        Some((p.x, p.y))
    } else {
        None
    }
}

/// Handle an OS file drop: if it landed over the terminal panel (the shell page)
/// of the active session tab, upload the file to that tab's current remote
/// directory.
///
/// `hovered_pos` is the caller's best guess at the drop point in logical
/// window-client coordinates, taken from the most recent `CursorMoved` event
/// (see the `WEvent::DroppedFile` handler). Windows ignores it and queries
/// the OS cursor directly instead: Win32 suppresses `WM_MOUSEMOVE` for the
/// window while an OLE drag-and-drop is in progress, so `CursorMoved` can be
/// stale by the time the drop lands. macOS and X11/Wayland do deliver real
/// pointer motion during drag-hover as ordinary `CursorMoved` events, so the
/// last one seen is accurate there — this is what previously left
/// drag-and-drop upload a no-op on every non-Windows platform (#356).
#[cfg(windows)]
pub(super) fn handle_file_drop(
    win: &AppWindow,
    sftp_handles: &SftpHandles,
    path: std::path::PathBuf,
    _hovered_pos: Option<(f32, f32)>,
) {
    let active = win.get_active_tab_id().to_string();
    if active == "welcome" {
        return;
    }
    let w = win.window();
    let scale = w.scale_factor().max(0.01);
    let Some(inner) = w.with_winit_window(|ww| ww.inner_position().ok()).flatten() else {
        return;
    };
    let Some((cx, cy)) = cursor_pos() else {
        return;
    };
    // Drop point in logical client coordinates.
    let client_x = (cx - inner.x) as f32 / scale;
    let client_y = (cy - inner.y) as f32 / scale;
    handle_file_drop_at(win, sftp_handles, path, client_x, client_y);
}

#[cfg(not(windows))]
pub(super) fn handle_file_drop(
    win: &AppWindow,
    sftp_handles: &SftpHandles,
    path: std::path::PathBuf,
    hovered_pos: Option<(f32, f32)>,
) {
    let Some((client_x, client_y)) = hovered_pos else {
        // No CursorMoved was ever observed for this drag (e.g. the file was
        // dropped the instant it entered the window). Without a position we
        // cannot tell which panel it landed on, so do nothing rather than
        // guess — matches the previous no-op rather than uploading to the
        // wrong place.
        return;
    };
    handle_file_drop_at(win, sftp_handles, path, client_x, client_y);
}

/// Shared drop-position → upload logic for both platform front-ends above.
pub(super) fn handle_file_drop_at(
    win: &AppWindow,
    sftp_handles: &SftpHandles,
    path: std::path::PathBuf,
    client_x: f32,
    client_y: f32,
) {
    let active = win.get_active_tab_id().to_string();
    if active == "welcome" {
        return;
    }
    // Accept drops anywhere over the whole terminal panel ("shell page"), so
    // dragging a file onto the terminal uploads it to the session's current
    // directory — not just onto the SFTP file list (#drag-onto-shell).
    let Some((_active, term, _term_state)) = active_terminal_panel_rects(win) else {
        return;
    };
    if !contains_logical(term, client_x, client_y) {
        return; // dropped outside the terminal panel — ignore
    }

    // Split view: dropping onto the *local* half copies the file into the local
    // folder that half is showing instead of uploading it to the remote one.
    if let Some((tab, dir)) = sftp_local_pane_hit(win, client_x, client_y) {
        copy_into(win, &tab, std::path::PathBuf::from(dir), path);
        return;
    }

    let dir = active_sftp_path(win, &active);
    if dir.is_empty() {
        return;
    }
    // Session-sync (#sync): when both toggles are on, also mirror the drop to
    // every other online session — each into *its own* current SFTP dir. This
    // matches the upload button's behaviour (drag-and-drop is a separate path).
    let sync = win.get_sync_input() && win.get_sync_upload_enabled();
    let other_dirs = if sync {
        terminal_sftp_paths(win)
    } else {
        HashMap::new()
    };
    // Ask once if the file name is already taken remotely (dropping a file onto
    // the terminal used to overwrite silently). Done before taking the handles
    // lock: the dialog blocks, and nothing else may wait on that mutex.
    let Some(conflict) = resolve_upload_conflict(win, &active, std::slice::from_ref(&path)) else {
        return;
    };
    if let Ok(handles) = sftp_handles.lock() {
        if let Some(h) = handles.get(&active) {
            win.set_download_open(true);
            h.upload(path.clone(), dir, conflict);
        }
        if sync {
            for (id, h) in handles.iter() {
                if id == &active {
                    continue;
                }
                if let Some(d) = other_dirs.get(id).filter(|d| !d.is_empty()) {
                    h.upload(path.clone(), d.clone(), conflict);
                }
            }
        }
    }
}
