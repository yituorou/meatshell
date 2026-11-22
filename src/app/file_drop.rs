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

/// The OS delivers a drop of several files / folders as one `DroppedFile`
/// event per item, all in the same burst. They are collected here and handled
/// together once the burst is over, so the drop asks its name-conflict
/// question once instead of once per item (#572).
#[derive(Default)]
pub(super) struct PendingDrop {
    paths: Vec<std::path::PathBuf>,
    /// Drop point of the burst's first item, logical client coordinates.
    at: Option<(f32, f32)>,
}

/// How long after the first item of a drop the rest are waited for. The items
/// arrive back to back within one event-loop pass; this only has to outlast it.
const DROP_BURST: std::time::Duration = std::time::Duration::from_millis(60);

/// Queue one dropped path; the first of a burst schedules the batch.
pub(super) fn queue_file_drop(
    win: &AppWindow,
    sftp_handles: &SftpHandles,
    pending: &std::rc::Rc<RefCell<PendingDrop>>,
    path: std::path::PathBuf,
    hovered_pos: Option<(f32, f32)>,
) {
    let first = {
        let mut batch = pending.borrow_mut();
        let first = batch.paths.is_empty();
        if first {
            batch.at = drop_point(win, hovered_pos);
        }
        batch.paths.push(path);
        first
    };
    if !first {
        return;
    }
    let weak = win.as_weak();
    let sftp_handles = sftp_handles.clone();
    let pending = pending.clone();
    slint::Timer::single_shot(DROP_BURST, move || {
        let batch = std::mem::take(&mut *pending.borrow_mut());
        let (Some(win), Some((x, y))) = (weak.upgrade(), batch.at) else {
            return;
        };
        handle_file_drop_at(&win, &sftp_handles, batch.paths, x, y);
    });
}

/// Where a drop landed, in logical window-client coordinates.
///
/// `hovered_pos` is the caller's best guess, taken from the most recent
/// `CursorMoved` event (see the `WEvent::DroppedFile` handler). Windows ignores
/// it and queries the OS cursor directly instead: Win32 suppresses
/// `WM_MOUSEMOVE` for the window while an OLE drag-and-drop is in progress, so
/// `CursorMoved` can be stale by the time the drop lands. macOS and
/// X11/Wayland do deliver real pointer motion during drag-hover as ordinary
/// `CursorMoved` events, so the last one seen is accurate there — this is what
/// previously left drag-and-drop upload a no-op on every non-Windows platform
/// (#356).
#[cfg(windows)]
fn drop_point(win: &AppWindow, _hovered_pos: Option<(f32, f32)>) -> Option<(f32, f32)> {
    let w = win.window();
    let scale = w.scale_factor().max(0.01);
    let inner = w.with_winit_window(|ww| ww.inner_position().ok()).flatten()?;
    let (cx, cy) = cursor_pos()?;
    Some(((cx - inner.x) as f32 / scale, (cy - inner.y) as f32 / scale))
}

/// `None` when no CursorMoved was ever observed for this drag (e.g. the file
/// was dropped the instant it entered the window): without a position we
/// cannot tell which panel it landed on, so the drop is ignored rather than
/// uploaded to the wrong place.
#[cfg(not(windows))]
fn drop_point(_win: &AppWindow, hovered_pos: Option<(f32, f32)>) -> Option<(f32, f32)> {
    hovered_pos
}

/// Shared drop-position → upload logic for one drop of any number of items.
pub(super) fn handle_file_drop_at(
    win: &AppWindow,
    sftp_handles: &SftpHandles,
    paths: Vec<std::path::PathBuf>,
    client_x: f32,
    client_y: f32,
) {
    if paths.is_empty() {
        return;
    }
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
        for path in paths {
            copy_into(win, &tab, std::path::PathBuf::from(&dir), path);
        }
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
    // Ask once, for the whole drop, if any name is already taken remotely
    // (dropping onto the terminal used to overwrite silently; asking per item
    // meant one dialog per file of a dropped build, #572). Done before taking
    // the handles lock: the dialog blocks, and nothing else may wait on that
    // mutex.
    let Some(conflict) = resolve_upload_conflict(win, &active, &paths) else {
        set_sftp_status(win, &active, t("已取消上传", "Upload cancelled"));
        return;
    };
    if let Ok(handles) = sftp_handles.lock() {
        if let Some(h) = handles.get(&active) {
            win.set_download_open(true);
            for path in &paths {
                h.upload(path.clone(), dir.clone(), conflict);
            }
        }
        if sync {
            for (id, h) in handles.iter() {
                if id == &active {
                    continue;
                }
                if let Some(d) = other_dirs.get(id).filter(|d| !d.is_empty()) {
                    for path in &paths {
                        h.upload(path.clone(), d.clone(), conflict);
                    }
                }
            }
        }
    }
}
