//! Raw HTTP layer. Consumers should never call this directly -- go through [`crate::BrantaService`].
//!
//! Transcribed from `Branta/V2/Services/BrantaClient.cs`. Historical note: dotnet/js/dart/python
//! previously had a bug in the logo-domain-check loop where an early `return` (instead of
//! `continue`) meant only the first payment in a GET response list was actually domain-checked;
//! this port never had that bug (it always used `continue`). All six SDKs were fixed to check
//! every payment, and every logo-bearing field, as of the 2026-09 logo-URL validation hardening
//! -- see `CLAUDE.md`.

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use sha2::Sha256;

use crate::error::BrantaError;
use crate::models::Payment;
use crate::options::BrantaApiOptions;

type HmacSha256 = Hmac<Sha256>;

/// Matches .NET's `Uri.EscapeDataString`: percent-encode everything except unreserved characters
/// (`ALPHA / DIGIT / "-" / "." / "_" / "~"`). Crucially this encodes `+`, `/`, and `=`, which
/// routinely appear in base64 ciphertext lookup values -- `application/x-www-form-urlencoded`
/// escaping (e.g. `url::form_urlencoded`) is the wrong tool here and would corrupt those values.
pub(crate) const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait BrantaClientTrait: Send + Sync {
    async fn get_payments<'a>(
        &'a self,
        destination_value: &'a str,
        options: Option<&'a BrantaApiOptions>,
    ) -> Result<Vec<Payment>, BrantaError>;

    async fn post_payment<'a>(
        &'a self,
        payment: Payment,
        options: Option<&'a BrantaApiOptions>,
    ) -> Result<Option<Payment>, BrantaError>;

    async fn is_api_key_valid<'a>(
        &'a self,
        options: Option<&'a BrantaApiOptions>,
    ) -> Result<bool, BrantaError>;
}

pub struct BrantaClient {
    http: reqwest::Client,
    default_options: BrantaApiOptions,
    /// Sent on payment lookups when set. Live integration tests pass
    /// `BRANTA_INTEGRATION_METRICS_TOKEN` so Guardrail can leave that traffic out of GET metrics.
    integration_metrics_token: Option<String>,
    /// Test-only escape hatch: `BrantaApiOptions::environment` is a fixed enum of real Branta
    /// servers, so integration tests against an ephemeral `wiremock` port need a way to point
    /// this client elsewhere without touching the enum or any production code path.
    #[cfg(test)]
    base_url_override: Option<String>,
}

impl BrantaClient {
    pub fn new(default_options: BrantaApiOptions) -> Self {
        Self {
            http: reqwest::Client::new(),
            default_options,
            integration_metrics_token: None,
            #[cfg(test)]
            base_url_override: None,
        }
    }

    /// Attach the secret live integration tests send so Guardrail can omit those lookups
    /// from GET metrics. An empty token sends no header.
    pub fn with_integration_metrics_token(mut self, token: impl Into<String>) -> Self {
        self.integration_metrics_token = Some(token.into());
        self
    }

    #[cfg(test)]
    fn with_base_url_override(mut self, base_url: impl Into<String>) -> Self {
        self.base_url_override = Some(base_url.into());
        self
    }

    fn base_url(&self, options: Option<&BrantaApiOptions>) -> String {
        #[cfg(test)]
        if let Some(ref override_url) = self.base_url_override {
            return override_url.clone();
        }
        self.default_options.get_base_url(options).to_string()
    }

