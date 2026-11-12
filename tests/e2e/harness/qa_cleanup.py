"""Retire one provenance-bound, completed disposable cloud fixture at capacity.

Never deletes local files, recovery material or coins. Uses the funding registry's
host lock so donor selection and cloud retirement cannot race on this host.
"""
from contextlib import contextmanager
from copy import deepcopy
import fcntl
import os
import stat
import json
from pathlib import Path
import time
import xml.etree.ElementTree as ET
from urllib.error import HTTPError, URLError
from urllib.request import Request, build_opener, ProxyHandler

from . import qa_funding, sponsor
from .clients import EXPLORERS, GENESIS, REST, Native, NoRedirect, http
from .state import Blocked, Failed, digest, private_read


def require(value, code):
    if not value:
        raise Failed("QA_CLEANUP_" + code, "Disposable QA retirement validation failed: " + code)


def options(value):
    require(isinstance(value, dict) and set(value) == {"version", "preserve_runs"}
            and type(value["version"]) is int and value["version"] == 1, "CONFIG")
    require(isinstance(value["preserve_runs"], list) and len(value["preserve_runs"]) <= 64
            and all(isinstance(p, str) and Path(p).is_absolute() for p in value["preserve_runs"]), "CONFIG")
    return deepcopy(value)


def api(runner, path, *, delete=False, body=None, missing=False):
    """Owner-authenticated, no redirects, bounded response, exact API paths only."""
    require(path.startswith(("/connect/cubes", "/keys")), "API_PATH")
    headers = {"Authorization": "Bearer " + runner.account["tokens"]["access_token"]}
    if body is not None:
        headers["Content-Type"] = "application/json"
    request = Request(REST + "/api/v1" + path, headers=headers,
                      data=sponsor.encoded(body) if body is not None else None,
                      method="DELETE" if delete else "GET")
    try:
        with build_opener(ProxyHandler({}), NoRedirect()).open(request, timeout=20) as response:
            raw = response.read(4_000_001)
        require(len(raw) <= 4_000_000, "RESPONSE_SIZE")
        value = json.loads(raw)
        require(value.get("success") is True, "API_RESPONSE")
        return value.get("data")
    except HTTPError as error:
        if missing and not delete and error.code == 404:
            return None  # Only exact IDs from a durable retirement manifest.
        raise Blocked("QA_CLEANUP_HTTP", "The QA retirement API did not confirm the operation.") from None
    except (URLError, OSError, TimeoutError):
        raise Blocked("QA_CLEANUP_UNCERTAIN", "Reconcile the saved QA retirement before any new cleanup.") from None
    except (ValueError, UnicodeError):
        raise Failed("QA_CLEANUP_RESPONSE", "QA retirement returned an invalid response.") from None


def run_identity(directory):
    directory = Path(directory)
    require(directory.is_absolute() and not directory.is_symlink(), "DIRECTORY")
    state, cp, binding = sponsor.read_run(directory)
    a = cp["artifacts"]
    require(state.get("version") == 1 and state.get("registration_started") is True
            and state.get("cube_uuid") == state.get("run_id") == a.get("cube_uuid")
            and state.get("cube_id") == a.get("cube_id")
            and str(state.get("target_key_id")) == a.get("target_key_id")
            and state.get("target_fingerprint") == a.get("target_fingerprint"), "PROVENANCE")
    require(type(state["cube_id"]) is int and state["cube_id"] > 0
            and str(state["target_key_id"]).isdigit() and int(state["target_key_id"]) > 0, "PROVENANCE")
    return state, cp, binding


LOCK_FILES = {"run.lock", "provision.lock", "profile/.device-secret.lock"}


