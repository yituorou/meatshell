//! SFTP subsystem worker.
//!
//! Each terminal tab that spawns an SSH shell also spawns a *separate* SSH
//! connection for SFTP. This keeps the shell PTY completely unblocked: large
//! file transfers cannot stall readline or vim.
//!
//! The public API is a simple command channel (`SftpHandle::commands`) that
//! accepts `SftpCommand` messages. Results and status updates are pushed back
//! via the shared `UnboundedSender<SessionEvent>` that already exists for the
//! terminal tab.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use russh::client::{self, Handler};
use russh::keys::key::PrivateKeyWithHashAlg;
use russh::Disconnect;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::{RawSftpSession, SftpSession};
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use ssh_key::{HashAlg, PublicKey};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::config::{AuthMethod, Session};
use crate::i18n::t;
use crate::ssh::{format_mtime, format_size, RemoteEntry, RemoteTreeNode, SessionEvent};

use super::transfer::{DownloadConflict, SftpCommand, SftpHandle};

impl SftpHandle {
    pub fn list_dir(&self, path: String) {
        let _ = self.commands.send(SftpCommand::ListDir(path));
    }
    pub fn refresh_dir(&self, path: String) {
        let _ = self.commands.send(SftpCommand::RefreshDir(path));
    }
    pub fn download(&self, remote: String, local_dir: String, conflict: DownloadConflict) {
        let _ = self.commands.send(SftpCommand::Download {
            remote,
            local_dir,
            conflict,
        });
    }
    pub fn download_archive(&self, remote_dir: String, names: Vec<String>, local_dir: String) {
        let _ = self.commands.send(SftpCommand::DownloadArchive {
            remote_dir,
            names,
            local_dir,
        });
    }
    pub fn cancel_transfer(&self, id: String) {
        let _ = self.commands.send(SftpCommand::CancelTransfer(id));
    }
    pub fn upload(&self, local: PathBuf, remote_dir: String) {
        let _ = self.commands.send(SftpCommand::Upload {
            local,
            remote_dir,
            cleanup_after: None,
        });
    }
    pub fn copy_to(
        &self,
        remotes: Vec<String>,
        target: UnboundedSender<SftpCommand>,
        target_dir: String,
    ) {
        let _ = self.commands.send(SftpCommand::CopyTo {
            remotes,
            target,
            target_dir,
        });
    }
    pub fn toggle_tree_node(&self, path: String) {
        let _ = self.commands.send(SftpCommand::ToggleTreeNode(path));
    }
    pub fn delete(&self, path: String) {
        let _ = self.commands.send(SftpCommand::Delete(path));
    }
    pub fn open_temp(&self, remote: String, edit: bool) {
        let _ = self.commands.send(SftpCommand::OpenTemp { remote, edit });
    }
    pub fn rename(&self, from: String, to: String) {
        let _ = self.commands.send(SftpCommand::Rename { from, to });
    }
    pub fn chmod(&self, path: String, mode: u32) {
        let _ = self.commands.send(SftpCommand::Chmod { path, mode });
    }
    pub fn mkdir(&self, path: String) {
        let _ = self.commands.send(SftpCommand::MkDir(path));
    }
    pub fn touch(&self, path: String) {
        let _ = self.commands.send(SftpCommand::TouchFile(path));
    }
    pub fn read_text(&self, remote: String, edit: bool) {
        let _ = self.commands.send(SftpCommand::ReadText { remote, edit });
    }
    pub fn write_text(&self, remote: String, content: String) {
        let _ = self
            .commands
            .send(SftpCommand::WriteText { remote, content });
    }
    pub fn close(&self) {
        let _ = self.commands.send(SftpCommand::Close);
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Spawn an SFTP worker on the Tokio runtime.
///
/// The worker opens its own SSH connection to the same server, authenticates,
/// and requests the `sftp` subsystem. Events (directory listings, progress,
/// errors) are sent back via `events`, which is the same sender used by the
/// terminal's shell session.
/// Turn an SFTP-worker failure into a status-bar message.
///
/// SFTP runs on its own SSH connection, fully separate from the shell PTY, so
/// when it can't connect the terminal keeps working — we just surface why in the
/// SFTP panel. The common bastion/jump-host case is "shell is allowed but the
/// `sftp` subsystem is not", which shows up as a failed subsystem request /
/// channel / handshake (or an explicit "permission denied"). For that family we
/// give a plain-language hint instead of the raw russh error (#190).
fn friendly_sftp_error(err: &anyhow::Error) -> String {
    let chain = err
        .chain()
        .map(|e| e.to_string().to_lowercase())
        .collect::<Vec<_>>()
        .join(" | ");
    let permission_like = [
        "subsystem",      // server refused the `sftp` subsystem request
        "sftp channel",   // channel_open_session refused
        "sftp handshake", // subsystem opened but no SFTP server behind it
        "permission",
        "denied",
        "prohibited", // "administratively prohibited"
        "not allowed",
    ]
    .iter()
    .any(|k| chain.contains(k));
    if permission_like {
        t(
            "SFTP 不可用,请检查是否有访问权限(服务器可能未开放 SFTP)",
            "SFTP unavailable — check whether you have permission (server may not allow SFTP)",
        )
        .to_string()
    } else {
        format!("{}: {err:#}", t("SFTP 错误", "SFTP error"))
    }
}

pub fn spawn_sftp(
    runtime: &tokio::runtime::Handle,
    session: Session,
    jump: Option<Session>,
    events: UnboundedSender<SessionEvent>,
) -> SftpHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let self_tx = cmd_tx.clone();
    let events_err = events.clone();
    let join = runtime.spawn(async move {
        if let Err(err) = run_sftp(session, jump, cmd_rx, self_tx, events).await {
            let _ = events_err.send(SessionEvent::SftpStatus(friendly_sftp_error(&err)));
        }
    });
    SftpHandle {
        commands: cmd_tx,
        join,
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tree state helpers
// ---------------------------------------------------------------------------

/// Recursively build the flat node list from tree state (DFS pre-order).
fn build_tree_nodes(
    path: &str,
    depth: u32,
    expanded: &std::collections::HashSet<String>,
    tree_dirs: &std::collections::HashMap<String, Vec<(String, String)>>,
    nodes: &mut Vec<RemoteTreeNode>,
) {
    let name = if path == "/" {
        "/".to_string()
    } else {
        path.rsplit('/').next().unwrap_or(path).to_string()
    };
    let children = tree_dirs.get(path);
    let has_children = children.map(|c| !c.is_empty()).unwrap_or(true);
    let is_expanded = expanded.contains(path);
    nodes.push(RemoteTreeNode {
        path: path.to_string(),
        name,
        depth,
        expanded: is_expanded,
        has_children,
    });
    if is_expanded {
        if let Some(ch) = children {
            for (_, child_path) in ch {
                build_tree_nodes(child_path, depth + 1, expanded, tree_dirs, nodes);
            }
        }
    }
}

/// Rebuild the flat tree node list from the current cache and push it to the UI.
fn emit_tree(
    tree_dirs: &std::collections::HashMap<String, Vec<(String, String)>>,
    tree_expanded: &std::collections::HashSet<String>,
    events: &UnboundedSender<SessionEvent>,
) {
    let mut nodes = Vec::new();
    build_tree_nodes("/", 0, tree_expanded, tree_dirs, &mut nodes);
    let _ = events.send(SessionEvent::SftpTreeUpdate(nodes));
}

/// Re-fetch a directory's sub-directories into the tree cache, but only if that
/// directory is already known to the tree (root or previously expanded) — so a
/// mutation under a collapsed/unknown branch doesn't graft unrelated nodes in.
/// This is how create/delete/rename keep the left tree in sync without a
/// reconnect (#189).
async fn sync_tree_dir(
    sftp: &SftpSession,
    dir: &str,
    tree_dirs: &mut std::collections::HashMap<String, Vec<(String, String)>>,
) {
    if tree_dirs.contains_key(dir) {
        let dirs = list_dirs_only_impl(sftp, dir).await.unwrap_or_default();
        tree_dirs.insert(dir.to_string(), dirs);
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

async fn run_sftp(
    session: Session,
    jump: Option<Session>,
    mut commands: UnboundedReceiver<SftpCommand>,
    self_tx: UnboundedSender<SftpCommand>,
    events: UnboundedSender<SessionEvent>,
) -> Result<()> {
    let _ = events.send(SessionEvent::SftpStatus(
        t("SFTP 连接中...", "SFTP connecting...").into(),
    ));

    // Open a dedicated SSH connection for SFTP.
    let config = Arc::new(client::Config {
        // Keep the idle SFTP connection alive (#160). Without a keepalive, an idle
        // SFTP connection (no file ops for a while) gets silently dropped by
        // NAT / firewall / server idle timeouts; afterwards every operation fails
        // ("folder read failed"). Send a keepalive every 30 s so traffic never
        // goes quiet; keepalive_max (default 3) still closes a genuinely dead
        // connection after ~90 s of unanswered keepalives.
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        // Match the shell connection's algorithm set so SFTP reaches the same
        // legacy servers (#172) instead of failing with "No common algorithm".
        preferred: russh::Preferred {
            kex: std::borrow::Cow::Borrowed(crate::ssh::COMPAT_KEX),
            cipher: std::borrow::Cow::Borrowed(crate::ssh::COMPAT_CIPHER),
            ..russh::Preferred::DEFAULT
        },
        ..<_>::default()
    });

    let addr = format!("{}:{}", session.host, session.port);
    // Isolate external-editor files by connection and keep the server in the
    // visible local filename. Different hosts (or concurrent edit sessions)
    // can therefore open the same remote basename without sharing one temp
    // file or editor document (#318).
    let external_edit_prefix = sanitize_filename(&session.host);
    let external_edit_dir = std::env::temp_dir().join("meatshell").join(format!(
        "{}-{}-{}",
        external_edit_prefix,
        session.port,
        Uuid::new_v4()
    ));
    // Keep the jump-host connection alive for the whole SFTP session — the
    // direct-tcpip tunnel rides on it (#211). Declared here so it lives to the
    // end of the function; `_`-prefixed so it isn't flagged unused.
    let mut _jump_keepalive;
    // Tunnel through an SSH jump host (#211), the same proxy as the shell (#7),
    // or connect directly.
    let mut handle = match &jump {
        Some(j) => {
            let (h, jh) = crate::ssh::connect_target_via_jump(
                j,
                &session.host,
                session.port,
                config.clone(),
                sftp_handler(&session, &events),
                &events,
            )
            .await
            .with_context(|| format!("sftp connect {} via jump failed", addr))?;
            _jump_keepalive = Some(jh);
            h
        }
        None => {
            _jump_keepalive = None;
            match crate::ssh::proxy::resolve(&session.proxy) {
                Some(p) => {
                    let stream = crate::ssh::proxy::connect(&p, &session.host, session.port)
                        .await
                        .with_context(|| format!("sftp proxy connect {} failed", addr))?;
                    client::connect_stream(config.clone(), stream, sftp_handler(&session, &events))
                        .await
                        .with_context(|| format!("sftp connect {} failed", addr))?
                }
                None => client::connect(
                    config.clone(),
                    addr.as_str(),
                    sftp_handler(&session, &events),
                )
                .await
                .with_context(|| format!("sftp connect {} failed", addr))?,
            }
        }
    };

    // Resolve missing username/password (shares the shell's prompt; the UI
    // de-dupes by session id so SFTP doesn't prompt a second time) (#110).
    let (user, password) = match crate::ssh::resolve_credentials(&session, &events).await {
        Some(c) => c,
        None => return Err(anyhow!(t("已取消登录", "login cancelled"))),
    };

    // --- Authenticate (same method as the shell session) -------------------
    let authed = match session.auth {
        AuthMethod::Password => {
            let mut ok = handle
                .authenticate_password(&user, password.as_str())
                .await
                .context("sftp password auth failed")?;
            if !ok {
                // Match the shell session's fallback: russh can hang if a second
                // auth method is attempted on the same failed handle, so reconnect
                // before trying keyboard-interactive (#86, #186).
                let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
                handle = match &jump {
                    Some(j) => {
                        let (h, jh) = crate::ssh::connect_target_via_jump(
                            j,
                            &session.host,
                            session.port,
                            config.clone(),
                            sftp_handler(&session, &events),
                            &events,
                        )
                        .await
                        .with_context(|| format!("sftp reconnect {} via jump failed", addr))?;
                        _jump_keepalive = Some(jh);
                        h
                    }
                    None => {
                        _jump_keepalive = None;
                        match crate::ssh::proxy::resolve(&session.proxy) {
                            Some(p) => {
                                let stream =
                                    crate::ssh::proxy::connect(&p, &session.host, session.port)
                                        .await
                                        .with_context(|| {
                                            format!("sftp proxy reconnect {} failed", addr)
                                        })?;
                                client::connect_stream(
                                    config.clone(),
                                    stream,
                                    sftp_handler(&session, &events),
                                )
                                .await
                                .with_context(|| format!("sftp reconnect {} failed", addr))?
                            }
                            None => client::connect(
                                config.clone(),
                                addr.as_str(),
                                sftp_handler(&session, &events),
                            )
                            .await
                            .with_context(|| format!("sftp reconnect {} failed", addr))?,
                        }
                    }
                };
                ok = crate::ssh::keyboard_interactive_auth(
                    &mut handle,
                    &user,
                    password.as_str(),
                    &session.id,
                    &session.host,
                    &events,
                )
                .await
                .context("sftp keyboard-interactive auth failed")?;
            }
            ok
        }
        AuthMethod::KeyboardInteractive => crate::ssh::keyboard_interactive_auth(
            &mut handle,
            &user,
            password.as_str(),
            &session.id,
            &session.host,
            &events,
        )
        .await
        .context("sftp keyboard-interactive auth failed")?,
        AuthMethod::Key => {
            // An encrypted private key needs its passphrase; reuse the session's
            // password field for it (empty = unencrypted), exactly like the shell
            // session does — otherwise a passphrase-protected key authenticates the
            // shell but fails SFTP with "the key is encrypted" (#133).
            let pass = password.as_str();
            let keypair = crate::ssh::load_session_private_key(&session, pass)?;
            // RSA keys need an explicit SHA-2 hash; other key types don't.
            let hash = keypair.algorithm().is_rsa().then_some(HashAlg::Sha256);
            let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(keypair), hash)
                .context("invalid private key")?;
            handle
                .authenticate_publickey(&user, key_with_hash)
                .await
                .context("sftp publickey auth failed")?
        }
    };

    if !authed {
        return Err(anyhow!(t("SFTP 认证失败", "SFTP authentication failed")));
    }

    // --- Open the sftp subsystem channel -----------------------------------
    let channel = handle
        .channel_open_session()
        .await
        .context("open sftp channel")?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .context("request sftp subsystem")?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .context("sftp handshake")?;
    // Share the session + connection so transfers can run on their own task,
    // leaving the command loop free to list/switch directories meanwhile (#116-2).
    let sftp = std::sync::Arc::new(sftp);
    let handle = std::sync::Arc::new(handle);

    // Owner/group names live for the whole SFTP connection so every listing
    // after the first can paint names without another `getent` round-trip.
    let owner_cache: SharedOwnerCache = Arc::new(Mutex::new(OwnerNameCache::default()));
    // Kick off a full passwd/group dump immediately — it overlaps the first
    // read_dir / tree walk so names are usually ready before the first paint.
    let (names_ready_tx, names_ready_rx) = tokio::sync::watch::channel(false);
    {
        let handle = handle.clone();
        let owner_cache = owner_cache.clone();
        tokio::spawn(async move {
            prefetch_all_owner_names(&handle, &owner_cache).await;
            let _ = names_ready_tx.send(true);
        });
    }

    // Per-transfer cancel flags, keyed by transfer id. A download task registers
    // its flag here; a CancelTransfer command flips it; the task removes it on
    // exit (#100 cancel download).
    let cancels: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Resolve the home directory and do an initial listing.
    let home = sftp
        .canonicalize(".")
        .await
        .unwrap_or_else(|_| "/".to_string());
    let _ = events.send(SessionEvent::SftpStatus(format!(
        "{} {}...",
        t("SFTP 加载", "SFTP loading"),
        home
    )));
    match list_dir_impl(&sftp, &home).await {
        Ok(entries) => {
            // Prefer painting names with the listing: wait briefly for the
            // connect-time full getent dump that started above.
            wait_owner_prefetch(&names_ready_rx, Duration::from_secs(3)).await;
            let mut entries = entries;
            let need = owner_cache.lock().unwrap().apply(&mut entries);
            let _ = events.send(SessionEvent::SftpEntries {
                path: home.clone(),
                entries: entries.clone(),
            });
            // Defer any remaining remote name lookup so directory-tree init can
            // run first; the command loop's EnrichEntries repaints with names.
            if !need.is_complete() {
                let _ = self_tx.send(SftpCommand::EnrichEntries {
                    path: home.clone(),
                    entries,
                });
            }
            let _ = events.send(SessionEvent::SftpStatus(home.clone()));
        }
        Err(e) => {
            let _ = events.send(SessionEvent::SftpError(list_error_msg(&home, &e)));
        }
    }

    // --- Directory tree initialization -------------------------------------
    // tree_dirs: path -> [(child_name, child_full_path)] for directories only
    // tree_expanded: set of paths currently shown as expanded
    let mut tree_dirs: std::collections::HashMap<String, Vec<(String, String)>> =
        std::collections::HashMap::new();
    let mut tree_expanded: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Fetch root "/" subdirs, then expand path down to home.
    let root_dirs = list_dirs_only_impl(&sftp, "/").await.unwrap_or_default();
    tree_dirs.insert("/".to_string(), root_dirs);
    tree_expanded.insert("/".to_string());

    // Walk each path segment from "/" toward home, expanding as we go.
    if home != "/" {
        let mut current = "/".to_string();
        for segment in home.trim_start_matches('/').split('/') {
            if segment.is_empty() {
                continue;
            }
            let child = format!("{}/{}", current.trim_end_matches('/'), segment);
            // Only expand if this child appeared in the parent listing.
            let found = tree_dirs
                .get(&current)
                .map(|c| c.iter().any(|(_, p)| p == &child))
                .unwrap_or(false);
            if !found {
                break;
            }
            let dirs = list_dirs_only_impl(&sftp, &child).await.unwrap_or_default();
            tree_dirs.insert(child.clone(), dirs);
            tree_expanded.insert(child.clone());
            current = child;
        }
    }
    {
        let mut nodes = Vec::new();
        build_tree_nodes("/", 0, &tree_expanded, &tree_dirs, &mut nodes);
        let _ = events.send(SessionEvent::SftpTreeUpdate(nodes));
    }

    // --- Command loop -------------------------------------------------------
    while let Some(cmd) = commands.recv().await {
        match cmd {
            SftpCommand::Close => break,

            SftpCommand::EnrichEntries { path, entries } => {
                let (entries, _) = enrich_entries_with_owners(&handle, &owner_cache, entries).await;
                // Repaint when any name is available — the deferred home listing
                // already painted with numbers/placeholders.
                if entries.iter().any(|entry| entry.owner.is_some() || entry.group.is_some()) {
                    let _ = events.send(SessionEvent::SftpEntries { path, entries });
                }
            }

            SftpCommand::ListDir(path) => {
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{} {}...",
                    t("加载", "Loading"),
                    path
                )));
                match list_dir_impl(&sftp, &path).await {
                    Ok(entries) => {
                        publish_dir_entries(&handle, &owner_cache, &names_ready_rx, &events, path.clone(), entries)
                            .await;
                        let _ = events.send(SessionEvent::SftpStatus(path));
                    }
                    Err(e) => {
                        let _ = events.send(SessionEvent::SftpError(list_error_msg(&path, &e)));
                    }
                }
            }

            SftpCommand::RefreshDir(path) => {
                // File panel — same as ListDir.
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{} {}...",
                    t("加载", "Loading"),
                    path
                )));
                match list_dir_impl(&sftp, &path).await {
                    Ok(entries) => {
                        publish_dir_entries(&handle, &owner_cache, &names_ready_rx, &events, path.clone(), entries)
                            .await;
                        let _ = events.send(SessionEvent::SftpStatus(path.clone()));
                    }
                    Err(e) => {
                        let _ = events.send(SessionEvent::SftpError(list_error_msg(&path, &e)));
                    }
                }
                // Tree — re-fetch every currently-expanded directory so deleted /
                // created folders sync without a reconnect (#189). Stale entries
                // whose parent no longer lists them are simply never walked by
                // build_tree_nodes, so they drop out on the rebuild.
                let expanded: Vec<String> = tree_expanded.iter().cloned().collect();
                for dir in expanded {
                    let dirs = list_dirs_only_impl(&sftp, &dir).await.unwrap_or_default();
                    tree_dirs.insert(dir, dirs);
                }
                emit_tree(&tree_dirs, &tree_expanded, &events);
            }

