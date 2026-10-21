# Tenshu 3D Launcher Lab

Standalone native Iced/wgpu exploration of the separate Cube Launcher. It is an independent Cargo workspace and binary: it has no dependency on Tenshu's wallet/daemon crates, Breez, Connect clients, real Cube storage, keys or `.env`. Everything is an in-memory synthetic fixture. The real Tenshu application is unchanged.

## Run

From the repository root:

```sh
cargo run --release --locked --manifest-path prototypes/launcher-3d/Cargo.toml
```

On macOS, optionally package an ad-hoc-signed local app:

```sh
bash prototypes/launcher-3d/package-macos.sh
open prototypes/launcher-3d/target/TenshuLauncherLab.app
```

Quit an already-running Lab before launching an updated bundle; macOS may otherwise reuse the existing process. The bundle has its own identifier, `io.coincube.tenshu.launcherlab`, and does not replace or sign Tenshu. This local bundle is not notarized or a distribution release. `target/` remains ignored. Build artifacts stay in this prototype's target directory; no root wallet build is required.

The crate pins Iced/iced_wgpu 0.14.0 and glam 0.25.0, with its own committed Cargo.lock. The parent repo requests Rust 1.97.1; this Mac currently invokes Homebrew Rust/Cargo 1.94.0, on which the prototype was built and tested. Validation with the parent toolchain and on Windows/Linux remains separate work.

## Review controls

- Fixture load: empty, 1, 4, 20 or 100 Cubes. Loading a preset discards previous in-memory fixture edits. The synthetic catalog caps creation at 500.
- All networks initially includes Bitcoin mainnet, Bitcoin Blake2b mainnet and Testnet4. Live chains includes both mainnets. Filters only change catalog visibility. Counts show hidden entries and Clear filters restores the full catalog.
- Create on explicitly chooses the synthetic network. Create Cube adds an in-memory fixture; it creates no file or key. If the chosen chain was hidden, the catalog returns to All networks so the new Cube is visible.
- Click a Cube or its name/catalog entry to select it. Arrow keys move selection; Enter opens the selected preview. The right-hand catalog is available at high counts where labels on every object would overlap.
- Open preview explains the next real-world unlock step. It performs no authentication, wallet loading, account access or transaction.
- Float enables gentle motion. Reduce motion disables float and selection interpolation; benchmarking can still redraw a static scene.
- Light theme changes only the Lab. Benchmark requests continuous redraw while the Lab is focused. Unfocused animation/benchmark pauses. Default idle rendering requests no continuous animation.

Protection states and No Vault are readable text. Colors distinguish synthetic networks and support selection, but are not the only source of identity/status. No official Blake2b logo or unverified security claim is invented. The labels/catalog use ordinary Iced widgets; this does not establish native assistive-technology conformance.

## Rendering

One reusable rounded Cube mesh, GPU instance data, a floor plane and inexpensive translucent contact-shadow quads are drawn by an Iced shader primitive. The scene reuses Iced's GPU device/queue and target. Its own depth attachment matches the physical window size, with viewport/scissor set to the scene region. Camera fitting and labels use the same projection. The camera is fixed and adapts to catalog size; no orbit/pan gesture is necessary.

The scene is intentionally limited: no physics, game engine, real-time shadow map, bloom, texture downloads or financial services. Picking uses projected box-face triangles as a practical approximation to the beveled mesh; small edge differences and selection/float motion are prototype limitations to evaluate. Labels remain stationary during the slight float. A dedicated compact/2.5D/GPU-unavailable fallback is not implemented yet.

## Performance measurements

Use an optimized release build and enable Benchmark. Resetting the fixture, changing filters/motion, or refocusing resets the rolling sample. Up to 300 redraw intervals and scene CPU samples are retained. The dashboard reports redraw events per second, mean/p95 interval, CPU preparation/encoding time and scene draw count. Print metrics emits a synthetic summary to stdout.

These are **not GPU timestamps, presented FPS, end-to-end latency or power measurements**. Vsync/compositor cadence, UI updates, scheduling and machine load influence redraw intervals. CPU encode time excludes asynchronous GPU execution. Do not infer battery behavior or cross-device performance from these numbers. Use Instruments/Metal GPU capture for that follow-up. The Lab dashboard is sufficient for initial comparative scene-size/motion review, not production performance acceptance.

## Validate

```sh
cargo fmt --manifest-path prototypes/launcher-3d/Cargo.toml -- --check
cargo test --locked --manifest-path prototypes/launcher-3d/Cargo.toml
cargo clippy --locked --manifest-path prototypes/launcher-3d/Cargo.toml --all-targets -- -D warnings
```

Tests cover cross-network filter semantics, bounded metrics, synthetic identity creation, finite rounded geometry and scene picking at multiple viewport/catalog sizes. Native checks should cover compact/large windows, empty and filtered-empty states, create/select/open-preview, 20/100 load, light/dark and reduced motion. Original test source and live captures belong to the task evidence; no private wallet screenshot belongs in this prototype.

## Scope and provenance

Origin: Robert's local Codex UX review conversation, 2026-10-21. Exact requested branch: `3d`. Base: `113e2ca2251d7cfb8c1d0dbba9374a79993c8f59`. Workspace instructions: `/Users/macstudio/git/coincubetech/AGENTS.md`; repo instructions: `CLAUDE.md`. Shared Agent Ops policy read at `b4e4fa3c3ecedad93f40d08f71baca7cea707a25`. No Buzz originating-thread link was supplied. Concept C remains the chosen direction inside open Cubes; this experiment reviews the separate Launcher's appearance and performance before integration.
