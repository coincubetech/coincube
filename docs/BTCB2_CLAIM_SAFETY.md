# Claim/Split safety assessment

`coincube_core::claim` evaluates supplied observations for the next preflight
stage of legacy intents. The desktop also has a separate typed ancestry path
for two ordinary `S+B` and `S+F` transactions. Neither an assessment nor a
typed read signs, broadcasts or declares funds permanently exclusive. The new
Claim/Split signing and submission gates remain closed.

The plan records the actual Bitcoin step-one transaction, its claimed prevouts,
exact chain pair, poison strategy and previously observed confirmation. A
serialized plan stores intent and an inclusion anchor, never permission. A new
assessment replaces every previous result, including after restart.

The generic legacy evaluator accepts Bitcoin mainnet/BTCB2 and Bitcoin
testnet4/BTCB2 testnet4 pairs. The retired OP_RETURN route checks the actual script is larger than 83 bytes. It also
requires a fresh active reduced_data flagday, dynamic expiry and a safety margin
against **chain median-time-past at that exact fork tip**. Scheduled, inactive,
expired, absent, unsupported, unavailable and malformed remain distinct. A
machine clock is used only to reject stale/future observation timestamps, never
to infer consensus activation. Zero policy windows refuse.

The generic evaluator does not accept ancestry as a Boolean poison strategy.
Typed Claim and Split builders instead construct ordinary Bitcoin `S+B` and
BTCB2 `S+F` transactions. Live proof requires distinct canonical coinbase
dependencies on the two chains, bounded confirmed paths, wallet ownership,
current unspent status and stable tips. A timestamp, an absent txid or a
caller's assertion is insufficient. Claimed shared prevouts must be nonempty,
duplicate-free and present in both exact transactions; each has one distinct
chain-specific funding input. Neither output may be OP_RETURN.

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

The authenticated Connect anchor adapter now binds network, tip height/hash,
fork activation and MTP. The separate anonymous two-chain Esplora adapter
requires fresh, uncached responses and brackets its reads with stable tips.
An unavailable, changed or malformed read refuses. The ancestry collector
does not use the legacy RDTS expiry rule; it derives exclusivity from the
verified input ancestry and current chain state.

The paired Claim and Split paths now have chain identity checks, retained
deposit and destination reservations, exact-pair signer requalification,
paired submission reviews and durable recovery screens. Their production gate
remains closed pending full paired GUI and hardware-signer acceptance,
independent review and Connect endpoint verification. The old route stays
closed for new transactions; historical uncertain records remain
reconcilable. Testnet4 infrastructure is deferred. See
`BTCB2_ANCESTRY_SPLIT_MIGRATION.md` for the remaining acceptance work.

## Source and validation

Tracker `coincube-api#276` safety corrections override the historical rev-5
paragraphs calling any post-fork receive permanent poison. The launch B1 brief
requires six confirmations and rechecking after reorg/expiry. Whole
`coincube-core` tests, pinned Rust formatting and strict clippy are required.
No production signer flag is enabled. Journal versions 10 and 11 and the
funding reservations must be preserved across rollback; never discard an
uncertain possible attempt. Robert retains merges, deployment and feature
activation.
