# Retired exact-byte poison submission

This document describes a historical transport and its safety tests. The
90-byte OP_RETURN Claim/Split route has no production signer or submission
caller. New transactions use the paired input-ancestry path described in
`BTCB2_ANCESTRY_SPLIT_MIGRATION.md`; this transport must not be reconnected to
it. Existing uncertain records still require exact-transaction reconciliation.

`DaemonControl::submit_verified_poison` accepts only the opaque core
`VerifiedPoisonTransfer`. It requires Bitcoin mainnet and the daemon's exact
wallet descriptor before calling its selected Bitcoin backend. Testnet4, fork
chains and mismatched descriptors refuse before transport. The GUI Daemon trait
provides an unsupported default for external/remote implementations; only the
embedded implementation forwards this in-process artifact. There is no JSON-RPC
method, proof deserialization, arbitrary transaction argument or UI caller.

The transport sends the artifact transaction without loading or finalizing a
stored PSBT by txid. Its exact witness bytes and wtxid therefore remain those
verified by core and supplied to a preceding preflight. Ordinary Bitcoin
`broadcast_spend` behavior is unchanged. No database write, automatic retry or
poller wait occurs here. Normal daemon polling can later observe the transaction.

`UpstreamAccepted` means only that the selected backend acknowledged submission,
not confirmation or inclusion. Any backend error returns `Uncertain` with the
same txid/wtxid, without leaking raw backend diagnostics. It may already have
submitted the transaction; reconciliation must use these exact identities.

This transport never granted Claim authorization. Its old design required
account/Cube/provider binding, a reserved owned destination, fresh RDTS and
UTXO observations, exact-transaction preflight, explicit confirmation and
durable uncertain-intent journaling. Those conditions do not make it an
approved fallback for new transactions. Signature integrity alone does not
prove maturity, current unspentness or replay exclusivity. No production
caller is enabled.

Synthetic daemon tests construct/sign/finalize realistic native P2WSH fixtures,
record exact submitted bytes, refuse wrong chains/descriptors before calls and
retain uncertainty without retry on transport failure. Full daemon tests and
strict clippy validate the daemon; GUI adapter compilation/pinned CI is separate,
and no local GUI runtime/keyring tests are authorized.

## Revocation and the start boundary

`SubmissionGate::new(&verified, not_after)` returns a one-use transaction-bound gate and a
cloneable `SubmissionRevoker`. Neither is authorization. The coordinator keeps
the gate private, binds its own context/generation and journals intent first,
then revokes on logout/provider/context changes. Gate identity includes chain,
txid and wtxid, so another valid witness for the same txid cannot reuse it.

The daemon acquires the actual Bitcoin backend mutex before atomically changing
Pending to Started immediately before calling broadcast, with no await between.
Revocation winning that transition yields Revoked and no transport call, even
when the submission was queued behind embedded/backend locks. Started cannot be
reset or reused; revocation after it cannot retract submission. A dropped outer
future after Started must remain uncertain in the journal. The test-only barrier
makes queued-backend cancellation deterministic without a production callback.

The embedded async adapter clones DaemonControl under a brief handle lock and
moves Arc-owned immutable artifact/gate into a blocking worker. Synchronous
backend waits cannot block the executor's revocation task. Join failure is
conservatively uncertain; a poisoned backend mutex refuses before gate entry.
The generic daemon command and ordinary Bitcoin broadcast paths are unchanged.

`not_after` is a required caller-supplied monotonic Instant computed
conservatively from the accepted observations/preflight lifetime. Queue time
consumes that lifetime. Immediately before gate entry under the backend lock,
expiry closes a Pending gate as Expired and returns a definite zero-call refusal.
Expired gates cannot reset or be reused; transport supplies no default duration.
Pending alone is not a freshness claim: expiration is enforced on dispatch.
