//! Wire models exchanged with the Branta API.
//!
//! Field names on the wire are snake_case and must match every other Branta SDK exactly.
//! `is_encrypted` and `is_metadata_decrypted` are client-side-only derived state and are never
//! serialized (`#[serde(skip)]`), mirroring `[JsonIgnore]`/`@Transient` in the sibling SDKs.

use serde::{Deserialize, Serialize};

use crate::enums::DestinationType;
use crate::error::BrantaError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub value: String,

    #[serde(rename = "primary", default)]
    pub is_primary: bool,

    #[serde(rename = "zk", default)]
    pub is_zk: bool,

    /// Client-side only: true while `value` still holds ciphertext. Never sent or received.
    #[serde(skip)]
    pub is_encrypted: bool,

    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<DestinationType>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zk_id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_dek: Option<String>,
}

impl Destination {
    pub fn new(value: impl Into<String>, r#type: Option<DestinationType>) -> Self {
        Self {
            value: value.into(),
            is_primary: false,
            is_zk: false,
            is_encrypted: false,
            r#type,
            zk_id: None,
            encrypted_dek: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Platform {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_light_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Payment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default)]
    pub destinations: Vec<Destination>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,

    #[serde(default)]
    pub ttl: i32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_logo_url: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_logo_light_url: Option<String>,

    /// Deserialize-only: set by the server, never sent on a POST.
    #[serde(default, skip_serializing, skip_serializing_if = "Option::is_none")]
    pub parent_platform: Option<Platform>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_platform: Option<Platform>,

    /// NOTE: the majority wire name across sibling SDKs (dotnet/js/python/dart). `branta-kotlin`
    /// alone uses `btcpay_server_plugin_version` (no underscore) — a pre-existing cross-SDK
    /// inconsistency, not something to replicate here. See CLAUDE.md.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub btc_pay_server_plugin_version: Option<String>,

    /// Client-side only: true once `metadata` has been decrypted. Never sent or received.
    #[serde(skip)]
    pub is_metadata_decrypted: bool,
}

impl Payment {
    /// The value of the first destination, or an error if the payment has none.
    pub fn get_default_value(&self) -> Result<&str, BrantaError> {
        self.destinations
            .first()
            .map(|d| d.value.as_str())
            .ok_or(BrantaError::NoDestinations)
    }
}

/// Result of `BrantaService::get_payments` / `get_payments_by_qr_code`.
///
/// `verify_url` is always populated, even when `payments` is empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaymentsResult {
    pub payments: Vec<Payment>,
    pub verify_url: String,
}

/// Result of `BrantaService::add_payment`.
#[derive(Debug, Clone, PartialEq)]
pub struct AddPaymentResult {
    pub payment: Payment,
    pub secret: String,
    pub verify_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_wire_format() {
        let d = Destination {
            value: "abc".into(),
            is_primary: true,
            is_zk: true,
            is_encrypted: true, // must not appear in the JSON
            r#type: Some(DestinationType::BitcoinAddress),
            zk_id: Some("zk-1".into()),
            encrypted_dek: Some("dek".into()),
        };
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "value": "abc",
                "primary": true,
                "zk": true,
                "type": "bitcoin_address",
                "zk_id": "zk-1",
                "encrypted_dek": "dek",
            })
        );
    }

    #[test]
    fn destination_optional_fields_omitted_when_none() {
        let d = Destination::new("abc", None);
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"value": "abc", "primary": false, "zk": false})
        );
    }

    #[test]
    fn destination_deserialize_defaults() {
        let d: Destination = serde_json::from_str(r#"{"value": "abc"}"#).unwrap();
        assert!(!d.is_primary);
        assert!(!d.is_zk);
        assert!(!d.is_encrypted);
        assert_eq!(d.r#type, None);
    }

    #[test]
    fn payment_wire_format_field_names() {
        let payment = Payment {
            description: Some("desc".into()),
            destinations: vec![Destination::new("abc", None)],
            created_at: Some("2026-01-01T00:00:00Z".into()),
            ttl: 600,
            metadata: Some("{}".into()),
            platform: Some("Acme".into()),
            platform_logo_url: Some("https://example.com/logo.png".into()),
            platform_logo_light_url: Some("https://example.com/logo-light.png".into()),
            parent_platform: Some(Platform {
                name: Some("Parent".into()),
                ..Default::default()
            }),
            child_platform: Some(Platform {
                name: Some("Child".into()),
                ..Default::default()
            }),
            btc_pay_server_plugin_version: Some("1.0.0".into()),
            is_metadata_decrypted: true, // must not appear in the JSON
        };
        let json = serde_json::to_value(&payment).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(obj.get("created_at").unwrap(), "2026-01-01T00:00:00Z");
        assert_eq!(obj.get("btc_pay_server_plugin_version").unwrap(), "1.0.0");
        assert_eq!(obj.get("child_platform").unwrap()["name"], "Child");
        assert!(!obj.contains_key("is_metadata_decrypted"));
        // parent_platform is deserialize-only: never present on serialize.
        assert!(!obj.contains_key("parent_platform"));
    }

    #[test]
    fn payment_parent_platform_round_trips_on_deserialize() {
        let json = serde_json::json!({
            "destinations": [],
            "parent_platform": {"name": "Parent"}
        });
        let payment: Payment = serde_json::from_value(json).unwrap();
        assert_eq!(payment.parent_platform.unwrap().name.unwrap(), "Parent");
    }

    #[test]
    fn get_default_value_errors_on_no_destinations() {
        let payment = Payment::default();
        assert!(matches!(
            payment.get_default_value(),
            Err(BrantaError::NoDestinations)
        ));
    }

    #[test]
    fn get_default_value_returns_first_destination() {
        let payment = Payment {
            destinations: vec![
                Destination::new("first", None),
                Destination::new("second", None),
            ],
            ..Default::default()
        };
        assert_eq!(payment.get_default_value().unwrap(), "first");
    }
}
