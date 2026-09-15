//! Launching RDP sessions in a remote desktop client that already speaks RDP.
//!
//! RDP follows FinalShell's model: meatshell never implements the protocol
//! itself. It only stores the account details — host, port, user, password,
//! domain, resolution — and hands them over, so the session opens in the
//! client's own native window instead of one of our tabs.
//!
//! * Windows: `mstsc`, driven through a generated `.rdp` file, which is the only
//!   way to pass credentials — `mstsc` deliberately refuses a password on its
//!   command line. The password goes in DPAPI-encrypted for the current user,
//!   exactly the format `mstsc` writes when you tick "remember me": only the
//!   same user on the same machine can decrypt it, and a failed decryption (or
//!   an unsaved password) just makes the client prompt instead.
//! * Linux / macOS / BSD: FreeRDP's `xfreerdp3` / `xfreerdp`, with the password
//!   handed over on stdin so it never shows up in the process list. The Flatpak
//!   bundle builds FreeRDP into `/app` (see `packaging/flatpak`), so no separate
//!   installation is needed there.

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

/// Portable pieces of the FreeRDP hand-off: which flavour the installed client
/// is, and the command line it needs. Deliberately free of `cfg` so the tests
/// below run on every platform, not only on Linux.
#[cfg_attr(windows, allow(dead_code))]
mod freerdp {
    use super::Account;

