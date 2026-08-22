// Single-instance coordination. All OS entry points (Windows jump list,
// macOS dock menu, Linux desktop action) launch `meatshell --new-window`.
// The first running instance owns a local endpoint under the data dir; later
// `--new-window` launches connect, send "new-window\n" and exit, and the
// primary opens the new window in-process (Chrome-style). Plain relaunches
// never forward: if the endpoint is taken they run as an independent second
// instance.
//
// Transport split: unix uses a unix-domain socket (`ipc.sock`); Windows uses
// a TCP loopback listener on 127.0.0.1 whose port is published in a port
// file (`ipc.port`), because std's Windows unix-socket support is unstable
// (nightly-only, rust-lang/rust#150487). On Windows every `socket_path`
// argument is therefore reinterpreted as the port-file path.

#[cfg(windows)]
use std::net::{SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MSG_NEW_WINDOW: &str = "new-window";

#[derive(Debug)]
pub enum Instance {
    /// This process owns the endpoint. `listen` accepts forwarded requests.
    Primary { listen: Listener },
    /// Another instance is running; the new-window request was forwarded and
    /// this process should exit with success.
    Forwarded,
}

/// Try to become the primary instance. With `forward`, a live primary
/// receives a new-window request and `Forwarded` is returned; without it a
/// live primary is reported as an error so a plain relaunch runs as its own
/// instance instead of opening a bonus window in the first one. Never
/// panics on IO trouble — callers treat errors as "just run normally".
#[cfg(unix)]
pub fn acquire(socket_path: &Path, forward: bool) -> std::io::Result<Instance> {
    if let Ok(listener) = UnixListener::bind(socket_path) {
        return Ok(Instance::Primary {
            listen: Listener { listener },
        });
    }
    // Bind failed: either a live primary owns the socket, or the file is
    // stale (the previous primary exited without unlinking it). Probe with
    // a connect, mirroring the stale port-file logic on Windows.
    match UnixStream::connect(socket_path) {
        Ok(mut stream) => {
            if !forward {
                // A plain launch must not forward: report the live primary
                // and let run() fall back to a normal second instance.
                return Err(std::io::Error::other(
                    "single-instance primary already running",
                ));
            }
            stream.write_all(format!("{MSG_NEW_WINDOW}\n").as_bytes())?;
            stream.flush()?;
            Ok(Instance::Forwarded)
        }
        Err(_) => {
            // Nothing is listening, so the file is stale. Remove it and
            // retry the bind once. Shutdown-time unlinking is deliberately
            // not attempted: it cannot run for a killed process, so this
            // retry is the robust recovery.
            let _ = std::fs::remove_file(socket_path);
            let listener = UnixListener::bind(socket_path)?;
            Ok(Instance::Primary {
                listen: Listener { listener },
            })
        }
    }
}

/// `socket_path` is the port-file path here (see module docs).
#[cfg(windows)]
pub fn acquire(socket_path: &Path, forward: bool) -> std::io::Result<Instance> {
    let port_file = socket_path;
    // A live primary? Connect with a short timeout; a stale port file
    // (parse error, refused/timeout connection) is treated as "no primary".
    if let Some(port) = read_port_file(port_file) {
        if forward {
            if forward_tcp(port).is_ok() {
                return Ok(Instance::Forwarded);
            }
        } else if connect_tcp(port).is_ok() {
            // A plain launch must not forward (and must not steal the port
            // file): report the live primary and let run() fall back to a
            // normal second instance.
            return Err(std::io::Error::other(
                "single-instance primary already running",
            ));
        }
    }
    // No live primary: start one on an ephemeral loopback port.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    // Without the port file nobody can find us; let the caller fall back
    // to a normal launch.
    std::fs::write(port_file, port.to_string())?;
    // Two processes can both see "no primary" and both reach this point.
    // Whoever wrote the port file last wins; re-check and defer if we lost.
    if read_port_file(port_file) != Some(port) {
        drop(listener);
        if forward {
            if let Some(winner) = read_port_file(port_file) {
                if forward_tcp(winner).is_ok() {
                    return Ok(Instance::Forwarded);
                }
            }
        }
        // Degradation: the race-loser could not reach the winner, so a
        // `--new-window` launch falls back to a second instance; rare and
        // acceptable.
        return Err(std::io::Error::other("lost single-instance race"));
    }
    Ok(Instance::Primary {
        listen: Listener { listener },
    })
}

#[cfg(windows)]
fn connect_tcp(port: u16) -> std::io::Result<TcpStream> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300))
}

#[cfg(windows)]
fn forward_tcp(port: u16) -> std::io::Result<Instance> {
    let mut stream = connect_tcp(port)?;
    stream.write_all(format!("{MSG_NEW_WINDOW}\n").as_bytes())?;
    stream.flush()?;
    // Whoever holds the port must ack: a stale port file can point at an
    // unrelated listener, and treating that as Forwarded would exit without
    // any window ever opening.
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    match BufReader::new(&stream).lines().next() {
        Some(Ok(line)) if line == "ack" => Ok(Instance::Forwarded),
        _ => Err(std::io::Error::other("no ack from single-instance primary")),
    }
}

#[cfg(windows)]
fn read_port_file(port_file: &Path) -> Option<u16> {
    std::fs::read_to_string(port_file).ok()?.trim().parse().ok()
}

/// Endpoint path inside the per-user data dir: the unix socket on unix, the
/// TCP port file on Windows (see module docs).
pub fn socket_path() -> PathBuf {
    #[cfg(windows)]
    {
        crate::config::data_dir().join("ipc.port")
    }
    #[cfg(not(windows))]
    {
        crate::config::data_dir().join("ipc.sock")
    }
}

#[derive(Debug)]
pub struct Listener {
    #[cfg(unix)]
    listener: UnixListener,
    #[cfg(windows)]
    listener: TcpListener,
}

impl Listener {
    /// Blocks forever, invoking `on_msg` for every complete line received.
    /// Spawn this on its own thread. Every accepted line is acked before
    /// dispatch so forwarders can verify they reached the real primary —
    /// on Windows the port file may stale and point at an unrelated
    /// listener, which would never ack.
    pub fn spawn<F: FnMut(String) + Send + 'static>(self, mut on_msg: F) {
        for mut stream in self.listener.incoming().flatten() {
            // One silent client must not wedge every later forward: bound
            // the wait for the first line and skip the connection if it
            // times out.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            if let Some(Ok(line)) = BufReader::new(&stream).lines().next() {
                let _ = stream.write_all(b"ack\n");
                let _ = stream.flush();
                on_msg(line);
            }
        }
    }
}

#[cfg(test)]
#[path = "../../tests/app/window_management/single_instance.rs"]
mod single_instance_tests;
