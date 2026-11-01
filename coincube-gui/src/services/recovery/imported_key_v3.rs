//! Password-Kit payload for imported local signers. This module deliberately
//! does not enter the Clone UI restore enum or grant signer admission. A fresh
//! device must first compare this payload with a trusted Connect checkpoint.

use std::{collections::HashSet, fmt, str::FromStr};

use coincube_core::{
    imported_key_crypt::ImportedKeySecret,
    imported_keys::{verify_imported_account, ImportedAccount},
    miniscript::bitcoin::{
        bip32::{DerivationPath, Fingerprint, Xpub},
        hashes::{sha256, Hash},
    },
};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    app::settings::WalletId, phone_signer::policy::PhonePolicy,
    services::imported_keys::LocalKeyRecord,
};

use super::{decrypt, encrypt, imported::ImportedDescriptorBlob, KdfParams};

pub(crate) const IMPORTED_KEY_BLOB_VERSION: u8 = 3;
const MAX_LOCAL_KEYS: usize = 32;
const MAX_PLAINTEXT_BYTES: usize = 64 * 1024;
const MAX_CIPHERTEXT_B64_BYTES: usize = 128 * 1024;

/// Secrets remain owned by `ImportedKeySecret`, which is redacted, non-Clone,
/// and zeroizes both strings on drop. The public record is the exact revision
/// that the inventory anchor admits, not a hint from signer metadata.
pub(crate) struct KitKey {
    pub record: LocalKeyRecord,
    pub secret: ImportedKeySecret,
}

/// A version-3 password payload. The outer encrypted envelope is bound to
/// `public.cube.uuid`; the Connect checkpoint is a separate restore gate.
pub(crate) struct ImportedKeyKitV3 {
    pub public: ImportedDescriptorBlob,
    pub operation_id: Uuid,
    pub wallet_id: String,
    pub policy_hash: String,
    pub inventory_generation: u64,
    pub local_keys: Vec<KitKey>,
}

impl fmt::Debug for ImportedKeyKitV3 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportedKeyKitV3")
            .field("version", &IMPORTED_KEY_BLOB_VERSION)
            .field("local_key_count", &self.local_keys.len())
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct WireRef<'a> {
    version: u8,
    public: &'a ImportedDescriptorBlob,
    operation_id: Uuid,
    wallet_id: &'a str,
    policy_hash: &'a str,
    inventory_generation: u64,
    local_keys: Vec<WireKeyRef<'a>>,
}

#[derive(Serialize)]
struct WireKeyRef<'a> {
    record: &'a LocalKeyRecord,
    words: &'a str,
    passphrase: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireOwned {
    version: u8,
    public: ImportedDescriptorBlob,
    operation_id: Uuid,
    wallet_id: String,
    policy_hash: String,
    inventory_generation: u64,
    local_keys: Vec<WireKeyOwned>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireKeyOwned {
    record: LocalKeyRecord,
    words: SecretText,
    passphrase: SecretText,
}

/// Wrap each parsed secret string immediately; never deserialize through a
/// generic JSON Value or a Clone-bearing UI message.
struct SecretText(Zeroizing<String>);

impl<'de> Deserialize<'de> for SecretText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self(Zeroizing::new(value)))
    }
}

impl ImportedKeyKitV3 {
    pub fn validate(&self) -> Result<(), String> {
        let chain = self.public.wallet.descriptor.chain();
        if chain.is_blake2b()
            || self.public.validate(chain).is_err()
            || self.public.cube.uuid.len() > 128
            || self.operation_id.is_nil()
            || self.public.import_operation_id != Some(self.operation_id)
            || self.inventory_generation == 0
            || self.local_keys.is_empty()
            || self.local_keys.len() > MAX_LOCAL_KEYS
        {
            return Err("Imported key Kit has an invalid scope or inventory generation.".into());
        }
        let expected_id = WalletId::for_imported(&self.public.wallet.descriptor, chain)
            .map_err(|_| "Imported key Kit wallet identity is invalid.")?;
        if self.wallet_id != expected_id.to_string() {
            return Err("Imported key Kit wallet identity differs from its descriptor.".into());
        }
        let policy =
            PhonePolicy::imported(self.public.wallet.descriptor.clone(), chain, expected_id)
                .map_err(|_| "Imported key Kit policy is invalid.")?;
        let expected_policy =
            hex::encode(sha256::Hash::hash(policy.descriptor().as_bytes()).to_byte_array());
        if self.policy_hash != expected_policy {
            return Err("Imported key Kit policy commitment differs from its descriptor.".into());
        }
        let mut records = HashSet::new();
        let mut accounts = HashSet::new();
        for key in &self.local_keys {
            if key.record.record_id.is_nil()
                || key.record.revision.is_nil()
                || !records.insert(key.record.record_id)
            {
                return Err("Imported key Kit has a duplicate or invalid record.".into());
            }
            let account = ImportedAccount {
                fingerprint: Fingerprint::from_str(&key.record.fingerprint)
                    .map_err(|_| "Imported key Kit fingerprint is invalid.")?,
                origin: DerivationPath::from_str(&key.record.origin)
                    .map_err(|_| "Imported key Kit origin is invalid.")?,
                xpub: Xpub::from_str(&key.record.xpub)
                    .map_err(|_| "Imported key Kit xpub is invalid.")?,
            };
            if account.fingerprint.to_string() != key.record.fingerprint
                || account.origin.to_string() != key.record.origin
                || account.xpub.to_string() != key.record.xpub
            {
                return Err("Imported key Kit account encoding is noncanonical.".into());
            }
            if !accounts.insert((account.fingerprint, account.origin.clone(), account.xpub)) {
                return Err("Imported key Kit has a duplicate account.".into());
            }
            let verified = verify_imported_account(
                &self.public.wallet.descriptor,
                &account.origin,
                Zeroizing::new(key.secret.words().to_owned()),
                Zeroizing::new(key.secret.passphrase().to_owned()),
            )
            .map_err(|_| "Imported key Kit signer does not match the wallet.")?;
            if verified.account() != &account {
                return Err("Imported key Kit record differs from its signer.".into());
            }
        }
        Ok(())
    }

