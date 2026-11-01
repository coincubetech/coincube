//! Operation-scoped encrypted local keys for an imported Bitcoin wallet.
//!
//! An immutable ciphertext is written and synced before its public manifest is
//! atomically replaced. The device-only inventory anchor is the admission
//! authority. An undeclared ciphertext causes inventory and signing to fail
//! closed; it is never deleted based on this unauthenticated public manifest.
//! No mnemonic enters Cube Settings. Keys, ordinary Send, and Recovery Kit are
//! separate integrations and do not consume this repository yet.

// Admission and signing are staged until the external inventory release gate.
#![allow(dead_code)]

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Mutex, MutexGuard, OnceLock},
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
    services::{
        duress::DuressLocalState,
        imported_inventory_anchor::{self as anchor, Anchor, Scope, State as AnchorState},
        unlock::device_secret,
    },
};

const MANIFEST: &str = "manifest.json";
const LOCK_PREFIX: &str = ".imported-key-anchor-";
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

struct RepositoryGuard {
    _process: MutexGuard<'static, ()>,
    _file: File,
}

fn process_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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

    fn anchor_scope(&self) -> Scope {
        Scope {
            cube_id: self.cube.id.clone(),
            chain: self.cube.network.api_str().into(),
            operation_id: self.operation_id,
            wallet_id: self.attachment.wallet_id.to_string(),
            policy_hash: self.policy_hash,
        }
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

    fn lock(&self) -> Result<RepositoryGuard, String> {
        self.lock_internal(false)
    }

    fn lock_internal(&self, allow_removed: bool) -> Result<RepositoryGuard, String> {
        if !allow_removed && self.is_removed()? {
            return Err("This imported key operation was removed.".into());
        }
        // This mutex carries no repository state. A panic releases the OS file
        // lock; recover its poisoned guard so later operations can revalidate
        // the on-disk and Keychain authorities instead of denying service forever.
        let process = process_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_root()?;
        // Operation deletion removes root(), so its lock must live in the
        // stable parent and must never be unlinked during operation removal.
        let path = self
            .directory
            .path()
            .join(format!("{LOCK_PREFIX}{}", self.operation_id));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        reject_non_file(&path)?;
        let file = options
            .open(&path)
            .map_err(|_| "Cannot open imported key lock.")?;
        reject_non_file(&path)?;
        FileExt::lock_exclusive(&file).map_err(|_| "Cannot lock imported key repository.")?;
        if !allow_removed && self.is_removed()? {
            return Err("This imported key operation was removed.".into());
        }
        Ok(RepositoryGuard {
            _process: process,
            _file: file,
        })
    }

    fn read_manifest(&self) -> Result<Manifest, String> {
        let mut authority = anchor::load(&self.anchor_scope())?.ok_or(
            "Imported key inventory has no device anchor; explicit migration is required.",
        )?;
        if matches!(authority.state, AnchorState::Pending { .. }) {
            authority = self.recover_pending(authority)?;
        }
        let (digest, retire_files, cleanup) = match &authority.state {
            AnchorState::Committed {
                digest,
                retire_files,
                cleanup_txn,
                ..
            } => (*digest, retire_files, *cleanup_txn),
            AnchorState::Pending { .. } => unreachable!("recovery returns a committed anchor"),
            AnchorState::Revoked { .. } => {
                return Err("This imported key operation was revoked.".into())
            }
        };
        if !self.root().join(MANIFEST).exists() {
            return Err("Anchored imported key manifest is missing.".into());
        }
        self.check_transaction_files(cleanup)?;
        let manifest = self.read_manifest_file(&self.root().join(MANIFEST), retire_files, false)?;
        let admitted: HashSet<_> = manifest
            .records
            .iter()
            .map(LocalKeyRecord::file_name)
            .collect();
        if retire_files.iter().any(|name| admitted.contains(name)) {
            return Err("Retired imported key is still admitted by the manifest.".into());
        }
        let bytes = serde_json::to_vec(&manifest).map_err(|_| "Cannot encode key manifest.")?;
        if anchor::digest(&bytes) != digest {
            return Err("Imported key manifest differs from its device anchor.".into());
        }
        if cleanup.is_some() {
            if let Err(error) = self.finish_cleanup(&authority) {
                log::warn!("Imported inventory cleanup remains pending: {error}");
            }
        }
        Ok(manifest)
    }

    fn read_manifest_recoverable(&self, _guard: &RepositoryGuard) -> Result<Manifest, String> {
        self.discard_proven_uncommitted_stage()?;
        self.read_manifest()
    }

    /// A Keychain read error can make the outcome of a pre-Pending write
    /// uncertain. Restore usability only when the old committed authority and
    /// both public manifests prove this was an uncommitted local candidate.
    /// Missing candidate identity, changed authority, or unauthenticated key
    /// files remain fail closed for owner-assisted recovery.
    fn discard_proven_uncommitted_stage(&self) -> Result<(), String> {
        self.discard_proven_uncommitted_stage_with(|| self.check_active(), || self.device(false))
    }

    fn discard_proven_uncommitted_stage_with(
        &self,
        credential: impl FnOnce() -> Result<Zeroizing<String>, String>,
        device: impl FnOnce() -> Result<coincube_core::seed_crypt::DeviceSecret, String>,
    ) -> Result<(), String> {
        let Some(authority) = anchor::load(&self.anchor_scope())? else {
            return Ok(());
        };
        let AnchorState::Committed {
            digest,
            cleanup_txn: None,
            retire_files,
            ..
        } = &authority.state
        else {
            return Ok(());
        };
        if !retire_files.is_empty() {
            return Ok(());
        }
        let mut txn = None;
        let mut has_prior = false;
        let mut has_prepared = false;
        let mut has_intent = false;
        let mut intent_tmp = None;
        let mut batch_tag = None;
        for entry in
            fs::read_dir(self.root()).map_err(|_| "Cannot inspect imported key storage.")?
        {
            let entry = entry.map_err(|_| "Cannot inspect imported key storage.")?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(raw) = name
                .strip_prefix(".intent-")
                .and_then(|rest| rest.strip_suffix(".tmp"))
            {
                let id = Uuid::parse_str(raw)
                    .map_err(|_| "Imported inventory has an unknown transaction file.")?;
                if id.is_nil() || raw != id.to_string() || intent_tmp.replace(id).is_some() {
                    return Err("Imported inventory has an unknown transaction file.".into());
                }
                reject_non_file(&entry.path())?;
                continue;
            }
            if let Some(raw) = name
                .strip_prefix(".batch-")
                .and_then(|rest| rest.strip_suffix(".tag"))
            {
                let id = Uuid::parse_str(raw)
                    .map_err(|_| "Imported inventory has an unknown transaction file.")?;
                if id.is_nil() || raw != id.to_string() || batch_tag.replace(id).is_some() {
                    return Err("Imported inventory has an unknown transaction file.".into());
                }
                reject_non_file(&entry.path())?;
                continue;
            }
            let parsed = IntoIterator::into_iter([
                (".prior-", ".manifest", &mut has_prior),
                (".prepared-", ".manifest", &mut has_prepared),
                (".intent-", ".anchor", &mut has_intent),
            ])
            .find_map(|(prefix, suffix, present)| {
                name.strip_prefix(prefix)
                    .and_then(|rest| rest.strip_suffix(suffix))
                    .map(|raw| (raw, present))
            });
            if let Some((raw, present)) = parsed {
                let id = Uuid::parse_str(raw)
                    .map_err(|_| "Imported inventory has an unknown transaction file.")?;
                if id.is_nil()
                    || raw != id.to_string()
                    || txn.is_some_and(|old| old != id)
                    || *present
                {
                    return Err("Imported inventory has an unknown transaction file.".into());
                }
                txn = Some(id);
                *present = true;
                reject_non_file(&entry.path())?;
            } else if name.starts_with(".prior-")
                || name.starts_with(".prepared-")
                || name.starts_with(".intent-")
                || name.starts_with(".batch-")
            {
                return Err("Imported inventory has an unknown transaction file.".into());
            }
        }
        if let Some(tmp_id) = intent_tmp {
            if txn.is_some() || batch_tag.is_some() {
                return Err("Imported inventory has an unknown transaction file.".into());
            }
            // The canonical intent is published only by rename after its
            // bytes are synced. A stranded temporary file can be discarded
            // if no key file exists outside the unchanged anchored manifest.
            let published = self.root().join(MANIFEST);
            let bytes = read_manifest_bytes(&published)?;
            if anchor::digest(&bytes) != *digest {
                return Err(
                    "Uncommitted imported inventory has no unchanged anchored prior.".into(),
                );
            }
            self.read_manifest_file(&published, &[], false)?;
            if anchor::load(&authority.scope)?.as_ref() != Some(&authority)
                || read_manifest_bytes(&published)? != bytes
            {
                return Err("Imported inventory authority changed before staging cleanup.".into());
            }
            return remove_staged(&self.intent_tmp(tmp_id));
        }
        let Some(txn_id) = txn else {
            if batch_tag.is_some() {
                return Err("Imported inventory has an unknown transaction file.".into());
            }
            return Ok(());
        };
        if batch_tag.is_some_and(|id| id != txn_id) {
            return Err("Imported inventory has an unknown transaction file.".into());
        }
        if !has_intent || (!has_prior && has_prepared) {
            return Err("Imported inventory has an unproven precommit transaction.".into());
        }
        self.check_transaction_files(Some(txn_id))?;
        if read_manifest_bytes(&self.intent(txn_id))? != authority.encode()? {
            return Err(
                "Imported inventory precommit intent differs from its device anchor.".into(),
            );
        }
        let published = self.root().join(MANIFEST);
        let published_bytes = read_manifest_bytes(&published)?;
        if anchor::digest(&published_bytes) != *digest {
            return Err("Uncommitted imported inventory has no unchanged anchored prior.".into());
        }
        if let Ok(old) = self.read_manifest_file(&published, &[], false) {
            // A crash may interrupt the prior or prepared writes. If no
            // candidate ciphertext exists, none has authority: discard the
            // incomplete transaction while retaining the unchanged inventory.
            // A legacy single-key admission always stages its ciphertext
            // before the prepared manifest. If that manifest is complete
            // but its candidate key is missing, a deletion may have been
            // interrupted: keep the transaction fail closed. Batch admission
            // instead records its authenticated tag before staging any key.
            if has_prepared && batch_tag.is_none() {
                if let Ok(next) = self.read_manifest_structure(&self.prepared(txn_id), false) {
                    let old_names: HashSet<_> =
                        old.records.iter().map(LocalKeyRecord::file_name).collect();
                    if next
                        .records
                        .iter()
                        .map(LocalKeyRecord::file_name)
                        .any(|name| !old_names.contains(&name))
                    {
                        return Err("Imported inventory has a missing single-key candidate.".into());
                    }
                }
            }
            if anchor::load(&authority.scope)?.as_ref() != Some(&authority)
                || read_manifest_bytes(&published)? != published_bytes
            {
                return Err("Imported inventory authority changed before staging cleanup.".into());
            }
            if has_prepared {
                remove_staged(&self.prepared(txn_id))?;
            }
            if batch_tag.is_some() {
                remove_staged(&self.batch_tag(txn_id))?;
            }
            if has_prior {
                remove_staged(&self.prior(txn_id))?;
            }
            return remove_staged(&self.intent(txn_id));
        }
        if has_prior && read_manifest_bytes(&self.prior(txn_id))? != published_bytes {
            return Err("Uncommitted imported inventory has no unchanged anchored prior.".into());
        }
        let mut new_files = Vec::new();
        if has_prepared {
            let raw_old: Manifest = serde_json::from_slice(&published_bytes)
                .map_err(|_| "Invalid anchored imported manifest.")?;
            let old_names: Vec<_> = raw_old
                .records
                .iter()
                .map(LocalKeyRecord::file_name)
                .collect();
            let prepared = self.prepared(txn_id);
            let prepared_bytes = read_manifest_bytes(&prepared)?;
            // The transaction intent and anchored prior were written before a
            // batch starts staging ciphertext. A crash can leave only some of
            // the candidate records. Their names are still bounded by the
            // canonical prepared manifest; none has admission authority.
            let next = if batch_tag.is_some() {
                self.read_manifest_structure(&prepared, false)?
            } else {
                self.read_manifest_file(&prepared, &old_names, false)?
            };
            let next_names: Vec<_> = next.records.iter().map(LocalKeyRecord::file_name).collect();
            let old = self.read_manifest_file(&published, &next_names, false)?;
            if serde_json::to_vec(&old).ok().as_deref() != Some(published_bytes.as_slice()) {
                return Err("Uncommitted imported prior manifest changed.".into());
            }
            let admitted: HashSet<_> = old_names.into_iter().collect();
            new_files = next_names
                .into_iter()
                .filter(|name| !admitted.contains(name))
                .collect();
            if batch_tag.is_some() {
                if !old.records.is_empty() {
                    return Err("Imported batch cannot replace an existing signer set.".into());
                }
                let device = device()?;
                self.verify_batch_tag(txn_id, &authority, &prepared_bytes, &device)?;
            } else if !old.records.is_empty() || !next.records.is_empty() {
                let credential = credential()?;
                let device = device()?;
                self.verify_records(&old, &credential, &device)?;
                self.verify_records(&next, &credential, &device)?;
            }
            if read_manifest_bytes(&prepared)? != prepared_bytes {
                return Err("Uncommitted imported candidate changed before cleanup.".into());
            }
        } else {
            // An intent can be the only durable transaction file when the
            // following prior create fails. Without a prepared candidate,
            // inventory validation must reject undeclared ciphertext before
            // removing either the intent or a lone prior.
            self.read_manifest_file(&published, &[], false)?;
        }
        if anchor::load(&authority.scope)?.as_ref() != Some(&authority)
            || read_manifest_bytes(&published)? != published_bytes
        {
            return Err("Imported inventory authority changed before staging cleanup.".into());
        }
        // Preserve the manifests and intent while deleting candidate
        // ciphertext: a crash must not leave a key with no candidate identity.
        // A partial deletion can still require owner-assisted recovery.
        for name in new_files {
            let path = self.root().join(name);
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == ErrorKind::NotFound && batch_tag.is_some() => {}
                Err(_) => return Err("Cannot inspect staged imported key.".into()),
                Ok(_) => remove_staged(&path)?,
            }
        }
        if has_prepared {
            remove_staged(&self.prepared(txn_id))?;
        }
        if batch_tag.is_some() {
            remove_staged(&self.batch_tag(txn_id))?;
        }
        if has_prior {
            remove_staged(&self.prior(txn_id))?;
        }
        remove_staged(&self.intent(txn_id))?;
        Ok(())
    }

    fn prepared(&self, txn_id: Uuid) -> PathBuf {
        self.root().join(format!(".prepared-{txn_id}.manifest"))
    }

    fn prior(&self, txn_id: Uuid) -> PathBuf {
        self.root().join(format!(".prior-{txn_id}.manifest"))
    }

    fn intent(&self, txn_id: Uuid) -> PathBuf {
        self.root().join(format!(".intent-{txn_id}.anchor"))
    }

    fn intent_tmp(&self, txn_id: Uuid) -> PathBuf {
        self.root().join(format!(".intent-{txn_id}.tmp"))
    }

    fn batch_tag(&self, txn_id: Uuid) -> PathBuf {
        self.root().join(format!(".batch-{txn_id}.tag"))
    }

    fn batch_tag_message(
        &self,
        txn_id: Uuid,
        authority: &Anchor,
        next_bytes: &[u8],
    ) -> Result<Vec<u8>, String> {
        let mut message = b"coincube-imported-key-batch-v1".to_vec();
        message.extend_from_slice(txn_id.as_bytes());
        message.extend_from_slice(&anchor::digest(&authority.encode()?));
        message.extend_from_slice(&anchor::digest(next_bytes));
        Ok(message)
    }

    fn write_batch_tag(
        &self,
        txn_id: Uuid,
        authority: &Anchor,
        next_bytes: &[u8],
        device: &coincube_core::seed_crypt::DeviceSecret,
    ) -> Result<(), String> {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &device[..]);
        let message = self.batch_tag_message(txn_id, authority, next_bytes)?;
        let tag = ring::hmac::sign(&key, &message);
        write_new(&self.batch_tag(txn_id), tag.as_ref())
    }

    fn verify_batch_tag(
        &self,
        txn_id: Uuid,
        authority: &Anchor,
        next_bytes: &[u8],
        device: &coincube_core::seed_crypt::DeviceSecret,
    ) -> Result<(), String> {
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &device[..]);
        let message = self.batch_tag_message(txn_id, authority, next_bytes)?;
        let tag = fs::read(self.batch_tag(txn_id))
            .map_err(|_| "Cannot read imported batch authority tag.")?;
        ring::hmac::verify(&key, &message, &tag)
            .map_err(|_| "Imported batch authority tag is invalid.".into())
    }

    fn stage_manifests(
        &self,
        authority: &Anchor,
        txn_id: Uuid,
        old_bytes: &[u8],
        next_bytes: &[u8],
    ) -> Result<(), String> {
        let tmp = self.intent_tmp(txn_id);
        write_new(&tmp, &authority.encode()?)?;
        if fs::rename(&tmp, self.intent(txn_id)).is_err() {
            remove_staged(&tmp)?;
            return Err("Cannot publish imported transaction intent.".into());
        }
        sync_parent(&self.intent(txn_id))?;
        let prior = self.prior(txn_id);
        if let Err(error) = write_new(&prior, old_bytes) {
            if anchor::load(&authority.scope)?.as_ref() != Some(authority) {
                return Err("Imported inventory anchor changed before staging cleanup.".into());
            }
            remove_staged(&self.intent(txn_id))?;
            return Err(error);
        }
        if let Err(error) = write_new(&self.prepared(txn_id), next_bytes) {
            self.discard_before_intent(authority, txn_id, false)?;
            return Err(error);
        }
        Ok(())
    }

    fn discard_before_intent(
        &self,
        authority: &Anchor,
        txn_id: Uuid,
        prepared_created: bool,
    ) -> Result<(), String> {
        if anchor::load(&authority.scope)?.as_ref() != Some(authority) {
            return Err("Imported inventory anchor changed before staging cleanup.".into());
        }
        if prepared_created {
            remove_staged(&self.prepared(txn_id))?;
        }
        remove_staged(&self.prior(txn_id))?;
        remove_staged(&self.intent(txn_id))
    }

    fn check_transaction_files(&self, allowed: Option<Uuid>) -> Result<(), String> {
        let prior = allowed.map(|txn| format!(".prior-{txn}.manifest"));
        let prepared = allowed.map(|txn| format!(".prepared-{txn}.manifest"));
        let intent = allowed.map(|txn| format!(".intent-{txn}.anchor"));
        let batch_tag = allowed.map(|txn| format!(".batch-{txn}.tag"));
        for entry in
            fs::read_dir(self.root()).map_err(|_| "Cannot inspect imported key storage.")?
        {
            let entry = entry.map_err(|_| "Cannot inspect imported key storage.")?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".manifest-") && name.ends_with(".tmp") {
                return Err("Imported inventory has an unexplained manifest staging file.".into());
            }
            if name.starts_with(".prior-")
                || name.starts_with(".prepared-")
                || name.starts_with(".intent-")
                || name.starts_with(".batch-")
            {
                if prior.as_deref() != Some(name.as_str())
                    && prepared.as_deref() != Some(name.as_str())
                    && intent.as_deref() != Some(name.as_str())
                    && batch_tag.as_deref() != Some(name.as_str())
                {
                    return Err("Imported inventory has an unknown transaction file.".into());
                }
                reject_non_file(&entry.path())?;
            }
        }
        Ok(())
    }

    fn verify_records(
        &self,
        manifest: &Manifest,
        credential: &str,
        device: &coincube_core::seed_crypt::DeviceSecret,
    ) -> Result<(), String> {
        for record in &manifest.records {
            self.read_key(record, credential, device)?;
        }
        Ok(())
    }

    fn resolve_update(&self, old: &Anchor, next: &Anchor) -> Result<(), String> {
        if anchor::update(old, next).is_ok() || anchor::load(&old.scope)?.as_ref() == Some(next) {
            Ok(())
        } else {
            Err("Imported inventory anchor update has an uncertain outcome.".into())
        }
    }

    fn recover_pending(&self, pending: Anchor) -> Result<Anchor, String> {
        let credential = self.check_active()?;
        let device = self.device(false)?;
        self.recover_pending_with(pending, &credential, &device)
    }

    fn recover_pending_with(
        &self,
        pending: Anchor,
        credential: &str,
        device: &coincube_core::seed_crypt::DeviceSecret,
    ) -> Result<Anchor, String> {
        let AnchorState::Pending {
            old_generation,
            old_digest,
            next_digest,
            txn_id,
            retire_files,
        } = &pending.state
        else {
            return Err("Imported inventory recovery had no pending transaction.".into());
        };
        let published = self.root().join(MANIFEST);
        let prepared = self.prepared(*txn_id);
        let prior = self.prior(*txn_id);
        self.check_transaction_files(Some(*txn_id))?;
        let prior_bytes = read_manifest_bytes(&prior)?;
        if anchor::digest(&prior_bytes) != *old_digest {
            return Err("Pending imported prior manifest changed or is missing.".into());
        }
        let published_bytes = read_manifest_bytes(&published)?;
        let published_digest = anchor::digest(&published_bytes);
        if published_digest == *old_digest {
            let prepared_bytes = read_manifest_bytes(&prepared)?;
            if anchor::digest(&prepared_bytes) != *next_digest {
                return Err("Pending imported manifest candidate changed.".into());
            }
            let next = self.read_manifest_file(&prepared, retire_files, false)?;
            let old = self.read_manifest_file(
                &published,
                &next
                    .records
                    .iter()
                    .map(LocalKeyRecord::file_name)
                    .collect::<Vec<_>>(),
                false,
            )?;
            if published_bytes != prior_bytes {
                return Err(
                    "Pending imported prior manifest differs from the published old manifest."
                        .into(),
                );
            }
            let next_names: HashSet<_> =
                next.records.iter().map(LocalKeyRecord::file_name).collect();
            let mut actual_retired: Vec<_> = old
                .records
                .iter()
                .map(LocalKeyRecord::file_name)
                .filter(|name| !next_names.contains(name))
                .collect();
            actual_retired.sort();
            if &actual_retired != retire_files {
                return Err("Pending imported retirement list differs from manifests.".into());
            }
            self.verify_records(&old, credential, device)?;
            self.verify_records(&next, credential, device)?;
            fs::rename(&prepared, &published)
                .map_err(|_| "Cannot publish recovered imported manifest.")?;
            sync_parent(&published)?;
        } else if published_digest == *next_digest {
            let next = self.read_manifest_file(&published, retire_files, false)?;
            let old = self.read_manifest_file(
                &prior,
                &next
                    .records
                    .iter()
                    .map(LocalKeyRecord::file_name)
                    .collect::<Vec<_>>(),
                false,
            )?;
            let next_names: HashSet<_> =
                next.records.iter().map(LocalKeyRecord::file_name).collect();
            let mut actual_retired: Vec<_> = old
                .records
                .iter()
                .map(LocalKeyRecord::file_name)
                .filter(|name| !next_names.contains(name))
                .collect();
            actual_retired.sort();
            if &actual_retired != retire_files {
                return Err(
                    "Pending imported retirement list differs from the prior manifest.".into(),
                );
            }
            self.verify_records(&old, credential, device)?;
            self.verify_records(&next, credential, device)?;
        } else {
            return Err("Pending imported manifest has unknown contents.".into());
        }
        let committed = Anchor {
            scope: pending.scope.clone(),
            state: AnchorState::Committed {
                generation: old_generation
                    .checked_add(1)
                    .ok_or("Imported inventory generation overflow.")?,
                digest: *next_digest,
                cleanup_txn: Some(*txn_id),
                retire_files: retire_files.clone(),
            },
        };
        self.resolve_update(&pending, &committed)?;
        Ok(committed)
    }

    fn finish_cleanup(&self, committed: &Anchor) -> Result<(), String> {
        let AnchorState::Committed {
            generation,
            digest,
            cleanup_txn: Some(txn),
            retire_files,
        } = &committed.state
        else {
            return Ok(());
        };
        for name in retire_files {
            let path = self.root().join(name);
            reject_non_file(&path)?;
            match fs::remove_file(&path) {
                Ok(()) => sync_parent(&path)?,
                Err(error) if error.kind() == ErrorKind::NotFound => (),
                Err(_) => return Err("Cannot clean retired imported key.".into()),
            }
        }
        let prepared = self.prepared(*txn);
        match fs::remove_file(&prepared) {
            Ok(()) => sync_parent(&prepared)?,
            Err(error) if error.kind() == ErrorKind::NotFound => (),
            Err(_) => return Err("Cannot clean prepared imported manifest.".into()),
        }
        let batch_tag = self.batch_tag(*txn);
        reject_non_file(&batch_tag)?;
        match fs::remove_file(&batch_tag) {
            Ok(()) => sync_parent(&batch_tag)?,
            Err(error) if error.kind() == ErrorKind::NotFound => (),
            Err(_) => return Err("Cannot clean imported batch authority tag.".into()),
        }
        let prior = self.prior(*txn);
        reject_non_file(&prior)?;
        match fs::remove_file(&prior) {
            Ok(()) => sync_parent(&prior)?,
            Err(error) if error.kind() == ErrorKind::NotFound => (),
            Err(_) => return Err("Cannot clean prior imported manifest.".into()),
        }
        let intent = self.intent(*txn);
        reject_non_file(&intent)?;
        match fs::remove_file(&intent) {
            Ok(()) => sync_parent(&intent)?,
            Err(error) if error.kind() == ErrorKind::NotFound => (),
            Err(_) => return Err("Cannot clean imported transaction intent.".into()),
        }
        let clear = Anchor {
            scope: committed.scope.clone(),
            state: AnchorState::Committed {
                generation: *generation,
                digest: *digest,
                cleanup_txn: None,
                retire_files: vec![],
            },
        };
        self.resolve_update(committed, &clear)
    }

    fn publish_manifest(
        &self,
        old: &Manifest,
        next: &Manifest,
        credential: &str,
        device: &coincube_core::seed_crypt::DeviceSecret,
        is_current: impl Fn() -> bool,
    ) -> Result<(), String> {
        self.publish_manifest_with_check(old, next, credential, device, is_current, || {
            #[cfg(not(test))]
            self.check_active().map(|_| ())?;
            Ok(())
        })
    }

    fn publish_manifest_with_check(
        &self,
        old: &Manifest,
        next: &Manifest,
        credential: &str,
        device: &coincube_core::seed_crypt::DeviceSecret,
        is_current: impl Fn() -> bool,
        check_active: impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.publish_manifest_with_stage(
            old,
            next,
            credential,
            device,
            is_current,
            check_active,
            false,
            || Ok(()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_manifest_with_stage(
        &self,
        old: &Manifest,
        next: &Manifest,
        credential: &str,
        device: &coincube_core::seed_crypt::DeviceSecret,
        is_current: impl Fn() -> bool,
        check_active: impl Fn() -> Result<(), String>,
        batch: bool,
        stage_keys: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let authority =
            anchor::load(&self.anchor_scope())?.ok_or("Imported inventory anchor is missing.")?;
        let (generation, old_digest) = match &authority.state {
            AnchorState::Committed {
                generation,
                digest,
                cleanup_txn: None,
                retire_files,
            } if retire_files.is_empty() => (*generation, *digest),
            _ => {
                return Err(
                    "Imported inventory requires cleanup or recovery before mutation.".into(),
                )
            }
        };
        let old_bytes = serde_json::to_vec(old).map_err(|_| "Cannot encode key manifest.")?;
        if anchor::digest(&old_bytes) != old_digest {
            return Err("Imported inventory changed before mutation.".into());
        }
        let next_bytes = serde_json::to_vec(next).map_err(|_| "Cannot encode key manifest.")?;
        let next_names: HashSet<_> = next.records.iter().map(LocalKeyRecord::file_name).collect();
        let old_names: HashSet<_> = old.records.iter().map(LocalKeyRecord::file_name).collect();
        let new_files: Vec<_> = next_names.difference(&old_names).cloned().collect();
        let mut retire_files: Vec<_> = old
            .records
            .iter()
            .map(LocalKeyRecord::file_name)
            .filter(|name| !next_names.contains(name))
            .collect();
        retire_files.sort();
        let txn_id = Uuid::new_v4();
        self.stage_manifests(&authority, txn_id, &old_bytes, &next_bytes)?;
        if batch {
            if let Err(error) = self.write_batch_tag(txn_id, &authority, &next_bytes, device) {
                self.discard_candidate_before_intent(&authority, txn_id, &new_files)?;
                return Err(error);
            }
        }
        if let Err(error) = stage_keys() {
            self.discard_candidate_before_intent(&authority, txn_id, &new_files)?;
            return Err(error);
        }
        if let Err(error) = check_active() {
            self.discard_candidate_before_intent(&authority, txn_id, &new_files)?;
            return Err(error);
        }
        if !is_current() {
            self.discard_candidate_before_intent(&authority, txn_id, &new_files)?;
            return Err("Imported inventory mutation was cancelled before commit intent.".into());
        }
        let pending = Anchor {
            scope: authority.scope.clone(),
            state: AnchorState::Pending {
                old_generation: generation,
                old_digest,
                next_digest: anchor::digest(&next_bytes),
                txn_id,
                retire_files,
            },
        };
        // A read immediately after a failed Keychain update can report a
        // stale old value even if Pending persisted. Preserve every file;
        // a later locked read may discard the candidate only after it
        // proves the unchanged committed anchor and authenticates keys.
        self.resolve_update(&authority, &pending)?;
        let committed = self.recover_pending_with(pending, credential, device)?;
        if let Err(error) = self.finish_cleanup(&committed) {
            log::warn!("Imported inventory committed with cleanup pending: {error}");
        }
        Ok(())
    }

    fn discard_candidate_before_intent(
        &self,
        authority: &Anchor,
        txn_id: Uuid,
        new_files: &[String],
    ) -> Result<(), String> {
        if anchor::load(&authority.scope)?.as_ref() != Some(authority) {
            return Err("Imported inventory anchor changed before staging cleanup.".into());
        }
        for name in new_files {
            let path = self.root().join(name);
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(_) => return Err("Cannot inspect staged imported key.".into()),
                Ok(_) => remove_staged(&path)?,
            }
        }
        let batch_tag = self.batch_tag(txn_id);
        match fs::symlink_metadata(&batch_tag) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err("Cannot inspect imported batch authority tag.".into()),
            Ok(_) => remove_staged(&batch_tag)?,
        }
        self.discard_before_intent(authority, txn_id, true)
    }

    /// Only a provably empty, live operation can acquire its first authority.
    /// A populated #830 repository needs an explicit owner-approved migration.
    fn provision_empty(&self) -> Result<(), String> {
        if anchor::load(&self.anchor_scope())?.is_some() {
            return Ok(());
        }
        #[cfg(not(test))]
        self.check_active()?;
        let empty = self.manifest(vec![]);
        let bytes = serde_json::to_vec(&empty).map_err(|_| "Cannot encode key manifest.")?;
        let path = self.root().join(MANIFEST);
        self.discard_proven_empty_provision_tmp(&path, &bytes)?;
        self.check_transaction_files(None)?;
        if path.exists() {
            let manifest = self.read_manifest_file(&path, &[], false)?;
            if !manifest.records.is_empty() || read_manifest_bytes(&path)? != bytes {
                return Err(
                    "Unanchored imported key inventory requires explicit migration.".into(),
                );
            }
        } else {
            // read_manifest_file rejects any existing ciphertext even when the
            // manifest is absent. A prepared file also blocks first provision.
            self.read_manifest_file(&path, &[], true)?;
            write_replace(&path, &bytes)?;
        }
        let authority = Anchor {
            scope: self.anchor_scope(),
            state: AnchorState::Committed {
                generation: 0,
                digest: anchor::digest(&bytes),
                cleanup_txn: None,
                retire_files: vec![],
            },
        };
        anchor::create(&authority)
    }

    /// A crash during the first empty manifest write can leave its synced
    /// temporary file behind before any anchor exists. Remove only the exact
    /// canonical empty bytes when the temporary file is the entire inventory.
    fn discard_proven_empty_provision_tmp(
        &self,
        published: &Path,
        empty_bytes: &[u8],
    ) -> Result<(), String> {
        if published.exists() {
            return Ok(());
        }
        let mut candidate = None;
        for entry in
            fs::read_dir(self.root()).map_err(|_| "Cannot inspect imported key storage.")?
        {
            let entry = entry.map_err(|_| "Cannot inspect imported key storage.")?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let valid_tmp = name
                .strip_prefix(".manifest-")
                .and_then(|rest| rest.strip_suffix(".tmp"))
                .and_then(|raw| Uuid::parse_str(raw).ok().map(|id| (raw, id)))
                .is_some_and(|(raw, id)| !id.is_nil() && raw == id.to_string());
            if !valid_tmp || candidate.is_some() {
                return Ok(());
            }
            candidate = Some(entry.path());
        }
        let Some(candidate) = candidate else {
            return Ok(());
        };
        if read_manifest_bytes(&candidate)? != empty_bytes {
            return Ok(());
        }
        if anchor::load(&self.anchor_scope())?.is_some() {
            return Err("Imported inventory anchor appeared during provision cleanup.".into());
        }
        remove_staged(&candidate)
    }

    fn read_manifest_file(
        &self,
        path: &Path,
        retired: &[String],
        allow_missing: bool,
    ) -> Result<Manifest, String> {
        let manifest = self.read_manifest_structure(path, allow_missing)?;
        self.check_inventory(&manifest, retired)?;
        Ok(manifest)
    }

    /// Check scope and canonical record identities without requiring every
    /// staged ciphertext to exist yet. Recovery uses this to discard a
    /// precommit transaction after an interrupted batch write.
    fn read_manifest_structure(
        &self,
        path: &Path,
        allow_missing: bool,
    ) -> Result<Manifest, String> {
        reject_non_file(path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = match options.open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::NotFound && allow_missing => {
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
        if manifest.records.windows(2).any(|pair| {
            (&pair[0].fingerprint, &pair[0].origin, &pair[0].xpub)
                >= (&pair[1].fingerprint, &pair[1].origin, &pair[1].xpub)
        }) {
            return Err("Imported key manifest records are not canonically ordered.".into());
        }
        Ok(manifest)
    }

    fn check_inventory(&self, manifest: &Manifest, retired: &[String]) -> Result<(), String> {
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
                if !declared.contains(&name) && !retired.iter().any(|old| old == &name) {
                    return Err("Imported key inventory contains an undeclared record.".into());
                }
                reject_non_file(&entry.path())?;
                if declared.contains(&name) {
                    present.insert(name);
                }
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
        let lock = self.lock()?;
        self.provision_empty()?;
        let manifest = self.read_manifest_recoverable(&lock)?;
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

    /// Admit a complete set of new local accounts in one anchor generation.
    /// Recovery Kit restore starts with an empty inventory. An exact retry
    /// after an uncertain commit returns the authenticated existing records;
    /// any different signer set must be reconciled explicitly.
    pub fn admit_new_batch(
        &self,
        entries: Vec<(ImportedAccount, ImportedKeySecret)>,
        is_current: impl Fn() -> bool,
    ) -> Result<Vec<LocalKeyRecord>, String> {
        if !is_current() {
            return Err("Imported key admission was cancelled.".into());
        }
        let credential = self.check_active()?;
        let lock = self.lock()?;
        self.provision_empty()?;
        let manifest = self.read_manifest_recoverable(&lock)?;
        let device = self.device(manifest.records.is_empty())?;
        self.admit_new_batch_locked(manifest, entries, &credential, &device, || {
            self.check_active().is_ok() && is_current()
        })
    }

    fn admit_new_batch_locked(
        &self,
        manifest: Manifest,
        entries: Vec<(ImportedAccount, ImportedKeySecret)>,
        credential: &str,
        device: &coincube_core::seed_crypt::DeviceSecret,
        is_current: impl Fn() -> bool,
    ) -> Result<Vec<LocalKeyRecord>, String> {
        if entries.is_empty() || entries.len() > MAX_RECORDS {
            return Err("Imported key batch has an invalid size.".into());
        }
        if !manifest.records.is_empty() {
            // A commit may have succeeded before the caller received its
            // result. Reopen only the identical, fully authenticated signer
            // set so a restore can resume after an uncertain response.
            if manifest.records.len() != entries.len() {
                return Err("Imported key batch conflicts with the current inventory.".into());
            }
            self.verify_records(&manifest, credential, device)?;
            let mut seen = Vec::with_capacity(entries.len());
            for (account, secret) in &entries {
                self.verify_batch_account(account, secret)?;
                if seen.contains(account)
                    || !manifest
                        .records
                        .iter()
                        .any(|record| record.account().ok().as_ref() == Some(account))
                {
                    return Err("Imported key batch conflicts with the current inventory.".into());
                }
                seen.push(account.clone());
            }
            if !is_current() {
                return Err("Imported key admission was cancelled.".into());
            }
            return Ok(manifest.records);
        }
        let mut records = Vec::with_capacity(entries.len());
        let mut ciphertexts = Vec::with_capacity(entries.len());
        for (account, secret) in entries {
            if records
                .iter()
                .any(|record: &LocalKeyRecord| record.account().ok().as_ref() == Some(&account))
            {
                return Err("Imported key batch contains a duplicate account.".into());
            }
            self.verify_batch_account(&account, &secret)?;
            let record = LocalKeyRecord::from_account(&account, Uuid::new_v4(), Uuid::new_v4());
            let ciphertext = imported_key_crypt::seal(
                &secret,
                credential,
                &self.binding(&record)?,
                Protection::DeviceBound(device),
            )
            .map_err(|_| "Cannot encrypt imported key.")?;
            records.push(record);
            ciphertexts.push(ciphertext);
        }
        let mut next = self.manifest(records.clone());
        next.records.sort_by(|left, right| {
            (&left.fingerprint, &left.origin, &left.xpub).cmp(&(
                &right.fingerprint,
                &right.origin,
                &right.xpub,
            ))
        });
        if !is_current() {
            return Err("Imported key admission was cancelled.".into());
        }
        self.publish_manifest_with_stage(
            &manifest,
            &next,
            credential,
            device,
            is_current,
            || {
                #[cfg(not(test))]
                self.check_active().map(|_| ())?;
                Ok(())
            },
            true,
            || {
                for (record, ciphertext) in records.iter().zip(ciphertexts.iter()) {
                    write_new(&self.root().join(record.file_name()), ciphertext)?;
                }
                Ok(())
            },
        )?;
        Ok(records)
    }

    fn verify_batch_account(
        &self,
        account: &ImportedAccount,
        secret: &ImportedKeySecret,
    ) -> Result<(), String> {
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
        Ok(())
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
        let before = anchor::load(&self.anchor_scope())?;
        let staged = self.root().join(record.file_name());
        write_new(&staged, &ciphertext)?;
        if let Some(index) = matching {
            manifest.records[index] = record.clone();
        } else {
            manifest.records.push(record.clone());
        }
        manifest.records.sort_by(|left, right| {
            (&left.fingerprint, &left.origin, &left.xpub).cmp(&(
                &right.fingerprint,
                &right.origin,
                &right.xpub,
            ))
        });
        if !is_current() {
            remove_staged(&staged)?;
            return Err("Imported key admission was cancelled.".into());
        }
        let old =
            self.read_manifest_file(&self.root().join(MANIFEST), &[record.file_name()], false)?;
        match self.publish_manifest(&old, &manifest, credential, device, is_current) {
            Ok(()) => Ok(record),
            Err(error) => {
                // Keep a staged ciphertext if an uncertain anchor read left a
                // complete pre-Pending pair. The next locked read must first
                // authenticate that candidate before discarding it.
                if staged.exists()
                    && anchor::load(&self.anchor_scope())?.as_ref() == before.as_ref()
                    && self.check_transaction_files(None).is_ok()
                {
                    remove_staged(&staged)?;
                }
                Err(error)
            }
        }
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
        let lock = self.lock()?;
        let manifest = self.read_manifest_recoverable(&lock)?;
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
        let lock = self.lock()?;
        let manifest = self.read_manifest_recoverable(&lock)?;
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
        let lock = self.lock()?;
        let mut manifest = self.read_manifest_recoverable(&lock)?;
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
        let mut old = manifest.clone();
        old.records.push(retired);
        old.records.sort_by(|left, right| {
            (&left.fingerprint, &left.origin, &left.xpub).cmp(&(
                &right.fingerprint,
                &right.origin,
                &right.xpub,
            ))
        });
        self.publish_manifest(&old, &manifest, &credential, &device, is_current)
    }

    /// Called by the import removal transaction after it has checked the
    /// wallet owner. A durable tombstone prevents an old in-flight admission
    /// from recreating this operation while Settings is being removed.
    pub(crate) fn remove_operation_storage(&self) -> Result<(), String> {
        let _lock = self.lock_internal(true)?;
        if !anchor::supported() {
            return self.remove_public_only_storage_without_anchor();
        }
        let scope = self.anchor_scope();
        match anchor::load(&scope)? {
            Some(current) if matches!(current.state, AnchorState::Revoked { .. }) => {}
            Some(current) => {
                let (generation, digest) = match &current.state {
                    AnchorState::Committed {
                        generation, digest, ..
                    } => (*generation, *digest),
                    AnchorState::Pending {
                        old_generation,
                        old_digest,
                        ..
                    } => (*old_generation, *old_digest),
                    AnchorState::Revoked { .. } => unreachable!(),
                };
                let revoked = Anchor {
                    scope: scope.clone(),
                    state: AnchorState::Revoked {
                        generation,
                        digest,
                        txn_id: Uuid::new_v4(),
                    },
                };
                self.resolve_update(&current, &revoked)?;
            }
            None => {
                // Explicit legacy removal of a #830 operation: it is never
                // admitted as trustworthy inventory. A permanent revocation
                // item is established before any local deletion begins.
                let revoked = Anchor {
                    scope: scope.clone(),
                    state: AnchorState::Revoked {
                        generation: 0,
                        digest: [0; 32],
                        txn_id: Uuid::new_v4(),
                    },
                };
                anchor::create(&revoked)?;
            }
        }
        let tombstone = self.tombstone();
        if !self.is_removed()? {
            write_new(&tombstone, b"removed-v1")?;
        }
        fs::remove_dir_all(self.root()).map_err(|_| "Cannot remove imported key storage.")?;
        sync_parent(&tombstone)
    }

    fn remove_public_only_storage_without_anchor(&self) -> Result<(), String> {
        // There is no verified replaceable OS item on this platform. No
        // local-key admission is possible; allow only a provably public
        // imported wallet's owner-verified deletion to remain available.
        self.read_manifest_file(&self.root().join(MANIFEST), &[], true)?;
        for entry in
            fs::read_dir(self.root()).map_err(|_| "Cannot inspect imported key storage.")?
        {
            let entry = entry.map_err(|_| "Cannot inspect imported key storage.")?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name != MANIFEST && name != "imported-keys.lock" {
                return Err("Imported local-key storage is ambiguous on this platform.".into());
            }
            reject_non_file(&entry.path())?;
        }
        let tombstone = self.tombstone();
        if !self.is_removed()? {
            write_new(&tombstone, b"removed-v1")?;
        }
        fs::remove_dir_all(self.root())
            .map_err(|_| "Cannot remove public imported key storage.")?;
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

fn read_manifest_bytes(path: &Path) -> Result<Vec<u8>, String> {
    reject_non_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|_| "Cannot read imported key manifest.")?;
    if !file
        .metadata()
        .map_err(|_| "Cannot inspect imported key manifest.")?
        .is_file()
    {
        return Err("Imported key manifest endpoint is unsafe.".into());
    }
    let mut bytes = Vec::new();
    file.take(16_385)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read imported key manifest.")?;
    if bytes.len() > 16_384 {
        return Err("Imported key manifest is oversized.".into());
    }
    Ok(bytes)
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

#[cfg(all(target_os = "macos", debug_assertions, feature = "native-anchor-probe"))]
pub fn native_probe_phase(phase: &str, run_id: Uuid, root: &Path) -> Result<(), String> {
    use crate::app::settings::{BitcoinImport, BitcoinImportStage};
    use crate::chain::ChainId;
    use coincube_core::miniscript::bitcoin::{bip32::Xpriv, secp256k1::Secp256k1, Network};
    use coincube_core::wallet_descriptor::ImportedWalletDescriptor;

    if run_id.is_nil() || !root.is_absolute() {
        return Err(
            "Native anchor probe requires an absolute disposable root and run UUID.".into(),
        );
    }
    let marker = root.join(format!(".native-anchor-probe-{run_id}"));
    if phase == "setup" {
        fs::create_dir(root).map_err(|_| "Cannot create native probe root.")?;
        write_new(&marker, b"synthetic imported anchor probe v1")?;
    } else if fs::read(&marker).ok().as_deref() != Some(b"synthetic imported anchor probe v1") {
        return Err("Native anchor probe root or run identity changed.".into());
    }

    anchor::with_native_probe_service(run_id, || {
        let directory = NetworkDirectory::new(root.join("regtest"));
        if phase == "setup" {
            directory
                .init()
                .map_err(|_| "Cannot initialize native probe directory.")?;
        }
        let secp = Secp256k1::new();
        let path = DerivationPath::from_str("m/48h/1h/0h/2h").unwrap();
        let mut keys = Vec::new();
        let mut phrases = Vec::new();
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
                .map_err(|_| "Cannot construct synthetic native probe wallet.")?;
        let account = verify_imported_account(
            &wallet,
            &path,
            Zeroizing::new(phrases[0].clone()),
            Zeroizing::new(String::new()),
        )
        .map_err(|_| "Cannot verify synthetic native probe account.")?
        .account()
        .clone();
        let attachment = ImportedWalletAttachment::new(wallet, ChainId::Regtest, 0, 100)
            .map_err(|_| "Cannot construct synthetic native probe attachment.")?;
        let mut cube = CubeSettings::new("Native anchor probe".into(), ChainId::Regtest);
        cube.id = format!("native-anchor-probe-{run_id}");
        cube.bitcoin_import = Some(Box::new(BitcoinImport {
            operation_id: run_id,
            stage: BitcoinImportStage::Ready {
                wallet: attachment.clone(),
            },
            last_error: None,
        }));
        let repository = LocalKeyRepository::new(&directory, &cube, &attachment)?;
        let device = Zeroizing::new([11u8; 32]);
        let scope = repository.anchor_scope();
        if phase == "setup" {
            repository.ensure_root()?;
            let bytes = serde_json::to_vec(&repository.manifest(vec![]))
                .map_err(|_| "Cannot encode native probe manifest.")?;
            write_new(&root.join("generation-0-manifest.json"), &bytes)?;
            write_replace(&repository.root().join(MANIFEST), &bytes)?;
            return anchor::create(&Anchor {
                scope,
                state: AnchorState::Committed {
                    generation: 0,
                    digest: anchor::digest(&bytes),
                    cleanup_txn: None,
                    retire_files: vec![],
                },
            });
        }
        let _lock = repository.lock()?;
        let assert_committed = |generation| -> Result<(), String> {
            let manifest = repository.read_manifest()?;
            let expected_records = if generation == 1 { 1 } else { 0 };
            if manifest.records.len() != expected_records {
                return Err("Native probe repository has the wrong signer inventory.".into());
            }
            if generation == 1 {
                let verified =
                    repository.read_key(&manifest.records[0], "synthetic unlock", &device)?;
                if verified.account() != &account {
                    return Err("Native probe imported signer identity changed.".into());
                }
            }
            let current = anchor::load(&scope)?.ok_or("Native probe anchor is missing.")?;
            match current.state {
                AnchorState::Committed {
                    generation: actual,
                    cleanup_txn: None,
                    ..
                } if actual == generation => Ok(()),
                _ => Err("Native probe anchor generation or cleanup state changed.".into()),
            }
        };
        let assert_no_stage = || -> Result<(), String> { repository.check_transaction_files(None) };
        let stage_synthetic_key = || -> Result<Manifest, String> {
            let record = LocalKeyRecord::from_account(&account, Uuid::new_v4(), Uuid::new_v4());
            let secret = ImportedKeySecret::new(
                Zeroizing::new(phrases[0].clone()),
                Zeroizing::new(String::new()),
            )
            .map_err(|_| "Cannot create synthetic native probe secret.")?;
            let ciphertext = imported_key_crypt::seal(
                &secret,
                "synthetic unlock",
                &repository.binding(&record)?,
                Protection::DeviceBound(&device),
            )
            .map_err(|_| "Cannot encrypt synthetic native probe key.")?;
            let staged = repository.root().join(record.file_name());
            write_new(&staged, &ciphertext)?;
            Ok(repository.manifest(vec![record]))
        };
        match phase {
            "update-fails" | "update-error" | "readback-fails" => {
                let old = repository.read_manifest()?;
                let before = anchor::load(&scope)?;
                let next = if phase == "readback-fails" {
                    repository.manifest(vec![])
                } else {
                    stage_synthetic_key()?
                };
                let fault = match phase {
                    "update-fails" => anchor::NativeProbeFault::UpdateQueryError,
                    "update-error" => anchor::NativeProbeFault::UpdateThenError,
                    _ => anchor::NativeProbeFault::UpdateThenReadbackError,
                };
                anchor::arm_native_probe_fault(fault);
                let result = repository.publish_manifest_with_check(
                    &old,
                    &next,
                    "synthetic unlock",
                    &device,
                    || true,
                    || Ok(()),
                );
                match phase {
                    "update-fails" => {
                        if result.is_ok() {
                            return Err("Faulted native update unexpectedly succeeded.".into());
                        }
                        if anchor::load(&scope)? != before {
                            return Err("Faulted native update changed the old anchor.".into());
                        }
                        repository.discard_proven_uncommitted_stage_with(
                            || Ok(Zeroizing::new("synthetic unlock".into())),
                            || Ok(Zeroizing::new([11u8; 32])),
                        )?;
                        assert_committed(0)?;
                        assert_no_stage()
                    }
                    "update-error" => {
                        result?;
                        assert_committed(1)?;
                        assert_no_stage()
                    }
                    _ => {
                        if result.is_ok() {
                            return Err("Faulted native readback unexpectedly succeeded.".into());
                        }
                        let pending =
                            anchor::load(&scope)?.ok_or("Native pending anchor disappeared.")?;
                        let AnchorState::Pending { txn_id, .. } = pending.state else {
                            return Err(
                                "Native readback failure did not preserve pending intent.".into()
                            );
                        };
                        let prepared = repository.prepared(txn_id);
                        let published = repository.root().join(MANIFEST);
                        fs::rename(&prepared, &published)
                            .map_err(|_| "Cannot simulate native post-rename restart.")?;
                        sync_parent(&published)?;
                        Ok(())
                    }
                }
            }
            "verify-old" => {
                assert_committed(0)?;
                assert_no_stage()
            }
            "verify-committed" => {
                assert_committed(1)?;
                assert_no_stage()?;
                let published = repository.root().join(MANIFEST);
                let current = read_manifest_bytes(&published)?;
                let old = fs::read(root.join("generation-0-manifest.json"))
                    .map_err(|_| "Cannot read synthetic prior manifest.")?;
                write_replace(&published, &old)?;
                if repository.read_manifest().is_ok() {
                    return Err("Replayed native prior manifest was accepted.".into());
                }
                write_replace(&published, &current)?;
                assert_committed(1)
            }
            "verify-pending-closed" => {
                if repository.read_manifest().is_ok() {
                    return Err(
                        "Pending native anchor was readable without recovery authority.".into(),
                    );
                }
                let pending = anchor::load(&scope)?.ok_or("Native pending anchor disappeared.")?;
                let AnchorState::Pending { txn_id, .. } = pending.state else {
                    return Err("Native pending anchor changed before recovery.".into());
                };
                repository.check_transaction_files(Some(txn_id))
            }
            "recover" => {
                let pending = anchor::load(&scope)?.ok_or("Native pending anchor disappeared.")?;
                if !matches!(pending.state, AnchorState::Pending { .. }) {
                    return Err("Native recovery requires a pending anchor.".into());
                }
                let committed =
                    repository.recover_pending_with(pending, "synthetic unlock", &device)?;
                repository.finish_cleanup(&committed)?;
                assert_committed(2)?;
                assert_no_stage()
            }
            "verify" => {
                assert_committed(2)?;
                assert_no_stage()
            }
            _ => Err("Unknown native anchor probe phase.".into()),
        }
    })
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
            let fixture = Self {
                root,
                directory,
                cube,
                attachment,
                phrases,
                accounts,
            };
            let repository = fixture.repository();
            repository.ensure_root().unwrap();
            let empty = repository.manifest(vec![]);
            let bytes = serde_json::to_vec(&empty).unwrap();
            write_replace(&repository.root().join(MANIFEST), &bytes).unwrap();
            anchor::create(&Anchor {
                scope: repository.anchor_scope(),
                state: AnchorState::Committed {
                    generation: 0,
                    digest: anchor::digest(&bytes),
                    cleanup_txn: None,
                    retire_files: vec![],
                },
            })
            .unwrap();
            fixture
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

        fn force_anchor_to_current_manifest(&self) {
            let repository = self.repository();
            let old = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
            let bytes = read_manifest_bytes(&repository.root().join(MANIFEST)).unwrap();
            let next = Anchor {
                scope: old.scope.clone(),
                state: AnchorState::Committed {
                    generation: 1,
                    digest: anchor::digest(&bytes),
                    cleanup_txn: None,
                    retire_files: vec![],
                },
            };
            let account = anchor::account(&old.scope).unwrap();
            anchor::tests::store_update(&account, &old.encode().unwrap(), &next.encode().unwrap())
                .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn batch_admission_commits_all_accounts_in_one_generation() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let device = Zeroizing::new([31u8; 32]);
        let entries = (0..3)
            .map(|index| {
                (
                    fixture.accounts[index].clone(),
                    ImportedKeySecret::new(
                        Zeroizing::new(fixture.phrases[index].clone()),
                        Zeroizing::new(String::new()),
                    )
                    .unwrap(),
                )
            })
            .collect();
        let records = repository
            .admit_new_batch_locked(
                repository.read_manifest().unwrap(),
                entries,
                "synthetic unlock",
                &device,
                || true,
            )
            .unwrap();
        assert_eq!(records.len(), 3);
        let reopened = fixture.repository();
        let manifest = reopened.read_manifest().unwrap();
        assert_eq!(manifest.records.len(), 3);
        assert!(records
            .iter()
            .all(|record| manifest.records.contains(record)));
        for record in &records {
            assert_eq!(
                reopened
                    .read_key(record, "synthetic unlock", &device)
                    .unwrap()
                    .account(),
                &record.account().unwrap()
            );
        }
        let authority = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        assert!(matches!(
            authority.state,
            AnchorState::Committed { generation: 1, .. }
        ));
        let retry_entries = (0..3)
            .map(|index| {
                (
                    fixture.accounts[index].clone(),
                    ImportedKeySecret::new(
                        Zeroizing::new(fixture.phrases[index].clone()),
                        Zeroizing::new(String::new()),
                    )
                    .unwrap(),
                )
            })
            .collect();
        let retried = repository
            .admit_new_batch_locked(
                manifest.clone(),
                retry_entries,
                "synthetic unlock",
                &device,
                || true,
            )
            .unwrap();
        assert_eq!(retried.len(), records.len());
        assert!(retried.iter().all(|record| records.contains(record)));
        assert!(repository
            .admit_new_batch_locked(manifest, vec![], "synthetic unlock", &device, || true)
            .is_err());
    }

    #[test]
    fn interrupted_precommit_batch_discards_only_candidate_keys() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let device = Zeroizing::new([32u8; 32]);
        let old = repository.read_manifest().unwrap();
        let records: Vec<_> = fixture.accounts[..2]
            .iter()
            .map(|account| LocalKeyRecord::from_account(account, Uuid::new_v4(), Uuid::new_v4()))
            .collect();
        let mut next = repository.manifest(records.clone());
        next.records.sort_by(|left, right| {
            (&left.fingerprint, &left.origin, &left.xpub).cmp(&(
                &right.fingerprint,
                &right.origin,
                &right.xpub,
            ))
        });
        let authority = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        let txn = Uuid::new_v4();
        repository
            .stage_manifests(
                &authority,
                txn,
                &serde_json::to_vec(&old).unwrap(),
                &serde_json::to_vec(&next).unwrap(),
            )
            .unwrap();
        repository
            .write_batch_tag(
                txn,
                &authority,
                &serde_json::to_vec(&next).unwrap(),
                &device,
            )
            .unwrap();
        let secret = ImportedKeySecret::new(
            Zeroizing::new(fixture.phrases[0].clone()),
            Zeroizing::new(String::new()),
        )
        .unwrap();
        let ciphertext = imported_key_crypt::seal(
            &secret,
            "synthetic unlock",
            &repository.binding(&records[0]).unwrap(),
            Protection::DeviceBound(&device),
        )
        .unwrap();
        write_new(&repository.root().join(records[0].file_name()), &ciphertext).unwrap();
        // Simulate a restart before the second key is written and before the
        // anchor's Pending transition. No partial signer may become readable.
        assert!(repository.read_manifest().is_err());
        let reopened = fixture.repository();
        reopened
            .discard_proven_uncommitted_stage_with(
                || Ok(Zeroizing::new("synthetic unlock".into())),
                || Ok(Zeroizing::new([32u8; 32])),
            )
            .unwrap();
        assert!(reopened.read_manifest().unwrap().records.is_empty());
        assert!(!reopened.root().join(records[0].file_name()).exists());
        reopened.check_transaction_files(None).unwrap();
    }

    #[test]
    fn tampered_batch_tag_cannot_discard_a_candidate_key() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let device = Zeroizing::new([36u8; 32]);
        let old = repository.read_manifest().unwrap();
        let record =
            LocalKeyRecord::from_account(&fixture.accounts[0], Uuid::new_v4(), Uuid::new_v4());
        let next = repository.manifest(vec![record.clone()]);
        let authority = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        let txn = Uuid::new_v4();
        let next_bytes = serde_json::to_vec(&next).unwrap();
        repository
            .stage_manifests(
                &authority,
                txn,
                &serde_json::to_vec(&old).unwrap(),
                &next_bytes,
            )
            .unwrap();
        repository
            .write_batch_tag(txn, &authority, &next_bytes, &device)
            .unwrap();
        let staged = repository.root().join(record.file_name());
        write_new(&staged, b"partial ciphertext").unwrap();
        write_replace(&repository.batch_tag(txn), b"tampered tag").unwrap();
        assert!(repository
            .discard_proven_uncommitted_stage_with(
                || Ok(Zeroizing::new("synthetic unlock".into())),
                || Ok(Zeroizing::new([36u8; 32])),
            )
            .is_err());
        assert!(staged.exists());
        assert!(repository.prepared(txn).exists());
    }

    #[test]
    fn cancelled_batch_leaves_no_partial_inventory() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let device = Zeroizing::new([33u8; 32]);
        let secret = ImportedKeySecret::new(
            Zeroizing::new(fixture.phrases[0].clone()),
            Zeroizing::new(String::new()),
        )
        .unwrap();
        assert!(repository
            .admit_new_batch_locked(
                repository.read_manifest().unwrap(),
                vec![(fixture.accounts[0].clone(), secret)],
                "synthetic unlock",
                &device,
                || false,
            )
            .is_err());
        assert!(repository.read_manifest().unwrap().records.is_empty());
        assert!(!fs::read_dir(repository.root()).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".key")));
        repository.check_transaction_files(None).unwrap();
    }

    #[test]
    fn invalid_batch_never_stages_a_signer() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let device = Zeroizing::new([35u8; 32]);
        let secret = |index: usize| {
            ImportedKeySecret::new(
                Zeroizing::new(fixture.phrases[index].clone()),
                Zeroizing::new(String::new()),
            )
            .unwrap()
        };
        for entries in [
            vec![
                (fixture.accounts[0].clone(), secret(0)),
                (fixture.accounts[0].clone(), secret(0)),
            ],
            vec![(fixture.accounts[1].clone(), secret(0))],
        ] {
            assert!(repository
                .admit_new_batch_locked(
                    repository.read_manifest().unwrap(),
                    entries,
                    "synthetic unlock",
                    &device,
                    || true,
                )
                .is_err());
            assert!(repository.read_manifest().unwrap().records.is_empty());
            assert!(!fs::read_dir(repository.root()).unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".key")));
        }
    }

    #[test]
    fn failed_batch_stage_discards_partial_ciphertext_before_intent() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let device = Zeroizing::new([34u8; 32]);
        let old = repository.read_manifest().unwrap();
        let record =
            LocalKeyRecord::from_account(&fixture.accounts[0], Uuid::new_v4(), Uuid::new_v4());
        let next = repository.manifest(vec![record.clone()]);
        let staged = repository.root().join(record.file_name());
        assert!(repository
            .publish_manifest_with_stage(
                &old,
                &next,
                "synthetic unlock",
                &device,
                || true,
                || Ok(()),
                true,
                || {
                    write_new(&staged, b"interrupted ciphertext")?;
                    Err("Synthetic batch write failure.".into())
                },
            )
            .is_err());
        assert!(!staged.exists());
        assert!(repository.read_manifest().unwrap().records.is_empty());
        repository.check_transaction_files(None).unwrap();
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
        records.sort_by(|left, right| {
            (&left.fingerprint, &left.origin, &left.xpub).cmp(&(
                &right.fingerprint,
                &right.origin,
                &right.xpub,
            ))
        });
        write_replace(
            &repository.root().join(MANIFEST),
            &serde_json::to_vec(&repository.manifest(records.clone())).unwrap(),
        )
        .unwrap();
        fixture.force_anchor_to_current_manifest();
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
        for record in &manifest.records {
            assert_eq!(
                reopened
                    .read_key(record, "synthetic unlock", &device)
                    .unwrap()
                    .account(),
                &record.account().unwrap()
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
    fn manifest_rejects_noncanonical_raw_json_even_when_semantically_equal() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let mut bytes = read_manifest_bytes(&repository.root().join(MANIFEST)).unwrap();
        bytes.push(b'\n');
        write_replace(&repository.root().join(MANIFEST), &bytes).unwrap();
        let _guard = repository.lock().unwrap();
        assert!(repository
            .read_manifest_file(&repository.root().join(MANIFEST), &[], false)
            .is_err());
        assert!(repository.read_manifest().is_err());
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
    fn pending_recovery_before_and_after_rename_never_readmits_retired_key() {
        for renamed in [false, true] {
            let fixture = Fixture::new();
            fixture.save_cube(fixture.cube.clone());
            let repository = fixture.repository();
            let device = Zeroizing::new([11u8; 32]);
            let record = repository
                .admit_locked(
                    repository.read_manifest().unwrap(),
                    &fixture.accounts[0],
                    ImportedKeySecret::new(
                        Zeroizing::new(fixture.phrases[0].clone()),
                        Zeroizing::new(String::new()),
                    )
                    .unwrap(),
                    None,
                    "synthetic unlock",
                    &device,
                    || true,
                )
                .unwrap();
            let old_bytes = read_manifest_bytes(&repository.root().join(MANIFEST)).unwrap();
            let next_bytes = serde_json::to_vec(&repository.manifest(vec![])).unwrap();
            let old = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
            let AnchorState::Committed {
                generation, digest, ..
            } = old.state
            else {
                panic!("expected committed inventory");
            };
            let txn = Uuid::new_v4();
            write_new(&repository.prior(txn), &old_bytes).unwrap();
            write_new(&repository.prepared(txn), &next_bytes).unwrap();
            let pending = Anchor {
                scope: old.scope,
                state: AnchorState::Pending {
                    old_generation: generation,
                    old_digest: digest,
                    next_digest: anchor::digest(&next_bytes),
                    txn_id: txn,
                    retire_files: vec![record.file_name()],
                },
            };
            let before = Anchor {
                scope: pending.scope.clone(),
                state: AnchorState::Committed {
                    generation,
                    digest,
                    cleanup_txn: None,
                    retire_files: vec![],
                },
            };
            anchor::update(&before, &pending).unwrap();
            if renamed {
                fs::rename(repository.prepared(txn), repository.root().join(MANIFEST)).unwrap();
                sync_parent(&repository.root().join(MANIFEST)).unwrap();
            }
            let directory = NetworkDirectory::new(fixture.directory.path().to_path_buf());
            let settings = Settings::from_file(&directory).unwrap();
            let cube = &settings.cubes[0];
            let attachment = cube.bitcoin_import.as_ref().unwrap().attachment().unwrap();
            let reopened = LocalKeyRepository::new(&directory, cube, attachment).unwrap();
            let _guard = reopened.lock().unwrap();
            reopened
                .recover_pending_with(pending, "synthetic unlock", &device)
                .unwrap();
            assert_eq!(reopened.read_manifest().unwrap().records.len(), 0);
            assert!(!reopened.root().join(record.file_name()).exists());
            write_replace(&reopened.root().join(MANIFEST), &old_bytes).unwrap();
            assert!(
                reopened.read_manifest().is_err(),
                "old manifest replay must fail"
            );
        }
    }

    #[test]
    fn restart_before_pending_refuses_unanchored_transaction_files() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let old = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        let old_bytes = read_manifest_bytes(&repository.root().join(MANIFEST)).unwrap();
        let txn = Uuid::new_v4();
        write_new(&repository.prior(txn), &old_bytes).unwrap();
        write_new(&repository.prepared(txn), &old_bytes).unwrap();

        let reopened = fixture.repository();
        let _guard = reopened.lock().unwrap();
        assert!(reopened.read_manifest().is_err());
        assert_eq!(anchor::load(&reopened.anchor_scope()).unwrap(), Some(old));
        assert_eq!(
            read_manifest_bytes(&reopened.root().join(MANIFEST)).unwrap(),
            old_bytes
        );
        assert!(reopened.prior(txn).exists());
        assert!(reopened.prepared(txn).exists());
    }

    #[test]
    fn failed_prepared_stage_does_not_leave_unanchored_prior() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let old = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        let old_bytes = read_manifest_bytes(&repository.root().join(MANIFEST)).unwrap();
        let txn = Uuid::new_v4();
        // Force the second create_new to fail after the prior file is durable.
        write_new(&repository.prepared(txn), b"preexisting").unwrap();
        assert!(repository
            .stage_manifests(&old, txn, &old_bytes, &old_bytes)
            .is_err());
        assert!(!repository.prior(txn).exists());
        assert_eq!(fs::read(repository.prepared(txn)).unwrap(), b"preexisting");
        fs::remove_file(repository.prepared(txn)).unwrap();

        let reopened = fixture.repository();
        let _guard = reopened.lock().unwrap();
        assert!(reopened.read_manifest().is_ok());
        assert_eq!(anchor::load(&reopened.anchor_scope()).unwrap(), Some(old));
    }

    #[test]
    fn poisoned_process_mutex_does_not_permanently_block_repository() {
        let _ = std::thread::spawn(|| {
            let _guard = process_lock()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            panic!("intentional mutex poison");
        })
        .join();
        let fixture = Fixture::new();
        let repository = fixture.repository();
        assert!(repository.lock().is_ok());
    }

    #[test]
    fn uncertain_pre_pending_readback_recovers_authenticated_candidate() {
        for interrupted_delete in [false, true] {
            let fixture = Fixture::new();
            let repository = fixture.repository();
            let device = Zeroizing::new([11u8; 32]);
            let old = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
            let _guard = repository.lock().unwrap();
            anchor::tests::inject_update_fault(
                &old.scope,
                anchor::tests::UpdateFault::BeforeWriteTwoReadErrors,
            );
            assert!(repository
                .admit_locked(
                    repository.read_manifest().unwrap(),
                    &fixture.accounts[0],
                    ImportedKeySecret::new(
                        Zeroizing::new(fixture.phrases[0].clone()),
                        Zeroizing::new(String::new()),
                    )
                    .unwrap(),
                    None,
                    "synthetic unlock",
                    &device,
                    || true,
                )
                .is_err());
            assert_eq!(anchor::load(&old.scope).unwrap(), Some(old));
            assert!(repository.read_manifest().is_err());
            if interrupted_delete {
                let key = fs::read_dir(repository.root())
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("key"))
                    .unwrap();
                // Model a crash after authenticated ciphertext deletion but
                // before the candidate manifests are discarded.
                remove_staged(&key).unwrap();
            }
            let result = repository.discard_proven_uncommitted_stage_with(
                || Ok(Zeroizing::new("synthetic unlock".into())),
                || Ok(Zeroizing::new([11u8; 32])),
            );
            if interrupted_delete {
                assert!(result.is_err());
                assert!(repository.read_manifest().is_err());
                assert!(fs::read_dir(repository.root()).unwrap().any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".prepared-")
                }));
            } else {
                result.unwrap();
                assert!(repository.read_manifest().unwrap().records.is_empty());
                repository.check_transaction_files(None).unwrap();
                assert!(!fs::read_dir(repository.root()).unwrap().any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .ends_with(".key")
                }));
            }
        }
    }

    #[test]
    fn lone_prior_recovers_only_without_unidentified_key() {
        for (truncated_prior, unknown_key) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let fixture = Fixture::new();
            let repository = fixture.repository();
            let old = read_manifest_bytes(&repository.root().join(MANIFEST)).unwrap();
            let txn = Uuid::new_v4();
            let authority = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
            write_new(&repository.intent(txn), &authority.encode().unwrap()).unwrap();
            write_new(
                &repository.prior(txn),
                if truncated_prior {
                    &b"partial prior"[..]
                } else {
                    old.as_slice()
                },
            )
            .unwrap();
            if unknown_key {
                write_new(
                    &repository
                        .root()
                        .join(format!("{}-{}.key", Uuid::new_v4(), Uuid::new_v4())),
                    b"unidentified",
                )
                .unwrap();
            }
            let _guard = repository.lock().unwrap();
            let recovered = repository.discard_proven_uncommitted_stage_with(
                || panic!("empty prior needs no credential"),
                || panic!("empty prior needs no device secret"),
            );
            assert_eq!(recovered.is_ok(), !unknown_key);
            assert_eq!(repository.prior(txn).exists(), unknown_key);
            assert_eq!(repository.intent(txn).exists(), unknown_key);
        }
    }

    #[test]
    fn truncated_prepared_without_candidate_keys_can_be_retried() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let authority = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        let old = read_manifest_bytes(&repository.root().join(MANIFEST)).unwrap();
        let txn = Uuid::new_v4();
        write_new(&repository.intent(txn), &authority.encode().unwrap()).unwrap();
        write_new(&repository.prior(txn), &old).unwrap();
        write_new(&repository.prepared(txn), b"partial prepared").unwrap();
        repository
            .discard_proven_uncommitted_stage_with(
                || panic!("no new key requires no credential"),
                || panic!("no new key requires no device secret"),
            )
            .unwrap();
        assert!(repository.read_manifest().unwrap().records.is_empty());
        repository.check_transaction_files(None).unwrap();
    }

    #[test]
    fn interrupted_intent_temp_is_discarded_before_any_key_is_staged() {
        for unknown_key in [false, true] {
            let fixture = Fixture::new();
            let repository = fixture.repository();
            let txn = Uuid::new_v4();
            write_new(&repository.intent_tmp(txn), b"partial intent").unwrap();
            if unknown_key {
                write_new(
                    &repository
                        .root()
                        .join(format!("{}-{}.key", Uuid::new_v4(), Uuid::new_v4())),
                    b"unidentified",
                )
                .unwrap();
            }
            let recovered = repository.discard_proven_uncommitted_stage_with(
                || panic!("intent temp needs no credential"),
                || panic!("intent temp needs no device secret"),
            );
            assert_eq!(recovered.is_ok(), !unknown_key);
            assert_eq!(repository.intent_tmp(txn).exists(), unknown_key);
            if !unknown_key {
                assert!(repository.read_manifest().unwrap().records.is_empty());
            }
        }
    }

    #[test]
    fn lone_intent_recovers_only_without_unidentified_key() {
        for (truncated_intent, unknown_key) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let fixture = Fixture::new();
            let repository = fixture.repository();
            let txn = Uuid::new_v4();
            let authority = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
            let intent = if truncated_intent {
                b"interrupted intent".to_vec()
            } else {
                authority.encode().unwrap()
            };
            write_new(&repository.intent(txn), &intent).unwrap();
            if unknown_key {
                write_new(
                    &repository
                        .root()
                        .join(format!("{}-{}.key", Uuid::new_v4(), Uuid::new_v4())),
                    b"unidentified",
                )
                .unwrap();
            }
            let _guard = repository.lock().unwrap();
            let recovered = repository.discard_proven_uncommitted_stage_with(
                || panic!("lone intent needs no credential"),
                || panic!("lone intent needs no device secret"),
            );
            assert_eq!(recovered.is_ok(), !unknown_key && !truncated_intent);
            assert_eq!(
                repository.intent(txn).exists(),
                unknown_key || truncated_intent
            );
            assert_eq!(anchor::load(&authority.scope).unwrap(), Some(authority));
        }
    }

    #[test]
    fn pre_pending_cleanup_refuses_changed_or_pending_anchor() {
        for pending_state in [false, true] {
            let fixture = Fixture::new();
            let repository = fixture.repository();
            let old = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
            let bytes = read_manifest_bytes(&repository.root().join(MANIFEST)).unwrap();
            let txn = Uuid::new_v4();
            repository
                .stage_manifests(&old, txn, &bytes, &bytes)
                .unwrap();
            let changed = if pending_state {
                Anchor {
                    scope: old.scope.clone(),
                    state: AnchorState::Pending {
                        old_generation: 0,
                        old_digest: anchor::digest(&bytes),
                        next_digest: anchor::digest(&bytes),
                        txn_id: txn,
                        retire_files: vec![],
                    },
                }
            } else {
                Anchor {
                    scope: old.scope.clone(),
                    state: AnchorState::Committed {
                        generation: 1,
                        digest: anchor::digest(&bytes),
                        cleanup_txn: None,
                        retire_files: vec![],
                    },
                }
            };
            let account = anchor::account(&old.scope).unwrap();
            anchor::tests::store_update(
                &account,
                &old.encode().unwrap(),
                &changed.encode().unwrap(),
            )
            .unwrap();
            let _guard = repository.lock().unwrap();
            let result = repository.discard_proven_uncommitted_stage_with(
                || panic!("changed authority must not request credential"),
                || panic!("changed authority must not request device"),
            );
            if pending_state {
                assert!(result.is_ok()); // Pending belongs to the normal recovery path.
            } else {
                assert!(result.is_err());
            }
            assert!(repository.prior(txn).exists());
            assert!(repository.prepared(txn).exists());
            assert!(repository.intent(txn).exists());
        }
    }

    #[test]
    fn failed_anchor_cas_recovers_staging_only_after_old_anchor_proof() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let old = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        let manifest = repository.read_manifest().unwrap();
        let device = Zeroizing::new([11u8; 32]);
        anchor::tests::inject_update_fault(&old.scope, anchor::tests::UpdateFault::BeforeWrite);
        assert!(repository
            .publish_manifest(&manifest, &manifest, "synthetic unlock", &device, || true)
            .is_err());
        assert_eq!(anchor::load(&old.scope).unwrap(), Some(old));
        assert!(repository.read_manifest().is_err());
        repository
            .discard_proven_uncommitted_stage_with(
                || panic!("empty candidate needs no credential"),
                || panic!("empty candidate needs no device secret"),
            )
            .unwrap();
        assert!(repository.read_manifest().is_ok());
        assert!(fs::read_dir(repository.root()).unwrap().all(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            !name.starts_with(".prior-") && !name.starts_with(".prepared-")
        }));
    }

    #[test]
    fn stale_old_read_after_persisted_pending_preserves_recovery_files() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let old = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        let manifest = repository.read_manifest().unwrap();
        let device = Zeroizing::new([11u8; 32]);
        anchor::tests::inject_update_fault(
            &old.scope,
            anchor::tests::UpdateFault::AfterWriteStaleOldRead,
        );
        assert!(repository
            .publish_manifest(&manifest, &manifest, "synthetic unlock", &device, || true)
            .is_err());
        let pending = anchor::load(&old.scope).unwrap().unwrap();
        let AnchorState::Pending { txn_id, .. } = pending.state else {
            panic!("persisted pending anchor was lost");
        };
        assert!(repository.intent(txn_id).exists());
        assert!(repository.prior(txn_id).exists());
        assert!(repository.prepared(txn_id).exists());
        let pending = anchor::load(&old.scope).unwrap().unwrap();
        repository
            .recover_pending_with(pending, "synthetic unlock", &device)
            .unwrap();
        assert!(repository.read_manifest().is_ok());
    }

    #[test]
    fn anchor_write_with_error_recovers_pending_on_restart() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let old = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        let manifest = repository.read_manifest().unwrap();
        let device = Zeroizing::new([11u8; 32]);
        anchor::tests::inject_update_fault(&old.scope, anchor::tests::UpdateFault::AfterWrite);
        repository
            .publish_manifest(&manifest, &manifest, "synthetic unlock", &device, || true)
            .unwrap();

        let reopened = fixture.repository();
        let _guard = reopened.lock().unwrap();
        assert!(reopened.read_manifest().is_ok());
        let current = anchor::load(&old.scope).unwrap().unwrap();
        assert!(matches!(
            current.state,
            AnchorState::Committed {
                generation: 1,
                cleanup_txn: None,
                ..
            }
        ));
        assert!(fs::read_dir(reopened.root()).unwrap().all(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            !name.starts_with(".prior-") && !name.starts_with(".prepared-")
        }));
    }

    #[test]
    fn failed_anchor_readback_preserves_pending_files_for_restart() {
        let fixture = Fixture::new();
        fixture.save_cube(fixture.cube.clone());
        let repository = fixture.repository();
        let old = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        let manifest = repository.read_manifest().unwrap();
        let device = Zeroizing::new([11u8; 32]);
        anchor::tests::inject_update_fault(
            &old.scope,
            anchor::tests::UpdateFault::AfterWriteReadbackError,
        );
        assert!(repository
            .publish_manifest(&manifest, &manifest, "synthetic unlock", &device, || true)
            .is_err());
        let pending = anchor::load(&old.scope).unwrap().unwrap();
        assert!(matches!(pending.state, AnchorState::Pending { .. }));

        let directory = NetworkDirectory::new(fixture.directory.path().to_path_buf());
        let settings = Settings::from_file(&directory).unwrap();
        let cube = &settings.cubes[0];
        let attachment = cube.bitcoin_import.as_ref().unwrap().attachment().unwrap();
        let reopened = LocalKeyRepository::new(&directory, cube, attachment).unwrap();
        let _guard = reopened.lock().unwrap();
        reopened
            .recover_pending_with(pending, "synthetic unlock", &device)
            .unwrap();
        assert!(reopened.read_manifest().is_ok());
    }

    #[test]
    fn restart_after_committed_finishes_only_anchored_cleanup() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let device = Zeroizing::new([11u8; 32]);
        let record = repository
            .admit_locked(
                repository.read_manifest().unwrap(),
                &fixture.accounts[0],
                ImportedKeySecret::new(
                    Zeroizing::new(fixture.phrases[0].clone()),
                    Zeroizing::new(String::new()),
                )
                .unwrap(),
                None,
                "synthetic unlock",
                &device,
                || true,
            )
            .unwrap();
        let old_bytes = read_manifest_bytes(&repository.root().join(MANIFEST)).unwrap();
        let next_bytes = serde_json::to_vec(&repository.manifest(vec![])).unwrap();
        let old = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        let AnchorState::Committed {
            generation, digest, ..
        } = &old.state
        else {
            panic!("expected committed inventory");
        };
        let (generation, digest) = (*generation, *digest);
        let txn = Uuid::new_v4();
        write_new(&repository.prior(txn), &old_bytes).unwrap();
        write_new(&repository.prepared(txn), &next_bytes).unwrap();
        let pending = Anchor {
            scope: old.scope.clone(),
            state: AnchorState::Pending {
                old_generation: generation,
                old_digest: digest,
                next_digest: anchor::digest(&next_bytes),
                txn_id: txn,
                retire_files: vec![record.file_name()],
            },
        };
        anchor::update(&old, &pending).unwrap();
        fs::rename(repository.prepared(txn), repository.root().join(MANIFEST)).unwrap();
        sync_parent(&repository.root().join(MANIFEST)).unwrap();
        let committed = repository
            .recover_pending_with(pending, "synthetic unlock", &device)
            .unwrap();
        assert!(matches!(
            committed.state,
            AnchorState::Committed {
                cleanup_txn: Some(_),
                ..
            }
        ));
        assert!(repository.root().join(record.file_name()).exists());
        assert!(repository.prior(txn).exists());

        let reopened = fixture.repository();
        let _guard = reopened.lock().unwrap();
        assert!(reopened.read_manifest().unwrap().records.is_empty());
        assert!(!reopened.root().join(record.file_name()).exists());
        assert!(!reopened.prior(txn).exists());
        assert!(matches!(
            anchor::load(&reopened.anchor_scope()).unwrap().unwrap().state,
            AnchorState::Committed {
                generation: next_generation,
                cleanup_txn: None,
                ..
            } if next_generation == generation + 1
        ));
        write_replace(&reopened.root().join(MANIFEST), &old_bytes).unwrap();
        assert!(reopened.read_manifest().is_err());
    }

    #[test]
    fn repository_lock_excludes_another_process() {
        const CHILD_PATH: &str = "COINCUBE_TEST_IMPORTED_ANCHOR_LOCK_CHILD";
        const CHILD_EXPECT_BLOCKED: &str = "COINCUBE_TEST_IMPORTED_ANCHOR_LOCK_BLOCKED";
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap();
            let acquired = FileExt::try_lock_exclusive(&file).unwrap();
            assert_eq!(acquired, std::env::var_os(CHILD_EXPECT_BLOCKED).is_none());
            return;
        }

        let fixture = Fixture::new();
        let repository = fixture.repository();
        let guard = repository.lock().unwrap();
        let lock_path = repository
            .directory
            .path()
            .join(format!("{LOCK_PREFIX}{}", repository.operation_id));
        let check_child = |blocked| {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .arg("--exact")
                .arg("services::imported_keys::tests::repository_lock_excludes_another_process")
                .env(CHILD_PATH, &lock_path);
            if blocked {
                child.env(CHILD_EXPECT_BLOCKED, "1");
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "child lock check failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        check_child(true);
        drop(guard);
        check_child(false);
    }

    #[test]
    fn owner_removal_waits_for_stable_lock_across_processes() {
        const CHILD_ROOT: &str = "COINCUBE_TEST_IMPORTED_ANCHOR_REMOVE_ROOT";
        const CHILD_MARKER: &str = "COINCUBE_TEST_IMPORTED_ANCHOR_REMOVE_MARKER";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let directory = NetworkDirectory::new(PathBuf::from(root).join("regtest"));
            let settings = Settings::from_file(&directory).unwrap();
            let cube = &settings.cubes[0];
            let attachment = cube.bitcoin_import.as_ref().unwrap().attachment().unwrap();
            let repository = LocalKeyRepository::new(&directory, cube, attachment).unwrap();
            fs::write(std::env::var_os(CHILD_MARKER).unwrap(), b"started").unwrap();
            repository.remove_operation_storage().unwrap();
            return;
        }

        let fixture = Fixture::new();
        fixture.save_cube(fixture.cube.clone());
        let repository = fixture.repository();
        let guard = repository.lock().unwrap();
        let marker = fixture.root.join("remove-child-started");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("services::imported_keys::tests::owner_removal_waits_for_stable_lock_across_processes")
            .env(CHILD_ROOT, &fixture.root)
            .env(CHILD_MARKER, &marker)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < deadline {
            assert!(
                child.try_wait().unwrap().is_none(),
                "child exited before lock attempt"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(marker.exists(), "child did not reach owner removal");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(child.try_wait().unwrap().is_none());
        assert!(repository.root().exists());
        assert!(!repository.tombstone().exists());
        drop(guard);
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "child removal failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(repository.is_removed().unwrap());
        assert!(!repository.root().exists());
    }

    #[test]
    fn renamed_pending_manifest_requires_exact_authenticated_prior() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let device = Zeroizing::new([11u8; 32]);
        let record = repository
            .admit_locked(
                repository.read_manifest().unwrap(),
                &fixture.accounts[0],
                ImportedKeySecret::new(
                    Zeroizing::new(fixture.phrases[0].clone()),
                    Zeroizing::new(String::new()),
                )
                .unwrap(),
                None,
                "synthetic unlock",
                &device,
                || true,
            )
            .unwrap();
        let old_bytes = read_manifest_bytes(&repository.root().join(MANIFEST)).unwrap();
        let next_bytes = serde_json::to_vec(&repository.manifest(vec![])).unwrap();
        let old = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        let AnchorState::Committed {
            generation, digest, ..
        } = old.state
        else {
            panic!("expected committed inventory")
        };
        let txn = Uuid::new_v4();
        write_new(&repository.prior(txn), &old_bytes).unwrap();
        write_new(&repository.prepared(txn), &next_bytes).unwrap();
        let pending = Anchor {
            scope: old.scope.clone(),
            state: AnchorState::Pending {
                old_generation: generation,
                old_digest: digest,
                next_digest: anchor::digest(&next_bytes),
                txn_id: txn,
                retire_files: vec![record.file_name()],
            },
        };
        let committed = Anchor {
            scope: old.scope,
            state: AnchorState::Committed {
                generation,
                digest,
                cleanup_txn: None,
                retire_files: vec![],
            },
        };
        anchor::update(&committed, &pending).unwrap();
        fs::rename(repository.prepared(txn), repository.root().join(MANIFEST)).unwrap();
        fs::remove_file(repository.prior(txn)).unwrap();
        assert!(repository
            .recover_pending_with(pending.clone(), "synthetic unlock", &device)
            .is_err());
        assert_eq!(
            anchor::load(&repository.anchor_scope()).unwrap(),
            Some(pending.clone())
        );
        write_new(&repository.prior(txn), b"corrupt").unwrap();
        assert!(repository
            .recover_pending_with(pending.clone(), "synthetic unlock", &device)
            .is_err());
        assert!(repository.root().join(record.file_name()).exists());
        fs::remove_file(repository.prior(txn)).unwrap();
        write_new(&repository.prior(Uuid::new_v4()), &old_bytes).unwrap();
        assert!(repository
            .recover_pending_with(pending, "synthetic unlock", &device)
            .is_err());
    }

    #[test]
    fn populated_unanchored_repository_is_not_silently_provisioned() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let account = anchor::account(&repository.anchor_scope()).unwrap();
        anchor::tests::store_remove(&account);
        let record =
            LocalKeyRecord::from_account(&fixture.accounts[0], Uuid::new_v4(), Uuid::new_v4());
        write_new(
            &repository.root().join(record.file_name()),
            b"legacy-ciphertext",
        )
        .unwrap();
        write_replace(
            &repository.root().join(MANIFEST),
            &serde_json::to_vec(&repository.manifest(vec![record])).unwrap(),
        )
        .unwrap();
        assert!(repository.provision_empty().is_err());
        assert!(anchor::load(&repository.anchor_scope()).unwrap().is_none());
    }

    #[test]
    fn first_provision_discards_only_exact_empty_orphan_tmp() {
        for (exact_empty, unknown_key) in [(true, false), (false, false), (true, true)] {
            let fixture = Fixture::new();
            let repository = fixture.repository();
            let account = anchor::account(&repository.anchor_scope()).unwrap();
            anchor::tests::store_remove(&account);
            fs::remove_file(repository.root().join(MANIFEST)).unwrap();
            let tmp = repository
                .root()
                .join(format!(".manifest-{}.tmp", Uuid::new_v4()));
            let bytes = serde_json::to_vec(&repository.manifest(vec![])).unwrap();
            write_new(
                &tmp,
                if exact_empty {
                    bytes.as_slice()
                } else {
                    b"{invalid"
                },
            )
            .unwrap();
            if unknown_key {
                write_new(&repository.root().join("unidentified.key"), b"opaque").unwrap();
            }
            let result = repository.provision_empty();
            assert_eq!(result.is_ok(), exact_empty && !unknown_key);
            assert_eq!(tmp.exists(), !exact_empty || unknown_key);
            assert_eq!(
                anchor::load(&repository.anchor_scope()).unwrap().is_some(),
                exact_empty && !unknown_key
            );
        }
    }

    #[test]
    fn public_only_removal_rejects_any_hidden_or_local_key_evidence() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let hidden = repository.root().join(".unknown");
        write_new(&hidden, b"unknown").unwrap();
        assert!(repository
            .remove_public_only_storage_without_anchor()
            .is_err());
        fs::remove_file(&hidden).unwrap();
        let key = repository
            .root()
            .join(format!("{}-{}.key", Uuid::new_v4(), Uuid::new_v4()));
        write_new(&key, b"unknown").unwrap();
        assert!(repository
            .remove_public_only_storage_without_anchor()
            .is_err());
        fs::remove_file(&key).unwrap();
        repository
            .remove_public_only_storage_without_anchor()
            .unwrap();
        assert!(repository.is_removed().unwrap());
        assert!(!repository.root().exists());
    }

    #[test]
    fn anchored_operation_does_not_reprovision_after_directory_move() {
        let fixture = Fixture::new();
        let repository = fixture.repository();
        let moved = repository.root().with_extension("moved");
        fs::rename(repository.root(), &moved).unwrap();
        repository.ensure_root().unwrap();
        assert!(repository.read_manifest().is_err());
        repository.provision_empty().unwrap(); // Existing anchor is not replaced.
        assert!(repository.read_manifest().is_err());
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
    fn reopened_revocation_rejects_restored_public_directory() {
        let fixture = Fixture::new();
        fixture.save_cube(fixture.cube.clone());
        let repository = fixture.repository();
        let old_bytes = read_manifest_bytes(&repository.root().join(MANIFEST)).unwrap();
        repository.remove_operation_storage().unwrap();
        let revoked = anchor::load(&repository.anchor_scope()).unwrap().unwrap();
        assert!(matches!(&revoked.state, AnchorState::Revoked { .. }));

        // Simulate restoring an old directory together with its old public
        // manifest. The external revocation remains the authority.
        fs::remove_file(repository.tombstone()).unwrap();
        repository.ensure_root().unwrap();
        write_replace(&repository.root().join(MANIFEST), &old_bytes).unwrap();
        let directory = NetworkDirectory::new(fixture.directory.path().to_path_buf());
        let settings = Settings::from_file(&directory).unwrap();
        let cube = &settings.cubes[0];
        let attachment = cube.bitcoin_import.as_ref().unwrap().attachment().unwrap();
        let reopened = LocalKeyRepository::new(&directory, cube, attachment).unwrap();
        let _guard = reopened.lock().unwrap();
        assert!(reopened.read_manifest().is_err());
        reopened.provision_empty().unwrap();
        assert_eq!(
            anchor::load(&reopened.anchor_scope()).unwrap(),
            Some(revoked)
        );
    }

    #[test]
    fn owner_removal_accepts_corrupt_or_missing_inventory() {
        for corrupt_manifest in [true, false] {
            let fixture = Fixture::new();
            let repository = fixture.repository();
            repository.ensure_root().unwrap();
            write_new(&repository.root().join("orphan.key"), b"opaque ciphertext").unwrap();
            if corrupt_manifest {
                write_replace(&repository.root().join(MANIFEST), b"{invalid").unwrap();
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
