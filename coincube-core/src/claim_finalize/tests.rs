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

#[test]
fn recovered_transaction_requires_the_exact_construction_and_valid_retained_witnesses() {
    let secp = secp256k1::Secp256k1::verification_only();
    for chain in [ChainId::Bitcoin, ChainId::Testnet4] {
        let (built, signers) = fixture(chain, false);
        let signed = sign(&built, &signers[..2]);
        let finalized = finalize_poison_transfer(&built, &signed, &secp).unwrap();
        let restored = verify_poison_transaction(&built, finalized.transaction(), &secp).unwrap();
        assert_eq!(restored.transaction(), finalized.transaction());
        assert_eq!(restored.signatures_per_input(), &[2, 2]);
        assert_eq!(restored.fee(), finalized.fee());
        let mut altered = finalized.transaction().clone();
        altered.input[0].witness.clear();
        assert!(verify_poison_transaction(&built, &altered, &secp).is_err());
        let mut altered = finalized.transaction().clone();
        let mut stack = altered.input[0].witness.to_vec();
        stack[1][4] ^= 1;
        altered.input[0].witness = bitcoin::Witness::from_slice(&stack);
        assert!(verify_poison_transaction(&built, &altered, &secp).is_err());
        let mut altered = finalized.transaction().clone();
        altered.output[1].value = Amount::from_sat(1);
        assert!(matches!(
            verify_poison_transaction(&built, &altered, &secp),
            Err(FinalizeError::ConstructionChanged)
        ));
        assert!(verify_poison_transaction(&built, &built.psbt().unsigned_tx, &secp).is_err());
    }
}

fn fork_fixture(chain: ChainId) -> (crate::claim_spend::ClaimForkSweep, Vec<MasterSigner>) {
    let (source, signers) = fixture(chain, false);
    let mut getter = Getter(HashMap::new());
    let coins: Vec<_> = source
        .psbt()
        .inputs
        .iter()
        .enumerate()
        .map(|(i, input)| {
            let previous = input.non_witness_utxo.clone().unwrap();
            getter.0.insert(previous.compute_txid(), previous);
            CandidateCoin {
                outpoint: source.psbt().unsigned_tx.input[i].previous_output,
                amount: input.witness_utxo.as_ref().unwrap().value,
                deriv_index: ChildNumber::from_normal_idx(7 + i as u32).unwrap(),
                is_change: false,
                must_select: false,
                sequence: None,
                ancestor_info: None,
            }
        })
        .collect();
    let fork = if chain == ChainId::Bitcoin {
        ChainId::BitcoinBlake2b
    } else {
        ChainId::BitcoinBlake2bTestnet4
    };
    let sweep = crate::claim_spend::create_claim_fork_sweep(
        &source,
        fork,
        &secp256k1::Secp256k1::verification_only(),
        &mut getter,
        &coins,
        ChildNumber::from_normal_idx(20).unwrap(),
        3,
        absolute::LockTime::ZERO,
    )
    .unwrap();
    (sweep, signers)
}

