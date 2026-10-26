# Tenshu local AI access (MCP)

This first slice supports **Bitcoin Vaults using Tenshu's embedded daemon on
macOS, Linux and Windows**. It reads the existing wallet engine. It does not support
Liquid, Spark, imported non-Vault policies, Bitcoin Blake2b or external daemons
yet. MCP starts disabled. Explicitly paired clients keep their selected scope until
revoked; protected access pauses while the Cube is locked or Tenshu is closed.
No Connect API or mobile Keychain changes are needed.

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

## Pair a client until revoked

1. Open and unlock the desired Cube normally. Go to **Cube → Settings → AI access**.
2. Enable AI access for this Cube. Read the financial-data disclosure.
3. Enter a client label, explicitly select this Cube's Vault, and select each read
   capability you want to grant. The label is for your reference, not an authenticated
   application identity.
4. Choose **Pair client until revoked**. Enabling the toggle alone grants no access.
   Tenshu saves the exact client scope and a random bearer credential in a
   device-keystore-protected encrypted registry.
5. Choose **Copy private client configuration** and paste it into your trusted MCP
   host's local configuration. It contains a credential: never paste it into a chat,
   issue, shared document or repository. Clear your clipboard after setup.
6. Restart/reconnect the MCP host. Discover tools, then call `tenshu_list_wallets`
   and use the returned opaque `wallet_id` for subsequent calls.

The copied command is derived from the running desktop executable's directory.
The adapter must be installed beside it. The GUI shows the authorized client and
successful-read count; this is not a verified host identity or a persistent-connection
indicator (IPC connections are per request).

Up to 16 independently revocable clients can be paired per Cube. Give separate
clients and Cubes separate host entries. Anyone holding a copied configuration
can impersonate that client within its exact scope while the Cube is available.
The registry is bound to the canonical data directory, chain, Cube, wallet storage
identity and descriptor; a replaced Vault does not inherit its permissions.

Pairing survives normal lock, close, backend suspension and restart. Each unlocked
session publishes a fresh private endpoint through the same private reconnect
locator; the adapter resolves it on every call. Existing client configuration
continues working after ordinary unlock/relaunch. The assistant cannot unlock a
Cube, and no wallet data is returned while it is locked/closed. Disabling AI access
pauses stored clients; it does not forget them. Draft permission fields configure
a new pairing; they never silently expand an existing client's scope. To change
a client's permissions in this slice, revoke its row and pair it again explicitly.

**Revoke this client** removes that client's durable authorization and stops its
runtime immediately. Revoking cannot erase information already given to the AI
provider. Pairing files are also removed on Cube deletion and duress wipe; the
keystore wrapping item can remain inert. Storage/authentication failures restore
no access. If a durable revocation write fails, Tenshu suspends all clients and
tries to invalidate only this registry's wrapping key; an error explicitly states
when durable revocation could not be confirmed. Do not treat that error as success.

Legacy configurations with TENSHU_MCP_SOCKET retain their one-hour session-only
semantics and are never silently upgraded. Pair again through the new UI once
to obtain a persistent configuration.

## STDIO client configuration

The GUI produces this shape, with actual paths and a private pairing credential:

```json
{
  "mcpServers": {
    "tenshu": {
      "command": "/Applications/Tenshu.app/Contents/MacOS/tenshu-mcp",
      "env": {
        "TENSHU_MCP_PAIRING": "/PRIVATE_DATADIR/regtest/mcp-pairings/CUBE_SCOPE/CLIENT.endpoint",
        "TENSHU_MCP_CREDENTIAL": "REPLACE_WITH_THE_PRIVATE_PAIRING_FROM_TENSHU"
      }
    }
  }
}
```

On Windows the copied command ends in `tenshu-mcp.exe`; the MSI installs it beside
`coincube.exe`. The private locator resolves to a local pipe name such as
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
| `tenshu_prepare_wallet_creation` | Public account keys and explicit topology | Separate setup pairing, actual descriptor validation and native builder; creation approval remains native. |
| `tenshu_prepare_transaction` | Opaque wallet/request IDs, recipients in sats, fee rate and fee ceiling | Separate transaction pairing; unsigned Regtest PSBT stays in Tenshu, only a summary is returned. |
| `tenshu_get_prepared_transaction` | Opaque wallet and draft IDs | Current transaction pairing/session summary only; no PSBT or input/change-address export. |

