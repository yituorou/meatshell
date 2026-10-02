use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Clipboard, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, Rgb8Pixel};
use std::{cell::RefCell, rc::Rc};

slint::slint! {
    import { LabeledInput } from "../ui/widgets.slint";
    import { JumpRow, SessionDialog } from "../ui/session_dialog.slint";
    import { Theme } from "../ui/theme.slint";
    import "../ui/fonts/MaterialIcons-Regular.ttf";
    import { Palette } from "std-widgets.slint";
    export component EditorFixture inherits Window {
        width: 360px; height: 240px; background: #000000;
        in-out property <string> note;
        in-out property <string> secret;
        in-out property <bool> allow-reveal: false;
        out property <bool> revealed: password.revealed;
        callback reveal() -> string;
        callback move-hop(int);
        public function focus-note() { note-input.focus-input(); }
        init => {
            Theme.ui-font-family = "Microsoft YaHei";
            Palette.color-scheme = ColorScheme.dark;
        }
        note-input := LabeledInput {
            x: 20px; y: 10px; width: 240px; height: 56px;
            label: "Note"; value <=> root.note;
        }
        password := LabeledInput {
            x: 20px; y: 80px; width: 240px; height: 56px;
            label: "Password"; password: true;
            value <=> root.secret; reveal-enabled: root.allow-reveal;
            reveal => { root.reveal() }
        }
        JumpRow {
            x: 20px; y: 160px; width: 300px;
            index: 0; count: 3; choices: ["Select", "a", "b"];
            hop: { id: "a", choice: 1 };
            move(to) => { root.move-hop(to); }
        }
    }
    export component DialogFixture inherits Window {
        width: 700px; height: 1100px;
        in-out property <bool> open: true;
        in-out property <bool> mac: false;
        in-out property <bool> covered: false;
        out property <int> cancel-count: 0;
        in-out property <string> password;
        in-out property <string> key;
        callback move-hop(int, int);
        init => {
            Theme.ui-font-family = "Microsoft YaHei";
            Palette.color-scheme = ColorScheme.dark;
        }
        SessionDialog {
            is-editing: true;
            is-mac: root.mac;
            covered: root.covered;
            cancel => { root.cancel-count += 1; root.open = false; }
            draft-name: "Example multi-hop session";
            draft-host: "target.example.invalid";
            draft-user: "demo";
            draft-note: "Example route, no real credentials";
            allow-secret-reveal: true;
            jump-choices: ["Select a jump host", "Bastion A (demo@a.example.invalid:22)", "Bastion B (demo@b.example.invalid:22)"];
            jumps: [{id: "a", choice: 1}, {id: "b", choice: 2}];
            move-jump(from, to) => { root.move-hop(from, to); }
            is-open <=> root.open;
            draft-password <=> root.password;
            draft-key-inline <=> root.key;
        }
    }
}

struct Backend {
    window: Rc<MinimalSoftwareWindow>,
    clipboard: Rc<RefCell<String>>,
}
impl slint::platform::Platform for Backend {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn set_clipboard_text(&self, text: &str, _: Clipboard) {
        *self.clipboard.borrow_mut() = text.into();
    }
    fn clipboard_text(&self, _: Clipboard) -> Option<String> {
        Some(self.clipboard.borrow().clone())
    }
}

fn render(window: &MinimalSoftwareWindow) -> Vec<Rgb8Pixel> {
    let mut pixels = vec![Rgb8Pixel::default(); 360 * 240];
    window.request_redraw();
    assert!(window.draw_if_needed(|r| {
        r.render(&mut pixels, 360);
    }));
    pixels
}
fn press(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    window.dispatch_event(WindowEvent::PointerPressed {
        position: slint::LogicalPosition::new(x, y),
        button: PointerEventButton::Left,
    });
}
fn release(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    window.dispatch_event(WindowEvent::PointerReleased {
        position: slint::LogicalPosition::new(x, y),
        button: PointerEventButton::Left,
    });
}
fn copy(window: &MinimalSoftwareWindow) {
    use slint::platform::Key;
    let modifier = if cfg!(target_os = "macos") {
        Key::Meta
    } else {
        Key::Control
    };
    for text in [slint::SharedString::from(modifier), "c".into()] {
        window.dispatch_event(WindowEvent::KeyPressed { text });
    }
    for text in [slint::SharedString::from("c"), modifier.into()] {
        window.dispatch_event(WindowEvent::KeyReleased { text });
    }
}

