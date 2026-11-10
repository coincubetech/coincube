"""One immutable spend intent, one stable request, one resumable broadcast."""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import time
from urllib.parse import urlsplit
import uuid

from .clients import Core, GRPC, Native, REST, assert_transaction, http, public_transaction, sats
from .phone import attach
from .state import Blocked, Failed, Journal, atomic_write, private_read

STAGES = ("chain", "cube_creation", "connect", "phone", "funding", "request", "sign", "collect", "broadcast", "verify")


def load_config(path):
    config = json.loads(private_read(path))
    required = {"version", "run_id", "connect_rest", "connect_grpc", "native_binary", "fixture", "intent", "core", "ios", "cube_creation"}
    if set(config) != required or config["version"] != 1:
        raise Failed("CONFIG_SCHEMA", "Configuration fields do not match schema version 1.")
    if config["connect_rest"] != REST or config["connect_grpc"] != GRPC:
        raise Failed("CONNECT_ENVIRONMENT", "This acceptance suite requires production Connect endpoints.")
    if not re.fullmatch(r"[a-zA-Z0-9_-]{1,80}", config["run_id"]):
        raise Failed("RUN_ID", "Use a stable run ID containing letters, digits, underscores or hyphens.")
    fixture = config["fixture"]
    if set(fixture) != {"profile_dir", "psbt_file", "descriptor_file", "expected_txid", "cube_id", "vault_id", "target_key_id", "target_fingerprint"}:
        raise Failed("FIXTURE_SCHEMA", "Fixture fields do not match the native helper protocol.")
    for field in ("psbt_file", "descriptor_file"):
        private_read(fixture[field])
    for path_value in (config["native_binary"], fixture["profile_dir"], fixture["psbt_file"], fixture["descriptor_file"]):
        if not isinstance(path_value, str) or not Path(path_value).is_absolute():
            raise Failed("ABSOLUTE_PATH", "Native helper and fixture paths must be absolute.")
    intent = config["intent"]
    if set(intent) != {"txid", "inputs", "outputs", "maximum_fee_sats"}:
        raise Failed("INTENT_SCHEMA", "Intent must specify transaction ID, exact inputs, exact outputs and fee cap.")
    if not re.fullmatch(r"[0-9a-f]{64}", intent["txid"]) or fixture["expected_txid"] != intent["txid"]:
        raise Failed("INTENT_TXID", "Fixture and spend intent must name the same transaction.")
    if not intent["inputs"] or any(not re.fullmatch(r"[0-9a-f]{64}:[0-9]+", v) for v in intent["inputs"]):
        raise Failed("INTENT_INPUTS", "Specify the exact funded outpoints for this spend.")
    if len(intent["outputs"]) != 2:
        raise Failed("INTENT_OUTPUTS", "This scenario requires one recipient and one change output.")
    for out in intent["outputs"]:
        if set(out) != {"address", "sats"} or not isinstance(out["address"], str) or not out["address"].startswith("tb1"):
            raise Failed("INTENT_ADDRESS", "The fixture must use Testnet4 SegWit destinations.")
        if type(out["sats"]) is not int or not 0 < out["sats"] <= 10_000_000:
            raise Failed("INTENT_AMOUNT", "Fixture outputs must be positive and at most 10 million test sats each.")
    if type(intent["maximum_fee_sats"]) is not int or not 0 < intent["maximum_fee_sats"] <= 100_000:
        raise Failed("FEE_CAP", "An explicit fee cap between 1 and 100,000 sats is required.")
    core = config["core"]
    if set(core) != {"url", "cookie_file"}:
        raise Failed("CORE_CONFIG", "Core configuration requires a loopback URL and cookie file.")
    url = urlsplit(core["url"])
    if url.scheme != "http" or url.hostname not in ("127.0.0.1", "::1") or url.username or url.password or url.query or url.fragment:
        raise Failed("CORE_URL", "Bitcoin Core RPC must use an explicit loopback URL without embedded credentials.")
    ios = config["ios"]
    if not {"minimum_build", "cube_label"} <= set(ios) or set(ios) - {"minimum_build", "cube_label", "pin_file", "appium", "xctestrun"}:
        raise Failed("IOS_CONFIG", "Unknown or missing iPhone configuration fields.")
    if type(ios["minimum_build"]) is not int or ios["minimum_build"] < 35 or not isinstance(ios["cube_label"], str) or not ios["cube_label"]:
        raise Failed("IOS_BUILD", "Require a Keychain build containing #194 (build 35 or later) and the fixture Cube label.")
    if type(config["cube_creation"]) is not bool:
        raise Failed("CUBE_CONFIG", "cube_creation must be true or false.")
    return config


