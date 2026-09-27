use super::*;
use coincube_core::{
    claim_ancestry::{retained::RetainedPath, search::OwnedLink},
    claim_spend::{create_ancestry_self_transfer, AncestrySelfTransfer},
    descriptors::CoincubeDescriptor,
    miniscript::bitcoin::{bip32::ChildNumber, consensus::serialize, secp256k1},
    spend::{CandidateCoin, TxGetter},
};
use std::{collections::HashMap, str::FromStr};

struct Getter(HashMap<Txid, Transaction>);
impl TxGetter for Getter {
    fn get_tx(&mut self, id: &Txid) -> Option<Transaction> {
        self.0.get(id).cloned()
    }
}
fn fixture(change: u32, large: bool) -> (AncestrySelfTransfer, RetainedPath) {
    let (built, path, _, _) = material(
        change,
        large,
        CoincubeDescriptor::from_str(WSH_DESC).unwrap(),
    );
    (built, path)
}
fn material(
    change: u32,
    large: bool,
    descriptor: CoincubeDescriptor,
) -> (
    AncestrySelfTransfer,
    RetainedPath,
    Getter,
    Vec<CandidateCoin>,
) {
    let secp = secp256k1::Secp256k1::verification_only();
    let mut ancestors = Vec::<Transaction>::new();
    if large {
        for _ in 0..3 {
            let parent = ancestors
                .last()
                .map(|tx| OutPoint::new(tx.compute_txid(), 0));
            ancestors.push(Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: parent.unwrap_or(OutPoint::null()),
                    witness: Witness::from_slice(&[vec![0; 399_800]]),
                    ..TxIn::default()
                }],
                output: vec![TxOut {
                    value: Amount::from_sat(100_000),
                    script_pubkey: ScriptBuf::new(),
                }],
            });
        }
    }
    let mut txs = HashMap::new();
    let mut coins = Vec::new();
    for i in 0..2 {
        let deriv_index = ChildNumber::from_normal_idx(i).unwrap();
        let mut input = TxIn::default();
        if i == 0 {
            if let Some(parent) = ancestors.last() {
                input.previous_output = OutPoint::new(parent.compute_txid(), 0);
            }
        }
        let tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![input],
            output: vec![TxOut {
                value: Amount::from_sat(100_000),
                script_pubkey: descriptor
                    .receive_descriptor()
                    .derive(deriv_index, &secp)
                    .script_pubkey(),
            }],
        };
        coins.push(CandidateCoin {
            outpoint: OutPoint::new(tx.compute_txid(), 0),
            amount: tx.output[0].value,
            deriv_index,
            is_change: false,
            must_select: true,
            sequence: None,
            ancestor_info: None,
        });
        txs.insert(tx.compute_txid(), tx);
    }
    let selected = coins[0].outpoint;
    let mut links = vec![OwnedLink {
        transaction: serialize(&txs[&selected.txid]),
        parent_input: large.then_some(0),
    }];
    for (i, tx) in ancestors.iter().enumerate().rev() {
        links.push(OwnedLink {
            transaction: serialize(tx),
            parent_input: (i != 0).then_some(0),
        });
    }
    let path = RetainedPath::new(selected, links).unwrap();
    let built = create_ancestry_self_transfer(
        ChainId::Bitcoin,
        &descriptor,
        &secp,
        &mut Getter(txs.clone()),
        &coins,
        ChildNumber::from_normal_idx(change).unwrap(),
        5,
        absolute::LockTime::ZERO,
        &path.reverify().unwrap(),
    )
    .unwrap();
    (built, path, Getter(txs), coins)
}
fn create(temp: &Temp, built: &AncestrySelfTransfer, path: &RetainedPath) -> Controller {
    Controller::create_ancestry(
        &temp.0,
        "bitcoin-cube".into(),
        "fork-cube".into(),
        built,
        path,
        context(),
    )
    .unwrap()
}

#[test]
fn large_path_reopens_without_construction_or_submission_authority() {
    let temp = Temp::new();
    let (built, path) = fixture(10, true);
    let c = create(&temp, &built, &path);
    assert_eq!(c.intent.version, 7);
    assert!(fs::metadata(temp.0.join("intent.json")).unwrap().len() > 1024 * 1024);
    let identity = c.identity().clone();
    drop(c);
    let mut c = Controller::reopen(&temp.0, &identity, context()).unwrap();
    assert_eq!(
        c.recorded_ancestry().unwrap().unwrap().encode(),
        path.encode()
    );
    assert!(!c.construction_verified);
    assert_eq!(c.status(), Status::Unchecked);
    let (wrong, _) = fixture(11, true);
    assert!(matches!(
        c.revalidate_ancestry_construction(&context(), &wrong),
        Err(Error::WrongIdentity)
    ));
    assert!(!c.construction_verified);
    c.revalidate_ancestry_construction(&context(), &built)
        .unwrap();
    assert!(c.construction_verified);
    assert_eq!(c.status(), Status::Unchecked);
    assert!(matches!(
        c.revalidate_ancestry_construction(&context(), &wrong),
        Err(Error::WrongIdentity)
    ));
    assert!(!c.construction_verified);
    assert_eq!(c.status(), Status::Unchecked);
    assert_eq!(c.intent.phase, Phase::Intent);
    assert!(c.intent.signed_txid.is_none());
}

