# Dormant Claim intent journal

`services::claim_workflow` is a bookkeeping prerequisite, not an enabled Claim
wizard. No UI caller, signing, broadcast, foreign-seed import or existing-wallet
write is installed. `Step2Authorization` is an empty enum: saved phases, txids and
observation eligibility cannot construct a spend permission.

Initial admission requires `claim_spend::PoisonSelfTransfer`, the opaque owned
native-P2WSH builder artifact. It binds exact unsigned transaction bytes, source
and target ChainId, claimed prevouts, distinct Cube identities and a canonical
descriptor digest. Input-ancestry poison remains unsupported. The builder proves
construction ownership, not live UTXO availability, reserved-index freshness,
RDTS activation or signature/mempool validity. No timestamp or missing other-chain
transaction proves chain-exclusive funds.

The private versioned journal stores the ClaimPlan, unsigned transaction digest,
phase, signed step-one txid and last known inclusion. It stores a digest binding
the non-secret account/provider identities, not account tokens. It stores no seed,
private key, signed witnesses or cached spend authorization. Reopening requires
matching wallet and account/provider identities, starts unchecked, and discards
all observation state. A new process generation is allowed, but initial
construction must be revalidated against a freshly reconstructed opaque artifact
before recording any new broadcast intent.

The caller supplies a private, owned directory dedicated to this intent. This
module never creates a wallet directory or chooses a production path. Unix mode
0700-style directory privacy and owner-only regular files are required; symlink
and hard-linked files refuse. A stable `claim.lock` inode holds an OS exclusive
lock for the controller lifetime. A second cooperating owner refuses immediately.
Never unlink the lock to force access. Byte-for-byte read-before-write comparison
also refuses unexpected noncooperating journal changes; this is not protection
against a malicious process running as the same OS user.

Writes use a new owner-only temporary file, full file sync, atomic same-directory
rename and parent-directory sync. A failed/uncertain write poisons that journal
handle; reopen from disk rather than continuing from an old snapshot. Temp files
left by a crash carry intent metadata only and are never selected as recovery
state. Windows currently refuses with `UnsupportedPlatform`; equivalent durable
replacement/ACL validation must land before enabling this workflow there.

A runtime context binds account, exact provider identity and generation. The
caller must revoke both this controller and its observation source on logout,
account/provider/Cube changes or cancellation. Revocation is sticky until reopen.
Observation tickets are one-use and controller-specific. Late completions cannot
restore an old status. Each accepted result is reassessed against the controller's
own immutable plan, current positive policy, current time and coherent preflight
tips; the result's previous assessment is ignored. Stale, unavailable, reorg and
changed views replace eligibility. Prior inclusion is retained as evidence of a
reorg, not silently erased to permit progression.

`record_broadcast_intent` consumes fresh observations, rechecks them and durably
records `BroadcastUncertain` with the witness-bound native-segwit txid *before*
any future external broadcast attempt. It verifies unsigned-byte identity and
nonempty witnesses only; it does not verify signatures or grant permission to
broadcast. A future orchestrator still needs final-witness verification and
chain-bound mempool/preflight acceptance. It must not broadcast after this method
fails. No broadcast is performed by this module. An uncertain request is reconciled
by new observations of the same txid: absence/unconfirmed stays uncertain;
confirmed inclusion may become `Tracking`. Neither phase means completed Split.
Transaction replacement has no in-place API: a different transaction requires an
explicit new builder plan and separately owned intent directory; the old uncertain
transaction must still be reconciled before any future live workflow proceeds.

Synthetic temporary-directory fixtures cover public artifact admission, restart,
uncertain broadcast, reorg/stale/unavailable observations, account/provider and
controller-ticket isolation, replacement, conflicting writers and write failure.
Local checks compile fixtures only. Full GUI runtime tests must run in ephemeral
pinned CI; no existing wallet or host secret-store fixtures are authorized here.
Remaining launch gates include UI lifecycle integration, fresh-indexer acceptance,
signature/final-witness and mempool preflight, live endpoint readiness, reorg recovery
policy and end-to-end synthetic create/sign/confirm/restart validation. No production
flag or service configuration is changed. Rollback leaves this module dormant and
preserves journal evidence; never delete an uncertain broadcast record automatically.
