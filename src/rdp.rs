//! Launching RDP sessions in the operating system's own remote desktop client.
//!
//! RDP follows FinalShell's model: meatshell never speaks the protocol itself.
//! It only stores the account details — host, port, user, password, domain —
//! and hands them to the client that ships with the system (`mstsc` on
//! Windows), so the session opens in a familiar native window instead of one
//! of our tabs.
//!
//! On Windows the hand-off goes through a generated `.rdp` file, which is the
//! only way to pass credentials: `mstsc` deliberately refuses a password on its
//! command line. The password is stored DPAPI-encrypted for the current user,
//! exactly the format `mstsc` writes when you tick "remember me" — the blob can
//! only be decrypted by the same user on the same machine, and a failed
//! decryption (or an unsaved password) just makes the client prompt instead.

use std::process::Command;

use crate::config::Session;
use crate::i18n::t;

/// The port RDP is assigned by IANA/MS-RDPBCGR.
const RDP_DEFAULT_PORT: u16 = 3389;

/// Smallest desktop side written into a connection file. Servers reject (or
/// clamp in surprising ways) tiny desktops, and the dialog can hand us anything.
const MIN_DESKTOP_SIDE: u16 = 200;

/// The account details passed to the system client.
#[derive(Debug, PartialEq, Eq)]
struct Account<'a> {
    host: &'a str,
    port: u16,
    /// User name without the domain part.
    user: &'a str,
    /// Windows logon domain, or "" when there is none.
    domain: &'a str,
    password: &'a str,
    /// Start full screen (at the local monitor resolution).
    fullscreen: bool,
    /// Desktop size for a windowed session, in pixels.
    width: u16,
    height: u16,
}

/// Start the system remote desktop client for `session`.
///
/// `Ok` carries a short description of what was launched (shown as a hint on
/// the welcome page), `Err` a human-readable reason. This call is fire and
/// forget: the client owns its own window and lifetime.
pub(crate) fn launch(session: &Session) -> Result<String, String> {
    let host = session.host.trim();
    if host.is_empty() {
        return Err(t("主机地址为空", "host is empty").to_string());
    }
    let port = if session.port == 0 {
        RDP_DEFAULT_PORT
    } else {
        session.port
    };
    // mstsc accepts both "DOMAIN\user" and "user@domain" in its user field; the
    // dialog's own domain field wins when it is filled in.
    let (user, implied_domain) = split_user_domain(session.user.trim());
    let domain = if session.rdp_domain.trim().is_empty() {
        implied_domain
    } else {
        session.rdp_domain.trim().to_string()
    };
    let account = Account {
        host,
        port,
        user: &user,
        domain: &domain,
        password: session.password.as_str(),
        fullscreen: session.rdp_fullscreen,
        width: session.rdp_width.max(MIN_DESKTOP_SIDE),
        height: session.rdp_height.max(MIN_DESKTOP_SIDE),
    };
    start_client(&account, &session.id)
}

/// Split `DOMAIN\user` / `user@domain` into its parts; plain names pass through.
fn split_user_domain(user: &str) -> (String, String) {
    if let Some((domain, name)) = user.split_once('\\') {
        return (name.to_string(), domain.to_string());
    }
    if let Some((name, domain)) = user.split_once('@') {
        return (name.to_string(), domain.to_string());
    }
    (user.to_string(), String::new())
}

/// Path of the per-session `.rdp` file handed to `mstsc`. One file per session,
/// overwritten on every connect, so repeated use never piles up temp files.
#[cfg(windows)]
fn rdp_file_path(session_id: &str) -> std::path::PathBuf {
    // Session ids are UUIDs, so they are safe as a file name.
    std::env::temp_dir().join(format!("meatshell-rdp-{session_id}.rdp"))
}

/// Build the `.rdp` payload for `account`.
#[cfg(windows)]
fn rdp_file_contents(account: &Account<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "full address:s:{}:{}\r\n",
        account.host, account.port
    ));
    if !account.user.is_empty() {
        out.push_str(&format!("username:s:{}\r\n", account.user));
    }
    if !account.domain.is_empty() {
        out.push_str(&format!("domain:s:{}\r\n", account.domain));
    }
    match protected_password(account.password) {
        Some(blob) => {
            out.push_str(&format!("password 51:b:{blob}\r\n"));
            // Credentials are complete: do not stop on mstsc's prompt.
            out.push_str("prompt for credentials:i:0\r\n");
        }
        // No stored password (or DPAPI failed): let mstsc ask, so the user can
        // still save the credentials there.
        None => out.push_str("prompt for credentials:i:1\r\n"),
    }
    // Display: either full screen — mstsc then uses the local monitor
    // resolution, which is the sharpest option — or a window at the configured
    // desktop size, scaled when the window is resized instead of scrolling.
    if account.fullscreen {
        out.push_str("screen mode id:i:2\r\n");
    } else {
        out.push_str("screen mode id:i:1\r\n");
        out.push_str(&format!("desktopwidth:i:{}\r\n", account.width));
        out.push_str(&format!("desktopheight:i:{}\r\n", account.height));
        out.push_str("smart sizing:i:1\r\n");
    }
    // Local clipboard shared (what a launcher is expected to give you), and no
    // certificate warning for servers whose RDP certificate is self-signed —
    // the usual case for a standalone Windows box.
    out.push_str("redirectclipboard:i:1\r\n");
    out.push_str("authentication level:i:0\r\n");
    out.push_str("session bpp:i:32\r\n");
    out.push_str("compression:i:1\r\n");
    out
}

