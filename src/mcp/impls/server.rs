use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use serde_json::{json, Value};

const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    "2024-11-05",
    "2025-03-26",
    "2025-06-18",
    LATEST_PROTOCOL_VERSION,
];

pub(crate) fn run_stdio() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("create MCP runtime")?;
    let allow_config_import = std::env::args().any(|arg| arg == "--allow-config-import");
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.context("read MCP request")?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => runtime.block_on(handle(request, allow_config_import)),
            Err(error) => Some(error_response(Value::Null, -32700, &error.to_string())),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut stdout, &response).context("write MCP response")?;
            stdout.write_all(b"\n").context("finish MCP response")?;
            stdout.flush().context("flush MCP response")?;
        }
    }
    Ok(())
}

async fn handle(request: Value, allow_config_import: bool) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str);
    if id.is_none() {
        return None;
    }
    let id = id.unwrap_or(Value::Null);
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    match method {
        Some("initialize") => Some(success_response(id, initialize(&params))),
        Some("ping") => Some(success_response(id, json!({}))),
        Some("tools/list") => Some(success_response(
            id,
            json!({ "tools": super::tools::definitions() }),
        )),
        Some("tools/call") => Some(call_tool(id, &params, allow_config_import).await),
        Some(_) => Some(error_response(id, -32601, "method not found")),
        None => Some(error_response(id, -32600, "invalid request")),
    }
}

fn initialize(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(LATEST_PROTOCOL_VERSION);
    let protocol_version = SUPPORTED_PROTOCOL_VERSIONS
        .contains(&requested)
        .then_some(requested)
        .unwrap_or(LATEST_PROTOCOL_VERSION);
    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "meatshell",
            "title": "MeatShell MCP",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "Manage saved MeatShell sessions and run permitted SSH automation without exposing stored secrets."
    })
}

async fn call_tool(id: Value, params: &Value, allow_config_import: bool) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return error_response(id, -32602, "missing tool name");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match super::tools::call_mcp(name, &arguments, allow_config_import).await {
        Ok(value) => success_response(
            id,
            json!({
                "content": [{ "type": "text", "text": pretty_json(&value) }],
                "structuredContent": value,
                "isError": false
            }),
        ),
        Err(error) => success_response(
            id,
            json!({
                "content": [{ "type": "text", "text": format!("{error:#}") }],
                "isError": true
            }),
        ),
    }
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initialize_negotiates_a_supported_version() {
        let response = handle(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": "2025-06-18" }
            }),
            false,
        )
        .await
        .unwrap();
        assert_eq!(response["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(response["result"]["serverInfo"]["name"], "meatshell");
    }

    #[tokio::test]
    async fn notifications_do_not_receive_responses() {
        assert!(handle(
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
            false
        )
        .await
        .is_none());
    }

    #[tokio::test]
    async fn lists_tools() {
        let response = handle(
            json!({
                "jsonrpc": "2.0",
                "id": "tools",
                "method": "tools/list"
            }),
            false,
        )
        .await
        .unwrap();
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 8);
    }
}
