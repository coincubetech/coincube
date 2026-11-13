use crate::app::settings::WalletId;
use crate::chain::ChainId;
use coincubed::datadir::DataDirectory;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::{fs, io};

use fs4::fs_std::FileExt;

/// A linear, root-bound capability. Only the exact successful final-root
/// `create_dir` below can construct it; the settings transaction consumes it.
pub(crate) struct CreatedRoot(CoincubeDirectory);

impl CreatedRoot {
    pub(crate) fn root(&self) -> &CoincubeDirectory {
        &self.0
    }
}

fn unsafe_bootstrap(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

/// Bootstrap locks are never unlinked. Validate the endpoint before and after
/// opening so a stale or redirected lock cannot grant proof.
fn bootstrap_lock(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            fs::OpenOptions::new().read(true).write(true).open(path)?
        }
        Err(error) => return Err(error),
    };
    let endpoint = fs::symlink_metadata(path)?;
    let opened = file.metadata()?;
    if !endpoint.is_file() || !opened.is_file() || endpoint.file_type().is_symlink() {
        return Err(unsafe_bootstrap("bootstrap lock is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (endpoint.dev(), endpoint.ino()) != (opened.dev(), opened.ino())
            || endpoint.uid() != unsafe { libc::geteuid() }
            || endpoint.mode() & 0o077 != 0
        {
            return Err(unsafe_bootstrap(
                "bootstrap lock ownership or identity is unsafe",
            ));
        }
    }
    Ok(file)
}

fn valid_root(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(unsafe_bootstrap("data root is not a directory"));
    }
    Ok(())
}