    fn resolve_api_key<'a>(
        &'a self,
        options: Option<&'a BrantaApiOptions>,
    ) -> Result<&'a str, BrantaError> {
        self.default_options
            .get_api_key(options)
            .ok_or(BrantaError::Unauthorized)
    }

    fn hmac_headers(
        &self,
        base_url: &str,
        json: &str,
        options: Option<&BrantaApiOptions>,
    ) -> Option<(String, String)> {
        let hmac_secret = self.default_options.get_hmac_secret(options)?;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_secs();
        let message = format!(
            "POST|{}/v2/payments|{json}|{timestamp}",
            base_url.trim_end_matches('/')
        );

        let mut mac = HmacSha256::new_from_slice(hmac_secret.as_bytes())
            .expect("HMAC accepts keys of any length");
        mac.update(message.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());

        Some((signature, timestamp.to_string()))
    }

    fn verify_logo_urls(&self, base_url: &str, payments: &[Payment]) -> Result<(), BrantaError> {
        let Ok(base) = url::Url::parse(base_url) else {
            return Ok(());
        };
        let base_origin = base.origin();

        let check = |url: Option<&str>, field: &'static str| -> Result<(), BrantaError> {
            let Some(logo_url) = url.filter(|s| !s.is_empty()) else {
                return Ok(());
            };
            let matches = url::Url::parse(logo_url)
                .map(|u| u.origin() == base_origin)
                .unwrap_or(false);
            if matches {
                Ok(())
            } else {
                Err(BrantaError::LogoUrlDomainMismatch(field.to_string()))
            }
        };

        for payment in payments {
            check(payment.platform_logo_url.as_deref(), "platform_logo_url")?;
            check(
                payment.platform_logo_light_url.as_deref(),
                "platform_logo_light_url",
            )?;
            check(
                payment
                    .parent_platform
                    .as_ref()
                    .and_then(|p| p.logo_url.as_deref()),
                "parent_platform.logo_url",
            )?;
            check(
                payment
                    .parent_platform
                    .as_ref()
                    .and_then(|p| p.logo_light_url.as_deref()),
                "parent_platform.logo_light_url",
            )?;
            check(
                payment
                    .child_platform
                    .as_ref()
                    .and_then(|p| p.logo_url.as_deref()),
                "child_platform.logo_url",
            )?;
            check(
                payment
                    .child_platform
                    .as_ref()
                    .and_then(|p| p.logo_light_url.as_deref()),
                "child_platform.logo_light_url",
            )?;
        }
        Ok(())
    }
}

