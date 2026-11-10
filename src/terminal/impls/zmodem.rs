//! Minimal bidirectional ZMODEM implementation for terminal `sz` / `rz`.
//!
//! When the user runs `sz <file>` in the terminal, the remote starts a ZMODEM
//! send. We implement just enough of the protocol to receive: reply to ZRQINIT
//! with ZRINIT, accept ZFILE, drive the transfer with ZRPOS/ZACK, collect the
//! ZDATA subpackets into a local file, and finish on ZEOF/ZFIN. Files land in
//! the user's Downloads directory (FinalShell style).
//!
//! We advertise CANFC32, so the sender uses CRC-32 binary frames; the CRC-16
//! paths are implemented for completeness but rarely exercised.
//!
//! When the remote runs `rz`, the complementary sender path lets the user pick
//! local files and streams them over the existing PTY. Every header is logged
//! at debug level to aid diagnosis, since the binary protocol is otherwise hard
//! to inspect.

use crate::i18n::t;
use crate::ssh::SessionEvent;
use anyhow::{bail, Context, Result};
use russh::client::Msg;
use russh::{Channel, ChannelMsg};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::UnboundedSender;

#[path = "zmodem_send.rs"]
mod send_impl;
pub(crate) use send_impl::send;

// --- Frame types -----------------------------------------------------------
const ZRQINIT: u8 = 0;
const ZRINIT: u8 = 1;
const ZACK: u8 = 3;
const ZFILE: u8 = 4;
const ZSKIP: u8 = 5;
const ZNAK: u8 = 6;
const ZABORT: u8 = 7;
const ZFIN: u8 = 8;
const ZRPOS: u8 = 9;
const ZDATA: u8 = 10;
const ZEOF: u8 = 11;
const ZCAN: u8 = 16;

// --- Control bytes ---------------------------------------------------------
const ZDLE: u8 = 0x18; // ZMODEM escape (also CAN)
const ZPAD: u8 = b'*';
const ZBIN: u8 = b'A'; // binary header, CRC-16
const ZHEX: u8 = b'B'; // hex header, CRC-16
const ZBIN32: u8 = b'C'; // binary header, CRC-32

// Data-subpacket terminators (the byte right after a ZDLE inside data).
const ZCRCE: u8 = b'h'; // end of frame, header follows, no ZACK
const ZCRCG: u8 = b'i'; // frame continues, no ZACK
const ZCRCQ: u8 = b'j'; // frame continues, ZACK expected
const ZCRCW: u8 = b'k'; // end of frame, ZACK expected
const ZRUB0: u8 = b'l'; // escaped 0x7f
const ZRUB1: u8 = b'm'; // escaped 0xff

// ZRINIT capability flags we advertise: full-duplex, overlap I/O, CRC-32.
const CANFDX: u8 = 0x01;
const CANOVIO: u8 = 0x02;
const CANFC32: u8 = 0x20;

