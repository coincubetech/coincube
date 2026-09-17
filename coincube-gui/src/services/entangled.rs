//! Entangled-deposit detection for a Bitcoin Blake2b Cube (`#276` I13).
//!
//! A BTCB2 coin whose funding transaction also exists on Bitcoin — a pre-fork
//! UTXO, or a deposit that was replayed across both chains — is *entangled*:
//! a legacy-signed spend of it is valid on both chains, so spending it on
//! BTCB2 without a replay-capable signature (or a poison split first) also
//! moves the Bitcoin twin. Detection is one lookup per deposit txid against
//! Connect's Esplora proxy for the **twin** chain (`bitcoin/mainnet` for
//! `BitcoinBlake2b`, `bitcoin/testnet4` for its testnet), authenticated with
//! the Connect session. Never a public explorer (`#276` correction 3).
//!
//! The answer is tri-state on purpose. Only a `200` with the same txid in the
//! body says *entangled*; only a `404` says *not entangled*; everything else
//! — no session, transport failure, timeout, a 5xx, an unexpected body — is
//! [`Entanglement::Unknown`], which the caller must treat as "not yet
//! checked", never as "safe". Unknown results are not cached, so the next sync
//! retries them.
//!
//! # Cache lifecycle
//!
//! - *Entangled* is terminal: a transaction confirmed on the twin chain does
//!   not un-exist, so it is never re-queried and never overwritten.
//! - *Not entangled* has a shelf life: anyone holding the funding transaction
//!   can broadcast it onto Bitcoin after our `404`, so a negative older than
//!   [`NEGATIVE_ANSWER_TTL`] is re-queried by the sync-driven task, and the
//!   spend screen re-checks the inputs of a replayable spend at the moment it
//!   matters ([`crate::app::state::vault::psbt::PsbtState`]).
//! - *Unknown* is never cached.

use std::time::{Duration, Instant};

use coincube_core::miniscript::bitcoin::Txid;
use serde::Deserialize;

use crate::{chain::ChainId, services::coincube::CoincubeClient};

/// Whether a deposit transaction also exists on the twin chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entanglement {
    /// The twin chain has a transaction with this txid: the coin exists on
    /// both chains.
    Entangled,
    /// The twin chain has no such transaction (a definite `404`).
    NotEntangled,
    /// The lookup could not answer. Treat as unchecked; retry later.
    Unknown,
}

