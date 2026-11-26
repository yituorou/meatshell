//! Where the shell-prompt completion popup lands (#583 follow-up).
//!
//! A wrong popup position is the kind of bug that only shows up on screen, so
//! this renders the production `TerminalView` and measures pixels instead of
//! trusting the bindings: the list must start on the line below the cursor, and
//! flip to the lines above it when the grid has no room underneath.
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::{ModelRc, Rgb8Pixel, SharedString, VecModel};
use std::rc::Rc;

slint::slint! {
    import { TerminalView, TermSpan } from "../ui/terminal_view.slint";
    import { Theme } from "../ui/theme.slint";
    import "../ui/fonts/MeatshellMono-Regular.ttf";

    export component HintFixture inherits Window {
        width: 760px;
        height: 460px;
        background: Theme.term-bg;
        in property <[TermSpan]> spans;
        in property <[string]> hint-items;
        in property <int> hint-row: -1;
        in property <int> hint-col: 0;
        in property <int> rows: 6;
        TerminalView {
            spans: root.spans;
            hint-items: root.hint-items;
            hint-row: root.hint-row;
            hint-col: root.hint-col;
            rows-used: root.rows;
            cursor-row: root.hint-row;
            cursor-col: root.hint-col;
            term-font-size-px: 14.0;
            // A colour the terminal never paints, so the cursor cell can be
            // located exactly and the cell metrics derived from it.
            cursor-color: #ff00ff;
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

type Rect = (usize, usize, usize, usize); // left, top, right, bottom

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

fn render(window: &MinimalSoftwareWindow) -> Vec<Rgb8Pixel> {
    let mut pixels = vec![Rgb8Pixel::default(); W * H];
    slint::platform::update_timers_and_animations();
    window.request_redraw();
    window.draw_if_needed(|r| {
        let _ = r.render(&mut pixels, W);
    });
    pixels
}

const MAGENTA: Rgb8Pixel = Rgb8Pixel {
    r: 255,
    g: 0,
    b: 255,
};

#[test]
fn popup_lands_under_the_cursor_and_flips_when_the_grid_is_full() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Backend(window.clone()))).unwrap();
    let ui = HintFixture::new().unwrap();
    window.set_size(slint::PhysicalSize::new(W as u32, H as u32));

    // A real screen, exactly as the shell leaves it after `cle` is typed: the
    // cursor position comes from vt100, not from a hardcoded number.
    let mut parser = vt100::Parser::new(
        std::num::NonZeroU16::new(6).unwrap(),
        std::num::NonZeroU16::new(80).unwrap(),
        0,
    );
    parser.process("╭─[far@MacBook-Air]──[~/projects]──[11:19:13]\r\n╰─❯ cle".as_bytes());
    let (cursor_row, cursor_col) = parser.screen().cursor_position();

    let spans = vec![
        span("╭─[far@MacBook-Air]──[~/projects]──[11:19:13]", 0, 0),
        span("╰─❯ cle", 1, 0),
    ];
    ui.set_spans(ModelRc::from(Rc::new(VecModel::from(spans))));
    ui.set_hint_row(cursor_row as i32);
    ui.set_hint_col(cursor_col as i32);

    // 1. No popup: the only magenta on screen is the cursor cell itself.
    let quiet = render(&window);
    let (cx, cy, cr, cb) = bbox_where(&quiet, |p| *p == MAGENTA).expect("the cursor cell is visible");
    let cell_w = cr - cx + 1;
    let cell_h = cb - cy + 1;
    println!("cursor cell at ({cx},{cy}) size {cell_w}x{cell_h}");

    // 2. Popup open under the cursor: only the list area may change.
    // What `prompt_hint` returns for `cle` (covered by its own unit tests).
    let items: Vec<SharedString> = ["clear", "clearer", "clears"]
        .iter()
        .map(|s| SharedString::from(*s))
        .collect();
    assert!(items.len() >= 2, "the fixture needs a real candidate list");
    ui.set_hint_items(ModelRc::from(Rc::new(VecModel::from(items.clone()))));
    let open = render(&window);
    let (l, t, r, b) = diff_bbox(&quiet, &open).expect("the popup must paint something");
    println!("popup bbox ({l},{t})-({r},{b})");

    assert!(
        t >= cy + cell_h - 1,
        "the list starts below the cursor row (cursor top {cy}, cell {cell_h}, list top {t})"
    );
    assert!(
        l + 1 >= cx,
        "the list is anchored at the cursor column (cursor x {cx}, list left {l})"
    );
    assert!(
        r - l > 100 && b - t > 60,
        "the list is big enough to be a real popup: {}x{}",
        r - l,
        b - t
    );

    // 3. Cursor on the last row of a full grid: the list has to flip above.
    ui.set_rows(40);
    ui.set_hint_row(39);
    ui.set_hint_items(ModelRc::from(Rc::new(VecModel::<SharedString>::default())));
    let quiet_bottom = render(&window);
    let (_, cursor_top) = bbox_where(&quiet_bottom, |p| *p == MAGENTA)
        .map(|r| (r.1, r.1))
        .expect("the cursor is on the last row");
    ui.set_hint_items(ModelRc::from(Rc::new(VecModel::from(items))));
    let open_bottom = render(&window);
    let (_, t2, _, b2) = diff_bbox(&quiet_bottom, &open_bottom).expect("the popup must paint something");
    println!("full-grid popup top {t2} bottom {b2}, cursor top {cursor_top}");
    assert!(
        t2 + 1 < cursor_top,
        "with no room below, the list flips above the cursor (list top {t2}, cursor top {cursor_top})"
    );
    assert!(
        b2 <= cursor_top + 1,
        "the flipped list ends at the cursor row, not over it (list bottom {b2}, cursor top {cursor_top})"
    );

    // Eye-check renders, written only when asked.
    if let Ok(dir) = std::env::var("MEATSHELL_HINT_QA_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        for (name, pixels) in [("below.png", &open), ("above.png", &open_bottom)] {
            let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::save_buffer(
                std::path::Path::new(&dir).join(name),
                &bytes,
                W as u32,
                H as u32,
                image::ColorType::Rgb8,
            )
            .unwrap();
        }
    }
}
