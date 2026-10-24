//! Separate, fixed-size local envelope for one imported BIP39 key record.
//!
//! This is deliberately not a seed-file version: old V1/V2/V3 seed readers,
//! costs, padding and byte layouts remain unchanged. Callers must make the
//! platform keystore decision before selecting `PinOnlyNoKeystore`; missing or
//! locked keystore data is never interpreted as permission to downgrade.

use std::{convert::TryInto, fmt};

use aes_gcm::{
    aead::{rand_core::RngCore, Aead, OsRng, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use miniscript::bitcoin::{bip32::Fingerprint, hashes::Hash};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    imported_keys::{account_in_both_branches, ImportedAccount},
    seed_crypt::{self, DeviceSecret, KdfParams, KDF_ID_ARGON2ID},
    wallet_descriptor::ImportedWalletDescriptor,
};

pub const MARKER: &[u8; 16] = b"CC_IMPORTED_KEY!";
pub const PADDED_PLAINTEXT_LEN: usize = 1024;
pub const MAX_MNEMONIC_BYTES: usize = 256;
pub const MAX_PASSPHRASE_BYTES: usize = 256;
pub const WIRE_LEN: usize = 16 + 8 + 16 + 12 + PADDED_PLAINTEXT_LEN + 16;
const HEADER_LEN: usize = 8;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const PIN_ONLY_VERSION: u8 = 1;
const DEVICE_BOUND_VERSION: u8 = 2;
const PAD_MAGIC: &[u8; 8] = b"CCIK\0\x01\0\0";
const HKDF_INFO: &[u8] = b"coincube/imported-key-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportedKeyCryptError {
    InvalidBinding,
    InvalidMnemonic,
    SecretTooLong,
    InvalidFormat,
    DeviceSecretRequired,
    Kdf,
    AuthenticationFailed,
    Randomness,
}
impl fmt::Display for ImportedKeyCryptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidBinding => "Invalid imported key record binding.",
            Self::InvalidMnemonic => "Invalid imported recovery phrase.",
            Self::SecretTooLong => "The imported phrase or passphrase exceeds its limit.",
            Self::InvalidFormat => "Invalid imported key record format.",
            Self::DeviceSecretRequired => "This imported key requires the device secret.",
            Self::Kdf => "Imported key derivation failed.",
            Self::AuthenticationFailed => "Imported key authentication failed.",
            Self::Randomness => "Imported key sealing randomness failed.",
        })
    }
}
impl std::error::Error for ImportedKeyCryptError {}

/// Every field is authenticated. The operation and record IDs are UUID bytes;
/// revision is a fresh UUID for each actual replacement, not a wall clock.
/// The full policy hash is the transport's canonical full-policy commitment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedKeyBinding {
    cube_id: String,
    chain: String,
    operation_id: [u8; 16],
    wallet_identity: [u8; 32],
    full_policy_hash: [u8; 32],
    fingerprint: Fingerprint,
    origin: String,
    xpub: String,
    record_id: [u8; 16],
    revision: [u8; 16],
}

impl ImportedKeyBinding {
    /// `full_policy_hash` must come from the admitted canonical D1 policy
    /// bytes, not an independently reserialized descriptor. The repository
    /// consumer is responsible for that provenance in R15b.2.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cube_id: &str,
        wallet: &ImportedWalletDescriptor,
        operation_id: [u8; 16],
        full_policy_hash: [u8; 32],
        account: &ImportedAccount,
        record_id: [u8; 16],
        revision: [u8; 16],
    ) -> Result<Self, ImportedKeyCryptError> {
        let origin = account.origin.to_string();
        let xpub = account.xpub.to_string();
        if cube_id.is_empty()
            || cube_id.len() > 128
            || origin.len() > 256
            || xpub.len() > 128
            || operation_id == [0; 16]
            || full_policy_hash == [0; 32]
            || record_id == [0; 16]
            || revision == [0; 16]
            || !account_in_both_branches(wallet, account)
        {
            return Err(ImportedKeyCryptError::InvalidBinding);
        }
        Ok(Self {
            cube_id: cube_id.into(),
            chain: wallet.chain().api_str().into(),
            operation_id,
            wallet_identity: wallet
                .identity(wallet.chain())
                .map_err(|_| ImportedKeyCryptError::InvalidBinding)?
                .to_byte_array(),
            full_policy_hash,
            fingerprint: account.fingerprint,
            origin,
            xpub,
            record_id,
            revision,
        })
    }

    fn aad(&self, header: &[u8; HEADER_LEN]) -> Vec<u8> {
        let mut out = Vec::with_capacity(512);
        out.extend_from_slice(MARKER);
        out.extend_from_slice(header);
        for field in [
            self.cube_id.as_bytes(),
            self.chain.as_bytes(),
            &self.operation_id,
            &self.wallet_identity,
            &self.full_policy_hash,
            self.fingerprint.as_bytes(),
            self.origin.as_bytes(),
            self.xpub.as_bytes(),
            &self.record_id,
            &self.revision,
        ] {
            out.extend_from_slice(&(field.len() as u16).to_be_bytes());
            out.extend_from_slice(field);
        }
        out
    }
}

