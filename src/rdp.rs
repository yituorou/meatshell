//! Launching RDP sessions in a remote desktop client that already speaks RDP.
//!
//! RDP follows FinalShell's model: meatshell never implements the protocol
//! itself. It only stores the account details — host, port, user, password,
//! domain, resolution — and hands them over, so the session opens in the
//! client's own native window instead of one of our tabs.
//!
//! * Windows: `mstsc`, driven through a generated `.rdp` file (address, user,
//!   display). `mstsc` deliberately refuses a password on its command line, so
//!   the password is saved the way its own "Remember me" saves it: a domain
//!   credential for `TERMSRV/<host>` in Windows Credential Manager, readable
//!   only by this Windows user. Without a saved password the client prompts.
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
    legacy_tls: bool,
    admin: bool,
    smart_sizing: bool,
    cert_policy: &'a str,
    network: &'a str,
    sound: bool,
    clipboard: bool,
    gfx: bool,
    extra_args: Vec<String>,
}

/// Start the system remote desktop client for `session`.
///
/// `Ok` carries a short description of what was launched (shown as a hint on
/// the welcome page), `Err` a human-readable reason. This call is fire and
/// forget: the client owns its own window and lifetime.
pub(crate) fn launch(session: &Session) -> Result<String, String> {
    with_account(session, |account| start_client(account, &session.id))
}

/// Validate on save and again on launch (imported configs are untrusted).
pub(crate) fn validate_options(session: &Session) -> Result<(), String> {
    freerdp::session_options(session).map(|_| ())
}

