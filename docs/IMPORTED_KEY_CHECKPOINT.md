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
that UUID before encryption. Callers must obtain this ID/UUID pair from the
current authenticated Cube record, and authenticate the source inventory before
constructing the Kit. This slice does not yet implement that export transaction.
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
supply an authoritative checkpoint response. Successful and 403 bodies are read with a 512 KiB cap.
Errors retain neither response bodies nor request URLs. The specific 403 code
`DEVICE_BOUND_SESSION_REQUIRED` yields `DeviceSignInRequired` with actionable
on-device sign-in text; it does not classify an otherwise valid session as dead.
401, other 403s, 404, 409, 423, and 429 remain distinct; Retry-After is preserved.
DELETE uses CAS and validates the resulting tombstone epoch. As with PUT, an
uncertain DELETE result must be reconciled by current status before another
mutation; the transport never retries it automatically.

## Remaining release boundaries

A separate reviewed transaction must authenticate export against the live local
inventory; atomically restore the exact wallet/operation and all local accounts;
persist the remote checkpoint binding in the device-authenticated inventory;
and recheck current authenticated status before every restored signer use.
Local and server transactions cannot be treated as one atomic transaction.
The recovery transaction must explicitly handle changes during admission and
uncertain local commits without exposing partially admitted signers. Existing
`admit_new_batch` and ordinary Send are untouched; this slice makes no claim that
their current callers enforce this checkpoint. Address-index reconciliation,
clean-device installer wiring, UI, and end-to-end restore QA remain outstanding.