class Runner:
    stages = STAGES
    def __init__(self, config, directory, *, deadline=90, confirm=False):
        self.config = config
        self.deadline = deadline
        self.confirm = confirm
        binding = dict(config)
        binding["fixture_hashes"] = {key: hashlib.sha256(private_read(config["fixture"][key])).hexdigest()
                                     for key in ("psbt_file", "descriptor_file")}
        self.journal = Journal(directory, binding)
        self.core = Core(config["core"])
        self.native = Native(config["native_binary"], config["fixture"])
        self.intent = config["intent"]
        self.stop = False
        self.completed = set()
        self.request_id = str(uuid.uuid5(uuid.NAMESPACE_URL, "coincube-testnet4/" + config["run_id"] + "/" + self.intent["txid"]))
        existing = self.journal.data["artifacts"].get("request_id")
        if existing and existing != self.request_id:
            raise Failed("REQUEST_ID", "Checkpoint request identity is inconsistent.")
        self.journal.artifact("request_id", self.request_id)

    @property
    def artifacts(self):
        return self.journal.data["artifacts"]

    def native_call(self, action, **extra):
        return self.native.call(action, request_id=self.request_id, **extra)

    def run(self, stage):
        if self.stop:
            raise Blocked("DEPENDENCY_BLOCKED", "An earlier stage did not complete in this invocation.")
        preceding = set(self.stages[:self.stages.index(stage)])
        if not preceding <= self.completed:
            raise Failed("STAGE_ORDER", "Stages must run in the suite's declared order.")
        try:
            getattr(self, "stage_" + stage)()
        except Blocked as error:
            self.stop = True
            self.journal.outcome(stage, "failed" if isinstance(error, Failed) else "blocked", error.code)
            raise
        except Exception:
            self.stop = True
            self.journal.outcome(stage, "failed", "UNEXPECTED_ERROR")
            raise Failed("UNEXPECTED_ERROR", "Unexpected runner error; raw exception suppressed to protect fixture data.") from None
        self.completed.add(stage)
        self.journal.outcome(stage, "passed")

    def stage_chain(self):
        self.core.chain()
        self.journal.artifact("chain", "bitcoin-testnet4")

    def close(self):
        self.journal.close()

    def stage_cube_creation(self):
        if not self.config["cube_creation"]:
            raise Blocked("CUBE_TEST_DISABLED", "Enable the signed Cube creation test for full acceptance.")
        repo = Path(__file__).resolve().parents[3]
        revision = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], timeout=5).decode().strip()
        if self.artifacts.get("cube_test_revision") == revision:
            return
        script = repo / "contrib/release/macos/run-signed-device-secret-tests.sh"
        import os
        env = os.environ.copy()
        env.update({"CARGO_NET_OFFLINE": "true", "BREEZ_API_KEY": "testnet4-fixture-placeholder",
                    "COINCUBE_API_URL": "http://127.0.0.1:1", "COINCUBE_CONNECT_GRPC_URL": "http://127.0.0.1:1",
                    "SIGN_IDENTITY": "Developer ID Application: COINCUBE TECHNOLOGY LLC (8UVR249AD5)"})
        env["PATH"] = "/opt/homebrew/opt/rustup/bin:" + env.get("PATH", "")
        env["CARGO_TARGET_DIR"] = str(repo / "target")
        try:
            result = subprocess.run([str(script), "creating_a_testnet4_cube_walks_the_backup_step_and_reopens"],
                                    cwd=repo, env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=1800)
        except (OSError, subprocess.TimeoutExpired):
            raise Blocked("CUBE_TEST_BUILD", "Signed Cube fixture test could not run; build/signing prerequisites are missing.") from None
        if result.returncode or b"1 passed; 0 failed" not in result.stdout:
            raise Blocked("CUBE_TEST", "Signed Cube creation/recovery/reopen test did not pass.")
        self.journal.artifact("cube_test_revision", revision)

    def stage_connect(self):
        result = self.native_call("inspect")
        fee = result["fee_sats"]
        if type(fee) is not int or not 0 <= fee <= self.intent["maximum_fee_sats"]:
            raise Failed("FEE_CAP", "Native PSBT fee exceeds the declared cap.")
        self.journal.artifact("fee_sats", fee)
        self.journal.artifact("descriptor_id", result["descriptor_id"])
        self.journal.artifact("native_binary_sha256", hashlib.sha256(Path(self.config["native_binary"]).read_bytes()).hexdigest())

    def stage_phone(self):
        # Always refresh hardware/build/network checks, even on resume.
        with attach(self.config["ios"]) as phone:
            observed = phone.check_fixture(self.config["ios"]["cube_label"])
        self.journal.artifact("phone", observed)

    def stage_funding(self):
        psbt = private_read(self.config["fixture"]["psbt_file"]).decode().strip()
        decoded = self.core.call("decodepsbt", psbt)
        assert_transaction(decoded["tx"], self.intent)
        if "fee" not in decoded or sats(decoded["fee"]) != self.artifacts["fee_sats"]:
            raise Failed("PSBT_FEE", "Core and native PSBT fee verification disagree.")
        if self.core.known(self.intent["txid"]) or public_transaction(self.intent["txid"]):
            if not (self.artifacts.get("session_id") and self.artifacts.get("signature_count_min", 0) >= 2
                    and self.artifacts.get("broadcast_hex_sha256")):
                raise Blocked("EXTERNAL_BROADCAST", "This transaction already exists without this run's signing/broadcast evidence; it cannot count as a completed replay.")
            self.journal.artifact("already_broadcast", True)
            return
        for outpoint in self.intent["inputs"]:
            txid, index = outpoint.split(":")
            if self.core.call("gettxout", txid, int(index), True) is None:
                raise Blocked("INPUT_SPENT", "A fixture input is unavailable and the intended transaction is not observed. No replacement spend will be created.")

    def stage_request(self):
        if self.artifacts.get("already_broadcast"):
            return
        self.core.chain()
        # request_id was fsynced in __init__. A lost RPC response therefore
        # retries the same server idempotency key, never a new signing session.
        response = self.native_call("request")
        session = response["session_id"]
        previous = self.artifacts.get("session_id")
        if previous and previous != session:
            raise Failed("SESSION_CHANGED", "Idempotent request returned a different session.")
        self.journal.artifact("session_id", session)

    def collect_once(self):
        result = self.native_call("collect", session_id=self.artifacts["session_id"],
                                  output_psbt_file=str(self.journal.directory / "signed.psbt"))
        if result["ready"]:
            self.journal.artifact("signature_count_min", result["signature_count_min"])
        return result["ready"]

    def stage_sign(self):
        if self.artifacts.get("already_broadcast") or self.collect_once():
            return
        if self.artifacts.get("approval_started"):
            # An approval may have reached the phone even when its response
            # was lost. Let collect keep polling across reruns; never enter
            # the approval UI or PIN a second time.
            return
        with attach(self.config["ios"]) as phone:
            phone.approve(self.request_id, self.intent, self.artifacts["fee_sats"],
                          self.config["ios"].get("pin_file"), self.journal, self.deadline)

    def stage_collect(self):
        if self.artifacts.get("already_broadcast"):
            return
        until = time.monotonic() + self.deadline
        while not self.collect_once():
            if time.monotonic() >= until:
                raise Blocked("SIGNATURE_DELIVERY", "Connect has not returned a verifiable second signature before the deadline.")
            time.sleep(2)

    def stage_broadcast(self):
        self.core.chain()
        txid = self.intent["txid"]
        if self.core.known(txid) or public_transaction(txid):
            self.journal.artifact("broadcast_txid", txid)
            return
        psbt = private_read(self.journal.directory / "signed.psbt").decode().strip()
        finalized = self.core.call("finalizepsbt", psbt)
        if finalized.get("complete") is not True:
            raise Failed("QUORUM", "Bitcoin Core could not finalize the signed 2-of-3 PSBT.")
        raw = finalized["hex"]
        decoded = self.core.call("decoderawtransaction", raw)
        assert_transaction(decoded, self.intent)
        result, = self.core.call("testmempoolaccept", [raw])
        if result.get("txid") != txid or result.get("allowed") is not True:
            raise Blocked("MEMPOOL_REJECTED", "Bitcoin Core did not accept the exact signed Testnet4 transaction in preflight.")
        if sats(result["fees"]["base"]) != self.artifacts["fee_sats"]:
            raise Failed("BROADCAST_FEE", "Final transaction fee does not match the reviewed fee.")
        raw_digest = hashlib.sha256(raw.encode()).hexdigest()
        previous = self.artifacts.get("broadcast_hex_sha256")
        if previous and previous != raw_digest:
            raise Failed("BROADCAST_CHANGED", "Signed bytes changed after a previous broadcast attempt.")
        atomic_write(self.journal.directory / "signed.hex", raw.encode())
        self.journal.artifact("broadcast_hex_sha256", raw_digest)
        # If the call times out, rerun reconciles txid above. Only these exact
        # bytes can be retried after mempool preflight; no re-funding or RBF.
        returned = self.core.call("sendrawtransaction", raw)
        if returned != txid:
            raise Failed("BROADCAST_TXID", "Broadcast RPC returned an unexpected transaction ID.")
        self.journal.artifact("broadcast_txid", txid)

    def stage_verify(self):
        until = time.monotonic() + self.deadline
        while True:
            result = public_transaction(self.intent["txid"])
            if result is not None:
                if result["fee"] != self.artifacts["fee_sats"]:
                    raise Failed("PUBLIC_FEE", "Public transaction fee differs from the reviewed transaction.")
                confirmed = result["status"].get("confirmed") is True
                self.journal.artifact("public_acceptance", {"txid": self.intent["txid"], "confirmed": confirmed})
                if not self.confirm or confirmed:
                    return
            if time.monotonic() >= until:
                raise Blocked("PUBLIC_PROPAGATION", "Independent explorers have not observed the required transaction state; rerun to resume verification.")
            time.sleep(3)
