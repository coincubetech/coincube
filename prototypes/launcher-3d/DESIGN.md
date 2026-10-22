---
name: Tenshu 3D Launcher Lab
description: As-built native design inventory for the standalone synthetic Cube launcher.
colors:
  action-orange: "rgb(243, 130, 29)"
  action-hover: "rgb(243, 168, 98)"
  action-ink: "rgb(25, 25, 23)"
  action-disabled: "rgb(100, 100, 95)"
  dark-muted: "rgb(179, 187, 183)"
  light-muted: "rgb(82, 84, 78)"
  dark-stage: "rgb(16, 20, 22)"
  light-stage: "rgb(227, 223, 213)"
  dark-label: "rgba(23, 28, 29, 0.9)"
  light-label: "rgba(248, 246, 240, 0.9)"
  dark-label-hover: "rgb(42, 47, 47)"
  light-label-hover: "rgb(220, 215, 205)"
  dark-label-selected: "rgb(58, 43, 29)"
  light-label-selected: "rgb(247, 225, 200)"
  dark-label-ink: "rgb(245, 240, 232)"
  light-label-ink: "rgb(28, 31, 28)"
  dark-backup-warning: "rgb(255, 192, 123)"
  light-backup-warning: "rgb(125, 64, 10)"
typography:
  headline:
    fontFamily: "IBM Plex Sans"
    fontSize: "28 Iced logical px"
    fontWeight: 400
  title:
    fontFamily: "IBM Plex Sans"
    fontSize: "19 Iced logical px"
    fontWeight: 400
  body:
    fontFamily: "IBM Plex Sans"
    fontSize: "16 Iced logical px"
    fontWeight: 400
  label:
    fontFamily: "IBM Plex Sans"
    fontSize: "14 Iced logical px"
    fontWeight: 400
  caption:
    fontFamily: "IBM Plex Sans"
    fontSize: "13 Iced logical px"
    fontWeight: 400
rounded:
  primary-control: "6 Iced logical px"
  selectable-label: "7 Iced logical px"
  stage-container: "10 Iced logical px"
spacing:
  label-lines: "2 Iced logical px"
  catalog-lines: "4 Iced logical px"
  detail-lines: "6 Iced logical px"
  catalog-items: "7 Iced logical px"
  fixture-controls: "10 Iced logical px"
  toolbar-controls: "12 Iced logical px"
  catalog-sections: "14 Iced logical px"
  empty-state: "16 Iced logical px"
  sections: "18 Iced logical px"
  scene-catalog: "20 Iced logical px"
  shell-inset: "24 Iced logical px"
components:
  button-primary:
    backgroundColor: "{colors.action-orange}"
    textColor: "{colors.action-ink}"
    rounded: "{rounded.primary-control}"
  button-primary-hover:
    backgroundColor: "{colors.action-hover}"
    textColor: "{colors.action-ink}"
    rounded: "{rounded.primary-control}"
  label-dark-selected:
    backgroundColor: "{colors.dark-label-selected}"
    textColor: "{colors.dark-label-ink}"
    rounded: "{rounded.selectable-label}"
  label-light-selected:
    backgroundColor: "{colors.light-label-selected}"
    textColor: "{colors.light-label-ink}"
    rounded: "{rounded.selectable-label}"
---

# Design System: Tenshu 3D Launcher Lab

## Overview

**Creative North Star: "Tangible Cubes, readable controls"**

This is a descriptive inventory of the standalone Iced/wgpu prototype at source commit `e72aacfc9d093a5e1af84cb10114a0597b90cb8f`, not a new identity or global Tenshu design policy. The fixed-camera floor makes synthetic Cube containers tangible; ordinary desktop controls and the right-hand catalog carry names, network identity and actions. Orange connects creation, selection and preview. IBM Plex Sans preserves the existing Tenshu type identity.

Authority is the built Rust/WGSL implementation, with the bounded direction in [DESIGN_BRIEF.md](DESIGN_BRIEF.md). There is no prototype `PRODUCT.md`; this technical lab does not establish production product authority. Its surface mode is Operate. Inside-Cube Concept C and the wallet application are outside this document's scope.

**Key Characteristics:**

- A dominant 3D floor paired with a persistent text catalog.
- Dark default and switchable light mode; custom warm label/action treatments over standard Iced themes.
- Network identity in both geometry color and readable text.
- Explicit synthetic-data language and CPU/redraw measurement limits.

