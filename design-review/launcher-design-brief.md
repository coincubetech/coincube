# Tenshu Launcher — spatial Cube exploration

Status: Concept C is selected for navigation inside an open Cube. The Launcher is a separate surface. Robert proposed spatial Cube objects and a cross-network catalog; specific visual treatment, default filtering and renderer remain proposals. This is design discovery, not authorization to change application code.

## Relationship to the selected design

Launcher: find/create/import/open Cubes; access Connect account and installation preferences. Open Cube: use Concept C’s task sidebar, wallets, activity, protection, Expert tools and scoped Settings. Provide an explicit All Cubes return and a compact Cube switcher. Preserve open tabs and their security state. Entering the Launcher must not silently close or lock an open Cube.

The live pass covered Launcher account access and app-wide preferences. The original concept screenshots concentrated on the open-Cube workspace and did not fully design the Launcher. This brief closes that scope gap without claiming a Launcher concept is already selected.

## Spatial proposal

An empty floor/stage introduces an anchored floating + Create Cube action and an existing import/restore entry. A created Cube sits or floats above the floor. At small counts, Cubes form a stable row; at larger counts, wrap into readable rows or a scalable catalog. Do not force camera control or a horizontal carousel to find a Cube. Give the selected object a restrained lift or lighting change; continuous bobbing, rotation or pulsing is optional exploration and disabled by reduced motion.

Explore fixed-camera real 3D, a lighter 2.5D/isometric rendering, and a compact catalog/list presentation with the same imagery/metadata. Compare findability, status clarity and rendering cost using 0, 1, 4 and 20 synthetic Cubes. Concept C inside the opened Cube stays fixed across these Launcher explorations.

## Identity and state

Every object has a readable text label: Cube name, precise chain/network, lock/open state and the available protection/capability summary. Use existing verified Bitcoin and Bitcoin Blake2b symbols where available, with text. Test network names remain explicit; a Bitcoin-shaped mark cannot distinguish mainnet from testnet on its own.

Backup not confirmed, backup stale and status unknown are different from verified readiness. Show only metadata safely available before unlock; do not unlock or read protected contents to decorate the catalog. No Vault configured is neutral capability information, such as “Spark only” when that capability is known. It is not a failed backup or a security emergency.

An aura may reinforce a selected state or a protection warning, but text and a stable icon/badge carry meaning. Avoid a separate simultaneous glow for every property. Preserve Cube identity colors if user-customizable colors already exist; do not overload them with severity and network meaning.

## Network catalog proposal

Recommend initially showing all local Cubes across supported networks. Offer remembered visibility filters for live chains, test networks and specific chains, with active-filter indication, hidden-result counts and Clear filters. Bitcoin mainnet and Bitcoin Blake2b mainnet are both live. Distinguish no Cubes from no filter matches, and keep create/import visible.

Catalog filtering only changes visibility. It must not change Cube network, aggregate balances across chains, reuse backend/signing credentials across chains or bypass network admission/security checks. Creation chooses its chain/network explicitly; importing/restoring validates the file/network. The existence of a single safe cross-network metadata catalog must be validated against source and protected storage before implementation.

## Usability and platform constraints

Fixed camera; readable labels outside the 3D material; conventional pointer and keyboard activation; focus follows the same catalog order; screen-reader names expose the same meaningful metadata; reduced motion removes movement without losing state. A compact presentation supports large catalogs and modest hardware. Backing up/unlocking remains a deliberate authenticated workflow, separate from object selection animation.

Use Rust/Iced desktop constraints when judging Claude’s proposals. A WebGL prototype proves composition and motion intent, not native integration, GPU compatibility or frame rate. Validate native startup, resizing/high DPI, obscured/minimized behavior, 1/20/100 object counts, reduced motion, unavailable GPU and keyboard interaction before accepting a production renderer.

## Recommended native rendering approach — evidence checked 2026-10-21

Tenshu’s inspected manifest uses Iced 0.14 with wgpu and canvas enabled; Cargo.lock pins wgpu 27.0.1. Recommend a small scene inside Iced’s shader widget, reusing Iced’s existing GPU device/queue and render lifecycle. Use a simple shared beveled-cube mesh rendered as instances, a floor plane, a fixed orthographic or shallow-perspective camera, restrained lighting, inexpensive contact-shadow treatment and limited selection/open animation. Keep text, filters, focus, status badges and create/open actions as ordinary Iced controls. The GPU scene consumes a sanitized presentation model, not wallet services or credentials.

Implement geometry/transforms/material effects with WGSL and the shader Primitive/Pipeline interfaces; use a compatible math crate such as glam for transforms if needed. Match Iced’s re-exported wgpu types rather than adding an unrelated latest wgpu version. Prototype the depth/compositing path with a custom pass or offscreen target as needed; do not assume Iced’s shared UI render pass supplies the desired 3D depth attachment. Keep the real-time effect bounded and suspend redraw when idle/hidden. No benchmark or native 3D spike has been run yet.

An isometric Canvas/SVG/pre-rendered fallback remains useful for reduced rendering capability and a compact list. Reduced motion itself can keep the real 3D scene static; it does not require changing the renderer. A full game-engine integration is not justified by this fixed-camera catalog alone. If rich free navigation/physics/complex environments later become requirements, revisit that architecture separately.

Verified primary references: [Iced 0.14 shader Primitive](https://docs.rs/iced/0.14.0/iced/widget/shader/trait.Primitive.html), [Iced shader Pipeline](https://docs.rs/iced/0.14.0/iced/widget/shader/trait.Pipeline.html), [glam](https://docs.rs/glam/latest/glam/). Repository evidence: coincube-gui/Cargo.toml:41–52 and Cargo.lock’s iced/iced_wgpu/wgpu entries. The dependency check is not a GPU compatibility test.
