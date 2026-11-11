"""Node-free checks that background bridge assertions cannot pass silently."""
# The Claim and foreign-Split Esplora bridges this file also exercised on
# `master` (`test_btcb2_claim_gui`, `test_btcb2_split_observation`) were removed
# on `redesign` with the Claim wizard and foreign Split (#958). What remains is
# the shared checked server they were built on.


def test_stalled_request_does_not_block_teardown():
    """An incomplete client request cannot prevent joined-handler teardown."""
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
