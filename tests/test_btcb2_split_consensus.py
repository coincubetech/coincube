"""Split steps 1 and 2 (#568 B6a, B6b) and the single-step unified fallback
(B4b-1a, B6-d) against both pinned nodes.

Five foreign (non-Cube) wallets, one per supported source shape, are funded
before the fork, so every coin is shared history on both chains. For each, the
production `foreign_split` code builds and finalizes step 1 and step 2
through the `split_regtest_vectors` bridge; this module supplies the chain
observations, the step-2 target and signs with disposable regtest keys. It
checks node consensus only: no GUI, journal, fee source, freshness proof,
preflight or target reservation.

A second wallet per shape, never split in two steps, takes the unified
fallback: one BTCB2 sweep of its shared coins signed `ALL|UNIFIED` (0x21),
accepted by the BTCB2 node and refused by the Bitcoin node for the
signature alone. This module signs it too, over Bitcoin Knots' unified
digest ported here and pinned to the upstream vectors, as the `coincube`
proprietary records the production finalizer reads. Owner decision P1
(block): every legacy signature on that route is refused, never classified.

The offline tests need no node. They fabricate the previous transactions and
run the same bridge and signer, so the signing and finalization paths for all
five shapes are exercised wherever the bridge is built.

The replay test pins, at node level, the window the post-submission reorg
design (#568, Section 2) must close: once step 1 leaves Bitcoin's active
chain, the step 2 BTCB2 mined is a valid Bitcoin transaction.
"""
import copy
import json
import os
import shutil
import struct
import subprocess
import tempfile
from decimal import Decimal

import pytest
from bip32 import BIP32
from bip32.utils import _pubkey_to_fingerprint, coincurve

from fixtures import *
from test_framework.authproxy import JSONRPCException
from test_framework.serializations import (
    PSBT,
    PSBT_IN_BIP32_DERIVATION,
    PSBT_IN_NON_WITNESS_UTXO,
    PSBT_IN_PARTIAL_SIG,
    PSBT_IN_PROPRIETARY,
    PSBT_IN_SIGHASH_TYPE,
    PSBT_IN_WITNESS_SCRIPT,
    PSBT_IN_WITNESS_UTXO,
    COutPoint,
    CTransaction,
    CTxIn,
    CTxOut,
    from_binary,
    hash160,
    hash256,
    ser_string,
    sha256,
    sighash_all_witness,
)

SHAPES = ("pkh", "sh_wpkh", "wpkh", "wsh_multi", "wsh_sortedmulti")
# Receive index of the step-1 destination; the funded coins use index 0.
DESTINATION_INDEX = 5
FEERATE_VB = 2
FUND_AMOUNTS = (Decimal("0.01"), Decimal("0.02"))
SIGHASH_ALL = 1
# The poison payload (`coincube-core/src/split_poison.rs`), parsed here
# independently of the Rust decoder: tag, version, chain byte (0 = Bitcoin
# mainnet), fork marker, outpoint commitment, zero padding.
POISON_TAG = b"COINCUBE-SPLIT"
POISON_VERSION = 1
POISON_CHAIN_BITCOIN = 0
# Step-2 target kind per shape: one P2TR, the rest P2WSH (the Vault types).
TARGET_KIND = {"wpkh": "p2tr"}
# The unified sighash (`coincube-core/src/unified_sighash.rs`, Knots' draft):
# the opt-in bit, the legacy output and input modes it keeps, the two script
# types this port covers, and the tag of its tagged hash.
SIGHASH_UNIFIED = 0x20
UNIFIED_SIGHASH_ALL = SIGHASH_ALL | SIGHASH_UNIFIED
SIGHASH_NONE = 2
SIGHASH_SINGLE = 3
SIGHASH_ANYONECANPAY = 0x80
SCRIPT_TYPE_BASE = 0
SCRIPT_TYPE_WITNESS_V0 = 1
UNIFIED_SIGHASH_TAG = sha256(b"UnifiedSighash")
# A unified signature's PSBT record (`coincube-core/src/psbt_unified.rs`):
# proprietary key data `<prefix "coincube"><subtype 0><public key>`, value
# the DER signature followed by 0x21.
UNIFIED_RECORD_PREFIX = ser_string(b"coincube") + bytes([0])
# Bitcoin Knots' vectors, copied unchanged into the core crate's test data.
UNIFIED_VECTORS = os.path.join(
    os.path.dirname(__file__),
    "..",
    "coincube-core",
    "tests",
    "data",
    "unified_sighash.json",
)


def split_tool():
    tool = os.getenv("SPLIT_REGTEST_TOOL_PATH")
    if not tool or not os.access(tool, os.X_OK):
        if os.getenv("BTCB2_HARNESS_REQUIRED") == "1":
            pytest.fail("SPLIT_REGTEST_TOOL_PATH must name the built Split test bridge")
        pytest.skip(
            "Build coincube-core example split_regtest_vectors and set SPLIT_REGTEST_TOOL_PATH"
        )
    return tool


def run_bridge(tool, request):
    result = subprocess.run(
        [tool], input=json.dumps(request), text=True, capture_output=True, timeout=30
    )
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


class ForeignWallet:
    """A disposable foreign wallet of one supported Split shape.

    Keys follow the harness convention: the origin fingerprint is the xpub's
    own, so a PSBT derivation path is `[branch, index]` from the master.
    """

    def __init__(self, shape):
        self.shape = shape
        count = 3 if shape.startswith("wsh_") else 1
        self.hds = [BIP32.from_seed(os.urandom(32), network="test") for _ in range(count)]

    def descriptor(self, branch):
        keys = [
            f"[{_pubkey_to_fingerprint(hd.pubkey).hex()}]{hd.get_xpub()}/{branch}/*"
            for hd in self.hds
        ]
        return {
            "pkh": "pkh({})",
            "sh_wpkh": "sh(wpkh({}))",
            "wpkh": "wpkh({})",
            "wsh_multi": "wsh(multi(2,{},{},{}))",
            "wsh_sortedmulti": "wsh(sortedmulti(2,{},{},{}))",
        }[self.shape].format(*keys)

    def pubkeys(self, branch, index):
        return [
            coincurve.PrivateKey(hd.get_privkey_from_path([branch, index])).public_key.format()
            for hd in self.hds
        ]

    def script_pubkey(self, branch, index):
        """Computed here, independently of the Rust construction and of Knots."""
        keys = self.pubkeys(branch, index)
        if self.shape == "pkh":
            return bytes([0x76, 0xA9, 0x14]) + hash160(keys[0]) + bytes([0x88, 0xAC])
        if self.shape == "wpkh":
            return bytes([0x00, 0x14]) + hash160(keys[0])
        if self.shape == "sh_wpkh":
            redeem = bytes([0x00, 0x14]) + hash160(keys[0])
            return bytes([0xA9, 0x14]) + hash160(redeem) + bytes([0x87])
        if self.shape == "wsh_sortedmulti":
            keys = sorted(keys)
        script = bytes([0x52]) + b"".join(bytes([0x21]) + k for k in keys)
        script += bytes([0x53, 0xAE])
        return bytes([0x00, 0x20]) + sha256(script)

    def signers(self):
        """Every key of a single-sig wallet; two of three for multisig."""
        return self.hds[:2]

    def signing_keys(self, psbt_in, signers=None):
        """`(pubkey, privkey)` for every BIP32 derivation of the input that
        one of `signers` (by default `signers()`) controls."""
        if signers is None:
            signers = self.signers()
        for pubkey, origin in psbt_in.map[PSBT_IN_BIP32_DERIVATION].items():
            raw_path = origin[4:]
            path = [
                int.from_bytes(raw_path[j : j + 4], "little")
                for j in range(0, len(raw_path), 4)
            ]
            for hd in signers:
                privkey = coincurve.PrivateKey(hd.get_privkey_from_path(path))
                if privkey.public_key.format() == pubkey:
                    yield pubkey, privkey

    def sign(self, psbt_b64, explicit_all=False, signers=None):
        """Add SIGHASH_ALL partial signatures, optionally with an explicit
        PSBT_IN_SIGHASH_TYPE of 0x01 on every input (#585 F1)."""
        psbt = PSBT.from_base64(psbt_b64)
        for i, psbt_in in enumerate(psbt.i):
            for pubkey, privkey in self.signing_keys(psbt_in, signers):
                digest = sighash_all(psbt, i, pubkey)
                signature = privkey.sign(digest, hasher=None) + bytes([SIGHASH_ALL])
                psbt_in.map.setdefault(PSBT_IN_PARTIAL_SIG, {})[pubkey] = signature
            if explicit_all:
                psbt_in.map[PSBT_IN_SIGHASH_TYPE] = struct.pack("<I", SIGHASH_ALL)
        return psbt.to_base64()

    def sign_unified(self, psbt_b64, request=UNIFIED_SIGHASH_ALL):
        """Add `ALL|UNIFIED` (0x21) signatures over the unified digest, as
        the records the production finalizer reads: no partial signature is
        written. Every input then requests `request`, 0x21 unless a test asks
        for the refusal of another value."""
        psbt = PSBT.from_base64(psbt_b64)
        spent = [spent_output(psbt, i) for i in range(len(psbt.i))]
        suffix = bytes([UNIFIED_SIGHASH_ALL])
        for i, psbt_in in enumerate(psbt.i):
            for pubkey, privkey in self.signing_keys(psbt_in):
                code, kind = unified_script_code(psbt_in, pubkey, spent[i])
                digest = unified_sighash(
                    psbt.tx, i, UNIFIED_SIGHASH_ALL, kind, spent, code
                )
                signature = privkey.sign(digest, hasher=None) + suffix
                records = psbt_in.map.setdefault(PSBT_IN_PROPRIETARY, {})
                records[UNIFIED_RECORD_PREFIX + pubkey] = signature
            psbt_in.map[PSBT_IN_SIGHASH_TYPE] = struct.pack("<I", request)
        return psbt.to_base64()


