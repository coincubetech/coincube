mod bitcoin;
pub mod commands;
pub mod config;
pub mod connect;
mod database;
pub mod datadir;
mod jsonrpc;
pub mod poison_broadcast;
#[cfg(test)]
mod testutils;

pub use bdk_electrum::electrum_client;
pub use bdk_esplora::esplora_client;
pub use bip329;
use bitcoin::electrum;
use datadir::DataDirectory;
pub use miniscript;

pub use crate::bitcoin::{
    d::{
        valid_node_instance, BitcoinD, BitcoindError, Blake2bDeploymentInfo, ChainStatus,
        ChainTipEntry, DeploymentProbeError, DeploymentStatus, ForkActivation, PruneState,
        RdtsSchedule, WalletError, NODE_INSTANCE_FILE, NODE_INSTANCE_LEN,
    },
    electrum::{Electrum, ElectrumError},
    esplora::{Esplora, EsploraError},
    managed_node_maintenance, sanctioned_rollback, set_managed_node_maintenance,
    set_sanctioned_rollback, BackendId, BlockChainTip, MaintenanceGuard, SanctionedRollback,
};

use crate::jsonrpc::server;
use crate::{
    bitcoin::{poller, BitcoinInterface},
    config::{Config, ConfigError},
    database::{
        sqlite::{preflight, FreshDbOptions, SqliteDb, SqliteDbError, MAX_DB_VERSION_NO_TX_DB},
        DatabaseInterface,
    },
};
pub use database::sqlite::preflight::{PreflightError, StoredIdentity};

use coincube_core::chain::ChainId;

use std::{
    error, fmt, io, path,
    sync::{self, mpsc},
    thread,
};

use miniscript::bitcoin::{constants::ChainHash, hashes::Hash, secp256k1, BlockHash};

#[cfg(not(test))]
use std::panic;
// A panic in any thread should stop the main thread, and print the panic.
#[cfg(not(test))]
pub fn setup_panic_hook() {
    panic::set_hook(Box::new(move |panic_info| {
        let file = panic_info
            .location()
            .map(|l| l.file())
            .unwrap_or_else(|| "'unknown'");
        let line = panic_info
            .location()
            .map(|l| l.line().to_string())
            .unwrap_or_else(|| "'unknown'".to_string());

        let bt = backtrace::Backtrace::new();
        let info = panic_info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic_info.payload().downcast_ref::<String>().cloned());
        log::error!(
            "panic occurred at line {} of file {}: {:?}\n{:?}",
            line,
            file,
            info,
            bt
        );
    }));
}

#[derive(Debug, Clone)]
pub struct ApiVersion<'a>(pub &'a str);

impl ApiVersion<'_> {
    pub fn major(&self) -> u32 {
        self.0
            .split('.')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }
}

impl fmt::Display for ApiVersion<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if cfg!(debug_assertions) {
            write!(f, "{}-dev", self.0)
        } else {
            write!(f, "{}", self.0)
        }
    }
}

pub const VERSION: ApiVersion = ApiVersion(env!("CARGO_PKG_VERSION"));

#[derive(Debug)]
pub enum StartupError {
    Io(io::Error),
    /// The in-memory configuration is inconsistent (identity vs. encoding). Checked before any
    /// filesystem access, because `start` can be handed a `Config` that never went through
    /// `Config::from_file` and its `check`.
    Config(ConfigError),
    /// The configured chain has no runtime in this build. Refused before any I/O.
    ChainDormant(ChainId),
    ConnectAdmission(connect::AdmissionError),
    /// The existing database belongs to another chain than the configuration names. Refused
    /// before the data directory, the watch-only wallet or any migration is touched.
    ChainMismatch {
        config: ChainId,
        stored: ChainId,
    },
    /// The existing database could not be identified without modifying it.
    DbPreflight(PreflightError),
    DefaultDataDirNotFound,
    DatadirCreation(path::PathBuf, io::Error),
    MissingBitcoindConfig,
    MissingElectrumConfig,
    MissingEsploraConfig,
    MissingBitcoinBackendConfig,
    DbMigrateBitcoinTxs(&'static str),
    Database(SqliteDbError),
    Bitcoind(BitcoindError),
    Electrum(ElectrumError),
    Esplora(EsploraError),
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::ConnectAdmission(e) => write!(f, "{}", e),
            Self::Io(e) => write!(f, "{}", e),
            Self::Config(e) => write!(f, "{}", e),
            Self::ChainDormant(chain) => write!(
                f,
                "This build carries the identity of chain '{}' but cannot run a wallet on it yet; \
                 nothing was created or modified.",
                chain
            ),
            Self::ChainMismatch { config, stored } => write!(
                f,
                "The database in this data directory was created for chain '{}' but the \
                 configuration is for chain '{}'; nothing was created or modified.",
                stored, config
            ),
            Self::DbPreflight(e) => write!(
                f,
                "Could not establish which chain the existing database belongs to; nothing was \
                 created or modified: {}",
                e
            ),
            Self::DefaultDataDirNotFound => write!(
                f,
                "Not data directory was specified and a default path could not be determined for this platform."
            ),
            Self::DatadirCreation(dir_path, e) => write!(
                f,
                "Could not create data directory at '{}': '{}'", dir_path.display(), e
            ),
            Self::MissingBitcoindConfig => write!(
                f,
                "Our Bitcoin interface is bitcoind but we have no 'bitcoind_config' entry in the configuration."
            ),
            Self::MissingElectrumConfig => write!(
                f,
                "Our Bitcoin interface is Electrum but we have no 'electrum_config' entry in the configuration."
            ),
            Self::MissingEsploraConfig => write!(
                f,
                "Our Bitcoin interface is Esplora but we have no 'esplora_config' entry in the configuration."
            ),
            Self::MissingBitcoinBackendConfig => write!(
                f,
                "No Bitcoin backend entry in the configuration."
            ),
            Self::DbMigrateBitcoinTxs(msg) => write!(
                f,
                "Error when migrating Bitcoin transaction from Bitcoin backend to database: {}.", msg
            ),
            Self::Database(e) => write!(f, "Error initializing database: '{}'.", e),
            Self::Bitcoind(e) => write!(f, "Error setting up bitcoind interface: '{}'.", e),
            Self::Electrum(e) => write!(f, "Error setting up Electrum interface: '{}'.", e),
            Self::Esplora(e) => write!(f, "Error setting up Esplora interface: '{}'.", e),
        }
    }
}

impl error::Error for StartupError {}

impl From<io::Error> for StartupError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<SqliteDbError> for StartupError {
    fn from(e: SqliteDbError) -> Self {
        Self::Database(e)
    }
}

impl From<BitcoindError> for StartupError {
    fn from(e: BitcoindError) -> Self {
        Self::Bitcoind(e)
    }
}

/// Configuration-only and injected-interface startup cannot admit fork wallets.
/// The separate authenticated embedded entry point requires a native-P2WSH
/// descriptor and ephemeral Connect authority before any wallet write.
fn chain_runtime_gate(chain: ChainId) -> Result<(), StartupError> {
    if chain.is_blake2b() {
        return Err(StartupError::ChainDormant(chain));
    }
    Ok(())
}

fn connect_startup_error(error: crate::bitcoin::esplora::client::Error) -> StartupError {
    use crate::bitcoin::esplora::client::Error;
    match error {
        Error::Admission(error) => StartupError::ConnectAdmission(error),
        Error::AllCooling => StartupError::ConnectAdmission(connect::AdmissionError::Throttled),
        Error::Aborted => StartupError::ConnectAdmission(connect::AdmissionError::Aborted),
        other => StartupError::Esplora(EsploraError::Client(other)),
    }
}

/// Establish, without modifying anything, that the database already in `data_dir` belongs to
/// the configured chain (and its encoding). Runs before the data directory is created, before
/// the bitcoind watch-only wallet is created or loaded, and before any migration or healing, so
/// a wrong-chain or unidentifiable database is refused with nothing touched.
fn preflight_existing_database(
    config: &Config,
    db_path: &path::Path,
) -> Result<StoredIdentity, StartupError> {
    let stored = preflight::read_stored_identity(db_path).map_err(StartupError::DbPreflight)?;
    let chain = config.bitcoin_config.chain;
    if stored.chain != chain {
        return Err(StartupError::ChainMismatch {
            config: chain,
            stored: stored.chain,
        });
    }
    if stored.network != config.bitcoin_config.network {
        // Unreachable when the config passed `check_chain_encoding` (the stored pair is
        // consistent by construction of the preflight), kept as a typed refusal anyway.
        return Err(StartupError::Database(SqliteDbError::InvalidNetwork(
            stored.network,
        )));
    }
    log::info!(
        "Existing database is for chain '{}' (schema version {}), matching the configuration.",
        stored.chain,
        stored.version
    );
    Ok(stored)
}

// Connect to the SQLite database. Create it if starting fresh, and do some sanity checks.
// If all went well, returns the interface to the SQLite database.
fn setup_sqlite(
    config: &Config,
    data_dir: &DataDirectory,
    fresh_data_dir: bool,
    secp: &secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    bitcoind: &Option<BitcoinD>,
) -> Result<SqliteDb, StartupError> {
    let db_path = data_dir.sqlite_db_file_path();
    let options = if fresh_data_dir {
        Some(FreshDbOptions::new(
            config.bitcoin_config.chain,
            config.main_descriptor.clone(),
        ))
    } else {
        None
    };

    // If opening an existing wallet whose database does not yet store the wallet transactions,
    // query them from the Bitcoin backend before proceeding to the migration.
    let sqlite = SqliteDb::new(db_path, options, secp)?;
    if !fresh_data_dir {
        let mut conn = sqlite.connection()?;
        let wallet_txs = if conn.db_version() <= MAX_DB_VERSION_NO_TX_DB {
            let bit = bitcoind.as_ref().ok_or(StartupError::DbMigrateBitcoinTxs(
                "a connection to a Bitcoin backend is required",
            ))?;
            let coins_txids = conn.db_list_all_txids();
            coins_txids
                .into_iter()
                .map(|txid| bit.get_transaction(&txid).map(|res| res.tx))
                .collect::<Option<Vec<_>>>()
                .ok_or(StartupError::DbMigrateBitcoinTxs(
                    "missing transaction in Bitcoin backend",
                ))?
        } else {
            Vec::new()
        };
        sqlite.maybe_apply_migrations(&wallet_txs)?;
    }

    sqlite.sanity_check(config.bitcoin_config.chain, &config.main_descriptor)?;
    log::info!("Database initialized and checked.");

    Ok(sqlite)
}

