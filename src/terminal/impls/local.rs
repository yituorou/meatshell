//! Local terminal session worker.
//!
//! Local shells need a real PTY/ConPTY. Plain stdin/stdout pipes break normal
//! console editing (Backspace/Delete/IME composition) and make Windows shells
//! disagree about encodings. `portable-pty` gives us ConPTY on Windows and a
//! Unix PTY on Linux/macOS while reusing the same UI event path as SSH.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::config::Session;
use crate::i18n::t;
use crate::ssh::{SessionCommand, SessionEvent, SessionHandle};

#[cfg(windows)]
const WSL_LOGIN_SHELL: &str = "shell=$(getent passwd \"$(id -un)\" 2>/dev/null | cut -d: -f7); \
     [ -x \"$shell\" ] || shell=${SHELL:-/bin/sh}; exec \"$shell\" -l";

pub fn spawn_local_session(
    runtime: &tokio::runtime::Handle,
    tab_id: String,
    session: Session,
    initial_cols: u32,
    initial_rows: u32,
) -> (SessionHandle, UnboundedReceiver<SessionEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
    let (evt_tx, evt_rx) = mpsc::unbounded_channel::<SessionEvent>();

    let evt_for_task = evt_tx.clone();
    let join = runtime.spawn(async move {
        if let Err(err) = run_local(
            session,
            cmd_rx,
            evt_for_task.clone(),
            initial_cols,
            initial_rows,
        )
        .await
        {
            let _ = evt_for_task.send(SessionEvent::Closed(format!("{err:#}")));
        }
    });

    (
        SessionHandle {
            tab_id,
            commands: cmd_tx,
            join,
        },
        evt_rx,
    )
}

