//! Rust SDK for the Branta V2 API.
//!
//! See the crate README for an integration guide and quick-start examples.

pub mod enums;
pub mod error;
pub mod extensions;
pub mod models;
pub mod options;
pub mod v2;

pub use enums::{BrantaServerBaseUrl, DestinationType, PrivacyMode};
pub use error::BrantaError;
pub use models::{AddPaymentResult, Destination, Payment, PaymentsResult, Platform};
pub use options::BrantaClientOptions;
pub use v2::{
    AesEncryptionTrait, BrantaClient, BrantaClientTrait, BrantaService, GuidSecretGenerator,
    PaymentBuilder, QrDestination, QrParser, SecretGeneratorTrait,
};
