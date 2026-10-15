"""Ordinary S+B and S+F Claim transactions against both pinned chains.

This is consensus and core PSBT coverage with disposable regtest keys. It does
not grant GUI signing authority or replace physical BitBox/Ledger approval.
"""

import json
import os
import subprocess
import tempfile
from decimal import Decimal

import pytest

from fixtures import *
from test_framework.serializations import PSBT
from test_framework.btcb2 import TwoChainRegtest, missing_binaries


@pytest.fixture
def two_chain(test_base_dir):
    """Each consensus case mutates shared UTXOs, so isolate its node pair."""
    missing = missing_binaries()
    if missing:
        message = f"BTCB2 harness binaries unavailable: {', '.join(missing)}"
        if os.getenv("BTCB2_HARNESS_REQUIRED") == "1":
            pytest.fail(message)
        pytest.skip(message)
    directory = tempfile.mkdtemp(prefix="btcb2_spent_pair-", dir=test_base_dir)
    harness = TwoChainRegtest(directory)
    try:
        harness.setup()
        yield harness
    finally:
        harness.cleanup()


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
    # A confirmation on either chain can be reorganized out without making
    # the other chain's exact signed bytes valid there: the foreign marker
    # remains absent, independent of which shared inputs become unspent.
    for own, other_raw in ((bitcoin, fork_raw), (fork, bitcoin_raw)):
        confirmed = own.rpc.getbestblockhash()
        own.rpc.invalidateblock(confirmed)
        rejected = own.rpc.testmempoolaccept([other_raw])[0]
        assert not rejected["allowed"], rejected
        assert rejected["reject-reason"] == "missing-inputs", rejected
        own.rpc.reconsiderblock(confirmed)
        assert own.rpc.getbestblockhash() == confirmed
        # Remove the transaction's confirmation and five successors as a
        # deeper reorg. The shared outpoints may become unspent, but the
        # opposite chain's funding coin still does not exist here.
        own.generate_block(5)
        six_deep_tip = own.rpc.getbestblockhash()
        first_confirmation = own.rpc.getblockhash(own.rpc.getblockcount() - 5)
        own.rpc.invalidateblock(first_confirmation)
        rejected = own.rpc.testmempoolaccept([other_raw])[0]
        assert not rejected["allowed"], rejected
        assert rejected["reject-reason"] == "missing-inputs", rejected
        own.rpc.reconsiderblock(first_confirmation)
        assert own.rpc.getbestblockhash() == six_deep_tip
    record_property("ordinary_claim_bitcoin_txid", signed["step1_txid"])
    record_property("ordinary_claim_fork_txid", signed["fork_txid"])


@pytest.mark.parametrize("spent_chain", ["bitcoin", "btcb2"])
def test_spent_funding_coin_refuses_the_saved_pair(two_chain, spent_chain):
    """A saved pair is unusable when either chain-specific input is spent."""
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
    bitcoin.generate_block(99)
    fork.generate_block(99)
    request = {
        "descriptor": str(two_chain.desc),
        "fork_marker": two_chain.fork_parent_hash,
        "coinbase_input": f"{bitcoin_id}:{bitcoin_vout}",
        "fork_coinbase_input": f"{fork_id}:{fork_vout}",
        "coins": [
            {
                "previous": bitcoin.rpc.getrawtransaction(
                    txid, False, two_chain.prefork_block_hashes[txid]
                ),
                "vout": vout,
                "index": index,
            }
            for index, (txid, vout, _) in enumerate(two_chain.prefork_outpoints)
        ]
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

    unsigned = vectors()
    request["signed_step1"] = two_chain.signer.sign_psbt(
        PSBT.from_base64(unsigned["step1_psbt"]), [0, 1]
    ).to_base64()
    request["signed_fork"] = two_chain.signer.sign_psbt(
        PSBT.from_base64(unsigned["fork_psbt"]), [0, 1]
    ).to_base64()
    pair = vectors()
    bitcoin_verdict = bitcoin.rpc.testmempoolaccept([pair["step1_raw"]])[0]
    fork_verdict = fork.rpc.testmempoolaccept([pair["fork_raw"]])[0]
    assert bitcoin_verdict["allowed"], bitcoin_verdict
    assert fork_verdict["allowed"], fork_verdict

    if spent_chain == "bitcoin":
        side, other = bitcoin, fork
        unsigned_key, raw_key, marker_key = (
            "step1_psbt", "step1_raw", "coinbase_input"
        )
        marker_id, marker_vout = bitcoin_id, bitcoin_vout
    else:
        side, other = fork, bitcoin
        unsigned_key, raw_key, marker_key = (
            "fork_psbt", "fork_raw", "fork_coinbase_input"
        )
        marker_id, marker_vout = fork_id, fork_vout

    # Spend only B or F to the same Vault script, leaving shared inputs intact.
    # Reuse the builder's exact marker metadata so the disposable test signer
    # signs an ordinary PSBT rather than granting the node the Vault keys.
    marker_only = PSBT.from_base64(unsigned[unsigned_key])
    marker_index = next(
        index for index, txin in enumerate(marker_only.tx.vin)
        if f"{txin.prevout.hash:064x}:{txin.prevout.n}" == request[marker_key]
    )
    marker_only.tx.vin = [marker_only.tx.vin[marker_index]]
    marker_only.i = [marker_only.i[marker_index]]
    value = side.rpc.gettxout(marker_id, marker_vout)["value"]
    marker_only.tx.vout[0].nValue = int(Decimal(str(value)) * 100_000_000) - 1_000
    marker_only.g.map[0] = marker_only.tx.serialize_without_witness()
    signed_marker = two_chain.signer.sign_psbt(marker_only, [0, 1])
    finalized = side.rpc.finalizepsbt(signed_marker.to_base64())
    assert finalized["complete"], finalized
    marker_spend_id = side.rpc.sendrawtransaction(finalized["hex"])
    side.generate_block(1, wait_for_mempool=marker_spend_id)

    assert side.rpc.gettxout(marker_id, marker_vout) is None
    assert not side.rpc.testmempoolaccept([pair[raw_key]])[0]["allowed"]
    # The opposite chain's funding input remains live. That does not
    # authorize broadcasting its side alone: the application requires both.
    other_raw_key = "fork_raw" if spent_chain == "bitcoin" else "step1_raw"
    assert other.rpc.testmempoolaccept([pair[other_raw_key]])[0]["allowed"]
