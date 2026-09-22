//! The ABI contract itself: every typed refusal the raw-fields entry can
//! produce, and the boundary's behaviour at its edges.
//!
//! The 142-vector KAT proves the digest is right and proves two of the six
//! `UnifiedSighashError` mappings (`UnsupportedScriptType`, and
//! `MissingUnifiedFlag` by its absence). It cannot reach the other four, or any
//! of the pointer and buffer cases, because every corpus row is well-formed.
//! Those are the mistakes a Dart caller will actually make, so they are covered
//! here instead.

use coincube_core::miniscript::bitcoin::{
    absolute, consensus::serialize, transaction, Amount, OutPoint, ScriptBuf, Sequence,
    Transaction, TxIn, TxOut, Witness,
};
use coincube_keychain_ffi::*;

/// `SIGHASH_ALL | SIGHASH_UNIFIED`.
const ALL_UNIFIED: u8 = 0x21;
/// `SIGHASH_SINGLE | SIGHASH_UNIFIED`.
const SINGLE_UNIFIED: u8 = 0x23;

fn transaction_with(inputs: usize, outputs: usize) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: (0..inputs)
            .map(|index| TxIn {
                previous_output: OutPoint {
                    txid: OutPoint::null().txid,
                    vout: index as u32,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            })
            .collect(),
        output: (0..outputs)
            .map(|index| TxOut {
                value: Amount::from_sat(1_000 + index as u64),
                script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
            })
            .collect(),
    }
}

fn prevouts(count: usize) -> Vec<TxOut> {
    (0..count)
        .map(|index| TxOut {
            value: Amount::from_sat(5_000 + index as u64),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51, 0x51]),
        })
        .collect()
}

struct Outcome {
    code: i32,
    detail: CcErrorDetail,
    digest: [u8; 32],
    message: String,
}

/// Call the raw-fields entry. Every buffer is real; only the values vary.
fn call(tx: &Transaction, index: u32, hash_type: u8, script_type: u8, spent: &[TxOut]) -> Outcome {
    let tx_bytes = serialize(tx);
    let prevout_bytes = serialize(&spent.to_vec());
    let script_code = [0x51u8, 0x51];
    let mut digest = [0u8; 32];
    let mut detail = CcErrorDetail::default();
    let mut message = [0u8; 256];
    // Safety: every pointer is a live local and every length matches its buffer.
    let code = unsafe {
        coincube_unified_sighash_digest(
            tx_bytes.as_ptr(),
            tx_bytes.len(),
            index,
            hash_type,
            script_type,
            prevout_bytes.as_ptr(),
            prevout_bytes.len(),
            script_code.as_ptr(),
            script_code.len(),
            digest.as_mut_ptr(),
            digest.len(),
            &mut detail,
            message.as_mut_ptr(),
            message.len(),
        )
    };
    let written = detail.message_len.min(message.len());
    Outcome {
        code,
        detail,
        digest,
        message: String::from_utf8_lossy(&message[..written]).to_string(),
    }
}

/// The baseline every negative case below is a single mutation away from.
#[test]
fn a_well_formed_call_succeeds() {
    let outcome = call(&transaction_with(1, 1), 0, ALL_UNIFIED, 0, &prevouts(1));
    assert_eq!(outcome.code, CC_OK, "{}", outcome.message);
    assert_ne!(
        outcome.digest, [0u8; 32],
        "a digest should have been written"
    );
    assert_eq!(outcome.detail.message_len, 0, "success carries no message");
}

/// `MissingUnifiedFlag` — a hash type that did not opt in.
///
/// Every corpus row sets `0x20`, so the KAT can only ever prove this mapping by
/// its absence. Here it is proven directly.
#[test]
fn hash_type_without_the_unified_flag_is_refused() {
    let outcome = call(&transaction_with(1, 1), 0, 0x01, 0, &prevouts(1));
    assert_eq!(outcome.code, CC_ERR_MISSING_UNIFIED_FLAG);
    assert_eq!(outcome.detail.detail_a, 0x01);
    assert!(!outcome.message.is_empty());
}

/// `PrevoutsLengthMismatch` — one spent output per input, or nothing doing.
#[test]
fn prevout_count_must_match_input_count() {
    let outcome = call(&transaction_with(1, 1), 0, ALL_UNIFIED, 0, &prevouts(2));
    assert_eq!(outcome.code, CC_ERR_PREVOUTS_LENGTH_MISMATCH);
    assert_eq!(outcome.detail.detail_a, 1, "inputs");
    assert_eq!(outcome.detail.detail_b, 2, "prevouts");
}

/// `InputIndexOutOfBounds` — with both numeric fields carried across.
#[test]
fn input_index_beyond_the_transaction_is_refused() {
    let outcome = call(&transaction_with(2, 2), 5, ALL_UNIFIED, 0, &prevouts(2));
    assert_eq!(outcome.code, CC_ERR_INPUT_INDEX_OUT_OF_BOUNDS);
    assert_eq!(outcome.detail.detail_a, 5, "index");
    assert_eq!(outcome.detail.detail_b, 2, "inputs");
}

