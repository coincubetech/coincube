// Rust JSON-RPC Library
// Written in 2015 by
//     Andrew Poelstra <apoelstra@wpsoftware.net>
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

//! Error handling
//!
//! Some useful methods for creating Error objects
//!

use std::io;
use std::{error, fmt};

use serde::{Deserialize, Serialize};

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

    use crate::daemon::DaemonError;

    #[test]
    fn transport_errors_keep_their_daemon_classification() {
        let timeout: Error = io::Error::new(io::ErrorKind::TimedOut, "deadline").into();
        assert!(matches!(
            DaemonError::from(timeout),
            DaemonError::RpcSocket(Some(io::ErrorKind::TimedOut), _)
        ));
        let malformed: Error = serde_json::from_str::<serde_json::Value>("{")
            .unwrap_err()
            .into();
        assert!(matches!(
            DaemonError::from(malformed),
            DaemonError::RpcSocket(None, _)
        ));
        for error in [Error::NonceMismatch, Error::VersionMismatch] {
            assert!(matches!(
                DaemonError::from(error),
                DaemonError::RpcSocket(None, _)
            ));
        }
        assert!(matches!(
            DaemonError::from(Error::NoErrorOrResult),
            DaemonError::NoAnswer
        ));
        assert!(matches!(
            DaemonError::from(Error::NotSupported),
            DaemonError::ClientNotSupported
        ));
    }

    #[test]
    fn rpc_error_wire_data_and_public_alias_are_preserved() {
        let rpc: RpcError = serde_json::from_str(
            r#"{"code":-32602,"message":"bad parameters","data":{"field":"txid"}}"#,
        )
        .unwrap();
        assert_eq!(rpc.data.as_ref().unwrap()["field"], "txid");
        assert_eq!(
            serde_json::to_value(&rpc).unwrap()["code"],
            RpcErrorCode::JSONRPC2_INVALID_PARAMS as i32
        );
        let transport_error: super::super::jsonrpc::Error = rpc.into();
        assert!(
            matches!(DaemonError::from(transport_error), DaemonError::Rpc(-32602, message) if message == "bad parameters")
        );
    }
}
