//! Operation-scoped encrypted local keys for an imported Bitcoin wallet.
//!
//! An immutable ciphertext is written and synced before its public manifest is
//! atomically replaced. The manifest is the only admission point. An
//! undeclared ciphertext causes inventory and signing to fail closed; it is
//! never deleted based on this unauthenticated public manifest. No mnemonic
//! enters Cube Settings. A whole-operation-directory rollback is not detected
//! here; an external authenticated anchor and recovery protocol are required
//! before Keys, ordinary Send, or Recovery Kit may consume this repository.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

use coincube_core::{
    imported_key_crypt::{self, ImportedKeyBinding, ImportedKeySecret, Protection},
    imported_keys::{verify_imported_account, ImportedAccount, VerifiedImportedKey},
    miniscript::bitcoin::{
        bip32::{DerivationPath, Fingerprint, Xpub},
        hashes::{sha256, Hash},
        psbt::Psbt,
    },
};
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    app::{
        self,
        settings::{CubeSettings, ImportedWalletAttachment, Settings},
    },
    dir::NetworkDirectory,
    phone_signer::policy::PhonePolicy,
    services::{duress::DuressLocalState, unlock::device_secret},
};

const MANIFEST: &str = "manifest.json";
const LOCK: &str = "imported-keys.lock";
const MAX_RECORDS: usize = 32;
// R15b.1's encrypted record frame uses version 2 for device-bound protection.
// This repository always writes version 2 and refuses a replayed PIN-only frame.
const DEVICE_BOUND_FORMAT_VERSION: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalKeyRecord {
    pub record_id: Uuid,
    pub revision: Uuid,
    pub fingerprint: String,
    pub origin: String,
    pub xpub: String,
}

impl LocalKeyRecord {
    fn account(&self) -> Result<ImportedAccount, String> {
        Ok(ImportedAccount {
            fingerprint: Fingerprint::from_str(&self.fingerprint)
                .map_err(|_| "Invalid imported account fingerprint.")?,
            origin: DerivationPath::from_str(&self.origin)
                .map_err(|_| "Invalid imported account origin.")?,
            xpub: Xpub::from_str(&self.xpub).map_err(|_| "Invalid imported account xpub.")?,
        })
    }

    fn from_account(account: &ImportedAccount, record_id: Uuid, revision: Uuid) -> Self {
        Self {
            record_id,
            revision,
            fingerprint: account.fingerprint.to_string(),
            origin: account.origin.to_string(),
            xpub: account.xpub.to_string(),
        }
    }

    fn file_name(&self) -> String {
        format!("{}-{}.key", self.record_id, self.revision)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    cube_id: String,
    chain: String,
    operation_id: Uuid,
    wallet_id: String,
    policy_hash: String,
    records: Vec<LocalKeyRecord>,
}

/// The public identity is recomputed from the canonical D1 transport bytes on
/// every operation. A short transport fingerprint is never used as authority.
pub struct LocalKeyRepository<'a> {
    directory: &'a NetworkDirectory,
    cube: &'a CubeSettings,
    attachment: &'a ImportedWalletAttachment,
    operation_id: Uuid,
    policy_hash: [u8; 32],
}

impl<'a> LocalKeyRepository<'a> {
    pub fn new(
        directory: &'a NetworkDirectory,
        cube: &'a CubeSettings,
        attachment: &'a ImportedWalletAttachment,
    ) -> Result<Self, String> {
        let import = cube
            .bitcoin_import
            .as_ref()
            .ok_or("No Bitcoin import for this Cube.")?;
        if import.attachment() != Some(attachment)
            || directory.path().file_name() != Some(std::ffi::OsStr::new(cube.network.dir_name()))
            || import.operation_id.is_nil()
            || cube.network.is_blake2b()
        {
            return Err("Imported key scope does not match this Cube.".into());
        }
        attachment.validate(cube.network)?;
        let policy = PhonePolicy::imported(
            attachment.descriptor.clone(),
            cube.network,
            attachment.wallet_id.clone(),
        )?;
        Ok(Self {
            directory,
            cube,
            attachment,
            operation_id: import.operation_id,
            policy_hash: sha256::Hash::hash(policy.descriptor().as_bytes()).to_byte_array(),
        })
    }

    fn root(&self) -> PathBuf {
        self.directory
            .path()
            .join("imported-local-keys")
            .join(self.operation_id.to_string())
    }

    fn tombstone(&self) -> PathBuf {
        self.directory
            .path()
            .join("imported-local-keys")
            .join(format!(".removed-{}", self.operation_id))
    }

