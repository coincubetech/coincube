# Exported screens

Static PNG exports of every board in the Tenshu prototype (canvas v120,
2026-10-24), so the design can be read without a claude.ai session. Pixels only:
exact copy, spacing, tokens and state logic are in `../canvas/<source>`.

- `dark/` is the main prototype page with the device settings at their defaults:
  Appearance Dark, Developer mode off, Bitcoin Blake2b off, Launcher style Cubes.
- `light/` is the "Light theme" canvas page: the same screens mounted with
  `force-theme=light`, plus the Launcher's List view.
- Each image is the board's default state at 1440 px wide (900 or 1100 px tall),
  scaled to fit. Boards taller than the frame are cut off at the bottom; the
  source has the rest. Other states are levers in the source (`data-props`).
- All data is synthetic.

| # | Screen | Source |
| --- | --- | --- |
| 01 | Launcher, Cubes view (light also has `01b`, List view) | `P-Launcher.dc.html` |
| 02 | PIN unlock | `P-Unlock.dc.html` |
| 03 | Opening a Cube (ends on the Overview) | `P-Opening.dc.html` |
| 04 | App preferences over the Launcher | `P-AppPrefs.dc.html` |
| 05 | Connect account over the Launcher | `P-Connect.dc.html` |
| 06 | Connect sign-in | `P-SignIn.dc.html` |
| 07 | Create Cube | `P-Create.dc.html` |
| 08 | Import or restore | `P-Restore.dc.html` |
| 09 | Installer gap screen: back up descriptor | `P-GapsInstaller.dc.html` |
| 10 | Border Wallet pattern | `P-BorderWallet.dc.html` |
| 11 | Delete Cube | `P-Delete.dc.html` |
| 12 | Overview | `P-Overview.dc.html` |
| 13 | Activity with detail panel | `P-Activity.dc.html` |
| 14 | Send | `P-Send.dc.html` |
| 15 | Receive | `P-Receive.dc.html` |
| 16 | Transfer | `P-Transfer.dc.html` |
| 17 | Liquid wallet | `P-Liquid.dc.html` |
| 18 | Vault › Tools (coin control) | `P-Advanced.dc.html` |
| 19 | Split coins, Blake2b off on this device | `P-Split.dc.html` |
| 20 | Claim BTCB2, Blake2b off on this device | `P-Claim.dc.html` |
| 21 | Sign transaction | `P-Signing.dc.html` |
| 22 | Settings | `P-Settings.dc.html` |
| 23 | Members | `P-Members.dc.html` |
| 24 | Duress (Connect) | `P-Duress.dc.html` |
| 25 | Cube Recovery Kit (account gap screen) | `P-GapsAccount.dc.html` |
| 26 | Liquid › Send USDt (wallet gap screen) | `P-GapsWallets.dc.html` |
| 27 | Split entry on the cube list (detail gap screen) | `P-GapsDetails.dc.html` |
| 28 | Marketplace · Buy/Sell | `P-BuySell.dc.html` |
| 29 | Marketplace · P2P | `P-P2P.dc.html` |
| 30 | AI access | `P-AI-Access.dc.html` |
| 31 | AI-led wallet setup, ready for review | `P-AI-Setup.dc.html` |
| 32 | AI-prepared transaction | `P-AI-Transaction.dc.html` |
| 33 | App preferences over an open Cube (dark only) | `P-AppPrefs-InCube.dc.html` |
| 34 | Connect over an open Cube (dark only) | `P-Connect-InCube.dc.html` |

Not exported: `P-Locking.dc.html` (a transition back to the Launcher; it looks
like 01), and the Split and Claim flows with Blake2b turned on (switch it on in
App preferences › Developer in the live prototype).
