//! Orchestrates zero-knowledge encrypt/decrypt around the raw HTTP calls in [`super::client`].
//!
//! Transcribed method-by-method from `Branta/V2/Services/BrantaService.cs`. The single most
//! important behavioral rule enforced throughout this file: decryption failures (wrong key,
//! mismatched destination, DEK decrypt failure) are always swallowed internally -- a destination
//! that fails to decrypt is simply left encrypted (`is_encrypted = true`), never surfaced as an
//! `Err`. The one deliberate exception: when a QR code carries both a plaintext on-chain address
//! and `branta_id`/`branta_secret` params, a *successful* decrypt of the ZK Bitcoin-address
//! destination is compared against that plaintext address, and a mismatch propagates
//! `Err(BrantaError::Tampered)` -- this is the only decrypt-path error `decrypt_destinations` is
//! allowed to surface. Otherwise, only `add_payment`'s HTTP call and the client's logo-domain
//! check are allowed to propagate an error out of this type.

use indexmap::IndexMap;
use percent_encoding::utf8_percent_encode;

use crate::enums::{DestinationType, PrivacyMode};
use crate::error::BrantaError;
use crate::extensions::{get_hash_zk_type, to_normalized_hash, to_url_fragment};
use crate::models::{AddPaymentResult, Payment, PaymentsResult};
use crate::options::BrantaClientOptions;

use super::client::{BrantaClient, BrantaClientTrait, PATH_SEGMENT};
use super::encryption::{AesEncryptionService, AesEncryptionTrait};
use super::parser::QrParser;
use super::secret_generator::{GuidSecretGenerator, SecretGeneratorTrait};

