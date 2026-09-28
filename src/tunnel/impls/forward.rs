//! SSH port forwarding / tunnels (#56).
//!
//! Local (-L) and dynamic (-D / SOCKS5) forwards run client-side: we listen on
//! a local TCP port and, per inbound connection, open a `direct-tcpip` channel
//! on the SSH session, then splice the two streams together. Remote (-R)
//! forwards are requested with `tcpip_forward` and serviced in the session
//! handler when the server opens channels back (see `ssh.rs`).

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use russh::client::{Handle, Msg};
use russh::Channel;
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::ssh::{ClientHandler, SessionEvent};

/// Emit a one-line notice into the terminal output stream.
fn notice(events: &UnboundedSender<SessionEvent>, msg: String) {
    let _ = events.send(SessionEvent::Output(format!("\r\n[meatshell] {msg}\r\n")));
}

fn bind_target(bind_addr: &str, bind_port: u16) -> String {
    let addr = if bind_addr.trim().is_empty() {
        "127.0.0.1"
    } else {
        bind_addr.trim()
    };
    // IPv6 literals must be bracketed for TcpListener::bind ("[::1]:8080");
    // an already-bracketed address is left as-is (#109).
    if addr.contains(':') && !addr.starts_with('[') {
        format!("[{addr}]:{bind_port}")
    } else {
        format!("{addr}:{bind_port}")
    }
}

/// Open a `direct-tcpip` channel to `host:port`, recording the originating peer
/// (some servers log / ACL on it).
async fn open_direct(
    handle: &Arc<tokio::sync::Mutex<Handle<ClientHandler>>>,
    host: &str,
    port: u16,
    peer: SocketAddr,
) -> Result<Channel<Msg>, russh::Error> {
    handle
        .lock()
        .await
        .channel_open_direct_tcpip(
            host.to_string(),
            port as u32,
            peer.ip().to_string(),
            peer.port() as u32,
        )
        .await
}

/// Local forward (-L): listen locally and tunnel each connection to
/// `target_host:target_port` reached from the SSH server's side.
pub fn spawn_local(
    handle: Arc<tokio::sync::Mutex<Handle<ClientHandler>>>,
    bind_addr: String,
    bind_port: u16,
    target_host: String,
    target_port: u16,
    events: UnboundedSender<SessionEvent>,
) -> JoinHandle<()> {
    let bind = bind_target(&bind_addr, bind_port);
    tokio::spawn(async move {
        let listener = match TcpListener::bind(&bind).await {
            Ok(l) => l,
            Err(e) => {
                notice(&events, format!("-L {bind} 监听失败 / bind failed: {e}"));
                return;
            }
        };
        notice(&events, format!("-L {bind} → {target_host}:{target_port}"));
        loop {
            let (mut inbound, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let handle = handle.clone();
            let host = target_host.clone();
            let ev = events.clone();
            tokio::spawn(async move {
                match open_direct(&handle, &host, target_port, peer).await {
                    Ok(ch) => {
                        let mut stream = ch.into_stream();
                        let _ = copy_bidirectional(&mut inbound, &mut stream).await;
                    }
                    Err(e) => notice(
                        &ev,
                        format!("-L {host}:{target_port} 连接失败 / open failed: {e}"),
                    ),
                }
            });
        }
    })
}

/// Dynamic forward (-D): a minimal SOCKS5 proxy. Each accepted connection
/// negotiates SOCKS5 (no auth, CONNECT only), then we open a `direct-tcpip`
/// channel to the requested destination and splice.
pub fn spawn_dynamic(
    handle: Arc<tokio::sync::Mutex<Handle<ClientHandler>>>,
    bind_addr: String,
    bind_port: u16,
    events: UnboundedSender<SessionEvent>,
) -> JoinHandle<()> {
    let bind = bind_target(&bind_addr, bind_port);
    tokio::spawn(async move {
        let listener = match TcpListener::bind(&bind).await {
            Ok(l) => l,
            Err(e) => {
                notice(&events, format!("-D {bind} 监听失败 / bind failed: {e}"));
                return;
            }
        };
        notice(&events, format!("-D {bind} (SOCKS5)"));
        loop {
            let (inbound, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let handle = handle.clone();
            let ev = events.clone();
            tokio::spawn(async move {
                if let Err(e) = socks5_serve(&handle, inbound, peer).await {
                    tracing::debug!("socks5 conn ended: {e}");
                    let _ = ev; // notices for SOCKS are too noisy; keep to trace
                }
            });
        }
    })
}

/// Handle one SOCKS5 client connection end-to-end.
async fn socks5_serve(
    handle: &Arc<tokio::sync::Mutex<Handle<ClientHandler>>>,
    mut inbound: TcpStream,
    peer: SocketAddr,
) -> std::io::Result<()> {
    // Greeting: VER, NMETHODS, METHODS[NMETHODS].
    let mut head = [0u8; 2];
    inbound.read_exact(&mut head).await?;
    if head[0] != 0x05 {
        return Ok(()); // not SOCKS5
    }
    let nmethods = head[1] as usize;
    let mut methods = vec![0u8; nmethods];
    inbound.read_exact(&mut methods).await?;
    // Reply: VER=5, METHOD=0 (no authentication).
    inbound.write_all(&[0x05, 0x00]).await?;

    // Request: VER, CMD, RSV, ATYP, DST.ADDR, DST.PORT.
    let mut req = [0u8; 4];
    inbound.read_exact(&mut req).await?;
    if req[0] != 0x05 {
        return Ok(());
    }
    if req[1] != 0x01 {
        // Only CONNECT is supported → reply "command not supported".
        let _ = inbound.write_all(&socks_reply(0x07)).await;
        return Ok(());
    }
    let host = match req[3] {
        0x01 => {
            let mut a = [0u8; 4];
            inbound.read_exact(&mut a).await?;
            Ipv4Addr::from(a).to_string()
        }
        0x04 => {
            let mut a = [0u8; 16];
            inbound.read_exact(&mut a).await?;
            Ipv6Addr::from(a).to_string()
        }
        0x03 => {
            let mut len = [0u8; 1];
            inbound.read_exact(&mut len).await?;
            let mut d = vec![0u8; len[0] as usize];
            inbound.read_exact(&mut d).await?;
            String::from_utf8_lossy(&d).into_owned()
        }
        _ => {
            let _ = inbound.write_all(&socks_reply(0x08)).await; // addr type unsupported
            return Ok(());
        }
    };
    let mut port = [0u8; 2];
    inbound.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);

    match open_direct(handle, &host, port, peer).await {
        Ok(ch) => {
            inbound.write_all(&socks_reply(0x00)).await?; // succeeded
            let mut stream = ch.into_stream();
            let _ = copy_bidirectional(&mut inbound, &mut stream).await;
        }
        Err(_) => {
            let _ = inbound.write_all(&socks_reply(0x05)).await; // connection refused
        }
    }
    Ok(())
}

/// A SOCKS5 reply with the given reply code and a zeroed bound address
/// (`0.0.0.0:0`) — clients don't need the real bound address for CONNECT.
fn socks_reply(code: u8) -> [u8; 10] {
    [0x05, code, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
}
