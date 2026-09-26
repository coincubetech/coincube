#!/usr/bin/env python3
"""Supervise a real Secret Service fixture inside a private dbus-run-session.

Exit 125 identifies fixture failure. The child command's status is otherwise
preserved. No automatic daemon restart: replacement owners invalidate the run.
"""
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time


class FixtureFailure(RuntimeError):
    pass


def bus_call(destination, path, method, *args):
    try:
        result = subprocess.run(
            ["dbus-send", "--session", "--print-reply", "--reply-timeout=1000",
             f"--dest={destination}", path, method, *args],
            capture_output=True, text=True, timeout=2,
        )
    except subprocess.TimeoutExpired as error:
        raise FixtureFailure("D-Bus did not answer within the fixture timeout") from error
    if result.returncode:
        raise FixtureFailure(result.stderr.strip() or "D-Bus request failed")
    return result.stdout


def service_owner():
    bus = ("org.freedesktop.DBus", "/org/freedesktop/DBus")
    reply = bus_call(*bus, "org.freedesktop.DBus.GetNameOwner",
                     "string:org.freedesktop.secrets")
    match = re.search(r'string "(:[^"]+)"', reply)
    if not match:
        raise FixtureFailure("D-Bus returned no Secret Service owner")
    owner = match.group(1)
    reply = bus_call(*bus, "org.freedesktop.DBus.GetConnectionUnixProcessID",
                     f"string:{owner}")
    match = re.search(r"uint32 (\d+)", reply)
    if not match:
        raise FixtureFailure("D-Bus returned no Secret Service process ID")
    return owner, int(match.group(1))


def require_original(daemon, expected):
    if daemon.poll() is not None:
        raise FixtureFailure(f"gnome-keyring-daemon exited with status {daemon.returncode}")
    owner = service_owner()
    if owner != expected:
        raise FixtureFailure(f"Secret Service owner changed from {expected} to {owner}")


def await_ready(daemon, timeout=15):
    deadline = time.monotonic() + timeout
    last_error = "Secret Service has not acquired its bus name"
    while time.monotonic() < deadline:
        if daemon.poll() is not None:
            raise FixtureFailure(f"gnome-keyring-daemon exited during startup ({daemon.returncode})")
        try:
            owner = service_owner()
            if owner[1] != daemon.pid:
                raise FixtureFailure(f"Secret Service belongs to PID {owner[1]}, not fixture PID {daemon.pid}")
            # Address the unique name, never auto-activate a replacement service.
            bus_call(owner[0], "/org/freedesktop/secrets", "org.freedesktop.DBus.Peer.Ping")
            require_original(daemon, owner)
            return owner
        except FixtureFailure as error:
            last_error = str(error)
        time.sleep(0.1)
    raise FixtureFailure(f"gnome-keyring-daemon was not ready: {last_error}")


def stop_group(process):
    if process is None:
        return
    # Kill the group even if its leader has exited, to reap a cargo test child.
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def supervise(command, log_path):
    daemon = child = None
    log_path = Path(log_path)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    try:
        if not os.environ.get("DBUS_SESSION_BUS_ADDRESS"):
            raise FixtureFailure("run the fixture inside dbus-run-session")
        with log_path.open("wb") as log:
            daemon = subprocess.Popen(
                ["gnome-keyring-daemon", "--foreground", "--unlock", "--components=secrets"],
                stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            owner = await_ready(daemon)
            env = dict(os.environ, COINCUBE_TEST_KEYRING_PID=str(daemon.pid))
            child = subprocess.Popen(command, env=env, start_new_session=True)
            while True:
                require_original(daemon, owner)
                status = child.poll()
                if status is not None:
                    require_original(daemon, owner)
                    return status if status >= 0 else 128 - status
                time.sleep(0.2)
    except (FixtureFailure, OSError) as error:
        print(f"INFRASTRUCTURE FAILURE: gnome-keyring-daemon fixture: {error}", file=sys.stderr, flush=True)
        if log_path.exists():
            print(log_path.read_text(errors="replace")[-16000:], file=sys.stderr, flush=True)
        return 125
    finally:
        stop_group(child)
        stop_group(daemon)


if __name__ == "__main__":
    def interrupted(signum, _frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, interrupted)
    command = sys.argv[1:]
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        sys.exit("usage: with_test_keyring.py -- COMMAND [ARG ...]")
    try:
        sys.exit(supervise(command, os.environ.get("COINCUBE_TEST_KEYRING_LOG", "test-logs/gnome-keyring.log")))
    except KeyboardInterrupt:
        sys.exit(130)
