//! SSH agent authentication (#528): log in with keys held by a running SSH
//! agent such as Bitwarden, 1Password, KeePassXC or the OpenSSH agent. The
//! private key never leaves the agent; it signs the login and may ask the user
//! to approve first (Bitwarden shows a prompt).
//!
//! This is meatshell's own minimal agent client (RFC draft-miller-ssh-agent:
//! REQUEST_IDENTITIES and SIGN_REQUEST only), not russh's. It predates the
//! russh 0.61 upgrade, when russh's agent client still allocated whatever frame
//! length the agent claimed (RUSTSEC-2026-0154); every frame is capped at
//! 256 KiB like the upstream fix. russh only sees the agent through its public
//! `Signer` trait.
//!
//! Where the agent is found:
//! * Windows: `SSH_AUTH_SOCK` when it names a pipe, else the OpenSSH agent
//!   pipe `\\.\pipe\openssh-ssh-agent` (Bitwarden and 1Password serve it too).
//! * Elsewhere: `SSH_AUTH_SOCK`, then Bitwarden's own socket
//!   (`~/.bitwarden-ssh-agent.sock`, and its Flatpak / Snap / macOS App Store
//!   locations), since apps started from the desktop often don't inherit
//!   `SSH_AUTH_SOCK`. An agent that answers but holds no keys is passed over
//!   for the next one: on macOS `SSH_AUTH_SOCK` is launchd's own agent, which
//!   is usually empty while Bitwarden listens on its own socket.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::i18n::t;

/// Same cap as the upstream fix for RUSTSEC-2026-0154.
const MAX_FRAME: usize = 256 * 1024;
const FAILURE: u8 = 5;
const REQUEST_IDENTITIES: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;
const SIGN_REQUEST: u8 = 13;
const SIGN_RESPONSE: u8 = 14;
/// Listing keys is local and quick; a hung agent must not hang the login.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(windows)]
pub(crate) const DEFAULT_WINDOWS_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

trait AgentStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AgentStream for T {}

/// A connection to a running SSH agent.
pub(crate) struct Agent {
    stream: Box<dyn AgentStream>,
    /// Pipe or socket path, for messages.
    pub(crate) location: String,
}

/// A key the agent offers, with its comment (usually the key's name).
pub(crate) struct AgentKey {
    pub(crate) key: ssh_key::PublicKey,
    pub(crate) comment: String,
}

/// Failure while the agent signs a login. `Send` is russh's own channel
/// error; the rest is the agent refusing or failing.
#[derive(Debug)]
pub(crate) enum AgentSignError {
    Send,
    Agent(anyhow::Error),
}

impl From<russh::SendError> for AgentSignError {
    fn from(_: russh::SendError) -> Self {
        Self::Send
    }
}

