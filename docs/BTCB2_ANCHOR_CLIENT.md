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

`observed_at` on a returned `NetworkAnchor` is Unix seconds on this desktop's
clock. When the envelope includes `served_at`, `local_observed_at` shifts the
wire stamp by the skew between that server time and `sent_at` (this desktop's
clock when the request was sent), using saturating subtraction:

```text
sent_at - (served_at - observed_at)
```

Server-side age and the round trip after send both still count toward the
90-second maximum. The shift does not extend freshness or add a positive skew
allowance. A negative wire `observed_at` is an invalid response and is refused
before conversion. An envelope without `served_at` (an older API) keeps the
server stamp, so that path still requires synchronized clocks. `trusted` and
`fresh_anchor` then reject a translated stamp in the future or older than
`MAX_ANCHOR_AGE` (90 seconds). Height, median time past, and the translated
stamp are checked-converted into the trusted anchor. A value that does not fit
the destination integer is `Unavailable` or `Stale`, not clamped into range.
Do not rewrite stamps locally to clear a stale admission. The stale message
still tells the operator to check the device and Connect clocks, then retry.

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

Startup retains typed `AnchorStartupError` values for service states, HTTP
classification, malformed responses, transport failures and daemon admission.
The UI maps those variants to fixed recovery instructions: sign in for missing
or expired authentication, configure an unconfigured backend, wait for indexing
or synchronization, and refuse wrong-chain evidence. No raw server error body
is displayed. Indexer lag never selects a lower anchor or a Bitcoin fallback.

Dead-poller detection revokes the refresh session before cleanup. Fork cleanup
uses the daemon's nonpanicking shutdown helper and logs failures; the existing
Bitcoin shutdown path is unchanged. Cleanup error tests belong to the daemon
helper; GUI coverage verifies revocation even when the handle is already gone.
