# Authenticated Connect admission

The daemon's `start_with_connect` entry takes an ephemeral `ConnectBackend`:
exact ChainId, one selected endpoint, current bearer token, and a
`ConnectAnchorAuthority`. It is deliberately not serializable. Fork installer
configuration is written through `Config::for_persistence`, which strips tokens;
Bitcoin configuration persistence is unchanged.

The authority implements the authenticated Connect anchor contract: a successful
response attests a consistent post-fork version-2 RPC header, bound to the exact
network. The daemon checks the authority's ChainId and observation age (maximum
90 seconds, future observations refused) and compares the concrete Esplora
provider's hash at that height. Provider lag, unavailable authority, stale data,
and mismatches refuse. The endpoint's spelling and shared genesis prove nothing.

The context is immutable and contains exactly one provider. Any endpoint or
credential change creates a new context. GUI adapters can refresh a successful
anchor every 30 seconds and clear it immediately on failure. BDK operations
consult the authority only at operation boundaries, not per transaction fetched.
A scan/read validates before and after obtaining results; changing anchors cause
results to be discarded before BDK applies them. Broadcast validates before send;
a successful send is not retroactively reported as failed by a later check.

Admission is ordered before directory creation, SQLite setup/migration, and node
writes. Generic configuration-only or injected-interface startup cannot admit a
fork without the explicit authority. The production `ChainDormant` gate remains
in place pending combined GUI/API acceptance. No live-server acceptance is
claimed. This is trusted-service chain identification, not malicious-upstream SPV
verification, and it does not prove funds are chain-exclusive or safe to claim.
