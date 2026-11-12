"""Automatic, bounded QA funding. Public snapshots only; native keys stay native."""
from contextlib import contextmanager
from copy import deepcopy
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import stat
from types import SimpleNamespace
import tempfile
import time

from . import sponsor
from .clients import EXPLORERS, GENESIS, http, public_transaction
from .state import Blocked, Failed, atomic_write, digest, private_read

# One durable host-wide registry, shared across worktrees and workstation files.
# It contains no wallet secrets. Never expire a reservation merely by age.
ROOT = Path.home() / "Library/Application Support/COINCUBE QA/funding-v1"
FILES = ("provision.json", "descriptor.txt", "checkpoint.json")
IDENTITIES = ("account_identity", "desktop_identity", "cube_id", "cube_uuid", "vault_id",
              "descriptor_id", "receive_address", "target_key_id", "target_fingerprint",
              "strict_acceptance", "strict_account", "strict_process", "qa_build")


def require(value, code):
    if not value:
        raise Failed("QA_FUNDING_" + code, "Automatic QA funding validation failed: " + code)


def options(value):
    require(isinstance(value, dict) and {"version", "sources"} <= set(value)
            and not set(value) - {"version", "sources", "maximum_amount_sats", "fee_sats",
                                  "faucet_wait_seconds", "register_completed"}, "CONFIG")
    require(type(value["version"]) is int and value["version"] == 1, "CONFIG")
    result = {"maximum_amount_sats": 500000, "fee_sats": 1000, "faucet_wait_seconds": 30,
              "register_completed": True, **deepcopy(value)}
    for key, low, high in (("maximum_amount_sats", 100000, 500000), ("fee_sats", 100, 1000),
                            ("faucet_wait_seconds", 0, 3600)):
        require(type(result[key]) is int and low <= result[key] <= high, "CONFIG")
    require(type(result["register_completed"]) is bool and isinstance(result["sources"], list)
            and len(result["sources"]) <= 32, "CONFIG")
    for entry in result["sources"]:
        require(isinstance(entry, dict) and {"source_run"} <= set(entry)
                and not set(entry) - {"source_run", "source_sponsor_dir"}, "SOURCE_CONFIG")
        require(all(isinstance(v, str) and Path(v).is_absolute() for v in entry.values()), "SOURCE_CONFIG")
    return result


def private_dir(path):
    require(not path.is_symlink(), "DIRECTORY")
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = path.stat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077,
            "DIRECTORY")


