//! Bounded SSH network stages. Human host-key decisions suspend the handshake
//! budget; credential and MFA prompts are deliberately outside network stages.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use futures::task::AtomicWaker;
use russh::client::{self, Handle, Handler};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::watch;

// An automation call owns all of its target/jump transports, including those
// opened by a spawned SFTP worker. Unlike the handshake guard this remains
// armed after authentication, so cancellation interrupts stalled writes/rekeys.
#[derive(Default)]
struct TransportGroup { cancelled: bool, streams: Vec<Weak<CancelState>> }
tokio::task_local! { static AUTOMATION_TRANSPORTS: Arc<Mutex<TransportGroup>>; }
struct CancelGroupOnDrop(Arc<Mutex<TransportGroup>>);
impl Drop for CancelGroupOnDrop {
    fn drop(&mut self) {
        let mut group = self.0.lock().unwrap_or_else(|e| e.into_inner());
        group.cancelled = true;
        for state in group.streams.iter().filter_map(Weak::upgrade) { state.cancel(); }
    }
}

pub(crate) async fn with_automation_cancellation<F: Future>(future: F) -> F::Output {
    let group = Arc::new(Mutex::new(TransportGroup::default()));
    let _guard = CancelGroupOnDrop(group.clone());
    AUTOMATION_TRANSPORTS.scope(group, future).await
}

// Capture the task-local context before spawning; task locals do not inherit
// automatically. A late-starting child still observes an already-cancelled group.
pub(crate) fn inherit_automation_cancellation<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let group = AUTOMATION_TRANSPORTS.try_with(Clone::clone).ok();
    async move {
        match group {
            Some(group) => AUTOMATION_TRANSPORTS.scope(group, future).await,
            None => future.await,
        }
    }
}

pub(crate) const SSH_NETWORK_TIMEOUT: Duration = Duration::from_secs(15);

/// One budget per stage, rather than a deadline for the whole login: several
/// healthy hops and an arbitrarily slow human decision must remain usable.
pub(crate) async fn network_stage<T, E, F>(label: &str, future: F) -> Result<T>
where
    E: Into<anyhow::Error>,
    F: Future<Output = std::result::Result<T, E>>,
{
    tokio::time::timeout(SSH_NETWORK_TIMEOUT, future)
        .await
        .map_err(|_| stage_timeout(label))?
        .map_err(Into::into)
        .with_context(|| label.to_owned())
}

/// A disconnect queues a local request but can still block behind a full
/// transport queue. Cleanup must not prevent reporting an already known result.
pub(crate) async fn disconnect_ssh<H: Handler>(handle: &Handle<H>, reason: &str) {
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        handle.disconnect(russh::Disconnect::ByApplication, reason, ""),
    )
    .await;
}

fn stage_timeout(label: &str) -> anyhow::Error {
    anyhow!(
        "{label} timed out after {} seconds",
        SSH_NETWORK_TIMEOUT.as_secs()
    )
}

#[derive(Clone)]
pub(crate) struct HostKeyWait(watch::Sender<bool>);

impl Default for HostKeyWait {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}

impl HostKeyWait {
    pub(crate) fn pause(&self) -> HostKeyPause {
        self.0.send_replace(true);
        HostKeyPause(self.clone())
    }
}

pub(crate) struct HostKeyPause(HostKeyWait);

impl Drop for HostKeyPause {
    fn drop(&mut self) {
        let HostKeyWait(waiting) = &self.0;
        waiting.send_replace(false);
    }
}

pub(crate) trait HandshakeHandler: Handler {
    fn host_key_wait(&self) -> &HostKeyWait;
}

async fn prompt_aware_timeout<T>(
    duration: Duration,
    mut waiting: watch::Receiver<bool>,
    future: impl Future<Output = T>,
) -> std::result::Result<T, ()> {
    tokio::pin!(future);
    let mut remaining = duration;
    let mut watching = true;
    loop {
        let paused = *waiting.borrow_and_update();
        let started = tokio::time::Instant::now();
        let deadline = async {
            if paused {
                std::future::pending::<()>().await;
            } else {
                tokio::time::sleep(remaining).await;
            }
        };
        tokio::select! {
            // Process a completed operation / newly opened prompt before expiry.
            biased;
            result = &mut future => return Ok(result),
            changed = waiting.changed(), if watching => watching = changed.is_ok(),
            _ = deadline => return Err(()),
        }
        if !paused {
            remaining = remaining.saturating_sub(started.elapsed());
        }
    }
}

/// russh spawns its KEX task before connect_stream resolves. Dropping only the
/// connect future can orphan that task, so cancellation also wakes and closes
/// its underlying stream. Disarm the guard only after the handle is returned.
pub(crate) async fn connect_ssh_stream<H, S>(
    config: Arc<client::Config>,
    stream: S,
    handler: H,
    label: &str,
) -> Result<Handle<H>>
where
    H: HandshakeHandler + Send + 'static,
    H::Error: std::error::Error + Send + Sync + 'static,
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let waiting = handler.host_key_wait().0.subscribe();
    let (stream, mut cancel) = CancelStream::new(stream);
    let result = prompt_aware_timeout(
        SSH_NETWORK_TIMEOUT,
        waiting,
        client::connect_stream(config, stream, handler),
    )
    .await
    .map_err(|_| stage_timeout(label))?
    .with_context(|| label.to_owned())?;
    cancel.0 = None;
    Ok(result)
}

#[derive(Default)]
struct CancelState {
    cancelled: AtomicBool,
    reader: AtomicWaker,
    writer: AtomicWaker,
}

