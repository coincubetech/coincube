# BTCB2 fiat display through Connect

BTCB2 quotes use authenticated `GET /api/v1/price/bitcoin-blake2b?fiat=USD`
through the current App's Connect client. Bitcoin's existing price source and
shared cache remain separate. BTCB2 skips the global Bitcoin scheduler and
its unconditional BTC/USD conversion request. It does not query CoinGecko,
mempool.space or Bitcoin's Connect exchange-rate endpoint. Testnet4 makes no
mainnet pricing request.

Each BTCB2 request carries the App generation and exact ChainId. Only the
current request for the selected currency can update that App's cache. Closing
or reopening a Cube creates a different generation; old replies are ignored.
A BTCB2 quote never seeds the Bitcoin/USD cache used for stablecoin conversion.

The typed quote preserves price, fiat, source names/prices/timestamps, median,
stale and updated_at. Fiat display requires the matching currency, two distinct
approved sources (nonkyc/neoxa), finite positive prices, matching arithmetic
median, source spread at most 10%, and timestamps at most 120 seconds old and
never in the future. Cached quotes also expire against the request's monotonic
clock, so a frozen wall clock cannot keep a quote indefinitely. updated_at must equal the oldest source timestamp. These
are conservative desktop limits; the server's stale flag always refuses a
quote even if these checks pass. Wider/older server policy cannot weaken them.

Polls are no more frequent than once per minute for an unchanged currency.
Stale, disabled, unavailable, malformed or expired data clears the fiat price,
uses native units and disables display-mode changes. Preferences stay enabled
so a later successful quote can restore fiat availability; it does not force
automatic toggling back to fiat. A saved fiat display mode is not used on BTCB2
startup before a usable quote arrives. Currency preferences are local ISO
currency choices; an unsupported currency response remains unavailable.
BTCB2 settings select only Connect and persist in that chain's directory.

## Verification and launch gates

Run full `cargo test -p coincube-gui --locked`, `cargo fmt --all -- --check`
and `cargo clippy -p coincube-gui --all-targets --locked -- -D warnings` with
repository-pinned Rust and a synthetic compile-time BREEZ_API_KEY. Mock tests
cover authenticated route/currency binding, source/freshness/spread failures,
HTTP 401/404/429/503, cache generation/chain isolation and settings persistence.
These fixtures are not live exchange or infrastructure acceptance.

Connect's production exchange adapters remain blocked on verified official
market/ticker captures and conversion semantics. An empty cache returns 503,
so native BTCB2 units are the expected behavior until that handoff completes.
No deployment, server restart, flag flip or existing-wallet operation is part
of this client change. Robert owns those actions and the final launch decision.
