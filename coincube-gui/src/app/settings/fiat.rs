use serde::{Deserialize, Serialize};

use crate::chain::ChainId;
use crate::services::fiat::{Currency, PriceSource};
use crate::utils::serde::{deser_fromstr, serialize_display};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PriceSetting {
    #[serde(
        deserialize_with = "deser_fromstr",
        serialize_with = "serialize_display"
    )]
    pub source: PriceSource,
    #[serde(
        deserialize_with = "deser_fromstr",
        serialize_with = "serialize_display"
    )]
    pub currency: Currency,
    pub is_enabled: bool,
}

impl PriceSetting {
    pub(crate) fn matches_display_quote(
        &self,
        chain: ChainId,
        currency: Currency,
        source: PriceSource,
    ) -> bool {
        // BTCB2 quotes come from Connect regardless of the saved Bitcoin provider.
        self.is_enabled
            && self.currency == currency
            && (chain.is_blake2b() || self.source == source)
    }
}

impl Default for PriceSetting {
    fn default() -> Self {
        Self {
            source: PriceSource::default(),
            currency: Currency::default(),
            is_enabled: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_quote_matching_respects_currency_and_chain_provider_policy() {
        let mut setting = PriceSetting {
            is_enabled: true,
            currency: Currency::USD,
            source: PriceSource::CoinGecko,
        };
        assert!(setting.matches_display_quote(
            ChainId::Bitcoin,
            Currency::USD,
            PriceSource::CoinGecko
        ));
        assert!(!setting.matches_display_quote(
            ChainId::Bitcoin,
            Currency::USD,
            PriceSource::Coincube
        ));
        assert!(setting.matches_display_quote(
            ChainId::BitcoinBlake2b,
            Currency::USD,
            PriceSource::Coincube
        ));
        assert!(!setting.matches_display_quote(
            ChainId::BitcoinBlake2b,
            Currency::EUR,
            PriceSource::Coincube
        ));
        setting.is_enabled = false;
        assert!(!setting.matches_display_quote(
            ChainId::BitcoinBlake2b,
            Currency::USD,
            PriceSource::Coincube
        ));
    }
}
