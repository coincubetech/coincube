import hashlib
import json
import logging
import os
import socket
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
        """Read the indexed tip within one budget, independent of the wallet."""
        deadline = time.monotonic() + timeout

        def remaining():
            seconds = deadline - time.monotonic()
            if seconds <= 0:
                raise socket.timeout("Electrs header request exceeded its budget")
            return seconds

        with socket.create_connection(("127.0.0.1", self.rpcport), timeout=remaining()) as sock:
            sock.settimeout(remaining())
            sock.sendall(b'{"jsonrpc":"2.0","id":0,"method":"blockchain.headers.subscribe","params":[]}\n')
            data = bytearray()
            while b"\n" not in data:
                if len(data) >= 4096:
                    raise ValueError("Electrs header response exceeds the size limit")
                # Renew the remaining budget for each fragment, not the timeout:
                # slow fragments cannot extend the overall request deadline.
                sock.settimeout(remaining())
                chunk = sock.recv(4096 - len(data))
                if not chunk:
                    raise ValueError("Electrs closed the header response before newline")
                data.extend(chunk)
            response = json.loads(data.split(b"\n", 1)[0])
        if response.get("id") != 0 or response.get("error") is not None:
            raise ValueError(f"Electrs tip request failed: {response}")
        header = bytes.fromhex(response["result"]["hex"])
        if len(header) != 80:
            raise ValueError("Electrs returned an invalid Bitcoin header")
        return hashlib.sha256(hashlib.sha256(header).digest()).digest()[::-1].hex()

    def wait_for_tip(self, expected_hash, timeout=TIMEOUT):
        # Exact hash, not just height: successive reorgs may have equal heights.
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"Electrs did not index tip {expected_hash} within {timeout}s")
            try:
                if self.tip_hash(timeout=min(5, remaining)) == expected_hash:
                    return
            except socket.timeout:
                # Indexing batches can delay RPC. Retry only while the original
                # barrier budget remains; malformed replies still fail directly.
                pass
            time.sleep(min(0.25, max(0, deadline - time.monotonic())))

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
            f.write("[electrum_config]\n")
            f.write(f"addr = '127.0.0.1:{self.rpcport}'\n")
