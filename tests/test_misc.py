import logging
from concurrent.futures import ThreadPoolExecutor
import pytest
import shutil
import time

from fixtures import *
from test_framework.authproxy import JSONRPCException
from test_framework.serializations import PSBT
from test_framework.utils import (
    BitcoinBackendType,
    BITCOIN_BACKEND_TYPE,
    wait_for,
    RpcError,
    OLD_COINCUBED_PATH,
    COINCUBED_PATH,
    COIN,
    TIMEOUT,
    IS_NOT_BITCOIND_24,
    USE_TAPROOT,
)

from threading import Event, Thread


def receive_and_send(coincubed, bitcoind):
    n_coins = len(coincubed.rpc.listcoins()["coins"])

    # Receive 3 coins in different blocks on different addresses.
    for _ in range(3):
        addr = coincubed.rpc.getnewaddress()["address"]
        txid = bitcoind.rpc.sendtoaddress(addr, 0.01)
        bitcoind.generate_block(1, wait_for_mempool=txid)
    wait_for(lambda: len(coincubed.rpc.listcoins()["coins"]) == n_coins + 3)

    # Create a spend that will create a change output, sign and broadcast it.
    outpoints = [
        next(
            c["outpoint"]
            for c in coincubed.rpc.listcoins()["coins"]
            if c["spend_info"] is None
        )
    ]
    destinations = {
        bitcoind.rpc.getnewaddress(): 200_000,
    }
    res = coincubed.rpc.createspend(destinations, outpoints, 42)
    psbt = PSBT.from_base64(res["psbt"])
    txid = psbt.tx.txid().hex()
    signed_psbt = coincubed.signer.sign_psbt(psbt, range(3))
    coincubed.rpc.updatespend(signed_psbt.to_base64())
    coincubed.rpc.broadcastspend(txid)
    bitcoind.generate_block(1, wait_for_mempool=txid)
    wait_for(
        lambda: coincubed.rpc.getinfo()["block_height"] == bitcoind.rpc.getblockcount()
    )

    # Spend all coins to check we can spend from change too. Re-create some deposits.
    outpoints = [
        c["outpoint"]
        for c in coincubed.rpc.listcoins()["coins"]
        if c["spend_info"] is None
    ]
    destinations = {
        bitcoind.rpc.getnewaddress(): 400_000,
        coincubed.rpc.getnewaddress()["address"]: 300_000,
        coincubed.rpc.getnewaddress()["address"]: 800_000,
    }
    res = coincubed.rpc.createspend(destinations, outpoints, 42)
    psbt = PSBT.from_base64(res["psbt"])
    txid = psbt.tx.txid().hex()
    # If we sign only with two keys it won't be able to finalize
    with pytest.raises(RpcError, match="ould not satisfy.* at index 0"):
        signed_psbt = coincubed.signer.sign_psbt(psbt, range(2))
        coincubed.rpc.updatespend(signed_psbt.to_base64())
        coincubed.rpc.broadcastspend(txid)
    # We can sign with different keys as long as there are 3 sigs
    signed_psbt = coincubed.signer.sign_psbt(psbt, range(1, 4))
    coincubed.rpc.updatespend(signed_psbt.to_base64())
    coincubed.rpc.broadcastspend(txid)
    bitcoind.generate_block(1, wait_for_mempool=txid)


def test_multisig(coincubed_multisig, bitcoind):
    """Test using coincubed with a descriptor that contains multiple keys for both
    the primary and recovery paths."""
    receive_and_send(coincubed_multisig, bitcoind)

    # Generate 10 blocks to test the recovery path
    bitcoind.generate_block(10)
    wait_for(
        lambda: coincubed_multisig.rpc.getinfo()["block_height"]
        == bitcoind.rpc.getblockcount()
    )

    # Sweep all coins through the recovery path. It needs 2 signatures out of
    # 5 keys. Sign with the second and the fifth ones.
    res = coincubed_multisig.rpc.createrecovery(bitcoind.rpc.getnewaddress(), 2)
    reco_psbt = PSBT.from_base64(res["psbt"])
    txid = reco_psbt.tx.txid().hex()
    signed_psbt = coincubed_multisig.signer.sign_psbt(reco_psbt, {10: [1, 4]})
    coincubed_multisig.rpc.updatespend(signed_psbt.to_base64())
    coincubed_multisig.rpc.broadcastspend(txid)


