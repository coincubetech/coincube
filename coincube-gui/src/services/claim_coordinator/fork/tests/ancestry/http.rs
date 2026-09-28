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
async fn ancestry_sweep_reconciliation_recollects_proof_without_persisting_completion() {
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
    let mut bitcoin_position = None;
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
        if route == "bitcoin/mainnet" {
            bitcoin_position = Some(position);
        } else {
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
            Status::Observation(Assessment::ObservationsEligibleForPreflight)
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

    // Existing completion markers remain gated while ancestry is valid. A
    // stable canonical root change can revoke only the exact recorded sweep.
    use crate::app::settings::{update_settings_file, CubeSettings, Settings, VaultIdentity};
    let completion_root = crate::dir::CoincubeDirectory::new(temp.0.join("ancestry-settings"));
    let identity = VaultIdentity::generate(coordinator.construction.descriptor());
    let completion_txid = coordinator.verified.transaction().compute_txid();
    for (chain, id) in [
        (ChainId::Bitcoin, "bitcoin-cube"),
        (ChainId::BitcoinBlake2b, "fork-cube"),
    ] {
        let mut cube =
            CubeSettings::new_with_raw_id(id.into(), id.into(), chain).with_vault(identity.clone());
        cube.split_completed_at_height = Some(u64::from(root_height + 2));
        cube.split_completion_txid = Some(completion_txid);
        update_settings_file(&completion_root.network_directory(chain), |mut settings| {
            settings.cubes.push(cube);
            Some(settings)
        })
        .await
        .unwrap();
    }
    assert!(matches!(
        coordinator
            .reconcile_completion(&current, &completion_root)
            .await,
        Err(Error::Unsupported)
    ));
    for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
        assert_eq!(
            Settings::from_file(&completion_root.network_directory(chain))
                .unwrap()
                .cubes[0]
                .split_completion_txid,
            Some(completion_txid)
        );
    }
    bitcoin_position.as_mut().unwrap().delete_async().await;
    let mut changed_root = root.clone();
    changed_root.output[0].value = Amount::from_sat(80_000);
    let changed_position = fresh(
        &server,
        &format!("/api/v1/esplora/bitcoin/mainnet/block/{}/txid/0", hash(3)),
        changed_root.compute_txid().to_string(),
    );
    server.mock(|when, then| {
        when.method(GET).path(format!(
            "/api/v1/esplora/bitcoin/mainnet/tx/{}/hex",
            changed_root.compute_txid()
        ));
        then.status(200).body(hex::encode(serialize(&changed_root)));
    });
    assert!(matches!(
        coordinator
            .reconcile_completion(&current, &completion_root)
            .await
            .unwrap(),
        CompletionReconciliation::AncestryInvalidated {
            kind: FailureKind::AncestryRootChanged { .. }
        }
    ));
    for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
        let cube = Settings::from_file(&completion_root.network_directory(chain))
            .unwrap()
            .cubes
            .remove(0);
        assert_eq!(cube.split_completed_at_height, None);
        assert_eq!(cube.split_completion_txid, None);
    }
    changed_position.delete_async().await;
    let restored_bitcoin_position = fresh(
        &server,
        &format!("/api/v1/esplora/bitcoin/mainnet/block/{}/txid/0", hash(3)),
        root.compute_txid().to_string(),
    );
    for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
        update_settings_file(&completion_root.network_directory(chain), |mut settings| {
            settings.cubes[0].split_completed_at_height = Some(u64::from(root_height + 2));
            settings.cubes[0].split_completion_txid = Some(completion_txid);
            Some(settings)
        })
        .await
        .unwrap();
    }
    fork_position.as_mut().unwrap().delete_async().await;
    let shared_position = fresh(
        &server,
        &format!(
            "/api/v1/esplora/bitcoin-blake2b/mainnet/block/{}/txid/0",
            hash(4)
        ),
        root.compute_txid().to_string(),
    );
    server.mock(|when, then| {
        when.method(GET).path(format!(
            "/api/v1/esplora/bitcoin-blake2b/mainnet/tx/{}/hex",
            root.compute_txid()
        ));
        then.status(200).body(hex::encode(serialize(&root)));
    });
    assert!(matches!(
        coordinator
            .reconcile_completion(&current, &completion_root)
            .await
            .unwrap(),
        CompletionReconciliation::AncestryInvalidated {
            kind: FailureKind::AncestryRootShared { .. }
        }
    ));
    for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
        assert_eq!(
            Settings::from_file(&completion_root.network_directory(chain))
                .unwrap()
                .cubes[0]
                .split_completion_txid,
            None
        );
    }
    shared_position.delete_async().await;
    let restored_fork_position = fresh(
        &server,
        &format!(
            "/api/v1/esplora/bitcoin-blake2b/mainnet/block/{}/txid/0",
            hash(4)
        ),
        fork_root.compute_txid().to_string(),
    );
    restored_fork_position.delete_async().await;
    let fork_failure = server.mock(|when, then| {
        when.method(GET).path(format!(
            "/api/v1/esplora/bitcoin-blake2b/mainnet/block/{}/txid/0",
            hash(4)
        ));
        then.status(500);
    });
    for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
        update_settings_file(&completion_root.network_directory(chain), |mut settings| {
            settings.cubes[0].split_completed_at_height = Some(u64::from(root_height + 2));
            settings.cubes[0].split_completion_txid = Some(completion_txid);
            Some(settings)
        })
        .await
        .unwrap();
    }
    assert!(matches!(
        coordinator
            .reconcile_completion(&current, &completion_root)
            .await,
        Err(Error::Observation(claim_observation::Failure {
            kind: FailureKind::Http(500),
            ..
        }))
    ));
    for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
        assert_eq!(
            Settings::from_file(&completion_root.network_directory(chain))
                .unwrap()
                .cubes[0]
                .split_completion_txid,
            Some(completion_txid)
        );
    }
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
    drop(coordinator);

    // The coordinator session and the HTTP ancestry source are separate
    // objects. A source from another provider or generation must fail before
    // network collection and must not turn a negative result into settings
    // authority.
    let settings_before: Vec<_> = [ChainId::Bitcoin, ChainId::BitcoinBlake2b]
        .iter()
        .copied()
        .map(|chain| {
            std::fs::read(
                completion_root
                    .network_directory(chain)
                    .path()
                    .join(crate::app::settings::SETTINGS_FILE_NAME),
            )
            .unwrap()
        })
        .collect();
    let (_session_sender, session_generation) = watch::channel(7);
    let foreign = MockServer::start();
    let mut foreign_client = CoincubeClient::for_test(foreign.base_url());
    foreign_client.set_token("synthetic-proof-token");
    let foreign_source = HttpObservationSource::new(
        foreign_client,
        ChainId::Bitcoin,
        ChainId::BitcoinBlake2b,
        CollectionContext {
            expected_generation: 7,
            generation: session_generation.clone(),
        },
    )
    .unwrap();
    let (foreign_construction, foreign_verified) = ancestry_sweep(&built);
    let mut foreign_coordinator = Coordinator::open(
        &temp.0,
        "bitcoin-cube".into(),
        "fork-cube".into(),
        &built,
        foreign_construction,
        foreign_verified,
        current.clone(),
        session_generation.clone(),
        Box::new(ProofServices {
            source: foreign_source,
            calls: calls.clone(),
        }),
        policy(),
    )
    .unwrap();
    assert!(matches!(
        foreign_coordinator
            .reconcile_completion(&current, &completion_root)
            .await,
        Err(Error::Observation(claim_observation::Failure {
            stage: claim_observation::Stage::Context,
            kind: FailureKind::Changed,
        }))
    ));
    drop(foreign_coordinator);

    let mut generation_client = CoincubeClient::for_test(server.base_url());
    generation_client.set_token("synthetic-proof-token");
    let generation_source = HttpObservationSource::new(
        generation_client,
        ChainId::Bitcoin,
        ChainId::BitcoinBlake2b,
        CollectionContext {
            expected_generation: 8,
            generation: session_generation.clone(),
        },
    )
    .unwrap();
    let (generation_construction, generation_verified) = ancestry_sweep(&built);
    let mut generation_coordinator = Coordinator::open(
        &temp.0,
        "bitcoin-cube".into(),
        "fork-cube".into(),
        &built,
        generation_construction,
        generation_verified,
        current.clone(),
        session_generation,
        Box::new(ProofServices {
            source: generation_source,
            calls: calls.clone(),
        }),
        policy(),
    )
    .unwrap();
    assert!(matches!(
        generation_coordinator
            .reconcile_completion(&current, &completion_root)
            .await,
        Err(Error::Observation(claim_observation::Failure {
            stage: claim_observation::Stage::Context,
            kind: FailureKind::Cancelled,
        }))
    ));
    for (chain, expected) in [ChainId::Bitcoin, ChainId::BitcoinBlake2b]
        .iter()
        .copied()
        .zip(&settings_before)
    {
        assert_eq!(
            std::fs::read(
                completion_root
                    .network_directory(chain)
                    .path()
                    .join(crate::app::settings::SETTINGS_FILE_NAME)
            )
            .unwrap(),
            expected.clone()
        );
    }

    drop(generation_coordinator);
    fork_failure.delete_async().await;
    fresh(
        &server,
        &format!(
            "/api/v1/esplora/bitcoin-blake2b/mainnet/block/{}/txid/0",
            hash(4)
        ),
        fork_root.compute_txid().to_string(),
    );
    restored_bitcoin_position.delete_async().await;
    let _race_changed_position = fresh(
        &server,
        &format!("/api/v1/esplora/bitcoin/mainnet/block/{}/txid/0", hash(3)),
        changed_root.compute_txid().to_string(),
    );
    let (_race_session_sender, race_session_generation) = watch::channel(7);
    let (race_source_sender, race_source_generation) = watch::channel(7);
    let mut race_client = CoincubeClient::for_test(server.base_url());
    race_client.set_token("synthetic-proof-token");
    let race_source = HttpObservationSource::new(
        race_client,
        ChainId::Bitcoin,
        ChainId::BitcoinBlake2b,
        CollectionContext {
            expected_generation: 7,
            generation: race_source_generation,
        },
    )
    .unwrap();
    let (race_construction, race_verified) = ancestry_sweep(&built);
    let mut race_coordinator = Coordinator::open(
        &temp.0,
        "bitcoin-cube".into(),
        "fork-cube".into(),
        &built,
        race_construction,
        race_verified,
        current.clone(),
        race_session_generation,
        Box::new(ProofServices {
            source: race_source,
            calls: calls.clone(),
        }),
        policy(),
    )
    .unwrap();
    let cleanup_reached = Arc::new(tokio::sync::Notify::new());
    let cleanup_release = Arc::new(tokio::sync::Notify::new());
    race_coordinator.completion_cleanup_barrier =
        Some((cleanup_reached.clone(), cleanup_release.clone()));
    let reconcile = race_coordinator.reconcile_completion(&current, &completion_root);
    let revoke_while_waiting = async {
        tokio::time::timeout(Duration::from_secs(3), cleanup_reached.notified())
            .await
            .unwrap();
        race_source_sender.send_replace(8);
        cleanup_release.notify_one();
    };
    let (result, ()) = tokio::join!(reconcile, revoke_while_waiting);
    assert!(matches!(result, Err(Error::CompletionPersistence(_))));
    for (chain, expected) in [ChainId::Bitcoin, ChainId::BitcoinBlake2b]
        .iter()
        .copied()
        .zip(&settings_before)
    {
        assert_eq!(
            std::fs::read(
                completion_root
                    .network_directory(chain)
                    .path()
                    .join(crate::app::settings::SETTINGS_FILE_NAME)
            )
            .unwrap(),
            expected.clone()
        );
    }
}
