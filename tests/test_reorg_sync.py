"""No-node regressions for Electrs synchronization before invalidation."""

import hashlib
import json
import socket
import threading
from unittest.mock import Mock, patch

import pytest

from test_framework.bitcoind import Bitcoind
from test_framework.electrs import Electrs, StaleBlockRequestWatchdog


@pytest.mark.parametrize("method", ["simple_reorg", "invalidate_remine"])
def test_reorg_waits_for_indexer_before_invalidating(method):
    node = Bitcoind.__new__(Bitcoind)
    events = []
    node.rpc = Mock()
    node.rpc.getblockcount.return_value = 12
    node.rpc.getblockhash.return_value = "old-block"
    node.wait_for_log = Mock()
    node.generate_block = Mock()
    node.generate_empty_blocks = Mock()
    node.before_reorg = lambda: events.append("indexed-old-tip")
    node.invalidated_blocks = []
    node.rpc.invalidateblock.side_effect = lambda h: events.append(
        ("invalidate", list(node.invalidated_blocks))
    )

    getattr(node, method)(10)

    # The invalidation is recorded before Core starts it: Core can refuse the
    # block to Electrs while the RPC is still running (#577).
    assert events == ["indexed-old-tip", ("invalidate", ["old-block"])]


def test_failed_indexer_barrier_preserves_old_chain():
    node = Bitcoind.__new__(Bitcoind)
    node.rpc = Mock()
    node.before_reorg = Mock(side_effect=TimeoutError("indexer stalled"))

    with pytest.raises(TimeoutError, match="indexer stalled"):
        node.invalidate_block("old-block")

    node.rpc.invalidateblock.assert_not_called()


def test_electrs_barrier_uses_exact_hash_not_height():
    header = bytes(range(80))
    expected = hashlib.sha256(hashlib.sha256(header).digest()).digest()[::-1].hex()
    stale_same_height = bytes(80)
    requests = []
    failures = []

    with socket.socket() as server:
        server.bind(("127.0.0.1", 0))
        server.listen()
        server.settimeout(5)
        electrs = Electrs.__new__(Electrs)
        electrs.rpcport = server.getsockname()[1]

        def serve():
            try:
                for value in [stale_same_height, header]:
                    with server.accept()[0] as conn:
                        conn.settimeout(5)
                        with conn.makefile("rb") as stream:
                            requests.append(json.loads(stream.readline()))
                        response = {
                            "id": 0,
                            "result": {"height": 114, "hex": value.hex()},
                        }
                        conn.sendall(json.dumps(response).encode() + b"\n")
            except Exception as error:
                failures.append(error)

        worker = threading.Thread(target=serve)
        worker.start()
        try:
            electrs.wait_for_tip(expected)
        finally:
            worker.join(timeout=6)

    assert not worker.is_alive()
    assert not failures
    assert len(requests) == 2
    assert all(r["method"] == "blockchain.headers.subscribe" for r in requests)


class Clock:
    def __init__(self):
        self.now = 0.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


def test_header_timeout_retries_then_succeeds_with_remaining_budget():
    electrs = Electrs.__new__(Electrs)
    clock = Clock()
    electrs.tip_hash = Mock(
        side_effect=[socket.timeout("busy indexing"), "expected"]
    )

    with patch("test_framework.electrs.time", clock):
        electrs.wait_for_tip("expected", timeout=1)

    assert electrs.tip_hash.call_count == 2
    assert electrs.tip_hash.call_args.kwargs["timeout"] == 0.75


def test_header_timeout_retries_only_within_original_deadline():
    electrs = Electrs.__new__(Electrs)
    clock = Clock()
    electrs.tip_hash = Mock(side_effect=socket.timeout("still indexing"))

    with patch("test_framework.electrs.time", clock):
        with pytest.raises(TimeoutError, match="did not index tip"):
            electrs.wait_for_tip("expected", timeout=1)

    assert clock.now == 1
    assert electrs.tip_hash.call_count == 4


def test_malformed_header_reply_is_not_retried():
    electrs = Electrs.__new__(Electrs)
    electrs.tip_hash = Mock(side_effect=ValueError("malformed header"))

    with pytest.raises(ValueError, match="malformed header"):
        electrs.wait_for_tip("expected")

    electrs.tip_hash.assert_called_once()


def test_header_fragments_cannot_extend_request_budget():
    electrs = Electrs.__new__(Electrs)
    electrs.rpcport = 1
    clock = Clock()
    sock = Mock()

    def fragment(_size):
        clock.sleep(0.4)
        return b"x"

    sock.recv.side_effect = fragment
    connection = Mock()
    connection.__enter__ = Mock(return_value=sock)
    connection.__exit__ = Mock(return_value=False)

    with patch("test_framework.electrs.time", clock), patch(
        "test_framework.electrs.socket.create_connection", return_value=connection
    ):
        with pytest.raises(socket.timeout, match="exceeded its budget"):
            electrs.tip_hash(timeout=1)

    assert sock.recv.call_count == 3
    assert sock.settimeout.call_args.args[0] < 0.3


