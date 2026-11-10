//! Extra local shells next to the built-in PowerShell / CMD / WSL entries.
//!
//! * Detected (#533, #538): PowerShell 7 (`pwsh`), Nushell (`nu`) and, on
//!   Windows, Git Bash are listed automatically when installed.
//! * User-defined (#538): Settings → Local shells takes a name, a program,
//!   optional arguments and an optional start folder, for any other shell
//!   (MSYS2 zsh, fish, Cygwin bash, …).
//!
//! Both appear in the "system" group of Quick connect and in the tab bar's
//! launch menu. The program is started directly as the tab's shell; unlike a
//! custom agent nothing is wrapped in PowerShell.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::config::{ConfigStore, CustomShell, Session};

use slint::ComponentHandle as _;

use super::agents::{find_program, search_dirs};
use super::session_models::builtin_local_session;

/// Detected shells: `system:<key>`.
const ID_PREFIX: &str = "system:";
/// User-defined shells: `system:custom:<config id>`.
const CUSTOM_ID_PREFIX: &str = "system:custom:";

#[cfg(windows)]
const EXTENSIONS: &[&str] = &[".exe"];
#[cfg(not(windows))]
const EXTENSIONS: &[&str] = &[""];

/// Result of the first detection pass, shared by every window. Filled off the
/// UI thread together with the agent CLIs (see `agents::wire_agent_launchers`).
static DETECTED: OnceLock<Vec<DetectedShell>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DetectedShell {
    pub key: &'static str,
    pub name: &'static str,
    pub path: PathBuf,
}

/// Detect once (blocking; call off the UI thread).
pub(super) fn detect_once() -> &'static [DetectedShell] {
    DETECTED.get_or_init(detect_shells)
}

/// Every known extra shell installed here, in menu order.
pub(super) fn detect_shells() -> Vec<DetectedShell> {
    let path_dirs = search_dirs();
    let mut out = Vec::new();
    if let Some(path) = find_pwsh(&path_dirs) {
        out.push(DetectedShell {
            key: "pwsh",
            name: "PowerShell 7",
            path,
        });
    }
    #[cfg(windows)]
    if let Some(path) = find_git_bash(&path_dirs) {
        out.push(DetectedShell {
            key: "gitbash",
            name: "Git Bash",
            path,
        });
    }
    if let Some(path) = find_nu(&path_dirs) {
        out.push(DetectedShell {
            key: "nu",
            name: "Nushell",
            path,
        });
    }
    // On Unix the login shell is already listed as the built-in entry; don't
    // list it a second time if it happens to be pwsh or nu.
    #[cfg(not(windows))]
    if let Some(login) = std::env::var_os("SHELL").map(PathBuf::from) {
        let same = |p: &Path| {
            std::fs::canonicalize(p).ok() == std::fs::canonicalize(&login).ok()
        };
        out.retain(|shell| !same(&shell.path));
    }
    out
}

