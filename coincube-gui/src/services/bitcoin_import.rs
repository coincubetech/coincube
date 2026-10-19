//! Recoverable installation of an imported wallet into one explicitly named Cube.
//!
//! The journal and ownership marker contain public metadata only. A wallet
//! becomes visible only after complete staged storage has been published. An
//! interrupted operation can publish/attach the same storage again, never choose
//! another Cube or delete a directory not owned by this operation.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use async_fd_lock::LockWrite;
use coincube_core::wallet_descriptor::WalletDescriptor;
use coincubed::{config::Config, datadir::DataDirectory};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::settings::{
        self, BitcoinImportStage, ImportedWalletAttachment, Settings, SettingsError, WalletId,
        WalletSettings,
    },
    dir::NetworkDirectory,
    services::duress::DuressLocalState,
};

const OWNER_FILE: &str = "import-owner.json";
const CONFIG_FILE: &str = "daemon.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    cube_id: String,
    operation_id: Uuid,
    wallet: ImportedWalletAttachment,
    phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    StorageReady,
    Published,
    Attached,
}

impl Journal {
    fn path(&self, directory: &NetworkDirectory) -> PathBuf {
        journal_path(directory, self.operation_id)
    }
    fn stage_path(&self, directory: &NetworkDirectory) -> PathBuf {
        directory
            .path()
            .join("data")
            .join(format!(".import-{}", self.operation_id))
    }
    fn final_path(&self, directory: &NetworkDirectory) -> PathBuf {
        directory
            .coincubed_data_directory(&self.wallet.wallet_id)
            .path()
            .to_path_buf()
    }
    fn persist(&self, directory: &NetworkDirectory) -> Result<(), String> {
        write_json(&self.path(directory), self)
    }
}

fn journal_path(directory: &NetworkDirectory, operation_id: Uuid) -> PathBuf {
    directory
        .path()
        .join("imports")
        .join(format!("{operation_id}.json"))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let parent = path.parent().ok_or("Missing import storage directory")?;
    crate::dir::create_directory(parent).map_err(|e| e.to_string())?;
    let tmp = parent.join(format!(".import-write-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(value).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        sync_directory(parent)
    })();
    if tmp.exists() {
        let _ = fs::remove_file(tmp);
    }
    result
}

fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn read_journal(path: &Path) -> Result<Option<Journal>, String> {
    match fs::read(path) {
        Ok(bytes) => {
            let value: Journal = serde_json::from_slice(&bytes)
                .map_err(|e| format!("Invalid import journal: {e}"))?;
            if value.version != 1 {
                return Err("Unsupported wallet import journal version.".into());
            }
            Ok(Some(value))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Cannot read import journal: {e}")),
    }
}

fn check_duress(directory: &NetworkDirectory) -> Result<(), String> {
    let root = directory
        .path()
        .parent()
        .ok_or("Missing Cube data directory")?;
    let state =
        DuressLocalState::load(root).map_err(|e| format!("Cannot check Duress Mode: {e}"))?;
    if state.enrolled || state.arming || state.active {
        return Err("Resolve Duress Mode in Settings before importing this wallet. Imported Bitcoin wallets do not yet support Duress Mode recovery.".into());
    }
    Ok(())
}

fn check_cube(settings: &Settings, journal: &Journal, allow_attached: bool) -> Result<(), String> {
    let cube = settings
        .cubes
        .iter()
        .find(|c| c.id == journal.cube_id)
        .ok_or("The Cube for this import no longer exists.")?;
    journal.wallet.validate(cube.network)?;
    if cube.vault_wallet_id.is_some() {
        return Err("This Cube already has a Vault.".into());
    }
    let intent = cube
        .bitcoin_import
        .as_ref()
        .ok_or("This Cube is not waiting for a wallet import.")?;
    if intent.operation_id != journal.operation_id {
        return Err("This import belongs to a different setup operation.".into());
    }
    if let Some(wallet) = intent.attachment() {
        if wallet != &journal.wallet {
            return Err("The pending import has changed. Review the wallet again.".into());
        }
    }
    if !allow_attached && intent.active_attachment().is_some() {
        return Err("This Cube already has an imported wallet.".into());
    }
    if settings.cubes.iter().any(|other| {
        other.id != cube.id
            && other
                .bitcoin_import
                .as_ref()
                .and_then(|v| v.attachment())
                .is_some_and(|w| w.wallet_id == journal.wallet.wallet_id)
    }) {
        return Err("This Bitcoin wallet is already attached to another Cube.".into());
    }
    Ok(())
}

async fn set_stage(
    directory: &NetworkDirectory,
    journal: &Journal,
    stage: BitcoinImportStage,
) -> Result<(), String> {
    settings::update_settings_file_checked(directory, |mut settings| {
        check_cube(&settings, journal, true).map_err(SettingsError::WritingFile)?;
        let cube = settings
            .cubes
            .iter_mut()
            .find(|c| c.id == journal.cube_id)
            .ok_or_else(|| SettingsError::WritingFile("Import Cube is missing".into()))?;
        let intent = cube
            .bitcoin_import
            .as_mut()
            .ok_or_else(|| SettingsError::WritingFile("Import intent is missing".into()))?;
        // A retry after successful attachment must not move a ready wallet back
        // to Installing or Syncing or reset its address allocations.
        if !intent.is_ready() {
            intent.stage = stage;
            intent.last_error = None;
        }
        Ok(Some(settings))
    })
    .await
    .map_err(|e| e.to_string())
}

fn owns_directory(path: &Path, journal: &Journal) -> Result<bool, String> {
    let Some(owner) = read_journal(&path.join(OWNER_FILE))? else {
        return Ok(false);
    };
    Ok(owner.cube_id == journal.cube_id
        && owner.operation_id == journal.operation_id
        && owner.wallet == journal.wallet)
}

fn verify_published(directory: &NetworkDirectory, journal: &Journal) -> Result<(), String> {
    let destination = journal.final_path(directory);
    if !owns_directory(&destination, journal)? {
        return Err(
            "The wallet directory belongs to another operation; no files were changed.".into(),
        );
    }
    let config =
        Config::from_file(Some(destination.join(CONFIG_FILE))).map_err(|e| e.to_string())?;
    if config.main_descriptor != WalletDescriptor::Imported(journal.wallet.descriptor.clone())
        || config.data_directory().as_ref().map(|d| d.path()) != Some(destination.as_path())
        || !destination.join("coincubed.sqlite3").is_file()
    {
        return Err("The installed wallet does not match the import journal.".into());
    }
    Ok(())
}

async fn finish(directory: &NetworkDirectory, journal: &mut Journal) -> Result<WalletId, String> {
    check_duress(directory)?;
    check_cube(
        &Settings::from_file(directory).map_err(|e| e.to_string())?,
        journal,
        true,
    )?;
    let stage = journal.stage_path(directory);
    let destination = journal.final_path(directory);
    if destination.exists() {
        verify_published(directory, journal)?;
    } else {
        if journal.phase == Phase::Prepared {
            return Err(
                "Wallet installation was interrupted. Review and confirm this import again.".into(),
            );
        }
        if !owns_directory(&stage, journal)? {
            return Err(
                "The staged wallet is missing its ownership record; no files were changed.".into(),
            );
        }
        fs::rename(&stage, &destination)
            .map_err(|e| format!("Cannot publish imported wallet: {e}"))?;
        sync_directory(
            destination
                .parent()
                .ok_or("Missing wallet parent directory")?,
        )?;
        verify_published(directory, journal)?;
    }
    journal.phase = Phase::Published;
    journal.persist(directory)?;
    // One settings transaction attaches the exact Cube and registers the local
    // wallet. This never uses the legacy 'first empty Cube' fallback.
    settings::update_settings_file_checked(directory, |mut settings| {
        check_cube(&settings, journal, true).map_err(SettingsError::WritingFile)?;
        let cube = settings
            .cubes
            .iter_mut()
            .find(|c| c.id == journal.cube_id)
            .ok_or_else(|| SettingsError::WritingFile("Import Cube is missing".into()))?;
        let intent = cube
            .bitcoin_import
            .as_mut()
            .ok_or_else(|| SettingsError::WritingFile("Import intent is missing".into()))?;
        let newly_attached = intent.active_attachment().is_none();
        if !intent.is_ready() {
            intent.stage = BitcoinImportStage::Syncing {
                wallet: journal.wallet.clone(),
            };
        }
        intent.last_error = None;
        // Seed-only creation Kits become stale only on first attachment. A
        // restart must preserve a Kit updated after the import completed.
        if newly_attached {
            cube.recovery_kit_last_backed_up_descriptor_fingerprint = None;
            cube.recovery_kit_last_backed_up_keychain_descriptor_fingerprint = None;
        }
        if !settings
            .wallets
            .iter()
            .any(|w| w.wallet_id() == journal.wallet.wallet_id)
        {
            settings.wallets.push(WalletSettings {
                name: "Imported Bitcoin wallet".into(),
                alias: None,
                descriptor_checksum: journal.wallet.wallet_id.descriptor_checksum.clone(),
                pinned_at: journal.wallet.wallet_id.timestamp,
                keys: Vec::new(),
                hardware_wallets: Vec::new(),
                remote_backend_auth: None,
                start_internal_bitcoind: Some(false),
                pending_rescan: None,
                keychain_keys_recorded: false,
            });
        }
        Ok(Some(settings))
    })
    .await
    .map_err(|e| e.to_string())?;
    journal.phase = Phase::Attached;
    journal.persist(directory)?;
    Ok(journal.wallet.wallet_id.clone())
}

/// Install or retry one validated public wallet. The config must point at its
/// final directory; initialization happens privately in a separate stage.
pub async fn install(
    directory: &NetworkDirectory,
    cube_id: &str,
    operation_id: Uuid,
    wallet: ImportedWalletAttachment,
    config: Config,
) -> Result<WalletId, String> {
    directory.init().map_err(|e| e.to_string())?;
    let _lock = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.path().join("wallet-import.lock"))
        .await
        .map_err(|e| e.to_string())?
        .lock_write()
        .await
        .map_err(|e| e.to_string())?;
    check_duress(directory)?;
    let mut journal = Journal {
        version: 1,
        cube_id: cube_id.into(),
        operation_id,
        wallet,
        phase: Phase::Prepared,
    };
    check_cube(
        &Settings::from_file(directory).map_err(|e| e.to_string())?,
        &journal,
        true,
    )?;
    let destination = journal.final_path(directory);
    if config.main_descriptor != WalletDescriptor::Imported(journal.wallet.descriptor.clone())
        || config.data_directory().as_ref().map(|d| d.path()) != Some(destination.as_path())
    {
        return Err(
            "The import configuration does not match this wallet and its storage directory.".into(),
        );
    }
    journal.wallet.validate(config.bitcoin_config.chain)?;
    if let Some(prior) = read_journal(&journal.path(directory))? {
        if prior.cube_id != journal.cube_id || prior.wallet != journal.wallet {
            return Err("This import operation already names a different wallet.".into());
        }
        journal = prior;
        if journal.phase != Phase::Prepared || destination.exists() {
            return finish(directory, &mut journal).await;
        }
    } else if destination.exists() {
        return Err("A wallet already exists at this destination. No files were changed.".into());
    }
    journal.persist(directory)?;
    set_stage(
        directory,
        &journal,
        BitcoinImportStage::Installing {
            wallet: journal.wallet.clone(),
        },
    )
    .await?;
    let stage = journal.stage_path(directory);
    if stage.exists() {
        if owns_directory(&stage, &journal)? {
            // Only unpublished storage owned by this exact prepared operation
            // can be discarded and recreated. Never touch a final wallet.
            fs::remove_dir_all(&stage).map_err(|e| e.to_string())?;
        } else if fs::read_dir(&stage)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
        {
            return Err(
                "The staging directory is not owned by this import; no files were changed.".into(),
            );
        }
    }
    crate::dir::create_directory(&stage).map_err(|e| e.to_string())?;
    write_json(&stage.join(OWNER_FILE), &journal)?;
    coincubed::create_imported_storage(
        &config,
        &DataDirectory::new(stage.clone()),
        journal.wallet.birthday,
        journal.wallet.initial_lookahead,
    )
    .map_err(|e| e.to_string())?;
    sync_directory(&stage)?;
    journal.phase = Phase::StorageReady;
    journal.persist(directory)?;
    finish(directory, &mut journal).await
}

/// Resume publication/attachment after restart. A Prepared journal needs the
/// review screen to supply configuration again; it is never mistaken for Ready.
pub async fn resume(
    directory: &NetworkDirectory,
    cube_id: &str,
    operation_id: Uuid,
) -> Result<Option<WalletId>, String> {
    directory.init().map_err(|e| e.to_string())?;
    let _lock = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.path().join("wallet-import.lock"))
        .await
        .map_err(|e| e.to_string())?
        .lock_write()
        .await
        .map_err(|e| e.to_string())?;
    let Some(mut journal) = read_journal(&journal_path(directory, operation_id))? else {
        return Ok(None);
    };
    if journal.cube_id != cube_id {
        return Err("This import journal belongs to a different Cube.".into());
    }
    if journal.phase == Phase::Prepared && !journal.final_path(directory).exists() {
        return Ok(None);
    }
    finish(directory, &mut journal).await.map(Some)
}
