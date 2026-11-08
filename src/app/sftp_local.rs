//! Local-filesystem side of the split (dual-pane) SFTP view.
//!
//! The right-hand pane of the split view is the usual remote listing owned by
//! the SFTP worker; the left-hand pane browses the machine MeatShell runs on.
//! Local listings are plain synchronous filesystem calls, so they run on a
//! short-lived thread and are published back on the Slint event loop (the same
//! pattern the file dialogs use) — a slow network drive can therefore never
//! block the UI thread.
//!
//! Uploads reuse the worker's existing `Upload` command (one command per path,
//! files and folders alike) and downloads go through `DownloadInto`, which
//! writes everything straight into the folder the pane is showing.

use super::*;

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

/// A local listing can be huge (`C:\Windows\WinSxS` has 100k+ entries) and the
/// Slint list materialises every row, so cap what one directory shows.
pub(super) const MAX_LOCAL_ENTRIES: usize = 5_000;

/// One completed local directory read.
#[derive(Debug, Default)]
pub(super) struct LocalListing {
    pub path: String,
    pub entries: Vec<SftpEntry>,
    /// More than [`MAX_LOCAL_ENTRIES`] items were present; only the first are shown.
    pub truncated: bool,
}

/// The user's home directory (the local pane's starting point).
pub(super) fn home_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        // Last resort: the drive/directory the process started in.
        .unwrap_or_else(|| PathBuf::from(std::path::MAIN_SEPARATOR.to_string()))
}

/// Home directory as the pane's path string.
pub(super) fn home_display() -> String {
    display_path(&home_dir())
}

/// Render an absolute path the way a user would type it.
///
/// Windows hands out verbatim paths (`\\?\C:\…`, `\\?\UNC\server\share\…`) from
/// some APIs; strip that prefix so the path bar stays readable and copy/pasteable.
pub(super) fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text
    }
}

/// Remove `.`/`..` components without touching the filesystem, so navigating up
/// from a drive root (`C:\` + `..`) stays at the root instead of failing.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // `pop` on a root (or prefix+root) refuses to climb out.
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(std::path::MAIN_SEPARATOR.to_string())
    } else {
        out
    }
}

/// Resolve what the user typed (or `..`, `~`) against the pane's current folder.
///
/// Pure path arithmetic: this never touches the filesystem. The listing thread
/// reports "no such folder" / "not a folder" instead, because probing the path
/// here ran on the UI thread and a dead mapped drive or sleeping NAS could
/// freeze the whole window while `std::fs::metadata` waited for a timeout.
pub(super) fn resolve_path(current: &str, input: &str) -> PathBuf {
    let trimmed = input.trim();
    let target = if trimmed.is_empty() || trimmed == "~" {
        home_dir()
    } else if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
    {
        home_dir().join(rest)
    } else {
        let typed = PathBuf::from(trimmed);
        if typed.is_absolute() || looks_like_windows_root(trimmed) {
            typed
        } else if current.is_empty() {
            typed
        } else {
            Path::new(current).join(typed)
        }
    };
    normalize(&target)
}

