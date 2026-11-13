# Exported screens

PNG renders of every board on the Tenshu canvas v175 (artifact version
`1793315838-41d9`, exported 2026-10-30), so the design can be read without a
claude.ai session. Pixels only: exact copy, spacing, tokens and state logic are
in `../canvas/<source>`.

- `dark/` is the "Prototype (clickable)" canvas page with the device settings at
  their defaults: Appearance Dark, Developer mode off, Bitcoin Blake2b off,
  Launcher style Cubes.
- `light/` is the "Light theme" canvas page: the same screens mounted with
  `force-theme=light`, named after the dark board they mount, plus the
  Launcher's List view (`P-Launcher-List.png`).
- `studies/` holds the v175 study pages (Launcher backdrop, page header, ghost
  Cubes, opening on the Cube). Their decisions are folded into the prototype; the
  studies stay for the reasoning and the states they show.
- Each image is the board's default state at its canvas size, not scaled
  (1440 × 900, 980 or 1100 px; `G-Ghost-Details` is 1120 × 620). Other states are levers in the source
  (`data-props`). Animated boards (`P-Opening`, the `O-*` finale and glow
  boards) are caught 2.5 s after they mount; the motion itself is only in the
  live canvas.
- All data is synthetic.

## How these were made

`render/` renders the board sources locally, without the Claude Design runtime:
`render/support.js` is a small stand-in for the runtime's template features
(`{{ holes }}`, `sc-if`, `sc-for`, `dc-import`, `DCLogic` state), and
`render/_blob/` holds the three images the boards reference. To regenerate after
the canvas sources change:

```bash
NODE_PATH=<dir containing playwright-core> node design-review/screens/render/capture.js
```

It needs Google Chrome (or `CHROME=<path>`). Pass board names to render only
those. The stand-in ignores event handlers, so it shows default states only. If a
render disagrees with the live canvas, the live canvas wins.

## Prototype, dark

| Image | Board | Source |
| --- | --- | --- |
| `dark/P-Launcher.png` | 1 · Launcher (Open → PIN → checks → Continue) | `P-Launcher.dc.html` |
| `dark/P-Unlock.png` | PIN / passkey unlock (standalone) | `P-Unlock.dc.html` |
| `dark/P-Opening.png` | Opening Cube (after checks / create / restore) | `P-Opening.dc.html` |
| `dark/P-Locking.png` | Locking Cube (hand-off → Launcher) | `P-Locking.dc.html` |
| `dark/P-AppPrefs.png` | App preferences (from the Launcher) | `P-AppPrefs.dc.html` |
| `dark/P-Connect.png` | Connect (from the Launcher account entry) | `P-Connect.dc.html` |
| `dark/P-SignIn.png` | Connect sign-in / sign-up | `P-SignIn.dc.html` |
| `dark/P-Create.png` | Create Cube (from the Launcher) | `P-Create.dc.html` |
| `dark/P-Restore.png` | Import or restore (from the Launcher) | `P-Restore.dc.html` |
| `dark/P-GapsInstaller.png` | Installer gap screens (from Create) | `P-GapsInstaller.dc.html` |
| `dark/P-BorderWallet.png` | Border Wallet | `P-BorderWallet.dc.html` |
| `dark/P-Delete.png` | Delete Cube | `P-Delete.dc.html` |
| `dark/P-Overview.png` | 2 · Overview hub (wallet pages, Send/Receive, Transfer, Tools) | `P-Overview.dc.html` |
| `dark/P-Activity.png` | Activity (sidebar) | `P-Activity.dc.html` |
| `dark/P-Send.png` | Send (from any Send pill) | `P-Send.dc.html` |
| `dark/P-Receive.png` | Receive(Bitcoin / Stablecoin selectors) | `P-Receive.dc.html` |
| `dark/P-Transfer.png` | Transfer (from the Transfer pill) | `P-Transfer.dc.html` |
| `dark/P-Swap.png` | Swap BTC ⇄ USDB (from the Spark page head) · Details, Review, Swapped | `P-Swap.dc.html` |
| `dark/P-Liquid.png` | Liquid wallet | `P-Liquid.dc.html` |
| `dark/P-Advanced.png` | Vault › Tools (from the Vault page) | `P-Advanced.dc.html` |
| `dark/P-Split.png` | Bitcoin Blake2b copy wizard (from Tools › Split coins) | `P-Split.dc.html` |
| `dark/P-Advanced-B2B.png` | Blake2b copy › Tools · Coin control and Split coins from Bitcoin Blake2b | `P-Advanced-B2B.dc.html` |
| `dark/P-Claim.png` | Claim BTCB2 (from Tools) | `P-Claim.dc.html` |
| `dark/P-Signing.png` | Signing, pairing, labels, export | `P-Signing.dc.html` |
| `dark/P-Settings.png` | Settings (sidebar foot) | `P-Settings.dc.html` |
| `dark/P-Members.png` | Members (from Settings) | `P-Members.dc.html` |
| `dark/P-Duress.png` | Duress (from Connect) | `P-Duress.dc.html` |
| `dark/P-GapsAccount.png` | Account gap screens (from Settings) | `P-GapsAccount.dc.html` |
| `dark/P-GapsWallets.png` | Wallet gap screens (from Tools / Settings) | `P-GapsWallets.dc.html` |
| `dark/P-GapsDetails.png` | Detail gap screens (from Settings) | `P-GapsDetails.dc.html` |
| `dark/P-BuySell.png` | Marketplace · Buy/Sell | `P-BuySell.dc.html` |
| `dark/P-P2P.png` | Marketplace · P2P | `P-P2P.dc.html` |
| `dark/P-AI-Access.png` | AI access · paired clients, pairing, permissions, revoke (from Settings) | `P-AI-Access.dc.html` |
| `dark/P-AI-Setup.png` | AI-led wallet setup · handoffs, states, final review | `P-AI-Setup.dc.html` |
| `dark/P-AI-Transaction.png` | AI-prepared transaction · draft, review, signing states, failures | `P-AI-Transaction.dc.html` |
| `dark/P-AppPrefs-InCube.png` | App preferences · over an open Cube (from the Cube top bar) | `P-AppPrefs-InCube.dc.html` |
| `dark/P-Connect-InCube.png` | Connect · over an open Cube (from the Cube top bar) | `P-Connect-InCube.dc.html` |

