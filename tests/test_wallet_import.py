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
        record = ", ".join(
            f"{key} = {json.dumps(value)}" for key, value in data.items()
        )
        config = (
            f"data_directory = {json.dumps(str(root / 'data'))}\n"
            'log_level = "DEBUG"\n'
            f"main_descriptor = {{ {record} }}\n"
            "import_birthday = 0\n"
            f"import_initial_lookahead = {lookahead}\n"
            '[bitcoin_config]\nnetwork = "regtest"\npoll_interval_secs = 1\n'
        )
        if backend == "esplora":
            config += f"[esplora_config]\naddr = {json.dumps(indexer.url)}\n"
        else:
            config += (
                "[electrum_config]\n"
                f'addr = "tcp://127.0.0.1:{indexer.electrum_port}"\n'
                "validate_domain = false\n"
            )
        Path(self.conf_file).write_text(config)

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
        with pytest.raises(RpcError):
            daemon.rpc.updatespend(unknown.to_base64())
        altered = PSBT.from_base64(first)
        origin_key = next(iter(altered.i[0].map[6]))
        altered.i[0].map[6][origin_key] = bytes([0]) * len(
            altered.i[0].map[6][origin_key]
        )
        with pytest.raises(RpcError):
            daemon.rpc.updatespend(altered.to_base64())
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
        daemon.start()
        daemon.wait_ready(node.rpc.getblockcount())
        assert not any(
            coin["outpoint"].startswith(funding + ":")
            for coin in daemon.rpc.listcoins()["coins"]
        )
        daemon.stop()
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
