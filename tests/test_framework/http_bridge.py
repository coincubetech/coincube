"""HTTP test servers must propagate worker failures to the test thread."""
import sys
from http.server import ThreadingHTTPServer


class CheckedThreadingHTTPServer(ThreadingHTTPServer):
    # server_close joins handlers before teardown checks their errors. A daemon
    # handler could otherwise append its failure after the test already passed.
    daemon_threads = False

    def __init__(self, address, handler, errors):
        self.errors = errors
        super().__init__(address, handler)

    def handle_error(self, request, client_address):
        # socketserver calls this while the handler exception is active.
        self.errors.append(repr(sys.exc_info()[1]))

    def assert_no_errors(self):
        assert not self.errors, self.errors
