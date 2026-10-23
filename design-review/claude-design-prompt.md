# Copy into Claude Design

Develop the selected Concept C navigation, a dedicated Launcher, and detailed Settings screens for **COINCUBE | Tenshu**, an existing native desktop Bitcoin wallet built in Rust/Iced for macOS, Windows and Linux. This is a design review before implementation. Produce an interactive prototype and reviewable screens; do not change application code or connect to a real wallet/account.

The primary audience is everyday users, with specialist tools easy to reach in an explicitly named Expert section. Stage one includes the separate Launcher, navigation inside open Cubes, and detailed Settings design. Robert selected Concept C; preserve its one readable sidebar and explicit Expert section. The Launcher’s 3D treatment is an exploration, not an approved production direction. Optimize task clarity, findability, readable scope and compact-window behavior. Keep the recognizable Tenshu identity; this is not a rebrand. Use IBM Plex Sans for functional text, the existing restrained orange #F3821D for actions/selection, dark neutral surfaces and the existing warm light theme as a second mode. Avoid decorative gradients, glass, ornamental charts and generic card grids. Reduce repeated container chrome and excessive empty space while keeping a comfortable desktop density.

Product truth:
- A Cube is an encrypted container. It may have a Vault, Spark and an eligible Liquid wallet. Keep active Cube, wallet and network identity explicit.
- Vaults have signing policies and time-locked recovery paths. **Backing up a Cube and spending through a Vault recovery path are different jobs.** Never imply a backup unlocks a time lock.
- Connect supplies account-backed services. Account authentication, this Cube’s service connection/registration and verified feature entitlement are separate states.
- Expert access includes coin control, PSBT proposals, descriptors, key details, node resources and maintenance. Preserve those capabilities without exposing them as everyday peers of Send/Receive.
- Stable Balance is a device preference that does not travel with the seed. A restored Spark wallet can hold USDB while this device’s preference is off. Explain and review this state; never silently enable conversion or promise a guaranteed peg.
- Local Keychain phone pairing is for an eligible signer key and Vault, on the same Wi-Fi, without the Connect API. Do not invent a cloud/local transport toggle or require Connect for the local flow.
- Liquid Settings is intentionally absent from current navigation. Shared seed protection belongs at Cube scope. Duress, Members and Marketplace have feature/eligibility gates; do not invent enabled controls or unsupported capabilities.

Live problems to address:
1. Three icon-heavy rails give multiple hierarchy levels equal emphasis. At a smaller window, Vault Settings is below the sidebar viewport; scrolling also moves primary Cube navigation out of view.
2. A Recovery Kit shortcut currently lands on General rather than Recovery.
3. Backup/protection and recovery-path spending are both called Recovery.
4. Wallet Settings leads with a raw descriptor; naming and common tasks are lower. Node health is mixed with raw logs, paths, tuning and rescan.
5. Two large warnings repeat above unrelated screens. Preserve the safety action, but make persistent attention compact and contextual.
6. Account billing leads with sales comparisons; device/security lists spread related details across wide rows.
7. After signing in through Home, an already-open Cube may still ask for sign-in in Lightning Address and Recovery while the shell says Signed in. Design truthful pending/disconnected/error states, not a cosmetic removal of the gate.

Selected structure and supplied references:
**C — One readable sidebar inside an open Cube:** Overview, Activity, Backup & protection; grouped wallet entries; collapsed but explicit Expert tools; Settings and Connect account anchored at the foot. Settings sections are scoped content tabs or a compact section selector, with no tertiary rail. Keep Cube switching available in the header and an explicit All Cubes / Launcher return. Preserve the user’s ability to keep multiple Cube tabs open; returning to the Launcher does not implicitly lock or close them. The exact typography, spacing, component styling and compositional craft may improve; the selected information hierarchy is the anchor, not a pixel-perfect mandate.

The local comparison page is http://127.0.0.1:56918/ and the detailed explorer is http://127.0.0.1:57727/tenshu-settings-explorer.html . These are loopback addresses on Robert’s Mac, not public design references. If inaccessible in your environment, use the attached portable files; do not claim to have inspected the URLs. A and B are historical alternatives, not active directions.