impl Entanglement {
    /// Whether the lookup produced an answer worth caching.
    pub fn is_resolved(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

/// How long a *not entangled* answer is trusted before the sync-driven task
/// asks again. One `GET` per deposit per hour through a proxy that already
/// serves Bitcoin Cubes.
pub const NEGATIVE_ANSWER_TTL: Duration = Duration::from_secs(60 * 60);

/// A resolved answer with the instant it was resolved at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedEntanglement {
    pub answer: Entanglement,
    pub resolved_at: Instant,
}

impl CachedEntanglement {
    /// Whether the sync-driven task should ask again: only a negative, and
    /// only once it is older than [`NEGATIVE_ANSWER_TTL`]. A positive answer
    /// is terminal.
    pub fn needs_refresh(&self, now: Instant) -> bool {
        matches!(self.answer, Entanglement::NotEntangled)
            && now.saturating_duration_since(self.resolved_at) >= NEGATIVE_ANSWER_TTL
    }
}

/// The Bitcoin-family chain a Bitcoin Blake2b chain forked from, on which a
/// BTCB2 deposit may have a twin. `None` for every non-BTCB2 chain: there is
/// nothing to look up.
pub fn twin_chain(chain: ChainId) -> Option<ChainId> {
    match chain {
        ChainId::BitcoinBlake2b => Some(ChainId::Bitcoin),
        ChainId::BitcoinBlake2bTestnet4 => Some(ChainId::Testnet4),
        ChainId::Bitcoin
        | ChainId::Testnet
        | ChainId::Testnet4
        | ChainId::Signet
        | ChainId::Regtest => None,
    }
}

/// Which deposits the sync-driven task should ask about now: every txid the
/// cache has no answer for or holds a stale negative for, minus the ones a
/// batch already claimed. Sorted and deduplicated.
pub fn pending_lookups(
    deposits: impl IntoIterator<Item = Txid>,
    cache: &std::collections::HashMap<Txid, CachedEntanglement>,
    in_flight: &std::collections::HashSet<Txid>,
    now: Instant,
) -> Vec<Txid> {
    let mut pending: Vec<Txid> = deposits
        .into_iter()
        .filter(|txid| !in_flight.contains(txid))
        .filter(|txid| {
            cache
                .get(txid)
                .is_none_or(|cached| cached.needs_refresh(now))
        })
        .collect();
    pending.sort();
    pending.dedup();
    pending
}

/// `GET {base}/api/v1/esplora/<twin>/tx/{txid}` — Esplora's transaction
/// lookup through Connect's proxy for the twin chain.
pub fn tx_lookup_url(base_url: &str, twin: ChainId, txid: &Txid) -> String {
    format!(
        "{}/api/v1/esplora/{}/tx/{}",
        base_url.trim_end_matches('/'),
        crate::installer::connect_esplora_path(twin),
        txid
    )
}

#[derive(Deserialize)]
struct EsploraTx {
    txid: String,
}

/// Look one deposit up on the twin chain of `chain`. `client` must already
/// carry the Connect session token; an unauthenticated client gets a `401`
/// and therefore [`Entanglement::Unknown`], which is the right answer.
pub async fn lookup(client: &CoincubeClient, chain: ChainId, txid: Txid) -> Entanglement {
    let Some(twin) = twin_chain(chain) else {
        return Entanglement::Unknown;
    };
    let url = tx_lookup_url(&client.base_url, twin, &txid);
    let response = match client.client.get(&url).send().await {
        Ok(response) => response,
        Err(e) => {
            tracing::warn!(
                target: "coincube_gui::entangled",
                "entangled lookup for {} failed: {}",
                txid,
                e
            );
            return Entanglement::Unknown;
        }
    };
    match response.status().as_u16() {
        200 => match response.json::<EsploraTx>().await {
            Ok(tx) if tx.txid == txid.to_string() => Entanglement::Entangled,
            Ok(tx) => {
                tracing::warn!(
                    target: "coincube_gui::entangled",
                    "entangled lookup for {} answered a different txid {}",
                    txid,
                    tx.txid
                );
                Entanglement::Unknown
            }
            Err(e) => {
                tracing::warn!(
                    target: "coincube_gui::entangled",
                    "entangled lookup for {} returned an unreadable body: {}",
                    txid,
                    e
                );
                Entanglement::Unknown
            }
        },
        404 => Entanglement::NotEntangled,
        status => {
            tracing::warn!(
                target: "coincube_gui::entangled",
                "entangled lookup for {} answered HTTP {}",
                txid,
                status
            );
            Entanglement::Unknown
        }
    }
}

/// Look every txid up in turn and return **every** answer, `Unknown` ones
/// included: the caller caches the resolved ones and needs the unresolved
/// ones too — to release its in-flight claim on them, and to say which inputs
/// it could not check.
pub async fn lookup_all(
    client: CoincubeClient,
    chain: ChainId,
    txids: Vec<Txid>,
) -> Vec<(Txid, Entanglement)> {
    let mut answers = Vec::with_capacity(txids.len());
    for txid in txids {
        let answer = lookup(&client, chain, txid).await;
        answers.push((txid, answer));
    }
    answers
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::{Method, MockServer};
    use std::str::FromStr;

    const TXID: &str = "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b";

    fn txid() -> Txid {
        Txid::from_str(TXID).unwrap()
    }

    #[test]
    fn only_blake2b_chains_have_a_twin() {
        assert_eq!(twin_chain(ChainId::BitcoinBlake2b), Some(ChainId::Bitcoin));
        assert_eq!(
            twin_chain(ChainId::BitcoinBlake2bTestnet4),
            Some(ChainId::Testnet4)
        );
        for chain in [
            ChainId::Bitcoin,
            ChainId::Testnet,
            ChainId::Testnet4,
            ChainId::Signet,
            ChainId::Regtest,
        ] {
            assert_eq!(twin_chain(chain), None, "{:?}", chain);
        }
    }

    #[test]
    fn lookup_url_targets_the_twin_chain_proxy_never_a_public_explorer() {
        let url = tx_lookup_url("https://api.example/", ChainId::Bitcoin, &txid());
        assert_eq!(
            url,
            format!("https://api.example/api/v1/esplora/bitcoin/mainnet/tx/{TXID}")
        );
        let url = tx_lookup_url("https://api.example", ChainId::Testnet4, &txid());
        assert_eq!(
            url,
            format!("https://api.example/api/v1/esplora/bitcoin/testnet4/tx/{TXID}")
        );
        assert!(!url.contains("mempool.space") && !url.contains("blockstream"));
    }

    #[tokio::test]
    async fn a_twin_transaction_is_entangled() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(Method::GET)
                .path(format!("/api/v1/esplora/bitcoin/mainnet/tx/{TXID}"));
            then.status(200)
                .header("content-type", "application/json")
                .body(format!(r#"{{"txid":"{TXID}","version":2,"locktime":0}}"#));
        });
        let client = CoincubeClient::for_test(server.base_url());
        assert_eq!(
            lookup(&client, ChainId::BitcoinBlake2b, txid()).await,
            Entanglement::Entangled
        );
        mock.assert();
    }

