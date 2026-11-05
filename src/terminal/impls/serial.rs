//! Serial-port session worker (issue #14 / #17).
//!
//! Mirrors the public surface of [`crate::ssh::spawn_session`] so the rest of
//! the UI pipeline (terminal output, key input, tab lifecycle) is reused
//! unchanged: it returns a [`SessionHandle`] plus an
//! [`UnboundedReceiver<SessionEvent>`].
//!
//! Unlike SSH there is no remote PTY, no SFTP and no resource monitor — a
//! serial line is just a raw byte pipe to a switch / router / MCU console.
//! The `serialport` crate is blocking, so one dedicated OS thread does all
//! port I/O (see [`pump_serial`] for why reads and writes share it).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serialport::{DataBits, FlowControl, Parity, StopBits};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::config::Session;
use crate::i18n::t;
use crate::ssh::{SessionCommand, SessionEvent, SessionHandle};

/// Spawn a serial-port session. See module docs for why the signature mirrors
/// `spawn_session` (minus the PTY size, which a serial line has no notion of).
pub fn spawn_serial_session(
    runtime: &tokio::runtime::Handle,
    tab_id: String,
    session: Session,
) -> (SessionHandle, UnboundedReceiver<SessionEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
    let (evt_tx, evt_rx) = mpsc::unbounded_channel::<SessionEvent>();

    let evt_for_task = evt_tx.clone();
    let join = runtime.spawn(async move {
        if let Err(err) = run_serial(session, cmd_rx, evt_for_task.clone()).await {
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

fn parse_data_bits(n: u8) -> DataBits {
    match n {
        5 => DataBits::Five,
        6 => DataBits::Six,
        7 => DataBits::Seven,
        _ => DataBits::Eight,
    }
}

fn parse_stop_bits(n: u8) -> StopBits {
    match n {
        2 => StopBits::Two,
        _ => StopBits::One,
    }
}

fn parse_parity(s: &str) -> Parity {
    match s {
        "odd" => Parity::Odd,
        "even" => Parity::Even,
        _ => Parity::None,
    }
}

fn parse_flow(s: &str) -> FlowControl {
    match s {
        "hardware" => FlowControl::Hardware,
        "software" => FlowControl::Software,
        _ => FlowControl::None,
    }
}

async fn run_serial(
    session: Session,
    mut commands: UnboundedReceiver<SessionCommand>,
    events: UnboundedSender<SessionEvent>,
) -> Result<()> {
    let port_name = session.serial_port.trim().to_string();
    if port_name.is_empty() {
        return Err(anyhow::anyhow!(t("串口号为空", "serial port is empty")));
    }

    let _ = events.send(SessionEvent::Status(format!(
        "{} {} @ {}",
        t("打开串口", "Opening serial"),
        port_name,
        session.baud_rate
    )));

    // Open on a blocking thread — serialport::open() can stall on a busy
    // device. The timeout bounds the stall: sans it, Close never reaches the
    // command pump (open is awaited before the pump starts) and the task
    // would wedge runtime shutdown forever.
    let open_name = port_name.clone();
    let baud = session.baud_rate;
    let data_bits = parse_data_bits(session.data_bits);
    let stop_bits = parse_stop_bits(session.stop_bits);
    let parity = parse_parity(&session.parity);
    let flow = parse_flow(&session.flow_control);
    let port = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::task::spawn_blocking(move || {
            serialport::new(&open_name, baud)
                .data_bits(data_bits)
                .stop_bits(stop_bits)
                .parity(parity)
                .flow_control(flow)
                // Short read timeout so the reader thread can poll the stop flag.
                .timeout(Duration::from_millis(50))
                .open()
        }),
    )
    .await
    .context(t("打开串口超时", "timed out opening the serial port"))?
    .context("serial open task panicked")?
    .with_context(|| {
        format!(
            "{} {}",
            t("打开串口失败", "failed to open serial port"),
            port_name
        )
    })?;

    let _ = events.send(SessionEvent::Connected);
    let _ = events.send(SessionEvent::Status(format!(
        "{} {} @ {} {}{}{}",
        t("已连接", "Connected"),
        port_name,
        session.baud_rate,
        session.data_bits,
        parity_letter(&session.parity),
        session.stop_bits,
    )));

    // --- Port I/O thread ----------------------------------------------------
    let running = Arc::new(AtomicBool::new(true));
    let io_running = running.clone();
    let io_events = events.clone();
    let (write_tx, write_rx) = std_mpsc::channel::<Vec<u8>>();
    // Per-session character set (#435): many consoles (switches, PLCs) emit
    // GBK. Stateful, so a multibyte char split across reads survives.
    let mut decoder = crate::terminal::TerminalEncoding::new(&session.encoding);
    let encoder = crate::terminal::TerminalEncoding::new(&session.encoding);
    let io_handle = std::thread::spawn(move || {
        let mut port = port;
        let result = pump_serial(&mut port, &write_rx, &io_running, |bytes| {
            let text = decoder.decode(bytes);
            text.is_empty() || io_events.send(SessionEvent::Output(text)).is_ok()
        });
        if let Err(message) = result {
            let _ = io_events.send(SessionEvent::Closed(message));
        }
    });

    // --- Command pump -------------------------------------------------------
    while let Some(cmd) = commands.recv().await {
        match cmd {
            SessionCommand::RawInput(bytes) => {
                // Never log keystroke bytes — they can be passwords (#15).
                tracing::debug!("serial write len={} bytes", bytes.len());
                // Hand off without waiting: each keystroke used to await its
                // own blocking write, so held keys queued up and kept acting
                // after release (#534). The I/O thread writes them in order.
                if write_tx.send(encoder.encode(&bytes)).is_err() {
                    break; // I/O thread ended (and reported why).
                }
            }
            // A serial line has no window size; nothing to propagate.
            SessionCommand::Resize(_, _) => {}
            SessionCommand::AddTunnel { .. }
            | SessionCommand::StopTunnel(_)
            | SessionCommand::StartTunnel(_)
            | SessionCommand::DeleteTunnel(_)
            | SessionCommand::SetResourceMonitoring(_) => {}
            SessionCommand::KillProcess { reply, .. } => {
                let _ = reply.send(crate::ssh::ProcessKillResult {
                    success: false,
                    message: t(
                        "串口不支持远程进程操作",
                        "Remote process control is unavailable for serial sessions",
                    )
                    .into(),
                });
            }
            SessionCommand::Close => break,
        }
    }

    // Stop the I/O thread and wait for it to finish.
    running.store(false, Ordering::Relaxed);
    drop(write_tx);
    let _ = tokio::task::spawn_blocking(move || io_handle.join()).await;
    let _ = events.send(SessionEvent::Closed(
        t("串口已关闭", "serial port closed").into(),
    ));
    Ok(())
}

/// Single-letter parity tag for the status line (8N1 style).
fn parity_letter(parity: &str) -> &'static str {
    match parity {
        "odd" => "O",
        "even" => "E",
        _ => "N",
    }
}

/// The port operations [`pump_serial`] needs, so the loop can be tested
/// without hardware.
trait SerialIo {
    /// Bytes already received and waiting in the driver's buffer.
    fn available(&mut self) -> std::io::Result<usize>;
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize>;
}

impl SerialIo for Box<dyn serialport::SerialPort> {
    fn available(&mut self) -> std::io::Result<usize> {
        Ok(self.bytes_to_read()? as usize)
    }
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(self, buf)
    }
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(self, buf)
    }
}

