# Bitcoin Blake2b — Esplora tip timestamps without raw headers

Scope: coincube-api#290. One daemon change in
`coincubed/src/bitcoin/esplora/client.rs::tip_time`, plus this note. No
activation, no chain-identity policy, no dependency change.

## Why

From the BLAKE2b hardfork height (mainnet 961,640) block headers are 164-byte
v2 headers. rust-bitcoin's `block::Header` is the 80-byte Bitcoin layout, so
`GET /block/<hash>/header` decoded through `esplora_client::BlockingClient::get_header_by_hash`
cannot represent a post-fork tip (plan audit F5). `tip_time` was the one
Esplora code path in `coincubed` that decoded a raw header for a block that
can be post-fork.

## What changed

`tip_time` now issues a single `GET /blocks` and reads the tip's `timestamp`
from the JSON block summaries (`esplora_client::BlockSummary`, present in the
pinned `esplora-client` 0.8.0 that `bdk_esplora` 0.15.0 resolves — there is
no per-hash JSON metadata call in that version, so `/blocks` is the available
JSON metadata API). The tip is the summary with the greatest `height`
(Esplora and mempool.space both document newest-first, but the choice does
not depend on order). Id, height and timestamp are read from the same JSON object of the same
response, so the value is bound to one block by construction. (The previous
two-request shape — tip hash, then the header *for that captured hash* — was
also bound to one block; the change here is the extended-header compatibility
and the single request per call, not a race fix.)

Failure semantics: an empty list or a timestamp outside `u32` is
`Error::TipMetadata`, a parse failure is `Error::Client`; nothing is defaulted,
and the trait boundary maps every error to `None` as before. Provider
selection, 402/429 cooldown, transport-failure cooldown, 5xx fall-through,
non-retryable short-circuit and shutdown abort are unchanged — the one request
goes through `try_in_order` like every other call. Bitcoin summaries return
the same value the header did.

Unknown JSON fields (a BLAKE2b indexer may add fork-specific ones) are
ignored by serde and covered by the mock test.

## What this does not claim

- It does not authenticate the chain: the summary is what the configured
  provider reports for its tip. Which chain that provider indexes is bound by
  endpoint selection (Connect routes, #282), not by this read.
- It is not "Esplora support for BTCB2 is complete". Remaining raw-format
  paths in `coincubed`'s Esplora backend, audited at `8dd8b396`:
  - `genesis_block_timestamp` → `get_header_by_hash(genesis)`: the genesis
    block is shared pre-fork history (80-byte header on both chains), so it
    is unaffected; deliberately not changed here.
  - `chain_tip` → `/blocks/tip/hash` + `/block/<hash>/status` (JSON): fine.
  - `bdk_esplora` `sync`/`full_scan` (`crates/esplora/src/blocking_ext.rs`):
    `/blocks` summaries, `/block-height/<h>`, `/scripthash/<h>/txs`,
    `/tx/<txid>` (raw **transaction** hex — transaction serialisation is
    unchanged by the fork) and `/tx/<txid>/status`: no raw headers.
  - `get_block_by_hash` (`/block/<hash>/raw`) and merkle-proof calls: not used
    by `coincubed`.
  So after this change no `coincubed` Esplora path decodes a post-fork raw
  header. The Electrum backend still does (`electrum/client.rs`
  `genesis_block_header` and header subscriptions) and remains unsuitable for
  BTCB2; that is a separate gate.

## Tests

`coincubed/src/bitcoin/esplora/client.rs` `tests`: a dependency-free mock
Esplora (`std::net::TcpListener`) records every request path and answers from
a canned table. The mock has a bounded
lifetime (stop flag + wake-connect + join on `Drop`) and reads a request head
through its `\r\n\r\n` terminator under an 8 KiB size bound applied before
the terminator is honoured, reads capped to the remaining capacity, an
absolute per-request deadline with socket waits capped to the time remaining,
and a stop-flag check between reads; an unterminated, oversized or timed-out
head is answered 400 and closed. Covered: BTCB2-style summaries with
extra fields → timestamp, exactly one `/blocks` request and no `/header`
request; Bitcoin summaries in arbitrary order → highest block; empty list /
`u32` overflow → `TipMetadata`, missing or negative timestamp / non-JSON →
`Client`, never a value; one request per call, each call returning its own
snapshot's tip; 429 primary → cooldown and fallback serves, cooled primary
not re-asked; 5xx falls through without cooldown; all providers down →
error; shutdown abort → no request; mock teardown completes with no request,
with a partial request held open, and with a peer trickling one byte per
50 ms; a head split across TCP writes is routed; an oversized head is refused
whether or not it is terminated; at helper level, a dripping peer ends at the
deadline and a raised stop flag ends the read within one slice.
