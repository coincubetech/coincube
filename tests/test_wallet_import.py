"""Imported-wallet lifecycle against disposable Bitcoin and Esplora/Electrum nodes.

Set WALLET_IMPORT_ELECTRS_PATH and COINCUBED_PATH to the binaries under test.
WALLET_IMPORT_REGTEST_REQUIRED=1 makes missing binaries a failure instead of a
skip. No default wallet, datadir or external service is used by this harness.
"""

from concurrent.futures import ThreadPoolExecutor
from decimal import Decimal
from pathlib import Path
import json
import os
import shutil
import sqlite3
import subprocess

from bip32 import BIP32
from bip32.utils import _pubkey_to_fingerprint
import pytest

from test_framework.bitcoind import Bitcoind, BitcoindRpcInterface
from test_framework.coincubed import Coincubed
from test_framework.esplora import EsploraElectrs
from test_framework.serializations import PSBT
from test_framework.utils import (
    COINCUBED_PATH,
    RpcError,
    TailableProc,
    UnixDomainSocketRpc,
    wait_for,
)

SHAPES = ("pkh", "sh-wpkh", "wpkh", "multi", "sortedmulti")


def required_binary(value, name):
    path = shutil.which(value) if value else None
    if path:
        return path
    message = f"{name} must name an executable for imported-wallet regtest"
    if os.getenv("WALLET_IMPORT_REGTEST_REQUIRED") == "1":
        pytest.fail(message)
    pytest.skip(message)


@pytest.fixture(scope="module")
def import_chain(tmp_path_factory):
    binary = required_binary(os.getenv("BITCOIND_PATH", "bitcoind"), "BITCOIND_PATH")
    indexer = required_binary(
        os.getenv("WALLET_IMPORT_ELECTRS_PATH"), "WALLET_IMPORT_ELECTRS_PATH"
    )
    required_binary(COINCUBED_PATH, "COINCUBED_PATH")
    root = tmp_path_factory.mktemp("import-chain")
    node = Bitcoind(str(root / "bitcoin"), bitcoind_path=binary)
    esplora = None
    try:
        node.startup()
        node.node_rpc.createwallet(
            node.rpc.wallet_name, False, False, "", False, True, True
        )
        node.rpc.generatetoaddress(121, node.rpc.getnewaddress())
        esplora = EsploraElectrs(
            str(root / "indexer"), node.bitcoin_dir, node.rpcport, electrs_path=indexer
        )
        esplora.startup()
        esplora.wait_for_tip(node.rpc.getbestblockhash(), timeout=120)
        yield node, esplora
    finally:
        if esplora is not None and esplora.proc is not None:
            esplora.cleanup()
        if node.proc is not None:
            node.cleanup()


def descriptor_keys(shape, branch, private_indices=(), seed_offset=41):
    """Independent disposable keys; origins match real account depths."""
    count = 3 if shape in ("multi", "sortedmulti") else 1
    purpose = {"pkh": 44, "sh-wpkh": 49, "wpkh": 84}.get(shape, 48)
    path = f"m/{purpose}'/1'/0'" + ("/2'" if count == 3 else "")
    keys = []
    for index in range(count):
        hd = BIP32.from_seed(bytes([seed_offset + index]) * 32, network="test")
        fingerprint = _pubkey_to_fingerprint(hd.pubkey).hex()
        xkey = (
            hd.get_xpriv_from_path(path)
            if index in private_indices
            else hd.get_xpub_from_path(path)
        )
        keys.append(f"[{fingerprint}/{path[2:]}]{xkey}/{branch}/*")
    if shape == "sh-wpkh":
        return f"sh(wpkh({keys[0]}))"
    if shape in ("multi", "sortedmulti"):
        return f"wsh({shape}(2,{','.join(keys)}))"
    return f"{shape}({keys[0]})"


def checked_descriptor(node, descriptor):
    return node.rpc.getdescriptorinfo(descriptor)["descriptor"]


