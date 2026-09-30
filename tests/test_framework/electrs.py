import hashlib
import json
import logging
import os
import re
import socket
import threading
import time

from ephemeral_port_reserve import reserve
from test_framework.utils import BitcoinBackend, TailableProc, ELECTRS_PATH, TIMEOUT


class Electrs(BitcoinBackend):
    def __init__(
        self,
        bitcoind_dir,
        bitcoind_rpcport,
        bitcoind_p2pport,
        electrs_dir,
        rpcport=None,
    ):
        TailableProc.__init__(self, electrs_dir, verbose=False)

        if rpcport is None:
            rpcport = reserve()

        # Prometheus metrics can't be deactivated in Electrs. Configure the port so it doesn't
        # conflict with other instances when running tests in parallel.
        monitoring_port = reserve()

        self.electrs_dir = electrs_dir
        self.rpcport = rpcport

        regtestdir = os.path.join(electrs_dir, "regtest")
        if not os.path.exists(regtestdir):
            os.makedirs(regtestdir)

        self.cmd_line = [
            ELECTRS_PATH,
            "--conf",
            "{}/electrs.toml".format(regtestdir),
        ]
        electrs_conf = {
            "daemon_dir": bitcoind_dir,
            "cookie_file": os.path.join(bitcoind_dir, "regtest", ".cookie"),
            "daemon_rpc_addr": f"127.0.0.1:{bitcoind_rpcport}",
            "daemon_p2p_addr": f"127.0.0.1:{bitcoind_p2pport}",
            "db_dir": electrs_dir,
            "network": "regtest",
            "electrum_rpc_addr": f"127.0.0.1:{self.rpcport}",
            "monitoring_addr": f"127.0.0.1:{monitoring_port}",
        }
        self.conf_file = os.path.join(regtestdir, "electrs.toml")
        with open(self.conf_file, "w") as f:
            for k, v in electrs_conf.items():
                f.write(f'{k} = "{v}"\n')

        self.env = {"RUST_LOG": "DEBUG"}

    def start(self):
        TailableProc.start(self)
        self.wait_for_log("auto-compactions enabled", timeout=TIMEOUT)
        logging.info("Electrs started")

    def startup(self):
        try:
            self.start()
        except Exception:
            self.stop()
            raise

    def tip_hash(self, timeout=5):
        """Return Electrs' indexed tip hash within a single time budget."""
        deadline = time.monotonic() + timeout

        def remaining():
            seconds = deadline - time.monotonic()
            if seconds <= 0:
                raise socket.timeout("Electrs header request exceeded its budget")
            return seconds

        with socket.create_connection(
            ("127.0.0.1", self.rpcport), timeout=remaining()
        ) as sock:
            sock.settimeout(remaining())
            sock.sendall(
                b'{"jsonrpc":"2.0","id":0,'
                b'"method":"blockchain.headers.subscribe","params":[]}\n'
            )
            data = bytearray()
            while b"\n" not in data:
                if len(data) >= 4096:
                    raise ValueError("Electrs header response exceeds the size limit")
                sock.settimeout(remaining())
                chunk = sock.recv(4096 - len(data))
                if not chunk:
                    # A restart (see StaleBlockRequestWatchdog) closes the
                    # connection under us; callers may retry this.
                    raise ConnectionResetError(
                        "Electrs closed the header response before newline"
                    )
                data.extend(chunk)

        response = json.loads(data.split(b"\n", 1)[0])
        if response.get("id") != 0 or response.get("error") is not None:
            raise ValueError(f"Electrs tip request failed: {response}")
        header = bytes.fromhex(response["result"]["hex"])
        if len(header) != 80:
            raise ValueError("Electrs returned an invalid Bitcoin header")
        return hashlib.sha256(hashlib.sha256(header).digest()).digest()[::-1].hex()

    def wait_for_tip(self, expected_hash, timeout=TIMEOUT):
        """Wait until Electrs has indexed the exact expected branch tip."""
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(
                    f"Electrs did not index tip {expected_hash} within {timeout}s"
                )
            try:
                if self.tip_hash(timeout=min(5, remaining)) == expected_hash:
                    return
            except (socket.timeout, ConnectionError):
                # Electrs can pause RPC while committing an index batch, and is
                # briefly unreachable while the watchdog restarts it. Keep the
                # original overall deadline rather than extending it.
                pass
            time.sleep(min(0.25, max(0, deadline - time.monotonic())))

    def stop(self):
        return TailableProc.stop(self)

    def restart(self, reason):
        """Replace a stalled Electrs with a new process on the same database
        and ports. SIGTERM is not honoured while Electrs is blocked on a block
        download, so the old process is killed after a short grace period.

        Only StaleBlockRequestWatchdog calls this, from its own thread; the
        fixture stops the watchdog before it stops Electrs."""
        with self.logs_cond:
            self.logs.append(f"HARNESS: restarting electrs: {reason}")
        TailableProc.stop(self, timeout=2)
        self.start()

    def cleanup(self):
        try:
            self.stop()
        except Exception:
            self.proc.kill()
        self.proc.wait()

    def append_to_coincubed_conf(self, conf_file):
        with open(conf_file, "a") as f:
            f.write("[electrum_config]\n")
            f.write(f"addr = '127.0.0.1:{self.rpcport}'\n")


