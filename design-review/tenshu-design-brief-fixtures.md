# Tenshu design review — shared brief for artboard builders

You are building reviewable design artboards for **COINCUBE | Tenshu**, a native Rust/Iced desktop Bitcoin wallet (macOS/Windows/Linux). This is a DESIGN REVIEW before implementation. Do not touch any application code. Everything is a synthetic prototype. Never write real addresses, xpubs, descriptors, seed words, emails, IPs or billing records.

Company: COINCUBE only. Do not load or reference any other company's design system.

## Deliverable format (Claude Design canvas, `.dc.html` artboards)

Write each artboard as a file at `<ROOT>/project/<Name>.dc.html` (ROOT is given in your task). One artboard per file. Use the Write tool for each file (one Write per file, whole file). You do NOT write `canvas.json`; the lead does. Do not publish anything.

Skeleton (copy exactly; the `support.js` line must be exactly as shown):

```html
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Launcher · 3D floor</title>
<script src="./support.js"></script>
</head>
<body>
<x-dc>
<helmet>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link href="https://fonts.googleapis.com/css2?family=IBM+Plex+Sans:wght@400;500;600&family=IBM+Plex+Mono:wght@400;500&display=swap" rel="stylesheet">
<style>
body{margin:0}
.tenshu{--bg:#161716;--panel:#202020;--rail:#111211;--ink:#f5f0e8;--muted:#b9bbb5;--line:#424242;--accent:#f3821d;--selected:#36291e;--good:#8bdfab;--warn:#f3b465;--bad:#ffad9b;--focus:#8fc1ff}
.tenshu.light{--bg:#faf9f6;--panel:#ffffff;--rail:#ede8e0;--ink:#202020;--muted:#575a54;--line:#c4beb4;--selected:#f7e3cc;--good:#23613b;--warn:#77450b;--bad:#8f2e1d;--focus:#1d5fb8}
.tenshu :focus-visible{outline:2px solid var(--focus);outline-offset:2px}
@media (prefers-reduced-motion: reduce){.tenshu *{animation:none!important;transition:none!important}}
.tenshu.reduced *{animation:none!important;transition:none!important}
a{color:#f3821d}a:hover{color:#ffa54d}
</style>
</helmet>
<div class="tenshu {{themeClass}} {{motionClass}}" style="width:1440px;height:900px;box-sizing:border-box;overflow:hidden;font-family:'IBM Plex Sans',system-ui,sans-serif;font-size:14px;line-height:1.45;color:var(--ink);background:var(--bg);display:flex;flex-direction:column">
  ... markup ...
</div>
</x-dc>
<script type="text/x-dc" data-dc-script data-props='{"theme":{"editor":"enum","options":["dark","light"],"default":"dark","section":"Review"},"reducedMotion":{"editor":"boolean","default":false,"section":"Review"},"$preview":{"width":1440,"height":900}}'>
class Component extends DCLogic {
  renderVals() {
    const theme = this.props.theme ?? 'dark';
    return {
      themeClass: theme === 'light' ? 'light' : '',
      motionClass: this.props.reducedMotion ? 'reduced' : ''
    };
  }
}
</script>
</body>
</html>
```

Format rules that fail silently if broken:
- Root element is FIXED size (desktop window mockup): 1440×900 or 1024×768 as your task says. `$preview` must match. Internal regions scroll with `overflow:auto`; the root never scrolls.
- `{{hole}}` is a dotted lookup into `renderVals()` ONLY — never an expression. Compute everything in `renderVals()`.
- Events: JSX camelCase whole-value attrs: `onClick="{{ pick }}"` where `pick` is a function from `renderVals()`. For per-item handlers map them onto the items in `renderVals()` and bind `onClick="{{ item.pick }}"` inside `<sc-for>`.
- Loops: `<sc-for list="{{ items }}" as="item" hint-placeholder-count="4"> … {{ item.name }} … {{ $index }} … </sc-for>`.
- Branches: `<sc-if value="{{ cond }}" hint-placeholder-val="{{ true }}"> … </sc-if>`. There is no else; use a second `<sc-if>` with the negated flag computed in `renderVals()`.
- Conditional styling in a loop: precompute per-item class/style strings in `renderVals()` and bind `class="row {{ item.cls }}"` or `style="{{ item.style }}"`. Style holes are fine for live state and declared tweak props.
- Logic: classic JS only (no imports, no TypeScript), `class Component extends DCLogic`, use `this.state` / `this.setState`. Initialize state in the constructor: `constructor(p){super(p);this.state={count:4}}`.
- All UI is markup inside `<x-dc>`. No `innerHTML`, no script-built DOM, no `<iframe>`, no global keydown handlers, no network except the Google Fonts link. No emoji; icons are small inline stroke `<svg aria-hidden="true">`.
- Accessible as drawn: real `<button type="button">`, `<a href>`, `<input>`+`<label>`; `aria-pressed` on toggles, `aria-current="page"` on the active nav item, `aria-label` on icon-only buttons. Never `role`/`onClick` on a div. Text contrast 4.5:1.
- `data-props` is single-quoted JSON on the script tag; escape `&` as `&amp;` inside it. Declare FEW tweaks: theme, reducedMotion, plus the review levers your task names (cube count, review state, window size). Copy is literal markup, not props.
- Links between artboards: `<a href="Cube-Workspace.dc.html">` (sibling file name). Style the `<a>` itself as the button.
- Close every element, quote every attribute. Keep each file self-contained; no shared CSS files.