// Connect to bitcoind. Setup the watchonly wallet, and do some sanity checks.
// If all went well, returns the interface to bitcoind.
/// Trigger a rescan when the watchonly wallet was never scanned over the history
/// our own database already holds.
///
/// # The state this catches
///
/// `import_descriptor` imports at `timestamp: "now"`, which is right for a Vault
/// being created — there is nothing behind it to find. It is wrong for one being
/// *restored*: the descriptors go in, bitcoind scans from today forward, and
/// every transaction that funded the wallet before the restore is invisible to
/// it. The database, restored alongside, knows about those coins perfectly well.
///
/// The two halves then disagree forever. `get_spender_txid` asks the wallet
/// about a coin's funding transaction, bitcoind answers `-5`, and the coin can
/// never be resolved as spent or unspent — so it stays selectable and the user
/// can build transactions that conflict with their own pending ones. Nothing
/// recovers on its own; only a rescan fixes it.
///
/// [`BitcoinD::wallet_sanity_checks`] does not cover this. It asks whether the
/// wallet is loaded and whether our descriptors are in it — both true here. The
/// missing question is *since when*.
///
/// # Why the daemon heals it rather than reporting it
///
/// Because the daemon is the only place that can do so reliably. The GUI used
/// to own this: a restore recorded the rescan it owed and started one once the
/// app was up. That holds right until something re-creates the watchonly wallet
/// afterwards — a backend switch does exactly that, and `setup_bitcoind` builds
/// the replacement at `timestamp: "now"` — and the completed scan is silently
/// discarded with nothing left to notice it. Here the check runs *after* every
/// wallet creation, on every start, so a discarded scan is simply redone and no
/// ordering can defeat it.
///
/// The date comes from our own database — the oldest confirmed coin — rather
/// than from a Recovery Kit, so this covers wallets recreated for reasons that
/// have nothing to do with a restore.
///
/// # What it costs, and what it does not
///
/// It **triggers** a rescan; it does not wait for one.
/// [`BitcoinD::start_rescan`] re-imports the descriptors through a no-reply
/// request precisely so the caller does not block for the scan's duration, and
/// returns as soon as bitcoind reports the new timestamps on the wallet. Startup
/// therefore continues while the chain scan runs in the background, exactly as
/// it does for a rescan started from Settings > Node.
///
/// So the coins stay unresolvable for as long as the scan takes, and the poller
/// logs about them meanwhile. That is the same window a manually started rescan
/// has; what this removes is the need for anyone to notice and start one.
/// The oldest coin the wallet's scan window does not cover, if any.
///
/// Split out from [`heal_scan_window`] so the comparison is testable without
/// standing up a node and a database. The boundary is deliberately exclusive: a
/// wallet scanned from exactly the oldest coin's block time *did* see that
/// block, and firing there would rescan on every correctly restored wallet.
fn history_outside_scan_window(
    scanned_from: u32,
    oldest_coin: Option<database::BlockInfo>,
) -> Option<database::BlockInfo> {
    oldest_coin.filter(|oldest| scanned_from > oldest.time)
}

/// Whether the wallet needs (re)scanning, given what it claims and what it holds.
///
/// Two independent tests, because the first one lies.
///
/// `scanned_from` is the descriptors' timestamp, and bitcoind stamps those when
/// the import is *requested*, not when the scan finishes — keeping the stamp
/// even if a shutdown aborts the scan half-way. A wallet interrupted mid-scan
/// therefore claims a window it never covered, and the timestamp comparison
/// alone reports it healthy forever.
///
/// `newest_coin_known` is the corrective: the wallet is asked whether it
/// actually holds the funding transaction of the most recent coin we have. A
/// scan runs forward from its start point, so the newest coin is the last thing
/// it would have found and the first thing missing when it is cut short.
///
/// Returns the point to scan from — always the *oldest* coin, since a scan that
/// stopped early has to be redone from the beginning, not resumed.
/// The two points [`heal_scan_window`] needs from our confirmed coins: where a
/// repair would have to start, and which coin to probe the wallet for.
///
/// They are chosen by **different keys on purpose**.
///
/// The probe is the coin in the *highest block*. A scan runs in chain order, so
/// the highest block is the last thing it reaches and the first thing missing
/// when it is cut short. Picking by block *time* instead would be wrong:
/// Bitcoin block timestamps only have to beat the median of the previous
/// eleven, so they tie and go backwards routinely. A coin in an earlier block
/// can carry a later timestamp, and probing that one — which a truncated scan
/// would already hold — reports the wallet healthy while coins in later blocks
/// are still missing.
///
/// The start point is the *earliest timestamp* of any coin, which is what
/// `importdescriptors` takes. Under the same non-monotonicity that is the
/// conservative choice: it is at or before the timestamp of the lowest block we
/// hold, so the rescan cannot begin after history we need.
fn scan_probe_points(
    confirmed: &[(database::BlockInfo, miniscript::bitcoin::OutPoint)],
) -> Option<(database::BlockInfo, miniscript::bitcoin::OutPoint)> {
    let start_from = confirmed
        .iter()
        .map(|(block, _)| *block)
        .min_by_key(|b| b.time)?;
    let probe = confirmed
        .iter()
        .max_by_key(|(block, _)| block.height)
        .map(|(_, outpoint)| *outpoint)?;
    Some((start_from, probe))
}

fn scan_repair_needed(
    scanned_from: u32,
    oldest_coin: Option<database::BlockInfo>,
    newest_coin_known: bool,
) -> Option<database::BlockInfo> {
    let oldest = oldest_coin?;
    if history_outside_scan_window(scanned_from, Some(oldest)).is_some() || !newest_coin_known {
        Some(oldest)
    } else {
        None
    }
}

fn heal_scan_window(
    bitcoind: &mut BitcoinD,
    db: &sync::Arc<sync::Mutex<dyn DatabaseInterface>>,
    descriptor: &coincube_core::descriptors::CoincubeDescriptor,
) {
    // The database first, deliberately. It is a local read, and when it comes
    // back empty — every fresh install — there is no history that could fall
    // outside any window, so the node is never asked. Ordering it the other way
    // would put an RPC on every startup to answer a question already moot.
    //
    // Unconfirmed coins have no block time and cannot be outside a scan window,
    // so they are skipped.
    let coins = db
        .lock()
        .expect("db mutex poisoned")
        .connection()
        .coins(&[], &[]);
    let confirmed: Vec<_> = coins
        .into_iter()
        .filter_map(|(outpoint, coin)| coin.block_info.map(|block| (block, outpoint)))
        .collect();
    let Some((oldest, newest_outpoint)) = scan_probe_points(&confirmed) else {
        return;
    };

    let Some(scanned_from) = bitcoind.earliest_descriptor_timestamp() else {
        return;
    };
    // Asked only when there is something to ask about, and only about one coin:
    // a wallet RPC per start, not per coin.
    let newest_coin_known = match bitcoind.knows_transaction(&newest_outpoint.txid) {
        Ok(known) => known,
        // Not an answer, so it must not be read as one. Fall back to the
        // descriptor-window test: a window that is plainly short still triggers
        // a repair, while a cut-short scan that only this probe could catch
        // waits for the next start rather than costing a full rescan of a
        // wallet that was never asked about.
        Err(e) => {
            log::warn!(
                concat!(
                    "Could not ask the watchonly wallet whether it holds '{}': {}. Judging the ",
                    "scan window on the descriptor timestamps alone this time; a repair this ",
                    "misses is retried on the next start."
                ),
                newest_outpoint.txid,
                e
            );
            true
        }
    };

    let Some(oldest) = scan_repair_needed(scanned_from, Some(oldest), newest_coin_known) else {
        return;
    };

    log::warn!(
        concat!(
            "Watchonly wallet cannot see coins we hold: it reports being scanned from unix ",
            "time {}, our database holds coins as far back as block {} (unix time {}), and ",
            "the newest of them is {} to it. Those coins cannot be tracked and may be ",
            "offered for spending after they are already spent. Expected after restoring ",
            "onto a fresh node, after anything that re-created the wallet, and after a scan ",
            "cut short by a shutdown. Triggering a rescan from unix time {}; it runs in the ",
            "background and these coins stay unresolvable until it finishes."
        ),
        scanned_from,
        oldest.height,
        oldest.time,
        if newest_coin_known {
            "known"
        } else {
            "unknown"
        },
        oldest.time,
    );

    match bitcoind.start_rescan(descriptor, oldest.time) {
        // bitcoind has accepted the re-import and stamped the descriptors; the
        // scan itself is still running. "Started", never "complete".
        Ok(()) => log::info!(
            "Rescan of the watchonly wallet started from unix time {}.",
            oldest.time
        ),
        // Not fatal: the daemon still runs, the coins simply stay unresolvable
        // and the next start tries again — including when the scan is aborted
        // or interrupted after this returns, which this call cannot observe.
        // Refusing to boot over this would be worse than running degraded.
        Err(e) => log::error!(
            concat!(
                "Could not start a rescan of the watchonly wallet from unix time {}: {}. ",
                "Coins older than the wallet's scan window stay untrackable until one runs."
            ),
            oldest.time,
            e
        ),
    }
}

fn setup_bitcoind(
    config: &Config,
    data_dir: &DataDirectory,
    fresh_data_dir: bool,
) -> Result<BitcoinD, StartupError> {
    let wo_path: path::PathBuf = data_dir.coincubed_watchonly_wallet_path();
    let wo_path_str = wo_path.to_str().expect("Must be valid unicode").to_string();
    // NOTE: On Windows, paths are canonicalized with a "\\?\" prefix to tell Windows to interpret
    // the string "as is" and to ignore the maximum size of a path. HOWEVER this is not properly
    // handled by most implementations of the C++ STL's std::filesystem. Therefore bitcoind would
    // fail to find the wallet if we didn't strip this prefix. It's not ideal, but a lesser evil
    // than other workarounds i could think about.
    // See https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file#win32-file-namespaces
    // about the prefix.
    // See https://stackoverflow.com/questions/71590689/how-to-properly-handle-windows-paths-with-the-long-path-prefix-with-stdfilesys
    // for a discussion of how one C++ STL implementation handles this.
    #[cfg(target_os = "windows")]
    let wo_path_str = wo_path_str.replace("\\\\?\\", "").replace("\\\\?", "");

    let bitcoind_config = match config.bitcoin_backend.as_ref() {
        Some(config::BitcoinBackend::Bitcoind(bitcoind_config)) => bitcoind_config,
        _ => Err(StartupError::MissingBitcoindConfig)?,
    };
    let bitcoind = BitcoinD::new(bitcoind_config, wo_path_str)?;
    bitcoind.node_sanity_checks(
        config.bitcoin_config.network,
        config.main_descriptor.is_taproot(),
    )?;
    if fresh_data_dir || !wo_path.exists() {
        log::info!("Creating a new watchonly wallet on bitcoind.");
        bitcoind.create_watchonly_wallet(&config.main_descriptor)?;
        log::info!("Watchonly wallet created.");
    }
    log::info!("Loading our watchonly wallet on bitcoind.");
    bitcoind.maybe_load_watchonly_wallet()?;
    bitcoind.wallet_sanity_checks(&config.main_descriptor)?;
    log::info!("Watchonly wallet loaded on bitcoind and sanity checked.");

    Ok(bitcoind)
}

// Create an Electrum interface from a client and BDK-based wallet, and do some sanity checks.
// If all went well, returns the interface to Electrum.
fn setup_electrum(
    config: &Config,
    db: sync::Arc<sync::Mutex<dyn DatabaseInterface>>,
) -> Result<Electrum, StartupError> {
    let electrum_config = match config.bitcoin_backend.as_ref() {
        Some(config::BitcoinBackend::Electrum(electrum_config)) => electrum_config,
        _ => Err(StartupError::MissingElectrumConfig)?,
    };
    // First create the client to communicate with the Electrum server.
    let client = electrum::client::Client::new(electrum_config)
        .map_err(|e| StartupError::Electrum(ElectrumError::Client(e)))?;
    // Then create the BDK-based wallet and populate it with DB data.
    let mut db_conn = db.connection();
    let tip = db_conn.chain_tip();
    let coins: Vec<_> = db_conn
        .coins(&[], &[])
        .into_values()
        .map(|c| crate::bitcoin::Coin {
            outpoint: c.outpoint,
            amount: c.amount,
            derivation_index: c.derivation_index,
            is_change: c.is_change,
            is_immature: c.is_immature,
            block_info: c.block_info.map(|info| crate::bitcoin::BlockInfo {
                height: info.height,
                time: info.time,
            }),
            spend_txid: c.spend_txid,
            spend_block: c.spend_block.map(|info| crate::bitcoin::BlockInfo {
                height: info.height,
                time: info.time,
            }),
        })
        .collect();
    let txids = db_conn.list_saved_txids();
    // This will only return those txs referenced by our coins, which may not be all of `txids`.
    let txs: Vec<_> = db_conn
        .list_wallet_transactions(&txids)
        .into_iter()
        .map(|(tx, _, _)| tx)
        .collect();
    let (receive_index, change_index) = (db_conn.receive_index(), db_conn.change_index());
    let genesis_hash = {
        let chain_hash = ChainHash::using_genesis_block(config.bitcoin_config.network);
        BlockHash::from_byte_array(*chain_hash.as_bytes())
    };
    let bdk_wallet = electrum::wallet::BdkWallet::new(
        &config.main_descriptor,
        genesis_hash,
        tip,
        &coins,
        &txs,
        receive_index,
        change_index,
    );
    let full_scan = db_conn.rescan_timestamp().is_some();
    let electrum = Electrum::new(client, bdk_wallet, full_scan).map_err(StartupError::Electrum)?;
    electrum
        .sanity_checks(&genesis_hash)
        .map_err(StartupError::Electrum)?;
    Ok(electrum)
}