def retained_tree(directory, *, pending_results=False):
    """Exact owned recovery/history tree; ignore only known synchronization locks.

    The live checkpoint legitimately gains confirmation/timestamps on re-verify;
    its complete immutable counterpart is retained in the source snapshot.
    """
    directory = Path(directory)
    require(directory.is_absolute() and not directory.is_symlink(), "DIRECTORY")
    result = {}
    paths = []
    for root, directories, files in os.walk(directory, followlinks=False):
        root = Path(root)
        if root == directory / "qa-build":
            # Reproducible compiler/source products, not recovery/history.
            caches = {"source", "derived", "Runner.xcarchive", "export", "validated"}
            directories[:] = [name for name in directories if name not in caches]
        paths.extend(root / name for name in directories + files)
    for path in sorted(paths):
        relative = path.relative_to(directory).as_posix()
        info = path.lstat()
        require(not stat.S_ISLNK(info.st_mode) and info.st_uid == os.getuid(), "HISTORY_FILE")
        if stat.S_ISDIR(info.st_mode):
            require(not info.st_mode & 0o077, "HISTORY_DIRECTORY")
            continue
        require(stat.S_ISREG(info.st_mode), "HISTORY_FILE")
        if (relative in LOCK_FILES or relative == "checkpoint.json"
                or (pending_results and relative == "results.xml")
                or (path.parent == directory / "profile" and path.name.startswith(".testnet4-secret-")
                    and path.name.endswith(".lock"))
                or relative == "profile/testnet4/settings.lock"):
            continue
        result[relative] = sponsor.sha(private_read(path))
    require(result, "HISTORY_EMPTY")
    return result


@contextmanager
def candidate_lease(record):
    directory = Path(record["directory"])
    require(directory.is_absolute() and not directory.is_symlink(), "DIRECTORY")
    try:
        fd = os.open(directory / "run.lock", os.O_RDWR | os.O_NOFOLLOW)
    except OSError:
        raise Blocked("QA_CLEANUP_RUN_LOCK", "The completed QA run's existing lease is unavailable.") from None
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077, "RUN_LOCK")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Blocked("QA_CLEANUP_RUN_BUSY", "The completed QA run is still owned by another process.") from None
        yield
    finally:
        os.close(fd)


def verify_local(record):
    from .provisioning import STAGES
    state, cp, binding = run_identity(record["snapshot"])
    require(set(cp.get("stages", {})) == set(STAGES)
            and all(v.get("status") == "passed" for v in cp["stages"].values()), "COMPLETION")
    require(binding == record["source_binding"] and cp["artifacts"]["account_identity"] == record["account"]
            and state["owner_user_id"] == record["owner"]
            and state["run_id"] == record["run_id"]
            and state["cube_id"] == record["cube_id"]
            and state["vault_id"] == record["vault_id"]
            and int(state["target_key_id"]) == record["key_id"]
            and state["target_fingerprint"] == record["fingerprint"], "PROVENANCE")
    directory = Path(record["directory"])
    require(retained_tree(directory, pending_results=record.get("sealed") is not True) == record["retained_files"], "RECOVERY_CHANGED")
    require(Path(state["profile_dir"]).resolve() == (directory / "profile").resolve(), "PROFILE")
    return state, cp


def backup_check(runner, record):
    from .provisioning import Provision
    state, _ = verify_local(record)
    reply = Provision(runner.provision.binary, Path(state["profile_dir"]),
                      Path(record["directory"]), state["run_id"]).call("backup_check")
    require(reply.get("backup_verified") is True and reply.get("run_id") == record["run_id"], "BACKUP")


