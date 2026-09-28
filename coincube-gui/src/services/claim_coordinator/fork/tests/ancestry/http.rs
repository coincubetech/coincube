//! Successful protocol qualification is still not coordinator spend authority.
use super::*;
use coincube_core::miniscript::bitcoin::consensus::{deserialize, serialize};
use httpmock::Mock;

struct ProofServices {
    source: HttpObservationSource,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl ForkServices for ProofServices {
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
        _: Arc<VerifiedClaimForkSweep>,
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
async fn ancestry_sweep_reconciliation_recollects_proof_without_granting_completion() {
    let (built, path, bitcoin) = built(10, false);
    let (construction, verified) = ancestry_sweep(&built);
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
            let then = then.header("x-cache", "BYPASS").header("cache-control", "no-store")
                .header("x-coincube-observation", "fresh");
            if route == "bitcoin/mainnet" {
                then.status(200).json_body(json!({"txid":built.psbt().unsigned_tx.compute_txid(),
                    "status":{"confirmed":true,"block_height":root_height+1,"block_hash":hash(5)}}));
            } else { then.status(404); }
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
                        "expiry_time":20000,"active":true}}}}}}));
    });
    fresh(
        &server,
        &format!(
            "/api/v1/esplora/bitcoin/mainnet/block-height/{}",
            root_height + 1
        ),
        hash(5).to_string(),
    );
    fresh(
        &server,
        &format!(
            "/api/v1/esplora/bitcoin-blake2b/mainnet/block-height/{}",
            root_height + 2
        ),
        hash(6).to_string(),
    );
    let sweep_read = fresh(
        &server,
        &format!(
            "/api/v1/esplora/bitcoin-blake2b/mainnet/tx/{}",
            verified.transaction().compute_txid()
        ),
        json!({"txid":verified.transaction().compute_txid(),"status":{"confirmed":true,
            "block_height":root_height+2,"block_hash":hash(6)}})
        .to_string(),
    );
    let temp = Temp::new();
    let controller = Controller::create_ancestry(
        &temp.0,
        "bitcoin-cube".into(),
        "fork-cube".into(),
        &built,
        &path,
        current.clone(),
    )
    .unwrap();
    drop(controller);
    let file = temp.0.join("intent.json");
    let mut stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    stored["phase"] = json!("Tracking");
    stored["signed_txid"] = json!(bitcoin.transaction().compute_txid());
    stored["bitcoin_transaction"] = json!(bitcoin.transaction());
    stored["bitcoin_attempts"] = json!([{ "wtxid":bitcoin.transaction().compute_wtxid() }]);
    stored["plan"]["previous_confirmation"] = json!({"height":root_height+1,"hash":hash(5)});
    stored["fork_sweep"] = json!(construction.psbt().unsigned_tx);
    stored["fork_change_index"] = json!(20);
    // Before any sweep submission exists, fresh proof permits only the owned
    // shared-input PSBT. Expired signing permission cannot be reused.
    std::fs::write(&file, serde_json::to_vec(&stored).unwrap()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut signing_client = CoincubeClient::for_test(server.base_url());
    signing_client.set_token("synthetic-proof-token");
    let signing_source = HttpObservationSource::new(
        signing_client,
        ChainId::Bitcoin,
        ChainId::BitcoinBlake2b,
        CollectionContext {
            expected_generation: 7,
            generation: generation.clone(),
        },
    )
    .unwrap();
    let (signing_construction, _) = ancestry_sweep(&built);
    let unsigned =
        coincube_core::psbt_unified::UnifiedPsbt::from_psbt(signing_construction.psbt().clone())
            .unwrap();
    let mut preparation = Preparation::open(
        &temp.0,
        "bitcoin-cube".into(),
        "fork-cube".into(),
        &built,
        signing_construction,
        current.clone(),
        generation.clone(),
        Box::new(ProofServices {
            source: signing_source,
            calls: calls.clone(),
        }),
        policy(),
    )
    .unwrap();
    let mut expired = preparation.check_signing(&current).await.unwrap();
    expired.not_after = Instant::now();
    assert!(matches!(
        preparation.signing_psbt(expired, &unsigned, &current),
        Err(Error::ExpiredEvidence)
    ));
    let checked = preparation.check_signing(&current).await.unwrap();
    let signing = preparation
        .signing_psbt(checked, &unsigned, &current)
        .unwrap();
    assert_eq!(signing.unsigned_tx, construction.psbt().unsigned_tx);
    assert!(signing
        .unsigned_tx
        .input
        .iter()
        .all(|input| input.previous_output != path.selected()));
    assert!(preparation.controller.recorded_fork_submission().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(preparation);
    stored["fork_submission"] = json!({"txid":verified.transaction().compute_txid(),"wtxid":verified.transaction().compute_wtxid()});
    std::fs::write(&file, serde_json::to_vec(&stored).unwrap()).unwrap();
    let before = std::fs::read(&file).unwrap();
    let mut coordinator = Coordinator::open(
        &temp.0,
        "bitcoin-cube".into(),
        "fork-cube".into(),
        &built,
        construction,
        verified,
        current.clone(),
        generation,
        Box::new(ProofServices {
            source,
            calls: calls.clone(),
        }),
        policy(),
    )
    .unwrap();
    for round in 1..=2 {
        let (status, transaction) = coordinator.reconcile_sweep(&current).await.unwrap();
        assert_eq!(
            status,
            Status::Observation(Assessment::InputProofUnsupported)
        );
        assert!(matches!(
            transaction,
            TransactionObservation::Confirmed { .. }
        ));
        root_read.assert_hits(4 + 2 * round);
        sweep_read.assert_hits(2 * round);
        for read in &step_reads {
            read.assert_hits(8 + 4 * round);
        }
        assert_eq!(std::fs::read(&file).unwrap(), before);
    }
    assert!(coordinator
        .check_completion(&current)
        .await
        .unwrap()
        .is_none());
    root_read.assert_hits(10);
    let settings_root = temp.0.join("no-settings");
    assert!(matches!(
        coordinator
            .reconcile_completion(
                &current,
                &crate::dir::CoincubeDirectory::new(settings_root.clone())
            )
            .await,
        Err(Error::Unsupported)
    ));
    assert!(!settings_root.exists());
    root_read.assert_hits(10);
    fork_position.as_ref().unwrap().delete_async().await;
    server.mock(|when, then| {
        when.method(GET).path(format!(
            "/api/v1/esplora/bitcoin-blake2b/mainnet/block/{}/txid/0",
            hash(4)
        ));
        then.status(500);
    });
    assert!(matches!(
        coordinator.reconcile_sweep(&current).await,
        Err(Error::Observation(claim_observation::Failure {
            kind: FailureKind::Http(500),
            ..
        }))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    sender.send_replace(8);
    assert!(matches!(
        coordinator.reconcile_sweep(&current).await,
        Err(Error::Revoked)
    ));
    assert_eq!(std::fs::read(&file).unwrap(), before);
}
