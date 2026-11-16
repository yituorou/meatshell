//! Exercise the production picker with more choices than fit in the window.
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Rgb8Pixel};
use std::rc::Rc;

#[path = "../src/app/jump_picker.rs"]
mod jump_picker;

slint::slint! {
    import { JumpCombo, JumpSearch } from "../ui/jump_combo.slint";
    import { Palette } from "std-widgets.slint";
    export { JumpSearch }
    export component PickerFixture inherits Window {
        width: 400px; height: 400px;
        init => { Palette.color-scheme = ColorScheme.dark; }
        in property <[string]> choices;
        in-out property <int> picked: 0;
        out property <int> selection-count: 0;
        JumpCombo {
            x: 20px; y: 20px; width: 360px; height: 60px;
            label: "Jump host";
            choices: root.choices;
            selected-index <=> root.picked;
            selected(index) => { root.selection-count += 1; }
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

fn render(window: &MinimalSoftwareWindow) {
    let mut pixels = vec![Rgb8Pixel::default(); 400 * 400];
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, 400);
    });
}

// Opt-in screenshots use the same fixture and synthetic labels as the test.
fn snapshot(window: &MinimalSoftwareWindow, name: &str) {
    let Ok(dir) = std::env::var("MEATSHELL_JUMP_PICKER_QA_DIR") else {
        return;
    };
    let mut pixels = vec![Rgb8Pixel::default(); 400 * 400];
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(&mut pixels, 400);
    });
    let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    std::fs::create_dir_all(&dir).unwrap();
    image::save_buffer(
        std::path::Path::new(&dir).join(name),
        &bytes,
        400,
        400,
        image::ColorType::Rgb8,
    )
    .unwrap();
}

fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    for pressed in [true, false] {
        let position = LogicalPosition::new(x, y);
        window.dispatch_event(if pressed {
            WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            }
        } else {
            WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            }
        });
    }
    render(window);
}

fn search(window: &MinimalSoftwareWindow, text: &str) {
    click(window, 100., 60.);
    for c in text.chars() {
        let text = c.to_string().into();
        window.dispatch_event(WindowEvent::KeyPressed { text });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: c.to_string().into(),
        });
    }
    render(window);
}

#[test]
fn long_picker_scrolls_and_search_keeps_original_session_indices() {
    std::thread::spawn(|| {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Backend(window.clone()))).unwrap();
        let ui = PickerFixture::new().unwrap();
        ui.global::<JumpSearch>()
            .on_matches(|label, query| jump_picker::matches(label.as_str(), query.as_str()));
        let mut choices = vec![slint::SharedString::from("None")];
        choices.extend((1..=100).map(|i| format!("Server-{i:03} (root@10.0.0.{i})").into()));
        ui.set_choices(Rc::new(slint::VecModel::from(choices)).into());
        ui.show().unwrap();
        window.set_size(slint::PhysicalSize::new(400, 400));
        render(&window);

        click(&window, 100., 60.);
        window.dispatch_event(WindowEvent::PointerScrolled {
            position: LogicalPosition::new(100., 240.),
            delta_x: 0.,
            delta_y: -10000.,
        });
        render(&window);
        snapshot(&window, "jump-host-picker-after.png");
        click(&window, 100., 348.);
        assert_eq!(
            ui.get_picked(),
            100,
            "the last session must be reachable by scrolling"
        );

        search(&window, "SERVER-099");
        snapshot(&window, "jump-host-picker-search.png");
        click(&window, 100., 160.);
        assert_eq!(
            ui.get_picked(),
            99,
            "filtering must retain the original session index"
        );

        search(&window, "10.0.0.42");
        click(&window, 100., 160.);
        assert_eq!(
            ui.get_picked(),
            42,
            "host search must work and clear the previous query"
        );

        search(&window, "no-match");
        click(&window, 100., 132.);
        assert_eq!(
            ui.get_picked(),
            0,
            "the clear choice remains available with no matches"
        );
        assert_eq!(ui.get_selection_count(), 4);
    })
    .join()
    .unwrap();
}
