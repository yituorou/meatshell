//! Local terminal session worker.
//!
//! Local shells need a real PTY/ConPTY. Plain stdin/stdout pipes break normal
//! console editing (Backspace/Delete/IME composition) and make Windows shells
//! disagree about encodings. `portable-pty` gives us ConPTY on Windows and a
//! Unix PTY on Linux/macOS while reusing the same UI event path as SSH.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, LazyLock, Mutex};

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
    let tab_for_task = tab_id.clone();
    let join = runtime.spawn(async move {
        if let Err(err) = run_local(
            session,
            tab_for_task,
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

/// Shell PID of every running local-terminal tab, keyed by tab id, so the tab
/// menu can find an `ssh` the user started in it (#501). Tab ids are UUIDs,
/// unique across windows.
static SHELL_PIDS: LazyLock<Mutex<HashMap<String, u32>>> = LazyLock::new(Default::default);

/// Keeps a tab's [`SHELL_PIDS`] entry for as long as its shell worker runs.
struct ShellPidGuard {
    tab_id: String,
    pid: u32,
}

impl ShellPidGuard {
    fn register(tab_id: &str, pid: u32) -> Self {
        SHELL_PIDS.lock().unwrap().insert(tab_id.to_string(), pid);
        Self {
            tab_id: tab_id.to_string(),
            pid,
        }
    }
}

impl Drop for ShellPidGuard {
    fn drop(&mut self) {
        let mut pids = SHELL_PIDS.lock().unwrap();
        // An in-place reconnect may already have registered the new shell.
        if pids.get(&self.tab_id) == Some(&self.pid) {
            pids.remove(&self.tab_id);
        }
    }
}

/// One process, as far as finding an `ssh` below a shell needs (#501).
#[derive(Debug, Clone)]
pub(crate) struct ProcessRecord {
    pub pid: u32,
    pub parent: Option<u32>,
    pub name: String,
    pub start_time: u64,
    pub cmd: Vec<String>,
}

fn is_ssh_program(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "ssh" || name == "ssh.exe"
}

/// The `ssh` the user is running under `root`: a descendant of the shell that
/// is not itself below another `ssh` (which skips the `ssh -W` helpers that
/// `-J` / ProxyJump spawn). With several, the most recently started wins.
pub(crate) fn find_user_ssh(processes: &[ProcessRecord], root: u32) -> Option<&ProcessRecord> {
    let mut children: HashMap<u32, Vec<&ProcessRecord>> = HashMap::new();
    for process in processes {
        if let Some(parent) = process.parent {
            if parent != process.pid {
                children.entry(parent).or_default().push(process);
            }
        }
    }
    // A "child" older than the shell is a PID-reuse artefact, not ours.
    let root_started = processes
        .iter()
        .find(|p| p.pid == root)
        .map_or(0, |p| p.start_time);
    let mut found: Vec<&ProcessRecord> = Vec::new();
    let mut seen = std::collections::HashSet::from([root]);
    let mut queue = std::collections::VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        for child in children.get(&pid).into_iter().flatten() {
            if child.start_time < root_started || !seen.insert(child.pid) {
                continue;
            }
            if is_ssh_program(&child.name) {
                found.push(child); // do not descend into its helpers
            } else {
                queue.push_back(child.pid);
            }
        }
    }
    found.into_iter().max_by_key(|p| (p.start_time, p.pid))
}

/// Command line of the `ssh` running in local tab `tab_id`, if any (#501).
/// `None` when the tab is not a running local terminal or no `ssh` runs in
/// it. An `ssh` inside WSL runs in the Linux VM and is not visible here.
pub(crate) fn ssh_command_in_tab(tab_id: &str) -> Option<Vec<String>> {
    let root = *SHELL_PIDS.lock().unwrap().get(tab_id)?;
    ssh_command_below(root)
}

/// Command line of the user's `ssh` below process `root` (see [`find_user_ssh`]).
fn ssh_command_below(root: u32) -> Option<Vec<String>> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let processes: Vec<ProcessRecord> = system
        .processes()
        .values()
        .map(|p| ProcessRecord {
            pid: p.pid().as_u32(),
            parent: p.parent().map(|pid| pid.as_u32()),
            name: p.name().to_string_lossy().into_owned(),
            start_time: p.start_time(),
            cmd: p
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
        })
        .collect();
    find_user_ssh(&processes, root)
        .map(|p| p.cmd.clone())
        .filter(|cmd| !cmd.is_empty())
}

async fn run_local(
    session: Session,
    tab_id: String,
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
    // WSL takes its directory as `--cd` (a Linux path); other local shells
    // can be started in a Windows / host folder, e.g. a custom agent's (#427).
    if session.host != "wsl" {
        if let Some(dir) = start_directory(&session.local_working_dir) {
            cmd.cwd(dir);
        }
    }
    cmd.env("TERM", session.effective_term_type());

    let child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("failed to start local terminal: {program}"))?;
    drop(pair.slave);
    let _shell_pid = child
        .process_id()
        .map(|pid| ShellPidGuard::register(&tab_id, pid));

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
    // Detected and user-defined shells (#533, #538) name their executable
    // directly; everything else is picked by `host` below.
    let program = session.local_program.trim();
    if !program.is_empty() {
        return (program.to_string(), session.local_args.clone());
    }
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
        "powershell" | _ => {
            let mut script = POWERSHELL_UTF8_SETUP.to_string();
            // `-NoExit` keeps PowerShell open once the program returns, so an
            // agent tab drops back to a prompt instead of closing (#427).
            if let Some(command) = startup_command(session) {
                script.push_str("; ");
                script.push_str(command);
            }
            (
                "powershell.exe".to_string(),
                vec![
                    "-NoLogo".to_string(),
                    "-NoExit".to_string(),
                    "-Command".to_string(),
                    script,
                ],
            )
        }
        #[cfg(not(windows))]
        _ => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            match startup_command(session) {
                None => (shell, Vec::new()),
                // A login shell loads the user's profile (PATH for node, etc.),
                // runs the program, then is replaced by an interactive shell
                // so the tab stays usable after the program exits (#427).
                Some(command) => {
                    let script = format!("{command}; exec {} -l", sh_quote(&shell));
                    (shell, vec!["-l".to_string(), "-c".to_string(), script])
                }
            }
        }
    }
}