## Light theme

| Image | Board | Source |
| --- | --- | --- |
| `light/P-Launcher.png` | Launcher · light · Cubes | `P-Launcher-Light.dc.html` |
| `light/P-Launcher-List.png` | Launcher · light · List | `P-Launcher-Light-List.dc.html` |
| `light/P-Unlock.png` | PIN / passkey unlock (standalone) · light | `P-Light-Unlock.dc.html` |
| `light/P-Opening.png` | Opening Cube (after checks / create / restore) · light | `P-Light-Opening.dc.html` |
| `light/P-AppPrefs.png` | App preferences (from the Launcher) · light | `P-Light-AppPrefs.dc.html` |
| `light/P-Connect.png` | Connect (from the Launcher account entry) · light | `P-Light-Connect.dc.html` |
| `light/P-SignIn.png` | Connect sign-in / sign-up · light | `P-Light-SignIn.dc.html` |
| `light/P-Create.png` | Create Cube (from the Launcher) · light | `P-Light-Create.dc.html` |
| `light/P-Restore.png` | Import or restore (from the Launcher) · light | `P-Light-Restore.dc.html` |
| `light/P-GapsInstaller.png` | Installer gap screens (from Create) · light | `P-Light-GapsInstaller.dc.html` |
| `light/P-BorderWallet.png` | Border Wallet · light | `P-Light-BorderWallet.dc.html` |
| `light/P-Delete.png` | Delete Cube · light | `P-Light-Delete.dc.html` |
| `light/P-Overview.png` | Overview hub (wallet pages, Send/Receive, Transfer, Tools) · light | `P-Light-Overview.dc.html` |
| `light/P-Activity.png` | Activity (sidebar) · light | `P-Light-Activity.dc.html` |
| `light/P-Send.png` | Send (from any Send pill) · light | `P-Light-Send.dc.html` |
| `light/P-Receive.png` | Receive(Bitcoin / Stablecoin selectors) · light | `P-Light-Receive.dc.html` |
| `light/P-Transfer.png` | Transfer (from the Transfer pill) · light | `P-Light-Transfer.dc.html` |
| `light/P-Swap.png` | Swap BTC ⇄ USDB (from the Spark page head) · light | `P-Light-Swap.dc.html` |
| `light/P-Liquid.png` | Liquid wallet · light | `P-Light-Liquid.dc.html` |
| `light/P-Advanced.png` | Vault › Tools (from the Vault page) · light | `P-Light-Advanced.dc.html` |
| `light/P-Split.png` | Bitcoin Blake2b copy wizard (from Tools › Split coins) · light | `P-Light-Split.dc.html` |
| `light/P-Claim.png` | Claim BTCB2 (from Tools) · light | `P-Light-Claim.dc.html` |
| `light/P-Signing.png` | Signing, pairing, labels, export · light | `P-Light-Signing.dc.html` |
| `light/P-Settings.png` | Settings (sidebar foot) · light | `P-Light-Settings.dc.html` |
| `light/P-Members.png` | Members (from Settings) · light | `P-Light-Members.dc.html` |
| `light/P-Duress.png` | Duress (from Connect) · light | `P-Light-Duress.dc.html` |
| `light/P-GapsAccount.png` | Account gap screens (from Settings) · light | `P-Light-GapsAccount.dc.html` |
| `light/P-GapsWallets.png` | Wallet gap screens (from Tools / Settings) · light | `P-Light-GapsWallets.dc.html` |
| `light/P-GapsDetails.png` | Detail gap screens (from Settings) · light | `P-Light-GapsDetails.dc.html` |
| `light/P-BuySell.png` | Marketplace · Buy/Sell · light | `P-Light-BuySell.dc.html` |
| `light/P-P2P.png` | Marketplace · P2P · light | `P-Light-P2P.dc.html` |
| `light/P-AI-Access.png` | AI access · paired clients, pairing, permissions, revoke (from Settings) · light | `P-Light-AI-Access.dc.html` |
| `light/P-AI-Setup.png` | AI-led wallet setup · handoffs, states, final review · light | `P-Light-AI-Setup.dc.html` |
| `light/P-AI-Transaction.png` | AI-prepared transaction · draft, review, signing states, failures · light | `P-Light-AI-Transaction.dc.html` |

