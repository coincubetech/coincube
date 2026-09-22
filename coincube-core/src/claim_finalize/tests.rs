use super::*;
use crate::{
    claim_spend::create_poison_self_transfer,
    descriptors::{CoincubePolicy, PathInfo},
    signer::MasterSigner,
    spend::{CandidateCoin, TxGetter},
};
use miniscript::{
    bitcoin::{
        absolute,
        bip32::{ChildNumber, DerivationPath},
        hashes::Hash,
        Amount, BlockHash, OutPoint, Sequence, TxIn, TxOut,
    },
    DescriptorPublicKey,
};
use std::{collections::HashMap, str::FromStr};
struct Getter(HashMap<Txid, Transaction>);
impl TxGetter for Getter {
    fn get_tx(&mut self, id: &Txid) -> Option<Transaction> {
        self.0.get(id).cloned()
    }
}
fn fixture(chain: ChainId, recovery: bool) -> (PoisonSelfTransfer, Vec<MasterSigner>) {
    let secp = secp256k1::Secp256k1::new();
    let signers: Vec<_> = (10..14)
        .map(crate::unified_signing::tests::signer)
        .collect();
    let keys: Vec<_> = signers
        .iter()
        .map(|s| {
            DescriptorPublicKey::from_str(&format!(
                "[{}]{}/<0;1>/*",
                s.fingerprint(&secp),
                s.xpub_at(&DerivationPath::default(), &secp)
            ))
            .unwrap()
        })
        .collect();
    let descriptor = CoincubeDescriptor::new(
        CoincubePolicy::new_legacy(
            PathInfo::Multi(2, keys[..3].to_vec()),
            std::iter::once((46, PathInfo::Single(keys[3].clone()))).collect(),
        )
        .unwrap(),
    );
    let verify = secp256k1::Secp256k1::verification_only();
    let mut getter = Getter(HashMap::new());
    let mut coins = Vec::new();
    for index in 7..9 {
        let index = ChildNumber::from_normal_idx(index).unwrap();
        let previous = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(100_000),
                script_pubkey: descriptor
                    .receive_descriptor()
                    .derive(index, &verify)
                    .script_pubkey(),
            }],
        };
        coins.push(CandidateCoin {
            outpoint: OutPoint::new(previous.compute_txid(), 0),
            amount: previous.output[0].value,
            deriv_index: index,
            is_change: false,
            must_select: true,
            sequence: recovery.then_some(Sequence(46)),
            ancestor_info: None,
        });
        getter.0.insert(previous.compute_txid(), previous);
    }
    let built = create_poison_self_transfer(
        chain,
        &descriptor,
        &verify,
        &mut getter,
        &coins,
        ChildNumber::from_normal_idx(12).unwrap(),
        5,
        absolute::LockTime::ZERO,
        BlockHash::from_byte_array([42; 32]),
    )
    .unwrap();
    (built, signers)
}
fn sign(built: &PoisonSelfTransfer, signers: &[MasterSigner]) -> Psbt {
    let secp = secp256k1::Secp256k1::new();
    signers.iter().fold(built.psbt().clone(), |psbt, s| {
        s.sign_psbt(psbt, &secp).unwrap()
    })
}
#[test]
fn valid_primary_and_recovery_witnesses_are_verified_on_both_bitcoin_chains() {
    let secp = secp256k1::Secp256k1::verification_only();
    for chain in [ChainId::Bitcoin, ChainId::Testnet4] {
        for recovery in [false, true] {
            let (built, signers) = fixture(chain, recovery);
            let signed = if recovery {
                sign(&built, &signers[3..])
            } else {
                sign(&built, &signers[..2])
            };
            let result = finalize_poison_transfer(&built, &signed, &secp).unwrap();
            assert_eq!(result.chain(), chain);
            assert_eq!(
                result.construction_txid(),
                built.psbt().unsigned_tx.compute_txid()
            );
            assert_eq!(
                result.transaction().compute_txid(),
                result.construction_txid()
            );
            assert_eq!(result.fee(), signed.fee().unwrap());
            assert!(result.vsize() > 0);
            assert_eq!(result.descriptor(), built.descriptor());
            assert_eq!(
                result.signatures_per_input(),
                if recovery { &[1, 1] } else { &[2, 2] }
            );
            assert_eq!(result.transaction().output, built.psbt().unsigned_tx.output);
            assert_eq!(result.transaction().output[0].script_pubkey.len(), 90);
            assert!(result
                .transaction()
                .input
                .iter()
                .all(|i| !i.witness.is_empty() && i.script_sig.is_empty()));
        }
    }
}
#[test]
fn mutations_to_unsigned_transaction_or_construction_metadata_refuse() {
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let signed = sign(&built, &signers[..2]);
    let secp = secp256k1::Secp256k1::verification_only();
    let mut variants = Vec::new();
    let mut p = signed.clone();
    p.unsigned_tx.output[1].value = Amount::from_sat(1);
    variants.push(p);
    let mut p = signed.clone();
    p.unsigned_tx.input.swap(0, 1);
    variants.push(p);
    let mut p = signed.clone();
    p.unsigned_tx.input[0].sequence = Sequence::MAX;
    variants.push(p);
    let mut p = signed.clone();
    p.unsigned_tx.lock_time = absolute::LockTime::from_consensus(123);
    variants.push(p);
    let mut p = signed.clone();
    p.inputs[0].witness_utxo.as_mut().unwrap().value = Amount::from_sat(99_999);
    variants.push(p);
    let mut p = signed.clone();
    p.inputs[0].non_witness_utxo = None;
    variants.push(p);
    let mut p = signed.clone();
    p.inputs[0].witness_script = None;
    variants.push(p);
    let mut p = signed.clone();
    p.inputs[0].bip32_derivation.clear();
    variants.push(p);
    let mut p = signed.clone();
    p.outputs[1].bip32_derivation.clear();
    variants.push(p);
    let mut p = signed.clone();
    p.inputs[0].proprietary.insert(
        bitcoin::psbt::raw::ProprietaryKey {
            prefix: b"coincube".to_vec(),
            subtype: 0,
            key: vec![2; 33],
        },
        vec![1; 72],
    );
    variants.push(p);
    let mut p = signed.clone();
    p.inputs[0].final_script_witness = Some(bitcoin::Witness::new());
    variants.push(p);
    for p in variants {
        assert!(matches!(
            finalize_poison_transfer(&built, &p, &secp),
            Err(FinalizeError::ConstructionChanged)
        ));
    }
    let mut finalized = signed;
    finalized.finalize_mut(&secp).unwrap();
    assert!(matches!(
        finalize_poison_transfer(&built, &finalized, &secp),
        Err(FinalizeError::ConstructionChanged)
    ));
}
#[test]
fn non_all_requests_and_signatures_including_unified_refuse() {
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let signed = sign(&built, &signers[..2]);
    let secp = secp256k1::Secp256k1::verification_only();
    for request in [2, 3, 0x21, 0x81] {
        let mut p = signed.clone();
        p.inputs[0].sighash_type = Some(bitcoin::psbt::PsbtSighashType::from_u32(request));
        assert!(matches!(
            finalize_poison_transfer(&built, &p, &secp),
            Err(FinalizeError::UnsupportedSighash)
        ));
    }
    let mut p = signed.clone();
    p.inputs[0]
        .partial_sigs
        .values_mut()
        .next()
        .unwrap()
        .sighash_type = EcdsaSighashType::AllPlusAnyoneCanPay;
    assert!(matches!(
        finalize_poison_transfer(&built, &p, &secp),
        Err(FinalizeError::UnsupportedSighash)
    ));
    let mut p = signed;
    for i in &mut p.inputs {
        i.sighash_type = Some(EcdsaSighashType::All.into());
    }
    assert!(finalize_poison_transfer(&built, &p, &secp).is_ok());
}
#[test]
fn invalid_surplus_signature_and_missing_quorum_refuse() {
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let secp = secp256k1::Secp256k1::new();
    assert!(matches!(
        finalize_poison_transfer(&built, &built.psbt().clone(), &secp),
        Err(FinalizeError::Unsatisfied)
    ));
    let partial = sign(&built, &signers[..1]);
    assert!(matches!(
        finalize_poison_transfer(&built, &partial, &secp),
        Err(FinalizeError::Unsatisfied)
    ));
    let mut full = sign(&built, &signers[..3]);
    let bad = secp.sign_ecdsa(
        &secp256k1::Message::from_digest([0; 32]),
        &secp256k1::SecretKey::from_slice(&[1; 32]).unwrap(),
    );
    full.inputs[0]
        .partial_sigs
        .values_mut()
        .last()
        .unwrap()
        .signature = bad;
    assert!(matches!(
        finalize_poison_transfer(&built, &full, &secp),
        Err(FinalizeError::InvalidSignature { input: 0 })
    ));
}
#[test]
fn retained_witness_tamper_and_unified_byte_fail_actual_interpreter() {
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let signed = sign(&built, &signers[..2]);
    let secp = secp256k1::Secp256k1::verification_only();
    let verified = finalize_poison_transfer(&built, &signed, &secp).unwrap();
    let prevouts: Vec<_> = signed
        .inputs
        .iter()
        .map(|i| i.witness_utxo.clone().unwrap())
        .collect();
    for unified in [false, true] {
        let mut tx = verified.transaction().clone();
        let mut stack: Vec<Vec<u8>> = tx.input[0].witness.iter().map(|x| x.to_vec()).collect();
        let sig = stack
            .iter_mut()
            .find(|x| x.first() == Some(&0x30) && x.len() > 8)
            .unwrap();
        if unified {
            *sig.last_mut().unwrap() = 0x21;
        } else {
            sig[5] ^= 1;
        }
        tx.input[0].witness = bitcoin::Witness::from_slice(&stack);
        assert_eq!(
            verify_retained_witness(&tx, built.psbt(), &prevouts, &secp),
            Err(FinalizeError::InvalidWitness)
        );
    }
    let mut tx = verified.transaction().clone();
    tx.input[0].witness.clear();
    assert_eq!(
        verify_retained_witness(&tx, built.psbt(), &prevouts, &secp),
        Err(FinalizeError::InvalidWitness)
    );
}
