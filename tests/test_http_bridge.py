"""Node-free checks that background bridge assertions cannot pass silently."""
from http.client import RemoteDisconnected
from types import SimpleNamespace
from urllib.error import HTTPError
from urllib.request import Request, urlopen

import pytest

from test_btcb2_claim_gui import Bridge as ClaimBridge
from test_btcb2_split_observation import Bridge as SplitBridge


@pytest.mark.parametrize("bridge_type", [ClaimBridge, SplitBridge])
@pytest.mark.parametrize("method", ["GET", "POST"])
def test_bridge_assertion_fails_teardown(bridge_type, method):
    harness = SimpleNamespace(legacy=None, blake2b=None,
                              electrs_legacy=None, electrs_blake2b=None)
    bridge = bridge_type(harness)
    try:
        # A caller may tolerate a failed HTTP request (for example a retrying
        # GUI). That must not conceal the handler's failed test assertion.
        request = Request(bridge.url + "/invalid-route", method=method)
        try:
            urlopen(request, timeout=5).close()
        except (HTTPError, RemoteDisconnected):
            pass
    finally:
        with pytest.raises(AssertionError, match="AssertionError"):
            bridge.close()
    assert not bridge.thread.is_alive()
    assert any("AssertionError" in error for error in bridge.errors)


@pytest.mark.parametrize("bridge_type", [ClaimBridge, SplitBridge])
def test_bridge_clean_shutdown(bridge_type):
    bridge = bridge_type(SimpleNamespace())
    bridge.close()
    assert not bridge.thread.is_alive()
    assert not bridge.errors
