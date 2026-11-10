"""Supported PIN onboarding and signer creation, without credential guessing."""
from copy import deepcopy
import re
import secrets
import time
from xml.etree import ElementTree

from .state import Blocked, Failed, digest


def recovery_words(xml):
    """Keep explicit word indices; a set of visible words loses column order."""
    root = ElementTree.fromstring(xml)
    labels = [n.get("label", "").strip() for n in root.iter()
              if n.get("visible") == "true" and n.get("type") == "XCUIElementTypeStaticText"]
    found = {}
    for index, label in enumerate(labels):
        match = re.fullmatch(r"(1[0-2]|[1-9])[.)]?\s+([a-z]+)", label)
        if match:
            number, word = int(match[1]), match[2]
        elif re.fullmatch(r"(1[0-2]|[1-9])[.)]?", label) and index + 1 < len(labels) and re.fullmatch(r"[a-z]+", labels[index + 1]):
            number, word = int(label.rstrip(".)")), labels[index + 1]
        else:
            continue
        if number in found and found[number] != word:
            raise Failed("RECOVERY_INDEX", "Conflicting recovery word indices on the phone.")
        found[number] = word
    if set(found) != set(range(1, 13)):
        raise Blocked("RECOVERY_WORDS", "All twelve indexed recovery words must be accessible before continuing.")
    return [found[number] for number in range(1, 13)]


