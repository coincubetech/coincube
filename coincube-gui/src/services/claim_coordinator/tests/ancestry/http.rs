//! Successful protocol qualification is still not coordinator spend authority.
use super::*;
use coincube_core::claim;
use httpmock::Mock;

struct ProofServices {
    source: HttpObservationSource,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl Services for ProofServices {
    fn source(&self) -> &dyn ObservationSource {
        &self.source
    }
    fn ancestry_source(&self) -> Option<&HttpObservationSource> {
        Some(&self.source)
    }
    async fn preflight(
        &self,
        _: &Transaction,
        _: BlockHash,
        _: FreshnessPolicy,
    ) -> Result<Evidence, claim_preflight::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(claim_preflight::Error::UnsupportedChain)
    }
    async fn submit(
        &self,
        _: VerifiedStep1,
        _: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(DaemonError::DaemonStopped)
    }
}
fn fresh<'a>(server: &'a MockServer, path: &str, body: String) -> Mock<'a> {
    server.mock(|when, then| {
        when.method(GET)
            .path(path)
            .header("x-coincube-observation", "fresh");
        then.status(200)
            .header("x-cache", "BYPASS")
            .header("cache-control", "no-store")
            .header("x-coincube-observation", "fresh")
            .body(body);
    })
}

#[tokio::test]
async fn successful_http_proof_is_recollected_and_cannot_authorize_coordinator_submission() {
    ancestry_protocol_case(true, 20_000).await;
}

#[tokio::test]
async fn expired_inactive_rdts_does_not_invalidate_positive_ancestry_observations() {
    ancestry_protocol_case(false, 1).await;
}

#[tokio::test]
async fn malformed_rdts_metadata_still_refuses_ancestry_collection() {
    ancestry_protocol_case(false, 0).await;
}