            SftpCommand::ToggleTreeNode(path) => {
                if tree_expanded.contains(&path) {
                    // Collapse this node and all descendants.
                    let prefix = format!("{}/", path.trim_end_matches('/'));
                    tree_expanded.retain(|p| p != &path && !p.starts_with(&prefix));
                } else {
                    // Expand: fetch children if not yet cached.
                    if !tree_dirs.contains_key(&path) {
                        let dirs = list_dirs_only_impl(&sftp, &path).await.unwrap_or_default();
                        tree_dirs.insert(path.clone(), dirs);
                    }
                    tree_expanded.insert(path.clone());
                }
                let mut nodes = Vec::new();
                build_tree_nodes("/", 0, &tree_expanded, &tree_dirs, &mut nodes);
                let _ = events.send(SessionEvent::SftpTreeUpdate(nodes));
            }

            SftpCommand::Download {
                remote,
                local_dir,
                conflict,
            } => {
                // Run on its own task so the command loop stays free to list /
                // switch directories during the transfer (#116-2).
                let sftp = sftp.clone();
                let handle = handle.clone();
                let events = events.clone();
                // Register a cancel flag up-front under the file id, so a
                // CancelTransfer arriving mid-download can flip it (#100).
                let file_id = Uuid::new_v4().to_string();
                let cancel = Arc::new(AtomicBool::new(false));
                cancels
                    .lock()
                    .unwrap()
                    .insert(file_id.clone(), cancel.clone());
                let cancels_done = cancels.clone();
                tokio::spawn(async move {
                    // A directory target → recursively mirror the whole tree (#50).
                    let is_dir = sftp
                        .metadata(&remote)
                        .await
                        .ok()
                        .map(|m| (m.permissions.unwrap_or(0) & 0o170_000) == 0o040_000)
                        .unwrap_or(false);
                    if is_dir {
                        let dirname = base_name(&remote);
                        // #100.3: an empty folder downloads nothing — just say so
                        // rather than silently creating an empty local directory.
                        let empty = list_dir_impl(&sftp, &remote)
                            .await
                            .map(|e| e.is_empty())
                            .unwrap_or(false);
                        if empty {
                            let _ = events.send(SessionEvent::SftpStatus(format!(
                                "{}: {}",
                                t("空文件夹", "Empty folder"),
                                dirname
                            )));
                            return;
                        }
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{} {}/...",
                            t("下载文件夹", "Downloading folder"),
                            dirname
                        )));
                        match download_dir(&sftp, &handle, &remote, &local_dir, &events).await {
                            Ok(_) => {
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {}",
                                    t("下载完成", "Downloaded"),
                                    dirname
                                )));
                            }
                            Err(e) => {
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {e}",
                                    t("下载失败", "Download failed")
                                )));
                            }
                        }
                    } else {
                        // Sanitize the server-supplied name before it touches the local
                        // filesystem (#26): a malicious server could otherwise craft a
                        // name with traversal, shell-special chars or a Windows reserved
                        // device name to write outside the chosen dir or hit a device.
                        let filename = sanitize_filename(&base_name(&remote));
                        let requested = download_target_path(&remote, &local_dir);
                        let local_path = match conflict {
                            DownloadConflict::Replace => requested,
                            DownloadConflict::KeepBoth => available_download_path(&requested),
                        };
                        let local_path_text = local_path.to_string_lossy().to_string();
                        let id = file_id.clone();
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{} {}...",
                            t("下载", "Downloading"),
                            filename
                        )));
                        match download_impl(
                            &handle,
                            &remote,
                            &local_path_text,
                            &filename,
                            &id,
                            &events,
                            &cancel,
                        )
                        .await
                        {
                            Ok(true) => {
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {}",
                                    t("下载完成", "Downloaded"),
                                    filename
                                )));
                            }
                            Ok(false) => {
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {}",
                                    t("已取消", "Cancelled"),
                                    filename
                                )));
                            }
                            Err(e) => {
                                emit_transfer(
                                    &events,
                                    &id,
                                    &filename,
                                    false,
                                    0,
                                    0,
                                    2,
                                    &e.to_string(),
                                );
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {e}",
                                    t("下载失败", "Download failed")
                                )));
                            }
                        }
                    }
                    cancels_done.lock().unwrap().remove(&file_id);
                });
            }

            SftpCommand::DownloadArchive {
                remote_dir,
                names,
                local_dir,
            } => {
                // #100: multi-select download. Instead of N concurrent transfers
                // (which raced and dropped files), tar everything into ONE archive
                // on the remote, pull that single file, then delete the temp.
                let sftp = sftp.clone();
                let handle = handle.clone();
                let events = events.clone();
                // Register a cancel flag up-front so CancelTransfer can flip it (#100).
                let id = Uuid::new_v4().to_string();
                let cancel = Arc::new(AtomicBool::new(false));
                cancels.lock().unwrap().insert(id.clone(), cancel.clone());
                let cancels_done = cancels.clone();
                tokio::spawn(async move {
                    let n = names.len();
                    let tmp = format!("/tmp/meatshell-{}.tar", Uuid::new_v4());
                    // Name the archive after the first item's stem, per the user:
                    // 11.txt → "11等文件.tar". Sanitize since names come from the server.
                    let first = names.first().map(|s| s.as_str()).unwrap_or("download");
                    let stem = first
                        .rsplit_once('.')
                        .map(|(a, _)| a)
                        .filter(|a| !a.is_empty())
                        .unwrap_or(first);
                    let arc_name =
                        sanitize_filename(&format!("{}{}.tar", stem, t("等文件", "-and-more")));
                    let local_path = format!("{}/{}", local_dir.trim_end_matches('/'), arc_name);
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{} {} {}...",
                        t("打包下载", "Archiving"),
                        n,
                        t("项", "items")
                    )));
                    // Show a "preparing" row in the transfer panel right away so a
                    // big selection isn't a silent wait while tar runs (#100). The
                    // download then reuses this same id, so the row turns into the
                    // live progress bar once bytes start flowing.
                    emit_transfer(&events, &id, &arc_name, false, 0, 0, 3, "");
                    // Plain tar (no gzip): the user prefers speed over a smaller file.
                    // Server-supplied names are untrusted → quote every argument.
                    // Preserve symlinks instead of dereferencing them. Following a
                    // server-controlled link could archive files outside the selected
                    // directory tree (for example, a link into /etc).
                    let mut cmd =
                        format!("tar -cf {} -C {}", sh_quote(&tmp), sh_quote(&remote_dir));
                    for nm in &names {
                        cmd.push(' ');
                        cmd.push_str(&sh_quote(nm));
                    }
                    let _ = &sftp; // listing session kept alive; transfer uses `handle`
                    let res: Result<bool> = async {
                        let st = exec_remote(&handle, &cmd).await.context("tar on remote")?;
                        if st != 0 {
                            return Err(anyhow!(t("远端 tar 打包失败", "remote tar failed")));
                        }
                        download_impl(&handle, &tmp, &local_path, &arc_name, &id, &events, &cancel)
                            .await
                    }
                    .await;
                    // Best-effort cleanup of the remote temp tar — success, failure
                    // or cancel all reach here, so no junk is left on the server (#100).
                    let _ = exec_remote(&handle, &format!("rm -f {}", sh_quote(&tmp))).await;
                    match res {
                        Ok(true) => {
                            let _ = events.send(SessionEvent::SftpStatus(format!(
                                "{}: {}",
                                t("下载完成", "Downloaded"),
                                arc_name
                            )));
                        }
                        Ok(false) => {
                            let _ = events.send(SessionEvent::SftpStatus(format!(
                                "{}: {}",
                                t("已取消", "Cancelled"),
                                arc_name
                            )));
                        }
                        Err(e) => {
                            emit_transfer(&events, &id, &arc_name, false, 0, 0, 2, &e.to_string());
                            let _ = events.send(SessionEvent::SftpStatus(format!(
                                "{}: {e}",
                                t("下载失败", "Download failed")
                            )));
                        }
                    }
                    cancels_done.lock().unwrap().remove(&id);
                });
            }

            SftpCommand::CancelTransfer(id) => {
                if let Some(flag) = cancels.lock().unwrap().get(&id) {
                    flag.store(true, Ordering::Relaxed);
                }
            }

            SftpCommand::Upload {
                local,
                remote_dir,
                cleanup_after,
            } => {
                // Run on its own task so the command loop stays free to list /
                // switch directories during the transfer (#116-2).
                let sftp = sftp.clone();
                let handle = handle.clone();
                let events = events.clone();
                let owner_cache = owner_cache.clone();
                let names_ready_rx = names_ready_rx.clone();
                // Register a cancel flag up-front under the file id so a
                // CancelTransfer arriving mid-upload can flip it (#100).
                let up_id = Uuid::new_v4().to_string();
                let cancel = Arc::new(AtomicBool::new(false));
                cancels
                    .lock()
                    .unwrap()
                    .insert(up_id.clone(), cancel.clone());
                let cancels_done = cancels.clone();
                tokio::spawn(async move {
                    // A directory source → recursively upload the whole tree (#50).
                    let is_dir = tokio::fs::metadata(&local)
                        .await
                        .map(|m| m.is_dir())
                        .unwrap_or(false);
                    if is_dir {
                        let dirname = match local_file_name_utf8(&local) {
                            Ok(name) => name,
                            Err(e) => {
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {e}",
                                    t("上传失败", "Upload failed")
                                )));
                                if let Some(path) = cleanup_after.as_deref() {
                                    cleanup_import_path(path).await;
                                }
                                cancels_done.lock().unwrap().remove(&up_id);
                                return;
                            }
                        };
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{} {}/...",
                            t("上传文件夹", "Uploading folder"),
                            dirname
                        )));
                        let res = upload_dir(&handle, &sftp, &local, &remote_dir, &events).await;
                        if let Ok(entries) = list_dir_impl(&sftp, &remote_dir).await {
                            publish_dir_entries(
                                &handle,
                                &owner_cache,
                                &names_ready_rx,
                                &events,
                                remote_dir.clone(),
                                entries,
                            )
                            .await;
                        }
                        match res {
                            Ok(_) => {
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {}",
                                    t("上传完成", "Uploaded"),
                                    dirname
                                )));
                            }
                            Err(e) => {
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {e}",
                                    t("上传失败", "Upload failed")
                                )));
                            }
                        }
                    } else {
                        let filename = match local_file_name_utf8(&local) {
                            Ok(name) => name,
                            Err(e) => {
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {e}",
                                    t("上传失败", "Upload failed")
                                )));
                                if let Some(path) = cleanup_after.as_deref() {
                                    cleanup_import_path(path).await;
                                }
                                cancels_done.lock().unwrap().remove(&up_id);
                                return;
                            }
                        };
                        let remote_path =
                            format!("{}/{}", remote_dir.trim_end_matches('/'), filename);
                        let id = up_id.clone();
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{} {}...",
                            t("上传", "Uploading"),
                            filename
                        )));
                        match upload_pipelined(
                            &handle,
                            &local,
                            &remote_path,
                            &filename,
                            &id,
                            &events,
                            &cancel,
                        )
                        .await
                        {
                            Ok(true) => {
                                if let Ok(entries) = list_dir_impl(&sftp, &remote_dir).await {
                                    publish_dir_entries(
                                        &handle,
                                        &owner_cache,
                                        &names_ready_rx,
                                        &events,
                                        remote_dir.clone(),
                                        entries,
                                    )
                                    .await;
                                }
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {}",
                                    t("上传完成", "Uploaded"),
                                    filename
                                )));
                            }
                            Ok(false) => {
                                // Refresh the listing so the removed partial file disappears.
                                if let Ok(entries) = list_dir_impl(&sftp, &remote_dir).await {
                                    publish_dir_entries(
                                        &handle,
                                        &owner_cache,
                                        &names_ready_rx,
                                        &events,
                                        remote_dir.clone(),
                                        entries,
                                    )
                                    .await;
                                }
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {}",
                                    t("已取消", "Cancelled"),
                                    filename
                                )));
                            }
                            Err(e) => {
                                emit_transfer(
                                    &events,
                                    &id,
                                    &filename,
                                    true,
                                    0,
                                    0,
                                    2,
                                    &e.to_string(),
                                );
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {e}",
                                    t("上传失败", "Upload failed")
                                )));
                            }
                        }
                    }
                    if let Some(path) = cleanup_after.as_deref() {
                        cleanup_import_path(path).await;
                    }
                    cancels_done.lock().unwrap().remove(&up_id);
                });
            }

            SftpCommand::UploadEdited { local, remote } => {
                let sftp = sftp.clone();
                let handle = handle.clone();
                let events = events.clone();
                let owner_cache = owner_cache.clone();
                let names_ready_rx = names_ready_rx.clone();
                tokio::spawn(async move {
                    let filename = base_name(&remote);
                    let remote_dir = parent_dir(&remote);
                    let id = Uuid::new_v4().to_string();
                    let no_cancel = Arc::new(AtomicBool::new(false));
                    let result = upload_pipelined(
                        &handle, &local, &remote, &filename, &id, &events, &no_cancel,
                    )
                    .await;
                    match result {
                        Ok(true) => {
                            if let Ok(entries) = list_dir_impl(&sftp, &remote_dir).await {
                                publish_dir_entries(
                                    &handle,
                                    &owner_cache,
                                    &names_ready_rx,
                                    &events,
                                    remote_dir,
                                    entries,
                                )
                                .await;
                            }
                            let _ = events.send(SessionEvent::SftpStatus(format!(
                                "{}: {}",
                                t("已上传修改", "Re-uploaded changes"),
                                filename
                            )));
                        }
                        Ok(false) => {}
                        Err(e) => {
                            let _ = events.send(SessionEvent::SftpStatus(format!(
                                "{}: {e}",
                                t("上传修改失败", "Failed to re-upload changes")
                            )));
                        }
                    }
                });
            }

            SftpCommand::CopyTo {
                remotes,
                target,
                target_dir,
            } => {
                let sftp = sftp.clone();
                let handle = handle.clone();
                let events = events.clone();
                tokio::spawn(async move {
                    let label = format!("{} {}", remotes.len(), t("项", "items"));
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{} {}...",
                        t("复制到其他会话", "Copying to another session"),
                        label
                    )));
                    for remote in remotes {
                        match stage_remote_for_copy(&sftp, &handle, &remote, &events).await {
                            Ok((local, cleanup_root)) => {
                                let _ = target.send(SftpCommand::Upload {
                                    local,
                                    remote_dir: target_dir.clone(),
                                    cleanup_after: Some(cleanup_root),
                                });
                            }
                            Err(e) => {
                                let _ = events.send(SessionEvent::SftpStatus(format!(
                                    "{}: {e}",
                                    t("复制失败", "Copy failed")
                                )));
                            }
                        }
                    }
                });
            }

            SftpCommand::Delete(path) => {
                let filename = base_name(&path);
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{} {}...",
                    t("删除", "Deleting"),
                    filename
                )));
                // Directories are removed recursively (a plain remove_dir only
                // works on an empty dir, so an uploaded folder couldn't be
                // deleted); files via remove_file.
                let is_dir = sftp
                    .metadata(&path)
                    .await
                    .ok()
                    .map(|m| (m.permissions.unwrap_or(0) & 0o170_000) == 0o040_000)
                    .unwrap_or(false);
                let res: Result<()> = if is_dir {
                    remove_dir_recursive(&sftp, &path).await
                } else {
                    sftp.remove_file(&path)
                        .await
                        .map(|_| ())
                        .map_err(|e| anyhow::anyhow!("{e}"))
                };
                match res {
                    Ok(_) => {
                        let parent = parent_dir(&path);
                        if let Ok(entries) = list_dir_impl(&sftp, &parent).await {
                            publish_dir_entries(
                                &handle,
                                &owner_cache,
                                &names_ready_rx,
                                &events,
                                parent.clone(),
                                entries,
                            )
                            .await;
                        }
                        // Keep the left directory tree in sync (#189): drop the
                        // deleted folder and any cached descendants, then re-list
                        // the parent's sub-dirs so the deleted node disappears
                        // without needing a reconnect.
                        let prefix = format!("{}/", path.trim_end_matches('/'));
                        tree_dirs.retain(|p, _| p != &path && !p.starts_with(&prefix));
                        tree_expanded.retain(|p| p != &path && !p.starts_with(&prefix));
                        sync_tree_dir(&sftp, &parent, &mut tree_dirs).await;
                        emit_tree(&tree_dirs, &tree_expanded, &events);
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{}: {}",
                            t("已删除", "Deleted"),
                            filename
                        )));
                    }
                    Err(e) => {
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{}: {e}",
                            t("删除失败", "Delete failed")
                        )));
                    }
                }
            }

            SftpCommand::Rename { from, to } => {
                let refresh = parent_dir(&from);
                match sftp.rename(&from, &to).await {
                    Ok(_) => {
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{}: {}",
                            t("已重命名", "Renamed"),
                            base_name(&to)
                        )));
                        // Sync the left tree (#189): drop the old name + cached
                        // descendants, then re-list both the source and the
                        // destination parent (rename can also move across dirs).
                        let prefix = format!("{}/", from.trim_end_matches('/'));
                        tree_dirs.retain(|p, _| p != &from && !p.starts_with(&prefix));
                        tree_expanded.retain(|p| p != &from && !p.starts_with(&prefix));
                        sync_tree_dir(&sftp, &refresh, &mut tree_dirs).await;
                        let to_parent = parent_dir(&to);
                        if to_parent != refresh {
                            sync_tree_dir(&sftp, &to_parent, &mut tree_dirs).await;
                        }
                        emit_tree(&tree_dirs, &tree_expanded, &events);
                    }
                    Err(e) => {
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{}: {e}",
                            t("重命名失败", "Rename failed")
                        )));
                    }
                }
                if let Ok(entries) = list_dir_impl(&sftp, &refresh).await {
                    publish_dir_entries(&handle, &owner_cache, &names_ready_rx, &events, refresh, entries).await;
                }
            }

            SftpCommand::Chmod { path, mode } => {
                let refresh = parent_dir(&path);
                let attrs = FileAttributes {
                    permissions: Some(mode),
                    ..Default::default()
                };
                match sftp.set_metadata(&path, attrs).await {
                    Ok(_) => {
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{}: {} → {:o}",
                            t("已修改权限", "Permissions changed"),
                            base_name(&path),
                            mode
                        )));
                    }
                    Err(e) => {
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{}: {e}",
                            t("修改权限失败", "chmod failed")
                        )));
                    }
                }
                if let Ok(entries) = list_dir_impl(&sftp, &refresh).await {
                    publish_dir_entries(&handle, &owner_cache, &names_ready_rx, &events, refresh, entries).await;
                }
            }

            SftpCommand::MkDir(path) => {
                let refresh = parent_dir(&path);
                match sftp.create_dir(&path).await {
                    Ok(_) => {
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{}: {}",
                            t("已新建文件夹", "Folder created"),
                            base_name(&path)
                        )));
                        // Show the new folder in the left tree too (#189).
                        sync_tree_dir(&sftp, &refresh, &mut tree_dirs).await;
                        emit_tree(&tree_dirs, &tree_expanded, &events);
                    }
                    Err(e) => {
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{}: {e}",
                            t("新建文件夹失败", "Create folder failed")
                        )));
                    }
                }
                if let Ok(entries) = list_dir_impl(&sftp, &refresh).await {
                    publish_dir_entries(&handle, &owner_cache, &names_ready_rx, &events, refresh, entries).await;
                }
            }

            SftpCommand::TouchFile(path) => {
                let refresh = parent_dir(&path);
                // create() truncates if the file exists, so refuse to clobber.
                let exists = sftp.metadata(&path).await.is_ok();
                if exists {
                    let _ = events.send(SessionEvent::SftpStatus(format!(
                        "{}: {}",
                        t("文件已存在", "File already exists"),
                        base_name(&path)
                    )));
                } else {
                    match sftp.create(&path).await {
                        Ok(_) => {
                            let _ = events.send(SessionEvent::SftpStatus(format!(
                                "{}: {}",
                                t("已新建文件", "File created"),
                                base_name(&path)
                            )));
                        }
                        Err(e) => {
                            let _ = events.send(SessionEvent::SftpStatus(format!(
                                "{}: {e}",
                                t("新建文件失败", "Create file failed")
                            )));
                        }
                    }
                }
                if let Ok(entries) = list_dir_impl(&sftp, &refresh).await {
                    publish_dir_entries(&handle, &owner_cache, &names_ready_rx, &events, refresh, entries).await;
                }
            }

            SftpCommand::OpenTemp { remote, edit } => {
                // Sanitize the remote-controlled name before it becomes a local
                // file path that we later hand to the OS "open" call.
                let filename = sanitize_filename(&base_name(&remote));
                let tmp_dir = external_edit_dir.clone();
                let _ = tokio::fs::create_dir_all(&tmp_dir).await;
                let local = tmp_dir.join(external_edit_local_name(
                    &external_edit_prefix,
                    &filename,
                    &Uuid::new_v4().to_string(),
                ));
                let local_str = local.to_string_lossy().to_string();
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{} {}...",
                    t("打开", "Opening"),
                    filename
                )));
                let xid = Uuid::new_v4().to_string();
                let no_cancel = Arc::new(AtomicBool::new(false));
                match download_impl(
                    &handle, &remote, &local_str, &filename, &xid, &events, &no_cancel,
                )
                .await
                {
                    Ok(_) => {
                        open_with_os(&local_str);
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{}: {}",
                            if edit {
                                t("已打开编辑", "Opened for editing")
                            } else {
                                t("已打开", "Opened")
                            },
                            filename
                        )));
                        if edit {
                            spawn_edit_watcher(self_tx.clone(), local_str, remote.clone());
                        }
                    }
                    Err(e) => {
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{}: {e}",
                            t("打开失败", "Open failed")
                        )));
                    }
                }
            }
            SftpCommand::ReadText { remote, edit } => {
                let name = base_name(&remote);
                let _ = events.send(SessionEvent::SftpStatus(format!(
                    "{} {}...",
                    t("打开", "Opening"),
                    name
                )));
                let (content, error) = match read_text_guarded(&sftp, &remote).await {
                    Ok(text) => (text, String::new()),
                    Err(msg) => (String::new(), msg),
                };
                let _ = events.send(SessionEvent::SftpFileText {
                    path: remote,
                    name,
                    content,
                    edit,
                    error,
                });
            }
            SftpCommand::WriteText { remote, content } => {
                let name = base_name(&remote);
                match write_text_file(&sftp, &remote, &content).await {
                    Ok(_) => {
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{}: {}",
                            t("已保存", "Saved"),
                            name
                        )));
                    }
                    Err(e) => {
                        let _ = events.send(SessionEvent::SftpStatus(format!(
                            "{}: {e:#}",
                            t("保存失败", "Save failed")
                        )));
                    }
                }
            }
        }
    }

    let _ = handle
        .disconnect(Disconnect::ByApplication, "bye", "")
        .await;
    Ok(())
}

