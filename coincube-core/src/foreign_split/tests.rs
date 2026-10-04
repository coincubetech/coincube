//! Signatures here come from rust-bitcoin's reference PSBT signer
//! (`Psbt::sign`), which chooses the legacy or BIP 143 digest itself, so the
//! finalizer's own digest selection is checked against an independent
//! implementation. Every finalized witness is also replayed by Miniscript's
//! interpreter. The repository's Knots vectors cover only the BTCB2 unified
//! digest; step 1 uses ordinary Bitcoin `SIGHASH_ALL`, for which the two-chain
//! regtest (#568 B6) is the node-acceptance check.

use super::*;
use miniscript::bitcoin::{
    bip32::{DerivationPath, Xpriv, Xpub},
    psbt::PsbtSighashType,
    Network, PublicKey,
};
use std::{convert::TryFrom, str::FromStr};

const FORK: u64 = 900;

type Mutation<T> = fn(&mut T);
type Change = Box<dyn Fn(&mut Psbt)>;
const FEERATE: u64 = 5;
/// Observed Bitcoin tip height for locktime checks.
const TIP: u32 = 860_000;

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
    /// Signatures needed per input.
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
                script_pubkey: op_return(vec![index as u8]),
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

fn inputs<'a>(source: &'a SplitSource, coins: &'a [SplitCoin]) -> SplitInputs<'a> {
    SplitInputs {
        chain: ChainId::Bitcoin,
        source,
        coins,
        fork_height: FORK,
        destination: 9,
    }
}

fn op_return(data: Vec<u8>) -> ScriptBuf {
    ScriptBuf::new_op_return(bitcoin::script::PushBytesBuf::try_from(data).unwrap())
}

fn marker() -> BlockHash {
    BlockHash::from_byte_array([42; 32])
}

