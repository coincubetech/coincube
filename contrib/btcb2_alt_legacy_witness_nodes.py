#!/usr/bin/env python3
"""Two-chain consensus probe for coincube#398.

Adapts the Knots 29.4.1 pairing documented in
WORK_LOGS/BTCB2_ACTIVATION/SIGNER_NODE_QA.py (SHA instance, no extra
activation argument; BLAKE2b instance with -testactivationheight=blake2b@102;
101 shared synthetic blocks). This script does not rerun that file, does not
pin its old signer worktree, and writes a new results path.

Application-path evidence lives in coincube-core unified_finalize tests, not
here. This probe only asks each node whether the synthetic witnesses are
consensus-valid against an unspent fixture.
"""

from __future__ import annotations

import json
import os
import socket
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path("/Users/macstudio/git/coincubetech")
QA = ROOT / ".scratch/bitcoin-blake2b/regtest-activation-qa"
WORKTREE = ROOT / ".scratch/bitcoin-blake2b/alt-legacy-witness"
OUT = ROOT / "WORK_LOGS/BTCB2_ACTIVATION/ALT_LEGACY_WITNESS_398_RESULTS.json"
BITCOIND = Path(os.environ.get("BITCOIND_PATH", QA / "bitcoind"))
BITCOIN_CLI = Path(os.environ.get("BITCOIN_CLI_PATH", QA / "bitcoin-cli"))


class Node:
    def __init__(self, name: str, extra: list[str], parent: Path):
        self.data = Path(tempfile.mkdtemp(prefix=f"alt-legacy-{name}-", dir=parent))
        self.data.chmod(0o700)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]
        self.args = [f"-datadir={self.data}", "-regtest", f"-rpcport={self.port}"]
        self.extra = extra
        self.process = None
        self.log = None

    def rpc(self, method: str, *params):
        cmd = [
            str(BITCOIN_CLI),
            *self.args,
            "-rpcconnect=127.0.0.1",
            method,
            *map(str, params),
        ]
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=20)
        if result.returncode:
            raise RuntimeError(result.stderr.strip())
        if not result.stdout.strip():
            return None
        try:
            return json.loads(result.stdout)
        except json.JSONDecodeError:
            return result.stdout.strip()

    def start(self):
        self.log = (self.data / "process.log").open("a")
        self.process = subprocess.Popen(
            [
                str(BITCOIND),
                *self.args,
                "-server=1",
                "-daemon=0",
                "-networkactive=0",
                "-connect=0",
                "-listen=0",
                "-dnsseed=0",
                "-discover=0",
                "-listenonion=0",
                "-natpmp=0",
                "-upnp=0",
                "-rpcbind=127.0.0.1",
                "-rpcallowip=127.0.0.1",
                "-disablewallet=1",
                "-printtoconsole=0",
                *self.extra,
            ],
            stdout=self.log,
            stderr=subprocess.STDOUT,
        )
        for _ in range(80):
            if self.process.poll() is not None:
                raise RuntimeError((self.data / "process.log").read_text())
            try:
                info = self.rpc("getblockchaininfo")
                assert info["chain"] == "regtest"
                assert not self.rpc("getnetworkinfo")["networkactive"]
                return
            except RuntimeError:
                time.sleep(0.25)
        raise RuntimeError("startup deadline")

    def stop(self):
        if self.process is not None and self.process.poll() is None:
            try:
                self.rpc("stop")
            except Exception:
                self.process.terminate()
            try:
                self.process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        if self.log:
            self.log.close()
            self.log = None


def accept(node: Node, raw_hex: str):
    return node.rpc("testmempoolaccept", json.dumps([raw_hex]))[0]