const MAX_BUILTIN_EDITOR_BYTES: usize = 512 * 1024;
const MAX_BUILTIN_EDITOR_LINES: usize = 10_000;
const MAX_BUILTIN_EDITOR_LINE_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
enum EditorTextRejection {
    TooLarge,
    TooManyLines,
    LineTooLong,
    Binary,
    InvalidUtf8,
}

fn validate_editor_text(bytes: Vec<u8>) -> std::result::Result<String, EditorTextRejection> {
    if bytes.len() > MAX_BUILTIN_EDITOR_BYTES {
        return Err(EditorTextRejection::TooLarge);
    }
    if bytes
        .iter()
        .any(|&byte| {
            (byte < 0x20 && byte != b'\t' && byte != b'\n' && byte != b'\r') || byte == 0x7f
        })
    {
        return Err(EditorTextRejection::Binary);
    }

    let text = String::from_utf8(bytes).map_err(|_| EditorTextRejection::InvalidUtf8)?;
    let mut line_count = 1usize;
    let mut line_bytes = 0usize;
    for byte in text.bytes() {
        if byte == b'\n' {
            line_count += 1;
            line_bytes = 0;
            if line_count > MAX_BUILTIN_EDITOR_LINES {
                return Err(EditorTextRejection::TooManyLines);
            }
        } else {
            line_bytes += 1;
            if line_bytes > MAX_BUILTIN_EDITOR_LINE_BYTES {
                return Err(EditorTextRejection::LineTooLong);
            }
        }
    }
    Ok(text)
}

