use super::*;
use crate::unified_sighash::{UnifiedSighashCache, SCRIPT_TYPE_WITNESS_V0};
use miniscript::bitcoin::{
    absolute, ecdsa,
    hashes::Hash,
    psbt::{raw::Key, Input, Output, PsbtSighashType},
    secp256k1::{Message, Secp256k1, SecretKey},
    taproot,
    transaction::Version,
    Amount, EcdsaSighashType, OutPoint, ScriptBuf, Sequence, TapSighashType, TxIn, TxOut, Txid,
    Witness, XOnlyPublicKey,
};

#[test]
fn mixed_standard_round_trip_uses_the_internal_namespace_losslessly() {
    let internal = mixed_internal_psbt();
    let standard = export_standard(&internal).unwrap();
    assert!(Psbt::deserialize(&standard).is_err());

    let imported = import_standard(&standard).unwrap();
    assert_eq!(imported, internal);
    assert_eq!(
        RawPsbt::parse(&export_standard(&imported).unwrap()).unwrap(),
        RawPsbt::parse(&standard).unwrap()
    );

    let signatures = unified_signatures(&imported).unwrap();
    assert_eq!(signatures.len(), 1);
    assert_eq!(signatures[0].input_index, 0);
    assert_eq!(signatures[0].signature.last(), Some(&UNIFIED_SIGHASH_ALL));
}

#[test]
fn internal_serialization_retains_all_maps_and_signing_intent() {
    let internal = mixed_internal_psbt();
    let bytes = serialize_internal(&internal).unwrap();
    let reparsed = deserialize_internal(&bytes).unwrap();
    assert_eq!(reparsed, internal);
    assert_eq!(
        reparsed.psbt.inputs[0].sighash_type,
        Some(PsbtSighashType::from_u32(0x21))
    );
    assert!(reparsed.psbt.inputs[1].sighash_type.is_none());
    assert_eq!(reparsed.psbt.unknown, internal.psbt.unknown);
    assert_eq!(reparsed.psbt.proprietary, internal.psbt.proprietary);
    assert_eq!(
        reparsed.psbt.inputs[0].unknown,
        internal.psbt.inputs[0].unknown
    );
    assert_eq!(
        reparsed.psbt.inputs[0].proprietary,
        internal.psbt.inputs[0].proprietary
    );
    assert_eq!(
        reparsed.psbt.outputs[0].unknown,
        internal.psbt.outputs[0].unknown
    );
    assert_eq!(
        reparsed.psbt.outputs[0].proprietary,
        internal.psbt.outputs[0].proprietary
    );
    assert_eq!(
        reparsed.psbt.inputs[0].tap_internal_key,
        internal.psbt.inputs[0].tap_internal_key
    );
}

#[test]
fn explicit_global_version_zero_presence_survives_all_round_trips() {
    let absent = base_psbt();
    let absent_bytes = export_standard(&absent).unwrap();
    assert!(!RawPsbt::parse(&absent_bytes)
        .unwrap()
        .has_explicit_global_version());

    let mut explicit_raw = RawPsbt::parse(&absent_bytes).unwrap();
    explicit_raw.global.pairs.push(RawPair {
        key: vec![PSBT_GLOBAL_VERSION],
        value: 0u32.to_le_bytes().to_vec(),
    });
    let explicit_bytes = explicit_raw.serialize().unwrap();

    let imported = import_standard(&explicit_bytes).unwrap();
    assert!(imported.has_explicit_global_version());
    assert_eq!(
        RawPsbt::parse(&export_standard(&imported).unwrap()).unwrap(),
        RawPsbt::parse(&explicit_bytes).unwrap()
    );

    let internal_bytes = serialize_internal(&imported).unwrap();
    assert!(RawPsbt::parse(&internal_bytes)
        .unwrap()
        .has_explicit_global_version());
    let reparsed = deserialize_internal(&internal_bytes).unwrap();
    assert!(reparsed.has_explicit_global_version());
    assert_eq!(reparsed, imported);
    assert_eq!(
        RawPsbt::parse(&export_standard(&reparsed).unwrap()).unwrap(),
        RawPsbt::parse(&explicit_bytes).unwrap()
    );
}

