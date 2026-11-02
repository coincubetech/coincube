//! Clean-device password-v3 restore coordinator. The public wallet installer
//! can crash independently of the device-only signer anchor, so a durable
//! non-secret intent blocks bound signing until both have been reconciled.

use std::{fs, io::Write, path::Path};

use coincubed::config::BitcoinBackend;
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    app::{
        self,
        settings::{ImportedWalletAttachment, Settings},
    },
    dir::NetworkDirectory,
    services::{
        bitcoin_import::{self, RestoredKitSource},
        coincube::{imported_checkpoint::Checkpoint, CoincubeClient, CubeResponse},
        duress::DuressLocalState,
        imported_inventory_anchor as anchor,
        imported_keys::{
            restore::restore_intent_path, retire_proven_empty_unanchored, LocalKeyRecord,
            LocalKeyRepository,
        },
    },
};

use super::{
    imported_checkpoint::{fetch_verified_password_kit, VerifiedDownload},
    imported_descriptor_blob_fingerprint,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestoreIntent {
    pub(crate) version: u8,
    pub(crate) cube_id: u64,
    pub(crate) cube_uuid: String,
    pub(crate) operation_id: Uuid,
    pub(crate) wallet_id: String,
    pub(crate) public_digest: String,
    pub(crate) source_inventory_generation: u64,
    pub(crate) checkpoint_generation: u64,
    pub(crate) checkpoint_commitment: String,
}

/// A point-in-time result, never a durable signing grant. Every use of a
/// restored key must pass `LocalKeyRepository::sign_restored` and its release
/// check against current Connect state.
pub struct RestoreAdmissionReceipt {
    records: Vec<LocalKeyRecord>,
    checkpoint_generation: u64,
    checkpoint_commitment: String,
}

impl RestoreAdmissionReceipt {
    /// Exact admitted record/revision pairs. Safe for display, never signer
    /// authority; signer use rechecks the local anchor and Connect checkpoint.
    pub fn record_revisions(&self) -> impl Iterator<Item = (Uuid, Uuid)> + '_ {
        self.records
            .iter()
            .map(|record| (record.record_id, record.revision))
    }

    pub fn checkpoint_generation(&self) -> u64 {
        self.checkpoint_generation
    }

    pub fn checkpoint_commitment(&self) -> &str {
        &self.checkpoint_commitment
    }
}

fn preflight(directory: &NetworkDirectory, download: &VerifiedDownload) -> Result<(), String> {
    let kit = download.kit();
    kit.validate()?;
    let chain = kit.public.wallet.descriptor.chain();
    if directory.path().file_name() != Some(std::ffi::OsStr::new(chain.dir_name())) {
        return Err("Imported restore network directory differs from its Kit.".into());
    }
    let settings = Settings::from_file(directory).map_err(|_| "Cannot inspect restore Cube.")?;
    let mut cubes = settings
        .cubes
        .iter()
        .filter(|cube| cube.id == kit.public.cube.uuid);
    let cube = cubes.next().ok_or("The restore Cube does not exist.")?;
    if cubes.next().is_some() || cube.network != chain || cube.vault_wallet_id.is_some() {
        return Err("The restore Cube or chain conflicts with this Kit.".into());
    }
    if app::session::current_cube_id().as_deref() != Some(&cube.id)
        || app::session::seed_file_password(cube).is_none()
    {
        return Err("Unlock the exact restore Cube before importing keys.".into());
    }
    let root = directory.path().parent().ok_or("Missing Cube data root.")?;
    let duress = DuressLocalState::load(root).map_err(|_| "Cannot check Duress Mode.")?;
    if duress.enrolled || duress.arming || duress.active {
        return Err("Imported keys are unavailable during Duress Mode.".into());
    }
    let attachment = ImportedWalletAttachment::new(
        kit.public.wallet.descriptor.clone(),
        chain,
        kit.public.wallet.birthday,
        kit.public.wallet.initial_lookahead,
    )?;
    if attachment.wallet_id.to_string() != kit.wallet_id {
        return Err("Imported Kit wallet identity changed.".into());
    }
    let existing = cube.bitcoin_import.as_ref();
    if existing.is_some_and(|intent| {
        intent.operation_id != kit.operation_id
            || intent
                .attachment()
                .is_some_and(|present| present != &attachment)
    }) {
        return Err("The Cube has a conflicting imported wallet.".into());
    }
    let wallet_count = settings
        .wallets
        .iter()
        .filter(|wallet| wallet.wallet_id() == attachment.wallet_id)
        .count();
    if wallet_count > 1 || (wallet_count == 1 && existing.is_none()) {
        return Err("An existing wallet conflicts with this restore.".into());
    }
    if conflicting_key_root(directory, kit.operation_id, existing.is_some())? {
        return Err("Existing imported key state conflicts with this restore.".into());
    }
    Ok(())
}