def test_multipath(coincubed_multipath, bitcoind):
    """Exercise various commands as well as recovery with a descriptor with multiple
    recovery paths."""
    receive_and_send(coincubed_multipath, bitcoind)

    # Generate 10 blocks to test the recovery path
    bitcoind.generate_block(10)
    wait_for(
        lambda: coincubed_multipath.rpc.getinfo()["block_height"]
        == bitcoind.rpc.getblockcount()
    )

    # We can't create a recovery tx for the second recovery path, as all coins were confirmed
    # within the last 19 blocks.
    with pytest.raises(
        RpcError,
        match="No coin currently spendable through this timelocked recovery path",
    ):
        coincubed_multipath.rpc.createrecovery(bitcoind.rpc.getnewaddress(), 2, 20)

    # Sweep all coins through the first recovery path (that is available after 10 blocks).
    # It needs 3 signatures out of 5 keys.
    res = coincubed_multipath.rpc.createrecovery(bitcoind.rpc.getnewaddress(), 2)
    reco_psbt = PSBT.from_base64(res["psbt"])
    txid = reco_psbt.tx.txid().hex()

    # NOTE: this test was commented out due to the introduced restriction to not include
    # the BIP32 derivations for other spending paths in PSBT inputs to support the Bitbox2
    # signing device (and most likely others).
    # TODO: reintroduce these tests once we get rid of this restriction.

    # Try to sign with the keys for the next recovery spending path, it'll fail.
    # signed_psbt = coincubed_multipath.signer.sign_psbt(reco_psbt, {20: range(3)})
    # coincubed_multipath.rpc.updatespend(signed_psbt.to_base64())
    # with pytest.raises(RpcError, match="Failed to finalize"):
    # coincubed_multipath.rpc.broadcastspend(txid)

    # Try to sign with the right keys but only two of them, it'll fail.
    signed_psbt = coincubed_multipath.signer.sign_psbt(reco_psbt, {10: range(2)})
    coincubed_multipath.rpc.updatespend(signed_psbt.to_base64())
    with pytest.raises(RpcError, match="Failed to finalize"):
        coincubed_multipath.rpc.broadcastspend(txid)

    # Finally add one more signature with an unused key from the right keyset.
    signed_psbt = coincubed_multipath.signer.sign_psbt(reco_psbt, {10: [2]})
    coincubed_multipath.rpc.updatespend(signed_psbt.to_base64())
    coincubed_multipath.rpc.broadcastspend(txid)

    # NOTE: commented out for the same reason as above.

    # Receive 3 more coins and make the second recovery path (20 blocks) available.
    # txids = []
    # for _ in range(3):
    # addr = coincubed_multipath.rpc.getnewaddress()["address"]
    # txids.append(bitcoind.rpc.sendtoaddress(addr, 0.42))
    # bitcoind.generate_block(20, wait_for_mempool=txids)
    # wait_for(
    # lambda: coincubed_multipath.rpc.getinfo()["block_height"]
    # == bitcoind.rpc.getblockcount()
    # )

    # We can create a recovery transaction for an earlier timelock.
    # coincubed_multipath.rpc.createrecovery(bitcoind.rpc.getnewaddress(), 2)

    # Sweep all coins through the second recovery path (that is available after 20 blocks).
    # It needs 3 signatures out of 5 keys.
    # res = coincubed_multipath.rpc.createrecovery(bitcoind.rpc.getnewaddress(), 2, 20)
    # reco_psbt = PSBT.from_base64(res["psbt"])
    # txid = reco_psbt.tx.txid().hex()

    # We can sign with any keys for the second recovery path (we need only 1 out of 10)
    # signed_psbt = coincubed_multipath.signer.sign_psbt(reco_psbt, {20: [8]})
    # coincubed_multipath.rpc.updatespend(signed_psbt.to_base64())
    # coincubed_multipath.rpc.broadcastspend(txid)

    # Now do this again but with signing using keys for the first recovery path.
    # Receive 3 more coins and make the second recovery path (20 blocks) available. Note this
    # is possible since the CSV checks the nSequence is >= to the value, not ==.
    # txids = []
    # for _ in range(3):
    # addr = coincubed_multipath.rpc.getnewaddress()["address"]
    # txids.append(bitcoind.rpc.sendtoaddress(addr, 0.398))
    # bitcoind.generate_block(20, wait_for_mempool=txids)
    # wait_for(
    # lambda: coincubed_multipath.rpc.getinfo()["block_height"]
    # == bitcoind.rpc.getblockcount()
    # )
    # Sweep all coins through the second recovery path (that is available after 20 blocks).
    # It needs 3 signatures out of 5 keys.
    # res = coincubed_multipath.rpc.createrecovery(bitcoind.rpc.getnewaddress(), 2, 20)
    # reco_psbt = PSBT.from_base64(res["psbt"])
    # txid = reco_psbt.tx.txid().hex()
    # We can sign with keys for the first recovery path (we need 3 out of 5)
    # signed_psbt = coincubed_multipath.signer.sign_psbt(reco_psbt, {10: range(2, 5)})
    # coincubed_multipath.rpc.updatespend(signed_psbt.to_base64())
    # coincubed_multipath.rpc.broadcastspend(txid)


