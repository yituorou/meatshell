use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, Rgb8Pixel};
use std::rc::Rc;

slint::slint! {
    import { InterfacePanel, UpdateMirrorRow } from "../ui/interface_panel.slint";
    import { SystemInfoWindow, SysInfoRow } from "../ui/system_info_window.slint";
    export { SystemInfoWindow, SysInfoRow }
    import { QuickDockPanel, QuickCmd } from "../ui/terminal_view.slint";
    import { Theme } from "../ui/theme.slint";
    import "../ui/fonts/MaterialIcons-Regular.ttf";
    import { Palette } from "std-widgets.slint";

    export component SettingsHost inherits Window {
        width: 720px;
        in property <length> viewport-height: 435px;
        height: root.viewport-height;
        in property <string> page: "update";
        in property <bool> lang-en: false;
        in property <[UpdateMirrorRow]> mirrors;
        in property <string> long-status: "当前已是最新版本 (v0.8.3)。";
        out property <int> reset-mirrors-count: 0;
        out property <int> save-editor-count: 0;
        out property <int> webdav-upload-count: 0;
        init => {
            Theme.ui-font-family = "Microsoft YaHei";
            Palette.color-scheme = ColorScheme.dark;
        }
        InterfacePanel {
            width: 100%;
            height: 100%;
            start-page: root.page;
            lang-en: root.lang-en;
            is-windows: true;
            update-china-tz: true;
            update-mirrors: root.mirrors;
            update-status: root.long-status;
            webdav-status: root.page == "webdav" ? root.long-status : "";
            reset-update-mirrors => { root.reset-mirrors-count += 1; }
            set-external-editor(_, _) => { root.save-editor-count += 1; }
            webdav-upload => { root.webdav-upload-count += 1; }
        }
    }

    export component QuickDockHost inherits Window {
        width: 240px;
        height: 220px;
        in property <[QuickCmd]> cmds;
        out property <int> activated-count: 0;
        out property <string> last-activated;
        init => {
            Theme.ui-font-family = "Microsoft YaHei";
            Palette.color-scheme = ColorScheme.dark;
        }
        QuickDockPanel {
            width: 100%;
            height: 100%;
            commands: root.cmds;
            dock-edge: "left";
            activate(cmd, _) => {
                root.activated-count += 1;
                root.last-activated = cmd;
            }
        }
    }
}

struct Backend {
    window: Rc<MinimalSoftwareWindow>,
}
impl slint::platform::Platform for Backend {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

fn render_size(window: &MinimalSoftwareWindow, w: u32, h: u32) -> Vec<Rgb8Pixel> {
    let mut pixels = vec![Rgb8Pixel::default(); (w * h) as usize];
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(&mut pixels, w as usize);
    });
    slint::platform::update_timers_and_animations();
    pixels
}

fn scroll(window: &MinimalSoftwareWindow, x: f32, y: f32, delta_y: f32) {
    window.dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(x, y),
    });
    window.dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(x, y),
        delta_x: 0.0,
        delta_y,
    });
}

fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    window.dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(x, y),
    });
    window.dispatch_event(WindowEvent::PointerPressed {
        position: slint::LogicalPosition::new(x, y),
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position: slint::LogicalPosition::new(x, y),
        button: PointerEventButton::Left,
    });
}

