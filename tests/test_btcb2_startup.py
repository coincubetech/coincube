"""No-node diagnostics regressions for the two-chain indexer fixture."""
import sys
from unittest.mock import Mock

import pytest

from test_framework.btcb2 import TwoChainRegtest
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


# Block hashes and a txid, as electrs logs and serves them.
HEADER_TIP, EARLIER_TIP, SWEEP = "0b" * 32, "33" * 32, "d1" * 32


def indexer_serving(tmp_path, tip, confirmed):
    """An EsploraElectrs whose REST API serves `tip` as its header tip and
    `confirmed[txid]` as each transaction's status; its log is set by the test."""
    process = EsploraElectrs.__new__(EsploraElectrs)
    TailableProc.__init__(process, str(tmp_path), verbose=False)
    process.prefix = "electrs-blake2b"
    process.running = True

    def rest(path, timeout=10):
        if path == "/blocks/tip/hash":
            return tip
        txid = path.removeprefix("/tx/").removesuffix("/status")
        assert path == f"/tx/{txid}/status", path
        return {"confirmed": confirmed[txid]}

    process.rest = rest
    return process


def synced(block_hash):
    # How TailableProc records the line electrs logs at the end of an update.
    return f"b'DEBUG - updating synced tip to {block_hash}'"


def test_wait_for_tip_waits_for_the_index_update_not_the_header_tip(tmp_path):
    confirmed = {SWEEP: False}
    process = indexer_serving(tmp_path, HEADER_TIP, confirmed)
    # The #610 failure (#617): electrs had applied the new block's header, so
    # `/blocks/tip/hash` served it, but had not finished writing the block's
    # transactions. A read of the sweep in that window was a 404.
    process.logs = [
        synced(EARLIER_TIP),
        f"b'DEBUG - downloading new block headers (232 already indexed) from {HEADER_TIP}'",
        "b'DEBUG - applying 1 new headers from height 232'",
        "b'DEBUG - adding transactions from 1 blocks using Bitcoind'",
    ]
    with pytest.raises(ValueError, match="Error waiting"):
        process.wait_for_tip(HEADER_TIP, timeout=1)

    # The update's last line: transactions and history are written.
    process.logs.append(synced(HEADER_TIP))
    process.wait_for_tip(HEADER_TIP, timeout=5)
    # A transaction the test mined must also read as confirmed.
    with pytest.raises(ValueError, match="Error waiting"):
        process.wait_for_tip(HEADER_TIP, timeout=1, confirmed_txids=[SWEEP])
    confirmed[SWEEP] = True
    process.wait_for_tip(HEADER_TIP, timeout=5, confirmed_txids=[SWEEP])


def test_wait_for_tip_needs_the_latest_update_to_be_for_that_tip(tmp_path):
    # Back on an earlier tip after a reorg: the header tip is already back, but
    # the update indexing it has not finished. Its old line must not count.
    process = indexer_serving(tmp_path, EARLIER_TIP, {})
    process.logs = [synced(EARLIER_TIP), synced(HEADER_TIP)]
    with pytest.raises(ValueError, match="Error waiting"):
        process.wait_for_tip(EARLIER_TIP, timeout=1)
    assert process.synced_tip() == HEADER_TIP
    process.logs.append(synced(EARLIER_TIP))
    process.wait_for_tip(EARLIER_TIP, timeout=5)