impl CancelState {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.reader.wake();
        self.writer.wake();
    }
}

struct CancelOnDrop(Option<Arc<CancelState>>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(state) = self.0.take() {
            state.cancel();
        }
    }
}

struct CancelStream<S> {
    inner: S,
    state: Arc<CancelState>,
}

impl<S> CancelStream<S> {
    fn new(inner: S) -> (Self, CancelOnDrop) {
        let state = Arc::new(CancelState::default());
        let _ = AUTOMATION_TRANSPORTS.try_with(|group| {
            let mut group = group.lock().unwrap_or_else(|e| e.into_inner());
            if group.cancelled { state.cancel(); }
            group.streams.push(Arc::downgrade(&state));
        });
        (
            Self {
                inner,
                state: state.clone(),
            },
            CancelOnDrop(Some(state)),
        )
    }

    fn check(&self, cx: &Context<'_>, read: bool) -> io::Result<()> {
        if read {
            self.state.reader.register(cx.waker());
        } else {
            self.state.writer.register(cx.waker());
        }
        if self.state.cancelled.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "SSH handshake cancelled",
            ))
        } else {
            Ok(())
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for CancelStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check(cx, true)?;
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for CancelStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check(cx, false)?;
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(cx, false)?;
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(cx, false)?;
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn unanswered_network_stage_expires() {
        let wait = HostKeyWait::default();
        let result = prompt_aware_timeout(
            Duration::from_millis(20),
            wait.0.subscribe(),
            std::future::pending::<()>(),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn human_host_key_wait_does_not_expire_handshake_budget() {
        let wait = HostKeyWait::default();
        let prompt = wait.clone();
        let result =
            prompt_aware_timeout(Duration::from_millis(30), wait.0.subscribe(), async move {
                let pause = prompt.pause();
                tokio::time::sleep(Duration::from_millis(100)).await;
                drop(pause);
                tokio::time::sleep(Duration::from_millis(5)).await;
                42
            })
            .await;
        assert_eq!(result, Ok(42));
    }

    #[tokio::test]
    async fn handshake_budget_resumes_after_human_prompt() {
        let wait = HostKeyWait::default();
        let prompt = wait.clone();
        let result =
            prompt_aware_timeout(Duration::from_millis(20), wait.0.subscribe(), async move {
                let pause = prompt.pause();
                tokio::time::sleep(Duration::from_millis(60)).await;
                drop(pause);
                std::future::pending::<()>().await;
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cancelling_handshake_wakes_detached_transport_io() {
        let (left, _right) = tokio::io::duplex(64);
        let (mut stream, cancel) = CancelStream::new(left);
        let reader = tokio::spawn(async move { stream.read_u8().await });
        tokio::task::yield_now().await;
        drop(cancel);
        let result = tokio::time::timeout(Duration::from_secs(1), reader)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
    }

    struct TestHandler(HostKeyWait);

    #[async_trait::async_trait]
    impl Handler for TestHandler {
        type Error = russh::Error;
    }

    impl HandshakeHandler for TestHandler {
        fn host_key_wait(&self) -> &HostKeyWait {
            &self.0
        }
    }

    #[tokio::test]
    async fn dropping_inflight_handshake_closes_russh_kex_task() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (stream, peer) = tokio::io::duplex(65536);
        let handshake = tokio::spawn(connect_ssh_stream(
            Arc::new(client::Config::default()),
            stream,
            TestHandler(HostKeyWait::default()),
            "fixture KEX",
        ));
        let mut peer = BufReader::new(peer);
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut banner = String::new();
            peer.read_line(&mut banner).await.unwrap();
            assert!(banner.starts_with("SSH-2.0-"));
            peer.write_all(b"SSH-2.0-fixture\r\n").await.unwrap();
            let mut packet = [0; 65536];
            // Receiving KEX data proves russh has spawned its transport task.
            assert!(peer.read(&mut packet).await.unwrap() > 0);
            handshake.abort();
            assert!(matches!(handshake.await, Err(error) if error.is_cancelled()));
            loop {
                match peer.read(&mut packet).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        })
        .await
        .expect("cancelled KEX transport was orphaned");
    }
}

#[cfg(test)]
mod automation_cancellation_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn automation_end_cancels_established_transport_after_handshake_guard_disarms() {
        let (mut stream, _peer) = with_automation_cancellation(async {
            let (transport, peer) = tokio::io::duplex(8);
            let (stream, mut handshake_guard) = CancelStream::new(transport);
            handshake_guard.0 = None;
            (stream, peer)
        }).await;
        assert!(stream.read_u8().await.is_err());
    }

    #[tokio::test]
    async fn cancellation_wakes_stalled_background_writer() {
        let (writer, _peer) = with_automation_cancellation(async {
            let (transport, peer) = tokio::io::duplex(1);
            let (mut stream, mut handshake_guard) = CancelStream::new(transport);
            handshake_guard.0 = None;
            let writer = tokio::spawn(async move { stream.write_all(&[1; 1024]).await });
            tokio::task::yield_now().await;
            (writer, peer)
        }).await;
        assert!(tokio::time::timeout(Duration::from_secs(1), writer).await.unwrap().unwrap().is_err());
    }

    #[tokio::test]
    async fn late_spawn_inherits_already_cancelled_group() {
        let child = with_automation_cancellation(async {
            inherit_automation_cancellation(async {
                let (transport, _peer) = tokio::io::duplex(8);
                let (mut stream, mut handshake_guard) = CancelStream::new(transport);
                handshake_guard.0 = None;
                stream.write_all(b"no").await
            })
        }).await;
        assert!(child.await.is_err());
    }
}
