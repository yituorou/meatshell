use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::config::{ConfigStore, Session};

use super::structs::Frontend;

const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
/// Advertised default for upload_file / download_file.
const DEFAULT_TRANSFER_TIMEOUT_SECONDS: u64 = 120;
const MAX_TIMEOUT_SECONDS: u64 = 300;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MIN_OUTPUT_BYTES: usize = 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Keys accepted for the session id, in priority order. `session_id` is the
/// advertised name. `saved_session_id` is an advertised alias: some MCP
/// bridges (Claude desktop's remote-devices bridge) consume a top-level
/// `session_id` argument for their own routing and drop it before forwarding,
/// so every session tool failed with "missing or invalid string argument:
/// session_id" while list_sessions/import_sessions kept working.
/// `sessionId` is accepted silently for camelCase-normalising clients.
pub(crate) const SESSION_ID_KEYS: &[&str] = &["session_id", "saved_session_id", "sessionId"];

pub(crate) async fn call(name: &str, arguments: &Value, frontend: Frontend) -> Result<Value> {
    crate::ssh::with_automation_cancellation(call_inner(name, arguments, frontend)).await
}

async fn call_inner(name: &str, arguments: &Value, frontend: Frontend) -> Result<Value> {
    match name {
        "list_sessions" => list_sessions(arguments, frontend),
        "get_session" => get_session(arguments, frontend),
        "import_sessions" => import_sessions(arguments, frontend),
        "run_command" => run_command(arguments, frontend).await,
        "list_remote_files" => list_remote_files(arguments, frontend).await,
        "read_remote_text_file" => read_remote_text_file(arguments, frontend).await,
        "upload_file" => upload_file(arguments, frontend).await,
        "download_file" => download_file(arguments, frontend).await,
        _ => Err(anyhow!("unknown tool: {name}")),
    }
}

/// Parse a tool's arguments exactly as `call` would, without touching the
/// configuration, the filesystem, or the network.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn validate_arguments(name: &str, arguments: &Value) -> Result<()> {
    match name {
        "list_sessions" => list_sessions_args(arguments).map(drop),
        "get_session" => session_id_arg(arguments).map(drop),
        "import_sessions" => import_args(arguments).map(drop),
        "run_command" => run_command_args(arguments).map(drop),
        "list_remote_files" => list_remote_files_args(arguments).map(drop),
        "read_remote_text_file" => read_remote_text_file_args(arguments).map(drop),
        "upload_file" => upload_args(arguments).map(drop),
        "download_file" => download_args(arguments).map(drop),
        _ => Err(anyhow!("unknown tool: {name}")),
    }
}

// ---------------------------------------------------------------------------
// Argument parsing (pure; mirrors the advertised MCP input schemas)
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
struct ListSessionsArgs<'a> {
    group: Option<&'a str>,
}

fn list_sessions_args(arguments: &Value) -> Result<ListSessionsArgs<'_>> {
    Ok(ListSessionsArgs {
        group: optional_string(arguments, "group")?,
    })
}

#[derive(Debug, PartialEq)]
struct ImportArgs<'a> {
    local_path: &'a str,
    dry_run: bool,
}

fn import_args(arguments: &Value) -> Result<ImportArgs<'_>> {
    let local_path = required_nonempty_string(arguments, "local_path")?;
    // Preview is the safe default for protocol callers.
    let dry_run = optional_bool(arguments, "dry_run")?.unwrap_or(true);
    Ok(ImportArgs {
        local_path,
        dry_run,
    })
}

#[derive(Debug, PartialEq)]
struct RunCommandArgs<'a> {
    session_id: &'a str,
    command: &'a str,
    timeout: Duration,
    max_output_bytes: usize,
}

fn run_command_args(arguments: &Value) -> Result<RunCommandArgs<'_>> {
    let session_id = session_id_arg(arguments)?;
    let command = required_nonempty_string(arguments, "command")?;
    let timeout = timeout_arg(arguments, DEFAULT_TIMEOUT_SECONDS)?;
    let max_output_bytes = optional_u64(arguments, "max_output_bytes")?
        .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES as u64)
        .clamp(MIN_OUTPUT_BYTES as u64, MAX_OUTPUT_BYTES as u64)
        as usize;
    Ok(RunCommandArgs {
        session_id,
        command,
        timeout,
        max_output_bytes,
    })
}

#[derive(Debug, PartialEq)]
struct SftpTarget<'a> {
    session_id: &'a str,
    timeout: Duration,
}

