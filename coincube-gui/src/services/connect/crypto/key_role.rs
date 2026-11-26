//! A key's role in a Vault, sealed to that key's phone (Mobile V3 Phase 1,
//! Desktop PR T2; plan invariant X9).
//!
//! COINCUBE Mobile shows which path of a Vault a key sits in: primary,
//! recovery or safety net. Connect must not learn that, so this computer
//! seals the role to the phone's registered transport key and Connect stores
//! and relays ciphertext of one fixed length.
//!
//! ## What sealing gives, and what it does not
//!
//! Confidentiality only, and only against a Connect that stores and relays
//! honestly: its database, its logs and anyone reading them see ciphertext.
//! The phone's key reaches this computer through Connect (`ResolveSigners`)
//! and is not pinned, so a Connect that actively substitutes its own key can
//! read what is sealed to it. That is the trust the signing rail already
//! places in the same relayed key, not a new one.
//!
//! No authenticity either. Anyone who holds a phone's public transport key
//! can seal to it, so a blob that opens proves nothing about who wrote it.
//! The role is therefore a **label**: it must gate nothing, on either app.
//!
//! ## Construction
//!
//! The primitives of every other envelope in the system (`SPEC-ecies-v1` §1:
//! secp256k1, compressed SEC1, raw-point ECDH, HKDF-SHA256 with a 32-zero-byte
//! salt, AES-256-GCM), under their own domain label. The signing rail's code
//! ([`super::transport`]) is not touched and shares no label with this, so a
//! role blob can never be opened as a signing payload, nor the reverse,
//! whatever request id either is tried with:
//!
//! ```text
//! label     = "coincube-connect-key-role-v1\x00"                      (29 bytes)
//! ikm       = compressed( e · P_device )
//! K         = HKDF-SHA256(ikm, salt = 0x00*32, info = label ‖ E ‖ P_device, L = 32)
//! AAD       = label ‖ u64be(vault_id) ‖ u64be(key_id) ‖ u64be(device_id)  (53 bytes)
//! plaintext = 0x01 ‖ role ‖ 0x00*14                                   (16 bytes)
//!             role: 0x01 primary, 0x02 recovery, 0x03 safety net
//! blob      = E (33) ‖ nonce (12) ‖ AES-256-GCM ciphertext ‖ tag (32)  (77 bytes)
//! ```
//!
//! The three ids are Connect's numeric ids: the Vault's, the key's
//! (`SignerTarget.key_id`) and the phone's signer device id
//! (`SignerTarget.device_id`). Binding them means a blob lifted to another
//! Vault, key or phone does not open. Every role is the same 16 bytes of
//! plaintext, so every blob is 77 bytes and its size says nothing.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use coincube_core::miniscript::bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
use rand::RngCore;
use sha2::Digest;

use crate::services::inheritance::ecies::{
    ecdh_ikm, hkdf_key, random_secret_key, NONCE_LEN, PUBKEY_LEN, TAG_LEN,
};
use crate::services::inheritance::EciesError;

/// Domain label for a sealed key role. Distinct from the signing rail's
/// (`coincube-connect-transport-v1\x00`), the inheritance and the cube-xpub
/// labels. It is in both the HKDF `info` and the AAD.
pub const KEY_ROLE_LABEL: &[u8] = b"coincube-connect-key-role-v1\x00";

/// Version byte that starts the plaintext.
pub const KEY_ROLE_PLAINTEXT_VERSION: u8 = 0x01;

/// Length of the plaintext, the same for every role.
pub const KEY_ROLE_PLAINTEXT_LEN: usize = 16;

/// Length of every sealed blob: `E ‖ nonce ‖ ciphertext ‖ tag`.
pub const KEY_ROLE_BLOB_LEN: usize = PUBKEY_LEN + NONCE_LEN + KEY_ROLE_PLAINTEXT_LEN + TAG_LEN;

