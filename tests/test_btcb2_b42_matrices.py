"""Bitcoin Blake2b (BTCB2) QA matrices — launch-ga Lane B4.2, the rows the
merged code can exercise today.

These sit on B4.1's two-chain fixture (`tests/test_framework/btcb2.py`) and
cover the two matrices that need real nodes and indexers:

* **isolation (I2)** — a Bitcoin Cube and a BTCB2 Cube on one machine: distinct
  datadirs and ports, both syncing, each stack restartable without disturbing
  the other;
* **backend (correction 4)** — a BTCB2 Cube whose only backend is an Esplora
  indexer: when that indexer stops, the Cube reports its own chain's last known
  state and **never** a Bitcoin balance.

The other two B4.2 matrices (Bitcoin regression, reachable replay rows) are
Rust-side and live with the code they test; `WORK_LOGS/LAUNCH_GA/B4/` records
which test covers which row, including the rows that are deferred and why.

Both matrices live in **one module**: `two_chain` is module-scoped, so a second
module means a second full two-node/two-indexer/two-daemon bring-up — about 90 s
of wall-clock and a second exposure to the setup-phase flake in #394 — for no
coverage gain. Splitting is safe at this head; a future splitter should read
#489 first, which records what the fixture layout would have to change for that
to stop being true.

Skipped unless the three harness binaries are configured (see tests/README.md).
"""

import os
import time

import pytest

from fixtures import *
from test_framework.utils import TIMEOUT, wait_for

# A sole provider retries after 5s, but the poller can sleep for 30s after
# exhausting its providers. Allow two poller sleeps plus 5s for local requests;
# retaining the former 120s transport cooldown must fail this recovery gate.
SOLE_PROVIDER_RECOVERY_BUDGET_SECS = 65


def _confirmed_outpoints(daemon):
    return {
        (c["outpoint"].split(":")[0], int(c["outpoint"].split(":")[1]))
        for c in daemon.rpc.listcoins()["coins"]
        if c["block_height"] is not None
    }


def _prefork_set(two_chain):
    return {(txid, vout) for txid, vout, _ in two_chain.prefork_outpoints}


def _poison(two_chain):
    return (two_chain.poison_outpoint[0], two_chain.poison_outpoint[1])


# ── isolation matrix (I2) ────────────────────────────────────────────────


def test_i2_one_machine_two_cubes_keep_every_path_and_port_apart(two_chain):
    """I2.1 — two Cubes on one machine share no datadir, port or socket."""
    a, b = two_chain.legacy, two_chain.blake2b
    ea, eb = two_chain.electrs_legacy, two_chain.electrs_blake2b
    da, db = two_chain.coincubed_legacy, two_chain.coincubed_blake2b

    paths = list(two_chain.datadir_claims().values())
    assert len(set(os.path.realpath(p) for p in paths)) == len(paths), paths
    # No datadir is nested inside another: "distinct" must not mean "one inside
    # the other", which would share a lock namespace.
    reals = sorted(os.path.realpath(p) for p in paths)
    for outer, inner in zip(reals, reals[1:]):
        assert not inner.startswith(outer + os.sep), (outer, inner)

    ports = [
        a.rpcport,
        b.rpcport,
        ea.http_port,
        ea.electrum_port,
        ea.monitoring_port,
        eb.http_port,
        eb.electrum_port,
        eb.monitoring_port,
    ]
    assert len(set(ports)) == len(ports), ports
    assert da.rpc.socket_path != db.rpc.socket_path

    # Both are live and each answers for its own chain.
    assert a.rpc.getblockcount() > 0 and b.rpc.getblockcount() > 0
    assert a.rpc.getbestblockhash() != b.rpc.getbestblockhash()


