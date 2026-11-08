use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::Secret;

/// Which transport a session uses.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    /// SSH shell + SFTP (the original and default behaviour).
    #[default]
    Ssh,
    /// Local serial port (COM3 / /dev/ttyUSB0) for switches, routers, MCUs (#14).
    Serial,
    /// Plain Telnet over TCP, for legacy network gear (#17).
    Telnet,
    /// Local shell process on this machine (PowerShell/CMD/WSL/$SHELL).
    Local,
    /// Embedded Microsoft Remote Desktop (RDP) session, rendered in a tab like
    /// FinalShell's "远程桌面连接".
    Rdp,
}

impl SessionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionKind::Ssh => "ssh",
            SessionKind::Serial => "serial",
            SessionKind::Telnet => "telnet",
            SessionKind::Local => "local",
            SessionKind::Rdp => "rdp",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "serial" => SessionKind::Serial,
            "telnet" => SessionKind::Telnet,
            "local" => SessionKind::Local,
            "rdp" => SessionKind::Rdp,
            _ => SessionKind::Ssh,
        }
    }
}

fn default_baud() -> u32 {
    115_200
}
fn default_data_bits() -> u8 {
    8
}
fn default_stop_bits() -> u8 {
    1
}
fn default_parity() -> String {
    "none".to_string()
}

fn default_flow() -> String {
    "none".to_string()
}

fn default_encoding() -> String {
    "UTF-8".to_string()
}

fn default_vt100_drawing() -> bool {
    true
}

/// Terminal type reported to the server (#498): the SSH `pty-req` TERM, the
/// Telnet TERMINAL-TYPE reply (RFC 1091) and `$TERM` for local shells.
pub const DEFAULT_TERM_TYPE: &str = "xterm-256color";

fn default_term_type() -> String {
    DEFAULT_TERM_TYPE.to_string()
}

/// A usable terminal type, or the default. Terminfo names are short ASCII
/// words (`xterm-256color`, `vt100`, `screen.xterm-256color`); anything else —
/// empty, oversized, spaces or control bytes from a hand-edited config — falls
/// back to the default rather than being sent to the server.
pub fn normalize_term_type(raw: &str) -> &str {
    let term = raw.trim();
    let valid = !term.is_empty()
        && term.len() <= 64
        && term
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'));
    if valid {
        term
    } else {
        DEFAULT_TERM_TYPE
    }
}

fn default_rdp_width() -> u16 {
    1280
}

fn default_rdp_height() -> u16 {
    720
}
fn default_rdp_cert_policy() -> String { "ignore".into() }
fn default_rdp_network() -> String { "auto".into() }
fn default_rdp_clipboard() -> bool { true }

/// Per-session override of the global session-log setting (#265).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SessionLogMode {
    /// Follow the global "record session logs" setting.
    #[default]
    Default,
    /// Always log this session.
    On,
    /// Never log this session.
    Off,
}

impl SessionLogMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionLogMode::Default => "default",
            SessionLogMode::On => "on",
            SessionLogMode::Off => "off",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "on" => SessionLogMode::On,
            "off" => SessionLogMode::Off,
            _ => SessionLogMode::Default,
        }
    }

    /// Effective on/off given the global default.
    pub fn resolve(self, global_enabled: bool) -> bool {
        match self {
            SessionLogMode::Default => global_enabled,
            SessionLogMode::On => true,
            SessionLogMode::Off => false,
        }
    }
}

/// How a session authenticates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethod {
    Password,
    #[serde(rename = "keyboard-interactive")]
    KeyboardInteractive,
    Key,
}

impl AuthMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthMethod::Password => "password",
            AuthMethod::KeyboardInteractive => "keyboard-interactive",
            AuthMethod::Key => "key",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "keyboard-interactive" | "keyboard" | "interactive" => AuthMethod::KeyboardInteractive,
            "key" => AuthMethod::Key,
            _ => AuthMethod::Password,
        }
    }
}