## Studies

| Image | Board | Canvas page | Source |
| --- | --- | --- | --- |
| `studies/P-Launcher-Rail.png` | B · Launcher · network activity via COINCUBE \| Connect (no local node running) | Launcher backdrop (design B) | `P-Launcher-Rail.dc.html` |
| `studies/P-Launcher-Rail-Node.png` | B · Launcher · network activity from a local node already running | Launcher backdrop (design B) | `P-Launcher-Rail-Node.dc.html` |
| `studies/P-Launcher-Rail-Light.png` | B · Launcher · light | Launcher backdrop (design B) | `P-Launcher-Rail-Light.dc.html` |
| `studies/P-AppPrefs-Rail.png` | B · App preferences › Launcher · Network activity on/off and source | Launcher backdrop (design B) | `P-AppPrefs-Rail.dc.html` |
| `studies/H-Overview.png` | Overview · shared page head | Page header study | `H-Overview.dc.html` |
| `studies/H-Wallet.png` | Wallet (Spark) · one Swap BTC ⇄ USDB in the head; rows keep Send and Receive; See all opens Activity filtered, Wallet settings opens Settings scoped | Page header study | `H-Wallet.dc.html` |
| `studies/H-Settings.png` | Settings · scope in the head, tabs on the rule | Page header study | `H-Settings.dc.html` |
| `studies/H-Send.png` | Send · the shipped Payment details card under the new head: Back on the right, steps on the rule | Page header study | `H-Send.dc.html` |
| `studies/H-Send-From.png` | Send · You send Change: pay from Bitcoin or US Dollar (USDB) | Page header study | `H-Send-From.dc.html` |
| `studies/H-Send-To.png` | Send · They receive Change: Lightning, on-chain, Spark, or a cross-chain stablecoin | Page header study | `H-Send-To.dc.html` |
| `studies/H-Receive.png` | Receive · same head (Vault shown) | Page header study | `H-Receive.dc.html` |
| `studies/H-Vault.png` | Wallet (Vault) · same page as Spark: one Bitcoin row, protection strip (time lock, PSBTs), Tools and Wallet settings in the head | Page header study | `H-Vault.dc.html` |
| `studies/H-Send-Vault.png` | Send (Vault) · same card as Spark, plus network fee and coins; Sign step for 2 of 3 | Page header study | `H-Send-Vault.dc.html` |
| `studies/H-Settings-Vault.png` | Settings scoped to Vault · spending keys and recovery path | Page header study | `H-Settings-Vault.dc.html` |
| `studies/H-Convert.png` | Swap BTC ⇄ USDB · from the Spark page head: From and To swap, amount, indicative rate; Get quote | Page header study | `H-Convert.dc.html` |
| `studies/H-Convert-Review.png` | Swap · review: quoted rate, fee, max price change, then Swap | Page header study | `H-Convert-Review.dc.html` |
| `studies/H-Settings-Spark.png` | Settings scoped to Spark · auto-convert received bitcoin, pay from USDB, max price change | Page header study | `H-Settings-Spark.dc.html` |
| `studies/H-Settings-Spark-Off.png` | Settings · Spark · turning auto-convert off converts the USDB balance back | Page header study | `H-Settings-Spark-Off.dc.html` |
| `studies/G-Launcher.png` | Launcher · Connect-only Cubes as ghosts, Key open | Connect Cubes as ghosts | `G-Launcher.dc.html` |
| `studies/G-Ghost-Details.png` | Ghost selected · the face each variety turns to | Connect Cubes as ghosts | `G-Ghost-Details.dc.html` |
| `studies/O-Launcher.png` | Launcher · PIN or passkey, then the Cube tumbles to its loading face and opens | Opening on the Cube | `O-Launcher.dc.html` |
| `studies/O-Finale-A.png` | Finale A · Bloom | Opening on the Cube | `O-Finale-A.dc.html` |
| `studies/O-Finale-B.png` | Finale B · Flash | Opening on the Cube | `O-Finale-B.dc.html` |
| `studies/O-Finale-C.png` | Finale C · Zoom | Opening on the Cube | `O-Finale-C.dc.html` |
| `studies/O-Glow-A.png` | Edge glow A · Soft | Opening on the Cube | `O-Glow-A.dc.html` |
| `studies/O-Glow-B.png` | Edge glow B · Bright | Opening on the Cube | `O-Glow-B.dc.html` |
| `studies/O-List.png` | List view · full screen | Opening on the Cube | `O-List.dc.html` |
| `studies/O-Stall.png` | Stalled loader · Cancel | Opening on the Cube | `O-Stall.dc.html` |