def sighash_all(psbt, i, pubkey):
    psbt_in = psbt.i[i].map
    if PSBT_IN_WITNESS_SCRIPT in psbt_in:
        return sighash_all_witness(psbt_in[PSBT_IN_WITNESS_SCRIPT], psbt, i)
    if PSBT_IN_WITNESS_UTXO in psbt_in:
        # P2WPKH, native or nested: BIP 143's scriptCode is the P2PKH script.
        code = bytes([0x76, 0xA9, 0x14]) + hash160(pubkey) + bytes([0x88, 0xAC])
        return sighash_all_witness(code, psbt, i)
    # Legacy P2PKH: the spent scriptPubKey replaces input i's empty scriptSig.
    previous = from_binary(CTransaction, psbt_in[PSBT_IN_NON_WITNESS_UTXO])
    tx = CTransaction(psbt.tx)
    for index, txin in enumerate(tx.vin):
        txin.scriptSig = b""
    tx.vin[i].scriptSig = previous.vout[tx.vin[i].prevout.n].scriptPubKey
    return hash256(tx.serialize_without_witness() + struct.pack("<I", SIGHASH_ALL))


def spent_output(psbt, i):
    """The output input `i` spends: the witness UTXO, or the previous
    transaction's output (a P2PKH input carries only the latter)."""
    psbt_in = psbt.i[i].map
    if PSBT_IN_WITNESS_UTXO in psbt_in:
        return from_binary(CTxOut, psbt_in[PSBT_IN_WITNESS_UTXO])
    previous = from_binary(CTransaction, psbt_in[PSBT_IN_NON_WITNESS_UTXO])
    return previous.vout[psbt.tx.vin[i].prevout.n]


def unified_script_code(psbt_in, pubkey, spent):
    """The scriptCode and script type the unified signer and finalizer use
    per shape (`coincube-core/src/unified_foreign.rs`): a P2WSH input signs
    its witness script as type 1; P2WPKH, native or nested, the implied P2PKH
    script as type 1; P2PKH its own scriptPubKey as type 0."""
    if PSBT_IN_WITNESS_SCRIPT in psbt_in.map:
        return psbt_in.map[PSBT_IN_WITNESS_SCRIPT], SCRIPT_TYPE_WITNESS_V0
    code = bytes([0x76, 0xA9, 0x14]) + hash160(pubkey) + bytes([0x88, 0xAC])
    if PSBT_IN_WITNESS_UTXO in psbt_in.map:
        return code, SCRIPT_TYPE_WITNESS_V0
    assert spent.scriptPubKey == code, spent
    return code, SCRIPT_TYPE_BASE


def unified_sighash(tx, index, hash_type, script_type, spent_outputs, script_code):
    """Bitcoin Knots' draft unified signature hash for script types 0 (bare,
    P2SH) and 1 (SegWit v0), ported here independently of the Rust
    implementation the bridge verifies against, and pinned to the upstream
    vectors by `test_unified_sighash_port_offline`. `spent_outputs` is the
    output every input spends, in input order."""
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


def script_pushes(script):
    """The data of a scriptSig made of direct pushes (a signature and a key)."""
    pushes, position = [], 0
    while position < len(script):
        size = script[position]
        assert 1 <= size <= 75, script.hex()
        pushes.append(script[position + 1 : position + 1 + size])
        position += 1 + size
    return pushes


def retained_items(shape, tx, i):
    """Input `i`'s items that may be signatures, as the retained-witness
    verifier takes them: the scriptSig pushes of a P2PKH spend, the witness
    items of a P2WPKH or P2SH-P2WPKH spend, the items before the witness
    script of a P2WSH spend."""
    if shape == "pkh":
        return script_pushes(tx.vin[i].scriptSig)
    stack = tx.wit.vtxinwit[i].scriptWitness.stack
    return stack[:-1] if shape.startswith("wsh_") else stack


def unsigned_tx_hex(psbt_b64):
    return PSBT.from_base64(psbt_b64).g.map[0].hex()


def txid_of(raw_hex):
    """Txid of a transaction serialized without witness."""
    return hash256(bytes.fromhex(raw_hex))[::-1].hex()


def outpoint_str(txid, vout):
    return f"{txid}:{vout}"


def check_poison(script, fork_marker, prevouts, built):
    """The poison's content, parsed here: chain byte, fork marker and the
    commitment to exactly `prevouts` (`(txid hex, vout)`), not only its length
    and opcode (#612 review, finding 2). The bridge's production decoder and
    rebuild must agree with the same bytes."""
    assert len(script) == 90, script.hex()
    assert script[:3] == bytes([0x6A, 0x4C, 87]), script.hex()
    payload = script[3:]
    assert payload[:14] == POISON_TAG
    assert payload[14] == POISON_VERSION
    assert payload[15] == POISON_CHAIN_BITCOIN
    # RPC hex is the reversed (display) byte order of the internal hash.
    assert payload[16:48] == bytes.fromhex(fork_marker)[::-1]
    # Sorted as the Rust set sorts outpoints: internal txid bytes, then vout.
    ordered = sorted((bytes.fromhex(txid)[::-1], vout) for txid, vout in prevouts)
    serialized = b"".join(txid + struct.pack("<I", vout) for txid, vout in ordered)
    assert payload[48:80] == sha256(serialized)
    assert payload[80:] == bytes(7)
    assert built["poison_script"] == script.hex()
    assert built["poison_fork_marker"] == fork_marker
    assert built["poison_rebuilds"] is True


def target_script(kind):
    """A step-2 target outside the foreign wallet: a fresh 1-key P2WSH, or a
    P2TR output key. Nothing here spends it."""
    key = coincurve.PrivateKey().public_key.format()
    if kind == "p2tr":
        return bytes([0x51, 0x20]) + key[1:]
    return bytes([0x00, 0x20]) + sha256(bytes([0x21]) + key + bytes([0xAC]))


def bridge_refuses(tool, request, reason):
    rejected = subprocess.run([tool], input=json.dumps(request), text=True,
                              capture_output=True, timeout=30)
    assert rejected.returncode != 0 and reason in rejected.stderr, rejected.stderr


def sign_and_finalize(tool, wallet, request, built):
    """Finalize with implicit and explicit SIGHASH_ALL; both must be identical."""
    request = dict(request, signed_step1=wallet.sign(built["step1_psbt"]))
    implicit = run_bridge(tool, request)
    request["signed_step1"] = wallet.sign(built["step1_psbt"], explicit_all=True)
    explicit = run_bridge(tool, request)
    assert explicit["step1_raw"] == implicit["step1_raw"], (explicit, implicit)
    assert explicit["step1_txid"] == implicit["step1_txid"]
    expected = 2 if wallet.shape.startswith("wsh_") else 1
    assert implicit["signatures_per_input"] == [expected] * len(request["coins"])
    assert implicit["construction_txid"] == built["unsigned_txid"]
    # Only native segwit spends keep the unsigned txid.
    native = wallet.shape in ("wpkh", "wsh_multi", "wsh_sortedmulti")
    assert (implicit["step1_txid"] == built["unsigned_txid"]) == native
    assert implicit["vsize"] <= built["maximum_signed_vbytes"], implicit
    return implicit


