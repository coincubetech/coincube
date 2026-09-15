# BTCB2 managed node: the dormant `KnotsBlake2b` provider

The desktop's managed-node layer (`coincube-gui/src/node/`) knows a third
provider, `NodeFlavor::KnotsBlake2b`, pinned to Bitcoin Knots
`29.4.1.knots20260508` — the BLAKE2b proof-of-work hardfork build. It is
**dormant**: every `ChainId` it serves reports `RuntimeSupport::Dormant`, so no
installer, loader or settings path can download, configure or start it. What
this slice ships is the isolation — directories, ports, provider/chain
refusals, planner scope — that a later activation builds on. Activation is
**not** a flag flip: the remaining gates are listed at the end of this note.

## What is isolated, and where

Bitcoin Blake2b keeps Bitcoin's network identity (magic, address and key
encodings), so a `bitcoin::Network` cannot tell the two chains apart. The
managed-node layer therefore keys everything on a second axis,
`NodeChainFamily`, derived from the chain's *identity* (`ChainId`), never from
its encoding:

| | Bitcoin family (unchanged) | Bitcoin Blake2b family |
|---|---|---|
| Providers | `Core`, `Knots` | `KnotsBlake2b` only |
| Root under the COINCUBE datadir | `bitcoind/` | `bitcoind-blake2b/` |
| Binaries | `bitcoind/bitcoin-<version>/bin/bitcoind` | `bitcoind-blake2b/bitcoin-29.4.1.knots20260508/bin/bitcoind` |
| Node datadir, `bitcoin.conf`, cookies, chainstate | `bitcoind/datadir/…` | `bitcoind-blake2b/datadir/…` |
| Flavour ledger | `bitcoind/managed_node_state.json` | `bitcoind-blake2b/managed_node_state.json` |
| Lock directory | `bitcoind/` | `bitcoind-blake2b/` |

Every legacy (family-less) helper — `internal_bitcoind_directory`,
`internal_bitcoind_datadir`, `internal_bitcoind_exe_path`,
`ManagedNodeState::path`, `CoincubeDirectory::bitcoind_directory` — is the
Bitcoin family's, byte for byte, so nothing an existing install relies on moves.

### Ports

Upstream gives the Blake2b build **the same default ports as Bitcoin**
(`src/kernel/chainparams.cpp`: `nDefaultPort = 8333` mainnet, `48333`
testnet4; `src/chainparamsbase.cpp`: RPC `8332` / `48332`), so "distinct
ports" cannot come from chain parameters. COINCUBE never uses the defaults for
a managed node anyway: each network section gets OS-allocated ports at setup.

Both places that create a network section — the installer's `DefineConfig`
and the settings path's `write_internal_bitcoind_config` — now go through one
bounded policy (`installer::…::allocate_ports_for_new_section` →
`node::bitcoind::reserved_managed_ports` + `allocate_managed_ports`):

- candidates come from the OS (`get_available_port`); the policy itself is
  deterministic and tested with an injected candidate sequence;
- a candidate is refused if it is a bitcoind default, a repeat, or **already
  recorded** by any other managed-node section: this conf's other networks
  (e.g. testnet4 when allocating mainnet) or any section of the other chain
  family's `bitcoin.conf` — bound right now or not, since a stopped node still
  owns its ports;
- an existing section keeps its ports; nothing already assigned is rewritten;
- absent confs reserve nothing, but an other-family conf that **exists and
  cannot be read fails the allocation closed** — its ports are unknown, and
  live socket probing would not notice a stopped node. The settings path
  allocates before it touches the flavour ledger, so a refusal leaves the
  ledger as it was.

What the policy does **not** provide is atomicity across concurrent setups:
two allocations running at the same moment can both pass the reservation
check before either persists its conf. Today a machine runs one managed
Bitcoin node; serialising allocate-and-persist across chain families (a lock
held from reservation through `to_file`) is a prerequisite for activating the
second family — see the gates below. The disjoint-port test fixtures in this
slice establish file separation and reservation reading, not concurrent
allocation.

### Fail-closed guards

`NodeFlavor::check_chain(chain)` refuses a provider/chain pairing across
families, and it runs before any side effect:

- installer step: on `SelectFlavor`, `DefineConfig` (before the conf or ledger
  is written), `Download` (before the request) and `Install` (before unpack);
