# Local Testnet4 sponsor

`tests/e2e/sponsor` funds a fresh QA vault using the completed disposable QA
vault's saved change output. It creates a separate intent and signed receipt;
it never broadcasts or changes either run's journals, configuration, PSBT,
provisioning state, or proof. This is a bounded funding utility for issue #1137,
not coin selection or a wallet recovery tool.

Build the signed native helper from the reviewed, clean commit:

```sh
tests/e2e/build-native
```

This uses pinned Rust 1.97.1, the `integration-tests` feature, this worktree's
Cargo target, and the existing COINCUBE signing identity and entitlements.
`tests/e2e/.local/native-build.json` records the commit and binary hashes.
The build includes signed Data Protection Keychain smoke tests; run it only
in the coordinator's authorized execution environment.

Prepare without reading the source backup or signing:

```sh
tests/e2e/sponsor --source-run /absolute/fresh02 --target-run /absolute/scenario03 \
  --output-dir /absolute/separate-sponsor --amount-sats 500000 --fee-sats 1000
```

The output directory must be private and separate from both runs and profiles
(including ancestors). The helper freezes the exact public-state hashes,
authenticated account identity, vault identities, previous transaction and
outpoint, scripts, and amounts in `sponsor-intent.json`. Both runs must have
the same authenticated account; the source must have completed public acceptance.
Check the intent against the authorized funding plan before passing its SHA-256:

```sh
tests/e2e/sponsor --source-run /absolute/fresh02 --target-run /absolute/scenario03 \
  --output-dir /absolute/separate-sponsor --amount-sats 500000 --fee-sats 1000 \
  --native-binary /absolute/checkout/tests/e2e/.local/Provision.app/Contents/MacOS/testnet4_provision \
  --sign-intent-sha256 EXACT_PREPARED_HASH
```

A coordinator can perform this comparison and hash dispatch in code; no manual
approval is part of the helper. For the authorized fresh02-to-scenario03 plan,
verify source transaction
`a65e0166bc54368b3d96fb852ad426408d5d78b530fd596b11e7fdf6e20607ac:1`,
989000 sats in, 500000 to scenario03, 1000 fee, and 488000 back to source change.

The native action reads only the existing source Data Protection Keychain backup.
It reconstructs the saved descriptor and signs with the Master Signer and Border
Wallet primary keys at change branch 1/index 0, using SIGHASH_ALL. It verifies
partial signatures and the finalized witness through the native interpreter.
The target is derived from its saved public descriptor at receive branch 0/index 0;
its secrets are never read. Amounts are limited to 100000–500000 sats, fees to
100–1000 sats, and inputs to the saved change output (at most 10000000 sats).

The Python caller independently decodes the transaction and verifies exact input,
outputs, fee, sequence, locktime, and txid. It checks the binary hash before and
after execution and rechecks both run snapshots before publishing a receipt.
Both fixed Testnet4 explorers must agree on genesis, previous raw transaction,
and unspent status, including a fresh check immediately before receipt publication.
Native evidence expires after 120 seconds.

Reruns require the identical intent and cannot replace the signed bytes. Replay
requires the source Keychain backup: the helper reconstructs the deterministic
Master-plus-Border witness and compares exact bytes, rejecting even a valid
Master-plus-phone replacement. A separate exclusive lock serializes invocations.

`sponsor-receipt.json` contains public signed transaction bytes and explorer
observations, with private filesystem permissions. It is **not** target strict
acceptance proof. The coordinator must recheck both explorers for the exact
unspent outpoint immediately before any separately authorized broadcast, then
let the normal fresh runner independently discover and validate target funding.
If the sponsor has already been broadcast, this helper stops on the spent source;
resume funding discovery in the target runner rather than create a new sponsor.

Offline checks:

```sh
tests/e2e/.venv/bin/python -m pytest tests/e2e/selftests/test_sponsor.py
PATH=/opt/homebrew/opt/rustup/bin:$PATH BREEZ_API_KEY=testnet4-fixture-placeholder \
  COINCUBE_API_URL=https://api.coincube.io cargo test --locked -p coincube-gui \
  --features integration-tests --example testnet4_provision -- --test-threads=1
```

These tests use synthetic transactions and secrets and never access live vaults,
Keychain, the phone, explorers, or broadcast endpoints. The signed build smoke
tests above are distinct and require the authorized local keystore environment.
