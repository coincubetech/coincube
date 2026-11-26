# Bitcoin Worldview Modes

Bitcoin Worldview is an opt-in presentation preference for Tenshu. The redesign's
navigation, layouts, forms, typography, financial hierarchy and wallet workflows
provide the common design. The modes vary the approved display copy only:
mainnet network names and voice lines.

> **Changed on 2026-11-25 (Robert).** Bitcoin Worldview is wording only.
> COINCUBE Desktop shows no illustrations, the Artwork preference is removed
> and the modes have no accent colours. The app still draws artwork until the
> Desktop V3 Phase 1 work lands; that plan is in the company brain under
> `plans/desktop-v3-phase1/`. This document describes the target.

## Modes

| Mode | Tagline | SHA-256 mainnet display | BLAKE2b mainnet display |
| --- | --- | --- | --- |
| Samurai | The Way of the Bitcoin | Bitcoin (BTC) | Bitcoin BLAKE2b (BTCB2) |
| Suitcoiner | The Market Has Spoken | Bitcoin (BTC) | LukeCoin (BTCB2) |
| Luke | No Spam Shall Pass | Spamcoin (BTC) | Bitcoin (XBT) |

- **Samurai** is the default. Keep ordinary financial copy neutral.
- **Suitcoiner** speaks for market consensus and institutional adoption.
- **Luke** speaks for validation, sovereignty and the ledger.

Every mode uses the same colours, typography and layout. None has artwork.

Keep humor in preferences, idle or empty states and optional post-success copy.
Keep confirmations, failures, security warnings and recovery instructions neutral.

## Device preference

Place **Bitcoin Worldview** in **App preferences → Appearance**, reachable from
the Launcher and inside a Cube, with the existing “This device” scope. Offer one
choice per mode that shows its tagline. Under the choices, show what the selected
mode changes: its network names from the chain presentation registry (BLAKE2b
only where that network is switched on) and example wording from the voice
catalog, for Samurai as for the other modes.

Persist the choice in installation-wide `global_settings.json` with the stable
values `samurai`, `suitcoiner` and `luke`. A missing value or an unrecognized
string selects Samurai. Reuse the locked single-field preference writer and
publish the accepted value after its save receipt. A failed save leaves the
previous choice active. The selection takes effect without restarting.

Worldview and System/Light/Dark are independent. As presentation consumers are
added, read the accepted mode at render time or refresh their presentation caches.
Retain pane, route, focus, draft, balance privacy and selected-chain state. A
worldview edit must never run the global reload that reconciles fork availability
and Split flows.

There is no **Artwork** row. Earlier builds saved an `existing` or `scene`
choice per mode under `artwork` in `global_settings.json`. Stop reading and
writing it, and keep loading settings files that still carry it.

## Localization

Language is independent of worldview and color scheme. All language and worldview
combinations must work. Spanish is the first additional locale to validate.

- Give new user-facing strings stable message IDs with English fallback. Keep the
  setting name, help, taglines, humor and technical label templates in the
  presentation copy catalog. Translate full messages and named placeholders so
  Spanish can reorder words and punctuation naturally. Review Spanish satire for
  tone as well as accuracy.
- Initially treat Samurai, Suitcoiner, Luke and the chain aliases as proper names.
  Keep tickers and canonical chain identifiers stable. Translate field labels and
  network qualifiers, for example `Red: Bitcoin BLAKE2b · Red principal`.
- Select locale independently of the saved worldview enum. Keep translated copy
  out of preference keys, widget IDs, input parsing and routing.
- Validate Spanish across all three modes, both color schemes and English
  fallback. Check minimum window width, high zoom, focus, accent marks and
  wrapping of full taglines and technical network labels. Format numbers and
  currencies according to the app's locale policy when available, without
  changing their values, rounding rules, units or amount parsing in this feature.

The desktop language selector is a device preference under **App preferences →
Appearance**. It defaults to English and uses the accepted settings receipt
independently of worldview. The Spanish catalog includes Samurai's desktop
navigation, setup, wallet screens, settings, bundled quotes, and app-authored
error guidance. Translate messages at presentation time so the accepted locale
also applies to deferred views and errors received earlier. Preserve canonical
network names, addresses, amounts, and support codes. Diagnostics returned by
external services, devices, or libraries may appear in their original language.
Voice lines use the accepted language independently of the selected worldview.

## Chain identity and financial safety

