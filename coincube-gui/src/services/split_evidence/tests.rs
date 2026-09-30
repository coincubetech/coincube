use super::*;
use crate::services::{
    foreign_scan::DiscoveredCoin,
    foreign_split_inventory::FreshIndex,
    split_source::split_source,
    split_test_wallets::{self as fixture, Shape, SHAPES},
};
use coincube_core::{
    foreign_split::{create_split_step1, reconstruct_split_step1, SplitInputs},
    miniscript::bitcoin::{absolute::LockTime, hashes::Hash, Amount},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const NOW: i64 = 1_800_000_000;

fn headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-cache", "BYPASS".parse().unwrap());
    headers.insert(CACHE_CONTROL, "no-store".parse().unwrap());
    headers
}

fn fresh<T>(chain: ChainId, value: T, observed_at: i64) -> FreshRead<T> {
    FreshRead::from_response(chain, value, observed_at, &headers()).unwrap()
}

/// Serves one fixed view of both chains. Every field is editable per test.
#[derive(Clone)]
struct Chains {
    tips: HashMap<ChainId, Vec<BlockRef>>,
    tip_reads: Arc<Mutex<HashMap<ChainId, usize>>>,
    previous: HashMap<Txid, Transaction>,
    status: HashMap<(ChainId, Txid), TransactionObservation>,
    canonical: HashMap<(ChainId, u64), BlockHash>,
    btcb2_spent: BTreeSet<OutPoint>,
    outspend_chains: Arc<Mutex<Vec<ChainId>>>,
    /// Observation stamp per chain.
    stamp: HashMap<ChainId, i64>,
    stale_status: bool,
}

impl Chains {
    /// Both chains agree on every coin in `coins` (the scan fixture).
    fn of(coins: &[DiscoveredCoin]) -> Self {
        let mut chains = Self {
            tips: HashMap::new(),
            tip_reads: Arc::default(),
            previous: HashMap::new(),
            status: HashMap::new(),
            canonical: HashMap::new(),
            btcb2_spent: BTreeSet::new(),
            outspend_chains: Arc::default(),
            stamp: HashMap::from([(ChainId::Bitcoin, NOW), (ChainId::BitcoinBlake2b, NOW)]),
            stale_status: false,
        };
        for (chain, height) in [
            (ChainId::Bitcoin, fixture::BITCOIN_TIP_HEIGHT),
            (ChainId::BitcoinBlake2b, fixture::BTCB2_TIP_HEIGHT),
        ] {
            let height = u64::from(height);
            chains.tips.insert(
                chain,
                vec![BlockRef {
                    height,
                    hash: fixture::block_hash(height),
                }],
            );
        }
        for coin in coins {
            let block = BlockRef {
                height: u64::from(coin.block_height.unwrap()),
                hash: coin.block_hash.unwrap(),
            };
            chains
                .previous
                .insert(coin.outpoint.txid, coin.previous.clone());
            for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
                chains.status.insert(
                    (chain, coin.outpoint.txid),
                    TransactionObservation::Confirmed {
                        txid: coin.outpoint.txid,
                        block,
                    },
                );
                chains.canonical.insert((chain, block.height), block.hash);
            }
        }
        chains
    }
}

#[async_trait]
impl SplitEvidenceSource for Chains {
    fn now(&self) -> i64 {
        NOW + 5
    }
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        let mut reads = self.tip_reads.lock().unwrap();
        let n = reads.entry(chain).or_default();
        let tips = &self.tips[&chain];
        let tip = tips[(*n).min(tips.len() - 1)];
        *n += 1;
        Ok(fresh(chain, tip, self.stamp[&chain]))
    }
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        let value = self
            .status
            .get(&(chain, txid))
            .copied()
            .unwrap_or(TransactionObservation::Absent);
        let stamp = if self.stale_status {
            NOW - MAX_EVIDENCE_AGE_SECONDS
        } else {
            self.stamp[&chain]
        };
        Ok(fresh(chain, value, stamp))
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        let hash = self
            .canonical
            .get(&(chain, height))
            .copied()
            .ok_or(FailureKind::Http(404))?;
        Ok(fresh(chain, hash, self.stamp[&chain]))
    }
    async fn previous_transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<Transaction>, FailureKind> {
        let tx = self
            .previous
            .get(&txid)
            .cloned()
            .ok_or(FailureKind::Http(404))?;
        Ok(fresh(chain, tx, self.stamp[&chain]))
    }
    async fn outspend(
        &self,
        chain: ChainId,
        outpoint: OutPoint,
    ) -> Result<FreshRead<Outspend>, FailureKind> {
        self.outspend_chains.lock().unwrap().push(chain);
        let spent = chain == ChainId::BitcoinBlake2b && self.btcb2_spent.contains(&outpoint);
        Ok(fresh(
            chain,
            if spent {
                Outspend::Spent
            } else {
                Outspend::Unspent
            },
            self.stamp[&chain],
        ))
    }
}