#[test]
fn fork_sweep_finalization_reports_actual_unified_mixed_and_legacy_witnesses() {
    use crate::{psbt_unified::UnifiedPsbt, unified_signing::sign_p2wsh_all_unified};
    let secp = secp256k1::Secp256k1::new();
    for chain in [ChainId::Bitcoin, ChainId::Testnet4] {
        let (sweep, signers) = fork_fixture(chain);
        for unified_count in 0..=2 {
            let mut signed = UnifiedPsbt::from_psbt(sweep.psbt().clone()).unwrap();
            assert!(finalize_claim_fork_sweep(&sweep, &signed, &secp).is_err());
            for (i, signer) in signers[..2].iter().enumerate() {
                signed = if i < unified_count {
                    sign_p2wsh_all_unified(signer, &signed, &secp).unwrap()
                } else {
                    // Legacy signers cannot consume the unified sighash request.
                    // Collect their signatures on the original ordinary PSBT and
                    // merge signatures only, preserving unified records already held.
                    let delta = UnifiedPsbt::from_psbt(
                        signer.sign_psbt(sweep.psbt().clone(), &secp).unwrap(),
                    )
                    .unwrap();
                    crate::psbt_unified::merge_signatures(&mut signed, &delta).unwrap();
                    signed
                };
                if i == 0 {
                    assert!(finalize_claim_fork_sweep(&sweep, &signed, &secp).is_err());
                }
            }
            let verified = finalize_claim_fork_sweep(&sweep, &signed, &secp).unwrap();
            let restored =
                verify_claim_fork_transaction(&sweep, verified.transaction(), &secp).unwrap();
            assert_eq!(restored.transaction(), verified.transaction());
            assert_eq!(restored.inputs(), verified.inputs());
            assert_eq!(restored.fee(), verified.fee());
            for mutation in 0..6 {
                let mut bad = verified.transaction().clone();
                let mut witness = bad.input[0].witness.to_vec();
                match mutation {
                    0 => witness.clear(),
                    1 => witness[1][4] ^= 1,
                    2 => {
                        let last = witness[1].len() - 1;
                        witness[1][last] = 0x81;
                    }
                    3 => witness.insert(0, vec![]),
                    4 => bad.output[0].value = Amount::from_sat(1),
                    _ => witness.swap(1, 2),
                }
                bad.input[0].witness = bitcoin::Witness::from_slice(&witness);
                assert!(
                    verify_claim_fork_transaction(&sweep, &bad, &secp).is_err(),
                    "mutation {}",
                    mutation
                );
            }
            assert_eq!(verified.chain(), sweep.chain());
            assert_eq!(verified.bitcoin_step1(), sweep.bitcoin_step1());
            assert_eq!(verified.fee(), sweep.psbt().fee().unwrap());
            assert_eq!(
                verified.transaction().compute_txid(),
                sweep.psbt().unsigned_tx.compute_txid()
            );
            assert!(verified
                .inputs()
                .iter()
                .all(|r| r.unified_used == unified_count && r.legacy_used == 2 - unified_count));
            assert!(verified
                .transaction()
                .input
                .iter()
                .all(|i| !i.witness.is_empty()));
        }
    }
}

#[test]
fn fork_finalizer_rejects_changed_construction_and_tampered_signatures() {
    use crate::{psbt_unified::UnifiedPsbt, unified_signing::sign_p2wsh_all_unified};
    let secp = secp256k1::Secp256k1::new();
    let (sweep, signers) = fork_fixture(ChainId::Bitcoin);
    let mut signed = UnifiedPsbt::from_psbt(sweep.psbt().clone()).unwrap();
    for signer in &signers[..2] {
        signed = sign_p2wsh_all_unified(signer, &signed, &secp).unwrap();
    }
    finalize_claim_fork_sweep(&sweep, &signed, &secp).unwrap();
    let mut mutations = Vec::new();
    let mut bad = signed.clone();
    bad.psbt_mut().unsigned_tx.output[0].value = Amount::from_sat(1);
    mutations.push(bad);
    let mut bad = signed.clone();
    bad.psbt_mut().inputs[0].bip32_derivation.clear();
    mutations.push(bad);
    let mut bad = signed.clone();
    bad.psbt_mut().outputs[0].bip32_derivation.clear();
    mutations.push(bad);
    let mut bad = signed.clone();
    bad.psbt_mut().inputs[0].non_witness_utxo = None;
    mutations.push(bad);
    let mut bad = signed.clone();
    bad.psbt_mut().inputs[0].final_script_witness = Some(bitcoin::Witness::from_slice(&[vec![1]]));
    mutations.push(bad);
    let mut bad = signed.clone();
    bad.psbt_mut().inputs[1].proprietary = bad.psbt().inputs[0].proprietary.clone();
    mutations.push(bad);
    let mut bad = signed.clone();
    // Well-formed DER, invalid signature: flip a scalar byte, preserving the
    // encoding and sighash. Neither the adapter nor metadata equality suffices.
    let value = bad.psbt_mut().inputs[0]
        .proprietary
        .values_mut()
        .next()
        .unwrap();
    value[10] ^= 1;
    mutations.push(bad);
    for bad in mutations {
        assert!(finalize_claim_fork_sweep(&sweep, &bad, &secp).is_err());
    }
}