#[test]
fn ordinary_psbt_without_unified_signatures_is_byte_content_preserving() {
    let mut ordinary = base_psbt();
    let (public_key, signature) = legacy_signature(7);
    ordinary.psbt.inputs[0]
        .partial_sigs
        .insert(public_key, signature);
    ordinary.psbt.inputs[1].tap_key_sig = Some(taproot_signature(8));
    let bytes = ordinary.psbt.serialize();
    let imported = import_standard(&bytes).unwrap();
    assert_eq!(imported, ordinary);
    assert_eq!(
        RawPsbt::parse(&export_standard(&imported).unwrap()).unwrap(),
        RawPsbt::parse(&bytes).unwrap()
    );
}

#[test]
fn unified_taproot_sighash_is_not_enabled() {
    let mut ordinary = base_psbt();
    ordinary.psbt.inputs[0].tap_key_sig = Some(taproot_signature(8));
    let mut raw = RawPsbt::parse(&ordinary.psbt.serialize()).unwrap();
    let tap_signature = raw.inputs[0]
        .pairs
        .iter_mut()
        .find(|pair| pair.key.as_slice() == [0x13])
        .unwrap();
    *tap_signature.value.last_mut().unwrap() = UNIFIED_SIGHASH_ALL;
    assert!(matches!(
        import_standard(&raw.serialize().unwrap()),
        Err(UnifiedPsbtError::TypedPsbt(_))
    ));
}

#[test]
fn duplicate_complete_keys_are_rejected_in_every_map() {
    let raw = RawPsbt::parse(&mixed_internal_psbt().psbt.serialize()).unwrap();
    for (case, expected_map) in [(0, 0), (1, 1), (2, 3)] {
        let mut duplicate = raw.clone();
        let map = match case {
            0 => &mut duplicate.global,
            1 => &mut duplicate.inputs[0],
            _ => &mut duplicate.outputs[0],
        };
        map.pairs.push(map.pairs[0].clone());
        let bytes = duplicate.serialize().unwrap();
        assert!(matches!(
            deserialize_internal(&bytes),
            Err(UnifiedPsbtError::DuplicateRawKey { map, .. }) if map == expected_map
        ));
    }
}

#[test]
fn standard_import_rejects_reserved_collisions_even_when_identical() {
    let internal = mixed_internal_psbt();
    let mut standard = RawPsbt::parse(&export_standard(&internal).unwrap()).unwrap();
    let internal_raw = RawPsbt::parse(&internal.psbt.serialize()).unwrap();
    let reserved = internal_raw.inputs[0]
        .pairs
        .iter()
        .find(|pair| raw_reserved_signature(pair, 0).unwrap().is_some())
        .unwrap()
        .clone();
    standard.inputs[0].pairs.push(reserved);
    let bytes = standard.serialize().unwrap();
    assert!(matches!(
        import_standard(&bytes),
        Err(UnifiedPsbtError::AmbiguousSignatureEncoding { input: 0, .. })
    ));
}

#[test]
fn malformed_keys_der_and_unified_modes_are_explicit_errors() {
    let internal = mixed_internal_psbt();
    let standard = RawPsbt::parse(&export_standard(&internal).unwrap()).unwrap();
    let unified_position = standard.inputs[0]
        .pairs
        .iter()
        .position(|pair| {
            pair.key.first() == Some(&PSBT_IN_PARTIAL_SIG)
                && pair.value.last() == Some(&UNIFIED_SIGHASH_ALL)
        })
        .unwrap();

    let mut invalid_key = standard.clone();
    invalid_key.inputs[0].pairs[unified_position].key = vec![PSBT_IN_PARTIAL_SIG, 2, 3];
    let error = import_standard(&invalid_key.serialize().unwrap()).unwrap_err();
    assert_eq!(error, UnifiedPsbtError::InvalidPublicKey { input: 0 });
    assert_eq!(
        error.to_string(),
        "invalid partial-signature public key in input 0"
    );

    let mut invalid_der = standard.clone();
    invalid_der.inputs[0].pairs[unified_position].value = vec![0x30, 0x00, 0x21];
    assert!(matches!(
        import_standard(&invalid_der.serialize().unwrap()),
        Err(UnifiedPsbtError::InvalidDerSignature { input: 0 })
    ));

    for sighash in [0x22, 0x23, 0xa1] {
        let mut unsupported = standard.clone();
        *unsupported.inputs[0].pairs[unified_position]
            .value
            .last_mut()
            .unwrap() = sighash;
        assert_eq!(
            import_standard(&unsupported.serialize().unwrap()).unwrap_err(),
            UnifiedPsbtError::UnsupportedUnifiedSighash { input: 0, sighash }
        );
    }
}