/// Receive one or more files via ZMODEM. `first` is the channel chunk that
/// triggered detection (it contains the leading ZRQINIT).
///
/// Returns any bytes read past the end of the ZMODEM session (typically the
/// shell prompt the sender's exit produces) so the caller can feed them back to
/// the terminal — otherwise the prompt would be swallowed. On a protocol failure
/// it returns an error and the caller cancels.
pub async fn receive(
    channel: &mut Channel<Msg>,
    first: &[u8],
    events: &UnboundedSender<SessionEvent>,
) -> Result<Vec<u8>> {
    let dest = download_dir();
    tokio::fs::create_dir_all(&dest)
        .await
        .with_context(|| format!("create download dir {}", dest.display()))?;

    tracing::debug!(
        "zmodem: receive start, first[{}]={:02x?}",
        first.len(),
        &first[..first.len().min(80)]
    );

    let mut rx = Rx::new(channel, first);
    let mut received = 0u32;
    let mut cur: Option<CurFile> = None;
    // A header already read ahead (e.g. the next ZFILE peeked after a ZEOF).
    let mut pending: Option<(u8, [u8; 4])> = None;

    loop {
        let (ftype, hdr) = match pending.take() {
            Some(h) => h,
            None => rx.read_header().await?,
        };
        tracing::debug!("zmodem rx header type={ftype} data={hdr:02x?}");
        match ftype {
            ZRQINIT => {
                rx.send_hex(ZRINIT, [0, 0, 0, CANFDX | CANOVIO | CANFC32])
                    .await?
            }
            ZFILE => {
                // Data subpacket: "name\0size mtime mode ...".
                let (sub, _end) = rx.read_subpacket(true).await?;
                let nul = sub.iter().position(|&b| b == 0).unwrap_or(sub.len());
                let name = sanitize(&String::from_utf8_lossy(&sub[..nul]));
                let size = sub
                    .get(nul + 1..)
                    .map(|rest| String::from_utf8_lossy(rest))
                    .and_then(|s| s.split_whitespace().next().map(str::to_owned))
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let path = dest.join(&name);
                let file = tokio::fs::File::create(&path)
                    .await
                    .with_context(|| format!("create {}", path.display()))?;
                let id = format!("zmodem-{}", uuid::Uuid::new_v4());
                emit(events, &id, &name, (false, 0, size, 0, ""));
                cur = Some(CurFile {
                    file,
                    name,
                    id,
                    size,
                    written: 0,
                });
                rx.send_hex(ZRPOS, 0u32.to_le_bytes()).await?;
            }
            ZDATA => loop {
                let (chunk, end) = rx.read_subpacket(true).await?;
                if let Some(c) = cur.as_mut() {
                    c.file.write_all(&chunk).await.context("write file")?;
                    c.written += chunk.len() as u64;
                    emit(
                        events,
                        &c.id,
                        &c.name,
                        (false, c.written, c.size.max(c.written), 0, ""),
                    );
                }
                match end {
                    ZCRCG => continue,
                    ZCRCQ => {
                        let pos = cur.as_ref().map(|c| c.written).unwrap_or(0) as u32;
                        rx.send_hex(ZACK, pos.to_le_bytes()).await?;
                    }
                    ZCRCE => break,
                    ZCRCW => {
                        let pos = cur.as_ref().map(|c| c.written).unwrap_or(0) as u32;
                        rx.send_hex(ZACK, pos.to_le_bytes()).await?;
                        break;
                    }
                    _ => break,
                }
            },
            ZEOF => {
                if let Some(mut c) = cur.take() {
                    c.file.flush().await.context("flush file")?;
                    emit(
                        events,
                        &c.id,
                        &c.name,
                        (false, c.written, c.size.max(c.written), 1, ""),
                    );
                    received += 1;
                }
                // ZEOF ends one *file*, not the whole session (#109). Tell the
                // sender we're ready for more (ZRINIT) and peek the next frame:
                // a multi-file `sz` sends the next ZFILE; otherwise it sends ZFIN
                // (or just waits for our ZFIN and sends nothing). The peek is
                // capped by a short timeout so a finished single-file transfer
                // never blocks on the long per-byte read timeout — anything that
                // isn't a ZFILE drops to the close handshake below.
                rx.send_hex(ZRINIT, [0, 0, 0, CANFDX | CANOVIO | CANFC32])
                    .await?;
                match tokio::time::timeout(Duration::from_secs(2), rx.read_header()).await {
                    Ok(Ok(h)) if h.0 == ZFILE => pending = Some(h),
                    _ => break, // ZFIN / unexpected / parse error / timeout → done
                }
            }
            ZFIN => break, // sender signals the whole session is done
            ZCAN | ZABORT => bail!("{}", t("传输被远端取消", "transfer aborted by sender")),
            ZNAK => { /* sender NAK; just keep going */ }
            _ => tracing::debug!("zmodem: ignoring unhandled frame type {ftype}"),
        }
    }

    // Close handshake. The sender (lrzsz `sz`) just sent ZEOF and is finishing
    // its session: it expects a ZRINIT, then sends ZFIN and waits for OUR ZFIN
    // before emitting "OO" (over-and-out) and exiting. We reply so it exits
    // promptly, and consume its ZFIN + OO here so they don't leak to the terminal
    // or get re-detected as a new transfer. Whatever follows (the shell prompt)
    // stays in the buffer and is returned to the caller (#76).
    if received > 0 {
        // Send ZRINIT + ZFIN *immediately and unconditionally*. This sender
        // finishes its session waiting for OUR ZFIN and does not send its own
        // first; if we wait to read its ZFIN it never comes and the sender hangs
        // ~100 s on its global timeout. Sending ZFIN proactively makes it exit at
        // once. Then swallow its lingering close frames (its ZFIN / "OO"),
        // stopping at the first byte that isn't part of a ZMODEM hex header or
        // "OO" — that byte begins the shell prompt, returned as leftover (#76).
        let _ = rx
            .send_hex(ZRINIT, [0, 0, 0, CANFDX | CANOVIO | CANFC32])
            .await;
        let _ = rx.send_hex(ZFIN, [0, 0, 0, 0]).await;
        let _ = tokio::time::timeout(Duration::from_millis(800), async {
            for _ in 0..64 {
                match rx.byte().await {
                    Ok(b) if is_close_byte(b) => continue,
                    Ok(b) => {
                        rx.buf.push_front(b); // start of the shell prompt
                        break;
                    }
                    Err(_) => break,
                }
            }
        })
        .await;
    }

    let _ = events.send(SessionEvent::Output(
        format!(
            "\r\n[meatshell] {} {} → {}\r\n",
            received,
            t("个文件已通过 sz 下载到", "file(s) downloaded via sz to"),
            dest.display()
        )
        .into(),
    ));
    // Hand back any trailing bytes (the shell prompt) so the caller can display
    // them instead of the receiver swallowing them.
    Ok(rx.buf.drain(..).collect())
}

