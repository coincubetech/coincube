# Multi-signer Mobile signing flow

How a desktop user collects signatures from contacts whose private keys
live in the COINCUBE Mobile app on their phones. Covers what's in the
desktop today (Phases 1–4 of the cross-repo signing-flow rollout); the
companion phone-app and API plans live alongside it under
`plans/PLAN-phase-N-*.md`.

If you're debugging a stuck session, jump to
[Debugging](#debugging-stuck-sessions) — it walks the log lines and
settings panel.

## Architecture

```text
┌────────────────────────────────────────────────────────────────────┐
│ coincube-gui (Iced app)                                            │
│                                                                    │
│  ┌────────────────┐    ┌──────────────────────┐                    │
│  │ PsbtState      │───▶│ KeychainSignModal    │                    │
│  │ (sign modal)   │    │ (orchestrator)       │                    │
│  └────────────────┘    └──────────┬───────────┘                    │
│         ▲                         │                                │
│         │                         ▼                                │
│   "Sign" → "Request"         ┌──────────────────────┐              │
│   button                     │ GrpcSessionClient    │              │
│         │                    │ ::resolve_signers    │              │
│         │                    │ ::create_signing_... │              │
│         │                    │ ::get_signing_...    │              │
│         │                    │ ::cancel_signing_... │              │
│         │                    └──────────┬───────────┘              │
│         │                               │                          │
│         │  Message::KeychainSign(       │                          │
│         │      StreamEvent(_)) ◀────────┤                          │
│         │                               │                          │
│         │                               ▼                          │
│                              ┌──────────────────────┐              │
│                              │ connect_stream       │              │
│                              │ (Iced subscription)  │              │
│                              └──────────────────────┘              │
└────────────────────────────────────────────────────────────────────┘
                                          │
                                          ▼ gRPC (TLS)
                              ┌──────────────────────┐
                              │ coincube-api         │
                              │ — RealtimeService    │
                              │ — SessionService     │
                              │ — DeviceService      │
                              └──────────────────────┘
                                          ▲
                                          │ push / poll
                                          ▼
                              ┌──────────────────────┐
                              │ COINCUBE Mobile app  │
                              └──────────────────────┘
```

Three independent gRPC services on the API:

- **DeviceService** — `RegisterDevice` for first-launch device
  registration. Idempotent: returns the existing row when the desktop
  re-launches.
- **SessionService** — `ResolveSigners`, `CreateSigningSession`,
  `GetSigningSession`, `CancelSigningSession`, plus the phone-side
  approve/sign RPCs.
- **RealtimeService** — a single long-lived bidi stream that pushes
  `SessionEvent`s to the desktop.

## One pipeline, a folder per chain

Every Cube runs the same signer bootstrap and stream, and keeps its Connect
session, signer device id and event cursor in one file shape, the
`connect.json` of its chain's folder: `bitcoin/` (and the other Bitcoin
networks' folders) for a Bitcoin Cube, `bitcoin-blake2b/` or
`bitcoin-blake2b-testnet4/` for a Bitcoin Blake2b Cube. The transport key
sits beside it, so a Bitcoin Blake2b Cube registers its own signer device,
named with its chain. The pipeline's inputs are the Cube's `ChainId` (folder
and network name) and its Connect endpoint (`App::connect_base_url`).

A Bitcoin Cube's file is written by its in-Cube sign-in. A Connect-served
Bitcoin Blake2b Cube writes its own session there at open and its pipeline
stays pinned to that session while it is open
([`BTCB2_VAULT_APP.md`](BTCB2_VAULT_APP.md)). A local Bitcoin Blake2b Cube
(on its own node, or with no Vault yet) writes it from its in-Cube sign-in,
exactly as a Bitcoin Cube does.

## Happy-path sequence

User has a Vault with two Mobile signers (themselves on this
desktop's Connect account, and one contact "Alice"). They click
"Sign" on a draft spend and then "Request" on a Mobile signer's row,
which opens the "Sign via Mobile" modal.

1. `KeychainSignModal::launch` →
   - REST `GET /connect/cubes/{cube_id}/vault` — returns the vault
     row with its `members` list.
   - REST `GET /connect/cubes/{cube_uuid}/keys` — returns every
     Mobile key on the cube, including fingerprints.
   - REST `GET /me` — viewer user id (used to split self vs. contact
     signers).
   - In-process: `build_keychain_index()` joins members with cube keys
     by `key_id`, producing a `Fingerprint → KeychainSignerInfo` map.
     The map's `fingerprint` is derived from the descriptor's
     `[origin]xpub` notation, *not* from `VaultMemberKeySummary` (which
     doesn't carry the fingerprint — see Phase 2 audit findings).
   - In-process: `classify_signers()` walks the descriptor + PSBT and
     returns the still-required signers as `RequiredSigner::Local` or
     `RequiredSigner::Keychain`.

2. `KeychainSignModal::on_classified` → `SessionService.ResolveSigners`
   for the vault id. The API answers with one `SignerTarget`
   (`device_id`, `key_id`, `key_fingerprint`) per resolvable signer
   and an `UnresolvedSigner` list for owners with no usable device.

3. For each target the desktop calls
   `SessionService.CreateSigningSession`. Each call returns a
   `SigningSession.session_id` which we stash on the corresponding
   `PendingSession` row. Status starts as `Pending`.

4. The phone app picks up the session via its own realtime stream and
   shows it to the owner. As the owner interacts, the API emits
   `SessionEvent`s on the desktop's stream:
   `SESSION_DELIVERED → SESSION_VIEWED → SESSION_APPROVED →
   SIGNATURE_SUBMITTED → SESSION_COMPLETED`.

5. On `SIGNATURE_SUBMITTED` the desktop calls
   `SessionService.GetSigningSession` to pull the partially-signed
   PSBT, decodes it, and merges via `Daemon::update_spend_tx` — the
   same merge path the local hardware-wallet flow uses.

6. Once every `PendingSession` is `Completed` the modal auto-closes,
   `tx.sigs` is recomputed against the merged PSBT, and the spend's
   action button switches from "Sign" to "Broadcast". The existing `BroadcastModal` handles the rest.

## Failure modes & UX

| Failure                       | Surface                                                                                          |
| ----------------------------- | ------------------------------------------------------------------------------------------------ |
| ResolveSigners `unresolved`    | Friendly per-signer message ("Alice hasn't set up COINCUBE Mobile yet…")                         |
| CreateSigningSession fails    | Per-row Failed status + Retry button; modal stays open                                            |
| SessionRejected               | Per-row Rejected + Retry button + "Alice declined the request"                                   |
| SessionExpired                | Per-row Expired + Retry button + "Alice didn't respond within 24h"                               |
| Stream drops mid-flight       | Modal banner: "Connection lost — reconnecting. Sessions are still active server-side…"           |
| Token expired                 | Modal banner: "Your Connect session has expired. Please sign in again." + phase → AllDone        |
| PSBT merge fails              | Per-row Failed + the merge error string (defensive — shouldn't happen in practice)               |

The full list lives at the top of
[`coincube-gui/src/app/state/vault/keychain_sign.rs`](../coincube-gui/src/app/state/vault/keychain_sign.rs).

## Status surfaces

- **Sidebar dot** (under the cube name): green/amber/red dot reflecting
  the realtime stream's health. Tooltip shows the latest state.
  Implementation: `coincube-gui/src/app/view/nav/mod.rs`
  (`connect_status_dot`).
- **Settings → About**: Connect Device card with `device_id`, account
  email, current stream status, and a "Re-register this device" button.
  Implementation: `coincube-gui/src/app/view/settings/about.rs`
  (`connect_device_card`).

## Debugging stuck sessions

When a user reports "I clicked sign and nothing happened":

1. **Check the sidebar dot.** Amber means the stream is reconnecting;
   red means it's errored. The tooltip carries the latest error.
2. **Check Settings → About → Connect Device.** The `device_id` should
   be populated. If empty, the desktop never registered — kick a
   re-register via the button.
3. **Grep the logs for the session_id.** Every signing-related log line
   uses target `coincube_gui::signing` and includes `session_id=` and
   `vault_id=` structured fields:

   ```sh
   journalctl --user -u coincube | grep coincube_gui::signing | grep session_id=abc-123
   ```

   The full lifecycle (`Classification complete → ResolveSigners
   returned → Signing session created → SessionEvent received →
   Merging signed PSBT`) should be reconstructable from those lines
   alone.

4. **Check the phone end.** If the desktop never sees
   `SESSION_DELIVERED`, the API hasn't routed the session to the phone.
   That's a phone-app or API problem, not a desktop one.

## Known limitations

- **No push notifications yet.** The phone signer must be in the
  foreground (or open the app) for sessions to be delivered promptly.
  Push support is in the `coincube-mobile` and `coincube-api` plans.
- **PSBTs are sent plaintext over TLS.** Per-session payload encryption
  is on the `coincube-api` roadmap (PR 3 in their plan).
- **`subscribe_vault_ids` is best-effort.** If the cube hasn't been
  registered server-side yet, the stream subscribes to all events for
  the user — slightly noisier than necessary but functionally fine.
- **No "resume from pending sessions on launch."** When the user
  reopens the app mid-flow, they have to re-open the spend to see
  pending sessions. Tracked under Phase 4 PR 3 as a follow-up.

## Related plans

- `plans/PLAN-phase-1-grpc-plumbing.md` — gRPC client + stream basics.
- `plans/PLAN-phase-2-vault-builder-audit.md` — Vault Builder Mobile key
  picker (already complete on master).
- `plans/PLAN-phase-3-signing-flow-keystone.md` — this feature's
  implementation plan.
- `plans/PLAN-phase-4-production-polish.md` — the polish work that
  shipped alongside this doc.
