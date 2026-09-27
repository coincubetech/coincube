"""Headless production Claim panel with software signing and real node submission.

Account/backend routing is explicitly synthetic. This does not prove rendered
UI, PIN unlock, hardware signing, or production daemon admission.
"""
import json
import os
import queue
import re
import base58
import threading
import time
import subprocess
import tempfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.request import urlopen
from urllib.error import HTTPError

import pytest

from test_framework.btcb2 import TwoChainRegtest, missing_binaries
from test_framework.authproxy import JSONRPCException


class Child:
    def __init__(self, tool, home):
        home.mkdir()
        self.home = home
        self.proc = subprocess.Popen(
            [tool, "claim_gui_regtest_driver", "--ignored", "--nocapture", "--test-threads=1"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, env={**os.environ, "CLAIM_GUI_REGTEST_CHILD": "1",
                            "HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"),
                            "XDG_DATA_HOME": str(home / ".local/share"),
                            "XDG_CACHE_HOME": str(home / ".cache")})
        self.lines = queue.Queue()
        self.transcript = []
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        for line in self.proc.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def receive(self):
        deadline = time.monotonic() + 45
        while True:
            line = self.lines.get(timeout=max(0.01, deadline - time.monotonic()))
            assert line is not None, "GUI child exited:\n" + "".join(self.transcript)
            self.transcript.append(line)
            marker = "CLAIM_GUI_JSON:"
            if marker in line:
                return json.loads(line.split(marker, 1)[1])
            assert time.monotonic() < deadline, "GUI child timeout"

    def send(self, value):
        self.proc.stdin.write(json.dumps(value) + "\n")
        self.proc.stdin.flush()
        return self.receive()

    def close(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
        self.reader.join(timeout=5)
        self.proc.stdin.close()
        self.proc.stdout.close()


class Bridge:
    def __init__(self, harness):
        self.harness = harness
        self.preflights = []
        self.errors = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def reply(self, status, body, content_type="application/json"):
                if not isinstance(body, bytes):
                    body = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Cache-Control", "no-store")
                self.send_header("X-Coincube-Observation", "fresh")
                self.send_header("X-Cache", "BYPASS")
                self.end_headers()
                self.wfile.write(body)

            def route(self):
                prefix = "/api/v1/esplora/"
                assert self.path.startswith(prefix)
                assert self.headers.get("Authorization") is None
                network, mainnet, suffix = self.path[len(prefix):].split("/", 2)
                assert mainnet == "mainnet"
                if network == "bitcoin":
                    return harness.legacy, harness.electrs_legacy, suffix, "mainnet"
                assert network == "bitcoin-blake2b"
                return harness.blake2b, harness.electrs_blake2b, suffix, network

            def do_GET(self):
                try:
                    if self.path == "/api/v1/connect/networks/bitcoin-blake2b/anchor":
                        assert self.headers.get("Authorization") == "Bearer synthetic-regtest-only"
                        node = harness.blake2b.rpc
                        tip = node.getbestblockhash()
                        header = node.getblockheader(tip)
                        deployments = node.getdeploymentinfo(tip)
                        fork = deployments["blake2b"]
                        rdts = deployments["deployments"]["reduced_data"]
                        assert fork["active"] and rdts["active"] and rdts["type"] == "flagday"
                        # The extended header is direct post-fork activation evidence.
                        assert len(node.getblockheader(tip, False)) == 328
                        assert node.getbestblockhash() == tip
                        self.reply(200, {"success": True, "data": {
                            "network": "bitcoin-blake2b", "state": "available", "anchor": {
                                "tip_hash": tip, "tip_height": header["height"],
                                "tip_median_time_past": header["mediantime"], "observed_at": int(time.time()),
                                "observation": {"tip_height": header["height"], "fork": fork,
                                    "rdts": {"state": "flagday", "flagday": {
                                        k: rdts[k] for k in ("height", "expiry_time", "active")}}}}}})
                        return
                    _, indexer, suffix, _ = self.route()
                    try:
                        with urlopen(indexer.url + "/" + suffix, timeout=4) as response:
                            self.reply(response.status, response.read(262145), response.headers.get("Content-Type", "application/json"))
                    except HTTPError as error:
                        self.reply(error.code, error.read(262145))
                except Exception as error:
                    owner.errors.append(repr(error))
                    self.reply(503, {"error": "bridge-refused"})

            def do_POST(self):
                try:
                    node, _, suffix, network = self.route()
                    assert suffix == "tx/preflight"
                    length = int(self.headers["Content-Length"])
                    assert 0 < length <= 262144
                    request = json.loads(self.rfile.read(length))
                    tip = node.rpc.getbestblockhash()
                    assert tip == request["tip_hash"]
                    result = node.rpc.testmempoolaccept([request["transaction"]])[0]
                    decoded = node.rpc.decoderawtransaction(request["transaction"])
                    assert node.rpc.getbestblockhash() == tip
                    owner.preflights.append(result)
                    verdict = {"txid": decoded["txid"], "wtxid": decoded["hash"],
                               "tip_hash": tip, "observed_at": int(time.time()), "allowed": result["allowed"]}
                    if not result["allowed"]:
                        verdict["reject_reason"] = result["reject-reason"]
                    self.reply(200, {"success": True, "data": {
                        "network": network, "state": "available", "result": verdict}})
                except Exception as error:
                    owner.errors.append(repr(error))
                    self.reply(503, {"error": "bridge-refused"})

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def test_gui_claims_both_chains_and_recovers_remined_bitcoin(tmp_path, record_property):
    tool = os.getenv("CLAIM_GUI_REGTEST_TEST_PATH")
    missing = missing_binaries()
    if not tool or not os.access(tool, os.X_OK) or missing:
        message = "Build GUI lib tests with regtest-harness and set CLAIM_GUI_REGTEST_TEST_PATH plus pinned node/indexer paths"
        if os.getenv("BTCB2_HARNESS_REQUIRED") == "1":
            pytest.fail(message)
        pytest.skip(message)
    if os.getenv("TEST_DIR"):
        Path(os.environ["TEST_DIR"]).mkdir(parents=True, exist_ok=True)
        tmp_path = Path(tempfile.mkdtemp(prefix="btcb2-gui-", dir=os.environ["TEST_DIR"]))
    child = Child(tool, tmp_path / "gui-home")
    harness = bridge = None
    try:
        descriptor = child.receive()
        assert descriptor["event"] == "descriptor"
        # Logical mainnet admission uses xpubs; regtest RPC requires tpubs.
        # Only the four-byte network prefix changes. The child checks the
        # funded script against its own independently derived script exactly.
        def testnet_public_key(match):
            payload = base58.b58decode_check(match.group())
            assert len(payload) == 78 and payload[:4] == bytes.fromhex("0488b21e")
            return base58.b58encode_check(bytes.fromhex("043587cf") + payload[4:]).decode()
        public_descriptor = re.sub(r"xpub[1-9A-HJ-NP-Za-km-z]+", testnet_public_key,
                                   descriptor["descriptor"].split("#")[0])
        harness = TwoChainRegtest(str(tmp_path / "nodes"), public_descriptor=public_descriptor)
        harness.setup()
        bridge = Bridge(harness)
        root = tmp_path / "gui"
        root.mkdir()
        txid, vout, _ = harness.prefork_outpoints[0]
        node = harness.legacy
        block = harness.prefork_block_hashes[txid]
        initial = {"root": str(root), "bridge": bridge.url,
                            "rpc": f"127.0.0.1:{node.rpcport}",
                            "cookie": Path(node.node_rpc.cookie_path).read_text().strip(),
                            "fork_rpc": f"127.0.0.1:{harness.blake2b.rpcport}",
                            "fork_cookie": Path(harness.blake2b.node_rpc.cookie_path).read_text().strip(),
                            "fork_tip_height": harness.blake2b.rpc.getblockcount(),
                            "previous": node.rpc.getrawtransaction(txid, False, block), "vout": vout,
                            "coin_height": node.rpc.getblockheader(block)["height"],
                            "tip_height": node.rpc.getblockcount()}
        ready = child.send(initial)
        assert ready["event"] == "ready", ready
        for action, stage in (("build", "plan"), ("sign", "sign"), ("open_signer", "sign"), ("hot_sign", "review")):
            result = child.send({"command": action})
            assert result["stage"] == stage, result
            assert result["submission_calls"] == 0 and result["submitted"] is None
        assert result["journal"] is not None
        assert bridge.preflights and all(v["allowed"] for v in bridge.preflights)
        result = child.send({"command": "confirm"})
        assert result["stage"] == "track" and result["submission_calls"] == 1, result
        submitted = result["submitted"]
        actual = node.rpc.getrawtransaction(submitted["txid"], True)
        assert actual["hex"] == submitted["raw"] and actual["hash"] == submitted["wtxid"]
        assert submitted["txid"] in node.rpc.getrawmempool()
        fork_rejection = harness.blake2b.rpc.testmempoolaccept([submitted["raw"]])[0]
        assert not fork_rejection["allowed"], fork_rejection
        assert fork_rejection.get("reject-reason"), fork_rejection
        record_property("step1_fork_rejection", fork_rejection["reject-reason"])
        # Claim uses the current Bitcoin height for anti-fee-sniping locktime.
        # Make that locktime final on the fork so it cannot mask the poison rule.
        lock_height = actual["locktime"]
        assert 0 <= lock_height < 500_000_000, actual
        advance = max(0, lock_height - harness.blake2b.rpc.getblockcount())
        if advance:
            harness.blake2b.generate_block(advance)
            harness.electrs_blake2b.wait_for_tip(harness.blake2b.rpc.getbestblockhash())
        fork_tip = harness.blake2b.rpc.getbestblockhash()
        with pytest.raises(JSONRPCException) as invalid_block:
            harness.blake2b.rpc.generateblock(harness.blake2b.rpc.getnewaddress(), [submitted["raw"]])
        block_error = invalid_block.value.error
        assert block_error["code"] == -25, block_error
        assert block_error["message"].startswith("TestBlockValidity failed: bad-txns-vout-script-toolarge,"), block_error
        assert submitted["txid"] in block_error["message"], block_error
        assert harness.blake2b.rpc.getbestblockhash() == fork_tip
        record_property("step1_fork_consensus_rejection", block_error)
        again = child.send({"command": "confirm"})
        assert again["submission_calls"] == 1, again
        for depth, added in ((1, 1), (5, 4)):
            node.generate_block(added, wait_for_mempool=submitted["txid"] if depth == 1 else None)
            harness.electrs_legacy.wait_for_tip(node.rpc.getbestblockhash())
            shallow = child.send({"command": "refresh"})
            assert shallow["tracking"] == {
                "status": f"Some(Observation(WaitingForDepth {{ confirmations: {depth} }}))",
                "busy": False, "error": None}, shallow
            assert shallow["submission_calls"] == 1
            journal = shallow["journal"]
            refused = child.send({"command": "fork_refuse_early"})
            assert "six Bitcoin confirmations" in refused["error"], refused
            assert refused["bitcoin_submission_calls"] == 1 and refused["fork_submission_calls"] == 0
            unchanged = child.send({"command": "refresh"})
            assert unchanged["journal"] == journal and unchanged["tracking"] == shallow["tracking"]
        node.generate_block(1)
        harness.electrs_legacy.wait_for_tip(node.rpc.getbestblockhash())
        tracked = child.send({"command": "refresh"})
        assert tracked["stage"] == "track" and tracked["submission_calls"] == 1
        assert tracked["tracking"] == {"status": "Some(Observation(ObservationsEligibleForPreflight))",
                                       "busy": False, "error": None}, tracked
        inclusion = tracked["journal"]["plan"]["previous_confirmation"]
        assert node.rpc.getblockhash(inclusion["height"]) == inclusion["hash"]
        assert node.rpc.getblockcount() - inclusion["height"] + 1 >= 6
        assert submitted["txid"] in node.rpc.getblock(inclusion["hash"])["tx"]
        opened = child.send({"command": "fork_open"})
        assert opened["submission_calls"] == 0 and opened["fork"]["error"] is None, opened
        child.send({"command": "fork_open_signer"})
        signed = child.send({"command": "fork_hot_sign"})
        assert signed["fork"]["review"] and not signed["fork"]["busy"] and signed["fork"]["error"] is None, signed
        assert signed["submission_calls"] == 0 and signed["submitted"] is None
        fork_result = child.send({"command": "fork_confirm"})
        assert fork_result["submission_calls"] == 1 and fork_result["fork"]["error"] is None, fork_result
        fork_tx = fork_result["submitted"]
        fork_node = harness.blake2b
        fork_actual = fork_node.rpc.getrawtransaction(fork_tx["txid"], True)
        assert fork_actual["hex"] == fork_tx["raw"] and fork_actual["hash"] == fork_tx["wtxid"]
        bitcoin_rejection = node.rpc.testmempoolaccept([fork_tx["raw"]])[0]
        assert not bitcoin_rejection["allowed"], bitcoin_rejection
        assert bitcoin_rejection.get("reject-reason"), bitcoin_rejection
        record_property("step2_bitcoin_rejection", bitcoin_rejection["reject-reason"])
        repeated = child.send({"command": "fork_confirm"})
        assert repeated["submission_calls"] == 1
        fork_node.generate_block(1, wait_for_mempool=fork_tx["txid"])
        fork_block = fork_node.rpc.getbestblockhash()
        fork_height = fork_node.rpc.getblockcount()
        harness.electrs_blake2b.wait_for_tip(fork_block)
        completed = child.send({"command": "fork_refresh"})
        assert completed["fork"]["tracking"]["saved"] is True, completed
        assert completed["fork"]["tracking"]["transaction"].startswith("Confirmed"), completed
        assert completed["fork"]["error"] is None and not completed["fork"]["busy"]
        assert fork_tx["txid"] in fork_node.rpc.getblock(fork_block)["tx"]
        for settings, cube_id in zip(completed["settings"], ("bitcoin-cube", "fork-cube")):
            cube = next(c for c in settings["cubes"] if c["id"] == cube_id)
            assert cube["split_completed_at_height"] == fork_height
            assert cube["split_completion_txid"] == fork_tx["txid"]
        preserved = {key: completed["journal"].get(key) for key in
                     ("fork_sweep", "fork_submission", "bitcoin_transaction", "bitcoin_attempts")}
        old_history = completed["journal"].get("inclusion_history", [])

        # Terminate the actual GUI child, keeping only its on-disk state. A
        # different process reloads without a signer, with freshly read node
        # transactions supplying its wallet adapter's historical reads.
        def durable_files():
            return {str(path.relative_to(root)): path.read_bytes()
                    for pattern in ("settings.json", "intent.json") for path in root.rglob(pattern)}
        before_restart = durable_files()
        assert len(before_restart) == 3, before_restart.keys()
        old_pid = child.proc.pid
        assert list(child.home.iterdir()) == []
        child.proc.kill()
        child.proc.wait(timeout=10)
        child.close()
        old_log = tmp_path / "before-restart" / "log"
        old_log.parent.mkdir()
        old_log.write_text("".join(child.transcript))
        child = Child(tool, tmp_path / "gui-home-restart")
        assert child.proc.pid != old_pid
        assert child.receive() == descriptor
        restart = child.send({**initial, "resume": True,
                              "tip_height": node.rpc.getblockcount(),
                              "fork_tip_height": fork_node.rpc.getblockcount(),
                              "bitcoin_recorded_raw": node.rpc.getrawtransaction(submitted["txid"], False, inclusion["hash"]),
                              "fork_recorded_raw": fork_node.rpc.getrawtransaction(fork_tx["txid"], False, fork_block)})
        assert restart == {"event": "ready", "resumed": True, "signer_available": False,
                           "bitcoin_submission_calls": 0, "fork_submission_calls": 0}, restart
        assert durable_files() == before_restart, "restart changed journal or paired completion settings"
        resumed_bitcoin = child.send({"command": "refresh"})
        assert resumed_bitcoin["tracking"] == {"status": "Some(Observation(ObservationsEligibleForPreflight))",
                                               "busy": False, "error": None}, resumed_bitcoin
        assert resumed_bitcoin["submission_calls"] == 0
        assert resumed_bitcoin["journal"] == completed["journal"]
        resumed_fork = child.send({"command": "fork_open"})
        assert resumed_fork["fork"]["outcome"] is not None and resumed_fork["submission_calls"] == 0
        resumed_completion = child.send({"command": "fork_refresh"})
        assert resumed_completion["fork"]["tracking"]["saved"] is True, resumed_completion
        assert resumed_completion["submission_calls"] == 0
        assert {key: resumed_completion["journal"].get(key) for key in preserved} == preserved
        record_property("restart_new_pid", child.proc.pid)
        record_property("restart_loaded_signer", False)

        # Invalidate only Bitcoin. The fork remains canonical and confirmed.
        # Present a complete competing branch to the pinned indexer. A bare
        # invalidateblock leaves a regtest-only shorter tip which this electrs
        # pin cannot index (schema.rs asserts on an empty replacement branch).
        # Keep its existing DB; this tests rollback + indexing the new branch,
        # not uninterrupted service during that synthetic transition.
        old_tip_height = node.rpc.getblockcount()
        harness.electrs_legacy.stop()
        node.invalidate_block(inclusion["hash"])
        node.generate_empty_blocks(old_tip_height - node.rpc.getblockcount() + 1)
        harness.electrs_legacy.startup()
        assert node.rpc.getblockheader(inclusion["hash"])["confirmations"] == -1
        assert fork_node.rpc.getbestblockhash() == fork_block
        assert fork_node.rpc.getrawtransaction(fork_tx["txid"], True, fork_block)["confirmations"] >= 1
        harness.electrs_legacy.wait_for_tip(node.rpc.getbestblockhash())
        reorged = child.send({"command": "fork_refresh"})
        assert reorged["fork"]["tracking"]["status"] == "Observation(Reorged)", reorged
        assert reorged["fork"]["tracking"]["transaction"].startswith("Confirmed")
        assert reorged["fork"]["tracking"]["saved"] is False
        assert reorged["fork"]["error"] is None and not reorged["fork"]["busy"]
        assert reorged["submission_calls"] == 0
        for settings in reorged["settings"]:
            for cube in settings["cubes"]:
                assert cube.get("split_completed_at_height") is None
                assert cube.get("split_completion_txid") is None
        assert reorged["journal"]["plan"]["previous_confirmation"] == inclusion
        assert {key: reorged["journal"].get(key) for key in preserved} == preserved
        assert reorged["journal"].get("inclusion_history", []) == old_history

        # A new coinbase destination ensures a different block even if time is unchanged.
        assert submitted["txid"] in node.rpc.getrawmempool()
        new_block = node.rpc.generatetoaddress(1, node.rpc.getnewaddress())[0]
        assert new_block != inclusion["hash"]
        new_inclusion = {"height": node.rpc.getblockcount(), "hash": new_block}
        new_actual = node.rpc.getrawtransaction(submitted["txid"], True, new_block)
        assert new_actual["hex"] == submitted["raw"] and new_actual["hash"] == submitted["wtxid"]
        harness.electrs_legacy.wait_for_tip(new_block)
        returned = child.send({"command": "bitcoin_return"})
        assert returned["tracking"]["status"] == "Some(Observation(Reorged))", returned
        review = child.send({"command": "review_reconfirmation"})
        assert review["reconfirmation_review"] == {"previous": inclusion, "confirmed": new_inclusion}, review
        assert review["journal"]["plan"]["previous_confirmation"] == inclusion
        confirmed = child.send({"command": "confirm_reconfirmation"})
        assert confirmed["reconfirmation_review"] is None
        assert confirmed["tracking"] == {"status": "Some(Observation(WaitingForDepth { confirmations: 1 }))",
                                         "busy": False, "error": None}, confirmed
        assert confirmed["journal"]["plan"]["previous_confirmation"] == new_inclusion
        assert confirmed["journal"]["inclusion_history"] == old_history + [{"previous": inclusion, "confirmed": new_inclusion}]
        assert {key: confirmed["journal"].get(key) for key in preserved} == preserved
        duplicate = child.send({"command": "confirm_reconfirmation"})
        assert duplicate["journal"] == confirmed["journal"] and duplicate["submission_calls"] == 0
        node.generate_block(5)
        harness.electrs_legacy.wait_for_tip(node.rpc.getbestblockhash())
        recovered = child.send({"command": "refresh"})
        assert recovered["tracking"]["status"] == "Some(Observation(ObservationsEligibleForPreflight))", recovered
        assert recovered["submission_calls"] == 0
        # Reopening the fork loader must recover its recorded transaction, not sign again.
        reopened = child.send({"command": "fork_open"})
        assert reopened["fork"]["outcome"] is not None and reopened["submission_calls"] == 0
        restored = child.send({"command": "fork_refresh"})
        assert restored["fork"]["tracking"]["saved"] is True, restored
        assert restored["submission_calls"] == 0
        for settings in restored["settings"]:
            for cube in settings["cubes"]:
                assert cube["split_completed_at_height"] == fork_height
                assert cube["split_completion_txid"] == fork_tx["txid"]
        record_property("fork_txid", fork_tx["txid"])
        record_property("fork_wtxid", fork_tx["wtxid"])
        record_property("bitcoin_reconfirmation", {"previous": inclusion, "confirmed": new_inclusion})
        assert not bridge.errors, bridge.errors
        record_property("step1_txid", submitted["txid"])
        record_property("step1_wtxid", submitted["wtxid"])
        record_property("gui_panel_submission_calls", tracked["submission_calls"])
        harness.assert_home_sandbox_untouched()
        assert list(child.home.iterdir()) == [], "GUI used a default HOME/XDG path"
        child.proc.stdin.write('{"command":"quit"}\n')
        child.proc.stdin.flush()
        assert child.proc.wait(timeout=10) == 0
    finally:
        child.close()
        (tmp_path / "log").write_text("".join(child.transcript))
        if bridge:
            bridge.close()
        if harness:
            harness.cleanup()
