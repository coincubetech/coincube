//! Headless Split panel driver for the disposable two-chain Python fixture.
//! The production Connect adapter performs every source scan and proof read.

use super::*;
use crate::{
    app::{settings::WalletId, split_intent::SplitIntent},
    services::{
        claim_workflow::SplitTargetBinding,
        foreign_scan::{BranchRange, ScanPlan},
        foreign_split_inventory, split_source,
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

/// Inspect the same Iced element the desktop presents, using the software
/// renderer so the disposable two-chain fixture checks its visible facts too.
async fn rendered_labels(panel: &SplitPanel) -> Vec<String> {
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
    .expect("software renderer for the paired Split wizard");
    let mut element = crate::app::view::vault::split::split_panel(panel);
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

fn has_label(labels: &[String], text: &str) -> bool {
    labels.iter().any(|label| label.contains(text))
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
    let descriptor =
        CoincubeDescriptor::from_str(crate::services::claim_workflow::TEST_WSH_DESC).unwrap();
    let wallet_id = WalletId::new(
        descriptor.to_string().split_once('#').unwrap().1.to_owned(),
        Some(1),
    );
    let binding = SplitTargetBinding::for_test(wallet_id.clone(), &descriptor, 4);
    let session = crate::app::state::vault::claim::ConnectSession {
        client: client.clone(),
        account: "synthetic-regtest-account".into(),
    };
    let connect: Arc<dyn SplitConnect> =
        Arc::new(step1::ProductionConnect::new(session, generation.clone()).unwrap());
    if init["resume"].as_bool().unwrap_or(false) {
        let source = split_source::split_source(&wallet.external, Some(&wallet.internal)).unwrap();
        let digest = source.digest();
        let mut panel = SplitPanel::resume(
            TARGET.into(),
            root.join("split"),
            digest,
            step1::journal_directory(&root.join("split"), digest),
        );
        panel.bind_target_vault(wallet_id, descriptor);
        panel.set_connect(Some(connect.clone()));
        let task = panel.begin();
        drive_panel(&mut panel, task, false).await;
        assert!(crate::services::claim_ancestry_gate::authorization().is_none());
        let denied = panel.update(SplitMessage::ReviewPairedBitcoin);
        drive_panel(&mut panel, denied, false).await;
        assert_eq!(panel.stage(), &Stage::PairedRecovery);
        let recovered = panel.paired_recovery().expect("reopened signed Split pair");
        let recovery_labels = rendered_labels(&panel).await;
        assert!(has_label(&recovery_labels, "Recorded two-chain split"));
        assert!(has_label(&recovery_labels, "Bitcoin transaction"));
        assert!(has_label(&recovery_labels, "Bitcoin Blake2b transaction"));
        assert!(has_label(&recovery_labels, "Check again"));
        assert!(!has_label(&recovery_labels, "Broadcast Transaction"));
        assert_eq!(
            recovered.bitcoin_unattempted,
            !init["broadcast_btcb2"].as_bool().unwrap_or(false)
        );
        assert_eq!(
            recovered.bitcoin_txid.unwrap().to_string(),
            init["bitcoin_txid"].as_str().unwrap()
        );
        assert_eq!(
            recovered.fork_txid.unwrap().to_string(),
            init["btcb2_txid"].as_str().unwrap()
        );
        let recovered_bitcoin_txid = recovered.bitcoin_txid;
        let recovered_fork_txid = recovered.fork_txid;
        let identity = crate::services::claim_workflow::split_identity(TARGET.into(), digest);
        let controller = Controller::reopen_settling(
            &step1::journal_directory(&root.join("split"), digest),
            &identity,
            connect.context(),
        )
        .await
        .unwrap();
        let pair = controller.recorded_signed_ancestry_pair().unwrap();
        let bitcoin_raw =
            coincube_core::miniscript::bitcoin::consensus::encode::serialize_hex(pair.bitcoin());
        let btcb2_raw =
            coincube_core::miniscript::bitcoin::consensus::encode::serialize_hex(pair.fork());
        drop(controller);
        if init["broadcast_bitcoin"].as_bool().unwrap_or(false) {
            crate::services::claim_ancestry_gate::enable_disposable_regtest_acceptance();
            let task = panel.update(SplitMessage::ReviewPairedBitcoin);
            drive_panel(&mut panel, task, false).await;
            assert_eq!(
                panel.stage(),
                &Stage::PairedBitcoinReview,
                "{:?}",
                panel.notice()
            );
            assert!(panel.paired_bitcoin_review().is_some());
            let review_labels = rendered_labels(&panel).await;
            assert!(has_label(&review_labels, "Bitcoin — broadcast now"));
            assert!(has_label(
                &review_labels,
                "Bitcoin Blake2b — recorded, not sent now"
            ));
            assert!(has_label(&review_labels, "Broadcast Transaction"));
            assert!(has_label(
                &review_labels,
                &recovered_bitcoin_txid.unwrap().to_string()
            ));
            let saved_review = panel.paired_bitcoin_review.clone();
            panel.paired_bitcoin_review.as_mut().unwrap().fork.output[0].script_pubkey =
                Default::default();
            let invalid_labels = rendered_labels(&panel).await;
            assert!(has_label(
                &invalid_labels,
                "Cannot display both signed destinations"
            ));
            assert!(!has_label(&invalid_labels, "Broadcast Transaction"));
            panel.paired_bitcoin_review = saved_review;
            let task = panel.update(SplitMessage::ConfirmPairedBitcoin);
            drive_panel(&mut panel, task, false).await;
            let recorded = Controller::reopen_settling(
                &step1::journal_directory(&root.join("split"), digest),
                &identity,
                connect.context(),
            )
            .await
            .unwrap();
            let attempt = recorded.recorded_signed_ancestry_pair().unwrap();
            assert!(attempt.possible_attempt(ChainId::Bitcoin).is_some());
            assert!(attempt.possible_attempt(ChainId::BitcoinBlake2b).is_none());
            emit(json!({
                "event":"split-bitcoin-broadcast-recorded",
                "bitcoin_txid":recovered_bitcoin_txid,
                "stage":format!("{:?}",panel.stage()),
                "notice":panel.notice(),
            }));
            return;
        }
        if init["broadcast_btcb2"].as_bool().unwrap_or(false) {
            assert!(recovered.fork_ready_for_review, "{:?}", recovered);
            crate::services::claim_ancestry_gate::enable_disposable_regtest_acceptance();
            let task = panel.update(SplitMessage::ReviewPairedFork);
            drive_panel(&mut panel, task, false).await;
            assert_eq!(
                panel.stage(),
                &Stage::PairedForkReview,
                "{:?}",
                panel.notice()
            );
            assert!(panel.paired_fork_review().is_some());
            let review_labels = rendered_labels(&panel).await;
            assert!(has_label(&review_labels, "Bitcoin — already recorded"));
            assert!(has_label(&review_labels, "Bitcoin Blake2b — broadcast now"));
            assert!(has_label(&review_labels, "Broadcast Transaction"));
            assert!(has_label(
                &review_labels,
                &recovered_fork_txid.unwrap().to_string()
            ));
            if init["pause_before_fork_confirm"].as_bool().unwrap_or(false) {
                emit(json!({"event":"split-btcb2-reviewed"}));
                assert_eq!(read(&mut input)["action"], "confirm_after_reorg");
                let task = panel.update(SplitMessage::ConfirmPairedFork);
                drive_panel(&mut panel, task, false).await;
                assert_eq!(panel.stage(), &Stage::PairedRecovery);
                assert!(panel.notice().is_some());
                let recorded = Controller::reopen_settling(
                    &step1::journal_directory(&root.join("split"), digest),
                    &identity,
                    connect.context(),
                )
                .await
                .unwrap();
                assert!(recorded
                    .recorded_signed_ancestry_pair()
                    .unwrap()
                    .possible_attempt(ChainId::BitcoinBlake2b)
                    .is_none());
                emit(json!({
                    "event":"split-btcb2-reorg-refused",
                    "reason":panel.notice(),
                }));
                return;
            }
            let task = panel.update(SplitMessage::ConfirmPairedFork);
            drive_panel(&mut panel, task, false).await;
            let recorded = Controller::reopen_settling(
                &step1::journal_directory(&root.join("split"), digest),
                &identity,
                connect.context(),
            )
            .await
            .unwrap();
            let attempt = recorded.recorded_signed_ancestry_pair().unwrap();
            assert!(attempt.possible_attempt(ChainId::Bitcoin).is_some());
            assert!(attempt.possible_attempt(ChainId::BitcoinBlake2b).is_some());
            emit(json!({
                "event":"split-btcb2-broadcast-recorded",
                "btcb2_txid":recovered_fork_txid,
                "stage":format!("{:?}",panel.stage()),
                "notice":panel.notice(),
            }));
            return;
        }
        emit(json!({
            "event":"split-signed-pair-reopened",
            "bitcoin_unattempted":recovered.bitcoin_unattempted,
            "bitcoin_status":recovered.bitcoin_status,
            "btcb2_status":recovered.fork_status,
            "bitcoin_raw":bitcoin_raw,
            "btcb2_raw":btcb2_raw,
        }));
        return;
    }
    let scanned =
        foreign_split_inventory::scan(client.clone(), source_plan(&wallet), 1, generation)
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
    let deposit_labels = rendered_labels(&panel).await;
    assert!(has_label(
        &deposit_labels,
        "Fund the source wallet on each chain"
    ));
    assert!(has_label(&deposit_labels, &addresses.bitcoin_deposit));
    assert!(has_label(&deposit_labels, &addresses.btcb2_deposit));
    assert!(has_label(&deposit_labels, "Check deposits"));
    assert!(!has_label(&deposit_labels, "Broadcast Transaction"));
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
    assert!(crate::services::claim_ancestry_gate::authorization().is_none());
    let closed = panel.update(SplitMessage::BeginPairedSigning);
    drive_panel(&mut panel, closed, false).await;
    assert_eq!(panel.stage(), &Stage::FundingDeposits);
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
    let preview_labels = rendered_labels(&panel).await;
    assert!(has_label(
        &preview_labels,
        "Paired transaction preview — read only"
    ));
    assert!(has_label(&preview_labels, "Bitcoin-only"));
    assert!(has_label(&preview_labels, "BTCB2-only"));
    assert!(has_label(&preview_labels, "Before fork · both chains"));
    assert!(!has_label(&preview_labels, "OP_RETURN"));
    let review = panel.funding_progress().unwrap().review.as_ref().unwrap();
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
    let destination = |output: &coincube_core::miniscript::bitcoin::TxOut| {
        coincube_core::miniscript::bitcoin::Address::from_script(
            &output.script_pubkey,
            coincube_core::miniscript::bitcoin::Network::Bitcoin,
        )
        .unwrap()
        .to_string()
    };
    assert!(has_label(
        &preview_labels,
        &format!(
            "Return to this source wallet: {} · {} sats",
            destination(&review.bitcoin.destination),
            review.bitcoin.destination.value.to_sat()
        )
    ));
    assert!(has_label(
        &preview_labels,
        &format!(
            "Send to your paired BTCB2 Cube: {} · {} sats",
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
    assert_eq!(read(&mut input)["action"], "sign_pair_spent");
    crate::services::claim_ancestry_gate::enable_disposable_regtest_acceptance();
    let task = panel.update(SplitMessage::BeginPairedSigning);
    drive_panel(&mut panel, task, false).await;
    assert_eq!(panel.stage(), &Stage::FundingDeposits);
    assert!(panel.paired_signing_view().is_none());
    emit(json!({
        "event":"split-signing-refused-spent",
        "notice":panel.notice(),
    }));
    assert_eq!(read(&mut input)["action"], "deposited");
    let task = panel.update(SplitMessage::CheckDeposits);
    drive_panel(&mut panel, task, true).await;
    let verified = panel
        .funding_progress()
        .is_some_and(|progress| progress.both_verified && progress.review.is_some());
    emit(json!({
        "event":"split-deposits-checked-again",
        "verified":verified,
        "notice":panel.notice(),
    }));
    assert!(verified, "live Split funding proof did not recover");
    assert_eq!(read(&mut input)["action"], "sign_pair_stale");
    let task = panel.update(SplitMessage::BeginPairedSigning);
    drive_panel(&mut panel, task, false).await;
    assert_eq!(panel.stage(), &Stage::FundingDeposits);
    assert!(panel.paired_signing_view().is_none());
    emit(json!({
        "event":"split-signing-refused-stale",
        "notice":panel.notice(),
    }));
    assert_eq!(read(&mut input)["action"], "deposited");
    let task = panel.update(SplitMessage::CheckDeposits);
    drive_panel(&mut panel, task, true).await;
    assert!(
        panel
            .funding_progress()
            .is_some_and(|progress| progress.both_verified && progress.review.is_some()),
        "live Split funding proof did not recover after a stale read: {:?}",
        panel.notice()
    );
    assert!(panel.notice().is_none());
    emit(json!({"event":"split-deposits-checked-after-stale"}));
    assert_eq!(read(&mut input)["action"], "sign_pair");
    let task = panel.update(SplitMessage::BeginPairedSigning);
    drive_panel(&mut panel, task, false).await;
    assert_eq!(panel.stage(), &Stage::PairedSign, "{:?}", panel.notice());
    let signing_labels = rendered_labels(&panel).await;
    assert!(has_label(&signing_labels, "Split: sign both transactions"));
    assert!(has_label(&signing_labels, "Save Bitcoin PSBT"));
    assert!(has_label(&signing_labels, "Save BTCB2 PSBT"));
    assert!(has_label(&signing_labels, "Import Bitcoin signatures"));
    assert!(has_label(&signing_labels, "Import BTCB2 signatures"));
    assert!(has_label(&signing_labels, "+ 1 Bitcoin-only coin"));
    assert!(has_label(&signing_labels, "+ 1 BTCB2-only coin"));
    assert!(!has_label(&signing_labels, "OP_RETURN"));
    let (bitcoin, fork, _, _) = panel.paired_signing_view().unwrap();
    for psbt in [bitcoin.psbt(), fork.psbt()] {
        assert_eq!(
            psbt.unsigned_tx.output.len(),
            1,
            "ordinary one-output spend"
        );
        assert!(!psbt.unsigned_tx.output[0].script_pubkey.is_op_return());
    }
    assert_eq!(
        bitcoin.psbt().unsigned_tx.input.len(),
        bitcoin.claimed_prevouts().len() + 1
    );
    assert_eq!(
        fork.psbt().unsigned_tx.input.len(),
        fork.claimed_prevouts().len() + 1
    );
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