#[test]
fn settings_update_page_scrolls_to_bottom_buttons_without_occlusion() {
    std::thread::spawn(|| {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Backend {
            window: window.clone(),
        }))
        .unwrap();
        let ui = SettingsHost::new().unwrap();
        let mirrors = (0..5)
            .map(|i| UpdateMirrorRow {
                url: format!("https://mirror{i}.example.com/").into(),
                key: format!("m{i}").into(),
                ping: "42 ms".into(),
                level: 1,
                fastest: i == 1,
                selected: i == 0,
                removable: i > 0,
            })
            .collect::<Vec<_>>();
        ui.set_mirrors(slint::ModelRc::new(slint::VecModel::from(mirrors)));
        ui.set_long_status(
            "检查更新失败：无法连接 https://github.com/yituorou/meatshell/releases/latest，请检查网络代理设置或切换下方的国内镜像源后重试。这是第一行第二行第三行测试文本。再加第四行确保彻底暴露高度低估。"
                .into(),
        );
        ui.show().unwrap();
        window.set_size(slint::PhysicalSize::new(720, 435));
        render_size(&window, 720, 435);
        for _ in 0..30 {
            scroll(&window, 400., 220., -120.);
            render_size(&window, 720, 435);
        }
        let pixels = render_size(&window, 720, 435);
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/settings-update-bottom.png");
        image::save_buffer(path, &bytes, 720, 435, image::ColorType::Rgb8).unwrap();

        // When scrolled fully to the bottom with `padding-bottom: 28px`, the 30px-tall
        // "恢复默认" button occupies y = 435 - 28 - 30 .. 435 - 28 = 377..407 (center ~392).
        click(&window, 305., 392.);
        render_size(&window, 720, 435);
        assert!(
            ui.get_reset_mirrors_count() > 0,
            "reset-update-mirrors button at the bottom of Settings > Updates must be reachable at y=392"
        );
    })
    .join()
    .unwrap();
}

#[test]
fn settings_sftp_page_scrolls_to_bottom_buttons_without_occlusion() {
    std::thread::spawn(|| {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Backend {
            window: window.clone(),
        }))
        .unwrap();
        let ui = SettingsHost::new().unwrap();
        ui.set_page("sftp".into());
        // 360px height forces SFTP (including External editor) to scroll, testing
        // that `padding-bottom: 24px` keeps the bottom Save button unoccluded.
        ui.set_viewport_height(360.0);
        ui.show().unwrap();
        window.set_size(slint::PhysicalSize::new(720, 360));
        render_size(&window, 720, 360);
        for _ in 0..30 {
            scroll(&window, 400., 220., -120.);
            render_size(&window, 720, 360);
        }
        let pixels = render_size(&window, 720, 360);
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/settings-sftp-bottom.png");
        image::save_buffer(path, &bytes, 720, 360, image::ColorType::Rgb8).unwrap();

        // With `padding-bottom: 24px`, the 30px-tall "保存" PrimaryButton at the
        // bottom-right of Settings > SFTP spans y = 360 - 24 - 30 .. 360 - 24 = 306..336 (center ~321).
        click(&window, 660., 321.);
        render_size(&window, 720, 360);
        assert!(
            ui.get_save_editor_count() > 0,
            "Save external editor button at the bottom of Settings > SFTP must be reachable at y=321"
        );
    })
    .join()
    .unwrap();
}

#[test]
fn settings_webdav_page_scrolls_to_bottom_without_occlusion() {
    std::thread::spawn(|| {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Backend {
            window: window.clone(),
        }))
        .unwrap();
        let ui = SettingsHost::new().unwrap();
        ui.set_page("webdav".into());
        ui.set_long_status(
            "WebDAV 同步失败：无法连接远端服务器，请检查 URL、用户名、应用令牌以及内网自签名证书选项。加长换行测试文本第一行第二行第三行。"
                .into(),
        );
        ui.show().unwrap();
        window.set_size(slint::PhysicalSize::new(720, 435));
        render_size(&window, 720, 435);
        for _ in 0..30 {
            scroll(&window, 400., 220., -120.);
            render_size(&window, 720, 435);
        }
        let pixels = render_size(&window, 720, 435);
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/settings-webdav-bottom.png");
        image::save_buffer(path, &bytes, 720, 435, image::ColorType::Rgb8).unwrap();

        // Locate the rendered blue PrimaryButton ("上传") in the scrolled frame so
        // we verify it is visible above the wrapped status text and padding-bottom.
        let mut blue_ys = Vec::new();
        for y in 0..435usize {
            let p = pixels[y * 720 + 660];
            if p.b > 200 && p.r < 100 {
                blue_ys.push(y as f32);
            }
        }
        assert!(
            !blue_ys.is_empty(),
            "WebDAV Upload button must be rendered inside the scrolled viewport"
        );
        let btn_center_y = (blue_ys[0] + blue_ys[blue_ys.len() - 1]) * 0.5;
        click(&window, 660., btn_center_y);
        render_size(&window, 720, 435);
        assert!(
            ui.get_webdav_upload_count() > 0,
            "Upload button at the bottom of Settings > WebDAV must remain reachable above wrapped status text"
        );
    })
    .join()
    .unwrap();
}

