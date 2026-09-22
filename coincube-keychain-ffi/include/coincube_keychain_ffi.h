/*
 * coincube_keychain_ffi.h — C ABI over coincube-core's unified signature
 * hashing and native-P2WSH unified signing.
 *
 * Hand-written to match `src/lib.rs`. Kept deliberately small: this is the
 * whole surface Keychain links against, and at a consensus-critical boundary a
 * header a reviewer can read end to end is worth more than one a generator
 * emitted. `tests/header_matches_abi.rs` pins the two together.
 *
 * Contract, in one place:
 *
 *   - Every entry returns CC_OK or a CC_ERR_* code. Nothing throws and nothing
 *     unwinds into C: a Rust panic is caught at the boundary and returned as
 *     CC_ERR_PANIC. That holds only for a build with panic = "unwind" (the
 *     default for the dev and release profiles); the workspace's "minimal"
 *     profile sets panic = "abort", under which a panic ends the process
 *     instead. Build this crate with unwind for the phone.
 *   - Input buffers are (pointer, length). A zero length is accepted with any
 *     pointer, including NULL.
 *   - Nothing is allocated on the Rust side and handed back, so there is no
 *     free function. Variable-length output goes into a caller-supplied buffer;
 *     pass a zero capacity first to learn the length from
 *     CC_ERR_BUFFER_TOO_SMALL's detail_a.
 *   - error_out and message_out are optional; pass NULL to ignore them. The
 *     message is core's own text, UTF-8, NOT NUL-terminated — read exactly
 *     CcErrorDetail.message_len bytes.
 *
 * What this boundary does NOT do: it applies no spend policy. It does not
 * refuse SIGHASH_ANYONECANPAY — 70 of the 142 supported upstream vectors set
 * 0x80 and core computes them; Keychain's refusal is a policy gate on the
 * signing path (Lane B3.2), not a property of the message. It also does not
 * decide which chain it is on; `network` selects key encodings only.
 */

#ifndef COINCUBE_KEYCHAIN_FFI_H
#define COINCUBE_KEYCHAIN_FFI_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bytes written by every digest entry. */
#define CC_DIGEST_LEN 32

/* Success. */
#define CC_OK 0

/* Boundary failures (10-19): this crate's own, not core's. */
#define CC_ERR_NULL_ARGUMENT        10  /* required pointer NULL, or output buffer too short */
#define CC_ERR_BUFFER_TOO_SMALL     11  /* detail_a = bytes required */
#define CC_ERR_INVALID_TRANSACTION  12
#define CC_ERR_INVALID_SPENT_OUTPUTS 13
#define CC_ERR_INVALID_UTF8         14
#define CC_ERR_UNKNOWN_NETWORK      15  /* detail_a = the value supplied */
#define CC_ERR_PANIC                16

/* unified_sighash refusals (20-25): one per UnifiedSighashError variant. */
#define CC_ERR_MISSING_UNIFIED_FLAG      20  /* detail_a = hash type */
#define CC_ERR_UNSUPPORTED_SCRIPT_TYPE   21  /* detail_a = script type */
#define CC_ERR_PREVOUTS_LENGTH_MISMATCH  22  /* detail_a = inputs, detail_b = prevouts */
#define CC_ERR_INPUT_INDEX_OUT_OF_BOUNDS 23  /* detail_a = index,  detail_b = inputs */
#define CC_ERR_INPUT_INDEX_TOO_LARGE     24  /* detail_a = index */
#define CC_ERR_MISSING_SINGLE_OUTPUT     25  /* detail_a = index,  detail_b = outputs */

/* PSBT and signing refusals (30-34). These carry core's text in message_out. */
#define CC_ERR_INVALID_PSBT      30
#define CC_ERR_PSBT_VALIDATION   31
#define CC_ERR_INVALID_MNEMONIC  32
#define CC_ERR_SIGNING           33
#define CC_ERR_EXPORT            34

/* Key and address encodings. Bitcoin Blake2b uses CC_NETWORK_BITCOIN. */
#define CC_NETWORK_BITCOIN 0
#define CC_NETWORK_TESTNET 1
#define CC_NETWORK_SIGNET  2
#define CC_NETWORK_REGTEST 3

