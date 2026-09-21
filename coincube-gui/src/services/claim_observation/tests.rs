use super::*;
use crate::services::coincube::network_status::{ForkActivation, NetworkObservation, RdtsFlagday};
use coincube_core::miniscript::bitcoin::{
    absolute, hashes::Hash, transaction, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn,
    TxOut, Witness,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

fn hash(n: u8) -> BlockHash {
    BlockHash::from_byte_array([n; 32])
}
fn txid(n: u8) -> Txid {
    Txid::from_byte_array([n; 32])
}
fn policy() -> Policy {
    Policy {
        max_observation_age_seconds: 60,
        expiry_margin_seconds: 600,
    }
}
fn plan() -> ClaimPlan {
    let prevout = OutPoint {
        txid: txid(3),
        vout: 0,
    };
    let mut script = vec![0x6a, 0x4c, 87];
    script.extend_from_slice(&[1; 87]);
    ClaimPlan {
        bitcoin_chain: ChainId::Bitcoin,
        fork_chain: ChainId::BitcoinBlake2b,
        step1: Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: prevout,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::ZERO,
                script_pubkey: ScriptBuf::from_bytes(script),
            }],
        },
        claimed_prevouts: vec![prevout],
        poison: Poison::OpReturn,
        previous_confirmation: None,
    }
}
fn headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("x-cache", "BYPASS".parse().unwrap());
    h.insert(CACHE_CONTROL, "private, no-store".parse().unwrap());
    h
}
fn response<T>(chain: ChainId, value: T, stamp: i64) -> FreshRead<T> {
    FreshRead::from_response(chain, value, stamp, &headers()).unwrap()
}
fn anchor() -> NetworkAnchorStatus {
    NetworkAnchorStatus {
        network: ChainId::BitcoinBlake2b,
        state: AnchorState::Available,
        anchor: Some(NetworkAnchor {
            tip_hash: hash(2),
            tip_height: 100,
            tip_median_time_past: 8_000,
            observed_at: 10_000,
            observation: NetworkObservation {
                tip_height: 100,
                fork: Some(ForkActivation {
                    height: 90,
                    active: true,
                }),
                rdts: RdtsStatus::Flagday {
                    flagday: RdtsFlagday {
                        height: 90,
                        expiry_time: 20_000,
                        active: true,
                    },
                },
            },
        }),
    }
}
#[derive(Clone, Copy)]
enum Fault {
    None,
    WrongChain,
    WrongHash,
    WrongTxid,
    Service503,
    TipRace,
    InclusionRace,
    PresenceRace,
    Stale,
    Future,
    Hang,
    Generation,
}
struct Fixture {
    fault: Fault,
    anchor: NetworkAnchorStatus,
    tip_height: u64,
    tip_calls: AtomicUsize,
    anchor_calls: AtomicUsize,
    tx_calls: AtomicUsize,
    hash_calls: AtomicUsize,
    calls: AtomicUsize,
    paths: Mutex<Vec<ChainId>>,
    sender: watch::Sender<u64>,
}
impl Fixture {
    fn new(fault: Fault) -> Self {
        let (sender, _) = watch::channel(7);
        Self {
            fault,
            anchor: anchor(),
            tip_height: 105,
            tip_calls: AtomicUsize::new(0),
            anchor_calls: AtomicUsize::new(0),
            tx_calls: AtomicUsize::new(0),
            hash_calls: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            paths: Mutex::new(vec![]),
            sender,
        }
    }
    fn context(&self) -> CollectionContext {
        CollectionContext {
            expected_generation: 7,
            generation: self.sender.subscribe(),
        }
    }
    fn record(&self, chain: ChainId) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.paths.lock().unwrap().push(chain);
    }
    async fn run(&self, p: &ClaimPlan) -> Result<CollectedAssessment, Failure> {
        collect(self, p, policy(), Duration::from_secs(1), self.context()).await
    }
}
#[async_trait]
impl ObservationSource for Fixture {
    fn now(&self) -> i64 {
        10_000
    }
    async fn anchor(&self, chain: ChainId) -> Result<NetworkAnchorStatus, FailureKind> {
        self.record(chain);
        self.anchor_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.anchor.clone())
    }
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        self.record(chain);
        let n = self.tip_calls.fetch_add(1, Ordering::SeqCst);
        if matches!(self.fault, Fault::Hang) {
            std::future::pending::<()>().await;
        }
        if n == 1 && matches!(self.fault, Fault::Generation) {
            self.sender.send_replace(8);
        }
        let chain = if matches!(self.fault, Fault::WrongChain) {
            ChainId::Testnet4
        } else {
            chain
        };
        let stamp = match self.fault {
            Fault::Stale => 9_000,
            Fault::Future => 10_001,
            _ => 10_000,
        };
        Ok(response(
            chain,
            BlockRef {
                height: self.tip_height,
                hash: if n == 1 && matches!(self.fault, Fault::TipRace) {
                    hash(8)
                } else {
                    hash(1)
                },
            },
            stamp,
        ))
    }
    async fn transaction(
        &self,
        chain: ChainId,
        id: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        self.record(chain);
        let n = self.tx_calls.fetch_add(1, Ordering::SeqCst);
        if chain.is_blake2b() && matches!(self.fault, Fault::Service503) {
            return Err(FailureKind::Http(503));
        }
        let id = if matches!(self.fault, Fault::WrongTxid) {
            txid(8)
        } else {
            id
        };
        let value = if chain.is_blake2b() {
            if n == 3 && matches!(self.fault, Fault::PresenceRace) {
                TransactionObservation::Unconfirmed { txid: id }
            } else {
                TransactionObservation::Absent
            }
        } else {
            TransactionObservation::Confirmed {
                txid: id,
                block: BlockRef {
                    height: 100,
                    hash: hash(5),
                },
            }
        };
        Ok(response(chain, value, 10_000))
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        _: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        self.record(chain);
        let n = self.hash_calls.fetch_add(1, Ordering::SeqCst);
        let h = if matches!(self.fault, Fault::WrongHash)
            || (n == 3 && matches!(self.fault, Fault::InclusionRace))
        {
            hash(8)
        } else if chain.is_blake2b() {
            hash(2)
        } else {
            hash(5)
        };
        Ok(response(chain, h, 10_000))
    }
}

