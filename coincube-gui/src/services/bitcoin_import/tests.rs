use super::*;
use crate::app::settings::{BitcoinImport, CubeSettings};
use crate::chain::ChainId;
use coincube_core::{
    miniscript::bitcoin::{
        bip32::{DerivationPath, Xpriv, Xpub},
        secp256k1::Secp256k1,
        Network,
    },
    wallet_descriptor::ImportedWalletDescriptor,
};
use std::{str::FromStr, time::Duration};

struct Fixture {
    root: PathBuf,
    directory: NetworkDirectory,
    cube: CubeSettings,
    other: CubeSettings,
    wallet: ImportedWalletAttachment,
    config: Config,
}
impl Fixture {
    async fn new() -> Self {
        let root = std::env::temp_dir().join(format!("bitcoin-import-test-{}", Uuid::new_v4()));
        let directory = NetworkDirectory::new(root.join("regtest"));
        let secp = Secp256k1::new();
        let master = Xpriv::new_master(Network::Regtest, &[42; 32]).unwrap();
        let account = master
            .derive_priv(&secp, &DerivationPath::from_str("m/84'/1'/0'").unwrap())
            .unwrap();
        let key = format!(
            "[{}/84'/1'/0']{}",
            master.fingerprint(&secp),
            Xpub::from_priv(&secp, &account)
        );
        let descriptor = ImportedWalletDescriptor::parse(
            &format!("wpkh({key}/0/*)"),
            Some(&format!("wpkh({key}/1/*)")),
            ChainId::Regtest,
        )
        .unwrap();
        let wallet =
            ImportedWalletAttachment::new(descriptor.clone(), ChainId::Regtest, 0, 200).unwrap();
        let config = Config::new(
            coincubed::config::BitcoinConfig::new(ChainId::Regtest, Duration::from_secs(30)),
            None,
            log::LevelFilter::Info,
            WalletDescriptor::Imported(descriptor),
            directory.coincubed_data_directory(&wallet.wallet_id),
        );
        let mut cube = CubeSettings::new("Import target".into(), ChainId::Regtest);
        cube.bitcoin_import = Some(BitcoinImport::pending());
        let mut other = CubeSettings::new("Another pending Cube".into(), ChainId::Regtest);
        other.bitcoin_import = Some(BitcoinImport::pending());
        directory.init().unwrap();
        settings::update_settings_file(&directory, |mut settings| {
            // The other Cube deliberately comes first: a first-empty fallback
            // would attach the wallet to the wrong user-visible Cube.
            settings.cubes = vec![other.clone(), cube.clone()];
            Some(settings)
        })
        .await
        .unwrap();
        Self {
            root,
            directory,
            cube,
            other,
            wallet,
            config,
        }
    }
    fn operation(&self) -> Uuid {
        self.cube.bitcoin_import.as_ref().unwrap().operation_id
    }
    async fn install(&self) -> Result<WalletId, String> {
        install(
            &self.directory,
            &self.cube.id,
            self.operation(),
            self.wallet.clone(),
            self.config.clone(),
        )
        .await
    }
    fn stored(&self) -> CubeSettings {
        Settings::from_file(&self.directory)
            .unwrap()
            .cubes
            .into_iter()
            .find(|cube| cube.id == self.cube.id)
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn installs_only_the_named_cube_and_retry_preserves_ready_backup_and_database() {
    let f = Fixture::new().await;
    assert_eq!(f.install().await.unwrap(), f.wallet.wallet_id);
    let saved = Settings::from_file(&f.directory).unwrap();
    assert!(saved.cubes[0].bitcoin_import_pending());
    assert_eq!(saved.cubes[0].id, f.other.id);
    assert!(saved.cubes[1].has_imported_bitcoin());
    assert!(saved.cubes[1].vault_wallet_id.is_none());
    assert!(!saved.cubes[1].bitcoin_import.as_ref().unwrap().is_ready());
    assert_eq!(saved.wallets.len(), 1);
    let data = f.directory.coincubed_data_directory(&f.wallet.wallet_id);
    let before_db = fs::read(data.sqlite_db_file_path()).unwrap();
    settings::update_settings_file(&f.directory, |mut settings| {
        let cube = settings
            .cubes
            .iter_mut()
            .find(|c| c.id == f.cube.id)
            .unwrap();
        cube.bitcoin_import.as_mut().unwrap().stage = BitcoinImportStage::Ready {
            wallet: f.wallet.clone(),
        };
        cube.recovery_kit_last_backed_up_descriptor_fingerprint = Some("updated-kit".into());
        Some(settings)
    })
    .await
    .unwrap();
    assert_eq!(f.install().await.unwrap(), f.wallet.wallet_id);
    let cube = f.stored();
    assert!(cube.bitcoin_import.as_ref().unwrap().is_ready());
    assert_eq!(
        cube.recovery_kit_last_backed_up_descriptor_fingerprint
            .as_deref(),
        Some("updated-kit")
    );
    assert_eq!(fs::read(data.sqlite_db_file_path()).unwrap(), before_db);
    assert_eq!(Settings::from_file(&f.directory).unwrap().wallets.len(), 1);
}

#[tokio::test]
async fn wrong_operation_and_duplicate_cube_are_refused_without_reassignment() {
    let f = Fixture::new().await;
    let before = fs::read(f.directory.path().join(settings::SETTINGS_FILE_NAME)).unwrap();
    assert!(install(
        &f.directory,
        &f.cube.id,
        Uuid::new_v4(),
        f.wallet.clone(),
        f.config.clone()
    )
    .await
    .unwrap_err()
    .contains("different setup"));
    assert_eq!(
        fs::read(f.directory.path().join(settings::SETTINGS_FILE_NAME)).unwrap(),
        before
    );
    f.install().await.unwrap();
    let other_operation = f.other.bitcoin_import.as_ref().unwrap().operation_id;
    assert!(install(
        &f.directory,
        &f.other.id,
        other_operation,
        f.wallet.clone(),
        f.config.clone()
    )
    .await
    .unwrap_err()
    .contains("another Cube"));
    assert!(Settings::from_file(&f.directory).unwrap().cubes[0].bitcoin_import_pending());
}

#[tokio::test]
async fn unrelated_destination_is_never_deleted_or_attached() {
    let f = Fixture::new().await;
    let destination = f.directory.coincubed_data_directory(&f.wallet.wallet_id);
    destination.init().unwrap();
    fs::write(
        destination.path().join("unrelated"),
        b"keep this wallet data",
    )
    .unwrap();
    assert!(f.install().await.is_err());
    assert_eq!(
        fs::read(destination.path().join("unrelated")).unwrap(),
        b"keep this wallet data"
    );
    assert!(f.stored().bitcoin_import_pending());
}

#[tokio::test]
async fn interrupted_publication_resumes_same_storage_and_exact_cube() {
    let f = Fixture::new().await;
    f.install().await.unwrap();
    let mut journal = read_journal(&journal_path(&f.directory, f.operation()))
        .unwrap()
        .unwrap();
    journal.phase = Phase::StorageReady;
    journal.persist(&f.directory).unwrap();
    fs::rename(
        journal.final_path(&f.directory),
        journal.stage_path(&f.directory),
    )
    .unwrap();
    settings::update_settings_file(&f.directory, |mut settings| {
        let cube = settings
            .cubes
            .iter_mut()
            .find(|c| c.id == f.cube.id)
            .unwrap();
        cube.bitcoin_import.as_mut().unwrap().stage = BitcoinImportStage::Installing {
            wallet: f.wallet.clone(),
        };
        settings.wallets.clear();
        Some(settings)
    })
    .await
    .unwrap();
    assert_eq!(
        resume(&f.directory, &f.cube.id, f.operation())
            .await
            .unwrap(),
        Some(f.wallet.wallet_id.clone())
    );
    assert!(!journal.stage_path(&f.directory).exists());
    assert!(journal
        .final_path(&f.directory)
        .join("coincubed.sqlite3")
        .is_file());
    assert!(f.stored().has_imported_bitcoin());
    assert!(!f.stored().bitcoin_import.as_ref().unwrap().is_ready());
    assert!(resume(&f.directory, &f.other.id, f.operation())
        .await
        .is_err());
}

#[tokio::test]
async fn duress_conflict_refuses_before_settings_or_wallet_storage_changes() {
    let f = Fixture::new().await;
    DuressLocalState {
        enrolled: true,
        ..Default::default()
    }
    .save(&f.root)
    .unwrap();
    let before = fs::read(f.directory.path().join(settings::SETTINGS_FILE_NAME)).unwrap();
    assert!(f.install().await.unwrap_err().contains("Duress Mode"));
    assert_eq!(
        fs::read(f.directory.path().join(settings::SETTINGS_FILE_NAME)).unwrap(),
        before
    );
    assert!(!f
        .directory
        .coincubed_data_directory(&f.wallet.wallet_id)
        .exists());
    assert!(DuressLocalState::load(&f.root).unwrap().enrolled);
}

#[test]
fn pending_import_preserves_vault_and_duress_capability_boundaries() {
    let mut cube = CubeSettings::new("Pending".into(), ChainId::Bitcoin);
    cube.bitcoin_import = Some(BitcoinImport::pending());
    let bytes = serde_json::to_vec(&cube).unwrap();
    let mut restored: CubeSettings = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(restored.bitcoin_import, cube.bitcoin_import);
    assert!(restored.bitcoin_import_pending());
    assert!(!restored.has_imported_bitcoin());
    assert!(!restored.supports_duress());
    restored.set_vault(settings::VaultIdentity::new(
        WalletId::new("legacy".into(), Some(1)),
        None,
    ));
    assert!(restored.vault_wallet_id.is_none());
    assert!(WalletId::new("legacy".into(), None).is_legacy());
    assert!(!WalletId::new(format!("imported-v1-{}", "a".repeat(64)), None).is_legacy());
}
