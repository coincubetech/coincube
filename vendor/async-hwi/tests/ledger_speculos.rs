//! Run the in-tree `LedgerSimulator` transport against the Ledger Bitcoin
//! app under Speculos.
//!
//! These tests require Speculos to be running and listening on its default
//! APDU port (127.0.0.1:9999) with the Ledger Bitcoin app loaded. They
//! are marked `#[ignore]` so plain `cargo test` does not pick them up;
//! run them with:
//!
//!     speculos --display headless --apdu-port 9999 path/to/app.elf &
//!     cargo test --test ledger_speculos -- --ignored --test-threads=1
//!
//! `--test-threads=1` is required because Speculos exposes a single APDU
//! socket; running multiple connections in parallel interleaves request
//! and response frames.
//!
//! The fingerprint values asserted here are those produced by Speculos's
//! built-in default seed, which Ledger has used since the project began
//! (BIP39 mnemonic "glory promote mansion idle axis finger extra
//! february uncover one trip resource lawn turtle enact monster seven
//! myth punch hobby comfort wild raise skin").

#![cfg(feature = "ledger")]

use async_hwi::ledger::LedgerSimulator;
use async_hwi::HWI;
use bitcoin::bip32::DerivationPath;
use std::str::FromStr;

/// Master fingerprint produced by Speculos's default seed.
const SPECULOS_DEFAULT_FINGERPRINT: &str = "f5acc2fd";

#[tokio::test]
#[ignore = "requires Speculos running on 127.0.0.1:9999"]
async fn speculos_master_fingerprint() {
    let device = LedgerSimulator::try_connect()
        .await
        .expect("connect to Speculos APDU port");
    let fp = device
        .get_master_fingerprint()
        .await
        .expect("get_master_fingerprint");
    assert_eq!(format!("{fp:x}"), SPECULOS_DEFAULT_FINGERPRINT);
}

#[tokio::test]
#[ignore = "requires Speculos running on 127.0.0.1:9999"]
async fn speculos_get_extended_pubkey_bip84_testnet() {
    let device = LedgerSimulator::try_connect()
        .await
        .expect("connect to Speculos APDU port");
    let path = DerivationPath::from_str("m/84h/1h/0h").unwrap();
    let xpub = device
        .get_extended_pubkey(&path)
        .await
        .expect("get_extended_pubkey");
    // Speculos default seed always derives the same xpub at this path.
    assert_eq!(
        xpub.to_string(),
        "tpubDCtKfsNyRhULjZ9XMS4VKKtVcPdVDi8MKUbcSD9MJDyjRu1A2ND5MiipozyyspBT9bg8upEp7a8EAgFxNxXn1d7QkdbL52Ty5jiSLcxPt1P"
    );
}