## Visual language (not a rebrand)

- Typeface: IBM Plex Sans for all functional text; IBM Plex Mono only for fingerprints/short identifiers.
- Colors: the CSS variables above. Orange `#F3821D` is reserved for primary actions, selection and meaningful state. Dark neutral surfaces by default; warm light theme as second mode (same hierarchy).
- No decorative gradients, glass, ornamental charts, generic card grids. Reduce repeated container chrome: use section rules (1px `var(--line)` dividers) and aligned rows instead of a rounded box for every row. Comfortable desktop density: 14px body, 13px secondary, 12px captions, 20–24px section headings, 4/8/12/16/24 spacing.
- Status is carried by TEXT plus a stable badge/icon; color is secondary and must differ in lightness. Never only an aura/glow. Never multiple simultaneous pulsing warnings.
- Focus ring: 2px `var(--focus)`. Show at least one element in keyboard-focus state where the task asks.

## Product truth (do not contradict)

- A **Cube** is an encrypted container. It may contain a **Vault** (multisig with signing policies and time-locked recovery paths), a **Spark** wallet (Lightning via Breez Spark SDK; supported on Bitcoin mainnet and regtest only) and an eligible **Liquid** wallet. Always keep active Cube, wallet and network identity explicit.
- **Backing up a Cube** (seed phrase; Connect Recovery Kit) and **spending through a Vault recovery path** (time-locked) are different jobs. Never imply a backup unlocks a time lock. Use the words "Backup & protection" for the former and "Recovery path" for the latter.
- **Connect** supplies account-backed services. Three separate states: (1) account authentication (signed in / signed out), (2) this Cube's service connection/registration (connected / stale / reconnect required / not registered), (3) verified feature entitlement (verified / unknown / denied). Never claim an upgrade is required while entitlement is unknown.
- **Expert tools**: coin control, PSBT proposals, descriptors, key details, node resources, maintenance/rescan. Reachable through one stable named "Expert" entry; never peers of Send/Receive.
- **Stable Balance** is a device preference that does not travel with the seed. A restored Spark wallet can hold USDB while this device's preference is off: explain and offer review; never silently enable conversion; never promise a guaranteed peg.
- **Pair with Keychain** (phone) pairs an eligible signer key and Vault over the same Wi-Fi WITHOUT the Connect API. Do not invent a cloud/local transport toggle; do not require Connect sign-in for the local flow.
- **Liquid** has no Settings entry. Shared seed protection is at Cube scope. **Duress**, **Members**, **Marketplace** are feature/eligibility gated: show gates, never enabled controls.
- Networks (distinct identities): Bitcoin mainnet; Bitcoin Blake2b mainnet (a separate live chain; "Bitcoin Blake2b supports Vault creation only"; access is account-gated behind a "BTCB2 beta" preference); test networks: Testnet4, Signet, Regtest, and Bitcoin Blake2b Testnet4. Both mainnets count as "Live chains". Do NOT invent a Blake2b logo, ticker or network parameters: mark Bitcoin with a plain ₿ mark and label Blake2b with a text badge "Blake2b" next to the ₿ mark. Test networks always carry an explicit text badge (e.g. "Testnet4"), never just a color.
- Launcher catalog shows only metadata safe before unlock: name, chain/network, lock/open state, protection/capability summary (e.g. "Backup confirmed", "Backup not confirmed", "Backup status unknown", "No Vault configured · Spark only"). Never balances. "No Vault configured" is neutral capability info, not an alarm. "Backup not confirmed" is actionable (warn tone). "Unknown/stale" is distinct from verified.
- Filters change catalog visibility only. They never change a Cube's chain, combine balances, reuse sessions across chains, bypass Blake2b admission or pick the chain for creation. Creation selects chain explicitly. Import/restore verifies the file/network before opening.
- Opening a Cube goes through the existing PIN/passkey and network-specific loading/admission flow. Animation must not delay or fake security checks.
- Returning to the Launcher ("All Cubes") never locks or closes open Cube tabs.