@contextmanager
def pool():
    private_dir(ROOT)
    fd = os.open(ROOT / "pool.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(fd)
        require(info.st_uid == os.getuid() and not info.st_mode & 0o077, "LOCK")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Blocked("QA_FUNDING_BUSY", "Another QA operation owns the funding registry.") from None
        path = ROOT / "pool.json"
        data = json.loads(private_read(path)) if path.exists() else {"version": 1, "accounts": {}, "claims": {}, "runs": {}}
        require(isinstance(data, dict) and type(data.get("version")) is int and data["version"] == 1
                and all(isinstance(data.get(k), dict) for k in ("accounts", "claims", "runs")), "REGISTRY")
        require(all(isinstance(v, dict) and data["runs"].get(v.get("target_key")) == k
                    for k, v in data["claims"].items())
                and all(k in data["claims"] and data["claims"][k].get("target_key") == r
                        for r, k in data["runs"].items()), "REGISTRY_INDEX")
        yield data
    finally:
        os.close(fd)


def save_pool(data):
    raw = sponsor.encoded(data)
    require(len(raw) <= 4000000, "REGISTRY_FULL")
    atomic_write(ROOT / "pool.json", raw)


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def root_keys(config):
    return {digest({k: str(Path(v).resolve()) for k, v in e.items()}) for e in config["sources"]}


def identity(directory, run_id, binding):
    state, checkpoint, _ = sponsor.read_run(directory)
    a = checkpoint["artifacts"]
    require(type(checkpoint.get("version")) is int and checkpoint["version"] == 1 and checkpoint.get("binding") == binding
            and type(state.get("version")) is int and state["version"] == 1 and state.get("run_id") == run_id
            and state.get("cube_uuid") == run_id
            and isinstance(state.get("profile_dir"), str)
            and Path(state["profile_dir"]).resolve() == directory / "profile", "TARGET")
    require(all(k in a and a[k] is not None for k in IDENTITIES), "TARGET_FIELDS")
    for key in ("account_identity", "desktop_identity", "strict_account"):
        require(isinstance(a[key], str) and re.fullmatch(r"[0-9a-f]{64}", a[key]), "TARGET_FIELDS")
    require(a["strict_account"] == a["account_identity"] and a["cube_uuid"] == run_id
            and a["cube_id"] == state.get("cube_id") and type(a["cube_id"]) is int and type(state.get("cube_id")) is int
            and a["cube_id"] > 0 and a["strict_acceptance"] == {"version": 1, "config_binding": binding}, "TARGET_FIELDS")
    process = a["strict_process"]
    require(isinstance(process, dict) and isinstance(process.get("process"), dict)
            and process["process"].get("bundle_id") == "io.coincube.keychainApp"
            and type(process["process"].get("pid")) is int and process["process"]["pid"] > 0
            and type(process.get("build")) is int and process["build"] >= 39
            and isinstance(process.get("device"), str) and re.fullmatch(r"[0-9a-f]{64}", process["device"]), "PROCESS")
    require(isinstance(a["qa_build"], dict) and isinstance(a["qa_build"].get("app"), dict)
            and a["qa_build"].get("define") == "COINCUBE_QA_HOME_REFRESH=true"
            and a["qa_build"].get("app", {}).get("build") == process["build"], "BUILD")
    require(isinstance(a["target_key_id"], str) and a["target_key_id"]
            and isinstance(a["target_fingerprint"], str) and re.fullmatch(r"[0-9a-f]{8}", a["target_fingerprint"]), "TARGET_FIELDS")
    sponsor.p2wsh_script(a["receive_address"])
    return {"state_sha256": sponsor.sha(private_read(directory / "provision.json")),
            "descriptor_sha256": sponsor.sha(private_read(directory / "descriptor.txt")),
            "binding": binding, "run_id": run_id, "artifacts": {k: deepcopy(a[k]) for k in IDENTITIES}}


def entries(data, account, config):
    managed = data["accounts"].get(account, []) if config["register_completed"] else []
    require(isinstance(managed, list), "REGISTRY")
    require(all(isinstance(e, dict) and e.get("kind") in ("completed", "sponsor")
                and isinstance(e.get("entry"), dict) and isinstance(e["entry"].get("source_run"), str)
                for e in managed), "REGISTRY")
    roots = root_keys(config)
    result = []
    for e in managed:
        require(isinstance(e, dict) and e.get("kind") in ("completed", "sponsor") and isinstance(e.get("entry"), dict), "REGISTRY")
        if not roots.intersection(e.get("authorization_roots", [])):
            continue
        result.append((e["entry"], e))
    result += [(e, None) for e in config["sources"]]
    seen = set()
    for entry, enrolled in result:
        key = digest(entry)
        if key not in seen:
            seen.add(key)
            yield entry, enrolled


def source_metadata(entry, enrolled, account, owner):
    source = Path(entry["source_run"])
    require(source.is_absolute() and not source.is_symlink(), "SOURCE")
    source = source.resolve()
    state, checkpoint, binding = sponsor.read_run(source)
    require(checkpoint["artifacts"]["account_identity"] == account and state["owner_user_id"] == owner,
            "ACCOUNT")
    if enrolled is not None:
        require(enrolled.get("source_binding") == binding, "SOURCE_CHANGED")
    spend = state.get("spend")
    require(isinstance(spend, dict) and checkpoint["artifacts"].get("public_acceptance", {}).get("txid") == spend.get("txid")
            and checkpoint["artifacts"].get("broadcast_txid") == spend.get("txid"), "SOURCE_INCOMPLETE")
    previous = spend["change_sats"]
    prior = entry.get("source_sponsor_dir")
    if prior is not None:
        prior = Path(prior)
        require(prior.is_absolute() and not prior.is_symlink(), "SOURCE")
        prior = prior.resolve()
        raw = private_read(prior / "sponsor-intent.json")
        receipt_raw = private_read(prior / "sponsor-receipt.json")
        parent = json.loads(raw)
        receipt = json.loads(receipt_raw)
        require(parent.get("version") == 1 and "prior_sponsor" not in parent and parent.get("source") == binding,
                "SOURCE_PARENT")
        sponsor.validate_receipt(receipt, parent, sponsor.sha(raw))
        if enrolled is not None:
            require(enrolled.get("receipt_sha256") == sponsor.sha(receipt_raw), "SOURCE_CHANGED")
        previous = receipt["change_sats"]
    require(type(previous) is int, "SOURCE")
    return source, prior, previous, binding


def preflight(runner):
    config = options(runner.config["qa_funding"])
    require(runner.strict is not None, "MODE")
    with pool() as data:
        for entry, enrolled in entries(data, runner.artifacts["account_identity"], config):
            source_metadata(entry, enrolled, runner.artifacts["account_identity"], runner.account_user_id)


class Funding:
    def __init__(self, runner):
        self.runner = runner
        self.config = options(runner.config["qa_funding"])
        require(runner.strict is not None and not runner.strict.requested, "MODE")
        self.live = runner.journal.directory.resolve()
        self.run_id = runner.run_id
        self.binding = runner.journal.data["binding"]
        require(self.binding == digest(runner.config), "CONFIG_BINDING")
        self.core = identity(self.live, self.run_id, self.binding)
        self.account = self.core["artifacts"]["account_identity"]
        self.owner = json.loads(private_read(self.live / "provision.json"))["owner_user_id"]
        self.key = digest({"directory": str(self.live), "run_id": self.run_id, "binding": self.binding})
        self.snapshot = self.live.with_name(self.live.name + "-funding-snapshot")
        self.output = self.live.with_name(self.live.name + "-funding")
        require(not any(p == ROOT.resolve() or ROOT.resolve() in p.parents
                        for p in (self.live, self.snapshot, self.output)), "DIRECTORY")
        self.native = runner.provision.binary
        self.native_sha256 = sponsor.binary_digest(self.native)

    def guard(self):
        require(sponsor.binary_digest(self.native) == self.native_sha256, "NATIVE_CHANGED")
        require(identity(self.live, self.run_id, self.binding) == self.core, "TARGET_CHANGED")
        cp = json.loads(private_read(self.live / "checkpoint.json"))["artifacts"]
        require(not any(k in cp for k in ("request_id", "session_id", "intent", "approval_started", "pin_dispatch_started")),
                "TARGET_ADVANCED")
        require(json.loads(private_read(self.live / "provision.json")).get("spend") is None, "TARGET_ADVANCED")
        self.runner.strict.observe()

    def freeze(self):
        self.guard()
        if not self.snapshot.exists():
            temporary = Path(tempfile.mkdtemp(prefix=".qa-funding-", dir=self.live.parent))
            try:
                for name in FILES:
                    atomic_write(temporary / name, private_read(self.live / name))
                manifest = {"version": 1, "directory": str(self.live), "identity": self.core,
                            "hashes": {name: sponsor.sha(private_read(temporary / name)) for name in FILES}}
                atomic_write(temporary / "manifest.json", sponsor.encoded(manifest))
                os.rename(temporary, self.snapshot)
                sync_directory(self.live.parent)
            finally:
                if temporary.exists():
                    shutil.rmtree(temporary)
        private_dir(self.snapshot)
        manifest_raw = private_read(self.snapshot / "manifest.json")
        manifest = json.loads(manifest_raw)
        require(manifest.get("version") == 1 and manifest.get("directory") == str(self.live)
                and manifest.get("identity") == self.core
                and manifest.get("hashes") == {n: sponsor.sha(private_read(self.snapshot / n)) for n in FILES}, "SNAPSHOT")
        # Check copied content against live identity, not just a mutable manifest.
        snap_state, snap_cp, _ = sponsor.read_run(self.snapshot)
        require(sponsor.sha(private_read(self.snapshot / "provision.json")) == self.core["state_sha256"]
                and sponsor.sha(private_read(self.snapshot / "descriptor.txt")) == self.core["descriptor_sha256"]
                and snap_state["run_id"] == self.run_id and snap_cp.get("binding") == self.binding
                and all(snap_cp["artifacts"].get(k) == self.core["artifacts"][k] for k in IDENTITIES), "SNAPSHOT")
        return sponsor.sha(manifest_raw)

    def target_output(self):
        self.guard()
        values = self.runner.core.utxos(self.core["artifacts"]["receive_address"])
        eligible = [v for v in values if 100000 <= v[2] <= 10000000
                    and v[2] >= self.runner.config["amount_sats"] + self.runner.config["fee_sats"] + 1000]
        require(len(eligible) <= 1, "TARGET_AMBIGUOUS")
        if eligible:
            return {"txid": eligible[0][0], "vout": eligible[0][1], "value_sats": eligible[0][2]}
        require(not values, "TARGET_UNSUITABLE")
        return None

    def adopt(self, point, proof=None):
        self.guard()
        raw = self.runner.core.raw(point["txid"])
        atomic_write(self.live / "funding.hex", raw.encode())
        if proof is not None:
            self.runner.pin_artifact("qa_funding", proof)
        self.runner.pin_artifact("funding_outpoint", point)

    def arguments(self, record):
        return SimpleNamespace(source_run=record["entry"]["source_run"], target_run=str(self.snapshot),
                               output_dir=str(self.output), amount_sats=record["amount_sats"],
                               fee_sats=record["fee_sats"], native_binary=self.native,
                               source_sponsor_dir=record["entry"].get("source_sponsor_dir"), sign_intent_sha256=None)

    def validate_record(self, record):
        require(record.get("target_key") == self.key and record.get("account") == self.account
                and record.get("live") == str(self.live) and record.get("snapshot") == str(self.snapshot)
                and record.get("output") == str(self.output) and record.get("identity") == self.core
                and record.get("manifest_sha256") == self.freeze(), "RESERVATION")
        require(record["native_sha256"] == sponsor.binary_digest(self.native), "NATIVE_CHANGED")
        raw = record["intent_json"].encode()
        require(sponsor.sha(raw) == record["intent_sha256"], "RESERVATION")
        intent = json.loads(raw)
        require(intent["network"] == "testnet4" and intent["previous_vout"] == 1
                and intent["amount_sats"] == record["amount_sats"] <= self.config["maximum_amount_sats"]
                and intent["fee_sats"] == record["fee_sats"] == self.config["fee_sats"]
                and intent["target_address"] == self.core["artifacts"]["receive_address"]
                and intent["target_script"] == sponsor.p2wsh_script(intent["target_address"])
                and intent["target"]["directory"] == str(self.snapshot)
                and intent["target"]["run_id"] == self.run_id, "PLAN")
        return intent

    def observed(self, receipt, intent):
        present = []
        for endpoint in EXPLORERS:
            require(http(endpoint + "/block-height/0").decode().strip() == GENESIS, "CHAIN")
            raw = http(endpoint + "/tx/" + receipt["txid"] + "/hex", missing_ok=True)
            if raw is None:
                present.append(False)
                continue
            require(raw.decode().strip() == receipt["raw_tx"], "PUBLISHED_BYTES")
            spent = json.loads(http(endpoint + "/tx/" + receipt["txid"] + "/outspend/0"))
            require(spent.get("spent") is False, "TARGET_SPENT")
            present.append(True)
        if all(present):
            sponsor.validate_receipt(receipt, intent, sponsor.sha(sponsor.encoded(intent)))
            return "known"
        return "partial" if any(present) else "missing"

    def complete(self, data, record, receipt, intent):
        record["phase"] = "accepted"
        save_pool(data)
        # A v1 sponsor can expose one more authorized source-change hop.
        if intent["version"] == 1 and self.config["register_completed"]:
            entry = {"source_run": record["entry"]["source_run"], "source_sponsor_dir": str(self.output)}
            add_source(data, self.account, {"kind": "sponsor", "entry": entry,
                       "authorization_roots": record["authorization_roots"],
                       "source_binding": sponsor.read_run(Path(entry["source_run"]))[2],
                       "receipt_sha256": sponsor.sha(private_read(self.output / "sponsor-receipt.json"))})
        point = {"txid": receipt["txid"], "vout": 0, "value_sats": intent["amount_sats"]}
        self.adopt(point, {"version": 1, "intent_sha256": record["intent_sha256"], "txid": receipt["txid"],
                          "amount_sats": intent["amount_sats"], "fee_sats": intent["fee_sats"],
                          "receipt_sha256": record["receipt_sha256"]})

    def execute(self):
        # Wait for a late faucet before reserving/signing any source. Existing
        # reservations bypass this grace and always reconcile their own bytes.
        with pool() as data:
            existing = data["runs"].get(self.key)
        if existing is None:
            start = self.runner.artifacts.get("funding_claim_at")
            if start is None:
                start = int(time.time())
                self.runner.journal.artifact("funding_claim_at", start)
            require(type(start) is int and start <= time.time() + 5, "CLOCK")
            while True:
                point = self.target_output()
                if point:
                    self.adopt(point)
                    return
                if time.time() >= start + self.config["faucet_wait_seconds"]:
                    break
                time.sleep(1)
        with pool() as data:
            manifest_hash = self.freeze()
            key = data["runs"].get(self.key)
            if key is None:
                point = self.target_output()
                if point:
                    self.adopt(point)
                    return
                for entry, enrolled in entries(data, self.account, self.config):
                    source, prior, value, _ = source_metadata(entry, enrolled, self.account, self.owner)
                    amount = min(self.config["maximum_amount_sats"], value - self.config["fee_sats"] - 1000)
                    if amount < 100000:
                        continue
                    try:
                        intent, _, _ = sponsor.make_intent(source, self.snapshot, amount, self.config["fee_sats"], prior)
                    except Failed as error:
                        if error.code in ("SOURCE_SPENT", "AMOUNT_BOUNDS"):
                            continue
                        raise
                    key = intent["previous_txid"] + ":1"
                    if key in data["claims"]:
                        key = None
                        continue
                    record = {"target_key": self.key, "account": self.account, "entry": deepcopy(entry),
                              "authorization_roots": sorted(root_keys(self.config).intersection(enrolled["authorization_roots"]))
                              if enrolled else [digest({k: str(Path(v).resolve()) for k, v in entry.items()})],
                              "live": str(self.live), "snapshot": str(self.snapshot), "output": str(self.output),
                              "identity": self.core, "manifest_sha256": manifest_hash,
                              "native_sha256": sponsor.binary_digest(self.native), "amount_sats": amount,
                              "fee_sats": self.config["fee_sats"], "intent_json": sponsor.encoded(intent).decode(),
                              "intent_sha256": sponsor.sha(sponsor.encoded(intent)), "phase": "reserved", "attempts": []}
                    data["claims"][key] = record
                    data["runs"][self.key] = key
                    save_pool(data)  # Before native key lookup, even across crashes.
                    break
                else:
                    key = None
                if key is None:
                    raise Blocked("QA_FUNDING_EMPTY", "No eligible unreserved disposable QA source is available.")
            require(key in data["claims"], "RESERVATION")
            record = data["claims"][key]
            intent = self.validate_record(record)
            require(key == intent["previous_txid"] + ":1", "RESERVATION")
            allowed = [e for e, _ in entries(data, self.account, self.config)]
            require(record["entry"] in allowed, "SOURCE_REVOKED")
            if record["phase"] == "external":
                point = record["external_point"]
                require(self.target_output() == point, "TARGET_CHANGED")
                self.adopt(point)
                return
            if record["phase"] == "reserved":
                point = self.target_output()
                if point:
                    record.update(phase="external", external_point=point)
                    save_pool(data)
                    self.adopt(point)
                    return
                args = self.arguments(record)
                require(sponsor.execute(args)["intent_sha256"] == record["intent_sha256"], "PLAN_CHANGED")
                self.guard()
                args.sign_intent_sha256 = record["intent_sha256"]
                sponsor.execute(args)
                receipt_raw = private_read(self.output / "sponsor-receipt.json")
                record.update(phase="signed", receipt_sha256=sponsor.sha(receipt_raw))
                save_pool(data)
            require(record["phase"] in ("signed", "broadcasting", "accepted"), "PHASE")
            raw = private_read(self.output / "sponsor-receipt.json")
            require(sponsor.sha(raw) == record["receipt_sha256"], "RECEIPT_CHANGED")
            receipt = json.loads(raw)
            require(receipt.get("native_binary_sha256") == record["native_sha256"], "NATIVE_CHANGED")
            sponsor.validate_receipt(receipt, intent, record["intent_sha256"])
            until = time.monotonic() + self.runner.deadline
            posts = 0
            while True:
                self.guard()
                status = self.observed(receipt, intent)
                if status == "known":
                    self.complete(data, record, receipt, intent)
                    return
                if status == "missing" and posts < len(EXPLORERS):
                    fresh = sponsor.observe(intent["previous_txid"], 1)
                    require(sponsor.sha(fresh[0]["raw_tx"].encode()) == intent["previous_raw_sha256"], "SOURCE_CHANGED")
                    if record["phase"] == "signed":
                        point = self.target_output()
                        if point:
                            record.update(phase="external", external_point=point)
                            save_pool(data)
                            self.adopt(point)
                            return
                    self.guard()
                    require(len(record["attempts"]) < 64, "RETRY_LIMIT")
                    endpoint = EXPLORERS[posts]
                    record["phase"] = "broadcasting"
                    record["attempts"].append({"endpoint": endpoint, "at": int(time.time()),
                                               "raw_sha256": sponsor.sha(receipt["raw_tx"].encode())})
                    save_pool(data)  # Durable uncertain outcome before POST.
                    posts += 1
                    try:
                        returned = http(endpoint + "/tx", body=receipt["raw_tx"].encode(),
                                        headers={"Content-Type": "text/plain"}).decode().strip()
                        record["response_matches"] = returned == receipt["txid"]
                        save_pool(data)
                    except (Blocked, UnicodeError):
                        pass  # Always reconcile; never construct another spend.
                    continue
                if time.monotonic() >= until:
                    raise Blocked("QA_FUNDING_PROPAGATION", "The frozen QA funding payment is not yet agreed by both explorers.")
                time.sleep(2)


def add_source(data, account, item):
    values = data["accounts"].setdefault(account, [])
    require(isinstance(values, list), "REGISTRY")
    existing = [v for v in values if v["entry"] == item["entry"]]
    if existing:
        require(existing == [item], "SOURCE_CHANGED")
        return
    require(len(values) < 256, "REGISTRY_FULL")
    values.insert(0, item)
    save_pool(data)


def register_completed(runner):
    from .provisioning import STAGES
    config = options(runner.config["qa_funding"])
    if not config["register_completed"]:
        return
    directory = runner.journal.directory.resolve()
    state, cp, binding = sponsor.read_run(directory)
    require(cp.get("binding") == runner.journal.data["binding"] == digest(runner.config) and state["run_id"] == runner.run_id
            and state["cube_uuid"] == runner.run_id and set(cp.get("stages", {})) == set(STAGES)
            and all(v.get("status") == "passed" for v in cp["stages"].values()), "COMPLETION")
    a = cp["artifacts"]
    require(runner.strict is not None and a.get("signature_count_min", 0) >= 2
            and a.get("strict_pin_input", {}).get("completed") is True
            and a.get("strict_home_refresh", {}).get("home_refresh_proven") is True, "COMPLETION")
    runner.strict.require_completion(a["request_id"], a["session_id"])
    raw = private_read(directory / "signed.hex").decode().strip()
    decoded = sponsor.transaction(raw)
    require(decoded["txid"] == a.get("broadcast_txid") == a.get("public_acceptance", {}).get("txid")
            and sponsor.sha(raw.encode()) == a.get("broadcast_hex_sha256"), "COMPLETION")
    public = public_transaction(decoded["txid"])
    require(public is not None and public.get("fee") == a.get("fee_sats"), "COMPLETION")
    for endpoint in EXPLORERS:
        require(http(endpoint + "/tx/" + decoded["txid"] + "/hex").decode().strip() == raw, "COMPLETION")
    with pool() as data:
        # Freeze public source evidence. Replaying the completed live run may
        # update stage timestamps; it must not invalidate a registered donor.
        snapshot = directory.with_name(directory.name + "-completed-source")
        require(ROOT.resolve() not in snapshot.parents and snapshot != ROOT.resolve(), "DIRECTORY")
        if not snapshot.exists():
            temporary = Path(tempfile.mkdtemp(prefix=".qa-source-", dir=directory.parent))
            try:
                for name in FILES:
                    atomic_write(temporary / name, private_read(directory / name))
                require(sponsor.read_run(directory)[2] == binding, "SOURCE_CHANGED")
                os.rename(temporary, snapshot)
                sync_directory(directory.parent)
            finally:
                if temporary.exists():
                    shutil.rmtree(temporary)
        private_dir(snapshot)
        saved_state, saved_cp, saved_binding = sponsor.read_run(snapshot)
        # A later verify may observe confirmation. Acceptance's immutable txid
        # and every other proof remain bound to the saved donor evidence.
        saved_artifacts, current_artifacts = deepcopy(saved_cp["artifacts"]), deepcopy(a)
        for artifacts in (saved_artifacts, current_artifacts):
            artifacts.get("public_acceptance", {}).pop("confirmed", None)
        require(saved_binding["state_sha256"] == binding["state_sha256"]
                and saved_binding["descriptor_sha256"] == binding["descriptor_sha256"]
                and saved_state["run_id"] == runner.run_id
                and saved_cp.get("binding") == cp["binding"] and saved_artifacts == current_artifacts
                and set(saved_cp.get("stages", {})) == set(STAGES)
                and all(v.get("status") == "passed" for v in saved_cp["stages"].values()), "SOURCE_CHANGED")
        key = digest({"directory": str(directory), "run_id": runner.run_id,
                      "binding": runner.journal.data["binding"]})
        claim = data["claims"].get(data["runs"].get(key))
        roots = sorted(root_keys(config).intersection(claim["authorization_roots"])) if claim else sorted(root_keys(config))
        add_source(data, a["account_identity"], {"kind": "completed", "entry": {"source_run": str(snapshot)},
                   "source_binding": saved_binding, "authorization_roots": roots})
