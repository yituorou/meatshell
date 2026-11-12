use serde::{Deserialize, Serialize};

use super::{DockEdgeSer, OutputHighlightRule, QuickCommand, Secret, Session};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WslProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub distribution: String,
    #[serde(default = "default_wsl_home")]
    pub directory: String,
}

fn default_wsl_home() -> String {
    "~".to_string()
}

/// A user-defined agent launcher (#427), for agent CLIs meatshell does not
/// detect by itself. `command` runs inside a local shell tab (PowerShell on
/// Windows, the login shell elsewhere), optionally starting in `directory`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomAgent {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub directory: String,
}

/// A user-defined local shell (#538), e.g. nushell, MSYS2 zsh or fish. Unlike
/// a custom agent, `program` is started directly as the tab's shell (no
/// PowerShell around it), with `args` (one line, split like a command line)
/// and an optional start `directory`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomShell {
    pub id: String,
    pub name: String,
    pub program: String,
    #[serde(default)]
    pub args: String,
    #[serde(default)]
    pub directory: String,
}

// Testing-stage MCP defaults. Switch these serde defaults to false before the
// feature is promoted from preview to a stable release.
fn default_mcp_preview_enabled() -> bool {
    true
}

/// Ships with the "幻想 3048" sci-fi wallpaper on by default (a dark theme). New
/// installs and users upgrading from before the wallpaper feature get it; once
/// the user picks anything (including "无"/none, stored as ""), their choice is
/// saved and sticks.
fn default_wallpaper() -> String {
    // Serde default for the `wallpaper` field: kept at the old "幻想 3048" so an
    // *existing* config that predates the field stays on tech — `migrate_defaults`
    // then advances default-following users through the migration chain. Brand-new
    // installs get the current default straight from `fresh_config`.
    "builtin:tech".to_string()
}

/// Bump when `migrate_defaults` gains a new one-time default-layout change.
pub const DEFAULTS_REV: u32 = 3;

pub(crate) const PREVIOUS_DEFAULT_WALLPAPER_TRANSPARENCY: f32 = 0.38;
pub(crate) const PREVIOUS_DEFAULT_WALLPAPER_OVERLAY: f32 =
    1.0 - PREVIOUS_DEFAULT_WALLPAPER_TRANSPARENCY;
pub(crate) const DEFAULT_WALLPAPER_TRANSPARENCY: f32 = 0.15;
pub(crate) const DEFAULT_WALLPAPER_OVERLAY: f32 = 1.0 - DEFAULT_WALLPAPER_TRANSPARENCY;

pub(crate) fn default_sidebar_width() -> f32 {
    220.0
}
pub(crate) fn default_sidebar_height() -> f32 {
    240.0
}
pub(crate) fn default_sftp_width() -> f32 {
    380.0
}
pub(crate) fn default_sftp_height() -> f32 {
    220.0
}
pub(crate) fn default_sftp_tree_width() -> f32 {
    160.0
}

