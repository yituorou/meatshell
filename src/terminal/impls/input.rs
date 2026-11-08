use crate::terminal::TermBuffers;
#[cfg(any(target_os = "windows", test))]
use super::state::CtrlKeySide;

/// Normalize clipboard line endings to the single CR byte expected for Enter
/// by a terminal, including inside bracketed-paste payloads.
pub(crate) fn normalize_pasted_newlines(text: &str) -> String {
    text.replace("\r\n", "\r").replace('\n', "\r")
}

/// Encode a terminal mouse event for the PTY using the encoding the remote
/// application requested (X10 / SGR) — so btop, htop, mc and other mouse-aware
/// TUI apps get click/drag/wheel events (see #terminal-mouse).
///
/// `btn` follows the xterm conventions (a vt100 `CellMouseButton`):
///   0/1/2 = left / middle / right button press,
///   32    = motion with no button,
///   35    = motion with a button held,
///   64/65 = wheel up / down.
/// `release` marks a button-release event (only meaningful for `btn` 0–2):
/// X10 encodes it as `btn + 3`, SGR keeps the same code but ends the report
/// with a lowercase `m`.
///
/// Coordinates are 1-based grid cells clamped to the screen, matching how the
/// remote draws its UI. `cols`/`rows` are the *screen* dimensions (not the
/// visible viewport) so the report always points at the same cell the program
/// rendered.
pub(crate) fn encode_mouse_event(
    btn: u8,
    release: bool,
    col: i32,
    row: i32,
    cols: u16,
    rows: u16,
    encoding: vt100::MouseProtocolEncoding,
) -> Vec<u8> {
    let c = (col.clamp(0, cols.saturating_sub(1) as i32) as u16 + 1).clamp(1, 223);
    let r = (row.clamp(0, rows.saturating_sub(1) as i32) as u16 + 1).clamp(1, 223);
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            let final_byte = if release { b'm' } else { b'M' };
            format!("\x1b[<{btn};{c};{r}{}", final_byte as char).into_bytes()
        }
        _ => {
            let cb = btn as u16 + if release { 3 } else { 0 } + 32;
            vec![0x1b, b'[', b'M', cb as u8, (c + 32) as u8, (r + 32) as u8]
        }
    }
}

/// Encode a command-bar submission and return the optional non-empty history
/// entry separately. An empty bar still represents an Enter key press (#307),
/// but must not add a blank command to persistent history.
pub(crate) fn encode_command_bar_input(command: &str) -> (Option<String>, Vec<u8>) {
    let command = command.trim_end().to_string();
    let mut bytes = command.clone().into_bytes();
    bytes.push(b'\n');
    let history = (!command.is_empty()).then_some(command);
    (history, bytes)
}

pub(crate) fn encode_pasted_text(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = normalize_pasted_newlines(text);
    if !bracketed {
        return normalized.into_bytes();
    }

    // Do not allow pasted content to forge the bracketed-paste terminator or
    // inject Ctrl+C while the remote application is accepting the payload.
    let filtered = normalized.replace(['\x1b', '\x03'], "");
    let mut bytes = Vec::with_capacity(filtered.len() + 12);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(filtered.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

pub(crate) fn terminal_uses_bracketed_paste(bufs: &TermBuffers, tab_id: &str) -> bool {
    let buffer = bufs
        .lock()
        .ok()
        .and_then(|buffers| buffers.get(tab_id).cloned());
    buffer
        .and_then(|buffer| {
            buffer
                .lock()
                .ok()
                .map(|buffer| buffer.parser.screen().bracketed_paste())
        })
        .unwrap_or(false)
}

pub(crate) fn paste_requires_large_review(text: &str) -> bool {
    const COMPACT_CHAR_LIMIT: usize = 600;
    const COMPACT_LINE_LIMIT: usize = 12;
    let bytes = text.as_bytes();
    let mut lines = 1usize;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                lines += 1;
                if bytes.get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
            }
            b'\n' => lines += 1,
            _ => {}
        }
        index += 1;
    }
    text.chars().count() > COMPACT_CHAR_LIMIT || lines > COMPACT_LINE_LIMIT
}

