# Coincube Codebase Orientation Guide

> Personal notes prepared for local use. Keep this file outside of version control (added to `.gitignore`).

## 1. Workspace Layout

```
coincube/
├─ coincube-core/        # Shared domain logic, descriptors, Miniscript helpers
├─ coincubed/            # Vault daemon (JSON-RPC server, scheduler, rescan jobs)
├─ coincube-gui/         # Installer + desktop app built with Iced
├─ coincube-ui/          # Reusable UI components + theming
├─ docs/, contrib/, tests/, fuzz/ ...
└─ Cargo.{toml,lock}     # Workspace manifests + overrides
```

### Crate roles
- **coincube-core**: Pure Rust logic. Expect no GUI/daemon deps. Great place to add business rules or Miniscript utilities that can be reused.
- **coincubed**: Long-running services (address derivation, wallet sync, spend creation). Exposes RPC via Unix socket. GUI talks to it through `Daemon` trait implementations.
- **coincube-gui**: Most day-to-day work happens here. Two subareas:
  - `installer/` wizard for first-time setup.
  - `app/` actual wallet UI with state machines, menus, toasts.
- **coincube-ui**: Shared widgets (buttons, cards, toast manager) and design tokens. When UI code gets messy, look here for abstractions to reuse.

## 2. GUI Architectural Pattern

The GUI uses a clean separation between **State**, **View**, and **Message**:

1. **State structs** live in `app/state/**`. Each implements:
   - `fn view(&self, menu, cache) -> Element<Message>`
   - `fn update(&mut self, daemon, cache, message) -> Task<Message>`
   - optional `subscription` and `reload` hooks.
2. **Views** live in `app/view/**`. They are mostly pure functions that build widgets and emit `view::Message` variants.
3. **Messages** (`app/view/message.rs`) tie everything together. `App::update` translates top-level messages to panel-specific updates.

### Checklist when adding a feature
1. Add/extend a `view::Message` variant.
2. Wire UI controls to emit that message.
3. Handle it inside the appropriate `State::update` arm.
4. If asynchronous work is needed, return a `Task` (usually via `Task::perform` or `Task::future`).
5. When background work finishes, send another message that the state can handle synchronously.

### Panel lifecycle
- Panels live inside `app/state/mod.rs` under `Panels` struct. They are `Box<dyn State>` objects swapped via menu selections.
- When a panel needs modal UI, define a local enum (e.g., `Modal::VerifyAddress`) and render via `modal::Modal::new(...)`.
- Toasts follow a shared pattern: keep `Option<String>` + wrap view with `toast::Manager`. For timed dismissal, spawn a delayed `Task` that sends a `ClearToast` message.

## 3. Messaging Patterns

| Layer | File(s) | Purpose |
| --- | --- | --- |
| `view::Message` | `app/view/message.rs` | Pure UI intents. Think "button X clicked". |
| `App::update` | `app/mod.rs` | Global routing, side effects (clipboard, nav guards). |
| Panel states | `app/state/**` | Business logic per screen. |

Tips:
- Keep global handlers (like `view::Message::Clipboard`) minimal so panels can intercept their own specialized messages.
- When adding nested enums (e.g., `VaultReceiveMessage`), always match them in both `view::Message` and the corresponding state.

## 4. Working with the Vault Daemon

- `Daemon` trait (in `coincubed`) abstracts RPC calls (get new address, list coins, etc.).
- GUI obtains a `Arc<dyn Daemon + Sync + Send>` via `self.daemon` inside `App`.
- Every state method receives `daemon: Option<Arc<...>>`; most vault panels expect it to be `Some` and will `expect` otherwise. Guard upstream if you need optional behavior.

Common async pattern:
```rust
Task::perform(async move {
    daemon.some_call().await.map_err(|e| e.into())
}, Message::SomeVariant)
```

## 5. Coin selection and spending

- Live in `app/state/vault/{coins,transactions,psbt}.rs`.
- Spend flows rely on helper structs in `coincube-core::spend` plus `coincubed` RPC.
- Hot key signing and hardware verification go through `hw/` module (Jade, COLDCARD, etc.).

## 6. Liquid wallet integration

- `app/state/liquid/**` contains Breez SDK wrappers.
- Use `BreezClient` (Arc) stored in `App`. Many tasks use `Task::perform` to drive asynchronous SDK calls.
- Keep an eye on `#[cfg(feature = "meld")]` sections if you enable additional features.

## 7. Installer

- `installer/state/**` and `installer/view/**` mirror the main app structure but focused on onboarding.
- Installer actions often emit `Message::Installer(InstallerMessage)` which `App` handles before the main panels are built.

## 8. UI components + Styling

- All UI primitives (buttons, cards, text helpers) live in `coincube-ui`. Reuse them instead of crafting ad-hoc styles.
- For colors/text sizes, prefer the helpers (`h3`, `p2_regular`, etc.) instead of raw `Text::new`.
- Toasts use `coincube_ui::component::toast::Manager` + `view::simple_toast` helper.

## 9. Testing + Tooling

- **Unit/integration tests**: `cargo test -p coincubed` or `cargo test -p coincube-gui`.
- **GUI smoke run**: `cargo run -p coincube-gui` (requires Breez SDK prerequisites).
- **Daemon only**: `cargo run -p coincubed -- --help`.
- **Lint/format**: `cargo fmt`, `cargo clippy --workspace --all-targets`.

## 10. Troubleshooting Checklist

1. **Missing imports**: Many modules prefer explicit `use crate::app::view::message::{...}`. Search existing files for patterns.
2. **Task return types**: Every `update` arm must return `Task<Message>`; use `Task::none()` or `Task::batch` to satisfy the compiler.
3. **Menu guards**: See `App::update` `Message::View(view::Message::Menu(..))` for gating actions (e.g., vault must exist before send screens).
4. **Clipboard flows**: Decide whether the action is panel-specific. Use custom messages if you need to show UI feedback.
5. **Async race conditions**: Use `Task::future` for timers and `Task::perform` for RPC calls. Always map results back into `Message` so state stays deterministic.

## 11. Suggested Navigation Strategy

- Start from `App::update` to understand top-level behavior.
- Jump into `app/state/<area>` for logic, then `app/view/<area>` for UI.
- Use `grep` for `Message::XYZ` to trace how a feature flows end-to-end.
- When dealing with daemon interactions, inspect the RPC call in `coincubed/src/daemon` to see what data structures look like.

Keep iterating on these notes as you explore more subsystems.
