//! Positive canonical pre-fork observations may close a search branch. They
//! never become retained proof links or authorize a negative chain conclusion.
use super::*;
use crate::services::coincube::network_anchor::NetworkAnchor;
use std::collections::BTreeMap;

pub(super) struct PruneContext {
    chain: ChainId,
    fork_height: u64,
    tip: Option<BlockRef>,
    anchor: Option<NetworkAnchor>,
    oldest_stamp: Option<i64>,
    frontier: BTreeMap<Txid, BlockRef>,
}

impl PruneContext {
    pub(super) fn new(source: &HttpObservationSource, chain: ChainId) -> Result<Self, FailureKind> {
        if !matches!(chain, ChainId::Bitcoin | ChainId::BitcoinBlake2b) {
            return Err(FailureKind::WrongChain);
        }
        Ok(Self {
            chain,
            fork_height: u64::from(source.ancestry_first_fork_height()?),
            tip: None,
            anchor: None,
            oldest_stamp: None,
            frontier: BTreeMap::new(),
        })
    }

    async fn start(
        &mut self,
        source: &HttpObservationSource,
        policy: Policy,
    ) -> Result<(), FailureKind> {
        if self.anchor.is_some() {
            return Ok(());
        }
        let anchor = super::super::super::checked_ancestry_anchor(
            source.anchor(ChainId::BitcoinBlake2b).await?,
            ChainId::BitcoinBlake2b,
            policy,
            source.now(),
        )
        .map_err(|error| error.kind)?;
        if anchor
            .observation
            .fork
            .as_ref()
            .is_none_or(|fork| fork.height != self.fork_height)
        {
            return Err(FailureKind::WrongChain);
        }
        let (tip, tip_stamp) = super::super::super::read(
            source.tip(self.chain).await?,
            self.chain,
            source,
            policy,
            Stage::BitcoinInclusion,
        )
        .map_err(|error| error.kind)?;
        if self.chain == ChainId::BitcoinBlake2b
            && (tip.height != anchor.tip_height || tip.hash != anchor.tip_hash)
        {
            return Err(FailureKind::Changed);
        }
        let oldest_stamp = tip_stamp.min(anchor.observed_at);
        self.tip = Some(tip);
        self.anchor = Some(anchor);
        self.oldest_stamp = Some(oldest_stamp);
        Ok(())
    }

    async fn confirmed_block(
        &self,
        source: &HttpObservationSource,
        txid: Txid,
        policy: Policy,
    ) -> Result<(BlockRef, i64), FailureKind> {
        self.confirmed_block_from_read(
            source,
            txid,
            source.transaction(self.chain, txid).await?,
            policy,
        )
    }

    fn confirmed_block_from_read(
        &self,
        source: &HttpObservationSource,
        expected_txid: Txid,
        transaction: FreshRead<TransactionObservation>,
        policy: Policy,
    ) -> Result<(BlockRef, i64), FailureKind> {
        let (transaction, stamp) = super::super::super::read(
            transaction,
            self.chain,
            source,
            policy,
            Stage::BitcoinInclusion,
        )
        .map_err(|error| error.kind)?;
        let TransactionObservation::Confirmed { txid, block } = transaction else {
            return Err(FailureKind::Changed);
        };
        if txid != expected_txid {
            return Err(FailureKind::Malformed);
        }
        if self.tip.is_some_and(|tip| block.height > tip.height) {
            return Err(FailureKind::Changed);
        }
        Ok((block, stamp))
    }

    #[cfg(test)]
    pub(super) async fn inspect(
        &mut self,
        source: &HttpObservationSource,
        txid: Txid,
        policy: Policy,
    ) -> Result<bool, FailureKind> {
        let transaction = source.transaction(self.chain, txid).await?;
        self.inspect_prefetched(source, txid, policy, transaction)
            .await
    }

