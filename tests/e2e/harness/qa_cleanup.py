"""Retire one provenance-bound, completed disposable cloud fixture at capacity.

Never deletes local files, recovery material or coins. Uses the funding registry's
host lock so donor selection and cloud retirement cannot race on this host.
"""
from copy import deepcopy
import json
from pathlib import Path
import time
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


def verify_local(record):
    state, cp, binding = run_identity(record["snapshot"])
    require(binding == record["source_binding"] and cp["artifacts"]["account_identity"] == record["account"]
            and state["owner_user_id"] == record["owner"]
            and state["run_id"] == record["run_id"]
            and state["cube_id"] == record["cube_id"]
            and state["vault_id"] == record["vault_id"]
            and int(state["target_key_id"]) == record["key_id"]
            and state["target_fingerprint"] == record["fingerprint"], "PROVENANCE")
    directory = Path(record["directory"])
    for name, sha in record["retained_files"].items():
        require(sponsor.sha(private_read(directory / name)) == sha, "RECOVERY_CHANGED")
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
        if retired(data, item["entry"]):
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


def retired(data, entry):
    return str(Path(entry["source_run"]).resolve()) in data.get("retired_cloud_sources", {})


def graph(runner, record):
    base = "/connect/cubes/" + str(record["cube_id"])
    cube = api(runner, base, missing=True)
    vault = api(runner, base + "/vault", missing=True)
    keys = api(runner, "/keys")["keys"]
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
        require(cube.get("id") == record["cube_id"] and cube.get("userId") == record["owner"]
                and cube.get("uuid") == record["run_id"] and cube.get("name") == record["cube_name"]
                and cube.get("network") == "testnet4" and cube.get("status") == "active"
                and cube.get("encryptionPubkey") == record["encryption_pubkey"]
                and not cube.get("members") and not cube.get("pendingInvites")
                and cube.get("lightningAddress") is None and cube.get("sparkStableBalance") is None
                and cube.get("hasRecoveryKit") is False
                and all(k.get("id") == record["key_id"] and k.get("primaryOwnerId") == record["owner"]
                        for k in cube.get("keys", [])), "CUBE_GRAPH")
        require(len(cube.get("keys", [])) == (1 if key is not None else 0), "KEY_GRAPH")
        embedded = cube.get("vault")
        require((embedded is None) == (vault is None)
                and (embedded is None or embedded.get("id") == record["vault_id"]), "VAULT_GRAPH")
    if vault is not None:
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
                  "keychain_id": None, "phase": "enrolled", "retained_files": {name: sponsor.sha(private_read(directory / name))
                      for name in ("provision.json", "descriptor.txt", "signed.hex", "local-signed.psbt", "provision-secrets.keychain.json")}}
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
                          if r.get("phase") == "enrolled" and r.get("account") == account
                          and r.get("owner") == runner.account_user_id and r.get("cube_id") not in protections
                          and r.get("run_id") != runner.run_id]
            if not candidates:
                raise Blocked("QA_CLEANUP_NO_CANDIDATE", "No safely enrolled disposable QA fixture can free a signer slot.")
            key, record = sorted(candidates, key=lambda item: item[0])[0]
        require(record.get("account") == account and record.get("owner") == runner.account_user_id, "ACCOUNT")
        if record["phase"] == "complete":
            return  # One fixture maximum per consumer, even if another remains full.
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
                    "residual_sats": record["residual_sats"], "local_recovery_retained": True}
            qa_funding.save_pool(data)
        runner.journal.artifact("qa_cleanup", {"retirement": key, "cube_id": record["cube_id"],
            "vault_id": record["vault_id"], "key_id": record["key_id"], "residual_sats": record["residual_sats"],
            "local_recovery_retained": True, "phase": record["phase"]})