fn sftp_target(arguments: &Value, default_timeout: u64) -> Result<SftpTarget<'_>> {
    Ok(SftpTarget {
        session_id: session_id_arg(arguments)?,
        timeout: timeout_arg(arguments, default_timeout)?,
    })
}

#[derive(Debug, PartialEq)]
struct PathArgs<'a> {
    target: SftpTarget<'a>,
    path: &'a str,
}

fn list_remote_files_args(arguments: &Value) -> Result<PathArgs<'_>> {
    Ok(PathArgs {
        target: sftp_target(arguments, DEFAULT_TIMEOUT_SECONDS)?,
        path: optional_string(arguments, "path")?.unwrap_or("."),
    })
}

fn read_remote_text_file_args(arguments: &Value) -> Result<PathArgs<'_>> {
    Ok(PathArgs {
        target: sftp_target(arguments, DEFAULT_TIMEOUT_SECONDS)?,
        path: required_nonempty_string(arguments, "path")?,
    })
}

#[derive(Debug, PartialEq)]
struct UploadArgs<'a> {
    target: SftpTarget<'a>,
    local_path: &'a str,
    remote_directory: &'a str,
}

fn upload_args(arguments: &Value) -> Result<UploadArgs<'_>> {
    Ok(UploadArgs {
        target: sftp_target(arguments, DEFAULT_TRANSFER_TIMEOUT_SECONDS)?,
        local_path: required_nonempty_string(arguments, "local_path")?,
        remote_directory: required_nonempty_string(arguments, "remote_directory")?,
    })
}

#[derive(Debug, PartialEq)]
struct DownloadArgs<'a> {
    target: SftpTarget<'a>,
    remote_path: &'a str,
    local_directory: &'a str,
}

fn download_args(arguments: &Value) -> Result<DownloadArgs<'_>> {
    Ok(DownloadArgs {
        target: sftp_target(arguments, DEFAULT_TRANSFER_TIMEOUT_SECONDS)?,
        remote_path: required_nonempty_string(arguments, "remote_path")?,
        local_directory: required_nonempty_string(arguments, "local_directory")?,
    })
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

fn import_sessions(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let args = import_args(arguments)?;
    // The CLI passes its explicit --dry-run choice; a server process must opt
    // in before applying.
    if !args.dry_run && !frontend.allows_config_import() {
        return Err(anyhow!(
            "configuration import is disabled; restart MCP with --allow-config-import to apply imports"
        ));
    }
    let mut store = load_store(frontend)?;
    enforce_transfer_permissions(&store, frontend)?;
    let result = store.import_from_preview(std::path::Path::new(args.local_path), args.dry_run)?;
    // Never return source JSON, credential material, or private file paths.
    Ok(json!({ "added": result.added, "skipped": result.skipped, "dry_run": args.dry_run }))
}

async fn upload_file(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let args = upload_args(arguments)?;
    let store = load_store(frontend)?;
    enforce_transfer_permissions(&store, frontend)?;
    drop(store);
    let (session, jump) = sftp_context(&args.target, frontend)?;
    let local_path = std::path::PathBuf::from(args.local_path);
    if !local_path.is_file() {
        return Err(anyhow!(
            "local upload source is not a regular file: {}",
            local_path.display()
        ));
    }
    super::sftp::transfer(
        session,
        jump,
        crate::sftp::SftpCommand::Upload {
            local: local_path,
            remote_dir: args.remote_directory.to_string(),
            cleanup_after: None,
            // Headless automation has nobody to ask; overwriting is the
            // documented behaviour of the upload tool.
            conflict: crate::sftp::UploadConflict::Replace,
        },
        true,
        args.target.timeout,
    )
    .await
}

async fn download_file(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let args = download_args(arguments)?;
    let store = load_store(frontend)?;
    enforce_transfer_permissions(&store, frontend)?;
    drop(store);
    let (session, jump) = sftp_context(&args.target, frontend)?;
    let remote_path = args.remote_path;
    let local_directory = std::path::PathBuf::from(args.local_directory);
    if !local_directory.is_dir() {
        return Err(anyhow!(
            "local download destination is not an existing directory: {}",
            local_directory.display()
        ));
    }
    let file_name = remote_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("remote_path must identify a file"))?;
    if local_directory.join(file_name).exists() {
        return Err(anyhow!(
            "download destination already exists: {}",
            local_directory.join(file_name).display()
        ));
    }
    let mut result = super::sftp::transfer(
        session,
        jump,
        crate::sftp::SftpCommand::Download {
            remote: remote_path.to_string(),
            local_dir: local_directory.to_string_lossy().into_owned(),
            conflict: crate::sftp::DownloadConflict::Replace,
        },
        false,
        args.target.timeout,
    )
    .await?;
    if let Some(object) = result.as_object_mut() {
        object.insert(
            "local_path".to_string(),
            json!(local_directory.join(file_name).to_string_lossy()),
        );
    }
    Ok(result)
}

