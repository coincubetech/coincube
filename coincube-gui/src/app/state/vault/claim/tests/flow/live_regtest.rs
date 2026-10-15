//! Child-process driver for disposable-node integration. Never loaded by normal builds.
use super::super::super::fork_panel::ForkClaimPanel;
use super::*;
use coincubed::poison_broadcast::regtest_harness::RegtestTransport;
use serde_json::Value;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicI32, Ordering};

pub(super) struct LiveTransport {
    pub transport: RegtestTransport,
    pub height: AtomicI32,
    pub bound: Option<crate::daemon::embedded::EmbeddedDaemon>,
}
impl std::fmt::Debug for LiveTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DisposableRegtestTransport")
    }
}
impl LiveTransport {
    pub fn info(&self, config: &coincubed::config::Config) -> GetInfoResult {
        GetInfoResult {
            version: "synthetic-regtest-adapter".into(),
            network: Network::Bitcoin,
            chain_id: Some(config.bitcoin_config.chain),
            block_height: self.height.load(Ordering::SeqCst),
            sync: 1.0,
            descriptors: GetInfoDescriptors {
                main: config.main_descriptor.clone(),
            },
            rescan_progress: None,
            refused_reorg_depth: None,
            chain_divergence: false,
            timestamp: 0,
            last_poll_timestamp: None,
            history_sync: Default::default(),
            receive_index: 1,
            change_index: 0,
        }
    }
}
fn emit(value: Value) {
    println!("CLAIM_GUI_JSON:{}", value);
    std::io::stdout().flush().unwrap();
}
fn read(input: &mut impl BufRead) -> Value {
    let mut line = String::new();
    assert!(
        input.read_line(&mut line).unwrap() > 0,
        "parent closed command channel"
    );
    serde_json::from_str(&line).unwrap()
}
fn received_coin(descriptor: &CoincubeDescriptor, entry: &Value) -> (Coin, Transaction) {
    let transaction: Transaction =
        coincube_core::miniscript::bitcoin::consensus::encode::deserialize_hex(
            entry["previous"].as_str().unwrap(),
        )
        .unwrap();
    let vout = u32::try_from(entry["vout"].as_u64().unwrap()).unwrap();
    let index = u32::try_from(entry["index"].as_u64().unwrap()).unwrap();
    let derivation_index = ChildNumber::from_normal_idx(index).unwrap();
    let output = transaction.output.get(vout as usize).unwrap();
    let expected = descriptor
        .receive_descriptor()
        .derive(derivation_index, &secp256k1::Secp256k1::verification_only())
        .script_pubkey();
    assert_eq!(output.script_pubkey, expected);
    (
        Coin {
            amount: output.value,
            outpoint: OutPoint::new(transaction.compute_txid(), vout),
            address: Address::from_script(&output.script_pubkey, Network::Bitcoin).unwrap(),
            block_height: Some(i32::try_from(entry["height"].as_u64().unwrap()).unwrap()),
            derivation_index,
            spend_info: None,
            is_immature: false,
            is_change: false,
            is_from_self: false,
        },
        transaction,
    )
}

/// Inspect the actual Claim wizard element at a desktop-sized viewport. The
/// two-chain fixture supplies the live addresses, coins and signed records.
async fn rendered_labels(panel: &ClaimStep1Panel, cache: &Cache) -> Vec<String> {
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{Id, Operation, Tree},
        Layout,
    };

    #[derive(Default)]
    struct Labels(Vec<String>);
    impl Operation for Labels {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(&mut self, _: Option<&Id>, _: iced::Rectangle, value: &str) {
            self.0.push(value.to_owned());
        }
    }

    let renderer = <iced::Renderer as Headless>::new(
        iced::Font::DEFAULT,
        iced::Pixels(16.0),
        Some("tiny-skia"),
    )
    .await
    .expect("software renderer for the paired Claim wizard");
    let menu = crate::app::menu::Menu::Vault(crate::app::menu::VaultSubMenu::Claim);
    let mut element = crate::app::view::vault::claim::view(&menu, cache, panel);
    let mut tree = Tree::new(element.as_widget());
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(iced::Size::ZERO, iced::Size::new(1200.0, 2400.0)),
    );
    let mut labels = Labels::default();
    element
        .as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
    labels.0
}

fn has_label(labels: &[String], value: &str) -> bool {
    labels.iter().any(|label| label.contains(value))
}
async fn drive(
    p: &mut ClaimStep1Panel,
    daemon: &Arc<dyn Daemon + Send + Sync>,
    cache: &Cache,
    task: Task<Message>,
) {
    let mut queue = std::collections::VecDeque::from(outputs(task).await);
    let mut count = 0;
    while let Some(message) = queue.pop_front() {
        count += 1;
        assert!(count <= 50, "unexpected task loop");
        let checked_funding = matches!(&message, Message::Claim(ClaimEvent::FundingChecked(..)));
        let next = p.update(Some(daemon.clone()), cache, message);
        if checked_funding {
            // The only follow-up is the 30-second proof-expiry timer. A real
            // event loop schedules it; draining it here would expire the
            // successful proof before this driver can inspect or sign it.
            break;
        }
        queue.extend(outputs(next).await);
    }
}