def pending(runner, record):
    state, cp = verify_local(record)
    a = cp["artifacts"]
    fixture = {"profile_dir": str(runner.profile), "psbt_file": str(Path(record["directory"]) / "local-signed.psbt"),
               "descriptor_file": str(Path(record["directory"]) / "descriptor.txt"),
               "expected_txid": state["spend"]["txid"], "cube_id": state["cube_id"],
               "vault_id": str(state["vault_id"]), "target_key_id": str(state["target_key_id"]),
               "target_fingerprint": state["target_fingerprint"], "request_id": a["request_id"]}
    reply = Native(runner.signing_binary, fixture).call("cleanup_audit")
    require(reply.get("owner_user_id") == record["owner"] == runner.account_user_id
            and isinstance(reply.get("sessions"), list) and isinstance(reply.get("decrypt_cubes"), list), "ACCOUNT")
    require(all(isinstance(s, dict) and isinstance(s.get("vault_id"), str)
                and isinstance(s.get("key_ids"), list) for s in reply["sessions"]), "PENDING")
    require(not any(s["vault_id"] == str(record["vault_id"]) or str(record["key_id"]) in s["key_ids"]
                    for s in reply["sessions"])
            and str(record["cube_id"]) not in reply["decrypt_cubes"]
            and record["run_id"] not in reply["decrypt_cubes"], "ACTIVE_REQUEST")


def residual(record):
    """Require both explorers to agree on only the known completed change.

    This intentionally does not sweep it. Any unexpected receipt quarantines
    the fixture for explicit investigation instead of guessing its ownership.
    """
    state, cp = verify_local(record)
    spend = state["spend"]
    raw = private_read(Path(record["directory"]) / "signed.hex").decode().strip()
    require(sponsor.transaction(raw)["txid"] == spend["txid"] == cp["artifacts"]["broadcast_txid"], "TRANSACTION")
    values = []
    expected = (spend["txid"], 1, spend["change_sats"])
    for endpoint in EXPLORERS:
        require(http(endpoint + "/block-height/0").decode().strip() == GENESIS, "CHAIN")
        require(http(endpoint + "/tx/" + spend["txid"] + "/hex").decode().strip() == raw, "TRANSACTION")
        points = []
        for address in (cp["artifacts"]["receive_address"], spend["change_address"]):
            rows = json.loads(http(endpoint + "/address/" + address + "/utxo"))
            require(isinstance(rows, list), "BALANCE")
            points += [(p["txid"], p["vout"], p["value"]) for p in rows]
        require(not points or points == [expected], "UNEXPECTED_FUNDS")
        outspend = json.loads(http(endpoint + "/tx/" + spend["txid"] + "/outspend/1"))
        require(type(outspend.get("spent")) is bool
                and outspend["spent"] == (not points), "BALANCE_INDEX")
        values.append(sum(p[2] for p in points))
    require(values[0] == values[1], "EXPLORER_DISAGREEMENT")
    return values[0]


def protected_ids(runner, data):
    """Cloud protection uses exact IDs; labels and run UUIDs never grant deletion."""
    funding = qa_funding.options(runner.config.get("qa_funding", {"version": 1, "sources": []}))
    protected = set()
    paths = [e["source_run"] for e in funding["sources"]] + options(runner.config["qa_cleanup"])["preserve_runs"]
    for path in paths:
        state, _, _ = sponsor.read_run(Path(path))
        protected.add(state["cube_id"])
    # An unfinished or externally superseded funding claim keeps both ends.
    for claim in data["claims"].values():
        if claim.get("phase") != "accepted":
            state, _, _ = sponsor.read_run(Path(claim["entry"]["source_run"]))
            protected.add(state["cube_id"])
            protected.add(claim["identity"]["artifacts"]["cube_id"])
    for outcome in data["external"].values():
        protected.add(outcome["identity"]["artifacts"]["cube_id"])
    # Managed donors with usable unspent change retain their cloud fixture.
    for item in data["accounts"].get(runner.artifacts["account_identity"], []):
        if retired(data, item["entry"], item, account=runner.artifacts["account_identity"], owner=runner.account_user_id):
            continue
        source, prior, value, _ = qa_funding.source_metadata(item["entry"], item,
                                    runner.artifacts["account_identity"], runner.account_user_id)
        if value < qa_funding.minimum_amount(runner.config) + funding["fee_sats"] + 1000:
            continue
        state, _, _ = sponsor.read_run(source)
        txid = state["spend"]["txid"]
        if prior:
            txid = json.loads(private_read(prior / "sponsor-receipt.json"))["txid"]
        try:
            sponsor.observe(txid, 1)
        except Failed as error:
            if error.code != "SOURCE_SPENT":
                raise
        else:
            protected.add(state["cube_id"])
    return protected