const POWERSHELL_UTF8_SETUP: &str = "$utf8 = New-Object System.Text.UTF8Encoding $false; \
     [Console]::InputEncoding = $utf8; [Console]::OutputEncoding = $utf8; $OutputEncoding = $utf8";

/// Arguments that start PowerShell 7 (`pwsh`) as an interactive tab shell
/// with the same UTF-8 console setup as Windows PowerShell (#533).
pub(crate) fn pwsh_shell_args() -> Vec<String> {
    vec![
        "-NoLogo".to_string(),
        "-NoExit".to_string(),
        "-Command".to_string(),
        POWERSHELL_UTF8_SETUP.to_string(),
    ]
}

/// Split a user-typed argument line (#538) into arguments: whitespace
/// separates, `"…"` or `'…'` group (quotes removed), and backslashes are
/// literal so Windows paths need no escaping.
pub(crate) fn split_args(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_word = true;
            }
            None if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            None => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

/// Host folder for a local shell to start in: blank means the default, and a
/// leading `~` is the user's home folder.
fn start_directory(dir: &str) -> Option<std::path::PathBuf> {
    let dir = dir.trim();
    if dir.is_empty() {
        return None;
    }
    let home = || {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from)
    };
    match dir.strip_prefix('~') {
        Some("") => home(),
        Some(rest) if rest.starts_with(['/', '\\']) => {
            home().map(|h| h.join(rest.trim_start_matches(['/', '\\'])))
        }
        _ => Some(std::path::PathBuf::from(dir)),
    }
}

/// Shell command line a generated local session runs after the shell starts
/// (#427). It is already in the hosting shell's syntax: detected agents are
/// stored as a quoted path, custom agents as the line the user typed.
fn startup_command(session: &Session) -> Option<&str> {
    let command = session.local_command.trim();
    (!command.is_empty()).then_some(command)
}

