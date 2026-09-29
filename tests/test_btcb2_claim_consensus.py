"""Production Claim construction/finalization against both pinned regtest nodes.

This mutates the shared pre-fork coins, so it deliberately owns a separate
module-scoped two_chain fixture. It is consensus integration coverage, not GUI
or signer-device end-to-end acceptance.
"""
import json
import os
import subprocess

import pytest

from fixtures import *
from test_framework.serializations import PSBT
from test_framework.authproxy import JSONRPCException


def test_owned_claim_poison_and_fork_sweep_consensus(two_chain, record_property):
    tool = os.getenv("CLAIM_REGTEST_TOOL_PATH")
    if not tool or not os.access(tool, os.X_OK):
        if os.getenv("BTCB2_HARNESS_REQUIRED") == "1":
            pytest.fail("CLAIM_REGTEST_TOOL_PATH must name the built Claim test bridge")
        pytest.skip("Build coincube-core example claim_regtest_vectors and set CLAIM_REGTEST_TOOL_PATH")
    a, b = two_chain.legacy, two_chain.blake2b
    request = {
        "descriptor": str(two_chain.desc),
        "fork_marker": two_chain.fork_parent_hash,
        "coins": [
            {"previous": a.rpc.getrawtransaction(txid, False, two_chain.prefork_block_hashes[txid]),
             "vout": vout, "index": index}
            for index, (txid, vout, _) in enumerate(two_chain.prefork_outpoints)
        ],
    }

    def vectors():
        result = subprocess.run([tool], input=json.dumps(request), text=True,
                                capture_output=True, timeout=30, check=True)
        return json.loads(result.stdout)

    def sign(psbt):
        return two_chain.signer.sign_psbt(PSBT.from_base64(psbt), [0, 1]).to_base64()

    request["signed_step1"] = sign(vectors()["step1_psbt"])
    built = vectors()
    step1, step1_id = built["step1_raw"], built["step1_txid"]
    verdict_a = a.rpc.testmempoolaccept([step1])[0]
    verdict_b = b.rpc.testmempoolaccept([step1])[0]
    assert verdict_a["allowed"], verdict_a
    assert not verdict_b["allowed"], verdict_b
    assert verdict_b["reject-reason"] == "scriptpubkey", verdict_b
    record_property("fork_poison_policy_reject_reason", verdict_b["reject-reason"])
    fork_tip = b.rpc.getbestblockhash()
    with pytest.raises(JSONRPCException) as rejected_block:
        b.rpc.generateblock(b.rpc.getnewaddress(), [step1])
    block_error = rejected_block.value.error
    assert block_error["code"] == -25, block_error
    assert block_error["message"].startswith(
        "TestBlockValidity failed: bad-txns-vout-script-toolarge,"
    ), block_error
    assert step1_id in block_error["message"], block_error
    record_property("fork_poison_block_reject_reason", block_error)
    assert b.rpc.getbestblockhash() == fork_tip
    assert a.rpc.sendrawtransaction(step1) == step1_id
    source_height = a.rpc.getblockcount() + 1
    a.generate_block(6, wait_for_mempool=step1_id)
    source_block = a.rpc.getblockhash(source_height)
    status = a.rpc.getrawtransaction(step1_id, True, source_block)
    assert status["confirmations"] >= 6

    # The same original inputs now fund the owned fork destination, with two
    # ordinary legacy signatures. Production finalization verifies every one.
    request["signed_fork"] = sign(built["fork_psbt"])
    built = vectors()
    sweep, sweep_id = built["fork_raw"], built["fork_txid"]
    assert b.rpc.testmempoolaccept([sweep])[0]["allowed"]
    rejected = a.rpc.testmempoolaccept([sweep])[0]
    assert not rejected["allowed"], rejected
    assert rejected["reject-reason"] == "missing-inputs", rejected
    record_property("bitcoin_fork_sweep_reject_reason", rejected["reject-reason"])
    assert b.rpc.sendrawtransaction(sweep) == sweep_id
    b.generate_block(1, wait_for_mempool=sweep_id)
    fork_block = b.rpc.getbestblockhash()
    assert b.rpc.getrawtransaction(sweep_id, True, fork_block)["confirmations"] >= 1
    assert any(a.rpc.gettxout(step1_id, i) for i in range(2))
    assert b.rpc.gettxout(sweep_id, 0) is not None

    # Positive reorg evidence: step 1 loses confirmation while the fork sweep
    # remains confirmed. The GUI must treat this as requiring fresh recovery.
    a.rpc.invalidateblock(status["blockhash"])
    assert a.rpc.getrawtransaction(step1_id, True, source_block).get("confirmations", 0) <= 0
    assert b.rpc.getrawtransaction(sweep_id, True, fork_block)["confirmations"] >= 1