#[test]
fn explicit_bypass_and_no_store_are_both_required() {
    for (cache, control, accepted) in [
        ("BYPASS", "no-store", true),
        ("BYPASS", "private, No-Store", true),
        ("HIT", "no-store", false),
        ("MISS", "no-store", false),
        ("BYPASS", "max-age=0", false),
        ("BYPASS", "no-store=false", false),
    ] {
        let mut h = HeaderMap::new();
        h.insert("x-cache", cache.parse().unwrap());
        h.insert(CACHE_CONTROL, control.parse().unwrap());
        assert_eq!(
            FreshRead::from_response(ChainId::Bitcoin, (), 10_000, &h).is_ok(),
            accepted
        );
    }
    assert_eq!(
        FreshRead::from_response(ChainId::Bitcoin, (), 10_000, &HeaderMap::new()).unwrap_err(),
        FailureKind::FreshnessUnverified
    );
    let mut ambiguous = headers();
    ambiguous.append("x-cache", "HIT".parse().unwrap());
    assert_eq!(
        FreshRead::from_response(ChainId::Bitcoin, (), 10_000, &ambiguous).unwrap_err(),
        FailureKind::FreshnessUnverified
    );
}

#[tokio::test]
async fn stable_six_deep_bundle_only_reaches_observation_eligibility() {
    let f = Fixture::new(Fault::None);
    let result = f.run(&plan()).await.unwrap();
    assert_eq!(
        result.assessment,
        Assessment::ObservationsEligibleForPreflight
    );
    assert_eq!(
        result.observations.fork.tip,
        result.observations.deployment.tip
    );
    assert_eq!(result.observations.fork.median_time_past, 8_000);
    assert_eq!(f.tx_calls.load(Ordering::SeqCst), 4);
    assert_eq!(f.tip_calls.load(Ordering::SeqCst), 2);
    assert_eq!(f.anchor_calls.load(Ordering::SeqCst), 2);
    assert!(f
        .paths
        .lock()
        .unwrap()
        .iter()
        .all(|c| matches!(c, ChainId::Bitcoin | ChainId::BitcoinBlake2b)));
    let mut f = Fixture::new(Fault::None);
    f.tip_height = 104;
    assert_eq!(
        f.run(&plan()).await.unwrap().assessment,
        Assessment::WaitingForDepth { confirmations: 5 }
    );
}

