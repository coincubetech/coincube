#!/usr/bin/env python3
"""Exercise a packaged Unix STDIO adapter without a wallet or network access."""
import concurrent.futures
import json
import os
from pathlib import Path
import secrets
import selectors
import socket
import subprocess
import sys
import tempfile
import time


def check(binary):
    with tempfile.TemporaryDirectory(prefix="mcp-smoke-", dir="/tmp") as directory:
        endpoint = str(Path(directory) / "rpc")
        credential = secrets.token_hex(32)
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(endpoint)
            os.chmod(endpoint, 0o600)
            listener.listen(1)
            listener.settimeout(10)

            def serve():
                connection, _ = listener.accept()
                with connection, connection.makefile("rb") as stream:
                    connection.settimeout(10)
                    request = json.loads(stream.readline(4097))
                    assert secrets.compare_digest(request["credential"], credential)
                    assert request["request"] == {"operation": "List", "input": {}}
                    connection.sendall(json.dumps({"Ok": {"kind": "List", "data": {
                        "wallets": [{"wallet_id": "packaging-fixture", "display_name": "Fixture", "wallet_type": "vault"}]
                    }}}).encode() + b"\n")

            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                peer = pool.submit(serve)
                process = subprocess.Popen(
                    [str(Path(binary).resolve())], stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
                    env={**os.environ, "TENSHU_MCP_SOCKET": endpoint,
                         "TENSHU_MCP_CREDENTIAL": credential},
                )
                try:
                    def send(message):
                        process.stdin.write(json.dumps(message).encode() + b"\n")
                        process.stdin.flush()

                    def receive(expected_id):
                        # Read unbuffered bytes so selector readiness cannot lose
                        # a line already hidden in a Python text buffer.
                        with selectors.DefaultSelector() as selector:
                            selector.register(process.stdout, selectors.EVENT_READ)
                            data = bytearray()
                            deadline = time.monotonic() + 10
                            while not data.endswith(b"\n"):
                                remaining = deadline - time.monotonic()
                                if remaining <= 0 or not selector.select(remaining):
                                    raise RuntimeError("adapter protocol timeout")
                                byte = process.stdout.read(1)
                                if not byte or len(data) >= 65536:
                                    raise RuntimeError("invalid adapter protocol frame")
                                data.extend(byte)
                        response = json.loads(data)
                        assert response["id"] == expected_id
                        assert "error" not in response
                        return response["result"]

                    send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                        "protocolVersion": "2024-11-05", "capabilities": {},
                        "clientInfo": {"name": "package-smoke", "version": "1"}}})
                    assert receive(1)["serverInfo"]["name"] == "tenshu-mcp"
                    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
                    send({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
                    assert {tool["name"] for tool in receive(2)["tools"]} == {
                        "tenshu_get_capabilities", "tenshu_list_wallets",
                        "tenshu_get_wallet_summary", "tenshu_get_transactions",
                        "tenshu_get_wallet_policy_summary",
                    }
                    send({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
                        "name": "tenshu_get_capabilities", "arguments": {}}})
                    capabilities = receive(3)["structuredContent"]
                    assert capabilities["schema_version"] == 1
                    assert capabilities["wallet_proposals"] is False
                    assert capabilities["wallet_creation_via_mcp"] is False
                    assert capabilities["transaction_preparation"] is False
                    send({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
                        "name": "tenshu_list_wallets", "arguments": {}}})
                    assert receive(4)["structuredContent"]["wallets"][0]["wallet_id"] == "packaging-fixture"
                    peer.result(timeout=10)
                    process.stdin.close()
                    assert process.wait(timeout=5) == 0
                    assert not process.stderr.read()
                    assert not process.stdout.read()
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=5)
    print("MCP packaged adapter smoke passed")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: check_mcp_binary.py /path/to/tenshu-mcp")
    check(sys.argv[1])