def signing_wallet(node, name, shape, private_indices):
    node.node_rpc.createwallet(name, False, True, "", False, True, True)
    rpc = BitcoindRpcInterface(node.bitcoin_dir, "regtest", node.rpcport, name)
    imports = []
    for branch in (0, 1):
        descriptor = descriptor_keys(shape, branch, private_indices)
        checksum = node.rpc.getdescriptorinfo(descriptor)["checksum"]
        imports.append(
            {
                "desc": f"{descriptor}#{checksum}",
                "timestamp": 0,
                "active": False,
                "internal": branch == 1,
                "range": [0, 1000],
            }
        )
    assert all(result["success"] for result in rpc.importdescriptors(imports))
    return rpc


class ImportedDaemon(Coincubed):
    """Reuse the normal process/RPC lifecycle with versioned imported config."""

    def __init__(self, root, receive, change, indexer, backend, lookahead=300):
        root.mkdir(parents=True)
        TailableProc.__init__(self, str(root), verbose=False)
        self.datadir = str(root)
        self.prefix = root.name
        self._poll_interval_secs = 1
        self.lookahead = lookahead
        self.conf_file = str(root / "daemon.toml")
        self.cmd_line = [COINCUBED_PATH, "--conf", self.conf_file]
        self.legacy_socket_path = str(root / "data" / "coincubed_rpc")
        self.rpc = UnixDomainSocketRpc(self.legacy_socket_path)
        data = {
            "kind": "imported",
            "version": 1,
            "chain": "regtest",
            "receive": receive,
            "change": change,
        }
        # JSON scalar strings are also TOML basic strings. Keep the imported
        # representation inline so the following scan keys remain at the root.
        self.record = ", ".join(
            f"{key} = {json.dumps(value)}" for key, value in data.items()
        )
        self.use_backend(backend, indexer)

    def use_backend(self, backend, source):
        """(Re)write the config for `backend` on the same data directory.

        `source` is the indexer for "esplora"/"electrum" and the `Bitcoind` node
        for "bitcoind" (the local node an imported wallet moves to)."""
        root = Path(self.datadir)
        config = (
            f"data_directory = {json.dumps(str(root / 'data'))}\n"
            'log_level = "DEBUG"\n'
            f"main_descriptor = {{ {self.record} }}\n"
            "import_birthday = 0\n"
            f"import_initial_lookahead = {self.lookahead}\n"
            '[bitcoin_config]\nnetwork = "regtest"\npoll_interval_secs = 1\n'
        )
        if backend == "esplora":
            config += f"[esplora_config]\naddr = {json.dumps(source.url)}\n"
        elif backend == "electrum":
            config += (
                "[electrum_config]\n"
                f'addr = "tcp://127.0.0.1:{source.electrum_port}"\n'
                "validate_domain = false\n"
            )
        elif backend == "bitcoind":
            cookie = Path(source.bitcoin_dir) / "regtest" / ".cookie"
            config += (
                "[bitcoind_config]\n"
                f"cookie_path = {json.dumps(str(cookie))}\n"
                f'addr = "127.0.0.1:{source.rpcport}"\n'
            )
        else:
            raise ValueError(backend)
        self.backend = backend
        Path(self.conf_file).write_text(config)

    @property
    def watchonly_wallet(self):
        return str(Path(self.datadir) / "data" / "coincubed_watchonly_wallet")

    def wait_ready_on_node(self, height):
        """`wait_ready` for the local node: no Connect scan runs there, so the
        persisted history-complete state is checked instead of scan progress."""

        def ready():
            info = self.rpc.getinfo()
            return (
                info["block_height"] == height
                and info.get("imported_wallet", {}).get("history_complete")
            )

        wait_for(ready, timeout=120)
        return self.rpc.getinfo()

    def wait_ready(self, height):
        def ready():
            info = self.rpc.getinfo()
            status = info.get("imported_wallet", {})
            return (
                info["block_height"] == height
                and info["sync"] == 1
                and status.get("history_complete")
                and status.get("receive_scanned", 0) >= self.lookahead
                and status.get("change_scanned", 0) >= self.lookahead
            )

        wait_for(ready, timeout=120)
        return self.rpc.getinfo()


