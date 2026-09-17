//! Finaliser tests over the B1.1 fixture: a 2-of-3 primary multisig with a
//! timelocked single-key recovery leaf, native P2WSH, authenticated prevouts.

use std::str::FromStr;

use miniscript::bitcoin::{ecdsa, relative, secp256k1, sighash::EcdsaSighashType, Sequence};

use crate::{
    psbt_unified::{unified_signatures, UnifiedPsbt},
    unified_signing::{sign_p2wsh_all_unified, tests::fixture, UnifiedSigningError},
};

use super::*;
use std::collections::BTreeMap;

fn secp() -> secp256k1::Secp256k1<secp256k1::All> {
    secp256k1::Secp256k1::new()
}

/// Add every legacy `SIGHASH_ALL` partial signature `signer` can produce for
/// the given inputs of this PSBT (the ordinary Bitcoin path), leaving unified
/// records untouched.
fn add_legacy_to(
    psbt: &UnifiedPsbt,
    signer: &crate::signer::MasterSigner,
    inputs: &[usize],
    secp: &secp256k1::Secp256k1<secp256k1::All>,
) -> UnifiedPsbt {
    let signed = signer.sign_psbt(psbt.psbt().clone(), secp).unwrap();
    let mut out = psbt.clone();
    for (index, input) in signed.inputs.into_iter().enumerate() {
        if inputs.contains(&index) {
            out.psbt_mut().inputs[index]
                .partial_sigs
                .extend(input.partial_sigs);
        }
    }
    out
}

fn add_legacy(
    psbt: &UnifiedPsbt,
    signer: &crate::signer::MasterSigner,
    secp: &secp256k1::Secp256k1<secp256k1::All>,
) -> UnifiedPsbt {
    let all: Vec<usize> = (0..psbt.psbt().inputs.len()).collect();
    add_legacy_to(psbt, signer, &all, secp)
}

/// What a plain cheapest-first satisfaction over *every* signature on `input`
/// (unified and legacy together, no preference pass) would put in the witness.
/// This is the behaviour the preference pass exists to override, so tests use
/// it to prove a scenario really is one where miniscript would drop a unified
/// signature.
fn naive_report(psbt: &UnifiedPsbt, input: usize) -> InputWitnessReport {
    let contexts = input_contexts(psbt).unwrap();
    let mut all: BTreeMap<PublicKey, AvailableSignature> = BTreeMap::new();
    for record in unified_signatures(psbt).unwrap() {
        if record.input_index != input {
            continue;
        }
        let der_part = &record.signature[..record.signature.len() - 1];
        all.insert(
            record.public_key,
            AvailableSignature {
                der: ecdsa::Signature {
                    signature: secp256k1::ecdsa::Signature::from_der(der_part).unwrap(),
                    sighash_type: EcdsaSighashType::All,
                },
                witness_bytes: record.signature.clone(),
                unified: true,
            },
        );
    }
    for (pk, sig) in &psbt.psbt().inputs[input].partial_sigs {
        let mut bytes = sig.signature.serialize_der().to_vec();
        bytes.push(0x01);
        all.insert(
            *pk,
            AvailableSignature {
                der: *sig,
                witness_bytes: bytes,
                unified: false,
            },
        );
    }
    satisfy_input(
        input,
        &contexts[input],
        &all,
        InputLocks::of(&psbt.psbt().unsigned_tx, input),
    )
    .unwrap()
    .1
}

/// The signature-shaped elements of a witness, split by trailing sighash byte.
fn signature_elements(witness: &Witness) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut unified = Vec::new();
    let mut legacy = Vec::new();
    for element in witness.iter() {
        if element.len() > 8 && element[0] == 0x30 {
            match element.last() {
                Some(&UNIFIED_SIGHASH_ALL) => unified.push(element.to_vec()),
                Some(0x01) => legacy.push(element.to_vec()),
                _ => panic!("unexpected sighash byte on a signature element"),
            }
        }
    }
    (unified, legacy)
}

