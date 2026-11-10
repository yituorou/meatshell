//! Visual QA for the split (dual-pane) SFTP view: renders the real window with
//! the local browser on the left and the remote listing on the right, and saves
//! the frame to `target/sftp-split-qa.png`.
//!
//! A rendering smoke test catches the mistakes a type check cannot: a pane
//! collapsing to zero width, the local pane painting over the remote one, or
//! the transfer buttons disappearing in a narrow dock.

use super::*;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Rgb8Pixel};

struct Backend(std::rc::Rc<MinimalSoftwareWindow>);

impl slint::platform::Platform for Backend {
    fn create_window_adapter(
        &self,
    ) -> Result<std::rc::Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn row(name: &str, is_dir: bool, size: &str, modified: &str) -> SftpEntry {
    SftpEntry {
        name: name.into(),
        full_path: format!("/{name}").into(),
        is_dir,
        size: size.into(),
        size_bytes: 0.0,
        modified: modified.into(),
        modified_ts: 0.0,
        mode: 0,
        file_type: if is_dir { "文件夹".into() } else { "文件".into() },
        permissions: "".into(),
        permissions_mode: 0,
        owner: "".into(),
        group: "".into(),
        selected: false,
    }
}

fn terminal(
    dual: bool,
    local_entries: Vec<SftpEntry>,
    remote_entries: Vec<SftpEntry>,
    panel_width: f32,
    local_pane_width: f32,
) -> TerminalState {
    TerminalState {
        id: "t1".into(),
        status: "connected".into(),
        spans: ModelRc::from(Rc::new(VecModel::default())),
        cursor_row: 0,
        cursor_col: 0,
        rows_used: 0,
        scroll_max: 0,
        scroll_offset: 0,
        is_alt_screen: false,
        mouse_tracked: false,
        find_matches: ModelRc::from(Rc::new(VecModel::default())),
        find_current: Default::default(),
        selection: ModelRc::from(Rc::new(VecModel::default())),
        sftp_path: "/home/me".into(),
        sftp_entries: ModelRc::from(Rc::new(VecModel::from(remote_entries))),
        sftp_status: "/home/me".into(),
        sftp_loading: false,
        sftp_tree_nodes: ModelRc::from(Rc::new(VecModel::default())),
        sftp_selected_count: 0,
        sftp_sort_key: "".into(),
        sftp_sort_dir: 0,
        sftp_available: true,
        sftp_bookmarks: ModelRc::from(Rc::new(VecModel::default())),
        sftp_recent: ModelRc::from(Rc::new(VecModel::default())),
        sftp_bookmarked: false,
        sftp_dual: dual,
        sftp_local_path: "C:\\Users\\me\\work".into(),
        sftp_local_entries: ModelRc::from(Rc::new(VecModel::from(local_entries))),
        sftp_local_status: "C:\\Users\\me\\work".into(),
        sftp_local_loading: false,
        sftp_local_selected_count: 0,
        sftp_local_pane_width: local_pane_width,
        sftp_local_quick_places: ModelRc::from(Rc::new(VecModel::default())),
        sftp_local_drive_places: ModelRc::from(Rc::new(VecModel::default())),
        sftp_local_bookmarks: ModelRc::from(Rc::new(VecModel::default())),
        sftp_local_recent: ModelRc::from(Rc::new(VecModel::default())),
        sftp_local_bookmarked: false,
        sftp_local_show_hidden: false,
        sftp_local_sort_key: "".into(),
        sftp_local_sort_dir: 0,
        sftp_local_filter: "".into(),
        sftp_local_all_entries: ModelRc::from(Rc::new(VecModel::default())),
        font_size: 0,
        tunnels: ModelRc::from(Rc::new(VecModel::default())),
        sftp_collapsed: false,
        sftp_panel_height: 300.0,
        sftp_panel_width: panel_width,
        sftp_saved_height: 300.0,
    }
}

fn tab(id: &str) -> TabInfo {
    TabInfo {
        id: id.into(),
        title: "host".into(),
        title_len: 4,
        kind: "terminal".into(),
        connected: true,
        local: false,
    }
}

/// Build the real window showing the split view with a few local and remote
/// rows, sized as requested. Returns the component (so callbacks can be wired)
/// and the software-renderer window (so pointer events can be dispatched).
fn build_ui(
    dual: bool,
    panel_width: f32,
    local_pane_width: f32,
    window_width: u32,
    window_height: u32,
    dock: &str,
) -> (AppWindow, Rc<MinimalSoftwareWindow>) {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Backend(window.clone()))).unwrap();
    let ui = AppWindow::new().unwrap();
    ui.on_sftp_entry_matches(|name, file_type, query| {
        crate::app::sftp_ui::sftp_entry_matches_query(name.as_str(), file_type.as_str(), query.as_str())
    });

    ui.set_terminals(ModelRc::from(Rc::new(VecModel::from(vec![terminal(
        dual,
        vec![
            row("src", true, "", "2026-10-27 10:00"),
            row("notes.txt", false, "1.2 KB", "2026-10-26 22:15"),
            row("build.log", false, "12.4 KB", "2026-10-27 09:58"),
        ],
        vec![
            row("www", true, "", "2026-10-20 08:00"),
            row("nginx.conf", false, "2.8 KB", "2026-10-19 17:42"),
        ],
        panel_width,
        local_pane_width,
    )]))));
    ui.set_tabs(ModelRc::from(Rc::new(VecModel::from(vec![tab("t1")]))));
    ui.set_panes(ModelRc::from(Rc::new(VecModel::from(vec![PaneInfo {
        id: 0,
        x: 0.0,
        y: 0.0,
        w: window_width as f32,
        h: window_height as f32,
        active_id: "t1".into(),
        focused: true,
        reserve_right: 0.0,
        tabs: ModelRc::from(Rc::new(VecModel::from(vec![tab("t1")]))),
    }]))));
    ui.set_active_tab_id("t1".into());
    // The Rust dock engine normally owns this central rect; give it the whole
    // window so the panel is not clipped by a stale default width.
    ui.set_dock_central_x(0.0);
    ui.set_dock_central_y(0.0);
    ui.set_dock_central_w(window_width as f32);
    ui.set_dock_central_h(window_height as f32);
    ui.set_sftp_dock(dock.into());
    ui.set_sftp_panel_height(300.0);
    ui.set_sftp_panel_width(panel_width);
    ui.show().unwrap();
    window.set_size(slint::PhysicalSize::new(window_width, window_height));
    (ui, window)
}

fn render_frame(window: &MinimalSoftwareWindow, width: u32, height: u32) -> Vec<Rgb8Pixel> {
    let mut pixels = vec![Rgb8Pixel::default(); (width * height) as usize];
    window.request_redraw();
    assert!(window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width as usize);
    }));
    pixels
}

fn render_split_view(
    dual: bool,
    panel_width: f32,
    local_pane_width: f32,
    window_width: u32,
    window_height: u32,
    dock: &str,
    shot: &str,
) -> Vec<Rgb8Pixel> {
    let (_ui, window) = build_ui(
        dual,
        panel_width,
        local_pane_width,
        window_width,
        window_height,
        dock,
    );
    let width = window_width;
    let height = window_height;
    let pixels = render_frame(&window, width, height);
    let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(shot);
    image::save_buffer(path, &bytes, width, height, image::ColorType::Rgb8).unwrap();
    pixels
}

#[test]
fn split_view_renders_both_panes() {
    std::thread::spawn(|| {
        let pixels = render_split_view(
            true,
            300.0,
            300.0,
            1100,
            800,
            "bottom",
            "target/sftp-split-qa.png",
        );
        // Both halves must paint content: the local pane's status bar and rows
        // on the left, the remote column header and rows on the right.
        let width = 1100usize;
        let left = pixels
            .iter()
            .enumerate()
            .filter(|(i, p)| i % width < 290 && p.r > 0)
            .count();
        let right = pixels
            .iter()
            .enumerate()
            .filter(|(i, p)| i % width > 320 && p.r > 0)
            .count();
        assert!(left > 50, "local pane painted almost nothing ({left})");
        assert!(right > 50, "remote pane painted almost nothing ({right})");
    })
    .join()
    .unwrap();
}

