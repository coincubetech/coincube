# coincube-keychain-ffi

A C ABI over `coincube-core`'s unified signature hashing and native-P2WSH
unified signing, so the Keychain phone signer can reach them instead of
reimplementing them.

This crate contains **no cryptography**. Every digest comes out of
`coincube_core::unified_sighash`, every signature out of
`coincube_core::unified_signing`. That is the point: the consensus-critical
message has one implementation (launch-GA master decision 6). Its only
dependency is `coincube-core`, and everything it needs from the
bitcoin/miniscript/bip39 stack arrives through core's own re-exports, so the two
halves of the boundary cannot be built against different versions of the
consensus types.

## Surface

`include/coincube_keychain_ffi.h` is the contract and is hand-written; read it
first. Four entries:

| Entry | What it is |
|---|---|
| `coincube_unified_sighash_digest` | Raw fields. Mirrors `unified_sighash::unified_sighash` field for field. |
| `coincube_unified_psbt_digest` | The production digest: one native-P2WSH input of a standard PSBT, at `SIGHASH_ALL\|SIGHASH_UNIFIED`. |
| `coincube_unified_psbt_sign` | `unified_signing::sign_p2wsh_all_unified` verbatim. |
| `coincube_unified_psbt_verify` | `unified_signing::verify_p2wsh_all_unified` verbatim. |

## Why there are two digest entries

The raw-fields entry is not a convenience. The upstream unified-sighash vector
corpus **cannot** be driven through a PSBT-shaped entry point, so without it
there would be no way to prove the boundary against those vectors at all.

The corpus's spent outputs are synthetic scripts (`51`, `5151`, `515151`, …),
not witness programs. Across the 142 supported rows, 0 of the 303 spent outputs
is a v0 witness program, and for 0 of the 66 witness-v0 rows does
`spentOutputs[inIdx].scriptPubKey` equal `P2WSH(scriptCode)`.
`unified_signing::validate_inputs` rejects on each of those independently, and
upstream of all of them the signing path can express only `0x21` while the
corpus spans many hash types — so even a corpus with real P2WSH prevouts could
not be driven through it.

So the two entries are tested differently and then pinned to each other:

- `tests/unified_sighash_kat.rs` drives the **raw-fields** entry over all 142
  supported vectors and asserts the 24 out-of-scope rows return the typed
  `CC_ERR_UNSUPPORTED_SCRIPT_TYPE`.
- `tests/psbt_entry.rs` drives the **PSBT** entry over genuine Vault P2WSH
  fixtures with core-derived expected digests, and — in
  `psbt_entry_agrees_with_raw_fields_entry` — asserts the two entries produce
  the same message for the same input. That is what carries the corpus's
  authority onto the entry production actually calls.

## Two things that would silently destroy the KAT

Both are guarded by assertions that fail loudly, and both were verified to fail
by deliberately introducing them:

1. **Do not refuse `ANYONECANPAY` at the digest layer.** 70 of the 142 supported
   vectors set `0x80` and core computes them. Keychain's ANYONECANPAY refusal is
   a spend-policy gate on the *signing* path (Lane B3.2), not a property of the
   message. Pushed down here it would make those 70 rows unreachable while the
   suite still looked green on the other 72 — so the KAT asserts the count is
   exactly 70.
2. **All 142 carry the `0x20` unified flag.** `CC_ERR_MISSING_UNIFIED_FLAG`
   appearing in the KAT is a field-plumbing bug at this boundary, not a vector
   problem. The failure message says so.

The corpus is reached with `include_str!` at the in-repo path. A byte-identical
copy exists outside the repository; a test reading *that* path passes locally
and fails in CI.

## Panic behaviour, which depends on the profile

Every entry catches a panic and returns `CC_ERR_PANIC` rather than unwinding
into C. That only holds for a build with `panic = "unwind"` — the default for
`dev` and `release`. The workspace's `minimal` profile sets `panic = "abort"`,
under which a panic ends the host process before the boundary can report
anything. Keychain should build this crate with unwind: a bug here should be a
failed signing attempt the user can retry, not an app that disappears mid-flow.

## Measured cost

Host is `aarch64-apple-darwin`, Homebrew rustc 1.94.0. Mobile targets are not
built here, so these are host figures, not shipping figures.

| | `release` | `minimal` (`opt-level=z`, thin LTO, strip) |
|---|---|---|
| `libcoincube_keychain_ffi.dylib` | 2,547,392 B (2.43 MiB); 2,336,152 B stripped | 1,992,656 B (1.90 MiB) |
| `libcoincube_keychain_ffi.a` | 20,636,640 B | 33,996,664 B (thin-LTO bitcode) |
| cold build, whole dep graph | 16.9 s | 17.5 s |

The `.a` is not a shipping size — the iOS linker dead-strips it into the app
binary; the dylib is the closer proxy for what Android packages per ABI. Six
symbols are exported and nothing else:

```
coincube_keychain_ffi_abi_version   coincube_unified_psbt_digest
coincube_keychain_ffi_digest_len    coincube_unified_psbt_sign
coincube_unified_sighash_digest     coincube_unified_psbt_verify
```

Marginal cost added to the root `cargo test` that CI runs: **1.0 s** to compile
this crate and its four test binaries with every shared dependency already
cached, plus **1.3 s** to run the 17 tests.

Most of the ~2 MB is `coincube-core`'s own graph — `secp256k1`, `miniscript`,
`bitcoin`, and also `argon2`/`aes-gcm`/`bip39`, which the phone does not need
here. Slimming that would mean feature-gating `coincube-core`, and this slice was
scoped not to touch core; it is a follow-up decision, not something to do
silently.

## Building for mobile

Not done here, and not proven by this crate's tests. `staticlib` is what iOS
links into its framework and `cdylib` is what Android packages per ABI in
`jniLibs`; wiring those into Keychain's lanes, and the Dart `dart:ffi` wrapper,
is Lane B3.1b. All three crate types export the same symbols, so the tests in
`tests/` exercise the shipped boundary.

## Tests

```sh
cargo test -p coincube-keychain-ffi         # 17 tests across four binaries
cargo fmt -- --check
cargo clippy -p coincube-keychain-ffi --all-targets -- -D warnings
```

The crate is in both `members` and `default-members` in the root `Cargo.toml`.
That is deliberate: CI runs a bare `cargo test`, which builds `default-members`,
so a crate listed only in `members` would have its KAT skipped in CI — passing
locally and gating nothing.
