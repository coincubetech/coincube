//! As for step 1, signatures come from rust-bitcoin's reference PSBT signer
//! (`Psbt::sign`), which chooses the legacy or BIP 143 digest itself, and
//! every finalized witness is replayed by Miniscript's interpreter. The
//! fixtures are a copy of step 1's, so that test file stays unchanged.

use super::*;
use miniscript::bitcoin::{
    bip32::{DerivationPath, Xpriv, Xpub},
    psbt::PsbtSighashType,
    Network, PublicKey,
};
use std::{convert::TryFrom, str::FromStr};

const FORK: u64 = 900;
const FEERATE: u64 = 5;
/// Observed Bitcoin tip height, for building step 1.
const TIP: u32 = 860_000;
/// Observed BTCB2 tip height for step 2's locktime.
const BTCB2_TIP: u32 = 861_000;
const BTCB2: ChainId = ChainId::BitcoinBlake2b;

type Change = Box<dyn Fn(&mut Psbt)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Wpkh,
    ShWpkh,
    Pkh,
    WshSortedMulti,
    WshMulti,
}
const SHAPES: [Shape; 5] = [
    Shape::Wpkh,
    Shape::ShWpkh,
    Shape::Pkh,
    Shape::WshSortedMulti,
    Shape::WshMulti,
];

fn master(seed: u8) -> Xpriv {
    Xpriv::new_master(Network::Bitcoin, &[seed; 32]).unwrap()
}

fn account(master: &Xpriv, path: &str) -> String {
    let secp = secp256k1::Secp256k1::new();
    let xpriv = master
        .derive_priv(&secp, &DerivationPath::from_str(path).unwrap())
        .unwrap();
    format!(
        "[{}/{}]{}",
        master.fingerprint(&secp),
        path.trim_start_matches("m/"),
        Xpub::from_priv(&secp, &xpriv)
    )
}

fn descriptor(text: &str) -> Descriptor<DescriptorPublicKey> {
    Descriptor::from_str(text).unwrap()
}

struct Wallet {
    source: SplitSource,
    signers: Vec<Xpriv>,
    threshold: usize,
}

fn wallet(shape: Shape) -> Wallet {
    let branch =
        |template: &str, branch: u32| descriptor(&template.replace("{b}", &branch.to_string()));
    let (template, signers, threshold) = match shape {
        Shape::Wpkh => {
            let m = master(1);
            (
                format!("wpkh({}/{{b}}/*)", account(&m, "m/84'/0'/0'")),
                vec![m],
                1,
            )
        }
        Shape::ShWpkh => {
            let m = master(1);
            (
                format!("sh(wpkh({}/{{b}}/*))", account(&m, "m/49'/0'/0'")),
                vec![m],
                1,
            )
        }
        Shape::Pkh => {
            let m = master(1);
            (
                format!("pkh({}/{{b}}/*)", account(&m, "m/44'/0'/0'")),
                vec![m],
                1,
            )
        }
        Shape::WshSortedMulti | Shape::WshMulti => {
            let masters = [master(1), master(2), master(3)];
            let keys: Vec<_> = masters
                .iter()
                .map(|m| format!("{}/{{b}}/*", account(m, "m/48'/0'/0'/2'")))
                .collect();
            let name = if shape == Shape::WshMulti {
                "multi"
            } else {
                "sortedmulti"
            };
            // Two of the three keys sign.
            (
                format!("wsh({name}(2,{}))", keys.join(",")),
                masters[..2].to_vec(),
                2,
            )
        }
    };
    Wallet {
        source: SplitSource::new(branch(&template, 0), Some(branch(&template, 1))).unwrap(),
        signers,
        threshold,
    }
}

fn block(height: u64) -> BlockRef {
    BlockRef {
        height,
        hash: BlockHash::from_byte_array([height as u8; 32]),
    }
}

fn coin(source: &SplitSource, branch: SplitBranch, index: u32, sats: u64) -> SplitCoin {
    let script = source.derive(branch, index).unwrap().script_pubkey();
    let previous = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([index as u8 + 1; 32]), index),
            ..TxIn::default()
        }],
        output: vec![
            TxOut {
                value: Amount::from_sat(1_000),
                script_pubkey: ScriptBuf::new_op_return(
                    bitcoin::script::PushBytesBuf::try_from(vec![index as u8]).unwrap(),
                ),
            },
            TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: script,
            },
        ],
    };
    SplitCoin {
        outpoint: OutPoint::new(previous.compute_txid(), 1),
        branch,
        index,
        previous,
        bitcoin_block: Some(block(FORK - 10)),
        btcb2_block: Some(block(FORK - 10)),
    }
}

