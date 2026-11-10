"""Appium/XCUITest over a USB-only WDA tunnel. No screenshots or raw UI logs."""
from contextlib import contextmanager
import json
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
            "appium:showXcodeLog": False, "appium:newCommandTimeout": 120,
        })
        client_config = AppiumClientConfig(remote_server_addr=url, timeout=30)
        driver = webdriver.Remote(options=options, client_config=client_config)
        driver.activate_app(BUNDLE)
        yield Phone(driver, build)
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
        return {value.strip() for n in root.iter() if n.get("visible") == "true"
                for key in ("label", "name", "value") for value in n.get(key, "").splitlines()}

    def click(self, label):
        from appium.webdriver.common.appiumby import AppiumBy
        # Values are JSON-quoted rather than interpolated into predicate syntax.
        predicate = "visible == true AND enabled == true AND (label == %s OR name == %s)" % (json.dumps(label), json.dumps(label))
        elements = self.driver.find_elements(AppiumBy.IOS_PREDICATE, predicate)
        if len(elements) != 1:
            raise Blocked("PHONE_SELECTOR", "Expected one visible, enabled Keychain control; the UI needs a selector update.")
        elements[0].click()

    def refresh(self):
        self.driver.execute_script("mobile: swipe", {"direction": "down"})

    def check_fixture(self, cube_label):
        self.click("Settings")
        if not any(v.casefold() == "testnet4" for v in self.labels()):
            raise Failed("PHONE_NETWORK", "Keychain Settings must show Testnet4 before running the scenario.")
        self.click("Cubes")
        if cube_label not in self.labels():
            raise Blocked("PHONE_CUBE", "The configured fixture Cube is not visible on the phone through Connect.")
        self.click("Home")
        return {"build": self.build, "network": "testnet4", "cube_visible": True}

    def approve(self, request_id, intent, fee_sats, pin_file, journal, timeout):
        note = "Testnet4 QA " + request_id
        until = time.monotonic() + timeout
        self.click("Home")
        self.refresh()
        while True:
            labels = self.labels()
            if note in labels:
                self.click(note)
                break
            # The Home banner opens the authenticated pending list.
            candidates = [v for v in labels if v.endswith("signing request waiting") or v.endswith("signing requests waiting")]
            if len(candidates) == 1:
                self.click(candidates[0])
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
        if journal.data["artifacts"].get("approval_started"):
            raise Blocked("APPROVAL_UNCERTAIN", "An earlier approval attempt has an uncertain outcome; reconcile the session before trying again.")
        journal.artifact("approval_started", True)
        self.click("Approve")
        until = time.monotonic() + timeout
        pin_entered = False
        while time.monotonic() < until:
            labels = self.labels()
            if "Signature Submitted" in labels or "Transaction approved" in labels:
                return
            if "Enter PIN" in labels and not pin_entered:
                if not pin_file:
                    raise Blocked("SIGNER_PIN", "This fixture needs its existing disposable signing PIN file for unattended approval.")
                pin = private_read(pin_file).decode().strip()
                if len(pin) != 6 or not pin.isascii() or not pin.isdigit():
                    raise Blocked("SIGNER_PIN", "The private fixture signing PIN must contain six digits.")
                for digit in pin:
                    self.click(digit)
                pin_entered = True
            if any("incorrect pin" in v.casefold() or "attempts remaining" in v.casefold() or "too many attempts" in v.casefold() for v in labels):
                raise Failed("PIN_REJECTED", "Keychain rejected the configured signing PIN. It will not be retried.")
            time.sleep(1)
        raise Blocked("SIGNING_TIMEOUT", "Keychain approval did not complete within the signing deadline.")
