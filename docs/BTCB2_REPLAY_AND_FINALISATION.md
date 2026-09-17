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
   fails the whole call. Prevouts and witness scripts are authenticated. Then
   **every** legacy `partial_sigs` entry of every input is verified against
   the BIP-143 digest and restricted to `SIGHASH_ALL` — including entries no
   witness will use. Each input's own sighash *request*
   (`PSBT_IN_SIGHASH_TYPE`) is held to one rule, defined once at the PSBT
   adapter and therefore applied at every boundary that parses, merges or
   stores a Blake2b PSBT — the desktop merge, the daemon's first insert and
   merge, the dispatch guard, the finaliser: absent, `SIGHASH_ALL` or
   `ALL|UNIFIED`, anything else refused. On an input that carries a unified
   record the verifier is stricter — absent or `ALL|UNIFIED` only — so a
   `SIGHASH_ALL` request next to a unified signature is refused, not
   reconciled. That cannot strand a Coincube spend: nothing in spend creation
   or signature merging writes the request field and the only writer, the
   unified signer, writes `0x21`; the case is reachable only through an
   imported PSBT. A PSBT that lies anywhere is not finalised from the parts
   that happen to be true. (This is stricter than a node's `finalizepsbt`, and
   than the Bitcoin path's `finalize_mut`, which only checks what it places —
   but it matches the module's refuse-rather-than-broadcast posture, and the
   adapter already refuses to *store* an ambiguous or conflicting record, so
   an unusable legacy record should not be reachable through Coincube's own
   flow. On the Bitcoin path `finalize_mut` checks only the signatures it
   selects for the witness, so an invalid legacy signature strands a spend
   there when it is one the satisfaction needs; an unused invalid entry can be
   ignored there, whereas here it is refused.)
2. Per input, unified signatures are offered to the miniscript satisfier
   alone. Only if the script cannot be satisfied from those are the verified
   legacy signatures added, and only for keys with **no** unified record (a
   key with both encodings is refused a layer down by the PSBT adapter).
   Timelock leaves are answered with the transaction-level consensus guards
   rust-miniscript's own `PsbtInputSatisfier` applies — CSV needs transaction
   version ≥ 2 and a sequence that is a relative lock time, CLTV needs a
   sequence that enables lock time — so a recovery witness is never assembled
   for a transaction a node would reject.
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
`update_spend` first holds the incoming PSBT to the adapter's rules **and
cryptographically verifies every signature it carries**
(`unified_finalize::verify_all_signatures`: unified records through the
verifier, legacy ones against the BIP-143 digest, `SIGHASH_ALL` only) on
BTCB2 — before the existence check, so a **first insert** stores only what
every later update and the finaliser would accept, and an invalid update
leaves the stored row byte-identical (`CommandError::UnifiedSpendValidation`,
nothing stored on refusal). The adapter alone validates representation, not
validity: a signature made for another transaction passes it, and once stored
would be refused by every later merge as a conflict against its correct
replacement — a spend that cannot be completed through the API that wrote
it. (A legacy `ANYONECANPAY` *flag* is a representation rule, refused by the
adapter itself; Taproot signature data on a P2WSH input is refused by the
shared verifier, key-path and script-path, whatever its sighash.) Unsigned and
partially signed spends pass — `update_spend` backfills `non_witness_utxo`
from the wallet before verifying (the public `updatespend` contract never
required it), and stores the backfilled, verified PSBT: **the value verified
is the value stored**. Storing the incoming bytes would also be sound — the
backfilled data is additive and bound to what the PSBT already commits to, so
the digest is the same — but the desktop's merge verifies the *merged* PSBT
against the row it read back, so the stored row carries the prevouts that
verification needs. The compatibility criterion, Blake2b-only (the other
chains return before any of this): a Blake2b PSBT whose signatures all verify
and whose previous transactions the wallet holds is accepted whether or not
the incoming bytes carried `non_witness_utxo`; when the wallet cannot supply
one, the refusal is the named `SpendMissingPreviousTransaction`, not a
verification error. All three shapes are pinned, as is the daemon-created
spend, which carries the authenticated prevout, P2WSH prevout and committing
witness script on every input. Then it merges signatures with the prior copy on
Bitcoin (last write wins on a key, as before) and, on BTCB2, runs the adapter
merge **against the stored PSBT as it is** — never after the copy, which would have overwritten a
stored signature before the adapter could compare it — refusing conflicting
or ambiguous encodings with nothing stored on refusal, and **verifying the
merged candidate as a whole** before it is stored: the adapter merge keeps the
stored request field, so a stored `SIGHASH_ALL` request plus an incoming
unified record is a PSBT the verifier refuses (`IncompatibleSighash`), refused
atomically rather than stored or reconciled; `broadcast_spend` uses
`finalize_mut` on Bitcoin and the core finaliser on BTCB2, logging each
input's report. Stored PSBTs round-trip the proprietary records unchanged.

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
  before. Before any signer — local, device or Keychain — is dispatched, the
  PSBT is refused if any input asks for or carries `ANYONECANPAY`, carries a
  reserved unified record the adapter rejects, or carries Taproot signature
  data at all (a Blake2b Vault is P2WSH; Taproot signatures bring their own
  sighash byte) — so no device or phone is prompted for a signature that would
  be thrown away. `Wallet::chain` is set by both wallet constructors, the
  local loader and the remote-backend path, from `CubeSettings::network`. Merges go through
  the adapter against the destination as it is, and the **merged result** is
  cryptographically verified (`verify_all_signatures`) before it replaces the
  in-memory PSBT — the merged PSBT, as the daemon verifies the whole PSBT it
  is handed, because a signer's result need not carry the prevouts
  verification needs — so the desktop never holds a PSBT the daemon would
  reject and a conflicting or invalid signature never overwrites a stored one.
  The merge destination is therefore always the local, authenticated PSBT:
  the Keychain API rail (`keychain_sign.rs`, `on_session_fetched`) seeds its
  accumulator from a clone of `tx.psbt` and merges every returned blob into
  that clone — the first included, so the adapter's transaction check binds
  every return to the request, not only the last — and assigns `tx.psbt` only
  once all of them merged; a refusal on any blob leaves `tx.psbt` untouched
  and schedules nothing for persistence. (The LAN rail never reaches this
  merge: `phone_signer` authenticates prevouts from the original request.)
  On the Bitcoin family the accumulator change is outcome-preserving — the
  historical copy applied in submission order, last write wins on a key —
  and pinned byte-for-byte against the former composition. A signer result's
  Taproot fields never enter: the adapter merge carries signatures only, and
  the dispatch guard refuses such a PSBT before any signer sees it. The Keychain flow's "who still has to sign" classification uses
  the same chain-keyed analysis, so a collected unified signature is not asked
  for again.
