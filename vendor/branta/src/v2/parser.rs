//! Parses raw QR text into one or more payment destinations.
//!
//! Transcribed from `Branta/V2/Classes/QRParser.cs`. Infallible by design — no sibling SDK ever
//! raises a parse error; unrecognized input just yields a destination with `type: None`.

use crate::enums::DestinationType;
use crate::extensions::{is_ark, is_bolt11, is_silent_payment};

#[derive(Debug, Clone, PartialEq)]
pub struct QrDestination {
    pub value: String,
    pub r#type: Option<DestinationType>,
}

#[derive(Debug, Clone, Default)]
pub struct QrParser {
    pub destinations: Vec<QrDestination>,
    pub on_chain_encryption_text: Option<String>,
    pub on_chain_encryption_secret: Option<String>,
}

impl QrParser {
    pub fn new(qr_text: &str) -> Self {
        let text = qr_text.trim();
        let mut parser = QrParser::default();

        let Some(scheme) = uri_scheme(text) else {
            parser.destinations.push(QrDestination {
                value: text.to_string(),
                r#type: detect_plain_text_type(text),
            });
            return parser;
        };

        let scheme_lower = scheme.to_ascii_lowercase();
        if scheme_lower != "bitcoin" && scheme_lower != "lightning" {
            // A recognized-but-unhandled URI scheme: the whole trimmed text becomes the value,
            // untyped -- matches the reference implementation's fallback branch exactly.
            parser.destinations.push(QrDestination {
                value: text.to_string(),
                r#type: None,
            });
            return parser;
        }

        let dest_value = extract_destination(text);
        let dest_type = if scheme_lower == "bitcoin" {
            Some(DestinationType::BitcoinAddress)
        } else {
            get_lightning_scheme_type(dest_value)
        };
        parser.destinations.push(QrDestination {
            value: dest_value.to_string(),
            r#type: dest_type,
        });

        let query = parse_query(extract_query(text));

        parser.on_chain_encryption_text = query.get("branta_id").cloned();
        parser.on_chain_encryption_secret = query.get("branta_secret").cloned();

        for key in ["lightning", "bolt12", "ark", "silent_payment"] {
            if let Some(value) = query.get(key) {
                parser.destinations.push(QrDestination {
                    value: value.clone(),
                    r#type: detect_plain_text_type(value),
                });
            }
        }

        parser
    }

    pub fn destination(&self) -> Option<&str> {
        self.destinations.first().map(|d| d.value.as_str())
    }

    pub fn destination_type(&self) -> Option<DestinationType> {
        self.destinations.first().and_then(|d| d.r#type)
    }

    /// True iff both `branta_id` and `branta_secret` query params were present.
    pub fn is_on_chain_zk(&self) -> bool {
        self.on_chain_encryption_text.is_some() && self.on_chain_encryption_secret.is_some()
    }
}

/// Returns the URI scheme (e.g. `"bitcoin"`) if `text` starts with a syntactically valid
/// `scheme:` prefix (RFC 3986 `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":"`), else `None`.
/// This is a practical stand-in for .NET's `Uri.TryCreate(text, UriKind.Absolute, ...)`.
fn uri_scheme(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let mut end = 1;
    while end < bytes.len() {
        let c = bytes[end];
        if c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.' {
            end += 1;
        } else {
            break;
        }
    }
    if end < bytes.len() && bytes[end] == b':' {
        Some(&text[..end])
    } else {
        None
    }
}

/// Everything after the first `:` and before the next `?` (or end of string).
fn extract_destination(text: &str) -> &str {
    let after_colon = text.split_once(':').map(|(_, rest)| rest).unwrap_or("");
    match after_colon.find('?') {
        Some(pos) => &after_colon[..pos],
        None => after_colon,
    }
}

/// Everything after the first `?` following the scheme (or empty if there is none).
fn extract_query(text: &str) -> &str {
    let after_colon = text.split_once(':').map(|(_, rest)| rest).unwrap_or("");
    match after_colon.find('?') {
        Some(pos) => &after_colon[pos + 1..],
        None => "",
    }
}

fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut parts = pair.splitn(2, '=');
        let key = parts.next().unwrap_or("");
        if key.is_empty() {
            continue;
        }
        let value = parts.next().unwrap_or("");
        let key = percent_decode(key).to_ascii_lowercase();
        let value = percent_decode(value);
        map.insert(key, value);
    }
    map
}

fn percent_decode(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .into_owned()
}

/// Destination-type detection for a `lightning:` URI's primary destination -- restricted to
/// bolt11/bolt12/ln_url, matching `GetDestinationType` in the reference (NOT the full
/// `detect_plain_text_type` matrix).
fn get_lightning_scheme_type(dest: &str) -> Option<DestinationType> {
    if is_bolt11(dest) {
        Some(DestinationType::Bolt11)
    } else if dest.len() >= 3 && dest[..3].eq_ignore_ascii_case("lno") {
        Some(DestinationType::Bolt12)
    } else if dest.len() >= 5 && dest[..5].eq_ignore_ascii_case("lnurl") {
        Some(DestinationType::LnUrl)
    } else {
        None
    }
}

