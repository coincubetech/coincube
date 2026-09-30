"""Esplora-API `electrs` (the mempool/Blockstream lineage) as a Coincube backend.

This is *not* the Electrum-protocol electrs in `electrs.py`: it speaks the
Esplora REST API that `coincubed`'s `esplora_config` backend consumes. The
BTCB2 harness runs the BLAKE2b-aware fork `retropex/electrs` (branch `mempool`,
commit 4453cac, "Support BLAKE2b"), which indexes both a legacy chain and a
post-fork BLAKE2b chain; the plain mempool electrs would do for legacy-only
runs. Build it with `tests/tools/fetch_electrs_blake2b.sh` and point
`ELECTRS_BLAKE2B_PATH` at the binary.
"""

import json
import logging
import os
import random
import re
import socket
import urllib.error
import urllib.request

from test_framework.utils import (
    BitcoinBackend,
    TailableProc,
    TIMEOUT,
    wait_for_while_condition_holds,
)

ELECTRS_BLAKE2B_PATH = os.getenv("ELECTRS_BLAKE2B_PATH")

# The last line of electrs' index update (`Indexer::update`, schema.rs at the
# pinned commit), logged at DEBUG (`-vvv`) once the block's transactions and
# history have been written. The header tip that `/blocks/tip/hash` serves
# moves at the *start* of the same update, before either is written (#617).
_SYNCED_TIP_RE = re.compile(r"updating synced tip to ([0-9a-f]{64})")

# Listener ports for electrs are picked *below* the kernel's ephemeral range
# (Linux 32768-60999, macOS 49152-65535) rather than with ephemeral_port_reserve.
# With `--jsonrpc-import` electrs opens hundreds of short-lived RPC connections
# to bitcoind while indexing, and only binds its REST/Electrum listeners once
# the initial index is done; a port reserved from the ephemeral range is handed
# out as a *source* port for one of those connections in the meantime, and the
# bind then fails with AddrInUse (seen on the ubuntu CI runner).
_LISTEN_PORT_RANGE = (20000, 32000)
# Ports already handed out by reserve_listen_port in this process. The probe
# socket is closed before the port is returned, and both indexers are
# constructed before either binds, so without this a later call could pick a
# number an earlier one already holds.
_RESERVED_LISTEN_PORTS = set()


def reserve_listen_port():
    """A currently-free TCP port on 127.0.0.1 below the ephemeral range, not
    handed out before in this process."""
    for _ in range(200):
        port = random.randint(*_LISTEN_PORT_RANGE)
        if port in _RESERVED_LISTEN_PORTS:
            continue
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            try:
                s.bind(("127.0.0.1", port))
            except OSError:
                continue
        _RESERVED_LISTEN_PORTS.add(port)
        return port
    raise RuntimeError("no free listener port found for electrs")


