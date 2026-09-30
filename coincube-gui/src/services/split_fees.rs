//! Fee rates for the two Split steps, each from its own chain's Connect
//! Esplora fee estimates, failing closed (#568 owner decisions D4 and P3).
//!
//! - Step 2 (BTCB2 sweep): [`ConnectBtcb2Fees`], the six-block estimate that
//!   Claim already uses (`HttpObservationSource::claim_fee_rate`).
//! - Step 1 (Bitcoin poison self-transfer): [`ConnectBitcoinFees`], the
//!   six-block estimate of Connect's Bitcoin Esplora.
//!
//! Each source is tagged with its chain and the resolvers below never consult
//! a source for another chain, so a Bitcoin estimate cannot price a BTCB2
//! sweep or the reverse. Any failure, including a stale (not fresh) response,
//! a missing six-block quote or a rate outside `1..=MAX_FEERATE`, is "no fee":
//! the caller refuses rather than guess. Nothing here is a local fallback or
//! the mainnet `FeeEstimator`.

use std::sync::Arc;

use coincube_core::{chain::ChainId, spend};
use tokio::sync::watch;

use super::{
    claim_observation::{http::HttpObservationSource, CollectionContext},
    coincube::CoincubeClient,
    foreign_psbt::{SweepFeeSource, UnavailableBtcb2Fees},
    split_evidence::ConnectEsplora,
};

/// Step-2 fee source: Connect's BTCB2 Esplora six-block estimate.
pub struct ConnectBtcb2Fees {
    source: HttpObservationSource,
    /// Owns the generation the source is bound to; dropping it cancels.
    _generation: watch::Sender<u64>,
}

impl ConnectBtcb2Fees {
    /// `None` when the client cannot make a Connect read (no account session
    /// or an unusable base URL).
    pub fn new(client: CoincubeClient) -> Option<Self> {
        let (sender, generation) = watch::channel(0);
        let source = HttpObservationSource::new(
            client,
            ChainId::Bitcoin,
            ChainId::BitcoinBlake2b,
            CollectionContext {
                expected_generation: 0,
                generation,
            },
        )
        .ok()?;
        Some(Self {
            source,
            _generation: sender,
        })
    }
}

#[async_trait::async_trait]
impl SweepFeeSource for ConnectBtcb2Fees {
    fn chain(&self) -> ChainId {
        ChainId::BitcoinBlake2b
    }
    async fn mid_priority_sat_vb(&self) -> Option<u64> {
        self.source.claim_fee_rate().await.ok()
    }
}

/// Step-1 fee source: Connect's Bitcoin Esplora six-block estimate.
pub struct ConnectBitcoinFees {
    esplora: ConnectEsplora,
    _generation: watch::Sender<u64>,
}

impl ConnectBitcoinFees {
    pub fn new(client: &CoincubeClient) -> Option<Self> {
        if client.token().is_none_or(|token| token.trim().is_empty()) {
            return None;
        }
        let (sender, generation) = watch::channel(0);
        let esplora = ConnectEsplora::new(
            client,
            CollectionContext {
                expected_generation: 0,
                generation,
            },
        )
        .ok()?;
        Some(Self {
            esplora,
            _generation: sender,
        })
    }
}

#[async_trait::async_trait]
impl SweepFeeSource for ConnectBitcoinFees {
    fn chain(&self) -> ChainId {
        ChainId::Bitcoin
    }
    async fn mid_priority_sat_vb(&self) -> Option<u64> {
        self.esplora.fee_rate(ChainId::Bitcoin).await.ok()
    }
}

/// The BTCB2 Split review's fee source: Connect BTCB2 fees for an account
/// session, otherwise unavailable. Never a Bitcoin estimate.
pub fn btcb2_fee_source(client: Option<CoincubeClient>) -> Arc<dyn SweepFeeSource> {
    match client.and_then(ConnectBtcb2Fees::new) {
        Some(source) => Arc::new(source),
        None => Arc::new(UnavailableBtcb2Fees),
    }
}

