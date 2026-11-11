//! Fluent builder for constructing a [`Payment`] to post.

use crate::enums::DestinationType;
use crate::models::{Destination, Payment, Platform};

#[derive(Debug, Default)]
pub struct PaymentBuilder {
    payment: Payment,
}

impl PaymentBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a destination. `add_payment` encrypts every destination it can.
    pub fn add_destination(
        mut self,
        address: impl Into<String>,
        r#type: Option<DestinationType>,
    ) -> Self {
        self.payment
            .destinations
            .push(Destination::new(address, r#type));
        self
    }

    pub fn set_description(mut self, description: impl Into<String>) -> Self {
        self.payment.description = Some(description.into());
        self
    }

    /// Merges `key`/`value` into the payment's metadata, which is stored as a JSON-object string
    /// (i.e. double-encoded: `metadata` is itself the serialized form of a `{key: value}` map).
    pub fn add_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let mut metadata_map: serde_json::Map<String, serde_json::Value> = self
            .payment
            .metadata
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        metadata_map.insert(key.into(), serde_json::Value::String(value.into()));
        self.payment.metadata =
            Some(serde_json::to_string(&metadata_map).expect("map of strings always serializes"));
        self
    }

    pub fn set_ttl(mut self, ttl: i32) -> Self {
        self.payment.ttl = ttl;
        self
    }

    pub fn set_platform_logo_url(mut self, platform_logo_url: impl Into<String>) -> Self {
        self.payment.platform_logo_url = Some(platform_logo_url.into());
        self
    }

    pub fn set_child_platform(
        mut self,
        name: impl Into<String>,
        logo_url: Option<String>,
        logo_light_url: Option<String>,
    ) -> Self {
        self.payment.child_platform = Some(Platform {
            name: Some(name.into()),
            logo_url,
            logo_light_url,
        });
        self
    }

    pub fn build(self) -> Payment {
        self.payment
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_destination_without_type() {
        let payment = PaymentBuilder::new().add_destination("addr1", None).build();
        assert_eq!(payment.destinations.len(), 1);
        assert_eq!(payment.destinations[0].value, "addr1");
        assert_eq!(payment.destinations[0].r#type, None);
        assert!(!payment.destinations[0].is_encrypted);
    }

    #[test]
    fn add_destination_with_type() {
        let payment = PaymentBuilder::new()
            .add_destination("addr1", Some(DestinationType::BitcoinAddress))
            .build();
        assert_eq!(
            payment.destinations[0].r#type,
            Some(DestinationType::BitcoinAddress)
        );
    }

    #[test]
    fn set_description() {
        let payment = PaymentBuilder::new().set_description("desc").build();
        assert_eq!(payment.description.as_deref(), Some("desc"));
    }

    #[test]
    fn add_metadata_merges_multiple_keys() {
        let payment = PaymentBuilder::new()
            .add_metadata("a", "1")
            .add_metadata("b", "2")
            .build();
        let map: serde_json::Value = serde_json::from_str(&payment.metadata.unwrap()).unwrap();
        assert_eq!(map["a"], "1");
        assert_eq!(map["b"], "2");
    }

    #[test]
    fn add_metadata_overwrites_same_key_not_duplicates() {
        let payment = PaymentBuilder::new()
            .add_metadata("a", "1")
            .add_metadata("a", "2")
            .build();
        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&payment.metadata.unwrap()).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map["a"], "2");
    }

    #[test]
    fn set_ttl() {
        let payment = PaymentBuilder::new().set_ttl(600).build();
        assert_eq!(payment.ttl, 600);
    }

    #[test]
    fn set_platform_logo_url() {
        let payment = PaymentBuilder::new()
            .set_platform_logo_url("https://example.com/logo.png")
            .build();
        assert_eq!(
            payment.platform_logo_url.as_deref(),
            Some("https://example.com/logo.png")
        );
    }

    #[test]
    fn set_child_platform() {
        let payment = PaymentBuilder::new()
            .set_child_platform(
                "ChildBrand",
                Some("https://example.com/logo.png".to_string()),
                Some("https://example.com/logo-light.png".to_string()),
            )
            .build();
        let child = payment.child_platform.unwrap();
        assert_eq!(child.name.as_deref(), Some("ChildBrand"));
        assert_eq!(
            child.logo_url.as_deref(),
            Some("https://example.com/logo.png")
        );
        assert_eq!(
            child.logo_light_url.as_deref(),
            Some("https://example.com/logo-light.png")
        );
    }

    #[test]
    fn full_chain_builds_expected_payment() {
        let payment = PaymentBuilder::new()
            .set_description("Testing description")
            .add_destination(
                "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa",
                Some(DestinationType::BitcoinAddress),
            )
            .set_ttl(600)
            .build();
        assert_eq!(payment.description.as_deref(), Some("Testing description"));
        assert_eq!(payment.ttl, 600);
        assert_eq!(
            payment.destinations[0].value,
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"
        );
    }
}