@pytest.mark.parametrize("shape", SHAPES)
@pytest.mark.parametrize("backend", ("esplora", "electrum"))
def test_import_history_send_and_restart(import_chain, tmp_path, shape, backend):
    node, indexer = import_chain
    receive = checked_descriptor(node, descriptor_keys(shape, 0))
    change = checked_descriptor(node, descriptor_keys(shape, 1))
    prefix = f"{shape}-{backend}"
    owner = signing_wallet(node, f"source-{prefix}", shape, (0, 1, 2))
    addresses = [
        node.rpc.deriveaddresses(receive, [0, 0])[0],
        node.rpc.deriveaddresses(change, [5, 5])[0],
        node.rpc.deriveaddresses(receive, [250, 250])[0],
    ]
    funding = node.rpc.sendmany(
        "", dict(zip(addresses, (Decimal("0.01"), Decimal("0.02"), Decimal("0.03"))))
    )
    node.generate_block(1, wait_for_mempool=funding)
    raw_funding = node.rpc.gettransaction(funding)["hex"]
    outputs = node.rpc.decoderawtransaction(raw_funding)["vout"]
    spent_vout = next(
        v["n"] for v in outputs if v["scriptPubKey"]["address"] == addresses[0]
    )
    prior_spend = node.rpc.createrawtransaction(
        [{"txid": funding, "vout": spent_vout}],
        {node.rpc.getnewaddress(): Decimal("0.00999")},
    )
    signed_prior = owner.signrawtransactionwithwallet(prior_spend)
    assert signed_prior["complete"]
    spent_txid = node.rpc.sendrawtransaction(signed_prior["hex"])
    node.generate_block(1, wait_for_mempool=spent_txid)
    indexer.wait_for_tip(
        node.rpc.getbestblockhash(), timeout=120, confirmed_txids=(funding, spent_txid)
    )
    daemon = ImportedDaemon(tmp_path / "imported", receive, change, indexer, backend)
    try:
        daemon.start()
        initial = daemon.wait_ready(node.rpc.getblockcount())
        identity = initial["imported_wallet"]["wallet_identity"]
        assert len(identity) == 64
        coins = daemon.rpc.listcoins()["coins"]
        funded_coins = [
            coin for coin in coins if coin["outpoint"].startswith(funding + ":")
        ]
        by_address = {coin["address"]: coin for coin in funded_coins}
        assert set(addresses) <= set(by_address)
        assert by_address[addresses[0]]["spend_info"]["txid"] == spent_txid
        assert by_address[addresses[1]]["is_change"]
        assert by_address[addresses[2]]["derivation_index"] == 250
        history = daemon.rpc.listconfirmed(0, 2**32 - 1, 100)["transactions"]
        history_ids = {
            node.rpc.decoderawtransaction(item["tx"])["txid"] for item in history
        }
        assert {funding, spent_txid} <= history_ids

        with ThreadPoolExecutor(max_workers=8) as pool:
            reserved = list(pool.map(lambda _: daemon.rpc.getnewaddress(), range(20)))
        assert len({entry["address"] for entry in reserved}) == 20
        largest = max(entry["derivation_index"] for entry in reserved)
        assert largest >= 270
        outpoints = [
            coin["outpoint"] for coin in funded_coins if coin["spend_info"] is None
        ]
        recipient = node.rpc.getnewaddress()
        first = daemon.rpc.createspend({recipient: 100_000}, outpoints, 2)["psbt"]
        # Creation must persist the exact construction before any signer sees it.
        unsigned_id = node.rpc.decodepsbt(first)["tx"]["txid"]
        assert daemon.rpc.listspendtxs([unsigned_id])["spend_txs"][0]["psbt"] == first
        unknown = PSBT.from_base64(first)
        unknown.tx.nLockTime += 1
        unknown.g.map[0] = unknown.tx.serialize_without_witness()
        serialized_psbt = unknown.to_base64()
        with pytest.raises(RpcError):
            daemon.rpc.updatespend(serialized_psbt)
        altered = PSBT.from_base64(first)
        origin_key = next(iter(altered.i[0].map[6]))
        altered.i[0].map[6][origin_key] = bytes([0]) * len(
            altered.i[0].map[6][origin_key]
        )
        serialized_psbt = altered.to_base64()
        with pytest.raises(RpcError):
            daemon.rpc.updatespend(serialized_psbt)
        daemon.rpc.updatespend(first)
        second = daemon.rpc.createspend({recipient: 100_000}, outpoints, 2)["psbt"]
        first_tx = node.rpc.decodepsbt(first)["tx"]
        second_tx = node.rpc.decodepsbt(second)["tx"]
        assert (
            first_tx["vout"] != second_tx["vout"]
        ), "drafts must reserve distinct change"
        txid = first_tx["txid"]
        if shape in ("multi", "sortedmulti"):
            signer_a = signing_wallet(node, f"signer-a-{prefix}", shape, (0,))
            signer_b = signing_wallet(node, f"signer-b-{prefix}", shape, (1,))
            partial = signer_a.walletprocesspsbt(first, True, "ALL", True, False)[
                "psbt"
            ]
            daemon.rpc.updatespend(partial)
            daemon.stop()
            daemon.start()
            daemon.wait_ready(node.rpc.getblockcount())
            saved = daemon.rpc.listspendtxs([txid])["spend_txs"][0]["psbt"]
            signed = signer_b.walletprocesspsbt(saved, True, "ALL", True, False)["psbt"]
        else:
            signed = owner.walletprocesspsbt(first, True, "ALL", True, False)["psbt"]
        daemon.rpc.updatespend(signed)
        reference = node.rpc.finalizepsbt(signed)
        assert reference["complete"]
        final_txid = node.rpc.decoderawtransaction(reference["hex"])["txid"]
        daemon.rpc.broadcastspend(txid)
        broadcast = daemon.rpc.listspendtxs([txid])["spend_txs"][0]
        assert broadcast["broadcast_txid"] == final_txid
        # Retrying after a transport interruption uses the recorded final id.
        daemon.rpc.broadcastspend(txid)
        node.generate_block(1, wait_for_mempool=final_txid)
        indexer.wait_for_tip(
            node.rpc.getbestblockhash(), timeout=120, confirmed_txids=(final_txid,)
        )
        daemon.wait_ready(node.rpc.getblockcount())
        daemon.stop()
        daemon.start()
        restarted = daemon.wait_ready(node.rpc.getblockcount())
        assert restarted["imported_wallet"]["wallet_identity"] == identity
        assert daemon.rpc.getnewaddress()["derivation_index"] > largest
        persisted = daemon.rpc.listspendtxs([txid])["spend_txs"][0]
        assert persisted["broadcast_txid"] == final_txid
    finally:
        if daemon.proc is not None:
            daemon.cleanup()


