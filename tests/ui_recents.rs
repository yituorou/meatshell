//! Recent connections: render the production card (empty main area of the
//! welcome-sidebar layout) and shelf (full welcome page) over the built-in
//! wallpaper, and check that clicks connect, × removes and Clear clears.
//!
//! Set `MEATSHELL_RECENTS_QA_DIR` to also write the frames as PNGs.
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model, Rgb8Pixel};
use std::cell::RefCell;
use std::rc::Rc;

slint::slint! {
    import "../ui/fonts/MaterialIcons-Regular.ttf";
    import { Theme } from "../ui/theme.slint";
    import { RecentSession, RecentsCard } from "../ui/recents.slint";
    import { Welcome, SessionInfo } from "../ui/welcome.slint";
    export { RecentSession, SessionInfo }

    export component RecentsFixture inherits Window {
        width: 1100px; height: 660px;
        in property <bool> sidebar-mode: true;
        in property <bool> wallpaper: true;
        in property <bool> dark: true;
        in property <[RecentSession]> recents;
        in property <[SessionInfo]> sessions;
        callback connect(string);
        callback remove(string);
        callback clear();
        init => {
            Theme.dark = root.dark;
            Theme.wallpaper-active = root.wallpaper;
            Theme.immersive = false;
        }
        changed dark => { Theme.dark = root.dark; }
        changed wallpaper => { Theme.wallpaper-active = root.wallpaper; }
        background: Theme.window-base;

        if root.wallpaper : Image {
            width: parent.width; height: parent.height;
            source: @image-url("../assets/ms.jpg");
            image-fit: cover;
        }

        // Welcome-sidebar layout: frosted session list on the left, the
        // empty main area with the card on the right (same placement as
        // app.slint).
        if root.sidebar-mode : Rectangle {
            Rectangle {
                x: 12px; y: 12px; width: 274px; height: parent.height - 24px;
                background: Theme.bg-panel;
                border-radius: Theme.radius-md;
                Text {
                    x: 16px; y: 16px;
                    text: "Quick connect";
                    color: Theme.text-primary; font-size: Theme.fs-lg; font-weight: 600;
                }
            }
            main := Rectangle {
                x: 298px; y: 0; width: parent.width - 298px; height: parent.height;
                background: Theme.bg-root;
                card := RecentsCard {
                    property <length> margin: 24px;
                    width: min(380px, parent.width - 2 * self.margin);
                    x: (parent.width - self.width) / 2;
                    y: max(self.margin, min(parent.height * 0.56,
                        parent.height - self.height - self.margin));
                    items: root.recents;
                    connect(id) => { root.connect(id); }
                    remove(id) => { root.remove(id); }
                    clear => { root.clear(); }
                }
            }
        }

        if !root.sidebar-mode : Welcome {
            width: parent.width; height: parent.height;
            sessions: root.sessions;
            recents: root.recents;
            connect-session(id) => { root.connect(id); }
            remove-recent(id) => { root.remove(id); }
            clear-recents => { root.clear(); }
        }
    }
}

struct Backend(Rc<MinimalSoftwareWindow>);
impl slint::platform::Platform for Backend {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

const W: u32 = 1100;
const H: u32 = 660;

fn frame(window: &MinimalSoftwareWindow) -> Vec<Rgb8Pixel> {
    let mut pixels = vec![Rgb8Pixel::default(); (W * H) as usize];
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, W as usize);
    });
    pixels
}

fn snapshot(window: &MinimalSoftwareWindow, name: &str) {
    let pixels = frame(window);
    let Ok(dir) = std::env::var("MEATSHELL_RECENTS_QA_DIR") else {
        return;
    };
    let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    std::fs::create_dir_all(&dir).unwrap();
    image::save_buffer(
        std::path::Path::new(&dir).join(name),
        &bytes,
        W,
        H,
        image::ColorType::Rgb8,
    )
    .unwrap();
}

fn pointer(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(x, y),
    });
}

fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    pointer(window, x, y);
    let position = LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    frame(window);
}

fn recents() -> Vec<RecentSession> {
    let row = |id: &str, name: &str, detail: &str, when: &str, icon: &str| RecentSession {
        id: id.into(),
        name: name.into(),
        detail: detail.into(),
        when: when.into(),
        icon: icon.into(),
    };
    vec![
        row("pve", "pve", "root@192.168.100.2", "Just now", "\u{E875}"),
        row("local", "local_server", "jeff@192.168.100.41", "12 min ago", "\u{E875}"),
        row("ps", "PowerShell", "Local shell", "09:05", "\u{EB8E}"),
        row("rdp", "内网电脑", "jeff@192.168.100.20", "Yesterday", "\u{E30C}"),
        row("koko", "issue359-koko", "issue359@192.168.100.41:2222", "Saturday", "\u{E875}"),
        row("istore", "iStoreOS", "root@192.168.100.1", "Nov 3", "\u{E875}"),
    ]
}

