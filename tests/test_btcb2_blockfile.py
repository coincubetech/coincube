"""Concurrent `getblock` probe for the Knots block-file fault (#394).

The two-chain harness fails a few percent of Linux CI runs with three lines in
the node's own log::

    [error] Unable to seek to position 47310 of .../blocks/blk00000.dat
    [error] Unable to close file .../blocks/blk00000.dat
    [error] ReadRawBlock: OpenBlockFile failed for FlatFilePos(nFile=0, nPos=47318)

`getblock` then answers ``Block not found on disk``, and upstream electrs treats
a failed batch item as fatal in its fetcher thread, so one transient node error
reports as every module-scoped harness test erroring at setup.

This module exists because that chain is unreproducible by re-running the whole
harness: it needs two nodes, two indexers and two daemons to exercise one thing
— many concurrent ``getblock`` calls against one node's block file. It drives
exactly that, in batches, the way electrs does with ``--jsonrpc-import``, and
fails on the node's own error lines rather than on a downstream symptom.

It is a reproduction vehicle first and a regression test second. Until #394 has
a cause, a green run here proves only that this probe did not trigger it; a red
run is a reproduction with the node's error in the assertion message and, under
``BTCB2_TRACE_SYSCALLS=1``, the failing thread's syscalls and errno next to it
in ``<datadir>/strace.log``.

Bounds (env, all optional)::

    BTCB2_BLOCKFILE_BLOCKS    blocks to mine                     (default 120)
    BTCB2_BLOCKFILE_THREADS   concurrent batch senders           (default 16)
    BTCB2_BLOCKFILE_ROUNDS    passes over the chain per thread   (default 8)
    BTCB2_BLOCKFILE_BATCH     getblock calls per JSON-RPC batch  (default 100)
    BTCB2_BLOCKFILE_SECONDS   wall-clock cap on the hammer       (default 180)
"""

import base64
import json
import logging
import os
import shutil
import tempfile
import time
import urllib.error
import urllib.request

from concurrent.futures import ThreadPoolExecutor
from unittest.mock import Mock

import pytest

from fixtures import *  # noqa: F401,F403  (test_base_dir)
from test_framework import utils
from test_framework.bitcoind import Bitcoind
from test_framework.strace import describe_failure
from test_framework.btcb2 import (
    KNOTS_LEGACY_PATH,
    TwoChainRegtest,
    _child_env,
    node_block_file_errors,
    node_block_file_report,
    reread_every_block,
)

def _knob(name, default):
    """`int`/`float` env override, treating an unset-or-empty value as absent.

    A CI dispatch input that was left blank arrives as an empty string, and
    `int("")` would fail the run for the one reason that has nothing to do with
    what is being measured.
    """
    value = os.getenv(name, "").strip()
    return type(default)(value) if value else default


BLOCKS = _knob("BTCB2_BLOCKFILE_BLOCKS", 120)
THREADS = _knob("BTCB2_BLOCKFILE_THREADS", 16)
ROUNDS = _knob("BTCB2_BLOCKFILE_ROUNDS", 8)
BATCH = _knob("BTCB2_BLOCKFILE_BATCH", 100)
SECONDS = _knob("BTCB2_BLOCKFILE_SECONDS", 180.0)


def _rpc_batch(node, calls, timeout=60):
    """Send one JSON-RPC *batch*, the way electrs imports blocks.

    Batching is not incidental. electrs sends 100 `getblock`s in one request
    and reports `batch request getblock 80/100 failed`; bitcoind serves a batch
    from a single HTTP worker thread, so the shape of the load — one thread
    walking the block file while others do the same — is a property of the
    batch, not of the call count.
    """
    with open(os.path.join(node.bitcoin_dir, "regtest", ".cookie")) as f:
        authpair = f.read()
    auth = base64.b64encode(authpair.encode()).decode()
    payload = json.dumps(
        [
            {"jsonrpc": "2.0", "id": i, "method": method, "params": params}
            for i, (method, params) in enumerate(calls)
        ]
    ).encode()
    request = urllib.request.Request(
        f"http://127.0.0.1:{node.rpcport}/",
        data=payload,
        headers={
            "Authorization": f"Basic {auth}",
            "Content-Type": "application/json",
        },
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read())