fn env_dir(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Option<PathBuf> {
    env_dir("USERPROFILE").or_else(|| env_dir("HOME"))
}

/// PowerShell 7: PATH first (the installer adds it), then the default
/// install folders, so a pwsh installed after meatshell started is found too.
fn find_pwsh(path_dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut dirs = path_dirs.to_vec();
    #[cfg(windows)]
    {
        for base in ["ProgramFiles", "ProgramW6432"] {
            if let Some(pf) = env_dir(base) {
                dirs.push(pf.join("PowerShell").join("7"));
                dirs.push(pf.join("PowerShell").join("7-preview"));
            }
        }
        if let Some(home) = home_dir() {
            dirs.push(home.join(".dotnet").join("tools"));
        }
    }
    #[cfg(not(windows))]
    {
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/opt/microsoft/powershell/7"));
    }
    find_program("pwsh", &dirs, EXTENSIONS)
}

/// Nushell: PATH (winget / scoop / cargo put it there), then the MSI's and
/// cargo's default folders.
fn find_nu(path_dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut dirs = path_dirs.to_vec();
    #[cfg(windows)]
    {
        if let Some(pf) = env_dir("ProgramFiles") {
            dirs.push(pf.join("nu").join("bin"));
        }
        if let Some(local) = env_dir("LOCALAPPDATA") {
            dirs.push(local.join("Programs").join("nu").join("bin"));
        }
    }
    if let Some(home) = home_dir() {
        dirs.push(home.join(".cargo").join("bin"));
    }
    #[cfg(not(windows))]
    {
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
    }
    find_program("nu", &dirs, EXTENSIONS)
}

/// Git for Windows' bash. Deliberately NOT a PATH lookup of `bash`, which on
/// most machines is `System32\bash.exe` (the WSL launcher). Instead: the
/// Git install that owns `git.exe` on PATH, then the default install folders.
#[cfg(windows)]
fn find_git_bash(path_dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(git) = find_program("git", path_dirs, EXTENSIONS) {
        // <root>\cmd\git.exe, <root>\bin\git.exe or <root>\mingw64\bin\git.exe
        let mut dir = git.parent().map(Path::to_path_buf);
        for _ in 0..3 {
            let Some(d) = dir else { break };
            roots.push(d.clone());
            dir = d.parent().map(Path::to_path_buf);
        }
    }
    for base in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Some(pf) = env_dir(base) {
            roots.push(pf.join("Git"));
        }
    }
    if let Some(local) = env_dir("LOCALAPPDATA") {
        roots.push(local.join("Programs").join("Git"));
    }
    git_bash_in(&roots)
}

/// `<root>\bin\bash.exe` for the first root that has it.
#[cfg(any(windows, test))]
fn git_bash_in(roots: &[PathBuf]) -> Option<PathBuf> {
    roots
        .iter()
        .map(|root| root.join("bin").join("bash.exe"))
        .find(|p| p.is_file())
}

/// Arguments each detected shell starts with.
fn detected_args(key: &str) -> Vec<String> {
    match key {
        "pwsh" => crate::terminal::local::pwsh_shell_args(),
        // A login shell reads ~/.bash_profile like Git's own "Git Bash" link.
        "gitbash" => vec!["--login".to_string(), "-i".to_string()],
        _ => Vec::new(),
    }
}

fn program_stem(program: &str) -> String {
    Path::new(program)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string())
}

fn shell_session(id: &str, name: &str, program: String, args: Vec<String>) -> Session {
    // `host` is only the Quick connect subtitle for these rows; the program
    // itself comes from `local_program`.
    let mut session = builtin_local_session(id, name, &program_stem(&program));
    session.local_program = program;
    session.local_args = args;
    session
}

fn detected_session(shell: &DetectedShell) -> Session {
    shell_session(
        &format!("{ID_PREFIX}{}", shell.key),
        shell.name,
        shell.path.to_string_lossy().into_owned(),
        detected_args(shell.key),
    )
}

fn custom_session(shell: &CustomShell) -> Session {
    let mut session = shell_session(
        &format!("{CUSTOM_ID_PREFIX}{}", shell.id),
        &shell.name,
        shell.program.clone(),
        crate::terminal::local::split_args(&shell.args),
    );
    session.local_working_dir = shell.directory.clone();
    session
}

/// Detected (once detection has finished) then user-defined shells.
pub(super) fn shell_sessions(store: &ConfigStore) -> Vec<Session> {
    DETECTED
        .get()
        .into_iter()
        .flatten()
        .map(detected_session)
        .chain(store.custom_shells().iter().map(custom_session))
        .collect()
}