def fake_previous(script, amount_sat):
    tx = CTransaction()
    tx.nVersion = 2
    tx.vin = [CTxIn(COutPoint(int.from_bytes(os.urandom(32), "big"), 0), b"", 0xFFFFFFFD)]
    tx.vout = [CTxOut(amount_sat, script)]
    return tx.serialize_without_witness().hex()


def offline_request(wallet):
    block = {"height": 100, "hash": os.urandom(32).hex()}
    coins = [
        {"previous": fake_previous(wallet.script_pubkey(branch, 0), 1_000_000 * (branch + 1)),
         "vout": 0, "branch": ("external", "internal")[branch], "index": 0,
         "bitcoin_block": block, "btcb2_block": block}
        for branch in (0, 1)
    ]
    return {
        "external": wallet.descriptor(0),
        "internal": wallet.descriptor(1),
        "coins": coins,
        "fork_height": 200,
        "fork_marker": os.urandom(32).hex(),
        "destination": DESTINATION_INDEX,
        "feerate_vb": FEERATE_VB,
        "locktime": 150,
        "bitcoin_tip_height": 150,
    }


@pytest.mark.parametrize("shape", SHAPES)
def test_split_step1_bridge_offline(shape):
    """No node: fabricated prevouts, the real bridge and the real signer."""
    tool = split_tool()
    wallet = ForeignWallet(shape)
    request = offline_request(wallet)
    block = request["coins"][0]["bitcoin_block"]
    prevouts = [(txid_of(c["previous"]), c["vout"]) for c in request["coins"]]
    built = run_bridge(tool, request)
    unsigned = from_binary(CTransaction, bytes.fromhex(unsigned_tx_hex(built["step1_psbt"])))
    assert unsigned.vout[1].scriptPubKey == wallet.script_pubkey(0, DESTINATION_INDEX)
    check_poison(unsigned.vout[0].scriptPubKey, request["fork_marker"], prevouts, built)
    assert sorted(built["claimed_prevouts"]) == sorted(outpoint_str(*p) for p in prevouts)
    assert built["fee"] == built["maximum_signed_vbytes"] * FEERATE_VB
    request["recorded_step1"] = unsigned_tx_hex(built["step1_psbt"])
    assert run_bridge(tool, request)["reconstructed_txid"] == built["unsigned_txid"]
    del request["recorded_step1"]
    sign_and_finalize(tool, wallet, request, built)

    # A coin at or after the fork height is refused, whatever the signatures.
    late = copy.deepcopy(request)
    late["coins"][0]["bitcoin_block"] = late["coins"][0]["btcb2_block"] = {
        "height": 200, "hash": block["hash"]}
    bridge_refuses(tool, late, "PostFork")


def step2_request(request, claimed, target, tip):
    """A step-2 request over the step-1 request's coins, source and fork."""
    return {
        "external": request["external"],
        "internal": request["internal"],
        "coins": request["coins"],
        "fork_height": request["fork_height"],
        "step2": {
            "claimed": list(claimed),
            "target": target.hex(),
            "feerate_vb": FEERATE_VB,
            "locktime": tip,
            "btcb2_tip_height": tip,
        },
    }


def check_step2_construction(tool, request, built, target):
    """One output, the target, no change; exactly the claimed inputs; fee at
    the worst-case size; deterministic reconstruction."""
    unsigned = from_binary(CTransaction, bytes.fromhex(unsigned_tx_hex(built["step2_psbt"])))
    assert [o.scriptPubKey for o in unsigned.vout] == [target]
    assert built["target"] == target.hex()
    spent = sorted(f"{i.prevout.hash:064x}:{i.prevout.n}" for i in unsigned.vin)
    assert spent == sorted(request["step2"]["claimed"]), (spent, request["step2"]["claimed"])
    assert sorted(built["claimed_prevouts"]) == spent
    assert built["fee"] == built["maximum_signed_vbytes"] * FEERATE_VB
    total = sum(
        from_binary(CTransaction, bytes.fromhex(c["previous"])).vout[c["vout"]].nValue
        for c in request["coins"]
    )
    assert unsigned.vout[0].nValue == total - built["fee"]
    assert unsigned.nLockTime == request["step2"]["locktime"]
    recorded = copy.deepcopy(request)
    recorded["step2"]["recorded"] = unsigned_tx_hex(built["step2_psbt"])
    assert run_bridge(tool, recorded)["reconstructed_txid"] == built["unsigned_txid"]


def check_step2_refusals(tool, wallet, request):
    """Refused by the bridge (the production construction) before anything
    could be signed or broadcast."""
    claimed = request["step2"]["claimed"]
    # A coin step 1 did not claim, spent alongside the claimed one.
    extra = copy.deepcopy(request)
    extra["step2"]["claimed"] = claimed[:1]
    bridge_refuses(tool, extra, "ClaimedMismatch")
    # A claimed coin left out.
    short = copy.deepcopy(request)
    short["coins"] = [
        c for c in short["coins"]
        if outpoint_str(txid_of(c["previous"]), c["vout"]) == claimed[0]
    ]
    assert len(short["coins"]) == 1
    bridge_refuses(tool, short, "ClaimedMismatch")
    # Back into the foreign wallet: step 1's own destination script.
    home = copy.deepcopy(request)
    home["step2"]["target"] = wallet.script_pubkey(0, DESTINATION_INDEX).hex()
    bridge_refuses(tool, home, "InvalidTarget")
    # Not a Vault address type.
    wpkh = copy.deepcopy(request)
    wpkh["step2"]["target"] = (bytes([0x00, 0x14]) + os.urandom(20)).hex()
    bridge_refuses(tool, wpkh, "InvalidTarget")
    # A locktime above the observed BTCB2 tip.
    late = copy.deepcopy(request)
    late["step2"]["locktime"] = late["step2"]["btcb2_tip_height"] + 1
    bridge_refuses(tool, late, "Locktime")


def sign_and_finalize_step2(tool, wallet, request, built):
    """Finalize step 2 with implicit and explicit SIGHASH_ALL; both identical."""
    request = copy.deepcopy(request)
    request["step2"]["signed"] = wallet.sign(built["step2_psbt"])
    implicit = run_bridge(tool, request)
    request["step2"]["signed"] = wallet.sign(built["step2_psbt"], explicit_all=True)
    explicit = run_bridge(tool, request)
    assert explicit["step2_raw"] == implicit["step2_raw"], (explicit, implicit)
    assert explicit["step2_txid"] == implicit["step2_txid"]
    expected = 2 if wallet.shape.startswith("wsh_") else 1
    assert implicit["signatures_per_input"] == [expected] * len(request["coins"])
    assert implicit["construction_txid"] == built["unsigned_txid"]
    native = wallet.shape in ("wpkh", "wsh_multi", "wsh_sortedmulti")
    assert (implicit["step2_txid"] == built["unsigned_txid"]) == native
    assert implicit["vsize"] <= built["maximum_signed_vbytes"], implicit
    # A restart later than the construction: ten blocks on, as one would be.
    check_step2_recorded(
        tool, request, built, implicit["step2_raw"], implicit, request["step2"]["locktime"] + 10
    )
    return implicit


def check_step2_recorded(tool, request, built, raw, finalized, tip):
    """A restart at BTCB2 tip `tip`, as the service layer does it (#638 F2):
    the recorded signed bytes verify against the recorded unsigned sweep
    rebuilt at that tip (`reconstruct_split_step2`, then
    `verify_split_step2_transaction`), not against a fresh construction at
    the original tip. A tip below the recorded locktime is refused, and the
    same bytes with a changed locktime do not verify."""
    request = copy.deepcopy(request)
    request["step2"].pop("signed", None)
    locktime = request["step2"]["locktime"]
    assert tip >= locktime, (tip, locktime)
    # A restart knows the current tip and the journal's two records; a fresh
    # construction there would carry the current tip as its locktime.
    request["step2"]["locktime"] = request["step2"]["btcb2_tip_height"] = tip
    request["step2"]["recorded"] = unsigned_tx_hex(built["step2_psbt"])
    request["step2"]["recorded_signed"] = raw
    verified = run_bridge(tool, request)
    assert verified["reconstructed_txid"] == finalized["construction_txid"], verified
    assert verified["verified_txid"] == finalized["step2_txid"], verified
    assert verified["verified_signatures_per_input"] == finalized["signatures_per_input"]
    # Below the recorded locktime the record is not final there: refused
    # before any signature is looked at.
    early = copy.deepcopy(request)
    early["step2"]["locktime"] = early["step2"]["btcb2_tip_height"] = locktime - 1
    bridge_refuses(tool, early, "Locktime")
    tampered = bytes.fromhex(raw)
    assert struct.unpack("<I", tampered[-4:])[0] == locktime, raw
    request["step2"]["recorded_signed"] = (tampered[:-4] + struct.pack("<I", locktime - 1)).hex()
    bridge_refuses(tool, request, "ConstructionChanged")


