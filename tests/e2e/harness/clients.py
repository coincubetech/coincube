import base64
from decimal import Decimal
import json
from pathlib import Path
import subprocess
from urllib.error import HTTPError, URLError
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

from .state import Blocked, Failed, private_read

GENESIS = "00000000da84f2bafbbc53dee25a72ae507ff4914b867c565be350b0da8bf043"
REST = "https://api.coincube.io"
GRPC = "https://grpc.coincube.io:443"
EXPLORERS = ("https://mempool.space/testnet4/api", "https://blockstream.info/testnet4/api")


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def http(url, *, body=None, headers=None, timeout=15, missing_ok=False):
    opener = build_opener(ProxyHandler({}), NoRedirect())
    try:
        with opener.open(Request(url, data=body, headers=headers or {}), timeout=timeout) as response:
            data = response.read(4_000_001)
        if len(data) > 4_000_000:
            raise Failed("RESPONSE_SIZE", "Service response exceeds the size limit.")
        return data
    except HTTPError as error:
        if missing_ok and error.code == 404:
            return None
        # Bitcoin Core returns JSON RPC errors with HTTP 500.
        if body is not None and error.code == 500:
            return error.read(4_000_000)
        raise Blocked("HTTP_STATUS", "A required service rejected the request.") from None
    except (URLError, OSError, TimeoutError):
        raise Blocked("SERVICE_UNREACHABLE", "A required service is unreachable.") from None


class Core:
    def __init__(self, config):
        self.url = config["url"]
        self.cookie = config["cookie_file"]

    def call(self, method, *params, missing_ok=False):
        cookie = private_read(self.cookie).decode().strip()
        auth = base64.b64encode(cookie.encode()).decode()
        raw = http(self.url, body=json.dumps({"jsonrpc": "2.0", "id": "qa", "method": method,
                                             "params": params}).encode(),
                   headers={"Authorization": "Basic " + auth, "Content-Type": "application/json"})
        reply = json.loads(raw, parse_float=Decimal)
        error = reply.get("error")
        if error:
            if missing_ok and error.get("code") == -5:
                return None
            raise Blocked("CORE_RPC", "Bitcoin Core could not complete the required RPC.")
        return reply["result"]

    def chain(self):
        chain = self.call("getblockchaininfo")
        if chain["chain"] != "testnet4" or self.call("getblockhash", 0) != GENESIS:
            raise Failed("WRONG_CHAIN", "Bitcoin Core must be on Bitcoin Testnet4.")
        if chain.get("initialblockdownload", True):
            raise Blocked("NODE_SYNC", "Bitcoin Core is still synchronizing Testnet4.")

    def known(self, txid):
        tx = self.call("getrawtransaction", txid, True, missing_ok=True)
        if tx is None:
            return False
        if tx["txid"] != txid:
            raise Failed("TXID_MISMATCH", "Bitcoin Core returned an unexpected transaction.")
        return True


def sats(value):
    value = Decimal(str(value)) * 100_000_000
    if value != value.to_integral_value() or value < 0:
        raise Failed("AMOUNT", "Transaction has an invalid satoshi amount.")
    return int(value)


def assert_transaction(tx, intent):
    if tx["txid"] != intent["txid"]:
        raise Failed("TXID_MISMATCH", "Transaction does not match the fixture's unsigned transaction.")
    inputs = [f'{entry["txid"]}:{entry["vout"]}' for entry in tx["vin"]]
    if sorted(inputs) != sorted(intent["inputs"]) or len(set(inputs)) != len(inputs):
        raise Failed("INPUT_MISMATCH", "Transaction inputs differ from the declared fixture.")
    outputs = []
    for entry in tx["vout"]:
        address = entry["scriptPubKey"].get("address")
        if not address:
            raise Failed("OUTPUT_SCRIPT", "Every fixture output must have a decoded address.")
        outputs.append((address, sats(entry["value"])))
    expected = [(entry["address"], entry["sats"]) for entry in intent["outputs"]]
    if sorted(outputs) != sorted(expected):
        raise Failed("OUTPUT_MISMATCH", "Recipient or change outputs differ from the declared fixture.")


class Native:
    def __init__(self, binary, fixture):
        self.binary = binary
        self.fixture = fixture

    def call(self, action, **extra):
        request = dict(self.fixture, action=action, network="testnet4", **extra)
        try:
            result = subprocess.run([self.binary], input=json.dumps(request).encode(),
                                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=60)
        except (OSError, subprocess.TimeoutExpired):
            raise Blocked("NATIVE_RUNNER", "Native Connect helper is missing or timed out; rerun with the same checkpoint.") from None
        try:
            reply = json.loads(result.stdout)
        except (ValueError, UnicodeError):
            raise Failed("NATIVE_PROTOCOL", "Native helper did not return the expected JSON protocol.") from None
        if result.returncode or reply.get("ok") is not True:
            code = reply.get("code", "native_error")
            if not isinstance(code, str) or not code.replace("_", "").isalnum() or len(code) > 80:
                code = "native_error"
            environmental = {"connect_auth_required", "connect_auth_expired", "connect_device_required",
                             "transport_key_unavailable", "connect_unavailable", "connect_timeout",
                             "connect_fixture_not_found", "private_file_permissions_required",
                             "fixture_file_unavailable", "target_unresolved", "target_transport_unavailable",
                             "connect_rpc_refused", "connect_rest_refused", "output_directory_unavailable"}
            category = Blocked if code in environmental else Failed
            raise category("NATIVE_" + code.upper(), "Native Connect prerequisite or validation failed: " + code)
        if reply.get("network") != "testnet4" or reply.get("txid") != self.fixture["expected_txid"]:
            raise Failed("NATIVE_BINDING", "Native helper returned the wrong chain or transaction.")
        return reply


def public_transaction(txid):
    observations = []
    for endpoint in EXPLORERS:
        if http(endpoint + "/block-height/0").decode().strip() != GENESIS:
            raise Failed("EXPLORER_CHAIN", "Explorer genesis is not Bitcoin Testnet4.")
        raw = http(endpoint + "/tx/" + txid, missing_ok=True)
        if raw is None:
            observations.append(None)
            continue
        tx = json.loads(raw)
        if tx.get("txid") != txid:
            raise Failed("EXPLORER_TXID", "Explorer returned an unexpected transaction.")
        observations.append(tx)
    if any(tx is None for tx in observations):
        return None
    left, right = observations
    if left.get("fee") != right.get("fee") or left.get("vin") != right.get("vin") or left.get("vout") != right.get("vout"):
        raise Failed("EXPLORER_DISAGREEMENT", "Independent explorers disagree about the transaction.")
    both_confirmed = all(item.get("status", {}).get("confirmed") is True for item in observations)
    if both_confirmed and left["status"].get("block_hash") != right["status"].get("block_hash"):
        raise Blocked("EXPLORER_CONFIRMATION", "Independent explorers disagree about the confirmation block.")
    left["status"]["confirmed"] = both_confirmed
    return left
