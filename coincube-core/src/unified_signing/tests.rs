use std::str::FromStr;

use miniscript::{
    bitcoin::{
        self, absolute,
        bip32::{self, DerivationPath},
        ecdsa, transaction, Amount, Network, OutPoint, Sequence, Transaction, TxIn, TxOut,
    },
    descriptor::{DerivPaths, DescriptorMultiXKey, DescriptorPublicKey, Wildcard},
};

use crate::{
    descriptors::{CoincubeDescriptor, CoincubePolicy, PathInfo},
    psbt_unified::{export_standard, import_standard, serialize_internal, unified_signatures},
};

use super::*;

struct Fixture {
    signers: Vec<MasterSigner>,
    psbt: UnifiedPsbt,
}

fn signer(byte: u8) -> MasterSigner {
    let mnemonic = bip39::Mnemonic::from_entropy(&[byte; 16]).unwrap();
    MasterSigner::from_mnemonic(Network::Bitcoin, mnemonic).unwrap()
}

fn descriptor_key(
    signer: &MasterSigner,
    branch: u32,
    secp: &secp256k1::Secp256k1<secp256k1::All>,
) -> DescriptorPublicKey {
    let origin = DerivationPath::from(vec![
        bip32::ChildNumber::from_hardened_idx(48).unwrap(),
        bip32::ChildNumber::from_hardened_idx(branch).unwrap(),
    ]);
    DescriptorPublicKey::MultiXPub(DescriptorMultiXKey {
        origin: Some((signer.fingerprint(secp), origin.clone())),
        xkey: signer.xpub_at(&origin, secp),
        derivation_paths: DerivPaths::new(vec![
            DerivationPath::from_str("m/0").unwrap(),
            DerivationPath::from_str("m/1").unwrap(),
        ])
        .unwrap(),
        wildcard: Wildcard::Unhardened,
    })
}

fn funding_transaction(output: TxOut, marker: u32) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::new(),
            sequence: Sequence(marker),
            witness: bitcoin::Witness::new(),
        }],
        output: vec![output],
    }
}

fn fixture(input_count: usize) -> Fixture {
    let secp = secp256k1::Secp256k1::new();
    let signers = vec![signer(1), signer(2), signer(3), signer(4)];
    let primary = PathInfo::Multi(
        2,
        vec![
            descriptor_key(&signers[0], 0, &secp),
            descriptor_key(&signers[1], 0, &secp),
            descriptor_key(&signers[2], 0, &secp),
        ],
    );
    let recovery = PathInfo::Single(descriptor_key(&signers[2], 1, &secp));
    let descriptor = CoincubeDescriptor::new(
        CoincubePolicy::new_legacy(primary, [(46, recovery)].iter().cloned().collect()).unwrap(),
    );
    let derived = descriptor.receive_descriptor().derive(7.into(), &secp);
    let output = TxOut {
        value: Amount::from_sat(50_000),
        script_pubkey: derived.script_pubkey(),
    };

    let mut previous = Vec::new();
    let mut inputs = Vec::new();
    for index in 0..input_count {
        let previous_tx = funding_transaction(output.clone(), (index as u32) + 1);
        inputs.push(TxIn {
            previous_output: OutPoint {
                txid: previous_tx.compute_txid(),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: bitcoin::Witness::new(),
        });
        previous.push(previous_tx);
    }
    let unsigned_tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: inputs,
        output: vec![TxOut {
            value: Amount::from_sat(40_000),
            script_pubkey: ScriptBuf::new_p2wsh(&ScriptBuf::new().wscript_hash()),
        }],
    };
    let mut psbt = Psbt::from_unsigned_tx(unsigned_tx).unwrap();
    for (map, previous_tx) in psbt.inputs.iter_mut().zip(previous) {
        derived.update_psbt_in(map);
        map.non_witness_utxo = Some(previous_tx);
        map.witness_utxo = Some(output.clone());
    }
    Fixture {
        signers,
        psbt: UnifiedPsbt::from_psbt(psbt).unwrap(),
    }
}