/// Bech32 (`bc1...`) addresses compare case-insensitively (wallets often render them uppercase in
/// QR codes for denser encoding); base58 stays exact-match since case is semantically significant
/// there.
fn addresses_match(a: &str, b: &str) -> bool {
    fn is_bech32(v: &str) -> bool {
        v.to_lowercase().starts_with("bc1")
    }
    if is_bech32(a) && is_bech32(b) {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

pub struct BrantaService {
    default_options: BrantaClientOptions,
    client: Box<dyn BrantaClientTrait>,
    aes: Box<dyn AesEncryptionTrait>,
    secret_generator: Box<dyn SecretGeneratorTrait>,
}

impl BrantaService {
    /// Production constructor: real HTTP client, real AES-GCM, real UUID generator.
    pub fn new(default_options: BrantaClientOptions) -> Self {
        Self {
            client: Box::new(BrantaClient::new(default_options.clone())),
            aes: Box::new(AesEncryptionService),
            secret_generator: Box::new(GuidSecretGenerator),
            default_options,
        }
    }

    /// Test/advanced constructor: inject alternate implementations (e.g. mocks).
    pub fn with_deps(
        default_options: BrantaClientOptions,
        client: Box<dyn BrantaClientTrait>,
        aes: Box<dyn AesEncryptionTrait>,
        secret_generator: Box<dyn SecretGeneratorTrait>,
    ) -> Self {
        Self {
            default_options,
            client,
            aes,
            secret_generator,
        }
    }

    pub async fn get_payments(
        &self,
        destination_value: &str,
        destination_encryption_key: Option<&str>,
        options: Option<&BrantaClientOptions>,
    ) -> Result<PaymentsResult, BrantaError> {
        let hash_zk_type = get_hash_zk_type(destination_value);
        let privacy = self.default_options.get_privacy(options);

        // Supplying a key at all signals ZK intent and bypasses this check, even for a
        // non-hash-ZK value (e.g. a Bitcoin address looked up with its secret in Strict mode).
        if hash_zk_type.is_none()
            && destination_encryption_key.is_none()
            && privacy == PrivacyMode::Strict
        {
            return Err(BrantaError::PrivacyModeViolation);
        }

        let normalized = if hash_zk_type.is_some() {
            destination_value.to_lowercase()
        } else {
            destination_value.to_string()
        };

        let mut lookup_value = if hash_zk_type.is_some() {
            self.aes
                .encrypt(&normalized, &to_normalized_hash(&normalized), true)
                .await?
        } else {
            destination_value.to_string()
        };

        let mut payments = self.client.get_payments(&lookup_value, options).await?;

        // Loose mode only: fall back to a plain-value lookup if the encrypted lookup missed.
        // Strict mode never falls back to plain.
        if payments.is_empty() && hash_zk_type.is_some() && privacy != PrivacyMode::Strict {
            lookup_value = normalized.clone();
            payments = self.client.get_payments(&lookup_value, options).await?;
        }

        let mut keys = IndexMap::new();
        for payment in &mut payments {
            self.decrypt_destinations(
                payment,
                &normalized,
                destination_encryption_key,
                hash_zk_type,
                &mut keys,
                None,
            )
            .await?;
        }

        Ok(PaymentsResult {
            payments,
            verify_url: self.build_verify_url(options, &lookup_value, &keys),
        })
    }

    pub async fn get_payments_by_qr_code(
        &self,
        qr_text: &str,
        options: Option<&BrantaClientOptions>,
    ) -> Result<PaymentsResult, BrantaError> {
        let parser = QrParser::new(qr_text);

        if parser.is_on_chain_zk() {
            let on_chain_text = parser.on_chain_encryption_text.clone().unwrap();
            let additional_values: Vec<String> = parser
                .destinations
                .iter()
                .filter(|d| get_hash_zk_type(&d.value).is_some())
                .map(|d| d.value.clone())
                .collect();
            let on_chain_address = parser
                .destinations
                .iter()
                .find(|d| d.r#type == Some(DestinationType::BitcoinAddress))
                .map(|d| d.value.clone());
            return self
                .get_payments_for_zk(
                    &on_chain_text,
                    parser.on_chain_encryption_secret.as_deref(),
                    &additional_values,
                    on_chain_address.as_deref(),
                    options,
                )
                .await;
        }

        // A parser that found nothing at all degrades to an empty result rather than panicking
        // -- QrParser is infallible by contract, and so is this method.
        let Some(destination) = parser.destination().map(str::to_string) else {
            return Ok(PaymentsResult {
                payments: Vec::new(),
                verify_url: self.build_verify_url(options, "", &IndexMap::new()),
            });
        };

        if self.default_options.get_privacy(options) == PrivacyMode::Strict
            && get_hash_zk_type(&destination).is_none()
        {
            // Strict mode, plain-text destination: never even reach the network.
            return Ok(PaymentsResult {
                payments: Vec::new(),
                verify_url: self.build_verify_url(options, &destination, &IndexMap::new()),
            });
        }

        self.get_payments(&destination, None, options).await
    }

    async fn get_payments_for_zk(
        &self,
        lookup_value: &str,
        encryption_key: Option<&str>,
        additional_hash_values: &[String],
        expected_on_chain_address: Option<&str>,
        options: Option<&BrantaClientOptions>,
    ) -> Result<PaymentsResult, BrantaError> {
        let mut payments = self.client.get_payments(lookup_value, options).await?;

        let mut keys = IndexMap::new();
        for payment in &mut payments {
            self.decrypt_destinations(
                payment,
                lookup_value,
                encryption_key,
                None,
                &mut keys,
                expected_on_chain_address,
            )
            .await?;
            for value in additional_hash_values {
                self.decrypt_hash_zk_destinations(payment, value, &mut keys)
                    .await;
            }
        }

        Ok(PaymentsResult {
            payments,
            verify_url: self.build_verify_url(options, lookup_value, &keys),
        })
    }

    pub async fn add_payment(
        &self,
        mut payment: Payment,
        options: Option<&BrantaClientOptions>,
    ) -> Result<AddPaymentResult, BrantaError> {
        if self.default_options.get_privacy(options) == PrivacyMode::Strict
            && payment.destinations.iter().any(|d| !d.is_zk)
        {
            return Err(BrantaError::NonZkDestinationInStrictMode);
        }

        let mut dek: Option<String> = None;
        if payment.metadata.is_some() && payment.destinations.iter().any(|d| d.is_zk) {
            let generated = self.secret_generator.generate();
            let metadata = payment.metadata.as_deref().unwrap().to_string();
            payment.metadata = Some(self.aes.encrypt(&metadata, &generated, false).await?);
            dek = Some(generated);
        }

        // One shared secret for every BitcoinAddress ZK destination in this payment.
        let secret = self.secret_generator.generate();
        let mut encrypted_to_key: IndexMap<String, String> = IndexMap::new();

        for destination in payment.destinations.iter_mut() {
            if !destination.is_zk {
                continue;
            }

            if destination.r#type == Some(DestinationType::BitcoinAddress) {
                let ciphertext = self
                    .aes
                    .encrypt(
                        &destination.value,
                        &secret,
                        self.secret_generator.deterministic_nonce(),
                    )
                    .await?;
                destination.value = ciphertext.clone();
                encrypted_to_key.insert(ciphertext, secret.clone());
                if let Some(dek_value) = &dek {
                    destination.encrypted_dek =
                        Some(self.aes.encrypt(dek_value, &secret, false).await?);
                }
            } else {
                if get_hash_zk_type(&destination.value).is_none() {
                    return Err(BrantaError::UnsupportedZkDestinationType(
                        destination.r#type,
                    ));
                }
                let normalized = destination.value.to_lowercase();
                let key = to_normalized_hash(&normalized);
                let ciphertext = self.aes.encrypt(&normalized, &key, true).await?;
                destination.value = ciphertext.clone();
                encrypted_to_key.insert(ciphertext, key.clone());
                if let Some(dek_value) = &dek {
                    destination.encrypted_dek =
                        Some(self.aes.encrypt(dek_value, &key, false).await?);
                }
            }
        }

        // Snapshot the (now-mutated, possibly-ciphertext) primary value before `payment` moves
        // into `post_payment` -- ordering matters, see `BrantaService.cs` lines 174-208.
        let primary_value = payment
            .destinations
            .first()
            .map(|d| d.value.clone())
            .unwrap_or_default();

        let response_payment = self
            .client
            .post_payment(payment, options)
            .await?
            .ok_or(BrantaError::NoPaymentReturned)?;

        let mut keys = IndexMap::new();
        for destination in &response_payment.destinations {
            if let Some(zk_id) = &destination.zk_id {
                if let Some(key) = encrypted_to_key.get(&destination.value) {
                    keys.insert(zk_id.clone(), key.clone());
                }
            }
        }

        let verify_url = self.build_verify_url(options, &primary_value, &keys);

        Ok(AddPaymentResult {
            payment: response_payment,
            secret,
            verify_url,
        })
    }

    pub async fn is_api_key_valid(
        &self,
        options: Option<&BrantaClientOptions>,
    ) -> Result<bool, BrantaError> {
        self.client.is_api_key_valid(options).await
    }

    /// For every destination on `payment`: sets `is_encrypted` unconditionally, then attempts a
    /// decrypt only for ZK destinations whose type matches (BitcoinAddress with a caller-supplied
    /// key, or a hash-ZK type derived from `destination_value`). Decrypt failures are swallowed.
    ///
    /// The one case that *does* propagate an error: `expected_on_chain_address` is the plaintext
    /// Bitcoin address parsed straight from a scanned QR code (when one was present). If the
    /// BitcoinAddress destination decrypts successfully but doesn't match it, this is a sign the
    /// QR's visible address was swapped while its `branta_id`/`branta_secret` were left pointing
    /// at a legitimate payment -- returns `Err(BrantaError::Tampered)` instead of trusting the
    /// decrypted value.
    async fn decrypt_destinations(
        &self,
        payment: &mut Payment,
        destination_value: &str,
        encryption_key: Option<&str>,
        hash_zk_type: Option<DestinationType>,
        keys: &mut IndexMap<String, String>,
        expected_on_chain_address: Option<&str>,
    ) -> Result<(), BrantaError> {
        for i in 0..payment.destinations.len() {
            let is_zk = payment.destinations[i].is_zk;
            payment.destinations[i].is_encrypted = is_zk;
            if !is_zk {
                continue;
            }

            let dest_type = payment.destinations[i].r#type;

            if dest_type == Some(DestinationType::BitcoinAddress) {
                let Some(key) = encryption_key else { continue };
                let value = payment.destinations[i].value.clone();
                let Ok(plaintext) = self.aes.decrypt(&value, key).await else {
                    continue;
                };

                if let Some(expected) = expected_on_chain_address {
                    if !addresses_match(&plaintext, expected) {
                        return Err(BrantaError::Tampered);
                    }
                }

                payment.destinations[i].value = plaintext;
                payment.destinations[i].is_encrypted = false;
                if let Some(zk_id) = payment.destinations[i].zk_id.clone() {
                    keys.entry(zk_id).or_insert_with(|| key.to_string());
                }
                self.try_decrypt_metadata(payment, i, key).await;
            } else if let Some(hzt) = hash_zk_type {
                if dest_type == Some(hzt) {
                    let key = to_normalized_hash(destination_value);
                    let value = payment.destinations[i].value.clone();
                    if let Ok(plaintext) = self.aes.decrypt(&value, &key).await {
                        payment.destinations[i].value = plaintext;
                        payment.destinations[i].is_encrypted = false;
                        if let Some(zk_id) = payment.destinations[i].zk_id.clone() {
                            keys.entry(zk_id).or_insert_with(|| key.clone());
                        }
                        self.try_decrypt_metadata(payment, i, &key).await;
                    }
                }
            }
        }
        Ok(())
    }

    /// Used only from the on-chain-ZK combined-QR path: decrypts destinations whose type matches
    /// `get_hash_zk_type(plain_value)`, keyed purely off that value's derived hash (no caller
    /// supplied key needed).
    async fn decrypt_hash_zk_destinations(
        &self,
        payment: &mut Payment,
        plain_value: &str,
        keys: &mut IndexMap<String, String>,
    ) {
        let Some(hash_zk_type) = get_hash_zk_type(plain_value) else {
            return;
        };
        let key = to_normalized_hash(plain_value);

        for i in 0..payment.destinations.len() {
            if !payment.destinations[i].is_zk
                || payment.destinations[i].r#type != Some(hash_zk_type)
            {
                continue;
            }
            let value = payment.destinations[i].value.clone();
            if let Ok(plaintext) = self.aes.decrypt(&value, &key).await {
                payment.destinations[i].value = plaintext;
                payment.destinations[i].is_encrypted = false;
                if let Some(zk_id) = payment.destinations[i].zk_id.clone() {
                    keys.entry(zk_id).or_insert_with(|| key.clone());
                }
                self.try_decrypt_metadata(payment, i, &key).await;
            }
        }
    }

    /// No-op unless the destination at `index` has an `encrypted_dek`, `payment.metadata` is
    /// set, and metadata hasn't already been decrypted for this payment (only the first
    /// successful ZK destination decrypt attempts this; later ones skip it even if they'd also
    /// succeed). Swallows any decrypt failure.
    async fn try_decrypt_metadata(&self, payment: &mut Payment, index: usize, key_used: &str) {
        if payment.is_metadata_decrypted {
            return;
        }
        let Some(encrypted_dek) = payment.destinations[index].encrypted_dek.clone() else {
            return;
        };
        let Some(metadata) = payment.metadata.clone() else {
            return;
        };

        let Ok(dek) = self.aes.decrypt(&encrypted_dek, key_used).await else {
            return;
        };
        let Ok(plaintext_metadata) = self.aes.decrypt(&metadata, &dek).await else {
            return;
        };

        payment.metadata = Some(plaintext_metadata);
        payment.is_metadata_decrypted = true;
    }

    fn build_verify_url(
        &self,
        options: Option<&BrantaClientOptions>,
        payment_lookup: &str,
        keys: &IndexMap<String, String>,
    ) -> String {
        let base_url = self.default_options.get_base_url(options);
        let encoded = utf8_percent_encode(payment_lookup, PATH_SEGMENT).to_string();
        let mut url = format!("{base_url}/v2/verify/{encoded}");
        if !keys.is_empty() {
            url.push_str(&to_url_fragment(keys.iter()));
        }
        url
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::BrantaServerBaseUrl;
    use crate::models::Destination;
    use crate::v2::client::MockBrantaClientTrait;
    use crate::v2::encryption::MockAesEncryptionTrait;
    use crate::v2::secret_generator::MockSecretGeneratorTrait;

    fn strict_options() -> BrantaClientOptions {
        BrantaClientOptions::new(BrantaServerBaseUrl::Staging)
    }

    fn loose_options() -> BrantaClientOptions {
        let mut opts = BrantaClientOptions::new(BrantaServerBaseUrl::Staging);
        opts.privacy = PrivacyMode::Loose;
        opts
    }

    fn service(
        options: BrantaClientOptions,
        client: MockBrantaClientTrait,
        aes: MockAesEncryptionTrait,
        secret_generator: MockSecretGeneratorTrait,
    ) -> BrantaService {
        BrantaService::with_deps(
            options,
            Box::new(client),
            Box::new(aes),
            Box::new(secret_generator),
        )
    }

    fn zk_destination(value: &str, zk_id: &str, r#type: DestinationType) -> Destination {
        Destination {
            value: value.to_string(),
            is_primary: false,
            is_zk: true,
            is_encrypted: false,
            r#type: Some(r#type),
            zk_id: Some(zk_id.to_string()),
            encrypted_dek: None,
        }
    }

    // ---- get_payments: privacy-mode gating ----

    #[tokio::test]
    async fn strict_mode_plain_bitcoin_lookup_without_key_errors() {
        let client = MockBrantaClientTrait::new();
        let aes = MockAesEncryptionTrait::new();
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(strict_options(), client, aes, secret_gen);

        let result = svc
            .get_payments("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa", None, None)
            .await;
        assert!(matches!(result, Err(BrantaError::PrivacyModeViolation)));
    }

    #[tokio::test]
    async fn strict_mode_plain_bitcoin_lookup_with_key_succeeds() {
        // Supplying a key at all bypasses the plain-lookup ban, even for a non-hash-ZK value.
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| Ok(vec![]));
        let aes = MockAesEncryptionTrait::new();
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(strict_options(), client, aes, secret_gen);

        let result = svc
            .get_payments("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa", Some("some-key"), None)
            .await
            .unwrap();
        assert!(result.payments.is_empty());
    }

    #[tokio::test]
    async fn strict_mode_hash_zk_lookup_does_not_error_and_uses_encrypted_lookup() {
        let mut client = MockBrantaClientTrait::new();
        client
            .expect_get_payments()
            .withf(|value, _| value == "ENCRYPTED-LOOKUP")
            .returning(|_, _| Ok(vec![]));
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_encrypt()
            .withf(|value, _, deterministic| value == "lnbc1qsomething" && *deterministic)
            .returning(|_, _, _| Ok("ENCRYPTED-LOOKUP".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(strict_options(), client, aes, secret_gen);

        let result = svc
            .get_payments("lnbc1qsomething", None, None)
            .await
            .unwrap();
        assert!(result.payments.is_empty());
    }

    #[tokio::test]
    async fn strict_mode_hash_zk_not_found_never_falls_back_to_plain() {
        let mut client = MockBrantaClientTrait::new();
        client
            .expect_get_payments()
            .times(1)
            .returning(|_, _| Ok(vec![])); // exactly once: no fallback call
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_encrypt()
            .returning(|_, _, _| Ok("ENCRYPTED-LOOKUP".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(strict_options(), client, aes, secret_gen);

        svc.get_payments("lnbc1qsomething", None, None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn loose_mode_hash_zk_not_found_falls_back_to_plain_lookup() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().times(2).returning(|value, _| {
            if value == "lnbc1qsomething" {
                Ok(vec![Payment {
                    destinations: vec![],
                    ..Default::default()
                }])
            } else {
                Ok(vec![])
            }
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_encrypt()
            .returning(|_, _, _| Ok("ENCRYPTED-LOOKUP".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let result = svc
            .get_payments("lnbc1qsomething", None, None)
            .await
            .unwrap();
        assert_eq!(result.payments.len(), 1);
    }

    // ---- get_payments: ZK destination decrypt behavior ----

    #[tokio::test]
    async fn zk_bitcoin_destination_decrypts_with_correct_key() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![zk_destination(
                    "CIPHERTEXT",
                    "zk-1",
                    DestinationType::BitcoinAddress,
                )],
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .withf(|value, key| value == "CIPHERTEXT" && key == "correct-key")
            .returning(|_, _| Ok("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let result = svc
            .get_payments("CIPHERTEXT", Some("correct-key"), None)
            .await
            .unwrap();
        let dest = &result.payments[0].destinations[0];
        assert_eq!(dest.value, "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa");
        assert!(!dest.is_encrypted);
    }

    #[tokio::test]
    async fn zk_bitcoin_destination_wrong_key_stays_encrypted_no_error() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![zk_destination(
                    "CIPHERTEXT",
                    "zk-1",
                    DestinationType::BitcoinAddress,
                )],
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .returning(|_, _| Err(BrantaError::DecryptionFailed("bad key".into())));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let result = svc
            .get_payments("CIPHERTEXT", Some("wrong-key"), None)
            .await
            .unwrap();
        let dest = &result.payments[0].destinations[0];
        assert_eq!(dest.value, "CIPHERTEXT"); // untouched
        assert!(dest.is_encrypted);
    }

    #[tokio::test]
    async fn zk_bitcoin_destination_no_key_supplied_stays_encrypted() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![zk_destination(
                    "CIPHERTEXT",
                    "zk-1",
                    DestinationType::BitcoinAddress,
                )],
                ..Default::default()
            }])
        });
        let aes = MockAesEncryptionTrait::new(); // decrypt must never be called
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let result = svc.get_payments("CIPHERTEXT", None, None).await.unwrap();
        assert!(result.payments[0].destinations[0].is_encrypted);
    }

    #[tokio::test]
    async fn non_zk_destination_is_left_completely_untouched() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![Destination::new(
                    "plain-value",
                    Some(DestinationType::BitcoinAddress),
                )],
                ..Default::default()
            }])
        });
        let aes = MockAesEncryptionTrait::new(); // decrypt must never be called
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let result = svc.get_payments("plain-value", None, None).await.unwrap();
        assert_eq!(result.payments[0].destinations[0].value, "plain-value");
        assert!(!result.payments[0].destinations[0].is_encrypted);
    }

    #[tokio::test]
    async fn hash_zk_destination_decrypts_by_hash_derived_key() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![zk_destination(
                    "CIPHERTEXT",
                    "zk-1",
                    DestinationType::Bolt11,
                )],
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_encrypt()
            .returning(|_, _, _| Ok("LOOKUP".to_string()));
        aes.expect_decrypt()
            .withf(|value, key| {
                value == "CIPHERTEXT" && key == to_normalized_hash("lnbc1qsomething")
            })
            .returning(|_, _| Ok("lnbc1qsomething".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let result = svc
            .get_payments("lnbc1qsomething", None, None)
            .await
            .unwrap();
        assert_eq!(result.payments[0].destinations[0].value, "lnbc1qsomething");
        assert!(!result.payments[0].destinations[0].is_encrypted);
    }

    #[tokio::test]
    async fn verify_url_includes_fragment_when_a_key_resolved() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![zk_destination(
                    "CIPHERTEXT",
                    "zk-1",
                    DestinationType::BitcoinAddress,
                )],
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .returning(|_, _| Ok("1A1zP...".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let result = svc
            .get_payments("CIPHERTEXT", Some("key"), None)
            .await
            .unwrap();
        assert!(result.verify_url.contains("#k-zk-1=key"));
    }

    #[tokio::test]
    async fn verify_url_has_no_fragment_when_no_key_resolved() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| Ok(vec![]));
        let aes = MockAesEncryptionTrait::new();
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let result = svc.get_payments("plain-value", None, None).await.unwrap();
        assert!(!result.verify_url.contains('#'));
        assert!(result.verify_url.contains("/v2/verify/plain-value"));
    }

    // ---- get_payments_by_qr_code ----

    #[tokio::test]
    async fn qr_strict_plain_bitcoin_short_circuits_with_no_network_call() {
        let client = MockBrantaClientTrait::new(); // no expectations set -> panics if called
        let aes = MockAesEncryptionTrait::new();
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(strict_options(), client, aes, secret_gen);

        let result = svc
            .get_payments_by_qr_code("bitcoin:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa", None)
            .await
            .unwrap();
        assert!(result.payments.is_empty());
        assert!(!result.verify_url.is_empty());
    }

    #[tokio::test]
    async fn qr_strict_hash_zk_destination_succeeds_over_network() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| Ok(vec![]));
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_encrypt()
            .returning(|_, _, _| Ok("LOOKUP".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(strict_options(), client, aes, secret_gen);

        let result = svc
            .get_payments_by_qr_code("lnbc1qsomething", None)
            .await
            .unwrap();
        assert!(result.payments.is_empty());
    }

    #[tokio::test]
    async fn qr_on_chain_zk_decrypts_bitcoin_with_secret() {
        let mut client = MockBrantaClientTrait::new();
        client
            .expect_get_payments()
            .withf(|value, _| value == "onchain-id")
            .returning(|_, _| {
                Ok(vec![Payment {
                    destinations: vec![zk_destination(
                        "CIPHERTEXT",
                        "zk-1",
                        DestinationType::BitcoinAddress,
                    )],
                    ..Default::default()
                }])
            });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .withf(|value, key| value == "CIPHERTEXT" && key == "onchain-secret")
            .returning(|_, _| Ok("1A1zP...".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let qr = "bitcoin:1A1zP...?branta_id=onchain-id&branta_secret=onchain-secret";
        let result = svc.get_payments_by_qr_code(qr, None).await.unwrap();
        assert_eq!(result.payments[0].destinations[0].value, "1A1zP...");
    }

    #[tokio::test]
    async fn qr_combined_on_chain_zk_also_decrypts_hash_zk_destination() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![
                    zk_destination("BTC-CIPHERTEXT", "zk-1", DestinationType::BitcoinAddress),
                    zk_destination("LN-CIPHERTEXT", "zk-2", DestinationType::Bolt11),
                ],
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .withf(|value, key| value == "BTC-CIPHERTEXT" && key == "onchain-secret")
            .returning(|_, _| Ok("1A1zP...".to_string()));
        aes.expect_decrypt()
            .withf(|value, key| {
                value == "LN-CIPHERTEXT" && key == to_normalized_hash("lnbc1qsomething")
            })
            .returning(|_, _| Ok("lnbc1qsomething".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let qr = "bitcoin:1A1zP...?branta_id=onchain-id&branta_secret=onchain-secret&lightning=lnbc1qsomething";
        let result = svc.get_payments_by_qr_code(qr, None).await.unwrap();
        assert_eq!(result.payments[0].destinations[0].value, "1A1zP...");
        assert_eq!(result.payments[0].destinations[1].value, "lnbc1qsomething");
    }

    #[tokio::test]
    async fn qr_on_chain_zk_leaves_unrelated_destination_encrypted() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![
                    zk_destination("BTC-CIPHERTEXT", "zk-1", DestinationType::BitcoinAddress),
                    zk_destination("ARK-CIPHERTEXT", "zk-2", DestinationType::ArkAddress),
                ],
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .withf(|value, _| value == "BTC-CIPHERTEXT")
            .returning(|_, _| Ok("1A1zP...".to_string()));
        // No expectation for ARK-CIPHERTEXT decrypt with any key derived from unrelated values --
        // additional_hash_values is empty here since no ark value appears in the QR text itself.
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let qr = "bitcoin:1A1zP...?branta_id=onchain-id&branta_secret=onchain-secret";
        let result = svc.get_payments_by_qr_code(qr, None).await.unwrap();
        assert!(result.payments[0].destinations[1].is_encrypted);
        assert_eq!(result.payments[0].destinations[1].value, "ARK-CIPHERTEXT");
    }

    // ---- add_payment ----

    #[tokio::test]
    async fn add_payment_strict_mode_all_non_zk_errors() {
        let client = MockBrantaClientTrait::new();
        let aes = MockAesEncryptionTrait::new();
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(strict_options(), client, aes, secret_gen);

        let payment = Payment {
            destinations: vec![Destination::new(
                "addr",
                Some(DestinationType::BitcoinAddress),
            )],
            ..Default::default()
        };
        let result = svc.add_payment(payment, None).await;
        assert!(matches!(
            result,
            Err(BrantaError::NonZkDestinationInStrictMode)
        ));
    }

    #[tokio::test]
    async fn add_payment_strict_mode_mixed_zk_and_non_zk_errors() {
        let client = MockBrantaClientTrait::new();
        let aes = MockAesEncryptionTrait::new();
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(strict_options(), client, aes, secret_gen);

        let payment = Payment {
            destinations: vec![
                zk_destination("addr1", "zk-1", DestinationType::BitcoinAddress),
                Destination::new("addr2", Some(DestinationType::BitcoinAddress)),
            ],
            ..Default::default()
        };
        let result = svc.add_payment(payment, None).await;
        assert!(matches!(
            result,
            Err(BrantaError::NonZkDestinationInStrictMode)
        ));
    }

    #[tokio::test]
    async fn add_payment_plain_destination_untouched_in_loose_mode() {
        let mut client = MockBrantaClientTrait::new();
        client
            .expect_post_payment()
            .returning(|payment, _| Ok(Some(payment)));
        let aes = MockAesEncryptionTrait::new(); // encrypt must never be called
        let mut secret_gen = MockSecretGeneratorTrait::new();
        secret_gen
            .expect_generate()
            .returning(|| "generated-secret".to_string());
        let svc = service(loose_options(), client, aes, secret_gen);

        let payment = Payment {
            destinations: vec![Destination::new(
                "addr",
                Some(DestinationType::BitcoinAddress),
            )],
            ..Default::default()
        };
        let result = svc.add_payment(payment, None).await.unwrap();
        assert_eq!(result.payment.destinations[0].value, "addr");
    }

    #[tokio::test]
    async fn add_payment_zk_bitcoin_encrypts_with_shared_secret() {
        let mut client = MockBrantaClientTrait::new();
        client
            .expect_post_payment()
            .returning(|payment, _| Ok(Some(payment)));
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_encrypt()
            .withf(|value, secret, _| value == "addr" && secret == "the-secret")
            .returning(|_, _, _| Ok("CIPHERTEXT".to_string()));
        let mut secret_gen = MockSecretGeneratorTrait::new();
        secret_gen
            .expect_generate()
            .returning(|| "the-secret".to_string());
        secret_gen.expect_deterministic_nonce().returning(|| false);
        let svc = service(loose_options(), client, aes, secret_gen);

        let payment = Payment {
            destinations: vec![zk_destination(
                "addr",
                "zk-1",
                DestinationType::BitcoinAddress,
            )],
            ..Default::default()
        };
        let result = svc.add_payment(payment, None).await.unwrap();
        assert_eq!(result.secret, "the-secret");
        assert_eq!(result.payment.destinations[0].value, "CIPHERTEXT");
    }

    #[tokio::test]
    async fn add_payment_zk_hash_type_encrypts_with_hash_derived_key() {
        let mut client = MockBrantaClientTrait::new();
        client
            .expect_post_payment()
            .returning(|payment, _| Ok(Some(payment)));
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_encrypt()
            .withf(|value, key, deterministic| {
                value == "lnbc1qsomething"
                    && key == to_normalized_hash("lnbc1qsomething")
                    && *deterministic
            })
            .returning(|_, _, _| Ok("CIPHERTEXT".to_string()));
        let mut secret_gen = MockSecretGeneratorTrait::new();
        secret_gen
            .expect_generate()
            .returning(|| "unused-secret".to_string());
        let svc = service(loose_options(), client, aes, secret_gen);

        let payment = Payment {
            destinations: vec![zk_destination(
                "lnbc1qsomething",
                "zk-1",
                DestinationType::Bolt11,
            )],
            ..Default::default()
        };
        let result = svc.add_payment(payment, None).await.unwrap();
        assert_eq!(result.payment.destinations[0].value, "CIPHERTEXT");
    }

    #[tokio::test]
    async fn add_payment_unsupported_zk_type_errors() {
        let client = MockBrantaClientTrait::new();
        let aes = MockAesEncryptionTrait::new();
        let mut secret_gen = MockSecretGeneratorTrait::new();
        secret_gen
            .expect_generate()
            .returning(|| "secret".to_string());
        let svc = service(loose_options(), client, aes, secret_gen);

        // "not-a-known-format" matches no hash-ZK type and isn't BitcoinAddress.
        let payment = Payment {
            destinations: vec![zk_destination(
                "not-a-known-format",
                "zk-1",
                DestinationType::LnAddress,
            )],
            ..Default::default()
        };
        let result = svc.add_payment(payment, None).await;
        assert!(matches!(
            result,
            Err(BrantaError::UnsupportedZkDestinationType(_))
        ));
    }

    #[tokio::test]
    async fn add_payment_no_payment_returned_errors() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_post_payment().returning(|_, _| Ok(None));
        let aes = MockAesEncryptionTrait::new();
        let mut secret_gen = MockSecretGeneratorTrait::new();
        secret_gen
            .expect_generate()
            .returning(|| "secret".to_string());
        let svc = service(loose_options(), client, aes, secret_gen);

        let payment = Payment {
            destinations: vec![Destination::new(
                "addr",
                Some(DestinationType::BitcoinAddress),
            )],
            ..Default::default()
        };
        let result = svc.add_payment(payment, None).await;
        assert!(matches!(result, Err(BrantaError::NoPaymentReturned)));
    }

    // ---- add_payment: metadata / DEK envelope ----

    #[tokio::test]
    async fn add_payment_with_metadata_and_zk_destination_sets_encrypted_dek() {
        let mut client = MockBrantaClientTrait::new();
        client
            .expect_post_payment()
            .returning(|payment, _| Ok(Some(payment)));
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_encrypt()
            .withf(|value, secret, det| value == "{\"a\":\"b\"}" && secret == "dek-value" && !det)
            .returning(|_, _, _| Ok("ENCRYPTED-METADATA".to_string()));
        aes.expect_encrypt()
            .withf(|value, secret, _| value == "addr" && secret == "shared-secret")
            .returning(|_, _, _| Ok("CIPHERTEXT".to_string()));
        aes.expect_encrypt()
            .withf(|value, secret, det| value == "dek-value" && secret == "shared-secret" && !det)
            .returning(|_, _, _| Ok("ENCRYPTED-DEK".to_string()));
        let mut secret_gen = MockSecretGeneratorTrait::new();
        let mut call_count = 0;
        secret_gen.expect_generate().returning(move || {
            call_count += 1;
            if call_count == 1 {
                "dek-value".to_string()
            } else {
                "shared-secret".to_string()
            }
        });
        secret_gen.expect_deterministic_nonce().returning(|| false);
        let svc = service(loose_options(), client, aes, secret_gen);

        let payment = Payment {
            destinations: vec![zk_destination(
                "addr",
                "zk-1",
                DestinationType::BitcoinAddress,
            )],
            metadata: Some("{\"a\":\"b\"}".to_string()),
            ..Default::default()
        };
        let result = svc.add_payment(payment, None).await.unwrap();
        assert_eq!(
            result.payment.metadata.as_deref(),
            Some("ENCRYPTED-METADATA")
        );
        assert_eq!(
            result.payment.destinations[0].encrypted_dek.as_deref(),
            Some("ENCRYPTED-DEK")
        );
    }

    #[tokio::test]
    async fn add_payment_with_metadata_but_no_zk_destination_does_not_encrypt_metadata() {
        let mut client = MockBrantaClientTrait::new();
        client
            .expect_post_payment()
            .returning(|payment, _| Ok(Some(payment)));
        let aes = MockAesEncryptionTrait::new(); // encrypt must never be called
        let mut secret_gen = MockSecretGeneratorTrait::new();
        secret_gen
            .expect_generate()
            .returning(|| "secret".to_string());
        let svc = service(loose_options(), client, aes, secret_gen);

        let payment = Payment {
            destinations: vec![Destination::new(
                "addr",
                Some(DestinationType::BitcoinAddress),
            )],
            metadata: Some("plain-metadata".to_string()),
            ..Default::default()
        };
        let result = svc.add_payment(payment, None).await.unwrap();
        assert_eq!(result.payment.metadata.as_deref(), Some("plain-metadata"));
        assert!(result.payment.destinations[0].encrypted_dek.is_none());
    }

    #[tokio::test]
    async fn get_payments_decrypts_metadata_when_encrypted_dek_present() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![Destination {
                    encrypted_dek: Some("ENCRYPTED-DEK".to_string()),
                    ..zk_destination("CIPHERTEXT", "zk-1", DestinationType::BitcoinAddress)
                }],
                metadata: Some("ENCRYPTED-METADATA".to_string()),
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .withf(|value, key| value == "CIPHERTEXT" && key == "the-key")
            .returning(|_, _| Ok("plain-address".to_string()));
        aes.expect_decrypt()
            .withf(|value, key| value == "ENCRYPTED-DEK" && key == "the-key")
            .returning(|_, _| Ok("dek-value".to_string()));
        aes.expect_decrypt()
            .withf(|value, key| value == "ENCRYPTED-METADATA" && key == "dek-value")
            .returning(|_, _| Ok("plain-metadata".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let result = svc
            .get_payments("CIPHERTEXT", Some("the-key"), None)
            .await
            .unwrap();
        assert_eq!(
            result.payments[0].metadata.as_deref(),
            Some("plain-metadata")
        );
        assert!(result.payments[0].is_metadata_decrypted);
    }

    #[tokio::test]
    async fn get_payments_metadata_decrypt_failure_leaves_metadata_untouched() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![Destination {
                    encrypted_dek: Some("ENCRYPTED-DEK".to_string()),
                    ..zk_destination("CIPHERTEXT", "zk-1", DestinationType::BitcoinAddress)
                }],
                metadata: Some("ENCRYPTED-METADATA".to_string()),
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .withf(|value, _| value == "CIPHERTEXT")
            .returning(|_, _| Ok("plain-address".to_string()));
        aes.expect_decrypt()
            .withf(|value, _| value == "ENCRYPTED-DEK")
            .returning(|_, _| Err(BrantaError::DecryptionFailed("bad dek".into())));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let result = svc
            .get_payments("CIPHERTEXT", Some("the-key"), None)
            .await
            .unwrap();
        assert_eq!(
            result.payments[0].metadata.as_deref(),
            Some("ENCRYPTED-METADATA")
        );
        assert!(!result.payments[0].is_metadata_decrypted);
    }

    #[tokio::test]
    async fn get_payments_decrypts_metadata_only_once_per_payment() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![
                    Destination {
                        encrypted_dek: Some("DEK-1".to_string()),
                        ..zk_destination("CIPHER-1", "zk-1", DestinationType::BitcoinAddress)
                    },
                    Destination {
                        encrypted_dek: Some("DEK-2".to_string()),
                        ..zk_destination("CIPHER-1", "zk-2", DestinationType::BitcoinAddress)
                    },
                ],
                metadata: Some("ENCRYPTED-METADATA".to_string()),
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .withf(|v, _| v == "CIPHER-1")
            .returning(|_, _| Ok("plain".to_string()));
        // Only DEK-1 should ever be decrypted -- the second destination's decrypt succeeds too,
        // but try_decrypt_metadata must no-op once is_metadata_decrypted is already true.
        aes.expect_decrypt()
            .withf(|v, _| v == "DEK-1")
            .times(1)
            .returning(|_, _| Ok("dek".to_string()));
        aes.expect_decrypt()
            .withf(|v, _| v == "ENCRYPTED-METADATA")
            .times(1)
            .returning(|_, _| Ok("decrypted".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let result = svc
            .get_payments("CIPHER-1", Some("shared-key"), None)
            .await
            .unwrap();
        assert_eq!(result.payments[0].metadata.as_deref(), Some("decrypted"));
    }

    // ---- get_payments_by_qr_code: address binding ----

    const SWAPPED_ADDRESS: &str = "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2";
    const BECH32_ADDRESS: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";

    #[tokio::test]
    async fn qr_on_chain_zk_swapped_address_rejects() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![zk_destination(
                    "CIPHERTEXT",
                    "zk-1",
                    DestinationType::BitcoinAddress,
                )],
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .returning(|_, _| Ok("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let qr =
            format!("bitcoin:{SWAPPED_ADDRESS}?branta_id=onchain-id&branta_secret=onchain-secret");
        let result = svc.get_payments_by_qr_code(&qr, None).await;
        assert!(matches!(result, Err(BrantaError::Tampered)));
    }

    #[tokio::test]
    async fn qr_on_chain_zk_matching_address_does_not_error() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![zk_destination(
                    "CIPHERTEXT",
                    "zk-1",
                    DestinationType::BitcoinAddress,
                )],
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .returning(|_, _| Ok("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let qr =
            "bitcoin:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa?branta_id=onchain-id&branta_secret=onchain-secret";
        let result = svc.get_payments_by_qr_code(qr, None).await.unwrap();
        assert_eq!(
            result.payments[0].destinations[0].value,
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"
        );
    }

    #[tokio::test]
    async fn qr_uppercase_bech32_matches_lowercase_registered_address() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![zk_destination(
                    "CIPHERTEXT",
                    "zk-1",
                    DestinationType::BitcoinAddress,
                )],
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .returning(|_, _| Ok(BECH32_ADDRESS.to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let qr = format!(
            "bitcoin:{}?branta_id=onchain-id&branta_secret=onchain-secret",
            BECH32_ADDRESS.to_uppercase()
        );
        let result = svc.get_payments_by_qr_code(&qr, None).await.unwrap();
        assert_eq!(result.payments[0].destinations[0].value, BECH32_ADDRESS);
    }

    #[tokio::test]
    async fn qr_base58_case_mismatch_rejects() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![zk_destination(
                    "CIPHERTEXT",
                    "zk-1",
                    DestinationType::BitcoinAddress,
                )],
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .returning(|_, _| Ok("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let qr = "bitcoin:1a1zp1ep5qgefi2dmptftl5slmv7divfna?branta_id=onchain-id&branta_secret=onchain-secret";
        let result = svc.get_payments_by_qr_code(qr, None).await;
        assert!(matches!(result, Err(BrantaError::Tampered)));
    }

    #[tokio::test]
    async fn qr_lightning_with_zk_params_no_plain_address_decrypts_without_comparison() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![zk_destination(
                    "CIPHERTEXT",
                    "zk-1",
                    DestinationType::BitcoinAddress,
                )],
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .returning(|_, _| Ok("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        // "lightning:" scheme carries no plaintext on-chain address, so nothing to compare
        // against even though branta_id/branta_secret are present.
        let qr = "lightning:lnbc1qsomething?branta_id=onchain-id&branta_secret=onchain-secret";
        let result = svc.get_payments_by_qr_code(qr, None).await.unwrap();
        assert_eq!(
            result.payments[0].destinations[0].value,
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"
        );
    }

    #[tokio::test]
    async fn qr_combined_zk_swapped_address_rejects() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_get_payments().returning(|_, _| {
            Ok(vec![Payment {
                destinations: vec![
                    zk_destination("BTC-CIPHERTEXT", "zk-1", DestinationType::BitcoinAddress),
                    zk_destination("LN-CIPHERTEXT", "zk-2", DestinationType::Bolt11),
                ],
                ..Default::default()
            }])
        });
        let mut aes = MockAesEncryptionTrait::new();
        aes.expect_decrypt()
            .withf(|value, _| value == "BTC-CIPHERTEXT")
            .returning(|_, _| Ok("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".to_string()));
        let secret_gen = MockSecretGeneratorTrait::new();
        let svc = service(loose_options(), client, aes, secret_gen);

        let qr = format!(
            "bitcoin:{SWAPPED_ADDRESS}?branta_id=onchain-id&branta_secret=onchain-secret&lightning=lnbc1qsomething"
        );
        let result = svc.get_payments_by_qr_code(&qr, None).await;
        assert!(matches!(result, Err(BrantaError::Tampered)));
    }

    // ---- is_api_key_valid ----

    #[tokio::test]
    async fn is_api_key_valid_passes_through_true() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_is_api_key_valid().returning(|_| Ok(true));
        let svc = service(
            loose_options(),
            client,
            MockAesEncryptionTrait::new(),
            MockSecretGeneratorTrait::new(),
        );
        assert!(svc.is_api_key_valid(None).await.unwrap());
    }

    #[tokio::test]
    async fn is_api_key_valid_passes_through_false() {
        let mut client = MockBrantaClientTrait::new();
        client.expect_is_api_key_valid().returning(|_| Ok(false));
        let svc = service(
            loose_options(),
            client,
            MockAesEncryptionTrait::new(),
            MockSecretGeneratorTrait::new(),
        );
        assert!(!svc.is_api_key_valid(None).await.unwrap());
    }
}