struct CurFile {
    file: tokio::fs::File,
    name: String,
    id: String,
    size: u64,
    written: u64,
}

/// Reader/writer over the SSH channel with a byte buffer and ZMODEM helpers.
struct Rx<'a> {
    ch: &'a mut Channel<Msg>,
    buf: VecDeque<u8>,
    closed: bool,
}

impl<'a> Rx<'a> {
    fn new(ch: &'a mut Channel<Msg>, first: &[u8]) -> Self {
        Rx {
            ch,
            buf: first.iter().copied().collect(),
            closed: false,
        }
    }

    /// Next raw byte; pulls more channel data when the buffer drains.
    async fn byte(&mut self) -> Result<u8> {
        loop {
            if let Some(b) = self.buf.pop_front() {
                return Ok(b);
            }
            if self.closed {
                bail!("channel closed during ZMODEM");
            }
            // Guard against a stalled transfer hanging the session forever.
            let msg = tokio::time::timeout(Duration::from_secs(30), self.ch.wait())
                .await
                .map_err(|_| anyhow::anyhow!("ZMODEM read timed out"))?;
            match msg {
                Some(ChannelMsg::Data { data }) => self.buf.extend(data.iter().copied()),
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    self.buf.extend(data.iter().copied())
                }
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => self.closed = true,
                _ => {}
            }
        }
    }

    /// One logical byte with ZDLE un-escaping applied.
    async fn zbyte(&mut self) -> Result<u8> {
        let b = self.byte().await?;
        if b != ZDLE {
            return Ok(b);
        }
        let e = self.byte().await?;
        Ok(match e {
            ZRUB0 => 0x7f,
            ZRUB1 => 0xff,
            _ => e ^ 0x40,
        })
    }

    /// Read the next frame header, scanning past any padding/garbage. Returns
    /// the frame type and its four data bytes.
    async fn read_header(&mut self) -> Result<(u8, [u8; 4])> {
        loop {
            // Find ZDLE followed by a recognised format byte.
            if self.byte().await? != ZDLE {
                continue;
            }
            match self.byte().await? {
                ZHEX => return self.read_hex_header().await,
                ZBIN => return self.read_bin_header(false).await,
                ZBIN32 => return self.read_bin_header(true).await,
                _ => continue, // not a header start (could be a CAN run); keep scanning
            }
        }
    }

    async fn read_hex_header(&mut self) -> Result<(u8, [u8; 4])> {
        let mut bytes = [0u8; 5];
        for b in bytes.iter_mut() {
            *b = self.hex_byte().await?;
        }
        let crc_hi = self.hex_byte().await?;
        let crc_lo = self.hex_byte().await?;
        let crc = u16::from_be_bytes([crc_hi, crc_lo]);
        if crc16(&bytes) != crc {
            bail!("hex header CRC mismatch");
        }
        // Swallow the trailing CR/LF (+ optional XON) up to the newline. GNU
        // lrzsz sets the high bit on LF (`0x8a`). ZFIN has no following XON, so
        // failing to recognise that byte deadlocks: we wait for a third byte
        // while remote `rz` waits for our final "OO" (#308).
        for _ in 0..3 {
            if is_zmodem_line_feed(self.byte().await?) {
                break;
            }
        }
        Ok((bytes[0], [bytes[1], bytes[2], bytes[3], bytes[4]]))
    }

    async fn read_bin_header(&mut self, crc32: bool) -> Result<(u8, [u8; 4])> {
        let mut bytes = [0u8; 5];
        for b in bytes.iter_mut() {
            *b = self.zbyte().await?;
        }
        if crc32 {
            let mut c = [0u8; 4];
            for b in c.iter_mut() {
                *b = self.zbyte().await?;
            }
            if crc32_of(&bytes) != u32::from_le_bytes(c) {
                bail!("bin32 header CRC mismatch");
            }
        } else {
            let hi = self.zbyte().await?;
            let lo = self.zbyte().await?;
            if crc16(&bytes) != u16::from_be_bytes([hi, lo]) {
                bail!("bin16 header CRC mismatch");
            }
        }
        Ok((bytes[0], [bytes[1], bytes[2], bytes[3], bytes[4]]))
    }

    /// Read a data subpacket, returning the (un-escaped) data and the terminator
    /// byte (ZCRCE/ZCRCG/ZCRCQ/ZCRCW). The CRC covers data + terminator.
    async fn read_subpacket(&mut self, crc32: bool) -> Result<(Vec<u8>, u8)> {
        let mut data = Vec::new();
        loop {
            let b = self.byte().await?;
            if b != ZDLE {
                data.push(b);
                continue;
            }
            let e = self.byte().await?;
            match e {
                ZCRCE | ZCRCG | ZCRCQ | ZCRCW => {
                    let mut crcbuf = data.clone();
                    crcbuf.push(e);
                    if crc32 {
                        let mut c = [0u8; 4];
                        for x in c.iter_mut() {
                            *x = self.zbyte().await?;
                        }
                        if crc32_of(&crcbuf) != u32::from_le_bytes(c) {
                            bail!("subpacket CRC-32 mismatch");
                        }
                    } else {
                        let hi = self.zbyte().await?;
                        let lo = self.zbyte().await?;
                        if crc16(&crcbuf) != u16::from_be_bytes([hi, lo]) {
                            bail!("subpacket CRC-16 mismatch");
                        }
                    }
                    return Ok((data, e));
                }
                ZRUB0 => data.push(0x7f),
                ZRUB1 => data.push(0xff),
                _ => data.push(e ^ 0x40),
            }
        }
    }

    /// Read two hex ASCII digits into a byte.
    async fn hex_byte(&mut self) -> Result<u8> {
        let hi = from_hex(self.byte().await?)?;
        let lo = from_hex(self.byte().await?)?;
        Ok((hi << 4) | lo)
    }

    /// Send a hex-encoded header (always accepted regardless of CRC mode).
    async fn send_hex(&mut self, ftype: u8, data: [u8; 4]) -> Result<()> {
        let payload = [ftype, data[0], data[1], data[2], data[3]];
        let crc = crc16(&payload);
        let mut out = vec![ZPAD, ZPAD, ZDLE, ZHEX];
        for &b in &payload {
            out.extend_from_slice(&hex_digits(b));
        }
        out.extend_from_slice(&hex_digits((crc >> 8) as u8));
        out.extend_from_slice(&hex_digits((crc & 0xff) as u8));
        out.extend_from_slice(b"\r\n");
        // XON after every hex header except ZACK/ZFIN (per the protocol).
        if ftype != ZACK && ftype != ZFIN {
            out.push(0x11);
        }
        tracing::debug!("zmodem tx type={ftype} bytes={:02x?}", &out);
        self.ch.data(&out[..]).await.context("zmodem send header")?;
        Ok(())
    }
}