fn conflicting_key_root(
    directory: &NetworkDirectory,
    operation_id: Uuid,
    existing_import: bool,
) -> Result<bool, String> {
    let key_root = directory
        .path()
        .join("imported-local-keys")
        .join(operation_id.to_string());
    let prior_key_root = match fs::symlink_metadata(&key_root) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err("Cannot inspect existing imported key state.".into()),
    };
    Ok(prior_key_root
        && (!existing_import
            || !fs::symlink_metadata(restore_intent_path(directory, operation_id))
                .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())))
}

fn write_intent(path: &Path, intent: &RestoreIntent) -> Result<(), String> {
    let bytes = serde_json::to_vec(intent).map_err(|_| "Cannot encode imported restore intent.")?;
    let parent = path.parent().ok_or("Missing imported restore directory.")?;
    crate::dir::create_directory(parent)
        .map_err(|_| "Cannot create imported restore directory.")?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(&bytes)
                .map_err(|_| "Cannot write imported restore intent.")?;
            file.sync_all()
                .map_err(|_| "Cannot sync imported restore intent.")?;
            fs::File::open(parent)
                .and_then(|dir| dir.sync_all())
                .map_err(|_| "Cannot sync imported restore directory.")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata =
                fs::symlink_metadata(path).map_err(|_| "Cannot inspect restore intent.")?;
            if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
                return Err("Unsafe imported restore intent.".into());
            }
            let existing = fs::read(path).map_err(|_| "Cannot read restore intent.")?;
            // A crash during create-new/write precedes every public mutation.
            // Under the operation lock, an exact incomplete prefix can safely
            // be discarded and retried; a different complete intent cannot.
            if existing.len() < bytes.len() && bytes.starts_with(&existing) {
                fs::remove_file(path).map_err(|_| "Cannot discard incomplete restore intent.")?;
                fs::File::open(parent)
                    .and_then(|dir| dir.sync_all())
                    .map_err(|_| "Cannot sync restore intent cleanup.")?;
                return write_intent(path, intent);
            }
            let decoded: RestoreIntent = serde_json::from_slice(&existing)
                .map_err(|_| "Invalid imported restore intent.")?;
            if decoded != *intent || existing != bytes {
                return Err("A different imported restore already owns this operation.".into());
            }
        }
        Err(_) => return Err("Cannot create imported restore intent.".into()),
    }
    Ok(())
}

pub(crate) fn lock_restore_intent(path: &Path) -> Result<fs::File, String> {
    let parent = path.parent().ok_or("Missing imported restore directory.")?;
    crate::dir::create_directory(parent)
        .map_err(|_| "Cannot create imported restore directory.")?;
    let name = path
        .file_name()
        .ok_or("Missing imported restore intent name.")?;
    let lock_path = parent.join(format!("{}.lock", name.to_string_lossy()));
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&lock_path)
        .map_err(|_| "Cannot open imported restore lock.")?;
    let metadata = fs::symlink_metadata(&lock_path).map_err(|_| "Cannot inspect restore lock.")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Unsafe imported restore lock.".into());
    }
    file.lock_exclusive()
        .map_err(|_| "Cannot lock imported restore operation.")?;
    Ok(file)
}

pub(crate) fn read_intent(path: &Path) -> Result<Option<RestoreIntent>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot inspect imported restore intent.".into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
        return Err("Unsafe imported restore intent.".into());
    }
    let bytes = fs::read(path).map_err(|_| "Cannot read imported restore intent.")?;
    let intent: RestoreIntent =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid imported restore intent.")?;
    if intent.version != 1
        || serde_json::to_vec(&intent).map_err(|_| "Cannot encode imported restore intent.")?
            != bytes
    {
        return Err("Invalid imported restore intent.".into());
    }
    Ok(Some(intent))
}

pub(crate) fn clear_intent(path: &Path, intent: &RestoreIntent) -> Result<(), String> {
    let expected =
        serde_json::to_vec(intent).map_err(|_| "Cannot encode imported restore intent.")?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "Cannot inspect restore intent.")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 4096 {
        return Err("Unsafe imported restore intent.".into());
    }
    if fs::read(path).map_err(|_| "Cannot read imported restore intent.")? != expected {
        return Err("Imported restore intent changed before completion.".into());
    }
    fs::remove_file(path).map_err(|_| "Cannot finish imported restore intent.")?;
    let parent = path.parent().ok_or("Missing imported restore directory.")?;
    fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| "Cannot sync completed restore intent.".into())
}

