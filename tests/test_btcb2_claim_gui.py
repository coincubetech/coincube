"""Exercise Claim and Split paired funding on disposable nodes.

The test drivers run the real deposit and paired signing transitions, but do
not render the UI or use physical hardware signers.
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
from test_framework.esplora import EsploraElectrs


BECH32_ALPHABET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"


def regtest_address(mainnet):
    """Translate only a valid v0 mainnet SegWit address's HRP for regtest.

    The public descriptor and witness program are unchanged. This bridge
    exists solely because the GUI's logical chain identity is mainnet while
    the disposable Knots indexers enforce regtest address encoding.
    """
    assert mainnet == mainnet.lower() and mainnet.startswith("bc1q")
    values = [BECH32_ALPHABET.index(char) for char in mainnet[3:]]

    def expand(hrp):
        return [ord(char) >> 5 for char in hrp] + [0] + [ord(char) & 31 for char in hrp]

    def polymod(data):
        checksum = 1
        for value in data:
            top = checksum >> 25
            checksum = ((checksum & 0x1ffffff) << 5) ^ value
            for bit, generator in enumerate(
                (0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3)
            ):
                if (top >> bit) & 1:
                    checksum ^= generator
        return checksum

    assert len(values) >= 7 and polymod(expand("bc") + values) == 1
    payload = values[:-6]
    checksum = polymod(expand("bcrt") + payload + [0] * 6) ^ 1
    digits = payload + [(checksum >> (5 * (5 - index))) & 31 for index in range(6)]
    return "bcrt1" + "".join(BECH32_ALPHABET[digit] for digit in digits)


class Child:
    def __init__(self, tool, home, multisig=False, driver="claim_gui_regtest_driver"):
        home.mkdir()
        self.home = home
        self.marker = "SPLIT_GUI_JSON:" if driver == "split_gui_regtest_driver" else "CLAIM_GUI_JSON:"
        self.proc = subprocess.Popen(
            [tool, driver, "--ignored", "--nocapture", "--test-threads=1"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, env={**os.environ, "CLAIM_GUI_REGTEST_CHILD": "1",
                            "SPLIT_GUI_REGTEST_CHILD": "1" if driver == "split_gui_regtest_driver" else "0",
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

    def receive(self, timeout=45):
        deadline = time.monotonic() + timeout
        while True:
            line = self.lines.get(timeout=max(0.01, deadline - time.monotonic()))
            assert line is not None, "GUI child exited:\n" + "".join(self.transcript)
            self.transcript.append(line)
            if self.marker in line:
                return json.loads(line.split(self.marker, 1)[1])
            assert time.monotonic() < deadline, "GUI child timeout"

    def send(self, value, timeout=45):
        self.proc.stdin.write(json.dumps(value) + "\n")
        self.proc.stdin.flush()
        return self.receive(timeout)

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
        self.stale_outspend_hits = 0
        self.spent_outspend = None
        self.spent_outspend_hits = 0
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
                    _, indexer, suffix, network = self.route()
                    if suffix == "fee-estimates":
                        # Fresh, chain-scoped test quotes: regtest electrs has
                        # no meaningful mempool estimate before these deposits.
                        self.reply(200, {"6": 2.0})
                        return
                    if owner.spent_outspend == (network, suffix):
                        owner.spent_outspend_hits += 1
                        self.reply(200, {"spent": True, "txid": "00" * 32})
                        return
                    requested_address = None
                    if suffix.startswith("address/"):
                        path = suffix.split("/")
                        requested_address = path[1]
                        path[1] = regtest_address(path[1])
                        suffix = "/".join(path)
                    try:
                        with urlopen(indexer.url + "/" + suffix, timeout=4) as response:
                            body = response.read(262145)
                            if requested_address is not None and suffix.count("/") == 1:
                                # Esplora echoes the regtest HRP; the GUI
                                # validates the original mainnet-style address.
                                stats = json.loads(body)
                                assert stats["address"] == regtest_address(requested_address)
                                stats["address"] = requested_address
                                body = json.dumps(stats).encode()
                            stale = owner.stale_outspend and "/outspend/" in suffix
                            if stale:
                                owner.stale_outspend_hits += 1
                            self.reply(
                                response.status, body,
                                response.headers.get("Content-Type", "application/json"),
                                "stale" if stale else "fresh",
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
        with urlopen(prefix + "/fee-estimates", timeout=5) as response:
            assert json.load(response) == {"6": 2.0}
            assert response.headers["X-Coincube-Observation"] == "fresh"
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


def test_paired_claim_wizard_checks_deposits_and_records_signed_pair(tmp_path):
    """Drive paired signing, reject changed reads, then broadcast each side."""
    tool = os.getenv("CLAIM_GUI_REGTEST_TEST_PATH")
    missing = missing_binaries()
    if not tool or not os.access(tool, os.X_OK) or missing:
        message = "Build GUI lib tests with regtest-harness and set CLAIM_GUI_REGTEST_TEST_PATH plus pinned node/indexer paths"
        if os.getenv("BTCB2_HARNESS_REQUIRED") == "1":
            pytest.fail(message)
        pytest.skip(message)
    if os.getenv("TEST_DIR"):
        Path(os.environ["TEST_DIR"]).mkdir(parents=True, exist_ok=True)
        tmp_path = Path(tempfile.mkdtemp(prefix="btcb2-paired-wizard-", dir=os.environ["TEST_DIR"]))
    child = Child(tool, tmp_path / "gui-home")
    harness = bridge = None
    first_transcript = []
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
        bridge = Bridge(harness)
        node = harness.legacy
        root = tmp_path / "gui"
        root.mkdir()

        def shared_coin(index):
            txid, vout, _ = harness.prefork_outpoints[index]
            block = harness.prefork_block_hashes[txid]
            return {"previous": node.rpc.getrawtransaction(txid, False, block),
                    "vout": vout, "height": node.rpc.getblockheader(block)["height"],
                    "index": index}

        first = shared_coin(0)
        initial = {
            "mode": "paired-wizard-deposits",
            "root": str(root), "bridge": bridge.url, "backend": "esplora",
            "rpc": f"127.0.0.1:{node.rpcport}",
            "cookie": Path(node.node_rpc.cookie_path).read_text().strip(),
            "fork_rpc": f"127.0.0.1:{harness.blake2b.rpcport}",
            "fork_cookie": Path(harness.blake2b.node_rpc.cookie_path).read_text().strip(),
            "fork_tip_height": harness.blake2b.rpc.getblockcount(),
            "previous": first["previous"], "vout": first["vout"],
            "coin_height": first["height"], "tip_height": node.rpc.getblockcount(),
            "shared_extra": [shared_coin(1), shared_coin(2)],
            # Use the disposable nodes' actual fork boundary for the same
            # ancestry collector exercised by the standalone proof cases.
            "ancestry_first_fork_height": harness.activation_height,
            # The test mines each funding coin and 99 maturity blocks after
            # opening the panel; this exclusive bound must include both.
            "ancestry_historical_limit": max(
                node.rpc.getblockcount(), harness.blake2b.rpc.getblockcount()
            ) + 256,
            "ancestry_shared_history_height": harness.activation_height - 1,
            "ancestry_shared_history_hash": harness.fork_parent_hash,
        }
        result = child.send(initial, timeout=180)
        assert result["event"] == "paired-deposit-addresses", result
        assert result["bitcoin"].startswith("bc1") and result["btcb2"].startswith("bc1")
        assert result["bitcoin"] != result["btcb2"]
        assert result["bitcoin_minimum"] > 0 and result["btcb2_minimum"] > 0
        assert result["bitcoin_index"] != result["btcb2_index"]

        def fund(node, mainnet_address, index):
            address = regtest_address(mainnet_address)
            block = node.rpc.generatetoaddress(1, address)[0]
            txid = node.rpc.getblock(block)["tx"][0]
            transaction = node.rpc.getrawtransaction(txid, True, block)
            vout = next(
                output["n"] for output in transaction["vout"]
                if output["scriptPubKey"].get("address") == address
            )
            node.generate_block(99)
            assert node.rpc.gettxout(txid, vout)["confirmations"] == 100
            return {"previous": transaction["hex"], "vout": vout,
                    "height": node.rpc.getblockheader(block)["height"],
                    "tip_height": node.rpc.getblockcount(), "index": index}

        bitcoin_funding = fund(node, result["bitcoin"], result["bitcoin_index"])
        fork_funding = fund(
            harness.blake2b, result["btcb2"], result["btcb2_index"]
        )
        initial["bitcoin_funding"] = bitcoin_funding
        initial["btcb2_funding"] = fork_funding
        for chain_node, indexer in (
            (node, harness.electrs_legacy),
            (harness.blake2b, harness.electrs_blake2b),
        ):
            tip = chain_node.rpc.getbestblockhash()
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                try:
                    with urlopen(indexer.url + "/blocks/tip/hash", timeout=3) as response:
                        if response.read().decode() == tip:
                            break
                except Exception:
                    pass
                time.sleep(0.25)
            else:
                pytest.fail(f"{indexer.url} did not index {tip}")

        checked = child.send({"action": "deposited", "bitcoin": bitcoin_funding,
                              "btcb2": fork_funding}, timeout=180)
        assert checked["event"] == "paired-deposits-checked", checked
        assert checked["verified"] and checked["error"] is None, checked
        bridge.stale_outspend = True
        refused = child.send({"action": "sign_pair_stale"}, timeout=180)
        assert refused["event"] == "paired-signing-refused-stale", refused
        assert refused["error"] and bridge.stale_outspend_hits > 0, refused
        assert not bridge.submissions and not bridge.errors
        bridge.stale_outspend = False
        checked = child.send({"action": "deposited"}, timeout=180)
        assert checked["event"] == "paired-deposits-checked-after-stale", checked
        ready = child.send({"action": "sign_pair"}, timeout=180)
        assert ready["event"] == "paired-signing-ready", ready
        for side in ("bitcoin", "btcb2"):
            assert ready[f"{side}_unsigned"] != ready[f"{side}_signed"]
        recorded = child.send({
            "action": "return_signatures",
            "bitcoin_signed": ready["bitcoin_signed"],
            "btcb2_signed": ready["btcb2_signed"],
        }, timeout=180)
        assert recorded["event"] == "paired-signatures-recorded", recorded
        assert len(recorded["bitcoin_txid"]) == len(recorded["btcb2_txid"]) == 64
        assert not bridge.submissions and not bridge.errors
        funding_txid = node.rpc.decoderawtransaction(bitcoin_funding["previous"])["txid"]
        bridge.spent_outspend = (
            "mainnet", f"tx/{funding_txid}/outspend/{bitcoin_funding['vout']}"
        )
        spent = child.send({"action": "recheck_spent"}, timeout=180)
        assert spent["event"] == "paired-review-refused-spent", spent
        assert bridge.spent_outspend_hits > 0
        assert spent["reason"] and not bridge.submissions and not bridge.errors
        bridge.spent_outspend = None
        bridge.stale_outspend = True
        stale = child.send({"action": "recheck_stale"}, timeout=180)
        assert stale["event"] == "paired-review-refused-stale", stale
        assert stale["reason"], stale
        assert not bridge.submissions and not bridge.errors
        bridge.stale_outspend = False
        child.proc.stdin.write('{"action":"quit"}\n')
        child.proc.stdin.flush()
        assert child.proc.wait(timeout=10) == 0
        first_transcript = child.transcript[:]
        child.close()
        child = Child(tool, tmp_path / "reopen-home")
        assert child.receive() == descriptor
        reopened = child.send({
            **initial,
            "mode": "paired-wizard-reopen",
            "resume": True,
            "tip_height": node.rpc.getblockcount(),
            "fork_tip_height": harness.blake2b.rpc.getblockcount(),
            "bitcoin_txid": recorded["bitcoin_txid"],
            "btcb2_txid": recorded["btcb2_txid"],
        }, timeout=180)
        assert reopened["event"] == "paired-signed-pair-reopened", reopened
        assert reopened["bitcoin_unattempted"], reopened
        assert not bridge.submissions and not bridge.errors
        for own, other, raw_name in (
            (harness.legacy, harness.blake2b, "bitcoin_raw"),
            (harness.blake2b, harness.legacy, "btcb2_raw"),
        ):
            raw = reopened[raw_name]
            assert own.rpc.testmempoolaccept([raw])[0]["allowed"], raw_name
            assert not other.rpc.testmempoolaccept([raw])[0]["allowed"], raw_name
        assert child.proc.wait(timeout=10) == 0
        first_transcript.extend(child.transcript)
        child.close()
        child = Child(tool, tmp_path / "bitcoin-broadcast-home")
        assert child.receive() == descriptor
        first_broadcast = child.send({
            **initial, "mode": "paired-wizard-reopen", "resume": True,
            "broadcast_bitcoin": True,
            "tip_height": node.rpc.getblockcount(),
            "fork_tip_height": harness.blake2b.rpc.getblockcount(),
            "bitcoin_txid": recorded["bitcoin_txid"],
            "btcb2_txid": recorded["btcb2_txid"],
        }, timeout=180)
        assert first_broadcast["event"] == "claim-bitcoin-broadcast-recorded", first_broadcast
        assert first_broadcast["bitcoin_txid"] == recorded["bitcoin_txid"]
        assert len(bridge.submissions) == 1 and not bridge.errors
        assert bridge.submissions[0] == {
            "network": "mainnet", "raw": reopened["bitcoin_raw"],
            "txid": recorded["bitcoin_txid"],
        }
        assert child.proc.wait(timeout=10) == 0
        first_transcript.extend(child.transcript)
        child.close()
        harness.legacy.generate_block(6, wait_for_mempool=recorded["bitcoin_txid"])
        bitcoin_tip = harness.legacy.rpc.getbestblockhash()
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            try:
                with urlopen(harness.electrs_legacy.url + "/blocks/tip/hash", timeout=3) as response:
                    if response.read().decode() == bitcoin_tip:
                        break
            except Exception:
                pass
            time.sleep(0.25)
        else:
            pytest.fail(f"Bitcoin indexer did not reach {bitcoin_tip}")
        child = Child(tool, tmp_path / "btcb2-broadcast-home")
        assert child.receive() == descriptor
        second_broadcast = child.send({
            **initial, "mode": "paired-wizard-reopen", "resume": True,
            "broadcast_btcb2": True,
            "tip_height": node.rpc.getblockcount(),
            "fork_tip_height": harness.blake2b.rpc.getblockcount(),
            "bitcoin_txid": recorded["bitcoin_txid"],
            "btcb2_txid": recorded["btcb2_txid"],
        }, timeout=180)
        assert second_broadcast["event"] == "claim-btcb2-broadcast-recorded", second_broadcast
        assert second_broadcast["btcb2_txid"] == recorded["btcb2_txid"]
        assert len(bridge.submissions) == 2 and not bridge.errors
        assert bridge.submissions[1] == {
            "network": "bitcoin-blake2b", "raw": reopened["btcb2_raw"],
            "txid": recorded["btcb2_txid"],
        }
        assert child.proc.wait(timeout=10) == 0
    finally:
        child.close()
        (tmp_path / "log").write_text("".join(first_transcript + child.transcript))
        try:
            if bridge:
                bridge.close()
        finally:
            if harness:
                harness.cleanup()


def test_paired_split_wizard_records_ordinary_signed_pair(tmp_path):
    """Refuse a spent marker, then sign and broadcast Split on both nodes."""
    tool = os.getenv("CLAIM_GUI_REGTEST_TEST_PATH")
    missing = missing_binaries()
    if not tool or not os.access(tool, os.X_OK) or missing:
        message = "Build GUI lib tests with regtest-harness and set CLAIM_GUI_REGTEST_TEST_PATH plus pinned node/indexer paths"
        if os.getenv("BTCB2_HARNESS_REQUIRED") == "1":
            pytest.fail(message)
        pytest.skip(message)
    if os.getenv("TEST_DIR"):
        Path(os.environ["TEST_DIR"]).mkdir(parents=True, exist_ok=True)
        tmp_path = Path(tempfile.mkdtemp(prefix="btcb2-split-wizard-", dir=os.environ["TEST_DIR"]))
    child = Child(tool, tmp_path / "gui-home", driver="split_gui_regtest_driver")
    harness = bridge = None
    first_transcript = []
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
        harness = TwoChainRegtest(
            str(tmp_path / "nodes"), public_descriptor=public_descriptor,
            start_daemons=False,
        )
        harness.setup()
        bridge = Bridge(harness)
        root = tmp_path / "gui"
        root.mkdir()
        initial = {
            "root": str(root), "bridge": bridge.url,
            "ancestry_first_fork_height": harness.activation_height,
            "ancestry_historical_limit": max(
                harness.legacy.rpc.getblockcount(), harness.blake2b.rpc.getblockcount()
            ) + 256,
            "ancestry_shared_history_height": harness.activation_height - 1,
            "ancestry_shared_history_hash": harness.fork_parent_hash,
        }
        first = child.send(initial, timeout=180)
        assert first["event"] == "split-deposit-addresses", first
        assert first["bitcoin"].startswith("bc1q")
        assert first["btcb2"].startswith("bc1q")
        assert first["bitcoin"] != first["btcb2"]
        assert first["bitcoin_minimum"] > 0 and first["btcb2_minimum"] > 0

        funding_outpoints = {}
        for chain, node, address in (
            ("bitcoin", harness.legacy, first["bitcoin"]),
            ("btcb2", harness.blake2b, first["btcb2"]),
        ):
            block = node.rpc.generatetoaddress(1, regtest_address(address))[0]
            txid = node.rpc.getblock(block)["tx"][0]
            transaction = node.rpc.getrawtransaction(txid, True, block)
            vout = next(
                output["n"] for output in transaction["vout"]
                if output["scriptPubKey"].get("address") == regtest_address(address)
            )
            funding_outpoints[chain] = (txid, vout)
            node.generate_block(99)
            assert node.rpc.getblockcount() >= node.rpc.getblockheader(block)["height"] + 99
        assert funding_outpoints["bitcoin"][0] != funding_outpoints["btcb2"][0]
        for node, indexer in (
            (harness.legacy, harness.electrs_legacy),
            (harness.blake2b, harness.electrs_blake2b),
        ):
            tip = node.rpc.getbestblockhash()
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                try:
                    with urlopen(indexer.url + "/blocks/tip/hash", timeout=3) as response:
                        if response.read().decode() == tip:
                            break
                except Exception:
                    pass
                time.sleep(0.25)
            else:
                pytest.fail(f"{indexer.url} did not index {tip}")

        checked = child.send({"action": "deposited"}, timeout=180)
        assert checked["event"] == "split-deposits-checked", checked
        assert checked["verified"] and checked["notice"] is None, checked
        txid, vout = funding_outpoints["bitcoin"]
        bridge.spent_outspend = ("mainnet", f"tx/{txid}/outspend/{vout}")
        refused = child.send({"action": "sign_pair_spent"}, timeout=180)
        assert refused["event"] == "split-signing-refused-spent", refused
        assert refused["notice"] and bridge.spent_outspend_hits > 0, refused
        assert not bridge.submissions and not bridge.errors
        bridge.spent_outspend = None
        checked = child.send({"action": "deposited"}, timeout=180)
        assert checked["event"] == "split-deposits-checked-again", checked
        assert checked["verified"] and checked["notice"] is None, checked
        bridge.stale_outspend = True
        refused = child.send({"action": "sign_pair_stale"}, timeout=180)
        assert refused["event"] == "split-signing-refused-stale", refused
        assert refused["notice"] and bridge.stale_outspend_hits > 0, refused
        assert not bridge.submissions and not bridge.errors
        bridge.stale_outspend = False
        checked = child.send({"action": "deposited"}, timeout=180)
        assert checked["event"] == "split-deposits-checked-after-stale", checked
        recorded = child.send({"action": "sign_pair"}, timeout=180)
        assert recorded["event"] == "split-signed-pair-recorded", recorded
        assert len(recorded["bitcoin_txid"]) == len(recorded["btcb2_txid"]) == 64
        assert recorded["bitcoin_unattempted"]
        assert not bridge.submissions and not bridge.errors
        assert child.proc.wait(timeout=10) == 0
        first_transcript = child.transcript[:]
        child.close()
        child = Child(tool, tmp_path / "reopen-home", driver="split_gui_regtest_driver")
        assert child.receive() == descriptor
        reopened = child.send({
            **initial, "resume": True,
            "bitcoin_txid": recorded["bitcoin_txid"],
            "btcb2_txid": recorded["btcb2_txid"],
        }, timeout=180)
        assert reopened["event"] == "split-signed-pair-reopened", reopened
        assert reopened["bitcoin_unattempted"] and not bridge.submissions
        for own, other, raw_name in (
            (harness.legacy, harness.blake2b, "bitcoin_raw"),
            (harness.blake2b, harness.legacy, "btcb2_raw"),
        ):
            raw = reopened[raw_name]
            assert own.rpc.testmempoolaccept([raw])[0]["allowed"], raw_name
            assert not other.rpc.testmempoolaccept([raw])[0]["allowed"], raw_name
        assert child.proc.wait(timeout=10) == 0
        first_transcript.extend(child.transcript)
        child.close()
        child = Child(tool, tmp_path / "bitcoin-broadcast-home", driver="split_gui_regtest_driver")
        assert child.receive() == descriptor
        first_broadcast = child.send({
            **initial, "resume": True, "broadcast_bitcoin": True,
            "bitcoin_txid": recorded["bitcoin_txid"],
            "btcb2_txid": recorded["btcb2_txid"],
        }, timeout=180)
        assert first_broadcast["event"] == "split-bitcoin-broadcast-recorded", first_broadcast
        assert first_broadcast["bitcoin_txid"] == recorded["bitcoin_txid"]
        assert len(bridge.submissions) == 1 and not bridge.errors
        assert bridge.submissions[0] == {
            "network": "mainnet", "raw": reopened["bitcoin_raw"],
            "txid": recorded["bitcoin_txid"],
        }
        assert child.proc.wait(timeout=10) == 0
        first_transcript.extend(child.transcript)
        child.close()
        harness.legacy.generate_block(6, wait_for_mempool=recorded["bitcoin_txid"])
        bitcoin_tip = harness.legacy.rpc.getbestblockhash()
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            try:
                with urlopen(harness.electrs_legacy.url + "/blocks/tip/hash", timeout=3) as response:
                    if response.read().decode() == bitcoin_tip:
                        break
            except Exception:
                pass
            time.sleep(0.25)
        else:
            pytest.fail(f"Bitcoin indexer did not reach {bitcoin_tip}")
        child = Child(tool, tmp_path / "btcb2-broadcast-home", driver="split_gui_regtest_driver")
        assert child.receive() == descriptor
        second_broadcast = child.send({
            **initial, "resume": True, "broadcast_btcb2": True,
            "bitcoin_txid": recorded["bitcoin_txid"],
            "btcb2_txid": recorded["btcb2_txid"],
        }, timeout=180)
        assert second_broadcast["event"] == "split-btcb2-broadcast-recorded", second_broadcast
        assert second_broadcast["btcb2_txid"] == recorded["btcb2_txid"]
        assert len(bridge.submissions) == 2 and not bridge.errors
        assert bridge.submissions[1] == {
            "network": "bitcoin-blake2b", "raw": reopened["btcb2_raw"],
            "txid": recorded["btcb2_txid"],
        }
        assert child.proc.wait(timeout=10) == 0
    finally:
        child.close()
        (tmp_path / "split-log").write_text("".join(first_transcript + child.transcript))
        try:
            if bridge:
                bridge.close()
        finally:
            if harness:
                harness.cleanup()


@pytest.mark.parametrize(
    "mutation",
    ["bitcoin-spend", "btcb2-spend", "bitcoin-reorg", "btcb2-reorg"],
)
def test_live_paired_funding_proof_expires_after_marker_change(tmp_path, mutation):
    """The two-chain collector must reject a spent or reorganized funding coin."""
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
            return {"previous": transaction["hex"], "vout": vout,
                    "txid": txid, "block": block}

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
                tip_hash = node.rpc.getbestblockhash()
                while time.monotonic() < deadline:
                    try:
                        with urlopen(indexer.url + "/blocks/tip/height", timeout=3) as response:
                            indexed_height = int(response.read())
                        with urlopen(indexer.url + "/blocks/tip/hash", timeout=3) as response:
                            indexed_hash = response.read().decode()
                        if indexed_height == target and indexed_hash == tip_hash:
                            break
                    except Exception:
                        pass
                    time.sleep(0.25)
                else:
                    pytest.fail(f"{indexer.url} did not index tip {target}: {tip_hash}")

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

        revoked = child.send({**command, "action": "revoke"})
        assert revoked == {
            "event": "paired-funding-proof", "valid": False, "failure": "Cancelled"
        }, revoked
        restarted = child.send({**command, "action": "restart"})
        assert restarted == {
            "event": "paired-funding-proof", "valid": True, "failure": None
        }, restarted

        changed_node, changed_marker = (
            (bitcoin, bitcoin_marker) if mutation.startswith("bitcoin") else (fork, fork_marker)
        )
        other_node, other_marker = (
            (fork, fork_marker) if mutation.startswith("bitcoin") else (bitcoin, bitcoin_marker)
        )
        if mutation.endswith("spend"):
            value = Decimal(str(changed_node.rpc.gettxout(
                changed_marker["txid"], changed_marker["vout"]
            )["value"]))
            raw = changed_node.rpc.createrawtransaction(
                [{"txid": changed_marker["txid"], "vout": changed_marker["vout"]}],
                {changed_node.rpc.getnewaddress(): value - Decimal("0.0001")},
            )
            signed = changed_node.rpc.signrawtransactionwithwallet(raw)
            assert signed["complete"], signed
            spent_id = changed_node.rpc.sendrawtransaction(signed["hex"])
            changed_node.generate_block(1, wait_for_mempool=spent_id)
        else:
            # Remove the funding coinbase itself. Replacement blocks reach the
            # same height, so height alone must not pass the indexer waiter.
            previous_tip = changed_node.rpc.getbestblockhash()
            previous_height = changed_node.rpc.getblockcount()
            marker_height = changed_node.rpc.getblockheader(changed_marker["block"])["height"]
            changed_node.rpc.invalidateblock(changed_marker["block"])
            changed_node.generate_block(100)
            assert changed_node.rpc.getblockcount() == previous_height
            assert changed_node.rpc.getbestblockhash() != previous_tip
            assert changed_node.rpc.getblockhash(marker_height) != changed_marker["block"]
            if mutation.endswith("reorg"):
                # The pinned indexer may panic while the node tip moves during
                # a deep reorg. Reopen the same database after the new tip has
                # settled: losing its index is not an acceptable recovery path
                # for a deployed service.
                old = (harness.electrs_legacy if mutation == "bitcoin-reorg"
                       else harness.electrs_blake2b)
                old.stop()
                assert any(Path(old.db_dir).iterdir())
                replacement = EsploraElectrs(
                    electrs_dir=old.electrs_dir,
                    bitcoind_dir=(harness.legacy_dir if mutation == "bitcoin-reorg"
                                  else harness.blake2b_dir),
                    bitcoind_rpcport=changed_node.rpcport,
                    electrs_path=harness.electrs_path,
                    http_port=old.http_port,
                )
                replacement.env.update(old.env)
                if mutation == "bitcoin-reorg":
                    harness.electrs_legacy = replacement
                else:
                    harness.electrs_blake2b = replacement
                replacement.startup()
                replacement.wait_for_tip(changed_node.rpc.getbestblockhash(), timeout=90)
        wait_for_indexers()
        assert changed_node.rpc.gettxout(changed_marker["txid"], changed_marker["vout"]) is None
        assert other_node.rpc.gettxout(other_marker["txid"], other_marker["vout"]) is not None
        after_spend = child.send(command)
        assert after_spend["event"] == "paired-funding-proof" and not after_spend["valid"], after_spend
        assert (after_spend["failure"] == "Changed" or
                after_spend["failure"].startswith("AncestryRootChanged")), after_spend
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