@pytest.mark.parametrize("backend", ("esplora", "electrum"))
def test_empty_import_is_complete_and_preserves_allocations(
    import_chain, tmp_path, backend
):
    node, indexer = import_chain
    receive = checked_descriptor(node, descriptor_keys("wpkh", 0, seed_offset=81))
    change = checked_descriptor(node, descriptor_keys("wpkh", 1, seed_offset=81))
    daemon = ImportedDaemon(
        tmp_path / "empty", receive, change, indexer, backend, lookahead=40
    )
    try:
        daemon.start()
        info = daemon.wait_ready(node.rpc.getblockcount())
        assert info["imported_wallet"]["history_complete"]
        assert daemon.rpc.listcoins()["coins"] == []
        assert daemon.rpc.listconfirmed(0, 2**32 - 1, 100)["transactions"] == []
        first = daemon.rpc.getnewaddress()
        assert first["address"] == node.rpc.deriveaddresses(receive, [1, 1])[0]
        assert first["derivation_index"] == 1
        daemon.stop()
        daemon.start()
        daemon.wait_ready(node.rpc.getblockcount())
        second = daemon.rpc.getnewaddress()
        assert second["derivation_index"] == first["derivation_index"] + 1
        assert second["address"] == node.rpc.deriveaddresses(receive, [2, 2])[0]
    finally:
        if daemon.proc is not None:
            daemon.cleanup()