def test_i2_each_stack_restarts_without_disturbing_the_other(two_chain):
    """I2.2 — stop and restart each node; the other keeps serving throughout.

    The failure this exists to catch is a shared path or port that only shows
    up on a restart: a second stack that cannot come back while the first is
    running, or one whose restart takes the other down with it.
    """
    a, b = two_chain.legacy, two_chain.blake2b
    tip_a, tip_b = a.rpc.getbestblockhash(), b.rpc.getbestblockhash()
    count_a, count_b = a.rpc.getblockcount(), b.rpc.getblockcount()

    # Bitcoin side down and back up: the BTCB2 node is untouched meanwhile.
    a.stop()
    assert b.rpc.getbestblockhash() == tip_b
    assert b.rpc.getblockcount() == count_b
    a.start()
    assert a.rpc.getbestblockhash() == tip_a
    assert a.rpc.getblockcount() == count_a

    # And the reverse.
    b.stop()
    assert a.rpc.getbestblockhash() == tip_a
    assert a.rpc.getblockcount() == count_a
    b.start()
    assert b.rpc.getbestblockhash() == tip_b
    assert b.rpc.getblockcount() == count_b

    # The two chains still disagree exactly where they did before.
    n = two_chain.activation_height
    assert a.rpc.getblockhash(n - 1) == b.rpc.getblockhash(n - 1)
    assert a.rpc.getblockhash(n) != b.rpc.getblockhash(n)

    two_chain.assert_home_sandbox_untouched()


# ── backend matrix (correction 4) ────────────────────────────────────────


def test_backend_loss_never_shows_a_bitcoin_balance(two_chain):
    """Backend.1 — with its indexer stopped, a BTCB2 Cube never reports the
    Bitcoin chain's coins, and recovers when the indexer returns.

    The Bitcoin-only post-fork coin (`poison_outpoint`) is the probe: it exists
    on the Bitcoin chain and can never exist on the BLAKE2b one. If it ever
    appears in the BTCB2 Cube's coin set — with the backend up, down, or coming
    back — the Cube is reading the wrong chain.

    The "height did not move" assertion is only worth something if the same
    daemon demonstrably *does* move when the backend is up, so the positive
    control below runs first and the negative is held for many poll intervals
    rather than sampled once.
    """
    b, db = two_chain.blake2b, two_chain.coincubed_blake2b
    da = two_chain.coincubed_legacy
    prefork, poison = _prefork_set(two_chain), _poison(two_chain)
    poll = db.poll_interval_secs

    # Baseline: the BTCB2 Cube holds the shared pre-fork coins and not the
    # Bitcoin-only one; the Bitcoin Cube holds both.
    wait_for(lambda: prefork <= _confirmed_outpoints(db), timeout=TIMEOUT * 3)
    assert poison not in _confirmed_outpoints(db)
    wait_for(lambda: prefork | {poison} <= _confirmed_outpoints(da), timeout=TIMEOUT * 3)

    # Positive control: with the backend up the Cube follows its own chain
    # within a few polls. Without this the negative below passes on a daemon
    # that never advances for any reason.
    b.generate_block(1)
    two_chain.electrs_blake2b.wait_for_tip(
        b.rpc.getbestblockhash(), timeout=TIMEOUT * 3
    )
    wait_for(
        lambda: db.rpc.getinfo()["block_height"] == b.rpc.getblockcount(),
        timeout=TIMEOUT * 3,
    )
    height_before = db.rpc.getinfo()["block_height"]

    two_chain.electrs_blake2b.stop()
    try:
        # The Cube keeps answering — a dead backend must not take the daemon
        # down — and what it answers is still its own chain.
        coins = _confirmed_outpoints(db)
        assert prefork <= coins
        assert poison not in coins
        assert db.rpc.getinfo()["block_height"] == height_before

        # Mine on BLAKE2b with the indexer down. The Cube cannot follow: that
        # is the "backend unavailable" state, and it must stay stuck on its own
        # last known height rather than serve anything else. Held for well over
        # the poll interval that just proved it responsive.
        b.generate_block(2)
        deadline = time.time() + max(10 * poll, 10)
        while time.time() < deadline:
            info = db.rpc.getinfo()
            assert info["block_height"] == height_before, info
            assert info["block_height"] < two_chain.legacy.rpc.getblockcount()
            assert poison not in _confirmed_outpoints(db)
            time.sleep(poll)
    finally:
        two_chain.electrs_blake2b.start()

    # The indexer must first catch up before daemon recovery can be measured.
    two_chain.electrs_blake2b.wait_for_tip(
        b.rpc.getbestblockhash(), timeout=TIMEOUT * 3
    )
    # This is a daemon-level bound, not a promise of a five-second GUI refresh:
    # the provider cooldown is short, while the poller's retry sleep is 30s.
    recovery_started = time.monotonic()
    wait_for(
        lambda: db.rpc.getinfo()["block_height"] == b.rpc.getblockcount(),
        timeout=SOLE_PROVIDER_RECOVERY_BUDGET_SECS,
    )
    recovery_secs = time.monotonic() - recovery_started
    assert recovery_secs <= SOLE_PROVIDER_RECOVERY_BUDGET_SECS, recovery_secs
    print(
        f"BTCB2 Cube resumed {recovery_secs:.1f}s after the indexer returned "
        f"(sole-provider recovery budget {SOLE_PROVIDER_RECOVERY_BUDGET_SECS}s)"
    )
    assert db.rpc.getinfo()["block_height"] > height_before
    coins = _confirmed_outpoints(db)
    assert prefork <= coins
    assert poison not in coins


