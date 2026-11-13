"""Cancel one bound, unsigned QA request and quarantine its uncertain PIN attempt.

Never signs, broadcasts, resets strict proof, or guesses/retries a PIN. All output
is separate from the immutable failed run. The encrypted audit explicitly keeps
local PIN input unresolved even after remote cancellation is proved.
"""
import argparse
from contextlib import contextmanager
from copy import deepcopy
import fcntl
import json
import os
from pathlib import Path
import stat
import uuid

from .clients import Native
from .provisioning import Provision, phone_state_root, validate_config
from .qa_build import configure_fresh
from .secrets import SecretStore
from .sponsor import binary_digest, encoded, output_lock, read_run, sha
from .state import Blocked, Failed, atomic_write, digest, private_read


def require(condition, code):
    if not condition:
        raise Failed(code, "Signing abort validation failed: " + code)


@contextmanager
def source_lock(directory):
    # Do not create, truncate, or write anything in the original run.
    fd = os.open(directory / "run.lock", os.O_RDONLY | os.O_NOFOLLOW)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid()
                and not info.st_mode & 0o077, "SOURCE_LOCK")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Blocked("SOURCE_BUSY", "The original runner must release its run lock.") from None
        yield
    finally:
        os.close(fd)


def bind_source(directory, config):
    state, checkpoint, snapshot = read_run(directory)
    artifacts = checkpoint["artifacts"]
    require(checkpoint["binding"] == digest(config), "CONFIG_CHANGED")
    run_id = str(uuid.uuid5(uuid.NAMESPACE_URL, "coincube-fresh-testnet4/" + str(directory)))
    require(state["run_id"] == run_id and Path(state["profile_dir"]) == directory / "profile", "RUN_BINDING")
    require(artifacts["account_identity"] == digest({"email": config["account_email"].casefold(),
                                                    "user_id": state["owner_user_id"]}), "ACCOUNT_CHANGED")
    names = ("account_identity", "desktop_identity", "request_id", "session_id", "intent", "psbt_sha256",
             "descriptor_id", "target_key_id", "target_fingerprint", "cube_id", "cube_uuid", "vault_id", "fee_sats")
    binding = artifacts["approval_binding"]
    require(isinstance(binding, dict) and isinstance(binding.get("device"), str)
            and len(binding["device"]) == 64, "APPROVAL_BINDING")
    expected = {**{name: artifacts[name] for name in names}, "run_id": run_id,
                "journal_binding": checkpoint["binding"], "device": binding["device"]}
    require(binding == expected and isinstance(artifacts.get("pin_dispatch_started"), dict)
            and "strict_pin_input" not in artifacts and "broadcast_txid" not in artifacts
            and "public_acceptance" not in artifacts, "ATTEMPT_BINDING")
    context = artifacts["approval_context"]
    require(artifacts["pin_dispatch_started"].get("context") == context
            and context.get("binding") == binding and context.get("request_id") == binding["request_id"]
            and context.get("session_id") == binding["session_id"] and context.get("device") == binding["device"]
            and context.get("intent_sha256") == digest(binding["intent"])
            and context.get("fee_sats") == binding["fee_sats"], "APPROVAL_CONTEXT")
    for key in ("cube_id", "cube_uuid", "target_key_id", "target_fingerprint", "descriptor_id"):
        require(state[key] == artifacts[key], "PROVISION_BINDING")
    require(state["spend"]["txid"] == binding["intent"]["txid"]
            and binding["request_id"] == str(uuid.uuid5(uuid.UUID(run_id), state["spend"]["txid"]))
            and state["spend"]["fee_sats"] == binding["fee_sats"], "TRANSACTION_BINDING")
    require(Path(artifacts["descriptor_file"]) == directory / "descriptor.txt", "DESCRIPTOR_PATH")
    psbt = directory / "local-signed.psbt"
    require(sha(private_read(psbt)) == binding["psbt_sha256"], "PSBT_CHANGED")
    snapshot["psbt_sha256"] = binding["psbt_sha256"]
    fixture = {name: artifacts[name] for name in ("cube_id", "vault_id", "target_key_id", "target_fingerprint", "descriptor_file")}
    fixture.update(profile_dir=str(directory / "profile"), psbt_file=str(psbt), expected_txid=binding["intent"]["txid"])
    return state, binding, snapshot, fixture