/// Invoked directly by the Python integration parent with --ignored --nocapture.
/// Account/backend routing is synthetic; signing, panel transitions, journals,
/// coordinator checks, gate consumption, and node submission are real.
#[tokio::test]
#[ignore = "requires the disposable two-chain Python parent"]
async fn claim_gui_regtest_driver() {
    assert_eq!(std::env::var("CLAIM_GUI_REGTEST_CHILD").as_deref(), Ok("1"));
    let mut input = std::io::BufReader::new(std::io::stdin());
    let multisig = std::env::var("CLAIM_GUI_REGTEST_MULTISIG").as_deref() == Ok("1");
    let mut fixture = fixture_with_multisig(multisig);
    emit(json!({"event":"descriptor", "descriptor":fixture.descriptor.to_string()}));
    let init = read(&mut input);
    let root = PathBuf::from(init["root"].as_str().unwrap());
    assert!(root.is_dir());
    let resume = init["resume"].as_bool().unwrap_or(false);
    let second = if resume { None } else { fixture.second.take() };
    let fixture_marker = root.join("regtest-fixture-descriptor");
    if resume {
        assert_eq!(
            std::fs::read_to_string(&fixture_marker).unwrap(),
            fixture.descriptor.to_string(),
            "only reopen a matching synthetic test root"
        );
    } else {
        assert!(
            root.read_dir().unwrap().next().is_none(),
            "use a fresh synthetic root"
        );
        std::fs::write(&fixture_marker, fixture.descriptor.to_string()).unwrap();
    }
    let recovered_transaction = |name: &str| {
        init[name].as_str().map(|raw| {
            coincube_core::miniscript::bitcoin::consensus::encode::deserialize_hex::<Transaction>(
                raw,
            )
            .unwrap()
        })
    };
    let previous: Transaction =
        coincube_core::miniscript::bitcoin::consensus::encode::deserialize_hex(
            init["previous"].as_str().unwrap(),
        )
        .unwrap();
    let vout = u32::try_from(init["vout"].as_u64().unwrap()).unwrap();
    let output = previous.output.get(vout as usize).unwrap();
    assert_eq!(
        output.script_pubkey,
        fixture.previous.output[0].script_pubkey
    );
    fixture.coin.outpoint = OutPoint::new(previous.compute_txid(), vout);
    fixture.coin.amount = output.value;
    fixture.coin.block_height = Some(i32::try_from(init["coin_height"].as_u64().unwrap()).unwrap());
    fixture.previous = previous;
    let ancestry_coin = init["ancestry_previous"].as_str().map(|raw| {
        let transaction = coincube_core::miniscript::bitcoin::consensus::encode::deserialize_hex::<
            Transaction,
        >(raw)
        .unwrap();
        let vout = u32::try_from(init["ancestry_vout"].as_u64().unwrap()).unwrap();
        let output = transaction.output.get(vout as usize).unwrap();
        let derivation_index = ChildNumber::from_normal_idx(
            u32::try_from(init["ancestry_derivation_index"].as_u64().unwrap()).unwrap(),
        )
        .unwrap();
        let expected = fixture
            .descriptor
            .receive_descriptor()
            .derive(derivation_index, &secp256k1::Secp256k1::verification_only())
            .script_pubkey();
        assert_eq!(output.script_pubkey, expected);
        Coin {
            amount: output.value,
            outpoint: OutPoint::new(transaction.compute_txid(), vout),
            address: Address::from_script(&output.script_pubkey, Network::Bitcoin).unwrap(),
            block_height: Some(i32::try_from(init["ancestry_height"].as_u64().unwrap()).unwrap()),
            derivation_index,
            spend_info: None,
            is_immature: false,
            is_change: false,
            is_from_self: false,
        }
    });
    let shared_extra: Vec<(Coin, Transaction)> = init["shared_extra"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| received_coin(&fixture.descriptor, entry))
        .collect();
    let base = init["bridge"].as_str().unwrap();
    let mut config: coincubed::config::Config = toml::from_str(&format!(
        "main_descriptor = '{}'\ndata_directory = '{}'\n[bitcoin_config]\nnetwork = 'bitcoin'\n[esplora_config]\naddr = '{}/api/v1/esplora/bitcoin/mainnet'\n",
        fixture.descriptor, root.display(), base)).unwrap();
    let transport = RegtestTransport::new(
        init["rpc"].as_str().unwrap().parse().unwrap(),
        init["cookie"].as_str().unwrap(),
        ChainId::Bitcoin,
        fixture.descriptor.clone(),
    )
    .unwrap();
    let bound = if matches!(
        init["backend"].as_str(),
        Some("node" | "electrum" | "fallback" | "both-reject" | "fallback-timeout")
    ) {
        let (selected, handle) = transport
            .bound_handle(
                root.join(format!("bound-transport-{}", uuid::Uuid::new_v4())),
                if init["backend"].as_str() == Some("electrum") {
                    Some(init["electrum"].as_str().unwrap().parse().unwrap())
                } else {
                    None
                },
            )
            .unwrap();
        config = selected.clone();
        Some(crate::daemon::embedded::EmbeddedDaemon::from_regtest_handle(selected, handle))
    } else {
        None
    };
    let daemon = Arc::new(FlowDaemon {
        binding: coincubed::poison_broadcast::ClaimBackendBinding::for_test(&config),
        config,
        coin: fixture.coin,
        previous: fixture.previous,
        submitted: Mutex::new(recovered_transaction("bitcoin_recorded_raw")),
        hits: Mutex::new(Vec::new()),
        queried_txs: Mutex::new(Vec::new()),
        ancestry_coin,
        live: Some(LiveTransport {
            bound,
            transport,
            height: AtomicI32::new(i32::try_from(init["tip_height"].as_u64().unwrap()).unwrap()),
        }),
        extra_coins: Mutex::new(shared_extra.clone()),
        indices: Mutex::new((1, 0)),
    });
    let dyn_daemon: Arc<dyn Daemon + Send + Sync> = daemon.clone();
    let mut wallet = Wallet::new(fixture.descriptor);
    // A reopened tracking session deliberately has no signing key loaded.
    // Recovery must use the journal and independently supplied node reads.
    wallet.signer = if resume {
        None
    } else {
        Some(Arc::new(Signer::new(fixture.hot)))
    };
    let wallet = Arc::new(wallet);
    let datadir = CoincubeDirectory::new(root);
    let mut fork_wallet = (*wallet).clone();
    fork_wallet.chain = ChainId::BitcoinBlake2b;
    fork_wallet.pinned_at = Some(77);
    let fork_wallet = Arc::new(fork_wallet);
    if !resume {
        for (chain, id, vault) in [
            (ChainId::Bitcoin, "bitcoin-cube", wallet.clone()),
            (ChainId::BitcoinBlake2b, "fork-cube", fork_wallet.clone()),
        ] {
            use crate::app::settings::{update_settings_file, CubeSettings, VaultIdentity};
            let cube = CubeSettings::new_with_raw_id(id.into(), id.into(), chain)
                .with_vault(VaultIdentity::new(vault.id(), Some(&vault.main_descriptor)));
            update_settings_file(&datadir.network_directory(chain), |mut settings| {
                settings.cubes = vec![cube];
                Some(settings)
            })
            .await
            .unwrap();
        }
    }
    let mut fork_config = daemon.config.clone();
    fork_config.bitcoin_config.chain = ChainId::BitcoinBlake2b;
    fork_config.bitcoin_backend = Some(coincubed::config::BitcoinBackend::Esplora(
        toml::from_str(&format!(
            "addr = '{base}/api/v1/esplora/bitcoin-blake2b/mainnet'"
        ))
        .unwrap(),
    ));
    let fork_daemon = Arc::new(FlowDaemon {
        binding: coincubed::poison_broadcast::ClaimBackendBinding::for_test(&fork_config),
        config: fork_config,
        coin: daemon.coin.clone(),
        previous: daemon.previous.clone(),
        submitted: Mutex::new(recovered_transaction("fork_recorded_raw")),
        hits: Mutex::new(Vec::new()),
        queried_txs: Mutex::new(Vec::new()),
        ancestry_coin: None,
        live: Some(LiveTransport {
            bound: None,
            height: AtomicI32::new(
                i32::try_from(init["fork_tip_height"].as_u64().unwrap()).unwrap(),
            ),
            transport: RegtestTransport::new(
                init["fork_rpc"].as_str().unwrap().parse().unwrap(),
                init["fork_cookie"].as_str().unwrap(),
                ChainId::BitcoinBlake2b,
                fork_wallet.main_descriptor.clone(),
            )
            .unwrap(),
        }),
        extra_coins: Mutex::new(shared_extra),
        indices: Mutex::new((1, 0)),
    });
    for (source, field) in [
        (&daemon, "bitcoin_funding"),
        (&fork_daemon, "btcb2_funding"),
    ] {
        if init[field].is_object() {
            let (coin, previous) = received_coin(&wallet.main_descriptor, &init[field]);
            source.extra_coins.lock().unwrap().push((coin, previous));
        }
    }
    let fork_dyn: Arc<dyn Daemon + Send + Sync> = fork_daemon.clone();
    let fork_cache = Cache {
        network: Network::Bitcoin,
        fiat_chain: ChainId::BitcoinBlake2b,
        ..Cache::default()
    };
    let mut fork_panel: Option<ForkClaimPanel> = None;
    let mut client = CoincubeClient::for_test(base.to_string());
    client.set_token("synthetic-regtest-only");
    if init["ancestry_first_fork_height"].is_u64() {
        client.enable_regtest_ancestry(
            u32::try_from(init["ancestry_first_fork_height"].as_u64().unwrap()).unwrap(),
            u32::try_from(init["ancestry_historical_limit"].as_u64().unwrap()).unwrap(),
            init["ancestry_shared_history_height"].as_u64().unwrap(),
            init["ancestry_shared_history_hash"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap(),
        );
    }
    let (_sender, generation) = watch::channel(1);
    if init["mode"].as_str() == Some("paired-funding-proof") {
        use coincube_core::{
            claim_ancestry::{retained::RetainedPath, search::OwnedLink},
            miniscript::bitcoin::consensus::encode::{deserialize_hex, serialize},
        };
        let mut source = HttpObservationSource::new(
            client.clone(),
            ChainId::Bitcoin,
            ChainId::BitcoinBlake2b,
            CollectionContext {
                expected_generation: 1,
                generation: generation.clone(),
            },
        )
        .unwrap();
        emit(json!({"event":"paired-funding-ready"}));
        for _ in 0..5 {
            let command = read(&mut input);
            if command["action"].as_str() == Some("revoke") {
                // A same-value notification still invalidates the old
                // observation session and every retained proof it produced.
                _sender.send_modify(|_| {});
            } else if command["action"].as_str() == Some("restart") {
                // Only a new collector explicitly acknowledging the current
                // session may start a fresh two-chain read after a restart.
                let mut current = generation.clone();
                current.borrow_and_update();
                source = HttpObservationSource::new(
                    client.clone(),
                    ChainId::Bitcoin,
                    ChainId::BitcoinBlake2b,
                    CollectionContext {
                        expected_generation: 1,
                        generation: current,
                    },
                )
                .unwrap();
            }
            let path = |name: &str| {
                let transaction: Transaction =
                    deserialize_hex(command[name]["previous"].as_str().unwrap()).unwrap();
                let vout = u32::try_from(command[name]["vout"].as_u64().unwrap()).unwrap();
                RetainedPath::new(
                    OutPoint::new(transaction.compute_txid(), vout),
                    vec![OwnedLink {
                        transaction: serialize(&transaction),
                        parent_input: None,
                    }],
                )
                .unwrap()
            };
            let bitcoin_path = path("bitcoin");
            let fork_path = path("btcb2");
            let checked = source
                .collect_funding_pair(&bitcoin_path, &fork_path, CHECK_POLICY.observations)
                .await;
            emit(json!({
                "event":"paired-funding-proof",
                "valid":checked.is_ok(),
                "failure":checked.err().map(|error| format!("{error:?}")),
            }));
        }
        return;
    }
    let mut panel = ClaimStep1Panel::new(
        wallet.clone(),
        datadir.clone(),
        "bitcoin-cube".into(),
        generation,
        Some(ConnectSession {
            client,
            account: "7".into(),
        }),
    )
    .with_feerate_source(FeerateSource::Fixed(5));
    let cache = Cache {
        network: Network::Bitcoin,
        fiat_chain: ChainId::Bitcoin,
        ..Cache::default()
    };
    let task = panel.reload(Some(dyn_daemon.clone()), Some(wallet));
    drive(&mut panel, &dyn_daemon, &cache, task).await;
    if init["mode"].as_str() == Some("paired-wizard-reopen") {
        assert!(resume, "a signed pair must reopen from its saved journal");
        let Stage::PairedRecovery(recovery) = &panel.stage else {
            panic!("signed pair did not reopen: {:?}", panel.restart_error);
        };
        let recovery_labels = rendered_labels(&panel, &cache).await;
        assert!(has_label(&recovery_labels, "Recorded two-chain claim"));
        assert!(has_label(&recovery_labels, "Bitcoin transaction"));
        assert!(has_label(&recovery_labels, "Bitcoin Blake2b transaction"));
        assert!(has_label(&recovery_labels, "Check again"));
        assert!(!has_label(&recovery_labels, "Broadcast Transaction"));
        assert_eq!(
            recovery.bitcoin_txid.unwrap().to_string(),
            init["bitcoin_txid"].as_str().unwrap()
        );
        assert_eq!(
            recovery.fork_txid.unwrap().to_string(),
            init["btcb2_txid"].as_str().unwrap()
        );
        assert!(panel.restart_error.is_none());
        assert!(!panel.can_sign_or_submit());
        let recovery = recovery.clone();
        use coincube_core::miniscript::bitcoin::hashes::{sha256, Hash as _};
        let context = ClaimStep1Panel::funding_context(
            panel.connect.as_ref().unwrap(),
            1,
            panel.generation.clone(),
        )
        .unwrap();
        let identity = claim_workflow::WalletIdentity {
            bitcoin_cube: "bitcoin-cube".into(),
            fork_cube: "fork-cube".into(),
            descriptor_digest: sha256::Hash::hash(
                panel.wallet.main_descriptor.to_string().as_bytes(),
            ),
        };
        let directory = journal_directory(&datadir, &panel.wallet);
        let recorded = claim_workflow::Controller::reopen_settling(&directory, &identity, context)
            .await
            .unwrap();
        let pair = recorded.recorded_signed_ancestry_pair().unwrap();
        let bitcoin_raw =
            coincube_core::miniscript::bitcoin::consensus::encode::serialize_hex(pair.bitcoin());
        let btcb2_raw =
            coincube_core::miniscript::bitcoin::consensus::encode::serialize_hex(pair.fork());
        drop(recorded);
        if init["broadcast_bitcoin"].as_bool().unwrap_or(false)
            || init["broadcast_btcb2"].as_bool().unwrap_or(false)
        {
            crate::services::claim_ancestry_gate::enable_disposable_regtest_acceptance();
            panel.bind_fork_daemon(Some(ClaimForkBinding::new(
                &fork_dyn,
                &fork_wallet,
                _sender.subscribe(),
            )));
            if init["broadcast_bitcoin"].as_bool().unwrap_or(false) {
                let task = panel.update(
                    Some(dyn_daemon.clone()),
                    &cache,
                    Message::View(view::Message::Claim(
                        view::ClaimMessage::ReviewPairedBitcoin,
                    )),
                );
                drive(&mut panel, &dyn_daemon, &cache, task).await;
                assert!(
                    matches!(panel.stage, Stage::PairedBitcoinReview(..)),
                    "{:?}",
                    panel.restart_error
                );
                let review_labels = rendered_labels(&panel, &cache).await;
                assert!(has_label(&review_labels, "Bitcoin — broadcast now"));
                assert!(has_label(
                    &review_labels,
                    "Bitcoin Blake2b — recorded, not sent now"
                ));
                assert!(has_label(&review_labels, "Broadcast Transaction"));
                assert!(has_label(
                    &review_labels,
                    &recovery.bitcoin_txid.unwrap().to_string()
                ));
                let saved_script = match &mut panel.stage {
                    Stage::PairedBitcoinReview(_, review) => {
                        std::mem::take(&mut review.fork.output[0].script_pubkey)
                    }
                    _ => unreachable!(),
                };
                let invalid_labels = rendered_labels(&panel, &cache).await;
                assert!(has_label(
                    &invalid_labels,
                    "Cannot display both signed destinations"
                ));
                assert!(!has_label(&invalid_labels, "Broadcast Transaction"));
                if let Stage::PairedBitcoinReview(_, review) = &mut panel.stage {
                    review.fork.output[0].script_pubkey = saved_script;
                }
                let task = panel.update(
                    Some(dyn_daemon.clone()),
                    &cache,
                    Message::View(view::Message::Claim(
                        view::ClaimMessage::ConfirmPairedBitcoin,
                    )),
                );
                drive(&mut panel, &dyn_daemon, &cache, task).await;
                assert!(
                    matches!(panel.stage, Stage::PairedRecovery(..)),
                    "{:?}",
                    panel.restart_error
                );
                let recorded = claim_workflow::Controller::reopen_settling(
                    &directory,
                    &identity,
                    ClaimStep1Panel::funding_context(
                        panel.connect.as_ref().unwrap(),
                        1,
                        panel.generation.clone(),
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
                let attempt = recorded.recorded_signed_ancestry_pair().unwrap();
                assert!(attempt.possible_attempt(ChainId::Bitcoin).is_some());
                assert!(attempt.possible_attempt(ChainId::BitcoinBlake2b).is_none());
                emit(json!({
                    "event":"claim-bitcoin-broadcast-recorded",
                    "bitcoin_txid":recovery.bitcoin_txid,
                    "notice":panel.restart_error,
                }));
                return;
            }
            assert!(recovery.fork_ready_for_review, "{:?}", recovery);
            let task = panel.update(
                Some(dyn_daemon.clone()),
                &cache,
                Message::View(view::Message::Claim(view::ClaimMessage::ReviewPairedFork)),
            );
            drive(&mut panel, &dyn_daemon, &cache, task).await;
            assert!(
                matches!(panel.stage, Stage::PairedForkReview(..)),
                "{:?}",
                panel.restart_error
            );
            let review_labels = rendered_labels(&panel, &cache).await;
            assert!(has_label(&review_labels, "Bitcoin Blake2b — broadcast now"));
            assert!(has_label(
                &review_labels,
                "Bitcoin — recorded and confirmed"
            ));
            assert!(has_label(&review_labels, "Broadcast Transaction"));
            assert!(has_label(
                &review_labels,
                &recovery.fork_txid.unwrap().to_string()
            ));
            if init["pause_before_fork_confirm"].as_bool().unwrap_or(false) {
                emit(json!({"event":"claim-btcb2-reviewed"}));
                assert_eq!(read(&mut input)["action"], "confirm_after_reorg");
                let task = panel.update(
                    Some(dyn_daemon.clone()),
                    &cache,
                    Message::View(view::Message::Claim(view::ClaimMessage::ConfirmPairedFork)),
                );
                drive(&mut panel, &dyn_daemon, &cache, task).await;
                assert!(matches!(panel.stage, Stage::PairedRecovery(..)));
                assert!(panel.restart_error.is_some());
                let recorded = claim_workflow::Controller::reopen_settling(
                    &directory,
                    &identity,
                    ClaimStep1Panel::funding_context(
                        panel.connect.as_ref().unwrap(),
                        1,
                        panel.generation.clone(),
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
                assert!(recorded
                    .recorded_signed_ancestry_pair()
                    .unwrap()
                    .possible_attempt(ChainId::BitcoinBlake2b)
                    .is_none());
                emit(json!({
                    "event":"claim-btcb2-reorg-refused",
                    "reason":panel.restart_error,
                }));
                return;
            }
            let task = panel.update(
                Some(dyn_daemon.clone()),
                &cache,
                Message::View(view::Message::Claim(view::ClaimMessage::ConfirmPairedFork)),
            );
            drive(&mut panel, &dyn_daemon, &cache, task).await;
            assert!(
                matches!(panel.stage, Stage::PairedRecovery(..)),
                "{:?}",
                panel.restart_error
            );
            let recorded = claim_workflow::Controller::reopen_settling(
                &directory,
                &identity,
                ClaimStep1Panel::funding_context(
                    panel.connect.as_ref().unwrap(),
                    1,
                    panel.generation.clone(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
            let attempt = recorded.recorded_signed_ancestry_pair().unwrap();
            assert!(attempt.possible_attempt(ChainId::Bitcoin).is_some());
            assert!(attempt.possible_attempt(ChainId::BitcoinBlake2b).is_some());
            emit(json!({
                "event":"claim-btcb2-broadcast-recorded",
                "btcb2_txid":recovery.fork_txid,
                "notice":panel.restart_error,
            }));
            return;
        }
        emit(json!({
            "event":"paired-signed-pair-reopened",
            "bitcoin_unattempted":recovery.bitcoin_unattempted,
            "bitcoin_status":recovery.bitcoin_status,
            "btcb2_status":recovery.fork_status,
            "bitcoin_raw":bitcoin_raw,
            "btcb2_raw":btcb2_raw,
        }));
        return;
    }
    if init["mode"].as_str() == Some("paired-wizard-deposits") {
        // The fixture supplies a checked precondition so this test targets
        // the real two-chain reservation and address-preparation transition.
        // It does not manufacture a signer or submission authorization.
        panel.pre.target = Some("fork-cube".into());
        panel.pre.shape = None;
        panel.pre.checked = Some(checked_ok(unix_now()));
        panel.backend = BackendState::Ready;
        panel.bind_fork_daemon(Some(ClaimForkBinding::new(
            &fork_dyn,
            &fork_wallet,
            _sender.subscribe(),
        )));
        assert!(panel.can_prepare_funding());
        let task = panel.update(
            Some(dyn_daemon.clone()),
            &cache,
            Message::View(view::Message::Claim(view::ClaimMessage::StartFunding)),
        );
        drive(&mut panel, &dyn_daemon, &cache, task).await;
        let Stage::FundingDeposits(addresses) = &panel.stage else {
            panic!(
                "paired deposit preparation failed: {:?}",
                panel.funding_error
            );
        };
        assert_ne!(addresses.bitcoin_deposit, addresses.btcb2_deposit);
        assert!(!panel.can_sign_or_submit());
        let deposit_labels = rendered_labels(&panel, &cache).await;
        assert!(has_label(
            &deposit_labels,
            "Prepare Bitcoin and Bitcoin Blake2b deposits"
        ));
        assert!(has_label(&deposit_labels, &addresses.bitcoin_deposit));
        assert!(has_label(&deposit_labels, &addresses.btcb2_deposit));
        assert!(has_label(&deposit_labels, "Check deposits"));
        assert!(!has_label(&deposit_labels, "Broadcast Transaction"));
        use coincube_core::miniscript::bitcoin::hashes::{sha256, Hash as _};
        let descriptor = panel.wallet.main_descriptor.clone();
        let identity = claim_workflow::WalletIdentity {
            bitcoin_cube: "bitcoin-cube".into(),
            fork_cube: "fork-cube".into(),
            descriptor_digest: sha256::Hash::hash(descriptor.to_string().as_bytes()),
        };
        let context = ClaimStep1Panel::funding_context(
            panel.connect.as_ref().unwrap(),
            1,
            panel.generation.clone(),
        )
        .unwrap();
        let saved = claim_workflow::load_claim_funding(
            &journal_directory(&datadir, &panel.wallet),
            &descriptor,
            &identity,
            &context,
        )
        .unwrap()
        .unwrap();
        assert_eq!(serde_json::to_value(&saved).unwrap()["released"], true);
        emit(json!({
            "event":"paired-deposit-addresses",
            "bitcoin":addresses.bitcoin_deposit,
            "btcb2":addresses.btcb2_deposit,
            "bitcoin_minimum":addresses.bitcoin_minimum_sats,
            "btcb2_minimum":addresses.btcb2_minimum_sats,
            "bitcoin_index":saved.indices.bitcoin_deposit,
            "btcb2_index":saved.indices.btcb2_deposit,
        }));
        let command = read(&mut input);
        if command["action"] == "quit" {
            return;
        }
        assert_eq!(command["action"], "deposited");
        for (daemon, field) in [(&daemon, "bitcoin"), (&fork_daemon, "btcb2")] {
            let entry = &command[field];
            let (coin, previous) = received_coin(&descriptor, entry);
            daemon.extra_coins.lock().unwrap().push((coin, previous));
            daemon.live.as_ref().unwrap().height.store(
                i32::try_from(entry["tip_height"].as_u64().unwrap()).unwrap(),
                Ordering::SeqCst,
            );
        }
        let task = panel.update(
            Some(dyn_daemon.clone()),
            &cache,
            Message::View(view::Message::Claim(view::ClaimMessage::CheckFunding)),
        );
        drive(&mut panel, &dyn_daemon, &cache, task).await;
        let verified = panel.funding_review().is_some() && panel.prepared_pair.is_some();
        assert!(matches!(panel.stage, Stage::FundingDeposits(_)));
        assert!(
            !panel.can_sign_or_submit(),
            "the production signer gate remains closed"
        );
        assert!(verified, "paired Claim funding preview was not verified");
        let preview_labels = rendered_labels(&panel, &cache).await;
        assert!(has_label(
            &preview_labels,
            "Paired transaction preview — read only"
        ));
        assert!(has_label(&preview_labels, "Bitcoin-only funding coin"));
        assert!(has_label(&preview_labels, "BTCB2-only funding coin"));
        assert!(has_label(&preview_labels, "Before fork"));
        assert!(!has_label(&preview_labels, "OP_RETURN"));
        let review = panel.funding_review().unwrap();
        for row in review
            .bitcoin
            .shared
            .iter()
            .chain([&review.bitcoin.funding, &review.fork.funding])
        {
            assert!(has_label(&preview_labels, &row.outpoint.to_string()));
            assert!(has_label(
                &preview_labels,
                &format!("{} sats", row.amount.to_sat())
            ));
        }
        let destination = |output: &TxOut| {
            Address::from_script(&output.script_pubkey, Network::Bitcoin)
                .unwrap()
                .to_string()
        };
        assert!(has_label(
            &preview_labels,
            &format!(
                "Back to this Vault: {} · {} sats",
                destination(&review.bitcoin.destination),
                review.bitcoin.destination.value.to_sat()
            )
        ));
        assert!(has_label(
            &preview_labels,
            &format!(
                "Back to the paired BTCB2 Vault: {} · {} sats",
                destination(&review.fork.destination),
                review.fork.destination.value.to_sat()
            )
        ));
        assert!(has_label(
            &preview_labels,
            &format!("Fee: {} sats", review.bitcoin.fee.to_sat())
        ));
        assert!(has_label(
            &preview_labels,
            &format!("Fee: {} sats", review.fork.fee.to_sat())
        ));
        emit(json!({
            "event":"paired-deposits-checked",
            "verified":verified,
            "status":panel.funding_status,
            "error":panel.funding_error,
        }));
        assert_eq!(read(&mut input)["action"], "sign_pair_stale");
        crate::services::claim_ancestry_gate::enable_disposable_regtest_acceptance();
        let task = panel.update(
            Some(dyn_daemon.clone()),
            &cache,
            Message::View(view::Message::Claim(view::ClaimMessage::SignPair)),
        );
        drive(&mut panel, &dyn_daemon, &cache, task).await;
        assert!(matches!(panel.stage, Stage::FundingDeposits(_)));
        assert!(panel.funding_error.is_some());
        emit(json!({
            "event":"paired-signing-refused-stale",
            "error":panel.funding_error,
        }));
        assert_eq!(read(&mut input)["action"], "deposited");
        let task = panel.update(
            Some(dyn_daemon.clone()),
            &cache,
            Message::View(view::Message::Claim(view::ClaimMessage::CheckFunding)),
        );
        drive(&mut panel, &dyn_daemon, &cache, task).await;
        assert!(panel.funding_review().is_some() && panel.prepared_pair.is_some());
        assert!(panel.funding_error.is_none());
        emit(json!({"event":"paired-deposits-checked-after-stale"}));
        let command = read(&mut input);
        if command["action"] == "quit" {
            return;
        }
        assert_eq!(command["action"], "sign_pair");
        let task = panel.update(
            Some(dyn_daemon.clone()),
            &cache,
            Message::View(view::Message::Claim(view::ClaimMessage::SignPair)),
        );
        drive(&mut panel, &dyn_daemon, &cache, task).await;
        let signer = panel.wallet.signer.as_ref().unwrap().clone();
        let (bitcoin_psbt, fork_psbt) = match &panel.stage {
            Stage::PairedSign { bitcoin, fork, .. } => {
                (bitcoin.tx.psbt.clone(), fork.tx.psbt.clone())
            }
            _ => panic!("paired signer admission failed: {:?}", panel.funding_error),
        };
        let signing_labels = rendered_labels(&panel, &cache).await;
        assert!(has_label(
            &signing_labels,
            "Sign both ordinary Claim transactions"
        ));
        assert!(has_label(&signing_labels, "Bitcoin transaction"));
        assert!(has_label(&signing_labels, "Bitcoin Blake2b transaction"));
        assert!(!has_label(&signing_labels, "OP_RETURN"));
        let signed_bitcoin = signer.sign_psbt(bitcoin_psbt.clone()).unwrap();
        let signed_fork = signer.sign_psbt(fork_psbt.clone()).unwrap();
        emit(json!({
            "event":"paired-signing-ready",
            "bitcoin_unsigned":bitcoin_psbt.to_string(),
            "btcb2_unsigned":fork_psbt.to_string(),
            "bitcoin_signed":signed_bitcoin.to_string(),
            "btcb2_signed":signed_fork.to_string(),
        }));
        let returned = read(&mut input);
        assert_eq!(returned["action"], "return_signatures");
        for (side, field) in [
            (ChainId::Bitcoin, "bitcoin_signed"),
            (ChainId::BitcoinBlake2b, "btcb2_signed"),
        ] {
            let psbt = returned[field].as_str().unwrap().parse::<Psbt>().unwrap();
            let task = panel.update(
                Some(dyn_daemon.clone()),
                &cache,
                Message::Claim(ClaimEvent::PairedPsbt(
                    panel.paired_sign_seq,
                    side,
                    Box::new(Message::Export(
                        crate::export::ImportExportMessage::Progress(
                            crate::export::Progress::Psbt(psbt),
                        ),
                    )),
                )),
            );
            drive(&mut panel, &dyn_daemon, &cache, task).await;
        }
        let (bitcoin_txid, btcb2_txid) = match &panel.stage {
            Stage::PairedRecovery(recovery) => (recovery.bitcoin_txid, recovery.fork_txid),
            Stage::PairedSign { error, .. } => panic!("paired signatures refused: {:?}", error),
            _ => panic!("paired signing did not record the exact pair"),
        };
        emit(json!({
            "event":"paired-signatures-recorded",
            "bitcoin_txid":bitcoin_txid,
            "btcb2_txid":btcb2_txid,
        }));
        let command = read(&mut input);
        if command["action"] == "recheck_stale" || command["action"] == "recheck_spent" {
            let task = panel.update(
                Some(dyn_daemon.clone()),
                &cache,
                Message::View(view::Message::Claim(
                    view::ClaimMessage::ReviewPairedBitcoin,
                )),
            );
            drive(&mut panel, &dyn_daemon, &cache, task).await;
            assert!(matches!(panel.stage, Stage::PairedRecovery(_)));
            assert!(panel.restart_error.is_some());
            emit(json!({
                "event":if command["action"] == "recheck_spent" {
                    "paired-review-refused-spent"
                } else {
                    "paired-review-refused-stale"
                },
                "reason":panel.restart_error,
            }));
            if command["action"] == "recheck_spent" {
                let next = read(&mut input);
                assert_eq!(next["action"], "recheck_stale");
                let task = panel.update(
                    Some(dyn_daemon.clone()),
                    &cache,
                    Message::View(view::Message::Claim(
                        view::ClaimMessage::ReviewPairedBitcoin,
                    )),
                );
                drive(&mut panel, &dyn_daemon, &cache, task).await;
                assert!(matches!(panel.stage, Stage::PairedRecovery(_)));
                assert!(panel.restart_error.is_some());
                emit(json!({
                    "event":"paired-review-refused-stale",
                    "reason":panel.restart_error,
                }));
            }
            assert_eq!(read(&mut input)["action"], "quit");
        } else {
            assert_eq!(command["action"], "quit");
        }
        return;
    }
    if !resume {
        assert!(
            panel.refusal().is_some(),
            "retired one-sided claim routes must refuse"
        );
        assert!(!panel.can_build());
        assert!(!daemon.hits().contains(&"submit_verified_poison"));
        emit(json!({"event":"legacy-claim-refused"}));
        return;
    }
    if resume {
        let restart_state = match &panel.stage {
            Stage::Track {
                session,
                status,
                busy,
                error,
                ..
            } => format!(
                "track(session={}, status={status:?}, busy={busy}, error={error:?})",
                session.is_some()
            ),
            Stage::Preconditions => "preconditions".into(),
            Stage::Plan { .. } => "plan".into(),
            Stage::CheckingSign(_) => "checking-sign".into(),
            Stage::Sign { .. } => "sign".into(),
            Stage::Review { .. } => "review".into(),
            _ => format!("unexpected({:?})", std::mem::discriminant(&panel.stage)),
        };
        assert!(
            matches!(
                &panel.stage,
                Stage::Track {
                    session: Some(_),
                    busy: false,
                    error: None,
                    ..
                }
            ),
            "recorded Bitcoin claim must reopen for tracking: restart_error={:?}, state={restart_state}",
            panel.restart_error,
        );
        assert!(!panel.can_build());
        assert!(!daemon.hits().contains(&"reserve_change"));
    } else {
        assert_eq!(panel.refusal(), None, "{:?}", panel.pre.checked);
        assert!(panel.can_build());
    }
    emit(json!({"event":"ready", "resumed":resume,
        "signer_available":panel.wallet.signer.is_some(),
        "bitcoin_submission_calls":daemon.hits().iter().filter(|h| **h == "submit_verified_poison").count(),
        "fork_submission_calls":fork_daemon.hits().iter().filter(|h| **h == "submit_verified_claim_fork").count()}));
    loop {
        let cmd = read(&mut input);
        let action = cmd["command"].as_str().unwrap();
        let message = match action {
            "build" => Message::View(view::Message::Claim(view::ClaimMessage::Build)),
            "sign" => Message::View(view::Message::Claim(view::ClaimMessage::Sign)),
            "open_signer" => Message::View(view::Message::Spend(view::SpendTxMessage::Sign)),
            "hot_sign" => Message::View(view::Message::Spend(
                view::SpendTxMessage::SelectMasterSigner,
            )),
            "second_sign" => {
                let second = second.as_ref().expect("second software signer configured");
                let psbt = match &panel.stage {
                    Stage::Sign { psbt, .. } => psbt.tx.psbt.clone(),
                    _ => panic!("second signature must start at Sign"),
                };
                let secp = secp256k1::Secp256k1::new();
                Message::Signed(
                    second.fingerprint(&secp),
                    Ok(second.sign_psbt(psbt, &secp).unwrap()),
                )
            }
            "review_reconfirmation" => Message::View(view::Message::Claim(
                view::ClaimMessage::ReviewReconfirmation,
            )),
            "confirm_reconfirmation" => Message::View(view::Message::Claim(
                view::ClaimMessage::ConfirmReconfirmation,
            )),
            "fork_refuse_early" => {
                let journal = journal_directory(&datadir, &panel.wallet).join("intent.json");
                let before = std::fs::read(&journal).unwrap();
                let error = panel
                    .take_fork_handoff()
                    .expect_err("early handoff must refuse");
                assert_eq!(std::fs::read(journal).unwrap(), before);
                assert!(fork_panel.is_none());
                emit(json!({"event":action,"error":error,
                    "bitcoin_submission_calls":daemon.hits().iter().filter(|h| **h == "submit_verified_poison").count(),
                    "fork_submission_calls":fork_daemon.hits().iter().filter(|h| **h == "submit_verified_claim_fork").count()}));
                continue;
            }
            "fork_open" => {
                let handoff = panel.take_fork_handoff().unwrap();
                assert_eq!(handoff.fork_cube(), "fork-cube");
                let loaded = fork_load::load(
                    &datadir,
                    fork_wallet.clone(),
                    fork_dyn.clone(),
                    panel.connect.clone().unwrap(),
                    "bitcoin-cube",
                    "fork-cube",
                    1,
                    _sender.subscribe(),
                    5,
                )
                .await
                .unwrap();
                fork_panel = Some(
                    ForkClaimPanel::new(
                        datadir.clone(),
                        fork_wallet.clone(),
                        vec![fork_daemon.coin.clone()],
                        loaded,
                    )
                    .unwrap(),
                );
                emit_fork(
                    action,
                    fork_panel.as_ref().unwrap(),
                    &fork_daemon,
                    &datadir,
                    &panel.wallet,
                );
                continue;
            }
            "fork_open_signer" | "fork_hot_sign" | "fork_second_sign" | "fork_confirm"
            | "fork_refresh" => {
                let p = fork_panel.as_mut().unwrap();
                let message = match action {
                    "fork_open_signer" => {
                        Message::View(view::Message::Spend(view::SpendTxMessage::Sign))
                    }
                    "fork_hot_sign" => Message::View(view::Message::Spend(
                        view::SpendTxMessage::SelectMasterSigner,
                    )),
                    "fork_second_sign" => {
                        let second = second.as_ref().expect("second software signer configured");
                        let secp = secp256k1::Secp256k1::new();
                        let psbt = fork_panel::tests::live_psbt(p);
                        Message::Signed(
                            second.fingerprint(&secp),
                            Ok(second.sign_psbt(psbt, &secp).unwrap()),
                        )
                    }
                    "fork_confirm" => {
                        Message::View(view::Message::Claim(view::ClaimMessage::Confirm))
                    }
                    _ => Message::View(view::Message::Claim(view::ClaimMessage::Refresh)),
                };
                let task = p.update(Some(fork_dyn.clone()), &fork_cache, message);
                drive_fork(p, &fork_dyn, &fork_cache, task).await;
                emit_fork(action, p, &fork_daemon, &datadir, &panel.wallet);
                continue;
            }
            "bitcoin_return" => {
                assert!(fork_panel.as_ref().unwrap().can_return_to_bitcoin());
                drop(fork_panel.take());
                panel.return_from_fork();
                let task = panel.reload(Some(dyn_daemon.clone()), None);
                drive(&mut panel, &dyn_daemon, &cache, task).await;
                Message::View(view::Message::Claim(view::ClaimMessage::Refresh))
            }
            "replace_backend" => {
                let live = daemon.live.as_ref().unwrap();
                let bound = live.bound.as_ref().unwrap();
                let before = bound.claim_backend_binding().await.unwrap();
                let (_, replacement) = live
                    .transport
                    .bound_handle(
                        PathBuf::from(init["root"].as_str().unwrap())
                            .join(format!("replacement-{}", uuid::Uuid::new_v4())),
                        if init["backend"].as_str() == Some("electrum") {
                            Some(init["electrum"].as_str().unwrap().parse().unwrap())
                        } else {
                            None
                        },
                    )
                    .unwrap();
                bound.replace_regtest_handle(replacement).await;
                assert_ne!(before, bound.claim_backend_binding().await.unwrap());
                emit(json!({"event":action,"binding_changed":true}));
                continue;
            }
            "confirm" => Message::View(view::Message::Claim(view::ClaimMessage::Confirm)),
            "refresh" => Message::View(view::Message::Claim(view::ClaimMessage::Refresh)),
            "quit" => {
                if let Some(bound) = daemon.live.as_ref().and_then(|live| live.bound.as_ref()) {
                    bound.stop().await.unwrap();
                }
                break;
            }
            _ => panic!("unknown command"),
        };
        let task = panel.update(Some(dyn_daemon.clone()), &cache, message);
        drive(&mut panel, &dyn_daemon, &cache, task).await;
        let stage = match &panel.stage {
            Stage::Plan { .. } => "plan",
            Stage::Sign { .. } => "sign",
            Stage::Review { .. } => "review",
            Stage::Track { .. } => "track",
            _ => "preconditions",
        };
        let (construction, selected_ancestry_input) = match &panel.stage {
            Stage::Plan { built } => match &**built {
                Construction::OpReturn(_) => (Some("op-return"), None),
                Construction::Ancestry { .. } => (
                    Some("input-ancestry"),
                    built
                        .selected_ancestry_input()
                        .map(|outpoint| outpoint.to_string()),
                ),
            },
            _ => (None, None),
        };
        let reviewed_route = match &panel.stage {
            Stage::Review {
                snapshot: Some(snapshot),
                ..
            } => Some(snapshot.route.label()),
            _ => None,
        };
        let review_error = match &panel.stage {
            Stage::Review { error, .. } => error.clone(),
            _ => None,
        };
        let submission_outcome = match &panel.stage {
            Stage::Track { outcome, .. } => Some(format!("{outcome:?}")),
            _ => None,
        };
        let tracking = match &panel.stage {
            Stage::Track {
                status,
                busy,
                error,
                ..
            } => Some(json!({"status":format!("{status:?}"),"busy":busy,"error":error})),
            _ => None,
        };
        let tx = daemon.submitted.lock().unwrap().clone();
        let journal = journal_directory(&datadir, &panel.wallet).join("intent.json");
        emit(
            json!({"event":action,"stage":stage,"construction":construction,
            "selected_ancestry_input":selected_ancestry_input,
            "reviewed_route":reviewed_route,"review_error":review_error,
            "node_submissions":daemon.hits().iter().filter(|h| **h == "submit_verified_poison_to_node").count(),
            "connect_submissions":daemon.hits().iter().filter(|h| **h == "submit_verified_poison_to_connect").count(),
            "tracking":tracking,"submission_outcome":submission_outcome,"reconfirmation_review":panel.reconfirmation(),"submitted":tx.map(|tx| json!({
            "txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),
            "raw":coincube_core::miniscript::bitcoin::consensus::encode::serialize_hex(&tx)})),
            "submission_calls":daemon.hits().iter().filter(|h| **h == "submit_verified_poison").count(),
            "journal":std::fs::read(&journal).ok().map(|bytes| serde_json::from_slice::<Value>(&bytes).unwrap())}),
        );
    }
}

async fn drive_fork(
    p: &mut ForkClaimPanel,
    daemon: &Arc<dyn Daemon + Send + Sync>,
    cache: &Cache,
    task: Task<Message>,
) {
    let mut queue = std::collections::VecDeque::from(outputs(task).await);
    let mut count = 0;
    while let Some(message) = queue.pop_front() {
        count += 1;
        assert!(count <= 50);
        queue.extend(outputs(p.update(Some(daemon.clone()), cache, message)).await);
    }
}
fn emit_fork(
    action: &str,
    panel: &ForkClaimPanel,
    daemon: &FlowDaemon,
    root: &CoincubeDirectory,
    source: &Wallet,
) {
    let tx = daemon.submitted.lock().unwrap().clone();
    let settings: Vec<Value> = [ChainId::Bitcoin, ChainId::BitcoinBlake2b]
        .iter()
        .copied()
        .map(|chain| {
            let path = root
                .network_directory(chain)
                .path()
                .join(crate::app::settings::SETTINGS_FILE_NAME);
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
        })
        .collect();
    emit(
        json!({"event":action,"fork":fork_panel::tests::live_snapshot(panel),"settings":settings,
        "submission_calls":daemon.hits().iter().filter(|h| **h == "submit_verified_claim_fork").count(),
        "submitted":tx.map(|tx| json!({"txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),
            "raw":coincube_core::miniscript::bitcoin::consensus::encode::serialize_hex(&tx)})),
        "journal":serde_json::from_slice::<Value>(&std::fs::read(journal_directory(root,source).join("intent.json")).unwrap()).unwrap()}),
    );
}