fn enforce_transfer_permissions(store: &ConfigStore, frontend: Frontend) -> Result<()> {
    if frontend.is_mcp() && !store.mcp_allow_file_transfers() {
        return Err(anyhow!(
            "file transfers are disabled in Settings > Interface > MCP"
        ));
    }
    Ok(())
}

async fn list_remote_files(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let args = list_remote_files_args(arguments)?;
    let (session, jump) = sftp_context(&args.target, frontend)?;
    super::sftp::list(session, jump, args.path.to_string(), args.target.timeout).await
}

async fn read_remote_text_file(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let args = read_remote_text_file_args(arguments)?;
    let (session, jump) = sftp_context(&args.target, frontend)?;
    super::sftp::read_text(session, jump, args.path.to_string(), args.target.timeout).await
}

fn sftp_context(target: &SftpTarget<'_>, frontend: Frontend) -> Result<(Session, Vec<Session>)> {
    let store = load_store(frontend)?;
    if frontend.is_mcp() && !store.mcp_use_saved_credentials() {
        return Err(anyhow!(
            "using saved credentials is disabled in Settings > Interface > MCP"
        ));
    }
    let session = visible_session(&store, target.session_id, frontend)?.clone();
    if session.kind.as_str() != "ssh" {
        return Err(anyhow!("SFTP tools only support SSH sessions"));
    }
    let jump = visible_jump_chain(&store, &session, frontend)?;
    Ok((session, jump))
}

fn load_store(frontend: Frontend) -> Result<ConfigStore> {
    let store = ConfigStore::load().context("load MeatShell configuration")?;
    if frontend.is_mcp() && !store.mcp_enabled() {
        return Err(anyhow!("MCP is disabled in Settings > Interface > MCP"));
    }
    Ok(store)
}

/// Per-session MCP scope (#432). The CLI is an explicit local user action and
/// always sees every session.
fn exposed_to(session: &Session, frontend: Frontend) -> bool {
    !frontend.is_mcp() || session.mcp_access
}

/// Look up a session the caller may use. Sessions hidden from MCP report the
/// same error as missing ones so their existence is not disclosed.
fn visible_session<'a>(
    store: &'a ConfigStore,
    id: &str,
    frontend: Frontend,
) -> Result<&'a Session> {
    store
        .get(id)
        .filter(|session| exposed_to(session, frontend))
        .ok_or_else(|| anyhow!("session not found: {id}"))
}

/// Resolve the jump route, refusing it when any hop is hidden from MCP:
/// routing through a hop authenticates with that hop's saved credentials.
fn visible_jump_chain(
    store: &ConfigStore,
    session: &Session,
    frontend: Frontend,
) -> Result<Vec<Session>> {
    let jump = store.resolve_jump_chain(session)?;
    ensure_hops_exposed(&jump, frontend)?;
    Ok(jump)
}

fn ensure_hops_exposed(jump: &[Session], frontend: Frontend) -> Result<()> {
    if jump.iter().any(|hop| !exposed_to(hop, frontend)) {
        return Err(anyhow!(
            "this session routes through a jump host that does not allow MCP access"
        ));
    }
    Ok(())
}

fn list_sessions(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let args = list_sessions_args(arguments)?;
    let store = load_store(frontend)?;
    let sessions: Vec<Value> = store
        .sessions()
        .iter()
        .filter(|session| exposed_to(session, frontend))
        .filter(|session| args.group.map_or(true, |group| session.group == group))
        .map(safe_session)
        .collect();
    Ok(json!({ "sessions": sessions }))
}

fn get_session(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let id = session_id_arg(arguments)?;
    let store = load_store(frontend)?;
    Ok(safe_session(visible_session(&store, id, frontend)?))
}