fn sessions() -> Vec<SessionInfo> {
    let s = |id: &str, name: &str, host: &str, user: &str, group: &str, header: &str| {
        SessionInfo {
            id: id.into(),
            name: name.into(),
            host: host.into(),
            port: 22,
            user: user.into(),
            auth: "password".into(),
            last_used: "never".into(),
            group: group.into(),
            group_header: header.into(),
            ..Default::default()
        }
    };
    vec![
        s("local", "local_server", "192.168.100.41", "jeff", "local", "local"),
        s("pve", "pve", "192.168.100.2", "root", "local", ""),
        s("koko", "issue359-koko", "192.168.100.41", "issue359", "local", ""),
        s("istore", "iStoreOS", "192.168.100.1", "root", "local", ""),
        s("nas", "nas", "192.168.100.5", "admin", "nas", "nas"),
    ]
}

#[test]
fn recents_card_and_shelf_connect_remove_and_clear() {
    std::thread::spawn(|| {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Backend(window.clone()))).unwrap();
        let ui = RecentsFixture::new().unwrap();
        let model = Rc::new(slint::VecModel::from(recents()));
        ui.set_recents(model.clone().into());
        ui.set_sessions(Rc::new(slint::VecModel::from(sessions())).into());
        let events: Rc<RefCell<Vec<String>>> = Rc::default();
        {
            let events = events.clone();
            ui.on_connect(move |id| events.borrow_mut().push(format!("connect {id}")));
        }
        {
            let events = events.clone();
            let model = model.clone();
            ui.on_remove(move |id| {
                events.borrow_mut().push(format!("remove {id}"));
                if let Some(i) = (0..model.row_count()).find(|&i| model.row_data(i).unwrap().id == id) {
                    model.remove(i);
                }
            });
        }
        {
            let events = events.clone();
            ui.on_clear(move || events.borrow_mut().push("clear".into()));
        }
        ui.show().unwrap();
        window.set_size(slint::PhysicalSize::new(W, H));
        frame(&window);
        // Let the fade-in finish.
        std::thread::sleep(std::time::Duration::from_millis(300));
        frame(&window);

        // Card geometry, as laid out in app.slint: main area x 298..1100.
        let card_x = 298. + (802. - 380.) / 2.;
        let card_h = 10. + 22. + 4. + 2. + 6. * 48. + 5. * 2. + 8.;
        let card_y = (660f32 * 0.56).min(660. - card_h - 24.).max(24.);
        let first_row_y = card_y + 10. + 26. + 2. + 24.;

        snapshot(&window, "recents-card-dark-wallpaper.png");
        pointer(&window, card_x + 150., first_row_y);
        snapshot(&window, "recents-card-hover.png");

        click(&window, card_x + 150., first_row_y);
        assert_eq!(events.borrow().last().unwrap(), "connect pve");
        // × sits where the time was, at the row's right edge.
        click(&window, card_x + 380. - 8. - 9. - 11., first_row_y);
        assert_eq!(events.borrow().last().unwrap(), "remove pve");
        assert_eq!(model.row_count(), 5);

        ui.set_dark(false);
        frame(&window);
        snapshot(&window, "recents-card-light-wallpaper.png");
        ui.set_dark(true);
        ui.set_wallpaper(false);
        frame(&window);
        snapshot(&window, "recents-card-dark-plain.png");

        // Full welcome page (welcome-as-sidebar off).
        ui.set_wallpaper(true);
        ui.set_sidebar_mode(false);
        frame(&window);
        std::thread::sleep(std::time::Duration::from_millis(300));
        snapshot(&window, "recents-shelf-dark.png");
        ui.set_dark(false);
        frame(&window);
        snapshot(&window, "recents-shelf-light.png");

        // Header "Clear" sits at the right end of the shelf header.
        click(&window, 1100. - 24. - 16., 24. + 34. + 4. + 20. + 11.);
        assert_eq!(events.borrow().last().unwrap(), "clear", "{:?}", events.borrow());
    })
    .join()
    .unwrap();
}
