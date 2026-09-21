//! Regression test for #452: the hidden keyboard/IME anchor must stay
//! geometrically inside the terminal viewport while the view is scrolled back.
//!
//! Slint drops the focus of a focused item whose geometry no longer intersects
//! its clip rect (`ItemRc::is_visible` is purely geometric, checked at the top
//! of `WindowInner::process_key_input`). ui/terminal_view.slint used to park
//! this anchor at -1000px in that state, so the first key press after
//! scrolling back cleared the focus and every later key was swallowed — the
//! terminal looked completely unresponsive and Enter could never scroll back to
//! the bottom.
//!
//! The probe mirrors the real hierarchy: a hidden 1x1 TextInput that is a child
//! of the key-capture scope and declared *before* the clipped viewport, so the
//! viewport's pointer layer sits on top of it and it can never be hovered. It
//! parks on the viewport's first cell instead of off-window.

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::ComponentHandle;
use std::rc::Rc;

slint::slint! {
    export component AnchorProbe inherits Window {
        width: 320px;
        height: 200px;
        in property <bool> scrolled: false;
        in-out property <string> last-key: "";
        in-out property <bool> anchor-focused: false;
        // The viewport must start at the scope's origin: the absolutely
        // positioned anchor must not take part in any layout.
        out property <length> viewport-top: viewport.y;

        public function focus-anchor() { anchor.focus(); }

        scope := FocusScope {
            x: 0px;
            y: 0px;
            width: root.width;
            height: root.height;

            // Hidden keyboard/IME anchor — declared before the viewport, so
            // every pointer layer of the terminal stays above it.
            anchor := TextInput {
                // Live caret: a cell inside the viewport. Scrolled back: the
                // viewport's first cell. Never off-window: parking it outside
                // the clip made Slint drop its focus and swallow every key.
                x: 10px;
                y: root.scrolled ? 8px : 40px;
                width: 1px;
                height: 1px;
                opacity: 0;
                text-cursor-width: 0px;
                single-line: true;

                changed has-focus => { root.anchor-focused = self.has-focus; }

                key-pressed(e) => {
                    root.last-key = e.text;
                    accept
                }
            }

            VerticalLayout {
                spacing: 0;

                viewport := Rectangle {
                    vertical-stretch: 1;
                    clip: true;

                    // The terminal's pointer layer (body-touch-overlay):
                    // declared after the anchor, so it owns every hit test.
                    TouchArea {
                        x: 0px;
                        y: 0px;
                        width: parent.width;
                        height: parent.height;
                        mouse-cursor: text;
                        clicked => { anchor.focus(); }
                    }
                }
            }
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

#[test]
fn parked_keyboard_anchor_keeps_focus_and_keys_when_scrolled_back() {
    // Slint contexts are thread-local; keep this window out of the app's.
    std::thread::spawn(|| {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Backend(window.clone()))).unwrap();
        let ui = AnchorProbe::new().unwrap();
        ui.show().unwrap();
        window.set_size(slint::PhysicalSize::new(320, 200));

        let press = |text: &str| {
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.into() });
        };

        ui.invoke_focus_anchor();
        press("a");
        assert_eq!(ui.get_last_key(), "a", "keys must reach the anchor");
        assert!(ui.get_anchor_focused());

        // Scroll back: the live caret is gone, the anchor parks inside the
        // viewport and must keep both the focus and the keystrokes.
        ui.set_scrolled(true);
        press("\n");
        assert_eq!(
            ui.get_last_key(),
            "\n",
            "key swallowed while scrolled back (#452)"
        );
        assert!(
            ui.get_anchor_focused(),
            "focus dropped while scrolled back (#452)"
        );

        assert_eq!(ui.get_viewport_top(), 0.0);
    })
    .join()
    .unwrap();
}