// Create an Esplora interface from a client and BDK-based wallet, and do some sanity checks.
// If all went well, returns the interface to Esplora.
fn setup_esplora(
    config: &Config,
    db: sync::Arc<sync::Mutex<dyn DatabaseInterface>>,
    scan_abort: sync::Arc<sync::atomic::AtomicBool>,
    prepared_client: Option<crate::bitcoin::esplora::client::Client>,
) -> Result<Esplora, StartupError> {
    let esplora_config = match config.bitcoin_backend.as_ref() {
        Some(config::BitcoinBackend::Esplora(esplora_config)) => esplora_config,
        _ => Err(StartupError::MissingEsploraConfig)?,
    };
    let genesis_hash = {
        let chain_hash = ChainHash::using_genesis_block(config.bitcoin_config.network);
        BlockHash::from_byte_array(*chain_hash.as_bytes())
    };
    let client = match prepared_client {
        Some(client) => client,
        None => crate::bitcoin::esplora::client::Client::new(esplora_config, scan_abort)
            .map_err(|e| StartupError::Esplora(EsploraError::Client(e)))?,
    };
    let mut db_conn = db.connection();
    let tip = db_conn.chain_tip();
    let coins: Vec<_> = db_conn
        .coins(&[], &[])
        .into_values()
        .map(|c| crate::bitcoin::Coin {
            outpoint: c.outpoint,
            amount: c.amount,
            derivation_index: c.derivation_index,
            is_change: c.is_change,
            is_immature: c.is_immature,
            block_info: c.block_info.map(|info| crate::bitcoin::BlockInfo {
                height: info.height,
                time: info.time,
            }),
            spend_txid: c.spend_txid,
            spend_block: c.spend_block.map(|info| crate::bitcoin::BlockInfo {
                height: info.height,
                time: info.time,
            }),
        })
        .collect();
    let txids = db_conn.list_saved_txids();
    let txs: Vec<_> = db_conn
        .list_wallet_transactions(&txids)
        .into_iter()
        .map(|(tx, _, _)| tx)
        .collect();
    let (receive_index, change_index) = (db_conn.receive_index(), db_conn.change_index());
    let bdk_wallet = electrum::wallet::BdkWallet::new(
        &config.main_descriptor,
        genesis_hash,
        tip,
        &coins,
        &txs,
        receive_index,
        change_index,
    );
    let full_scan = db_conn.rescan_timestamp().is_some();
    let esplora = Esplora::new(client, bdk_wallet, full_scan).map_err(StartupError::Esplora)?;
    esplora
        .sanity_checks(&genesis_hash)
        .map_err(StartupError::Esplora)?;
    Ok(esplora)
}

#[derive(Clone)]
pub struct DaemonControl {
    config: Config,
    bitcoin: sync::Arc<sync::Mutex<dyn BitcoinInterface>>,
    poller_sender: mpsc::SyncSender<poller::PollerMessage>,
    // FIXME: Should we require Sync on DatabaseInterface rather than using a Mutex?
    db: sync::Arc<sync::Mutex<dyn DatabaseInterface>>,
    secp: secp256k1::Secp256k1<secp256k1::VerifyOnly>,
    // Lock-free mirror of the poller's latest sync progress. `get_info` reads
    // this instead of locking `bitcoin`, so it never blocks behind the poller's
    // full wallet scan (the "Starting daemon…" stall).
    sync_progress_cache: sync::Arc<crate::bitcoin::SyncProgressCache>,
    // Lock-free mirror of the poller's "refused an implausibly deep reorg" alert,
    // read by `get_info` for the same reason as `sync_progress_cache`.
    reorg_alert_cache: sync::Arc<crate::bitcoin::ReorgAlertCache>,
}

impl DaemonControl {
    pub(crate) fn new(
        config: Config,
        bitcoin: sync::Arc<sync::Mutex<dyn BitcoinInterface>>,
        poller_sender: mpsc::SyncSender<poller::PollerMessage>,
        db: sync::Arc<sync::Mutex<dyn DatabaseInterface>>,
        secp: secp256k1::Secp256k1<secp256k1::VerifyOnly>,
        sync_progress_cache: sync::Arc<crate::bitcoin::SyncProgressCache>,
        reorg_alert_cache: sync::Arc<crate::bitcoin::ReorgAlertCache>,
    ) -> DaemonControl {
        DaemonControl {
            config,
            bitcoin,
            poller_sender,
            db,
            secp,
            sync_progress_cache,
            reorg_alert_cache,
        }
    }

    // Useful for unit test to directly mess up with the DB
    #[cfg(test)]
    pub fn db(&self) -> sync::Arc<sync::Mutex<dyn DatabaseInterface>> {
        self.db.clone()
    }
}

/// The handle to a Coincube daemon. It might either be the handle for a daemon which exposes a
/// JSONRPC server or one which exposes its API through a `DaemonControl`.
#[allow(clippy::large_enum_variant)]
pub enum DaemonHandle {
    Controller {
        poller_sender: mpsc::SyncSender<poller::PollerMessage>,
        poller_handle: thread::JoinHandle<()>,
        control: DaemonControl,
        /// Set on `stop` so an in-flight Esplora scan aborts promptly instead of
        /// blocking the poller join on requests to dead/throttled providers.
        /// Unused (but harmless) for the bitcoind/electrum backends.
        scan_abort: sync::Arc<sync::atomic::AtomicBool>,
    },
    Server {
        poller_sender: mpsc::SyncSender<poller::PollerMessage>,
        poller_handle: thread::JoinHandle<()>,
        rpcserver_shutdown: sync::Arc<sync::atomic::AtomicBool>,
        rpcserver_handle: thread::JoinHandle<Result<(), io::Error>>,
        /// See [`DaemonHandle::Controller::scan_abort`].
        scan_abort: sync::Arc<sync::atomic::AtomicBool>,
    },
}

impl DaemonHandle {
    /// This starts the Coincube daemon. A user of this interface should regularly poll the `is_alive`
    /// method to check for internal errors. To shut down the daemon use the `stop` method.
    ///
    /// The `with_rpc_server` controls whether we should start a JSONRPC server to receive queries
    /// or instead return a `DaemonControl` object for a caller to access the daemon's API.
    ///
    /// You may specify a custom Bitcoin interface through the `bitcoin` parameter. If `None`, the
    /// default Bitcoin interface (`bitcoind` JSONRPC) will be used.
    /// You may specify a custom Database interface through the `db` parameter. If `None`, the
    /// default Database interface (SQLite) will be used.
    pub fn start(
        config: Config,
        bitcoin: Option<impl BitcoinInterface + 'static>,
        db: Option<impl DatabaseInterface + 'static>,
        with_rpc_server: bool,
    ) -> Result<Self, StartupError> {
        Self::start_inner(
            config,
            bitcoin,
            db,
            with_rpc_server,
            None,
            chain_runtime_gate,
        )
    }

    /// Start an embedded native-P2WSH fork wallet with ephemeral authenticated Connect
    /// authority. Generic daemon startup and external JSON-RPC remain unavailable for forks.
    /// Admission and existing database identity checks still precede every write.
    pub fn start_with_connect(
        config: Config,
        backend: connect::ConnectBackend,
        with_rpc_server: bool,
    ) -> Result<Self, StartupError> {
        if !config.bitcoin_config.chain.is_blake2b()
            || with_rpc_server
            || config.pending_bitcoind.is_some()
            || !matches!(
                config.main_descriptor.descriptor(),
                miniscript::Descriptor::Wsh(_)
            )
        {
            return Err(StartupError::ConnectAdmission(
                connect::AdmissionError::InvalidBackend,
            ));
        }
        Self::start_inner(
            config,
            Option::<BitcoinD>::None,
            Option::<SqliteDb>::None,
            false,
            Some(backend),
            // Only this authenticated, embedded, native-P2WSH entry point opens
            // the fork runtime. start/start_default retain chain_runtime_gate.
            |_| Ok(()),
        )
    }