fn coins(source: &SplitSource) -> Vec<SplitCoin> {
    vec![
        coin(source, SplitBranch::External, 0, 100_000),
        coin(source, SplitBranch::Internal, 3, 50_000),
    ]
}

/// A target Cube's Vault address: a P2WSH 2-of-3 of unrelated keys.
fn vault_target() -> ScriptBuf {
    let keys: Vec<_> = (5..=7)
        .map(|seed| format!("{}/0/*", account(&master(seed), "m/48'/0'/0'/2'")))
        .collect();
    descriptor(&format!("wsh(sortedmulti(2,{}))", keys.join(",")))
        .at_derivation_index(0)
        .unwrap()
        .script_pubkey()
}

/// Step 1 for `coins`, whose claimed prevouts step 2 spends.
fn step1(source: &SplitSource, coins: &[SplitCoin]) -> SplitStep1 {
    create_split_step1(
        &SplitInputs {
            chain: ChainId::Bitcoin,
            source,
            coins,
            fork_height: FORK,
            destination: 9,
        },
        FEERATE,
        LockTime::ZERO,
        TIP,
        BlockHash::from_byte_array([42; 32]),
    )
    .unwrap()
}

fn inputs<'a>(
    source: &'a SplitSource,
    coins: &'a [SplitCoin],
    claimed: &'a [OutPoint],
    target: &'a bitcoin::Script,
) -> SplitStep2Inputs<'a> {
    SplitStep2Inputs {
        chain: BTCB2,
        source,
        coins,
        fork_height: FORK,
        claimed,
        target,
    }
}

fn create(inputs: &SplitStep2Inputs<'_>) -> Result<SplitStep2, Error> {
    create_split_step2(inputs, FEERATE, LockTime::ZERO, BTCB2_TIP)
}

fn sign(psbt: &Psbt, signers: &[Xpriv]) -> Psbt {
    let secp = secp256k1::Secp256k1::new();
    let mut signed = psbt.clone();
    for signer in signers {
        signed.sign(signer, &secp).unwrap();
    }
    signed
}

fn unsigned(tx: &Transaction) -> Transaction {
    let mut tx = tx.clone();
    for input in &mut tx.input {
        input.script_sig = ScriptBuf::new();
        input.witness.clear();
    }
    tx
}

fn finalize(
    step: &SplitStep2,
    coins: &[SplitCoin],
    source: &SplitSource,
    signed: &Psbt,
) -> Result<VerifiedSplitStep2, FinalizeError> {
    finalize_split_step2(
        step,
        coins,
        source,
        signed,
        &secp256k1::Secp256k1::verification_only(),
    )
}

/// One wallet, its coins, step 1's claimed prevouts and the Vault target.
struct Fixture {
    wallet: Wallet,
    coins: Vec<SplitCoin>,
    claimed: Vec<OutPoint>,
    target: ScriptBuf,
}

impl Fixture {
    fn new(shape: Shape) -> Self {
        let wallet = wallet(shape);
        let coins = coins(&wallet.source);
        let claimed = step1(&wallet.source, &coins).claimed_prevouts();
        Fixture {
            wallet,
            coins,
            claimed,
            target: vault_target(),
        }
    }
    fn inputs(&self) -> SplitStep2Inputs<'_> {
        inputs(
            &self.wallet.source,
            &self.coins,
            &self.claimed,
            &self.target,
        )
    }
    fn step(&self) -> SplitStep2 {
        create(&self.inputs()).unwrap()
    }
    fn finalize(
        &self,
        step: &SplitStep2,
        signed: &Psbt,
    ) -> Result<VerifiedSplitStep2, FinalizeError> {
        finalize(step, &self.coins, &self.wallet.source, signed)
    }
}

