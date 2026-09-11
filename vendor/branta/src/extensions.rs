//! Free-function helpers: hash-ZK type detection and normalized-hash/URL-fragment building.
//!
//! Transcribed from `Branta/Extensions/BrantaExtensions.cs`. `get_hash_zk_type` intentionally
//! returns `None` for `Bolt12`/`LnUrl`/`LnAddress`/`TetherAddress` — those are valid
//! `DestinationType`s but are NOT hash-ZK types in any sibling SDK. Do not "fix" this.

use sha2::{Digest, Sha256};

use crate::enums::DestinationType;

pub fn is_bolt11(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("lnbc") || lower.starts_with("lntb") || lower.starts_with("lnbcrt")
}

pub fn is_ark(value: &str) -> bool {
    value.to_ascii_lowercase().starts_with("ark1")
}

pub fn is_silent_payment(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("sp1") || lower.starts_with("tsp1")
}

/// Returns the hash-ZK `DestinationType` for `value`, or `None` if it isn't one of the three
/// hash-ZK types (bolt11, ark, silent payment).
pub fn get_hash_zk_type(value: &str) -> Option<DestinationType> {
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

/// `SHA-256(lowercase(value))` formatted as 64-char **uppercase** hex, matching .NET's
/// `Convert.ToHexString`. This value doubles as the AES secret for hash-ZK destinations, so a
/// case mismatch here silently breaks cross-SDK payment lookups.
pub fn to_normalized_hash(value: &str) -> String {
    let normalized = value.to_lowercase();
    let digest = Sha256::digest(normalized.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02X}"));
    }
    out
}

/// Builds a `#k-{zk_id}={key}&k-{zk_id2}={key2}...` URL fragment from resolved decryption keys.
///
/// Iterates `keys` in insertion order (an `IndexMap` or similarly ordered map is expected) so
/// fragment ordering is deterministic, matching sibling SDKs whose native map types preserve
/// insertion order.
pub fn to_url_fragment<'a>(keys: impl IntoIterator<Item = (&'a String, &'a String)>) -> String {
    let parts: Vec<String> = keys
        .into_iter()
        .map(|(k, v)| format!("k-{k}={v}"))
        .collect();
    format!("#{}", parts.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    #[test]
    fn is_bolt11_case_insensitive_prefixes() {
        assert!(is_bolt11("lnbc1..."));
        assert!(is_bolt11("LNBC1..."));
        assert!(is_bolt11("lntb1..."));
        assert!(is_bolt11("LNTB1..."));
        assert!(is_bolt11("lnbcrt1..."));
        assert!(is_bolt11("LNBCRT1..."));
        assert!(!is_bolt11("bc1qsomething"));
    }

    #[test]
    fn is_ark_case_insensitive_prefix() {
        assert!(is_ark("ark1qsomething"));
        assert!(is_ark("ARK1QSOMETHING"));
        assert!(!is_ark("bc1qsomething"));
    }

    #[test]
    fn is_silent_payment_case_insensitive_prefixes() {
        assert!(is_silent_payment("sp1qsomething"));
        assert!(is_silent_payment("SP1QSOMETHING"));
        assert!(is_silent_payment("tsp1qsomething"));
        assert!(is_silent_payment("TSP1QSOMETHING"));
        assert!(!is_silent_payment("bc1qsomething"));
    }

    #[test]
    fn get_hash_zk_type_only_bolt11_ark_silent_payment() {
        assert_eq!(get_hash_zk_type("lnbc1..."), Some(DestinationType::Bolt11));
        assert_eq!(
            get_hash_zk_type("ark1..."),
            Some(DestinationType::ArkAddress)
        );
        assert_eq!(
            get_hash_zk_type("sp1..."),
            Some(DestinationType::SilentPayment)
        );
    }

    #[test]
    fn get_hash_zk_type_excludes_non_hash_zk_types() {
        // These are valid DestinationTypes but NOT hash-ZK types. A future contributor could
        // "helpfully" add them here — don't. Guarded explicitly so that change gets caught.
        assert_eq!(get_hash_zk_type("lno1..."), None); // bolt12
        assert_eq!(get_hash_zk_type("LNURL1..."), None); // ln_url
        assert_eq!(get_hash_zk_type("user@example.com"), None); // ln_address
        assert_eq!(
            get_hash_zk_type("0x00000000000000000000000000000000000000"),
            None
        ); // tether
        assert_eq!(get_hash_zk_type("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"), None);
        // bitcoin
    }

    #[test]
    fn to_normalized_hash_is_uppercase_hex_of_lowercased_value() {
        // Known-good vector: SHA-256("test") = 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
        let hash = to_normalized_hash("test");
        assert_eq!(
            hash,
            "9F86D081884C7D659A2FEAA0C55AD015A3BF4F1B2B0B822CD15D6C15B0F00A08".to_uppercase()
        );
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, hash.to_uppercase());
    }

    #[test]
    fn to_normalized_hash_is_case_insensitive_on_input() {
        assert_eq!(to_normalized_hash("TEST"), to_normalized_hash("test"));
        assert_eq!(to_normalized_hash("TeSt"), to_normalized_hash("test"));
    }

    #[test]
    fn to_url_fragment_empty() {
        let keys: IndexMap<String, String> = IndexMap::new();
        assert_eq!(to_url_fragment(keys.iter()), "#");
    }

    #[test]
    fn to_url_fragment_single_key() {
        let mut keys = IndexMap::new();
        keys.insert("zk-1".to_string(), "secret-1".to_string());
        assert_eq!(to_url_fragment(keys.iter()), "#k-zk-1=secret-1");
    }

    #[test]
    fn to_url_fragment_multiple_keys_preserves_insertion_order() {
        let mut keys = IndexMap::new();
        keys.insert("zk-1".to_string(), "secret-1".to_string());
        keys.insert("zk-2".to_string(), "secret-2".to_string());
        assert_eq!(
            to_url_fragment(keys.iter()),
            "#k-zk-1=secret-1&k-zk-2=secret-2"
        );
    }
}