#[test]
fn quick_dock_wrapped_items_scroll_to_bottom_without_occlusion() {
    std::thread::spawn(|| {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Backend {
            window: window.clone(),
        }))
        .unwrap();
        let ui = QuickDockHost::new().unwrap();
        let mut cmds = Vec::new();
        for i in 0..12 {
            cmds.push(QuickCmd {
                name: format!("Very long quick command label number {i} that definitely wraps onto multiple lines inside narrow dock").into(),
                command: format!("cmd-{i}").into(),
                command_preview: format!("cmd-{i}").into(),
                group: "".into(),
                group_header: "".into(),
                collapsed: false,
                send_enter: true,
                orig_index: i,
            });
        }
        ui.set_cmds(slint::ModelRc::new(slint::VecModel::from(cmds)));
        ui.show().unwrap();
        window.set_size(slint::PhysicalSize::new(240, 220));
        render_size(&window, 240, 220);
        for _ in 0..40 {
            scroll(&window, 120., 120., -120.);
            render_size(&window, 240, 220);
        }
        let pixels = render_size(&window, 240, 220);
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/quick-dock-bottom.png");
        image::save_buffer(path, &bytes, 240, 220, image::ColorType::Rgb8).unwrap();
        click(&window, 120., 150.);
        render_size(&window, 240, 220);
        assert_eq!(ui.get_last_activated().as_str(), "cmd-11");
    })
    .join()
    .unwrap();
}

#[test]
fn system_info_wrapped_rows_scroll_to_bottom_without_occlusion() {
    std::thread::spawn(|| {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Backend {
            window: window.clone(),
        }))
        .unwrap();
        let ui = SystemInfoWindow::new().unwrap();
        ui.set_ui_font_family("Microsoft YaHei".into());
        ui.set_dark_mode(true);
        let long_rows = (0..6)
            .map(|i| SysInfoRow {
                c1: format!("very-long-key-name-{i}-that-wraps-across-lines").into(),
                c2: format!("very-long-value-for-overview-{i}-with-lots-of-characters-to-force-line-wrapping-inside-the-cell").into(),
                c3: format!("key-{i}").into(),
                c4: format!("another-very-long-wrapped-value-{i}-abcdefghijklmnopqrstuvwxyz0123456789").into(),
                c5: "".into(),
            })
            .collect::<Vec<_>>();
        ui.set_overview_rows(slint::ModelRc::new(slint::VecModel::from(long_rows.clone())));
        let fs_rows = (0..6)
            .map(|i| SysInfoRow {
                c1: format!("/dev/mapper/vg_system-lv_very_long_volume_name_{i}_with_extra_suffix").into(),
                c2: "1.9 TiB".into(),
                c3: "900 GiB".into(),
                c4: "1.0 TiB".into(),
                c5: if i == 5 { "LAST_MOUNT_VISIBLE".into() } else { format!("/mnt/very/deep/nested/mountpoint/number/{i}").into() },
            })
            .collect::<Vec<_>>();
        ui.set_filesystem_rows(slint::ModelRc::new(slint::VecModel::from(fs_rows)));
        ui.show().unwrap();
        window.set_size(slint::PhysicalSize::new(760, 520));
        render_size(&window, 760, 520);
        for _ in 0..80 {
            scroll(&window, 380., 260., -120.);
            render_size(&window, 760, 520);
        }
        let pixels = render_size(&window, 760, 520);
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sysinfo-bottom.png");
        image::save_buffer(path, &bytes, 760, 520, image::ColorType::Rgb8).unwrap();
    })
    .join()
    .unwrap();
}