- **Status** (`state/vault/replay.rs`): four states derived from the
  finaliser's report over the merged PSBT — *Replay protected*, *Replayable —
  no replay-capable signature on input N* (amber; Broadcast disabled until "I
  understand this can also spend my Bitcoin" is ticked; the tick is keyed to
  the PSBT's bytes, so it survives a recompute over the same signatures and is
  dropped on any content change even when the status enum stays equal),
  *Split — cannot replay* (wired; its evidence
  type `SplitEvidence` has no values until Lane B1.5, so the state is
  unreachable by construction and tested as such), *Unknown / not yet checked*
  (no signatures, not enough, or the verifier refused). On BTCB2 "ready to
  broadcast" is the finaliser's verdict, not the `partial_sigs` count.
- **Entangled deposits (I13)** (`services/entangled.rs`): after each sync, one
  `GET /api/v1/esplora/<twin>/tx/{txid}` through Connect per unresolved deposit
  (`bitcoin/mainnet` for `BitcoinBlake2b`, `bitcoin/testnet4` for its
  testnet). `200` with the same txid → *Entangled*; `404` → *Not entangled*;
  anything else → *Unknown*, not cached, retried next sync. Coins carry an
  *Entangled* / *Not yet checked* badge. **A replayable input whose deposit is
  confirmed *Entangled* is a requirement, not a warning**
  (`replay::blocked_entangled_inputs`): the acknowledgement does not apply,
  Broadcast stays disabled, the picker stays open, and the copy names the
  remedy that exists in this build — a replay-capable signature on that input
  (Cube key or Border Wallet key); splitting first is B1.5 and is said to be
  unavailable rather than offered. *Unknown* never blocks: gating on an
  unanswered lookup would stop every BTCB2 spend until a sync completed, and
  "never reads as not entangled" asks for the honest amber, not a hard stop.
  The entangled set is read from the cache at every check (handler, picker
  close, view), never frozen into the review at signature time, so a lookup
  landing after the last signature tightens the gate — and Sign stays
  offered and the picker stays open while the requirement is unmet, so the
  hot key or Border Wallet signature that satisfies it can still be collected.
  Consequence, accepted deliberately: a Vault whose only usable path has no
  replay-capable signer cannot spend a known-entangled coin until Lane B3
  (Keychain unified) or B1.5 (split) — which is why the creation notice exists.
  **Cache lifecycle:** *Entangled* is terminal (never re-queried, never
  overwritten); *Not entangled* carries the instant it was resolved and is
  re-queried by the sync task once older than one hour (anyone holding the
  funding transaction can broadcast it onto Bitcoin after our 404); *Unknown*
  is never cached. Lookup batches are single-flight (claimed txids are
  excluded from the next batch and released as a whole when the reply lands,
  `Unknown` included). At the moment it matters — on entering the spend screen
  and whenever the status becomes replayable, once per set of signatures — the
  screen re-checks that spend's inputs not already known *Entangled*; Broadcast
  is disabled and says it is checking meanwhile (the gate stays a pure function
  of cache state plus the in-flight flag); *Entangled* back closes the gate,
  *Unknown* back leaves the acknowledgement path open with copy saying the
  check could not complete, a *Protected* spend checks nothing. Each check
  carries a process-wide generation token; a reply is accepted only for the
  generation currently in flight, so a reply from an earlier instance of the
  screen or from before a signature was added never clears the current claim
  (its positive still lands in the cache — *Entangled* is terminal), and a
  reply is applied before any new check is kicked; a stale reply's *positive*
  is still cached (terminal), but a *negative* is cached only once the screen
  has accepted the reply for its current generation, so a stale `NotEntangled`
  can never re-stamp an earlier answer's resolve instant. **The gate holds at final
  dispatch too:** Confirm in the Broadcast dialog re-runs `broadcast_ready`
  against the current cache, the dialog is never opened unready, and a dialog
  open on a spend that stops being ready (a lookup landed, a re-check started,
  a signature changed) is closed back to the spend screen with the reason —
  no path reaches `broadcast_spend_tx` ungated.
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
