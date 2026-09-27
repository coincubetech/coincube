//! Bounded network discovery, retaining raw evidence without authorizing a spend.
use super::*;
use coincube_core::{
    claim_ancestry::{
        self,
        search::{OwnedLink, Search, Step},
    },
    miniscript::bitcoin::OutPoint,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

const MAX_REQUESTS: usize = 128;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct CollectionBudget {
    requests: AtomicUsize,
    bytes: AtomicUsize,
}
impl CollectionBudget {
    fn new() -> Self {
        Self {
            requests: AtomicUsize::new(MAX_REQUESTS),
            bytes: AtomicUsize::new(MAX_RESPONSE_BYTES),
        }
    }
    pub(super) fn charge_request(&self, reserved_bytes: usize) -> Result<(), FailureKind> {
        self.requests
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .map_err(|_| FailureKind::CollectionLimit)?;
        self.charge_bytes(reserved_bytes)
    }
    pub(super) fn charge_bytes(&self, bytes: usize) -> Result<(), FailureKind> {
        self.bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                n.checked_sub(bytes)
            })
            .map_err(|_| FailureKind::CollectionLimit)?;
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryError {
    Structural(claim_ancestry::Error),
    Observation(FailureKind),
}
#[derive(Debug)]
pub struct DiscoveredAncestry {
    links: Vec<OwnedLink>,
    pair: CoinbasePair,
    observed_at: i64,
}
impl DiscoveredAncestry {
    pub fn links(&self) -> &[OwnedLink] {
        &self.links
    }
    pub fn pair(&self) -> &CoinbasePair {
        &self.pair
    }
    pub fn observed_at(&self) -> i64 {
        self.observed_at
    }
    pub fn generation(&self) -> u64 {
        self.pair.generation()
    }
}
impl HttpObservationSource {
    fn discovery_snapshot(&self) -> Self {
        Self {
            authenticated: self.authenticated.clone(),
            anonymous: self.anonymous.clone(),
            base: self.base.clone(),
            generation: self.generation.clone(),
            expected: self.expected,
            budget: Some(
                self.budget
                    .clone()
                    .unwrap_or_else(|| Arc::new(CollectionBudget::new())),
            ),
        }
    }
    /// Discover one qualifying mainnet dependency with shared graph, response,
    /// request and 30s time limits. None means no qualifying candidate was found,
    /// never proof of replay safety. Persisted links require re-verification;
    /// ownership, maturity, spend policy and final fresh preflight remain separate.
    pub async fn discover_ancestry(
        &self,
        selected: OutPoint,
        policy: Policy,
    ) -> Result<Option<DiscoveredAncestry>, DiscoveryError> {
        if policy.max_observation_age_seconds <= 0 || policy.expiry_margin_seconds <= 0 {
            return Err(DiscoveryError::Observation(FailureKind::InvalidPlan));
        }
        let mut search = Search::new(selected).map_err(DiscoveryError::Structural)?;
        let snapshot = self.discovery_snapshot();
        let observation = DiscoveryError::Observation;
        let result = tokio::time::timeout(MAX_COLLECTION_TIME, async {
            loop {
                match search.step().map_err(DiscoveryError::Structural)? {
                    Step::Fetch { txid, max_bytes } => {
                        let raw = snapshot
                            .ancestry_transaction_limited(ChainId::Bitcoin, txid, max_bytes)
                            .await
                            .map_err(observation)?;
                        search.provide(raw).map_err(DiscoveryError::Structural)?;
                    }
                    Step::Candidate(candidate) => {
                        let (root, stamp) = super::super::read(
                            snapshot
                                .transaction(ChainId::Bitcoin, candidate.dependency.root().txid)
                                .await
                                .map_err(observation)?,
                            ChainId::Bitcoin,
                            &snapshot,
                            policy,
                            Stage::BitcoinInclusion,
                        )
                        .map_err(|error| observation(error.kind))?;
                        let TransactionObservation::Confirmed { block, .. } = root else {
                            // A coinbase cannot legitimately be a mempool transaction.
                            // Treat absent/inconsistent provider data as unavailable evidence.
                            return Err(observation(FailureKind::Changed));
                        };
                        let height = u32::try_from(block.height)
                            .map_err(|_| observation(FailureKind::Malformed))?;
                        match snapshot
                            .coinbase_pair(&candidate.dependency, height, policy)
                            .await
                        {
                            // Only a positively disqualified shared/out-of-range root can be skipped.
                            Err(FailureKind::UnsupportedPoison) => continue,
                            Err(error) => return Err(observation(error)),
                            Ok(pair) => {
                                if pair.bitcoin().block != block {
                                    return Err(observation(FailureKind::Changed));
                                }
                                let oldest = pair.observed_at().min(stamp);
                                if !super::super::fresh(
                                    oldest,
                                    snapshot.now(),
                                    policy.max_observation_age_seconds,
                                ) {
                                    return Err(observation(FailureKind::Stale));
                                }
                                return Ok(Some(DiscoveredAncestry {
                                    links: candidate.links,
                                    pair,
                                    observed_at: oldest,
                                }));
                            }
                        }
                    }
                    Step::Complete => return Ok(None),
                }
            }
        })
        .await
        .map_err(|_| observation(FailureKind::Deadline))?;
        if *snapshot.generation.borrow() != snapshot.expected
            || snapshot.generation.has_changed().is_err()
        {
            return Err(observation(FailureKind::Cancelled));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{ancestry_fixture, source as make_source};
    use super::*;
    use httpmock::prelude::*;

    fn policy() -> Policy {
        Policy {
            max_observation_age_seconds: 60,
            expiry_margin_seconds: 600,
        }
    }

    #[tokio::test]
    async fn nested_snapshots_share_budget_and_stop_before_an_extra_request() {
        let server = MockServer::start();
        let (source, _sender) = make_source(&server);
        let (txid, raw) = ancestry_fixture();
        let mock = server.mock(|when, then| {
            when.method(GET);
            then.status(200).body(hex::encode(&raw));
        });
        let mut limited = source.discovery_snapshot();
        limited.budget = Some(Arc::new(CollectionBudget {
            requests: AtomicUsize::new(1),
            bytes: AtomicUsize::new(1000),
        }));
        let nested = limited.discovery_snapshot();
        assert_eq!(
            limited
                .ancestry_transaction(ChainId::Bitcoin, txid)
                .await
                .unwrap(),
            raw
        );
        assert_eq!(
            nested.ancestry_transaction(ChainId::Bitcoin, txid).await,
            Err(FailureKind::CollectionLimit)
        );
        assert!(matches!(
            nested
                .discover_ancestry(OutPoint::new(txid, 0), policy())
                .await,
            Err(DiscoveryError::Observation(FailureKind::CollectionLimit))
        ));
        mock.assert_hits(1);
        assert_eq!(
            source
                .ancestry_transaction(ChainId::Bitcoin, txid)
                .await
                .unwrap(),
            raw
        );
        mock.assert_hits(2);
    }
    #[tokio::test]
    async fn response_budget_and_remaining_graph_limit_are_enforced() {
        let server = MockServer::start();
        let (source, _sender) = make_source(&server);
        let (txid, raw) = ancestry_fixture();
        let mock = server.mock(|when, then| {
            when.method(GET);
            then.status(200).body(hex::encode(&raw));
        });
        let mut limited = source.discovery_snapshot();
        limited.budget = Some(Arc::new(CollectionBudget {
            requests: AtomicUsize::new(10),
            bytes: AtomicUsize::new(raw.len() * 2 - 1),
        }));
        assert_eq!(
            limited.ancestry_transaction(ChainId::Bitcoin, txid).await,
            Err(FailureKind::CollectionLimit)
        );
        assert!(matches!(
            limited
                .discover_ancestry(OutPoint::new(txid, 0), policy())
                .await,
            Err(DiscoveryError::Observation(FailureKind::CollectionLimit))
        ));
        assert_eq!(
            source
                .ancestry_transaction_limited(ChainId::Bitcoin, txid, raw.len() - 1)
                .await,
            Err(FailureKind::Malformed)
        );
        mock.assert_hits(3);
        let server = MockServer::start();
        let (source, _sender) = make_source(&server);
        let mock = server.mock(|when, then| {
            when.method(GET);
            then.status(500);
        });
        let mut limited = source.discovery_snapshot();
        limited.budget = Some(Arc::new(CollectionBudget {
            requests: AtomicUsize::new(10),
            bytes: AtomicUsize::new(
                crate::services::coincube::network_anchor::MAX_ANCHOR_BODY_BYTES - 1,
            ),
        }));
        assert!(matches!(
            limited.anchor(ChainId::BitcoinBlake2b).await,
            Err(FailureKind::CollectionLimit)
        ));
        mock.assert_hits(0);
    }
    #[tokio::test]
    async fn failed_root_observation_never_becomes_an_empty_search_or_skipped_branch() {
        use coincube_core::miniscript::bitcoin::{
            consensus::{deserialize, serialize},
            script::Builder,
            Transaction, TxIn,
        };
        use serde_json::json;
        for (status, confirmed, expected) in [
            (503, false, Some(FailureKind::Http(503))),
            (404, false, Some(FailureKind::Changed)),
            (200, false, Some(FailureKind::Changed)),
            (200, true, None),
        ] {
            let server = MockServer::start();
            let (source, _sender) = make_source(&server);
            let (_, raw) = ancestry_fixture();
            let mut root: Transaction = deserialize(&raw).unwrap();
            root.input[0].script_sig = Builder::new().push_int(17).push_int(1).into_script();
            let mut later = root.clone();
            later.input[0].script_sig = Builder::new().push_int(18).push_int(1).into_script();
            let mut selected = root.clone();
            selected.input = vec![
                TxIn {
                    previous_output: OutPoint::new(root.compute_txid(), 0),
                    ..Default::default()
                },
                TxIn {
                    previous_output: OutPoint::new(later.compute_txid(), 0),
                    ..Default::default()
                },
            ];
            for tx in [&selected, &root] {
                server.mock(|when, then| {
                    when.method(GET).path(format!(
                        "/api/v1/esplora/bitcoin/mainnet/tx/{}/hex",
                        tx.compute_txid()
                    ));
                    then.status(200).body(hex::encode(serialize(tx)));
                });
            }
            let later_read = server.mock(|when, then| {
                when.method(GET).path(format!(
                    "/api/v1/esplora/bitcoin/mainnet/tx/{}/hex",
                    later.compute_txid()
                ));
                then.status(200).body(hex::encode(serialize(&later)));
            });
            server.mock(|when, then| {
                when.method(GET).path(format!(
                    "/api/v1/esplora/bitcoin/mainnet/tx/{}",
                    root.compute_txid()
                ));
                then.status(status)
                    .header("x-cache", "BYPASS")
                    .header("cache-control", "no-store")
                    .header("x-coincube-observation", "fresh")
                    .json_body(
                        json!({"txid":root.compute_txid(),"status": if confirmed { json!({"confirmed":true,"block_height":17,"block_hash":"88".repeat(32)}) } else { json!({"confirmed":false}) }}),
                    );
            });
            server.mock(|when, then| {
                when.method(GET).path(format!(
                    "/api/v1/esplora/bitcoin/mainnet/tx/{}", later.compute_txid()
                ));
                then.status(200)
                    .header("x-cache", "BYPASS")
                    .header("cache-control", "no-store")
                    .header("x-coincube-observation", "fresh")
                    .json_body(json!({"txid":later.compute_txid(),"status":{"confirmed":true,"block_height":18,"block_hash":"88".repeat(32)}}));
            });
            let result = source
                .discover_ancestry(
                    OutPoint::new(selected.compute_txid(), 0),
                    Policy {
                        max_observation_age_seconds: 60,
                        expiry_margin_seconds: 600,
                    },
                )
                .await;
            if let Some(expected) = expected {
                assert!(
                    matches!(result,Err(DiscoveryError::Observation(error)) if error == expected)
                );
                later_read.assert_hits(0);
            } else {
                assert!(matches!(result, Ok(None)));
                later_read.assert_hits(1);
            }
        }
    }
}
