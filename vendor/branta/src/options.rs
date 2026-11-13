//! Service configuration, overridable per call.

use crate::enums::BrantaEnvironment;

/// Constructor argument and per-call override.
///
/// A per-call `environment` of `None` leaves the service default in place.
#[derive(Debug, Clone, Default)]
pub struct BrantaApiOptions {
    pub environment: Option<BrantaEnvironment>,
    pub api_key: Option<String>,
    pub hmac_secret: Option<String>,
}

impl BrantaApiOptions {
    pub fn new(environment: BrantaEnvironment) -> Self {
        Self {
            environment: Some(environment),
            api_key: None,
            hmac_secret: None,
        }
    }

    pub fn get_base_url(&self, overrides: Option<&BrantaApiOptions>) -> &'static str {
        overrides
            .and_then(|options| options.environment)
            .or(self.environment)
            .unwrap_or(BrantaEnvironment::Staging)
            .url()
    }

    pub fn get_api_key<'a>(&'a self, overrides: Option<&'a BrantaApiOptions>) -> Option<&'a str> {
        overrides
            .and_then(|options| options.api_key.as_deref())
            .or(self.api_key.as_deref())
    }

    pub fn get_hmac_secret<'a>(
        &'a self,
        overrides: Option<&'a BrantaApiOptions>,
    ) -> Option<&'a str> {
        overrides
            .and_then(|options| options.hmac_secret.as_deref())
            .or(self.hmac_secret.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_over_default_field_by_field() {
        let default = BrantaApiOptions {
            api_key: Some("default-key".into()),
            hmac_secret: Some("default-hmac".into()),
            ..BrantaApiOptions::new(BrantaEnvironment::Staging)
        };
        let over = BrantaApiOptions {
            api_key: Some("child-key".into()),
            ..BrantaApiOptions::new(BrantaEnvironment::Production)
        };

        assert_eq!(
            default.get_base_url(Some(&over)),
            "https://guardrail.branta.pro"
        );
        assert_eq!(default.get_api_key(Some(&over)), Some("child-key"));
        assert_eq!(default.get_hmac_secret(Some(&over)), Some("default-hmac"));
    }

    #[test]
    fn missing_environment_does_not_override() {
        let default = BrantaApiOptions::new(BrantaEnvironment::Production);
        let over = BrantaApiOptions::default();
        assert_eq!(
            default.get_base_url(Some(&over)),
            "https://guardrail.branta.pro"
        );
    }
}