    fn start_inner(
        config: Config,
        bitcoin: Option<impl BitcoinInterface + 'static>,
        db: Option<impl DatabaseInterface + 'static>,
        with_rpc_server: bool,
        connect: Option<connect::ConnectBackend>,
        // Generic startup refuses forks. Authenticated embedded startup has its
        // own narrow capability checks before entering this shared sequence.
        runtime_gate: fn(ChainId) -> Result<(), StartupError>,
    ) -> Result<Self, StartupError> {
        let secp = secp256k1::Secp256k1::verification_only();

        // Before touching anything: the configuration must be self-consistent, and this build
        // must be able to run the chain it names. Neither check does any I/O.
        config
            .bitcoin_config
            .check_chain_encoding()
            .map_err(StartupError::Config)?;
        if let Some(backend) = connect.as_ref() {
            if backend.chain() != config.bitcoin_config.chain
                || bitcoin.is_some()
                || !matches!(
                    config.bitcoin_backend.as_ref(),
                    Some(config::BitcoinBackend::Esplora(selection)) if backend.matches_selection(selection)
                )
            {
                return Err(StartupError::ConnectAdmission(
                    connect::AdmissionError::InvalidBackend,
                ));
            }
        }
        runtime_gate(config.bitcoin_config.chain)?;
        // Authenticated admission precedes every filesystem/database/node write.
        let scan_abort = sync::Arc::new(sync::atomic::AtomicBool::new(false));
        let prepared_client = match connect {
            Some(backend) => Some(
                crate::bitcoin::esplora::client::Client::new_for_connect(
                    backend,
                    scan_abort.clone(),
                )
                .map_err(connect_startup_error)?,
            ),
            None if config.bitcoin_config.chain.is_blake2b() => {
                return Err(StartupError::ConnectAdmission(
                    connect::AdmissionError::MissingAuth,
                ))
            }
            None => None,
        };

        // Then check the data directory. An existing database must belong to the configured
        // chain before we create anything, set up the watch-only wallet or migrate it.
        let data_dir = config
            .data_directory()
            .ok_or(StartupError::DefaultDataDirNotFound)?;
        let fresh_data_dir = !data_dir.exists() || !data_dir.sqlite_db_file_path().exists();
        if !fresh_data_dir {
            preflight_existing_database(&config, &data_dir.sqlite_db_file_path())?;
        } else if data_dir.exists() {
            // A directory without a database is only "fresh" if nothing of a database is left
            // in it: a stray rollback journal or WAL sidecar means one was here.
            preflight::refuse_orphan_sidecars(&data_dir.sqlite_db_file_path())
                .map_err(StartupError::DbPreflight)?;
        }
        if !data_dir.exists() {
            data_dir
                .init()
                .map_err(|e| StartupError::DatadirCreation(data_dir.path().to_path_buf(), e))?;
            log::info!(
                "Created a new data directory at '{}'",
                data_dir.path().to_string_lossy()
            );
        }

        // Set up the connection to bitcoind (if using it) first as we may need it for the database
        // migration when setting up SQLite below.
        let mut bitcoind = if bitcoin.is_none() {
            if let Some(config::BitcoinBackend::Bitcoind(_)) = &config.bitcoin_backend {
                Some(setup_bitcoind(&config, &data_dir, fresh_data_dir)?)
            } else {
                None
            }
        } else {
            None
        };

        // Then set up the database backend.
        let db = match db {
            Some(db) => sync::Arc::from(sync::Mutex::from(db)),
            None => sync::Arc::from(sync::Mutex::from(setup_sqlite(
                &config,
                &data_dir,
                fresh_data_dir,
                &secp,
                &bitcoind,
            )?)) as sync::Arc<sync::Mutex<dyn DatabaseInterface>>,
        };

        // A wallet whose scan window starts after the coins we already know about
        // cannot track them, and nothing else notices — see `heal_scan_window`.
        if let Some(bitcoind) = bitcoind.as_mut() {
            heal_scan_window(bitcoind, &db, &config.main_descriptor);
        }

        // Shared abort flag: `stop` flips it so an in-flight Esplora scan stops
        // walking the provider chain and returns promptly, instead of the poller
        // (and the `stop` that joins it) blocking on dead/throttled providers.

        // Finally set up the Bitcoin backend.
        let bit = match (bitcoin, &config.bitcoin_backend) {
            (Some(bit), _) => sync::Arc::from(sync::Mutex::from(bit)),
            (None, Some(config::BitcoinBackend::Bitcoind(..))) => sync::Arc::from(
                sync::Mutex::from(bitcoind.expect("bitcoind must have been set already")),
            )
                as sync::Arc<sync::Mutex<dyn BitcoinInterface>>,
            (None, Some(config::BitcoinBackend::Electrum(..))) => {
                sync::Arc::from(sync::Mutex::from(setup_electrum(&config, db.clone())?))
            }
            (None, Some(config::BitcoinBackend::Esplora(..))) => {
                sync::Arc::from(sync::Mutex::from(setup_esplora(
                    &config,
                    db.clone(),
                    scan_abort.clone(),
                    prepared_client,
                )?))
            }
            (None, None) => Err(StartupError::MissingBitcoinBackendConfig)?,
        };

        // Shared, lock-free sync-progress mirror: the poller publishes into it,
        // `get_info` reads from it — so `get_info` (and the GUI's startup gate
        // that awaits it) never blocks behind the poller's full wallet scan.
        let sync_progress_cache = sync::Arc::new(crate::bitcoin::SyncProgressCache::default());
        let reorg_alert_cache = sync::Arc::new(crate::bitcoin::ReorgAlertCache::default());

        // Start the poller thread. Keep the thread handle to be able to check if it crashed. Store
        // an atomic to be able to stop it.
        let mut bitcoin_poller = poller::Poller::new(
            bit.clone(),
            db.clone(),
            config.main_descriptor.clone(),
            sync_progress_cache.clone(),
            reorg_alert_cache.clone(),
        );
        let (poller_sender, poller_receiver) = mpsc::sync_channel(1);
        let poller_handle = thread::Builder::new()
            .name("Bitcoin Network poller".to_string())
            .spawn({
                let poll_interval = config.bitcoin_config.poll_interval_secs;
                move || {
                    log::info!("Bitcoin poller started.");
                    bitcoin_poller.poll_forever(poll_interval, poller_receiver);
                    log::info!("Bitcoin poller stopped.");
                }
            })
            .expect("Spawning the poller thread must never fail.");

        // Create the API the external world will use to talk to us, either directly through the Rust
        // structure or through the JSONRPC server we may setup below.
        let control = DaemonControl::new(
            config,
            bit,
            poller_sender.clone(),
            db,
            secp,
            sync_progress_cache,
            reorg_alert_cache,
        );

        if with_rpc_server {
            let rpcserver_shutdown = sync::Arc::from(sync::atomic::AtomicBool::from(false));
            let rpcserver_handle = thread::Builder::new()
                .name("Bitcoin Network poller".to_string())
                .spawn({
                    let shutdown = rpcserver_shutdown.clone();
                    move || {
                        server::run(&data_dir.coincubed_rpc_socket_path(), control, shutdown)?;
                        Ok(())
                    }
                })
                .expect("Spawning the RPC server thread should never fail.");

            return Ok(DaemonHandle::Server {
                poller_sender,
                poller_handle,
                rpcserver_shutdown,
                rpcserver_handle,
                scan_abort,
            });
        }

        Ok(DaemonHandle::Controller {
            poller_sender,
            poller_handle,
            control,
            scan_abort,
        })
    }

    /// Start the Coincube daemon with the default Bitcoin and database interfaces (`bitcoind` RPC
    /// and SQLite).
    pub fn start_default(
        config: Config,
        with_rpc_server: bool,
    ) -> Result<DaemonHandle, StartupError> {
        Self::start(
            config,
            Option::<BitcoinD>::None,
            Option::<SqliteDb>::None,
            with_rpc_server,
        )
    }

    /// Check whether the daemon is still up and running. This needs to be regularly polled to
    /// check for internal errors. If this returns `false`, collect the error using the `stop`
    /// method.
    pub fn is_alive(&self) -> bool {
        match self {
            Self::Controller {
                ref poller_handle, ..
            } => !poller_handle.is_finished(),
            Self::Server {
                ref poller_handle,
                ref rpcserver_handle,
                ..
            } => !poller_handle.is_finished() && !rpcserver_handle.is_finished(),
        }
    }

    /// Cleanup for ephemeral Connect ownership, including failed startup/Drop.
    /// Closed channels and panicked workers become errors instead of unwinding.
    /// Always joins both workers even when one failed. Existing stop semantics
    /// remain unchanged for callers that do not opt into this cleanup path.
    pub fn stop_for_cleanup(self) -> io::Result<()> {
        fn failure() -> io::Error {
            io::Error::other("Daemon worker failed during cleanup")
        }
        match self {
            Self::Controller {
                poller_sender,
                poller_handle,
                scan_abort,
                ..
            } => {
                scan_abort.store(true, sync::atomic::Ordering::Relaxed);
                let disconnected = poller_sender.send(poller::PollerMessage::Shutdown).is_err();
                let panicked = poller_handle.join().is_err();
                if disconnected || panicked {
                    Err(failure())
                } else {
                    Ok(())
                }
            }
            Self::Server {
                poller_sender,
                poller_handle,
                rpcserver_shutdown,
                rpcserver_handle,
                scan_abort,
            } => {
                scan_abort.store(true, sync::atomic::Ordering::Relaxed);
                rpcserver_shutdown.store(true, sync::atomic::Ordering::Relaxed);
                let disconnected = poller_sender.send(poller::PollerMessage::Shutdown).is_err();
                let rpc_result = rpcserver_handle
                    .join()
                    .map_err(|_| failure())
                    .and_then(|result| result);
                let poller_result = poller_handle.join().map_err(|_| failure());
                rpc_result.and(poller_result).and(if disconnected {
                    Err(failure())
                } else {
                    Ok(())
                })
            }
        }
    }

