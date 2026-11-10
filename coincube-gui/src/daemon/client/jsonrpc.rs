// Rust JSON-RPC Library
// Written by
//     Andrew Poelstra <apoelstra@wpsoftware.net>
//     Wladimir J. van der Laan <laanwj@gmail.com>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//
//! Client support
//!
//! Support for connecting to JSONRPC servers over UNIX socets, sending requests,
//! and parsing responses
//!

#[cfg(not(windows))]
use tokio::net::UnixStream;

use std::fmt::Debug;

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{error, fmt, io};
#[cfg(not(windows))]
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[cfg(not(windows))]
use tracing::debug;

/// A handle to a remote JSONRPC server
#[derive(Debug, Clone)]
pub struct JsonRPCClient {
    #[cfg_attr(windows, allow(unused))]
    sockpath: PathBuf,
    timeout: Duration,
}

#[async_trait::async_trait]
impl super::Client for JsonRPCClient {
    type Error = Error;
    async fn request<S: Serialize + Debug + Send, D: DeserializeOwned + Debug + Send>(
        &self,
        method: &str,
        params: Option<S>,
    ) -> Result<D, Self::Error> {
        self.send_request(method, params)
            .await
            .and_then(|res| res.into_result())
    }
}

// A single deadline covers connection, write and the complete response.
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(30);

impl JsonRPCClient {
    /// Creates a new client
    pub fn new<P: AsRef<Path>>(sockpath: P) -> JsonRPCClient {
        JsonRPCClient {
            sockpath: sockpath.as_ref().to_path_buf(),
            timeout: DEFAULT_RPC_TIMEOUT,
        }
    }

    /// Override the request deadline. `None` restores the finite default.
    #[allow(dead_code)]
    pub fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout.unwrap_or(DEFAULT_RPC_TIMEOUT);
    }

    #[cfg(windows)]
    pub async fn send_request<S: Serialize + Debug + Send, D: DeserializeOwned + Debug + Send>(
        &self,
        _method: &str,
        _params: Option<S>,
    ) -> Result<Response<D>, Error> {
        Err(Error::NotSupported)
    }

    /// Sends a request to a client
    #[cfg(not(windows))]
    pub async fn send_request<S: Serialize + Debug + Send, D: DeserializeOwned + Debug + Send>(
        &self,
        method: &str,
        params: Option<S>,
    ) -> Result<Response<D>, Error> {
        let request = Request {
            method,
            params,
            id: std::process::id(),
            jsonrpc: "2.0",
        };
        let mut encoded = serde_json::to_vec(&request)?;
        encoded.push(b'\n');
        debug!("Sending to coincubed: method={}", method);

        let response: Response<D> = tokio::time::timeout(self.timeout, async {
            let mut stream = UnixStream::connect(&self.sockpath).await?;
            stream.write_all(&encoded).await?;
            // The daemon writes a JSON object without a newline and leaves the
            // socket open. Decode as soon as a complete object arrives; neither
            // EOF nor a newline can be required as a response delimiter.
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let count = stream.read(&mut chunk).await?;
                if count == 0 {
                    return if bytes.is_empty() {
                        Err(Error::NoErrorOrResult)
                    } else {
                        serde_json::from_slice(&bytes).map_err(Error::from)
                    };
                }
                bytes.extend_from_slice(&chunk[..count]);
                match serde_json::from_slice(&bytes) {
                    Ok(response) => return Ok(response),
                    Err(error) if error.is_eof() => continue,
                    Err(error) => return Err(Error::Json(error)),
                }
            }
        })
        .await
        .map_err(|_| {
            Error::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "daemon RPC deadline exceeded; operation outcome may be unknown",
            ))
        })??;
        if response
            .jsonrpc
            .as_ref()
            .is_some_and(|version| version != "2.0")
        {
            return Err(Error::VersionMismatch);
        }

        if response.id != request.id {
            return Err(Error::NonceMismatch);
        }

        debug!("Received from coincubed: method={}", method);

        Ok(response)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// A JSONRPC request object
