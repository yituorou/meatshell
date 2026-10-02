use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use super::structs::CliCommand;

async fn call_cli(name: &str, arguments: &Value) -> Result<Value> {
    crate::automation::call(name, arguments, crate::automation::Frontend::Cli).await
}

pub(crate) fn run(args: &[String]) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("create CLI runtime")?;
    let command_name = args.get(2).map(String::as_str).unwrap_or("help");
    let command = CliCommand::parse(Some(command_name))
        .ok_or_else(|| anyhow!("unknown CLI command: {command_name}"))?;
    let value = match command {
        CliCommand::Import => {
            let path = args
                .get(3)
                .filter(|arg| !arg.starts_with("--"))
                .ok_or_else(|| {
                    anyhow!("usage: meatshell cli import <export.json> [--dry-run] [--json]")
                })?;
            for arg in &args[4..] {
                if !matches!(arg.as_str(), "--dry-run" | "--json") {
                    return Err(anyhow!(
                        "unknown import option (expected --dry-run or --json)"
                    ));
                }
            }
            runtime.block_on(call_cli(
                "import_sessions",
                &json!({
                    "local_path": path,
                    "dry_run": args.iter().any(|arg| arg == "--dry-run")
                }),
            ))?
        }
        CliCommand::Sessions => {
            let group = option_value(args, "--group")?;
            runtime.block_on(call_cli("list_sessions", &json!({ "group": group })))?
        }
        CliCommand::Session => {
            let id = args
                .get(3)
                .ok_or_else(|| anyhow!("usage: meatshell cli session <session-id> [--json]"))?;
            runtime.block_on(call_cli("get_session", &json!({ "session_id": id })))?
        }
        CliCommand::Exec => {
            let id = args.get(3).ok_or_else(|| {
                anyhow!("usage: meatshell cli exec <session-id> [--timeout <seconds>] [--json] -- <command>")
            })?;
            let delimiter = args
                .iter()
                .position(|arg| arg == "--")
                .ok_or_else(|| anyhow!("exec command must follow --"))?;
            let remote_command = args[delimiter + 1..].join(" ");
            if remote_command.trim().is_empty() {
                return Err(anyhow!("remote command must not be empty"));
            }
            let timeout = option_value(args, "--timeout")?
                .map(|value| value.parse::<u64>())
                .transpose()
                .context("--timeout must be a positive integer")?
                .unwrap_or(30);
            runtime.block_on(call_cli(
                "run_command",
                &json!({
                    "session_id": id,
                    "command": remote_command,
                    "timeout_seconds": timeout
                }),
            ))?
        }
        CliCommand::Files => {
            let id = args.get(3).ok_or_else(|| {
                anyhow!("usage: meatshell cli files <session-id> [path] [--json]")
            })?;
            let path = args
                .get(4)
                .filter(|value| !value.starts_with("--"))
                .map(String::as_str)
                .unwrap_or(".");
            runtime.block_on(call_cli(
                "list_remote_files",
                &json!({ "session_id": id, "path": path }),
            ))?
        }
        CliCommand::Read => {
            let id = args.get(3).ok_or_else(|| {
                anyhow!("usage: meatshell cli read <session-id> <remote-path> [--json]")
            })?;
            let path = args.get(4).ok_or_else(|| {
                anyhow!("usage: meatshell cli read <session-id> <remote-path> [--json]")
            })?;
            runtime.block_on(call_cli(
                "read_remote_text_file",
                &json!({ "session_id": id, "path": path }),
            ))?
        }
        CliCommand::Upload => {
            let id = args.get(3).ok_or_else(|| {
                anyhow!("usage: meatshell cli upload <session-id> <local-path> <remote-directory> [--json]")
            })?;
            let local_path = args.get(4).ok_or_else(|| anyhow!("missing local path"))?;
            let remote_directory = args
                .get(5)
                .ok_or_else(|| anyhow!("missing remote directory"))?;
            runtime.block_on(call_cli(
                "upload_file",
                &json!({
                    "session_id": id,
                    "local_path": local_path,
                    "remote_directory": remote_directory,
                    "timeout_seconds": 120
                }),
            ))?
        }
        CliCommand::Download => {
            let id = args.get(3).ok_or_else(|| {
                anyhow!("usage: meatshell cli download <session-id> <remote-path> <local-directory> [--json]")
            })?;
            let remote_path = args.get(4).ok_or_else(|| anyhow!("missing remote path"))?;
            let local_directory = args
                .get(5)
                .ok_or_else(|| anyhow!("missing local directory"))?;
            runtime.block_on(call_cli(
                "download_file",
                &json!({
                    "session_id": id,
                    "remote_path": remote_path,
                    "local_directory": local_directory,
                    "timeout_seconds": 120
                }),
            ))?
        }
        CliCommand::Help => {
            print_help();
            return Ok(());
        }
    };

    if args.iter().any(|arg| arg == "--json") {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        print_human(command, &value);
    }
    Ok(())
}

