## Coincubed blackbox tests

Here we test `coincubed` by starting it on a regression testing Bitcoin network,
and by then talking to it as an user would, from the outside.

Python scripts are used for the automation, and specifically the [`pytest` framework](https://docs.pytest.org/en/stable/index.html).

Credits: this test framework was taken and adapted from revaultd, which was itself adapted from
[C-lightning's test framework](https://github.com/ElementsProject/lightning/tree/master/contrib/pyln-testing).

### Building the project for testing

To run the tests, we must build `coincubed`.

```
$ cargo build --release
```

The `coincubed` and `coincube-cli` binaries will be in the `target/release` directory at the root of the repository.

### Test dependencies

Before running the tests, you might need to install some system packages. Here's an example for Ubuntu:

```
sudo apt update
sudo apt install build-essential libssl-dev libffi-dev python3-dev
sudo apt install autoconf automake libtool
```

Functional tests dependencies can be installed using `pip`. Use a virtual environment:

```
# Create a new virtual environment, preferably.
python3 -m venv venv
. venv/bin/activate
# Get the deps
pip install -r tests/requirements.txt
```

Additionally you need to have `bitcoind` installed on your computer, please
refer to [bitcoincore](https://bitcoincore.org/en/download/) for installation. You may use a
specific `bitcoind` binary by specifying the `BITCOIND_PATH` env var.

### Running the tests

From the root of the repository:

```
pytest tests/
```

For running the tests under Taproot a `bitcoind` version 26.0 or superior must be used. It can be
pointed to using the `BITCOIND_PATH` variable. For now, one must also compile the `taproot_signer`
Rust program:

```
(cd tests/tools/taproot_signer && cargo build --release)
```

Then the test suite can be run by using Taproot descriptors instead of P2WSH descriptors by setting
the `USE_TAPROOT` environment variable to `1`.

### BTCB2 two-chain harness

`tests/test_btcb2_harness.py` brings up the Bitcoin Blake2b (BTCB2) regtest
harness from launch-ga Lane B4.1: Bitcoin Knots `29.3.knots20260507` (the pinned
non-enforcing build) and `29.4.1.knots20260508` with the BLAKE2b hardfork
scheduled at a chosen height, a BLAKE2b-aware Esplora indexer
(`retropex/electrs`, branch `mempool`, commit `4453cac`) on each chain, and
`coincubed` on the Esplora backend against each. Both nodes start from one
pre-fork history (node B is started on a copy of node A's datadir at
`activation_height - 1`) and are never peered, so they diverge at the height.
The fixture funds a 2-of-3 Vault with a recovery path before the fork and creates
one post-fork Bitcoin-only coin (spent from a post-fork coinbase) for the input
poison. See `tests/test_framework/btcb2.py`.

The harness is skipped unless all three binaries are configured:

```
(cd tests/tools/knots_verify && cargo build --release)   # or set CARGO_TARGET_DIR to where it was built
export KNOTS_LEGACY_PATH=$(tests/tools/fetch_knots.sh 29.3.knots20260507)
export KNOTS_BLAKE2B_PATH=$(tests/tools/fetch_knots.sh 29.4.1.knots20260508)
export ELECTRS_BLAKE2B_PATH=$(tests/tools/fetch_electrs_blake2b.sh)
pytest tests/test_btcb2_harness.py -vvv
```

The production Claim construction/finalization regression uses the same binaries
and a test-only Rust bridge:

```sh
cargo build --release --package coincube-core --example claim_regtest_vectors
export CLAIM_REGTEST_TOOL_PATH="$PWD/target/release/examples/claim_regtest_vectors"
pytest tests/test_btcb2_claim_consensus.py -vvv --junitxml=claim-consensus.xml
```

A separate headless GUI test drives the production Claim panel's build, signer
picker, software signing, review, explicit confirmation and confirmation tracking.
It sends through the opt-in gated regtest transport and independently checks the
exact transaction and witness on the real node. Build and select the lib-test
executable (not the GUI application):

```sh
BREEZ_API_KEY=DUMMY_BREEZ_API_KEY cargo test --package coincube-gui --lib --features regtest-harness --no-run --message-format=json > claim-gui-build.json
export CLAIM_GUI_REGTEST_TEST_PATH="$(jq -r 'select(.reason == "compiler-artifact" and .target.name == "coincube_gui" and .profile.test == true and .executable != null) | .executable' claim-gui-build.json)"
pytest tests/test_btcb2_claim_gui.py -vvv --junitxml=claim-gui.xml
```

This test owns separate disposable nodes and synthetic keys. Its account, logical
mainnet routes and backend adapter are test fixtures; deployment, observations,
preflight, signing and submission use actual node evidence. It also drives fork-panel software signing and explicit submission, verifies paired
completion markers, and exercises Bitcoin reorg withdrawal and explicit
reconfirmation after the identical transaction is re-mined. The pinned indexer is
paused while the complete competing branch is assembled, then restarted with the
same database: this proves GUI recovery after indexing resumes, not uninterrupted
indexer availability or outage UI. Return-to-Bitcoin uses panel lifecycle methods;
fork reopening is in the same process. Process restart, App/tab routing, rendered
UI, PIN unlock and hardware signing remain outside this live test.
The `regtest-harness` feature is off in normal builds. The labelled BTCB2 workflow
builds this driver and fails if the required executable or pinned nodes are missing.

Set the bridge path to your Cargo target directory when using `CARGO_TARGET_DIR`.
This test spends the fixture's original coins, so it has its own module-scoped
harness. It checks the production OP_RETURN self-transfer and legacy 2-of-3 fork
sweep against both real nodes, pins the fork block rejection `bad-txns-vout-script-toolarge` and Bitcoin
`missing-inputs` response in JUnit, and
invalidates Bitcoin confirmation while the fork sweep remains confirmed. The
legacy node uses `datacarriersize=100`, matching the managed-node Claim policy;
the fork node does not receive that override. The bridge never loads keys or
contacts a network: disposable fixture keys sign its PSBTs in Python. This is
consensus integration coverage, not GUI, hardware, input-poison, or restart
acceptance. The labelled CI workflow builds the bridge and runs this test too.

`fetch_knots.sh` downloads a release for the host platform and refuses to extract
it unless its checksum is listed in the release `SHA256SUMS` *and*
`SHA256SUMS.asc` verifies against the Knots signing key vendored in
`coincube-gui/assets/knots_signing_key.asc` — the same check the desktop
installer performs (`tests/tools/knots_verify`, no `gpg` needed). The check runs
on every invocation, cached or not, and the extracted `bitcoind` is reused only
if it matches the archive member byte for byte; there is no stored "verified"
marker to trust. No dedicated variable names the verifier outside the script's
own test mode (`KNOTS_TEST_MODE=1`, used only by `tests/test_btcb2_tools.py`);
in real runs it is located through `CARGO_TARGET_DIR` (then the repo-local
build), and the script prints the verifier it resolved on stderr every time so
a stale or foreign build is visible in the log.
`fetch_electrs_blake2b.sh` clones and builds the pinned indexer commit (RocksDB
compiles from source: a few minutes the first time; `clang`/`cmake` required)
under `~/.cache/coincube/electrs-blake2b` — outside the repository, or Cargo
would treat the checkout as part of Coincube's workspace. It refuses to build
or reuse anything if that checkout has local changes (edited, staged or
untracked files): `--locked` pins dependencies, not source. Your edits are left
in place; restore the tree or use another cache dir. `tests/test_btcb2_tools.py`
covers these checks without a build. Knots downloads cache
under `tests/tools/knots/` (gitignored). In CI `BTCB2_HARNESS_REQUIRED=1` turns
a missing binary into a failure rather than a skip.

Every node, indexer and daemon datadir is created under the test directory, and
every child process runs with `HOME`/`XDG_*` pointing at an empty sandbox inside
it; `test_every_datadir_is_pinned_under_the_test_directory` fails if anything
lands there. In CI the harness runs from `.github/workflows/btcb2-regtest.yml`
only on pull requests labelled `btcb2` (or on manual dispatch).

### Tips and tricks

#### Logging

We use the [Live Logging](https://docs.pytest.org/en/latest/logging.html#live-logs)
functionality from pytest. It is configured in (`pyproject.toml`)[../pyproject.toml] to
output `INFO`-level to the console. If a test fails, the entire `DEBUG` log is output.

You can override the config at runtime with the `--log-cli-level` option:

```
pytest -vvv --log-cli-level=DEBUG -k test_startup
```

Note that we record all logs from daemons, and we start them with `log_level = "debug"`.

#### Running tests in parallel

In order to run tests in parallel, you can use `-n` arg:

```
pytest -n 8 tests/
```

### Test lints

Just use [`black`](https://github.com/psf/black).

### More

See the environment variables in `test_framework/utils.py`.