/// Phrase and exact typed passphrase are kept only in zeroizing buffers.
pub struct ImportedKeySecret {
    words: Zeroizing<String>,
    passphrase: Zeroizing<String>,
}
impl fmt::Debug for ImportedKeySecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ImportedKeySecret([REDACTED])")
    }
}
impl ImportedKeySecret {
    pub fn new(
        words: Zeroizing<String>,
        passphrase: Zeroizing<String>,
    ) -> Result<Self, ImportedKeyCryptError> {
        if words.len() > MAX_MNEMONIC_BYTES || passphrase.len() > MAX_PASSPHRASE_BYTES {
            return Err(ImportedKeyCryptError::SecretTooLong);
        }
        bip39::Mnemonic::parse_in(bip39::Language::English, words.as_str())
            .map_err(|_| ImportedKeyCryptError::InvalidMnemonic)?;
        Ok(Self { words, passphrase })
    }
    pub fn words(&self) -> &str {
        self.words.as_str()
    }
    pub fn passphrase(&self) -> &str {
        self.passphrase.as_str()
    }
    /// Explicitly revoke in-memory source text before normal drop when an
    /// import or recovery workflow is cancelled.
    pub fn clear(&mut self) {
        self.words.zeroize();
        self.passphrase.zeroize();
    }
    pub fn into_parts(self) -> (Zeroizing<String>, Zeroizing<String>) {
        (self.words, self.passphrase)
    }
}

/// The platform must select this explicitly. `PinOnlyNoKeystore` is valid
/// only on a platform where no OS device-secret facility is available.
pub enum Protection<'a> {
    DeviceBound(&'a DeviceSecret),
    PinOnlyNoKeystore,
}

fn header(version: u8, params: KdfParams) -> [u8; HEADER_LEN] {
    let m = params.memory_kib.to_be_bytes();
    [
        version,
        KDF_ID_ARGON2ID,
        m[0],
        m[1],
        m[2],
        m[3],
        params.t_cost,
        params.p_cost,
    ]
}

fn parse_header(bytes: &[u8]) -> Result<(u8, KdfParams), ImportedKeyCryptError> {
    if bytes.len() != HEADER_LEN
        || bytes[1] != KDF_ID_ARGON2ID
        || !matches!(bytes[0], PIN_ONLY_VERSION | DEVICE_BOUND_VERSION)
    {
        return Err(ImportedKeyCryptError::InvalidFormat);
    }
    let params = KdfParams {
        memory_kib: u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]),
        t_cost: bytes[6],
        p_cost: bytes[7],
    };
    validate_header_params(params, seed_crypt::WRITE_PARAMS)?;
    Ok((bytes[0], params))
}

/// Pure pre-KDF check. The floor remains private and production always passes
/// the seed-file write cost; tests can prove that exact floor without paying
/// for a 256 MiB Argon2 derivation on every malformed-header case.
fn validate_header_params(
    params: KdfParams,
    floor: KdfParams,
) -> Result<(), ImportedKeyCryptError> {
    // Bound untrusted header work before Argon2 allocates memory. Production
    // V1/V2 records use at least the existing seed cost; a new format version
    // is required to raise this ceiling.
    if params.memory_kib < floor.memory_kib
        || params.memory_kib > 262_144
        || params.t_cost < floor.t_cost
        || params.t_cost > 6
        || params.p_cost < floor.p_cost
        || params.p_cost > 8
    {
        return Err(ImportedKeyCryptError::InvalidFormat);
    }
    Ok(())
}