fn recorded(coins: &[DiscoveredCoin]) -> Vec<RecordedOutpoint> {
    coins
        .iter()
        .map(|coin| RecordedOutpoint {
            outpoint: coin.outpoint,
            branch: match coin.branch {
                crate::services::foreign_scan::Branch::External => SplitBranch::External,
                crate::services::foreign_scan::Branch::Internal => SplitBranch::Internal,
            },
            index: coin.index,
        })
        .collect()
}

async fn authenticate(
    chains: &Chains,
    coins: &[DiscoveredCoin],
) -> Result<AuthenticatedOutpoints, EvidenceError> {
    authenticate_outpoints(
        chains,
        &recorded(coins),
        fixture::FORK,
        MAX_EVIDENCE_AGE_SECONDS,
    )
    .await
}

/// Acceptance: every shape builds step 1 from the inventory's splittable
/// coins with the proven fresh receive index and the observed Bitcoin tip,
/// and reconstructs it from freshly authenticated outpoints, which equal the
/// scan's coins.
#[tokio::test]
async fn split_every_shape_builds_step1_from_splittable_coins_and_reauthenticates() {
    for shape in SHAPES {
        let wallet = fixture::wallet(shape);
        let inventory = fixture::inventory(&wallet);
        let coins = inventory.splittable_coins();
        assert_eq!(coins.len(), 2, "{shape:?}");
        let FreshIndex::Proven(destination) = inventory.fresh_receive() else {
            panic!("{:?}: no proven fresh index", shape);
        };
        let source = split_source(&wallet.external, Some(&wallet.internal)).unwrap();
        let tip = inventory.bitcoin_tip_height();
        let inputs = SplitInputs {
            chain: ChainId::Bitcoin,
            source: &source,
            coins: &coins,
            fork_height: inventory.fork_height(),
            destination,
        };
        let step1 = create_split_step1(
            &inputs,
            3,
            LockTime::from_height(tip).unwrap(),
            tip,
            BlockHash::from_byte_array([7; 32]),
        )
        .unwrap_or_else(|error| panic!("{:?}: {}", shape, error));
        assert_eq!(step1.claimed_prevouts().len(), 2);

        let shared = fixture::shared_coins(&wallet);
        let chains = Chains::of(&shared);
        let authenticated = authenticate(&chains, &shared).await.unwrap();
        let mut expected = coins.clone();
        expected.sort_by_key(|coin| coin.outpoint);
        let mut got = authenticated.coins.clone();
        got.sort_by_key(|coin| coin.outpoint);
        assert_eq!(got, expected, "{shape:?}");
        let rebuilt = reconstruct_split_step1(
            &SplitInputs {
                coins: &authenticated.coins,
                ..inputs
            },
            &step1.psbt().unsigned_tx,
            authenticated.bitcoin_tip.height as u32,
        )
        .unwrap();
        assert_eq!(rebuilt.psbt(), step1.psbt(), "{shape:?}");
    }
}

/// Post-fork, pending, one-chain and disagreeing coins never become step-1
/// inputs.
#[test]
fn split_only_shared_pre_fork_coins_are_splittable_coins() {
    use crate::services::foreign_scan::Branch;
    let wallet = fixture::wallet(Shape::Wpkh);
    let shared = fixture::coin(&wallet, Branch::External, 0, 10_000, Some(880));
    let post_fork = fixture::coin(&wallet, Branch::External, 1, 11_000, Some(905));
    let pending = fixture::coin(&wallet, Branch::External, 2, 12_000, None);
    let btcb2_only = fixture::coin(&wallet, Branch::Internal, 0, 13_000, Some(870));
    let bitcoin_only = fixture::coin(&wallet, Branch::Internal, 1, 14_000, Some(871));
    let everything = || vec![shared.clone(), post_fork.clone(), pending.clone()];
    let mut btcb2 = everything();
    btcb2.push(btcb2_only);
    let mut bitcoin = everything();
    bitcoin.push(bitcoin_only);
    let inventory = SplitInventory::join(
        &fixture::report(ChainId::BitcoinBlake2b, btcb2),
        &fixture::report(ChainId::Bitcoin, bitcoin),
        fixture::GENERATION,
        true,
    )
    .unwrap();
    let outpoints: Vec<_> = inventory
        .splittable_coins()
        .iter()
        .map(|coin| coin.outpoint)
        .collect();
    assert_eq!(outpoints, vec![shared.outpoint]);
    assert_eq!(
        inventory.splittable_coins()[0].bitcoin_block,
        Some(BlockRef {
            height: 880,
            hash: fixture::block_hash(880)
        })
    );
    // The same coin confirmed in different blocks never joins at all.
    let mut other = shared.clone();
    other.block_hash = Some(fixture::block_hash(1));
    assert!(SplitInventory::join(
        &fixture::report(ChainId::BitcoinBlake2b, vec![shared.clone()]),
        &fixture::report(ChainId::Bitcoin, vec![other]),
        fixture::GENERATION,
        true,
    )
    .is_err());
    assert_eq!(inventory.bitcoin_tip_height(), fixture::BITCOIN_TIP_HEIGHT);
    assert_eq!(inventory.btcb2_tip_height(), fixture::BTCB2_TIP_HEIGHT);
}

