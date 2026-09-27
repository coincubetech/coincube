use super::*;
use coincube_core::{
    claim_ancestry::{retained::RetainedPath, search::OwnedLink},
    claim_finalize::{finalize_ancestry_transfer, VerifiedAncestryTransfer},
    claim_spend::{create_ancestry_self_transfer, AncestrySelfTransfer},
    miniscript::bitcoin::consensus::{deserialize, serialize},
};
fn built(
    change: u32,
    alternate: bool,
) -> (AncestrySelfTransfer, RetainedPath, VerifiedAncestryTransfer) {
    let (owned, _) = artifact(ChainId::Bitcoin, true, 10);
    let descriptor = owned.descriptor();
    let secp = secp256k1::Secp256k1::new();
    let verify = secp256k1::Secp256k1::verification_only();
    struct Getter(Vec<Transaction>);
    impl TxGetter for Getter {
        fn get_tx(&mut self, id: &Txid) -> Option<Transaction> {
            self.0.iter().find(|tx| tx.compute_txid() == *id).cloned()
        }
    }
    let txs: Vec<_> = (0..2)
        .map(|index| Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(100_000),
                script_pubkey: descriptor
                    .receive_descriptor()
                    .derive(index.into(), &verify)
                    .script_pubkey(),
            }],
        })
        .collect();
    let coins: Vec<_> = txs
        .iter()
        .enumerate()
        .map(|(i, tx)| CandidateCoin {
            outpoint: OutPoint::new(tx.compute_txid(), 0),
            amount: tx.output[0].value,
            deriv_index: (i as u32).into(),
            is_change: false,
            must_select: true,
            sequence: None,
            ancestor_info: None,
        })
        .collect();
    let path = RetainedPath::new(
        coins[0].outpoint,
        vec![OwnedLink {
            transaction: serialize(&txs[0]),
            parent_input: None,
        }],
    )
    .unwrap();
    let source = create_ancestry_self_transfer(
        ChainId::Bitcoin,
        descriptor,
        &verify,
        &mut Getter(txs),
        &coins,
        ChildNumber::from_normal_idx(change).unwrap(),
        5,
        absolute::LockTime::ZERO,
        &path.reverify().unwrap(),
    )
    .unwrap();
    let mut psbt = source.psbt().clone();
    for byte in [40, if alternate { 42 } else { 41 }] {
        let signer = MasterSigner::from_mnemonic(
            Network::Bitcoin,
            Mnemonic::from_entropy(&[byte; 16]).unwrap(),
        )
        .unwrap();
        psbt = signer.sign_psbt(psbt, &secp).unwrap();
    }
    let signed = finalize_ancestry_transfer(&source, &psbt, &verify).unwrap();
    (source, path, signed)
}

#[tokio::test]
async fn ancestry_admission_binds_artifacts_before_writing_and_cannot_review_without_proof() {
    for mode in 0..4 {
        let h = Harness::new().await;
        let temp = Temp::new();
        let (source, path, signed) = built(10, false);
        let (wrong, _, _) = built(11, false);
        let mut current = context();
        let mut checks = policy();
        if mode == 2 {
            current.generation += 1;
        }
        if mode == 3 {
            checks.observations.max_observation_age_seconds = 0;
        }
        let result = Coordinator::open_ancestry(
            &temp.0,
            "bitcoin-cube".into(),
            "fork-cube".into(),
            if mode == 1 { &wrong } else { &source },
            &path,
            signed,
            current.clone(),
            h.sender.subscribe(),
            h.coordinator.services,
            checks,
            false,
        );
        if mode > 0 {
            assert!(result.is_err());
            assert!(!temp.0.join("intent.json").exists());
            continue;
        }
        let mut coordinator = result.unwrap();
        assert!(matches!(coordinator.verified, VerifiedStep1::Ancestry(_)));
        assert_eq!(coordinator.phase(), Phase::Intent);
        let before = std::fs::read(temp.0.join("intent.json")).unwrap();
        assert!(matches!(
            coordinator.prepare_review(&current).await,
            Err(Error::Unsupported)
        ));
        assert_eq!(h.calls.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(temp.0.join("intent.json")).unwrap(), before);
        h.sender.send_replace(8);
        assert!(matches!(
            coordinator.prepare_review(&current).await,
            Err(Error::Revoked)
        ));
    }
}

#[tokio::test]
async fn ancestry_resume_binds_saved_path_account_and_exact_recorded_witness() {
    for mode in 0..5 {
        let temp = Temp::new();
        let (source, path, signed) = built(10, false);
        let controller = Controller::create_ancestry(
            &temp.0,
            "bitcoin-cube".into(),
            "fork-cube".into(),
            &source,
            &path,
            context(),
        )
        .unwrap();
        drop(controller);
        let file = temp.0.join("intent.json");
        if mode >= 3 {
            let mut stored: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            stored["phase"] = json!("BroadcastUncertain");
            stored["signed_txid"] = json!(signed.transaction().compute_txid());
            stored["bitcoin_transaction"] = json!(signed.transaction());
            stored["bitcoin_attempts"] = json!([{ "wtxid": signed.transaction().compute_wtxid() }]);
            std::fs::write(&file, serde_json::to_vec(&stored).unwrap()).unwrap();
        }
        let h = Harness::new().await;
        let mut current = context();
        if mode == 1 {
            current.account = "other-account".into();
        }
        let mut links = path.links().to_vec();
        let mut root: Transaction = deserialize(&links[0].transaction).unwrap();
        root.input[0].witness.push([1]);
        links[0].transaction = serialize(&root);
        let changed_path = RetainedPath::new(path.selected(), links).unwrap();
        let (_, _, alternate) = built(10, true);
        assert_eq!(
            signed.transaction().compute_txid(),
            alternate.transaction().compute_txid()
        );
        assert_ne!(
            signed.transaction().compute_wtxid(),
            alternate.transaction().compute_wtxid()
        );
        let before = std::fs::read(&file).unwrap();
        let result = Coordinator::open_ancestry(
            &temp.0,
            "bitcoin-cube".into(),
            "fork-cube".into(),
            &source,
            if mode == 2 { &changed_path } else { &path },
            if mode == 4 { alternate } else { signed },
            current,
            h.sender.subscribe(),
            h.coordinator.services,
            policy(),
            true,
        );
        if mode == 0 || mode == 3 {
            let resumed = result.unwrap();
            assert_eq!(resumed.controller.status(), Status::Unchecked);
            assert_eq!(
                resumed.phase(),
                if mode == 0 {
                    Phase::Intent
                } else {
                    Phase::BroadcastUncertain
                }
            );
        } else {
            assert!(result.is_err());
        }
        assert_eq!(h.calls.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(&file).unwrap(), before);
    }
}
