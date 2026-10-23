# Tenshu — navigation concepts and detailed Settings layouts

Status: Robert selected Concept C on 2026-10-21. Its in-Cube navigation structure is selected; Launcher integration, detailed visual design and final implementation brief remain open. A/B remain comparison history, not active alternatives. No Rust/Iced application code changed. The local HTML is a synthetic review prototype, not a wallet. Existing product identity is retained; this round changes information hierarchy and interaction structure.

## Decision

**Selected: C, One readable sidebar.** Preserve its everyday task hierarchy, grouped wallets, explicit Expert access and clear Settings scope. The earlier recommendation became Robert’s explicit choice. Launcher structure was underrepresented in the mockups; launcher-design-brief.md now defines that separate app level. The proposed 3D Launcher is still exploratory and does not approve a visual implementation.

| Concept | Navigation and Settings topology | Best fit | Honest tradeoff |
|---|---|---|---|
| A · Top workspaces | Compact Cube/account header → horizontal Overview, Wallets, Activity, Protection, Expert, Settings workspaces. Settings uses a category list beside bounded content. | Users frequently move between overall funds, protection and account-level work. | Wallet identity is less spatially persistent; every wallet task needs an explicit wallet context. More horizontal pressure with localization. |
| B · Wallet workspace | Persistent Cube wallet list → wallet workspace tabs → scoped Settings sections in the content header. Shared Cube protection, account and app entries remain in the wallet list footer. | Users mainly work within one wallet and switch between Spark, Vault and Liquid. | Shared tasks can be mistaken for wallet-specific tasks. Explicit scope and escape routes are essential. |
| C · One readable sidebar | One text hierarchy: Overview, Activity, Backup & protection; wallet entries; collapsed Expert tools; anchored Settings and Connect account. Settings categories are content tabs, with no tertiary rail. | Everyday use with deliberate access to specialist tools. | Grouping must stay disciplined as features grow; many Settings tabs require wrapping or a section selector at compact widths. |

All three keep one active Cube, network and account identity visible. Cube switching is always available in the header. App preferences and account management can be reached without closing/reopening a Cube. Current open-Cube tabs need not be removed by this redesign; evaluate tab density separately from navigation.

## What to compare in the explorer

Start with Backup & protection in each concept. Then find a display preference, phone pairing, node health, Lightning Address, an accounting export and account devices. Toggle the review state to signed out, stale Cube connection, offline node, no Vault, imported Vault, Testnet4, loading and save error. A light-theme preview tests the same hierarchy; it does not prove the current native theme is correct.

The explorer has 11 detailed Settings layouts. The top “Settings layout” selector is a review control for jumping between them; inside the proposed app, categories are limited to the selected scope. Ordinary preference controls demonstrate form layout; consequential controls explain the next existing workflow. They never reveal a seed, pair a phone, contact a server, sign out, export data or change a live setting. No action in the prototype is evidence of a completed native workflow.

## Common layout and interaction rules

- Header: active Cube name and explicit network; account authentication and Cube connection are separate labeled states. An orange dot alone is insufficient.
- Settings heading: named scope and object, not just General. Scope selection navigates between affected objects; it does not change a setting’s storage scope.
- Bounded content: use aligned rows with labels, explanatory text and controls close together. Let long descriptions wrap. Use section rules rather than a rounded container for every row.
- Risk: keep an unresolved backup action accessible from the shell, with one compact status entry linking to the full explanation. Backup pages lead with incomplete protection. Do not duplicate two large warnings above every unrelated screen or collapse different risks into one severity color.
- Expert tools: coin control, PSBT proposals, descriptors and maintenance remain reachable through one stable named entry. Common tasks do not require enabling Developer mode. Final eligibility still follows the actual wallet/network/capabilities.
- Native desktop: Tab/Shift-Tab order follows reading order; provide visible focus, accessible names/roles/states, standard keyboard activation and platform-appropriate shortcuts. No hover-only functionality. The web explorer does not validate Iced accessibility.
- Compact range: the native evidence used 2738×2112 and 2048×1664 pixel captures. Do not infer logical points from Retina pixels. Validate the implementation at actual supported logical window dimensions. Keep Cube switching, Settings and Expert access reachable without scrolling away the primary identity/navigation.
- Saving: the prototype proposes explicit Save/Discard for grouped identity/display forms. Final behavior must reconcile current persistence semantics. Theme/other existing immediate preferences retain clear immediate feedback; do not silently change save semantics. Consequential actions use their existing focused review/confirmation flows.
- Dirty forms: preserve edits on failure; display a specific error at the affected group; prevent duplicate submits while saving; retain focus. Ask Save/Discard/Stay before leaving a dirty form. The explorer illustrates these states but does not implement every persistence transition.

