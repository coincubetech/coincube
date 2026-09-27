//! Mainnet-only root-pair collection. No spend authorization is produced.
use super::*;

// Pinned Knots v29.4.1.knots20260508 src/kernel/chainparams.cpp (mainnet)
// and src/validation.cpp BIP34_IMPLIES_BIP30_LIMIT. The known pre-BIP34
// history has no future-height prefix in this interval. Do not generalize
// this to arbitrary histories, regtest, the 490897 exception, or >=1983702.
const FIRST_FORK_HEIGHT: u32 = 961_640;
const HISTORICAL_LIMIT: u32 = 1_983_702;
const BIP34_HEIGHT: u64 = 227_931;
const BIP34_HASH: &str = "000000000000024b89b42a942fe0d9fea3bb44ab7bd1b19115dd6a759c0808b8";

/// Matching current mainnet histories with distinct same-height coinbase roots.
/// Still only provider-trusted observations: the selected output's ownership,
/// maturity, spendability, complete ancestry path and submission policy remain
/// separate. Recollect after restart/reorg and recheck generation before use.
#[derive(Debug)]
pub struct CoinbasePair {
    selected: OutPoint,
    bitcoin: CanonicalCoinbase,
    fork: CanonicalCoinbase,
    anchor: super::super::super::NetworkAnchor,
    observed_at: i64,
    generation: u64,
}
impl CoinbasePair {
    pub fn selected(&self) -> OutPoint {
        self.selected
    }
    pub fn bitcoin(&self) -> &CanonicalCoinbase {
        &self.bitcoin
    }
    pub fn fork(&self) -> &CanonicalCoinbase {
        &self.fork
    }
    pub fn anchor(&self) -> &super::super::super::NetworkAnchor {
        &self.anchor
    }
    pub fn observed_at(&self) -> i64 {
        self.observed_at
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl HttpObservationSource {
    /// Bind a previously verified dependency to positive observations from both
    /// mainnet chains and their known common pre-BIP34 history. No negative
    /// transaction lookup is used. The complete collection shares a 30s budget.
    pub async fn coinbase_pair(
        &self,
        dependency: &claim_ancestry::CoinbaseDependency,
        height: u32,
        policy: Policy,
    ) -> Result<CoinbasePair, FailureKind> {
        if !(FIRST_FORK_HEIGHT..HISTORICAL_LIMIT).contains(&height) {
            return Err(FailureKind::UnsupportedPoison);
        }
        dependency
            .check_height_commitment(height)
            .map_err(|_| FailureKind::Malformed)?;
        tokio::time::timeout(MAX_COLLECTION_TIME, async {
            let fork_chain = ChainId::BitcoinBlake2b;
            let before = super::super::super::checked_anchor(
                self.anchor(fork_chain).await?,
                fork_chain,
                policy,
                self.now(),
            )
            .map_err(|e| e.kind)?;
            if before
                .observation
                .fork
                .as_ref()
                .is_none_or(|fork| fork.height != u64::from(FIRST_FORK_HEIGHT))
            {
                return Err(FailureKind::WrongChain);
            }
            let expected_history =
                BlockHash::from_str(BIP34_HASH).map_err(|_| FailureKind::Malformed)?;
            let bitcoin_history = self.hash_at_height(ChainId::Bitcoin, BIP34_HEIGHT).await?;
            let fork_history = self.hash_at_height(fork_chain, BIP34_HEIGHT).await?;
            if bitcoin_history.value != expected_history || fork_history.value != expected_history {
                return Err(FailureKind::WrongChain);
            }
            let bitcoin = self.canonical_coinbase(ChainId::Bitcoin, height).await?;
            let fork = self.canonical_coinbase(fork_chain, height).await?;
            if bitcoin.value.txid != dependency.root().txid {
                return Err(FailureKind::Malformed);
            }
            if bitcoin.value.txid == fork.value.txid {
                return Err(FailureKind::UnsupportedPoison);
            }
            if fork.value.tip.hash != before.tip_hash || fork.value.tip.height != before.tip_height
            {
                return Err(FailureKind::Changed);
            }
            let mut oldest = before
                .observed_at
                .min(bitcoin_history.observed_at)
                .min(fork_history.observed_at)
                .min(bitcoin.observed_at)
                .min(fork.observed_at);
            for (chain, root) in [
                (ChainId::Bitcoin, &bitcoin.value),
                (fork_chain, &fork.value),
            ] {
                let history = self.hash_at_height(chain, BIP34_HEIGHT).await?;
                let block = self.hash_at_height(chain, u64::from(height)).await?;
                let tip = self.tip(chain).await?;
                if history.value != expected_history
                    || block.value != root.block.hash
                    || tip.value != root.tip
                {
                    return Err(FailureKind::Changed);
                }
                oldest = oldest
                    .min(history.observed_at)
                    .min(block.observed_at)
                    .min(tip.observed_at);
            }
            let after = super::super::super::checked_anchor(
                self.anchor(fork_chain).await?,
                fork_chain,
                policy,
                self.now(),
            )
            .map_err(|e| e.kind)?;
            if before.tip_hash != after.tip_hash
                || before.tip_height != after.tip_height
                || before.tip_median_time_past != after.tip_median_time_past
                || before.observation != after.observation
            {
                return Err(FailureKind::Changed);
            }
            oldest = oldest.min(after.observed_at);
            if !super::super::super::fresh(oldest, self.now(), policy.max_observation_age_seconds) {
                return Err(FailureKind::Stale);
            }
            if *self.generation.borrow() != self.expected || self.generation.has_changed().is_err()
            {
                return Err(FailureKind::Cancelled);
            }
            Ok(CoinbasePair {
                selected: dependency.selected(),
                bitcoin: bitcoin.value,
                fork: fork.value,
                anchor: after,
                observed_at: oldest,
                generation: self.expected,
            })
        })
        .await
        .map_err(|_| FailureKind::Deadline)?
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::tests::{ancestry_fixture, source};
    use super::*;
    use coincube_core::miniscript::bitcoin::{
        consensus::{deserialize, serialize},
        script::Builder,
        Amount, Transaction,
    };
    use httpmock::{prelude::*, Mock};
    use serde_json::json;

    fn fresh_mock<'a>(server: &'a MockServer, path: &str, body: String) -> Mock<'a> {
        server.mock(|when, then| {
            when.method(GET)
                .path(path)
                .header("x-coincube-observation", "fresh");
            then.status(200)
                .header("x-coincube-observation", "fresh")
                .header("x-cache", "BYPASS")
                .header("cache-control", "no-store")
                .body(body);
        })
    }
    #[tokio::test]
    async fn paired_roots_bind_history_anchor_identity_and_current_views() {
        for case in [
            "valid",
            "upper",
            "bitcoin-history",
            "revoked",
            "history",
            "root",
            "shared",
            "anchor-tip",
            "activation",
            "stale",
            "policy",
            "anchor-change",
            "bitcoin-reorg",
        ] {
            let server = MockServer::start();
            let (source, sender) = source(&server);
            let root_height = if case == "upper" {
                HISTORICAL_LIMIT - 1
            } else {
                FIRST_FORK_HEIGHT
            };
            let (_, bytes) = ancestry_fixture();
            let mut bitcoin: Transaction = deserialize(&bytes).unwrap();
            bitcoin.input[0].script_sig = Builder::new()
                .push_int(i64::from(root_height))
                .push_int(1)
                .into_script();
            let mut fork = bitcoin.clone();
            if case != "shared" {
                fork.output[0].value = Amount::from_sat(4000);
            } else {
                fork.input[0].witness.push([42]);
                assert_eq!(fork.compute_txid(), bitcoin.compute_txid());
                assert_ne!(serialize(&fork), serialize(&bitcoin));
            }
            let bitcoin_raw = serialize(&bitcoin);
            let mut selected_tx = bitcoin.clone();
            if case == "root" {
                selected_tx.output[0].value = Amount::from_sat(3000);
            }
            let selected_raw = serialize(&selected_tx);
            let dependency = claim_ancestry::verify(
                OutPoint {
                    txid: selected_tx.compute_txid(),
                    vout: 0,
                },
                &[claim_ancestry::Link {
                    transaction: &selected_raw,
                    parent_input: None,
                }],
            )
            .unwrap();
            let tip_height = u64::from(root_height) + 200;
            let bitcoin_tip = "11".repeat(32);
            let fork_tip = "22".repeat(32);
            let bitcoin_block = "33".repeat(32);
            let fork_block = "44".repeat(32);
            let mut bitcoin_mapping = None;
            for (chain, tip, block, tx) in [
                (ChainId::Bitcoin, &bitcoin_tip, &bitcoin_block, &bitcoin),
                (ChainId::BitcoinBlake2b, &fork_tip, &fork_block, &fork),
            ] {
                let prefix = format!(
                    "/api/v1/esplora/{}",
                    HttpObservationSource::prefix(chain).unwrap()
                );
                fresh_mock(&server, &format!("{}/blocks/tip/hash", prefix), tip.clone());
                fresh_mock(
                    &server,
                    &format!("{}/block/{}/status", prefix, tip),
                    json!({"in_best_chain":true,"height":tip_height}).to_string(),
                );
                fresh_mock(
                    &server,
                    &format!("{}/block-height/{}", prefix, tip_height),
                    tip.clone(),
                );
                let mapping = fresh_mock(
                    &server,
                    &format!("{}/block-height/{}", prefix, root_height),
                    block.clone(),
                );
                if chain == ChainId::Bitcoin {
                    bitcoin_mapping = Some(mapping);
                }
                fresh_mock(
                    &server,
                    &format!("{}/block-height/{}", prefix, BIP34_HEIGHT),
                    if (case == "history" && chain == ChainId::BitcoinBlake2b)
                        || (case == "bitcoin-history" && chain == ChainId::Bitcoin)
                    {
                        "55".repeat(32)
                    } else {
                        BIP34_HASH.to_owned()
                    },
                );
                fresh_mock(
                    &server,
                    &format!("{}/block/{}/txid/0", prefix, block),
                    tx.compute_txid().to_string(),
                );
            }
            server.mock(|when, then| {
                when.method(GET).path(format!(
                    "/api/v1/esplora/bitcoin/mainnet/tx/{}/hex",
                    bitcoin.compute_txid()
                ));
                then.status(200).body(hex::encode(bitcoin_raw));
            });
            let fork_read = server.mock(|when, then| {
                when.method(GET).path(format!(
                    "/api/v1/esplora/bitcoin-blake2b/mainnet/tx/{}/hex",
                    fork.compute_txid()
                ));
                then.status(200)
                    .delay(Duration::from_millis(
                        if matches!(case, "anchor-change" | "bitcoin-reorg" | "revoked") {
                            200
                        } else {
                            0
                        },
                    ))
                    .body(hex::encode(serialize(&fork)));
            });
            let body = json!({"success":true,"data":{"network":"bitcoin-blake2b","state":"available","anchor":{
                "tip_hash":if case == "anchor-tip" {"66".repeat(32)} else {fork_tip.clone()},
                "tip_height":tip_height,"tip_median_time_past":10000,
                "observed_at":source.now() - if case == "stale" {120} else {0},
                "observation":{"tip_height":tip_height,"fork":{"height":FIRST_FORK_HEIGHT - if case == "activation" {1} else {0},"active":true},
                    "rdts":{"state":"flagday","flagday":{"height":FIRST_FORK_HEIGHT,"expiry_time":20000,"active":true}}}
            }}});
            let anchor = server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v1/connect/networks/bitcoin-blake2b/anchor")
                    .header("authorization", "Bearer synthetic-observation-token");
                then.status(200).json_body(body.clone());
            });
            let policy = Policy {
                max_observation_age_seconds: if case == "policy" { 0 } else { 60 },
                expiry_margin_seconds: 600,
            };
            let collect = source.coinbase_pair(&dependency, root_height, policy);
            let result = if matches!(case, "anchor-change" | "bitcoin-reorg" | "revoked") {
                let disrupt = async {
                    tokio::time::timeout(Duration::from_secs(1), async {
                        while fork_read.hits_async().await == 0 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("fork collection must begin");
                    if case == "revoked" {
                        sender.send_replace(5);
                    } else if case == "anchor-change" {
                        anchor.delete_async().await;
                        let mut changed = body.clone();
                        changed["data"]["anchor"]["tip_median_time_past"] = json!(10001);
                        server.mock(|when, then| {
                            when.method(GET)
                                .path("/api/v1/connect/networks/bitcoin-blake2b/anchor");
                            then.status(200).json_body(changed);
                        });
                    } else {
                        bitcoin_mapping.as_mut().unwrap().delete_async().await;
                        fresh_mock(
                            &server,
                            &format!(
                                "/api/v1/esplora/bitcoin/mainnet/block-height/{}",
                                FIRST_FORK_HEIGHT
                            ),
                            "77".repeat(32),
                        );
                    }
                };
                let (result, ()) = tokio::join!(collect, disrupt);
                result
            } else {
                collect.await
            };
            match case {
                "valid" | "upper" => {
                    let pair = result.unwrap();
                    assert_eq!(pair.selected(), dependency.selected());
                    assert_eq!(pair.bitcoin().txid, bitcoin.compute_txid());
                    assert_eq!(pair.fork().txid, fork.compute_txid());
                    assert_eq!(
                        pair.anchor().tip_hash,
                        BlockHash::from_str(&fork_tip).unwrap()
                    );
                    assert_eq!(pair.generation(), 4);
                    assert!(pair.observed_at() <= source.now());
                    for height in [FIRST_FORK_HEIGHT - 1, HISTORICAL_LIMIT, u32::MAX] {
                        assert_eq!(
                            source
                                .coinbase_pair(&dependency, height, policy)
                                .await
                                .unwrap_err(),
                            FailureKind::UnsupportedPoison
                        );
                    }
                }
                "history" | "bitcoin-history" | "activation" => {
                    assert_eq!(result.unwrap_err(), FailureKind::WrongChain)
                }
                "revoked" => assert_eq!(result.unwrap_err(), FailureKind::Cancelled),
                "root" => assert_eq!(result.unwrap_err(), FailureKind::Malformed),
                "shared" => assert_eq!(result.unwrap_err(), FailureKind::UnsupportedPoison),
                "stale" | "policy" => assert_eq!(result.unwrap_err(), FailureKind::Stale),
                _ => assert_eq!(result.unwrap_err(), FailureKind::Changed),
            }
        }
    }
}