    /// Which command line dialect a FreeRDP build speaks. FreeRDP 3 added the
    /// optional `force` value to `/from-stdin` — read the credentials before
    /// connecting instead of when the server asks for them — and FreeRDP 2
    /// rejects that value, so the two cannot share one flag.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum Dialect {
        /// FreeRDP 2.x: the `xfreerdp` binary on older distributions.
        V2,
        /// FreeRDP 3.x: `xfreerdp3`, and what the Flatpak bundle carries.
        V3,
    }

    /// Client binaries to try, in order.
    ///
    /// `PATH` comes first, which is what someone who installed FreeRDP by hand
    /// expects and where Flatpak puts the bundled client (`/app/bin`). The
    /// absolute paths cover GUI launches: those can inherit a bare `PATH` that
    /// misses `/usr/local/bin`, and macOS GUI apps never see `/opt/homebrew`.
    pub(super) const CLIENTS: [&str; 9] = [
        "xfreerdp3",
        "xfreerdp",
        "/app/bin/xfreerdp3",
        "/usr/bin/xfreerdp3",
        "/usr/local/bin/xfreerdp3",
        "/opt/homebrew/bin/xfreerdp3",
        "/usr/bin/xfreerdp",
        "/usr/local/bin/xfreerdp",
        "/opt/homebrew/bin/xfreerdp",
    ];

    /// Read the major version out of a `--version` banner such as
    /// "This is FreeRDP version 3.21.0 (3.21.0)". `None` means the text says
    /// nothing usable; the caller then assumes the older, always-valid flag.
    pub(super) fn parse_dialect(version_output: &str) -> Option<Dialect> {
        let after = version_output.split_once("version")?.1;
        let major: u32 = after
            .trim_start()
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()?;
        Some(if major >= 3 { Dialect::V3 } else { Dialect::V2 })
    }

    /// Ask a client binary for its version. `None` also means "no such binary",
    /// which is how the caller finds the next candidate.
    pub(super) fn probe_dialect(client: &str) -> Option<Dialect> {
        for flag in ["--version", "/version"] {
            let Ok(output) = std::process::Command::new(client)
                .arg(flag)
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
            else {
                return None;
            };
            if let Some(dialect) = parse_dialect(&String::from_utf8_lossy(&output.stdout)) {
                return Some(dialect);
            }
        }
        None
    }

    /// The command line for a FreeRDP connect.
    ///
    /// The password is deliberately absent — it goes over stdin, see
    /// [`credential_lines`]. User name and domain are not secrets: they sit in
    /// `mstsc`'s `.rdp` file on Windows and in the session list in the UI.
    pub(super) fn args(account: &Account<'_>, dialect: Dialect) -> Vec<String> {
        let mut args = vec![
            format!("/v:{}:{}", account.host, account.port),
            "/cert:ignore".to_string(),
            "/clipboard".to_string(),
            // Let the session follow the window when it is resized. FreeRDP
            // refuses to start when `+smart-sizing` is given as well — "Smart
            // sizing and dynamic resolution are mutually exclusive options" —
            // and following the window keeps the picture pixel sharp instead of
            // scaling it up, so this is the one we ask for.
            "+dynamic-resolution".to_string(),
        ];
        // Omitted when empty: `/u:` with no value does not mean "no user name".
        if !account.user.is_empty() {
            args.push(format!("/u:{}", account.user));
        }
        if !account.domain.is_empty() {
            args.push(format!("/d:{}", account.domain));
        }
        // Same display choice as the Windows path.
        if account.fullscreen {
            args.push("/f".to_string());
        } else {
            args.push(format!("/w:{}", account.width));
            args.push(format!("/h:{}", account.height));
        }
        args.push(match dialect {
            // `:force` reads stdin up front. Without it FreeRDP 3 only reads
            // when the server asks for credentials, which is where its
            // `/from-stdin` handling regressed (FreeRDP issue #10217).
            Dialect::V3 => "/from-stdin:force".to_string(),
            Dialect::V2 => "/from-stdin".to_string(),
        });
        args
    }

    /// The lines the client reads from stdin, in the order it asks for them.
    ///
    /// FreeRDP only prompts for what the command line left out, so with a user
    /// name in the arguments this is the password alone — no reliance on an
    /// undocumented order of user name, password and domain.
    pub(super) fn credential_lines(account: &Account<'_>) -> Vec<String> {
        let mut lines = Vec::new();
        if account.user.is_empty() {
            lines.push(account.user.to_string());
        }
        lines.push(account.password.to_string());
        lines
    }

    /// True inside a Flatpak sandbox, where the client comes with the bundle in
    /// `/app` rather than with the host.
    pub(super) fn in_flatpak_sandbox() -> bool {
        std::env::var_os("FLATPAK_ID").is_some() || std::path::Path::new("/.flatpak-info").exists()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn account() -> Account<'static> {
            Account {
                host: "10.0.0.5",
                port: 3389,
                user: "alice",
                domain: "CONTOSO",
                password: "s3cret",
                fullscreen: false,
                width: 1600,
                height: 900,
            }
        }

        #[test]
        fn dialect_follows_the_reported_major_version() {
            assert_eq!(
                parse_dialect("This is FreeRDP version 3.21.0 (3.21.0)"),
                Some(Dialect::V3)
            );
            assert_eq!(
                parse_dialect("This is FreeRDP version 2.11.5 (2.11.5)"),
                Some(Dialect::V2)
            );
            // Nothing usable → the caller falls back to the older dialect.
            assert_eq!(parse_dialect(""), None);
            assert_eq!(parse_dialect("xfreerdp: command not found"), None);
        }

        #[test]
        fn the_password_never_reaches_the_command_line() {
            for dialect in [Dialect::V2, Dialect::V3] {
                let args = args(&account(), dialect);
                assert!(
                    !args.iter().any(|arg| arg.contains("s3cret")),
                    "{dialect:?} leaked the password: {args:?}"
                );
                assert!(args.contains(&"/u:alice".to_string()));
                assert!(args.contains(&"/d:CONTOSO".to_string()));
                assert!(args.contains(&"/v:10.0.0.5:3389".to_string()));
            }
        }

        #[test]
        fn v3_forces_the_stdin_read_and_v2_must_not() {
            assert!(args(&account(), Dialect::V3).contains(&"/from-stdin:force".to_string()));
            let v2 = args(&account(), Dialect::V2);
            assert!(v2.contains(&"/from-stdin".to_string()));
            // FreeRDP 2 rejects a value there and would refuse to start.
            assert!(!v2.iter().any(|arg| arg.starts_with("/from-stdin:")));
        }

        /// FreeRDP aborts with "Smart sizing and dynamic resolution are
        /// mutually exclusive options" when both are on the command line, which
        /// is how the very first Deepin test run failed.
        #[test]
        fn only_one_resize_strategy_is_requested() {
            for dialect in [Dialect::V2, Dialect::V3] {
                let args = args(&account(), dialect);
                assert!(args.contains(&"+dynamic-resolution".to_string()));
                assert!(
                    !args.iter().any(|arg| arg.contains("smart-sizing")),
                    "{dialect:?} asked for both resize strategies: {args:?}"
                );
            }
        }

        #[test]
        fn windowed_and_full_screen_sizes_are_mutually_exclusive() {
            let mut account = account();
            let windowed = args(&account, Dialect::V3);
            assert!(windowed.contains(&"/w:1600".to_string()));
            assert!(windowed.contains(&"/h:900".to_string()));
            assert!(!windowed.contains(&"/f".to_string()));

            account.fullscreen = true;
            let full = args(&account, Dialect::V3);
            assert!(full.contains(&"/f".to_string()));
            assert!(!full
                .iter()
                .any(|arg| arg.starts_with("/w:") || arg.starts_with("/h:")));
        }

        #[test]
        fn an_empty_user_or_domain_is_not_sent_at_all() {
            let mut account = account();
            account.user = "";
            account.domain = "";
            let args = args(&account, Dialect::V3);
            assert!(!args
                .iter()
                .any(|arg| arg.starts_with("/u:") || arg.starts_with("/d:")));
        }

        #[test]
        fn credentials_are_only_the_fields_missing_from_the_command_line() {
            // User name (and domain) came with the arguments, so the password is
            // the only thing the client asks for.
            assert_eq!(credential_lines(&account()), vec!["s3cret".to_string()]);

            // Without a user name the client asks for it too, and an empty line
            // is the answer.
            let mut anonymous = account();
            anonymous.user = "";
            assert_eq!(
                credential_lines(&anonymous),
                vec![String::new(), "s3cret".to_string()]
            );
        }

        #[test]
        fn the_flatpak_bundle_is_looked_up_by_name_and_by_path() {
            assert!(CLIENTS.contains(&"xfreerdp3"));
            assert!(CLIENTS.contains(&"/app/bin/xfreerdp3"));
        }
    }
}