fn editor_rejection_message(rejection: EditorTextRejection) -> String {
    match rejection {
        EditorTextRejection::TooLarge => t(
            "文件过大,无法在内置编辑器中打开(上限 512 KB),请使用外部打开/编辑或下载",
            "Too large for the built-in editor (512 KB limit); open/edit externally or download it instead",
        ),
        EditorTextRejection::TooManyLines => t(
            "文本过大(超过 10,000 行),无法在内置编辑器中打开,请使用外部打开/编辑或下载",
            "Text too large (over 10,000 lines) for the built-in editor; open/edit externally or download it instead",
        ),
        EditorTextRejection::LineTooLong => t(
            "文件包含过长的单行,无法在内置编辑器中安全打开,请使用外部打开/编辑或下载",
            "Contains a line too long for the built-in editor; open/edit externally or download it instead",
        ),
        EditorTextRejection::Binary => t(
            "包含控制字符(疑似二进制),无法以文本打开,请下载查看",
            "Contains control characters (likely binary); download it instead",
        ),
        EditorTextRejection::InvalidUtf8 => {
            t("非 UTF-8 文本,无法打开", "Not UTF-8 text; cannot open")
        }
    }
    .into()
}

/// Read a remote file as UTF-8 text for the built-in editor, rejecting content
/// that would make Slint eagerly lay out an unsafe amount of text (#70, #331).
/// Returns the text on success or a human-readable error message on failure.
async fn read_text_guarded(
    sftp: &SftpSession,
    remote: &str,
) -> std::result::Result<String, String> {
    use tokio::io::AsyncReadExt;
    let size = sftp
        .metadata(remote)
        .await
        .ok()
        .and_then(|m| m.size)
        .unwrap_or(0);
    if size > MAX_BUILTIN_EDITOR_BYTES as u64 {
        return Err(editor_rejection_message(EditorTextRejection::TooLarge));
    }
    let f = sftp
        .open(remote)
        .await
        .map_err(|e| format!("{}: {e}", t("打开失败", "Open failed")))?;
    // Metadata may be missing or stale. Read at most one byte past the limit so
    // an untrusted remote file can never make this path allocate without bound.
    let mut bytes = Vec::with_capacity((size as usize).min(MAX_BUILTIN_EDITOR_BYTES));
    let mut reader = f.take(MAX_BUILTIN_EDITOR_BYTES as u64 + 1);
    let mut chunk = [0u8; 8192];
    let mut lines = 1usize;
    loop {
        let count = reader.read(&mut chunk).await
            .map_err(|e| format!("{}: {e}", t("读取失败", "Read failed")))?;
        if count == 0 {
            break;
        }
        lines += chunk[..count].iter().filter(|&&byte| byte == b'\n').count();
        if lines > MAX_BUILTIN_EDITOR_LINES {
            return Err(editor_rejection_message(EditorTextRejection::TooManyLines));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    validate_editor_text(bytes).map_err(editor_rejection_message)
}

/// Overwrite a remote file with the given text (CREATE | WRITE | TRUNCATE).
async fn write_text_file(sftp: &SftpSession, remote: &str, content: &str) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut f = sftp
        .create(remote)
        .await
        .with_context(|| format!("create remote {remote}"))?;
    f.write_all(content.as_bytes())
        .await
        .context("write remote file")?;
    f.flush().await.context("flush remote file")?;
    let _ = f.shutdown().await;
    Ok(())
}

/// File name component of a path.  Handles both remote (`/`) and local Windows
/// (`\`) separators, so uploading `C:\…\frp.tar.gz` yields `frp.tar.gz` rather
/// than the whole path (which previously became the remote file name).
fn base_name(path: &str) -> String {
    let sep = |c: char| c == '/' || c == '\\';
    path.trim_end_matches(sep)
        .rsplit(sep)
        .next()
        .unwrap_or(path)
        .to_string()
}

fn local_file_name_utf8(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("local file name is not valid UTF-8: {}", path.display()))
}

