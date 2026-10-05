"""Split (#568 B6c-2): a Bitcoin reorg of step 1 seen through the production
Split services and their journal, on the two pinned nodes.

A child process (`split_service_regtest_driver`, the coincube-gui lib tests
built with the `regtest-harness` feature) owns the production Split services:
`SplitProduction` and the step-1 coordinator, `SplitPreparation` (the
six-confirmation gate before step 2) and, after step 2, the journal record
of its submission and the `SplitStep2Reconciler`. Every observation they make
is a real HTTP read of the nodes' indexers through a local stand-in for
Connect's Esplora, anchor and preflight routes (the Claim GUI test's bridge
pattern), and step 1 reaches Bitcoin through Connect's submit route.

Synthetic: the foreign wallet's keys (a P2PKH wallet funded before the fork,
so its coins are shared history), the target Vault and its address
reservation (no daemon), the Connect account, and the step-2 fee, passed
explicitly because Connect's regtest fee estimates are empty. Step 2 is
built, signed and recorded by the child; this module sends its bytes to the
BTCB2 node itself. No GUI, panel, hardware signing or daemon is involved.

The Bitcoin reorg follows the Claim GUI test: the Bitcoin indexer is
stopped, step 1's block invalidated and a longer competing branch mined
without it, then the indexer restarts on its database. Step 1 waits in the
Bitcoin mempool (O2) until a new block re-mines it (O1).
"""

import json
import os
import queue
import re
import subprocess
import tempfile
import threading
import time
from decimal import Decimal
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import urlopen

import base58
import pytest

from test_framework.btcb2 import (
    MIN_ACTIVATION_HEIGHT,
    TwoChainRegtest,
    missing_binaries,
)
from test_framework.utils import wait_for

MARKER = "SPLIT_SERVICE_JSON:"
# The coins the parent funds before the fork: (branch, index, amount).
FUNDING = (("external", 0, Decimal("0.01")), ("internal", 1, Decimal("0.02")))
BRANCHES = {"external": 0, "internal": 1}
STEP1_FEERATE = 2
STEP2_FEERATE = 2
MIN_CONFIRMATIONS = 6


# ── address translation ───────────────────────────────────────────────────
# Production reads Connect's mainnet routes with mainnet addresses; the
# regtest indexer only parses regtest ones. The stand-in re-encodes the same
# script for regtest (BIP 173/350, Base58Check) and back in its replies.

BECH32_CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"
BECH32M_CONST = 0x2BC830A3


def _polymod(values):
    generator = [0x3B6A57B2, 0x26508E6D, 0x1EA119FA, 0x3D4233DD, 0x2A1462B3]
    chk = 1
    for value in values:
        top = chk >> 25
        chk = (chk & 0x1FFFFFF) << 5 ^ value
        for i in range(5):
            chk ^= generator[i] if ((top >> i) & 1) else 0
    return chk


def _hrp_expand(hrp):
    return [ord(c) >> 5 for c in hrp] + [0] + [ord(c) & 31 for c in hrp]


def bech32_reencode(address, hrp):
    """The same witness program under another human-readable part."""
    old_hrp, data = address.lower().rsplit("1", 1)
    values = [BECH32_CHARSET.index(c) for c in data]
    const = _polymod(_hrp_expand(old_hrp) + values)
    assert const in (1, BECH32M_CONST), address
    payload = values[:-6]
    polymod = _polymod(_hrp_expand(hrp) + payload + [0] * 6) ^ const
    checksum = [(polymod >> 5 * (5 - i)) & 31 for i in range(6)]
    return hrp + "1" + "".join(BECH32_CHARSET[v] for v in payload + checksum)


def base58_reversion(address, versions):
    payload = base58.b58decode_check(address)
    return base58.b58encode_check(bytes([versions[payload[0]]]) + payload[1:]).decode()


def to_regtest(address):
    if address.lower().startswith("bc1"):
        return bech32_reencode(address, "bcrt")
    return base58_reversion(address, {0x00: 0x6F, 0x05: 0xC4})


def to_mainnet(address):
    if address.lower().startswith("bcrt1"):
        return bech32_reencode(address, "bc")
    return base58_reversion(address, {0x6F: 0x00, 0xC4: 0x05})


