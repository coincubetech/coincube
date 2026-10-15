//! Headless Split panel driver for the disposable two-chain Python fixture.
//! The production Connect adapter performs every source scan and proof read.

use super::*;
use crate::{
    app::{settings::WalletId, split_intent::SplitIntent},
    services::{
        claim_workflow::SplitTargetBinding,
        foreign_scan::{BranchRange, ScanPlan},
        foreign_split_inventory,
    },
};
use coincube_core::miniscript::bitcoin::Psbt;
use serde_json::{json, Value};
use std::{
    convert::TryFrom,
    io::{BufRead, Write},
    str::FromStr,
};
use tokio::sync::watch;

fn emit(value: Value) {
    println!("SPLIT_GUI_JSON:{value}");
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

async fn drive_panel(panel: &mut SplitPanel, task: Task<Message>, preserve_preview: bool) {
    let mut pending = vec![task];
    let mut count = 0;
    while let Some(task) = pending.pop() {
        for event in events(task).await {
            count += 1;
            assert!(count < 60, "unexpected Split panel task loop");
            let checked = matches!(event, SplitEvent::FundingChecked(..));
            let next = panel.apply(event);
            // A live UI schedules the preview-expiry task for later. Draining
            // it here would wait 30 seconds and erase the proof before sign.
            if !(preserve_preview && checked) {
                pending.push(next);
            }
        }
    }
}

fn source_plan(wallet: &fixture::Wallet) -> ScanPlan {
    let range = |descriptor: &crate::services::foreign_scan::ScanDescriptor| BranchRange {
        descriptor: descriptor.clone(),
        start: 0,
        end_exclusive: 100,
    };
    ScanPlan {
        chain: ChainId::BitcoinBlake2b,
        branches: vec![range(&wallet.external), range(&wallet.internal)],
        gap: 20,
    }
}

fn sign(psbt: &Psbt, keys: &[coincube_core::miniscript::bitcoin::bip32::Xpriv]) -> Psbt {
    let mut signed = psbt.clone();
    let secp = Secp256k1::new();
    for key in keys {
        signed.sign(key, &secp).unwrap();
    }
    signed
}

#[tokio::test]
#[ignore = "requires the disposable two-chain Python parent"]
async fn split_gui_regtest_driver() {
    assert_eq!(std::env::var("SPLIT_GUI_REGTEST_CHILD").as_deref(), Ok("1"));
    let mut input = std::io::BufReader::new(std::io::stdin());
    let wallet = fixture::wallet(Shape::Wpkh);
    let source_text = wallet.external.descriptor().to_string();
    let public_descriptor = source_text
        .split('#')
        .next()
        .unwrap()
        .replace("/0/*", "/<0;1>/*");
    assert_ne!(public_descriptor, source_text);
    emit(json!({"event":"descriptor", "descriptor":public_descriptor}));

    let init = read(&mut input);
    let root = PathBuf::from(init["root"].as_str().unwrap());
    assert!(root.is_dir());
    let mut client = CoincubeClient::for_test(init["bridge"].as_str().unwrap().to_owned());
    client.set_token("synthetic-regtest-only");
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
    let (_sender, generation) = watch::channel(1u64);
    let scanned =
        foreign_split_inventory::scan(client.clone(), source_plan(&wallet), 1, generation.clone())
            .await
            .expect("initial live two-chain source scan");
    let intent = SplitIntent::new(
        TARGET.into(),
        ChainId::BitcoinBlake2b,
        1,
        &client,
        scanned,
        wallet.external.clone(),
        Some(wallet.internal.clone()),
    )
    .expect("current Split intent");
    let descriptor =
        CoincubeDescriptor::from_str(crate::services::claim_workflow::TEST_WSH_DESC).unwrap();
    let wallet_id = WalletId::new(
        descriptor.to_string().split_once('#').unwrap().1.to_owned(),
        Some(1),
    );
    let binding = SplitTargetBinding::for_test(wallet_id.clone(), &descriptor, 4);
    let session = crate::app::state::vault::claim::ConnectSession {
        client,
        account: "synthetic-regtest-account".into(),
    };
    let connect: Arc<dyn SplitConnect> =
        Arc::new(step1::ProductionConnect::new(session, generation).unwrap());
    let mut panel = SplitPanel::start(TARGET.into(), root.join("split"), intent);
    panel.bind_funding_target(binding);
    panel.bind_target_vault(wallet_id, descriptor);
    panel.set_connect(Some(connect.clone()));
    let task = panel.begin();
    drive_panel(&mut panel, task, false).await;
    assert_eq!(
        panel.stage(),
        &Stage::FundingDeposits,
        "{:?}",
        panel.notice()
    );
    let addresses = panel.funding_addresses().unwrap();
    emit(json!({
        "event":"split-deposit-addresses",
        "bitcoin":addresses.bitcoin_deposit,
        "btcb2":addresses.btcb2_deposit,
        "bitcoin_minimum":addresses.fee_quote.bitcoin_minimum_sats,
        "btcb2_minimum":addresses.fee_quote.btcb2_minimum_sats,
    }));

    assert_eq!(read(&mut input)["action"], "deposited");
    let task = panel.update(SplitMessage::CheckDeposits);
    drive_panel(&mut panel, task, true).await;
    let verified = panel
        .funding_progress()
        .is_some_and(|progress| progress.both_verified && progress.review.is_some());
    emit(json!({
        "event":"split-deposits-checked",
        "verified":verified,
        "notice":panel.notice(),
    }));
    assert!(
        verified,
        "live Split funding proof was refused: {:?}",
        panel.notice()
    );
    assert_eq!(read(&mut input)["action"], "sign_pair");
    crate::services::claim_ancestry_gate::enable_disposable_regtest_acceptance();
    let task = panel.update(SplitMessage::BeginPairedSigning);
    drive_panel(&mut panel, task, false).await;
    assert_eq!(panel.stage(), &Stage::PairedSign, "{:?}", panel.notice());
    let (bitcoin, fork, _, _) = panel.paired_signing_view().unwrap();
    let bitcoin_signed = sign(bitcoin.psbt(), &wallet.signers);
    let fork_signed = sign(fork.psbt(), &wallet.signers);
    for (chain, name, signed) in [
        (ChainId::Bitcoin, "bitcoin", bitcoin_signed),
        (ChainId::BitcoinBlake2b, "btcb2", fork_signed),
    ] {
        let path = root.join(format!("split-{name}-signed.txt"));
        std::fs::write(&path, split_psbt_file::encode(&signed, Encoding::Base64)).unwrap();
        let task = panel.apply(SplitEvent::PairedImportChosen(
            panel.seq,
            chain,
            Some(vec![path]),
        ));
        drive_panel(&mut panel, task, false).await;
    }
    let recovered = panel.paired_recovery().expect("signed pair recovery");
    assert!(recovered.bitcoin_unattempted);
    assert!(recovered.bitcoin_txid.is_some() && recovered.fork_txid.is_some());
    emit(json!({
        "event":"split-signed-pair-recorded",
        "bitcoin_txid":recovered.bitcoin_txid,
        "btcb2_txid":recovered.fork_txid,
        "bitcoin_unattempted":recovered.bitcoin_unattempted,
    }));
}