Ordinary builds report `wallet_proposals:false`. The opt-in native demo reports
its Regtest bounds in `proposal_support`; this static metadata is not permission.
Read-only AI access cannot grant proposal authority. There is no direct MCP
approve/install/sign/broadcast tool. See the experimental demo below.

The initial proposal shape is deliberately limited to a 3-of-5 primary and one
explicitly chosen recovery path (1–5 slots, valid threshold, positive block delay)
for Bitcoin Regtest P2WSH. No recovery values are silently selected by Tenshu.
The topology-only tool accepts no keys, descriptors, paths, networks, labels or credentials. Separately scoped public-key wallet assembly is described below.
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

No seeds, private keys, passwords, credentials, descriptors, wallet xpubs, raw
scripts, labels or PSBTs are exported. Read tools omit addresses. A separately
authorized transaction tool echoes only its caller-supplied recipient addresses
and amounts; the AI provider may receive those results. Topology/wallet drafts
change bounded session memory. Transaction preparation also reserves an internal
change index through the existing engine, without saving a spend, signing or
broadcasting. Preparation tools are annotated as state-changing. Signing-session
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
Windows named-pipe and pairing-file security are included. Any later unsigned
transaction proposal needs a separate
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
   enrollment. Creating consumes the current proposal runtime authorization,
   without deleting persistent client pairing.
6. Completion returns to the launcher. Unlock afresh. A proposal-only pairing
   stays registered but does not gain permission to read the new Vault. Explicitly
   pair a read-scoped client to inspect its policy/balance; future setup workflow
   orchestration is a later slice. Old proposal IDs never become wallet-read authority. Lock before admission discards setup; lock
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


## AI-assisted wallet preparation (Regtest experiment)

With the `mcp-proposals` GUI feature and matching `native-proposals` companion,
a separately paired client can call `tenshu_prepare_wallet_creation`. It assembles
one immutable 3-of-5 primary policy with an explicitly chosen timed recovery path
and public account-key assignments. The desktop compiles the actual P2WSH
descriptor using the existing wallet engine and fills the native builder. It does
not create a wallet or assert that devices, independent custody or backups were
verified.

In an unlocked empty Regtest Cube, open AI access, select **Allow this client to
prepare a Regtest wallet using public account keys**, and choose **Pair setup
client until revoked**. This is a distinct durable scope. Existing balance/history/
policy and topology-only clients do not gain this permission. Each client still
needs the user's ordinary Cube unlock; drafts and approval do not survive lock.

Arguments are a JSON object with `request_id`, `topology` and `keys`:

```json
{
  "request_id": "inheritance-demo-1",
  "topology": {
    "primary": {"threshold": 3, "key_count": 5},
    "recovery": {
      "quorum": {"threshold": 2, "key_count": 3},
      "relative_delay_blocks": 144
    }
  },
  "keys": {
    "primary": ["[fingerprint]tpub…", "[fingerprint]tpub…", "[fingerprint]tpub…", "[fingerprint]tpub…", "[fingerprint]tpub…"],
    "recovery": ["[fingerprint]tpub…", "[fingerprint]tpub…", "[fingerprint]tpub…"]
  }
}
```

Replace the placeholders with public account xpubs supplied for this setup. Only
origin-bearing test-network account keys without derivation suffixes or wildcards
are accepted; the origin path must match the account depth. All assignments need
distinct fingerprints and underlying account keys in this first slice. Distinct
fingerprints are not proof of separate ownership. Private keys, seed phrases,
PINs, arbitrary descriptors, filesystem paths and network overrides are not inputs.
No hardware command, secret access or remote request is performed by this tool.