pub(crate) fn default_sftp_visible_columns() -> Vec<String> {
    ["name", "type", "size", "modified", "permissions", "owner", "group"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub(crate) fn default_quick_panel_width() -> f32 {
    260.0
}

pub(crate) fn default_quick_panel_height() -> f32 {
    220.0
}

/// On-disk layout. Keep additive to ease forward-compat.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigFile {
    #[serde(default)]
    pub sessions: Vec<Session>,
    /// User-managed WSL launch entries. An empty list keeps the implicit default
    /// WSL entry for backwards compatibility.
    #[serde(default)]
    pub wsl_profiles: Vec<WslProfile>,
    /// User-defined agent launchers shown next to the detected ones (#427).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_agents: Vec<CustomAgent>,
    /// User-defined local shells listed with the built-in ones (#538).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_shells: Vec<CustomShell>,
    /// Preset SFTP download directory. Empty = ask each time.
    #[serde(default)]
    pub download_dir: String,
    /// UI language code: "zh" (default) or "en".
    #[serde(default)]
    pub language: String,
    /// Theme preference: "system" (default) | "dark" | "light".
    #[serde(default)]
    pub theme_pref: String,
    /// Platform renderer preference. Windows uses software/auto/gpu; macOS uses
    /// software/femtovg/skia. Missing or foreign-platform values use the platform default.
    #[serde(default)]
    pub renderer_mode: String,
    /// Terminal font family. Empty = the built-in default ("Meatshell Mono").
    #[serde(default)]
    pub font_family: String,
    /// Terminal font size in px. 0 = the built-in default.
    #[serde(default)]
    pub font_size: u32,
    /// Terminal line-height multiplier. 0 means the default 1.0.
    #[serde(default)]
    pub terminal_line_spacing: f32,
    /// Force regular terminal text to render with a bold face (#262).
    #[serde(default)]
    pub terminal_bold: bool,
    /// Terminal insertion cursor shape: block (default), bar, or underline (#275).
    #[serde(default)]
    pub terminal_cursor_style: String,
    /// Custom terminal cursor colour as #RRGGBB. Empty follows the theme (#275).
    #[serde(default)]
    pub terminal_cursor_color: String,
    /// Stored inverted so missing/legacy config keeps the automatic plain-text
    /// output highlighter enabled by default.
    #[serde(default)]
    pub output_highlight_disabled: bool,
    /// Built-in output highlight preset: "log" (default) or "devops".
    #[serde(default)]
    pub output_highlight_preset: String,
    /// User-defined rules applied before the selected built-in preset.
    #[serde(default)]
    pub output_highlight_rules: Vec<OutputHighlightRule>,
    /// Stored inverted so complete JSON lines are formatted and syntax-coloured
    /// by default while still allowing users to preserve byte-for-byte display.
    #[serde(default)]
    pub json_format_disabled: bool,
    /// Record terminal output of new tabs to plain-text session logs (#265).
    /// Off by default; saved sessions can override it either way.
    #[serde(default)]
    pub session_log_enabled: bool,
    /// Folder for session logs. Empty = `<log dir>/sessions`.
    #[serde(default)]
    pub session_log_dir: String,
    /// Global UI scale in percent (#100). 0 = default (100%).
    #[serde(default)]
    pub ui_scale: u32,
    /// Immersive wallpaper id: "" = none, "builtin:light" / "builtin:dark" /
    /// "builtin:tech", or a filesystem path to a custom image. Drives the
    /// wallpaper + tinted theme. Defaults to the "幻想 3048" built-in.
    #[serde(default = "default_wallpaper")]
    pub wallpaper: String,
    /// Explicit session groups/folders (#41), including empty ones so a folder
    /// can exist before any session is moved into it. "default" is implicit and
    /// not stored here.
    #[serde(default)]
    pub groups: Vec<String>,
    /// Quick Connect folders that were collapsed when the UI was last used.
    /// `None` is a legacy/new config and starts with every folder collapsed;
    /// `Some([])` means the user explicitly expanded every folder.
    #[serde(default)]
    pub collapsed_session_groups: Option<Vec<String>>,
    /// Stored inverted ("don't follow") so both serde and the Default derive
    /// yield `false` = the feature defaults to ON: the SFTP panel follows the
    /// terminal's cd (OSC 7) unless the user opts out in Interface settings.
    #[serde(default)]
    pub sftp_no_follow_cd: bool,
    /// Always prompt for the save location on each download instead of using the
    /// preset download dir. Defaults to false (#87).
    #[serde(default)]
    pub download_always_ask: bool,
    /// Hide the quick-command bar under the terminal. Defaults to false.
    #[serde(default)]
    pub hide_cmd_bar: bool,
    /// Stored inverted so multiline paste confirmation remains enabled for
    /// existing configurations unless the user explicitly disables it (#300).
    #[serde(default)]
    pub paste_confirm_disabled: bool,
    /// Right-click copies a selection or pastes when no selection exists.
    #[serde(default)]
    pub right_click_copy: bool,
    /// Stored inverted so Ctrl+Alt+V, Shift+Insert, and middle-click paste stay
    /// enabled for existing users (#300).
    #[serde(default)]
    pub extra_paste_shortcuts_disabled: bool,
    /// User-customised keyboard shortcuts: action id → chord string such as
    /// "Ctrl+Shift+C". Only overrides are stored; an action missing here uses
    /// its built-in (platform-specific) default, so "reset all" is just
    /// clearing the map.
    #[serde(default)]
    pub keybindings: std::collections::BTreeMap<String, String>,
    /// SFTP path bookmarks + recent folders, keyed by session id (#500).
    /// Sessions with nothing saved are pruned.
    #[serde(default)]
    pub sftp_places: std::collections::BTreeMap<String, super::SftpPlaces>,
    /// Open SFTP text files in a panel docked inside the main window instead of
    /// the detached editor window. Defaults to false (#494).
    #[serde(default)]
    pub editor_embedded: bool,
    /// Hide auxiliary panels and edge strips so the terminal fills the window.
    #[serde(default)]
    pub zen_mode: bool,
    /// Saved quick commands (#55).
    #[serde(default)]
    pub quick_commands: Vec<QuickCommand>,
    /// Explicit quick-command group names — mirrors `groups` for sessions so that
    /// empty quick-command groups survive and can be renamed/deleted (#55).
    #[serde(default)]
    pub quick_groups: Vec<String>,
    /// Opt-in docked quick-command sidebar (#215). The command-bar popup remains
    /// available until the user actually drags it into the main dock layer.
    #[serde(default)]
    pub quick_commands_as_sidebar: bool,
    #[serde(default)]
    pub quick_panel_open: bool,
    #[serde(default)]
    pub quick_panel_collapsed: bool,
    #[serde(default = "default_quick_panel_width")]
    pub quick_panel_width: f32,
    #[serde(default = "default_quick_panel_height")]
    pub quick_panel_height: f32,
    #[serde(default)]
    pub quick_panel_dock: String,
    /// Recent commands sent from the command box, oldest first, capped (#55).
    #[serde(default)]
    pub command_history: Vec<String>,
    /// Collapse the left resource sidebar on startup (#78).
    #[serde(default)]
    pub collapse_sidebar_default: bool,
    /// Last resource-sidebar collapsed state. None means fall back to
    /// `collapse_sidebar_default` for older configs.
    #[serde(default)]
    pub sidebar_collapsed: Option<bool>,
    /// User-adjustable width of the left resource sidebar, in logical pixels.
    /// Persisted across restarts so the drag-resized width sticks.
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    /// Resource-panel docking: size when docked top/bottom, and which edge it is
    /// docked to (left|right|top|bottom). Persisted so the layout sticks (#dock).
    #[serde(default = "default_sidebar_height")]
    pub sidebar_height: f32,
    #[serde(default)]
    pub sidebar_dock: String,
    /// SFTP-panel docking: extents (px) and docked edge, persisted (#dock).
    #[serde(default = "default_sftp_width")]
    pub sftp_panel_width: f32,
    #[serde(default = "default_sftp_height")]
    pub sftp_panel_height: f32,
    #[serde(default = "default_sftp_tree_width")]
    pub sftp_tree_width: f32,
    #[serde(default)]
    pub sftp_dock: String,
    /// Columns shown in the SFTP file list. Unknown values are ignored by the
    /// accessor so newer builds can safely read older or hand-edited configs.
    #[serde(default = "default_sftp_visible_columns")]
    pub sftp_visible_columns: Vec<String>,
    /// Split (local/remote) SFTP view: open in the split view next time (#532).
    #[serde(default)]
    pub sftp_split_enabled: bool,
    /// Last local folder the split view showed ("" = home). The pane re-checks
    /// it when it opens and falls back to home when it is gone.
    #[serde(default)]
    pub sftp_local_dir: String,
    /// Local pane's share of the SFTP panel (0.0 = even split, else 0.2–0.8).
    #[serde(default)]
    pub sftp_local_split: f32,
    /// Local folder bookmarks + recent folders for the split view (#532).
    /// Machine-wide: the same folders exist for every session.
    #[serde(default, skip_serializing_if = "super::LocalPlaces::is_empty")]
    pub local_places: super::LocalPlaces,
    /// Local pane display options (#532 follow-up): show hidden / system
    /// entries, and which column the list is sorted by.
    #[serde(default)]
    pub sftp_local_show_hidden: bool,
    #[serde(default)]
    pub sftp_local_sort_key: String,
    #[serde(default)]
    pub sftp_local_sort_dir: i32,
    /// Last window size in logical px (0 = unset → use the built-in default).
    /// Lets users keep their preferred window size across restarts.
    #[serde(default)]
    pub window_width: f32,
    #[serde(default)]
    pub window_height: f32,
    /// Collapse the bottom SFTP panel on startup (#78).
    #[serde(default)]
    pub collapse_sftp_default: bool,
    /// When session-sync is on, also mirror SFTP uploads to the other online
    /// sessions (same path, falling back to each panel's current dir).
    #[serde(default)]
    pub sync_upload: bool,
    /// WebDAV sync settings (#185). Password is encrypted at rest like session
    /// passwords; remote_path is the JSON export object path under the endpoint.
    #[serde(default)]
    pub webdav_enabled: bool,
    #[serde(default)]
    pub webdav_url: String,
    #[serde(default)]
    pub webdav_username: String,
    #[serde(default)]
    pub webdav_password: Secret,
    #[serde(default)]
    pub webdav_remote_path: String,
    #[serde(default)]
    pub webdav_accept_invalid_certs: bool,
    /// Render the welcome page (session list) as a docked left sidebar instead of
    /// a "New tab" tab (v0.5). Persisted so the layout choice sticks.
    #[serde(default)]
    pub welcome_as_sidebar: bool,
    /// Width (logical px) of the welcome/session sidebar when docked (v0.5).
    #[serde(default)]
    pub welcome_sidebar_width: f32,
    /// Welcome/session sidebar dock edge (left|right|top|bottom).
    #[serde(default)]
    pub welcome_sidebar_dock: String,
    /// Welcome sidebar collapsed to the edge icon strip (IDEA-style) (v0.5).
    /// None means the user has not explicitly collapsed/expanded it yet.
    #[serde(default)]
    pub welcome_collapsed: Option<bool>,
    /// Frosted-panel opacity over a wallpaper (0.30–1.00); user-adjustable via the
    /// Interface › Wallpaper opacity slider. 0 = use the current default.
    #[serde(default)]
    pub wallpaper_overlay: f32,
    /// Colour theme preset id (Settings › Wallpaper & theme). Empty = default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color_theme: String,
    /// `#RRGGBB` for the "custom" colour theme.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color_theme_custom: String,
    /// Turn off immersive colours: a wallpaper stays, but no longer recolours
    /// the interface or switches light/dark. Default false keeps them on.
    #[serde(default)]
    pub immersive_colors_disabled: bool,
    /// Terminal colour scheme used in dark mode (Settings › Terminal colours).
    /// Empty = the default MeatShell terminal colours.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub terminal_scheme_dark: String,
    /// Terminal colour scheme used in light mode. Empty = default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub terminal_scheme_light: String,
    /// Settings-panel font scale, percent (80–160). 0 = 100% default (v0.5).
    #[serde(default)]
    pub panel_font: u32,
    /// Turn off SSH keepalives (Settings › Connection, #515). Default false =
    /// keepalive on, as since #160.
    #[serde(default)]
    pub ssh_keepalive_disabled: bool,
    /// Seconds of server silence before a keepalive is sent. 0 = default (30).
    #[serde(default)]
    pub ssh_keepalive_interval: u32,
    /// Unanswered keepalives before the connection is treated as dead.
    /// 0 = default (3).
    #[serde(default)]
    pub ssh_keepalive_max: u32,
    /// Disable the startup "new version available" check (#184). Default false =
    /// keep checking (preserves existing behaviour for upgrading users); turning
    /// it on stops the GitHub releases query and the banner.
    #[serde(default)]
    pub update_check_disabled: bool,
    /// Download mirror prefixes for one-click updates, raced against GitHub.
    /// Used only with the Chinese UI in a mainland China time zone.
    /// `None` = the built-in list (`update::mirror::DEFAULT_MIRRORS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_mirrors: Option<Vec<String>>,
    /// The download source the user picked (a mirror prefix, `""` = GitHub).
    /// `None` = automatic: use the fastest source at download time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_mirror_choice: Option<String>,
    /// Enable the local stdio MCP server (`meatshell mcp serve`).
    #[serde(default = "default_mcp_preview_enabled")]
    pub mcp_enabled: bool,
    /// Allow MCP tools to use credentials already stored by MeatShell. Secrets
    /// remain internal and are never included in protocol responses.
    #[serde(default = "default_mcp_preview_enabled")]
    pub mcp_use_saved_credentials: bool,
    /// Allow MCP clients to execute arbitrary commands on saved SSH sessions.
    #[serde(default = "default_mcp_preview_enabled")]
    pub mcp_allow_commands: bool,
    /// Allow MCP clients to upload local files and download remote files.
    #[serde(default = "default_mcp_preview_enabled")]
    pub mcp_allow_file_transfers: bool,
    /// One-time default-layout migration marker (#new-user-defaults). 0 = config
    /// predates the migration. `migrate_defaults` bumps it to `DEFAULTS_REV` after
    /// pushing the new look (default wallpaper / welcome-as-sidebar / right-docked
    /// resource panel / wallpaper overlay) to users still sitting on old defaults.
    #[serde(default)]
    pub defaults_rev: u32,
    /// Per-edge stacks of simultaneously-expanded docked panels (#dock-stack),
    /// e.g. session list + Quick shared on the left edge, split vertically.
    /// Empty (or <2 slots on a valid edge) → the legacy single-panel-per-edge
    /// layout.
    #[serde(default)]
    pub dock_stacks: Vec<DockEdgeSer>,
}

/// Portable export file (issue #46): sessions with everything in plaintext
/// **except** the password, which is encrypted with a fixed key baked into the
/// binary so the file opens on *any* machine running meatshell.
///
/// Security note: a built-in key in open-source code is **obfuscation, not real
/// security** — anyone with the source can derive it. It only stops a casual
/// over-the-shoulder read of the file, same level as FinalShell's export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExportFile {
    /// Format marker / version so the schema can evolve later.
    pub(crate) meatshell_export: u32,
    pub(crate) sessions: Vec<Session>,
}
