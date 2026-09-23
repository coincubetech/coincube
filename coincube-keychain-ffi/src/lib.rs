//! C ABI over `coincube_core`'s unified signature hashing, for the Keychain
//! phone signer.
//!
//! This crate adds **no** cryptography. Every digest it returns comes out of
//! [`coincube_core::unified_sighash`] and every signature out of
//! [`coincube_core::unified_signing`]; the only thing here is the boundary that
//! lets Dart reach them, so that the consensus-critical message has exactly one
//! implementation (launch-GA master decision 6, Lane B3.1).
//!
//! # Two entry points
//!
//! * [`coincube_unified_sighash_digest`] mirrors
//!   [`coincube_core::unified_sighash::unified_sighash`] field for field. It
//!   exists because the upstream vector corpus cannot be driven through a
//!   PSBT-shaped entry: none of its spent outputs is a witness program, so
//!   every row is refused by `unified_signing`'s P2WSH gates before any digest
//!   is computed. This entry is what the known-answer test drives.
//! * [`coincube_unified_psbt_digest`], [`coincube_unified_psbt_sign`] and
//!   [`coincube_unified_psbt_verify`] are the production shape: real PSBT bytes
//!   off the wire, a real Vault P2WSH input.
//!
//! # What this boundary deliberately does not do
//!
//! It applies no spend policy. In particular it does **not** refuse
//! `SIGHASH_ANYONECANPAY` at the digest layer: 70 of the 142 supported upstream
//! vectors set `0x80` and core computes them, so a refusal here would make
//! those 70 unreachable while leaving the test suite green on the remaining 72.
//! Keychain's ANYONECANPAY refusal is a policy gate on the *signing* path
//! (Lane B3.2), not a property of the message.
//!
//! Nor does it decide which chain it is on. `network` selects key and address
//! encodings only; Bitcoin and Bitcoin Blake2b share those. Selecting unified
//! sighashing is the caller's decision, made from an authenticated chain
//! identity (desktop invariant I5).
//!
//! # Calling convention
//!
//! Every entry returns `CC_OK` or one of the `CC_ERR_*` codes below; nothing
//! unwinds into C. Byte buffers travel in as `(pointer, length)`; a zero length
//! is accepted with any pointer value, including null. Nothing is allocated on the Rust side and
//! handed back, so there is no free function: variable-length output uses a
//! caller-supplied buffer and reports the length it needed
//! (`CC_ERR_BUFFER_TOO_SMALL`), which the caller can query by passing a zero
//! capacity first.
//!
//! `error_out` and `message_out` are optional; pass null to ignore them. The
//! message is core's own `Display` text, UTF-8, **not** NUL-terminated.
//!
//! `CcErrorDetail::message_len` is the length **required**, not the length
//! written: when the message did not fit, it is larger than `message_cap`. So
//! read `min(message_cap, message_len)` bytes, or resize to `message_len` and
//! call again. Reading `message_len` bytes unconditionally overruns the
//! caller's own buffer whenever the message was truncated — the write from this
//! side is always bounded by `message_cap`, so that overrun is the caller's read,
//! not a write from here.
//!
//! Truncation is by bytes, not by characters, so a short read can end in the
//! middle of a multi-byte UTF-8 sequence. Decode leniently (Rust's
//! `String::from_utf8_lossy`, Dart's `utf8.decode(..., allowMalformed: true)`) or
//! resize to `message_len` and call again for the whole message.

use std::{panic, slice};

use coincube_core::{
    bip39,
    miniscript::bitcoin::{consensus::deserialize, secp256k1, Network, Script, Transaction, TxOut},
    psbt_unified::{export_standard, import_standard, UnifiedPsbt},
    signer::MasterSigner,
    spend::authenticate_previous_output,
    unified_sighash::{
        unified_sighash, UnifiedSighashCache, UnifiedSighashError, SCRIPT_TYPE_WITNESS_V0,
    },
    unified_signing::{sign_p2wsh_all_unified, verify_p2wsh_all_unified},
};

/// Hash type this crate's PSBT entries commit to: `SIGHASH_ALL | SIGHASH_UNIFIED`.
///
/// Mirrors `unified_signing`'s own private `UNIFIED_SIGHASH_ALL`. The PSBT
/// signing path can express exactly this one value, which is the fourth and
/// conceptually decisive reason the vector corpus cannot be driven through it.
const UNIFIED_SIGHASH_ALL: u8 = 0x21;

