//! Search wallet-supplied candidates without multiplying collection budgets.
use super::*;

pub const MAX_ANCESTRY_CANDIDATES: usize = 32;
impl HttpObservationSource {
    /// Search in caller preference order. The caller supplies owned, confirmed,
    /// mature, unspent candidates disjoint from the shared Claim inputs; those
    /// conditions must be checked again during construction and preflight.
    ///
    /// None means no candidate qualified, not proof that any coin is replay-safe.
    /// A transient error or exhausted bound aborts rather than masquerading as
    /// an empty search. The caller must assess OP_RETURN independently for fallback.
    pub async fn discover_preferred_ancestry(
        &self,
        candidates: &[OutPoint],
        policy: Policy,
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
        if *self.generation.borrow() != self.expected || self.generation.has_changed().is_err() {
            return Err(observation(FailureKind::Cancelled));
        }
        let snapshot = self.discovery_snapshot();
        let result = tokio::time::timeout(MAX_COLLECTION_TIME, async {
            for candidate in candidates {
                if let Some(proof) = snapshot.discover_ancestry(*candidate, policy).await? {
                    return Ok(Some(proof));
                }
            }
            Ok(None)
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