#[test]
fn every_shape_sweeps_exactly_the_claimed_prevouts_into_the_target() {
    for shape in SHAPES {
        let fixture = Fixture::new(shape);
        let wallet = &fixture.wallet;
        let step = fixture.step();
        let tx = &step.psbt().unsigned_tx;

        // Inputs are exactly step 1's claimed prevouts, in the same order.
        assert_eq!(step.claimed_prevouts(), fixture.claimed, "{shape:?}");
        assert!(tx
            .input
            .iter()
            .all(|i| i.sequence == Sequence::ENABLE_RBF_NO_LOCKTIME));
        assert_eq!(tx.version, transaction::Version::TWO);
        // One output, the target: no poison and no change.
        assert_eq!(tx.output.len(), 1);
        assert_eq!(tx.output[0].script_pubkey, fixture.target);
        assert_eq!(step.target(), fixture.target.as_script());
        assert!(step.psbt().outputs[0].bip32_derivation.is_empty());
        assert_eq!(step.chain(), BTCB2);

        assert_eq!(step.fee().to_sat(), step.maximum_signed_vbytes() * FEERATE);
        assert_eq!(
            tx.output[0].value.to_sat(),
            150_000 - step.maximum_signed_vbytes() * FEERATE
        );
        for input in &step.psbt().inputs {
            assert!(input.non_witness_utxo.is_some());
            assert_eq!(input.witness_utxo.is_some(), shape != Shape::Pkh);
            assert!(!input.bip32_derivation.is_empty());
            assert!(input.partial_sigs.is_empty() && input.sighash_type.is_none());
        }
        assert_eq!(create(&fixture.inputs()).unwrap().psbt(), step.psbt());

        let verified = fixture
            .finalize(&step, &sign(step.psbt(), &wallet.signers))
            .unwrap();
        assert_eq!(verified.construction_txid(), step.txid());
        assert_eq!(&unsigned(verified.transaction()), tx);
        assert_eq!(verified.fee(), step.fee());
        assert_eq!(verified.chain(), BTCB2);
        assert_eq!(
            verified.signatures_per_input(),
            &[wallet.threshold, wallet.threshold],
            "{shape:?}"
        );
        assert!(verified.vsize() as u64 <= step.maximum_signed_vbytes());
        assert!(verified.fee().to_sat() >= verified.vsize() as u64 * FEERATE);
    }
}

#[test]
fn a_two_of_three_sortedmulti_finalizes_with_any_two_keys() {
    let fixture = Fixture::new(Shape::WshSortedMulti);
    let step = fixture.step();
    for pair in [[1, 2], [1, 3], [2, 3]] {
        let signers = [master(pair[0]), master(pair[1])];
        let verified = fixture
            .finalize(&step, &sign(step.psbt(), &signers))
            .unwrap();
        assert_eq!(verified.signatures_per_input(), &[2, 2], "{pair:?}");
    }
    assert_eq!(
        fixture
            .finalize(&step, &sign(step.psbt(), &[master(2)]))
            .unwrap_err(),
        FinalizeError::Unsatisfied
    );
}

#[test]
fn step2_runs_on_bitcoin_blake2b_only() {
    let fixture = Fixture::new(Shape::Wpkh);
    let recorded = fixture.step().psbt().unsigned_tx.clone();
    for chain in ChainId::ALL {
        let inputs = SplitStep2Inputs {
            chain,
            ..fixture.inputs()
        };
        let created = create(&inputs);
        let rebuilt = reconstruct_split_step2(&inputs, &recorded, BTCB2_TIP);
        if chain.is_blake2b() {
            assert!(created.is_ok() && rebuilt.is_ok(), "{:?}", chain);
        } else {
            assert_eq!(created.unwrap_err(), Error::NotBitcoinBlake2b(chain));
            assert_eq!(rebuilt.unwrap_err(), Error::NotBitcoinBlake2b(chain));
        }
    }
}

#[test]
fn explicit_sighash_all_is_accepted_like_the_default() {
    for shape in SHAPES {
        let fixture = Fixture::new(shape);
        let step = fixture.step();
        let implicit = fixture
            .finalize(&step, &sign(step.psbt(), &fixture.wallet.signers))
            .unwrap();
        let mut explicit = step.psbt().clone();
        for input in &mut explicit.inputs {
            input.sighash_type = Some(PsbtSighashType::from(EcdsaSighashType::All));
            assert_eq!(input.sighash_type.unwrap().to_u32(), 0x01);
        }
        let explicit = fixture
            .finalize(&step, &sign(&explicit, &fixture.wallet.signers))
            .unwrap();
        assert_eq!(explicit.transaction(), implicit.transaction(), "{shape:?}");
    }
}

