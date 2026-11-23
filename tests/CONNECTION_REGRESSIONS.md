# Session editor and SSH connection regressions

These tests use generated credentials and loopback servers only. Do not substitute
real profiles or keys. They do not prove compatibility with every remote server.

## Rust and lightweight UI checks

```sh
cargo test --locked --features headless
cargo check --locked --bin meatshell
```

The first command includes the production SSH/config unit tests, the editor-test
ownership model, and lightweight Slint fixtures. `ui_auth_prompts` compiles the
production prompt queues against a minimal window and inert persistence, avoiding
the memory cost of the full application. `ui_session_editor` renders the real
session dialog and verifies Save while a test is pending, failed, timed out or
successful, plus close/cancel and connection-field change notifications.

The separate default-feature `cargo check` is required: headless builds exclude
the desktop controller and do not validate its Rust/Slint integration. Desktop
end-to-end/manual testing is still useful for platform-specific focus and fonts.

## Loopback SSH/SFTP checks

Install the test dependency `paramiko` (and its `cryptography` dependency), then:

```sh
cargo build --locked --features headless
python tests/ssh_jump_chain_e2e.py --exe target/debug/meatshell
python tests/ssh_jump_chain_e2e.py --exe target/debug/meatshell --stage-timeouts
python tests/config_import_e2e.py --exe target/debug/meatshell
python tests/sftp_folder_e2e.py --exe target/debug/meatshell
```

The authentication matrix includes unencrypted RSA PEM and OpenSSH keys, inline
and file keys on either jump hop, encrypted OpenSSH keys and incorrect passphrases,
three distinct keys, mixed password/keyboard-interactive auth, RSA-SHA256-only
servers, commands and SFTP. It also rejects missing/cyclic routes, denied
forwarding, and untrusted host keys without falling back to a direct connection.

The slow suite deliberately stalls public-key authentication, forwarding-open,
and the target SSH banner. Each should report its hop/stage after about 15 seconds,
before the 30-second operation limit. To reproduce the missing stage deadlines on
a pre-fix binary, use `--expect-unbounded-stages` instead of `--stage-timeouts`.

Network-stage deadlines do not limit human credential/MFA entry or host-key
confirmation. The handshake budget pauses for host-key decisions, and cancelling
a test closes its own pending transport and prompts. Disconnect cleanup is
best-effort and cannot delay the completed test indefinitely.

`sftp_folder_e2e.py` serves a real local directory over a paramiko loopback
SFTP server and exercises single-file upload/download round-trips through the
pipelined transfer implementation, including nested remote paths and
byte-identical round-trips. Whole-folder batches are a GUI-only path: CLI/MCP
uploads reject directories and the automation adapter returns on the first
terminal transfer event, so neither can drive a full batch. Batch enumeration,
queue registration, failure isolation, cancellation and the outcome tally are
covered by Rust unit tests in `src/sftp/impls/sftp.rs` (`transfer_batch_tests`).

## Directory following after sudo

```sh
cargo test --locked --features headless --bin meatshell ssh::cwd::tests
```

OSC 7 remains authoritative. If `sudo` discards the login shell's hook, the SSH
worker also accepts the Bash window-title convention `root@host: /absolute/path`
(with or without the separator space) through complete OSC 0/2 notifications on
the original host. Ubuntu's standard Bash PS1 uses `\e]0;\u@\h: \w\a` for this.
This is a shell title convention, not a general meaning of OSC 0/2: a remote
program can deliberately set such a title just as it can send OSC 7. Arbitrary
visible prompt text is never used to infer a directory.

The reader waits for BEL or ST across packet boundaries. Plain command output,
readline bracketed-paste settings, and `#` characters inside a path cannot
prematurely complete a notification. Tests cover both title opcodes, every UTF-8
split boundary, OSC 7 precedence across packets, literal title paths, unrelated
control strings, interrupted/oversized frames, and shell transitions. No second
terminal screen is maintained, no commands are sent to the PTY, and SFTP
privileges do not change.

Home-relative (`~`), abbreviated, and custom titles without a complete path still
need OSC 7 shell integration; the client must not guess root's HOME. A root shell
that sends neither OSC 7 nor a matching title cannot be followed by this fallback,
even if its visible prompt contains a path. Validate the server's PS1, `sudo -s`,
successive `cd` commands (including names containing `# `), and `exit` when
testing a desktop build. Synthetic traces do not test sudo policy or access to
directories the SFTP login user cannot read.

## Editor invariants

- Only a newly added, never-selected jump row may be omitted from Save/Test
- Saved empty/dangling hops, broken legacy routes and cycles remain errors
- Save validates configuration and never requires a successful connection test
- Save, Cancel, reopening, another Test, route edits and connection-field edits
  invalidate earlier results and queued test prompts
- Cancelling a test cannot cancel or clear another terminal/window's login input
- A test-owned authentication overlay offers Save/Create without accepting a
  host key, supplying credentials or completing MFA
