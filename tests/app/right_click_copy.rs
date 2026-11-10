//! Optional visual review of the new setting, without a desktop display.
use super::*;

#[test]
#[ignore = "writes docs/screenshots/right-click-copy.png for visual review"]
fn right_click_copy_settings_preview() {
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::{ComponentHandle, Rgb8Pixel};

    struct Backend(Rc<MinimalSoftwareWindow>);
    impl slint::platform::Platform for Backend {
        fn create_window_adapter(
            &self,
        ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }
    std::thread::spawn(|| {
        let (width, height) = (1100, 760);
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Backend(window.clone()))).unwrap();
        let ui = AppWindow::new().unwrap();
        ui.set_lang_en(true);
        ui.set_interface_start_page("input".into());
        ui.set_right_click_copy(true);
        ui.show().unwrap();
        window.set_size(slint::PhysicalSize::new(width, height));
        ui.set_interface_open(true);
        for _ in 0..8 {
            std::thread::sleep(std::time::Duration::from_millis(60));
            slint::platform::update_timers_and_animations();
        }
        let mut pixels = vec![Rgb8Pixel::default(); (width * height) as usize];
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, width as usize);
        });
        let bytes: Vec<u8> = pixels
            .iter()
            .flat_map(|pixel| [pixel.r, pixel.g, pixel.b])
            .collect();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/screenshots/right-click-copy.png");
        image::save_buffer(path, &bytes, width, height, image::ColorType::Rgb8).unwrap();
    })
    .join()
    .unwrap();
}
