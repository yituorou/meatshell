//! One-click launchers for coding-agent CLIs (#427).
//!
//! Like Warp's new-tab menu: the "⌄" next to each pane's "+" lists the local
//! shells and every known agent CLI found on this machine. Picking an agent
//! opens a local terminal tab (PowerShell on Windows, the login shell
//! elsewhere) that starts it; the shell stays open after the agent exits.
//! The same agents are listed in a read-only "agents" group in Quick connect.
//! Detection only looks for executables; nothing is installed or configured.
//! Agents meatshell does not know about can be added by hand under
//! Settings → AI agents (a name, a command and an optional start folder).

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;

use crate::config::{ConfigStore, CustomAgent, Session};

use slint::ComponentHandle as _;

use super::session_models::builtin_local_session;

/// Session-id prefix for agent launchers, next to the `system:` built-ins.
pub(super) const AGENT_ID_PREFIX: &str = "agent:";
/// User-defined agents: `agent:custom:<config id>`.
const CUSTOM_ID_PREFIX: &str = "agent:custom:";

/// Built-in Quick connect group holding the detected agents. Like "system" it
/// is generated, read-only (no edit / move / delete) and not saved to config.
pub(super) const AGENTS_GROUP: &str = "agents";

/// Result of the first detection pass, shared by every window.
static DETECTED: OnceLock<Vec<DetectedAgent>> = OnceLock::new();

pub(super) struct AgentSpec {
    /// Stable id suffix (`agent:<key>`).
    pub key: &'static str,
    /// Button and tab title.
    pub name: &'static str,
    /// Executable name without extension.
    pub program: &'static str,
}

/// Known agent CLIs, in button order.
pub(super) const AGENTS: &[AgentSpec] = &[
    AgentSpec {
        key: "claude",
        name: "Claude Code",
        program: "claude",
    },
    AgentSpec {
        key: "codex",
        name: "Codex",
        program: "codex",
    },
    AgentSpec {
        key: "gemini",
        name: "Gemini CLI",
        program: "gemini",
    },
    AgentSpec {
        key: "qwen",
        name: "Qwen Code",
        program: "qwen",
    },
    AgentSpec {
        key: "copilot",
        name: "Copilot CLI",
        program: "copilot",
    },
    AgentSpec {
        key: "opencode",
        name: "OpenCode",
        program: "opencode",
    },
    AgentSpec {
        key: "kimi",
        name: "Kimi CLI",
        program: "kimi",
    },
    AgentSpec {
        key: "cursor",
        name: "Cursor CLI",
        program: "cursor-agent",
    },
    AgentSpec {
        key: "codebuddy",
        name: "CodeBuddy",
        program: "codebuddy",
    },
    AgentSpec {
        key: "iflow",
        name: "iFlow CLI",
        program: "iflow",
    },
    AgentSpec {
        key: "crush",
        name: "Crush",
        program: "crush",
    },
    AgentSpec {
        key: "aider",
        name: "Aider",
        program: "aider",
    },
];

/// Extensions tried in order on Windows. A native `.exe` (Claude Code's own
/// installer) wins over npm's `.cmd` shim; `.ps1` shims are skipped because
/// the default execution policy refuses to run them.
#[cfg(windows)]
const EXTENSIONS: &[&str] = &[".exe", ".com", ".cmd", ".bat"];
#[cfg(not(windows))]
const EXTENSIONS: &[&str] = &[""];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DetectedAgent {
    pub key: &'static str,
    pub name: &'static str,
    pub path: PathBuf,
}

/// Every known agent installed on this machine, in `AGENTS` order.
pub(super) fn detect_agents() -> Vec<DetectedAgent> {
    let dirs = search_dirs();
    AGENTS
        .iter()
        .filter_map(|spec| {
            find_program(spec.program, &dirs, EXTENSIONS).map(|path| DetectedAgent {
                key: spec.key,
                name: spec.name,
                path,
            })
        })
        .collect()
}

