# BTCB2 Connect backend integration

Audit base: `b45a9d4853319133593d1e01992f5ac9a8dc8676` (2026-09-19).
This is preparation for runtime activation; this build still refuses BTCB2
Cube creation/opening. A server feature flag does not override that gate.

## Implemented backend foundations

- Shared `coincube_core::chain::ChainId` distinguishes mainnet and testnet4
  BTCB2 from their Bitcoin encoding twins. Config, database and directory
  identity use the chain; addresses use its `bitcoin_network()` projection.
- `installer::connect_esplora_config` gives either BTCB2 chain exactly one
  authenticated Connect URL, with no public Bitcoin fallbacks. Bitcoin's
  existing provider selection is preserved.
- `loader::start_daemon` checks config chain against Cube chain before config
  migration or daemon startup. Legacy migration does not recognize BTCB2
  strings and leaves those configs untouched.
- Esplora tip time uses `/blocks` JSON. Genesis/rescan lower-bound time now
  uses `/blocks/0`, requiring exactly one height-zero summary and a `u32`
  timestamp; malformed metadata and upstream errors remain errors at the
  client boundary. The existing `BitcoinInterface` genesis method maps errors
  to zero, unchanged by this patch; that is not evidence of backend health.
- The locked BDK Esplora implementation (`8936e828`, `blocking_ext.rs`)
  uses JSON block summaries, block hashes, scripthash transactions and
  transaction/output status for `sync`/`full_scan`. Its transaction bytes are
  decoded as transactions, not block headers. The daemon Esplora client now
  contains no raw block/header decoder calls. Electrum is a separate backend
  and must remain unavailable for BTCB2.

## Gates before activation

Do not enable the runtime merely because the node is synchronized:

1. Finish installer selection using authenticated chain identity. Current
   installer constructors still take `bitcoin::Network`; `Context` retains
   chain through `bitcoin_config` and `set_bitcoin_network`, but that does not
   add a BTCB2 creation entry. The generic `DefineNode` still includes Electrum.
2. Finish optional managed-node startup with chain-aware Tor configuration and
   pending-node paths (`loader::start_daemon` currently uses Bitcoin encoding
   for `prepare_inbound_tor` and a Bitcoin internal-directory helper). Keep
   Connect Esplora usable without any local node.
3. Wire fresh P2WSH Cube creation and feature hiding through the runtime gate;
   keep unsupported Taproot signing and Keychain LAN closed. Claim/Split
   entry/state machinery is not present in `UserFlow` at the audited base.
   Claim/Split must use verified poison evidence, live expiry, confirmation
   depth and reorg rechecks per API tracker #276, never timestamp/absence
   heuristics.
4. Integrate the authenticated typed status client and separate BTCB2 price
   consumer. Pricing unavailable/stale must show native units; no Bitcoin
   asset price substitution.
5. Complete synthetic end-to-end, signer and flag-off acceptance before
   changing the hard `RuntimeSupport::Dormant` gates. Existing hot unified
   signing and replay-model code does not by itself prove those entry flows.

## Infrastructure handoff

The server setup is owned separately. Bitcoin Esplora
`100.125.72.49:3000` must never serve BTCB2. Reported BTCB2 RPC
`127.0.0.1:39332` and REST `127.0.0.1:3002` are **server loopback** addresses,
not Mac/API deployment endpoints. Indexing completion, stability, tailnet
access and live authenticated requests are pending infrastructure acceptance.
Do not modify/restart services from this integration task. Testnet4 has a
separate configuration and remains unavailable until its infrastructure exists.

After that handoff, use a disposable wallet/datadir and a dedicated development
Connect account to check correct-chain balance, status, scan and broadcast;
then stop the disposable upstream and verify unavailable behavior without a
Bitcoin request. Mock tests do not satisfy this live acceptance gate.

Rollback: keep the server BTCB2 feature flag off and retain the dormant app
runtime until acceptance. Robert owns deployment, flags and merges. Reverting
this JSON timestamp patch restores the old raw-header read only; it does not
alter datadirs, nodes, wallets, signer policy or Bitcoin provider selection.