def main():
    assert BITCOIND.is_file(), BITCOIND
    assert BITCOIN_CLI.is_file(), BITCOIN_CLI
    parent = Path(tempfile.mkdtemp(prefix="alt-legacy-398-", dir=QA))
    parent.chmod(0o700)
    template = json.loads(
        subprocess.check_output(
            [
                "cargo",
                "run",
                "--quiet",
                "-p",
                "coincube-core",
                "--example",
                "alt_legacy_witness",
                "--",
                "template",
            ],
            cwd=WORKTREE,
            text=True,
        )
    )
    sha = Node("sha", [], parent)
    blake = Node("blake", ["-testactivationheight=blake2b@102"], parent)
    result = {"passed": False}
    try:
        sha.start()
        blake.start()
        common = sha.rpc("generatetoaddress", 101, template["address"])
        for height in common:
            assert blake.rpc("submitblock", sha.rpc("getblock", height, 0)) is None
        assert sha.rpc("getbestblockhash") == blake.rpc("getbestblockhash")
        funding = sha.rpc("getblock", common[0], 2)["tx"][0]["hex"]
        signed = json.loads(
            subprocess.check_output(
                [
                    "cargo",
                    "run",
                    "--quiet",
                    "-p",
                    "coincube-core",
                    "--example",
                    "alt_legacy_witness",
                    "--",
                    "sign",
                ],
                cwd=WORKTREE,
                input=funding,
                text=True,
            )
        )
        sha.rpc("generatetoaddress", 1, template["address"])
        blake.rpc("generatetoaddress", 1, template["address"])
        cases = {
            "unified": signed["unified_hex"],
            "mixed": signed["mixed_hex"],
            "alternate_legacy": signed["alternate_legacy_hex"],
            "same_keys_legacy": signed["same_keys_legacy_hex"],
            "insufficient_legacy": signed["insufficient_legacy_hex"],
        }
        verdicts = {
            name: {"sha": accept(sha, raw), "blake": accept(blake, raw)}
            for name, raw in cases.items()
        }
        assert verdicts["unified"]["blake"].get("allowed") is True, verdicts["unified"]
        assert verdicts["unified"]["sha"].get("allowed") is False, verdicts["unified"]
        assert verdicts["mixed"]["blake"].get("allowed") is True, verdicts["mixed"]
        assert verdicts["mixed"]["sha"].get("allowed") is False, verdicts["mixed"]
        assert verdicts["alternate_legacy"]["sha"].get("allowed") is True, verdicts[
            "alternate_legacy"
        ]
        assert verdicts["alternate_legacy"]["blake"].get("allowed") is True, verdicts[
            "alternate_legacy"
        ]
        assert verdicts["same_keys_legacy"]["sha"].get("allowed") is True, verdicts[
            "same_keys_legacy"
        ]
        assert verdicts["insufficient_legacy"]["sha"].get("allowed") is False, verdicts[
            "insufficient_legacy"
        ]
        assert verdicts["insufficient_legacy"]["blake"].get("allowed") is False, verdicts[
            "insufficient_legacy"
        ]
        result = {
            "passed": True,
            "issue": 398,
            "core_head": subprocess.check_output(
                ["git", "-C", str(WORKTREE), "rev-parse", "HEAD"], text=True
            ).strip(),
            "binary": blake.rpc("getnetworkinfo")["subversion"],
            "common_height": 101,
            "fork_height": 102,
            "txid": signed["txid"],
            "unified_wtxid": signed["unified_wtxid"],
            "alternate_wtxid": signed["alternate_wtxid"],
            "verdicts": {
                name: {
                    chain: {
                        "allowed": item.get("allowed"),
                        "reject-reason": item.get("reject-reason"),
                    }
                    for chain, item in pair.items()
                }
                for name, pair in verdicts.items()
            },
            "datadirs": [str(sha.data), str(blake.data)],
            "networkactive": False,
            "limitations": (
                "Consensus probe of a simple 2-of-3 P2WSH. Does not drive the "
                "GUI, daemon updatespend, Keychain, hardware, or poison split."
            ),
        }
        print(json.dumps(result, indent=2), flush=True)
    finally:
        for node in (sha, blake):
            node.stop()
        result["all_temporary_processes_stopped"] = all(
            n.process is None or n.process.poll() is not None for n in (sha, blake)
        )
        OUT.parent.mkdir(parents=True, exist_ok=True)
        OUT.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
