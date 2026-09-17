#!/usr/bin/env python3
"""Two-chain consensus probe for coincube#398.

Adapts the Knots 29.4.1 pairing documented in
WORK_LOGS/BTCB2_ACTIVATION/SIGNER_NODE_QA.py (SHA instance, no extra
activation argument; BLAKE2b instance with -testactivationheight=blake2b@102;
101 shared synthetic blocks). This script does not rerun that file and does
not pin any other worktree.

The tested repository is the checkout that contains this file, unless
--repo / BTCB2_ALT_LEGACY_REPO overrides it. Binary, temporary and output
paths are required to be explicit (arguments or environment). There is no
host-specific default checkout.

Application-path evidence lives in coincube-core unified_finalize tests, not
here. This probe only asks each node whether the synthetic witnesses are
consensus-valid against an unspent fixture.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import tempfile
import time
from pathlib import Path

SCRIPT_PATH = Path(__file__).resolve()
DEFAULT_REPO = SCRIPT_PATH.parents[1]


def parse_args(argv=None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo",
        default=os.environ.get("BTCB2_ALT_LEGACY_REPO", str(DEFAULT_REPO)),
        help="Checkout to cargo-run and git-describe (default: directory containing contrib/)",
    )
    parser.add_argument(
        "--bitcoind",
        default=os.environ.get("BITCOIND_PATH"),
        help="Knots bitcoind binary (env BITCOIND_PATH)",
    )
    parser.add_argument(
        "--bitcoin-cli",
        default=os.environ.get("BITCOIN_CLI_PATH"),
        help="bitcoin-cli binary (env BITCOIN_CLI_PATH; default: sibling of --bitcoind)",
    )
    parser.add_argument(
        "--datadir-parent",
        default=os.environ.get("BTCB2_ALT_LEGACY_DATADIR_PARENT"),
        help="Parent directory for disposable node datadirs (env BTCB2_ALT_LEGACY_DATADIR_PARENT; default: tempfile)",
    )
    parser.add_argument(
        "--out",
        default=os.environ.get("BTCB2_ALT_LEGACY_OUT"),
        help="Write the full JSON result here (env BTCB2_ALT_LEGACY_OUT; default: stdout only)",
    )
    parser.add_argument(
        "--print-config",
        action="store_true",
        help="Print resolved paths and git provenance, then exit",
    )
    return parser.parse_args(argv)


def resolve_config(args: argparse.Namespace) -> dict:
    repo = Path(args.repo).resolve()
    if not (repo / "Cargo.toml").is_file() or not (repo / "coincube-core").is_dir():
        raise SystemExit(f"not a coincube checkout: {repo}")
    if not args.bitcoind:
        raise SystemExit("bitcoind path required (--bitcoind or BITCOIND_PATH)")
    bitcoind = Path(args.bitcoind).resolve()
    if not bitcoind.is_file():
        raise SystemExit(f"bitcoind not found: {bitcoind}")
    bitcoin_cli = Path(args.bitcoin_cli).resolve() if args.bitcoin_cli else bitcoind.parent / "bitcoin-cli"
    if not bitcoin_cli.is_file():
        raise SystemExit(f"bitcoin-cli not found: {bitcoin_cli}")
    if args.datadir_parent:
        datadir_parent = Path(args.datadir_parent).resolve()
        datadir_parent.mkdir(parents=True, exist_ok=True)
    elif args.print_config:
        datadir_parent = None
    else:
        datadir_parent = Path(tempfile.mkdtemp(prefix="btcb2-alt-legacy-"))
        datadir_parent.chmod(0o700)
    out = Path(args.out).resolve() if args.out else None
    return {
        "repo": repo,
        "bitcoind": bitcoind,
        "bitcoin_cli": bitcoin_cli,
        "datadir_parent": datadir_parent,
        "out": out,
    }


def git_provenance(repo: Path) -> dict:
    def git(*git_args: str) -> str:
        return subprocess.check_output(["git", "-C", str(repo), *git_args], text=True).strip()

    porcelain = git("status", "--porcelain")
    return {
        "repo": str(repo),
        "head": git("rev-parse", "HEAD"),
        "branch": git("rev-parse", "--abbrev-ref", "HEAD"),
        "dirty": bool(porcelain),
        "status_porcelain": porcelain,
    }


def command_version(binary: Path) -> str:
    result = subprocess.run(
        [str(binary), "--version"],
        capture_output=True,
        text=True,
        timeout=10,
    )
    text = (result.stdout or result.stderr).strip()
    return text.splitlines()[0] if text else ""


class Node:
    def __init__(self, name: str, extra: list[str], parent: Path, bitcoind: Path, bitcoin_cli: Path):
        self.name = name
        self.bitcoind = bitcoind
        self.bitcoin_cli = bitcoin_cli
        self.data = Path(tempfile.mkdtemp(prefix=f"alt-legacy-{name}-", dir=parent))
        self.data.chmod(0o700)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]
        self.args = [f"-datadir={self.data}", "-regtest", f"-rpcport={self.port}"]
        self.extra = extra
        self.process = None
        self.log = None
        self.argv = []

    def rpc(self, method: str, *params):
        cmd = [
            str(self.bitcoin_cli),
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
        self.argv = [
            str(self.bitcoind),
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
        ]
        self.process = subprocess.Popen(
            self.argv,
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


def cargo_example(repo: Path, command: str, stdin: str | None = None) -> dict:
    argv = [
        "cargo",
        "run",
        "--quiet",
        "-p",
        "coincube-core",
        "--example",
        "alt_legacy_witness",
        "--",
        command,
    ]
    output = subprocess.check_output(argv, cwd=repo, input=stdin, text=True)
    return json.loads(output)


def main(argv=None) -> None:
    args = parse_args(argv)
    config = resolve_config(args)
    provenance = git_provenance(config["repo"])
    if args.print_config:
        print(
            json.dumps(
                {
                    "script": str(SCRIPT_PATH),
                    "default_repo_from_script": str(DEFAULT_REPO),
                    "repo": str(config["repo"]),
                    "bitcoind": str(config["bitcoind"]),
                    "bitcoin_cli": str(config["bitcoin_cli"]),
                    "datadir_parent": str(config["datadir_parent"])
                    if config["datadir_parent"]
                    else None,
                    "out": str(config["out"]) if config["out"] else None,
                    "cwd": os.getcwd(),
                    "provenance": provenance,
                },
                indent=2,
            )
        )
        return

    template = cargo_example(config["repo"], "template")
    sha = Node("sha", [], config["datadir_parent"], config["bitcoind"], config["bitcoin_cli"])
    blake = Node(
        "blake",
        ["-testactivationheight=blake2b@102"],
        config["datadir_parent"],
        config["bitcoind"],
        config["bitcoin_cli"],
    )
    result = {"passed": False, "provenance": provenance}
    try:
        sha.start()
        blake.start()
        common = sha.rpc("generatetoaddress", 101, template["address"])
        for height in common:
            assert blake.rpc("submitblock", sha.rpc("getblock", height, 0)) is None
        assert sha.rpc("getbestblockhash") == blake.rpc("getbestblockhash")
        funding = sha.rpc("getblock", common[0], 2)["tx"][0]["hex"]
        signed = cargo_example(config["repo"], "sign", stdin=funding)
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
        decoded = {
            name: sha.rpc("decoderawtransaction", raw) for name, raw in cases.items()
        }
        result = {
            "passed": False,
            "issue": 398,
            "provenance": provenance,
            "nodes": {
                "bitcoind_path": str(config["bitcoind"]),
                "bitcoin_cli_path": str(config["bitcoin_cli"]),
                "bitcoind_version": command_version(config["bitcoind"]),
                "bitcoin_cli_version": command_version(config["bitcoin_cli"]),
                "subversion": blake.rpc("getnetworkinfo")["subversion"],
                "sha_extra_args": sha.extra,
                "blake_extra_args": blake.extra,
                "sha_argv": sha.argv,
                "blake_argv": blake.argv,
                "networkactive": False,
                "common_height": 101,
                "fork_height": 102,
                "activation": "-testactivationheight=blake2b@102",
            },
            "template": template,
            "funding_hex": funding,
            "transactions": cases,
            "txid": signed["txid"],
            "unified_wtxid": signed["unified_wtxid"],
            "alternate_wtxid": signed["alternate_wtxid"],
            "decoded": {
                name: {
                    "txid": item.get("txid"),
                    "hash": item.get("hash"),
                    "size": item.get("size"),
                    "vsize": item.get("vsize"),
                    "weight": item.get("weight"),
                }
                for name, item in decoded.items()
            },
            "verdicts": verdicts,
            "datadirs": [str(sha.data), str(blake.data)],
            "limitations": (
                "Consensus probe of a simple 2-of-3 P2WSH. Does not drive the "
                "GUI, daemon updatespend, Keychain, hardware, or poison split."
            ),
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
        result["passed"] = True
    finally:
        for node in (sha, blake):
            node.stop()
        result["all_temporary_processes_stopped"] = all(
            n.process is None or n.process.poll() is not None for n in (sha, blake)
        )
        payload = json.dumps(result, indent=2) + "\n"
        print(payload, end="", flush=True)
        if config["out"]:
            config["out"].parent.mkdir(parents=True, exist_ok=True)
            config["out"].write_text(payload)


if __name__ == "__main__":
    main()
