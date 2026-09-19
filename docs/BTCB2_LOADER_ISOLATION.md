# BTCB2 loader isolation prerequisite

The loader now follows the Cube's explicit `ChainId` for wallet/socket,
first-scan, stored display settings and managed-node resources. Encoding
`bitcoin::Network` remains for address and node network-section formats.
BTCB2 pending-node detection and log subscriptions use `bitcoind-blake2b`,
including its distinct testnet4 subdirectory, never Bitcoin's node directory.

Connect Esplora startup needs neither a local node nor managed Tor. When the
optional managed route is selected, loader preflight and the new chain-aware
Tor boundary keep BTCB2 from provisioning, reconfiguring or stopping Bitcoin's
shared Tor process. BTCB2 managed Tor returns an explicit unavailable error
before configuration access. Bitcoin still uses its existing implementation.
A separate per-family Tor lifecycle is required before offering that optional
managed path for BTCB2; this patch does not claim to implement it.

The app runtime gate remains dormant. Necessary remaining activation work is
chain-preserving creation/restore and seed lifecycle, authenticated signing
identity/feature checks, status/price consumption and synthetic end-to-end
acceptance. Connect-only backend selection can proceed independently of the
optional managed-Tor prerequisite, but must not be exposed by changing the
runtime enum alone. No actual node, Tor process, wallet or existing database
was changed by this task. Robert retains merge and deployment control.