/// Preview only the command line, never the saved password or stdin contents.
pub(crate) fn preview_args(session: &Session) -> Result<String, String> {
    if cfg!(windows) {
        return Err(t(
            "Windows 使用 mstsc，FreeRDP 参数不适用",
            "Windows uses mstsc; FreeRDP arguments do not apply",
        )
        .to_string());
    }
    with_account(session, |account| {
        let (binary, dialect) = freerdp::CLIENTS
            .iter()
            .find_map(|binary| freerdp::probe_dialect(binary).map(|dialect| (*binary, dialect)))
            .unwrap_or(("xfreerdp", freerdp::Dialect::V2));
        let rendered = freerdp::args(account, dialect)
            .into_iter()
            .map(|arg| {
                // This is a preview, not a shell command; quote whitespace only for readability.
                if arg.contains(char::is_whitespace) {
                    format!("{arg:?}")
                } else {
                    arg
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        Ok(format!("{binary} {rendered}"))
    })
}

fn with_account<R>(
    session: &Session,
    action: impl FnOnce(&Account<'_>) -> Result<R, String>,
) -> Result<R, String> {
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
    #[cfg(not(windows))]
    let extra_args = freerdp::session_options(session)?;
    #[cfg(windows)]
    let extra_args = Vec::new();
    let account = Account {
        host,
        port,
        user: &user,
        domain: &domain,
        password: session.password.as_str(),
        fullscreen: session.rdp_fullscreen,
        width: session.rdp_width.max(MIN_DESKTOP_SIDE),
        height: session.rdp_height.max(MIN_DESKTOP_SIDE),
        legacy_tls: session.rdp_legacy_tls,
        admin: session.rdp_admin,
        smart_sizing: session.rdp_smart_sizing,
        cert_policy: &session.rdp_cert_policy,
        network: &session.rdp_network,
        sound: session.rdp_sound,
        clipboard: session.rdp_clipboard,
        gfx: session.rdp_gfx,
        extra_args,
    };
    action(&account)
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

/// Build the `.rdp` payload for `account`. The password is never written
/// into the file: `saved_credential` says whether [`save_credential`] stored
/// it in Windows Credential Manager, where mstsc picks it up on its own.
#[cfg(windows)]
fn rdp_file_contents(account: &Account<'_>, saved_credential: bool) -> String {
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
    if saved_credential {
        // Credentials are complete: do not stop on mstsc's prompt.
        out.push_str("prompt for credentials:i:0\r\n");
    } else {
        // No stored password (or saving it failed): let mstsc ask, so the
        // user can still save the credentials there.
        out.push_str("prompt for credentials:i:1\r\n");
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

/// Windows Credential Manager targets mstsc looks up for `host:port`. mstsc
/// files "Remember me" credentials under `TERMSRV/<host>`; a non-default
/// port is also covered with `TERMSRV/<host>:<port>`.
#[cfg(windows)]
fn credential_targets(host: &str, port: u16) -> Vec<String> {
    let mut targets = vec![format!("TERMSRV/{host}")];
    if port != RDP_DEFAULT_PORT {
        targets.push(format!("TERMSRV/{host}:{port}"));
    }
    targets
}

/// The user name as mstsc stores it: `DOMAIN\user`, or just `user`.
#[cfg(windows)]
fn credential_user(account: &Account<'_>) -> String {
    if account.domain.is_empty() {
        account.user.to_string()
    } else {
        format!("{}\\{}", account.domain, account.user)
    }
}

/// Hand the saved password to mstsc the way its own "Remember me" does: as a
/// domain credential for `TERMSRV/<host>` in Windows Credential Manager.
///
/// Not as a `password 51:b:` line in the `.rdp` file: mstsc keeps sending that
/// password even after the server redirects the connection with one-time
/// credentials, which Ubuntu's GNOME Remote Desktop does after its login
/// screen. The redirected login is then rejected and the window closes at
/// once, while typed or remembered credentials work.
///
/// False when there is nothing to save (no user or password) or Windows
/// refused, so the caller lets mstsc prompt instead.
#[cfg(windows)]
fn save_credential(account: &Account<'_>) -> bool {
    if account.user.is_empty() || account.password.is_empty() {
        return false;
    }
    let user = credential_user(account);
    let mut saved = true;
    for target in credential_targets(account.host, account.port) {
        if let Err(code) = credman::write_domain_password(&target, &user, account.password) {
            tracing::warn!("rdp: could not save the credential for {target}: error {code}");
            saved = false;
        }
    }
    saved
}

/// Minimal Credential Manager bindings (advapi32), kept local like the rest
/// of this module's Windows calls.
#[cfg(windows)]
mod credman {
    use std::ptr::{null, null_mut};

    const CRED_TYPE_DOMAIN_PASSWORD: u32 = 2;
    /// Kept across logons for this Windows user, like mstsc's "Remember me".
    const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;

    #[repr(C)]
    struct Credential {
        flags: u32,
        kind: u32,
        target_name: *const u16,
        comment: *const u16,
        last_written: [u32; 2],
        credential_blob_size: u32,
        credential_blob: *const u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut core::ffi::c_void,
        target_alias: *const u16,
        user_name: *const u16,
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        #[cfg_attr(not(test), allow(dead_code))]
        fn CredDeleteW(target_name: *const u16, kind: u32, flags: u32) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetLastError() -> u32;
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Create or replace the domain credential for `target`. The blob is the
    /// UTF-16 password without a terminating NUL. `Err` is the Win32 error.
    pub(super) fn write_domain_password(
        target: &str,
        user: &str,
        password: &str,
    ) -> Result<(), u32> {
        let target = wide(target);
        let user = wide(user);
        let blob: Vec<u8> = password.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let credential = Credential {
            flags: 0,
            kind: CRED_TYPE_DOMAIN_PASSWORD,
            target_name: target.as_ptr(),
            comment: null(),
            last_written: [0, 0],
            credential_blob_size: u32::try_from(blob.len()).map_err(|_| 87u32)?,
            credential_blob: blob.as_ptr(),
            persist: CRED_PERSIST_LOCAL_MACHINE,
            attribute_count: 0,
            attributes: null_mut(),
            target_alias: null(),
            user_name: user.as_ptr(),
        };
        // SAFETY: every pointer refers to a live, NUL-terminated (strings) or
        // correctly sized (blob) buffer that outlives the call.
        if unsafe { CredWriteW(&credential, 0) } != 0 {
            Ok(())
        } else {
            // SAFETY: reads the calling thread's last-error value.
            Err(unsafe { GetLastError() })
        }
    }

    /// Remove the domain credential for `target` (used by tests to clean up).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn delete_domain_password(target: &str) -> bool {
        let target = wide(target);
        // SAFETY: `target` is a live NUL-terminated buffer.
        unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_DOMAIN_PASSWORD, 0) != 0 }
    }
}

#[cfg(windows)]
fn start_client(account: &Account<'_>, session_id: &str) -> Result<String, String> {
    let saved_credential = save_credential(account);
    let path = rdp_file_path(session_id);
    std::fs::write(&path, rdp_file_contents(account, saved_credential)).map_err(|err| {
        format!(
            "{} {}: {err}",
            t("无法写入连接文件", "cannot write the connection file"),
            path.display()
        )
    })?;
    Command::new("mstsc").arg(&path).spawn().map_err(|err| {
        format!(
            "{} mstsc: {err}",
            t(
                "无法启动系统远程桌面",
                "cannot start the system remote desktop client"
            )
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
    use crate::config::Session;
    use std::collections::HashSet;

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

    /// Each nonempty line is one argument, not a shell fragment. Chinese and
    /// English aliases cover common values; arbitrary native FreeRDP switches
    /// remain available in their original spelling.
    pub(super) fn parse_extra_args(input: &str) -> Result<Vec<String>, String> {
        let mut result = Vec::new();
        let mut keys = HashSet::new();
        for (index, line) in input.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if result.len() >= 32 || line.len() > 512 || line.chars().any(char::is_control) {
                return Err(format!(
                    "{} {}",
                    super::t(
                        "RDP 参数过多或过长，行",
                        "too many or too long RDP arguments, line"
                    ),
                    index + 1
                ));
            }
            let arg = if !line.starts_with(['/', '+', '-']) && line.contains('=') {
                let (name, value) = line.split_once('=').expect("checked above");
                match (name.trim().to_ascii_lowercase().as_str(), value.trim()) {
                    ("网络" | "network", "局域网" | "lan") => "/network:lan".to_string(),
                    ("网络" | "network", "广域网" | "wan") => "/network:wan".to_string(),
                    ("网络" | "network", "自动" | "auto") => "/network:auto".to_string(),
                    ("音频" | "sound", "开" | "on") => "/sound".to_string(),
                    ("音频" | "sound", "关" | "off") => "-sound".to_string(),
                    ("麦克风" | "microphone", "开" | "on") => "/microphone".to_string(),
                    ("打印机" | "printer", "开" | "on") => "/printer".to_string(),
                    ("色深" | "bpp", "16" | "24" | "32") => format!("/bpp:{}", value.trim()),
                    ("共享目录" | "drive", path) if path.contains(',') => {
                        format!("/drive:{path}")
                    }
                    _ => {
                        return Err(format!(
                            "{} {}",
                            super::t(
                                "未知的 RDP 参数别名，行",
                                "unknown RDP argument alias, line"
                            ),
                            index + 1
                        ))
                    }
                }
            } else {
                line.to_string()
            };
            let key = option_key(&arg).ok_or_else(|| {
                format!(
                    "{} {}",
                    super::t("无效的 RDP 参数，行", "invalid RDP argument, line"),
                    index + 1
                )
            })?;
            // Keep credentials on stdin. The forbidden options also prevent
            // overriding connection identity, certificate policy, and display.
            if [
                "p",
                "password",
                "passwd",
                "gateway",
                "proxy",
                "v",
                "u",
                "d",
                "from-stdin",
                "cert",
                "tls",
                "tls-seclevel",
                "size",
                "w",
                "h",
                "f",
                "smart-sizing",
                "dynamic-resolution",
                "admin",
                "clipboard",
            ]
            .contains(&key.as_str())
                || key.contains("secret")
                || key.contains("token")
                || key.contains("pass")
                || arg.contains(';')
                || arg.contains('|')
                || arg.contains('`')
                || (key != "drive" && arg.contains(char::is_whitespace))
            {
                return Err(format!(
                    "{} {}",
                    super::t(
                        "禁止或冲突的 RDP 参数，行",
                        "forbidden or conflicting RDP argument, line"
                    ),
                    index + 1
                ));
            }
            // Multiple shares are valid, but the share name must be unique.
            let unique_key = if key == "drive" {
                let spec = arg.split_once(':').map(|(_, value)| value).unwrap_or("");
                let (name, path) = spec.split_once(',').ok_or_else(|| {
                    format!(
                        "{} {}",
                        super::t(
                            "共享目录缺少名称或路径，行",
                            "drive share needs a name and path, line"
                        ),
                        index + 1
                    )
                })?;
                if name.is_empty() || path.is_empty() {
                    return Err(format!(
                        "{} {}",
                        super::t(
                            "共享目录缺少名称或路径，行",
                            "drive share needs a name and path, line"
                        ),
                        index + 1
                    ));
                }
                format!("drive:{}", name.to_lowercase())
            } else {
                key
            };
            if !keys.insert(unique_key) {
                return Err(format!(
                    "{} {}",
                    super::t("重复的 RDP 参数，行", "duplicate RDP argument, line"),
                    index + 1
                ));
            }
            result.push(arg);
        }
        Ok(result)
    }

    fn option_key(arg: &str) -> Option<String> {
        let name = arg
            .strip_prefix('/')
            .or_else(|| arg.strip_prefix('+'))
            .or_else(|| arg.strip_prefix('-'))?;
        let key = name.split(':').next()?.to_ascii_lowercase();
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return None;
        }
        Some(key)
    }

    pub(super) fn session_options(session: &Session) -> Result<Vec<String>, String> {
        if !["ignore", "tofu", "deny"].contains(&session.rdp_cert_policy.as_str())
            || !["auto", "lan", "wan", "broadband-high", "broadband-low"]
                .contains(&session.rdp_network.as_str())
        {
            return Err(super::t("RDP 常用参数无效", "invalid RDP common options").to_string());
        }
        let args = parse_extra_args(&session.rdp_extra_args)?;
        for arg in &args {
            let key = option_key(arg).expect("validated above");
            if (key == "network" && session.rdp_network != "auto")
                || (key == "sound" && session.rdp_sound)
                || (key == "gfx" && session.rdp_gfx)
            {
                return Err(format!(
                    "{}: {key}",
                    super::t(
                        "RDP 参数与常用设置冲突",
                        "RDP argument conflicts with common settings"
                    )
                ));
            }
        }
        Ok(args)
    }

    /// The command line for a FreeRDP connect.
    ///
    /// The password is deliberately absent — it goes over stdin, see
    /// [`credential_lines`]. User name and domain are not secrets: they sit in
    /// `mstsc`'s `.rdp` file on Windows and in the session list in the UI.
    pub(super) fn args(account: &Account<'_>, dialect: Dialect) -> Vec<String> {
        let mut args = vec![
            format!("/v:{}:{}", account.host, account.port),
            format!("/cert:{}", account.cert_policy),
            if account.clipboard {
                "/clipboard"
            } else {
                "-clipboard"
            }
            .to_string(),
        ];
        // Explicit compatibility flags, scoped to this one saved target.
        if account.legacy_tls {
            args.push(match dialect {
                Dialect::V3 => "/tls:seclevel:0".to_string(),
                Dialect::V2 => "/tls-seclevel:0".to_string(),
            });
        }
        if account.admin {
            args.push("/admin".to_string());
        }
        if account.network != "auto" {
            args.push(format!("/network:{}", account.network));
        }
        if account.sound {
            args.push("/sound".to_string());
        }
        if account.gfx {
            args.push("/gfx".to_string());
        }
        if account.smart_sizing {
            args.push("/smart-sizing".to_string());
        } else {
            args.push("+dynamic-resolution".to_string());
        }
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
            if account.smart_sizing {
                args.push(format!("/size:{}x{}", account.width, account.height));
            } else {
                args.push(format!("/w:{}", account.width));
                args.push(format!("/h:{}", account.height));
            }
        }
        args.extend(account.extra_args.iter().cloned());
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
                legacy_tls: false,
                admin: false,
                smart_sizing: false,
                cert_policy: "ignore",
                network: "auto",
                sound: false,
                clipboard: true,
                gfx: false,
                extra_args: Vec::new(),
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
        fn compatibility_options_are_per_server_and_never_leak_to_defaults() {
            let mut target = account();
            let default = args(&target, Dialect::V3);
            assert!(!default.contains(&"/tls:seclevel:0".to_string()));
            assert!(!default.contains(&"/admin".to_string()));
            assert!(!default.contains(&"/smart-sizing".to_string()));

            target.legacy_tls = true;
            target.admin = true;
            target.smart_sizing = true;
            let compat = args(&target, Dialect::V3);
            assert!(compat.contains(&"/tls:seclevel:0".to_string()));
            assert!(compat.contains(&"/admin".to_string()));
            assert!(compat.contains(&"/smart-sizing".to_string()));
            assert!(compat.contains(&"/size:1600x900".to_string()));
            assert!(!compat.contains(&"+dynamic-resolution".to_string()));
            assert!(!compat.iter().any(|arg| arg.contains("s3cret")));
            let compat_v2 = args(&target, Dialect::V2);
            assert!(compat_v2.contains(&"/tls-seclevel:0".to_string()));
            assert!(!compat_v2.contains(&"/tls:seclevel:0".to_string()));
        }

        #[test]
        fn bilingual_extra_args_translate_without_shell_or_credentials() {
            let input =
                "网络=局域网\n音频=开\n麦克风=开\n色深=24\n共享目录=资料,/tmp/中文 路径\n/gfx";
            assert_eq!(
                parse_extra_args(input).unwrap(),
                vec![
                    "/network:lan",
                    "/sound",
                    "/microphone",
                    "/bpp:24",
                    "/drive:资料,/tmp/中文 路径",
                    "/gfx"
                ]
            );
            assert_eq!(
                parse_extra_args("network=wan\nsound=on").unwrap(),
                vec!["/network:wan", "/sound"]
            );
        }

        #[test]
        fn multiple_named_drive_shares_and_equals_in_paths_are_allowed() {
            assert_eq!(
                parse_extra_args("/drive:one,/tmp/a=b\n共享目录=二,/tmp/中文 路径").unwrap(),
                vec!["/drive:one,/tmp/a=b", "/drive:二,/tmp/中文 路径"]
            );
            assert!(parse_extra_args("/drive:one,/tmp/a\n/drive:one,/tmp/b").is_err());
        }

        #[test]
        fn extra_args_reject_sensitive_conflicting_duplicate_and_shell_syntax() {
            for input in [
                "/p:secret",
                "/password:secret",
                "/gateway:g:host,p:secret",
                "/proxy:socks5://u:p@host",
                "/v:evil",
                "/u:other",
                "/from-stdin",
                "/cert:ignore",
                "/size:100x100",
                "/tls:seclevel:0",
                "/network:lan\n/network:wan",
                "network=lan\n/network:wan",
                "/gfx;touch /tmp/x",
                "not-an-option",
                "网络=未知",
            ] {
                assert!(parse_extra_args(input).is_err(), "accepted {input:?}");
            }
        }

        #[test]
        fn common_options_and_extra_args_are_per_session() {
            let mut target = account();
            target.cert_policy = "tofu";
            target.network = "wan";
            target.sound = true;
            target.clipboard = false;
            target.gfx = true;
            target.extra_args = parse_extra_args("/bpp:24").unwrap();
            let selected = args(&target, Dialect::V3);
            for option in [
                "/cert:tofu",
                "/network:wan",
                "/sound",
                "-clipboard",
                "/gfx",
                "/bpp:24",
            ] {
                assert!(selected.contains(&option.to_string()), "missing {option}");
            }
            let defaults = args(&account(), Dialect::V3);
            assert!(defaults.contains(&"/cert:ignore".to_string()));
            assert!(defaults.contains(&"/clipboard".to_string()));
            assert!(!defaults.contains(&"/bpp:24".to_string()));
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

    /// Default FreeRDP-era options for the mstsc tests, which only care about
    /// connection, credential and window-size fields.
    #[cfg(windows)]
    fn test_account_defaults() -> Account<'static> {
        Account {
            host: "",
            port: 3389,
            user: "",
            domain: "",
            password: "",
            fullscreen: false,
            width: 1280,
            height: 720,
            legacy_tls: false,
            admin: false,
            smart_sizing: false,
            cert_policy: "ignore",
            network: "auto",
            sound: false,
            clipboard: true,
            gfx: false,
            extra_args: Vec::new(),
        }
    }

    #[test]
    fn legacy_session_json_defaults_to_secure_compatibility_settings() {
        let mut old = serde_json::to_value(Session::new_empty()).unwrap();
        for key in [
            "rdp_legacy_tls",
            "rdp_admin",
            "rdp_smart_sizing",
            "rdp_cert_policy",
            "rdp_network",
            "rdp_sound",
            "rdp_clipboard",
            "rdp_gfx",
            "rdp_extra_args",
        ] {
            old.as_object_mut().unwrap().remove(key);
        }
        let session: Session = serde_json::from_value(old).unwrap();
        assert!(!session.rdp_legacy_tls);
        assert!(!session.rdp_admin);
        assert!(!session.rdp_smart_sizing);
        assert_eq!(session.rdp_cert_policy, "ignore");
        assert_eq!(session.rdp_network, "auto");
        assert!(!session.rdp_sound && session.rdp_clipboard && !session.rdp_gfx);
        assert!(session.rdp_extra_args.is_empty());

        let mut selected = session;
        selected.rdp_legacy_tls = true;
        selected.rdp_admin = true;
        selected.rdp_smart_sizing = true;
        selected.rdp_cert_policy = "tofu".into();
        selected.rdp_network = "wan".into();
        selected.rdp_extra_args = "网络=局域网".into();
        selected.rdp_sound = true;
        selected.rdp_clipboard = false;
        selected.rdp_gfx = true;
        let saved: Session =
            serde_json::from_value(serde_json::to_value(selected).unwrap()).unwrap();
        assert!(saved.rdp_legacy_tls && saved.rdp_admin && saved.rdp_smart_sizing);
        assert_eq!(saved.rdp_cert_policy, "tofu");
        assert_eq!(saved.rdp_network, "wan");
        assert_eq!(saved.rdp_extra_args, "网络=局域网");
        assert!(saved.rdp_sound && !saved.rdp_clipboard && saved.rdp_gfx);
    }

    // FreeRDP-only: `preview_args` deliberately refuses on Windows (mstsc).
    #[test]
    #[cfg(not(windows))]
    fn preview_never_contains_the_saved_password_and_validation_blocks_conflicts() {
        let mut session = Session::new_empty();
        session.host = "10.0.0.5".into();
        session.user = "alice".into();
        session.password = crate::config::Secret::new("super-secret");
        session.rdp_extra_args = "网络=局域网\n/bpp:24".into();
        let preview = preview_args(&session).unwrap();
        assert!(preview.contains("/network:lan") && preview.contains("/bpp:24"));
        assert!(!preview.contains("super-secret"));
        session.rdp_network = "wan".into();
        assert!(validate_options(&session).is_err());
    }

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
        assert_eq!(split_user_domain(""), (String::new(), String::new()));
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
            legacy_tls: false,
            admin: false,
            smart_sizing: false,
            cert_policy: "ignore",
            network: "auto",
            sound: false,
            clipboard: true,
            gfx: false,
            extra_args: Vec::new(),
        };
        let contents = rdp_file_contents(&account, false);
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
            ..test_account_defaults()
        };
        let contents = rdp_file_contents(&account, false);
        assert!(contents.contains("screen mode id:i:2\r\n"));
        assert!(!contents.contains("desktopwidth:i:"));
        assert!(!contents.contains("desktopheight:i:"));
        // An empty user name must not be written as `username:s:`.
        assert!(!contents.contains("username:s:"));
    }

    /// The password never goes into the `.rdp` file: with a saved credential
    /// mstsc must not prompt, without one it must.
    #[test]
    #[cfg(windows)]
    fn rdp_file_never_contains_the_password() {
        let account = Account {
            host: "server",
            port: 3389,
            user: "alice",
            domain: "",
            password: "p@ssw0rd-示例",
            fullscreen: false,
            width: 1280,
            height: 720,
            ..test_account_defaults()
        };
        let saved = rdp_file_contents(&account, true);
        assert!(saved.contains("prompt for credentials:i:0\r\n"));
        let unsaved = rdp_file_contents(&account, false);
        assert!(unsaved.contains("prompt for credentials:i:1\r\n"));
        for contents in [saved, unsaved] {
            assert!(!contents.contains("password"));
            assert!(!contents.contains("p@ssw0rd"));
        }
    }

    #[test]
    #[cfg(windows)]
    fn credentials_use_mstsc_targets_and_user_names() {
        assert_eq!(credential_targets("10.0.0.5", 3389), ["TERMSRV/10.0.0.5"]);
        assert_eq!(
            credential_targets("ubuntu.lan", 3390),
            ["TERMSRV/ubuntu.lan", "TERMSRV/ubuntu.lan:3390"]
        );
        let mut account = Account {
            host: "h",
            port: 3389,
            user: "alice",
            domain: "",
            password: "x",
            fullscreen: false,
            width: 1280,
            height: 720,
            ..test_account_defaults()
        };
        assert_eq!(credential_user(&account), "alice");
        account.domain = "CONTOSO";
        assert_eq!(credential_user(&account), "CONTOSO\\alice");
        // Nothing to save without a user or a password: mstsc prompts.
        account.password = "";
        assert!(!save_credential(&account));
    }

    /// Writing the credential must succeed for this Windows user; the test
    /// target is removed again so Credential Manager is left as it was.
    #[test]
    #[cfg(windows)]
    fn credential_manager_accepts_the_saved_password() {
        let target = format!("TERMSRV/meatshell-test-{}.invalid", uuid::Uuid::new_v4());
        assert_eq!(
            credman::write_domain_password(&target, "alice", "p@ssw0rd-示例"),
            Ok(())
        );
        assert!(credman::delete_domain_password(&target));
    }
}
