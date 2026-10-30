# Tenshu redesign · implementation hand-off

This folder is the complete hand-off for implementing the Tenshu redesign in this
repository. It was produced in a Claude Design review with Robert on 2026-10-22/23.
Read this file first, then the review doc, then the inventory.

| What | Where |
| --- | --- |
| Live prototype (clickable, needs a signed-in claude.ai browser) | https://claude.ai/artifact/4DkSWQshpyXVu2kVrbneRz (canvas v175; the sources below were exported from it on 2026-10-30, artifact version `1793315838-41d9`) |
| Exported screenshots of every screen, dark and light (open without a browser session) | `screens/dark/`, `screens/light/`, `screens/studies/`, index in `screens/README.md` (canvas v175) |
| Artboard sources (canvas v175: the linked `P-*` prototype and its light twins, plus the v175 studies `H-*` page head, `G-*` ghost Cubes and `O-*` opening, and the `P-*-Rail` Launcher backdrop boards; plain HTML + CSS + a small script each) | `canvas/*.dc.html`, index and canvas pages in `canvas/canvas.json` |
| Earlier exploration boards the prototype was distilled from (superseded) | `archive/explorations/` |
| Every decision Robert made, in order, with reasons | `tenshu-launcher-and-settings-review.md` |
| Feature coverage matrix (88 rows: existing feature → screen) | `tenshu-feature-inventory.md` |
| Product frame (users, platform, constraints) | `PRODUCT.md` |
| Design briefs v1–v6 (history; superseded where the review doc disagrees) | `tenshu-design-brief-*.md`, `tenshu-concepts.md` |
| Pre-canvas concept pages (superseded) | `archive/` |

Precedence when sources disagree: review doc > artboard source > brief.

## v175 delta (2026-10-29)

On 2026-10-29 Robert reviewed a page-header study and had it folded into the
prototype across every Cube board (canvas v125 → v175). The approved plan for
that delta is `company-brain/plans/tenshu-cube-v175/` (master plan and desktop
plan). Delivery is tracked in #950. Where the plan touches a screen, it
supersedes the v124 artboard and the §5 step for that screen (master §3
decision 4). The §5 steps the delta does not touch stay as written. The `P-`
boards are the acceptance target, in both themes at 1024×768 and 1440×900. The
`H-*` page-header study is the reference they were built from.

