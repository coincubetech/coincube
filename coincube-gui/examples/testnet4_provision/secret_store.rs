//! Device-only Keychain JSON records. No credentials in paths, argv or errors.
use coincube_core::miniscript::bitcoin::hashes::{sha256, Hash};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    path::Path,
};
use zeroize::Zeroizing;

type Result<T> = std::result::Result<T, &'static str>;
pub const SERVICE: &str = "io.coincube.testnet4-integration.v1";

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Locator {
    pub version: u32,
    pub service: String,
    pub account: String,
}

pub fn locator(profile: &Path, run: &str, name: &str) -> Result<Locator> {
    if !matches!(name, "phone_credentials" | "provision_backup") {
        return Err("invalid_secret_name");
    }
    let profile = fs::canonicalize(profile).map_err(|_| "secret_profile_unavailable")?;
    let identity = serde_json::to_vec(&(SERVICE, profile, run, name))
        .map_err(|_| "secret_identity_invalid")?;
    Ok(Locator {
        version: 1,
        service: SERVICE.into(),
        account: sha256::Hash::hash(&identity).to_string(),
    })
}

pub fn lock(profile: &Path, locator: &Locator) -> Result<fs::File> {
    #[cfg(unix)]
    {
        use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(profile.join(format!(".testnet4-secret-{}.lock", locator.account)))
            .map_err(|_| "secret_lock_unavailable")?;
        // SAFETY: the descriptor remains open until the complete CAS ends.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("secret_busy");
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        let _ = (profile, locator);
        Err("macos_keychain_required")
    }
}