/// Which spending path of a Vault a key sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyRole {
    Primary,
    Recovery,
    SafetyNet,
}

impl KeyRole {
    /// The role byte in the plaintext.
    pub fn code(self) -> u8 {
        match self {
            Self::Primary => 0x01,
            Self::Recovery => 0x02,
            Self::SafetyNet => 0x03,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            0x01 => Some(Self::Primary),
            0x02 => Some(Self::Recovery),
            0x03 => Some(Self::SafetyNet),
            _ => None,
        }
    }

    fn plaintext(self) -> [u8; KEY_ROLE_PLAINTEXT_LEN] {
        let mut pt = [0u8; KEY_ROLE_PLAINTEXT_LEN];
        pt[0] = KEY_ROLE_PLAINTEXT_VERSION;
        pt[1] = self.code();
        pt
    }

    /// Strict: the version, a known role, and zero padding. Anything else is
    /// not a role.
    fn from_plaintext(pt: &[u8]) -> Option<Self> {
        if pt.len() != KEY_ROLE_PLAINTEXT_LEN
            || pt[0] != KEY_ROLE_PLAINTEXT_VERSION
            || pt[2..].iter().any(|b| *b != 0)
        {
            return None;
        }
        Self::from_code(pt[1])
    }
}

/// What a sealed role is bound to. A blob opens only for the same three ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyRoleBinding {
    /// Connect's id of the Vault.
    pub vault_id: u64,
    /// Connect's id of the key (`SignerTarget.key_id`).
    pub key_id: u64,
    /// Connect's id of the phone (`SignerTarget.device_id`).
    pub device_id: u64,
}

impl KeyRoleBinding {
    /// `label ‖ u64be(vault_id) ‖ u64be(key_id) ‖ u64be(device_id)`. Fixed
    /// widths, so no two bindings share an encoding.
    fn aad(&self) -> Vec<u8> {
        let mut aad = Vec::with_capacity(KEY_ROLE_LABEL.len() + 24);
        aad.extend_from_slice(KEY_ROLE_LABEL);
        aad.extend_from_slice(&self.vault_id.to_be_bytes());
        aad.extend_from_slice(&self.key_id.to_be_bytes());
        aad.extend_from_slice(&self.device_id.to_be_bytes());
        aad
    }
}

/// Seals `role` to a phone's registered transport public key
/// (`SignerTarget.transport_pubkey`, 33 bytes compressed). Returns the
/// [`KEY_ROLE_BLOB_LEN`]-byte blob. A fresh ephemeral key and nonce per call.
pub fn seal_key_role(
    recipient_pub: &[u8],
    binding: &KeyRoleBinding,
    role: KeyRole,
) -> Result<Vec<u8>, EciesError> {
    if recipient_pub.len() != PUBKEY_LEN {
        return Err(EciesError::MalformedEnvelope("transport pubkey length"));
    }
    let p = PublicKey::from_slice(recipient_pub)
        .map_err(|_| EciesError::MalformedEnvelope("transport pubkey"))?;
    let eph_sk = random_secret_key();
    let mut nonce = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce);
    seal_with_ephemeral(&p, &eph_sk, &nonce, binding, role)
}

/// The deterministic core: [`seal_key_role`] supplies random inputs, the
/// shared test vector fixed ones.
fn seal_with_ephemeral(
    recipient_pub: &PublicKey,
    eph_sk: &SecretKey,
    nonce: &[u8; NONCE_LEN],
    binding: &KeyRoleBinding,
    role: KeyRole,
) -> Result<Vec<u8>, EciesError> {
    let secp = Secp256k1::new();
    let eph_pk = PublicKey::from_secret_key(&secp, eph_sk).serialize();
    let ikm = ecdh_ikm(recipient_pub, eph_sk);
    let key = hkdf_key(
        KEY_ROLE_LABEL,
        ikm.as_ref(),
        &eph_pk,
        &recipient_pub.serialize(),
    );
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(EciesError::Cipher)?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: &role.plaintext(),
                aad: &binding.aad(),
            },
        )
        .map_err(|_| EciesError::Seal)?;
    let mut blob = Vec::with_capacity(KEY_ROLE_BLOB_LEN);
    blob.extend_from_slice(&eph_pk);
    blob.extend_from_slice(nonce);
    blob.extend_from_slice(&ciphertext);
    debug_assert_eq!(blob.len(), KEY_ROLE_BLOB_LEN);
    Ok(blob)
}