#[test]
fn two_unified_signatures_finalise_a_replay_protected_witness() {
    let secp = secp();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    let signed = sign_p2wsh_all_unified(&fixture.signers[1], &signed, &secp).unwrap();

    let finalized = finalize_p2wsh_all_unified(&signed, &secp).unwrap();
    assert_eq!(finalized.inputs.len(), 1);
    assert_eq!(
        finalized.inputs[0],
        InputWitnessReport {
            unified_used: 2,
            legacy_used: 0
        }
    );
    assert!(finalized.inputs[0].replay_protected());

    let witness = &finalized.transaction.input[0].witness;
    let (unified, legacy) = signature_elements(witness);
    assert_eq!(unified.len(), 2);
    assert!(legacy.is_empty(), "no legacy signature may appear");
    // The witness bytes are the verified records, byte for byte.
    let records: Vec<Vec<u8>> = unified_signatures(&signed)
        .unwrap()
        .into_iter()
        .map(|r| r.signature)
        .collect();
    for element in &unified {
        assert!(records.contains(element));
    }
    // Witness script last; the rest of the transaction is the unsigned one.
    assert_eq!(
        witness.last().unwrap(),
        signed.psbt().inputs[0]
            .witness_script
            .as_ref()
            .unwrap()
            .as_bytes()
    );
    let mut stripped = finalized.transaction.clone();
    stripped.input[0].witness = Witness::new();
    assert_eq!(stripped, signed.psbt().unsigned_tx);
}

#[test]
fn legacy_fills_only_what_unified_cannot_satisfy() {
    let secp = secp();
    let fixture = fixture(1);
    // Signer 0: unified. Signer 1: legacy only.
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    let signed = add_legacy(&signed, &fixture.signers[1], &secp);
    assert_eq!(signed.psbt().inputs[0].partial_sigs.len(), 1);

    let finalized = finalize_p2wsh_all_unified(&signed, &secp).unwrap();
    assert_eq!(
        finalized.inputs[0],
        InputWitnessReport {
            unified_used: 1,
            legacy_used: 1
        }
    );
    assert!(finalized.inputs[0].replay_protected());
    let (unified, legacy) = signature_elements(&finalized.transaction.input[0].witness);
    assert_eq!((unified.len(), legacy.len()), (1, 1));
    assert_eq!(
        unified[0],
        unified_signatures(&signed).unwrap()[0].signature
    );
}

#[test]
fn a_key_with_both_a_unified_and_a_legacy_signature_is_refused_by_the_adapter() {
    // The "never drop a verified unified witness for a legacy one" rule starts
    // one layer down: the adapter refuses the ambiguous representation outright,
    // so the finaliser never sees a key with both and never has to choose.
    let secp = secp();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    let both = add_legacy(&signed, &fixture.signers[0], &secp);
    assert!(matches!(
        finalize_p2wsh_all_unified(&both, &secp),
        Err(UnifiedFinalizeError::Signing(UnifiedSigningError::Adapter(
            crate::psbt_unified::UnifiedPsbtError::AmbiguousSignatureEncoding { input: 0, .. }
        )))
    ));
}

#[test]
fn a_unified_signature_is_never_crowded_out_by_legacy_ones() {
    // 2-of-3 with a unified signature from the *third* key in script order and
    // legacy signatures from the first two: a cheapest-first satisfaction would
    // take the two legacy ones and leave the unified unused. The finaliser must
    // keep the unified signature in the witness.
    let secp = secp();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[2], &fixture.psbt, &secp).unwrap();
    // Signer 2 also signs the recovery key, but that leaf is timelocked and not
    // enabled here, so only its primary-key record can enter the witness.
    let signed = add_legacy(&signed, &fixture.signers[0], &secp);
    let signed = add_legacy(&signed, &fixture.signers[1], &secp);

    // The scenario is real: a plain cheapest-first satisfaction over the whole
    // set does drop the unified signature.
    assert_eq!(
        naive_report(&signed, 0).unified_used,
        0,
        "premise: cheapest-first drops the unified signature"
    );

    let finalized = finalize_p2wsh_all_unified(&signed, &secp).unwrap();
    assert!(
        finalized.inputs[0].replay_protected(),
        "a verified unified signature was dropped for legacy ones: {:?}",
        finalized.inputs[0]
    );
    assert_eq!(finalized.inputs[0].unified_used, 1);
    assert_eq!(finalized.inputs[0].legacy_used, 1);
}

