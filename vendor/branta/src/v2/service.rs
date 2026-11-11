//! Encrypt and decrypt around the HTTP client.
//!
//! `verify` returns a status. A swapped address or a derived-key plaintext that
//! is not the scanned value is [`VerifyStatus::Tampered`]. A 404 is
//! [`VerifyStatus::Unknown`]. Any other non-OK response, and a network failure,
//! is [`VerifyStatus::Incomplete`]. A logo URL on the wrong origin still returns
//! [`BrantaError::LogoUrlDomainMismatch`].
//!
//! `add_payment` encrypts a copy. A failed post leaves the caller's [`Payment`] plaintext.

use indexmap::IndexMap;
use percent_encoding::utf8_percent_encode;
use uuid::Uuid;

use crate::enums::{DestinationType, VerifyStatus};
use crate::error::BrantaError;
use crate::extensions::{derived_key_type, to_normalized_hash, to_url_fragment};
use crate::models::{AddPaymentResult, Payment, VerifyResult};
use crate::options::BrantaApiOptions;

use super::client::{BrantaClient, BrantaClientTrait, PATH_SEGMENT};
use super::encryption::{AesEncryptionService, AesEncryptionTrait};
use super::parser::QrParser;
use super::secret_generator::{GuidSecretGenerator, SecretGeneratorTrait};

enum LookupFail {
    Tampered,
    Incomplete,
    Other(BrantaError),
}

