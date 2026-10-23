# Tenshu Launcher and Concept C Settings — design review

COINCUBE | Tenshu (native Rust/Iced desktop wallet). Written half of the design-review package. Companion to `tenshu-concepts.md` and `launcher-design-brief.md` in this folder.

## Status and scope

- Date: 2026-10-21.
- Robert selected **Concept C · One readable sidebar** for navigation inside an open Cube on 2026-10-21. This document describes Concept C as drawn; it does not reopen A/B.
- The **Launcher** (All Cubes) is a separate app level. All three Launcher presentations are explorations. The fixed-camera 3D floor in particular is exploratory and is not an approved visual or renderer.
- All data is synthetic: Cube names, wallet names, balances, policies, fingerprints (`a1b2c3d4` style), emails (`*.example.test`), devices, phones, contacts and billing rows. Placeholders such as `[PLAN NAME]`, `[RENEWAL DATE]`, `[AMOUNT]`, `[SERVER]`, `[HEIGHT]`, `[DATE]`, `[VERSION]` stand in for values that must come from source or backend. No price, date, ticker, logo or network parameter was invented.

**Design simulation versus product behavior.** The artboards are clickable design simulations. Their buttons move between drawn states, change local component state and follow links between artboards. They do not open a Cube, check a PIN or passkey, run network admission, reveal a seed, create a Recovery Kit, pair a phone, contact Connect, Branta, an Electrum server or the Spark bridge, export a file, sign out, remove a device or change any stored preference. A "Saved" message, a "Paired" row, a "Connected" chip or a completed loading step in an artboard is evidence of a proposed layout, never of a supported or completed native workflow. Where an artboard shows behavior the current app does not have (for example a Lightning Address claim flow inside Spark preferences, or a gate row for Duress), the behavior is a proposal subject to the open questions in this document.

**Canvas.** Published on a Claude Design canvas: https://claude.ai/artifact/4DkSWQshpyXVu2kVrbneRz (title "Tenshu Launcher & Concept C").

| Artboard | Size | What it shows | Review levers |
|---|---|---|---|
| Launcher-3D | 1440×900 | Fixed-camera CSS 3D floor; Cubes as beveled objects with text labels; filter bar; Create/Import dock; selected-Cube Open bar | `cubeCount` 0/1/4/20, `theme`, `reducedMotion`, `focusDemo`; in-artboard filter chips and chain menu |
| Launcher-Iso | 1440×900 | Same catalog as static SVG isometric glyphs on a grid floor, equal-size rows | Same as Launcher-3D |
| Launcher-Catalog | 1440×900 | Compact table: glyph, name, network, lock state, protection/capability, per-row Open; grouped by network when unfiltered | Same as Launcher-3D |
| Launcher-States | 1440×1100 | Static state sheet: object in three presentations, network badges (dark and light), lock/protection badges, filter chips, empty states, unlock dialog, long name, keyboard focus | `theme` |
| Journey | 1440×900 | Five-step flow Launcher → Unlock → Loading → Cube → All Cubes, with a review strip outside the app | `step`, `theme`, `reducedMotion`; in-artboard Back/Next |
| Cube-Workspace | 1440×900 | Concept C shell, Overview/Activity/wallet pages, Backup & protection, Expert, Settings with scope selector and section tabs, 11 sections | `page`, `reviewState` (10), `pairStep`, `lnState`, `alertsState`, `theme`, `reducedMotion` |
| Cube-Compact | 1024×768 | Concept C at compact width: 200px sidebar, scope and section as two select controls, five sections drawn | `page`, `reviewState` normal/signedOut/longNames, `theme`, `reducedMotion` |

## Launcher explorations

All three Launcher artboards share the same header (Tenshu mark, "All Cubes", account button "alex@example.test · Signed in", "App preferences"), the same filter bar, the same 20-Cube fixture list, the same Create Cube / Import or restore dock and the same safe-before-unlock metadata. They differ only in how a Cube is drawn and laid out.

Layout as coded:

- **3D floor**: up to 6 visible Cubes sit in one row of 88px objects with the label centred below. Above 6, rows of 5 with the label beside the object; back rows render smaller (about 44px rising to 56px at the front row) and are indented, so 20 Cubes form four receding rows. Selection lifts the object 10px, shrinks its shadow and outlines its faces in orange.
- **Isometric**: same row/column rule (one row up to 6, then 5 columns) but every glyph is the same size; no recession. Selection lifts 4px and strokes the glyph outline in orange.
- **Catalog**: one 56px row per Cube with a 40px glyph. Columns: Cube, Network, State, Protection and capability, Open. With "All networks" the list is grouped under network headings with counts; any other filter shows one flat list. Each row has its own Open link; the selected row's Open is primary.

| Criterion | 3D floor | 2.5D isometric | Compact catalog |
|---|---|---|---|
| Findability at 1 | Strong: one large object, centred, label below | Strong | Adequate; a single row looks sparse but is unambiguous |
| Findability at 4 | Strong: one readable row | Strong | Strong; grouping by network adds orientation |
| Findability at 20 | Weak: four receding rows, smaller back objects, labels beside; scanning is spatial, not alphabetical | Moderate: uniform 5-column grid, all labels same size | Strongest: one column of names, network headings, counts; scrolls predictably |
| Status legibility | Text lines below each object (lock, protection, capability); object itself carries only selection | Same text lines | Same text in aligned columns; easiest to compare across Cubes |
| Long names | Two lines then ellipsis; full name in `title` | Same | Same; wider name column than the objects' label width |
| Mixed chains / test networks | ₿ mark + "Blake2b" text badge + test badge ("Testnet4", "Signet", "Regtest") + full network name in every label | Same | Same, plus network group headings ("Bitcoin mainnet (n)", "Testnet4 (n)") |
| Keyboard / focus | Each Cube is a `<button>` in catalog order; focus lifts the object and draws a 2px ring; Open is a separate control in the bottom bar | Same | Name is the selection button; Open is a per-row link; two tab stops per row |
| Reduced motion | Only the 0.18s lift transition exists; reduced motion removes it. No continuous motion drawn | Only a 0.12s lift; removed | No motion to remove |
| Light / dark | Both drawn via `theme`; floor and faces re-tinted | Both drawn | Both drawn |
| Modest hardware | Needs a GPU scene in production; highest risk | Static vector/raster; low risk | Ordinary widgets; lowest risk |
| Native rendering cost | Iced shader widget + WGSL + depth path; unbenchmarked | Iced canvas, SVG or pre-rendered images | Existing Iced layout widgets only |
| Visual appeal | Highest; matches Robert's floor proposal | Good; reads as the same object family | Plain; functional |

**States covered.** Levers and the state sheet cover:

| State | 3D floor | Isometric | Catalog | State sheet / Journey |
|---|---|---|---|---|
| 0 Cubes | "No Cubes yet." on empty floor, dashed cube outline, "You choose the network when you create it." | Same on grid floor | Same in list area | (e) "0 Cubes" mini |
| 1 / 4 / 20 Cubes | One row / one row / four receding rows | One row / one row / 5-column grid | 1 / 4 / 20 rows in network groups | — |
| Locked / open | Lock icon + "Locked" or "Open"; only Everyday is open | Same | State column | (c) badges; Journey shows "Open · stays open" |
| Backup confirmed | Check icon, good tone | Same | Same | (c) |
| Backup not confirmed | Triangle icon, warn tone (Blake workspace, Blake test, Donations) | Same | Same | (c), (a) |
| Backup status unknown | Question-in-circle, muted (Lab, Signet scratch, Regtest bench, Side project, Lab 2, Lab 3) | Same | Same | (c) |
| Backup stale | Clock-in-circle, warn tone (Travel fund) | Same | Same | (c) |
| No Vault | Neutral caption "No Vault configured · Spark only" (Archive, Donations, Shop till) | Same | Same | (c) "neutral" |
| Imported Vault | "Imported Vault · no local keys", neutral tone; caption "Watch-only Vault" | Same | Same | (c) |
| No matches vs no Cubes | "No Cubes match these filters." + "n hidden." + Clear filters; distinct from "No Cubes yet." | Same | Same | (e) side by side |
| Focus | `focusDemo` rings the second Cube and shows "Press Enter to open" | Same | Same on second row | (h) |
| Light / dark | `theme` | `theme` | `theme` | (b) badges in both themes |
| Reduced motion | `reducedMotion` removes the lift transition | Same | n/a | Journey `reducedMotion` |

Not covered by any Launcher artboard: a text search or type-ahead, 100 Cubes, an open-Cube tab strip in the Launcher header (drawn only in Journey), a GPU-unavailable fallback, and a resized or minimum window.

## Mixed-network catalog and filter behavior

Rules as drawn, and as proposed for implementation:

1. **Default is All networks.** Every local Cube on every supported network is listed. In the catalog presentation this view is grouped by network.
2. **Live chains** shows Bitcoin mainnet and Bitcoin Blake2b mainnet. Both are live chains; Blake2b is not a test network.
3. **Test networks** shows Testnet4, Signet, Regtest and Bitcoin Blake2b Testnet4.
4. **Chain** opens a menu of specific chains (Bitcoin, Bitcoin Blake2b, Testnet4, Signet, Regtest, Blake2b Testnet4) with a per-chain count. In the artboards the four filter options are mutually exclusive (one active at a time); multi-select is not drawn.
5. **Remembered.** The filter bar states "Filters are remembered on this device". The artboards do not persist anything; persistence scope (this device, per installation) is an open question.
6. **Active-filter indication.** The active chip shows a check mark and `aria-pressed="true"`; the Chain chip relabels to "Chain: Testnet4" etc.
7. **Hidden count.** "Showing 3 of 4 · 1 hidden by filters"; with no filter, "Showing n Cubes". The count is an `aria-live="polite"` region.
8. **Clear filters** appears only while a filter is active, both in the bar and inside the no-match message.
9. **No Cubes versus no matches** are different messages. Create Cube and Import stay visible in both.
10. The chain menu repeats the rule in place: "Filters only change what is shown. Each Cube keeps its own network."

Filters never:

- change a Cube's chain or network;
- combine or total balances across chains (the Launcher shows no balances at all);
- reuse a backend session, signing credential or Connect session across chains;
- bypass Bitcoin Blake2b admission or the BTCB2 beta gate;
- choose the chain for a new Cube.

**Creation selects its chain explicitly** ("Creation asks for the network."). **Import or restore verifies the file and network first** ("Import checks the file and network first."). Neither is drawn beyond the dock copy.

**Validation required.** A single global catalog that lists Cubes from every network before unlock assumes the app can read name, network, lock state and a protection/capability summary for all Cubes without unlocking any of them. That must be validated against source and protected storage before implementation: where each field is stored, whether it is readable pre-unlock, whether reading it on another chain's data directory has side effects, and whether Blake2b Cubes may be listed when the BTCB2 beta preference is off (current source refuses to open them: "Enable Bitcoin Blake2b - Beta in Global Settings to open this Cube", `coincube-gui/src/chain.rs`).

## Launcher → unlock → Cube → All Cubes journey

Journey.dc.html, five steps driven by the `step` lever or the Back/Next review strip (outside the app frame):

| Step | What is drawn | Simulated |
|---|---|---|
| 1 Launcher | Header with "All Cubes" tab plus an open "Lab" tab; catalog list (Everyday locked, Blake workspace locked, Lab open, Archive locked); "Import or restore", "Create Cube…"; note "Only what is safe before unlock … No balances." | Open on Everyday advances to step 2; other rows do nothing |
| 2 Unlock | Modal over the catalog: "Open Everyday · ₿ Bitcoin mainnet", masked PIN field, Unlock, Use passkey, Cancel; "Network admission checks run before the Cube opens." | Unlock/Use passkey advance; no PIN is checked |
| 3 Loading | Cube header with Everyday and network; "Opening Everyday"; ordered list: PIN accepted · Done, Bitcoin mainnet checks (Network admission for this Cube) · In progress, Loading Vault, Spark, Liquid · Waiting; Cancel opening; "Each step finishes before the next starts. Nothing here is skipped or shortened for display." | Static; nothing progresses on its own |
| 4 Cube | Concept C shell: Cube switcher, "All Cubes" link, Account and Cube connection chips, sidebar, Overview with three wallets and synthetic per-wallet balances | Switcher and All Cubes both advance to step 5 |
| 5 All Cubes | Header tabs: All Cubes, Everyday, Lab; Everyday row "Open · stays open" with "Switch to"; note "Returning to All Cubes never locks or closes open Cubes. Everyday and Lab stay open as tabs." | Switch to returns to step 4 |

The state sheet (f) draws the same unlock as a dialog with three ordered steps ("PIN accepted", "Checking Bitcoin mainnet admission… Network checks · step 2 of 3", "Loading wallets").

Guarantees the design requires:

- Opening uses the existing PIN/passkey flow and the network-specific loading and admission flow. Animation (object lift, transitions) never delays, shortens, reorders or fakes a security check, and a step is never shown as done before it is done.
- Cancel is available while unlocking and while loading.
- Returning to All Cubes (switcher menu item "All Cubes · Open Cubes stay open", or the header link) keeps every open Cube tab open and unlocked. "Switch to" on an open Cube changes tabs without re-unlocking.
- The Launcher shows no balances; balances appear only inside an opened Cube, per wallet and never combined across networks.

## Concept C inside an open Cube

**Shell** (Cube-Workspace, Cube-Compact):

- **Header**: Tenshu mark · Cube switcher button (Cube name + ₿ network badge, test badge when applicable; menu lists open Cube tabs with Current/Open and a final "All Cubes · Open Cubes stay open" link) · right side, two separately labelled chips: **Account** ("alex@example.test · Signed in", "Signed out" + Sign in, "Checking…") and **Cube connection** ("Connected", "Reconnect required", "Not checked while signed out", "Checking…").
- **One sidebar** (232px; 200px compact): identity block (Cube, name, network) → Overview, Activity, Backup & protection → group "Wallets" (Spark · Daily spending, Vault · Family savings, Liquid · Travel, each with a sub-line such as "Stable Balance · review", "2-of-3 · synthetic", "Liquid network") → **Expert** disclosure button ("5 tools", `aria-expanded`, collapsed by default): Coin control, PSBT proposals, Descriptors & keys, Node resources, Maintenance.
- Only the middle section (Wallets + Expert) scrolls (`.mid{overflow:auto}`); identity and primary nav stay at the top, attention row and foot stay at the bottom.
- **Attention row** above the foot: one compact button, e.g. "Backup not confirmed · Review"; at most one item plus "+1 more".
- **Anchored foot**: Settings; Connect account (with email or "Signed out").

**Settings model**:

- Heading names the scope: "Settings · Cube · Everyday", with a scope line ("Applies to this Cube on Bitcoin mainnet. Rows marked This device stay on this computer.").
- **Scope selector** (toggle buttons, `aria-pressed`): Cube · Everyday / This device / Connect account / Vault · Family savings / Spark · Daily spending.
- **Section tabs** for that scope only, as a nav of buttons with `aria-current`:
  - Cube: Display & appearance, Backup & protection, Payment checks, Data & exports
  - This device: About & app preferences, Spark
  - Connect account: Account & plan, Devices & security, Connections
  - Vault: Wallet & signers, Pair with Keychain, Connections, Data & exports
  - Spark: Spark, Connections
- At 1024×768 the scope selector and section tabs become two labelled `<select>` controls ("Scope", "Section").
- No tertiary rail. The sidebar's Settings opens Cube scope; Connect account opens Connect account scope.

**How the six live problems are addressed:**

