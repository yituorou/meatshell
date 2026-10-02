use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::config::{ConfigStore, Session};

use super::structs::Frontend;

const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
const MAX_TIMEOUT_SECONDS: u64 = 300;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

pub(crate) async fn call(name: &str, arguments: &Value, frontend: Frontend) -> Result<Value> {
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

fn import_sessions(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let path = required_string(arguments, "local_path")?;
    if path.trim().is_empty() {
        return Err(anyhow!("local_path must not be empty"));
    }
    // Preview is the safe default for protocol callers. The CLI passes its
    // explicit --dry-run choice; a server process must opt in before applying.
    let dry_run = optional_bool(arguments, "dry_run")?.unwrap_or(true);
    if !dry_run && !frontend.allows_config_import() {
        return Err(anyhow!(
            "configuration import is disabled; restart MCP with --allow-config-import to apply imports"
        ));
    }
    let mut store = load_store(frontend)?;
    enforce_transfer_permissions(&store, frontend)?;
    let result = store.import_from_preview(std::path::Path::new(path), dry_run)?;
    // Never return source JSON, credential material, or private file paths.
    Ok(json!({ "added": result.added, "skipped": result.skipped, "dry_run": dry_run }))
}

async fn upload_file(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let store = load_store(frontend)?;
    enforce_transfer_permissions(&store, frontend)?;
    drop(store);
    let (session, jump, timeout) = sftp_context(arguments, frontend)?;
    let local_path = std::path::PathBuf::from(required_string(arguments, "local_path")?);
    if !local_path.is_file() {
        return Err(anyhow!(
            "local upload source is not a regular file: {}",
            local_path.display()
        ));
    }
    let remote_directory = required_string(arguments, "remote_directory")?;
    super::sftp::transfer(
        session,
        jump,
        crate::sftp::SftpCommand::Upload {
            local: local_path,
            remote_dir: remote_directory.to_string(),
            cleanup_after: None,
        },
        true,
        timeout,
    )
    .await
}

async fn download_file(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let store = load_store(frontend)?;
    enforce_transfer_permissions(&store, frontend)?;
    drop(store);
    let (session, jump, timeout) = sftp_context(arguments, frontend)?;
    let remote_path = required_string(arguments, "remote_path")?;
    let local_directory = std::path::PathBuf::from(required_string(arguments, "local_directory")?);
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
        timeout,
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
    let (session, jump, timeout) = sftp_context(arguments, frontend)?;
    let path = optional_string(arguments, "path")?
        .unwrap_or(".")
        .to_string();
    super::sftp::list(session, jump, path, timeout).await
}

async fn read_remote_text_file(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let (session, jump, timeout) = sftp_context(arguments, frontend)?;
    let path = required_string(arguments, "path")?;
    if path.trim().is_empty() {
        return Err(anyhow!("path must not be empty"));
    }
    super::sftp::read_text(session, jump, path.to_string(), timeout).await
}

fn sftp_context(
    arguments: &Value,
    frontend: Frontend,
) -> Result<(Session, Vec<Session>, Duration)> {
    let store = load_store(frontend)?;
    if frontend.is_mcp() && !store.mcp_use_saved_credentials() {
        return Err(anyhow!(
            "using saved credentials is disabled in Settings > Interface > MCP"
        ));
    }
    let id = required_string(arguments, "session_id")?;
    let session = store
        .get(id)
        .cloned()
        .ok_or_else(|| anyhow!("session not found: {id}"))?;
    if session.kind.as_str() != "ssh" {
        return Err(anyhow!("SFTP tools only support SSH sessions"));
    }
    let jump = store.resolve_jump_chain(&session)?;
    let timeout = optional_u64(arguments, "timeout_seconds")?
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
        .clamp(1, MAX_TIMEOUT_SECONDS);
    Ok((session, jump, Duration::from_secs(timeout)))
}

fn load_store(frontend: Frontend) -> Result<ConfigStore> {
    let store = ConfigStore::load().context("load MeatShell configuration")?;
    if frontend.is_mcp() && !store.mcp_enabled() {
        return Err(anyhow!("MCP is disabled in Settings > Interface > MCP"));
    }
    Ok(store)
}

fn list_sessions(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let store = load_store(frontend)?;
    let group = optional_string(arguments, "group")?;
    let sessions: Vec<Value> = store
        .sessions()
        .iter()
        .filter(|session| group.map_or(true, |group| session.group == group))
        .map(safe_session)
        .collect();
    Ok(json!({ "sessions": sessions }))
}

fn get_session(arguments: &Value, frontend: Frontend) -> Result<Value> {
    let store = load_store(frontend)?;
    let id = required_string(arguments, "session_id")?;
    let session = store
        .get(id)
        .ok_or_else(|| anyhow!("session not found: {id}"))?;
    Ok(safe_session(session))
}

async fn run_command(arguments: &Value, frontend: Frontend) -> Result<Value> {
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

    let id = required_string(arguments, "session_id")?;
    let command = required_string(arguments, "command")?;
    if command.trim().is_empty() {
        return Err(anyhow!("command must not be empty"));
    }
    let timeout_seconds = optional_u64(arguments, "timeout_seconds")?
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
        .clamp(1, MAX_TIMEOUT_SECONDS);
    let max_output_bytes = optional_u64(arguments, "max_output_bytes")?
        .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES as u64)
        .clamp(1024, MAX_OUTPUT_BYTES as u64) as usize;

    let session = store
        .get(id)
        .cloned()
        .ok_or_else(|| anyhow!("session not found: {id}"))?;
    if session.kind.as_str() != "ssh" {
        return Err(anyhow!("run_command only supports SSH sessions"));
    }
    let jump = store.resolve_jump_chain(&session)?;

    let result = crate::ssh::execute_command(
        session,
        jump,
        command,
        Duration::from_secs(timeout_seconds),
        max_output_bytes,
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

fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing or invalid string argument: {key}"))
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
    fn numeric_arguments_are_strict() {
        assert_eq!(optional_u64(&json!({}), "n").unwrap(), None);
        assert_eq!(optional_u64(&json!({ "n": 12 }), "n").unwrap(), Some(12));
        assert!(optional_u64(&json!({ "n": -1 }), "n").is_err());
        assert!(optional_u64(&json!({ "n": "12" }), "n").is_err());
    }
}