fn release_owner_network(
    directory: &NetworkDirectory,
    intent: &RestoreIntent,
) -> Result<String, String> {
    let settings = Settings::from_file(directory).map_err(|_| "Cannot inspect restore Cube.")?;
    let mut cubes = settings
        .cubes
        .iter()
        .filter(|cube| cube.id == intent.cube_uuid);
    let cube = cubes.next().ok_or("The restore Cube does not exist.")?;
    if cubes.next().is_some()
        || directory.path().file_name() != Some(std::ffi::OsStr::new(cube.network.dir_name()))
        || cube.bitcoin_import.as_ref().is_some_and(|import| {
            import.operation_id != intent.operation_id
                || import
                    .attachment()
                    .is_some_and(|attachment| attachment.wallet_id.to_string() != intent.wallet_id)
        })
    {
        return Err("The restore Cube or import operation changed.".into());
    }
    if app::session::current_cube_id().as_deref() != Some(&cube.id)
        || app::session::seed_file_password(cube).is_none()
    {
        return Err("Unlock the exact restore Cube before release.".into());
    }
    let root = directory.path().parent().ok_or("Missing Cube data root.")?;
    let duress = DuressLocalState::load(root).map_err(|_| "Cannot check Duress Mode.")?;
    if duress.enrolled || duress.arming || duress.active {
        return Err("Imported keys are unavailable during Duress Mode.".into());
    }
    Ok(cube.network.api_str().into())
}

/// Reconcile only an intent whose operation never acquired a device anchor.
/// The caller holds its operation lock. No Kit plaintext or public attachment
/// is required, and no signing key is admitted from the intent file.
pub(crate) fn release_proven_empty_intent_locked(
    directory: &NetworkDirectory,
    path: &Path,
    intent: &RestoreIntent,
    cube: &CubeResponse,
    remote: &Checkpoint,
    is_current: impl Fn() -> bool,
) -> Result<(), String> {
    if !is_current() || intent.operation_id.is_nil() || intent.cube_id == 0 {
        return Err("Imported restore release was cancelled or invalid.".into());
    }
    let network = release_owner_network(directory, intent)?;
    remote
        .validate()
        .map_err(|_| "Invalid restored checkpoint revocation proof.")?;
    if cube.id != intent.cube_id
        || cube.uuid != intent.cube_uuid
        || cube.network != network
        || cube.status != "active"
        || remote.active
        || remote.generation <= intent.checkpoint_generation
    {
        return Err("Restore intent has no later inactive owner-scoped epoch.".into());
    }
    retire_proven_empty_unanchored(
        directory,
        &intent.cube_uuid,
        &network,
        intent.operation_id,
        &intent.wallet_id,
    )?;
    if !is_current() || release_owner_network(directory, intent)? != network {
        return Err("Imported restore release changed before intent clearance.".into());
    }
    clear_intent(path, intent)
}

/// Owner-scoped cleanup for a revoked v3 restore that stopped before an
/// attachment or first device anchor existed. The current Connect tombstone
/// must be strictly later than the durable intent's source generation.
pub async fn release_revoked_password_v3_intent(
    directory: &NetworkDirectory,
    client: &CoincubeClient,
    cube_uuid: &str,
    operation_id: Uuid,
    is_current: impl Fn() -> bool,
) -> Result<(), String> {
    if !is_current() || operation_id.is_nil() {
        return Err("Imported restore release was cancelled or invalid.".into());
    }
    let path = restore_intent_path(directory, operation_id);
    let _operation_lock = lock_restore_intent(&path)?;
    let intent = read_intent(&path)?.ok_or("No durable imported restore intent to release.")?;
    if intent.operation_id != operation_id || intent.cube_uuid != cube_uuid {
        return Err("Restore intent belongs to another Cube or operation.".into());
    }
    release_owner_network(directory, &intent)?;
    let cube = client
        .get_imported_checkpoint_cube(intent.cube_id)
        .await
        .map_err(|_| "Cannot verify restored Cube owner for release.")?;
    let remote = client
        .get_imported_checkpoint(intent.cube_id)
        .await
        .map_err(|_| "Cannot verify restored checkpoint revocation.")?;
    release_proven_empty_intent_locked(directory, &path, &intent, &cube, &remote, is_current)
}