pub struct Request<'f, T: Serialize> {
    /// The name of the RPC call
    pub method: &'f str,
    /// Parameters to the RPC call
    pub params: Option<T>,
    /// Identifier for this Request, which should appear in the response
    pub id: u32,
    /// jsonrpc field, MUST be "2.0"
    pub jsonrpc: &'f str,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
/// A JSONRPC response object
pub struct Response<T> {
    /// A result if there is one, or null
    pub result: Option<T>,
    /// An error if there is one, or null
    pub error: Option<RpcError>,
    /// Identifier for this Request, which should match that of the request
    pub id: u32,
    /// jsonrpc field, MUST be "2.0"
    pub jsonrpc: Option<String>,
}

impl<T> Response<T> {
    /// Extract the result from a response, consuming the response
    pub fn into_result(self) -> Result<T, Error> {
        if let Some(e) = self.error {
            return Err(Error::Rpc(e));
        }

        self.result.ok_or(Error::NoErrorOrResult)
    }

    /// Returns whether or not the `result` field is empty
    #[allow(dead_code)]
    pub fn is_none(&self) -> bool {
        self.result.is_none()
    }
}

#[allow(dead_code)]
#[derive(Debug)]
#[allow(non_camel_case_types)]
pub enum RpcErrorCode {
    // Standard errors defined by JSON-RPC 2.0 standard
    /// Invalid request
    JSONRPC2_INVALID_REQUEST = -32600,
    /// Method not found
    JSONRPC2_METHOD_NOT_FOUND = -32601,
    /// Invalid parameters
    JSONRPC2_INVALID_PARAMS = -32602,
}

