use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tokio::runtime::Runtime;

use crate::config::ConfigStore;
use crate::resource::{LocalSnap, NetHist, TabStatuses};
use crate::sftp::{SftpHandles, SftpLastCwd};
use crate::ssh::SessionHandle;
use crate::terminal::{RenderGates, TermBuffers};
use crate::ui::{AppWindow, EditorWindow};

/// Shared dependencies for starting or reconnecting a session tab.
pub(crate) struct ConnectCtx {
    pub(crate) weak: slint::Weak<AppWindow>,
    pub(crate) editor: slint::Weak<EditorWindow>,
    /// Registry id of the window this session belongs to, so connect-time
    /// prompts (host key / credentials / MFA) open their dialog in the
    /// owning window rather than whichever window happens to resolve the
    /// global queue front first (#multi-window).
    pub(crate) window_id: u64,
    pub(crate) runtime: Arc<Runtime>,
    pub(crate) handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    pub(crate) sftp_handles: SftpHandles,
    pub(crate) sftp_last_cwd: SftpLastCwd,
    pub(crate) bufs: TermBuffers,
    pub(crate) render_gates: RenderGates,
    pub(crate) tab_statuses: TabStatuses,
    pub(crate) local_snap: LocalSnap,
    pub(crate) local_net_hist: NetHist,
    pub(crate) last_term_size: Arc<Mutex<(u32, u32)>>,
    pub(crate) sftp_follow_cd: Arc<AtomicBool>,
    pub(crate) store: Rc<RefCell<ConfigStore>>,
    /// Process-wide tab delivery routes. Starting a session registers its
    /// route here so a later detach/merge can retarget the running pumps
    /// at another window without respawning them (#tab-detach).
    pub(crate) tab_routes: crate::app::core::TabRoutes,
}

pub(crate) use super::pending_prompts::{PendingCred, PendingHostKey, PendingMfa};