All UI dimensions below are Iced logical pixels, independent of physical display scaling. Frontmatter size strings record those native units; they are not CSS declarations. Scene values remain Rust/WGSL floating-point vectors and world units. The accompanying `.impeccable/design.json` records motion metadata without browser replicas of native widgets.

## Colors

Warm orange actions and labels sit against charcoal or pale neutral scene surfaces. Custom UI colors are recorded once in the frontmatter; references below name their roles.

### Primary

- **Action orange** (`action-orange`) fills Create Cube and Open preview and outlines the selected catalog/object label. **Action hover** is shared by Hovered and Pressed states; **action ink** remains the label foreground. Disabled creation uses **action disabled** at the catalog cap.
- **Selected warm surfaces** (`dark-label-selected`, `light-label-selected`) distinguish the current Cube without replacing its network color.

### Secondary

These are geometry material inputs, not flat UI swatches or additional action colors. [model.rs](src/model.rs) supplies exact RGBA `f32` vectors: Bitcoin mainnet (`[0.87, 0.42, 0.12, 1.0]`), Bitcoin Blake2b mainnet (`[0.25, 0.56, 0.64, 1.0]`), and Testnet4 (`[0.43, 0.46, 0.58, 1.0]`). Lighting changes their rendered appearance; converting them directly to UI hex values would misrepresent the shader pipeline.

### Neutral

- **Muted text** supports subtitles, networks, fixture labels, notices and metrics, using the theme-specific `dark-muted` / `light-muted` pair.
- **Stage surfaces** use `dark-stage` / `light-stage` at the container level. The visible floor is separately shaded from RGBA uniform inputs: dark (`[0.07, 0.085, 0.09, 1.0]`) and light (`[0.68, 0.65, 0.59, 1.0]`).
- **Selectable labels** use translucent rest surfaces, opaque hover surfaces and theme-specific ink from the frontmatter. The same style function renders catalog entries and on-floor name buttons.
- **Backup warning text** uses the theme-specific warning pair only for a selected Cube whose backup is unconfirmed. Catalog backup text remains muted; words carry the status.

**The Text-Supported Identity Rule.** Every catalog entry names its network. Color alone never carries the catalog's network or backup meaning.

The application explicitly selects Iced `Theme::Dark` / `Theme::Light`, rather than the production `coincube-ui` custom theme. Built-in shell backgrounds, general text, pick lists, checkboxes, secondary buttons and text buttons remain Iced defaults. Iced core 0.14.0 defines a dark shell background (`#2B2D31`), dark text RGB (`0.90, 0.90, 0.90`), white/black light shell colors and built-in blue primary (`#5865F2`). That mixed control palette is implementation drift, not a new Tenshu accent prescription.

## Typography

**Body and heading font:** embedded `IBMPlexSans-Regular.ttf`, registered as IBM Plex Sans and selected as the application's default font. Only the regular face is explicitly loaded; there is no custom display or monospace face, weight hierarchy or letter-spacing override. No explicit fallback stack is configured.

The frontmatter captures recurring headline/title/body/label/caption roles. Additional observed sizes are: empty-stage heading (26), selected-detail title (17), on-floor Cube name (15), and small network/status/measurement caveats (12). The catalog title uses the title role; catalog Cube names and the launcher subtitle use body. Metrics and notices use caption. These are a practical native size ramp, not a mathematically defined scale.

Line height is inherited from Iced; text is not assigned CSS line-height, tracking or a web reading measure. Control text sizes that are not explicitly set remain renderer/widget defaults. Heading emphasis comes from size and position, not fabricated semibold styling.

**The Single-Face Rule.** Preserve the embedded regular IBM Plex Sans identity when extending this prototype; do not infer extra loaded weights from heading roles.

## Layout

The centered desktop window starts at (1440 × 900) with a minimum of (1024 × 700). A full-window column has shell inset and section spacing from the frontmatter. Its vertical order is identity header, network/create toolbar, fixture/motion toolbar, expanding scene/catalog row, selection details, notice, performance row, then measurement caveat. There is no page-level scroll container.

The header puts the product name/subtitle at left and the synthetic-lab label and Light theme checkbox at right. The first toolbar puts visibility filtering/counts/Clear filters at left and Create on/network/Create Cube at right. The fixture row places Empty/1/4/12/20 at left and Float/Benchmark/Reduce motion at right. Flexible spacers split these groups; rows do not implement wrap or a compact variant.