/* Out-of-band detail for a non-CC_OK return. Zeroed on entry to every call. */
typedef struct CcErrorDetail {
  uint64_t detail_a;
  uint64_t detail_b;
  /* UTF-8 bytes written to message_out, or the length required when the
   * supplied capacity was too small. Never NUL-terminated. */
  size_t message_len;
} CcErrorDetail;

/* ABI revision of this library. Bumped on any change to a signature or to a
 * code's meaning. */
int32_t coincube_keychain_ffi_abi_version(void);

/* CC_DIGEST_LEN, for callers that would otherwise hard-code 32. */
size_t coincube_keychain_ffi_digest_len(void);

/*
 * Entry 1 — raw fields. A field-for-field mirror of
 * coincube_core::unified_sighash::unified_sighash.
 *
 * spent_outputs is a consensus-encoded TxOut vector (CompactSize count then
 * each output), holding the output spent by every input of tx, in input order.
 * script_code may be empty. On CC_OK exactly CC_DIGEST_LEN bytes are written
 * to digest_out.
 *
 * This is the entry the 142-vector known-answer test drives: the upstream
 * corpus cannot be driven through the PSBT entry below, because none of its
 * spent outputs is a witness program.
 */
int32_t coincube_unified_sighash_digest(
    const uint8_t *tx, size_t tx_len,
    uint32_t input_index,
    uint8_t hash_type,
    uint8_t script_type,
    const uint8_t *spent_outputs, size_t spent_outputs_len,
    const uint8_t *script_code, size_t script_code_len,
    uint8_t *digest_out, size_t digest_out_len,
    CcErrorDetail *error_out,
    uint8_t *message_out, size_t message_cap);

/*
 * Entry 2a — the production digest. Unified signature hash for one native-P2WSH
 * input of a standard BIP174 PSBT, at SIGHASH_ALL|SIGHASH_UNIFIED over
 * SegWit v0, which is the only hash type the unified signing path expresses.
 *
 * The PSBT and all of its inputs are validated by core's
 * verify_p2wsh_all_unified first, so the P2WSH gates are core's refusal rather
 * than a second copy of them here.
 */
int32_t coincube_unified_psbt_digest(
    const uint8_t *psbt, size_t psbt_len,
    uint32_t input_index,
    uint8_t *digest_out, size_t digest_out_len,
    CcErrorDetail *error_out,
    uint8_t *message_out, size_t message_cap);

/*
 * Entry 2b — sign. coincube_core::unified_signing::sign_p2wsh_all_unified
 * verbatim: core validates every input, derives each candidate key from the
 * PSBT's own bip32_derivation and refuses when the derived key does not match
 * the key the PSBT claims, signs at 0x21, sets PSBT_IN_SIGHASH_TYPE on only the
 * inputs it signed, and verifies the result before returning it.
 *
 * The signer arrives as a BIP39 phrase (UTF-8, not NUL-terminated) because that
 * is what core's MasterSigner is rooted in and because per-input derivation is
 * part of its safety contract — a bare private key cannot drive it. The phrase
 * is borrowed for the call only.
 *
 * Output is standard BIP174 bytes. Pass psbt_out_cap = 0 with psbt_out = NULL
 * to learn the required length.
 */
int32_t coincube_unified_psbt_sign(
    const uint8_t *psbt, size_t psbt_len,
    const uint8_t *mnemonic, size_t mnemonic_len,
    uint8_t network,
    uint8_t *psbt_out, size_t psbt_out_cap, size_t *psbt_out_len,
    CcErrorDetail *error_out,
    uint8_t *message_out, size_t message_cap);

/*
 * Entry 2c — verify. coincube_core::unified_signing::verify_p2wsh_all_unified
 * verbatim. verified_out receives the number of unified signatures verified.
 * Zero means the PSBT and its P2WSH inputs validated but carried no unified
 * signature; it does NOT mean the PSBT is sufficiently signed or finalizable.
 */
int32_t coincube_unified_psbt_verify(
    const uint8_t *psbt, size_t psbt_len,
    size_t *verified_out,
    CcErrorDetail *error_out,
    uint8_t *message_out, size_t message_cap);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* COINCUBE_KEYCHAIN_FFI_H */
