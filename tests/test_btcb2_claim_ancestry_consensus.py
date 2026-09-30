"""Owned ancestry construction against both nodes, without mainnet proof admission.

A separate module-scoped fixture isolates the spent shared inputs. This proves
node consensus and production construction/finalization, not automatic input
preference, canonical mainnet history, GUI consent, or submission authorization.
"""
import json
import os
import subprocess

import pytest

from fixtures import *
from test_framework.authproxy import JSONRPCException
from test_framework.serializations import PSBT


def test_ancestry_input_maturity_and_excluded_fork_sweep(two_chain, record_property):
    tool = os.getenv("CLAIM_REGTEST_TOOL_PATH")
    if not tool or not os.access(tool, os.X_OK):
        if os.getenv("BTCB2_HARNESS_REQUIRED") == "1":
            pytest.fail("CLAIM_REGTEST_TOOL_PATH must name the built Claim test bridge")
        pytest.skip("Build claim_regtest_vectors and set CLAIM_REGTEST_TOOL_PATH")
    a, b = two_chain.legacy, two_chain.blake2b
    root_block = a.rpc.generatetoaddress(1, two_chain.vault_addresses[3])[0]
    root_height = a.rpc.getblockcount()
    root_id = a.rpc.getblock(root_block)["tx"][0]
    root = a.rpc.getrawtransaction(root_id, True, root_block)
    vout = next(o["n"] for o in root["vout"]
                if o["scriptPubKey"].get("address") == two_chain.vault_addresses[3])
    b.generate_block(root_height - b.rpc.getblockcount())
    fork_root = b.rpc.getblock(b.rpc.getblockhash(root_height))["tx"][0]
    assert fork_root != root_id
    assert b.rpc.gettxout(root_id, vout) is None
    request = {
        "descriptor": str(two_chain.desc),
        "fork_marker": two_chain.fork_parent_hash,
        "coinbase_input": f"{root_id}:{vout}",
        "coins": [
            {"previous": a.rpc.getrawtransaction(txid, False, two_chain.prefork_block_hashes[txid]),
             "vout": output, "index": index}
            for index, (txid, output, _) in enumerate(two_chain.prefork_outpoints)
        ] + [{"previous": root["hex"], "vout": vout, "index": 3}],
    }

    def vectors():
        result = subprocess.run([tool], input=json.dumps(request), text=True,
                                capture_output=True, timeout=30, check=True)
        return json.loads(result.stdout)

    def sign(psbt):
        return two_chain.signer.sign_psbt(PSBT.from_base64(psbt), [0, 1]).to_base64()

    request["signed_step1"] = sign(vectors()["step1_psbt"])
    built = vectors()
    raw, txid = built["step1_raw"], built["step1_txid"]
    decoded = a.rpc.decoderawtransaction(raw)
    assert len(decoded["vout"]) == 1
    assert decoded["vout"][0]["scriptPubKey"]["type"] == "witness_v0_scripthash"
    a.generate_block(98)
    assert a.rpc.gettxout(root_id, vout)["confirmations"] == 99
    immature = a.rpc.testmempoolaccept([raw])[0]
    assert not immature["allowed"], immature
    assert immature["reject-reason"] == "bad-txns-premature-spend-of-coinbase", immature
    record_property("coinbase_99_confirmations_rejected", immature["reject-reason"])
    a.generate_block(1)
    assert a.rpc.gettxout(root_id, vout)["confirmations"] == 100
    assert a.rpc.testmempoolaccept([raw])[0]["allowed"]
    rejected = b.rpc.testmempoolaccept([raw])[0]
    assert not rejected["allowed"] and rejected["reject-reason"] == "missing-inputs", rejected
    fork_tip = b.rpc.getbestblockhash()
    with pytest.raises(JSONRPCException) as failed_block:
        b.rpc.generateblock(b.rpc.getnewaddress(), [raw])
    assert failed_block.value.error["code"] == -25
    assert "bad-txns-inputs-missingorspent" in failed_block.value.error["message"]
    assert b.rpc.getbestblockhash() == fork_tip
    record_property("ancestry_fork_block_rejection", failed_block.value.error)
    assert a.rpc.sendrawtransaction(raw) == txid
    a.generate_block(6, wait_for_mempool=txid)
    assert a.rpc.gettxout(txid, 0)["confirmations"] == 6

    request["signed_fork"] = sign(built["fork_psbt"])
    built = vectors()
    sweep, sweep_id = built["fork_raw"], built["fork_txid"]
    inputs = {(i["txid"], i["vout"]) for i in b.rpc.decoderawtransaction(sweep)["vin"]}
    assert inputs == {(tx, output) for tx, output, _ in two_chain.prefork_outpoints}
    assert (root_id, vout) not in inputs
    assert b.rpc.testmempoolaccept([sweep])[0]["allowed"]
    rejected = a.rpc.testmempoolaccept([sweep])[0]
    assert not rejected["allowed"] and rejected["reject-reason"] == "missing-inputs", rejected
    assert b.rpc.sendrawtransaction(sweep) == sweep_id
    b.generate_block(1, wait_for_mempool=sweep_id)
    assert b.rpc.gettxout(sweep_id, 0) is not None
    assert a.rpc.gettxout(txid, 0) is not None
    record_property("ancestry_step1_txid", txid)
    record_property("ancestry_fork_sweep_txid", sweep_id)