/// Full plain-text destination-type detection matrix, transcribed from `DetectPlainTextType`.
fn detect_plain_text_type(value: &str) -> Option<DestinationType> {
    if is_bolt11(value) {
        return Some(DestinationType::Bolt11);
    }
    if value.len() >= 3 && value[..3].eq_ignore_ascii_case("lno") {
        return Some(DestinationType::Bolt12);
    }
    if value.len() >= 5 && value[..5].eq_ignore_ascii_case("lnurl") {
        return Some(DestinationType::LnUrl);
    }
    if is_ark(value) {
        return Some(DestinationType::ArkAddress);
    }
    if is_silent_payment(value) {
        return Some(DestinationType::SilentPayment);
    }
    if is_ethereum_address(value) {
        return Some(DestinationType::TetherAddress);
    }
    if is_tron_address(value) {
        return Some(DestinationType::TetherAddress);
    }
    if is_ln_address(value) {
        return Some(DestinationType::LnAddress);
    }
    if value.starts_with('1')
        || value.starts_with('3')
        || value.len() >= 3 && value[..3].eq_ignore_ascii_case("bc1")
    {
        return Some(DestinationType::BitcoinAddress);
    }
    None
}

fn is_ethereum_address(value: &str) -> bool {
    value.len() == 42
        && value[..2].eq_ignore_ascii_case("0x")
        && value[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn is_tron_address(value: &str) -> bool {
    value.len() == 34 && value.starts_with('T')
}

/// `^[^@\s]+@[^@\s]+\.[^@\s]+$` -- exactly one `@`, non-empty local/domain parts, domain contains
/// a `.` with non-empty text on both sides.
fn is_ln_address(value: &str) -> bool {
    if value.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let at_positions: Vec<usize> = value.match_indices('@').map(|(i, _)| i).collect();
    if at_positions.len() != 1 {
        return false;
    }
    let at = at_positions[0];
    if at == 0 || at == value.len() - 1 {
        return false;
    }
    let domain = &value[at + 1..];
    domain
        .char_indices()
        .any(|(i, c)| c == '.' && i > 0 && i < domain.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitcoin_uri_without_query() {
        let p = QrParser::new("bitcoin:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa");
        assert_eq!(p.destination(), Some("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"));
        assert_eq!(p.destination_type(), Some(DestinationType::BitcoinAddress));
        assert_eq!(p.on_chain_encryption_text, None);
        assert_eq!(p.on_chain_encryption_secret, None);
    }

    #[test]
    fn bitcoin_uri_with_branta_zk_params() {
        let p = QrParser::new(
            "bitcoin:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa?branta_id=abc%2Bdef%3D&branta_secret=1234",
        );
        assert_eq!(p.destination(), Some("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"));
        assert_eq!(p.destination_type(), Some(DestinationType::BitcoinAddress));
        assert_eq!(p.on_chain_encryption_text.as_deref(), Some("abc+def="));
        assert_eq!(p.on_chain_encryption_secret.as_deref(), Some("1234"));
        assert!(p.is_on_chain_zk());
    }

    #[test]
    fn bitcoin_uri_with_lightning_query_param_percent_decoded() {
        let p = QrParser::new(
            "bitcoin:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa?lightning=lnbc100n1ptest%3Dpadded",
        );
        assert_eq!(p.destinations.len(), 2);
        assert_eq!(p.destinations[1].value, "lnbc100n1ptest=padded");
        assert_eq!(p.destinations[1].r#type, Some(DestinationType::Bolt11));
    }

    #[test]
    fn plain_bitcoin_address() {
        let p = QrParser::new("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa");
        assert_eq!(p.destination(), Some("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"));
        assert_eq!(p.destination_type(), Some(DestinationType::BitcoinAddress));
    }

    #[test]
    fn lightning_uri_bolt11() {
        let p = QrParser::new("lightning:lnbc100n1ptest");
        assert_eq!(p.destination(), Some("lnbc100n1ptest"));
        assert_eq!(p.destination_type(), Some(DestinationType::Bolt11));
    }

    #[test]
    fn plain_bolt11() {
        let p = QrParser::new("lnbc100n1ptest");
        assert_eq!(p.destination(), Some("lnbc100n1ptest"));
        assert_eq!(p.destination_type(), Some(DestinationType::Bolt11));
    }

    #[test]
    fn lightning_uri_bolt12() {
        let p = QrParser::new("lightning:lno1qcptest");
        assert_eq!(p.destination(), Some("lno1qcptest"));
        assert_eq!(p.destination_type(), Some(DestinationType::Bolt12));
    }

    #[test]
    fn plain_bolt12() {
        let p = QrParser::new("lno1qcptest");
        assert_eq!(p.destination(), Some("lno1qcptest"));
        assert_eq!(p.destination_type(), Some(DestinationType::Bolt12));
    }

    #[test]
    fn lightning_uri_lnurl() {
        let p = QrParser::new("lightning:LNURL1DP68GURN8GHJ");
        assert_eq!(p.destination(), Some("LNURL1DP68GURN8GHJ"));
        assert_eq!(p.destination_type(), Some(DestinationType::LnUrl));
    }

    #[test]
    fn plain_lnurl() {
        let p = QrParser::new("LNURL1DP68GURN8GHJ");
        assert_eq!(p.destination(), Some("LNURL1DP68GURN8GHJ"));
        assert_eq!(p.destination_type(), Some(DestinationType::LnUrl));
    }

    #[test]
    fn plain_ethereum_style_tether_address() {
        let p = QrParser::new("0x742d35Cc6634C0532925a3b844Bc454e4438f44e");
        assert_eq!(
            p.destination(),
            Some("0x742d35Cc6634C0532925a3b844Bc454e4438f44e")
        );
        assert_eq!(p.destination_type(), Some(DestinationType::TetherAddress));
    }

    #[test]
    fn plain_tron_style_tether_address() {
        let p = QrParser::new("TJmUNSGV6b1CCVXN1KkABY49nUJGWDH3Hd");
        assert_eq!(p.destination(), Some("TJmUNSGV6b1CCVXN1KkABY49nUJGWDH3Hd"));
        assert_eq!(p.destination_type(), Some(DestinationType::TetherAddress));
    }

    #[test]
    fn plain_ark_address() {
        let p = QrParser::new("ark1qqjqtest");
        assert_eq!(p.destination(), Some("ark1qqjqtest"));
        assert_eq!(p.destination_type(), Some(DestinationType::ArkAddress));
    }

    #[test]
    fn plain_silent_payment_sp1() {
        let p = QrParser::new("sp1qqwl5p9jhz0000h5zkvlf9gfqv9dl9qjp5ggq5x3fw");
        assert_eq!(p.destination_type(), Some(DestinationType::SilentPayment));
    }

    #[test]
    fn plain_silent_payment_tsp1() {
        let p = QrParser::new("tsp1qqwl5p9jhz0000h5zkvlf9gfqv9dl9qjp5ggq5x3fw");
        assert_eq!(p.destination_type(), Some(DestinationType::SilentPayment));
    }

    #[test]
    fn ln_address_detected() {
        let p = QrParser::new("user@example.com");
        assert_eq!(p.destination_type(), Some(DestinationType::LnAddress));
    }

    #[test]
    fn unrecognized_plain_text_has_no_type() {
        let p = QrParser::new("not-any-known-format");
        assert_eq!(p.destination(), Some("not-any-known-format"));
        assert_eq!(p.destination_type(), None);
    }

    #[test]
    fn whitespace_is_trimmed() {
        let p = QrParser::new("  lnbc100n1ptest  ");
        assert_eq!(p.destination(), Some("lnbc100n1ptest"));
        assert_eq!(p.destination_type(), Some(DestinationType::Bolt11));
    }

    #[test]
    fn combined_bitcoin_and_lightning_qr_with_empty_first_query_segment() {
        let p =
            QrParser::new("bitcoin:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa?&lightning=lnbc100n1ptest");
        assert_eq!(p.destinations.len(), 2);
        assert_eq!(p.destination(), Some("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"));
        assert_eq!(p.destination_type(), Some(DestinationType::BitcoinAddress));
        assert_eq!(p.destinations[1].value, "lnbc100n1ptest");
        assert_eq!(p.destinations[1].r#type, Some(DestinationType::Bolt11));
        assert!(!p.is_on_chain_zk());
    }

    #[test]
    fn combined_bitcoin_lightning_and_ark_qr() {
        let p = QrParser::new(
            "bitcoin:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa?&lightning=lnbc100n1ptest&ark=ark100testaddress",
        );
        assert_eq!(p.destinations.len(), 3);
        assert_eq!(p.destinations[1].value, "lnbc100n1ptest");
        assert_eq!(p.destinations[1].r#type, Some(DestinationType::Bolt11));
        assert_eq!(p.destinations[2].value, "ark100testaddress");
        assert_eq!(p.destinations[2].r#type, Some(DestinationType::ArkAddress));
        assert!(!p.is_on_chain_zk());
    }

    #[test]
    fn combined_bitcoin_and_silent_payment_qr() {
        let p = QrParser::new(
            "bitcoin:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa?silent_payment=sp1qqwl5p9jhz0000h5zkvlf9gfqv9dl9qjp5ggq5x3fw",
        );
        assert_eq!(p.destinations.len(), 2);
        assert_eq!(
            p.destinations[1].value,
            "sp1qqwl5p9jhz0000h5zkvlf9gfqv9dl9qjp5ggq5x3fw"
        );
        assert_eq!(
            p.destinations[1].r#type,
            Some(DestinationType::SilentPayment)
        );
    }

    #[test]
    fn other_uri_scheme_falls_back_to_whole_text_untyped() {
        let p = QrParser::new("http://example.com/pay?amount=1");
        assert_eq!(p.destination(), Some("http://example.com/pay?amount=1"));
        assert_eq!(p.destination_type(), None);
    }

    #[test]
    fn empty_input_does_not_panic() {
        let p = QrParser::new("");
        // Degrades gracefully -- no panic, no type match.
        assert_eq!(p.destination_type(), None);
        assert!(!p.is_on_chain_zk());
    }
}
