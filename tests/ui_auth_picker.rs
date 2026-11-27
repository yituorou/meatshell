//! Where the terminal's password picker lands, and that it paints at all.
//!
//! Same reasoning as `ui_hint_popup.rs`: a popup that renders nothing — or
//! renders into the wrong parent — still compiles, and a Slint element outside
//! a layout defaults to 0×0 without a word. So the production `TerminalView` is
//! rendered head-lessly and the pixels are measured, with the frames also saved
//! as PNG when `MEATSHELL_AUTH_QA_DIR` is set.
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ModelRc, Rgb8Pixel, SharedString, VecModel};
use std::rc::Rc;

slint::slint! {
    import { TerminalView, TermSpan, CredChoice } from "../ui/terminal_view.slint";
    import { Theme } from "../ui/theme.slint";
    import "../ui/fonts/MeatshellMono-Regular.ttf";

    export component AuthFixture inherits Window {
        width: 760px;
        height: 460px;
        background: Theme.term-bg;
        in property <[TermSpan]> spans;
        in property <bool> auth-prompt;
        in property <[CredChoice]> cred-choices;
        in property <int> rows: 6;
        in-out property <string> used-id;
        TerminalView {
            spans: root.spans;
            rows-used: root.rows;
            cursor-row: 1;
            cursor-col: 9;
            term-font-size-px: 14.0;
            // A colour the terminal never paints, so the cursor cell can be
            // located exactly.
            cursor-color: #ff00ff;
            auth-prompt: root.auth-prompt;
            cred-choices: root.cred-choices;
            use-credential(id) => { root.used-id = id; }
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

const W: usize = 760;
const H: usize = 460;

type Rect = (usize, usize, usize, usize); // left, top, right, bottom

fn span(text: &str, row: i32, col: i32) -> TermSpan {
    TermSpan {
        text: text.into(),
        fg: slint::Color::from_rgb_u8(220, 220, 220),
        bg: slint::Color::from_argb_u8(0, 0, 0, 0),
        bold: false,
        underline: false,
        row,
        col,
        cells: text.chars().count() as i32,
        cjk: false,
        font: "".into(),
        emoji: false,
        emoji_image: slint::Image::default(),
    }
}

fn choice(id: &str, name: &str, summary: &str) -> CredChoice {
    CredChoice {
        id: SharedString::from(id),
        name: SharedString::from(name),
        summary: SharedString::from(summary),
    }
}

fn grown(found: Option<Rect>, x: usize, y: usize) -> Option<Rect> {
    Some(match found {
        None => (x, y, x, y),
        Some((l, t, r, b)) => (l.min(x), t.min(y), r.max(x), b.max(y)),
    })
}

fn bbox_where(pixels: &[Rgb8Pixel], hits: impl Fn(&Rgb8Pixel) -> bool) -> Option<Rect> {
    let mut found = None;
    for y in 0..H {
        for x in 0..W {
            if hits(&pixels[y * W + x]) {
                found = grown(found, x, y);
            }
        }
    }
    found
}

fn diff_bbox(a: &[Rgb8Pixel], b: &[Rgb8Pixel]) -> Option<Rect> {
    let mut found = None;
    for y in 0..H {
        for x in 0..W {
            let (p, q) = (&b[y * W + x], &a[y * W + x]);
            if p.r != q.r || p.g != q.g || p.b != q.b {
                found = grown(found, x, y);
            }
        }
    }
    found
}

/// Synthetic left click at a physical pixel, the way a real pointer arrives.
fn click(window: &MinimalSoftwareWindow, x: usize, y: usize) {
    let position = slint::LogicalPosition::new(x as f32, y as f32);
    let target = window.window();
    target.dispatch_event(WindowEvent::PointerMoved { position });
    target.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    target.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

fn render(window: &MinimalSoftwareWindow) -> Vec<Rgb8Pixel> {
    let mut pixels = vec![Rgb8Pixel::default(); W * H];
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    window.draw_if_needed(|r| {
        let _ = r.render(&mut pixels, W);
    });
    pixels
}

/// Write a frame for eyeballing; a no-op unless the caller asked for it.
fn save_png(name: &str, pixels: &[Rgb8Pixel]) {
    let Ok(dir) = std::env::var("MEATSHELL_AUTH_QA_DIR") else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let mut rgb = Vec::with_capacity(W * H * 3);
    for pixel in pixels {
        rgb.extend_from_slice(&[pixel.r, pixel.g, pixel.b]);
    }
    let _ = image::save_buffer(
        std::path::Path::new(&dir).join(name),
        &rgb,
        W as u32,
        H as u32,
        image::ColorType::Rgb8,
    );
}

const MAGENTA: Rgb8Pixel = Rgb8Pixel {
    r: 255,
    g: 0,
    b: 255,
};

fn fixture() -> (Rc<MinimalSoftwareWindow>, AuthFixture) {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Backend(window.clone()))).unwrap();
    let ui = AuthFixture::new().unwrap();
    window.set_size(slint::PhysicalSize::new(W as u32, H as u32));
    // A real `sudo` screen: the prompt, then the cursor waiting for input.
    let mut parser = vt100::Parser::new(
        std::num::NonZeroU16::new(6).unwrap(),
        std::num::NonZeroU16::new(80).unwrap(),
        0,
    );
    parser.process(b"[sudo] password for far: ");
    let (row, _col) = parser.screen().cursor_position();
    assert_eq!(row, 0, "the prompt sits on the first row");
    let spans = vec![span("[sudo] password for far:", 0, 0)];
    ui.set_spans(ModelRc::from(Rc::new(VecModel::from(spans))));
    (window, ui)
}

#[test]
fn picker_paints_under_the_prompt_and_lists_the_credentials() {
    let (window, ui) = fixture();
    let quiet = render(&window);
    let (cx, cy, _cr, cb) = bbox_where(&quiet, |p| *p == MAGENTA).expect("the cursor cell is visible");
    let cell_h = cb - cy + 1;

    let rows = vec![
        choice("c1", "核心交换机", "admin · 密码"),
        choice("c2", "GitHub", "a-noble · 密码"),
    ];
    ui.set_cred_choices(ModelRc::from(Rc::new(VecModel::from(rows))));
    ui.set_auth_prompt(true);
    let open = render(&window);
    save_png("auth_picker.png", &open);
    let (l, t, r, b) = diff_bbox(&quiet, &open).expect("the picker must paint something");
    println!("picker bbox ({l},{t})-({r},{b}), cursor at ({cx},{cy}) cell {cell_h}");

    assert!(
        t >= cy + cell_h - 1,
        "the picker opens below the prompt line (cursor top {cy}, cell {cell_h}, picker top {t})"
    );
    assert!(
        l >= cx - 2,
        "the picker is anchored at the cursor column (cursor x {cx}, picker left {l})"
    );
    assert!(
        r - l >= 200 && b - t >= 90,
        "two rows plus the header must be a real popup: {}x{}",
        r - l,
        b - t
    );

    // Closing it again leaves the grid as it was — the popup is an overlay, not
    // a layout change. (The cursor blink phase can differ between frames, so
    // compare against the popup's own footprint rather than demanding equality.)
    ui.set_auth_prompt(false);
    let closed = render(&window);
    let leftover = diff_bbox(&quiet, &closed).map(|(l, t, r, b)| (r - l, b - t));
    println!("leftover after close: {leftover:?}");
    assert!(
        leftover.map(|(w, h)| w < 20 && h < 20).unwrap_or(true),
        "closing the picker must leave no rectangle behind: {leftover:?}"
    );
}

#[test]
fn a_click_on_a_row_hands_its_id_to_the_picker() {
    let (window, ui) = fixture();
    let quiet = render(&window);
    ui.set_cred_choices(ModelRc::from(Rc::new(VecModel::from(vec![
        choice("c1", "核心交换机", "admin · 密码"),
        choice("c2", "GitHub", "a-noble · 密码"),
    ]))));
    ui.set_auth_prompt(true);
    let open = render(&window);
    let (l, t, _r, _b) = diff_bbox(&quiet, &open).expect("the picker paints");
    // Header row (20px + 4px padding) then the first credential row (34px).
    let (x, y) = (l + 30, t + 30);
    println!("clicking first row at ({x},{y}), picker at ({l},{t})");
    click(&window, x, y);
    assert_eq!(
        ui.get_used_id().as_str(),
        "c1",
        "a click on the first row must select that credential (x={x}, y={y})"
    );
}

#[test]
fn an_empty_credential_list_still_explains_where_to_add_one() {
    let (window, ui) = fixture();
    let quiet = render(&window);
    ui.set_auth_prompt(true);
    let empty = render(&window);
    save_png("auth_picker_empty.png", &empty);
    let (l, t, r, b) = diff_bbox(&quiet, &empty).expect("the empty state must paint too");
    assert!(
        r - l >= 100 && b - t >= 24,
        "the empty state is one readable row: {}x{}",
        r - l,
        b - t
    );
}