@pytest.mark.parametrize("shape", SHAPES)
def test_split_step2_bridge_offline(shape):
    """No node: step 2 over fabricated prevouts, claimed by a real step 1."""
    tool = split_tool()
    wallet = ForeignWallet(shape)
    request = offline_request(wallet)
    claimed = run_bridge(tool, request)["claimed_prevouts"]
    target = target_script(TARGET_KIND.get(shape, "p2wsh"))
    request2 = step2_request(request, claimed, target, 150)
    built = run_bridge(tool, request2)
    check_step2_construction(tool, request2, built, target)
    check_step2_refusals(tool, wallet, request2)
    sign_and_finalize_step2(tool, wallet, request2, built)
    check_step2_signature_refusals(tool, wallet, request, request2, built)


def check_step2_signature_refusals(tool, wallet, request1, request2, built):
    """The step-2 finalizer refuses step 1's signatures over the same coins
    and any hash type but ALL, including BTCB2 `ALL|UNIFIED` (0x21, #585 F1)."""
    signed1 = PSBT.from_base64(wallet.sign(run_bridge(tool, request1)["step1_psbt"]))
    by_prevout = {
        (i.prevout.hash, i.prevout.n): signed1.i[n].map[PSBT_IN_PARTIAL_SIG]
        for n, i in enumerate(signed1.tx.vin)
    }
    transplanted = PSBT.from_base64(built["step2_psbt"])
    for n, txin in enumerate(transplanted.tx.vin):
        transplanted.i[n].map[PSBT_IN_PARTIAL_SIG] = dict(
            by_prevout[(txin.prevout.hash, txin.prevout.n)])
    crossed = copy.deepcopy(request2)
    crossed["step2"]["signed"] = transplanted.to_base64()
    bridge_refuses(tool, crossed, "InvalidSignature")

    # A BTCB2 unified request, `ALL|UNIFIED`, over otherwise valid signatures.
    unified = PSBT.from_base64(wallet.sign(built["step2_psbt"]))
    for psbt_in in unified.i:
        psbt_in.map[PSBT_IN_SIGHASH_TYPE] = struct.pack("<I", 0x21)
    refused = copy.deepcopy(request2)
    refused["step2"]["signed"] = unified.to_base64()
    bridge_refuses(tool, refused, "UnsupportedSighash")
    # A signature whose own hash-type byte is not ALL (ALL|ANYONECANPAY).
    anyone = PSBT.from_base64(wallet.sign(built["step2_psbt"]))
    for psbt_in in anyone.i:
        psbt_in.map[PSBT_IN_PARTIAL_SIG] = {
            key: sig[:-1] + bytes([0x81])
            for key, sig in psbt_in.map[PSBT_IN_PARTIAL_SIG].items()
        }
    refused["step2"]["signed"] = anyone.to_base64()
    bridge_refuses(tool, refused, "UnsupportedSighash")


def unified_request(request, target, tip):
    """A unified-sweep request over a request's coins, source and fork."""
    return {
        "external": request["external"],
        "internal": request["internal"],
        "coins": request["coins"],
        "fork_height": request["fork_height"],
        "unified": {
            "target": target.hex(),
            "feerate_vb": FEERATE_VB,
            "locktime": tip,
            "btcb2_tip_height": tip,
        },
    }


def check_unified_construction(tool, request, built, target):
    """Step 2's construction without a step 1: one output, the target, no
    change; exactly the wallet's coins; fee at the worst-case size;
    deterministic reconstruction."""
    unsigned = from_binary(
        CTransaction, bytes.fromhex(unsigned_tx_hex(built["unified_psbt"]))
    )
    assert [o.scriptPubKey for o in unsigned.vout] == [target]
    assert built["target"] == target.hex()
    spent = sorted(f"{i.prevout.hash:064x}:{i.prevout.n}" for i in unsigned.vin)
    coins = sorted(
        outpoint_str(txid_of(c["previous"]), c["vout"]) for c in request["coins"]
    )
    assert spent == coins == sorted(built["spent_outpoints"]), (spent, coins, built)
    assert built["fee"] == built["maximum_signed_vbytes"] * FEERATE_VB
    total = sum(
        from_binary(CTransaction, bytes.fromhex(c["previous"])).vout[c["vout"]].nValue
        for c in request["coins"]
    )
    assert unsigned.vout[0].nValue == total - built["fee"]
    assert unsigned.nLockTime == request["unified"]["locktime"]
    assert built["fork_height"] == request["fork_height"]
    recorded = copy.deepcopy(request)
    recorded["unified"]["recorded"] = unsigned_tx_hex(built["unified_psbt"])
    assert run_bridge(tool, recorded)["reconstructed_txid"] == built["unsigned_txid"]


def check_unified_refusals(tool, wallet, request):
    """Refused by the bridge (the production construction) before anything
    could be signed or broadcast."""
    # Back into the foreign wallet: one of its own scripts.
    home = copy.deepcopy(request)
    home["unified"]["target"] = wallet.script_pubkey(0, DESTINATION_INDEX).hex()
    bridge_refuses(tool, home, "InvalidTarget")
    # Not a Vault address type.
    wpkh = copy.deepcopy(request)
    wpkh["unified"]["target"] = (bytes([0x00, 0x14]) + os.urandom(20)).hex()
    bridge_refuses(tool, wpkh, "InvalidTarget")
    # A locktime above the observed BTCB2 tip.
    late = copy.deepcopy(request)
    late["unified"]["locktime"] = late["unified"]["btcb2_tip_height"] + 1
    bridge_refuses(tool, late, "Locktime")
    # A coin at or after the fork height: not shared history (D10).
    post_fork = copy.deepcopy(request)
    block = post_fork["coins"][0]["btcb2_block"]
    post_fork["coins"][0]["bitcoin_block"] = post_fork["coins"][0]["btcb2_block"] = {
        "height": request["fork_height"],
        "hash": block["hash"],
    }
    bridge_refuses(tool, post_fork, "PostFork")
    # No fee.
    free = copy.deepcopy(request)
    free["unified"]["feerate_vb"] = 0
    bridge_refuses(tool, free, "Economics")
    # One request builds one thing.
    both = copy.deepcopy(request)
    both["step2"] = dict(request["unified"], claimed=[])
    bridge_refuses(tool, both, "one of step 1, step 2 or the unified sweep")


def sign_and_finalize_unified(tool, wallet, request, built):
    """Finalize the unified sweep: every input's witness holds `threshold`
    verified 0x21 signatures and no legacy one, so it classifies Protected;
    then the bytes verify as a recorded sweep at a later tip."""
    request = copy.deepcopy(request)
    request["unified"]["signed"] = wallet.sign_unified(built["unified_psbt"])
    finalized = run_bridge(tool, request)
    count = len(request["coins"])
    expected = 2 if wallet.shape.startswith("wsh_") else 1
    assert finalized["inputs"] == [{"unified_used": expected, "legacy_used": 0}] * count
    assert finalized["replay_status"] == "Protected", finalized
    assert finalized["construction_txid"] == built["unsigned_txid"]
    assert finalized["verified_fee"] == built["fee"]
    native = wallet.shape in ("wpkh", "wsh_multi", "wsh_sortedmulti")
    assert (finalized["unified_txid"] == built["unsigned_txid"]) == native
    assert finalized["vsize"] <= built["maximum_signed_vbytes"], finalized
    # The bytes themselves: every retained signature ends in 0x21, none in 0x01.
    tx = from_binary(CTransaction, bytes.fromhex(finalized["unified_raw"]))
    for i in range(count):
        items = retained_items(wallet.shape, tx, i)
        signatures = [item for item in items if item[:1] == b"\x30"]
        assert len(signatures) == expected, items
        assert all(s[-1] == UNIFIED_SIGHASH_ALL for s in signatures), items
    check_unified_recorded(
        tool,
        request,
        built,
        finalized["unified_raw"],
        finalized,
        request["unified"]["locktime"] + 10,
    )
    return finalized


