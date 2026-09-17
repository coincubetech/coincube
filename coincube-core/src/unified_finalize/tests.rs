//! Finaliser tests over the B1.1 fixture: a 2-of-3 primary multisig with a
//! timelocked single-key recovery leaf, native P2WSH, authenticated prevouts.

use std::str::FromStr;

use miniscript::bitcoin::{ecdsa, secp256k1, sighash::EcdsaSighashType, Sequence};

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
    // Signer 2 also holds the recovery key; keep only its primary-key record so
    // the recovery leaf (timelocked, not enabled here) plays no part.
    let signed = add_legacy(&signed, &fixture.signers[0], &secp);
    let signed = add_legacy(&signed, &fixture.signers[1], &secp);

    // The scenario is real: a plain cheapest-first satisfaction over the whole
    // set does drop the unified signature.
    {
        let contexts = input_contexts(&signed).unwrap();
        let mut all: BTreeMap<PublicKey, AvailableSignature> = BTreeMap::new();
        for record in unified_signatures(&signed).unwrap() {
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
        for (pk, sig) in &signed.psbt().inputs[0].partial_sigs {
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
        let txin = &signed.psbt().unsigned_tx.input[0];
        let (_, naive) = satisfy_input(
            0,
            &contexts[0],
            &all,
            txin.sequence,
            signed.psbt().unsigned_tx.lock_time,
        )
        .unwrap();
        assert_eq!(
            naive.unified_used, 0,
            "premise: cheapest-first drops the unified signature"
        );
    }

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
