# BTCB2 Connect Vault construction

Installer construction now retains ChainId from entry through Context, restore
API filter strings and node selection. The existing Bitcoin constructor is a
compatibility wrapper. The public chain-aware constructor still refuses
RuntimeSupport::Dormant before generating a signer.

The fork builder has a dedicated CreateWallet step sequence with authenticated
Connect selection and no managed-node, generic-node, legacy remote-wallet or
Breez/Spark step. Other fork installer flows and supplied legacy remote/SDK
clients are refused at the public boundary. This constructs a Vault flow; it
does not enable Home Cube creation, Claim/Split or seed-only fork products.

The descriptor editor resets stale Taproot state on chain load, refuses a
Taproot event, rejects invalid type/chain at apply, and hides the Taproot picker
in every template's advanced settings. P2WSH remains the supported creation
format. These guards are independent of the global dormant runtime gate.

Dependencies before activation: #410/#414 installer and seed isolation, #412
registration chain mapping, #411 loader isolation, #416 signer discovery, and
a chain-safe descriptor key picker. The current picker still exposes legacy
Keychain/hardware capabilities and needs explicit fork restrictions. Home
creation/feature visibility and a chain-bound session for seedless paths are
also unfinished. No global runtime flag is changed by this PR.

Tests use in-memory contexts and synthetic signers: private construction retains
both fork identities without creating a directory, public construction refuses
while dormant, and Taproot state/events/apply fail closed. Required full GUI
and pinned strict-clippy CI remain the acceptance evidence.
