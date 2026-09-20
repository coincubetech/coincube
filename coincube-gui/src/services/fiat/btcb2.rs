//! BTCB2 prices come only from the authenticated Connect aggregate.
//! A stale or unavailable quote is never a Bitcoin/aggregator lookup trigger.
use serde::Deserialize;

use super::{
    api::{GetPriceResult, PriceApiError},
    Currency,
};
use crate::services::{
    coincube::{CoincubeClient, CoincubeError},
    http::ResponseExt,
};

/// Conservative desktop freshness limit, also applied to already-cached quotes.
/// API policy may be stricter; its `stale` flag is always authoritative.
pub const MAX_QUOTE_AGE: u64 = 120;
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Debug, Clone, Deserialize)]
pub struct Btcb2Quote {
    pub price: f64,
    pub fiat: String,
    pub sources: Vec<PriceObservation>,
    pub median: bool,
    pub stale: bool,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceObservation {
    pub name: String,
    pub price: f64,
    pub at: u64,
}

pub fn unix_now() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

pub fn timestamp_fresh(at: u64, now: u64) -> bool {
    at > 0 && at <= now && now - at <= MAX_QUOTE_AGE
}

impl Btcb2Quote {
    pub fn usable_price(
        &self,
        currency: Currency,
        now: u64,
    ) -> Result<GetPriceResult, PriceApiError> {
        let unusable =
            || PriceApiError::CannotParseData("BTCB2 quote is unavailable or stale".into());
        if self.stale
            || !self.median
            || self.fiat != currency.to_string()
            || !self.price.is_finite()
            || self.price <= 0.0
            || self.sources.len() != 2
            || !timestamp_fresh(self.updated_at, now)
        {
            return Err(unusable());
        }
        let first = &self.sources[0];
        let second = &self.sources[1];
        if first.name == second.name
            || self.sources.iter().any(|source| {
                !matches!(source.name.as_str(), "nonkyc" | "neoxa")
                    || !source.price.is_finite()
                    || source.price <= 0.0
                    || !timestamp_fresh(source.at, now)
            })
            || self.updated_at != first.at.min(second.at)
        {
            return Err(unusable());
        }
        // Half-sums avoid overflowing two otherwise finite prices.
        let median = first.price / 2.0 + second.price / 2.0;
        if !median.is_finite()
            || (self.price - median).abs() > median * 1e-12
            || (first.price - second.price).abs() / median > 0.10
        {
            return Err(unusable());
        }
        Ok(GetPriceResult {
            value: self.price,
            updated_at: Some(self.updated_at),
        })
    }
}

#[derive(Deserialize)]
struct QuoteEnvelope {
    success: bool,
    data: Btcb2Quote,
    error: Option<serde_json::Value>,
}

impl CoincubeClient {
    /// Mainnet BTCB2 only. Callers must apply `usable_price` before fiat display.
    pub async fn btcb2_quote(&self, currency: Currency) -> Result<Btcb2Quote, CoincubeError> {
        let response = self
            .client
            .get(format!("{}/api/v1/price/bitcoin-blake2b", self.base_url))
            .query(&[("fiat", currency.to_string())])
            .send()
            .await?
            .check_success()
            .await?;
        let envelope: QuoteEnvelope = response.json().await?;
        if !envelope.success || envelope.error.is_some() {
            return Err(CoincubeError::Api("Invalid BTCB2 pricing response".into()));
        }
        Ok(envelope.data)
    }
}

pub fn request_error(error: CoincubeError) -> PriceApiError {
    match error {
        CoincubeError::Unsuccessful(info) => PriceApiError::NotSuccessResponse(info),
        _ => PriceApiError::RequestFailed("BTCB2 pricing is unavailable".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::{Method::GET, MockServer};
    use serde_json::json;

    fn quote() -> Btcb2Quote {
        Btcb2Quote {
            price: 102.0,
            fiat: "USD".into(),
            median: true,
            stale: false,
            updated_at: 1000,
            sources: vec![
                PriceObservation {
                    name: "nonkyc".into(),
                    price: 100.0,
                    at: 1000,
                },
                PriceObservation {
                    name: "neoxa".into(),
                    price: 104.0,
                    at: 1001,
                },
            ],
        }
    }

    #[test]
    fn quote_matrix_requires_both_sources_and_fresh_matching_aggregate() {
        assert_eq!(
            quote().usable_price(Currency::USD, 1120).unwrap().value,
            102.0
        );
        assert!(quote().usable_price(Currency::USD, 1121).is_err());
        assert!(quote().usable_price(Currency::EUR, 1001).is_err());
        assert!(quote().usable_price(Currency::USD, 999).is_err());
        let variants: [fn(&mut Btcb2Quote); 13] = [
            |q| q.stale = true,
            |q| q.median = false,
            |q| q.price = 100.0,
            |q| q.price = f64::NAN,
            |q| q.price = 0.0,
            |q| {
                q.sources.pop();
            },
            |q| q.sources[0].name = "bitcoin".into(),
            |q| q.sources[0].name = "neoxa".into(),
            |q| q.sources[0].at = 0,
            |q| q.sources[0].at = 1002,
            |q| q.sources[0].price = -1.0,
            |q| q.updated_at = 1001,
            |q| {
                q.sources[0].price = 50.0;
                q.price = 77.0;
            },
        ];
        for mutate in variants {
            let mut q = quote();
            mutate(&mut q);
            assert!(
                q.usable_price(Currency::USD, 1001).is_err(),
                "accepted {:?}",
                q
            );
        }
    }

    #[tokio::test]
    async fn authenticated_client_uses_only_btcb2_route_and_selected_fiat() {
        let server = MockServer::start();
        let endpoint = server.mock(|when, then| {
            when.method(GET).path("/api/v1/price/bitcoin-blake2b")
                .query_param("fiat", "EUR").header("authorization", "Bearer synthetic-price-token");
            then.status(200).json_body(json!({"success": true, "error": null, "data": {
                "price": 102, "fiat": "EUR", "median": true, "stale": false, "updated_at": 1000,
                "sources": [{"name":"nonkyc", "price":100, "at":1000}, {"name":"neoxa", "price":104, "at":1000}]
            }}));
        });
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("synthetic-price-token");
        let quote = client.btcb2_quote(Currency::EUR).await.unwrap();
        endpoint.assert();
        assert_eq!(
            quote.usable_price(Currency::EUR, 1000).unwrap().value,
            102.0
        );
    }

    #[tokio::test]
    async fn endpoint_failures_never_return_a_bitcoin_quote() {
        for status in [401, 404, 429, 503] {
            let server = MockServer::start();
            let endpoint = server.mock(|when, then| {
                when.method(GET).path("/api/v1/price/bitcoin-blake2b");
                then.status(status).json_body(json!({"success":false,"data":null,"error":{"code":"SERVICE_UNAVAILABLE","message":"Unavailable"}}));
            });
            let bitcoin = server.mock(|when, then| {
                when.method(GET).path("/api/v1/exchange-rates/price/USD");
                then.status(200).json_body(json!({"value":99999}));
            });
            let client = CoincubeClient::for_test(server.base_url());
            assert!(matches!(client.btcb2_quote(Currency::USD).await,
                Err(CoincubeError::Unsuccessful(info)) if info.status_code == status));
            endpoint.assert();
            bitcoin.assert_hits(0);
        }
    }
}

#[cfg(test)]
mod malformed_quote_tests {
    use super::*;
    use httpmock::{Method::GET, MockServer};
    use serde_json::json;

    #[tokio::test]
    async fn a_bitcoin_price_body_or_missing_required_field_is_not_a_btcb2_quote() {
        for body in [
            json!({"value": 99000}),
            json!({"success":true,"data":{"price":102,"fiat":"USD"}}),
        ] {
            let server = MockServer::start();
            let endpoint = server.mock(|when, then| {
                when.method(GET).path("/api/v1/price/bitcoin-blake2b");
                then.status(200).json_body(body);
            });
            let client = CoincubeClient::for_test(server.base_url());
            assert!(client.btcb2_quote(Currency::USD).await.is_err());
            endpoint.assert();
        }
    }
}
