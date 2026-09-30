"""Split step 1 (#568 B6a) construction/finalization against both pinned nodes.

Five foreign (non-Cube) wallets, one per supported source shape, are funded
before the fork, so every coin is shared history on both chains. For each, the
production `foreign_split` code builds and finalizes step 1 through the
`split_regtest_vectors` bridge; this module supplies the chain observations
and signs with disposable regtest keys. It checks node consensus only: no GUI,
journal, fee source, freshness proof, preflight or step 2 (B6b).

The first test needs no node. It fabricates the previous transactions and
runs the same bridge and signer, so the signing and finalization paths for all
five shapes are exercised wherever the bridge is built.
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
    sha256,
    sighash_all_witness,
)

SHAPES = ("pkh", "sh_wpkh", "wpkh", "wsh_multi", "wsh_sortedmulti")
# Receive index of the step-1 destination; the funded coins use index 0.
DESTINATION_INDEX = 5
FEERATE_VB = 2
FUND_AMOUNTS = (Decimal("0.01"), Decimal("0.02"))
SIGHASH_ALL = 1


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

    def sign(self, psbt_b64, explicit_all=False):
        """Add SIGHASH_ALL partial signatures, optionally with an explicit
        PSBT_IN_SIGHASH_TYPE of 0x01 on every input (#585 F1)."""
        psbt = PSBT.from_base64(psbt_b64)
        for i, psbt_in in enumerate(psbt.i):
            for pubkey, origin in psbt_in.map[PSBT_IN_BIP32_DERIVATION].items():
                raw_path = origin[4:]
                path = [
                    int.from_bytes(raw_path[j : j + 4], "little")
                    for j in range(0, len(raw_path), 4)
                ]
                for hd in self.signers():
                    privkey = coincurve.PrivateKey(hd.get_privkey_from_path(path))
                    if privkey.public_key.format() != pubkey:
                        continue
                    digest = sighash_all(psbt, i, pubkey)
                    signature = privkey.sign(digest, hasher=None) + bytes([SIGHASH_ALL])
                    psbt_in.map.setdefault(PSBT_IN_PARTIAL_SIG, {})[pubkey] = signature
            if explicit_all:
                psbt_in.map[PSBT_IN_SIGHASH_TYPE] = struct.pack("<I", SIGHASH_ALL)
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


def unsigned_tx_hex(psbt_b64):
    return PSBT.from_base64(psbt_b64).g.map[0].hex()


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


@pytest.mark.parametrize("shape", SHAPES)
def test_split_step1_bridge_offline(shape):
    """No node: fabricated prevouts, the real bridge and the real signer."""
    tool = split_tool()
    wallet = ForeignWallet(shape)
    block = {"height": 100, "hash": os.urandom(32).hex()}
    coins = [
        {"previous": fake_previous(wallet.script_pubkey(branch, 0), 1_000_000 * (branch + 1)),
         "vout": 0, "branch": ("external", "internal")[branch], "index": 0,
         "bitcoin_block": block, "btcb2_block": block}
        for branch in (0, 1)
    ]
    request = {
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
    built = run_bridge(tool, request)
    unsigned = from_binary(CTransaction, bytes.fromhex(unsigned_tx_hex(built["step1_psbt"])))
    assert unsigned.vout[1].scriptPubKey == wallet.script_pubkey(0, DESTINATION_INDEX)
    assert len(unsigned.vout[0].scriptPubKey) == 90 and unsigned.vout[0].scriptPubKey[0] == 0x6A
    assert built["fee"] == built["maximum_signed_vbytes"] * FEERATE_VB
    request["recorded_step1"] = unsigned_tx_hex(built["step1_psbt"])
    assert run_bridge(tool, request)["reconstructed_txid"] == built["unsigned_txid"]
    del request["recorded_step1"]
    sign_and_finalize(tool, wallet, request, built)

    # A coin at or after the fork height is refused, whatever the signatures.
    late = copy.deepcopy(request)
    late["coins"][0]["bitcoin_block"] = late["coins"][0]["btcb2_block"] = {
        "height": 200, "hash": block["hash"]}
    rejected = subprocess.run([tool], input=json.dumps(late), text=True,
                              capture_output=True, timeout=30)
    assert rejected.returncode != 0 and "PostFork" in rejected.stderr, rejected.stderr


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
            for shape, wallet in self.wallets.items():
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
                self.funded[shape] = (txid, scripts)
            self.legacy.generate_block(1, wait_for_mempool=txids)
            block = rpc.getbestblockhash()
            assert rpc.getblockcount() < fork - 1
            for shape, (txid, scripts) in list(self.funded.items()):
                decoded = rpc.getrawtransaction(txid, True, block)
                self.funded[shape] = [
                    (txid, out["n"], scripts[out["scriptPubKey"]["hex"]], 0, block)
                    for out in decoded["vout"]
                    if out["scriptPubKey"]["hex"] in scripts
                ]
                assert len(self.funded[shape]) == 2
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


@pytest.mark.parametrize("shape", SHAPES)
def test_split_step1_consensus(split_chains, shape, record_property):
    tool = split_tool()
    a, b = split_chains.legacy, split_chains.blake2b
    wallet = split_chains.wallets[shape]
    coins = []
    for txid, vout, branch, index, block in split_chains.funded[shape]:
        coins.append({
            "previous": a.rpc.getrawtransaction(txid, False, block),
            "vout": vout,
            "branch": ("external", "internal")[branch],
            "index": index,
            "bitcoin_block": observe(a, txid, block),
            "btcb2_block": observe(b, txid, block),
        })
    tip = a.rpc.getblockcount()
    request = {
        "external": wallet.descriptor(0),
        "internal": wallet.descriptor(1),
        "coins": coins,
        "fork_height": split_chains.activation_height,
        "fork_marker": split_chains.fork_parent_hash,
        "destination": DESTINATION_INDEX,
        "feerate_vb": FEERATE_VB,
        "locktime": tip,
        "bitcoin_tip_height": tip,
    }
    built = run_bridge(tool, request)
    request["recorded_step1"] = unsigned_tx_hex(built["step1_psbt"])
    assert run_bridge(tool, request)["reconstructed_txid"] == built["unsigned_txid"]
    del request["recorded_step1"]
    finalized = sign_and_finalize(tool, wallet, request, built)
    step1, step1_id = finalized["step1_raw"], finalized["step1_txid"]

    # Accepted by the Bitcoin node at no more than the construction's estimate.
    verdict_a = a.rpc.testmempoolaccept([step1])[0]
    assert verdict_a["allowed"], verdict_a
    assert verdict_a["txid"] == step1_id
    assert verdict_a["vsize"] == finalized["vsize"], (verdict_a, finalized)
    assert verdict_a["vsize"] <= built["maximum_signed_vbytes"], (verdict_a, built)
    record_property(f"{shape}_vsize", verdict_a["vsize"])
    record_property(f"{shape}_maximum_signed_vbytes", built["maximum_signed_vbytes"])

    # Refused by the BTCB2 node: by relay policy, and by consensus in a block.
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
    prevouts = [(c[0], c[1]) for c in split_chains.funded[shape]]
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