/// True for bytes that make up a ZMODEM hex close frame (ZFIN) or the "OO"
/// over-and-out, used to drain the sender's lingering close frames without
/// eating the shell prompt that follows (which starts with ESC/letters) (#76).
fn is_close_byte(b: u8) -> bool {
    matches!(b,
        b'*' | ZDLE | b'A' | b'B' | b'C' | b'O'
        | b'\r' | b'\n' | 0x8a | 0x11
        | b'0'..=b'9' | b'a'..=b'f')
}

/// Where received files go: the user's Downloads dir, else a temp fallback.
fn download_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::env::temp_dir().join("meatshell"))
}

/// Reduce a sender-supplied name to a safe basename inside the download dir.
fn sanitize(name: &str) -> String {
    let base = name
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !matches!(c, '\0' | '/' | '\\'))
        .collect();
    // Trim trailing dots/spaces (illegal on Windows) and leading spaces, but
    // KEEP leading dots so dotfiles like ".viminfo" keep their name (#76).
    let cleaned = cleaned
        .trim_end_matches(|c| c == '.' || c == ' ')
        .trim_start_matches(' ');
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        "download".to_string()
    } else {
        cleaned.to_string()
    }
}

fn emit(
    events: &UnboundedSender<SessionEvent>,
    id: &str,
    name: &str,
    progress: (bool, u64, u64, u8, &str),
) {
    let (is_upload, transferred, total, state, msg) = progress;
    let _ = events.send(SessionEvent::SftpTransfer {
        id: id.to_string(),
        name: name.to_string(),
        is_upload,
        transferred,
        total,
        sampled_at: None,
        state,
        msg: msg.to_string(),
    });
}

