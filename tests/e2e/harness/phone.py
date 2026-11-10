"""Appium/XCUITest over a USB-only WDA tunnel. No screenshots or raw UI logs."""
from contextlib import contextmanager
from copy import deepcopy
import json
import hashlib
import fcntl
import os
from pathlib import Path
import select
import signal
import socket
import socketserver
import subprocess
import sys
import threading
import tempfile
import time
from xml.etree import ElementTree

from .clients import http
from .state import Blocked, Failed, private_read

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "diagnostics"))
import ios_usb_wda as usb

BUNDLE = "io.coincube.keychainApp"


def device_query(*args):
    try:
        result = subprocess.run(["xcrun", "devicectl", *args, "--timeout", "15", "--json-output", "-"],
                                capture_output=True, timeout=20, check=True)
        return json.loads(result.stdout)["result"]
    except (OSError, subprocess.SubprocessError, ValueError, KeyError):
        raise Blocked("IOS_DEVICE", "Xcode cannot inspect the wired iPhone. Check pairing, unlock and Developer Mode.") from None


def wired_device(minimum_build):
    devices = device_query("list", "devices")["devices"]
    wired = [d for d in devices if d.get("connectionProperties", {}).get("transportType") == "wired"]
    if len(wired) != 1:
        raise Blocked("IOS_DEVICE_COUNT", "Exactly one wired iPhone is required.")
    device = wired[0]
    apps = device_query("device", "info", "apps", "--device", device["identifier"],
                        "--bundle-id", BUNDLE, "--include-removable-apps", "--include-default-apps")["apps"]
    if len(apps) != 1 or not str(apps[0].get("bundleVersion", "")).isdigit():
        raise Blocked("KEYCHAIN_INSTALL", "A versioned Keychain build must be installed on the iPhone.")
    build = int(apps[0]["bundleVersion"])
    if build < minimum_build:
        raise Blocked("KEYCHAIN_BUILD", "Installed Keychain is older than the configured minimum build.")
    return device, build


class USBProxy(socketserver.ThreadingTCPServer):
    daemon_threads = True

    def __init__(self, device):
        self.device = device
        super().__init__(("127.0.0.1", 0), USBHandler)


class USBHandler(socketserver.BaseRequestHandler):
    def handle(self):
        try:
            with usb.mux_socket() as wire:
                reply = usb.mux_request(wire, {"MessageType": "Connect", "DeviceID": self.server.device,
                                             "PortNumber": socket.htons(8100)})
                if reply.get("Number") != 0:
                    return
                sockets = (self.request, wire)
                while True:
                    readable, _, _ = select.select(sockets, [], [], 60)
                    if not readable:
                        return
                    for source in readable:
                        data = source.recv(65536)
                        if not data:
                            return
                        destination = wire if source is self.request else self.request
                        destination.sendall(data)
        except (OSError, ValueError):
            return


def stop_owned(process):
    if process is not None and process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)