The scene fills remaining width and height. The catalog stays fixed at (260) wide, separated from the scene by `scene-catalog` spacing. Only its list scrolls. Entries have padding (12), catalog item spacing and catalog line spacing from the frontmatter. The selected-detail row uses spacing (18), with metadata separated by (22) and internal title/metadata spacing from `detail-lines`.

For up to four visible Cubes, world positions form a single row. Above four, columns are `ceil(sqrt(count))` and rows fill in catalog order. Centers are spaced (3.4) in world X and (3.7) in world Z, at world Y (1.35). The orthographic camera uses a fixed viewing direction proportional to `(9, 12, 20)` and refits to count and scene aspect ratio; there is no orbit or pan UI.

On-floor name/network buttons appear only at counts up to eight. When scene width divided by visible count is below (220 logical px), only the selected object's label is retained; with no selection, the catalog supplies identity. Label anchors use the scene projection at world Y (-0.2), remain stationary during floating, and are clamped using horizontal inset (100), assumed width (200), vertical offset (6) and bottom allowance (70). These fixed label allowances describe the current workaround, not a proven responsive sizing system.

**The Catalog Continuity Rule.** The regular catalog remains available when object labels are reduced or absent at higher counts.

Compact fixture fit now passes at 1024×700 logical content size for the tested 4/12/20 English fixtures; see [VERIFICATION.md](VERIFICATION.md). Dense selected-object identity, catalog scrolling and complete keyboard focus still require refinement before integration. Toolbar wrapping, text scaling, longer names/localization and smaller windows remain unverified. The fixed catalog and minimum window are current constraints, not responsive-layout rules.

## Elevation & Depth

The 3D scene owns depth. A reusable rounded mesh, directional lighting, view-dependent rim light, specular highlights, floor shading and soft contact-shadow quads create volume. Ordinary 2D selectable labels use tonal layering plus a thin selection border, with no authored drop shadow. These are native GPU materials, not CSS elevation tokens.

[scene.wgsl](src/scene.wgsl) uses normalized light direction `(-0.6, 1.0, 0.7)`, ambient/diffuse weights (0.36 / 0.64), specular exponent (55) and strength (0.23), and material-colored rim strength (0.15). Selected geometry adds a warm rim vector `(0.65, 0.30, 0.07)` without changing the underlying network material. The floor uses radial falloff `exp(-length(xz) * 0.035)` multiplied into `(0.75 + 0.25 * falloff)`.

Each contact shadow is an alpha-blended floor quad with half extent (1.6 world units). Radial `smoothstep(0, 1, distance)` falloff reaches maximum black alpha (0.40) at its center. This is inexpensive contact shading, not a shadow map or physically simulated shadow. Shadows stay on the floor while Cubes move.

Selection lifts a Cube by (0.25 world units) with exponential approach `1 - exp(-elapsed_seconds * 18)`; selection redraw requests last (350 ms). Optional Float adds `sin(time * 1.2 + id * 0.4) * 0.07` world Y displacement. Reduce motion disables Float and selection interpolation, applying the selected lift immediately. Focus loss pauses floating/benchmark redraw; default idle does not request continuous animation. Benchmark may redraw a static reduced-motion scene. The prototype uses its own checkbox, with no OS reduced-motion preference integration established.

## Shapes

Primary controls, selectable labels and the stage container use the frontmatter radii. Selected labels add an orange border (1 logical px); unselected labels set border width (0). Rest/hover/selected treatments are otherwise shared between floor labels and catalog entries. Floor labels have padding (7 vertical, 10 horizontal). The container's radius is not evidence that the shader is masked to rounded corners; GPU rendering uses a rectangular viewport/scissor.

The Cube silhouette is authored geometry: each face is subdivided at `[-1, -0.96, -0.88, 0, 0.88, 0.96, 1]`, clamped to a core half extent (0.86), then expanded along its normal by bevel radius (0.14 world units). This geometric bevel supplies the soft edge; no bitmap illustration, texture or logo asset is used.

## Components

### Action and experiment controls

Create Cube and Open preview share the primary style: orange fill, dark ink and native primary-control radius. Hovered and Pressed share the lighter orange; Disabled uses the muted disabled fill. Creation disables at (7) synthetic Cubes on the chosen network; its count appears beside the network selector. Three fixture networks permit (21) total. Default padding and other inherited behavior come from Iced's button implementation, not a prototype-defined control-height system. A keyboard adapter adds a two-logical-pixel theme-contrasting outline with a three-pixel gap, outside existing control bounds; selection remains orange.