fn add_explicit_v0(bytes: &[u8]) -> Vec<u8> {
    // Insert key 0xfb / four-byte zero immediately before the global map terminator.
    let mut position = 5;
    loop {
        let key_len = bytes[position] as usize;
        position += 1;
        if key_len == 0 {
            let mut result = Vec::with_capacity(bytes.len() + 7);
            result.extend_from_slice(&bytes[..position - 1]);
            result.extend_from_slice(&[1, 0xfb, 4, 0, 0, 0, 0]);
            result.extend_from_slice(&bytes[position - 1..]);
            return result;
        }
        position += key_len;
        let value_len = bytes[position] as usize;
        position += 1 + value_len;
    }
}

fn insert_unified(
    psbt: &mut UnifiedPsbt,
    input: usize,
    public_key: PublicKey,
    signature: secp256k1::ecdsa::Signature,
) {
    let mut bytes = signature.serialize_der().to_vec();
    bytes.push(UNIFIED_SIGHASH_ALL);
    psbt.psbt_mut().inputs[input]
        .proprietary
        .insert(proprietary_key(&public_key), bytes);
}

#[test]
fn deterministic_primary_and_recovery_keys_sign_and_verify() {
    let secp = secp256k1::Secp256k1::new();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    assert_eq!(verify_p2wsh_all_unified(&signed, &secp), Ok(1));

    let signed = sign_p2wsh_all_unified(&fixture.signers[2], &signed, &secp).unwrap();
    // Signer 2 owns one key in the primary branch and the pkh recovery leaf.
    assert_eq!(verify_p2wsh_all_unified(&signed, &secp), Ok(3));
    assert_eq!(unified_signatures(&signed).unwrap().len(), 3);
}

#[test]
fn produced_signature_matches_shared_unified_digest_contract() {
    let secp = secp256k1::Secp256k1::new();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    let record = unified_signatures(&signed).unwrap().remove(0);
    assert_eq!(record.signature.last(), Some(&UNIFIED_SIGHASH_ALL));

    let contexts = validate_inputs(&signed).unwrap();
    let spent_outputs: Vec<_> = contexts
        .iter()
        .map(|context| context.spent_output.clone())
        .collect();
    let cache = UnifiedSighashCache::new(&signed.psbt().unsigned_tx, &spent_outputs).unwrap();
    let digest = cache
        .signature_hash(
            record.input_index,
            0x21,
            SCRIPT_TYPE_WITNESS_V0,
            &contexts[record.input_index].witness_script,
        )
        .unwrap();
    let signature =
        secp256k1::ecdsa::Signature::from_der(&record.signature[..record.signature.len() - 1])
            .unwrap();
    secp.verify_ecdsa(
        &secp256k1::Message::from_digest(digest),
        &signature,
        &record.public_key.inner,
    )
    .unwrap();
}

#[test]
fn matching_origin_derivation_depth_is_bounded_without_mutation() {
    let secp = secp256k1::Secp256k1::new();
    let fixture = fixture(1);
    let fingerprint = fixture.signers[0].fingerprint(&secp);
    let public_key = fixture.psbt.psbt().inputs[0]
        .bip32_derivation
        .iter()
        .find(|(_, source)| source.0 == fingerprint)
        .map(|(key, _)| *key)
        .unwrap();
    let child = bip32::ChildNumber::from_normal_idx(0).unwrap();

    let mut at_limit = fixture.psbt.clone();
    at_limit.psbt_mut().inputs[0].bip32_derivation.insert(
        public_key,
        (fingerprint, DerivationPath::from(vec![child; 255])),
    );
    let at_limit_before = serialize_internal(&at_limit).unwrap();
    assert!(matches!(
        sign_p2wsh_all_unified(&fixture.signers[0], &at_limit, &secp),
        Err(UnifiedSigningError::DerivedPublicKeyMismatch { input: 0, .. })
    ));
    assert_eq!(serialize_internal(&at_limit).unwrap(), at_limit_before);

    let mut over_limit = fixture.psbt.clone();
    over_limit.psbt_mut().inputs[0].bip32_derivation.insert(
        public_key,
        (fingerprint, DerivationPath::from(vec![child; 256])),
    );
    let over_limit_before = serialize_internal(&over_limit).unwrap();
    assert!(matches!(
        sign_p2wsh_all_unified(&fixture.signers[0], &over_limit, &secp),
        Err(UnifiedSigningError::DerivationPathTooDeep {
            input: 0,
            depth: 256,
            ..
        })
    ));
    assert_eq!(serialize_internal(&over_limit).unwrap(), over_limit_before);
}