Keep canonical `coincube_core::chain::ChainId` as the source of truth. The GUI
presentation registry maps `(WorldviewMode, ChainId)` to mainnet display aliases
only on approved surfaces. Preserve existing technical names for Testnet,
Testnet4, Signet, Regtest and BLAKE2b Testnet4.

Derive a mode-independent technical network label from the exact chain held by the
operation. Show it in the action area for signing, broadcast, replay and Split
review, address verification, network selection for a wallet operation, and import
or restore. For example, Luke may show `Bitcoin (XBT)` as secondary copy beside
`Bitcoin BLAKE2b · Mainnet`. Bitcoin SHA-256 and BLAKE2b share address encodings,
so an address or the projected `bitcoin::Network` cannot identify the chain.

Keep aliases out of chain selection and admission, addresses, transaction
construction and signing, PSBTs, descriptors, replay checks, broadcast targets,
fees, price symbols, Connect/API/RPC payloads, exports and wallet persistence.
Keep asset and amount arithmetic on the existing typed paths. Use the existing
wallet and chain data as input to presentation.

## Voice and humor

Samurai uses calm, direct language and is the default when a view has no
accepted worldview. Its voice lines are stoic: quiet discipline and elegant
simplicity, with no satire, market or ideology jokes, so they suit every user
as the neutral voice. Samurai speaks on the same passive surfaces as the other
modes, uses one chain-neutral line for an empty Vault, and stays out of the
consensus exchange in the picker. Suitcoiner treats market consensus and institutional
adoption with absolute confidence. Luke treats validation, sovereignty, and
the ledger with austere conviction. Deliver the satire sincerely. A short line
beside a clear label is usually enough.

Keep the English and Spanish pairs together in
[`coincube-ui/src/worldview_copy.rs`](../../coincube-ui/src/worldview_copy.rs).
The launcher welcome, overview, creation, empty history, synchronization,
payment success, and Appearance screens draw from that registry. The picker
lets Suitcoiner say “Consensus by market capitalization.” and Luke answer
“Consensus is not a popularity contest.” A switch message follows an accepted
device preference save. Rivalry Easter eggs are reserved for a later pass.

On a visible, synchronized Vault with a known zero balance, the active typed
mainnet selects the joke. Suitcoiner calls an empty BLAKE2b Vault
“institutionally compliant.” Luke is pleased when a SHA-256 Vault has no
Spamcoin. Samurai's empty-Vault line names neither chain. A hidden, loading, stale, or
unscanned balance and any test network suppress these lines in every mode.

Keep seed phrases, recovery instructions, error messages, fees, addresses,
confirmation statuses, and technical settings neutral. Payment captions are
secondary to the existing outcome and amount. Broadcasting does not establish
confirmation. Mode aliases are confined to the existing passive display surfaces.

## No artwork, no accents

Decided on 2026-11-25. Before then each mode had two artwork sets and a
decorative accent colour.

- No screen shows an illustration: not Opening or loading, the startup error,
  Overview, payment results, empty activity, Backup, Recovery Kit, the PIN
  prompt or Restore complete. A finished step shows a plain check mark.
- Quotes stay, as text. Quote choice stays stable during a mode switch.
- The modes have no accent colour. Warning, error, success, focus and
  chain-identification colours are unchanged and mean what they meant.
- Asset symbols, the interactive Launcher Cube and the app mark are not
  artwork and stay. Whether Cube avatars stay is an open decision in the
  Desktop V3 Phase 1 plan.
- The image files under `coincube-ui/static/images/worldview`, `kage` and
  `recovery`, and the Cube art used only by quote screens, are removed with
  the code that draws them.

## Delivery and validation

1. Add this spec and the device preference. Test defaults, old and future
   settings, save receipts, reloading, failed saves and keyboard interaction.
2. Classify label call sites and add the presentation registry and canonical
   confirmation labels. Test all seven chain IDs in every mode. Compare fixed
   transaction fixtures across modes for selected inputs, outputs, fees, PSBTs,
   signing, replay eligibility and broadcast routing, including the twin-chain
   case with shared address encodings.
3. Illustrations and accents shipped in this step and are removed by the
   2026-11-25 decision. Verify that a mode switch still updates network names
   and voice lines live, and that no screen is left with an empty space where
   art was. Validate Spanish against the same layouts.

COINCUBE Mobile remains a separate mobile signing surface. Its prompts retain their
canonical chain identity. Review phone-signing handoffs from Tenshu in each
worldview.