/// Length of a unified signature hash.
pub const CC_DIGEST_LEN: usize = 32;

/// ABI revision. Bump on any change to a signature or a code's meaning.
const ABI_VERSION: i32 = 1;

// ---------------------------------------------------------------------------
// Status codes
// ---------------------------------------------------------------------------

/// Success.
pub const CC_OK: i32 = 0;

// Boundary failures: 10-19. These are this crate's own, not core's.

/// A required pointer was null, or an output buffer was too short.
pub const CC_ERR_NULL_ARGUMENT: i32 = 10;
/// Output buffer too small; `detail_a` is the number of bytes required.
pub const CC_ERR_BUFFER_TOO_SMALL: i32 = 11;
/// `tx` was not a consensus-encoded transaction.
pub const CC_ERR_INVALID_TRANSACTION: i32 = 12;
/// `spent_outputs` was not a consensus-encoded `TxOut` vector.
pub const CC_ERR_INVALID_SPENT_OUTPUTS: i32 = 13;
/// A string argument was not valid UTF-8.
pub const CC_ERR_INVALID_UTF8: i32 = 14;
/// `network` was not one of the `CC_NETWORK_*` values; `detail_a` is the value.
pub const CC_ERR_UNKNOWN_NETWORK: i32 = 15;
/// A panic was caught at the boundary.
///
/// Only reachable when this library is built with `panic = "unwind"`, which is
/// the default for the `dev` and `release` profiles. The workspace's `minimal`
/// profile sets `panic = "abort"`, and under that setting a panic terminates the
/// host process before this code can be returned. Keychain's mobile lanes
/// should build this crate with unwind so a bug here is a failed signing
/// attempt rather than a crashed app mid-flow (Lane B3.1b).
pub const CC_ERR_PANIC: i32 = 16;

// `unified_sighash` refusals: 20-25. One code per `UnifiedSighashError`
// variant, mapped by an exhaustive match, so adding a variant to core is a
// compile error here rather than a silent collapse into a neighbouring code.

/// [`UnifiedSighashError::MissingUnifiedFlag`]; `detail_a` is the hash type.
///
/// In the upstream-vector KAT this code is a plumbing bug at this boundary, not
/// a vector problem: all 166 data rows set `0x20`.
pub const CC_ERR_MISSING_UNIFIED_FLAG: i32 = 20;
/// [`UnifiedSighashError::UnsupportedScriptType`]; `detail_a` is the script type.
pub const CC_ERR_UNSUPPORTED_SCRIPT_TYPE: i32 = 21;
/// [`UnifiedSighashError::PrevoutsLengthMismatch`]; `detail_a` inputs, `detail_b` prevouts.
pub const CC_ERR_PREVOUTS_LENGTH_MISMATCH: i32 = 22;
/// [`UnifiedSighashError::InputIndexOutOfBounds`]; `detail_a` index, `detail_b` inputs.
pub const CC_ERR_INPUT_INDEX_OUT_OF_BOUNDS: i32 = 23;
/// [`UnifiedSighashError::InputIndexTooLarge`]; `detail_a` is the index.
pub const CC_ERR_INPUT_INDEX_TOO_LARGE: i32 = 24;
/// [`UnifiedSighashError::MissingSingleOutput`]; `detail_a` index, `detail_b` outputs.
pub const CC_ERR_MISSING_SINGLE_OUTPUT: i32 = 25;

// PSBT and signing refusals: 30-35. Coarser on purpose: these carry core's
// `Display` text in `message_out`, and a per-variant typed mapping of
// `UnifiedSigningError` is Lane B3.2's to add when its UI needs one.

/// The bytes were not a PSBT this adapter accepts.
pub const CC_ERR_INVALID_PSBT: i32 = 30;
/// `unified_signing` refused the PSBT or one of its inputs.
pub const CC_ERR_PSBT_VALIDATION: i32 = 31;
/// The mnemonic was not a valid BIP39 phrase.
pub const CC_ERR_INVALID_MNEMONIC: i32 = 32;
/// Any refusal from core's signing operation, **including the input validation
/// that operation performs itself**.
///
/// `sign_p2wsh_all_unified` validates every input before it signs anything, and
/// this boundary maps every `UnifiedSigningError` it returns to this one code.
/// So a PSBT the digest and verify entries refuse with
/// [`CC_ERR_PSBT_VALIDATION`] is refused by the sign entry with this code
/// instead — same PSBT, same reason, different number. Branch on the code to
/// decide whether signing was attempted, not to decide whether the PSBT was
/// well-formed; `message_out` carries core's own reason text and is accurate in
/// both cases.
///
/// Splitting this into a per-variant typed mapping is Lane B3.2's, as noted on
/// the group comment above, rather than something to add here as a rider.
pub const CC_ERR_SIGNING: i32 = 33;
/// The signed PSBT could not be exported as standard bytes.
pub const CC_ERR_EXPORT: i32 = 34;

