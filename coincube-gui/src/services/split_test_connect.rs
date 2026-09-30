//! Test-only strict model of Connect's Esplora proxy for fresh observations.
//!
//! Connect serves a request carrying `X-Coincube-Observation: fresh` only for
//! the paths its allowlist admits and answers every other fresh path with
//! `400 Unsupported Esplora observation path`. This ports that allowlist from
//! coincube-api `main` at `a2c5c02e4509dc0faef55c68827197c5c3ab5da0`,
//! `services/core/esplora/client/observation.go` (`IsFreshObservationPath`,
//! `observationHashPath`, `observationAddress`, `validObservationAddress`) and
//! `services/core/esplora/handlers/proxy.go` (`ErrInvalidObservation` → 400).
//! Keep it in step with that file: a test double more permissive than Connect
//! let #615's first reader pass here and fail in production.

use std::str::FromStr;

use coincube_core::miniscript::bitcoin::{Address, Network};
use httpmock::prelude::*;

const PREFIXES: [&str; 2] = [
    "/api/v1/esplora/bitcoin/mainnet",
    "/api/v1/esplora/bitcoin-blake2b/mainnet",
];

fn hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `validObservationAddress`: decodes for the network and re-encodes to the
/// same text (mainnet or testnet encodings, which BTCB2 shares).
fn valid_address(value: &str) -> bool {
    Address::from_str(value).is_ok_and(|address| {
        [Network::Bitcoin, Network::Testnet].iter().any(|network| {
            address
                .clone()
                .require_network(*network)
                .is_ok_and(|checked| checked.to_string() == value)
        })
    })
}

/// `IsFreshObservationPath`, for a network-relative path such as
/// `/fee-estimates`.
pub fn fresh_path_allowed(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    if (parts.len() == 3 || parts.len() == 4)
        && parts[0].is_empty()
        && parts[1] == "address"
        && (parts.len() == 3 || parts[3] == "utxo")
        && !parts[2].is_empty()
    {
        return valid_address(parts[2]);
    }
    if matches!(
        path,
        "/fee-estimates" | "/blocks/tip/hash" | "/blocks/tip/height"
    ) {
        return true;
    }
    // ^/(tx/[0-9a-fA-F]{64}(/status)?|block/[0-9a-fA-F]{64}/(status|txid/0))$
    match parts.as_slice() {
        ["", "tx", hash] | ["", "tx", hash, "status"] if hex64(hash) => return true,
        ["", "block", hash, "status"] | ["", "block", hash, "txid", "0"] if hex64(hash) => {
            return true
        }
        _ => {}
    }
    path.strip_prefix("/block-height/").is_some_and(|value| {
        !value.is_empty()
            && value.bytes().all(|b| b.is_ascii_digit())
            && value.parse::<u32>().is_ok()
    })
}

fn fresh_header(request: &HttpMockRequest) -> bool {
    request.headers.as_ref().is_some_and(|headers| {
        headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("x-coincube-observation"))
    })
}

fn relative(request: &HttpMockRequest) -> Option<&str> {
    PREFIXES
        .iter()
        .find_map(|prefix| request.path.strip_prefix(prefix))
}

fn refused_fresh(request: &HttpMockRequest) -> bool {
    fresh_header(request) && relative(request).is_none_or(|path| !fresh_path_allowed(path))
}

fn anonymous(request: &HttpMockRequest) -> bool {
    request.headers.as_ref().is_none_or(|headers| {
        headers.iter().all(|(name, _)| {
            ![
                "authorization",
                "cookie",
                "x-device-fingerprint",
                "x-device-name",
            ]
            .iter()
            .any(|bad| name.eq_ignore_ascii_case(bad))
        })
    })
}

fn not_fresh(request: &HttpMockRequest) -> bool {
    !fresh_header(request)
}

/// Install Connect's refusal: every fresh request for a path off the
/// allowlist gets 400. Returns the mock so a test can assert it was never hit.
pub fn strict(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.matches(refused_fresh);
        then.status(400)
            .body("Unsupported Esplora observation path");
    })
}

/// Serve a fresh observation of `path` (network-relative) on `network`
/// (`bitcoin` or `bitcoin-blake2b`). Panics if Connect would refuse the path,
/// so a test cannot serve what production cannot.
pub fn serve_fresh<'a>(
    server: &'a MockServer,
    network: &str,
    path: &str,
    body: &str,
) -> httpmock::Mock<'a> {
    assert!(fresh_path_allowed(path), "Connect refuses fresh {}", path);
    server.mock(|when, then| {
        when.method(GET)
            .path(format!("/api/v1/esplora/{network}/mainnet{path}"))
            .header("x-coincube-observation", "fresh")
            .matches(anonymous);
        then.status(200)
            .header("X-Coincube-Observation", "fresh")
            .header("X-Cache", "BYPASS")
            .header("Cache-Control", "no-store")
            .body(body);
    })
}

/// Serve an ordinary (cacheable, not fresh) read.
pub fn serve_cached<'a>(
    server: &'a MockServer,
    network: &str,
    path: &str,
    body: &str,
) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.method(GET)
            .path(format!("/api/v1/esplora/{network}/mainnet{path}"))
            .matches(not_fresh)
            .matches(anonymous);
        then.status(200).body(body);
    })
}

#[test]
fn split_connect_fresh_allowlist_port_matches_coincube_api() {
    let hash = "ab".repeat(32);
    for allowed in [
        "/fee-estimates".to_string(),
        "/blocks/tip/hash".into(),
        "/blocks/tip/height".into(),
        "/block-height/0".into(),
        "/block-height/4294967295".into(),
        format!("/tx/{hash}"),
        format!("/tx/{hash}/status"),
        format!("/block/{hash}/status"),
        format!("/block/{hash}/txid/0"),
        "/address/bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".into(),
        "/address/bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4/utxo".into(),
        "/address/1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2/utxo".into(),
    ] {
        assert!(fresh_path_allowed(&allowed), "{}", allowed);
    }
    for refused in [
        format!("/tx/{hash}/hex"),
        format!("/tx/{hash}/raw"),
        format!("/tx/{hash}/outspend/0"),
        format!("/tx/{hash}/outspends"),
        format!("/tx/{}", &hash[..63]),
        format!("/block/{hash}"),
        "/block-height/4294967296".into(),
        "/block-height/-1".into(),
        "/block-height/".into(),
        "/address/bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4/txs".into(),
        "/address/BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4/utxo".into(),
        "/address/notanaddress/utxo".into(),
        "/scripthash/00/utxo".into(),
        "/mempool".into(),
    ] {
        assert!(!fresh_path_allowed(&refused), "{}", refused);
    }
}
