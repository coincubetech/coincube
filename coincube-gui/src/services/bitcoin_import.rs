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

mod removal;
pub use removal::remove_cube_import;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restored_address_state: Option<RestoredAddressState>,
}

/// Durable high-water marks from a Recovery Kit; these are allocated indices,
/// not historical scan coverage. They must be restored before publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoredAddressState {
    pub receive_index: u32,
    pub change_index: u32,
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

fn check_directory_chain(
    directory: &NetworkDirectory,
    chain: crate::chain::ChainId,
) -> Result<(), String> {
    if directory.path().file_name() != Some(std::ffi::OsStr::new(chain.dir_name())) {
        return Err("The import storage directory belongs to a different chain.".into());
    }
    Ok(())
}

fn check_cube(
    directory: &NetworkDirectory,
    settings: &Settings,
    journal: &Journal,
    allow_attached: bool,
) -> Result<(), String> {
    let cube = settings
        .cubes
        .iter()
        .find(|c| c.id == journal.cube_id)
        .ok_or("The Cube for this import no longer exists.")?;
    check_directory_chain(directory, cube.network)?;
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
        check_cube(directory, &settings, journal, true).map_err(SettingsError::WritingFile)?;
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
        && owner.wallet == journal.wallet
        && owner.restored_address_state == journal.restored_address_state)
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
        || config.bitcoin_config.chain != journal.wallet.descriptor.chain()
        || config.import_birthday != Some(journal.wallet.birthday)
        || config.import_initial_lookahead != Some(journal.wallet.initial_lookahead)
        || config.data_directory().as_ref().map(|d| d.path()) != Some(destination.as_path())
        || !destination.join("coincubed.sqlite3").is_file()
    {
        return Err("The installed wallet does not match the import journal.".into());
    }
    if let Some(required) = journal.restored_address_state {
        let (receive, change) =
            coincubed::read_imported_address_state(&config, &DataDirectory::new(destination))
                .map_err(|e| e.to_string())?;
        if receive < required.receive_index || change < required.change_index {
            return Err("The restored wallet is missing its saved address reservations.".into());
        }
    }
    Ok(())
}