#[test]
fn fork_finalizer_refuses_a_retained_legacy_alternative_to_a_unified_witness() {
    use crate::{
        psbt_unified::{merge_signatures, UnifiedPsbt},
        unified_finalize::UnifiedFinalizeError,
        unified_signing::sign_p2wsh_all_unified,
    };
    let secp = secp256k1::Secp256k1::new();
    for chain in [ChainId::Bitcoin, ChainId::Testnet4] {
        let (sweep, signers) = fork_fixture(chain);
        let unified = sign_p2wsh_all_unified(
            &signers[0],
            &UnifiedPsbt::from_psbt(sweep.psbt().clone()).unwrap(),
            &secp,
        )
        .unwrap();
        // Legacy signatures are collected on the ordinary PSBT and merged in.
        let with_legacy = |indices: &[usize]| {
            let legacy = indices.iter().fold(sweep.psbt().clone(), |psbt, i| {
                signers[*i].sign_psbt(psbt, &secp).unwrap()
            });
            let mut mixed = unified.clone();
            merge_signatures(&mut mixed, &UnifiedPsbt::from_psbt(legacy).unwrap()).unwrap();
            mixed
        };
        // One legacy signature completes the mixed witness and cannot satisfy
        // multi(2) on its own: accepted.
        let verified = finalize_claim_fork_sweep(&sweep, &with_legacy(&[1]), &secp).unwrap();
        assert!(verified
            .inputs()
            .iter()
            .all(|r| r.unified_used == 1 && r.legacy_used == 1));
        // Two retained legacy signatures satisfy multi(2) without the unified
        // one, so a standard PSBT consumer could build a replayable witness.
        let refused = finalize_claim_fork_sweep(&sweep, &with_legacy(&[1, 2]), &secp);
        assert!(
            matches!(
                refused,
                Err(ClaimForkFinalizeError::Finalize(
                    UnifiedFinalizeError::UnsafeLegacyAlternative {
                        input: 0,
                        legacy_signatures: 2,
                    }
                ))
            ),
            "{:?}",
            refused.as_ref().map(|v| v.inputs())
        );
        // Recovering the published mixed witness is not a retention decision.
        let restored =
            verify_claim_fork_transaction(&sweep, verified.transaction(), &secp).unwrap();
        assert_eq!(restored.transaction(), verified.transaction());
    }
}

fn ancestry_fixture(
    recovery: bool,
) -> (crate::claim_spend::AncestrySelfTransfer, Vec<MasterSigner>) {
    let (source, signers) = fixture(ChainId::Bitcoin, recovery);
    let mut getter = Getter(HashMap::new());
    let mut coins = Vec::new();
    for (i, input) in source.psbt().inputs.iter().enumerate() {
        let tx = input.non_witness_utxo.clone().unwrap();
        let outpoint = source.psbt().unsigned_tx.input[i].previous_output;
        let script = &tx.output[outpoint.vout as usize].script_pubkey;
        let secp = secp256k1::Secp256k1::verification_only();
        let deriv_index = (7..9)
            .map(|i| ChildNumber::from_normal_idx(i).unwrap())
            .find(|index| {
                source
                    .descriptor()
                    .receive_descriptor()
                    .derive(*index, &secp)
                    .script_pubkey()
                    == *script
            })
            .unwrap();
        coins.push(CandidateCoin {
            outpoint,
            amount: tx.output[outpoint.vout as usize].value,
            deriv_index,
            is_change: false,
            must_select: true,
            sequence: recovery.then_some(Sequence(46)),
            ancestor_info: None,
        });
        getter.0.insert(tx.compute_txid(), tx);
    }
    let selected = coins[0].outpoint;
    let raw = bitcoin::consensus::serialize(&getter.0[&selected.txid]);
    let dependency = crate::claim_ancestry::verify(
        selected,
        &[crate::claim_ancestry::Link {
            transaction: &raw,
            parent_input: None,
        }],
    )
    .unwrap();
    let built = crate::claim_spend::create_ancestry_self_transfer(
        ChainId::Bitcoin,
        source.descriptor(),
        &secp256k1::Secp256k1::verification_only(),
        &mut getter,
        &coins,
        ChildNumber::from_normal_idx(12).unwrap(),
        5,
        absolute::LockTime::ZERO,
        &dependency,
    )
    .unwrap();
    (built, signers)
}

