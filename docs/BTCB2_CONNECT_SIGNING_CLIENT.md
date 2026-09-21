# Connect signing client chain identity

Rail1 implements the desktop obligations in BTCB2_SIGNING_IDENTITY.md against
coincube-api#310 contract at eb7ca9ab067b3f7b951a7fd7171323e7dacc79e5.
The vendored connect.proto is byte-identical to that server revision. It adds
network identity and target capabilities without changing encrypted payloads.

The authenticated client states Wallet.chain.api_str() on every create. A
nonempty resolve identity must match literally; aliases and unknown networks
refuse. Empty resolve/session identity is tolerated only for Bitcoin-family
Cubes during the existing compatibility window. BTCB2 rejects an empty resolve
before creating anything. Created or fetched sessions with an incorrect identity
are discarded and cancellation is requested. No returned signature from that
session is opened, verified, merged or persisted by the desktop. Cancellation
is best effort; a phone may already have received the session.

BTCB2 targets must advertise chain-identity-v1 before any sealing. Registration
advertises this desktop capability. This does not advertise btcb2-unified-v1,
change signer replay classification or enable unsupported signing. Existing
cryptographic verification under Wallet.chain remains authoritative for replay
protection. Identity metadata alone is not spend authorization.

LAN now has explicit BTCB2 refusal independent of the global dormant flag:
pairing Start/Pick refuses, hardware discovery skips phone browsing/dialing,
and PhoneSigner::sign_tx refuses before inspecting or sealing a PSBT. Switching
between same-descriptor Bitcoin/BTCB2 twins revokes an active pairing run.
These guards remain until chain-bound pairing v3 and signer prerequisites ship. The additive protobuf fields on the pre-identity LAN constructor
remain empty; this PR does not pretend it implements the LAN identity contract.
No Keychain repository changes or live service changes are included.

Verification uses synthetic Vaults and encrypted fixtures: exact/missing/unknown
network matrix, pre-seal refusals, create/fetch defensive cancellation,
capability refusal, server error classifications, existing encrypted-return
merge tests with explicit matching identity. Run the full GUI package and strict
workspace lint with repository-pinned Rust; mocks are not live deployment
acceptance. Infrastructure routing and actual Keychain compatibility remain
separate launch gates. Rollback is reverting this client commit before enabling
BTCB2 signing; never expose BTCB2 to a pre-identity desktop.
