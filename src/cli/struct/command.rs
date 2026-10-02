#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CliCommand {
    Sessions,
    Session,
    Import,
    Exec,
    Files,
    Read,
    Upload,
    Download,
    Help,
}

impl CliCommand {
    pub(crate) fn parse(value: Option<&str>) -> Option<Self> {
        match value.unwrap_or("help") {
            "sessions" => Some(Self::Sessions),
            "session" => Some(Self::Session),
            "import" => Some(Self::Import),
            "exec" => Some(Self::Exec),
            "files" => Some(Self::Files),
            "read" => Some(Self::Read),
            "upload" => Some(Self::Upload),
            "download" => Some(Self::Download),
            "help" | "--help" | "-h" => Some(Self::Help),
            _ => None,
        }
    }
}
