//! Errors returned by fallible SDK operations.
//!
//! A tampered destination and an unfinished lookup are [`crate::VerifyStatus`] values.
//! `verify` does not return them as errors. A logo URL on the wrong origin still
//! returns [`BrantaError::LogoUrlDomainMismatch`].

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrantaError {
    #[error("Unauthorized")]
    Unauthorized,

    #[error("request failed with status {status}")]
    RequestFailed { status: u16 },

    #[error("No payment returned from server")]
    NoPaymentReturned,

    #[error("destination type {0} cannot be encrypted")]
    UnsupportedDestinationType(String),

    #[error("{0} domain does not match the configured base URL")]
    LogoUrlDomainMismatch(String),

    #[error("invalid encrypted data: too short")]
    EncryptedDataTooShort,

    #[error("encryption failed: {0}")]
    EncryptionFailed(String),

    #[error("decryption failed: {0}")]
    DecryptionFailed(String),

    /// A lookup did not finish. `verify` turns this into [`crate::VerifyStatus::Incomplete`].
    #[error("lookup did not finish")]
    Incomplete,

    #[error("HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
