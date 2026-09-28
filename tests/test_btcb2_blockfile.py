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

It is a reproduction vehicle first and a regression test second. Until the
upstream node defect tracked by #394 is fixed and validated, a green run here
proves only that this probe did not trigger it; a red run is a reproduction
with the node's error in the assertion message and, under
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
from test_framework.strace import _returned_tid, describe_failure
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
BLOCKFILE_PROBE_NODE_ARGS = [
    # Ordinary test nodes disable Knots' automatic Tor subprocess. This probe
    # opts back into the vendor failure path with a deterministically missing
    # executable so it remains useful for verifying an upstream fix (#394).
    "-listenonion=1",
    "-torexecute=coincube-btcb2-probe-missing-tor",
]


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


def _decode_getblock_batch(responses, expected_count):
    """Validate one batch and count responses that decode as raw blocks.

    JSON-RPC batch response order is not significant, so identity rather than
    position binds each response to the request. A malformed or substituted
    response must not contribute activity evidence for this probe.
    """
    if not isinstance(responses, list):
        raise ValueError("getblock batch response is not a list")
    if len(responses) != expected_count:
        raise ValueError(
            f"getblock batch returned {len(responses)}/{expected_count} responses"
        )

    by_id = {}
    for response in responses:
        if not isinstance(response, dict):
            raise ValueError("getblock batch contains a non-object response")
        response_id = response.get("id")
        if type(response_id) is not int or response_id in by_id:
            raise ValueError(
                f"getblock batch has invalid or duplicate id {response_id!r}"
            )
        by_id[response_id] = response
    expected_ids = set(range(expected_count))
    if set(by_id) != expected_ids:
        raise ValueError(
            f"getblock batch ids {sorted(by_id)} do not match "
            f"{sorted(expected_ids)}"
        )

    decoded = 0
    rpc_errors = []
    for response_id in range(expected_count):
        response = by_id[response_id]
        if response.get("error") is not None:
            rpc_errors.append(response["error"])
            continue
        result = response.get("result")
        if not isinstance(result, str):
            raise ValueError(f"getblock response {response_id} has no hex result")
        try:
            block = bytes.fromhex(result)
        except ValueError as error:
            raise ValueError(
                f"getblock response {response_id} is not valid hex"
            ) from error
        if len(block) < 80:
            raise ValueError(
                f"getblock response {response_id} decoded to only {len(block)} bytes"
            )
        decoded += 1
    return decoded, rpc_errors


def _run_getblock_probe(node, batches, deadline, threads, rounds, rpc_batch=_rpc_batch):
    """Run bounded workers and return evidence gathered by the caller thread."""
    activity = {
        "attempted": 0,
        "decoded_blocks": 0,
        "successful_batches": 0,
        "rounds_run": 0,
        "rpc_errors": [],
        "response_errors": [],
        "transport_errors": [],
    }

    def one_pass(_worker):
        outcomes = []
        for calls in batches:
            if time.time() > deadline:
                break
            try:
                responses = rpc_batch(node, calls)
                decoded, rpc_errors = _decode_getblock_batch(responses, len(calls))
                outcomes.append(("response", len(calls), decoded, rpc_errors))
            except (urllib.error.URLError, OSError) as error:
                outcomes.append(("transport", len(calls), repr(error)))
            except (TypeError, ValueError) as error:
                outcomes.append(("malformed", len(calls), repr(error)))
        return outcomes

    with ThreadPoolExecutor(max_workers=threads) as pool:
        for _ in range(rounds):
            if time.time() > deadline:
                break
            for outcomes in pool.map(one_pass, range(threads)):
                for outcome in outcomes:
                    kind, count, *details = outcome
                    activity["attempted"] += count
                    if kind == "transport":
                        activity["transport_errors"].append(details[0])
                    elif kind == "malformed":
                        activity["response_errors"].append(details[0])
                    else:
                        decoded, rpc_errors = details
                        activity["decoded_blocks"] += decoded
                        activity["rpc_errors"].extend(rpc_errors)
                        if decoded == count and not rpc_errors:
                            activity["successful_batches"] += 1
            activity["rounds_run"] += 1
    return activity


def _probe_activity_error(activity):
    """Explain why a completed loop did not exercise readable block data."""
    if activity["successful_batches"] > 0 and activity["decoded_blocks"] > 0:
        return None
    return (
        "probe completed without a successful decoded getblock batch: "
        f"{activity['attempted']} calls attempted, "
        f"{activity['decoded_blocks']} blocks decoded, "
        f"{len(activity['transport_errors'])} transport errors, "
        f"{len(activity['response_errors'])} malformed responses, and "
        f"{len(activity['rpc_errors'])} RPC errors"
    )


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
        extra_args=BLOCKFILE_PROBE_NODE_ARGS,
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


