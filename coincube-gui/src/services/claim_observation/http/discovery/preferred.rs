//! Search wallet-supplied candidates without multiplying collection budgets.
use super::*;

pub const MAX_ANCESTRY_CANDIDATES: usize = 32;
impl HttpObservationSource {
    /// Discover both chain-specific funding paths and refresh every shared
    /// input as one bounded read. A one-sided result is never returned: the
    /// caller still has to bind the selected outputs to its source wallet,
    /// reserve them, build both transactions and run signer preflight.
    ///
    /// The outer snapshot shares one request/byte budget across both searches
    /// and the final collection, with one wall-clock deadline for the pair.
    pub async fn discover_spend_inputs(
        &self,
        bitcoin_candidates: &[OutPoint],
        fork_candidates: &[OutPoint],
        shared: &[OutPoint],
        policy: Policy,
    ) -> Result<Option<CollectedSpendInputs>, DiscoveryError> {
        let invalid = || DiscoveryError::Observation(FailureKind::InvalidPlan);
        let shared_set: BTreeSet<_> = shared.iter().copied().collect();
        if bitcoin_candidates.len() > MAX_ANCESTRY_CANDIDATES
            || fork_candidates.len() > MAX_ANCESTRY_CANDIDATES
        {
            return Err(DiscoveryError::Observation(FailureKind::CollectionLimit));
        }
        if shared_set.is_empty()
            || shared_set.len() != shared.len()
            || shared_set.iter().any(OutPoint::is_null)
            || policy.max_observation_age_seconds <= 0
            || policy.expiry_margin_seconds <= 0
            || bitcoin_candidates.iter().any(OutPoint::is_null)
            || fork_candidates.iter().any(OutPoint::is_null)
            || bitcoin_candidates.iter().collect::<BTreeSet<_>>().len() != bitcoin_candidates.len()
            || fork_candidates.iter().collect::<BTreeSet<_>>().len() != fork_candidates.len()
            || bitcoin_candidates
                .iter()
                .any(|coin| shared_set.contains(coin))
            || fork_candidates.iter().any(|coin| shared_set.contains(coin))
            || bitcoin_candidates
                .iter()
                .any(|coin| fork_candidates.contains(coin))
        {
            return Err(invalid());
        }
        let source = self.discovery_snapshot();
        tokio::time::timeout(MAX_COLLECTION_TIME, async {
            let Some(bitcoin) = source
                .discover_preferred_ancestry(bitcoin_candidates, policy)
                .await?
            else {
                return Ok(None);
            };
            let Some(fork) = source
                .discover_preferred_fork_ancestry(fork_candidates, policy)
                .await?
            else {
                return Ok(None);
            };
            let bitcoin_path = bitcoin
                .retained_path()
                .map_err(DiscoveryError::Structural)?;
            let fork_path = fork.retained_path().map_err(DiscoveryError::Structural)?;
            let inputs = source
                .collect_spend_inputs(&bitcoin_path, &fork_path, shared, policy)
                .await
                .map_err(DiscoveryError::Observation)?;
            Ok(Some(inputs))
        })
        .await
        .map_err(|_| DiscoveryError::Observation(FailureKind::Deadline))?
    }

    /// Search in caller preference order. The caller supplies owned, confirmed,
    /// mature, unspent candidates disjoint from the shared Claim inputs; those
    /// conditions must be checked again during construction and preflight.
    ///
    /// None means no candidate qualified, not proof that any coin is replay-safe.
    /// A transient error or exhausted bound aborts rather than masquerading as
    /// an empty search.
    pub async fn discover_preferred_ancestry(
        &self,
        candidates: &[OutPoint],
        policy: Policy,
    ) -> Result<Option<DiscoveredAncestry>, DiscoveryError> {
        self.discover_preferred_ancestry_for(candidates, policy, ChainId::Bitcoin)
            .await
    }

    /// Search caller-supplied BTCB2 candidates under the same bounded budget.
    /// The caller must separately verify ownership and current spendability.
    pub async fn discover_preferred_fork_ancestry(
        &self,
        candidates: &[OutPoint],
        policy: Policy,
    ) -> Result<Option<DiscoveredAncestry>, DiscoveryError> {
        self.discover_preferred_ancestry_for(candidates, policy, ChainId::BitcoinBlake2b)
            .await
    }

