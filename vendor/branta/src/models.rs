//! Wire models exchanged with the Branta API.

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::enums::{DestinationType, VerifyStatus};

/// A payment destination.
///
/// `id` is sent as `zk_id`. Serialization always sends `zk: true`.
/// On a GET, `zk: true` attempts decrypt. A missing or false `zk` skips decrypt.
#[derive(Debug, Clone, PartialEq)]
pub struct Destination {
    pub value: String,
    pub is_primary: bool,
    pub is_encrypted: bool,
    pub r#type: Option<DestinationType>,
    pub id: Option<String>,
    pub encrypted_dek: Option<String>,
}

impl Destination {
    pub fn new(value: impl Into<String>, r#type: Option<DestinationType>) -> Self {
        Self {
            value: value.into(),
            is_primary: false,
            is_encrypted: false,
            r#type,
            id: None,
            encrypted_dek: None,
        }
    }
}

impl Serialize for Destination {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut len = 3;
        if self.r#type.is_some() {
            len += 1;
        }
        if self.id.is_some() {
            len += 1;
        }
        if self.encrypted_dek.is_some() {
            len += 1;
        }
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("value", &self.value)?;
        map.serialize_entry("primary", &self.is_primary)?;
        map.serialize_entry("zk", &true)?;
        if let Some(r#type) = self.r#type {
            map.serialize_entry("type", &r#type)?;
        }
        if let Some(id) = &self.id {
            map.serialize_entry("zk_id", id)?;
        }
        if let Some(dek) = &self.encrypted_dek {
            map.serialize_entry("encrypted_dek", dek)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Destination {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DestVisitor;
        impl<'de> Visitor<'de> for DestVisitor {
            type Value = Destination;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a destination object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut value = None;
                let mut is_primary = false;
                let mut zk: Option<bool> = None;
                let mut r#type = None;
                let mut id = None;
                let mut encrypted_dek = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "value" => value = Some(map.next_value()?),
                        "primary" => is_primary = map.next_value().unwrap_or(false),
                        "zk" => zk = map.next_value().ok(),
                        "type" => r#type = map.next_value().ok(),
                        "zk_id" => id = map.next_value().ok(),
                        "encrypted_dek" => encrypted_dek = map.next_value().ok(),
                        _ => {
                            let _ = map.next_value::<de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(Destination {
                    value: value.unwrap_or_default(),
                    is_primary,
                    is_encrypted: zk == Some(true),
                    r#type,
                    id,
                    encrypted_dek,
                })
            }
        }
        deserializer.deserialize_map(DestVisitor)
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

fn is_zero(value: &i32) -> bool {
    *value == 0
}

/// A payment exchanged with Guardrail.
///
/// `description` is not a plaintext API field. `add_payment` folds it into
/// encrypted metadata, and `verify` lifts it back after metadata decrypts.
/// `parent_platform` is set by the server and is never sent on a POST.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Payment {
    #[serde(skip)]
    pub description: Option<String>,
    #[serde(default)]
    pub destinations: Vec<Destination>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ttl: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_logo_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_logo_light_url: Option<String>,
    #[serde(default, skip_serializing)]
    pub parent_platform: Option<Platform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_platform: Option<Platform>,
    #[serde(skip)]
    pub is_metadata_decrypted: bool,
}

/// What [`crate::BrantaService::verify`] returns. `payments` is set only for a match.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifyResult {
    pub status: VerifyStatus,
    pub payments: Vec<Payment>,
    pub verify_url: String,
}

/// The result of [`crate::BrantaService::add_payment`].
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
    fn destination_wire_always_sends_zk_true() {
        let destination = Destination {
            value: "abc".into(),
            is_primary: true,
            is_encrypted: false,
            r#type: Some(DestinationType::BitcoinAddress),
            id: Some("zk-1".into()),
            encrypted_dek: Some("dek".into()),
        };
        let json = serde_json::to_value(&destination).unwrap();
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
    fn missing_or_false_zk_skips_decrypt() {
        let missing: Destination = serde_json::from_str(r#"{"value":"abc"}"#).unwrap();
        assert!(!missing.is_encrypted);
        let falsy: Destination = serde_json::from_str(r#"{"value":"abc","zk":false}"#).unwrap();
        assert!(!falsy.is_encrypted);
        let truthy: Destination =
            serde_json::from_str(r#"{"value":"abc","zk":true,"zk_id":"zk-1"}"#).unwrap();
        assert!(truthy.is_encrypted);
        assert_eq!(truthy.id.as_deref(), Some("zk-1"));
    }

    #[test]
    fn payment_omits_description_parent_and_unset_ttl() {
        let payment = Payment {
            description: Some("desc".into()),
            destinations: vec![Destination::new("abc", None)],
            ttl: 0,
            parent_platform: Some(Platform {
                name: Some("Parent".into()),
                ..Platform::default()
            }),
            ..Payment::default()
        };
        let json = serde_json::to_value(&payment).unwrap();
        assert!(json.get("description").is_none());
        assert!(json.get("parent_platform").is_none());
        assert!(json.get("ttl").is_none());
        assert_eq!(json["destinations"][0]["zk"], true);
    }

    #[test]
    fn payment_sends_ttl_when_set() {
        let payment = Payment {
            ttl: 600,
            ..Payment::default()
        };
        let json = serde_json::to_value(&payment).unwrap();
        assert_eq!(json["ttl"], 600);
    }
}
