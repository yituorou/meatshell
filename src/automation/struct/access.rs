/// Entry point invoking the shared MeatShell automation capabilities.
///
/// MCP applies persisted permission gates because an external agent initiates
/// calls. CLI commands are explicit local user actions and therefore do not
/// depend on whether the MCP server itself is enabled.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Frontend {
    Mcp { allow_config_import: bool },
    Cli,
}

impl Frontend {
    pub(crate) fn is_mcp(self) -> bool {
        matches!(self, Self::Mcp { .. })
    }

    pub(crate) fn allows_config_import(self) -> bool {
        matches!(
            self,
            Self::Cli
                | Self::Mcp {
                    allow_config_import: true
                }
        )
    }
}