# Captured Core stdout lines from the failure in job 109661976225
# (test_reorg_exclusion), stored as TailableProc stores them.
ELECTRS_VERSION = (
    "b'2026-09-29T23:15:21Z [net] receive version message: /electrs:0.10.6/: "
    "version 70001, blocks=0, us=0.0.0.0:0, txrelay=0, peer=0'"
)
STALE_BLOCK = "70184ec7669912f769b8223df8f2abf67b5cd5175b12b1f308d85205de86e3ce"
GETDATA = (
    "b'2026-09-29T23:15:28Z [net] received getdata for: witness-block "
    f"{STALE_BLOCK} peer=0'"
)
REFUSED = (
    'b"2026-09-29T23:15:28Z [net] ProcessGetBlockData: ignoring request from '
    "peer=0 for old block that isn't in the main chain\""
)


def watchdog_for(lines, invalidated=("old-block",), probe=socket.timeout("stuck")):
    node = Mock()
    node.logs = list(lines)
    node.logs_cond = threading.RLock()
    node.invalidated_blocks = list(invalidated)
    electrs = Mock()
    electrs.tip_hash.side_effect = probe
    return StaleBlockRequestWatchdog(node, electrs, probe_timeout=3), node, electrs


def test_refused_electrs_block_after_invalidation_restarts_a_stalled_electrs():
    watchdog, _, electrs = watchdog_for([ELECTRS_VERSION, GETDATA, REFUSED])

    watchdog.scan()

    electrs.tip_hash.assert_called_once_with(timeout=3)
    electrs.restart.assert_called_once()
    assert STALE_BLOCK in electrs.restart.call_args.args[0]
    assert watchdog.restarts == [
        {"peer": "0", "subver": "/electrs:0.10.6/", "block": STALE_BLOCK}
    ]
    assert watchdog.ignored == []


def test_lines_are_processed_once_across_scans():
    watchdog, node, electrs = watchdog_for([ELECTRS_VERSION, GETDATA])

    watchdog.scan()
    electrs.restart.assert_not_called()
    node.logs.append(REFUSED)
    watchdog.scan()
    watchdog.scan()

    electrs.restart.assert_called_once()


def test_refusal_without_a_harness_invalidation_is_not_recovered():
    watchdog, _, electrs = watchdog_for(
        [ELECTRS_VERSION, GETDATA, REFUSED], invalidated=()
    )

    watchdog.scan()

    electrs.tip_hash.assert_not_called()
    electrs.restart.assert_not_called()
    assert watchdog.ignored[0]["why"] == "the harness has not invalidated a block"


def test_refusal_to_another_peer_is_not_recovered():
    other_peer = [
        line.replace("peer=0", "peer=1") for line in [GETDATA, REFUSED]
    ]
    watchdog, _, electrs = watchdog_for([ELECTRS_VERSION] + other_peer)

    watchdog.scan()

    electrs.restart.assert_not_called()
    assert watchdog.ignored[0]["why"] == "the refused peer is not Electrs"


def test_electrs_that_still_answers_is_not_restarted():
    watchdog, _, electrs = watchdog_for(
        [ELECTRS_VERSION, GETDATA, REFUSED], probe=None
    )
    electrs.tip_hash.side_effect = None
    electrs.tip_hash.return_value = "some-tip"

    watchdog.scan()

    electrs.restart.assert_not_called()
    assert watchdog.ignored[0]["why"] == "Electrs still answers RPC"


def test_restarts_stop_at_the_budget():
    watchdog, _, electrs = watchdog_for(
        [ELECTRS_VERSION, GETDATA] + [REFUSED] * 5
    )

    watchdog.scan()

    assert electrs.restart.call_count == 3
    assert len(watchdog.restarts) == 3
    assert [e["why"] for e in watchdog.ignored] == [
        "restart budget of 3 exhausted"
    ] * 2


def test_barrier_retries_while_electrs_restarts():
    electrs = Electrs.__new__(Electrs)
    clock = Clock()
    electrs.tip_hash = Mock(
        side_effect=[
            ConnectionResetError("killed mid-request"),
            ConnectionRefusedError("not listening yet"),
            "expected",
        ]
    )

    with patch("test_framework.electrs.time", clock):
        electrs.wait_for_tip("expected", timeout=1)

    assert electrs.tip_hash.call_count == 3


def test_closed_header_connection_is_a_connection_error():
    electrs = Electrs.__new__(Electrs)
    electrs.rpcport = 1
    sock = Mock()
    sock.recv.return_value = b""
    connection = Mock()
    connection.__enter__ = Mock(return_value=sock)
    connection.__exit__ = Mock(return_value=False)

    with patch(
        "test_framework.electrs.socket.create_connection", return_value=connection
    ):
        with pytest.raises(ConnectionResetError, match="before newline"):
            electrs.tip_hash(timeout=1)


def test_a_failed_restart_is_recorded_not_counted():
    watchdog, _, electrs = watchdog_for([ELECTRS_VERSION, GETDATA, REFUSED])
    electrs.restart.side_effect = TimeoutError("electrs did not start")

    watchdog.scan()

    assert watchdog.restarts == []
    assert "the restart failed" in watchdog.ignored[0]["why"]
