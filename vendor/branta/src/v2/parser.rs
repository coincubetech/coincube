//! QR and pasted-text parsing. Infallible: an unrecognized value has no type.

use std::collections::HashMap;

use percent_encoding::percent_decode_str;

use crate::enums::DestinationType;
use crate::extensions::{derived_key_type, is_ark, is_bolt11, is_silent_payment};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QrDestination {
    pub value: String,
    pub r#type: Option<DestinationType>,
}

#[derive(Debug, Clone)]
pub(crate) struct QrParser {
    pub destinations: Vec<QrDestination>,
    pub on_chain_encryption_text: String,
    pub on_chain_encryption_secret: String,
    pub has_unverifiable_instruction: bool,
}

impl QrParser {
    pub fn new(qr_text: &str) -> Self {
        let text = normalize_qr_text(qr_text);
        let mut parser = Self {
            destinations: Vec::new(),
            on_chain_encryption_text: String::new(),
            on_chain_encryption_secret: String::new(),
            has_unverifiable_instruction: false,
        };

        let Some(scheme) = uri_scheme(&text) else {
            parser.destinations.push(QrDestination {
                r#type: detect_plain_text_type(&text),
                value: text,
            });
            return parser;
        };

        let scheme_lower = scheme.to_ascii_lowercase();
        if scheme_lower != "bitcoin" && scheme_lower != "lightning" {
            parser.destinations.push(QrDestination {
                value: text,
                r#type: None,
            });
            return parser;
        }

        let dest_value = extract_destination(&text);
        let dest_type = if scheme_lower == "bitcoin" {
            Some(DestinationType::BitcoinAddress)
        } else {
            lightning_scheme_type(&dest_value)
        };
        if !dest_value.is_empty()
            && dest_type != Some(DestinationType::BitcoinAddress)
            && derived_key_type(&dest_value).is_none()
        {
            parser.has_unverifiable_instruction = true;
        }
        parser.destinations.push(QrDestination {
            value: dest_value,
            r#type: dest_type,
        });

        let query = parse_query(&extract_query(&text));
        parser.on_chain_encryption_text = query.get("branta_id").cloned().unwrap_or_default();
        parser.on_chain_encryption_secret = query.get("branta_secret").cloned().unwrap_or_default();

        let mut add_bindable = |key: &str, on_chain: bool| {
            let Some(value) = query.get(key) else {
                return;
            };
            let dest_type = if on_chain {
                Some(DestinationType::BitcoinAddress)
            } else {
                detect_plain_text_type(value)
            };
            if !value.is_empty() && !on_chain && derived_key_type(value).is_none() {
                parser.has_unverifiable_instruction = true;
            }
            parser.destinations.push(QrDestination {
                value: value.clone(),
                r#type: dest_type,
            });
        };

        add_bindable("lightning", false);
        add_bindable("bolt12", false);
        add_bindable("ark", false);
        add_bindable("silent_payment", false);
        add_bindable("bc", true);
        add_bindable("tb", true);
        add_bindable("sp", false);
        add_bindable("lno", false);
        if query.get("pay").is_some_and(|value| !value.is_empty()) {
            parser.has_unverifiable_instruction = true;
        }
        parser
    }

    pub fn destination(&self) -> Option<&str> {
        self.destinations
            .first()
            .map(|destination| destination.value.as_str())
    }

    pub fn is_on_chain_zk(&self) -> bool {
        !self.on_chain_encryption_text.is_empty() && !self.on_chain_encryption_secret.is_empty()
    }
}

pub(crate) fn detect_plain_text_type(value: &str) -> Option<DestinationType> {
    if is_bolt11(value) {
        return Some(DestinationType::Bolt11);
    }
    if is_ark(value) {
        return Some(DestinationType::ArkAddress);
    }
    if is_silent_payment(value) {
        return Some(DestinationType::SilentPayment);
    }
    let lower = value.to_ascii_lowercase();
    if value.starts_with('1')
        || value.starts_with('3')
        || lower.starts_with("bc1")
        || lower.starts_with("tb1")
        || lower.starts_with("bcrt1")
    {
        return Some(DestinationType::BitcoinAddress);
    }
    None
}

fn lightning_scheme_type(dest: &str) -> Option<DestinationType> {
    if is_bolt11(dest) {
        Some(DestinationType::Bolt11)
    } else {
        None
    }
}

fn normalize_qr_text(qr_text: &str) -> String {
    let value = qr_text.trim().trim_matches(|c: char| (c as u32) <= 0x20);
    value
        .chars()
        .filter(|c| *c != '\t' && *c != '\n' && *c != '\r')
        .collect()
}

fn uri_scheme(text: &str) -> Option<&str> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let mut end = first.len_utf8();
    for c in chars {
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.') {
            end += c.len_utf8();
            continue;
        }
        break;
    }
    if text[end..].starts_with(':') {
        Some(&text[..end])
    } else {
        None
    }
}

fn extract_destination(text: &str) -> String {
    let after = text.split_once(':').map(|(_, after)| after).unwrap_or("");
    after.split('?').next().unwrap_or("").to_string()
}

fn extract_query(text: &str) -> String {
    let after = text.split_once(':').map(|(_, after)| after).unwrap_or("");
    after
        .split_once('?')
        .map(|(_, query)| query.to_string())
        .unwrap_or_default()
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key.is_empty() {
            continue;
        }
        let decoded_key = percent_decode_str(key).decode_utf8_lossy().to_string();
        let decoded_value = percent_decode_str(value).decode_utf8_lossy().to_string();
        let lowered = decoded_key.to_ascii_lowercase();
        map.entry(lowered).or_insert(decoded_value);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tb1_and_bcrt1_are_bitcoin() {
        assert_eq!(
            QrParser::new("tb1qexample").destinations[0].r#type,
            Some(DestinationType::BitcoinAddress)
        );
        assert_eq!(
            QrParser::new("bcrt1qexample").destinations[0].r#type,
            Some(DestinationType::BitcoinAddress)
        );
    }

    #[test]
    fn control_character_inside_scheme_still_types_the_address() {
        let parser = QrParser::new("bitcoi\tn:bc1qmerchant");
        assert_eq!(parser.destination(), Some("bc1qmerchant"));
        assert_eq!(
            parser.destinations[0].r#type,
            Some(DestinationType::BitcoinAddress)
        );
    }

    #[test]
    fn duplicate_query_key_keeps_the_first_value() {
        let parser =
            QrParser::new("bitcoin:bc1qreal?branta_secret=first&branta_secret=second&branta_id=id");
        assert_eq!(parser.on_chain_encryption_secret, "first");
        assert_eq!(parser.on_chain_encryption_text, "id");
    }

    #[test]
    fn bolt12_lno_and_lnurl_are_unverifiable() {
        assert!(QrParser::new("bitcoin:bc1qmerchant?lno=lno1offer").has_unverifiable_instruction);
        assert!(QrParser::new("lightning:LNURL1DP68GURN8GHJ").has_unverifiable_instruction);
        assert!(QrParser::new("lno1offer").destinations[0].r#type.is_none());
    }
}