def test_direct_node_backend_outage_preserves_chain_and_recovers(two_chain):
    """B.5: direct node RPC backend isolation, outage and recovery.

    This exercises the backend used with a managed node, not the GUI launcher.
    The additional daemon has its own temporary database and control socket.
    """
    from test_framework.coincubed import Coincubed

    b = two_chain.blake2b
    directory = os.path.join(two_chain.directory, "coincubed-blake2b-direct")
    os.mkdir(directory)
    daemon = Coincubed(directory, two_chain.signer, two_chain.desc, b)
    daemon.env.update(two_chain.coincubed_blake2b.env)
    prefork, poison = _prefork_set(two_chain), _poison(two_chain)
    try:
        daemon.start()
        genesis_time = b.rpc.getblockheader(b.rpc.getblockhash(0))["time"]
        daemon.rpc.startrescan(genesis_time)
        wait_for(lambda: prefork <= _confirmed_outpoints(daemon), timeout=TIMEOUT * 3)
        assert poison not in _confirmed_outpoints(daemon)
        assert daemon.rpc.socket_path != two_chain.coincubed_blake2b.rpc.socket_path

        # Positive control before testing a stale height: prove this daemon can
        # advance through its own node and discover a new chain-local deposit.
        txid = b.rpc.sendtoaddress(two_chain.vault_addresses[0], 0.01)
        b.generate_block(1, wait_for_mempool=txid)
        wait_for(
            lambda: daemon.rpc.getinfo()["block_height"] == b.rpc.getblockcount()
            and any(outpoint[0] == txid for outpoint in _confirmed_outpoints(daemon)),
            timeout=TIMEOUT * 3,
        )
        before = daemon.rpc.getinfo()["block_height"]
        coins_before = _confirmed_outpoints(daemon)
        assert poison not in coins_before

        # The indexer is unrelated to this daemon; stop it to keep its own node
        # reconnect behavior out of this direct-RPC outage measurement.
        two_chain.electrs_blake2b.stop()
        try:
            outage_log_start = len(daemon.logs)
            b.stop()
            legacy_tip = two_chain.legacy.rpc.getbestblockhash()
            deadline = time.monotonic() + max(10 * daemon.poll_interval_secs, 10)
            while time.monotonic() < deadline:
                assert daemon.rpc.getinfo()["block_height"] == before
                assert _confirmed_outpoints(daemon) == coins_before
                assert two_chain.legacy.rpc.getbestblockhash() == legacy_tip
                time.sleep(daemon.poll_interval_secs)
            assert daemon.is_in_log(
                "Transient error when sending request to bitcoind",
                start=outage_log_start,
            ), "the daemon did not observe the node outage"
            assert daemon.rpc.getinfo()["block_height"] == before
            assert _confirmed_outpoints(daemon) == coins_before
        finally:
            if b.proc.poll() is not None:
                b.start()
            two_chain.electrs_blake2b.start()

        # No daemon restart, backend replacement or provider fallback: the same
        # process must recover through the same configured node backend.
        pid = daemon.proc.pid
        b.generate_block(2)
        wait_for(
            lambda: daemon.rpc.getinfo()["block_height"] == b.rpc.getblockcount(),
            timeout=TIMEOUT * 3,
        )
        assert daemon.proc.pid == pid
        assert daemon.rpc.getinfo()["block_height"] > before
        assert _confirmed_outpoints(daemon) == coins_before
        assert poison not in _confirmed_outpoints(daemon)
        two_chain.assert_home_sandbox_untouched()
    finally:
        daemon.cleanup()
