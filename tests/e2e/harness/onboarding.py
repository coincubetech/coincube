"""Supported PIN onboarding and signer creation, without credential guessing."""
import re
import secrets
import time
from xml.etree import ElementTree

from .state import Blocked, Failed


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
        # Exclude only secure ancestors, never choose the first remaining field.
        return [field for field in candidates
                if field.get_attribute("type") != "XCUIElementTypeSecureTextField"
                or not field.find_elements(AppiumBy.XPATH,
                    'descendant::XCUIElementTypeTextField | descendant::XCUIElementTypeSecureTextField')]

    def fill_one(self, value):
        fields = self.fields()
        if len(fields) != 1:
            raise Blocked("PHONE_FIELD", "Expected exactly one visible Keychain input field.")
        fields[0].click()
        fields[0].clear()
        fields[0].send_keys(value)

    def save(self, state):
        self.store.save(state)

    def pin_attempt(self, purpose):
        state = self.store.load()
        if not state or state.get("identity") != self.identity or not state.get("verified"):
            raise Failed("PHONE_CREDENTIAL", "A verified account-bound phone credential is required.")
        if state.get("pending_attempt"):
            raise Blocked("PHONE_PIN_UNCERTAIN", "A previous PIN attempt is unresolved across runs; no further PIN will be entered.")
        state["pending_attempt"] = purpose
        self.save(state)
        return state["pin"]

    def reconcile_pin(self, purpose):
        state = self.store.load()
        if state and state.get("pending_attempt") == purpose:
            state.pop("pending_attempt")
            self.save(state)

    def reveal(self, label, direction="up"):
        from appium.webdriver.common.appiumby import AppiumBy
        for attempt in range(11):
            if label in self.phone.labels():
                return
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
        # An interruption after scrolling can leave a known recovery page at
        # its footer. Return only these unambiguous pages to their heading so
        # index/field enumeration is identical to the first attempt.
        if state and len(state.get("words", [])) == 12:
            if ("Check what you wrote down" not in labels
                    and labels & {"Confirm and finish setup", "Show me the words again"}):
                self.reveal("Check what you wrote down", direction="down")
                labels = self.phone.labels()
            elif ("Write down these 12 words" not in labels
                    and "I have written them down" in labels):
                self.reveal("Write down these 12 words", direction="down")
                labels = self.phone.labels()
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
            self.reveal("I have written them down")
            self.phone.click("I have written them down")
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
            self.reveal("Confirm and finish setup")
            state["recovery_submission_started"] = True
            self.save(state)
            self.phone.click("Confirm and finish setup")
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