#[test]
fn malformed_reserved_entries_fail_validation_and_export() {
    let mut invalid_key = base_psbt();
    invalid_key.psbt.inputs[0].proprietary.insert(
        ProprietaryKey {
            prefix: PROPRIETARY_PREFIX.to_vec(),
            subtype: 0,
            key: vec![2, 3],
        },
        unified_signature(9).1,
    );
    assert!(matches!(
        validate_internal(&invalid_key),
        Err(UnifiedPsbtError::InvalidPublicKey { input: 0 })
    ));
    assert!(export_standard(&invalid_key).is_err());

    let mut invalid_der = base_psbt();
    let public_key = unified_signature(10).0;
    invalid_der.psbt.inputs[0]
        .proprietary
        .insert(proprietary_key(&public_key), vec![0x30, 0, 0x21]);
    assert!(matches!(
        serialize_internal(&invalid_der),
        Err(UnifiedPsbtError::InvalidDerSignature { input: 0 })
    ));

    let mut invalid_mode = base_psbt();
    let (public_key, mut signature) = unified_signature(11);
    *signature.last_mut().unwrap() = 0x22;
    invalid_mode.psbt.inputs[0]
        .proprietary
        .insert(proprietary_key(&public_key), signature);
    assert!(matches!(
        unified_signatures(&invalid_mode),
        Err(UnifiedPsbtError::UnsupportedUnifiedSighash {
            input: 0,
            sighash: 0x22
        })
    ));
}

#[test]
fn typed_unknown_key_aliases_are_rejected_in_every_map() {
    let alias = ProprietaryKey {
        prefix: b"foreign-alias".to_vec(),
        subtype: 7,
        key: vec![1, 2, 3],
    }
    .to_key();

    let mut global = base_psbt();
    global.psbt.unknown.insert(alias.clone(), vec![4]);
    assert_eq!(
        validate_internal(&global),
        Err(UnifiedPsbtError::NonCanonicalTypedMap)
    );
    assert!(serialize_internal(&global).is_err());
    assert!(export_standard(&global).is_err());
    assert!(unified_signatures(&global).is_err());

    let mut input = base_psbt();
    input.psbt.inputs[0].unknown.insert(
        ProprietaryKey {
            prefix: PROPRIETARY_PREFIX.to_vec(),
            subtype: PROPRIETARY_SUBTYPE,
            key: vec![0],
        }
        .to_key(),
        vec![UNIFIED_SIGHASH_ALL],
    );
    assert_eq!(
        validate_internal(&input),
        Err(UnifiedPsbtError::NonCanonicalTypedMap)
    );

    let mut output = base_psbt();
    output.psbt.outputs[0].unknown.insert(alias, vec![5]);
    assert_eq!(
        validate_internal(&output),
        Err(UnifiedPsbtError::NonCanonicalTypedMap)
    );
}

#[test]
fn unsigned_transaction_scripts_and_witnesses_fail_every_boundary() {
    let mut script_sig = base_psbt();
    script_sig.psbt.unsigned_tx.input[0].script_sig = ScriptBuf::from_bytes(vec![0x51]);
    assert_eq!(
        validate_internal(&script_sig),
        Err(UnifiedPsbtError::UnsignedTransactionHasScriptSig { input: 0 })
    );
    assert!(serialize_internal(&script_sig).is_err());
    assert!(export_standard(&script_sig).is_err());
    assert!(unified_signatures(&script_sig).is_err());
    assert!(import_standard(&script_sig.psbt.serialize()).is_err());
    assert!(UnifiedPsbt::from_psbt(script_sig.psbt.clone()).is_err());

    let mut witness = base_psbt();
    witness.psbt.unsigned_tx.input[1].witness.push([0x51]);
    assert_eq!(
        validate_internal(&witness),
        Err(UnifiedPsbtError::UnsignedTransactionHasWitness { input: 1 })
    );
    assert!(serialize_internal(&witness).is_err());
    assert!(export_standard(&witness).is_err());
    assert!(unified_signatures(&witness).is_err());
    let mut witness_raw = RawPsbt::parse(&base_psbt().psbt.serialize()).unwrap();
    witness_raw
        .global
        .pairs
        .iter_mut()
        .find(|pair| pair.key.as_slice() == [PSBT_GLOBAL_UNSIGNED_TX])
        .unwrap()
        .value = consensus::serialize(&witness.psbt.unsigned_tx);
    assert!(import_standard(&witness_raw.serialize().unwrap()).is_err());
    assert!(UnifiedPsbt::from_psbt(witness.psbt.clone()).is_err());
}

