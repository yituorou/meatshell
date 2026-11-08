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
