# BTCB2 Connect Vault construction

Installer construction now retains ChainId from entry through Context, restore
API filter strings and node selection. The existing Bitcoin constructor is a
compatibility wrapper over the same gate: a Cube on another chain is refused
rather than built as a Bitcoin installer carrying that Cube. The public chain-aware constructor requires the explicit
authenticated Connect capability and current client for fork creation. The
installer rechecks the account flag before backend admission and persistence.

The fork builder has a dedicated CreateWallet step sequence with authenticated
Connect selection and no managed-node, generic-node, legacy remote-wallet or
Breez/Spark step. Other fork installer flows and supplied legacy remote/SDK
clients are refused at the public boundary. This constructs a Vault flow; it
does not enable Claim/Split or seed-only fork products. Home creation separately
requires the current authenticated account flag.

The descriptor editor resets stale Taproot state on chain load, refuses a
Taproot event, rejects invalid type/chain at apply, and hides the Taproot picker
in every template's advanced settings. P2WSH remains the supported creation
format. These guards are independent of the global dormant runtime gate.

The integrated route includes installer/seed isolation, registration chain
mapping, loader isolation, signer discovery and explicit descriptor key-picker
restrictions. Generic runtime support remains dormant; the authenticated Connect
capability is deliberately separate. No production account flag is changed.

Synthetic coverage includes the public installer, PIN decrypt and authenticated
embedded reopen, account-failure/no-write matrices and feature-hidden Home
transitions. Full package tests on repository-pinned CI and independent final-head
review remain required; local compile checks are supplemental evidence only.