async fn run_local(
    session: Session,
    mut commands: UnboundedReceiver<SessionCommand>,
    events: UnboundedSender<SessionEvent>,
    initial_cols: u32,
    initial_rows: u32,
) -> Result<()> {
    let (program, args) = local_program(&session);
    let label = if session.name.trim().is_empty() {
        program.clone()
    } else {
        session.name.clone()
    };
    let _ = events.send(SessionEvent::Status(format!(
        "{} {}",
        t("启动本地终端", "Starting local terminal"),
        label
    )));

    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: initial_rows.clamp(1, u16::MAX as u32) as u16,
            cols: initial_cols.clamp(1, u16::MAX as u32) as u16,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("failed to open local pty")?;

    let mut cmd = CommandBuilder::new(&program);
    for arg in &args {
        cmd.arg(arg);
    }
    cmd.env("TERM", "xterm-256color");

    let child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("failed to start local terminal: {program}"))?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().context("local pty reader")?;
    let writer = pair.master.take_writer().context("local pty writer")?;
    let writer = Arc::new(Mutex::new(writer));
    let child = Arc::new(Mutex::new(child));

    let _ = events.send(SessionEvent::Connected);
    let _ = events.send(SessionEvent::Status(format!(
        "{} {}",
        t("已启动", "Started"),
        label
    )));

    {
        let reader_events = events.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = reader_events.send(SessionEvent::Closed(
                            t("本地终端已退出", "local terminal exited").into(),
                        ));
                        break;
                    }
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                        if reader_events.send(SessionEvent::Output(text)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = reader_events.send(SessionEvent::Closed(format!(
                            "{}: {e}",
                            t("本地终端读取失败", "local terminal read failed")
                        )));
                        break;
                    }
                }
            }
        });
    }

    while let Some(cmd) = commands.recv().await {
        match cmd {
            SessionCommand::RawInput(bytes) => {
                tracing::debug!("local pty write len={} bytes", bytes.len());
                let mut guard = writer.lock().unwrap();
                if guard.write_all(&bytes).and_then(|_| guard.flush()).is_err() {
                    let _ = events.send(SessionEvent::Closed(t("写入失败", "write failed").into()));
                    break;
                }
            }
            SessionCommand::Resize(cols, rows) => {
                let _ = pair.master.resize(PtySize {
                    rows: rows.clamp(1, u16::MAX as u32) as u16,
                    cols: cols.clamp(1, u16::MAX as u32) as u16,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
            SessionCommand::AddTunnel { .. }
            | SessionCommand::StopTunnel(_)
            | SessionCommand::StartTunnel(_)
            | SessionCommand::DeleteTunnel(_)
            | SessionCommand::SetResourceMonitoring(_) => {}
            SessionCommand::KillProcess { reply, .. } => {
                let _ = reply.send(crate::ssh::ProcessKillResult {
                    success: false,
                    message: t(
                        "本地终端不支持远程进程操作",
                        "Remote process control is unavailable for local terminals",
                    )
                    .into(),
                });
            }
            SessionCommand::Close => {
                let _ = child.lock().unwrap().kill();
                break;
            }
        }
    }
    Ok(())
}

fn local_program(session: &Session) -> (String, Vec<String>) {
    match session.host.as_str() {
        #[cfg(windows)]
        "cmd" => (
            "cmd.exe".to_string(),
            vec![
                "/Q".to_string(),
                "/K".to_string(),
                "chcp 65001>nul".to_string(),
            ],
        ),
        #[cfg(windows)]
        "wsl" => {
            let mut args = Vec::new();
            if !session.local_distribution.trim().is_empty() {
                args.push("--distribution".to_string());
                args.push(session.local_distribution.clone());
            }
            args.push("--cd".to_string());
            args.push(if session.local_working_dir.trim().is_empty() {
                "~".to_string()
            } else {
                session.local_working_dir.clone()
            });
            // Do not rely on wsl.exe's implicit shell launch. In particular,
            // Arch WSL installations whose passwd login shell is fish can open
            // a PTY without ever presenting an interactive prompt (#352).
            // Resolve the current Linux user's configured shell inside the
            // distribution, then replace the temporary POSIX shell with it.
            args.extend([
                "--exec".to_string(),
                "/bin/sh".to_string(),
                "-lc".to_string(),
                WSL_LOGIN_SHELL.to_string(),
            ]);
            ("wsl.exe".to_string(), args)
        }
        #[cfg(windows)]
        "powershell" | _ => (
            "powershell.exe".to_string(),
            vec![
                "-NoLogo".to_string(),
                "-NoExit".to_string(),
                "-Command".to_string(),
                "$utf8 = New-Object System.Text.UTF8Encoding $false; [Console]::InputEncoding = $utf8; [Console]::OutputEncoding = $utf8; $OutputEncoding = $utf8".to_string(),
            ],
        ),
        #[cfg(not(windows))]
        _ => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            (shell, Vec::new())
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::{local_program, WSL_LOGIN_SHELL};
    use crate::config::Session;

    #[cfg(windows)]
    #[test]
    fn windows_shells_start_in_utf8_mode() {
        let mut session = Session::new_empty();
        session.host = "powershell".to_string();
        let (_, ps_args) = local_program(&session);
        assert!(ps_args.iter().any(|arg| arg.contains("OutputEncoding")));
        assert!(ps_args.iter().any(|arg| arg.contains("InputEncoding")));

        session.host = "cmd".to_string();
        let (_, cmd_args) = local_program(&session);
        assert!(cmd_args.iter().any(|arg| arg.contains("chcp 65001")));
    }

    #[cfg(windows)]
    #[test]
    fn wsl_uses_distribution_and_home_by_default() {
        let mut session = Session::new_empty();
        session.host = "wsl".to_string();
        session.local_distribution = "Ubuntu-24.04".to_string();
        let (_, args) = local_program(&session);
        assert_eq!(
            args,
            [
                "--distribution",
                "Ubuntu-24.04",
                "--cd",
                "~",
                "--exec",
                "/bin/sh",
                "-lc",
                WSL_LOGIN_SHELL,
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn wsl_explicitly_starts_the_passwd_login_shell() {
        assert!(WSL_LOGIN_SHELL.contains("getent passwd"));
        assert!(WSL_LOGIN_SHELL.contains("exec \"$shell\" -l"));
        assert!(!WSL_LOGIN_SHELL.contains("fish"));
    }
}