#[test]
fn every_other_sighash_refuses() {
    let fixture = Fixture::new(Shape::Wpkh);
    let signers = &fixture.wallet.signers;
    let step = fixture.step();
    for other in [
        EcdsaSighashType::None,
        EcdsaSighashType::Single,
        EcdsaSighashType::AllPlusAnyoneCanPay,
        EcdsaSighashType::NonePlusAnyoneCanPay,
        EcdsaSighashType::SinglePlusAnyoneCanPay,
    ] {
        // A genuine `other` signature on input 1, with and without its request.
        let mut requested = step.psbt().clone();
        requested.inputs[1].sighash_type = Some(PsbtSighashType::from(other));
        let mut genuine = sign(&requested, signers);
        assert_eq!(
            fixture.finalize(&step, &genuine).unwrap_err(),
            FinalizeError::UnsupportedSighash,
            "{other:?}"
        );
        genuine.inputs[1].sighash_type = None;
        assert_eq!(
            fixture.finalize(&step, &genuine).unwrap_err(),
            FinalizeError::UnsupportedSighash,
            "{other:?}"
        );
        // An ALL signature whose request or byte is relabeled.
        let mut request = sign(step.psbt(), signers);
        request.inputs[1].sighash_type = Some(PsbtSighashType::from(other));
        assert_eq!(
            fixture.finalize(&step, &request).unwrap_err(),
            FinalizeError::UnsupportedSighash
        );
        let mut byte = sign(step.psbt(), signers);
        for signature in byte.inputs[1].partial_sigs.values_mut() {
            signature.sighash_type = other;
        }
        assert_eq!(
            fixture.finalize(&step, &byte).unwrap_err(),
            FinalizeError::UnsupportedSighash
        );
    }
    // The BTCB2 unified request (0x21) is not ordinary ALL on step 2.
    let mut unified = sign(step.psbt(), signers);
    unified.inputs[0].sighash_type = Some(PsbtSighashType::from_u32(0x21));
    assert_eq!(
        fixture.finalize(&step, &unified).unwrap_err(),
        FinalizeError::UnsupportedSighash
    );
    // Nor may a unified signature record ride along in the proprietary map.
    let mut proprietary = sign(step.psbt(), signers);
    let (key, signature) = proprietary.inputs[0]
        .partial_sigs
        .iter()
        .next()
        .map(|(k, s)| (*k, *s))
        .unwrap();
    let mut record = signature.signature.serialize_der().to_vec();
    record.push(0x21);
    proprietary.inputs[0].proprietary.insert(
        bitcoin::psbt::raw::ProprietaryKey {
            prefix: b"coincube".to_vec(),
            subtype: 0,
            key: key.to_bytes(),
        },
        record,
    );
    assert_eq!(
        fixture.finalize(&step, &proprietary).unwrap_err(),
        FinalizeError::ConstructionChanged
    );
}

#[test]
fn coins_must_be_exactly_the_claimed_prevouts() {
    let fixture = Fixture::new(Shape::Wpkh);
    let source = &fixture.wallet.source;
    let target = fixture.target.as_script();
    let good = &fixture.coins;
    let claimed = &fixture.claimed;
    let run =
        |coins: &[SplitCoin], claimed: &[OutPoint]| create(&inputs(source, coins, claimed, target));

    // A claimed coin left out, an unclaimed coin added, or another coin swapped in.
    assert_eq!(
        run(&good[..1], claimed).unwrap_err(),
        Error::ClaimedMismatch
    );
    let extra = coin(source, SplitBranch::External, 5, 20_000);
    let mut more = good.clone();
    more.push(extra.clone());
    assert_eq!(run(&more, claimed).unwrap_err(), Error::ClaimedMismatch);
    let swapped = vec![good[0].clone(), extra];
    assert_eq!(run(&swapped, claimed).unwrap_err(), Error::ClaimedMismatch);
    // A duplicated claim is not the claimed set.
    let doubled = vec![claimed[0], claimed[0], claimed[1]];
    assert_eq!(run(good, &doubled).unwrap_err(), Error::ClaimedMismatch);
    assert_eq!(run(&[], &[]).unwrap_err(), Error::Empty);
    assert_eq!(
        run(&[good[0].clone(), good[0].clone()], claimed).unwrap_err(),
        Error::DuplicateInput(good[0].outpoint)
    );
    // Each coin is authenticated and must be shared pre-fork history.
    let mut post_fork = good.clone();
    post_fork[1].btcb2_block = Some(block(FORK));
    assert!(matches!(
        run(&post_fork, claimed).unwrap_err(),
        Error::NotSplittable { .. }
    ));
    let mut lying = good.clone();
    lying[0].index = 1;
    assert_eq!(
        run(&lying, claimed).unwrap_err(),
        Error::ScriptMismatch(good[0].outpoint)
    );
    let mut substituted = good.clone();
    substituted[0].previous = good[1].previous.clone();
    assert!(matches!(
        run(&substituted, claimed).unwrap_err(),
        Error::InputAuthentication { .. }
    ));
    // Caller order of the coins does not matter.
    let reversed: Vec<_> = good.iter().rev().cloned().collect();
    let reversed_claims: Vec<_> = claimed.iter().rev().copied().collect();
    assert_eq!(
        run(&reversed, &reversed_claims).unwrap().psbt(),
        fixture.step().psbt()
    );
}

