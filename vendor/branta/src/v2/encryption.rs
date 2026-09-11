//! AES-256-GCM encrypt/decrypt.
//!
//! This module's wire format MUST match byte-for-byte across every Branta SDK — it is the
//! foundation of cross-SDK zero-knowledge payment interop. Transcribed from
//! `Branta/Classes/AesEncryption.cs`:
//!
//! - Key = `SHA-256(UTF-8 secret bytes)` — 32 bytes, used directly as the AES-256 key.
//! - Nonce/IV = 12 bytes. Random (CSPRNG) when `deterministic_nonce` is false. When true: the
//!   first 12 bytes of `HMAC-SHA256(key = key_data [the SHA-256'd key, NOT the raw secret],
//!   message = UTF-8 plaintext value)`.
//! - AES-256-GCM with a 16-byte tag. Wire format: `base64_standard(iv || ciphertext || tag)`.
//! - Decrypt: base64-decode; a result shorter than 28 bytes (12 iv + 16 tag minimum) is a
//!   distinct `EncryptedDataTooShort` error, checked before any crypto is attempted. Any other
//!   failure (e.g. wrong key -> GCM tag mismatch) collapses into a generic `DecryptionFailed`.

use aes_gcm::aead::{Aead, AeadCore, Generate, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::error::BrantaError;

type HmacSha256 = Hmac<Sha256>;

/// Encrypts `value` with a key derived from `secret`.
///
/// When `deterministic_nonce` is true, the same `(value, secret)` pair always produces the same
/// ciphertext — used for hash-ZK destination lookups, where the lookup value must be
/// reconstructible without a stored secret.
pub fn encrypt(
    value: &str,
    secret: &str,
    deterministic_nonce: bool,
) -> Result<String, BrantaError> {
    let key_data = Sha256::digest(secret.as_bytes());

    let mut iv = [0u8; 12];
    if deterministic_nonce {
        let mut mac = HmacSha256::new_from_slice(&key_data)
            .map_err(|e| BrantaError::EncryptionFailed(e.to_string()))?;
        mac.update(value.as_bytes());
        let derived = mac.finalize().into_bytes();
        iv.copy_from_slice(&derived[..12]);
    } else {
        let nonce = Nonce::<<Aes256Gcm as AeadCore>::NonceSize>::generate();
        iv.copy_from_slice(nonce.as_slice());
    }

    let cipher = Aes256Gcm::new_from_slice(&key_data)
        .map_err(|e| BrantaError::EncryptionFailed(e.to_string()))?;
    let nonce = Nonce::try_from(iv.as_slice()).expect("iv is always 12 bytes");
    let ciphertext_and_tag = cipher
        .encrypt(&nonce, value.as_bytes())
        .map_err(|e| BrantaError::EncryptionFailed(e.to_string()))?;

    let mut result = Vec::with_capacity(iv.len() + ciphertext_and_tag.len());
    result.extend_from_slice(&iv);
    result.extend_from_slice(&ciphertext_and_tag);

    Ok(BASE64.encode(result))
}

/// Decrypts a value produced by [`encrypt`] using the matching `secret`.
pub fn decrypt(encrypted_value: &str, secret: &str) -> Result<String, BrantaError> {
    let data = BASE64
        .decode(encrypted_value)
        .map_err(|e| BrantaError::DecryptionFailed(e.to_string()))?;

    if data.len() < 28 {
        return Err(BrantaError::EncryptedDataTooShort);
    }

    let key_data = Sha256::digest(secret.as_bytes());
    let (iv, ciphertext_and_tag) = data.split_at(12);

    let cipher = Aes256Gcm::new_from_slice(&key_data)
        .map_err(|e| BrantaError::DecryptionFailed(e.to_string()))?;
    let nonce = Nonce::try_from(iv).map_err(|e| BrantaError::DecryptionFailed(e.to_string()))?;
    let plaintext = cipher
        .decrypt(&nonce, ciphertext_and_tag)
        .map_err(|e| BrantaError::DecryptionFailed(e.to_string()))?;

    String::from_utf8(plaintext).map_err(|e| BrantaError::DecryptionFailed(e.to_string()))
}

/// Trait wrapper around [`encrypt`]/[`decrypt`] so `BrantaService` can be constructed with a
/// mock in tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait AesEncryptionTrait: Send + Sync {
    async fn encrypt(
        &self,
        value: &str,
        secret: &str,
        deterministic_nonce: bool,
    ) -> Result<String, BrantaError>;
    async fn decrypt(&self, encrypted_value: &str, secret: &str) -> Result<String, BrantaError>;
}

