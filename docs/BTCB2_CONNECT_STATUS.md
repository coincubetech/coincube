# BTCB2 status through Connect

`CoincubeClient::network_status(ChainId)` reads the authenticated
`GET /api/v1/connect/networks/{network}/status` endpoint using the client's
existing session token. Only `BitcoinBlake2b` and `BitcoinBlake2bTestnet4`
are accepted; the response identity must match the requested identity.
It performs no direct node RPC and never retries against Bitcoin or mainnet.

The wire contract is Connect API's `docs/BTCB2_NODE_STATUS.md` (implemented at
`03903b89ff3e6659b65b3e178028a1e783adc39f`). The method returns `NetworkStatus`:

- `Available`: an active fork and parsed flagday schedule were reported.
- `NotConfigured`, `ConfigurationError`, `RpcUnavailable`, `Malformed`:
  no observation; these are distinct server-reported states.
- `ForkAbsent`, `ForkInactive`, `RdtsAbsent`, `RdtsUnsupported`:
  the typed observation is retained even though the probe is unavailable.

These typed HTTP 503 responses are `Ok(NetworkStatus)`; callers must inspect
`state`, not just whether the request returned `Ok`. HTTP 401, 404 (including
account flag off), 429 and other failures remain
`NetworkStatusError::Request(CoincubeError)` with their HTTP classifications.
Malformed JSON, missing required fields, unknown states, mismatched identities,
and contradictory status/envelope/observation combinations yield
`NetworkStatusError::InvalidResponse` with fixed diagnostic text.

`observation` retains node tip height, independent fork and RDTS heights,
required booleans, and signed `i64` RDTS `expiry_time`. An RDTS flagday with
`active: false` is a valid report, including after expiry. No expiry is
hard-coded or inferred from the desktop clock. Null fork means absent;
missing fork is malformed. Unknown additional fields are ignored.

**This is an observation, not chain authentication or permission to spend.**
The configured server/node can claim a schedule without proving the chain.
Claim/Split must still verify chain consistency, positive poison evidence,
RDTS enforcement and expiry margin, confirmations and reorg safety. This
client does not activate those flows or lift feature/signer gates.

## Developer verification and deployment boundary

Run with repository-pinned Rust 1.97.1 and a synthetic compile-time SDK key:

```sh
BREEZ_API_KEY=synthetic-test-key cargo test -p coincube-gui --locked
cargo fmt --all -- --check
BREEZ_API_KEY=synthetic-test-key cargo clippy -p coincube-gui --all-targets --locked -- -D warnings
```

Tests use loopback HTTP fixtures and synthetic tokens; they make no live
node or wallet calls. Live acceptance awaits the infrastructure owner:
separate BTCB2 RPC/electrs tailnet access, completed stable indexing,
intended-chain checks, and dev authenticated endpoint acceptance. Server
loopback `127.0.0.1:3002` is not a usable Mac/droplet endpoint. The existing
Bitcoin Esplora service must never be supplied as BTCB2 upstream.

The API requires its existing session/account flag plus per-network optional
`BTCB2_NODE_RPC_*` / `BTCB2_TESTNET4_NODE_RPC_*` configuration. See the API
runbook for secret-free setup and rollback. No additional desktop env variable,
local node, production flag flip or service restart is introduced here.
Rollback the client change by reverting its commit; leave production BTCB2
disabled until the coordinated launch gates pass. Testnet4 remains separately
configured or explicitly unavailable and never falls back to mainnet.