#[test]
fn invalid_typed_merge_inputs_leave_destination_byte_exact() {
    let mut destination = base_psbt();
    destination.explicit_global_version = true;
    let before = serialize_internal(&destination).unwrap();

    let mut aliased_delta = base_psbt();
    aliased_delta.psbt.inputs[0].unknown.insert(
        ProprietaryKey {
            prefix: PROPRIETARY_PREFIX.to_vec(),
            subtype: PROPRIETARY_SUBTYPE,
            key: vec![0],
        }
        .to_key(),
        vec![UNIFIED_SIGHASH_ALL],
    );
    assert_eq!(
        merge_signatures(&mut destination, &aliased_delta),
        Err(UnifiedPsbtError::NonCanonicalTypedMap)
    );
    assert_eq!(serialize_internal(&destination).unwrap(), before);

    let mut invalid_destination = base_psbt();
    invalid_destination.psbt.unsigned_tx.input[0].script_sig = ScriptBuf::from_bytes(vec![0x51]);
    let invalid_before = invalid_destination.psbt.serialize();
    assert_eq!(
        merge_input_signatures(&mut invalid_destination, &base_psbt(), 0),
        Err(UnifiedPsbtError::UnsignedTransactionHasScriptSig { input: 0 })
    );
    assert_eq!(invalid_destination.psbt.serialize(), invalid_before);
}

#[test]
fn internal_legacy_and_unified_collision_is_rejected() {
    let mut psbt = base_psbt();
    let (public_key, legacy) = legacy_signature(12);
    let (_, unified) = unified_signature(12);
    psbt.psbt.inputs[0].partial_sigs.insert(public_key, legacy);
    psbt.psbt.inputs[0]
        .proprietary
        .insert(proprietary_key(&public_key), unified);
    assert!(matches!(
        validate_internal(&psbt),
        Err(UnifiedPsbtError::AmbiguousSignatureEncoding { input: 0, .. })
    ));
}

#[test]
fn foreign_proprietary_namespaces_are_untouched() {
    let mut psbt = base_psbt();
    let foreign = ProprietaryKey {
        prefix: b"vendor".to_vec(),
        subtype: 0,
        key: vec![1, 2, 3],
    };
    psbt.psbt.inputs[0]
        .proprietary
        .insert(foreign.clone(), vec![4, 5]);
    let imported = import_standard(&export_standard(&psbt).unwrap()).unwrap();
    assert_eq!(
        imported.psbt.inputs[0].proprietary.get(&foreign),
        Some(&vec![4, 5])
    );
    assert!(unified_signatures(&imported).unwrap().is_empty());
}

#[test]
fn version_and_typed_map_counts_fail_closed() {
    let mut version = base_psbt();
    version.psbt.version = 2;
    assert_eq!(
        validate_internal(&version),
        Err(UnifiedPsbtError::UnsupportedVersion(2))
    );
    assert_eq!(
        import_standard(&version.psbt.serialize()),
        Err(UnifiedPsbtError::UnsupportedVersion(2))
    );
    let v2_without_v0_transaction = [
        PSBT_MAGIC.as_slice(),
        &[1, PSBT_GLOBAL_VERSION, 4, 2, 0, 0, 0, 0],
    ]
    .concat();
    assert_eq!(
        import_standard(&v2_without_v0_transaction),
        Err(UnifiedPsbtError::UnsupportedVersion(2))
    );

    let mut missing_input = base_psbt();
    missing_input.psbt.inputs.pop();
    assert!(matches!(
        validate_internal(&missing_input),
        Err(UnifiedPsbtError::MapCountMismatch { .. })
    ));

    let mut missing_output = base_psbt();
    missing_output.psbt.outputs.pop();
    assert!(matches!(
        validate_internal(&missing_output),
        Err(UnifiedPsbtError::MapCountMismatch { .. })
    ));
}

