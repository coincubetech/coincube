# Vault-only BTCB2 app boundary

`App::new_for_chain` accepts the exact-chain cache/wallet/Cube settings, an
optional Cube encryption key derived directly during unlock, the current
Connect client, and an already admitted embedded daemon. It has no Breez or
Spark argument. Chain mismatch, absent authentication, a generic provider,
fallback endpoint or local node configuration is refused.

The fork app constructs no Liquid backend or Liquid panels, no Spark client,
and no marketplace panel. Its wallet registry has no Lightning route. The
sidebar receives ChainId and hides unsupported fork routes; direct events
cannot select those routes. Bitcoin constructors retain their existing clients.
Storage, rescan markers and device transport keys use the Cube's ChainId.
A watch-only Cube may lack a Cube encryption key; encrypted signing remains
unavailable until the necessary signer key exists.

The current recovery-alert heartbeat contract only accepts Bitcoin-family IDs,
so fork heartbeats are withheld. This is an explicit unsupported feature, not
a mainnet projection.

App session replacement/logout revokes the current authority, drops its retained
client and encryption key, and clears fiat data. Global Home authentication
changes must invoke the same invalidation through every open tab and pending
unlock/Loader. Reopening uses a new authenticated context. Generic backend
switches cannot reuse fork authority and instruct the user to reopen instead.
Persisted daemon configuration is sanitized; no bearer token belongs on disk.

Anchor freshness currently assumes synchronized API/device wall clocks and
rejects any future timestamp. A stale-evidence error instructs the operator to
check both clocks. There is no added skew allowance, timestamp clamping or
freshness extension. The status/anchor service's shared per-IP rate budget
remains authoritative; rate limits do not trigger unbounded retries.

This boundary alone does not activate the runtime gate. Authenticated unlock,
create/reopen workflow evidence, exact-head review and dedicated live indexer
acceptance are separate launch gates. Claim/Split, LAN pairing and unsupported
external signers remain gated.

The admitted client is installed into both Connect account and Cube consumers
before the initialization task runs. Its bootstrap loads user information from
that exact endpoint/token and never restores a saved keyring session. Late user
responses are generation-checked. Both generic App constructors return a typed
refusal for fork Cube settings; a seed-only fork App cannot be constructed.

A failed initial user bootstrap revokes authority immediately, even before the
account reaches its dashboard. Explicit transient retries retain the admitted
endpoint/token. Revocation also clears account/Cube consumers, not only the
backend's authority. The admitted in-memory session never owns a separate saved
keyring session, so its refusal does not delete unrelated saved credentials.
Unit-test account secrets use thread-local memory; direct legacy test keyring
consumers use a separate unit-test namespace.
