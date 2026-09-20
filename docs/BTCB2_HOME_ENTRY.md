# Home chain identity and BTCB2 creation

Home selection and its installer event carry `ChainId`, including isolated
storage and Connect network strings. Bitcoin's network projection is used only
where a signer needs address/key encoding. The installer receives the selected
identity through `try_new_for_chain` and rejects dormant chains before Home
initializes the data directory.

The current runtime gate remains closed for both BTCB2 identities; neither is
added to the launcher. A typed dormant Home shows an unavailable explanation and
refuses creation, import, recovery, passkey and stale callback events before
seed, keystore or filesystem side effects. Direct selection cannot bypass the
same gate. Once separately approved runtime admission exists, BTCB2's Create
Cube action leads only to the authenticated Connect Vault installer. It does
not run the seed-only Liquid/Spark creation flow. Seed/passkey recovery helpers
also refuse BTCB2 independently of runtime activation.

This change does not activate Claim/Split, Keychain LAN, testnet4 infrastructure,
or any server service. Launch still requires a canonical post-fork trust anchor
with provenance, chain-bound provider admission, and live endpoint acceptance.
A shared genesis or server-reported status height cannot substitute for that
trust contract. No expiry is hard-coded and no live-wallet verification is used.

Developer verification: run the full `coincube-gui` package tests and strict
workspace clippy with the repository-pinned toolchain and a dedicated target
directory. Home gate tests use synthetic temporary directories only. Live
server checks remain pending the infrastructure handoff; do not use Bitcoin's
port 3000 endpoint for BTCB2. Rollback is a code revert before release; no data
migration, production flag change or environment update is part of this PR.