#[test]
fn a_unified_signature_is_kept_whichever_key_position_holds_it() {
    // The drop is position-dependent: `multi` takes keys in script order, so
    // the unified signature survives a naive satisfaction on some positions
    // and not others. Sweep every position, with the other two signers legacy,
    // on a one- and a two-input spend. The sweep must include at least one
    // position the naive pass drops, or it proves nothing about the fix.
    let secp = secp();
    for input_count in [1usize, 2] {
        let mut dropped_by_naive = Vec::new();
        for unified_signer in 0..3 {
            let fixture = fixture(input_count);
            let signed =
                sign_p2wsh_all_unified(&fixture.signers[unified_signer], &fixture.psbt, &secp)
                    .unwrap();
            let signed = (0..3)
                .filter(|s| *s != unified_signer)
                .fold(signed, |psbt, s| {
                    add_legacy(&psbt, &fixture.signers[s], &secp)
                });

            for input in 0..input_count {
                if naive_report(&signed, input).unified_used == 0 {
                    dropped_by_naive.push((unified_signer, input));
                }
            }

            let finalized = finalize_p2wsh_all_unified(&signed, &secp)
                .unwrap_or_else(|e| panic!("unified signer {}: {}", unified_signer, e));
            for (input, report) in finalized.inputs.iter().enumerate() {
                assert!(
                    report.replay_protected(),
                    "inputs={} unified signer {} input {}: unified signature dropped: {:?}",
                    input_count,
                    unified_signer,
                    input,
                    report
                );
                assert_eq!(
                    (report.unified_used, report.legacy_used),
                    (1, 1),
                    "inputs={} unified signer {} input {}",
                    input_count,
                    unified_signer,
                    input
                );
                let (unified, legacy) =
                    signature_elements(&finalized.transaction.input[input].witness);
                assert_eq!((unified.len(), legacy.len()), (1, 1));
            }
        }
        assert!(
            !dropped_by_naive.is_empty(),
            "the sweep never reached a position the naive satisfaction drops"
        );
    }
}

#[test]
fn a_unified_signature_the_script_cannot_use_leaves_an_honest_legacy_witness() {
    // Signer 2's unified signature is left only on its *recovery* key, whose
    // leaf is behind a timelock this transaction does not enable. No
    // satisfaction can include it, so the input finalises from the two legacy
    // signatures and says so — the preference pass never manufactures
    // protection that the script cannot express.
    let secp = secp();
    let fixture = fixture(1);
    let mut signed = sign_p2wsh_all_unified(&fixture.signers[2], &fixture.psbt, &secp).unwrap();
    let primary_keys: Vec<PublicKey> = input_contexts(&signed).unwrap()[0]
        .miniscript
        .iter_pk()
        .collect();
    let removed_primary = {
        let input = &mut signed.psbt_mut().inputs[0];
        let before = input.proprietary.len();
        input
            .proprietary
            .retain(|key, _| !primary_keys.iter().any(|pk| key.key == pk.to_bytes()));
        before - input.proprietary.len()
    };
    assert_eq!(removed_primary, 1, "signer 2 had one primary-key record");
    let remaining = unified_signatures(&signed).unwrap();
    assert_eq!(remaining.len(), 1);
    assert!(!primary_keys.contains(&remaining[0].public_key));
    let signed = add_legacy(&signed, &fixture.signers[0], &secp);
    let signed = add_legacy(&signed, &fixture.signers[1], &secp);

    let finalized = finalize_p2wsh_all_unified(&signed, &secp).unwrap();
    assert_eq!(
        finalized.inputs[0],
        InputWitnessReport {
            unified_used: 0,
            legacy_used: 2
        }
    );
    assert!(!finalized.inputs[0].replay_protected());
    let (unified, legacy) = signature_elements(&finalized.transaction.input[0].witness);
    assert!(unified.is_empty());
    assert_eq!(legacy.len(), 2);
}