pub(crate) async fn restore_verified_password_kit(
    directory: &NetworkDirectory,
    client: &CoincubeClient,
    download: VerifiedDownload,
    backend: BitcoinBackend,
    is_current: impl Fn() -> bool,
) -> Result<RestoreAdmissionReceipt, String> {
    restore_verified_password_kit_with_anchor_support(
        directory,
        client,
        download,
        backend,
        is_current,
        anchor::supported(),
    )
    .await
}

async fn restore_verified_password_kit_with_anchor_support(
    directory: &NetworkDirectory,
    client: &CoincubeClient,
    download: VerifiedDownload,
    backend: BitcoinBackend,
    is_current: impl Fn() -> bool,
    anchor_supported: bool,
) -> Result<RestoreAdmissionReceipt, String> {
    if !is_current() {
        return Err("Imported restore was cancelled.".into());
    }
    // Reject unsupported platforms before creating the operation lock,
    // durable intent, or public wallet.
    if !anchor_supported {
        return Err(
            "Imported restore requires a verified device keystore adapter on this platform.".into(),
        );
    }
    let path = restore_intent_path(directory, download.kit().operation_id);
    let _operation_lock = lock_restore_intent(&path)?;
    preflight(directory, &download)?;
    let kit = download.kit();
    let cube_id = download.cube_id();
    let cube = client
        .get_imported_checkpoint_cube(cube_id)
        .await
        .map_err(|_| "Cannot verify Connect Cube for restore.")?;
    if cube.id != cube_id
        || cube.uuid != kit.public.cube.uuid
        || cube.network != kit.public.cube.network
        || cube.status != "active"
    {
        return Err("Connect Cube differs from this imported Kit.".into());
    }
    download
        .recheck(client)
        .await
        .map_err(|_| "Imported checkpoint changed before restore.")?;
    if !is_current() {
        return Err("Imported restore was cancelled.".into());
    }
    let checkpoint = download.checkpoint();
    let intent = RestoreIntent {
        version: 1,
        cube_id,
        cube_uuid: kit.public.cube.uuid.clone(),
        operation_id: kit.operation_id,
        wallet_id: kit.wallet_id.clone(),
        public_digest: imported_descriptor_blob_fingerprint(&kit.public)
            .ok_or("Cannot authenticate imported public wallet.")?,
        source_inventory_generation: kit.inventory_generation,
        checkpoint_generation: checkpoint.generation,
        checkpoint_commitment: checkpoint.commitment.clone(),
    };
    write_intent(&path, &intent)?; // Durable before any public wallet mutation.
    let public = kit.public.clone(); // Public metadata only; no signer secret is cloned.
    bitcoin_import::restore_from_kit(
        directory,
        &intent.cube_uuid,
        &public,
        backend,
        RestoredKitSource::PasswordV3,
    )
    .await?;
    download
        .recheck(client)
        .await
        .map_err(|_| "Imported checkpoint changed during public restore.")?;
    if !is_current() {
        return Err("Imported restore was cancelled.".into());
    }
    let live = Settings::from_file(directory).map_err(|_| "Cannot reload restored Cube.")?;
    let cube = live
        .cubes
        .iter()
        .find(|cube| cube.id == intent.cube_uuid)
        .ok_or("Restored Cube disappeared.")?;
    let attachment = cube
        .bitcoin_import
        .as_ref()
        .and_then(|import| import.active_attachment())
        .ok_or("Restored imported wallet is not attached.")?;
    let repository = LocalKeyRepository::new(directory, cube, attachment)?;
    let current_public = repository
        .live_checkpoint_public()
        .map_err(|_| "Cannot authenticate installed imported wallet.")?;
    if imported_descriptor_blob_fingerprint(&current_public).as_deref()
        != Some(&intent.public_digest)
    {
        return Err("Installed wallet or address state differs from the Kit.".into());
    }
    let checkpoint = crate::services::coincube::imported_checkpoint::Checkpoint {
        generation: intent.checkpoint_generation,
        commitment: intent.checkpoint_commitment.clone(),
        active: true,
    };
    let records = repository.admit_restored_batch(download, &is_current)?;
    repository
        .finish_restored_batch_verified(
            client,
            intent.cube_id,
            &checkpoint,
            intent.source_inventory_generation,
            &is_current,
        )
        .await?;
    let final_public = repository
        .live_checkpoint_public()
        .map_err(|_| "Cannot verify restored wallet after key admission.")?;
    if imported_descriptor_blob_fingerprint(&final_public).as_deref() != Some(&intent.public_digest)
    {
        return Err("Restored wallet or address state changed during admission.".into());
    }
    if !is_current() {
        return Err("Imported restore was cancelled.".into());
    }
    clear_intent(&path, &intent)?;
    Ok(RestoreAdmissionReceipt {
        records,
        checkpoint_generation: checkpoint.generation,
        checkpoint_commitment: checkpoint.commitment,
    })
}

