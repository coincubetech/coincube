# Dormant Claim/Split safety assessment

`coincube_core::claim` evaluates supplied observations for the next preflight
stage. Nothing calls it from the application yet. It never signs, broadcasts,
constructs GUI `SplitEvidence`, or declares funds permanently exclusive. The
existing Split label remains unreachable. This is a core prerequisite, not a
completed Claim/Split workflow or live acceptance.

The plan records the actual Bitcoin step-one transaction, its claimed prevouts,
exact chain pair, poison strategy and previously observed confirmation. A
serialized plan stores intent and an inclusion anchor, never permission. A new
assessment replaces every previous result, including after restart.

Only Bitcoin mainnet/BTCB2 and Bitcoin testnet4/BTCB2 testnet4 pairs are accepted.
The OP_RETURN route checks the actual script is larger than 83 bytes. It also
requires a fresh active reduced_data flagday, dynamic expiry and a safety margin
against **chain median-time-past at that exact fork tip**. Scheduled, inactive,
expired, absent, unsupported, unavailable and malformed remain distinct. A
machine clock is used only to reject stale/future observation timestamps, never
to infer consensus activation. Zero policy windows refuse.

Input-ancestry poison is unsupported. There is no verified ancestry constructor
in this slice, so a timestamp, an absent txid or a caller's Boolean cannot make
that route eligible. Step one's absence on BTCB2 is a necessary additional check
for the OP_RETURN route, not sufficient poison evidence on its own. Presence or
unknown status refuses. Claimed prevouts must be a nonempty duplicate-free subset
of the actual step-one inputs; duplicate/null inputs and a mismatched txid refuse.

Bitcoin step one needs at least six confirmations. Inclusion is compared with a
fresh block-hash-at-height read and the last recorded inclusion anchor. A changed
anchor, removed confirmation or mismatched current block returns a reorg state;
an unavailable query remains unknown. Assessment without fresh matching tip
rechecks returns `NeedsPreflightRecheck`. The most permissive result is named
`ObservationsEligibleForPreflight` deliberately: it is not spend authorization,
consensus validation, a persistent split state or evidence for the replay label.

## Observation trust and remaining integration

These public structs describe **observations**, not cryptographic proofs. Core
does not authenticate RPC/Esplora, verify proof of work or Merkle inclusion,
validate destinations as self-transfer addresses, inspect signatures, or check
all current UTXO/spendability and fee policies. A caller must establish those
properties and recheck immediately before any signing/broadcast. Supplying
fabricated internally consistent observations defeats an observation evaluator;
its result must never be treated as independently verified chain evidence.

The authenticated typed Connect status client in desktop PR #408 supplies
network, tip height, fork activation and `reduced_data` height/expiry/active.
It does **not** provide a tip hash or median-time-past. A future adapter must
obtain chain-bound MTP and bracket the typed status request with identical
before/after fork tip hashes before attaching its anchor. No adapter exists here;
missing inputs cannot be manufactured from local time or a block timestamp.
The adapter must also query both chains' step-one status and current Bitcoin
block hash, then obtain new tip reads immediately before preflight.

Outstanding: authenticated observation collection, current best-chain/inclusion
verification, state persistence and reorg presentation, descriptor scanning and
ownership checks, pre-broadcast validation, construction and signing of both
steps, foreign-wallet/hardware interop, and two-chain regtest rejection/acceptance.
No endpoint, testnet4 infrastructure or live service is assumed ready. Use only
synthetic fixtures until infrastructure handoff and owner authorization.

## Source and validation

Tracker `coincube-api#276` safety corrections override the historical rev-5
paragraphs calling any post-fork receive permanent poison. The launch B1 brief
requires six confirmations and rechecking after reorg/expiry. Whole
`coincube-core` tests, pinned Rust formatting and strict clippy are required.
This module adds no dependencies, networking, environment variables or runtime
flag. Rollback removes the dormant module/export; no wallet or database migration
is involved. Robert retains merges, deployment and feature activation.