    #[tokio::test]
    async fn a_404_is_definitely_not_entangled() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(Method::GET)
                .path(format!("/api/v1/esplora/bitcoin/mainnet/tx/{TXID}"));
            then.status(404).body("Transaction not found");
        });
        let client = CoincubeClient::for_test(server.base_url());
        assert_eq!(
            lookup(&client, ChainId::BitcoinBlake2b, txid()).await,
            Entanglement::NotEntangled
        );
    }

    #[tokio::test]
    async fn every_failure_is_unknown_never_not_entangled() {
        // 401 (no session), 500, a 200 whose body is not the transaction, a
        // 200 for a different txid, and a dead endpoint all read Unknown.
        for (status, body) in [
            (
                401,
                r#"{"success":false,"error":{"code":"unauthorized","message":"x"}}"#,
            ),
            (500, "boom"),
            (200, "<html>rate limited</html>"),
            (
                200,
                r#"{"txid":"0000000000000000000000000000000000000000000000000000000000000000"}"#,
            ),
        ] {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(Method::GET)
                    .path(format!("/api/v1/esplora/bitcoin/mainnet/tx/{TXID}"));
                then.status(status).body(body);
            });
            let client = CoincubeClient::for_test(server.base_url());
            assert_eq!(
                lookup(&client, ChainId::BitcoinBlake2b, txid()).await,
                Entanglement::Unknown,
                "status {} body {}",
                status,
                body
            );
        }
        // Connection refused.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let client = CoincubeClient::for_test(format!("http://{addr}"));
        assert_eq!(
            lookup(&client, ChainId::BitcoinBlake2b, txid()).await,
            Entanglement::Unknown
        );
        // A non-BTCB2 chain has nothing to look up.
        assert_eq!(
            lookup(&client, ChainId::Bitcoin, txid()).await,
            Entanglement::Unknown
        );
    }

    #[test]
    fn only_a_stale_negative_needs_a_refresh() {
        let now = Instant::now();
        let fresh_no = CachedEntanglement {
            answer: Entanglement::NotEntangled,
            resolved_at: now,
        };
        assert!(!fresh_no.needs_refresh(now));
        assert!(!fresh_no.needs_refresh(now + NEGATIVE_ANSWER_TTL - Duration::from_secs(1)));
        assert!(fresh_no.needs_refresh(now + NEGATIVE_ANSWER_TTL));
        let yes = CachedEntanglement {
            answer: Entanglement::Entangled,
            resolved_at: now,
        };
        assert!(
            !yes.needs_refresh(now + NEGATIVE_ANSWER_TTL * 100),
            "Entangled is terminal"
        );
        // A clock that went backwards does not panic or refresh.
        assert!(!fresh_no.needs_refresh(now - Duration::from_secs(5)));
    }

    #[test]
    fn pending_lookups_skip_fresh_negatives_positives_and_claimed_txids() {
        use std::collections::{HashMap, HashSet};
        let now = Instant::now();
        let t = |b: u8| Txid::from_str(&format!("{:0>64}", b)).unwrap();
        let mut cache = HashMap::new();
        cache.insert(
            t(1),
            CachedEntanglement {
                answer: Entanglement::NotEntangled,
                resolved_at: now,
            },
        );
        cache.insert(
            t(2),
            CachedEntanglement {
                answer: Entanglement::NotEntangled,
                resolved_at: now - NEGATIVE_ANSWER_TTL,
            },
        );
        cache.insert(
            t(3),
            CachedEntanglement {
                answer: Entanglement::Entangled,
                resolved_at: now - NEGATIVE_ANSWER_TTL * 10,
            },
        );
        let in_flight: HashSet<Txid> = HashSet::from([t(4)]);
        let pending = pending_lookups(
            [t(5), t(4), t(3), t(2), t(1), t(5)],
            &cache,
            &in_flight,
            now,
        );
        // 5: never asked; 2: stale negative. 1 fresh, 3 terminal, 4 claimed.
        assert_eq!(pending, vec![t(2), t(5)]);
    }

    #[tokio::test]
    async fn lookup_all_returns_every_answer_including_unknown() {
        let other =
            Txid::from_str("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(Method::GET)
                .path(format!("/api/v1/esplora/bitcoin/mainnet/tx/{TXID}"));
            then.status(200).body(format!(r#"{{"txid":"{TXID}"}}"#));
        });
        server.mock(|when, then| {
            when.method(Method::GET)
                .path(format!("/api/v1/esplora/bitcoin/mainnet/tx/{other}"));
            then.status(503).body("unavailable");
        });
        let client = CoincubeClient::for_test(server.base_url());
        let answers = lookup_all(client, ChainId::BitcoinBlake2b, vec![txid(), other]).await;
        assert_eq!(
            answers,
            vec![
                (txid(), Entanglement::Entangled),
                (other, Entanglement::Unknown)
            ]
        );
    }
}