/// Narrow backend entrypoint for a future P-Restore front-end. No decrypted
/// phrase enters a cloneable UI restore message.
pub async fn fetch_and_restore_password_v3(
    directory: &NetworkDirectory,
    client: &CoincubeClient,
    cube_id: u64,
    cube_uuid: &str,
    password: &Zeroizing<String>,
    backend: BitcoinBackend,
    is_current: impl Fn() -> bool,
) -> Result<RestoreAdmissionReceipt, String> {
    let download = fetch_verified_password_kit(client, cube_id, cube_uuid, password)
        .await
        .map_err(|_| "Cannot verify current imported key Kit.")?;
    restore_verified_password_kit(directory, client, download, backend, is_current).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unsupported_anchor_rejects_restore_before_intent_or_public_wallet() {
        let root =
            std::env::temp_dir().join(format!("imported-restore-unsupported-{}", Uuid::new_v4()));
        let directory = NetworkDirectory::new(root.join("bitcoin"));
        directory.init().unwrap();
        let kit = super::super::imported_key_v3::tests::sample();
        let path = restore_intent_path(&directory, kit.operation_id);
        let download = VerifiedDownload::for_test(
            42,
            Checkpoint {
                generation: 1,
                commitment: "a".repeat(64),
                active: true,
            },
            kit,
        );
        let client = CoincubeClient::for_test("http://127.0.0.1:1");
        let backend = BitcoinBackend::Electrum(coincubed::config::ElectrumConfig {
            addr: "tcp://127.0.0.1:50001".into(),
            validate_domain: true,
        });
        let result = restore_verified_password_kit_with_anchor_support(
            &directory,
            &client,
            download,
            backend,
            || true,
            false,
        )
        .await;
        assert!(matches!(result, Err(ref error) if error.contains("device keystore adapter")));
        assert!(
            !path.exists(),
            "unsupported Restore must not write an intent"
        );
        assert!(
            !path.parent().unwrap().exists(),
            "unsupported Restore must not create an operation lock"
        );
        assert!(
            !directory.path().join("settings.json").exists(),
            "unsupported Restore must not attach a public wallet"
        );
        assert_eq!(
            fs::read_dir(directory.path().join("data")).unwrap().count(),
            0,
            "unsupported Restore must not install wallet data"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_intent_recovers_partial_write_and_rejects_conflicting_retry() {
        let root = std::env::temp_dir().join(format!("imported-restore-intent-{}", Uuid::new_v4()));
        let directory = NetworkDirectory::new(root.join("regtest"));
        directory.init().unwrap();
        let operation_id = Uuid::new_v4();
        let path = restore_intent_path(&directory, operation_id);
        let intent = RestoreIntent {
            version: 1,
            cube_id: 42,
            cube_uuid: "synthetic-cube".into(),
            operation_id,
            wallet_id: "synthetic-wallet".into(),
            public_digest: "a".repeat(64),
            source_inventory_generation: 7,
            checkpoint_generation: 3,
            checkpoint_commitment: "b".repeat(64),
        };
        let _lock = lock_restore_intent(&path).unwrap();
        fs::write(&path, &serde_json::to_vec(&intent).unwrap()[..24]).unwrap();
        write_intent(&path, &intent).unwrap();
        write_intent(&path, &intent).unwrap();
        let mut other = intent.clone();
        other.checkpoint_generation = 4;
        assert!(write_intent(&path, &other).is_err());
        clear_intent(&path, &intent).unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_same_operation_key_root_requires_durable_restore_intent() {
        let root = std::env::temp_dir().join(format!("imported-restore-keys-{}", Uuid::new_v4()));
        let directory = NetworkDirectory::new(root.join("regtest"));
        directory.init().unwrap();
        let operation_id = Uuid::new_v4();
        let key_root = directory
            .path()
            .join("imported-local-keys")
            .join(operation_id.to_string());
        fs::create_dir_all(&key_root).unwrap();
        assert!(conflicting_key_root(&directory, operation_id, true).unwrap());
        let intent = restore_intent_path(&directory, operation_id);
        fs::create_dir_all(intent.parent().unwrap()).unwrap();
        fs::write(&intent, b"pending").unwrap();
        assert!(!conflicting_key_root(&directory, operation_id, true).unwrap());
        assert!(conflicting_key_root(&directory, operation_id, false).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
