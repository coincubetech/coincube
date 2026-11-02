# Imported-key password Kit checkpoint (staged desktop integration)

This backend slice implements the checkpoint contract merged by
[API #362](https://github.com/coincubetech/coincube-api/pull/362) for
[desktop #914](https://github.com/coincubetech/coincube/issues/914).
It does not expose a Restore UI, admit local keys, or enable signing.

## Authenticated counters and ciphertext

The v3 password plaintext now requires two different nonzero counters:

- `inventory_generation`: the source device's authenticated local inventory
  generation. It may be 3 when the first server publication is generation 1.
- `checkpoint_generation`: the intended Connect publication epoch, calculated
  as checked `expectedGeneration + 1` before encryption.

Earlier **staged, unreleased** v3 plaintext without `checkpoint_generation`
fails decoding. There is no fallback to the inventory counter. Public v2 Kits
and the legacy Recovery Kit endpoints are unchanged. This is a staged v3 wire
contract change and requires recovery/security review before product wiring.

`PreparedPublication` owns one immutable ciphertext for one CAS attempt. The
caller supplies the known API Cube ID and expected Cube UUID; the Kit must match
that UUID before encryption. This low-level type does not authenticate source
inventory; live export uses the repository transaction described below.
Publication succeeds only when the response is active and its epoch and
lowercase SHA-256 commitment match the exact UTF-8 ciphertext bytes. PUT is sent
once with no legacy POST fallback. An uncertain network, gateway, or malformed
success response must be reconciled with current authenticated GET status using
the same prepared ciphertext. Re-encrypting produces different bytes and cannot
prove whether the earlier write succeeded. A conflict is never automatically
retried against a newer generation.

`fetch_verified_password_kit` requires an active, nonzero ciphertext response,
checks its exact byte commitment, decrypts with the expected Cube UUID, validates
all v3 wallet/key bindings, and compares the encrypted checkpoint epoch to the
response epoch. It then reads current status again after the expensive KDF.
Missing ciphertext, tombstones, mismatched epochs/commitments, malformed status,
and unavailable reads fail closed. Old ciphertext republished at a later epoch
is rejected even when the API correctly hashes those old bytes.

`VerifiedDownload` is non-Clone and secret-owning with redacted Debug. It exposes
no key extraction or admission method. Its recheck is a point-in-time server
observation, not an atomic admission/signing capability or a persistent grant.

## Transport behavior

All four methods use the existing authenticated Connect client and its stable
device identity headers through the existing ten-second anchor transport, which
refuses redirects. A redirected endpoint cannot forward the ciphertext body or
supply an authoritative checkpoint response. Successful, 403, and 423 bodies are
read with a 512 KiB cap.
Errors retain neither response bodies nor request URLs. The specific 403 code
`DEVICE_BOUND_SESSION_REQUIRED` yields `DeviceSignInRequired` with actionable
on-device sign-in text; it does not classify an otherwise valid session as dead.
401, other 403s, 404, 409, and 429 remain distinct; Retry-After is preserved.
A bounded 423 body with `TRUSTED_DEVICE_DELAY` retains its optional
`data.available_at` timestamp. Duress and malformed or oversized 423 bodies
remain the neutral `Locked` fallback; no raw server message is retained.
DELETE uses CAS and validates the resulting tombstone epoch. As with PUT, an
uncertain DELETE result must be reconciled by current status before another
mutation; the transport never retries it automatically.

## Remaining release boundaries

A separate reviewed transaction must atomically restore the exact wallet/operation
and all local accounts;
persist the remote checkpoint binding in the device-authenticated inventory;
and recheck current authenticated status before every restored signer use.
Local and server transactions cannot be treated as one atomic transaction.
The recovery transaction must explicitly handle changes during admission and
uncertain local commits without exposing partially admitted signers. `admit_new_batch` now refuses a bound inventory but still exposes no clean-device
checkpoint admission path. Ordinary Send is untouched; no claim is made that
its current callers enforce this checkpoint. Address-index reconciliation,
clean-device installer wiring, UI, and end-to-end restore QA remain outstanding.

## Live-inventory publication transaction (follow-up, still staged)

`LocalKeyRepository` can now prepare a password Kit from its actual live records,
not a caller-supplied secret inventory. Preparation reads the authenticated Cube
ID/UUID/network and checkpoint status through the bounded no-redirect transport.
It accepts no caller-supplied public blob: current unique Settings metadata and
the read-only daemon SQLite identity/high-water snapshot construct that blob.
Its full public fingerprint is rechecked after the KDF and before confirmation;
missing/stale metadata, config, database or address state refuses completion.
Under the stable operation lock it checks the live unlock/session/duress scope,
committed device anchor, exact manifest, and each device-bound ciphertext. The
v3 validator rederives every exported account against both descriptor branches.
The source inventory generation and digest are captured separately from the
predicted server publication epoch. Run these KDF-bearing calls off the UI thread.

An optional manifest journal stores the immutable ciphertext, its exact hash,
transaction ID, API Cube ID, and source inventory identity. Existing manifests
without the field retain identical canonical bytes. The journal is covered by
the existing manifest digest in the device-only monotonic anchor; it does not
change the OS anchor format or use an unauthenticated sidecar. New journal
manifests are bounded at 256 KiB (ciphertext at 128 KiB); old manifests retain
their 16 KiB bound.

The local transaction first commits **Prepared** through the existing
Prepared/Pending/Committed crash protocol, before sending any network mutation.
This metadata commit advances the local counter by one. Resume reopens and
verifies that journal, checks current API scope/status, and sends at most one
explicit CAS PUT using the stored bytes. A matching current status reconciles
an earlier uncertain upload without another PUT. It commits **Confirmed** only
after exact active epoch/hash proof and repeated local scope/currentness checks;
that second metadata commit advances the local counter again. Confirmation
removes the stored ciphertext but retains its hash, remote epoch, source digest
and original source generation. A retry after uncertain local confirmation can
reopen the already-confirmed receipt. Directory/manifest rollback cannot restore
an older journal against the external anchor.

Cancellation before local intent leaves no upload intent. Cancellation after
Prepared may leave a durable unresolved attempt, even if no request was sent.
An unchanged GET cannot prove that a delayed/uncertain PUT will never commit;
the API has no reservation/cancel token. The transaction therefore retains the
same bytes and does not claim remote cancellation. Conflicts, missing status,
active newer epochs, tombstones, and mismatched responses do not count as backup
completion. No status cache substitutes for authenticated current reads.

**Availability tradeoff:** Prepared and Confirmed inventories refuse admit,
replace, batch admission, key removal, and whole-operation deletion. Merely
clearing a local receipt while its old server Kit is active could restore a
removed signer on a clean device. The caller must first explicitly revoke the
server checkpoint; `release_checkpoint_after_revocation` performs no remote
mutation. It authenticates a strictly later inactive tombstone, checks the live
scope/inventory again, then clears the journal through the same crash-safe local
transaction. A later active epoch never releases keys. An ambiguous local
Pending transaction must be recovered before operation deletion; a missing or
corrupt nonempty manifest cannot prove absence of a remote binding and refuses
deletion. A provably empty anchored inventory still permits owner cleanup.

The server and local commits are not one atomic distributed transaction.
Receipts describe an authenticated observation, not perpetual server state.
Remote revocation/replacement coordination in a product flow, clean-device
admission, persisted signer-use enforcement, UI, and native Restore QA remain
unimplemented. These APIs expose no imported signing keys and do not change
ordinary Send or grant signer authority. Initial unbound inventories refuse to
overwrite any already-active remote checkpoint; explicit migration/recovery of
that state remains a separate transaction.

Initial cancellation now targets [API #364](https://github.com/coincubetech/coincube-api/pull/364)
(candidate `3f25ef25d7f883106ea928e63025e8fa048bd974`, not assumed merged).
Its first DELETE expected 0 atomically creates an inactive generation-1 empty
commitment tombstone. Status validation accepts that inactive shape but still
requires a real hash for every active checkpoint. An older API rejects this
fence; the desktop retains its unresolved journal and never fabricates success
or uploads stale data merely to create a revocation epoch.

`cancel_checkpoint` is an explicit remote mutation, independent of public-state
drift. It reconciles current status, conditionally revokes only the exact known
Kit (or its original inactive epoch), and records local release only after a
fresh later inactive proof. If a first PUT wins the race, cancellation can
revoke that exact ciphertext at the next epoch. If DELETE wins, the tombstone
fences the delayed PUT. The bounded driver never retries the same epoch within
one call and never chases/revokes a conflicting active Kit. A lost DELETE reply
is reconciled by GET; interruption retains the journal for a later exact retry.
The API #364 contract and desktop candidate require independent review and the
normal merge gates before product wiring or publication.

Address allocation can advance after Confirmed. Its receipt is point-in-time,
not proof of a current complete Restore later; Restore/signer admission remains
unwired until address-state drift triggers refresh or an equivalent current
state gate exists. No wallet metadata or allocation lease spans the network.

Deletion callers must order server checkpoint revocation, authenticated local
release, and only then local operation/server Cube deletion. Deleting the server
Cube first can make its owner/scope endpoints unavailable, so this backend will
refuse to erase a bound local authority without proof rather than assume the
server Kit disappeared. No deletion UI is wired or modified by this slice.