def check_unified_recorded(tool, request, built, raw, finalized, tip):
    """A restart at BTCB2 tip `tip`, as for step 2 (#638 F2): the recorded
    signed bytes verify against the recorded unsigned sweep rebuilt at that
    tip (`reconstruct_unified_sweep`, then
    `verify_unified_sweep_transaction`), Protected again. A tip below the
    recorded locktime is refused, and the same bytes with a changed locktime
    do not verify."""
    request = copy.deepcopy(request)
    request["unified"].pop("signed", None)
    locktime = request["unified"]["locktime"]
    assert tip >= locktime, (tip, locktime)
    request["unified"]["locktime"] = request["unified"]["btcb2_tip_height"] = tip
    request["unified"]["recorded"] = unsigned_tx_hex(built["unified_psbt"])
    request["unified"]["recorded_signed"] = raw
    verified = run_bridge(tool, request)
    assert verified["reconstructed_txid"] == finalized["construction_txid"], verified
    assert verified["verified_txid"] == finalized["unified_txid"], verified
    assert verified["verified_inputs"] == finalized["inputs"]
    assert verified["verified_replay_status"] == "Protected"
    early = copy.deepcopy(request)
    early["unified"]["locktime"] = early["unified"]["btcb2_tip_height"] = locktime - 1
    bridge_refuses(tool, early, "Locktime")
    tampered = bytes.fromhex(raw)
    assert struct.unpack("<I", tampered[-4:])[0] == locktime, raw
    request["unified"]["recorded_signed"] = (
        tampered[:-4] + struct.pack("<I", locktime - 1)
    ).hex()
    bridge_refuses(tool, request, "ConstructionChanged")


def check_unified_signature_refusals(tool, wallet, request, built):
    """Owner decision P1 (block): a legacy signature anywhere on this route is
    refused, never classified. Returns the legacy twin: the same unsigned
    transaction finalized by step 2's path with SIGHASH_ALL, what a standard
    PSBT consumer makes of these keys and what Bitcoin accepts (the
    consensus test shows it); the retained-witness verifier refuses it."""
    # SIGHASH_ALL partial signatures under a 0x21 request: the 0x01 twin.
    legacy = PSBT.from_base64(wallet.sign(built["unified_psbt"]))
    for psbt_in in legacy.i:
        psbt_in.map[PSBT_IN_SIGHASH_TYPE] = struct.pack("<I", UNIFIED_SIGHASH_ALL)
    refused = copy.deepcopy(request)
    refused["unified"]["signed"] = legacy.to_base64()
    bridge_refuses(tool, refused, "LegacySignature")
    # Unified records under a 0x01 request: refused by the finalizer, whose
    # error is anchored exactly. The bare name would also match the PSBT
    # adapter's `UnsupportedSighashRequest`, which refuses requests the
    # chain never serves (0x02, say) before the finalizer is reached.
    refused["unified"]["signed"] = wallet.sign_unified(
        built["unified_psbt"], request=SIGHASH_ALL
    )
    bridge_refuses(tool, refused, "UnsupportedSighash { input")
    # Unified records plus a legacy partial signature by the same key: the
    # PSBT adapter refuses the ambiguity before the finalizer is reached.
    mixed = PSBT.from_base64(wallet.sign_unified(built["unified_psbt"]))
    mixed.i[0].map[PSBT_IN_PARTIAL_SIG] = dict(legacy.i[0].map[PSBT_IN_PARTIAL_SIG])
    refused["unified"]["signed"] = mixed.to_base64()
    bridge_refuses(tool, refused, "AmbiguousSignatureEncoding")
    if wallet.shape.startswith("wsh_"):
        # Unified records from two keys plus the third key's legacy
        # signature: unambiguous, and refused by the finalizer.
        third = PSBT.from_base64(
            wallet.sign(built["unified_psbt"], signers=wallet.hds[2:])
        )
        mixed = PSBT.from_base64(wallet.sign_unified(built["unified_psbt"]))
        mixed.i[0].map[PSBT_IN_PARTIAL_SIG] = dict(third.i[0].map[PSBT_IN_PARTIAL_SIG])
        assert len(mixed.i[0].map[PSBT_IN_PARTIAL_SIG]) == 1
        refused["unified"]["signed"] = mixed.to_base64()
        bridge_refuses(tool, refused, "LegacySignature")
    # The retained-witness twin. The unified construction is step 2's over
    # the same coins, target, fee and locktime, so step 2's finalizer yields
    # the same unsigned transaction with 0x01 witnesses.
    tip = request["unified"]["btcb2_tip_height"]
    assert request["unified"]["locktime"] == tip
    twin_request = step2_request(
        request, built["spent_outpoints"], bytes.fromhex(built["target"]), tip
    )
    built2 = run_bridge(tool, twin_request)
    assert built2["unsigned_txid"] == built["unsigned_txid"], (built2, built)
    twin_request["step2"]["signed"] = wallet.sign(built2["step2_psbt"])
    twin = run_bridge(tool, twin_request)["step2_raw"]
    recorded = copy.deepcopy(request)
    recorded["unified"]["recorded"] = unsigned_tx_hex(built["unified_psbt"])
    recorded["unified"]["recorded_signed"] = twin
    bridge_refuses(tool, recorded, "LegacySignature")
    return twin


@pytest.mark.parametrize("shape", SHAPES)
def test_split_unified_bridge_offline(shape):
    """No node: the unified sweep over fabricated prevouts, signed 0x21 here
    and finalized by the real finalizer; every legacy twin refused."""
    tool = split_tool()
    wallet = ForeignWallet(shape)
    target = target_script(TARGET_KIND.get(shape, "p2wsh"))
    request = unified_request(offline_request(wallet), target, 150)
    built = run_bridge(tool, request)
    check_unified_construction(tool, request, built, target)
    check_unified_refusals(tool, wallet, request)
    sign_and_finalize_unified(tool, wallet, request, built)
    check_unified_signature_refusals(tool, wallet, request, built)


# ── two-chain consensus ───────────────────────────────────────────────────


def split_two_chain_class():
    from test_framework.btcb2 import MIN_ACTIVATION_HEIGHT, TwoChainRegtest

    class SplitTwoChain(TwoChainRegtest):
        """The pinned two-node history with foreign wallets funded pre-fork.

        No indexer or daemon: step 1 involves no Cube, so only the nodes run.
        """

        def __init__(self, directory):
            super().__init__(directory)
            self.wallets = {shape: ForeignWallet(shape) for shape in SHAPES}
            self.funded = {}  # shape -> [(txid, vout, branch, index, block hash)]
            # The unified fallback's wallets: never split in two steps, so
            # their coins stay unspent on both chains until the sweep.
            self.unified_wallets = {shape: ForeignWallet(shape) for shape in SHAPES}
            self.unified_funded = {}
            # shape -> the confirmed step 1 (txid, block, claimed prevouts, raw).
            self.step1 = {}
            # shape -> the step 2 mined on BTCB2 (raw, txid, block).
            self.step2 = {}

        def setup(self):
            os.makedirs(self.home_dir, exist_ok=True)
            self._start_legacy_and_fund()
            self._fork()
            self.blake2b.generate_block(self.post_fork_blocks_blake2b)
            self.legacy.generate_block(self.post_fork_blocks_blake2b)
            assert self.legacy.rpc.getblockhash(self.activation_height) != \
                self.blake2b.rpc.getblockhash(self.activation_height)

        def _start_legacy_and_fund(self):
            # The base class lands on its activation height - 1. Stop it at
            # the earliest allowed height so the foreign funding below still
            # confirms before the real fork, then land on the fork parent.
            fork = self.activation_height
            self.activation_height = MIN_ACTIVATION_HEIGHT
            try:
                super()._start_legacy_and_fund()
            finally:
                self.activation_height = fork
            rpc = self.legacy.rpc
            txids = []
            sets = (
                (self.wallets, self.funded),
                (self.unified_wallets, self.unified_funded),
            )
            funding = [
                (shape, wallet, funded)
                for wallets, funded in sets
                for shape, wallet in wallets.items()
            ]
            for shape, wallet, funded in funding:
                scripts = {}
                outputs = {}
                for branch, amount in zip((0, 1), FUND_AMOUNTS):
                    desc = rpc.getdescriptorinfo(wallet.descriptor(branch))["descriptor"]
                    address = rpc.deriveaddresses(desc, [0, 0])[0]
                    info = rpc.validateaddress(address)
                    assert bytes.fromhex(info["scriptPubKey"]) == wallet.script_pubkey(branch, 0)
                    outputs[address] = amount
                    scripts[info["scriptPubKey"]] = branch
                txid = rpc.sendmany("", outputs)
                txids.append(txid)
                funded[shape] = (txid, scripts)
            self.legacy.generate_block(1, wait_for_mempool=txids)
            block = rpc.getbestblockhash()
            assert rpc.getblockcount() < fork - 1
            for _, funded in sets:
                for shape, (txid, scripts) in list(funded.items()):
                    decoded = rpc.getrawtransaction(txid, True, block)
                    funded[shape] = [
                        (txid, out["n"], scripts[out["scriptPubKey"]["hex"]], 0, block)
                        for out in decoded["vout"]
                        if out["scriptPubKey"]["hex"] in scripts
                    ]
                    assert len(funded[shape]) == 2
            self.legacy.generate_block(fork - 1 - rpc.getblockcount())
            assert rpc.getblockcount() == fork - 1
            self.fork_parent_hash = rpc.getbestblockhash()

    return SplitTwoChain


