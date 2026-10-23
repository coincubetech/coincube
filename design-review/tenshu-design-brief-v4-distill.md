# Distill pass (2026-10-22) — read after BRIEF.md, BRIEF-V2/V3/V4

Robert's feedback: some elements are too verbose, and helper text reads as if it were part of the design ("Locks Everyday and returns to the Launcher" sitting beside the Lock Cube button in the top bar). This pass applies the impeccable distill and clarify rules to every V4 artboard and the V2 shell top bar. Builders: apply ALL of the following; this is not optional polish.

Rules
1. Say each idea once. If a heading states it, no intro restates it. If a button's label says what happens, no helper sentence repeats it.
2. Controls carry their consequence in the label or in the confirmation step, never as an always-visible caption beside them. Top bar: "Lock Cube" alone; the consequence ("Everyday locks and the Launcher opens") appears only in its confirmation row after the click. Same rule for "All Cubes" (no note), the eye toggle (no label text, aria-label only), and chips (state word only).
3. Helper sentences under fields exist only when they change what the user types (format, limits). Otherwise delete them. "Choose a PIN for this Cube, or use a passkey instead." goes; the passkey button already says it.
4. Notes that mark review-only behaviour ("Prototype · …", "subject to source validation") are collapsed into ONE muted footer line per artboard, never inline beside controls, except the "Mechanics subject to source validation" line on the Create step list, which stays.
5. Stepper labels are one or two words. Page subtitle under the H1 is the Cube · network line only.
6. Cut every explanatory sentence in half, then again. Target: no body sentence over 12 words except safety wording quoted from the product (Branta note, deposit-once, single-use address).
7. No jargon the user cannot act on. If a term from source is obscure to Robert, replace it with plain language and keep the source term only in the review document, not the UI.
8. Keep every safeguard, every state, every lever and all accessibility attributes. Distill removes words and repeated chrome, not function.

Specific fixes (V3-Cube-Transfer.dc.html)
- New Cube step: add "Confirm PIN" as a second masked field; Create Cube stays disabled until both match, with the single error "PINs don't match" shown under the second field. Passkey path skips both fields (hide them when passkey is chosen, show "Passkey will be set on this device").
- Done step: replace the "Split coins (next step)" summary with plain language and a real entry point. Heading "Family savings · Blake2b created". One line: "Your coins now exist on both chains and can be spent separately." Then ONE primary action "Open the new Cube" and ONE secondary action "Split coins now" that links to the Vault's Advanced › Split coins (in this prototype, link to `V2-Cube-Overview.dc.html`). Under the secondary action, one muted line: "Splitting separates each coin into a Bitcoin-only and a Blake2b-only coin. Two transactions, then confirmations on each chain." No source labels (no "Build step 1/2", "Before fork", "Abandon split") in the UI.
- Explain step: cut to the identity block plus one sentence.

Specific fixes (V2-Cube-Overview.dc.html, V2-Cube-Compact.dc.html, V3-Wallet-Send.dc.html, V3-Wallet-Receive.dc.html, V3-Activity.dc.html)
- Top bar per rule 2 (remove the Lock Cube caption; add the confirmation row behaviour where the artboard has state for it, otherwise just drop the caption).
- Apply rules 1–8 throughout: field helpers, step intros, "Prototype" rows (keep the paste-example buttons but label the row with a single muted word "Example" and move the "Prototype" disclaimer to the footer), review-step explanations, empty states.

## Decision (2026-10-22)
Robert confirmed: top-bar chips show only the state word ("Signed in", "Connected", "Reconnect required"); the account email and the "Cube service" label are screen-reader text and hover title only. This supersedes the earlier "two separate labeled chips" wording in BRIEF.md and BRIEF-V2.md.
