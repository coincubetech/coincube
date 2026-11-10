"""Fresh production-Connect/Testnet4 scenario. No vault or PSBT input fixture."""
from contextlib import ExitStack
from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, build_opener, ProxyHandler
import uuid

from .clients import EXPLORERS, GENESIS, GRPC, REST, Native, NoRedirect, http, public_transaction
from .onboarding import Onboarding
from .phone import attach
from .runner import Runner
from .secrets import SecretStore
from .state import Blocked, Failed, Journal, atomic_write, digest, private_read

STAGES = ("environment", "authentication", "phone_setup", "cube_creation", "phone_key", "vault_creation",
          "funding", "prepare", "connect", "phone", "request", "sign", "collect", "broadcast", "verify")
FAUCET = "https://cypherfaucet.com/api/v1/claim"


def validate_config(config):
    required = {"version", "account_email", "profile_dir", "connect_rest", "connect_grpc", "ios", "amount_sats", "fee_sats"}
    optional = {"native_binary", "signing_binary", "phone_state_dir", "desktop_session_namespace"}
    if not required <= set(config) or set(config) - required - optional or config["version"] != 2:
        raise Failed("CONFIG_SCHEMA", "Fresh-run configuration must use schema version 2 without vault or PSBT fixtures.")
    if config["connect_rest"] != REST or config["connect_grpc"] != GRPC:
        raise Failed("CONNECT_ENVIRONMENT", "Fresh acceptance requires production Connect endpoints.")
    if not isinstance(config["account_email"], str) or not re.fullmatch(r"[^\s@]+@[^\s@]+\.[^\s@]+", config["account_email"]):
        raise Failed("CONNECT_ACCOUNT", "Specify the exact signed-in disposable QA account email.")
    if config.get("desktop_session_namespace", "development") not in ("development", "production"):
        raise Failed("CONNECT_SESSION_NAMESPACE", "Desktop session namespace must identify the development or production app.")
    for field in ("profile_dir", "native_binary", "signing_binary", "phone_state_dir"):
        if field in config and (not isinstance(config[field], str) or not Path(config[field]).is_absolute()):
            raise Failed("ABSOLUTE_PATH", "Workstation paths must be absolute.")
    for field, minimum, limit in (("amount_sats", 1_000, 100_000), ("fee_sats", 546, 10_000)):
        if type(config[field]) is not int or not minimum <= config[field] <= limit:
            raise Failed("SPEND_LIMIT", "Fresh-run amount/fee must fit the bounded Testnet4 scenario.")
    ios = config["ios"]
    if not {"minimum_build"} <= set(ios) or set(ios) - {"minimum_build", "appium", "xctestrun"}:
        raise Failed("IOS_CONFIG", "Fresh phone configuration accepts only minimum_build, appium and xctestrun.")
    if type(ios["minimum_build"]) is not int or ios["minimum_build"] < 35:
        raise Failed("IOS_BUILD", "A Keychain build containing #194 is required.")
    return config


def copy_auth(source, destination):
    """App-created caches may be 644. Copy only owned, non-writable-by-others files."""
    for name in ("connect.json", "connect_transport_key"):
        path = Path(source) / "testnet4" / name
        try:
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        except OSError:
            raise Blocked("CONNECT_AUTH", "The signed-in desktop account and its existing transport key are required.") from None
        with os.fdopen(fd, "rb") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
                raise Failed("AUTH_SOURCE", "The source account cache must be an owned, non-writable-by-others regular file.")
            content = stream.read(4_000_001)
        if len(content) > 4_000_000:
            raise Failed("AUTH_SIZE", "Account cache exceeds its size limit.")
        target = destination / "testnet4" / name
        target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        if name == "connect_transport_key" and target.exists() and private_read(target) != content:
            raise Failed("TRANSPORT_CHANGED", "The source transport key changed during this run.")
        atomic_write(target, content)


class Provision:
    def __init__(self, binary, profile, directory, run_id):
        self.binary = binary
        self.base = {"network": "testnet4", "profile_dir": str(profile),
                     "run_dir": str(directory), "run_id": run_id}

    def call(self, action, **fields):
        try:
            result = subprocess.run([self.binary], input=json.dumps(dict(self.base, action=action, **fields)).encode(),
                                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=180)
        except (OSError, subprocess.TimeoutExpired):
            raise Blocked("PROVISION_RUNNER", "Native provisioning did not finish; repeat the same run to reconcile its durable identity.") from None
        try:
            reply = json.loads(result.stdout)
        except (ValueError, UnicodeError):
            raise Failed("PROVISION_PROTOCOL", "Native provisioning returned an invalid response.") from None
        if result.returncode or reply.get("ok") is not True:
            code = reply.get("code", "provision_failed")
            if not isinstance(code, str) or not re.fullmatch(r"[a-z0-9_]{1,80}", code):
                code = "provision_failed"
            unavailable = {"connect_auth_expired", "connect_auth_required", "connect_unavailable",
                           "connect_request_refused", "cube_limit_reached", "run_busy", "run_lock_unavailable",
                           "provision_timeout", "runtime_unavailable", "private_file_unavailable",
                           "keychain_unavailable", "secret_busy", "secret_lock_unavailable",
                           "secret_profile_unavailable", "macos_keychain_required",
                           "desktop_session_unavailable", "desktop_session_incomplete", "device_registration_uncertain"}
            error = Blocked if code in unavailable else Failed
            raise error("PROVISION_" + code.upper(), "Native provisioning failed: " + code)
        if reply.get("network") != "testnet4":
            raise Failed("PROVISION_CHAIN", "Native provisioning returned the wrong network.")
        return reply