#[test]
fn compact_size_truncation_nonminimality_and_trailing_data_are_rejected() {
    for bytes in [
        [b"psbt\xff".as_slice(), &[0xfd]].concat(),
        [b"psbt\xff".as_slice(), &[0xfe, 1, 2, 3]].concat(),
        [b"psbt\xff".as_slice(), &[0xff, 1, 2, 3, 4, 5, 6, 7]].concat(),
    ] {
        assert_eq!(
            RawPsbt::parse(&bytes),
            Err(UnifiedPsbtError::TruncatedCompactSize)
        );
    }

    for encoded in [vec![0xfd, 1, 0], vec![0xfe, 0xfd, 0, 0, 0], {
        let mut value = vec![0xff];
        value.extend_from_slice(&u64::from(u32::MAX).to_le_bytes());
        value
    }] {
        let bytes = [PSBT_MAGIC.as_slice(), encoded.as_slice()].concat();
        assert_eq!(
            RawPsbt::parse(&bytes),
            Err(UnifiedPsbtError::NonMinimalCompactSize)
        );
    }

    let nonminimal_value = [PSBT_MAGIC.as_slice(), &[1, 0, 0xfd, 1, 0, 0]].concat();
    assert_eq!(
        RawPsbt::parse(&nonminimal_value),
        Err(UnifiedPsbtError::NonMinimalCompactSize)
    );

    let mut trailing = base_psbt().psbt.serialize();
    trailing.push(0);
    assert_eq!(
        deserialize_internal(&trailing),
        Err(UnifiedPsbtError::TrailingData)
    );
}

#[test]
fn truncated_fields_missing_maps_and_missing_transaction_are_rejected() {
    let truncated_key = [PSBT_MAGIC.as_slice(), &[2, 0]].concat();
    assert_eq!(
        RawPsbt::parse(&truncated_key),
        Err(UnifiedPsbtError::TruncatedField)
    );

    let truncated_value = [PSBT_MAGIC.as_slice(), &[1, 0, 2, 1]].concat();
    assert_eq!(
        RawPsbt::parse(&truncated_value),
        Err(UnifiedPsbtError::TruncatedField)
    );

    let no_transaction = [PSBT_MAGIC.as_slice(), &[0]].concat();
    assert_eq!(
        RawPsbt::parse(&no_transaction),
        Err(UnifiedPsbtError::MissingUnsignedTransaction)
    );

    let invalid_transaction = [PSBT_MAGIC.as_slice(), &[1, 0, 1, 0, 0]].concat();
    assert!(matches!(
        RawPsbt::parse(&invalid_transaction),
        Err(UnifiedPsbtError::InvalidUnsignedTransaction(_))
    ));

    let mut missing_map = base_psbt().psbt.serialize();
    missing_map.pop();
    assert!(matches!(
        deserialize_internal(&missing_map),
        Err(UnifiedPsbtError::MissingMap { .. }) | Err(UnifiedPsbtError::TruncatedCompactSize)
    ));
}

#[test]
fn input_size_limit_is_enforced_before_parsing() {
    let oversized = vec![0u8; MAX_PSBT_BYTES + 1];
    assert_eq!(
        import_standard(&oversized),
        Err(UnifiedPsbtError::InputTooLarge {
            actual: MAX_PSBT_BYTES + 1,
            maximum: MAX_PSBT_BYTES,
        })
    );
}

#[test]
fn both_merge_apis_enforce_complete_wrapper_size_atomically() {
    for explicit_version in [false, true] {
        assert_merge_size_boundary(explicit_version, false);
        assert_merge_size_boundary(explicit_version, true);
    }
}

#[test]
fn every_deterministic_truncation_is_rejected_without_panicking() {
    let valid = export_standard(&mixed_internal_psbt()).unwrap();
    for length in 0..valid.len() {
        assert!(
            import_standard(&valid[..length]).is_err(),
            "accepted prefix {}",
            length
        );
    }
}

#[test]
fn checked_merge_is_idempotent_and_keeps_destination_metadata() {
    let mut destination = base_psbt();
    destination.psbt.inputs[0].unknown.insert(
        Key {
            type_value: 0x70,
            key: vec![1],
        },
        vec![2],
    );
    let metadata = destination.psbt.inputs[0].unknown.clone();

    let mut delta = base_psbt();
    let (legacy_key, legacy) = legacy_signature(15);
    delta.psbt.inputs[0].partial_sigs.insert(legacy_key, legacy);
    let (unified_key, unified) = unified_signature(16);
    delta.psbt.inputs[0]
        .proprietary
        .insert(proprietary_key(&unified_key), unified);

    merge_signatures(&mut destination, &delta).unwrap();
    let once = destination.clone();
    merge_signatures(&mut destination, &delta).unwrap();
    assert_eq!(destination, once);
    assert_eq!(destination.psbt.inputs[0].unknown, metadata);
    assert!(destination.psbt.inputs[0]
        .partial_sigs
        .contains_key(&legacy_key));
    assert_eq!(unified_signatures(&destination).unwrap().len(), 1);
}

