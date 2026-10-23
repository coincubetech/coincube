# Claim observation boundaries

`services::claim_observation` collects supplied chain observations and invokes
`coincube_core::claim::assess` for legacy intent evaluation. Its HTTP adapter
also powers the production two-deposit status checks and separate typed
Bitcoin/BTCB2 ancestry collectors. These reads perform no wallet writes,
signing or broadcast.
`ObservationsEligibleForPreflight` remains an observation assessment, never
spend permission or a persistent Split label.

The caller supplies an actual `ClaimPlan`, positive maximum-age/expiry-margin
policy and a collection budget up to 30 seconds. Exact mainnet/mainnet or
testnet4/testnet4 pairs and transaction/claimed-prevout structure are validated
before I/O for the generic legacy assessment. Input ancestry uses separate
typed collectors and validators: each funding input must trace through a
bounded, confirmed path to a canonical coinbase that differs between the two
chains at the dependency root. A post-fork timestamp or missing transaction
is never a proof of chain exclusivity.

Automatic Claim ancestry discovery may inspect up to 512 distinct transactions,
4,096 distinct dependency edges and 4,000,000 bytes of raw graph data per candidate.
It retains at most 64 links and 4,000,000 bytes in any proof, with a 400,000-byte limit per
transaction. A positively confirmed pre-fork transaction may close a search
branch only after its raw txid, canonical height mapping and pinned authenticated
fork height agree; a completed negative search rechecks every closed frontier,
the tip, anchor, freshness and session. Both funding searches and candidate
loops share one 90-second deadline, 4,096 discovery requests and 16 MiB of
response data. Final collection remains a separate 30-second phase with at
most 256 remaining requests on those same counters. Standalone collection
and retained-path requalification retain their existing bounds. Repeating a
deterministic search-limit result does not extend the graph or prove that a
deposit has qualifying ancestry.

Claim ancestry now emits identifier-free timing snapshots for each chain's
search result and for 5-second request, 30-second collection and 90-second
search deadlines. The deposit-check screen can copy the current attempt's
read-only report after a result. A completed deposit check also records a fixed
phase, result category and elapsed time even when it stops before ancestry
observation begins; an outer 150-second expiry has its own category. The report
never copies free-form failure text. Each ancestry snapshot contains a typed
stage, elapsed time, shared charged requests/bytes and per-chain visited,
pre-fork pruned, candidate root, shared-root and unsupported-root counts. It
contains no transaction or
outpoint IDs, URLs, addresses, account data or credentials. A negative search
can thus distinguish a pruned pre-fork frontier from rejected candidate roots,
but neither the report nor a log line is spend authority. Reports clear on a
new check, cancellation or context revocation. These diagnostics do not change
the established deadlines, budgets or proof gates.

Each search node now fetches raw bytes and its fresh full-transaction metadata
at the same time under the same shared bounds. Raw-read failures retain their
precedence; metadata identity, freshness and canonical pre-fork checks still
complete before the node advances. Graph traversal and root qualification
remain ordered. At most two node reads run together per chain, or four across
the paired search. The 5-second request, 30-second collection and 90-second
search deadlines, shared request/byte budgets, core proof rules and signing
gates are unchanged.

In a synthetic two-chain fixture, each qualifying root sat behind 121 canonical
pre-fork siblings (123 visited transactions, 121 pruned nodes and one root per
chain). Only raw and full-transaction metadata reads had 380 ms mock latency;
height, anchor, root and final collection reads answered immediately. The
serial search hit its 90-second deadline after 90.99 seconds. With paired node
reads, the same fixture completed final paired collection in 49.63 seconds.
This establishes the bounded-latency improvement under the fixture, not the
cause or eligibility of any live deposit.

The source is one immutable API/provider/account context. Its authenticated
anchor must come from the typed `CoincubeClient::network_anchor` endpoint. That
response already binds dynamic RDTS, MTP, hash and height coherently; there is no
need to combine the older height-only `/status` with an unrelated tip timestamp.
Unavailable anchor states stay typed failures with no fabricated tip. RDTS
inactive, scheduled, expired, absent and malformed are not spend authorization.
Wall clock validates observation freshness only; consensus expiry uses chain MTP.

Indexer reads must return `FreshRead`, constructed from actual response headers
containing both `X-Cache: BYPASS` and a `Cache-Control: no-store` directive. The
adapter must request `X-Coincube-Observation: fresh`, preserve explicit ChainId,
use the exact configured Connect endpoint, and never substitute Bitcoin/public
explorer data. Transaction lookups remain anonymous: no JWT or device headers.
Clearing a CoincubeClient token alone does not remove its device headers. The response-aware `HttpObservationSource` requires the reviewed API fresh
contract; ordinary cached Esplora methods are not fresh evidence.

