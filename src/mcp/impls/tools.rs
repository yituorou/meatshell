use anyhow::Result;
use serde_json::{json, Value};

/// Alias for `session_id`. Claude desktop's remote-devices bridge consumes a
/// top-level `session_id` tool argument and does not forward it, so a client
/// behind such a bridge can send `saved_session_id` instead. Clients that
/// already send `session_id` are unaffected.
const SESSION_ID_ALIAS: &str = "saved_session_id";

pub(super) fn definitions() -> Value {
    let mut tools = base_definitions();
    for tool in tools.as_array_mut().expect("tool array") {
        let schema = &mut tool["inputSchema"];
        let Some(session_id) = schema["properties"].get_mut("session_id") else {
            continue;
        };
        session_id["description"] = json!(
            "Stable session id returned by list_sessions. Required unless saved_session_id is given."
        );
        schema["properties"][SESSION_ID_ALIAS] = json!({
            "type": "string",
            "description": "Same as session_id. Use this when your MCP client or bridge drops a session_id argument. Exactly one of session_id or saved_session_id is required."
        });
        if let Some(required) = schema["required"].as_array_mut() {
            required.retain(|key| key != "session_id");
        }
    }
    tools
}

fn base_definitions() -> Value {
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
            "description": "List saved MeatShell sessions without exposing passwords, private keys, or other secrets. Sessions the user has not allowed for MCP access are omitted. When the profile folder was chosen automatically, data_dir names the folder the sessions were read from.",
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
            "description": "Get non-secret connection metadata for one saved MeatShell session. When the profile folder was chosen automatically, data_dir names the folder it was read from.",
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

    const ID: &str = "4c6646ef-c12e-4544-8791-704c2cd7f9c3";

    fn tools() -> Vec<Value> {
        definitions().as_array().unwrap().clone()
    }

    fn name(tool: &Value) -> &str {
        tool["name"].as_str().unwrap()
    }

    /// A valid value for one advertised property, derived from its schema.
    fn sample(key: &str, property: &Value) -> Value {
        match property["type"].as_str().unwrap() {
            "string" if crate::automation::SESSION_ID_KEYS.contains(&key) => json!(ID),
            "string" => json!(format!("sample-{key}")),
            "integer" => property
                .get("default")
                .or_else(|| property.get("minimum"))
                .cloned()
                .unwrap_or(json!(1)),
            "boolean" => property.get("default").cloned().unwrap_or(json!(true)),
            other => panic!("unhandled schema type {other} for {key}"),
        }
    }

    fn required(tool: &Value) -> Vec<String> {
        tool["inputSchema"]["required"]
            .as_array()
            .map(|keys| {
                keys.iter()
                    .map(|key| key.as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn has_session_id(tool: &Value) -> bool {
        tool["inputSchema"]["properties"].get("session_id").is_some()
    }

    /// Required properties, plus `session_id` for session tools.
    fn minimal_arguments(tool: &Value) -> serde_json::Map<String, Value> {
        let properties = &tool["inputSchema"]["properties"];
        let mut keys = required(tool);
        if has_session_id(tool) {
            keys.push("session_id".into());
        }
        keys.into_iter()
            .map(|key| {
                let value = sample(&key, &properties[&key]);
                (key, value)
            })
            .collect()
    }

    #[test]
    fn every_tool_accepts_its_minimal_schema_shaped_arguments() {
        for tool in tools() {
            let args = Value::Object(minimal_arguments(&tool));
            crate::automation::validate_arguments(name(&tool), &args)
                .unwrap_or_else(|e| panic!("{}({args}): {e:#}", name(&tool)));
        }
    }

    #[test]
    fn every_tool_accepts_all_advertised_properties() {
        for tool in tools() {
            let args: serde_json::Map<String, Value> = tool["inputSchema"]["properties"]
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, _)| key.as_str() != SESSION_ID_ALIAS)
                .map(|(key, property)| (key.clone(), sample(key, property)))
                .collect();
            let args = Value::Object(args);
            crate::automation::validate_arguments(name(&tool), &args)
                .unwrap_or_else(|e| panic!("{}({args}): {e:#}", name(&tool)));
        }
    }

    #[test]
    fn every_required_argument_is_enforced() {
        for tool in tools() {
            let mut keys = required(&tool);
            if has_session_id(&tool) {
                keys.push("session_id".into());
            }
            for key in keys {
                let mut args = minimal_arguments(&tool);
                args.remove(&key);
                let err = crate::automation::validate_arguments(name(&tool), &Value::Object(args))
                    .expect_err(&format!("{} accepted arguments without {key}", name(&tool)));
                assert!(err.to_string().contains(&key), "{}: {err:#}", name(&tool));
            }
        }
    }

    #[test]
    fn session_tools_advertise_and_accept_the_alias() {
        let session_tools: Vec<Value> = tools().into_iter().filter(has_session_id).collect();
        let names: Vec<&str> = session_tools.iter().map(name).collect();
        assert_eq!(
            names,
            [
                "get_session",
                "run_command",
                "list_remote_files",
                "read_remote_text_file",
                "upload_file",
                "download_file"
            ]
        );
        assert!(crate::automation::SESSION_ID_KEYS.contains(&SESSION_ID_ALIAS));
        for tool in &session_tools {
            let schema = &tool["inputSchema"];
            assert_eq!(schema["properties"][SESSION_ID_ALIAS]["type"], "string");
            assert_eq!(schema["additionalProperties"], false);
            let mut args = minimal_arguments(tool);
            args.remove("session_id");
            args.insert(SESSION_ID_ALIAS.into(), json!(ID));
            crate::automation::validate_arguments(name(tool), &Value::Object(args))
                .unwrap_or_else(|e| panic!("{} via alias: {e:#}", name(tool)));
        }
    }

    #[test]
    fn non_session_tools_are_unchanged() {
        for tool in tools().iter().filter(|tool| !has_session_id(tool)) {
            assert!(tool["inputSchema"]["properties"]
                .get(SESSION_ID_ALIAS)
                .is_none());
        }
        let import = tools()
            .into_iter()
            .find(|tool| name(tool) == "import_sessions")
            .unwrap();
        assert_eq!(required(&import), ["local_path"]);
    }
}
