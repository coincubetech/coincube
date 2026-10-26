//! Authenticated, device-local client registry. Never a source of Cube unlock.
use chacha20poly1305::{
    aead::{Aead, Payload},
    ChaCha20Poly1305, KeyInit, Nonce,
};
use coincube_mcp::pairing::{check_private, protect_new};
use fs4::fs_std::FileExt;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
};
use zeroize::{Zeroize, Zeroizing};

const LIMIT: u64 = 128 * 1024;
pub(super) const MAX_CLIENTS: usize = 16;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    root: PathBuf,
    chain: String,
    cube: String,
}

#[derive(Clone)]
pub(super) struct Location {
    pub directory: PathBuf,
    pub account: String,
    binding: Binding,
    datadir: PathBuf,
}
impl Location {
    pub fn matches(&self, root: &Path, chain: crate::chain::ChainId, cube: &str) -> bool {
        root.canonicalize()
            .is_ok_and(|root| root == self.binding.root)
            && self.binding.chain == chain.dir_name()
            && self.binding.cube == cube
    }
    pub fn chain_matches(&self, chain: crate::chain::ChainId) -> bool {
        self.binding.chain == chain.dir_name()
    }
    pub fn new(datadir: &Path, chain: crate::chain::ChainId, cube: &str) -> io::Result<Self> {
        let root = datadir.canonicalize()?;
        let binding = Binding {
            root: root.clone(),
            chain: chain.dir_name().into(),
            cube: cube.into(),
        };
        let digest = hex::encode(Sha256::digest(serde_json::to_vec(&binding)?));
        Ok(Self {
            directory: root
                .join(chain.dir_name())
                .join("mcp-pairings")
                .join(&digest),
            account: format!("mcp-pairing-v1-{digest}"),
            binding,
            datadir: root,
        })
    }
    pub fn exists(&self) -> bool {
        self.directory.exists()
    }
    pub fn locator(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.endpoint"))
    }
    pub fn open(&self, create: bool) -> io::Result<Store> {
        let key = if create {
            crate::services::unlock::device_secret::get_or_create(&self.datadir, &self.account)
        } else {
            // An existing registry never provisions a replacement wrapping key.
            crate::services::unlock::device_secret::load(&self.account)
        }
        .map_err(|_| io::Error::other("pairing keystore unavailable"))?;
        Store::open(self.clone(), key, create)
    }
    pub fn invalidate_key(&self) -> io::Result<()> {
        crate::services::unlock::device_secret::delete(&self.account)
            .map_err(|_| io::Error::other("pairing revocation not durable"))
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum Scope {
    Read {
        wallet: String,
        descriptor: String,
        opaque: String,
        capabilities: [bool; 3],
    },
    Proposal,
    /// Separately consented public-key assembly; never native creation approval.
    WalletCreation,
    /// Separate unsigned transaction permission; does not include reads/signing.
    TransactionPreparation {
        wallet: String,
        descriptor: String,
        opaque: String,
    },
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Client {
    pub id: String,
    pub name: String,
    pub credential: String,
    pub scope: Scope,
    pub created_at: u64,
}
impl Drop for Client {
    fn drop(&mut self) {
        self.credential.zeroize();
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    binding: Binding,
    enabled: bool,
    clients: Vec<Client>,
}

/// Whether a failed addition is known not to have changed stored authorization.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AddError {
    Unchanged,
    Uncertain,
}

/// Finish the durability barrier that may have failed after atomic replacement.
fn confirm_directory(directory: &Path) -> io::Result<()> {
    check_private(directory, true)?;
    #[cfg(unix)]
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

/// The owner lock lives as long as any endpoint for this registry. A competing
/// process cannot restore a stale copy or overwrite another process's locator.
pub(super) struct Store {
    pub location: Location,
    key: Zeroizing<[u8; 32]>,
    document: Document,
    _owner: Arc<fs::File>,
}
impl Store {
    pub(super) fn open(
        location: Location,
        key: Zeroizing<[u8; 32]>,
        create: bool,
    ) -> io::Result<Self> {
        let parent = location
            .directory
            .parent()
            .ok_or_else(|| io::Error::other("invalid pairing path"))?;
        for path in [parent, location.directory.as_path()] {
            match fs::create_dir(path) {
                Ok(()) => protect_new(path, true)?,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => check_private(path, true)?,
                Err(e) => return Err(e),
            }
        }
        if location.directory.canonicalize()? != location.directory {
            return Err(io::Error::other("pairing path redirected"));
        }
        let lock = location.directory.join("owner.lock");
        let owner = match fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lock)
        {
            Ok(file) => {
                protect_new(&lock, false)?;
                file
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                check_private(&lock, false)?;
                fs::OpenOptions::new().read(true).write(true).open(&lock)?
            }
            Err(e) => return Err(e),
        };
        if !owner.try_lock_exclusive()? {
            return Err(io::Error::other("pairing registry already open"));
        }
        let path = location.directory.join("clients.enc");
        let document = if path.exists() {
            Self::read_document(&location, &key)?
        } else if create {
            Document {
                version: 1,
                binding: location.binding.clone(),
                enabled: true,
                clients: Vec::new(),
            }
        } else {
            return Err(io::Error::other("pairing registry missing"));
        };
        let store = Self {
            location,
            key,
            document,
            _owner: Arc::new(owner),
        };
        if create && !path.exists() {
            store.persist(&store.document)?;
        }
        Ok(store)
    }
    /// Reread under the existing owner lock, preserving every startup validation.
    fn read_document(location: &Location, key: &[u8; 32]) -> io::Result<Document> {
        check_private(&location.directory, true)?;
        if location.directory.canonicalize()? != location.directory {
            return Err(io::Error::other("pairing path redirected"));
        }
        let path = location.directory.join("clients.enc");
        check_private(&path, false)?;
        let file = fs::File::open(&path)?;
        if file.metadata()?.len() > LIMIT {
            return Err(io::Error::other("pairing registry too large"));
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() < 33 || &bytes[..5] != b"TMCP\x01" {
            return Err(io::Error::other("invalid pairing registry"));
        }
        let cipher = ChaCha20Poly1305::new_from_slice(key.as_slice())
            .map_err(|_| io::Error::other("pairing key unavailable"))?;
        let aad = serde_json::to_vec(&location.binding)?;
        let plain = Zeroizing::new(
            cipher
                .decrypt(
                    Nonce::from_slice(&bytes[5..17]),
                    Payload {
                        msg: &bytes[17..],
                        aad: &aad,
                    },
                )
                .map_err(|_| io::Error::other("pairing registry authentication failed"))?,
        );
        let document: Document = serde_json::from_slice(&plain)
            .map_err(|_| io::Error::other("invalid pairing registry"))?;
        if document.version != 1
            || document.binding != location.binding
            || document.clients.len() > MAX_CLIENTS
        {
            return Err(io::Error::other("invalid pairing scope"));
        }
        let mut ids = std::collections::HashSet::new();
        for client in &document.clients {
            if client.id.len() != 32
                || !client.id.bytes().all(|b| b.is_ascii_hexdigit())
                || !ids.insert(client.id.clone())
                || client.credential.len() != 64
                || !client.credential.bytes().all(|b| b.is_ascii_hexdigit())
                || client.name.is_empty()
                || client.name.chars().count() > 64
                || client.name.chars().any(char::is_control)
            {
                return Err(io::Error::other("invalid pairing client"));
            }
        }
        Ok(document)
    }
    pub fn owner(&self) -> Arc<fs::File> {
        self._owner.clone()
    }
    pub fn enabled(&self) -> bool {
        self.document.enabled
    }
    pub fn clients(&self) -> &[Client] {
        &self.document.clients
    }
    fn persist(&self, document: &Document) -> io::Result<()> {
        check_private(&self.location.directory, true)?;
        let plain = Zeroizing::new(serde_json::to_vec(document)?);
        let mut nonce = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let aad = serde_json::to_vec(&self.location.binding)?;
        let cipher = ChaCha20Poly1305::new_from_slice(self.key.as_ref())
            .map_err(|_| io::Error::other("pairing key unavailable"))?;
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plain,
                    aad: &aad,
                },
            )
            .map_err(|_| io::Error::other("pairing persistence failed"))?;
        let mut bytes = b"TMCP\x01".to_vec();
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&sealed);
        coincube_mcp::pairing::write_private(&self.location.directory.join("clients.enc"), &bytes)
    }
    /// A failed add preserves existing consent only when disk still matches it.
    /// An exact replacement also requires the directory durability barrier before
    /// it can be acknowledged. Unknown state must suspend/invalidate the registry.
    fn reconcile_add(
        &mut self,
        next: Document,
        confirm: impl FnOnce(&Path) -> io::Result<()>,
    ) -> Result<(), AddError> {
        match Self::read_document(&self.location, &self.key) {
            Ok(saved) if saved == self.document => Err(AddError::Unchanged),
            Ok(saved) if saved == next => {
                confirm(&self.location.directory).map_err(|_| AddError::Uncertain)?;
                self.document = next;
                Ok(())
            }
            _ => Err(AddError::Uncertain),
        }
    }
    pub fn add(&mut self, name: String, scope: Scope) -> Result<String, AddError> {
        self.add_with(name, scope, Self::persist)
    }
    fn add_with(
        &mut self,
        name: String,
        scope: Scope,
        persist: impl FnOnce(&Self, &Document) -> io::Result<()>,
    ) -> Result<String, AddError> {
        if self.document.clients.len() >= MAX_CLIENTS {
            return Err(AddError::Unchanged);
        }
        let mut random = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let id = uuid::Uuid::new_v4().simple().to_string();
        let client = Client {
            id: id.clone(),
            name,
            credential: hex::encode(random),
            scope,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| AddError::Unchanged)?
                .as_secs(),
        };
        let mut next = self.document.clone();
        next.clients.push(client);
        if persist(self, &next).is_err() {
            self.reconcile_add(next, confirm_directory)?;
        } else {
            self.document = next;
        }
        Ok(id)
    }
    pub fn remove(&mut self, id: &str) -> io::Result<()> {
        let mut next = self.document.clone();
        next.clients.retain(|c| c.id != id);
        self.persist(&next)?;
        self.document = next;
        Ok(())
    }
    pub fn set_enabled(&mut self, enabled: bool) -> io::Result<()> {
        let mut next = self.document.clone();
        next.enabled = enabled;
        self.persist(&next)?;
        self.document = next;
        Ok(())
    }
}

