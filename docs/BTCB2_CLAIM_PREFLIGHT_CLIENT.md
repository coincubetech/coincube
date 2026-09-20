# Dormant transaction preflight client

`services::claim_preflight::PreflightClient` reads optional operator-node policy
observations. It has no UI caller and performs no signing, broadcast, local-node
startup, raw fork-header parsing or wallet writes. An accepted result is evidence
of the configured node's policy at that observation, never spend authorization,
final-witness verification, poison proof or a later broadcast guarantee.

The constructor receives only an exact API origin and a revocable generation;
it never accepts CoincubeClient, JWT, account/device headers or transaction-linked
metadata. Requests use a separate anonymous client, refuse redirects and send
only fixed JSON fields `transaction` and `tip_hash` to the two reviewed POST
routes `/api/v1/esplora/{bitcoin|bitcoin-blake2b}/mainnet/tx/preflight`.
No generic RPC method, fee override, fallback endpoint or retry loop exists.
Testnet4 refuses before I/O rather than mapping to legacy testnet or mainnet.

The caller supplies the actual finalized Transaction, fresh expected tip and
explicit FreshnessPolicy. Structural checks require nonempty inputs/outputs and
at most400,000 weight units, matching the API bound. Without descriptors/prevouts
the reader cannot cryptographically verify final witnesses; that remains an
independent caller gate, not inferred from the presence of witness bytes.

The total request/read deadline is15seconds, covering the API's bounded body and
RPC windows. Responses stream into at most16KiB and require Cache-Control:no-store.
Requests also send Cache-Control:no-cache. Unknown/malformed or contradictory
success/state/result matrices refuse. Valid HTTP200 evidence binds exact chain,
txid, wtxid, expected tip, generation and original server collection timestamp.
Allowed=false remains a policy rejection with a bounded machine-code reason;
arbitrary node diagnostics are never returned by this client.

HTTP503 with a typed unavailable network state remains distinct from503 capacity
or wrapper failures without data. Typed HTTP errors preserve an optional integer
Retry-After hint; no automatic retry follows it. A partial result on503 refuses.
Flag-off404, malformed-input400, throttled429, unexpected redirects and transport
failures never become accepted evidence. This contract depends on the separately
reviewed optional API preflight service; an older API must fail closed.

Freshness requires caller-supplied positive max_age_seconds and an explicit
max_future_skew_seconds in1..=5. There is no default or silent widening. Values
beyond the five-second maximum refuse, as do observations beyond the selected
past/future bounds. observed_at remains the server assertion, not a receipt-time
restamp. Keep API/device clocks synchronized. Caller consumption must recheck
current generation, chain/tip and freshness because delivery can race logout or
a new block. Mempool contents can change even while the tip remains unchanged.

The caller increments the shared generation on account/provider/Cube changes or
cancellation; closure of all senders cancels too. In-flight response futures drop
on invalidation. No result persistence, background polling or cached permission
is installed. Future orchestration still needs owned destinations, fee/current
UTXO checks, six-confirmation and reorg/expiry checks, and final-witness replay
validation before any explicit broadcast path can be exposed.

Synthetic HTTP fixtures cover both exact routes and anonymous headers, binding
and witness mutation, service/error matrices, size/marker/redirect refusal,
clock boundary values and cancellation. Local validation compiles fixtures only;
full GUI runtime belongs in ephemeral pinned CI. No live infrastructure or
existing-wallet acceptance is claimed. Keep server CLAIM_PREFLIGHT_ENABLED=false
until infrastructure/synthetic end-to-end acceptance and Robert's release decision.
Rollback disables that server option; this dormant client treats404 as unavailable.
