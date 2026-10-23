# V2 addendum — read AFTER BRIEF.md (BRIEF.md rules still apply unless changed here)

Robert reviewed V1 and asked for a second pass. V1 artboards stay untouched; V2 artboards are NEW files prefixed `V2-`. Robert's verdict on V1: "a bit cluttered"; the sidebar "is hard to tell apart from the rest of the application"; he is leaning toward the 3D Launcher. Decisions he made on 2026-10-21:

## Launcher 3D (V2)
- Most users have a handful of Cubes: Cubes are BIG by default. At 1–4 Cubes each cube is ~220px wide on the floor (one row, centered); at 5–8, two rows at ~170px; at 9–20, rows of 5 at ~120px (still readable). Never a carousel; the stage may scroll vertically only above 10.
- Cube faces carry meaning (decided: "Network on top, wallets on sides"):
  - TOP face: the network mark. Bitcoin mainnet = ₿ glyph drawn as inline SVG (a circle with a bold B; author the SVG, no emoji); Bitcoin Blake2b mainnet = the same ₿ mark plus a small text tag "Blake2b" on the top face; test networks = the ₿ mark drawn in outline only plus a text tag with the network name ("Testnet4", "Signet", "Regtest", "Blake2b Testnet4").
  - LEFT face: wallet-capability symbols present in the Cube, as a small row of drawn icons: Vault = shield, Spark = lightning bolt, Liquid = drop. Absent wallets are simply not drawn (no greyed ghosts).
  - RIGHT face: lock/open state as a drawn padlock (closed/open) plus, ONLY when backup is not confirmed or unknown, one small attention badge (a circled "!" for not confirmed, a circled "?" for unknown). Confirmed shows nothing extra.
  - OUTLINE hue = chain family: Bitcoin mainnet = warm orange edge (#F3821D at 60%); Bitcoin Blake2b mainnet = a distinct cool hue (#5FB3C9 at 60%); test networks = dashed neutral grey edge. Outline is 1.5px and always paired with the top-face tag, never the only cue.
  - AURA: selection only (soft orange glow under/around the selected cube). No other glows. No pulsing. Nothing animates continuously.
  - Face material: flat neutral greys (top lightest), subtle contact shadow on the floor. Light theme: lighter greys, same hue logic.
- Label below each cube shrinks to: Cube name (max 2 lines) + one line "Locked" / "Open" (and "Open · this tab" for the current Cube). Network name and protection text move to a hover-free DETAIL PANEL that appears for the SELECTED cube (bottom-right or right rail): full network name, lock state, protection line, wallets list, and the Open button. Keyboard: arrow keys move selection in the prototype is optional; Tab order must reach each cube as a real button.
- Keep: top bar with All Cubes, filters (All networks / Live chains / Test networks / Chain ▾), hidden count + Clear filters, account state, App preferences; anchored "+ Create Cube" and "Import or restore a Cube"; the 0-Cube and no-match states; levers cubeCount, theme, reducedMotion, focusDemo.
- Add a legend row (small, muted, bottom-left): the three face meanings in words with the drawn icons, so the encoding is learnable.

## Inside the Cube (V2 shell)
- Keep the top bar (Tenshu mark, Cube switcher with network, account chip, Cube connection chip). REMOVE "Connect account" from the sidebar foot (the top bar carries it). The foot keeps only Settings.
- Sidebar must be visibly distinct: its own surface (`--rail`, 1px right rule), 248px wide, each entry = drawn 18px stroke icon + word (Overview = four-square grid, Activity = list/clock, Backup & protection = shield-check, Spark = bolt, Vault = shield, Liquid = drop, Settings = gear). Selected entry: `--selected` background + orange left-to-text icon tint, text stays ink. Icons are inline SVG with one consistent 1.5px stroke; no emoji or unicode glyphs.
- Wallets stay grouped under a "Wallets" heading, exactly as Robert likes.
- Expert: decided "Per-Vault Advanced". The global Expert group is REMOVED. Each Vault wallet page has an "Advanced" disclosure (collapsed by default) holding Coin control, PSBT proposals, Descriptors & keys, Recovery path tools. Node resources and Maintenance live under Settings › Connections (Vault scope). Spark/Liquid pages have no Advanced disclosure unless a real tool exists (none in this design).
- Compact attention row above the foot stays ("Backup not confirmed · Review").

## Overview page (V2) — decided "Keep its structure, refine craft"
Model it on the current Tenshu Overview (Robert's screenshot, synthetic values only):
- "Total Balance" with a hide-balances eye toggle button; large fiat amount; sats line beneath ("50,360 SATS" style, number bold, unit muted).
- "Wallets" heading with its own eye toggle. One container per wallet (Spark, Liquid, Vault) with a 1px `--line` border and 16px radius (this is the one place cards are allowed: the incumbent uses them): wallet icon + name (muted), big fiat amount, sats line, then one ASSET ROW per asset: coin mark (₿ orange circle; L-BTC = ₿ with a small Liquid tag; USDt = teal circle with T) + ticker + amount (bold number, muted unit, then fiat) and per-asset Send (filled orange pill) / Receive (outlined orange pill) on the right. Vault shows BTC with 0 and "Recovery path" link in the card foot. Liquid shows L-BTC and USDt.
- A centered outlined "Transfer" pill under the cards (with the up/down arrows icon), as in the incumbent.
- Add, without clutter: an explicit network line in the page heading area ("Everyday · Bitcoin mainnet") and the compact attention row where applicable. Tighten vertical rhythm: card padding 20px, asset rows 56px, no empty bands.
- Synthetic balances: Total $41.70 / 50,360 SATS; Spark $20.74 / 25,053 SATS; Liquid $20.95 / 25,307 SATS (L-BTC 12,784 SATS $10.58; USDt 10.37); Vault $0.00 / 0 SATS. These match the screenshot's fixture values and are labeled synthetic in the footer.
- Hidden-balances state (lever): amounts render as "••••••".

## Craft bar for V2 (from the impeccable craft floor)
Less clutter than V1: fewer words per element, one idea per row. Spacing: tight groups, generous separation, more space above a heading than below it. One authored motion moment at most (the selected cube lift), none under reduced motion. Themed focus rings, tabular numerals (`font-variant-numeric: tabular-nums`) on all amounts. No kicker/eyebrow labels above headings. No gradient text, glass, left-border accents, or hard offset shadows. Icons drawn, one stroke weight.
