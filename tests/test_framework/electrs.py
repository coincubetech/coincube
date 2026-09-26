import hashlib
import json
import logging
import os
import socket

from ephemeral_port_reserve import reserve
from test_framework.utils import BitcoinBackend, TailableProc, ELECTRS_PATH, TIMEOUT, wait_for


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

    def tip_hash(self):
        """Read the indexed tip via Electrum, independent of the wallet poller."""
        with socket.create_connection(("127.0.0.1", self.rpcport), timeout=5) as sock:
            sock.sendall(b'{"jsonrpc":"2.0","id":0,"method":"blockchain.headers.subscribe","params":[]}\n')
            with sock.makefile("rb") as stream:
                response = json.loads(stream.readline(4096))
        if response.get("id") != 0 or response.get("error") is not None:
            raise ValueError(f"Electrs tip request failed: {response}")
        header = bytes.fromhex(response["result"]["hex"])
        if len(header) != 80:
            raise ValueError("Electrs returned an invalid Bitcoin header")
        return hashlib.sha256(hashlib.sha256(header).digest()).digest()[::-1].hex()

    def wait_for_tip(self, expected_hash):
        # Exact hash, not just height: successive reorgs may have equal heights.
        # No restart/retry-on-timeout: an unhealthy indexer must still fail CI.
        wait_for(lambda: self.tip_hash() == expected_hash)

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
