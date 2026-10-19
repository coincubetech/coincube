//! Versioned Recovery Kit payload for an imported public Bitcoin wallet.
//! The Cube seed remains separate and is never treated as a wallet signer.

use coincube_core::wallet_descriptor::ImportedWalletDescriptor;
use serde::{Deserialize, Serialize};

use crate::chain::ChainId;

use super::plaintext::DescriptorBlobCube;

pub const IMPORTED_BLOB_VERSION: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportedBlobKind {
    Imported,
}

/// The ciphertext server only sees this as opaque `encryptedWalletDescriptor` bytes.
/// Indices are the daemon's durable *last allocated* values, not scan bounds.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedDescriptorBlob {
    pub version: u8,
    pub kind: ImportedBlobKind,
    pub cube: DescriptorBlobCube,
    pub wallet: ImportedDescriptorBlobWallet,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedDescriptorBlobWallet {
    pub name: String,
    pub descriptor: ImportedWalletDescriptor,
    pub birthday: u32,
    pub initial_lookahead: u32,
    pub receive_index: u32,
    pub change_index: u32,
}

impl std::fmt::Debug for ImportedDescriptorBlob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportedDescriptorBlob")
            .field("version", &self.version)
            .field("kind", &"imported")
            .field("cube", &self.cube)
            .field("wallet", &self.wallet)
            .finish()
    }
}

impl std::fmt::Debug for ImportedDescriptorBlobWallet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportedDescriptorBlobWallet")
            .field("name", &"<redacted>")
            .field("descriptor", &"<redacted>")
            .field("birthday", &self.birthday)
            .field("initial_lookahead", &self.initial_lookahead)
            .field("receive_index", &self.receive_index)
            .field("change_index", &self.change_index)
            .finish()
    }
}

impl ImportedDescriptorBlob {
    pub fn new(
        cube_uuid: String,
        chain: ChainId,
        wallet: ImportedDescriptorBlobWallet,
    ) -> Result<Self, String> {
        let blob = Self {
            version: IMPORTED_BLOB_VERSION,
            kind: ImportedBlobKind::Imported,
            cube: DescriptorBlobCube {
                uuid: cube_uuid,
                network: chain.api_str().to_owned(),
            },
            wallet,
        };
        blob.validate(chain)?;
        Ok(blob)
    }

    pub fn validate(&self, chain: ChainId) -> Result<(), String> {
        if self.version != IMPORTED_BLOB_VERSION
            || chain.is_blake2b()
            || self.cube.uuid.is_empty()
            || self.cube.network != chain.api_str()
            || self.wallet.descriptor.chain() != chain
            || self.wallet.descriptor.identity(chain).is_err()
            || !(1..=10_000).contains(&self.wallet.initial_lookahead)
            || self.wallet.receive_index > 1_000_000
            || self.wallet.change_index > 1_000_000
        {
            return Err(
                "Imported Recovery Kit has inconsistent wallet identity or address state".into(),
            );
        }
        Ok(())
    }
}

