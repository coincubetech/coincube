# BTCB2 unified sighash foundation

The pure implementation in `coincube-core::unified_sighash` follows Bitcoin
Knots tag `v29.4.1.knots20260508`, commit
`8c85b1585dac23f964e2dd32045624de7f02aa58`. The primary references are
`doc/unified-sighash.md`, `src/script/interpreter.cpp`, and the unchanged test
fixture recorded in `coincube-core/tests/data/README.md`.

This slice supports script type 0 (bare/P2SH) and script type 1 (SegWit v0).
Taproot key-path and tapscript remain unsupported. The module computes digests
only; it does not choose signing policy, add signatures, or change Bitcoin
signing behavior.

## PSBT follow-up

PSBT input key type `0x03` (`PSBT_IN_SIGHASH_TYPE`) stores the hash type as a
four-byte little-endian unsigned integer. `ALL|UNIFIED` is therefore value
`0x00000021`, encoded as value bytes `21 00 00 00`. Including the one-byte key
and CompactSize lengths, the complete key-value pair is
`01 03 04 21 00 00 00`.

The current `bitcoin` 0.32 API can preserve this value without a fork using
`PsbtSighashType::from_u32(0x21)` and `to_u32()`. Later signing integration must
avoid `ecdsa_hash_ty()`, which deliberately rejects non-standard ECDSA values,
and must reconcile the declared PSBT type with the effective signature byte as
specified upstream. No dependency change is needed for raw representation in
this slice.