The v175 sources are in `canvas/` (exported on 2026-10-30, #955), and
`screens/` has a local render of every board's default state (#957). Where a
render disagrees with the live canvas, the canvas wins. Each board that changed
names its decision in a dated `(Robert, 2026-10-29)` comment in its CSS. The
renders are at canvas size (mostly 1440 px wide), so check 1024×768 against the
source. States behind a lever, such as the Swap Review and Swapped steps or the
auto-convert turn-off confirmation on `P-Settings`, are only in the source and
the live canvas. `screens/studies/H-Convert-Review.png` and
`screens/studies/H-Settings-Spark-Off.png` show their study versions.

| Area | v175 rule | Boards | Plan step |
| --- | --- | --- | --- |
| Top bar | One bar on every screen, Launcher and Cube: 64px, rail colour, 24px side padding, a 1px rule below. On the right: the borderless 32px hide-balances eye first, then the status chips, then App preferences as a borderless sliders icon with an "App preferences" tooltip. The gear stays inside Cube Settings. On the Launcher the only chip is the Connect account entry. The Cube service chip opens Settings › Vault › Node. This replaces the 56px bar in §4. | the `P-` Launcher and Cube boards, `H-*` | L1 |
| Page head | Pinned, as on Activity: a 22px title and the Cube · network line on the left, page actions on the right, tabs or steps on a rule across the content pane. The page scrolls under it. Send, Receive and Swap end the actions with "Back to ‹wallet›"; Transfer ends them with "Back to Overview". | `H-*` and every `P-` Cube page | L3, W4–W6 |
| Side panel | Labels start right after the icon. Wallet rows read "Spark · ‹nickname›" and "Vault · ‹nickname›". The Spark icon is the stroked asterisk. Selection never paints a focus ring; a row reached by keyboard still does (master I9). | every `P-` Cube board | L2 |
| Overview | Total balance, two wallet cards side by side, Transfer as a head action, and Recent activity under day headings with a wallet column. | `P-Overview`, `H-Overview` | W1 |
| Wallet pages | Spark and Vault share one shape: Total balance, one row per asset, Recent activity. Spark's head actions are Swap BTC ⇄ USDB and Wallet settings. The Vault's are Tools and Wallet settings, and it adds a protection strip (recovery path, PSBTs waiting). | `P-Overview` (wallet pages), `H-Wallet`, `H-Vault` | W2, W3 |
| Settings | The Cube / Vault / Spark scope switch is a segmented control right of the title, also on the Connect-account gap screens and Members. On/off settings are switches; segmented controls are for real choices. Spark tabs: Balances & conversion · Lightning address · Connections (`H-Settings-Spark*` show an earlier set with Payment checks and Advanced; `P-Settings` wins). | `P-Settings`, `H-Settings*`, `P-GapsAccount`, `P-Members` | W5 |
| Spark USDB | A separate US Dollar (USDB) balance. Swap BTC ⇄ USDB in three steps (Details, Review, Swapped): an indicative rate, then Get quote, then the fee and the 0.5% max price change. Auto-convert lives in Spark Settings. Send has an explicit pay-from choice. Receive says payments arrive as bitcoin. | `P-Swap`, `H-Convert`, `H-Convert-Review`, `H-Settings-Spark`, `H-Settings-Spark-Off`, `H-Send-From`, `P-Receive` | S1–S4 |
| Launcher | Ghost Cubes on the floor (see-through, dashed footprint, Recovery Kit state on the edges) with a details face. Opening on the Cube: it tumbles to its bottom face and grows to a 700px face whose edge glow brightens with each loader stage, then the Flash finale opens the Overview. | `G-Launcher`, `G-Ghost-Details`, `O-*` (Flash is `O-Finale-B`) | N1, N2 |

Where the plan departs from the boards, the plan wins:

- **Turning auto-convert off keeps USDB.** Robert decided this on 2026-10-30
  (#950): existing USDB stays USDB and only future receive conversion stops,
  as master §3 decision 5 and plan S2 say, with the confirmation "Your USDB
  stays USDB, and new bitcoin you receive stays bitcoin." The v175 boards'
  turn-off copy is stale. `P-Settings` asks "Turn off auto-convert?", says the
  whole USDB balance converts back to bitcoin at the current rate, and
  confirms with "Turn off and convert". `H-Settings-Spark-Off` is titled
  "turning auto-convert off converts the USDB balance back", and its render
  (`screens/studies/H-Settings-Spark-Off.png`) shows that dialog. Build
  neither the convert-back copy nor the conversion. The board's "stays USDB"
  status note, shown while auto-convert is off, still applies.
- **Liquid is hidden.** Most Cube boards still show a Liquid wallet (a side
  panel row, an Overview card, activity rows), and `P-Liquid` is still on the
  canvas. Build without them, for every account and Cube (master §3 decision 2,
  plan L2). Key derivation is untouched.
- **Swap copy.** `P-Swap` Review says "Quoted rate". Flashnet prices are not
  locked, so the app says "estimated", and the 0.5% cap is enforced in every
  request (master I5). Swap Review also says that conversions run through
  Flashnet, a third-party AMM, that a failed swap is refunded and shows as its
  own row, and that USDB is an issued token, not bitcoin (master I6). The boards
  do not carry that disclosure.
- **Spark nickname.** "Daily spending" on the boards is a fixture. Spark gets
  an optional per-Cube nickname, edited in Balances & conversion; without one
  the row reads plain "Spark" (master §3 decision 6).
- **Page column.** The boards still declare the left-anchored 1120px column.
  Keep the centred 960px column in §4 (master I7, #926).
- **USDB logo.** The boards use the logo from Flashnet's token list. Confirm its
  licence or use a COINCUBE-drawn mark (plan W2).

Already in the app and not changed by the plan: the Launcher's network activity
(canvas page "Launcher backdrop (design B)": `P-Launcher-Rail*`,
`P-AppPrefs-Rail` and the `backdrop` lever on `P-Launcher`) is
`home/launcher_activity.rs` with its App preferences row. Connect-only Cubes
already stand on the floor as ghosts (#943, #949); N1 is checked against the
`G-` boards.

Tokens: the §4 table is unchanged at v175. The boards add `--ghost-btc`,
`--ghost-b2b`, `--ghost-test` and `--ghost-fill` for ghost Cubes, and settle
`--on-accent` (`#1a1208`, text on the accent fill) and `--b2b` (`#d4a62a` dark,
`#8a6a10` light), which v124 boards set inconsistently.

Track S changes wallet behaviour by design, so the §3 rule against touching
`coincube-spark-bridge/` does not apply to it (master invariant I1). Every other
path in that rule stays off-limits.

## 1. How to read the prototype

- Each `.dc.html` is one artboard. Open it in an editor: the `<helmet>` block holds
  the screen's CSS, the markup follows, and the `<script data-dc-script>` holds the
  per-screen state. `data-props` on that script lists the review "levers" (the
  states Robert reviewed). `{{ holes }}` are filled from `renderVals()`.
- The files reference `./support.js` and the Claude Design runtime, which are not
  in this repo, so they do not open on their own in a browser.
  `screens/render/` has a local stand-in that renders their default states
  (see `screens/README.md`). Use `screens/` (or the live link)
  for pixels and the files for exact copy, spacing, colours and state logic. Each
  screenshot shows the board's default state with the device settings at their
  defaults (Dark, Developer mode off, Bitcoin Blake2b off, Launcher style Cubes);
  other states are levers in the source.
- The canvas has six pages (`pages` in `canvas.json`): the clickable prototype,
  its forced-light twin, the Launcher backdrop (design B), the page-header study
  (`H-*`), Connect Cubes as ghosts (`G-*`) and Opening on the Cube (`O-*`). The
  prototype page has seven rows: Launcher & account; Create, restore, delete;
  Inside a Cube; Vault tools & signing; Settings & protection; Marketplace; AI
  access. Its links stay inside the prototype. The earlier exploration boards
  live in `archive/explorations/` and are not current.
- Inside the live canvas, the per-screen "play" button opens that screen
  full-frame with working links. Prefer it over scrolling the canvas.

## 2. Prototype mechanics that are NOT product behaviour

Implement none of these. They exist only so the prototype could be clicked through.

| In the prototype | Why it is there | In the app |
| --- | --- | --- |
| `localStorage` keys `tenshu.proto.open` and `tenshu.proto.seen` | Share "which Cube is open" across artboards | In-memory app state (`gui/tab.rs::State`, `app/session.rs`) |
| `localStorage` key `tenshu.proto.b2b` | Share the Bitcoin Blake2b device toggle across artboards | A persisted per-device app setting (next to Developer mode in `app/view/settings/general.rs`); when off, nothing Blake2b is rendered anywhere |
| `localStorage` key `tenshu.proto.dev` | Share the Developer-mode toggle across artboards | The existing Developer-mode device setting; when off, no test network is rendered anywhere (no Live/Test chips, "All networks" only with two or more networks listed, no test networks in the Launcher menu or Cube list, none in Create) |
| v175 `localStorage` hand-offs `tenshu.proto.land`, `settingsScope`, `sendFrom`, `recvTo`, `openCube` and `restored`, and the `tenshu-proto` window event | Tell the next artboard where to land (wallet page, Settings scope and tab, Send/Receive wallet and asset, which Cube opened, which ghosts were restored) | Route arguments and in-memory state (`app/menu.rs::Menu`, `gui/tab.rs::State`) |
| v175 `localStorage` keys `tenshu.proto.theme`, `launcher`, `backdrop` and `chainsrc` | Share App preferences (Appearance, Launcher style, network activity and its source) across artboards | The existing persisted device settings |
| `kf` focus demos and the `focusDemo` lever | Show the keyboard focus ring in a still frame | Real keyboard focus |
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
| Spark page (v175: the Spark page in P-Overview, H-Wallet): Total balance, one row per asset (Bitcoin, US Dollar), Recent activity, Lightning Address block; head actions Swap BTC ⇄ USDB and Wallet settings. The Stable Balance card and "Stable" badge leave the page; the control moves to Settings as auto-convert | `app/view/spark/overview.rs`, `app/state/spark/overview.rs`, `app/state/connect/cube.rs` (Lightning Address claim), `coincube-ui/src/component/tenshu/asset_row.rs`, `coincube-ui/src/image.rs` (`asset_network_logo`) and `coincube-ui/static/logos/` (the USDB logo) |
| Swap BTC ⇄ USDB (v175: P-Swap, H-Convert, H-Convert-Review): Details, Review, Swapped | New `app/state/spark/swap.rs` and `app/view/spark/swap.rs`, with `Menu::Spark(SparkSubMenu::Swap)` in `app/menu.rs`. The bridge requests live in `coincube-spark-protocol` and `coincube-spark-bridge` (Track S) |
| Spark Settings (v175: P-Settings, Spark scope; H-Settings-Spark, H-Settings-Spark-Off): tabs Balances & conversion · Lightning address · Connections. Balances & conversion holds the Auto-convert switch and its turn-off confirmation (USDB stays USDB; see the v175 delta), "Pay from USDB when bitcoin runs short · Always on", Max price change 0.5% (read-only), Swap by hand, and the Spark nickname (plan only, not on the boards) | `app/view/spark/settings/**`, `app/state/spark/settings.rs`, `app/view/settings/frame.rs` (scope switch and tabs), `app/settings/mod.rs` (the nickname, next to `WalletSettings.alias`) |
| Settings = configuration (P-Settings: Wallet & signers, Node, Pair with Keychain, Connections, Data & exports; Spark scope) | `app/view/settings/**`, `app/state/settings/**`, `app/settings/mod.rs` (`CubeSettings`), `app/view/vault/settings/**`, `app/view/spark/settings/**`, `node/**` |
| Tools = operations (P-Advanced: Coin control, PSBT proposals, Recovery path, Split coins) | `app/view/vault/coins.rs`, `psbts.rs`, `psbt.rs`, `recovery.rs`, `split.rs`, `claim.rs` |
| Transfer to another Cube (P-Transfer) | `app/view/vault/spend/**` + Launcher unlock-in-flow |
| Marketplace Buy/Sell, P2P (P-BuySell, P-P2P) | `app/view/buysell/**`, `app/view/p2p/**`, `app/state/buysell.rs` |
| Duress, Members, Border Wallet, Delete, Signing (P-Duress, P-Members, P-BorderWallet, P-Delete, P-Signing) | `app/view/duress/**`, `app/view/connect/cube_members.rs`, `app/view/settings/local_signing.rs`, `app/view/vault/hw.rs`, `phone_signer/**` |
| App preferences (P-AppPrefs), incl. the Developer-mode and Bitcoin Blake2b device toggles | `app/view/settings/general.rs`, `app/view/settings/about.rs`, `feature_flags.rs` (Blake2b is a runtime device setting now, not a Connect entitlement) |
| AI access: paired clients, pairing, permissions, revoke (P-AI-Access, entry from Cube › Settings › AI access) | `app/local_mcp.rs` and the `coincube-mcp` crate on the `mcp` branch (today: one-hour grants, read capabilities, copied config). Planned: pairing that persists until revoked, scoped to Cubes/wallets, with three permissions (read · manage setup · prepare transactions). Keep the credential out of chat; keep the provider disclosure. |
| AI-led wallet setup progress, handoffs, final native review (P-AI-Setup) | `installer/proposal.rs` and `installer/**` (today: experimental Regtest proposal → native key selection → exact-state review digest). Planned: the assistant orchestrates; Tenshu owns device actions, PIN/passkey, backup, the final review and admission. |
| AI-prepared transaction draft and review (P-AI-Transaction) | New: a preparation capability over `coincube-core::spend` and the daemon's coin selection and fee estimation, with Tenshu's exchange rate; signing stays in `app/view/vault/spend/**` and the existing signers. No autonomous spending. |
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

### Page layout (Cube screens)

Every screen inside the Cube shell uses one page container: one column, the
same width on every page, centred in the content area. Robert chose this on
2026-10-28 over the earlier left-anchored layout, which left large monitors
empty on the right.

| Rule | Value |
| --- | --- |
| Anchor | Centred. Below the page width plus two gutters the column fills the space between the gutters, so it starts one gutter from the side menu as before. |
| Gutter | `clamp(24px, 3vw, 48px)` on both sides (30.7px at the 1024 minimum window, 43.2px at 1440, 48px from 1600). In the app: `(0.03 × window width).clamp(24, 48)`. |
| Top padding | 28px above the page title on every page; 48px bottom padding. |
| Page width | Content is at most 960px wide on every page, Activity included. 960 is what Send and Receive need for their 680px form and 240px step column side by side. |
| Activity | The title, filters and table sit on the page column. The detail panel (420px) sits at the content area's right edge; the column stays where it is while it clears the panel by a gutter, then slides left, then narrows once it reaches the left gutter, so it never jumps as the window is resized. |
| Page header | The same on every page: title, then the Cube and network line. A right-side item (Overview's network line, Activity's result count) sits at the container's right edge. |

Inner layouts sit inside this container and start at its left edge. For example,
Send and Receive keep their 680px form and 240px step column, with no centering
of their own.
In the artboards the rule is the "page shell" block at the end of each
`<helmet><style>`: `--gutter` and `--page-max` on `.tenshu`, applied to `.main .pg`
(Activity: `.phd`, `.filters`, `.lst`). In the app this is `coincube_ui::component::tenshu::page`, wrapped by the
Cube shell (`coincube-gui/src/app/view/mod.rs`: `dashboard`, and
`dashboard_activity`, which applies the same column in `view::activity`). Do not
give a screen its own side padding, page `max_width` or centering. The artboards,
v175 included, still declare the left-anchored 1120px rule
(`--page-max:1120px`, `.main .pg{margin:0}`); follow this section, not the
artboards, for the column.

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
9. **AI access (on the `mcp` feature branch, not master).** Persistent pairing
   and client management first (P-AI-Access), then AI-led setup orchestration
   on top of the existing proposal/native-review path (P-AI-Setup), then
   transaction preparation (P-AI-Transaction). Each screen carries Today /
   Planned labels; the review doc section "AI access revised" has the exact
   states and the current-vs-planned boundary.

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
- Every Cube screen uses the shared page container (§4, "Page layout"): one
  centred 960px column. No screen centers or pads its own column.
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
