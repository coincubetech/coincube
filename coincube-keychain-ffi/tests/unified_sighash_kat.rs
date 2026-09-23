//! Known-answer test: every applicable upstream unified-sighash vector, driven
//! through the C ABI rather than through `coincube_core` directly.
//!
//! `coincube-core` already pins this corpus in
//! `unified_sighash::tests::matches_all_applicable_upstream_vectors`. The point
//! of running it again here is the boundary: this file calls the `extern "C"`
//! symbols Keychain will link, so a field transposed on the way across — a
//! hash type dropped, a spent-output vector decoded in the wrong order, an
//! index truncated — fails here and not in production.
//!
//! # Why the raw-fields entry and not the PSBT entry
//!
//! The corpus cannot be driven through a PSBT/P2WSH entry point. Its spent
//! outputs are synthetic scripts (`51`, `5151`, `515151`, …): across the 142
//! supported rows, **0 of the 303** spent outputs is a v0 witness program, and
//! for **0 of the 66** witness-v0 rows does `spentOutputs[inIdx].scriptPubKey`
//! equal `P2WSH(scriptCode)`. `unified_signing::validate_inputs` rejects on
//! each of those independently, and upstream of all of them the signing path
//! can express only `SIGHASH_ALL | SIGHASH_UNIFIED` while this corpus spans
//! many hash types. The PSBT-shaped entry is covered by
//! `tests/psbt_entry.rs` with genuine P2WSH fixtures instead.
//!
//! # The corpus path is pinned deliberately
//!
//! `include_str!` reaches the in-repo copy at compile time. A byte-identical
//! copy of this file exists outside the repository; a test that read *that*
//! path would pass on a developer machine and fail in CI.

use coincube_core::miniscript::bitcoin::{
    consensus::serialize,
    hashes::{sha256, Hash},
    hex::FromHex,
    Amount, ScriptBuf, TxOut,
};
use coincube_keychain_ffi::*;
use serde_json::Value;

/// The in-repo corpus. Compile-time, so a moved file is a build failure.
const CORPUS: &str = include_str!("../../coincube-core/tests/data/unified_sighash.json");

/// Byte length of the corpus, as measured at `4bfb755b`.
const CORPUS_BYTES: usize = 75_340;
/// SHA-256 of the corpus, as measured at `4bfb755b`.
const CORPUS_SHA256: &str = "5c5e95fc1ab8ef9ce6b3cb6e76b8c74a987d182ebd01d8e2b98b6d0fbb26f630";

/// Rows this implementation computes: script type 0 (bare/P2SH) and 1 (SegWit v0).
const EXPECTED_SUPPORTED: usize = 142;
/// Rows deliberately out of scope: 12 Taproot key-path and 12 tapscript.
const EXPECTED_UNSUPPORTED: usize = 24;
/// Supported rows that set `SIGHASH_ANYONECANPAY`.
///
/// Asserted so that pushing Lane B3.2's ANYONECANPAY *spend-policy* refusal
/// down into the digest layer cannot pass unnoticed. It would make these 70
/// rows unreachable while the suite still looked green on the other 72.
const EXPECTED_ANYONECANPAY: usize = 70;
/// Supported rows that set `SIGHASH_UNIFIED`: all of them.
const EXPECTED_UNIFIED_FLAG: usize = 142;

struct Row {
    script_code: Vec<u8>,
    raw_tx: Vec<u8>,
    input_index: u32,
    hash_type: u8,
    script_type: u8,
    spent_outputs: Vec<TxOut>,
    expected: [u8; 32],
}

/// Parse the corpus, skipping its header row exactly as core's own test does.
fn rows() -> Vec<Row> {
    let parsed: Vec<Value> = serde_json::from_str(CORPUS).expect("corpus is valid JSON");
    assert_eq!(
        parsed.len(),
        167,
        "corpus should hold one header row plus 166 data rows"
    );
    parsed
        .into_iter()
        .skip(1)
        .map(|row| {
            let fields = row.as_array().expect("row is an array");
            Row {
                script_code: Vec::from_hex(fields[0].as_str().unwrap()).unwrap(),
                raw_tx: Vec::from_hex(fields[1].as_str().unwrap()).unwrap(),
                input_index: fields[2].as_u64().unwrap() as u32,
                hash_type: fields[3].as_u64().unwrap() as u8,
                script_type: fields[4].as_u64().unwrap() as u8,
                spent_outputs: fields[5]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| {
                        let output = value.as_array().unwrap();
                        TxOut {
                            value: Amount::from_sat(output[0].as_u64().unwrap()),
                            script_pubkey: ScriptBuf::from_bytes(
                                Vec::from_hex(output[1].as_str().unwrap()).unwrap(),
                            ),
                        }
                    })
                    .collect(),
                expected: <[u8; 32]>::from_hex(fields[6].as_str().unwrap()).unwrap(),
            }
        })
        .collect()
}

