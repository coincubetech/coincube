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
state. On Windows, the journal requires a fixed local NTFS volume, pins each
directory component against replacement, and validates a protected owner-only
ACL on every journal file. Its replacement uses write-through semantics;
Windows crash, ACL, hard-link, and reparse-point tests remain part of platform
acceptance before enabling this workflow.

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


## Imported-wallet pairs

Claim from an imported Bitcoin wallet creates a dedicated Bitcoin Blake2b Cube,
just as native Vault Claim creates a dedicated pair. Reopening Claim resumes
that source's deterministic pair; it does not offer unrelated destination Cubes.
The original Bitcoin Cube and wallet are retained.

The paired imported wallet preserves the exact receive and change descriptors,
key origins, and external signer references. The Cube's unlock seed, PIN
protection and backup identity are preserved separately: its seed is never
substituted for an imported spending key. The fork Cube has no Spark wallet.
Wallet IDs, databases, history and address allocation are chain-specific. Initial
allocation/discovery bounds are copied, but subsequent scans run independently.

Pair creation uses the staged import journal. Retries reuse a completed pair
without rewriting its database, or resume the same pending installation using
its original allocation snapshot. Conflicting or duplicate records fail closed.
The encrypted imported Recovery Kits carry the import operation identity,
pairing and signer metadata. Restoring the source preserves the operation ID
bound by the target, so restoring both Kits can revalidate the original pair.
Older Kits without that optional identity remain readable; restoration never
invents a matching pair identity or replaces a conflicting existing operation.
An imported wallet backed up before these fields were included needs a one-time
Recovery Kit update to carry its stable import operation and external signer
metadata. Its old backup remains readable, but its stored fingerprint correctly
shows drift until each backup copy is updated. Fingerprints include the full
new payload; an old backup is never relabeled as containing the missing fields.
Restoring the fork wallet alone permits history viewing, while Claim requires
both source and target records to pass revalidation.

The shared Split engine binds the exact paired descriptors and a receive index
whose script has no transaction history on either chain. The destination must
be inside both authenticated scans and distinct from the three funding/return
addresses. History at another index, including a higher funding address, does
not make this unused destination stale.

Imported Claim records retain the minimum source range that must be scanned.
Reopening, funding checks and signing requalification also reload the source's
current complete receive/change range and verify its durable wallet identity,
descriptors and import bounds. Both chains must cover the saved minimum and the
current source range; a growing source cannot be checked using an older, shorter
snapshot. The destination's own unused history is checked again before
construction and signing requalification. Existing native target journal fields
and unrelated legacy Split attempts remain unchanged; older arbitrary-destination
attempts are not silently converted into paired claims.

Ordinary Send, PSBT update/finalization and broadcast remain unavailable for an
imported fork wallet until they have fork-specific protections. Claim continues
through the protected shared Split path. This pairing support does not enable
production signing or broadcast or bypass the existing capability/session gates.