#[test]
fn target_must_be_usable_and_clear_its_dust_floor() {
    let fixture = Fixture::new(Shape::Wpkh);
    let source = &fixture.wallet.source;
    let with = |target: &bitcoin::Script| {
        create(&inputs(source, &fixture.coins, &fixture.claimed, target))
    };
    let spent = fixture.coins[0].previous.output[1].script_pubkey.clone();
    for target in [
        ScriptBuf::new(),
        ScriptBuf::new_op_return(bitcoin::script::PushBytesBuf::try_from(vec![1u8]).unwrap()),
        spent,
    ] {
        assert_eq!(with(&target).unwrap_err(), Error::InvalidTarget, "{target}");
    }
    // Any other address type is a usable target, Taproot included.
    let secp = secp256k1::Secp256k1::new();
    let key = master(8).private_key.x_only_public_key(&secp).0;
    let fresh_own = source
        .derive(SplitBranch::External, 20)
        .unwrap()
        .script_pubkey()
        .to_owned();
    let other_pkh = ScriptBuf::new_p2pkh(
        &PublicKey::new(master(8).private_key.public_key(&secp)).pubkey_hash(),
    );
    for target in [
        ScriptBuf::new_p2tr(&secp, key, None),
        fresh_own,
        other_pkh.clone(),
    ] {
        assert!(with(&target).is_ok(), "{}", target);
    }

    // The floor is max(500, Core's relay dust for the target): 546 for P2PKH.
    let probe = |sats: u64, target: &bitcoin::Script| {
        let coins = vec![coin(source, SplitBranch::External, 0, sats)];
        let claimed = vec![coins[0].outpoint];
        create_split_step2(
            &inputs(source, &coins, &claimed, target),
            1,
            LockTime::ZERO,
            BTCB2_TIP,
        )
        .map(|step| {
            (
                step.maximum_signed_vbytes(),
                step.psbt().unsigned_tx.output[0].value,
            )
        })
    };
    for (target, floor) in [(fixture.target.clone(), 500), (other_pkh, 546)] {
        let (vbytes, _) = probe(100_000, &target).unwrap();
        let (_, value) = probe(vbytes + floor, &target).unwrap();
        assert_eq!(value.to_sat(), floor);
        assert_eq!(
            probe(vbytes + floor - 1, &target).unwrap_err(),
            Error::Economics
        );
    }
}

#[test]
fn fee_rate_and_locktime_bounds_refuse() {
    let fixture = Fixture::new(Shape::WshMulti);
    let inputs = fixture.inputs();
    for feerate in [0, spend::MAX_FEERATE + 1] {
        assert_eq!(
            create_split_step2(&inputs, feerate, LockTime::ZERO, BTCB2_TIP).unwrap_err(),
            Error::Economics
        );
    }
    let at = |locktime| create_split_step2(&inputs, FEERATE, locktime, BTCB2_TIP);
    assert!(at(LockTime::from_height(BTCB2_TIP).unwrap()).is_ok());
    for refused in [
        LockTime::from_height(BTCB2_TIP + 1).unwrap(),
        LockTime::from_time(500_000_000).unwrap(),
    ] {
        assert_eq!(at(refused).unwrap_err(), Error::Locktime, "{refused:?}");
    }
}