    /// Stop the Coincube daemon. This returns any error which may have occurred.
    pub fn stop(self) -> Result<(), Box<dyn error::Error>> {
        match self {
            Self::Controller {
                poller_sender,
                poller_handle,
                scan_abort,
                ..
            } => {
                // Abort any in-flight Esplora scan FIRST, then signal shutdown,
                // so the poller can't be stuck mid-scan when we join it below.
                scan_abort.store(true, sync::atomic::Ordering::Relaxed);
                poller_sender
                    .send(poller::PollerMessage::Shutdown)
                    .expect("The other end should never have hung up before this.");
                poller_handle.join().expect("Poller thread must not panic");
                Ok(())
            }
            Self::Server {
                poller_sender,
                poller_handle,
                rpcserver_shutdown,
                rpcserver_handle,
                scan_abort,
            } => {
                scan_abort.store(true, sync::atomic::Ordering::Relaxed);
                poller_sender
                    .send(poller::PollerMessage::Shutdown)
                    .expect("The other end should never have hung up before this.");
                rpcserver_shutdown.store(true, sync::atomic::Ordering::Relaxed);
                rpcserver_handle
                    .join()
                    .expect("Poller thread must not panic")?;
                poller_handle.join().expect("Poller thread must not panic");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    #[test]
    fn cleanup_joins_failed_workers_without_unwinding() {
        for panic_worker in [false, true] {
            let (sender, receiver) = mpsc::sync_channel(1);
            drop(receiver);
            let poller = thread::spawn(move || {
                if panic_worker {
                    panic!("synthetic poller failure");
                }
            });
            let rpc = thread::spawn(|| -> io::Result<()> {
                Err(io::Error::other("synthetic RPC failure"))
            });
            let abort = sync::Arc::new(sync::atomic::AtomicBool::new(false));
            let shutdown = sync::Arc::new(sync::atomic::AtomicBool::new(false));
            let handle = DaemonHandle::Server {
                poller_sender: sender,
                poller_handle: poller,
                rpcserver_shutdown: shutdown.clone(),
                rpcserver_handle: rpc,
                scan_abort: abort.clone(),
            };
            assert!(handle.stop_for_cleanup().is_err());
            assert!(abort.load(sync::atomic::Ordering::Relaxed));
            assert!(shutdown.load(sync::atomic::Ordering::Relaxed));
        }
    }
    #[test]
    fn cleanup_delivers_shutdown_and_joins_healthy_workers() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let poller = thread::spawn(move || {
            assert!(matches!(
                receiver.recv(),
                Ok(poller::PollerMessage::Shutdown)
            ));
        });
        let rpc = thread::spawn(|| -> io::Result<()> { Ok(()) });
        let handle = DaemonHandle::Server {
            poller_sender: sender,
            poller_handle: poller,
            rpcserver_shutdown: sync::Arc::new(sync::atomic::AtomicBool::new(false)),
            rpcserver_handle: rpc,
            scan_abort: sync::Arc::new(sync::atomic::AtomicBool::new(false)),
        };
        assert!(handle.stop_for_cleanup().is_ok());
    }
}

#[cfg(test)]
mod scan_window_tests {
    use super::*;
    use crate::database::BlockInfo;

    /// The state that took the app down: a wallet imported at `timestamp: "now"`
    /// onto a restored database. The wallet was scanned from today, the coins
    /// are from weeks ago, and nothing else in startup notices — the wallet is
    /// loaded and the descriptors are present, which is all
    /// `wallet_sanity_checks` asks.
    #[test]
    fn a_restored_database_behind_a_freshly_scanned_wallet_is_flagged() {
        let oldest = BlockInfo {
            height: 145_604,
            time: 1_784_953_848,
        };
        assert_eq!(
            history_outside_scan_window(1_787_373_712, Some(oldest)),
            Some(oldest),
            "coins predating the scan window are unreachable to the wallet"
        );
    }

    /// A wallet scanned from before its oldest coin is correct, and must stay
    /// quiet — this runs on every startup.
    #[test]
    fn a_wallet_scanned_before_its_oldest_coin_is_quiet() {
        let oldest = BlockInfo {
            height: 145_604,
            time: 1_784_953_848,
        };
        assert_eq!(
            history_outside_scan_window(1_784_953_000, Some(oldest)),
            None
        );
    }

    /// Exactly at the oldest coin's block time means that block was scanned.
    /// An inclusive comparison here would warn on every correct restore.
    #[test]
    fn the_boundary_is_not_a_fault() {
        let oldest = BlockInfo {
            height: 145_604,
            time: 1_784_953_848,
        };
        assert_eq!(
            history_outside_scan_window(oldest.time, Some(oldest)),
            None,
            "a wallet scanned from the oldest coin's block time saw that block"
        );
    }

    /// The rescan the daemon performs must start from at or before the oldest
    /// coin — that is the whole point, and an off-by-one here would scan past
    /// the very block it exists to reach.
    #[test]
    fn the_heal_starts_at_or_before_the_oldest_coin() {
        let oldest = BlockInfo {
            height: 145_604,
            time: 1_784_953_848,
        };
        let gap = history_outside_scan_window(1_787_373_712, Some(oldest))
            .expect("a short window is a gap");
        assert!(
            gap.time <= oldest.time,
            "rescanning from {} would start after the coin at {}",
            gap.time,
            oldest.time
        );
        assert_eq!(gap, oldest);
    }

    fn outpoint(n: u8) -> miniscript::bitcoin::OutPoint {
        miniscript::bitcoin::OutPoint {
            txid: miniscript::bitcoin::Txid::from_raw_hash(
                miniscript::bitcoin::hashes::Hash::from_byte_array([n; 32]),
            ),
            vout: 0,
        }
    }

    /// An unanswerable probe must not be read as "the wallet is missing this".
    ///
    /// `knows_transaction` returns `Err` for anything that is not the wallet's
    /// own `-5`, and the caller substitutes `true` — "do not trigger on the
    /// probe" — leaving the descriptor-window test to decide alone. Reading the
    /// failure as `false` instead would rescan a healthy wallet whenever the RPC
    /// hiccupped, on every start, for a question never actually asked.
    #[test]
    fn an_unanswerable_probe_defers_instead_of_forcing_a_rescan() {
        let oldest = BlockInfo {
            height: 145_604,
            time: 1_784_953_848,
        };

        // Window fine, probe unanswerable: nothing is known to be wrong, so
        // nothing is repaired — and the next start asks again.
        assert_eq!(
            scan_repair_needed(oldest.time - 1, Some(oldest), true),
            None
        );

        // Window plainly short: still repaired, because that test stands on its
        // own and needed no probe.
        assert_eq!(
            scan_repair_needed(1_787_373_712, Some(oldest), true),
            Some(oldest),
            "a short window is conclusive without the probe"
        );
    }

    /// The probe must follow block **height**, not block time.
    ///
    /// Bitcoin timestamps only have to beat the median of the previous eleven
    /// blocks, so they tie and run backwards routinely. Here the coin with the
    /// latest *time* sits in an earlier block than the newest coin — exactly the
    /// shape a truncated scan produces, since it would already hold the earlier
    /// block and be missing the later one. Probing by time asks about the coin
    /// it has and concludes the wallet is fine.
    #[test]
    fn the_probe_follows_height_when_timestamps_run_backwards() {
        let early_block_late_stamp = BlockInfo {
            height: 145_600,
            time: 1_784_960_000,
        };
        let late_block_early_stamp = BlockInfo {
            height: 145_610,
            time: 1_784_953_848,
        };
        let confirmed = vec![
            (early_block_late_stamp, outpoint(1)),
            (late_block_early_stamp, outpoint(2)),
        ];

        let (start_from, probe) = scan_probe_points(&confirmed).expect("two confirmed coins");
        assert_eq!(
            probe,
            outpoint(2),
            "the coin in the highest block is the one a cut-short scan would be missing"
        );
        assert_eq!(
            start_from, late_block_early_stamp,
            "the start point is the earliest timestamp, which is the conservative floor"
        );
    }

    /// No confirmed coins, no probe points — the fresh-restore state.
    #[test]
    fn no_confirmed_coins_yields_no_probe_points() {
        assert!(scan_probe_points(&[]).is_none());
    }

    /// The case the descriptor timestamp cannot see: a scan cut short.
    ///
    /// `importdescriptors` stamps the descriptors when the import is requested,
    /// not when the scan finishes, and keeps the stamp through an aborting
    /// shutdown — the poller says as much in its own note. So a wallet
    /// interrupted mid-scan reports a window it never covered, and the
    /// timestamp test alone calls it healthy forever while its coins stay
    /// unresolvable.
    #[test]
    fn a_scan_cut_short_is_caught_even_though_the_window_looks_right() {
        let oldest = BlockInfo {
            height: 145_604,
            time: 1_784_953_848,
        };
        // Window looks perfect — scanned from before the oldest coin.
        let scanned_from = oldest.time - 1;

        assert_eq!(
            history_outside_scan_window(scanned_from, Some(oldest)),
            None,
            "the timestamp test alone sees nothing wrong"
        );
        assert_eq!(
            scan_repair_needed(scanned_from, Some(oldest), false),
            Some(oldest),
            "but the wallet not holding the newest coin's tx is proof it did not finish"
        );
        assert_eq!(
            scan_repair_needed(scanned_from, Some(oldest), true),
            None,
            "a wallet that both claims and holds the history is left alone"
        );
    }

    /// A repair always restarts from the oldest coin, never from where a
    /// cut-short scan happened to stop: bitcoind rescans forward from a
    /// timestamp, so resuming from the middle would leave the gap in place.
    #[test]
    fn a_repair_restarts_from_the_oldest_coin() {
        let oldest = BlockInfo {
            height: 145_604,
            time: 1_784_953_848,
        };
        for (scanned_from, known) in [(1_787_373_712, true), (oldest.time - 1, false)] {
            assert_eq!(
                scan_repair_needed(scanned_from, Some(oldest), known),
                Some(oldest)
            );
        }
    }

    /// An empty database yields no gap — and that is a **limit**, not just an
    /// optimisation.
    ///
    /// A freshly restored Vault starts with an empty database, so this path
    /// cannot tell how far back its history goes and correctly does nothing.
    /// Seeding that first scan is the Recovery Kit birthday's job, carried by
    /// `coincube_gui::app::start_pending_rescan`; the heal here is what keeps
    /// the result once the database has coins to reason from. Removing either
    /// half leaves restored wallets unable to see their own history.
    #[test]
    fn no_coins_means_no_gap_can_be_derived() {
        assert_eq!(history_outside_scan_window(u32::MAX, None), None);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        config::{BitcoinConfig, BitcoindConfig, BitcoindRpcAuth},
        testutils::*,
    };

    use coincube_core::descriptors::CoincubeDescriptor;

    use miniscript::bitcoin;
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        net, path,
        str::FromStr,
        thread, time,
    };

    // Read all bytes from the socket until the end of a JSON object, good enough approximation.
    fn read_til_json_end(stream: &mut net::TcpStream) {
        stream
            .set_read_timeout(Some(time::Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();

            if line.starts_with("Authorization") {
                let mut buf = vec![0; 256];
                reader.read_until(b'}', &mut buf).unwrap();
                return;
            }
        }
    }

    // Respond to the two "echo" sent at startup to sanity check the connection
    fn complete_sanity_check(server: &net::TcpListener) {
        let echo_resp =
            "HTTP/1.1 200\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":[]}\n".as_bytes();

        // Read the first echo, respond to it
        {
            let (mut stream, _) = server.accept().unwrap();
            read_til_json_end(&mut stream);
            stream.write_all(echo_resp).unwrap();
            stream.flush().unwrap();
        }

        // Read the second echo, respond to it
        let (mut stream, _) = server.accept().unwrap();
        read_til_json_end(&mut stream);
        stream.write_all(echo_resp).unwrap();
        stream.flush().unwrap();
    }

    // Send them a pruned getblockchaininfo telling them we are at version 24.0
    fn complete_version_check(server: &net::TcpListener) {
        let net_resp =
            "HTTP/1.1 200\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"version\":240000}}\n"
                .as_bytes();
        let (mut stream, _) = server.accept().unwrap();
        read_til_json_end(&mut stream);
        stream.write_all(net_resp).unwrap();
        stream.flush().unwrap();
    }

    // Send them a pruned getblockchaininfo telling them we are on mainnet
    fn complete_network_check(server: &net::TcpListener) {
        let net_resp =
            "HTTP/1.1 200\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"chain\":\"main\"}}\n"
                .as_bytes();
        let (mut stream, _) = server.accept().unwrap();
        read_til_json_end(&mut stream);
        stream.write_all(net_resp).unwrap();
        stream.flush().unwrap();
    }

    // Send them responses for the calls involved when creating a fresh wallet
    fn complete_wallet_creation(server: &net::TcpListener) {
        {
            let net_resp =
                ["HTTP/1.1 200\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":[]}\n".as_bytes()]
                    .concat();
            let (mut stream, _) = server.accept().unwrap();
            read_til_json_end(&mut stream);
            stream.write_all(&net_resp).unwrap();
            stream.flush().unwrap();
        }

        {
            let net_resp = [
            "HTTP/1.1 200\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"name\":\"dummy\"}}\n"
                .as_bytes(),
            ]
            .concat();
            let (mut stream, _) = server.accept().unwrap();
            read_til_json_end(&mut stream);
            stream.write_all(&net_resp).unwrap();
            stream.flush().unwrap();
        }

        let net_resp = [
            "HTTP/1.1 200\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":[{\"success\":true}]}\n"
                .as_bytes(),
        ]
        .concat();
        let (mut stream, _) = server.accept().unwrap();
        read_til_json_end(&mut stream);
        stream.write_all(&net_resp).unwrap();
        stream.flush().unwrap();
    }

    // Send them a dummy result to loadwallet.
    fn complete_wallet_loading(server: &net::TcpListener) {
        {
            let listwallets_resp =
                "HTTP/1.1 200\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":[]}\n".as_bytes();
            let (mut stream, _) = server.accept().unwrap();
            read_til_json_end(&mut stream);
            stream.write_all(listwallets_resp).unwrap();
            stream.flush().unwrap();
        }

        let loadwallet_resp =
            "HTTP/1.1 200\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"name\":\"dummy\"}}\n"
                .as_bytes();
        let (mut stream, _) = server.accept().unwrap();
        read_til_json_end(&mut stream);
        stream.write_all(loadwallet_resp).unwrap();
        stream.flush().unwrap();
    }

    // Send them a response to 'listwallets' with the watchonly wallet path
    fn complete_wallet_check(server: &net::TcpListener, watchonly_wallet_path: &str) {
        let net_resp = [
            "HTTP/1.1 200\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":[\"".as_bytes(),
            watchonly_wallet_path.as_bytes(),
            "\"]}\n".as_bytes(),
        ]
        .concat();
        let (mut stream, _) = server.accept().unwrap();
        read_til_json_end(&mut stream);
        stream.write_all(&net_resp).unwrap();
        stream.flush().unwrap();
    }

    // Send them a response to 'listdescriptors' with the receive and change descriptors
    fn complete_desc_check(server: &net::TcpListener, receive_desc: &str, change_desc: &str) {
        let net_resp = [
            "HTTP/1.1 200\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"descriptors\":[{\"desc\":\"".as_bytes(),
            receive_desc.as_bytes(),
            "\",\"timestamp\":0},".as_bytes(),
            "{\"desc\":\"".as_bytes(),
            change_desc.as_bytes(),
            "\",\"timestamp\":1}]}}\n".as_bytes(),
        ]
        .concat();
        let (mut stream, _) = server.accept().unwrap();
        read_til_json_end(&mut stream);
        stream.write_all(&net_resp).unwrap();
        stream.flush().unwrap();
    }

    // Send them a response to 'getblockhash' with the genesis block hash
    fn complete_tip_init(server: &net::TcpListener) {
        let net_resp = [
            "HTTP/1.1 200\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f\"}\n".as_bytes(),
        ]
        .concat();
        let (mut stream, _) = server.accept().unwrap();
        read_til_json_end(&mut stream);
        stream.write_all(&net_resp).unwrap();
        stream.flush().unwrap();
    }

    fn finish_daemon_shutdown(server: &net::TcpListener, daemon: thread::JoinHandle<()>) {
        let sync_resp =
            "HTTP/1.1 200\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"verificationprogress\":0.0,\"headers\":1,\"blocks\":0}}\n"
                .as_bytes();
        let deadline = time::Instant::now() + time::Duration::from_secs(10);

        server.set_nonblocking(true).unwrap();
        while !daemon.is_finished() {
            assert!(
                time::Instant::now() < deadline,
                "daemon shutdown stalled while draining the optional poller RPC",
            );
            match server.accept() {
                Ok((mut stream, _)) => {
                    // Same macOS inheritance as the synthetic Connect server
                    // below: the accepted socket must block, or read_line
                    // fails with WouldBlock before the RPC bytes arrive.
                    stream.set_nonblocking(false).unwrap();
                    read_til_json_end(&mut stream);
                    stream.write_all(sync_resp).unwrap();
                    stream.flush().unwrap();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(time::Duration::from_millis(10));
                }
                Err(error) => panic!("accepting optional poller RPC: {}", error),
            }
        }
        server.set_nonblocking(false).unwrap();
        daemon.join().unwrap();
    }

    // TODO: we could move the dummy bitcoind thread stuff to the bitcoind module to test the
    // bitcoind interface, and use the DummyCoincube from testutils to sanity check the startup.
    // Note that startup as checked by this unit test is also tested in the functional test
    // framework.
    #[test]
    fn daemon_startup() {
        // This exercises a startup path with a known thread race: the poller can
        // begin polling (making an unscripted RPC) before it observes the
        // `Shutdown` this test sends, which stalls startup. Run the body under a
        // watchdog so a stall FAILS the test promptly instead of hanging CI
        // forever (resolving the long-standing TODO this replaced). A panic
        // inside the body (e.g. the ~1-minute RPC-retry timeout) is surfaced too.
        let worker = thread::spawn(daemon_startup_inner);
        let deadline = time::Instant::now() + time::Duration::from_secs(120);
        while !worker.is_finished() {
            assert!(
                time::Instant::now() < deadline,
                "daemon_startup stalled for >120s (startup race lost) — failing instead of hanging",
            );
            thread::sleep(time::Duration::from_millis(50));
        }
        worker.join().expect("daemon_startup body panicked");
    }

    fn daemon_startup_inner() {
        let tmp_dir = tmp_dir();
        fs::create_dir_all(&tmp_dir).unwrap();
        let data_dir: path::PathBuf = [tmp_dir.as_path(), path::Path::new("datadir")]
            .iter()
            .collect();
        fs::create_dir_all(&data_dir).unwrap();
        let wo_path: path::PathBuf = [
            data_dir.as_path(),
            path::Path::new("bitcoin"),
            path::Path::new("coincubed_watchonly_wallet"),
        ]
        .iter()
        .collect();
        let wo_path_str = wo_path.to_str().unwrap().to_string();

        // Configure a dummy bitcoind
        let network = bitcoin::Network::Bitcoin;
        let cookie: path::PathBuf = [
            tmp_dir.as_path(),
            path::Path::new(&format!(
                "dummy_bitcoind_{:?}.cookie",
                thread::current().id()
            )),
        ]
        .iter()
        .collect();
        fs::write(&cookie, [0; 32]).unwrap(); // Will overwrite should it exist already
        let addr: net::SocketAddr =
            net::SocketAddrV4::new(net::Ipv4Addr::new(127, 0, 0, 1), 0).into();
        let server = net::TcpListener::bind(addr).unwrap();
        let addr = server.local_addr().unwrap();
        let bitcoin_config =
            BitcoinConfig::new(ChainId::from(network), time::Duration::from_secs(2));
        let bitcoind_config = BitcoindConfig {
            addr,
            rpc_auth: BitcoindRpcAuth::CookieFile(cookie),
        };

        // Create a dummy config with this bitcoind
        let desc_str = concat!(
            "wsh(andor(pk([aabbccdd]xpub68JJTXc1MWK8KLW4HGLXZBJknja7kDUJuFHnM424LbziEXsfkh1WQCiEjjHw4z",
            "LqSUm4rvhgyGkkuRowE9tCJSgt3TQB5J3SKAbZ2SdcKST/<0;1>/*),older(10000),pk([aabbccdd]xpub68JJT",
            "Xc1MWK8PEQozKsRatrUHXKFNkD1Cb1BuQU9Xr5moCv87anqGyXLyUd4KpnDyZgo3gz4aN1r3NiaoweFW8Uut",
            "BsBbgKHzaD5HkTkifK/<0;1>/*)))#3xh8xmhn"
        );
        let desc = CoincubeDescriptor::from_str(desc_str).unwrap();
        let receive_desc = desc.receive_descriptor().clone();
        let change_desc = desc.change_descriptor().clone();
        let mut data_directory = data_dir.clone();
        data_directory.push("bitcoin");
        let config = Config::new(
            bitcoin_config,
            Some(config::BitcoinBackend::Bitcoind(bitcoind_config)),
            log::LevelFilter::Debug,
            desc,
            DataDirectory::new(data_directory),
        );

        // Start the daemon in a new thread so the current one acts as the bitcoind server.
        let t = thread::spawn({
            let config = config.clone();
            move || {
                let handle = DaemonHandle::start_default(config, false).unwrap();
                handle.stop().unwrap();
            }
        });
        complete_sanity_check(&server);
        complete_version_check(&server);
        complete_network_check(&server);
        complete_wallet_creation(&server);
        complete_wallet_loading(&server);
        complete_wallet_check(&server, &wo_path_str);
        complete_desc_check(&server, &receive_desc.to_string(), &change_desc.to_string());
        complete_tip_init(&server);
        finish_daemon_shutdown(&server, t);

        // Real bitcoind creates the wallet directory after `createwallet`. The
        // scripted RPC server cannot do that for us, so mirror the side effect
        // before exercising the restart path.
        fs::create_dir_all(&wo_path).unwrap();

        // The wallet path exists now, so a restart only loads the wallet.
        let t = thread::spawn({
            let config = config.clone();
            move || {
                let handle = DaemonHandle::start_default(config, false).unwrap();
                handle.stop().unwrap();
            }
        });
        complete_sanity_check(&server);
        complete_version_check(&server);
        complete_network_check(&server);
        complete_wallet_loading(&server);
        complete_wallet_check(&server, &wo_path_str);
        complete_desc_check(&server, &receive_desc.to_string(), &change_desc.to_string());
        finish_daemon_shutdown(&server, t);

        fs::remove_dir_all(&tmp_dir).unwrap();
    }

    // ── Chain identity is settled before anything is touched (coincube-api#292) ─────────

    mod chain_binding {
        use super::*;
        use crate::database::sqlite::preflight::PreflightError;
        use miniscript::bitcoin::hashes::{sha256, Hash};

        const DESC_STR: &str = concat!(
            "wsh(andor(pk([aabbccdd]xpub68JJTXc1MWK8KLW4HGLXZBJknja7kDUJuFHnM424LbziEXsfkh1WQCiEjjHw4z",
            "LqSUm4rvhgyGkkuRowE9tCJSgt3TQB5J3SKAbZ2SdcKST/<0;1>/*),older(10000),pk([aabbccdd]xpub68JJT",
            "Xc1MWK8PEQozKsRatrUHXKFNkD1Cb1BuQU9Xr5moCv87anqGyXLyUd4KpnDyZgo3gz4aN1r3NiaoweFW8Uut",
            "BsBbgKHzaD5HkTkifK/<0;1>/*)))#3xh8xmhn"
        );

        /// A scripted-bitcoind listener that nobody has connected to yet, and a way to prove
        /// it stayed that way.
        struct SilentNode {
            server: net::TcpListener,
            addr: net::SocketAddr,
        }

        impl SilentNode {
            fn bind() -> Self {
                let addr: net::SocketAddr =
                    net::SocketAddrV4::new(net::Ipv4Addr::new(127, 0, 0, 1), 0).into();
                let server = net::TcpListener::bind(addr).unwrap();
                server.set_nonblocking(true).unwrap();
                let addr = server.local_addr().unwrap();
                SilentNode { server, addr }
            }

            /// No RPC reached this node: nothing ever connected.
            fn assert_untouched(&self) {
                match self.server.accept() {
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    other => panic!("the node received a connection: {:?}", other.map(|_| ())),
                }
            }
        }

        /// A bitcoind-backed daemon config for `chain` whose data directory is `data_directory`.
        fn config_for(
            chain: ChainId,
            tmp_dir: &path::Path,
            data_directory: path::PathBuf,
            node: &SilentNode,
        ) -> Config {
            let cookie = tmp_dir.join(format!(
                "dummy_bitcoind_{:?}.cookie",
                thread::current().id()
            ));
            fs::write(&cookie, [0; 32]).unwrap();
            Config::new(
                BitcoinConfig::new(chain, time::Duration::from_secs(2)),
                Some(config::BitcoinBackend::Bitcoind(BitcoindConfig {
                    addr: node.addr,
                    rpc_auth: BitcoindRpcAuth::CookieFile(cookie),
                })),
                log::LevelFilter::Debug,
                CoincubeDescriptor::from_str(DESC_STR).unwrap(),
                DataDirectory::new(data_directory),
            )
        }

        #[test]
        fn fork_persistence_drops_tokens_and_dormant_gate_stays_no_write() {
            let tmp =
                std::env::temp_dir().join(format!("connect-admission-{}", std::process::id()));
            fs::create_dir_all(&tmp).unwrap();
            let node = SilentNode::bind();
            let data = tmp.join("new-wallet");
            let mut config = config_for(ChainId::BitcoinBlake2b, &tmp, data.clone(), &node);
            config.bitcoin_backend = Some(config::BitcoinBackend::Esplora(config::EsploraConfig {
                addr: "https://fixture.invalid".into(),
                token: Some("synthetic-jwt".into()),
                fallback_addr: None,
                fallback_token: Some("synthetic-fallback".into()),
                secondary_fallback_addr: None,
                secondary_fallback_token: Some("synthetic-second".into()),
            }));
            let encoded = toml::to_string(&config.for_persistence()).unwrap();
            assert!(!encoded.contains("synthetic-"));
            assert!(encoded.contains("fixture.invalid"));
            assert!(matches!(
                DaemonHandle::start_default(config.clone(), false),
                Err(StartupError::ChainDormant(_))
            ));
            assert!(!data.exists());
            node.assert_untouched();
            config.bitcoin_config =
                BitcoinConfig::new(ChainId::Bitcoin, time::Duration::from_secs(2));
            assert!(toml::to_string(&config.for_persistence())
                .unwrap()
                .contains("synthetic-jwt"));
            fs::remove_dir_all(tmp).unwrap();
        }

        #[test]
        fn failed_admission_is_typed_and_precedes_real_startup_writes() {
            struct Authority(connect::TrustedChainAnchor);
            impl connect::ConnectAnchorAuthority for Authority {
                fn fresh_anchor(
                    &self,
                ) -> Result<connect::TrustedChainAnchor, connect::AdmissionError> {
                    Ok(self.0.clone())
                }
            }
            let tmp = std::env::temp_dir().join(format!("connect-ordering-{}", std::process::id()));
            fs::create_dir_all(&tmp).unwrap();
            let node = SilentNode::bind();
            let data = tmp.join("must-not-exist");
            let mut config = config_for(ChainId::BitcoinBlake2b, &tmp, data.clone(), &node);
            let endpoint = format!("http://{}", node.addr);
            config.bitcoin_backend = Some(config::BitcoinBackend::Esplora(config::EsploraConfig {
                addr: endpoint.clone(),
                token: None,
                fallback_addr: None,
                fallback_token: None,
                secondary_fallback_addr: None,
                secondary_fallback_token: None,
            }));
            // Only the private test invocation bypasses the unchanged production
            // dormant policy; every real admission and startup statement runs.
            let result = DaemonHandle::start_inner(
                config.clone(),
                Option::<BitcoinD>::None,
                Option::<SqliteDb>::None,
                false,
                None,
                |_| Ok(()),
            );
            assert!(matches!(
                result,
                Err(StartupError::ConnectAdmission(
                    connect::AdmissionError::MissingAuth
                ))
            ));
            assert!(!data.exists());
            for (chain, observed_at, expected) in [
                (
                    ChainId::Bitcoin,
                    time::SystemTime::now(),
                    connect::AdmissionError::WrongChain,
                ),
                (
                    ChainId::BitcoinBlake2b,
                    time::SystemTime::now() - time::Duration::from_secs(120),
                    connect::AdmissionError::Stale,
                ),
            ] {
                let authority = sync::Arc::new(Authority(connect::TrustedChainAnchor {
                    chain,
                    height: 900000,
                    hash: BlockHash::from_byte_array([7; 32]),
                    median_time_past: 1000,
                    observed_at,
                }));
                let backend = connect::ConnectBackend::new(
                    ChainId::BitcoinBlake2b,
                    endpoint.clone(),
                    "synthetic-jwt".into(),
                    authority,
                )
                .unwrap();
                let result = DaemonHandle::start_with_connect(config.clone(), backend, false);
                assert!(
                    matches!(result,Err(StartupError::ConnectAdmission(actual)) if actual==expected)
                );
                assert!(!data.exists());
            }
            node.assert_untouched();
            for error in [
                connect::AdmissionError::HashMismatch,
                connect::AdmissionError::IndexerBehind,
                connect::AdmissionError::Unavailable,
            ] {
                assert!(
                    matches!(connect_startup_error(crate::bitcoin::esplora::client::Error::Admission(error)),StartupError::ConnectAdmission(actual) if actual==error)
                );
            }
            fs::remove_dir_all(tmp).unwrap();
        }

        #[test]
        fn authenticated_fork_start_rejects_unsupported_capabilities_before_io() {
            struct UnusedAuthority;
            impl connect::ConnectAnchorAuthority for UnusedAuthority {
                fn fresh_anchor(
                    &self,
                ) -> Result<connect::TrustedChainAnchor, connect::AdmissionError> {
                    panic!("capability refusal must precede authority I/O");
                }
            }
            let tmp =
                std::env::temp_dir().join(format!("connect-capabilities-{}", std::process::id()));
            fs::create_dir_all(&tmp).unwrap();
            let node = SilentNode::bind();
            let data = tmp.join("must-not-exist");
            let endpoint = format!("http://{}", node.addr);
            let mut original = config_for(ChainId::BitcoinBlake2b, &tmp, data.clone(), &node);
            let local_node = original.bitcoin_backend.clone();
            original.bitcoin_backend =
                Some(config::BitcoinBackend::Esplora(config::EsploraConfig {
                    addr: endpoint.clone(),
                    token: None,
                    fallback_addr: None,
                    fallback_token: None,
                    secondary_fallback_addr: None,
                    secondary_fallback_token: None,
                }));
            for case in ["rpc", "local_node", "taproot", "bitcoin", "fallback"] {
                let mut config = original.clone();
                match case {
                    "local_node" => {
                        if let Some(config::BitcoinBackend::Bitcoind(node)) = local_node.clone() {
                            config.pending_bitcoind = Some(node);
                        }
                    }
                    "taproot" => config.main_descriptor = CoincubeDescriptor::from_str(
                        "tr([abcdef01]xpub6Eze7yAT3Y1wGrnzedCNVYDXUqa9NmHVWck5emBaTbXtURbe1NWZbK9bsz1TiVE7Cz341PMTfYgFw1KdLWdzcM1UMFTcdQfCYhhXZ2HJvTW/<0;1>/*,and_v(v:pk([abcdef01]xpub688Hn4wScQAAiYJLPg9yH27hUpfZAUnmJejRQBCiwfP5PEDzjWMNW1wChcninxr5gyavFqbbDjdV1aK5USJz8NDVjUy7FRQaaqqXHh5SbXe/<0;1>/*),older(52560)))#0mt7e93c"
                    ).unwrap(),
                    "bitcoin" => config.bitcoin_config = BitcoinConfig::new(ChainId::Bitcoin, time::Duration::from_secs(2)),
                    "fallback" => {
                        if let Some(config::BitcoinBackend::Esplora(ref mut selection)) = config.bitcoin_backend {
                            selection.fallback_addr = Some("http://bitcoin.invalid".into());
                        }
                    }
                    _ => {}
                }
                let backend = connect::ConnectBackend::new(
                    ChainId::BitcoinBlake2b,
                    endpoint.clone(),
                    "synthetic-jwt".into(),
                    sync::Arc::new(UnusedAuthority),
                )
                .unwrap();
                assert!(
                    matches!(
                        DaemonHandle::start_with_connect(config, backend, case == "rpc"),
                        Err(StartupError::ConnectAdmission(
                            connect::AdmissionError::InvalidBackend
                        ))
                    ),
                    "{}",
                    case
                );
                assert!(!data.exists(), "{}", case);
            }
            node.assert_untouched();
            fs::remove_dir_all(tmp).unwrap();
        }

        #[test]
        fn authenticated_fork_creates_and_reopens_a_synthetic_wallet() {
            use std::io::{Read, Write};
            use std::sync::atomic::{AtomicBool, Ordering};
            struct Authority(connect::TrustedChainAnchor);
            impl connect::ConnectAnchorAuthority for Authority {
                fn fresh_anchor(
                    &self,
                ) -> Result<connect::TrustedChainAnchor, connect::AdmissionError> {
                    Ok(self.0.clone())
                }
            }
            let tmp = std::env::temp_dir().join(format!("connect-open-{}", std::process::id()));
            fs::create_dir_all(&tmp).unwrap();
            let node = SilentNode::bind();
            let data = tmp.join("synthetic-wallet");
            let mut config = config_for(ChainId::BitcoinBlake2b, &tmp, data.clone(), &node);
            let listener = net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let anchor_hash = BlockHash::from_byte_array([7; 32]);
            let genesis = BlockHash::from_byte_array(
                *ChainHash::using_genesis_block(bitcoin::Network::Bitcoin).as_bytes(),
            );
            let stop = sync::Arc::new(AtomicBool::new(false));
            let requests = sync::Arc::new(sync::Mutex::new(Vec::new()));
            let serving_stop = stop.clone();
            let serving_requests = requests.clone();
            let server = thread::spawn(move || {
                while !serving_stop.load(Ordering::Relaxed) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(peer) => peer,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(time::Duration::from_millis(2));
                            continue;
                        }
                        Err(e) => panic!("synthetic accept: {}", e),
                    };
                    // macOS hands accept(2) callers a socket that inherits the
                    // listener's O_NONBLOCK (Linux's accept4 does not). Left
                    // that way, the first read returns WouldBlock before the
                    // client's request bytes land and the connection is
                    // dropped unanswered; the client then sees a reset. Wait
                    // for the request like a server: blocking reads sliced by
                    // the read timeout, bounded only by the deadline below.
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(time::Duration::from_millis(100)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(time::Duration::from_secs(1)))
                        .unwrap();
                    let deadline = time::Instant::now() + time::Duration::from_secs(1);
                    let mut head = Vec::new();
                    while head.len() < 8192
                        && time::Instant::now() < deadline
                        && !head.windows(4).any(|x| x == b"\r\n\r\n")
                    {
                        let mut chunk = [0; 1024];
                        match stream.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => head.extend_from_slice(&chunk[..n]),
                            // A read slice elapsed with nothing new: keep
                            // waiting until the deadline, do not give up.
                            Err(e)
                                if e.kind() == io::ErrorKind::WouldBlock
                                    || e.kind() == io::ErrorKind::TimedOut => {}
                            Err(_) => break,
                        }
                    }
                    let head = String::from_utf8_lossy(&head);
                    if !head.ends_with("\r\n\r\n") {
                        continue;
                    }
                    assert!(head
                        .to_ascii_lowercase()
                        .contains("authorization: bearer synthetic-jwt"));
                    let path = head.split_whitespace().nth(1).unwrap().to_string();
                    serving_requests.lock().unwrap().push(path.clone());
                    let (status, body) = match path.as_str() {
                        "/block-height/0" => (200, genesis.to_string()),
                        "/block-height/900000" | "/blocks/tip/hash" => {
                            (200, anchor_hash.to_string())
                        }
                        path if path.ends_with("/status") => (
                            200,
                            "{\"in_best_chain\":true,\"height\":900000,\"next_best\":null}".into(),
                        ),
                        _ => (404, String::new()),
                    };
                    let response = format!(
                        "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        status,
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
            });
            config.bitcoin_backend = Some(config::BitcoinBackend::Esplora(config::EsploraConfig {
                addr: endpoint.clone(),
                token: None,
                fallback_addr: None,
                fallback_token: None,
                secondary_fallback_addr: None,
                secondary_fallback_token: None,
            }));
            // Stop the synthetic HTTP thread even when startup fails; never leave
            // a failed fixture serving in the background.
            let result = (|| -> Result<(), StartupError> {
                for _ in 0..2 {
                    let backend = connect::ConnectBackend::new(
                        ChainId::BitcoinBlake2b,
                        endpoint.clone(),
                        "synthetic-jwt".into(),
                        sync::Arc::new(Authority(connect::TrustedChainAnchor {
                            chain: ChainId::BitcoinBlake2b,
                            height: 900000,
                            hash: anchor_hash,
                            median_time_past: 1_700_000_000,
                            observed_at: time::SystemTime::now(),
                        })),
                    )
                    .unwrap();
                    let daemon = DaemonHandle::start_with_connect(config.clone(), backend, false)?;
                    daemon.stop_for_cleanup().unwrap();
                    let stored =
                        preflight::read_stored_identity(&data.join("coincubed.sqlite3")).unwrap();
                    assert_eq!(stored.chain, ChainId::BitcoinBlake2b);
                    assert_eq!(stored.network, bitcoin::Network::Bitcoin);
                }
                Ok(())
            })();
            stop.store(true, Ordering::Relaxed);
            server.join().unwrap();
            result.unwrap();
            assert!(
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|p| *p == "/block-height/900000")
                    .count()
                    >= 2
            );
            node.assert_untouched();
            fs::remove_dir_all(tmp).unwrap();
        }

        /// A genuine version-8 database for `chain` in a fresh data directory.
        fn v8_database(data_directory: &path::Path, chain: ChainId) -> path::PathBuf {
            fs::create_dir_all(data_directory).unwrap();
            let db_path = data_directory.join("coincubed.sqlite3");
            let secp = secp256k1::Secp256k1::verification_only();
            let options = FreshDbOptions::legacy(
                chain,
                CoincubeDescriptor::from_str(DESC_STR).unwrap(),
                V8_SCHEMA,
                8,
            );
            SqliteDb::new(db_path.clone(), Some(options), &secp).unwrap();
            db_path
        }

        fn sha256_of(path: &path::Path) -> sha256::Hash {
            sha256::Hash::hash(&fs::read(path).unwrap())
        }

        fn listing(dir: &path::Path) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }

        #[test]
        fn an_inconsistent_config_is_refused_before_any_filesystem_access() {
            let tmp_dir = tmp_dir();
            fs::create_dir_all(&tmp_dir).unwrap();
            let node = SilentNode::bind();
            let data_directory = tmp_dir.join("never-created").join("bitcoin");
            let mut config = config_for(ChainId::Bitcoin, &tmp_dir, data_directory.clone(), &node);
            // Hand-built and never through `Config::check`: identity says Bitcoin, encoding
            // says signet.
            config.bitcoin_config.network = bitcoin::Network::Signet;

            match DaemonHandle::start_default(config, false) {
                Err(StartupError::Config(ConfigError::Unexpected(msg))) => {
                    assert!(msg.contains("signet"), "{}", msg)
                }
                other => panic!("expected a config refusal, got {:?}", other.map(|_| ())),
            }
            assert!(!data_directory.exists());
            assert!(!data_directory.parent().unwrap().exists());
            node.assert_untouched();
            fs::remove_dir_all(tmp_dir).unwrap();
        }

        #[test]
        fn a_dormant_chain_is_refused_before_any_filesystem_access() {
            for chain in [ChainId::BitcoinBlake2b, ChainId::BitcoinBlake2bTestnet4] {
                let tmp_dir = tmp_dir();
                fs::create_dir_all(&tmp_dir).unwrap();
                let node = SilentNode::bind();
                let data_directory = tmp_dir.join("never-created").join(chain.dir_name());
                let config = config_for(chain, &tmp_dir, data_directory.clone(), &node);
                config.bitcoin_config.check_chain_encoding().unwrap();

                match DaemonHandle::start_default(config, false) {
                    Err(StartupError::ChainDormant(c)) => assert_eq!(c, chain),
                    other => panic!(
                        "{:?}: expected ChainDormant, got {:?}",
                        chain,
                        other.map(|_| ())
                    ),
                }
                assert!(!data_directory.exists(), "{:?}", chain);
                assert!(!data_directory.parent().unwrap().exists(), "{:?}", chain);
                node.assert_untouched();
                fs::remove_dir_all(tmp_dir).unwrap();
            }
        }