async fn ancestry_protocol_case(rdts_active: bool, expiry_time: i64) {
    let (built, path, verified) = built(10, false);
    let root: Transaction = deserialize(&path.links()[0].transaction).unwrap();
    let mut fork_root = root.clone();
    fork_root.output[0].value = Amount::from_sat(90_000);
    let server = MockServer::start();
    let (sender, generation) = watch::channel(7);
    let mut client = CoincubeClient::for_test(server.base_url());
    client.set_token("synthetic-proof-token");
    let source = HttpObservationSource::new(
        client,
        ChainId::Bitcoin,
        ChainId::BitcoinBlake2b,
        CollectionContext {
            expected_generation: 7,
            generation: generation.clone(),
        },
    )
    .unwrap();
    let current = Context {
        provider: source.provider_identity(),
        ..context()
    };
    let root_height = 961_640u32;
    let tip_height = root_height + 200;
    let mut fork_position = None;
    let mut step_reads = Vec::new();
    for (route, tip, block, tx) in [
        ("bitcoin/mainnet", hash(1), hash(3), &root),
        ("bitcoin-blake2b/mainnet", hash(2), hash(4), &fork_root),
    ] {
        let prefix = format!("/api/v1/esplora/{}", route);
        fresh(
            &server,
            &format!("{}/blocks/tip/hash", prefix),
            tip.to_string(),
        );
        fresh(
            &server,
            &format!("{}/block/{}/status", prefix, tip),
            json!({"in_best_chain":true,"height":tip_height}).to_string(),
        );
        fresh(
            &server,
            &format!("{}/block-height/{}", prefix, tip_height),
            tip.to_string(),
        );
        fresh(
            &server,
            &format!("{}/block-height/{}", prefix, root_height),
            block.to_string(),
        );
        fresh(
            &server,
            &format!("{}/block-height/227931", prefix),
            "000000000000024b89b42a942fe0d9fea3bb44ab7bd1b19115dd6a759c0808b8".into(),
        );
        let position = fresh(
            &server,
            &format!("{}/block/{}/txid/0", prefix, block),
            tx.compute_txid().to_string(),
        );
        if route.starts_with("bitcoin-blake2b") {
            fork_position = Some(position);
        }
        server.mock(|when, then| {
            when.method(GET)
                .path(format!("{}/tx/{}/hex", prefix, tx.compute_txid()));
            then.status(200).body(hex::encode(serialize(tx)));
        });
        step_reads.push(server.mock(|when, then| {
            when.method(GET).path(format!(
                "{}/tx/{}",
                prefix,
                built.psbt().unsigned_tx.compute_txid()
            ));
            then.status(404)
                .header("x-cache", "BYPASS")
                .header("cache-control", "no-store")
                .header("x-coincube-observation", "fresh");
        }));
    }
    let root_read = fresh(
        &server,
        &format!("/api/v1/esplora/bitcoin/mainnet/tx/{}", root.compute_txid()),
        json!({"txid":root.compute_txid(),"status":{"confirmed":true,
            "block_height":root_height,"block_hash":hash(3)}})
        .to_string(),
    );
    server.mock(|when, then| {
        when.method(GET)
            .path("/api/v1/connect/networks/bitcoin-blake2b/anchor")
            .header("authorization", "Bearer synthetic-proof-token");
        then.status(200).json_body(json!({"success":true,"data":{
            "network":"bitcoin-blake2b","state":"available","anchor":{
                "tip_hash":hash(2),"tip_height":tip_height,"tip_median_time_past":10000,
                "observed_at":source.now(),"observation":{"tip_height":tip_height,
                    "fork":{"height":root_height,"active":true},
                    "rdts":{"state":"flagday","flagday":{"height":root_height,
                        "expiry_time":expiry_time,"active":rdts_active}}}}}}));
    });
    let plan = claim::ClaimPlan {
        bitcoin_chain: ChainId::Bitcoin,
        fork_chain: ChainId::BitcoinBlake2b,
        step1: built.psbt().unsigned_tx.clone(),
        claimed_prevouts: built.claimed_prevouts().to_vec(),
        poison: claim::Poison::InputAncestry,
        previous_confirmation: None,
    };
    let collected = source
        .collect_ancestry(
            &path,
            &plan,
            policy().observations,
            policy().collection_budget,
        )
        .await;
    if expiry_time == 0 {
        assert!(matches!(
            collected,
            Err(claim_observation::Failure {
                kind: FailureKind::Malformed,
                ..
            })
        ));
        return;
    }
    let collected = collected.unwrap();
    let checked_at = source.now();
    let proof_context = |now| crate::services::claim_observation::http::AncestryContext {
        provider: &current.provider,
        generation: current.generation,
        policy: policy().observations,
        now,
        tips: collected.assessment().observations.preflight,
    };
    assert_eq!(
        collected
            .bitcoin_confirmation(&path, &plan, proof_context(checked_at))
            .unwrap(),
        claim::BitcoinConfirmation::Unconfirmed
    );
    assert_eq!(
        collected
            .assess_verified_observations(&path, &plan, proof_context(checked_at))
            .unwrap()
            .assessment,
        Assessment::WaitingForConfirmation
    );
    let mut tightened = proof_context(checked_at + 2);
    tightened.policy.max_observation_age_seconds = 1;
    assert!(matches!(
        collected.assess_verified_observations(&path, &plan, tightened),
        Err(FailureKind::Stale)
    ));
    let mut altered = plan.clone();
    altered.step1.output[0].value = Amount::from_sat(1);
    assert_eq!(
        collected.bitcoin_confirmation(&path, &altered, proof_context(checked_at)),
        Err(FailureKind::Changed)
    );
    assert_eq!(
        collected.bitcoin_confirmation(
            &path,
            &plan,
            proof_context(checked_at + policy().observations.max_observation_age_seconds + 1)
        ),
        Err(FailureKind::Stale)
    );
    assert_eq!(
        collected.assessment().assessment,
        Assessment::InputProofUnsupported
    );
    let temp = Temp::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut coordinator = Coordinator::open_ancestry(
        &temp.0,
        "bitcoin-cube".into(),
        "fork-cube".into(),
        &built,
        &path,
        verified,
        current.clone(),
        generation,
        Box::new(ProofServices {
            source,
            calls: calls.clone(),
        }),
        policy(),
        false,
    )
    .unwrap();
    let before = std::fs::read(temp.0.join("intent.json")).unwrap();
    for round in 1..=2 {
        assert!(matches!(
            coordinator.prepare_review(&current).await,
            Err(Error::NotReady(Assessment::InputProofUnsupported))
        ));
        root_read.assert_hits(round + 1);
        for read in &step_reads {
            read.assert_hits(2 * (round + 1));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(temp.0.join("intent.json")).unwrap(), before);
    }
    // The controller requires the opaque live proof and rechecks its lifetime
    // at the durable-intent boundary, even after accepting the observation.
    let source = coordinator.services.ancestry_source().unwrap();
    let intent_collection = source
        .collect_ancestry(
            &path,
            &plan,
            policy().observations,
            policy().collection_budget,
        )
        .await
        .unwrap();
    let intent_checked_at = source.now();
    let ticket = coordinator.controller.begin_check(&current).unwrap();
    assert_eq!(
        coordinator
            .controller
            .apply_ancestry_observation(
                ticket,
                &current,
                Ok(intent_collection),
                policy().observations,
                intent_checked_at,
            )
            .unwrap(),
        claim_workflow::Status::Observation(Assessment::WaitingForConfirmation),
    );
    let stale_at = intent_checked_at + policy().observations.max_observation_age_seconds + 1;
    for now in [stale_at, intent_checked_at] {
        assert!(matches!(
            coordinator.controller.record_broadcast_intent(
                &current,
                coordinator.verified.transaction(),
                policy().observations,
                now,
            ),
            Err(claim_workflow::Error::Unchecked),
        ));
    }
    assert_eq!(
        coordinator.controller.phase(),
        claim_workflow::Phase::Intent
    );
    assert_eq!(std::fs::read(temp.0.join("intent.json")).unwrap(), before);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // Exercise the same inclusion observer through fresh HTTP collection, then
    // change the canonical height mapping while preserving the tx status reply.
    step_reads[0].delete_async().await;
    let step_height = tip_height - 5;
    fresh(
        &server,
        &format!(
            "/api/v1/esplora/bitcoin/mainnet/tx/{}",
            plan.step1.compute_txid()
        ),
        json!({"txid":plan.step1.compute_txid(),"status":{"confirmed":true,
            "block_height":step_height,"block_hash":hash(5)}})
        .to_string(),
    );
    let mut mapping = fresh(
        &server,
        &format!("/api/v1/esplora/bitcoin/mainnet/block-height/{step_height}"),
        hash(5).to_string(),
    );
    for expected in [
        claim::BitcoinConfirmation::Confirmed { confirmations: 6 },
        claim::BitcoinConfirmation::Reorged,
    ] {
        let source = coordinator.services.ancestry_source().unwrap();
        let observed = source
            .collect_ancestry(
                &path,
                &plan,
                policy().observations,
                policy().collection_budget,
            )
            .await
            .unwrap();
        let now = source.now();
        assert_eq!(
            observed
                .bitcoin_confirmation(
                    &path,
                    &plan,
                    crate::services::claim_observation::http::AncestryContext {
                        provider: &current.provider,
                        generation: current.generation,
                        policy: policy().observations,
                        now,
                        tips: observed.assessment().observations.preflight,
                    }
                )
                .unwrap(),
            expected
        );
        assert_eq!(
            observed
                .assess_verified_observations(
                    &path,
                    &plan,
                    crate::services::claim_observation::http::AncestryContext {
                        provider: &current.provider,
                        generation: current.generation,
                        policy: policy().observations,
                        now,
                        tips: observed.assessment().observations.preflight,
                    }
                )
                .unwrap()
                .assessment,
            if expected == claim::BitcoinConfirmation::Reorged {
                Assessment::Reorged
            } else {
                Assessment::ObservationsEligibleForPreflight
            }
        );
        assert_eq!(
            observed.assessment().assessment,
            Assessment::InputProofUnsupported
        );
        mapping.delete_async().await;
        mapping = fresh(
            &server,
            &format!("/api/v1/esplora/bitcoin/mainnet/block-height/{step_height}"),
            hash(6).to_string(),
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(temp.0.join("intent.json")).unwrap(), before);
    // Contradictory positive fork presence must win over the exclusion result.
    step_reads[1].delete_async().await;
    fresh(
        &server,
        &format!(
            "/api/v1/esplora/bitcoin-blake2b/mainnet/tx/{}",
            plan.step1.compute_txid()
        ),
        json!({"txid":plan.step1.compute_txid(),"status":{"confirmed":false}}).to_string(),
    );
    let source = coordinator.services.ancestry_source().unwrap();
    let present = source
        .collect_ancestry(
            &path,
            &plan,
            policy().observations,
            policy().collection_budget,
        )
        .await
        .unwrap();
    assert_eq!(
        present
            .assess_verified_observations(
                &path,
                &plan,
                crate::services::claim_observation::http::AncestryContext {
                    provider: &current.provider,
                    generation: current.generation,
                    policy: policy().observations,
                    now: source.now(),
                    tips: present.assessment().observations.preflight,
                }
            )
            .unwrap()
            .assessment,
        Assessment::Step1AlreadyOnFork
    );
    // A later unavailable canonical lookup must replace the prior successful
    // qualification, never reuse it or proceed to transaction preflight.
    fork_position.as_mut().unwrap().delete_async().await;
    server.mock(|when, then| {
        when.method(GET).path(format!(
            "/api/v1/esplora/bitcoin-blake2b/mainnet/block/{}/txid/0",
            hash(4)
        ));
        then.status(500);
    });
    assert!(matches!(
        coordinator.prepare_review(&current).await,
        Err(Error::Observation(claim_observation::Failure {
            kind: FailureKind::Http(500),
            ..
        }))
    ));
    root_read.assert_hits(8);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    sender.send_replace(8);
    assert_eq!(
        collected.bitcoin_confirmation(&path, &plan, proof_context(checked_at)),
        Err(FailureKind::Cancelled)
    );
    assert!(matches!(
        coordinator.prepare_review(&current).await,
        Err(Error::Revoked)
    ));
    root_read.assert_hits(8);
    assert_eq!(std::fs::read(temp.0.join("intent.json")).unwrap(), before);
}