/// `C:` / `D:` without a separator are drive roots, not relative names.
fn looks_like_windows_root(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Parent folder of a directory, or `None` at a filesystem root.
pub(super) fn parent_of(path: &str) -> Option<PathBuf> {
    let parent = Path::new(path).parent()?;
    if parent.as_os_str().is_empty() {
        None
    } else {
        Some(parent.to_path_buf())
    }
}

/// Compare two local folder paths the way the OS would for the pane's purpose:
/// trailing separators are ignored, and Windows compares case-insensitively.
/// Deliberately lexical — no filesystem access, so a stale network share can't
/// stall the UI thread on a path comparison.
pub(super) fn same_dir(a: &str, b: &str) -> bool {
    let normalize = |path: &str| {
        let trimmed = path.trim_end_matches(['/', '\\']);
        if cfg!(windows) {
            trimmed.to_lowercase()
        } else {
            trimmed.to_string()
        }
    };
    normalize(a) == normalize(b)
}

/// Read one local directory into Slint rows (folders first, natural name order).
pub(super) fn list_dir(dir: &Path) -> Result<LocalListing, String> {
    let read = std::fs::read_dir(dir).map_err(|e| {
        format!(
            "{} {}: {e}",
            t("无法访问", "Cannot open"),
            display_path(dir)
        )
    })?;

    let mut entries = Vec::new();
    let mut truncated = false;
    for item in read {
        let Ok(item) = item else { continue };
        if entries.len() >= MAX_LOCAL_ENTRIES {
            truncated = true;
            break;
        }
        let full = item.path();
        let name = item.file_name().to_string_lossy().to_string();
        // `file_type` reports a symlink itself; `metadata` follows it, so a
        // link to a folder behaves like a folder when double-clicked.
        let is_link = item.file_type().map(|kind| kind.is_symlink()).unwrap_or(false);
        let meta = std::fs::metadata(&full).ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|delta| delta.as_secs().min(u32::MAX as u64) as u32)
            .unwrap_or(0);
        entries.push(SftpEntry {
            name: name.into(),
            full_path: display_path(&full).into(),
            is_dir,
            size: if is_dir {
                "".into()
            } else {
                format_size(size).into()
            },
            size_bytes: size as f32,
            modified: format_mtime(modified).into(),
            modified_ts: modified as f32,
            // Local files carry no POSIX metadata worth showing, so the pane
            // hides the permissions/owner/group columns entirely.
            mode: 0,
            file_type: local_type_label(is_dir, is_link),
            permissions: "".into(),
            permissions_mode: 0,
            owner: "".into(),
            group: "".into(),
            selected: false,
        });
    }
    sort_sftp_entries(&mut entries, "", 0);
    Ok(LocalListing {
        path: display_path(dir),
        entries,
        truncated,
    })
}

fn local_type_label(is_dir: bool, is_link: bool) -> SharedString {
    let label = if is_link {
        t("符号链接", "Symbolic link")
    } else if is_dir {
        t("文件夹", "Folder")
    } else {
        t("文件", "File")
    };
    label.into()
}

/// Publish a listing into a tab's local-pane fields.
fn publish_local_listing(
    window: &AppWindow,
    tab_id: &str,
    listing: LocalListing,
    note: Option<String>,
) {
    let terminals = window.get_terminals();
    let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
        return;
    };
    // The status line is reserved for what the path bar cannot say: a failed
    // listing, a truncated one, or the outcome of a local copy.
    let status = if listing.truncated {
        format!(
            "{} ({MAX_LOCAL_ENTRIES}+)",
            t("条目过多，仅显示前 5000 项", "Too many entries — showing the first 5000")
        )
    } else {
        note.unwrap_or_default()
    };
    let entries = ModelRc::from(Rc::new(VecModel::from(listing.entries)));
    let shown = listing.path.clone();
    let bookmarked = bookmarked_in(&model_strings(&window.get_local_bookmarks()), &shown);
    update_terminal_row(model, tab_id, |row| {
        row.sftp_local_path = shown.clone().into();
        row.sftp_local_entries = entries.clone();
        row.sftp_local_loading = false;
        row.sftp_local_selected_count = 0;
        row.sftp_local_status = status.into();
        row.sftp_local_bookmarked = bookmarked;
    });
    // Remember the folder for the next tab / restart (#532). Written to the
    // global property, persisted with the rest of the layout on close.
    window.set_sftp_last_local_dir(listing.path.into());
    // Most recent first, like the remote pane's history (#500).
    record_recent(window, &shown);
}

/// Read `dir` on a worker thread and publish it when it comes back.
///
/// The tab may have been closed (or the window gone) by then, in which case the
/// result is dropped.
pub(super) fn load_dir(window: &AppWindow, tab_id: &str, dir: PathBuf) {
    load_dir_with_status(window, tab_id, dir, None);
}

/// Like [`load_dir`], but keeps `note` in the pane's status line afterwards
/// (used to confirm a local copy without a second repaint).
pub(super) fn load_dir_with_status(
    window: &AppWindow,
    tab_id: &str,
    dir: PathBuf,
    note: Option<String>,
) {
    {
        let terminals = window.get_terminals();
        if let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() {
            // Only flip the spinner: the row area already paints "Loading...",
            // and writing a status here would pop the error strip in and out
            // on every navigation.
            update_terminal_row(model, tab_id, |row| row.sftp_local_loading = true);
        }
    }
    let weak = window.as_weak();
    let tab = tab_id.to_string();
    std::thread::spawn(move || {
        let result = list_dir(&dir);
        let _ = weak.upgrade_in_event_loop(move |window| match result {
            Ok(listing) => publish_local_listing(&window, &tab, listing, note),
            Err(message) => {
                let terminals = window.get_terminals();
                if let Some(model) =
                    terminals.as_any().downcast_ref::<VecModel<TerminalState>>()
                {
                    update_terminal_row(model, &tab, |row| {
                        row.sftp_local_loading = false;
                        row.sftp_local_status = message.into();
                    });
                }
            }
        });
    });
}

