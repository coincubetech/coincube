//! Per-service configuration, overridable per-call.

use crate::enums::{BrantaServerBaseUrl, PrivacyMode};

#[derive(Debug, Clone)]
pub struct BrantaClientOptions {
    pub base_url: BrantaServerBaseUrl,
    pub default_api_key: Option<String>,
    pub hmac_secret: Option<String>,
    pub privacy: PrivacyMode,
}

impl BrantaClientOptions {
    pub fn new(base_url: BrantaServerBaseUrl) -> Self {
        Self {
            base_url,
            default_api_key: None,
            hmac_secret: None,
            privacy: PrivacyMode::default(),
        }
    }

    /// Resolves the effective base URL: `overrides` wins if present, else `self`.
    pub fn get_base_url(&self, overrides: Option<&BrantaClientOptions>) -> &'static str {
        overrides.map(|o| o.base_url).unwrap_or(self.base_url).url()
    }

    /// Resolves the effective privacy mode: `overrides` wins if present, else `self`.
    pub fn get_privacy(&self, overrides: Option<&BrantaClientOptions>) -> PrivacyMode {
        overrides.map(|o| o.privacy).unwrap_or(self.privacy)
    }

    /// Resolves the effective API key, field-by-field: `overrides`'s key wins if `Some`, else
    /// falls back to `self`'s key.
    pub fn get_api_key<'a>(
        &'a self,
        overrides: Option<&'a BrantaClientOptions>,
    ) -> Option<&'a str> {
        overrides
            .and_then(|o| o.default_api_key.as_deref())
            .or(self.default_api_key.as_deref())
    }

    /// Resolves the effective HMAC secret, field-by-field: `overrides`'s secret wins if `Some`,
    /// else falls back to `self`'s secret.
    pub fn get_hmac_secret<'a>(
        &'a self,
        overrides: Option<&'a BrantaClientOptions>,
    ) -> Option<&'a str> {
        overrides
            .and_then(|o| o.hmac_secret.as_deref())
            .or(self.hmac_secret.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(base_url: BrantaServerBaseUrl) -> BrantaClientOptions {
        BrantaClientOptions::new(base_url)
    }

    #[test]
    fn override_wins_over_default_field_by_field() {
        let default = BrantaClientOptions {
            default_api_key: Some("default-key".into()),
            hmac_secret: Some("default-hmac".into()),
            ..opts(BrantaServerBaseUrl::Staging)
        };
        let mut over = opts(BrantaServerBaseUrl::Production);
        over.default_api_key = Some("child-key".into());
        // over.hmac_secret left None -> should fall back to default's hmac_secret.

        assert_eq!(
            default.get_base_url(Some(&over)),
            "https://guardrail.branta.pro"
        );
        assert_eq!(default.get_api_key(Some(&over)), Some("child-key"));
        assert_eq!(default.get_hmac_secret(Some(&over)), Some("default-hmac"));
    }

    #[test]
    fn none_overrides_fall_back_entirely_to_default() {
        let default = opts(BrantaServerBaseUrl::Staging);
        assert_eq!(
            default.get_base_url(None),
            "https://staging.guardrail.branta.pro"
        );
        assert_eq!(default.get_privacy(None), PrivacyMode::Strict);
        assert_eq!(default.get_api_key(None), None);
        assert_eq!(default.get_hmac_secret(None), None);
    }
}