@contextmanager
def attach(config):
    from appium import webdriver
    from appium.options.ios import XCUITestOptions
    from appium.webdriver.client_config import AppiumClientConfig

    device, build = wired_device(config["minimum_build"])
    runner = server = driver = proxy = None
    lease = os.open(Path(tempfile.gettempdir()) / ("coincube-testnet4-ios-" + str(os.getuid()) + ".lock"),
                    os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        if os.fstat(lease).st_uid != os.getuid() or os.fstat(lease).st_mode & 0o077:
            raise Blocked("PHONE_LOCK", "The USB automation lease file has unsafe ownership or permissions.")
        try:
            fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Blocked("PHONE_BUSY", "Another integration run owns the iPhone automation lease.") from None
        usb_device = usb.wired_phone()
        try:
            ready = usb.wda_get(usb_device, "/status").get("ready") is True
        except (OSError, ValueError):
            ready = False
        if not ready and config.get("xctestrun"):
            runner = subprocess.Popen(["xcodebuild", "test-without-building", "-xctestrun", config["xctestrun"],
                                       "-destination", "id=" + device["hardwareProperties"]["udid"], "-quiet"],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
            until = time.monotonic() + 60
            while time.monotonic() < until and runner.poll() is None:
                try:
                    ready = usb.wda_get(usb_device, "/status").get("ready") is True
                    if ready:
                        break
                except (OSError, ValueError):
                    pass
                time.sleep(1)
        if not ready:
            raise Blocked("WDA_START", "Signed WebDriverAgent is not ready over USB. Supply its xctestrun or start the existing runner.")
        proxy = USBProxy(usb_device)
        threading.Thread(target=proxy.serve_forever, daemon=True).start()
        wda_url = "http://127.0.0.1:" + str(proxy.server_address[1])
        with socket.socket() as reserve:
            reserve.bind(("127.0.0.1", 0))
            port = reserve.getsockname()[1]
        executable = config.get("appium", "appium")
        version = subprocess.run([executable, "--version"], capture_output=True, timeout=10, check=True).stdout.decode().strip()
        if version != "3.9.0":
            raise Blocked("APPIUM_VERSION", "This suite is pinned to Appium 3.9.0; install the documented tool versions.")
        drivers = json.loads(subprocess.run([executable, "driver", "list", "--installed", "--json"],
                                           capture_output=True, timeout=20, check=True).stdout)
        if drivers.get("xcuitest", {}).get("version") != "12.22.8":
            raise Blocked("XCUITEST_VERSION", "This suite is pinned to the Appium XCUITest driver 12.22.8.")
        server = subprocess.Popen([executable, "--address", "127.0.0.1", "--port", str(port), "--log-level", "error"],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        url = "http://127.0.0.1:" + str(port)
        until = time.monotonic() + 25
        while True:
            try:
                if json.loads(http(url + "/status", timeout=1))["value"]["ready"]:
                    break
            except (Blocked, ValueError, KeyError):
                pass
            if time.monotonic() > until or server.poll() is not None:
                raise Blocked("APPIUM_START", "Appium did not become ready.")
            time.sleep(.25)
        options = XCUITestOptions().load_capabilities({
            "platformName": "iOS", "appium:automationName": "XCUITest",
            "appium:udid": device["hardwareProperties"]["udid"], "appium:bundleId": BUNDLE,
            "appium:webDriverAgentUrl": wda_url, "appium:noReset": True,
            "appium:autoLaunch": False, "appium:forceAppLaunch": False,
            "appium:shouldTerminateApp": False, "appium:skipLogCapture": True,
            # Funding/confirmation and native provisioning can leave the UI
            # idle longer than a delivery timeout while this run owns USB.
            "appium:showXcodeLog": False, "appium:newCommandTimeout": 7200,
        })
        client_config = AppiumClientConfig(remote_server_addr=url, timeout=30)
        driver = webdriver.Remote(options=options, client_config=client_config)
        driver.activate_app(BUNDLE)
        phone = Phone(driver, build)
        phone.device_digest = hashlib.sha256(device["hardwareProperties"]["udid"].encode()).hexdigest()
        yield phone
    except Blocked:
        raise
    except Exception:
        raise Blocked("APPIUM_USB", "Appium USB session failed; no raw device or authentication output was saved.") from None
    finally:
        if driver is not None:
            try:
                driver.quit()
            except Exception:
                pass
        stop_owned(server)
        if proxy is not None:
            proxy.shutdown()
            proxy.server_close()
        stop_owned(runner)
        os.close(lease)


class Phone:
    def __init__(self, driver, build):
        self.driver, self.build = driver, build

    def labels(self):
        if self.driver.execute_script("mobile: activeAppInfo").get("bundleId") != BUNDLE:
            raise Blocked("PHONE_AUTH", "Keychain yielded to an OS authentication prompt; unattended signing cannot proceed.")
        root = ElementTree.fromstring(self.driver.page_source)
        labels = {value.strip() for n in root.iter() if n.get("visible") == "true"
                  for key in ("label", "name", "value") for value in n.get(key, "").splitlines()}
        if "Key limit reached" in labels:
            raise Blocked("SIGNER_LIMIT", "The QA account has reached Keychain's signer-key limit; creation requires available account capacity.")
        return labels

    def matching_controls(self, label):
        from appium.webdriver.common.appiumby import AppiumBy
        # Values are JSON-quoted rather than interpolated into predicate syntax.
        predicate = "visible == true AND enabled == true AND (label == %s OR name == %s)" % (json.dumps(label), json.dumps(label))
        return self.driver.find_elements(AppiumBy.IOS_PREDICATE, predicate)

    def click(self, label):
        elements = self.matching_controls(label)
        if len(elements) != 1:
            raise Blocked("PHONE_SELECTOR", "Expected one visible, enabled Keychain control; the UI needs a selector update.")
        elements[0].click()

    def refresh(self):
        self.driver.execute_script("mobile: swipe", {"direction": "down"})

    def matching_cards(self, title, *, container=False):
        """A known card title may be the first line of a merged AX label."""
        from appium.webdriver.common.appiumby import AppiumBy
        if not title or title.isdigit() or "\n" in title:
            raise Failed("PHONE_CARD_TITLE", "Card selection requires one nonnumeric title line.")
        exact, prefix = json.dumps(title), json.dumps(title + "\n")
        types = ('type == "XCUIElementTypeOther"' if container else
                 '(type == "XCUIElementTypeStaticText" OR type == "XCUIElementTypeButton")')
        predicate = ('visible == true AND enabled == true AND '
                     f'{types} AND '
                     f'(label == {exact} OR name == {exact} OR label BEGINSWITH {prefix} OR name BEGINSWITH {prefix})')
        return self.driver.find_elements(AppiumBy.IOS_PREDICATE, predicate)

    def card_control(self, title, timeout=15, *, container=False):
        until = time.monotonic() + timeout
        while True:
            elements = self.matching_cards(title, container=container)
            if len(elements) == 1:
                return elements[0]
            # The chooser can expose its title before hardware capability
            # checks enable the card. Wait only for absence, never ambiguity,
            # and never retry a click whose result could be uncertain.
            if len(elements) > 1 or time.monotonic() >= until:
                raise Blocked("PHONE_SELECTOR", "Expected exactly one visible, enabled card with the intended title line.")
            time.sleep(.25)

    def home_key_card(self, title, timeout=15):
        """Refresh Home's key list and reveal its exact horizontal PageView card."""
        from appium.webdriver.common.appiumby import AppiumBy
        until = time.monotonic() + timeout

        def find():
            labels = self.labels()  # Foreground/auth and public-route guard.
            if not {"Home", "Cubes", "Settings"} <= labels:
                raise Blocked("PHONE_HOME", "Signer-card discovery must remain on the public Home route.")
            candidates = self.matching_cards(title, container=True)
            if len(candidates) > 1:
                raise Blocked("PHONE_SELECTOR", "The exact signer title identifies multiple cards; no swipe or tap was sent.")
            return (candidates[0] if candidates else None), labels

        control, labels = find()
        if control is not None:
            return control
        # home.dart nests the horizontal key PageView in a vertical ListView.
        # The source-backed header proves we have reached its refresh boundary.
        for _ in range(4):
            if "Add signer key" in labels:
                break
            if time.monotonic() >= until:
                raise Blocked("PHONE_CARD_MISSING", "The exact signer card was not revealed within the bounded Home search.")
            self.driver.execute_script("mobile: swipe", {"direction": "down"})
            time.sleep(.25)
            control, labels = find()
            if control is not None:
                return control
        if "Add signer key" not in labels:
            raise Blocked("PHONE_HOME", "The Home refresh header could not be established without guessing.")
        self.refresh()  # onRefresh dispatches GetUserKeysEvent in build 38.
        time.sleep(.5)
        # A retained PageController may start at a nonzero page. Sweep both
        # directions with a hard gesture cap; never substitute another title.
        for direction in ["left"] * 8 + ["right"] * 16:
            control, labels = find()
            if control is not None:
                return control
            if "Add signer key" not in labels:
                raise Blocked("PHONE_HOME", "The public Home header disappeared during signer-card discovery.")
            if time.monotonic() >= until:
                break
            # The observed QA cards expose one merged Other container. Scope
            # a horizontal gesture to it when unique; otherwise use the public
            # Home gesture surface, guarded above, without selecting any card.
            cards = self.driver.find_elements(AppiumBy.IOS_PREDICATE,
                'visible == true AND enabled == true AND type == "XCUIElementTypeOther" AND '
                '(label BEGINSWITH "QA " OR name BEGINSWITH "QA ")')
            if len(cards) > 1:
                raise Blocked("PHONE_CAROUSEL", "The visible signer gesture container is ambiguous; no swipe was sent.")
            gesture = {"direction": direction}
            if cards:
                gesture["elementId"] = cards[0].id
            self.driver.execute_script("mobile: swipe", gesture)
            time.sleep(.25)
        control, _ = find()
        if control is not None:
            return control
        raise Blocked("PHONE_CARD_MISSING", "The exact signer card is absent after a bounded refresh and carousel search.")

    def click_card(self, title, timeout=15, *, container=False):
        self.card_control(title, timeout, container=container).click()

    def click_line(self, line):
        """Select one leaf control containing a complete merged label line."""
        from appium.webdriver.common.appiumby import AppiumBy
        if not line or line.isdigit() or "\n" in line:
            raise Failed("PHONE_LINE", "A request control requires one complete nonnumeric label line.")
        terms = []
        for attribute in ("label", "name"):
            terms.extend((f'{attribute} == {json.dumps(line)}',
                          f'{attribute} BEGINSWITH {json.dumps(line + chr(10))}',
                          f'{attribute} ENDSWITH {json.dumps(chr(10) + line)}',
                          f'{attribute} CONTAINS {json.dumps(chr(10) + line + chr(10))}'))
        predicate = ('visible == true AND enabled == true AND '
                     '(type == "XCUIElementTypeStaticText" OR type == "XCUIElementTypeButton" OR type == "XCUIElementTypeOther") AND ('
                     + ' OR '.join(terms) + ')')
        candidates = self.driver.find_elements(AppiumBy.IOS_PREDICATE, predicate)
        identifiers = {element.id for element in candidates}
        leaves = [element for element in candidates if not any(child.id in identifiers
                  for child in element.find_elements(AppiumBy.XPATH, 'descendant::*'))]
        if len(leaves) != 1:
            raise Blocked("PHONE_SELECTOR", "The exact request label line must identify one leaf control; no tap was sent.")
        leaves[0].click()

    def dismiss_incoming(self, labels):
        if {"Incoming transaction", "Review the details before approving", "View transaction", "Not now"} <= labels:
            # The app's onDismiss only pops this notification sheet. It does
            # not reject the session. Select the exact run note from the
            # authenticated queue instead of assuming this modal is our request.
            self.click("Not now")
            return True
        return False

    def restore_navigation(self):
        # noReset preserves the route left by a previous attempt. Only dismiss
        # completed results or navigate back; never approve, reject, retry, or
        # enter credentials while reconciling an interrupted run.
        restarted = False
        for _ in range(16):
            labels = self.labels()
            if self.dismiss_incoming(labels):
                pass
            elif {"Create new signer key", "Create Key", "Cancel"} <= labels:
                self.click("Cancel")
            elif {"Home", "Cubes", "Settings"} <= labels:
                return
            elif "Done" in labels and labels & {"Signature Submitted", "Transaction approved"}:
                self.click("Done")
            elif {"Signer key created", "Back to home"} <= labels:
                self.click("Back to home")
            elif "Go to home" in labels:
                self.click("Go to home")
            elif {"Signer key", "Back"} <= labels and not self.matching_controls("Back") and not restarted:
                # Key details exposes its decorative Back child as disabled
                # through XCTest. This persisted route has no pending ceremony;
                # relaunch without reset instead of guessing a parent tap.
                self.driver.terminate_app(BUNDLE)
                self.driver.activate_app(BUNDLE)
                restarted = True
            elif "Back" in labels:
                self.click("Back")
            elif "Pending Requests" in labels and not restarted:
                # This page uses an unlabeled IconButton for back. Restart
                # only this known route instead of guessing coordinates or
                # tapping an unrelated control. Flutter starts at '/', and
                # persisted login/keys are retained (noReset).
                self.driver.terminate_app(BUNDLE)
                self.driver.activate_app(BUNDLE)
                restarted = True
            time.sleep(.5)
        raise Blocked("PHONE_NAVIGATION", "Keychain could not safely return to its main navigation; no approval or PIN was retried.")

    def check_fixture(self, cube_label, timeout=0):
        self.restore_navigation()
        self.click("Settings")
        if not any(v.casefold() == "testnet4" for v in self.labels()):
            raise Failed("PHONE_NETWORK", "Keychain Settings must show Testnet4 before running the scenario.")
        self.click("Cubes")
        until = time.monotonic() + timeout
        while cube_label not in self.labels():
            if time.monotonic() >= until:
                raise Blocked("PHONE_CUBE", "The run's Cube is not visible on the phone through Connect.")
            self.refresh()
            time.sleep(1)
        self.click("Home")
        return {"build": self.build, "network": "testnet4", "cube_visible": True}

    def process_identity(self):
        info = self.driver.execute_script("mobile: activeAppInfo")
        pid = info.get("pid")
        if info.get("bundleId") != BUNDLE or type(pid) is not int or pid <= 0:
            raise Blocked("PHONE_PROCESS", "The foreground Keychain process cannot be verified.")
        return {"pid": pid, "bundle_id": BUNDLE}

    def observe_key_limit(self):
        before = self.process_identity()
        if self.build != 38:
            raise Blocked("KEY_RECOVERY_BUILD", "This recovery observation requires the reviewed Keychain build 38.")
        try:
            self.labels()
        except Blocked as error:
            if error.code != "SIGNER_LIMIT":
                raise
        else:
            raise Blocked("KEY_RECOVERY_SCREEN", "The expected capacity-refusal screen is not visible.")
        if self.process_identity() != before:
            raise Blocked("KEY_GENERATION_PROCESS", "Keychain restarted during the refusal observation.")
        return {"build": self.build, "code": "SIGNER_LIMIT", "process": before,
                "observation": "visible_capacity_modal"}

    def restart_for_key_generation(self, journal, receipt):
        if (self.build != 38 or receipt.get("creation_started") or receipt.get("pin_entered")
                or receipt.get("phase") not in {"planned", "process_stopped", "ready_for_reconciliation", "reserved"}):
            raise Blocked("KEY_GENERATION_UNCERTAIN", "The one-time key generation process boundary cannot be replayed after dispatch.")
        self.driver.terminate_app(BUNDLE)
        if int(self.driver.query_app_state(BUNDLE)) != 1:
            raise Blocked("PHONE_PROCESS", "The old Keychain process did not conclusively stop.")
        receipt["phase"] = "process_stopped"
        journal.artifact("key_generation_recovery", deepcopy(receipt))
        self.driver.activate_app(BUNDLE)
        until = time.monotonic() + 15
        while int(self.driver.query_app_state(BUNDLE)) != 4:
            if time.monotonic() >= until:
                raise Blocked("PHONE_PROCESS", "Keychain did not return to the foreground after the verified stop.")
            time.sleep(.25)
        receipt["current_process"] = self.process_identity()
        receipt["phase"] = "ready_for_reconciliation"
        journal.artifact("key_generation_recovery", deepcopy(receipt))

    def restart_for_upgrade(self, journal, receipt):
        if (self.build < 38 or self.build <= receipt["original_build"]
                or receipt["current_build"] != self.build
                or receipt["phase"] not in {"planned", "process_stopped", "ready_for_review"}):
            raise Blocked("APPROVAL_RECOVERY", "The signing upgrade recovery boundary is invalid.")
        # Repeat termination if interrupted before review. This is not another
        # approval generation; dispatch permanently consumes the receipt.
        self.driver.terminate_app(BUNDLE)
        if int(self.driver.query_app_state(BUNDLE)) != 1:  # NOT_RUNNING
            raise Blocked("PHONE_PROCESS", "Keychain termination was not verified; no approval may follow.")
        receipt["phase"] = "process_stopped"
        journal.artifact("approval_upgrade_recovery", deepcopy(receipt))
        self.driver.activate_app(BUNDLE)
        until = time.monotonic() + 15
        while int(self.driver.query_app_state(BUNDLE)) != 4:  # RUNNING_IN_FOREGROUND
            if time.monotonic() >= until:
                raise Blocked("PHONE_PROCESS", "Keychain did not return to the foreground after the verified stop.")
            time.sleep(.25)
        receipt["current_process"] = self.process_identity()
        receipt["phase"] = "ready_for_review"
        journal.artifact("approval_upgrade_recovery", deepcopy(receipt))

    @staticmethod
    def intent_digest(intent):
        return hashlib.sha256(json.dumps(intent, sort_keys=True, separators=(",", ":")).encode()).hexdigest()

    def approval_context(self, request_id, session_id, intent, fee_sats, journal):
        binding = journal.data["artifacts"].get("approval_binding")
        if not session_id or not isinstance(binding, dict) or not binding or self.build < 38:
            raise Blocked("SIGNING_SEMANTICS", "Fresh signing requires build 38 session-bound PIN accessibility and saved approval bindings.")
        return {"binding": deepcopy(binding), "request_id": request_id, "session_id": session_id,
                "intent_sha256": self.intent_digest(intent), "fee_sats": fee_sats,
                "build": self.build, "device": self.device_digest, "process": self.process_identity()}

    @staticmethod
    def reject_pin_error(labels):
        if any(any(term in value.casefold() for term in
                   ("incorrect pin", "attempts remaining", "too many attempts", "locked out")) for value in labels):
            raise Failed("PIN_REJECTED", "Keychain rejected the configured signing PIN. It will not be retried.")

    def require_empty_pin(self, session_id):
        labels = self.labels()
        self.reject_pin_error(labels)
        sessions = {v for v in labels if v.startswith("signing-pin-session:")}
        entries = {v for v in labels if v.startswith("signing-pin-entry:")}
        if ("Enter PIN" not in labels or sessions != {"signing-pin-session:" + session_id}
                or entries != {"signing-pin-entry:empty"}
                or any(len(self.matching_controls(str(digit))) != 1 for digit in range(10))):
            raise Blocked("PIN_SESSION", "The empty PIN challenge is not uniquely bound to the saved signing session.")

    def enter_first_pin(self, request_id, session_id, intent, fee_sats, journal, pin_provider, before_pin):
        artifacts = journal.data["artifacts"]
        if "pin_resume_probe" in artifacts or "pin_dispatch_started" in artifacts:
            raise Blocked("PIN_ATTEMPT_UNCERTAIN", "A prior PIN lifecycle or input attempt cannot be replayed.")
        expected = self.approval_context(request_id, session_id, intent, fee_sats, journal)
        if artifacts.get("approval_context") != expected:
            raise Blocked("PIN_PROCESS", "The pending PIN challenge does not have the original process and transaction bindings.")
        self.require_empty_pin(session_id)
        if not before_pin():
            return False
        self.verify_pin_resume(journal, session_id=session_id)
        if self.approval_context(request_id, session_id, intent, fee_sats, journal) != expected:
            raise Blocked("PIN_PROCESS", "The Keychain process changed across the PIN lifecycle probe.")
        self.require_empty_pin(session_id)
        if not before_pin():
            return False
        journal.artifact("pin_dispatch_started", {"context": deepcopy(expected)})
        # The provider persists the encrypted credential's pending-purpose
        # marker before returning the PIN. Any later exception is unretryable.
        pin = pin_provider()
        if len(pin) != 6 or not pin.isascii() or not pin.isdigit():
            raise Blocked("SIGNER_PIN", "The private signing PIN must contain six digits.")
        for digit in pin:
            self.click(digit)
        return True

    def wait_signed(self, timeout):
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            labels = self.labels()
            self.reject_pin_error(labels)
            if "Signature Submitted" in labels or "Transaction approved" in labels:
                return
            time.sleep(1)
        raise Blocked("SIGNING_TIMEOUT", "Keychain approval did not complete within the signing deadline.")

    def resume_first_pin(self, request_id, session_id, intent, fee_sats, journal, timeout, *, pin_provider, before_pin):
        # No route restoration, navigation, or Approve dispatch on this path.
        if self.enter_first_pin(request_id, session_id, intent, fee_sats, journal, pin_provider, before_pin):
            self.wait_signed(timeout)

    def verify_pin_resume(self, journal, *, session_id=None):
        # Observe lifecycle survival only. Resume reconnects the stream, but
        # does not prove Home's FetchPendingSessionsEvent actually ran.
        journal.artifact("pin_resume_probe", {"status": "started", "trigger": "background_resume"})
        self.driver.execute_script("mobile: backgroundApp", {"seconds": 1})
        self.driver.activate_app(BUNDLE)
        until = time.monotonic() + 3
        while True:
            if "Enter PIN" not in self.labels():
                journal.artifact("pin_resume_probe", {"status": "failed", "trigger": "background_resume"})
                raise Failed("PIN_CHALLENGE_DISMISSED", "The PIN challenge disappeared across app resume before any credential was entered.")
            if session_id:
                self.require_empty_pin(session_id)
            if time.monotonic() >= until:
                break
            time.sleep(.25)
        journal.artifact("pin_resume_probe", {"status": "passed", "trigger": "background_resume",
                                               "observation_seconds": 3, "home_refresh_proven": False})

    def approve(self, request_id, intent, fee_sats, pin_file, journal, timeout, *, pin_provider=None, probe_pin_resume=False,
                session_id=None, before_approve=None, before_pin=None, reapproval=False):
        artifacts = journal.data["artifacts"]
        receipt = artifacts.get("approval_upgrade_recovery", {})
        if reapproval and (not session_id or receipt.get("phase") != "ready_for_review"
                           or receipt.get("binding") != artifacts.get("approval_binding")
                           or receipt.get("current_build") != self.build
                           or self.build <= receipt.get("original_build", self.build)
                           or receipt.get("current_process") != self.process_identity()
                           or "pin_resume_probe" in artifacts or "pin_dispatch_started" in artifacts):
            raise Blocked("APPROVAL_RECOVERY", "The one-time approval recovery receipt is not valid for this process.")
        if session_id and (not callable(before_approve) or not callable(before_pin) or not callable(pin_provider)):
            raise Blocked("APPROVAL_BINDING", "Session-bound signing requires remote-state and private-credential guards.")
        if artifacts.get("approval_started") and not reapproval:
            raise Blocked("APPROVAL_UNCERTAIN", "An earlier approval attempt must be reconciled before any phone navigation.")
        note = "Testnet4 QA " + request_id
        until = time.monotonic() + timeout
        self.restore_navigation()
        self.click("Home")
        self.refresh()
        while True:
            labels = self.labels()
            if self.dismiss_incoming(labels):
                if time.monotonic() >= until:
                    raise Blocked("REQUEST_DELIVERY", "Incoming notifications prevented selection of the exact request before the deadline.")
                time.sleep(.25)
                continue
            if note in labels:
                self.click_line(note)
                break
            # The Home banner opens the authenticated pending list.
            candidates = [v for v in labels if v.endswith("signing request waiting") or v.endswith("signing requests waiting")]
            if len(candidates) == 1:
                self.click_line(candidates[0])
            if time.monotonic() >= until:
                raise Blocked("REQUEST_DELIVERY", "The exact request did not appear in Keychain before the delivery deadline.")
            time.sleep(1)
        required = {"Approve transaction", "testnet4", f"{fee_sats} sats"}
        required.update(out["address"] for out in intent["outputs"])
        required.update(f'{out["sats"]} sats' for out in intent["outputs"])
        seen = set()
        for _ in range(8):
            seen.update(self.labels())
            if {v.casefold() for v in required} <= {v.casefold() for v in seen}:
                break
            self.driver.execute_script("mobile: swipe", {"direction": "up"})
            time.sleep(.3)
        else:
            raise Failed("PHONE_REVIEW", "The phone did not display the expected network, destinations, amounts and fee.")
        # One attempted PIN entry per run, including across crashes. Never
        # guess or retry a credential: repeated failures can wipe a signer.
        if artifacts.get("approval_started") and not reapproval:
            raise Blocked("APPROVAL_UNCERTAIN", "An earlier approval attempt has an uncertain outcome; reconcile the session before trying again.")
        if session_id:
            if not before_approve():
                return
            context = self.approval_context(request_id, session_id, intent, fee_sats, journal)
            if reapproval:
                if context["process"] != receipt["current_process"]:
                    raise Blocked("APPROVAL_RECOVERY", "Keychain restarted during the recovered transaction review.")
                receipt["phase"] = "approval_dispatched"
                journal.artifact("approval_upgrade_recovery", deepcopy(receipt))
            journal.artifact("approval_context", context)
        journal.artifact("approval_started", True)
        self.click("Approve")
        until = time.monotonic() + timeout
        pin_entered = False
        while time.monotonic() < until:
            labels = self.labels()
            if "Signature Submitted" in labels or "Transaction approved" in labels:
                return
            self.reject_pin_error(labels)
            if "Enter PIN" in labels and not pin_entered:
                if session_id:
                    if not self.enter_first_pin(request_id, session_id, intent, fee_sats, journal, pin_provider, before_pin):
                        return
                    pin_entered = True
                    continue
                if probe_pin_resume:
                    self.verify_pin_resume(journal)
                if not pin_file and pin_provider is None:
                    raise Blocked("SIGNER_PIN", "This fixture needs its existing disposable signing PIN file for unattended approval.")
                pin = pin_provider() if pin_provider else private_read(pin_file).decode().strip()
                if len(pin) != 6 or not pin.isascii() or not pin.isdigit():
                    raise Blocked("SIGNER_PIN", "The private fixture signing PIN must contain six digits.")
                for digit in pin:
                    self.click(digit)
                pin_entered = True
            if any("incorrect pin" in v.casefold() or "attempts remaining" in v.casefold() or "too many attempts" in v.casefold() for v in labels):
                raise Failed("PIN_REJECTED", "Keychain rejected the configured signing PIN. It will not be retried.")
            time.sleep(1)
        raise Blocked("SIGNING_TIMEOUT", "Keychain approval did not complete within the signing deadline.")