/// The local session an `agent:…` id opens, or `None` when the id is not an
/// agent launcher, the custom agent was removed, or a detected agent is no
/// longer installed. Detected programs are resolved again on every click, so
/// an agent installed or removed after meatshell started is handled correctly.
pub(super) fn agent_session(store: &ConfigStore, id: &str) -> Option<Session> {
    if let Some(custom_id) = id.strip_prefix(CUSTOM_ID_PREFIX) {
        return store
            .custom_agents()
            .iter()
            .find(|agent| agent.id == custom_id)
            .map(custom_agent_session);
    }
    let spec = agent_spec(id)?;
    let path = find_program(spec.program, &search_dirs(), EXTENSIONS)?;
    Some(agent_session_for(spec, &path))
}

/// Display name of an `agent:…` id, whether or not it is installed.
pub(super) fn agent_name(store: &ConfigStore, id: &str) -> Option<String> {
    if let Some(custom_id) = id.strip_prefix(CUSTOM_ID_PREFIX) {
        return store
            .custom_agents()
            .iter()
            .find(|agent| agent.id == custom_id)
            .map(|agent| agent.name.clone());
    }
    agent_spec(id).map(|spec| spec.name.to_string())
}

fn agent_spec(id: &str) -> Option<&'static AgentSpec> {
    let key = id.strip_prefix(AGENT_ID_PREFIX)?;
    AGENTS.iter().find(|spec| spec.key == key)
}

/// Local shell session in the "agents" group that runs `command` (a command
/// line in the hosting shell's syntax). `label` is the Quick connect subtitle.
fn agent_shell_session(id: &str, name: &str, command: String, label: String) -> Session {
    #[cfg(windows)]
    let host = "powershell";
    #[cfg(not(windows))]
    let host = "shell";
    let mut session = builtin_local_session(id, name, host);
    session.group = AGENTS_GROUP.to_string();
    session.local_command = command;
    // Generated sessions are never opened in the session editor, so the note
    // field carries the Quick connect subtitle (the program, not the shell).
    session.note = label;
    session
}

fn agent_session_for(spec: &AgentSpec, path: &Path) -> Session {
    let path_text = path.to_string_lossy();
    // Call the resolved path, never the bare name: PowerShell would otherwise
    // prefer npm's `.ps1` shim, which the default execution policy blocks.
    #[cfg(windows)]
    let command = format!("& {}", crate::terminal::local::powershell_quote(&path_text));
    #[cfg(not(windows))]
    let command = crate::terminal::local::sh_quote(&path_text);
    let label = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    agent_shell_session(
        &format!("{AGENT_ID_PREFIX}{}", spec.key),
        spec.name,
        command,
        label,
    )
}

/// Program name for a command line's Quick connect subtitle: the first word,
/// or the quoted path (`"C:/My Tools/x.exe" -y`, PowerShell's `& '…'`),
/// without folder or extension.
fn program_label(command: &str) -> String {
    let line = command.trim().trim_start_matches('&').trim_start();
    let program = match line.chars().next() {
        Some(quote @ ('"' | '\'')) => line[1..].split(quote).next().unwrap_or_default(),
        _ => line.split_whitespace().next().unwrap_or_default(),
    };
    Path::new(program)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string())
}

fn custom_agent_session(agent: &CustomAgent) -> Session {
    let mut session = agent_shell_session(
        &format!("{CUSTOM_ID_PREFIX}{}", agent.id),
        &agent.name,
        agent.command.clone(),
        program_label(&agent.command),
    );
    session.local_working_dir = agent.directory.clone();
    session
}

/// Every agent launcher, in menu and Quick connect order: the detected agents
/// (empty until the first detection pass has finished), then the custom ones.
pub(super) fn agent_sessions(store: &ConfigStore) -> Vec<Session> {
    let detected = DETECTED.get().into_iter().flatten().filter_map(|agent| {
        AGENTS
            .iter()
            .find(|spec| spec.key == agent.key)
            .map(|spec| agent_session_for(spec, &agent.path))
    });
    detected
        .chain(store.custom_agents().iter().map(custom_agent_session))
        .collect()
}

/// `PATH`, then the usual per-user install locations. The fallbacks cover an
/// agent installed after meatshell started (Windows processes keep the PATH
/// they were launched with) and macOS apps opened from Finder, which get a
/// minimal PATH.
pub(super) fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    if let Some(home) = &home {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".bun").join("bin"));
    }
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("npm"));
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = &home {
            dirs.push(home.join(".npm-global").join("bin"));
            dirs.push(home.join(".cargo").join("bin"));
        }
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    dirs
}