def retired(data, entry, enrolled=None, *, account=None, owner=None):
    path = str(Path(entry["source_run"]).resolve())
    tombstones = data.get("retired_cloud_sources", {})
    require(isinstance(tombstones, dict), "TOMBSTONE")
    if path not in tombstones:
        return False
    tombstone = tombstones[path]
    require(isinstance(tombstone, dict) and set(tombstone) == {
        "retirement", "run_id", "cube_id", "account", "source_binding", "residual_sats", "local_recovery_retained"}, "TOMBSTONE")
    records = data.get("retirements", {})
    require(isinstance(records, dict) and isinstance(tombstone["retirement"], str), "TOMBSTONE")
    record = records.get(tombstone["retirement"])
    require(isinstance(record, dict) and type(record.get("version")) is int and record["version"] == 1
            and record.get("phase") == "complete" and record.get("sealed") is True and record.get("snapshot") == path
            and tombstone["local_recovery_retained"] is True
            and type(tombstone["cube_id"]) is int and type(tombstone["residual_sats"]) is int
            and tombstone["residual_sats"] >= 0
            and all(tombstone[k] == record.get(k) for k in ("run_id", "cube_id", "account", "source_binding", "residual_sats")), "TOMBSTONE")
    require((account is None or record["account"] == account)
            and (owner is None or record["owner"] == owner), "TOMBSTONE")
    verify_local(record)
    if enrolled is not None:
        require(enrolled.get("source_binding") == record["source_binding"], "TOMBSTONE")
        _, _, _, binding = qa_funding.source_metadata(entry, enrolled, record["account"], record["owner"])
        require(binding == record["source_binding"], "TOMBSTONE")
    return True


def graph(runner, record):
    base = "/connect/cubes/" + str(record["cube_id"])
    cube = api(runner, base, missing=True)
    vault = api(runner, base + "/vault", missing=True)
    cube_keys = api(runner, base + "/keys", missing=True)
    require((cube is None and cube_keys is None) or isinstance(cube_keys, list), "KEY_SCHEMA")
    key_result = api(runner, "/keys")
    require(isinstance(key_result, dict) and isinstance(key_result.get("keys"), list), "KEY_SCHEMA")
    keys = key_result["keys"]
    matches = [k for k in keys if k.get("id") == record["key_id"]]
    require(len(matches) <= 1, "KEY_GRAPH")
    key = matches[0] if matches else None
    if key is not None:
        require(key.get("cubeId") == record["cube_id"] and key.get("keychainId") == record["keychain_id"]
                and key.get("network") == "testnet4" and key.get("fingerprint") == record["fingerprint"]
                and key.get("status") == "active" and key.get("vaultId") in (None, record["vault_id"]), "KEY_GRAPH")
    # Scan the owner-visible Cube graph for shared/reassigned key references.
    cubes = api(runner, "/connect/cubes")
    require(isinstance(cubes, list), "CUBE_GRAPH")
    for other in cubes:
        if other.get("id") != record["cube_id"]:
            if other.get("hasVault") and not other.get("vault"):
                other = api(runner, "/connect/cubes/" + str(other["id"]))
            require(not any(k.get("id") == record["key_id"] for k in other.get("keys", []))
                    and not any(m.get("keyId") == record["key_id"] for m in (other.get("vault") or {}).get("members", [])), "SHARED_KEY")
    if cube is not None:
        require(isinstance(cube, dict) and all(isinstance(cube.get(k), list) for k in ("members", "pendingInvites"))
                and (isinstance(cube.get("keys"), list) or ("keys" not in cube and cube_keys == []))
                and "vault" in cube, "CUBE_SCHEMA")
        require(cube.get("id") == record["cube_id"] and cube.get("userId") == record["owner"]
                and cube.get("uuid") == record["run_id"] and cube.get("name") == record["cube_name"]
                and cube.get("network") == "testnet4" and cube.get("status") == "active"
                and cube.get("encryptionPubkey") == record["encryption_pubkey"]
                and not cube.get("members") and not cube.get("pendingInvites")
                and cube.get("lightningAddress") is None and cube.get("sparkStableBalance") is None
                and cube.get("hasRecoveryKit") is False
                and all(k.get("id") == record["key_id"] and k.get("primaryOwnerId") == record["owner"]
                        for k in cube.get("keys", [])), "CUBE_GRAPH")
        require(len(cube_keys) == len(cube.get("keys", [])) == (1 if key is not None else 0)
                and all(k.get("id") == record["key_id"] for k in cube_keys), "KEY_GRAPH")
        embedded = cube.get("vault")
        require((embedded is None) == (vault is None)
                and (embedded is None or embedded.get("id") == record["vault_id"]), "VAULT_GRAPH")
    if vault is not None:
        require(isinstance(vault, dict) and isinstance(vault.get("members"), list), "VAULT_SCHEMA")
        require(cube is not None and vault.get("id") == record["vault_id"]
                and vault.get("cubeId") == record["cube_id"] and vault.get("fingerprint") == record["descriptor_id"]
                and vault.get("status") == "active" and len(vault.get("members", [])) == 1
                and vault["members"][0].get("keyId") == record["key_id"]
                and vault["members"][0].get("role") == "keyholder", "VAULT_GRAPH")
    return cube, vault, key