/// Encrypt `plain` into the hex-encoded DPAPI blob an `.rdp` file expects.
/// `None` means "no password to pass" (empty, or DPAPI is unavailable).
#[cfg(windows)]
fn protected_password(plain: &str) -> Option<String> {
    if plain.is_empty() {
        return None;
    }
    let mut wide: Vec<u16> = plain.encode_utf16().collect();
    // A terminating NUL is what a C-style reader expects; mstsc accepts both.
    wide.push(0);

    #[repr(C)]
    struct DataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    #[link(name = "crypt32")]
    extern "system" {
        fn CryptProtectData(
            p_data_in: *const DataBlob,
            sz_data_descr: *const u16,
            p_optional_entropy: *const DataBlob,
            pv_reserved: *mut core::ffi::c_void,
            p_prompt_struct: *mut core::ffi::c_void,
            dw_flags: u32,
            p_data_out: *mut DataBlob,
        ) -> i32;
        fn LocalFree(h_mem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }

    let input = DataBlob {
        cb_data: u32::try_from(wide.len()).ok()? * 2,
        pb_data: wide.as_mut_ptr().cast::<u8>(),
    };
    let mut output = DataBlob {
        cb_data: 0,
        pb_data: std::ptr::null_mut(),
    };
    // SAFETY: `input` describes a live buffer, `output` is only written by the
    // call, and both pointers stay valid for its duration. No entropy and no
    // LOCAL_MACHINE flag: that is exactly what mstsc uses when it decrypts.
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            &mut output,
        )
    };
    if ok == 0 || output.pb_data.is_null() {
        return None;
    }
    // SAFETY: the call succeeded, so `output` owns `cb_data` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize) };
    let hex: String = bytes.iter().map(|byte| format!("{byte:02X}")).collect();
    // SAFETY: the buffer came from CryptProtectData, which requires LocalFree.
    unsafe {
        LocalFree(output.pb_data.cast::<core::ffi::c_void>());
    }
    Some(hex)
}

#[cfg(windows)]
fn start_client(account: &Account<'_>, session_id: &str) -> Result<String, String> {
    let path = rdp_file_path(session_id);
    std::fs::write(&path, rdp_file_contents(account)).map_err(|err| {
        format!(
            "{} {}: {err}",
            t("无法写入连接文件", "cannot write the connection file"),
            path.display()
        )
    })?;
    Command::new("mstsc").arg(&path).spawn().map_err(|err| {
        format!(
            "{} mstsc: {err}",
            t("无法启动系统远程桌面", "cannot start the system remote desktop client")
        )
    })?;
    Ok(format!("{}:{} (mstsc)", account.host, account.port))
}

