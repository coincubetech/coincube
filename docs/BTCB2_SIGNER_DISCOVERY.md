# Chain-preserving Vault seed discovery

The local-daemon loader and remote-backend application constructor pass the
Cube's ChainId to hot-signer discovery. Password and no-password scans select
only that chain's mnemonic folder. Bitcoin Network callers remain compatible.
Encrypted but locked/unopenable keys retain their existing distinct states.
The core filtered-reader keeps legacy plaintext handling and Vault filtering.

Unlock fingerprint backfill and Connect encryption-public-key fallback also
select the Cube's chain folder; their tab callbacks persist backfilled settings
into the same chain directory. Encryption key derivation still uses the
chain's Bitcoin key-encoding network, never as a storage selector.

This PR stacks on #414 (explicit seed storage APIs). #411's loader/Tor changes
are a separate prerequisite with non-overlapping hunks; merge both. Pricing
#413's Cache constructor fields are also independent and must be retained.

Runtime gates remain closed. Normal GUI OpenCube checks the folder identity
and RuntimeSupport before constructing the PIN/passkey entry state; Loader
also refuses dormant chains. SeedSource::resolve remains Network-based and
has only Liquid/Spark SDK callers. Those unsupported BTCB2 wallet features must
stay hidden when Vault runtime is eventually enabled. This work does not
claim their SDK paths are chain-aware.

Synthetic regression tests put the same seed in Bitcoin and fork folders
under different PINs, and prove the fork scan cannot use the Bitcoin copy.
They cover missing fork storage, locked keys, failed decrypt versus no seed,
correct decrypt, legacy filtering, and Connect metadata backfill. No existing
wallet files or production services are touched.

The existing unlocked-signer cache has no ChainId. Fork Vault lookup and
Connect encryption-key fallback bypass that cache and authenticate the fork's
own seed file; Bitcoin keeps the cached fast path. Fork encryption-key fallback
uses the keystore-aware reader for v3 seeds. Synthetic tests preload a Bitcoin
session with the same Cube ID/fingerprint and verify a missing fork file still
refuses; the Bitcoin control still returns its cached signer. A fully
chain-bound session cache is required before any seedless fork signing path
can depend on cache reuse.