/// The **phone's half**: opens a blob with the phone's transport secret. This
/// computer never holds that secret; the function exists so the format is
/// defined in one place and tests can stand in for the phone.
///
/// Wrong key, wrong binding, a tampered blob and a plaintext that is not a
/// role are one indistinguishable error.
pub fn open_key_role(
    device_secret: &SecretKey,
    binding: &KeyRoleBinding,
    blob: &[u8],
) -> Result<KeyRole, EciesError> {
    if blob.len() != KEY_ROLE_BLOB_LEN {
        return Err(EciesError::MalformedEnvelope("key role blob length"));
    }
    let (eph_pub, rest) = blob.split_at(PUBKEY_LEN);
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let e = PublicKey::from_slice(eph_pub)
        .map_err(|_| EciesError::MalformedEnvelope("ephemeral pubkey"))?;
    let secp = Secp256k1::new();
    let device_pub = PublicKey::from_secret_key(&secp, device_secret).serialize();
    let ikm = ecdh_ikm(&e, device_secret);
    let key = hkdf_key(KEY_ROLE_LABEL, ikm.as_ref(), eph_pub, &device_pub);
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(EciesError::Cipher)?;
    let pt = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &binding.aad(),
            },
        )
        .map_err(|_| EciesError::BadKeyOrCorrupt)?;
    KeyRole::from_plaintext(&pt).ok_or(EciesError::BadKeyOrCorrupt)
}