/// Single-quoted PowerShell literal: no variable or subexpression expansion;
/// an embedded `'` is written twice.
#[cfg(any(windows, test))]
pub(crate) fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Single-quoted POSIX shell word; an embedded `'` closes, escapes and reopens.
#[cfg(any(not(windows), test))]
pub(crate) fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod ssh_process_tests {
    use super::{find_user_ssh, ProcessRecord};

    fn proc(pid: u32, parent: u32, name: &str, start: u64) -> ProcessRecord {
        ProcessRecord {
            pid,
            parent: Some(parent),
            name: name.into(),
            start_time: start,
            cmd: vec![name.into(), format!("host{pid}")],
        }
    }

    #[test]
    fn finds_ssh_below_the_shell_and_skips_jump_helpers() {
        let processes = vec![
            proc(10, 1, "powershell.exe", 100),
            proc(20, 10, "ssh.exe", 110),
            // ProxyJump helper spawned by the user's ssh.
            proc(21, 20, "ssh.exe", 111),
            // Someone else's ssh.
            proc(30, 1, "ssh.exe", 120),
        ];
        assert_eq!(find_user_ssh(&processes, 10).unwrap().pid, 20);
    }

    #[test]
    fn looks_through_intermediate_processes_and_prefers_the_newest() {
        let processes = vec![
            proc(10, 1, "bash", 100),
            proc(11, 10, "tmux", 101),
            proc(12, 11, "ssh", 102),
            proc(13, 10, "SSH", 105),
        ];
        assert_eq!(find_user_ssh(&processes, 10).unwrap().pid, 13);
    }

    #[test]
    fn none_without_ssh_or_for_reused_pids() {
        let processes = vec![proc(10, 1, "bash", 100), proc(11, 10, "vim", 101)];
        assert!(find_user_ssh(&processes, 10).is_none());
        // Started before the shell: its parent pid was reused by the shell.
        let processes = vec![proc(10, 1, "bash", 100), proc(11, 10, "ssh", 50)];
        assert!(find_user_ssh(&processes, 10).is_none());
        // Self-parented entries (pid 0 on Windows) do not loop.
        let processes = vec![proc(0, 0, "System Idle Process", 0)];
        assert!(find_user_ssh(&processes, 0).is_none());
    }

    /// Real process tree: a shell running an OpenSSH client that hangs
    /// connecting to a TEST-NET address. Needs `ssh` on PATH, so it is
    /// ignored by default: `cargo test -- --ignored real_ssh_below_a_shell`.
    #[test]
    #[ignore]
    fn real_ssh_below_a_shell_is_found() {
        let ssh = "ssh -o ConnectTimeout=30 -o BatchMode=yes -p 2222 probe@192.0.2.1";
        #[cfg(windows)]
        let mut shell = std::process::Command::new("cmd.exe")
            .args(["/C", ssh])
            .spawn()
            .unwrap();
        // `; true` stops sh from exec-ing ssh in place of itself.
        #[cfg(not(windows))]
        let mut shell = std::process::Command::new("sh")
            .args(["-c", &format!("{ssh}; true")])
            .spawn()
            .unwrap();
        let root = shell.id();
        let mut found = None;
        for _ in 0..50 {
            found = super::ssh_command_below(root);
            if found.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        #[cfg(windows)]
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &root.to_string()])
            .output();
        #[cfg(not(windows))]
        let _ = std::process::Command::new("pkill")
            .args(["-P", &root.to_string()])
            .output();
        let _ = shell.kill();
        let argv = found.expect("ssh below the shell");
        let invocation = crate::ssh::ssh_cli::parse_command_line(&argv).unwrap();
        assert_eq!(invocation.host, "192.0.2.1");
        assert_eq!(invocation.user.as_deref(), Some("probe"));
        assert_eq!(invocation.port, Some(2222));
    }
}

#[cfg(test)]
mod quote_tests {
    use super::{powershell_quote, sh_quote};