fn key(
    pin: &str,
    salt: &[u8; SALT_LEN],
    params: KdfParams,
    device: Option<&DeviceSecret>,
    binding: &ImportedKeyBinding,
) -> Result<Zeroizing<[u8; 32]>, ImportedKeyCryptError> {
    let pin_half =
        seed_crypt::derive_key(pin, salt, params).map_err(|_| ImportedKeyCryptError::Kdf)?;
    let Some(device) = device else {
        return Ok(pin_half);
    };
    let mut ikm = Zeroizing::new(Vec::with_capacity(64));
    ikm.extend_from_slice(pin_half.as_ref());
    ikm.extend_from_slice(device.as_ref());
    let mut info = Vec::with_capacity(544);
    info.extend_from_slice(HKDF_INFO);
    info.extend_from_slice(&binding.aad(&header(DEVICE_BOUND_VERSION, params)));
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), &ikm);
    let mut key = Zeroizing::new([0u8; 32]);
    hk.expand(&info, key.as_mut())
        .map_err(|_| ImportedKeyCryptError::Kdf)?;
    Ok(key)
}

fn padded(secret: &ImportedKeySecret) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(vec![0u8; PADDED_PLAINTEXT_LEN]);
    out[..8].copy_from_slice(PAD_MAGIC);
    out[8..10].copy_from_slice(&(secret.words.len() as u16).to_be_bytes());
    out[10..12].copy_from_slice(&(secret.passphrase.len() as u16).to_be_bytes());
    out[12..12 + secret.words.len()].copy_from_slice(secret.words.as_bytes());
    let start = 12 + secret.words.len();
    out[start..start + secret.passphrase.len()].copy_from_slice(secret.passphrase.as_bytes());
    out
}

fn unpad(data: Zeroizing<Vec<u8>>) -> Result<ImportedKeySecret, ImportedKeyCryptError> {
    if data.len() != PADDED_PLAINTEXT_LEN || !data.starts_with(PAD_MAGIC) {
        return Err(ImportedKeyCryptError::InvalidFormat);
    }
    let w = u16::from_be_bytes([data[8], data[9]]) as usize;
    let p = u16::from_be_bytes([data[10], data[11]]) as usize;
    if w > MAX_MNEMONIC_BYTES
        || p > MAX_PASSPHRASE_BYTES
        || 12 + w + p > data.len()
        || data[12 + w + p..].iter().any(|byte| *byte != 0)
    {
        return Err(ImportedKeyCryptError::InvalidFormat);
    }
    let words =
        std::str::from_utf8(&data[12..12 + w]).map_err(|_| ImportedKeyCryptError::InvalidFormat)?;
    let passphrase = std::str::from_utf8(&data[12 + w..12 + w + p])
        .map_err(|_| ImportedKeyCryptError::InvalidFormat)?;
    ImportedKeySecret::new(
        Zeroizing::new(words.to_owned()),
        Zeroizing::new(passphrase.to_owned()),
    )
}

pub fn seal(
    secret: &ImportedKeySecret,
    pin: &str,
    binding: &ImportedKeyBinding,
    protection: Protection<'_>,
) -> Result<Vec<u8>, ImportedKeyCryptError> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    OsRng
        .try_fill_bytes(&mut salt)
        .map_err(|_| ImportedKeyCryptError::Randomness)?;
    OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| ImportedKeyCryptError::Randomness)?;
    seal_with(
        secret,
        pin,
        binding,
        protection,
        &salt,
        &nonce,
        seed_crypt::WRITE_PARAMS,
    )
}

