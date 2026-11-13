//! Rust SDK for the Branta V2 API.
//!
//! Use [`BrantaService::verify`] on send and [`BrantaService::add_payment`] on receive.
//! Status handling and parent platforms are in the [developer docs](https://developer.branta.pro).

mod enums;
mod error;
mod extensions;
mod models;
mod options;
mod v2;

pub use enums::{BrantaEnvironment, DestinationType, VerifyStatus};
pub use error::BrantaError;
pub use models::{AddPaymentResult, Destination, Payment, Platform, VerifyResult};
pub use options::BrantaApiOptions;
pub use v2::{BrantaService, PaymentBuilder};