@pytest.fixture
def legacy_node(request, test_base_dir):
    """One pinned non-enforcing Knots node with `BLOCKS` blocks of history.

    Deliberately not the `two_chain` fixture: no indexer, no daemon and no
    second node, so a failure here is the node's alone.
    """
    if not (KNOTS_LEGACY_PATH and os.access(KNOTS_LEGACY_PATH, os.X_OK)):
        msg = "KNOTS_LEGACY_PATH is unset or not executable"
        # Same rule as the two-chain fixture: in the labelled workflow a
        # missing binary is a broken pipeline, and a skip must not read green.
        if os.getenv("BTCB2_HARNESS_REQUIRED") == "1":
            pytest.fail(msg)
        pytest.skip(msg)

    directory = tempfile.mkdtemp(prefix="btcb2_blockfile-", dir=test_base_dir)
    home_dir = os.path.join(directory, "home-sandbox")
    os.makedirs(home_dir, exist_ok=True)
    node = Bitcoind(
        bitcoin_dir=os.path.join(directory, "knots-legacy"),
        bitcoind_path=KNOTS_LEGACY_PATH,
    )
    node.env.update(_child_env(home_dir))
    node.startup()
    try:
        node.rpc.createwallet(node.rpc.wallet_name, False, False, "", False, True, True)
        node.generate_block(BLOCKS)
        yield node
    finally:
        node.cleanup()
    if request.session.testsfailed == 0:
        shutil.rmtree(directory)
    else:
        print(f"Test failed, leaving directory '{directory}' intact")


def test_concurrent_getblock_never_loses_the_block_file(legacy_node):
    """Many concurrent batched `getblock`s must not make the node lose its
    own block file (#394)."""
    node = legacy_node
    tip = node.rpc.getblockcount()
    assert tip >= BLOCKS
    hashes = [node.rpc.getblockhash(height) for height in range(tip + 1)]
    batches = [
        [("getblock", [h, 0]) for h in hashes[start : start + BATCH]]
        for start in range(0, len(hashes), BATCH)
    ]

    deadline = time.time() + SECONDS
    rpc_errors = []
    transport_errors = []
    sent = 0

    def one_pass(_worker):
        results = []
        for calls in batches:
            if time.time() > deadline:
                break
            try:
                results.append((len(calls), _rpc_batch(node, calls)))
            except (urllib.error.URLError, OSError, ValueError) as e:
                transport_errors.append(repr(e))
        return results

    rounds_run = 0
    with ThreadPoolExecutor(max_workers=THREADS) as pool:
        for _ in range(ROUNDS):
            if time.time() > deadline:
                break
            for results in pool.map(one_pass, range(THREADS)):
                for count, responses in results:
                    sent += count
                    for response in responses:
                        if response.get("error") is not None:
                            rpc_errors.append(response["error"])
            rounds_run += 1

    logging.info(
        "blockfile probe: %s getblock calls over %s/%s rounds, %s threads, "
        "batches of %s, %s blocks",
        sent,
        rounds_run,
        ROUNDS,
        THREADS,
        BATCH,
        len(hashes),
    )
    assert rounds_run > 0, f"wall-clock cap {SECONDS}s hit before a single round"

    # A green traced run has to prove it was traced. Without this, a run that
    # passes because the trace never happened is indistinguishable from a run
    # that passes because the fault did not occur, and the second is the only
    # one worth reporting.
    if utils.TRACE_SYSCALLS:
        assert os.path.exists(node.strace_log), (
            f"BTCB2_TRACE_SYSCALLS=1 but {node.strace_log} does not exist: "
            "this run proves nothing about #394"
        )
        with open(node.strace_log, "r", errors="replace") as trace:
            block_file_calls = sum(
                1 for line in trace if "blk00000.dat" in line and "lseek(" in line
            )
        size = os.path.getsize(node.strace_log)
        logging.info(
            "blockfile probe: syscall trace %s bytes, %s lseek calls on "
            "blk00000.dat, at %s",
            size,
            block_file_calls,
            node.strace_log,
        )
        assert block_file_calls > 0, (
            f"{node.strace_log} holds no lseek on blk00000.dat, so the trace "
            "did not cover the calls #394 is about"
        )

    node_errors = node_block_file_errors(node)
    if rpc_errors or node_errors or transport_errors:
        report = node_block_file_report(
            node, "knots-legacy", "concurrent getblock probe failed"
        )
        pytest.fail(
            f"block-file fault reproduced after {sent} getblock calls "
            f"({rounds_run} rounds x {THREADS} threads, batches of {BATCH}).\n"
            f"getblock RPC errors: {rpc_errors[:5]}\n"
            f"transport errors: {transport_errors[:5]}\n"
            f"{report}\n"
            f"{_trace_excerpt(node)}"
        )


def _trace_excerpt(node):
    """The failing descriptor's history, in the failure message and on disk.

    The full trace is tens of megabytes; this is the part that answers the
    question, and it is small enough to survive artifact collection.
    """
    if not utils.TRACE_SYSCALLS or not os.path.exists(node.strace_log):
        return "(not traced: re-run with BTCB2_TRACE_SYSCALLS=1 for the errno)"
    excerpt = describe_failure(node.strace_log, "blk00000.dat")
    path = os.path.join(node.bitcoin_dir, "strace-excerpt.log")
    try:
        with open(path, "w") as handle:
            handle.write(excerpt + "\n")
    except OSError as error:  # pragma: no cover - the excerpt still goes to the log
        logging.warning("could not write %s: %s", path, error)
    return excerpt