use crate::services::foreign_split_inventory::SplitInventory;

fn failure(result: Result<AuthenticatedOutpoints, EvidenceError>) -> EvidenceFailure {
    result.unwrap_err().failure
}

#[tokio::test]
async fn split_authentication_works_when_spent_on_bitcoin_and_never_reads_its_outspend() {
    let wallet = fixture::wallet(Shape::Wpkh);
    let shared = fixture::shared_coins(&wallet);
    let chains = Chains::of(&shared);
    let authenticated = authenticate(&chains, &shared).await.unwrap();
    assert_eq!(authenticated.coins.len(), 2);
    assert_eq!(
        authenticated.bitcoin_tip.height,
        u64::from(fixture::BITCOIN_TIP_HEIGHT)
    );
    assert!(chains
        .outspend_chains
        .lock()
        .unwrap()
        .iter()
        .all(|chain| *chain == ChainId::BitcoinBlake2b));
    assert_eq!(chains.outspend_chains.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn split_authentication_refuses_stale_reads() {
    let wallet = fixture::wallet(Shape::Wpkh);
    let shared = fixture::shared_coins(&wallet);
    let mut chains = Chains::of(&shared);
    chains.stale_status = true;
    let error = authenticate(&chains, &shared).await.unwrap_err();
    assert_eq!(error.failure, EvidenceFailure::Stale(ChainId::Bitcoin));
    assert_eq!(error.outpoint, Some(shared[0].outpoint));
    let mut chains = Chains::of(&shared);
    chains.stamp.insert(ChainId::BitcoinBlake2b, NOW - 200);
    assert_eq!(
        failure(authenticate(&chains, &shared).await),
        EvidenceFailure::Stale(ChainId::BitcoinBlake2b)
    );
    // Stamped in the future is not fresh either.
    let mut chains = Chains::of(&shared);
    chains.stamp.insert(ChainId::Bitcoin, NOW + 100);
    assert_eq!(
        failure(authenticate(&chains, &shared).await),
        EvidenceFailure::Stale(ChainId::Bitcoin)
    );
    // A zero or negative bound never disables the check.
    let chains = Chains::of(&shared);
    assert_eq!(
        authenticate_outpoints(&chains, &recorded(&shared), fixture::FORK, 0)
            .await
            .unwrap_err()
            .failure,
        EvidenceFailure::InvalidPolicy
    );
}

#[tokio::test]
async fn split_authentication_refuses_a_substituted_previous_transaction() {
    let wallet = fixture::wallet(Shape::Wpkh);
    let shared = fixture::shared_coins(&wallet);
    let mut chains = Chains::of(&shared);
    let mut substitute = shared[0].previous.clone();
    substitute.output[1].value = Amount::from_sat(1);
    chains.previous.insert(shared[0].outpoint.txid, substitute);
    let error = authenticate(&chains, &shared).await.unwrap_err();
    assert_eq!(
        (error.outpoint, error.failure),
        (Some(shared[0].outpoint), EvidenceFailure::TxidMismatch)
    );
    // A status for another txid is refused too.
    let mut chains = Chains::of(&shared);
    chains.status.insert(
        (ChainId::BitcoinBlake2b, shared[1].outpoint.txid),
        TransactionObservation::Confirmed {
            txid: shared[0].outpoint.txid,
            block: BlockRef {
                height: 890,
                hash: fixture::block_hash(890),
            },
        },
    );
    assert_eq!(
        failure(authenticate(&chains, &shared).await),
        EvidenceFailure::TxidMismatch
    );
    // A vout the transaction does not have.
    let chains = Chains::of(&shared);
    let mut missing = recorded(&shared);
    missing[0].outpoint.vout = 9;
    assert_eq!(
        authenticate_outpoints(&chains, &missing, fixture::FORK, MAX_EVIDENCE_AGE_SECONDS)
            .await
            .unwrap_err()
            .failure,
        EvidenceFailure::MissingOutput
    );
}

#[tokio::test]
async fn split_authentication_refuses_cross_chain_disagreement_and_reorged_blocks() {
    let wallet = fixture::wallet(Shape::Wpkh);
    let shared = fixture::shared_coins(&wallet);
    let txid = shared[0].outpoint.txid;
    // BTCB2 confirms it in another block at the same height.
    let mut chains = Chains::of(&shared);
    let other = BlockRef {
        height: 880,
        hash: fixture::block_hash(1),
    };
    chains.status.insert(
        (ChainId::BitcoinBlake2b, txid),
        TransactionObservation::Confirmed { txid, block: other },
    );
    chains
        .canonical
        .insert((ChainId::BitcoinBlake2b, 880), other.hash);
    assert_eq!(
        failure(authenticate(&chains, &shared).await),
        EvidenceFailure::ChainsDisagree
    );
    // The status names a block no longer at that height.
    let mut chains = Chains::of(&shared);
    chains
        .canonical
        .insert((ChainId::Bitcoin, 880), fixture::block_hash(2));
    assert_eq!(
        failure(authenticate(&chains, &shared).await),
        EvidenceFailure::NotInBestChain(ChainId::Bitcoin)
    );
    // Absent or unconfirmed on one chain.
    for observation in [
        TransactionObservation::Absent,
        TransactionObservation::Unconfirmed { txid },
    ] {
        let mut chains = Chains::of(&shared);
        chains
            .status
            .insert((ChainId::BitcoinBlake2b, txid), observation);
        assert_eq!(
            failure(authenticate(&chains, &shared).await),
            EvidenceFailure::NotConfirmed(ChainId::BitcoinBlake2b)
        );
    }
}

#[tokio::test]
async fn split_authentication_refuses_btcb2_spent_and_post_fork_coins() {
    let wallet = fixture::wallet(Shape::Wpkh);
    let shared = fixture::shared_coins(&wallet);
    let mut chains = Chains::of(&shared);
    chains.btcb2_spent.insert(shared[1].outpoint);
    let error = authenticate(&chains, &shared).await.unwrap_err();
    assert_eq!(
        (error.outpoint, error.failure),
        (Some(shared[1].outpoint), EvidenceFailure::Btcb2Spent)
    );
    // Confirmed at the fork height on both chains: post-fork.
    let chains = Chains::of(&shared);
    assert_eq!(
        authenticate_outpoints(&chains, &recorded(&shared), 890, MAX_EVIDENCE_AGE_SECONDS)
            .await
            .unwrap_err(),
        EvidenceError {
            outpoint: Some(shared[1].outpoint),
            failure: EvidenceFailure::PostFork
        }
    );
}

#[tokio::test]
async fn split_authentication_refuses_moving_tips_and_bad_sets() {
    let wallet = fixture::wallet(Shape::Wpkh);
    let shared = fixture::shared_coins(&wallet);
    let mut chains = Chains::of(&shared);
    chains
        .tips
        .get_mut(&ChainId::BitcoinBlake2b)
        .unwrap()
        .push(BlockRef {
            height: 951,
            hash: fixture::block_hash(951),
        });
    assert_eq!(
        failure(authenticate(&chains, &shared).await),
        EvidenceFailure::Changed(ChainId::BitcoinBlake2b)
    );
    let chains = Chains::of(&shared);
    assert_eq!(
        failure(authenticate(&chains, &[]).await),
        EvidenceFailure::Empty
    );
    let twice = vec![shared[0].clone(), shared[0].clone()];
    assert_eq!(
        failure(authenticate(&chains, &twice).await),
        EvidenceFailure::Duplicate
    );
}

mod http {
    use super::*;
    use httpmock::prelude::*;

    fn anonymous(request: &HttpMockRequest) -> bool {
        request.headers.as_ref().is_none_or(|headers| {
            headers.iter().all(|(name, _)| {
                ![
                    "authorization",
                    "cookie",
                    "x-device-fingerprint",
                    "x-device-name",
                ]
                .iter()
                .any(|bad| name.eq_ignore_ascii_case(bad))
            })
        })
    }

    fn esplora(server: &MockServer) -> (ConnectEsplora, watch::Sender<u64>) {
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("synthetic-split-token");
        let (sender, generation) = watch::channel(3);
        (
            ConnectEsplora::new(
                &client,
                CollectionContext {
                    expected_generation: 3,
                    generation,
                },
            )
            .unwrap(),
            sender,
        )
    }

    fn serve<'a>(
        server: &'a MockServer,
        path: &str,
        body: &str,
        markers: bool,
    ) -> httpmock::Mock<'a> {
        server.mock(|when, then| {
            when.method(GET)
                .path(format!("/api/v1/esplora/{path}"))
                .header("x-coincube-observation", "fresh")
                .matches(anonymous);
            let then = then.status(200).body(body);
            if markers {
                then.header("X-Coincube-Observation", "fresh")
                    .header("X-Cache", "BYPASS")
                    .header("Cache-Control", "no-store");
            }
        })
    }

    #[tokio::test]
    async fn split_outspend_and_previous_transaction_are_fresh_anonymous_reads() {
        let wallet = fixture::wallet(Shape::Wpkh);
        let coin = &fixture::shared_coins(&wallet)[0];
        let server = MockServer::start();
        let (esplora, sender) = esplora(&server);
        let outpoint = coin.outpoint;
        let spent = serve(
            &server,
            &format!(
                "bitcoin-blake2b/mainnet/tx/{}/outspend/{}",
                outpoint.txid, outpoint.vout
            ),
            r#"{"spent":true,"txid":"00","vin":0,"status":{"confirmed":false}}"#,
            true,
        );
        assert_eq!(
            *esplora
                .outspend(ChainId::BitcoinBlake2b, outpoint)
                .await
                .unwrap()
                .value(),
            Outspend::Spent
        );
        spent.assert();
        let hex =
            coincube_core::miniscript::bitcoin::consensus::encode::serialize_hex(&coin.previous);
        let previous = serve(
            &server,
            &format!("bitcoin/mainnet/tx/{}/hex", outpoint.txid),
            &hex,
            true,
        );
        assert_eq!(
            esplora
                .transaction_bytes(ChainId::Bitcoin, outpoint.txid)
                .await
                .unwrap()
                .value(),
            &coin.previous
        );
        previous.assert();
        // A cache hit without the fresh-read acknowledgement is refused.
        let unmarked = serve(
            &server,
            &format!(
                "bitcoin/mainnet/tx/{}/outspend/{}",
                outpoint.txid, outpoint.vout
            ),
            r#"{"spent":false}"#,
            false,
        );
        assert!(matches!(
            esplora.outspend(ChainId::Bitcoin, outpoint).await,
            Err(FailureKind::FreshnessUnverified)
        ));
        unmarked.assert();
        // 404 is a failure, never "unspent"; other chains are refused.
        assert!(matches!(
            esplora
                .outspend(ChainId::BitcoinBlake2b, OutPoint::new(outpoint.txid, 7))
                .await,
            Err(FailureKind::Http(404))
        ));
        assert!(matches!(
            esplora.outspend(ChainId::Testnet4, outpoint).await,
            Err(FailureKind::WrongChain)
        ));
        // A revoked generation reads nothing.
        sender.send_replace(4);
        assert!(matches!(
            esplora.outspend(ChainId::BitcoinBlake2b, outpoint).await,
            Err(FailureKind::Cancelled)
        ));
        spent.assert_hits(1);
    }
}

/// D1: nothing in this slice gains a GUI caller. The evidence, source and
/// file modules are reached only from their own tests; the fee sources only
/// through `app::split_fee_source`, which already priced the review.
#[test]
fn split_b1a_services_have_no_new_gui_callers() {
    fn walk(dir: &std::path::Path, hits: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, hits);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                for needle in [
                    "split_evidence::",
                    "split_source::",
                    "split_psbt_file::",
                    "split_fees::",
                    "splittable_coins(",
                ] {
                    if text.contains(needle) {
                        hits.push((
                            path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                                .unwrap()
                                .to_string_lossy()
                                .into_owned(),
                            needle.to_owned(),
                        ));
                    }
                }
            }
        }
    }
    let mut hits = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut hits,
    );
    hits.sort();
    let allowed = |file: &str, needle: &str| {
        file.starts_with("src/services/split_")
            || (file == "src/services/foreign_split_inventory.rs" && needle == "splittable_coins(")
            || (file == "src/app/mod.rs" && needle == "split_fees::")
    };
    let unexpected: Vec<_> = hits
        .iter()
        .filter(|(file, needle)| !allowed(file, needle))
        .collect();
    assert!(unexpected.is_empty(), "{:?}", unexpected);
}
