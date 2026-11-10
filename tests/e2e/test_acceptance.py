"""Live API/physical-device acceptance; never part of an implicit default run."""
import pytest

from harness.runner import STAGES
from harness.state import Blocked, Failed


@pytest.mark.testnet4_live
@pytest.mark.parametrize("stage", STAGES)
def test_testnet4_acceptance(stage, acceptance, request, record_property):
    __tracebackhide__ = True
    if request.config.getoption("--qa-preflight") and STAGES.index(stage) > STAGES.index("phone"):
        pytest.skip("Read-only preflight requested; transaction stages not selected.")
    record_property("stage", stage)
    try:
        acceptance.run(stage)
    except Failed as error:
        record_property("outcome", "failed")
        record_property("code", error.code)
        pytest.fail(error.code + ": " + str(error), pytrace=False)
    except Blocked as error:
        request.config._testnet4_blocked = True
        record_property("outcome", "blocked")
        record_property("code", error.code)
        pytest.skip(error.code + ": " + str(error))
    record_property("outcome", "passed")
