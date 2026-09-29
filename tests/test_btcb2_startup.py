"""No-node diagnostics regressions for the two-chain indexer fixture."""
import sys
from unittest.mock import Mock

import pytest

from test_framework.btcb2 import (
    PARASITE_CAT21_LOCKTIME,
    TwoChainRegtest,
    send_to_address,
)
from test_framework.esplora import EsploraElectrs
from test_framework.utils import TailableProc


def node():
    result = Mock()
    result.rpc.getblockcount.return_value = 3
    result.rpc.getblockhash.side_effect = lambda height: f"hash-{height}"
    result.rpc.getbestblockhash.return_value = "hash-3"
    result.rpc.getblock.return_value = "00" * 81
    return result


def test_preflight_reads_all_history_including_genesis():
    source = node()
    TwoChainRegtest._preflight_block_data(source, "knots-blake2b")
    assert [c.args for c in source.rpc.getblock.call_args_list] == [
        (f"hash-{height}", 0) for height in range(4)
    ]


@pytest.mark.parametrize("malformed", [False, True])
def test_preflight_identifies_the_actual_historical_failure(malformed):
    source = node()
    def read(block_hash, _verbosity):
        if block_hash == "hash-1":
            if malformed:
                return "not-hex"
            raise ValueError("Block not found on disk")
        return "00" * 81
    source.rpc.getblock.side_effect = read
    with pytest.raises(RuntimeError, match="knots-blake2b.*height 1, hash hash-1, tip 3"):
        TwoChainRegtest._preflight_block_data(source, "knots-blake2b")
    assert source.rpc.getblock.call_count == 2


def test_preflight_refuses_chain_change():
    source = node()
    source.rpc.getbestblockhash.return_value = "replacement-tip"
    with pytest.raises(RuntimeError, match="chain changed"):
        TwoChainRegtest._preflight_block_data(source, "knots-legacy")


def test_indexer_panic_is_in_startup_diagnostic(tmp_path, caplog):
    process = EsploraElectrs.__new__(EsploraElectrs)
    TailableProc.__init__(process, str(tmp_path), verbose=False)
    process.prefix = "electrs-blake2b"
    process.cmd_line = [sys.executable, "-u", "-c",
                        "import sys; print('fetcher panic: Block not found on disk', file=sys.stderr); sys.exit(1)"]
    with pytest.raises(ValueError) as failure:
        process.startup()
    assert "Process died" in str(failure.value)
    assert "electrs-blake2b startup failed" in caplog.text
    assert "fetcher panic: Block not found on disk" in caplog.text
    if hasattr(failure.value, "__notes__"):
        assert "Block not found on disk" in "\n".join(failure.value.__notes__)
    assert process.proc.poll() == 1


def test_cleanup_failure_does_not_hide_startup_error(tmp_path, caplog):
    process = EsploraElectrs.__new__(EsploraElectrs)
    TailableProc.__init__(process, str(tmp_path), verbose=False)
    process.prefix = "electrs-blake2b"
    process.start = Mock(side_effect=FileNotFoundError("missing indexer"))
    process.stop = Mock(side_effect=RuntimeError("cleanup failed"))
    with pytest.raises(FileNotFoundError, match="missing indexer"):
        process.startup()
    assert "Cleanup also failed: cleanup failed" in caplog.text


def test_indexer_startup_checks_both_nodes_before_launch():
    harness = TwoChainRegtest.__new__(TwoChainRegtest)
    harness.legacy = node()
    harness.blake2b = node()
    harness.blake2b.rpc.getblock.side_effect = ValueError("unreadable fork block")
    # No indexer paths exist: reaching construction before preflight is a failure.
    with pytest.raises(RuntimeError, match="knots-blake2b.*height 0"):
        harness._start_indexers()
    assert harness.legacy.rpc.getblock.call_count == 4
    assert harness.blake2b.rpc.getblock.call_count == 1


def funding_node(tip, funded_locktime=None):
    result = Mock()
    result.rpc.getblockcount.return_value = tip
    result.rpc.fundrawtransaction.return_value = {"hex": "funded"}
    result.rpc.signrawtransactionwithwallet.return_value = {"hex": "signed", "complete": True}
    result.rpc.decoderawtransaction.return_value = {
        "locktime": tip if funded_locktime is None else funded_locktime
    }
    result.rpc.sendrawtransaction.return_value = "txid"
    return result


def test_harness_send_pins_the_locktime_to_the_tip():
    """#590: no anti-fee-sniping jitter that could reach Knots' cat21 locktime."""
    source = funding_node(102)
    assert send_to_address(source, "addr", 1) == "txid"
    source.rpc.createrawtransaction.assert_called_once_with([], {"addr": 1}, 102)
    source.rpc.fundrawtransaction.assert_called_once()
    source.rpc.sendrawtransaction.assert_called_once_with("signed")


def test_harness_send_refuses_a_locktime_the_wallet_moved():
    source = funding_node(102, funded_locktime=PARASITE_CAT21_LOCKTIME)
    with pytest.raises(AssertionError, match="changed locktime 102 to 21"):
        send_to_address(source, "addr", 1)
    source.rpc.sendrawtransaction.assert_not_called()


def test_harness_send_refuses_to_send_at_the_cat21_tip():
    source = funding_node(PARASITE_CAT21_LOCKTIME)
    with pytest.raises(RuntimeError, match="parasite-cat21"):
        send_to_address(source, "addr", 1)
    source.rpc.createrawtransaction.assert_not_called()
