//! Authenticated Unix-only IPC. Every request checks the live grant twice.
use crate::{Error, Reader, Request, Response};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    task::{JoinHandle, JoinSet},
};

/// Maximum serialized request frame size, including the newline, in bytes.
pub const REQUEST_LIMIT: usize = 4096;
/// Maximum serialized response frame size, including the newline, in bytes.
pub const RESPONSE_LIMIT: usize = 65536;
/// Wall-clock budget for one peer connection and its wallet read.
pub const DEADLINE: Duration = Duration::from_secs(10);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    credential: String,
    request: Request,
}

/// Memory-only bearer grant bound to one wallet, capability set and expiry.
pub struct Grant {
    session_valid: Box<dyn Fn() -> bool + Send + Sync>,
    credential: String,
    wallet_id: String,
    capabilities: [bool; 3],
    expires: Instant,
    revoked: AtomicBool,
    /// Completed successful reads; this is not a connected-client count.
    pub successful_calls: AtomicU64,
}
impl Grant {
    /// Generate a fresh 256-bit credential and a monotonic expiration deadline.
    ///
    /// Capabilities are balance, confirmed history and policy, in that order.
    pub fn new(wallet_id: String, capabilities: [bool; 3], duration: Duration) -> Self {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self {
            session_valid: Box::new(|| true),
            credential: bytes.iter().map(|b| format!("{b:02x}")).collect(),
            wallet_id,
            capabilities,
            expires: Instant::now() + duration,
            revoked: AtomicBool::new(false),
            successful_calls: AtomicU64::new(0),
        }
    }
    /// Bind authorization to the desktop's live unlocked-session generation.
    pub fn with_session_guard(mut self, valid: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.session_valid = Box::new(valid);
        self
    }
    /// Irreversibly invalidate the grant, including future completion checks.
    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::SeqCst);
    }
    /// Require an unrevoked grant, unexpired lifetime and valid desktop session.
    pub fn active(&self) -> bool {
        !self.revoked.load(Ordering::SeqCst)
            && Instant::now() < self.expires
            && (self.session_valid)()
    }
    /// Check live authorization, constant-time credential equality, scope and input.
    ///
    /// Wallet identity and capabilities are checked before validation errors are
    /// returned, so an unauthorized caller does not learn wallet details.
    pub fn authorize(&self, credential: &str, request: &Request) -> Result<(), Error> {
        if !self.active() || !bool::from(self.credential.as_bytes().ct_eq(credential.as_bytes())) {
            return Err(Error::AccessDenied);
        }
        if let Some((wallet, cap)) = request.scope()
            && (wallet != self.wallet_id || !self.capabilities[cap])
        {
            return Err(Error::AccessDenied);
        }
        if !request.valid() {
            return Err(Error::InvalidInput);
        }
        Ok(())
    }
    /// Return the bearer credential only for private trusted-host configuration.
    /// Never include this value in model tool arguments, results or logs.
    pub fn credential(&self) -> &str {
        &self.credential
    }
}