## Detailed Settings layouts

### Display & appearance

Scope: show the actual affected Cube/device for each preference. Source validation must settle persistence for units, display mode, currency and badges; grouping in one screen must not imply a shared storage scope.

Order: amounts and currency → Cube identity/avatar → application appearance. Controls: BTC/Sats unit, primary fiat/Bitcoin amount, fiat estimate enablement, currency, exchange-rate source, direction badges, name where an existing rename capability is available, Avatar entry, application theme link. Network is read-only context, not a full-size preference row. Preserve chain-specific fiat restrictions and unavailable currencies. On save error, retain inputs and explain which preferences failed. Do not add a new rename API merely to implement the mock.

### Backup & protection

Scope: active Cube, with Vault-specific recovery sub-section clearly named. First block: actual readiness summary and one next action. Methods: seed phrase status; Connect Recovery Kit status, methods and requirements. Existing recovery-method distinctions, encryption/password/Keychain options, descriptor freshness and positive-evidence rules must be preserved when the underlying panel is opened; do not reduce all protection to a single boolean.

Keep seed reveal in its existing focused authenticated workflow. Recovery Kit links go directly to this screen/method, never General. A separate Vault recovery section explains time-locked **spending**, using live policy/chain availability and a clear “View recovery path” route. Alerts show verified eligibility, unknown/loading, signed out, connection error and denied states distinctly. Never claim an upgrade is required while entitlement is unknown. No seed, descriptor or Recovery Kit is generated in the concept.

### Wallet & signers

Scope: selected Vault and network. Order: wallet identity/name → human-readable normal/recovery policy → signer list and aliases → Pair with Keychain → eligible Connect members → expert descriptor/key/hardware registration disclosure. Keep signer fingerprints distinct from Vault identifiers. Preserve imported-wallet and hardware-only differences; do not infer local key ownership. Unsupported actions carry reasons, not inert buttons. If no Vault exists, explain that Vault settings do not apply and link to the available wallets.

### Pair with Keychain

Scope: selected Vault and eligible signer key. Show the key the phone holds, recognizable alias, supported transport and availability reason before discovery. The existing local flow requires the same Wi-Fi and signs without the Connect API; do not label it as requiring Connect sign-in. Keep remote/Connect signing capability distinct rather than inventing a transport toggle. Place key alias editing nearby or link directly to the correct field.

Layout states: idle with eligible key; no Keychain keys; key not selected; discovering/phone picker; secure QR/waiting with cancel/expiry; error with retry; paired phones with meaningful identity/status. Preserve cryptographic handshake and consent. Show full key details only on disclosure. The original observed disabled Pair phone state had an unknown cause; no cause has been retroactively asserted.

### Payment checks

Scope: current implementation’s recipient-check preference, labeled accurately. Lead with what is checked and when. Keep recipient identity checks separate from cosmetic display controls. Place Branta strict-mode privacy explanation and privacy-information link beside the preference. Preserve the exact claim: plaintext destination and amount are not shared by strict mode, but lookup timing and network connection may be observable. Never imply that this check guarantees a recipient or removes transaction review. Retain security safeguards around disabling checks.

### Connections

Scope: account auth, this Cube’s service session and selected wallet backend, each explicit. Lead with human-readable connection/sync health and specific recovery action. Account Signed in does not prove the Cube is connected, registered, admitted or entitled. Include checking, offline, expired session, reconnect/reopen-required and error states without showing credentials.