/// Open a remembered folder, falling back to `fallback` when it is gone.
///
/// Used the first time a pane opens with a folder restored from the config: a
/// removed USB stick or unmounted network drive should land on the home folder
/// instead of an error strip (#532). The existence check runs on the listing
/// thread, like every other local read.
pub(super) fn load_dir_or_fallback(
    window: &AppWindow,
    tab_id: &str,
    dir: PathBuf,
    fallback: PathBuf,
) {
    {
        let terminals = window.get_terminals();
        if let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() {
            update_terminal_row(model, tab_id, |row| row.sftp_local_loading = true);
        }
    }
    let weak = window.as_weak();
    let tab = tab_id.to_string();
    std::thread::spawn(move || {
        let (result, note) = match list_dir(&dir) {
            Ok(listing) => (Ok(listing), None),
            Err(message) => {
                if !same_dir(&display_path(&dir), &display_path(&fallback)) {
                    match list_dir(&fallback) {
                        Ok(listing) => (
                            Ok(listing),
                            Some(format!(
                                "{}: {message}",
                                t(
                                    "上次的本地目录不可用，已回到主目录",
                                    "Last local folder is gone — opened the home folder instead"
                                )
                            )),
                        ),
                        Err(_) => (Err(message), None),
                    }
                } else {
                    (Err(message), None)
                }
            }
        };
        let _ = weak.upgrade_in_event_loop(move |window| match result {
            Ok(listing) => publish_local_listing(&window, &tab, listing, note),
            Err(message) => {
                let terminals = window.get_terminals();
                if let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() {
                    update_terminal_row(model, &tab, |row| {
                        row.sftp_local_loading = false;
                        row.sftp_local_status = message.into();
                    });
                }
            }
        });
    });
}

/// Navigate the local pane: `..`, `~`, an absolute path, or a folder name.
///
/// Resolution is pure; the folder is read (and any "not found / not a folder"
/// error reported) by the listing thread.
pub(super) fn navigate(window: &AppWindow, tab_id: &str, input: &str) {
    let current = current_path(window, tab_id);
    load_dir(window, tab_id, resolve_path(&current, input));
}

/// Go to the parent folder (no-op at a drive root).
pub(super) fn navigate_up(window: &AppWindow, tab_id: &str) {
    let Some(parent) = parent_of(&current_path(window, tab_id)) else {
        return;
    };
    load_dir(window, tab_id, parent);
}

/// The folder a tab's local pane is showing ("" before it was ever opened).
pub(super) fn current_path(window: &AppWindow, tab_id: &str) -> String {
    let terminals = window.get_terminals();
    let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
        return String::new();
    };
    (0..model.row_count())
        .filter_map(|i| model.row_data(i))
        .find(|row| row.id.as_str() == tab_id)
        .map(|row| row.sftp_local_path.to_string())
        .unwrap_or_default()
}

/// Checked local rows, in list order.
pub(super) fn selected_paths(model: &VecModel<TerminalState>, tab_id: &str) -> Vec<String> {
    for i in 0..model.row_count() {
        let Some(row) = model.row_data(i) else { continue };
        if row.id.as_str() != tab_id {
            continue;
        }
        let Some(entries) = row.sftp_local_entries.as_any().downcast_ref::<VecModel<SftpEntry>>()
        else {
            return Vec::new();
        };
        return (0..entries.row_count())
            .filter_map(|index| entries.row_data(index))
            .filter(|entry| entry.selected)
            .map(|entry| entry.full_path.to_string())
            .collect();
    }
    Vec::new()
}