#[test]
fn split_view_defaults_to_an_even_split() {
    // No dragged width (0) → the divider sits in the middle of the panel, and
    // the geometry Rust uses for OS-drop hit testing agrees with it.
    std::thread::spawn(|| {
        let pixels = render_split_view(
            true,
            300.0,
            0.0,
            1100,
            800,
            "bottom",
            "target/sftp-split-half-qa.png",
        );
        let width = 1100usize;
        let is_border = |pixel: Rgb8Pixel| {
            pixel.r.abs_diff(58) < 8 && pixel.g.abs_diff(61) < 8 && pixel.b.abs_diff(70) < 8
        };
        let mut best = (0usize, 0usize);
        for x in 120..980 {
            let count = (520..780).filter(|&y| is_border(pixels[y * width + x])).count();
            if count > best.1 {
                best = (x, count);
            }
        }
        assert!(best.1 > 120, "split divider not found (best {best:?})");
        assert!(
            (best.0 as i32 - 550).abs() <= 8,
            "divider at x={} should be in the middle (550)",
            best.0
        );
    })
    .join()
    .unwrap();
}

#[test]
fn split_view_honours_the_remembered_ratio() {
    // A divider share saved from a previous session (#532) must place the
    // divider and the OS-drop hit test at the same x.
    std::thread::spawn(|| {
        let (ui, window) = build_ui(true, 300.0, 0.0, 1100, 800, "bottom");
        ui.set_sftp_local_split(0.3);
        let pixels = render_frame(&window, 1100, 800);
        let width = 1100usize;
        let is_border = |pixel: Rgb8Pixel| {
            pixel.r.abs_diff(58) < 8 && pixel.g.abs_diff(61) < 8 && pixel.b.abs_diff(70) < 8
        };
        let mut best = (0usize, 0usize);
        for x in 120..980 {
            let count = (520..780).filter(|&y| is_border(pixels[y * width + x])).count();
            if count > best.1 {
                best = (x, count);
            }
        }
        assert!(best.1 > 120, "split divider not found (best {best:?})");
        assert!(
            (best.0 as i32 - 330).abs() <= 8,
            "divider at x={} should honour a 30% share (330)",
            best.0
        );
        let (_, rect) = active_sftp_local_pane_rect(&ui).expect("local half");
        assert!(
            (rect.w - 330.0).abs() <= 8.0,
            "hit-test width {} should match the 30% share",
            rect.w
        );
    })
    .join()
    .unwrap();
}

#[test]
fn local_places_are_mirrored_into_tabs() {
    // The bookmarks / recent lists are machine-wide AppWindow properties; every
    // tab keeps a copy for its menu, and the star follows the folder it shows.
    std::thread::spawn(|| {
        let (ui, _window) = build_ui(true, 300.0, 0.0, 1100, 800, "bottom");
        let folder = SharedString::from("C:\\Users\\me\\work");
        ui.set_local_bookmarks(ModelRc::from(Rc::new(VecModel::from(vec![folder.clone()]))));
        ui.set_local_recent(ModelRc::from(Rc::new(VecModel::from(vec![folder]))));
        publish_local_places(&ui);

        let terminals = ui.get_terminals();
        let model = terminals
            .as_any()
            .downcast_ref::<VecModel<TerminalState>>()
            .expect("terminals model");
        let row = model.row_data(0).expect("tab");
        assert_eq!(row.sftp_local_bookmarks.row_count(), 1);
        assert_eq!(row.sftp_local_recent.row_count(), 1);
        // The harness tab is showing that same folder, so the star lights up.
        #[cfg(windows)]
        assert!(row.sftp_local_bookmarked, "the current folder is bookmarked");
    })
    .join()
    .unwrap();
}

#[test]
fn split_view_survives_a_side_dock() {
    // A side-docked panel hides the tree and uses the compact toolbar; the two
    // panes still have to appear side by side.
    std::thread::spawn(|| {
        let pixels = render_split_view(
            true,
            520.0,
            260.0,
            1400,
            800,
            "right",
            "target/sftp-split-side-qa.png",
        );
        let width = 1400usize;
        // Panel occupies the right 520px: local pane on its left, remote right.
        let local = pixels
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                let x = i % width;
                (940..1000).contains(&x) && p.r > 0
            })
            .count();
        let remote = pixels
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                let x = i % width;
                (1020..1380).contains(&x) && p.r > 0
            })
            .count();
        assert!(local > 50, "local pane missing in a side dock ({local})");
        assert!(remote > 50, "remote pane missing in a side dock ({remote})");
    })
    .join()
    .unwrap();
}

#[test]
fn os_drop_hit_test_matches_the_visible_split() {
    // The OS drop handler routes a file to the local half by geometry; it must
    // agree with the pane the user actually sees (even split by default).
    std::thread::spawn(|| {
        let (ui, _window) = build_ui(true, 300.0, 0.0, 1100, 800, "bottom");
        let (tab, rect) =
            active_sftp_local_pane_rect(&ui).expect("split view should expose the local half");
        assert_eq!(tab, "t1");
        assert!(
            (rect.w - 550.0).abs() <= 8.0,
            "even split: local half is {}px, expected ~550",
            rect.w
        );
        let inside = rect.y + 40.0;
        assert!(
            sftp_local_pane_hit(&ui, rect.x + 40.0, inside).is_some(),
            "left half must accept a local copy drop"
        );
        assert!(
            sftp_local_pane_hit(&ui, rect.x + rect.w + 80.0, inside).is_none(),
            "right half must fall through to the upload path"
        );
        assert!(
            sftp_local_pane_hit(&ui, rect.x + 40.0, rect.y - 40.0).is_none(),
            "the toolbar is not a drop target"
        );
    })
    .join()
    .unwrap();
}

#[test]
fn classic_view_still_renders_without_the_local_pane() {
    std::thread::spawn(|| {
        let (ui, window) = build_ui(false, 700.0, 0.0, 700, 700, "bottom");
        let pixels = render_frame(&window, 700, 700);
        assert!(pixels.iter().any(|p| p.r > 0));
        // Without the split view there is no local half to drop onto.
        assert!(active_sftp_local_pane_rect(&ui).is_none());
    })
    .join()
    .unwrap();
}

// ---------------------------------------------------------------------------
// Split-view drag & drop
// ---------------------------------------------------------------------------

/// `Theme.accent` (#4a90e2): folder icons and active toolbar buttons.
fn is_accent(pixel: Rgb8Pixel) -> bool {
    pixel.r.abs_diff(74) < 20 && pixel.g.abs_diff(144) < 20 && pixel.b.abs_diff(226) < 20
}

/// Bands of consecutive rows that contain an accent pixel in `x0..x1`.
/// Folder icons are accent-coloured, so this finds the rows in a pane without
/// hard-coding the layout heights.
fn accent_bands(
    pixels: &[Rgb8Pixel],
    width: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
) -> Vec<(usize, usize)> {
    let mut bands: Vec<(usize, usize)> = Vec::new();
    for y in y0..y1 {
        if (x0..x1).any(|x| is_accent(pixels[y * width + x])) {
            match bands.last_mut() {
                Some((_, end)) if y <= *end + 2 => *end = y,
                _ => bands.push((y, y)),
            }
        }
    }
    bands
}

fn press(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    window.dispatch_event(WindowEvent::PointerPressed {
        position: LogicalPosition::new(x, y),
        button: PointerEventButton::Left,
    });
}

fn move_to(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(x, y),
    });
}

fn release(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    window.dispatch_event(WindowEvent::PointerReleased {
        position: LogicalPosition::new(x, y),
        button: PointerEventButton::Left,
    });
}