#[test]
fn merge_rejects_transaction_index_and_signature_conflicts_atomically() {
    let mut destination = base_psbt();
    let before = destination.psbt.serialize();
    let mut other_transaction = base_psbt();
    other_transaction.psbt.unsigned_tx.lock_time = absolute::LockTime::from_consensus(2);
    assert_eq!(
        merge_signatures(&mut destination, &other_transaction),
        Err(UnifiedPsbtError::UnsignedTransactionMismatch)
    );
    assert_eq!(destination.psbt.serialize(), before);

    assert!(matches!(
        merge_input_signatures(&mut destination, &base_psbt(), 2),
        Err(UnifiedPsbtError::InputIndexOutOfBounds {
            index: 2,
            inputs: 2
        })
    ));
    assert_eq!(destination.psbt.serialize(), before);

    let mut first = base_psbt();
    let (key, signature) = legacy_signature(17);
    first.psbt.inputs[0].partial_sigs.insert(key, signature);
    merge_signatures(&mut destination, &first).unwrap();
    let stable = destination.psbt.serialize();

    let mut conflict = base_psbt();
    let (_, different) = legacy_signature_for_key(17, 18);
    conflict.psbt.inputs[0].partial_sigs.insert(key, different);
    assert!(matches!(
        merge_signatures(&mut destination, &conflict),
        Err(UnifiedPsbtError::ConflictingSignature { input: 0, .. })
    ));
    assert_eq!(destination.psbt.serialize(), stable);
}

#[test]
fn merge_rejects_legacy_unified_conflicts_and_is_atomic_across_inputs() {
    let mut destination = base_psbt();
    let (key, legacy) = legacy_signature(19);
    destination.psbt.inputs[1].partial_sigs.insert(key, legacy);
    let before = destination.psbt.serialize();

    let mut delta = base_psbt();
    let (addition_key, addition) = legacy_signature(20);
    delta.psbt.inputs[0]
        .partial_sigs
        .insert(addition_key, addition);
    let (_, unified) = unified_signature(19);
    delta.psbt.inputs[1]
        .proprietary
        .insert(proprietary_key(&key), unified);

    assert!(matches!(
        merge_signatures(&mut destination, &delta),
        Err(UnifiedPsbtError::AmbiguousSignatureEncoding { input: 1, .. })
    ));
    assert_eq!(destination.psbt.serialize(), before);
    assert!(!destination.psbt.inputs[0]
        .partial_sigs
        .contains_key(&addition_key));
}

#[test]
fn merge_rejects_different_unified_signatures_for_one_key() {
    let mut destination = base_psbt();
    let (key, first) = unified_signature(21);
    destination.psbt.inputs[0]
        .proprietary
        .insert(proprietary_key(&key), first);
    let before = destination.psbt.serialize();

    let mut delta = base_psbt();
    let (_, second) = unified_signature_for_key(21, 22);
    delta.psbt.inputs[0]
        .proprietary
        .insert(proprietary_key(&key), second);
    assert!(matches!(
        merge_signatures(&mut destination, &delta),
        Err(UnifiedPsbtError::ConflictingSignature { input: 0, .. })
    ));
    assert_eq!(destination.psbt.serialize(), before);
}

