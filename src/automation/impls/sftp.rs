//! Non-interactive SFTP operations shared by CLI and MCP adapters.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::config::Session;
use crate::sftp::SftpCommand;
use crate::ssh::SessionEvent;

// Dropping a cancelled HTTP/CLI operation must stop its detached SFTP worker.
// Sending Close alone can wait behind a stalled transfer; abort also drops the
// SSH connection's existing cancellation guard.
struct AutomationSftp(crate::sftp::SftpHandle);
impl std::ops::Deref for AutomationSftp {
    type Target = crate::sftp::SftpHandle;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl Drop for AutomationSftp {
    fn drop(&mut self) {
        let _ = self.0.commands.send(SftpCommand::Close);
        self.0.join.abort();
    }
}


pub(super) async fn list(
    session: Session,
    jump: Vec<Session>,
    path: String,
    timeout: Duration,
) -> Result<Value> {
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let handle = AutomationSftp(crate::sftp::spawn_sftp(&tokio::runtime::Handle::current(), session, jump, events));
    handle
        .commands
        .send(SftpCommand::ListDir(path.clone()))
        .map_err(|_| anyhow!("SFTP worker stopped before listing {path}"))?;

    let result = tokio::time::timeout(timeout, async {
        let mut last_status = String::new();
        while let Some(event) = event_rx.recv().await {
            match event {
                SessionEvent::SftpEntries {
                    path: event_path,
                    entries,
                } if event_path == path || path == "." => {
                    let entries = entries
                        .into_iter()
                        .map(|entry| {
                            json!({
                                "name": entry.name,
                                "path": entry.full_path,
                                "is_directory": entry.is_dir,
                                "size": entry.size,
                                "modified": entry.modified,
                                "mode": format!("{:04o}", entry.mode),
                                "permissions": crate::ssh::format_permissions(entry.permissions_mode),
                                "uid": entry.uid,
                                "gid": entry.gid,
                                "owner": entry.owner,
                                "group": entry.group,
                                "file_type": entry.file_type,
                            })
                        })
                        .collect::<Vec<_>>();
                    return Ok(json!({ "path": event_path, "entries": entries }));
                }
                SessionEvent::SftpError(error) => return Err(anyhow!(error)),
                SessionEvent::SftpStatus(status) => last_status = status,
                SessionEvent::HostKeyPrompt { responder, .. } => {
                    responder.respond(false);
                    return Err(anyhow!("remote host key is not trusted by MeatShell"));
                }
                SessionEvent::CredentialPrompt { responder, .. } => {
                    responder.respond(None);
                    return Err(anyhow!("saved session credentials are incomplete"));
                }
                SessionEvent::MfaPrompt { responder, .. } => {
                    responder.respond(None);
                    return Err(anyhow!(
                        "interactive MFA is not supported by MCP file tools"
                    ));
                }
                _ => {}
            }
        }
        Err(anyhow!(if last_status.is_empty() {
            "SFTP worker stopped before returning a directory listing".to_string()
        } else {
            last_status
        }))
    })
    .await
    .map_err(|_| anyhow!("SFTP list timed out"))?;
    let _ = handle.commands.send(SftpCommand::Close);
    result
}

pub(super) async fn read_text(
    session: Session,
    jump: Vec<Session>,
    path: String,
    timeout: Duration,
) -> Result<Value> {
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let handle = AutomationSftp(crate::sftp::spawn_sftp(&tokio::runtime::Handle::current(), session, jump, events));
    handle
        .commands
        .send(SftpCommand::ReadText {
            remote: path.clone(),
            edit: false,
        })
        .map_err(|_| anyhow!("SFTP worker stopped before reading {path}"))?;

    let result = tokio::time::timeout(timeout, async {
        let mut last_status = String::new();
        while let Some(event) = event_rx.recv().await {
            match event {
                SessionEvent::SftpFileText {
                    path: event_path,
                    content,
                    error,
                    ..
                } if event_path == path => {
                    if error.is_empty() {
                        return Ok(json!({ "path": event_path, "content": content }));
                    }
                    return Err(anyhow!(error));
                }
                SessionEvent::SftpError(error) => return Err(anyhow!(error)),
                SessionEvent::SftpStatus(status) => last_status = status,
                SessionEvent::HostKeyPrompt { responder, .. } => {
                    responder.respond(false);
                    return Err(anyhow!("remote host key is not trusted by MeatShell"));
                }
                SessionEvent::CredentialPrompt { responder, .. } => {
                    responder.respond(None);
                    return Err(anyhow!("saved session credentials are incomplete"));
                }
                SessionEvent::MfaPrompt { responder, .. } => {
                    responder.respond(None);
                    return Err(anyhow!(
                        "interactive MFA is not supported by MCP file tools"
                    ));
                }
                _ => {}
            }
        }
        Err(anyhow!(if last_status.is_empty() {
            "SFTP worker stopped before returning the file".to_string()
        } else {
            last_status
        }))
    })
    .await
    .map_err(|_| anyhow!("SFTP read timed out"))?;
    let _ = handle.commands.send(SftpCommand::Close);
    result
}

pub(super) async fn transfer(
    session: Session,
    jump: Vec<Session>,
    command: SftpCommand,
    upload: bool,
    timeout: Duration,
) -> Result<Value> {
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let handle = AutomationSftp(crate::sftp::spawn_sftp(&tokio::runtime::Handle::current(), session, jump, events));
    handle
        .commands
        .send(command)
        .map_err(|_| anyhow!("SFTP worker stopped before starting the transfer"))?;

    let result = tokio::time::timeout(timeout, async {
        let mut last_status = String::new();
        while let Some(event) = event_rx.recv().await {
            match event {
                SessionEvent::SftpTransfer {
                    name,
                    is_upload,
                    transferred,
                    total,
                    state,
                    msg,
                    ..
                } if is_upload == upload && state != 0 => {
                    if state == 1 {
                        return Ok(json!({
                            "name": name,
                            "direction": if upload { "upload" } else { "download" },
                            "transferred": transferred,
                            "total": total,
                        }));
                    }
                    return Err(anyhow!(if msg.is_empty() {
                        "SFTP transfer failed".to_string()
                    } else {
                        msg
                    }));
                }
                SessionEvent::SftpError(error) => return Err(anyhow!(error)),
                SessionEvent::SftpStatus(status) => last_status = status,
                SessionEvent::HostKeyPrompt { responder, .. } => {
                    responder.respond(false);
                    return Err(anyhow!("remote host key is not trusted by MeatShell"));
                }
                SessionEvent::CredentialPrompt { responder, .. } => {
                    responder.respond(None);
                    return Err(anyhow!("saved session credentials are incomplete"));
                }
                SessionEvent::MfaPrompt { responder, .. } => {
                    responder.respond(None);
                    return Err(anyhow!(
                        "interactive MFA is not supported by MCP file tools"
                    ));
                }
                _ => {}
            }
        }
        Err(anyhow!(if last_status.is_empty() {
            "SFTP worker stopped before completing the transfer".to_string()
        } else {
            last_status
        }))
    })
    .await
    .map_err(|_| anyhow!("SFTP transfer timed out"))?;
    let _ = handle.commands.send(SftpCommand::Close);
    result
}
