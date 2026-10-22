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
    /// and the final collection. Search has one 90s deadline for the pair;
    /// final fresh collection retains its own 30s deadline.
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
        let source = self.discovery_search_snapshot();
        let found = tokio::time::timeout(MAX_DISCOVERY_TIME, async {
            let search = |chain, candidates| {
                let source = &source;
                async move {
                    let result = source
                        .discover_preferred_ancestry_for(candidates, policy, chain)
                        .await;
                    source.report_funding(
                        chain,
                        match &result {
                            Ok(Some(_)) => FundingProofStatus::PathFound,
                            Ok(None) => FundingProofStatus::NoQualifyingAncestry,
                            Err(error) => FundingProofStatus::Failed(*error),
                        },
                    );
                    result
                }
            };
            let (bitcoin, fork) = if source.funding_observer.is_some() {
                tokio::join!(
                    search(ChainId::Bitcoin, bitcoin_candidates),
                    search(ChainId::BitcoinBlake2b, fork_candidates)
                )
            } else {
                let bitcoin = search(ChainId::Bitcoin, bitcoin_candidates).await?;
                if bitcoin.is_none() {
                    return Ok(None);
                }
                (
                    Ok(bitcoin),
                    search(ChainId::BitcoinBlake2b, fork_candidates).await,
                )
            };
            let (Some(bitcoin), Some(fork)) = (bitcoin?, fork?) else {
                return Ok(None);
            };
            Ok(Some((bitcoin, fork)))
        })
        .await
        .map_err(|_| DiscoveryError::Observation(FailureKind::Deadline))??;
        let Some((bitcoin, fork)) = found else {
            return Ok(None);
        };
        // Final collection keeps the established 256-request ceiling without
        // resetting the shared search counters or response-byte allowance.
        source.budget.as_ref().unwrap().restrict_to_collection();
        let bitcoin_path = bitcoin
            .retained_path()
            .map_err(DiscoveryError::Structural)?;
        let fork_path = fork.retained_path().map_err(DiscoveryError::Structural)?;
        let inputs = source
            .collect_spend_inputs(&bitcoin_path, &fork_path, shared, policy)
            .await
            .map_err(DiscoveryError::Observation)?;
        Ok(Some(inputs))
    }

    /// Search in caller preference order. The caller supplies owned, confirmed,
    /// mature, unspent candidates disjoint from the shared Claim inputs; those
    /// conditions must be checked again during construction and preflight.
    ///
    /// None means no candidate qualified, not proof that any coin is replay-safe.
    /// Transient and shared-budget failures abort. A candidate-local graph
    /// limit allows later candidates, but never masquerades as an empty search.
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
            || candidates.iter().any(OutPoint::is_null)
            || candidates.iter().collect::<BTreeSet<_>>().len() != candidates.len()
        {
            return Err(observation(FailureKind::InvalidPlan));
        }
        if !session_current(&self.generation, self.expected) {
            return Err(observation(FailureKind::Cancelled));
        }
        let snapshot = self.discovery_search_snapshot();
        let result = tokio::time::timeout(MAX_DISCOVERY_TIME, async {
            let mut local_limit = None;
            for candidate in candidates {
                match snapshot
                    .discover_ancestry_for(*candidate, policy, root_chain)
                    .await
                {
                    Ok(Some(proof)) => return Ok(Some(proof)),
                    Ok(None) => {}
                    Err(
                        error @ DiscoveryError::Structural(
                            claim_ancestry::Error::PathLimit | claim_ancestry::Error::ByteLimit,
                        ),
                    ) => {
                        local_limit.get_or_insert(error);
                    }
                    Err(error) => return Err(error),
                }
            }
            match local_limit {
                Some(error) => Err(error),
                None => Ok(None),
            }
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
    async fn claim_diagnostics_observe_both_sides_without_short_circuit_or_extra_budget() {
        use std::sync::Mutex;
        for (bitcoin_failure, fork_failure) in [(true, false), (false, true), (true, true)] {
            let server = MockServer::start();
            let _cooldown =
                crate::services::esplora_backoff::TestCooldownGuard::new(&server.base_url());
            let (source, _sender) = source(&server);
            let legacy_source = source.discovery_snapshot();
            let seen = Arc::new(Mutex::new([FundingProofStatus::Pending; 2]));
            let observed = seen.clone();
            let mut source = source.with_funding_observer(Arc::new(move |chain, status| {
                observed.lock().unwrap()[usize::from(chain == ChainId::BitcoinBlake2b)] = status;
            }));
            // Two attempts share one finite request counter, including nested snapshots.
            source.budget = Some(Arc::new(CollectionBudget {
                requests: AtomicUsize::new(2),
                bytes: AtomicUsize::new(MAX_RESPONSE_BYTES),
            }));
            let budget = source.budget.clone().unwrap();
            let (id, _) = ancestry_fixture();
            let bitcoin = OutPoint::new(id, 0);
            let fork = OutPoint::new(id, 1);
            let shared = OutPoint::new(id, 2);
            let bitcoin_read = server.mock(|when, then| {
                when.method(GET)
                    .path(format!("/api/v1/esplora/bitcoin/mainnet/tx/{id}/hex"));
                then.status(503);
            });
            let fork_read = server.mock(|when, then| {
                when.method(GET).path(format!(
                    "/api/v1/esplora/bitcoin-blake2b/mainnet/tx/{id}/hex"
                ));
                then.status(429);
            });
            // Legacy callers still return Bitcoin None before touching a failing fork.
            assert!(legacy_source
                .discover_spend_inputs(&[], &[fork], &[shared], policy())
                .await
                .unwrap()
                .is_none());
            fork_read.assert_hits(0);
            let result = source
                .discover_spend_inputs(
                    if bitcoin_failure {
                        std::slice::from_ref(&bitcoin)
                    } else {
                        &[]
                    },
                    if fork_failure {
                        std::slice::from_ref(&fork)
                    } else {
                        &[]
                    },
                    &[shared],
                    policy(),
                )
                .await;
            assert!(result.is_err());
            let seen = *seen.lock().unwrap();
            assert_eq!(
                seen[0],
                if bitcoin_failure {
                    FundingProofStatus::Failed(DiscoveryError::Observation(FailureKind::Http(503)))
                } else {
                    FundingProofStatus::NoQualifyingAncestry
                }
            );
            assert_eq!(
                seen[1],
                if fork_failure {
                    FundingProofStatus::Failed(DiscoveryError::Observation(FailureKind::Http(429)))
                } else {
                    FundingProofStatus::NoQualifyingAncestry
                }
            );
            bitcoin_read.assert_hits(usize::from(bitcoin_failure));
            fork_read.assert_hits(usize::from(fork_failure));
            assert_eq!(
                budget.requests.load(Ordering::Relaxed),
                2 - usize::from(bitcoin_failure) - usize::from(fork_failure)
            );
        }
    }

    #[tokio::test]
    async fn preferred_candidates_share_request_budget_and_transient_errors_abort() {
        for transient in [false, true] {
            let server = MockServer::start();
            let (mut source, _sender) = source(&server);
            source.budget = Some(Arc::new(CollectionBudget {
                requests: AtomicUsize::new(2),
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
            reads[1].assert_hits(0);
            statuses[1].assert_hits(0);
        }
    }

    #[tokio::test]
    async fn candidate_local_depth_limit_allows_a_later_candidate() {
        use coincube_core::miniscript::bitcoin::{script::Builder, TxIn};
        let server = MockServer::start();
        let (source, _sender) = source(&server);
        let (_, raw) = ancestry_fixture();
        let root: Transaction = deserialize(&raw).unwrap();
        let mut chain = vec![root.clone()];
        for tag in 1..=claim_ancestry::MAX_PATH_TRANSACTIONS {
            let mut tx = root.clone();
            tx.input = vec![TxIn {
                previous_output: OutPoint::new(chain.last().unwrap().compute_txid(), 0),
                script_sig: Builder::new().push_int(tag as i64).into_script(),
                ..TxIn::default()
            }];
            chain.push(tx);
        }
        let first = OutPoint::new(chain.last().unwrap().compute_txid(), 0);
        let mut other = root.clone();
        other.output[0].value = Amount::from_sat(6000);
        let second = OutPoint::new(other.compute_txid(), 0);
        for tx in chain.iter().skip(1) {
            let txid = tx.compute_txid();
            server.mock(|when, then| {
                when.method(GET)
                    .path(format!("/api/v1/esplora/bitcoin/mainnet/tx/{txid}/hex"));
                then.status(200).body(hex::encode(serialize(tx)));
            });
            server.mock(|when, then| {
                when.method(GET)
                    .path(format!("/api/v1/esplora/bitcoin/mainnet/tx/{txid}"));
                then.status(200)
                    .header("x-cache", "BYPASS")
                    .header("cache-control", "no-store")
                    .header("x-coincube-observation", "fresh")
                    .json_body(json!({"txid":txid,"status":{"confirmed":true,"block_height":961650,"block_hash":"88".repeat(32)}}));
            });
        }
        let below_limit = server.mock(|when, then| {
            when.method(GET).path(format!(
                "/api/v1/esplora/bitcoin/mainnet/tx/{}/hex",
                root.compute_txid()
            ));
            then.status(500);
        });
        let next_candidate = server.mock(|when, then| {
            when.method(GET).path(format!(
                "/api/v1/esplora/bitcoin/mainnet/tx/{}/hex",
                second.txid
            ));
            then.status(503);
        });
        let result = source
            .discover_preferred_ancestry(&[first, second], policy())
            .await;
        assert_eq!(
            result.err(),
            Some(DiscoveryError::Observation(FailureKind::Http(503)))
        );
        below_limit.assert_hits(0);
        next_candidate.assert_hits(1);
    }
}