@pytest.fixture(scope="module")
def split_chains(request, test_base_dir):
    from test_framework.btcb2 import missing_binaries

    missing = missing_binaries()
    if missing:
        msg = f"BTCB2 harness binaries unset or not executable: {', '.join(missing)}"
        if os.getenv("BTCB2_HARNESS_REQUIRED") == "1":
            pytest.fail(msg)
        pytest.skip(msg)
    directory = tempfile.mkdtemp(prefix="btcb2_split-", dir=test_base_dir)
    harness = split_two_chain_class()(directory)
    try:
        harness.setup()
    except Exception:
        harness.cleanup()
        raise

    yield harness

    harness.cleanup()
    if request.session.testsfailed == 0:
        shutil.rmtree(directory)
    else:
        print(f"Test failed, leaving directory '{directory}' intact")


def observe(node, txid, block_hash):
    """The coin's confirming block as this node itself reports it."""
    header = node.rpc.getblockheader(block_hash)
    assert header["confirmations"] > 0, header
    node.rpc.getrawtransaction(txid, False, block_hash)
    return {"height": header["height"], "hash": block_hash}


def observed_coins(split_chains, shape, funded=None):
    """Each funded coin with its confirming block as each node reports it."""
    a, b = split_chains.legacy, split_chains.blake2b
    if funded is None:
        funded = split_chains.funded
    coins = []
    for txid, vout, branch, index, block in funded[shape]:
        coins.append({
            "previous": a.rpc.getrawtransaction(txid, False, block),
            "vout": vout,
            "branch": ("external", "internal")[branch],
            "index": index,
            "bitcoin_block": observe(a, txid, block),
            "btcb2_block": observe(b, txid, block),
        })
    return coins


def step1_request(split_chains, shape):
    wallet = split_chains.wallets[shape]
    tip = split_chains.legacy.rpc.getblockcount()
    return {
        "external": wallet.descriptor(0),
        "internal": wallet.descriptor(1),
        "coins": observed_coins(split_chains, shape),
        "fork_height": split_chains.activation_height,
        "fork_marker": split_chains.fork_parent_hash,
        "destination": DESTINATION_INDEX,
        "feerate_vb": FEERATE_VB,
        "locktime": tip,
        "bitcoin_tip_height": tip,
    }


def level(node, height):
    """Mine `node` up to at least `height`, so a transaction with a height
    locktime of `height` is final in its next block on that chain."""
    count = node.rpc.getblockcount()
    if count < height:
        node.generate_block(height - count)
    assert node.rpc.getblockcount() >= height


def confirmed_step1(split_chains, shape):
    """Step 1 confirmed on Bitcoin for `shape`: the one the step-1 test
    recorded, or, when that test did not run first, built, signed and mined
    to six confirmations here."""
    if shape not in split_chains.step1:
        tool = split_tool()
        a = split_chains.legacy
        wallet = split_chains.wallets[shape]
        request = step1_request(split_chains, shape)
        built = run_bridge(tool, request)
        finalized = sign_and_finalize(tool, wallet, request, built)
        step1_id = finalized["step1_txid"]
        height = a.rpc.getblockcount() + 1
        assert a.rpc.sendrawtransaction(finalized["step1_raw"]) == step1_id
        a.generate_block(6, wait_for_mempool=step1_id)
        block = a.rpc.getblockhash(height)
        assert a.rpc.getrawtransaction(step1_id, True, block)["confirmations"] == 6
        split_chains.step1[shape] = (
            step1_id, block, built["claimed_prevouts"], finalized["step1_raw"]
        )
    return split_chains.step1[shape]


@pytest.mark.parametrize("shape", SHAPES)
def test_split_step1_consensus(split_chains, shape, record_property):
    tool = split_tool()
    a, b = split_chains.legacy, split_chains.blake2b
    wallet = split_chains.wallets[shape]
    request = step1_request(split_chains, shape)
    tip = request["bitcoin_tip_height"]
    built = run_bridge(tool, request)
    request["recorded_step1"] = unsigned_tx_hex(built["step1_psbt"])
    assert run_bridge(tool, request)["reconstructed_txid"] == built["unsigned_txid"]
    del request["recorded_step1"]
    finalized = sign_and_finalize(tool, wallet, request, built)
    step1, step1_id = finalized["step1_raw"], finalized["step1_txid"]
    # The poison's content in the exact bytes the BTCB2 block refuses below.
    prevouts = [(c[0], c[1]) for c in split_chains.funded[shape]]
    signed = from_binary(CTransaction, bytes.fromhex(step1))
    check_poison(signed.vout[0].scriptPubKey, split_chains.fork_parent_hash, prevouts, built)

    # Accepted by the Bitcoin node at no more than the construction's estimate.
    verdict_a = a.rpc.testmempoolaccept([step1])[0]
    assert verdict_a["allowed"], verdict_a
    assert verdict_a["txid"] == step1_id
    assert verdict_a["vsize"] == finalized["vsize"], (verdict_a, finalized)
    assert verdict_a["vsize"] <= built["maximum_signed_vbytes"], (verdict_a, built)
    record_property(f"{shape}_vsize", verdict_a["vsize"])
    record_property(f"{shape}_maximum_signed_vbytes", built["maximum_signed_vbytes"])

    # Refused by the BTCB2 node: by relay policy, and by consensus in a block.
    # The locktime is Bitcoin's tip, and earlier shapes mined Bitcoin only.
    # Bring BTCB2 level first so the candidate block is final there and the
    # poison, not the locktime, is what consensus refuses.
    level(b, tip)
    verdict_b = b.rpc.testmempoolaccept([step1])[0]
    assert not verdict_b["allowed"], verdict_b
    assert verdict_b["reject-reason"] == "scriptpubkey", verdict_b
    fork_tip = b.rpc.getbestblockhash()
    with pytest.raises(JSONRPCException) as rejected_block:
        b.rpc.generateblock(b.rpc.getnewaddress(), [step1])
    block_error = rejected_block.value.error
    assert block_error["code"] == -25, block_error
    assert block_error["message"].startswith(
        "TestBlockValidity failed: bad-txns-vout-script-toolarge,"
    ), block_error
    assert step1_id in block_error["message"], block_error
    record_property(f"{shape}_btcb2_block_reject", block_error["message"])
    assert b.rpc.getbestblockhash() == fork_tip

    # Confirmed on Bitcoin; the same outpoints stay unspent on BTCB2.
    assert a.rpc.sendrawtransaction(step1) == step1_id
    a.generate_block(6, wait_for_mempool=step1_id)
    status = a.rpc.getrawtransaction(step1_id, True, a.rpc.getblockhash(tip + 1))
    assert status["confirmations"] == 6, status
    step1_block = status["blockhash"]
    for txid, vout in prevouts:
        assert a.rpc.gettxout(txid, vout, False) is None
        assert b.rpc.gettxout(txid, vout, False) is not None
    assert a.rpc.gettxout(step1_id, 1, False) is not None

    # Reorg: invalidating step 1's block takes it out of the active chain and
    # the spent outpoints are unspent on Bitcoin again.
    best = a.rpc.getbestblockhash()
    a.rpc.invalidateblock(step1_block)
    reorged = a.rpc.getrawtransaction(step1_id, True, step1_block)
    assert not reorged["in_active_chain"], reorged
    assert reorged.get("confirmations", 0) <= 0, reorged
    for txid, vout in prevouts:
        assert a.rpc.gettxout(txid, vout, False) is not None
    assert a.rpc.gettxout(step1_id, 1, False) is None
    # Restore the chain for the next shape.
    a.rpc.reconsiderblock(step1_block)
    assert a.rpc.getbestblockhash() == best
    assert a.rpc.getrawtransaction(step1_id, True, step1_block)["confirmations"] == 6
    split_chains.step1[shape] = (step1_id, step1_block, built["claimed_prevouts"], step1)