class Explorer:
    def chain(self):
        for endpoint in EXPLORERS:
            if http(endpoint + "/block-height/0").decode().strip() != GENESIS:
                raise Failed("EXPLORER_CHAIN", "Both explorers must identify Bitcoin Testnet4 by genesis.")

    def utxos(self, address):
        self.chain()
        observed = []
        for endpoint in EXPLORERS:
            data = json.loads(http(endpoint + "/address/" + address + "/utxo"))
            observed.append({(v["txid"], v["vout"], v["value"]) for v in data})
        if observed[0] != observed[1]:
            raise Blocked("FUNDING_PROPAGATION", "The two Testnet4 explorers do not yet agree about funding.")
        return sorted(observed[0])

    def raw(self, txid):
        self.chain()
        raws = [http(endpoint + "/tx/" + txid + "/hex").decode().strip() for endpoint in EXPLORERS]
        if raws[0] != raws[1] or not re.fullmatch(r"[0-9a-f]+", raws[0]) or len(raws[0]) % 2:
            raise Failed("FUNDING_RAW", "Independent explorers must return identical valid transaction bytes.")
        return raws[0]

    def known(self, txid):
        return public_transaction(txid) is not None


def faucet_claim(address, journal):
    artifacts = journal.data["artifacts"]
    if artifacts.get("funding_claim_started"):
        raise Blocked("FUNDING_PENDING", "A faucet claim was already attempted; reconcile its address rather than claim again.")
    if time.time() < artifacts.get("faucet_retry_at", 0):
        raise Blocked("FAUCET_RATE_LIMIT", "The faucet retry window has not elapsed; no claim was sent.")
    journal.artifact("funding_claim_started", True)
    request = Request(FAUCET, data=json.dumps({"network": "btc-testnet", "address": address}).encode(),
                      headers={"Content-Type": "application/json", "User-Agent": "COINCUBE-Testnet4-QA/1.0"})
    try:
        with build_opener(ProxyHandler({}), NoRedirect()).open(request, timeout=30) as response:
            result = json.loads(response.read(100_000))
    except HTTPError as error:
        if error.code in (409, 429):
            try:
                refusal = json.loads(error.read(100_000))
                delay = max(60, min(int(refusal.get("retry_after", 3600)), 86400))
            except (ValueError, TypeError):
                delay = 3600
            # An explicit rate-limit/empty refusal did not send a payout.
            journal.artifact("faucet_retry_at", int(time.time()) + delay)
            journal.artifact("funding_claim_started", False)
            raise Blocked("FAUCET_RATE_LIMIT", "The faucet refused funding; its retry window is checkpointed.") from None
        raise Blocked("FAUCET_UNCERTAIN", "Faucet response was uncertain; future runs reconcile the address without repeating the claim.") from None
    except (URLError, OSError, ValueError, TimeoutError):
        raise Blocked("FAUCET_UNCERTAIN", "Faucet response was uncertain; future runs reconcile the address without repeating the claim.") from None
    if result.get("ok") is not True or result.get("network") != "btc-testnet" or not re.fullmatch(r"[0-9a-f]{64}", result.get("txid", "")):
        raise Blocked("FAUCET_RESPONSE", "Faucet response could not be validated; no additional claim will be made.")
    journal.artifact("funding_claim_txid", result["txid"])