#[async_trait]
impl BrantaClientTrait for BrantaClient {
    async fn get_payments<'a>(
        &'a self,
        destination_value: &'a str,
        options: Option<&'a BrantaApiOptions>,
    ) -> Result<Vec<Payment>, BrantaError> {
        let base_url = self.base_url(options);
        let encoded = utf8_percent_encode(destination_value, PATH_SEGMENT).to_string();

        let mut request = self.http.get(format!("{base_url}/v2/payments/{encoded}"));
        if let Some(token) = self
            .integration_metrics_token
            .as_deref()
            .filter(|token| !token.is_empty())
        {
            request = request.header("X-Branta-Integration-Token", token);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(_) => return Err(BrantaError::Incomplete),
        };

        if response.status().as_u16() == 404 {
            return Ok(Vec::new());
        }
        if !response.status().is_success() {
            return Err(BrantaError::Incomplete);
        }

        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(_) => return Err(BrantaError::Incomplete),
        };
        if bytes.is_empty() {
            return Ok(Vec::new());
        }

        let payments: Vec<Payment> = serde_json::from_slice(&bytes)?;

        self.verify_logo_urls(&base_url, &payments)?;

        Ok(payments)
    }

    async fn post_payment<'a>(
        &'a self,
        payment: Payment,
        options: Option<&'a BrantaApiOptions>,
    ) -> Result<Option<Payment>, BrantaError> {
        let base_url = self.base_url(options);
        let api_key = self.resolve_api_key(options)?;
        let json = serde_json::to_string(&payment)?;

        let mut request = self
            .http
            .post(format!("{base_url}/v2/payments"))
            .bearer_auth(api_key)
            .header("Content-Type", "application/json; charset=utf-8")
            .body(json.clone());

        if let Some((signature, timestamp)) = self.hmac_headers(&base_url, &json, options) {
            request = request
                .header("X-HMAC-Signature", signature)
                .header("X-HMAC-Timestamp", timestamp);
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            return Err(BrantaError::RequestFailed {
                status: response.status().as_u16(),
            });
        }

        let text = response.text().await?;
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&text)?))
    }

    async fn is_api_key_valid<'a>(
        &'a self,
        options: Option<&'a BrantaApiOptions>,
    ) -> Result<bool, BrantaError> {
        let base_url = self.base_url(options);
        let api_key = self.resolve_api_key(options)?;

        let response = self
            .http
            .get(format!("{base_url}/v2/api-keys/health-check"))
            .bearer_auth(api_key)
            .send()
            .await?;

        Ok(response.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::BrantaEnvironment;
    use crate::models::Platform;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn options() -> BrantaApiOptions {
        BrantaApiOptions::new(BrantaEnvironment::Localhost)
    }

    fn client_for(server: &MockServer) -> BrantaClient {
        BrantaClient::new(options()).with_base_url_override(server.uri())
    }

    #[tokio::test]
    async fn get_payments_percent_encodes_path_segment_including_plus_slash_equals() {
        let server = MockServer::start().await;
        let ciphertext = "abc+def/ghi="; // characters that must be percent-encoded, not form-encoded
        let encoded = "abc%2Bdef%2Fghi%3D";

        Mock::given(method("GET"))
            .and(path(format!("/v2/payments/{encoded}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(Vec::<Payment>::new()))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server);
        let result = client.get_payments(ciphertext, None).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn get_payments_returns_empty_on_non_2xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = client_for(&server);
        let err = client.get_payments("value", None).await.unwrap_err();
        assert!(matches!(err, BrantaError::Incomplete));
    }

    #[tokio::test]
    async fn get_payments_returns_empty_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = client_for(&server);
        assert!(client.get_payments("value", None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_payments_returns_empty_on_empty_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = client_for(&server);
        assert_eq!(
            client.get_payments("value", None).await.unwrap(),
            Vec::new()
        );
    }

    #[tokio::test]
    async fn get_payments_returns_empty_on_malformed_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let client = client_for(&server);
        let err = client.get_payments("value", None).await.unwrap_err();
        assert!(matches!(err, BrantaError::Json(_)));
    }

    #[tokio::test]
    async fn get_payments_checks_every_payments_logo_not_just_the_first() {
        let server = MockServer::start().await;
        let payments = vec![
            Payment::default(), // no logo url -- must `continue`, not `return`
            Payment {
                platform_logo_url: Some("https://evil.example.com/logo.png".to_string()),
                ..Default::default()
            },
        ];
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(payments))
            .mount(&server)
            .await;

        let client = client_for(&server);
        let result = client.get_payments("value", None).await;
        assert!(matches!(result, Err(BrantaError::LogoUrlDomainMismatch(_))));
    }

    #[tokio::test]
    async fn get_payments_catches_mismatched_platform_logo_light_url() {
        let server = MockServer::start().await;
        let payments = vec![Payment {
            platform_logo_light_url: Some("https://evil.example.com/logo-light.png".to_string()),
            ..Default::default()
        }];
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(payments))
            .mount(&server)
            .await;

        let client = client_for(&server);
        let result = client.get_payments("value", None).await;
        assert!(matches!(result, Err(BrantaError::LogoUrlDomainMismatch(_))));
    }

    #[tokio::test]
    async fn get_payments_catches_mismatched_parent_platform_logo_url() {
        let server = MockServer::start().await;
        let body = serde_json::json!([
            { "parent_platform": { "logo_url": "https://evil.example.com/logo.png" } }
        ]);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = client_for(&server);
        let result = client.get_payments("value", None).await;
        assert!(matches!(result, Err(BrantaError::LogoUrlDomainMismatch(_))));
    }

    #[tokio::test]
    async fn get_payments_catches_mismatched_parent_platform_logo_light_url() {
        let server = MockServer::start().await;
        let body = serde_json::json!([
            { "parent_platform": { "logo_light_url": "https://evil.example.com/logo-light.png" } }
        ]);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = client_for(&server);
        let result = client.get_payments("value", None).await;
        assert!(matches!(result, Err(BrantaError::LogoUrlDomainMismatch(_))));
    }

    #[tokio::test]
    async fn get_payments_catches_mismatched_child_platform_logo_url() {
        let server = MockServer::start().await;
        let payments = vec![Payment {
            child_platform: Some(Platform {
                logo_url: Some("https://evil.example.com/logo.png".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }];
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(payments))
            .mount(&server)
            .await;

        let client = client_for(&server);
        let result = client.get_payments("value", None).await;
        assert!(matches!(result, Err(BrantaError::LogoUrlDomainMismatch(_))));
    }

    #[tokio::test]
    async fn get_payments_catches_mismatched_child_platform_logo_light_url() {
        let server = MockServer::start().await;
        let payments = vec![Payment {
            child_platform: Some(Platform {
                logo_light_url: Some("https://evil.example.com/logo-light.png".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        }];
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(payments))
            .mount(&server)
            .await;

        let client = client_for(&server);
        let result = client.get_payments("value", None).await;
        assert!(matches!(result, Err(BrantaError::LogoUrlDomainMismatch(_))));
    }

    #[tokio::test]
    async fn get_payments_passes_when_logos_match_or_are_absent() {
        let server = MockServer::start().await;
        let matching_logo = format!("{}/logo.png", server.uri());
        let matching_logo_light = format!("{}/logo-light.png", server.uri());
        let body = serde_json::json!([
            {},
            {
                "platform_logo_url": matching_logo,
                "platform_logo_light_url": matching_logo_light,
                "parent_platform": { "logo_url": matching_logo, "logo_light_url": matching_logo_light },
                "child_platform": { "logo_url": matching_logo, "logo_light_url": matching_logo_light },
            },
        ]);
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = client_for(&server);
        assert_eq!(client.get_payments("value", None).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn get_payments_sends_integration_metrics_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(move |req: &wiremock::Request| {
                assert_eq!(
                    req.headers
                        .get("X-Branta-Integration-Token")
                        .and_then(|value| value.to_str().ok()),
                    Some("secret")
                );
                ResponseTemplate::new(200).set_body_json(serde_json::json!([]))
            })
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server).with_integration_metrics_token("secret");
        assert!(client.get_payments("value", None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn get_payments_omits_integration_metrics_token_when_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(move |req: &wiremock::Request| {
                assert!(!req.headers.contains_key("X-Branta-Integration-Token"));
                ResponseTemplate::new(200).set_body_json(serde_json::json!([]))
            })
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server).with_integration_metrics_token("");
        assert!(client.get_payments("value", None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn post_payment_sends_bearer_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/payments"))
            .respond_with(move |req: &wiremock::Request| {
                assert_eq!(req.headers.get("Authorization").unwrap(), "Bearer test-key");
                ResponseTemplate::new(200).set_body_json(Payment::default())
            })
            .expect(1)
            .mount(&server)
            .await;

        let mut opts = options();
        opts.api_key = Some("test-key".to_string());
        let client = BrantaClient::new(opts).with_base_url_override(server.uri());
        client.post_payment(Payment::default(), None).await.unwrap();
    }

    #[tokio::test]
    async fn post_payment_unauthorized_without_making_any_request() {
        let server = MockServer::start().await;
        // No mock registered at all; `.expect(0)` makes wiremock fail the test if any request
        // reaches the server, proving the Unauthorized check short-circuits before the network call.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let client = client_for(&server); // no api key configured
        let result = client.post_payment(Payment::default(), None).await;
        assert!(matches!(result, Err(BrantaError::Unauthorized)));
    }

    #[tokio::test]
    async fn post_payment_includes_hmac_headers_when_configured() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/payments"))
            .respond_with(move |req: &wiremock::Request| {
                assert!(req.headers.contains_key("X-HMAC-Signature"));
                assert!(req.headers.contains_key("X-HMAC-Timestamp"));
                ResponseTemplate::new(200).set_body_json(Payment::default())
            })
            .expect(1)
            .mount(&server)
            .await;

        let mut opts = options();
        opts.api_key = Some("test-key".to_string());
        opts.hmac_secret = Some("shared-secret".to_string());
        let client = BrantaClient::new(opts).with_base_url_override(server.uri());
        client.post_payment(Payment::default(), None).await.unwrap();
    }

    #[tokio::test]
    async fn post_payment_omits_hmac_headers_when_not_configured() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/payments"))
            .respond_with(move |req: &wiremock::Request| {
                assert!(!req.headers.contains_key("X-HMAC-Signature"));
                ResponseTemplate::new(200).set_body_json(Payment::default())
            })
            .expect(1)
            .mount(&server)
            .await;

        let mut opts = options();
        opts.api_key = Some("test-key".to_string());
        let client = BrantaClient::new(opts).with_base_url_override(server.uri());
        client.post_payment(Payment::default(), None).await.unwrap();
    }

    #[tokio::test]
    async fn post_payment_non_2xx_returns_request_failed_with_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let mut opts = options();
        opts.api_key = Some("test-key".to_string());
        let client = BrantaClient::new(opts).with_base_url_override(server.uri());
        let result = client.post_payment(Payment::default(), None).await;
        assert!(matches!(
            result,
            Err(BrantaError::RequestFailed { status: 401 })
        ));
    }

    #[tokio::test]
    async fn is_api_key_valid_true_on_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/api-keys/health-check"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let mut opts = options();
        opts.api_key = Some("test-key".to_string());
        let client = BrantaClient::new(opts).with_base_url_override(server.uri());
        assert!(client.is_api_key_valid(None).await.unwrap());
    }

    #[tokio::test]
    async fn is_api_key_valid_false_on_failure_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/api-keys/health-check"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let mut opts = options();
        opts.api_key = Some("test-key".to_string());
        let client = BrantaClient::new(opts).with_base_url_override(server.uri());
        assert!(!client.is_api_key_valid(None).await.unwrap());
    }

    #[tokio::test]
    async fn is_api_key_valid_unauthorized_without_api_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let client = client_for(&server);
        let result = client.is_api_key_valid(None).await;
        assert!(matches!(result, Err(BrantaError::Unauthorized)));
    }
}