    fn is_removed(&self) -> Result<bool, String> {
        let path = self.tombstone();
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
            Err(_) => Err("Cannot inspect imported key removal marker.".into()),
            Ok(_) => {
                reject_non_file(&path)?;
                if fs::read(&path).map_err(|_| "Cannot read imported key removal marker.")?
                    != b"removed-v1"
                {
                    return Err("Invalid imported key removal marker.".into());
                }
                Ok(true)
            }
        }
    }

    fn manifest(&self, records: Vec<LocalKeyRecord>) -> Manifest {
        Manifest {
            version: 1,
            cube_id: self.cube.id.clone(),
            chain: self.cube.network.api_str().into(),
            operation_id: self.operation_id,
            wallet_id: self.attachment.wallet_id.to_string(),
            policy_hash: hex::encode(self.policy_hash),
            records,
        }
    }

    fn check_active(&self) -> Result<Zeroizing<String>, String> {
        if self.is_removed()? {
            return Err("This imported key operation was removed.".into());
        }
        let settings = Settings::from_file(self.directory)
            .map_err(|_| "Cannot verify the current Cube import.")?;
        let live = settings
            .cubes
            .iter()
            .find(|cube| cube.id == self.cube.id)
            .ok_or("The Cube for this imported key no longer exists.")?;
        if live.network != self.cube.network
            || live.bitcoin_import.as_ref().is_none_or(|import| {
                import.operation_id != self.operation_id
                    || import.attachment() != Some(self.attachment)
            })
        {
            return Err("The Cube import changed; its local keys are unavailable.".into());
        }
        if app::session::current_cube_id().as_deref() != Some(&self.cube.id) {
            return Err("Unlock this Cube before using imported keys.".into());
        }
        let root = self
            .directory
            .path()
            .parent()
            .ok_or("Missing Cube data root.")?;
        let duress = DuressLocalState::load(root).map_err(|_| "Cannot check Duress Mode.")?;
        if duress.enrolled || duress.arming || duress.active {
            return Err("Imported keys are unavailable during Duress Mode.".into());
        }
        app::session::seed_file_password(self.cube)
            .ok_or_else(|| "The Cube unlock session has expired.".into())
    }

    fn device(&self, create: bool) -> Result<coincube_core::seed_crypt::DeviceSecret, String> {
        let root = self
            .directory
            .path()
            .parent()
            .ok_or("Missing Cube data root.")?;
        if create {
            device_secret::get_or_create(root, &self.cube.id)
        } else {
            device_secret::load(&self.cube.id)
        }
        .map_err(|_| "The Cube device secret is unavailable.".into())
    }

    fn binding(&self, record: &LocalKeyRecord) -> Result<ImportedKeyBinding, String> {
        let account = record.account()?;
        ImportedKeyBinding::new(
            &self.cube.id,
            &self.attachment.descriptor,
            *self.operation_id.as_bytes(),
            self.policy_hash,
            &account,
            *record.record_id.as_bytes(),
            *record.revision.as_bytes(),
        )
        .map_err(|_| "Imported record binding does not match this wallet.".into())
    }

    fn ensure_root(&self) -> Result<(), String> {
        let parent = self.directory.path().join("imported-local-keys");
        for path in [&parent, &self.root()] {
            match fs::symlink_metadata(path) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if meta.permissions().mode() & 0o077 != 0 {
                            return Err("Imported key directory permissions are unsafe.".into());
                        }
                    }
                }
                Ok(_) => return Err("Imported key storage path is unsafe.".into()),
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    #[cfg(unix)]
                    let result = {
                        use std::os::unix::fs::DirBuilderExt;
                        fs::DirBuilder::new().mode(0o700).create(path)
                    };
                    #[cfg(not(unix))]
                    let result = fs::create_dir(path);
                    result.map_err(|_| "Cannot create imported key storage.")?;
                }
                Err(_) => return Err("Cannot inspect imported key storage.".into()),
            }
        }
        Ok(())
    }

    fn lock(&self) -> Result<File, String> {
        if self.is_removed()? {
            return Err("This imported key operation was removed.".into());
        }
        self.ensure_root()?;
        let path = self.root().join(LOCK);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        reject_non_file(&path)?;
        let file = options
            .open(&path)
            .map_err(|_| "Cannot open imported key lock.")?;
        reject_non_file(&path)?;
        file.lock_exclusive()
            .map_err(|_| "Cannot lock imported key repository.")?;
        if self.is_removed()? {
            return Err("This imported key operation was removed.".into());
        }
        Ok(file)
    }

    fn read_manifest(&self) -> Result<Manifest, String> {
        let path = self.root().join(MANIFEST);
        reject_non_file(&path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                for entry in
                    fs::read_dir(self.root()).map_err(|_| "Cannot inspect imported key storage.")?
                {
                    let entry = entry.map_err(|_| "Cannot inspect imported key storage.")?;
                    if entry.file_name().to_string_lossy().ends_with(".key") {
                        return Err("Imported key manifest is missing for existing records.".into());
                    }
                }
                return Ok(self.manifest(vec![]));
            }
            Err(_) => return Err("Cannot read imported key manifest.".into()),
        };
        if !file
            .metadata()
            .map_err(|_| "Cannot inspect imported key manifest.")?
            .is_file()
        {
            return Err("Imported key manifest endpoint is unsafe.".into());
        }
        let mut bytes = Vec::with_capacity(16_385);
        file.take(16_385)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read imported key manifest.")?;
        if bytes.len() > 16_384 {
            return Err("Imported key manifest is oversized.".into());
        }
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid imported key manifest.")?;
        if serde_json::to_vec(&manifest).ok().as_deref() != Some(bytes.as_slice()) {
            return Err("Imported key manifest has a noncanonical or duplicate field.".into());
        }
        let expected = self.manifest(manifest.records.clone());
        if manifest.version != expected.version
            || manifest.cube_id != expected.cube_id
            || manifest.chain != expected.chain
            || manifest.operation_id != expected.operation_id
            || manifest.wallet_id != expected.wallet_id
            || manifest.policy_hash != expected.policy_hash
            || manifest.records.len() > MAX_RECORDS
        {
            return Err("Imported key manifest belongs to another authority.".into());
        }
        let mut ids = HashSet::new();
        let mut accounts = HashSet::new();
        for record in &manifest.records {
            self.binding(record)?;
            if record.record_id.is_nil()
                || record.revision.is_nil()
                || !ids.insert(record.record_id)
                || !accounts.insert((
                    record.fingerprint.clone(),
                    record.origin.clone(),
                    record.xpub.clone(),
                ))
            {
                return Err("Duplicate or invalid imported key record.".into());
            }
        }
        self.check_inventory(&manifest)?;
        Ok(manifest)
    }

    fn check_inventory(&self, manifest: &Manifest) -> Result<(), String> {
        let declared: HashSet<_> = manifest
            .records
            .iter()
            .map(LocalKeyRecord::file_name)
            .collect();
        let mut present = HashSet::new();
        for entry in
            fs::read_dir(self.root()).map_err(|_| "Cannot inspect imported key storage.")?
        {
            let entry = entry.map_err(|_| "Cannot inspect imported key storage.")?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".key") {
                if !declared.contains(&name) {
                    return Err("Imported key inventory contains an undeclared record.".into());
                }
                present.insert(name);
            }
        }
        if present != declared {
            return Err("Imported key inventory is missing a declared record.".into());
        }
        Ok(())
    }

    fn read_key(
        &self,
        record: &LocalKeyRecord,
        credential: &str,
        device: &coincube_core::seed_crypt::DeviceSecret,
    ) -> Result<VerifiedImportedKey, String> {
        let path = self.root().join(record.file_name());
        let bytes = read_key_frame(&path)?;
        if bytes[imported_key_crypt::MARKER.len()] != DEVICE_BOUND_FORMAT_VERSION {
            return Err("Imported key protection was downgraded.".into());
        }
        let secret =
            imported_key_crypt::open(&bytes, credential, &self.binding(record)?, Some(device))
                .map_err(|_| "Imported key record could not be authenticated.")?;
        let (words, passphrase) = secret.into_parts();
        let key = verify_imported_account(
            &self.attachment.descriptor,
            &record.account()?.origin,
            words,
            passphrase,
        )
        .map_err(|_| "Imported key does not match both wallet branches.")?;
        if key.account() != &record.account()? {
            return Err("Imported key account identity changed.".into());
        }
        Ok(key)
    }

    /// Admit or replace one exact account. The caller supplies its current
    /// revision for replacement; a stale or missing expectation cannot win.
    /// The returned public record is safe for a future Keys UI or Kit manifest.
    pub fn admit(
        &self,
        account: &ImportedAccount,
        secret: ImportedKeySecret,
        expected_revision: Option<Uuid>,
        is_current: impl Fn() -> bool,
    ) -> Result<LocalKeyRecord, String> {
        if !is_current() {
            return Err("Imported key admission was cancelled.".into());
        }
        let credential = self.check_active()?;
        let _lock = self.lock()?;
        let manifest = self.read_manifest()?;
        if !self.root().join(MANIFEST).exists() {
            // Establish an empty durable inventory before any ciphertext is
            // staged. A later missing manifest must never silently discard
            // previously admitted records.
            write_replace(
                &self.root().join(MANIFEST),
                &serde_json::to_vec(&manifest).map_err(|_| "Cannot encode key manifest.")?,
            )?;
        }
        let device = self.device(manifest.records.is_empty())?;
        self.admit_locked(
            manifest,
            account,
            secret,
            expected_revision,
            &credential,
            &device,
            || self.check_active().is_ok() && is_current(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn admit_locked(
        &self,
        mut manifest: Manifest,
        account: &ImportedAccount,
        secret: ImportedKeySecret,
        expected_revision: Option<Uuid>,
        credential: &str,
        device: &coincube_core::seed_crypt::DeviceSecret,
        is_current: impl Fn() -> bool,
    ) -> Result<LocalKeyRecord, String> {
        // Corruption of any admitted sibling blocks mutation; never replace a
        // broken repository with a misleading partial inventory.
        for old in &manifest.records {
            self.read_key(old, credential, device)?;
        }
        let matching = manifest
            .records
            .iter()
            .position(|r| r.account().ok().as_ref() == Some(account));
        match (matching, expected_revision) {
            (None, None) if manifest.records.len() < MAX_RECORDS => {}
            (Some(index), Some(revision)) if manifest.records[index].revision == revision => {}
            _ => return Err("Imported key revision is stale or account already exists.".into()),
        }
        let verified = verify_imported_account(
            &self.attachment.descriptor,
            &account.origin,
            Zeroizing::new(secret.words().to_owned()),
            Zeroizing::new(secret.passphrase().to_owned()),
        )
        .map_err(|_| "The supplied words do not match this exact wallet account.")?;
        if verified.account() != account {
            return Err("The supplied words do not match this exact wallet account.".into());
        }
        let retired = matching.map(|index| manifest.records[index].clone());
        let record = LocalKeyRecord::from_account(
            account,
            matching.map_or_else(Uuid::new_v4, |index| manifest.records[index].record_id),
            Uuid::new_v4(),
        );
        let ciphertext = imported_key_crypt::seal(
            &secret,
            credential,
            &self.binding(&record)?,
            Protection::DeviceBound(device),
        )
        .map_err(|_| "Cannot encrypt imported key.")?;
        let staged = self.root().join(record.file_name());
        write_new(&staged, &ciphertext)?;
        if let Some(index) = matching {
            manifest.records[index] = record.clone();
        } else {
            manifest.records.push(record.clone());
        }
        if !is_current() {
            remove_staged(&staged)?;
            return Err("Imported key admission was cancelled.".into());
        }
        write_replace(
            &self.root().join(MANIFEST),
            &serde_json::to_vec(&manifest).map_err(|_| "Cannot encode key manifest.")?,
        )?;
        if let Some(retired) = retired {
            self.remove_retired(&retired)?;
        }
        Ok(record)
    }

    /// Decrypt and re-derive every admitted account before making one signer
    /// available to an operation-scoped callback. No root is cached globally.
    fn with_key<T>(
        &self,
        record_id: Uuid,
        expected_revision: Uuid,
        is_current: impl Fn() -> bool,
        use_key: impl FnOnce(&VerifiedImportedKey) -> Result<T, String>,
    ) -> Result<T, String> {
        if !is_current() {
            return Err("Imported signing was cancelled.".into());
        }
        let credential = self.check_active()?;
        let _lock = self.lock()?;
        let manifest = self.read_manifest()?;
        let device = self.device(false)?;
        let mut result = None;
        let mut use_key = Some(use_key);
        for record in &manifest.records {
            let key = self.read_key(record, &credential, &device)?;
            if record.record_id == record_id && record.revision == expected_revision {
                result = Some(use_key.take().ok_or("Duplicate imported key record.")?(
                    &key,
                )?);
            }
        }
        self.check_active()?;
        if !is_current() {
            return Err("Imported signing was cancelled.".into());
        }
        result.ok_or_else(|| "Imported key record is missing or stale.".into())
    }

    pub fn sign(
        &self,
        record_id: Uuid,
        expected_revision: Uuid,
        original: &Psbt,
        current: &Psbt,
        is_current: impl Fn() -> bool,
    ) -> Result<Psbt, String> {
        self.with_key(record_id, expected_revision, is_current, |key| {
            key.sign(&self.attachment.descriptor, original, current)
                .map_err(|_| "Imported signing failed verification.".into())
        })
    }

    pub fn records(&self) -> Result<Vec<LocalKeyRecord>, String> {
        let credential = self.check_active()?;
        let _lock = self.lock()?;
        let manifest = self.read_manifest()?;
        if manifest.records.is_empty() {
            return Ok(vec![]);
        }
        let device = self.device(false)?;
        for record in &manifest.records {
            self.read_key(record, &credential, &device)?;
        }
        self.check_active()?;
        Ok(manifest.records)
    }

    pub fn remove(
        &self,
        record_id: Uuid,
        expected_revision: Uuid,
        is_current: impl Fn() -> bool,
    ) -> Result<(), String> {
        if !is_current() {
            return Err("Imported key removal was cancelled.".into());
        }
        let credential = self.check_active()?;
        let _lock = self.lock()?;
        let mut manifest = self.read_manifest()?;
        let device = self.device(false)?;
        for record in &manifest.records {
            self.read_key(record, &credential, &device)?;
        }
        let index = manifest
            .records
            .iter()
            .position(|r| r.record_id == record_id && r.revision == expected_revision)
            .ok_or("Imported key revision is stale or missing.")?;
        let retired = manifest.records.remove(index);
        self.check_active()?;
        if !is_current() {
            return Err("Imported key removal was cancelled.".into());
        }
        write_replace(
            &self.root().join(MANIFEST),
            &serde_json::to_vec(&manifest).map_err(|_| "Cannot encode key manifest.")?,
        )?;
        self.remove_retired(&retired)
    }

    fn remove_retired(&self, record: &LocalKeyRecord) -> Result<(), String> {
        let path = self.root().join(record.file_name());
        reject_non_file(&path)?;
        match fs::remove_file(&path) {
            Ok(()) => sync_parent(&path),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(_) => Err("Cannot remove the replaced imported key.".into()),
        }
    }

    /// Called by the import removal transaction after it has checked the
    /// wallet owner. A durable tombstone prevents an old in-flight admission
    /// from recreating this operation while Settings is being removed.
    pub(crate) fn remove_operation_storage(&self) -> Result<(), String> {
        if self.is_removed()? {
            match fs::symlink_metadata(self.root()) {
                Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                    fs::remove_dir_all(self.root())
                        .map_err(|_| "Cannot finish imported key removal.")?;
                    return sync_parent(&self.tombstone());
                }
                _ => return Err("Imported key storage path is unsafe.".into()),
            }
        }
        self.ensure_root()?;
        let lock = self.lock()?;
        let tombstone = self.tombstone();
        write_new(&tombstone, b"removed-v1")?;
        drop(lock);
        fs::remove_dir_all(self.root()).map_err(|_| "Cannot remove imported key storage.")?;
        sync_parent(&tombstone)
    }
}

/// Check an endpoint before reading. An absent path is permitted for initial
/// creation, while symlinks, directories and unusual file types are refused.
fn reject_non_file(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o077 != 0 {
                    return Err("Imported key file permissions are unsafe.".into());
                }
            }
            Ok(())
        }
        Ok(_) => Err("Imported key storage endpoint is unsafe.".into()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(_) => Err("Cannot inspect imported key storage endpoint.".into()),
    }
}