#[test]
fn no_matching_key_is_explicitly_valid_and_byte_exact() {
    let secp = secp256k1::Secp256k1::new();
    let fixture = fixture(1);
    assert_eq!(verify_p2wsh_all_unified(&fixture.psbt, &secp), Ok(0));
    let before = serialize_internal(&fixture.psbt).unwrap();
    let result = sign_p2wsh_all_unified(&fixture.signers[3], &fixture.psbt, &secp).unwrap();
    assert_eq!(serialize_internal(&result).unwrap(), before);
}

#[test]
fn multiple_inputs_sign_only_matching_derivations_and_preserve_no_key_input() {
    let secp = secp256k1::Secp256k1::new();
    let mut fixture = fixture(2);
    let fingerprint = fixture.signers[0].fingerprint(&secp);
    fixture.psbt.psbt_mut().inputs[1]
        .bip32_derivation
        .retain(|_, source| source.0 != fingerprint);
    fixture.psbt.psbt_mut().inputs[1].sighash_type = Some(PsbtSighashType::from_u32(1));
    let untouched = fixture.psbt.psbt().inputs[1].clone();

    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    assert_eq!(verify_p2wsh_all_unified(&signed, &secp), Ok(1));
    assert_eq!(
        signed.psbt().inputs[0].sighash_type,
        Some(PsbtSighashType::from_u32(UNIFIED_SIGHASH_ALL.into()))
    );
    assert_eq!(signed.psbt().inputs[1], untouched);
}

#[test]
fn unified_signature_cannot_be_relocated_between_inputs() {
    let secp = secp256k1::Secp256k1::new();
    let fixture = fixture(2);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    let records = unified_signatures(&signed).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(verify_p2wsh_all_unified(&signed, &secp), Ok(2));

    let source = records
        .iter()
        .find(|record| record.input_index == 0)
        .unwrap();
    let target = records
        .iter()
        .find(|record| record.input_index == 1)
        .unwrap();
    assert_eq!(source.public_key, target.public_key);

    let key = proprietary_key(&source.public_key);
    let moved_signature = signed.psbt().inputs[0].proprietary[&key].clone();
    let mut relocated = signed.clone();
    relocated.psbt_mut().inputs[1]
        .proprietary
        .insert(key, moved_signature);
    assert!(matches!(
        verify_p2wsh_all_unified(&relocated, &secp),
        Err(UnifiedSigningError::InvalidUnifiedSignature { input: 1, .. })
    ));
}

#[test]
fn explicit_version_unknown_metadata_and_legacy_signature_survive() {
    let secp = secp256k1::Secp256k1::new();
    let mut fixture = fixture(1);
    fixture.psbt.psbt_mut().unknown.insert(
        bitcoin::psbt::raw::Key {
            type_value: 0x70,
            key: vec![1, 2],
        },
        vec![3, 4],
    );
    let foreign_key = PublicKey::new(
        fixture.signers[1]
            .xpriv_at(&DerivationPath::from_str("m/9").unwrap(), &secp)
            .to_priv()
            .public_key(&secp)
            .inner,
    );
    let legacy = secp.sign_ecdsa(
        &secp256k1::Message::from_digest([9; 32]),
        &fixture.signers[1]
            .xpriv_at(&DerivationPath::from_str("m/9").unwrap(), &secp)
            .private_key,
    );
    fixture.psbt.psbt_mut().inputs[0].partial_sigs.insert(
        foreign_key,
        ecdsa::Signature {
            signature: legacy,
            sighash_type: bitcoin::sighash::EcdsaSighashType::All,
        },
    );
    let wire = add_explicit_v0(&export_standard(&fixture.psbt).unwrap());
    let explicit = import_standard(&wire).unwrap();
    assert!(explicit.has_explicit_global_version());
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &explicit, &secp).unwrap();
    assert!(signed.has_explicit_global_version());
    assert_eq!(signed.psbt().unknown, explicit.psbt().unknown);
    assert_eq!(
        signed.psbt().inputs[0].partial_sigs,
        explicit.psbt().inputs[0].partial_sigs
    );
}