/// Single-quote a string for safe interpolation into a remote `/bin/sh`
/// command. Remote names come from the *server's* listing and are therefore
/// untrusted — without quoting, a crafted name like `; rm -rf ~` would run.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Run a one-shot command on the remote over its own exec channel and return
/// the exit status. Stdout/stderr are drained and discarded.
async fn exec_remote(handle: &client::Handle<SftpClientHandler>, cmd: &str) -> Result<u32> {
    let mut ch = handle
        .channel_open_session()
        .await
        .context("open exec channel")?;
    ch.exec(true, cmd.as_bytes())
        .await
        .context("exec remote command")?;
    let mut status = 0u32;
    while let Some(msg) = ch.wait().await {
        match msg {
            russh::ChannelMsg::ExitStatus { exit_status } => status = exit_status,
            russh::ChannelMsg::Close => break,
            _ => {}
        }
    }
    Ok(status)
}

async fn exec_remote_output(
    handle: &client::Handle<SftpClientHandler>,
    cmd: &str,
) -> Result<(u32, String)> {
    let mut ch = handle
        .channel_open_session()
        .await
        .context("open owner lookup channel")?;
    ch.exec(true, cmd.as_bytes())
        .await
        .context("execute owner lookup")?;
    let mut output = Vec::new();
    let mut status = 0u32;
    while let Some(msg) = ch.wait().await {
        match msg {
            russh::ChannelMsg::Data { data }
            | russh::ChannelMsg::ExtendedData { data, .. } => output.extend_from_slice(&data),
            russh::ChannelMsg::ExitStatus { exit_status } => status = exit_status,
            russh::ChannelMsg::Close => break,
            _ => {}
        }
    }
    Ok((status, String::from_utf8_lossy(&output).into_owned()))
}

/// Parent directory of a remote path ("/a/b" → "/a", "/a" → "/").
fn parent_dir(path: &str) -> String {
    let p = path.trim_end_matches('/');
    match p.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => p[..i].to_string(),
    }
}

/// Open a local file with the OS default application.
///
/// Security: we must NOT route the path through a shell.  The previous
/// `cmd /C start "" <path>` let cmd.exe re-parse the path, so a remote file name
/// containing shell metacharacters (`&` `|` `>` `<` `^` …) — e.g. `foo&calc.exe`
/// — could inject and run arbitrary commands when the user opened it.  We call
/// `ShellExecuteW` directly instead: it treats the path as one opaque string, so
/// no shell parsing happens.  (`xdg-open` on Unix already takes a single argv
/// argument and never invokes a shell.)
#[cfg(windows)]
fn open_with_os(path: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: isize,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> isize;
    }
    let to_wide = |s: &str| -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let op = to_wide("open");
    let file = to_wide(path);
    unsafe {
        ShellExecuteW(
            0,
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        );
    }
}