/// `InputIndexTooLarge` is unreachable through this ABI, by construction.
///
/// Core returns it when an index does not fit `u32`. This boundary takes the
/// index *as* a `u32`, so on any target where `usize` is at least 32 bits the
/// conversion inside core cannot fail and the bounds check at
/// `unified_sighash.rs:176` fires first. `CC_ERR_INPUT_INDEX_TOO_LARGE` is
/// therefore declared for completeness of the mapping rather than as a code a
/// caller can provoke — asserted here so that stops being folklore.
#[test]
fn input_index_too_large_is_unreachable_through_this_abi() {
    let outcome = call(
        &transaction_with(1, 1),
        u32::MAX,
        ALL_UNIFIED,
        0,
        &prevouts(1),
    );
    assert_eq!(
        outcome.code, CC_ERR_INPUT_INDEX_OUT_OF_BOUNDS,
        "u32::MAX is out of bounds, not too large: {}",
        outcome.message
    );
    assert_ne!(outcome.code, CC_ERR_INPUT_INDEX_TOO_LARGE);
}

/// `MissingSingleOutput` — `SIGHASH_SINGLE` needs an output at the input's index.
#[test]
fn sighash_single_without_a_matching_output_is_refused() {
    let outcome = call(&transaction_with(2, 1), 1, SINGLE_UNIFIED, 0, &prevouts(2));
    assert_eq!(outcome.code, CC_ERR_MISSING_SINGLE_OUTPUT);
    assert_eq!(outcome.detail.detail_a, 1, "index");
    assert_eq!(outcome.detail.detail_b, 1, "outputs");
}