        #[test]
        fn a_wrong_chain_database_is_refused_before_rpc_wallet_or_migration() {
            // A testnet4 database sitting in the directory a signet config points at.
            let tmp_dir = tmp_dir();
            fs::create_dir_all(&tmp_dir).unwrap();
            let node = SilentNode::bind();
            let data_directory = tmp_dir.join("signet");
            let db_path = v8_database(&data_directory, ChainId::Testnet4);
            let before = (sha256_of(&db_path), listing(&data_directory));
            let config = config_for(ChainId::Signet, &tmp_dir, data_directory.clone(), &node);

            match DaemonHandle::start_default(config, false) {
                Err(StartupError::ChainMismatch { config, stored }) => {
                    assert_eq!((config, stored), (ChainId::Signet, ChainId::Testnet4))
                }
                other => panic!("expected ChainMismatch, got {:?}", other.map(|_| ())),
            }
            // Same bytes, no journal, no watch-only wallet, still version 8, no RPC.
            assert_eq!((sha256_of(&db_path), listing(&data_directory)), before);
            assert_eq!(listing(&data_directory), vec!["coincubed.sqlite3"]);
            assert!(!data_directory.join("coincubed_watchonly_wallet").exists());
            let version: i64 = rusqlite::Connection::open(&db_path)
                .unwrap()
                .query_row("SELECT version FROM version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, 8);
            node.assert_untouched();
            fs::remove_dir_all(tmp_dir).unwrap();
        }

        #[test]
        fn a_bitcoin_config_over_a_fork_database_is_refused_the_same_way() {
            // The encoding twin: a mainnet config must not adopt a Bitcoin Blake2b database.
            let tmp_dir = tmp_dir();
            fs::create_dir_all(&tmp_dir).unwrap();
            let node = SilentNode::bind();
            let data_directory = tmp_dir.join("bitcoin");
            fs::create_dir_all(&data_directory).unwrap();
            let db_path = data_directory.join("coincubed.sqlite3");
            let secp = secp256k1::Secp256k1::verification_only();
            let options = FreshDbOptions::new(
                ChainId::Bitcoin,
                CoincubeDescriptor::from_str(DESC_STR).unwrap(),
            );
            SqliteDb::new(db_path.clone(), Some(options), &secp).unwrap();
            // Written by the test: the daemon cannot create one while the chain is dormant.
            rusqlite::Connection::open(&db_path)
                .unwrap()
                .execute("UPDATE tip SET chain = 'bitcoin-blake2b'", [])
                .unwrap();
            let before = sha256_of(&db_path);
            let config = config_for(ChainId::Bitcoin, &tmp_dir, data_directory.clone(), &node);

            match DaemonHandle::start_default(config, false) {
                Err(StartupError::ChainMismatch { config, stored }) => {
                    assert_eq!(
                        (config, stored),
                        (ChainId::Bitcoin, ChainId::BitcoinBlake2b)
                    )
                }
                other => panic!("expected ChainMismatch, got {:?}", other.map(|_| ())),
            }
            assert_eq!(sha256_of(&db_path), before);
            node.assert_untouched();
            fs::remove_dir_all(tmp_dir).unwrap();
        }

        #[test]
        fn an_unidentifiable_database_is_refused_before_rpc_wallet_or_migration() {
            let tmp_dir = tmp_dir();
            fs::create_dir_all(&tmp_dir).unwrap();
            let node = SilentNode::bind();

            // A database from a future build.
            let data_directory = tmp_dir.join("future");
            let db_path = v8_database(&data_directory, ChainId::Bitcoin);
            rusqlite::Connection::open(&db_path)
                .unwrap()
                .execute("UPDATE version SET version = 10", [])
                .unwrap();
            let before = (sha256_of(&db_path), listing(&data_directory));
            let config = config_for(ChainId::Bitcoin, &tmp_dir, data_directory.clone(), &node);
            match DaemonHandle::start_default(config, false) {
                Err(StartupError::DbPreflight(PreflightError::UnsupportedVersion(10))) => {}
                other => panic!("expected UnsupportedVersion, got {:?}", other.map(|_| ())),
            }
            assert_eq!((sha256_of(&db_path), listing(&data_directory)), before);
            node.assert_untouched();

            // A crashed writer's hot journal: refused, not repaired, and a second start runs
            // into exactly the same refusal.
            let data_directory = tmp_dir.join("crashed");
            let db_path = v8_database(&data_directory, ChainId::Bitcoin);
            let mut writer = rusqlite::Connection::open(&db_path).unwrap();
            writer.pragma_update(None, "synchronous", "OFF").unwrap();
            let tx = writer
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            tx.execute("UPDATE tip SET blockheight = 7", []).unwrap();
            let staged = tmp_dir.join("crashed-copy");
            fs::create_dir_all(&staged).unwrap();
            fs::copy(&db_path, staged.join("coincubed.sqlite3")).unwrap();
            fs::copy(
                data_directory.join("coincubed.sqlite3-journal"),
                staged.join("coincubed.sqlite3-journal"),
            )
            .unwrap();
            tx.rollback().unwrap();
            drop(writer);
            let before = (
                sha256_of(&staged.join("coincubed.sqlite3")),
                sha256_of(&staged.join("coincubed.sqlite3-journal")),
                listing(&staged),
            );
            for _ in 0..2 {
                let config = config_for(ChainId::Bitcoin, &tmp_dir, staged.clone(), &node);
                match DaemonHandle::start_default(config, false) {
                    Err(StartupError::DbPreflight(PreflightError::RecoveryRequired(_))) => {}
                    other => panic!("expected RecoveryRequired, got {:?}", other.map(|_| ())),
                }
                assert_eq!(
                    (
                        sha256_of(&staged.join("coincubed.sqlite3")),
                        sha256_of(&staged.join("coincubed.sqlite3-journal")),
                        listing(&staged),
                    ),
                    before
                );
            }
            node.assert_untouched();
            fs::remove_dir_all(tmp_dir).unwrap();
        }

        #[test]
        fn a_stray_sidecar_without_a_database_is_refused_before_rpc_wallet_or_creation() {
            // The directory exists but the database does not: without this check the start
            // would be "fresh" — create a database, create the watch-only wallet — next to
            // the remains of a database that is gone.
            for sidecar in [
                "coincubed.sqlite3-journal",
                "coincubed.sqlite3-wal",
                "coincubed.sqlite3-shm",
            ] {
                let tmp_dir = tmp_dir();
                fs::create_dir_all(&tmp_dir).unwrap();
                let node = SilentNode::bind();
                let data_directory = tmp_dir.join("bitcoin");
                fs::create_dir_all(&data_directory).unwrap();
                let stray = data_directory.join(sidecar);
                fs::write(&stray, b"remains").unwrap();
                let before = listing(&data_directory);
                let config = config_for(ChainId::Bitcoin, &tmp_dir, data_directory.clone(), &node);

                match DaemonHandle::start_default(config, false) {
                    Err(StartupError::DbPreflight(PreflightError::OrphanSidecar(p))) => {
                        assert_eq!(p, stray, "{}", sidecar)
                    }
                    other => panic!(
                        "{}: expected OrphanSidecar, got {:?}",
                        sidecar,
                        other.map(|_| ())
                    ),
                }
                assert_eq!(fs::read(&stray).unwrap(), b"remains");
                assert_eq!(listing(&data_directory), before, "{}", sidecar);
                assert!(!data_directory.join("coincubed.sqlite3").exists());
                assert!(!data_directory.join("coincubed_watchonly_wallet").exists());
                node.assert_untouched();
                fs::remove_dir_all(tmp_dir).unwrap();
            }
        }

        /// The compatibility path: an existing Bitcoin-family v8 database starts, is migrated
        /// to v9 during startup, and the daemon runs against it exactly as it did before.
        #[test]
        fn an_existing_v8_bitcoin_database_starts_and_is_migrated() {
            let worker = thread::spawn(v8_bitcoin_database_starts_inner);
            let deadline = time::Instant::now() + time::Duration::from_secs(120);
            while !worker.is_finished() {
                assert!(time::Instant::now() < deadline, "startup stalled");
                thread::sleep(time::Duration::from_millis(50));
            }
            worker
                .join()
                .expect("startup with an existing v8 database panicked");
        }

        fn v8_bitcoin_database_starts_inner() {
            let tmp_dir = tmp_dir();
            fs::create_dir_all(&tmp_dir).unwrap();
            let data_directory = tmp_dir.join("bitcoin");
            let db_path = v8_database(&data_directory, ChainId::Bitcoin);
            let wo_path = data_directory.join("coincubed_watchonly_wallet");
            let wo_path_str = wo_path.to_str().unwrap().to_string();

            let cookie = tmp_dir.join(format!(
                "dummy_bitcoind_{:?}.cookie",
                thread::current().id()
            ));
            fs::write(&cookie, [0; 32]).unwrap();
            let addr: net::SocketAddr =
                net::SocketAddrV4::new(net::Ipv4Addr::new(127, 0, 0, 1), 0).into();
            let server = net::TcpListener::bind(addr).unwrap();
            let addr = server.local_addr().unwrap();
            let desc = CoincubeDescriptor::from_str(DESC_STR).unwrap();
            let receive_desc = desc.receive_descriptor().clone();
            let change_desc = desc.change_descriptor().clone();
            let config = Config::new(
                BitcoinConfig::new(ChainId::Bitcoin, time::Duration::from_secs(2)),
                Some(config::BitcoinBackend::Bitcoind(BitcoindConfig {
                    addr,
                    rpc_auth: BitcoindRpcAuth::CookieFile(cookie),
                })),
                log::LevelFilter::Debug,
                desc,
                DataDirectory::new(data_directory.clone()),
            );

            // Same scripted exchange as a first start: the wallet does not exist yet, the
            // database does (at v8).
            let t = thread::spawn({
                let config = config.clone();
                move || {
                    let handle = DaemonHandle::start_default(config, false).unwrap();
                    handle.stop().unwrap();
                }
            });
            complete_sanity_check(&server);
            complete_version_check(&server);
            complete_network_check(&server);
            complete_wallet_creation(&server);
            complete_wallet_loading(&server);
            complete_wallet_check(&server, &wo_path_str);
            complete_desc_check(&server, &receive_desc.to_string(), &change_desc.to_string());
            complete_tip_init(&server);
            finish_daemon_shutdown(&server, t);

            // Migrated in place: v9, identity backfilled, encoding kept.
            let stored =
                crate::database::sqlite::preflight::read_stored_identity(&db_path).unwrap();
            assert_eq!(
                stored,
                StoredIdentity {
                    version: 9,
                    chain: ChainId::Bitcoin,
                    network: bitcoin::Network::Bitcoin,
                }
            );
            fs::remove_dir_all(&tmp_dir).unwrap();
        }
    }
}