/// Production `AesEncryptionTrait` implementation, delegating to the free functions above.
#[derive(Debug, Clone, Copy, Default)]
pub struct AesEncryptionService;

#[async_trait]
impl AesEncryptionTrait for AesEncryptionService {
    async fn encrypt(
        &self,
        value: &str,
        secret: &str,
        deterministic_nonce: bool,
    ) -> Result<String, BrantaError> {
        encrypt(value, secret, deterministic_nonce)
    }

    async fn decrypt(&self, encrypted_value: &str, secret: &str) -> Result<String, BrantaError> {
        decrypt(encrypted_value, secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_matching_secret() {
        let ciphertext = encrypt("hello world", "my-secret", false).unwrap();
        let plaintext = decrypt(&ciphertext, "my-secret").unwrap();
        assert_eq!(plaintext, "hello world");
    }

    #[test]
    fn round_trip_deterministic() {
        let ciphertext = encrypt("hello world", "my-secret", true).unwrap();
        let plaintext = decrypt(&ciphertext, "my-secret").unwrap();
        assert_eq!(plaintext, "hello world");
    }

    #[test]
    fn wrong_secret_fails_to_decrypt() {
        let ciphertext = encrypt("hello world", "my-secret", false).unwrap();
        let result = decrypt(&ciphertext, "wrong-secret");
        assert!(matches!(result, Err(BrantaError::DecryptionFailed(_))));
    }

    #[test]
    fn random_nonce_produces_different_ciphertext_each_call() {
        let a = encrypt("hello world", "my-secret", false).unwrap();
        let b = encrypt("hello world", "my-secret", false).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn deterministic_nonce_produces_identical_ciphertext_each_call() {
        let a = encrypt("hello world", "my-secret", true).unwrap();
        let b = encrypt("hello world", "my-secret", true).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn deterministic_round_trip_with_hash_derived_key() {
        use crate::extensions::to_normalized_hash;
        let value = "lnbc1qsomething";
        let key = to_normalized_hash(value);
        let ciphertext = encrypt(value, &key, true).unwrap();
        assert_eq!(decrypt(&ciphertext, &key).unwrap(), value);
    }

    #[test]
    fn unicode_value_round_trips() {
        let value = "hello 世界 🚀 emoji test";
        let ciphertext = encrypt(value, "my-secret", false).unwrap();
        assert_eq!(decrypt(&ciphertext, "my-secret").unwrap(), value);
    }

    #[test]
    fn too_short_base64_is_a_distinct_error() {
        // "AAAA" decodes to 3 zero bytes -- far short of the 28-byte (12 iv + 16 tag) minimum.
        let result = decrypt("AAAA", "my-secret");
        assert!(matches!(result, Err(BrantaError::EncryptedDataTooShort)));
    }

    #[test]
    fn malformed_base64_is_a_decryption_failure_not_too_short() {
        let result = decrypt("not-valid-base64!!!", "my-secret");
        assert!(matches!(result, Err(BrantaError::DecryptionFailed(_))));
    }

    /// Proves real cross-SDK interop, not just internal self-consistency: this exact ciphertext
    /// was produced by running `branta-python`'s `AesEncryption.encrypt("hello world",
    /// "my-secret", deterministic_nonce=True)` directly.
    #[test]
    fn cross_sdk_fixed_vector_matches_python() {
        let expected_ciphertext = "mPIKHc3ywVlsBHf3Lv2Rwpz2+fKE0kgUePq2m4fPIUidMuGEHVIB";

        let ciphertext = encrypt("hello world", "my-secret", true).unwrap();
        assert_eq!(ciphertext, expected_ciphertext);

        let plaintext = decrypt(expected_ciphertext, "my-secret").unwrap();
        assert_eq!(plaintext, "hello world");
    }
}