/// First `dir/program<ext>` that is an executable file. Directories are
/// searched in order and every extension is tried per directory, matching how
/// shells resolve commands.
pub(super) fn find_program(
    program: &str,
    dirs: &[PathBuf],
    extensions: &[&str],
) -> Option<PathBuf> {
    dirs.iter()
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| {
            extensions.iter().map(move |ext| {
                let mut name = OsString::from(program);
                name.push(ext);
                dir.join(name)
            })
        })
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Wire the agent launchers of one window: the tab bar's launch menu, the
/// Quick connect "agents" group and the Settings → AI agents page. Detection
/// touches the filesystem for every PATH entry, so the first window runs it
/// off the UI thread and never delays startup; later windows reuse the result.
pub(super) fn wire_agent_launchers(
    ctx: &super::WinCtx,
    sessions_model: &Rc<slint::VecModel<super::SessionInfo>>,
) {
    let window = &ctx.window;

    // Rebuild everything that lists agents in this window.
    {
        let weak = window.as_weak();
        let store = ctx.store.clone();
        let sessions_model = sessions_model.clone();
        window.on_refresh_agent_launchers(move || {
            let Some(w) = weak.upgrade() else { return };
            refresh(&w, &weak, &store, &sessions_model);
        });
    }
    {
        let weak = window.as_weak();
        let store = ctx.store.clone();
        let registry = ctx.registry.clone();
        window.on_add_custom_agent(move |name, command, directory| {
            let added = {
                let mut s = store.borrow_mut();
                let added = s.add_custom_agent(&name, &command, &directory);
                if added {
                    let _ = s.save();
                }
                added
            };
            if added {
                if let Some(w) = weak.upgrade() {
                    w.set_agent_new_name("".into());
                    w.set_agent_new_command("".into());
                    w.set_agent_new_directory("".into());
                }
                // Every window (this one included) refreshes from the store.
                registry.broadcast_config_changed();
            }
        });
    }
    {
        let store = ctx.store.clone();
        let registry = ctx.registry.clone();
        window.on_remove_custom_agent(move |id| {
            {
                let mut s = store.borrow_mut();
                s.remove_custom_agent(&id);
                let _ = s.save();
            }
            registry.broadcast_config_changed();
        });
    }
    {
        let weak = window.as_weak();
        window.on_pick_agent_directory(move || {
            if let Some(folder) = super::DialogOwner::of_weak(&weak).file().pick_folder() {
                if let Some(w) = weak.upgrade() {
                    w.set_agent_new_directory(folder.to_string_lossy().to_string().into());
                }
            }
        });
    }
    // A custom agent added or removed in any window reaches every window via
    // the config broadcast, whose listener (listen_for_config_changes) calls
    // refresh-agent-launchers. The registry keeps one listener per window, so
    // this module must not register its own.

    window.invoke_refresh_agent_launchers();
    if DETECTED.get().is_some() {
        return;
    }
    let weak = window.as_weak();
    let spawned = std::thread::Builder::new()
        .name("agent-detect".into())
        .spawn(move || {
            // Extra local shells (#533, #538) share this off-thread pass.
            let shells = super::local_shells::detect_once();
            tracing::info!(
                "local shells detected: {}",
                shells.iter().map(|s| s.name).collect::<Vec<_>>().join(", ")
            );
            let agents = DETECTED.get_or_init(detect_agents);
            tracing::info!(
                "agent launchers detected: {}",
                agents.iter().map(|a| a.name).collect::<Vec<_>>().join(", ")
            );
            let _ = weak.upgrade_in_event_loop(|w| w.invoke_refresh_agent_launchers());
        });
    if let Err(err) = spawned {
        tracing::warn!("agent detection thread failed to start: {err}");
    }
}

