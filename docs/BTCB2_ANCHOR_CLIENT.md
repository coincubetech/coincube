# Authenticated Connect anchor client

`CoincubeClient::network_anchor(chain)` calls the authenticated chain-specific
`/api/v1/connect/networks/{network}/anchor` endpoint. Its typed result preserves
all unavailable states. A 200 requires the exact requested network, a complete
hash/height/MTP/observation, an active fork at or below that height, and an RDTS
flagday object. RDTS may be inactive; the anchor does not authorize Claim/Split
or interpret expiry. Malformed or contradictory data never becomes available.

Connect attests that it read a post-fork `header_version` 2 header, matching the
RPC tip and deployment snapshot with the same best hash before and after.
This is operator-trusted authenticated evidence, not SPV verification. The
separate daemon admission code must compare this anchor's hash at its height
against the selected indexer before using that provider.

`authenticated_backend` accepts only the dedicated endpoint derived from the
same authenticated client's API base and exact ChainId. Bitcoin endpoints,
missing authentication and implicit fallback are refused. Successful evidence
is cached in memory with a 30-second refresh and daemon-enforced maximum age of
90 seconds. Refresh failure clears the snapshot. Revocation prevents a late
response restoring it, and dropping the authority aborts its refresh task.
The client rejects future timestamps and checked-converts daemon integer fields.

`EmbeddedDaemon::start_authenticated` consumes that context through the explicit
daemon startup entry. Its retained `config()` is sanitized for persistence;
the JWT is not serialized. Stop revokes authority first. An abandoned async
startup owns a cleanup guard for its daemon handle. Existing Bitcoin startup
is unchanged. `Daemon::invalidate_connect_session` lets the GUI revoke evidence
before logout/account/provider changes; a new context and startup are required.

This slice does not flip the GUI runtime gate. Authenticated unlock/Loader
handoff, App Vault-only initialization and installer validation are coordinated
follow-on changes. Live server acceptance remains pending dedicated BTCB2
indexer readiness, tailnet access and RPC infrastructure handoff. No real funds,
wallets or services were used to validate this client.
