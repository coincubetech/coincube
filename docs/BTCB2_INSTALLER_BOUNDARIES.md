# BTCB2 installer boundaries (dormant)

This slice prepares installer state and backend selection. It does not enable
BTCB2 Cube creation or opening. `Installer::try_new_for_chain` refuses dormant
chains before signer generation, and the persistence boundary also refuses them.
The existing Bitcoin constructor and behavior remain available.

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

Remaining activation work includes authenticated feature/status gating,
chain-aware creation/restore entry and Connect registration, seed persistence
review, Taproot/Keychain restrictions, pricing consumption, and Claim/Split
safety machinery. The explicit creation entry includes a separate refusal for
BTCB2 so changing the global runtime-support enum alone cannot expose that
unfinished pipeline. The backend configuration here is inert preparation for
that later implementation, not live acceptance evidence.

Validation uses synthetic tokens and temporary paths only. Live RPC/indexer
connectivity, stable indexing, tailnet endpoint approval and full synthetic
end-to-end acceptance remain separate gates. Robert owns merges, deployments
and feature flags; no services or existing wallets were modified.