def credential_valid(state, identity):
    require(isinstance(state, dict) and state.get("identity") == identity
            and state.get("verified") is True, "CREDENTIAL_IDENTITY")
    reservation = state.get("key_generation_reservation")
    require(reservation is None or (isinstance(reservation, dict) and reservation.get("status") == "completed"),
            "KEY_GENERATION_PENDING")


def validate_terminal(reply, intent):
    binding = intent["binding"]
    require(reply.get("ok") is True and reply.get("action") == "cancel"
            and reply.get("network") == "testnet4" and reply.get("cancelled") is True
            and type(reply.get("status")) is int and reply["status"] == 8
            and type(reply.get("submitted_signature_count")) is int and reply["submitted_signature_count"] == 0
            and reply.get("ready") is False and reply.get("signature_count_min") == 1
            and reply.get("owner_user_id") == intent["owner_user_id"]
            and reply.get("txid") == binding["intent"]["txid"]
            and reply.get("fee_sats") == binding["fee_sats"], "TERMINAL_RECEIPT")
    for field in ("request_id", "session_id", "descriptor_id", "target_key_id", "target_fingerprint"):
        require(reply.get(field) == binding[field], "TERMINAL_BINDING")
    require(digest(reply.get("creator_device_id")) == binding["desktop_identity"]
            and isinstance(reply.get("target_device_id"), str) and bool(reply["target_device_id"]), "TERMINAL_DEVICE")


def quarantine(store, intent, receipt):
    """CAS only the original revision; exact replay cannot consume a newer attempt."""
    validate_terminal(receipt["terminal"], intent)
    require(receipt.get("intent_sha256") == digest(intent) and receipt.get("phase") == "terminal_verified", "RECEIPT_INTENT")
    audit = {"intent_sha256": digest(intent), "terminal_receipt_sha256": digest(receipt),
             "purpose": intent["purpose"], "session_id": intent["binding"]["session_id"],
             "remote_status": "cancelled", "local_pin_outcome": "unresolved",
             "authorization_ref": intent["authorization_ref"]}
    state = store.load()
    credential_valid(state, intent["credential_identity"])
    history = state.get("cancelled_signing_attempts", [])
    require(isinstance(history, list) and all(isinstance(item, dict) for item in history), "AUDIT_FORMAT")
    matches = [item for item in history if item.get("intent_sha256") == audit["intent_sha256"]]
    if matches:
        require(matches == [audit] and state.get("pending_attempt") != intent["purpose"], "AUDIT_REPLAY_CONFLICT")
        return "already_quarantined"  # In particular, leave any newer pending attempt alone.
    require(store.sha256 == intent["credential_revision"] and state.get("pending_attempt") == intent["purpose"],
            "CREDENTIAL_CAS_CONFLICT")
    updated = deepcopy(state)
    updated["cancelled_signing_attempts"] = history + [audit]
    del updated["pending_attempt"]
    store.save(updated)  # Native compare-and-swap plus encrypted readback verification.
    return "quarantined"