#[test]
fn reconstruction_rebuilds_the_exact_recorded_step() {
    for shape in SHAPES {
        let fixture = Fixture::new(shape);
        let inputs = fixture.inputs();
        let locktime = LockTime::from_height(BTCB2_TIP - 5).unwrap();
        let step = create_split_step2(&inputs, 7, locktime, BTCB2_TIP).unwrap();
        let recorded = step.psbt().unsigned_tx.clone();
        let rebuilt = reconstruct_split_step2(&inputs, &recorded, BTCB2_TIP).unwrap();
        assert_eq!(rebuilt.psbt(), step.psbt(), "{shape:?}");
        assert_eq!(rebuilt.fee(), step.fee());
        assert_eq!(
            rebuilt.maximum_signed_vbytes(),
            step.maximum_signed_vbytes()
        );
        let signed = sign(step.psbt(), &fixture.wallet.signers);
        assert_eq!(
            unsigned(fixture.finalize(&rebuilt, &signed).unwrap().transaction()),
            recorded
        );
        // A tip below the recorded locktime refuses.
        assert_eq!(
            reconstruct_split_step2(&inputs, &recorded, BTCB2_TIP - 6).unwrap_err(),
            Error::Locktime
        );
    }
}

#[test]
fn reconstruction_refuses_a_substituted_or_uneconomic_record() {
    let fixture = Fixture::new(Shape::ShWpkh);
    let inputs = fixture.inputs();
    let step = fixture.step();
    let recorded = step.psbt().unsigned_tx.clone();
    let refuse = |mutate: &dyn Fn(&mut Transaction)| {
        let mut tx = recorded.clone();
        mutate(&mut tx);
        reconstruct_split_step2(&inputs, &tx, BTCB2_TIP).unwrap_err()
    };
    let change = TxOut {
        value: Amount::from_sat(10_000),
        script_pubkey: fixture.coins[0].previous.output[1].script_pubkey.clone(),
    };
    // A change output, or none at all.
    assert!(matches!(
        refuse(&|tx| tx.output.push(change.clone())),
        Error::Recorded(_)
    ));
    assert!(matches!(
        refuse(&|tx| tx.output.clear()),
        Error::Recorded(_)
    ));
    // Another target script.
    assert!(matches!(
        refuse(&|tx| tx.output[0].script_pubkey = change.script_pubkey.clone()),
        Error::Recorded(_)
    ));
    assert!(matches!(
        refuse(&|tx| tx.input[0].sequence = Sequence::MAX),
        Error::Recorded(_)
    ));
    assert!(matches!(
        refuse(&|tx| tx.version = transaction::Version::ONE),
        Error::Recorded(_)
    ));
    assert!(matches!(
        refuse(&|tx| tx.input.reverse()),
        Error::Recorded(_)
    ));
    // Zero, negative and dust outputs.
    for sats in [150_000, 150_001, 100] {
        assert_eq!(
            refuse(&|tx| tx.output[0].value = Amount::from_sat(sats)),
            Error::Economics
        );
    }
    // Another target in the inputs than in the record.
    let other = ScriptBuf::new_p2wsh(&bitcoin::WScriptHash::from_byte_array([9; 32]));
    assert!(matches!(
        reconstruct_split_step2(
            &SplitStep2Inputs {
                target: &other,
                ..inputs
            },
            &recorded,
            BTCB2_TIP
        )
        .unwrap_err(),
        Error::Recorded(_)
    ));
    // A higher fee within bounds is kept.
    let mut higher = recorded.clone();
    higher.output[0].value -= Amount::from_sat(1_000);
    let rebuilt = reconstruct_split_step2(&inputs, &higher, BTCB2_TIP).unwrap();
    assert_eq!(rebuilt.fee(), step.fee() + Amount::from_sat(1_000));
}

