#!/usr/bin/env python3
"""Check an existing WebDriverAgent over USB without opening an XCTest session."""
import http.client
import json
import plistlib
import socket
import struct
import sys
from contextlib import closing

MAX_RESPONSE = 1_000_000
USBMUX_HEADER = struct.Struct("<IIII")
KEYCHAIN_BUNDLE = "io.coincube.keychainApp"


def mux_socket():
    sock = socket.socket(socket.AF_UNIX)
    try:
        sock.settimeout(8)
        sock.connect("/var/run/usbmuxd")
    except OSError:
        sock.close()
        raise
    return sock


def mux_request(sock, payload):
    body = plistlib.dumps(payload)
    sock.sendall(USBMUX_HEADER.pack(USBMUX_HEADER.size + len(body), 1, 8, 1) + body)
    with sock.makefile("rb") as reader:
        size, version, message, tag = USBMUX_HEADER.unpack(reader.read(USBMUX_HEADER.size))
        if version != 1 or message != 8 or tag != 1:
            raise ValueError("unexpected USB device service response")
        body_size = size - USBMUX_HEADER.size
        if not 0 < body_size <= MAX_RESPONSE:
            raise ValueError("invalid USB device service response size")
        return plistlib.loads(reader.read(body_size))


def wired_phone():
    with closing(mux_socket()) as sock:
        devices = mux_request(sock, {"MessageType": "ListDevices"})["DeviceList"]
    wired = [device for device in devices if device["Properties"]["ConnectionType"] == "USB"]
    if len(wired) != 1:
        raise ValueError(f"expected one wired iOS device, found {len(wired)}")
    return wired[0]["DeviceID"]


def wda_get(device, route):
    with closing(mux_socket()) as sock:
        result = mux_request(sock, {
            "MessageType": "Connect",
            "DeviceID": device,
            "PortNumber": socket.htons(8100),
        })
        if result["Number"] != 0:
            raise ValueError(f"USB connection to WebDriverAgent port 8100 failed ({result['Number']})")
        sock.sendall(f"GET {route} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".encode())
        with closing(http.client.HTTPResponse(sock)) as response:
            response.begin()
            if response.status != 200:
                raise ValueError(f"WebDriverAgent returned HTTP {response.status}")
            raw = response.read(MAX_RESPONSE + 1)
            if len(raw) > MAX_RESPONSE:
                raise ValueError("WebDriverAgent response too large")
            return json.loads(raw)["value"]


def main():
    try:
        device = wired_phone()
        if wda_get(device, "/status").get("ready") is not True:
            raise ValueError("WebDriverAgent reports that it is unready")
        app = wda_get(device, "/wda/activeAppInfo")
        if app.get("bundleId") != KEYCHAIN_BUNDLE or not app.get("pid"):
            raise ValueError("WebDriverAgent is reachable but Keychain is not observable in the foreground")
    except (OSError, http.client.HTTPException, KeyError, ValueError, struct.error, plistlib.InvalidFileException) as error:
        # Validation errors above contain only fixed text and numeric statuses.
        # Other exceptions may include raw response fragments or local details.
        message = str(error) if type(error) is ValueError else type(error).__name__
        print(f"USB Keychain preflight blocked: {message}", file=sys.stderr)
        return 1
    print("WebDriverAgent is ready over USB and reports Keychain in the foreground.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
