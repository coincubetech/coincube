//! AES-256-GCM + Argon2id envelope for the Cube Recovery Kit.
//!
//! Wire format (binary, then base64-encoded for transport).
//!
//! Version 1, still decrypted:
//!
//! ```text
//! version (1B=0x01) || kdf_id (1B=0x01) ||
//! kdf_params (4B: memory_kib u16 BE || t_cost u8 || p_cost u8) ||
//! salt (16B) || nonce (12B) || ciphertext || tag (16B)
//! ```
//!
//! AAD for version 1 is the 6-byte header.
//!
//! Version 2 is what `encrypt` writes. Memory cost is a u32 so it can match
//! the seed file, and the cube id is appended to the header in the AAD:
//!
//! ```text
//! version (1B=0x02) || kdf_id (1B=0x01) ||
//! memory_kib (4B u32 BE) || t_cost u8 || p_cost u8 ||
//! salt (16B) || nonce (12B) || ciphertext || tag (16B)
//! ```
//!
//! Aligned with `PLAN-cube-recovery-kit-desktop.md` §2.1.

use std::convert::TryFrom;

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use rand::RngCore;
use zeroize::Zeroizing;

use super::error::RecoveryError;

/// Envelope version 1. Still decrypted. New kits are version 2.
pub const ENVELOPE_VERSION: u8 = 0x01;

/// Envelope version this client writes. Memory cost is a u32 so it can match
/// the seed-file Argon2id parameters, and the cube id is in the GCM data.
pub const ENVELOPE_VERSION_V2: u8 = 0x02;

/// KDF identifier for Argon2id-with-per-envelope-params. Adding a new KDF
/// is a wire-format break and needs a new value here (and matching decrypt
/// dispatch).
pub const KDF_ID_ARGON2ID_V1: u8 = 0x01;

const V1_HEADER_LEN: usize = 6; // version + kdf_id + memory u16 + t + p
const V2_HEADER_LEN: usize = 8; // version + kdf_id + memory u32 + t + p
const HEADER_LEN: usize = V1_HEADER_LEN;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const KEY_LEN: usize = 32;
const MIN_ENVELOPE_LEN: usize = HEADER_LEN + SALT_LEN + NONCE_LEN + TAG_LEN;

/// Argon2id KDF cost parameters carried inside every envelope so that
/// decrypt uses the exact same parameters that encrypt used — the caller
/// never re-derives the wire params on decrypt.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub t_cost: u8,
    pub p_cost: u8,
}

impl KdfParams {
    /// v1 defaults: matches the Argon2id params used for PIN hashing
    /// (`coincube-gui/src/app/settings/mod.rs` — m=19456 KiB, t=2, p=1).
    /// Carrying the params in the envelope means we can bump these later
    /// without breaking existing backups.
    pub const DEFAULT_V1: Self = Self {
        memory_kib: 19456,
        t_cost: 2,
        p_cost: 1,
    };

    /// Same Argon2id cost as the on-disk seed file (256 MiB, t=3, p=4).
    /// New Recovery Kits use this. Decrypt still honours the params stored
    /// in an older envelope.
    pub const SEED: Self = Self {
        memory_kib: 262_144,
        t_cost: 3,
        p_cost: 4,
    };
}

fn v1_header(
    version: u8,
    kdf_id: u8,
    params: KdfParams,
) -> Result<[u8; V1_HEADER_LEN], RecoveryError> {
    let memory = u16::try_from(params.memory_kib)
        .map_err(|_| RecoveryError::InvalidParams(argon2::Error::MemoryTooLittle))?;
    let m = memory.to_be_bytes();
    Ok([version, kdf_id, m[0], m[1], params.t_cost, params.p_cost])
}

fn v2_header(version: u8, kdf_id: u8, params: KdfParams) -> [u8; V2_HEADER_LEN] {
    let m = params.memory_kib.to_be_bytes();
    [
        version,
        kdf_id,
        m[0],
        m[1],
        m[2],
        m[3],
        params.t_cost,
        params.p_cost,
    ]
}