def register_completed(runner):
    """Only a newly verified strict run can auto-enroll; never discover by label."""
    from .provisioning import STAGES
    require(runner.strict is not None and not runner.stop, "MODE")
    directory = runner.journal.directory.resolve()
    qa_funding.register_completed(runner, cleanup_only=True)
    state, cp, _ = run_identity(directory)
    require(state["run_id"] == runner.run_id and cp["binding"] == digest(runner.config)
            and set(cp["stages"]) == set(STAGES)
            and all(s.get("status") == "passed" for s in cp["stages"].values()), "COMPLETION")
    a = cp["artifacts"]
    require(a.get("signature_count_min", 0) >= 2 and a.get("strict_pin_input", {}).get("completed") is True
            and a.get("strict_home_refresh", {}).get("home_refresh_proven") is True, "COMPLETION")
    runner.strict.require_completion(a["request_id"], a["session_id"])
    snapshot = directory.with_name(directory.name + "-completed-source")
    saved, saved_cp, binding = run_identity(snapshot)
    require(saved == state and saved_cp["binding"] == cp["binding"], "COMPLETION")
    with qa_funding.pool() as data:
        records = data.setdefault("retirements", {})
        require(isinstance(records, dict) and len(records) < 1024, "REGISTRY")
        key = digest({"account": a["account_identity"], "run_id": runner.run_id, "directory": str(directory)})
        record = {"version": 1, "account": a["account_identity"], "owner": state["owner_user_id"],
                  "run_id": state["run_id"], "directory": str(directory), "snapshot": str(snapshot),
                  "source_binding": binding, "cube_id": state["cube_id"], "cube_name": state["cube_name"],
                  "vault_id": state["vault_id"], "descriptor_id": state["descriptor_id"],
                  "key_id": int(state["target_key_id"]), "fingerprint": state["target_fingerprint"],
                  "keychain_id": None, "phase": "enrolled", "sealed": False,
                  "retained_files": retained_tree(directory, pending_results=True)}
        keys = api(runner, "/keys")["keys"]
        match = [k for k in keys if k.get("id") == record["key_id"]]
        require(len(match) == 1 and type(match[0].get("keychainId")) is int, "KEY_GRAPH")
        record["keychain_id"] = match[0]["keychainId"]
        cube = api(runner, "/connect/cubes/" + str(record["cube_id"]))
        public_key = cube.get("encryptionPubkey")
        require(isinstance(public_key, str) and len(public_key) == 66 and public_key[:2] in ("02", "03")
                and all(c in "0123456789abcdef" for c in public_key), "CUBE_GRAPH")
        record["encryption_pubkey"] = public_key
        graph(runner, record)
        backup_check(runner, record)
        if key in records:
            previous = deepcopy(records[key])
            previous["phase"] = "enrolled"
            previous["sealed"] = False
            previous["retained_files"].pop("results.xml", None)
            previous.pop("residual_sats", None)
            previous.pop("updated_at", None)
            previous.pop("consumer", None)
            require(previous == record, "ENROLLMENT_CHANGED")
            return
        records[key] = record
        qa_funding.save_pool(data)