The legacy OP_RETURN collection reads Bitcoin tip and a fork anchor, checks indexer agreement at the
fork anchor height, and reads both transaction states plus Bitcoin inclusion
hash. It repeats indexer/transaction/inclusion checks, then re-reads Bitcoin tip
and authenticated fork anchor. Any change discards the collection rather than
retrying or retaining eligibility. The oldest relevant timestamp is retained;
late reads never refresh older observations. Core checks six-confirmation depth,
prior-inclusion reorgs, actual OP_RETURN poison and dynamic expiry policy.

A successful fresh 404 may be represented as `Absent`; 503/transport/malformed
responses must remain errors. Even explicit absence is only an extra check for
the legacy OP_RETURN route. The source trait is an observation trust boundary, not a
cryptographic proof constructor: fabricated source values can defeat any local
observation evaluator. Full consensus, self-transfer ownership, fee/UTXO policy,
final-witness replay validation and pre-broadcast acceptance remain separate.

The HTTP tip adapter validates best-chain membership and complete height/hash
metadata using tip hash, block status, hash at height and a repeated tip hash.
It only requests allowlisted fresh paths; it never reads raw headers.

The caller owns a watch generation sender and increments it on logout, account,
provider, Cube/chain replacement or explicit cancellation. Generation changes
and sender closure cancel in-flight work; no source task is detached. A final
generation check also catches changes during an immediately-ready source call.
The returned generation must also match when the caller consumes the result;
delivery itself can race revocation. The source must release I/O when its future
is dropped. No automatic retry loop or cache of eligibility is installed.

The paired ancestry collector additionally authenticates every shared
pre-fork outpoint as the same canonical transaction output on both chains and
requires both funding and shared inputs unspent before the first send. Once
Bitcoin confirms, a distinct post-Bitcoin collector checks the exact signed
Bitcoin witness in a canonical block at six confirmations, requires its inputs
spent on Bitcoin, and requires the BTCB2 funding and shared inputs still
unspent on BTCB2. Each collector brackets stable two-chain tips and the
account generation; neither grants signer or broadcast access on its own.

Fixture tests cover chain isolation, wrong hashes/txids, absence versus 503,
5/6 confirmation depth, inclusion/tip/presence races, prior-confirmation reorg,
stale/future timestamps, dynamic RDTS states, malformed anchors, deadline and
account-generation cancellation. Local unit tests and pinned two-chain regtest
also cover portions of the new ancestry path. They are not production signing
acceptance or a completed Claim/Split workflow.

The current API exposes Bitcoin mainnet and legacy Bitcoin testnet, not Bitcoin
testnet4. The trait fixtures cover the exact testnet4 pair, but a future concrete
adapter must refuse that unconfigured pair; it must never map Bitcoin testnet4
to legacy testnet or mainnet. Testnet4 infrastructure remains deferred.

## HTTP adapter boundary

`HttpObservationSource::new` freezes the authenticated client/API origin and a
revocable generation. Only the mainnet Bitcoin/BTCB2 pair currently constructs;
unsupported pairs fail before I/O. Anonymous requests use a separately built
client with no JWT, cookies or device headers, no redirects and a five-second
request deadline. The origin must not contain userinfo, query, fragment or a
path prefix. No alternate provider or chain fallback exists.

Fresh observation responses require all three acknowledgement markers.
Fresh `/tx/{txid}` JSON metadata has a separate 2 MiB response cap because its
input and prevout details can be much larger than the raw transaction. Other
fresh reads retain the 256 KiB cap; raw transaction hex retains its 400,000-byte
decoded cap. The shared 16 MiB collection budget also charges the larger read.
An oversized `/tx` response is a collection-limit refusal, while malformed JSON
remains a malformed-response refusal. Request-start wall timestamps are retained
conservatively. `/tx` responses must contain the requested txid and complete,
consistent confirmation fields; `/tx/.../status` alone cannot establish that body
binding. Only an explicit fresh 404 becomes absence. Other HTTP codes remain
typed failures.

The shared authenticated anchor reader now disables redirects, uses a ten-second
HTTP timeout, and streams at most 64 KiB. Existing typed state/error parsing is
reused. The adapter additionally bounds that call to five seconds and cancels
all reads on generation revocation. Anchor auth remains separate from anonymous
transaction lookups. No cache, retries or background collector is installed.

Local HTTP fixture tests use isolated mock servers. They exercise the reader
contract without opening a production signer gate.

Device identity headers are captured once when CoincubeClient is constructed and
reused across token replacement, clearing and bounded anchor requests. Anchor
polling never regenerates a fingerprint or reads device identity from disk.
A fresh client construction remains the explicit identity refresh boundary.
Anonymous observation requests still use their own header-free transport.
