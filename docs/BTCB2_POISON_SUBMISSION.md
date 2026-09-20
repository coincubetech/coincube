# Dormant exact-byte poison submission

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

This is a transport, NOT Claim authorization. Before calling, future orchestration
must bind current account/Cube/provider context, reserved owned destination,
fresh RDTS/expiry/MTP and UTXO observations, exact-transaction preflight, explicit
user confirmation and durable uncertain-intent journaling. Signature integrity
does not prove maturity, current unspentness, replay exclusivity or permission.
Six-confirmation/reorg checks remain separate before step2. No production flags
are enabled; no live broadcasts are part of this change.

Synthetic daemon tests construct/sign/finalize realistic native P2WSH fixtures,
record exact submitted bytes, refuse wrong chains/descriptors before calls and
retain uncertainty without retry on transport failure. Full daemon tests and
strict clippy validate the daemon; GUI adapter compilation/pinned CI is separate,
and no local GUI runtime/keyring tests are authorized.