@pytest.mark.parametrize("shape", SHAPES)
def test_split_step2_consensus(split_chains, shape, record_property):
    """Step 2 spends exactly step 1's claimed prevouts to the target: accepted
    on BTCB2 (mempool and a mined block) and refused on Bitcoin, where step 1
    already spent them."""
    tool = split_tool()
    a, b = split_chains.legacy, split_chains.blake2b
    wallet = split_chains.wallets[shape]
    step1_id, step1_block, claimed, _ = confirmed_step1(split_chains, shape)
    prevouts = [(c[0], c[1]) for c in split_chains.funded[shape]]
    assert sorted(claimed) == sorted(outpoint_str(*p) for p in prevouts)

    # Step 1 has at least six confirmations on Bitcoin, and the claimed
    # outpoints are spent there and unspent on BTCB2.
    status = a.rpc.getrawtransaction(step1_id, True, step1_block)
    assert status["in_active_chain"] and status["confirmations"] >= 6, status
    for txid, vout in prevouts:
        assert a.rpc.gettxout(txid, vout, False) is None
        assert b.rpc.gettxout(txid, vout, False) is not None
    record_property(f"{shape}_step1_confirmations", status["confirmations"])

    # The locktime is BTCB2's tip; bring BTCB2 level with Bitcoin first so
    # both chains' next block is past it (B6a's alignment, for both chains).
    level(b, a.rpc.getblockcount())
    tip = b.rpc.getblockcount()
    target = target_script(TARGET_KIND.get(shape, "p2wsh"))
    request = step2_request(step1_request(split_chains, shape), claimed, target, tip)
    built = run_bridge(tool, request)
    check_step2_construction(tool, request, built, target)
    check_step2_refusals(tool, wallet, request)
    finalized = sign_and_finalize_step2(tool, wallet, request, built)
    step2, step2_id = finalized["step2_raw"], finalized["step2_txid"]
    record_property(f"{shape}_step2_target", TARGET_KIND.get(shape, "p2wsh"))

    # Accepted by the BTCB2 node at no more than the construction's estimate.
    verdict_b = b.rpc.testmempoolaccept([step2])[0]
    assert verdict_b["allowed"], verdict_b
    assert verdict_b["txid"] == step2_id
    assert verdict_b["vsize"] == finalized["vsize"], (verdict_b, finalized)
    assert verdict_b["vsize"] <= built["maximum_signed_vbytes"], (verdict_b, built)
    record_property(f"{shape}_step2_vsize", verdict_b["vsize"])
    record_property(f"{shape}_step2_maximum_signed_vbytes", built["maximum_signed_vbytes"])

    # Refused by the Bitcoin node: its inputs are spent by step 1. Bitcoin is
    # first brought past the locktime, so missing inputs, not finality, is
    # what each check refuses.
    level(a, tip)
    verdict_a = a.rpc.testmempoolaccept([step2])[0]
    assert not verdict_a["allowed"], verdict_a
    assert verdict_a["reject-reason"] == "missing-inputs", verdict_a
    bitcoin_tip = a.rpc.getbestblockhash()
    with pytest.raises(JSONRPCException) as rejected_block:
        a.rpc.generateblock(a.rpc.getnewaddress(), [step2])
    block_error = rejected_block.value.error
    assert block_error["code"] == -25, block_error
    assert block_error["message"].startswith(
        "TestBlockValidity failed: bad-txns-inputs-missingorspent,"
    ), block_error
    record_property(f"{shape}_bitcoin_block_reject", block_error["message"])
    assert a.rpc.getbestblockhash() == bitcoin_tip

    # Mined on BTCB2: the claimed outpoints are spent there, into the target.
    assert b.rpc.sendrawtransaction(step2) == step2_id
    b.generate_block(1, wait_for_mempool=step2_id)
    mined = b.rpc.getrawtransaction(step2_id, True, b.rpc.getbestblockhash())
    assert mined["confirmations"] == 1, mined
    # Byte identity, witness included: what BTCB2 mined is what was sent,
    # and its wtxid is the hash of those bytes (#638 F3). Only pkh carries
    # no witness, so only there the wtxid is the txid.
    assert mined["hex"] == step2, (mined["hex"], step2)
    assert mined["hash"] == hash256(bytes.fromhex(step2))[::-1].hex(), mined
    assert (mined["hash"] == step2_id) == (shape == "pkh"), mined
    assert [o["scriptPubKey"]["hex"] for o in mined["vout"]] == [target.hex()]
    # The bytes BTCB2 mined verify as a recorded step 2 at the tip after
    # mining, where a restart would rebuild it (#638 F2).
    check_step2_recorded(tool, request, built, mined["hex"], finalized, b.rpc.getblockcount())
    for txid, vout in prevouts:
        assert b.rpc.gettxout(txid, vout, False) is None
    assert b.rpc.gettxout(step2_id, 0, False)["scriptPubKey"]["hex"] == target.hex()
    # Step 1 is unaffected on Bitcoin.
    assert a.rpc.getrawtransaction(step1_id, True, step1_block)["in_active_chain"]
    split_chains.step2[shape] = (step2, step2_id, mined["blockhash"])


def confirmed_step2(split_chains, shape):
    """Step 2 mined on BTCB2 for `shape`: the one the step-2 test recorded,
    or, when that test did not run first, built, signed and mined here, with
    Bitcoin brought past its locktime as that test does."""
    if shape not in split_chains.step2:
        tool = split_tool()
        a, b = split_chains.legacy, split_chains.blake2b
        wallet = split_chains.wallets[shape]
        _, _, claimed, _ = confirmed_step1(split_chains, shape)
        level(b, a.rpc.getblockcount())
        tip = b.rpc.getblockcount()
        target = target_script(TARGET_KIND.get(shape, "p2wsh"))
        request = step2_request(step1_request(split_chains, shape), claimed, target, tip)
        built = run_bridge(tool, request)
        finalized = sign_and_finalize_step2(tool, wallet, request, built)
        step2_id = finalized["step2_txid"]
        assert b.rpc.sendrawtransaction(finalized["step2_raw"]) == step2_id
        b.generate_block(1, wait_for_mempool=step2_id)
        block = b.rpc.getbestblockhash()
        assert b.rpc.getrawtransaction(step2_id, True, block)["confirmations"] == 1
        level(a, tip)
        split_chains.step2[shape] = (finalized["step2_raw"], step2_id, block)
    return split_chains.step2[shape]


@pytest.mark.parametrize("shape", SHAPES)
def test_split_step2_replay_after_step1_reorg(split_chains, shape, record_property):
    """The window the post-submission reorg design must close (#568, B6c-1),
    pinned at node level. Bitcoin refuses the step 2 BTCB2 mined only while
    step 1 is in its active chain (the counterfactual, first) or in its
    mempool. Once step 1's block is invalidated, the same bytes are a valid
    Bitcoin transaction over the claimed prevouts: the mempool refuses them
    only as step 1's conflict, and a block carrying them is accepted. The
    chain is restored afterwards. Nothing here observes through Split."""
    a, b = split_chains.legacy, split_chains.blake2b
    step1_id, step1_block, claimed, step1 = confirmed_step1(split_chains, shape)
    step2, step2_id, step2_block = confirmed_step2(split_chains, shape)
    prevouts = [(c[0], c[1]) for c in split_chains.funded[shape]]
    locktime = struct.unpack("<I", bytes.fromhex(step2)[-4:])[0]
    # Bitcoin is past step 2's locktime, so finality never refuses it below.
    level(a, locktime)
    best = a.rpc.getbestblockhash()
    assert a.rpc.getrawtransaction(step1_id, True, step1_block)["in_active_chain"]

    # Counterfactual: with step 1 in the active chain, consensus refuses the
    # replay (the step-2 test shows the mempool does too).
    with pytest.raises(JSONRPCException) as refused:
        a.rpc.generateblock(a.rpc.getnewaddress(), [step2])
    assert refused.value.error["code"] == -25, refused.value.error
    assert refused.value.error["message"].startswith(
        "TestBlockValidity failed: bad-txns-inputs-missingorspent,"
    ), refused.value.error
    assert a.rpc.getbestblockhash() == best

    # Step 1 leaves the active chain. Core returns a disconnected block's
    # transactions to its mempool only for a reorg ten blocks deep or less;
    # deeper, a peer still relaying step 1 puts it back, as here.
    a.rpc.invalidateblock(step1_block)
    for txid, vout in prevouts:
        assert a.rpc.gettxout(txid, vout, False) is not None
    returned = step1_id in a.rpc.getrawmempool()
    record_property(f"{shape}_step1_returned_to_mempool", returned)
    if not returned:
        assert a.rpc.sendrawtransaction(step1) == step1_id
    assert step1_id in a.rpc.getrawmempool()
    # Step 2's locktime is BTCB2's tip at its construction, past step 1's
    # block: mine empty blocks on the new branch until step 2 is final
    # there, leaving step 1 in the mempool.
    branch_root = a.rpc.getblockcount() + 1
    a.generate_empty_blocks(max(0, locktime - a.rpc.getblockcount()))
    assert a.rpc.getblockcount() >= locktime
    assert step1_id in a.rpc.getrawmempool()
    # The mempool refuses step 2 only because step 1 conflicts with it: both
    # signal replaceability and step 2 pays less than step 1.
    verdict = a.rpc.testmempoolaccept([step2])[0]
    assert not verdict["allowed"], verdict
    assert verdict["reject-reason"] in ("insufficient fee", "txn-mempool-conflict"), verdict
    record_property(f"{shape}_replay_mempool_reject", verdict["reject-reason"])
    # The replay: a block carrying step 2 is valid on Bitcoin. Step 1 is
    # evicted as its conflict and the claimed coins move to the target.
    replay_block = a.rpc.generateblock(a.rpc.getnewaddress(), [step2])["hash"]
    record_property(f"{shape}_replay_block", replay_block)
    replayed = a.rpc.getrawtransaction(step2_id, True, replay_block)
    assert replayed["in_active_chain"] and replayed["hex"] == step2, replayed
    assert step1_id not in a.rpc.getrawmempool()
    assert a.rpc.gettxout(step2_id, 0, False) is not None
    for txid, vout in prevouts:
        assert a.rpc.gettxout(txid, vout, False) is None

    # Restore: drop the whole branch, then reconsider step 1's block. Step 2
    # is neither in Bitcoin's active chain nor relayable there again.
    a.rpc.invalidateblock(a.rpc.getblockhash(branch_root))
    a.rpc.reconsiderblock(step1_block)
    assert a.rpc.getbestblockhash() == best
    assert a.rpc.getrawtransaction(step1_id, True, step1_block)["in_active_chain"]
    assert not a.rpc.getrawtransaction(step2_id, True, replay_block)["in_active_chain"]
    assert step2_id not in a.rpc.getrawmempool()
    verdict = a.rpc.testmempoolaccept([step2])[0]
    assert not verdict["allowed"] and verdict["reject-reason"] == "missing-inputs", verdict
    for txid, vout in prevouts:
        assert a.rpc.gettxout(txid, vout, False) is None
    # BTCB2 saw none of it.
    assert b.rpc.getrawtransaction(step2_id, True, step2_block)["in_active_chain"]


