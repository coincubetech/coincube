"""Two-chain Bitcoin Blake2b (BTCB2) regtest harness — launch-ga Lane B4.1.

Brings up the two node builds Tenshu cares about from one shared pre-fork
history, an Esplora indexer against each, and a Coincube Vault whose coins exist
on both chains:

* node A — Bitcoin Knots ``29.3.knots20260507``, the pinned non-enforcing build
  (SHA256d forever, the "Bitcoin" side);
* node B — Bitcoin Knots ``29.4.1.knots20260508`` with the BLAKE2b hardfork
  scheduled at ``activation_height`` via ``-testactivationheight=blake2b@N`` and
  RDTS (BIP-110) scheduled with ``-rdtsexpiry`` (without it the regtest
  deployment stays unscheduled and the OP_RETURN limit is never a block rule —
  see the BTCB2 audit on coincube-api#276, F11);
* ``retropex/electrs`` (Esplora API, BLAKE2b-aware) indexing each chain;
* ``coincubed`` on the Esplora backend against each indexer, both loaded with the
  same 2-of-3 Vault descriptor with a timelocked recovery path.

Fixtures the B4.2 matrices consume:

* ``prefork_outpoints`` — Vault coins confirmed *before* ``activation_height``,
  hence present on both chains (entangled);
* ``poison_outpoint`` — a Vault coin created on node A *after* the fork by
  spending a post-fork coinbase: it exists on the Bitcoin side only and can never
  exist on the BLAKE2b chain (coinbase ancestry), so it is a permanent input
  poison for the split.

Every datadir is a synthetic temporary directory under the harness directory,
and every child process runs with ``HOME``/XDG variables pointing at an empty
sandbox inside it, so a config that silently fell back to a real user directory
would show up as files in the sandbox (``assert_home_sandbox_untouched``).

The pre-fork history is shared by stopping node A at ``activation_height - 1``
and starting node B on a copy of its datadir. The two nodes are never peered:
each mines its own chain past the height, which is exactly the divergence the
harness exists to model.
"""

import logging
import os
import shutil
from decimal import Decimal

from bip32.utils import _pubkey_to_fingerprint
from bip380.descriptors import Descriptor
from test_framework.bitcoind import Bitcoind
from test_framework.coincubed import Coincubed
from test_framework.esplora import EsploraElectrs
from test_framework.signer import MultiSigner
from test_framework.utils import TIMEOUT, wait_for

KNOTS_LEGACY_VERSION = "29.3.knots20260507"
KNOTS_BLAKE2B_VERSION = "29.4.1.knots20260508"
# retropex/electrs, branch `mempool`, "Support BLAKE2b".
ELECTRS_BLAKE2B_COMMIT = "4453cac61979322c0260f4b90e899379ae606206"

KNOTS_LEGACY_PATH = os.getenv("KNOTS_LEGACY_PATH")
KNOTS_BLAKE2B_PATH = os.getenv("KNOTS_BLAKE2B_PATH")
ELECTRS_BLAKE2B_PATH = os.getenv("ELECTRS_BLAKE2B_PATH")

# 2100-01-01T00:00:00Z. RDTS rules apply to every BLAKE2b block whose parent's
# median-time-past is below this, i.e. to the whole regtest run.
RDTS_EXPIRY_FAR_FUTURE = 4102444800

# Regtest coinbase maturity.
COINBASE_MATURITY = 100


def missing_binaries():
    """Names of the harness binaries that are unset or not executable."""
    return [
        name
        for name, path in (
            ("KNOTS_LEGACY_PATH", KNOTS_LEGACY_PATH),
            ("KNOTS_BLAKE2B_PATH", KNOTS_BLAKE2B_PATH),
            ("ELECTRS_BLAKE2B_PATH", ELECTRS_BLAKE2B_PATH),
        )
        if not (path and os.access(path, os.X_OK))
    ]


def harness_available():
    return not missing_binaries()