/// Bytes that are not a transaction.
#[test]
fn malformed_transaction_bytes_are_refused() {
    let junk = [0xffu8; 9];
    let prevout_bytes = serialize(&prevouts(1));
    let mut digest = [0u8; 32];
    let mut detail = CcErrorDetail::default();
    // Safety: live locals, matching lengths.
    let code = unsafe {
        coincube_unified_sighash_digest(
            junk.as_ptr(),
            junk.len(),
            0,
            ALL_UNIFIED,
            0,
            prevout_bytes.as_ptr(),
            prevout_bytes.len(),
            std::ptr::null(),
            0,
            digest.as_mut_ptr(),
            digest.len(),
            &mut detail,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(code, CC_ERR_INVALID_TRANSACTION);
    assert!(
        detail.message_len > 0,
        "the decode error's own text should be reported even with no message buffer"
    );
}

/// Bytes that are not a consensus-encoded `TxOut` vector.
#[test]
fn malformed_spent_outputs_are_refused() {
    let tx_bytes = serialize(&transaction_with(1, 1));
    let junk = [0xfeu8; 3];
    let mut digest = [0u8; 32];
    let mut detail = CcErrorDetail::default();
    // Safety: live locals, matching lengths.
    let code = unsafe {
        coincube_unified_sighash_digest(
            tx_bytes.as_ptr(),
            tx_bytes.len(),
            0,
            ALL_UNIFIED,
            0,
            junk.as_ptr(),
            junk.len(),
            std::ptr::null(),
            0,
            digest.as_mut_ptr(),
            digest.len(),
            &mut detail,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(code, CC_ERR_INVALID_SPENT_OUTPUTS);
}

/// A null input pointer with a non-zero length is refused, not dereferenced.
#[test]
fn null_input_pointer_with_a_length_is_refused() {
    let prevout_bytes = serialize(&prevouts(1));
    let mut digest = [0u8; 32];
    // Safety: the null pointer is paired with a non-zero length precisely to
    // check that the boundary rejects it instead of reading it.
    let code = unsafe {
        coincube_unified_sighash_digest(
            std::ptr::null(),
            32,
            0,
            ALL_UNIFIED,
            0,
            prevout_bytes.as_ptr(),
            prevout_bytes.len(),
            std::ptr::null(),
            0,
            digest.as_mut_ptr(),
            digest.len(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(code, CC_ERR_NULL_ARGUMENT);
}

/// An empty script code is legitimate, and a null pointer is fine at length zero.
///
/// `slice::from_raw_parts` on a null pointer is undefined behaviour even for a
/// zero length, so this is the case the `borrow` helper exists for.
#[test]
fn empty_script_code_with_a_null_pointer_is_accepted() {
    let tx_bytes = serialize(&transaction_with(1, 1));
    let prevout_bytes = serialize(&prevouts(1));
    let mut digest = [0u8; 32];
    // Safety: a null pointer at length zero is explicitly allowed by the ABI.
    let code = unsafe {
        coincube_unified_sighash_digest(
            tx_bytes.as_ptr(),
            tx_bytes.len(),
            0,
            ALL_UNIFIED,
            0,
            prevout_bytes.as_ptr(),
            prevout_bytes.len(),
            std::ptr::null(),
            0,
            digest.as_mut_ptr(),
            digest.len(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(code, CC_OK);
    assert_ne!(digest, [0u8; 32]);
}

/// A digest buffer shorter than 32 bytes is refused before anything is written.
#[test]
fn short_digest_buffer_is_refused() {
    let tx_bytes = serialize(&transaction_with(1, 1));
    let prevout_bytes = serialize(&prevouts(1));
    let mut digest = [0u8; 31];
    // Safety: the buffer really is 31 bytes; the point is that the boundary
    // checks the length rather than writing 32.
    let code = unsafe {
        coincube_unified_sighash_digest(
            tx_bytes.as_ptr(),
            tx_bytes.len(),
            0,
            ALL_UNIFIED,
            0,
            prevout_bytes.as_ptr(),
            prevout_bytes.len(),
            std::ptr::null(),
            0,
            digest.as_mut_ptr(),
            digest.len(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(code, CC_ERR_NULL_ARGUMENT);
    assert_eq!(digest, [0u8; 31], "nothing should have been written");
}

/// Null optional out-parameters are ignored rather than written through.
#[test]
fn null_optional_out_parameters_are_ignored() {
    let tx_bytes = serialize(&transaction_with(1, 1));
    let prevout_bytes = serialize(&prevouts(2));
    let mut digest = [0u8; 32];
    // Safety: both optional out-parameters are null, which the ABI allows.
    let code = unsafe {
        coincube_unified_sighash_digest(
            tx_bytes.as_ptr(),
            tx_bytes.len(),
            0,
            ALL_UNIFIED,
            0,
            prevout_bytes.as_ptr(),
            prevout_bytes.len(),
            std::ptr::null(),
            0,
            digest.as_mut_ptr(),
            digest.len(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(
        code, CC_ERR_PREVOUTS_LENGTH_MISMATCH,
        "the status code must survive with no detail or message buffer"
    );
}

/// A message buffer smaller than the message is filled and the full length
/// reported, so a caller can size a buffer and ask again.
#[test]
fn message_truncation_reports_the_required_length() {
    let tx_bytes = serialize(&transaction_with(1, 1));
    let prevout_bytes = serialize(&prevouts(1));
    let mut digest = [0u8; 32];
    let mut detail = CcErrorDetail::default();
    let mut tiny = [0u8; 4];
    // Safety: `tiny` really is 4 bytes; the boundary must not write past it.
    let code = unsafe {
        coincube_unified_sighash_digest(
            tx_bytes.as_ptr(),
            tx_bytes.len(),
            0,
            0x01, // no unified flag, so there is a message to truncate
            0,
            prevout_bytes.as_ptr(),
            prevout_bytes.len(),
            std::ptr::null(),
            0,
            digest.as_mut_ptr(),
            digest.len(),
            &mut detail,
            tiny.as_mut_ptr(),
            tiny.len(),
        )
    };
    assert_eq!(code, CC_ERR_MISSING_UNIFIED_FLAG);
    assert!(
        detail.message_len > tiny.len(),
        "message_len should be the length required ({}), not the length written",
        detail.message_len
    );
    assert_eq!(
        &tiny,
        b"hash".as_slice(),
        "the prefix of core's message should be present"
    );
}

/// With no message buffer at all, the detail still reports the length required.
#[test]
fn absent_message_buffer_still_reports_the_required_length() {
    let outcome_detail = {
        let tx_bytes = serialize(&transaction_with(1, 1));
        let prevout_bytes = serialize(&prevouts(1));
        let mut digest = [0u8; 32];
        let mut detail = CcErrorDetail::default();
        // Safety: live locals; the message buffer is deliberately null.
        unsafe {
            coincube_unified_sighash_digest(
                tx_bytes.as_ptr(),
                tx_bytes.len(),
                0,
                0x01,
                0,
                prevout_bytes.as_ptr(),
                prevout_bytes.len(),
                std::ptr::null(),
                0,
                digest.as_mut_ptr(),
                digest.len(),
                &mut detail,
                std::ptr::null_mut(),
                0,
            )
        };
        detail
    };
    assert!(outcome_detail.message_len > 0);
    assert_eq!(outcome_detail.detail_a, 0x01);
}

/// The detail struct is reset on entry, so a reused one cannot leak a stale
/// value into a later successful call.
#[test]
fn detail_is_cleared_on_entry() {
    let mut detail = CcErrorDetail {
        detail_a: 0xdead,
        detail_b: 0xbeef,
        message_len: 999,
    };
    let tx_bytes = serialize(&transaction_with(1, 1));
    let prevout_bytes = serialize(&prevouts(1));
    let mut digest = [0u8; 32];
    // Safety: live locals, matching lengths.
    let code = unsafe {
        coincube_unified_sighash_digest(
            tx_bytes.as_ptr(),
            tx_bytes.len(),
            0,
            ALL_UNIFIED,
            0,
            prevout_bytes.as_ptr(),
            prevout_bytes.len(),
            std::ptr::null(),
            0,
            digest.as_mut_ptr(),
            digest.len(),
            &mut detail,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(code, CC_OK);
    assert_eq!(
        detail.detail_a, 0,
        "stale detail_a survived a successful call"
    );
    assert_eq!(
        detail.detail_b, 0,
        "stale detail_b survived a successful call"
    );
    assert_eq!(detail.message_len, 0, "stale message_len survived");
}