fn parse_header(buf: &[u8]) -> Result<(u8, u8, KdfParams, usize), RecoveryError> {
    if buf.is_empty() {
        return Err(RecoveryError::Truncated);
    }
    let version = buf[0];
    let header_len = match version {
        ENVELOPE_VERSION => V1_HEADER_LEN,
        ENVELOPE_VERSION_V2 => V2_HEADER_LEN,
        _ => {
            return Err(RecoveryError::Unsupported {
                version,
                kdf_id: buf.get(1).copied().unwrap_or(0),
            })
        }
    };
    if buf.len() < header_len {
        return Err(RecoveryError::Truncated);
    }
    let kdf_id = buf[1];
    let (memory_kib, t_cost, p_cost) = if version == ENVELOPE_VERSION {
        (
            u32::from(u16::from_be_bytes([buf[2], buf[3]])),
            buf[4],
            buf[5],
        )
    } else {
        (
            u32::from_be_bytes([buf[2], buf[3], buf[4], buf[5]]),
            buf[6],
            buf[7],
        )
    };
    Ok((
        version,
        kdf_id,
        KdfParams {
            memory_kib,
            t_cost,
            p_cost,
        },
        header_len,
    ))
}

fn derive_key(
    password: &[u8],
    salt: &[u8],
    params: KdfParams,
) -> Result<Zeroizing<[u8; KEY_LEN]>, RecoveryError> {
    // A hostile envelope must not be able to ask for more RAM than a real
    // kit uses. Version 2 stores the cost as a u32.
    if params.memory_kib == 0 || params.memory_kib > KdfParams::SEED.memory_kib {
        return Err(RecoveryError::InvalidParams(argon2::Error::MemoryTooMuch));
    }
    let argon_params = Params::new(
        params.memory_kib,
        u32::from(params.t_cost),
        u32::from(params.p_cost),
        Some(KEY_LEN),
    )
    .map_err(RecoveryError::InvalidParams)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon2
        .hash_password_into(password, salt, key.as_mut())
        .map_err(RecoveryError::Kdf)?;
    Ok(key)
}

/// Encrypts `plaintext` under `password`, bound to `cube_id`, and returns a
/// base64-encoded version-2 envelope. Generates a fresh salt and nonce.
pub fn encrypt(
    plaintext: &[u8],
    password: &Zeroizing<String>,
    params: KdfParams,
    cube_id: &str,
) -> Result<String, RecoveryError> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    let mut rng = rand::thread_rng();
    rng.fill_bytes(&mut salt);
    rng.fill_bytes(&mut nonce);

    encrypt_with(
        plaintext,
        password.as_bytes(),
        &salt,
        &nonce,
        params,
        ENVELOPE_VERSION_V2,
        KDF_ID_ARGON2ID_V1,
        Some(cube_id),
    )
}

/// Internal encrypt that takes explicit salt/nonce/version/kdf_id so tests
/// can pin known-answer vectors. Version 2 requires `cube_id`. Callers
/// outside the crate should use `encrypt`.
#[allow(clippy::too_many_arguments)]
fn encrypt_with(
    plaintext: &[u8],
    password: &[u8],
    salt: &[u8; SALT_LEN],
    nonce: &[u8; NONCE_LEN],
    params: KdfParams,
    version: u8,
    kdf_id: u8,
    cube_id: Option<&str>,
) -> Result<String, RecoveryError> {
    let key = derive_key(password, salt, params)?;
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(RecoveryError::Cipher)?;
    let (header, aad) = match version {
        ENVELOPE_VERSION => {
            let header = v1_header(version, kdf_id, params)?;
            let aad = header.to_vec();
            (header.to_vec(), aad)
        }
        ENVELOPE_VERSION_V2 => {
            let header = v2_header(version, kdf_id, params);
            let mut aad = header.to_vec();
            aad.extend_from_slice(cube_id.unwrap_or("").as_bytes());
            (header.to_vec(), aad)
        }
        _ => {
            return Err(RecoveryError::Unsupported { version, kdf_id });
        }
    };

    let ct = cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| RecoveryError::Seal)?;

    let mut out = Vec::with_capacity(header.len() + SALT_LEN + NONCE_LEN + ct.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(salt);
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ct);
    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}