fn option_value<'a>(args: &'a [String], option: &str) -> Result<Option<&'a str>> {
    let Some(index) = args.iter().position(|arg| arg == option) else {
        return Ok(None);
    };
    args.get(index + 1)
        .filter(|value| !value.starts_with("--"))
        .map(|value| Some(value.as_str()))
        .ok_or_else(|| anyhow!("missing value for {option}"))
}

fn print_human(command: CliCommand, value: &Value) {
    match command {
        CliCommand::Import => println!(
            "{} {} sessions, skipped {} duplicates",
            if value.get("dry_run").and_then(Value::as_bool) == Some(true) {
                "Would import"
            } else {
                "Imported"
            },
            value.get("added").and_then(Value::as_u64).unwrap_or(0),
            value.get("skipped").and_then(Value::as_u64).unwrap_or(0)
        ),
        CliCommand::Sessions => {
            if let Some(sessions) = value.get("sessions").and_then(Value::as_array) {
                for session in sessions {
                    println!(
                        "{}\t{}@{}:{}\t{}",
                        text(session, "id"),
                        text(session, "user"),
                        text(session, "host"),
                        session.get("port").and_then(Value::as_u64).unwrap_or(0),
                        text(session, "name")
                    );
                }
            }
        }
        CliCommand::Session => println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        ),
        CliCommand::Exec => {
            print!("{}", text(value, "stdout"));
            eprint!("{}", text(value, "stderr"));
            if value.get("timed_out").and_then(Value::as_bool) == Some(true) {
                eprintln!("command timed out");
            }
        }
        CliCommand::Files => {
            if let Some(entries) = value.get("entries").and_then(Value::as_array) {
                for entry in entries {
                    println!(
                        "{}\t{}\t{}",
                        if entry.get("is_directory").and_then(Value::as_bool) == Some(true) {
                            "dir"
                        } else {
                            "file"
                        },
                        entry.get("size").and_then(Value::as_u64).unwrap_or(0),
                        text(entry, "path")
                    );
                }
            }
        }
        CliCommand::Read => print!("{}", text(value, "content")),
        CliCommand::Upload | CliCommand::Download => println!(
            "{} {} bytes",
            text(value, "name"),
            value
                .get("transferred")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ),
        CliCommand::Help => println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        ),
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn print_help() {
    println!(
        "MeatShell CLI\n\n\
         Usage:\n\
           meatshell cli import <export.json> [--dry-run] [--json]\n\
           meatshell cli sessions [--group <name>] [--json]\n\
           meatshell cli session <session-id> [--json]\n\
           meatshell cli exec <session-id> [--timeout <seconds>] [--json] -- <command>\n\
           meatshell cli files <session-id> [path] [--json]\n\
           meatshell cli read <session-id> <remote-path> [--json]\n\
           meatshell cli upload <session-id> <local-path> <remote-directory> [--json]\n\
           meatshell cli download <session-id> <remote-path> <local-directory> [--json]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_option_values() {
        let args = vec![
            "meatshell".into(),
            "cli".into(),
            "sessions".into(),
            "--group".into(),
            "prod".into(),
        ];
        assert_eq!(option_value(&args, "--group").unwrap(), Some("prod"));
        assert_eq!(option_value(&args, "--json").unwrap(), None);
    }
}