fn mixed_internal_psbt() -> UnifiedPsbt {
    let mut psbt = base_psbt();
    let (legacy_key, legacy) = legacy_signature(1);
    psbt.psbt.inputs[0].partial_sigs.insert(legacy_key, legacy);
    let (unified_key, unified) = unified_signature(2);
    psbt.psbt.inputs[0]
        .proprietary
        .insert(proprietary_key(&unified_key), unified);
    psbt.psbt.inputs[0].sighash_type = Some(PsbtSighashType::from_u32(0x21));

    psbt.psbt.unknown.insert(
        Key {
            type_value: 0x70,
            key: vec![0],
        },
        vec![1],
    );
    psbt.psbt.proprietary.insert(
        ProprietaryKey {
            prefix: b"foreign-global".to_vec(),
            subtype: 9,
            key: vec![2],
        },
        vec![3],
    );
    psbt.psbt.inputs[0].unknown.insert(
        Key {
            type_value: 0x71,
            key: vec![4],
        },
        vec![5],
    );
    psbt.psbt.inputs[0].proprietary.insert(
        ProprietaryKey {
            prefix: b"foreign-input".to_vec(),
            subtype: 8,
            key: vec![6],
        },
        vec![7],
    );
    psbt.psbt.outputs[0].unknown.insert(
        Key {
            type_value: 0x72,
            key: vec![8],
        },
        vec![9],
    );
    psbt.psbt.outputs[0].proprietary.insert(
        ProprietaryKey {
            prefix: b"foreign-output".to_vec(),
            subtype: 7,
            key: vec![10],
        },
        vec![11],
    );
    let (xonly, _) = XOnlyPublicKey::from_keypair(&bitcoin::secp256k1::Keypair::from_secret_key(
        &Secp256k1::new(),
        &secret_key(3),
    ));
    psbt.psbt.inputs[0].tap_internal_key = Some(xonly);
    psbt
}

fn base_psbt() -> UnifiedPsbt {
    let script = ScriptBuf::from_bytes(vec![0x51]);
    UnifiedPsbt {
        psbt: Psbt {
            unsigned_tx: Transaction {
                version: Version::TWO,
                lock_time: absolute::LockTime::from_consensus(1),
                input: vec![
                    TxIn {
                        previous_output: OutPoint {
                            txid: Txid::all_zeros(),
                            vout: 0,
                        },
                        script_sig: ScriptBuf::new(),
                        sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                        witness: Witness::new(),
                    },
                    TxIn {
                        previous_output: OutPoint {
                            txid: Txid::from_byte_array([1; 32]),
                            vout: 1,
                        },
                        script_sig: ScriptBuf::new(),
                        sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                        witness: Witness::new(),
                    },
                ],
                output: vec![
                    TxOut {
                        value: Amount::from_sat(1_000),
                        script_pubkey: script.clone(),
                    },
                    TxOut {
                        value: Amount::from_sat(2_000),
                        script_pubkey: script,
                    },
                ],
            },
            version: 0,
            xpub: BTreeMap::new(),
            proprietary: BTreeMap::new(),
            unknown: BTreeMap::new(),
            inputs: vec![Input::default(), Input::default()],
            outputs: vec![Output::default(), Output::default()],
        },
        explicit_global_version: false,
    }
}

fn assert_merge_size_boundary(explicit_version: bool, one_input_only: bool) {
    let shape = merge_size_shape(explicit_version);
    let mut delta =
        UnifiedPsbt::from_psbt(Psbt::from_unsigned_tx(shape.psbt.unsigned_tx.clone()).unwrap())
            .unwrap();
    let before_signature = delta.psbt.serialize().len();
    let (public_key, signature) = legacy_signature(28);
    delta.psbt.inputs[0]
        .partial_sigs
        .insert(public_key, signature);
    let signature_size = delta.psbt.serialize().len() - before_signature;
    validate_internal(&delta).unwrap();

    for one_byte_over in [false, true] {
        let expected_merged_size = MAX_PSBT_BYTES + usize::from(one_byte_over);
        let version_size = if explicit_version {
            EXPLICIT_GLOBAL_VERSION_SERIALIZED_SIZE
        } else {
            0
        };
        let destination_typed_size = expected_merged_size - version_size - signature_size;
        let mut destination = padded_psbt_at_typed_size(shape.clone(), destination_typed_size);
        let before = serialize_internal(&destination).unwrap();
        assert_eq!(
            before.len(),
            expected_merged_size - signature_size,
            "initial wrapper size differs"
        );

        let result = if one_input_only {
            merge_input_signatures(&mut destination, &delta, 0)
        } else {
            merge_signatures(&mut destination, &delta)
        };

        if one_byte_over {
            assert_eq!(
                result,
                Err(UnifiedPsbtError::InputTooLarge {
                    actual: MAX_PSBT_BYTES + 1,
                    maximum: MAX_PSBT_BYTES,
                })
            );
            assert_eq!(serialize_internal(&destination).unwrap(), before);
        } else {
            result.unwrap();
            assert_eq!(
                serialize_internal(&destination).unwrap().len(),
                MAX_PSBT_BYTES
            );
        }
        assert_eq!(destination.has_explicit_global_version(), explicit_version);
    }
}