@pytest.mark.parametrize("backend", ("esplora", "electrum"))
@pytest.mark.parametrize("operation", ("restore", "expand"))
def test_import_recovers_history_after_saved_scan_request(
    import_chain, tmp_path, backend, operation
):
    tool = required_binary(
        os.getenv("WALLET_IMPORT_STATE_TOOL_PATH"), "WALLET_IMPORT_STATE_TOOL_PATH"
    )
    node, indexer = import_chain
    seed_offset = 91 if operation == "restore" else 92
    receive = checked_descriptor(
        node, descriptor_keys("wpkh", 0, seed_offset=seed_offset)
    )
    change = checked_descriptor(
        node, descriptor_keys("wpkh", 1, seed_offset=seed_offset)
    )
    receive_index, change_index = (45, 32) if operation == "restore" else (250, 250)
    addresses = [
        node.rpc.deriveaddresses(receive, [receive_index, receive_index])[0],
        node.rpc.deriveaddresses(change, [change_index, change_index])[0],
    ]
    funding = node.rpc.sendmany("", {address: Decimal("0.01") for address in addresses})
    node.generate_block(1, wait_for_mempool=funding)
    indexer.wait_for_tip(
        node.rpc.getbestblockhash(), timeout=120, confirmed_txids=(funding,)
    )
    daemon = ImportedDaemon(
        tmp_path / operation, receive, change, indexer, backend, lookahead=20
    )
    try:
        if operation == "expand":
            daemon.start()
            daemon.wait_ready(node.rpc.getblockcount())
            assert not any(
                coin["outpoint"].startswith(funding + ":")
                for coin in daemon.rpc.listcoins()["coins"]
            )
            daemon.stop()
        # A Kit restores into unpublished staged storage; wider discovery
        # updates an existing stopped wallet. Both persist before startup.
        arguments = [tool, daemon.conf_file, operation]
        arguments += ["45", "32"] if operation == "restore" else ["300"]
        subprocess.run(arguments, check=True, capture_output=True, text=True)
        # The persisted request, not live memory, must cause scanning after restart.
        with sqlite3.connect(Path(daemon.datadir) / "data" / "coincubed.sqlite3") as db:
            assert (
                db.execute("SELECT import_history_complete FROM wallets").fetchone()[0]
                == 0
            )
        if operation == "expand":
            daemon.lookahead = 300
        daemon.start()
        info = daemon.wait_ready(node.rpc.getblockcount())
        coins = daemon.rpc.listcoins()["coins"]
        assert set(addresses) <= {
            coin["address"]
            for coin in coins
            if coin["outpoint"].startswith(funding + ":")
        }
        if operation == "restore":
            assert info["imported_wallet"]["receive_scanned"] >= 65
            assert info["imported_wallet"]["change_scanned"] >= 52
        else:
            assert info["imported_wallet"]["discovery_target"] == 300
        assert daemon.rpc.getnewaddress()["derivation_index"] > receive_index
    finally:
        if daemon.proc is not None:
            daemon.cleanup()