#[test]
fn dragging_a_row_onto_the_other_pane_transfers_it() {
    std::thread::spawn(|| {
        let (ui, window) = build_ui(true, 300.0, 300.0, 1100, 800, "bottom");
        let uploads = Rc::new(RefCell::new(Vec::<String>::new()));
        let downloads = Rc::new(RefCell::new(Vec::<String>::new()));
        {
            let uploads = uploads.clone();
            ui.on_sftp_local_upload(move |tab: SharedString, path: SharedString| {
                uploads.borrow_mut().push(format!("{tab}|{path}"));
            });
        }
        {
            let downloads = downloads.clone();
            ui.on_sftp_download_to_local(move |tab: SharedString, path: SharedString| {
                downloads.borrow_mut().push(format!("{tab}|{path}"));
            });
        }

        let frame = render_frame(&window, 1100, 800);
        // Local rows: folder icon at pane x 30..50 (the toolbar's "Files" tab is
        // accent too, but sits above the rows — hence the last band).
        let local_bands = accent_bands(&frame, 1100, 30, 50, 500, 800);
        // Remote rows: local half 300px + 5px divider + checkbox/icon chrome.
        let remote_bands = accent_bands(&frame, 1100, 334, 356, 400, 800);
        let local_y = local_bands
            .last()
            .map(|(start, _)| *start as f32 + 6.0)
            .expect("no local folder row");
        let remote_y = remote_bands
            .last()
            .map(|(start, _)| *start as f32 + 6.0)
            .expect("no remote folder row");

        // Local → remote: drop on the right half uploads the dragged folder.
        press(&window, 40.0, local_y);
        move_to(&window, 130.0, local_y);
        move_to(&window, 700.0, local_y);
        assert_eq!(uploads.borrow().len(), 0, "transfer must wait for the drop");
        // Visual: the drag ghost and the highlighted drop pane while hovering.
        let hovering = render_frame(&window, 1100, 800);
        let bytes: Vec<u8> = hovering.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        image::save_buffer(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sftp-drag-qa.png"),
            &bytes,
            1100,
            800,
            image::ColorType::Rgb8,
        )
        .unwrap();
        release(&window, 700.0, local_y);
        assert_eq!(
            uploads.borrow().as_slice(),
            ["t1|/src"],
            "dragging a local row onto the remote pane must upload it"
        );
        assert!(downloads.borrow().is_empty());

        // Remote → local: drop on the left half downloads.
        press(&window, 340.0, remote_y);
        move_to(&window, 260.0, remote_y);
        move_to(&window, 120.0, remote_y);
        release(&window, 120.0, remote_y);
        assert_eq!(
            downloads.borrow().as_slice(),
            ["t1|/www"],
            "dragging a remote row onto the local pane must download it"
        );
        assert_eq!(uploads.borrow().len(), 1);

        // Dropping back on the same pane transfers nothing.
        press(&window, 40.0, local_y);
        move_to(&window, 200.0, local_y);
        release(&window, 200.0, local_y);
        assert_eq!(uploads.borrow().len(), 1, "same-pane drop must be a no-op");
        assert_eq!(downloads.borrow().len(), 1);
    })
    .join()
    .unwrap();
}

#[test]
fn dragging_a_checked_row_drags_the_whole_checked_set() {
    std::thread::spawn(|| {
        let (ui, window) = build_ui(true, 300.0, 300.0, 1100, 800, "bottom");
        let uploads = Rc::new(RefCell::new(Vec::<String>::new()));
        let bulk = Rc::new(RefCell::new(Vec::<usize>::new()));
        {
            let uploads = uploads.clone();
            ui.on_sftp_local_upload(move |_tab: SharedString, path: SharedString| {
                uploads.borrow_mut().push(path.to_string());
            });
        }
        {
            let bulk = bulk.clone();
            ui.on_sftp_local_upload_selected(move |_tab: SharedString| {
                bulk.borrow_mut().push(1);
            });
        }
        // Two checked local rows: the row we drag is one of several checked
        // items, so the whole set must travel.
        {
            let terminals = ui.get_terminals();
            let model = terminals
                .as_any()
                .downcast_ref::<VecModel<TerminalState>>()
                .unwrap();
            let mut row = model.row_data(0).unwrap();
            let entries = row
                .sftp_local_entries
                .as_any()
                .downcast_ref::<VecModel<SftpEntry>>()
                .unwrap();
            for i in 0..entries.row_count() {
                let mut entry = entries.row_data(i).unwrap();
                entry.selected = true;
                entries.set_row_data(i, entry);
            }
            row.sftp_local_selected_count = entries.row_count() as i32;
            model.set_row_data(0, row);
        }
        let frame = render_frame(&window, 1100, 800);
        let bands = accent_bands(&frame, 1100, 30, 50, 500, 800);
        let row_y = bands
            .last()
            .map(|(start, _)| *start as f32 + 6.0)
            .expect("no local folder row");

        press(&window, 40.0, row_y);
        move_to(&window, 130.0, row_y);
        move_to(&window, 700.0, row_y);
        release(&window, 700.0, row_y);
        assert_eq!(
            bulk.borrow().len(),
            1,
            "a checked row drags the whole checked set"
        );
        assert!(uploads.borrow().is_empty(), "single-item upload must not run");
    })
    .join()
    .unwrap();
}

// ---------------------------------------------------------------------------
// Wheel scrolling in the remote file list (#540)
// ---------------------------------------------------------------------------

#[test]
fn wheel_over_file_rows_scrolls_the_list() {
    std::thread::spawn(|| {
        let (ui, window) = build_ui(false, 700.0, 0.0, 1400, 900, "right");
        // Enough folders to overflow the list (folder icons are accent-coloured).
        {
            let terminals = ui.get_terminals();
            let model = terminals.as_any().downcast_ref::<VecModel<TerminalState>>().unwrap();
            let mut row = model.row_data(0).unwrap();
            let entries: Vec<SftpEntry> =
                (0..80).map(|i| self::row(&format!("dir{i:02}"), true, "", "2026-11-01 10:00")).collect();
            row.sftp_entries = ModelRc::from(Rc::new(VecModel::from(entries)));
            model.set_row_data(0, row);
        }
        // The file-name column's pixels: rows move in whole-row steps, so a
        // scroll shows up as different names, not as a shifted first row.
        let name_column = |window: &MinimalSoftwareWindow| -> Vec<(u8, u8, u8)> {
            let frame = render_frame(window, 1400, 900);
            (160..880)
                .flat_map(|y| (745..800).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let p = frame[y * 1400 + x];
                    (p.r, p.g, p.b)
                })
                .collect()
        };
        let wheel = |x: f32, y: f32, dy: f32| {
            window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(x, y) });
            window.dispatch_event(WindowEvent::PointerScrolled {
                position: LogicalPosition::new(x, y),
                delta_x: 0.0,
                delta_y: dy,
            });
        };
        let top = name_column(&window);
        // Over a row's file name, the list itself must scroll.
        wheel(770.0, 400.0, -120.0);
        assert!(name_column(&window) != top, "wheel over a file name did not scroll the list");
    })
    .join()
    .unwrap();
}

// ---------------------------------------------------------------------------
// Tab menu: start / stop a session log (#535)
// ---------------------------------------------------------------------------

#[test]
fn tab_menu_session_log_item_toggles_that_tab() {
    std::thread::spawn(|| {
        let (ui, window) = build_ui(false, 700.0, 0.0, 1400, 900, "right");
        let toggled = Rc::new(RefCell::new(Vec::<String>::new()));
        ui.on_tab_log_active(|_tab: SharedString| false);
        {
            let toggled = toggled.clone();
            ui.on_tab_log_toggle(move |tab: SharedString| toggled.borrow_mut().push(tab.to_string()));
        }
        render_frame(&window, 1400, 900);
        let right_click = |x: f32, y: f32| {
            window.dispatch_event(WindowEvent::PointerPressed {
                position: LogicalPosition::new(x, y),
                button: PointerEventButton::Right,
            });
            window.dispatch_event(WindowEvent::PointerReleased {
                position: LogicalPosition::new(x, y),
                button: PointerEventButton::Right,
            });
        };
        // The "host" tab sits at the left of the tab strip.
        right_click(40.0, 55.0);
        let menu = render_frame(&window, 1400, 900);
        let bytes: Vec<u8> = menu.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        image::save_buffer(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/tab-log-menu-qa.png"),
            &bytes,
            1400,
            900,
            image::ColorType::Rgb8,
        )
        .unwrap();
        // Fourth item: 4px padding + 3 × (28px item + 1px spacing) + half an item.
        let item_y = 55.0 + 4.0 + 3.0 * 29.0 + 14.0;
        press(&window, 100.0, item_y);
        release(&window, 100.0, item_y);
        assert_eq!(toggled.borrow().as_slice(), ["t1"]);
    })
    .join()
    .unwrap();
}

