"""Prepare/sign one completed QA vault's change. Deliberately cannot broadcast."""
import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
import re
import stat
from pathlib import Path
import subprocess
import tempfile
import time

from .clients import EXPLORERS, GENESIS, http
from .state import Blocked, Failed, atomic_write, private_read


def require(condition, code):
    if not condition:
        raise Failed(code, "Sponsor validation failed: " + code)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def encoded(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def transaction(raw):
    """Independent bounded Bitcoin wire decoder; accepts canonical compact sizes."""
    require(isinstance(raw, str) and len(raw) <= 8_000_000 and len(raw) % 2 == 0, "RAW_TRANSACTION")
    data = bytes.fromhex(raw)
    offset = 0
    def take(count):
        nonlocal offset
        require(0 <= count <= len(data) - offset, "RAW_TRANSACTION")
        result = data[offset:offset + count]
        offset += count
        return result
    def size():
        lead = take(1)[0]
        if lead < 253:
            return lead
        length = {253: 2, 254: 4, 255: 8}[lead]
        value = int.from_bytes(take(length), "little")
        require(value >= {253: 253, 254: 65536, 255: 4294967296}[lead], "RAW_TRANSACTION")
        return value
    version = take(4)
    witness = data[offset:offset + 2] == b"\x00\x01"
    if witness:
        take(2)
    start = offset
    count = size()
    require(1 <= count <= 1000, "RAW_TRANSACTION")
    inputs = []
    for _ in range(count):
        txid = take(32)[::-1].hex()
        vout = int.from_bytes(take(4), "little")
        script = take(size()).hex()
        sequence = int.from_bytes(take(4), "little")
        inputs.append({"txid": txid, "vout": vout, "script": script, "sequence": sequence})
    count = size()
    require(1 <= count <= 1000, "RAW_TRANSACTION")
    outputs = []
    for _ in range(count):
        value = int.from_bytes(take(8), "little")
        outputs.append({"value_sats": value, "script": take(size()).hex()})
    body = data[start:offset]
    witnesses = []
    if witness:
        for _ in inputs:
            count = size()
            require(count <= 1000, "RAW_TRANSACTION")
            witnesses.append([take(size()).hex() for _ in range(count)])
    locktime = take(4)
    require(offset == len(data), "RAW_TRANSACTION")
    unsigned = version + body + locktime
    return {"txid": hashlib.sha256(hashlib.sha256(unsigned).digest()).digest()[::-1].hex(),
            "version": int.from_bytes(version, "little"), "inputs": inputs, "outputs": outputs,
            "locktime": int.from_bytes(locktime, "little"), "witnesses": witnesses}


def p2wsh_script(address):
    """Decode and checksum a lowercase Testnet v0 P2WSH address."""
    alphabet = "qpzry9x8gf2tvdw0s3jn54khce6mua7l"
    require(isinstance(address, str) and address.startswith("tb1") and address == address.lower(), "TARGET_ADDRESS")
    values = [alphabet.index(c) for c in address[3:]]
    check = 1
    for value in [3, 3, 0, 20, 2] + values:  # bech32 HRP expansion of "tb"
        top, check = check >> 25, ((check & 0x1ffffff) << 5) ^ value
        for index, generator in enumerate((0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3)):
            if (top >> index) & 1:
                check ^= generator
    require(check == 1 and len(values) == 59 and values[0] == 0, "TARGET_ADDRESS")
    accumulator = bits = 0
    program = bytearray()
    for value in values[1:-6]:
        accumulator = (accumulator << 5) | value
        bits += 5
        while bits >= 8:
            bits -= 8
            program.append((accumulator >> bits) & 255)
    require(bits < 5 and (accumulator & ((1 << bits) - 1)) == 0 and len(program) == 32, "TARGET_ADDRESS")
    return "0020" + program.hex()


def observe(txid, vout):
    evidence = []
    for endpoint in EXPLORERS:
        require(http(endpoint + "/block-height/0").decode().strip() == GENESIS, "EXPLORER_CHAIN")
        raw = http(endpoint + "/tx/" + txid + "/hex").decode().strip()
        require(transaction(raw)["txid"] == txid, "SOURCE_TXID")
        status = json.loads(http(endpoint + f"/tx/{txid}/outspend/{vout}"))
        require(status.get("spent") is False, "SOURCE_SPENT")
        evidence.append({"endpoint": endpoint, "genesis": GENESIS, "observed_at": int(time.time()),
                         "txid": txid, "vout": vout, "raw_tx": raw, "spent": False})
    require(evidence[0]["raw_tx"] == evidence[1]["raw_tx"], "EXPLORER_DISAGREEMENT")
    return evidence


def read_run(directory):
    content = {name: private_read(directory / name) for name in ("provision.json", "descriptor.txt", "checkpoint.json")}
    state = json.loads(content["provision.json"])
    checkpoint = json.loads(content["checkpoint.json"])
    artifacts = checkpoint.get("artifacts", {})
    account = artifacts.get("account_identity")
    require(isinstance(account, str) and re.fullmatch(r"[0-9a-f]{64}", account) is not None
            and type(state.get("owner_user_id")) is int and state["owner_user_id"] > 0
            and type(state.get("vault_id")) is int and state["vault_id"] > 0
            and artifacts.get("vault_id") == str(state["vault_id"])
            and isinstance(state.get("descriptor_id"), str) and bool(state["descriptor_id"])
            and artifacts.get("descriptor_id") == state["descriptor_id"], "ACCOUNT_BINDING")
    binding = {"directory": str(directory), "run_id": state["run_id"],
               "state_sha256": sha(content["provision.json"]), "descriptor_sha256": sha(content["descriptor.txt"]),
               "checkpoint_sha256": sha(content["checkpoint.json"])}
    return state, checkpoint, binding


def make_intent(source_dir, target_dir, amount, fee, prior_dir=None):
    require(type(amount) is int and 100000 <= amount <= 500000 and type(fee) is int and 100 <= fee <= 1000, "AMOUNT_BOUNDS")
    source, checkpoint, source_binding = read_run(source_dir)
    target, target_checkpoint, target_binding = read_run(target_dir)
    require(source["run_id"] != target["run_id"] and source["vault_id"] != target["vault_id"]
            and source["owner_user_id"] is not None and source["owner_user_id"] == target["owner_user_id"]
            and checkpoint["artifacts"]["account_identity"] == target_checkpoint["artifacts"]["account_identity"], "RUN_IDENTITY")
    spend = source["spend"]
    txid = spend["txid"]
    require(checkpoint["artifacts"]["public_acceptance"]["txid"] == txid
            and checkpoint["artifacts"]["broadcast_txid"] == txid, "SOURCE_INCOMPLETE")
    prior = None
    expected_change = spend["change_sats"]
    if prior_dir is not None:
        intent_text = private_read(prior_dir / "sponsor-intent.json")
        receipt_text = private_read(prior_dir / "sponsor-receipt.json")
        parent = json.loads(intent_text)
        receipt = json.loads(receipt_text)
        require(parent.get("version") == 1 and "prior_sponsor" not in parent
                and parent.get("source") == source_binding
                and parent.get("previous_txid") == txid, "PARENT_BINDING")
        validate_receipt(receipt, parent, sha(intent_text))
        old_target = parent["target"]
        prior_state, prior_checkpoint, prior_binding = read_run(Path(old_target["directory"]).resolve())
        require(all(prior_binding[k] == old_target[k] for k in ("directory", "run_id", "descriptor_sha256"))
                and prior_state["owner_user_id"] == source["owner_user_id"]
                and prior_state["vault_id"] != source["vault_id"]
                and prior_checkpoint["artifacts"]["account_identity"] == checkpoint["artifacts"]["account_identity"],
                "PARENT_TARGET_BINDING")
        prior = {"intent_json": intent_text.decode(), "receipt_json": receipt_text.decode(), "target": prior_binding}
        txid, expected_change = receipt["txid"], receipt["change_sats"]
    evidence = observe(txid, 1)
    if prior is not None:
        require(evidence[0]["raw_tx"] == receipt["raw_tx"], "PARENT_SIGNED_BYTES")
    previous = transaction(evidence[0]["raw_tx"])
    require(len(previous["outputs"]) == 2, "SOURCE_CHANGE")
    change = previous["outputs"][1]
    require(change["value_sats"] == expected_change and change["script"] == p2wsh_script(spend["change_address"]), "SOURCE_CHANGE")
    target_address = target_checkpoint["artifacts"]["receive_address"]
    remaining = change["value_sats"] - amount - fee
    require(1000 <= remaining and change["value_sats"] <= 10000000, "AMOUNT_BOUNDS")
    intent = {"version": 2 if prior else 1, "network": "testnet4", "authorization_ref": "coincube#1137:testnet4-sponsor",
            "source": source_binding, "target": target_binding, "previous_txid": txid, "previous_vout": 1,
            "previous_value_sats": change["value_sats"], "previous_raw_sha256": sha(evidence[0]["raw_tx"].encode()),
            "source_script": change["script"], "target_address": target_address,
            "target_script": p2wsh_script(target_address), "amount_sats": amount,
            "fee_sats": fee, "change_sats": remaining}
    if prior is not None:
        intent["prior_sponsor"] = prior
    return intent, evidence, source


def validate_receipt(receipt, intent, intent_hash):
    require(receipt.get("ok") is True and receipt.get("action") == "sponsor" and receipt.get("network") == "testnet4"
            and receipt.get("intent_sha256") == intent_hash, "RECEIPT_BINDING")
    decoded = transaction(receipt["raw_tx"])
    require(decoded["version"] == 2 and decoded["locktime"] == 0
            and decoded["inputs"] == [{"txid": intent["previous_txid"], "vout": 1, "script": "", "sequence": 0xffffffff}]
            and decoded["outputs"] == [{"value_sats": intent["amount_sats"], "script": intent["target_script"]},
                                       {"value_sats": intent["change_sats"], "script": intent["source_script"]}]
            and intent["previous_value_sats"] - sum(v["value_sats"] for v in decoded["outputs"]) == intent["fee_sats"]
            and decoded["txid"] == receipt.get("txid") and len(decoded["witnesses"]) == 1
            and len(decoded["witnesses"][0]) >= 3, "SIGNED_TRANSACTION")
    for field in ("amount_sats", "fee_sats", "change_sats"):
        require(receipt.get(field) == intent[field], "RECEIPT_BINDING")


@contextmanager
def output_lock(directory, protected):
    require(directory.is_absolute() and not directory.is_symlink(), "OUTPUT_DIRECTORY")
    resolved = directory.resolve()
    require(not any(resolved == path or path in resolved.parents or resolved in path.parents for path in protected), "OUTPUT_ISOLATION")
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = directory.stat()
    require(info.st_uid == os.getuid() and not info.st_mode & 0o077, "OUTPUT_PERMISSIONS")
    fd = os.open(directory / "sponsor.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Blocked("SPONSOR_BUSY", "Another sponsor invocation owns the output directory.") from None
        yield
    finally:
        os.close(fd)


def binary_digest(path):
    path = Path(path)
    require(path.is_absolute() and not path.is_symlink(), "NATIVE_BINARY")
    info = path.stat()
    require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o022,
            "NATIVE_BINARY")
    return sha(path.read_bytes())


def verify_snapshot(intent):
    for key in ("source", "target"):
        require(read_run(Path(intent[key]["directory"]))[2] == intent[key], "RUN_SNAPSHOT_CHANGED")
    if "prior_sponsor" in intent:
        binding = intent["prior_sponsor"]["target"]
        require(read_run(Path(binding["directory"]))[2] == binding, "PARENT_SNAPSHOT_CHANGED")


def execute(args):
    require(all(Path(value).is_absolute() and not Path(value).is_symlink()
                for value in (args.source_run, args.target_run, args.output_dir)), "ABSOLUTE_PATHS")
    source_dir, target_dir, output = (Path(value).resolve() for value in (args.source_run, args.target_run, args.output_dir))
    source_state = json.loads(private_read(source_dir / "provision.json"))
    target_state = json.loads(private_read(target_dir / "provision.json"))
    protected = [source_dir, target_dir, Path(source_state["profile_dir"]).resolve(), Path(target_state["profile_dir"]).resolve()]
    prior_dir = getattr(args, "source_sponsor_dir", None)
    if prior_dir is not None:
        prior_dir = Path(prior_dir)
        require(prior_dir.is_absolute() and not prior_dir.is_symlink(), "PARENT_DIRECTORY")
        prior_dir = prior_dir.resolve()
        parent = json.loads(private_read(prior_dir / "sponsor-intent.json"))
        prior_target = Path(parent["target"]["directory"]).resolve()
        prior_state = json.loads(private_read(prior_target / "provision.json"))
        protected += [prior_dir, prior_target, Path(prior_state["profile_dir"]).resolve()]
    with output_lock(output, protected):
        intent, evidence, source = make_intent(source_dir, target_dir, args.amount_sats, args.fee_sats, prior_dir)
        path = output / "sponsor-intent.json"
        content = encoded(intent)
        if path.exists():
            require(private_read(path) == content, "SPONSOR_INTENT_CHANGED")
        else:
            require(args.sign_intent_sha256 is None, "PREPARE_FIRST")
            atomic_write(path, content)
        intent_hash = sha(content)
        if args.sign_intent_sha256 is None:
            return {"prepared": True, "intent_sha256": intent_hash, "source_txid": intent["previous_txid"],
                    "amount_sats": intent["amount_sats"], "fee_sats": intent["fee_sats"], "change_sats": intent["change_sats"]}
        require(args.sign_intent_sha256 == intent_hash and args.native_binary and Path(args.native_binary).is_absolute(), "SIGN_INTENT_BINDING")
        binary_hash = binary_digest(args.native_binary)
        receipt_path = output / "sponsor-receipt.json"
        fd, temporary = tempfile.mkstemp(prefix=".sponsor-evidence-", dir=output)
        try:
            with os.fdopen(fd, "wb") as stream:
                stream.write(encoded(evidence))
            request = {"action": "sponsor", "network": "testnet4", "profile_dir": source["profile_dir"],
                       "run_dir": str(output), "run_id": source["run_id"], "sponsor_intent_file": str(path),
                       "sponsor_evidence_file": temporary, "sponsor_intent_sha256": intent_hash}
            if receipt_path.exists():
                request["sponsor_receipt_file"] = str(receipt_path)
            result = subprocess.run([args.native_binary], input=encoded(request), stdout=subprocess.PIPE,
                                    stderr=subprocess.DEVNULL, timeout=120)
            require(binary_digest(args.native_binary) == binary_hash, "NATIVE_BINARY_CHANGED")
            verify_snapshot(intent)
            receipt = json.loads(result.stdout)
            require(result.returncode == 0, "NATIVE_SPONSOR")
            validate_receipt(receipt, intent, intent_hash)
            fresh = observe(intent["previous_txid"], 1)
            require(sha(fresh[0]["raw_tx"].encode()) == intent["previous_raw_sha256"], "SOURCE_CHANGED")
            verify_snapshot(intent)
            require(private_read(path) == content, "SPONSOR_INTENT_CHANGED")
            if receipt_path.exists():
                saved = json.loads(private_read(receipt_path))
                validate_receipt(saved, intent, intent_hash)
                require(saved["raw_tx"] == receipt["raw_tx"], "SIGNED_BYTES_CHANGED")
            else:
                receipt["evidence"] = fresh
                receipt["native_binary_sha256"] = binary_hash
                atomic_write(receipt_path, encoded(receipt))
            return {"signed": True, "broadcast": False, "txid": receipt["txid"], "intent_sha256": intent_hash}
        finally:
            os.unlink(temporary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for field in ("source-run", "target-run", "output-dir"):
        parser.add_argument("--" + field, required=True)
    parser.add_argument("--amount-sats", type=int, required=True)
    parser.add_argument("--fee-sats", type=int, required=True)
    parser.add_argument("--native-binary")
    parser.add_argument("--source-sponsor-dir", help="Private original v1 sponsor intent/receipt directory; supports exactly one sponsor hop.")
    parser.add_argument("--sign-intent-sha256")
    args = parser.parse_args()
    os.umask(0o077)
    try:
        result = execute(args)
    except Blocked as error:
        print(json.dumps({"ok": False, "code": error.code}))
        return 2
    except Exception:
        print('{"ok":false,"code":"SPONSOR_FAILED"}')
        return 2
    print(json.dumps(result))
    return 0