fn merge_size_shape(explicit_global_version: bool) -> UnifiedPsbt {
    let mut psbt = base_psbt();
    psbt.psbt.unsigned_tx.input.truncate(1);
    psbt.psbt.inputs.truncate(1);
    let output = psbt.psbt.unsigned_tx.output[0].clone();
    psbt.psbt.unsigned_tx.output = vec![output; 8];
    psbt.psbt.outputs = vec![Output::default(); 8];
    psbt.explicit_global_version = explicit_global_version;
    psbt
}

fn padded_psbt_at_typed_size(mut psbt: UnifiedPsbt, target: usize) -> UnifiedPsbt {
    const ENTRIES: usize = 16;
    const LARGE_COMPACT_SIZE_GROWTH: usize = 4;

    for output_index in 0..8 {
        for entry_index in 0..2 {
            psbt.psbt.outputs[output_index].unknown.insert(
                Key {
                    type_value: 0x70 + entry_index as u8,
                    key: vec![output_index as u8],
                },
                Vec::new(),
            );
        }
    }

    let empty_size = psbt.psbt.serialize().len();
    let value_bytes = target
        .checked_sub(empty_size + ENTRIES * LARGE_COMPACT_SIZE_GROWTH)
        .unwrap();
    let value_size = value_bytes / ENTRIES;
    let remainder = value_bytes % ENTRIES;
    assert!(value_size >= 0x1_0000);

    for (index, output) in psbt.psbt.outputs.iter_mut().enumerate() {
        for (entry_index, value) in output.unknown.values_mut().enumerate() {
            let flat_index = index * 2 + entry_index;
            *value = vec![0x5a; value_size + usize::from(flat_index < remainder)];
        }
    }
    assert_eq!(psbt.psbt.serialize().len(), target);
    validate_internal(&psbt).unwrap();
    psbt
}

fn secret_key(seed: u8) -> SecretKey {
    SecretKey::from_slice(&[seed; 32]).unwrap()
}

fn public_key(seed: u8) -> PublicKey {
    PublicKey::new(bitcoin::secp256k1::PublicKey::from_secret_key(
        &Secp256k1::new(),
        &secret_key(seed),
    ))
}

fn legacy_signature(seed: u8) -> (PublicKey, ecdsa::Signature) {
    legacy_signature_for_key(seed, seed)
}

fn legacy_signature_for_key(key_seed: u8, message_seed: u8) -> (PublicKey, ecdsa::Signature) {
    let secp = Secp256k1::new();
    let signature = secp.sign_ecdsa(
        &Message::from_digest([message_seed; 32]),
        &secret_key(key_seed),
    );
    (
        public_key(key_seed),
        ecdsa::Signature {
            signature,
            sighash_type: EcdsaSighashType::All,
        },
    )
}

fn unified_signature(seed: u8) -> (PublicKey, Vec<u8>) {
    unified_signature_for_key(seed, seed)
}

fn unified_signature_for_key(key_seed: u8, message_seed: u8) -> (PublicKey, Vec<u8>) {
    let psbt = base_psbt();
    let prevouts = vec![
        TxOut {
            value: Amount::from_sat(4_000),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        },
        TxOut {
            value: Amount::from_sat(5_000),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        },
    ];
    let digest = UnifiedSighashCache::new(&psbt.psbt.unsigned_tx, &prevouts)
        .unwrap()
        .signature_hash(
            usize::from(message_seed % 2),
            UNIFIED_SIGHASH_ALL,
            SCRIPT_TYPE_WITNESS_V0,
            prevouts[0].script_pubkey.as_script(),
        )
        .unwrap();
    let signature = Secp256k1::new()
        .sign_ecdsa(&Message::from_digest(digest), &secret_key(key_seed))
        .serialize_der();
    let mut value = signature.as_ref().to_vec();
    value.push(UNIFIED_SIGHASH_ALL);
    (public_key(key_seed), value)
}

fn taproot_signature(seed: u8) -> taproot::Signature {
    let secp = Secp256k1::new();
    let keypair = bitcoin::secp256k1::Keypair::from_secret_key(&secp, &secret_key(seed));
    taproot::Signature {
        signature: secp.sign_schnorr_no_aux_rand(&Message::from_digest([seed; 32]), &keypair),
        sighash_type: TapSighashType::All,
    }
}