pub(super) fn descriptor_identity(wallet: &super::Wallet) -> String {
    hex::encode(Sha256::digest(
        wallet.main_descriptor.to_string().as_bytes(),
    ))
}

pub(super) fn remove_cube(root: &Path, chain: crate::chain::ChainId, cube: &str) -> io::Result<()> {
    let location = Location::new(root, chain, cube)?;
    remove_cube_at(&location, || location.invalidate_key())
}

fn remove_cube_at(
    location: &Location,
    invalidate_key: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    match fs::symlink_metadata(&location.directory) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return invalidate_key(),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    check_private(&location.directory, true)?;
    if location.directory.canonicalize()? != location.directory {
        return Err(io::Error::other("pairing path redirected"));
    }
    let lock = location.directory.join("owner.lock");
    let _owner = match check_private(&lock, false) {
        Ok(()) => {
            let owner = fs::OpenOptions::new().read(true).write(true).open(lock)?;
            if !owner.try_lock_exclusive()? {
                return Err(io::Error::other("paired Cube is open in another process"));
            }
            Some(owner)
        }
        // A previous removal can have deleted owner.lock before failing to
        // remove another file. Let the same Cube deletion finish on retry.
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    fs::remove_dir_all(&location.directory)?;
    // If this fails, the absent-directory branch retries key invalidation.
    invalidate_key()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn location(root: &Path, cube: &str) -> Location {
        fs::create_dir_all(root.join("regtest")).unwrap();
        Location::new(root, crate::chain::ChainId::Regtest, cube).unwrap()
    }
    #[test]
    fn cube_removal_retries_after_partial_directory_or_key_deletion() {
        let root = std::env::temp_dir().join(format!("mcp-delete-retry-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let location = location(&root, "retry");
        fs::create_dir_all(&location.directory).unwrap();
        protect_new(&location.directory, true).unwrap();
        fs::write(location.directory.join("clients.enc"), b"partial").unwrap();

        // A prior remove_dir_all may have removed owner.lock first. A keyring
        // failure after directory removal is also retryable without creating
        // or reading any real keystore entry in this synthetic test.
        let mut invalidations = 0;
        assert!(remove_cube_at(&location, || {
            invalidations += 1;
            Err(io::Error::other("synthetic keyring failure"))
        })
        .is_err());
        assert!(!location.directory.exists());
        remove_cube_at(&location, || {
            invalidations += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(invalidations, 2);
        fs::remove_dir_all(root).unwrap();
    }
    fn fixture() -> (PathBuf, Store) {
        let root = std::env::temp_dir().join(format!("mcp-add-recovery-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let mut store = Store::open(
            location(&root, "add-recovery"),
            Zeroizing::new([41; 32]),
            true,
        )
        .unwrap();
        store.add("existing".into(), Scope::Proposal).unwrap();
        (root, store)
    }
    fn next_document(store: &Store) -> Document {
        let mut next = store.document.clone();
        let mut client = next.clients[0].clone();
        client.id = uuid::Uuid::new_v4().simple().to_string();
        client.name = "new".into();
        client.credential = hex::encode([47; 32]);
        next.clients.push(client);
        next
    }
    #[test]
    fn failed_add_before_replacement_preserves_existing_client_and_allows_retry() {
        let (root, mut store) = fixture();
        let old = store.document.clone();
        assert_eq!(
            store.add_with("new".into(), Scope::Proposal, |_, _| {
                Err(io::Error::other("write failed before replacement"))
            }),
            Err(AddError::Unchanged)
        );
        assert!(store.document == old);
        assert!(Store::read_document(&store.location, &store.key).unwrap() == old);
        // The existing registry/owner/key remain usable, including after restart.
        let location = store.location.clone();
        drop(store);
        let mut store = Store::open(location, Zeroizing::new([41; 32]), false).unwrap();
        assert!(store.document == old);
        store.add("retry".into(), Scope::Proposal).unwrap();
        assert_eq!(store.clients().len(), 2);
        assert!(store.clients()[0] == old.clients[0]);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn failed_add_after_replacement_adopts_exact_client_after_durability_retry() {
        let (root, mut store) = fixture();
        let mut attempted = None;
        let id = store
            .add_with("new".into(), Scope::Proposal, |store, next| {
                attempted = Some(next.clone());
                store.persist(next)?;
                Err(io::Error::other("directory sync failed after replacement"))
            })
            .unwrap();
        let attempted = attempted.unwrap();
        assert!(
            store.document == attempted,
            "do not regenerate consent or credentials"
        );
        assert_eq!(store.clients()[1].id, id);
        let location = store.location.clone();
        drop(store);
        let store = Store::open(location, Zeroizing::new([41; 32]), false).unwrap();
        assert!(store.document == attempted);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn visible_add_with_failed_durability_retry_remains_uncertain() {
        let (root, mut store) = fixture();
        let old = store.document.clone();
        let next = next_document(&store);
        store.persist(&next).unwrap();
        assert_eq!(
            store.reconcile_add(next, |_| Err(io::Error::other(
                "directory sync still failed"
            ))),
            Err(AddError::Uncertain)
        );
        assert!(
            store.document == old,
            "uncertain writes must not activate a new runtime"
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unknown_failed_add_state_is_never_reported_as_unchanged_or_successful() {
        for failure in ["different", "invalid", "corrupt", "missing", "not-file"] {
            let (root, mut store) = fixture();
            let old = store.document.clone();
            let next = next_document(&store);
            let path = store.location.directory.join("clients.enc");
            match failure {
                "different" => {
                    let mut other = next.clone();
                    other.clients[0].name = "unexpected replacement".into();
                    store.persist(&other).unwrap();
                }
                "invalid" => {
                    let mut invalid = next.clone();
                    invalid.clients[1].id = invalid.clients[0].id.clone();
                    store.persist(&invalid).unwrap();
                }
                "corrupt" => fs::write(&path, b"invalid authenticated registry").unwrap(),
                "missing" => fs::remove_file(&path).unwrap(),
                "not-file" => {
                    fs::remove_file(&path).unwrap();
                    fs::create_dir(&path).unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(
                store.reconcile_add(next, |_| panic!(
                    "unknown state cannot reach durability retry"
                )),
                Err(AddError::Uncertain),
                "{failure}"
            );
            assert!(store.document == old);
            drop(store);
            fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn registry_process_probe() {
        let Some(root) = std::env::var_os("TENSHU_PAIRING_LOCK_PROBE") else {
            return;
        };
        let location = location(Path::new(&root), "process-fixture");
        assert!(
            Store::open(location, Zeroizing::new([19; 32]), false).is_err(),
            "another process must not acquire the registry"
        );
    }
    #[test]
    fn competing_process_cannot_open_registry_and_failed_write_does_not_remove_client() {
        let root =
            std::env::temp_dir().join(format!("mcp-pairing-process-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let location = location(&root, "process-fixture");
        let mut store = Store::open(location.clone(), Zeroizing::new([19; 32]), true).unwrap();
        let id = store
            .add("process fixture".into(), Scope::Proposal)
            .unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app::local_mcp::pairings::tests::registry_process_probe",
                "--test-threads=1",
            ])
            .env("TENSHU_PAIRING_LOCK_PROBE", &root)
            .status()
            .unwrap();
        assert!(status.success());
        // Deterministically deny atomic replacement without permissions/racy disk pressure.
        let path = location.directory.join("clients.enc");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(store.remove(&id).is_err());
        assert_eq!(
            store.clients()[0].id,
            id,
            "failed durable removal cannot report a successful in-memory change"
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn authenticated_registry_roundtrip_revoke_and_wrong_context_fail_closed() {
        let root = std::env::temp_dir().join(format!("mcp-pairing-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let location = location(&root, "fixture");
        let mut store = Store::open(location.clone(), Zeroizing::new([7; 32]), true).unwrap();
        let id = store.add("fixture".into(), Scope::Proposal).unwrap();
        let credential = store.clients()[0].credential.clone();
        assert!(!fs::read(location.directory.join("clients.enc"))
            .unwrap()
            .windows(credential.len())
            .any(|v| v == credential.as_bytes()));
        assert!(
            Store::open(location.clone(), Zeroizing::new([7; 32]), false).is_err(),
            "competing owner must fail"
        );
        drop(store);
        assert!(Store::open(location.clone(), Zeroizing::new([8; 32]), false).is_err());
        let mut store = Store::open(location.clone(), Zeroizing::new([7; 32]), false).unwrap();
        assert_eq!(store.clients()[0].credential, credential);
        store.remove(&id).unwrap();
        drop(store);
        assert!(
            Store::open(location.clone(), Zeroizing::new([7; 32]), false)
                .unwrap()
                .clients()
                .is_empty()
        );
        let mut wrong = location.clone();
        wrong.binding.cube = "other".into();
        assert!(Store::open(wrong, Zeroizing::new([7; 32]), false).is_err());
        let path = location.directory.join("clients.enc");
        let mut bytes = fs::read(&path).unwrap();
        bytes[20] ^= 1;
        fs::write(path, bytes).unwrap();
        assert!(Store::open(location, Zeroizing::new([7; 32]), false).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