#[test]
fn finalization_refuses_any_change_to_the_construction() {
    let fixture = Fixture::new(Shape::ShWpkh);
    let step = fixture.step();
    let signed = sign(step.psbt(), &fixture.wallet.signers);
    let secp = secp256k1::Secp256k1::new();
    let internal_key = PublicKey::new(master(9).private_key.public_key(&secp));
    let changes: Vec<Change> = vec![
        Box::new(|p| p.unsigned_tx.output[0].value = Amount::from_sat(1_000)),
        Box::new(|p| p.unsigned_tx.input.reverse()),
        Box::new(|p| {
            p.inputs.pop();
        }),
        Box::new(|p| p.inputs[0].bip32_derivation.clear()),
        Box::new(|p| p.inputs[0].redeem_script = None),
        Box::new(|p| p.inputs[0].witness_utxo = None),
        Box::new(|p| {
            p.outputs[0].redeem_script = Some(ScriptBuf::new());
        }),
        Box::new(move |p| p.inputs[0].tap_internal_key = Some(internal_key.inner.into())),
        Box::new(|p| p.inputs[0].final_script_witness = Some(bitcoin::Witness::new())),
        Box::new(|p| {
            p.inputs[1].unknown.insert(
                bitcoin::psbt::raw::Key {
                    type_value: 0xfc,
                    key: vec![1],
                },
                vec![2],
            );
        }),
    ];
    for (index, change) in changes.iter().enumerate() {
        let mut tampered = signed.clone();
        change(&mut tampered);
        assert_eq!(
            fixture.finalize(&step, &tampered).unwrap_err(),
            FinalizeError::ConstructionChanged,
            "change {index}"
        );
    }
    assert!(fixture.finalize(&step, &signed).is_ok());
}

#[test]
fn finalization_verifies_every_signature() {
    let fixture = Fixture::new(Shape::Wpkh);
    let step = fixture.step();
    let signed = sign(step.psbt(), &fixture.wallet.signers);
    let first = |psbt: &Psbt, input: usize| {
        psbt.inputs[input]
            .partial_sigs
            .iter()
            .next()
            .map(|(k, s)| (*k, *s))
            .unwrap()
    };
    let (key0, sig0) = first(&signed, 0);
    let (key1, sig1) = first(&signed, 1);

    // A valid signature over another input's digest.
    let mut wrong_digest = signed.clone();
    wrong_digest.inputs[0].partial_sigs.insert(key0, sig1);
    assert_eq!(
        fixture.finalize(&step, &wrong_digest).unwrap_err(),
        FinalizeError::InvalidSignature { input: 0 }
    );
    // A surplus signature by a key the input does not commit to.
    let mut foreign = signed.clone();
    foreign.inputs[0].partial_sigs.insert(key1, sig1);
    assert_eq!(
        fixture.finalize(&step, &foreign).unwrap_err(),
        FinalizeError::InvalidSignature { input: 0 }
    );
    // A stranger's signature that is valid over this input's digest.
    let secp = secp256k1::Secp256k1::new();
    let stranger = master(9);
    let stranger_key = stranger.to_priv().public_key(&secp);
    let mut probe = step.psbt().clone();
    probe.inputs[0].bip32_derivation = std::iter::once((
        stranger_key.inner,
        (stranger.fingerprint(&secp), DerivationPath::master()),
    ))
    .collect();
    probe.sign(&stranger, &secp).unwrap();
    let mut surplus = signed.clone();
    surplus.inputs[0]
        .partial_sigs
        .insert(stranger_key, probe.inputs[0].partial_sigs[&stranger_key]);
    assert_eq!(
        fixture.finalize(&step, &surplus).unwrap_err(),
        FinalizeError::InvalidSignature { input: 0 }
    );
    // An uncompressed key never signs a segwit input.
    let mut uncompressed = signed.clone();
    uncompressed.inputs[0].partial_sigs.clear();
    uncompressed.inputs[0].partial_sigs.insert(
        PublicKey {
            compressed: false,
            inner: key0.inner,
        },
        sig0,
    );
    assert_eq!(
        fixture.finalize(&step, &uncompressed).unwrap_err(),
        FinalizeError::InvalidSignature { input: 0 }
    );
    // Missing signatures.
    assert_eq!(
        fixture.finalize(&step, step.psbt()).unwrap_err(),
        FinalizeError::Unsatisfied
    );
    let mut partial = signed.clone();
    partial.inputs[1].partial_sigs.clear();
    assert_eq!(
        fixture.finalize(&step, &partial).unwrap_err(),
        FinalizeError::Unsatisfied
    );
}