/// Toggle one local row's checkbox and recount the selection.
pub(super) fn toggle_selected(model: &VecModel<TerminalState>, tab_id: &str, index: i32) {
    let Ok(index) = usize::try_from(index) else {
        return;
    };
    for i in 0..model.row_count() {
        let Some(row) = model.row_data(i) else { continue };
        if row.id.as_str() != tab_id {
            continue;
        }
        let Some(entries) = row.sftp_local_entries.as_any().downcast_ref::<VecModel<SftpEntry>>()
        else {
            return;
        };
        if let Some(mut entry) = entries.row_data(index) {
            entry.selected = !entry.selected;
            entries.set_row_data(index, entry);
        }
        let mut count = 0;
        for index in 0..entries.row_count() {
            if entries.row_data(index).map(|e| e.selected).unwrap_or(false) {
                count += 1;
            }
        }
        let mut row = row.clone();
        row.sftp_local_selected_count = count;
        model.set_row_data(i, row);
        return;
    }
}

/// Shift+click range on the local pane: the checked set becomes `from..=to`.
pub(super) fn select_range(model: &VecModel<TerminalState>, tab_id: &str, from: i32, to: i32) -> i32 {
    for i in 0..model.row_count() {
        let Some(row) = model.row_data(i) else { continue };
        if row.id.as_str() != tab_id {
            continue;
        }
        let Some(entries) = row.sftp_local_entries.as_any().downcast_ref::<VecModel<SftpEntry>>()
        else {
            return 0;
        };
        let count = apply_range_selection(entries, from, to) as i32;
        let mut row = row.clone();
        row.sftp_local_selected_count = count;
        model.set_row_data(i, row);
        return count;
    }
    0
}

// ---------------------------------------------------------------------------
// Places dropdown (#532): user folders + drives
// ---------------------------------------------------------------------------

/// One row of the local pane's places dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LocalPlace {
    pub label: String,
    pub path: String,
    pub icon: &'static str,
    /// The pane is showing this folder.
    pub current: bool,
}

fn home_place(current: &str) -> LocalPlace {
    let path = home_display();
    LocalPlace {
        label: t("主目录", "Home").to_string(),
        current: same_dir(&path, current),
        path,
        icon: "\u{E88A}",
    }
}

/// Home, desktop, downloads and documents — whichever of them exist.
pub(super) fn quick_places(current: &str) -> Vec<LocalPlace> {
    let Some(dirs) = directories::UserDirs::new() else {
        return vec![home_place(current)];
    };
    let mut places = vec![home_place(current)];
    for (zh, en, dir, icon) in [
        ("桌面", "Desktop", dirs.desktop_dir(), "\u{E30C}"),
        ("下载", "Downloads", dirs.download_dir(), "\u{E2C4}"),
        ("文档", "Documents", dirs.document_dir(), "\u{E24D}"),
    ] {
        let Some(dir) = dir else { continue };
        let path = display_path(dir);
        places.push(LocalPlace {
            label: t(zh, en).to_string(),
            current: same_dir(&path, current),
            path,
            icon,
        });
    }
    places
}

/// Drives (Windows) / mount points, labelled with the volume name when known.
///
/// `sysinfo` asks the OS for every drive's volume information, so this runs
/// off the UI thread (see [`refresh_places`]): a mapped drive that has gone
/// away can otherwise wait for a network timeout.
pub(super) fn drive_places(current: &str) -> Vec<LocalPlace> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut seen = HashSet::new();
    let mut places = Vec::new();
    for disk in disks.list() {
        let path = display_path(disk.mount_point());
        // Windows also reports folder mount points inside a drive; the menu
        // only wants whole drives (`C:\`).
        #[cfg(windows)]
        if path.len() != 3 || !path.ends_with('\\') {
            continue;
        }
        if !seen.insert(path.clone()) {
            continue;
        }
        let label = disk.name().to_string_lossy().trim().to_string();
        let letter = path.trim_end_matches(['\\', '/']);
        places.push(LocalPlace {
            label: if label.is_empty() {
                letter.to_string()
            } else {
                format!("{letter} — {label}")
            },
            current: same_dir(&path, current),
            path,
            icon: "\u{E30C}",
        });
    }
    places.sort_by(|a, b| a.path.cmp(&b.path));
    places
}

fn into_slint_place(place: LocalPlace) -> SftpLocalPlace {
    SftpLocalPlace {
        label: place.label.into(),
        path: place.path.into(),
        icon: place.icon.into(),
        current: place.current,
    }
}

