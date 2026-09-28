/// Commands posted to the worker task by the UI.
#[derive(Debug)]
pub enum SessionCommand {
    /// Send raw bytes directly to the PTY (individual keystrokes, no modification).
    RawInput(Vec<u8>),
    /// Notify the remote PTY of a terminal resize.
    Resize(u32, u32),
    /// Start or pause periodic local/remote resource monitoring for this session.
    SetResourceMonitoring(bool),
    /// Start a runtime-only SSH tunnel for this connected session (#206).
    AddTunnel {
        id: String,
        forward: crate::config::PortForward,
    },
    /// Stop a runtime tunnel created for this connected session (#206).
    StopTunnel(String),
    /// Restart a stopped or manually enabled saved tunnel.
    StartTunnel(String),
    /// Remove a tunnel row after cancelling its listener.
    DeleteTunnel(String),
    /// Terminate one remote process on a short-lived exec channel. Supplying a
    /// password selects the privileged `sudo -S` path; the secret is never
    /// written to the interactive PTY or shell history.
    KillProcess {
        pid: u32,
        root_password: Option<crate::config::Secret>,
        reply: tokio::sync::oneshot::Sender<ProcessKillResult>,
    },
    /// Gracefully disconnect and drop the session.
    Close,
}

#[derive(Debug)]
pub struct ProcessKillResult {
    pub success: bool,
    pub message: String,
}