/// Call the raw-fields entry for one row and return `(code, digest, detail)`.
fn digest_through_ffi(row: &Row) -> (i32, [u8; 32], CcErrorDetail) {
    let prevouts = serialize(&row.spent_outputs);
    let mut digest = [0u8; 32];
    let mut detail = CcErrorDetail::default();
    let mut message = [0u8; 256];
    // Safety: every pointer below is derived from a live local that outlives
    // the call, and each length matches its buffer.
    let code = unsafe {
        coincube_unified_sighash_digest(
            row.raw_tx.as_ptr(),
            row.raw_tx.len(),
            row.input_index,
            row.hash_type,
            row.script_type,
            prevouts.as_ptr(),
            prevouts.len(),
            row.script_code.as_ptr(),
            row.script_code.len(),
            digest.as_mut_ptr(),
            digest.len(),
            &mut detail,
            message.as_mut_ptr(),
            message.len(),
        )
    };
    (code, digest, detail)
}

/// Name a status code, so a failure report says what went wrong rather than
/// making the reader look the number up.
fn code_name(code: i32) -> &'static str {
    match code {
        CC_OK => "CC_OK",
        CC_ERR_NULL_ARGUMENT => "CC_ERR_NULL_ARGUMENT",
        CC_ERR_BUFFER_TOO_SMALL => "CC_ERR_BUFFER_TOO_SMALL",
        CC_ERR_INVALID_TRANSACTION => "CC_ERR_INVALID_TRANSACTION",
        CC_ERR_INVALID_SPENT_OUTPUTS => "CC_ERR_INVALID_SPENT_OUTPUTS",
        CC_ERR_PANIC => "CC_ERR_PANIC",
        CC_ERR_MISSING_UNIFIED_FLAG => {
            "CC_ERR_MISSING_UNIFIED_FLAG (a plumbing bug at this boundary: \
             every corpus row sets 0x20)"
        }
        CC_ERR_UNSUPPORTED_SCRIPT_TYPE => "CC_ERR_UNSUPPORTED_SCRIPT_TYPE",
        CC_ERR_PREVOUTS_LENGTH_MISMATCH => "CC_ERR_PREVOUTS_LENGTH_MISMATCH",
        CC_ERR_INPUT_INDEX_OUT_OF_BOUNDS => "CC_ERR_INPUT_INDEX_OUT_OF_BOUNDS",
        CC_ERR_INPUT_INDEX_TOO_LARGE => "CC_ERR_INPUT_INDEX_TOO_LARGE",
        CC_ERR_MISSING_SINGLE_OUTPUT => "CC_ERR_MISSING_SINGLE_OUTPUT",
        _ => "unknown code",
    }
}

/// The corpus this test pins is the one it was written against.
///
/// Byte length, digest and row count are all asserted: a swapped corpus fails
/// loudly here instead of quietly changing what the counts below mean.
#[test]
fn corpus_is_the_pinned_one() {
    assert_eq!(CORPUS.len(), CORPUS_BYTES, "corpus byte length changed");
    assert_eq!(
        sha256::Hash::hash(CORPUS.as_bytes()).to_string(),
        CORPUS_SHA256,
        "corpus SHA-256 changed"
    );
    assert_eq!(rows().len(), 166, "corpus data-row count changed");
}