/// Slint `[string]` model → `Vec<String>` (the machine-wide places live in
/// AppWindow properties; the config only sees them when the window closes).
pub(super) fn model_strings(model: &ModelRc<SharedString>) -> Vec<String> {
    model.iter().map(|item| item.to_string()).collect()
}

fn places_from(window: &AppWindow) -> crate::config::LocalPlaces {
    crate::config::LocalPlaces {
        bookmarks: model_strings(&window.get_local_bookmarks()),
        recent: model_strings(&window.get_local_recent()),
    }
}

fn bookmarked_in(bookmarks: &[String], path: &str) -> bool {
    crate::config::LocalPlaces {
        bookmarks: bookmarks.to_vec(),
        recent: Vec::new(),
    }
    .is_bookmarked(path)
}

/// Mirror the machine-wide bookmarks / recent folders into every tab (the menu
/// and the star read the tab's copy) and re-check the star against the folder
/// each tab is showing.
pub(super) fn publish_local_places(window: &AppWindow) {
    let places = places_from(window);
    let bookmarks = super::sftp_places::string_model(&places.bookmarks);
    let recent = super::sftp_places::string_model(&places.recent);
    let terminals = window.get_terminals();
    let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
        return;
    };
    for index in 0..model.row_count() {
        let Some(row) = model.row_data(index) else {
            continue;
        };
        let path = row.sftp_local_path.to_string();
        let bookmarked = bookmarked_in(&places.bookmarks, &path);
        update_terminal_row(model, row.id.as_str(), |row| {
            row.sftp_local_bookmarks = bookmarks.clone();
            row.sftp_local_recent = recent.clone();
            row.sftp_local_bookmarked = bookmarked;
        });
    }
}

fn write_places(window: &AppWindow, places: &crate::config::LocalPlaces) {
    window.set_local_bookmarks(super::sftp_places::string_model(&places.bookmarks));
    window.set_local_recent(super::sftp_places::string_model(&places.recent));
    publish_local_places(window);
}

/// Star clicked: bookmark / un-bookmark the folder the tab is showing.
pub(super) fn toggle_bookmark(window: &AppWindow, tab_id: &str) {
    let path = current_path(window, tab_id);
    if path.is_empty() {
        return;
    }
    let mut places = places_from(window);
    places.toggle_bookmark(&path);
    write_places(window, &places);
}

/// Remove a bookmark from the places menu.
pub(super) fn remove_bookmark(window: &AppWindow, path: &str) {
    let mut places = places_from(window);
    places.remove_bookmark(path);
    write_places(window, &places);
}

/// "Clear recent" in the places menu.
pub(super) fn clear_recent(window: &AppWindow) {
    let mut places = places_from(window);
    places.clear_recent();
    write_places(window, &places);
}

/// Record a folder at the top of the recent list (called when a listing lands).
fn record_recent(window: &AppWindow, path: &str) {
    let mut places = places_from(window);
    if places.record_visit(path) {
        write_places(window, &places);
    }
}

/// Rebuild the places dropdown for a tab and publish it.
///
/// Runs off the UI thread because enumerating drives asks the OS for volume
/// information and may wait on a dead network drive.
pub(super) fn refresh_places(window: &AppWindow, tab_id: &str) {
    let current = current_path(window, tab_id);
    let weak = window.as_weak();
    let tab = tab_id.to_string();
    std::thread::spawn(move || {
        let quick: Vec<SftpLocalPlace> = quick_places(&current)
            .into_iter()
            .map(into_slint_place)
            .collect();
        let drives: Vec<SftpLocalPlace> = drive_places(&current)
            .into_iter()
            .map(into_slint_place)
            .collect();
        let _ = weak.upgrade_in_event_loop(move |window| {
            let terminals = window.get_terminals();
            let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            let quick = ModelRc::from(Rc::new(VecModel::from(quick)));
            let drives = ModelRc::from(Rc::new(VecModel::from(drives)));
            update_terminal_row(model, &tab, |row| {
                row.sftp_local_quick_places = quick.clone();
                row.sftp_local_drive_places = drives.clone();
            });
        });
    });
}