def _child_env(home_dir):
    """Environment overrides that make any HOME/XDG fallback land in the sandbox."""
    return {
        "HOME": home_dir,
        "XDG_CONFIG_HOME": os.path.join(home_dir, ".config"),
        "XDG_DATA_HOME": os.path.join(home_dir, ".local", "share"),
        "XDG_CACHE_HOME": os.path.join(home_dir, ".cache"),
    }


def _xpub_fingerprint(hd):
    return _pubkey_to_fingerprint(hd.pubkey).hex()


def _multi_expression(thresh, keys):
    # Same convention as `fixtures.multi_expression`: the origin fingerprint is
    # the xpub's own, which is not how a real origin works but tells keys apart.
    inner = ",".join(
        f"[{_xpub_fingerprint(key)}]{key.get_xpub()}/<0;1>/*" for key in keys
    )
    return f"multi({thresh},{inner})"


def vault_descriptor(signer, csv_value):
    """`wsh(or_d(multi(2, K1, K2, K3), and_v(v:multi(1, R1), older(csv))))`.

    K1 = hot key, K2 = Keychain-simulated, K3 = the hardware stand-in that will
    only ever produce legacy signatures; R1 = the recovery key. Same shape as
    `fixtures.multisig_desc(signer, csv, False, 2, 1)`.
    """
    prim = _multi_expression(2, signer.prim_hds)
    recov = _multi_expression(1, signer.recov_hds[csv_value])
    return Descriptor.from_str(f"wsh(or_d({prim},and_v(v:{recov},older({csv_value}))))")