/// Session-owned private listener; dropping it revokes access and aborts workers.
pub struct Server {
    /// Shared grant used for trusted settings status and explicit revocation.
    pub grant: Arc<Grant>,
    /// Private endpoint path copied into trusted client configuration.
    pub socket: PathBuf,
    task: JoinHandle<()>,
    _directory: tempfile::TempDir,
}
impl Server {
    /// Bind a fresh private Unix endpoint and start bounded, authenticated reads.
    /// Requires an active Tokio runtime; setup errors do not authorize a client.
    pub fn start(reader: Arc<dyn Reader>, grant: Grant) -> io::Result<Self> {
        // /tmp keeps sockaddr_un paths short even on macOS. tempfile creates
        // the directory atomically with 0700; never chmod an existing path.
        let directory = tempfile::Builder::new()
            .prefix("tenshu-mcp-")
            .tempdir_in("/tmp")?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        let socket = directory.path().join("rpc");
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let grant = Arc::new(grant);
        let permission = grant.clone();
        let task = tokio::spawn(async move {
            let Ok(listener) = UnixListener::from_std(listener) else {
                permission.revoke();
                return;
            };
            let mut workers = JoinSet::new();
            let expiry = tokio::time::Instant::from_std(permission.expires);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(expiry) => { permission.revoke(); break; }
                    Some(_) = workers.join_next(), if !workers.is_empty() => {}
                    connection = listener.accept(), if workers.len() < 8 => {
                        let Ok((stream, _)) = connection else { permission.revoke(); break; };
                        let reader = reader.clone(); let grant = permission.clone();
                        workers.spawn(async move { let _ = tokio::time::timeout(DEADLINE, serve(stream, reader, grant)).await; });
                    }
                }
            }
            // JoinSet drop aborts all in-flight reads, including stalled peers.
        });
        Ok(Self {
            grant,
            socket,
            task,
            _directory: directory,
        })
    }
}
impl Drop for Server {
    /// Revoke synchronously before scheduling listener and worker cancellation.
    fn drop(&mut self) {
        self.grant.revoke();
        self.task.abort();
    }
}

/// Read one newline-terminated frame, rejecting truncation and oversized input.
/// The caller supplies the connection deadline; this helper only bounds bytes.
pub async fn line<R: tokio::io::AsyncRead + Unpin>(reader: R, max: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    BufReader::new(reader)
        .take((max + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .await?;
    if bytes.len() > max || bytes.last() != Some(&b'\n') {
        return Err(io::Error::other("invalid frame"));
    }
    Ok(bytes)
}
/// Authenticate one request before and after a read, then write a bounded result.
async fn serve(
    mut stream: UnixStream,
    reader: Arc<dyn Reader>,
    grant: Arc<Grant>,
) -> io::Result<()> {
    let bytes = line(&mut stream, REQUEST_LIMIT).await?;
    let result = match serde_json::from_slice::<Envelope>(&bytes) {
        Ok(envelope) => {
            match grant.authorize(&envelope.credential, &envelope.request) {
                Err(e) => Err(e),
                Ok(()) => {
                    let request = envelope.request;
                    let result = reader.read(request.clone()).await;
                    // Never release a completion from a revoked or expired session.
                    grant.authorize(&envelope.credential, &request).and(result)
                }
            }
        }
        Err(_) => Err(Error::InvalidInput),
    };
    if result.is_ok() {
        grant.successful_calls.fetch_add(1, Ordering::Relaxed);
    }
    let mut bytes = serde_json::to_vec(&result).map_err(|_| io::Error::other("serialization"))?;
    if bytes.len() >= RESPONSE_LIMIT {
        bytes = serde_json::to_vec(&Err::<Response, _>(Error::Unavailable))?;
    }
    bytes.push(b'\n');
    stream.write_all(&bytes).await
}
/// Make one authenticated local request within the frame and time limits.
/// Endpoint and credential are trusted host configuration, never tool arguments;
/// transport failures become sanitized unavailable errors.
pub async fn call(socket: &Path, credential: &str, request: Request) -> Result<Response, Error> {
    let operation = async {
        let mut stream = UnixStream::connect(socket)
            .await
            .map_err(|_| Error::Unavailable)?;
        let envelope = Envelope {
            credential: credential.into(),
            request,
        };
        let mut bytes = serde_json::to_vec(&envelope).map_err(|_| Error::InvalidInput)?;
        bytes.push(b'\n');
        if bytes.len() > REQUEST_LIMIT {
            return Err(Error::InvalidInput);
        }
        stream
            .write_all(&bytes)
            .await
            .map_err(|_| Error::Unavailable)?;
        let bytes = line(&mut stream, RESPONSE_LIMIT)
            .await
            .map_err(|_| Error::Unavailable)?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Unavailable)?
    };
    tokio::time::timeout(DEADLINE, operation)
        .await
        .map_err(|_| Error::Unavailable)?
}
