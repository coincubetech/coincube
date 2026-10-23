# Tenshu redesign · implementation hand-off

This folder is the complete hand-off for implementing the Tenshu redesign in this
repository. It was produced in a Claude Design review with Robert on 2026-10-22/23.
Read this file first, then the review doc, then the inventory.

| What | Where |
| --- | --- |
| Live prototype (clickable, needs a signed-in claude.ai browser) | https://claude.ai/artifact/4DkSWQshpyXVu2kVrbneRz (canvas v86) |
| Artboard sources (30 linked screens, plain HTML + CSS + a small script each) | `canvas/*.dc.html`, index in `canvas/canvas.json` |
| Earlier exploration boards the prototype was distilled from (superseded) | `archive/explorations/` |
| Every decision Robert made, in order, with reasons | `tenshu-launcher-and-settings-review.md` |
| Feature coverage matrix (88 rows: existing feature → screen) | `tenshu-feature-inventory.md` |
| Product frame (users, platform, constraints) | `PRODUCT.md` |
| Design briefs v1–v6 (history; superseded where the review doc disagrees) | `tenshu-design-brief-*.md`, `tenshu-concepts.md` |
| Pre-canvas concept pages (superseded) | `archive/` |

Precedence when sources disagree: review doc > artboard source > brief.

## 1. How to read the prototype

- Each `.dc.html` is one artboard. Open it in an editor: the `<helmet>` block holds
  the screen's CSS, the markup follows, and the `<script data-dc-script>` holds the
  per-screen state. `data-props` on that script lists the review "levers" (the
  states Robert reviewed). `{{ holes }}` are filled from `renderVals()`.
- The files reference `./support.js` and the Claude Design runtime, which are not
  in this repo. They do not render standalone. Use the live link for pixels and the
  files for exact copy, spacing, colours and state logic.
- The canvas holds only the linked prototype, in six rows: Launcher & account;
  Create, restore, delete; Inside a Cube; Vault tools & signing; Settings &
  protection; Marketplace. Every link stays inside this set. The earlier
  exploration boards live in `archive/explorations/` and are not current.
- Inside the live canvas, the per-screen "play" button opens that screen
  full-frame with working links. Prefer it over scrolling the canvas.

## 2. Prototype mechanics that are NOT product behaviour

Implement none of these. They exist only so the prototype could be clicked through.

| In the prototype | Why it is there | In the app |
| --- | --- | --- |
| `localStorage` keys `tenshu.proto.open` and `tenshu.proto.seen` | Share "which Cube is open" across artboards | In-memory app state (`gui/tab.rs::State`, `app/session.rs`) |
| `localStorage` key `tenshu.proto.b2b` | Share the Bitcoin Blake2b device toggle across artboards | A persisted per-device app setting (next to Developer mode in `app/view/settings/general.rs`); when off, nothing Blake2b is rendered anywhere |
| `localStorage` key `tenshu.proto.dev` | Share the Developer-mode toggle across artboards | The existing Developer-mode device setting; when off, no test network is rendered anywhere (no Live/Test chips, "All networks" only with two or more networks listed, no test networks in the Launcher menu or Cube list, none in Create) |
| `P-Locking` / `V3-Locking` artboards | The runtime navigates on a link before the link's own handler runs | Lock Cube is one action: clear the session, return to the Launcher |
| `P-Opening` mounting `P-Overview` after 900 ms via `dc-import` | No programmatic navigation in the runtime | Opening finishes → Overview, no timer |
| Dashed "picker" buttons (`.pb`) and `data-props` levers | Reach lever-only states by clicking | Real state from the daemon / Connect |
| Synthetic names, balances, addresses, emails (`*@example.test`) | Review rule: no real data | Real data; keep the copy |
| Prototype PIN accepts anything | No crypto in a prototype | `services/unlock/` unchanged |
| `/_blob/…` images | Artifact asset store | Ship the network icons from `coincube-ui/static` |
| Fixed 1440×900 and 1024×768 frames | Canvas artboards | Responsive iced layout; 1024×768 is the minimum to honour |

## 3. Code map · where each screen lands

All paths are under `coincube-gui/src/` unless noted. Read `CLAUDE.md` at the repo
root first: the GUI is Elm-style, split by layer (`app/state/**` logic,
`app/view/**` render), subtrees mirror each other, and `app/menu.rs::Menu` is
navigation.