/// Bounds for the paste-confirm preview string.
///
/// Slint's software renderer stores glyph geometry in i16 (slint#12985 /
/// #12994). Binding a multi-thousand-line clipboard payload to the confirm
/// dialog's `Text` laid out coordinates past ±32767 and panicked. The full
/// paste stays in Rust until the user confirms; only this bounded preview
/// enters the UI tree (#434).
pub(crate) fn build_paste_preview(text: &str) -> String {
    const MAX_LINES: usize = 48;
    const MAX_LINE_CHARS: usize = 240;
    const MAX_CHARS: usize = 6 * 1024;
    const NOTICE_RESERVE: usize = 120;
    const ELLIPSIS: &str = "…";

    if text.is_empty() {
        return String::new();
    }

    let total_lines = text.lines().count();
    let total_chars = text.chars().count();

    let mut out = String::new();
    let mut taken = 0usize;
    let mut truncated = false;

    for line in text.lines() {
        if taken >= MAX_LINES {
            truncated = true;
            break;
        }

        let mut display = String::new();
        let mut n = 0usize;
        for ch in line.chars() {
            if n >= MAX_LINE_CHARS {
                display.push_str(ELLIPSIS);
                truncated = true;
                break;
            }
            display.push(ch);
            n += 1;
        }

        if out.chars().count() + display.chars().count() + 1 + NOTICE_RESERVE > MAX_CHARS {
            truncated = true;
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&display);
        taken += 1;
    }

    if truncated || taken < total_lines {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "{}{taken}/{total_lines}{}{total_chars}{}",
            crate::i18n::t(
                "…（预览已截断：显示 ",
                "… (preview truncated: showing ",
            ),
            crate::i18n::t(" 行，共 ", " lines, "),
            crate::i18n::t(
                " 字符；确认后粘贴完整内容）",
                " chars; confirming pastes the full content)",
            ),
        ));
    }

    out
}

/// Full payload for an open multi-line paste review. Keyed by tab so a
/// confirm for the wrong session cannot steal another tab's clipboard.
pub(crate) type PendingPaste = std::sync::Mutex<Option<(String /* tab_id */, String /* full */)>>;

/// Store the full clipboard text and return the bounded UI preview (#434).
///
/// Callers must only put the returned preview into Slint. The full text stays
/// here until [`take_pending_paste`] on confirm.
pub(crate) fn store_pending_paste(
    pending: &PendingPaste,
    tab_id: String,
    full_text: String,
) -> String {
    let preview = build_paste_preview(&full_text);
    if let Ok(mut slot) = pending.lock() {
        *slot = Some((tab_id, full_text));
    }
    preview
}

/// Take the full paste for `tab_id` (confirm). Returns `None` when empty or
/// when the pending payload belongs to a different tab.
pub(crate) fn take_pending_paste(pending: &PendingPaste, tab_id: &str) -> Option<String> {
    let mut slot = pending.lock().ok()?;
    match slot.as_ref() {
        Some((pending_tab, _)) if pending_tab == tab_id => slot.take().map(|(_, text)| text),
        _ => None,
    }
}

/// Drop any pending full paste (cancel / replace).
pub(crate) fn clear_pending_paste(pending: &PendingPaste) {
    if let Ok(mut slot) = pending.lock() {
        *slot = None;
    }
}

#[cfg(any(target_os = "windows", test))]
pub(crate) fn windows_process_ctrl_release(
    state: i_slint_backend_winit::winit::event::ElementState,
    logical_key: &i_slint_backend_winit::winit::keyboard::Key,
    physical_key: &i_slint_backend_winit::winit::keyboard::PhysicalKey,
) -> Option<CtrlKeySide> {
    use i_slint_backend_winit::winit::event::ElementState;
    use i_slint_backend_winit::winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

    if state != ElementState::Released || !matches!(logical_key, Key::Named(NamedKey::Process)) {
        return None;
    }

    match physical_key {
        PhysicalKey::Code(KeyCode::ControlLeft) => Some(CtrlKeySide::Left),
        PhysicalKey::Code(KeyCode::ControlRight) => Some(CtrlKeySide::Right),
        _ => None,
    }
}

pub(crate) fn should_drop_bare_ctrl_marker(
    key: &str,
    ctrl: bool,
    workaround: bool,
) -> bool {
    workaround
        && ctrl
        && matches!(
            key.chars().collect::<Vec<_>>().as_slice(),
            ['\u{0011}'] | ['\u{0016}']
        )
}

/// Slint/winit may deliver Ctrl+C either as ETX with `ctrl=true` or as the
/// already-translated ETX byte after the modifier flag has been cleared.
/// In both forms it is a real terminal interrupt and must reach the PTY.
pub(crate) fn is_terminal_interrupt(key: &str) -> bool {
    key == "\u{0003}"
}

