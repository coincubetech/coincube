use crate::app::settings::WalletId;
use crate::chain::ChainId;
use coincubed::datadir::DataDirectory;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

    /// Records the active data directory for the process (idempotent — only the
    /// first call wins, which is the startup resolution).
    pub fn set_active(dir: CoincubeDirectory) {
        let _ = ACTIVE_DATADIR.set(dir);
    }

    /// The active data directory, honouring a custom `--datadir`. Falls back to
    /// the OS default if it was never recorded (e.g. in unit tests).
    pub fn active() -> Result<CoincubeDirectory, Box<dyn std::error::Error>> {
        match ACTIVE_DATADIR.get() {
            Some(dir) => Ok(dir.clone()),
            None => Self::new_default(),
        }
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

    pub fn bitcoind_directory(&self) -> BitcoindDirectory {
        let mut path = self.0.clone();
        path.push("bitcoind");
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
    use super::*;
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
