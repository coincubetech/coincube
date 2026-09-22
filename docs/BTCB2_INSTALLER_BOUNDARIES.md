# BTCB2 installer boundaries

The explicit authenticated Connect route permits a BTCB2 Vault only with a current
account flag and exact-chain backend admission. Generic runtime support remains
dormant; Bitcoin retains its existing constructor and behavior. Missing, false,
or unreadable account availability refuses before fork filesystem writes.

`Context::new_for_chain` preserves chain identity separately from address
encoding. Directory/config output and failed-install cleanup use that identity.
Connect-only selection keeps the chain in its authenticated Esplora URL, clears
pending local node state and configures no Bitcoin fallback. Both BTCB2 variants
require a token; testnet4 is never mapped to mainnet. The backend step does not
load the Bitcoin family's global node configuration for a BTCB2 context.

Generic external Esplora cannot distinguish BTCB2 from Bitcoin using genesis:
the two chains share it. This path refuses BTCB2 before any network request or
configuration mutation. Electrum and the generic Bitcoin node choices are
removed from the BTCB2 node-selection state. The optional managed-node route
remains refused pending complete loader/Tor/directory isolation.

Home adds fork networks only for an authenticated enabled account. Creation is
native P2WSH, Vault-only, with a fresh PIN-protected seed and mandatory offline
backup. Restore, passkey, hardware/LAN and Claim/Split entry points remain closed.
Testnet4 has its own identity and endpoint and never falls back to mainnet.

Validation uses synthetic tokens and temporary paths only. Live RPC/indexer
connectivity, stable indexing, tailnet endpoint approval and full synthetic
end-to-end acceptance remain separate gates. Robert owns merges, deployments
and feature flags; no services or existing wallets were modified.
