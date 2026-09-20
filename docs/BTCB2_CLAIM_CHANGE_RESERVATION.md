# Durable change reservation for owned Claim preparation

`DaemonControl::reserve_change()` returns a non-deserializable
`ChangeReservation` with exact `ChainId`, full public descriptor and normal BIP32
change index. It is a committed allocation, not spend permission, a UTXO lock,
a signed transaction, or a completed Claim flow. No new JSON-RPC endpoint or
GUI action is enabled here. Embedded callers can use the daemon method; an
external-daemon consumer needs a separately reviewed wire adapter.

The SQLite implementation requests `synchronous=FULL`, uses `BEGIN IMMEDIATE`, checks the persisted chain,
encoding and descriptor, advances the existing change high-water mark, extends
the address lookahead, and commits before returning. Separate SQLite connections
and processes serialize on that transaction. No schema version or migration is
needed. Errors are fallible: storage/commit failure, identity mismatch, exhausted
normal derivation/lookahead, or unsupported database implementation. A commit
error may have burned the index; never guess that an index can be reused.

Ordinary fresh change allocation in `create_spend` and RBF without a previous
change output uses this same primitive. Consequently failed or cancelled fresh
attempts now burn an index too, even if no change output ultimately exists.
Explicitly supplied change addresses and an existing RBF change output retain
intentional reuse. Reservations prevent reuse by subsequent *fresh allocation*;
they do not prevent a caller explicitly asking to pay an already known address.

The existing monotone `set_derivation_index` transaction refuses lower indices,
so a late poller or post-builder update cannot move behind a concurrent
reservation. Its address lookahead is extended in the same transaction. A new
reservation refuses before mutation if the next index or either branch's full
lookahead would enter hardened derivation. There is no release operation: after
cancellation, process exit or restart, allocations remain burned. The caller's
Claim journal must retain the returned binding with its exact construction; a
restart may allocate a new index but must never silently replace an old plan.

This is one Claim prerequisite. It does not collect fresh funds/deployment
observations, sign, preflight, broadcast, import keys or enable UI. Apply the
canonical #276 dynamic RDTS/MTP, confirmation and reorg corrections in the
future controller; do not infer chain-exclusive funds from timestamps or absence.

Validation uses synthetic temporary SQLite databases: concurrent independent
connections, reopen, lower late updates, address lookahead, wrong chain/descriptor,
precommit insert failure with rollback, and overflow. The command test binds the
ordinary allocator to the same primitive and checks failed attempts burn an index.
Run the whole `coincubed` package and strict clippy with repository-pinned tooling.
No existing wallet, live node, GUI runtime or deployment acceptance is implied.