# ── node-free regressions for the diagnosis machinery ────────────────────────
#
# These run everywhere, with no binaries: they are what keeps the evidence path
# itself honest, since the fault it exists to catch is intermittent and the
# probe above can be green for the wrong reason.


def test_syscall_trace_prefix_is_empty_unless_asked_for(monkeypatch):
    monkeypatch.setattr(utils, "TRACE_SYSCALLS", False)
    assert utils.syscall_trace_prefix("/tmp/strace.log") == []


def test_syscall_trace_prefix_follows_threads_and_appends(monkeypatch):
    monkeypatch.setattr(utils, "TRACE_SYSCALLS", True)
    monkeypatch.setattr(utils.sys, "platform", "linux")
    monkeypatch.setattr(utils.shutil, "which", lambda _: "/usr/bin/strace")
    argv = utils.syscall_trace_prefix("/somewhere/strace.log")
    assert argv[0] == "/usr/bin/strace"
    # -f is the whole point: without per-thread lines the trace cannot say
    # which thread closed the descriptor another was reading (#394).
    assert "-f" in argv
    # -A: the two-chain fork stops and restarts node A, which must not throw
    # away the trace of everything before the fork.
    assert "-A" in argv
    assert "-y" in argv
    assert argv[-2:] == ["-o", "/somewhere/strace.log"]
    traced = argv[argv.index("-e") + 1]
    for call in ("openat", "lseek", "read", "close", "dup"):
        assert call in traced


def test_syscall_trace_prefix_refuses_rather_than_running_untraced(monkeypatch):
    """Asking for a trace and silently not getting one is the worst outcome:
    the run costs the same and proves nothing."""
    monkeypatch.setattr(utils, "TRACE_SYSCALLS", True)
    monkeypatch.setattr(utils.sys, "platform", "linux")
    monkeypatch.setattr(utils.shutil, "which", lambda _: None)
    with pytest.raises(RuntimeError, match="not on PATH"):
        utils.syscall_trace_prefix("/somewhere/strace.log")

    monkeypatch.setattr(utils.sys, "platform", "darwin")
    with pytest.raises(RuntimeError, match="not Linux"):
        utils.syscall_trace_prefix("/somewhere/strace.log")


def _fake_node(tmp_path, debug_log_lines=(), unreadable=()):
    node = Mock()
    node.bitcoin_dir = str(tmp_path)
    # A real node has this attribute whether or not it is being traced; the
    # path only appears in the report when the file exists.
    node.strace_log = str(tmp_path / "strace.log")
    regtest = tmp_path / "regtest"
    regtest.mkdir(parents=True, exist_ok=True)
    (regtest / "debug.log").write_text("\n".join(debug_log_lines) + "\n")
    node.rpc.getblockcount.return_value = 2
    node.rpc.getblockhash.side_effect = lambda height: f"hash-{height}"
    node.rpc.getbestblockhash.return_value = "hash-2"

    def getblock(block_hash, _verbosity):
        if block_hash in unreadable:
            raise ValueError("Block not found on disk")
        return "00" * 81

    node.rpc.getblock.side_effect = getblock
    return node


BLOCK_FILE_FAILURE = [
    "2026-09-28T02:37:41Z [rpc] ThreadRPCServer method=getblock user=__cookie__",
    "2026-09-28T02:37:41Z [error] Unable to seek to position 47310 of /d/blocks/blk00000.dat",
    "2026-09-28T02:37:41Z [error] Unable to close file /d/blocks/blk00000.dat",
    "2026-09-28T02:37:41Z [error] ReadRawBlock: OpenBlockFile failed for "
    "FlatFilePos(nFile=0, nPos=47318)",
]


def test_block_file_errors_are_read_from_the_nodes_own_log(tmp_path):
    node = _fake_node(tmp_path, BLOCK_FILE_FAILURE)
    errors = node_block_file_errors(node)
    assert len(errors) == 3
    assert "Unable to seek to position 47310" in errors[0]
    assert "nPos=47318" in errors[2]
    # The surrounding successful getblock lines are not errors.
    assert all("ThreadRPCServer" not in line for line in errors)


def test_a_missing_log_is_reported_not_swallowed(tmp_path):
    node = Mock()
    node.bitcoin_dir = str(tmp_path / "nonexistent")
    assert "could not read" in node_block_file_errors(node)[0]