async fn run_command(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let args = run_command_args(arguments)?;
    let store = load_store(frontend)?;
    if frontend.is_mcp() && !store.mcp_use_saved_credentials() {
        return Err(anyhow!(
            "using saved credentials is disabled in Settings > Interface > MCP"
        ));
    }
    if frontend.is_mcp() && !store.mcp_allow_commands() {
        return Err(anyhow!(
            "arbitrary command execution is disabled in Settings > Interface > MCP"
        ));
    }

    let session = visible_session(&store, args.session_id, frontend)?.clone();
    if session.kind.as_str() != "ssh" {
        return Err(anyhow!("run_command only supports SSH sessions"));
    }
    let jump = visible_jump_chain(&store, &session, frontend)?;

    let result = crate::ssh::execute_command(
        session,
        jump,
        args.command,
        args.timeout,
        args.max_output_bytes,
    )
    .await?;
    serde_json::to_value(result).context("serialize command result")
}

fn safe_session(session: &Session) -> Value {
    json!({
        "id": session.id,
        "name": session.name,
        "kind": session.kind.as_str(),
        "host": session.host,
        "port": session.port,
        "user": session.user,
        "auth": session.auth.as_str(),
        "group": session.group,
        "has_saved_password": !session.password.is_empty(),
        "has_private_key": !session.private_key_path.trim().is_empty()
            || !session.private_key_inline.is_empty(),
        "jump_session_id": session.jump_session_id,
        "jump_session_ids": session.jump_session_ids,
        "has_proxy": !session.proxy.trim().is_empty(),
    })
}

// ---------------------------------------------------------------------------
// Primitive argument readers
// ---------------------------------------------------------------------------

/// The session id, from `session_id` or one of its aliases. Aliases that are
/// present must agree with each other.
fn session_id_arg(arguments: &Value) -> Result<&str> {
    let mut found: Option<(&str, &str)> = None;
    for &key in SESSION_ID_KEYS {
        let Some(value) = optional_string(arguments, key)? else {
            continue;
        };
        match found {
            Some((first_key, first)) if first != value => {
                return Err(anyhow!(
                    "conflicting session id arguments: {first_key} and {key} differ"
                ))
            }
            Some(_) => {}
            None => found = Some((key, value)),
        }
    }
    found
        .map(|(_, value)| value)
        .ok_or_else(|| missing_argument(arguments, "session_id"))
}

/// Error for a missing required argument. Lists the argument *names* that did
/// arrive (never values) so a client or bridge that renames or drops keys is
/// diagnosable from the error alone.
fn missing_argument(arguments: &Value, key: &str) -> anyhow::Error {
    let received: Vec<&str> = arguments
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default();
    anyhow!(
        "missing or invalid string argument: {key} (received argument keys: [{}])",
        received.join(", ")
    )
}

fn timeout_arg(arguments: &Value, default_seconds: u64) -> Result<Duration> {
    let seconds = optional_u64(arguments, "timeout_seconds")?
        .unwrap_or(default_seconds)
        .clamp(1, MAX_TIMEOUT_SECONDS);
    Ok(Duration::from_secs(seconds))
}

fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| missing_argument(arguments, key))
}

/// Required string with `minLength: 1` in the advertised schema.
fn required_nonempty_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    let value = required_string(arguments, key)?;
    if value.trim().is_empty() {
        return Err(anyhow!("{key} must not be empty"));
    }
    Ok(value)
}

fn optional_string<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| anyhow!("invalid string argument: {key}")),
    }
}

fn optional_bool(arguments: &Value, key: &str) -> Result<Option<bool>> {
    match arguments.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| anyhow!("invalid boolean argument: {key}")),
    }
}

