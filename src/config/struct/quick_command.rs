use serde::{Deserialize, Serialize};

/// A saved quick command (#55): a named snippet the user clicks to send to the
/// active terminal.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QuickCommand {
    pub name: String,
    pub command: String,
    /// Optional group/folder name. Empty = the implicit "default" group (#55).
    #[serde(default)]
    pub group: String,
    /// Whether clicking the chip sends + executes (appends Return). `false` only
    /// drops the command into the input box to tweak first. Defaults to `true` so
    /// existing quick commands keep running on click. (B站 suggestion)
    #[serde(default = "default_true")]
    pub send_enter: bool,
}

impl QuickCommand {
    /// The stored form of a group name typed by the user (#534). The implicit
    /// group is *displayed* as "default", so typing that name (any case) means
    /// that group, stored as "". Keeping it as a real name produced a second
    /// "default" header whose members were the ungrouped commands.
    pub fn normalize_group(group: &str) -> String {
        let group = group.trim();
        if group.eq_ignore_ascii_case("default") {
            String::new()
        } else {
            group.to_string()
        }
    }
}

/// One user-defined client-side terminal highlighting rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputHighlightRule {
    pub pattern: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub whole_line: bool,
    /// Stable palette id: red | yellow | green | cyan | magenta | gray.
    #[serde(default)]
    pub color: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}
