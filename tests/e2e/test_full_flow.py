"""Fresh disposable Cube/vault/funding/signing acceptance, explicitly selected."""
import pytest

from harness.provisioning import STAGES
from harness.state import Blocked, Failed


@pytest.mark.testnet4_live
@pytest.mark.parametrize("stage", STAGES)
def test_fresh_testnet4(stage, acceptance, request, record_property):
    __tracebackhide__ = True
    if acceptance.config["version"] != 2:
        pytest.fail("Fresh flow requires schema version 2.", pytrace=False)
    if request.config.getoption("--qa-preflight") and stage not in ("environment", "authentication", "phone_setup"):
        pytest.skip("Setup preflight selected; Cube, signer, vault and transaction stages not selected.")
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