Attach/reference this packet, if supplied:
- `concept-c-selected.png`: synthetic screenshot of the selected in-Cube structure.
- `tenshu-settings-explorer.html`: self-contained interactive review prototype; select C, inspect each Settings layout and review state.
- `tenshu-concepts.md`: selected structure, detailed layout requirements, old-to-new mapping, evidence limits.
- `launcher-design-brief.md`: Launcher scope, network catalog and spatial-Cube requirements.
The screenshot and prototype contain synthetic fixture data only. Treat them as explicit structural references; identify deliberate departures and their reasons. They are design proposals, not screenshots of shipped behavior. Do not infer a lack of Launcher merely because the selected screenshot shows an open Cube.

Dedicated Launcher — explore Robert’s spatial Cube idea:
The Launcher is separate from every open Cube. It owns discovering, creating, importing and opening Cubes, plus app-wide preferences and Connect account access. Its catalog may be visible while Cubes are locked. Use only metadata safe and available before unlock; do not expose balances or protected contents or claim a backup is complete when status is unknown.

Start with an empty, restrained floor/stage and a clearly anchored floating “+ Create Cube” action. Include an explicit Import Cube / existing supported restore entry. After creation, place a recognizable Cube object on or just above the floor. Additional Cubes occupy a stable row at small counts; provide readable wrapping/rows or another scalable catalog arrangement at larger counts, without a forced horizontal carousel. A selected Cube opens through the existing PIN/passkey and network-specific loading/admission flow; animation must not delay access or pretend that security checks completed.

Show a stable text identity beside/below each object: Cube name, precise chain/network badge with the existing verified coin mark where available, lock/open state and concise protection/capability status. Support Bitcoin mainnet, Bitcoin Blake2b mainnet and the supported test networks as distinct identities. Do not invent a BTCB2 logo, ticker or network parameters. Symbols supplement explicit names. “No Vault configured” is neutral capability information, not a security alarm. “Backup not confirmed” is actionable; unknown/stale status must remain distinguishable from verified readiness. Color/auras are secondary and must not be the only way to read status. Avoid assigning a different glow to every property or simultaneous pulsing warnings.

Propose one catalog showing all local Cubes across supported networks, initially All networks, with lightweight remembered filters such as Live chains and Test networks plus chain-specific filtering. Both supported mainnets count as live; test environments stay explicitly marked in the catalog and after opening. Make active filters and hidden-result counts obvious, provide Clear filters, and distinguish no Cubes from no matches. Filters affect catalog visibility only; they never change a Cube’s chain, combine balances, reuse a signing/backend session across chains, bypass Blake2b admission rules or determine the selected chain for creation implicitly. New Cube creation explicitly selects chain/network. Restoring/importing verifies the file/network before opening. These are design requirements for future source validation, not a claim that a global network catalog already exists.

Compare three Launcher presentations while keeping Concept C inside the Cube fixed:
1. Fixed-camera 3D floor with floating/sitting Cubes: soft lighting, stable object positions, readable text, minimal purposeful selection/open transition.
2. A restrained 2.5D/isometric treatment expressing the same Cube metaphor with simpler rendering and lower motion cost.
3. A compact catalog/list companion using the same Cube imagery and identical metadata, as the accessibility/reduced-motion/high-count reference.
Use the same fixtures and states in all three. Assess findability and status legibility as well as visual appeal. Show 0, 1, 4 and 20 Cubes; long names; mixed chains/testnets; locked/open; incomplete/unknown backup; no Vault; no matches after filtering; keyboard focus; light/dark and reduced motion. Keep create/import reachable at each count. Require no camera manipulation, orbit, hover-only reveal, drag gesture or continuous animation to use the catalog. A compact view is a proposed alternate presentation, not an already shipped capability.