/// A single saved SSH target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    #[serde(default)]
    pub password: Secret,
    #[serde(default)]
    pub private_key_path: String,
    #[serde(default)]
    pub private_key_inline: Secret,
    /// Optional outbound proxy, e.g. "socks5://127.0.0.1:1080" or
    /// "http://user:pass@host:8080". Empty = use $ALL_PROXY, else direct.
    #[serde(default)]
    pub proxy: String,
    /// Optional SSH jump host (bastion): the id of another saved SSH session to
    /// tunnel this connection through, like OpenSSH's ProxyJump. Empty = direct.
    /// The jump session may reference another jump. The full chain is validated
    /// before connecting; each hop supplies its own host/user/auth (#211).
    #[serde(default)]
    pub jump_session_id: String,
    /// Explicit connection order (first hop first). When non-empty, this list
    /// replaces inherited jump references without modifying the saved hops.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jump_session_ids: Vec<String>,
    /// Opt in to revealing saved credentials in the editor; always masked on open.
    #[serde(default)]
    pub allow_secret_reveal: bool,
    /// Expose this session to MCP clients (#432). When false, MCP tools treat
    /// it as nonexistent and refuse routes that use it as a jump host. The CLI
    /// is unaffected. Absent in older configs → true, preserving behaviour.
    #[serde(default = "default_true")]
    pub mcp_access: bool,
    #[serde(default)]
    pub last_used: Option<String>,
    /// Optional folder/group name to organize sessions in the list (#41).
    /// Empty = ungrouped. Sessions are grouped by this in Quick Connect.
    #[serde(default)]
    pub group: String,

    // --- Transport ----------------------------------------------------------
    /// SSH (default), Serial, or Telnet. Absent in old config files → Ssh.
    #[serde(default)]
    pub kind: SessionKind,

    /// WSL distribution and startup directory for generated local sessions.
    /// The directory defaults to the selected distribution user's home (`~`).
    #[serde(default)]
    pub local_distribution: String,
    #[serde(default)]
    pub local_working_dir: String,
    /// Program a generated local session runs inside its shell, e.g. the
    /// resolved path of an agent CLI launched from the welcome page (#427).
    /// The shell stays open after the program exits. Never set on saved
    /// sessions, so it is left out of the config file when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub local_command: String,

    // --- Serial-only fields (ignored unless kind == Serial) -----------------
    /// Serial device path, e.g. "COM3" (Windows) or "/dev/ttyUSB0" (Linux).
    #[serde(default)]
    pub serial_port: String,
    #[serde(default = "default_baud")]
    pub baud_rate: u32,
    #[serde(default = "default_data_bits")]
    pub data_bits: u8,
    #[serde(default = "default_stop_bits")]
    pub stop_bits: u8,
    /// "none" | "odd" | "even".
    #[serde(default = "default_parity")]
    pub parity: String,
    /// "none" | "hardware" | "software".
    #[serde(default = "default_flow")]
    pub flow_control: String,

    // --- RDP-only fields (ignored unless kind == Rdp) -----------------------
    /// Windows logon domain (or "." for the local machine); empty = none. A
    /// "DOMAIN\user" style username is split into this when the connection is
    /// handed to the system RDP client.
    #[serde(default)]
    pub rdp_domain: String,
    /// Ask the system client to start the desktop full screen. It then uses the
    /// local monitor resolution, which keeps the image pixel-exact; the size
    /// below is only used when this is false.
    #[serde(default)]
    pub rdp_fullscreen: bool,
    /// Remote desktop size in pixels for a windowed session.
    #[serde(default = "default_rdp_width")]
    pub rdp_width: u16,
    #[serde(default = "default_rdp_height")]
    pub rdp_height: u16,
    /// Explicit per-server opt-in: TLS security level 0 weakens encryption.
    #[serde(default)]
    pub rdp_legacy_tls: bool,
    /// Request the administrative session on the remote server.
    #[serde(default)]
    pub rdp_admin: bool,
    /// Scale fixed resolution instead of dynamically changing remote resolution.
    #[serde(default)]
    pub rdp_smart_sizing: bool,
    /// FreeRDP-specific options; ignored by Windows mstsc.
    #[serde(default = "default_rdp_cert_policy")]
    pub rdp_cert_policy: String,
    #[serde(default = "default_rdp_network")]
    pub rdp_network: String,
    #[serde(default)]
    pub rdp_sound: bool,
    #[serde(default = "default_rdp_clipboard")]
    pub rdp_clipboard: bool,
    #[serde(default)]
    pub rdp_gfx: bool,
    /// One FreeRDP argument or supported bilingual alias per line.
    #[serde(default)]
    pub rdp_extra_args: String,

    /// Character encoding used by the interactive terminal stream (#338).
    /// UTF-8 remains the default for existing and newly created sessions.
    #[serde(default = "default_encoding")]
    pub encoding: String,

    /// Honor VT100 line drawing (DEC Special Graphics via `ESC ( 0` and SO/SI)
    /// even when the encoding is UTF-8 (#376). PuTTY's "Enable VT100 line
    /// drawing even in UTF-8 mode". On by default like xterm, VTE and Windows
    /// Terminal, so ncurses borders (nvtop, mc) aren't shown as `lqqk` (#543).
    /// It was opt-in under the key `vt100_drawing`; a stored `false` there was
    /// almost always the old default, so that key is no longer read and every
    /// session starts with line drawing on. Turning it off is saved under
    /// the new key.
    #[serde(rename = "vt100_line_drawing", default = "default_vt100_drawing")]
    pub vt100_drawing: bool,

    /// Terminal type (`TERM`) announced to the server (#498). Some programs
    /// (e.g. older telnet targets) only work with plain `xterm`. Absent in
    /// older configs → `xterm-256color`, the value always used before.
    #[serde(default = "default_term_type")]
    pub term_type: String,

    /// Record this session's terminal output to a log file (#265):
    /// follow the global setting, or force on/off.
    #[serde(default)]
    pub session_log: SessionLogMode,

    // --- SSH port forwarding / tunnels (#56) --------------------------------
    /// Tunnels established automatically when this SSH session connects.
    #[serde(default)]
    pub forwards: Vec<PortForward>,

    /// Expect/send rules evaluated against interactive terminal output (#212).
    #[serde(default)]
    pub triggers: Vec<SessionTrigger>,

    /// Skip shell-integration setup (the cwd-follow hook + remote resource
    /// monitor). Those assume a POSIX shell and can interfere with Windows
    /// shells or interactive auto-login scripts. SFTP remains available.
    #[serde(default)]
    pub disable_shell_integration: bool,
    /// Who the SFTP panel works as (#438): "" = the login user (the `sftp`
    /// subsystem), "sudo" = root, by running sftp-server through sudo, or
    /// "custom" = run `sftp_command` as the SFTP server, like WinSCP's
    /// "SFTP server" setting.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sftp_mode: String,
    /// SFTP server command for `sftp_mode == "custom"`, e.g.
    /// `sudo -u www-data /usr/lib/openssh/sftp-server`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sftp_command: String,
    /// Free-form note for this session — somewhere to stash extra info (jump-host
    /// details, credentials hints, owner, etc.). Shown only in the edit dialog.
    /// (B站 suggestion)
    #[serde(default)]
    pub note: String,
}