fn optional_u64(arguments: &Value, key: &str) -> Result<Option<u64>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| anyhow!("invalid positive integer argument: {key}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "4c6646ef-c12e-4544-8791-704c2cd7f9c3";

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn import_permissions_are_explicit() {
        assert!(Frontend::Cli.allows_config_import());
        assert!(!Frontend::Mcp {
            allow_config_import: false
        }
        .allows_config_import());
        assert!(Frontend::Mcp {
            allow_config_import: true
        }
        .allows_config_import());
        assert_eq!(optional_bool(&json!({}), "dry_run").unwrap(), None);
        assert_eq!(
            optional_bool(&json!({"dry_run": false}), "dry_run").unwrap(),
            Some(false)
        );
        assert!(optional_bool(&json!({"dry_run": "false"}), "dry_run").is_err());
        assert!(optional_bool(&json!({"dry_run": null}), "dry_run").is_err());
        let err = import_sessions(
            &json!({"local_path": "unused", "dry_run": false}),
            Frontend::Mcp {
                allow_config_import: false,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("--allow-config-import"));
    }

    #[test]
    fn mcp_scope_hides_opted_out_sessions_from_mcp_only() {
        let mcp = Frontend::Mcp {
            allow_config_import: false,
        };
        let mut hidden = Session::new_empty();
        hidden.mcp_access = false;
        let shown = Session::new_empty();
        assert!(!exposed_to(&hidden, mcp));
        assert!(exposed_to(&shown, mcp));
        assert!(exposed_to(&hidden, Frontend::Cli));

        assert!(ensure_hops_exposed(&[shown.clone()], mcp).is_ok());
        let err = ensure_hops_exposed(&[shown.clone(), hidden.clone()], mcp).unwrap_err();
        assert!(err.to_string().contains("jump host"));
        assert!(ensure_hops_exposed(&[hidden], Frontend::Cli).is_ok());
    }

    #[test]
    fn mcp_access_defaults_on_for_new_and_legacy_sessions() {
        let session = Session::new_empty();
        assert!(session.mcp_access);
        let mut legacy = serde_json::to_value(&session).unwrap();
        legacy.as_object_mut().unwrap().remove("mcp_access");
        let restored: Session = serde_json::from_value(legacy).unwrap();
        assert!(restored.mcp_access);
        let mut opted_out = session;
        opted_out.mcp_access = false;
        let json = serde_json::to_value(&opted_out).unwrap();
        assert!(!serde_json::from_value::<Session>(json).unwrap().mcp_access);
    }

    #[test]
    fn numeric_arguments_are_strict() {
        assert_eq!(optional_u64(&json!({}), "n").unwrap(), None);
        assert_eq!(optional_u64(&json!({ "n": 12 }), "n").unwrap(), Some(12));
        assert!(optional_u64(&json!({ "n": -1 }), "n").is_err());
        assert!(optional_u64(&json!({ "n": "12" }), "n").is_err());
    }

    #[test]
    fn list_sessions_with_and_without_group() {
        assert_eq!(
            list_sessions_args(&json!({})).unwrap(),
            ListSessionsArgs { group: None }
        );
        assert_eq!(
            list_sessions_args(&json!({ "group": "local" })).unwrap(),
            ListSessionsArgs {
                group: Some("local")
            }
        );
        assert_eq!(
            list_sessions_args(&json!({ "group": null })).unwrap(),
            ListSessionsArgs { group: None }
        );
        assert!(list_sessions_args(&json!({ "group": 1 })).is_err());
    }

    #[test]
    fn get_session_reads_snake_case_session_id() {
        assert_eq!(session_id_arg(&json!({ "session_id": ID })).unwrap(), ID);
        assert!(validate_arguments("get_session", &json!({ "session_id": ID })).is_ok());
    }

    #[test]
    fn session_id_aliases_are_accepted_and_must_agree() {
        assert_eq!(
            session_id_arg(&json!({ "saved_session_id": ID })).unwrap(),
            ID
        );
        assert_eq!(session_id_arg(&json!({ "sessionId": ID })).unwrap(), ID);
        assert_eq!(
            session_id_arg(&json!({ "session_id": ID, "saved_session_id": ID })).unwrap(),
            ID
        );
        let err =
            session_id_arg(&json!({ "session_id": ID, "saved_session_id": "other" })).unwrap_err();
        assert!(err.to_string().contains("conflicting"), "{err}");
        assert!(session_id_arg(&json!({ "session_id": 5 })).is_err());
    }

    #[test]
    fn missing_session_id_reports_received_keys_only() {
        let err = session_id_arg(&json!({ "command": "secret-value", "path": "/x" }))
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("missing or invalid string argument: session_id"),
            "{err}"
        );
        assert!(err.contains("command") && err.contains("path"), "{err}");
        assert!(!err.contains("secret-value"), "{err}");
        assert!(session_id_arg(&json!({}))
            .unwrap_err()
            .to_string()
            .contains("[]"));
    }

    #[test]
    fn run_command_parses_schema_shaped_arguments() {
        assert_eq!(
            run_command_args(&json!({ "session_id": ID, "command": "uname -a" })).unwrap(),
            RunCommandArgs {
                session_id: ID,
                command: "uname -a",
                timeout: secs(DEFAULT_TIMEOUT_SECONDS),
                max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            }
        );
        assert_eq!(
            run_command_args(&json!({
                "session_id": ID,
                "command": "ls",
                "timeout_seconds": 5,
                "max_output_bytes": 2048
            }))
            .unwrap(),
            RunCommandArgs {
                session_id: ID,
                command: "ls",
                timeout: secs(5),
                max_output_bytes: 2048,
            }
        );
        // Out-of-range values clamp to the advertised bounds.
        let value = json!({
            "session_id": ID,
            "command": "ls",
            "timeout_seconds": 9999,
            "max_output_bytes": 1
        });
        let clamped = run_command_args(&value).unwrap();
        assert_eq!(clamped.timeout, secs(MAX_TIMEOUT_SECONDS));
        assert_eq!(clamped.max_output_bytes, MIN_OUTPUT_BYTES);
        assert!(run_command_args(&json!({ "session_id": ID })).is_err());
        assert!(run_command_args(&json!({ "session_id": ID, "command": "  " })).is_err());
        assert!(run_command_args(&json!({ "command": "ls" })).is_err());
        assert!(run_command_args(&json!({
            "session_id": ID, "command": "ls", "timeout_seconds": "5"
        }))
        .is_err());
    }

    #[test]
    fn list_remote_files_defaults_path() {
        let value = json!({ "session_id": ID });
        let args = list_remote_files_args(&value).unwrap();
        assert_eq!(args.path, ".");
        assert_eq!(args.target.session_id, ID);
        assert_eq!(args.target.timeout, secs(DEFAULT_TIMEOUT_SECONDS));
        let value = json!({ "session_id": ID, "path": "/etc", "timeout_seconds": 10 });
        let args = list_remote_files_args(&value).unwrap();
        assert_eq!(args.path, "/etc");
        assert_eq!(args.target.timeout, secs(10));
    }

    #[test]
    fn read_remote_text_file_requires_path() {
        let value = json!({ "session_id": ID, "path": "/etc/hosts" });
        let args = read_remote_text_file_args(&value).unwrap();
        assert_eq!(args.path, "/etc/hosts");
        assert!(read_remote_text_file_args(&json!({ "session_id": ID })).is_err());
        assert!(read_remote_text_file_args(&json!({ "session_id": ID, "path": "" })).is_err());
    }

    #[test]
    fn upload_file_parses_schema_shaped_arguments() {
        let value = json!({
            "session_id": ID,
            "local_path": "C:\\tmp\\a.txt",
            "remote_directory": "/tmp"
        });
        let args = upload_args(&value).unwrap();
        assert_eq!(args.local_path, "C:\\tmp\\a.txt");
        assert_eq!(args.remote_directory, "/tmp");
        // Advertised default for transfers is 120 seconds.
        assert_eq!(args.target.timeout, secs(DEFAULT_TRANSFER_TIMEOUT_SECONDS));
        assert!(upload_args(&json!({ "session_id": ID, "local_path": "a" })).is_err());
        assert!(upload_args(&json!({
            "session_id": ID, "local_path": "a", "remote_directory": ""
        }))
        .is_err());
    }

    #[test]
    fn download_file_parses_schema_shaped_arguments() {
        let value = json!({
            "session_id": ID,
            "remote_path": "/var/log/syslog",
            "local_directory": "D:\\Downloads",
            "timeout_seconds": 60
        });
        let args = download_args(&value).unwrap();
        assert_eq!(args.remote_path, "/var/log/syslog");
        assert_eq!(args.local_directory, "D:\\Downloads");
        assert_eq!(args.target.timeout, secs(60));
        assert_eq!(
            download_args(&json!({
                "session_id": ID, "remote_path": "/a", "local_directory": "."
            }))
            .unwrap()
            .target
            .timeout,
            secs(DEFAULT_TRANSFER_TIMEOUT_SECONDS)
        );
        assert!(download_args(&json!({ "session_id": ID, "remote_path": "/a" })).is_err());
    }

    #[test]
    fn import_sessions_parses_schema_shaped_arguments() {
        assert_eq!(
            import_args(&json!({ "local_path": "export.json" })).unwrap(),
            ImportArgs {
                local_path: "export.json",
                dry_run: true
            }
        );
        assert!(
            !import_args(&json!({ "local_path": "x", "dry_run": false }))
                .unwrap()
                .dry_run
        );
        assert!(import_args(&json!({ "local_path": " " })).is_err());
        assert!(import_args(&json!({})).is_err());
    }
}
