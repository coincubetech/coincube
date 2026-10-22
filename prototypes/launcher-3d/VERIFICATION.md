# Prototype verification — 2026-10-21

## Scope

Synthetic standalone Launcher only. The native application was built and opened on this Mac Studio using Homebrew Rust/Cargo 1.94.0. Parent Rust 1.97.1, Windows and Linux are unverified. No wallet, account or network service is connected.

## Automated validation

- Release build and macOS local bundle/ad-hoc signing succeeded.
- `cargo fmt --manifest-path prototypes/launcher-3d/Cargo.toml -- --check` passed.
- `cargo clippy --locked --manifest-path prototypes/launcher-3d/Cargo.toml --all-targets -- -D warnings` passed.
- `cargo test --locked --manifest-path prototypes/launcher-3d/Cargo.toml`: 10 passed.
- Tests cover filter semantics, unique synthetic identity and seven-per-network capacity, bounded measurements, finite mesh geometry, projected picking at two scene sizes and 1/4/12/20/21 counts, camera depth range through21 Cubes, discarding timing samples when Clear filters changes scene load, and clearing stale collecting status when sampling pauses or stops.

## Native observations

Native review at code `93e3f278ee4dc7292dfb07f9310368388b8e7fe8` completed the compact gate. Lab-only F4/F5 load 12/20 fixtures; F6/F7 request 1024×700 / 1440×900 logical content sizes through Iced's window API. Native captures measure 2048×1464 / 2880×1864 pixels including the title bar at 2× display scale. This bypassed the automation driver's failing window-resize drag without simulating the app.

At both sizes, 12/20 geometry, toolbars, selection details and Open preview fit. The four-Cube selected label is fully contained without the earlier overlap. Live chains shows 14 of 20; Blake2b shows 7 of 20; Clear restores 20. Creation is disabled at 7/7 on the selected network. Dark/light views and Float plus Reduce motion were inspected. Arrow selection and Enter preview work. Three Tab presses produced no visible focus; source review confirms missing broader focus traversal. Selection of Cube 012/020 leaves its catalog entry offscreen, and all scene labels are suppressed above eight objects. Compact catalog exposes about two complete rows. These are refinement findings, not clipping failures.

The two independent Impeccable assessments support retaining the 3D direction, with selected-object identity/catalog synchronization, complete keyboard operation, and catalog density prioritized before integration. One proposed light-theme blank-button finding was rejected: the native capture visibly contains the disabled Create Cube text. Capacity explanation can still be clearer. No design refinements have been implemented in this review pass.

Native captures were made before a subsequent status-only repair: CodeRabbit correctly identified that reset metrics displayed “Collecting fresh samples…” even while sampling was stopped. The shared sampling condition now selects the truthful idle text; a transition regression covers benchmark, float/reduced motion and focus pause/resume. No geometry/layout changed. Final native smoke verification is recorded below after rebuilding.

An earlier debug capture displayed 20 Cubes with Float and Benchmark enabled: approximately 59.8 redraw events/s, mean interval 16.72 ms, p95 interval 19.60 ms. These are observational debug-build redraw measurements, not a release benchmark or presented GPU FPS. Before Robert clarified the Estate limit, a release sample with100 shown, Benchmark on and Float off recorded 60.0 redraws/s, mean 16.67 ms, p95 17.16 ms, CPU prepare 0.147 ms and CPU encode 0.005 ms at 1440 × 900 logical window size (2880 × 1864 captured pixels including title bar). This single-machine sample does not establish a performance target. No GPU execution, presentation latency, memory peak, power or cross-device measurements were made.

Local native evidence is outside Git in the Codex artifact folder `ux-review/launcher-qa-20261021/`: `12-compact.jpg`, `12-compact-last-selected.jpg`, `12-compact-tab.jpg`, `12-ordinary.jpg`, `20-ordinary-last-selected.jpg`, `20-compact-last-selected.jpg`, `20-compact-preview.jpg`, `4-compact-label-check.jpg`, `20-filter-live-14.jpg`, `20-filter-blake-7.jpg`, `20-compact-light.jpg`, `20-compact-light-reduced-motion.jpg`, and restored `12-compact-review-default.jpg`. These are actual native captures, not browser replicas or edited layouts.

## Independent review

Fresh capable Codex subagent `/root/launcher_independent_review` reviewed frozen code `e72aacfc9d093a5e1af84cb10114a0597b90cb8f` after writer release. Its only confirmed code finding was Clear filters retaining timing samples from the previous load. A regression test failed before the repair (render count remained1), then passed after resetting samples when Clear changes the filter. All8 tests, Clippy, formatting and release packaging pass after this bounded first repair batch. Exact repaired-head review is recorded on the draft PR.

Fresh Assessment B `/root/launcher_evidence_12_20` independently accepted the Lab shortcut delta at `93e3f278ee4dc7292dfb07f9310368388b8e7fe8`. Assessment A `/root/launcher_design_12_20` completed before B findings entered synthesis; neither saw the other's assessment. Parent alone drove the native app, and both independently inspected source and captures. The web detector ran against main.rs and returned `[]`, exit 0, but this native target has zero eligible web markup files: no native Rust/WGSL scan coverage or clean-native certification is claimed. No browser overlay/server applies.

Impeccable finish disposition: **fix before integration**. Compact fixture fit passes; production keyboard access and dense selection identity require refinement. This supersedes the earlier recapture disposition. Heuristic score: 25/36 (error recovery outside the synthetic lab's scope). This score is a design assessment, not a measured task-success rate.

## Owner clarification

Robert clarified Estate permits7 Cubes per network and realistic totals are10–20. The current lab has Empty/1/4/12/20 presets, enforces7 per synthetic network independently, and derives GPU instance capacity from the21-Cube fixture maximum. The earlier100-Cube sample is historical stress evidence, not a layout or product-capacity target. Production subscription enforcement remains outside this standalone lab.

## Review boundary

This is ready for prototype exploration, not Tenshu integration. The requested compact/12–20 review is complete for the tested English fixtures. Long names, localization, text scaling, complete assistive technology operation, exact floating-silhouette picking and cross-device performance remain unverified. Native accessibility semantics, a GPU-unavailable/compact fallback and exact animated-surface picking remain explicit follow-up work.

Task origin: Robert’s local Codex request, exact branch `3d`, COINCUBE workspace `/Users/macstudio/git/coincubetech`; startup instructions `/Users/macstudio/git/coincubetech/AGENTS.md`, company-brain startup files and repository `CLAUDE.md` were read. Shared Agent Ops policy was read from reviewed main commit `b4e4fa3c3ecedad93f40d08f71baca7cea707a25`. No originating Buzz link was provided.