/// Feed the credentials to a running client over its stdin: a pipe cannot be
/// read out of the process list, and nothing is written to disk.
#[cfg(not(windows))]
fn write_credentials(child: &mut std::process::Child, account: &Account<'_>) {
    use std::io::Write as _;
    let Some(mut stdin) = child.stdin.take() else {
        return;
    };
    for line in freerdp::credential_lines(account) {
        let _ = writeln!(stdin, "{line}");
    }
    let _ = stdin.flush();
}

/// Give a freshly started client a moment to fail, collecting what it printed.
///
/// A missing display, an unknown flag or a client that needs different
/// arguments makes it exit at once, and knowing that beats announcing a window
/// that never appears. `Some(stderr)` means it exited; the text is for the log,
/// not for the UI. `None` means it is still running, which is what we want.
#[cfg(not(windows))]
fn early_exit(child: &mut std::process::Child) -> Option<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(700);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => return None,
        }
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        use std::io::Read as _;
        let _ = pipe.take(4096).read_to_string(&mut stderr);
    }
    Some(stderr.trim().to_string())
}

/// Linux / macOS / BSD: drive FreeRDP's `xfreerdp3` / `xfreerdp`.
#[cfg(not(windows))]
fn start_client(account: &Account<'_>, _session_id: &str) -> Result<String, String> {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Err(t(
            "当前环境没有图形显示（DISPLAY / WAYLAND_DISPLAY），无法打开远程桌面窗口",
            "no graphical display here (DISPLAY / WAYLAND_DISPLAY) — cannot open a remote desktop window",
        )
        .to_string());
    }
    for client in freerdp::CLIENTS {
        // Unknown version (or a client that does not answer): assume the older
        // dialect, whose flag is accepted by both.
        let dialect = freerdp::probe_dialect(client).unwrap_or(freerdp::Dialect::V2);
        let mut command = Command::new(client);
        command
            .args(freerdp::args(account, dialect))
            // An AppImage would otherwise hand its own library directory down to
            // the client, which then loads our bundled glib/OpenSSL and breaks.
            .env_remove("LD_LIBRARY_PATH")
            .env_remove("LD_PRELOAD")
            .env_remove("APPDIR")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let Ok(mut child) = command.spawn() else {
            continue;
        };
        write_credentials(&mut child, account);
        if let Some(stderr) = early_exit(&mut child) {
            // The client's own words go to the log, not into the UI: FreeRDP
            // prefixes every line with a timestamp and an internal module name,
            // which tells the user nothing. Everything else about the failure
            // (that it exited, and which binary it was) still reaches them, so a
            // broken connection is never announced as a started session.
            tracing::warn!("{client} exited right away: {stderr}");
            return Err(format!(
                "{} ({client})",
                t(
                    "RDP 客户端启动失败，详情见日志",
                    "the RDP client failed to start — see the log for details"
                )
            ));
        }
        // Detached reaper: `Child` does not wait on drop, so without this every
        // connect would leave a zombie behind for as long as meatshell runs.
        // The stderr pipe is drained in the same thread — a full pipe would
        // block the client.
        let stderr = child.stderr.take();
        std::thread::spawn(move || {
            if let Some(mut pipe) = stderr {
                let _ = std::io::copy(&mut pipe, &mut std::io::sink());
            }
            let _ = child.wait();
        });
        return Ok(format!("{}:{} ({client})", account.host, account.port));
    }
    Err(if freerdp::in_flatpak_sandbox() {
        t(
            "Flatpak 版自带 FreeRDP 客户端，找不到说明安装损坏，请重新安装 meatshell 的 Flatpak 包",
            "the Flatpak bundle ships its own FreeRDP client — it is missing, so please reinstall the meatshell Flatpak",
        )
    } else {
        t(
            "未找到 FreeRDP 客户端；请先安装：Debian/Ubuntu `sudo apt install freerdp3-x11`，Fedora `sudo dnf install freerdp`，Arch `sudo pacman -S freerdp`，macOS `brew install freerdp`",
            "no FreeRDP client found; install it first: Debian/Ubuntu `sudo apt install freerdp3-x11`, Fedora `sudo dnf install freerdp`, Arch `sudo pacman -S freerdp`, macOS `brew install freerdp`",
        )
    }
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