Node: active backend and synchronization before backend switching. Resource presets/pruning/mempool/custom fields and logs/paths/socket/version details appear under expert disclosure. Maintenance/rescan has a separate consequential section showing requirements, start point, validation, progress/cancel behavior only where currently supported, and consequences before starting. Preserve irreversible pruning warnings and restart requirements. Never auto-switch backend or rescan to repair authentication.

### Spark preferences

Scope: Spark wallet plus device-specific Stable Balance preference. Lead with actual preference state and current balance implications. A restored wallet holding USDB while the device preference is off gets an inline explanation here and compact shell attention, not an automatic preference change.

Stable Balance remains a focused review flow; do not turn a consequential conversion into an unreviewed generic toggle. Lightning Address shows account authentication, this Cube’s service readiness, then current address/claim state. Loading, existing address, available claim, validation error, conflict, network error and feature gate remain distinguishable; preserve actual username/eligibility rules. Bridge health is concise, with subprocess, JSON-RPC/stdio and sanitized error details under diagnostics. Testnet unsupported state explains the network gate. No invented yield, peg guarantee or recovery guarantee.

### Data & exports

Scope: selected wallet for files, active Cube for Recovery Kit. Three groups: protection material (direct Recovery Kit link, encrypted descriptor); wallet portability (wallet import/export and plaintext descriptor); records (BIP329 labels and transaction table). Explain file purpose and privacy implications before initiating an export. Records do not replace recovery material. Preserve import validation, encryption, hardware registration and native file dialogs. Never expose an export button that bypasses current safeguards.

### Account & plan

Scope: Connect account across Cubes. Order: identity/authentication → current plan and verified billing/expiry/payment status when available → billing history → compare plans → secondary promo/referral entry → trusted Contacts → sign out. No plan prices, renewal dates or entitlements are invented in the mock. Preserve existing upfront-payment semantics and transaction handoff.

Contacts sub-layout: one heading, compact recognizable contact rows with their verified roles, explicit add action and existing edit/remove detail. Empty state explains the purpose. Handle long emails, duplicate/invalid entries, loading and error. Use synthetic identities in external design material.

### Devices & security

Scope: Connect account. Verified-device rows keep recognizable device name, platform, current-device label and last activity close together. Device removal names its actual effect after source validation; use a review step that identifies the device and explains consequences. Do not guess whether removing it revokes all tokens or destroys data. Recent login activity shows a compact summary and a route to all records, with successful/failed states in text as well as color. Preserve meaningful timestamps/time zones and diagnostic details on demand. Duress remains gated: it was not visible in this reviewed account and must not become an invented available control.

### About & diagnostics

Scope: installation. App version plus verified build provenance; daemon/bridge versions under details. Download Stats belongs with product reference, not beside protection. App-wide preferences include theme, Developer mode and BTCB2 beta with their actual scope and existing gates; beta controls never masquerade as network switches. Device registration/re-registration is a repair operation with explanation, not a casual About action. Logs are sanitized. Signed-in account state, stream status and Cube device registration remain distinct.

### Avatar, Members, Liquid and Marketplace boundaries

Avatar is a subordinate Cube identity page: show current avatar, service loading/error, available regeneration allowance and eligibility from the backend, and an explicit regenerate action; preserve current preview/commit behavior and quota rather than inventing a free unlimited generator. Its signed-in native content was blocked by the state mismatch in this pass.

Members stays within Wallet & signers for eligible Connect Vaults, with account identity and Vault context. Preserve invitation/role semantics and gates; live member management was not reviewed. Liquid currently has no Settings navigation entry; shared seed backup links to Cube protection and does not imply the source placeholder is reachable. Marketplace/P2P configuration and Duress were unavailable in the inspected navigation: retain source-backed gates and schedule live validation before detailed feature-specific implementation. These are deliberate evidence limits, not omitted known settings or approved new capabilities.

## Existing-to-proposed mapping

