// Process-global core shared by every window. Slint is single-threaded:
// all windows live on the same UI thread, so the Rc<RefCell<>> members are
// only ever touched from that thread (the listener registry is written once
// per window construction, also on the UI thread).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use tokio::runtime::Runtime;

use crate::config::ConfigStore;
use crate::resource::{LocalSnap, NetHist, TabStatuses};
use crate::sftp::{SftpHandles, SftpLastCwd};
use crate::ssh::SessionHandle;
use crate::terminal::{RenderGates, TermBuffers};
use crate::ui::{
    AppWindow, PaneInfo, ProcWindow, SplitterInfo, SystemInfoWindow, TabInfo, TerminalState,
};

/// Where a tab's session events are currently delivered. Session pump
/// threads hold an `Arc<Mutex<TabRoute>>` per tab and re-read it on every
/// batch, so moving a tab to another window is just rewriting this struct —
/// the pumps keep running and immediately target the new window.
///
/// Every field is thread-safe (the pumps run off the UI thread).
#[derive(Clone)]
pub struct TabRoute {
    pub window: slint::Weak<AppWindow>,
    pub window_id: u64,
    pub bufs: TermBuffers,
    pub gates: RenderGates,
    pub statuses: TabStatuses,
    pub local_snap: LocalSnap,
    pub net_hist: NetHist,
    pub sftp_handles: SftpHandles,
    pub sftp_last_cwd: SftpLastCwd,
    pub follow_cd: Arc<std::sync::atomic::AtomicBool>,
}

/// Tab id → its current delivery route. Shared with the pump threads.
pub type TabRoutes = Arc<Mutex<HashMap<String, Arc<Mutex<TabRoute>>>>>;

/// Everything another window (or a tab transfer) needs to reach into one
/// open window. UI-thread-only: every Rc<RefCell> here is borrowed on the
/// Slint thread exclusively.
#[derive(Clone)]
pub struct WindowState {
    pub weak: slint::Weak<AppWindow>,
    pub handles: Rc<RefCell<HashMap<String, SessionHandle>>>,
    pub bufs: TermBuffers,
    pub gates: RenderGates,
    pub statuses: TabStatuses,
    pub sftp_handles: SftpHandles,
    pub sftp_last_cwd: SftpLastCwd,
    pub local_snap: LocalSnap,
    pub net_hist: NetHist,
    pub follow_cd: Arc<std::sync::atomic::AtomicBool>,
    pub layout: Rc<RefCell<crate::layout::Layout>>,
    pub tabs_model: Rc<slint::VecModel<TabInfo>>,
    pub terminals_model: Rc<slint::VecModel<TerminalState>>,
    pub panes_model: Rc<slint::VecModel<PaneInfo>>,
    pub splitters_model: Rc<slint::VecModel<SplitterInfo>>,
    /// Repeated Slint timers owned by this window (e.g. the 1 Hz system
    /// sampler). Dropping the state on close drops these, which stops them —
    /// otherwise they keep firing for the whole process lifetime.
    pub timers: Rc<RefCell<Vec<slint::Timer>>>,
    pub content_size: Rc<Cell<(f32, f32)>>,
    /// Strong handles: both windows start hidden, and the Slint backend only
    /// keeps a component alive while its window is visible — without these
    /// they'd be destroyed as soon as `open_window` returns. Released when
    /// `forget_window_state` drops this entry on close.
    pub proc_win: Rc<ProcWindow>,
    pub sys_win: Rc<SystemInfoWindow>,
    pub proc_weak: slint::Weak<ProcWindow>,
    pub sys_weak: slint::Weak<SystemInfoWindow>,
}

/// Open-window registry, generic over the window handle so it can be unit
/// tested without constructing Slint components. Production instantiates
/// `WindowRegistry<slint::Weak<AppWindow>>`.
#[derive(Default)]
pub struct WindowRegistry<H> {
    next_id: RefCell<u64>,
    windows: RefCell<HashMap<u64, H>>,
    /// Config listeners keyed by window id, so `unregister` can drop the
    /// closing window's closure (it captures that window's sessions model
    /// and terminal buffers — leaving it behind would leak them until exit).
    listeners: RefCell<HashMap<u64, Rc<dyn Fn()>>>,
}

impl<H: Clone> WindowRegistry<H> {
    pub fn register(&self, handle: H) -> u64 {
        let mut next = self.next_id.borrow_mut();
        *next += 1;
        let id = *next;
        self.windows.borrow_mut().insert(id, handle);
        id
    }

    /// Remove a window (and its config listener); returns true when the
    /// registry became empty (the caller must then quit the event loop).
    pub fn unregister(&self, id: u64) -> bool {
        self.windows.borrow_mut().remove(&id);
        self.listeners.borrow_mut().remove(&id);
        self.windows.borrow().is_empty()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.windows.borrow().is_empty()
    }

    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.windows.borrow().len()
    }

    #[cfg(test)]
    pub fn for_each<F: FnMut(&H)>(&self, mut f: F) {
        for h in self.windows.borrow().values() {
            f(h);
        }
    }

    /// A handle of the most recently registered window (used as cascade /
    /// position origin for the next one).
    pub fn newest(&self) -> Option<H> {
        self.windows
            .borrow()
            .iter()
            .max_by_key(|(id, _)| *id)
            .map(|(_, h)| h.clone())
    }

    pub fn add_config_listener(&self, id: u64, f: Rc<dyn Fn()>) {
        self.listeners.borrow_mut().insert(id, f);
    }

    /// Config (sessions / theme / language …) changed in one window; every
    /// window refreshes its derived UI state.
    pub fn broadcast_config_changed(&self) {
        for l in self.listeners.borrow().values() {
            l();
        }
    }
}

pub struct AppCore {
    pub runtime: Arc<Runtime>,
    /// Shared among all windows; touched only on the Slint UI thread.
    pub store: Rc<RefCell<ConfigStore>>,
    /// Live windows; the last one closing quits the shared event loop.
    pub registry: Rc<WindowRegistry<slint::Weak<AppWindow>>>,
    /// Per-window state reachable across windows (tab detach/merge), keyed
    /// by the same id the registry hands out. UI-thread-only.
    pub window_states: Rc<RefCell<HashMap<u64, WindowState>>>,
    /// Tab id → delivery route, shared with the session pump threads so a
    /// tab can be retargeted at another window while its pumps keep running.
    pub tab_routes: TabRoutes,
    /// Set once the first window of the process lifetime finishes opening.
    /// The in-app update check runs only for that window — keying it off
    /// `registry.count() == 1` would re-fire after close-then-open.
    /// UI-thread-only, like the rest of this struct.
    pub first_window_done: Cell<bool>,
}

#[cfg(test)]
#[path = "../../tests/app/window_management/registry.rs"]
mod registry_tests;