def test_coinbase_deposit(coincubed, bitcoind):
    """Check we detect deposits from (mature) coinbase transactions."""
    wait_for_sync = lambda: wait_for(
        lambda: coincubed.rpc.getinfo()["block_height"] == bitcoind.rpc.getblockcount()
    )
    wait_for_sync()

    # Create a new deposit in a coinbase transaction. We must detect it and treat it as immature.
    addr = coincubed.rpc.getnewaddress()["address"]
    bitcoind.rpc.generatetoaddress(1, addr)
    wait_for_sync()
    coins = coincubed.rpc.listcoins()["coins"]
    assert (
        len(coins) == 1
        and coins[0]["is_immature"]
        and coins[0]["spend_info"] is None
        and not coins[0]["is_from_self"]
    )

    # Generate 100 blocks to make the coinbase mature. We should detect it as such.
    # It remains as not from self.
    bitcoind.generate_block(100)
    wait_for_sync()
    coin = coincubed.rpc.listcoins()["coins"][0]
    assert (
        not coin["is_immature"]
        and coin["block_height"] is not None
        and not coins[0]["is_from_self"]
    )

    # We must be able to spend the mature coin.
    destinations = {bitcoind.rpc.getnewaddress(): int(0.999999 * COIN)}
    res = coincubed.rpc.createspend(destinations, [coin["outpoint"]], 42)
    psbt = PSBT.from_base64(res["psbt"])
    txid = psbt.tx.txid().hex()
    signed_psbt = coincubed.signer.sign_psbt(psbt)
    coincubed.rpc.updatespend(signed_psbt.to_base64())
    coincubed.rpc.broadcastspend(txid)
    bitcoind.generate_block(1, wait_for_mempool=txid)
    wait_for_sync()
    coin = next(
        c for c in coincubed.rpc.listcoins()["coins"] if c["outpoint"] == coin["outpoint"]
    )
    assert (
        not coin["is_immature"]
        and coin["block_height"] is not None
        and coin["spend_info"] is not None
    )

    # We must also properly detect coinbase deposits to a change address. We used to have
    # an assertion that a coin cannot both be change and a coinbase deposit. Since change
    # is determined by the address... Technically we can.
    change_desc = coincubed.multi_desc.singlepath_descriptors()[1]
    change_addr = bitcoind.rpc.deriveaddresses(str(change_desc), [0, 0])[0]
    bitcoind.rpc.generatetoaddress(1, change_addr)
    wait_for(lambda: any(c["is_immature"] for c in coincubed.rpc.listcoins()["coins"]))
    coin = next(c for c in coincubed.rpc.listcoins()["coins"] if c["is_immature"])
    assert coin["is_change"] and not coin["is_from_self"]
    bitcoind.generate_block(100)
    wait_for_sync()
    coin = next(
        c for c in coincubed.rpc.listcoins()["coins"] if c["outpoint"] == coin["outpoint"]
    )
    assert (
        not coin["is_immature"]
        and coin["block_height"] is not None
        and not coin["is_from_self"]
    )


@pytest.mark.skipif(
    OLD_COINCUBED_PATH is None or USE_TAPROOT,
    reason="Need the old coincubed binary to create the datadir.",
)
@pytest.mark.skipif(
    BITCOIN_BACKEND_TYPE is not BitcoinBackendType.Bitcoind,
    reason="Only bitcoind backend was available for older coincubed versions.",
)
def test_migration(coincubed_multisig_legacy_datadir, bitcoind):
    """Test we can start a newer coincubed on a datadir created by an older coincubed."""
    coincubed = coincubed_multisig_legacy_datadir

    # Set the old binary and re-create the datadir.
    coincubed.cmd_line[0] = OLD_COINCUBED_PATH
    coincubed.restart_fresh(bitcoind)
    old_coincubed_ver = coincubed.rpc.getinfo()["version"]
    assert old_coincubed_ver in ["0.3.0", "0.9.0", "1.0.0"]

    # Perform some transactions. On Coincube v0.3 there was no "updated_at" for Spend
    # transaction drafts.
    receive_and_send(coincubed, bitcoind)
    spend_txs = coincubed.rpc.listspendtxs()["spend_txs"]
    assert len(spend_txs) == 2
    if old_coincubed_ver == "0.3.0":
        assert all("updated_at" not in s for s in spend_txs)

    # Set back the new binary. We should be able to read and, if necessary, upgrade
    # the old database and generally all files from the datadir.
    coincubed.cmd_line[0] = COINCUBED_PATH
    coincubed.restart_fresh(bitcoind)

    # And we can go on to create more deposits and transactions. Make sure we now have
    # the "updated_at" field on tx drafts.
    receive_and_send(coincubed, bitcoind)
    spend_txs = coincubed.rpc.listspendtxs()["spend_txs"]
    assert len(spend_txs) == 2 and all(s["updated_at"] is not None for s in spend_txs)


