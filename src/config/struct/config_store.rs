use std::path::PathBuf;

use super::ConfigFile;

pub struct ConfigStore {
    pub(crate) path: PathBuf,
    pub(crate) backup_dir: Option<PathBuf>,
    pub(crate) cache: ConfigFile,
    /// Ciphertext snapshot used to reject stale full-file writes.
    pub(crate) disk_snapshot: std::cell::RefCell<Option<String>>,
    /// ChaCha20-Poly1305 key loaded from (or freshly generated into)
    /// `secret.key` in the same directory as `sessions.json`.
    pub(crate) key: [u8; 32],
}