#[test]
fn too_many_legacy_candidates_to_search_is_refused_not_degraded() {
    // The subset search is bounded. Past the bound, with a unified signature
    // present and legacy ones that satisfy on their own, the input is refused
    // with a typed error rather than finalised replayable.
    let secp = secp();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[2], &fixture.psbt, &secp).unwrap();
    let signed = add_legacy(&signed, &fixture.signers[0], &secp);
    let signed = add_legacy(&signed, &fixture.signers[1], &secp);
    let contexts = input_contexts(&signed).unwrap();

    let mut unified: BTreeMap<PublicKey, AvailableSignature> = BTreeMap::new();
    for record in unified_signatures(&signed).unwrap() {
        let der_part = &record.signature[..record.signature.len() - 1];
        unified.insert(
            record.public_key,
            AvailableSignature {
                der: ecdsa::Signature {
                    signature: secp256k1::ecdsa::Signature::from_der(der_part).unwrap(),
                    sighash_type: EcdsaSighashType::All,
                },
                witness_bytes: record.signature.clone(),
                unified: true,
            },
        );
    }
    let mut legacy: BTreeMap<PublicKey, AvailableSignature> = BTreeMap::new();
    let mut a_legacy_sig = None;
    for (pk, sig) in &signed.psbt().inputs[0].partial_sigs {
        let mut bytes = sig.signature.serialize_der().to_vec();
        bytes.push(0x01);
        a_legacy_sig = Some(*sig);
        legacy.insert(
            *pk,
            AvailableSignature {
                der: *sig,
                witness_bytes: bytes,
                unified: false,
            },
        );
    }
    // Pad with signatures for keys the script does not contain: they can never
    // be used, only searched over.
    let filler = a_legacy_sig.unwrap();
    for i in 0..MAX_LEGACY_KEYS_FOR_SEARCH as u32 {
        let secret = secp256k1::SecretKey::from_slice(&[(i + 100) as u8; 32]).unwrap();
        let pk = PublicKey::new(secp256k1::PublicKey::from_secret_key(&secp, &secret));
        let mut bytes = filler.signature.serialize_der().to_vec();
        bytes.push(0x01);
        legacy.insert(
            pk,
            AvailableSignature {
                der: filler,
                witness_bytes: bytes,
                unified: false,
            },
        );
    }
    assert!(legacy.len() > MAX_LEGACY_KEYS_FOR_SEARCH);

    let locks = InputLocks::of(&signed.psbt().unsigned_tx, 0);
    let result = satisfy_preferring_unified(0, &contexts[0], &unified, &legacy, locks);
    match result {
        Err(UnifiedFinalizeError::RefusedToDropUnified {
            input: 0,
            legacy_candidates,
        }) => assert_eq!(legacy_candidates, legacy.len()),
        other => panic!(
            "expected RefusedToDropUnified, got {:?}",
            other.map(|r| r.1)
        ),
    }
    // Within the bound the same input is fine.
    let (_, report) = satisfy_preferring_unified(
        0,
        &contexts[0],
        &unified,
        &legacy
            .into_iter()
            .take(MAX_LEGACY_KEYS_FOR_SEARCH)
            .collect(),
        locks,
    )
    .unwrap();
    assert!(report.replay_protected());
}

#[test]
fn a_stray_unified_shaped_element_is_refused_by_the_backstop() {
    let secp = secp();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    let record = &unified_signatures(&signed).unwrap()[0];
    assert!(looks_like_der_signature(&record.signature), "DER || 0x21");
    let mut legacy_shaped = record.signature.clone();
    *legacy_shaped.last_mut().unwrap() = 0x01;
    assert!(looks_like_der_signature(&legacy_shaped), "DER || 0x01");
    assert!(!looks_like_der_signature(&[]));
    assert!(!looks_like_der_signature(&[1]));
    assert!(!looks_like_der_signature(
        &fixture.signers[0].fingerprint(&secp).to_bytes()
    ));
    assert!(!looks_like_der_signature(&record.public_key.to_bytes()));
}

#[test]
fn legacy_only_finalises_but_is_reported_replayable() {
    let secp = secp();
    let fixture = fixture(1);
    let signed = add_legacy(&fixture.psbt, &fixture.signers[0], &secp);
    let signed = add_legacy(&signed, &fixture.signers[1], &secp);

    let finalized = finalize_p2wsh_all_unified(&signed, &secp).unwrap();
    assert_eq!(
        finalized.inputs[0],
        InputWitnessReport {
            unified_used: 0,
            legacy_used: 2
        }
    );
    assert!(!finalized.inputs[0].replay_protected());
    let (unified, legacy) = signature_elements(&finalized.transaction.input[0].witness);
    assert!(unified.is_empty());
    assert_eq!(legacy.len(), 2);
}