fn create(inputs: &SplitInputs<'_>) -> Result<SplitStep1, Error> {
    create_split_step1(inputs, FEERATE, LockTime::ZERO, TIP, marker())
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

fn finalize(step: &SplitStep1, signed: &Psbt) -> Result<VerifiedSplitStep1, FinalizeError> {
    finalize_split_step1(step, signed, &secp256k1::Secp256k1::verification_only())
}

#[test]
fn every_supported_shape_builds_signs_and_finalizes() {
    for shape in SHAPES {
        let wallet = wallet(shape);
        let coins = coins(&wallet.source);
        let inputs = inputs(&wallet.source, &coins);
        let step = create(&inputs).unwrap();
        let tx = &step.psbt().unsigned_tx;

        // Inputs are exactly the selected coins, ordered by outpoint.
        let mut outpoints: Vec<_> = coins.iter().map(|c| c.outpoint).collect();
        outpoints.sort();
        assert_eq!(step.claimed_prevouts(), outpoints, "{shape:?}");
        assert!(tx
            .input
            .iter()
            .all(|i| i.sequence == Sequence::ENABLE_RBF_NO_LOCKTIME));

        // Output 0 is Claim's shared poison over the same outpoint set.
        let poison = split_poison_script(
            ChainId::Bitcoin,
            marker(),
            &outpoints.iter().copied().collect(),
        )
        .unwrap();
        assert_eq!(tx.output.len(), 2);
        assert_eq!(tx.output[0].value, Amount::ZERO);
        assert_eq!(tx.output[0].script_pubkey, poison);
        assert_eq!(step.fork_marker(), marker());
        // Output 1 is the fresh address of the same wallet, marked as its own.
        let destination = wallet
            .source
            .derive(SplitBranch::External, 9)
            .unwrap()
            .script_pubkey();
        assert_eq!(tx.output[1].script_pubkey, destination);
        assert!(!step.psbt().outputs[1].bip32_derivation.is_empty());

        // The fee is charged for the worst-case signed size, poison included.
        assert_eq!(step.fee().to_sat(), step.maximum_signed_vbytes() * FEERATE);
        assert_eq!(
            tx.output[1].value.to_sat(),
            150_000 - step.maximum_signed_vbytes() * FEERATE
        );
        for input in &step.psbt().inputs {
            assert!(input.non_witness_utxo.is_some());
            assert_eq!(input.witness_utxo.is_some(), shape != Shape::Pkh);
            assert!(!input.bip32_derivation.is_empty());
            assert!(input.partial_sigs.is_empty() && input.sighash_type.is_none());
        }
        // Deterministic.
        assert_eq!(create(&inputs).unwrap().psbt(), step.psbt());

        let verified = finalize(&step, &sign(step.psbt(), &wallet.signers)).unwrap();
        assert_eq!(verified.construction_txid(), step.txid());
        assert_eq!(&unsigned(verified.transaction()), tx);
        // Only native segwit spends keep the unsigned txid once signed; the
        // step to track on chain is always the finalized transaction's.
        assert_eq!(
            verified.transaction().compute_txid() == step.txid(),
            matches!(shape, Shape::Wpkh | Shape::WshSortedMulti | Shape::WshMulti),
            "{shape:?}"
        );
        assert_eq!(verified.fee(), step.fee());
        assert_eq!(verified.chain(), ChainId::Bitcoin);
        assert_eq!(
            verified.signatures_per_input(),
            &[wallet.threshold, wallet.threshold],
            "{shape:?}"
        );
        // The worst-case size bounds the actual size, so the paid rate is
        // never below the requested one.
        assert!(
            verified.vsize() as u64 <= step.maximum_signed_vbytes(),
            "{:?}",
            shape
        );
        assert!(verified.fee().to_sat() >= verified.vsize() as u64 * FEERATE);
    }
}

#[test]
fn explicit_sighash_all_is_accepted_like_the_default() {
    // Owner decision F1 (#585): an explicit 0x01 request is the default.
    for shape in SHAPES {
        let wallet = wallet(shape);
        let coins = coins(&wallet.source);
        let step = create(&inputs(&wallet.source, &coins)).unwrap();
        let implicit = finalize(&step, &sign(step.psbt(), &wallet.signers)).unwrap();
        let mut explicit = step.psbt().clone();
        for input in &mut explicit.inputs {
            input.sighash_type = Some(PsbtSighashType::from(EcdsaSighashType::All));
            assert_eq!(input.sighash_type.unwrap().to_u32(), 0x01);
        }
        let explicit = finalize(&step, &sign(&explicit, &wallet.signers)).unwrap();
        // RFC 6979 signing is deterministic: the same transaction results.
        assert_eq!(explicit.transaction(), implicit.transaction(), "{shape:?}");
    }
}

#[test]
fn every_other_sighash_refuses_including_a_mixed_psbt() {
    let wallet = wallet(Shape::Wpkh);
    let coins = coins(&wallet.source);
    let step = create(&inputs(&wallet.source, &coins)).unwrap();
    for other in [
        EcdsaSighashType::None,
        EcdsaSighashType::Single,
        EcdsaSighashType::AllPlusAnyoneCanPay,
        EcdsaSighashType::NonePlusAnyoneCanPay,
        EcdsaSighashType::SinglePlusAnyoneCanPay,
    ] {
        // Input 0 signs ALL, input 1 signs `other`.
        let mut mixed = step.psbt().clone();
        mixed.inputs[1].sighash_type = Some(PsbtSighashType::from(other));
        let signed = sign(&mixed, &wallet.signers);
        assert_eq!(
            signed.inputs[1]
                .partial_sigs
                .values()
                .next()
                .unwrap()
                .sighash_type,
            other
        );
        assert_eq!(
            finalize(&step, &signed).unwrap_err(),
            FinalizeError::UnsupportedSighash,
            "{other:?}"
        );
        // An ALL signature cannot be relabeled either.
        let mut relabeled = sign(step.psbt(), &wallet.signers);
        relabeled.inputs[1].sighash_type = Some(PsbtSighashType::from(other));
        assert_eq!(
            finalize(&step, &relabeled).unwrap_err(),
            FinalizeError::UnsupportedSighash
        );
    }
    // Nor may a unified BTCB2 request (0x21) appear on the Bitcoin step.
    let mut unified = sign(step.psbt(), &wallet.signers);
    unified.inputs[0].sighash_type = Some(PsbtSighashType::from_u32(0x21));
    assert_eq!(
        finalize(&step, &unified).unwrap_err(),
        FinalizeError::UnsupportedSighash
    );
}

#[test]
fn taproot_and_unsupported_sources_refuse() {
    let key = account(&master(1), "m/86'/0'/0'");
    let tr = descriptor(&format!("tr({key}/0/*)"));
    let wpkh = descriptor(&format!("wpkh({key}/0/*)"));
    assert_eq!(SplitSource::new(tr.clone(), None), Err(Error::Taproot));
    assert_eq!(
        SplitSource::new(wpkh.clone(), Some(tr)),
        Err(Error::Taproot)
    );
    for text in [
        // Arbitrary wsh policies and nested segwit scripts have no route.
        format!("wsh(pkh({key}/0/*))"),
        format!("sh(wsh(multi(1,{key}/0/*)))"),
        format!("sh(multi(1,{key}/0/*))"),
        // Ambiguous multipath and hardened public derivation.
        format!("wpkh({key}/<0;1>/*)"),
        format!("wpkh({key}/0/*')"),
        format!("wpkh({key}/0'/*)"),
    ] {
        assert_eq!(
            SplitSource::new(descriptor(&text), None),
            Err(Error::UnsupportedDescriptor),
            "{text}"
        );
    }
    assert!(SplitSource::new(wpkh, None).is_ok());
}

#[test]
fn only_coins_confirmed_pre_fork_in_the_same_block_on_both_chains_are_spent() {
    let wallet = wallet(Shape::Wpkh);
    let good = coins(&wallet.source);
    let cases: [(Mutation<SplitCoin>, NotSplittable); 6] = [
        // BTCB2-only (spent on Bitcoin, or created on BTCB2 after the fork).
        (
            |c| c.bitcoin_block = None,
            NotSplittable::NoBitcoinConfirmation,
        ),
        (|c| c.btcb2_block = None, NotSplittable::NoBtcb2Confirmation),
        (
            |c| {
                c.bitcoin_block = Some(block(FORK));
                c.btcb2_block = Some(block(FORK));
            },
            NotSplittable::PostFork,
        ),
        (
            |c| {
                c.bitcoin_block = Some(block(FORK + 5));
                c.btcb2_block = Some(block(FORK - 10));
            },
            NotSplittable::PostFork,
        ),
        (
            |c| c.btcb2_block = Some(block(FORK - 11)),
            NotSplittable::ChainsDisagree,
        ),
        (
            |c| {
                c.btcb2_block = Some(BlockRef {
                    height: FORK - 10,
                    hash: BlockHash::from_byte_array([0xee; 32]),
                })
            },
            NotSplittable::ChainsDisagree,
        ),
    ];
    for (mutate, reason) in cases {
        let mut coins = good.clone();
        mutate(&mut coins[1]);
        // One unsplittable coin refuses the whole step: never silently dropped.
        assert_eq!(
            create(&inputs(&wallet.source, &coins)).unwrap_err(),
            Error::NotSplittable {
                outpoint: coins[1].outpoint,
                reason
            }
        );
    }
    // The first pre-fork height is FORK - 1.
    let mut coins = good;
    coins[1].bitcoin_block = Some(block(FORK - 1));
    coins[1].btcb2_block = Some(block(FORK - 1));
    assert!(create(&inputs(&wallet.source, &coins)).is_ok());
}

#[test]
fn step1_runs_on_bitcoin_only() {
    let wallet = wallet(Shape::Wpkh);
    let coins = coins(&wallet.source);
    for chain in ChainId::ALL {
        let result = create(&SplitInputs {
            chain,
            ..inputs(&wallet.source, &coins)
        });
        if matches!(chain, ChainId::Bitcoin | ChainId::Testnet4) {
            assert!(result.is_ok(), "{:?}", chain);
        } else {
            assert_eq!(result.unwrap_err(), Error::UnsupportedChain(chain));
        }
    }
}

#[test]
fn inconsistent_coins_and_destinations_refuse() {
    let wallet = wallet(Shape::Wpkh);
    let source = &wallet.source;
    let good = coins(source);
    let run = |coins: &[SplitCoin], destination| {
        create(&SplitInputs {
            destination,
            ..inputs(source, coins)
        })
    };
    let destination = 9;
    assert_eq!(run(&[], destination).unwrap_err(), Error::Empty);
    let duplicate = vec![good[0].clone(), good[0].clone()];
    assert_eq!(
        run(&duplicate, destination).unwrap_err(),
        Error::DuplicateInput(good[0].outpoint)
    );
    assert_eq!(run(&good, 1 << 31).unwrap_err(), Error::InvalidIndex);
    // The stated derivation must produce the authenticated script.
    let mut lying = good.clone();
    lying[0].index = 1;
    assert_eq!(
        run(&lying, destination).unwrap_err(),
        Error::ScriptMismatch(good[0].outpoint)
    );
    // The previous transaction authenticates the prevout.
    let mut substituted = good.clone();
    substituted[0].previous = good[1].previous.clone();
    assert!(matches!(
        run(&substituted, destination).unwrap_err(),
        Error::InputAuthentication {
            reason: InputAuthError::TxidMismatch { .. },
            ..
        }
    ));
    let mut vout = good.clone();
    vout[0].outpoint.vout = 7;
    vout[0].previous = good[0].previous.clone();
    assert!(matches!(
        run(&vout, destination).unwrap_err(),
        Error::InputAuthentication { .. }
    ));
    // The destination must not be a spent script (coin 0 is receive index 0).
    assert_eq!(run(&good, 0).unwrap_err(), Error::DestinationNotFresh);
    // Internal coins need the internal descriptor.
    let external_only = SplitSource::new(source.external().clone(), None).unwrap();
    assert_eq!(
        create(&inputs(&external_only, &good)).unwrap_err(),
        Error::MissingInternal
    );
    // A fixed descriptor has one address: no fresh destination exists.
    let secp = secp256k1::Secp256k1::new();
    let key = PublicKey::new(master(4).private_key.public_key(&secp));
    let fixed = SplitSource::new(descriptor(&format!("wpkh({key})")), None).unwrap();
    let fixed_coin = coin(&fixed, SplitBranch::External, 0, 100_000);
    assert_eq!(
        create(&SplitInputs {
            destination: 0,
            ..inputs(&fixed, std::slice::from_ref(&fixed_coin))
        })
        .unwrap_err(),
        Error::DestinationNotFresh
    );
}

#[test]
fn fee_rate_and_dust_bounds_refuse() {
    let wallet = wallet(Shape::Wpkh);
    let coins = coins(&wallet.source);
    let inputs = inputs(&wallet.source, &coins);
    for feerate in [0, spend::MAX_FEERATE + 1] {
        assert_eq!(
            create_split_step1(&inputs, feerate, LockTime::ZERO, TIP, marker()).unwrap_err(),
            Error::Economics
        );
    }
    // The top rate is allowed when the coins can pay it.
    let rich = vec![coin(&wallet.source, SplitBranch::External, 0, 10_000_000)];
    assert!(create_split_step1(
        &SplitInputs {
            coins: &rich,
            ..inputs
        },
        spend::MAX_FEERATE,
        LockTime::ZERO,
        TIP,
        marker()
    )
    .is_ok());
    let small = vec![coin(&wallet.source, SplitBranch::External, 0, 1_500)];
    let small_inputs = SplitInputs {
        coins: &small,
        ..inputs
    };
    // 1,500 sats cannot pay ~160 vB at 10 sat/vB and keep a non-dust output.
    assert_eq!(
        create_split_step1(&small_inputs, 10, LockTime::ZERO, TIP, marker()).unwrap_err(),
        Error::Economics
    );
    assert!(create_split_step1(&small_inputs, 1, LockTime::ZERO, TIP, marker()).is_ok());
}

#[test]
fn reconstruction_rebuilds_the_exact_recorded_step() {
    for shape in SHAPES {
        let wallet = wallet(shape);
        let coins = coins(&wallet.source);
        let inputs = inputs(&wallet.source, &coins);
        let locktime = LockTime::from_height(850_000).unwrap();
        let step = create_split_step1(&inputs, 7, locktime, TIP, marker()).unwrap();
        let recorded = step.psbt().unsigned_tx.clone();
        let rebuilt = reconstruct_split_step1(&inputs, &recorded, TIP).unwrap();
        // Every field is rebuilt, not restored, and still identical.
        assert_eq!(rebuilt.psbt(), step.psbt(), "{shape:?}");
        assert_eq!(rebuilt.fee(), step.fee());
        assert_eq!(rebuilt.fork_marker(), marker());
        assert_eq!(
            rebuilt.maximum_signed_vbytes(),
            step.maximum_signed_vbytes()
        );
        // A reconstructed step finalizes the original's signatures.
        let signed = sign(step.psbt(), &wallet.signers);
        assert_eq!(
            unsigned(finalize(&rebuilt, &signed).unwrap().transaction()),
            step.psbt().unsigned_tx
        );
        // Input order in the caller's coin list does not matter.
        let reversed: Vec<_> = coins.iter().rev().cloned().collect();
        assert!(reconstruct_split_step1(
            &SplitInputs {
                coins: &reversed,
                ..inputs
            },
            &recorded,
            TIP
        )
        .is_ok());
    }
}

#[test]
fn reconstruction_refuses_a_substituted_or_uneconomic_record() {
    let wallet = wallet(Shape::Wpkh);
    let coins = coins(&wallet.source);
    let inputs = inputs(&wallet.source, &coins);
    let step = create(&inputs).unwrap();
    let recorded = step.psbt().unsigned_tx.clone();
    let refuse = |mutate: &dyn Fn(&mut Transaction)| {
        let mut tx = recorded.clone();
        mutate(&mut tx);
        reconstruct_split_step1(&inputs, &tx, TIP).unwrap_err()
    };
    assert!(matches!(
        refuse(&|tx| {
            tx.output.remove(0);
        }),
        Error::Recorded(_)
    ));
    assert!(matches!(
        refuse(&|tx| tx.output[0].script_pubkey = op_return(vec![0u8; 40])),
        Error::Recorded(_)
    ));
    assert!(matches!(
        refuse(&|tx| tx.output.swap(0, 1)),
        Error::Recorded(_)
    ));
    // A foreign destination script.
    assert!(matches!(
        refuse(&|tx| tx.output[1].script_pubkey = coins[0].previous.output[1].script_pubkey.clone()),
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
    // A poison committing to a different input set does not rebuild.
    assert!(matches!(
        refuse(&|tx| {
            let other: BTreeSet<_> = std::iter::once(coins[0].outpoint).collect();
            tx.output[0].script_pubkey =
                split_poison_script(ChainId::Bitcoin, marker(), &other).unwrap();
        }),
        Error::Recorded(_)
    ));
    // Zero, negative and over-cap fees.
    assert_eq!(
        refuse(&|tx| tx.output[1].value = Amount::from_sat(150_000)),
        Error::Economics
    );
    assert_eq!(
        refuse(&|tx| tx.output[1].value = Amount::from_sat(150_001)),
        Error::Economics
    );
    assert_eq!(
        refuse(&|tx| tx.output[1].value = Amount::from_sat(100)),
        Error::Economics
    );
    // Inputs differ from the recorded ones (here the recorded output also
    // exceeds the remaining coin, so economics refuse first).
    assert!(matches!(
        reconstruct_split_step1(
            &SplitInputs {
                coins: &coins[..1],
                ..inputs
            },
            &recorded,
            TIP
        )
        .unwrap_err(),
        Error::Recorded(_) | Error::Economics
    ));
    let mut swapped = coins.clone();
    swapped[1] = coin(&wallet.source, SplitBranch::Internal, 4, 50_000);
    assert!(matches!(
        reconstruct_split_step1(
            &SplitInputs {
                coins: &swapped,
                ..inputs
            },
            &recorded,
            TIP
        )
        .unwrap_err(),
        Error::Recorded(_)
    ));
    assert!(matches!(
        reconstruct_split_step1(
            &SplitInputs {
                destination: 10,
                ..inputs
            },
            &recorded,
            TIP
        )
        .unwrap_err(),
        Error::Recorded(_)
    ));
    // A higher fee than estimated is fine when within bounds.
    let mut higher = recorded.clone();
    higher.output[1].value -= Amount::from_sat(1_000);
    let rebuilt = reconstruct_split_step1(&inputs, &higher, TIP).unwrap();
    assert_eq!(rebuilt.fee(), step.fee() + Amount::from_sat(1_000));
}

#[test]
fn finalization_refuses_any_change_to_the_construction() {
    let wallet = wallet(Shape::ShWpkh);
    let coins = coins(&wallet.source);
    let step = create(&inputs(&wallet.source, &coins)).unwrap();
    let signed = sign(step.psbt(), &wallet.signers);
    let secp = secp256k1::Secp256k1::new();
    let internal_key = PublicKey::new(master(9).private_key.public_key(&secp));
    let changes: Vec<Change> = vec![
        Box::new(|p| p.unsigned_tx.output[1].value = Amount::from_sat(1_000)),
        Box::new(|p| p.unsigned_tx.input.reverse()),
        Box::new(|p| {
            p.inputs.pop();
        }),
        Box::new(|p| p.inputs[0].bip32_derivation.clear()),
        Box::new(|p| p.inputs[0].redeem_script = None),
        Box::new(|p| p.inputs[0].witness_utxo = None),
        Box::new(|p| p.outputs[1].bip32_derivation.clear()),
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
            finalize(&step, &tampered).unwrap_err(),
            FinalizeError::ConstructionChanged,
            "change {index}"
        );
    }
    assert!(finalize(&step, &signed).is_ok());
}

#[test]
fn finalization_verifies_every_signature_and_the_quorum() {
    let wallet = wallet(Shape::Wpkh);
    let coins = coins(&wallet.source);
    let step = create(&inputs(&wallet.source, &coins)).unwrap();
    let signed = sign(step.psbt(), &wallet.signers);
    let (key0, sig0) = signed.inputs[0]
        .partial_sigs
        .iter()
        .next()
        .map(|(k, s)| (*k, *s))
        .unwrap();
    let (key1, sig1) = signed.inputs[1]
        .partial_sigs
        .iter()
        .next()
        .map(|(k, s)| (*k, *s))
        .unwrap();

    // A valid signature over another input's digest.
    let mut wrong_digest = signed.clone();
    wrong_digest.inputs[0].partial_sigs.insert(key0, sig1);
    assert_eq!(
        finalize(&step, &wrong_digest).unwrap_err(),
        FinalizeError::InvalidSignature { input: 0 }
    );
    // A surplus signature by a key the input does not commit to.
    let mut foreign = signed.clone();
    foreign.inputs[0].partial_sigs.insert(key1, sig1);
    assert_eq!(
        finalize(&step, &foreign).unwrap_err(),
        FinalizeError::InvalidSignature { input: 0 }
    );
    // Even when that surplus signature is valid over this input's digest:
    // the finalizer would ignore it, so the key check must refuse it.
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
    let valid_stranger = probe.inputs[0].partial_sigs[&stranger_key];
    let mut surplus = signed.clone();
    surplus.inputs[0]
        .partial_sigs
        .insert(stranger_key, valid_stranger);
    assert_eq!(
        finalize(&step, &surplus).unwrap_err(),
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
        finalize(&step, &uncompressed).unwrap_err(),
        FinalizeError::InvalidSignature { input: 0 }
    );
    // Missing signatures.
    assert_eq!(
        finalize(&step, step.psbt()).unwrap_err(),
        FinalizeError::Unsatisfied
    );
    let mut partial = signed.clone();
    partial.inputs[1].partial_sigs.clear();
    assert_eq!(
        finalize(&step, &partial).unwrap_err(),
        FinalizeError::Unsatisfied
    );

    // A 2-of-3 with one signature is unsatisfied; any two keys satisfy it.
    let multisig = self::wallet(Shape::WshSortedMulti);
    let coins = self::coins(&multisig.source);
    let step = create(&inputs(&multisig.source, &coins)).unwrap();
    assert_eq!(
        finalize(&step, &sign(step.psbt(), &multisig.signers[..1])).unwrap_err(),
        FinalizeError::Unsatisfied
    );
    let second_and_third = [master(2), master(3)];
    let verified = finalize(&step, &sign(step.psbt(), &second_and_third)).unwrap();
    assert_eq!(verified.signatures_per_input(), &[2, 2]);
    // All three signatures verify; the witness keeps two.
    let all = [master(1), master(2), master(3)];
    let verified = finalize(&step, &sign(step.psbt(), &all)).unwrap();
    assert_eq!(verified.signatures_per_input(), &[2, 2]);
}

#[test]
fn a_legacy_only_step_has_no_witness_overhead() {
    let wallet = wallet(Shape::Pkh);
    let coins = coins(&wallet.source);
    let step = create(&inputs(&wallet.source, &coins)).unwrap();
    let verified = finalize(&step, &sign(step.psbt(), &wallet.signers)).unwrap();
    let tx = verified.transaction();
    assert!(tx.input.iter().all(|i| i.witness.is_empty()));
    // P2PKH signatures are 71-73 bytes: the estimate is within a few vbytes.
    assert!(step.maximum_signed_vbytes() - (tx.vsize() as u64) <= 4);
}

#[test]
fn destination_must_clear_core_relay_dust_for_each_shape() {
    for shape in SHAPES {
        let wallet = wallet(shape);
        let script = wallet
            .source
            .derive(SplitBranch::External, 9)
            .unwrap()
            .script_pubkey();
        // Core's dust threshold at the default dust relay fee.
        let core_dust = script.minimal_non_dust().to_sat();
        let expected = match shape {
            Shape::Pkh => 546,
            Shape::ShWpkh => 540,
            Shape::Wpkh => 294,
            Shape::WshSortedMulti | Shape::WshMulti => 330,
        };
        assert_eq!(core_dust, expected, "{shape:?}");
        let floor = core_dust.max(spend::DUST_OUTPUT_SATS);
        // The worst-case size does not depend on the coin's value.
        let probe = vec![coin(&wallet.source, SplitBranch::External, 0, 100_000)];
        let vbytes = create(&SplitInputs {
            coins: &probe,
            ..inputs(&wallet.source, &probe)
        })
        .unwrap()
        .maximum_signed_vbytes();
        let at = |sats: u64| {
            let coins = vec![coin(&wallet.source, SplitBranch::External, 0, sats)];
            let inputs = inputs(&wallet.source, &coins);
            create_split_step1(&inputs, 1, LockTime::ZERO, TIP, marker())
        };
        let step = at(vbytes + floor).unwrap();
        assert_eq!(step.psbt().unsigned_tx.output[1].value.to_sat(), floor);
        assert_eq!(
            at(vbytes + floor - 1).unwrap_err(),
            Error::Economics,
            "{shape:?}"
        );

        // Reconstruction and finalization apply the same floor.
        let coins = vec![coin(
            &wallet.source,
            SplitBranch::External,
            0,
            vbytes + floor,
        )];
        let inputs = inputs(&wallet.source, &coins);
        let mut below = step.psbt().unsigned_tx.clone();
        below.output[1].value = Amount::from_sat(floor - 1);
        assert_eq!(
            reconstruct_split_step1(&inputs, &below, TIP).unwrap_err(),
            Error::Economics,
            "{shape:?}"
        );
        assert!(finalize(&step, &sign(step.psbt(), &wallet.signers)).is_ok());
    }
}

#[test]
fn change_descriptor_must_be_the_same_wallet() {
    for shape in SHAPES {
        // The fixture's external/internal pair differs only in the branch step.
        let wallet = wallet(shape);
        let external = wallet.source.external().clone();
        let internal = wallet.source.internal().unwrap().clone();
        assert!(SplitSource::new(external.clone(), Some(internal)).is_ok());
        // Another shape of the same keys is not the same wallet.
        let other_shape = self::wallet(if shape == Shape::Wpkh {
            Shape::Pkh
        } else {
            Shape::Wpkh
        });
        assert_eq!(
            SplitSource::new(external.clone(), other_shape.source.internal().cloned()),
            Err(Error::UnrelatedInternal),
            "{shape:?}"
        );
    }
    let a = account(&master(1), "m/84'/0'/0'");
    let b = account(&master(7), "m/84'/0'/0'");
    let c = account(&master(1), "m/84'/0'/1'");
    let external = descriptor(&format!("wpkh({a}/0/*)"));
    for (internal, ok) in [
        (format!("wpkh({a}/1/*)"), true),
        // Unrelated master: step 1 would otherwise be able to pay another wallet.
        (format!("wpkh({b}/1/*)"), false),
        // Same master, other account.
        (format!("wpkh({c}/1/*)"), false),
        // A different path shape.
        (format!("wpkh({a}/1/0/*)"), false),
        (format!("pkh({a}/1/*)"), false),
    ] {
        assert_eq!(
            SplitSource::new(external.clone(), Some(descriptor(&internal))).is_ok(),
            ok,
            "{internal}"
        );
        if !ok {
            assert_eq!(
                SplitSource::new(external.clone(), Some(descriptor(&internal))),
                Err(Error::UnrelatedInternal)
            );
        }
    }
    // Multisig: key order, threshold and every key's origin must match.
    let keys: Vec<_> = (1..=3)
        .map(|seed| account(&master(seed), "m/48'/0'/0'/2'"))
        .collect();
    let multi = |name: &str, k: usize, order: [usize; 3], branch: [u32; 3]| {
        descriptor(&format!(
            "wsh({name}({k},{}/{}/*,{}/{}/*,{}/{}/*))",
            keys[order[0]], branch[0], keys[order[1]], branch[1], keys[order[2]], branch[2]
        ))
    };
    let external = multi("sortedmulti", 2, [0, 1, 2], [0, 0, 0]);
    for (internal, ok) in [
        (multi("sortedmulti", 2, [0, 1, 2], [1, 1, 1]), true),
        (multi("sortedmulti", 1, [0, 1, 2], [1, 1, 1]), false),
        (multi("multi", 2, [0, 1, 2], [1, 1, 1]), false),
        (multi("sortedmulti", 2, [1, 0, 2], [1, 1, 1]), false),
    ] {
        assert_eq!(
            SplitSource::new(external.clone(), Some(internal.clone())).is_ok(),
            ok,
            "{internal}"
        );
    }
    let stranger = account(&master(9), "m/48'/0'/0'/2'");
    let swapped = descriptor(&format!(
        "wsh(sortedmulti(2,{}/1/*,{}/1/*,{stranger}/1/*))",
        keys[0], keys[1]
    ));
    assert_eq!(
        SplitSource::new(external, Some(swapped)),
        Err(Error::UnrelatedInternal)
    );
}

#[test]
fn a_non_all_signature_byte_refuses_without_an_input_request() {
    // The input-level request stays absent: only the signature's own
    // trailing sighash byte says it is not ALL.
    let wallet = wallet(Shape::Wpkh);
    let coins = coins(&wallet.source);
    let step = create(&inputs(&wallet.source, &coins)).unwrap();
    for other in [
        EcdsaSighashType::None,
        EcdsaSighashType::Single,
        EcdsaSighashType::AllPlusAnyoneCanPay,
    ] {
        // A genuine `other` signature, with the request removed afterwards.
        let mut requested = step.psbt().clone();
        requested.inputs[1].sighash_type = Some(PsbtSighashType::from(other));
        let mut genuine = sign(&requested, &wallet.signers);
        genuine.inputs[1].sighash_type = None;
        assert_eq!(
            finalize(&step, &genuine).unwrap_err(),
            FinalizeError::UnsupportedSighash,
            "{other:?}"
        );
        // An ALL signature whose byte is relabeled.
        let mut relabeled = sign(step.psbt(), &wallet.signers);
        for signature in relabeled.inputs[1].partial_sigs.values_mut() {
            signature.sighash_type = other;
        }
        assert!(relabeled.inputs.iter().all(|i| i.sighash_type.is_none()));
        assert_eq!(
            finalize(&step, &relabeled).unwrap_err(),
            FinalizeError::UnsupportedSighash,
            "{other:?}"
        );
    }
}

#[test]
fn locktime_must_be_a_height_at_or_below_the_tip() {
    let wallet = wallet(Shape::Wpkh);
    let coins = coins(&wallet.source);
    let inputs = inputs(&wallet.source, &coins);
    let build = |locktime: LockTime| create_split_step1(&inputs, FEERATE, locktime, TIP, marker());
    for ok in [0, 1, TIP - 100, TIP] {
        assert!(build(LockTime::from_height(ok).unwrap()).is_ok(), "{}", ok);
    }
    for refused in [
        LockTime::from_height(TIP + 1).unwrap(),
        LockTime::from_height(499_999_999).unwrap(),
        // Time-based locktimes, including a far-future one.
        LockTime::from_time(500_000_000).unwrap(),
        LockTime::from_time(4_000_000_000).unwrap(),
    ] {
        assert_eq!(build(refused).unwrap_err(), Error::Locktime, "{refused:?}");
    }

    // Reconstruction checks the recorded locktime against the current tip.
    let step = build(LockTime::from_height(TIP).unwrap()).unwrap();
    let recorded = step.psbt().unsigned_tx.clone();
    assert!(reconstruct_split_step1(&inputs, &recorded, TIP).is_ok());
    // A later tip still accepts an earlier locktime.
    assert!(reconstruct_split_step1(&inputs, &recorded, TIP + 50).is_ok());
    assert_eq!(
        reconstruct_split_step1(&inputs, &recorded, TIP - 1).unwrap_err(),
        Error::Locktime
    );
    for locktime in [
        LockTime::from_height(TIP + 1).unwrap(),
        LockTime::from_time(4_000_000_000).unwrap(),
    ] {
        let mut tx = recorded.clone();
        tx.lock_time = locktime;
        assert_eq!(
            reconstruct_split_step1(&inputs, &tx, TIP).unwrap_err(),
            Error::Locktime,
            "{locktime:?}"
        );
    }
}

/// D9 source digest: domain-tagged (`coincube/split-source/v1`), then
/// branch-tagged and length-prefixed over each descriptor's
/// canonical text, so the receive-only and paired sources differ, every
/// shape differs, and the value is pinned (`split_from` and the Split
/// journal store it).
#[test]
fn split_source_digest_is_canonical_and_pinned() {
    let digests: Vec<_> = SHAPES
        .iter()
        .map(|shape| wallet(*shape).source.digest())
        .collect();
    for (i, a) in digests.iter().enumerate() {
        for b in &digests[i + 1..] {
            assert_ne!(a, b);
        }
    }
    let paired = wallet(Shape::Wpkh).source;
    let receive_only = SplitSource::new(paired.external().clone(), None).unwrap();
    assert_ne!(paired.digest(), receive_only.digest());
    assert_eq!(paired.digest(), wallet(Shape::Wpkh).source.digest());
    assert_eq!(
        paired.digest().to_string(),
        "be64de72f4745a148474c2b1c06112619c8ba5d05950c9321bd568aa5ff399d9"
    );
}

/// The construction keeps the fork height its coins were checked against,
/// from creation and from reconstruction alike; it is not in the
/// transaction, which is the same for any fork height above the coins.
#[test]
fn split_step1_keeps_its_inputs_fork_height() {
    let wallet = wallet(Shape::Pkh);
    let coins = coins(&wallet.source);
    let at = |fork_height| SplitInputs {
        fork_height,
        ..inputs(&wallet.source, &coins)
    };
    let step = create(&at(FORK)).unwrap();
    assert_eq!(step.fork_height(), FORK);
    let higher = create(&at(FORK + 50)).unwrap();
    assert_eq!(higher.fork_height(), FORK + 50);
    assert_eq!(higher.psbt(), step.psbt());
    let recorded = step.psbt().unsigned_tx.clone();
    for fork_height in [FORK, FORK + 50] {
        let rebuilt = reconstruct_split_step1(&at(fork_height), &recorded, TIP).unwrap();
        assert_eq!(rebuilt.fork_height(), fork_height);
        assert_eq!(rebuilt.psbt(), step.psbt());
    }
}

/// #568 B1b restart: a recorded signed step 1 verifies against the exact
/// rebuilt construction, for every shape, and gives what finalization gave.
/// Any change to its bytes, a missing quorum, a non-`ALL` signature or another
/// construction refuses.
#[test]
fn a_recorded_signed_step1_verifies_only_against_its_exact_construction() {
    let secp = secp256k1::Secp256k1::verification_only();
    for shape in SHAPES {
        let wallet = wallet(shape);
        let coins = coins(&wallet.source);
        let step = create(&inputs(&wallet.source, &coins)).unwrap();
        let finalized = finalize(&step, &sign(step.psbt(), &wallet.signers)).unwrap();
        let recorded = finalized.transaction().clone();

        // Rebuilt from the coins and the recorded unsigned bytes.
        let rebuilt =
            reconstruct_split_step1(&inputs(&wallet.source, &coins), &unsigned(&recorded), TIP)
                .unwrap();
        let verified = verify_split_step1_transaction(&rebuilt, &recorded, &secp).unwrap();
        assert_eq!(verified.transaction(), &recorded, "{:?}", shape);
        assert_eq!(verified.construction_txid(), finalized.construction_txid());
        assert_eq!(verified.fee(), finalized.fee());
        assert_eq!(verified.chain(), ChainId::Bitcoin);
        assert_eq!(
            verified.signatures_per_input(),
            finalized.signatures_per_input(),
            "{:?}",
            shape
        );

        // A flipped signature byte, in a scriptSig or a witness.
        let mut tampered = recorded.clone();
        if tampered.input[0].witness.is_empty() {
            let mut bytes = tampered.input[0].script_sig.to_bytes();
            bytes[5] ^= 1;
            tampered.input[0].script_sig = ScriptBuf::from_bytes(bytes);
        } else {
            let mut items: Vec<Vec<u8>> = tampered.input[0].witness.to_vec();
            let signature = items.iter_mut().find(|item| item.len() > 60).unwrap();
            signature[5] ^= 1;
            tampered.input[0].witness = bitcoin::Witness::from_slice(&items);
        }
        assert!(
            verify_split_step1_transaction(&rebuilt, &tampered, &secp).is_err(),
            "{:?}",
            shape
        );

        // An unsigned input.
        let mut stripped = recorded.clone();
        stripped.input[1].script_sig = ScriptBuf::new();
        stripped.input[1].witness.clear();
        assert!(verify_split_step1_transaction(&rebuilt, &stripped, &secp).is_err());

        // Another construction: a different destination.
        let other = create(&SplitInputs {
            destination: 10,
            ..inputs(&wallet.source, &coins)
        })
        .unwrap();
        assert_eq!(
            verify_split_step1_transaction(&other, &recorded, &secp).unwrap_err(),
            FinalizeError::ConstructionChanged
        );
    }

    // A multisig quorum short by one key refuses.
    let multisig = wallet(Shape::WshSortedMulti);
    let multisig_coins = coins(&multisig.source);
    let step = create(&inputs(&multisig.source, &multisig_coins)).unwrap();
    let full = finalize(&step, &sign(step.psbt(), &multisig.signers)).unwrap();
    let mut short = full.transaction().clone();
    for input in &mut short.input {
        let mut items: Vec<Vec<u8>> = input.witness.to_vec();
        // [empty, sig, sig, script]: drop one signature.
        items.remove(1);
        input.witness = bitcoin::Witness::from_slice(&items);
    }
    assert!(verify_split_step1_transaction(&step, &short, &secp).is_err());

    // Genuine signatures of another sighash type, placed where the ALL ones
    // were (#625 review F1). The restart path has no PSBT-level sighash
    // check: the in-replay `SIGHASH_ALL` condition is the only defence.
    // An explicit 0x01 request gives exactly the default signatures.
    for shape in [
        Shape::Wpkh,
        Shape::Pkh,
        Shape::WshSortedMulti,
        Shape::WshMulti,
    ] {
        let wallet = wallet(shape);
        let coins = coins(&wallet.source);
        let step = create(&inputs(&wallet.source, &coins)).unwrap();
        let all = sign(step.psbt(), &wallet.signers);
        let recorded = finalize(&step, &all).unwrap().transaction().clone();
        for (sighash, accepted) in [
            (EcdsaSighashType::All, true),
            (EcdsaSighashType::None, false),
            (EcdsaSighashType::Single, false),
            (EcdsaSighashType::AllPlusAnyoneCanPay, false),
        ] {
            let mut request = step.psbt().clone();
            for input in &mut request.inputs {
                input.sighash_type = Some(PsbtSighashType::from(sighash));
            }
            let other = sign(&request, &wallet.signers);
            let swapped = with_signatures_of(&recorded, &all, &other);
            assert_eq!(swapped == recorded, accepted, "{:?} {:?}", shape, sighash);
            assert_eq!(
                verify_split_step1_transaction(&step, &swapped, &secp).is_ok(),
                accepted,
                "{:?} {:?}",
                shape,
                sighash
            );
        }
    }

    // Structurally changed satisfactions refuse.
    let refused = |shape: Shape, change: &dyn Fn(&mut Transaction)| {
        let wallet = wallet(shape);
        let coins = coins(&wallet.source);
        let step = create(&inputs(&wallet.source, &coins)).unwrap();
        let mut tx = finalize(&step, &sign(step.psbt(), &wallet.signers))
            .unwrap()
            .transaction()
            .clone();
        assert!(verify_split_step1_transaction(&step, &tx, &secp).is_ok());
        change(&mut tx);
        assert!(
            verify_split_step1_transaction(&step, &tx, &secp).is_err(),
            "{:?}",
            shape
        );
    };
    let push_witness = |tx: &mut Transaction, item: Vec<u8>, front: bool| {
        let mut items: Vec<Vec<u8>> = tx.input[0].witness.to_vec();
        if front {
            items.insert(0, item);
        } else {
            items.push(item);
        }
        tx.input[0].witness = bitcoin::Witness::from_slice(&items);
    };
    // An extra witness item.
    refused(Shape::Wpkh, &|tx| push_witness(tx, vec![1], false));
    refused(Shape::Wpkh, &|tx| push_witness(tx, vec![1], true));
    // A scriptSig on a native segwit input.
    refused(Shape::Wpkh, &|tx| {
        tx.input[0].script_sig = bitcoin::script::Builder::new().push_int(1).into_script()
    });
    // An extra scriptSig item, and a witness on a pkh input.
    refused(Shape::Pkh, &|tx| {
        let mut bytes = vec![0x51]; // OP_1 before the pushes
        bytes.extend_from_slice(tx.input[0].script_sig.as_bytes());
        tx.input[0].script_sig = ScriptBuf::from_bytes(bytes);
    });
    refused(Shape::Pkh, &|tx| push_witness(tx, vec![1], false));
    for shape in [Shape::WshSortedMulti, Shape::WshMulti] {
        // A non-empty CHECKMULTISIG dummy, and an extra leading item.
        refused(shape, &|tx| {
            let mut items: Vec<Vec<u8>> = tx.input[0].witness.to_vec();
            assert!(items[0].is_empty());
            items[0] = vec![1];
            tx.input[0].witness = bitcoin::Witness::from_slice(&items);
        });
        refused(shape, &|tx| push_witness(tx, Vec::new(), true));
    }
}

/// `recorded` with every signature of `from` replaced by the same key's
/// signature in `to`, in witnesses and scriptSig pushes alike.
fn with_signatures_of(recorded: &Transaction, from: &Psbt, to: &Psbt) -> Transaction {
    let mut tx = recorded.clone();
    for (index, input) in tx.input.iter_mut().enumerate() {
        let swap = |item: &[u8]| -> Option<Vec<u8>> {
            from.inputs[index]
                .partial_sigs
                .iter()
                .find(|(_, sig)| sig.to_vec() == item)
                .map(|(key, _)| to.inputs[index].partial_sigs[key].to_vec())
        };
        let items: Vec<Vec<u8>> = input
            .witness
            .iter()
            .map(|item| swap(item).unwrap_or_else(|| item.to_vec()))
            .collect();
        input.witness = bitcoin::Witness::from_slice(&items);
        if !input.script_sig.is_empty() {
            let mut builder = bitcoin::script::Builder::new();
            for instruction in input.script_sig.instructions() {
                match instruction.unwrap() {
                    bitcoin::script::Instruction::PushBytes(bytes) => {
                        let data =
                            swap(bytes.as_bytes()).unwrap_or_else(|| bytes.as_bytes().to_vec());
                        builder = builder
                            .push_slice(bitcoin::script::PushBytesBuf::try_from(data).unwrap());
                    }
                    bitcoin::script::Instruction::Op(op) => builder = builder.push_opcode(op),
                }
            }
            input.script_sig = builder.into_script();
        }
    }
    tx
}

/// #647 nit: `NotBitcoinBlake2b` is returned by step 2 and by the unified
/// sweep plan alike, so its message names neither (S3 item 7e).
#[test]
fn not_bitcoin_blake2b_message_is_chain_neutral() {
    let message = Error::NotBitcoinBlake2b(ChainId::Bitcoin).to_string();
    assert!(message.contains("Bitcoin Blake2b only"), "{message}");
    assert!(!message.contains("Split"), "{message}");
    assert!(!message.contains("step 2"), "{message}");
}
