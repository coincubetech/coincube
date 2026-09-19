# BTCB2 descriptor source gates

The descriptor picker carries explicit ChainId. BTCB2 creation exposes local
Cube keys and imported/pasted xpubs only. Hardware, Keychain, provider-token
and Border Wallet entry points stay hidden until their complete chain-bound
workflow is verified. A manually supplied xpub is watch-only here; accepting
its encoding makes no claim about signature capability or replay protection.

Equivalent navigation/fetch/token/result messages are refused before work,
including stale hardware key results. Hardware discovery subscriptions are
not started for a fork picker. Existing-key selection uses the same capability
rule. Descriptor apply independently refuses unsupported sources, so a stale
editor state cannot bypass the picker. The global runtime gate is unchanged.

This conservative creation policy is distinct from KeySource::replay_capable:
a locally derived Border Wallet key may produce unified signatures, but its
complete creation/runtime flow is not enabled by that fact alone. Claim/Split,
Keychain LAN and unsupported signer transports remain separate launch gates.

Stacks on #421 and its installer/seed dependencies. Synthetic tests cover
external-source messages and descriptor apply; Bitcoin picker behavior stays
unchanged. Full GUI/pinned strict CI and independent review are required before
acceptance. No live devices, funds or existing-wallet changes are used.