impl std::fmt::Display for AgentSignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Send => f.write_str("SSH connection closed during agent authentication"),
            Self::Agent(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for AgentSignError {}

impl Agent {
    /// Connect to the user's SSH agent.
    pub(crate) async fn connect() -> Result<Self> {
        open().await
    }

    #[cfg(test)]
    fn over<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(stream: S) -> Self {
        Self {
            stream: Box::new(stream),
            location: "test".into(),
        }
    }

    /// Send one request frame and read one bounded reply frame.
    async fn call(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        let mut frame = Vec::with_capacity(4 + request.len());
        frame.extend_from_slice(&(request.len() as u32).to_be_bytes());
        frame.extend_from_slice(request);
        self.stream.write_all(&frame).await.context("writing to the SSH agent")?;
        self.stream.flush().await.context("writing to the SSH agent")?;
        let mut len = [0u8; 4];
        self.stream
            .read_exact(&mut len)
            .await
            .context("reading from the SSH agent")?;
        let len = u32::from_be_bytes(len) as usize;
        if len == 0 || len > MAX_FRAME {
            bail!("the SSH agent sent an invalid reply ({len} bytes)");
        }
        let mut reply = vec![0u8; len];
        self.stream
            .read_exact(&mut reply)
            .await
            .context("reading from the SSH agent")?;
        Ok(reply)
    }

    /// The keys the agent offers. Keys meatshell can't parse are skipped.
    pub(crate) async fn identities(&mut self) -> Result<Vec<AgentKey>> {
        let reply = tokio::time::timeout(LIST_TIMEOUT, self.call(&[REQUEST_IDENTITIES]))
            .await
            .map_err(|_| anyhow!("the SSH agent did not answer within {} seconds", LIST_TIMEOUT.as_secs()))??;
        let mut r = Reader(&reply);
        match r.u8()? {
            IDENTITIES_ANSWER => {}
            FAILURE => bail!("the SSH agent refused to list its keys"),
            other => bail!("unexpected SSH agent reply {other}"),
        }
        let count = r.u32()? as usize;
        let mut keys = Vec::new();
        for _ in 0..count {
            let blob = r.string()?;
            let comment = String::from_utf8_lossy(r.string()?).into_owned();
            match ssh_key::PublicKey::from_bytes(blob) {
                Ok(key) => keys.push(AgentKey { key, comment }),
                Err(error) => tracing::debug!("skipping SSH agent key {comment:?}: {error}"),
            }
        }
        Ok(keys)
    }

    /// Ask the agent to sign `data` with `key`; returns the signature blob
    /// (`string algorithm || string signature`). For RSA, `hash` picks
    /// `rsa-sha2-256` / `rsa-sha2-512` (the name russh announced); `None` is
    /// SHA-1 `ssh-rsa`. Other key types ignore it.
    async fn sign(
        &mut self,
        key: &ssh_key::PublicKey,
        hash: Option<ssh_key::HashAlg>,
        data: &[u8],
    ) -> Result<Vec<u8>> {
        let blob = key.to_bytes().context("encoding the public key")?;
        let mut request = vec![SIGN_REQUEST];
        put_string(&mut request, &blob);
        put_string(&mut request, data);
        request.extend_from_slice(&sign_flags(key, hash).to_be_bytes());
        // No timeout here: the agent may wait for the user to approve.
        let reply = self.call(&request).await?;
        let mut r = Reader(&reply);
        match r.u8()? {
            SIGN_RESPONSE => {}
            FAILURE => bail!("{}", t(
                "SSH Agent 拒绝了签名请求（可能在 Bitwarden 中被拒绝，或密钥已锁定）",
                "the SSH agent declined to sign (denied in Bitwarden, or the key is locked)",
            )),
            other => bail!("unexpected SSH agent reply {other}"),
        }
        let signature = r.string()?.to_vec();
        // Must be `string algorithm || string signature`.
        let mut s = Reader(&signature);
        s.string()?;
        s.string()?;
        Ok(signature)
    }
}

/// The user may need to unlock their password manager and approve the
/// login; the server's own login grace time (OpenSSH: 120 s) still applies.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Servers count every offered key towards MaxAuthTries (OpenSSH: 6).
const MAX_KEYS: usize = 5;

/// Log in as `user` with the agent's keys. Each key is first offered without
/// a signature; the agent is only asked to sign — and Bitwarden only
/// prompts — for a key the server accepts. `Ok(false)` = no key was accepted.
pub(crate) async fn login<H: russh::client::Handler>(
    handle: &mut russh::client::Handle<H>,
    user: &str,
    agent: &mut Agent,
) -> Result<bool> {
    let keys = agent.identities().await?;
    if keys.is_empty() {
        bail!(
            "{} ({})",
            t(
                "SSH Agent 中没有密钥。请在 Bitwarden 中添加 SSH 密钥并确认密码库已解锁",
                "The SSH agent has no keys. Add an SSH key in Bitwarden and make sure the vault is unlocked",
            ),
            agent.location
        );
    }
    for entry in keys.into_iter().take(MAX_KEYS) {
        tracing::debug!("offering SSH agent key {:?}", entry.comment);
        let hash = super::ssh::rsa_hash(handle, entry.key.algorithm().is_rsa()).await;
        let attempt = tokio::time::timeout(
            APPROVAL_TIMEOUT,
            handle.authenticate_publickey_with(user, entry.key, hash, &mut *agent),
        )
        .await
        .map_err(|_| {
            anyhow!(
                "{} ({}s)",
                t("等待 SSH Agent 批准超时", "Timed out waiting for the SSH agent to approve the login"),
                APPROVAL_TIMEOUT.as_secs()
            )
        })?;
        match attempt {
            Ok(result) if result.success() => return Ok(true),
            Ok(_) => continue,
            Err(AgentSignError::Agent(error)) => return Err(error),
            Err(AgentSignError::Send) => bail!("the SSH connection closed during login"),
        }
    }
    Ok(false)
}

impl russh::Signer for Agent {
    type Error = AgentSignError;

    /// russh hands over the data to sign and expects it back with the
    /// signature blob appended as an SSH string.
    async fn auth_sign(
        &mut self,
        key: &russh::keys::agent::AgentIdentity,
        hash_alg: Option<ssh_key::HashAlg>,
        to_sign: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, Self::Error> {
        let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = key else {
            return Err(AgentSignError::Agent(anyhow!("SSH agent certificates are not supported")));
        };
        let signature = self.sign(key, hash_alg, &to_sign).await.map_err(AgentSignError::Agent)?;
        let mut out = to_sign;
        put_string(&mut out, &signature);
        Ok(out)
    }
}

/// SIGN_REQUEST flags: SSH_AGENT_RSA_SHA2_256 = 2, SSH_AGENT_RSA_SHA2_512 = 4.
fn sign_flags(key: &ssh_key::PublicKey, hash: Option<ssh_key::HashAlg>) -> u32 {
    if !key.algorithm().is_rsa() {
        return 0;
    }
    match hash {
        Some(ssh_key::HashAlg::Sha256) => 2,
        Some(ssh_key::HashAlg::Sha512) => 4,
        _ => 0,
    }
}

fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// Bounds-checked reader over one agent reply.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.0.len() {
            bail!("truncated SSH agent reply");
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn string(&mut self) -> Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}

#[cfg(windows)]
async fn open() -> Result<Agent> {
    use tokio::net::windows::named_pipe::ClientOptions;
    const ERROR_PIPE_BUSY: i32 = 231;
    let path = std::env::var("SSH_AUTH_SOCK")
        .ok()
        .filter(|p| p.starts_with(r"\\.\pipe\") || p.starts_with(r"\\?\pipe\"))
        .unwrap_or_else(|| DEFAULT_WINDOWS_PIPE.to_string());
    for _ in 0..40 {
        match ClientOptions::new().open(&path) {
            Ok(pipe) => {
                return Ok(Agent {
                    stream: Box::new(pipe),
                    location: path,
                })
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(no_agent_message(&path)),
            Err(e) => return Err(e).with_context(|| format!("opening the SSH agent at {path}")),
        }
    }
    bail!("the SSH agent at {path} stayed busy")
}

/// Bitwarden's own agent socket, relative to `$HOME`.
#[cfg(unix)]
const BITWARDEN_SOCKETS: &[&str] = &[
    // .deb / AppImage / macOS .dmg
    ".bitwarden-ssh-agent.sock",
    // Flatpak
    ".var/app/com.bitwarden.desktop/data/.bitwarden-ssh-agent.sock",
    // Snap
    "snap/bitwarden/current/.bitwarden-ssh-agent.sock",
    // macOS App Store (sandboxed)
    "Library/Containers/com.bitwarden.desktop/Data/.bitwarden-ssh-agent.sock",
];

#[cfg(unix)]
async fn open() -> Result<Agent> {
    let mut candidates = Vec::new();
    if let Some(sock) = std::env::var_os("SSH_AUTH_SOCK").filter(|s| !s.is_empty()) {
        candidates.push(std::path::PathBuf::from(sock));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        for socket in BITWARDEN_SOCKETS {
            let path = home.join(socket);
            if !candidates.contains(&path) {
                candidates.push(path);
            }
        }
    }
    first_agent_with_keys(&candidates).await
}

/// The first agent in `candidates` that holds keys. One that answers with no
/// keys only wins when none has any (macOS: launchd's empty agent behind
/// `SSH_AUTH_SOCK` must not hide Bitwarden's socket, #528).
#[cfg(unix)]
async fn first_agent_with_keys(candidates: &[std::path::PathBuf]) -> Result<Agent> {
    let mut empty = None;
    let mut last_error = None;
    for path in candidates {
        if !path.exists() {
            continue;
        }
        let location = path.display().to_string();
        let stream = match tokio::net::UnixStream::connect(path).await {
            Ok(stream) => stream,
            Err(e) => {
                last_error = Some(anyhow::Error::new(e).context(format!("connecting to the SSH agent at {location}")));
                continue;
            }
        };
        let mut agent = Agent {
            stream: Box::new(stream),
            location,
        };
        match agent.identities().await {
            Ok(keys) if !keys.is_empty() => return Ok(agent),
            Ok(_) => {
                tracing::debug!("SSH agent at {} has no keys, trying the next one", agent.location);
                empty.get_or_insert(agent);
            }
            Err(e) => {
                tracing::debug!("SSH agent at {} failed to list keys: {e:#}", agent.location);
                last_error = Some(e.context(format!("listing keys of the SSH agent at {}", agent.location)));
            }
        }
    }
    if let Some(agent) = empty {
        return Ok(agent);
    }
    match last_error {
        Some(e) => Err(e),
        None => bail!(no_agent_message("$SSH_AUTH_SOCK")),
    }
}

fn no_agent_message(location: &str) -> String {
    format!(
        "{} ({location})",
        t(
            "找不到正在运行的 SSH Agent。请启动 Bitwarden 等密码管理器并开启其 SSH Agent，或启动 OpenSSH Agent 服务",
            "No SSH agent is running. Start Bitwarden (or another password manager) with its SSH agent turned on, or the OpenSSH agent service",
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn ed25519() -> ssh_key::PublicKey {
        ssh_key::PublicKey::new(
            ssh_key::public::KeyData::Ed25519(ssh_key::public::Ed25519PublicKey([3u8; 32])),
            "test",
        )
    }

    /// Run `agent_side` as the agent: read one request frame, write `reply`.
    async fn exchange(reply: Vec<u8>) -> (Agent, tokio::task::JoinHandle<Vec<u8>>) {
        let (client, mut server) = duplex(1 << 20);
        let task = tokio::spawn(async move {
            let mut len = [0u8; 4];
            server.read_exact(&mut len).await.unwrap();
            let mut request = vec![0u8; u32::from_be_bytes(len) as usize];
            server.read_exact(&mut request).await.unwrap();
            server.write_all(&reply).await.unwrap();
            request
        });
        (Agent::over(client), task)
    }

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut out = (body.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(body);
        out
    }

    #[tokio::test]
    async fn lists_keys_and_skips_ones_it_cannot_parse() {
        let key = ed25519();
        let mut body = vec![IDENTITIES_ANSWER];
        body.extend_from_slice(&2u32.to_be_bytes());
        put_string(&mut body, &key.to_bytes().unwrap());
        put_string(&mut body, b"bitwarden key");
        put_string(&mut body, b"garbage");
        put_string(&mut body, b"bad");
        let (mut agent, task) = exchange(frame(&body)).await;
        let keys = agent.identities().await.unwrap();
        assert_eq!(task.await.unwrap(), [REQUEST_IDENTITIES]);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].comment, "bitwarden key");
        assert_eq!(keys[0].key.key_data(), key.key_data());
    }

    #[tokio::test]
    async fn oversized_frames_are_refused_before_allocating() {
        // RUSTSEC-2026-0154: a hostile length must not drive the allocation.
        let (mut agent, _task) = exchange(u32::MAX.to_be_bytes().to_vec()).await;
        let error = match agent.identities().await {
            Ok(_) => panic!("an oversized frame was accepted"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("invalid reply"), "{error}");
    }

    #[tokio::test]
    async fn signing_appends_the_signature_as_an_ssh_string() {
        use russh::Signer as _;
        let key = ed25519();
        let mut sig = Vec::new();
        put_string(&mut sig, b"ssh-ed25519");
        put_string(&mut sig, &[7u8; 64]);
        let mut body = vec![SIGN_RESPONSE];
        put_string(&mut body, &sig);
        let (mut agent, task) = exchange(frame(&body)).await;
        let signed = agent
            .auth_sign(&identity(key), Some(ssh_key::HashAlg::Sha512), b"session-data".to_vec())
            .await
            .unwrap();
        let request = task.await.unwrap();
        assert_eq!(request[0], SIGN_REQUEST);
        assert!(request.ends_with(&0u32.to_be_bytes()), "no RSA flag for ed25519");
        let mut expected = b"session-data".to_vec();
        put_string(&mut expected, &sig);
        assert_eq!(&signed[..], &expected[..]);
    }

    #[tokio::test]
    async fn a_refused_signature_is_an_error() {
        use russh::Signer as _;
        let (mut agent, _task) = exchange(frame(&[FAILURE])).await;
        let result = agent.auth_sign(&identity(ed25519()), None, b"x".to_vec()).await;
        assert!(matches!(result, Err(AgentSignError::Agent(_))));
    }

    /// An agent on a Unix socket that answers every REQUEST_IDENTITIES with
    /// `keys` copies of one ed25519 key.
    #[cfg(unix)]
    fn unix_agent(path: &std::path::Path, keys: u32) {
        let listener = tokio::net::UnixListener::bind(path).unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    loop {
                        let mut len = [0u8; 4];
                        if stream.read_exact(&mut len).await.is_err() {
                            return;
                        }
                        let mut request = vec![0u8; u32::from_be_bytes(len) as usize];
                        stream.read_exact(&mut request).await.unwrap();
                        let mut body = vec![IDENTITIES_ANSWER];
                        body.extend_from_slice(&keys.to_be_bytes());
                        for _ in 0..keys {
                            put_string(&mut body, &ed25519().to_bytes().unwrap());
                            put_string(&mut body, b"bitwarden key");
                        }
                        stream.write_all(&frame(&body)).await.unwrap();
                    }
                });
            }
        });
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_empty_agent_does_not_hide_one_with_keys() {
        // #528 on macOS: SSH_AUTH_SOCK is launchd's agent (no keys), Bitwarden
        // (App Store) listens in its sandbox container.
        let dir = std::env::temp_dir().join(format!("meatshell-agent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let launchd = dir.join("launchd.sock");
        let bitwarden = dir.join("bitwarden.sock");
        let missing = dir.join("missing.sock");
        unix_agent(&launchd, 0);
        unix_agent(&bitwarden, 1);

        let candidates = [missing.clone(), launchd.clone(), bitwarden.clone()];
        let mut agent = first_agent_with_keys(&candidates).await.unwrap();
        assert_eq!(agent.location, bitwarden.display().to_string());
        // The connection stays usable for the login that follows.
        assert_eq!(agent.identities().await.unwrap().len(), 1);

        // With no agent holding keys, the empty one is still used, so the
        // login reports "no keys" with its location.
        let agent = first_agent_with_keys(&[missing.clone(), launchd.clone()]).await.unwrap();
        assert_eq!(agent.location, launchd.display().to_string());

        assert!(first_agent_with_keys(&[missing]).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn identity(key: ssh_key::PublicKey) -> russh::keys::agent::AgentIdentity {
        russh::keys::agent::AgentIdentity::PublicKey { key, comment: String::new() }
    }

    #[test]
    fn rsa_sign_requests_ask_for_the_hash_russh_announced() {
        let rsa = rsa_private().public_key().clone();
        assert_eq!(sign_flags(&rsa, Some(ssh_key::HashAlg::Sha256)), 2);
        assert_eq!(sign_flags(&rsa, Some(ssh_key::HashAlg::Sha512)), 4);
        assert_eq!(sign_flags(&rsa, None), 0, "SHA-1 ssh-rsa");
        assert_eq!(sign_flags(&ed25519(), Some(ssh_key::HashAlg::Sha512)), 0);
    }

    // ── End to end: a real russh server, a fake agent holding real keys ──

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Serves REQUEST_IDENTITIES / SIGN_REQUEST from `keys` until closed and
    /// counts signatures (each one would be a Bitwarden prompt).
    async fn fake_agent(
        mut stream: tokio::io::DuplexStream,
        keys: Vec<ssh_key::PrivateKey>,
        signs: Arc<AtomicUsize>,
    ) {
        use russh::keys::signature::Signer as _;
        loop {
            let mut len = [0u8; 4];
            if stream.read_exact(&mut len).await.is_err() {
                return;
            }
            let mut request = vec![0u8; u32::from_be_bytes(len) as usize];
            stream.read_exact(&mut request).await.unwrap();
            let mut body = Vec::new();
            match request[0] {
                REQUEST_IDENTITIES => {
                    body.push(IDENTITIES_ANSWER);
                    body.extend_from_slice(&(keys.len() as u32).to_be_bytes());
                    for (i, key) in keys.iter().enumerate() {
                        put_string(&mut body, &key.public_key().to_bytes().unwrap());
                        put_string(&mut body, format!("key {i}").as_bytes());
                    }
                }
                SIGN_REQUEST => {
                    let mut r = Reader(&request[1..]);
                    let blob = r.string().unwrap();
                    let data = r.string().unwrap();
                    let flags = r.u32().unwrap();
                    let key = keys
                        .iter()
                        .find(|k| k.public_key().to_bytes().unwrap() == blob)
                        .expect("asked to sign with a key the agent doesn't hold");
                    signs.fetch_add(1, Ordering::SeqCst);
                    // Like OpenSSH's agent: the flags pick the RSA hash.
                    let signature: ssh_key::Signature = match key.key_data().rsa() {
                        Some(rsa) => {
                            let hash = match flags {
                                2 => ssh_key::HashAlg::Sha256,
                                4 => ssh_key::HashAlg::Sha512,
                                // Tests expect SHA-2; a SHA-1 request fails the login.
                                _ => {
                                    stream.write_all(&frame(&[FAILURE])).await.unwrap();
                                    continue;
                                }
                            };
                            (rsa, Some(hash)).try_sign(data).unwrap()
                        }
                        None => key.try_sign(data).unwrap(),
                    };
                    let mut sig = Vec::new();
                    put_string(&mut sig, signature.algorithm().as_str().as_bytes());
                    put_string(&mut sig, signature.as_bytes());
                    body.push(SIGN_RESPONSE);
                    put_string(&mut body, &sig);
                }
                _ => body.push(FAILURE),
            }
            stream.write_all(&frame(&body)).await.unwrap();
        }
    }

    struct TestServer {
        trusted: ssh_key::PublicKey,
    }

    impl russh::server::Handler for TestServer {
        type Error = russh::Error;
        async fn auth_publickey_offered(
            &mut self,
            _: &str,
            key: &ssh_key::PublicKey,
        ) -> std::result::Result<russh::server::Auth, Self::Error> {
            Ok(self.verdict(key))
        }
        async fn auth_publickey(
            &mut self,
            _: &str,
            key: &ssh_key::PublicKey,
        ) -> std::result::Result<russh::server::Auth, Self::Error> {
            Ok(self.verdict(key))
        }
    }

    impl TestServer {
        fn verdict(&self, key: &ssh_key::PublicKey) -> russh::server::Auth {
            if key.key_data() == self.trusted.key_data() {
                russh::server::Auth::Accept
            } else {
                russh::server::Auth::Reject { proceed_with_methods: None, partial_success: false }
            }
        }
    }

    struct TestClient;

    impl russh::client::Handler for TestClient {
        type Error = russh::Error;
        async fn check_server_key(&mut self, _: &ssh_key::PublicKey) -> std::result::Result<bool, Self::Error> {
            Ok(true)
        }
    }

    fn ed25519_private() -> ssh_key::PrivateKey {
        use rand::RngCore as _;
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        ssh_key::private::Ed25519Keypair::from_seed(&seed).into()
    }

    /// The 2048-bit RSA fixture shared with the key-format tests.
    fn rsa_private() -> ssh_key::PrivateKey {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/ssh/impls/testdata/keys/rsa_openssh");
        russh::keys::load_secret_key(path, None).unwrap()
    }

    /// Log in to a server trusting `trusted` with an agent holding `keys`.
    async fn agent_login(trusted: ssh_key::PublicKey, keys: Vec<ssh_key::PrivateKey>) -> (Result<bool>, usize) {
        let (client_io, server_io) = duplex(1 << 16);
        let server_config = Arc::new(russh::server::Config {
            keys: vec![ed25519_private()],
            methods: russh::MethodSet::from(&[russh::MethodKind::PublicKey][..]),
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        tokio::spawn(async move {
            if let Ok(session) =
                russh::server::run_stream(server_config, server_io, TestServer { trusted }).await
            {
                let _ = session.await;
            }
        });
        let mut handle = russh::client::connect_stream(
            Arc::new(russh::client::Config::default()),
            client_io,
            TestClient,
        )
        .await
        .unwrap();
        let (agent_io, agent_side) = duplex(1 << 16);
        let signs = Arc::new(AtomicUsize::new(0));
        tokio::spawn(fake_agent(agent_side, keys, signs.clone()));
        let mut agent = Agent::over(agent_io);
        let result = login(&mut handle, "deploy", &mut agent).await;
        (result, signs.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn logs_in_with_the_accepted_key_and_signs_only_once() {
        let decoy = ed25519_private();
        let trusted = ed25519_private();
        let (result, signs) =
            agent_login(trusted.public_key().clone(), vec![decoy, trusted.clone()]).await;
        assert!(result.unwrap(), "the server verified the agent's signature");
        assert_eq!(signs, 1, "the rejected decoy key must not be signed (no extra Bitwarden prompt)");
    }

    #[tokio::test]
    async fn logs_in_with_an_rsa_agent_key_using_sha2() {
        // russh 0.49 could only sign agent RSA logins as SHA-1 `ssh-rsa`;
        // now the agent is asked for the rsa-sha2-* the server supports.
        let trusted = rsa_private();
        let (result, signs) = agent_login(trusted.public_key().clone(), vec![trusted]).await;
        assert!(result.unwrap(), "the server verified the agent's RSA signature");
        assert_eq!(signs, 1);
    }

    #[tokio::test]
    async fn no_accepted_key_fails_without_signing() {
        let (result, signs) = agent_login(ed25519_private().public_key().clone(), vec![ed25519_private()]).await;
        assert!(!result.unwrap());
        assert_eq!(signs, 0);
    }

    #[test]
    fn truncated_replies_are_errors_not_panics() {
        let mut r = Reader(&[0, 0, 0, 9, 1, 2]);
        assert!(r.string().is_err());
        assert!(Reader(&[]).u8().is_err());
    }
}