fn seal_with(
    secret: &ImportedKeySecret,
    pin: &str,
    binding: &ImportedKeyBinding,
    protection: Protection<'_>,
    salt: &[u8; SALT_LEN],
    nonce: &[u8; NONCE_LEN],
    params: KdfParams,
) -> Result<Vec<u8>, ImportedKeyCryptError> {
    parse_header(&header(PIN_ONLY_VERSION, params))?;
    let (version, device) = match protection {
        Protection::DeviceBound(secret) => (DEVICE_BOUND_VERSION, Some(secret)),
        Protection::PinOnlyNoKeystore => (PIN_ONLY_VERSION, None),
    };
    let header = header(version, params);
    let key = key(pin, salt, params, device, binding)?;
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| ImportedKeyCryptError::Kdf)?;
    let framed = padded(secret);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: &framed,
                aad: &binding.aad(&header),
            },
        )
        .map_err(|_| ImportedKeyCryptError::AuthenticationFailed)?;
    let mut out = Vec::with_capacity(WIRE_LEN);
    out.extend_from_slice(MARKER);
    out.extend_from_slice(&header);
    out.extend_from_slice(salt);
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

pub fn open(
    data: &[u8],
    pin: &str,
    binding: &ImportedKeyBinding,
    device_secret: Option<&DeviceSecret>,
) -> Result<ImportedKeySecret, ImportedKeyCryptError> {
    if data.len() != WIRE_LEN || !data.starts_with(MARKER) {
        return Err(ImportedKeyCryptError::InvalidFormat);
    }
    let body = &data[MARKER.len()..];
    let header: [u8; HEADER_LEN] = body[..HEADER_LEN]
        .try_into()
        .map_err(|_| ImportedKeyCryptError::InvalidFormat)?;
    let (version, params) = parse_header(&header)?;
    let device = if version == DEVICE_BOUND_VERSION {
        Some(device_secret.ok_or(ImportedKeyCryptError::DeviceSecretRequired)?)
    } else {
        None
    };
    let salt: &[u8; SALT_LEN] = body[HEADER_LEN..HEADER_LEN + SALT_LEN]
        .try_into()
        .map_err(|_| ImportedKeyCryptError::InvalidFormat)?;
    let nonce = &body[HEADER_LEN + SALT_LEN..HEADER_LEN + SALT_LEN + NONCE_LEN];
    let ciphertext = &body[HEADER_LEN + SALT_LEN + NONCE_LEN..];
    let key = key(pin, salt, params, device, binding)?;
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| ImportedKeyCryptError::Kdf)?;
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &binding.aad(&header),
            },
        )
        .map_err(|_| ImportedKeyCryptError::AuthenticationFailed)?;
    unpad(Zeroizing::new(plaintext))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{chain::ChainId, imported_keys::verify_imported_account};
    use miniscript::bitcoin::{bip32::DerivationPath, secp256k1::Secp256k1};
    use std::str::FromStr;

    #[test]
    fn production_header_floor_and_ceiling_reject_each_dimension_before_kdf() {
        let production = KdfParams::SEED;
        assert_eq!(validate_header_params(production, KdfParams::SEED), Ok(()));
        assert_eq!(
            validate_header_params(KdfParams::TEST, KdfParams::SEED),
            Err(ImportedKeyCryptError::InvalidFormat)
        );
        for weaker in [
            KdfParams {
                memory_kib: production.memory_kib - 1,
                ..production
            },
            KdfParams {
                t_cost: production.t_cost - 1,
                ..production
            },
            KdfParams {
                p_cost: production.p_cost - 1,
                ..production
            },
        ] {
            assert_eq!(
                validate_header_params(weaker, KdfParams::SEED),
                Err(ImportedKeyCryptError::InvalidFormat),
                "production accepted an under-floor header: {weaker:?}"
            );
        }
        for excessive in [
            KdfParams {
                memory_kib: 262_145,
                ..production
            },
            KdfParams {
                t_cost: 7,
                ..production
            },
            KdfParams {
                p_cost: 9,
                ..production
            },
        ] {
            assert_eq!(
                validate_header_params(excessive, KdfParams::SEED),
                Err(ImportedKeyCryptError::InvalidFormat),
                "production accepted an over-ceiling header: {excessive:?}"
            );
        }
        // Unit codec tests intentionally use TEST; the pure check above
        // still proves the production floor without a high-cost KDF run.
        assert_eq!(
            parse_header(&header(PIN_ONLY_VERSION, KdfParams::TEST))
                .unwrap()
                .1,
            KdfParams::TEST
        );
    }

    fn fixture() -> (
        ImportedWalletDescriptor,
        ImportedAccount,
        ImportedKeySecret,
        ImportedKeyBinding,
    ) {
        let phrase = bip39::Mnemonic::from_entropy(&[0x42; 16])
            .unwrap()
            .to_string();
        let path = DerivationPath::from_str("m/84h/1h/0h").unwrap();
        let signer = crate::signer::SessionSigner::from_mnemonic(
            miniscript::bitcoin::Network::Regtest,
            bip39::Mnemonic::parse(&phrase).unwrap(),
            "synthetic passphrase",
        )
        .unwrap();
        let secp = Secp256k1::new();
        let key = format!(
            "[{}/84h/1h/0h]{}",
            signer.fingerprint(&secp),
            signer.xpub_at(&path, &secp),
        );
        let wallet = ImportedWalletDescriptor::parse(
            &format!("wpkh({key}/0/*)"),
            Some(&format!("wpkh({key}/1/*)")),
            ChainId::Regtest,
        )
        .unwrap();
        let verified = verify_imported_account(
            &wallet,
            &path,
            Zeroizing::new(phrase.clone()),
            Zeroizing::new("synthetic passphrase".into()),
        )
        .unwrap();
        let account = verified.account().clone();
        let secret = ImportedKeySecret::new(
            Zeroizing::new(phrase),
            Zeroizing::new("synthetic passphrase".into()),
        )
        .unwrap();
        let binding = ImportedKeyBinding::new(
            "cube-synthetic",
            &wallet,
            [1; 16],
            [2; 32],
            &account,
            [3; 16],
            [4; 16],
        )
        .unwrap();
        (wallet, account, secret, binding)
    }

    #[test]
    fn fixed_size_pin_and_device_roundtrip_and_redaction() {
        let (_, _, secret, binding) = fixture();
        let device = Zeroizing::new([7u8; 32]);
        for protection in [
            Protection::PinOnlyNoKeystore,
            Protection::DeviceBound(&device),
        ] {
            let sealed = seal_with(
                &secret,
                "synthetic pin",
                &binding,
                protection,
                &[5; SALT_LEN],
                &[6; NONCE_LEN],
                KdfParams::TEST,
            )
            .unwrap();
            assert_eq!(sealed.len(), WIRE_LEN);
            assert!(!sealed
                .windows(secret.words().len())
                .any(|w| w == secret.words().as_bytes()));
            let restored = open(&sealed, "synthetic pin", &binding, Some(&device)).unwrap();
            assert_eq!(restored.words(), secret.words());
            assert_eq!(restored.passphrase(), secret.passphrase());
            assert!(!format!("{restored:?}").contains(secret.words()));
            assert!(!format!("{restored:?}").contains(secret.passphrase()));
            assert_eq!(
                sealed,
                seal_with(
                    &secret,
                    "synthetic pin",
                    &binding,
                    if sealed[MARKER.len()] == DEVICE_BOUND_VERSION {
                        Protection::DeviceBound(&device)
                    } else {
                        Protection::PinOnlyNoKeystore
                    },
                    &[5; SALT_LEN],
                    &[6; NONCE_LEN],
                    KdfParams::TEST,
                )
                .unwrap()
            );
        }
        let mut cancelled = ImportedKeySecret::new(
            Zeroizing::new(secret.words().to_owned()),
            Zeroizing::new(secret.passphrase().to_owned()),
        )
        .unwrap();
        cancelled.clear();
        assert!(cancelled.words().is_empty());
        assert!(cancelled.passphrase().is_empty());
    }

    #[test]
    fn device_binding_pin_and_every_semantic_field_fail_closed() {
        let (_, _, secret, binding) = fixture();
        let device = Zeroizing::new([7u8; 32]);
        let sealed = seal_with(
            &secret,
            "synthetic pin",
            &binding,
            Protection::DeviceBound(&device),
            &[5; SALT_LEN],
            &[6; NONCE_LEN],
            KdfParams::TEST,
        )
        .unwrap();
        assert!(matches!(
            open(&sealed, "synthetic pin", &binding, None),
            Err(ImportedKeyCryptError::DeviceSecretRequired)
        ));
        assert!(matches!(
            open(&sealed, "wrong", &binding, Some(&device)),
            Err(ImportedKeyCryptError::AuthenticationFailed)
        ));
        assert!(matches!(
            open(
                &sealed,
                "synthetic pin",
                &binding,
                Some(&Zeroizing::new([8; 32]))
            ),
            Err(ImportedKeyCryptError::AuthenticationFailed)
        ));
        let mut variants = Vec::new();
        let mut change = binding.clone();
        change.cube_id.push('x');
        variants.push(change);
        let mut change = binding.clone();
        change.chain.push('x');
        variants.push(change);
        let mut change = binding.clone();
        change.operation_id[0] ^= 1;
        variants.push(change);
        let mut change = binding.clone();
        change.wallet_identity[0] ^= 1;
        variants.push(change);
        let mut change = binding.clone();
        change.full_policy_hash[0] ^= 1;
        variants.push(change);
        let mut change = binding.clone();
        change.fingerprint = "00000000".parse().unwrap();
        variants.push(change);
        let mut change = binding.clone();
        change.origin.push('x');
        variants.push(change);
        let mut change = binding.clone();
        change.xpub.push('x');
        variants.push(change);
        let mut change = binding.clone();
        change.record_id[0] ^= 1;
        variants.push(change);
        let mut change = binding.clone();
        change.revision[0] ^= 1;
        variants.push(change);
        for changed in variants {
            assert!(matches!(
                open(&sealed, "synthetic pin", &changed, Some(&device)),
                Err(ImportedKeyCryptError::AuthenticationFailed)
            ));
        }
    }

    #[test]
    fn malformed_header_length_padding_and_limits_rejected_before_kdf() {
        let (_, _, secret, binding) = fixture();
        let device = Zeroizing::new([7u8; 32]);
        let sealed = seal_with(
            &secret,
            "synthetic pin",
            &binding,
            Protection::DeviceBound(&device),
            &[5; SALT_LEN],
            &[6; NONCE_LEN],
            KdfParams::TEST,
        )
        .unwrap();
        for length in [0, 1, WIRE_LEN - 1, WIRE_LEN + 1] {
            let mut malformed = sealed.clone();
            malformed.resize(length, 0);
            assert!(matches!(
                open(&malformed, "synthetic pin", &binding, Some(&device)),
                Err(ImportedKeyCryptError::InvalidFormat)
            ));
        }
        for (position, value) in [
            (0, b'X'),
            (16, 3),
            (17, 2),
            (18, 0xff),
            (22, 255),
            (23, 255),
        ] {
            let mut malformed = sealed.clone();
            malformed[position] = value;
            assert!(matches!(
                open(&malformed, "synthetic pin", &binding, Some(&device)),
                Err(ImportedKeyCryptError::InvalidFormat)
            ));
        }
        let mut downgrade = sealed.clone();
        downgrade[16] = PIN_ONLY_VERSION;
        assert!(open(&downgrade, "synthetic pin", &binding, Some(&device)).is_err());
        let mut tampered = sealed.clone();
        tampered[100] ^= 1;
        assert!(matches!(
            open(&tampered, "synthetic pin", &binding, Some(&device)),
            Err(ImportedKeyCryptError::AuthenticationFailed)
        ));
        let mut plain = padded(&secret);
        let end = 12 + secret.words().len() + secret.passphrase().len();
        plain[end] = 1;
        assert!(matches!(
            unpad(plain),
            Err(ImportedKeyCryptError::InvalidFormat)
        ));
        let mut plain = padded(&secret);
        plain[12] = 0xff;
        assert!(matches!(
            unpad(plain),
            Err(ImportedKeyCryptError::InvalidFormat)
        ));
        assert!(matches!(
            ImportedKeySecret::new(
                Zeroizing::new("x".repeat(257)),
                Zeroizing::new(String::new())
            ),
            Err(ImportedKeyCryptError::SecretTooLong)
        ));
        assert!(matches!(
            ImportedKeySecret::new(Zeroizing::new("x".into()), Zeroizing::new("x".repeat(257))),
            Err(ImportedKeyCryptError::SecretTooLong)
        ));
        assert!(matches!(
            ImportedKeySecret::new(
                Zeroizing::new("not a valid phrase".into()),
                Zeroizing::new(String::new())
            ),
            Err(ImportedKeyCryptError::InvalidMnemonic)
        ));
    }

    #[test]
    fn raw_utf8_passphrase_limit_is_independent_of_nfkd_derivation() {
        let (wallet, account, secret, _) = fixture();
        let path = account.origin.clone();
        let composed = "é".repeat(128); // 256 raw bytes, 384 after NFKD.
        let decomposed = "e\u{301}".repeat(128);
        let phrase = secret.words().to_owned();
        let signer = crate::signer::SessionSigner::from_mnemonic(
            miniscript::bitcoin::Network::Regtest,
            bip39::Mnemonic::parse(&phrase).unwrap(),
            &composed,
        )
        .unwrap();
        let secp = Secp256k1::new();
        let key = format!(
            "[{}/84h/1h/0h]{}",
            signer.fingerprint(&secp),
            signer.xpub_at(&path, &secp)
        );
        let composed_wallet = ImportedWalletDescriptor::parse(
            &format!("wpkh({key}/0/*)"),
            Some(&format!("wpkh({key}/1/*)")),
            ChainId::Regtest,
        )
        .unwrap();
        for pass in [&composed, &decomposed] {
            // Decomposed raw input exceeds 256 bytes and is rejected before derivation.
            let result = verify_imported_account(
                &composed_wallet,
                &path,
                Zeroizing::new(phrase.clone()),
                Zeroizing::new(pass.to_string()),
            );
            if pass.len() <= MAX_PASSPHRASE_BYTES {
                assert!(result.is_ok());
            } else {
                assert!(matches!(
                    result,
                    Err(crate::imported_keys::ImportedKeyError::SecretTooLong)
                ));
            }
        }
        let short_composed = "café";
        let short_decomposed = "cafe\u{301}";
        let signer = crate::signer::SessionSigner::from_mnemonic(
            miniscript::bitcoin::Network::Regtest,
            bip39::Mnemonic::parse(&phrase).unwrap(),
            short_composed,
        )
        .unwrap();
        let short_key = format!(
            "[{}/84h/1h/0h]{}",
            signer.fingerprint(&secp),
            signer.xpub_at(&path, &secp),
        );
        let short_wallet = ImportedWalletDescriptor::parse(
            &format!("wpkh({short_key}/0/*)"),
            Some(&format!("wpkh({short_key}/1/*)")),
            ChainId::Regtest,
        )
        .unwrap();
        for pass in [short_composed, short_decomposed] {
            assert!(verify_imported_account(
                &short_wallet,
                &path,
                Zeroizing::new(phrase.clone()),
                Zeroizing::new(pass.into()),
            )
            .is_ok());
        }
        assert!(matches!(
            verify_imported_account(
                &short_wallet,
                &path,
                Zeroizing::new(phrase.clone()),
                Zeroizing::new("café!".into()),
            ),
            Err(crate::imported_keys::ImportedKeyError::KeyDoesNotMatchWallet)
        ));
        let raw = ImportedKeySecret::new(Zeroizing::new(phrase), Zeroizing::new(composed.clone()))
            .unwrap();
        let binding = ImportedKeyBinding::new(
            "cube-synthetic",
            &composed_wallet,
            [1; 16],
            [2; 32],
            &verify_imported_account(
                &composed_wallet,
                &path,
                Zeroizing::new(raw.words().to_owned()),
                Zeroizing::new(raw.passphrase().to_owned()),
            )
            .unwrap()
            .account()
            .clone(),
            [3; 16],
            [4; 16],
        )
        .unwrap();
        let sealed = seal_with(
            &raw,
            "pin",
            &binding,
            Protection::PinOnlyNoKeystore,
            &[5; 16],
            &[6; 12],
            KdfParams::TEST,
        )
        .unwrap();
        assert_eq!(
            open(&sealed, "pin", &binding, None).unwrap().passphrase(),
            composed
        );
        assert_ne!(
            wallet.identity(wallet.chain()).unwrap(),
            composed_wallet.identity(composed_wallet.chain()).unwrap()
        );
    }
}