| Existing entry | Proposed destination/purpose |
|---|---|
| Cube → Settings → General | Display & appearance; recipient checks split to Payment checks; network stays context |
| Cube → Settings → Recovery | Backup & protection, with separately named Vault recovery-path route |
| Cube → Settings → Avatar | Display & appearance → Cube avatar |
| Cube → Settings → Stats | About → Download statistics |
| Cube → Settings → About | About & diagnostics; service/session details in Connections; repair actions under diagnostics |
| Cube → Settings → Members | Wallet & signers → Members, only when eligible |
| Vault → Settings → Wallet | Wallet & signers; descriptor/hardware controls disclosed as expert details |
| Vault → Settings → Pair | Pair with Keychain, directly reachable from Wallet & signers |
| Vault → Settings → Node | Connections; expert resource configuration; separate maintenance/rescan |
| Vault → Settings → Import/Export | Data & exports, grouped by purpose; Recovery Kit links to protection |
| Vault → Coins / PSBTs | Expert tools → Coin control / PSBT proposals; contextual shortcuts remain |
| Vault → Recovery | Explicit recovery-path spending route, distinct from backup readiness |
| Spark → Settings → General | Spark preferences; diagnostic detail separated |
| Spark → Settings → Lightning Address | Spark preferences → Lightning Address; contextual access from Receive can be evaluated later |
| Home → Connect → Overview/Contacts/Plan & Billing | Account & plan; Contacts as subordinate account task |
| Home → Connect → Security | Devices & security |
| Home → Global Settings | About/app preferences, clearly installation-wide |

## Validation before implementation acceptance

Re-run the same task matrix on the selected native design: display-unit change with feedback; incomplete backup; direct Recovery Kit destination; backup versus recovery-path distinction; pairing eligibility and key identity; wallet/key alias; sync/offline health; Spark restored preference; Lightning Address after Home sign-in; accounting export versus recovery material; account device review; compact-window orientation. Include keyboard and assistive technology. Do not mark source-only accessibility findings as conformance results.

Open decisions: Launcher presentation selection and integration; exact persistence/save semantics per preference; supported logical minimum window size; account state fix proven against a known binary; remaining gated/imported states. After selection, complete one brief confirmation round before Rust/Iced changes. No direction contract or DESIGN.md has been written.

## Preview verification

One batched browser inspection covered the three desktop structures and a compact sidebar state. A correction batch limited the Settings sections to their selected scope and made the app's main content scroll independently of the navigation/footer. The confirmation pass inspected all three final 1440×900 frames, the 1024×768 compact explorer, and light-theme device/security layout. Each of the eleven layout selectors rendered the expected heading. Signed-out account, no-Vault, Testnet/Spark unavailable and preference-save-error messages were exercised and visible; the prototype browser reported no console errors during that pass. These checks validate the review artifact, not production/native behavior, assistive-technology compliance or every proposed transition.

Review controls and app viewport are separate: the synthetic app occupies a bounded canvas beneath the comparison controls. Scroll the Settings content inside that canvas. A native implementation must size the entire app to its real window and validate its own minimum dimensions. Form edits are demonstration-only, not durable preference storage; full dirty-navigation and save transitions remain in the design requirements.

## Process evidence

Impeccable context had already run once in this ongoing session and was not rerun. Confirmed audience and stage scope were reused. A bounded design-only worktree holds a minimal PRODUCT.md with confirmed facts; the shared checkout is untouched. Source baseline 8495c3c0f233650b8642363c455ccd429b71bc47; shared policy b4e4fa3c3ecedad93f40d08f71baca7cea707a25. Surface seed 6d8ed46e, successful API retry, presented candidates 3/5/4 from seven task-grounded structures. This records process, not user approval. No image generation, external design upload or live wallet mutation was performed.

Internal candidate set, in resonance order: single task hub with wallet drill-in; wallet list with contextual drawer; top workspaces; one readable sidebar; persistent wallet workspace; task launcher with recent destinations; separate native Settings window. The presented hand compares three materially distinct structures within one visual identity.

## 2026-10-21 selection and scope correction

Robert explicitly chose Concept C and confirmed that the Launcher is separate from Cubes. He proposed a floor with floating/sitting Cube objects, create action, rows of Cubes, status cues, network symbols and an all-network catalog with optional filtering. These are exploration inputs, not approval of a particular 3D renderer, aura vocabulary or filter default. See launcher-design-brief.md and the revised Claude Design prompt. No running app or prototype UI was modified by this written scope revision.