#[tokio::test]
async fn invalid_plan_and_pair_refuse_before_any_source_call() {
    for change in 0..5 {
        let f = Fixture::new(Fault::None);
        let mut p = plan();
        match change {
            0 => p.fork_chain = ChainId::BitcoinBlake2bTestnet4,
            1 => p.claimed_prevouts.clear(),
            2 => p.step1.input[0].previous_output = OutPoint::null(),
            3 => p.step1.output[0].script_pubkey = ScriptBuf::new(),
            _ => p.poison = Poison::InputAncestry,
        }
        assert!(f.run(&p).await.is_err());
        assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    }
    let f = Fixture::new(Fault::None);
    let mut policy = policy();
    policy.expiry_margin_seconds = 0;
    assert!(
        collect(&f, &plan(), policy, Duration::from_secs(1), f.context())
            .await
            .is_err()
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn wrong_data_races_and_failures_never_produce_a_partial_bundle() {
    for (fault, stage, kind) in [
        (
            Fault::WrongChain,
            Stage::BitcoinTip,
            FailureKind::WrongChain,
        ),
        (Fault::WrongHash, Stage::ForkIndexer, FailureKind::Changed),
        (
            Fault::WrongTxid,
            Stage::BitcoinTransaction,
            FailureKind::Malformed,
        ),
        (
            Fault::Service503,
            Stage::ForkTransaction,
            FailureKind::Http(503),
        ),
        (Fault::TipRace, Stage::Preflight, FailureKind::Changed),
        (Fault::InclusionRace, Stage::Preflight, FailureKind::Changed),
        (Fault::PresenceRace, Stage::Preflight, FailureKind::Changed),
        (Fault::Stale, Stage::BitcoinTip, FailureKind::Stale),
        (Fault::Future, Stage::BitcoinTip, FailureKind::Stale),
    ] {
        assert_eq!(
            Fixture::new(fault).run(&plan()).await.unwrap_err(),
            Failure { stage, kind }
        );
    }
    // A real fresh 404 can be NotObserved, but only alongside verified poison/RDTS.
    assert_eq!(
        Fixture::new(Fault::None)
            .run(&plan())
            .await
            .unwrap()
            .observations
            .fork
            .step1_presence,
        ForkTransactionPresence::NotObserved
    );
}

#[tokio::test]
async fn previous_confirmation_reorg_is_not_retained_as_eligible() {
    let f = Fixture::new(Fault::None);
    let mut p = plan();
    p.previous_confirmation = Some(BlockRef {
        height: 100,
        hash: hash(9),
    });
    assert_eq!(f.run(&p).await.unwrap().assessment, Assessment::Reorged);
}

#[tokio::test]
async fn dynamic_rdts_uses_anchor_mtp_not_the_later_wall_clock() {
    for (active, height, expiry, expected) in [
        (
            true,
            90,
            20_000,
            Assessment::ObservationsEligibleForPreflight,
        ),
        (false, 110, 20_000, Assessment::RdtsScheduled),
        (false, 90, 20_000, Assessment::RdtsInactive),
        (false, 90, 8_000, Assessment::RdtsExpired),
        (true, 90, 8_600, Assessment::ExpiryMargin),
        (
            true,
            90,
            8_000,
            Assessment::Deployment(DeploymentState::Malformed),
        ),
    ] {
        let mut f = Fixture::new(Fault::None);
        f.anchor.anchor.as_mut().unwrap().observation.rdts = RdtsStatus::Flagday {
            flagday: RdtsFlagday {
                height,
                expiry_time: expiry,
                active,
            },
        };
        assert_eq!(f.run(&plan()).await.unwrap().assessment, expected);
    }
}

#[tokio::test]
async fn unavailable_anchor_states_remain_distinct_without_fake_tip_values() {
    for state in [
        AnchorState::NotConfigured,
        AnchorState::ConfigurationError,
        AnchorState::RpcUnavailable,
        AnchorState::Malformed,
        AnchorState::ForkAbsent,
        AnchorState::ForkInactive,
        AnchorState::RdtsAbsent,
        AnchorState::RdtsUnsupported,
        AnchorState::WrongChain,
        AnchorState::Syncing,
        AnchorState::InconsistentSnapshot,
        AnchorState::ForkUnverified,
    ] {
        let mut f = Fixture::new(Fault::None);
        f.anchor.state = state;
        f.anchor.anchor = None;
        assert_eq!(
            f.run(&plan()).await.unwrap_err(),
            failure(Stage::ForkAnchor, FailureKind::Anchor(state))
        );
    }
    for stamp in [9_000, 10_001] {
        let mut f = Fixture::new(Fault::None);
        f.anchor.anchor.as_mut().unwrap().observed_at = stamp;
        assert_eq!(f.run(&plan()).await.unwrap_err().kind, FailureKind::Stale);
    }
}

#[tokio::test]
async fn generation_change_and_timeout_cancel_collection_without_retry() {
    let f = Fixture::new(Fault::Generation);
    assert_eq!(
        f.run(&plan()).await.unwrap_err(),
        failure(Stage::Context, FailureKind::Cancelled)
    );
    let f = Fixture::new(Fault::Hang);
    let result = collect(&f, &plan(), policy(), Duration::from_millis(1), f.context()).await;
    assert_eq!(
        result.unwrap_err(),
        failure(Stage::Context, FailureKind::Deadline)
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    let context = f.context();
    f.sender.send_replace(8);
    assert_eq!(
        collect(&f, &plan(), policy(), Duration::from_secs(1), context)
            .await
            .unwrap_err()
            .kind,
        FailureKind::Cancelled
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn testnet4_pair_stays_separate_from_mainnet() {
    let mut f = Fixture::new(Fault::None);
    f.anchor.network = ChainId::BitcoinBlake2bTestnet4;
    let mut p = plan();
    p.bitcoin_chain = ChainId::Testnet4;
    p.fork_chain = ChainId::BitcoinBlake2bTestnet4;
    assert_eq!(
        f.run(&p).await.unwrap().assessment,
        Assessment::ObservationsEligibleForPreflight
    );
    assert!(f
        .paths
        .lock()
        .unwrap()
        .iter()
        .all(|c| matches!(c, ChainId::Testnet4 | ChainId::BitcoinBlake2bTestnet4)));
}

#[test]
fn malformed_anchor_projection_cannot_invent_a_coherent_view() {
    for change in 0..5 {
        let mut a = anchor();
        match change {
            0 => a.anchor = None,
            1 => a.anchor.as_mut().unwrap().observation.tip_height = 99,
            2 => a.anchor.as_mut().unwrap().observation.fork = None,
            3 => a.anchor.as_mut().unwrap().observation.rdts = RdtsStatus::Absent,
            _ => a.anchor.as_mut().unwrap().tip_median_time_past = -1,
        }
        assert_eq!(
            project_anchor(
                a,
                ChainId::BitcoinBlake2b,
                txid(1),
                ForkTransactionPresence::NotObserved,
                policy(),
                10_000
            )
            .unwrap_err()
            .kind,
            FailureKind::Malformed
        );
    }
}

#[tokio::test]
async fn revocation_drops_an_in_flight_read_and_closed_context_refuses() {
    let f = Fixture::new(Fault::Hang);
    let sender = f.sender.clone();
    let revoke = tokio::spawn(async move {
        tokio::task::yield_now().await;
        sender.send_replace(8);
    });
    assert_eq!(
        f.run(&plan()).await.unwrap_err().kind,
        FailureKind::Cancelled
    );
    revoke.await.unwrap();
    assert!(f.calls.load(Ordering::SeqCst) <= 1);
    let (sender, receiver) = watch::channel(7);
    drop(sender);
    let f = Fixture::new(Fault::None);
    let context = CollectionContext {
        expected_generation: 7,
        generation: receiver,
    };
    assert_eq!(
        collect(&f, &plan(), policy(), Duration::from_secs(1), context)
            .await
            .unwrap_err()
            .kind,
        FailureKind::Cancelled
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn collection_future_can_be_owned_by_a_send_task() {
    fn require_send<T: Send>(_: T) {}
    let f = Fixture::new(Fault::None);
    let p = plan();
    require_send(collect(
        &f,
        &p,
        policy(),
        Duration::from_secs(1),
        f.context(),
    ));
}