#[test]
fn per_input_reports_distinguish_protected_and_replayable_inputs() {
    let secp = secp();
    let fixture = fixture(2);
    // Both inputs get unified signatures from signers 0 and 1, then input 1's
    // unified records are removed and legacy ones supplied instead.
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    let mut signed = sign_p2wsh_all_unified(&fixture.signers[1], &signed, &secp).unwrap();
    signed.psbt_mut().inputs[1].proprietary.clear();
    signed.psbt_mut().inputs[1].sighash_type = None;
    let signed = add_legacy_to(&signed, &fixture.signers[0], &[1], &secp);
    let signed = add_legacy_to(&signed, &fixture.signers[1], &[1], &secp);

    let finalized = finalize_p2wsh_all_unified(&signed, &secp).unwrap();
    assert_eq!(finalized.inputs[0].unified_used, 2);
    assert_eq!(finalized.inputs[0].legacy_used, 0);
    assert!(finalized.inputs[0].replay_protected());
    assert_eq!(finalized.inputs[1].unified_used, 0);
    assert_eq!(finalized.inputs[1].legacy_used, 2);
    assert!(!finalized.inputs[1].replay_protected());
}

#[test]
fn recovery_key_after_the_timelock_finalises_through_the_recovery_leaf() {
    let secp = secp();
    let mut fixture = fixture(1);
    // The recovery leaf is `pkh(recovery) and older(46)`: enable the relative
    // lock on the input, then the recovery key's unified signature alone must do.
    fixture.psbt.psbt_mut().unsigned_tx.input[0].sequence = Sequence::from_height(46);
    let signed = sign_p2wsh_all_unified(&fixture.signers[2], &fixture.psbt, &secp).unwrap();
    // Signer 2 signs both its primary key and the recovery key.
    assert_eq!(unified_signatures(&signed).unwrap().len(), 2);

    let finalized = finalize_p2wsh_all_unified(&signed, &secp).unwrap();
    assert_eq!(finalized.inputs[0].legacy_used, 0);
    assert!(finalized.inputs[0].replay_protected());
    assert_eq!(
        finalized.inputs[0].unified_used, 1,
        "the recovery leaf needs one signature"
    );
    assert_eq!(
        finalized.transaction.input[0].sequence,
        Sequence::from_height(46)
    );
}

#[test]
fn insufficient_unified_signatures_are_unsatisfiable_not_padded() {
    let secp = secp();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    assert!(matches!(
        finalize_p2wsh_all_unified(&signed, &secp),
        Err(UnifiedFinalizeError::Unsatisfiable { input: 0, .. })
    ));
}

#[test]
fn anyonecanpay_legacy_signature_is_refused_before_any_witness() {
    let secp = secp();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    let mut signed = add_legacy(&signed, &fixture.signers[1], &secp);
    let (key, sig) = signed.psbt().inputs[0]
        .partial_sigs
        .iter()
        .next()
        .map(|(k, s)| (*k, *s))
        .unwrap();
    signed.psbt_mut().inputs[0].partial_sigs.insert(
        key,
        ecdsa::Signature {
            signature: sig.signature,
            sighash_type: EcdsaSighashType::AllPlusAnyoneCanPay,
        },
    );
    match finalize_p2wsh_all_unified(&signed, &secp) {
        Err(UnifiedFinalizeError::UnsupportedLegacySighash {
            input: 0,
            public_key,
            sighash,
        }) => {
            assert_eq!(public_key, key);
            assert_eq!(sighash, 0x81);
        }
        other => panic!("expected the ANYONECANPAY refusal, got {:?}", other),
    }
}