/// `Network::Bitcoin` key and address encodings. Bitcoin Blake2b uses these.
pub const CC_NETWORK_BITCOIN: u8 = 0;
/// `Network::Testnet`.
pub const CC_NETWORK_TESTNET: u8 = 1;
/// `Network::Signet`.
pub const CC_NETWORK_SIGNET: u8 = 2;
/// `Network::Regtest`.
pub const CC_NETWORK_REGTEST: u8 = 3;

/// Out-of-band detail for a non-`CC_OK` return.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CcErrorDetail {
    /// First numeric field of the underlying error; see each code.
    pub detail_a: u64,
    /// Second numeric field, where the error has one; otherwise zero.
    pub detail_b: u64,
    /// Length of core's message in bytes: the length **required**, which is
    /// larger than `message_cap` when the message was truncated. Never
    /// NUL-terminated.
    ///
    /// Read `min(message_cap, message_len)` bytes, or resize to `message_len`
    /// and call again. Do not read `message_len` bytes unconditionally. A short
    /// read may end mid-UTF-8-sequence, so decode leniently or resize.
    pub message_len: usize,
}

/// ABI revision of this library.
#[no_mangle]
pub extern "C" fn coincube_keychain_ffi_abi_version() -> i32 {
    ABI_VERSION
}

/// Length of the digest every digest entry writes, for callers that would
/// otherwise hard-code 32.
#[no_mangle]
pub extern "C" fn coincube_keychain_ffi_digest_len() -> usize {
    CC_DIGEST_LEN
}

// ---------------------------------------------------------------------------
// Entry 1: raw fields, mirroring `coincube_core::unified_sighash::unified_sighash`
// ---------------------------------------------------------------------------