    async fn discover_preferred_ancestry_for(
        &self,
        candidates: &[OutPoint],
        policy: Policy,
        root_chain: ChainId,
    ) -> Result<Option<DiscoveredAncestry>, DiscoveryError> {
        let observation = DiscoveryError::Observation;
        if candidates.len() > MAX_ANCESTRY_CANDIDATES {
            return Err(observation(FailureKind::CollectionLimit));
        }
        if policy.max_observation_age_seconds <= 0
            || policy.expiry_margin_seconds <= 0
            || candidates.iter().any(OutPoint::is_null)
            || candidates.iter().collect::<BTreeSet<_>>().len() != candidates.len()
        {
            return Err(observation(FailureKind::InvalidPlan));
        }
        if !session_current(&self.generation, self.expected) {
            return Err(observation(FailureKind::Cancelled));
        }
        let snapshot = self.discovery_snapshot();
        let result = tokio::time::timeout(MAX_COLLECTION_TIME, async {
            for candidate in candidates {
                if let Some(proof) = snapshot
                    .discover_ancestry_for(*candidate, policy, root_chain)
                    .await?
                {
                    return Ok(Some(proof));
                }
            }
            Ok(None)
        })
        .await
        .map_err(|_| observation(FailureKind::Deadline))?;
        if !session_current(&snapshot.generation, snapshot.expected) {
            return Err(observation(FailureKind::Cancelled));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::tests::{ancestry_fixture, source};
    use super::*;
    use coincube_core::miniscript::bitcoin::{
        consensus::{deserialize, serialize},
        Amount, Transaction,
    };
    use httpmock::prelude::*;
    use serde_json::json;

    fn policy() -> Policy {
        Policy {
            max_observation_age_seconds: 60,
            expiry_margin_seconds: 600,
        }
    }

    #[tokio::test]
    async fn preferred_search_rejects_invalid_sets_and_closed_sessions_before_io() {
        let server = MockServer::start();
        let (source, sender) = source(&server);
        let any = server.mock(|when, then| {
            when.method(GET);
            then.status(500);
        });
        let (id, _) = ancestry_fixture();
        let selected = OutPoint::new(id, 0);
        for candidates in [vec![selected, selected], vec![OutPoint::null()]] {
            assert!(matches!(
                source
                    .discover_preferred_ancestry(&candidates, policy())
                    .await,
                Err(DiscoveryError::Observation(FailureKind::InvalidPlan))
            ));
        }
        assert!(matches!(
            source
                .discover_preferred_ancestry(&vec![selected; MAX_ANCESTRY_CANDIDATES + 1], policy())
                .await,
            Err(DiscoveryError::Observation(FailureKind::CollectionLimit))
        ));
        let mut bad_policy = policy();
        bad_policy.max_observation_age_seconds = 0;
        assert!(matches!(
            source.discover_preferred_ancestry(&[], bad_policy).await,
            Err(DiscoveryError::Observation(FailureKind::InvalidPlan))
        ));
        assert!(source
            .discover_preferred_ancestry(&[], policy())
            .await
            .unwrap()
            .is_none());
        drop(sender);
        assert!(matches!(
            source.discover_preferred_ancestry(&[], policy()).await,
            Err(DiscoveryError::Observation(FailureKind::Cancelled))
        ));
        any.assert_hits(0);
    }

    #[tokio::test]
    async fn paired_search_refuses_overlap_and_oversize_before_any_network_read() {
        let mut client = crate::services::coincube::CoincubeClient::for_test("http://127.0.0.1:1");
        client.set_token("paired-search-fixture");
        let (_sender, generation) = tokio::sync::watch::channel(1);
        let source = HttpObservationSource::new(
            client,
            ChainId::Bitcoin,
            ChainId::BitcoinBlake2b,
            CollectionContext {
                expected_generation: 1,
                generation,
            },
        )
        .unwrap();
        let (id, _) = ancestry_fixture();
        let shared = OutPoint::new(id, 0);
        let funding = OutPoint::new(id, 1);
        for (bitcoin, fork, common) in [
            (vec![shared], vec![funding], vec![shared]),
            (vec![funding], vec![funding], vec![shared]),
            (vec![funding, funding], vec![], vec![shared]),
            (vec![funding], vec![], vec![]),
        ] {
            assert!(matches!(
                source
                    .discover_spend_inputs(&bitcoin, &fork, &common, policy())
                    .await,
                Err(DiscoveryError::Observation(FailureKind::InvalidPlan))
            ));
        }
        assert!(matches!(
            source
                .discover_spend_inputs(
                    &[],
                    &vec![funding; MAX_ANCESTRY_CANDIDATES + 1],
                    &[shared],
                    policy()
                )
                .await,
            Err(DiscoveryError::Observation(FailureKind::CollectionLimit))
        ));
    }

    #[tokio::test]
    async fn preferred_candidates_share_request_budget_and_transient_errors_abort() {
        for transient in [false, true] {
            let server = MockServer::start();
            let (mut source, _sender) = source(&server);
            source.budget = Some(Arc::new(CollectionBudget {
                requests: AtomicUsize::new(3),
                bytes: AtomicUsize::new(MAX_RESPONSE_BYTES),
            }));
            let (_, raw) = ancestry_fixture();
            let first: Transaction = deserialize(&raw).unwrap();
            let mut second = first.clone();
            second.output[0].value = Amount::from_sat(6000);
            let mut reads = Vec::new();
            let mut statuses = Vec::new();
            for (i, tx) in [&first, &second].iter().enumerate() {
                reads.push(server.mock(|when, then| {
                    when.method(GET).path(format!(
                        "/api/v1/esplora/bitcoin/mainnet/tx/{}/hex",
                        tx.compute_txid()
                    ));
                    then.status(200).body(hex::encode(serialize(*tx)));
                }));
                statuses.push(server.mock(|when, then| {
                    when.method(GET).path(format!("/api/v1/esplora/bitcoin/mainnet/tx/{}", tx.compute_txid()));
                    then.status(if transient && i == 0 {500} else {200})
                        .header("x-cache", "BYPASS").header("cache-control", "no-store").header("x-coincube-observation", "fresh")
                        .json_body(json!({"txid":tx.compute_txid(), "status":{"confirmed":true,"block_height":17,"block_hash":"88".repeat(32)}}));
                }));
            }
            let result = source
                .discover_preferred_ancestry(
                    &[
                        OutPoint::new(first.compute_txid(), 0),
                        OutPoint::new(second.compute_txid(), 0),
                    ],
                    policy(),
                )
                .await;
            let expected = if transient {
                FailureKind::Http(500)
            } else {
                FailureKind::CollectionLimit
            };
            assert!(matches!(result, Err(DiscoveryError::Observation(kind)) if kind == expected));
            reads[0].assert_hits(1);
            statuses[0].assert_hits(1);
            reads[1].assert_hits(if transient { 0 } else { 1 });
            statuses[1].assert_hits(0);
        }
    }
}