#[test]
fn an_invalid_unified_signature_fails_the_whole_finalisation() {
    let secp = secp();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    // Enough legacy signatures to finalise on their own …
    let signed = add_legacy(&signed, &fixture.signers[1], &secp);
    let mut signed = add_legacy(&signed, &fixture.signers[2], &secp);
    // … but the unified record is tampered, so nothing is assembled.
    let (key, value) = signed.psbt().inputs[0]
        .proprietary
        .iter()
        .next()
        .map(|(k, v)| (k.clone(), v.clone()))
        .unwrap();
    let mut tampered = value.clone();
    tampered[10] ^= 0x01;
    signed.psbt_mut().inputs[0]
        .proprietary
        .insert(key, tampered);
    assert!(matches!(
        finalize_p2wsh_all_unified(&signed, &secp),
        Err(UnifiedFinalizeError::Signing(
            UnifiedSigningError::InvalidUnifiedSignature { input: 0, .. }
        ))
    ));
}

#[test]
fn an_invalid_legacy_signature_is_refused() {
    let secp = secp();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    let mut signed = add_legacy(&signed, &fixture.signers[1], &secp);
    // A well-formed signature over the wrong digest.
    let (key, sig) = signed.psbt().inputs[0]
        .partial_sigs
        .iter()
        .next()
        .map(|(k, s)| (*k, *s))
        .unwrap();
    let wrong = secp.sign_ecdsa(
        &secp256k1::Message::from_digest([7; 32]),
        &fixture.signers[1]
            .xpriv_at(
                &miniscript::bitcoin::bip32::DerivationPath::from_str("m/48'/0'/0/7").unwrap(),
                &secp,
            )
            .private_key,
    );
    let _ = sig;
    signed.psbt_mut().inputs[0].partial_sigs.insert(
        key,
        ecdsa::Signature {
            signature: wrong,
            sighash_type: EcdsaSighashType::All,
        },
    );
    assert!(matches!(
        finalize_p2wsh_all_unified(&signed, &secp),
        Err(UnifiedFinalizeError::InvalidLegacySignature { input: 0, .. })
    ));
}

/// A legacy record that no witness would use is still verified: two unified
/// signatures satisfy the 2-of-3 on their own, yet a third key's wrong-digest
/// or `ANYONECANPAY` legacy entry refuses the whole call. The finaliser's
/// contract is "every signature in the PSBT is verified", not "every
/// signature it happened to need".
#[test]
fn an_unused_legacy_record_is_still_verified_and_can_refuse_the_call() {
    let secp = secp();
    let fixture = fixture(1);
    let two_unified = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    let two_unified = sign_p2wsh_all_unified(&fixture.signers[1], &two_unified, &secp).unwrap();
    // Premise: unified alone finalises, protected.
    let clean = finalize_p2wsh_all_unified(&two_unified, &secp).unwrap();
    assert_eq!(
        (clean.inputs[0].unified_used, clean.inputs[0].legacy_used),
        (2, 0)
    );

    // Signer 2's legacy record, first with a wrong digest…
    let with_third = add_legacy(&two_unified, &fixture.signers[2], &secp);
    let third_key = *with_third.psbt().inputs[0]
        .partial_sigs
        .keys()
        .next()
        .expect("signer 2 signed its primary key");
    let mut wrong_digest = with_third.clone();
    let wrong = secp.sign_ecdsa(
        &secp256k1::Message::from_digest([9; 32]),
        &fixture.signers[2]
            .xpriv_at(
                &miniscript::bitcoin::bip32::DerivationPath::from_str("m/48'/0'/0/7").unwrap(),
                &secp,
            )
            .private_key,
    );
    wrong_digest.psbt_mut().inputs[0].partial_sigs.insert(
        third_key,
        ecdsa::Signature {
            signature: wrong,
            sighash_type: EcdsaSighashType::All,
        },
    );
    match finalize_p2wsh_all_unified(&wrong_digest, &secp) {
        Err(UnifiedFinalizeError::InvalidLegacySignature {
            input: 0,
            public_key,
        }) => {
            assert_eq!(public_key, third_key)
        }
        other => panic!("expected the unused record to be refused, got {:?}", other),
    }

    // …then with ANYONECANPAY on an otherwise valid signature.
    let mut anyonecanpay = with_third.clone();
    let valid = with_third.psbt().inputs[0].partial_sigs[&third_key];
    anyonecanpay.psbt_mut().inputs[0].partial_sigs.insert(
        third_key,
        ecdsa::Signature {
            signature: valid.signature,
            sighash_type: EcdsaSighashType::AllPlusAnyoneCanPay,
        },
    );
    match finalize_p2wsh_all_unified(&anyonecanpay, &secp) {
        Err(UnifiedFinalizeError::UnsupportedLegacySighash {
            input: 0,
            public_key,
            sighash,
        }) => {
            assert_eq!(public_key, third_key);
            assert_eq!(sighash, 0x81);
        }
        other => panic!("expected the ANYONECANPAY refusal, got {:?}", other),
    }

    // And the valid third record, unused, is fine.
    let ok = finalize_p2wsh_all_unified(&with_third, &secp).unwrap();
    assert_eq!(
        (ok.inputs[0].unified_used, ok.inputs[0].legacy_used),
        (2, 0)
    );
}

