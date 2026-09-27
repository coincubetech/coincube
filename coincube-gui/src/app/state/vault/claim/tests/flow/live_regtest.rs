//! Child-process driver for disposable-node integration. Never loaded by normal builds.
use super::super::super::fork_panel::ForkClaimPanel;
use super::*;
use coincubed::poison_broadcast::regtest_harness::RegtestTransport;
use serde_json::Value;
use std::io::{BufRead, Write};

pub(super) struct LiveTransport {
    pub transport: RegtestTransport,
    pub height: i32,
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
            block_height: self.height,
            sync: 1.0,
            descriptors: GetInfoDescriptors {
                main: config.main_descriptor.clone(),
            },
            rescan_progress: None,
            refused_reorg_depth: None,
            chain_divergence: false,
            timestamp: 0,
            last_poll_timestamp: None,
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
        queue.extend(outputs(p.update(Some(daemon.clone()), cache, message)).await);
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
    let base = init["bridge"].as_str().unwrap();
    let config: coincubed::config::Config = toml::from_str(&format!(
        "main_descriptor = '{}'\ndata_directory = '{}'\n[bitcoin_config]\nnetwork = 'bitcoin'\n[esplora_config]\naddr = '{}/api/v1/esplora/bitcoin/mainnet'\n",
        fixture.descriptor, root.display(), base)).unwrap();
    let transport = RegtestTransport::new(
        init["rpc"].as_str().unwrap().parse().unwrap(),
        init["cookie"].as_str().unwrap(),
        ChainId::Bitcoin,
        fixture.descriptor.clone(),
    )
    .unwrap();
    let daemon = Arc::new(FlowDaemon {
        config,
        coin: fixture.coin,
        previous: fixture.previous,
        submitted: Mutex::new(recovered_transaction("bitcoin_recorded_raw")),
        hits: Mutex::new(Vec::new()),
        live: Some(LiveTransport {
            transport,
            height: i32::try_from(init["tip_height"].as_u64().unwrap()).unwrap(),
        }),
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
    if let Some(coincubed::config::BitcoinBackend::Esplora(selection)) =
        &mut fork_config.bitcoin_backend
    {
        selection.addr = format!("{base}/api/v1/esplora/bitcoin-blake2b/mainnet");
    }
    let fork_daemon = Arc::new(FlowDaemon {
        config: fork_config,
        coin: daemon.coin.clone(),
        previous: daemon.previous.clone(),
        submitted: Mutex::new(recovered_transaction("fork_recorded_raw")),
        hits: Mutex::new(Vec::new()),
        live: Some(LiveTransport {
            height: i32::try_from(init["fork_tip_height"].as_u64().unwrap()).unwrap(),
            transport: RegtestTransport::new(
                init["fork_rpc"].as_str().unwrap().parse().unwrap(),
                init["fork_cookie"].as_str().unwrap(),
                ChainId::BitcoinBlake2b,
                fork_wallet.main_descriptor.clone(),
            )
            .unwrap(),
        }),
    });
    let fork_dyn: Arc<dyn Daemon + Send + Sync> = fork_daemon.clone();
    let fork_cache = Cache {
        network: Network::Bitcoin,
        fiat_chain: ChainId::BitcoinBlake2b,
        ..Cache::default()
    };
    let mut fork_panel: Option<ForkClaimPanel> = None;
    let mut client = CoincubeClient::for_test(base.to_string());
    client.set_token("synthetic-regtest-only");
    let (_sender, generation) = watch::channel(1);
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
    if resume {
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
            "recorded Bitcoin claim must reopen for tracking: {:?}",
            panel.restart_error
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
            "confirm" => Message::View(view::Message::Claim(view::ClaimMessage::Confirm)),
            "refresh" => Message::View(view::Message::Claim(view::ClaimMessage::Refresh)),
            "quit" => break,
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
            json!({"event":action,"stage":stage,"tracking":tracking,"reconfirmation_review":panel.reconfirmation(),"submitted":tx.map(|tx| json!({
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