## Synthetic fixtures (use exactly these; label policy as synthetic)

Cubes (catalog order):
1. **Everyday** — Bitcoin mainnet — open — Backup confirmed — Vault + Spark + Liquid
2. **Blake workspace** — Bitcoin Blake2b mainnet — locked — Backup not confirmed — Vault only
3. **Lab** — Testnet4 — locked — Backup status unknown — Vault only (Spark unavailable on this network)
4. **Archive** — Bitcoin mainnet — locked — Backup confirmed — No Vault configured · Spark only
For the 20-Cube state add synthetic extras: "Household 2024", "Travel fund", "Signet scratch" (Signet), "Regtest bench" (Regtest), "Blake test" (Bitcoin Blake2b Testnet4), "Imported Vault (watch)" (imported Vault, no local keys), "A very long Cube name that keeps going to test wrapping behaviour" (Bitcoin mainnet), "Team treasury", "Grandparents", "Donations", "Side project", "Cold archive 2", "Lab 2", "Lab 3", "Emergency", "Shop till" — mixed chains, mostly Bitcoin mainnet; mark at least 5 as test networks and 2 as Blake2b.

Inside the **Everyday** Cube: wallets are **Spark · Daily spending**, **Vault · Family savings**, **Liquid · Travel**. Vault policy: "2-of-3 synthetic policy" with signers "This device", "Keychain · Alex's phone", "Hardware · Ledger-style device" (aliases only, fingerprints shown as `a1b2c3d4` style synthetic mono). Recovery path: "after 180 days without spending, 1-of-2 recovery keys" (synthetic). Connect account: **alex@example.test**. Devices: "This Mac (current)", "Windows laptop", "Linux desktop". Plan: "Current plan: [PLAN NAME] · Billing status: verified" — never invent prices or renewal dates; use placeholders like [RENEWAL DATE].

Review states (where your task asks for a state lever): normal, loading, signed out, authenticated account but disconnected Cube, offline node, incomplete backup, no Vault, imported Vault, Testnet (Spark unavailable), long names, save failure.

## Problems this design must visibly fix
1. Three icon rails with equal emphasis; Vault Settings below the fold at small windows; scrolling the sidebar moved primary Cube navigation out of view. → One readable sidebar; the sidebar's identity+primary nav never scrolls away; only the middle section scrolls; Settings and Connect account anchored at the foot.
2. Recovery Kit shortcut landed on General. → Lands on Backup & protection › Recovery Kit.
3. Backup and recovery-path spending both called "Recovery". → Renamed as above.
4. Wallet Settings led with a raw descriptor; node health mixed with logs/paths/tuning/rescan. → Identity/policy/aliases first; descriptor under Expert disclosure; Connections leads with health, diagnostics disclosed, maintenance separate.
5. Two large warnings above unrelated screens. → One compact persistent attention row in the shell, contextual detail on the relevant page.
6. Billing led with sales comparisons; device lists spread details across wide rows. → Identity + verified status first, comparison secondary; compact device rows.

## Concept C structure (the anchor inside an open Cube)
Header: Tenshu mark · Cube switcher (name + network badge, dropdown of open Cube tabs + "All Cubes") · right: account state ("alex@example.test · Signed in" / "Signed out") and Cube connection state as two separate labeled chips.
Sidebar (one text hierarchy, ~232px): Overview · Activity · Backup & protection; group "Wallets": Spark · Daily spending / Vault · Family savings / Liquid · Travel; group "Expert" (collapsed by default, explicit disclosure): Coin control, PSBT proposals, Descriptors & keys, Node resources, Maintenance; foot (anchored): Settings, Connect account. Compact attention row above the foot: "Backup not confirmed · Review" when applicable.
Settings page: heading with explicit scope ("Settings · Cube · Everyday"), a scope selector (Cube · Everyday / This device / Connect account / Vault · Family savings / Spark · Daily spending), and the sections for that scope shown as content tabs; at 1024 width the tabs become a compact section selector (select-like). No tertiary rail.
Sections (11): Display & appearance; Backup & protection; Wallet & signers; Pair with Keychain; Payment checks; Connections; Spark; Data & exports; Account & plan; Devices & security; About & app preferences.
