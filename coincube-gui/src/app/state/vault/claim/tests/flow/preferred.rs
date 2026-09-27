use super::*;
use coincube_core::miniscript::bitcoin::consensus::serialize;

#[tokio::test]
async fn automatic_ancestry_build_prefers_owned_input_and_never_falls_back_on_provider_failure() {
    let fixture = fixture_with_multisig(false);
    let server = MockServer::start();
    let height = 961_640;
    let mut root = fixture.previous.clone();
    root.input[0].script_sig = coincube_core::miniscript::bitcoin::script::Builder::new()
        .push_int(height)
        .push_int(1)
        .into_script();
    root.output[0].script_pubkey = fixture
        .descriptor
        .receive_descriptor()
        .derive(1.into(), &secp256k1::Secp256k1::verification_only())
        .script_pubkey();
    let mut selected = fixture.coin.clone();
    selected.outpoint = OutPoint::new(root.compute_txid(), 0);
    selected.block_height = Some(height as i32);
    selected.derivation_index = 1.into();
    selected.address =
        Address::from_script(&root.output[0].script_pubkey, Network::Bitcoin).unwrap();
    let mut fork_root = root.clone();
    fork_root.output[0].value = Amount::from_sat(90_000);
    let hash = |byte| BlockHash::from_byte_array([byte; 32]);
    let fresh_body = |path: String, body: String| {
        server.mock(|when, then| {
            when.method(GET)
                .path(path)
                .header("x-coincube-observation", "fresh");
            fresh(then.status(200)).body(body);
        })
    };
    let mut fork_position = None;
    for (route, tip, block, tx) in [
        ("bitcoin/mainnet", hash(1), hash(3), &root),
        ("bitcoin-blake2b/mainnet", hash(2), hash(4), &fork_root),
    ] {
        let prefix = format!("/api/v1/esplora/{route}");
        fresh_body(format!("{prefix}/blocks/tip/hash"), tip.to_string());
        fresh_body(
            format!("{prefix}/block/{tip}/status"),
            json!({"in_best_chain":true,"height":height+200}).to_string(),
        );
        fresh_body(
            format!("{prefix}/block-height/{}", height + 200),
            tip.to_string(),
        );
        fresh_body(format!("{prefix}/block-height/{height}"), block.to_string());
        fresh_body(
            format!("{prefix}/block-height/227931"),
            "000000000000024b89b42a942fe0d9fea3bb44ab7bd1b19115dd6a759c0808b8".into(),
        );
        let position = fresh_body(
            format!("{prefix}/block/{block}/txid/0"),
            tx.compute_txid().to_string(),
        );
        if route.starts_with("bitcoin-blake2b") {
            fork_position = Some(position);
        }
        server.mock(|when, then| {
            when.method(GET)
                .path(format!("{prefix}/tx/{}/hex", tx.compute_txid()));
            then.status(200).body(hex::encode(serialize(tx)));
        });
    }
    fresh_body(format!("/api/v1/esplora/bitcoin/mainnet/tx/{}", root.compute_txid()),
        json!({"txid":root.compute_txid(),"status":{"confirmed":true,"block_height":height,"block_hash":hash(3)}}).to_string());
    server.mock(|when, then| {
        when.method(GET).path("/api/v1/connect/networks/bitcoin-blake2b/anchor");
        then.status(200).json_body(json!({"success":true,"data":{
            "network":"bitcoin-blake2b","state":"available","anchor":{
                "tip_hash":hash(2),"tip_height":height+200,"tip_median_time_past":10000,
                "observed_at":unix_now(),"observation":{"tip_height":height+200,
                    "fork":{"height":height,"active":true},
                    "rdts":{"state":"flagday","flagday":{"height":height,"expiry_time":200000,"active":true}}}}}}));
    });
    let config = toml::from_str(&format!("main_descriptor = '{}'\n[bitcoin_config]\nnetwork = 'bitcoin'\n[esplora_config]\naddr = '{}/api/v1/esplora/bitcoin/mainnet'\n", fixture.descriptor, server.base_url())).unwrap();
    let daemon = Arc::new(FlowDaemon {
        config,
        coin: fixture.coin.clone(),
        previous: fixture.previous,
        submitted: Mutex::new(None),
        hits: Mutex::new(Vec::new()),
        queried_txs: Mutex::new(Vec::new()),
        ancestry_coin: Some(selected.clone()),
        #[cfg(feature = "regtest-harness")]
        live: None,
    });
    let wallet = Arc::new(Wallet::new(fixture.descriptor));
    let mut client = CoincubeClient::for_test(server.base_url());
    client.set_token("preferred-test-token");
    let connect = ConnectSession {
        client,
        account: "7".into(),
    };
    let (sender, generation) = watch::channel(1);
    let coins = CoinSet {
        pre_fork: vec![fixture.coin.clone()],
        ancestry_candidates: vec![selected.clone()],
        post_fork: 1,
        tip_height: height as i32 + 200,
    };
    let window = ForkWindow {
        fork_height: height as u64,
        fork_hash: hash(4),
        tip_height: height as u64 + 200,
        median_time_past: 10000,
        expires_at: 200000,
        rdts: Ok(()),
    };
    let built = super::super::super::preferred::build_preferred(
        daemon.clone(),
        wallet.clone(),
        coins.clone(),
        5,
        window.clone(),
        connect.clone(),
        1,
        generation.clone(),
    )
    .await
    .unwrap();
    assert_eq!(built.selected_ancestry_input(), Some(selected.outpoint));
    assert_eq!(built.psbt().unsigned_tx.output.len(), 1);
    assert!(!built.psbt().unsigned_tx.output[0]
        .script_pubkey
        .is_op_return());
    let Construction::Ancestry { transfer, .. } = &*built else {
        panic!("positive ancestry must be preferred")
    };
    assert_eq!(transfer.claimed_prevouts(), &[fixture.coin.outpoint]);
    let mut many = coins.clone();
    for vout in 0..32 {
        let mut extra = selected.clone();
        extra.outpoint = OutPoint::new(Txid::from_byte_array([255; 32]), vout);
        many.ancestry_candidates.push(extra);
    }
    many.ancestry_candidates.reverse(); // the caller's order cannot change preference
    let bounded = super::super::super::preferred::build_preferred(
        daemon.clone(),
        wallet.clone(),
        many,
        5,
        window.clone(),
        connect.clone(),
        1,
        generation.clone(),
    )
    .await
    .unwrap();
    assert_eq!(bounded.selected_ancestry_input(), Some(selected.outpoint));
    assert_eq!(
        daemon
            .hits()
            .iter()
            .filter(|h| **h == "reserve_change")
            .count(),
        2
    );
    assert!(!daemon
        .queried_txs
        .lock()
        .unwrap()
        .contains(&selected.outpoint.txid));
    for route in ["bitcoin/mainnet", "bitcoin-blake2b/mainnet"] {
        server.mock(|when, then| {
            when.method(GET).path(format!(
                "/api/v1/esplora/{route}/tx/{}",
                built.psbt().unsigned_tx.compute_txid()
            ));
            fresh(then.status(404));
        });
    }
    let proof_reads = fork_position.as_ref().unwrap().hits();
    let check = super::super::super::signing::check(
        &built,
        daemon.clone(),
        wallet.clone(),
        connect.clone(),
        1,
        generation.clone(),
    )
    .await;
    assert!(check.unwrap_err().contains("not available yet"));
    assert!(
        fork_position.as_ref().unwrap().hits() > proof_reads,
        "signing must recollect the proof"
    );
    let root_dir = std::env::temp_dir().join(format!("ancestry-signing-{}", uuid::Uuid::new_v4()));
    let mut panel = ClaimStep1Panel::new(
        wallet.clone(),
        CoincubeDirectory::new(root_dir.clone()),
        "bitcoin-cube".into(),
        generation.clone(),
        Some(connect.clone()),
    );
    panel.stage = Stage::Plan { built: bounded };
    let cache = Cache::default();
    let menu = Menu::Vault(crate::app::menu::VaultSubMenu::Claim);
    let check_task = panel.request_signing(daemon.clone());
    assert!(matches!(panel.stage, Stage::CheckingSign(_)));
    drop(view::vault::claim::view(&menu, &cache, &panel));
    let signer = panel.update(
        Some(daemon.clone()),
        &cache,
        Message::View(view::Message::Spend(
            view::SpendTxMessage::SelectMasterSigner,
        )),
    );
    assert!(outputs(signer).await.is_empty());
    assert!(matches!(panel.stage, Stage::CheckingSign(_)));
    let mut notices = Vec::new();
    for message in outputs(check_task).await {
        notices.extend(outputs(panel.update(Some(daemon.clone()), &cache, message)).await);
    }
    assert!(matches!(panel.stage, Stage::Plan { .. }));
    assert!(notices.iter().any(|m| matches!(m, Message::View(view::Message::ShowError(e)) if e.contains("not available yet"))));
    let late = panel.request_signing(daemon.clone());
    panel.cancel();
    for message in outputs(late).await {
        assert!(outputs(panel.update(Some(daemon.clone()), &cache, message))
            .await
            .is_empty());
    }
    assert!(matches!(panel.stage, Stage::Preconditions));
    assert!(
        !root_dir.exists(),
        "checking must not create a journal or wallet directory"
    );
    let mut immature = selected.clone();
    immature.is_immature = true;
    let changed = Arc::new(FlowDaemon {
        config: daemon.config.clone(),
        coin: daemon.coin.clone(),
        previous: daemon.previous.clone(),
        submitted: Mutex::new(None),
        hits: Mutex::new(Vec::new()),
        queried_txs: Mutex::new(Vec::new()),
        ancestry_coin: Some(immature),
        #[cfg(feature = "regtest-harness")]
        live: None,
    });
    assert!(super::super::super::preferred::build_preferred(
        changed.clone(),
        wallet.clone(),
        coins.clone(),
        5,
        window.clone(),
        connect.clone(),
        1,
        generation.clone()
    )
    .await
    .is_err());
    assert!(!changed.hits().contains(&"reserve_change"));
    // A locally recorded broadcast must also win over a lagging daemon UTXO view.
    let before = fork_position.as_ref().unwrap().hits();
    assert!(super::super::super::signing::check(
        &built,
        changed.clone(),
        wallet.clone(),
        connect.clone(),
        1,
        generation.clone()
    )
    .await
    .unwrap_err()
    .contains("not yet mature"));
    assert_eq!(fork_position.as_ref().unwrap().hits(), before);
    for derivation in [
        ChildNumber::from_hardened_idx(0).unwrap(),
        ChildNumber::from_normal_idx(0).unwrap(),
    ] {
        let mut bad_coin = selected.clone();
        bad_coin.derivation_index = derivation;
        let bad = Arc::new(FlowDaemon {
            config: daemon.config.clone(),
            coin: daemon.coin.clone(),
            previous: daemon.previous.clone(),
            submitted: Mutex::new(None),
            hits: Mutex::new(Vec::new()),
            queried_txs: Mutex::new(Vec::new()),
            ancestry_coin: Some(bad_coin),
            #[cfg(feature = "regtest-harness")]
            live: None,
        });
        let refusal = super::super::super::signing::check(
            &built,
            bad,
            wallet.clone(),
            connect.clone(),
            1,
            generation.clone(),
        )
        .await
        .unwrap_err();
        assert!(refusal.contains(if derivation.is_hardened() {
            "derivation is invalid"
        } else {
            "metadata changed"
        }));
        assert_eq!(fork_position.as_ref().unwrap().hits(), before);
    }
    let spent_wallet = Arc::new(Wallet::new(wallet.main_descriptor.clone()));
    spent_wallet.record_broadcast(
        built.psbt().unsigned_tx.clone(),
        vec![selected.clone()],
        Vec::new(),
        Network::Bitcoin,
    );
    assert!(super::super::super::preferred::build_preferred(
        daemon.clone(),
        spent_wallet,
        coins.clone(),
        5,
        window.clone(),
        connect.clone(),
        1,
        generation.clone()
    )
    .await
    .is_err());
    assert_eq!(
        daemon
            .hits()
            .iter()
            .filter(|h| **h == "reserve_change")
            .count(),
        2
    );
    fork_position.as_mut().unwrap().delete();
    server.mock(|when, then| {
        when.method(GET).path(format!(
            "/api/v1/esplora/bitcoin-blake2b/mainnet/block/{}/txid/0",
            hash(4)
        ));
        then.status(500);
    });
    assert!(super::super::super::preferred::build_preferred(
        daemon.clone(),
        wallet.clone(),
        coins.clone(),
        5,
        window.clone(),
        connect.clone(),
        1,
        generation.clone()
    )
    .await
    .is_err());
    assert_eq!(
        daemon
            .hits()
            .iter()
            .filter(|h| **h == "reserve_change")
            .count(),
        2
    );
    sender.send_replace(2);
    let before = daemon.hits();
    assert!(super::super::super::preferred::build_preferred(
        daemon.clone(),
        wallet,
        coins,
        5,
        window,
        connect,
        1,
        generation
    )
    .await
    .is_err());
    assert_eq!(daemon.hits(), before);
}

#[tokio::test]
async fn revoked_or_refreshed_build_result_cannot_install_a_plan() {
    for refresh in [false, true] {
        let (mut flow, ready) = reach_signed().await;
        drop(ready);
        flow.p.stage = Stage::Preconditions;
        let build = flow.p.build(flow.daemon.clone());
        assert!(flow.p.building.is_some());
        let probe = if refresh {
            Some(flow.p.probe(flow.daemon.clone()))
        } else {
            flow.p.revoke();
            None
        };
        for message in outputs(build).await {
            drop(
                flow.p
                    .update(Some(flow.daemon.clone()), &flow.cache, message),
            );
        }
        assert!(matches!(flow.p.stage, Stage::Preconditions));
        assert!(flow.p.building.is_none());
        if let Some(probe) = probe {
            assert!(
                flow.p.pre.checking,
                "a stale build must not finish the newer probe"
            );
            for message in outputs(probe).await {
                drop(
                    flow.p
                        .update(Some(flow.daemon.clone()), &flow.cache, message),
                );
            }
        }
        assert!(!flow.p.pre.checking);
        assert_eq!(submissions(&flow), 0);
    }
}