def test_reread_names_the_height_that_is_still_unreadable(tmp_path):
    node = _fake_node(tmp_path, BLOCK_FILE_FAILURE, unreadable={"hash-1"})
    assert list(reread_every_block(node)) == [1]
    assert "Block not found on disk" in reread_every_block(node)[1]


def test_a_successful_reread_is_reported_as_transient(tmp_path):
    """The distinction the whole issue turns on: data that is readable seconds
    later was never missing, so the failure was in the descriptor, not the
    file."""
    node = _fake_node(tmp_path, BLOCK_FILE_FAILURE)
    report = node_block_file_report(node, "knots-legacy")
    assert "Unable to seek to position 47310" in report
    assert "re-read of every block succeeded" in report
    assert "transient" in report


def test_a_failing_reread_is_not_called_transient(tmp_path):
    node = _fake_node(tmp_path, BLOCK_FILE_FAILURE, unreadable={"hash-1"})
    report = node_block_file_report(node, "knots-legacy")
    assert "still fails at: 1" in report
    assert "transient" not in report


def test_indexer_failure_is_reported_with_the_nodes_block_file_state(tmp_path):
    """An indexer that dies at startup must surface the node's own error and an
    exact re-read, with the original exception still attached."""
    harness = TwoChainRegtest.__new__(TwoChainRegtest)
    harness.home_dir = str(tmp_path / "home")
    os.makedirs(harness.home_dir, exist_ok=True)
    harness.legacy = _fake_node(tmp_path / "knots-legacy", BLOCK_FILE_FAILURE)
    harness.blake2b = _fake_node(tmp_path / "knots-blake2b")
    harness.legacy_dir = harness.legacy.bitcoin_dir
    harness.blake2b_dir = harness.blake2b.bitcoin_dir
    harness.electrs_legacy_dir = str(tmp_path / "electrs-legacy")
    harness.electrs_blake2b_dir = str(tmp_path / "electrs-blake2b")
    # A path that cannot be executed: `startup()` raises out of Popen, which is
    # the same route as an indexer that dies while fetching.
    harness.electrs_path = str(tmp_path / "no-such-electrs")

    with pytest.raises(RuntimeError) as failure:
        harness._start_indexers()
    assert "knots-legacy: indexer startup failed" in str(failure.value)
    assert "Unable to seek to position 47310" in str(failure.value)
    assert "re-read of every block succeeded" in str(failure.value)
    assert isinstance(failure.value.__cause__, OSError)


# ── the trace reader ─────────────────────────────────────────────────────────

TRACE = """\
1001 05:22:56.100000 openat(AT_FDCWD</w>, "/d/blocks/blk00000.dat", O_RDONLY) = 23</d/blocks/blk00000.dat> <0.000012>
1002 05:22:56.100100 openat(AT_FDCWD</w>, "/d/blocks/blk00000.dat", O_RDONLY) = 24</d/blocks/blk00000.dat> <0.000011>
1002 05:22:56.100200 close(24</d/blocks/blk00000.dat>) = 0 <0.000004>
1002 05:22:56.100300 close(23</d/blocks/blk00000.dat>) = 0 <0.000004>
1001 05:22:56.100400 lseek(23</d/blocks/blk00000.dat>, 24197, SEEK_SET) = -1 EBADF (Bad file descriptor) <0.000005>
1001 05:22:56.100500 close(23</d/blocks/blk00000.dat>) = -1 EBADF (Bad file descriptor) <0.000004>
"""


def test_the_trace_reader_names_the_thread_that_closed_the_descriptor(tmp_path):
    trace = tmp_path / "strace.log"
    trace.write_text(TRACE)
    report = describe_failure(str(trace), "blk00000.dat")
    assert "fd 23 failed in tid 1001" in report
    assert "EBADF" in report
    # The close that poisoned it came from another thread, and saying so is the
    # entire value of the report.
    assert "* 05:22:56.100300 tid    1002 close = 0" in report
    assert "other threads touched fd 23 before it failed: 1002" in report
    # fd 24's life is a different descriptor's and must not be mixed in.
    assert "100100" not in report


def test_the_trace_reader_says_so_when_nothing_failed(tmp_path):
    trace = tmp_path / "strace.log"
    trace.write_text(TRACE.replace("-1 EBADF (Bad file descriptor)", "0"))
    report = describe_failure(str(trace), "blk00000.dat")
    assert "none of them failed" in report


def test_the_trace_reader_never_raises_on_a_broken_trace(tmp_path):
    """It runs inside a failure path; throwing there would hide the failure it
    exists to explain."""
    assert "could not read" in describe_failure(str(tmp_path / "absent"), "blk")
    trace = tmp_path / "strace.log"
    trace.write_text("not a trace at all\n<... resumed>\n+++ exited with 0 +++\n")
    assert "no parsable strace lines" in describe_failure(str(trace), "blk")