async fn finish(directory: &NetworkDirectory, journal: &mut Journal) -> Result<WalletId, String> {
    check_duress(directory)?;
    check_cube(
        directory,
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
        check_cube(directory, &settings, journal, true).map_err(SettingsError::WritingFile)?;
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
    install_inner(directory, cube_id, operation_id, wallet, config, None).await
}

/// Recovery installation uses the same journal, adding address reservations
/// before storage becomes visible to the GUI or a running daemon.
pub async fn install_restored(
    directory: &NetworkDirectory,
    cube_id: &str,
    operation_id: Uuid,
    wallet: ImportedWalletAttachment,
    config: Config,
    address_state: RestoredAddressState,
) -> Result<WalletId, String> {
    install_inner(
        directory,
        cube_id,
        operation_id,
        wallet,
        config,
        Some(address_state),
    )
    .await
}

async fn install_inner(
    directory: &NetworkDirectory,
    cube_id: &str,
    operation_id: Uuid,
    wallet: ImportedWalletAttachment,
    config: Config,
    restored_address_state: Option<RestoredAddressState>,
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
        .map_err(|e| format!("Cannot lock wallet import: {e:?}"))?;
    check_duress(directory)?;
    let mut journal = Journal {
        version: 1,
        cube_id: cube_id.into(),
        operation_id,
        wallet,
        phase: Phase::Prepared,
        restored_address_state,
    };
    check_cube(
        directory,
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
        if prior.cube_id != journal.cube_id
            || prior.operation_id != operation_id
            || prior.wallet != journal.wallet
            || (journal.restored_address_state.is_some()
                && prior.restored_address_state != journal.restored_address_state)
        {
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
    if let Some(state) = journal.restored_address_state {
        // The initializer writes scan fields into the saved configuration.
        let saved_config =
            Config::from_file(Some(stage.join(CONFIG_FILE))).map_err(|e| e.to_string())?;
        coincubed::restore_imported_address_state(
            &saved_config,
            &DataDirectory::new(stage.clone()),
            state.receive_index,
            state.change_index,
        )
        .map_err(|e| e.to_string())?;
    }
    sync_directory(&stage)?;
    journal.phase = Phase::StorageReady;
    journal.persist(directory)?;
    finish(directory, &mut journal).await
}

/// The authenticated recovery route whose backup record was just restored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoredKitSource {
    Password,
    OwnerKeychain,
}

/// Restore the public-wallet half of an authenticated Recovery Kit into its
/// exact Cube. The caller restores/unlocks the Cube seed through the normal
/// installer; it is never supplied to this wallet's spending code.
pub async fn restore_from_kit(
    directory: &NetworkDirectory,
    cube_id: &str,
    blob: &crate::services::recovery::ImportedDescriptorBlob,
    backend: coincubed::config::BitcoinBackend,
    source: RestoredKitSource,
) -> Result<settings::CubeSettings, String> {
    let chain = blob.wallet.descriptor.chain();
    blob.validate(chain)?;
    check_directory_chain(directory, chain)?;
    check_duress(directory)?;
    if cube_id != blob.cube.uuid {
        return Err("This imported wallet Recovery Kit belongs to another Cube.".into());
    }
    let wallet = ImportedWalletAttachment::new(
        blob.wallet.descriptor.clone(),
        chain,
        blob.wallet.birthday,
        blob.wallet.initial_lookahead,
    )?;
    settings::update_settings_file_checked(directory, |mut settings| {
        let cube = settings
            .cubes
            .iter_mut()
            .find(|cube| cube.id == cube_id)
            .ok_or_else(|| {
                SettingsError::WritingFile("The Cube to restore no longer exists.".into())
            })?;
        if cube.network != chain || cube.vault_wallet_id.is_some() {
            return Err(SettingsError::WritingFile(
                "This Cube already has a Vault or belongs to another chain.".into(),
            ));
        }
        match &cube.bitcoin_import {
            Some(intent)
                if intent
                    .attachment()
                    .is_some_and(|present| present != &wallet) =>
            {
                return Err(SettingsError::WritingFile(
                    "This Cube is already importing another Bitcoin wallet.".into(),
                ));
            }
            None => cube.bitcoin_import = Some(Box::new(settings::BitcoinImport::pending())),
            _ => (),
        }
        Ok(Some(settings))
    })
    .await
    .map_err(|e| e.to_string())?;
    let operation_id = Settings::from_file(directory)
        .map_err(|e| e.to_string())?
        .cubes
        .into_iter()
        .find(|cube| cube.id == cube_id)
        .and_then(|cube| cube.bitcoin_import)
        .ok_or("The restore operation disappeared.")?
        .operation_id;
    let config = Config::new(
        coincubed::config::BitcoinConfig::new(chain, std::time::Duration::from_secs(10)),
        Some(backend),
        log::LevelFilter::Info,
        WalletDescriptor::Imported(blob.wallet.descriptor.clone()),
        directory.coincubed_data_directory(&wallet.wallet_id),
    );
    install_restored(
        directory,
        cube_id,
        operation_id,
        wallet.clone(),
        config,
        RestoredAddressState {
            receive_index: blob.wallet.receive_index,
            change_index: blob.wallet.change_index,
        },
    )
    .await?;
    let fingerprint = crate::services::recovery::imported_descriptor_blob_fingerprint(blob)
        .ok_or("Could not fingerprint the restored imported wallet.")?;
    settings::update_settings_file_checked(directory, |mut settings| {
        let cube = settings
            .cubes
            .iter_mut()
            .find(|cube| cube.id == cube_id)
            .ok_or_else(|| SettingsError::WritingFile("The restored Cube disappeared.".into()))?;
        if !cube.bitcoin_import.as_ref().is_some_and(|intent| {
            intent.operation_id == operation_id && intent.active_attachment() == Some(&wallet)
        }) {
            return Err(SettingsError::WritingFile(
                "The Cube's imported wallet changed during recovery.".into(),
            ));
        }
        match source {
            RestoredKitSource::Password => {
                cube.recovery_kit_password_backed_up = true;
                cube.recovery_kit_last_backed_up_descriptor_fingerprint = Some(fingerprint.clone());
            }
            RestoredKitSource::OwnerKeychain => {
                cube.recovery_kit_last_backed_up_keychain_descriptor_fingerprint =
                    Some(fingerprint.clone());
            }
        }
        if let Some(saved_wallet) = settings
            .wallets
            .iter_mut()
            .find(|entry| entry.wallet_id() == wallet.wallet_id)
        {
            saved_wallet.name = blob.wallet.name.clone();
        }
        Ok(Some(settings))
    })
    .await
    .map_err(|e| e.to_string())?;
    Settings::from_file(directory)
        .map_err(|e| e.to_string())?
        .cubes
        .into_iter()
        .find(|cube| cube.id == cube_id)
        .ok_or_else(|| "The restored Cube disappeared.".into())
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
        .map_err(|e| format!("Cannot lock wallet import: {e:?}"))?;
    let Some(mut journal) = read_journal(&journal_path(directory, operation_id))? else {
        return Ok(None);
    };
    if journal.cube_id != cube_id || journal.operation_id != operation_id {
        return Err("This import journal belongs to a different Cube or setup operation.".into());
    }
    check_cube(
        directory,
        &Settings::from_file(directory).map_err(|e| e.to_string())?,
        &journal,
        true,
    )?;
    if journal.phase == Phase::Prepared && !journal.final_path(directory).exists() {
        return Ok(None);
    }
    finish(directory, &mut journal).await.map(Some)
}

/// Explicitly abandon setup before a wallet has been published. Ordinary
/// Cancel does not call this: it leaves the Cube's resumable import intact.
/// Published/attached wallets require the normal wallet-removal flow instead.
pub async fn abandon_pending(
    directory: &NetworkDirectory,
    cube_id: &str,
    operation_id: Uuid,
) -> Result<(), String> {
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
        .map_err(|e| format!("Cannot lock wallet import: {e:?}"))?;
    let current = Settings::from_file(directory).map_err(|e| e.to_string())?;
    let cube = current
        .cubes
        .iter()
        .find(|c| c.id == cube_id)
        .ok_or("The import Cube no longer exists.")?;
    let intent = cube
        .bitcoin_import
        .as_ref()
        .ok_or("This Cube has no pending import.")?;
    if intent.operation_id != operation_id || !intent.is_pending() || cube.vault_wallet_id.is_some()
    {
        return Err("This import can no longer be abandoned as an unfinished setup.".into());
    }
    let path = journal_path(directory, operation_id);
    if let Some(journal) = read_journal(&path)? {
        check_cube(directory, &current, &journal, false)?;
        if journal.cube_id != cube_id
            || journal.operation_id != operation_id
            || journal.final_path(directory).exists()
        {
            return Err(
                "This wallet has already been installed. Resume it before changing its attachment."
                    .into(),
            );
        }
        let stage = journal.stage_path(directory);
        if stage.exists() {
            if !owns_directory(&stage, &journal)? {
                return Err(
                    "The staged data is not owned by this import; no files were removed.".into(),
                );
            }
            fs::remove_dir_all(&stage).map_err(|e| e.to_string())?;
            sync_directory(stage.parent().ok_or("Missing staging parent")?)?;
        }
    }
    settings::update_settings_file_checked(directory, |mut settings| {
        let cube = settings
            .cubes
            .iter_mut()
            .find(|c| c.id == cube_id)
            .ok_or_else(|| {
                SettingsError::WritingFile("The import Cube no longer exists.".into())
            })?;
        if !cube
            .bitcoin_import
            .as_ref()
            .is_some_and(|intent| intent.operation_id == operation_id && intent.is_pending())
            || cube.vault_wallet_id.is_some()
        {
            return Err(SettingsError::WritingFile(
                "This Cube's wallet setup changed. Nothing was detached.".into(),
            ));
        }
        cube.bitcoin_import = None;
        Ok(Some(settings))
    })
    .await
    .map_err(|e| e.to_string())?;
    match fs::remove_file(&path) {
        Ok(()) => sync_directory(path.parent().ok_or("Missing journal parent")?)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.to_string()),
    }
    Ok(())
}

/// Reconcile durable setup readiness from a daemon positively identified as
/// this Cube's wallet. Current backend errors remain separate UI freshness
/// information; a different wallet/session cannot complete this operation.
pub async fn reconcile_sync(
    directory: &NetworkDirectory,
    cube_id: &str,
    operation_id: Uuid,
    info: &coincubed::commands::GetInfoResult,
) -> Result<(), String> {
    let status = info
        .imported_wallet
        .as_ref()
        .ok_or("The daemon did not report imported wallet synchronization status.")?;
    let imported = match &info.descriptors.main {
        WalletDescriptor::Imported(descriptor) => descriptor,
        WalletDescriptor::Vault(_) => {
            return Err("This daemon is running a Vault, not the imported Bitcoin wallet.".into())
        }
    };
    if info.chain_id != Some(status.chain_id)
        || info.network != status.chain_id.bitcoin_network()
        || imported
            .identity(status.chain_id)
            .map_err(|e| e.to_string())?
            .to_string()
            != status.wallet_identity
    {
        return Err(
            "The daemon's wallet identity or chain does not match its synchronization report."
                .into(),
        );
    }
    check_directory_chain(directory, status.chain_id)?;
    let update = |settings: &mut Settings| -> Result<bool, String> {
        let cube = settings
            .cubes
            .iter_mut()
            .find(|c| c.id == cube_id)
            .ok_or("The import Cube no longer exists.")?;
        if cube.vault_wallet_id.is_some() {
            return Err("This Cube already has a Vault.".into());
        }
        let intent = cube
            .bitcoin_import
            .as_mut()
            .ok_or("The import operation no longer exists.")?;
        let wallet = intent
            .active_attachment()
            .ok_or("The imported wallet is not installed yet.")?
            .clone();
        wallet.validate(cube.network)?;
        if intent.operation_id != operation_id
            || cube.network != status.chain_id
            || wallet.descriptor != *imported
            || status.birthday != wallet.birthday
            || status.initial_lookahead < wallet.initial_lookahead
            || status.discovery_target < status.initial_lookahead
            || status.discovery_target > 10_000
        {
            return Err(
                "This synchronization result belongs to another wallet or setup operation.".into(),
            );
        }
        let stage = if status.history_complete
            && status.receive_scanned >= status.discovery_target
            && status.change_scanned >= status.discovery_target
            && info.sync.is_finite()
            && info.sync >= 1.0
            && info.rescan_progress.is_none()
            && !info.history_sync.full_scan_in_progress
            && info.history_sync.last_failure.is_none()
            && !info.chain_divergence
            && info.refused_reorg_depth.is_none()
        {
            BitcoinImportStage::Ready { wallet }
        } else {
            BitcoinImportStage::Syncing { wallet }
        };
        let changed = intent.stage != stage;
        intent.stage = stage;
        Ok(changed)
    };
    // A refresh with unchanged readiness should not rewrite settings. Validate
    // identity even on this fast path; returning None from the settings writer
    // would delete the settings file, not skip a write.
    let mut snapshot = Settings::from_file(directory).map_err(|e| e.to_string())?;
    if !update(&mut snapshot)? {
        return Ok(());
    }
    settings::update_settings_file_checked(directory, |mut settings| {
        // Revalidate against the state held under the settings write lock.
        update(&mut settings).map_err(SettingsError::WritingFile)?;
        Ok(Some(settings))
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
