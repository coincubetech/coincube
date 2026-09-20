# Dormant Claim observation collector

`services::claim_observation` collects supplied chain observations and invokes
`coincube_core::claim::assess`. It includes an opt-in HTTP adapter but has no application caller, and performs no wallet writes, signing, broadcast, import or UI exposure.
`ObservationsEligibleForPreflight` remains an observation assessment, never
spend permission or a persistent Split label.

The caller supplies an actual `ClaimPlan`, positive maximum-age/expiry-margin
policy and a collection budget up to 30 seconds. Exact mainnet/mainnet or
testnet4/testnet4 pairs and transaction/claimed-prevout structure are validated
before I/O. Input ancestry remains unsupported. A post-fork timestamp or missing
transaction is never a proof of chain exclusivity.

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

Collection reads Bitcoin tip and a fork anchor, checks indexer agreement at the
fork anchor height, and reads both transaction states plus Bitcoin inclusion
hash. It repeats indexer/transaction/inclusion checks, then re-reads Bitcoin tip
and authenticated fork anchor. Any change discards the collection rather than
retrying or retaining eligibility. The oldest relevant timestamp is retained;
late reads never refresh older observations. Core checks six-confirmation depth,
prior-inclusion reorgs, actual OP_RETURN poison and dynamic expiry policy.

A successful fresh 404 may be represented as `Absent`; 503/transport/malformed
responses must remain errors. Even explicit absence is only an extra check for
the OP_RETURN route. The source trait is an observation trust boundary, not a
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

Fixture tests cover chain isolation, wrong hashes/txids, absence versus 503,
5/6 confirmation depth, inclusion/tip/presence races, prior-confirmation reorg,
stale/future timestamps, dynamic RDTS states, malformed anchors, deadline and
account-generation cancellation. Local validation is compile/fmt/clippy only;
full GUI runtime tests belong in ephemeral repository-pinned CI. These fixtures
are not live infrastructure acceptance or a completed Claim/Split workflow.

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

Every anonymous response requires all three fresh acknowledgement markers.
Bodies are streamed with a 256 KiB cap (oversized legitimate transactions remain
unavailable). Request-start wall timestamps are retained conservatively. `/tx`
responses must contain the requested txid and complete, consistent confirmation
fields; `/tx/.../status` alone cannot establish that body binding. Only an explicit
fresh 404 becomes absence. Other HTTP codes remain typed failures.

The shared authenticated anchor reader now disables redirects, uses a ten-second
HTTP timeout, and streams at most 64 KiB. Existing typed state/error parsing is
reused. The adapter additionally bounds that call to five seconds and cancels
all reads on generation revocation. Anchor auth remains separate from anonymous
transaction lookups. No cache, retries or background collector is installed.

Local HTTP fixture tests are compiled locally, executed only in ephemeral CI;
no live endpoint or production flag is exercised.
