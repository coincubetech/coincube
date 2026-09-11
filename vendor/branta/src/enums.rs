//! Enums shared across the SDK: server base URL, ZK privacy mode, and payment destination type.

use serde::{Deserialize, Serialize};

/// Which Branta server environment to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrantaServerBaseUrl {
    Staging,
    Production,
    Localhost,
}

impl BrantaServerBaseUrl {
    /// The base URL for this server environment (no trailing slash).
    pub fn url(&self) -> &'static str {
        match self {
            Self::Staging => "https://staging.guardrail.branta.pro",
            Self::Production => "https://guardrail.branta.pro",
            Self::Localhost => "http://localhost:3000",
        }
    }
}

/// Controls whether plain-text (non zero-knowledge) lookups and destinations are permitted.
///
/// `Strict` is the default and should be used unless a QR scanner is unavailable and
/// zero-knowledge encryption is therefore impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum PrivacyMode {
    /// Forbid plain-text on-chain lookups and non-ZK destinations. Never serialized over the wire.
    #[default]
    Strict,
    /// No zero-knowledge restrictions.
    Loose,
}

/// The type of a payment destination (on-chain address, Lightning invoice, etc).
///
/// Wire format is snake_case and MUST match every other Branta SDK exactly for cross-SDK
/// interoperability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationType {
    BitcoinAddress,
    Bolt11,
    Bolt12,
    LnUrl,
    TetherAddress,
    LnAddress,
    ArkAddress,
    SilentPayment,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_match_reference() {
        assert_eq!(
            BrantaServerBaseUrl::Staging.url(),
            "https://staging.guardrail.branta.pro"
        );
        assert_eq!(
            BrantaServerBaseUrl::Production.url(),
            "https://guardrail.branta.pro"
        );
        assert_eq!(
            BrantaServerBaseUrl::Localhost.url(),
            "http://localhost:3000"
        );
    }

    #[test]
    fn privacy_mode_defaults_to_strict() {
        assert_eq!(PrivacyMode::default(), PrivacyMode::Strict);
    }

    #[test]
    fn destination_type_wire_format_is_exact() {
        // These string literals must match every sibling SDK byte-for-byte.
        let cases = [
            (DestinationType::BitcoinAddress, "\"bitcoin_address\""),
            (DestinationType::Bolt11, "\"bolt11\""),
            (DestinationType::Bolt12, "\"bolt12\""),
            (DestinationType::LnUrl, "\"ln_url\""),
            (DestinationType::TetherAddress, "\"tether_address\""),
            (DestinationType::LnAddress, "\"ln_address\""),
            (DestinationType::ArkAddress, "\"ark_address\""),
            (DestinationType::SilentPayment, "\"silent_payment\""),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected, "wire format mismatch for {variant:?}");
            let round_tripped: DestinationType = serde_json::from_str(expected).unwrap();
            assert_eq!(round_tripped, variant);
        }
    }
}
