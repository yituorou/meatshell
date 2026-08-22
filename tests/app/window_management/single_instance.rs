use crate::app::single_instance::{acquire, Instance};
use std::sync::mpsc;

/// Per-pid dir, per-test endpoint filename. Cargo runs tests in the same
/// process (separate threads), so a shared endpoint would let one test's
/// primary answer another test's acquire; the distinct names keep each test
/// deterministic. The pre-remove also guards against a stale file from an
/// earlier run under a reused pid. This is a unix-socket path on unix and a
/// TCP port-file path on Windows (see `single_instance` module docs).
fn temp_socket_path(test_name: &str) -> std::path::PathBuf {
    let n = std::process::id();
    let dir = std::env::temp_dir().join(format!("meatshell-si-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("ipc-{test_name}.sock"));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn first_acquire_becomes_primary_and_second_forwards() {
    let path = temp_socket_path("first_acquire_becomes_primary_and_second_forwards");
    let (tx, rx) = mpsc::channel();

    // Plain launch: becomes the primary.
    let instance = acquire(&path, false).expect("primary acquire");
    let Instance::Primary { listen } = instance else {
        panic!("first acquire must be primary");
    };
    std::thread::spawn(move || {
        listen.spawn(move |msg| {
            let _ = tx.send(msg);
        });
    });

    // Give the accept loop a moment to start.
    std::thread::sleep(std::time::Duration::from_millis(100));

    // `--new-window` launch: forwards to the primary.
    match acquire(&path, true).expect("forward acquire") {
        Instance::Forwarded => {}
        Instance::Primary { .. } => panic!("second acquire must forward"),
    }

    let msg = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("primary should receive the message");
    assert_eq!(msg, "new-window");
}

#[test]
fn forwarding_without_primary_fails() {
    let path = temp_socket_path("forwarding_without_primary_fails");
    // No primary bound: acquire must become primary, not Forwarded.
    match acquire(&path, true).expect("acquire") {
        Instance::Primary { .. } => {}
        Instance::Forwarded => panic!("no primary to forward to"),
    }
}

/// A previous primary that exited without unlinking leaves a file behind.
/// `UnixListener::bind` fails with EADDRINUSE for any existing file, so
/// acquire must detect staleness via the failed connect probe, remove the
/// file and take over the endpoint instead of erroring out.
#[test]
fn stale_socket_file_is_recovered() {
    let path = temp_socket_path("stale_socket_file_is_recovered");
    // A file with no listener behind it: stale endpoint.
    std::fs::write(&path, b"stale").unwrap();
    match acquire(&path, true).expect("acquire over a stale socket file") {
        Instance::Primary { .. } => {}
        Instance::Forwarded => panic!("nothing is listening behind a stale file"),
    }
}

/// A plain second launch must not forward "new-window" to the primary
/// (run() only honors `Forwarded` for `--new-window`, so forwarding would
/// spawn a bonus window in the first instance while also starting a second
/// one). acquire(forward=false) must therefore report the live primary as
/// an error without sending anything.
#[test]
fn plain_acquire_does_not_forward_to_primary() {
    let path = temp_socket_path("plain_acquire_does_not_forward_to_primary");
    let (tx, rx) = mpsc::channel();

    let instance = acquire(&path, false).expect("primary acquire");
    let Instance::Primary { listen } = instance else {
        panic!("first acquire must be primary");
    };
    std::thread::spawn(move || {
        listen.spawn(move |msg| {
            let _ = tx.send(msg);
        });
    });

    // Give the accept loop a moment to start.
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Bind fails, the probe finds a live primary, no message is sent.
    acquire(&path, false).expect_err("plain acquire must not forward");

    // The primary must not have received anything from the probe.
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "plain launch must not deliver a message to the primary"
    );
}
