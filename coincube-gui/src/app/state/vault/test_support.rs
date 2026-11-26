//! Shared fixtures for vault test modules.

use std::sync::Arc;

use coincube_core::miniscript::bitcoin::Psbt;
use tokio::sync::RwLock;

use crate::services::connect::client::auth::AccessTokenResponse;

/// An empty (no inputs/outputs) unsigned PSBT, suitable as a placeholder in tests.
pub fn empty_psbt() -> Psbt {
    use coincube_core::miniscript::bitcoin::{absolute, transaction, Transaction};

    Psbt::from_unsigned_tx(Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::Blocks(absolute::Height::ZERO),
        input: vec![],
        output: vec![],
    })
    .expect("empty unsigned tx is psbt-compatible")
}

/// A shared, never-expiring set of Connect access tokens for tests.
pub fn tokens() -> Arc<RwLock<AccessTokenResponse>> {
    Arc::new(RwLock::new(AccessTokenResponse {
        access_token: "access".to_string(),
        refresh_token: "refresh".to_string(),
        expires_at: i64::MAX,
    }))
}

/// A Bitcoin Blake2b-shaped Vault fixture for the replay model: a 2-of-3
/// primary path over three deterministic hot signers plus a single-key
/// recovery leaf (`older(46)`, signer 2's second key), and an unsigned
/// one-input PSBT spending a coin of that Vault with an authenticated prevout
/// and its witness script. The same shape `coincubed`'s chain-keyed spend
/// tests use, so the desktop and the daemon are exercised on one fixture.
#[path = "test_support_unified.rs"]
pub mod unified;