#[test]
fn step1_signatures_do_not_sign_step2() {
    // The same keys, inputs and prevouts: only the digest differs.
    for shape in SHAPES {
        let fixture = Fixture::new(shape);
        let step1 = step1(&fixture.wallet.source, &fixture.coins);
        let signed1 = sign(step1.psbt(), &fixture.wallet.signers);
        let step = fixture.step();
        let mut grafted = step.psbt().clone();
        for (input, signed) in grafted.inputs.iter_mut().zip(&signed1.inputs) {
            input.partial_sigs = signed.partial_sigs.clone();
        }
        assert_eq!(
            fixture.finalize(&step, &grafted).unwrap_err(),
            FinalizeError::InvalidSignature { input: 0 },
            "{shape:?}"
        );
        // And a step-2 PSBT is not a step-1 construction.
        let signed2 = sign(step.psbt(), &fixture.wallet.signers);
        assert_eq!(
            finalize_split_step1(&step1, &signed2, &secp256k1::Secp256k1::new()).unwrap_err(),
            FinalizeError::ConstructionChanged
        );
    }
}

#[test]
fn finalization_binds_the_supplied_coins_and_source() {
    let fixture = Fixture::new(Shape::Wpkh);
    let step = fixture.step();
    let signed = sign(step.psbt(), &fixture.wallet.signers);
    let source = &fixture.wallet.source;
    let good = &fixture.coins;
    let refuse = |coins: &[SplitCoin], source: &SplitSource| {
        finalize(&step, coins, source, &signed).unwrap_err()
    };
    // Another wallet, or the same wallet without its change descriptor.
    let other = self::wallet(Shape::Pkh).source;
    assert_eq!(refuse(good, &other), FinalizeError::CoinsChanged);
    let external_only = SplitSource::new(source.external().clone(), None).unwrap();
    assert_eq!(refuse(good, &external_only), FinalizeError::CoinsChanged);
    // A coin missing, added, relabeled, substituted, or no longer pre-fork.
    assert_eq!(refuse(&good[..1], source), FinalizeError::CoinsChanged);
    let mut more = good.clone();
    more.push(coin(source, SplitBranch::External, 5, 20_000));
    assert_eq!(refuse(&more, source), FinalizeError::CoinsChanged);
    let mut relabeled = good.clone();
    relabeled[0].index = 1;
    assert_eq!(refuse(&relabeled, source), FinalizeError::CoinsChanged);
    let mut substituted = good.clone();
    substituted[0].previous = good[1].previous.clone();
    assert_eq!(refuse(&substituted, source), FinalizeError::CoinsChanged);
    let mut reorged = good.clone();
    reorged[1].bitcoin_block = None;
    assert_eq!(refuse(&reorged, source), FinalizeError::CoinsChanged);
    // Caller order is irrelevant.
    let reversed: Vec<_> = good.iter().rev().cloned().collect();
    assert!(finalize(&step, &reversed, source, &signed).is_ok());
}

#[test]
fn every_key_must_change_the_same_branch_step() {
    // #568 I1: one receive/change step pair for the whole wallet.
    let keys: Vec<_> = (1..=3)
        .map(|seed| account(&master(seed), "m/48'/0'/0'/2'"))
        .collect();
    let multi = |branch: [u32; 3]| {
        descriptor(&format!(
            "wsh(sortedmulti(2,{}/{}/*,{}/{}/*,{}/{}/*))",
            keys[0], branch[0], keys[1], branch[1], keys[2], branch[2]
        ))
    };
    let external = multi([0, 0, 0]);
    for (internal, ok) in [
        ([1, 1, 1], true),
        ([0, 0, 0], true),
        ([7, 7, 7], true),
        ([1, 1, 7], false),
        ([1, 0, 1], false),
    ] {
        assert_eq!(
            SplitSource::new(external.clone(), Some(multi(internal))).is_ok(),
            ok,
            "{internal:?}"
        );
        if !ok {
            assert_eq!(
                SplitSource::new(external.clone(), Some(multi(internal))),
                Err(Error::UnrelatedInternal)
            );
        }
    }
    // The external side must itself move the same way on every key.
    assert_eq!(
        SplitSource::new(multi([0, 0, 2]), Some(multi([1, 1, 1]))),
        Err(Error::UnrelatedInternal)
    );
}