/// Decrypts a base64-encoded envelope. Returns the plaintext in a
/// `Zeroizing<Vec<u8>>` so it's wiped on drop. A tag mismatch is reported
/// as `BadPasswordOrCorrupt` — the caller cannot distinguish a wrong
/// password from a tampered envelope, by design.
///
/// Version 1 ignores `cube_id` (those kits were not bound to one). Version 2
/// fails closed when `cube_id` does not match the value used at seal time.
pub fn decrypt(
    envelope_b64: &str,
    password: &Zeroizing<String>,
    cube_id: &str,
) -> Result<Zeroizing<Vec<u8>>, RecoveryError> {
    let raw = base64::engine::general_purpose::STANDARD.decode(envelope_b64)?;
    if raw.len() < MIN_ENVELOPE_LEN {
        return Err(RecoveryError::Truncated);
    }

    let (version, kdf_id, params, header_len) = parse_header(&raw)?;
    if kdf_id != KDF_ID_ARGON2ID_V1 {
        return Err(RecoveryError::Unsupported { version, kdf_id });
    }
    if raw.len() < header_len + SALT_LEN + NONCE_LEN + TAG_LEN {
        return Err(RecoveryError::Truncated);
    }

    let salt = &raw[header_len..header_len + SALT_LEN];
    let nonce = &raw[header_len + SALT_LEN..header_len + SALT_LEN + NONCE_LEN];
    let ct_and_tag = &raw[header_len + SALT_LEN + NONCE_LEN..];

    let key = derive_key(password.as_bytes(), salt, params)?;
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(RecoveryError::Cipher)?;

    let mut aad = raw[..header_len].to_vec();
    if version == ENVELOPE_VERSION_V2 {
        aad.extend_from_slice(cube_id.as_bytes());
    }
    let pt = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ct_and_tag,
                aad: &aad,
            },
        )
        .map_err(|_| RecoveryError::BadPasswordOrCorrupt)?;

    Ok(Zeroizing::new(pt))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pw(s: &str) -> Zeroizing<String> {
        Zeroizing::new(s.to_string())
    }

    // Deliberately reduced cost for fast tests. Production kits use KdfParams::SEED.
    const TEST_PARAMS: KdfParams = KdfParams {
        memory_kib: 512,
        t_cost: 1,
        p_cost: 1,
    };

    #[test]
    fn roundtrip_basic() {
        let password = pw("correct horse battery staple");
        let plaintext = b"hello cube recovery";
        let env = encrypt(plaintext, &password, TEST_PARAMS, "cube").unwrap();
        let got = decrypt(&env, &password, "cube").unwrap();
        assert_eq!(got.as_slice(), plaintext);
    }

    #[test]
    fn wrong_password_maps_to_bad_or_corrupt() {
        let env = encrypt(b"secret", &pw("right password"), TEST_PARAMS, "cube").unwrap();
        let err = decrypt(&env, &pw("wrong password"), "cube").unwrap_err();
        assert!(matches!(err, RecoveryError::BadPasswordOrCorrupt));
    }

    #[test]
    fn flipped_header_byte_rejected_by_aad() {
        // Flip the low byte of the v2 memory cost. The KDF id stays valid;
        // the cost stays under the cap; the tag fails because the header
        // is authenticated and the derived key changes.
        let env = encrypt(b"hello", &pw("pw"), TEST_PARAMS, "cube").unwrap();
        let mut raw = base64::engine::general_purpose::STANDARD
            .decode(&env)
            .unwrap();
        raw[5] ^= 0x01;
        let mutated = base64::engine::general_purpose::STANDARD.encode(raw);

        // Tampered header either (a) changes the derived key via the
        // params, or (b) breaks the AAD binding — either way, decrypt
        // must fail. Both paths surface as `BadPasswordOrCorrupt`.
        let err = decrypt(&mutated, &pw("pw"), "cube").unwrap_err();
        assert!(
            matches!(err, RecoveryError::BadPasswordOrCorrupt),
            "expected BadPasswordOrCorrupt, got {:?}",
            err
        );
    }

    #[test]
    fn flipped_ciphertext_byte_rejected_by_tag() {
        let env = encrypt(b"hello", &pw("pw"), TEST_PARAMS, "cube").unwrap();
        let mut raw = base64::engine::general_purpose::STANDARD
            .decode(&env)
            .unwrap();
        // Flip a byte in the ciphertext region (past header + salt + nonce).
        let ct_start = V2_HEADER_LEN + SALT_LEN + NONCE_LEN;
        raw[ct_start] ^= 0x01;
        let mutated = base64::engine::general_purpose::STANDARD.encode(raw);

        let err = decrypt(&mutated, &pw("pw"), "cube").unwrap_err();
        assert!(matches!(err, RecoveryError::BadPasswordOrCorrupt));
    }

    #[test]
    fn truncated_envelope_rejected() {
        let env = encrypt(b"hello", &pw("pw"), TEST_PARAMS, "cube").unwrap();
        let mut raw = base64::engine::general_purpose::STANDARD
            .decode(&env)
            .unwrap();
        raw.truncate(MIN_ENVELOPE_LEN - 1);
        let truncated = base64::engine::general_purpose::STANDARD.encode(raw);
        let err = decrypt(&truncated, &pw("pw"), "cube").unwrap_err();
        assert!(matches!(err, RecoveryError::Truncated));
    }

    #[test]
    fn unsupported_version_rejected() {
        let env = encrypt(b"hello", &pw("pw"), TEST_PARAMS, "cube").unwrap();
        let mut raw = base64::engine::general_purpose::STANDARD
            .decode(&env)
            .unwrap();
        raw[0] = 0x99;
        let mutated = base64::engine::general_purpose::STANDARD.encode(raw);
        let err = decrypt(&mutated, &pw("pw"), "cube").unwrap_err();
        assert!(matches!(
            err,
            RecoveryError::Unsupported { version: 0x99, .. }
        ));
    }

    #[test]
    fn unsupported_kdf_id_rejected() {
        let env = encrypt(b"hello", &pw("pw"), TEST_PARAMS, "cube").unwrap();
        let mut raw = base64::engine::general_purpose::STANDARD
            .decode(&env)
            .unwrap();
        raw[1] = 0xFE;
        let mutated = base64::engine::general_purpose::STANDARD.encode(raw);
        let err = decrypt(&mutated, &pw("pw"), "cube").unwrap_err();
        assert!(matches!(
            err,
            RecoveryError::Unsupported { kdf_id: 0xFE, .. }
        ));
    }

    #[test]
    fn large_plaintext_roundtrips() {
        // 1.5 MiB — exercises the Vec growth path and any AEAD buffer
        // boundaries that would only show up at scale.
        let password = pw("pw");
        let plaintext = vec![0xA5u8; 1_500_000];
        let env = encrypt(&plaintext, &password, TEST_PARAMS, "cube").unwrap();
        let got = decrypt(&env, &password, "cube").unwrap();
        assert_eq!(got.as_slice(), plaintext.as_slice());
    }

    #[test]
    fn known_answer_pins_wire_format() {
        // Fixed inputs → stable envelope. Guards against accidental wire
        // format drift (field order, endianness, AAD contents). If this
        // test changes, the wire format has changed and every backed-up
        // kit in the wild would need re-encryption.
        let password = b"known-answer-password";
        let salt = [0x11u8; SALT_LEN];
        let nonce = [0x22u8; NONCE_LEN];
        let plaintext = b"KAT";
        let env = encrypt_with(
            plaintext,
            password,
            &salt,
            &nonce,
            TEST_PARAMS,
            ENVELOPE_VERSION,
            KDF_ID_ARGON2ID_V1,
            None,
        )
        .unwrap();

        // Decode and assert the prefix bytes explicitly — byte-comparing
        // the whole envelope as base64 would be fragile against any
        // future implementation detail in aes-gcm / argon2. The ciphertext
        // portion is covered by the roundtrip test.
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&env)
            .unwrap();
        assert_eq!(raw[0], ENVELOPE_VERSION, "version byte");
        assert_eq!(raw[1], KDF_ID_ARGON2ID_V1, "kdf_id byte");
        let m = u32::from(u16::from_be_bytes([raw[2], raw[3]]));
        assert_eq!(m, TEST_PARAMS.memory_kib);
        assert_eq!(raw[4], TEST_PARAMS.t_cost);
        assert_eq!(raw[5], TEST_PARAMS.p_cost);
        assert_eq!(&raw[HEADER_LEN..HEADER_LEN + SALT_LEN], &salt);
        assert_eq!(
            &raw[HEADER_LEN + SALT_LEN..HEADER_LEN + SALT_LEN + NONCE_LEN],
            &nonce
        );
        assert_eq!(
            raw.len(),
            HEADER_LEN + SALT_LEN + NONCE_LEN + plaintext.len() + TAG_LEN
        );
    }

    #[test]
    fn distinct_salts_and_nonces_across_calls() {
        // Two encrypts of the same plaintext under the same password must
        // produce different envelopes — otherwise the PRNG wiring is
        // broken or cached.
        let password = pw("pw");
        let a = encrypt(b"same", &password, TEST_PARAMS, "cube").unwrap();
        let b = encrypt(b"same", &password, TEST_PARAMS, "cube").unwrap();
        assert_ne!(a, b, "two sealings collided — RNG not feeding salt/nonce");
    }
}