@pytest.mark.skipif(
    not IS_NOT_BITCOIND_24, reason="Need 'generateblock' with 'submit=False'"
)
def test_bitcoind_submit_block(bitcoind):
    block_count = bitcoind.rpc.getblockcount()
    block = bitcoind.rpc.generateblock(bitcoind.rpc.getnewaddress(), [], False)
    bitcoind.submit_block(block_count, block["hex"])
    wait_for(lambda: bitcoind.rpc.getblockcount() == block_count + 1)


def bitcoind_wait_block_height(bitcoind, height, overloaded):
    """Occupy an RPC slot until a known height, with bounded overload retries."""
    deadline = time.monotonic() + TIMEOUT
    while time.monotonic() < deadline:
        try:
            result = bitcoind.rpc.waitforblockheight(height, TIMEOUT * 1000)
            assert result["height"] >= height
            return
        except JSONRPCException as e:
            if e.http_status != 503:
                raise
            overloaded.set()
            time.sleep(0.1)
    raise TimeoutError("Node work queue did not recover")


@pytest.mark.skipif(
    not IS_NOT_BITCOIND_24, reason="Need 'generateblock' with 'submit=False'"
)
@pytest.mark.skipif(
    BITCOIN_BACKEND_TYPE is not BitcoinBackendType.Bitcoind,
    reason="Tests the retry logic specific to the bitcoind backend.",
)
def test_retry_on_workqueue_exceeded(coincubed, bitcoind):
    """Make sure we retry requests to bitcoind if it is temporarily overloaded."""
    # Restart both processes with a single RPC worker and queue slot, keeping
    # startup traffic outside the deliberately overloaded interval.
    coincubed.stop()
    bitcoind.cmd_line += ["-rpcworkqueue=1", "-rpcthreads=1"]
    bitcoind.stop()
    bitcoind.start()

    # Mine a block without submitting it; its height will release all waiters.
    block_count = bitcoind.rpc.getblockcount()
    block = bitcoind.rpc.generateblock(bitcoind.rpc.getnewaddress(), [], False)

    # Only restart Coincube now to make sure the above bitcoind RPCs don't conflict with the
    # ones performed by Coincube at startup.
    coincubed.start()

    # One request occupies the only RPC thread, one fills the queue, and a
    # third observes 503. All three complete at the same known height, even
    # if they enter the RPC handler only after the block has arrived.
    # CI's shared executor has only three workers. All three waiters must run
    # concurrently with the daemon command, so own exactly four here.
    with ThreadPoolExecutor(max_workers=4) as executor:
        overloaded = Event()
        waiters = [
            executor.submit(
                bitcoind_wait_block_height, bitcoind, block_count + 1, overloaded
            )
            for _ in range(3)
        ]
        try:
            assert overloaded.wait(TIMEOUT), "RPC work queue was not saturated"
            # getinfo uses cached sync/height and no longer exercises node RPC.
            # Timestamp zero forces the generic genesis/tip reads but is rejected
            # before changing rescan state once the node answers.
            f_coincube = executor.submit(coincubed.rpc.startrescan, 0)
            coincubed.wait_for_logs(
                [
                    "Transient error when sending request to bitcoind.*(status: 503, body: Work queue depth exceeded)",
                    "Retrying RPC request to bitcoind",
                ],
                timeout=TIMEOUT,
            )
        finally:
            # Use P2P, which remains available with the RPC queue full. Always
            # release the blocked requests, including when the log assertion fails.
            bitcoind.submit_block(block_count, block["hex"])
            for future in waiters:
                future.result(TIMEOUT)

        with pytest.raises(RpcError, match="Insane timestamp"):
            f_coincube.result(TIMEOUT)
        assert coincubed.rpc.getinfo()["rescan_progress"] is None
