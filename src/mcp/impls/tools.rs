use anyhow::Result;
use serde_json::{json, Value};

pub(super) fn definitions() -> Value {
    json!([
        {
            "name": "import_sessions",
            "description": "Preview or append sessions from a local MeatShell portable export, native JSON profile, or FinalShell export. Existing sessions/settings are preserved; duplicates are skipped. Returns counts only. Requires file-transfer permission; applying also requires server startup with --allow-config-import. Export files can contain recoverable credentials.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "local_path": { "type": "string", "minLength": 1, "description": "Export file path on the MCP server's machine (maximum 16 MiB)." },
                    "dry_run": { "type": "boolean", "default": true, "description": "Preview only unless explicitly false." }
                },
                "required": ["local_path"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "list_sessions",
            "description": "List saved MeatShell sessions without exposing passwords, private keys, or other secrets.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "group": { "type": "string", "description": "Optional exact session group filter." }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "get_session",
            "description": "Get non-secret connection metadata for one saved MeatShell session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "Stable session id returned by list_sessions." }
                },
                "required": ["session_id"],
                "additionalProperties": false
            }
        },
        {
            "name": "run_command",
            "description": "Execute one non-interactive command on a saved SSH session. Requires the MCP saved-credentials and arbitrary-command permissions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "Stable session id returned by list_sessions." },
                    "command": { "type": "string", "minLength": 1 },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 300, "default": 30 },
                    "max_output_bytes": { "type": "integer", "minimum": 1024, "maximum": 4194304, "default": 1048576 }
                },
                "required": ["session_id", "command"],
                "additionalProperties": false
            }
        },
        {
            "name": "list_remote_files",
            "description": "List a remote directory over MeatShell SFTP without exposing credentials.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "path": { "type": "string", "default": "." },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 300, "default": 30 }
                },
                "required": ["session_id"],
                "additionalProperties": false
            }
        },
        {
            "name": "read_remote_text_file",
            "description": "Read a bounded UTF-8 text file over MeatShell SFTP. Binary, oversized, or excessively long files are rejected.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "path": { "type": "string", "minLength": 1 },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 300, "default": 30 }
                },
                "required": ["session_id", "path"],
                "additionalProperties": false
            }
        },
        {
            "name": "upload_file",
            "description": "Upload one local file to a remote directory over MeatShell SFTP. Requires the MCP file-transfer permission.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "local_path": { "type": "string", "minLength": 1 },
                    "remote_directory": { "type": "string", "minLength": 1 },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 300, "default": 120 }
                },
                "required": ["session_id", "local_path", "remote_directory"],
                "additionalProperties": false
            }
        },
        {
            "name": "download_file",
            "description": "Download one remote file into an existing local directory over MeatShell SFTP. Existing files are not overwritten. Requires the MCP file-transfer permission.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "remote_path": { "type": "string", "minLength": 1 },
                    "local_directory": { "type": "string", "minLength": 1 },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 300, "default": 120 }
                },
                "required": ["session_id", "remote_path", "local_directory"],
                "additionalProperties": false
            }
        }
    ])
}

pub(super) async fn call_mcp(
    name: &str,
    arguments: &Value,
    allow_config_import: bool,
) -> Result<Value> {
    crate::automation::call(
        name,
        arguments,
        crate::automation::Frontend::Mcp {
            allow_config_import,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_do_not_expose_secret_arguments() {
        let text = definitions().to_string();
        assert!(!text.contains("\"password\":"));
        assert!(!text.contains("\"private_key_inline\":"));
        assert!(text.contains("run_command"));
    }
}
