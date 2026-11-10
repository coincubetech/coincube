//! Derived-key detection, the normalized hash, and verify-URL fragments.

use indexmap::IndexMap;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use sha2::{Digest, Sha256};

use crate::enums::DestinationType;

/// encodeURIComponent's safe set: ALPHA / DIGIT / "-" / "_" / "." / "!" / "~" / "*" / "'" / "(" / ")".
const FRAGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

pub(crate) fn is_bolt11(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("lnbc") || lower.starts_with("lntb") || lower.starts_with("lnbcrt")
}

pub(crate) fn is_ark(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("ark1") || lower.starts_with("tark1")
}

pub(crate) fn is_silent_payment(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("sp1") || lower.starts_with("tsp1")
}

/// bolt11, Ark, or silent payment. Other BIP21 extras are `None`. They are not destination types.
pub(crate) fn derived_key_type(value: &str) -> Option<DestinationType> {
    if is_bolt11(value) {
        Some(DestinationType::Bolt11)
    } else if is_ark(value) {
        Some(DestinationType::ArkAddress)
    } else if is_silent_payment(value) {
        Some(DestinationType::SilentPayment)
    } else {
        None
    }
}

/// SHA-256 of the lowercased value as 64-char uppercase hex.
///
/// This is the AES secret for a derived-key destination.
pub(crate) fn to_normalized_hash(value: &str) -> String {
    let digest = Sha256::digest(value.to_lowercase().as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02X}"));
    }
    out
}

/// `#k-{id}={key}&...` in insertion order. Names and values are percent-encoded.
/// An empty map returns `""`.
pub(crate) fn to_url_fragment(keys: &IndexMap<String, String>) -> String {
    if keys.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = keys
        .iter()
        .map(|(key, value)| {
            format!(
                "k-{}={}",
                utf8_percent_encode(key, FRAGMENT),
                utf8_percent_encode(value, FRAGMENT)
            )
        })
        .collect();
    format!("#{}", parts.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_key_type_only_bolt11_ark_silent_payment() {
        assert_eq!(derived_key_type("lnbc1..."), Some(DestinationType::Bolt11));
        assert_eq!(
            derived_key_type("ark1..."),
            Some(DestinationType::ArkAddress)
        );
        assert_eq!(
            derived_key_type("tark1..."),
            Some(DestinationType::ArkAddress)
        );
        assert_eq!(
            derived_key_type("sp1..."),
            Some(DestinationType::SilentPayment)
        );
        for value in [
            "lno1...",
            "LNURL1...",
            "user@example.com",
            "0x00000000000000000000000000000000000000",
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa",
        ] {
            assert_eq!(derived_key_type(value), None, "{value}");
        }
    }

    #[test]
    fn to_normalized_hash_is_uppercase_hex_of_lowercased_value() {
        assert_eq!(
            to_normalized_hash("test"),
            "9F86D081884C7D659A2FEAA0C55AD015A3BF4F1B2B0B822CD15D6C15B0F00A08"
        );
        assert_eq!(to_normalized_hash("TEST"), to_normalized_hash("test"));
    }

    #[test]
    fn fragment_encodes_reserved_characters_and_empty_is_blank() {
        let keys = IndexMap::new();
        assert_eq!(to_url_fragment(&keys), "");
        let mut keys = IndexMap::new();
        keys.insert("k a".into(), "x&y".into());
        assert_eq!(to_url_fragment(&keys), "#k-k%20a=x%26y");
    }
}