/// A library error
#[derive(Debug)]
pub enum Error {
    /// Json error
    Json(serde_json::Error),
    /// IO Error
    Io(io::Error),
    /// Error response
    Rpc(RpcError),
    /// Response has neither error nor result
    NoErrorOrResult,
    /// Response to a request did not have the expected nonce
    NonceMismatch,
    /// Response to a request had a jsonrpc field other than "2.0"
    VersionMismatch,
    /// unix socket communication is not supported.
    NotSupported,
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Error {
        Error::Json(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

impl From<RpcError> for Error {
    fn from(e: RpcError) -> Error {
        Error::Rpc(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Json(ref e) => write!(f, "JSON decode error: {}", e),
            Error::Io(ref e) => write!(f, "IO error response: {}", e),
            Error::Rpc(ref r) => write!(f, "RPC error response: {:?}", r),
            Error::NoErrorOrResult => write!(f, "Malformed RPC response"),
            Error::NonceMismatch => write!(f, "Nonce of response did not match nonce of request"),
            Error::VersionMismatch => write!(f, "`jsonrpc` field set to non-\"2.0\""),
            Error::NotSupported => write!(f, "unix socket communication is not supported"),
        }
    }
}

impl error::Error for Error {
    fn cause(&self) -> Option<&dyn error::Error> {
        match *self {
            Error::Json(ref e) => Some(e),
            _ => None,
        }
    }
}

impl From<Error> for super::DaemonError {
    fn from(e: Error) -> super::DaemonError {
        match e {
            Error::Io(e) => super::DaemonError::RpcSocket(Some(e.kind()), format!("io: {:?}", e)),
            Error::Json(e) => super::DaemonError::RpcSocket(None, format!("json decode: {}", e)),
            Error::NonceMismatch => {
                super::DaemonError::RpcSocket(None, format!("transport: {}", e))
            }
            Error::VersionMismatch => {
                super::DaemonError::RpcSocket(None, format!("transport: {}", e))
            }
            Error::NoErrorOrResult => super::DaemonError::NoAnswer,
            Error::NotSupported => super::DaemonError::ClientNotSupported,
            Error::Rpc(e)
                if i64::from(e.code) == coincubed::commands::UNSAFE_LEGACY_ALTERNATIVE_ERROR =>
            {
                super::DaemonError::UnsafeLegacyAlternative(e.message)
            }
            Error::Rpc(e) => super::DaemonError::Rpc(e.code, e.message),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
/// A JSONRPC error object
pub struct RpcError {
    /// The integer identifier of the error
    pub code: i32,
    /// A string describing the error
    pub message: String,
    /// Additional data specific to the error
    pub data: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_legacy_rpc_code_preserves_the_typed_daemon_error() {
        let daemon_error = super::super::DaemonError::from(Error::Rpc(RpcError {
            code: coincubed::commands::UNSAFE_LEGACY_ALTERNATIVE_ERROR as i32,
            message: "policy refusal".to_string(),
            data: None,
        }));
        assert!(matches!(
            daemon_error,
            super::super::DaemonError::UnsafeLegacyAlternative(message)
                if message == "policy refusal"
        ));
    }
}

#[cfg(all(test, not(windows)))]
mod transport_tests {
    use super::*;
    use crate::daemon::client::Client;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::UnixListener;

    struct SocketFile(PathBuf);
    impl Drop for SocketFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn listener() -> (SocketFile, UnixListener, JsonRPCClient) {
        let path = std::env::temp_dir().join(format!("r{:x}.sock", rand::random::<u64>()));
        let listener = UnixListener::bind(&path).unwrap();
        let client = JsonRPCClient::new(&path);
        (SocketFile(path), listener, client)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stalled_peer_times_out_without_blocking_the_executor() {
        let (_file, listener, mut client) = listener();
        client.set_timeout(Some(Duration::from_millis(100)));
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let peer = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            let _ = released.await;
        });
        let request = client.request::<(), serde_json::Value>("getinfo", None);
        tokio::pin!(request);
        // On a single-thread runtime this timer cannot fire if the RPC blocks.
        tokio::select! {
            result = &mut request => panic!("request completed before heartbeat: {result:?}"),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
        assert!(matches!(request.await, Err(Error::Io(e)) if e.kind() == io::ErrorKind::TimedOut));
        let _ = release.send(());
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn partial_response_does_not_extend_the_total_deadline() {
        let (_file, listener, mut client) = listener();
        client.set_timeout(Some(Duration::from_millis(100)));
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            if stream.write_all(b"{\"result\":\"").await.is_err() {
                return;
            }
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                if stream.write_all(b"x").await.is_err() {
                    break;
                }
            }
        });
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            client.request::<(), serde_json::Value>("getinfo", None),
        )
        .await
        .expect("a streaming peer must not defeat the deadline");
        assert!(matches!(result, Err(Error::Io(e)) if e.kind() == io::ErrorKind::TimedOut));
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn fragmented_response_completes_without_newline_or_eof() {
        let (_file, listener, client) = listener();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut request = String::new();
            stream.read_line(&mut request).await.unwrap();
            let request: serde_json::Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["method"], "getlabels");
            let response = serde_json::to_vec(&serde_json::json!({
                "id": request["id"], "jsonrpc": "2.0", "result": {"labels": {}}
            }))
            .unwrap();
            for part in response.chunks(7) {
                stream.get_mut().write_all(part).await.unwrap();
                tokio::task::yield_now().await;
            }
            let _ = released.await;
        });
        let daemon = crate::daemon::client::Coincubed::new(client);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            crate::daemon::Daemon::get_labels(&daemon, &std::collections::HashSet::new()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.is_empty());
        let _ = release.send(());
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn protocol_errors_are_preserved() {
        for (body, expected) in [
            (
                serde_json::json!({"id": 0, "jsonrpc": "2.0", "result": {}}),
                "nonce",
            ),
            (
                serde_json::json!({"id": std::process::id(), "jsonrpc": "1.0", "result": {}}),
                "version",
            ),
            (
                serde_json::json!({"id": std::process::id(), "jsonrpc": "2.0", "error": {"code": -32602, "message": "bad params"}}),
                "rpc",
            ),
        ] {
            let (_file, listener, client) = listener();
            let peer = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                stream.read_line(&mut String::new()).await.unwrap();
                stream
                    .get_mut()
                    .write_all(&serde_json::to_vec(&body).unwrap())
                    .await
                    .unwrap();
            });
            let error = client
                .request::<(), serde_json::Value>("getinfo", None)
                .await
                .unwrap_err();
            assert!(matches!(
                (expected, error),
                ("nonce", Error::NonceMismatch)
                    | ("version", Error::VersionMismatch)
                    | ("rpc", Error::Rpc(_))
            ));
            peer.await.unwrap();
        }
    }
}