#[test]
fn prevout_authentication_rejects_missing_txid_vout_and_witness_conflict() {
    let secp = secp256k1::Secp256k1::new();
    let fixture = fixture(2);
    for mutate in 0..4 {
        let mut bad = fixture.psbt.clone();
        match mutate {
            0 => bad.psbt_mut().inputs[1].non_witness_utxo = None,
            1 => {
                bad.psbt_mut().unsigned_tx.input[1].previous_output.txid =
                    bitcoin::Txid::all_zeros()
            }
            2 => bad.psbt_mut().unsigned_tx.input[1].previous_output.vout = 9,
            _ => {
                bad.psbt_mut().inputs[1]
                    .witness_utxo
                    .as_mut()
                    .unwrap()
                    .value = Amount::from_sat(1)
            }
        }
        assert!(matches!(
            sign_p2wsh_all_unified(&fixture.signers[0], &bad, &secp),
            Err(UnifiedSigningError::InputAuthentication { input: 1, .. })
        ));
    }
}

#[test]
fn wrong_script_family_commitment_and_unsupported_script_are_rejected() {
    let secp = secp256k1::Secp256k1::new();
    let fixture = fixture(1);

    let mut bare = fixture.psbt.clone();
    let (changed_output, changed_txid) = {
        let previous = bare.psbt_mut().inputs[0].non_witness_utxo.as_mut().unwrap();
        previous.output[0].script_pubkey = ScriptBuf::new();
        (previous.output[0].clone(), previous.compute_txid())
    };
    bare.psbt_mut().inputs[0].witness_utxo = Some(changed_output);
    bare.psbt_mut().unsigned_tx.input[0].previous_output.txid = changed_txid;
    assert!(matches!(
        verify_p2wsh_all_unified(&bare, &secp),
        Err(UnifiedSigningError::UnsupportedPrevoutScript { input: 0 })
    ));

    let mut mismatch = fixture.psbt.clone();
    mismatch.psbt_mut().inputs[0].witness_script = Some(ScriptBuf::new());
    assert!(matches!(
        verify_p2wsh_all_unified(&mismatch, &secp),
        Err(UnifiedSigningError::WitnessScriptCommitmentMismatch { input: 0 })
    ));

    let mut unsupported = fixture.psbt.clone();
    let script = bitcoin::script::Builder::new()
        .push_opcode(bitcoin::opcodes::all::OP_CODESEPARATOR)
        .push_int(1)
        .into_script();
    let (changed_output, changed_txid) = {
        let previous = unsupported.psbt_mut().inputs[0]
            .non_witness_utxo
            .as_mut()
            .unwrap();
        previous.output[0].script_pubkey = script.to_p2wsh();
        (previous.output[0].clone(), previous.compute_txid())
    };
    unsupported.psbt_mut().inputs[0].witness_utxo = Some(changed_output);
    unsupported.psbt_mut().inputs[0].witness_script = Some(script);
    unsupported.psbt_mut().unsigned_tx.input[0]
        .previous_output
        .txid = changed_txid;
    assert!(matches!(
        verify_p2wsh_all_unified(&unsupported, &secp),
        Err(UnifiedSigningError::UnsupportedWitnessScript { input: 0, .. })
    ));
}

#[test]
fn incompatible_sighash_and_wrong_derivation_are_atomic() {
    let secp = secp256k1::Secp256k1::new();
    let fixture = fixture(1);
    let mut bad_mode = fixture.psbt.clone();
    bad_mode.psbt_mut().inputs[0].sighash_type = Some(PsbtSighashType::from_u32(1));
    let before = serialize_internal(&bad_mode).unwrap();
    assert!(matches!(
        sign_p2wsh_all_unified(&fixture.signers[0], &bad_mode, &secp),
        Err(UnifiedSigningError::IncompatibleSighash {
            input: 0,
            actual: 1
        })
    ));
    assert_eq!(serialize_internal(&bad_mode).unwrap(), before);

    let mut wrong_key = fixture.psbt.clone();
    let (public_key, source) = wrong_key.psbt().inputs[0]
        .bip32_derivation
        .iter()
        .find(|(_, source)| source.0 == fixture.signers[0].fingerprint(&secp))
        .map(|(key, source)| (*key, source.clone()))
        .unwrap();
    wrong_key.psbt_mut().inputs[0]
        .bip32_derivation
        .remove(&public_key);
    let replacement = fixture.signers[3]
        .xpriv_at(&DerivationPath::from_str("m/8").unwrap(), &secp)
        .private_key
        .public_key(&secp);
    wrong_key.psbt_mut().inputs[0]
        .bip32_derivation
        .insert(replacement, source);
    assert!(matches!(
        sign_p2wsh_all_unified(&fixture.signers[0], &wrong_key, &secp),
        Err(UnifiedSigningError::DerivedPublicKeyMismatch { input: 0, .. })
    ));
}

