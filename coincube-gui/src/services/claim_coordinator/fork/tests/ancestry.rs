use super::*;
use crate::services::claim_coordinator::tests::ancestry::built;

fn ancestry_sweep(source: &AncestrySelfTransfer) -> (ClaimForkSweep, VerifiedClaimForkSweep) {
    struct Getter(Vec<Transaction>);
    impl TxGetter for Getter {
        fn get_tx(&mut self, id: &Txid) -> Option<Transaction> {
            self.0.iter().find(|tx| tx.compute_txid() == *id).cloned()
        }
    }
    let mut transactions = Vec::new();
    let coins: Vec<_> = source
        .psbt()
        .unsigned_tx
        .input
        .iter()
        .zip(&source.psbt().inputs)
        .filter(|(txin, _)| source.claimed_prevouts().contains(&txin.previous_output))
        .map(|(txin, input)| {
            transactions.push(input.non_witness_utxo.clone().unwrap());
            CandidateCoin {
                outpoint: txin.previous_output,
                amount: input.witness_utxo.as_ref().unwrap().value,
                deriv_index: 1.into(),
                is_change: false,
                must_select: true,
                sequence: None,
                ancestor_info: None,
            }
        })
        .collect();
    let secp = secp256k1::Secp256k1::new();
    let construction = coincube_core::claim_spend::create_ancestry_fork_sweep(
        source,
        ChainId::BitcoinBlake2b,
        &secp256k1::Secp256k1::verification_only(),
        &mut Getter(transactions),
        &coins,
        20.into(),
        3,
        absolute::LockTime::ZERO,
    )
    .unwrap();
    assert!(construction
        .psbt()
        .unsigned_tx
        .input
        .iter()
        .all(|input| input.previous_output != source.poison_input()));
    let signed = [40, 41]
        .iter()
        .fold(construction.psbt().clone(), |psbt, byte| {
            MasterSigner::from_mnemonic(
                Network::Bitcoin,
                Mnemonic::from_entropy(&[*byte; 16]).unwrap(),
            )
            .unwrap()
            .sign_psbt(psbt, &secp)
            .unwrap()
        });
    let verified = coincube_core::claim_finalize::finalize_claim_fork_sweep(
        &construction,
        &coincube_core::psbt_unified::UnifiedPsbt::from_psbt(signed).unwrap(),
        &secp,
    )
    .unwrap();
    (construction, verified)
}

#[tokio::test]
async fn ancestry_fork_admission_binds_journal_and_requires_proof_before_signing_or_review() {
    for mode in 0..5 {
        let temp = Temp::new();
        let server = MockServer::start();
        let (sender, generation) = watch::channel(7);
        let (source, path, bitcoin) = built(10, false);
        let (wrong, _, _) = built(11, false);
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
        let mut stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        stored["phase"] = json!("BroadcastUncertain");
        stored["signed_txid"] = json!(bitcoin.transaction().compute_txid());
        stored["bitcoin_transaction"] = json!(bitcoin.transaction());
        stored["bitcoin_attempts"] = json!([{ "wtxid": bitcoin.transaction().compute_wtxid() }]);
        std::fs::write(&file, serde_json::to_vec(&stored).unwrap()).unwrap();
        let before = std::fs::read(&file).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let fixture = || {
            Box::new(Fixture {
                source_txid: bitcoin.transaction().compute_txid(),
                preflight: PreflightClient::new(
                    &server.base_url(),
                    CollectionContext {
                        expected_generation: 7,
                        generation: generation.clone(),
                    },
                )
                .unwrap(),
                stamp: 1000,
                clock: Arc::new(AtomicI64::new(1000)),
                fault: Arc::new(AtomicUsize::new(0)),
                calls: calls.clone(),
                reached: Arc::new(tokio::sync::Notify::new()),
                directory: temp.0.clone(),
            })
        };
        let mut current = context();
        if mode == 2 {
            current.account = "other".into();
        }
        if mode == 3 {
            sender.send_replace(8);
        }
        let (construction, _) = ancestry_sweep(&source);
        let result = Preparation::open(
            &temp.0,
            "bitcoin-cube".into(),
            "fork-cube".into(),
            if mode == 1 { &wrong } else { &source },
            construction,
            current.clone(),
            generation.clone(),
            fixture(),
            policy(),
        );
        if mode > 0 && mode < 4 {
            assert!(result.is_err());
        } else {
            let mut preparation = result.unwrap();
            assert!(matches!(
                preparation.check_signing(&current).await,
                Err(Error::Unsupported)
            ));
            drop(preparation);
        }
        let (construction, verified) = ancestry_sweep(&source);
        let result = Coordinator::open(
            &temp.0,
            "bitcoin-cube".into(),
            "fork-cube".into(),
            if mode == 1 { &wrong } else { &source },
            construction,
            verified,
            current.clone(),
            generation.clone(),
            fixture(),
            policy(),
        );
        if mode > 0 && mode < 4 {
            assert!(result.is_err());
        } else {
            let mut coordinator = result.unwrap();
            assert!(matches!(
                coordinator.prepare_review(&current).await,
                Err(Error::Unsupported)
            ));
            assert!(matches!(
                coordinator.checked_sweep(&current).await,
                Err(Error::InvalidBinding)
            ));
            if mode == 4 {
                sender.send_replace(8);
                assert!(matches!(
                    coordinator.prepare_review(&current).await,
                    Err(Error::Revoked)
                ));
            }
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(&file).unwrap(), before);
    }
}

mod http;