def regtest_public_keys(descriptor):
    """Logical mainnet xpubs as regtest tpubs: only the version bytes change."""

    def convert(match):
        payload = base58.b58decode_check(match.group())
        assert len(payload) == 78 and payload[:4] == bytes.fromhex("0488b21e")
        return base58.b58encode_check(bytes.fromhex("043587cf") + payload[4:]).decode()

    return re.sub(r"xpub[1-9A-HJ-NP-Za-km-z]+", convert, descriptor.split("#")[0])


# ── child, Connect stand-in, chains ───────────────────────────────────────


class Child:
    def __init__(self, tool, home):
        home.mkdir()
        self.home = home
        self.proc = subprocess.Popen(
            [
                tool,
                "split_service_regtest_driver",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            env={
                **os.environ,
                "SPLIT_SERVICE_REGTEST_CHILD": "1",
                "HOME": str(home),
                "XDG_CONFIG_HOME": str(home / ".config"),
                "XDG_DATA_HOME": str(home / ".local/share"),
                "XDG_CACHE_HOME": str(home / ".cache"),
            },
        )
        self.lines = queue.Queue()
        self.transcript = []
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        for line in self.proc.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def receive(self):
        deadline = time.monotonic() + 120
        while True:
            line = self.lines.get(timeout=max(0.01, deadline - time.monotonic()))
            assert line is not None, "Split child exited:\n" + "".join(self.transcript)
            self.transcript.append(line)
            if MARKER in line:
                return json.loads(line.split(MARKER, 1)[1])
            assert time.monotonic() < deadline, "Split child timeout"

    def send(self, value):
        self.proc.stdin.write(json.dumps(value) + "\n")
        self.proc.stdin.flush()
        return self.receive()

    def command(self, name, **fields):
        result = self.send({"command": name, **fields})
        assert result["event"] == name, result
        return result

    def quit(self):
        self.proc.stdin.write('{"command":"quit"}\n')
        self.proc.stdin.flush()
        assert self.proc.wait(timeout=30) == 0, "".join(self.transcript)

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
    """Connect's anchor, Esplora and preflight routes over the two nodes'
    indexers. Records every POST: step 1's submission is the only send."""

    def __init__(self, harness):
        self.posts = []
        self.submissions = []
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
                network, mainnet, suffix = self.path[len(prefix) :].split("/", 2)
                assert mainnet == "mainnet"
                if network == "bitcoin":
                    return harness.legacy, harness.electrs_legacy, suffix, "mainnet"
                assert network == "bitcoin-blake2b"
                return harness.blake2b, harness.electrs_blake2b, suffix, network

            def anchor(self):
                assert (
                    self.headers.get("Authorization") == "Bearer synthetic-regtest-only"
                )
                node = harness.blake2b.rpc
                tip = node.getbestblockhash()
                header = node.getblockheader(tip)
                deployments = node.getdeploymentinfo(tip)
                fork = deployments["blake2b"]
                rdts = deployments["deployments"]["reduced_data"]
                assert fork["active"] and rdts["active"] and rdts["type"] == "flagday"
                assert len(node.getblockheader(tip, False)) == 328
                assert node.getbestblockhash() == tip
                flagday = {k: rdts[k] for k in ("height", "expiry_time", "active")}
                self.reply(
                    200,
                    {
                        "success": True,
                        "data": {
                            "network": "bitcoin-blake2b",
                            "state": "available",
                            "anchor": {
                                "tip_hash": tip,
                                "tip_height": header["height"],
                                "tip_median_time_past": header["mediantime"],
                                "observed_at": int(time.time()),
                                "observation": {
                                    "tip_height": header["height"],
                                    "fork": fork,
                                    "rdts": {"state": "flagday", "flagday": flagday},
                                },
                            },
                        },
                    },
                )

            def do_GET(self):
                try:
                    if self.path == "/api/v1/connect/networks/bitcoin-blake2b/anchor":
                        return self.anchor()
                    _, indexer, suffix, _ = self.route()
                    address = None
                    parts = suffix.split("/")
                    if parts[0] == "address":
                        address = parts[1]
                        parts[1] = to_regtest(address)
                    try:
                        with urlopen(
                            indexer.url + "/" + "/".join(parts), timeout=4
                        ) as response:
                            body = response.read(262145)
                            content_type = response.headers.get(
                                "Content-Type", "application/json"
                            )
                            if address is not None and len(parts) == 2:
                                stats = json.loads(body)
                                assert to_mainnet(stats["address"]) == address
                                stats["address"] = address
                                body = json.dumps(stats).encode()
                            self.reply(response.status, body, content_type)
                    except HTTPError as error:
                        self.reply(error.code, error.read(262145))
                except Exception as error:
                    owner.errors.append(repr(error))
                    self.reply(503, {"error": "bridge-refused"})

            def do_POST(self):
                try:
                    node, _, suffix, network = self.route()
                    owner.posts.append((network, suffix))
                    length = int(self.headers["Content-Length"])
                    assert 0 < length <= 262144
                    body = self.rfile.read(length)
                    if suffix == "tx":
                        assert network == "mainnet"
                        raw = body.decode("ascii")
                        txid = node.rpc.sendrawtransaction(raw)
                        owner.submissions.append({"raw": raw, "txid": txid})
                        self.reply(200, txid.encode("ascii"), "text/plain")
                        return
                    assert suffix == "tx/preflight"
                    request = json.loads(body)
                    tip = node.rpc.getbestblockhash()
                    assert tip == request["tip_hash"]
                    result = node.rpc.testmempoolaccept([request["transaction"]])[0]
                    decoded = node.rpc.decoderawtransaction(request["transaction"])
                    assert node.rpc.getbestblockhash() == tip
                    owner.preflights.append((network, result))
                    verdict = {
                        "txid": decoded["txid"],
                        "wtxid": decoded["hash"],
                        "tip_hash": tip,
                        "observed_at": int(time.time()),
                        "allowed": result["allowed"],
                    }
                    if not result["allowed"]:
                        verdict["reject_reason"] = result["reject-reason"]
                    self.reply(
                        200,
                        {
                            "success": True,
                            "data": {
                                "network": network,
                                "state": "available",
                                "result": verdict,
                            },
                        },
                    )
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


class SplitServiceChains(TwoChainRegtest):
    """The pinned two-node history with one foreign wallet funded before the
    fork, both indexers, and no daemon: a Split has no Bitcoin Cube."""

    def __init__(self, directory, descriptors):
        super().__init__(directory)
        # branch name -> the child's descriptor, with regtest key versions.
        self.descriptors = descriptors
        self.funded = []  # (txid, vout, branch, index, block hash)

    def setup(self):
        os.makedirs(self.home_dir, exist_ok=True)
        self._start_legacy_and_fund()
        self._fork()
        self.blake2b.generate_block(self.post_fork_blocks_blake2b)
        self.legacy.generate_block(self.post_fork_blocks_blake2b)
        assert self.legacy.rpc.getblockhash(
            self.activation_height
        ) != self.blake2b.rpc.getblockhash(self.activation_height)
        self._start_indexers()

    def _start_legacy_and_fund(self):
        # As the Split consensus module does: stop the base funding at the
        # earliest allowed height so this wallet's funding still confirms
        # before the real fork, then land on the fork parent.
        fork = self.activation_height
        self.activation_height = MIN_ACTIVATION_HEIGHT
        try:
            super()._start_legacy_and_fund()
        finally:
            self.activation_height = fork
        rpc = self.legacy.rpc
        outputs = {}
        scripts = {}
        for branch, index, amount in FUNDING:
            desc = rpc.getdescriptorinfo(self.descriptors[branch])["descriptor"]
            address = rpc.deriveaddresses(desc, [index, index])[0]
            outputs[address] = amount
            scripts[rpc.validateaddress(address)["scriptPubKey"]] = (branch, index)
        txid = rpc.sendmany("", outputs)
        self.legacy.generate_block(1, wait_for_mempool=txid)
        block = rpc.getbestblockhash()
        assert rpc.getblockcount() < fork - 1
        decoded = rpc.getrawtransaction(txid, True, block)
        self.funded = [
            (txid, out["n"], *scripts[out["scriptPubKey"]["hex"]], block)
            for out in decoded["vout"]
            if out["scriptPubKey"]["hex"] in scripts
        ]
        assert len(self.funded) == len(FUNDING)
        self.legacy.generate_block(fork - 1 - rpc.getblockcount())
        assert rpc.getblockcount() == fork - 1
        self.fork_parent_hash = rpc.getbestblockhash()


def driver_path():
    tool = os.getenv("SPLIT_SERVICE_REGTEST_TEST_PATH")
    missing = missing_binaries()
    if not tool or not os.access(tool, os.X_OK) or missing:
        message = (
            "Build GUI lib tests with regtest-harness and set "
            "SPLIT_SERVICE_REGTEST_TEST_PATH plus pinned node/indexer paths"
        )
        if os.getenv("BTCB2_HARNESS_REQUIRED") == "1":
            pytest.fail(message)
        pytest.skip(message)
    return tool


class Split:
    """One disposable split: its child, chains and Connect stand-in."""

    def __init__(self, tmp_path):
        tool = driver_path()
        if os.getenv("TEST_DIR"):
            Path(os.environ["TEST_DIR"]).mkdir(parents=True, exist_ok=True)
            tmp_path = Path(
                tempfile.mkdtemp(prefix="btcb2-split-", dir=os.environ["TEST_DIR"])
            )
        self.tmp_path = tmp_path
        self.harness = self.bridge = None
        self.child = Child(tool, tmp_path / "split-home")

    def start(self):
        descriptors = self.child.receive()
        assert descriptors["event"] == "descriptors", descriptors
        regtest = {
            branch: regtest_public_keys(descriptors[branch]) for branch in BRANCHES
        }
        self.harness = SplitServiceChains(str(self.tmp_path / "nodes"), regtest)
        self.harness.setup()
        self.bridge = Bridge(self.harness)
        a, b = self.harness.legacy, self.harness.blake2b
        coins = []
        for txid, vout, branch, index, block in self.harness.funded:
            header = a.rpc.getblockheader(block)
            # Shared history: the same block on both chains.
            assert b.rpc.getblockheader(block)["height"] == header["height"]
            assert header["height"] < self.harness.activation_height
            coins.append(
                {
                    "previous": a.rpc.getrawtransaction(txid, False, block),
                    "vout": vout,
                    "branch": branch,
                    "index": index,
                    "block": {"height": header["height"], "hash": block},
                }
            )
        root = self.tmp_path / "split-root"
        root.mkdir()
        self.ready = self.child.send(
            {
                "root": str(root),
                "bridge": self.bridge.url,
                "coins": coins,
                "fork_height": self.harness.activation_height,
                "fork_marker": b.rpc.getblockhash(self.harness.activation_height),
                "bitcoin_tip_height": a.rpc.getblockcount(),
                "feerate": STEP1_FEERATE,
            }
        )
        assert self.ready["event"] == "ready", self.ready
        claimed = {f"{txid}:{vout}" for txid, vout, *_ in self.harness.funded}
        assert set(self.ready["claimed"]) == claimed
        return self.ready

    @property
    def step1(self):
        return self.ready["step1_txid"]

    def wait_bitcoin(self, confirmed=()):
        a = self.harness.legacy
        self.harness.electrs_legacy.wait_for_tip(
            a.rpc.getbestblockhash(), confirmed_txids=list(confirmed)
        )

    def wait_fork(self, confirmed=()):
        b = self.harness.blake2b
        self.harness.electrs_blake2b.wait_for_tip(
            b.rpc.getbestblockhash(), confirmed_txids=list(confirmed)
        )

    def submit_and_bury(self, record_property):
        """Step 1 submitted once through Connect and buried six deep:
        WaitingForDepth at one, the gate refusing below six, minting at six.
        Returns step 1's recorded block."""
        a = self.harness.legacy
        submitted = self.child.command("submit")
        assert submitted["route"] == "Connect", submitted
        assert submitted["outcome"].startswith("Ok(UpstreamAccepted"), submitted
        assert self.bridge.submissions == [
            {"raw": self.ready["step1_raw"], "txid": self.step1}
        ]
        assert self.step1 in a.rpc.getrawmempool()
        actual = a.rpc.getrawtransaction(self.step1, True)
        assert actual["hash"] == self.ready["step1_wtxid"]
        record_property("step1_txid", self.step1)

        a.generate_block(1, wait_for_mempool=self.step1)
        self.wait_bitcoin([self.step1])
        shallow = self.child.command("reconcile_step1")
        assert (
            shallow["status"] == "Ok(Observation(WaitingForDepth { confirmations: 1 }))"
        ), shallow
        refused = self.child.command("check_signing")
        assert not refused["minted"], refused
        assert (
            refused["error"]
            == "Coordinator(NotReady(WaitingForDepth { confirmations: 1 }))"
        ), refused
        a.generate_block(MIN_CONFIRMATIONS - 1)
        self.wait_bitcoin([self.step1])
        eligible = self.child.command("reconcile_step1")
        assert (
            eligible["status"] == "Ok(Observation(ObservationsEligibleForPreflight))"
        ), eligible
        recorded = eligible["journal"]["plan"]["previous_confirmation"]
        assert a.rpc.getblockhash(recorded["height"]) == recorded["hash"]
        assert self.step1 in a.rpc.getblock(recorded["hash"])["tx"]
        assert eligible["journal"].get("inclusion_history", []) == []
        minted = self.child.command("check_signing")
        assert minted["minted"] and minted["tracked_txid"] == self.step1, minted
        assert self.step1 not in self.harness.blake2b.rpc.getrawmempool()
        return recorded

    def reorg_bitcoin(self, recorded):
        """Take step 1's block out of Bitcoin's best chain: a longer empty
        branch, indexed after a restart. Step 1 waits in the mempool."""
        a = self.harness.legacy
        old_tip_height = a.rpc.getblockcount()
        self.harness.electrs_legacy.stop()
        a.invalidate_block(recorded["hash"])
        a.generate_empty_blocks(old_tip_height - a.rpc.getblockcount() + 1)
        self.harness.electrs_legacy.startup()
        assert a.rpc.getblockheader(recorded["hash"])["confirmations"] == -1
        assert self.step1 in a.rpc.getrawmempool()
        self.wait_bitcoin()
        # The indexer must serve step 1 from its mempool, not a 404.
        status = f"{self.harness.electrs_legacy.url}/tx/{self.step1}/status"
        wait_for(lambda: self._unconfirmed(status))

    @staticmethod
    def _unconfirmed(url):
        try:
            with urlopen(url, timeout=2) as response:
                return json.loads(response.read())["confirmed"] is False
        except (HTTPError, OSError, ValueError):
            return False

    def remine(self, recorded):
        """Mine step 1 again from the mempool, into a new block."""
        a = self.harness.legacy
        assert self.step1 in a.rpc.getrawmempool()
        block = a.rpc.generatetoaddress(1, a.rpc.getnewaddress())[0]
        assert block != recorded["hash"]
        assert self.step1 in a.rpc.getblock(block)["tx"]
        self.wait_bitcoin([self.step1])
        return {"height": a.rpc.getblockcount(), "hash": block}

    def finish(self):
        assert not self.bridge.errors, self.bridge.errors
        self.harness.assert_home_sandbox_untouched()
        assert (
            list(self.child.home.iterdir()) == []
        ), "the child used a default HOME/XDG path"
        self.child.quit()

    def close(self):
        try:
            self.child.close()
            (self.tmp_path / "log").write_text("".join(self.child.transcript))
        finally:
            try:
                if self.bridge:
                    self.bridge.close()
            finally:
                if self.harness:
                    self.harness.cleanup()


def test_split_observation_reorg_before_step2(tmp_path, record_property):
    split = Split(tmp_path)
    try:
        split.start()
        a = split.harness.legacy
        recorded = split.submit_and_bury(record_property)

        # The reorg: the journal keeps the recorded block and the gate refuses.
        split.reorg_bitcoin(recorded)
        reorged = split.child.command("reconcile_step1")
        assert reorged["status"] == "Ok(Observation(Reorged))", reorged
        assert reorged["journal"]["plan"]["previous_confirmation"] == recorded
        assert reorged["journal"].get("inclusion_history", []) == []
        refused = split.child.command("check_signing")
        assert not refused["minted"], refused
        assert refused["error"] == "Coordinator(NotReady(Reorged))", refused
        assert refused["journal"] == reorged["journal"]

        # Re-mined in another block: still Reorged until the explicit
        # acknowledgement, which grows the inclusion history by one entry.
        moved = split.remine(recorded)
        still = split.child.command("reconcile_step1")
        assert still["status"] == "Ok(Observation(Reorged))", still
        assert still["journal"]["plan"]["previous_confirmation"] == recorded
        refused = split.child.command("check_signing")
        assert refused["error"] == "Coordinator(NotReady(Reorged))", refused
        reconfirmed = split.child.command("reconfirm_step1")
        assert reconfirmed["review"] == {
            "previous": recorded,
            "confirmed": moved,
        }, reconfirmed
        assert reconfirmed["confirmed"] is None, reconfirmed
        assert (
            reconfirmed["status"]
            == "Ok(Observation(WaitingForDepth { confirmations: 1 }))"
        ), reconfirmed
        journal = reconfirmed["journal"]
        assert journal["plan"]["previous_confirmation"] == moved
        assert journal["inclusion_history"] == [
            {"previous": recorded, "confirmed": moved}
        ]
        refused = split.child.command("check_signing")
        assert (
            refused["error"]
            == "Coordinator(NotReady(WaitingForDepth { confirmations: 1 }))"
        ), refused

        # Eligible again at six in the new block.
        a.generate_block(MIN_CONFIRMATIONS - 1)
        split.wait_bitcoin([split.step1])
        minted = split.child.command("check_signing")
        assert minted["minted"] and minted["tracked_txid"] == split.step1, minted
        assert minted["journal"]["plan"]["previous_confirmation"] == moved
        assert minted["journal"]["inclusion_history"] == journal["inclusion_history"]

        # Step 1 was sent once, through Connect; nothing else was sent.
        assert split.bridge.posts.count(("mainnet", "tx")) == 1
        assert all(network == "mainnet" for network, _ in split.bridge.posts)
        assert len(minted["journal"]["bitcoin_attempts"]) == 1
        record_property(
            "step1_reconfirmation", {"previous": recorded, "confirmed": moved}
        )
        split.finish()
    finally:
        split.close()


def test_split_observation_reorg_after_step2(tmp_path, record_property):
    split = Split(tmp_path)
    try:
        split.start()
        a, b = split.harness.legacy, split.harness.blake2b
        recorded = split.submit_and_bury(record_property)

        # Step 2 recorded with its submission intent, then mined on BTCB2 by
        # this parent: the child holds no step-2 transport.
        step2 = split.child.command("record_step2", feerate=STEP2_FEERATE)
        step2_txid = step2["step2_txid"]
        submission = step2["journal"]["fork_submission"]
        assert submission == {
            "txid": step2_txid,
            "wtxid": step2["step2_wtxid"],
        }, submission
        assert step2["journal"]["split"]["step2_transaction"] is not None
        decoded = b.rpc.decoderawtransaction(step2["step2_raw"])
        assert {f"{i['txid']}:{i['vout']}" for i in decoded["vin"]} == set(
            split.ready["claimed"]
        )
        assert [o["scriptPubKey"]["hex"] for o in decoded["vout"]] == [
            step2["target_script"]
        ]
        assert b.rpc.sendrawtransaction(step2["step2_raw"]) == step2_txid
        b.generate_block(MIN_CONFIRMATIONS, wait_for_mempool=step2_txid)
        split.wait_fork([step2_txid])
        step2_block = b.rpc.getblockhash(b.rpc.getblockcount() - MIN_CONFIRMATIONS + 1)
        mined = b.rpc.getrawtransaction(step2_txid, True, step2_block)
        assert (
            mined["hex"] == step2["step2_raw"]
            and mined["confirmations"] == MIN_CONFIRMATIONS
        )
        record_property("step2_txid", step2_txid)

        # The step-1 coordinator never reopens a journal with a recorded
        # step-2 submission (#568 D13 = A).
        closed = split.child.command("reconcile_step1")
        assert closed["open_error"] == "SubmissionAlreadyRecorded", closed

        # Before the reorg: step 1 eligible and completion minted (the
        # positive control for the refusals below).
        before = split.child.command("reconcile_step2")
        assert (
            before["status"] == "Observation(ObservationsEligibleForPreflight)"
        ), before
        assert before["step2"].startswith("Confirmed"), before
        assert before["after_step2"] == {"kind": "Eligible"}, before
        complete = split.child.command("check_completion")
        assert complete["completion"]["txid"] == step2_txid, complete
        journal = complete["journal"]
        assert journal["plan"]["previous_confirmation"] == recorded

        # O2: step 1 out of every block and back in Bitcoin's mempool.
        split.reorg_bitcoin(recorded)
        assert (
            b.rpc.getrawtransaction(step2_txid, True, step2_block)["confirmations"]
            >= MIN_CONFIRMATIONS
        )
        in_mempool = split.child.command("reconcile_step2")
        assert in_mempool["status"] == "Observation(Reorged)", in_mempool
        assert in_mempool["step2"].startswith("Confirmed"), in_mempool
        assert in_mempool["step1"].startswith("Unconfirmed"), in_mempool
        assert in_mempool["after_step2"] == {"kind": "InMempool"}, in_mempool
        assert (
            in_mempool["journal"] == journal
        ), "a reorged reconcile must not change the journal"
        refused = split.child.command("check_completion")
        assert refused["completion"] is None, refused
        assert refused["journal"] == journal

        # O1: re-mined in another block. Reported, journal unchanged, until
        # the explicit acknowledgement.
        moved = split.remine(recorded)
        remined = split.child.command("reconcile_step2")
        assert remined["status"] == "Observation(Reorged)", remined
        assert remined["after_step2"] == {
            "kind": "Remined",
            "previous": recorded,
            "confirmed": moved,
        }, remined
        assert remined["journal"] == journal
        refused = split.child.command("check_completion")
        assert refused["completion"] is None, refused
        reconfirmed = split.child.command("reconfirm_after_step2")
        assert reconfirmed["review"] == {
            "previous": recorded,
            "confirmed": moved,
        }, reconfirmed
        assert reconfirmed["confirmed"] is None, reconfirmed
        acknowledged = reconfirmed["journal"]
        assert acknowledged["plan"]["previous_confirmation"] == moved
        assert acknowledged["inclusion_history"] == [
            {"previous": recorded, "confirmed": moved}
        ]
        for key in (
            "fork_submission",
            "fork_sweep",
            "bitcoin_transaction",
            "bitcoin_attempts",
        ):
            assert acknowledged.get(key) == journal.get(key), key
        assert (
            acknowledged["split"]["step2_transaction"]
            == journal["split"]["step2_transaction"]
        )
        assert acknowledged["split"].get("step1_conflict") is None

        # Completion stays refused until step 1 is six deep again.
        shallow = split.child.command("reconcile_step2")
        assert (
            shallow["status"] == "Observation(WaitingForDepth { confirmations: 1 })"
        ), shallow
        assert shallow["after_step2"] == {
            "kind": "Shallow",
            "confirmations": 1,
        }, shallow
        refused = split.child.command("check_completion")
        assert refused["completion"] is None, refused
        a.generate_block(MIN_CONFIRMATIONS - 1)
        split.wait_bitcoin([split.step1])
        eligible = split.child.command("reconcile_step2")
        assert eligible["after_step2"] == {"kind": "Eligible"}, eligible
        complete = split.child.command("check_completion")
        assert complete["completion"]["txid"] == step2_txid, complete
        assert (
            complete["journal"]["inclusion_history"]
            == acknowledged["inclusion_history"]
        )

        # Nothing was sent after step 1: one Connect submission, no BTCB2
        # POST, step 2 never on Bitcoin.
        assert split.bridge.posts.count(("mainnet", "tx")) == 1
        assert all(network == "mainnet" for network, _ in split.bridge.posts)
        assert step2_txid not in a.rpc.getrawmempool()
        assert len(complete["journal"]["bitcoin_attempts"]) == 1
        assert complete["journal"]["split"].get("step2_resubmissions", []) == []
        record_property(
            "step1_reconfirmation", {"previous": recorded, "confirmed": moved}
        )
        split.finish()
    finally:
        split.close()
