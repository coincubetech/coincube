# Prompt: fix Tenshu’s contradictory Connect sign-in state

You are assigned to investigate and fix a Tenshu Connect session bug in the COINCUBE `coincube` Rust/Iced desktop application. Deliver a minimal, tested fix through a draft PR. Keep this separate from the navigation/Settings makeover underway. Do not redesign the app or fix unrelated UX issues.

## Startup and ownership

Workspace: `/Users/macstudio/git/coincubetech`. Target repository: `/Users/macstudio/git/coincubetech/coincube`; verify its remote is `coincubetech/coincube`. Read the absolute workspace `AGENTS.md`, then the required company-brain startup files in order: `MEMORY.md`, `USER.md`, `AGENTS.md`, `PROJECTS.md`, and `projects/coincube-desktop/PROJECT.md`. Read the repository’s `CLAUDE.md` and applicable local instructions.

Load shared Agent Ops policy as instructed by the workspace. Prior discovery pinned reviewed main commit `b4e4fa3c3ecedad93f40d08f71baca7cea707a25` from `/Users/macstudio/git/tirith-tech/agent-ops` (verified origin `git@github.com:tirith-tech/agent-ops.git`). Verify/fetch the current policy and record the full commit you actually read; read committed `AGENTS.md`, `README.md`, `companies/coincube.json`, `contracts/task.md`, `docs/task-routing.md`, `workflows/delivery.md`, and `workflows/pr-review-repair.md`. Record startup evidence and limitations rather than claiming unread policy.

Check existing claims, branches, issues and PRs for this defect before writing. Use your own isolated `codex/` task branch/worktree from the appropriate verified base; preserve all unrelated work. Do not write into the active design worktree `.worktrees/tenshu-ux-concepts`. Prior source inspection used `8495c3c0f233650b8642363c455ccd429b71bc47`; establish current source and binary provenance before diagnosing.

## What was actually observed

Native macOS app bundle ID `io.coincube.tenshu`, title `Tenshu v1.0.0`, during a live walkthrough on 2026-10-21. The runtime binary’s source SHA was **not independently established**; version text alone is insufficient.

1. A Bitcoin-mainnet Cube named Family was already unlocked in an App tab. Spark → Settings → Lightning Address displayed “Sign in to Connect.”
2. The user signed in through a separate Home tab.
3. Home then displayed authenticated account Overview, Contacts, Plan & Billing, and Security, including an account plan.
4. Returning to the already-open Family tab, Spark → Settings → Lightning Address still displayed “Sign in to Connect,” while that window’s bottom status said “Signed in.”
5. Cube → Settings → Recovery also continued to offer Sign In and a plan-upgrade message.

This proves contradictory visible state. It does **not** establish the cause, the account’s entitlement to every feature, whether a backend check failed, or whether reopening the Cube would resolve it. No sign-out, restart, Cube reopening, device re-registration, backup creation or wallet mutation was attempted during discovery.

Local evidence, if available to your session: `/Users/macstudio/.codex/visualizations/2026/10/21/01a15ff7-df23-78c1-9f8b-785e87568a04/ux-review/live-walkthrough.md`, finding L11. Screenshots `live/25-connect-overview.jpg`, `live/29-lightning-address-after-signin.jpg`, and `live/30-cube-recovery-after-signin.jpg` contain private account/wallet metadata; inspect locally and redact before any publication. If these files are unavailable, the reproduction above is the handoff evidence, not a reason to invent observations.

## Investigation leads, verified in the prior source reference

