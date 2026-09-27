use super::*;
use coincube_core::{
    claim_ancestry::{retained::RetainedPath, search::OwnedLink},
    claim_spend::{create_ancestry_self_transfer, AncestrySelfTransfer},
    descriptors::CoincubeDescriptor,
    miniscript::bitcoin::{bip32::ChildNumber, consensus::serialize, secp256k1},
    spend::{CandidateCoin, TxGetter},
};
use std::{collections::HashMap, str::FromStr};

fn fixture(change: u32, large: bool) -> (AncestrySelfTransfer, RetainedPath) {
    let descriptor = CoincubeDescriptor::from_str(WSH_DESC).unwrap();
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
    struct Getter(HashMap<Txid, Transaction>);
    impl TxGetter for Getter {
        fn get_tx(&mut self, id: &Txid) -> Option<Transaction> {
            self.0.get(id).cloned()
        }
    }
    let built = create_ancestry_self_transfer(
        ChainId::Bitcoin,
        &descriptor,
        &secp,
        &mut Getter(txs),
        &coins,
        ChildNumber::from_normal_idx(change).unwrap(),
        5,
        absolute::LockTime::ZERO,
        &path.reverify().unwrap(),
    )
    .unwrap();
    (built, path)
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