#[cfg(not(windows))]
fn open_with_os(path: &str) {
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Make a remote-supplied file name safe to use as a *local* file name (for
/// both downloads and temp files): drops path separators (defence-in-depth
/// against traversal), replaces characters invalid on Windows or special to
/// shells with `_`, trims surrounding whitespace and Windows' trailing dots,
/// and neutralises reserved device names (CON, NUL, COM1…).  Normal names
/// (letters, digits, `.`, `-`, `_`, Unicode) pass through; Unix dotfiles keep
/// their leading dot.  Falls back to `file` when nothing usable remains.
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*' | '&' | '^' | '%' | '!' | '`'
            | '$' | '\'' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    // Drop leading whitespace and trailing dots/spaces (Windows strips the
    // latter silently). A leading dot is preserved so `.bashrc` survives.
    let trimmed = cleaned.trim_start_matches(' ').trim_end_matches([' ', '.']);
    if trimmed.is_empty() {
        return "file".to_string();
    }
    // Windows reserved device names are reserved case-insensitively and even
    // with an extension ("CON.txt" still opens the console). A download named
    // after one could read/write a device instead of a file, so prefix `_`.
    let stem = trimmed.split('.').next().unwrap_or(trimmed);
    let reserved = matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if reserved {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    }
}

fn external_edit_local_name(host: &str, filename: &str, unique: &str) -> String {
    format!(
        "{}_{}_{}",
        sanitize_filename(host),
        sanitize_filename(unique),
        sanitize_filename(filename)
    )
}

pub(crate) fn download_target_path(remote: &str, local_dir: &str) -> PathBuf {
    Path::new(local_dir).join(sanitize_filename(&base_name(remote)))
}

fn available_download_path(requested: &Path) -> PathBuf {
    if !requested.exists() {
        return requested.to_path_buf();
    }
    let parent = requested.parent().unwrap_or_else(|| Path::new(""));
    let stem = requested
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = requested.extension().and_then(|value| value.to_str());
    for suffix in 1usize.. {
        let name = match extension {
            Some(extension) if !extension.is_empty() => format!("{stem} ({suffix}).{extension}"),
            _ => format!("{stem} ({suffix})"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("an unused download filename suffix must exist")
}

/// Watch a downloaded temp file and re-upload it to the remote whenever it
/// changes on disk (the "edit" flow).  Re-upload is routed back through the
/// worker's own command channel.  Stops when the channel closes or after a
/// generous idle window.
fn spawn_edit_watcher(self_tx: UnboundedSender<SftpCommand>, local: String, remote: String) {
    tokio::spawn(async move {
        let mtime = |p: &str| std::fs::metadata(p).ok().and_then(|m| m.modified().ok());
        let mut last = mtime(&local);
        // ~40 min of 2s polls; also exits early once the worker is gone.
        for _ in 0..1200 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if self_tx.is_closed() {
                break;
            }
            let cur = mtime(&local);
            if cur.is_some() && cur != last {
                last = cur;
                let _ = self_tx.send(SftpCommand::UploadEdited {
                    local: PathBuf::from(&local),
                    remote: remote.clone(),
                });
            }
        }
    });
}

// ---------------------------------------------------------------------------
// SFTP helpers
// ---------------------------------------------------------------------------

async fn cleanup_import_path(path: &Path) {
    let res = match tokio::fs::metadata(path).await {
        Ok(meta) if meta.is_dir() => tokio::fs::remove_dir_all(path).await,
        Ok(_) => tokio::fs::remove_file(path).await,
        Err(_) => Ok(()),
    };
    if let Err(e) = res {
        tracing::debug!("failed to clean temporary SFTP copy {:?}: {e}", path);
    }
}

async fn stage_remote_for_copy(
    sftp: &SftpSession,
    handle: &client::Handle<SftpClientHandler>,
    remote: &str,
    events: &UnboundedSender<SessionEvent>,
) -> Result<(PathBuf, PathBuf)> {
    let cleanup_root =
        std::env::temp_dir().join(format!("meatshell-remote-copy-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(&cleanup_root)
        .await
        .with_context(|| format!("failed to create temp dir {}", cleanup_root.display()))?;

    let name = sanitize_filename(&base_name(remote));
    let local_path = cleanup_root.join(&name);
    let local_parent = cleanup_root.to_string_lossy().to_string();
    let is_dir = sftp
        .metadata(remote)
        .await
        .ok()
        .map(|m| (m.permissions.unwrap_or(0) & 0o170_000) == 0o040_000)
        .unwrap_or(false);
    let no_cancel = Arc::new(AtomicBool::new(false));
    let id = Uuid::new_v4().to_string();

    if is_dir {
        tokio::fs::create_dir_all(&local_path)
            .await
            .with_context(|| format!("failed to create temp dir {}", local_path.display()))?;
        let empty = list_dir_impl(sftp, remote)
            .await
            .map(|entries| entries.is_empty())
            .unwrap_or(false);
        if !empty {
            download_dir(sftp, handle, remote, &local_parent, events).await?;
        }
    } else {
        let local = local_path.to_string_lossy().to_string();
        download_impl(handle, remote, &local, &name, &id, events, &no_cancel).await?;
    }

    Ok((local_path, cleanup_root))
}

/// A friendlier message for a failed directory listing, calling out the common
/// permission-denied case explicitly rather than dumping the raw error (#112).
fn list_error_msg(path: &str, e: &impl std::fmt::Display) -> String {
    let raw = e.to_string();
    let low = raw.to_lowercase();
    if low.contains("permission") || low.contains("denied") {
        format!("{}: {}", t("权限不足,无法访问", "Permission denied"), path)
    } else {
        format!("{} {}: {}", t("无法访问", "Cannot open"), path, raw)
    }
}

async fn list_dir_impl(sftp: &SftpSession, path: &str) -> Result<Vec<RemoteEntry>> {
    let raw = sftp
        .read_dir(path)
        .await
        .with_context(|| format!("read_dir {path} failed"))?;

    let mut entries: Vec<RemoteEntry> = raw
        .into_iter()
        .filter(|e| {
            let n = e.file_name();
            n != "." && n != ".."
        })
        .map(|e| {
            let name = e.file_name().to_string();
            let full_path = format!("{}/{}", path.trim_end_matches('/'), name);
            let meta = e.metadata();
            // Determine if entry is a directory via Unix permission bits.
            let permissions = meta.permissions.unwrap_or(0);
            let is_dir = (permissions & 0o170_000) == 0o040_000;
            let size = meta.size.unwrap_or(0);
            let modified = meta.mtime.unwrap_or(0);
            RemoteEntry {
                name,
                full_path,
                is_dir,
                size,
                modified,
                mode: permissions & 0o7777,
                permissions_mode: permissions,
                uid: meta.uid,
                gid: meta.gid,
                owner: None,
                group: None,
                file_type: crate::ssh::file_type_from_mode(permissions).to_string(),
            }
        })
        .collect();

    // Sort: directories first, then files; both groups alphabetically.
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    Ok(entries)
}

/// Per-SFTP-connection cache of `uid`/`gid` → account name.
///
/// SFTP listings only carry numeric IDs. Resolving them costs a remote `getent`
/// round-trip, so the first paint of a directory used to flash raw numbers and
/// only later flip to names. A full passwd/group dump is prefetched at connect;
/// this cache is filled from that dump (and later targeted lookups) and
/// consulted on every listing in the same connection, so warm IDs paint as
/// names immediately. Unknown IDs stay blank in the UI rather than showing
/// raw numbers.
type SharedOwnerCache = Arc<Mutex<OwnerNameCache>>;

#[derive(Debug, Default)]
struct OwnerNameCache {
    owners: HashMap<u32, String>,
    groups: HashMap<u32, String>,
    /// IDs `getent` answered for with "no such account" (e.g. NFS zombie UIDs).
    /// Later listings skip the remote lookup for these instead of retrying.
    owner_miss: HashSet<u32>,
    group_miss: HashSet<u32>,
    /// A full `getent passwd` / `getent group` dump has been absorbed. After
    /// that, any ID not in the maps is a real miss and never needs `getent`.
    full_lookup_done: bool,
}

/// IDs still needing a remote lookup after the cache was consulted.
#[derive(Debug, Default)]
struct OwnerLookupNeed {
    uids: Vec<u32>,
    gids: Vec<u32>,
}

impl OwnerLookupNeed {
    fn is_complete(&self) -> bool {
        self.uids.is_empty() && self.gids.is_empty()
    }
}

impl OwnerNameCache {
    /// Fill `entry.owner` / `entry.group` from the cache. Unknown IDs are
    /// collected into the returned need-list; misses (and any ID after a full
    /// passwd dump) count as already resolved — the UI leaves those cells blank
    /// and does not trigger `getent`.
    fn apply(&self, entries: &mut [RemoteEntry]) -> OwnerLookupNeed {
        let mut need_uids = std::collections::BTreeSet::new();
        let mut need_gids = std::collections::BTreeSet::new();
        for entry in entries.iter_mut() {
            if let Some(uid) = entry.uid {
                if let Some(name) = self.owners.get(&uid) {
                    entry.owner = Some(name.clone());
                } else if !self.owner_miss.contains(&uid) && !self.full_lookup_done {
                    need_uids.insert(uid);
                }
            }
            if let Some(gid) = entry.gid {
                if let Some(name) = self.groups.get(&gid) {
                    entry.group = Some(name.clone());
                } else if !self.group_miss.contains(&gid) && !self.full_lookup_done {
                    need_gids.insert(gid);
                }
            }
        }
        OwnerLookupNeed {
            uids: need_uids.into_iter().collect(),
            gids: need_gids.into_iter().collect(),
        }
    }

    /// Store a successful targeted `getent` parse. Requested IDs with no name
    /// become misses so later listings skip the remote lookup for them. Callers
    /// must **not** absorb a failed/timeout lookup — that would permanently pin
    /// blanks for IDs that simply had not been resolved yet.
    fn absorb_resolved(
        &mut self,
        need: &OwnerLookupNeed,
        owners: HashMap<u32, String>,
        groups: HashMap<u32, String>,
    ) {
        for (id, name) in owners {
            self.owner_miss.remove(&id);
            self.owners.insert(id, name);
        }
        for (id, name) in groups {
            self.group_miss.remove(&id);
            self.groups.insert(id, name);
        }
        for &uid in &need.uids {
            if !self.owners.contains_key(&uid) {
                self.owner_miss.insert(uid);
            }
        }
        for &gid in &need.gids {
            if !self.groups.contains_key(&gid) {
                self.group_miss.insert(gid);
            }
        }
    }

    /// Store a full passwd/group dump from the connect-time prefetch. After
    /// this, IDs absent from the maps are treated as misses (no more `getent`).
    fn absorb_full_lookup(&mut self, owners: HashMap<u32, String>, groups: HashMap<u32, String>) {
        for (id, name) in owners {
            self.owner_miss.remove(&id);
            self.owners.insert(id, name);
        }
        for (id, name) in groups {
            self.group_miss.remove(&id);
            self.groups.insert(id, name);
        }
        self.full_lookup_done = true;
    }
}

/// Split `getent passwd` / `getent group` output (see `resolve_owner_names`)
/// into id → name maps.
fn parse_owner_lookup_output(output: &str) -> (HashMap<u32, String>, HashMap<u32, String>) {
    let mut owners = HashMap::new();
    let mut groups = HashMap::new();
    let mut in_groups = false;
    for line in output.lines() {
        if line == "--MEATSHELL-GROUPS--" {
            in_groups = true;
            continue;
        }
        let fields: Vec<&str> = line.split(':').collect();
        if in_groups {
            if let Some((id, name)) = fields
                .get(2)
                .and_then(|id| id.parse::<u32>().ok())
                .zip(fields.first().copied())
            {
                groups.insert(id, name.to_string());
            }
        } else if let Some((id, name)) = fields
            .get(2)
            .and_then(|id| id.parse::<u32>().ok())
            .zip(fields.first().copied())
        {
            owners.insert(id, name.to_string());
        }
    }
    (owners, groups)
}

/// Fill owner/group names on `entries`: cache first, remote `getent` only for
/// IDs this connection has never seen. Returns the entries and whether the
/// remote lookup produced any new names (i.e. the caller should repaint).
async fn enrich_entries_with_owners(
    handle: &client::Handle<SftpClientHandler>,
    cache: &SharedOwnerCache,
    mut entries: Vec<RemoteEntry>,
) -> (Vec<RemoteEntry>, bool) {
    let need = cache.lock().unwrap().apply(&mut entries);
    if need.is_complete() {
        // Warm cache (or nothing to resolve): names are already in place.
        return (entries, false);
    }
    match resolve_owner_names(handle, &need.uids, &need.gids).await {
        OwnerLookup::Failed => (entries, false),
        OwnerLookup::Parsed { owners, groups } => {
            let gained = !owners.is_empty() || !groups.is_empty();
            {
                let mut cache = cache.lock().unwrap();
                cache.absorb_resolved(&need, owners, groups);
                cache.apply(&mut entries);
            }
            (entries, gained)
        }
    }
}

/// Wait briefly for the connect-time full passwd/group dump so the first
/// listing can paint names instead of blanks. No-op once the dump has landed
/// (or after `max` elapsed — the listing then paints blanks and fills in later).
async fn wait_owner_prefetch(ready: &tokio::sync::watch::Receiver<bool>, max: Duration) {
    if *ready.borrow() {
        return;
    }
    let mut ready = ready.clone();
    let _ = tokio::time::timeout(max, ready.wait_for(|done| *done)).await;
}

/// Dump the entire remote passwd + group tables into `cache` (best-effort).
/// Runs at SFTP connect, overlapping the first `read_dir`, so names are usually
/// ready before the first paint. On timeout / missing `getent` the cache is
/// left untouched and later listings fall back to targeted per-ID lookups.
async fn prefetch_all_owner_names(
    handle: &client::Handle<SftpClientHandler>,
    cache: &SharedOwnerCache,
) {
    // No id arguments → getent prints the whole database. `command -v getent
    // || exit 127` keeps a host without getent from marking the dump done
    // with an empty map (which would blank every owner until reconnect).
    let command = "command -v getent >/dev/null 2>&1 || exit 127; \
getent passwd 2>/dev/null; printf '\\n--MEATSHELL-GROUPS--\\n'; getent group 2>/dev/null";
    let Ok(Ok((status, output))) = tokio::time::timeout(
        Duration::from_secs(5),
        exec_remote_output(handle, command),
    )
    .await
    else {
        return;
    };
    if status == 127 {
        return;
    }
    let (owners, groups) = parse_owner_lookup_output(&output);
    cache.lock().unwrap().absorb_full_lookup(owners, groups);
}

/// Publish a directory listing to the UI with owner names filled in.
///
/// Prefetch hit / warm cache → a single `SftpEntries` paint already carrying
/// names. Otherwise the first paint leaves owner/group blank (never raw uid/gid)
/// and a second paint lands once `getent` returns.
async fn publish_dir_entries(
    handle: &client::Handle<SftpClientHandler>,
    cache: &SharedOwnerCache,
    names_ready: &tokio::sync::watch::Receiver<bool>,
    events: &UnboundedSender<SessionEvent>,
    path: String,
    entries: Vec<RemoteEntry>,
) {
    wait_owner_prefetch(names_ready, Duration::from_millis(1500)).await;
    let mut entries = entries;
    let need = cache.lock().unwrap().apply(&mut entries);
    if need.is_complete() {
        let _ = events.send(SessionEvent::SftpEntries { path, entries });
        return;
    }
    let _ = events.send(SessionEvent::SftpEntries {
        path: path.clone(),
        entries: entries.clone(),
    });
    let (entries, gained) = enrich_entries_with_owners(handle, cache, entries).await;
    if gained {
        let _ = events.send(SessionEvent::SftpEntries { path, entries });
    }
}

enum OwnerLookup {
    /// `getent` produced parseable output. IDs missing from the maps are real
    /// "no such account" answers and may be negative-cached.
    Parsed {
        owners: HashMap<u32, String>,
        groups: HashMap<u32, String>,
    },
    /// Timeout or transport failure — must not write misses.
    Failed,
}

async fn resolve_owner_names(
    handle: &client::Handle<SftpClientHandler>,
    uids: &[u32],
    gids: &[u32],
) -> OwnerLookup {
    let mut uid_args: Vec<String> = uids.iter().map(u32::to_string).collect();
    uid_args.sort();
    uid_args.dedup();
    let mut gid_args: Vec<String> = gids.iter().map(u32::to_string).collect();
    gid_args.sort();
    gid_args.dedup();
    if uid_args.is_empty() && gid_args.is_empty() {
        return OwnerLookup::Parsed {
            owners: HashMap::new(),
            groups: HashMap::new(),
        };
    }

    // IDs are parsed as u32 above, so interpolating them cannot introduce shell
    // syntax. A single getent invocation handles all files in the directory.
    // `command -v getent || exit 127` keeps a host without getent from looking
    // like "every uid is a zombie" (which would negative-cache the whole map).
    let passwd = if uid_args.is_empty() {
        "true".to_string()
    } else {
        format!("getent passwd {} 2>/dev/null", uid_args.join(" "))
    };
    let group = if gid_args.is_empty() {
        "true".to_string()
    } else {
        format!("getent group {} 2>/dev/null", gid_args.join(" "))
    };
    let command = format!(
        "command -v getent >/dev/null 2>&1 || exit 127; {passwd}; printf '\\n--MEATSHELL-GROUPS--\\n'; {group}"
    );
    let Ok(Ok((status, output))) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        exec_remote_output(handle, &command),
    )
    .await
    else {
        return OwnerLookup::Failed;
    };
    if status == 127 {
        return OwnerLookup::Failed;
    }
    let (owners, groups) = parse_owner_lookup_output(&output);
    OwnerLookup::Parsed { owners, groups }
}

/// List only the subdirectories of `path` (no files). Used to build the tree.
async fn list_dirs_only_impl(sftp: &SftpSession, path: &str) -> Result<Vec<(String, String)>> {
    let entries = list_dir_impl(sftp, path).await?;
    Ok(entries
        .into_iter()
        .filter(|e| e.is_dir)
        .map(|e| (e.name, e.full_path))
        .collect())
}

/// Emit a transfer-progress event.
fn emit_transfer(
    events: &UnboundedSender<SessionEvent>,
    id: &str,
    name: &str,
    is_upload: bool,
    transferred: u64,
    total: u64,
    state: u8,
    msg: &str,
) {
    let _ = events.send(SessionEvent::SftpTransfer {
        id: id.to_string(),
        name: name.to_string(),
        is_upload,
        transferred,
        total,
        state,
        msg: msg.to_string(),
    });
}

