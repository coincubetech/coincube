# Local MCP architecture

## Assessment (2026-10-21)

Tenshu is a Rust Cargo workspace with an Iced 0.14 desktop, not a web frontend.
`coincube-gui/src/gui/tab.rs` owns unlocked `App` instances and lock transitions.
`app/mod.rs` owns the active Cube, daemon, wallet, settings panels and shutdown.
`daemon/mod.rs::Daemon` is the existing async wallet interface. The embedded
implementation calls `coincubed::DaemonControl`; external implementations use
local JSON-RPC. The existing daemon socket includes mutating commands and is
**not** an MCP authorization boundary. Never proxy arbitrary daemon RPC methods.

The daemon stores Vault coins, transactions and unsigned spends in SQLite under
the network/wallet datadir. `list_coins`, `get_info` and `list_history_txs` supply
the current UI. `Wallet.main_descriptor.policy()` exposes deterministic primary
and relative-block recovery paths through `coincube-core::descriptors`. Spend
creation/coin selection live in `coincube-core::spend` and daemon commands; the
GUI coordinates PSBT review and existing Connect/phone signing. These operations
are deliberately absent from MCP. PIN/passkey unlocking lives in `services/unlock`
and `services/passkey`; `app/session` caches unlocked material. MCP never reads
these secrets. Liquid and Spark have distinct SDK-backed interfaces through
`app/wallets`; they are not represented as Vaults by this implementation.

Tests use Rust unit/integration tests, injectable daemon backends, and pytest
regtest tests (`tests/`). GUI build needs protoc and the existing SDK dependency
patches. Make builds the separate Spark bridge; packaging must also include the
new adapter executable.

## Integration

- `coincube-mcp`: small shared typed read protocol, revocable session gate and
  Unix socket host; official Rust `rmcp` SDK adapter behind the `adapter` feature.
- `coincube-gui/src/app/local_mcp.rs`: trusted settings, active-Vault service
  implementation, minimized projections from existing wallet APIs.
- `tenshu-mcp`: STDIO binary launched by the AI host; one fixed typed request per
  authenticated local IPC connection, no wallet engine, database or AI SDK.
- Cube Settings → AI access: opt-in, wallet scope and capability selection,
  expiring access credential, configuration and immediate revocation.

MVP supports macOS/Linux/Windows and the embedded Bitcoin Vault daemon only. External
daemon clients log raw RPC responses at debug level and are excluded; imported
non-Vault policies, Liquid, Spark and Bitcoin Blake2b are also excluded. Unix sockets live in a randomly created mode-0700
session directory; no TCP/HTTP listener exists. A 256-bit random bearer secret,
separate from all wallet/Connect credentials, authenticates each request. Client
names are display labels, never authentication. The UI issues one client grant
per unlocked Cube; replacing it revokes the previous grant. Grants expire after
one hour and are never persisted. Unlock/restart requires fresh consent. Other
Cubes require separate grants. Windows uses a protected named pipe with logon-session ACLs and server-PID
verification (see the extension below); there is no TCP fallback.

## Trust and threat model

AI hosts, models, tool arguments and metadata are untrusted. Wallet data may leave
the device through the host even though this server is local. Consent explicitly
covers one Vault and selected read capabilities. Credentials belong in private
host configuration, never prompts or tool responses. Theft of a credential permits
its scoped reads until expiration/revocation. This does not defend against malware
running as the user's OS account that can inspect application memory or control
its UI; filesystem permissions alone are not client authentication.

A non-secret session generation prevents a grant from reviving when the same Cube
is reopened. Synchronous daemon/SQLite reads run in a bounded blocking pool; the
worker retains its permit even if its async request times out.
The desktop gate validates credential, expiry, wallet and capability immediately
before service access and again before releasing results. Revocation closes the
listener and cancels queued operations. Lock/close/backend change revokes the
grant synchronously. A request already delivered to a client cannot be recalled.
Requests/responses and concurrency are bounded and reads have deadlines. Generic
errors contain no daemon messages, descriptors, filesystem paths or wallet
existence hints. Explicit response DTOs omit seeds, keys, xpubs, descriptors,
addresses, scripts, labels and PSBTs. Unknown commands cannot reach wallet APIs.
User-controlled text never controls routing or authorization.

## Tools and semantics

- `tenshu_list_wallets`: one authorized active Vault, opaque session wallet ID.
- `tenshu_get_wallet_summary`: current daemon balances in satoshis and sync state;
  no promise that a policy is presently satisfiable or that data is fully synced.
- `tenshu_get_transactions`: bounded confirmed history, newest-first; no labels,
  scripts or addresses. Limits avoid inventing unsupported cursor semantics.
- `tenshu_get_wallet_policy_summary`: script type, deterministic signing thresholds
  and key counts, recovery delays in blocks. No prediction of recovery eligibility.

Signing-session status is deferred: Connect/LAN signing state has multiple owners
and no single read-only authoritative session API. No fake status tool is shipped.
Future unsigned proposals must have separate capabilities and trusted UI approval;
this read protocol cannot call signing, broadcast, unlock, import or recovery.

## References and startup evidence

Repository: verified `git@github.com:coincubetech/coincube.git`, base `8495c3c0`.
Instructions read: `/Users/macstudio/git/coincubetech/AGENTS.md`, brain
`MEMORY.md`, `USER.md`, `AGENTS.md`, `PROJECTS.md`, desktop `PROJECT.md` and
`ARCHITECTURE.md`; repository `CLAUDE.md`, `CONTRIBUTING.md`. No repository
`AGENTS.md` or nested instruction files were present in the tracked tree.
Shared Agent Ops origin verified and main fetched; committed policy pinned at
`b4e4fa3c3ecedad93f40d08f71baca7cea707a25` (AGENTS, README, company profile,
task contract, routing, delivery and review/repair workflow read).

Official sources checked: [Rust SDK](https://github.com/modelcontextprotocol/rust-sdk),
[MCP specification](https://modelcontextprotocol.io/specification/2026-07-28),
[security guidance](https://modelcontextprotocol.io/docs/draft/tutorials/security/security_best_practices).
Use the SDK's actual supported protocol negotiation; do not claim support for a
revision merely because it appears in a planning reference. Rust avoids adding
a Node runtime; TypeScript SDK v2 is not applicable to this codebase.

## Windows transport extension (Phase 2)

The Windows adapter retains the same grant checks and wire protocol. A random
local `\\.\pipe\tenshu-mcp-<desktop-pid>-<random>` endpoint is created with
first-instance protection and remote-client rejection. Its protected DACL allows
only the desktop process's logon SID, excluding other users and logon sessions;
no default permissive pipe ACL is used. Missing logon identity fails closed.
Before sending any credential, the adapter validates the local endpoint format
and verifies its server process ID with the OS. Client SQOS permits identification
only, preventing server-side impersonation of the adapter. These controls do not
claim protection from arbitrary malware already controlling the same OS session.

Shared input limits, deadlines, bounded workers, grant expiry and session
revocation apply unchanged. Windows CI must execute named-pipe and STDIO tests;
cross-compilation alone is insufficient. The desktop and MSI install the `.exe`
companion as a sibling. No TCP fallback or remotely addressable endpoint is added.

References: [Microsoft pipe security](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights),
[Tokio named pipes](https://docs.rs/tokio/latest/tokio/net/windows/named_pipe/index.html).
