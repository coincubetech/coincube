"""Node-free checks that background bridge assertions cannot pass silently."""
from http.client import RemoteDisconnected
from types import SimpleNamespace
from urllib.error import HTTPError
from urllib.request import Request, urlopen

import pytest

from test_btcb2_claim_gui import Bridge as ClaimBridge
from test_btcb2_split_observation import Bridge as SplitBridge


@pytest.mark.parametrize("bridge_type", [ClaimBridge, SplitBridge])
@pytest.mark.parametrize("method", ["GET", "POST"])
def test_bridge_assertion_fails_teardown(bridge_type, method):
    harness = SimpleNamespace(legacy=None, blake2b=None,
                              electrs_legacy=None, electrs_blake2b=None)
    bridge = bridge_type(harness)
    try:
        # A caller may tolerate a failed HTTP request (for example a retrying
        # GUI). That must not conceal the handler's failed test assertion.
        request = Request(bridge.url + "/invalid-route", method=method)
        try:
            urlopen(request, timeout=5).close()
        except (HTTPError, RemoteDisconnected):
            pass
    finally:
        with pytest.raises(AssertionError, match="AssertionError"):
            bridge.close()
    assert not bridge.thread.is_alive()
    assert any("AssertionError" in error for error in bridge.errors)


@pytest.mark.parametrize("bridge_type", [ClaimBridge, SplitBridge])
def test_bridge_clean_shutdown(bridge_type):
    bridge = bridge_type(SimpleNamespace())
    bridge.close()
    assert not bridge.thread.is_alive()
    assert not bridge.errors


def test_stalled_request_does_not_block_teardown():
    import socket
    import threading
    from http.server import BaseHTTPRequestHandler
    from test_framework.http_bridge import CheckedThreadingHTTPServer

    entered = threading.Event()

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            entered.set()
            self.rfile.read(10)

    errors = []
    server = CheckedThreadingHTTPServer(("127.0.0.1", 0), Handler, errors)
    server.request_timeout = 0.2
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    client = socket.create_connection(server.server_address, timeout=5)
    try:
        client.sendall(b"POST / HTTP/1.0\r\nContent-Length: 10\r\n\r\nx")
        assert entered.wait(5), "handler did not start"
        server.shutdown()
        # If joining stalls, a watchdog closes this test's own client to avoid
        # stranding the test run; the assertion below still reports regression.
        timed_out = threading.Event()

        def unblock():
            timed_out.set()
            client.shutdown(socket.SHUT_RDWR)

        watchdog = threading.Timer(3, unblock)
        watchdog.start()
        try:
            server.server_close()
        finally:
            watchdog.cancel()
            watchdog.join()
        assert not timed_out.is_set(), "teardown hung on an incomplete request"
        # BaseHTTPRequestHandler handles socket timeouts by closing the
        # connection; it does not report them as uncaught handler failures.
        server.assert_no_errors()
    finally:
        client.close()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
