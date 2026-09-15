# BTCB2 chain binding: daemon configuration and SQLite identity

The daemon (`coincubed`) used to know only a `bitcoin::Network`, which cannot
tell Bitcoin Blake2b (BTCB2) from Bitcoin: the fork kept Bitcoin's network
identity, so a BTCB2 Cube *encodes* exactly like a mainnet one. This slice
carries the stable chain identity (`coincube_core::chain::ChainId`) through
the daemon's configuration and its SQLite database, and makes the daemon
refuse a wrong or unknown chain **before** it creates the data directory,
creates or loads the bitcoind watch-only wallet, migrates the database or
starts any healing. BTCB2 itself stays **dormant**: the daemon refuses to run
on it, before any I/O, until the backend gates listed at the end exist.

## Configuration: one key, two meanings

`[bitcoin_config] network` is still the only key and, for the Bitcoin
family, still writes and reads the same five strings every existing
`daemon.toml` contains — `bitcoin`, `testnet`, `testnet4`, `signet`,
`regtest`. Its value is now the chain *identity* (`ChainId`'s directory
string); Bitcoin Blake2b writes `bitcoin-blake2b` / `bitcoin-blake2b-testnet4`.

In memory, `BitcoinConfig` carries both `chain: ChainId` (identity) and
`network: bitcoin::Network` (encoding, derived — never on the wire). All the
encoding-only readers of `bitcoin_config.network` are unchanged. A hand-built
pair that disagrees is refused by `Config::check` and, independently, by
`DaemonHandle::start` before it touches the filesystem (`StartupError::Config`).
The data directory is keyed on the identity (`chain.dir_name()`), which is
byte-identical for the Bitcoin family and `bitcoin-blake2b/` for the fork.

Why the identity lives in the existing key rather than an added optional one:
`BitcoinConfig` never denied unknown fields, so an older binary would have
*ignored* a new `chain = "bitcoin-blake2b"` key and run the file as mainnet.
With the fork string in `network`, every older binary refuses the file at
parse time (its `bitcoin::Network` does not know the string) — the same
protection `settings.json` already relies on.

## Database: version 9

`tip` now has two identity columns, both `NOT NULL`:

| column    | meaning                                  | Bitcoin family | BTCB2 mainnet     |
|-----------|------------------------------------------|----------------|-------------------|
| `network` | encoding, as `bitcoin::Network` spells it | `bitcoin`      | `bitcoin`         |
| `chain`   | identity, as `ChainId` spells it          | `bitcoin`      | `bitcoin-blake2b` |

The v8 → v9 migration (`migrate_v8_to_v9`) is one database transaction: it
reads the single legacy `tip.network` row, parses it **as a `bitcoin::Network`**
(the only thing a legacy row ever held — a fork string there is refused, never
"backfilled"), rebuilds `tip` (a rebuild rather than `ALTER TABLE ADD COLUMN`,
so the constraint is real and the column order equals a fresh database's),
sets `chain` to the identity of that network, and bumps the version. Zero or
several identity rows abort it. Nothing else is touched; a failure leaves the
database at v8 with the old `tip` intact. Running it again is a no-op.

`sanity_check` now compares the stored identity with the configured one
(`SqliteDbError::ChainMismatch`) as well as the encoding column.

## The read-only preflight

`database::sqlite::preflight::read_stored_identity` answers "what version and
chain does this file hold?" without modifying it, and `DaemonHandle::start`
runs it on any existing database before `data_dir.init()`, `setup_bitcoind`
and `setup_sqlite`. Ordering in `start`:

1. `check_chain_encoding()` on the in-memory config — no I/O.
2. `chain_runtime_gate` — BTCB2 → `StartupError::ChainDormant`, no I/O.
3. If a database exists: the preflight, then `StartupError::ChainMismatch` if
   its identity is not the configured one.
4. Only then: directory creation, watch-only wallet, migrations, healing,
   backends — in the order they always ran.

What the preflight does, and what it promises:

- It inspects the 100-byte SQLite header with a plain read-only file open
  first: the magic (`NotSqliteDatabase` for empty, truncated or foreign
  files) and the file-format bytes 18/19. A WAL-format database (`2`) or a
  stray `-wal` / `-shm` sidecar is refused **before SQLite is opened**, because
  opening a WAL database — even read-only — may create its `-shm` file.
  coincubed has never set a journal mode, so every database it created is in
  rollback-journal mode; the header gate covers files it did not create too.
- It then opens SQLite `READ_ONLY` without `CREATE` (a missing file is an
  error, never an empty new database), sets `PRAGMA query_only`, and reads the
  version and identity rows inside **one read transaction**, so both come from
  a single snapshot: another connection may prepare a write meanwhile but
  cannot commit one until the preflight is done. The reads use explicit
  columns and are version-aware — a v8 row is never decoded with the v9
  `SELECT *` decoder. Exactly one `version` row and exactly one `tip` row are
  required; unknown chain strings, an identity whose encoding disagrees with
  its `network` column, negative or future versions are typed errors, never a
  default, never a panic.
- For a quiescent rollback-journal database this writes no page and creates
  no file: the bytes and the directory listing are what they were. That is a
  statement about this process's behaviour; it says nothing about another
  process replacing or writing the file concurrently.

## Compatibility consequences — read before upgrading

- **Older binaries refuse a v9 database** with `UnsupportedVersion(9)`, the
  same way every previous schema bump behaved. Downgrading the application
  after the migration therefore needs the **pre-upgrade copy of
  `coincubed.sqlite3`**: take one before the first start of a build carrying
  this change. The migration drops no rows (it rebuilds the one-row `tip`
  table), but there is no automatic downgrade.
- **A database with a pending rollback journal is refused, not repaired.**
  Previously the first read-write open silently rolled a crashed writer's
  `-journal` back. Now the preflight reports `RecoveryRequired` and leaves the
  file and its journal untouched, because rolling back would mutate a database
  whose chain is not yet established. Restarting alone does **not** clear it: a
  restart runs into the same journal. Recovery is a separate, deliberate step
  (any ordinary read-write open of that file, once its directory/config
  pairing has been confirmed); a reviewed recovery workflow is a follow-up.
- **A database manually converted to WAL mode is refused** (`WalDatabase`),
  as is any database with `-wal`/`-shm` sidecars next to it. coincubed never
  produced such files; converting back (`PRAGMA journal_mode = DELETE` with a
  tool of your choice) restores the supported layout.
- The GUI loader additionally refuses to start a node or daemon when the
  Cube's `daemon.toml` names a different chain than the Cube record it sits
  under (`Error::ChainMismatch`), which covers a Cube whose database does not
  exist yet — the daemon's preflight has nothing to compare against in that
  case.

## Not in this slice

No checkpoint or provider trust policy, no node schedule (`deployment_info`)
consistency check, no chain-keyed Esplora route, no post-reorg revalidation,
no signing/finalization, no RPC exposure of the identity (`getinfo` still
reports `network`), no activation flag. The daemon's BTCB2 refusal
(`chain_runtime_gate`) is the single policy line those later gates replace.
