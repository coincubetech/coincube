"""Imported wallets on the local Bitcoin Core/Knots node, without an indexer.

The full Connect-scan-then-switch flow lives in test_wallet_import.py and needs
WALLET_IMPORT_ELECTRS_PATH. These tests cover the node side on its own: a
history-incomplete imported wallet is refused, and one whose history is
recorded complete runs on the node with an explicit, widening descriptor range.
"""

from decimal import Decimal
from pathlib import Path
import sqlite3

import pytest

from fixtures import *  # noqa: F401,F403 (the `bitcoind` fixture)
from test_framework.bitcoind import BitcoindRpcInterface
from test_framework.utils import COINCUBED_PATH, TailableProc, wait_for
from test_wallet_import import (
    ImportedDaemon,
    checked_descriptor,
    descriptor_keys,
    required_binary,
)

# What a wallet opened on a dead Esplora endpoint points at; only its storage
# matters to these tests.
UNREACHABLE_ESPLORA = type("Unreachable", (), {"url": "http://127.0.0.1:9"})()

LOOKAHEAD = 300
# The highest receive index funded before widening. Core extends a branch by
# itself to an index it sees used plus its keypool of 1000.
FUNDED_INDEX = 100
# Past that own extension (1100), so only the daemon's widening covers it.
DEEP_INDEX = 1_150


def create_imported_storage(daemon):
    """Let coincubed create the imported database, as a first start would,
    without needing a reachable Connect/Esplora backend."""
    daemon.use_backend("esplora", UNREACHABLE_ESPLORA)
    TailableProc.start(daemon)
    db_path = Path(daemon.datadir) / "data" / "coincubed.sqlite3"
    wait_for(lambda: db_path.exists() and daemon.is_in_log("Database initialized"))
    TailableProc.stop(daemon)
    return db_path


def mark_history_complete(db_path):
    """Fixture shortcut for the Connect scan (complete_imported_history_for_test
    does the same in Rust): record both branches scanned past the target."""
    with sqlite3.connect(db_path) as db:
        target = db.execute("SELECT import_initial_lookahead FROM wallets").fetchone()[
            0
        ]
        db.execute(
            "UPDATE wallets SET import_receive_scanned = ?, import_change_scanned = ?, "
            "import_history_complete = 1",
            (target, target),
        )


def imported_daemon(bitcoind, directory, seed_offset, lookahead=LOOKAHEAD):
    required_binary(COINCUBED_PATH, "COINCUBED_PATH")
    receive = checked_descriptor(
        bitcoind, descriptor_keys("wpkh", 0, seed_offset=seed_offset)
    )
    change = checked_descriptor(
        bitcoind, descriptor_keys("wpkh", 1, seed_offset=seed_offset)
    )
    daemon = ImportedDaemon(
        Path(directory) / "imported",
        receive,
        change,
        UNREACHABLE_ESPLORA,
        "esplora",
        lookahead=lookahead,
    )
    return daemon, receive, change


def watchonly_ranges(bitcoind, daemon):
    rpc = BitcoindRpcInterface(
        bitcoind.bitcoin_dir, "regtest", bitcoind.rpcport, daemon.watchonly_wallet
    )
    return [entry["range"] for entry in rpc.listdescriptors()["descriptors"]]


def test_incomplete_imported_wallet_is_refused_on_the_node(bitcoind, directory):
    daemon, _, _ = imported_daemon(bitcoind, directory, seed_offset=101)
    try:
        # Fresh imported storage: incomplete by definition.
        daemon.use_backend("bitcoind", bitcoind)
        TailableProc.start(daemon)
        daemon.wait_for_log("history scan is not complete")
        assert daemon.proc.wait(30) != 0
        assert not Path(daemon.watchonly_wallet).exists()
        # Existing storage whose Connect scan has not completed: still refused,
        # and nothing is created on the node.
        create_imported_storage(daemon)
        daemon.use_backend("bitcoind", bitcoind)
        TailableProc.start(daemon)
        daemon.wait_for_log("history scan is not complete")
        assert daemon.proc.wait(30) != 0
        assert daemon.watchonly_wallet not in bitcoind.node_rpc.listwallets()
    finally:
        if daemon.proc is not None and daemon.proc.poll() is None:
            daemon.cleanup()


