"""No-node regressions for the Electrs pre-invalidation synchronization."""
import hashlib
import json
import socket
import threading
from unittest.mock import Mock, patch

import pytest

from test_framework.bitcoind import Bitcoind
from test_framework.electrs import Electrs


@pytest.mark.parametrize("method", ["simple_reorg", "invalidate_remine", "invalidate_block"])
def test_reorg_waits_before_invalidating_and_propagates_failure(method):
    node = Bitcoind.__new__(Bitcoind)
    events = []
    node.rpc = Mock()
    node.rpc.getblockcount.return_value = 12
    node.rpc.getblockhash.return_value = "old-block"
    node.rpc.invalidateblock.side_effect = lambda _: events.append("invalidate")
    node.wait_for_log = Mock()
    node.generate_block = Mock()
    node.generate_empty_blocks = Mock()
    node.before_reorg = lambda: events.append("synced")
    getattr(node, method)(10)
    assert events == ["synced", "invalidate"]

    events.clear()
    node.before_reorg = Mock(side_effect=TimeoutError("indexer stalled"))
    with pytest.raises(TimeoutError, match="indexer stalled"):
        getattr(node, method)(10)
    assert events == []


def test_electrs_barrier_uses_exact_hash_not_height():
    header = bytes(range(80))
    expected = hashlib.sha256(hashlib.sha256(header).digest()).digest()[::-1].hex()
    stale = bytes(80)
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
                for value in [stale, header]:
                    with server.accept()[0] as conn:
                        conn.settimeout(5)
                        with conn.makefile("rb") as stream:
                            requests.append(json.loads(stream.readline()))
                        response = {"id": 0, "result": {"height": 114, "hex": value.hex()}}
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


def test_header_timeout_retries_within_original_deadline():
    electrs = Electrs.__new__(Electrs)
    clock = Clock()
    electrs.tip_hash = Mock(side_effect=[socket.timeout("busy indexing"), "expected"])
    with patch("test_framework.electrs.time", clock):
        electrs.wait_for_tip("expected", timeout=1)
    assert electrs.tip_hash.call_count == 2
    assert electrs.tip_hash.call_args.kwargs["timeout"] == 0.75


def test_header_timeouts_exhaust_budget_and_malformed_reply_is_not_retried():
    electrs = Electrs.__new__(Electrs)
    clock = Clock()
    electrs.tip_hash = Mock(side_effect=socket.timeout("still indexing"))
    with patch("test_framework.electrs.time", clock):
        with pytest.raises(TimeoutError, match="did not index tip"):
            electrs.wait_for_tip("expected", timeout=1)
    assert clock.now == 1
    assert electrs.tip_hash.call_count == 4
    electrs.tip_hash = Mock(side_effect=ValueError("malformed header"))
    with pytest.raises(ValueError, match="malformed header"):
        electrs.wait_for_tip("expected")
    assert electrs.tip_hash.call_count == 1


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