- Vault settings (`app/state/vault/settings/bitcoind.rs`,
  `provider_serves_network`): the managed-flavour pick and retry
  (`start_internal_node_setup`, before the download or binary lookup), the
  completed-download handler (before the manifest fetch and install), the
  node-card flavour switch (never armed, and refused on confirm),
  `RestartNodeToApply` and `NodeResourceApply` (before the node is stopped,
  Tor touched or the conf rewritten) — and, independently, the helpers behind
  them: `ensure_tor_and_start_managed` (before Tor is provisioned),
  `configure_and_start_internal_bitcoind` (before the archive is unpacked)
  and `write_internal_bitcoind_config` (before the conf, ledger or identity
  marker is written). The pickers list Core/Knots only; these guards pin the
  boundary for a provider that reaches the pending setup or the ledger by any
  other route;
- `Bitcoind::maybe_start` (every start path: loader, installer, settings):
  before the identity marker is written, the conf migrated or a binary
  resolved;
- `Bitcoind::maybe_start_for_chain` (the loader's entry, keyed on `ChainId`):
  refuses a `Dormant` chain first, then any non-Bitcoin family.

`select_managed_bitcoind_exe` searches only the provider's own family root, so
a Bitcoin chain never resolves a Blake2b binary and the Blake2b provider has no
Bitcoin fallback. `InternalBitcoindConfig` never emits `consensusrules` for any
flavour.

The RDTS repair planner (`node/revalidate.rs`) answers
`Skip(NotABitcoinChain)` for a Blake2b chain, or a Blake2b provider on any
chain, **before** consulting heights, flavours or prune state; and
`reconcile_after_start` returns before loading the ledger, so a Blake2b start
can neither read nor rewrite the Bitcoin node's repair records.

### Release verification

The Blake2b release is published under the Knots URL scheme
(`https://bitcoinknots.org/files/29.x/29.4.1.knots20260508/`) and signed by the
already-pinned Knots key (`KNOTS_SIGNING_KEY_FINGERPRINT`,
`1A3E761F19D2CC7785C5502EA291A2C45D0C504A`). No new trust root is introduced:
`DownloadVerification::for_flavor` builds the same `ReleaseManifest`
verification for both Knots providers, and the version alone selects the
manifest. The real `SHA256SUMS` + `SHA256SUMS.asc` for this release are vendored
under `coincube-gui/src/installer/step/node/test_fixtures/knots_blake2b_*`; the
test proves the signature verifies under the pinned key and that every platform
archive name the provider derives (`arm64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-linux-gnu`, `aarch64-linux-gnu`, `win64-pgpverifiable.zip`) is listed.

## Daemon dependency: chain health on a Blake2b node

Activating the provider needs `coincubed` to read the hardfork's status. The
fields below are taken from Knots tag `v29.4.1.knots20260508`
(`src/rpc/blockchain.cpp`, `src/kernel/chainparams.cpp`,
`src/chainparamsbase.cpp`); they are **not** what the desktop's existing
`reduced_data` probe assumes.

### `getdeploymentinfo`

```json
{
  "hash": "...",
  "height": N,
  "deployments": {
    "...": { },
    "reduced_data": {
      "type": "flagday",
      "height": <Blake2bHeight>,
      "expiry_time": <RdtsExpiryTime>,
      "active": true | false
    }
  },
  "blake2b": {
    "height": <Blake2bHeight>,
    "active": true | false
  }
}
```

- **`blake2b` is a top-level object, a sibling of `deployments`, present only
  when a hardfork height is configured** (`blockchain.cpp`, the
  `getdeploymentinfo` result: `"blake2b", /*optional=*/true, "hardfork
  schedule, present only when one is configured"`; emitted when
  `consensus.Blake2bHeight != std::numeric_limits<int>::max()`). `active` is
  `DeploymentActiveAfter(blockindex, DEPLOYMENT_BLAKE2B)`: whether the hardfork
  rules apply to the block *after* the queried one. This — not a `deployments`
  entry — is the fork-activation signal.
- **`deployments.reduced_data` is a flag-day deployment with an expiry**
  (`RdtsFlagDayDescPushBack`): `type` is `"flagday"`, `height` is the RDTS
  activation height (the BLAKE2b hardfork height), `expiry_time` is
  `RdtsExpiryTime`, and `active` is `RdtsActiveAt(height + 1, parent MTP)` —
  it turns **`false` again once the parent's median-time-past reaches
  `expiry_time`**. It is omitted entirely when unscheduled. There is no `bip9`
  sub-object: the desktop's `DeploymentStatus.status` (read from `bip9.status`)
  is always empty on this build, so `has_failed()` (`!active && status ==
  "failed"`) is always `false` — including after expiry.
  RDTS previously activated via versionbits; that deployment was removed after
  the stall at 961633 (`chainparams.cpp`, mainnet comment).

### Scheduled heights and expiries

| Network | `Blake2bHeight` | `RdtsExpiryTime` |
|---|---|---|
| mainnet | `961640` | `1819756800` (2027-09-01 00:00 UTC) |
| testnet4 | `150308` | `1791903600` (2026-10-13 15:00 UTC) |
| regtest | unscheduled by default | unscheduled by default |

Source: `src/kernel/chainparams.cpp` at the tag (mainnet `CMainParams`,
testnet4 `CTestNet4Params`, regtest `CRegTestParams`).

### Regtest

Regtest schedules nothing unless asked (`src/chainparamsbase.cpp` argument
help; `src/chainparams.cpp` `ReadRegTestArgs`; `src/kernel/chainparams.cpp`
`CRegTestParams`, lines 633–665 at the tag). The two settings are independent
in what they emit:

| Flags | `Blake2bHeight` | `RdtsExpiryTime` | `getdeploymentinfo` |
|---|---|---|---|
| none | unset (`INT_MAX`) | unset (`INT64_MIN`) | neither `blake2b` nor `deployments.reduced_data` |
| `-testactivationheight=blake2b@N` only | `N` | unset | top-level `blake2b: {height: N, active}` **is emitted** (`blockchain.cpp:2029`, gated on height alone); `reduced_data` **absent** (`RdtsFlagDayDescPushBack`, `blockchain.cpp:1951–1952`, needs height *and* expiry) |
| `-testactivationheight=blake2b@N -rdtsexpiry=T` | `N` | `T` | both: `blake2b` and `reduced_data {type: flagday, height: N, expiry_time: T, active}` |
| `-rdtsexpiry=T` without the height | — | — | **startup error**: `-rdtsexpiry requires -testactivationheight=blake2b@<height>` (`src/chainparams.cpp:84`); `T <= 1296688602` (regtest genesis time) is likewise rejected (`:91`) |

`-blake2b_headline=<headline>` overrides the consensus-critical headline and
also requires the activation height (`src/chainparams.cpp:98`). RDTS activates
at the BLAKE2b height; only its expiry is separately settable
(`kernel/chainparams.cpp:660–665`).

A chain-health probe must therefore read the two objects independently: a
node with `blake2b` but no `reduced_data` is a scheduled hardfork with RDTS
unscheduled — not a failed deployment, and not "unscheduled" either.

## Gates before the provider can be un-dormant

Flipping `RuntimeSupport` alone would not produce a working Blake2b node. The
integration work that remains, in the order it is needed:

1. **coincubed chain health** — a reader keyed on the top-level
   `blake2b.{height,active}` object rather than on `deployments.reduced_data`;
   the existing `deployment_status("reduced_data")` probe is a Bitcoin-chain
   RDTS repair input and does not describe the hardfork. And a reading of
   `reduced_data.active` that understands expiry: `false` after `expiry_time`
   is the designed end of RDTS enforcement, not a failed deployment (today's
   `has_failed()` is always `false` on this build, so nothing would even
   notice).
2. **Concurrent port allocation** — serialise allocate-and-persist across
   chain families (see *Ports*); the bounded policy in this slice reserves
   recorded ports but is not a lock.
3. **A Blake2b start path** — `Bitcoind::maybe_start` is the Bitcoin family's
   (its datadir, ledger, lock and `-chain=` argument are Bitcoin's);
   `maybe_start_for_chain` refuses the Blake2b family outright. A start path
   that spawns from `bitcoind-blake2b/`, writes that family's ledger and lock,
   and never runs the RDTS reconciliation is not written.
4. **Product surfaces** — an installer/settings picker that offers the
   provider only for a Blake2b Vault (`ChainId`-keyed, never the
   `bitcoin::Network`-keyed screens that exist today), with the settings
   screen itself carrying the Vault's `ChainId`.
5. **Live verification** — a node test on a **synthetic, temporary regtest
   datadir** with `-testactivationheight=blake2b@N -rdtsexpiry=T`; nothing in
   this slice starts a node, and none of its tests do.

Until those exist the provider stays `Dormant`, and every entry point above
refuses it before touching disk.