    #[test]
    fn powershell_quote_doubles_single_quotes() {
        assert_eq!(
            powershell_quote(r"C:\Users\a\claude.exe"),
            r"'C:\Users\a\claude.exe'"
        );
        assert_eq!(
            powershell_quote("C:\\O'Neil\\x.cmd"),
            "'C:\\O''Neil\\x.cmd'"
        );
        assert_eq!(powershell_quote("$env:X"), "'$env:X'");
    }

    #[test]
    fn sh_quote_escapes_single_quotes() {
        assert_eq!(sh_quote("/usr/local/bin/codex"), "'/usr/local/bin/codex'");
        assert_eq!(sh_quote("/home/o'neil/bin/x"), r"'/home/o'\''neil/bin/x'");
    }
}

#[cfg(test)]
mod start_directory_tests {
    use super::start_directory;

    #[test]
    fn blank_means_default_and_tilde_is_home() {
        assert_eq!(start_directory("  "), None);
        assert_eq!(
            start_directory(r"D:\work\proj"),
            Some(std::path::PathBuf::from(r"D:\work\proj"))
        );
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from);
        assert_eq!(start_directory("~"), home);
        assert_eq!(start_directory("~/proj"), home.map(|h| h.join("proj")));
        // `~name` is not the home folder.
        assert_eq!(
            start_directory("~name"),
            Some(std::path::PathBuf::from("~name"))
        );
    }
}

#[cfg(test)]
mod custom_shell_tests {
    use super::{local_program, split_args};
    use crate::config::Session;

    #[test]
    fn argument_lines_split_on_spaces_and_keep_quoted_groups() {
        assert!(split_args("   ").is_empty());
        assert_eq!(split_args("-l -i"), ["-l", "-i"]);
        assert_eq!(
            split_args(r#"--config "C:\My Files\config.nu" -e 'ls | first'"#),
            ["--config", r"C:\My Files\config.nu", "-e", "ls | first"]
        );
        // Backslashes are literal (Windows paths), empty quotes are an argument.
        assert_eq!(split_args(r#"C:\bin\x.exe """#), [r"C:\bin\x.exe", ""]);
    }

    #[test]
    fn a_session_with_a_program_starts_it_directly() {
        let mut session = Session::new_empty();
        // `host` is only a label for these sessions and must not pick the shell.
        session.host = "nu".to_string();
        session.local_program = r"C:\Program Files\nu\bin\nu.exe".to_string();
        session.local_args = vec!["-l".to_string()];
        let (program, args) = local_program(&session);
        assert_eq!(program, r"C:\Program Files\nu\bin\nu.exe");
        assert_eq!(args, ["-l"]);
    }
}

#[cfg(all(test, not(windows)))]
mod unix_tests {
    use super::local_program;
    use crate::config::Session;

    #[test]
    fn agent_command_runs_in_login_shell_then_keeps_shell() {
        let mut session = Session::new_empty();
        session.host = "shell".to_string();
        session.local_command = "/opt/bin/claude".to_string();
        let (shell, args) = local_program(&session);
        assert_eq!(args[0], "-l");
        assert_eq!(args[1], "-c");
        assert!(args[2].starts_with("/opt/bin/claude; exec "));
        assert!(args[2].ends_with(" -l"));
        assert!(args[2].contains(&shell));

        session.local_command.clear();
        assert!(local_program(&session).1.is_empty());
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
    fn powershell_runs_agent_command_after_utf8_setup() {
        let mut session = Session::new_empty();
        session.host = "powershell".to_string();
        session.local_command = r"& 'C:\Users\me\AppData\Roaming\npm\codex.cmd'".to_string();
        let (program, args) = local_program(&session);
        assert_eq!(program, "powershell.exe");
        assert!(args.iter().any(|arg| arg == "-NoExit"));
        let script = args.last().unwrap();
        assert!(script.contains("OutputEncoding"));
        assert!(script.ends_with(r"; & 'C:\Users\me\AppData\Roaming\npm\codex.cmd'"));
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