1. **Three equal icon rails; Vault Settings below the fold; sidebar scroll moved primary nav.** One text sidebar replaces three rails; Settings and Connect account are anchored at the foot and only the Wallets/Expert block scrolls. Demonstrated in Cube-Workspace (any page) and Cube-Compact at 1024×768 with `reviewState=longNames`, where long Cube and Vault names wrap without pushing the foot off screen.
2. **Recovery Kit shortcut landed on General.** Every Recovery Kit link ("Open Backup & protection › Recovery Kit") goes to Cube › Backup & protection, where the Recovery Kit row lives. Demonstrated in Cube-Workspace, Settings › Cube › Data & exports and Settings › Vault › Data & exports. The prototype lands at the top of the tab rather than scrolling to the row (`id="recovery-kit"` exists).
3. **Backup and recovery-path spending both called "Recovery".** Backup is "Backup & protection"; spending is "Recovery path", introduced as "Spending, not backup … A backup never unlocks it." Demonstrated in Cube-Workspace `page=backup` (section "Vault recovery path") and `page=wallet-vault` (section "Recovery path").
4. **Wallet Settings led with a raw descriptor; node health mixed with logs, paths, tuning and rescan.** Wallet & signers leads with identity, policy and signer aliases; descriptor, key details and hardware registration sit under "Expert · descriptor, key details, hardware registration". Connections leads with three health rows; resources, logs and paths are disclosed; Maintenance (rescan) is a separate consequential section. Demonstrated in Cube-Workspace `reviewState=longNames` (Vault › Wallet & signers) and `reviewState=offlineNode` (Vault › Connections).
5. **Two large warnings above unrelated screens.** One compact attention row in the sidebar; full detail only on the relevant page (Backup & protection lead block; Vault page "Node offline" banner; Spark page Stable Balance notice). Demonstrated in Cube-Workspace `reviewState=normal`, `offlineNode`, `cubeDisconnected`, `testnetSparkUnavailable`.
6. **Billing led with sales comparison; device lists spread across wide rows.** Account & plan leads with identity, current plan and "Billing status: verified"; Compare plans is a secondary button; referral is a one-line link. Devices are compact rows (name, platform, last activity, action) with an inline removal review. Demonstrated in Cube-Workspace Settings › Connect account › Account & plan and › Devices & security.

## Detailed Settings layouts

Each section below is described as drawn in Cube-Workspace unless noted. "Preserved safeguards" lists what an implementation must keep from current flows.

**1. Display & appearance** (scope Cube)
- Order: read-only network line → Amounts & currency (Bitcoin unit BTC/Sats, Primary amount Bitcoin/Fiat, Fiat estimates, Currency, Exchange-rate source, Direction badges) → Cube identity (Cube name, Avatar) → Application appearance (Theme pointer to This device).
- Every row carries a storage label ("This Cube", "This device"); "Grouping on one screen does not mean one storage scope."
- States: clean, dirty ("Unsaved changes"), saved, save failure (`reviewState=saveFailure`: alert names the two unsaved fields, edits kept, "Try again"), loading skeleton, avatar loading/error/ready.
- Unavailable currency stays listed but disabled ("[CURRENCY] · unavailable on this chain").
- Safeguards: no new rename API; Avatar shows "Regenerations remaining: [N from service]" and preview-before-confirm.

**2. Backup & protection** (scope Cube; also the sidebar page)
- Order: readiness lead (status, title, detail, one next action "Confirm seed phrase backup") → Backup methods (Seed phrase; Connect Recovery Kit with methods and requirements, "Descriptor in kit") → Vault recovery path (Recovery path summary, View recovery path; Recovery alerts).
- States: not confirmed (normal), unknown (`testnetSparkUnavailable`), Recovery Kit sign-in required / reconnect required / not applicable (no Vault); alerts verified / unknown / signed out / denied / not applicable.
- Safeguards: seed reveal and confirmation stay in the authenticated flows ("Opens the authenticated reveal flow"); Recovery Kit setup disabled with a reason when signed out or disconnected; "No plan change is implied while this is unknown."

**3. Wallet & signers** (scope Vault)
- Order: imported notice (when applicable) → Identity (Vault name, Network fixed, Wallet ID `[CHECKSUM]-[TIMESTAMP]`) → Policy (normal spending, recovery path) → Signers (alias inputs, fingerprint, kind, where it signs; Save names/Discard) → Phone signing and members (Pair with Keychain link; Members gate) → Expert disclosure (masked descriptor, key details table, hardware registration).
- States: normal, imported Vault (external signers, no local key), long names, no Vault ("Vault settings don't apply").
- Members: "Not enabled · Available for eligible Connect Vaults · No member controls until this Vault is eligible."
- Safeguards: descriptor copy "Shows a privacy warning before copying"; encrypted export asks for password and location; hardware registration opens its existing flow.

**4. Pair with Keychain** (scope Vault)
- Order: key the phone will hold (radio list with eligibility reasons) → How pairing works ("Same Wi-Fi · no Connect account needed"; remote signing is separate) → Pair a phone → Paired phones.
- States via `pairStep`: idle, picker ("Looking for phones on this Wi-Fi…"), waiting (QR, `[VERIFICATION CODE]`, expiry countdown, Cancel), error ("Nothing was saved and no key left this computer."), paired (row with Unpair…). Imported Vault: disabled with "no Keychain-eligible key in this Vault".
- Safeguards: cryptographic handshake confirmed on both screens; Unpair opens a review; no Connect sign-in required for the local flow.

**5. Payment checks** (scope Cube)
- Order: what is checked and when → "Check supported payment requests" (on; turning off opens the existing confirmation) → Strict mode with privacy text and link.
- Branta strict-mode claim, verbatim as required by `tenshu-concepts.md`: "plaintext destination and amount are not shared by strict mode, but lookup timing and network connection may be observable." Artboard copy: "Strict mode does not share the plaintext destination or payment amount. Branta can still observe lookup timing and your network connection."
- Footer: "A check never guarantees who the recipient is."
- Storage label reads "Storage per current preference" (unvalidated).