Separate the distinctive Launcher from the task-focused in-Cube workspace. Explore a coherent relationship between their materials, palette, typography and selection behavior, rather than carrying a large animated floor into every Settings screen. Evaluate native Rust/Iced rendering feasibility: a web/real-time 3D prototype is a visual proposal, not proof the same engine or frame rate is available in the app. Identify which effects can use static/pre-rendered or simple vector assets. Make the baseline usable with reduced motion and modest hardware. Our recommended native production candidate is a small Iced shader-widget/wgpu scene with shared cube geometry and ordinary Iced controls over/around it. A 2.5D/static fallback remains an exploration; do not prescribe a full game engine or a browser webview as the native architecture. Rendering feasibility and performance still require a spike.

Provide detailed layouts for:
1. Display & appearance: BTC/Sats, primary fiat/Bitcoin amount, fiat estimates/currency/rate source, direction badges; Cube identity/avatar; theme explicitly app-wide. Label actual storage scope; do not invent a rename API.
2. Backup & protection: readiness and next action; seed and Recovery Kit methods; keep actual method requirements, encryption, descriptor freshness and existing authenticated reveal flows. Separately show Vault recovery-path availability and alert eligibility.
3. Wallet & signers: identity, readable policy, signer aliases, Pair with Keychain, eligible Members; descriptor/key/hardware registration under expert disclosure.
4. Pair with Keychain: eligible key selection, same-network explanation, explicit disabled reason, phone picker, QR/waiting/cancel/error, paired phones. Preserve security boundaries.
5. Payment checks: recipient identity checks and Branta privacy details. Strict mode does not share plaintext destination/amount; timing and network connection may be observable. Do not imply guaranteed identity.
6. Connections: account auth versus Cube session versus backend/sync health; actionable failures; node resource/diagnostic disclosure; separate consequential maintenance/rescan.
7. Spark: Stable Balance state and restored-device mismatch; Lightning Address existing/claim/checking/signed-out/disconnected/error states; concise bridge health with technical diagnostics secondary.
8. Data & exports: recovery material versus wallet portability versus accounting/labels. Direct Recovery Kit link, encrypted/plain descriptor, wallet import/export, BIP329 labels, transaction table; preserve safeguards and privacy explanations.
9. Account & plan: identity, current plan and verified billing status, billing history, secondary plan comparison/referral, Contacts, sign out. Use no invented prices or renewal dates.
10. Devices & security: current device, compact identity/last activity/removal review; recent login summary and full history. Do not invent what device deletion revokes. Keep Duress gated.
11. About/app preferences: versions/build provenance, product Download Stats, installation-wide theme/Developer mode/BTCB2 beta, diagnostic and registration repair disclosure. Avatar detail shows current image, real quota/eligibility, loading/error and explicit regeneration without an invented free generator.

Use clearly **synthetic** fixtures: Cubes Everyday (Bitcoin mainnet), Blake workspace (Bitcoin Blake2b mainnet), Lab (supported test network) and Archive (Bitcoin mainnet); Spark Daily spending; Vault Family savings; Liquid Travel; account alex@example.test; generic devices. Example policy 2-of-3 must be labeled synthetic. No real addresses, xpubs, descriptors, recovery words, emails, IPs, billing records or private screenshot data. Do not upload or ask to connect a real financial account.

Show normal, loading, signed out, authenticated-account/disconnected-Cube, offline node, incomplete backup, no Vault, imported Vault, Testnet/Spark unavailable, long names and save failure. Disabled actions explain cause and next step. Show explicit scope and native-friendly keyboard/focus behavior. Retain actual safeguards for seed reveal, pairing, exports, device removal, billing and security changes.

Design at representative desktop sizes such as 1440×900 and 1024×768 logical pixels; these are concept canvases, not verified native minimum sizes. Show structural adaptation rather than shrinking text or making essential navigation disappear. Keep scope, Settings and Expert access stable. Include dark/light views, meaningful focus states and readable contrast.

Deliver: three Launcher explorations; selected Concept C inside-Cube navigation and expanded detailed Settings prototype; Launcher → unlock → Cube → All Cubes journey; mixed-network filter behavior; current-to-proposed route map; component/state notes; five findability walkthroughs (missing backup, pairing, node health, Lightning Address after sign-in, accounting export); a Launcher recommendation with tradeoffs and rendering/accessibility implications; and open questions that require real source/backend validation. Distinguish clickable design simulations from actual supported product behavior. Stop before implementation.
