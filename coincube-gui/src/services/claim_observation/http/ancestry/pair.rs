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
            // Classify invalidation only after both histories, height mappings,
            // tips and the authenticated anchor stayed unchanged across the
            // complete fresh collection. A provider error or bracket race must
            // remain indeterminate and cannot revoke durable completion.
            if bitcoin.value.txid != dependency.root().txid {
                return Err(FailureKind::AncestryRootChanged);
            }
            if bitcoin.value.txid == fork.value.txid {
                return Err(FailureKind::AncestryRootShared);
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
mod binding_tests;

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
            "root-stale",
            "shared-revoked",
            "root-anchor-change",
            "root-bitcoin-reorg",
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
            if !matches!(case, "shared" | "shared-revoked") {
                fork.output[0].value = Amount::from_sat(4000);
            } else {
                fork.input[0].witness.push([42]);
                assert_eq!(fork.compute_txid(), bitcoin.compute_txid());
                assert_ne!(serialize(&fork), serialize(&bitcoin));
            }
            let bitcoin_raw = serialize(&bitcoin);
            let mut selected_tx = bitcoin.clone();
            if matches!(
                case,
                "root" | "root-stale" | "root-anchor-change" | "root-bitcoin-reorg"
            ) {
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
                        if matches!(
                            case,
                            "anchor-change"
                                | "root-anchor-change"
                                | "bitcoin-reorg"
                                | "root-bitcoin-reorg"
                                | "revoked"
                                | "shared-revoked"
                        ) {
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
                "observed_at":source.now() - if matches!(case, "stale" | "root-stale") {120} else {0},
                "observation":{"tip_height":tip_height,"fork":{"height":FIRST_FORK_HEIGHT - if case == "activation" {1} else {0},"active":true},
                    "rdts":{"state":"flagday","flagday":{"height":FIRST_FORK_HEIGHT,"expiry_time":20000,"active":true}}}
            }}});
            let mut anchor = server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v1/connect/networks/bitcoin-blake2b/anchor")
                    .header("authorization", "Bearer synthetic-observation-token");
                then.status(200).json_body(body.clone());
            });
            let policy = Policy {
                max_observation_age_seconds: if case == "policy" { 0 } else { 60 },
                expiry_margin_seconds: 600,
            };
            let root_status = fresh_mock(&server, &format!("/api/v1/esplora/bitcoin/mainnet/tx/{}", bitcoin.compute_txid()),
                json!({"txid":bitcoin.compute_txid(),"status":{"confirmed":true,"block_height":root_height,"block_hash":bitcoin_block}}).to_string());
            let collect = source.coinbase_pair(&dependency, root_height, policy);
            let result = if matches!(
                case,
                "anchor-change"
                    | "root-anchor-change"
                    | "bitcoin-reorg"
                    | "root-bitcoin-reorg"
                    | "revoked"
                    | "shared-revoked"
            ) {
                let disrupt = async {
                    tokio::time::timeout(Duration::from_secs(1), async {
                        while fork_read.hits_async().await == 0 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("fork collection must begin");
                    if matches!(case, "revoked" | "shared-revoked") {
                        sender.send_replace(5);
                    } else if matches!(case, "anchor-change" | "root-anchor-change") {
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
                    if case == "valid" {
                        // The first input leads to a positively observed pre-fork
                        // root; discovery must continue to the second input.
                        let mut old_root = bitcoin.clone();
                        old_root.input[0].script_sig =
                            Builder::new().push_int(17).push_int(1).into_script();
                        let old_id = old_root.compute_txid();
                        let mut selected = bitcoin.clone();
                        selected.input = vec![
                            coincube_core::miniscript::bitcoin::TxIn {
                                previous_output: OutPoint::new(old_id, 0),
                                ..Default::default()
                            },
                            coincube_core::miniscript::bitcoin::TxIn {
                                previous_output: OutPoint::new(bitcoin.compute_txid(), 0),
                                ..Default::default()
                            },
                        ];
                        for tx in [&old_root, &selected] {
                            server.mock(|when, then| {
                                when.method(GET).path(format!(
                                    "/api/v1/esplora/bitcoin/mainnet/tx/{}/hex",
                                    tx.compute_txid()
                                ));
                                then.status(200).body(hex::encode(serialize(tx)));
                            });
                        }
                        fresh_mock(&server,&format!("/api/v1/esplora/bitcoin/mainnet/tx/{}",old_id),
                            json!({"txid":old_id,"status":{"confirmed":true,"block_height":17,"block_hash":"88".repeat(32)}}).to_string());
                        let selected = OutPoint::new(selected.compute_txid(), 0);
                        let discovered = source
                            .discover_ancestry(selected, policy)
                            .await
                            .unwrap()
                            .unwrap();
                        let preferred = source
                            .discover_preferred_ancestry(
                                &[OutPoint::new(old_id, 0), selected],
                                policy,
                            )
                            .await
                            .unwrap()
                            .unwrap();
                        assert_eq!(preferred.pair().selected(), selected);
                        assert!(source
                            .discover_preferred_ancestry(&[OutPoint::new(old_id, 0)], policy)
                            .await
                            .unwrap()
                            .is_none());
                        assert_eq!(discovered.pair().selected(), selected);
                        assert_eq!(discovered.links().len(), 2);
                        assert_eq!(discovered.links()[0].parent_input, Some(1));
                        let retained = discovered.retained_path().unwrap();
                        let restored = claim_ancestry::retained::RetainedPath::decode(
                            selected,
                            &retained.encode(),
                        )
                        .unwrap();
                        assert_eq!(
                            restored.reverify().unwrap().root().txid,
                            bitcoin.compute_txid()
                        );
                        assert_eq!(restored.links()[0].parent_input, Some(1));
                        let renewed = source.requalify_ancestry(&restored, policy).await.unwrap();
                        assert_eq!(renewed.pair().bitcoin().txid, bitcoin.compute_txid());
                        assert_eq!(renewed.generation(), 4);
                        let plan = super::binding_tests::plan(&restored);
                        let mut step_reads = Vec::new();
                        for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
                            step_reads.push(server.mock(|when, then| {
                                when.method(GET).path(format!(
                                    "/api/v1/esplora/{}/tx/{}",
                                    HttpObservationSource::prefix(chain).unwrap(),
                                    plan.step1.compute_txid()
                                ));
                                then.status(404)
                                    .header("x-cache", "BYPASS")
                                    .header("cache-control", "no-store")
                                    .header("x-coincube-observation", "fresh");
                            }));
                        }
                        let combined = source
                            .collect_ancestry(&restored, &plan, policy, MAX_COLLECTION_TIME)
                            .await
                            .unwrap();
                        assert_eq!(
                            combined.assessment().assessment,
                            Assessment::InputProofUnsupported
                        );
                        assert_eq!(
                            combined.assessment().observations.bitcoin_transaction,
                            TransactionObservation::Absent
                        );
                        assert_eq!(combined.ancestry().pair().selected(), selected);
                        assert_eq!(
                            combined.observed_at(),
                            combined.assessment().observations.bitcoin.observed_at
                        );
                        assert_eq!(
                            combined.observed_at(),
                            combined.assessment().observations.fork.observed_at
                        );
                        for mock in &step_reads {
                            mock.assert_hits(2);
                        }
                        let sweep = Txid::from_str(&"55".repeat(32)).unwrap();
                        let sweep_path =
                            format!("/api/v1/esplora/bitcoin-blake2b/mainnet/tx/{sweep}");
                        let sweep_read = fresh_mock(
                            &server,
                            &sweep_path,
                            json!({"txid":sweep,"status":{"confirmed":true,
                                "block_height":root_height,"block_hash":fork_block}})
                            .to_string(),
                        );
                        let roots_before = root_status.hits();
                        let checked = source
                            .collect_ancestry_sweep(
                                &restored,
                                &plan,
                                sweep,
                                policy,
                                MAX_COLLECTION_TIME,
                            )
                            .await
                            .unwrap();
                        root_status.assert_hits(roots_before + 2);
                        sweep_read.assert_hits(2);
                        assert_eq!(checked.ancestry().ancestry().pair().selected(), selected);
                        assert_eq!(
                            checked.sweep().assessment().assessment,
                            Assessment::InputProofUnsupported
                        );
                        assert!(
                            matches!(checked.sweep().transaction(), TransactionObservation::Confirmed { txid, .. } if txid == sweep)
                        );
                        assert!(checked.sweep().observed_at() <= checked.ancestry().observed_at());
                        assert_eq!(
                            checked.sweep().observed_at(),
                            checked.sweep().assessment().observations.fork.observed_at
                        );
                        // Change only RDTS metadata after the first proof pass.
                        // Both separate ancestry collections remain valid, but
                        // they must not be combined across different anchors.
                        sweep_read.delete_async().await;
                        let delayed_sweep = server.mock(|when, then| {
                            when.method(GET).path(&sweep_path);
                            then.status(200)
                                .delay(Duration::from_millis(100))
                                .header("x-cache", "BYPASS")
                                .header("cache-control", "no-store")
                                .header("x-coincube-observation", "fresh")
                                .json_body(json!({"txid":sweep,"status":{"confirmed":true,
                                    "block_height":root_height,"block_hash":fork_block}}));
                        });
                        let collection = source.collect_ancestry_sweep(
                            &restored,
                            &plan,
                            sweep,
                            policy,
                            MAX_COLLECTION_TIME,
                        );
                        let disrupt = async {
                            tokio::time::timeout(Duration::from_secs(2), async {
                                while delayed_sweep.hits_async().await == 0 {
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .unwrap();
                            anchor.delete_async().await;
                            let mut changed = body.clone();
                            changed["data"]["anchor"]["observation"]["rdts"]["flagday"]
                                ["expiry_time"] = json!(21000);
                            server.mock(|when, then| {
                                when.method(GET)
                                    .path("/api/v1/connect/networks/bitcoin-blake2b/anchor");
                                then.status(200).json_body(changed);
                            })
                        };
                        let (result, changed_anchor) = tokio::join!(collection, disrupt);
                        assert!(matches!(
                            result,
                            Err(Failure {
                                stage: Stage::ForkTransaction,
                                kind: FailureKind::Changed
                            })
                        ));
                        changed_anchor.delete_async().await;
                        anchor = server.mock(|when, then| {
                            when.method(GET)
                                .path("/api/v1/connect/networks/bitcoin-blake2b/anchor")
                                .header("authorization", "Bearer synthetic-observation-token");
                            then.status(200).json_body(body.clone());
                        });
                        delayed_sweep.delete_async().await;
                        // A sweep reported in a noncanonical block cannot be
                        // combined with an otherwise successful ancestry proof.
                        fresh_mock(
                            &server,
                            &sweep_path,
                            json!({"txid":sweep,"status":{"confirmed":true,
                                "block_height":root_height,"block_hash":"66".repeat(32)}})
                            .to_string(),
                        );
                        assert!(matches!(
                            source
                                .collect_ancestry_sweep(
                                    &restored,
                                    &plan,
                                    sweep,
                                    policy,
                                    MAX_COLLECTION_TIME
                                )
                                .await,
                            Err(Failure {
                                stage: Stage::ForkIndexer,
                                kind: FailureKind::Changed
                            })
                        ));
                        let before = root_status.hits();
                        assert!(matches!(
                            source
                                .collect_ancestry_sweep(
                                    &restored,
                                    &plan,
                                    plan.step1.compute_txid(),
                                    policy,
                                    MAX_COLLECTION_TIME
                                )
                                .await,
                            Err(Failure {
                                stage: Stage::Plan,
                                kind: FailureKind::InvalidPlan
                            })
                        ));
                        root_status.assert_hits(before);
                        let mut invalid = plan.clone();
                        invalid.claimed_prevouts.push(selected);
                        assert!(matches!(
                            source
                                .collect_ancestry(&restored, &invalid, policy, MAX_COLLECTION_TIME)
                                .await,
                            Err(Failure {
                                stage: Stage::Plan,
                                kind: FailureKind::InvalidPlan
                            })
                        ));
                        assert!(matches!(
                            source
                                .collect_ancestry(&restored, &plan, policy, Duration::ZERO)
                                .await,
                            Err(Failure {
                                stage: Stage::Plan,
                                kind: FailureKind::InvalidPlan
                            })
                        ));
                        root_status.assert_hits(before);
                        // Change only fork activation metadata between the two
                        // passes, keeping both tips, MTP and RDTS unchanged.
                        anchor.delete_async().await;
                        let slow_anchor = server.mock(|when, then| {
                            when.method(GET)
                                .path("/api/v1/connect/networks/bitcoin-blake2b/anchor");
                            then.status(200)
                                .delay(Duration::from_millis(50))
                                .json_body(body.clone());
                        });
                        let change_anchor = async {
                            tokio::time::timeout(Duration::from_secs(3), async {
                                while slow_anchor.hits_async().await < 2 {
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .unwrap();
                            slow_anchor.delete_async().await;
                            let mut changed = body.clone();
                            changed["data"]["anchor"]["observation"]["fork"]["height"] =
                                json!(FIRST_FORK_HEIGHT + 1);
                            server.mock(|when, then| {
                                when.method(GET)
                                    .path("/api/v1/connect/networks/bitcoin-blake2b/anchor");
                                then.status(200).json_body(changed);
                            })
                        };
                        let (result, changed_anchor) = tokio::join!(
                            source.collect_ancestry(&restored, &plan, policy, MAX_COLLECTION_TIME),
                            change_anchor
                        );
                        assert!(matches!(
                            result,
                            Err(Failure {
                                stage: Stage::Preflight,
                                kind: FailureKind::Changed
                            })
                        ));
                        changed_anchor.assert_hits(2);
                        changed_anchor.delete_async().await;
                        server.mock(|when, then| {
                            when.method(GET)
                                .path("/api/v1/connect/networks/bitcoin-blake2b/anchor");
                            then.status(200).json_body(body.clone());
                        });
                        // A delayed transaction read happens only after ancestry
                        // requalification. The one outer budget includes both phases.
                        step_reads[0].delete_async().await;
                        let delayed = server.mock(|when, then| {
                            when.method(GET).path(format!(
                                "/api/v1/esplora/bitcoin/mainnet/tx/{}",
                                plan.step1.compute_txid()
                            ));
                            then.status(404)
                                .delay(Duration::from_secs(2))
                                .header("x-cache", "BYPASS")
                                .header("cache-control", "no-store")
                                .header("x-coincube-observation", "fresh");
                        });
                        assert!(matches!(
                            source
                                .collect_ancestry(&restored, &plan, policy, Duration::from_secs(1))
                                .await,
                            Err(Failure {
                                kind: FailureKind::Deadline,
                                ..
                            })
                        ));
                        delayed.assert_hits(1);
                        root_status.delete_async().await;
                        server.mock(|when, then| {
                            when.method(GET).path(format!(
                                "/api/v1/esplora/bitcoin/mainnet/tx/{}",
                                bitcoin.compute_txid()
                            ));
                            then.status(404)
                                .header("x-cache", "BYPASS")
                                .header("cache-control", "no-store")
                                .header("x-coincube-observation", "fresh");
                        });
                        assert!(matches!(source.requalify_ancestry(&restored, policy).await,
                            Err(crate::services::claim_observation::http::DiscoveryError::Observation(FailureKind::Changed))));
                        assert_eq!(discovered.generation(), 4);
                        assert!(discovered.observed_at() <= source.now());
                        let links: Vec<_> = discovered
                            .links()
                            .iter()
                            .map(|link| claim_ancestry::Link {
                                transaction: &link.transaction,
                                parent_input: link.parent_input,
                            })
                            .collect();
                        assert_eq!(
                            claim_ancestry::verify(selected, &links)
                                .unwrap()
                                .root()
                                .txid,
                            bitcoin.compute_txid()
                        );
                        super::binding_tests::check(&renewed, &restored, &source, sender);
                    }

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
                "revoked" | "shared-revoked" => {
                    assert_eq!(result.unwrap_err(), FailureKind::Cancelled)
                }
                "root" => assert_eq!(result.unwrap_err(), FailureKind::AncestryRootChanged),
                "shared" => assert_eq!(result.unwrap_err(), FailureKind::AncestryRootShared),
                "stale" | "root-stale" | "policy" => {
                    assert_eq!(result.unwrap_err(), FailureKind::Stale)
                }
                _ => assert_eq!(result.unwrap_err(), FailureKind::Changed),
            }
        }
    }
}
