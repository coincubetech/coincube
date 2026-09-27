//! Positive indexer observations only, never chain-exclusivity authorization.
mod pair;
pub use pair::CoinbasePair;

use super::*;
use coincube_core::{claim_ancestry, miniscript::bitcoin::OutPoint};

/// A structurally checked coinbase reported at position zero of a block whose
/// height mapping and chain tip stayed unchanged during this collection.
/// This is trusted-provider evidence, not a proof-of-work or Merkle proof.
/// Callers still need the authenticated fork anchor, historical uniqueness,
/// pair comparison, freshness/generation checks and all spend-policy checks.
#[derive(Debug)]
pub struct CanonicalCoinbase {
    pub block: BlockRef,
    pub tip: BlockRef,
    pub txid: Txid,
    pub transaction: Vec<u8>,
    pub generation: u64,
}

impl HttpObservationSource {
    /// Collect a bounded positive coinbase observation. A 404 is an error,
    /// never evidence that an outpoint is exclusive to the other chain.
    pub async fn canonical_coinbase(
        &self,
        chain: ChainId,
        height: u32,
    ) -> Result<FreshRead<CanonicalCoinbase>, FailureKind> {
        if height > i32::MAX as u32 {
            return Err(FailureKind::Malformed);
        }
        tokio::time::timeout(MAX_COLLECTION_TIME, async {
            let tip = self.tip(chain).await?;
            if u64::from(height) > tip.value.height {
                return Err(FailureKind::Malformed);
            }
            let block = self.hash_at_height(chain, u64::from(height)).await?;
            let (status, bytes, headers, stamp) = self
                .get(chain, &format!("block/{}/txid/0", block.value))
                .await?;
            if status != 200 {
                return Err(FailureKind::Http(status));
            }
            if bytes.len() != 64 || !bytes.iter().all(u8::is_ascii_hexdigit) {
                return Err(FailureKind::Malformed);
            }
            let txid =
                Txid::from_str(std::str::from_utf8(&bytes).map_err(|_| FailureKind::Malformed)?)
                    .map_err(|_| FailureKind::Malformed)?;
            let raw = self.ancestry_transaction(chain, txid).await?;
            let dependency = claim_ancestry::verify(
                OutPoint { txid, vout: 0 },
                &[claim_ancestry::Link {
                    transaction: &raw,
                    parent_input: None,
                }],
            )
            .map_err(|_| FailureKind::Malformed)?;
            dependency
                .check_height_commitment(height)
                .map_err(|_| FailureKind::Malformed)?;
            let block_after = self.hash_at_height(chain, u64::from(height)).await?;
            let tip_after = self.tip(chain).await?;
            if block.value != block_after.value || tip.value != tip_after.value {
                return Err(FailureKind::Changed);
            }
            FreshRead::from_response(
                chain,
                CanonicalCoinbase {
                    block: BlockRef {
                        height: u64::from(height),
                        hash: block.value,
                    },
                    tip: tip.value,
                    txid,
                    transaction: raw,
                    generation: self.expected,
                },
                stamp
                    .min(tip.observed_at)
                    .min(block.observed_at)
                    .min(block_after.observed_at)
                    .min(tip_after.observed_at),
                &headers,
            )
        })
        .await
        .map_err(|_| FailureKind::Deadline)?
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{ancestry_fixture, source};
    use super::*;
    use coincube_core::miniscript::bitcoin::{
        consensus::{deserialize, serialize},
        script::Builder,
        Transaction,
    };
    use httpmock::prelude::*;
    use serde_json::json;

    #[tokio::test]
    async fn canonical_coinbase_requires_positive_fresh_structural_evidence() {
        for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
            for case in [
                "valid",
                "wrong-height",
                "ordinary-tx",
                "wrong-txid",
                "missing",
                "cached",
                "bad-id",
                "future",
                "changed-mapping",
                "revoked",
            ] {
                let server = MockServer::start();
                let (source, sender) = source(&server);
                let prefix = format!(
                    "/api/v1/esplora/{}",
                    HttpObservationSource::prefix(chain).unwrap()
                );
                let tip = "11".repeat(32);
                let block = "22".repeat(32);
                let (_, raw) = ancestry_fixture();
                let mut tx: Transaction = deserialize(&raw).unwrap();
                tx.input[0].script_sig = Builder::new()
                    .push_int(if case == "wrong-height" { 18 } else { 17 })
                    .push_int(1)
                    .into_script();
                if case == "ordinary-tx" {
                    tx.input[0].previous_output.txid = Txid::from_str(&"33".repeat(32)).unwrap();
                }
                let txid = tx.compute_txid();
                let requested = if case == "wrong-txid" {
                    "44".repeat(32)
                } else {
                    txid.to_string()
                };
                let mut observations = Vec::new();
                for (path, body) in [
                    ("blocks/tip/hash".to_owned(), tip.clone()),
                    (
                        format!("block/{}/status", tip),
                        json!({"in_best_chain":true,"height":100}).to_string(),
                    ),
                    ("block-height/100".to_owned(), tip.clone()),
                    ("block-height/17".to_owned(), block.clone()),
                ] {
                    observations.push(server.mock(|when, then| {
                        when.method(GET)
                            .path(format!("{}/{}", prefix, path))
                            .header("x-coincube-observation", "fresh");
                        then.status(200)
                            .header("x-coincube-observation", "fresh")
                            .header("x-cache", "BYPASS")
                            .header("cache-control", "no-store")
                            .body(body);
                    }));
                }
                let position = server.mock(|when, then| {
                    when.method(GET)
                        .path(format!("{}/block/{}/txid/0", prefix, block));
                    let then = then
                        .status(if case == "missing" { 404 } else { 200 })
                        .header("x-cache", if case == "cached" { "HIT" } else { "BYPASS" })
                        .header("x-coincube-observation", "fresh")
                        .header("cache-control", "no-store");
                    then.body(if case == "bad-id" {
                        "not-a-txid".to_owned()
                    } else {
                        requested.clone()
                    });
                });
                let raw_mock = server.mock(|when, then| {
                    when.method(GET)
                        .path(format!("{}/tx/{}/hex", prefix, requested));
                    then.status(200)
                        .delay(Duration::from_millis(
                            if matches!(case, "changed-mapping" | "revoked") {
                                200
                            } else {
                                0
                            },
                        ))
                        .header("x-cache", "HIT")
                        .body(hex::encode(serialize(&tx)));
                });
                let collect =
                    source.canonical_coinbase(chain, if case == "future" { 101 } else { 17 });
                let result = if matches!(case, "changed-mapping" | "revoked") {
                    let disrupt = async {
                        tokio::time::timeout(Duration::from_secs(1), async {
                            while raw_mock.hits_async().await == 0 {
                                tokio::task::yield_now().await;
                            }
                        })
                        .await
                        .expect("collector must reach raw transaction read");
                        if case == "revoked" {
                            sender.send_replace(5);
                        } else {
                            observations[3].delete_async().await;
                            server.mock(|when, then| {
                                when.method(GET).path(format!("{}/block-height/17", prefix));
                                then.status(200)
                                    .header("x-coincube-observation", "fresh")
                                    .header("x-cache", "BYPASS")
                                    .header("cache-control", "no-store")
                                    .body("55".repeat(32));
                            });
                        }
                    };
                    let (result, ()) = tokio::join!(collect, disrupt);
                    result
                } else {
                    collect.await
                };
                match case {
                    "valid" => {
                        let result = result.unwrap();
                        assert_eq!(result.chain, chain);
                        assert_eq!(
                            result.value.block,
                            BlockRef {
                                height: 17,
                                hash: BlockHash::from_str(&block).unwrap()
                            }
                        );
                        assert_eq!(result.value.tip.height, 100);
                        assert_eq!(result.value.txid, txid);
                        assert_eq!(result.value.transaction, serialize(&tx));
                        assert_eq!(result.value.generation, 4);
                        assert!(result.observed_at <= source.now());
                    }
                    "changed-mapping" => assert_eq!(result.unwrap_err(), FailureKind::Changed),
                    "revoked" => assert_eq!(result.unwrap_err(), FailureKind::Cancelled),
                    "missing" => assert_eq!(result.unwrap_err(), FailureKind::Http(404)),
                    "cached" => assert_eq!(result.unwrap_err(), FailureKind::FreshnessUnverified),
                    _ => assert_eq!(result.unwrap_err(), FailureKind::Malformed),
                }
                if case == "future" {
                    position.assert_hits(0);
                }
            }
        }
    }
}