#[test]
fn tampered_ancestry_records_fail_reopen_without_rewriting_evidence() {
    let (built, path) = fixture(10, false);
    for mutation in 0..9 {
        let temp = Temp::new();
        let c = create(&temp, &built, &path);
        let identity = c.identity().clone();
        let mut value = serde_json::to_value(&c.intent).unwrap();
        drop(c);
        match mutation {
            0 => value["ancestry"] = serde_json::Value::Null,
            1 => value["version"] = 6.into(),
            2 => value["ancestry"]["raw"] = "00".into(),
            3 => {
                value["ancestry"]["selected"] =
                    serde_json::to_value(built.claimed_prevouts()[0]).unwrap()
            }
            4 => {
                value["plan"]["claimed_prevouts"] =
                    serde_json::to_value(vec![built.poison_input()]).unwrap()
            }
            5 => value["plan"]["claimed_prevouts"] = serde_json::json!([]),
            6 => value["plan"]["poison"] = serde_json::to_value(Poison::OpReturn).unwrap(),
            7 => value["bitcoin_change_index"] = serde_json::Value::Null,
            8 => {
                value["ancestry"]["raw"] = "00"
                    .repeat(coincube_core::claim_ancestry::retained::MAX_ENCODED_BYTES + 1)
                    .into()
            }
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        let file = temp.0.join("intent.json");
        fs::write(&file, &bytes).unwrap();
        assert!(
            Controller::reopen(&temp.0, &identity, context()).is_err(),
            "mutation {}",
            mutation
        );
        assert_eq!(fs::read(file).unwrap(), bytes);
    }
}

#[test]
fn larger_ancestry_allowance_does_not_relax_legacy_journal_limit() {
    let temp = Temp::new();
    let c = controller(&temp);
    drop(c);
    let file = temp.0.join("intent.json");
    let mut bytes = fs::read(&file).unwrap();
    bytes.resize(1024 * 1024 + 1, b' ');
    fs::write(&file, &bytes).unwrap();
    assert!(matches!(
        Controller::reopen(&temp.0, &identity(), context()),
        Err(Error::InvalidJournal)
    ));
    assert_eq!(fs::read(file).unwrap(), bytes);
}

#[test]
fn restore_ancestry_rebuilds_intent_and_authenticates_recorded_witness_without_writes() {
    use coincube_core::claim_finalize::finalize_ancestry_transfer;
    let (owned, _) = real_artifact(ChainId::Bitcoin, true, 10);
    let (built, path, mut getter, coins) = material(10, false, owned.descriptor().clone());
    let curve = secp256k1::Secp256k1::new();
    let mut signed = built.psbt().clone();
    for byte in [40, 41] {
        let signer = MasterSigner::from_mnemonic(
            Network::Bitcoin,
            Mnemonic::from_entropy(&[byte; 16]).unwrap(),
        )
        .unwrap();
        signed = signer.sign_psbt(signed, &curve).unwrap();
    }
    let verified = finalize_ancestry_transfer(&built, &signed, &curve).unwrap();
    for mode in 0..3 {
        let temp = Temp::new();
        let c = create(&temp, &built, &path);
        let identity = c.identity().clone();
        let mut intent = c.intent.clone();
        drop(c);
        if mode > 0 {
            intent.phase = Phase::BroadcastUncertain;
            intent.signed_txid = Some(verified.transaction().compute_txid());
            let mut transaction = verified.transaction().clone();
            if mode == 2 {
                // Still structurally nonempty and identical unsigned txid, but
                // not a valid script satisfaction. Journal parsing isn't proof.
                transaction.input[0].witness = Witness::from_slice(&[vec![1; 64]]);
            }
            intent.bitcoin_attempts = vec![BitcoinSubmissionAttempt {
                wtxid: Some(transaction.compute_wtxid()),
            }];
            intent.bitcoin_transaction = Some(transaction);
            fs::write(
                temp.0.join("intent.json"),
                serde_json::to_vec(&intent).unwrap(),
            )
            .unwrap();
        }
        let before = fs::read(temp.0.join("intent.json")).unwrap();
        let mut c = Controller::reopen(&temp.0, &identity, context()).unwrap();
        let restored = c.restore_ancestry(&context(), built.descriptor(), &mut getter, &coins);
        if mode == 2 {
            assert!(matches!(restored, Err(Error::InvalidJournal)));
            assert!(!c.construction_verified);
        } else {
            let (restored, signature) = restored.unwrap();
            assert_eq!(restored.psbt(), built.psbt());
            assert_eq!(
                signature.as_ref().map(|v| v.transaction()),
                (mode == 1).then_some(verified.transaction())
            );
            assert!(c.construction_verified);
            // A failed new reconstruction withdraws the successful check.
            assert!(c
                .restore_ancestry(&context(), built.descriptor(), &mut getter, &coins[..1])
                .is_err());
            assert!(!c.construction_verified);
        }
        assert_eq!(c.status(), Status::Unchecked);
        assert_eq!(fs::read(temp.0.join("intent.json")).unwrap(), before);
    }
}

#[test]
fn restore_ancestry_fork_uses_only_shared_inputs_and_rejects_saved_output_tampering() {
    use coincube_core::{
        claim_finalize::finalize_ancestry_transfer, claim_spend::create_ancestry_fork_sweep,
    };
    let (owned, _) = real_artifact(ChainId::Bitcoin, true, 10);
    let (built, path, mut getter, coins) = material(10, false, owned.descriptor().clone());
    let curve = secp256k1::Secp256k1::new();
    let mut signed = built.psbt().clone();
    for byte in [40, 41] {
        let signer = MasterSigner::from_mnemonic(
            Network::Bitcoin,
            Mnemonic::from_entropy(&[byte; 16]).unwrap(),
        )
        .unwrap();
        signed = signer.sign_psbt(signed, &curve).unwrap();
    }
    let verified = finalize_ancestry_transfer(&built, &signed, &curve).unwrap();
    // The fork side cannot retrieve the Bitcoin-only input's transaction.
    getter.0.remove(&built.poison_input().txid);
    let sweep = create_ancestry_fork_sweep(
        &built,
        ChainId::BitcoinBlake2b,
        &secp256k1::Secp256k1::verification_only(),
        &mut getter,
        &coins[1..],
        ChildNumber::from_normal_idx(11).unwrap(),
        5,
        absolute::LockTime::ZERO,
    )
    .unwrap();
    for mode in 0..6 {
        let temp = Temp::new();
        let c = create(&temp, &built, &path);
        let identity = c.identity().clone();
        let mut intent = c.intent.clone();
        drop(c);
        intent.phase = Phase::Tracking;
        intent.signed_txid = Some(verified.transaction().compute_txid());
        intent.bitcoin_transaction = Some(verified.transaction().clone());
        intent.bitcoin_attempts = vec![BitcoinSubmissionAttempt {
            wtxid: Some(verified.transaction().compute_wtxid()),
        }];
        intent.fork_change_index = Some(11);
        intent.fork_sweep = Some(sweep.psbt().unsigned_tx.clone());
        match mode {
            2 => intent.fork_change_index = Some(12),
            3 => {
                intent.fork_sweep.as_mut().unwrap().output[0].script_pubkey = built
                    .descriptor()
                    .receive_descriptor()
                    .derive(ChildNumber::from_normal_idx(12).unwrap(), &curve)
                    .script_pubkey();
            }
            4 => {
                intent.fork_sweep.as_mut().unwrap().output[0].value = Amount::from_sat(200_000);
            }
            _ => {}
        }
        let file = temp.0.join("intent.json");
        fs::write(&file, serde_json::to_vec(&intent).unwrap()).unwrap();
        let before = fs::read(&file).unwrap();
        let mut c = Controller::reopen(&temp.0, &identity, context()).unwrap();
        let (wrong, _) = fixture(12, false);
        let result = c.restore_ancestry_fork_sweep(
            &context(),
            if mode == 5 { &wrong } else { &built },
            &mut getter,
            if mode == 1 { &coins } else { &coins[1..] },
        );
        if mode == 0 {
            let restored = result.unwrap();
            assert_eq!(restored.psbt(), sweep.psbt());
            assert_eq!(
                restored.bitcoin_step1(),
                built.psbt().unsigned_tx.compute_txid()
            );
            assert!(c.construction_verified);
            assert!(c
                .restore_ancestry_fork_sweep(&context(), &built, &mut getter, &[])
                .is_err());
        } else {
            assert!(result.is_err(), "mode {}", mode);
        }
        assert!(!c.construction_verified);
        assert_eq!(c.status(), Status::Unchecked);
        assert_eq!(fs::read(&file).unwrap(), before);
    }
}
