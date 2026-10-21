# Prototype verification — 2026-10-21

## Scope

Synthetic standalone Launcher only. The native application was built and opened on this Mac Studio using Homebrew Rust/Cargo 1.94.0. Parent Rust 1.97.1, Windows and Linux are unverified. No wallet, account or network service is connected.

## Automated validation

- Release build and macOS local bundle/ad-hoc signing succeeded.
- `cargo fmt --manifest-path prototypes/launcher-3d/Cargo.toml -- --check` passed.
- `cargo clippy --locked --manifest-path prototypes/launcher-3d/Cargo.toml --all-targets -- -D warnings` passed.
- `cargo test --locked --manifest-path prototypes/launcher-3d/Cargo.toml`: 9 passed.
- Tests cover filter semantics, unique synthetic identity and seven-per-network capacity, bounded measurements, finite mesh geometry, projected picking at two scene sizes and 1/4/12/20/21 counts, camera depth range through21 Cubes, and discarding timing samples when Clear filters changes scene load.

## Native observations

The final release build renders the four-Cube catalog, beveled geometry, floor, network labels and selection. Arrow-key selection and Enter open preview were confirmed in the release app. An earlier native compact-window check revealed overlapping object labels; source now limits compact object labels to the selected Cube while retaining the catalog. Final compact recapture is pending because native pointer automation stopped resolving the app window, although keyboard input and screenshots remained available intermittently. Do not treat this as a completed compact acceptance check.

An earlier debug capture displayed 20 Cubes with Float and Benchmark enabled: approximately 59.8 redraw events/s, mean interval 16.72 ms, p95 interval 19.60 ms. These are observational debug-build redraw measurements, not a release benchmark or presented GPU FPS. Before Robert clarified the Estate limit, a release sample with100 shown, Benchmark on and Float off recorded 60.0 redraws/s, mean 16.67 ms, p95 17.16 ms, CPU prepare 0.147 ms and CPU encode 0.005 ms at 1440 × 900 logical window size (2880 × 1864 captured pixels including title bar). This single-machine sample does not establish a performance target. No GPU execution, presentation latency, memory peak, power or cross-device measurements were made.

Local task captures are outside the repository to keep private UX-review material out of Git. The final ordinary release capture is `launcher-3d-release-final.jpg`; `launcher-3d-release-100.jpg` records the release stress sample. Earlier compact and debug-20 captures are intermediate evidence, not final acceptance.

## Independent review

Fresh capable Codex subagent `/root/launcher_independent_review` reviewed frozen code `e72aacfc9d093a5e1af84cb10114a0597b90cb8f` after writer release. Its only confirmed code finding was Clear filters retaining timing samples from the previous load. A regression test failed before the repair (render count remained1), then passed after resetting samples when Clear changes the filter. All8 tests, Clippy, formatting and release packaging pass after this bounded first repair batch. Exact repaired-head review is recorded on the draft PR.

Impeccable finish disposition: **recapture**. The ordinary capture is valid; final compact evidence is pending. Release100 benchmark evidence was subsequently captured after click input recovered, while native resize input remained blocked. No visual ship verdict is claimed.

## Owner clarification

Robert clarified Estate permits7 Cubes per network and realistic totals are10–20. The current lab has Empty/1/4/12/20 presets, enforces7 per synthetic network independently, and derives GPU instance capacity from the21-Cube fixture maximum. The earlier100-Cube sample is historical stress evidence, not a layout or product-capacity target. Production subscription enforcement remains outside this standalone lab.

## Review boundary

This is ready for prototype exploration, not Tenshu integration. Review empty/filtered-empty creation, filter changes, 12/20 loads, compact sizing, light mode and reduced motion in the running app before choosing production behavior. Native accessibility semantics, a GPU-unavailable/compact fallback and exact animated-surface picking remain explicit follow-up work.

Task origin: Robert’s local Codex request, exact branch `3d`, COINCUBE workspace `/Users/macstudio/git/coincubetech`; startup instructions `/Users/macstudio/git/coincubetech/AGENTS.md`, company-brain startup files and repository `CLAUDE.md` were read. Shared Agent Ops policy was read from reviewed main commit `b4e4fa3c3ecedad93f40d08f71baca7cea707a25`. No originating Buzz link was provided.
