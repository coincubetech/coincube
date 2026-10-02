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
    /// Chain and address of every unspent-output read.
    utxo_reads: Arc<Mutex<Vec<(ChainId, String)>>>,
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
            utxo_reads: Arc::default(),
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
    ) -> Result<Transaction, FailureKind> {
        let _ = chain;
        self.previous
            .get(&txid)
            .cloned()
            .ok_or(FailureKind::Http(404))
    }
    async fn unspent_outputs(
        &self,
        chain: ChainId,
        address: &str,
    ) -> Result<FreshRead<Vec<OutPoint>>, FailureKind> {
        self.utxo_reads
            .lock()
            .unwrap()
            .push((chain, address.to_owned()));
        let unspent = self
            .previous
            .values()
            .flat_map(|tx| {
                let txid = tx.compute_txid();
                tx.output
                    .iter()
                    .enumerate()
                    .filter_map(move |(vout, output)| {
                        let paid = bitcoin::Address::from_script(
                            &output.script_pubkey,
                            bitcoin::Network::Bitcoin,
                        )
                        .is_ok_and(|a| a.to_string() == address);
                        paid.then(|| OutPoint::new(txid, vout as u32))
                    })
            })
            .filter(|outpoint| {
                chain != ChainId::BitcoinBlake2b || !self.btcb2_spent.contains(outpoint)
            })
            .collect();
        Ok(fresh(chain, unspent, self.stamp[&chain]))
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
async fn split_authentication_works_when_spent_on_bitcoin_and_never_reads_its_state() {
    let wallet = fixture::wallet(Shape::Wpkh);
    let shared = fixture::shared_coins(&wallet);
    let chains = Chains::of(&shared);
    let authenticated = authenticate(&chains, &shared).await.unwrap();
    assert_eq!(authenticated.coins.len(), 2);
    assert_eq!(
        authenticated.bitcoin_tip.height,
        u64::from(fixture::BITCOIN_TIP_HEIGHT)
    );
    // Only BTCB2 spent state is read, at each coin's own address.
    let reads = chains.utxo_reads.lock().unwrap().clone();
    let addresses: Vec<_> = shared
        .iter()
        .map(|coin| {
            (
                ChainId::BitcoinBlake2b,
                bitcoin::Address::from_script(
                    &coin.output.script_pubkey,
                    bitcoin::Network::Bitcoin,
                )
                .unwrap()
                .to_string(),
            )
        })
        .collect();
    assert_eq!(reads, addresses);
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
    // An output with no address (here OP_RETURN) cannot be read on BTCB2.
    let mut chains = Chains::of(&shared);
    let mut unaddressed = shared[0].previous.clone();
    unaddressed.output[1].script_pubkey = bitcoin::ScriptBuf::new_op_return([1]);
    let unaddressed_txid = unaddressed.compute_txid();
    chains.previous.insert(unaddressed_txid, unaddressed);
    let mut no_address = recorded(&shared);
    no_address[0].outpoint = OutPoint::new(unaddressed_txid, 1);
    assert_eq!(
        authenticate_outpoints(
            &chains,
            &no_address,
            fixture::FORK,
            MAX_EVIDENCE_AGE_SECONDS
        )
        .await
        .unwrap_err()
        .failure,
        EvidenceFailure::NoAddress
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
    use crate::services::split_test_connect::{serve_cached, serve_fresh, strict};
    use httpmock::prelude::*;

    fn client(server: &MockServer) -> CoincubeClient {
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("synthetic-split-token");
        client
    }

    fn address(coin: &DiscoveredCoin) -> String {
        bitcoin::Address::from_script(&coin.output.script_pubkey, bitcoin::Network::Bitcoin)
            .unwrap()
            .to_string()
    }

    /// Serve both chains as Connect would, through the strict fresh-path
    /// model. Returns the 400 refusal mock.
    fn serve_chains<'a>(
        server: &'a MockServer,
        coins: &[DiscoveredCoin],
        btcb2_spent: &[OutPoint],
    ) -> httpmock::Mock<'a> {
        let refusal = strict(server);
        for (network, height) in [
            ("bitcoin", fixture::BITCOIN_TIP_HEIGHT),
            ("bitcoin-blake2b", fixture::BTCB2_TIP_HEIGHT),
        ] {
            let tip = fixture::block_hash(u64::from(height));
            serve_fresh(server, network, "/blocks/tip/hash", &tip.to_string());
            serve_fresh(
                server,
                network,
                &format!("/block/{tip}/status"),
                &format!(r#"{{"in_best_chain":true,"height":{height},"next_best":null}}"#),
            );
            serve_fresh(
                server,
                network,
                &format!("/block-height/{height}"),
                &tip.to_string(),
            );
            for coin in coins {
                let (h, hash) = (coin.block_height.unwrap(), coin.block_hash.unwrap());
                let txid = coin.outpoint.txid;
                serve_fresh(
                    server,
                    network,
                    &format!("/tx/{txid}"),
                    &format!(
                        r#"{{"txid":"{txid}","status":{{"confirmed":true,"block_height":{h},"block_hash":"{hash}"}}}}"#
                    ),
                );
                serve_fresh(
                    server,
                    network,
                    &format!("/block-height/{h}"),
                    &hash.to_string(),
                );
            }
        }
        for coin in coins {
            serve_cached(
                server,
                "bitcoin",
                &format!("/tx/{}/hex", coin.outpoint.txid),
                &coincube_core::miniscript::bitcoin::consensus::encode::serialize_hex(
                    &coin.previous,
                ),
            );
            let listed = if btcb2_spent.contains(&coin.outpoint) {
                "[]".to_owned()
            } else {
                format!(
                    r#"[{{"txid":"{}","vout":{},"value":{},"status":{{"confirmed":true}}}}]"#,
                    coin.outpoint.txid,
                    coin.outpoint.vout,
                    coin.output.value.to_sat()
                )
            };
            serve_fresh(
                server,
                "bitcoin-blake2b",
                &format!("/address/{}/utxo", address(coin)),
                &listed,
            );
        }
        refusal
    }

    /// P2 (#615 review): the production reader authenticates against the
    /// real Connect contract. Its only fresh reads are allowlisted paths; the
    /// previous transaction comes from the txid-checked cached route.
    #[tokio::test]
    async fn split_production_reader_authenticates_through_connect_allowed_paths() {
        let wallet = fixture::wallet(Shape::Wpkh);
        let shared = fixture::shared_coins(&wallet);
        let server = MockServer::start();
        let refusal = serve_chains(&server, &shared, &[]);
        let (_sender, generation) = watch::channel(3);
        let source = ConnectSplitEvidence::new(client(&server), 3, generation).unwrap();
        let authenticated = authenticate_outpoints(
            &source,
            &recorded(&shared),
            fixture::FORK,
            MAX_EVIDENCE_AGE_SECONDS,
        )
        .await
        .unwrap();
        assert_eq!(authenticated.coins.len(), 2);
        assert_eq!(
            authenticated.btcb2_tip.height,
            u64::from(fixture::BTCB2_TIP_HEIGHT)
        );
        refusal.assert_hits(0);

        // Spent on BTCB2: its address no longer lists it.
        let server = MockServer::start();
        let refusal = serve_chains(&server, &shared, &[shared[1].outpoint]);
        let (_sender, generation) = watch::channel(3);
        let source = ConnectSplitEvidence::new(client(&server), 3, generation).unwrap();
        let error = authenticate_outpoints(
            &source,
            &recorded(&shared),
            fixture::FORK,
            MAX_EVIDENCE_AGE_SECONDS,
        )
        .await
        .unwrap_err();
        assert_eq!(
            (error.outpoint, error.failure),
            (Some(shared[1].outpoint), EvidenceFailure::Btcb2Spent)
        );
        refusal.assert_hits(0);
    }

    #[tokio::test]
    async fn split_unspent_outputs_are_fresh_anonymous_reads() {
        let wallet = fixture::wallet(Shape::Wpkh);
        let coin = &fixture::shared_coins(&wallet)[0];
        let server = MockServer::start();
        let refusal = strict(&server);
        let (sender, generation) = watch::channel(3);
        let esplora = ConnectEsplora::new(
            &client(&server),
            CollectionContext {
                expected_generation: 3,
                generation,
            },
        )
        .unwrap();
        let listed = serve_fresh(
            &server,
            "bitcoin-blake2b",
            &format!("/address/{}/utxo", address(coin)),
            &format!(
                r#"[{{"txid":"{}","vout":{},"value":1,"status":{{"confirmed":false}}}}]"#,
                coin.outpoint.txid, coin.outpoint.vout
            ),
        );
        assert_eq!(
            esplora
                .unspent_outputs(ChainId::BitcoinBlake2b, &address(coin))
                .await
                .unwrap()
                .value(),
            &vec![coin.outpoint]
        );
        listed.assert();
        // A cache hit without the fresh-read acknowledgement is refused.
        let unmarked = server.mock(|when, then| {
            when.method(GET).path(format!(
                "/api/v1/esplora/bitcoin/mainnet/address/{}/utxo",
                address(coin)
            ));
            then.status(200).body("[]");
        });
        assert!(matches!(
            esplora
                .unspent_outputs(ChainId::Bitcoin, &address(coin))
                .await,
            Err(FailureKind::FreshnessUnverified)
        ));
        unmarked.assert();
        // Connect's 400 for a path it does not allow is an error, never "spent".
        assert!(matches!(
            esplora
                .unspent_outputs(ChainId::BitcoinBlake2b, "notanaddress")
                .await,
            Err(FailureKind::Http(400))
        ));
        refusal.assert_hits(1);
        assert!(matches!(
            esplora
                .unspent_outputs(ChainId::BitcoinBlake2b, "a/b")
                .await,
            Err(FailureKind::Malformed)
        ));
        assert!(matches!(
            esplora
                .unspent_outputs(ChainId::Testnet4, &address(coin))
                .await,
            Err(FailureKind::WrongChain)
        ));
        // A revoked generation reads nothing.
        sender.send_replace(4);
        assert!(matches!(
            esplora
                .unspent_outputs(ChainId::BitcoinBlake2b, &address(coin))
                .await,
            Err(FailureKind::Cancelled)
        ));
        listed.assert_hits(1);
    }
}

/// Every identifier with the text before it (trailing whitespace removed).
fn identifiers(text: &str) -> Vec<(&str, &str)> {
    let mut found = Vec::new();
    let mut start = None;
    for (index, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        let word = c.is_ascii_alphanumeric() || c == '_';
        match (word, start) {
            (true, None) => start = Some(index),
            (false, Some(from)) => {
                found.push((&text[from..index], text[..from].trim_end()));
                start = None;
            }
            _ => {}
        }
    }
    found
}

/// D1: nothing in this slice gains a GUI caller. The evidence, source and
/// file modules are reached only from their own tests; the fee sources only
/// through `app::split_fee_source`, which already priced the review.
///
/// Matches identifiers, not paths, so an alias (`use ...::split_source as
/// x`), a glob or a re-export still has to name the module or item somewhere.
#[test]
fn split_b1a_services_have_no_new_gui_callers() {
    const MODULES: [&str; 6] = [
        "split_evidence",
        "split_source",
        "split_psbt_file",
        "split_fees",
        "split_test_wallets",
        "split_test_connect",
    ];
    const ITEMS: [&str; 8] = [
        "splittable_coins",
        "authenticate_outpoints",
        "ConnectSplitEvidence",
        "ConnectEsplora",
        "ConnectBitcoinFees",
        "ConnectBtcb2Fees",
        "btcb2_fee_source",
        "bitcoin_step1_feerate",
    ];
    const OWN_FILES: [&str; 7] = [
        "src/services/split_evidence.rs",
        "src/services/split_evidence/tests.rs",
        "src/services/split_fees.rs",
        "src/services/split_psbt_file.rs",
        "src/services/split_source.rs",
        "src/services/split_test_wallets.rs",
        "src/services/split_test_connect.rs",
    ];
    fn walk(dir: &std::path::Path, files: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push((
                    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    std::fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let allowed = |file: &str, ident: &str| {
        OWN_FILES.contains(&file)
            || (file == "src/services/mod.rs" && MODULES.contains(&ident))
            || (file == "src/services/foreign_split_inventory.rs"
                && [
                    "splittable_coins",
                    "authenticate_outpoints",
                    "split_evidence",
                ]
                .contains(&ident))
            || (file == "src/services/foreign_psbt.rs"
                && ["ConnectBtcb2Fees", "split_fees"].contains(&ident))
            || (file == "src/app/mod.rs" && ["split_fees", "btcb2_fee_source"].contains(&ident))
            // B1b: the Split step-1 panel, whose only production constructor
            // is the resume of an existing journal
            // (`app::state::vault::split::tests::split_panel_has_no_gui_entry_point`).
            || file.starts_with("src/app/state/vault/split/")
            || (file == "src/app/view/vault/split.rs" && ident == "split_psbt_file")
            // B2: the dormant step-2 gate reads BTCB2 unspent outputs; it has
            // no GUI caller (`fork::split::tests::split_step2_gate_has_no_gui_caller`).
            || (file == "src/services/claim_coordinator/fork/split.rs"
                && ["split_evidence", "ConnectEsplora"].contains(&ident))
    };
    let mut unexpected = Vec::new();
    for (file, text) in &files {
        for (ident, before) in identifiers(text) {
            // `claim_coordinator` has an unrelated `split_evidence` method;
            // a method definition or call is not a path to these modules.
            let method = before.ends_with('.') || before.ends_with("fn");
            if (MODULES.contains(&ident) || ITEMS.contains(&ident))
                && !(method && MODULES.contains(&ident))
                && !allowed(file, ident)
            {
                unexpected.push((file.clone(), ident.to_owned()));
            }
        }
        // The app may only reach the BTCB2 review fee source.
        if file == "src/app/mod.rs" {
            assert_eq!(
                text.matches("split_fees").count(),
                text.matches("split_fees::btcb2_fee_source(").count(),
                "app/mod.rs uses split_fees beyond the review fee source"
            );
        }
    }
    assert!(unexpected.is_empty(), "{:?}", unexpected);
}