/// Drift includes allocated addresses so a Kit becomes stale after an unused
/// receive address has been handed out. Scan coverage is intentionally absent.
pub fn imported_descriptor_blob_fingerprint(blob: &ImportedDescriptorBlob) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(blob).ok()?;
    Some(hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use coincube_core::miniscript::bitcoin::{
        bip32::{DerivationPath, Xpriv, Xpub},
        secp256k1::Secp256k1,
        Network,
    };

    use super::*;
    use crate::services::recovery::{
        decrypt_wallet_descriptor_blob, encrypt, KdfParams, RecoveredWalletBlob,
    };

    const TEST_PARAMS: KdfParams = KdfParams {
        memory_kib: 512,
        t_cost: 1,
        p_cost: 1,
    };

    fn sample() -> ImportedDescriptorBlob {
        let secp = Secp256k1::new();
        let master = Xpriv::new_master(Network::Bitcoin, &[19; 32]).unwrap();
        let path = DerivationPath::from_str("m/84h/0h/0h").unwrap();
        let account = master.derive_priv(&secp, &path).unwrap();
        let xpub = Xpub::from_priv(&secp, &account);
        let key = format!("[{}/84h/0h/0h]{xpub}", master.fingerprint(&secp));
        let receive = format!("wpkh({key}/0/*)");
        let change = format!("wpkh({key}/1/*)");
        let descriptor =
            ImportedWalletDescriptor::parse(&receive, Some(&change), ChainId::Bitcoin).unwrap();
        ImportedDescriptorBlob::new(
            "cube-uuid".into(),
            ChainId::Bitcoin,
            ImportedDescriptorBlobWallet {
                name: "My wallet".into(),
                descriptor,
                birthday: 0,
                initial_lookahead: 300,
                receive_index: 7,
                change_index: 3,
            },
        )
        .unwrap()
    }

    #[test]
    fn imported_blob_roundtrip_and_drift_track_allocated_addresses() {
        let blob = sample();
        let json = serde_json::to_vec(&blob).unwrap();
        let decoded: ImportedDescriptorBlob = serde_json::from_slice(&json).unwrap();
        decoded.validate(ChainId::Bitcoin).unwrap();
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), json);
        let original = imported_descriptor_blob_fingerprint(&blob);
        let mut advanced = decoded;
        advanced.wallet.receive_index += 1;
        assert_ne!(original, imported_descriptor_blob_fingerprint(&advanced));
        assert!(advanced.validate(ChainId::Signet).is_err());
        assert!(!format!("{advanced:?}").contains("xpub"));
        assert!(!format!("{advanced:?}").contains("My wallet"));
    }

    #[test]
    fn imported_blob_refuses_invalid_version_and_address_index() {
        let mut blob = sample();
        blob.version = 3;
        assert!(blob.validate(ChainId::Bitcoin).is_err());
        blob.version = IMPORTED_BLOB_VERSION;
        blob.wallet.change_index = 1 << 31;
        assert!(blob.validate(ChainId::Bitcoin).is_err());
    }

    fn account_key(seed: u8, path: &str) -> String {
        let secp = Secp256k1::new();
        let master = Xpriv::new_master(Network::Bitcoin, &[seed; 32]).unwrap();
        let path = DerivationPath::from_str(path).unwrap();
        let account = master.derive_priv(&secp, &path).unwrap();
        format!(
            "[{}{}]{}",
            master.fingerprint(&secp),
            path.to_string().trim_start_matches('m'),
            Xpub::from_priv(&secp, &account)
        )
    }

    #[test]
    fn encrypted_imported_kit_preserves_all_five_script_shapes_and_signers() {
        let password = zeroize::Zeroizing::new("correct horse battery staple".to_owned());
        let scripts = [
            ("pkh", "m/44h/0h/0h"),
            ("sh(wpkh)", "m/49h/0h/0h"),
            ("wpkh", "m/84h/0h/0h"),
            ("wsh(multi)", "m/48h/0h/0h/2h"),
            ("wsh(sortedmulti)", "m/48h/0h/0h/2h"),
        ];
        for (shape, path) in scripts {
            let account_keys: Vec<_> = (1..=3).map(|seed| account_key(seed, path)).collect();
            let branch = |index: u32| match shape {
                "pkh" => format!("pkh({}/{index}/*)", account_keys[0]),
                "sh(wpkh)" => format!("sh(wpkh({}/{index}/*))", account_keys[0]),
                "wpkh" => format!("wpkh({}/{index}/*)", account_keys[0]),
                "wsh(multi)" => format!(
                    "wsh(multi(2,{}/{index}/*,{}/{index}/*,{}/{index}/*))",
                    account_keys[0], account_keys[1], account_keys[2]
                ),
                _ => format!(
                    "wsh(sortedmulti(2,{}/{index}/*,{}/{index}/*,{}/{index}/*))",
                    account_keys[0], account_keys[1], account_keys[2]
                ),
            };
            let original =
                ImportedWalletDescriptor::parse(&branch(0), Some(&branch(1)), ChainId::Bitcoin)
                    .unwrap();
            let blob = ImportedDescriptorBlob::new(
                "same-cube".into(),
                ChainId::Bitcoin,
                ImportedDescriptorBlobWallet {
                    name: shape.into(),
                    descriptor: original.clone(),
                    birthday: 0,
                    initial_lookahead: 300,
                    receive_index: 251,
                    change_index: 13,
                },
            )
            .unwrap();
            let ciphertext =
                encrypt(&serde_json::to_vec(&blob).unwrap(), &password, TEST_PARAMS).unwrap();
            let RecoveredWalletBlob::Imported(restored) =
                decrypt_wallet_descriptor_blob(&ciphertext, &password).unwrap()
            else {
                panic!("{shape}: expected imported Kit");
            };
            assert_eq!(restored.cube.uuid, "same-cube");
            assert_eq!(restored.wallet.birthday, 0);
            assert_eq!(restored.wallet.initial_lookahead, 300);
            assert_eq!(
                (restored.wallet.receive_index, restored.wallet.change_index),
                (251, 13)
            );
            assert_eq!(
                restored
                    .wallet
                    .descriptor
                    .identity(ChainId::Bitcoin)
                    .unwrap(),
                original.identity(ChainId::Bitcoin).unwrap()
            );
            for index in [0, 1, 17, 251] {
                assert_eq!(
                    restored
                        .wallet
                        .descriptor
                        .receive_descriptor()
                        .at_derivation_index(index)
                        .unwrap()
                        .script_pubkey(),
                    original
                        .receive_descriptor()
                        .at_derivation_index(index)
                        .unwrap()
                        .script_pubkey(),
                    "{shape} receive {index}"
                );
                assert_eq!(
                    restored
                        .wallet
                        .descriptor
                        .change_descriptor()
                        .at_derivation_index(index)
                        .unwrap()
                        .script_pubkey(),
                    original
                        .change_descriptor()
                        .at_derivation_index(index)
                        .unwrap()
                        .script_pubkey(),
                    "{shape} change {index}"
                );
            }
        }
    }
}