# ── unified fallback consensus ────────────────────────────────────────────


def unified_base_request(split_chains, shape):
    """The unified wallet's coins, source and fork for `shape`: no step 1
    exists for these coins, and none is built."""
    wallet = split_chains.unified_wallets[shape]
    return {
        "external": wallet.descriptor(0),
        "internal": wallet.descriptor(1),
        "coins": observed_coins(split_chains, shape, split_chains.unified_funded),
        "fork_height": split_chains.activation_height,
    }


@pytest.mark.parametrize("shape", SHAPES)
def test_split_unified_fallback_consensus(split_chains, shape, record_property):
    """The single-step fallback (#568 B4b-1a, B6-d): one BTCB2 sweep of the
    foreign wallet's shared coins to the target, signed `ALL|UNIFIED` (0x21)
    with no step 1 and no poison. Accepted and mined by the BTCB2 node;
    refused by the Bitcoin node, in its mempool and in a block, for the
    signature alone: the same construction with legacy signatures is a valid
    Bitcoin transaction (checked, never sent). Nothing moves on Bitcoin."""
    tool = split_tool()
    a, b = split_chains.legacy, split_chains.blake2b
    wallet = split_chains.unified_wallets[shape]
    prevouts = [(c[0], c[1]) for c in split_chains.unified_funded[shape]]
    # Shared pre-fork history, unspent on both chains.
    for txid, vout in prevouts:
        assert a.rpc.gettxout(txid, vout, False) is not None
        assert b.rpc.gettxout(txid, vout, False) is not None

    # The locktime is BTCB2's tip; bring BTCB2 level with Bitcoin first.
    level(b, a.rpc.getblockcount())
    tip = b.rpc.getblockcount()
    target = target_script(TARGET_KIND.get(shape, "p2wsh"))
    request = unified_request(unified_base_request(split_chains, shape), target, tip)
    built = run_bridge(tool, request)
    check_unified_construction(tool, request, built, target)
    check_unified_refusals(tool, wallet, request)
    finalized = sign_and_finalize_unified(tool, wallet, request, built)
    twin = check_unified_signature_refusals(tool, wallet, request, built)
    sweep, sweep_id = finalized["unified_raw"], finalized["unified_txid"]
    assert finalized["replay_status"] == "Protected"
    record_property(f"{shape}_unified_target", TARGET_KIND.get(shape, "p2wsh"))
    record_property(f"{shape}_unified_inputs", json.dumps(finalized["inputs"]))

    # Accepted by the BTCB2 node at no more than the construction's estimate.
    verdict_b = b.rpc.testmempoolaccept([sweep])[0]
    assert verdict_b["allowed"], verdict_b
    assert verdict_b["txid"] == sweep_id
    assert verdict_b["vsize"] == finalized["vsize"], (verdict_b, finalized)
    assert verdict_b["vsize"] <= built["maximum_signed_vbytes"], (verdict_b, built)
    record_property(f"{shape}_unified_vsize", verdict_b["vsize"])
    record_property(
        f"{shape}_unified_maximum_signed_vbytes", built["maximum_signed_vbytes"]
    )

    # Refused by the Bitcoin node for the signature: by relay policy, and by
    # consensus in a block. Bitcoin is first brought past the locktime, so
    # the script, not finality, is what each check refuses; the coins are
    # unspent there, so it is not missing inputs either.
    level(a, tip)
    verdict_a = a.rpc.testmempoolaccept([sweep])[0]
    assert not verdict_a["allowed"], verdict_a
    assert verdict_a["reject-reason"] == (
        "mempool-script-verify-flag-failed "
        "(Signature hash type missing or not understood)"
    ), verdict_a
    record_property(
        f"{shape}_bitcoin_unified_mempool_reject", verdict_a["reject-reason"]
    )
    bitcoin_tip = a.rpc.getbestblockhash()
    with pytest.raises(JSONRPCException) as rejected_block:
        a.rpc.generateblock(a.rpc.getnewaddress(), [sweep])
    block_error = rejected_block.value.error
    assert block_error["code"] == -25, block_error
    # Consensus has no hash-type policy: the signature simply does not
    # verify under the legacy digest, and the script ends false.
    assert block_error["message"].startswith(
        "TestBlockValidity failed: mandatory-script-verify-flag-failed "
        "(Script evaluated without error but finished with a false/empty top "
        "stack element), input "
    ), block_error
    record_property(f"{shape}_bitcoin_unified_block_reject", block_error["message"])
    assert a.rpc.getbestblockhash() == bitcoin_tip
    # The witness is all that keeps the chains apart: the legacy twin of the
    # same transaction is a valid, standard Bitcoin transaction. Not sent.
    verdict_twin = a.rpc.testmempoolaccept([twin])[0]
    assert verdict_twin["allowed"], verdict_twin
    assert verdict_twin["txid"] not in a.rpc.getrawmempool()
    record_property(f"{shape}_bitcoin_legacy_twin_allowed", verdict_twin["allowed"])

    # Mined on BTCB2: the coins are spent there, into the target.
    assert b.rpc.sendrawtransaction(sweep) == sweep_id
    b.generate_block(1, wait_for_mempool=sweep_id)
    mined = b.rpc.getrawtransaction(sweep_id, True, b.rpc.getbestblockhash())
    assert mined["confirmations"] == 1, mined
    assert mined["hex"] == sweep, (mined["hex"], sweep)
    assert mined["hash"] == hash256(bytes.fromhex(sweep))[::-1].hex(), mined
    assert (mined["hash"] == sweep_id) == (shape == "pkh"), mined
    assert [o["scriptPubKey"]["hex"] for o in mined["vout"]] == [target.hex()]
    # The bytes BTCB2 mined verify as a recorded sweep at the tip after
    # mining, where a restart would rebuild it.
    check_unified_recorded(
        tool, request, built, mined["hex"], finalized, b.rpc.getblockcount()
    )
    for txid, vout in prevouts:
        assert b.rpc.gettxout(txid, vout, False) is None
    assert b.rpc.gettxout(sweep_id, 0, False)["scriptPubKey"]["hex"] == target.hex()
    # Nothing moved on Bitcoin.
    for txid, vout in prevouts:
        assert a.rpc.gettxout(txid, vout, False) is not None
    assert a.rpc.gettxout(sweep_id, 0, False) is None
