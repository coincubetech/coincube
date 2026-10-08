# Wallet history on a managed pruned node

Tenshu can preserve Connect-discovered Vault history when adopting its managed
regular-Bitcoin Core or Bitcoin Knots node. Pruning removes block bodies, not
transactions already saved in the node's wallet database. A newly imported or
previously offline wallet may still need those bodies to discover older activity.

The Node settings panel exposes three related operations:

- **Import Connect history** retrieves known funding and spending transactions
  from the current wallet engine, requests Merkle-block proofs from the configured
  Esplora providers, validates their transaction membership and active-chain block
  hash, and imports supported transactions with `importprunedfunds`. Existing
  Coincube database history remains intact. Missing proofs and transactions Core
  cannot import, including transactions with no wallet-owned output, fall back to
  block scanning. Tokens are sent only to their configured provider; redirects
  are disabled.
- **Recover wallet history** accepts an exact block height or a UTC date in
  `YYYY-MM-DD` form. It fetches missing blocks from connected outbound archival
  peers, verifies the block bodies, scans retained batches, and saves progress.
  Date resolution uses median block time with a conservative two-hour allowance
  and 144-block margin; use a height when an exact boundary is required. The scan
  ends at the chain tip captured when the operation starts, followed by spend
  reconciliation. Normal wallet tracking handles subsequent blocks.
- **Retain recent blocks** accepts 1–3650 days, including 180 and 365. Applying a
  window also recovers missing blocks in the requested period. Tenshu switches
  the managed node to manual pruning and maintains a rolling timestamp cutoff.
  Choosing a storage-size target in the existing node resource settings disables
  rolling retention after outstanding recoveries have been cancelled/completed.

## Spend reconciliation and switching

A funding inclusion proof establishes confirmation, not current unspentness.
Before declaring recovery complete or permitting the local-node handoff, Tenshu
checks funding and spending records against the local active chain and compares
outputs against `gettxout` with mempool spends included. Pending records require
local mempool membership. A mismatch keeps Connect active and leaves a resumable
recovery obligation; it never treats an imported funding transaction alone as
evidence of a spendable balance.

Core wallet records are replayed into Coincube's database on each local daemon
startup, and explicitly after recovery on an already active local backend. Replay
uses saved transaction records, so it works after the scanned blocks are pruned.
Only a successful coherent database update acknowledges its replay request ticket;
an older poll cannot acknowledge a newer request. An unknown-birthday restored
Vault needs a reconciled scan from genesis and an acknowledged local database
replay to retire its full-rescan obligation. A verified completed genesis scan can
permit the Connect-to-local handoff while that obligation remains recorded.
A recent date does not establish complete older history. Descriptor ranges
preserve Core's existing range and include current Coincube indices plus a margin.
Replay maps outputs through index 1000 or the existing wallet lookahead, whichever
is larger; unmatched owned outputs leave replay incomplete and require restoring
the Vault's address indices. These operations use watch-only descriptor wallets.

## Retention, interruption, and disk use

Progress and retention intent are atomic private files under
`<Coincube data directory>/node-history/<network>/`. A recovery is bound to the
network genesis, managed-node instance, endpoint, wallet path and descriptor.
Changed identities or malformed journals are refused. A per-network filesystem
lease serializes block fetching/scanning, reconciliation and pruning across Cubes.
Pruning is limited by the earliest unfinished scan. Downloaded blocks do not
count as scanned progress. Reorganizations rewind the affected recovery before
completion.

**Pause after batch** waits for the current worker; **Resume** rechecks the saved
identity and continues; **Cancel** releases the recovery's retention obligation.
Wallet records already imported/scanned survive cancellation and later pruning.
Temporary recovery restores the previous storage-size pruning target when no
unfinished recoveries remain. A user-selected rolling window stays enabled.

Core can refuse to load an offline wallet whose catch-up blocks were deleted.
Tenshu then retains the entire requested range before retrying wallet loading.
If Core still requires older blocks, the recovery pauses and asks for an earlier
start. Managed startup suppresses automatic wallet loading while manual retention
or an unloaded-wallet recovery is recorded, keeping wallet RPC available for
explicit loading. It never deletes a wallet or block file and never runs reindex.

Recovery and rolling retention can consume hundreds of gigabytes. A 512 MiB
free-space guard pauses fetching; it is an emergency reserve, not a size guarantee.
Core prunes whole files and keeps its minimum recent-block reserve, so a date
window is a minimum retention intent rather than an exact disk cap. Tenshu must
remain open with a Cube using the node for rolling pruning to advance; while it
is closed the manual-pruning node continues retaining blocks and can grow.
Peer availability, bandwidth, wallet catch-up requirements and disk capacity
determine whether a requested historical range can be recovered.

## Compatibility and verification

These controls apply only to owned, cookie-authenticated, regular-Bitcoin nodes.
External nodes and Bitcoin Blake2b are excluded. The same RPC contract was checked
with Bitcoin Core **29.0** and regular-Bitcoin Knots **29.3.knots20260507**.

Run the isolated two-node contract test with a downloaded release binary:

```sh
python3 tests/pruned_history_regtest.py /absolute/path/to/bitcoind
```

This provider RPC contract test demonstrates that a funding proof alone can produce a stale wallet UTXO, that
spending-only import is rejected, that a fetched pruned block can be scanned
despite an unchanged `pruneheight`, and that wallet records survive a subsequent
pruning request and restart. It does not require that the second pruning request
deletes the fetched block's mixed-age file. It creates private regtest data
directories and stops only its own nodes. GUI unit tests cover proof membership,
provider fallback, identity bounds, filesystem exclusion, failed scan checkpoints,
reorganization, bootstrap progress, stale mempool spend rejection, pruning holds,
and obsolete GUI callbacks. A daemon HTTP integration regression verifies that
Core-only historical funding enters Coincube's coin and transaction APIs even
when its database tip has already reached the current chain tip.

Relevant upstream RPC documentation: [importprunedfunds](https://bitcoincore.org/en/doc/29.0.0/rpc/wallet/importprunedfunds/),
[getblockfrompeer](https://bitcoincore.org/en/doc/29.0.0/rpc/blockchain/getblockfrompeer/),
[rescanblockchain](https://bitcoincore.org/en/doc/29.0.0/rpc/wallet/rescanblockchain/),
and [pruneblockchain](https://bitcoincore.org/en/doc/29.0.0/rpc/blockchain/pruneblockchain/).
