#!/usr/bin/env python3
"""Unit checks everywhere; --live-kill probes the actual Linux fixture."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

import with_test_keyring as fixture


class SupervisionTests(unittest.TestCase):
    def test_dead_daemon_is_infrastructure_failure(self):
        daemon = Mock(returncode=-9)
        daemon.poll.return_value = -9
        with self.assertRaisesRegex(fixture.FixtureFailure, "gnome-keyring-daemon exited"):
            fixture.require_original(daemon, (":1.0", 123))

    def test_replacement_owner_cannot_hide_failure(self):
        daemon = Mock()
        daemon.poll.return_value = None
        with patch.object(fixture, "service_owner", return_value=(":1.1", 456)):
            with self.assertRaisesRegex(fixture.FixtureFailure, "owner changed"):
                fixture.require_original(daemon, (":1.0", 123))

    def test_readiness_requires_original_pid_and_responsive_unique_name(self):
        daemon = Mock(pid=123)
        daemon.poll.return_value = None
        with patch.object(fixture, "service_owner", return_value=(":1.0", 123)), patch.object(fixture, "bus_call") as call:
            self.assertEqual(fixture.await_ready(daemon), (":1.0", 123))
            call.assert_called_once_with(":1.0", "/org/freedesktop/secrets", "org.freedesktop.DBus.Peer.Ping")


def live_kill():
    wrapper = Path(__file__).with_name("with_test_keyring.py").resolve()
    with tempfile.TemporaryDirectory(prefix="coincube-keyring-supervision-") as directory:
        # Exercise the real daemon on a fresh private bus. The command cannot
        # run until readiness succeeds, then kills exactly that fixture PID.
        env = dict(os.environ, COINCUBE_TEST_KEYRING_LOG=str(Path(directory) / "daemon.log"))
        result = subprocess.run(
            ["dbus-run-session", "--", sys.executable, str(wrapper), "--", sys.executable, "-c",
             "import os,signal,time; os.kill(int(os.environ['COINCUBE_TEST_KEYRING_PID']), signal.SIGKILL); print('KILLED_ORIGINAL_DAEMON', flush=True); time.sleep(10)"],
            env=env, capture_output=True, text=True, timeout=30,
        )
        assert result.returncode == 125, (result.returncode, result.stdout, result.stderr)
        assert "INFRASTRUCTURE FAILURE: gnome-keyring-daemon fixture:" in result.stderr, result.stderr
        assert "KILLED_ORIGINAL_DAEMON" in result.stdout, result.stdout + result.stderr
        print("Real gnome-keyring-daemon kill correctly failed the supervised run as infrastructure.")


if __name__ == "__main__":
    if sys.argv[1:] == ["--live-kill"]:
        live_kill()
    else:
        unittest.main()
