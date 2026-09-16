//! Read-only identity preflight for an existing database.
//!
//! [`DaemonHandle::start`](crate::DaemonHandle::start) used to learn which chain a database was
//! created for only from the final `sanity_check`, i.e. *after* it had created the data
//! directory, created or loaded the bitcoind watch-only wallet and applied every pending schema
//! migration. For a Bitcoin-family Cube that order was merely wasteful; with a fork identity that
//! encodes exactly like mainnet it is dangerous, because the mutations would already have
//! happened against the wrong chain. This module answers "what chain and version does this file
//! hold?" without changing anything, so the daemon can refuse first.
//!
//! What "without changing anything" means here, precisely:
//!
//! - The file's 100-byte SQLite header is inspected with a plain read-only `File::open` before
//!   SQLite is involved at all. A WAL-format database (header bytes 18/19 == 2) or a stray `-wal`
//!   / `-shm` sidecar is refused at that point, because opening a WAL database — even read-only
//!   — may create its `-shm` file. coincubed has never set a journal mode, so every database it
//!   created is rollback-journal; this gate covers files it did not create too.
//! - SQLite is then opened `READ_ONLY` without `CREATE` (a missing file is an error, never an
//!   empty new database) and `PRAGMA query_only` is set. For a quiescent rollback-journal
//!   database this writes no page and creates no journal, so the file bytes and its sidecars are
//!   exactly what they were. A crashed writer's hot `-journal` makes SQLite refuse the read-only
//!   connection (`SQLITE_READONLY_ROLLBACK`): that is surfaced as
//!   [`PreflightError::RecoveryRequired`] and *not* recovered here, because rolling the journal
//!   back would mutate a database whose chain is not yet known. Restarting alone does not clear
//!   it; recovery is a separate, deliberate step.
//! - These guarantees are about this process's own behaviour on a quiescent file. They say
//!   nothing about another process replacing or writing the file concurrently.
//!
//! The read is version-aware and uses explicit columns only. It never decodes an old row with
//! the current [`DbTip`](super::schema::DbTip) `SELECT *` decoder: a v8 row has no `chain`
//! column, and a v9 decoder must not be pointed at it.

use crate::database::sqlite::{DB_VERSION, MAX_DB_VERSION_NO_CHAIN};

use std::{
    fmt, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    str::FromStr,
};

use coincube_core::chain::ChainId;
use miniscript::bitcoin::Network;

/// The 100-byte header every SQLite database starts with.
const SQLITE_HEADER_LEN: usize = 100;
/// `"SQLite format 3\0"`.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
/// Header offsets of the file-format write/read version bytes: `1` for the legacy rollback
/// journal, `2` for WAL. See https://www.sqlite.org/fileformat.html.
const FORMAT_WRITE_VERSION_OFFSET: usize = 18;
const FORMAT_READ_VERSION_OFFSET: usize = 19;
const FORMAT_LEGACY: u8 = 1;
const FORMAT_WAL: u8 = 2;

/// What an existing database says about itself, read without modifying it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredIdentity {
    /// The schema version found in the file, `0..=DB_VERSION`.
    pub version: i64,
    /// The chain identity. For a pre-v9 database this is the Bitcoin-family identity of its
    /// `network` column — the only thing such a database can hold.
    pub chain: ChainId,
    /// The encoding column as stored.
    pub network: Network,
}

#[derive(Debug)]
pub enum PreflightError {
    /// No file at this path (nothing was created).
    NotFound(PathBuf),
    Io(PathBuf, io::Error),
    /// Too short to be a database, or not carrying the SQLite magic.
    NotSqliteDatabase(PathBuf),
    /// Header bytes 18/19 say WAL. Refused before any SQLite open so no `-shm` can appear.
    WalDatabase(PathBuf),
    /// Header bytes 18/19 are neither the legacy nor the WAL value.
    UnknownFileFormat {
        path: PathBuf,
        write_version: u8,
        read_version: u8,
    },
    /// A `-wal` or `-shm` file sits next to the database.
    UnexpectedSidecar(PathBuf),
    /// A `-journal`, `-wal` or `-shm` file is present but the database itself is not: the
    /// remains of a database that was removed or never finished being written. Starting
    /// "fresh" here would create a new database next to them.
    OrphanSidecar(PathBuf),
    /// SQLite needs to roll back a hot journal (or recover a WAL) before this database can be
    /// read, which a read-only connection will not do. Not recovered automatically.
    RecoveryRequired(PathBuf),
    /// Opened fine, but the `version` / `tip` tables are not there.
    NotACoincubeDatabase(PathBuf),
    /// A version this build does not know (negative or newer than [`DB_VERSION`]).
    UnsupportedVersion(i64),
    /// Zero or several identity rows, or an identity whose parts disagree.
    MalformedIdentity(String),
    /// A chain string this build does not know. Never mapped to a default.
    UnknownChain(String),
    Sqlite(rusqlite::Error),
}

