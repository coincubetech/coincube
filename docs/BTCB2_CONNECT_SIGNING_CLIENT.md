# Connect signing client chain identity

Rail1 implements the desktop obligations in BTCB2_SIGNING_IDENTITY.md against
coincube-api#310 contract at eb7ca9ab067b3f7b951a7fd7171323e7dacc79e5.
The vendored connect.proto is byte-identical to that server revision. It adds
network identity and target capabilities without changing encrypted payloads.

The authenticated client states Wallet.chain.api_str() on every create. A
nonempty resolve identity must match literally; aliases and unknown networks
refuse. An empty resolve or session identity is refused on every chain
(signing identity contract Amendment 1, A3; master plan decision 30): an empty
resolve creates nothing, and an empty created or fetched session is cancelled.
The copy names the Vault's chain by its display label. Before this, an empty
identity was tolerated on Bitcoin-family Cubes during a compatibility window,
which is closed before launch. Created or fetched sessions with an incorrect identity
are discarded and cancellation is requested. No returned signature from that
session is opened, verified, merged or persisted by the desktop. Cancellation
is best effort; a phone may already have received the session.

BTCB2 targets must advertise both chain-identity-v1 and btcb2-unified-v2 before
any PSBT preparation or sealing. The retired btcb2-unified-v1, which released
COINCUBE Mobile builds send without being able to sign, admits nothing: a target
that stores it without btcb2-unified-v2 reads as an outdated app. Registration still advertises only this
desktop's chain-identity capability because the desktop is not the signing
target. A resolved phone carrying both capabilities may classify that exact
Mobile signer as replay-capable for signer/path selection. Existing
cryptographic verification under Wallet.chain remains authoritative for the
transaction's post-signing replay status; capability metadata alone never marks
a spend replay protected.

On a Connect-served BTCB2 Cube the signer bootstrap now feeds this client:
the Cube's own session is saved in its chain folder's `connect.json`, and
device registration and the stream run on that session, pinned to it
(`BTCB2_VAULT_APP.md`, "Connect signer bootstrap"). The signing window's REST
client uses the endpoint the pipeline recorded; on BTCB2 there is no
fallback to the configured API, so the Cube's bearer never reaches the
default host. A local BTCB2 Cube runs the same bootstrap from its own
sign-in, as a Bitcoin Cube does.

The LAN (Wi-Fi) rail's blanket BTCB2 refusals (pairing Start/Pick, hardware
discovery, `PhoneSigner::sign_tx`) were removed by Desktop PR 8c, once v3
chain-bound pairing shipped (PRs 8a, 8b). A pairing row is now dialled only for
its own chain, and on Bitcoin Blake2b only with `chain-identity-v1` and
`btcb2-unified-v2` recorded at pairing; anything else is refused before a session
is sent, with the contract's R2.6 or R2.7 copy shown in the settings and signer
lists. Switching between same-descriptor Bitcoin/BTCB2 twins still revokes an
active pairing run. No COINCUBE Mobile repository changes or live service changes are included.

Verification uses synthetic Vaults and encrypted fixtures: exact/missing/unknown
network matrix, pre-seal refusals, create/fetch defensive cancellation,
capability refusal, server error classifications, existing encrypted-return
merge tests with explicit matching identity. Run the full GUI package and strict
workspace lint with repository-pinned Rust; mocks are not live deployment
acceptance. Infrastructure routing and actual COINCUBE Mobile compatibility remain
separate launch gates. Rollback is reverting this client commit before enabling
BTCB2 signing; never expose BTCB2 to a pre-identity desktop.
