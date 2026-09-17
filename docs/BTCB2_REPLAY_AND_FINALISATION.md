# BTCB2 replay model and finalisation (Lane B1.2)

Scope: how a Bitcoin Blake2b Vault spend is signed, merged, finalised and
described to the user, and why finalisation happens in `coincube-core` rather
than on a node. Companion to `BTCB2_UNIFIED_SIGHASH.md` (the digest) and
`BTCB2_CHAIN_BINDING.md` (the chain identity every arm below keys on).

With the flag off — every Bitcoin-family `ChainId` — nothing here runs: the
signing, merge, analysis and finalisation entry points dispatch to the prior
code, pinned byte for byte by `bitcoin_paths_are_unchanged` (desktop) and
`chain_keyed_spend::bitcoin_*` (daemon).

## Finalisation spike: node `finalizepsbt` (a) vs core finaliser (b)

**Chosen: (b), `coincube_core::unified_finalize`.**

| | (a) managed node `finalizepsbt` | (b) core finaliser |
|---|---|---|
| Needs a local BLAKE2b node | yes — the RPC lives on the node | no |
| Works on the approved default backend (Connect Esplora, no node) | **no** | yes — only *broadcast* is a backend call |
| Sees unified (`0x21`) signatures | yes (consensus code) | yes — from the PSBT's proprietary records |
| rust-miniscript's `finalize_mut` | not applicable | never used: its typed `partial_sigs` decoder rejects the `0x21` byte, so it cannot see a unified record and would finalise from whatever legacy signatures are present — exactly the replayable witness the unified ones exist to prevent |
| Can guarantee a verified unified witness is never dropped for a legacy one | not without re-parsing the node's output | yes — the finaliser assembles the witness itself and reports what went in |
| Verification of signatures before assembly | node | `verify_p2wsh_all_unified` for unified, BIP-143 check for legacy; refuses anything but `SIGHASH_ALL`/`ALL\|UNIFIED` (so `ANYONECANPAY` never reaches a witness) |
| Scope | any script the node understands | native P2WSH Vault descriptors — every Coincube Vault |

(a) fails the lane's correction 4 (Connect Esplora stays the default backend
without a local node), so (b) is the only option that keeps BTCB2 usable at
launch. The cost is that the finaliser must be at least as careful as the
node about *which* signatures go into the witness — the next section.

## What the finaliser guarantees

`finalize_p2wsh_all_unified(psbt, secp) -> FinalizedSpend { transaction, inputs: Vec<InputWitnessReport> }`

1. Every unified record is cryptographically verified first; one bad record
   fails the whole call. Prevouts and witness scripts are authenticated.
2. Per input, unified signatures are offered to the miniscript satisfier
   alone. Only if the script cannot be satisfied from those are verified
   legacy `SIGHASH_ALL` signatures added, and only for keys with **no**
   unified record (a key with both encodings is refused a layer down by the
   PSBT adapter).
3. **A usable unified signature is never dropped for legacy ones.** miniscript
   picks the cheapest satisfaction and `multi` takes keys in script order, so
   with more signatures than the threshold needs it can build an all-legacy
   witness while a unified signature sits on a later key (Elrond's finding at
   `78233ad2`). The finaliser detects that outcome and searches every subset
   of the legacy candidates, always offering all unified signatures, for a
   satisfaction that keeps one; the result is a deterministic function of the
   signatures present. A legacy-only witness comes out only when no offered
   subset lets the script use a unified signature (e.g. a unified record on a
   recovery key whose timelock this transaction does not enable). Over the
   search bound (12 legacy candidates on one input — 63 ms worst case) the
   input is refused with `RefusedToDropUnified`, never degraded.
4. The per-input report (`unified_used`, `legacy_used`) is the **only** basis
   for a replay statement: `replay_protected()` is true iff the witness holds
   at least one verified unified signature, which is invalid on Bitcoin.

## Daemon

`coincubed` keys both spend mutations on `config.bitcoin_config.chain`:
`update_spend` merges signatures with the prior copy on Bitcoin and, on BTCB2,
additionally runs the adapter merge (refuses conflicting or ambiguous
encodings, nothing stored on refusal); `broadcast_spend` uses `finalize_mut`
on Bitcoin and the core finaliser on BTCB2, logging each input's report.
Stored PSBTs round-trip the proprietary records unchanged.

## Desktop

- **Signer index** (`state/vault/signers.rs`): `ReplayProtection::{Capable,
  Legacy, UserMarked(bool)}` — hot key and Border Wallet keys are capable,
  Keychain keys are `Legacy` until Lane B3, devices carry the user's mark
  (`KeySetting::replay_protected`, sparse on disk, unmarked = `false`). The
  Settings toggle that sets a mark is not in this slice; `UserMarked(true)`
  is reachable only through the settings file until it lands.
- **Signing** (`state/vault/psbt.rs`): on BTCB2 the hot key and Border Wallet
  sign unified (`Signer::sign_psbt_unified`,
  `sign_psbt_with_border_wallet_unified`); hardware and Keychain sign legacy as
  before. `ANYONECANPAY` on any input is refused before any signer is
  dispatched. Merges go through the adapter so the desktop never holds a PSBT
  the daemon would reject.
- **Status** (`state/vault/replay.rs`): four states derived from the
  finaliser's report over the merged PSBT — *Replay protected*, *Replayable —
  no replay-capable signature on input N* (amber; Broadcast disabled until "I
  understand this can also spend my Bitcoin" is ticked; the tick is dropped
  whenever a signature changes), *Split — cannot replay* (wired; its evidence
  type `SplitEvidence` has no values until Lane B1.5, so the state is
  unreachable by construction and tested as such), *Unknown / not yet checked*
  (no signatures, not enough, or the verifier refused). On BTCB2 "ready to
  broadcast" is the finaliser's verdict, not the `partial_sigs` count.
- **Entangled deposits (I13)** (`services/entangled.rs`): after each sync, one
  `GET /api/v1/esplora/<twin>/tx/{txid}` through Connect per unresolved deposit
  (`bitcoin/mainnet` for `BitcoinBlake2b`, `bitcoin/testnet4` for its
  testnet). `200` with the same txid → *Entangled*; `404` → *Not entangled*;
  anything else → *Unknown*, not cached, retried next sync. Coins carry an
  *Entangled* / *Not yet checked* badge; the replayable pill names entangled
  inputs as also existing on Bitcoin.
- **Creation** (installer descriptor editor): on BTCB2 a complete path with no
  replay-capable key gets a non-blocking notice ("spends from this path can be
  replayed onto Bitcoin unless the coins were split first"). Devices stay
  selectable.
- **I8** (`replay::bitcoin_cube_unswept_notice`): copy only — "These coins
  also exist on Bitcoin Blake2b until swept there." — rendered for a Bitcoin
  Cube only once Lane B1.5 records what has been swept; nothing supplies that
  yet.

## Not in this slice

Keychain unified signing (Lane B3); the Settings → Signers replay-mark toggle;
poison-split evidence and the *Split* state's producer (B1.5); BTCB2 runtime
activation — every path above is chain-gated and, in this build, a BTCB2 Cube
is refused before a Vault opens (B1.3).