def execute(args):
    for value in (args.source_run, args.config, args.output_dir, args.native_binary, args.signing_binary):
        require(Path(value).is_absolute() and not Path(value).is_symlink(), "ABSOLUTE_PATH")
    require(isinstance(args.authorize_abort, str) and 0 < len(args.authorize_abort.strip()) <= 512, "AUTHORIZATION_REQUIRED")
    # Fresh runs bind the lexical absolute path, including symlinked parents.
    directory, output = Path(args.source_run).absolute(), Path(args.output_dir).resolve()
    config = json.loads(private_read(args.config))
    if "qa_build" in config.get("ios", {}):
        # Use the same effective strict configuration as the fresh launcher.
        # Historical configurations without automatic builds retain their
        # original binding, including explicit non-strict compatibility runs.
        config = configure_fresh(config)
    config = validate_config(config)
    phone_root = phone_state_root(config).resolve()
    with source_lock(directory), output_lock(output, [directory.resolve(), phone_root, Path(config["profile_dir"]).resolve(), Path(args.config).resolve()]):
        state, binding, snapshot, fixture = bind_source(directory, config)
        phone_dir = phone_root / binding["account_identity"]
        locator = phone_dir / "phone-secrets.keychain.json"
        private_read(locator)  # Refuse locator creation during reconciliation.
        secret_id = str(uuid.uuid5(uuid.NAMESPACE_URL, "coincube-phone/" + binding["account_identity"] + "/" + binding["device"]))
        store = SecretStore(Provision(args.native_binary, phone_dir, phone_dir, secret_id), locator)
        credential = store.load()
        identity = binding["account_identity"] + ":" + binding["device"]
        credential_valid(credential, identity)
        intent = {"version": 1, "network": "testnet4", "authorization_ref": args.authorize_abort,
                  "source": snapshot, "binding": binding, "owner_user_id": state["owner_user_id"],
                  "credential_identity": identity, "purpose": "sign:" + binding["request_id"],
                  "native_sha256": binary_digest(args.native_binary), "signing_sha256": binary_digest(args.signing_binary)}
        path = output / "abort-intent.json"
        if path.exists():
            saved = json.loads(private_read(path))
            intent["credential_revision"] = saved["credential_revision"]
            require(intent == saved, "ABORT_INTENT_CHANGED")
        else:
            require(credential.get("pending_attempt") == intent["purpose"] and isinstance(store.sha256, str), "PENDING_ATTEMPT")
            intent["credential_revision"] = store.sha256
            atomic_write(path, encoded(intent))
        # Check before remote mutation; no cancellation on a changed credential.
        audit_exists = any(isinstance(v, dict) and v.get("intent_sha256") == digest(intent)
                           for v in credential.get("cancelled_signing_attempts", []))
        require(audit_exists or (store.sha256 == intent["credential_revision"]
                                and credential.get("pending_attempt") == intent["purpose"]), "CREDENTIAL_CAS_CONFLICT")
        require(bind_source(directory, config)[2] == snapshot, "SOURCE_CHANGED")
        require(binary_digest(args.native_binary) == intent["native_sha256"]
                and binary_digest(args.signing_binary) == intent["signing_sha256"], "NATIVE_CHANGED")
        reply = Native(args.signing_binary, fixture).call("cancel", request_id=binding["request_id"],
            session_id=binding["session_id"], authorize_abort=args.authorize_abort,
            abort_binding={"owner_user_id": state["owner_user_id"], "creator_identity": binding["desktop_identity"],
                           "psbt_sha256": binding["psbt_sha256"]})
        validate_terminal(reply, intent)
        receipt = {"version": 1, "phase": "terminal_verified", "intent_sha256": digest(intent), "terminal": reply}
        receipt_path = output / "abort-terminal.json"
        if receipt_path.exists():
            require(json.loads(private_read(receipt_path)) == receipt, "TERMINAL_CHANGED")
        else:
            atomic_write(receipt_path, encoded(receipt))
        require(json.loads(private_read(receipt_path)) == receipt, "TERMINAL_READBACK")
        require(bind_source(directory, config)[2] == snapshot, "SOURCE_CHANGED")
        require(binary_digest(args.native_binary) == intent["native_sha256"]
                and binary_digest(args.signing_binary) == intent["signing_sha256"], "NATIVE_CHANGED")
        result = quarantine(store, intent, receipt)
        atomic_write(output / "abort-result.json", encoded({"phase": result, "intent_sha256": digest(intent),
                     "terminal_receipt_sha256": digest(receipt), "original_run_preserved": True}))
        return {"ok": True, "phase": result, "network": "testnet4"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source-run", "config", "output-dir", "native-binary", "signing-binary", "authorize-abort"):
        parser.add_argument("--" + name, required=True)
    try:
        print(json.dumps(execute(parser.parse_args()), sort_keys=True))
        return 0
    except Blocked as error:
        print(json.dumps({"ok": False, "code": error.code}))
        return 2
    except (OSError, ValueError, TypeError, KeyError):
        print(json.dumps({"ok": False, "code": "ABORT_INPUT_OR_STATE"}))
        return 2
