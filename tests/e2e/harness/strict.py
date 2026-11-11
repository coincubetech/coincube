"""Versioned strict UI acceptance: no restart, passive delivery, correlated Home refresh."""
from copy import deepcopy
import re
import time

from .state import Blocked, Failed, digest

BUNDLE = "io.coincube.keychainApp"
IDLE_SECONDS = 90
MAX_OBSERVATION_GAP = 5
RECOVERIES = ("request_discovery_restart", "approval_upgrade_recovery", "key_generation_recovery")


def validate_mode(config):
    mode = config.get("strict_acceptance")
    if "strict_acceptance" in config and (type(mode) is not dict or mode != {"version": 1}
                             or type(mode.get("version")) is not int):
        raise Failed("STRICT_CONFIG", "Strict acceptance must explicitly select version 1.")
    if mode is not None and config["ios"]["minimum_build"] < 39:
        raise Failed("STRICT_BUILD", "Strict acceptance requires a QA Home-refresh build numbered 39 or newer.")


class StrictPolicy:
    def __init__(self, journal):
        self.journal = journal
        self.phone = None
        mode = {"version": 1, "config_binding": journal.data["binding"]}
        existing = self.artifacts.get("strict_acceptance")
        if existing is None:
            if self.artifacts or journal.data["stages"]:
                raise Failed("STRICT_RETROFIT", "Strict acceptance requires a fresh run; existing results cannot become strict evidence.")
            journal.artifact("strict_acceptance", mode)
        elif existing != mode:
            raise Failed("STRICT_BINDING", "The strict acceptance mode or configuration changed.")
        if any(name in self.artifacts for name in RECOVERIES):
            raise Failed("STRICT_RECOVERY", "Restart/recovery allowances are incompatible with strict acceptance.")

    @property
    def artifacts(self):
        return self.journal.data["artifacts"]

    @property
    def requested(self):
        return "strict_request" in self.artifacts

    def pin(self, name, value):
        previous = self.artifacts.get(name)
        if previous is not None and previous != value:
            raise Failed("STRICT_BINDING", "A pinned strict acceptance identity changed.")
        self.journal.artifact(name, deepcopy(value))

    def bind_phone(self, phone):
        self.phone = phone
        phone.strict = self
        if phone.build < 39:
            raise Blocked("STRICT_BUILD", "The installed app lacks the required QA Home-refresh contract.")
        observed = self.observe(pin=True)
        probe = self.artifacts.get("strict_home_refresh")
        if probe is not None and probe.get("phase") != "completed":
            raise Blocked("STRICT_PROBE_UNCERTAIN", "The strict refresh probe was interrupted and cannot be retried.")
        return observed

    def observe(self, *, pin=False):
        # Never activate: activation can silently relaunch a dead app.
        if int(self.phone.driver.query_app_state(BUNDLE)) != 4:
            raise Blocked("STRICT_PROCESS", "Strict acceptance requires the original app process in the foreground at each observation.")
        process = self.phone.process_identity()
        observed = {"device": self.phone.device_digest, "build": self.phone.build, "process": process}
        expected = self.artifacts.get("strict_process")
        if expected is None:
            if not pin:
                raise Failed("STRICT_BINDING", "The strict phone process was not pinned before use.")
            self.pin("strict_process", observed)
        elif expected != observed:
            raise Blocked("STRICT_PROCESS", "The strict phone PID, build or device changed; no restart is allowed.")
        return observed

    def reject_restart(self):
        raise Blocked("STRICT_RESTART", "Strict acceptance forbids app restart and upgrade/recovery workarounds.")

    def before_navigation(self):
        self.observe()
        if self.requested:
            raise Blocked("STRICT_NAVIGATION", "Setup/navigation recovery cannot run after the strict request boundary.")

    def before_refresh(self):
        self.observe()
        if self.requested:
            raise Blocked("STRICT_REFRESH", "Manual refresh after request creation cannot count as passive delivery.")
        previous = self.artifacts.get("strict_setup_refreshes", {"count": 0})
        self.journal.artifact("strict_setup_refreshes", {"count": previous["count"] + 1,
                             "process": deepcopy(self.artifacts["strict_process"]), "phase": "before_request"})

    def account(self, identity):
        self.observe()
        self.pin("strict_account", identity)
        if any(name in self.artifacts for name in RECOVERIES):
            self.reject_restart()

    def idle(self):
        if self.requested:
            self.observe()
            return
        previous = self.artifacts.get("strict_idle")
        history = list(self.artifacts.get("strict_idle_history", []))
        if previous is not None:
            history.append(deepcopy(previous))
            self.journal.artifact("strict_idle_history", history)
        proof = {"phase": "observing", "process": self.observe(), "required_seconds": IDLE_SECONDS}
        self.journal.artifact("strict_idle", proof)
        start = last = time.monotonic()
        samples, longest = 0, 0.0
        while True:
            self.observe()
            labels = self.phone.labels()
            from appium.webdriver.common.appiumby import AppiumBy
            modals = self.phone.driver.find_elements(AppiumBy.IOS_PREDICATE,
                'visible == true AND (type == "XCUIElementTypeAlert" OR type == "XCUIElementTypeSheet")')
            if (modals or not {"Home", "Cubes", "Settings", "Add signer key"} <= labels
                    or labels & {"Incoming transaction", "Enter PIN", "Enter your PIN", "Approve transaction",
                                 "Create new signer key", "Name your signer key", "Set up your keychain"}):
                raise Blocked("STRICT_IDLE", "The pre-request idle observation left public foreground Home.")
            now = time.monotonic()
            gap = now - last
            if gap < 0 or gap > MAX_OBSERVATION_GAP:
                raise Blocked("STRICT_IDLE_GAP", "Continuous idle observation was interrupted; no elapsed-time shortcut is accepted.")
            samples += 1
            longest = max(longest, gap)
            last = now
            if now - start >= IDLE_SECONDS:
                break
            time.sleep(min(1, IDLE_SECONDS - (now - start)))
        proof.update(phase="completed", observed_seconds=now - start, samples=samples, maximum_gap_seconds=longest)
        self.journal.artifact("strict_idle", proof)

    def begin_request(self, binding):
        self.observe()
        if self.requested:
            if self.artifacts["strict_request"]["binding"] != binding:
                raise Failed("STRICT_BINDING", "The resumed strict request changed its transaction or account binding.")
            return
        self.idle()  # Always a full new observation if interrupted before request dispatch.
        self.pin("strict_request", {"binding": deepcopy(binding), "process": self.observe(),
                                  "idle_sha256": digest(self.artifacts["strict_idle"]), "session_id": None})

    def bind_session(self, session_id):
        self.observe()
        boundary = deepcopy(self.artifacts["strict_request"])
        if boundary["session_id"] not in (None, session_id):
            raise Failed("STRICT_BINDING", "The strict signing session changed.")
        boundary["session_id"] = session_id
        self.journal.artifact("strict_request", boundary)

    def context(self, request_id, session_id):
        self.observe()
        boundary = self.artifacts.get("strict_request")
        idle = self.artifacts.get("strict_idle", {})
        if (not boundary or boundary["binding"]["request_id"] != request_id or boundary["session_id"] != session_id
                or boundary["process"] != self.artifacts["strict_process"]
                or boundary.get("idle_sha256") != digest(idle) or idle.get("phase") != "completed"
                or idle.get("process") != boundary["process"] or idle.get("observed_seconds", 0) < IDLE_SECONDS
                or idle.get("maximum_gap_seconds", MAX_OBSERVATION_GAP + 1) > MAX_OBSERVATION_GAP):
            raise Failed("STRICT_BINDING", "The strict UI proof is not bound to the exact request, session and process.")
        return {"version": 1, "config_binding": self.journal.data["binding"], "request": deepcopy(boundary)}

    def delivery_receipt(self, request_id, session_id):
        return {"context": self.context(request_id, session_id),
                "source": "target_device_stream_acknowledgement", "status": 2,
                "submitted_signature_count": 0, "approval_binding": deepcopy(self.artifacts.get("approval_binding"))}

    def require_delivery(self, request_id, session_id):
        if self.artifacts.get("strict_delivery") != self.delivery_receipt(request_id, session_id):
            raise Failed("STRICT_DELIVERY", "Exact target-device DELIVERED evidence must precede queue navigation.")

    def wait_delivery(self, request_id, session_id, collect, timeout):
        expected = self.delivery_receipt(request_id, session_id)
        if not expected["approval_binding"]:
            raise Failed("STRICT_BINDING", "Strict delivery requires the immutable approval binding.")
        if "strict_delivery" in self.artifacts:
            self.require_delivery(request_id, session_id)
            return
        until = time.monotonic() + timeout
        while True:
            self.observe()
            result = collect()  # Production native collection validates exact session/target/transaction.
            self.observe()
            if (result.get("ready") is not False or type(result.get("status")) is not int
                    or type(result.get("submitted_signature_count")) is not int
                    or result["submitted_signature_count"] != 0 or result["status"] not in (1, 2)):
                raise Blocked("STRICT_DELIVERY_STATE", "An advanced session without saved passive delivery evidence cannot pass strict acceptance.")
            if result["status"] == 2:
                self.pin("strict_delivery", expected)
                return
            if time.monotonic() >= until:
                raise Blocked("REQUEST_DELIVERY", "The exact target device did not acknowledge delivery before the deadline; no navigation or restart was attempted.")
            time.sleep(1)

    def _leaf_controls(self, prefix, *, button=False):
        from appium.webdriver.common.appiumby import AppiumBy
        import json
        if button:
            predicate = ('visible == true AND enabled == true AND type == "XCUIElementTypeButton" AND '
                         f'(label == {json.dumps(prefix)} OR name == {json.dumps(prefix)})')
        else:
            predicate = ('visible == true AND '
                         f'(label BEGINSWITH {json.dumps(prefix)} OR name BEGINSWITH {json.dumps(prefix)})')
        candidates = {element.id: element for element in self.phone.driver.find_elements(AppiumBy.IOS_PREDICATE, predicate)}
        leaves = [element for element in candidates.values() if not any(child.id in candidates
                  for child in element.find_elements(AppiumBy.XPATH, 'descendant::*'))]
        if len(leaves) != 1:
            raise Blocked("STRICT_HOOK", "A QA marker or trigger is missing or ambiguous; no input was sent.")
        return leaves[0]

    def marker(self, prefix):
        control = self._leaf_controls(prefix)
        values = {v for name in ("label", "name") if isinstance(v := control.get_attribute(name), str) and v.startswith(prefix)}
        if len(values) != 1:
            raise Blocked("STRICT_HOOK", "A QA marker has inconsistent accessibility values.")
        return next(iter(values))

    def read_probe(self, session_id, *, empty=True):
        self.observe()
        if empty:
            self.phone.require_empty_pin(session_id)
        scope = self.marker("qa-home-refresh-scope:")
        match = re.fullmatch(re.escape("qa-home-refresh-scope:" + session_id + ":") + r"([1-9][0-9]*)", scope)
        if not match:
            raise Blocked("STRICT_SCOPE", "The QA refresh scope is not the exact session and mounted PIN challenge.")
        status = self.marker("qa-home-refresh-status:").removeprefix("qa-home-refresh-status:")
        counts = self.marker("qa-home-refresh-counts:")
        parsed = re.fullmatch(r"qa-home-refresh-counts:requested=([01]):started=([01]):completed=([01]):failed=([01])", counts)
        if not parsed or status not in {"idle", "running", "completed", "failed", "invalidated", "timed-out"}:
            raise Blocked("STRICT_HOOK", "The QA refresh status/count contract is malformed.")
        values = tuple(map(int, parsed.groups()))
        requested, started, completed, failed = values
        if (started > requested or completed > started or failed > requested or completed + failed > 1
                or (status == "idle" and values != (0, 0, 0, 0))
                or (status == "completed" and values != (1, 1, 1, 0))):
            raise Blocked("STRICT_HOOK", "The QA refresh counters do not describe a valid correlated operation.")
        if status in {"failed", "invalidated", "timed-out"} or failed:
            raise Blocked("STRICT_HOME_REFRESH", "The correlated Home refresh failed, timed out or was invalidated.")
        return {"scope": scope, "status": status, "counts": values}

    def probe(self, request_id, session_id, timeout):
        context = self.context(request_id, session_id)
        self.require_delivery(request_id, session_id)
        saved = self.artifacts.get("strict_home_refresh")
        if saved is not None and saved.get("phase") != "completed":
            raise Blocked("STRICT_PROBE_UNCERTAIN", "An interrupted refresh probe cannot be clicked again.")
        current = self.read_probe(session_id)
        if saved is not None:
            if (saved.get("context") != context or saved.get("scope") != current["scope"]
                    or current["status"] != "completed" or current["counts"] != (1, 1, 1, 0)):
                raise Blocked("STRICT_SCOPE", "The completed refresh proof no longer matches the live PIN mount.")
            return
        if current["status"] != "idle" or current["counts"] != (0, 0, 0, 0):
            raise Blocked("STRICT_PROBE_USED", "The mounted QA refresh trigger was already used outside this run.")
        trigger = self._leaf_controls("qa-home-refresh-trigger", button=True)
        if self.read_probe(session_id) != current:
            raise Blocked("STRICT_SCOPE", "The mounted PIN challenge changed before the refresh intent was saved.")
        proof = {"phase": "attempted", "context": context, "scope": current["scope"], "counts": [0, 0, 0, 0]}
        self.journal.artifact("strict_home_refresh", proof)
        try:
            trigger.click()
        except Exception:
            raise Blocked("STRICT_PROBE_UNCERTAIN", "The QA refresh click outcome is uncertain; it will not be repeated.") from None
        until = time.monotonic() + timeout
        prior = current["counts"]
        while True:
            current = self.read_probe(session_id)
            if current["scope"] != proof["scope"] or any(new < old for new, old in zip(current["counts"], prior)):
                raise Blocked("STRICT_SCOPE", "The PIN mount changed or its correlated refresh counters regressed.")
            prior = current["counts"]
            if current["status"] == "completed":
                proof.update(phase="completed", counts=list(prior), status="completed", home_refresh_proven=True)
                self.journal.artifact("strict_home_refresh", proof)
                return
            if time.monotonic() >= until:
                raise Blocked("STRICT_HOME_REFRESH", "The correlated Home refresh did not complete before the deadline.")
            time.sleep(.25)

    def assert_live_probe(self, request_id, session_id, *, empty=True):
        """Recheck the exact mount after remote/credential calls, without clicking."""
        context = self.context(request_id, session_id)
        saved = self.artifacts.get("strict_home_refresh", {})
        current = self.read_probe(session_id, empty=empty)
        if (saved.get("context") != context or saved.get("phase") != "completed"
                or saved.get("scope") != current["scope"] or current["status"] != "completed"
                or current["counts"] != (1, 1, 1, 0)):
            raise Blocked("STRICT_SCOPE", "The completed Home-refresh proof does not match the current PIN mount.")
        return current["scope"]

    def require_completion(self, request_id, session_id):
        context = self.context(request_id, session_id)
        proof = self.artifacts.get("strict_home_refresh", {})
        approval = self.artifacts.get("approval_context")
        dispatch = self.artifacts.get("pin_dispatch_started", {})
        self.require_delivery(request_id, session_id)
        if (self.artifacts.get("approval_started") is not True
                or proof.get("status") != "completed"
                or not re.fullmatch(re.escape("qa-home-refresh-scope:" + session_id + ":") + r"[1-9][0-9]*", str(proof.get("scope", "")))
                or proof.get("context") != context or proof.get("phase") != "completed"
                or proof.get("counts") != [1, 1, 1, 0] or proof.get("home_refresh_proven") is not True
                or not approval or dispatch.get("context") != approval
                or dispatch.get("strict_scope") != proof.get("scope")
                or self.artifacts.get("strict_pin_input") != {"context": approval, "scope": proof.get("scope"), "completed": True}
                or approval.get("binding") != self.artifacts.get("approval_binding")
                or approval.get("intent_sha256") != digest(context["request"]["binding"]["intent"])
                or approval.get("fee_sats") != context["request"]["binding"]["fee_sats"]
                or approval.get("request_id") != request_id or approval.get("session_id") != session_id
                or approval.get("process") != self.artifacts["strict_process"]["process"]
                or approval.get("build") != self.phone.build or approval.get("device") != self.phone.device_digest):
            raise Failed("STRICT_PROOF_REQUIRED", "A signature alone does not prove strict passive delivery, correlated Home refresh and this run's PIN dispatch.")