class TwoChainRegtest:
    def __init__(
        self,
        directory,
        activation_height=110,
        post_fork_blocks_blake2b=6,
        legacy_path=None,
        blake2b_path=None,
        electrs_path=None,
        csv_value=10,
    ):
        self.directory = directory
        self.activation_height = activation_height
        self.post_fork_blocks_blake2b = post_fork_blocks_blake2b
        self.legacy_path = legacy_path or KNOTS_LEGACY_PATH
        self.blake2b_path = blake2b_path or KNOTS_BLAKE2B_PATH
        self.electrs_path = electrs_path or ELECTRS_BLAKE2B_PATH
        self.csv_value = csv_value

        self.home_dir = os.path.join(directory, "home-sandbox")
        self.legacy_dir = os.path.join(directory, "knots-legacy")
        self.blake2b_dir = os.path.join(directory, "knots-blake2b")
        self.electrs_legacy_dir = os.path.join(directory, "electrs-legacy")
        self.electrs_blake2b_dir = os.path.join(directory, "electrs-blake2b")
        self.coincubed_legacy_dir = os.path.join(directory, "coincubed-legacy")
        self.coincubed_blake2b_dir = os.path.join(directory, "coincubed-blake2b")

        self.legacy = None
        self.blake2b = None
        self.electrs_legacy = None
        self.electrs_blake2b = None
        self.coincubed_legacy = None
        self.coincubed_blake2b = None

        self.signer = None
        self.desc = None
        self.vault_addresses = []
        self.prefork_outpoints = []  # (txid, vout, amount_btc)
        self.prefork_txids = []
        self.prefork_block_hashes = {}  # txid -> block hash (shared by both chains)
        self.poison_block_hash = None
        self.poison_outpoint = None  # (txid, vout, amount_btc)
        self.poison_coinbase_txid = None
        self.fork_parent_hash = None
        self.fork_block_hash_legacy = None
        self.fork_block_hash_blake2b = None

    # ── lifecycle ─────────────────────────────────────────────────────────

    def setup(self):
        os.makedirs(self.home_dir, exist_ok=True)
        self._start_legacy_and_fund()
        self._fork()
        self._mine_past_fork_and_create_poison()
        self._start_indexers()
        self._start_daemons()

    def cleanup(self):
        for proc in (
            self.coincubed_legacy,
            self.coincubed_blake2b,
            self.electrs_legacy,
            self.electrs_blake2b,
            self.legacy,
            self.blake2b,
        ):
            if proc is None:
                continue
            try:
                proc.cleanup()
            except Exception as e:  # pragma: no cover — best effort teardown
                logging.warning("cleanup of %s failed: %s", proc.prefix, e)

    # ── phases ────────────────────────────────────────────────────────────

    def _new_node(self, bitcoin_dir, bitcoind_path, extra_args=None):
        node = Bitcoind(
            bitcoin_dir=bitcoin_dir, bitcoind_path=bitcoind_path, extra_args=extra_args
        )
        node.env.update(_child_env(self.home_dir))
        return node

    def _start_legacy_and_fund(self):
        n = self.activation_height
        self.legacy = self._new_node(self.legacy_dir, self.legacy_path)
        self.legacy.startup()
        rpc = self.legacy.rpc
        rpc.createwallet(rpc.wallet_name, False, False, "", False, True, True)
        # Mature one coinbase for funding.
        self.legacy.generate_block(COINBASE_MATURITY + 1)
        assert rpc.getblockcount() == COINBASE_MATURITY + 1

        # The Vault: 2-of-3 primary path, 1-key recovery path after `csv_value`.
        self.signer = MultiSigner(3, {self.csv_value: 1}, is_taproot=False)
        self.desc = vault_descriptor(self.signer, self.csv_value)
        receive_desc, _ = self.desc.singlepath_descriptors()
        checksummed = rpc.getdescriptorinfo(str(receive_desc))["descriptor"]
        self.vault_addresses = rpc.deriveaddresses(checksummed, [0, 3])

        # Fund three Vault coins *before* the activation height. They confirm at
        # a height every chain shares, so they exist on both sides of the fork.
        amounts = [Decimal("1.0"), Decimal("0.5"), Decimal("0.25")]
        for addr, amount in zip(self.vault_addresses[:3], amounts):
            txid = rpc.sendtoaddress(addr, amount)
            self.prefork_txids.append(txid)
            self.legacy.generate_block(1, wait_for_mempool=txid)
            self.prefork_block_hashes[txid] = rpc.getbestblockhash()
            decoded = rpc.gettransaction(txid, True, True)["decoded"]
            vout = next(
                o["n"]
                for o in decoded["vout"]
                if o["scriptPubKey"].get("address") == addr
            )
            self.prefork_outpoints.append((txid, vout, amount))

        # Land exactly on activation_height - 1: the last block both chains share.
        remaining = (n - 1) - rpc.getblockcount()
        assert (
            remaining >= 0
        ), f"activation_height {n} is below the funded height {rpc.getblockcount() + 1}"
        if remaining:
            self.legacy.generate_block(remaining)
        assert rpc.getblockcount() == n - 1
        self.fork_parent_hash = rpc.getbestblockhash()

    def _fork(self):
        """Give node B node A's history up to `activation_height - 1`."""
        self.legacy.stop()
        shutil.copytree(self.legacy_dir, self.blake2b_dir)
        # The copy carries node A's log and bitcoin.conf; both are rewritten by
        # the Bitcoind constructor (new ports) and TailableProc (new log).
        self.blake2b = self._new_node(
            self.blake2b_dir,
            self.blake2b_path,
            extra_args=[
                f"-testactivationheight=blake2b@{self.activation_height}",
                f"-rdtsexpiry={RDTS_EXPIRY_FAR_FUTURE}",
            ],
        )
        self.blake2b.startup()
        self.legacy.start()
        assert self.legacy.rpc.getbestblockhash() == self.fork_parent_hash
        assert self.blake2b.rpc.getbestblockhash() == self.fork_parent_hash

    def _mine_past_fork_and_create_poison(self):
        n = self.activation_height
        # Node B: the first block at `n` is the first BLAKE2b block.
        self.blake2b.generate_block(self.post_fork_blocks_blake2b)
        self.fork_block_hash_blake2b = self.blake2b.rpc.getblockhash(n)

        # Node A keeps mining SHA256d. Mature its block-`n` coinbase so it can be
        # spent into the Vault: that coin descends from a coinbase that exists on
        # the Bitcoin side only, which is what makes it a permanent input poison.
        self.legacy.generate_block(COINBASE_MATURITY + 1)
        self.fork_block_hash_legacy = self.legacy.rpc.getblockhash(n)
        rpc = self.legacy.rpc
        coinbase_txid = rpc.getblock(self.fork_block_hash_legacy)["tx"][0]
        self.poison_coinbase_txid = coinbase_txid
        coinbase = rpc.getrawtransaction(
            coinbase_txid, True, self.fork_block_hash_legacy
        )
        cb_out = coinbase["vout"][0]
        fee = Decimal("0.0001")
        raw = rpc.createrawtransaction(
            [{"txid": coinbase_txid, "vout": 0}],
            {self.vault_addresses[3]: Decimal(str(cb_out["value"])) - fee},
        )
        signed = rpc.signrawtransactionwithwallet(raw)
        assert signed["complete"], signed
        poison_txid = rpc.sendrawtransaction(signed["hex"])
        self.legacy.generate_block(1, wait_for_mempool=poison_txid)
        self.poison_block_hash = rpc.getbestblockhash()
        self.poison_raw_hex = signed["hex"]
        self.poison_outpoint = (poison_txid, 0, Decimal(str(cb_out["value"])) - fee)

    def _start_indexers(self):
        self.electrs_legacy = EsploraElectrs(
            electrs_dir=self.electrs_legacy_dir,
            bitcoind_dir=self.legacy_dir,
            bitcoind_rpcport=self.legacy.rpcport,
            electrs_path=self.electrs_path,
        )
        self.electrs_blake2b = EsploraElectrs(
            electrs_dir=self.electrs_blake2b_dir,
            bitcoind_dir=self.blake2b_dir,
            bitcoind_rpcport=self.blake2b.rpcport,
            electrs_path=self.electrs_path,
        )
        for electrs, node in (
            (self.electrs_legacy, self.legacy),
            (self.electrs_blake2b, self.blake2b),
        ):
            electrs.env.update(_child_env(self.home_dir))
            electrs.startup()
            electrs.wait_for_tip(node.rpc.getbestblockhash(), timeout=TIMEOUT * 3)

    def _start_daemons(self):
        for d in (self.coincubed_legacy_dir, self.coincubed_blake2b_dir):
            os.makedirs(d, exist_ok=True)
        self.coincubed_legacy = Coincubed(
            self.coincubed_legacy_dir, self.signer, self.desc, self.electrs_legacy
        )
        self.coincubed_blake2b = Coincubed(
            self.coincubed_blake2b_dir, self.signer, self.desc, self.electrs_blake2b
        )
        for daemon, node in (
            (self.coincubed_legacy, self.legacy),
            (self.coincubed_blake2b, self.blake2b),
        ):
            daemon.env.update(_child_env(self.home_dir))
            daemon.start()
            tip = node.rpc.getblockcount()
            wait_for(
                lambda: daemon.rpc.getinfo()["block_height"] == tip,
                timeout=TIMEOUT * 3,
                debug_fn=lambda: f"{daemon.prefix} at {daemon.rpc.getinfo()['block_height']} (tip {tip})",
            )

    # ── helpers for tests and the B4.2 matrices ───────────────────────────

    def deployment_info(self, node):
        return node.rpc.getdeploymentinfo()

    def assert_home_sandbox_untouched(self):
        """No child process fell back to a HOME/XDG-derived directory."""
        created = []
        for root, dirs, files in os.walk(self.home_dir):
            for name in dirs + files:
                created.append(os.path.relpath(os.path.join(root, name), self.home_dir))
        assert created == [], f"processes wrote into the HOME sandbox: {created}"

    def datadir_claims(self):
        """Every path each process was told to use, for the pinning assertion."""
        claims = {
            "knots-legacy": self.legacy.bitcoin_dir,
            "knots-blake2b": self.blake2b.bitcoin_dir,
            "electrs-legacy": self.electrs_legacy.db_dir,
            "electrs-blake2b": self.electrs_blake2b.db_dir,
            "coincubed-legacy": self.coincubed_legacy.datadir,
            "coincubed-blake2b": self.coincubed_blake2b.datadir,
        }
        return claims