    pub(super) async fn inspect_prefetched(
        &mut self,
        source: &HttpObservationSource,
        txid: Txid,
        policy: Policy,
        transaction: FreshRead<TransactionObservation>,
    ) -> Result<bool, FailureKind> {
        // The caller has already fetched raw bytes under this txid; Search
        // checks their identity and every referenced output before advancing.
        let (block, stamp) = self.confirmed_block_from_read(source, txid, transaction, policy)?;
        if block.height >= self.fork_height {
            return Ok(false);
        }
        self.start(source, policy).await?;
        if self.tip.is_some_and(|tip| block.height > tip.height) {
            return Err(FailureKind::Changed);
        }
        self.oldest_stamp = Some(self.oldest_stamp.unwrap().min(stamp));
        let (hash, hash_stamp) = super::super::super::read(
            source.hash_at_height(self.chain, block.height).await?,
            self.chain,
            source,
            policy,
            Stage::BitcoinInclusion,
        )
        .map_err(|error| error.kind)?;
        if hash != block.hash {
            return Err(FailureKind::Changed);
        }
        self.oldest_stamp = Some(self.oldest_stamp.unwrap().min(hash_stamp));
        self.frontier.insert(txid, block);
        Ok(true)
    }

    pub(super) async fn finish(
        &self,
        source: &HttpObservationSource,
        policy: Policy,
    ) -> Result<(), FailureKind> {
        let Some(anchor) = &self.anchor else {
            return Ok(());
        };
        let mut oldest_stamp = self.oldest_stamp.unwrap();
        for (&txid, &before) in &self.frontier {
            let (block, stamp) = self.confirmed_block(source, txid, policy).await?;
            oldest_stamp = oldest_stamp.min(stamp);
            if block != before || block.height >= self.fork_height {
                return Err(FailureKind::Changed);
            }
            let (hash, hash_stamp) = super::super::super::read(
                source.hash_at_height(self.chain, block.height).await?,
                self.chain,
                source,
                policy,
                Stage::BitcoinInclusion,
            )
            .map_err(|error| error.kind)?;
            if hash != block.hash {
                return Err(FailureKind::Changed);
            }
            oldest_stamp = oldest_stamp.min(hash_stamp);
        }
        let (tip, tip_stamp) = super::super::super::read(
            source.tip(self.chain).await?,
            self.chain,
            source,
            policy,
            Stage::BitcoinInclusion,
        )
        .map_err(|error| error.kind)?;
        if Some(tip) != self.tip {
            return Err(FailureKind::Changed);
        }
        oldest_stamp = oldest_stamp.min(tip_stamp);
        let after = super::super::super::checked_ancestry_anchor(
            source.anchor(ChainId::BitcoinBlake2b).await?,
            ChainId::BitcoinBlake2b,
            policy,
            source.now(),
        )
        .map_err(|error| error.kind)?;
        if after.tip_hash != anchor.tip_hash
            || after.tip_height != anchor.tip_height
            || after.tip_median_time_past != anchor.tip_median_time_past
            || after.observation != anchor.observation
            || !session_current(&source.generation, source.expected)
        {
            return Err(FailureKind::Changed);
        }
        oldest_stamp = oldest_stamp.min(after.observed_at);
        if !super::super::super::fresh(
            oldest_stamp,
            source.now(),
            policy.max_observation_age_seconds,
        ) {
            return Err(FailureKind::Stale);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::tests::{ancestry_fixture, source as make_source};
    use super::*;
    use httpmock::prelude::*;
    use reqwest::header::HeaderValue;
    use serde_json::json;
    use std::str::FromStr;

    #[tokio::test]
    async fn prefetched_metadata_must_match_the_requested_node_before_pruning() {
        let server = MockServer::start();
        let (source, _sender) = make_source(&server);
        let (expected, _) = ancestry_fixture();
        let other = Txid::from_str(&"99".repeat(32)).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-cache", HeaderValue::from_static("BYPASS"));
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        let metadata = FreshRead::from_response(
            ChainId::Bitcoin,
            TransactionObservation::Confirmed {
                txid: other,
                block: BlockRef {
                    height: 961_600,
                    hash: BlockHash::from_str(&"88".repeat(32)).unwrap(),
                },
            },
            source.now(),
            &headers,
        )
        .unwrap();
        let mut pruning = PruneContext::new(&source, ChainId::Bitcoin).unwrap();
        assert_eq!(
            pruning
                .inspect_prefetched(
                    &source,
                    expected,
                    Policy {
                        max_observation_age_seconds: 90,
                        expiry_margin_seconds: 600,
                    },
                    metadata
                )
                .await,
            Err(FailureKind::Malformed)
        );
        assert!(pruning.anchor.is_none());
        assert!(pruning.frontier.is_empty());
    }

    #[tokio::test]
    async fn frontier_recheck_rejects_reorg_cancelled_session_and_aged_bracket() {
        for scenario in ["reorg", "cancelled", "aged"] {
            let server = MockServer::start();
            let (source, sender) = make_source(&server);
            let (txid, raw) = ancestry_fixture();
            let base = "/api/v1/esplora/bitcoin/mainnet";
            let tip_hash = "aa".repeat(32);
            let frontier_hash = "bb".repeat(32);
            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v1/connect/networks/bitcoin-blake2b/anchor");
                then.status(200).json_body(json!({"success":true,"data":{
                    "network":"bitcoin-blake2b","state":"available","anchor":{
                        "tip_hash":tip_hash,"tip_height":961700,
                        "tip_median_time_past":10000,"observed_at":source.now(),
                        "observation":{"tip_height":961700,"fork":{"height":961640,"active":true},
                            "rdts":{"state":"unsupported"}}
                    }
                }}));
            });
            let fresh = |path: String, body: String| {
                server.mock(|when, then| {
                    when.method(GET).path(path);
                    then.status(200)
                        .header("x-cache", "BYPASS")
                        .header("cache-control", "no-store")
                        .header("x-coincube-observation", "fresh")
                        .body(body);
                })
            };
            fresh(format!("{base}/blocks/tip/hash"), tip_hash.clone());
            fresh(
                format!("{base}/block/{tip_hash}/status"),
                json!({"in_best_chain":true,"height":961700}).to_string(),
            );
            fresh(format!("{base}/block-height/961700"), tip_hash.clone());
            let mut mapping = fresh(format!("{base}/block-height/961600"), frontier_hash.clone());
            fresh(
                format!("{base}/tx/{txid}"),
                json!({"txid":txid,"status":{"confirmed":true,"block_height":961600,
                    "block_hash":frontier_hash}})
                .to_string(),
            );
            server.mock(|when, then| {
                when.method(GET).path(format!("{base}/tx/{txid}/hex"));
                then.status(200).body(hex::encode(&raw));
            });
            assert_eq!(
                source
                    .ancestry_transaction(ChainId::Bitcoin, txid)
                    .await
                    .unwrap(),
                raw
            );
            let mut pruning = PruneContext::new(&source, ChainId::Bitcoin).unwrap();
            assert!(pruning
                .inspect(
                    &source,
                    txid,
                    Policy {
                        max_observation_age_seconds: 60,
                        expiry_margin_seconds: 600,
                    }
                )
                .await
                .unwrap());
            if scenario == "reorg" {
                mapping.delete();
                fresh(format!("{base}/block-height/961600"), "cc".repeat(32));
            } else if scenario == "cancelled" {
                sender.send_replace(5);
            } else {
                // Model time passing beyond the 60-second policy while the
                // final HTTP reads themselves remain fresh.
                pruning.oldest_stamp = Some(source.now() - 120);
            }
            let result = pruning
                .finish(
                    &source,
                    Policy {
                        max_observation_age_seconds: 60,
                        expiry_margin_seconds: 600,
                    },
                )
                .await;
            assert_eq!(
                result,
                Err(match scenario {
                    "reorg" => FailureKind::Changed,
                    "cancelled" => FailureKind::Cancelled,
                    _ => FailureKind::Stale,
                })
            );
        }
    }
}