impl fmt::Display for PreflightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(p) => write!(f, "No database file at '{}'.", p.display()),
            Self::Io(p, e) => write!(f, "Could not read database file '{}': {}.", p.display(), e),
            Self::NotSqliteDatabase(p) => {
                write!(f, "'{}' is not an SQLite database.", p.display())
            }
            Self::WalDatabase(p) => write!(
                f,
                "Database '{}' is in WAL journal mode, which coincubed does not use; refusing to \
                 open it.",
                p.display()
            ),
            Self::UnknownFileFormat {
                path,
                write_version,
                read_version,
            } => write!(
                f,
                "Database '{}' has an unknown file format (write version {}, read version {}).",
                path.display(),
                write_version,
                read_version
            ),
            Self::UnexpectedSidecar(p) => write!(
                f,
                "Unexpected WAL/SHM sidecar '{}' next to the database; refusing to open it.",
                p.display()
            ),
            Self::OrphanSidecar(p) => write!(
                f,
                "Found '{}' but no database next to it; refusing to create a fresh database \
                 over the remains of another. Move the stray file away first.",
                p.display()
            ),
            Self::RecoveryRequired(p) => write!(
                f,
                "Database '{}' has a pending rollback journal from an interrupted write and needs \
                 recovery before it can be opened; it was left untouched.",
                p.display()
            ),
            Self::NotACoincubeDatabase(p) => write!(
                f,
                "'{}' is an SQLite database but not a Coincube one (no version/tip tables).",
                p.display()
            ),
            Self::UnsupportedVersion(v) => write!(f, "Unsupported database version '{}'.", v),
            Self::MalformedIdentity(msg) => write!(f, "Database identity is malformed: {}.", msg),
            Self::UnknownChain(s) => write!(f, "Database names an unknown chain '{}'.", s),
            Self::Sqlite(e) => write!(f, "SQLite error: '{}'", e),
        }
    }
}

impl std::error::Error for PreflightError {}

/// The header gate: everything we can learn from the first 100 bytes without SQLite.
fn inspect_header(db_path: &Path) -> Result<(), PreflightError> {
    let mut file = match fs::File::open(db_path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(PreflightError::NotFound(db_path.to_path_buf()))
        }
        Err(e) => return Err(PreflightError::Io(db_path.to_path_buf(), e)),
    };
    let mut header = [0u8; SQLITE_HEADER_LEN];
    if let Err(e) = file.read_exact(&mut header) {
        return Err(match e.kind() {
            io::ErrorKind::UnexpectedEof => {
                PreflightError::NotSqliteDatabase(db_path.to_path_buf())
            }
            _ => PreflightError::Io(db_path.to_path_buf(), e),
        });
    }
    if &header[..SQLITE_MAGIC.len()] != SQLITE_MAGIC {
        return Err(PreflightError::NotSqliteDatabase(db_path.to_path_buf()));
    }
    let (write_version, read_version) = (
        header[FORMAT_WRITE_VERSION_OFFSET],
        header[FORMAT_READ_VERSION_OFFSET],
    );
    match (write_version, read_version) {
        (FORMAT_LEGACY, FORMAT_LEGACY) => Ok(()),
        (FORMAT_WAL, _) | (_, FORMAT_WAL) => {
            Err(PreflightError::WalDatabase(db_path.to_path_buf()))
        }
        _ => Err(PreflightError::UnknownFileFormat {
            path: db_path.to_path_buf(),
            write_version,
            read_version,
        }),
    }
}

