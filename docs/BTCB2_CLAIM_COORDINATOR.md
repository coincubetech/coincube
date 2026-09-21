# Dormant owned Claim step-one coordinator

`services::claim_coordinator` joins the existing opaque poison construction,
verified final witness, observation collector, anonymous preflight, private
journal and exact-byte transport. It is not an enabled Claim workflow. No UI,
signing keys, foreign-wallet import, step-two permission or automatic retry is
provided. Earlier change reservation, coin/prevout selection, and explicit user
consent to **sign** remain the caller's responsibility; the coordinator receives
already signed, cryptographically verified data.

Initial support is deliberately narrow: Bitcoin mainnet source, BTCB2 mainnet
target, owned native P2WSH with a single primary key, no relative-lock recovery
path. Multisig, Taproot and testnet refuse before journal creation. Production
uses the current admitted `CoincubeClient` and embedded source daemon. Its
Esplora selection must be exactly the same API origin's anonymous
`/api/v1/esplora/bitcoin/mainnet` endpoint, without bearer token, fallback
endpoints or global fallback selection. Other backend configurations refuse;
the coordinator never changes them. Authenticated identity/RDTS comes only from
the existing anchor client; transaction lookups/preflight/broadcast remain
anonymous. Context identity is the exact chain and endpoint selection, not a
Debug dump, credential fingerprint or serialized configuration. Account identity
must come from the admitted App session; no JWT contents are trusted as identity.

`create` binds both Cube IDs, descriptor, unsigned transaction, verified final
transaction and immutable account/provider/generation. `prepare_review` actually
collects observations, calls node preflight with the exact final transaction and
Bitcoin tip, then recollects and compares the coherent chain/RDTS view. It
requires WaitingForConfirmation for step one and Accepted preflight evidence
with matching chain, txid, wtxid, tip and generation. Source timestamps remain
server assertions: callers provide positive age/margin and explicit 1–5 second
preflight future-skew allowance. Existing anchor/collector checks retain their
strict clock requirement; synchronize API/device clocks rather than restamping.

A non-Clone opaque `Review` exposes the transaction, fee, identity and observed
view for a future confirmation screen. `confirm_and_submit` must be called only
for explicit confirmation of that exact view. It consumes the token before any
await, repeats collect→preflight→collect, and refuses changed tips, RDTS/MTP,
inclusion or presence. Chain growth during this short confirmation flow requires
a new review; this is not the daemon's long-scan policy. Each collection is at
most 30 seconds and preflight at most 15; no retry loop exists.

Only after those checks does it durably record `BroadcastUncertain`, then create
and register a one-use revocable submission gate. The transport sends the exact
verified bytes. A required monotonic gate deadline is the minimum remaining
Bitcoin/fork/deployment/preflight age budget and caller collection budget
(capped at 30 seconds), captured before journal persistence. One second is
subtracted for timestamp quantization; future preflight timestamps within the
explicit skew allowance never extend the budget. Exhausted evidence refuses.
The backend checks this same deadline under its actual lock before starting;
slow journal writes or queues cannot renew evidence.

Upstream acknowledgement is not confirmation or chain-exclusive
funds. Errors, timeout (30-second wait), dropped futures, or changed context after
intent persistence leave the saved attempt uncertain. Reconcile the exact txid;
never generate or automatically submit a replacement.

## Mandatory future UI cancellation wiring

The owner obtains `Coordinator::revoker()` before starting asynchronous work.
Logout, cancellation, Cube/account/provider replacement must call `revoke()`
**synchronously before replacing context**, then advance the shared monotonically
increasing generation. Generation-watch polling alone is not a synchronous
queued-submission barrier. One shared lock coordinates gate publication with
revocation: revocation before publication makes later registration revoke the
gate; revocation after publication revokes that exact gate. The transport's
atomic Pending→Started versus revoke transition under the actual backend lock is
the submission linearization point. Revocation after Started cannot unsend a
transaction and remains uncertain. A local RAII guard also revokes when the
submission future is dropped. The production transport runs blocking backend
work off the async executor so cancellation can be processed while queued.

`resume` revalidates the construction and starts unchecked. If a journal already
records an attempt, no new review/submission is available; `reconcile` only reads
fresh observations and updates tracking. No cached eligibility, boolean
`authorized`, timestamp, missing transaction or journal phase grants spending
permission. Step two remains uninhabited in the existing journal module.

## Evidence and remaining gates

Synthetic tests use real core builder/finalizer artifacts and HTTP preflight
fixtures, injected fresh observation sources and recording transport. They cover
journal-before-submit ordering, unavailable/stale/inactive observations, exact
review/context binding, write failures, cancellation, uncertain restart and
unsupported profiles. Injection stays module-private; production construction
always joins the actual clients and daemon. Local validation is compile/fmt/
clippy only; full GUI runtime belongs in pinned ephemeral CI.

No live acceptance is claimed. Deployed fresh/preflight routes, synchronized
separate BTCB2 indexer, operator anchor correctness, upstream policy support,
capacity/clock acceptance, end-to-end synthetic two-chain lifecycle/reorg tests,
and future UI signing/confirmation/revocation wiring remain launch gates. The
coordinator does not expose Claim or change any production feature flag.