/// Linux / macOS / BSD: drive FreeRDP's `xfreerdp` when it is installed. The
/// credential block is fed through stdin (`/from-stdin`) so the password never
/// shows up in the process list.
#[cfg(not(windows))]
fn start_client(account: &Account<'_>, _session_id: &str) -> Result<String, String> {
    for client in ["xfreerdp3", "xfreerdp"] {
        let mut command = Command::new(client);
        command
            .arg(format!("/v:{}:{}", account.host, account.port))
            .arg("/cert:ignore")
            .arg("+clipboard")
            .arg("/from-stdin")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Same display choice as the Windows path.
        if account.fullscreen {
            command.arg("/f");
        } else {
            command.arg(format!("/w:{}", account.width));
            command.arg(format!("/h:{}", account.height));
        }
        let Ok(mut child) = command.spawn() else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write as _;
            // /from-stdin reads user name, password and domain, one per line.
            let _ = writeln!(stdin, "{}", account.user);
            let _ = writeln!(stdin, "{}", account.password);
            let _ = writeln!(stdin, "{}", account.domain);
        }
        return Ok(format!("{}:{} ({client})", account.host, account.port));
    }
    Err(t(
        "未找到系统 RDP 客户端，请安装 FreeRDP（xfreerdp）后重试",
        "no system RDP client found — install FreeRDP (xfreerdp) and try again",
    )
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_and_domain_are_split_from_the_user_field() {
        assert_eq!(
            split_user_domain("CONTOSO\\alice"),
            ("alice".to_string(), "CONTOSO".to_string())
        );
        assert_eq!(
            split_user_domain("alice@contoso.com"),
            ("alice".to_string(), "contoso.com".to_string())
        );
        assert_eq!(
            split_user_domain("alice"),
            ("alice".to_string(), String::new())
        );
        assert_eq!(
            split_user_domain(""),
            (String::new(), String::new())
        );
    }

    #[test]
    #[cfg(windows)]
    fn rdp_file_carries_address_and_account() {
        let account = Account {
            host: "192.168.1.10",
            port: 3389,
            user: "alice",
            domain: "CONTOSO",
            password: "",
            fullscreen: false,
            width: 1920,
            height: 1080,
        };
        let contents = rdp_file_contents(&account);
        assert!(contents.contains("full address:s:192.168.1.10:3389\r\n"));
        assert!(contents.contains("username:s:alice\r\n"));
        assert!(contents.contains("domain:s:CONTOSO\r\n"));
        // No saved password → let mstsc prompt.
        assert!(contents.contains("prompt for credentials:i:1\r\n"));
        assert!(!contents.contains("password 51:b:"));
        // Windowed at the configured desktop size.
        assert!(contents.contains("screen mode id:i:1\r\n"));
        assert!(contents.contains("desktopwidth:i:1920\r\n"));
        assert!(contents.contains("desktopheight:i:1080\r\n"));
    }

    /// Full screen lets mstsc use the local monitor resolution, so no desktop
    /// size may be requested — otherwise the session would be scaled.
    #[test]
    #[cfg(windows)]
    fn full_screen_does_not_request_a_desktop_size() {
        let account = Account {
            host: "server",
            port: 3389,
            user: "",
            domain: "",
            password: "",
            fullscreen: true,
            width: 1280,
            height: 720,
        };
        let contents = rdp_file_contents(&account);
        assert!(contents.contains("screen mode id:i:2\r\n"));
        assert!(!contents.contains("desktopwidth:i:"));
        assert!(!contents.contains("desktopheight:i:"));
        // An empty user name must not be written as `username:s:`.
        assert!(!contents.contains("username:s:"));
    }

    /// A stored password must produce a blob mstsc can decrypt, which also
    /// proves the DPAPI call itself is wired up correctly.
    #[test]
    #[cfg(windows)]
    fn stored_password_round_trips_through_dpapi() {
        #[repr(C)]
        struct DataBlob {
            cb_data: u32,
            pb_data: *mut u8,
        }

        #[link(name = "crypt32")]
        extern "system" {
            fn CryptUnprotectData(
                p_data_in: *const DataBlob,
                ppsz_data_descr: *mut *mut u16,
                p_optional_entropy: *const DataBlob,
                pv_reserved: *mut core::ffi::c_void,
                p_prompt_struct: *mut core::ffi::c_void,
                dw_flags: u32,
                p_data_out: *mut DataBlob,
            ) -> i32;
            fn LocalFree(h_mem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        }

        let account = Account {
            host: "127.0.0.1",
            port: 3389,
            user: "alice",
            domain: "",
            password: "p@ssw0rd-示例",
            fullscreen: false,
            width: 1280,
            height: 720,
        };
        let contents = rdp_file_contents(&account);
        let line = contents
            .lines()
            .find_map(|line| line.strip_prefix("password 51:b:"))
            .expect("a stored password must be written");
        assert!(contents.contains("prompt for credentials:i:0\r\n"));

        let blob: Vec<u8> = (0..line.len() / 2)
            .map(|i| u8::from_str_radix(&line[i * 2..i * 2 + 2], 16).expect("hex blob"))
            .collect();
        let input = DataBlob {
            cb_data: blob.len() as u32,
            pb_data: blob.as_ptr() as *mut u8,
        };
        let mut output = DataBlob {
            cb_data: 0,
            pb_data: std::ptr::null_mut(),
        };
        // SAFETY: same contract as the encryption side.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                &mut output,
            )
        };
        assert_ne!(ok, 0, "the blob must be decryptable by this user");
        // SAFETY: the call succeeded, so the buffer is readable.
        let plain = unsafe {
            std::slice::from_raw_parts(output.pb_data, output.cb_data as usize)
        };
        let wide: Vec<u16> = plain
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let decoded = String::from_utf16_lossy(&wide);
        // SAFETY: the buffer came from CryptUnprotectData.
        unsafe {
            LocalFree(output.pb_data.cast::<core::ffi::c_void>());
        }
        assert_eq!(
            decoded.trim_end_matches('\0'),
            "p@ssw0rd-示例",
            "the round-tripped password must match"
        );
    }
}