/// Copy a path dropped from the OS file manager into `dir` (the split view's
/// local half). A name that is already taken is kept side by side, like a file
/// manager's copy. The work runs off the UI thread and reports the outcome in
/// the pane's status line.
pub(super) fn copy_into(window: &AppWindow, tab_id: &str, dir: PathBuf, source: PathBuf) {
    let Some(name) = source.file_name().map(|name| name.to_string_lossy().to_string()) else {
        return;
    };
    let shown = display_path(&dir);
    let weak = window.as_weak();
    let tab = tab_id.to_string();
    std::thread::spawn(move || {
        let target = crate::sftp::available_download_path(&dir.join(&name));
        let result = copy_recursive(&source, &target).map(|()| target);
        let _ = weak.upgrade_in_event_loop(move |window| {
            let terminals = window.get_terminals();
            let Some(model) = terminals.as_any().downcast_ref::<VecModel<TerminalState>>() else {
                return;
            };
            match result {
                Ok(target) => {
                    let copied = target
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or(name);
                    let note = format!("{}: {copied}", t("已复制到本机", "Copied locally"));
                    if same_dir(&current_path(&window, &tab), &shown) {
                        load_dir_with_status(&window, &tab, dir.clone(), Some(note));
                    } else {
                        update_terminal_row(model, &tab, |row| {
                            row.sftp_local_status = note.into();
                        });
                    }
                }
                Err(message) => {
                    update_terminal_row(model, &tab, |row| {
                        row.sftp_local_status =
                            format!("{}: {message}", t("复制失败", "Copy failed")).into();
                    });
                }
            }
        });
    });
}

/// How deep a folder copy may descend. A symbolic-link or junction loop that
/// `canonicalize` cannot collapse would otherwise recurse until the stack (or
/// the disk) gives out.
const MAX_COPY_DEPTH: u32 = 32;

/// Recursively copy a file or a whole folder, like a file manager's copy.
///
/// Symlinks are followed (a linked folder is copied by content), but a physical
/// folder that was already copied is skipped, so `C:\a\loop -> C:\a` cannot
/// recurse forever. Copying a folder into its own subtree is refused up front:
/// the new folder would appear inside the tree being walked and grow without
/// end.
fn copy_recursive(source: &Path, target: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(source).map_err(|e| format!("{}: {e}", display_path(source)))?;
    if meta.is_dir() && target_lies_inside(source, target) {
        return Err(t(
            "不能把文件夹复制到它自己的子目录里",
            "Cannot copy a folder into its own subfolder",
        )
        .to_string());
    }
    let mut visited = HashSet::new();
    copy_tree(source, target, 0, &mut visited)
}

/// True when the folder `target` would end up inside `source` (lexically or
/// through links). `target` need not exist yet: its parent is resolved.
fn target_lies_inside(source: &Path, target: &Path) -> bool {
    let Ok(source_canon) = std::fs::canonicalize(source) else {
        // Unresolvable source (already an error elsewhere): fall back to the
        // lexical check so an obvious `a` → `a/b` still gets caught.
        return target.starts_with(source);
    };
    let probe = target.parent().unwrap_or(target);
    let Ok(probe) = std::fs::canonicalize(probe) else {
        return target.starts_with(&source_canon);
    };
    probe.starts_with(&source_canon)
}

fn copy_tree(
    source: &Path,
    target: &Path,
    depth: u32,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    if depth > MAX_COPY_DEPTH {
        return Err(format!(
            "{} ({})",
            t(
                "目录层级过深，可能遇到符号链接环",
                "Folder nesting too deep — possible symlink loop"
            ),
            display_path(source)
        ));
    }
    let meta = std::fs::metadata(source).map_err(|e| format!("{}: {e}", display_path(source)))?;
    if meta.is_dir() {
        // A link/junction that leads back into a folder we already copied is
        // skipped instead of duplicating its contents (or looping).
        if let Ok(identity) = std::fs::canonicalize(source) {
            if !visited.insert(identity) {
                return Ok(());
            }
        }
        std::fs::create_dir_all(target).map_err(|e| format!("{}: {e}", display_path(target)))?;
        let entries =
            std::fs::read_dir(source).map_err(|e| format!("{}: {e}", display_path(source)))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", display_path(source)))?;
            copy_tree(
                &entry.path(),
                &target.join(entry.file_name()),
                depth + 1,
                visited,
            )?;
        }
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("{}: {e}", display_path(parent)))?;
    }
    std::fs::copy(source, target)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", display_path(source)))
}

#[cfg(test)]
#[path = "../../tests/app/sftp_local/mod.rs"]
mod tests;