@pytest.mark.probe
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
    activity = _run_getblock_probe(node, batches, deadline, THREADS, ROUNDS)

    logging.info(
        "blockfile probe: %s getblock calls, %s decoded blocks in %s successful "
        "batches over %s/%s rounds, %s threads, batches of %s, %s blocks",
        activity["attempted"],
        activity["decoded_blocks"],
        activity["successful_batches"],
        activity["rounds_run"],
        ROUNDS,
        THREADS,
        BATCH,
        len(hashes),
    )
    assert activity["rounds_run"] > 0, (
        f"wall-clock cap {SECONDS}s hit before a single round"
    )

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

    # A reset connection is not the fault. 32 senders opening a connection per
    # call outruns what the node's HTTP layer will accept, and run 36382138529
    # failed on exactly that while the node logged no block-file error at all.
    # #394 is the node failing to read a block it has, so that is what this
    # fails on: the node's own error lines, or getblock refusing to serve.
    if activity["transport_errors"]:
        logging.warning(
            "blockfile probe: %s transport errors, not the #394 fault: %s",
            len(activity["transport_errors"]),
            activity["transport_errors"][:3],
        )
    if activity["response_errors"]:
        logging.warning(
            "blockfile probe: %s malformed/mismatched batch responses: %s",
            len(activity["response_errors"]),
            activity["response_errors"][:3],
        )
    assert node.proc.poll() is None, (
        f"the node exited with {node.proc.returncode} during the probe; "
        f"transport errors: {activity['transport_errors'][:3]}"
    )

    node_errors = node_block_file_errors(node)
    activity_error = _probe_activity_error(activity)
    if (
        activity["rpc_errors"]
        or activity["response_errors"]
        or node_errors
        or activity_error
    ):
        report = node_block_file_report(
            node, "knots-legacy", "concurrent getblock probe failed"
        )
        pytest.fail(
            f"block-file probe failed after {activity['attempted']} getblock calls "
            f"({activity['rounds_run']} rounds x {THREADS} threads, "
            f"batches of {BATCH}).\n"
            f"activity error: {activity_error}\n"
            f"getblock RPC errors: {activity['rpc_errors'][:5]}\n"
            "transport errors (context, not the fault): "
            f"{len(activity['transport_errors'])}\n"
            f"malformed response errors: {activity['response_errors'][:5]}\n"
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


def _raw_block_response(response_id, byte=0):
    return {"id": response_id, "result": f"{byte:02x}" * 80, "error": None}


def test_batch_activity_requires_matching_ids_and_decoded_blocks():
    responses = [_raw_block_response(1, 1), _raw_block_response(0, 2)]
    decoded, rpc_errors = _decode_getblock_batch(responses, 2)
    assert decoded == 2
    assert rpc_errors == []


@pytest.mark.parametrize(
    "responses, expected_count, message",
    [
        ({"id": 0, "result": "00" * 80}, 1, "not a list"),
        ([_raw_block_response(0)], 2, "returned 1/2"),
        ([_raw_block_response(0), _raw_block_response(0)], 2, "duplicate id"),
        ([_raw_block_response(0), _raw_block_response(2)], 2, "do not match"),
        (
            [{"id": 0, "result": "not-hex", "error": None}],
            1,
            "not valid hex",
        ),
        ([_raw_block_response(0) | {"result": "00"}], 1, "only 1 bytes"),
    ],
)
def test_malformed_or_mismatched_batch_is_not_activity(
    responses, expected_count, message
):
    with pytest.raises(ValueError, match=message):
        _decode_getblock_batch(responses, expected_count)


def test_all_transport_failures_cannot_make_the_probe_green():
    def unavailable(_node, _calls):
        raise urllib.error.URLError("connection refused")

    activity = _run_getblock_probe(
        Mock(),
        [[("getblock", ["hash-0", 0])]],
        time.time() + 5,
        threads=2,
        rounds=1,
        rpc_batch=unavailable,
    )
    assert activity["rounds_run"] == 1
    assert activity["attempted"] == 2
    assert activity["successful_batches"] == 0
    assert activity["decoded_blocks"] == 0
    assert len(activity["transport_errors"]) == 2
    assert "without a successful decoded getblock batch" in _probe_activity_error(
        activity
    )


def test_mismatched_responses_cannot_make_the_probe_green():
    def mismatched(_node, _calls):
        return [_raw_block_response(1)]

    activity = _run_getblock_probe(
        Mock(),
        [[("getblock", ["hash-0", 0])]],
        time.time() + 5,
        threads=1,
        rounds=1,
        rpc_batch=mismatched,
    )
    assert activity["successful_batches"] == 0
    assert activity["decoded_blocks"] == 0
    assert len(activity["response_errors"]) == 1
    assert "without a successful decoded getblock batch" in _probe_activity_error(
        activity
    )


def test_rpc_error_batch_cannot_make_the_probe_green():
    response = [{"id": 0, "result": None, "error": {"code": -1}}]
    decoded, rpc_errors = _decode_getblock_batch(response, 1)
    assert decoded == 0
    assert rpc_errors == [{"code": -1}]


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
    for call in ("openat", "lseek", "read", "close", "dup", "clone", "fork"):
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
    assert "another thread of this process touched fd 23 before it failed: 1002" in report
    # fd 24's life is a different descriptor's and must not be mixed in.
    assert "100100" not in report


def test_the_trace_reader_says_so_when_nothing_failed(tmp_path):
    trace = tmp_path / "strace.log"
    trace.write_text(TRACE.replace("-1 EBADF (Bad file descriptor)", "0"))
    report = describe_failure(str(trace), "blk00000.dat")
    assert "no lseek/read/close on blk00000.dat failed" in report


def test_the_trace_reader_never_raises_on_a_broken_trace(tmp_path):
    """It runs inside a failure path; throwing there would hide the failure it
    exists to explain."""
    assert "could not read" in describe_failure(str(tmp_path / "absent"), "blk")
    trace = tmp_path / "strace.log"
    trace.write_text("not a trace at all\n<... resumed>\n+++ exited with 0 +++\n")
    assert "no parsable strace lines" in describe_failure(str(trace), "blk")


CHILD_TRACE = """\
1001 05:22:56.100000 openat(AT_FDCWD</w>, "/d/blocks/blk00000.dat", O_RDONLY) = 23</d/blocks/blk00000.dat> <0.000012>
{storm}\
1001 05:22:56.200400 lseek(23, 24197, SEEK_SET) = -1 EBADF (Bad file descriptor) <0.000005>
"""


def test_a_forked_child_is_not_reported_as_a_sibling_thread(tmp_path):
    """The child closes its inherited copy of the table before exec. Calling
    that "another thread closed your descriptor" is exactly backwards, and it
    is what a first reading of run 36382138529 looked like."""
    storm = "".join(
        f"2002 05:22:56.1{i:05d} close({fd}) = -1 EBADF (Bad file descriptor) <0.000004>\n"
        for i, fd in enumerate(range(5000, 0, -1))
    )
    storm += "2002 05:22:56.200300 close(23</d/blocks/blk00000.dat>) = 0 <0.000004>\n"
    trace = tmp_path / "strace.log"
    creation = "2001 05:22:56.099999 fork() = 2002 <0.000100>\n"
    trace.write_text(creation + CHILD_TRACE.format(storm=storm))
    report = describe_failure(str(trace), "blk00000.dat")
    assert "fd 23 failed in tid 1001" in report
    assert "no sibling thread touched fd 23" in report
    assert "forked child clearing its inherited descriptor table" in report


def test_a_busy_shared_table_sibling_is_not_mistaken_for_a_child(tmp_path):
    """A long-lived worker can close thousands of sockets during churn. Its
    lifetime count does not make its descriptor table a forked copy."""
    trace = tmp_path / "strace.log"
    creation = (
        "1001 05:22:55.000000 clone(child_stack=NULL, "
        "flags=CLONE_VM|CLONE_FS|CLONE_FILES|SIGCHLD) = 1002 <0.000100>\n"
    )
    busy_closes = "".join(
        f"1002 05:22:55.{i:06d} close({9000 - i}) = 0 <0.000001>\n"
        for i in range(4096)
    )
    sibling_close = (
        "1002 05:22:56.100300 close(23</d/blocks/blk00000.dat>) = 0 "
        "<0.000004>\n"
    )
    # Keep the failing call path-annotated here so descriptor reconstruction is
    # independent of the classification under test. (The bare-fd case is
    # covered by the fork-child and real-trace fixtures.)
    shared_trace = CHILD_TRACE.replace(
        "lseek(23,", "lseek(23</d/blocks/blk00000.dat>,"
    )
    trace.write_text(creation + shared_trace.format(storm=busy_closes + sibling_close))
    report = describe_failure(str(trace), "blk00000.dat")
    assert "* 05:22:56.100300 tid    1002 close = 0" in report
    assert (
        "another thread of this process touched fd 23 before it failed: 1002"
        in report
    )
    assert "no sibling thread touched fd 23" not in report


def test_successful_lifo_closes_without_creation_evidence_remain_a_sibling(tmp_path):
    """A shared worker can close many real descriptors in descending order.
    Successful closes are not the failed inherited-table sweep from the legacy
    trace and must not become child-process evidence on their own."""
    trace = tmp_path / "strace.log"
    busy_closes = "".join(
        f"1002 05:22:55.{i:06d} close({9000 - i}) = 0 <0.000001>\n"
        for i in range(4096)
    )
    sibling_close = (
        "1002 05:22:56.100300 close(23</d/blocks/blk00000.dat>) = 0 "
        "<0.000004>\n"
    )
    shared_trace = CHILD_TRACE.replace(
        "lseek(23,", "lseek(23</d/blocks/blk00000.dat>,"
    )
    trace.write_text(shared_trace.format(storm=busy_closes + sibling_close))
    report = describe_failure(str(trace), "blk00000.dat")
    assert "* 05:22:56.100300 tid    1002 close = 0" in report
    assert (
        "another thread of this process touched fd 23 before it failed: 1002"
        in report
    )
    assert "no sibling thread touched fd 23" not in report


def test_process_creation_return_zero_is_not_a_child_tid():
    event = {"ret": "0"}
    assert _returned_tid(event) is None
    event["ret"] = "-1 EAGAIN (Resource temporarily unavailable)"
    assert _returned_tid(event) is None
    event["ret"] = "2002"
    assert _returned_tid(event) == "2002"


def test_a_failing_open_is_not_mistaken_for_a_descriptor_fault(tmp_path):
    """`openat(...) = -1 ENOENT` has no descriptor to trace, and reporting a
    history of "fd -1" is noise dressed as evidence."""
    trace = tmp_path / "strace.log"
    trace.write_text(
        '1001 05:22:56.100000 openat(AT_FDCWD</w>, "/d/blocks/blk00000.dat", O_RDWR)'
        " = -1 ENOENT (No such file or directory) <0.000035>\n"
    )
    report = describe_failure(str(trace), "blk00000.dat")
    assert "no lseek/read/close on blk00000.dat failed" in report
    assert "fd -1" not in report


# The sequence run 36382915647 captured, trimmed to fd 27. Every awkward part
# of it is load-bearing: the open that names the file is split across two
# lines, its result carries no path because the descriptor was already dead,
# the close that killed it completed *before* that open completed, and a forked
# child's failed close sits in the middle looking like a culprit.
REAL_TRACE = """\
8878  05:46:32.849699 close(27)         = -1 EBADF (Bad file descriptor) <0.000015>
8833  05:46:32.854550 close(27<pipe:[180543]> <unfinished ...>
8833  05:46:32.854603 <... close resumed>) = 0 <0.000040>
8820  05:46:32.854615 openat(AT_FDCWD</w>, "/d/regtest/blocks/blk00000.dat", O_RDONLY <unfinished ...>
8833  05:46:32.854646 close(27</d/regtest/blocks/blk00000.dat> <unfinished ...>
8833  05:46:32.854681 <... close resumed>) = 0 <0.000023>
8820  05:46:32.854694 <... openat resumed>) = 27 <0.000060>
8820  05:46:32.854832 lseek(27, 8192, SEEK_SET) = -1 EBADF (Bad file descriptor) <0.000029>
8820  05:46:32.855056 close(27)         = -1 EBADF (Bad file descriptor) <0.000021>
"""


def test_the_trace_reader_reconstructs_the_double_close(tmp_path):
    trace = tmp_path / "strace.log"
    # 8878 is a forked child: give it the close storm that makes it one.
    storm = "".join(
        f"8878 05:46:32.8{i:05d} close({fd}) = -1 EBADF (Bad file descriptor) <0.000004>\n"
        for i, fd in enumerate(range(9000, 3999, -1))
    )
    trace.write_text(storm + REAL_TRACE)
    report = describe_failure(str(trace), "blk00000.dat")

    # The failing call is the one the node's log complains about, found even
    # though nothing in it names the block file.
    assert "fd 27 failed in tid 8820" in report
    assert "lseek(27, 8192, SEEK_SET) = -1 EBADF" in report
    # Both of the other thread's closes are present, including the one that
    # completed before the open it destroyed.
    assert report.count("tid    8833 close = 0") == 2
    assert "another thread of this process touched fd 27 before it failed: 8833" in report
    # The child's failed close is shown but not blamed.
    assert "c 05:46:32" in report
    assert "8878" not in report.split("another thread of this process")[1]
