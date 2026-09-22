//! Anonymous fresh Esplora reads and a separate authenticated anchor context.
use super::*;
use crate::services::coincube::{network_anchor::AnchorStartupError, CoincubeClient};
use serde::Deserialize;
use std::{
    convert::TryFrom,
    future::Future,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

const BODY_LIMIT: usize = 256 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Immutable snapshot. Recreate on account/provider change; revoke its generation
/// before dropping old consumers. No inherited authenticated client headers are
/// copied into the anonymous transport, and redirects never follow transaction URLs.
pub struct HttpObservationSource {
    authenticated: CoincubeClient,
    anonymous: reqwest::Client,
    base: String,
    generation: watch::Receiver<u64>,
    expected: u64,
}
impl HttpObservationSource {
    pub fn new(
        client: CoincubeClient,
        bitcoin: ChainId,
        fork: ChainId,
        context: CollectionContext,
    ) -> Result<Self, FailureKind> {
        // API has no Bitcoin testnet4 route. Legacy testnet is not interchangeable.
        if (bitcoin, fork) != (ChainId::Bitcoin, ChainId::BitcoinBlake2b) {
            return Err(FailureKind::WrongChain);
        }
        let url = reqwest::Url::parse(&client.base_url).map_err(|_| FailureKind::Malformed)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(FailureKind::Malformed);
        }
        if client.token().is_none_or(str::is_empty) {
            return Err(FailureKind::Http(401));
        }
        let anonymous = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| FailureKind::Unavailable)?;
        Ok(Self {
            base: url.as_str().trim_end_matches('/').to_owned(),
            authenticated: client,
            anonymous,
            expected: context.expected_generation,
            generation: context.generation,
        })
    }
    fn prefix(chain: ChainId) -> Result<&'static str, FailureKind> {
        match chain {
            ChainId::Bitcoin => Ok("bitcoin/mainnet"),
            ChainId::BitcoinBlake2b => Ok("bitcoin-blake2b/mainnet"),
            _ => Err(FailureKind::WrongChain),
        }
    }
    async fn bounded<T>(
        &self,
        future: impl Future<Output = Result<T, FailureKind>>,
    ) -> Result<T, FailureKind> {
        let mut generation = self.generation.clone();
        if *generation.borrow() != self.expected || generation.has_changed().is_err() {
            return Err(FailureKind::Cancelled);
        }
        let cancelled = async {
            loop {
                if generation.changed().await.is_err()
                    || *generation.borrow_and_update() != self.expected
                {
                    break;
                }
            }
        };
        let result = tokio::select! { biased;
            _ = cancelled => Err(FailureKind::Cancelled),
            result = tokio::time::timeout(REQUEST_TIMEOUT, future) => result.map_err(|_| FailureKind::Deadline)?,
        };
        if *generation.borrow() != self.expected || generation.has_changed().is_err() {
            return Err(FailureKind::Cancelled);
        }
        result
    }
    async fn get(
        &self,
        chain: ChainId,
        path: &str,
    ) -> Result<(u16, Vec<u8>, HeaderMap, i64), FailureKind> {
        let prefix = Self::prefix(chain)?;
        self.bounded(async {
            let stamp = self.now();
            let mut response = self
                .anonymous
                .get(format!("{}/api/v1/esplora/{}/{}", self.base, prefix, path))
                .header("X-Coincube-Observation", "fresh")
                .header(CACHE_CONTROL, "no-cache")
                .send()
                .await
                .map_err(|_| FailureKind::Unavailable)?;
            let status = response.status().as_u16();
            if status != 200 && status != 404 {
                return Err(FailureKind::Http(status));
            }
            let headers = response.headers().clone();
            let mut markers = headers.get_all("x-coincube-observation").iter();
            if markers.next().and_then(|v| v.to_str().ok()) != Some("fresh")
                || markers.next().is_some()
            {
                return Err(FailureKind::FreshnessUnverified);
            }
            FreshRead::from_response(chain, (), stamp, &headers)?;
            if response
                .content_length()
                .is_some_and(|n| n > BODY_LIMIT as u64)
            {
                return Err(FailureKind::Malformed);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| FailureKind::Unavailable)?
            {
                if chunk.len() > BODY_LIMIT.saturating_sub(bytes.len()) {
                    return Err(FailureKind::Malformed);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok((status, bytes, headers, stamp))
        })
        .await
    }
    async fn hash(&self, chain: ChainId, path: &str) -> Result<FreshRead<BlockHash>, FailureKind> {
        let (status, bytes, headers, stamp) = self.get(chain, path).await?;
        if status != 200 {
            return Err(FailureKind::Http(status));
        }
        let value = std::str::from_utf8(&bytes)
            .map_err(|_| FailureKind::Malformed)?
            .trim();
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(FailureKind::Malformed);
        }
        FreshRead::from_response(
            chain,
            BlockHash::from_str(value).map_err(|_| FailureKind::Malformed)?,
            stamp,
            &headers,
        )
    }
}
#[derive(Deserialize)]
struct BlockStatus {
    in_best_chain: bool,
    height: Option<u32>,
}
#[derive(Deserialize)]
struct TxStatus {
    confirmed: bool,
    block_height: Option<u32>,
    block_hash: Option<BlockHash>,
}
#[derive(Deserialize)]
struct TransactionInfo {
    txid: Txid,
    status: TxStatus,
}
#[async_trait]
impl ObservationSource for HttpObservationSource {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|v| i64::try_from(v.as_secs()).ok())
            .unwrap_or(-1)
    }
    async fn anchor(&self, chain: ChainId) -> Result<NetworkAnchorStatus, FailureKind> {
        if chain != ChainId::BitcoinBlake2b {
            return Err(FailureKind::WrongChain);
        }
        self.bounded(async {
            self.authenticated.network_anchor(chain).await.map_err(|e| {
                match AnchorStartupError::from(e) {
                    AnchorStartupError::Http(status) => FailureKind::Http(status),
                    AnchorStartupError::Transport => FailureKind::Unavailable,
                    _ => FailureKind::Malformed,
                }
            })
        })
        .await
    }
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        let before = self.hash(chain, "blocks/tip/hash").await?;
        let (status, bytes, headers, stamp) = self
            .get(chain, &format!("block/{}/status", before.value))
            .await?;
        if status != 200 {
            return Err(FailureKind::Http(status));
        }
        let block: BlockStatus =
            serde_json::from_slice(&bytes).map_err(|_| FailureKind::Malformed)?;
        if !block.in_best_chain {
            return Err(FailureKind::Changed);
        }
        let height = block.height.ok_or(FailureKind::Malformed)?;
        let at_height = self.hash_at_height(chain, u64::from(height)).await?;
        let after = self.hash(chain, "blocks/tip/hash").await?;
        if before.value != after.value || before.value != at_height.value {
            return Err(FailureKind::Changed);
        }
        FreshRead::from_response(
            chain,
            BlockRef {
                height: u64::from(height),
                hash: before.value,
            },
            stamp
                .min(before.observed_at)
                .min(after.observed_at)
                .min(at_height.observed_at),
            &headers,
        )
    }
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        // /tx includes txid; /status alone cannot bind a body to the requested tx.
        let (status, bytes, headers, stamp) = self.get(chain, &format!("tx/{}", txid)).await?;
        let value = if status == 404 {
            TransactionObservation::Absent
        } else {
            let tx: TransactionInfo =
                serde_json::from_slice(&bytes).map_err(|_| FailureKind::Malformed)?;
            if tx.txid != txid {
                return Err(FailureKind::Malformed);
            }
            match (
                tx.status.confirmed,
                tx.status.block_height,
                tx.status.block_hash,
            ) {
                (false, None, None) => TransactionObservation::Unconfirmed { txid },
                (true, Some(height), Some(hash)) => TransactionObservation::Confirmed {
                    txid,
                    block: BlockRef {
                        height: u64::from(height),
                        hash,
                    },
                },
                _ => return Err(FailureKind::Malformed),
            }
        };
        FreshRead::from_response(chain, value, stamp, &headers)
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        let height = u32::try_from(height).map_err(|_| FailureKind::Malformed)?;
        self.hash(chain, &format!("block-height/{}", height)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use serde_json::json;

    fn source(server: &MockServer) -> (HttpObservationSource, watch::Sender<u64>) {
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("synthetic-observation-token");
        let (sender, generation) = watch::channel(4);
        (
            HttpObservationSource::new(
                client,
                ChainId::Bitcoin,
                ChainId::BitcoinBlake2b,
                CollectionContext {
                    expected_generation: 4,
                    generation,
                },
            )
            .unwrap(),
            sender,
        )
    }
    fn id() -> Txid {
        Txid::from_str(&"11".repeat(32)).unwrap()
    }
    #[tokio::test]
    async fn anonymous_exact_route_and_requested_txid_are_preserved() {
        let server = MockServer::start();
        let (source, _sender) = source(&server);
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path(format!(
                    "/api/v1/esplora/bitcoin-blake2b/mainnet/tx/{}",
                    id()
                ))
                .header("x-coincube-observation", "fresh")
                .header("cache-control", "no-cache")
                .matches(|request| {
                    request.headers.as_ref().is_none_or(|headers| {
                        headers.iter().all(|(name, _)| {
                            ![
                                "authorization",
                                "cookie",
                                "x-device-fingerprint",
                                "x-device-name",
                            ]
                            .iter()
                            .any(|forbidden| name.eq_ignore_ascii_case(forbidden))
                        })
                    })
                });
            then.status(200)
                .header("X-Coincube-Observation", "fresh")
                .header("X-Cache", "BYPASS")
                .header("Cache-Control", "no-store")
                .json_body(json!({"txid":id(),"status":{"confirmed":false}}));
        });
        let result = source
            .transaction(ChainId::BitcoinBlake2b, id())
            .await
            .unwrap();
        assert_eq!(result.chain, ChainId::BitcoinBlake2b);
        assert_eq!(
            result.value,
            TransactionObservation::Unconfirmed { txid: id() }
        );
        assert!(result.observed_at <= source.now());
        mock.assert();
    }
    #[tokio::test]
    async fn failures_and_missing_markers_never_become_absence() {
        for (status, marker, body, expected) in [
            (404, true, "".to_owned(), None),
            (
                404,
                false,
                "".to_owned(),
                Some(FailureKind::FreshnessUnverified),
            ),
            (503, true, "".to_owned(), Some(FailureKind::Http(503))),
            (
                200,
                true,
                "not-json".to_owned(),
                Some(FailureKind::Malformed),
            ),
            (
                200,
                true,
                json!({"txid":"22".repeat(32),"status":{"confirmed":false}}).to_string(),
                Some(FailureKind::Malformed),
            ),
            (
                200,
                true,
                "x".repeat(BODY_LIMIT + 1),
                Some(FailureKind::Malformed),
            ),
        ] {
            let server = MockServer::start();
            let (source, _sender) = source(&server);
            let mock = server.mock(|when, then| {
                when.method(GET).path(format!(
                    "/api/v1/esplora/bitcoin-blake2b/mainnet/tx/{}",
                    id()
                ));
                let then = then
                    .status(status)
                    .header("X-Cache", "BYPASS")
                    .header("Cache-Control", "no-store")
                    .body(body);
                if marker {
                    then.header("X-Coincube-Observation", "fresh");
                }
            });
            match expected {
                Some(error) => assert_eq!(
                    source
                        .transaction(ChainId::BitcoinBlake2b, id())
                        .await
                        .unwrap_err(),
                    error
                ),
                None => assert_eq!(
                    source
                        .transaction(ChainId::BitcoinBlake2b, id())
                        .await
                        .unwrap()
                        .value,
                    TransactionObservation::Absent
                ),
            }
            mock.assert_hits(1);
        }
    }
    #[tokio::test]
    async fn tip_is_hash_height_bound_and_rechecks_the_tip() {
        for wrong in [false, true] {
            let server = MockServer::start();
            let (source, _sender) = source(&server);
            let hash = "33".repeat(32);
            let tip = server.mock(|when, then| {
                when.path("/api/v1/esplora/bitcoin/mainnet/blocks/tip/hash");
                then.status(200)
                    .header("X-Coincube-Observation", "fresh")
                    .header("X-Cache", "BYPASS")
                    .header("Cache-Control", "no-store")
                    .body(&hash);
            });
            let metadata = server.mock(|when, then| {
                when.path(format!(
                    "/api/v1/esplora/bitcoin/mainnet/block/{}/status",
                    hash
                ));
                then.status(200)
                    .header("X-Coincube-Observation", "fresh")
                    .header("X-Cache", "BYPASS")
                    .header("Cache-Control", "no-store")
                    .json_body(json!({"in_best_chain":true,"height":105}));
            });
            let at_height = server.mock(|when, then| {
                when.path("/api/v1/esplora/bitcoin/mainnet/block-height/105");
                then.status(200)
                    .header("X-Coincube-Observation", "fresh")
                    .header("X-Cache", "BYPASS")
                    .header("Cache-Control", "no-store")
                    .body(if wrong { "44".repeat(32) } else { hash.clone() });
            });
            let result = source.tip(ChainId::Bitcoin).await;
            if wrong {
                assert_eq!(result.unwrap_err(), FailureKind::Changed);
            } else {
                assert_eq!(
                    result.unwrap().value,
                    BlockRef {
                        height: 105,
                        hash: BlockHash::from_str(&hash).unwrap()
                    }
                );
            }
            tip.assert_hits(2);
            metadata.assert_hits(1);
            at_height.assert_hits(1);
        }
    }
    #[tokio::test]
    async fn unsupported_pair_revocation_and_wrong_chain_refuse_without_io() {
        let server = MockServer::start();
        let (source, sender) = source(&server);
        let any = server.mock(|_, then| {
            then.status(500);
        });
        assert_eq!(
            source.tip(ChainId::Testnet4).await.unwrap_err(),
            FailureKind::WrongChain
        );
        assert_eq!(
            source.anchor(ChainId::Bitcoin).await.unwrap_err(),
            FailureKind::WrongChain
        );
        let client = source.authenticated.clone();
        let (_, generation) = watch::channel(4);
        assert!(matches!(
            HttpObservationSource::new(
                client,
                ChainId::Testnet4,
                ChainId::BitcoinBlake2bTestnet4,
                CollectionContext {
                    expected_generation: 4,
                    generation
                }
            ),
            Err(FailureKind::WrongChain)
        ));
        sender.send(5).unwrap();
        assert_eq!(
            source
                .transaction(ChainId::Bitcoin, id())
                .await
                .unwrap_err(),
            FailureKind::Cancelled
        );
        any.assert_hits(0);
    }
    #[tokio::test]
    async fn anchor_401_and_redirects_remain_errors_without_following() {
        for status in [401, 302] {
            let server = MockServer::start();
            let (source, _sender) = source(&server);
            let anchor = server.mock(|when, then| {
                when.path("/api/v1/connect/networks/bitcoin-blake2b/anchor")
                    .header("authorization", "Bearer synthetic-observation-token");
                then.status(status)
                    .header("Location", format!("{}/must-not-follow", server.base_url()));
            });
            let redirected = server.mock(|when, then| {
                when.path("/must-not-follow");
                then.status(200);
            });
            assert_eq!(
                source.anchor(ChainId::BitcoinBlake2b).await.unwrap_err(),
                FailureKind::Http(status)
            );
            anchor.assert_hits(1);
            redirected.assert_hits(0);
        }
    }
    #[tokio::test]
    async fn anonymous_redirect_never_reaches_a_second_endpoint() {
        let server = MockServer::start();
        let (source, _sender) = source(&server);
        let redirect = server.mock(|when, then| {
            when.path(format!(
                "/api/v1/esplora/bitcoin-blake2b/mainnet/tx/{}",
                id()
            ));
            then.status(302).header(
                "Location",
                format!("{}/bitcoin-fallback", server.base_url()),
            );
        });
        let fallback = server.mock(|when, then| {
            when.path("/bitcoin-fallback");
            then.status(200);
        });
        assert_eq!(
            source
                .transaction(ChainId::BitcoinBlake2b, id())
                .await
                .unwrap_err(),
            FailureKind::Http(302)
        );
        redirect.assert_hits(1);
        fallback.assert_hits(0);
    }

    #[tokio::test]
    async fn cancellation_drops_inflight_http_future() {
        let server = MockServer::start();
        let (source, sender) = source(&server);
        server.mock(|when, then| {
            when.path(format!("/api/v1/esplora/bitcoin/mainnet/tx/{}", id()));
            then.status(200).delay(Duration::from_secs(2));
        });
        let revoke = async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            sender.send(5).unwrap();
        };
        let (result, ()) = tokio::join!(source.transaction(ChainId::Bitcoin, id()), revoke);
        assert_eq!(result.unwrap_err(), FailureKind::Cancelled);
    }
}