fn hex_digits(b: u8) -> [u8; 2] {
    const H: &[u8; 16] = b"0123456789abcdef";
    [H[(b >> 4) as usize], H[(b & 0x0f) as usize]]
}

fn from_hex(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => bail!("invalid hex digit {c:#x}"),
    }
}

fn is_zmodem_line_feed(byte: u8) -> bool {
    byte & 0x7f == b'\n'
}

/// CRC-16/XMODEM (poly 0x1021, init 0, no final xor) — ZMODEM header/subpacket.
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-32/ISO-HDLC (zlib): init 0xFFFFFFFF, reflected, final xor 0xFFFFFFFF.
fn crc32_of(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_known_vector() {
        // CRC-16/XMODEM of "123456789" is 0x31C3.
        assert_eq!(crc16(b"123456789"), 0x31C3);
    }

    #[test]
    fn crc32_known_vector() {
        // CRC-32 of "123456789" is 0xCBF43926.
        assert_eq!(crc32_of(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn accepts_lrzsz_high_bit_line_feed() {
        assert!(is_zmodem_line_feed(b'\n'));
        assert!(is_zmodem_line_feed(0x8a));
        assert!(!is_zmodem_line_feed(b'\r'));
    }

    #[test]
    fn sanitize_strips_paths() {
        assert_eq!(sanitize("/etc/passwd"), "passwd");
        assert_eq!(sanitize("..\\..\\x"), "x");
        assert_eq!(sanitize(""), "download");
        // Dotfiles keep their leading dot.
        assert_eq!(sanitize(".viminfo"), ".viminfo");
        assert_eq!(sanitize("/home/jeff/.bashrc"), ".bashrc");
        // Trailing dots/spaces are trimmed; pure-dot names rejected.
        assert_eq!(sanitize("name..."), "name");
        assert_eq!(sanitize(".."), "download");
    }
}
