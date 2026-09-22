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
A scan/read validates before and after obtaining results. A refreshed higher tip
is allowed only if the old anchor hash remains at its old height and the new
trusted anchor hash matches too. Rollback or a changed old hash discards results
before BDK applies them. Only the refreshed after-observation must still be fresh,
so a scan longer than 90 seconds can complete. Broadcast validates before send;
a successful send is not retroactively reported as failed by a later check.

Admission is ordered before directory creation, SQLite setup/migration, and node
writes. Generic configuration-only or injected-interface startup cannot admit a
fork. Only `start_with_connect` admits a fork, and only for native P2WSH,
embedded operation without a JSON-RPC server or pending managed node. Its
explicit authority and exact provider selection must pass admission before any
write. GUI launcher exposure remains separately gated pending combined GUI/API
acceptance. No live-server acceptance is claimed. This is trusted-service chain identification, not malicious-upstream SPV
verification, and it does not prove funds are chain-exclusive or safe to claim.

A provider 404 at the trusted anchor is `IndexerBehind`: admission still refuses,
and the GUI owns a bounded, caller-visible retry. A running provider receives a
30-second cooldown on that response; no lower or unverified anchor is substituted.
HTTP402/429 admission probes enter the existing rate-limit cooldown, and abort is
checked before/after each added probe and between operation phases. Fork tip hash
and status are grouped into one guarded operation to avoid duplicate probe pairs.

`StartupError::ConnectAdmission` retains typed unavailable/lag/hash/stale errors.
The runtime bearer token is authoritative: a stale token in an input config is
ignored and never used to construct the fork provider. The GUI adapter separately
retains sanitized config for backend-switch persistence. `median_time_past` is
informational metadata here, not Claim or expiry authorization.

The no-write admission tests exercise the public authenticated entry for stale
and wrong-chain authorities, and reject RPC exposure, pending managed nodes,
Taproot, Bitcoin identity and fallback providers before I/O. The private startup
body additionally tests missing authority. A synthetic loopback Esplora test
creates and reopens a new BTCB2 SQLite wallet through the public authenticated
entry and verifies its stored chain; it does not use a live node or existing
wallet. Generic `start`/`start_default` retain their fork refusal. Manually configuring a
Bitcoin backend with a fork endpoint remains a pre-existing provider-trust
limitation. Operator acceptance must verify separate correct endpoints; generated
Bitcoin and BTCB2 routes remain distinct. This does not introduce a new Bitcoin
identity architecture or an additional code-activation gate.

`DaemonHandle::stop_for_cleanup` provides nonpanicking cleanup for abandoned fork
startup and GUI drop paths. It sets abort/shutdown flags, attempts worker joins
even after a closed control channel, and returns sanitized errors. The existing
Bitcoin `stop` behavior is unchanged.