#[test]
fn editor_mouse_selection_clips_scrolls_and_reveals_only_on_click() {
    std::thread::spawn(|| {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        let clipboard = Rc::new(RefCell::new(String::new()));
        slint::platform::set_platform(Box::new(Backend {
            window: window.clone(),
            clipboard: clipboard.clone(),
        }))
        .unwrap();
        let ui = EditorFixture::new().unwrap();
        ui.show().unwrap();
        window.set_size(slint::PhysicalSize::new(360, 240));
        let text = "测试中文备注0123456789".repeat(16);
        ui.set_note(text.clone().into());
        render(&window);
        press(&window, 32., 50.);
        for _ in 0..100 {
            window.dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(300., 50.),
            });
            let pixels = render(&window);
            // No selection, text or cursor may paint outside the field.
            for y in 36..65 {
                for x in 262..360 {
                    let pixel = pixels[y * 360 + x];
                    assert_eq!((pixel.r, pixel.g, pixel.b), (0, 0, 0));
                }
            }
        }
        release(&window, 300., 50.);
        copy(&window);
        assert!(
            clipboard.borrow().ends_with("0123456789"),
            "drag did not reach hidden end"
        );
        assert!(
            clipboard.borrow().len() > text.len() / 2,
            "selection did not scroll"
        );
        press(&window, 248., 50.);
        for _ in 0..100 {
            window.dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(0., 50.),
            });
            render(&window);
        }
        release(&window, 0., 50.);
        copy(&window);
        assert!(
            clipboard.borrow().starts_with("测试"),
            "reverse drag did not reach start"
        );

        let calls = Rc::new(RefCell::new(0));
        let c = calls.clone();
        ui.on_reveal(move || {
            *c.borrow_mut() += 1;
            "fixture-only-secret".into()
        });
        assert!(ui.get_secret().is_empty());
        assert!(!ui.get_revealed());
        ui.set_allow_reveal(true);
        render(&window);
        assert_eq!(*calls.borrow(), 0);
        press(&window, 240., 120.);
        release(&window, 240., 120.);
        render(&window);
        assert_eq!(*calls.borrow(), 1);
        assert!(ui.get_revealed());
        assert_eq!(ui.get_secret(), "fixture-only-secret");
        press(&window, 240., 120.);
        release(&window, 240., 120.);
        assert!(!ui.get_revealed());
        ui.set_allow_reveal(false);
        render(&window);
        assert!(!ui.get_revealed());

        let moved = Rc::new(RefCell::new(-1));
        let m = moved.clone();
        ui.on_move_hop(move |to| *m.borrow_mut() = to);
        press(&window, 32., 185.);
        window.dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(32., 313.),
        });
        release(&window, 32., 313.);
        assert_eq!(*moved.borrow(), 2);
    })
    .join()
    .unwrap();
}

#[test]
fn closing_dialog_clears_ui_secret_buffers() {
    std::thread::spawn(|| {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Backend {
            window: window.clone(),
            clipboard: Rc::default(),
        }))
        .unwrap();
        let ui = DialogFixture::new().unwrap();
        ui.show().unwrap();
        window.set_size(slint::PhysicalSize::new(700, 1100));
        let mut pixels = vec![Rgb8Pixel::default(); 700 * 1100];
        window.request_redraw();
        assert!(window.draw_if_needed(|r| {
            r.render(&mut pixels, 700);
        }));
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/session-editor.png");
        image::save_buffer(path, &bytes, 700, 1100, image::ColorType::Rgb8).unwrap();
        // These full-dialog coordinates use Windows font metrics. The isolated
        // row drag above is independent of platform font layout.
        #[cfg(windows)]
        {
            let moved = Rc::new(RefCell::new((-1, -1)));
            let m = moved.clone();
            ui.on_move_hop(move |from, to| *m.borrow_mut() = (from, to));
            // Real dialog coordinates in the fixed-size fixture, including its Flickable.
            press(&window, 162., 795.);
            window.dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(162., 859.),
            });
            release(&window, 162., 859.);
            assert_eq!(*moved.borrow(), (0, 1), "dialog scrolling stole the drag");
        }
        ui.set_password("fixture-password".into());
        ui.set_key("fixture-private-key".into());
        ui.set_open(false);
        slint::platform::update_timers_and_animations();
        assert!(ui.get_password().is_empty());
        assert!(ui.get_key().is_empty());
        ui.set_open(true);
        slint::platform::update_timers_and_animations();
        assert!(ui.get_password().is_empty());
    })
    .join()
    .unwrap();
}

