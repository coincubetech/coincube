# Coincube Release Notes

## Purpose and Scope

This document tracks all changes made to Coincube since its inception as a hard fork of [Liana](https://github.com/wizardsardine/liana) (v13.0) by Wizardsardine. Coincube is a comprehensive Bitcoin wallet solution featuring vault custody, Lightning-enabled liquid spending, integrated buy/sell, and peer-to-peer trading — built by Coincube Technology LLC.

For architecture details, see the [Devin Wiki](https://app.devin.ai/wiki/coincubetech/coincube) (internal — requires Devin access) or the project `README.md` and `docs/` directory. For the original Liana changelog, see `CHANGELOG_LIANA.md`.

Sources: `README.md`, `Cargo.toml` (workspace root)

---

## Liana vs Coincube: What Changed

The table below summarizes what Coincube inherits from Liana and what is new or significantly modified.

### Inherited from Liana (Vault Engine)

These components are largely unchanged from the Liana v13.0 codebase:

| Component | Description | Sources |
|-----------|-------------|---------|
| `coincubed` daemon | Core wallet engine, JSON-RPC 2.0 API over Unix socket, PSBT handling, coin selection | `coincubed/src/` |
| Output descriptors | BIP 380 descriptor parsing, Miniscript, multi-path policies | `coincube-core/src/descriptors/` |
| Time-locked recovery | CSV-based recovery paths, coin refresh, inheritance timelocks | `coincube-core/src/spend.rs` |
| Database layer | SQLite schema for addresses, coins, transactions, labels, spend PSBTs | `coincubed/src/database/` |
| Bitcoin backends | Bitcoin Core (managed/external), Electrum, Liana Connect | `coincubed/src/bitcoin/`, `coincube-gui/src/services/connect/` |
| Hardware wallet support | Ledger, BitBox02, Coldcard, Jade, Specter DIY via `async-hwi` | `coincube-gui/src/hw.rs` |
| Installer wizard | Multi-step wallet creation with descriptor editor, key import, device registration | `coincube-gui/src/installer/` |
| Transaction management | Spend creation, RBF, coin control, PSBT signing workflow | `coincube-gui/src/app/state/vault/` |
| Multi-wallet / pane / tab | Multiple wallets with panes and tabs in a single window. Now operates inside Coincube's **Cube** abstraction (see below). | `coincube-gui/src/gui/` |
| Functional test framework | Python-based RPC integration tests | `tests/test_rpc.py` |
| Documentation | API reference, usage guide, signing devices, getting started | `docs/API.md`, `docs/USAGE.md`, `docs/TRY.md`, `docs/SIGNING_DEVICES.md` |

### Added by Coincube

These are entirely new subsystems built on top of the Liana base:

| Feature | Description | Sources |
|---------|-------------|---------|
| **Cube Architecture** | New base abstraction replacing Liana's top-level wallet concept. Each Cube is a named container holding one Vault plus Liquid wallet, Buy/Sell, P2P, USDt, and Connect. Multi-cube launcher with per-cube settings and PIN entry. | `coincube-gui/src/launcher.rs`, `coincube-gui/src/app/settings/` |
| **Liquid Wallet** | Lightning-enabled spending wallet via Breez SDK Liquid (send, receive, on-chain swap) | `coincube-gui/src/app/state/liquid/` |
| **Vault ↔ Liquid Transfers** | Bidirectional fund transfers between vault (on-chain) and liquid (Lightning) via Breez SDK, with HW signing | `coincube-gui/src/app/state/liquid/send.rs` |
| **Buy/Sell** | Integrated fiat on/off-ramp via Mavapay (Africa) and Meld (international) with CEF webview | `coincube-gui/src/app/state/buysell.rs`, `coincube-gui/src/app/view/buysell/` |
| **Mostro P2P Trading** | Decentralized peer-to-peer BTC trading over Nostr with chat, disputes, hold invoices | `coincube-gui/src/app/view/p2p/` (branch: `mostro-p2p`) |
| **USDt Wallet** | Liquid-based USDt support via SideSwap with cross-asset payments | `coincube-gui/src/app/state/usdt/` (branch: `mostro-p2p`) |
| **Border Wallet Signer** | Mnemonic seed phrase generation from grid patterns with PSBT signing | `coincube-core/src/border_wallet/` (branch: `mostro-p2p`) |
| **Coincube Connect** | Coincube's own remote backend (Esplora-based) with email/OTP auth via Coincube API, distinct from inherited Liana Connect | `coincube-gui/src/installer/step/coincube_connect.rs`, `coincube-gui/src/services/coincube/` |
| **Light/Dark Mode** | User-selectable themes with persistence, theme-aware rendering | `coincube-ui/src/theme/`, `coincube-gui/src/gui/mod.rs` |
| **Global Home Dashboard** | Unified home showing combined Vault + Liquid balances with accordion sidebar | `coincube-gui/src/app/view/mod.rs` |
| **Toast System** | Global overlay notifications designed to meet WCAG AA contrast guidelines, log level propagation | `coincube-gui/src/app/view/mod.rs`, `coincube-gui/src/app/view/vault/warning.rs` |
| **Coincube API Client** | Go backend integration for buy/sell, geolocation, registration, user management | `coincube-gui/src/services/coincube/` |
| **Fiat Price** | Real-time fiat price display, configurable source, fiat editing on send page | `coincube-gui/src/app/state/settings/general.rs` |
| **Release Infrastructure** | GitHub Actions CI/CD, Windows MSI, macOS DMG with GPG signing, Linux packages | `.github/workflows/` |
| **Documentation** | Build guide, recovery docs, Breez SDK regtest setup, Apple cert rotation, release process, GPG verification | `docs/BUILD.md`, `docs/RECOVER.md`, `docs/RELEASE.md`, `docs/BREEZ_SDK_REGTEST.md`, `docs/APPLE_CERT_ROTATION.md`, `docs/security/` |

### Modified from Liana

These Liana components were significantly adapted for Coincube:

| Component | What Changed | Sources |
|-----------|-------------|---------|
| GUI theme & branding | Full rebrand (colors, logotype, icons), warm color palette, light/dark mode | `coincube-ui/src/color.rs`, `coincube-ui/src/theme/` |
| Sidebar navigation | Accordion-based with expandable sections (Vault, Liquid, Marketplace, P2P, USDt, Connect) | `coincube-gui/src/app/view/mod.rs` |
| Settings persistence | Added `GlobalSettings` with theme mode, developer mode, account tier | `coincube-gui/src/app/settings/mod.rs` |
| PIN security | Encrypted PIN storage, PIN-gated cube access, confirmation flows | `coincube-gui/src/app/settings/mod.rs` |
| Iced framework | Upgraded from Iced 0.13.1 to 0.14.0, deprecated `iced_wry` | `Cargo.toml` |

---

## Release Timeline

### Unreleased (v1.1.0 — target: May 2026)

Everything below shipped after the March 2026 section and is not recorded there.
Entries are grouped by what changed for the user; PR numbers are the coincube
repository's. Internal refactors, test-only work and CI are collapsed at the end.

#### Features

**Recipient identity on send (Branta)**

Sources: `coincube-gui/src/services/branta.rs`, `vendor/branta/`
- Addresses you send to are checked against Branta's Strict recipient list before you confirm, for both Vault and Spark sends, so a substituted address in a pasted invoice or a compromised clipboard shows as an unknown recipient rather than going out silently (#368).
- Recipient logos and Lightning URI handling were corrected shortly after, and the vendored SDK is pinned and hash-checked so a tampered drop fails the build (#371, #373).

**Spark: stablecoin send and SideShift receive**
- Spark can send stablecoin balances, and receiving into Spark now goes through SideShift, which replaced the Liquid swap path (#276).
- Cross-chain addresses are validated before you commit, and the conversion fee is shown in sats rather than left implicit (#278).
- A circuit breaker stops the stable-balance display flapping when the rate source is unavailable, and shared preparation failures use payment wording instead of protocol wording (#366, #379).

**Liquid sunset**
- The Liquid wallet is no longer offered. Existing accounts that already hold a Liquid balance keep it and can still move it; new Cubes do not get a Liquid wallet, and Spark plus SideShift covers what Liquid was there for (#276).

**COINCUBE | Tenshu**
- The desktop app is now COINCUBE | Tenshu throughout, with its own icon on all three platforms and its own bundle identifier (#273, #275, #314).

**macOS signing and notarization**
- Release builds are signed and notarized as a bundle, and CI now checks that the provisioning profile actually authorises the signing certificate rather than discovering the mismatch at notarization time (#316).

**Node resource settings**
- The managed local node's disk and memory footprint is configurable, with a Small computer preset, and the app states what a local node actually costs in disk rather than leaving you to find out during the initial sync (#277).
- The bundled node moved from Bitcoin Core to Bitcoin Knots, with a migration path for existing local nodes (#225, #302).

**Inbound Tor**
- The managed node can accept inbound connections over Tor, off by default (#267).

**Cube unlock hardening**
- Unlocking a Cube was hardened: the seed file is bound to the device and to the Cube it belongs to, so a copied data directory does not open elsewhere, and wallet descriptors are encrypted at rest (#313, #353).
- Passkey unlock is available and is the default where the platform supports it (#318, #319, #320).

**Recovery alerts** (Coming soon)
- Alert recipients are read from the Vault rather than from Cube membership, so the people a Vault actually names are the people who get told (#334), and an owner is no longer sent an alert about their own key (#397).

**COINCUBE | Connect blinding**

Sources: `coincube-gui/src/services/coincube/`, `coincubed/src/connect.rs`
- Connect no longer sees your extended public keys or your transaction drafts. Keys are sealed to your own devices' keys (ECIES) before they leave the app, and signing requests travel end-to-end encrypted between COINCUBE | Tenshu and COINCUBE | Keychain; Connect stores and routes that ciphertext and holds no key that can open it (#249, #315).

**Billing**
- Plans, the plan picker and the checkout flow are in the desktop app, with the Estate launch offer applied automatically on account creation (#153, #163, #209, #222, #226, #227, #229).

**Bitcoin Blake2b (BTCB2) — flag-gated; ships ON only if Lane B4 passes**
- COINCUBE can run a Vault-only Cube on Bitcoin Blake2b, the BLAKE2b proof-of-work fork, served by Connect's BTCB2 Esplora or a managed Knots node. Chain identity is carried end to end — storage, seed persistence, backups, Cube registration, signing sessions and fiat quotes are all isolated per chain, so a BTCB2 Cube can never read or write a Bitcoin Cube's material (#370, #374, #382, #383, #410–#446).
- Replay protection is the point of the feature: a unified sighash makes a BTCB2 signature invalid on Bitcoin, per-input replay status is shown before you broadcast, and signatures that cannot be verified read as unknown rather than protected (#369, #372, #381, #392, #400).
- The Claim machinery — observation, preflight, poison self-transfer, change reservation and submission — is merged but not user-reachable in this release (#423–#466).
- Fiat for a BTCB2 Vault comes from BTCB2 listings only and falls back to native units; it never shows the Bitcoin price (#413).

**Liana Connect is no longer offered when you create a Cube**
- Creating a Cube, and every restore and recovery flow, is local-first: the old "Use Liana Connect" backend choice is gone from those paths. It remains on the Add wallet flow for mainnet and signet (`installer/mod.rs:722`).

#### Fixes

**Spark wallet now works in installed builds**

Sources: `.github/workflows/{main,nightly,releases}.yml`, `contrib/release/wix/main.wxs`, `coincube-spark-bridge/Cargo.toml`
- Release artifacts never contained `coincube-spark-bridge`, the sibling process that hosts the Spark SDK. It is a standalone Cargo workspace, so the `cargo build --package coincube-gui` in each workflow could not build it, and no artifact published since the Spark wallet landed (2026-04-15) had it. Anyone who opened the Spark tab in an installed build was told Spark was "not configured for this cube" — their cube was fine, and there was nothing they could have configured. Development builds were unaffected, which is why this went unnoticed: the gui falls back to the bridge in the checkout's `target/` directory.
- All three platforms now build and ship the bridge alongside the app: in `Tenshu.app/Contents/MacOS/` (signed and notarized with the bundle), in the MSI's `bin` directory, and in the Linux `.tar.gz`. Each workflow asserts the packaged artifact contains it, is the right architecture, is covered by the signature, and answers a JSON-RPC round trip — the failure is invisible to `codesign`, notarization and `spctl`, so only an explicit check catches a recurrence.
- The Linux release asset keeps its `tenshu-<version>-<target>.tar.gz` name but now extracts to a directory holding both binaries; the nightly Linux artifact changes from a bare binary to that same archive.
- The bridge's 15 unit tests and its clippy lints now run in CI (`bridge_tests` job); a bare `cargo test` at the root never reached them.

**Other fixes**

- Recovery Kit and phone recovery: sealing a seed-only kit with a phone, owner COINCUBE | Keychain recovery, inherited-Vault recovery and the Recovery Kit screens were substantially reworked and fixed (#246, #253, #271, #279, #286, #291, #294, #324, #325, #354, #355, #358, #359).
- Duress mode (Coming soon — gated per account by Connect's `duressEnabled` flag, `services/coincube/mod.rs:817`): the PIN is restricted to four digits, a decoy Recovery Kit password returns the locked response, Cube material is not left behind when a wipe hits a filesystem error, and the Vault gate covers the duress case (#256, #285, #292, #299, #301).
- Hardware wallets: advisories for the Coldcard RNG issue and the BitBox firmware issue are shown in-app, used key sources are disabled in the picker, and multi-signature ordering was corrected (#296, #329, #332, #340).
- Windows and Linux: an unmovable window on Ubuntu, a sizing problem on Windows, and vault key labels overflowing their card were fixed (#348, #349, #367, #375).
- Transfers: the fee preview no longer reserves a change address on every keystroke, so a transfer no longer burns change indices it never uses (#474).
- Spark: wallet timeouts, loading failures and several UI problems (#203, #304, #357).
- Vault: unconfirmed and pending transaction states, live confirmation counts, an insufficient-funds error that misreported the cause, resync and rescan behaviour, and self-healing of Vault members (#211, #212, #218, #328, #344, #345, #347).
- Error messages no longer leak backend or SDK wording to the user (#384, #385).

#### Internal and CI

- Test coverage, CI cost controls, toolchain pinning, static-analysis cleanup, dependency mirroring and cross-platform test-harness fixes, none user-visible (#147, #156–#161, #170–#177, #238–#243, #272, #287, #300, #317, #337–#339, #350, #352, #361, #365, #386–#388, #399, #418, #467–#469, #472, #476, #481, #490, #493).

### March 2026

#### Features

**Mostro P2P Trading**

Sources: `coincube-gui/src/app/view/p2p/` (branch: `mostro-p2p`)
- Integrated Mostro protocol for decentralized peer-to-peer Bitcoin trading over Nostr.
- Order book with real-time subscription and order form validation (node limits, premium slider, fiat bounds).
- P2P chat system with deterministic nicknames derived from pubkeys.
- Dispute chat system for trade resolution.
- Hold invoice support with copy feedback.
- Hide Cancel/Dispute/Contact buttons on completed trades.

**USDt Wallet (SideSwap)**

Sources: `coincube-gui/src/app/state/usdt/` (branch: `mostro-p2p`)
- Added USDt wallet powered by SideSwap for Liquid-based stablecoin support.
- Cross-asset payments: send USDt and pay fees with USDt.
- Asset selector logic for switching between L-BTC and USDt in the send flow.

**Coincube Connect**

Sources: `coincube-gui/src/installer/step/coincube_connect.rs`, `coincube-gui/src/services/coincube/`
- Coincube's own remote backend using Esplora, with email/OTP authentication via Coincube API.
- Distinct from inherited Liana Connect (`services/connect/` which uses Wizardsardine's lianalite.com).
- Lightning address support.
- Avatar system with deterministic generation.

**Light/Dark Mode**

Sources: `coincube-ui/src/theme/palette.rs`, `coincube-gui/src/gui/mod.rs`
- User-selectable light and dark themes with persistence across sessions.
- Theme-aware logotype, sidebar, and widget rendering.
- Sun/moon toggle icon in sidebar.

**Border Wallet Signer**

Sources: `coincube-core/src/border_wallet/` (branch: `mostro-p2p`)
- New `coincube-core` module for Border Wallet recovery phrase generation.
- Grid creation, pattern building, and enrollment derivation.
- PSBT signing integration with zeroization of secrets on drop.

**Buy/Sell Improvements**

Sources: `coincube-gui/src/app/state/buysell.rs`, `coincube-gui/src/app/view/buysell/`
- Improved sell UI and buy mode flow.
- Prevent double-spend for lightning fulfillment.
- Skip invoice display screen for Mavapay buy widget.
- Restore default styling for sell mode.
- Developer pay-in simulation for sell mode.
- Globally re-enable Mavapay with runtime env detection (`ENABLE_MAVAPAY`).

**Toast Notification System**

Sources: `coincube-gui/src/app/view/mod.rs`, `coincube-gui/src/app/view/vault/warning.rs`
- Migrated to global toast overlay with severity colors designed to meet WCAG AA contrast guidelines (targeting 4.5:1 ratio).
- Chronological sorting, log level propagation, and extracted notification theme helper.

**Wallet Recovery**

Sources: `coincube-gui/src/app/state/liquid/`
- Lightning mnemonic usage for master key and protocol restore.
- Added missing recovery flow for Liquid funds.

**Node Management**

Sources: `coincube-gui/src/app/settings/mod.rs`
- Switch between Connect and local node in settings.
- Debounced bitcoind RPC polling.
- IBD-based detection for blockchain download completion.
- Automatic switch to local node after sync.

#### Fixes
- BTC URI-prefilled amount now validates balance and lightning limits.
- Fixed accordion collapse issues.
- Deduplicated code, removed dead handlers, fixed state resets.
- Fixed Mostro config path and protocol message iteration.
- Handle first poll not yet returned.
- Fixed fresh data directory detection bug.

---

### February 2026

#### Features

**Liquid Wallet**

Sources: `coincube-gui/src/app/state/liquid/`
- Added payment refund functionality for failed Lightning payments.
- Cancel button for pending operations.
- Recovery flow for Liquid wallet funds.
- BIP39 word suggestions as you type during seed recovery.
- Liquid BTC receive feature (COIN-287).
- Liquid-to-Vault transfer UI flow refinements.

**Buy/Sell**

Sources: `coincube-gui/src/app/state/buysell.rs`, `coincube-gui/src/app/view/buysell/`
- Region selector for Meld buy/sell.
- Copy recipient address button in Meld webview.
- Allow selecting existing address in buy/sell flow.

**Settings**

Sources: `coincube-gui/src/app/state/settings/general.rs`
- Developer mode toggle (removed unused MFA).
- Fiat price enabled by default (COIN-285).

#### Fixes
- Refundable flow error toast messages.
- Handle unsupported networks in Liquid wallet.
- Fixed rusqlite_migration build script.
- Vault send form now respects global sats/BTC display setting.

---

### v1.0.0 — January 31, 2026

#### Features

**Liquid Wallet (formerly "Active")**

Sources: `coincube-gui/src/app/state/liquid/`
- Full send and receive flow for Liquid wallet via Breez SDK.
- Send to on-chain addresses from Liquid wallet.
- Vault ↔ Liquid bidirectional funds transfer with hardware wallet signing.
- Reusable transactions component for consistent display.
- Balance display in Global Home section.
- Loading indicators for Liquid send and receive.
- Input validation for send amounts.
- Renamed "Active" to "Liquid" across the codebase.

**Buy/Sell**

Sources: `coincube-gui/src/app/state/buysell.rs`, `coincube-gui/src/app/view/buysell/`
- Switched Mavapay implementation to use Coincube API backend.
- Enhanced Mavapay checkout and order confirmation UI.
- Order history view.
- Previous/back buttons in buy/sell flow.
- Reset button fix in buy/sell history and after Mavapay purchase.
- Meld UI improvements and improved error text.

**Cube Management**

Sources: `coincube-gui/src/launcher.rs`, `coincube-gui/src/app/settings/mod.rs`
- PIN confirmation required before cube deletion.
- Force sync and export functionality.
- Loading indicators for cube creation and PIN entry buttons.
- Asynchronous cube settings save with UI error display.
- Allow retrying cube save without re-running installation.

**Infrastructure**

Sources: `.github/workflows/`, `coincube-gui/src/services/`
- Promoted version to v1.0.0.
- GPG signing and DMG release for macOS.
- Coincube Esplora integration.
- Migrated SSE implementation to `reqwest-sse` for real-time event streaming.
- Passwordless auth migration.
- Toast message when address is copied under Vault receive.
- Error toasts now stack.
- Scrollable error toasts for long messages.

#### Fixes
- Fixed Breez `sign_ecdsa_recoverable`.
- Fixed PIN entry delete issue.
- Fixed vault settings bug.

---

### December 2025

#### Features

**Foundation**

Sources: `coincube-gui/src/launcher.rs`, `coincube-gui/src/app/settings/mod.rs`
- Hard fork from Liana with full rebrand to Coincube.
- Removed auto-migration (no longer supported after fork).
- Cube architecture: multi-cube launcher with per-cube settings and wallets.
- PIN entry system for cube access with UX enhancements.

**Liquid Wallet**

Sources: `coincube-gui/src/app/state/liquid/`
- Integrated Breez SDK Liquid for Lightning-enabled spending wallet.
- Data fetching from Breez SDK.
- Home page setup with balance overview.
- Confirm transfer view for Vault ↔ Liquid movements.

**Buy/Sell**

Sources: `coincube-gui/src/app/state/buysell.rs`, `coincube-gui/src/app/view/buysell/`
- Mavapay integration moved to main buy/sell panel.
- Inline Mavapay client functions.
- User logout from buy/sell panel.
- Country symbol display and reference labels for quotes/orders.

**GUI**

Sources: `coincube-gui/Cargo.toml`, `coincube-ui/src/`
- Upgraded Iced to 0.14.0.
- Validation hint messages on spend transaction view.
- Active settings page view.
- Deprecated iced_wry and Onramper.

#### Fixes
- Fixed assigning remote wallet to cube.
- De-duplicated cube create logic.
- Added missing signer definitions.
- Fixed installer bugs and formatting.

---

### August–November 2025 (Pre-Rebrand Foundation)

Coincube-specific development began in August 2025 while still carrying the Liana name internally.
This period laid the groundwork for the full rebrand in December.

#### Features

**Buy/Sell Platform**

Sources: `coincube-gui/src/app/state/buysell.rs`, `coincube-gui/src/app/view/buysell/`
- Meld buy/sell integration via embedded CEF-based webview.
- Mavapay payment flow with webview checkout.
- Onramper integration for international users.
- Geolocation-based provider routing (country/ISO code detection, manual fallback).
- Native login forms and account type selection for non-webview builds.
- Runtime feature detection replacing compile-time feature flags (`dev-coincube`, `dev-meld`, `dev-onramp`).
- Fiat amount validation and currency converters.

**Webview Engine**
- CEF-based webview for embedded buy/sell flows.
- Advanced webview interface with performance tuning (reduced framerate, memory usage, tickrate).
- Webview fallback rendering for unsupported platforms.

**Fiat Price**

Sources: `coincube-gui/src/app/state/settings/general.rs`
- Fiat price display on home page with configurable caching.
- Fiat amount editing on the send page with validation.
- Global fiat price cache shared across panels.

**Coincube API Integration**

Sources: `coincube-gui/src/services/coincube/`
- Folded geolocation service into unified Coincube service.
- Folded registration service into Coincube service.
- Coincube API client for buy/sell backend operations.

**Security**

Sources: `coincube-gui/src/app/settings/mod.rs`
- PIN encryption with improved storage.
- Encrypted descriptor backup and import (BIP draft).

**GUI**

Sources: `coincube-gui/src/app/view/mod.rs`, `coincube-ui/src/`
- Accordion-based sidebar with expandable sections (Vault, Liquid, Marketplace, P2P, USDt, Connect).
- Global Home dashboard showing combined Vault and Liquid balances.
- Home page auto-refresh every 10 seconds and on scroll-to-top.
- Windows application icon.

**Release Infrastructure**

Sources: `.github/workflows/`
- GitHub Actions release pipeline.
- Windows MSI installer via `cargo-wix`.
- macOS `.app` bundle packaging.
- Linux dependency management and CI setup.

#### Fixes
- Reduced webview memory usage and rendering lag.
- Fixed encrypted backup sanitization before writing to disk.
- Manual country selection fallback when geolocation fails.

---

> **Heritage:** Coincube's vault daemon is built on [Liana](https://github.com/wizardsardine/liana)
> (v0.2–v13.0) by Wizardsardine. The original Liana changelog is preserved in `CHANGELOG_LIANA.md`.
