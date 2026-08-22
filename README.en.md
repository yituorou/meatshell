# meatshell

[简体中文](./README.md) | **English**

A lightweight, low-memory SSH / terminal client inspired by FinalShell, but
written entirely in **Rust + [Slint](https://slint.dev)**. The goal is to keep
FinalShell's core experience (resource-monitor sidebar, session management,
tabbed terminals) while cutting memory use from the 400 MB+ of a JVM app down to
the tens-of-MB range of a native binary.

## Screenshots

<p align="center">
  <img src="docs/screenshots/01-welcome-en.png" alt="Welcome / session management" width="800"><br>
  <em>Welcome page: session management + local resource monitor sidebar</em>
</p>

<p align="center">
  <img src="docs/screenshots/02-terminal-htop.png" alt="Terminal + SFTP" width="800"><br>
  <em>Tabbed terminal (full-screen btop) + SFTP file browser + remote resource monitoring</em>
</p>

## Download & install

Every `v*` tag triggers a GitHub Actions build that produces native binaries for
**Windows / Linux / macOS**, published on the
[Releases](https://github.com/jeff141/meatshell/releases) page.

### Windows

Download `meatshell-*-windows-x86_64.zip`, unzip, and run `meatshell.exe`.

### Linux

```bash
tar -xzf meatshell-*-linux-x86_64.tar.gz
cd meatshell-*-linux-x86_64
./meatshell                                  # run it directly
# Optional: install the app icon + launcher entry (shows the icon in the dock /
# app list — no argument needed, it finds the binary next to the script)
chmod +x install-linux.sh && ./install-linux.sh
```

> Requires glibc ≥ 2.35 (Ubuntu 22.04+ / Debian 12+). On Wayland you may need to
> log out/in once after installing the icon.

Building from source with `cargo run` on Linux Mint / Ubuntu / Debian requires
the Slint/winit/rfd system development packages:

```bash
sudo apt update
sudo apt install -y --no-install-recommends \
  build-essential pkg-config cmake \
  libfontconfig1-dev libfreetype6-dev \
  libxcb1-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libgl1-mesa-dev libegl1-mesa-dev libgtk-3-dev \
  libudev-dev
```

### macOS

The download is a `.zip` containing the `meatshell.app` bundle:

```bash
# Unzip (aarch64 = Apple Silicon, x86_64 = Intel)
unzip meatshell-*-macos-*.zip
# Move it to Applications (optional — it also runs in place)
mv meatshell.app /Applications/
# Clear the quarantine flag, otherwise macOS says "meatshell is damaged and can't be opened"
xattr -dr com.apple.quarantine /Applications/meatshell.app
# Open it (or double-click in Finder)
open /Applications/meatshell.app
```

> If you didn't move it to `/Applications`, point both paths above at wherever the `.app` actually is (e.g. `~/Downloads/meatshell.app`).

> To build from source, see [Running](#running) below.

## Features

### Done

- [x] FinalShell-style UI with dark / light / follow-system themes
- [x] Local + remote resource monitoring (CPU / memory / swap / network / disk)
- [x] Remote process monitor (CPU-sorted table with PID copy and permission-aware termination)
- [x] Full VT/ANSI terminal emulation (btop / htop / vim render correctly)
- [x] Color emoji, including skin tones, flags, and ZWJ sequences
- [x] Tabs (welcome page + multiple sessions)
- [x] Session management: create / edit / delete / groups, local JSON, export / import (including FinalShell connection files)
  - Config location: `%APPDATA%/meatshell/sessions.json` (Windows)
    / `~/.config/meatshell/sessions.json` (Linux)
    / `~/Library/Application Support/meatshell/sessions.json` (macOS)
- [x] SSH (`russh`, pure Rust): password / private key / encrypted key (passphrase)
- [x] SFTP browser + upload / download (drag-and-drop) + in-terminal ZMODEM (`sz`) receive
- [x] SSH port forwarding / tunnels: local -L / remote -R / dynamic -D (SOCKS5)
- [x] Quick commands + command box (broadcast to all sessions) + command history
- [x] Serial / Telnet sessions
- [x] Outbound proxy (SOCKS5 / HTTP)
- [x] Import `~/.ssh/config`
- [x] Session passwords encrypted at rest (ChaCha20-Poly1305)
- [x] Known-hosts (`known_hosts`) verification + first-connect confirmation
- [x] Split panes for tabbed terminals
- [x] Multiple windows: Ctrl+Shift+N (macOS ⌘⇧N) or the system "New window" entry (Windows taskbar / macOS Dock / Linux desktop right-click), managed as a single Chrome-style process

Color emoji graphics are provided by [Twemoji](https://github.com/jdecked/twemoji)
under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). See
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for the full attribution.

### Planned

- [ ] Store session passwords in the OS keychain

## Tech stack

| Module        | Choice                                                            |
| ------------- | ----------------------------------------------------------------- |
| UI            | [Slint](https://slint.dev) (compiled pure Rust, no GC)            |
| Async runtime | [`tokio`](https://tokio.rs)                                       |
| SSH protocol  | [`russh`](https://crates.io/crates/russh) (no libssh dependency)  |
| System metrics| [`sysinfo`](https://crates.io/crates/sysinfo)                     |
| Serialization | `serde` + `serde_json`                                            |
| Logging       | `tracing` + `tracing-subscriber`                                  |

## Running

```bash
cargo run --release
```

On first launch an empty session store is created at
`%APPDATA%/meatshell/sessions.json`. Click **"＋ New Session"** in the top-right
to add your first server.

## CLI and MCP automation

The MeatShell CLI and MCP server share the sessions and SSH/SFTP implementation
used by the GUI. The CLI is suited to scripts, CI, and explicit commands, while
MCP lets an MCP-capable AI client perform server inspection, log analysis, and
file transfers from natural-language requests. They are two entry points to the
same saved server configuration.

> Before using either interface, create the target session in the GUI and connect
> successfully once to complete host-key confirmation. Passwords, private keys,
> and other secrets are never returned by CLI/MCP. Do not put plaintext passwords
> in prompts or MCP configuration files.

### CLI

Show every available command:

```bash
meatshell cli help
```

Common examples:

```bash
# List saved sessions; the first column is the session-id used below
meatshell cli sessions
meatshell cli sessions --json

# Show non-secret metadata for one session
meatshell cli session <session-id>

# Run a non-interactive SSH command; the remote command must follow --
meatshell cli exec <session-id> -- free -h
meatshell cli exec <session-id> --timeout 60 --json -- journalctl -n 100 --no-pager

# Browse, read, and transfer remote files
meatshell cli files <session-id> /var/log
meatshell cli read <session-id> /var/log/example.log
meatshell cli upload <session-id> ./local.txt /tmp
meatshell cli download <session-id> /tmp/result.txt ./downloads
```

Get `<session-id>` from `meatshell cli sessions`. A download requires an existing
local destination directory and will not overwrite a file with the same name.

### MCP

First open **Settings → Interface → MCP** in MeatShell:

1. Enable MCP.
2. Allow saved credentials when required.
3. Allow arbitrary SSH commands for remote diagnostics.
4. Allow file transfers when uploads or downloads are required.

Then register a stdio MCP server named `meatshell` in your MCP-capable client:

```json
{
  "mcpServers": {
    "meatshell": {
      "command": "/absolute/path/to/meatshell",
      "args": ["mcp", "serve"]
    }
  }
}
```

On Windows, `command` can be `C:\\path\\to\\meatshell.exe`. Restart or refresh
the MCP client; the `meatshell` server should expose tools for session lookup,
remote commands, directory listing, bounded text reads, uploads, and downloads.
MCP configuration locations vary by AI client, so consult that client's docs.

#### MCP JSON-RPC examples

An AI client normally creates these requests automatically; you do not need to
enter them manually. When debugging the stdio connection, send each request as
one complete line of JSON and complete initialization in this order:

```jsonl
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"example-client","version":"1.0.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
```

List saved sessions and obtain a `<session-id>`:

```jsonl
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_sessions","arguments":{}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_session","arguments":{"session_id":"<session-id>"}}}
```

Run read-only OOM diagnostics and browse the heap-dump directory:

```jsonl
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"run_command","arguments":{"session_id":"<session-id>","command":"free -h; printf '\\n=== kernel OOM ===\\n'; dmesg 2>/dev/null | grep -iE 'oom|out of memory|killed process' | tail -50 || true; printf '\\n=== Java ===\\n'; ps -ef | grep '[j]ava'","timeout_seconds":30,"max_output_bytes":1048576}}}
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"list_remote_files","arguments":{"session_id":"<session-id>","path":"/home/jeff/test/heapdumps"}}}
```

Read a log or download a heap dump:

```jsonl
{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"read_remote_text_file","arguments":{"session_id":"<session-id>","path":"/home/jeff/test/logs/meatshell-log-demo-error.log"}}}
{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"download_file","arguments":{"session_id":"<session-id>","remote_path":"/home/jeff/test/heapdumps/example.hprof","local_directory":"/existing/local/directory","timeout_seconds":120}}}
```

`read_remote_text_file` accepts only bounded UTF-8 text. Use `download_file` for
binary files such as HPROF dumps. The local destination directory must already
exist, and the tool will not overwrite a file with the same name.

Once configured, give the AI client a request such as:

> Use the `meatshell` MCP to investigate an OOM on my `192.168.100.41` server.
> Heap dumps are in `/home/jeff/test/heapdumps`. Check system memory, kernel OOM
> records, Java processes, application logs, and the HPROF files, then identify
> the root cause. Perform read-only diagnostics first; do not restart services or
> delete files.

MCP first uses `list_sessions` to find the matching saved session, then invokes
remote-command or SFTP tools within the permissions you granted. If several
sessions use the same host, include the GUI session name in the prompt. A good
diagnostic prompt states the target host, log or dump paths, and whether restarts,
configuration changes, or file downloads are allowed.

## Project layout

```
meatshell/
├── Cargo.toml
├── build.rs                 # Slint compiler entry point
├── ui/
│   ├── app.slint            # top-level window
│   ├── theme.slint          # design tokens
│   ├── widgets.slint        # reusable buttons / inputs / sparkline
│   ├── sidebar.slint        # left-hand system monitor panel
│   ├── tabs.slint           # top tab bar
│   ├── welcome.slint        # welcome page / quick connect
│   ├── session_dialog.slint # new / edit session dialog
│   └── terminal_view.slint  # terminal view (v0.1 line-buffered)
└── src/
    ├── main.rs
    ├── app.rs               # UI ↔ backend bridge
    ├── config.rs            # session JSON persistence
    ├── system.rs            # CPU / memory / network sampling
    └── ssh.rs               # SSH session worker
```

## Development notes

- Slint widgets use a strict layout DSL; after editing a `.slint` file,
  `cargo check` is the fastest feedback loop.
- The application event loop is single-threaded (required by Slint); all
  cross-thread UI updates go through `slint::invoke_from_event_loop` callbacks.
- SSH / SFTP share the `known_hosts` verification path: first contact asks for
  trust and remembers the host key, while later key changes prompt again.

## Release

Do not bump `Cargo.toml` by hand and then create a tag. Use the release helper
so the tag points at a commit that already contains the matching Cargo version:

```powershell
.\scripts\release.ps1 v0.6.0 -Push
```

The script updates `Cargo.toml` / `Cargo.lock`, runs `cargo check --locked`,
verifies `meatshell --version`, commits `Release v0.6.0`, creates an annotated
tag, and pushes the current branch plus the tag. See
[docs/release.md](docs/release.md) for details.

## Related Groups

<p align="center">
  <img src="docs/QR/QQ_Group_QR_Code.jpg" alt="QQ group QR code" width="300"><br>
  <em>Scan the QR code to join QQ groups to exchange user experiences, provide feedback, or get the latest updates</em>
</p>

## License

Dual-licensed under MIT OR Apache-2.0.