// ---------------------------------------------------------------------------
// Deleting checked remote entries (#531)
// ---------------------------------------------------------------------------

/// Check (or uncheck) every remote entry and update the selected count.
fn check_all_remote(ui: &AppWindow, checked: bool) {
    let terminals = ui.get_terminals();
    let model = terminals
        .as_any()
        .downcast_ref::<VecModel<TerminalState>>()
        .unwrap();
    let mut row = model.row_data(0).unwrap();
    let entries = row
        .sftp_entries
        .as_any()
        .downcast_ref::<VecModel<SftpEntry>>()
        .unwrap();
    for i in 0..entries.row_count() {
        let mut entry = entries.row_data(i).unwrap();
        entry.selected = checked;
        entries.set_row_data(i, entry);
    }
    row.sftp_selected_count = if checked { entries.row_count() as i32 } else { 0 };
    model.set_row_data(0, row);
}

/// Classic (single-pane) view docked right at full height, so the context
/// menu opens below the rows without being pushed around by the window edge.
/// Returns the y of the last remote row (nginx.conf).
fn classic_right_dock() -> (AppWindow, Rc<MinimalSoftwareWindow>, f32) {
    let (ui, window) = build_ui(false, 700.0, 0.0, 1400, 900, "right");
    check_all_remote(&ui, true);
    let frame = render_frame(&window, 1400, 900);
    // Checked rows paint an accent checkbox at the start of the row.
    let bands = accent_bands(&frame, 1400, 700, 760, 0, 900);
    let row_y = bands
        .last()
        .map(|(start, end)| (*start + *end) as f32 / 2.0)
        .expect("no checked remote row");
    (ui, window, row_y)
}