    /// Return zeroizing plaintext for the password envelope only. Callers
    /// must encrypt it before any persistence or network operation.
    pub fn encode_password_plaintext(&self) -> Result<Zeroizing<Vec<u8>>, String> {
        self.validate()?;
        let mut bytes = Zeroizing::new(Vec::new());
        let keys = self
            .local_keys
            .iter()
            .map(|key| WireKeyRef {
                record: &key.record,
                words: key.secret.words(),
                passphrase: key.secret.passphrase(),
            })
            .collect();
        let wire = WireRef {
            version: IMPORTED_KEY_BLOB_VERSION,
            public: &self.public,
            operation_id: self.operation_id,
            wallet_id: &self.wallet_id,
            policy_hash: &self.policy_hash,
            inventory_generation: self.inventory_generation,
            local_keys: keys,
        };
        serde_json::to_writer(&mut *bytes, &wire).map_err(|_| "Cannot encode imported key Kit.")?;
        if bytes.len() > MAX_PLAINTEXT_BYTES {
            return Err("Imported key Kit exceeds its size limit.".into());
        }
        Ok(bytes)
    }

    pub fn decode_password_plaintext(bytes: &[u8], cube_id: &str) -> Result<Self, String> {
        if bytes.len() > MAX_PLAINTEXT_BYTES {
            return Err("Imported key Kit exceeds its size limit.".into());
        }
        let wire: WireOwned = parse(bytes)?;
        if wire.version != IMPORTED_KEY_BLOB_VERSION || wire.public.cube.uuid != cube_id {
            return Err("Imported key Kit version or Cube does not match.".into());
        }
        let keys = wire
            .local_keys
            .into_iter()
            .map(|key| {
                Ok(KitKey {
                    record: key.record,
                    secret: ImportedKeySecret::new(key.words.0, key.passphrase.0)
                        .map_err(|_| "Imported key Kit phrase is invalid.")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let kit = Self {
            public: wire.public,
            operation_id: wire.operation_id,
            wallet_id: wire.wallet_id,
            policy_hash: wire.policy_hash,
            inventory_generation: wire.inventory_generation,
            local_keys: keys,
        };
        kit.validate()?;
        Ok(kit)
    }

    pub fn encrypt_password(&self, password: &Zeroizing<String>) -> Result<String, String> {
        let plaintext = self.encode_password_plaintext()?;
        encrypt(
            &plaintext,
            password,
            KdfParams::SEED,
            &self.public.cube.uuid,
        )
        .map_err(|_| "Cannot encrypt imported key Kit.".into())
    }

    pub fn decrypt_password(
        ciphertext: &str,
        password: &Zeroizing<String>,
        cube_id: &str,
    ) -> Result<Self, String> {
        if ciphertext.len() > MAX_CIPHERTEXT_B64_BYTES {
            return Err("Imported key Kit ciphertext exceeds its size limit.".into());
        }
        let plaintext = decrypt(ciphertext, password, cube_id)
            .map_err(|_| "Recovery password is incorrect or the Kit is corrupted.")?;
        Self::decode_password_plaintext(&plaintext, cube_id)
    }
}

fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    serde_json::from_slice(bytes).map_err(|_| "Imported key Kit shape is invalid.".into())
}

#[cfg(test)]
mod tests {
    use coincube_core::{
        imported_key_crypt::ImportedKeySecret,
        miniscript::bitcoin::{
            bip32::{DerivationPath, Xpriv, Xpub},
            secp256k1::Secp256k1,
            Network,
        },
        wallet_descriptor::ImportedWalletDescriptor,
    };

    use super::*;
    use crate::{
        chain::ChainId,
        services::recovery::{
            imported::ImportedDescriptorBlobWallet, parse_wallet_descriptor_blob,
        },
    };

    fn sample() -> ImportedKeyKitV3 {
        let words = bip39::Mnemonic::from_entropy(&[19u8; 16])
            .unwrap()
            .to_string();
        let secp = Secp256k1::new();
        let mnemonic = bip39::Mnemonic::parse(&words).unwrap();
        let master = Xpriv::new_master(Network::Bitcoin, &mnemonic.to_seed("")).unwrap();
        let path = DerivationPath::from_str("m/84h/0h/0h").unwrap();
        let xpub = Xpub::from_priv(&secp, &master.derive_priv(&secp, &path).unwrap());
        let fingerprint = master.fingerprint(&secp);
        let key = format!("[{fingerprint}/84h/0h/0h]{xpub}");
        let descriptor = ImportedWalletDescriptor::parse(
            &format!("wpkh({key}/0/*)"),
            Some(&format!("wpkh({key}/1/*)")),
            ChainId::Bitcoin,
        )
        .unwrap();
        let operation_id = Uuid::new_v4();
        let public = ImportedDescriptorBlob::new_with_pair(
            "42".into(),
            ChainId::Bitcoin,
            ImportedDescriptorBlobWallet {
                name: "Private wallet".into(),
                descriptor: descriptor.clone(),
                birthday: 0,
                initial_lookahead: 300,
                receive_index: 0,
                change_index: 0,
            },
            None,
            None,
            Some(operation_id),
        )
        .unwrap();
        let wallet_id = WalletId::for_imported(&descriptor, ChainId::Bitcoin).unwrap();
        let policy =
            PhonePolicy::imported(descriptor, ChainId::Bitcoin, wallet_id.clone()).unwrap();
        ImportedKeyKitV3 {
            public,
            operation_id,
            wallet_id: wallet_id.to_string(),
            policy_hash: hex::encode(
                sha256::Hash::hash(policy.descriptor().as_bytes()).to_byte_array(),
            ),
            inventory_generation: 1,
            local_keys: vec![KitKey {
                record: LocalKeyRecord {
                    record_id: Uuid::new_v4(),
                    revision: Uuid::new_v4(),
                    fingerprint: fingerprint.to_string(),
                    origin: path.to_string(),
                    xpub: xpub.to_string(),
                },
                secret: ImportedKeySecret::new(
                    Zeroizing::new(words),
                    Zeroizing::new(String::new()),
                )
                .unwrap(),
            }],
        }
    }

    #[test]
    fn password_v3_roundtrip_is_secret_bearing_but_not_a_ui_blob() {
        let kit = sample();
        let plaintext = kit.encode_password_plaintext().unwrap();
        assert!(parse_wallet_descriptor_blob(&plaintext).is_err());
        assert!(!format!("{kit:?}").contains(kit.local_keys[0].secret.words()));
        let decoded = ImportedKeyKitV3::decode_password_plaintext(&plaintext, "42").unwrap();
        assert_eq!(decoded.local_keys[0].record, kit.local_keys[0].record);
        assert!(ImportedKeyKitV3::decode_password_plaintext(&plaintext, "other").is_err());
        let password = Zeroizing::new("test password".to_owned());
        let encrypted = kit.encrypt_password(&password).unwrap();
        assert!(ImportedKeyKitV3::decrypt_password(&encrypted, &password, "42").is_ok());
        assert!(ImportedKeyKitV3::decrypt_password(&encrypted, &password, "43").is_err());
        assert!(ImportedKeyKitV3::decrypt_password(
            &"A".repeat(MAX_CIPHERTEXT_B64_BYTES + 1),
            &password,
            "42"
        )
        .is_err());
    }

    #[test]
    fn password_v3_rejects_unknown_version_and_wrong_scope() {
        let mut kit = sample();
        let plaintext = kit.encode_password_plaintext().unwrap();
        let changed = String::from_utf8(plaintext.to_vec()).unwrap().replacen(
            "\"version\":3",
            "\"version\":4",
            1,
        );
        assert!(ImportedKeyKitV3::decode_password_plaintext(changed.as_bytes(), "42").is_err());
        kit.policy_hash = "00".repeat(32);
        assert!(kit.validate().is_err());
        kit = sample();
        kit.inventory_generation = 0;
        assert!(kit.validate().is_err());
        kit = sample();
        kit.operation_id = Uuid::new_v4();
        assert!(kit.validate().is_err());
    }

    #[test]
    fn password_v3_rejects_wrong_key_and_duplicate_record() {
        let mut kit = sample();
        kit.local_keys[0].secret = ImportedKeySecret::new(
            Zeroizing::new(kit.local_keys[0].secret.words().to_owned()),
            Zeroizing::new("wrong passphrase".to_owned()),
        )
        .unwrap();
        assert!(kit.validate().is_err());
        let mut kit = sample();
        let duplicate = KitKey {
            record: kit.local_keys[0].record.clone(),
            secret: ImportedKeySecret::new(
                Zeroizing::new(kit.local_keys[0].secret.words().to_owned()),
                Zeroizing::new(String::new()),
            )
            .unwrap(),
        };
        kit.local_keys.push(duplicate);
        assert!(kit.validate().is_err());
        kit.local_keys[1].record.revision = Uuid::new_v4();
        assert!(kit.validate().is_err());
    }
}