# Bitcoin Core's `-debug=net` lines that describe a peer's block request. They
# are read from the node's captured stdout, where each line is the repr of the
# raw bytes (see TailableProc.tail), so the patterns are unanchored.
_VERSION_RE = re.compile(
    r"receive version message: (?P<subver>.*?): version \d+,.* peer=(?P<peer>\d+)"
)
_GETDATA_BLOCK_RE = re.compile(
    r"received getdata for: (?:witness-)?block (?P<block>[0-9a-f]{64}) peer=(?P<peer>\d+)"
)
_REFUSED_OLD_BLOCK_RE = re.compile(
    r"ignoring request from peer=(?P<peer>\d+) for old block that isn't in the main chain"
)


class StaleBlockRequestWatchdog:
    """Restart Electrs when Core refuses a block Electrs is waiting for (#577).

    Electrs 0.10 fetches blocks over P2P to answer history requests and waits
    for each requested block without a timeout. Core does not serve a block
    that `invalidateblock` has removed from the main chain: it logs "ignoring
    request ... for old block that isn't in the main chain" and sends nothing,
    not even `notfound`. Electrs' single request thread then blocks forever,
    so it never indexes the replacement branch and never answers another RPC.

    The refusal comes from `invalidateblock` itself: after an ordinary reorg
    Core keeps serving a recently stale block whose scripts it validated
    (`BlockRequestAllowed` in net_processing.cpp), but an invalidated block
    fails that check. So this recovery is limited to exactly that case. It
    restarts Electrs only when all of these hold:

    - the harness has invalidated a block on this node (`invalidated_blocks`);
    - the refused request came from the Electrs P2P peer; and
    - Electrs then fails to answer a header request within `probe_timeout`.

    Every other stall is left to fail the test. Each restart is recorded in
    `restarts`, logged, and noted in the Electrs log; at most `max_restarts`
    are made before the watchdog gives up and lets the test fail.
    """

    def __init__(
        self, bitcoind, electrs, probe_timeout=5, max_restarts=3, poll_interval=0.2
    ):
        self.bitcoind = bitcoind
        self.electrs = electrs
        self.probe_timeout = probe_timeout
        self.max_restarts = max_restarts
        self.poll_interval = poll_interval
        self.restarts = []
        # Refusals that did not lead to a restart, with the reason why.
        self.ignored = []
        self._pos = 0
        self._subver = {}
        self._requested = {}
        self._stop = threading.Event()
        self._thread = None

    def start(self):
        self._thread = threading.Thread(
            target=self._run, name="electrs-stale-block-watchdog", daemon=True
        )
        self._thread.start()

    def stop(self):
        self._stop.set()
        if self._thread is not None:
            self._thread.join()
            self._thread = None

    def _run(self):
        while not self._stop.wait(self.poll_interval):
            try:
                self.scan()
            except Exception:
                logging.exception("Electrs stale-block watchdog failed")

    def scan(self):
        """Process the node's log lines captured since the previous scan."""
        with self.bitcoind.logs_cond:
            lines = self.bitcoind.logs[self._pos :]
        self._pos += len(lines)
        for line in lines:
            if self._stop.is_set():
                return
            m = _VERSION_RE.search(line)
            if m:
                self._subver[m["peer"]] = m["subver"]
                continue
            m = _GETDATA_BLOCK_RE.search(line)
            if m:
                self._requested[m["peer"]] = m["block"]
                continue
            m = _REFUSED_OLD_BLOCK_RE.search(line)
            if m:
                self._on_refusal(m["peer"])

    def _on_refusal(self, peer):
        event = {
            "peer": peer,
            "subver": self._subver.get(peer),
            # Core names only the first block of a getdata; Electrs sends one
            # per history lookup, so this is normally the refused block.
            "block": self._requested.get(peer),
        }
        if not (event["subver"] or "").startswith("/electrs:"):
            self._ignore(event, "the refused peer is not Electrs")
            return
        if not getattr(self.bitcoind, "invalidated_blocks", None):
            self._ignore(event, "the harness has not invalidated a block")
            return
        try:
            self.electrs.tip_hash(timeout=self.probe_timeout)
        except socket.timeout:
            pass
        except Exception as e:
            self._ignore(event, f"the stall probe failed: {e!r}")
            return
        else:
            self._ignore(event, "Electrs still answers RPC")
            return
        if len(self.restarts) >= self.max_restarts:
            self._ignore(event, f"restart budget of {self.max_restarts} exhausted")
            return
        reason = (
            f"Core refused old-branch block {event['block']} to Electrs peer={peer} "
            f"after invalidateblock and Electrs stopped answering RPC for "
            f"{self.probe_timeout}s (#577)"
        )
        logging.warning(reason)
        try:
            self.electrs.restart(reason)
        except Exception as e:
            self._ignore(event, f"the restart failed: {e!r}")
            return
        self.restarts.append(event)

    def _ignore(self, event, why):
        logging.warning(f"Not restarting Electrs after a refused block {event}: {why}")
        self.ignored.append(dict(event, why=why))