fn addresses_match(a: &str, b: &str) -> bool {
    fn is_bech32(value: &str) -> bool {
        let lower = value.to_ascii_lowercase();
        lower.starts_with("bc1") || lower.starts_with("tb1") || lower.starts_with("bcrt1")
    }
    if is_bech32(a) && is_bech32(b) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Every on-chain address in the QR must agree. `None` means the QR has none.
fn uniform_on_chain_address(parser: &QrParser) -> Result<Option<String>, LookupFail> {
    let mut addresses = parser.destinations.iter().filter(|destination| {
        destination.r#type == Some(DestinationType::BitcoinAddress)
            && !destination.value.trim().is_empty()
    });
    let Some(first) = addresses.next() else {
        return Ok(None);
    };
    if addresses.any(|destination| !addresses_match(&first.value, &destination.value)) {
        return Err(LookupFail::Tampered);
    }
    Ok(Some(first.value.clone()))
}

fn to_verify_result(payments: Vec<Payment>, verify_url: String) -> VerifyResult {
    if payments.is_empty() {
        VerifyResult {
            status: VerifyStatus::Unknown,
            payments,
            verify_url,
        }
    } else {
        VerifyResult {
            status: VerifyStatus::Match,
            payments,
            verify_url,
        }
    }
}

fn lookup_error(err: BrantaError) -> LookupFail {
    match err {
        BrantaError::Incomplete => LookupFail::Incomplete,
        other => LookupFail::Other(other),
    }
}

pub struct BrantaService {
    default_options: BrantaApiOptions,
    client: Box<dyn BrantaClientTrait>,
    aes: Box<dyn AesEncryptionTrait>,
    secret_generator: Box<dyn SecretGeneratorTrait>,
}

impl BrantaService {
    /// Production constructor: real HTTP client, real AES-GCM, real UUID generator.
    pub fn new(default_options: BrantaApiOptions) -> Self {
        Self::with_deps(
            default_options.clone(),
            Box::new(BrantaClient::new(default_options)),
            Box::new(AesEncryptionService),
            Box::new(GuidSecretGenerator),
        )
    }

    /// Same as [`Self::new`], attaching the integration-metrics token live tests send.
    /// An empty token sends no header.
    pub fn new_with_token(default_options: BrantaApiOptions, token: Option<String>) -> Self {
        let mut client = BrantaClient::new(default_options.clone());
        if let Some(token) = token.filter(|value| !value.is_empty()) {
            client = client.with_integration_metrics_token(token);
        }
        Self::with_deps(
            default_options,
            Box::new(client),
            Box::new(AesEncryptionService),
            Box::new(GuidSecretGenerator),
        )
    }

    /// Inject a client, AES implementation, and secret generator.
    pub fn with_deps(
        default_options: BrantaApiOptions,
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

    /// Parses a scanned QR or pasted text and looks it up.
    pub async fn verify(
        &self,
        input: &str,
        options: Option<&BrantaApiOptions>,
    ) -> Result<VerifyResult, BrantaError> {
        let parser = QrParser::new(input);
        let lookup = self.scanned_lookup(&parser);
        match self.verify_parsed(&parser, options).await {
            Ok(result) => Ok(result),
            Err(LookupFail::Tampered) => Ok(VerifyResult {
                status: VerifyStatus::Tampered,
                payments: Vec::new(),
                verify_url: self.build_verify_url(options, &lookup, &IndexMap::new()),
            }),
            Err(LookupFail::Incomplete) => Ok(VerifyResult {
                status: VerifyStatus::Incomplete,
                payments: Vec::new(),
                verify_url: self.build_verify_url(options, &lookup, &IndexMap::new()),
            }),
            Err(LookupFail::Other(err)) => Err(err),
        }
    }

    fn scanned_lookup(&self, parser: &QrParser) -> String {
        if parser.on_chain_encryption_text.is_empty() {
            parser.destination().unwrap_or("").to_string()
        } else {
            parser.on_chain_encryption_text.clone()
        }
    }

    async fn verify_parsed(
        &self,
        parser: &QrParser,
        options: Option<&BrantaApiOptions>,
    ) -> Result<VerifyResult, LookupFail> {
        let expected_on_chain = uniform_on_chain_address(parser)?;
        let lookup = self.scanned_lookup(parser);

        if parser.has_unverifiable_instruction {
            return Ok(VerifyResult {
                status: VerifyStatus::Unknown,
                payments: Vec::new(),
                verify_url: self.build_verify_url(options, &lookup, &IndexMap::new()),
            });
        }

        if parser.is_on_chain_zk() {
            let additional: Vec<String> = parser
                .destinations
                .iter()
                .filter(|destination| derived_key_type(&destination.value).is_some())
                .map(|destination| destination.value.clone())
                .collect();
            if expected_on_chain.is_none() && additional.is_empty() {
                return Ok(VerifyResult {
                    status: VerifyStatus::Unknown,
                    payments: Vec::new(),
                    verify_url: self.build_verify_url(options, &lookup, &IndexMap::new()),
                });
            }
            let (payments, verify_url) = self
                .lookup_by_branta_id(
                    &parser.on_chain_encryption_text,
                    &parser.on_chain_encryption_secret,
                    &additional,
                    expected_on_chain.as_deref(),
                    options,
                )
                .await?;
            return Ok(to_verify_result(payments, verify_url));
        }

        let destination = parser.destination().unwrap_or("");
        if derived_key_type(destination).is_none() {
            return Ok(VerifyResult {
                status: VerifyStatus::Unknown,
                payments: Vec::new(),
                verify_url: self.build_verify_url(options, destination, &IndexMap::new()),
            });
        }

        let (payments, verify_url) = self.lookup_by_derived_key(destination, options).await?;
        Ok(to_verify_result(payments, verify_url))
    }

    async fn lookup_by_branta_id(
        &self,
        lookup_value: &str,
        encryption_key: &str,
        additional_hash_values: &[String],
        expected_on_chain_address: Option<&str>,
        options: Option<&BrantaApiOptions>,
    ) -> Result<(Vec<Payment>, String), LookupFail> {
        let mut payments = self
            .client
            .get_payments(lookup_value, options)
            .await
            .map_err(lookup_error)?;

        let mut keys = IndexMap::new();
        let mut bound = Vec::new();
        for payment in &mut payments {
            let on_chain_bound = self
                .decrypt_destinations(
                    payment,
                    encryption_key,
                    &mut keys,
                    expected_on_chain_address,
                )
                .await?;
            let mut hashes_bound = true;
            for value in additional_hash_values {
                if !self
                    .decrypt_derived_key_destinations(payment, value, &mut keys)
                    .await?
                {
                    hashes_bound = false;
                }
            }
            let has_payable =
                expected_on_chain_address.is_some() || !additional_hash_values.is_empty();
            if has_payable && on_chain_bound && hashes_bound {
                bound.push(payment.clone());
            }
        }
        Ok((bound, self.build_verify_url(options, lookup_value, &keys)))
    }

    async fn lookup_by_derived_key(
        &self,
        destination_value: &str,
        options: Option<&BrantaApiOptions>,
    ) -> Result<(Vec<Payment>, String), LookupFail> {
        let normalized = destination_value.to_lowercase();
        let lookup_value = self
            .aes
            .encrypt(&normalized, &to_normalized_hash(&normalized), true)
            .await
            .map_err(LookupFail::Other)?;

        let mut payments = self
            .client
            .get_payments(&lookup_value, options)
            .await
            .map_err(lookup_error)?;

        let mut keys = IndexMap::new();
        let mut bound = Vec::new();
        for payment in &mut payments {
            if self
                .decrypt_derived_key_destinations(payment, &normalized, &mut keys)
                .await?
            {
                bound.push(payment.clone());
            }
        }
        Ok((bound, self.build_verify_url(options, &lookup_value, &keys)))
    }

    /// Encrypts a copy of `payment` and posts that copy.
    pub async fn add_payment(
        &self,
        payment: &Payment,
        options: Option<&BrantaApiOptions>,
    ) -> Result<AddPaymentResult, BrantaError> {
        let mut submitted = payment.clone();
        fold_description_into_metadata(&mut submitted);

        let mut dek = String::new();
        if submitted
            .metadata
            .as_deref()
            .is_some_and(|value| !value.is_empty())
            && !submitted.destinations.is_empty()
        {
            dek = self.secret_generator.generate();
            submitted.metadata = Some(
                self.aes
                    .encrypt(submitted.metadata.as_deref().unwrap_or(""), &dek, false)
                    .await?,
            );
        }

        let secret = self.secret_generator.generate();
        let mut encrypted_to_key: IndexMap<String, String> = IndexMap::new();

        for destination in &mut submitted.destinations {
            if destination.id.is_none() {
                destination.id = Some(Uuid::new_v4().to_string());
            }
            if destination.r#type.is_none() {
                destination.r#type = super::parser::detect_plain_text_type(&destination.value);
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
                if !dek.is_empty() {
                    destination.encrypted_dek = Some(self.aes.encrypt(&dek, &secret, false).await?);
                }
                continue;
            }

            let Some(derived) = derived_key_type(&destination.value) else {
                return Err(BrantaError::UnsupportedDestinationType(
                    destination_type_name(destination.r#type),
                ));
            };
            if destination.r#type.is_none() {
                destination.r#type = Some(derived);
            }
            let normalized = destination.value.to_lowercase();
            let key = to_normalized_hash(&normalized);
            let ciphertext = self.aes.encrypt(&normalized, &key, true).await?;
            destination.value = ciphertext.clone();
            encrypted_to_key.insert(ciphertext, key.clone());
            if !dek.is_empty() {
                destination.encrypted_dek = Some(self.aes.encrypt(&dek, &key, false).await?);
            }
        }

        let primary_value = submitted
            .destinations
            .first()
            .map(|destination| destination.value.clone())
            .unwrap_or_default();

        let response = self
            .client
            .post_payment(submitted, options)
            .await?
            .ok_or(BrantaError::NoPaymentReturned)?;

        let mut keys = IndexMap::new();
        for destination in &response.destinations {
            let Some(id) = destination.id.clone() else {
                continue;
            };
            if let Some(key) = encrypted_to_key.get(&destination.value) {
                keys.entry(id).or_insert_with(|| key.clone());
            }
        }

        Ok(AddPaymentResult {
            verify_url: self.build_verify_url(options, &primary_value, &keys),
            payment: response,
            secret,
        })
    }

    pub async fn is_api_key_valid(
        &self,
        options: Option<&BrantaApiOptions>,
    ) -> Result<bool, BrantaError> {
        self.client.is_api_key_valid(options).await
    }

    async fn decrypt_destinations(
        &self,
        payment: &mut Payment,
        encryption_key: &str,
        keys: &mut IndexMap<String, String>,
        expected_on_chain_address: Option<&str>,
    ) -> Result<bool, LookupFail> {
        let mut on_chain_bound = expected_on_chain_address.is_none();
        for index in 0..payment.destinations.len() {
            if !payment.destinations[index].is_encrypted
                || payment.destinations[index].r#type != Some(DestinationType::BitcoinAddress)
            {
                continue;
            }
            if encryption_key.is_empty() {
                continue;
            }
            let Ok(plaintext) = self
                .aes
                .decrypt(&payment.destinations[index].value, encryption_key)
                .await
            else {
                continue;
            };
            if expected_on_chain_address
                .is_some_and(|expected| !addresses_match(&plaintext, expected))
            {
                return Err(LookupFail::Tampered);
            }
            if expected_on_chain_address.is_some() {
                on_chain_bound = true;
            }
            payment.destinations[index].value = plaintext;
            payment.destinations[index].is_encrypted = false;
            if let Some(id) = payment.destinations[index].id.clone() {
                keys.entry(id).or_insert_with(|| encryption_key.to_string());
            }
            self.try_decrypt_metadata(payment, index, encryption_key)
                .await;
        }
        Ok(on_chain_bound)
    }

    /// Reports whether a destination of `plain_value`'s type decrypted to that value.
    /// A different plaintext is tampered for the whole call.
    async fn decrypt_derived_key_destinations(
        &self,
        payment: &mut Payment,
        plain_value: &str,
        keys: &mut IndexMap<String, String>,
    ) -> Result<bool, LookupFail> {
        let Some(derived) = derived_key_type(plain_value) else {
            return Ok(false);
        };
        let key = to_normalized_hash(plain_value);
        let mut bound = false;
        for index in 0..payment.destinations.len() {
            if !payment.destinations[index].is_encrypted
                || payment.destinations[index].r#type != Some(derived)
            {
                continue;
            }
            let Ok(plaintext) = self
                .aes
                .decrypt(&payment.destinations[index].value, &key)
                .await
            else {
                continue;
            };
            if !plaintext.eq_ignore_ascii_case(plain_value) {
                return Err(LookupFail::Tampered);
            }
            payment.destinations[index].value = plaintext;
            payment.destinations[index].is_encrypted = false;
            if let Some(id) = payment.destinations[index].id.clone() {
                keys.entry(id).or_insert_with(|| key.clone());
            }
            self.try_decrypt_metadata(payment, index, &key).await;
            bound = true;
        }
        Ok(bound)
    }

    async fn try_decrypt_metadata(&self, payment: &mut Payment, index: usize, key_used: &str) {
        if payment.is_metadata_decrypted {
            return;
        }
        let Some(encrypted_dek) = payment.destinations[index]
            .encrypted_dek
            .clone()
            .filter(|value| !value.is_empty())
        else {
            return;
        };
        let Some(metadata) = payment.metadata.clone().filter(|value| !value.is_empty()) else {
            return;
        };
        let Ok(dek) = self.aes.decrypt(&encrypted_dek, key_used).await else {
            return;
        };
        let Ok(plaintext) = self.aes.decrypt(&metadata, &dek).await else {
            return;
        };
        payment.metadata = Some(plaintext);
        payment.is_metadata_decrypted = true;
        lift_description_from_metadata(payment);
    }

    fn build_verify_url(
        &self,
        options: Option<&BrantaApiOptions>,
        payment_lookup: &str,
        keys: &IndexMap<String, String>,
    ) -> String {
        let base = self
            .default_options
            .get_base_url(options)
            .trim_end_matches('/');
        let encoded = utf8_percent_encode(payment_lookup, PATH_SEGMENT);
        let mut url = format!("{base}/v2/verify/{encoded}");
        url.push_str(&to_url_fragment(keys));
        url
    }
}

fn destination_type_name(r#type: Option<DestinationType>) -> String {
    match r#type {
        Some(DestinationType::BitcoinAddress) => "bitcoin_address",
        Some(DestinationType::Bolt11) => "bolt11",
        Some(DestinationType::ArkAddress) => "ark_address",
        Some(DestinationType::SilentPayment) => "silent_payment",
        None => "unknown",
    }
    .to_string()
}