/// BIP-68: a CSV leaf is only enforced for transaction version ≥ 2. The bare
/// `Sequence` satisfier does not know the version, so without the guard the
/// finaliser would assemble a recovery witness for a version-1 transaction
/// that every node rejects — while the pill read *Replay protected*.
#[test]
fn a_version_one_transaction_cannot_take_the_csv_recovery_leaf() {
    let secp = secp();
    let mut fixture = fixture(1);
    fixture.psbt.psbt_mut().unsigned_tx.input[0].sequence = Sequence::from_height(46);
    fixture.psbt.psbt_mut().unsigned_tx.version = miniscript::bitcoin::transaction::Version::ONE;
    let signed = sign_p2wsh_all_unified(&fixture.signers[2], &fixture.psbt, &secp).unwrap();

    // Premise: the bare sequence satisfier says the timelock is met, version
    // notwithstanding — the guard is what refuses.
    assert!(<Sequence as Satisfier<PublicKey>>::check_older(
        &Sequence::from_height(46),
        relative::LockTime::from_height(46)
    ));
    assert!(matches!(
        finalize_p2wsh_all_unified(&signed, &secp),
        Err(UnifiedFinalizeError::Unsatisfiable { input: 0, .. })
    ));

    // Version 2, same everything else: the recovery leaf is taken (the
    // existing `recovery_key_after_the_timelock…` test, restated here so the
    // two sit side by side).
    let mut v2 = signed.clone();
    v2.psbt_mut().unsigned_tx.version = miniscript::bitcoin::transaction::Version::TWO;
    // The unified signatures committed to the version-1 transaction; re-sign.
    v2.psbt_mut().inputs[0].proprietary.clear();
    let v2 = sign_p2wsh_all_unified(&fixture.signers[2], &v2, &secp).unwrap();
    let finalized = finalize_p2wsh_all_unified(&v2, &secp).unwrap();
    assert!(finalized.inputs[0].replay_protected());
    assert_eq!(finalized.inputs[0].unified_used, 1);
}