/// Centre of the red "Delete" menu item (`#d86c6c` text) in a menu opened
/// below `below_y` (the toolbar's red delete-checked icon sits above it).
fn find_delete_item(pixels: &[Rgb8Pixel], width: usize, height: usize, below_y: f32) -> (f32, f32) {
    let is_red =
        |p: Rgb8Pixel| p.r.abs_diff(216) < 24 && p.g.abs_diff(108) < 24 && p.b.abs_diff(108) < 24;
    let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0, usize::MAX, 0);
    for y in (below_y as usize + 8)..height {
        for x in 0..width {
            if is_red(pixels[y * width + x]) {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    assert!(x0 <= x1, "Delete menu item not painted");
    ((x0 + x1) as f32 / 2.0, (y0 + y1) as f32 / 2.0)
}

#[test]
fn context_menu_delete_on_a_checked_row_deletes_the_whole_selection() {
    std::thread::spawn(|| {
        let (ui, window, row_y) = classic_right_dock();
        window.dispatch_event(WindowEvent::PointerPressed {
            position: LogicalPosition::new(900.0, row_y),
            button: PointerEventButton::Right,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position: LogicalPosition::new(900.0, row_y),
            button: PointerEventButton::Right,
        });
        let menu = render_frame(&window, 1400, 900);
        let bytes: Vec<u8> = menu.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        image::save_buffer(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sftp-delete-menu-qa.png"),
            &bytes,
            1400,
            900,
            image::ColorType::Rgb8,
        )
        .unwrap();
        let (x, y) = find_delete_item(&menu, 1400, 900, row_y);
        press(&window, x, y);
        release(&window, x, y);
        assert!(ui.get_confirm_delete_open(), "Delete must ask for confirmation");
        assert!(
            ui.get_confirm_delete_batch(),
            "right-clicking a checked row must delete every checked entry, not only that row"
        );
    })
    .join()
    .unwrap();
}

#[test]
fn delete_key_deletes_checked_entries_or_the_highlighted_row() {
    std::thread::spawn(|| {
        use slint::platform::Key;
        let (ui, window, row_y) = classic_right_dock();
        let delete_key = || {
            window.dispatch_event(WindowEvent::KeyPressed { text: Key::Delete.into() });
            window.dispatch_event(WindowEvent::KeyReleased { text: Key::Delete.into() });
        };
        // A plain click highlights the row and focuses the list.
        press(&window, 900.0, row_y);
        release(&window, 900.0, row_y);
        delete_key();
        assert!(ui.get_confirm_delete_open(), "Del must open the delete confirmation");
        assert!(ui.get_confirm_delete_batch(), "Del with checked entries deletes all of them");

        ui.set_confirm_delete_open(false);
        ui.set_confirm_delete_batch(false);
        check_all_remote(&ui, false);
        render_frame(&window, 1400, 900);
        press(&window, 900.0, row_y);
        release(&window, 900.0, row_y);
        delete_key();
        assert!(ui.get_confirm_delete_open());
        assert!(!ui.get_confirm_delete_batch());
        assert_eq!(ui.get_confirm_delete_path().as_str(), "/nginx.conf");
    })
    .join()
    .unwrap();
}

#[test]
fn round5_sftp_overhaul_visual_and_functional_verification() {
    use crate::app::sftp_ui::{friendly_file_type, sftp_entry_matches_query, sort_sftp_entries};
    use crate::ui::TransferInfo;

    // 1. Verify descriptive file type classification and real-time filtering.
    assert_eq!(friendly_file_type("src", true, "folder"), "文件夹");
    assert_eq!(friendly_file_type("release-v0.8.2.tar.gz", false, "file"), "TAR.GZ 压缩包");
    assert_eq!(friendly_file_type("deploy.sh", false, "file"), "SH 文件");
    assert_eq!(friendly_file_type("config.json", false, "file"), "JSON 文件");
    assert_eq!(friendly_file_type("README.md", false, "file"), "MD 文件");
    assert!(sftp_entry_matches_query("release-v0.8.2.tar.gz", "TAR.GZ 压缩包", "tar.gz"));
    assert!(sftp_entry_matches_query("release-v0.8.2.tar.gz", "TAR.GZ 压缩包", "压缩包"));
    assert!(!sftp_entry_matches_query("release-v0.8.2.tar.gz", "TAR.GZ 压缩包", "nginx"));

    // Round 6: Verify hidden dotfile filtering (`__hide_dot__:`) and subfolder autocomplete (`__sub__:`).
    assert!(!sftp_entry_matches_query(".agents", "文件夹", "__hide_dot__:"));
    assert!(!sftp_entry_matches_query(".docker", "文件夹", "__hide_dot__:doc"));
    assert!(sftp_entry_matches_query(".docker", "文件夹", "doc"));
    assert!(sftp_entry_matches_query("workspace", "文件夹", "__hide_dot__:work"));
    assert!(sftp_entry_matches_query("workspace", "文件夹", "__sub__:/Users/huangpj\n/Users/huangpj"));
    assert!(sftp_entry_matches_query("workspace", "文件夹", "__sub__:/Users/huangpj\n/Users/huangpj/wo"));
    assert!(!sftp_entry_matches_query("downloads", "文件夹", "__sub__:/Users/huangpj\n/Users/huangpj/wo"));
    assert!(!sftp_entry_matches_query(".agents", "文件夹", "__hide_dot__:__sub__:/Users/huangpj\n/Users/huangpj"));
    assert!(sftp_entry_matches_query(".agents", "文件夹", "__sub__:/Users/huangpj\n/Users/huangpj/.a"));

    // 2. Verify multi-column sorting (name, size, type, modified) keeps directories on top.
    let mut sample = vec![
        SftpEntry {
            name: "z-script.sh".into(),
            full_path: "/root/z-script.sh".into(),
            is_dir: false,
            size: "4.2 KB".into(),
            size_bytes: 4300.0,
            modified: "2026-11-08 14:20".into(),
            modified_ts: 100.0,
            mode: 0o100755,
            file_type: friendly_file_type("z-script.sh", false, "file").into(),
            permissions: "rwxr-xr-x".into(),
            permissions_mode: 0o755,
            owner: "root".into(),
            group: "root".into(),
            selected: false,
        },
        SftpEntry {
            name: "a-config.json".into(),
            full_path: "/root/a-config.json".into(),
            is_dir: false,
            size: "128.0 KB".into(),
            size_bytes: 131072.0,
            modified: "2026-11-09 09:15".into(),
            modified_ts: 200.0,
            mode: 0o100644,
            file_type: friendly_file_type("a-config.json", false, "file").into(),
            permissions: "rw-r--r--".into(),
            permissions_mode: 0o644,
            owner: "www".into(),
            group: "www".into(),
            selected: false,
        },
        SftpEntry {
            name: "logs".into(),
            full_path: "/root/logs".into(),
            is_dir: true,
            size: "".into(),
            size_bytes: 0.0,
            modified: "2026-11-09 10:00".into(),
            modified_ts: 300.0,
            mode: 0o040755,
            file_type: friendly_file_type("logs", true, "folder").into(),
            permissions: "rwxr-xr-x".into(),
            permissions_mode: 0o755,
            owner: "root".into(),
            group: "root".into(),
            selected: false,
        },
    ];
    sort_sftp_entries(&mut sample, "size", -1);
    assert_eq!(sample[0].name.as_str(), "logs");
    assert_eq!(sample[1].name.as_str(), "a-config.json");
    assert_eq!(sample[2].name.as_str(), "z-script.sh");

    sort_sftp_entries(&mut sample, "type", 1);
    assert_eq!(sample[0].name.as_str(), "logs");
    assert_eq!(sample[1].name.as_str(), "a-config.json"); // JSON 配置 < Shell 脚本

    // 3. Render real Slint UI frames for Light & Dark modes and save QA screenshots.
    std::thread::spawn(|| {
        let (ui, window) = build_ui(true, 420.0, 0.0, 1440, 900, "bottom");
        ui.set_sftp_panel_height(420.0);

        let mut local_entries: Vec<SftpEntry> = (0..35)
            .map(|idx| SftpEntry {
                name: format!(".hidden-cache-{idx:02}").into(),
                full_path: format!("/Users/huangpj/.hidden-cache-{idx:02}").into(),
                is_dir: true,
                size: "".into(),
                size_bytes: 0.0,
                modified: "2026-07-15 22:13".into(),
                modified_ts: 100.0,
                mode: 0,
                file_type: "文件夹".into(),
                permissions: "".into(),
                permissions_mode: 0,
                owner: "".into(),
                group: "".into(),
                selected: false,
            })
            .collect();
        local_entries.extend(vec![
            SftpEntry {
                name: "workspace".into(),
                full_path: "/Users/huangpj/workspace".into(),
                is_dir: true,
                size: "".into(),
                size_bytes: 0.0,
                modified: "2026-11-09 19:30".into(),
                modified_ts: 500.0,
                mode: 0,
                file_type: "文件夹".into(),
                permissions: "".into(),
                permissions_mode: 0,
                owner: "".into(),
                group: "".into(),
                selected: false,
            },
            SftpEntry {
                name: "downloads".into(),
                full_path: "/Users/huangpj/downloads".into(),
                is_dir: true,
                size: "".into(),
                size_bytes: 0.0,
                modified: "2026-11-09 18:10".into(),
                modified_ts: 450.0,
                mode: 0,
                file_type: "文件夹".into(),
                permissions: "".into(),
                permissions_mode: 0,
                owner: "".into(),
                group: "".into(),
                selected: false,
            },
            SftpEntry {
                name: "documents".into(),
                full_path: "/Users/huangpj/documents".into(),
                is_dir: true,
                size: "".into(),
                size_bytes: 0.0,
                modified: "2026-11-09 16:05".into(),
                modified_ts: 420.0,
                mode: 0,
                file_type: "文件夹".into(),
                permissions: "".into(),
                permissions_mode: 0,
                owner: "".into(),
                group: "".into(),
                selected: false,
            },
            SftpEntry {
                name: "release-v0.8.2.tar.gz".into(),
                full_path: "/Users/huangpj/release-v0.8.2.tar.gz".into(),
                is_dir: false,
                size: "48.6 MB".into(),
                size_bytes: 50_960_793.0,
                modified: "2026-11-09 20:15".into(),
                modified_ts: 600.0,
                mode: 0,
                file_type: "TAR.GZ 压缩包".into(),
                permissions: "".into(),
                permissions_mode: 0,
                owner: "".into(),
                group: "".into(),
                selected: true,
            },
            SftpEntry {
                name: "deploy-prod.sh".into(),
                full_path: "/Users/huangpj/deploy-prod.sh".into(),
                is_dir: false,
                size: "6.4 KB".into(),
                size_bytes: 6553.0,
                modified: "2026-11-09 17:42".into(),
                modified_ts: 400.0,
                mode: 0,
                file_type: "SH 文件".into(),
                permissions: "".into(),
                permissions_mode: 0,
                owner: "".into(),
                group: "".into(),
                selected: false,
            },
            SftpEntry {
                name: "docker-compose.yml".into(),
                full_path: "/Users/huangpj/docker-compose.yml".into(),
                is_dir: false,
                size: "3.1 KB".into(),
                size_bytes: 3174.0,
                modified: "2026-11-08 22:05".into(),
                modified_ts: 300.0,
                mode: 0,
                file_type: "YML 文件".into(),
                permissions: "".into(),
                permissions_mode: 0,
                owner: "".into(),
                group: "".into(),
                selected: false,
            },
        ]);

        let remote_entries = vec![
            SftpEntry {
                name: ".docker".into(),
                full_path: "/root/.docker".into(),
                is_dir: true,
                size: "".into(),
                size_bytes: 0.0,
                modified: "2024-11-11 14:49".into(),
                modified_ts: 100.0,
                mode: 0o040755,
                file_type: "文件夹".into(),
                permissions: "rwxr-xr-x".into(),
                permissions_mode: 0o755,
                owner: "root".into(),
                group: "root".into(),
                selected: false,
            },
            SftpEntry {
                name: "app-cluster".into(),
                full_path: "/root/app-cluster".into(),
                is_dir: true,
                size: "".into(),
                size_bytes: 0.0,
                modified: "2026-11-09 20:00".into(),
                modified_ts: 600.0,
                mode: 0o040755,
                file_type: "文件夹".into(),
                permissions: "rwxr-xr-x".into(),
                permissions_mode: 0o755,
                owner: "root".into(),
                group: "root".into(),
                selected: false,
            },
            SftpEntry {
                name: "backups".into(),
                full_path: "/root/backups".into(),
                is_dir: true,
                size: "".into(),
                size_bytes: 0.0,
                modified: "2026-11-09 12:30".into(),
                modified_ts: 500.0,
                mode: 0o040750,
                file_type: "文件夹".into(),
                permissions: "rwxr-x---".into(),
                permissions_mode: 0o750,
                owner: "root".into(),
                group: "root".into(),
                selected: false,
            },
            SftpEntry {
                name: "redis-recovery-2026-11-01".into(),
                full_path: "/root/redis-recovery-2026-11-01".into(),
                is_dir: true,
                size: "".into(),
                size_bytes: 0.0,
                modified: "2026-11-01 16:58".into(),
                modified_ts: 490.0,
                mode: 0o040755,
                file_type: "文件夹".into(),
                permissions: "rwxr-xr-x".into(),
                permissions_mode: 0o755,
                owner: "root".into(),
                group: "root".into(),
                selected: false,
            },
            SftpEntry {
                name: "nginx.conf".into(),
                full_path: "/root/nginx.conf".into(),
                is_dir: false,
                size: "8.2 KB".into(),
                size_bytes: 8396.0,
                modified: "2026-11-09 19:45".into(),
                modified_ts: 580.0,
                mode: 0o100644,
                file_type: "CONF 文件".into(),
                permissions: "rw-r--r--".into(),
                permissions_mode: 0o644,
                owner: "root".into(),
                group: "root".into(),
                selected: false,
            },
            SftpEntry {
                name: "settings.json".into(),
                full_path: "/root/settings.json".into(),
                is_dir: false,
                size: "2.4 KB".into(),
                size_bytes: 2457.0,
                modified: "2026-11-09 16:20".into(),
                modified_ts: 480.0,
                mode: 0o100644,
                file_type: "JSON 文件".into(),
                permissions: "rw-r--r--".into(),
                permissions_mode: 0o644,
                owner: "root".into(),
                group: "root".into(),
                selected: false,
            },
            SftpEntry {
                name: "server-metrics.db".into(),
                full_path: "/root/server-metrics.db".into(),
                is_dir: false,
                size: "142.8 MB".into(),
                size_bytes: 149_736_652.0,
                modified: "2026-11-09 20:40".into(),
                modified_ts: 620.0,
                mode: 0o100640,
                file_type: "DB 文件".into(),
                permissions: "rw-r-----".into(),
                permissions_mode: 0o640,
                owner: "postgres".into(),
                group: "postgres".into(),
                selected: false,
            },
        ];

        let transfers = vec![
            TransferInfo {
                id: "1".into(),
                name: "release-v0.8.2.tar.gz".into(),
                detail: "33.0 MB / 48.6 MB · 18.4 MB/s".into(),
                percent: 0.68,
                state: 0,
                is_upload: true,
            },
            TransferInfo {
                id: "2".into(),
                name: "nginx.conf".into(),
                detail: "8.2 KB · 已完成".into(),
                percent: 1.0,
                state: 1,
                is_upload: false,
            },
        ];
        ui.set_transfers(ModelRc::from(Rc::new(VecModel::from(transfers))));
        ui.set_has_active_transfers(true);

        let mut term = terminal(true, local_entries, remote_entries, 420.0, 0.0);
        term.sftp_path = "/root".into();
        term.sftp_local_path = "/Users/huangpj".into();
        term.sftp_panel_height = 420.0;
        term.sftp_sort_key = "name".into();
        term.sftp_sort_dir = 1;
        term.sftp_bookmarked = true;
        term.sftp_bookmarks = ModelRc::from(Rc::new(VecModel::from(vec![
            SharedString::from("/root"),
            SharedString::from("/etc/nginx"),
            SharedString::from("/var/www/html"),
            SharedString::from("/opt/meatshell"),
        ])));
        term.sftp_recent = ModelRc::from(Rc::new(VecModel::from(vec![
            SharedString::from("/var/log"),
            SharedString::from("/tmp"),
        ])));
        ui.set_terminals(ModelRc::from(Rc::new(VecModel::from(vec![term]))));

        crate::i18n::set_language("zh");
        crate::i18n::apply_to_slint();
        // Switch to Light mode first to match the user's screenshot.
        ui.set_dark_mode(false);

        let save_shot = |name: &str, pixels: &[Rgb8Pixel]| {
            let dir = std::path::Path::new(
                "/Users/huangpj/.gemini/antigravity/brain/bd90a851-25a6-47de-a257-4eaf6fbd0a5f/screenshots",
            );
            if dir.parent().map(|p| p.exists()).unwrap_or(false) {
                let _ = std::fs::create_dir_all(dir);
                let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                let _ = image::save_buffer(
                    dir.join(name),
                    &bytes,
                    1440,
                    900,
                    image::ColorType::Rgb8,
                );
            }
        };

        // Shot 1 (Round 6): Split View (双栏) in Light Mode with Dedicated Path Row,
        // Dock/Fullscreen switcher, hidden dotfiles filtered out by default,
        // exclusive column sorting indicators, and root-level Tooltip active via hover.
        // Render once so layout positions are computed, then hover over the fullscreen button (x=1396, y=531).
        let _ = render_frame(&window, 1440, 900);
        move_to(&window, 1396.0, 531.0);
        let f1 = render_frame(&window, 1440, 900);
        save_shot("r6_01_sftp_dual_dedicated_path_light.png", &f1);

        // Shot 2 (Round 6): Subfolder Quick Autocomplete Dropdown open in both Local and Remote panes.
        // Click inside the Local path input (x=250, y=596) so it gains focus AND opens the dropdown,
        // and click the `▾` trigger inside the Remote path bar (x=1346, y=596).
        move_to(&window, 250.0, 596.0);
        press(&window, 250.0, 596.0);
        release(&window, 250.0, 596.0);
        move_to(&window, 1346.0, 596.0);
        press(&window, 1346.0, 596.0);
        release(&window, 1346.0, 596.0);
        let f2 = render_frame(&window, 1440, 900);
        save_shot("r6_02_sftp_subfolder_autocomplete_light.png", &f2);

        // Close the Local autocomplete dropdown by clicking its `×` close button (x=690, y=629)
        // to verify the `×` button closes it even when path-input had focus!
        move_to(&window, 690.0, 629.0);
        press(&window, 690.0, 629.0);
        release(&window, 690.0, 629.0);
        // Close the Remote autocomplete dropdown by clicking its `×` close button (x=1412, y=629)
        move_to(&window, 1412.0, 629.0);
        press(&window, 1412.0, 629.0);
        release(&window, 1412.0, 629.0);

        // Shot 3 (Round 6): Fullscreen SFTP Mode in Dark Mode
        ui.set_dark_mode(true);
        // Click the maximize/fullscreen button in SftpPanel's top mode bar (x=1396, y=531)
        move_to(&window, 1396.0, 531.0);
        press(&window, 1396.0, 531.0);
        release(&window, 1396.0, 531.0);
        let _ = render_frame(&window, 1440, 900);
        // Hover over the fullscreen restore button in the top mode bar (x=1396, y=125) to show its tooltip
        move_to(&window, 1396.0, 125.0);
        let f3 = render_frame(&window, 1440, 900);
        save_shot("r6_03_sftp_fullscreen_dark.png", &f3);

        // Exit fullscreen & return to Light Mode for Round 7 QA screenshots
        press(&window, 1396.0, 125.0);
        release(&window, 1396.0, 125.0);
        move_to(&window, 10.0, 10.0);
        ui.set_dark_mode(false);
        ui.set_download_dir("/Users/huangpj/Downloads".into());

        // Shot 4 (Round 7): Transfer Manager Popover — Empty State (compact height, centered empty card, full untruncated path)
        let saved_transfers = ui.get_transfers();
        ui.set_transfers(ModelRc::from(Rc::new(VecModel::<TransferInfo>::default())));
        ui.set_has_active_transfers(false);
        ui.set_download_open(true);
        let f4 = render_frame(&window, 1440, 900);
        save_shot("r7_01_transfer_manager_empty_light.png", &f4);

        // Shot 5 (Round 7): Transfer Manager Popover — Populated State (active upload + completed download with open-folder button)
        ui.set_transfers(saved_transfers);
        ui.set_has_active_transfers(true);
        let f5 = render_frame(&window, 1440, 900);
        save_shot("r7_02_transfer_manager_populated_light.png", &f5);
        ui.set_download_open(false);

        // Shot 6 (Round 7): Server Resource Monitor Sidebar with 35 Docker/veth Network Interfaces & Scrollable Bounded NIC Dropdown
        ui.set_is_local_mode(false);
        ui.set_conn_state(1);
        ui.set_status_heading("服务器监控".into());
        ui.set_connection_state("root@10.0.0.51".into());
        ui.set_conn_host("root@10.0.0.51".into());
        ui.set_resource_title("核心负载".into());
        ui.set_metrics_available(true);
        ui.set_system_info_available(true);
        ui.set_proc_available(true);
        ui.set_cpu_percent(0.14);
        ui.set_mem_percent(0.38);
        ui.set_mem_detail("23.7G/62.8G".into());
        ui.set_swap_percent(0.01);
        ui.set_swap_detail("0.0G/4.0G".into());
        ui.set_net_top_up("24.3 MB/s".into());
        ui.set_net_top_down("17.6 MB/s".into());
        ui.set_net_show_selector(true);
        ui.set_net_selected("ens32".into());
        let mut many_ifaces: Vec<SharedString> = vec![
            "ens32".into(),
            "docker_gwbridge".into(),
            "docker0".into(),
            "br-2d369e3fa075".into(),
            "br-e18579c1b0c6".into(),
        ];
        for idx in 0..30 {
            many_ifaces.push(format!("veth{:07x}", 0x95aec00 + idx).into());
        }
        ui.set_net_ifaces(ModelRc::from(Rc::new(VecModel::from(many_ifaces))));
        ui.set_dock_panels(ModelRc::from(Rc::new(VecModel::from(vec![
            crate::ui::PanelGeomInfo {
                kind: "sidebar".into(),
                edge: "left".into(),
                x: 48.0,
                y: 0.0,
                w: 280.0,
                h: 868.0,
            },
        ]))));
        ui.set_dock_central_x(328.0);
        ui.set_dock_central_w(1112.0);
        let _ = render_frame(&window, 1440, 900);
        // Click the NIC selector button inside NetBlock (x=266, y=395) to open the bounded scrollable PopupWindow!
        move_to(&window, 266.0, 395.0);
        press(&window, 266.0, 395.0);
        release(&window, 266.0, 395.0);
        let f6 = render_frame(&window, 1440, 900);
        save_shot("r7_03_nic_dropdown_scrollable_35_ifaces_light.png", &f6);

        // Close the NIC popup by clicking outside
        press(&window, 600.0, 200.0);
        release(&window, 600.0, 200.0);

        // Shot 7 (Round 8): Quick Commands Sidebar (`QuickDockPanel`) with Grouped Command Cards & Hover Actions
        let quick_cmds = vec![
            crate::ui::QuickCmd {
                name: "列出详细目录 (ls)".into(),
                command: "ls -la --color=auto".into(),
                group: "default".into(),
                group_header: "default".into(),
                collapsed: false,
                orig_index: 0,
                send_enter: true,
                command_preview: "ls -la --color=auto".into(),
            },
            crate::ui::QuickCmd {
                name: "排查端口占用".into(),
                command: "lsof -i :8080".into(),
                group: "default".into(),
                group_header: "".into(),
                collapsed: false,
                orig_index: 1,
                send_enter: false,
                command_preview: "lsof -i :8080".into(),
            },
            crate::ui::QuickCmd {
                name: "查看运行容器状态".into(),
                command: "docker ps --format 'table {{.Names}}\\t{{.Status}}\\t{{.Ports}}'".into(),
                group: "Docker 运维".into(),
                group_header: "Docker 运维".into(),
                collapsed: false,
                orig_index: 2,
                send_enter: true,
                command_preview: "docker ps --format 'table {{.Names}}\\t{{.Status}}\\t{{.Ports}}'".into(),
            },
            crate::ui::QuickCmd {
                name: "实时追踪容器日志".into(),
                command: "docker logs -f --tail 200 app-server".into(),
                group: "Docker 运维".into(),
                group_header: "".into(),
                collapsed: false,
                orig_index: 3,
                send_enter: false,
                command_preview: "docker logs -f --tail 200 app-server".into(),
            },
        ];
        ui.set_quick_commands(ModelRc::from(Rc::new(VecModel::from(quick_cmds))));
        ui.set_quick_panel_open(true);
        ui.set_quick_panel_collapsed(false);
        ui.set_quick_panel_dock("left".into());
        ui.set_dock_panels(ModelRc::from(Rc::new(VecModel::from(vec![
            crate::ui::PanelGeomInfo {
                kind: "quick".into(),
                edge: "left".into(),
                x: 48.0,
                y: 0.0,
                w: 280.0,
                h: 868.0,
            },
        ]))));
        let _ = render_frame(&window, 1440, 900);
        // Hover over the second command card in QuickDockPanel (x=180, y=225) to show Run / Fill / Edit micro-buttons
        move_to(&window, 180.0, 225.0);
        let f7 = render_frame(&window, 1440, 900);
        save_shot("r8_01_quick_commands_sidebar_light.png", &f7);

        // Shot 8 (Round 8): Quick Commands Management / Add & Edit Modal in Light Mode
        move_to(&window, 10.0, 10.0);
        ui.set_qcm_edit_index(-1);
        ui.set_qcm_name("查看 Nginx 错误日志".into());
        ui.set_qcm_group("Docker 运维".into());
        ui.set_qcm_command("tail -n 100 -f /var/log/nginx/error.log".into());
        ui.set_qcm_send_enter(true);
        ui.set_quick_cmd_manage_open(true);
        let f8 = render_frame(&window, 1440, 900);
        save_shot("r8_02_quick_commands_modal_light.png", &f8);

        // Shot 9 (Round 8): Quick Commands Sidebar + Modal in Dark Mode (Editing mode preview)
        ui.set_dark_mode(true);
        ui.set_qcm_edit_index(1);
        ui.set_qcm_name("排查端口占用".into());
        ui.set_qcm_group("".into());
        ui.set_qcm_command("lsof -i :8080".into());
        ui.set_qcm_send_enter(false);
        let f9 = render_frame(&window, 1440, 900);
        save_shot("r8_03_quick_commands_modal_dark.png", &f9);
        ui.set_quick_cmd_manage_open(false);
        ui.set_dark_mode(false);

        // Shot 10 (Round 9): Settings › Terminal with iTerm2-Color-Schemes Theme Manager (Light Mode)
        {
            let mgr = crate::terminal_theme::ThemeManager::new(
                "default".into(),
                vec!["dracula".into(), "catppuccin-mocha".into()],
                Vec::new(),
                false,
            );
            let total = mgr.all_themes().len() as i32;
            let to_c = |(r, g, b): (u8, u8, u8)| slint::Color::from_rgb_u8(r, g, b);
            let to_item = |t: &crate::terminal_theme::TerminalTheme, fav: bool| -> crate::ui::TerminalSchemeItem {
                let p = t.to_resolved_palette();
                let name = if t.theme_id == "default" {
                    "默认配色 (跟随工作台明暗)".to_string()
                } else {
                    t.name.clone()
                };
                crate::ui::TerminalSchemeItem {
                    id: t.theme_id.clone().into(),
                    name: name.into(),
                    author: t.author.clone().into(),
                    is_dark: p.is_dark,
                    is_favorite: fav,
                    is_custom: false,
                    bg: to_c(p.background),
                    fg: to_c(p.foreground),
                    cursor: to_c(p.cursor),
                    cursor_text: to_c(p.cursor_text),
                    selection: to_c(p.selection),
                    selection_text: to_c(p.selection_text),
                    c0: to_c(p.ansi[0]),
                    c1: to_c(p.ansi[1]),
                    c2: to_c(p.ansi[2]),
                    c3: to_c(p.ansi[3]),
                    c4: to_c(p.ansi[4]),
                    c5: to_c(p.ansi[5]),
                    c6: to_c(p.ansi[6]),
                    c7: to_c(p.ansi[7]),
                    c8: to_c(p.ansi[8]),
                    c9: to_c(p.ansi[9]),
                    c10: to_c(p.ansi[10]),
                    c11: to_c(p.ansi[11]),
                    c12: to_c(p.ansi[12]),
                    c13: to_c(p.ansi[13]),
                    c14: to_c(p.ansi[14]),
                    c15: to_c(p.ansi[15]),
                }
            };
            let featured_ids = [
                "default",
                "dracula",
                "catppuccin-mocha",
                "catppuccin-latte",
                "tokyonight",
                "tokyonight-day",
                "iterm2-solarized-dark",
                "iterm2-solarized-light",
                "gruvbox-dark",
                "nord",
                "one-half-dark",
                "atom-one-light",
            ];
            let scheme_items: Vec<crate::ui::TerminalSchemeItem> = featured_ids
                .iter()
                .filter_map(|id| mgr.get_theme(id).map(|t| to_item(&t, *id == "dracula" || *id == "catppuccin-mocha")))
                .collect();
            ui.set_terminal_schemes_total(total);
            ui.set_terminal_schemes_has_more(true);
            ui.set_terminal_schemes(ModelRc::from(Rc::new(VecModel::from(scheme_items.clone()))));
            let def_theme = mgr.current_theme();
            ui.set_terminal_scheme_id("default".into());
            ui.set_active_terminal_scheme(to_item(&def_theme, false));
            ui.set_interface_start_page("terminal".into());
            ui.set_interface_open(true);
            let f10 = render_frame(&window, 1440, 900);
            save_shot("r9_01_terminal_themes_settings_light.png", &f10);

            // Shot 11 (Round 9): Settings › Terminal with Dracula Selected & Previewed (Dark Mode)
            ui.set_dark_mode(true);
            let dracula = mgr.get_theme("dracula").expect("dracula exists");
            let dp = dracula.to_resolved_palette();
            ui.set_terminal_scheme_id("dracula".into());
            ui.set_active_terminal_scheme(to_item(&dracula, true));
            ui.set_term_scheme_active(true);
            ui.set_term_scheme_is_dark(dp.is_dark);
            ui.set_term_scheme_bg(to_c(dp.background));
            ui.set_term_scheme_fg(to_c(dp.foreground));
            ui.set_term_scheme_cursor(to_c(dp.cursor));
            ui.set_term_scheme_cursor_text(to_c(dp.cursor_text));
            ui.set_term_scheme_selection(to_c(dp.selection));
            ui.set_term_scheme_selection_text(to_c(dp.selection_text));
            ui.set_term_scheme_accent(to_c(dp.ansi[4]));
            ui.set_term_cursor_color(to_c(dp.cursor));
            let f11 = render_frame(&window, 1440, 900);
            save_shot("r9_02_terminal_themes_settings_dracula_dark.png", &f11);

            // Shot 12 (Round 9): Active Terminal Rendered Live with Dracula Palette (ANSI 16 + TrueColor + Selection + CommandBar)
            ui.set_interface_open(false);
            ui.set_sftp_collapsed(true);
            ui.set_dock_central_h(868.0);
            ui.set_panes(ModelRc::from(Rc::new(VecModel::from(vec![PaneInfo {
                id: 0,
                x: 0.0,
                y: 0.0,
                w: 1112.0,
                h: 868.0,
                active_id: "t1".into(),
                focused: true,
                reserve_right: 0.0,
                tabs: ModelRc::from(Rc::new(VecModel::from(vec![tab("t1")]))),
            }]))));
            let spans = vec![
                crate::ui::TermSpan {
                    text: "root@prod-cluster".into(),
                    row: 0,
                    col: 0,
                    cells: 17,
                    fg: to_c(dp.ansi[10]),
                    bg: slint::Color::from_argb_u8(0, 0, 0, 0),
                    bold: true,
                    ..Default::default()
                },
                crate::ui::TermSpan {
                    text: ":".into(),
                    row: 0,
                    col: 17,
                    cells: 1,
                    fg: to_c(dp.foreground),
                    bg: slint::Color::from_argb_u8(0, 0, 0, 0),
                    ..Default::default()
                },
                crate::ui::TermSpan {
                    text: "~/meatshell".into(),
                    row: 0,
                    col: 18,
                    cells: 11,
                    fg: to_c(dp.ansi[12]),
                    bg: slint::Color::from_argb_u8(0, 0, 0, 0),
                    bold: true,
                    ..Default::default()
                },
                crate::ui::TermSpan {
                    text: " (main*) $ cargo test --locked".into(),
                    row: 0,
                    col: 29,
                    cells: 30,
                    fg: to_c(dp.ansi[13]),
                    bg: slint::Color::from_argb_u8(0, 0, 0, 0),
                    ..Default::default()
                },
                crate::ui::TermSpan {
                    text: "   Compiling meatshell v0.8.2 (iTerm2-Color-Schemes: 915 community themes loaded)".into(),
                    row: 1,
                    col: 0,
                    cells: 81,
                    fg: to_c(dp.ansi[6]),
                    bg: slint::Color::from_argb_u8(0, 0, 0, 0),
                    bold: true,
                    ..Default::default()
                },
                crate::ui::TermSpan {
                    text: "test terminal_theme::tests::all_915_builtin_themes_load ... ".into(),
                    row: 2,
                    col: 0,
                    cells: 60,
                    fg: to_c(dp.foreground),
                    bg: slint::Color::from_argb_u8(0, 0, 0, 0),
                    ..Default::default()
                },
                crate::ui::TermSpan {
                    text: "ok".into(),
                    row: 2,
                    col: 60,
                    cells: 2,
                    fg: to_c(dp.ansi[2]),
                    bg: slint::Color::from_argb_u8(0, 0, 0, 0),
                    bold: true,
                    ..Default::default()
                },
                crate::ui::TermSpan {
                    text: "WARN: High memory pressure on node-03 (87.4%)".into(),
                    row: 3,
                    col: 0,
                    cells: 45,
                    fg: to_c(dp.ansi[3]),
                    bg: slint::Color::from_argb_u8(0, 0, 0, 0),
                    ..Default::default()
                },
                crate::ui::TermSpan {
                    text: "ERROR: Connection timeout on replica-02 (retrying in 2s)".into(),
                    row: 4,
                    col: 0,
                    cells: 56,
                    fg: to_c(dp.ansi[1]),
                    bg: slint::Color::from_argb_u8(0, 0, 0, 0),
                    bold: true,
                    ..Default::default()
                },
                crate::ui::TermSpan {
                    text: " TRUECOLOR 24-BIT RGB ".into(),
                    row: 5,
                    col: 0,
                    cells: 22,
                    fg: slint::Color::from_rgb_u8(255, 255, 255),
                    bg: slint::Color::from_rgb_u8(139, 92, 246),
                    bold: true,
                    ..Default::default()
                },
            ];
            let mut term_item = terminal(false, vec![], vec![], 300.0, 300.0);
            term_item.spans = ModelRc::from(Rc::new(VecModel::from(spans)));
            term_item.cursor_row = 6;
            term_item.cursor_col = 0;
            term_item.rows_used = 7;
            term_item.sftp_collapsed = true;
            term_item.selection = ModelRc::from(Rc::new(VecModel::from(vec![crate::ui::TermMatch {
                row: 2,
                col: 5,
                len: 50,
            }])));
            ui.set_terminals(ModelRc::from(Rc::new(VecModel::from(vec![term_item]))));
            let f12 = render_frame(&window, 1440, 900);
            save_shot("r9_03_terminal_live_dracula.png", &f12);

            // Shot 13: CommandBar floating Quick Commands Popup open in Dracula Terminal
            ui.set_quick_panel_open(false);
            ui.set_dock_panels(ModelRc::from(Rc::new(VecModel::<crate::ui::PanelGeomInfo>::default())));
            ui.set_dock_central_x(48.0);
            ui.set_dock_central_w(1392.0);
            ui.set_panes(ModelRc::from(Rc::new(VecModel::from(vec![PaneInfo {
                id: 0,
                x: 0.0,
                y: 0.0,
                w: 1392.0,
                h: 868.0,
                active_id: "t1".into(),
                focused: true,
                reserve_right: 0.0,
                tabs: ModelRc::from(Rc::new(VecModel::from(vec![tab("t1")]))),
            }]))));
            let _ = render_frame(&window, 1440, 900);
            // Click the `⚡ 快捷 ⌃` pill on CommandBar (x=82, y=881) to open the Quick Commands popup
            move_to(&window, 82.0, 881.0);
            press(&window, 82.0, 881.0);
            release(&window, 82.0, 881.0);
            let f13 = render_frame(&window, 1440, 900);
            save_shot("r9_04_command_bar_quick_popup_dracula.png", &f13);
        }
    })
    .join()
    .unwrap();
}
