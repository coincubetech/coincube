"""No-node regressions for process cleanup and retained harness directories."""

import os
from pathlib import Path
import signal
import sys
from types import MethodType, SimpleNamespace

import pytest

from fixtures import two_chain
from test_framework.bitcoind import Bitcoind
from test_framework.electrs import Electrs
from test_framework.esplora import EsploraElectrs
from test_framework.utils import TailableProc


@pytest.mark.parametrize("wrapper", [Bitcoind, Electrs, EsploraElectrs])
def test_startup_preserves_missing_executable_error(wrapper, tmp_path):
    proc = wrapper.__new__(wrapper)
    TailableProc.__init__(proc, outputDir=str(tmp_path))
    proc.cmd_line = [str(tmp_path / "nonexistent-binary")]
    proc.start = MethodType(TailableProc.start, proc)
    with pytest.raises(FileNotFoundError):
        proc.startup()
    assert proc.proc is None
    assert proc.stop() is None


@pytest.mark.skipif(os.name == "nt", reason="SIGTERM disposition is POSIX-only")
def test_stop_kills_and_reaps_a_process_that_ignores_sigterm(tmp_path):
    proc = TailableProc(outputDir=str(tmp_path))
    proc.cmd_line = [sys.executable, "-u", "-c", (
        "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        "print('ready', flush=True); time.sleep(60)"
    )]
    proc.start()
    try:
        proc.wait_for_log("ready", timeout=10)
        assert proc.stop(timeout=0.05) == -signal.SIGKILL
        assert not proc.thread.is_alive()
    finally:
        if proc.proc.poll() is None:
            proc.kill()


def test_two_chain_instances_do_not_reuse_a_retained_directory(monkeypatch, tmp_path):
    import test_framework.btcb2 as btcb2

    class FakeHarness:
        def __init__(self, directory):
            self.directory = Path(directory)

        def setup(self):
            # A second harness must not start in an earlier module's datadir.
            (self.directory / "existing-node").mkdir()

        def cleanup(self):
            pass

    monkeypatch.setattr(btcb2, "missing_binaries", lambda: [])
    monkeypatch.setattr(btcb2, "TwoChainRegtest", FakeHarness)
    request = SimpleNamespace(session=SimpleNamespace(testsfailed=1))
    directories = []
    for _ in range(2):
        fixture = two_chain.__wrapped__(request, str(tmp_path))
        harness = next(fixture)
        directories.append(harness.directory)
        with pytest.raises(StopIteration):
            next(fixture)
        assert (harness.directory / "existing-node").is_dir()
    assert directories[0] != directories[1]
