# Bitcoin Blake2b — daemon chain-health reader (`getdeploymentinfo`)

Scope: coincube-api#288, the first of the gates listed in
`docs/BTCB2_MANAGED_NODE.md` ("coincubed chain health"). Read that document
first; this one covers only the typed probe `coincubed` now offers and what it
does and does not establish. Nothing here activates the fork, starts a node,
or touches the managed-node start path.

## What the node reports

Knots `v29.4.1.knots20260508`, `src/rpc/blockchain.cpp`:

- **Top-level `blake2b`** (L2029-2037): emitted only when a hardfork height is
  configured (`consensus.Blake2bHeight != INT_MAX`), as
  `{height: Blake2bHeight, active: DeploymentActiveAfter(blockindex, DEPLOYMENT_BLAKE2B)}`.
  `active` describes the block *after* the queried one.
- **`deployments.reduced_data`** (`RdtsFlagDayDescPushBack`, L1948-1963):
  emitted only when the hardfork height **and** `RdtsExpiryTime` are both set,
  as `{type: "flagday", height: RdtsActivationHeight(), expiry_time:
  RdtsExpiryTime, active: RdtsActiveAt(height + 1, parent MTP)}`. `active`
  turns `false` again once the parent's median-time-past reaches
  `expiry_time` — the designed end of RDTS enforcement. There is no `bip9`
  object on this build.
- `RPCHelpForDeployment` (L1915-1919) fixes the field types: `type` ∈
  {`buried`, `bip9`, `flagday`}, `height` NUM, `expiry_time` NUM_TIME
  (median time), `active` BOOL.

The two objects are produced by separate code paths and are independent facts.
A Bitcoin Core node reports neither.

## The reader

```rust
let info: Result<Blake2bDeploymentInfo, DeploymentProbeError> = bitcoind.deployment_info();

pub struct Blake2bDeploymentInfo {
    pub fork: Option<ForkActivation>,   // top-level `blake2b`, or None if the node reported none
    pub rdts: RdtsSchedule,             // `deployments.reduced_data`, typed
}
pub struct ForkActivation { pub height: u64, pub active: bool }
pub enum RdtsSchedule {
    Absent,                                                     // no `reduced_data` entry
    FlagDay { height: u64, expiry_time: i64, active: bool },    // reported verbatim
    Unsupported { kind: String },                               // a non-"flagday" `type`; fields not interpreted
}
pub enum DeploymentProbeError {
    Rpc(BitcoindError),        // the request failed; is_warming_up/is_transient/is_unauthorized still apply
    Malformed(&'static str),   // the node answered with something that is not the tagged schema
}
```

`deployment_info` is `getdeploymentinfo` through the ordinary retrying node
client plus the pure parser `parse_deployment_info`, which the unit tests drive
with JSON fixtures (no node is started by any test).

### Three outcomes that never collapse

| the node… | result |
|---|---|
| could not be reached / answered an RPC error | `Err(Rpc(e))` — `e` is the original `BitcoindError` |
| answered, but a field the reader uses is missing or has the wrong type | `Err(Malformed(field))` |
| answered with no `blake2b` object | `Ok` with `fork: None` |
| answered with no `reduced_data` entry | `Ok` with `rdts: Absent` |
| answered with a `reduced_data` whose `type` is not `flagday` | `Ok` with `rdts: Unsupported { kind }` |

An RPC error is not "inactive"; malformed data is not "unscheduled"; an absent
fork object is "this node has no hardfork height configured" (a stock Core
build, or Knots without `-testactivationheight=blake2b@N`) — it is **not**
evidence about the chain, healthy or otherwise. Booleans are never defaulted:
a missing `active` is `Malformed`, not `false`.

### What is deliberately not done

- **No inference.** `active` values are the node's own; the reader does not
  compute anything from the local clock, does not treat `active: false` after
  `expiry_time` as a failure (it is the normal post-expiry report), and does
  not infer an active fork from an active RDTS entry or vice versa.
- **No reconciliation.** `blake2b.height` and `reduced_data.height` are
  documented to be the same value but are read independently and both
  preserved if they differ. Whether they must agree — and whether either
  matches the chain a Cube expects — is caller policy, not established here.
- **No authentication.** A matching schedule says what a node *reports*, not
  which chain it validates. `coincubed`'s `node_sanity_checks` compares the
  BIP70 chain name, which is `main` on both Bitcoin and BTCB2, so this reader
  establishes no chain authentication and no spend safety. Those remain
  separate gates (`docs/BTCB2_MANAGED_NODE.md`, "Gates before the provider can
  be un-dormant").
- **No BIP9 interpretation.** On the tagged build `reduced_data` carries no
  `bip9` object, so a BIP9 state could say nothing about flag-day expiry; the
  reader does not look for one.
- **Unknown fields are tolerated** everywhere (root, `blake2b`, `reduced_data`,
  other deployments), so a future node can extend the result without breaking
  the probe; only the fields the reader uses are validated strictly.

### The Bitcoin-family RDTS probe is gone

`BitcoinD::deployment_status(name)` and `DeploymentStatus::has_failed()`, and
their sole caller (the managed-node revalidation's `rdts_abandoned` input),
were deleted in RDTS sunset PR 4 (#510). This reader is the only
`getdeploymentinfo` consumer left in the workspace; it is a separate, opt-in
API whose consumer is Connect (coincube-api#288), with no in-repo Rust caller.

## Fixtures covered by the unit tests

stock Bitcoin Core (no fork, no RDTS) · fork configured without RDTS · both
objects before activation · active fork with active RDTS · RDTS `active:
false` after expiry · non-`flagday` `reduced_data` (`bip9`, `buried`) ·
unequal fork/RDTS heights · RDTS entry with no fork object · unknown fields at
every level · malformed matrix (non-object root; missing/non-object
`deployments`; non-object `reduced_data`; missing/non-string `type`;
missing/string/negative/float heights; missing/string/float `expiry_time`;
missing/string/number `active` — for both objects) · RPC failure propagation
(warming-up code, transport timeout, HTTP 401) with classifiers intact ·
malformed messages never echo node data.

## Toolchain note

The repository pins Rust 1.97.1. Local validation of this change ran on
Homebrew Rust 1.94.0 (no `rustup` on the build host); the pinned CI toolchain
is authoritative.