/// Download a remote file over a dedicated, *pipelined* raw SFTP channel.
///
/// The high-level reader issues one READ and waits for the reply before the
/// next, so throughput is capped by the round-trip time (slow on any latent
/// link). Here we keep many READ requests in flight at once, each tagged with
/// its absolute offset so out-of-order completion is fine — mirroring
/// `upload_pipelined`.
///
/// Returns `Ok(true)` when the whole file was written, or `Ok(false)` if the
/// transfer was cancelled. In both the cancel and error cases the partial
/// local file is removed so no half-downloaded junk is left behind.
async fn download_impl(
    handle: &client::Handle<SftpClientHandler>,
    remote: &str,
    local: &str,
    name: &str,
    id: &str,
    events: &UnboundedSender<SessionEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<bool> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    const CHUNK: usize = 32 * 1024;
    const MAX_INFLIGHT: usize = 32; // ~1 MB outstanding hides the RTT

    let channel = handle
        .channel_open_session()
        .await
        .context("open sftp download channel")?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .context("request sftp subsystem")?;
    let raw = Arc::new(RawSftpSession::new(channel.into_stream()));
    raw.init().await.context("sftp download handshake")?;

    let total = raw
        .stat(remote)
        .await
        .ok()
        .and_then(|a| a.attrs.size)
        .unwrap_or(0);
    let fhandle = raw
        .open(remote, OpenFlags::READ, FileAttributes::default())
        .await
        .with_context(|| format!("open remote {remote}"))?
        .handle;
    let mut local_file = tokio::fs::File::create(local)
        .await
        .with_context(|| format!("create local {local}"))?;

    emit_transfer(events, id, name, false, 0, total, 0, "");

    let mut done: u64 = 0;
    let mut last = Instant::now();
    let mut err: Option<anyhow::Error> = None;
    let mut cancelled = false;

    if total > 0 {
        let mut next_off = 0u64;
        let mut inflight = FuturesUnordered::new();
        loop {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
            }
            // Top up the pipeline with fresh READ requests.
            while !cancelled && err.is_none() && next_off < total && inflight.len() < MAX_INFLIGHT {
                let off = next_off;
                let want = ((total - off) as usize).min(CHUNK);
                next_off += want as u64;
                let raw2 = raw.clone();
                let h = fhandle.clone();
                inflight.push(async move {
                    // Fill the whole chunk, coping with short reads.
                    let mut data = Vec::with_capacity(want);
                    let mut o = off;
                    let end = off + want as u64;
                    while o < end {
                        match raw2.read(h.clone(), o, (end - o) as u32).await {
                            Ok(d) => {
                                if d.data.is_empty() {
                                    break;
                                }
                                o += d.data.len() as u64;
                                data.extend_from_slice(&d.data);
                            }
                            Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => break,
                            Err(e) => return Err(anyhow!("read remote: {e}")),
                        }
                    }
                    Ok::<(u64, Vec<u8>), anyhow::Error>((off, data))
                });
            }
            if inflight.is_empty() {
                break;
            }
            match inflight.next().await {
                Some(Ok((off, data))) => {
                    if !data.is_empty() {
                        if let Err(e) = local_file.seek(std::io::SeekFrom::Start(off)).await {
                            err = Some(anyhow!("seek local: {e}"));
                        } else if let Err(e) = local_file.write_all(&data).await {
                            err = Some(anyhow!("write local: {e}"));
                        } else {
                            done += data.len() as u64;
                        }
                    }
                    if last.elapsed() >= Duration::from_millis(150) {
                        last = Instant::now();
                        emit_transfer(events, id, name, false, done, total, 0, "");
                    }
                }
                Some(Err(e)) => err = Some(e),
                None => {}
            }
            if (cancelled || err.is_some()) && inflight.is_empty() {
                break;
            }
        }
    } else {
        // Unknown / zero size: serial drain until EOF (rare; keeps correctness).
        let mut off = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            match raw.read(fhandle.clone(), off, CHUNK as u32).await {
                Ok(d) => {
                    if d.data.is_empty() {
                        break;
                    }
                    local_file
                        .write_all(&d.data)
                        .await
                        .context("write local file")?;
                    off += d.data.len() as u64;
                    done += d.data.len() as u64;
                    if last.elapsed() >= Duration::from_millis(150) {
                        last = Instant::now();
                        emit_transfer(events, id, name, false, done, done, 0, "");
                    }
                }
                Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => break,
                Err(e) => {
                    err = Some(anyhow!("read remote: {e}"));
                    break;
                }
            }
        }
    }

    let _ = raw.close(fhandle).await;

    if let Some(e) = err {
        drop(local_file);
        let _ = tokio::fs::remove_file(local).await;
        return Err(e);
    }
    if cancelled {
        drop(local_file);
        let _ = tokio::fs::remove_file(local).await;
        emit_transfer(
            events,
            id,
            name,
            false,
            done,
            total,
            4,
            t("已取消", "Cancelled"),
        );
        return Ok(false);
    }
    local_file.flush().await.context("flush local file")?;
    emit_transfer(events, id, name, false, done, total.max(done), 1, "");
    Ok(true)
}