/// After any traffic, poll for incoming bytes this often so echo shows up
/// promptly; when the line has been quiet for `ACTIVE_FOR`, back off to
/// `POLL_IDLE` to keep an idle console tab cheap.
const POLL_ACTIVE: Duration = Duration::from_millis(2);
const POLL_IDLE: Duration = Duration::from_millis(20);
const ACTIVE_FOR: Duration = Duration::from_millis(500);

/// All port I/O for one session on one thread (#534).
///
/// serialport opens Windows COM ports for synchronous I/O and `try_clone`
/// only duplicates the handle, so a write on the clone waited behind the
/// reader thread's pending `ReadFile` — up to the full read timeout on an
/// idle line, for every keystroke. Combined with awaiting each write in turn,
/// a held arrow key queued writes faster than they drained and kept moving
/// the cursor after release; Enter felt sluggish for the same reason.
///
/// Here queued writes always go first, and a read is only issued for bytes
/// the driver already holds, so `ReadFile` returns at once and can never
/// hold a keystroke back. Between those the thread sleeps on the write queue,
/// which wakes it immediately for the next keystroke.
///
/// `output` receives each chunk read and returns false to stop. Returns Ok on
/// a normal stop (flag cleared or the queue's sender dropped), or the
/// user-facing reason the port failed.
fn pump_serial<P: SerialIo>(
    port: &mut P,
    writes: &std_mpsc::Receiver<Vec<u8>>,
    running: &AtomicBool,
    mut output: impl FnMut(&[u8]) -> bool,
) -> std::result::Result<(), String> {
    let mut buf = [0u8; 4096];
    let mut last_activity = Instant::now();
    while running.load(Ordering::Relaxed) {
        // 1. Everything the user typed, in order.
        loop {
            match writes.try_recv() {
                Ok(bytes) => {
                    write_fully(port, &bytes, running)?;
                    last_activity = Instant::now();
                }
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }

        // 2. Only bytes that have already arrived.
        let pending = port.available().map_err(|e| read_error(&e))?;
        if pending > 0 {
            let want = pending.min(buf.len());
            match port.read(&mut buf[..want]) {
                Ok(0) => {}
                Ok(n) => {
                    last_activity = Instant::now();
                    if !output(&buf[..n]) {
                        return Ok(());
                    }
                    continue; // more may be waiting; check writes first
                }
                Err(e) if is_retryable(&e) => {}
                Err(e) => return Err(read_error(&e)),
            }
        }

        // 3. Nothing to read: sleep until the next keystroke or poll tick.
        let wait = if last_activity.elapsed() < ACTIVE_FOR {
            POLL_ACTIVE
        } else {
            POLL_IDLE
        };
        match writes.recv_timeout(wait) {
            Ok(bytes) => {
                write_fully(port, &bytes, running)?;
                last_activity = Instant::now();
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
    Ok(())
}

/// Write all of `bytes`, retrying the driver's write timeout (hardware flow
/// control with a peer that is not ready yet) until the session closes.
fn write_fully<P: SerialIo>(
    port: &mut P,
    mut bytes: &[u8],
    running: &AtomicBool,
) -> std::result::Result<(), String> {
    while !bytes.is_empty() && running.load(Ordering::Relaxed) {
        match port.write(bytes) {
            Ok(0) => std::thread::sleep(Duration::from_millis(1)),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if is_retryable(&e) => {}
            Err(e) => return Err(format!("{}: {e}", t("串口写入失败", "serial write failed"))),
        }
    }
    Ok(())
}

fn is_retryable(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
    )
}

fn read_error(e: &std::io::Error) -> String {
    format!("{}: {e}", t("串口读取错误", "serial read error"))
}

#[cfg(test)]
mod pump_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Behaves like a Windows COM handle opened for synchronous I/O: a read
    /// with nothing buffered blocks for the whole read timeout, and nothing
    /// else can use the port meanwhile.
    #[derive(Default)]
    struct FakeLine {
        incoming: VecDeque<u8>,
        written: Vec<u8>,
        blocking_reads: usize,
        write_timeouts_left: usize,
    }

    #[derive(Clone, Default)]
    struct FakePort(Arc<Mutex<FakeLine>>);

    impl SerialIo for FakePort {
        fn available(&mut self) -> std::io::Result<usize> {
            Ok(self.0.lock().unwrap().incoming.len())
        }
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let mut line = self.0.lock().unwrap();
            if line.incoming.is_empty() {
                line.blocking_reads += 1;
                std::thread::sleep(Duration::from_millis(50));
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            let n = buf.len().min(line.incoming.len());
            for slot in &mut buf[..n] {
                *slot = line.incoming.pop_front().unwrap();
            }
            Ok(n)
        }
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut line = self.0.lock().unwrap();
            if line.write_timeouts_left > 0 {
                line.write_timeouts_left -= 1;
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            line.written.extend_from_slice(buf);
            // Local echo, like a console.
            line.incoming.extend(buf.iter().copied());
            Ok(buf.len())
        }
    }

    fn spawn_pump(
        port: FakePort,
    ) -> (
        std_mpsc::Sender<Vec<u8>>,
        Arc<AtomicBool>,
        Arc<Mutex<Vec<u8>>>,
        std::thread::JoinHandle<std::result::Result<(), String>>,
    ) {
        let (tx, rx) = std_mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let echoed = Arc::new(Mutex::new(Vec::new()));
        let (flag, sink) = (running.clone(), echoed.clone());
        let handle = std::thread::spawn(move || {
            let mut port = port;
            pump_serial(&mut port, &rx, &flag, |bytes| {
                sink.lock().unwrap().extend_from_slice(bytes);
                true
            })
        });
        (tx, running, echoed, handle)
    }

    fn wait_for(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        done()
    }

    #[test]
    fn held_key_burst_is_written_and_echoed_without_backlog() {
        // 60 auto-repeated arrow keys. The old reader/writer split waited up
        // to 50 ms per key on Windows: ~3 s of cursor movement after release.
        let port = FakePort::default();
        let (tx, running, echoed, handle) = spawn_pump(port.clone());
        let key = b"\x1b[D".to_vec();
        let start = Instant::now();
        for _ in 0..60 {
            tx.send(key.clone()).unwrap();
        }
        let total = key.len() * 60;
        assert!(
            wait_for(Duration::from_secs(2), || echoed.lock().unwrap().len() == total),
            "echo incomplete"
        );
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "burst took {:?}",
            start.elapsed()
        );
        running.store(false, Ordering::Relaxed);
        drop(tx);
        handle.join().unwrap().unwrap();
        let line = port.0.lock().unwrap();
        assert_eq!(line.written.len(), total);
        assert_eq!(line.blocking_reads, 0, "never block in read on an idle line");
    }

    #[test]
    fn device_output_is_delivered_while_idle() {
        let port = FakePort::default();
        let (tx, running, echoed, handle) = spawn_pump(port.clone());
        port.0.lock().unwrap().incoming.extend(b"Router> ".iter().copied());
        assert!(wait_for(Duration::from_secs(1), || echoed.lock().unwrap().as_slice()
            == b"Router> "));
        running.store(false, Ordering::Relaxed);
        drop(tx);
        handle.join().unwrap().unwrap();
    }

    #[test]
    fn write_timeouts_from_flow_control_are_retried_not_fatal() {
        let port = FakePort::default();
        port.0.lock().unwrap().write_timeouts_left = 3;
        let (tx, running, _echoed, handle) = spawn_pump(port.clone());
        tx.send(b"show run\r".to_vec()).unwrap();
        assert!(wait_for(Duration::from_secs(1), || port.0.lock().unwrap().written
            == b"show run\r"));
        running.store(false, Ordering::Relaxed);
        drop(tx);
        assert!(handle.join().unwrap().is_ok());
    }

    #[test]
    fn closing_the_queue_stops_the_pump() {
        let (tx, _running, _echoed, handle) = spawn_pump(FakePort::default());
        drop(tx);
        assert!(handle.join().unwrap().is_ok());
    }
}
