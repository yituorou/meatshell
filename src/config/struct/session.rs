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
    false
}

fn default_rdp_width() -> u16 {
    1280
}

fn default_rdp_height() -> u16 {
    720
}

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
    /// Single hop only; the jump session supplies its own host/user/auth (#211).
    #[serde(default)]
    pub jump_session_id: String,
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

    /// Character encoding used by the interactive terminal stream (#338).
    /// UTF-8 remains the default for existing and newly created sessions.
    #[serde(default = "default_encoding")]
    pub encoding: String,

    /// Honor VT100 line drawing (DEC Special Graphics via `ESC ( 0` and SO/SI)
    /// even when the encoding is UTF-8 (#376). PuTTY's "Enable VT100 line
    /// drawing even in UTF-8 mode"; opt-in, off for new/existing sessions.
    #[serde(default = "default_vt100_drawing")]
    pub vt100_drawing: bool,

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
    pub kind: String,
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
        for mode in [SessionLogMode::Default, SessionLogMode::On, SessionLogMode::Off] {
            assert_eq!(SessionLogMode::from_str(mode.as_str()), mode);
        }
    }
}

impl Session {
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
            last_used: None,
            group: String::new(),
            kind: SessionKind::Ssh,
            local_distribution: String::new(),
            local_working_dir: String::new(),
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
            encoding: default_encoding(),
            vt100_drawing: default_vt100_drawing(),
            session_log: SessionLogMode::Default,
            forwards: Vec::new(),
            triggers: Vec::new(),
            disable_shell_integration: false,
            note: String::new(),
        }
    }
}
