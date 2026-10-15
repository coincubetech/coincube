# Claim and Split intent journals

`services::claim_workflow` records Claim and foreign Split intents. The current
desktop shows two-deposit preparation and read-only checks, but the ordinary
ancestry pair is not yet admitted to production signing or broadcast. The
90-byte OP_RETURN path is retired for new transactions. Existing legacy
records remain readable for reconciliation. `Step2Authorization` is an empty
enum: saved phases, txids and observation eligibility cannot construct a
spend permission.

Version-10 Claim and version-11 Split intents bind two ordinary unsigned
transactions: all shared pre-fork inputs plus a verified Bitcoin-only funding
input on Bitcoin, and the same shared inputs plus a verified BTCB2-only funding
input on BTCB2. Their descriptors, reserved indices, destinations, fee rates,
retained ancestry paths and account/provider identities are recorded and
checked on reopen. Core construction proves wallet ownership and transaction
shape; the live two-chain collector must separately prove both funding
dependencies, canonical shared inputs and current spendability. A timestamp or
an absent transaction on the other chain is never such proof.

The private versioned journal stores no seed, private key, account token or
cached spend authority. A version-10/11 record may atomically retain both
finalized signed witnesses and their exact txid/wtxid before any possible
network attempt. Each possible attempt is separately persisted before I/O and
cannot be cleared or repeated after restart. Reopening requires matching
wallet and account/provider identities, starts unchecked, and discards live
ancestry, preflight and approval state. The exact witnesses are recovery data,
not permission to send. The older single-step journals retain their own
legacy reconstruction and reconciliation rules.

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

For legacy records, `record_broadcast_intent` consumes fresh observations, rechecks them and durably
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

Paired possible-attempt recording additionally requires a short-lived,
controller-bound ancestry result for the exact two signed witnesses and
matching node-policy tips. After Bitcoin confirms, a different collector
checks the exact canonical six-deep Bitcoin witness and that the BTCB2 inputs
remain unspent. Its result must agree with the independently recovered Bitcoin
confirmation block. Neither result is restored from the journal. The paired
Claim and Split coordinators now join wallet reservations, fresh review,
explicit approval and one-use transport gates. The production gate stays
closed until the end-to-end and hardware acceptance in
`BTCB2_ANCESTRY_SPLIT_MIGRATION.md` is complete.

Synthetic temporary-directory fixtures cover public artifact admission, restart,
uncertain broadcast, reorg/stale/unavailable observations, account/provider and
controller-ticket isolation, replacement, conflicting writers and write failure.
Local unit tests exercise the journal and recovery readers; pinned two-chain
regtest covers the ordinary signed transactions. The GUI now has paired
signing, submission review and recovery routes behind the closed production
gate. It still needs full paired GUI create/sign/confirm/restart acceptance,
real hardware-signer smoke tests and independent review. No production signer
gate or service configuration has been opened. Never delete an uncertain
broadcast record automatically.
