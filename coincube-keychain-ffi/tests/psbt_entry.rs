//! The PSBT-shaped production entry, over genuine native-P2WSH fixtures.
//!
//! The upstream vector corpus cannot reach this entry — see the module comment
//! in `unified_sighash_kat.rs` — so it gets its own fixtures: a real Vault
//! descriptor (2-of-3 primary plus a timelocked recovery path), derived, funded,
//! and spent. Expected digests are **core-derived**: computed by calling
//! `coincube_core::unified_sighash::UnifiedSighashCache` directly and compared
//! against what comes back through the C ABI, so the assertion is "the boundary
//! agrees with core", not "the boundary agrees with a number someone typed".

use std::str::FromStr;

use coincube_core::{
    descriptors::{CoincubeDescriptor, CoincubePolicy, PathInfo},
    miniscript::{
        bitcoin::{
            self, absolute,
            bip32::{self, DerivationPath},
            psbt::Psbt,
            secp256k1, transaction, Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction,
            TxIn, TxOut,
        },
        descriptor::{DerivPaths, DescriptorMultiXKey, DescriptorPublicKey, Wildcard},
    },
    psbt_unified::{export_standard, UnifiedPsbt},
    signer::MasterSigner,
    unified_sighash::{UnifiedSighashCache, SCRIPT_TYPE_WITNESS_V0},
};
use coincube_keychain_ffi::*;

/// What the unified signing path signs, and the only hash type it can express.
const UNIFIED_SIGHASH_ALL: u8 = 0x21;