fn fold_description_into_metadata(payment: &mut Payment) {
    let Some(description) = payment
        .description
        .clone()
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let mut metadata_map: serde_json::Map<String, serde_json::Value> = payment
        .metadata
        .as_deref()
        .filter(|value| !value.is_empty())
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default();
    metadata_map
        .entry("description")
        .or_insert_with(|| serde_json::Value::String(description));
    if let Ok(encoded) = serde_json::to_string(&metadata_map) {
        payment.metadata = Some(encoded);
        payment.description = None;
    }
}

fn lift_description_from_metadata(payment: &mut Payment) {
    let Some(metadata) = payment.metadata.as_deref() else {
        return;
    };
    let Ok(metadata_map) =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(metadata)
    else {
        return;
    };
    if let Some(description) = metadata_map
        .get("description")
        .and_then(|value| value.as_str())
    {
        payment.description = Some(description.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::BrantaEnvironment;
    use crate::models::Destination;
    use async_trait::async_trait;
    use std::sync::Mutex;

    use super::super::encryption::encrypt;

    struct ScriptedClient {
        payments: Mutex<Result<Vec<Payment>, bool>>,
    }

    impl ScriptedClient {
        fn payments(payments: Vec<Payment>) -> Self {
            Self {
                payments: Mutex::new(Ok(payments)),
            }
        }

        fn incomplete() -> Self {
            Self {
                payments: Mutex::new(Err(true)),
            }
        }
    }

    #[async_trait]
    impl BrantaClientTrait for ScriptedClient {
        async fn get_payments<'a>(
            &'a self,
            _destination_value: &'a str,
            _options: Option<&'a BrantaApiOptions>,
        ) -> Result<Vec<Payment>, BrantaError> {
            match &*self.payments.lock().unwrap() {
                Ok(payments) => Ok(payments.clone()),
                Err(_) => Err(BrantaError::Incomplete),
            }
        }

        async fn post_payment<'a>(
            &'a self,
            payment: Payment,
            _options: Option<&'a BrantaApiOptions>,
        ) -> Result<Option<Payment>, BrantaError> {
            let _ = payment;
            Ok(Some(Payment::default()))
        }

        async fn is_api_key_valid<'a>(
            &'a self,
            _options: Option<&'a BrantaApiOptions>,
        ) -> Result<bool, BrantaError> {
            Ok(true)
        }
    }

    struct FixedSecret;

    impl SecretGeneratorTrait for FixedSecret {
        fn generate(&self) -> String {
            "11111111-1111-4111-8111-111111111111".into()
        }

        fn deterministic_nonce(&self) -> bool {
            true
        }
    }

    fn service(client: ScriptedClient) -> BrantaService {
        BrantaService::with_deps(
            BrantaApiOptions::new(BrantaEnvironment::Staging),
            Box::new(client),
            Box::new(AesEncryptionService),
            Box::new(FixedSecret),
        )
    }

    fn bolt11_payment(plaintext: &str, key_source: &str) -> Payment {
        let key = to_normalized_hash(&key_source.to_lowercase());
        Payment {
            destinations: vec![Destination {
                value: encrypt(&plaintext.to_lowercase(), &key, true).unwrap(),
                is_primary: true,
                is_encrypted: true,
                r#type: Some(DestinationType::Bolt11),
                id: Some("dest-1".into()),
                encrypted_dek: None,
            }],
            ..Payment::default()
        }
    }

    #[tokio::test]
    async fn plain_address_is_unknown_without_a_lookup() {
        let svc = service(ScriptedClient::incomplete());
        let result = svc.verify("bc1qmerchant", None).await.unwrap();
        assert_eq!(result.status, VerifyStatus::Unknown);
        assert!(result.payments.is_empty());
        assert!(result
            .verify_url
            .starts_with("https://staging.guardrail.branta.pro/v2/verify/bc1qmerchant"));
        assert!(!result.verify_url.contains('#'));
    }

    #[tokio::test]
    async fn bolt11_match_decrypts() {
        let invoice = "lnbc10testinvoice";
        let svc = service(ScriptedClient::payments(vec![bolt11_payment(
            invoice, invoice,
        )]));
        let result = svc.verify(invoice, None).await.unwrap();
        assert_eq!(result.status, VerifyStatus::Match);
        assert_eq!(result.payments[0].destinations[0].value, invoice);
        assert!(result.verify_url.contains("#k-dest-1="));
    }

    #[tokio::test]
    async fn derived_key_plaintext_mismatch_is_tampered() {
        let scanned = "lnbc10realinvoice";
        let svc = service(ScriptedClient::payments(vec![bolt11_payment(
            "lnbc10evilinvoice",
            scanned,
        )]));
        let result = svc.verify(scanned, None).await.unwrap();
        assert_eq!(result.status, VerifyStatus::Tampered);
        assert!(result.payments.is_empty());
        assert!(!result.verify_url.contains('#'));
    }

    #[tokio::test]
    async fn server_error_is_incomplete() {
        let svc = service(ScriptedClient::incomplete());
        let result = svc.verify("lnbc10realinvoice", None).await.unwrap();
        assert_eq!(result.status, VerifyStatus::Incomplete);
        assert!(!result.verify_url.contains('#'));
    }

    #[tokio::test]
    async fn branta_params_without_a_payable_destination_do_not_get() {
        let svc = service(ScriptedClient::incomplete());
        let result = svc
            .verify("bitcoin:?branta_id=abc&branta_secret=def", None)
            .await
            .unwrap();
        assert_eq!(result.status, VerifyStatus::Unknown);
    }

    #[tokio::test]
    async fn conflicting_on_chain_addresses_are_tampered() {
        let svc = service(ScriptedClient::incomplete());
        let result = svc
            .verify("bitcoin:bc1qaaa?bc=bc1qbbb", None)
            .await
            .unwrap();
        assert_eq!(result.status, VerifyStatus::Tampered);
    }

    #[tokio::test]
    async fn add_payment_keeps_caller_plaintext_and_omits_unset_ttl() {
        let client = CaptureClient;
        let svc = BrantaService::with_deps(
            BrantaApiOptions::new(BrantaEnvironment::Staging),
            Box::new(client),
            Box::new(AesEncryptionService),
            Box::new(FixedSecret),
        );
        let payment = crate::PaymentBuilder::new()
            .set_description("hello")
            .add_destination("bc1qexample", Some(DestinationType::BitcoinAddress))
            .build();
        let result = svc.add_payment(&payment, None).await.unwrap();
        assert_eq!(payment.destinations[0].value, "bc1qexample");
        assert_eq!(payment.description.as_deref(), Some("hello"));
        assert_ne!(result.payment.destinations[0].value, "bc1qexample");
        assert!(result.verify_url.contains("#k-resp-id="));
        assert_eq!(result.secret, "11111111-1111-4111-8111-111111111111");
    }

    #[tokio::test]
    async fn unencryptable_destination_is_rejected() {
        let svc = service(ScriptedClient::payments(Vec::new()));
        let payment = crate::PaymentBuilder::new()
            .add_destination("lno1offer", None)
            .build();
        let err = svc.add_payment(&payment, None).await.unwrap_err();
        assert!(matches!(err, BrantaError::UnsupportedDestinationType(_)));
        assert_eq!(payment.destinations[0].value, "lno1offer");
    }

    struct CaptureClient;

    #[async_trait]
    impl BrantaClientTrait for CaptureClient {
        async fn get_payments<'a>(
            &'a self,
            _destination_value: &'a str,
            _options: Option<&'a BrantaApiOptions>,
        ) -> Result<Vec<Payment>, BrantaError> {
            Ok(Vec::new())
        }

        async fn post_payment<'a>(
            &'a self,
            payment: Payment,
            _options: Option<&'a BrantaApiOptions>,
        ) -> Result<Option<Payment>, BrantaError> {
            let json = serde_json::to_value(&payment).unwrap();
            assert!(json.get("ttl").is_none());
            assert!(json.get("description").is_none());
            let metadata = json["metadata"].as_str().unwrap();
            assert!(!metadata.contains("hello"));
            let mut response = payment;
            response.destinations[0].id = Some("resp-id".into());
            Ok(Some(response))
        }

        async fn is_api_key_valid<'a>(
            &'a self,
            _options: Option<&'a BrantaApiOptions>,
        ) -> Result<bool, BrantaError> {
            Ok(true)
        }
    }
}