fn create_exact_root(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    fs::create_dir(path)
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoincubeDirectory(PathBuf);

/// The process-wide active data directory, resolved once at startup from
/// `--datadir` (or the OS default). Set in `GUI::new`; read by code that runs
/// outside the datadir-carrying structs (e.g. the account-level Connect panel's
/// duress-fingerprint helper) so it uses the REAL data path, not the default.
static ACTIVE_DATADIR: OnceLock<CoincubeDirectory> = OnceLock::new();

impl CoincubeDirectory {
    pub fn new(p: PathBuf) -> Self {
        CoincubeDirectory(p)
    }
    pub fn new_default() -> Result<Self, Box<dyn std::error::Error>> {
        default_datadir().map(CoincubeDirectory::new)
    }

    /// Run before logger, GUI, or any Cube publication. Every updated startup
    /// enters B, including existing roots. Only a successful exact final-root
    /// mkdir can authorize an initial shown choice. A settings failure leaves
    /// the root in place; GUI's strict load then presents a masked retry state.
    pub fn bootstrap_privacy(
        &self,
    ) -> io::Result<
        Result<
            crate::app::settings::global::PreferencesReceipt,
            crate::app::settings::global::PrivacyInitError,
        >,
    > {
        use crate::app::settings::global::GlobalSettings;

        // Existing roots may be passed as `.` or `..`, neither of which has a
        // `file_name`. Resolve them before deriving the lock locations.
        // Keep absent roots lexical so only the exact final-root mkdir below
        // can grant the first-install creator proof.
        let root_path = match fs::symlink_metadata(self.path()) {
            Ok(_) => {
                valid_root(self.path())?;
                fs::canonicalize(self.path())?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => self.path().to_path_buf(),
            Err(error) => return Err(error),
        };
        let parent = root_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let name = root_path
            .file_name()
            .ok_or_else(|| unsafe_bootstrap("data root has no final component"))?;
        create_directory(parent).map_err(|error| io::Error::other(error.to_string()))?;
        let parent = fs::canonicalize(parent)?;
        let root = parent.join(name);
        let parent_lock = parent.join(".coincube-bootstrap.lock");
        // A new root needs the parent lock before its final mkdir. Once that
        // lock exists, every startup for this root joins it, so no existing-
        // root startup can initialize hidden between mkdir and the creator's
        // settings transaction. An older install may have no parent lock and
        // its parent may be read-only; lock inside its writable root instead.
        let lock = match fs::symlink_metadata(&parent_lock) {
            Ok(_) => bootstrap_lock(&parent_lock)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::symlink_metadata(&root) {
                    Ok(_) => {
                        valid_root(&root)?;
                        bootstrap_lock(&root.join(".coincube-bootstrap.lock"))?
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        bootstrap_lock(&parent_lock)?
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        FileExt::lock_exclusive(&lock)?;
        let result = match fs::symlink_metadata(&root) {
            Ok(_) => {
                valid_root(&root)?;
                GlobalSettings::load_or_initialize_preferences(&CoincubeDirectory::new(root))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match create_exact_root(&root) {
                    Ok(()) => {
                        valid_root(&root)?;
                        #[cfg(unix)]
                        {
                            // If this supported parent sync fails, do not
                            // commit an initial shown choice. The root remains
                            // existing/unproven and will initialize hidden on
                            // retry. This is not a cross-filesystem guarantee.
                            fs::File::open(&parent)?.sync_all()?;
                        }
                        GlobalSettings::initialize_created_preferences(CreatedRoot(
                            CoincubeDirectory::new(root),
                        ))
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        valid_root(&root)?;
                        GlobalSettings::load_or_initialize_preferences(&CoincubeDirectory::new(
                            root,
                        ))
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        FileExt::unlock(&lock)?;
        Ok(result)
    }

    /// Records the active data directory for the process (idempotent — only the
    /// first call wins, which is the startup resolution).
    pub fn set_active(dir: CoincubeDirectory) {
        let _ = ACTIVE_DATADIR.set(dir);
    }

    /// The active data directory, honouring a custom `--datadir`. Falls back to
    /// the OS default if it was never recorded.
    pub fn active() -> Result<CoincubeDirectory, Box<dyn std::error::Error>> {
        match ACTIVE_DATADIR.get() {
            Some(dir) => Ok(dir.clone()),
            None => Self::unrecorded_default(),
        }
    }

    #[cfg(not(test))]
    fn unrecorded_default() -> Result<CoincubeDirectory, Box<dyn std::error::Error>> {
        Self::new_default()
    }

    /// Unit tests never call `GUI::new`, so nothing records a datadir. Code that
    /// reads `active()` (the duress reconcile after a mocked sign-in, for one)
    /// must not reach the developer's real Coincube directory.
    #[cfg(test)]
    fn unrecorded_default() -> Result<CoincubeDirectory, Box<dyn std::error::Error>> {
        Ok(CoincubeDirectory::new(std::env::temp_dir().join(format!(
            "coincube-test-active-{}",
            std::process::id()
        ))))
    }
}

impl CoincubeDirectory {
    pub fn exists(&self) -> bool {
        self.0.as_path().exists()
    }
    pub fn init(&self) -> Result<(), Box<dyn std::error::Error>> {
        create_directory(self.0.as_path())
    }
    pub fn path(&self) -> &Path {
        self.0.as_path()
    }

    /// The per-chain directory: `<datadir>/<ChainId::dir_name>`. Keyed on the
    /// chain's *identity*, not its encoding, so a Bitcoin Blake2b Cube lives
    /// in `bitcoin-blake2b/` and never in `bitcoin/` — the two encode alike,
    /// which is exactly why the directory must not be derived from
    /// `bitcoin::Network`. Bitcoin-family callers that hold a `Network` still
    /// resolve to the same paths as before (`From<Network> for ChainId` is
    /// the identity mapping for that family).
    pub fn network_directory<C: Into<ChainId>>(&self, chain: C) -> NetworkDirectory {
        let mut path = self.0.clone();
        path.push(chain.into().dir_name());
        NetworkDirectory::new(path)
    }

    /// The Bitcoin-family managed-node root, `<datadir>/bitcoind`. Unchanged
    /// for every existing install; see [`Self::bitcoind_directory_for`] for
    /// the chain-keyed form.
    pub fn bitcoind_directory(&self) -> BitcoindDirectory {
        self.bitcoind_directory_for(crate::node::bitcoind::NodeChainFamily::Bitcoin)
    }

    /// The managed-node root for a chain family: `bitcoind/` for Bitcoin and
    /// its test networks, `bitcoind-blake2b/` for Bitcoin Blake2b. Binaries,
    /// the node datadir (config, cookie, chainstate), locks and the managed-node
    /// state ledger all live below it, so the two chains' nodes never share a
    /// file.
    pub fn bitcoind_directory_for(
        &self,
        family: crate::node::bitcoind::NodeChainFamily,
    ) -> BitcoindDirectory {
        let mut path = self.0.clone();
        path.push(family.root_dir_name());
        BitcoindDirectory::new(path)
    }
}

// Get the absolute path to the COINCUBE configuration folder.
///
/// This a "coincube" directory in the XDG standard configuration directory for all OSes but
/// Linux-based ones, for which it's `~/.coincube`.
/// Rationale: we want to have the database, RPC socket, etc.. in the same folder as the
/// configuration file but for Linux the XDG specify a data directory (`~/.local/share/`) different
/// from the configuration one (`~/.config/`).
fn default_datadir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    let configs_dir = dirs::home_dir();

    #[cfg(not(target_os = "linux"))]
    let configs_dir = dirs::config_dir();

    if let Some(mut path) = configs_dir {
        #[cfg(target_os = "linux")]
        path.push(".coincube");

        #[cfg(not(target_os = "linux"))]
        path.push("Coincube");

        return Ok(path);
    }

    Err("Failed to get default data directory".into())
}

#[derive(Clone, Debug)]
pub struct NetworkDirectory(PathBuf);

impl NetworkDirectory {
    pub fn new(p: PathBuf) -> Self {
        NetworkDirectory(p)
    }
}

impl NetworkDirectory {
    pub fn exists(&self) -> bool {
        self.0.as_path().exists()
    }
    pub fn init(&self) -> Result<(), Box<dyn std::error::Error>> {
        create_directory(self.0.as_path())?;
        create_directory(&self.0.as_path().join("data"))
    }
    pub fn path(&self) -> &Path {
        self.0.as_path()
    }
    pub fn coincubed_data_directory(&self, wallet_id: &WalletId) -> DataDirectory {
        let mut path = self.0.clone();
        if !wallet_id.is_legacy() {
            path.push("data");
            path.push(wallet_id.to_string());
        }
        DataDirectory::new(path)
    }
}

#[derive(Clone, Debug)]
pub struct BitcoindDirectory(PathBuf);

impl BitcoindDirectory {
    pub fn new(p: PathBuf) -> Self {
        BitcoindDirectory(p)
    }
    pub fn exists(&self) -> bool {
        self.0.as_path().exists()
    }
    pub fn init(&self) -> Result<(), Box<dyn std::error::Error>> {
        create_directory(self.0.as_path())
    }
    pub fn path(&self) -> &Path {
        self.0.as_path()
    }
}

pub(crate) fn create_directory(
    datadir_path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    return {
        use std::fs::DirBuilder;
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = DirBuilder::new();
        builder.mode(0o700).recursive(true).create(datadir_path)?;
        Ok(())
    };

    // TODO: permissions on Windows..
    #[cfg(not(unix))]
    return {
        std::fs::create_dir_all(datadir_path)?;
        Ok(())
    };
}

#[cfg(test)]
mod tests {
    struct TestParent(PathBuf);

    impl TestParent {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("coincube-bootstrap-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn root(&self, name: &str) -> CoincubeDirectory {
            CoincubeDirectory::new(self.0.join(name))
        }
    }

    impl Drop for TestParent {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn first_creator_starts_shown_and_restarts_keep_the_established_choice() {
        let parent = TestParent::new();
        let root = parent.root("custom-root");
        let first = root.bootstrap_privacy().unwrap().unwrap();
        assert_eq!(first.balances_hidden(), Some(false));
        let second = root.bootstrap_privacy().unwrap().unwrap();
        assert_eq!(second.balances_hidden(), Some(false));
        assert!(
            std::fs::read_to_string(crate::app::settings::global::GlobalSettings::path(&root))
                .unwrap()
                .contains("\"balance_privacy_initialized\": true")
        );
    }

    #[test]
    fn existing_empty_and_post_creation_crash_roots_initialize_hidden() {
        let parent = TestParent::new();
        let root = parent.root("existing");
        fs::create_dir(root.path()).unwrap();
        assert_eq!(
            root.bootstrap_privacy().unwrap().unwrap().balances_hidden(),
            Some(true)
        );

        let lost_global = parent.root("lost-global");
        fs::create_dir(lost_global.path()).unwrap();
        // This is also the state after mkdir succeeded but the first global
        // transaction failed before its rename or the process crashed.
        assert_eq!(
            lost_global
                .bootstrap_privacy()
                .unwrap()
                .unwrap()
                .balances_hidden(),
            Some(true)
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_writable_root_initializes_under_read_only_parent() {
        use std::os::unix::fs::PermissionsExt;

        let parent = TestParent::new();
        let root = parent.root("existing");
        fs::create_dir(root.path()).unwrap();
        fs::set_permissions(&parent.0, fs::Permissions::from_mode(0o500)).unwrap();
        let result = root.bootstrap_privacy();
        fs::set_permissions(&parent.0, fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(result.unwrap().unwrap().balances_hidden(), Some(true));
        assert!(root.path().join(".coincube-bootstrap.lock").is_file());
        assert!(!parent.0.join(".coincube-bootstrap.lock").exists());
    }

    #[test]
    fn existing_root_dot_and_dotdot_aliases_start_hidden() {
        let parent = TestParent::new();
        let existing = parent.root("existing");
        fs::create_dir(existing.path()).unwrap();
        fs::create_dir(existing.path().join("child")).unwrap();

        for alias in [
            existing.path().join("."),
            existing.path().join("child").join(".."),
        ] {
            let receipt = CoincubeDirectory::new(alias)
                .bootstrap_privacy()
                .unwrap()
                .unwrap();
            assert_eq!(receipt.balances_hidden(), Some(true));
        }
        assert!(existing.path().join(".coincube-bootstrap.lock").is_file());
    }

    #[test]
    fn malformed_global_is_preserved_and_unsafe_final_root_is_refused() {
        let parent = TestParent::new();
        let root = parent.root("malformed");
        fs::create_dir(root.path()).unwrap();
        let global = crate::app::settings::global::GlobalSettings::path(&root);
        fs::write(&global, b"{invalid").unwrap();
        assert!(root.bootstrap_privacy().unwrap().is_err());
        assert_eq!(fs::read(&global).unwrap(), b"{invalid");

        let file_root = parent.root("file-root");
        fs::write(file_root.path(), b"unsafe").unwrap();
        assert!(file_root.bootstrap_privacy().is_err());
    }

    #[test]
    fn two_startups_serialize_on_parent_lock_and_keep_first_choice() {
        use std::sync::mpsc;
        use std::time::Duration;

        let parent = TestParent::new();
        let root = parent.root("shared");
        let lock = bootstrap_lock(&parent.0.join(".coincube-bootstrap.lock")).unwrap();
        FileExt::lock_exclusive(&lock).unwrap();
        let (sender, receiver) = mpsc::channel();
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let root = root.clone();
                let sender = sender.clone();
                std::thread::spawn(move || {
                    sender
                        .send(root.bootstrap_privacy().unwrap().unwrap().balances_hidden())
                        .unwrap();
                })
            })
            .collect();
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        FileExt::unlock(&lock).unwrap();
        for _ in 0..2 {
            assert_eq!(
                receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
                Some(false)
            );
        }
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn explicit_choice_committed_while_startup_waits_wins_recheck() {
        use crate::app::settings::preferences::PreferenceChange;
        use std::sync::mpsc;
        use std::time::Duration;

        let parent = TestParent::new();
        let root = parent.root("existing");
        fs::create_dir(root.path()).unwrap();
        let lock = bootstrap_lock(&root.path().join(".coincube-bootstrap.lock")).unwrap();
        FileExt::lock_exclusive(&lock).unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker_root = root.clone();
        let worker = std::thread::spawn(move || {
            sender
                .send(
                    worker_root
                        .bootstrap_privacy()
                        .unwrap()
                        .unwrap()
                        .balances_hidden(),
                )
                .unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        let saved = crate::app::settings::global::GlobalSettings::save_preference(
            &crate::app::settings::global::GlobalSettings::path(&root),
            PreferenceChange::HideBalances(false),
        )
        .unwrap();
        assert_eq!(saved.balances_hidden(), Some(false));
        FileExt::unlock(&lock).unwrap();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            Some(false)
        );
        worker.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn parent_alias_shares_lock_and_final_root_symlink_is_refused() {
        use std::os::unix::fs::symlink;
        let parent = TestParent::new();
        let alias = parent.0.with_extension("alias");
        symlink(&parent.0, &alias).unwrap();
        let alias_root = CoincubeDirectory::new(alias.join("same"));
        assert_eq!(
            alias_root
                .bootstrap_privacy()
                .unwrap()
                .unwrap()
                .balances_hidden(),
            Some(false)
        );
        assert_eq!(
            parent
                .root("same")
                .bootstrap_privacy()
                .unwrap()
                .unwrap()
                .balances_hidden(),
            Some(false)
        );
        assert!(parent.0.join(".coincube-bootstrap.lock").is_file());
        assert!(!alias.join(".coincube-bootstrap.lock").is_symlink());

        symlink(parent.root("same").path(), parent.0.join("linked-root")).unwrap();
        assert!(parent.root("linked-root").bootstrap_privacy().is_err());

        let other_parent = TestParent::new();
        symlink(
            parent.0.join(".coincube-bootstrap.lock"),
            other_parent.0.join(".coincube-bootstrap.lock"),
        )
        .unwrap();
        assert!(other_parent.root("new").bootstrap_privacy().is_err());
        assert!(!other_parent.root("new").path().exists());
        fs::remove_file(alias).unwrap();
    }

    #[test]
    fn unrecorded_active_datadir_is_never_the_os_default_in_tests() {
        // Whatever an earlier test recorded, `active()` is a temp path: the
        // fallback is per-process temp, and every test fixture's root is temp.
        let active = super::CoincubeDirectory::active().unwrap();
        assert!(
            active.path().starts_with(std::env::temp_dir()),
            "{:?}",
            active
        );
        if let Ok(real) = super::CoincubeDirectory::new_default() {
            assert_ne!(active, real);
        }
    }

    use super::*;
    use crate::chain::ChainIdExt;
    use coincube_core::miniscript::bitcoin::Network;

    #[test]
    fn bitcoin_family_paths_are_unchanged_by_the_chain_id_key() {
        let root = CoincubeDirectory::new(PathBuf::from("/tmp/coincube-test"));
        for chain in ChainId::LAUNCHER {
            let network: Network = chain.bitcoin_network();
            // What every existing install was written under …
            let legacy = root.path().join(network.to_string());
            // … is what both the `Network` and the `ChainId` callers resolve to.
            assert_eq!(root.network_directory(network).path(), legacy.as_path());
            assert_eq!(root.network_directory(chain).path(), legacy.as_path());
        }
    }

    #[test]
    fn managed_node_roots_are_distinct_per_chain_family_and_bitcoins_is_unchanged() {
        use crate::node::bitcoind::NodeChainFamily;
        let root = CoincubeDirectory::new(PathBuf::from("/tmp/coincube-test"));
        assert_eq!(
            root.bitcoind_directory().path(),
            root.path().join("bitcoind")
        );
        assert_eq!(
            root.bitcoind_directory_for(NodeChainFamily::Bitcoin).path(),
            root.bitcoind_directory().path()
        );
        assert_eq!(
            root.bitcoind_directory_for(NodeChainFamily::BitcoinBlake2b)
                .path(),
            root.path().join("bitcoind-blake2b")
        );
    }

    #[test]
    fn a_blake2b_identity_gets_its_own_directory_never_bitcoins() {
        let root = CoincubeDirectory::new(PathBuf::from("/tmp/coincube-test"));
        let bitcoin = root.network_directory(ChainId::Bitcoin);
        let btcb2 = root.network_directory(ChainId::BitcoinBlake2b);
        let btcb2_t4 = root.network_directory(ChainId::BitcoinBlake2bTestnet4);
        let testnet4 = root.network_directory(ChainId::Testnet4);
        assert_eq!(btcb2.path(), root.path().join("bitcoin-blake2b"));
        assert_eq!(
            btcb2_t4.path(),
            root.path().join("bitcoin-blake2b-testnet4")
        );
        assert_ne!(btcb2.path(), bitcoin.path());
        assert_ne!(btcb2_t4.path(), testnet4.path());
        // Projecting to the encoding first would collapse into `bitcoin/`:
        // that is the mistake the ChainId key exists to make impossible.
        assert_eq!(
            root.network_directory(ChainId::BitcoinBlake2b.bitcoin_network())
                .path(),
            bitcoin.path()
        );
    }
}