/// Names of the detected shells for the Settings page ("" before detection).
fn detected_summary() -> String {
    DETECTED
        .get()
        .map(|shells| {
            shells
                .iter()
                .map(|s| s.name)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// Push the Settings → Local shells list into one window.
pub(super) fn refresh_settings(w: &super::AppWindow, store: &ConfigStore) {
    let rows: Vec<super::CustomShellInfo> = store
        .custom_shells()
        .iter()
        .map(|shell| super::CustomShellInfo {
            id: shell.id.clone().into(),
            name: shell.name.clone().into(),
            program: shell.program.clone().into(),
            args: shell.args.clone().into(),
            directory: shell.directory.clone().into(),
        })
        .collect();
    w.set_custom_shells(slint::ModelRc::new(slint::VecModel::from(rows)));
    w.set_detected_shells(detected_summary().into());
}

/// Settings → Local shells callbacks of one window. Lists are refreshed by
/// the config broadcast → `refresh-agent-launchers`, like custom agents.
pub(super) fn wire_custom_shells(ctx: &super::WinCtx) {
    let window = &ctx.window;
    {
        let weak = window.as_weak();
        let store = ctx.store.clone();
        let registry = ctx.registry.clone();
        window.on_add_custom_shell(move |name, program, args, directory| {
            let added = {
                let mut s = store.borrow_mut();
                let added = s.add_custom_shell(&name, &program, &args, &directory);
                if added {
                    let _ = s.save();
                }
                added
            };
            if added {
                if let Some(w) = weak.upgrade() {
                    w.set_shell_new_name("".into());
                    w.set_shell_new_program("".into());
                    w.set_shell_new_args("".into());
                    w.set_shell_new_directory("".into());
                }
                registry.broadcast_config_changed();
            }
        });
    }
    {
        let store = ctx.store.clone();
        let registry = ctx.registry.clone();
        window.on_remove_custom_shell(move |id| {
            {
                let mut s = store.borrow_mut();
                s.remove_custom_shell(&id);
                let _ = s.save();
            }
            registry.broadcast_config_changed();
        });
    }
    {
        let weak = window.as_weak();
        window.on_pick_shell_program(move || {
            let mut dialog = super::DialogOwner::of_weak(&weak).file();
            if cfg!(windows) {
                dialog = dialog.add_filter("Programs", &["exe", "cmd", "bat", "com"]);
            }
            if let Some(file) = dialog.pick_file() {
                if let Some(w) = weak.upgrade() {
                    let path = file.to_string_lossy().to_string();
                    if w.get_shell_new_name().trim().is_empty() {
                        w.set_shell_new_name(program_stem(&path).into());
                    }
                    w.set_shell_new_program(path.into());
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        window.on_pick_shell_directory(move || {
            if let Some(folder) = super::DialogOwner::of_weak(&weak).file().pick_folder() {
                if let Some(w) = weak.upgrade() {
                    w.set_shell_new_directory(folder.to_string_lossy().to_string().into());
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_shell_starts_its_program_directly_with_split_args() {
        let shell = CustomShell {
            id: "abc".into(),
            name: "MSYS2 zsh".into(),
            program: r"C:\msys64\usr\bin\zsh.exe".into(),
            args: r#"-l -i --rcs "C:\My Files\zshrc""#.into(),
            directory: r"D:\Projects".into(),
        };
        let s = custom_session(&shell);
        assert_eq!(s.id, "system:custom:abc");
        assert_eq!(s.group, "system");
        assert_eq!(s.host, "zsh");
        assert_eq!(s.local_program, r"C:\msys64\usr\bin\zsh.exe");
        assert_eq!(s.local_args, ["-l", "-i", "--rcs", r"C:\My Files\zshrc"]);
        assert_eq!(s.local_working_dir, r"D:\Projects");
    }

    #[test]
    fn detected_shells_get_their_start_arguments() {
        let pwsh = detected_session(&DetectedShell {
            key: "pwsh",
            name: "PowerShell 7",
            path: PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe"),
        });
        assert_eq!(pwsh.id, "system:pwsh");
        assert_eq!(pwsh.local_args[..3], ["-NoLogo", "-NoExit", "-Command"]);
        assert!(pwsh.local_args[3].contains("UTF8Encoding"));
        let nu = detected_session(&DetectedShell {
            key: "nu",
            name: "Nushell",
            path: PathBuf::from("/usr/bin/nu"),
        });
        assert!(nu.local_args.is_empty());
        assert_eq!(detected_args("gitbash"), ["--login", "-i"]);
    }

    #[test]
    fn git_bash_is_found_under_a_git_root_not_via_path_bash() {
        let tmp = std::env::temp_dir().join(format!("ms-gitbash-{}", std::process::id()));
        let root = tmp.join("Git");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin").join("bash.exe"), b"").unwrap();
        let found = git_bash_in(&[tmp.join("Nope"), root.join("cmd"), root.clone()]);
        assert_eq!(found, Some(root.join("bin").join("bash.exe")));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