pub trait Backend {
    fn load(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>>;
    fn add(&self, account: &str, bytes: &[u8]) -> Result<bool>;
    fn update(&self, account: &str, bytes: &[u8]) -> Result<()>;
    fn verify(&self, account: &str) -> Result<()>;
}
pub struct Keychain;
#[cfg(target_os = "macos")]
impl Backend for Keychain {
    fn load(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        coincube_gui::services::unlock::device_secret::integration::load(account)
            .map_err(|_| "keychain_unavailable")
    }
    fn add(&self, account: &str, bytes: &[u8]) -> Result<bool> {
        coincube_gui::services::unlock::device_secret::integration::add(account, bytes)
            .map_err(|_| "keychain_unavailable")
    }
    fn update(&self, account: &str, bytes: &[u8]) -> Result<()> {
        coincube_gui::services::unlock::device_secret::integration::update(account, bytes)
            .map_err(|_| "keychain_unavailable")
    }
    fn verify(&self, account: &str) -> Result<()> {
        coincube_gui::services::unlock::device_secret::integration::verify(account)
            .map_err(|_| "keychain_protection_unverified")
    }
}
#[cfg(not(target_os = "macos"))]
impl Backend for Keychain {
    fn load(&self, _: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        Err("macos_keychain_required")
    }
    fn add(&self, _: &str, _: &[u8]) -> Result<bool> {
        Err("macos_keychain_required")
    }
    fn update(&self, _: &str, _: &[u8]) -> Result<()> {
        Err("macos_keychain_required")
    }
    fn verify(&self, _: &str) -> Result<()> {
        Err("macos_keychain_required")
    }
}

/// Sorted recursively, independent of serde_json's optional preserve_order flag.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonical(&map[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        value => value.clone(),
    }
}
pub fn encode(value: &Value) -> Result<Zeroizing<Vec<u8>>> {
    if !value.is_object() {
        return Err("secret_object_required");
    }
    let bytes =
        Zeroizing::new(serde_json::to_vec(&canonical(value)).map_err(|_| "invalid_secret_json")?);
    if bytes.len() > 65_536 {
        return Err("secret_too_large");
    }
    Ok(bytes)
}
pub struct Snapshot {
    pub value: Value,
    pub sha256: String,
}
pub fn get(store: &dyn Backend, locator: &Locator) -> Result<Option<Snapshot>> {
    let Some(bytes) = store.load(&locator.account)? else {
        return Ok(None);
    };
    if bytes.len() > 65_536 {
        return Err("invalid_stored_secret");
    }
    store.verify(&locator.account)?;
    let record: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid_stored_secret")?;
    let canonical = encode(&record)?;
    if *canonical != *bytes {
        return Err("noncanonical_stored_secret");
    }
    if record["version"] != 1
        || !record["value"].is_object()
        || !record["revision_nonce"]
            .as_str()
            .is_some_and(|n| n.len() == 64 && n.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("invalid_stored_secret");
    }
    Ok(Some(Snapshot {
        value: record["value"].clone(),
        sha256: sha256::Hash::hash(&canonical).to_string(),
    }))
}
pub fn put(
    store: &dyn Backend,
    locator: &Locator,
    value: &Value,
    expected: Option<&str>,
) -> Result<String> {
    let _ = encode(value)?;
    let current = get(store, locator)?;
    if current.as_ref().map(|item| item.sha256.as_str()) != expected {
        return Err("secret_revision_conflict");
    }
    // The persisted CAS hash must not become an offline verifier for a
    // PIN-only value. This random revision nonce stays inside Keychain.
    let bytes = encode(
        &serde_json::json!({"version":1,"revision_nonce":hex::encode(rand::random::<[u8;32]>()),"value":value}),
    )?;
    if current.is_some() {
        store.update(&locator.account, &bytes)?;
    } else if !store.add(&locator.account, &bytes)? {
        return Err("secret_revision_conflict");
    }
    let readback = get(store, locator)?.ok_or("secret_readback_failed")?;
    if readback.value != *value {
        return Err("secret_readback_failed");
    }
    Ok(readback.sha256)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::{cell::RefCell, collections::HashMap};
    #[derive(Default)]
    pub struct Memory(pub RefCell<HashMap<String, Vec<u8>>>);
    impl Backend for Memory {
        fn load(&self, k: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
            Ok(self.0.borrow().get(k).cloned().map(Zeroizing::new))
        }
        fn add(&self, k: &str, b: &[u8]) -> Result<bool> {
            let mut values = self.0.borrow_mut();
            if values.contains_key(k) {
                return Ok(false);
            }
            values.insert(k.into(), b.to_vec());
            Ok(true)
        }
        fn update(&self, k: &str, b: &[u8]) -> Result<()> {
            self.0.borrow_mut().insert(k.into(), b.to_vec());
            Ok(())
        }
        fn verify(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }
    #[test]
    fn compare_and_swap_preserves_previous_revision_on_conflict() {
        let store = Memory::default();
        let id = Locator {
            version: 1,
            service: SERVICE.into(),
            account: "test".into(),
        };
        let first = serde_json::json!({"pin":"synthetic","stages":{"backup":false}});
        let revision = put(&store, &id, &first, None).unwrap();
        assert_eq!(
            put(&store, &id, &first, None),
            Err("secret_revision_conflict")
        );
        let second = serde_json::json!({"pin":"synthetic","stages":{"backup":true}});
        assert_eq!(
            put(&store, &id, &second, Some("wrong")),
            Err("secret_revision_conflict")
        );
        assert_eq!(get(&store, &id).unwrap().unwrap().sha256, revision);
        let next = put(&store, &id, &second, Some(&revision)).unwrap();
        assert_ne!(next, revision);
        assert_eq!(get(&store, &id).unwrap().unwrap().value, second);
    }
    #[test]
    fn canonical_json_sorts_nested_objects() {
        let a: Value = serde_json::from_str(r#"{"z":{"b":2,"a":1},"a":0}"#).unwrap();
        assert_eq!(&*encode(&a).unwrap(), br#"{"a":0,"z":{"a":1,"b":2}}"#);
    }
    #[test]
    fn revision_hashes_do_not_expose_a_pin_only_verifier() {
        let store = Memory::default();
        let first = Locator {
            version: 1,
            service: SERVICE.into(),
            account: "first".into(),
        };
        let second = Locator {
            version: 1,
            service: SERVICE.into(),
            account: "second".into(),
        };
        let value = serde_json::json!({"pin":"1234"});
        let a = put(&store, &first, &value, None).unwrap();
        let b = put(&store, &second, &value, None).unwrap();
        assert_ne!(a, b);
        assert_ne!(a, sha256::Hash::hash(&encode(&value).unwrap()).to_string());
        assert_eq!(get(&store, &first).unwrap().unwrap().value, value);
    }
    #[test]
    fn failed_protection_check_cannot_report_a_successful_write() {
        struct Unverified(Memory);
        impl Backend for Unverified {
            fn load(&self, k: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
                self.0.load(k)
            }
            fn add(&self, k: &str, b: &[u8]) -> Result<bool> {
                self.0.add(k, b)
            }
            fn update(&self, k: &str, b: &[u8]) -> Result<()> {
                self.0.update(k, b)
            }
            fn verify(&self, _: &str) -> Result<()> {
                Err("keychain_protection_unverified")
            }
        }
        let store = Unverified(Memory::default());
        let id = Locator {
            version: 1,
            service: SERVICE.into(),
            account: "test".into(),
        };
        assert_eq!(
            put(&store, &id, &serde_json::json!({"synthetic":true}), None),
            Err("keychain_protection_unverified")
        );
    }
    #[test]
    #[cfg(target_os = "macos")]
    #[ignore = "requires a code-signed runner with the data-protection Keychain entitlement"]
    fn signed_keychain_roundtrip_and_compare_and_swap() {
        let id = Locator {
            version: 1,
            service: SERVICE.into(),
            account: format!("offline-test-{}", uuid::Uuid::new_v4()),
        };
        let value = serde_json::json!({"synthetic_offline_probe":uuid::Uuid::new_v4().to_string()});
        let first = put(&Keychain, &id, &value, None).unwrap();
        let next = serde_json::json!({"synthetic_offline_probe":"updated"});
        assert_eq!(
            put(&Keychain, &id, &next, None),
            Err("secret_revision_conflict")
        );
        let second = put(&Keychain, &id, &next, Some(&first)).unwrap();
        assert_ne!(first, second);
        assert!(get(&Keychain, &id).unwrap().unwrap().value == next);
        coincube_gui::services::unlock::device_secret::integration::delete_test_item(&id.account)
            .unwrap();
        assert!(get(&Keychain, &id).unwrap().is_none());
    }
}
