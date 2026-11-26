use super::*;

use std::cell::RefCell;
use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, VecModel};

struct Backend(Rc<MinimalSoftwareWindow>);

impl slint::platform::Platform for Backend {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn terminal_state() -> TerminalState {
    TerminalState {
        id: "t1".into(),
        status: "connected".into(),
        spans: ModelRc::from(Rc::new(VecModel::default())),
        cursor_row: 0,
        cursor_col: 0,
        rows_used: 5,
        scroll_max: 20,
        scroll_offset: 0,
        is_alt_screen: false,
        mouse_tracked: false,
        hint_items: ModelRc::from(Rc::new(VecModel::default())),
        hint_row: -1,
        hint_col: 0,
        find_matches: ModelRc::from(Rc::new(VecModel::default())),
        find_current: Default::default(),
        selection: ModelRc::from(Rc::new(VecModel::default())),
        sftp_path: "".into(),
        sftp_entries: ModelRc::from(Rc::new(VecModel::default())),
        sftp_all_entries: ModelRc::from(Rc::new(VecModel::<SftpEntry>::default())),
        sftp_filter: "".into(),
        sftp_status: "".into(),
        sftp_loading: false,
        sftp_tree_nodes: ModelRc::from(Rc::new(VecModel::default())),
        sftp_selected_count: 0,
        sftp_sort_key: "".into(),
        sftp_sort_dir: 0,
        sftp_available: false,
        sftp_bookmarks: ModelRc::from(Rc::new(VecModel::default())),
        sftp_recent: ModelRc::from(Rc::new(VecModel::default())),
        sftp_bookmarked: false,
        sftp_dual: false,
        sftp_local_path: "".into(),
        sftp_local_entries: ModelRc::from(Rc::new(VecModel::default())),
        sftp_local_status: "".into(),
        sftp_local_loading: false,
        sftp_local_selected_count: 0,
        sftp_local_pane_width: 0.0,
        sftp_local_quick_places: ModelRc::from(Rc::new(VecModel::default())),
        sftp_local_drive_places: ModelRc::from(Rc::new(VecModel::default())),
        sftp_local_bookmarks: ModelRc::from(Rc::new(VecModel::default())),
        sftp_local_recent: ModelRc::from(Rc::new(VecModel::default())),
        sftp_local_bookmarked: false,
        sftp_local_show_hidden: false,
        sftp_local_sort_key: "".into(),
        sftp_local_sort_dir: 0,
        sftp_local_filter: "".into(),
        sftp_local_all_entries: ModelRc::from(Rc::new(VecModel::<SftpEntry>::default())),
        sftp_suggest: Default::default(),
        sftp_local_suggest: Default::default(),
        font_size: 0,
        tunnels: ModelRc::from(Rc::new(VecModel::default())),
        sftp_collapsed: true,
        sftp_panel_height: 0.0,
        sftp_panel_width: 0.0,
        sftp_saved_height: 0.0,
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

fn build_ui() -> (AppWindow, Rc<MinimalSoftwareWindow>) {
    let (width, height) = (1400_u32, 900_u32);
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Backend(window.clone()))).unwrap();
    let ui = AppWindow::new().unwrap();

    ui.set_terminals(ModelRc::from(Rc::new(VecModel::from(vec![terminal_state()]))));
    ui.set_tabs(ModelRc::from(Rc::new(VecModel::from(vec![tab("t1")]))));
    ui.set_panes(ModelRc::from(Rc::new(VecModel::from(vec![PaneInfo {
        id: 0,
        x: 0.0,
        y: 0.0,
        w: width as f32,
        h: height as f32,
        active_id: "t1".into(),
        focused: true,
        reserve_right: 0.0,
        tabs: ModelRc::from(Rc::new(VecModel::from(vec![tab("t1")]))),
    }]))));
    ui.set_active_tab_id("t1".into());
    ui.set_dock_central_x(0.0);
    ui.set_dock_central_y(0.0);
    ui.set_dock_central_w(width as f32);
    ui.set_dock_central_h(height as f32);
    ui.show().unwrap();
    window.set_size(slint::PhysicalSize::new(width, height));
    // One frame so the item tree is laid out before events are dispatched.
    let mut pixels = vec![slint::Rgb8Pixel::default(); (width * height) as usize];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, width as usize);
    });
    (ui, window)
}

#[test]
fn clearing_a_stuck_grab_lets_the_terminal_receive_wheel_events_again() {
    std::thread::spawn(|| {
        let (ui, window) = build_ui();
        let scrolled = Rc::new(RefCell::new(0.0_f32));
        {
            let scrolled = scrolled.clone();
            ui.on_terminal_scroll(move |_tab: slint::SharedString, delta: f32| {
                *scrolled.borrow_mut() += delta;
            });
        }
        let clicked = Rc::new(RefCell::new(false));
        {
            let clicked = clicked.clone();
            ui.on_term_select_start(
                move |_tab: slint::SharedString, _r: i32, _c: i32, _ctrl: bool, _shift: bool| {
                    *clicked.borrow_mut() = true;
                },
            );
        }

        let terminal = LogicalPosition::new(600.0, 300.0);
        let tab_strip = LogicalPosition::new(40.0, 55.0);
        // Press on a tab and never release — on Windows the release is lost
        // when alt+tab breaks the mouse capture, leaving Slint's mouse grab
        // stuck on the tab's item stack.
        window.dispatch_event(WindowEvent::PointerPressed {
            position: tab_strip,
            button: PointerEventButton::Left,
        });
        // A wheel over the terminal must NOT reach it while the grab lives.
        window.dispatch_event(WindowEvent::PointerScrolled {
            position: terminal,
            delta_x: 0.0,
            delta_y: -60.0,
        });
        assert_eq!(
            *scrolled.borrow(),
            0.0,
            "stuck grab routed the wheel past the terminal"
        );

        // Focus loss dispatches PointerExited (the window-events hook fix),
        // which clears the grab; the next wheel is hit-tested again.
        window.dispatch_event(WindowEvent::PointerExited);
        window.dispatch_event(WindowEvent::PointerScrolled {
            position: terminal,
            delta_x: 0.0,
            delta_y: -60.0,
        });
        assert!(
            *scrolled.borrow() != 0.0,
            "after the exit the terminal receives the wheel again"
        );
        // A click on the terminal reaches it as well (the user's "clicking
        // doesn't work either" half of the report).
        window.dispatch_event(WindowEvent::PointerPressed {
            position: terminal,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position: terminal,
            button: PointerEventButton::Left,
        });
        assert!(*clicked.borrow(), "after the exit the terminal receives clicks");
    })
    .join()
    .unwrap();
}

#[test]
fn deactivation_dispatches_the_pointer_exit() {
    let source = include_str!("../../../src/app/window_events.rs");
    let start = source.find("if !*f {").expect("deactivation arm of WEvent::Focused");
    let arm = &source[start..start + 1500];
    assert!(
        arm.contains("WindowEvent::PointerExited"),
        "losing focus must clear Slint's mouse state (stuck grab after alt+tab)"
    );
}