/// A deterministic signer. Entropy is fixed so digests are reproducible.
fn signer(byte: u8) -> MasterSigner {
    let mnemonic = coincube_core::bip39::Mnemonic::from_entropy(&[byte; 16]).unwrap();
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

struct Fixture {
    signers: Vec<MasterSigner>,
    psbt: UnifiedPsbt,
}

/// A spend of `input_count` native-P2WSH Vault outputs.
fn fixture(input_count: usize) -> Fixture {
    let secp = secp256k1::Secp256k1::new();
    let signers = vec![signer(1), signer(2), signer(3)];
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
    assert!(
        output.script_pubkey.is_p2wsh(),
        "fixture must spend genuine native P2WSH, which is exactly what the \
         vector corpus cannot offer"
    );

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

/// The digest core produces for one input, computed without going near the ABI.
fn core_digest(psbt: &UnifiedPsbt, index: usize) -> [u8; 32] {
    let spent_outputs: Vec<TxOut> = psbt
        .psbt()
        .inputs
        .iter()
        .map(|input| input.witness_utxo.clone().unwrap())
        .collect();
    let witness_script = psbt.psbt().inputs[index].witness_script.clone().unwrap();
    UnifiedSighashCache::new(&psbt.psbt().unsigned_tx, &spent_outputs)
        .unwrap()
        .signature_hash(
            index,
            UNIFIED_SIGHASH_ALL,
            SCRIPT_TYPE_WITNESS_V0,
            &witness_script,
        )
        .unwrap()
}

struct FfiResult {
    code: i32,
    detail: CcErrorDetail,
    message: String,
}

fn psbt_digest_through_ffi(bytes: &[u8], index: u32) -> (FfiResult, [u8; 32]) {
    let mut digest = [0u8; 32];
    let mut detail = CcErrorDetail::default();
    let mut message = [0u8; 512];
    // Safety: all pointers come from live locals and the lengths match.
    let code = unsafe {
        coincube_unified_psbt_digest(
            bytes.as_ptr(),
            bytes.len(),
            index,
            digest.as_mut_ptr(),
            digest.len(),
            &mut detail,
            message.as_mut_ptr(),
            message.len(),
        )
    };
    let text =
        String::from_utf8_lossy(&message[..detail.message_len.min(message.len())]).to_string();
    (
        FfiResult {
            code,
            detail,
            message: text,
        },
        digest,
    )
}

fn verify_through_ffi(bytes: &[u8]) -> (FfiResult, usize) {
    let mut verified = 0usize;
    let mut detail = CcErrorDetail::default();
    let mut message = [0u8; 512];
    // Safety: all pointers come from live locals and the lengths match.
    let code = unsafe {
        coincube_unified_psbt_verify(
            bytes.as_ptr(),
            bytes.len(),
            &mut verified,
            &mut detail,
            message.as_mut_ptr(),
            message.len(),
        )
    };
    let text =
        String::from_utf8_lossy(&message[..detail.message_len.min(message.len())]).to_string();
    (
        FfiResult {
            code,
            detail,
            message: text,
        },
        verified,
    )
}

/// Sign through the ABI using the two-call length protocol.
fn sign_through_ffi(bytes: &[u8], mnemonic: &str) -> (FfiResult, Vec<u8>) {
    let phrase = mnemonic.as_bytes();
    let mut required = 0usize;
    let mut detail = CcErrorDetail::default();
    let mut message = [0u8; 512];

    // First call: zero capacity, to learn the length.
    // Safety: `psbt_out` is null and `psbt_out_cap` is zero, which the contract
    // allows; every other pointer is a live local.
    let code = unsafe {
        coincube_unified_psbt_sign(
            bytes.as_ptr(),
            bytes.len(),
            phrase.as_ptr(),
            phrase.len(),
            CC_NETWORK_BITCOIN,
            std::ptr::null_mut(),
            0,
            &mut required,
            &mut detail,
            message.as_mut_ptr(),
            message.len(),
        )
    };
    if code != CC_ERR_BUFFER_TOO_SMALL {
        let text =
            String::from_utf8_lossy(&message[..detail.message_len.min(message.len())]).to_string();
        return (
            FfiResult {
                code,
                detail,
                message: text,
            },
            Vec::new(),
        );
    }
    assert_eq!(
        detail.detail_a as usize, required,
        "CC_ERR_BUFFER_TOO_SMALL should report the required length both ways"
    );

    // Second call: a buffer of exactly that size.
    let mut out = vec![0u8; required];
    let mut written = 0usize;
    let mut detail = CcErrorDetail::default();
    // Safety: `out` holds `required` writable bytes; the rest are live locals.
    let code = unsafe {
        coincube_unified_psbt_sign(
            bytes.as_ptr(),
            bytes.len(),
            phrase.as_ptr(),
            phrase.len(),
            CC_NETWORK_BITCOIN,
            out.as_mut_ptr(),
            out.len(),
            &mut written,
            &mut detail,
            message.as_mut_ptr(),
            message.len(),
        )
    };
    let text =
        String::from_utf8_lossy(&message[..detail.message_len.min(message.len())]).to_string();
    out.truncate(written);
    (
        FfiResult {
            code,
            detail,
            message: text,
        },
        out,
    )
}

/// Every input's digest, through the ABI, equals the digest core computes.
#[test]
fn psbt_digest_matches_core_for_every_input() {
    let fixture = fixture(3);
    let bytes = export_standard(&fixture.psbt).unwrap();
    for index in 0..fixture.psbt.psbt().inputs.len() {
        let expected = core_digest(&fixture.psbt, index);
        let (result, digest) = psbt_digest_through_ffi(&bytes, index as u32);
        assert_eq!(
            result.code, CC_OK,
            "input {index}: {} ({})",
            result.code, result.message
        );
        assert_eq!(
            digest, expected,
            "input {index}: FFI digest differs from core's"
        );
    }
}

/// The two entries agree: a P2WSH input's digest is the raw-fields digest of the
/// same fields.
///
/// This is the seam that matters. Entry (1) carries the known-answer test but is
/// not what production calls; entry (2) is production but has no upstream
/// vectors. Pinning them to each other is what transfers the corpus's authority
/// onto the entry Keychain actually uses.
#[test]
fn psbt_entry_agrees_with_raw_fields_entry() {
    use coincube_core::miniscript::bitcoin::consensus::serialize;

    let fixture = fixture(2);
    let bytes = export_standard(&fixture.psbt).unwrap();
    let spent_outputs: Vec<TxOut> = fixture
        .psbt
        .psbt()
        .inputs
        .iter()
        .map(|input| input.witness_utxo.clone().unwrap())
        .collect();
    let raw_tx = serialize(&fixture.psbt.psbt().unsigned_tx);
    let prevouts = serialize(&spent_outputs);

    for index in 0..fixture.psbt.psbt().inputs.len() {
        let witness_script = fixture.psbt.psbt().inputs[index]
            .witness_script
            .clone()
            .unwrap();
        let script_bytes = witness_script.as_bytes();

        let mut raw_digest = [0u8; 32];
        let mut detail = CcErrorDetail::default();
        // Safety: all pointers come from live locals and the lengths match.
        let code = unsafe {
            coincube_unified_sighash_digest(
                raw_tx.as_ptr(),
                raw_tx.len(),
                index as u32,
                UNIFIED_SIGHASH_ALL,
                SCRIPT_TYPE_WITNESS_V0,
                prevouts.as_ptr(),
                prevouts.len(),
                script_bytes.as_ptr(),
                script_bytes.len(),
                raw_digest.as_mut_ptr(),
                raw_digest.len(),
                &mut detail,
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(code, CC_OK, "input {index}: raw entry refused");

        let (result, psbt_digest) = psbt_digest_through_ffi(&bytes, index as u32);
        assert_eq!(result.code, CC_OK, "input {index}: {}", result.message);
        assert_eq!(
            psbt_digest, raw_digest,
            "input {index}: the PSBT entry and the raw-fields entry must produce \
             the same message"
        );
    }
}

/// Sign, then verify, through the ABI.
#[test]
fn sign_then_verify_through_the_ffi() {
    let fixture = fixture(2);
    let bytes = export_standard(&fixture.psbt).unwrap();

    // Nothing signed yet: validation passes, zero signatures verified.
    let (result, verified) = verify_through_ffi(&bytes);
    assert_eq!(result.code, CC_OK, "{}", result.message);
    assert_eq!(
        verified, 0,
        "an unsigned PSBT verifies zero signatures, which is not the same as \
         being finalizable"
    );

    let phrase = fixture.signers[0].mnemonic_str();
    let (result, signed) = sign_through_ffi(&bytes, &phrase);
    assert_eq!(result.code, CC_OK, "{}", result.message);
    assert!(!signed.is_empty(), "signed PSBT should not be empty");
    assert_ne!(signed, bytes, "signing should change the PSBT it was given");

    let (result, verified) = verify_through_ffi(&signed);
    assert_eq!(result.code, CC_OK, "{}", result.message);
    assert_eq!(
        verified, 2,
        "one signer holding a key in both inputs produces two unified \
         signatures"
    );
}

/// A second signer's signatures accumulate rather than replacing the first's.
#[test]
fn second_signer_accumulates() {
    let fixture = fixture(1);
    let bytes = export_standard(&fixture.psbt).unwrap();

    let (result, once) = sign_through_ffi(&bytes, &fixture.signers[0].mnemonic_str());
    assert_eq!(result.code, CC_OK, "{}", result.message);
    let (result, twice) = sign_through_ffi(&once, &fixture.signers[1].mnemonic_str());
    assert_eq!(result.code, CC_OK, "{}", result.message);

    let (result, verified) = verify_through_ffi(&twice);
    assert_eq!(result.code, CC_OK, "{}", result.message);
    assert_eq!(verified, 2, "both signers' records should be present");
}

/// A PSBT carrying only `non_witness_utxo` still produces a digest.
///
/// Regression test. The first version of this crate built its spent-output
/// vector by reading `witness_utxo` from each input, which looks right and is
/// backwards: core's `authenticate_previous_output` *requires* the full
/// `non_witness_utxo` and treats `witness_utxo` as an optional cross-check. So a
/// PSBT that `unified_signing` accepts — this one — was refused at the boundary
/// with "has no witness utxo". Going through core's own authentication fixed
/// that and picked up its txid/vout/amount checks at the same time.
#[test]
fn witness_utxo_is_not_required_when_the_previous_transaction_is_present() {
    let fixture = fixture(2);
    let mut stripped = fixture.psbt.clone();
    for input in stripped.psbt_mut().inputs.iter_mut() {
        assert!(
            input.non_witness_utxo.is_some(),
            "core requires the full previous transaction"
        );
        input.witness_utxo = None;
    }
    let bytes = export_standard(&stripped).unwrap();

    for index in 0..stripped.psbt().inputs.len() {
        let (result, digest) = psbt_digest_through_ffi(&bytes, index as u32);
        assert_eq!(
            result.code, CC_OK,
            "input {index}: {} ({})",
            result.code, result.message
        );
        // Same message as the fully-populated fixture: dropping an optional
        // cross-check must not change what gets signed.
        assert_eq!(
            digest,
            core_digest(&fixture.psbt, index),
            "input {index}: digest changed when witness_utxo was dropped"
        );
    }
}

/// Dropping the previous transaction *is* refused, because core needs it to
/// authenticate the prevout at all.
#[test]
fn missing_previous_transaction_is_refused() {
    let fixture = fixture(1);
    let mut broken = fixture.psbt.clone();
    broken.psbt_mut().inputs[0].non_witness_utxo = None;
    let bytes = export_standard(&broken).unwrap();

    let (result, _) = psbt_digest_through_ffi(&bytes, 0);
    assert_eq!(
        result.code, CC_ERR_PSBT_VALIDATION,
        "expected a validation refusal, got {} ({})",
        result.code, result.message
    );
}

/// Core owns the refusal: an input missing its witness script is rejected by
/// `unified_signing`, and the boundary reports it rather than computing a digest
/// over a script it guessed.
#[test]
fn missing_witness_script_is_refused_with_cores_reason() {
    let fixture = fixture(1);
    let mut broken = fixture.psbt.clone();
    broken.psbt_mut().inputs[0].witness_script = None;
    let bytes = export_standard(&broken).unwrap();

    let (result, _) = psbt_digest_through_ffi(&bytes, 0);
    assert_eq!(
        result.code, CC_ERR_PSBT_VALIDATION,
        "expected a validation refusal, got {} ({})",
        result.code, result.message
    );
    assert!(
        !result.message.is_empty(),
        "the refusal should carry core's own reason"
    );
}

/// A prevout that is not native P2WSH is refused.
#[test]
fn non_p2wsh_prevout_is_refused() {
    let fixture = fixture(1);
    let mut broken = fixture.psbt.clone();
    let mut spent = broken.psbt().inputs[0].witness_utxo.clone().unwrap();
    spent.script_pubkey = ScriptBuf::new_p2wpkh(
        &bitcoin::PublicKey::from_slice(&[
            0x02, 0xc6, 0x04, 0x7f, 0x94, 0x41, 0xed, 0x7d, 0x6d, 0x30, 0x45, 0x40, 0x6e, 0x95,
            0xc0, 0x7c, 0xd8, 0x5c, 0x77, 0x8e, 0x4b, 0x8c, 0xef, 0x3c, 0xa7, 0xab, 0xac, 0x09,
            0xb9, 0x5c, 0x70, 0x9e, 0xe5,
        ])
        .unwrap()
        .wpubkey_hash()
        .unwrap(),
    );
    broken.psbt_mut().inputs[0].witness_utxo = Some(spent);
    broken.psbt_mut().inputs[0].non_witness_utxo = None;
    let bytes = export_standard(&broken).unwrap();

    let (result, _) = psbt_digest_through_ffi(&bytes, 0);
    assert_eq!(
        result.code, CC_ERR_PSBT_VALIDATION,
        "expected a validation refusal, got {} ({})",
        result.code, result.message
    );
}

/// An out-of-range input index is a typed refusal, not a panic.
#[test]
fn input_index_out_of_bounds_is_typed() {
    let fixture = fixture(1);
    let bytes = export_standard(&fixture.psbt).unwrap();
    let (result, _) = psbt_digest_through_ffi(&bytes, 9);
    assert_eq!(
        result.code, CC_ERR_INPUT_INDEX_OUT_OF_BOUNDS,
        "got {} ({})",
        result.code, result.message
    );
    assert_eq!(result.detail.detail_a, 9);
    assert_eq!(result.detail.detail_b, 1);
}

/// Bytes that are not a PSBT are refused at the adapter, not deeper in.
#[test]
fn garbage_is_not_a_psbt() {
    let (result, _) = psbt_digest_through_ffi(b"not a psbt at all", 0);
    assert_eq!(
        result.code, CC_ERR_INVALID_PSBT,
        "got {} ({})",
        result.code, result.message
    );
}

/// An unknown network code is rejected before any key material is touched.
#[test]
fn unknown_network_is_rejected() {
    let fixture = fixture(1);
    let bytes = export_standard(&fixture.psbt).unwrap();
    let phrase = fixture.signers[0].mnemonic_str();
    let mut detail = CcErrorDetail::default();
    let mut written = 0usize;
    // Safety: null output with zero capacity is allowed; the rest are locals.
    let code = unsafe {
        coincube_unified_psbt_sign(
            bytes.as_ptr(),
            bytes.len(),
            phrase.as_bytes().as_ptr(),
            phrase.len(),
            99,
            std::ptr::null_mut(),
            0,
            &mut written,
            &mut detail,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(code, CC_ERR_UNKNOWN_NETWORK);
    assert_eq!(detail.detail_a, 99);
}

/// The ABI reports its own revision and digest length.
#[test]
fn abi_metadata_is_exported() {
    assert_eq!(coincube_keychain_ffi_abi_version(), 1);
    assert_eq!(coincube_keychain_ffi_digest_len(), 32);
    assert_eq!(CC_DIGEST_LEN, 32);
}
