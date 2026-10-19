# Bitcoin wallet import

`Import Bitcoin wallet` below `Create Cube` creates a Cube through the normal unlock and backup flow, then imports an existing public Bitcoin wallet as that Cube's **Vault**. The existing Vault wallet type, card, navigation, Overview, Receive, Coins and Transactions screens are reused. Send and PSBT screens use an adapter for the imported signing policy within Vault. Importing does not transfer coins or import spending secrets. The original hardware wallets or external PSBT signers remain necessary to spend.

The common GUI `Wallet` and App daemon lifecycle serve both created and imported Vaults. The core `WalletDescriptor` retains strict `Vault(CoincubeDescriptor)` and `Imported(ImportedWalletDescriptor)` policy variants: an imported single-signature or multisignature wallet does not acquire artificial inheritance paths. Connect's existing `hasVault` contract still means inheritance capability and is not changed merely because the imported wallet appears under Vault.

## Supported wallets

Production onboarding accepts Bitcoin mainnet public ranged descriptors with key origins and conventional receive `/0/*` and change `/1/*` branches. Supply both descriptors or a combined `/<0;1>/*` descriptor. Supported scripts are `pkh`, `sh(wpkh)`, `wpkh`, `wsh(multi)`, and `wsh(sortedmulti)`. Standard hardware single-signature account discovery is also available; multisignature import requires the complete descriptor.

The local daemon supports imported wallets through Esplora or Electrum. Connect's authenticated Esplora proxy can provide blockchain data without using the remote Vault backend. Imported Bitcoin Core RPC configurations are rejected with guidance to choose Esplora or Electrum. Taproot, arbitrary Miniscript, private-key/seed imports, nonstandard branches and originless descriptors are outside this release.

## Lifecycle and recovery

The import operation persists against one Cube UUID, with `PendingSource`, `Installing`, `Syncing` and `Ready` stages. A locked journal publishes owned staged storage before attachment. Retrying preserves the operation, descriptor identity and address counters. Cancellation after Cube creation leaves a resumable Cube. Deletion checks ownership before removing imported storage and retains the Cube until cleanup completes.

Synchronization distinguishes unknown or incomplete history from an empty wallet. Coverage and allocation counters are separate. Wider discovery is persisted and resumes after a daemon restart; restored address high-water marks extend the historical scan before it becomes complete. Receive and change addresses are allocated durably, including change reserved for outstanding drafts.

Send uses the normal daemon draft lifecycle with imported-script construction and strict PSBT verification. Pasted or file-loaded partial signatures and hardware signatures must match the stored unsigned transaction, authenticated previous transactions, origins and owned change. Multisignature progress survives restart. Broadcast records the final transaction ID, including legacy inputs whose signed ID differs from the draft ID.

The encrypted Recovery Kit v2 wallet payload retains both branches, chain, wallet name, birthday, initial discovery window and allocated receive/change counters. Password and owner-Keychain recovery restore the same imported wallet; inherited Vault recovery remains Vault-only. Backup fingerprints include address allocation state. A Cube-only Kit becomes incomplete after import, and updating the Kit requires the existing authenticated backup ceremony. The Cube's seed is never an imported Bitcoin signer.

Imported wallets do not expose inheritance, Claim, Split-destination or new Duress enrollment capabilities. Existing strict Vault descriptors and legacy Kit/config/database formats remain supported.

## Reused code

- The existing GUI `Wallet`, normal loader/App runtime, Vault navigation and common balance/history/address screens handle both policies.
- Core public-wallet pair validation and account descriptor derivation are shared through `public_wallet_pair` and `public_wallet_source`.
- GUI descriptor/hardware input uses `wallet_source_form`, `public_account_source` and `public_hardware_source`.
- Bounded PSBT file transport uses `services::psbt_file`.
- Hardware policy construction and transport are shared while preserving Split policy identifiers and separate imported policy binding.
- Split retains its own discovery/admission rules, two-chain evidence, authorization and finalization. Import uses persistent Bitcoin synchronization and an independent spend verifier.

## Validation

The funded integration matrix is `tests/test_wallet_import.py`. It uses disposable local Bitcoin Core and Esplora-electrs processes, synthetic keys, independent multisignature signers, and explicit temporary data directories. Set `WALLET_IMPORT_REGTEST_REQUIRED=1` to make missing dependencies a failure. `coincubed/examples/import-wallet-state.rs` is a regtest-only driver for offline address-state restoration and wider-scan requests.

Run:

```sh
cargo test -p coincube-core -p coincubed --lib
cargo test -p coincube-gui --lib
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
make build

BITCOIND_PATH=/path/to/bitcoind \
WALLET_IMPORT_ELECTRS_PATH=/path/to/esplora-electrs \
COINCUBED_PATH=/path/to/coincubed \
WALLET_IMPORT_STATE_TOOL_PATH=/path/to/import-wallet-state \
WALLET_IMPORT_REGTEST_REQUIRED=1 \
python -m pytest tests/test_wallet_import.py -q
```

### Local review record (2026-10-19)

Implementation was divided among three Sol workers: shared descriptors/signing, daemon/recovery, and onboarding/UI. The coordinator integrated commits in an isolated feature branch and cross-reviewed publication, deletion, recovery and asynchronous signing boundaries.

- Core unit tests: 347 passed.
- Daemon unit tests: 320 passed.
- Funded regtest integration: 16 passed, covering all five script shapes through Esplora and Electrum, independent multisignature signers, saved partial signatures, broadcast, historical discovery, restore and concurrent address allocation.
- Full GUI unit suite on the default thread stack: 3,216 passed, 9 ignored, 0 failed. The nine suite-declared skips cover seven signed macOS keychain checks, one pinned Blake2b-node smoke check and one subprocess helper. The final provider-label correction passed focused tests of actual embedded/external daemon binding, safe endpoint captions and the shared-Vault App loading path.
- Strict all-targets Clippy, formatting and the required `make build` (including the Spark bridge) passed. These checks also passed after the final settings-label correction.
- Native macOS visual checks covered first-run and existing-Cube import entries, source selection, review, errors, synchronization, the common Vault screens, address/QR generation and wider discovery in light/dark themes. A fresh launch retained the selected custom server, expanded history coverage and previously generated receive address. All visual testing used synthetic public descriptors and isolated temporary data.
- Hardware evidence uses mocked device transport, including policy registration/signature verification. Physical device sessions were unavailable; do not treat this as a physical-device compatibility certification.
- Verification was on macOS ARM64. Other platform CI remains a release check.
- Connect server source was inspected: `services/connect/cube/types/types.go` and `services/connect/cube/service/recovery_kit.go` in `coincube-api` store the encrypted wallet descriptor as opaque ciphertext. No server protocol change or invented `hasVault` value is used. A live authenticated production backup upload was not performed.
