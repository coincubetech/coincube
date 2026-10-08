"""Disposable provider-contract test: python tests/pruned_history_regtest.py BITCOIND.

Never connects to an existing node or wallet. Exercises real pruning, inclusion
proof import, pure-outgoing fallback, fetched DATA without undo, and persistence.
"""
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from decimal import Decimal
from pathlib import Path

from test_framework.authproxy import AuthServiceProxy, JSONRPCException


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait(predicate, seconds=60):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, JSONRPCException):
            pass
        time.sleep(0.1)
    raise AssertionError("Timed out waiting for disposable node")


class Node:
    def __init__(self, binary, directory, prune):
        self.binary, self.directory, self.prune = binary, directory, prune
        self.rpcport, self.p2pport = port(), port()
        self.url = f"http://fixture:fixture@127.0.0.1:{self.rpcport}"
        self.rpc = AuthServiceProxy(self.url)
        self.process = None
        directory.mkdir()
        try:
            self.start()
        except Exception:
            self.stop()
            raise

    def start(self):
        self.process = subprocess.Popen([
            self.binary, "-regtest", "-server", f"-datadir={self.directory}",
            f"-rpcport={self.rpcport}", f"-port={self.p2pport}",
            "-rpcuser=fixture", "-rpcpassword=fixture", "-bind=127.0.0.1",
            "-listenonion=0", "-dnsseed=0", "-connect=0", "-nowallet",
            "-fallbackfee=0.00001", f"-prune={self.prune}",
        ], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        wait(lambda: AuthServiceProxy(self.url).getblockchaininfo())
        self.rpc = AuthServiceProxy(self.url)

    def stop(self):
        if self.process is not None:
            try:
                self.rpc.stop()
            except (OSError, JSONRPCException):
                pass
            try:
                self.process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.process.terminate()
                self.process.wait(timeout=10)
            self.process = None

    def wallet(self, name):
        return AuthServiceProxy(f"http://fixture:fixture@127.0.0.1:{self.rpcport}/wallet/{name}")


def exercise(binary):
    root = Path(tempfile.mkdtemp(prefix="coincube-history-regtest-"))
    nodes = []
    try:
        archive = Node(binary, root / "archive", 0)
        nodes.append(archive)
        local = Node(binary, root / "local", 1)
        nodes.append(local)
        archive.rpc.createwallet("miner")
        miner = archive.wallet("miner")
        mining = miner.getnewaddress()
        archive.rpc.generatetoaddress(300, mining)
        receive = miner.getnewaddress()
        descriptor = miner.getaddressinfo(receive)["desc"]
        funding = miner.sendtoaddress(receive, Decimal("1"))
        fund_block = archive.rpc.generatetoaddress(1, mining)[0]
        funding_raw = miner.gettransaction(funding)["hex"]
        decoded = archive.rpc.decoderawtransaction(funding_raw)
        vout = next(out["n"] for out in decoded["vout"] if out["scriptPubKey"].get("address") == receive)
        raw = miner.createrawtransaction([{"txid": funding, "vout": vout}], {miner.getnewaddress(): Decimal("0.999")})
        spender_raw = miner.signrawtransactionwithwallet(raw)["hex"]
        spender = miner.sendrawtransaction(spender_raw)
        spend_block = archive.rpc.generatetoaddress(1, mining)[0]
        fund_proof = archive.rpc.gettxoutproof([funding], fund_block)
        spend_proof = archive.rpc.gettxoutproof([spender], spend_block)

        # Fill a complete block file so the old funding/spending bodies can be
        # genuinely pruned. generateblock bypasses relay policy but validates
        # consensus; no public network, fee or real funds are involved.
        mature = miner.listunspent(101)
        for coin in mature[:150]:
            raw = miner.createrawtransaction([{"txid": coin["txid"], "vout": coin["vout"]}], [{"data": "00" * 900000}])
            signed = miner.signrawtransactionwithwallet(raw)
            assert signed["complete"]
            archive.rpc.generateblock(mining, [signed["hex"]])
        archive.rpc.generatetoaddress(1200, mining)
        local.rpc.addnode(f"127.0.0.1:{archive.p2pport}", "onetry")
        wait(lambda: local.rpc.getblockcount() == archive.rpc.getblockcount(), 180)
        local.rpc.pruneblockchain(local.rpc.getblockcount() - 288)
        before = local.rpc.getblockchaininfo()["pruneheight"]
        assert before > 302, before
        try:
            local.rpc.getblock(spend_block, 0)
            raise AssertionError("Expected a genuinely pruned spending block")
        except JSONRPCException as error:
            assert error.error["code"] == -1

        local.rpc.createwallet("recovery", True, True, "", False, True, True)
        wallet = local.wallet("recovery")
        # Compressed regtest mining puts genuinely pruned blocks inside the
        # normal "now minus two hours" import window. Skip implicit scanning;
        # this test deliberately reconstructs history with proofs/bounded scans.
        imported = wallet.importdescriptors([{"desc": descriptor, "timestamp": int(time.time()) + 10800}])
        assert imported[0]["success"], imported
        local.rpc.verifytxoutproof(fund_proof)
        wallet.importprunedfunds(funding_raw, fund_proof)
        assert wallet.gettransaction(funding, True)["confirmations"] > 0
        assert local.rpc.gettxout(funding, vout, True) is None
        # An inclusion proof alone does not reconcile this spent output.
        assert any(coin["txid"] == funding for coin in wallet.listunspent())
        try:
            wallet.importprunedfunds(spender_raw, spend_proof)
            raise AssertionError("Pure outgoing import must need a rescan")
        except JSONRPCException as error:
            assert error.error["code"] == -5
        peer = next(peer["id"] for peer in local.rpc.getpeerinfo() if not peer["inbound"] and int(peer["services"], 16) & 9 == 9)
        local.rpc.getblockfrompeer(spend_block, peer)
        wait(lambda: local.rpc.getblock(spend_block, 0))
        assert local.rpc.getblockchaininfo()["pruneheight"] == before
        scanned = wallet.rescanblockchain(302, 302)
        assert scanned == {"start_height": 302, "stop_height": 302}
        assert wallet.gettransaction(spender, True)["confirmations"] > 0
        assert not any(coin["txid"] == funding for coin in wallet.listunspent())
        local.rpc.pruneblockchain(local.rpc.getblockcount() - 288)
        local.stop()
        local.start()
        assert local.rpc.listwallets() == []  # -nowallet keeps wallet RPC usable.
        local.rpc.loadwallet("recovery")
        assert local.wallet("recovery").gettransaction(spender, True)["confirmations"] > 0
        print(f"PASS {archive.rpc.getnetworkinfo()['subversion']}: proof import, spend reconciliation, fetched-block scan, restart persistence", flush=True)
    finally:
        for node in reversed(nodes):
            node.stop()
        shutil.rmtree(root)


if __name__ == "__main__":
    exercise(str(Path(sys.argv[1]).resolve(strict=True)))
