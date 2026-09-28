use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::config::PortForward;

use super::{
    CredentialResponder, HostKeyResponder, MfaResponder, ProcInfo, ProcessKillResult, RemoteEntry,
    RemoteTreeNode, RuntimeTunnelInfo, SessionCommand, SystemDetails,
};

/// Events emitted back to the UI thread.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Free-form status text for the tab header / status line.
    Status(String),
    /// A chunk of stdout/stderr output from the remote shell.
    Output(String),
    /// Connection is up.
    Connected,
    /// Connection closed (either cleanly or after an error).
    Closed(String),
    /// The server presented a host key that is unknown or has changed; the UI
    /// must show a confirmation dialog and answer via `responder` (#109-5). The
    /// handler is blocked awaiting that answer.
    HostKeyPrompt {
        host: String,
        port: u16,
        key_type: String,
        fingerprint: String,
        /// True when a *different* key was previously stored (possible MITM).
        changed: bool,
        responder: HostKeyResponder,
    },
    /// The session is missing a username and/or password; the UI must prompt for
    /// them and answer via `responder`. The auth flow is blocked meanwhile (#110).
    CredentialPrompt {
        session_id: String,
        host: String,
        user: String,
        need_user: bool,
        need_password: bool,
        responder: CredentialResponder,
    },
    /// A keyboard-interactive challenge that isn't the account password —
    /// typically an MFA / OTP / verification-code prompt from a bastion such as
    /// JumpServer. The UI shows `prompt` and answers via `responder`; the auth
    /// flow is blocked meanwhile (#86-MFA).
    MfaPrompt {
        session_id: String,
        host: String,
        /// The server's prompt text, e.g. "MFA code: " / "Verification code:".
        prompt: String,
        /// Whether typed input should be visible (false = hide, like a password).
        echo: bool,
        responder: MfaResponder,
    },
    /// Remote machine resource sample (from the monitor channel).
    /// Memory/swap are in KiB (as reported by /proc/meminfo).
    ResourceStats {
        cpu_percent: f32,
        mem_used_kib: u64,
        mem_total_kib: u64,
        swap_used_kib: u64,
        swap_total_kib: u64,
        /// Per-interface (name, rx_bytes_per_sec, tx_bytes_per_sec).
        net: Vec<(String, u64, u64)>,
        /// Per-filesystem (mount_point, available_bytes, total_bytes).
        disks: Vec<(String, u64, u64)>,
        /// Effective login name reported by the remote host (`id -un`).
        current_user: String,
        /// Top processes by CPU (#23). Empty if the host's `ps` is unusable.
        procs: Vec<ProcInfo>,
        /// Detailed system information for the detached system-info window.
        /// Detailed data is present only for the separately delayed one-shot
        /// system-information probe; lightweight resource samples leave it None.
        sys: Option<SystemDetails>,
    },

    /// Effective user and top-process snapshot from the dedicated lightweight
    /// process channel. Keeping this separate prevents a slow `df`, `lspci`, or
    /// other system-information probe from freezing the process window.
    ProcessStats {
        current_user: String,
        procs: Vec<ProcInfo>,
    },

    /// A command the user ran in the terminal, captured via the shell hook
    /// (OSC 697) so it can join the command-box history (#113).
    CommandRan(String),

    /// Runtime tunnel state changed (#206).
    TunnelUpdate(Vec<RuntimeTunnelInfo>),

    // --- SFTP events -------------------------------------------------------
    /// The shell's current working directory changed (parsed from OSC 7).
    CwdChanged(String),
    /// SFTP directory listing arrived.
    SftpEntries {
        path: String,
        entries: Vec<RemoteEntry>,
    },
    /// Free-form SFTP status message (progress, errors, etc.).
    SftpStatus(String),
    /// A directory listing failed (e.g. permission denied): show the message and
    /// stop the panel's loading spinner without disturbing the current view (#112).
    SftpError(String),
    /// Directory tree structure changed (full rebuild pushed on every toggle).
    SftpTreeUpdate(Vec<RemoteTreeNode>),
    /// File-transfer progress / completion (download or upload).
    SftpTransfer {
        id: String,
        name: String,
        is_upload: bool,
        transferred: u64,
        total: u64,
        state: u8, // 0 = active, 1 = done, 2 = error
        msg: String,
    },
    /// A remote text file loaded for the built-in viewer/editor (#70). On
    /// failure (too large, binary, non-UTF-8, I/O error) `error` is non-empty
    /// and `content` is empty.
    SftpFileText {
        path: String,
        name: String,
        content: String,
        edit: bool,
        error: String,
    },
}

/// Handle retained by the UI layer to talk to a running session.
pub struct SessionHandle {
    #[allow(dead_code)] // used by future resize / reconnect flows
    pub tab_id: String,
    pub commands: UnboundedSender<SessionCommand>,
    #[allow(dead_code)] // keep alive; detach on Drop is fine for v0.1
    pub join: JoinHandle<()>,
}

impl SessionHandle {
    pub fn send_raw(&self, bytes: Vec<u8>) {
        let _ = self.commands.send(SessionCommand::RawInput(bytes));
    }

    pub fn resize(&self, cols: u32, rows: u32) {
        let _ = self.commands.send(SessionCommand::Resize(cols, rows));
    }

    pub fn set_resource_monitoring(&self, enabled: bool) {
        let _ = self
            .commands
            .send(SessionCommand::SetResourceMonitoring(enabled));
    }

    pub fn add_tunnel(&self, id: String, forward: PortForward) {
        let _ = self
            .commands
            .send(SessionCommand::AddTunnel { id, forward });
    }

    pub fn stop_tunnel(&self, id: String) {
        let _ = self.commands.send(SessionCommand::StopTunnel(id));
    }

    pub fn start_tunnel(&self, id: String) {
        let _ = self.commands.send(SessionCommand::StartTunnel(id));
    }

    pub fn delete_tunnel(&self, id: String) {
        let _ = self.commands.send(SessionCommand::DeleteTunnel(id));
    }

    pub fn kill_process(
        &self,
        pid: u32,
        root_password: Option<crate::config::Secret>,
    ) -> tokio::sync::oneshot::Receiver<ProcessKillResult> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        let _ = self.commands.send(SessionCommand::KillProcess {
            pid,
            root_password,
            reply,
        });
        rx
    }

    pub fn close(&self) {
        let _ = self.commands.send(SessionCommand::Close);
    }
}
