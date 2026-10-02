use crate::ssh::{CredentialResponder, HostKeyResponder, MfaResponder};

pub(crate) struct PendingHostKey {
    /// Registry id of the window that owns this prompt's session(s); the
    /// dialog is shown there and the entry is aborted if that window closes.
    pub(crate) window_id: u64,
    /// An editor test owns its prompts independently of terminal connections.
    pub(crate) test_id: Option<u64>,
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) changed: bool,
    pub(crate) title: String,
    pub(crate) message: String,
    pub(crate) detail: String,
    pub(crate) confirm_label: String,
    pub(crate) responders: Vec<HostKeyResponder>,
}

pub(crate) struct PendingCred {
    /// Owning window's registry id (see `PendingHostKey::window_id`).
    pub(crate) window_id: u64,
    /// An editor test owns its prompts independently of terminal connections.
    pub(crate) test_id: Option<u64>,
    pub(crate) session_id: String,
    pub(crate) host: String,
    pub(crate) user: String,
    pub(crate) need_user: bool,
    pub(crate) need_password: bool,
    pub(crate) responders: Vec<CredentialResponder>,
}

pub(crate) struct PendingMfa {
    /// Owning window's registry id (see `PendingHostKey::window_id`).
    pub(crate) window_id: u64,
    /// An editor test owns its prompts independently of terminal connections.
    pub(crate) test_id: Option<u64>,
    pub(crate) session_id: String,
    pub(crate) host: String,
    pub(crate) prompt: String,
    pub(crate) echo: bool,
    pub(crate) responders: Vec<MfaResponder>,
}