class FreshRunner(Runner):
    stages = STAGES

    def __init__(self, config, directory, *, deadline=90, confirm=False):
        self.config = validate_config(config)
        self.deadline, self.confirm = deadline, confirm
        self.journal = Journal(directory, config)
        self.stack = ExitStack()
        self.stop, self.completed = False, set()
        self.profile = self.journal.directory / "profile"
        self.run_id = str(uuid.uuid5(uuid.NAMESPACE_URL, "coincube-fresh-testnet4/" + str(self.journal.directory)))
        self.core = Explorer()  # chain()/known() interface; v1 continues using full Core RPC.
        self.intent = None
        self.phone = None
        self.deferred_phone_purpose = None

    def close(self):
        self.stack.close()
        self.journal.close()

    def pin_artifact(self, name, value):
        previous = self.artifacts.get(name)
        if previous is not None and previous != value:
            raise Failed("PROVISION_CHANGED", "A previously checkpointed generated identity changed.")
        self.journal.artifact(name, value)

    def stage_environment(self):
        self.core.chain()
        repo = Path(__file__).resolve().parents[3]
        binary = self.config.get("native_binary")
        signing = self.config.get("signing_binary")
        if not binary or not signing:
            # Build/sign once per source revision. No credentials in argv/output.
            try:
                result = subprocess.run([str(repo / "tests/e2e/build-native")], cwd=repo,
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=1800)
            except (OSError, subprocess.TimeoutExpired):
                raise Blocked("NATIVE_BUILD", "The native helper build/signing prerequisites are unavailable.") from None
            if result.returncode:
                raise Blocked("NATIVE_BUILD", "The native helper build or code-sign verification failed.")
            binary = str(repo / "tests/e2e/.local/Provision.app/Contents/MacOS/testnet4_provision")
            signing = str(repo / "tests/e2e/.local/testnet4_connect")
        self.signing_binary = signing
        self.profile.mkdir(mode=0o700, parents=True, exist_ok=True)
        if not (self.profile / "testnet4/connect.json").exists():
            source = Path(self.config["profile_dir"]) / "testnet4"
            if (source / "connect.json").exists() or (source / "connect_transport_key").exists():
                copy_auth(self.config["profile_dir"], self.profile)
        self.provision = Provision(binary, self.profile, self.journal.directory, self.run_id)
        self.phone = self.stack.enter_context(attach(self.config["ios"]))

    def stage_authentication(self):
        registered = self.provision.call("bootstrap_auth", expected_email=self.config["account_email"],
                                         session_namespace=self.config.get("desktop_session_namespace", "development"))
        cache = json.loads(private_read(self.profile / "testnet4/connect.json"))
        candidates = [a for a in cache["accounts"] if a["email"] == cache.get("active_email")]
        if len(candidates) != 1 or not candidates[0].get("device_id"):
            raise Failed("CONNECT_ACCOUNT", "Bootstrap must produce one active, registered desktop Connect account.")
        self.account = candidates[0]
        if (self.account["email"].casefold() != self.config["account_email"].casefold()
                or registered["email"].casefold() != self.account["email"].casefold()
                or registered["device_id"] != self.account["device_id"]):
            raise Failed("CONNECT_ACCOUNT", "Bootstrap account or device differs from the expected QA identity.")
        self.pin_artifact("account_identity", digest({"email": self.account["email"].casefold(), "user_id": registered["user_id"]}))
        self.pin_artifact("desktop_identity", digest(self.account["device_id"]))
        repo = Path(__file__).resolve().parents[3]
        phone_dir = Path(self.config.get("phone_state_dir", str(repo / "tests/e2e/.local/phone"))) / self.artifacts["account_identity"]
        # This long-lived, account-bound credential record is generated by the
        # first successful onboarding, never supplied as a vault fixture.
        self.phone_journal = Journal(phone_dir, {"account": self.artifacts["account_identity"], "device": self.phone.device_digest})
        self.stack.callback(self.phone_journal.close)
        secret_id = str(uuid.uuid5(uuid.NAMESPACE_URL, "coincube-phone/" + self.artifacts["account_identity"] + "/" + self.phone.device_digest))
        store = SecretStore(Provision(self.provision.binary, phone_dir, phone_dir, secret_id), phone_dir / "phone-secrets.keychain.json")
        self.onboarding = Onboarding(self.phone, store, self.artifacts["account_identity"], self.deadline)
        self.defer_pending_phone_attempt(store.load())
        if self.deferred_phone_purpose:
            return
        labels = self.phone.labels()
        if {"Home", "Cubes", "Settings"} <= labels:
            self.phone.click("Settings")
            if self.account["email"].casefold() not in {v.casefold() for v in self.phone.labels()}:
                raise Failed("PHONE_ACCOUNT", "Keychain and the desktop must be signed into the same QA account.")
            self.phone.click("Home")

    def phone_validation_identity(self):
        return {"account": self.artifacts["account_identity"],
                "device": self.phone.device_digest, "network": "testnet4"}

    def defer_pending_phone_attempt(self, state):
        """Reconcile an existing attempt before touching its surviving UI."""
        self.deferred_phone_purpose = None
        if state is not None and state.get("identity") != self.onboarding.identity:
            raise Failed("PHONE_IDENTITY", "Saved phone credentials belong to a different account or device.")
        pending = state.get("pending_attempt") if state else None
        if state and "pending_attempt" in state and (not isinstance(pending, str) or not pending):
            raise Failed("PHONE_PENDING_INTENT", "The saved PIN purpose is malformed; no phone navigation is allowed.")
        key_purpose = "key:QA " + self.run_id
        request = self.artifacts.get("request_id")
        sign_purpose = "sign:" + request if isinstance(request, str) else None
        # Approval is recorded before opening the PIN screen. Its interruption
        # can precede the credential marker and must still preserve that route.
        if pending is None:
            if self.artifacts.get("approval_started") and self.artifacts.get("signature_count_min", 0) < 2:
                pending = sign_purpose or "invalid-sign-request"
            elif self.artifacts.get("phone_key_pin_entered") and not self.artifacts.get("target_key_id"):
                pending = key_purpose
            else:
                return
        if not state or state.get("verified") is not True:
            raise Blocked("PHONE_PENDING_IDENTITY", "An unresolved phone attempt requires a verified account/device credential record.")
        if pending == key_purpose:
            baseline = self.artifacts.get("baseline_key_ids")
            if (self.artifacts.get("phone_key_creation_started") is not True
                    or self.artifacts.get("phone_key_pin_entered") is not True
                    or not isinstance(baseline, list) or any(not isinstance(v, str) for v in baseline)
                    or not self.artifacts.get("cube_id")):
                raise Failed("PHONE_PENDING_INTENT", "The pending key attempt lacks this run's durable creation evidence.")
        elif sign_purpose is not None and pending == sign_purpose:
            intent = self.artifacts.get("intent")
            txid = intent.get("txid") if isinstance(intent, dict) else None
            if (self.artifacts.get("approval_started") is not True
                    or not isinstance(txid, str) or not re.fullmatch(r"[0-9a-f]{64}", txid)
                    or request != str(uuid.uuid5(uuid.UUID(self.run_id), txid))
                    or not isinstance(self.artifacts.get("session_id"), str) or not self.artifacts["session_id"]
                    or not self.artifacts.get("target_key_id") or not self.artifacts.get("vault_id")
                    or not intent.get("inputs") or not intent.get("outputs")):
                raise Failed("PHONE_PENDING_INTENT", "The pending signing attempt lacks this run's exact session and spend evidence.")
        else:
            raise Blocked("PHONE_PENDING_FOREIGN", "An unresolved phone attempt belongs to another operation or run; no navigation is allowed.")
        expected = self.phone_validation_identity()
        validation = self.artifacts.get("phone_validation")
        if validation is None:
            # Older checkpoints predate the explicit receipt. Their passed
            # stage is bound to this same run/configuration by Journal.
            previous = self.journal.data["stages"].get("phone_setup", {})
            if previous.get("status") != "passed":
                raise Blocked("PHONE_VALIDATION_REQUIRED", "An unresolved attempt has no prior successful account/network check in this run.")
            validation = {**expected, "source": "legacy_phone_setup", "stage_outcome": dict(previous)}
            self.journal.artifact("phone_validation", validation)
        if (not isinstance(validation, dict) or any(validation.get(k) != v for k, v in expected.items())
                or validation.get("source") not in ("settings_ui", "legacy_phone_setup")):
            raise Failed("PHONE_VALIDATION_CHANGED", "Saved phone account, device or network validation does not match this run.")
        self.journal.artifact("phone_validation_reused", {"purpose": pending, "validation": validation})
        self.deferred_phone_purpose = pending
        if pending == sign_purpose:
            self.archive_approval_attempt(state)

    def approval_binding(self):
        names = ("account_identity", "desktop_identity", "request_id", "session_id", "intent",
                 "psbt_sha256", "descriptor_id", "target_key_id", "target_fingerprint", "cube_id", "cube_uuid", "vault_id", "fee_sats")
        if any(not self.artifacts.get(name) for name in names):
            raise Blocked("APPROVAL_EVIDENCE", "The signing attempt lacks complete immutable run evidence.")
        if not re.fullmatch(r"[0-9a-f]{64}", self.artifacts["psbt_sha256"]):
            raise Failed("APPROVAL_EVIDENCE", "The saved prepared transaction hash is invalid.")
        return deepcopy({**{name: self.artifacts[name] for name in names}, "run_id": self.run_id,
                         "journal_binding": self.journal.data["binding"], "device": self.phone.device_digest})

    def untouched_signing_pin(self, state):
        return (isinstance(state, dict) and state.get("identity") == self.onboarding.identity
                and state.get("verified") is True and "pending_attempt" not in state
                and "pin_resume_probe" not in self.artifacts and "pin_dispatch_started" not in self.artifacts)

    def validate_approval_context(self, context):
        binding = self.approval_binding()
        if (not isinstance(context, dict) or context.get("binding") != binding
                or context.get("request_id") != binding["request_id"]
                or context.get("session_id") != binding["session_id"]
                or context.get("intent_sha256") != digest(binding["intent"])
                or context.get("fee_sats") != binding["fee_sats"]
                or context.get("device") != self.phone.device_digest
                or type(context.get("build")) is not int or context["build"] < 38
                or not isinstance(context.get("process"), dict)
                or type(context["process"].get("pid")) is not int or context["process"]["pid"] <= 0
                or context["process"].get("bundle_id") != "io.coincube.keychainApp"):
            raise Failed("APPROVAL_CHANGED", "The saved approval context does not match this run and device.")
        return binding

    def archive_approval_attempt(self, state):
        """Save original evidence before rerun stage outcomes overwrite it."""
        existing = self.artifacts.get("approval_original_attempt")
        if existing is not None:
            if not isinstance(existing, dict) or existing.get("binding") != self.approval_binding():
                raise Failed("APPROVAL_CHANGED", "The archived signing attempt no longer matches this run.")
            return
        if not self.untouched_signing_pin(state):
            return  # Collection remains possible; PIN replay never is.
        context = self.artifacts.get("approval_context")
        if context is not None:
            binding = self.validate_approval_context(context)
            original = {"origin": "approval-context", "binding": binding, "build": context["build"],
                        "context": deepcopy(context)}
        else:
            phone = self.artifacts.get("phone", {})
            before = self.journal.data["stages"].get("phone", {})
            after = self.journal.data["stages"].get("sign", {})
            if (not isinstance(phone, dict) or type(phone.get("build")) is not int or phone["build"] != 37
                    or phone.get("network") != "testnet4" or phone.get("cube_visible") is not True
                    or before.get("status") != "passed" or after.get("status") != "blocked"
                    or after.get("code") != "PHONE_AUTH"
                    or type(before.get("at")) is not int or type(after.get("at")) is not int
                    or not 0 < before["at"] <= after["at"]):
                return
            try:
                binding = self.approval_binding()
            except Blocked:
                return
            original = {"origin": "legacy-stage-evidence", "binding": binding, "build": phone["build"],
                        "phone": deepcopy(phone), "phone_stage": deepcopy(before), "sign_stage": deepcopy(after)}
        self.pin_artifact("approval_original_attempt", original)

    def collect_once(self):
        result = self.native_call("collect", session_id=self.artifacts["session_id"],
                                  output_psbt_file=str(self.journal.directory / "signed.psbt"))
        self.last_collection = result
        if result["ready"]:
            self.journal.artifact("signature_count_min", result["signature_count_min"])
        return result["ready"]

    def require_viewed_unsubmitted(self):
        binding = self.approval_binding()
        pinned = self.artifacts.get("approval_binding")
        original = self.artifacts.get("approval_original_attempt")
        if ((pinned is not None and pinned != binding)
                or (original is not None and original.get("binding") != binding)):
            raise Failed("APPROVAL_CHANGED", "The reviewed transaction binding changed before approval or PIN entry.")
        if self.collect_once():
            return False
        result = self.last_collection
        if (result.get("ready") is not False or type(result.get("status")) is not int
                or result["status"] != 3 or type(result.get("submitted_signature_count")) is not int
                or result["submitted_signature_count"] != 0):
            raise Blocked("APPROVAL_REMOTE_STATE", "Only an exact viewed session with zero submitted signatures may continue its first PIN attempt.")
        return True

    def upgrade_approval(self):
        original = self.artifacts.get("approval_original_attempt")
        if original is None:
            return False
        binding = self.approval_binding()
        if original.get("binding") != binding:
            raise Failed("APPROVAL_CHANGED", "The archived signing attempt changed.")
        state = self.onboarding.store.load()
        if not self.untouched_signing_pin(state):
            return False
        build = self.phone.build
        if type(build) is not int or build < 38 or type(original.get("build")) is not int or build <= original["build"]:
            return False
        receipt = self.artifacts.get("approval_upgrade_recovery")
        if receipt is None:
            receipt = {"original_attempt": deepcopy(original), "binding": binding, "original_build": original["build"],
                       "current_build": build, "phase": "planned"}
            self.journal.artifact("approval_upgrade_recovery", receipt)
        if (not isinstance(receipt, dict) or receipt.get("original_attempt") != original
                or receipt.get("binding") != binding or receipt.get("current_build") != build
                or receipt.get("original_build") != original["build"]):
            raise Failed("APPROVAL_CHANGED", "The saved upgrade recovery no longer matches this attempt.")
        if receipt.get("phase") == "approval_dispatched":
            return False
        if receipt.get("phase") not in ("planned", "process_stopped", "ready_for_review"):
            raise Failed("APPROVAL_RECOVERY_PHASE", "The recovery checkpoint has an invalid phase.")
        if not self.require_viewed_unsubmitted():
            return False
        self.phone.restart_for_upgrade(self.journal, receipt)
        if receipt.get("phase") != "ready_for_review":
            raise Blocked("APPROVAL_PROCESS_BOUNDARY", "The old signing process was not conclusively stopped and replaced.")
        if not self.require_viewed_unsubmitted():
            return False
        self.deferred_phone_purpose = None
        self.stage_phone_setup()
        self.stage_phone()
        self.approve_phone(reapproval=True)
        return True

    def approve_phone(self, *, reapproval=False):
        binding = self.approval_binding()
        self.pin_artifact("approval_binding", binding)
        self.phone.approve(self.request_id, self.intent, self.artifacts["fee_sats"],
                           None, self.journal, self.deadline, session_id=self.artifacts["session_id"],
                           pin_provider=lambda: self.onboarding.pin_attempt("sign:" + self.request_id),
                           probe_pin_resume=True, before_approve=self.require_viewed_unsubmitted,
                           before_pin=self.require_viewed_unsubmitted, reapproval=reapproval)

    def finish_deferred_phone_attempt(self, purpose):
        if self.deferred_phone_purpose != purpose:
            return
        # Only callers with a matched key or cryptographically verified signature
        # reach here. No absent/timeout result can release the navigation guard.
        self.deferred_phone_purpose = None
        self.stage_phone_setup()

    def stage_phone_setup(self):
        if self.deferred_phone_purpose:
            return  # Historical validation receipt is retained, not re-observed.
        self.pin = self.onboarding.setup()
        self.phone.restore_navigation()
        self.phone.click("Settings")
        labels = self.phone.labels()
        if self.account["email"].casefold() not in {v.casefold() for v in labels}:
            raise Failed("PHONE_ACCOUNT", "Keychain and the desktop must be signed into the same QA account.")
        if "Testnet4" not in labels:
            choices = labels & {"Bitcoin (mainnet)", "Testnet", "Signet", "Regtest", "Bitcoin Blake2b", "Bitcoin Blake2b Testnet4"}
            if len(choices) != 1:
                raise Blocked("PHONE_NETWORK", "The current network selector is ambiguous.")
            self.phone.click(next(iter(choices)))
            self.onboarding.wait("Testnet4")
            self.phone.click("Testnet4")
            self.onboarding.wait("Testnet4")
        self.phone.click("Home")
        self.journal.artifact("phone_validation", {**self.phone_validation_identity(), "source": "settings_ui"})

    def stage_cube_creation(self):
        result = self.provision.call("cube")
        if result.get("backup_verified") is not True:
            raise Failed("CUBE_BACKUP", "The newly created Cube's backup was not verified.")
        for key in ("cube_id", "cube_uuid", "cube_name"):
            self.pin_artifact(key, result[key])

    def keys(self):
        raw = http(REST + "/api/v1/keys", headers={"Authorization": "Bearer " + self.account["tokens"]["access_token"]})
        return json.loads(raw)["data"]["keys"]

    def stage_phone_key(self):
        label = "QA " + self.run_id
        keys = self.keys()
        matches = [k for k in keys if k["name"] == label and k["network"] == "testnet4"]
        if matches and (not self.artifacts.get("phone_key_creation_started") or "baseline_key_ids" not in self.artifacts):
            raise Failed("KEY_NOT_FRESH", "A matching key predates this run's recorded creation attempt.")
        if not matches and not self.artifacts.get("phone_key_creation_started"):
            self.pin_artifact("baseline_key_ids", sorted(str(k["id"]) for k in keys))
            self.onboarding.create_key(label, self.pin, self.journal)
        until = time.monotonic() + self.deadline
        while not matches:
            matches = [k for k in self.keys() if k["name"] == label and k["network"] == "testnet4"]
            if time.monotonic() >= until:
                raise Blocked("KEY_REGISTRATION", "The exact generated phone key has not appeared in Connect; no duplicate key was created.")
            if not matches:
                time.sleep(2)
        if len(matches) != 1:
            raise Failed("KEY_AMBIGUOUS", "The run-specific phone key label must identify exactly one key.")
        key = matches[0]
        if str(key["id"]) in self.artifacts.get("baseline_key_ids", []):
            raise Failed("KEY_NOT_FRESH", "The purported new phone key existed before this run.")
        if key.get("cubeId") not in (None, 0, self.artifacts["cube_id"]):
            raise Failed("KEY_OWNERSHIP", "The generated key must be a personal key or attached only to this run's Cube.")
        key_path = self.journal.directory / "phone-key.json"
        identity = {"derivation_path": key["derivationPath"], "fingerprint": key["fingerprint"],
                    "key_id": str(key["id"]), "network": "testnet4"}
        saved = json.loads(private_read(key_path)) if key_path.exists() else None
        if saved is not None and (any(saved.get(k) != v for k, v in identity.items())
                                  or (key.get("xpub") and saved.get("xpub") != key["xpub"])):
            raise Failed("KEY_CHANGED", "The registered key differs from this run's saved public identity.")
        # Attaching/resealing intentionally removes the plaintext xpub from
        # Connect. Keep the original public export for native envelope checking.
        public_key = key.get("xpub") or (saved.get("xpub") if saved else None)
        if not public_key:
            raise Failed("KEY_OWNERSHIP", "The newly created key needs its original public export before attachment.")
        self.pin_artifact("target_key_id", str(key["id"]))
        self.pin_artifact("target_fingerprint", key["fingerprint"])
        self.onboarding.reconcile_pin("key:" + label)
        if saved is None:
            atomic_write(key_path, json.dumps(dict(identity, xpub=public_key)).encode())
        self.finish_deferred_phone_attempt("key:" + label)

    def ensure_phone_key_attachment(self):
        target, cube = self.artifacts["target_key_id"], self.artifacts["cube_id"]
        until = time.monotonic() + self.deadline
        while True:
            matches = [key for key in self.keys() if str(key["id"]) == target]
            if len(matches) != 1:
                raise Failed("KEY_ATTACHMENT_IDENTITY", "The exact created signer key must remain uniquely available.")
            key = matches[0]
            if (key.get("name") != "QA " + self.run_id or key.get("network") != "testnet4"
                    or key.get("fingerprint") != self.artifacts["target_fingerprint"]
                    or key.get("status") != "active" or key.get("cubeId") not in (None, 0, cube)):
                raise Failed("KEY_ATTACHMENT_IDENTITY", "The created signer key changed identity, eligibility or Cube binding.")
            if key.get("cubeId") == cube:
                response = json.loads(http(REST + "/api/v1/connect/cubes/" + self.artifacts["cube_uuid"] + "/keys",
                    headers={"Authorization": "Bearer " + self.account["tokens"]["access_token"]}))
                rows = [row for row in response["data"] if str(row["id"]) == target]
                if len(rows) == 1 and rows[0].get("xpubEnvelope"):
                    # Presence is a polling condition only. The native vault
                    # action verifies encryption and exact exported identity.
                    self.journal.artifact("phone_key_attached", True)
                    return
            elif not self.artifacts.get("phone_key_attachment_started"):
                if self.deferred_phone_purpose:
                    raise Blocked("KEY_ATTACHMENT_PENDING", "An unresolved signing attempt forbids attachment navigation.")
                self.onboarding.attach_key("QA " + self.run_id, self.artifacts["cube_name"], self.journal)
            if time.monotonic() >= until:
                raise Blocked("KEY_ATTACHMENT_PENDING", "Keychain binding and its encrypted public-key envelope are not both visible; no attachment was replayed.")
            time.sleep(1)

    def stage_vault_creation(self):
        self.ensure_phone_key_attachment()
        result = self.provision.call("vault", target_key_id=self.artifacts["target_key_id"],
                                    target_fingerprint=self.artifacts["target_fingerprint"],
                                    phone_key_file=str(self.journal.directory / "phone-key.json"))
        for key in ("vault_id", "descriptor_id", "descriptor_file", "receive_address"):
            self.pin_artifact(key, result[key])
        if not self.deferred_phone_purpose:
            self.phone.check_fixture(self.artifacts["cube_name"], self.deadline)

    def stage_funding(self):
        if self.artifacts.get("funding_outpoint"):
            return  # Native prepare revalidates the exact saved raw transaction.
        address = self.artifacts["receive_address"]
        if not isinstance(address, str) or not re.fullmatch(r"tb1q[a-z0-9]{58}", address):
            raise Failed("FUNDING_ADDRESS", "Native provisioning must return a Testnet4 P2WSH funding address.")
        if not self.artifacts.get("funding_claim_started"):
            faucet_claim(address, self.journal)
        until = time.monotonic() + self.deadline
        while True:
            try:
                outputs = self.core.utxos(address)
            except Blocked as error:
                if isinstance(error, Failed) or error.code != "FUNDING_PROPAGATION":
                    raise
                outputs = []
            eligible = [v for v in outputs if 100_000 <= v[2] <= 10_000_000
                        and v[2] >= self.config["amount_sats"] + self.config["fee_sats"] + 1_000
                        and (not self.artifacts.get("funding_claim_txid") or v[0] == self.artifacts["funding_claim_txid"])]
            if len(eligible) == 1:
                txid, vout, value = eligible[0]
                atomic_write(self.journal.directory / "funding.hex", self.core.raw(txid).encode())
                self.pin_artifact("funding_outpoint", {"txid": txid, "vout": vout, "value_sats": value})
                return
            if len(eligible) > 1:
                raise Failed("FUNDING_AMBIGUOUS", "More than one eligible faucet output exists; the runner will not guess.")
            if time.monotonic() >= until:
                raise Blocked("FUNDING_PENDING", "The faucet payment is not yet visible in both Testnet4 explorers.")
            time.sleep(3)

    def stage_prepare(self):
        result = self.provision.call("prepare_spend", funding_tx_file=str(self.journal.directory / "funding.hex"),
                                    funding_vout=self.artifacts["funding_outpoint"]["vout"],
                                    amount_sats=self.config["amount_sats"], fee_sats=self.config["fee_sats"])
        self.intent = {"txid": result["expected_txid"], "inputs": [f'{v["txid"]}:{v["vout"]}' for v in result["inputs"]],
                       "outputs": [{"address": v["address"], "sats": v["value_sats"]} for v in result["outputs"]],
                       "maximum_fee_sats": self.config["fee_sats"]}
        self.pin_artifact("intent", self.intent)
        fixture = {k: self.artifacts[k] for k in ("cube_id", "vault_id", "target_key_id", "target_fingerprint", "descriptor_file")}
        fixture.update(profile_dir=str(self.profile), psbt_file=result["psbt_file"], expected_txid=self.intent["txid"])
        self.native = Native(self.signing_binary, fixture)
        self.request_id = str(uuid.uuid5(uuid.UUID(self.run_id), self.intent["txid"]))
        self.pin_artifact("request_id", self.request_id)
        self.pin_artifact("psbt_sha256", hashlib.sha256(private_read(result["psbt_file"])).hexdigest())

    def stage_connect(self):
        result = self.native_call("inspect")
        if result["fee_sats"] != self.config["fee_sats"]:
            raise Failed("FEE_CAP", "The generated PSBT fee differs from the fixed scenario fee.")
        self.pin_artifact("fee_sats", result["fee_sats"])
        if self.core.known(self.intent["txid"]):
            if not self.artifacts.get("broadcast_hex_sha256") or self.artifacts.get("signature_count_min", 0) < 2:
                raise Failed("EXTERNAL_BROADCAST", "A spend appeared without this run's signature and broadcast evidence.")
            self.journal.artifact("already_broadcast", True)
        else:
            outpoint = self.artifacts["funding_outpoint"]
            if (outpoint["txid"], outpoint["vout"], outpoint["value_sats"]) not in self.core.utxos(self.artifacts["receive_address"]):
                raise Blocked("INPUT_SPENT", "The generated funding output is unavailable; no replacement funding will be requested.")

    def stage_phone(self):
        if self.deferred_phone_purpose:
            return
        self.journal.artifact("phone", self.phone.check_fixture(self.artifacts["cube_name"], self.deadline))

    def stage_sign(self):
        if self.artifacts.get("already_broadcast") or self.collect_once():
            return
        if not self.artifacts.get("approval_started"):
            self.approve_phone()
            return
        if self.upgrade_approval():
            return
        context = self.artifacts.get("approval_context")
        if context is not None and self.untouched_signing_pin(self.onboarding.store.load()):
            self.validate_approval_context(context)
            self.phone.resume_first_pin(self.request_id, self.artifacts["session_id"], self.intent,
                self.artifacts["fee_sats"], self.journal, self.deadline,
                pin_provider=lambda: self.onboarding.pin_attempt("sign:" + self.request_id),
                before_pin=self.require_viewed_unsubmitted)

    def stage_collect(self):
        super().stage_collect()
        self.onboarding.reconcile_pin("sign:" + self.request_id)
        if self.deferred_phone_purpose == "sign:" + self.request_id:
            self.finish_deferred_phone_attempt("sign:" + self.request_id)
            self.stage_phone()

    def stage_broadcast(self):
        self.core.chain()
        if self.core.known(self.intent["txid"]):
            self.journal.artifact("broadcast_txid", self.intent["txid"])
            return
        result = self.provision.call("finalize", signed_psbt_file=str(self.journal.directory / "signed.psbt"))
        if result["txid"] != self.intent["txid"] or result["fee_sats"] != self.artifacts["fee_sats"]:
            raise Failed("FINAL_INTENT", "Finalized transaction identity or fee differs from the pinned spend.")
        if [f'{v["txid"]}:{v["vout"]}' for v in result["inputs"]] != self.intent["inputs"] or [
                {"address": v["address"], "sats": v["value_sats"]} for v in result["outputs"]] != self.intent["outputs"]:
            raise Failed("FINAL_INTENT", "Finalized transaction inputs or outputs differ from the pinned spend.")
        raw = result["raw_tx"]
        if not re.fullmatch(r"[0-9a-f]+", raw) or len(raw) % 2:
            raise Failed("FINAL_BYTES", "Native finalization did not return valid signed transaction bytes.")
        self.pin_artifact("broadcast_hex_sha256", hashlib.sha256(raw.encode()).hexdigest())
        atomic_write(self.journal.directory / "signed.hex", raw.encode())
        # Retry may send only the same finalized bytes. The Testnet4 node behind
        # Esplora performs mempool acceptance; no local full-node setup required.
        returned = http(EXPLORERS[0] + "/tx", body=raw.encode(), headers={"Content-Type": "text/plain"}).decode().strip()
        if returned != self.intent["txid"]:
            raise Failed("BROADCAST_TXID", "Testnet4 broadcast returned an unexpected transaction ID.")
        self.journal.artifact("broadcast_txid", returned)