#[test]
fn ancestry_primary_and_recovery_signatures_pass_recovered_witness_verification() {
    let secp = secp256k1::Secp256k1::new();
    for recovery in [false, true] {
        let (built, signers) = ancestry_fixture(recovery);
        let signing = if recovery {
            &signers[3..]
        } else {
            &signers[..2]
        };
        let signed = signing.iter().fold(built.psbt().clone(), |psbt, s| {
            s.sign_psbt(psbt, &secp).unwrap()
        });
        let verified = finalize_ancestry_transfer(&built, &signed, &secp).unwrap();
        assert_eq!(verified.transaction().output.len(), 1);
        assert!(!verified.transaction().output[0]
            .script_pubkey
            .is_op_return());
        assert_eq!(verified.poison_input(), built.poison_input());
        assert_eq!(verified.claimed_prevouts(), built.claimed_prevouts());
        assert_eq!(
            verified.signatures_per_input(),
            if recovery { &[1, 1] } else { &[2, 2] }
        );
        assert_eq!(verified.chain(), ChainId::Bitcoin);
        assert_eq!(verified.descriptor(), built.descriptor());
        assert_eq!(
            verified.construction_txid(),
            built.psbt().unsigned_tx.compute_txid()
        );
        assert_eq!(verified.fee(), signed.fee().unwrap());
        assert_eq!(verified.vsize(), verified.transaction().vsize());
        let restored = verify_ancestry_transaction(&built, verified.transaction(), &secp).unwrap();
        assert_eq!(restored.transaction(), verified.transaction());
        assert_eq!(restored.poison_input(), verified.poison_input());
        assert_eq!(restored.claimed_prevouts(), verified.claimed_prevouts());
        for index in 0..2 {
            let mut bad = verified.transaction().clone();
            bad.input[index].witness.clear();
            assert!(verify_ancestry_transaction(&built, &bad, &secp).is_err());
        }
        let mut bad = verified.transaction().clone();
        bad.output[0].value = Amount::from_sat(1);
        assert!(matches!(
            verify_ancestry_transaction(&built, &bad, &secp),
            Err(FinalizeError::ConstructionChanged)
        ));
    }
}

#[test]
fn ancestry_rejects_wrong_metadata_non_all_and_invalid_or_insufficient_signatures() {
    let (built, signers) = ancestry_fixture(false);
    let secp = secp256k1::Secp256k1::new();
    let partial = signers[0].sign_psbt(built.psbt().clone(), &secp).unwrap();
    assert!(matches!(
        finalize_ancestry_transfer(&built, &partial, &secp),
        Err(FinalizeError::Unsatisfied)
    ));
    let signed = signers[1].sign_psbt(partial, &secp).unwrap();
    let mut bad = signed.clone();
    bad.inputs[0].non_witness_utxo = None;
    assert!(matches!(
        finalize_ancestry_transfer(&built, &bad, &secp),
        Err(FinalizeError::ConstructionChanged)
    ));
    let mut bad = signed.clone();
    bad.unsigned_tx.output[0].value = Amount::from_sat(1);
    assert!(matches!(
        finalize_ancestry_transfer(&built, &bad, &secp),
        Err(FinalizeError::ConstructionChanged)
    ));
    for flag in [2, 3, 0x21, 0x81] {
        let mut bad = signed.clone();
        bad.inputs[0].sighash_type = Some(bitcoin::psbt::PsbtSighashType::from_u32(flag));
        assert!(matches!(
            finalize_ancestry_transfer(&built, &bad, &secp),
            Err(FinalizeError::UnsupportedSighash)
        ));
    }
    let mut bad = signed;
    bad.inputs[0]
        .partial_sigs
        .values_mut()
        .next()
        .unwrap()
        .signature = secp.sign_ecdsa(
        &secp256k1::Message::from_digest([0; 32]),
        &secp256k1::SecretKey::from_slice(&[1; 32]).unwrap(),
    );
    assert!(matches!(
        finalize_ancestry_transfer(&built, &bad, &secp),
        Err(FinalizeError::InvalidSignature { input: 0 })
    ));
}