fn with_suffix(db_path: &Path, suffix: &str) -> PathBuf {
    let mut name = db_path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The `-wal` / `-shm` files SQLite keeps next to a WAL-mode database.
fn sidecar_paths(db_path: &Path) -> [PathBuf; 2] {
    [with_suffix(db_path, "-wal"), with_suffix(db_path, "-shm")]
}

fn refuse_sidecars(db_path: &Path) -> Result<(), PreflightError> {
    for sidecar in sidecar_paths(db_path) {
        if sidecar.exists() {
            return Err(PreflightError::UnexpectedSidecar(sidecar));
        }
    }
    Ok(())
}

/// For a data directory *without* a database at `db_path`: refuse to treat it as fresh when
/// the rollback journal or a WAL sidecar of a database is still there. Those files mean a
/// database existed (or was being created) and is gone; a fresh database written next to
/// them would inherit a hot journal that is not its own, or bury evidence someone may need.
/// Purely a filesystem check — no file is opened, created or removed.
pub fn refuse_orphan_sidecars(db_path: &Path) -> Result<(), PreflightError> {
    for sidecar in [
        with_suffix(db_path, "-journal"),
        with_suffix(db_path, "-wal"),
        with_suffix(db_path, "-shm"),
    ] {
        if sidecar.exists() {
            return Err(PreflightError::OrphanSidecar(sidecar));
        }
    }
    Ok(())
}

/// Translate SQLite's refusal codes into the typed reasons above.
fn sqlite_error(db_path: &Path, e: rusqlite::Error) -> PreflightError {
    if let rusqlite::Error::SqliteFailure(ffi_err, _) = &e {
        match ffi_err.extended_code {
            rusqlite::ffi::SQLITE_READONLY_ROLLBACK
            | rusqlite::ffi::SQLITE_READONLY_RECOVERY
            | rusqlite::ffi::SQLITE_READONLY_CANTINIT => {
                return PreflightError::RecoveryRequired(db_path.to_path_buf())
            }
            rusqlite::ffi::SQLITE_NOTADB => {
                return PreflightError::NotSqliteDatabase(db_path.to_path_buf())
            }
            rusqlite::ffi::SQLITE_CANTOPEN => {
                return PreflightError::NotFound(db_path.to_path_buf())
            }
            _ => {}
        }
    }
    PreflightError::Sqlite(e)
}

/// Exactly one row, or a typed refusal naming how many there were.
fn exactly_one<T>(what: &str, mut rows: Vec<T>) -> Result<T, PreflightError> {
    if rows.len() != 1 {
        return Err(PreflightError::MalformedIdentity(format!(
            "expected exactly one {} row, found {}",
            what,
            rows.len()
        )));
    }
    Ok(rows.pop().expect("length checked"))
}

fn query_all<T>(
    conn: &rusqlite::Connection,
    db_path: &Path,
    stmt: &str,
    f: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, PreflightError> {
    conn.prepare(stmt)
        .and_then(|mut s| s.query_map([], f)?.collect::<rusqlite::Result<Vec<T>>>())
        .map_err(|e| sqlite_error(db_path, e))
}

/// Read the version and chain identity of the database at `db_path` without modifying it.
/// See the module documentation for exactly what that promise covers.
///
/// The version and the identity rows are read inside one read transaction, so they come from
/// a single snapshot: while it is open, another connection may prepare a write (an ordinary
/// migration, say) but cannot commit one, so the version this returns is the version the
/// identity row was read under. That is a guarantee about SQLite connections sharing the file
/// through its locking protocol; it does not cover a process replacing the file underneath.
pub fn read_stored_identity(db_path: &Path) -> Result<StoredIdentity, PreflightError> {
    read_stored_identity_with(db_path, || {})
}

/// [`read_stored_identity`] with a hook that runs between the version read and the identity
/// read, inside the read transaction. Production passes a no-op; the snapshot test uses it to
/// interleave a concurrent migration attempt deterministically.
pub(super) fn read_stored_identity_with(
    db_path: &Path,
    between_reads: impl FnOnce(),
) -> Result<StoredIdentity, PreflightError> {
    inspect_header(db_path)?;
    refuse_sidecars(db_path)?;

    let mut conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| sqlite_error(db_path, e))?;
    conn.busy_timeout(std::time::Duration::from_secs(60))
        .map_err(|e| sqlite_error(db_path, e))?;
    conn.pragma_update(None, "query_only", true)
        .map_err(|e| sqlite_error(db_path, e))?;

    // One read transaction for everything below. Deferred: the shared lock is taken by the
    // first SELECT and held until the transaction ends, which is exactly the snapshot we want.
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
        .map_err(|e| sqlite_error(db_path, e))?;

    // Is this one of ours at all? (The first statement is also where SQLite takes its shared
    // lock and notices a hot journal.)
    let tables = query_all(
        &tx,
        db_path,
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('version', 'tip')",
        |row| row.get::<_, String>(0),
    )?;
    if tables.len() != 2 {
        return Err(PreflightError::NotACoincubeDatabase(db_path.to_path_buf()));
    }

    let version = exactly_one(
        "version",
        query_all(&tx, db_path, "SELECT version FROM version", |row| {
            row.get::<_, i64>(0)
        })?,
    )?;
    if !(0..=DB_VERSION).contains(&version) {
        return Err(PreflightError::UnsupportedVersion(version));
    }

    between_reads();

    let (network, chain) = if version <= MAX_DB_VERSION_NO_CHAIN {
        // A legacy row only ever held a `bitcoin::Network`, so that is the only alphabet
        // accepted — a fork string here is unknown, never "backfilled" into a fork identity.
        let raw = exactly_one(
            "tip",
            query_all(&tx, db_path, "SELECT network FROM tip", |row| {
                row.get::<_, String>(0)
            })?,
        )?;
        let network =
            Network::from_str(&raw).map_err(|_| PreflightError::UnknownChain(raw.clone()))?;
        (network, ChainId::from(network))
    } else {
        let (raw_network, raw_chain) = exactly_one(
            "tip",
            query_all(&tx, db_path, "SELECT network, chain FROM tip", |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?,
        )?;
        let network = Network::from_str(&raw_network).map_err(|_| {
            PreflightError::MalformedIdentity(format!("unknown network '{}'", raw_network))
        })?;
        let chain = ChainId::from_dir_name(&raw_chain)
            .ok_or_else(|| PreflightError::UnknownChain(raw_chain.clone()))?;
        if chain.bitcoin_network() != network {
            return Err(PreflightError::MalformedIdentity(format!(
                "chain '{}' encodes as '{}' but the network column says '{}'",
                chain,
                chain.bitcoin_network(),
                network
            )));
        }
        (network, chain)
    };

    // A read transaction has nothing to commit; ending it releases the shared lock.
    tx.rollback().map_err(|e| sqlite_error(db_path, e))?;

    Ok(StoredIdentity {
        version,
        chain,
        network,
    })
}
