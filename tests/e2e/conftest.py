import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent))
from harness.runner import Runner, load_config
from harness.state import Blocked, Failed


def pytest_addoption(parser):
    group = parser.getgroup("testnet4")
    group.addoption("--qa-config", help="Private JSON fixture configuration (mode 600).")
    group.addoption("--qa-run-dir", help="Persistent private checkpoints (mode 700).")
    group.addoption("--qa-preflight", action="store_true", help="Stop after read-only environment/phone checks and the disposable Cube test.")
    group.addoption("--qa-deadline", type=int, default=90, help="Bounded delivery/propagation wait in seconds.")
    group.addoption("--qa-confirm", action="store_true", help="Require a mined confirmation instead of mempool acceptance.")


def pytest_configure(config):
    config.addinivalue_line("markers", "testnet4_live: real USB iPhone and production Connect using disposable Testnet4 fixtures")
    config._testnet4_blocked = False


@pytest.fixture(scope="session")
def acceptance(request):
    __tracebackhide__ = True
    path = request.config.getoption("--qa-config")
    if not path:
        pytest.skip("Testnet4 live suite is opt-in; supply --qa-config and --qa-run-dir.")
    directory = request.config.getoption("--qa-run-dir")
    deadline = request.config.getoption("--qa-deadline")
    if not directory or not 1 <= deadline <= 3600:
        pytest.fail("Specify a persistent --qa-run-dir and a deadline of 1–3600 seconds.", pytrace=False)
    if getattr(request.config.option, "numprocesses", None):
        pytest.fail("The live suite requires one serial worker; do not use pytest-xdist.", pytrace=False)
    try:
        runner = Runner(load_config(path), directory, deadline=deadline, confirm=request.config.getoption("--qa-confirm"))
    except Failed as error:
        pytest.fail(error.code + ": " + str(error), pytrace=False)
    except Blocked as error:
        request.config._testnet4_blocked = True
        pytest.skip(error.code + ": " + str(error))
    except Exception:
        request.config._testnet4_blocked = True
        pytest.skip("FIXTURE_CONFIG: private fixture configuration could not be loaded.")
    yield runner
    runner.journal.close()


@pytest.hookimpl(trylast=True)
def pytest_sessionfinish(session, exitstatus):
    # pytest normally treats skips as success. A selected live run with a
    # missing prerequisite must instead be non-green to unattended callers.
    if session.config._testnet4_blocked and exitstatus == 0:
        session.exitstatus = 2


def pytest_terminal_summary(terminalreporter, exitstatus, config):
    if config._testnet4_blocked:
        terminalreporter.write_line("TESTNET4 BLOCKED — checkpoint preserved; fix the reported prerequisite and repeat the same command.")