**6. Connections** (scopes Connect account, Vault, Spark)
- Order: Health (Account authentication; This Cube's service session; Node backend & sync or Spark bridge) → scope-specific detail.
- Vault: Node backend ("Switching backends never happens automatically", Change backend…) → Expert disclosure (resource preset, sanitized logs, paths and versions) → Maintenance (Rescan: requires, consequences, "never repairs sign-in or Cube connection problems"; disabled while offline).
- Spark: Bridge diagnostics disclosure (process, JSON-RPC over stdio, last error, copy sanitized diagnostics).
- States: signed out, reconnect required, offline node, checking, no Vault.
- Safeguards: pruning irreversible and restart required; Reconnect "never rescans or switches backends".

**7. Spark** (scope Spark; pointer under This device)
- Order: Stable Balance (restored-device notice, "On this device · Off", Review Stable Balance) → Lightning Address (requires: account state · this Cube's service state; then address/claim) → Bridge health (pointer to Connections).
- **Stable Balance restored-device mismatch**: "This wallet holds USDB but Stable Balance is off on this device · Review … nothing converts until you review. No peg is guaranteed." Also shown on the Spark wallet page and as the sidebar sub-line "Stable Balance · review".
- Lightning Address states via `lnState`: existing, claim (format hint), checking, signed out, disconnected, error.
- Testnet4: "Spark isn't available on Testnet4 … supported on Bitcoin mainnet and Regtest only."
- Safeguards: Stable Balance changes only through the focused review flow; never auto-enabled.

**8. Data & exports** (scopes Cube and Vault)
- Cube: Protection material (Recovery Kit → Backup & protection) → pointer to Vault files and records (disabled with reason when no Vault).
- Vault: Protection material (Recovery Kit link; Encrypted descriptor) → Wallet portability (Import/export wallet file; Plaintext descriptor) → Records (Labels BIP329; Transaction table CSV). Each group opens with a privacy sentence.
- "Records don't replace recovery material."
- Safeguards: native file dialogs with validation; plaintext descriptor "Shows a privacy warning first"; imports "validated against this network before anything changes".

**9. Account & plan** (scope Connect account)
- Order: Identity (email, Signed in, Change email…) → Plan and billing (`[PLAN NAME]`, "Billing status: verified · paid upfront through [RENEWAL DATE]", Compare plans) → Billing history table (placeholders) → Referral line → Contacts (synthetic Jordan, Sam; roles; Add contact) → Sign out….
- States: signed in; signed out ("Your Cubes stay usable; Connect services such as Recovery Kit and Lightning Address pause.").
- Safeguards: no invented prices or dates; upfront-payment semantics kept; sign out opens a confirmation; "Open Cubes stay open".

**10. Devices & security** (scope Connect account)
- Order: Devices (This Mac (current), Windows laptop, Linux desktop: name, platform, last activity, Remove) → Recent logins (Successful/Failed in text, `[DATE] [TIME] [TZ]`, View all) → Duress gate.
- Device removal review: "Removing signs this device out of your Connect account on next contact; what else it revokes is subject to source validation."
- Duress: "Not available · Not available for this account. No controls until the feature is enabled."
- Signed out: "Sign in to review devices".

**11. About & app preferences** (scope This device)
- Order: About (Version with build and "signed release, provenance verified"; Download Stats) → App preferences, installation-wide (Theme Dark/Light "Applies immediately"; Developer mode; BTCB2 beta "Installation-wide · account-gated", disabled when signed out) → Diagnostics and repair disclosure (component versions, sanitized logs, Repair device registration…).
- BTCB2 copy: "It is not a network switch and never changes an existing Cube's network."
- Safeguards: repair opens a review before contacting the service; logs sanitized.

**Gates summary.** Members (Wallet & signers) and Duress (Devices & security) are drawn as gate rows with no controls. Liquid has no Settings entry; the Liquid page points to Backup & protection. Marketplace/P2P is not drawn in any artboard. BTCB2 beta is drawn as an installation preference that also depends on account access.

## Current-to-proposed route map

"Verified" means the label or route was confirmed in `coincube/coincube-gui/src` at baseline `8495c3c0f233650b8642363c455ccd429b71bc47`.

| Current entry | Status | Proposed destination | Purpose / notes |
|---|---|---|---|
| Cube rail › Settings (lands on General) | Verified, `app/view/nav/cube.rs` | Sidebar foot › Settings → Cube scope | Workspace lands on Display & appearance; Compact lands on Backup & protection (inconsistency, see departures) |
| Cube › Settings › General | Verified, `app/view/nav/tertiary.rs` | Cube › Display & appearance | Recipient checks move to Payment checks; network becomes read-only context |
| Cube › Settings › Recovery | Verified | Cube › Backup & protection, plus sidebar page Backup & protection | **Naming split**: backup = "Backup & protection" |
| Cube › Settings › Avatar | Verified | Cube › Display & appearance › Cube identity › Avatar | Quota and preview kept |
| Cube › Settings › Stats | Verified | This device › About & app preferences › Download Stats | Product reference, not beside protection |
| Cube › Settings › About | Verified | This device › About & app preferences | Service/session detail moves to Connections; repair under diagnostics |
| Cube › Settings › Members | Verified, behind `CUBE_MEMBERS_UI_ENABLED` | Vault › Wallet & signers › Members (gate row) | Current source hides the item when the flag is off |
| Vault rail › Settings (lands on Node) | Verified, `app/view/nav/vault.rs` | Vault scope › Wallet & signers first | Identity before infrastructure |
| Vault › Settings › Node | Verified | Vault › Connections (+ Expert disclosure, Maintenance) | Health first |
| Vault › Settings › Wallet | Verified | Vault › Wallet & signers | Descriptor under Expert disclosure |
| Vault › Settings › Import/Export | Verified | Vault › Data & exports; Cube › Data & exports pointer | Grouped by purpose |
| Vault › Settings › Import/Export › "Connect Recovery Kit" | Verified: routes to `CubeSettingsOption::General` (`app/view/vault/settings/mod.rs`, `import_export`) | Cube › Backup & protection › Recovery Kit | **Recovery Kit shortcut fix**; the card lives in the Recovery section (`SettingsSection::Recovery`, `app/state/settings/general.rs`) |
| Vault › Settings › Pair | Verified, hidden for imported Vaults | Vault › Pair with Keychain; link from Wallet & signers | Design shows a disabled reason for imported Vaults instead of hiding |
| Vault rail › Recovery | Verified, hidden for imported Vaults | Vault wallet page › Recovery path; Backup & protection › Vault recovery path | **Naming split**: spending = "Recovery path" |
| Vault rail › Coins | Verified | Expert › Coin control | |
| Vault rail › PSBTs | Verified | Expert › PSBT proposals | |
| Vault rail › Send / Receive / Transactions / Overview | Verified | Wallet page actions; Activity | Not redesigned in detail |
| Vault rail › Claim BTCB2 / Resume Claim | Verified, grant-gated | Not drawn | Open question 15 |
| Spark › Settings › General | Verified | Spark › Spark (Stable Balance, bridge health); This device › Spark pointer | Diagnostics to Spark › Connections |
| Spark › Settings › Lightning Address | Verified | Spark › Spark › Lightning Address; read-only row on Spark wallet page | |
| Liquid › Settings | Verified absent (`app/view/nav/liquid.rs`) | None; Liquid page links to Backup & protection | |
| Home › Cubes | Verified, `HomeSection::Cubes` | Launcher (All Cubes) | |
| Home › Connect › Overview | Verified, `home.rs` | Connect account › Account & plan › Identity | |
| Home › Connect › Contacts | Verified | Connect account › Account & plan › Contacts | |
| Home › Connect › Plan & Billing | Verified | Connect account › Account & plan › Plan and billing | Identity and verified status first |
| Home › Connect › Security | Verified | Connect account › Devices & security | |
| Home › Connect › Duress | Verified, shown only when `show_duress()` | Devices & security › Duress gate row | Current source hides rather than greys; see departures |
| Home › Settings ("Global Settings") | Verified, `global_settings_view` | Launcher › App preferences = This device › About & app preferences | Current rows: "Developer mode", "Bitcoin Blake2b - Beta"; "Changes are saved automatically." |
| Sidebar dark/light toggle | Verified, `app/view/nav/mod.rs` | This device › App preferences › Theme | Immediate |
| Home › Recover a Vault | Verified, `HomeSection::RecoverVault` | Not drawn | Launcher entry needed; open question 16 |
| Home › non-Cube wallet scan | Section exists; label unverified | Not drawn | Open question 16 |
| Marketplace › P2P › Settings | Verified tab in P2P panel | Not drawn; stays in Marketplace, gated | |

## Component and state notes

Iced mapping names Iced 0.14 widgets (`button`, `text`, `row`/`column`, `container`, `scrollable`, `pick_list`, `toggler`, `text_input`, `radio`, `tooltip`, `rule`, `svg`, `image`, `canvas`, `shader`) and existing `coincube_ui` components where observed (`button::menu` / `menu_active`, `card::simple`, `text::h4_bold`). Screen-reader exposure in Iced 0.14 is unverified (open question 13); "accessible name" below states the required text, not a proven capability.

| Component | Variants | States | Text-first rule | Focus | Iced mapping |
|---|---|---|---|---|---|
| Network badge | Bitcoin mainnet; Blake2b mainnet (₿ + "Blake2b"); test (₿ + dashed badge "Testnet4"/"Signet"/"Regtest"); Blake2b Testnet4 (both badges) | — | Full network name always present; ₿ is decorative | Not focusable | `row[svg/text ₿, container(text) badge, text]`; custom container style for dashed border |
| Lock badge | Open, Locked | — | Word + lock icon shape (open shackle differs) | Not focusable | `row[svg, text]` |
| Protection / capability badge | Backup confirmed, not confirmed, unknown, stale, imported (no local keys); capability caption | Tone good / warn / muted / neutral | Word carries meaning; icon shape differs per state; tones differ in lightness | Not focusable | `row[svg, text]` with text style per tone; caption `text` secondary |
| Attention row | Backup not confirmed; checking; reconnect required (+1 more); node offline (+1 more); unknown | One at a time | Sentence + "Review" | Single button, in sidebar tab order before Settings | `button(row[svg, text, text])`; priority picked in state |
| Scope chip pair | Account (signed in / signed out + Sign in / checking); Cube connection (connected / reconnect required / not checked / checking) | — | Each chip has a "k" label ("Account", "Cube connection") | Only Sign in focusable | `row[container(row[text label, text state]), button?]` |
| Section row | Label + storage tag / description / control (segmented, toggle, select, input, link, button + hint) | Normal, disabled with reason, error | Description and hint are text, never tooltip-only | Control is the stop; label linked to control | 3-column `row` with fixed control width; `pick_list`, `toggler`, `text_input`, segmented = `row` of `button`s with selected style |
| Disclosure | Expert (sidebar), Expert in Wallet & signers, node resources, bridge diagnostics, diagnostics and repair | Collapsed (default), expanded | Label names contents; "Show"/"Hide" text | Button with expanded state; content follows in order | `button` + conditional `column`; state in panel |
| Save / Discard group | Display form; signer aliases | Clean (both disabled), dirty, saving, saved, failed ("Not saved · edits kept", "Try again") | Status sentence left of buttons | Focus stays on group after save/failure | `row[text, button, button]`; edits held in state until success |
| Compact section selector | Scope select + Section select (1024) | Per scope options | Visible labels "Scope", "Section" | Two selects in order | `pick_list` ×2 |
| Settings tabs | Section buttons per scope (1440) | Current (`aria-current`) | Text labels; wrap when long | Tab order across buttons | `row` of `button`s with active style (not a new tab widget) |
| Cube object | 3D floor (88px / 44–56px), isometric glyph, catalog 40px glyph | Default, selected (lift, orange outline), focused (ring + lift) | Label block outside the object carries all meaning | The label+object button is one stop; Open is a separate stop | 3D: one `shader` widget for the scene with ordinary `button`s positioned for labels; Iso: `svg` or `canvas`; catalog: `svg` in a `row` |
| Filter chip | All networks, Live chains, Test networks, Chain (menu with counts) | Off, on (check + pressed), menu open | Chip text; Chain relabels to "Chain: X" | Each chip a stop; menu items in order | `button` with toggle style; menu via overlay/`pick_list`; count `text` |
| Empty state | No Cubes yet; no matches (+ Clear filters); no Vault; Spark unavailable | — | Heading + sentence + next action | Action button focusable | `column[text, text, button]` |

## Five findability walkthroughs

**1. Missing backup**
- Start: Cube-Workspace, `page=overview`, `reviewState=normal`.
- Steps: see attention row "Backup not confirmed · Review" above the sidebar foot → click it (or Tab to it, Enter) → Backup & protection page: "Backup not confirmed", "Confirm your seed phrase", detail, "Confirm seed phrase backup" → Backup methods show Seed phrase "Not confirmed" and Recovery Kit "Not set up".
- Alternate: Settings (foot) → Scope Cube → tab Backup & protection, same content.
- Variations: `reviewState=testnetSparkUnavailable` shows "Backup status unknown"; Launcher-Catalog shows Blake workspace "Backup not confirmed" before unlock.
- Simulated: buttons perform nothing; the authenticated confirmation flow is not drawn.

**2. Pairing a phone**
- Start: Cube-Workspace, `page=settings`, `reviewState=normal`.
- Steps: Scope "Vault · Family savings" → tab "Wallet & signers" → "Open Pair with Keychain" (or tab "Pair with Keychain" directly) → eligible key "Keychain · Alex's phone e5f6a7b8" preselected; other keys show why not eligible → "Pair phone" → picker: "Alex's iPhone", "Pixel test phone" → "Pair with this phone" → QR with "[VERIFICATION CODE]", expiry → Simulate approval → Paired phones row with Unpair….
- Lever: `pairStep` idle/picker/waiting/error/paired; `reviewState=importedVault` lands here with Pair disabled and reason.
- Simulated: discovery, QR, handshake and countdown are static; "Simulate failure/approval" are review buttons, not product controls.

**3. Node health**
- Start: Cube-Workspace, `reviewState=offlineNode`, `page=overview`.
- Steps: sidebar Vault sub-line reads "Node offline"; attention row "Vault node offline +1 more · Review" → click → Settings › Vault › Connections: Health rows (Account signed in; Cube session connected; Node backend "Offline … Balances may be out of date." + Retry connection) → Node backend (Change backend…) → Expert disclosure (resources, logs, paths) → Maintenance (Review rescan… disabled, "Requires a connected node").
- Also: Vault wallet page banner "Node offline … Open Connections".
- Simulated: Retry does nothing; "+1 more" is not expandable.

**4. Lightning Address after sign-in**
- Start: Cube-Workspace, `reviewState=signedOut` (lands on Settings › Connect account › Account & plan "Signed out of Connect").
- Steps: header Account chip "Signed out" with Sign in → Scope "Spark · Daily spending" → tab Spark → Lightning Address: "Requires · Account · signed out · This Cube's service · not checked"; "Sign in to Connect to claim or manage a Lightning Address." → (reviewer switches `reviewState=normal`) → "Your address alex@ln.example.test" (synthetic), Copy, Change….
- Lever: `lnState=claim` shows the claim input and format hint; Check availability → checking; `lnState=error`; `reviewState=cubeDisconnected` forces the "reconnect" state.
- Also: Spark wallet page shows the address row; Cube-Compact `page=settings` → Scope Spark.
- Simulated: sign-in is a lever change; no availability check runs.

**5. Accounting export**
- Start: Cube-Workspace, `page=settings`, `reviewState=normal`.
- Steps: Scope Cube → tab Data & exports → "Wallet portability and records" → "Open Vault › Data & exports" → Records → Transaction table "CSV for accounting. Fiat values use this Cube's rate source." → Export transactions.
- Shorter path: Scope Vault → tab Data & exports.
- Distinction drawn: Protection material sits first and links to Backup & protection; "Records don't replace recovery material."
- Simulated: no file dialog, no file.

## Launcher recommendation

**Production candidate: compact catalog.** It wins on findability at 20+, status comparison, long names, keyboard order, screen-reader naming (the row label already concatenates name, network, state, protection and capability), modest hardware and implementation cost, and it uses existing Iced layout widgets. Add a name search or type-ahead before production; none of the three artboards has one.

**Always-available companion: the isometric presentation**, as a selectable "floor" view over the same data and filters, at the same object identity as the catalog glyph. It delivers most of the spatial floor Robert proposed, renders as static vector or pre-rendered assets, has no camera, and degrades to the catalog at high counts.

**3D floor: keep exploring, do not commit.** It is the most appealing at 1–6 Cubes and the weakest at 20 (receding rows, smaller back labels). If pursued, it should replace the isometric view only after a native spike proves cost and accessibility, and it should switch to the catalog automatically above a count threshold to be decided.

Tradeoffs: the catalog is plain and loses the "place" feeling; the isometric view doubles layout states to maintain; the 3D floor adds a GPU dependency and a render path that Iced does not provide for 3D depth out of the box.

**Rendering implications** (per `launcher-design-brief.md`):
- 3D: a small scene inside Iced 0.14's `shader` widget, reusing Iced's wgpu device/queue and render lifecycle; one shared beveled-cube mesh drawn as instances, floor plane, fixed orthographic or shallow-perspective camera, restrained lighting, inexpensive contact shadow, limited selection/open animation; WGSL with the shader `Primitive`/`Pipeline` interfaces; transforms via a crate such as glam; match Iced's re-exported wgpu types (Cargo.lock pins wgpu 27.0.1). Depth/compositing needs a custom pass or offscreen target; do not assume the shared UI pass has a depth attachment. Suspend redraw when idle or hidden.
- Labels, badges, filters, focus rings, Create/Import/Open stay ordinary Iced controls over or beside the scene. The scene consumes a sanitized presentation model only, never wallet services or credentials.
- Static, pre-rendered or vector assets: isometric glyphs, catalog glyphs, floor grid, empty-state outline, network/lock/protection icons. Real-time only for the 3D object lighting and selection lift.
- Reduced motion keeps the same scene static (no lift transition); it does not require a different renderer.
- No game engine. No webview.

**Accessibility implications:** text carries every state; focus order equals catalog order; each Cube's accessible name includes name, network, lock state, protection and capability; Open is a separate, named control ("Open Everyday"); the compact catalog is the fallback for high counts and assistive technology; no hover-only information (the long-name `title` tooltip must have a non-hover equivalent).

**A native rendering and performance spike is still required** before any spatial presentation is accepted: startup, resize/high DPI, minimized/obscured behavior, 1/20/100 objects, reduced motion, GPU-unavailable fallback and keyboard interaction. No benchmark or native 3D spike has been run.

## Deliberate departures from the supplied references

1. **Avatar under Display & appearance › Cube identity**, not a separate Settings entry. Reason: avatar is Cube identity alongside the name.
2. **Everyday backup state differs between artboards.** Launcher artboards show Everyday "Backup confirmed" (brief fixture); Cube-Workspace and Cube-Compact `normal` show "Backup not confirmed" in the attention row and Backup page. Reason: the Workspace needed an incomplete-backup normal state to demonstrate problem 5. Reviewers should treat these as two scenarios, not one Cube.
3. **Two status lines on Cube labels** (lock state; protection) plus a capability caption, rather than one combined line. Reason: lock and protection are independent and each needs its own icon and word.
4. **Added `alertsState` lever** (auto/verified/unknown/signedOut/denied) beyond the brief's review states. Reason: Recovery alerts entitlement must show unknown distinct from denied.
5. **Settings tabs are nav buttons with `aria-current`**, not an ARIA tablist. Reason: each section is a destination that can be linked to (e.g. the Recovery Kit fix).
6. **Lightning Address synthetic value** `alex@ln.example.test`, and claim format hint "3–32 characters: lowercase letters, numbers, dots, dashes." Both synthetic; real rules are an open question.
7. **Journey fixture changes**: Everyday starts Locked (Launcher artboards: Open) and Lab is Open (fixture: Locked). Reason: show an unlock and an already-open tab in one flow.
8. **Lab's network label** is "Bitcoin" + Testnet4 badge in Journey and the Cube header ("Bitcoin · Testnet4"), versus "Bitcoin Testnet4" in Launcher labels. Needs one convention.
9. **Extra fixture states** chosen by the builders: Travel fund "Backup stale", Donations "Backup not confirmed" with "No Vault configured · Spark only", Imported Vault "Watch-only Vault", per-Cube capability captions such as "Vault + Spark".
10. **Spark appears under This device** as a pointer tab for the Stable Balance device preference, in addition to Spark scope.
11. **Connections appears in three scopes** (Connect account, Vault, Spark) with a shared Health block.
12. **Payment checks and Data & exports placed in Cube scope** (Data & exports also in Vault scope). Storage labels read "Storage per current preference".
13. **Pair with Keychain and Members shown with reasons, not hidden.** Current source hides Pair for imported Vaults and Members behind `CUBE_MEMBERS_UI_ENABLED`.
14. **Duress drawn as a "Not available" row.** Current source hides Duress when not entitled ("Hidden, not greyed … like Marketplace", `home.rs`). Needs Robert's decision.
15. **Developer mode copy** reads "Shows developer diagnostics. Not needed for Expert tools or everyday tasks." Current source describes it as "Show test networks in the Cubes network dropdown." The artboard copy drops the test-network creation role; must be corrected or the role moved.
16. **BTCB2 beta label** "BTCB2 beta · Installation-wide · account-gated"; current label is "Bitcoin Blake2b - Beta".
17. **Vault Settings default** lands on Wallet & signers; current lands on Node.
18. **Settings default landing differs**: Workspace lands on Display & appearance, Compact on Backup & protection. Compact draws only 5 of 11 sections (others show a "Not drawn in this compact artboard" note).
19. **"Press Enter to open" focus hint** while Enter on a Cube selects it and Open is a separate control. Hint or behavior must change.
20. **Filters "remembered"** in copy only; the prototype does not persist. Chain filter is single-select.
21. **No search** in any Launcher presentation.
22. **Review-only controls inside the app frame**: "Simulate failure" / "Simulate approval" in Pair waiting state.
23. **Recovery Kit links land on the tab top**, not scrolled to the Recovery Kit row.
24. **Attention row "+1 more"** is not expandable and does not name the second item.
25. **Unlock dialog in the state sheet** has a prefilled masked PIN field (synthetic); production must start empty.
26. **Synthetic amounts inside Cubes** (e.g. 0.01240000 BTC, "Plus 25.00 USDB", 1.20000000 tBTC), synthetic pairing expiry "4:52", synthetic phones and contacts, placeholder paths and versions.

## Open questions requiring source or backend validation

1. Persistence scope per preference (unit, primary amount, fiat, currency, rate source, direction badges, theme, filters): Cube, device or installation; immediate or explicit save. — Source read, then Robert for save semantics.
2. Is there a Cube rename API, and where is the name stored? — Source read.
3. Supported logical minimum window size (points, not Retina pixels). — Robert, after source read of window config.
4. Can name, network, lock state and protection/capability summary be read for every Cube on every network before unlock, without side effects? — Source read (protected storage, data-directory layout).
5. What does removing a device revoke (Connect session, tokens, Recovery Kit access, registrations)? — Backend team.
6. Recovery alerts entitlement API: how are verified, unknown, denied and not-applicable distinguished? — Backend team.
7. Lightning Address eligibility and username rules (length, characters, conflicts, change policy). — Backend team.
8. Spark bridge diagnostics fields actually available (process, PID, version, transport, last error) and their sanitization. — Source read.
9. Blake2b admission and BTCB2 gate: may locked Blake2b Cubes be listed when the beta preference is off? What does the account flag versus local preference control? — Source read, backend team.
10. Keychain-eligible key rules: which signer keys may move to a phone, and how ineligibility reasons are derived. — Source read.
11. Descriptor freshness in the Recovery Kit ("Descriptor in kit"): how is staleness determined? — Source read, backend team.
12. Avatar regeneration quota endpoint and preview/commit semantics. — Backend team.
13. Iced 0.14 accessibility: screen-reader names/roles/states and focus traversal for custom widgets. — Source read / spike.
14. Iced 0.14 `shader` widget with the enabled feature set (`wgpu`, `canvas`, `advanced`): depth attachment and compositing with UI layers; redraw suspension. — Spike.
15. Vault "Claim BTCB2" / "Resume Claim" rail entry: where does it live in Concept C? — Robert.
16. Home "Recover a Vault" (heir discovery) and the non-Cube wallet scan section: where do they live on the Launcher? — Robert, after source read.
17. GPU-unavailable fallback: detection and automatic switch to isometric or catalog. — Spike.
18. Reduced-motion signal per OS (macOS, Windows, Linux desktops) and how Iced exposes it. — Source read / spike.
19. Should gated features (Duress, Members, Marketplace) be hidden as today or shown as gate rows? — Robert.
20. Developer mode's role: keep "show test networks in the network dropdown" or move test-network creation elsewhere? — Robert.
21. Payment checks: is the recipient-check preference stored per Cube, per device or per account? — Source read.
22. Launcher search: required for production, and at what count does the 3D/isometric view yield to the catalog? — Robert.
23. Attention row priority when several conditions hold (backup, node offline, reconnect, Stable Balance). — Robert, then source read for available signals.
24. Stable Balance restored-device detection: what signal shows USDB is held while the device preference is off? — Source read.

## Evidence and limits

- Application source baseline: `8495c3c0f233650b8642363c455ccd429b71bc47` (coincube checkout HEAD matched it during this pass). Route labels in the map were read from that tree.
- Shared policy commit: `b4e4fa3c3ecedad93f40d08f71baca7cea707a25`.
- The earlier HTML explorer at the loopback URL was inspected in the built-in browser by the lead.
- The comparison page on port 56918 was not reachable.
- The seven artboards were checked by their builders with stand-in renderers, not in the real Claude Design canvas runtime. This document was written from the artboards' markup and `renderVals()` logic; the canvas was not fetched.
- No native rendering spike, benchmark, GPU test or assistive-technology test has been run.
- No application code changed. Nothing was committed.

## V2 pass (2026-10-21, after Robert's first review)

Robert's feedback on V1: the screens felt cluttered; the sidebar was not distinct from the rest of the app; he is leaning toward the 3D Launcher and wants most meaning on the Cube faces, bigger cubes by default, icons with words in the sidebar, the top bar kept (so no Connect account entry in the sidebar foot), and a rethink of the global Expert group because most advanced tools belong to a specific Vault. He also asked that the Overview take the good parts of the current Tenshu Overview. V1 artboards are unchanged; V2 artboards are a fourth row on the same canvas (https://claude.ai/artifact/4DkSWQshpyXVu2kVrbneRz). This pass ran through the impeccable skill (context load, one structured probe, craft floor); V1 had reused the earlier impeccable round's outputs without rerunning it.

Decisions Robert made in the probe:

| Question | Decision |
|---|---|
| What the 3D Cube faces encode | Network mark on the top face (₿; ₿ + "Blake2b" tag; outline-only ₿ + network tag for test networks); wallet symbols present on the left face (shield, bolt, drop); padlock plus a single backup badge ("!" not confirmed, "?" unknown) on the right face; outline hue = chain family; aura = selection only |
| Where expert tools live | Per-Vault "Advanced" disclosure on each Vault page (Coin control, PSBT proposals, Descriptors & keys, Recovery path tools); Node resources and Maintenance under Settings › Connections; the global Expert group is removed |
| Overview | Keep the current Tenshu Overview structure (Total Balance with hide toggle, Wallets with per-wallet containers, asset rows with per-asset Send/Receive, Transfer action) and refine its craft |

V2 artboards:

- `V2-Launcher-3D.dc.html` (1440×900, interactive): big cubes (1–4 Cubes in one centered row at ~215px; 5–8 in two rows at ~170px; 9–20 in rows of five at ~120px with vertical scroll only), face encoding as decided, chain-family outline (orange / #5FB3C9 / dashed grey), selection-only floor aura, labels reduced to name + Locked/Open, a detail panel for the selected Cube with the Open link, filters and hidden count moved into the top bar, a face legend in the bottom bar, pinned Create/Import, 0-Cube and no-match states. Levers: cubeCount, theme, reducedMotion, focusDemo. Departures: at 20 Cubes the network tag is also printed under non-mainnet labels because the top-face tag is unreadable at ~120px; camera tilt 35°; a 360px gap is reserved for the detail panel at 5+ Cubes.
- `V2-Launcher-Faces.dc.html` (1440×760, static): the four fixture Cubes at ~260px, the three faces enlarged with captions, edge-hue swatches, selected vs not selected, the "!"/"?" badges, dark and light strips.
- `V2-Cube-Overview.dc.html` (1440×900, interactive): the shell with a distinct sidebar (own surface, 1px right rule, 248px, drawn 18px icons + words, selected fill with orange icon), Wallets group kept, foot = Settings only, compact attention row, no Expert group; pages Overview (modeled on the current Tenshu Overview with synthetic values, hide-balances toggles, tabular numerals), Vault (identity, balance, Send/Receive, Recovery path route, recent activity, collapsed Advanced disclosure), Spark, Activity, Backup. Levers: theme, reducedMotion, page, hideBalances, reviewState (normal, signedOut, cubeDisconnected, longNames). Departure: at 1440×900 the Overview scrolls about 80px (the Transfer pill is below the fold at the incumbent's density).
- `V2-Cube-Compact.dc.html` (1024×768, interactive): same shell at 208px sidebar with icons + words; wallet containers put the fiat amount and sats on the header row to fit three cards; Advanced sits above recent activity on the Vault page so it is reachable without scrolling.

Fixture note: in the V2 set, Everyday is "Backup not confirmed" everywhere (Launcher badge, Faces sheet, shell attention row). The V1 inconsistency recorded above remains in V1 only.

Still open from this pass: whether the chain-family outline hue for Bitcoin Blake2b (#5FB3C9) is acceptable brand-wise, since no Blake2b mark exists; whether Recovery path tools belong under Advanced or stay a top-level Vault route only; and native feasibility of face-mounted icons in the Iced shader scene (text and icons on faces are simpler as a pre-rendered texture atlas per face than as live text). The artboards were checked with stand-in renderers by the builders, not in the live canvas runtime.

## V3 Overview without cards (2026-10-22)

Robert asked to see a non-card Overview, keeping the original asset icons and open to a different Receive treatment. `V3-Cube-Overview.dc.html` (1440×900, interactive) sits beside the V2 row on the canvas and reuses the V2 shell. Structure is carried by typography, alignment and 1px rules; there are no cards. Lever `variant`: "ledger" (one 960px column: Total Balance with eye toggle and network at right, 44px fiat, sats, Transfer; then Wallets with three sections whose headers share the asset rows' five-column grid) or "split" (380px left column with total, Transfer, network, and per-wallet links that anchor to the sections on the right). Lever `receiveStyle`: "ghost" (borderless ink text with a tray icon, orange on hover), "icon" (36px circular icon button with a caption on focus), "text" (orange underlined link); Send stays the filled pill in all three. Coin marks are inline SVG recreations of the current Tenshu marks (orange ₿ disc with Spark/Liquid/Vault sub-badges; teal USDt). Receive controls carry the wallet name in their accessible label because Spark and Vault both hold BTC. USDt shows no fiat, as in the current app, to avoid implying a peg. Not viewed in the live canvas runtime; logic ran for all variant and Receive combinations.

Launcher animation question (2026-10-22): Robert proposed a select-to-spin flow (cube grows and yaws to a free face for details; Open yaws again to a face for PIN/passkey). Assessment given: feasible if every spin ends square to the camera and the face content is a flat overlaid panel; two hidden vertical faces serve details and PIN; CSS prototype one to two days; native Iced/wgpu a two-to-three-day spike (overlay compositing, focus, resize, high DPI) then one to two weeks; PIN entry must accept input immediately and never wait on the animation; reduced motion and no-GPU cut straight to the panel. No artboard built yet.

## V3 Launcher spin-to-face prototype (2026-10-22)

`V3-Launcher-Spin.dc.html` (1440×900, interactive; right of the V2 Launcher row) prototypes Robert's select-to-spin idea on the V2 floor. State machine: browse → details → unlock → checks. Selecting a Cube translates it to the stage center, scales it so the projected face is 420px, and yaws 45°→180° so the hidden back-left face ends square to the camera (the scene's perspective origin follows the cube, so the final face is axis-aligned). Open yaws 180°→270° to the back-right face. In both cases the face content is a flat panel hard-positioned over the face rectangle, as the native Iced plan requires: details (name, network, Lock/Backup/Wallets rows, capability note, Open, Back) and unlock (masked PIN input with label and autofocus, Unlock, Use passkey, Cancel, "Network checks run after your PIN is accepted"). Checks shows a static step list (PIN accepted → Checking <network> admission… → Loading wallets) and a "Continue to Cube" link captioned as a prototype shortcut. Every transition is 480ms with one ease-out curve; other Cubes dim to 40% and scale to 0.92 in place; the V2 selection lift and aura are removed so the spin is the only motion; nothing animates continuously. Reduced motion (lever or OS) removes all transitions and shows the panel at once. No global keydown handler; cubes are buttons, panel controls are real inputs and buttons, a live region announces phase changes. Levers: cubeCount, startPhase, theme, reducedMotion, focusDemo.

Departures and limits: every Cube is locked in this artboard (opening an already open Cube would switch tabs, not ask for a PIN); the landing rectangle is exact only while the floor is unscrolled, so the 20-Cube case is out of scope for the spin; the 420px face overlaps dimmed Cubes behind it; PIN autofocus and the `tabIndex` binding assume the runtime's JSX-style attributes; animating `perspective-origin` is browser-supported but unverified in the canvas runtime. Not viewed in the live canvas runtime.

Revision (2026-10-22, after Robert's review of the spin prototype): details and PIN/passkey content now live on the cube's back faces as real DOM controls that rotate into view (the overlaid panel and its fade are removed); spin duration 820ms with cubic-bezier(0.22,1,0.36,1), dimming 500ms; switching Cubes from the details face reverse-spins the outgoing cube to its slot while the incoming cube grows and turns after a 160ms delay (no re-mounting); the incoming cube always opens on details with the PIN cleared. Cost assessment given for native content-on-geometry: flat overlay at rest (days); textured face during the spin with live widgets at rest (one to two weeks beyond the spike; recommended); fully live widgets on angled geometry (three to six weeks, breaks screen readers/focus/IME; ruled out). Known prototype limits: face text may look soft mid-spin (scaled DOM), the active face border reads ~4px, PIN autofocus depends on the runtime.

## V3 decisions (2026-10-22)

Robert approved the spin-to-face Launcher as the production direction (native tier: textured face during the turn, live widgets at rest) with the compact catalog as the fallback by preference, reduced motion, weak hardware or high counts. Further decisions and refinements are recorded in `tenshu-design-brief-v3-decisions.md`: one open Cube at a time with cross-Cube transfer and Blake2b coin splitting handled inside flows; Send/Receive and an all-wallet Activity list with transaction detail as the next wallet-page design job; Connect features moved to the Launcher level (mockup pending); universal hide-balances and theme toggles in the top bar; the V2 compact Overview layout as the model at every size. Each item lists what needs source validation.

## V3 Connect at the Launcher level (2026-10-22)

`V3-Launcher-Connect.dc.html` (1440×900, interactive; right of the spin artboard) draws Connect as a focused 1040px surface over the dimmed, static Launcher floor, reached from the account entry in the Launcher top bar. The top bar adds the two universal, device-wide toggles before the account entry: Hide balances (eye, aria-pressed) and Theme (sun/moon, aria-pressed; switches the theme in state). Sections (left list, real buttons with aria-current): Account (identity, sign-in method, what Connect provides, Sign out through an inline review with the consequence line; signed-out form with email link or passkey; loading skeletons; error with Retry), Plan & billing ([PLAN NAME], verified status, [RENEWAL DATE], per-feature entitlement text: Recovery Kit verified, Recovery alerts unknown · checking, Bitcoin Blake2b beta not enabled; three [DATE] history rows; Compare plans and Referral secondary), Contacts (rows with role tags, long-email wrap, Add contact, empty state), Devices & security (current device first, compact rows, inline removal review with the brief's wording, sign-in activity with Successful/Failed in text, Duress gated), Recovery services (per-Cube table for the four fixtures with Recovery Kit status and alert eligibility; every row says the Cube must be unlocked to change anything and links to the Launcher). Levers: section, accountState (signedIn/signedOut/loading/error), hideBalances, theme, reducedMotion. Departures: the theme button's label changes between "Dark" and "Light" while also carrying aria-pressed, which is ambiguous for screen readers (recommend a constant "Dark theme" label with aria-pressed); billing rows carry no amounts; two muted prototype links drive the contacts empty state and sign-in completion. Not viewed in the live canvas runtime.

## Refinements after the Connect review (2026-10-22)

Per brief section 7: the "All Cubes › Connect" breadcrumb is removed; the Connect panel carries its own heading and a Close control back to the Launcher. The theme toggle is removed from every top bar; the eye (Hide balances) stays. New `V3-Launcher-AppPrefs.dc.html` (1440×900, interactive) draws the device-scoped App preferences page in the Launcher world: Appearance as System (default) / Light / Dark radios following the OS value; Launcher style Cubes / List with the automatic fallback note; Show test networks by default (filter default only); Hide balances (shares state with the top-bar eye); Motion Follow system / Reduced (Reduced forces the List launcher); Developer mode with its real current effects stated from source (shows test networks in the Cube network dropdown; Vault setup clones the Cube master signer instead of generating a new one, `coincube-gui/src/installer/mod.rs` at baseline); Bitcoin Blake2b beta as status text tagged "Account" with a link to Connect (source has both a device setting and an account opt-in, so the tag names where it is managed); Diagnostics (sanitized logs, Repair device registration through an inline review, stream status); About with version/build placeholders, daemon and bridge versions under a disclosure, Download Stats, licenses. Levers: section, theme (as OS theme), reducedMotion, hideBalances.

Bitcoin Blake2b mark: Robert supplied the logo (gold coin with ₿). It is uploaded to the canvas and now serves as the network mark on the cube top faces, the detail/spin faces, the Faces sheet, and Connect's recovery table, with the text tag kept; Blake2b Testnet4 renders it at 60% in a dashed ring with the explicit tag. The Blake2b chain-family edge hue is gold rgba(212,166,42,.6). The Faces sheet gained a "rail badge" row showing the coin-plus-bottom-right-badge pattern (Spark, Lightning, Liquid, Vault) on both the orange Bitcoin coin and the gold Blake2b coin; its fixture cubes shrank to 144px to keep the sheet at 760px. The spin artboard's top bar gained the eye toggle and links to Connect and App preferences. Light-theme contrast of the gold edge against light face greys and the blob image resolving in the live canvas were not verified.

Bitcoin mark (2026-10-22): Robert supplied the standard orange Bitcoin logo (the featured image of the linked Cointribune article). It is uploaded to the canvas and replaces every drawn ₿ used as the Bitcoin network mark or BTC asset coin across the V2 and V3 artboards (Launcher 3D, Faces, Spin, Connect, App preferences, and the three Cube Overview artboards). Bitcoin test networks render it at 60% in a dashed ring with the explicit tag, matching Blake2b Testnet4. Rail badges stay layered at the bottom-right. Noted gap: the V2 Cube Overview artboards' Spark BTC and Vault BTC coins never carried a rail badge (only L-BTC did); applying the full coin-plus-rail pattern there is a separate change. V1 artboards keep their drawn marks. Not viewed in the live canvas runtime.

Rail badges on V2 Overview coins (2026-10-22): the Spark BTC and Vault BTC coins on both V2 Overview artboards now carry their rail badges (Spark: black disc with white spark; Vault: orange disc with chain link), 12px inside the 32px coin with a 1px `--panel` ring, matching the L-BTC badge and the V3 pattern. The earlier gap is closed; light-theme rings follow `--panel` but were not checked by eye.

## V4 flows (2026-10-22): single open Cube, cross-Cube transfer, Send, Receive, Activity

Brief: `tenshu-design-brief-v4-flows.md` (current product labels quoted from source at baseline 8495c3c0). A fifth canvas row holds four interactive artboards; the V2 Overview artboards' Cube switcher was changed in place to the single-Cube model (name · network; "All Cubes"; "Lock Cube" with its note; no tab list).

- `V3-Cube-Transfer.dc.html`: transfer between wallets with "Another Cube…" as a destination. Steps: From/To → pick the other Cube (Cubes on a different network are listed but disabled with the reason in text) → "Unlock Archive for this transfer only" as an inline panel with PIN usable immediately and the admission check list → destination wallet and amount (unit switch, fiat estimate, min/max placeholders, Use maximum) → quote (route named, fee, total) → done ("Archive is locked again."). A "Split coins into Bitcoin and Bitcoin Blake2b…" entry opens an explainer with requirements, flagged "Mechanics subject to source validation". Lever `step`.
- `V3-Wallet-Send.dc.html`: per wallet (lever) compose → Prepare → review → (Vault: sign with per-signer status under the 2-of-3 synthetic policy → broadcast) → sent, with destination-type detection tags, amount unit switch, source wordings ("This invoice sets its own amount.", "Fees (USDt) · Paid in USDt", "Waiting fee acceptance", "Fee Rate (sat/vB)", "Two payment addresses are the same"), recipient checks and the Branta note on review, and error states invalidDestination / overBalance / prepareFailed / feeAcceptance. A muted "Prototype · paste example" row fills synthetic destinations. Vault uses a synthetic 250,000-sat balance (the Overview fixture's 0 would make every send an error).
- `V3-Wallet-Receive.dc.html`: method first per wallet (Spark: invoice, Lightning address, Spark address, deposit; Liquid: Lightning, Liquid address, on-chain → L-BTC swap with "ONE deposit only" and Swap ID, USDt; Vault: single-use address with "Verify on hardware device" and label). States: loading, offline (reason per wallet), signed out, Testnet gate (Cube becomes Lab; Spark/Liquid gated, "Receive with Vault"). Synthetic QR is a drawn placeholder.
- `V3-Activity.dc.html`: all-wallet list with wallet/direction/status selects, date-range text, search by label or id, active-filter count and Clear filters; 14 synthetic rows grouped by day including the cross-Cube transfer, a failed Lightning payment, a pending swap and a Vault spend at "1 of 2 signatures"; a 420px detail panel (not a modal) with amount/fiat at the time, fee, status and confirmations, time with zone, wallet and rail, masked destination, editable BIP329 label, ids with Copy, and one rail-specific disclosure ("PSBT and signers" with Continue signing). Levers presetWallet and selectedRow (string enum). Synthetic amounts do not reconcile with the Overview balances; a footer says so.

Limits: none of the four was rendered in the live canvas runtime (logic ran across every lever combination); controlled inputs use onChange and simulated waits use setTimeout, both unverified in the runtime; the Send artboard's "Continue signing" entry from Activity cannot preset the Sign step.

Corrections after Robert's V4 review (2026-10-22): (1) Split is a Vault clone, not a coin selector: with the Vault selected as the transfer source, "Create a Bitcoin Blake2b copy of this Vault…" appears (only when Blake2b beta access is enabled for the account; a `btcb2Beta` lever reviews the hidden state; the user is never told to enable it) and runs Explain (mirrored keys, policy, signers) → New Cube (name, fixed Bitcoin Blake2b network with the gold mark, own PIN/passkey, seed reused) → Create (step list; "Mechanics subject to source validation") → Done with "Open the new Cube" and a three-line summary of the existing coin-split flow using its source labels (Before fork · both chains, Build step 1/2, Bitcoin-only, BTCB2-only, Abandon split). (2) Send shows no rail badge or tag until a destination is detected; "They receive" reads "Add a destination" until then. (3) Spark Receive gains "From another network" and Send's stablecoin kind gains a rail picker: USDt on Ethereum, Tron, Binance, Solana, Liquid; USDC on Ethereum, Solana; drawn network glyphs, no third-party logos; footnote "Stablecoin rail list subject to SDK validation" (source's send placeholder names only Ethereum, Tron, Solana). Not rendered in the live canvas runtime.

Distill pass (2026-10-22): Robert flagged verbosity and helper text that read as part of the design. The impeccable distill and clarify rules were applied to the V4 row and the V2 shell (`tenshu-design-brief-v4-distill.md`): each idea said once; control consequences live in the label or a confirmation step, never as an always-visible caption (Lock Cube now opens an inline "Lock Everyday and open the Launcher? · Lock · Cancel" row); field helpers only where they change input; one muted prototype footer per artboard; one-or-two-word stepper labels; obscure source labels removed from the UI. Visible copy fell from 512 to 338 words on the transfer artboard and from 877 to 752 across Send, Receive, Activity and the two V2 Overview artboards (script-side hints were cut further). Also: the New Cube step gained a Confirm PIN field with a match rule and a passkey path that hides both fields; the Done step now says the coins exist on both chains and can be spent separately, with "Open the new Cube" primary and "Split coins now" secondary (linking to the Vault's Advanced › Split coins; one plain line on what splitting does). Chips now show the state word only with the label and email as screen-reader text, a departure from the original "two labeled chips" wording that Robert confirmed on 2026-10-22. Not rendered in the live canvas runtime.

## Vault Advanced screens, Settings port, and the clickable prototype page (2026-10-22)

Three review artboards joined the V4 row. `V3-Vault-Advanced.dc.html`: the Vault's Advanced area with sections Coin control (five synthetic coins with select, confirmations, label, masked address, chain badge, status; totals; Send selected, Label, "Refresh coin" per source, the "Before fork · both chains" badge explained in one line), PSBT proposals (list with "N of 2" signatures; detail with signer status, Sign, Export, Broadcast gated on signatures, Delete through an inline review), Descriptors & keys (masked descriptor with Show/Copy, "Export encrypted" primary, plain export behind an inline review, keys table, hardware registration states), Recovery path (availability per path, "A backup does not unlock a time lock.", the recovery spend's real first step), and the Split entry with in-progress state. Departures: no Freeze (the source has none); the coin badge wording differs from the running app's "Seen on both chains"; the Vault holds synthetic coins. `V3-Vault-Split.dc.html`: the existing split flow in the V2 shell following the source's paired two-chain route: Check (needs a Blake2b copy; else links to the clone flow), Coins (read-only; the source spends every shared coin together), Fees (one confirmed fee coin per chain; "Check deposits"), Sign (both transactions), Bitcoin broadcast with Blake2b "recorded, not sent now", confirmations "N of 6", Blake2b broadcast, Done with a Bitcoin-only and a Blake2b-only coin; reorg, dropped and abandon states with the source's safety wording. Unverified: whether a "Complete split" step that deletes source descriptors exists on this route, and the Blake2b confirmation count. `P-Settings.dc.html`: the 11-section Settings ported into the V2 shell and distilled; the Connect account scope is removed (Account & plan and Devices & security now live in the Launcher-level Connect page; the Connections tab keeps a one-line account status linking there); This device shows only a link to App preferences plus the Spark device preference; Display drops the theme row.

Prototype page: a second canvas page "Prototype (clickable)" holds `P-` copies of the chosen screens (Launcher spin, Connect, App preferences, Overview hub, Activity, Settings, Send, Receive, Transfer/Blake2b copy, Advanced, Split) with every link rewritten to stay inside the set and dead navigation turned into real links; the canvas launches focused on the Launcher. Audit: every href in the P- files targets a P- file (plus one in-page anchor). Known limits of the prototype: Send and Receive open the same page from any pill (the wallet is that page's own state, since artboards cannot pass parameters), the Overview's built-in Settings page is reachable only via the disconnected-state attention row, and each artboard keeps its own state across links. Not rendered in the live canvas runtime.

## Engineering finding: Developer mode changes Vault signer derivation (2026-10-22)

Robert challenged the App preferences copy "New Vaults set up on this device also use a copy of the Cube's master key as their hot key" and asked that security never degrade because Developer mode is on. Verified in source at baseline 8495c3c0: `coincube-gui/src/installer/mod.rs` lines 527–548 (`Installer::new`) select the installer's hot signer as a clone of the Cube master signer (via the Liquid signer) when `developer_mode` is true, falling back to a freshly generated signer otherwise or when the master signer is unavailable; the comment at line 1570 states "In developer mode this signer is a clone of the Cube master signer", and `services/unlock/mod.rs` (253, 330, 2788) and `app/settings/mod.rs` (1558) rely on it. The shipped settings row (`home.rs` 5090) describes Developer mode only as "Show test networks in the Cubes network dropdown." The seed is stored encrypted in both modes (the hardening comment at installer 1570–1575). Design disposition: the artboard shows only the shipped description. Open for Robert/engineering: whether Developer mode should affect signer derivation at all, since reusing the Cube master seed as a Vault hot key couples the Vault to the Liquid/Spark seed and the setting's description does not disclose it. This is a product/security decision, not a design one.

Connect sign-in truth from source: email plus one-time code ("We sent a code to {email}", "Enter the verification code sent to {email}", "Resend OTP ({cooldown}s)", "That verification code didn't work", "This code has expired."), sign-up as "Create an Account" with an optional "Promo or referral code" ("Code applied."), "New to COINCUBE | Connect? Create account". Passkeys apply to Cubes, not to Connect sign-in; the earlier Connect artboard's "Sign in with passkey" was wrong and is removed in the V5 pass.

## V5 refinements (2026-10-23): Launcher chrome, Connect identity, sign-in and sign-up

Per `tenshu-design-brief-v5-launcher-connect.md`, applied to the review artboards and their Prototype-page copies. Launcher: the "All Cubes" title is removed from the top bar (kept as a screen-reader heading); the face legend moved out of the bottom bar into a "Key" control at the bottom-right that opens a flat card on hover, focus or click (click toggles; Escape closes; the hover/focus peek has a CSS fallback in case the runtime does not bind those handlers); Create Cube is the filled orange primary and Import the outlined secondary at the bottom-left. Connect: the top-bar entry now reads "Connect · alex@example.test · Signed in" or "Connect · Sign in"; the signed-out panel shows what Connect provides and the sign-in form; sign-in is email → one-time code with resend cooldown and invalid/expired errors (levers authMode, authStep, authError); sign-up is "Create an Account" → email → code → optional promo or referral code → done; passkey sign-in to Connect is removed (passkeys belong to Cubes). The section-list overlap (selected row's ring overdrawn by the next row) is fixed with a 6px row gap and stacking order in both panels. Developer mode's description is the shipped text only. Departures to confirm: the shipped sign-up puts the promo field on the email form, the artboard asks for it after the code; several strings use the brief's wording rather than the source's ("Send code" / "Verify" / "Resend code" instead of "Continue" / "Verify OTP" / "Resend OTP"; "That verification code didn't work" is not a source string; "This code has expired." is the source's promo-redeem error, not the sign-in one). Not rendered in the live canvas runtime.

Decision (2026-10-23): Robert confirmed Developer mode must not change Vault signer derivation. Tracked as an engineering issue, separate from this design work: https://github.com/coincubetech/coincube/issues/761 (coincubetech/coincube#761).

## V6 coverage pass and the complete prototype (2026-10-23)

Robert set the goal of one-to-one coverage of every current Tenshu feature as new artboards (older versions kept) tied into one testable prototype. The native app could not be driven from this session, so coverage was built from the source at baseline 8495c3c0: `tenshu-feature-inventory.md` lists 88 user-facing screens/flows with their exact labels, fields, states, gates and safeguards, and a coverage matrix (before this pass: 26 covered, 33 partial, 29 none). Brief: `tenshu-design-brief-v6-coverage.md`.

New review artboards (sixth and seventh canvas rows, all levers listed on each): Connect sign-in/sign-up with the app's exact email → token flow and promo field on the sign-up form (item 1); Create Cube in the app's order with gates, the 4-digit PIN/passkey step, the creation backup gate ("Copying the Coincube folder is not a backup"), the Vault builder (six templates, timelock diagram, Set keys with the key-source picker), node/backend choice and Connect registration (item 2); Import or restore with every path the app has (descriptor/hardware import with scan and PSBT loop, Recovery Kit, phone recovery, inherited Vault behind its flag, mnemonic noted as having no entry point in this build), PIN setup and finish (item 3); Opening Cube with the cube illustration, the 65-quote list with attribution, three-dot progress and the loader's stage and error strings (item 4); Receive restructured to the Send layout with Bitcoin / Stablecoin master selectors and secondary method rows, SideShift stablecoin flows per wallet (item 5); Launcher wording changed from chain to network, Live chains → Live networks (item 6); plus standalone PIN/passkey unlock, Delete Cube modals, Border Wallet enrolment and reconstruction, Marketplace Buy/Sell (Mavapay and Meld flows) and P2P (Mostro book, create, 19 trade states incl. the Spark hold-invoice step, chat, settings), Duress (status, enrolment, contacts, active screen), Members (gated), Liquid (overview, swap, transactions, settings, sunset gate), Signing (hardware states and advisories, Keychain session, pairing, labels, export, Branta), and Claim BTCB2.

Prototype page: fourteen new `P-` screens joined the set and the old Receive copy was replaced by the V6 Receive. Wiring audit: every href in every P- file targets an existing P- file (three template hrefs resolve only to P- files; three in-page anchors). Walk: Launcher → Create / Import or restore / Delete → Opening → Overview → wallets (Liquid page, Marketplace group) → Send / Receive / Transfer → Activity → Advanced / Split / Claim → Settings / Members / Signing / Border Wallet → Connect / Sign-in / Duress / App preferences → All Cubes. Limits: the Launcher has no signed-out state, so its Connect entry always opens the signed-in Connect page (sign-in is reached from Connect); the old in-page Connect form remains in P-Connect beside the new sign-in screen; several in-page animations became links; Spark "last transaction" and Spark Settings › General have no separate artboard (their content lives in the Settings Spark scope and the Overview); the Members page draws no roles because the app has none. Builders verified logic across lever combinations and link audits; nothing was rendered in the live canvas runtime.

Gap pass (2026-10-23): after the V6 set the matrix read 73 covered / 14 partial / 1 none. Three further artboards close those rows: `V6-Gaps-Installer` (descriptor backup and hardware registration steps the inventory had missed, the chain-unavailable screen and Blake2b-only Launcher mode, the launcher-level split scan with its "no balance" conclusion, hardware-detection states in the key-source picker, the installer Connect step with benefits and "Skip for now", import extras incl. the shared-wallet token, .bed decrypt and the UNSAFE mnemonic option with its warning verbatim, the legacy remote-backend login and "wallet deleted"), `V6-Gaps-Wallets` (Liquid SideShift send with network chooser and rate mode and a swap-status timeline, Spark bridge connecting/error/not-configured, pending-deposit claims, Vault overview rescan-date/sync-failure/recovery timers, RBF bump and cancel, node settings extras gated by backend as source does: Tor, own Esplora, history recovery, retention, custom feerate validation, Launcher restore icons), `V6-Gaps-Account` (Recovery Kit protection-mode picker password/phone/both with its states, alerts keyholder scope and gates, Avatar questionnaire, generation quota, LN-address gate, loading/error). Each has a `screen` lever with per-screen `state` values and an "App: …" route line. Prototype copies are linked from Create ("More installer screens"), Advanced ("More wallet screens") and Settings ("More Settings screens"). Source finding: the CreateWallet installer runs BackupDescriptor then RegisterDescriptor as two steps, which the inventory had omitted. Not rendered in the live canvas runtime.

## Verification pass and sign-in wiring (2026-10-23)

Visual verification: a local stand-in for the canvas runtime (the builders' template emulator, served from the session scratchpad on 127.0.0.1) rendered the prototype screens in the built-in browser at 1440 wide. Inspected and found as designed: Launcher (four Cubes with the Bitcoin and Blake2b logos, "Network ▾" chip, Key control bottom-right, orange Create Cube bottom-left), the unlock face, Connect sign-in email and token steps, Create Cube's Network stage, Import or restore hub, Opening Cube (cube illustration, quote with attribution, three dots, "Connecting to daemon..."), Receive in Bitcoin mode (Lightning invoice with They send / You receive blocks, QR and copy field) and Stablecoin mode (seven rails with network badges, refund address), the Overview hub with the Marketplace group, Settings, P2P order book, and the Duress status page. This is a stand-in render, not the Claude Design runtime, so runtime-specific behaviors (controlled inputs, autofocus, hover handlers) remain unverified there.

Sign-in wiring: the Launcher gained an `accountState` lever; signed out, its Connect entry reads "Connect · Sign in" and opens the V6 sign-in screen. The Connect panel's own email/code forms were removed; signed out it shows what Connect provides with Sign in and Create account linking to the sign-in screen, and the other sections' gates link there too. The eight small sub-details named on covered matrix rows are drawn in `V6-Gaps-Details` (see below) so the matrix carries no open "Missing" cells.

`V6-Gaps-Details.dc.html` (2026-10-23): the eight sub-details from covered matrix rows, each a `screen` with states: the Launcher's "Split a Bitcoin wallet" entry with its four visibility conditions mapped onto the redesigned filter; Stable Balance notice and USDt pending lines on the Cube Overview (both source sentences verbatim); Connect renewal and expired banners; the single plan checkout invoice carrying both a Lightning invoice and an on-chain address with its six states; the contact "Add to Cube(s)" dialog; Spark Send's address-mismatch guard and "Payment status unconfirmed" guidance (verbatim); Vault batch label, feerate options and coin-selection status strings; share-wallet email invitation (the app sends an invitation, it does not generate or revoke links; nothing in the app opens that section at baseline). Prototype copy linked from Settings. Not rendered in the live canvas runtime.

Live-app check attempt (2026-10-23): a screen capture from this session failed ("could not create image from display": the session's process has no Screen Recording permission), and no Tenshu GUI process was running at the time (only the managed bitcoind processes were listed). Per Robert's instruction, coverage therefore rests on the source at baseline 8495c3c0, not on the running application. To close this, Robert can open Tenshu and walk the sign-in, Create Cube, Receive and Overview screens beside the Prototype page, or grant Screen Recording to the terminal host so a later session can capture the window.

## Runtime verification in Claude Design (2026-10-23)

The prototype was exercised in the real Claude Design runtime through Robert's signed-in Chrome (the built-in browser is not signed in). Walked and confirmed: Launcher → select Everyday (spin to details face) → Open (spin to unlock face) → click PIN field, type four digits (field takes focus and input; the Cube stays on the face: the hit-testing fix holds) → Unlock → checks face → Continue to Cube → Opening Cube screen (quote cycling, stage text, "Everyday is open", Continue) → Overview hub → Receive from the Spark row → Bitcoin method row and the Stablecoin selector with its seven rails → Connect page → Sign out (inline review) → signed-out Connect with "Connect · Sign in" in the top bar → Sign in screen → Continue → token step → Verify → "Signed in" with the top bar updated. The Key card opens on hover, so hover handlers bind in this runtime; controlled inputs and onInput handlers work. One defect found and fixed: the sign-in email field was a static demo value, so the token step echoed the default address rather than what was typed; the field is now bound to state and echoed on the token and signed-in steps (V6-Connect-SignIn and P-SignIn). The native Tenshu app itself was not reached (no Screen Recording permission for this session; no GUI process running), so the comparison against the running app remains Robert's.

Sign-up walked in the live runtime (2026-10-23): signed-out Connect → Create account → the sign-in screen (it opens in sign-in mode; "Don't have an account? Sign up" switches modes, since artboards cannot pass state across links) → "Create an Account" with email and the optional promo or referral code on the same form, as the app has it → Continue → token step echoing the typed address → Verify → "Account created · Code applied" with the new address in the top bar. Both the email binding and the promo field work in the runtime.

## Cross-reference: native Launcher Lab spike (found 2026-10-23)

A standalone native Iced/wgpu Launcher prototype already exists at `.worktrees/tenshu-3d/prototypes/launcher-3d` (branch `3d`, head 4b11c94f, PR coincubetech/coincube#742 targeting `design-refresh`), with its own README, DESIGN.md and VERIFICATION.md; a built `TenshuLauncherLab.app` was running on this Mac during this session alongside Tenshu itself. Its verification (2026-10-21/22) supersedes this document's earlier "a native rendering spike is still required": the Iced shader-primitive scene with one reusable cube mesh, GPU instances, floor and contact shadows reuses Iced's device/queue and target; a release sample at 1440×900 recorded about 60 redraws/s with sub-millisecond CPU prepare/encode at 100 Cubes (historical stress), and 12/20-Cube fixtures fit at 1024×700 and 1440×900 in native captures; keyboard operation is complete; the Impeccable disposition is "fix before integration" with selected-identity/catalog synchronization and catalog density deferred "while Claude Design prepares the larger UI/UX artifact", which is this canvas. Reconciliation with this review: the Lab draws the V1/V2-era floor (labels under cubes, right-hand catalog at high counts), not yet the spin-to-face flow, the face encoding (network on top, wallets left, lock and backup badge right), the Key card, or the Bitcoin/Blake2b logo marks; the Estate limit the Lab models (7 Cubes per network, 21 across three fixture networks, realistic totals 10–20) matches this canvas's 0/1/4/20 fixtures and should become the Launcher's stated capacity rule; the Lab's GPU-unavailable/compact fallback is explicitly not implemented, which is where this canvas's compact catalog applies. Items for the integration brief: face textures (snapshot during the turn, live widgets at rest), the 820ms turn and handoff, the Key card, the single-open-Cube model, and the capacity rule. Native accessibility semantics remain unverified on both sides.

## Native comparison, first capture (2026-10-23)

Screen Recording was granted and Tenshu v1.0.0 was running, so its window was captured by window id (not brought to front, no input sent). The app was on its PIN entry screen for a Cube; the capture was deleted after comparison because it names a real Cube. Observed: a tab strip with two "Home" tabs and two "Enter PIN" tabs plus "+", a Back pill at the top-left, the heading "Enter PIN for <Cube>", four empty rounded digit boxes with an orange eye toggle beside them, and a disabled Submit pill. Comparison with the prototype: `V6-Cube-Unlock` matches this screen's structure (four digit boxes, eye toggle, Submit, Back, source strings). The spin-to-face unlock face (`V3-Launcher-Spin`, `P-Launcher`) instead uses a single masked PIN field; for consistency with the native pattern and with Create Cube's "4-digit PIN" validation, the unlock face should adopt the four-box entry. The duplicated Home and Enter PIN tabs are live evidence of the tab clutter the single-open-Cube model removes. The `Tenshu · 3D Launcher Lab` window was captured too (synthetic data): network-colored cubes without face marks, a right-hand "Your Cubes" catalog with name, network and protection line, "7/7 on Bitcoin mainnet" capacity, fixture and benchmark controls; it confirms the native scene renders 20 Cubes at 1024-wide, and it differs from this canvas exactly as the cross-reference section lists. Remaining native screens (Overview, Send, Receive, Settings, Connect sign-in/sign-up, Create Cube) require Robert to navigate the app; captures can then be taken the same way without sending input.

Unlock face aligned with the native PIN entry (2026-10-23): the spin unlock face in `V3-Launcher-Spin` and `P-Launcher` now uses four masked single-digit boxes with an eye toggle, auto-advance and Backspace-back, Unlock enabled only when all four are filled, matching Tenshu's PIN screen and the Create Cube rule. Live-runtime test found box 1 rendering empty after its digit (the runtime re-applied autoFocus on re-render); fixed by focusing box 1 from the Open handler and re-syncing displayed values from state. Re-tested live: Open focuses box 1, all four digits display, Unlock enables, checks follow. Paste of four digits at once is not handled; the review lever starting directly on the unlock phase does not focus box 1.

Native navigation attempt (2026-10-23): with Screen Recording granted, window captures work, but synthetic input needs Accessibility, which this session's process does not have ("osascript is not allowed assistive access", error -1719). Tenshu stayed on its PIN entry screen throughout, and entering a PIN is not something this session will do. The native side-by-side therefore covers the PIN screen only; the remaining screens (Overview, Send, Receive, Settings, Connect sign-in and sign-up, Create Cube) need Robert to navigate, after which they can be captured by window id without input, or Accessibility granted to the terminal host so non-destructive navigation (Home tab, Back, Create/Import/Settings/Sign-in entry points, no submissions) can be scripted.

Accessibility (2026-10-23): the process ancestry for this session's shell is Claude.app → claude-code helper → zsh, so Accessibility must be granted to `/Applications/Claude.app` (System Settings › Privacy & Security › Accessibility), not to Terminal; `AXIsProcessTrustedWithOptions` was called with the prompt option so the entry appears there. Until it is enabled, window capture works but no non-destructive navigation of Tenshu is possible from this session.

## Native comparison, second pass (2026-10-23, Accessibility granted to Claude.app)

Captured by window id with Robert's unlocked Cube on screen (captures deleted afterwards; they show a real Cube name and balances). Two more native screens compared:

- **Cube Overview (live):** left icon rail (Tenshu mark, avatar with Cube name and account email, Cube/Overview, Spark/Settings, Liquid, Vault icons, Light Mode, "Signed in"), two stacked full-width orange warning banners ("Your master seed phrase is not backed up…" with Back Up Now; "This Spark wallet holds USDB from when Stable Balance was on, but the setting is off on this device…" with Spark Settings), Total Balance with eye toggle, Wallets with Spark / Liquid / Vault cards, per-asset Send and Receive pills, centered Transfer pill. Prototype match: V2-Cube-Overview/P-Overview reproduce the balance block, cards, asset rows with the coin-plus-rail marks, Send/Receive pills and Transfer; the two stacked banners are exactly live problem 5 (two large warnings above unrelated screens), replaced in the prototype by the compact attention row plus contextual detail on Backup & protection and the Spark page (the Stable Balance mismatch notice is drawn in V6-Gaps-Details). The live rail's "Light Mode" and "Signed in" foot entries moved to App preferences and the top-bar chips respectively.
- **Vault page (live):** selecting Vault swaps in a second icon rail (Overview, Send, Receive, Coins, Transactions, PSBTs, Recovery, Claim BTCB2, Settings) beside the primary rail: live problem 1 (icon-heavy rails with equal emphasis). Content: a Balance card showing "—" with "Balance unknown until this Vault's history is scanned. Pick the date it was created to start.", a second warning "This Vault was restored from a Recovery Kit that doesn't record when the wallet was created…" with Pick a date / Dismiss, and "Last transactions". Prototype match: the Vault page in V2-Cube-Overview plus V3-Vault-Advanced carry Coins, PSBTs, Recovery path and Claim under one Advanced disclosure and the Vault scope in Settings; the restored-Vault scan-date state is drawn in V6-Gaps-Wallets (`vaultOverview`, voRescanDate). Observed and not yet drawn: the Vault page's "Last transactions" empty heading with no empty-state copy.

Automation note: clicks are delivered by a CGEvent helper (System Events "click at" and plain CGEvents without click-state are ignored by the Iced window); Iced also ignores clicks while its window is not frontmost. The session's auto-mode classifier then blocked the activate-and-click script, so the walk stopped at the Vault page. Remaining native screens still to compare: Send, Receive, Settings, the Connect sign-in and sign-up screens, Create Cube, Import or restore, and the Home Cube list.

Automation result (2026-10-23, later): synthetic CGEvent clicks (with click-state fields, after a title-bar focus click) are not acted on by the Iced window: the hover state renders but no navigation occurs, and some window captures taken mid-render came back blank. The one navigation observed earlier (Vault page appearing) cannot be attributed to the script; Robert may have been operating the app. Driving Tenshu from this session is therefore not viable without a different input path; window capture by id remains reliable. The remaining native screens are to be compared by capturing after Robert navigates to each.

## Source comparison for the screens the native walk could not reach (2026-10-23)

Robert's goal allows the source as the comparison baseline where the running app cannot be reached. Driving Tenshu from this session is not viable (Iced ignores synthetic input), so the seven remaining screens were compared against the source at baseline 8495c3c0 by checking every label, field and state string the feature inventory lists for them against the prototype artboards (`P-*`) and their review copies. Result per screen:

- **Send** (`P-Send`): all inventory strings present (recipient, amount, fee, Lightning/Spark routing, review, confirm). No change.
- **Receive** (`P-Receive`): Bitcoin / Stablecoin master selectors, Lightning, Spark address, USDT methods, copy and QR present. No change.
- **Settings** (`P-Settings`, `P-AppPrefs`, `P-Delete`): the Global Settings toggles ("Developer mode", "Show test networks in the Cubes network dropdown.", "Bitcoin Blake2b - Beta") live in App preferences by the V3 decision; Stable Balance, node, backup and Members are in Cube Settings; Delete Cube has its own artboard. No change.
- **Connect sign-in / sign-up** (`P-SignIn`, `V6-Connect-SignIn`): the artboards carried a redesigned four-item benefits list and no email prompt. Aligned to the app (`installer/view/mod.rs` L1101–1111): "Create a free COINCUBE | Connect account to unlock:" with the app's three bullets (Hosted Esplora server, Encrypted wallet descriptor backup, Integration with the companion Keychain mobile app) and "Enter your email address to receive a verification code." above the email field. The Connect account page (`P-Connect`, `V3-Launcher-Connect`) used the same four-item list; aligned to the same three so the two screens agree. The token step keeps the app's "An authentication was sent to your email:" line (`services/connect/login.rs` L451) and the 6-digit token field.
- **Create Cube** (`P-Create`, `V6-Cube-Create`): the app's intro sentence "A Cube is your account which can contain a Spark wallet, a Liquid wallet, a Vault wallet and other features." (`home.rs` L5356) was missing under "Create a new Cube"; added. Every other inventory string (signed-out note, Unlock method, PIN and Confirm PIN, backup choice, not-a-backup copy, key sources, wallet alias) was already present.
- **Import or restore** (`P-Restore`): all inventory strings present; the heading is "Review Bitcoin wallet" in the review state and "Import a Bitcoin wallet" is covered by the source-tab form. No change.
- **Home Cube list** (`P-Launcher`, `V3-Launcher-Spin`): the Launcher replaces the list, and its details face showed Lock, Backup and Wallets but not the Home list's Connect status line (`home.rs` L5611–5636: "Backed up to Connect — recovery kit ready", "Registered to Connect — no recovery kit", "Not yet synced to Connect. It will sync automatically in a moment.", "Sovereign — local only"). Added a Connect row to the details face with those strings, keyed to each fixture's backup state.

Published as canvas versions 38 and 40 and re-checked in the Claude Design runtime through Chrome: the Everyday Cube's details face shows "Connect · Registered to Connect — no recovery kit", the Create Cube step shows the intro sentence under its heading, and the signed-out Connect → Sign in screen shows the three app bullets and the email prompt with the bound address. Prototype link audit: every `P-*` artboard links only to existing `P-*` artboards (no dead or cross-row links), and the canvas launches on `P-Launcher`, so the prototype page is one connected application of 29 screens.

What this comparison cannot establish: pixel layout and spacing of the native screens, and runtime-only states (loading, server errors) beyond the strings the source emits. Those still need the native screens on screen, captured by window id.

## Independent coverage audit and full prototype walkthrough (2026-10-23)

**Independent audit of the coverage matrix.** A separate Opus agent, briefed only with the inventory and the artboard folder, re-checked every one of the 88 inventory rows by exact source strings (3–6 per row, 482 strings, entity-unescaped, em-dash-tolerant, with a lenient pass for quotes and ellipses) and spot-checked 15 rows (87 strings) against the source at 8495c3c0. Results:

- The scratchpad copy of the inventory had gone stale (it still carried the pre-V6 26/33/29 matrix and no `P-*` mappings); it is now replaced by the worktree copy, which maps every row to its V6/prototype artboards. The audit's findings below are against the artboards themselves, so the stale copy did not affect them.
- By exact-string presence: 48 rows fully present, 38 rows partly present, 2 rows absent (47 Connect Security, 80 Vault Split). Every one of the 87 spot-checked source strings exists in the app, so the absences are real differences in wording, not inventory errors.
- By content, read row by row against the artboards: every one of the 88 features is drawn. The partial and absent verdicts are wording the redesign changed deliberately: Connect Security is drawn as "Signed-in devices" and "Recent sign-ins" (the app says "Verified Devices" and "Login Activity"); Vault Split is the redesigned clone flow Robert asked for, so the app's "Split: step 1 on Bitcoin / Build step 2 / Reserve address" strings are intentionally absent; Settings → General uses "Bitcoin unit / Primary amount / Direction badges" for the app's "Bitcoin display unit: / Primary balance value: / Show direction badges on transactions:"; the Home list's Connect status lines were added to the Launcher details face earlier today; Lightning Address claim and change, coin details, Border Wallet generation, Spark transaction states and firmware advisories are present under adapted copy.
- Residual, recorded as open: three runtime-only state strings are not drawn anywhere (Spark transactions "Loading payment history from the Spark bridge…" and "Failed to load payment history", and the Vault coin detail's "Outpoint:" / "Spend txid:" rows), plus the Connect Overview "Access until" line. These are states, not screens; the audit scripts and per-string results are in the session scratchpad under `audit/`.

**End-to-end walkthrough of the prototype page** in the Claude Design runtime through Robert's Chrome, canvas version 41, every one of the 29 `P-*` screens reached by its in-prototype links (no board opened from the canvas except to restart a path): Launcher → select → Open → four-box PIN → Unlock → checks → Opening Cube (quote, "Everyday is open") → Overview; Overview → Activity (detail panel), Backup & protection, Send (chip destination, amount, Review, Confirm and send, Sent), Receive (Bitcoin / Stablecoin selectors), Vault page → Advanced (Coin control, PSBT proposals, Descriptors & keys, Recovery path, Split coins) → Split flow (Before you start, Coins, Fund the fees), Claim BTCB2 (deposits verify, Review & sign, Broadcast, six confirmations) → Unlock handoff; Liquid, P2P (order book), Buy/Sell, Transfer, Settings (Cube / This device / Vault / Spark scopes) → Members, Pair with Keychain (Signing with firmware advisory and five device states), the three gap strips (Account: Recovery Kit mode; Wallets: Liquid USDt cross-network send; Details: remote-only Cube rows); Launcher → Import or restore (Import Bitcoin wallet, Recovery Kit, Recover a Vault), App preferences (Appearance System/Light/Dark switch, Developer), Connect (Account, Plan & billing, Contacts, Devices & security, Recovery services, sign out → signed-out view → Sign in) → Duress (status, Set up Duress Mode), Delete Cube (PIN-confirmed, Cancel → "Nothing was deleted"), Create Cube (Network, Cube with PIN + Confirm PIN, Backup → phrase → verify, Connect, Vault template, Policy diagram, Keys with the key-source picker → Border Wallet pattern grid, Node, Done) → installer gap screens.

Two defects found and fixed during the walk (version 41): the phrase-verify step accepted empty boxes and moved on (now requires all three words and shows "The words you entered don't match. Please try again.", re-tested live: empty rejected, filled advances); and nothing else broke. Two observations left as-is: the Claim → "Continue in Bitcoin Blake2b" handoff lands on the standalone Unlock screen with the Everyday fixture rather than the paired Blake2b Cube (the Unlock screen only switches fixture in its signed-out lever; artboards cannot pass state across links), and the in-Cube top bar still uses an "All Cubes" button to return to the Launcher, which may deserve the same wording review as the Launcher heading.

What this does not establish: user acceptance of the artboards, and pixel-level parity with the native screens the session could not drive.

## Native comparison, third pass: Connect sign-up (2026-10-23)

Robert brought Tenshu to a new Home tab on the signed-out Connect screen, captured by window id (capture deleted; the tab strip names a real Cube). Observed: header "COINCUBE | CONNECT", a Back pill, heading "Create an Account", lead "Create a COINCUBE account to access Connect and Buy/Sell", an Email field, a "Promo or referral code (optional)" field, a disabled Continue pill, and the sidebar's orange "Sign In" button. Prototype match: `P-SignIn` sign-up mode has the same three fields in the same order, Continue, and the Back control; the stepper (Email → Token → Done) and the benefits list are the redesign's additions. Robert's direction on the same screen: the lead becomes "Create a COINCUBE | Connect account to access these features:" followed by the "What Connect provides" list, expanded to call out inheritance (inviting family or friends to hold keys in the Keychain app, with time-locked recovery keys to recover funds). Applied in version 42 to both sign-in modes; the list now has eight items drawn from the app's sign-in bullets, the Connect API's per-plan features (coincube-api `plan.go`) and the desktop plan taglines (`connect/mod.rs`): encrypted Recovery Kit per Cube, inheritance keyholders, recovery alerts and emergency contacts, hosted Esplora, Keychain phone signing, Lightning Address and avatar, Duress and Connect-enforced policies (Pro/Estate), Buy/Sell and P2P. The brain's project and decision records add nothing user-facing beyond these (Connect blinding and heir escrow are security posture, not features).

Synthetic input retried with Robert watching: CGEvent clicks via both the HID and session event taps, after bringing the process (named "Coincube") to the front, still produce no navigation, and System Events reports no windows for the process, so the Iced window exposes no accessibility tree to click through. Native screens therefore continue to be compared as Robert brings them up.

## Residual states, Connect wording and the benefits list (2026-10-23, canvas version 43)

Closed the audit's residuals: Activity gained a `sparkHistory` lever with the app's "Loading payment history from the Spark bridge…" row and "Failed to load payment history" banner (Retry is prototype-only; the app shows the text alone); Vault Advanced → Coin control gained a per-row Details toggle showing "Outpoint:" and "Spend txid:" with a sixth, spent fixture coin (the app omits the Spend txid line for unspent coins; the prototype shows "—"); Connect → Plan & billing gained a `planState` lever whose cancelled state reads "Access until [RENEWAL DATE]" with a Resubscribe… button; Connect → Devices & security now uses the app's own headings "Verified Devices" and "Login Activity". The "What Connect provides" list on the Connect account page is the same eight items as the sign-in and sign-up screens. All six artboards (prototype and review copies) were syntax-checked and the Connect page, Plan & billing, Devices & security, sign-in and sign-up were re-checked in the live runtime through Chrome.

Vault Split (audit row 80) stays as redesigned: its source strings ("Split: step 1 on Bitcoin", "Build step 2", "Reserve address") are the step jargon Robert asked to remove, so their absence is deliberate, not a gap.

## Branta recipient verification in the new designs (2026-10-23, canvas version 45)

Robert asked that the designs factor in the Branta verification the app already ships (https://branta.pro, "Pay With Certainty": a verified recipient shows as a name and logo with a "Verified" badge; Branta never holds funds). What the app has (baseline 8495c3c0): a Cube Settings toggle "Recipient identity checks" with strict-mode copy and a "Branta privacy information" link (`settings/general.rs`); a shared card "Recipient identified" with logo, platform name, description and "View with Branta" (`shared/recipient_identity.rs`) that renders only for a current identification; `LookupResult { Silent, Identified, Tampered }` with the mismatch message "This payment request’s address does not match its verification data. Do not send." replacing the Spark send review and disabling Confirm, with a "Replace payment request" button; and, for Vault PSBTs, an identity review per identified output (address, amount, card) before broadcast, capped at 280px.

Already drawn before this pass: the Settings toggle and strict-mode copy (P-Settings → Payment checks, Cube-Workspace), the identity card as a Signing-board section, and a "Recipient identity (Branta) · Not identified" row in the Send review. Added now: Send (P-Send / V3-Wallet-Send) gained a `recipientIdentity` lever (notIdentified, identified, tampered, checksOff). Identified shows "Verified · Example Coffee Co." in the checks and the identity card under the recipient line; tampered replaces the review with the app's exact mismatch message and a primary "Replace payment request" that returns to compose, with no Confirm; checksOff shows "Checks off" with a "Turn on in Settings" link. Vault Advanced → PSBT detail (P-Advanced / V3-Vault-Advanced) gained a `psbtIdentity` lever that, for a ready, unbroadcast proposal, shows a "Recipient identity" block (first recipient's masked address and amount, then the card) above the Sign/Export/Broadcast row. Both pairs syntax-checked and smoke-tested by script across every lever value; the default Send review (Not identified) was re-walked in the live runtime. The lever states themselves were not viewed in a browser (they are set from the canvas lever panel, not reachable from prototype links).

Open design point for Robert: the Liquid and cross-Cube Transfer flows have no Branta row because the app only looks up bolt11 invoices and on-chain bitcoin: requests, and the P2P and Buy/Sell flows are counterparties of a different kind; if Branta should appear there too, that is new product scope, not a gap against the current app.

Prototype pickers for lever-only states (2026-10-23, canvas versions 46–47): the Branta result (Send compose: Not identified / Identified / Tampered / Checks off), the PSBT recipient identity (Vault Advanced → PSBT detail) and the Spark history state (Activity filters row) now have small dashed prototype buttons in the clickable prototype, so every state is reachable by clicking rather than from the canvas lever panel. Viewed in the live runtime: Send → Identified shows the "Recipient identified" card under the recipient line and "Verified · Example Coffee Co." in the checks; Send → Tampered replaces the review with the app's mismatch message and "Replace payment request", which returns to compose. A duplicated picker in the prototype Send copy from the first edit pass was removed in version 47.

## Opening Cube: no Continue step (2026-10-23, canvas version 51)

Robert: "I don't think we need to require the user to press Continue. I'd rather just open the Cube." The Opening Cube screen (`P-Opening` / `V6-Cube-Opening`) no longer shows a Continue button. Once the checks pass, "Everyday is open" shows for about a second and the Cube's Overview then appears in place (the artboard mounts the Overview artboard as a child with `dc-import`, the format's documented way to compose artboards; prototype links cannot be followed from script, which the two earlier attempts confirmed: a scripted location change errored and a scripted click on a hidden link was ignored). Verified in the live runtime: Launcher → unlock → checks → Opening → Overview with no click, and the Overview's own links still work from there. For the native app this is simply the loader finishing straight into the Cube, which is what Tenshu already does; the prototype now matches.

Prototype pickers were viewed live as well: Activity → Spark history Loading (bridge row with spinner) and Failed (banner with Retry), and Vault Advanced → PSBT "School fees, two payees" → Identified (Recipient identity block with address, amount and the Branta card above the actions).

## Evidence checklist against Robert's six items (2026-10-23, canvas version 51)

1. **Connect sign-in and sign-up.** `P-SignIn` / `V6-Connect-SignIn`: the app's fields in the app's order (sign-up: Email, "Promo or referral code (optional)", Continue; sign-in: Email, Continue; token step: "An authentication was sent to your email:", 6-digit token, Verify; Done), the app's benefits lead and the eight-item list with inheritance, the app's email prompt. Walked live in both modes; the native "Create an Account" screen Robert brought up matched field for field.
2. **Create Cube.** `P-Create` / `V6-Cube-Create`: Network, Cube (name, Unlock method Passkey/PIN, Enter PIN, Confirm PIN, app intro sentence), Backup (write phrase → verify, Recovery Kit, "I'll do this later" gate with the not-a-backup copy), Connect, Vault template, Policy diagram, Keys (key-source picker, Border Wallet), Node, Done → Open this Cube. Walked live through all nine steps; empty phrase verify now rejected.
3. **Import or restore.** `P-Restore` / `V6-Cube-Restore`: Import Bitcoin wallet (descriptor/hardware, scan start, PSBT loop), Recovery Kit (email, code, Cube picker, recovery password), Recover with phone, Recover a Vault (keyholder list, approve on Keychain), Recovery phrase (12 words), each with PIN setup and finish. Walked live: Import, Recovery Kit and Recover a Vault paths.
4. **Opening Cube with quotes.** `P-Opening` / `V6-Cube-Opening`: cube illustration, the 65-quote list with attribution, the loader's stage strings, "Checks passed", "<Cube> is open", then the Cube opens by itself (no Continue). Walked live.
5. **Receive like Send.** `P-Receive` / `V6-Wallet-Receive`: the Send layout (Payment details card, You send / They receive coins, stepper) with Bitcoin and Stablecoin master selectors; Bitcoin → Lightning invoice, Lightning address, Spark address, Bitcoin address; Stablecoin → USDt on Liquid and the SideShift networks. Walked live on both selectors.
6. **Launcher wording.** Every user-facing Launcher string says network ("All networks", "Live networks", "Test networks", "Network" menu, "Show Cubes by network", "Filters change what is shown, never a Cube's network"); "chain" survives only as code identifiers and the `chainLabel` hole, whose values are network names. Grepped the visible text of `P-Launcher` and `V3-Launcher-Spin`.

**One testable application.** The Prototype page's 29 `P-*` screens link only to each other (link audit), the canvas launches on `P-Launcher`, and the whole set was walked end to end in the live runtime, including every lever-only state through the prototype pickers. Older versions (V1–V6 rows) are kept untouched beside it.

**Not verifiable from this session.** Native pixel parity for screens Robert has not brought up in Tenshu, and Robert's own acceptance.

Launcher wording, final sweep (2026-10-23, canvas version 52): the user-facing Launcher strings were changed from chain to network in the V6 pass; this sweep renamed the remaining 31 internal identifiers and template holes (`chainLabel`, `chainMenu`, `chainItems`, `c.chain`, …) to `network…` in `P-Launcher` and `V3-Launcher-Spin`, so neither copy contains the word "chain" anywhere. Both scripts syntax-checked.

## Robert's round of 2026-10-23 (afternoon): Advanced, Spark page, Lightning Address, Stable Balance

- **Vault Advanced header and entry.** The Vault page's four-row Advanced list is now one row ("Advanced · Coin control, PSBT proposals, descriptors & keys, recovery path, split coins, node, import/export, Keychain pairing") that opens the Advanced page, whose header now uses the Send screen's back-arrow pattern ("‹ Vault · Family savings" above "Advanced"). Both copies (`P-Overview`/`V2-Cube-Overview`, `P-Advanced`/`V3-Vault-Advanced`).
- **Three new Advanced tabs**, drawn 1:1 from the native screens Robert captured: **Node** (active backend, local blockchain status, Switch to COINCUBE | Connect, Advanced node settings with Bitcoin Core/Knots select, network, block height, cookie and socket rows, Inbound connections over Tor with the three toggles, connections/onion/shared-today lines, Restart node to apply, Node resources with Minimal/Compact/Default and custom MB, mempool cap, small/regular computer, estimated total and prune notes, Blockchain rescan with Year/Month/Day); **Import/Export** (Connect Recovery Kit, Encrypted descriptor, Export wallet, Import wallet, Other formats: BIP 329 labels, Transactions table, Descriptor only plain-text); **Pair with Keychain** (Pair a phone with the "No Keychain keys to pair" state and a `pairState` lever for ready/paired, Paired phones, vault-id footnote). The tab bar wraps with eight tabs. Synthetic placeholders replace the real cookie path and onion address.
- **Split vs Claim.** In the app, "Split" (`view/vault/split.rs`) moves a *foreign* wallet's pre-fork coins into this Vault on Blake2b so they can't be replayed (started from Home → "Split a Bitcoin wallet"), while "Claim Bitcoin Blake2b" (`view/vault/claim.rs`) separates this Vault's *own* coins through a Connect-coordinated two-chain transaction pair (Bitcoin broadcast first, Blake2b recorded, then confirmations). Robert's redesign already redefined "Split coins into Bitcoin and Bitcoin Blake2b" as exactly that second job (clone this Vault to a Blake2b Cube and separate its coins, with the fund-fees / sign-both / track-confirmations steps), so the "Claim BTCB2" card on the Split tab duplicated it and was removed; its gating sentence moved onto the Split card. The foreign-wallet split stays at the Launcher level (installer gaps). `P-Claim` remains as a review artboard.
- **Spark page.** Gained a Recent activity list (four synthetic rows, "View all transactions") between the balance and the Lightning Address / Stable Balance rows, as on the native Spark overview; Stable Balance is a toggle instead of a Review button; Lightning Address gained a Change button with the app's in-place rename form ("Current: …", new username + domain, "Choose a new username", Cancel / Change) and the app's confirmation ("Change your Lightning Address?" / "Yes, change it"). Walked live: the address changes to the new username.
- **Settings → Spark.** Stable Balance is an on/off toggle with copy from the Breez Spark SDK stable-balance guide (activation converts excess sats to the token, deactivation converts back; failed conversions retry; amounts can differ slightly from the estimate). The "Cube service ready" badge is gone: the app claims the address through Connect per registered Cube (`state/connect/cube.rs`), which needs the signed-in account, not the Cube's gRPC session.

Native comparison, fourth pass (2026-10-23): Tenshu on Spark → Settings → Lightning Address in edit mode (captured by window id, deleted afterwards; the tab strip names a real Cube). Observed: card "Your Lightning Address", "Current: <address>", a "new username" field with the "@coincube.io" suffix beside it, hint "Choose a new username", Cancel and a disabled Change button. Prototype match: the Spark page's change form (`P-Overview`) has the same elements in the same order, with Change enabled only once a valid username is typed and the app's "Change your Lightning Address?" / "Yes, change it" confirmation after it; the prototype's domain is the synthetic `ln.example.test`. The Node, Import/Export and Pair with Keychain tabs were drawn directly from Robert's native captures of those screens.

## Decision: Settings versus Tools (2026-10-23, Robert approved)

Rule: **Settings is configuration** (names, policy, signers and aliases, node, phone pairing, connections, backups and exports); **Tools (formerly Advanced) is operations on coins and transactions** (coin control, PSBT proposals, recovery spend, split coins). A test: a screen with Save is a setting; one that ends in Sign or Broadcast is a tool. Each item has one home and the other area links across; the same item in two places is a UX fault (two mental maps, two copies to keep consistent, an app that feels larger than it is). Applied: Pair with Keychain, Node and Import/Export leave the Tools page (they live in Settings → Vault: Pair with Keychain, a new Node tab, Data & exports with the app's seven rows); Descriptors & keys leaves Tools (signer aliases and registration are in Wallet & signers, descriptor export in Data & exports); the recovery path stays a tool, with Settings keeping the one-line policy and an "Open recovery tools" link. The Vault page row reads "Tools · Coin control, PSBT proposals, recovery path, split coins"; the page is titled Tools.

Applied and verified (canvas versions 58–60): the Tools page has four tabs under the back-arrow header; the Vault page row reads "Tools · Coin control, PSBT proposals, recovery path, split coins"; Settings → Vault gained a Node tab (status, Advanced node settings, Tor inbound, resources, rescan, all stateful) and its Connections tab points at it instead of repeating the node backend and rescan; Wallet & signers links "Open recovery tools" and "Open Vault tools"; the Split flow's back links say Tools; board titles and the V2 row title say Tools. Two defects found in the live walk and fixed: a stray closing tag left the old Maintenance block rendering on every Vault Settings tab (removed), and the transplanted Node resource blocks used the grid row class and overlapped (now plain blocks). Walked live: Vault page → Tools → four tabs; Settings → Vault → Node (collapsed and expanded, resources, rescan) and Connections.

## Cube avatar placement and the Launcher's open state (2026-10-23, Robert's request)

**Avatar.** The Cube's sumi-e avatar (managed in Cube → Settings → Avatar) now appears in two places: on the Launcher, beside each Cube's name and state under its cube and in the details face title; and inside the Cube, as an identity block at the top of the side menu above Overview (avatar, Cube name, "Bitcoin mainnet · Open"), matching where the native app shows the avatar on its rail. The artwork is a synthetic ink-stroke placeholder per Cube (ensō, bamboo, mountain, water, crane, pine) standing in for the generated images.

**Open state on the Launcher.** The Launcher used to show every Cube as Locked after an unlock because artboards keep their own state. The prototype now remembers the open Cube across screens (a single key in the frame's localStorage, prototype-only; the app keeps this in memory): unlocking from the Launcher marks that Cube Open; the Launcher label turns green "Open" with an unlocked glyph; its details face shows "Lock · Open" with "Go to Cube" (straight to the Overview, no checks) and "Lock"; and every in-Cube "Lock Cube" confirmation clears it so the Launcher shows Locked again. One open Cube at a time, as decided in V4.

## Cube switcher moved into the side menu; open-state fix (2026-10-23, canvas v74)

**Decision (Robert):** the top bar no longer carries the Cube name, network, "All Cubes" or "Lock Cube". The side menu's identity block (avatar, Cube name, network icon + "Bitcoin mainnet", chevron) is the switcher. Clicking it opens a dropdown under the block with "All Cubes" and "Lock Cube"; "Lock Cube" expands in place to "Lock Everyday and open the Launcher? · Lock / Cancel". The top bar keeps only the Tenshu mark and the status chips. Applied to all 36 Cube screens (P-, V2-, V3-, V6-); V3-Cube-Overview keeps its own "Open Cubes" variant of the menu.

**Open-state fix (Robert: "when I unlock a Cube, it still shows as Locked on the Launcher").** Verified live in Chrome:
- Launcher → Everyday → Open → PIN → Unlock → checks face → Continue to Cube → Opening → Overview (auto). Back via All Cubes: Launcher in browse, "Everyday · Open", details face shows "Go to Cube / Lock".
- Overview → side-menu switcher → Lock Cube → Lock: Launcher shows "Everyday · Locked".

**Prototype mechanics worth knowing (runtime, not app behaviour):**
- The Claude Design runtime navigates on a prototype link before the link's own `onClick`/`onMouseDown` run, so a link cannot change state on its way out. Two hand-off artboards absorb that: `P-Locking.dc.html` and `V3-Locking.dc.html` clear the open-Cube flag (`tenshu.proto.open`) and mount the Launcher in place with `dc-import`. Every "Lock" link points at them.
- The Overview pages mark `tenshu.proto.seen` on load; the Launcher bounces a stale unlock/checks face to browse only once that marker matches the open Cube, so the checks face still shows once after unlocking.
- A `<div>` wrapper around the menu's `<sc-if>` block lost its closing tag in three screens because the extraction regex stopped at the lock prompt's own `</div></sc-if>`; the menu now sits first inside `<nav class="side">` with `.side{position:relative}` and a fixed `top:69px`, matching the old header pattern.

## Bitcoin Blake2b device toggle; canvas reduced to the prototype (2026-10-23, canvas v86)

**Decisions (Robert):**
1. Nothing Blake2b shows anywhere in Tenshu unless Bitcoin Blake2b is turned on for this device. With it off, Create's Network step lists only Bitcoin (and the test networks under Developer mode); the Launcher's network menu and Cube list carry no Blake2b entries; Transfer has no "Blake2b copy"; Tools has no Split tab and no "both chains" coin notes; Connect's account page has no Blake2b entitlement row.
2. Enabling it is a plain toggle in App preferences › Developer ("Bitcoin Blake2b · This device"), not a Connect beta grant. Robert may reverse this later; the gate is a single device flag so that is a one-place change.
3. The canvas keeps only the linked prototype: 30 screens in six titled rows (Launcher & account; Create, restore, delete; Inside a Cube; Vault tools & signing; Settings & protection; Marketplace). The 41 exploration boards (Launcher 1–3, Concept C, V2, V3/V4, V6) are off the canvas and archived under `design-review/archive/explorations/`. All links inside the prototype were re-pointed to prototype screens; the full walk still works.

**Verified live in Chrome:** toggle off → Launcher shows Everyday, Lab, Archive, Household 2024 and the network menu lists Bitcoin mainnet, Testnet4, Signet, Regtest; Create › Network shows Bitcoin only. Toggle on → "Blake workspace" returns to the Launcher and the menu adds Bitcoin Blake2b mainnet and Blake2b Testnet4. Split coins and Claim pages show a "Bitcoin Blake2b is off on this device" notice with a link to App preferences when reached while off.

**Prototype mechanics:** the flag is `tenshu.proto.b2b` in localStorage (same pattern as the open-Cube flag); every gated screen reads it at render. Lever-driven review states that assume Blake2b (Opening's `blake2bRetry` error, P2P on a Blake2b Cube, installer gap screens) are unchanged; they are reachable only through levers.

## Developer mode gates test networks; brand mark returns to the Launcher (2026-10-23, canvas v87)

**Decisions (Robert):**
1. With Developer mode off there are no test networks anywhere: the Launcher shows "All networks" only when more than one network is listed, shows "Live networks" and "Test networks" only in Developer mode, and lists only live networks in its Network menu and Cube list; Create's "Test networks" group shows only "Test networks appear when Developer mode is on · App preferences". Turning Developer mode on restores all of it. The separate "Show test networks by default" preference in App preferences › Launcher was removed as redundant.
2. The "Tenshu" mark with the cube icon at the top left of every screen is now a button back to the Launcher. It does not lock the open Cube; the Launcher shows that Cube as Open with "Go to Cube".

**Verified live in Chrome (v87):** Developer mode off → Launcher shows no chips with Bitcoin alone, "All networks" alone once Blake2b is on, and all three chips in Developer mode; the menu lists Bitcoin mainnet and, with the Blake2b toggle on, Bitcoin Blake2b mainnet; Create › Network shows the note under "Test networks". Brand click from App preferences and Create returns to the Launcher.

**Prototype mechanics:** the flag is `tenshu.proto.dev` in localStorage, written by the Developer-mode toggle and read by the Launcher and Create at render; a filter left on Live/Test or a test network falls back to "All" when the flag is off.

## Import Bitcoin wallet: full import with keys, no PSBT signing page (2026-10-23, canvas v91)

**Decision (Robert):** importing a wallet is not watch-only. A descriptor carries no secrets, so after the descriptor the user supplies the wallet's keys (recovery words, a hardware device, or a Keychain phone) and the wallet becomes a normal wallet in the Cube: Send, Receive, Activity and Tools work on it with Tenshu signing. The "Sign" step with the unsigned-PSBT round trip (Save PSBT file › sign in the original wallet › Load signed PSBT › Broadcast) is removed.

**What the app does today, for the record:** `app/state/imported_bitcoin.rs` imports public descriptors only ("Use your existing Bitcoin wallet in this Cube. Signing stays with your original wallet or hardware device.") and spends through the PSBT loop, and the Cube step in `home.rs` says the original wallet's signers remain necessary. The resulting Cube looks like any Cube with a Vault, so the limitation only shows at the point of spending. The redesign changes this behaviour; Codex should implement it, not preserve it.

**Prototype (P-Restore, path "import"):** steps are Cube → Source → Keys → Review → Scan → Done. Keys lists each signer from the descriptor with its fingerprint and path, offers Recovery words / Hardware wallet / Keychain phone as the source, and "Review wallet" stays disabled until the key verifies against the fingerprint. Review shows "Keys: 1 of 1 provided · recovery words · verified against the fingerprint". Done summarises the wallet, where its keys live, and that the Recovery Kit now includes it, with "Open Cube". Copy that described a watch-only import was rewritten on the hub, the Cube step and the Source step. Importing a wallet is not Blake2b-specific; only the later Split tool is.

**Verified live in Chrome (v90):** Launcher › Import or restore › Import Bitcoin wallet, through Cube (PIN), Source, Keys (verify), Review, Scan, Done.

## Activity declutter; recent-activity rows share one grammar (2026-10-23, canvas v93)

**Robert:** "The activity page is a bit crowded, especially the side details screen", and the Vault/Spark "Recent activity" rows should match the Activity rows.

**Activity page (P-Activity):**
- Filter bar is one row: Wallet, Direction, Status, Date range, Search. The filter count and "Clear filters" appear only while a filter is active. The prototype-only Spark-history pickers moved to a small dashed "Prototype · Spark history" strip under the page title, out of the product bar.
- Table: Amount and Fiat merged into one right-aligned cell, sats on top and fiat as a caption. Columns: Time, Wallet, Details, Amount, Status.
- Detail panel: header is title, "Vault · Family savings · Bitcoin on-chain", amount with fiat beneath, one short status line ("Awaiting a second signature · 1 of 2 collected"). For a transaction that still needs signatures, "PSBT and signers" sits directly under the status with "Continue signing"; for other transactions it stays collapsed at the bottom. Detail rows cut to Date (time zone in a tooltip), Fee, Confirmations (only after broadcast) and Destination; the Wallet and Rail rows are gone because the header carries them. Txid is hidden until broadcast. Identifier rows use a small copy icon instead of a button.

**Recent activity on the Overview pages (P-Overview, Vault, Spark and the hub list):** the same row as Activity minus the Wallet column: date, direction glyph with the label, amount with fiat caption, status chip. Rows link to Activity. Vault now has the same "View all transactions" link as Spark. The hub list keeps a small wallet name under the label.

**Verified live in Chrome (v92/v93):** Activity filter row, merged amount cell, detail panel order for the 1-of-2 PSBT; Vault and Spark recent-activity rows.

## AI access revised: pair once, AI-led setup, AI-prepared transactions (2026-10-23, canvas v95)

**Brief (Robert):** replace the read-only / topology-proposal direction with AI-led wallet setup and transaction preparation through Tenshu's local MCP server. Tenshu stays the wallet engine; seeds, keys, PINs, passwords and passkeys never enter the conversation.

**What exists today (`mcp` branch, docs/mcp/README.md and docs/architecture/local-mcp.md):** Cube › Settings › AI access with a per-session opt-in, one client grant per unlocked Cube that expires after one hour and ends on lock/close, a private configuration copied by hand into the host, three read capabilities (balances, confirmed history, policy summary), and an experimental Regtest build where a client stages one 3-of-5 proposal with a chosen recovery that hands into Tenshu's native key selection and review. No transaction tools.

**New screens (row 7 on the canvas; entry from Cube › Settings › AI access):**
- **AI access (P-AI-Access).** Paired clients with name, scoped Cubes and wallets, permission chips, availability (Available / Paused · Cube was locked / Not connected), last activity, Edit permissions and Revoke. "Pair a client" in four steps: client (Claude Desktop, Maple AI Desktop, other MCP host), wallet scope with "wallets created later" off by default, permissions (Read wallet information · Manage wallet setup · Prepare unsigned transactions; none granted automatically), one-time pairing code; manual configuration stays under Advanced with the credential masked and a "never paste into chat" warning. Revoke explains that access ends now, that already-read data may be with the provider, and that created wallets stay. A dashed card states that paired is not available: locking pauses protected operations, pairing survives, the client reconnects after a normal unlock or relaunch, and can never unlock a Cube. The disclosure that a local connection can still send data to the AI provider sits at the top.
- **AI wallet setup (P-AI-Setup).** The native progress page for "Help me set up a 3-of-5 Bitcoin multisig wallet with inheritance": assistant quote, status pill, a handoff card per state (AI working · waiting for you: create a PIN · waiting for device: approve on the hardware wallet · validation issue: descriptor differs from the plan · ready for final review · creating · created · recoverable failure), a progress list with who acts on each step, and the plan (3-of-5, 2-of-3 after 144 relative blocks, P2WSH, Regtest clearly labelled, loopback node). The final native review reads the real descriptor: spending and recovery, network and node, five signers with provenance, and four distinct checks (descriptor validated, devices verified, backup recorded, recovery tested) with the note that none implies another and that distinct fingerprints do not prove independent custody. Any change invalidates the review. After creation, pairing stays and access to the new Vault follows the chosen permissions. Cancelling keeps pairing; failure states what exists and what does not.
- **AI-prepared transaction (P-AI-Transaction).** "Prepare a transaction sending $100 worth of bitcoin": status pill (Draft · Ready to review and sign · Waiting for signatures · Signed · Broadcast), the draft with recipient and network check, exact sats and requested fiat, Tenshu's rate source and timestamp with validity, fee rate and fee ("added on top; the recipient receives the full amount"), total, change, signatures, checks (Branta, coin age); failure and resume states for insufficient funds, wrong-network address, stale quote (refresh shows changes before signing), unavailable signer, and changed inputs (a what-changed list). "Review and sign in Tenshu" leads to Send; nothing here spends.

**Status labels:** every screen carries Today / Planned badges (toggle "Status labels" in the prototype strip) and an implementation-status legend: today is read-only with hourly grants and the Regtest experiment; persistent pairing, setup orchestration and transaction preparation are planned.