/// Resolve the step-1 Bitcoin fee rate. A source scoped to any other chain is
/// never queried; an out-of-bounds rate is unavailable, not clamped.
pub async fn bitcoin_step1_feerate(source: &dyn SweepFeeSource) -> Option<u64> {
    if source.chain() != ChainId::Bitcoin {
        return None;
    }
    source
        .mid_priority_sat_vb()
        .await
        .filter(|rate| (1..=spend::MAX_FEERATE).contains(rate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::foreign_psbt::btcb2_sweep_feerate;
    use httpmock::prelude::*;

    fn client(server: &MockServer) -> CoincubeClient {
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("synthetic-fee-token");
        client
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

    fn quote<'a>(
        server: &'a MockServer,
        network: &str,
        body: &str,
        fresh: bool,
    ) -> httpmock::Mock<'a> {
        server.mock(|when, then| {
            when.method(GET)
                .path(format!("/api/v1/esplora/{network}/mainnet/fee-estimates"))
                .header("x-coincube-observation", "fresh")
                .matches(anonymous);
            let then = then.status(200).body(body);
            if fresh {
                then.header("X-Coincube-Observation", "fresh")
                    .header("X-Cache", "BYPASS")
                    .header("Cache-Control", "no-store");
            }
        })
    }

    /// D4: the step-2 review is priced by Connect BTCB2 fees, from the BTCB2
    /// route only, and refuses instead of guessing.
    #[tokio::test]
    async fn split_btcb2_fees_come_from_connect_btcb2_and_fail_closed() {
        for (body, fresh, expected) in [
            (r#"{"6":2.2,"1":9}"#, true, Some(3)),
            (r#"{"6":0.5}"#, true, Some(1)),
            (r#"{"1":9}"#, true, None),
            (r#"{"6":0}"#, true, None),
            (r#"{"6":"4"}"#, true, None),
            (r#"{"6":1e30}"#, true, None),
            (r#"{"6":2}"#, false, None),
        ] {
            let server = MockServer::start();
            let refusal = crate::services::split_test_connect::strict(&server);
            let btcb2 = quote(&server, "bitcoin-blake2b", body, fresh);
            let bitcoin = quote(&server, "bitcoin", r#"{"6":7}"#, true);
            let source = btcb2_fee_source(Some(client(&server)));
            assert_eq!(source.chain(), ChainId::BitcoinBlake2b);
            assert_eq!(btcb2_sweep_feerate(&*source).await, expected, "{body}");
            btcb2.assert_hits(1);
            bitcoin.assert_hits(0);
            refusal.assert_hits(0);
            // Never a step-1 price.
            assert_eq!(bitcoin_step1_feerate(&*source).await, None);
        }
        // Above the ceiling is unavailable, not clamped.
        let server = MockServer::start();
        let over = spend::MAX_FEERATE + 1;
        quote(
            &server,
            "bitcoin-blake2b",
            &format!(r#"{{"6":{over}}}"#),
            true,
        );
        assert_eq!(
            btcb2_sweep_feerate(&*btcb2_fee_source(Some(client(&server)))).await,
            None
        );
        // No session, or an unreachable Connect: unavailable.
        assert_eq!(btcb2_sweep_feerate(&*btcb2_fee_source(None)).await, None);
        let unreachable = CoincubeClient::for_test("http://127.0.0.1:1".to_owned());
        assert_eq!(
            btcb2_sweep_feerate(&*btcb2_fee_source(Some(unreachable))).await,
            None
        );
    }

    /// P3: step 1 is priced by Connect Bitcoin fees only, failing closed.
    #[tokio::test]
    async fn split_bitcoin_step1_fees_come_from_connect_bitcoin_and_fail_closed() {
        for (body, fresh, expected) in [
            (r#"{"6":4.01}"#, true, Some(5)),
            (r#"{"2":9}"#, true, None),
            (r#"{"6":-3}"#, true, None),
            (r#"not json"#, true, None),
            (r#"{"6":4}"#, false, None),
        ] {
            let server = MockServer::start();
            let refusal = crate::services::split_test_connect::strict(&server);
            let bitcoin = quote(&server, "bitcoin", body, fresh);
            let btcb2 = quote(&server, "bitcoin-blake2b", r#"{"6":7}"#, true);
            let source = ConnectBitcoinFees::new(&client(&server)).unwrap();
            assert_eq!(bitcoin_step1_feerate(&source).await, expected, "{body}");
            bitcoin.assert_hits(1);
            btcb2.assert_hits(0);
            refusal.assert_hits(0);
            assert_eq!(btcb2_sweep_feerate(&source).await, None);
        }
        let server = MockServer::start();
        let over = spend::MAX_FEERATE + 1;
        quote(&server, "bitcoin", &format!(r#"{{"6":{over}}}"#), true);
        let source = ConnectBitcoinFees::new(&client(&server)).unwrap();
        assert_eq!(bitcoin_step1_feerate(&source).await, None);
        // Signed out: no source at all.
        assert!(ConnectBitcoinFees::new(&CoincubeClient::for_test(server.base_url())).is_none());
    }
}
