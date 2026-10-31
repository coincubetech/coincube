"""Bitcoin Knots' unified signature hash, ported to Python and pinned to the
upstream vectors.

The port was written for the removed foreign-Split consensus module
(`test_btcb2_split_consensus.py`), which signed its `ALL|UNIFIED` (0x21)
sweep with it. It is kept on its own as an independent reference for the
Rust port (`coincube-core/src/unified_sighash.rs`) and for any later
two-chain test that signs with the unified sighash. No node is needed.
"""
import json
import os
import struct

from test_framework.serializations import (
    CTransaction,
    CTxOut,
    from_binary,
    ser_string,
    sha256,
)

# The unified sighash (`coincube-core/src/unified_sighash.rs`, Knots' draft):
# the opt-in bit, the legacy output and input modes it keeps, the two script
# types this port covers, and the tag of its tagged hash.
SIGHASH_UNIFIED = 0x20
SIGHASH_NONE = 2
SIGHASH_SINGLE = 3
SIGHASH_ANYONECANPAY = 0x80
SCRIPT_TYPE_BASE = 0
SCRIPT_TYPE_WITNESS_V0 = 1
UNIFIED_SIGHASH_TAG = sha256(b"UnifiedSighash")
# Bitcoin Knots' vectors, copied unchanged into the core crate's test data.
UNIFIED_VECTORS = os.path.join(
    os.path.dirname(__file__),
    "..",
    "coincube-core",
    "tests",
    "data",
    "unified_sighash.json",
)


def unified_sighash(tx, index, hash_type, script_type, spent_outputs, script_code):
    """Bitcoin Knots' draft unified signature hash for script types 0 (bare,
    P2SH) and 1 (SegWit v0), ported here independently of the Rust
    implementation (`coincube-core/src/unified_sighash.rs`), and pinned to the
    upstream vectors by `test_unified_sighash_port_offline`. `spent_outputs`
    is the output every input spends, in input order."""
    assert hash_type & SIGHASH_UNIFIED, hex(hash_type)
    assert script_type in (SCRIPT_TYPE_BASE, SCRIPT_TYPE_WITNESS_V0), script_type
    assert len(spent_outputs) == len(tx.vin), (len(spent_outputs), len(tx.vin))
    output_type = hash_type & 0x1F
    anyone_can_pay = bool(hash_type & SIGHASH_ANYONECANPAY)
    # Epoch, hash type, version, the locktime zero-extended to five bytes.
    message = bytes([0, hash_type]) + struct.pack("<i", tx.nVersion)
    message += struct.pack("<I", tx.nLockTime) + b"\x00"
    if not anyone_can_pay:
        message += sha256(b"".join(txin.prevout.serialize() for txin in tx.vin))
        message += sha256(
            b"".join(struct.pack("<q", out.nValue) for out in spent_outputs)
        )
        message += sha256(
            b"".join(ser_string(out.scriptPubKey) for out in spent_outputs)
        )
        message += sha256(
            b"".join(struct.pack("<I", txin.nSequence) for txin in tx.vin)
        )
    if output_type not in (SIGHASH_NONE, SIGHASH_SINGLE):
        message += sha256(b"".join(out.serialize() for out in tx.vout))
    message += bytes([script_type])
    if anyone_can_pay:
        message += tx.vin[index].prevout.serialize() + spent_outputs[index].serialize()
        message += struct.pack("<I", tx.vin[index].nSequence)
    else:
        message += struct.pack("<I", index)
    message += ser_string(script_code)
    if output_type == SIGHASH_SINGLE:
        message += sha256(tx.vout[index].serialize())
    return sha256(UNIFIED_SIGHASH_TAG + UNIFIED_SIGHASH_TAG + message)


def test_unified_sighash_port_offline():
    """This module's digest, byte for byte, on every upstream Knots vector of
    script types 0 and 1; the Taproot rows are out of scope, as in the Rust
    port's own test."""
    with open(UNIFIED_VECTORS) as vectors:
        rows = json.load(vectors)
    assert rows[0] == [
        "scriptCode",
        "rawTx",
        "inIdx",
        "hashType",
        "scriptType",
        "spentOutputs",
        "sighash",
    ]
    checked = skipped = 0
    for script_code, raw, index, hash_type, script_type, spent, expected in rows[1:]:
        if script_type > SCRIPT_TYPE_WITNESS_V0:
            skipped += 1
            continue
        tx = from_binary(CTransaction, bytes.fromhex(raw))
        outputs = [CTxOut(amount, bytes.fromhex(script)) for amount, script in spent]
        digest = unified_sighash(
            tx, index, hash_type, script_type, outputs, bytes.fromhex(script_code)
        )
        assert digest.hex() == expected, (checked + skipped + 1, expected)
        checked += 1
    assert (checked, skipped) == (142, 24)