/// Every supported row's digest, through the ABI, byte for byte against core's.
#[test]
fn supported_vectors_match_core_through_the_ffi() {
    let mut checked = 0usize;
    let mut anyonecanpay = 0usize;
    let mut unified_flag = 0usize;

    for (index, row) in rows().iter().enumerate() {
        // Row index as core reports it: the header row is row 0, so the first
        // data row is vector 1.
        let vector = index + 1;
        if row.script_type > 1 {
            continue;
        }
        if row.hash_type & 0x80 != 0 {
            anyonecanpay += 1;
        }
        if row.hash_type & 0x20 != 0 {
            unified_flag += 1;
        }

        let (code, digest, detail) = digest_through_ffi(row);
        assert_eq!(
            code,
            CC_OK,
            "vector {vector}: expected CC_OK, got {} ({}) detail_a={} detail_b={}",
            code,
            code_name(code),
            detail.detail_a,
            detail.detail_b
        );
        assert_eq!(
            digest, row.expected,
            "vector {vector}: digest differs from core's expected value \
             (scriptType={}, hashType=0x{:02x})",
            row.script_type, row.hash_type
        );
        checked += 1;
    }

    assert_eq!(
        checked, EXPECTED_SUPPORTED,
        "supported-vector count changed; a corpus swap must not silently \
         reduce coverage"
    );
    assert_eq!(
        anyonecanpay, EXPECTED_ANYONECANPAY,
        "ANYONECANPAY row count changed. If this dropped to zero, an \
         ANYONECANPAY refusal has been pushed into the digest layer, where it \
         does not belong"
    );
    assert_eq!(
        unified_flag, EXPECTED_UNIFIED_FLAG,
        "every supported row must set SIGHASH_UNIFIED (0x20)"
    );
}

/// Every out-of-scope row returns the exact typed refusal, not a panic and not
/// a generic error.
///
/// Reachability is not incidental: core checks the unified flag at
/// `unified_sighash.rs:172`, *before* the script type at `:175`. These rows
/// return `UnsupportedScriptType` only because all 24 of them also set `0x20`
/// (their hash types are `0x21 0x22 0x23 0xa1 0xa2 0xa3`). This test asserts
/// the variant and its script type, so a row losing the unified flag would show
/// up as `CC_ERR_MISSING_UNIFIED_FLAG` here rather than passing as "some error".
#[test]
fn taproot_vectors_return_the_typed_unsupported_refusal() {
    let mut skipped = 0usize;
    let mut key_path = 0usize;
    let mut tapscript = 0usize;

    for (index, row) in rows().iter().enumerate() {
        let vector = index + 1;
        if row.script_type <= 1 {
            continue;
        }
        match row.script_type {
            2 => key_path += 1,
            3 => tapscript += 1,
            other => panic!("vector {vector}: unexpected script type {other}"),
        }

        let (code, _, detail) = digest_through_ffi(row);
        assert_eq!(
            code,
            CC_ERR_UNSUPPORTED_SCRIPT_TYPE,
            "vector {vector}: expected CC_ERR_UNSUPPORTED_SCRIPT_TYPE, got {} ({})",
            code,
            code_name(code)
        );
        assert_eq!(
            detail.detail_a, row.script_type as u64,
            "vector {vector}: refusal should carry the offending script type"
        );
        assert!(
            detail.message_len > 0,
            "vector {vector}: refusal should carry core's message"
        );
        skipped += 1;
    }

    assert_eq!(
        skipped, EXPECTED_UNSUPPORTED,
        "out-of-scope row count changed"
    );
    assert_eq!(key_path, 12, "Taproot key-path row count changed");
    assert_eq!(tapscript, 12, "tapscript row count changed");
}

/// The two counts core pins, pinned again on this side of the boundary.
///
/// `coincube-core` asserts `checked == 142` and `skipped == 24`. Duplicating it
/// here means a corpus swap that shifted the split would fail this crate's
/// suite too, rather than reducing FFI coverage quietly.
#[test]
fn script_type_split_matches_cores_assertion() {
    let rows = rows();
    let supported = rows.iter().filter(|row| row.script_type <= 1).count();
    let unsupported = rows.iter().filter(|row| row.script_type > 1).count();
    assert_eq!(supported, EXPECTED_SUPPORTED);
    assert_eq!(unsupported, EXPECTED_UNSUPPORTED);
    assert_eq!(supported + unsupported, rows.len());
    assert_eq!(
        rows.iter().filter(|row| row.script_type == 0).count(),
        76,
        "base (bare/P2SH) row count changed"
    );
    assert_eq!(
        rows.iter().filter(|row| row.script_type == 1).count(),
        66,
        "witness-v0 row count changed"
    );
}