class Onboarding:
    def __init__(self, phone, store, identity, deadline):
        self.phone, self.deadline = phone, deadline
        self.store = store
        self.identity = identity + ":" + phone.device_digest

    def wait(self, *choices):
        until = time.monotonic() + self.deadline
        while True:
            labels = self.phone.labels()
            if any(label in labels for label in choices):
                return labels
            if time.monotonic() >= until:
                raise Blocked("PHONE_FLOW", "The expected Keychain setup screen did not appear.")
            time.sleep(.5)

    def fields(self):
        from appium.webdriver.common.appiumby import AppiumBy
        candidates = self.phone.driver.find_elements(AppiumBy.IOS_PREDICATE,
            'visible == true AND enabled == true AND (type == "XCUIElementTypeTextField" OR type == "XCUIElementTypeSecureTextField")')
        # iOS screenshot protection wraps Flutter in a secure text field. Its
        # forwarded AX children are editable; the container itself is not.
        # The same wrapper becomes a plain TextField when protection is off.
        # Exclude input ancestors of either type, never choose the first match.
        return [field for field in candidates
                if not field.find_elements(AppiumBy.XPATH,
                    'descendant::XCUIElementTypeTextField | descendant::XCUIElementTypeSecureTextField')]

    def fill_one(self, value):
        try:
            fields = self.fields()
        except Exception:
            raise Blocked("PHONE_INPUT_QUERY", "Keychain input discovery failed; no input was sent.") from None
        if len(fields) != 1:
            raise Blocked("PHONE_FIELD", "Expected exactly one visible Keychain input field.")
        for code, operation in (("FOCUS", fields[0].click), ("CLEAR", fields[0].clear)):
            try:
                operation()
            except Exception:
                raise Blocked("PHONE_INPUT_" + code, "Keychain input preparation failed; no value was sent.") from None
        try:
            fields[0].send_keys(value)
        except Exception:
            raise Blocked("PHONE_INPUT_DISPATCH", "Keychain input delivery is uncertain; no automatic replay is permitted.") from None

    def save(self, state):
        self.store.save(state)

    def pin_attempt(self, purpose):
        state = self.store.load()
        if not state or state.get("identity") != self.identity or not state.get("verified"):
            raise Failed("PHONE_CREDENTIAL", "A verified account-bound phone credential is required.")
        reservation = state.get("key_generation_reservation")
        if reservation is not None and not isinstance(reservation, dict):
            raise Failed("KEY_GENERATION_CHANGED", "The protected generation reservation is malformed.")
        if reservation and reservation.get("status") != "completed":
            raise Blocked("KEY_GENERATION_RESERVED", "Only the explicitly reserved key generation may use this credential.")
        if state.get("pending_attempt"):
            raise Blocked("PHONE_PIN_UNCERTAIN", "A previous PIN attempt is unresolved across runs; no further PIN will be entered.")
        state["pending_attempt"] = purpose
        self.save(state)
        return state["pin"]

    def reconcile_pin(self, purpose):
        state = self.store.load()
        if state and state.get("pending_attempt") == purpose:
            reservation = state.get("key_generation_reservation")
            if reservation and reservation.get("status") != "completed":
                if reservation.get("status") != "consumed" or reservation.get("purpose") != purpose:
                    raise Failed("KEY_GENERATION_CHANGED", "The pending credential does not match its reserved generation.")
                reservation["status"] = "completed"
            state.pop("pending_attempt")
            self.save(state)

    @staticmethod
    def quarantine_record(receipt):
        binding = receipt["binding"]
        return {"recovery_id": receipt["recovery_id"], "purpose": binding["original_purpose"],
                "disposition": "outcome_unresolved", "binding_sha256": digest(binding),
                "authorization": binding["authorization"],
                "original_credential_revision": binding["original_credential_revision"]}

    def reserve_key_generation(self, receipt):
        binding = receipt["binding"]
        state = self.store.load()
        if not state or state.get("identity") != self.identity or not state.get("verified") or binding.get("identity") != self.identity:
            raise Failed("PHONE_CREDENTIAL", "Recovery requires the original verified account/device credential.")
        expected = {"recovery_id": receipt["recovery_id"], "binding_sha256": digest(binding),
                    "purpose": binding["new_purpose"]}
        quarantine = self.quarantine_record(receipt)
        reservation = state.get("key_generation_reservation")
        if reservation is not None:
            history = state.get("quarantined_attempts", [])
            if (not isinstance(reservation, dict) or any(reservation.get(k) != v for k, v in expected.items())
                    or reservation.get("status") not in {"reserved", "consumed", "completed"}
                    or history != [quarantine]):
                raise Failed("KEY_GENERATION_CHANGED", "The encrypted recovery history or reservation changed.")
            if ((reservation["status"] == "reserved" and "pending_attempt" in state)
                    or (reservation["status"] == "consumed" and state.get("pending_attempt") != binding["new_purpose"])):
                raise Failed("KEY_GENERATION_CHANGED", "The encrypted reservation and pending attempt disagree.")
            return reservation["status"]  # Never clear a newer pending attempt.
        if (receipt.get("phase") != "ready_for_reconciliation"
                or self.store.sha256 != binding["original_credential_revision"]
                or state.get("pending_attempt") != binding["original_purpose"]
                or state.get("quarantined_attempts")
                or not binding.get("authorization")
                or binding["new_purpose"] == binding["original_purpose"]):
            raise Blocked("KEY_GENERATION_UNCERTAIN", "The original credential revision and one-time recovery authorization must still match.")
        # One encrypted CAS both preserves the unresolved attempt and reserves
        # its replacement. The credential never becomes generally available.
        state["quarantined_attempts"] = [quarantine]
        state.pop("pending_attempt")
        state["key_generation_reservation"] = {**expected, "status": "reserved"}
        self.save(state)
        return "reserved"

    def reserved_key_pin(self, receipt):
        binding = receipt["binding"]
        state = self.store.load()
        reservation = state.get("key_generation_reservation") if state else None
        if (not state or state.get("identity") != self.identity or not state.get("verified")
                or binding.get("identity") != self.identity
                or state.get("quarantined_attempts") != [self.quarantine_record(receipt)]
                or "pending_attempt" in state or not isinstance(reservation, dict)
                or reservation.get("recovery_id") != receipt["recovery_id"]
                or reservation.get("binding_sha256") != digest(binding)
                or reservation.get("purpose") != binding["new_purpose"]
                or reservation.get("status") != "reserved"
                or receipt.get("phase") != "creation_dispatched" or receipt.get("pin_entered") is not True):
            raise Blocked("KEY_GENERATION_UNCERTAIN", "This exact generation has no unused credential reservation.")
        pin = state.get("pin")
        if not isinstance(pin, str) or len(pin) != 6 or not pin.isascii() or not pin.isdigit():
            raise Failed("PHONE_CREDENTIAL", "The protected credential is malformed.")
        reservation["status"] = "consumed"
        state["pending_attempt"] = binding["new_purpose"]
        self.save(state)  # Atomic intent before returning any credential bytes.
        return pin

    def reveal(self, label, direction="up"):
        from appium.webdriver.common.appiumby import AppiumBy
        for attempt in range(11):
            # Retain the active-app/auth-prompt check, but ancestor text is not
            # proof that the actual button is visible and enabled.
            self.phone.labels()
            controls = self.phone.matching_controls(label)
            if len(controls) == 1:
                return controls[0]
            if len(controls) > 1:
                raise Blocked("PHONE_SELECTOR", "The intended setup control is ambiguous; no tap was sent.")
            if attempt == 10:
                break
            scrolls = self.phone.driver.find_elements(AppiumBy.IOS_PREDICATE,
                'visible == true AND enabled == true AND type == "XCUIElementTypeScrollView"')
            if len(scrolls) != 1:
                raise Blocked("PHONE_SCROLL", "Expected one visible setup scroll view; no unscoped gesture was sent.")
            self.phone.driver.execute_script("mobile: swipe", {"direction": direction, "elementId": scrolls[0].id})
            time.sleep(.3)
        raise Blocked("PHONE_CONTROL", "The expected setup control is not visible after bounded scrolling.")

    @staticmethod
    def recovery_submitted(state):
        return bool(state and state.get("recovery_submission_started")
                    and len(state.get("words", [])) == 12
                    and state.get("entered_Create a PIN") and state.get("entered_Confirm your PIN"))

    def resume_uncommitted_setup(self, state, labels):
        """Archive one lost, uncommitted generation; never retry an unlock.

        Keychain's ConfirmInitialPin generates only RAM state. Its first blob,
        PIN and entropy writes occur after ConfirmRecoveryPhraseBackedUp.
        A cold launch can therefore return to the fresh chooser without a
        master. Do not apply that reasoning to a partial keypad, saved phrase,
        submitted challenge, existing master or an unresolved unlock attempt.
        """
        markers = ("entered_Create a PIN", "entered_Confirm your PIN")
        if not any(state.get(key) for key in markers):
            return
        chooser = {"Set up your keychain", "Use a PIN", "Use a passkey", "I have a recovery phrase"}
        history = state.get("abandoned_setup_generations", [])
        if (not chooser <= labels or not all(state.get(key) is True for key in markers)
                or state.get("words") or state.get("recovery_submission_started")
                or state.get("verified") or state.get("pending_attempt") or history):
            raise Blocked("SETUP_PIN_UNCERTAIN", "Only one completed, uncommitted PIN setup may restart from the genuine fresh chooser.")
        state["abandoned_setup_generations"] = [{
            "reason": "fresh_chooser_before_recovery_submission",
            "observed_build": self.phone.build,
            "recorded_at": int(time.time()),
            **{key: state[key] for key in markers},
        }]
        for key in markers:
            state.pop(key)
        # Same encrypted PIN, same account/device binding. Save the history
        # before any UI action; an interruption cannot erase the prior attempt.
        self.save(state)

    def setup(self):
        labels = self.phone.labels()
        state = self.store.load()
        if state and state.get("identity") != self.identity:
            raise Failed("PHONE_IDENTITY", "Saved phone credentials belong to a different account or device.")
        if state and state.get("verified"):
            self.phone.restore_navigation()
            return state["pin"]
        if {"Home", "Cubes", "Settings"} <= labels:
            # SetupDone can auto-route Home before a restarted process sees
            # it. Only recover a ceremony whose words and challenge-submit
            # intent were already durably saved by this account/device.
            if self.recovery_submitted(state):
                state["verified"] = True
                self.save(state)
                return state["pin"]
            if not state or not state.get("verified"):
                raise Blocked("PHONE_MASTER_NOT_OWNED", "This signed-in Keychain already has a master that this run did not provision. It will not replace the master, guess a PIN, or bypass a passkey prompt.")
            return state["pin"]
        if (state and state.get("recovery_submission_started")
                and "Go to home" not in labels):
            raise Blocked("RECOVERY_SUBMISSION_UNCERTAIN", "An earlier recovery submission is unresolved; no answers or confirmation will be replayed.")
        # An interruption before submission can leave a known recovery page at
        # its footer. Return only these unambiguous pages to their heading.
        if state and len(state.get("words", [])) == 12:
            if ("Check what you wrote down" not in labels
                    and labels & {"Confirm and finish setup", "Show me the words again"}):
                self.reveal("Check what you wrote down", direction="down")
                labels = self.phone.labels()
            elif ("Write down these 12 words" not in labels
                    and "I have written them down" in labels):
                self.reveal("Write down these 12 words", direction="down")
                labels = self.phone.labels()
        # Only the genuine first-time setup route is eligible. In particular,
        # never click an existing-keys warning, recovery reset, or restore flow.
        if not state:
            if "Use a PIN" not in labels:
                raise Blocked("PHONE_SETUP_REQUIRED", "A signed-in fresh QA keychain must expose its normal Use a PIN setup route.")
            state = {"identity": self.identity, "pin": f"{secrets.randbelow(1_000_000):06d}", "verified": False}
            self.save(state)
        if "Use a PIN" in labels:
            self.resume_uncommitted_setup(state, labels)
            self.phone.click_card("Use a PIN")
            labels = self.wait("Create a PIN")
        for title, next_title in (("Create a PIN", "Confirm your PIN"), ("Confirm your PIN", "Write down these 12 words")):
            if title in labels:
                # A partial keypad entry cannot be reconstructed safely.
                key = "entered_" + title
                if state.get(key):
                    raise Blocked("SETUP_PIN_UNCERTAIN", "An interrupted setup PIN entry cannot be repeated automatically.")
                state[key] = True
                self.save(state)
                for digit in state["pin"]:
                    self.phone.click(digit)
                labels = self.wait(next_title)
        if "Write down these 12 words" in labels:
            words = recovery_words(self.phone.driver.page_source)
            if state.get("words") and state["words"] != words:
                raise Failed("RECOVERY_CHANGED", "Recovery material changed during an interrupted setup.")
            state["words"] = words
            self.save(state)
            self.reveal("I have written them down").click()
            labels = self.wait("Check what you wrote down")
        if "Check what you wrote down" in labels:
            if not state.get("words"):
                raise Blocked("RECOVERY_MISSING", "The generated phone recovery material was not durably saved.")
            root = ElementTree.fromstring(self.phone.driver.page_source)
            numbers = [int(m[1]) for n in root.iter() if n.get("visible") == "true"
                       for m in [re.fullmatch(r"Word (1[0-2]|[1-9])", n.get("label", ""))] if m]
            # Accessibility can repeat ancestor labels; preserve ordered unique indices.
            numbers = list(dict.fromkeys(numbers))
            fields = self.fields()
            if len(numbers) != 3 or len(fields) != 3:
                raise Blocked("RECOVERY_CHALLENGE", "The three recovery challenge fields must be unambiguous.")
            for field, number in zip(fields, numbers):
                field.clear()
                field.send_keys(state["words"][number - 1])
            button = self.reveal("Confirm and finish setup")
            state["recovery_submission_started"] = True
            state["recovery_dispatch_status"] = "dispatch_pending"
            self.save(state)
            self.phone.labels()  # Stop if an OS/auth prompt took foreground.
            button.click()  # Exactly one dispatch; every click error is uncertain.
            labels = self.wait("Go to home", "Home")
        if "Go to home" in labels or {"Home", "Cubes", "Settings"} <= labels:
            if not self.recovery_submitted(state):
                raise Blocked("RECOVERY_UNVERIFIED", "Setup completion requires the saved phrase, both PIN entries and recovery submission intent.")
            state["verified"] = True
            self.save(state)
            if "Go to home" in labels:
                self.phone.click("Go to home")
                self.wait("Home")
            return state["pin"]
        raise Blocked("PHONE_SETUP_STATE", "Keychain setup did not reach its verified recovery completion state.")

    def create_key(self, name, pin, journal):
        self.phone.restore_navigation()
        self.phone.click("Home")
        for _ in range(8):
            if "Add signer key" in self.phone.labels():
                break
            self.phone.driver.execute_script("mobile: swipe", {"direction": "up"})
            time.sleep(.3)
        self.phone.click("Add signer key")
        labels = self.wait("Create new signer key", "Name your signer key")
        if "Create new signer key" in labels:
            self.phone.click("Create Key")
            self.wait("Name your signer key")
        self.fill_one(name)
        journal.artifact("phone_key_creation_started", True)
        self.phone.click("Create signer key")
        labels = self.wait("Enter your PIN", "Signer key created")
        if "Enter your PIN" in labels:
            if journal.data["artifacts"].get("phone_key_pin_entered"):
                raise Blocked("KEY_PIN_UNCERTAIN", "A previous signer-key PIN attempt must be reconciled through Connect before any retry.")
            journal.artifact("phone_key_pin_entered", True)
            self.fill_one(self.pin_attempt("key:" + name))
            self.phone.click("Unlock")
            self.wait("Signer key created")
        self.phone.click("Back to home")
        self.wait("Home")

    def attach_key(self, name, cube_name, journal):
        """Use Keychain's attach-and-reseal flow; no credential is required."""
        self.phone.restore_navigation()
        self.phone.click("Home")
        self.phone.click_card(name, container=True)
        self.wait("Signer key")
        control = self.reveal("Attach to Cube")
        control.click()
        self.wait("Pick a cube on the testnet4 network.")
        # The selection dispatches both binding and public-key resealing.
        # An uncertain response is reconciled remotely, never tapped again.
        control = self.phone.card_control(cube_name)
        journal.artifact("phone_key_attachment_started", True)
        control.click()

    def create_recovery_key(self, name, journal, receipt, *, before_create, before_pin):
        if (receipt.get("phase") != "reserved" or receipt.get("creation_started")
                or receipt.get("pin_entered") or name != receipt["binding"]["new_key_label"]
                or self.phone.process_identity() != receipt["current_process"]):
            raise Blocked("KEY_GENERATION_UNCERTAIN", "The one-time key generation cannot be started again.")
        self.phone.restore_navigation()
        self.phone.click("Home")
        for _ in range(8):
            if "Add signer key" in self.phone.labels():
                break
            self.phone.driver.execute_script("mobile: swipe", {"direction": "up"})
            time.sleep(.3)
        self.phone.click("Add signer key")
        labels = self.wait("Create new signer key", "Name your signer key")
        if "Create new signer key" in labels:
            self.phone.click("Create Key")
            self.wait("Name your signer key")
        self.fill_one(name)
        name_fields = self.fields()
        if len(name_fields) != 1 or name_fields[0].get_attribute("value") != name:
            raise Blocked("KEY_GENERATION_NAME", "The new key name did not match its reserved generation before dispatch.")
        before_create()
        if self.phone.process_identity() != receipt["current_process"]:
            raise Blocked("KEY_GENERATION_PROCESS", "The key-creation process changed before dispatch.")
        receipt["creation_started"] = True
        receipt["phase"] = "creation_dispatched"
        journal.artifact("key_generation_recovery", deepcopy(receipt))
        self.phone.click("Create signer key")
        labels = self.wait("Enter your PIN", "Signer key created")
        if "Enter your PIN" not in labels:
            raise Blocked("KEY_GENERATION_AUTH", "The authorized PIN-backed generation did not reach its normal PIN challenge.")
        self.resume_recovery_key_pin(journal, receipt, before_pin=before_pin)

    def resume_recovery_key_pin(self, journal, receipt, *, before_pin):
        """Continue only the first input of this still-running new generation."""
        if (receipt.get("phase") != "creation_dispatched" or receipt.get("creation_started") is not True
                or receipt.get("pin_entered") is not False or self.phone.build != 38
                or self.phone.process_identity() != receipt.get("current_process")):
            raise Blocked("KEY_GENERATION_UNCERTAIN", "Only the untouched challenge in the original generation process may continue.")
        # In creation_dispatched, this call can only validate an existing
        # reservation; it cannot create one or release the old quarantine.
        if self.reserve_key_generation(receipt) != "reserved":
            raise Blocked("KEY_GENERATION_UNCERTAIN", "The generation credential reservation was already consumed.")
        labels = self.phone.labels()
        if "Enter your PIN" not in labels:
            raise Blocked("KEY_GENERATION_AUTH", "The original generation's PIN challenge is no longer visible.")
        self.phone.reject_pin_error(labels)
        if any(term in value.casefold() for value in labels for term in
               ("wrong pin", "locked", "try again", "re-verified", "binding", "wiped", "restore")):
            raise Blocked("KEY_GENERATION_AUTH", "The new PIN challenge reports an authentication prerequisite.")
        fields = self.fields()
        # Build 38's _UnlockPinSheet creates an empty TextEditingController.
        # WDA omits value for that empty Flutter leaf. None is accepted ONLY
        # here, after same-process/no-input/unused-reservation proof above;
        # it is not a general assertion that obscured inputs are empty.
        if len(fields) != 1 or fields[0].get_attribute("value") not in (None, "", "6-digit PIN"):
            raise Blocked("KEY_GENERATION_INPUT", "The new credential field is not provably empty and unique.")
        if self.phone.process_identity() != receipt["current_process"]:
            raise Blocked("KEY_GENERATION_PROCESS", "The new PIN challenge belongs to a different process.")
        before_pin()
        if (self.phone.process_identity() != receipt["current_process"]
                or "Enter your PIN" not in self.phone.labels()
                or fields[0].get_attribute("value") not in (None, "", "6-digit PIN")):
            raise Blocked("KEY_GENERATION_INPUT", "The exact empty new challenge changed before credential dispatch.")
        receipt["pin_entered"] = True
        journal.artifact("key_generation_recovery", deepcopy(receipt))
        pin = self.reserved_key_pin(receipt)
        # The new prompt is already empty. Never clear or replay uncertain input.
        try:
            fields[0].click()
            fields[0].send_keys(pin)
        except Exception:
            raise Blocked("PHONE_INPUT_DISPATCH", "The authorized new PIN input is uncertain and will not be repeated.") from None
        self.phone.click("Unlock")
        self.wait("Signer key created")
        self.phone.click("Back to home")
        self.wait("Home")
