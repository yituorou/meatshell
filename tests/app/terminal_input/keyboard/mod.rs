use super::super::*;

mod anchor_focus;

#[test]
fn windows_process_key_ctrl_release_keeps_physical_side() {
    use i_slint_backend_winit::winit::event::ElementState;
    use i_slint_backend_winit::winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

    let process = Key::Named(NamedKey::Process);
    assert_eq!(
        windows_process_ctrl_release(
            ElementState::Released,
            &process,
            &PhysicalKey::Code(KeyCode::ControlLeft),
        ),
        Some(CtrlKeySide::Left)
    );
    assert_eq!(
        windows_process_ctrl_release(
            ElementState::Released,
            &process,
            &PhysicalKey::Code(KeyCode::ControlRight),
        ),
        Some(CtrlKeySide::Right)
    );
}

#[test]
fn windows_process_key_recovery_ignores_other_key_events() {
    use i_slint_backend_winit::winit::event::ElementState;
    use i_slint_backend_winit::winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

    let process = Key::Named(NamedKey::Process);
    let left_ctrl = PhysicalKey::Code(KeyCode::ControlLeft);
    assert_eq!(
        windows_process_ctrl_release(ElementState::Pressed, &process, &left_ctrl),
        None
    );
    assert_eq!(
        windows_process_ctrl_release(
            ElementState::Released,
            &Key::Named(NamedKey::Control),
            &left_ctrl,
        ),
        None
    );
    assert_eq!(
        windows_process_ctrl_release(
            ElementState::Released,
            &process,
            &PhysicalKey::Code(KeyCode::KeyC),
        ),
        None
    );
}

#[test]
fn bare_alt_is_not_forwarded() {
    assert_eq!(
        key_to_pty_bytes("\u{0012}", false, true, false),
        Vec::<u8>::new()
    );
}

#[test]
fn home_and_end_follow_application_cursor_mode() {
    assert_eq!(key_to_pty_bytes("\u{F729}", false, false, false), b"\x1b[H");
    assert_eq!(key_to_pty_bytes("\u{F72B}", false, false, false), b"\x1b[F");
    assert_eq!(key_to_pty_bytes("\u{F729}", false, false, true), b"\x1bOH");
    assert_eq!(key_to_pty_bytes("\u{F72B}", false, false, true), b"\x1bOF");
}

#[test]
fn bare_modifier_codes_are_dropped() {
    for cp in 0x10u32..=0x18 {
        let s = char::from_u32(cp).unwrap().to_string();
        assert_eq!(
            key_to_pty_bytes(&s, false, false, false),
            Vec::<u8>::new(),
            "code point {:#04x} should be dropped",
            cp
        );
    }
}

#[test]
fn ctrl_letter_c0_still_passes() {
    assert!(is_terminal_interrupt("\u{0003}"));
    assert!(!is_terminal_interrupt("c"));
    assert_eq!(key_to_pty_bytes("\u{0003}", true, false, false), vec![0x03]);
    // Some Linux backends have already translated Ctrl+C to ETX by the time
    // the modifier snapshot is taken, so ctrl=false must still preserve it.
    assert_eq!(
        key_to_pty_bytes("\u{0003}", false, false, false),
        vec![0x03]
    );
    assert_eq!(key_to_pty_bytes("\u{0012}", true, false, false), vec![0x12]);
    assert_eq!(key_to_pty_bytes("\u{0018}", true, false, false), vec![0x18]);
}

#[test]
fn platform_bare_ctrl_markers_do_not_reach_nano() {
    assert!(should_drop_bare_ctrl_marker("\u{0011}", true, true));
    assert!(should_drop_bare_ctrl_marker("\u{0016}", true, true));
    assert!(!should_drop_bare_ctrl_marker("\u{0017}", true, true));
    assert!(!should_drop_bare_ctrl_marker("\u{0011}", true, false));
    assert!(!should_drop_bare_ctrl_marker("x", true, true));
    assert_eq!(key_to_pty_bytes("q", true, false, false), vec![0x11]);
    assert_eq!(key_to_pty_bytes("v", true, false, false), vec![0x16]);
    assert_eq!(key_to_pty_bytes("x", true, false, false), vec![0x18]);
}

#[test]
fn macos_ctrl_w_still_comes_from_the_final_printable_key() {
    // The affected device repeats bare Control as U+0017. A real Ctrl+W is
    // still generated from the chord's final printable W event.
    assert!(should_drop_macos_bare_ctrl_marker("\u{0017}", true, true));
    assert!(!should_drop_macos_bare_ctrl_marker("\u{0017}", true, false));
    assert!(!should_drop_macos_bare_ctrl_marker("\u{0017}", false, true));
    assert_eq!(key_to_pty_bytes("w", true, false, false), vec![0x17]);
}

#[test]
fn macos_ime_bare_ctrl_backspace_marker_is_platform_scoped() {
    assert!(should_drop_macos_bare_ctrl_marker("\u{0008}", true, true));
    assert!(!should_drop_macos_bare_ctrl_marker("\u{0008}", true, false));
    assert!(!should_drop_macos_bare_ctrl_marker("\u{0008}", false, true));
    // A genuine Ctrl+H still arrives through the final printable letter and is
    // encoded to the same control byte at the PTY boundary.
    assert_eq!(key_to_pty_bytes("h", true, false, false), vec![0x08]);
}

#[test]
fn alt_letter_still_sends_esc_prefix() {
    assert_eq!(key_to_pty_bytes("a", false, true, false), vec![0x1b, b'a']);
}