class EsploraElectrs(BitcoinBackend):
    def __init__(
        self,
        electrs_dir,
        bitcoind_dir,
        bitcoind_rpcport,
        electrs_path=None,
        http_port=None,
    ):
        TailableProc.__init__(self, electrs_dir, verbose=False)

        self.electrs_path = electrs_path or ELECTRS_BLAKE2B_PATH
        assert self.electrs_path, "ELECTRS_BLAKE2B_PATH (or electrs_path) is required"
        self.electrs_dir = electrs_dir
        self.bitcoind_dir = bitcoind_dir
        self.http_port = http_port or reserve_listen_port()
        # Electrum RPC and Prometheus can't be disabled; pin them to free ports so
        # two instances (one per chain) coexist.
        self.electrum_port = reserve_listen_port()
        self.monitoring_port = reserve_listen_port()
        self.prefix = os.path.split(electrs_dir)[-1]

        self.db_dir = os.path.join(electrs_dir, "db")
        os.makedirs(self.db_dir, exist_ok=True)

        # `--daemon-dir` locates the node's `regtest/.cookie`; blocks are pulled
        # over JSON-RPC (`--jsonrpc-import`) rather than read from the node's
        # blk*.dat files, so the indexer never touches another process's files.
        self.cmd_line = [
            self.electrs_path,
            "-vvv",
            "--network",
            "regtest",
            "--daemon-dir",
            bitcoind_dir,
            "--daemon-rpc-addr",
            f"127.0.0.1:{bitcoind_rpcport}",
            "--jsonrpc-import",
            "--db-dir",
            self.db_dir,
            "--http-addr",
            f"127.0.0.1:{self.http_port}",
            "--electrum-rpc-addr",
            f"127.0.0.1:{self.electrum_port}",
            "--monitoring-addr",
            f"127.0.0.1:{self.monitoring_port}",
        ]

    @property
    def url(self):
        return f"http://127.0.0.1:{self.http_port}"

    def rest(self, path, timeout=10):
        """GET `path` from the Esplora API; JSON is decoded, text returned raw."""
        with urllib.request.urlopen(self.url + path, timeout=timeout) as resp:
            body = resp.read()
        try:
            return json.loads(body)
        except ValueError:
            return body.decode()

    def rest_status(self, path, timeout=10):
        """HTTP status code for GET `path` (404 for an unknown txid, etc.)."""
        try:
            with urllib.request.urlopen(self.url + path, timeout=timeout) as resp:
                return resp.status
        except urllib.error.HTTPError as e:
            return e.code

    def _tip_height_or_none(self):
        """Tip height, or None while the REST server is not answering yet."""
        try:
            return self.rest("/blocks/tip/height", timeout=2)
        except (urllib.error.URLError, OSError, ValueError):
            return None

    def start(self):
        TailableProc.start(self)
        # electrs logs "REST server running" *before* it binds the socket, so
        # the log line alone is not readiness: poll the API until it answers,
        # and fail fast if the process dies (e.g. a bind panic).
        self.wait_for_log("REST server running on", timeout=TIMEOUT)
        # `condition` is re-checked on every poll: a bind panic after the log
        # line stops the wait immediately instead of running out the timeout.
        wait_for_while_condition_holds(
            lambda: self._tip_height_or_none() is not None,
            lambda: self.running,
            timeout=TIMEOUT,
            debug_fn=lambda: f"{self.prefix}: REST API not answering yet",
        )
        logging.info("Esplora electrs started on %s", self.url)

    def startup(self):
        try:
            self.start()
        except Exception as error:
            cleanup_error = None
            try:
                self.stop()
            except Exception as failure:
                cleanup_error = failure
            with self.logs_cond:
                tail = "\n".join(self.logs[-40:])[-8192:]
            diagnostic = f"{self.prefix} startup failed; last process log lines:\n{tail}"
            if cleanup_error is not None:
                diagnostic += f"\nCleanup also failed: {cleanup_error}"
            # Preserve the original exception type (including missing binaries).
            # Python 3.11+ renders notes with the traceback; older runners still
            # capture the diagnostic in pytest's error log.
            if hasattr(error, "add_note"):
                error.add_note(diagnostic)
            logging.error(diagnostic)
            raise

    def _tip_hash_or_none(self):
        try:
            return self.rest("/blocks/tip/hash", timeout=2)
        except (urllib.error.URLError, OSError):
            return None

    def synced_tip(self):
        """The tip of electrs' last completed index update, from its log, or
        None before the first one. Logs survive a restart; the restarted
        process's first update logs its own line after them."""
        with self.logs_cond:
            for line in reversed(self.logs):
                match = _SYNCED_TIP_RE.search(line)
                if match:
                    return match.group(1)
        return None

    def _confirmed_or_none(self, txid):
        try:
            return self.rest(f"/tx/{txid}/status", timeout=2).get("confirmed")
        except (urllib.error.URLError, OSError, ValueError, AttributeError):
            return None

    def _tip_state(self, confirmed_txids):
        return (
            self._tip_hash_or_none(),
            self.synced_tip(),
            {txid: self._confirmed_or_none(txid) for txid in confirmed_txids},
        )

    def wait_for_tip(self, block_hash, timeout=TIMEOUT, confirmed_txids=()):
        """Block until the indexer's tip is `block_hash` and electrs can serve its
        transactions (fails at once if it exits).

        `/blocks/tip/hash` alone is not enough: electrs moves that header tip
        before it writes the block's transactions and history, so a read right
        after it can 404 a transaction the block contains (#617). This also
        waits for the update's closing "synced tip" log line for `block_hash`,
        and for each of `confirmed_txids` to read as confirmed."""
        wait_for_while_condition_holds(
            lambda: self._tip_state(confirmed_txids)
            == (block_hash, block_hash, {txid: True for txid in confirmed_txids}),
            lambda: self.running,
            timeout=timeout,
            debug_fn=lambda: (
                f"{self.prefix} (tip, synced tip, confirmed) "
                f"{self._tip_state(confirmed_txids)} != {block_hash}"
            ),
        )

    def stop(self):
        return TailableProc.stop(self)

    def cleanup(self):
        try:
            self.stop()
        except Exception:
            self.proc.kill()
        self.proc.wait()

    def append_to_coincubed_conf(self, conf_file):
        with open(conf_file, "a") as f:
            f.write("[esplora_config]\n")
            f.write(f"addr = '{self.url}'\n")
