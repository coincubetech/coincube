"""Ordinary S+B and S+F Claim transactions against both pinned chains.

This is consensus and core PSBT coverage with disposable regtest keys. It does
not grant GUI signing authority or replace physical BitBox/Ledger approval.
"""

import json
import os
import subprocess

import pytest

from fixtures import *
from test_framework.serializations import PSBT


def _coinbase_for_vault(node, address):
    block = node.rpc.generatetoaddress(1, address)[0]
    coinbase_id = node.rpc.getblock(block)["tx"][0]
    transaction = node.rpc.getrawtransaction(coinbase_id, True, block)
    vout = next(
        output["n"]
        for output in transaction["vout"]
        if output["scriptPubKey"].get("address") == address
    )
    return coinbase_id, vout, transaction["hex"]


def test_claim_pair_uses_distinct_chain_specific_inputs(two_chain, record_property):
    tool = os.getenv("CLAIM_REGTEST_TOOL_PATH")
    if not tool or not os.access(tool, os.X_OK):
        if os.getenv("BTCB2_HARNESS_REQUIRED") == "1":
            pytest.fail("CLAIM_REGTEST_TOOL_PATH must name the built Claim test bridge")
        pytest.skip("Build claim_regtest_vectors and set CLAIM_REGTEST_TOOL_PATH")

    bitcoin, fork = two_chain.legacy, two_chain.blake2b
    receive, _ = two_chain.desc.singlepath_descriptors()
    descriptor = bitcoin.rpc.getdescriptorinfo(str(receive))["descriptor"]
    bitcoin_address, fork_address = bitcoin.rpc.deriveaddresses(descriptor, [4, 5])
    bitcoin_id, bitcoin_vout, bitcoin_hex = _coinbase_for_vault(
        bitcoin, bitcoin_address
    )
    fork_id, fork_vout, fork_hex = _coinbase_for_vault(fork, fork_address)
    assert bitcoin_id != fork_id
    bitcoin.generate_block(99)
    fork.generate_block(99)
    assert bitcoin.rpc.gettxout(bitcoin_id, bitcoin_vout)["confirmations"] == 100
    assert fork.rpc.gettxout(fork_id, fork_vout)["confirmations"] == 100
    assert bitcoin.rpc.gettxout(fork_id, fork_vout) is None
    assert fork.rpc.gettxout(bitcoin_id, bitcoin_vout) is None

    shared = [
        {
            "previous": bitcoin.rpc.getrawtransaction(
                txid, False, two_chain.prefork_block_hashes[txid]
            ),
            "vout": vout,
            "index": index,
        }
        for index, (txid, vout, _) in enumerate(two_chain.prefork_outpoints)
    ]
    request = {
        "descriptor": str(two_chain.desc),
        "fork_marker": two_chain.fork_parent_hash,
        "coinbase_input": f"{bitcoin_id}:{bitcoin_vout}",
        "fork_coinbase_input": f"{fork_id}:{fork_vout}",
        "coins": shared
        + [
            {"previous": bitcoin_hex, "vout": bitcoin_vout, "index": 4},
            {"previous": fork_hex, "vout": fork_vout, "index": 5},
        ],
    }

    def vectors():
        completed = subprocess.run(
            [tool], input=json.dumps(request), text=True,
            capture_output=True, timeout=30, check=True,
        )
        return json.loads(completed.stdout)

    def sign(psbt):
        return two_chain.signer.sign_psbt(PSBT.from_base64(psbt), [0, 1]).to_base64()

    unsigned = vectors()
    expected_shared = {f"{txid}:{vout}" for txid, vout, _ in two_chain.prefork_outpoints}
    assert set(unsigned["shared_prevouts"]) == expected_shared
    request["signed_step1"] = sign(unsigned["step1_psbt"])
    request["signed_fork"] = sign(unsigned["fork_psbt"])
    signed = vectors()
    assert signed["step1_psbt"] == unsigned["step1_psbt"]
    assert signed["fork_psbt"] == unsigned["fork_psbt"]

    bitcoin_raw, fork_raw = signed["step1_raw"], signed["fork_raw"]
    bitcoin_decoded = bitcoin.rpc.decoderawtransaction(bitcoin_raw)
    fork_decoded = fork.rpc.decoderawtransaction(fork_raw)
    bitcoin_inputs = {f"{item['txid']}:{item['vout']}" for item in bitcoin_decoded["vin"]}
    fork_inputs = {f"{item['txid']}:{item['vout']}" for item in fork_decoded["vin"]}
    assert bitcoin_inputs == expected_shared | {request["coinbase_input"]}
    assert fork_inputs == expected_shared | {request["fork_coinbase_input"]}
    for decoded in (bitcoin_decoded, fork_decoded):
        assert len(decoded["vout"]) == 1
        assert decoded["vout"][0]["scriptPubKey"]["type"] == "witness_v0_scripthash"

    for own, other, raw in (
        (bitcoin, fork, bitcoin_raw),
        (fork, bitcoin, fork_raw),
    ):
        verdict = own.rpc.testmempoolaccept([raw])[0]
        rejected = other.rpc.testmempoolaccept([raw])[0]
        assert verdict["allowed"], verdict
        assert not rejected["allowed"] and rejected["reject-reason"] == "missing-inputs", rejected

    # Reversed order is safe: the BTCB2 transaction can confirm first because
    # its own marker input exists only there, even while Bitcoin is unconfirmed.
    assert fork.rpc.sendrawtransaction(fork_raw) == signed["fork_txid"]
    fork.generate_block(1, wait_for_mempool=signed["fork_txid"])
    assert bitcoin.rpc.sendrawtransaction(bitcoin_raw) == signed["step1_txid"]
    bitcoin.generate_block(1, wait_for_mempool=signed["step1_txid"])
    record_property("ordinary_claim_bitcoin_txid", signed["step1_txid"])
    record_property("ordinary_claim_fork_txid", signed["fork_txid"])
