# BTCB2 loader isolation

The authenticated fork loader starts a managed companion or an active local
backend using explicit `ChainId`. Bitcoin Blake2b node files, cookie paths, locks,
logs and configured-flavour records use `bitcoind-blake2b`; Bitcoin keeps
`bitcoind`. Both mainnet and testnet4 retain their exact chain directory.

Connect-only startup does not start a node or touch Tor. With a local companion,
the loader selects the fork binary and validates its fork schedule before reuse.
It clears only the fork config's stale inbound settings and uses outbound peers;
it does not reconfigure or stop Bitcoin's Tor process.

A synced node becomes the wallet backend only through the daemon's explicit
embedded local-node admission. On restart, an active local backend avoids the
Connect feature-status request, while existing Cube session/unlock authentication
remains unchanged. The GUI preserves the managed handle and pending configuration
across installer, loader and runtime handoff. Configs with an ordinary Bitcoin
cookie, a remote address or an inconsistent chain are refused.

Generic startup remains dormant for forks. External sockets, migration and
remote backends remain closed. See `BTCB2_MANAGED_NODE.md` for the local trust
boundary, activation checks and release limitations.