| Screen(s) | Existing module(s) to change |
| --- | --- |
| Launcher (P-Launcher): network tabs, 3D floor, details face, PIN/passkey face, checks face | `home.rs`, `pin_entry.rs`, `passkey_unlock.rs`, `loader.rs` (checks), `gui/tab.rs` |
| Opening Cube with quotes (P-Opening) | `loader.rs` |
| Connect sign-in/sign-up, plans, devices, login activity (P-SignIn, P-Connect) | `app/state/connect/**`, `app/view/connect/**`, `services/coincube/client.rs` |
| Create Cube, Import or restore (P-Create, P-Restore, P-GapsInstaller) | `installer/**` |
| Cube shell: side menu with identity block + Cube switcher, top bar with chips (every `P-` Cube screen) | `app/mod.rs` (`Panels`), `app/menu.rs`, `app/view/nav/**`, `app/view/wallet_header.rs`, `app/view/mod.rs` |
| Overview hub, wallet pages (P-Overview) | `app/view/global_home.rs`, `app/state/global_home.rs`, `app/view/vault/overview.rs`, `app/view/spark/overview.rs`, `app/view/liquid/**` |
| Send with Branta identity (P-Send) | `app/view/vault/spend/**`, `app/view/spark/send.rs`, `services/branta.rs`, `app/view/shared/**` |
| Receive with Bitcoin / Stablecoin selectors (P-Receive) | `app/view/vault/receive.rs`, `app/view/spark/receive.rs`, `app/view/spark/sideshift_receive.rs` |
| Activity (P-Activity) | `app/view/vault/transactions.rs`, `app/view/spark/transactions.rs`, `app/view/spark/last_tx.rs` |
| Spark page: recent activity, Lightning Address change, Stable Balance toggle | `app/view/spark/**`, `app/state/spark/**`, `app/state/connect/cube.rs` (Lightning Address claim) |
| Settings = configuration (P-Settings: Wallet & signers, Node, Pair with Keychain, Connections, Data & exports; Spark scope) | `app/view/settings/**`, `app/state/settings/**`, `app/settings/mod.rs` (`CubeSettings`), `app/view/vault/settings/**`, `app/view/spark/settings/**`, `node/**` |
| Tools = operations (P-Advanced: Coin control, PSBT proposals, Recovery path, Split coins) | `app/view/vault/coins.rs`, `psbts.rs`, `psbt.rs`, `recovery.rs`, `split.rs`, `claim.rs` |
| Transfer to another Cube (P-Transfer) | `app/view/vault/spend/**` + Launcher unlock-in-flow |
| Marketplace Buy/Sell, P2P (P-BuySell, P-P2P) | `app/view/buysell/**`, `app/view/p2p/**`, `app/state/buysell.rs` |
| Duress, Members, Border Wallet, Delete, Signing (P-Duress, P-Members, P-BorderWallet, P-Delete, P-Signing) | `app/view/duress/**`, `app/view/connect/cube_members.rs`, `app/view/settings/local_signing.rs`, `app/view/vault/hw.rs`, `phone_signer/**` |
| App preferences (P-AppPrefs), incl. the Developer-mode and Bitcoin Blake2b device toggles | `app/view/settings/general.rs`, `app/view/settings/about.rs`, `feature_flags.rs` (Blake2b is a runtime device setting now, not a Connect entitlement) |
| Theme, type, components | `coincube-ui/src/theme/**` (`palette.rs` for tokens), `coincube-ui/src/font.rs`, `coincube-ui/src/component/**` |

Do not touch `coincubed/`, `coincube-core/`, `coincube-spark-bridge/`, `services/unlock/`,
`services/passkey/`, `services/connect/crypto/`, or anything under `grpc/`. The redesign is
presentation and navigation; wallet logic, signing, recovery and crypto stay as they are.

## 4. Tokens

The artboards define these on `.tenshu` (dark) and `.tenshu.light`. Map them onto
`coincube-ui/src/theme/palette.rs`; the names are the prototype's, pick the crate's.

| Token | Dark | Light | Used for |
| --- | --- | --- | --- |
| bg | `#161716` | `#faf9f6` | window background |
| panel | `#202020` | `#ffffff` | top bar, cards, menus |
| rail | `#111211` | `#ede8e0` | side menu |
| ink | `#f5f0e8` | `#202020` | text |
| muted | `#b9bbb5` | `#575a54` | secondary text, captions |
| line | `#424242` | `#c4beb4` | borders, dividers |
| accent | `#f3821d` | `#f3821d` | primary buttons, selected tab underline |
| acc-ink | `#f59a45` | `#a14d00` | links, accent text on bg |
| selected | `#36291e` | `#f7e3cc` | selected nav item |
| good | `#8bdfab` | `#23613b` | confirmed, open, connected |
| warn | `#f3b465` | `#77450b` | backup not confirmed, reconnect |
| bad | `#ffad9b` | `#8f2e1d` | errors, Branta mismatch |
| focus | `#8fc1ff` | `#1d5fb8` | keyboard focus ring (2px, 2px offset) |
| teal | `#2a9d7f` | `#1f7a62` | Liquid / stablecoin accents |
| hover | `rgba(245,240,232,.06)` | `rgba(32,32,32,.05)` | hover fill |
| shadow | `rgba(0,0,0,.45)` | `rgba(60,40,10,.18)` | menus, popovers |