def ensure_capacity(runner):
    require(runner.strict is not None and not runner.strict.requested, "MODE")
    require(not any(k in runner.artifacts for k in ("request_id", "session_id", "approval_started", "pin_dispatch_started")), "ACTIVE_RUN")
    account = runner.artifacts["account_identity"]
    consumer = digest({"run_id": runner.run_id, "binding": runner.journal.data["binding"]})
    with qa_funding.pool() as data:
        records = data.setdefault("retirements", {})
        require(isinstance(records, dict), "REGISTRY")
        # Resume our one durable selection, including a lost successful DELETE.
        own = [(k, r) for k, r in records.items() if r.get("consumer") == consumer]
        require(len(own) <= 1, "REGISTRY")
        require(not any(r.get("account") == account and r.get("phase") not in ("enrolled", "complete")
                        and r.get("consumer") != consumer for r in records.values()), "RETIREMENT_BUSY")
        if own:
            key, record = own[0]
        else:
            protections = protected_ids(runner, data)
            candidates = [(k, r) for k, r in records.items()
                          if r.get("phase") == "enrolled" and type(r.get("sealed")) is bool and r.get("account") == account
                          and r.get("owner") == runner.account_user_id and r.get("cube_id") not in protections
                          and r.get("run_id") != runner.run_id]
            if not candidates:
                raise Blocked("QA_CLEANUP_NO_CANDIDATE", "No safely enrolled disposable QA fixture can free a signer slot.")
            key, record = sorted(candidates, key=lambda item: item[0])[0]
        require(record.get("account") == account and record.get("owner") == runner.account_user_id, "ACCOUNT")
        if record["phase"] == "complete":
            retired(data, {"source_run": record["snapshot"]})
            return  # One fixture maximum per consumer, even if another remains full.
        with candidate_lease(record):
            if record.get("sealed") is not True:
                backup_check(runner, record)
                seal_record(record)
                qa_funding.save_pool(data)
            def guard():
                runner.strict.observe()
                require(not runner.strict.requested and record["cube_id"] not in protected_ids(runner, data), "PROTECTED")
                verify_local(record)
                backup_check(runner, record)
                pending(runner, record)
                amount = residual(record)
                funding = qa_funding.options(runner.config.get("qa_funding", {"version": 1, "sources": []}))
                require(amount < qa_funding.minimum_amount(runner.config) + funding["fee_sats"] + 1000, "FUNDING_SOURCE")
                if "residual_sats" in record:
                    require(amount == record["residual_sats"], "BALANCE_CHANGED")
                else:
                    record["residual_sats"] = amount
                return graph(runner, record)
            cube, vault, phone_key = guard()
            if record["phase"] == "enrolled":
                require(cube is not None and vault is not None and phone_key is not None, "MISSING_FIXTURE")
                record.update(consumer=consumer, phase="vault_intent", updated_at=int(time.time()))
                qa_funding.save_pool(data)
            base = "/connect/cubes/" + str(record["cube_id"])
            steps = (("vault_intent", "key_intent", base + "/vault", None, 1),
                     ("key_intent", "cube_intent", "/keys/delete", {"keyId": record["key_id"], "keychainId": record["keychain_id"]}, 2),
                     ("cube_intent", "complete", base, None, 0))
            require(record["phase"] in {s[0] for s in steps}, "PHASE")
            for phase, after, path, body, index in steps:
                if record["phase"] != phase:
                    continue
                resources = guard()
                if phase != "vault_intent":
                    require(resources[1] is None, "VAULT_REAPPEARED")
                if phase == "cube_intent":
                    require(resources[2] is None, "KEY_REAPPEARED")
                if resources[index] is not None:
                    # Intent already fsynced. An uncertain transport stays here.
                    api(runner, path, delete=True, body=body)
                require(graph(runner, record)[index] is None, "DELETE_UNCONFIRMED")
                record.update(phase=after, updated_at=int(time.time()))
                if after == "complete":
                    data.setdefault("retired_cloud_sources", {})[record["snapshot"]] = {
                        "retirement": key, "run_id": record["run_id"], "cube_id": record["cube_id"],
                        "account": record["account"], "source_binding": record["source_binding"],
                        "residual_sats": record["residual_sats"], "local_recovery_retained": True}
                qa_funding.save_pool(data)

        runner.journal.artifact("qa_cleanup", {"retirement": key, "cube_id": record["cube_id"],
            "vault_id": record["vault_id"], "key_id": record["key_id"], "residual_sats": record["residual_sats"],
            "local_recovery_retained": True, "phase": record["phase"]})