/// BIP-65: an input whose sequence is final disables the transaction's lock
/// time, so a CLTV leaf is not satisfiable through it whatever `lock_time`
/// says. The Vault descriptors carry no `after()` leaf, so this uses a
/// hand-built `and_v(v:pk(K),after(200))` P2WSH script with a legacy
/// signature, which is the only signature kind such a script can get here.
#[test]
fn a_final_sequence_input_cannot_take_a_cltv_leaf() {
    use miniscript::bitcoin::{
        absolute, sighash::SighashCache, transaction, Amount, OutPoint, Psbt, ScriptBuf,
        Transaction, TxIn, TxOut,
    };
    let secp = secp();
    let secret = secp256k1::SecretKey::from_slice(&[3u8; 32]).unwrap();
    let public_key = PublicKey::new(secp256k1::PublicKey::from_secret_key(&secp, &secret));
    let miniscript = Miniscript::<PublicKey, Segwitv0>::from_str(&format!(
        "and_v(v:pk({}),after(200))",
        public_key
    ))
    .unwrap();
    let witness_script = miniscript.encode();
    let prevout = TxOut {
        value: Amount::from_sat(30_000),
        script_pubkey: witness_script.to_p2wsh(),
    };
    let funding = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::new(),
            sequence: Sequence(1),
            witness: Witness::new(),
        }],
        output: vec![prevout.clone()],
    };
    let build = |sequence: Sequence| -> UnifiedPsbt {
        let unsigned = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::from_height(200).unwrap(),
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: funding.compute_txid(),
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                sequence,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(20_000),
                script_pubkey: ScriptBuf::new_p2wsh(&ScriptBuf::new().wscript_hash()),
            }],
        };
        let digest = SighashCache::new(&unsigned)
            .p2wsh_signature_hash(0, &witness_script, prevout.value, EcdsaSighashType::All)
            .unwrap();
        let signature = secp.sign_ecdsa(
            &secp256k1::Message::from_digest(digest.to_byte_array()),
            &secret,
        );
        let mut psbt = Psbt::from_unsigned_tx(unsigned).unwrap();
        psbt.inputs[0].non_witness_utxo = Some(funding.clone());
        psbt.inputs[0].witness_utxo = Some(prevout.clone());
        psbt.inputs[0].witness_script = Some(witness_script.clone());
        psbt.inputs[0].partial_sigs.insert(
            public_key,
            ecdsa::Signature {
                signature,
                sighash_type: EcdsaSighashType::All,
            },
        );
        UnifiedPsbt::from_psbt(psbt).unwrap()
    };

    // Final sequence: lock time disabled, the leaf is unsatisfiable.
    assert!(matches!(
        finalize_p2wsh_all_unified(&build(Sequence::MAX), &secp),
        Err(UnifiedFinalizeError::Unsatisfiable { input: 0, .. })
    ));
    // Any non-final sequence enables it; legacy-only, honestly replayable.
    let finalized =
        finalize_p2wsh_all_unified(&build(Sequence::ENABLE_RBF_NO_LOCKTIME), &secp).unwrap();
    assert_eq!(
        finalized.inputs[0],
        InputWitnessReport {
            unified_used: 0,
            legacy_used: 1
        }
    );
    assert_eq!(
        finalized.transaction.lock_time,
        absolute::LockTime::from_height(200).unwrap()
    );
}

#[test]
fn a_missing_witness_script_is_refused_by_verification_first() {
    let secp = secp();
    let fixture = fixture(1);
    let mut signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    signed.psbt_mut().inputs[0].witness_script = None;
    assert!(matches!(
        finalize_p2wsh_all_unified(&signed, &secp),
        Err(UnifiedFinalizeError::Signing(
            UnifiedSigningError::MissingWitnessScript { input: 0 }
        ))
    ));
}

#[test]
fn spent_amount_reads_the_authenticated_prevout() {
    let fixture = fixture(2);
    assert_eq!(
        spent_amount(&fixture.psbt, 1),
        Some(Amount::from_sat(50_000))
    );
    assert_eq!(spent_amount(&fixture.psbt, 2), None);
}

/// Independent check of the assembled witness structure: for a legacy-only
/// spend rust-miniscript's interpreter can verify the whole witness (it knows
/// `SIGHASH_ALL`), so if it accepts this one, the satisfaction the finaliser
/// builds — the same one it builds for unified signatures, bar the sighash
/// byte — is consensus-shaped and every signature in it checks out.
#[test]
fn a_legacy_only_witness_is_accepted_by_the_miniscript_interpreter() {
    use miniscript::{bitcoin::sighash::Prevouts, interpreter::Interpreter};

    let secp = secp();
    let fixture = fixture(2);
    let signed = add_legacy(&fixture.psbt, &fixture.signers[0], &secp);
    let signed = add_legacy(&signed, &fixture.signers[2], &secp);
    let finalized = finalize_p2wsh_all_unified(&signed, &secp).unwrap();

    let prevouts: Vec<TxOut> = signed
        .psbt()
        .inputs
        .iter()
        .map(|i| i.witness_utxo.clone().unwrap())
        .collect();
    for (index, txin) in finalized.transaction.input.iter().enumerate() {
        let interpreter = Interpreter::from_txdata(
            &prevouts[index].script_pubkey,
            &txin.script_sig,
            &txin.witness,
            txin.sequence,
            finalized.transaction.lock_time,
        )
        .unwrap();
        let satisfied: Vec<_> = interpreter
            .iter(
                &secp,
                &finalized.transaction,
                index,
                &Prevouts::All(&prevouts),
            )
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|e| {
                panic!("input {} witness rejected by the interpreter: {}", index, e)
            });
        assert!(!satisfied.is_empty());
        assert!(!finalized.inputs[index].replay_protected());
    }
}