#[test]
fn session_cancel_shortcuts_work_on_open_and_in_focused_inputs() {
    std::thread::spawn(|| {
        use slint::platform::Key;
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Backend {
            window: window.clone(),
            clipboard: Rc::default(),
        }))
        .unwrap();
        let ui = DialogFixture::new().unwrap();
        ui.show().unwrap();
        window.set_size(slint::PhysicalSize::new(700, 1100));
        let settle = || {
            slint::platform::update_timers_and_animations();
            let mut pixels = vec![Rgb8Pixel::default(); 700 * 1100];
            window.request_redraw();
            window.draw_if_needed(|r| {
                r.render(&mut pixels, 700);
            });
            slint::platform::update_timers_and_animations();
        };
        let key = |value: slint::SharedString| {
            window.dispatch_event(WindowEvent::KeyPressed {
                text: value.clone(),
            });
            window.dispatch_event(WindowEvent::KeyReleased { text: value });
        };
        settle();
        ui.set_password("unsaved-fixture".into());
        key(Key::Escape.into());
        settle();
        assert!(
            !ui.get_open(),
            "Escape must cancel immediately after opening"
        );
        assert!(ui.get_password().is_empty());
        assert_eq!(ui.get_cancel_count(), 1);
        key(Key::Escape.into());
        assert_eq!(
            ui.get_cancel_count(),
            1,
            "closed dialogs must not consume keys"
        );

        ui.set_open(true);
        settle();
        // The note input is within the real dialog's keyboard scope.
        #[cfg(windows)]
        {
            press(&window, 250., 385.);
            release(&window, 250., 385.);
        }
        key(Key::Escape.into());
        settle();
        assert!(!ui.get_open(), "a focused input must not swallow Escape");

        ui.set_open(true);
        ui.set_mac(true);
        settle();
        key(".".into());
        assert!(ui.get_open(), "a plain period must not cancel");
        ui.set_covered(true);
        settle();
        key(Key::Escape.into());
        assert!(
            ui.get_open(),
            "a higher-priority dialog must keep ownership"
        );
        ui.set_covered(false);
        settle();
        let command = if cfg!(target_os = "macos") {
            Key::Meta
        } else {
            Key::Control
        };
        window.dispatch_event(WindowEvent::KeyPressed {
            text: command.into(),
        });
        key(".".into());
        window.dispatch_event(WindowEvent::KeyReleased {
            text: command.into(),
        });
        settle();
        assert!(!ui.get_open(), "macOS Command-period must cancel");

        ui.set_mac(false);
        ui.set_open(true);
        settle();
        window.dispatch_event(WindowEvent::KeyPressed {
            text: command.into(),
        });
        key(".".into());
        window.dispatch_event(WindowEvent::KeyReleased {
            text: command.into(),
        });
        assert!(ui.get_open(), "Command-period is macOS-only");
        #[cfg(windows)]
        {
            press(&window, 490., 808.);
            release(&window, 490., 808.);
            settle();
            key(Key::Escape.into());
            settle();
            assert!(
                ui.get_open(),
                "Escape must close the jump popup before its dialog"
            );
            key(Key::Escape.into());
            settle();
            assert!(
                !ui.get_open(),
                "focus must return to the dialog after popup dismissal"
            );
        }
    })
    .join()
    .unwrap();
}
