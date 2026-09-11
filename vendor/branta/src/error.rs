//! Error type returned by fallible SDK operations.
//!
//! Note what is deliberately absent: a `QrParseException`-style variant. No sibling SDK ever
//! raises one — `QrParser` degrades gracefully (returns `None`/empty fields) rather than
//! erroring, and this port preserves that: `QrParser::new` is infallible.
//!
//! Also note the behavioral contract enforced by `v2::service`: decryption failures during a
//! lookup (wrong key, mismatched destination) are always swallowed internally and never surface
//! as a `BrantaError` — only `add_payment`'s HTTP call and the logo-URL domain check are allowed
//! to bubble an error out of `BrantaService`.

use thiserror::Error;

use crate::enums::DestinationType;

#[derive(Debug, Error)]
pub enum BrantaError {
    #[error("PrivacyMode::Strict does not permit plain-text lookups for this destination type")]
    PrivacyModeViolation,

    #[error(
        "PrivacyMode::Strict requires all destinations to be ZK; one or more destinations have is_zk = false"
    )]
    NonZkDestinationInStrictMode,

    #[error("Unauthorized")]
    Unauthorized,

    #[error("request failed with status {status}")]
    RequestFailed { status: u16 },

    #[error("No payment returned from server")]
    NoPaymentReturned,

    #[error("destination type {0:?} does not support ZK")]
    UnsupportedZkDestinationType(Option<DestinationType>),

    #[error("platform_logo_url domain does not match the configured base_url domain")]
    LogoUrlDomainMismatch,

    #[error("invalid encrypted data: too short")]
    EncryptedDataTooShort,

    #[error("encryption failed: {0}")]
    EncryptionFailed(String),

    #[error("decryption failed: {0}")]
    DecryptionFailed(String),

    #[error("Payment has no destinations")]
    NoDestinations,

    #[error(
        "The Bitcoin address in the QR code does not match the address verified by Branta. The QR code may have been tampered with."
    )]
    Tampered,

    #[error("HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