#[test]
fn transaction_amount_script_and_signature_tampering_fail_verification() {
    let secp = secp256k1::Secp256k1::new();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();

    let mut tx = signed.clone();
    tx.psbt_mut().unsigned_tx.output[0].value = Amount::from_sat(39_999);
    assert!(matches!(
        verify_p2wsh_all_unified(&tx, &secp),
        Err(UnifiedSigningError::InvalidUnifiedSignature { .. })
    ));

    let mut amount = signed.clone();
    let (changed_output, changed_txid) = {
        let previous = amount.psbt_mut().inputs[0]
            .non_witness_utxo
            .as_mut()
            .unwrap();
        previous.output[0].value = Amount::from_sat(49_999);
        (previous.output[0].clone(), previous.compute_txid())
    };
    amount.psbt_mut().inputs[0].witness_utxo = Some(changed_output);
    amount.psbt_mut().unsigned_tx.input[0].previous_output.txid = changed_txid;
    assert!(matches!(
        verify_p2wsh_all_unified(&amount, &secp),
        Err(UnifiedSigningError::InvalidUnifiedSignature { .. })
    ));

    let mut signature = signed.clone();
    let record = unified_signatures(&signature).unwrap().remove(0);
    let wrong = secp.sign_ecdsa(
        &secp256k1::Message::from_digest([7; 32]),
        &fixture.signers[0]
            .xpriv_at(&DerivationPath::from_str("m/8").unwrap(), &secp)
            .private_key,
    );
    insert_unified(&mut signature, 0, record.public_key, wrong);
    assert!(matches!(
        verify_p2wsh_all_unified(&signature, &secp),
        Err(UnifiedSigningError::InvalidUnifiedSignature { .. })
    ));
}

#[test]
fn existing_signature_key_must_belong_to_supported_script_semantics() {
    let secp = secp256k1::Secp256k1::new();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    let record = unified_signatures(&signed).unwrap().remove(0);
    let mut wrong_key = signed.clone();
    wrong_key.psbt_mut().inputs[0]
        .proprietary
        .remove(&proprietary_key(&record.public_key));
    let outsider = fixture.signers[3]
        .xpriv_at(&DerivationPath::from_str("m/8").unwrap(), &secp)
        .to_priv()
        .public_key(&secp);
    let wrong_signature = secp.sign_ecdsa(
        &secp256k1::Message::from_digest([8; 32]),
        &fixture.signers[3]
            .xpriv_at(&DerivationPath::from_str("m/8").unwrap(), &secp)
            .private_key,
    );
    insert_unified(&mut wrong_key, 0, outsider, wrong_signature);
    assert!(matches!(
        verify_p2wsh_all_unified(&wrong_key, &secp),
        Err(UnifiedSigningError::KeyNotInWitnessScript { input: 0, .. })
    ));
}

#[test]
fn existing_conflict_refuses_without_changing_input() {
    let secp = secp256k1::Secp256k1::new();
    let fixture = fixture(1);
    let signed = sign_p2wsh_all_unified(&fixture.signers[0], &fixture.psbt, &secp).unwrap();
    let record = unified_signatures(&signed).unwrap().remove(0);
    let mut conflicting = signed.clone();
    conflicting.psbt_mut().inputs[0].partial_sigs.insert(
        record.public_key,
        ecdsa::Signature {
            signature: secp.sign_ecdsa(
                &secp256k1::Message::from_digest([5; 32]),
                &fixture.signers[0]
                    .xpriv_at(&DerivationPath::from_str("m/8").unwrap(), &secp)
                    .private_key,
            ),
            sighash_type: bitcoin::sighash::EcdsaSighashType::All,
        },
    );
    let snapshot = conflicting.clone();
    assert!(matches!(
        sign_p2wsh_all_unified(&fixture.signers[1], &conflicting, &secp),
        Err(UnifiedSigningError::Adapter(
            UnifiedPsbtError::AmbiguousSignatureEncoding { .. }
        ))
    ));
    assert_eq!(conflicting, snapshot);
}
