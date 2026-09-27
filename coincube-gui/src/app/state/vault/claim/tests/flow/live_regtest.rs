//! Child-process driver for disposable-node integration. Never loaded by normal builds.
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
    let mut fixture = fixture_with_multisig(false);
    emit(json!({"event":"descriptor", "descriptor":fixture.descriptor.to_string()}));
    let init = read(&mut input);
    let root = PathBuf::from(init["root"].as_str().unwrap());
    assert!(root.is_dir());
    assert!(
        root.read_dir().unwrap().next().is_none(),
        "use a fresh synthetic root"
    );
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
        submitted: Mutex::new(None),
        hits: Mutex::new(Vec::new()),
        live: Some(LiveTransport {
            transport,
            height: i32::try_from(init["tip_height"].as_u64().unwrap()).unwrap(),
        }),
    });
    let dyn_daemon: Arc<dyn Daemon + Send + Sync> = daemon.clone();
    let mut wallet = Wallet::new(fixture.descriptor);
    wallet.signer = Some(Arc::new(Signer::new(fixture.hot)));
    let wallet = Arc::new(wallet);
    let datadir = CoincubeDirectory::new(root);
    write_claim_target(&datadir, &wallet);
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
    assert_eq!(panel.refusal(), None, "{:?}", panel.pre.checked);
    assert!(panel.can_build());
    emit(json!({"event":"ready"}));
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
            json!({"event":action,"stage":stage,"tracking":tracking,"submitted":tx.map(|tx| json!({
            "txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),
            "raw":coincube_core::miniscript::bitcoin::consensus::encode::serialize_hex(&tx)})),
            "submission_calls":daemon.hits().iter().filter(|h| **h == "submit_verified_poison").count(),
            "journal":std::fs::read(&journal).ok().map(|bytes| serde_json::from_slice::<Value>(&bytes).unwrap())}),
        );
    }
}