The returned minimized proposal identifies the topology and native descriptor
validation result, without returning xpubs or descriptors. Exact retries return
the same proposal; changed keys, topology or retry identity conflict. Use
`tenshu_get_wallet_proposal` with its opaque ID to inspect that session's state.
Then open **Review prepared wallet in Tenshu**. The ordinary builder is populated
with the public keys and manual/unverified provenance. The user verifies hardware
and fingerprints, handles seed/backup/PIN/passkey steps, reviews the exact
configuration, and chooses **Create this Regtest Vault** in Tenshu. Actual key
changes require a fresh proposal; aliases and other native configuration are
bound by the final native review. No MCP call can approve creation or skip these
steps. The existing local node restriction and once-only creation admission apply.

Creation consumes the proposal session and does not grant read access to the new
Vault. Wallet reads require their own explicit pairing scope. This experimental
acceptance is separate from hardware-device, named-client and production-release
validation; it does not enable production wallet creation.


## Experimental unsigned transaction preparation

Build the desktop with `--features mcp-transactions` and its companion with
`--features adapter,native-transactions` (add the existing proposal features when
also testing wallet setup). This feature is off by default and restricted to a
Bitcoin Regtest Vault on an embedded daemon. It does not enable production wallets,
Liquid/Spark, imported policies, external daemons, signing or broadcasting.

In **Cube → Settings → AI access**, select the Vault and **Prepare unsigned
Regtest transactions**, then explicitly pair the new client. This is separate
from all read and wallet-setup scopes: existing clients are not upgraded, and a
transaction client cannot read balances/history/policy. It can list only its
paired Vault to obtain its opaque wallet ID. Pairing persists until revoked;
unsigned drafts last only for that unlocked session.

`tenshu_prepare_transaction` accepts:

```json
{
  "wallet_id": "OPAQUE_ID_FROM_THIS_CLIENTS_WALLET_LIST",
  "request_id": "payment-1",
  "recipients": [{"address": "BARE_REGTEST_ADDRESS", "amount_sats": 10000}],
  "fee_rate_sat_vb": 2,
  "max_fee_sats": 1000
}
```

The first slice uses exact integer satoshis and an explicit fee ceiling, with
1–8 distinct recipient addresses and 1–1000 sat/vB. Fiat conversion, payment URIs,
send-max, self-send, recovery-path selection, caller-supplied change addresses,
raw scripts/PSBTs and signing/broadcast arguments are excluded. The existing
engine selects eligible coins on the primary path and constructs the unsigned
PSBT. Preparation may reserve an internal change index; it does not reserve the
coins, save a spend, sign, broadcast, or guarantee future input availability.

Tenshu verifies the actual inputs, recipient amounts, wallet-owned change,
unsigned state and total fee before returning a minimized summary. The raw PSBT,
input identities and change address stay inside Tenshu. Request IDs are immutable
after an engine attempt: exact retries return the same draft or error without a
new change reservation, and changed input conflicts. At most eight engine
construction attempts (including failures) are retained per client/unlock. Use `tenshu_get_prepared_transaction` with the
returned draft ID and same opaque wallet ID to reread the summary; foreign IDs
and other scopes are denied.

The paired client's native row offers **Review prepared transaction in Tenshu**.
It opens the exact unsigned construction in the existing review/signing screen,
without saving a spend or requesting a signature. Check every recipient address,
amount, total fee, change and recipient verification status there. Hardware, PIN,
passkey, signing and any later broadcast are user actions under the existing
native flow. Back/cancel returns to AI access. No tool claims recipient identity,
hardware possession or backup verification. Lock/close/revocation clears access
to the session drafts; ordinary unlock restores only the separately approved
pairing, not old draft identities.

Transaction construction is gated on a completed poll newer than the native
startup baseline. Failed engine constructions are cached under their immutable
request IDs and count toward the eight-attempt session bound; retrying a fee-limit
failure does not reserve change again.

The MCP-prepared native review also shows an always-visible unsigned/signature
state and change total/address section from the actual loaded PSBT and wallet
classification. It does not require expansion, saving or signing to inspect
change. Ordinary spend presentation and signing authorization are unchanged.