#[cfg(target_os = "linux")]
pub(crate) fn bare_ctrl_marker_workaround_enabled() -> bool {
    // Slint/winit can expose a physical Control press as U+0011 or U+0016 on
    // Linux. This was first observed on Debian (#274) and is now confirmed on
    // Fedora as well (#369), so it is a backend/platform behaviour rather than
    // a distribution-specific quirk. The final letter event still generates
    // genuine Ctrl+Q/Ctrl+V bytes through `key_to_pty_bytes`.
    true
}

#[cfg(target_os = "macos")]
pub(crate) fn bare_ctrl_marker_workaround_enabled() -> bool {
    // Some macOS 26.5 devices repeat U+0017 while physical Control is held.
    // Without filtering it, nano receives Ctrl+W (search) before Ctrl+X (#312).
    true
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn bare_ctrl_marker_workaround_enabled() -> bool {
    false
}

/// Shift+Tab must reach the PTY as CBT / back-tab (`ESC [ Z`), which is what
/// xterm sends and what TUIs (Pi Agent, vim, fzf, readline menus…) bind.
/// Slint delivers it either as `Key.Backtab` (U+0019) or as a plain `"\t"`
/// with the shift flag set. Without this, the former was dropped by the IME
/// C0-marker filter and the latter was sent as a bare Tab, losing Shift.
/// Ctrl+Shift+Tab is handled earlier (tab cycling), and Ctrl+Y (U+0019 with
/// ctrl=true) must keep producing 0x19, so `ctrl` excludes this path.
pub(crate) fn is_back_tab(key: &str, ctrl: bool, alt: bool, shift: bool) -> bool {
    !ctrl && !alt && shift && (key == "\u{0019}" || key == "\t")
}

pub(crate) const BACK_TAB_BYTES: &[u8] = b"\x1b[Z";

/// xterm's encoding of a special key pressed with modifiers (#542):
/// `CSI 1 ; m X` for cursor keys, Home/End and F1–F4, `CSI n ; m ~` for the
/// tilde keys, where m = 1 + Shift(1) + Alt(2) + Ctrl(4). Shift+Left used to
/// arrive as a plain Left, so TUIs such as Codex could not tell them apart
/// (and Ctrl/Alt+arrow word jumps were lost the same way). Modified cursor
/// keys always use the CSI form, also in application-cursor mode, as xterm
/// does. `None` for keys without a modified form or with no modifier held.
fn modified_special_key(key: &str, shift: bool, alt: bool, ctrl: bool) -> Option<Vec<u8>> {
    let m = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    if m == 1 {
        return None;
    }
    let letter = |final_byte: char| format!("\x1b[1;{m}{final_byte}").into_bytes();
    let tilde = |number: u8| format!("\x1b[{number};{m}~").into_bytes();
    Some(match key {
        "\u{F700}" => letter('A'),
        "\u{F701}" => letter('B'),
        "\u{F702}" => letter('D'),
        "\u{F703}" => letter('C'),
        "\u{F729}" => letter('H'),
        "\u{F72B}" => letter('F'),
        "\u{F704}" => letter('P'),
        "\u{F705}" => letter('Q'),
        "\u{F706}" => letter('R'),
        "\u{F707}" => letter('S'),
        "\u{F72C}" => tilde(5),
        "\u{F72D}" => tilde(6),
        "\u{007F}" | "\u{F728}" => tilde(3),
        "\u{F708}" => tilde(15),
        "\u{F709}" => tilde(17),
        "\u{F70A}" => tilde(18),
        "\u{F70B}" => tilde(19),
        "\u{F70C}" => tilde(20),
        "\u{F70D}" => tilde(21),
        "\u{F70E}" => tilde(23),
        "\u{F70F}" => tilde(24),
        _ => return None,
    })
}

pub(crate) fn key_to_pty_bytes(
    key: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    app_cursor: bool,
) -> Vec<u8> {
    if let Some(sequence) = modified_special_key(key, shift, alt, ctrl) {
        return sequence;
    }
    let special: Option<&[u8]> = match key {
        "\u{F700}" => Some(if app_cursor { b"\x1bOA" } else { b"\x1b[A" }),
        "\u{F701}" => Some(if app_cursor { b"\x1bOB" } else { b"\x1b[B" }),
        "\u{F702}" => Some(if app_cursor { b"\x1bOD" } else { b"\x1b[D" }),
        "\u{F703}" => Some(if app_cursor { b"\x1bOC" } else { b"\x1b[C" }),
        "\u{F729}" => Some(if app_cursor { b"\x1bOH" } else { b"\x1b[H" }),
        "\u{F72B}" => Some(if app_cursor { b"\x1bOF" } else { b"\x1b[F" }),
        "\u{F72C}" => Some(b"\x1b[5~"),
        "\u{F72D}" => Some(b"\x1b[6~"),
        "\u{007F}" | "\u{F728}" => Some(b"\x1b[3~"),
        "\u{F704}" => Some(b"\x1bOP"),
        "\u{F705}" => Some(b"\x1bOQ"),
        "\u{F706}" => Some(b"\x1bOR"),
        "\u{F707}" => Some(b"\x1bOS"),
        "\u{F708}" => Some(b"\x1b[15~"),
        "\u{F709}" => Some(b"\x1b[17~"),
        "\u{F70A}" => Some(b"\x1b[18~"),
        "\u{F70B}" => Some(b"\x1b[19~"),
        "\u{F70C}" => Some(b"\x1b[20~"),
        "\u{F70D}" => Some(b"\x1b[21~"),
        "\u{F70E}" => Some(b"\x1b[23~"),
        "\u{F70F}" => Some(b"\x1b[24~"),
        _ => None,
    };
    if let Some(sequence) = special {
        return sequence.to_vec();
    }

    if key == "\u{0008}" {
        return vec![0x7f];
    }
    if key == "\n" && !ctrl && !alt {
        return vec![0x0d];
    }
    if key.is_empty() {
        return Vec::new();
    }

    if let Some(character) = key.chars().next() {
        let codepoint = character as u32;
        if key.chars().count() == 1 && !ctrl && (0x10..=0x18).contains(&codepoint) {
            return Vec::new();
        }
    }

    if ctrl {
        if let Some(character) = key.chars().next() {
            let codepoint = character as u32;
            if key.chars().count() == 1 && (0x01..=0x1f).contains(&codepoint) {
                return vec![codepoint as u8];
            }
        }
        if let Some(character) = key.chars().next() {
            if key.chars().count() == 1 {
                let upper = character.to_ascii_uppercase() as u8;
                let control = match upper {
                    b'A'..=b'Z' => Some(upper - b'A' + 1),
                    b'[' => Some(0x1b),
                    b'\\' => Some(0x1c),
                    b']' => Some(0x1d),
                    b'^' => Some(0x1e),
                    b'_' => Some(0x1f),
                    b'@' => Some(0x00),
                    _ => None,
                };
                if let Some(byte) = control {
                    return vec![byte];
                }
            }
        }
    }

    if key
        .chars()
        .any(|character| (0xE000..=0xF8FF).contains(&(character as u32)))
    {
        return Vec::new();
    }
    if alt && !ctrl {
        let mut bytes = vec![0x1b];
        bytes.extend_from_slice(key.as_bytes());
        return bytes;
    }
    key.as_bytes().to_vec()
}

#[cfg(windows)]
pub(crate) fn c0_letter_key_down(codepoint: u32) -> bool {
    if !(0x01..=0x1a).contains(&codepoint) {
        return true;
    }
    let virtual_key = (codepoint + 0x40) as i32;
    #[allow(non_snake_case)]
    extern "system" {
        fn GetKeyState(nVirtKey: i32) -> i16;
    }
    unsafe { (GetKeyState(virtual_key) as u16) & 0x8000 != 0 }
}

#[cfg(test)]
mod back_tab_tests {
    use super::*;

    #[test]
    fn slint_backtab_key_is_back_tab() {
        assert!(is_back_tab("\u{0019}", false, false, true));
    }

    #[test]
    fn shift_tab_text_is_back_tab() {
        assert!(is_back_tab("\t", false, false, true));
    }

    #[test]
    fn plain_tab_is_not_back_tab() {
        assert!(!is_back_tab("\t", false, false, false));
        assert_eq!(key_to_pty_bytes("\t", false, false, false, false), vec![0x09]);
    }

    #[test]
    fn modified_arrows_and_editing_keys_keep_their_modifiers() {
        // #542: Shift+Left used to be sent as a plain Left.
        let k = |key: &str, ctrl, alt, shift, app| key_to_pty_bytes(key, ctrl, alt, shift, app);
        assert_eq!(k("\u{F702}", false, false, true, false), b"\x1b[1;2D");
        assert_eq!(k("\u{F703}", false, false, true, true), b"\x1b[1;2C", "CSI form in app-cursor mode");
        assert_eq!(k("\u{F702}", true, false, false, false), b"\x1b[1;5D");
        assert_eq!(k("\u{F700}", false, true, false, false), b"\x1b[1;3A");
        assert_eq!(k("\u{F701}", true, false, true, false), b"\x1b[1;6B");
        assert_eq!(k("\u{F729}", false, false, true, false), b"\x1b[1;2H");
        assert_eq!(k("\u{F72B}", false, false, true, false), b"\x1b[1;2F");
        assert_eq!(k("\u{F728}", false, false, true, false), b"\x1b[3;2~");
        assert_eq!(k("\u{F72C}", true, false, false, false), b"\x1b[5;5~");
        assert_eq!(k("\u{F704}", false, false, true, false), b"\x1b[1;2P");
        assert_eq!(k("\u{F708}", true, false, false, false), b"\x1b[15;5~");
        // Unmodified keys are unchanged.
        assert_eq!(k("\u{F702}", false, false, false, false), b"\x1b[D");
        assert_eq!(k("\u{F702}", false, false, false, true), b"\x1bOD");
        assert_eq!(k("\u{F728}", false, false, false, false), b"\x1b[3~");
        // Shift on ordinary text does not change the bytes.
        assert_eq!(k("A", false, false, true, false), b"A");
    }

    #[test]
    fn ctrl_y_still_sends_em() {
        assert!(!is_back_tab("\u{0019}", true, false, false));
        assert!(!is_back_tab("\u{0019}", true, false, true));
        assert_eq!(key_to_pty_bytes("\u{0019}", true, false, false, false), vec![0x19]);
    }

    #[test]
    fn back_tab_sequence_is_cbt() {
        assert_eq!(BACK_TAB_BYTES, b"\x1b[Z");
    }
}

#[cfg(test)]
mod mouse_encoding_tests {
    use super::*;

    #[test]
    fn x10_left_press() {
        // Left press (0) at cell (1,1) → Cb=32+0=32 → ESC [ M sp ! !
        let bytes = encode_mouse_event(0, false, 0, 0, 80, 24, vt100::MouseProtocolEncoding::Default);
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 32, 33, 33]);
    }

    #[test]
    fn x10_left_release() {
        // Release adds 3 to the button code → Cb=32+3=35.
        let bytes = encode_mouse_event(0, true, 0, 0, 80, 24, vt100::MouseProtocolEncoding::Default);
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 35, 33, 33]);
    }

    #[test]
    fn x10_second_column_and_row() {
        // Cell (1,2) → Cx=33+... wait 2 → 34; row 1 → 33.
        let bytes = encode_mouse_event(0, false, 1, 0, 80, 24, vt100::MouseProtocolEncoding::Default);
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 32, 34, 33]);
    }

    #[test]
    fn sgr_left_press() {
        let bytes = encode_mouse_event(0, false, 0, 0, 80, 24, vt100::MouseProtocolEncoding::Sgr);
        assert_eq!(bytes, b"\x1b[<0;1;1M");
    }

    #[test]
    fn sgr_left_release_uses_lowercase_m() {
        let bytes = encode_mouse_event(0, true, 0, 0, 80, 24, vt100::MouseProtocolEncoding::Sgr);
        assert_eq!(bytes, b"\x1b[<0;1;1m");
    }

    #[test]
    fn x10_wheel() {
        let bytes = encode_mouse_event(64, false, 5, 3, 80, 24, vt100::MouseProtocolEncoding::Default);
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 64 + 32, 5 + 1 + 32, 3 + 1 + 32]);
    }

    #[test]
    fn sgr_wheel() {
        let bytes = encode_mouse_event(64, false, 5, 3, 80, 24, vt100::MouseProtocolEncoding::Sgr);
        assert_eq!(bytes, b"\x1b[<64;6;4M");
    }

    #[test]
    fn coordinates_clamped_to_screen() {
        // Negative / out-of-range columns clamp into the grid.
        let bytes = encode_mouse_event(0, false, -5, 999, 80, 24, vt100::MouseProtocolEncoding::Sgr);
        assert_eq!(bytes, b"\x1b[<0;1;24M");
    }

    #[test]
    fn x10_drag_motion() {
        let bytes = encode_mouse_event(32, false, 2, 2, 80, 24, vt100::MouseProtocolEncoding::Default);
        assert_eq!(bytes, vec![0x1b, b'[', b'M', 32 + 32, 2 + 1 + 32, 2 + 1 + 32]);
    }
}