def test_complete_import_moves_to_local_node(import_chain, tmp_path):
    """Scan on Esplora until history is complete, then run the same data
    directory on the local node after transferring the funding records the way
    the GUI does (importprunedfunds), and keep tracking beyond Core's range."""
    node, indexer = import_chain
    receive = checked_descriptor(node, descriptor_keys("wpkh", 0, seed_offset=111))
    change = checked_descriptor(node, descriptor_keys("wpkh", 1, seed_offset=111))
    addresses = [
        node.rpc.deriveaddresses(receive, [0, 0])[0],
        node.rpc.deriveaddresses(change, [5, 5])[0],
        node.rpc.deriveaddresses(receive, [250, 250])[0],
    ]
    funding = node.rpc.sendmany(
        "", dict(zip(addresses, (Decimal("0.01"), Decimal("0.02"), Decimal("0.03"))))
    )
    node.generate_block(1, wait_for_mempool=funding)
    indexer.wait_for_tip(
        node.rpc.getbestblockhash(), timeout=120, confirmed_txids=(funding,)
    )

    def snapshot(daemon):
        return sorted(
            (
                coin["outpoint"],
                coin["amount"],
                coin["address"],
                coin["derivation_index"],
                coin["is_change"],
            )
            for coin in daemon.rpc.listcoins()["coins"]
        )

    daemon = ImportedDaemon(tmp_path / "local-node", receive, change, indexer, "esplora")
    try:
        daemon.start()
        connect = daemon.wait_ready(node.rpc.getblockcount())
        assert connect["imported_wallet"]["history_complete"]
        before = snapshot(daemon)
        assert {coin[2] for coin in before} == set(addresses)
        daemon.stop()

        daemon.use_backend("bitcoind", node)
        daemon.start()
        local = daemon.wait_ready_on_node(node.rpc.getblockcount())
        assert (
            local["imported_wallet"]["wallet_identity"]
            == connect["imported_wallet"]["wallet_identity"]
        )
        watchonly = BitcoindRpcInterface(
            node.bitcoin_dir, "regtest", node.rpcport, daemon.watchonly_wallet
        )
        watchonly.importprunedfunds(
            node.rpc.gettransaction(funding)["hex"], node.rpc.gettxoutproof([funding])
        )
        wait_for(lambda: snapshot(daemon) == before)

        # A receive beyond Core's default range of 1000 is tracked once the
        # allocated addresses bring the range end within the discovery target.
        while daemon.rpc.getnewaddress()["derivation_index"] < 1000 - daemon.lookahead:
            pass
        node.generate_block(1)
        daemon.wait_for_log("Widening the imported wallet's descriptor range")
        deep_address = node.rpc.deriveaddresses(receive, [1150, 1150])[0]
        deep = node.rpc.sendtoaddress(deep_address, Decimal("0.04"))
        node.generate_block(1, wait_for_mempool=deep)
        wait_for(
            lambda: any(
                coin["outpoint"].startswith(deep + ":")
                and coin["derivation_index"] == 1150
                for coin in daemon.rpc.listcoins()["coins"]
            )
        )
    finally:
        if daemon.proc is not None:
            daemon.cleanup()


def test_incomplete_import_is_refused_on_local_node(import_chain, tmp_path):
    node, indexer = import_chain
    receive = checked_descriptor(node, descriptor_keys("wpkh", 0, seed_offset=112))
    change = checked_descriptor(node, descriptor_keys("wpkh", 1, seed_offset=112))
    daemon = ImportedDaemon(tmp_path / "incomplete", receive, change, indexer, "esplora")
    try:
        daemon.use_backend("bitcoind", node)
        TailableProc.start(daemon)
        daemon.wait_for_log("history scan is not complete")
        assert daemon.proc.wait(30) != 0
        assert daemon.watchonly_wallet not in node.node_rpc.listwallets()
    finally:
        if daemon.proc is not None and daemon.proc.poll() is None:
            daemon.cleanup()
