# Prototype verification — 2026-10-21

## Scope

Synthetic standalone Launcher only. The native application was built and opened on this Mac Studio using Homebrew Rust/Cargo 1.94.0. Parent Rust 1.97.1, Windows and Linux are unverified. No wallet, account or network service is connected.

## Automated validation

- Release build and macOS local bundle/ad-hoc signing succeeded.
- `cargo fmt --manifest-path prototypes/launcher-3d/Cargo.toml -- --check` passed.
- `cargo clippy --locked --manifest-path prototypes/launcher-3d/Cargo.toml --all-targets -- -D warnings` passed.
- `cargo test --locked --manifest-path prototypes/launcher-3d/Cargo.toml`: 7 passed.
- Tests cover filter semantics, unique synthetic identity and capacity, bounded measurements, finite mesh geometry, projected picking at two scene sizes and 1/4/20/100 counts, and camera depth range through 500 Cubes.

## Native observations

The final release build renders the four-Cube catalog, beveled geometry, floor, network labels and selection. Arrow-key selection was confirmed in the final app. An earlier native compact-window check revealed overlapping object labels; source now limits compact object labels to the selected Cube while retaining the catalog. Final compact recapture is pending because native pointer automation stopped resolving the app window, although keyboard input and screenshots remained available intermittently. Do not treat this as a completed compact acceptance check.

An earlier debug capture displayed 20 Cubes with Float and Benchmark enabled: approximately 59.8 redraw events/s, mean interval 16.72 ms, p95 interval 19.60 ms. These are observational debug-build redraw measurements, not a release benchmark or presented GPU FPS. A controlled release 100-Cube sample remains pending. No GPU execution, presentation latency, memory peak, power or cross-device measurements were made.

Local task captures are outside the repository to keep private UX-review material out of Git. The final ordinary release capture is `launcher-3d-release-final.jpg`; earlier compact and debug-20 captures are intermediate evidence, not final acceptance.

## Review boundary

This is ready for prototype exploration, not Tenshu integration. Review empty/filtered-empty creation, filter changes, 20/100 loads, compact sizing, light mode and reduced motion in the running app before choosing production behavior. Native accessibility semantics, a GPU-unavailable/compact fallback and exact animated-surface picking remain explicit follow-up work.

Task origin: Robert’s local Codex request, exact branch `3d`, COINCUBE workspace `/Users/macstudio/git/coincubetech`; startup instructions `/Users/macstudio/git/coincubetech/AGENTS.md`, company-brain startup files and repository `CLAUDE.md` were read. Shared Agent Ops policy was read from reviewed main commit `b4e4fa3c3ecedad93f40d08f71baca7cea707a25`. No originating Buzz link was provided.