fn refresh(
    w: &super::AppWindow,
    weak: &slint::Weak<super::AppWindow>,
    store: &Rc<RefCell<ConfigStore>>,
    sessions_model: &slint::VecModel<super::SessionInfo>,
) {
    let store = store.borrow();
    let menu: Vec<super::LaunchItem> = agent_sessions(&store)
        .into_iter()
        .map(|session| super::LaunchItem {
            id: session.id.into(),
            name: session.name.into(),
        })
        .collect();
    w.set_agents(slint::ModelRc::new(slint::VecModel::from(menu)));
    let custom: Vec<super::CustomAgentInfo> = store
        .custom_agents()
        .iter()
        .map(|agent| super::CustomAgentInfo {
            id: agent.id.clone().into(),
            name: agent.name.clone().into(),
            command: agent.command.clone().into(),
            directory: agent.directory.clone().into(),
        })
        .collect();
    w.set_custom_agents(slint::ModelRc::new(slint::VecModel::from(custom)));
    // Detected and user-defined local shells (#533, #538) live in the same
    // launch menu / Quick connect lists, so refresh them on the same signal.
    w.set_launch_shells(super::launch_shell_model(&store));
    super::local_shells::refresh_settings(w, &store);
    super::sync_sessions_for_window(weak, &store, sessions_model);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_executable(path: &Path) {
        std::fs::write(path, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn earlier_directory_wins_and_extensions_are_tried_in_order() {
        let root = std::env::temp_dir().join(format!("meatshell-agents-{}", uuid::Uuid::new_v4()));
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        touch_executable(&first.join("codex.cmd"));
        touch_executable(&first.join("codex.exe"));
        touch_executable(&second.join("codex.exe"));

        let dirs = vec![PathBuf::new(), first.clone(), second.clone()];
        assert_eq!(
            find_program("codex", &dirs, &[".exe", ".cmd"]),
            Some(first.join("codex.exe"))
        );
        assert_eq!(
            find_program("codex", &dirs, &[".cmd", ".exe"]),
            Some(first.join("codex.cmd"))
        );
        assert_eq!(find_program("gemini", &dirs, &[".exe", ".cmd"]), None);
        // A directory with the program's name is not a match.
        std::fs::create_dir_all(second.join("qwen.exe")).unwrap();
        assert_eq!(find_program("qwen", &dirs, &[".exe"]), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn non_executable_files_are_skipped() {
        let dir = std::env::temp_dir().join(format!("meatshell-agents-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("claude"), b"").unwrap();
        assert_eq!(find_program("claude", &[dir.clone()], &[""]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_ids_map_to_local_sessions_running_the_program() {
        let spec = agent_spec("agent:claude").unwrap();
        let path = PathBuf::from("/opt/agents/claude");
        let session = agent_session_for(spec, &path);
        assert_eq!(session.id, "agent:claude");
        assert_eq!(session.name, "Claude Code");
        assert_eq!(session.kind, crate::config::SessionKind::Local);
        assert_eq!(session.group, AGENTS_GROUP);
        assert_eq!(session.note, "claude");
        #[cfg(windows)]
        assert_eq!(session.local_command, "& '/opt/agents/claude'");
        #[cfg(not(windows))]
        assert_eq!(session.local_command, "'/opt/agents/claude'");

        assert!(agent_spec("agent:unknown").is_none());
        assert!(agent_spec("system:powershell").is_none());
        assert!(agent_spec("agent:custom:claude").is_none());
    }

    #[test]
    fn custom_agents_run_their_command_line_in_their_folder() {
        let agent = CustomAgent {
            id: "42".into(),
            name: "My agent".into(),
            command: r#""C:/Tools/My Agent/agent.exe" --fast"#.into(),
            directory: r"D:\work".into(),
        };
        let session = custom_agent_session(&agent);
        assert_eq!(session.id, "agent:custom:42");
        assert_eq!(session.name, "My agent");
        assert_eq!(session.group, AGENTS_GROUP);
        assert_eq!(session.local_command, agent.command);
        assert_eq!(session.local_working_dir, r"D:\work");
        assert_eq!(session.note, "agent");
    }

    #[test]
    fn program_label_finds_the_program_in_a_command_line() {
        assert_eq!(program_label("claude --model opus"), "claude");
        assert_eq!(program_label("npx -y @scope/agent"), "npx");
        assert_eq!(
            program_label(r#""C:/Tools/My Agent/agent.exe" --fast"#),
            "agent"
        );
        assert_eq!(program_label("& 'C:/npm/codex.cmd'"), "codex");
        assert_eq!(program_label("/opt/bin/qwen"), "qwen");
    }

    #[test]
    fn agent_keys_are_unique() {
        let mut keys: Vec<_> = AGENTS.iter().map(|spec| spec.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), AGENTS.len());
    }
}