/// One SSH tunnel (#56). `kind` is "local" (-L), "remote" (-R) or
/// "dynamic" (-D / SOCKS5). For local/remote, `host:host_port` is the target;
/// for dynamic it is ignored (the SOCKS client picks the destination).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PortForward {
    /// Stable identifier for a saved rule, including across edits and deletes.
    #[serde(default)]
    pub id: String,
    pub kind: String,
    /// Start this saved tunnel when the SSH connection is established.
    #[serde(default = "default_forward_auto_start")]
    pub auto_start: bool,
    /// Optional label to tell rules apart (#100). Empty = unnamed.
    #[serde(default)]
    pub name: String,
    /// Listener bind address (local side for L/D, remote side for R).
    /// Empty → 127.0.0.1.
    #[serde(default)]
    pub bind_addr: String,
    pub bind_port: u16,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub host_port: u16,
}

fn default_forward_auto_start() -> bool {
    true
}

/// Automatically send a response when literal terminal output is observed.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionTrigger {
    pub expect: String,
    #[serde(default)]
    pub response: Secret,
    #[serde(default = "default_true")]
    pub append_enter: bool,
    /// False means the rule is consumed after its first match.
    #[serde(default)]
    pub repeat: bool,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod session_log_mode_tests {
    use super::*;

    #[test]
    fn resolves_against_global_default() {
        assert!(SessionLogMode::Default.resolve(true));
        assert!(!SessionLogMode::Default.resolve(false));
        assert!(SessionLogMode::On.resolve(false));
        assert!(!SessionLogMode::Off.resolve(true));
    }

    #[test]
    fn missing_field_deserializes_as_default() {
        let mut value = serde_json::to_value(Session::new_empty()).unwrap();
        value.as_object_mut().unwrap().remove("session_log");
        let session: Session = serde_json::from_value(value).unwrap();
        assert_eq!(session.session_log, SessionLogMode::Default);
    }

    #[test]
    fn round_trips_through_strings() {
        for mode in [
            SessionLogMode::Default,
            SessionLogMode::On,
            SessionLogMode::Off,
        ] {
            assert_eq!(SessionLogMode::from_str(mode.as_str()), mode);
        }
    }
}

