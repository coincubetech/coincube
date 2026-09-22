/*
 * abi_smoke.c — a real C consumer of coincube-keychain-ffi.
 *
 * Deliberately NOT part of `cargo test` or CI: it would add a C toolchain step
 * to a lane that is paying for its Actions minutes. It exists because the Rust
 * integration tests link the crate as an `rlib`, which exercises the functions
 * but not the *shipped* artifact — symbol resolution in the dylib/staticlib, the
 * `CcErrorDetail` struct layout as C lays it out, and whether the hand-written
 * header is even valid C. This file checks all three, and doubles as the
 * reference call shape for the Dart `dart:ffi` wrapper in Lane B3.1b.
 *
 * It reproduces upstream unified-sighash vector 1 (the first data row of
 * coincube-core/tests/data/unified_sighash.json: scriptType 0, hashType 0xa3,
 * which sets ANYONECANPAY) and then checks that a Taproot script type comes back
 * as the typed refusal with its script type attached.
 *
 * Build and run against a release dylib:
 *
 *   cargo build -p coincube-keychain-ffi --release
 *   clang -O1 -Icoincube-keychain-ffi/include \
 *       coincube-keychain-ffi/contrib/abi_smoke.c \
 *       -Ltarget/release -lcoincube_keychain_ffi -o /tmp/abi_smoke
 *   DYLD_LIBRARY_PATH=target/release /tmp/abi_smoke     # macOS
 *
 * Or against the staticlib, which is the shape iOS links:
 *
 *   clang -O1 -Icoincube-keychain-ffi/include \
 *       coincube-keychain-ffi/contrib/abi_smoke.c \
 *       target/release/libcoincube_keychain_ffi.a \
 *       -framework Security -framework CoreFoundation -o /tmp/abi_smoke_static
 *
 * Exits 0 and prints OK on success; prints FAIL and exits 1 otherwise.
 */

#include <stdio.h>
#include <string.h>

#include "coincube_keychain_ffi.h"

/* Vector 1: scriptCode. */
static const uint8_t script_code[] = {
    0x53, 0x53, 0x53, 0x53, 0x53,
};
#define SCRIPT_CODE_LEN 5

/* Vector 1: rawTx, consensus-encoded. */
static const uint8_t raw_tx[] = {
    0x23, 0x16, 0xde, 0x5a, 0x02, 0xa4, 0xfe, 0xdc, 0x0d, 0x9e, 0x90, 0x2f,
    0x93, 0xa4, 0x02, 0x32, 0x98, 0x69, 0xaf, 0xcf, 0x6b, 0x22, 0xc4, 0x44,
    0xf8, 0x00, 0x29, 0x74, 0x18, 0xb5, 0x4d, 0x2e, 0x00, 0x2c, 0x57, 0xeb,
    0x6a, 0xf3, 0xc4, 0x73, 0x0b, 0x00, 0x56, 0x90, 0x37, 0x96, 0x5a, 0xca,
    0xc7, 0xbf, 0xd7, 0x27, 0x58, 0xf6, 0x0d, 0xdb, 0xe8, 0xca, 0x7b, 0x1d,
    0xa2, 0x3a, 0x70, 0x98, 0x4f, 0x5e, 0x8b, 0x03, 0xbc, 0x44, 0x88, 0x91,
    0x90, 0xea, 0x56, 0x03, 0x4e, 0xc7, 0xbf, 0x25, 0x92, 0x57, 0x00, 0x3f,
    0x9f, 0x66, 0xfc, 0x01, 0xcb, 0xbc, 0x46, 0x1e, 0xa5, 0xac, 0x04, 0x00,
    0x02, 0x54, 0x54, 0x4e, 0x11, 0x29, 0x01,
};
#define RAW_TX_LEN 103

/* Vector 1: spentOutputs as a consensus-encoded TxOut vector — CompactSize
 * count, then each output as 8-byte little-endian value plus CompactSize-prefixed
 * script pubkey. This is the encoding the ABI expects, and the shape the Dart
 * side will have to build. */
static const uint8_t prevouts[] = {
    0x02, 0xc7, 0x52, 0xda, 0x32, 0x83, 0x4f, 0x02, 0x00, 0x06, 0x51, 0x51,
    0x51, 0x51, 0x51, 0x51, 0xb1, 0x0f, 0x43, 0x0b, 0x40, 0x1e, 0x03, 0x00,
    0x07, 0x52, 0x52, 0x52, 0x52, 0x52, 0x52, 0x52,
};
#define PREVOUTS_LEN 32

#define IN_IDX      0
#define HASH_TYPE   0xa3   /* SIGHASH_UNIFIED | ANYONECANPAY | SINGLE */
#define SCRIPT_TYPE 0      /* bare / P2SH */
#define EXPECTED    "4a84224afd272deeaa13972fb03ea70c738d78e50b53a63af3b3a9decfb548f5"

int main(void) {
    printf("abi_version=%d digest_len=%zu\n",
           coincube_keychain_ffi_abi_version(),
           coincube_keychain_ffi_digest_len());

    uint8_t digest[CC_DIGEST_LEN];
    CcErrorDetail err;
    uint8_t msg[256];
    memset(digest, 0, sizeof digest);

    int32_t rc = coincube_unified_sighash_digest(
        raw_tx, RAW_TX_LEN, IN_IDX, HASH_TYPE, SCRIPT_TYPE,
        prevouts, PREVOUTS_LEN, script_code, SCRIPT_CODE_LEN,
        digest, sizeof digest, &err, msg, sizeof msg);

    char hex[2 * CC_DIGEST_LEN + 1];
    for (int i = 0; i < CC_DIGEST_LEN; i++) sprintf(hex + 2 * i, "%02x", digest[i]);
    hex[2 * CC_DIGEST_LEN] = 0;
    printf("rc=%d digest=%s\n", rc, hex);

    if (rc != CC_OK) {
        printf("FAIL: rc=%d msg=%.*s\n", rc, (int)err.message_len, msg);
        return 1;
    }
    if (strcmp(hex, EXPECTED) != 0) {
        printf("FAIL: expected %s\n", EXPECTED);
        return 1;
    }

    /* A Taproot script type must come back as the typed refusal, carrying the
     * offending script type — from C as well as from Rust. */
    rc = coincube_unified_sighash_digest(
        raw_tx, RAW_TX_LEN, IN_IDX, HASH_TYPE, 2,
        prevouts, PREVOUTS_LEN, script_code, SCRIPT_CODE_LEN,
        digest, sizeof digest, &err, msg, sizeof msg);
    printf("script_type=2 -> rc=%d detail_a=%llu msg=%.*s\n",
           rc, (unsigned long long)err.detail_a, (int)err.message_len, msg);
    if (rc != CC_ERR_UNSUPPORTED_SCRIPT_TYPE || err.detail_a != 2) {
        printf("FAIL: wrong refusal\n");
        return 1;
    }

    printf("OK: vector 1 reproduced and the typed refusal observed, from C\n");
    return 0;
}