def test_complete_imported_wallet_runs_on_the_node(bitcoind, directory):
    daemon, receive, change = imported_daemon(bitcoind, directory, seed_offset=102)
    try:
        mark_history_complete(create_imported_storage(daemon))
        daemon.use_backend("bitcoind", bitcoind)
        daemon.start()
        daemon.wait_ready_on_node(bitcoind.rpc.getblockcount())
        # Explicit range: Core's default 1000 at least, covering the target.
        assert watchonly_ranges(bitcoind, daemon) == [[0, 1000], [0, 1000]]

        addresses = {
            bitcoind.rpc.deriveaddresses(receive, [0, 0])[0]: Decimal("0.01"),
            bitcoind.rpc.deriveaddresses(change, [5, 5])[0]: Decimal("0.02"),
            bitcoind.rpc.deriveaddresses(receive, [FUNDED_INDEX, FUNDED_INDEX])[
                0
            ]: Decimal("0.03"),
        }
        funding = bitcoind.rpc.sendmany("", addresses)
        bitcoind.generate_block(1, wait_for_mempool=funding)

        def funded():
            return {
                coin["address"]
                for coin in daemon.rpc.listcoins()["coins"]
                if coin["outpoint"].startswith(funding + ":")
            }

        wait_for(lambda: funded() == set(addresses))
        indices = {
            coin["address"]: coin["derivation_index"]
            for coin in daemon.rpc.listcoins()["coins"]
        }
        funded_address = bitcoind.rpc.deriveaddresses(
            receive, [FUNDED_INDEX, FUNDED_INDEX]
        )[0]
        assert indices[funded_address] == FUNDED_INDEX

        # Core extends a branch by itself only past indices it sees used.
        # Addresses handed out but not yet used need the daemon: allocating to
        # within the discovery target of the range end makes it widen the range,
        # with the same headroom again.
        # The threshold is the range both branches cover on the node, which
        # Core's own extensions (here 1100 and 1005) can already have moved.
        covered = min(end for _, end in watchonly_ranges(bitcoind, daemon))
        while daemon.rpc.getnewaddress()["derivation_index"] + LOOKAHEAD < covered:
            pass
        bitcoind.generate_block(1)
        highest = daemon.rpc.getnewaddress()["derivation_index"]
        daemon.wait_for_log("Widening the imported wallet's descriptor range")
        # Core also extends a branch by itself (its keypool past an index it sees
        # used), so each branch is checked against what the daemon must ensure.
        wait_for(
            lambda: all(
                end >= highest + LOOKAHEAD
                for _, end in watchonly_ranges(bitcoind, daemon)
            )
        )
        widened = watchonly_ranges(bitcoind, daemon)
        assert not daemon.is_in_log("Could not widen")

        # A receive at a handed-out address past Core's own extension is tracked.
        beyond = bitcoind.rpc.deriveaddresses(receive, [DEEP_INDEX, DEEP_INDEX])[0]
        assert widened[0][1] >= DEEP_INDEX
        deep = bitcoind.rpc.sendtoaddress(beyond, Decimal("0.04"))
        bitcoind.generate_block(1, wait_for_mempool=deep)
        wait_for(
            lambda: any(
                coin["outpoint"].startswith(deep + ":")
                and coin["derivation_index"] == DEEP_INDEX
                for coin in daemon.rpc.listcoins()["coins"]
            )
        )

        # A restart keeps the wider range (never shrinks it) and the coins.
        daemon.stop()
        daemon.start()
        daemon.wait_ready_on_node(bitcoind.rpc.getblockcount())
        restarted = watchonly_ranges(bitcoind, daemon)
        assert all(
            after[1] >= before[1] for before, after in zip(widened, restarted)
        )
        assert set(addresses) | {beyond} <= {
            coin["address"] for coin in daemon.rpc.listcoins()["coins"]
        }
    finally:
        if daemon.proc is not None and daemon.proc.poll() is None:
            daemon.cleanup()


@pytest.mark.parametrize("seed_offset", (103,))
def test_start_rescan_stays_unavailable_for_imported_on_the_node(
    bitcoind, directory, seed_offset
):
    daemon, _, _ = imported_daemon(bitcoind, directory, seed_offset=seed_offset)
    try:
        mark_history_complete(create_imported_storage(daemon))
        daemon.use_backend("bitcoind", bitcoind)
        daemon.start()
        daemon.wait_ready_on_node(bitcoind.rpc.getblockcount())
        with pytest.raises(Exception, match="(?i)recovery"):
            daemon.rpc.startrescan(1296688602)
    finally:
        if daemon.proc is not None and daemon.proc.poll() is None:
            daemon.cleanup()


def test_node_maps_outputs_past_a_small_stored_lookahead(bitcoind, directory):
    """The GUI's default lookahead is 20, so the address table ends 20 past the
    highest index; outputs between there and the node's range end still map."""
    daemon, receive, change = imported_daemon(
        bitcoind, directory, seed_offset=104, lookahead=20
    )
    try:
        mark_history_complete(create_imported_storage(daemon))
        daemon.use_backend("bitcoind", bitcoind)
        daemon.start()
        daemon.wait_ready_on_node(bitcoind.rpc.getblockcount())
        expected = {
            bitcoind.rpc.deriveaddresses(receive, [50, 50])[0]: (50, False),
            bitcoind.rpc.deriveaddresses(change, [120, 120])[0]: (120, True),
        }
        funding = bitcoind.rpc.sendmany(
            "", {address: Decimal("0.01") for address in expected}
        )
        bitcoind.generate_block(1, wait_for_mempool=funding)

        def mapped():
            return {
                coin["address"]: (coin["derivation_index"], coin["is_change"])
                for coin in daemon.rpc.listcoins()["coins"]
                if coin["outpoint"].startswith(funding + ":")
            }

        wait_for(lambda: mapped() == expected)

        # Core extends the receive branch by itself past an index it sees used
        # (500 -> 1500) while the daemon last imported 1000 and has no reason to
        # widen (500 + 20 is far from it). An output in Core's extension still
        # maps, because each poll re-reads the node's ranges.
        used = bitcoind.rpc.sendtoaddress(
            bitcoind.rpc.deriveaddresses(receive, [500, 500])[0], Decimal("0.01")
        )
        bitcoind.generate_block(1, wait_for_mempool=used)
        wait_for(
            lambda: max(end for _, end in watchonly_ranges(bitcoind, daemon)) >= 1_500
        )
        assert not daemon.is_in_log("Widening the imported wallet's descriptor range")
        extended = bitcoind.rpc.sendtoaddress(
            bitcoind.rpc.deriveaddresses(receive, [1_200, 1_200])[0], Decimal("0.01")
        )
        bitcoind.generate_block(1, wait_for_mempool=extended)
        wait_for(
            lambda: any(
                coin["outpoint"].startswith(extended + ":")
                and coin["derivation_index"] == 1_200
                for coin in daemon.rpc.listcoins()["coins"]
            )
        )
    finally:
        if daemon.proc is not None and daemon.proc.poll() is None:
            daemon.cleanup()