/// `recipientKeyHash`: lowercase hex of `SHA-256(label ‖ P_device)`, where
/// `P_device` is the 33-byte transport key the role was sealed to.
///
/// Connect stores it beside the blob and never computes or checks it. This
/// computer reads it back to tell whether a phone already has a seal made to
/// its current key: a phone that re-registered has another key, so another
/// hash, and is sealed to again. The label is in the hash so a future format
/// re-seals too.
pub fn recipient_key_hash(recipient_pub: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(KEY_ROLE_LABEL);
    hasher.update(recipient_pub);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::connect::crypto::transport::{seal_to_device, DeviceTransportKey};
    use std::convert::TryInto;

    /// The vector COINCUBE Mobile's M10 tests against. Copy this file.
    const VECTOR: &str = include_str!("testdata/key_role_v1.json");

    const BINDING: KeyRoleBinding = KeyRoleBinding {
        vault_id: 5,
        key_id: 12,
        device_id: 34,
    };

    fn device() -> (SecretKey, [u8; PUBKEY_LEN]) {
        let sk = random_secret_key();
        let pk = PublicKey::from_secret_key(&Secp256k1::new(), &sk).serialize();
        (sk, pk)
    }

    fn unhex(v: &serde_json::Value, key: &str) -> Vec<u8> {
        hex::decode(
            v[key]
                .as_str()
                .unwrap_or_else(|| panic!("vector has no {}", key)),
        )
        .unwrap()
    }

    #[test]
    fn seal_then_open_roundtrips_for_every_role() {
        let (sk, pk) = device();
        for role in [KeyRole::Primary, KeyRole::Recovery, KeyRole::SafetyNet] {
            let blob = seal_key_role(&pk, &BINDING, role).unwrap();
            assert_eq!(open_key_role(&sk, &BINDING, &blob).unwrap(), role);
        }
    }

    /// The blob's size must say nothing about the role.
    #[test]
    fn every_role_seals_to_the_same_length() {
        let (_, pk) = device();
        assert_eq!(KEY_ROLE_BLOB_LEN, 77);
        for role in [KeyRole::Primary, KeyRole::Recovery, KeyRole::SafetyNet] {
            assert_eq!(role.plaintext().len(), KEY_ROLE_PLAINTEXT_LEN);
            for _ in 0..8 {
                assert_eq!(
                    seal_key_role(&pk, &BINDING, role).unwrap().len(),
                    KEY_ROLE_BLOB_LEN
                );
            }
        }
        // And two seals of one role share no bytes by construction: a fresh
        // ephemeral key and nonce each time.
        let a = seal_key_role(&pk, &BINDING, KeyRole::Primary).unwrap();
        let b = seal_key_role(&pk, &BINDING, KeyRole::Primary).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_blob_moved_to_another_vault_key_or_phone_does_not_open() {
        let (sk, pk) = device();
        let blob = seal_key_role(&pk, &BINDING, KeyRole::Recovery).unwrap();
        for wrong in [
            KeyRoleBinding {
                vault_id: 6,
                ..BINDING
            },
            KeyRoleBinding {
                key_id: 13,
                ..BINDING
            },
            KeyRoleBinding {
                device_id: 35,
                ..BINDING
            },
            // The same numbers in other positions.
            KeyRoleBinding {
                vault_id: 12,
                key_id: 5,
                device_id: 34,
            },
            KeyRoleBinding {
                vault_id: 5,
                key_id: 34,
                device_id: 12,
            },
        ] {
            assert!(matches!(
                open_key_role(&sk, &wrong, &blob),
                Err(EciesError::BadKeyOrCorrupt)
            ));
        }
        // Another phone cannot open it.
        let (other, _) = device();
        assert!(open_key_role(&other, &BINDING, &blob).is_err());
    }

    #[test]
    fn a_tampered_or_misshapen_blob_fails_closed() {
        let (sk, pk) = device();
        let blob = seal_key_role(&pk, &BINDING, KeyRole::Primary).unwrap();
        for i in [0, PUBKEY_LEN, PUBKEY_LEN + NONCE_LEN, KEY_ROLE_BLOB_LEN - 1] {
            let mut bad = blob.clone();
            bad[i] ^= 0x01;
            assert!(open_key_role(&sk, &BINDING, &bad).is_err(), "byte {}", i);
        }
        assert!(open_key_role(&sk, &BINDING, &blob[..KEY_ROLE_BLOB_LEN - 1]).is_err());
        let mut long = blob;
        long.push(0);
        assert!(open_key_role(&sk, &BINDING, &long).is_err());
    }

    #[test]
    fn only_the_three_roles_with_zero_padding_are_a_role() {
        assert_eq!(
            KeyRole::Primary.plaintext(),
            [1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(KeyRole::Recovery.plaintext()[..2], [1, 2]);
        assert_eq!(KeyRole::SafetyNet.plaintext()[..2], [1, 3]);
        let mut pt = KeyRole::Primary.plaintext();
        assert_eq!(KeyRole::from_plaintext(&pt), Some(KeyRole::Primary));
        pt[1] = 0;
        assert_eq!(KeyRole::from_plaintext(&pt), None);
        pt[1] = 4;
        assert_eq!(KeyRole::from_plaintext(&pt), None);
        let mut pt = KeyRole::Primary.plaintext();
        pt[0] = 2;
        assert_eq!(KeyRole::from_plaintext(&pt), None);
        let mut pt = KeyRole::Primary.plaintext();
        pt[15] = 1;
        assert_eq!(KeyRole::from_plaintext(&pt), None);
        assert_eq!(KeyRole::from_plaintext(&pt[..15]), None);
    }

    /// A role blob is not a signing payload and a signing payload is not a
    /// role blob, whatever request id is tried: the two share no label.
    #[test]
    fn a_role_blob_and_a_signing_payload_never_open_as_each_other() {
        // A real rail key, minted the way the app mints one; its secret is
        // read back from the sidecar so the test can also play the phone.
        let dir = std::env::temp_dir().join(format!("coincube-key-role-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let device =
            DeviceTransportKey::load_or_create(&crate::dir::NetworkDirectory::new(dir.clone()))
                .unwrap();
        let secret_hex = std::fs::read_to_string(dir.join("connect_transport_key")).unwrap();
        let sk = SecretKey::from_slice(&hex::decode(secret_hex.trim()).unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let pk = device.public_key();
        assert_eq!(
            PublicKey::from_secret_key(&Secp256k1::new(), &sk).serialize(),
            pk
        );

        let blob = seal_key_role(&pk, &BINDING, KeyRole::Primary).unwrap();
        let (eph, rest) = blob.split_at(PUBKEY_LEN);
        let (nonce, ct) = rest.split_at(NONCE_LEN);
        // Request ids an attacker might try, including the raw binding bytes
        // where they happen to be valid text.
        let binding_tail =
            String::from_utf8_lossy(&BINDING.aad()[KEY_ROLE_LABEL.len()..]).into_owned();
        for request_id in ["", "5", "5:12:34", "key-role", binding_tail.as_str()] {
            assert!(device.open(eph, nonce, ct, request_id).is_err());
        }

        // A 16-byte signing payload has the same ciphertext length as a role.
        let sealed = seal_to_device(&pk, "req-1", &KeyRole::Primary.plaintext()).unwrap();
        let mut as_blob = sealed.ephemeral_pubkey.clone();
        as_blob.extend_from_slice(&sealed.nonce);
        as_blob.extend_from_slice(&sealed.ciphertext);
        assert_eq!(as_blob.len(), KEY_ROLE_BLOB_LEN);
        assert!(open_key_role(&sk, &BINDING, &as_blob).is_err());
    }

    #[test]
    fn malformed_recipient_keys_are_refused() {
        assert!(seal_key_role(&[2u8; 32], &BINDING, KeyRole::Primary).is_err());
        assert!(seal_key_role(&[0u8; 33], &BINDING, KeyRole::Primary).is_err());
        assert!(seal_key_role(&[], &BINDING, KeyRole::Primary).is_err());
    }

    #[test]
    fn the_recipient_hash_is_per_key_and_sha256_shaped() {
        let (_, a) = device();
        let (_, b) = device();
        let hash = recipient_key_hash(&a);
        assert_eq!(hash.len(), 64);
        assert!(hash
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
        assert_eq!(hash, recipient_key_hash(&a));
        assert_ne!(hash, recipient_key_hash(&b));
    }

    /// The frozen vector: fixed keys, nonce and ids produce exactly this
    /// blob, and the blob opens to the role with the phone's secret.
    #[test]
    fn the_shared_vector_seals_and_opens() {
        let v: serde_json::Value = serde_json::from_str(VECTOR).unwrap();
        assert_eq!(
            v["label_hex"].as_str().unwrap(),
            hex::encode(KEY_ROLE_LABEL)
        );
        assert_eq!(v["blob_len"], KEY_ROLE_BLOB_LEN);
        assert_eq!(v["plaintext_len"], KEY_ROLE_PLAINTEXT_LEN);
        let device_sk = SecretKey::from_slice(&unhex(&v, "device_secret_hex")).unwrap();
        let device_pk = PublicKey::from_secret_key(&Secp256k1::new(), &device_sk);
        assert_eq!(
            device_pk.serialize().to_vec(),
            unhex(&v, "device_pubkey_hex")
        );
        assert_eq!(
            recipient_key_hash(&device_pk.serialize()),
            v["recipient_key_hash"].as_str().unwrap()
        );
        let eph_sk = SecretKey::from_slice(&unhex(&v, "ephemeral_secret_hex")).unwrap();
        let nonce: [u8; NONCE_LEN] = unhex(&v, "nonce_hex").try_into().unwrap();
        let binding = KeyRoleBinding {
            vault_id: v["vault_id"].as_u64().unwrap(),
            key_id: v["key_id"].as_u64().unwrap(),
            device_id: v["device_id"].as_u64().unwrap(),
        };
        assert_eq!(hex::encode(binding.aad()), v["aad_hex"].as_str().unwrap());

        let cases = v["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 3);
        for case in cases {
            let role = match case["role"].as_str().unwrap() {
                "primary" => KeyRole::Primary,
                "recovery" => KeyRole::Recovery,
                "safety_net" => KeyRole::SafetyNet,
                other => panic!("unknown role {}", other),
            };
            assert_eq!(case["role_code"], role.code());
            assert_eq!(
                hex::encode(role.plaintext()),
                case["plaintext_hex"].as_str().unwrap()
            );
            let blob = seal_with_ephemeral(&device_pk, &eph_sk, &nonce, &binding, role).unwrap();
            assert_eq!(hex::encode(&blob), case["blob_hex"].as_str().unwrap());
            use base64::Engine as _;
            assert_eq!(
                base64::engine::general_purpose::STANDARD.encode(&blob),
                case["blob_base64"].as_str().unwrap()
            );
            assert_eq!(open_key_role(&device_sk, &binding, &blob).unwrap(), role);
        }
    }

    /// Prints the vector file. Run by hand when the format changes:
    /// `cargo test -p coincube-gui --lib -- key_role::tests::print_vector --ignored --nocapture`
    #[test]
    #[ignore]
    fn print_vector() {
        use base64::Engine as _;
        let device_sk = SecretKey::from_slice(&[0x11; 32]).unwrap();
        let device_pk = PublicKey::from_secret_key(&Secp256k1::new(), &device_sk);
        let eph_sk = SecretKey::from_slice(&[0x22; 32]).unwrap();
        let nonce = [0x33u8; NONCE_LEN];
        let cases: Vec<serde_json::Value> = [
            ("primary", KeyRole::Primary),
            ("recovery", KeyRole::Recovery),
            ("safety_net", KeyRole::SafetyNet),
        ]
        .iter()
        .map(|(name, role)| {
            let blob = seal_with_ephemeral(&device_pk, &eph_sk, &nonce, &BINDING, *role).unwrap();
            serde_json::json!({
                "role": name,
                "role_code": role.code(),
                "plaintext_hex": hex::encode(role.plaintext()),
                "blob_hex": hex::encode(&blob),
                "blob_base64": base64::engine::general_purpose::STANDARD.encode(&blob),
            })
        })
        .collect();
        let v = serde_json::json!({
            "description": "Sealed key role, format 1. Shared by COINCUBE Desktop (seals) and COINCUBE Mobile (opens). Test keys only. The fixed ephemeral key and nonce exist to make the vector reproducible; real seals use fresh random ones, and reusing one nonce under one key as this file does across its three cases must never happen outside a vector.",
            "label_hex": hex::encode(KEY_ROLE_LABEL),
            "blob_len": KEY_ROLE_BLOB_LEN,
            "plaintext_len": KEY_ROLE_PLAINTEXT_LEN,
            "device_secret_hex": hex::encode([0x11u8; 32]),
            "device_pubkey_hex": hex::encode(device_pk.serialize()),
            "recipient_key_hash": recipient_key_hash(&device_pk.serialize()),
            "ephemeral_secret_hex": hex::encode([0x22u8; 32]),
            "nonce_hex": hex::encode(nonce),
            "vault_id": BINDING.vault_id,
            "key_id": BINDING.key_id,
            "device_id": BINDING.device_id,
            "aad_hex": hex::encode(BINDING.aad()),
            "cases": cases,
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
    }
}
