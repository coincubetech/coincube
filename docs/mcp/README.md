# Tenshu local AI access (MCP)

This first slice supports **Bitcoin Vaults using Tenshu's embedded daemon on
macOS, Linux and Windows**. It reads the existing wallet engine. It does not support
Liquid, Spark, imported non-Vault policies, Bitcoin Blake2b or external daemons
yet. MCP starts disabled and every new unlocked Cube session
requires explicit approval. No Connect API or Keychain changes are needed.

## Feature testing on `mcp`

Local MCP access is experimental development work on the `mcp` feature branch.
Feature PRs target `mcp`, where the Unix implementation and Windows follow-on can
be integrated and tested together. PR #739 depends on the foundation in #736.
Build from the feature checkout containing the changes being tested; the commands
below do not imply that MCP is included in the production release.

Review and CI completion make a PR eligible for human review. Merging feature
PRs into `mcp`, promoting the feature to `master`, and publishing a release are
separate owner decisions. Packaged-client interoperability and signed-artifact
checks remain release gates.

## Build and install

Use the repository's pinned Rust toolchain and normal desktop prerequisites
(`CLAUDE.md`, `CONTRIBUTING.md`). The official Rust MCP SDK is pinned to `rmcp
3.6.0`; Cargo.lock pins its dependencies. No Node/Python runtime or AI API key is
needed by the MCP executable.

```sh
make build               # Spark bridge, desktop and tenshu-mcp
make run                 # builds bridge + adapter, runs desktop
# Adapter alone, for development:
cargo build -p coincube-mcp --features adapter --locked
cargo test -p coincube-mcp --features adapter --locked
```

Debug binaries are `target/debug/coincube` and `target/debug/tenshu-mcp`.
`make release` uses `target/release/`; production workflows use `target/<triple>/minimal/`.
The Linux tarball includes `tenshu-mcp` beside `coincube`. The macOS bundle includes
`Tenshu.app/Contents/MacOS/tenshu-mcp` beside `Coincube`. The release/nightly workflows
build and stage it before signing. Packaging changes still need validation by CI
and a signed-release smoke test. Both macOS release architectures use native
runners and execute `contrib/check_mcp_binary.py` on the signed adapter before
publishing; the check covers initialization, discovery, authenticated fixture
reads and clean shutdown. Do not copy a development executable into an
already signed application bundle.

## Enable and approve a client

1. Open and unlock the desired Cube normally. Go to **Cube → Settings → AI access**.
2. Enable MCP for this session. Read the financial-data disclosure.
3. Enter a client label, explicitly select this Cube's Vault, and select each read
   capability you want to grant. The label is for your reference, not an authenticated
   application identity.
4. Choose **Approve client for one hour**. No client is authorized merely by enabling
   the toggle. The grant is a random bearer credential issued by Tenshu.
5. Choose **Copy private client configuration** and paste it into your trusted MCP
   host's local configuration. It contains a credential: never paste it into a chat,
   issue, shared document or repository. Clear your clipboard after setup.
6. Restart/reconnect the MCP host. Discover tools, then call `tenshu_list_wallets`
   and use the returned opaque `wallet_id` for subsequent calls.

The copied command is derived from the running desktop executable's directory.
The adapter must be installed beside it. The GUI shows the authorized client and
successful-read count; this is not a verified host identity or a persistent-connection
indicator (IPC connections are per request).

One grant per unlocked Cube is supported. Give separate Cubes separate host entries.
Do not share a grant among clients: anyone holding it can use its exact scope.
Approving again or changing any scope/client setting revokes the old grant.
**Revoke access now**, disabling MCP, locking/closing the Cube, changing the wallet
or backend, or exiting Tenshu ends access. Grants expire after one hour; enablement
and credentials are never persisted. After unlocking/reapproval, replace the host's
old configuration and reconnect. Revocation cannot erase data already sent to a host.

## STDIO client configuration

The GUI produces this shape, with actual paths and a fresh credential:

```json
{
  "mcpServers": {
    "tenshu": {
      "command": "/Applications/Tenshu.app/Contents/MacOS/tenshu-mcp",
      "env": {
        "TENSHU_MCP_SOCKET": "/tmp/tenshu-mcp-SESSION/rpc",
        "TENSHU_MCP_CREDENTIAL": "REPLACE_WITH_THE_PRIVATE_GRANT_FROM_TENSHU"
      }
    }
  }
}
```

On Windows the copied command ends in `tenshu-mcp.exe`; the MSI installs it beside
`coincube.exe`. `TENSHU_MCP_SOCKET` is a local pipe name such as
`\\.\pipe\tenshu-mcp-PID-RANDOM`, not a TCP address. Keep the generated name unchanged:
the adapter validates its format and checks the pipe server's OS process ID
before sending the credential. The pipe DACL permits only the current Windows
logon session and rejects remote clients. The bearer grant is still required.
The client grants identification-only SQOS, not impersonation rights.

Use the copied values, not these placeholders. On Linux/development builds the
command is the absolute installed/build path to `tenshu-mcp`. There are no tool
arguments for paths, credentials, shell commands or daemon RPC methods.

- **Maple AI Desktop:** preferred target, but its current custom-STDIO configuration
  UI has not been verified in this implementation. If the installed version supports
  local MCP servers, supply the command and two environment variables above in its
  local server settings. Do not confuse [Maple AI](https://maple.ai/docs) with the
  unrelated maple.dev telemetry service. No Maple inference/proxy integration is
  needed or included. Manual interoperability remains a release gate.
- **Claude Desktop:** use its local MCP developer configuration (`mcpServers` shape
  above). Local servers are distinct from cloud connectors. See [Claude's local
  MCP guide](https://support.claude.com/en/articles/10949351-getting-started-with-local-mcp-servers-on-claude-desktop)
  and [local versus remote connectors](https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp).
  A packaged `.mcpb` extension and marketplace distribution are future work.
- **ChatGPT:** this slice does not provide a direct ChatGPT connection. As checked
  2026-10-21, [OpenAI's custom MCP guide](https://help.openai.com/en/articles/12584461-connecting-custom-mcp-servers-in-chatgpt)
  describes remote connections, not direct local STDIO. No HTTP listener, tunnel,
  public relay or wallet-service deployment is added to work around this limitation.
- Other local STDIO MCP hosts can launch the same executable. The SDK handles
  negotiation, including legacy initialization and the 2026-07-28 protocol.

## Tools

All outputs are structured JSON with advertised schemas. Amounts are satoshis.
Tool discovery and `tenshu_get_capabilities` are public static build metadata.
They do not inspect installed wallets or grant authority. Every wallet-data call is authenticated
and scoped inside Tenshu, regardless of what the adapter or host claims.

| Tool | Input | Result / limitations |
|---|---|---|
| `tenshu_get_capabilities` | `{}` | Public static build metadata only; no installed-wallet inventory or live consent. Proposal availability depends on the experimental native build flag; direct MCP wallet creation is prohibited by design. |
| `tenshu_list_wallets` | `{}` | Only the authorized active Vault; opaque session ID and generic display name. |
| `tenshu_get_wallet_summary` | `{"wallet_id":"…"}` | Confirmed, unconfirmed and available sats, network and sync state. |
| `tenshu_get_transactions` | `{"wallet_id":"…","limit":20}` | Up to 50 most recent confirmed transactions; no pending history or pagination cursor. |
| `tenshu_get_wallet_policy_summary` | `{"wallet_id":"…"}` | Deterministic script type, thresholds, key counts, relative recovery delays in blocks. |
| `tenshu_prepare_wallet_proposal` | Explicit 3-of-5 primary, recovery quorum/delay, bounded retry ID | Requires separate proposal consent in the opt-in Regtest native build. Produces an in-memory topology draft; keys, actual descriptor validation and creation approval are native-only. |
| `tenshu_get_wallet_proposal` | `{"proposal_id":"…"}` | Only the current proposal grant's draft. Existing read grants cannot access proposals, and proposal grants cannot access wallet reads. |

Ordinary builds report `wallet_proposals:false`. The opt-in native demo reports
its Regtest bounds in `proposal_support`; this static metadata is not permission.
Read-only AI access cannot grant proposal authority. There is no direct MCP
approve/install/sign/broadcast tool. See the experimental demo below.

The initial proposal shape is deliberately limited to a 3-of-5 primary and one
explicitly chosen recovery path (1–5 slots, valid threshold, positive block delay)
for Bitcoin Regtest P2WSH. No recovery values are silently selected by Tenshu.
No model input accepts keys, descriptors, paths, networks, labels or credentials.
Only one draft per proposal grant is retained. An exact retry returns that draft;
changed content or a second request returns `proposal_conflict`. Revocation clears
access, and native setup must independently verify the live session before using
a draft. An initial topology draft reports `needs_keys` and `descriptor_validated:false`; only a trusted native callback can advance that validation status.

Confirmed balance excludes coins with an ongoing spend and includes only confirmed
UTXOs. Unconfirmed includes unspent change; the UI groups self-change differently.
Available follows the existing owned-coin predicate and excludes immature/spending
coins. This is coin availability, not proof that signatures or recovery conditions
are satisfied. Snapshots can change between calls and during sync; do not interpret
an unsynced zero as authoritative history. Transaction incoming/outgoing values
reuse the existing UI's representation, not a newly computed net accounting balance.
Incoming values include wallet-owned outputs, including change or self-transfers.
Outgoing values are zero for incoming payments: another sender's change is never
reported as Vault spending.
Key counts count policy keys, not independently verified people/devices. Recovery
conditions do not mean recovery can be performed now.

No seeds, private keys, passwords, credentials, descriptors, xpubs, addresses,
scripts, labels, PSBTs, signing or wallet-mutating tools are exposed. Preparation
changes only bounded session memory and is annotated as state-changing. Signing-session
status is deferred because the existing remote and LAN workflows do not share a
single authoritative read API.

## Privacy and troubleshooting

Local transport keeps this integration off the public network; your AI host may
still upload tool results to its provider. Check that host's privacy configuration.
The server makes no AI calls and adds no telemetry. Existing wallet synchronization
continues through the existing backend. A compromised OS account can inspect
memory/configuration; this feature does not sandbox arbitrary same-user malware.

- `access_denied`: invalid/expired/revoked grant, wallet outside the grant, or
  capability not selected. Wallet absence and lack of permission are intentionally
  indistinguishable. Review the grant in Tenshu; never let the AI approve itself.
- `wallet_unavailable`: desktop/daemon closed or unavailable, stale socket path,
  ten-second read timeout, or transport failure. Unlock/reapprove manually if needed.
- `invalid_input`: unknown fields, missing/wrong fields, wallet ID over 64 bytes,
  or history limit outside 1–50. Tool errors never return raw daemon exceptions.
- No executable: build `make mcp` with the same Cargo profile/target as the desktop,
  or install a bundle containing the adapter. Never use a shell wrapper from an
  untrusted source to work around a path error.
- Testing inside an OS sandbox may require permission to create Unix sockets.

## Validation and follow-up

`coincube-mcp/tests/security.rs` covers the SDK and actual STDIO process, typed
inputs, permissions, per-client isolation, expiration, revocation during reads,
shutdown, disconnection, Unix socket permissions, Windows logon-session ACLs and peer identity, bounded frames and sanitized errors.
`app::local_mcp::tests` exercises trusted settings and the actual embedded daemon
against a disposable SQLite wallet containing public-only synthetic transactions;
the blockchain backend is a test double. It is not a funded regtest/full client UI
acceptance run.

An additional acceptance test launches the actual STDIO executable using the
private configuration issued by desktop Settings, reads all four tools from that
SQLite wallet, and verifies scope denial, locking, revocation and clean EOF. It
runs explicitly in the pinned platform CI jobs after building the companion binary:

```sh
cargo build -p coincube-mcp --features adapter --locked
TENSHU_MCP_TEST_ADAPTER="$PWD/target/debug/tenshu-mcp" BREEZ_API_KEY=DUMMY_BREEZ_API_KEY cargo test -p coincube-gui --lib stdio_client_reads_real_wallet_and_observes_revocation -- --ignored
```

```sh
BREEZ_API_KEY=DUMMY_BREEZ_API_KEY cargo test -p coincube-gui --lib app::local_mcp::tests
cargo clippy -p coincube-mcp --features adapter --all-targets -- -D warnings
cargo fmt --all -- --check
```

Before production release, complete packaged-client acceptance on each platform with
Maple and Claude, verify signed packaging, and obtain independent security review.
Windows named-pipe security is included in this extension. Next add richer client
credential management. Any later unsigned transaction proposal needs a separate
capability, scoped operation ID and trusted desktop review. Signing remains with
existing user/Keychain mechanisms; it cannot be an extension of this read dispatch.

## Experimental native proposal demo

Ordinary builds keep proposal support off. A build opting into GUI
`mcp-proposals` and companion `native-proposals,adapter` implements the first
native-consent flow on an **empty, unlocked Bitcoin Regtest Cube only**. It is
not enabled in release/nightly workflows. Capability discovery reports the
build flag and bounded topology limits; that report is never live consent.

1. Discuss signer independence, shared failure locations and loss of any two
   primary signers. Choose the recovery quorum and positive relative block delay
   explicitly. Block time varies; possession/control of keys and recovery
   eligibility are separate from a descriptor's mathematical validity.
2. In a fresh disposable Regtest Cube, enable AI access and approve the separate
   proposal permission. Existing wallet-read grants cannot prepare a proposal.
   Save the private client configuration only in the trusted client's local file.
3. The client prepares one topology draft. It receives `needs_keys` with no
   public/private keys or descriptor. In Tenshu, click **Review proposal and
   select keys**. This native click opens the existing key picker, backup and
   hardware registration flows. These preparatory actions have their existing
   local/device confirmations; they are not MCP approvals.
4. Select real public keys locally. The wallet engine compiles the descriptor.
   The final native review verifies that its actual 3-of-5 primary and selected
   recovery exactly match the proposal, and displays signer fingerprints, local
   provenance and the descriptor. Only then does proposal status report
   `ready_for_native_review` / `descriptor_validated:true`. Revisions advance
   when validation is invalidated/restored. This proves neither independent
   custody nor a completed key/backup verification.
5. **Create this Regtest Vault** is a native action. Immediately before any
   wallet/daemon persistence, a shared per-Cube OS reservation, captured session
   generation, live grant and exact review snapshot must all pass. There is no
   approve/install/sign/broadcast MCP operation. This first demo accepts only an
   external loopback Bitcoin Core/Knots RPC on port 28443 and no Connect/provider
   enrollment. Creating the Vault consumes/revokes the proposal grant.
6. Completion returns to the launcher. Unlock afresh and approve a new read
   grant to inspect the installed policy/balance. Old proposal IDs/tokens never
   become wallet-read authority. Lock before admission discards setup; lock
   after admission hides setup content while admitted native completion may
   finish. A grant revocation cannot undo already-admitted durable work.

Failed/partial admitted installations retain a per-Cube admission tombstone and
never automatically delete an existing/partial wallet or retry it. Inspect the
local outcome before cleanup/retry; no MCP method can clear the tombstone. The
reservation is retained through native post-install association. Closing the
process can interrupt completion, which is a recovery/inspection case, not a
claim of rollback. This is an experimental Regtest gate, not production release
acceptance.

Keychain guidance uses existing setup and verification screens. The owner's
Cube Recovery Kit encryption key is not a Vault signing key. Do not label a
manually entered key as phone-attested, or claim that a backup/recovery rehearsal
passed because a key or descriptor exists. Phone/Connect-dependent setup is a
separate manual checkpoint when unavailable in the isolated offline demo.
