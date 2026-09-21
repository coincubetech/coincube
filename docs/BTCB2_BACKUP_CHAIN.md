# BTCB2 backup chain isolation

Backup and Recovery Kit selection uses the authoritative Cube ChainId retained in
Cache::fiat_chain and exposed by Cache::chain(). The legacy Network field describes
address encoding only. Settings records must also match the selected chain.

BTCB2 mnemonic access bypasses the legacy process session, whose key contains only
Cube ID and fingerprint. It opens and authenticates the seed in the requested
chain directory via the keystore-aware unlock service. Bitcoin retains its
PIN-verified session path. Recovery JSON carries the BTCB2 API network string.
Owner-self xpub envelope validation projects to Bitcoin Network only at the
existing key-encoding boundary, never to select storage or authenticate a seed.

Dependencies: signer discovery #416 and authoritative Cache constructors #413.
Runtime remains dormant. This does not enable Claim/Split, LAN signing, or SDKs.
Synthetic twin-Cube fixtures and same-ID/fingerprint cached-session fixtures
exercise directory selection and refusal of Bitcoin fallback. Required pinned
workspace tests and strict lint must pass before acceptance; no live wallet,
service, or server acceptance is claimed.