- `coincube-gui/src/home.rs`: successful transition already emits `ConnectSignedInBubble`; `gui/tab.rs` forwards it and `gui/pane.rs` broadcasts ConnectAccount Init to open Cube tabs. Do not assume this broadcast is absent.
- `gui/mod.rs`: shared-session adoption, global auth-change handling, and `refresh_sidebar_session_status`. The sidebar deliberately summarizes app-wide sign-in, separately from an individual Cube’s service readiness.
- `app/cache.rs` and `app/view/mod.rs`: `sidebar_connect_signed_in` differs from `connect_authenticated`.
- `app/mod.rs::connect_settings_content` gates Lightning Address on the local account panel’s `is_authenticated`; `app/view/settings/general.rs::recovery_section` uses `cache.connect_authenticated`.
- `app/state/connect/account.rs`: `authenticated_client`, `is_authenticated`, Init/refresh/session-loaded handling, shared-client adoption, epochs/generations, duress checks and admitted/reopen-required guards. `app/mod.rs` mirrors account state into the cache and updates Cube services. `services/coincube/client.rs` owns API/session operations.

Trace these paths and existing regression tests before changing them. They are leads, not an asserted root cause or a prescribed patch. Identify which state/event fails to converge and whether the observed binary predates existing fixes.

## Required behavior and regression coverage

After successful sign-in, an existing ordinary Bitcoin Cube must re-evaluate its applicable account-backed settings without requiring an unexplained second sign-in. Keep three facts distinct: account authentication, this Cube’s service connection/registration, and verified feature entitlement. If a Cube genuinely requires reconnect/reopen, an account check is pending, or a request fails, show the truthful specific state/action. Never make a feature usable merely because another tab says “Signed in,” and never treat unknown entitlement as a confirmed need to upgrade.

Add focused behavioral regression tests that fail before the fix and pass afterward. Cover the already-open Cube/Home login sequence, multiple Cube tabs and panes, a Cube opened after sign-in, session restore/token refresh with concurrent completions, explicit sign-out and account change, stale async results, unauthorized/expired tokens, transient backend failure and retry, and loading/unknown versus denied entitlement. Select the affected scenarios based on the fix and explain coverage. Preserve security behavior for duress, account ownership, fork/Bitcoin Blake2b admission and claim invalidation; do not weaken those guards or blindly copy an arbitrary sibling token into every service.

Prefer fakes/mocked clients and synthetic credentials. Never log, paste or commit tokens, OTPs, cookies, seeds, PINs, xprvs, recovery material or raw API credentials. Keep diagnostics to safe state names, event IDs and redacted metadata. Use a separate disposable test datadir/account for live validation. Preserve the user’s open Cubes, sign-in and data; have the user enter credentials if needed. No funds transfer, wallet deletion, recovery reveal/export, device deletion/re-registration, billing change or production database/payment mutation is needed to prove this fix.

Follow repository build guidance and run appropriate focused tests, formatting/lint checks and required CI; report actual commands, outcomes, exact tested SHA and unavailable checks. Before a large Rust build, follow the workspace artifact-size policy. Do not run live API integration tests without the required scoped test environment.

## Delivery

Explain the reproduced trigger, root cause supported by code/evidence, and resulting behavior. Commit, push normally, open a draft PR and follow the COINCUBE review/repair workflow. Request eligible supplemental reviews and obtain a capable independent review of the exact final commit. Record unavailable reviewers/monitoring honestly and escalate a defect surviving two attempted fixes. No force-push, merge or deployment. Return the draft PR, tested SHA, regression evidence, residual limitations and any safe manual verification still needed. If the bug is already fixed on current source, prove that with a known build and regression evidence and report the runtime/source mismatch instead of creating a redundant patch.

---

Preparation evidence (2026-10-21): the handoff author read the absolute workspace AGENTS.md; the five brain paths listed above; `coincube/CLAUDE.md`; all seven committed Agent Ops files at `b4e4fa3c3ecedad93f40d08f71baca7cea707a25`; L11 and its surrounding coverage notes; and the source paths listed in the investigation leads. Origin was independently checked; the parent task’s pinned-policy fetch was reused. Source HEAD of the read-only design worktree was verified as `8495c3c0f233650b8642363c455ccd429b71bc47`. No application code, UI state, credential store or GitHub state was changed to prepare this prompt.
