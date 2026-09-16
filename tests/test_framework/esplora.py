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
import urllib.request

from ephemeral_port_reserve import reserve
from test_framework.utils import BitcoinBackend, TailableProc, TIMEOUT, wait_for

ELECTRS_BLAKE2B_PATH = os.getenv("ELECTRS_BLAKE2B_PATH")


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
        self.http_port = http_port or reserve()
        # Electrum RPC and Prometheus can't be disabled; pin them to free ports so
        # two instances (one per chain) coexist.
        self.electrum_port = reserve()
        self.monitoring_port = reserve()
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

    def start(self):
        TailableProc.start(self)
        self.wait_for_log("REST server running on", timeout=TIMEOUT)
        logging.info("Esplora electrs started on %s", self.url)

    def startup(self):
        try:
            self.start()
        except Exception:
            self.stop()
            raise

    def wait_for_tip(self, block_hash, timeout=TIMEOUT):
        """Block until the indexer's tip is `block_hash`."""
        wait_for(
            lambda: self.rest("/blocks/tip/hash") == block_hash,
            timeout=timeout,
            debug_fn=lambda: f"electrs tip {self.rest('/blocks/tip/hash')} != {block_hash}",
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