/// Compute a unified signature hash from raw fields.
///
/// A field-for-field mirror of
/// [`coincube_core::unified_sighash::unified_sighash`]. `spent_outputs` is a
/// consensus-encoded `TxOut` vector (CompactSize count followed by each
/// output), holding the output spent by every input of `tx`, in input order.
/// `script_code` may be empty.
///
/// On `CC_OK`, exactly [`CC_DIGEST_LEN`] bytes are written to `digest_out`.
///
/// # Safety
///
/// `tx`, `spent_outputs` and `script_code` must each either have a zero length
/// or point to that many readable bytes. `digest_out` must point to
/// `digest_out_len` writable bytes. `error_out`, when non-null, must point to a
/// writable [`CcErrorDetail`]. `message_out`, when non-null, must point to
/// `message_cap` writable bytes. No pointer is retained after return.
#[no_mangle]
pub unsafe extern "C" fn coincube_unified_sighash_digest(
    tx: *const u8,
    tx_len: usize,
    input_index: u32,
    hash_type: u8,
    script_type: u8,
    spent_outputs: *const u8,
    spent_outputs_len: usize,
    script_code: *const u8,
    script_code_len: usize,
    digest_out: *mut u8,
    digest_out_len: usize,
    error_out: *mut CcErrorDetail,
    message_out: *mut u8,
    message_cap: usize,
) -> i32 {
    guard(error_out, message_out, message_cap, || {
        let tx_bytes = match borrow(tx, tx_len) {
            Some(bytes) => bytes,
            None => return Err(Failure::code(CC_ERR_NULL_ARGUMENT)),
        };
        let prevout_bytes = match borrow(spent_outputs, spent_outputs_len) {
            Some(bytes) => bytes,
            None => return Err(Failure::code(CC_ERR_NULL_ARGUMENT)),
        };
        let script_bytes = match borrow(script_code, script_code_len) {
            Some(bytes) => bytes,
            None => return Err(Failure::code(CC_ERR_NULL_ARGUMENT)),
        };
        if digest_out.is_null() || digest_out_len < CC_DIGEST_LEN {
            return Err(Failure::code(CC_ERR_NULL_ARGUMENT));
        }

        let transaction: Transaction = deserialize(tx_bytes)
            .map_err(|err| Failure::with_message(CC_ERR_INVALID_TRANSACTION, err.to_string()))?;
        let prevouts: Vec<TxOut> = deserialize(prevout_bytes)
            .map_err(|err| Failure::with_message(CC_ERR_INVALID_SPENT_OUTPUTS, err.to_string()))?;
        let code = Script::from_bytes(script_bytes);

        let digest = unified_sighash(
            &transaction,
            input_index as usize,
            hash_type,
            script_type,
            &prevouts,
            code,
        )
        .map_err(sighash_failure)?;

        // Safety: checked non-null and long enough above.
        slice::from_raw_parts_mut(digest_out, CC_DIGEST_LEN).copy_from_slice(&digest);
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Entry 2: the production, PSBT-shaped surface
// ---------------------------------------------------------------------------

/// Unified signature hash for one native-P2WSH input of a standard PSBT.
///
/// `psbt` is standard BIP174 bytes as they travel over Connect. The PSBT and
/// every one of its inputs are validated by
/// [`coincube_core::unified_signing::verify_p2wsh_all_unified`] first, so the
/// P2WSH gates and any unified signature already present are core's refusal,
/// not a second copy of it here. The digest is then taken at
/// `SIGHASH_ALL | SIGHASH_UNIFIED` over SegWit-v0, which is the only thing the
/// unified signing path can express.
///
/// Reconciling a PSBT's own `PSBT_IN_SIGHASH_TYPE` request against the chain's
/// rule is Lane B3.2's; this entry computes the message the Vault signs.
///
/// # Safety
///
/// As [`coincube_unified_sighash_digest`]: `psbt` must have a zero length or
/// point to that many readable bytes, `digest_out` must point to
/// `digest_out_len` writable bytes, and the two optional out-parameters must
/// either be null or point to writable storage of the stated size.
#[no_mangle]
pub unsafe extern "C" fn coincube_unified_psbt_digest(
    psbt: *const u8,
    psbt_len: usize,
    input_index: u32,
    digest_out: *mut u8,
    digest_out_len: usize,
    error_out: *mut CcErrorDetail,
    message_out: *mut u8,
    message_cap: usize,
) -> i32 {
    guard(error_out, message_out, message_cap, || {
        let bytes = match borrow(psbt, psbt_len) {
            Some(bytes) => bytes,
            None => return Err(Failure::code(CC_ERR_NULL_ARGUMENT)),
        };
        if digest_out.is_null() || digest_out_len < CC_DIGEST_LEN {
            return Err(Failure::code(CC_ERR_NULL_ARGUMENT));
        }

        let unified = import_standard(bytes)
            .map_err(|err| Failure::with_message(CC_ERR_INVALID_PSBT, err.to_string()))?;

        // Core owns the refusal. This runs `validate_inputs` over every input —
        // authenticated prevout, native P2WSH, no Taproot signature data,
        // witness script present and committed to, sane Segwitv0 miniscript —
        // and verifies any unified signature already carried.
        let secp = secp256k1::Secp256k1::verification_only();
        verify_p2wsh_all_unified(&unified, &secp)
            .map_err(|err| Failure::with_message(CC_ERR_PSBT_VALIDATION, err.to_string()))?;

        let index = input_index as usize;
        let inputs = &unified.psbt().inputs;
        let input = inputs.get(index).ok_or_else(|| {
            Failure::detailed(
                CC_ERR_INPUT_INDEX_OUT_OF_BOUNDS,
                index as u64,
                inputs.len() as u64,
            )
        })?;
        // `verify_p2wsh_all_unified` above established both of these for every
        // input; the `expect`-free unwraps keep that fact local and explicit.
        let witness_script = input.witness_script.as_ref().ok_or_else(|| {
            Failure::with_message(
                CC_ERR_PSBT_VALIDATION,
                format!("input {index} is missing its witness script"),
            )
        })?;
        let spent_outputs = spent_outputs_of(&unified)?;

        let digest = UnifiedSighashCache::new(&unified.psbt().unsigned_tx, &spent_outputs)
            .and_then(|cache| {
                cache.signature_hash(
                    index,
                    UNIFIED_SIGHASH_ALL,
                    SCRIPT_TYPE_WITNESS_V0,
                    witness_script,
                )
            })
            .map_err(sighash_failure)?;

        slice::from_raw_parts_mut(digest_out, CC_DIGEST_LEN).copy_from_slice(&digest);
        Ok(())
    })
}

/// Sign every input of a standard PSBT whose BIP32 derivation matches `mnemonic`.
///
/// This is [`coincube_core::unified_signing::sign_p2wsh_all_unified`] verbatim:
/// core validates every input, derives each candidate key from the PSBT's own
/// `bip32_derivation` and refuses when the derived key does not match the key
/// the PSBT claims, signs at `0x21`, sets `PSBT_IN_SIGHASH_TYPE` on only the
/// inputs it signed, and verifies the result before returning it.
///
/// The signer arrives as a BIP39 phrase because that is what
/// [`MasterSigner`] is rooted in and because per-input derivation is part of
/// core's safety contract — a bare private key cannot drive it. The phrase is
/// borrowed for the length of the call and the signer scrubs both secrets it
/// holds on drop.
///
/// Output is standard BIP174 bytes. Pass `psbt_out_cap` of zero to learn the
/// required length from `CC_ERR_BUFFER_TOO_SMALL`'s `detail_a`.
///
/// # Safety
///
/// `psbt` and `mnemonic` must each have a zero length or point to that many
/// readable bytes. `psbt_out` must point to `psbt_out_cap` writable bytes, or
/// be null when `psbt_out_cap` is zero. `psbt_out_len` must be null or point to
/// a writable `usize`. The two optional out-parameters follow the same rule as
/// on [`coincube_unified_sighash_digest`].
#[no_mangle]
pub unsafe extern "C" fn coincube_unified_psbt_sign(
    psbt: *const u8,
    psbt_len: usize,
    mnemonic: *const u8,
    mnemonic_len: usize,
    network: u8,
    psbt_out: *mut u8,
    psbt_out_cap: usize,
    psbt_out_len: *mut usize,
    error_out: *mut CcErrorDetail,
    message_out: *mut u8,
    message_cap: usize,
) -> i32 {
    guard(error_out, message_out, message_cap, || {
        let bytes = match borrow(psbt, psbt_len) {
            Some(bytes) => bytes,
            None => return Err(Failure::code(CC_ERR_NULL_ARGUMENT)),
        };
        let phrase_bytes = match borrow(mnemonic, mnemonic_len) {
            Some(bytes) => bytes,
            None => return Err(Failure::code(CC_ERR_NULL_ARGUMENT)),
        };
        let network = network_from_code(network)?;
        let phrase = std::str::from_utf8(phrase_bytes)
            .map_err(|err| Failure::with_message(CC_ERR_INVALID_UTF8, err.to_string()))?;
        let parsed = bip39::Mnemonic::parse_normalized(phrase)
            .map_err(|err| Failure::with_message(CC_ERR_INVALID_MNEMONIC, err.to_string()))?;
        let signer = MasterSigner::from_mnemonic(network, parsed)
            .map_err(|err| Failure::with_message(CC_ERR_INVALID_MNEMONIC, err.to_string()))?;

        let unified = import_standard(bytes)
            .map_err(|err| Failure::with_message(CC_ERR_INVALID_PSBT, err.to_string()))?;
        let secp = secp256k1::Secp256k1::new();
        let signed = sign_p2wsh_all_unified(&signer, &unified, &secp)
            .map_err(|err| Failure::with_message(CC_ERR_SIGNING, err.to_string()))?;
        let exported = export_standard(&signed)
            .map_err(|err| Failure::with_message(CC_ERR_EXPORT, err.to_string()))?;

        if !psbt_out_len.is_null() {
            *psbt_out_len = exported.len();
        }
        if psbt_out_cap < exported.len() {
            return Err(Failure::detailed(
                CC_ERR_BUFFER_TOO_SMALL,
                exported.len() as u64,
                0,
            ));
        }
        if psbt_out.is_null() {
            return Err(Failure::code(CC_ERR_NULL_ARGUMENT));
        }
        slice::from_raw_parts_mut(psbt_out, exported.len()).copy_from_slice(&exported);
        Ok(())
    })
}

/// Verify every unified signature a standard PSBT carries.
///
/// [`coincube_core::unified_signing::verify_p2wsh_all_unified`] verbatim.
/// `verified_out` receives the number of signatures verified. Zero means the
/// PSBT and its P2WSH inputs validated but carried no unified signature — it
/// does **not** mean the PSBT is sufficiently signed or finalizable.
///
/// # Safety
///
/// `psbt` must have a zero length or point to that many readable bytes.
/// `verified_out` must be null or point to a writable `usize`. The two optional
/// out-parameters follow the same rule as on
/// [`coincube_unified_sighash_digest`].
#[no_mangle]
pub unsafe extern "C" fn coincube_unified_psbt_verify(
    psbt: *const u8,
    psbt_len: usize,
    verified_out: *mut usize,
    error_out: *mut CcErrorDetail,
    message_out: *mut u8,
    message_cap: usize,
) -> i32 {
    guard(error_out, message_out, message_cap, || {
        let bytes = match borrow(psbt, psbt_len) {
            Some(bytes) => bytes,
            None => return Err(Failure::code(CC_ERR_NULL_ARGUMENT)),
        };
        let unified = import_standard(bytes)
            .map_err(|err| Failure::with_message(CC_ERR_INVALID_PSBT, err.to_string()))?;
        let secp = secp256k1::Secp256k1::verification_only();
        let verified = verify_p2wsh_all_unified(&unified, &secp)
            .map_err(|err| Failure::with_message(CC_ERR_PSBT_VALIDATION, err.to_string()))?;
        if !verified_out.is_null() {
            *verified_out = verified;
        }
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// A non-`CC_OK` outcome on its way back to the caller.
struct Failure {
    code: i32,
    detail_a: u64,
    detail_b: u64,
    message: Option<String>,
}

impl Failure {
    fn code(code: i32) -> Self {
        Self {
            code,
            detail_a: 0,
            detail_b: 0,
            message: None,
        }
    }

    fn detailed(code: i32, detail_a: u64, detail_b: u64) -> Self {
        Self {
            code,
            detail_a,
            detail_b,
            message: None,
        }
    }

    fn with_message(code: i32, message: String) -> Self {
        Self {
            code,
            detail_a: 0,
            detail_b: 0,
            message: Some(message),
        }
    }
}

/// Map a `UnifiedSighashError` onto its code and numeric fields.
///
/// The match is exhaustive with no wildcard arm on purpose: a new variant in
/// core must break this build rather than silently arrive as a neighbouring
/// code on the phone.
fn sighash_failure(error: UnifiedSighashError) -> Failure {
    let message = error.to_string();
    let (code, detail_a, detail_b) = match error {
        UnifiedSighashError::MissingUnifiedFlag(hash_type) => {
            (CC_ERR_MISSING_UNIFIED_FLAG, u64::from(hash_type), 0)
        }
        UnifiedSighashError::UnsupportedScriptType(script_type) => {
            (CC_ERR_UNSUPPORTED_SCRIPT_TYPE, u64::from(script_type), 0)
        }
        UnifiedSighashError::PrevoutsLengthMismatch { inputs, prevouts } => (
            CC_ERR_PREVOUTS_LENGTH_MISMATCH,
            inputs as u64,
            prevouts as u64,
        ),
        UnifiedSighashError::InputIndexOutOfBounds { index, inputs } => (
            CC_ERR_INPUT_INDEX_OUT_OF_BOUNDS,
            index as u64,
            inputs as u64,
        ),
        UnifiedSighashError::InputIndexTooLarge(index) => {
            (CC_ERR_INPUT_INDEX_TOO_LARGE, index as u64, 0)
        }
        UnifiedSighashError::MissingSingleOutput { index, outputs } => {
            (CC_ERR_MISSING_SINGLE_OUTPUT, index as u64, outputs as u64)
        }
    };
    Failure {
        code,
        detail_a,
        detail_b,
        message: Some(message),
    }
}

fn network_from_code(code: u8) -> Result<Network, Failure> {
    match code {
        CC_NETWORK_BITCOIN => Ok(Network::Bitcoin),
        CC_NETWORK_TESTNET => Ok(Network::Testnet),
        CC_NETWORK_SIGNET => Ok(Network::Signet),
        CC_NETWORK_REGTEST => Ok(Network::Regtest),
        other => Err(Failure::detailed(
            CC_ERR_UNKNOWN_NETWORK,
            u64::from(other),
            0,
        )),
    }
}

/// The output spent by every input, in input order.
///
/// This calls core's own [`authenticate_previous_output`] rather than reading
/// `witness_utxo` directly, because the two are not interchangeable and the
/// difference is easy to get backwards: core **requires** the full
/// `non_witness_utxo` (absent it, `InputAuthError::MissingPreviousTransaction`)
/// and treats `witness_utxo` as an optional cross-check that must agree with it.
/// Reading `witness_utxo` here would refuse a PSBT that `unified_signing` had
/// just accepted, and would also skip the txid, vout and amount checks that make
/// the prevout *authenticated* rather than merely asserted.
fn spent_outputs_of(psbt: &UnifiedPsbt) -> Result<Vec<TxOut>, Failure> {
    let inner = psbt.psbt();
    inner
        .unsigned_tx
        .input
        .iter()
        .zip(&inner.inputs)
        .enumerate()
        .map(|(index, (txin, input))| {
            authenticate_previous_output(
                &txin.previous_output,
                input.non_witness_utxo.as_ref(),
                input.witness_utxo.as_ref(),
            )
            .map_err(|reason| {
                Failure::with_message(
                    CC_ERR_PSBT_VALIDATION,
                    format!("input {index} previous output is not authenticated: {reason}"),
                )
            })
        })
        .collect()
}

/// Borrow `len` bytes at `ptr`.
///
/// A zero length yields an empty slice whatever the pointer is — an empty
/// script code is legitimate, and `from_raw_parts` on a null pointer is
/// undefined behaviour even for a zero length.
///
/// # Safety
///
/// When `len` is non-zero, `ptr` must point to `len` readable bytes that stay
/// valid for `'a`.
unsafe fn borrow<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if len == 0 {
        Some(&[])
    } else if ptr.is_null() {
        None
    } else {
        Some(slice::from_raw_parts(ptr, len))
    }
}

/// Run an entry body, convert its outcome to a status code, and stop a panic at
/// the boundary.
///
/// Unwinding out of `extern "C"` aborts the process. On a phone that is a crash
/// of the host app in the middle of a signing flow, which is strictly worse for
/// the user than a typed failure they can retry.
///
/// This only helps when the library is built with `panic = "unwind"`. Under
/// `panic = "abort"` — which the workspace's `minimal` profile sets — the
/// process is gone before [`catch_unwind`](std::panic::catch_unwind) returns.
/// See [`CC_ERR_PANIC`].
///
/// # Safety
///
/// `error_out` must be null or point to a writable [`CcErrorDetail`], and
/// `message_out` must be null or point to `message_cap` writable bytes.
unsafe fn guard(
    error_out: *mut CcErrorDetail,
    message_out: *mut u8,
    message_cap: usize,
    body: impl FnOnce() -> Result<(), Failure>,
) -> i32 {
    if !error_out.is_null() {
        *error_out = CcErrorDetail::default();
    }
    match panic::catch_unwind(panic::AssertUnwindSafe(body)) {
        Ok(Ok(())) => CC_OK,
        Ok(Err(failure)) => {
            report(&failure, error_out, message_out, message_cap);
            failure.code
        }
        Err(_) => {
            report(
                &Failure::code(CC_ERR_PANIC),
                error_out,
                message_out,
                message_cap,
            );
            CC_ERR_PANIC
        }
    }
}

/// Write a failure's details and message into the caller's storage.
///
/// # Safety
///
/// As [`guard`].
unsafe fn report(
    failure: &Failure,
    error_out: *mut CcErrorDetail,
    message_out: *mut u8,
    message_cap: usize,
) {
    let message = failure.message.as_deref().unwrap_or_default();
    let written = if message_out.is_null() {
        0
    } else {
        let written = message.len().min(message_cap);
        slice::from_raw_parts_mut(message_out, written)
            .copy_from_slice(&message.as_bytes()[..written]);
        written
    };
    if !error_out.is_null() {
        *error_out = CcErrorDetail {
            detail_a: failure.detail_a,
            detail_b: failure.detail_b,
            // The length *required*, so a caller that got a truncated message
            // can size a buffer and ask again. It therefore exceeds
            // `message_cap` on truncation, which is why the documented read rule
            // is `min(message_cap, message_len)` and not `message_len`.
            message_len: if written < message.len() {
                message.len()
            } else {
                written
            },
        };
    }
}