def reconcile_capacity(runner):
    """Finish our prior exact retirement even after its key freed capacity."""
    consumer = digest({"run_id": runner.run_id, "binding": runner.journal.data["binding"]})
    with qa_funding.pool() as data:
        records = data.get("retirements", {})
        require(isinstance(records, dict), "REGISTRY")
        pending = [r for r in records.values() if r.get("consumer") == consumer
                   and r.get("phase") != "complete"]
        require(len(pending) <= 1, "REGISTRY")
    if pending:
        ensure_capacity(runner)
        return True
    return False


def seal_record(record):
    from .provisioning import STAGES
    verify_local(record)
    if record.get("sealed") is True:
        return
    try:
        results = ET.fromstring(private_read(Path(record["directory"]) / "results.xml"))
    except ET.ParseError:
        raise Failed("QA_CLEANUP_RESULTS", "The completed QA JUnit evidence is invalid.") from None
    cases = results.findall(".//testcase")
    stages = []
    require(len(cases) == len(STAGES), "RESULTS")
    for case in cases:
        properties = {p.get("name"): p.get("value") for p in case.findall("./properties/property")}
        require(properties.get("outcome") == "passed" and properties.get("stage") in STAGES
                and not any(case.find(kind) is not None for kind in ("error", "failure", "skipped")), "RESULTS")
        stages.append(properties["stage"])
    require(set(stages) == set(STAGES), "RESULTS")
    record["retained_files"] = retained_tree(Path(record["directory"]))
    record["sealed"] = True


def seal_completed(runner):
    """Called after final JUnit serialization; no phone interaction is performed."""
    key = digest({"account": runner.artifacts["account_identity"], "run_id": runner.run_id,
                  "directory": str(runner.journal.directory.resolve())})
    with qa_funding.pool() as data:
        record = data.get("retirements", {}).get(key)
        require(isinstance(record, dict) and record.get("phase") == "enrolled"
                and record.get("run_id") == runner.run_id and record.get("owner") == runner.account_user_id
                and record.get("account") == runner.artifacts["account_identity"]
                and record.get("consumer") is None, "COMPLETION")
        with candidate_lease(record):
            verify_local(record)
            seal_record(record)
            qa_funding.save_pool(data)