Empty/1/4/12/20 use Iced secondary buttons. Clear filters and Print metrics use Iced text buttons. The two pick lists and four checkboxes use built-in theme styling; custom CSS-like states must not be inferred. Fixture buttons replace the in-memory catalog, rather than acting as persistent navigation tabs.

### Cube selection and preview

Geometry clicking, name-label clicking and catalog clicking select the same Cube. Catalog entries contain name, explicit network and backup/Vault status. Each uses the shared selectable-label style with rest, hovered/pressed and selected variants; selected styling takes precedence over hover. The catalog is one Tab stop with a visible group focus outline. Its arrow keys step through visible Cubes with wraparound; Enter/Space produces the selected preview notice. Before a control is focused, scene arrows/Enter remain shortcuts. A focused control owns its keys, and captured events do not reach global shortcuts. Selected-row reveal and dense object labels remain deferred design work.

Selection is not unlock. The details row shows the Cube/network, protection text, Vault presence and Locked fixture label, alongside Open preview. Preview only updates explanatory text. There is no authentication sheet, wallet loading or balances component in this lab.

### Empty and filtered-empty stage

An empty catalog centers a (26) heading, (16) subtitle and Create Cube action with empty-state spacing. A populated catalog hidden by filtering instead offers Show all Cubes. Creating on an explicitly chosen hidden network returns the filter to All so the new item is visible. These are implemented states; there is no service-loading spinner, remote error state or wallet recovery flow to document.

### Notices and measurements

Notices remain inline beneath selection details. Metrics use a compact text row and terminal-print action, followed by a persistent limitation statement. Rolling samples retain up to (300) redraw intervals and CPU preparation/encoding measurements; on-screen text updates on a (500 ms) subscription while focused animation or benchmarking is active. Redraws/s, mean/p95 interval, CPU prepare/encode and scene draws are not GPU timestamps, presented FPS, end-to-end latency or power. No release-performance target is established by their styling.

### Capability boundary

The shader scene has no native accessibility node model of its own. The complete Tab path, control activation and picker keys provide keyboard operation, verified in the native Lab; they do not establish VoiceOver or other assistive-technology conformance. Semantic exposure and text scaling remain unverified. No dedicated compact, 2.5D or GPU-unavailable fallback is implemented. The verification record retains a historical100-Cube stress sample from before the Estate-limit clarification; current design targets10–20 total Cubes; cross-device GPU/power acceptance remains unmeasured. Those gaps remain defects or unverified acceptance, not reusable design invariants.

## Do's and Don'ts

### Do:

- **Do** retain text-supported network and protection identity alongside 3D color.
- **Do** use the regular embedded IBM Plex Sans and the observed native units.
- **Do** keep the catalog available at scene densities where on-floor labels disappear.
- **Do** label synthetic actions and measurement scope as explicitly as the current lab.
- **Do** treat this inventory as a local source snapshot and recheck source when the prototype changes.

### Don't:

- **Don't** promote this lab's fixed layout or inherited Iced blue controls into global Tenshu rules.
- **Don't** translate shader material vectors into asserted UI hex colors or contact shading into CSS shadows.
- **Don't** represent selection/preview as authentication, or redraw cadence as GPU performance.
- **Don't** canonize compact overlap, missing focus/AT proof, rectangular shader clipping or absent fallback as intended future behavior.

Startup evidence: `/Users/macstudio/git/coincubetech/AGENTS.md`; company-brain `MEMORY.md`, `USER.md`, `AGENTS.md`, `PROJECTS.md`, `projects/coincube-desktop/PROJECT.md` (read in that order); target repository `CLAUDE.md`; Impeccable `SKILL.md`, full `reference/document.md`, `agents/impeccable_documenter.toml` and `reference/craft-floor.md`. Agent Ops origin was verified as `tirith-tech/agent-ops`; `AGENTS.md`, `README.md`, `companies/coincube.json`, `contracts/task.md`, `docs/task-routing.md`, `workflows/delivery.md` and `workflows/pr-review-repair.md` were read via `git show` at pinned reviewed-main revision `b4e4fa3c3ecedad93f40d08f71baca7cea707a25`. Cached `origin/main` matched; fresh fetch failed because `.git/FETCH_HEAD` was outside this session's write permissions. This document does not claim remote policy freshness or a new native QA pass. Source inventory: [main.rs](src/main.rs), [model.rs](src/model.rs), [scene.rs](src/scene.rs), [scene.wgsl](src/scene.wgsl), [metrics.rs](src/metrics.rs); observed native verification limits are retained from [VERIFICATION.md](VERIFICATION.md).
