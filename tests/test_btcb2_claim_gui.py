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


def test_gui_software_signs_reviews_and_submits_to_bitcoin(tmp_path, record_property):
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
        ready = child.send({"root": str(root), "bridge": bridge.url,
                            "rpc": f"127.0.0.1:{node.rpcport}",
                            "cookie": Path(node.node_rpc.cookie_path).read_text().strip(),
                            "previous": node.rpc.getrawtransaction(txid, False, block), "vout": vout,
                            "coin_height": node.rpc.getblockheader(block)["height"],
                            "tip_height": node.rpc.getblockcount()})
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
        assert not harness.blake2b.rpc.testmempoolaccept([submitted["raw"]])[0]["allowed"]
        again = child.send({"command": "confirm"})
        assert again["submission_calls"] == 1, again
        node.generate_block(6, wait_for_mempool=submitted["txid"])
        harness.electrs_legacy.wait_for_tip(node.rpc.getbestblockhash())
        tracked = child.send({"command": "refresh"})
        assert tracked["stage"] == "track" and tracked["submission_calls"] == 1
        assert tracked["tracking"] == {"status": "Some(Observation(ObservationsEligibleForPreflight))",
                                       "busy": False, "error": None}, tracked
        inclusion = tracked["journal"]["plan"]["previous_confirmation"]
        assert node.rpc.getblockhash(inclusion["height"]) == inclusion["hash"]
        assert node.rpc.getblockcount() - inclusion["height"] + 1 >= 6
        assert submitted["txid"] in node.rpc.getblock(inclusion["hash"])["tx"]
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