/// Recursively download a remote directory tree under `local_parent` (#50).
///
/// Iterative (work-stack) rather than a boxed async recursion: each remote dir
/// is mirrored to a sanitized local name, then its files are downloaded with the
/// same per-file pipeline used for single downloads. Names are sanitized (#26)
/// so a hostile server can't escape the chosen folder.
async fn download_dir(
    sftp: &SftpSession,
    handle: &client::Handle<SftpClientHandler>,
    remote_root: &str,
    local_parent: &str,
    events: &UnboundedSender<SessionEvent>,
) -> Result<()> {
    // Folder transfers aren't individually cancellable from the UI; a throwaway
    // never-set flag satisfies download_impl's signature.
    let no_cancel = Arc::new(AtomicBool::new(false));
    let root_name = sanitize_filename(&base_name(remote_root));
    let root_local = format!("{}/{}", local_parent.trim_end_matches('/'), root_name);
    // (remote_dir, local_dir) pairs still to mirror.
    let mut stack = vec![(remote_root.trim_end_matches('/').to_string(), root_local)];
    while let Some((rdir, ldir)) = stack.pop() {
        tokio::fs::create_dir_all(&ldir)
            .await
            .with_context(|| format!("create local dir {ldir}"))?;
        for entry in list_dir_impl(sftp, &rdir).await? {
            if entry.is_dir {
                let child_local = format!("{}/{}", ldir, sanitize_filename(&entry.name));
                stack.push((entry.full_path, child_local));
            } else {
                let fname = sanitize_filename(&entry.name);
                let lpath = format!("{}/{}", ldir, fname);
                let id = Uuid::new_v4().to_string();
                download_impl(
                    handle,
                    &entry.full_path,
                    &lpath,
                    &fname,
                    &id,
                    events,
                    &no_cancel,
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// Recursively remove a remote directory tree (#50 follow-up).
///
/// A plain `remove_dir` only deletes an *empty* directory, so deleting an
/// uploaded folder failed. We BFS to discover every sub-directory (deleting
/// files as we go), then rmdir them deepest-first.
async fn remove_dir_recursive(sftp: &SftpSession, root: &str) -> Result<()> {
    let mut all_dirs = vec![root.trim_end_matches('/').to_string()];
    let mut i = 0;
    while i < all_dirs.len() {
        let d = all_dirs[i].clone();
        i += 1;
        for entry in list_dir_impl(sftp, &d).await? {
            if entry.is_dir {
                all_dirs.push(entry.full_path);
            } else {
                sftp.remove_file(&entry.full_path)
                    .await
                    .map_err(|e| anyhow::anyhow!("remove file {}: {e}", entry.full_path))?;
            }
        }
    }
    // BFS discovered parents before children, so reversing gives deepest-first.
    for d in all_dirs.iter().rev() {
        sftp.remove_dir(d)
            .await
            .map_err(|e| anyhow::anyhow!("remove dir {d}: {e}"))?;
    }
    Ok(())
}

/// Recursively upload a local directory tree into `remote_parent` (#50).
///
/// Iterative work-stack: mirror each local dir to the remote (create_dir, whose
/// "already exists" error is ignored), then upload its files with the pipelined
/// path. Symlinks and other special files are skipped.
async fn upload_dir(
    handle: &client::Handle<SftpClientHandler>,
    sftp: &SftpSession,
    local_root: &Path,
    remote_parent: &str,
    events: &UnboundedSender<SessionEvent>,
) -> Result<()> {
    // Folder uploads aren't individually cancellable from the UI; a throwaway
    // never-set flag satisfies upload_pipelined's signature.
    let no_cancel = Arc::new(AtomicBool::new(false));
    let root_name = local_file_name_utf8(local_root)?;
    let remote_root = format!("{}/{}", remote_parent.trim_end_matches('/'), root_name);
    let mut stack = vec![(local_root.to_path_buf(), remote_root)];
    while let Some((ldir, rdir)) = stack.pop() {
        // Best-effort mkdir; an error usually just means the dir already exists.
        let _ = sftp.create_dir(&rdir).await;
        let mut rd = tokio::fs::read_dir(&ldir)
            .await
            .with_context(|| format!("read local dir {}", ldir.display()))?;
        while let Some(entry) = rd.next_entry().await.context("read dir entry")? {
            let lpath = entry.path();
            let name = local_file_name_utf8(&lpath)?;
            let rchild = format!("{}/{}", rdir, name);
            let ft = entry.file_type().await.context("file type")?;
            if ft.is_dir() {
                stack.push((lpath, rchild));
            } else if ft.is_file() {
                let id = Uuid::new_v4().to_string();
                upload_pipelined(handle, &lpath, &rchild, &name, &id, events, &no_cancel).await?;
            }
        }
    }
    Ok(())
}

/// Pipelined SFTP upload (#16).
///
/// The high-level `SftpSession`/`File` writes one chunk and waits for the
/// server's ack before sending the next, so throughput is capped by the
/// round-trip time (~15x slower than scp on a latent link).  Here we open a
/// dedicated raw SFTP channel and keep many WRITE requests in flight at once
/// (each tagged with its absolute offset, so out-of-order completion is fine),
/// which hides the latency and brings us within a single order of magnitude of
/// native scp.
async fn upload_pipelined(
    handle: &client::Handle<SftpClientHandler>,
    local: &Path,
    remote: &str,
    name: &str,
    id: &str,
    events: &UnboundedSender<SessionEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<bool> {
    use tokio::io::AsyncReadExt;

    const CHUNK: usize = 32 * 1024; // safe SFTP write size
    const MAX_INFLIGHT: usize = 32; // ~1 MB of outstanding writes hides the RTT

    let total = tokio::fs::metadata(local)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let mut local_file = tokio::fs::File::open(local)
        .await
        .with_context(|| format!("open local {}", local.display()))?;

    // Dedicated raw SFTP channel for the transfer (keeps the browse session
    // responsive and lets us issue concurrent WRITE requests).
    let channel = handle
        .channel_open_session()
        .await
        .context("open sftp upload channel")?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .context("request sftp subsystem")?;
    let raw = Arc::new(RawSftpSession::new(channel.into_stream()));
    raw.init().await.context("sftp upload handshake")?;

    let fhandle = raw
        .open(
            remote,
            OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
            FileAttributes::default(),
        )
        .await
        .with_context(|| format!("create remote {remote}"))?
        .handle;

    emit_transfer(events, id, name, true, 0, total, 0, "");

    let mut offset: u64 = 0;
    let mut done: u64 = 0;
    let mut last = Instant::now();
    let mut eof = false;
    let mut err: Option<anyhow::Error> = None;
    let mut cancelled = false;
    let mut inflight = FuturesUnordered::new();

    while !eof || !inflight.is_empty() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            eof = true; // stop reading more; drain what's in flight
        }
        // Top up the pipeline with fresh WRITE requests.
        while !eof && inflight.len() < MAX_INFLIGHT {
            let mut buf = vec![0u8; CHUNK];
            match local_file.read(&mut buf).await {
                Ok(0) => eof = true,
                Ok(n) => {
                    buf.truncate(n);
                    let off = offset;
                    offset += n as u64;
                    let raw2 = raw.clone();
                    let h = fhandle.clone();
                    inflight.push(async move { raw2.write(h, off, buf).await.map(|_| n as u64) });
                }
                Err(e) => {
                    err = Some(anyhow!("read local file: {e}"));
                    eof = true;
                }
            }
        }
        match inflight.next().await {
            Some(Ok(n)) => {
                done += n;
                if last.elapsed() >= Duration::from_millis(150) {
                    last = Instant::now();
                    emit_transfer(events, id, name, true, done, total, 0, "");
                }
            }
            Some(Err(e)) => {
                err = Some(anyhow!("write remote file: {e}"));
                eof = true; // stop reading more
            }
            None => {}
        }
        if err.is_some() {
            break;
        }
    }

    let _ = raw.close(fhandle).await;
    if let Some(e) = err {
        // Drop the partial remote file so a failed upload leaves no junk.
        let _ = raw.remove(remote).await;
        return Err(e);
    }
    if cancelled {
        // Remove the half-written remote file on cancel (#100).
        let _ = raw.remove(remote).await;
        emit_transfer(
            events,
            id,
            name,
            true,
            done,
            total,
            4,
            t("已取消", "Cancelled"),
        );
        return Ok(false);
    }
    emit_transfer(events, id, name, true, done, total.max(done), 1, "");
    Ok(true)
}

// ---------------------------------------------------------------------------
// russh client handler — verifies the host key against known_hosts, reusing the
// shell session's prompt path (#109-5). The UI de-duplicates by host:port, so a
// fresh host confirmed for the shell won't prompt again for SFTP.
// ---------------------------------------------------------------------------

struct SftpClientHandler {
    host: String,
    port: u16,
    events: UnboundedSender<SessionEvent>,
}

fn sftp_handler(session: &Session, events: &UnboundedSender<SessionEvent>) -> SftpClientHandler {
    SftpClientHandler {
        host: session.host.clone(),
        port: session.port,
        events: events.clone(),
    }
}

#[async_trait]
impl Handler for SftpClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(
            crate::ssh::verify_host_key(&self.host, self.port, server_public_key, &self.events)
                .await,
        )
    }

    async fn data(
        &mut self,
        _channel: russh::ChannelId,
        _data: &[u8],
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

// Keep format helpers and RemoteTreeNode imports live.
const _: fn() = || {
    let _ = format_size(0);
    let _ = format_mtime(0);
    let _: RemoteTreeNode;
};

#[cfg(test)]
mod sanitize_tests {
    use super::{
        available_download_path, download_target_path, external_edit_local_name,
        sanitize_filename, validate_editor_text, EditorTextRejection,
        MAX_BUILTIN_EDITOR_BYTES, MAX_BUILTIN_EDITOR_LINES, MAX_BUILTIN_EDITOR_LINE_BYTES,
    };

    #[test]
    fn plain_names_pass_through() {
        assert_eq!(sanitize_filename("report.txt"), "report.txt");
        assert_eq!(sanitize_filename("my-file_v2.tar.gz"), "my-file_v2.tar.gz");
        assert_eq!(sanitize_filename("数据.csv"), "数据.csv");
        // Unix dotfiles keep their leading dot.
        assert_eq!(sanitize_filename(".bashrc"), ".bashrc");
    }

    #[test]
    fn strips_path_separators_and_traversal() {
        // base_name already strips dirs, but sanitize is defence-in-depth: the
        // result must never keep a separator that could escape the target dir.
        assert_eq!(sanitize_filename("a/b\\c"), "a_b_c");
        let traversal = sanitize_filename("../../etc/passwd");
        assert!(!traversal.contains('/') && !traversal.contains('\\'));
        let win = sanitize_filename("..\\..\\Windows\\System32");
        assert!(!win.contains('/') && !win.contains('\\'));
    }

    #[test]
    fn replaces_shell_and_windows_special_chars() {
        assert_eq!(sanitize_filename("foo&calc.exe"), "foo_calc.exe");
        assert_eq!(sanitize_filename("a|b>c<d:e?f*g"), "a_b_c_d_e_f_g");
        assert_eq!(sanitize_filename("$(whoami)"), "_(whoami)");
        assert_eq!(sanitize_filename("a`b'c"), "a_b_c");
    }

    #[test]
    fn trims_whitespace_and_trailing_dots() {
        assert_eq!(sanitize_filename("   spaced.txt  "), "spaced.txt");
        assert_eq!(sanitize_filename("name..."), "name");
        // control chars become underscores, not trimmed
        assert_eq!(sanitize_filename("a\tb"), "a_b");
    }

    #[test]
    fn neutralises_windows_reserved_device_names() {
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("nul"), "_nul");
        assert_eq!(sanitize_filename("COM1"), "_COM1");
        assert_eq!(sanitize_filename("LPT9.txt"), "_LPT9.txt"); // reserved even with ext
        assert_eq!(sanitize_filename("Aux.log"), "_Aux.log");
        // Not reserved: a name that merely starts with the same letters.
        assert_eq!(sanitize_filename("console.txt"), "console.txt");
        assert_eq!(sanitize_filename("COM10"), "COM10");
    }

    #[test]
    fn empty_or_all_bad_falls_back() {
        assert_eq!(sanitize_filename(""), "file");
        assert_eq!(sanitize_filename("   "), "file");
        assert_eq!(sanitize_filename("..."), "file");
    }

    #[test]
    fn external_edits_include_the_host_in_the_local_name() {
        assert_eq!(
            external_edit_local_name("192.168.1.10", "nginx.conf", "edit-1"),
            "192.168.1.10_edit-1_nginx.conf"
        );
        assert_ne!(
            external_edit_local_name("server-a", "nginx.conf", "edit-1"),
            external_edit_local_name("server-b", "nginx.conf", "edit-1")
        );
        assert_ne!(
            external_edit_local_name("server-a", "nginx.conf", "edit-1"),
            external_edit_local_name("server-a", "nginx.conf", "edit-2")
        );
    }

    #[test]
    fn keep_both_uses_the_first_available_numbered_name() {
        let dir = std::env::temp_dir().join(format!("meatshell-download-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let requested = download_target_path("/remote/report.txt", dir.to_str().unwrap());
        std::fs::write(&requested, b"old").unwrap();
        std::fs::write(dir.join("report (1).txt"), b"older").unwrap();

        assert_eq!(available_download_path(&requested), dir.join("report (2).txt"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn editor_rejects_content_over_the_safe_byte_limit() {
        let bytes = vec![b'a'; MAX_BUILTIN_EDITOR_BYTES + 1];
        assert_eq!(
            validate_editor_text(bytes),
            Err(EditorTextRejection::TooLarge)
        );
    }

    #[test]
    fn editor_rejects_pathological_text_layouts() {
        let lines = vec![b'\n'; MAX_BUILTIN_EDITOR_LINES];
        assert_eq!(
            validate_editor_text(lines),
            Err(EditorTextRejection::TooManyLines)
        );
        let line = vec![b'a'; MAX_BUILTIN_EDITOR_LINE_BYTES + 1];
        assert_eq!(
            validate_editor_text(line),
            Err(EditorTextRejection::LineTooLong)
        );
    }

    #[test]
    fn editor_line_limit_is_ten_thousand_including_trailing_empty_line() {
        for newline in ["\n", "\r\n"] {
            let exact = newline.repeat(9_999);
            assert_eq!(validate_editor_text(exact.as_bytes().to_vec()), Ok(exact.clone()));
            let over = format!("{exact}{newline}");
            assert_eq!(validate_editor_text(over.into_bytes()),
                Err(EditorTextRejection::TooManyLines));
        }
    }

    #[test]
    fn editor_accepts_regular_utf8_text_within_limits() {
        assert_eq!(
            validate_editor_text("第一行\nsecond line\n".as_bytes().to_vec()),
            Ok("第一行\nsecond line\n".into())
        );
    }
}

#[cfg(test)]
mod owner_cache_tests {
    use super::*;

    fn entry(uid: Option<u32>, gid: Option<u32>) -> RemoteEntry {
        RemoteEntry {
            name: "f".into(),
            full_path: "/f".into(),
            is_dir: false,
            size: 0,
            modified: 0,
            mode: 0o644,
            permissions_mode: 0o100644,
            uid,
            gid,
            owner: None,
            group: None,
            file_type: "file".into(),
        }
    }

    #[test]
    fn apply_fills_names_from_cache_and_reports_complete() {
        let mut cache = OwnerNameCache::default();
        cache.owners.insert(1000, "www-data".into());
        cache.groups.insert(1000, "www-data".into());

        let mut entries = vec![entry(Some(1000), Some(1000))];
        let need = cache.apply(&mut entries);

        assert!(need.is_complete());
        assert_eq!(entries[0].owner.as_deref(), Some("www-data"));
        assert_eq!(entries[0].group.as_deref(), Some("www-data"));
    }

    #[test]
    fn apply_requests_only_unknown_ids() {
        let mut cache = OwnerNameCache::default();
        cache.owners.insert(0, "root".into());

        let mut entries = vec![entry(Some(0), Some(33)), entry(Some(1000), Some(33))];
        let need = cache.apply(&mut entries);

        assert!(!need.is_complete());
        assert_eq!(need.uids, vec![1000]);
        assert_eq!(need.gids, vec![33]);
        assert_eq!(entries[0].owner.as_deref(), Some("root"));
        assert_eq!(entries[0].group.as_deref(), None);
        assert_eq!(entries[1].owner.as_deref(), None);
    }

    #[test]
    fn miss_counts_as_resolved_so_getent_is_skipped() {
        let mut cache = OwnerNameCache::default();
        cache.owner_miss.insert(4242);
        cache.group_miss.insert(4242);

        let mut entries = vec![entry(Some(4242), Some(4242))];
        let need = cache.apply(&mut entries);

        assert!(need.is_complete());
        assert_eq!(entries[0].owner, None);
        assert_eq!(entries[0].group, None);
    }

    #[test]
    fn full_lookup_treats_unknown_ids_as_blanks_without_remote() {
        let mut cache = OwnerNameCache::default();
        cache.absorb_full_lookup(
            HashMap::from([(0, "root".to_string())]),
            HashMap::from([(0, "root".to_string())]),
        );

        let mut entries = vec![entry(Some(0), Some(0)), entry(Some(9999), Some(9999))];
        let need = cache.apply(&mut entries);

        // 0 resolves from the dump; 9999 is a real miss after a full dump and
        // must NOT queue another getent (UI stays blank).
        assert!(need.is_complete());
        assert_eq!(entries[0].owner.as_deref(), Some("root"));
        assert_eq!(entries[1].owner, None);
        assert_eq!(entries[1].group, None);
    }

    #[test]
    fn absorb_resolved_stores_names_and_negative_caches_missing_ids() {
        let mut cache = OwnerNameCache::default();
        let need = OwnerLookupNeed {
            uids: vec![0, 9999],
            gids: vec![0, 9999],
        };
        let mut owners = HashMap::new();
        owners.insert(0, "root".to_string());
        let mut groups = HashMap::new();
        groups.insert(0, "root".to_string());
        cache.absorb_resolved(&need, owners, groups);

        assert_eq!(cache.owners.get(&0).map(String::as_str), Some("root"));
        assert!(cache.owner_miss.contains(&9999));
        assert_eq!(cache.groups.get(&0).map(String::as_str), Some("root"));
        assert!(cache.group_miss.contains(&9999));

        let mut entries = vec![entry(Some(0), Some(0)), entry(Some(9999), Some(9999))];
        let need = cache.apply(&mut entries);
        assert!(need.is_complete());
    }

    #[test]
    fn absorb_replaces_miss_when_name_appears_later() {
        let mut cache = OwnerNameCache::default();
        cache.owner_miss.insert(7);
        cache.absorb_resolved(
            &OwnerLookupNeed {
                uids: vec![7],
                gids: vec![],
            },
            HashMap::from([(7, "alice".to_string())]),
            HashMap::new(),
        );
        assert!(!cache.owner_miss.contains(&7));
        assert_eq!(cache.owners.get(&7).map(String::as_str), Some("alice"));
    }

    #[test]
    fn failed_lookup_shape_does_not_mark_miss_via_empty_absorb() {
        // Documented contract: callers must not absorb a failed lookup. A
        // successful parse with zero names for a requested id *does* mark miss.
        let mut cache = OwnerNameCache::default();
        cache.absorb_resolved(
            &OwnerLookupNeed {
                uids: vec![5],
                gids: vec![],
            },
            HashMap::new(),
            HashMap::new(),
        );
        assert!(cache.owner_miss.contains(&5));
    }

    #[test]
    fn parse_owner_lookup_output_splits_passwd_and_group() {
        let output = "\
root:x:0:0:root:/root:/bin/bash
www-data:x:33:33:www-data:/var/www:/usr/sbin/nologin
--MEATSHELL-GROUPS--
root:x:0:
www-data:x:33:
";
        let (owners, groups) = parse_owner_lookup_output(output);
        assert_eq!(owners.get(&0).map(String::as_str), Some("root"));
        assert_eq!(owners.get(&33).map(String::as_str), Some("www-data"));
        assert_eq!(groups.get(&0).map(String::as_str), Some("root"));
        assert_eq!(groups.get(&33).map(String::as_str), Some("www-data"));
    }

    #[test]
    fn parse_owner_lookup_output_ignores_malformed_lines() {
        let output = "\
not-a-passwd-line
ghost:x:notanumber:0:a:b:c
--MEATSHELL-GROUPS--
also-bad
";
        let (owners, groups) = parse_owner_lookup_output(output);
        assert!(owners.is_empty());
        assert!(groups.is_empty());
    }

    #[test]
    fn second_apply_after_absorb_needs_no_remote_lookup() {
        // Simulates the warm path: first listing discovers 1000, later listings
        // must return `is_complete` so publish paints names in one shot.
        let mut cache = OwnerNameCache::default();
        let mut first = vec![entry(Some(1000), Some(1000))];
        let need = cache.apply(&mut first);
        assert!(!need.is_complete());
        cache.absorb_resolved(
            &need,
            HashMap::from([(1000, "alice".to_string())]),
            HashMap::from([(1000, "staff".to_string())]),
        );
        cache.apply(&mut first);
        assert_eq!(first[0].owner.as_deref(), Some("alice"));
        assert_eq!(first[0].group.as_deref(), Some("staff"));

        let mut second = vec![entry(Some(1000), Some(1000)), entry(Some(1000), Some(1000))];
        let need = cache.apply(&mut second);
        assert!(need.is_complete());
        assert!(second.iter().all(|e| e.owner.as_deref() == Some("alice")));
    }
}
