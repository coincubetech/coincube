"""Exercise retired Claim refusal and paired funding proof on disposable nodes.

This does not exercise the paired Claim wizard, rendered UI or hardware signing.
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
from decimal import Decimal
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from urllib.request import Request, urlopen
from urllib.error import HTTPError

import pytest

from test_framework.btcb2 import TwoChainRegtest, missing_binaries


class Child:
    def __init__(self, tool, home, multisig=False):
        home.mkdir()
        self.home = home
        self.proc = subprocess.Popen(
            [tool, "claim_gui_regtest_driver", "--ignored", "--nocapture", "--test-threads=1"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, env={**os.environ, "CLAIM_GUI_REGTEST_CHILD": "1",
                            "CLAIM_GUI_REGTEST_MULTISIG": "1" if multisig else "0",
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
        self.submissions = []
        self.policy_node = None
        self.withhold_submission_reply = False
        self.stale_outspend = False
        self.errors = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def reply(self, status, body, content_type="application/json", observation="fresh"):
                if not isinstance(body, bytes):
                    body = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Cache-Control", "no-store")
                self.send_header("X-Coincube-Observation", observation)
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
                            self.reply(
                                response.status, response.read(262145),
                                response.headers.get("Content-Type", "application/json"),
                                "stale" if owner.stale_outspend and "/outspend/" in suffix else "fresh",
                            )
                    except HTTPError as error:
                        self.reply(error.code, error.read(262145))
                except Exception as error:
                    owner.errors.append(repr(error))
                    self.reply(503, {"error": "bridge-refused"})

            def do_POST(self):
                try:
                    node, _, suffix, network = self.route()
                    if network == "mainnet" and owner.policy_node is not None:
                        node = owner.policy_node
                    length = int(self.headers["Content-Length"])
                    assert 0 < length <= 262144
                    body = self.rfile.read(length)
                    if suffix == "tx":
                        raw = body.decode("ascii")
                        txid = node.rpc.sendrawtransaction(raw)
                        owner.submissions.append({"network": network, "raw": raw, "txid": txid})
                        if owner.withhold_submission_reply:
                            # The real node accepted every byte; let the client's
                            # 15-second deadline expire without acknowledgement.
                            time.sleep(16)
                            self.close_connection = True
                            return
                        self.reply(200, txid.encode("ascii"), "text/plain")
                        return
                    assert suffix == "tx/preflight"
                    request = json.loads(body)
                    tip = node.rpc.getbestblockhash()
                    assert tip == request["tip_hash"]
                    result = node.rpc.testmempoolaccept([request["transaction"]])[0]
                    decoded = node.rpc.decoderawtransaction(request["transaction"])
                    assert node.rpc.getbestblockhash() == tip
                    owner.preflights.append({"network": network, "result": result})
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


@pytest.mark.parametrize(
    ("route", "network"),
    [("bitcoin", "mainnet"), ("bitcoin-blake2b", "bitcoin-blake2b")],
)
def test_bridge_preflight_and_submission_use_the_selected_chain(route, network):
    """A paired GUI test must never preflight or submit on the other node."""
    class Rpc:
        def __init__(self, chain):
            self.chain = chain
            self.calls = []

        def getbestblockhash(self):
            return f"{self.chain}-tip"

        def testmempoolaccept(self, transactions):
            self.calls.append(("preflight", transactions[0]))
            return [{"allowed": True}]

        def decoderawtransaction(self, raw):
            assert raw == f"{self.chain}-raw"
            return {"txid": f"{self.chain}-txid", "hash": f"{self.chain}-wtxid"}

        def sendrawtransaction(self, raw):
            self.calls.append(("submit", raw))
            return f"{self.chain}-txid"

    bitcoin, fork = Rpc("bitcoin"), Rpc("btcb2")
    harness = SimpleNamespace(
        legacy=SimpleNamespace(rpc=bitcoin),
        blake2b=SimpleNamespace(rpc=fork),
        electrs_legacy=None,
        electrs_blake2b=None,
    )
    bridge = Bridge(harness)
    selected = bitcoin if route == "bitcoin" else fork
    other = fork if route == "bitcoin" else bitcoin
    prefix = f"{bridge.url}/api/v1/esplora/{route}/mainnet"
    try:
        raw = f"{selected.chain}-raw"
        body = json.dumps({"tip_hash": selected.getbestblockhash(), "transaction": raw}).encode()
        with urlopen(Request(prefix + "/tx/preflight", body), timeout=5) as response:
            verdict = json.load(response)["data"]
        assert verdict["network"] == network
        assert verdict["result"]["txid"] == f"{selected.chain}-txid"
        with urlopen(Request(prefix + "/tx", raw.encode()), timeout=5) as response:
            assert response.read().decode() == f"{selected.chain}-txid"
        assert selected.calls == [("preflight", raw), ("submit", raw)]
        assert other.calls == []
        assert bridge.preflights == [{"network": network, "result": {"allowed": True}}]
        assert bridge.submissions == [
            {"network": network, "raw": raw, "txid": f"{selected.chain}-txid"}
        ]
        assert not bridge.errors
    finally:
        bridge.close()


@pytest.mark.parametrize("ancestry", [False, True], ids=["op-return", "input-ancestry"])
def test_retired_claim_routes_refuse_before_construction(tmp_path, ancestry):
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
    child = Child(tool, tmp_path / "gui-home", False)
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
        initial_legacy_mempool = set(node.rpc.getrawmempool())
        block = harness.prefork_block_hashes[txid]
        initial = {"root": str(root), "bridge": bridge.url, "backend": "esplora",
                            "electrum": f"127.0.0.1:{harness.electrs_legacy.electrum_port}",
                            "rpc": f"127.0.0.1:{node.rpcport}",
                            "cookie": Path(node.node_rpc.cookie_path).read_text().strip(),
                            "fork_rpc": f"127.0.0.1:{harness.blake2b.rpcport}",
                            "fork_cookie": Path(harness.blake2b.node_rpc.cookie_path).read_text().strip(),
                            "fork_tip_height": harness.blake2b.rpc.getblockcount(),
                            "previous": node.rpc.getrawtransaction(txid, False, block), "vout": vout,
                            "coin_height": node.rpc.getblockheader(block)["height"],
                            "tip_height": node.rpc.getblockcount()}
        if ancestry:
            ancestry_txid, ancestry_vout, _ = harness.poison_outpoint
            initial.update({
                "ancestry_previous": node.rpc.getrawtransaction(
                    ancestry_txid, False, harness.poison_block_hash),
                "ancestry_vout": ancestry_vout,
                "ancestry_height": node.rpc.getblockheader(
                    harness.poison_block_hash)["height"],
                "ancestry_derivation_index": 3,
                "ancestry_first_fork_height": harness.activation_height,
                "ancestry_historical_limit": node.rpc.getblockcount() + 1,
                "ancestry_shared_history_height": harness.activation_height - 1,
                "ancestry_shared_history_hash": harness.fork_parent_hash,
            })
        ready = child.send(initial)
        assert ready == {"event": "legacy-claim-refused"}, ready
        assert not bridge.preflights and not bridge.submissions
        assert not bridge.errors
        assert set(node.rpc.getrawmempool()) == initial_legacy_mempool
        assert child.proc.wait(timeout=10) == 0
    finally:
        child.close()
        (tmp_path / "log").write_text("".join(child.transcript))
        try:
            if bridge:
                bridge.close()
        finally:
            if harness:
                harness.cleanup()


@pytest.mark.parametrize("spent_chain", ["bitcoin", "btcb2"])
def test_live_paired_funding_proof_expires_after_marker_spend(tmp_path, spent_chain):
    """The real two-chain collector must reject either spent funding coin."""
    tool = os.getenv("CLAIM_GUI_REGTEST_TEST_PATH")
    missing = missing_binaries()
    if not tool or not os.access(tool, os.X_OK) or missing:
        message = "Build GUI lib tests with regtest-harness and set CLAIM_GUI_REGTEST_TEST_PATH plus pinned node/indexer paths"
        if os.getenv("BTCB2_HARNESS_REQUIRED") == "1":
            pytest.fail(message)
        pytest.skip(message)
    if os.getenv("TEST_DIR"):
        Path(os.environ["TEST_DIR"]).mkdir(parents=True, exist_ok=True)
        tmp_path = Path(tempfile.mkdtemp(prefix="btcb2-paired-proof-", dir=os.environ["TEST_DIR"]))
    child = Child(tool, tmp_path / "gui-home")
    harness = bridge = None
    try:
        descriptor = child.receive()
        assert descriptor["event"] == "descriptor"

        def testnet_public_key(match):
            payload = base58.b58decode_check(match.group())
            assert len(payload) == 78 and payload[:4] == bytes.fromhex("0488b21e")
            return base58.b58encode_check(bytes.fromhex("043587cf") + payload[4:]).decode()

        public_descriptor = re.sub(
            r"xpub[1-9A-HJ-NP-Za-km-z]+", testnet_public_key,
            descriptor["descriptor"].split("#")[0],
        )
        harness = TwoChainRegtest(str(tmp_path / "nodes"), public_descriptor=public_descriptor)
        harness.setup()
        bitcoin, fork = harness.legacy, harness.blake2b

        def marker(node):
            address = node.rpc.getnewaddress()
            block = node.rpc.generatetoaddress(1, address)[0]
            txid = node.rpc.getblock(block)["tx"][0]
            transaction = node.rpc.getrawtransaction(txid, True, block)
            vout = next(
                out["n"] for out in transaction["vout"]
                if out["scriptPubKey"].get("address") == address
            )
            node.generate_block(99)
            assert node.rpc.gettxout(txid, vout)["confirmations"] == 100
            return {"previous": transaction["hex"], "vout": vout, "txid": txid}

        bitcoin_marker = marker(bitcoin)
        # The fork cloned Bitcoin's wallet before divergence. Advance its
        # address once so same-height coinbases cannot accidentally have the
        # same txid merely because both miners used the same payout script.
        fork.rpc.getnewaddress()
        fork_marker = marker(fork)
        assert bitcoin_marker["txid"] != fork_marker["txid"]
        assert bitcoin.rpc.gettxout(fork_marker["txid"], fork_marker["vout"]) is None
        assert fork.rpc.gettxout(bitcoin_marker["txid"], bitcoin_marker["vout"]) is None

        def wait_for_indexers():
            deadline = time.monotonic() + 90
            for node, indexer in (
                (bitcoin, harness.electrs_legacy),
                (fork, harness.electrs_blake2b),
            ):
                target = node.rpc.getblockcount()
                while time.monotonic() < deadline:
                    try:
                        with urlopen(indexer.url + "/blocks/tip/height", timeout=3) as response:
                            if int(response.read()) >= target:
                                break
                    except Exception:
                        pass
                    time.sleep(0.25)
                else:
                    pytest.fail(f"{indexer.url} did not index height {target}")

        wait_for_indexers()
        bridge = Bridge(harness)
        root = tmp_path / "gui"
        root.mkdir()
        shared_id, shared_vout, _ = harness.prefork_outpoints[0]
        shared_block = harness.prefork_block_hashes[shared_id]
        ancestry_id, ancestry_vout, _ = harness.poison_outpoint
        initial = {
            "mode": "paired-funding-proof",
            "root": str(root),
            "bridge": bridge.url,
            "backend": "esplora",
            "rpc": f"127.0.0.1:{bitcoin.rpcport}",
            "cookie": Path(bitcoin.node_rpc.cookie_path).read_text().strip(),
            "fork_rpc": f"127.0.0.1:{fork.rpcport}",
            "fork_cookie": Path(fork.node_rpc.cookie_path).read_text().strip(),
            "fork_tip_height": fork.rpc.getblockcount(),
            "previous": bitcoin.rpc.getrawtransaction(shared_id, False, shared_block),
            "vout": shared_vout,
            "coin_height": bitcoin.rpc.getblockheader(shared_block)["height"],
            "tip_height": bitcoin.rpc.getblockcount(),
            "ancestry_previous": bitcoin.rpc.getrawtransaction(
                ancestry_id, False, harness.poison_block_hash
            ),
            "ancestry_vout": ancestry_vout,
            "ancestry_height": bitcoin.rpc.getblockheader(
                harness.poison_block_hash
            )["height"],
            "ancestry_derivation_index": 3,
            "ancestry_first_fork_height": harness.activation_height,
            "ancestry_historical_limit": bitcoin.rpc.getblockcount() + 1,
            "ancestry_shared_history_height": harness.activation_height - 1,
            "ancestry_shared_history_hash": harness.fork_parent_hash,
        }
        assert child.send(initial) == {"event": "paired-funding-ready"}
        command = {"bitcoin": bitcoin_marker, "btcb2": fork_marker}
        first = child.send(command)
        assert first == {"event": "paired-funding-proof", "valid": True, "failure": None}, first

        bridge.stale_outspend = True
        stale = child.send(command)
        assert stale == {
            "event": "paired-funding-proof", "valid": False,
            "failure": "FreshnessUnverified",
        }, stale
        bridge.stale_outspend = False

        spent_node, spent_marker = (
            (bitcoin, bitcoin_marker) if spent_chain == "bitcoin" else (fork, fork_marker)
        )
        other_node, other_marker = (
            (fork, fork_marker) if spent_chain == "bitcoin" else (bitcoin, bitcoin_marker)
        )
        value = Decimal(str(spent_node.rpc.gettxout(
            spent_marker["txid"], spent_marker["vout"]
        )["value"]))
        raw = spent_node.rpc.createrawtransaction(
            [{"txid": spent_marker["txid"], "vout": spent_marker["vout"]}],
            {spent_node.rpc.getnewaddress(): value - Decimal("0.0001")},
        )
        signed = spent_node.rpc.signrawtransactionwithwallet(raw)
        assert signed["complete"], signed
        spent_id = spent_node.rpc.sendrawtransaction(signed["hex"])
        spent_node.generate_block(1, wait_for_mempool=spent_id)
        wait_for_indexers()
        assert spent_node.rpc.gettxout(spent_marker["txid"], spent_marker["vout"]) is None
        assert other_node.rpc.gettxout(other_marker["txid"], other_marker["vout"]) is not None
        after_spend = child.send(command)
        assert after_spend == {
            "event": "paired-funding-proof", "valid": False, "failure": "Changed"
        }, after_spend
        assert not bridge.errors
        assert child.proc.wait(timeout=10) == 0
    finally:
        child.close()
        (tmp_path / "log").write_text("".join(child.transcript))
        try:
            if bridge:
                bridge.close()
        finally:
            if harness:
                harness.cleanup()