#[cfg(test)]
mod term_type_tests {
    use super::*;

    #[test]
    fn keeps_valid_terminfo_names() {
        for term in [
            "xterm",
            "xterm-256color",
            "vt100",
            "screen.xterm-256color",
            "rxvt-unicode+x",
        ] {
            assert_eq!(normalize_term_type(term), term);
        }
        assert_eq!(normalize_term_type("  xterm  "), "xterm");
    }

    #[test]
    fn falls_back_to_default_for_unusable_values() {
        let long = "x".repeat(65);
        for term in [
            "",
            "   ",
            "xterm 256",
            "x\u{1b}term",
            "xterm;rm",
            long.as_str(),
        ] {
            assert_eq!(normalize_term_type(term), DEFAULT_TERM_TYPE);
        }
    }

    #[test]
    fn missing_field_deserializes_as_default() {
        let mut value = serde_json::to_value(Session::new_empty()).unwrap();
        value.as_object_mut().unwrap().remove("term_type");
        let session: Session = serde_json::from_value(value).unwrap();
        assert_eq!(session.term_type, DEFAULT_TERM_TYPE);
    }
}

impl Session {
    /// The terminal type to announce for this session (#498).
    pub fn effective_term_type(&self) -> &str {
        normalize_term_type(&self.term_type)
    }

    pub fn new_empty() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: String::new(),
            host: String::new(),
            port: 22,
            user: "root".into(),
            auth: AuthMethod::Password,
            password: Secret::default(),
            private_key_path: String::new(),
            private_key_inline: Secret::default(),
            proxy: String::new(),
            jump_session_id: String::new(),
            jump_session_ids: Vec::new(),
            allow_secret_reveal: false,
            mcp_access: true,
            last_used: None,
            group: String::new(),
            kind: SessionKind::Ssh,
            local_distribution: String::new(),
            local_working_dir: String::new(),
            local_command: String::new(),
            serial_port: String::new(),
            baud_rate: default_baud(),
            data_bits: default_data_bits(),
            stop_bits: default_stop_bits(),
            parity: default_parity(),
            flow_control: default_flow(),
            rdp_domain: String::new(),
            rdp_fullscreen: false,
            rdp_width: default_rdp_width(),
            rdp_height: default_rdp_height(),
            rdp_legacy_tls: false,
            rdp_admin: false,
            rdp_smart_sizing: false,
            rdp_cert_policy: default_rdp_cert_policy(),
            rdp_network: default_rdp_network(),
            rdp_sound: false,
            rdp_clipboard: true,
            rdp_gfx: false,
            rdp_extra_args: String::new(),
            encoding: default_encoding(),
            vt100_drawing: default_vt100_drawing(),
            term_type: default_term_type(),
            session_log: SessionLogMode::Default,
            forwards: Vec::new(),
            triggers: Vec::new(),
            disable_shell_integration: false,
            sftp_mode: String::new(),
            sftp_command: String::new(),
            note: String::new(),
        }
    }
}