Type: IBM Plex Sans 400/500/600 for UI, IBM Plex Mono 400/500 for addresses,
txids, amounts in detail views. Base 14px / line-height 1.45; captions 12px;
tabular numerals everywhere amounts appear. Radii: 6px controls, 8px inputs and
switcher, 10px cards and menus, 16px chips. Side menu 248px; top bar 56px.
Reduced motion: every transition and the 3D floor spin are off under
`prefers-reduced-motion` and under the app preference.

## 5. Sequencing · one draft PR per step

Each step is independently reviewable and ships behind nothing; the app keeps
working after each. Acceptance is checked against the named `P-` artboard and the
review doc section in parentheses.

1. **Cube shell and side menu.** Identity block (avatar, name, network, chevron)
   as the Cube switcher with All Cubes / Lock Cube; top bar reduced to mark + chips;
   Settings vs Tools split in the nav. (P-Overview; review doc "Settings vs Tools",
   "Cube switcher moved into the side menu"). Acceptance: nav renders on every
   Cube screen, Lock returns to the Launcher with the Cube shown as Locked.
2. **Launcher.** Network tabs with "network" wording, avatar + state label per
   Cube, details face, PIN/passkey face, checks face, no Continue after Opening.
   (P-Launcher, P-Opening; review doc "Launcher sweep", "Opening auto-open").
3. **Overview hub and wallet pages.** Total balance, wallet cards, Spark page with
   recent activity, Lightning Address change, Stable Balance toggle; Vault page
   with the single Tools row. (P-Overview; review doc "Advanced/Spark/LN/Stable").
4. **Send and Receive.** Branta identity card, tampered-mismatch state, checks-off
   link; Receive with Bitcoin / Stablecoin master selectors in the Send-like
   layout. (P-Send, P-Receive; review doc "Branta").
5. **Settings.** Wallet & signers, Node, Pair with Keychain, Connections, Data &
   exports; Spark scope with Stable Balance; no "Cube service ready" badge.
   (P-Settings; review doc "Settings vs Tools decision and application").
6. **Tools.** Coin control with Details, PSBT proposals with Branta identity,
   Recovery path, Split coins; no Claim card. (P-Advanced, P-Split; review doc
   "Split vs Claim").
7. **Connect, Create, Restore.** Sign-in lead and eight-item benefits list, plan
   states, Create with three-word verification, Restore paths. (P-SignIn,
   P-Connect, P-Create, P-Restore).
8. **Activity, Transfer, Marketplace, Duress, Members, Signing, Border Wallet,
   Delete, App preferences, gap screens.** One PR per screen family.

## 6. Constraints that apply to every step

- COINCUBE workflow in the workspace `AGENTS.md`: task branch, draft PR, request
  reviews, repair confirmed findings, keep draft until gates pass. Robert merges.
- `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test` must pass; `make build` not bare `cargo build` (the Spark bridge is
  outside the workspace).
- Synthetic fixtures only in tests and screenshots; never real addresses, xpubs,
  descriptors, recovery words, emails or PINs.
- No change to unlock, passkey, signing, recovery, duress or Connect crypto paths.
- Keep both themes and reduced motion working on every screen you touch.
- Window minimum 1024×768 (the compact-layout boards are in
  `archive/explorations/`).
- Bitcoin Blake2b is a per-device toggle (App preferences › Developer). With it
  off, no Blake2b network, Cube, tab, card, copy or badge appears anywhere: not in
  the Launcher's network menu or Cube list, not in Create's network step, not in
  Transfer, Tools, Settings or Connect. Robert may later make this an account
  entitlement again; keep the gate in one place.
- Developer mode gates test networks the same way: off means no "Live networks" or "Test networks" chip on the Launcher ("All networks"
  shows only when more than one network is listed), no test network in its menu or Cube list, and
  Create's "Test networks" group shows only the pointer to App preferences.
  The former "Show test networks by default" preference is gone.
- The "Tenshu" mark at the top left of every screen is a button that returns
  to the Launcher without locking the open Cube.
- **Import Bitcoin wallet is a full import, not watch-only.** This is a
  behaviour change from today's `app/state/imported_bitcoin.rs` and the
  "Create a Cube for your Bitcoin wallet" step in `home.rs`, which import public
  descriptors only and spend through "Save PSBT file / Load signed PSBT / Sign
  with a connected device / Broadcast". In the redesign the user supplies the
  wallet's keys after the descriptor (recovery words, a hardware device, or a
  Keychain phone), each checked against its fingerprint, and the imported
  wallet then behaves like any wallet in the Cube with Tenshu signing. The
  PSBT round-trip page is gone; hardware-held keys sign through the normal Send
  flow as a Vault key does. Steps: Cube → Source → Keys → Review → Scan → Done
  (`P-Restore`, path "import"). Treat the current code as the thing to change,
  not as a constraint.