/// Read from one opened endpoint with a fixed upper bound. The file can
/// change after metadata inspection, so the actual returned length is the
/// authority before any frame byte is indexed.
fn read_key_frame(path: &Path) -> Result<Vec<u8>, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|_| "Imported key record is missing or unsafe.")?;
    let meta = file
        .metadata()
        .map_err(|_| "Cannot inspect imported key record.")?;
    if !meta.is_file() {
        return Err("Imported key record is not a regular file.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("Imported key file permissions are unsafe.".into());
        }
    }
    read_exact_frame(file)
}

fn read_exact_frame(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(imported_key_crypt::WIRE_LEN + 1);
    reader
        .take((imported_key_crypt::WIRE_LEN + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read imported key record.")?;
    if bytes.len() != imported_key_crypt::WIRE_LEN {
        return Err("Imported key record has an invalid length.".into());
    }
    Ok(bytes)
}

fn remove_staged(path: &Path) -> Result<(), String> {
    reject_non_file(path)?;
    fs::remove_file(path).map_err(|_| "Cannot discard cancelled imported key.")?;
    sync_parent(path)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "Cannot stage imported key.")?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|_| "Cannot write imported key.")?;
        file.sync_all().map_err(|_| "Cannot sync imported key.")?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn write_replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_file_name(format!(".manifest-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        write_new(&tmp, bytes)?;
        fs::rename(&tmp, path).map_err(|_| "Cannot publish imported key manifest.")?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

fn sync_parent(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(path.parent().ok_or("Missing imported key directory.")?)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| "Cannot sync imported key directory.")?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::settings::{BitcoinImport, BitcoinImportStage},
        chain::ChainId,
    };
    use coincube_core::miniscript::bitcoin::{bip32::Xpriv, secp256k1::Secp256k1, Network};
    use coincube_core::wallet_descriptor::ImportedWalletDescriptor;

    struct Fixture {
        root: PathBuf,
        directory: NetworkDirectory,
        cube: CubeSettings,
        attachment: ImportedWalletAttachment,
        phrases: Vec<String>,
        accounts: Vec<ImportedAccount>,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("imported-keys-{}", Uuid::new_v4()));
            let directory = NetworkDirectory::new(root.join("regtest"));
            directory.init().unwrap();
            let secp = Secp256k1::new();
            let path = DerivationPath::from_str("m/48h/1h/0h/2h").unwrap();
            let mut phrases = Vec::new();
            let mut keys = Vec::new();
            for seed in 1..=3 {
                let phrase = bip39::Mnemonic::from_entropy(&[seed; 16])
                    .unwrap()
                    .to_string();
                let root_key = Xpriv::new_master(
                    Network::Regtest,
                    &bip39::Mnemonic::parse(&phrase).unwrap().to_seed(""),
                )
                .unwrap();
                let xpub = Xpub::from_priv(&secp, &root_key.derive_priv(&secp, &path).unwrap());
                keys.push(format!(
                    "[{}/48h/1h/0h/2h]{xpub}",
                    root_key.fingerprint(&secp)
                ));
                phrases.push(phrase);
            }
            let branch = |index| {
                format!(
                    "wsh(sortedmulti(2,{}))",
                    keys.iter()
                        .map(|key| format!("{key}/{index}/*"))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            };
            let wallet =
                ImportedWalletDescriptor::parse(&branch(0), Some(&branch(1)), ChainId::Regtest)
                    .unwrap();
            let accounts = phrases
                .iter()
                .map(|phrase| {
                    verify_imported_account(
                        &wallet,
                        &path,
                        Zeroizing::new(phrase.clone()),
                        Zeroizing::new(String::new()),
                    )
                    .unwrap()
                    .account()
                    .clone()
                })
                .collect();
            let attachment =
                ImportedWalletAttachment::new(wallet, ChainId::Regtest, 0, 100).unwrap();
            let mut cube = CubeSettings::new("Synthetic".into(), ChainId::Regtest);
            cube.bitcoin_import = Some(Box::new(BitcoinImport {
                operation_id: Uuid::new_v4(),
                stage: BitcoinImportStage::Ready {
                    wallet: attachment.clone(),
                },
                last_error: None,
            }));
            Self {
                root,
                directory,
                cube,
                attachment,
                phrases,
                accounts,
            }
        }

        fn repository(&self) -> LocalKeyRepository<'_> {
            LocalKeyRepository::new(&self.directory, &self.cube, &self.attachment).unwrap()
        }

        fn save_cube(&self, cube: CubeSettings) {
            let settings = Settings {
                cubes: vec![cube],
                ..Settings::default()
            };
            fs::write(
                self.directory.path().join("settings.json"),
                serde_json::to_vec(&settings).unwrap(),
            )
            .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn two_independent_records_survive_repository_reopen() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        repository.ensure_root().unwrap();
        let device = Zeroizing::new([7u8; 32]);
        let mut records = Vec::new();
        for index in 0..2 {
            let record = LocalKeyRecord::from_account(
                &fixture.accounts[index],
                Uuid::new_v4(),
                Uuid::new_v4(),
            );
            let secret = ImportedKeySecret::new(
                Zeroizing::new(fixture.phrases[index].clone()),
                Zeroizing::new(String::new()),
            )
            .unwrap();
            let bytes = imported_key_crypt::seal(
                &secret,
                "synthetic unlock",
                &repository.binding(&record).unwrap(),
                Protection::DeviceBound(&device),
            )
            .unwrap();
            write_new(&repository.root().join(record.file_name()), &bytes).unwrap();
            records.push(record);
        }
        write_replace(
            &repository.root().join(MANIFEST),
            &serde_json::to_vec(&repository.manifest(records.clone())).unwrap(),
        )
        .unwrap();
        let reopened = fixture.repository();
        let manifest = reopened.read_manifest().unwrap();
        assert_eq!(manifest.records.len(), 2);
        // A same-scope empty public manifest cannot hide the still-encrypted
        // local records or authorize their deletion on a later admission.
        let original_manifest = fs::read(reopened.root().join(MANIFEST)).unwrap();
        write_replace(
            &reopened.root().join(MANIFEST),
            &serde_json::to_vec(&reopened.manifest(vec![])).unwrap(),
        )
        .unwrap();
        assert!(reopened.read_manifest().is_err());
        for record in &records {
            assert!(reopened.root().join(record.file_name()).exists());
        }
        write_replace(&reopened.root().join(MANIFEST), &original_manifest).unwrap();
        for (index, record) in manifest.records.iter().enumerate() {
            assert_eq!(
                reopened
                    .read_key(record, "synthetic unlock", &device)
                    .unwrap()
                    .account(),
                &fixture.accounts[index]
            );
            assert!(reopened.read_key(record, "wrong unlock", &device).is_err());
        }
        let wrong_device = Zeroizing::new([9u8; 32]);
        assert!(reopened
            .read_key(&records[0], "synthetic unlock", &wrong_device)
            .is_err());
        let pin_only = imported_key_crypt::seal(
            &ImportedKeySecret::new(
                Zeroizing::new(fixture.phrases[0].clone()),
                Zeroizing::new(String::new()),
            )
            .unwrap(),
            "synthetic unlock",
            &reopened.binding(&records[0]).unwrap(),
            Protection::PinOnlyNoKeystore,
        )
        .unwrap();
        write_replace(&reopened.root().join(records[0].file_name()), &pin_only).unwrap();
        assert!(reopened
            .read_key(&records[0], "synthetic unlock", &device)
            .is_err());
        fs::remove_file(reopened.root().join(MANIFEST)).unwrap();
        assert!(reopened.read_manifest().is_err());
    }

    #[test]
    fn manifest_rejects_duplicate_account_stale_scope_and_tamper() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        repository.ensure_root().unwrap();
        let first =
            LocalKeyRecord::from_account(&fixture.accounts[0], Uuid::new_v4(), Uuid::new_v4());
        let mut duplicate = first.clone();
        duplicate.record_id = Uuid::new_v4();
        write_replace(
            &repository.root().join(MANIFEST),
            &serde_json::to_vec(&repository.manifest(vec![first.clone(), duplicate])).unwrap(),
        )
        .unwrap();
        assert!(repository.read_manifest().is_err());

        let mutations: [fn(&mut Manifest); 3] = [
            |m: &mut Manifest| m.cube_id = "another-cube".into(),
            |m: &mut Manifest| m.chain = "bitcoin".into(),
            |m: &mut Manifest| m.operation_id = Uuid::new_v4(),
        ];
        for mutate in mutations {
            let mut wrong_scope = repository.manifest(vec![first.clone()]);
            mutate(&mut wrong_scope);
            write_replace(
                &repository.root().join(MANIFEST),
                &serde_json::to_vec(&wrong_scope).unwrap(),
            )
            .unwrap();
            assert!(repository.read_manifest().is_err());
        }

        let mut wrong_policy = repository.manifest(vec![first.clone()]);
        wrong_policy.policy_hash = hex::encode([8u8; 32]);
        write_replace(
            &repository.root().join(MANIFEST),
            &serde_json::to_vec(&wrong_policy).unwrap(),
        )
        .unwrap();
        assert!(repository.read_manifest().is_err());

        let canonical = serde_json::to_string(&repository.manifest(vec![first])).unwrap();
        let duplicated = canonical.replacen("{", "{\"version\":1,", 1);
        write_replace(&repository.root().join(MANIFEST), duplicated.as_bytes()).unwrap();
        assert!(repository.read_manifest().is_err());

        // The master fingerprint can match while a sibling account xpub does
        // not. Neither the public record nor its AEAD binding may accept it.
        let secp = Secp256k1::new();
        let root_key = Xpriv::new_master(
            Network::Regtest,
            &bip39::Mnemonic::parse(&fixture.phrases[0])
                .unwrap()
                .to_seed(""),
        )
        .unwrap();
        let sibling_path = DerivationPath::from_str("m/48h/1h/1h/2h").unwrap();
        let sibling_xpub =
            Xpub::from_priv(&secp, &root_key.derive_priv(&secp, &sibling_path).unwrap());
        let mut sibling_record =
            LocalKeyRecord::from_account(&fixture.accounts[0], Uuid::new_v4(), Uuid::new_v4());
        sibling_record.xpub = sibling_xpub.to_string();
        assert!(repository.binding(&sibling_record).is_err());
    }

    #[test]
    fn admission_replacement_stale_revision_and_cancel_preserve_sibling() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        repository.ensure_root().unwrap();
        let device = Zeroizing::new([11u8; 32]);
        let secret = |index: usize| {
            ImportedKeySecret::new(
                Zeroizing::new(fixture.phrases[index].clone()),
                Zeroizing::new(String::new()),
            )
            .unwrap()
        };
        let first = repository
            .admit_locked(
                repository.manifest(vec![]),
                &fixture.accounts[0],
                secret(0),
                None,
                "synthetic unlock",
                &device,
                || true,
            )
            .unwrap();
        let second = repository
            .admit_locked(
                repository.read_manifest().unwrap(),
                &fixture.accounts[1],
                secret(1),
                None,
                "synthetic unlock",
                &device,
                || true,
            )
            .unwrap();
        let replaced = repository
            .admit_locked(
                repository.read_manifest().unwrap(),
                &fixture.accounts[0],
                secret(0),
                Some(first.revision),
                "synthetic unlock",
                &device,
                || true,
            )
            .unwrap();
        assert_eq!(replaced.record_id, first.record_id);
        assert_ne!(replaced.revision, first.revision);
        assert!(repository
            .admit_locked(
                repository.read_manifest().unwrap(),
                &fixture.accounts[0],
                secret(0),
                Some(first.revision),
                "synthetic unlock",
                &device,
                || true,
            )
            .is_err());
        assert!(repository
            .admit_locked(
                repository.read_manifest().unwrap(),
                &fixture.accounts[2],
                secret(2),
                None,
                "synthetic unlock",
                &device,
                || false,
            )
            .is_err());
        let manifest = repository.read_manifest().unwrap();
        assert_eq!(manifest.records.len(), 2);
        assert!(manifest.records.contains(&replaced));
        assert!(manifest.records.contains(&second));
        assert!(!repository.root().join(first.file_name()).exists());
        let ciphertexts: HashSet<_> = fs::read_dir(repository.root())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".key"))
            .collect();
        assert_eq!(
            ciphertexts,
            HashSet::from([replaced.file_name(), second.file_name()])
        );
        assert_eq!(
            repository
                .read_key(&second, "synthetic unlock", &device)
                .unwrap()
                .account(),
            &fixture.accounts[1]
        );
    }

    #[test]
    fn removal_tombstone_blocks_recreating_old_operation() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        repository.remove_operation_storage().unwrap();
        assert!(repository.is_removed().unwrap());
        assert!(!repository.root().exists());
        repository.remove_operation_storage().unwrap();
        assert!(repository.lock().is_err());
    }

    #[test]
    fn owner_removal_accepts_corrupt_or_missing_inventory() {
        for corrupt_manifest in [true, false] {
            let fixture = Fixture::new();
            let repository = fixture.repository();
            repository.ensure_root().unwrap();
            write_new(&repository.root().join("orphan.key"), b"opaque ciphertext").unwrap();
            if corrupt_manifest {
                write_new(&repository.root().join(MANIFEST), b"{invalid").unwrap();
            }
            assert!(repository.read_manifest().is_err());
            repository.remove_operation_storage().unwrap();
            assert!(repository.is_removed().unwrap());
            assert!(!repository.root().exists());
        }
    }

    #[test]
    fn short_or_growing_record_read_is_bounded_and_never_indexes_short_data() {
        use std::io::Cursor;
        let len = imported_key_crypt::WIRE_LEN;
        assert!(read_exact_frame(Cursor::new(vec![0u8; len - 1])).is_err());
        assert!(read_exact_frame(std::io::repeat(0)).is_err());
        let fixture = Fixture::new();
        let repository = fixture.repository();
        repository.ensure_root().unwrap();
        let path = repository.root().join("short.key");
        write_new(&path, b"short").unwrap();
        assert!(read_key_frame(&path).is_err());
        fs::write(&path, vec![0u8; len + 1]).unwrap();
        assert!(read_key_frame(&path).is_err());
        #[cfg(unix)]
        {
            let link = repository.root().join("link.key");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(read_key_frame(&link).is_err());
        }
    }

    #[test]
    fn public_inventory_requires_current_cube_import_unlock_and_no_duress() {
        let _guard = app::session::test_guard();
        app::session::close();
        let fixture = Fixture::new();
        fixture.save_cube(fixture.cube.clone());
        let repository = fixture.repository();
        assert!(repository.records().is_err(), "locked Cube must refuse");

        app::session::open("another-cube", Zeroizing::new("synthetic PIN".into()));
        assert!(
            repository.records().is_err(),
            "wrong Cube session must refuse"
        );
        app::session::open(
            fixture.cube.id.clone(),
            Zeroizing::new("synthetic PIN".into()),
        );
        assert!(repository.records().unwrap().is_empty());

        let mut changed = fixture.cube.clone();
        changed.bitcoin_import.as_mut().unwrap().operation_id = Uuid::new_v4();
        fixture.save_cube(changed);
        assert!(
            repository.records().is_err(),
            "changed live operation must refuse"
        );
        fixture.save_cube(fixture.cube.clone());

        for state in [
            DuressLocalState {
                enrolled: true,
                ..DuressLocalState::default()
            },
            DuressLocalState {
                arming: true,
                ..DuressLocalState::default()
            },
            DuressLocalState {
                active: true,
                ..DuressLocalState::default()
            },
        ] {
            state.save(&fixture.root).unwrap();
            assert!(repository.records().is_err(), "Duress Mode must refuse");
        }
        DuressLocalState::default().save(&fixture.root).unwrap();
        assert!(repository.records().unwrap().is_empty());

        repository.remove_operation_storage().unwrap();
        assert!(repository.records().is_err(), "tombstone must refuse");
        app::session::close();
    }
}
