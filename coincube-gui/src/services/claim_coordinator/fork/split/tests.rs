//! Split (#568 B2) step-2 gate tests. Real step-1 constructions from the
//! core builder, signed by rust-bitcoin's reference PSBT signer; a real
//! journal; a synthetic two-chain view whose Bitcoin depth, best chain, fork
//! presence, RDTS window and BTCB2 unspent set each test controls. The view
//! can change between the gate's two collections (during the BTCB2 unspent
//! read), which is how a reorg or new block at the tip is injected.
use super::*;
use crate::services::claim_coordinator::Coordinator as Step1Coordinator;
use crate::services::{
    claim_observation::{FreshRead, TransactionObservation},
    coincube::network_anchor::{AnchorState, NetworkAnchor, NetworkAnchorStatus},
    coincube::network_status::{ForkActivation, NetworkObservation, RdtsFlagday, RdtsStatus},
};
use coincube_core::{
    claim::{BlockRef, MIN_CONFIRMATIONS},
    foreign_split::{
        create_split_step1, finalize_split_step1, SplitBranch, SplitCoin, SplitInputs, SplitSource,
    },
    miniscript::{
        bitcoin::{
            absolute::LockTime,
            bip32::{DerivationPath, Xpriv, Xpub},
            secp256k1::Secp256k1,
            transaction, Amount, TxIn, TxOut,
        },
        Descriptor,
    },
};
use httpmock::prelude::*;
use reqwest::header::HeaderMap;
use serde_json::json;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    str::FromStr,
    sync::{atomic::AtomicUsize, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

const FORK: u64 = 900;
const STEP1_TIP: u32 = 960;
const TARGET: &str = "btcb2-target-cube";
/// The fork anchor: tip height and its median-time-past.
const FORK_TIP: u64 = 100;
const MTP: i64 = 8_000;
static DIRS: AtomicUsize = AtomicUsize::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "split-step2-gate-{}-{}",
            std::process::id(),
            DIRS.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(p)
    }
    fn journal(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.0.join("intent.json")).unwrap()).unwrap()
    }
    fn rewrite(&self, edit: impl FnOnce(&mut serde_json::Value)) {
        let mut value = self.journal();
        edit(&mut value);
        std::fs::write(
            self.0.join("intent.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn hash(n: u8) -> BlockHash {
    BlockHash::from_byte_array([n; 32])
}
fn policy() -> CheckPolicy {
    CheckPolicy {
        observations: Policy {
            max_observation_age_seconds: 60,
            expiry_margin_seconds: 600,
        },
        preflight: FreshnessPolicy {
            max_age_seconds: 60,
            max_future_skew_seconds: 2,
        },
        collection_budget: Duration::from_secs(2),
    }
}
fn context() -> Context {
    Context {
        generation: 7,
        account: "synthetic-account".into(),
        provider: "synthetic-provider".into(),
    }
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct Wallet {
    source: SplitSource,
    signer: Xpriv,
}
/// A P2PKH wallet: its scriptSig signatures change the txid, so the gate
/// must track the signed txid, not the unsigned one.
fn wallet() -> Wallet {
    let secp = Secp256k1::new();
    let signer = Xpriv::new_master(Network::Bitcoin, &[4; 32]).unwrap();
    let path = "m/44'/0'/0'";
    let child = signer
        .derive_priv(&secp, &DerivationPath::from_str(path).unwrap())
        .unwrap();
    let key = format!(
        "[{}/{}]{}",
        signer.fingerprint(&secp),
        path.trim_start_matches("m/"),
        Xpub::from_priv(&secp, &child)
    );
    let branch = |b: u32| Descriptor::from_str(&format!("pkh({key}/{b}/*)")).unwrap();
    Wallet {
        source: SplitSource::new(branch(0), Some(branch(1))).unwrap(),
        signer,
    }
}
fn coin(source: &SplitSource, branch: SplitBranch, index: u32, sats: u64) -> SplitCoin {
    let descriptor = match branch {
        SplitBranch::External => source.external(),
        SplitBranch::Internal => source.internal().unwrap(),
    };
    let previous = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([index as u8 + 1; 32]), 0),
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(sats),
            script_pubkey: descriptor
                .at_derivation_index(index)
                .unwrap()
                .script_pubkey(),
        }],
    };
    let block = BlockRef {
        height: FORK - 10,
        hash: hash(0x33),
    };
    SplitCoin {
        outpoint: OutPoint::new(previous.compute_txid(), 0),
        branch,
        index,
        previous,
        bitcoin_block: Some(block),
        btcb2_block: Some(block),
    }
}
fn construction(wallet: &Wallet) -> SplitStep1 {
    let coins = [
        coin(&wallet.source, SplitBranch::External, 0, 150_000),
        coin(&wallet.source, SplitBranch::Internal, 1, 70_000),
    ];
    create_split_step1(
        &SplitInputs {
            chain: ChainId::Bitcoin,
            source: &wallet.source,
            coins: &coins,
            fork_height: FORK,
            destination: 5,
        },
        2,
        LockTime::from_height(STEP1_TIP).unwrap(),
        STEP1_TIP,
        hash(7),
    )
    .unwrap()
}
fn sign(step1: &SplitStep1, wallet: &Wallet) -> VerifiedSplitStep1 {
    let secp = Secp256k1::new();
    let mut psbt = step1.psbt().clone();
    psbt.sign(&wallet.signer, &secp).unwrap();
    finalize_split_step1(step1, &psbt, &secp).unwrap()
}

type Between = Box<dyn FnOnce(&mut View) + Send>;
type ReadHook = Box<dyn FnOnce() + Send>;

/// The synthetic two-chain view.
struct View {
    bitcoin_tip: BlockRef,
    /// Step 1's confirming block on Bitcoin; `None`: absent.
    step1_block: Option<BlockRef>,
    /// Step 1 seen on the fork chain.
    on_fork: bool,
    rdts_expiry: i64,
    /// BTCB2 unspent outpoints.
    unspent: BTreeSet<OutPoint>,
    /// A BTCB2 unspent read fails.
    unspent_fails: bool,
    /// Applied once, at the first BTCB2 unspent read: between the gate's
    /// two collections.
    between: Option<Between>,
    /// Addresses with history, per chain (step-2 target freshness).
    used: Vec<(ChainId, String)>,
    /// An address-history read fails.
    address_fails: bool,
    /// Address-history reads on this chain come back an hour old.
    address_stale: Option<ChainId>,
    /// Seconds added to the services' clock (`ObservationSource::now`).
    clock_offset: i64,
    /// Transactions seen on BTCB2 (step 2), by txid.
    on_btcb2: Vec<(Txid, TransactionObservation)>,
    /// The BTCB2 anchor's tip height.
    fork_tip: u64,
    /// Seconds subtracted from the stamp of every transaction read.
    read_age: i64,
    /// Seconds subtracted from the stamp of every BTCB2 unspent read.
    unspent_age: i64,
    /// Run once, at the first BTCB2 read that finds a listed transaction.
    on_btcb2_sighting: Option<ReadHook>,
    /// Answered once, by the next BTCB2 read of that transaction.
    on_btcb2_once: Option<(Txid, TransactionObservation)>,
}
impl View {
    /// Step 1 confirmed in `block` (hash 6) at height 100 with `depth`
    /// confirmations at the tip.
    fn confirmed(depth: u64, unspent: BTreeSet<OutPoint>) -> Self {
        let mut view = Self {
            bitcoin_tip: BlockRef {
                height: 0,
                hash: hash(1),
            },
            step1_block: Some(BlockRef {
                height: 100,
                hash: hash(6),
            }),
            on_fork: false,
            rdts_expiry: 20_000,
            unspent,
            unspent_fails: false,
            between: None,
            used: Vec::new(),
            address_fails: false,
            address_stale: None,
            clock_offset: 0,
            on_btcb2: Vec::new(),
            fork_tip: FORK_TIP,
            read_age: 0,
            unspent_age: 0,
            on_btcb2_sighting: None,
            on_btcb2_once: None,
        };
        view.set_depth(depth);
        view
    }
    fn set_depth(&mut self, depth: u64) {
        let block = self.step1_block.unwrap();
        self.bitcoin_tip = BlockRef {
            height: block.height + depth - 1,
            hash: hash(1 + depth as u8),
        };
    }
}

#[derive(Clone)]
struct Chains {
    view: Arc<Mutex<View>>,
    preflight: Arc<PreflightClient>,
    unspent_reads: Arc<AtomicUsize>,
    address_reads: Arc<AtomicUsize>,
}
/// The Connect origin the synthetic services were admitted at.
const ORIGIN: &str = "https://connect.example/";
impl Chains {
    fn read<T>(chain: ChainId, value: T) -> Result<FreshRead<T>, FailureKind> {
        Self::read_aged(chain, value, 0)
    }
    /// A read stamped `age` seconds ago.
    fn read_aged<T>(chain: ChainId, value: T, age: i64) -> Result<FreshRead<T>, FailureKind> {
        let mut headers = HeaderMap::new();
        headers.insert("x-cache", "BYPASS".parse().unwrap());
        headers.insert("cache-control", "no-store".parse().unwrap());
        FreshRead::from_response(chain, value, now() - age, &headers)
    }
    fn edit(&self, edit: impl FnOnce(&mut View)) {
        edit(&mut self.view.lock().unwrap());
    }
}
#[async_trait]
impl ObservationSource for Chains {
    fn now(&self) -> i64 {
        now() + self.view.lock().unwrap().clock_offset
    }
    async fn anchor(&self, chain: ChainId) -> Result<NetworkAnchorStatus, FailureKind> {
        let (expiry, tip) = {
            let view = self.view.lock().unwrap();
            (view.rdts_expiry, view.fork_tip)
        };
        Ok(NetworkAnchorStatus {
            network: chain,
            state: AnchorState::Available,
            anchor: Some(NetworkAnchor {
                tip_hash: hash(2),
                tip_height: tip,
                tip_median_time_past: MTP,
                observed_at: now(),
                observation: NetworkObservation {
                    tip_height: tip,
                    fork: Some(ForkActivation {
                        height: 90,
                        active: true,
                    }),
                    rdts: RdtsStatus::Flagday {
                        flagday: RdtsFlagday {
                            height: 90,
                            expiry_time: expiry,
                            active: true,
                        },
                    },
                },
            }),
        })
    }
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        assert_eq!(chain, ChainId::Bitcoin);
        Self::read(chain, self.view.lock().unwrap().bitcoin_tip)
    }
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        let mut view = self.view.lock().unwrap();
        if chain != ChainId::Bitcoin {
            if view.on_btcb2_once.is_some_and(|(id, _)| id == txid) {
                let (_, once) = view.on_btcb2_once.take().unwrap();
                return Self::read_aged(chain, once, view.read_age);
            }
            if let Some((_, seen)) = view.on_btcb2.iter().find(|(id, _)| *id == txid) {
                let seen = *seen;
                if seen != TransactionObservation::Absent {
                    if let Some(hook) = view.on_btcb2_sighting.take() {
                        hook();
                    }
                }
                return Self::read_aged(chain, seen, view.read_age);
            }
        }
        let observation = match (chain, view.step1_block) {
            (ChainId::Bitcoin, Some(block)) => TransactionObservation::Confirmed { txid, block },
            (ChainId::Bitcoin, None) => TransactionObservation::Absent,
            _ if view.on_fork => TransactionObservation::Unconfirmed { txid },
            _ => TransactionObservation::Absent,
        };
        Self::read_aged(chain, observation, view.read_age)
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        let view = self.view.lock().unwrap();
        let hash = match (chain, view.step1_block) {
            (ChainId::Bitcoin, Some(block)) if block.height == height => block.hash,
            (ChainId::Bitcoin, _) => hash(0x55),
            _ => hash(2),
        };
        Self::read(chain, hash)
    }
}
#[async_trait]
impl SplitForkServices for Chains {
    fn source(&self) -> &dyn ObservationSource {
        self
    }
    fn origin(&self) -> &str {
        ORIGIN
    }
    async fn address_used(
        &self,
        chain: ChainId,
        address: &str,
    ) -> Result<FreshRead<bool>, FailureKind> {
        self.address_reads.fetch_add(1, Ordering::SeqCst);
        let view = self.view.lock().unwrap();
        if view.address_fails {
            return Err(FailureKind::Http(503));
        }
        let used = view.used.contains(&(chain, address.to_owned()));
        if view.address_stale == Some(chain) {
            let mut headers = HeaderMap::new();
            headers.insert("x-cache", "BYPASS".parse().unwrap());
            headers.insert("cache-control", "no-store".parse().unwrap());
            return FreshRead::from_response(chain, used, now() - 3_600, &headers);
        }
        Self::read(chain, used)
    }
    async fn btcb2_unspent(&self, _address: &str) -> Result<FreshRead<Vec<OutPoint>>, FailureKind> {
        self.unspent_reads.fetch_add(1, Ordering::SeqCst);
        let mut view = self.view.lock().unwrap();
        if let Some(between) = view.between.take() {
            between(&mut view);
        }
        if view.unspent_fails {
            return Err(FailureKind::Http(400));
        }
        Self::read_aged(
            ChainId::BitcoinBlake2b,
            view.unspent.iter().copied().collect(),
            view.unspent_age,
        )
    }
}
/// The step-1 coordinator over the same view, for the reorg reviews.
#[async_trait]
impl Services for Chains {
    fn source(&self) -> &dyn ObservationSource {
        self
    }
    async fn preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<Evidence, claim_preflight::Error> {
        self.preflight
            .observe(ChainId::Bitcoin, tx, tip, policy)
            .await
    }
    async fn submit(
        &self,
        _: super::super::super::step1::VerifiedStep1,
        _: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        panic!("the step-2 gate tests never submit");
    }
}

/// A Split journal whose step 1 was submitted, and a view of both chains.
struct Harness {
    temp: Temp,
    _server: MockServer,
    sender: watch::Sender<u64>,
    wallet: Wallet,
    step1: SplitStep1,
    signed: Transaction,
    chains: Chains,
}
impl Harness {
    async fn new(depth: u64) -> Self {
        let wallet = wallet();
        let step1 = construction(&wallet);
        let verified = sign(&step1, &wallet);
        let signed = verified.transaction().clone();
        assert_ne!(signed.compute_txid(), step1.txid(), "pkh: the txids differ");
        let temp = Temp::new();
        Controller::create_split(&temp.0, TARGET.into(), &step1, &verified, FORK, context())
            .unwrap();
        // The submission recorded (B1b's confirm_and_submit wrote this).
        temp.rewrite(|intent| {
            intent["phase"] = "BroadcastUncertain".into();
            intent["signed_txid"] = signed.compute_txid().to_string().into();
            intent["bitcoin_attempts"] = json!([{ "wtxid": signed.compute_wtxid().to_string() }]);
        });
        let server = MockServer::start_async().await;
        let stamp = now();
        server.mock_async(|when, then| {
            when.method(POST).path("/api/v1/esplora/bitcoin/mainnet/tx/preflight");
            then.status(200).header("cache-control", "no-store").json_body(json!({"success":true,"data":{"network":"mainnet","state":"available","result":{"txid":signed.compute_txid(),"wtxid":signed.compute_wtxid(),"tip_hash":hash(0x40),"observed_at":stamp,"allowed":true,"reject_reason":serde_json::Value::Null}}}));
        }).await;
        let sender = watch::channel(7).0;
        let chains = Chains {
            view: Arc::new(Mutex::new(View::confirmed(
                depth,
                step1.claimed_prevouts().into_iter().collect(),
            ))),
            preflight: Arc::new(
                PreflightClient::new(
                    &server.base_url(),
                    CollectionContext {
                        expected_generation: 7,
                        generation: sender.subscribe(),
                    },
                )
                .unwrap(),
            ),
            unspent_reads: Arc::new(AtomicUsize::new(0)),
            address_reads: Arc::new(AtomicUsize::new(0)),
        };
        Self {
            temp,
            _server: server,
            sender,
            wallet,
            step1,
            signed,
            chains,
        }
    }
    fn verified(&self) -> VerifiedSplitStep1 {
        let verified = sign(&self.step1, &self.wallet);
        assert_eq!(verified.transaction(), &self.signed);
        verified
    }
    fn prepare(&self) -> Result<SplitPreparation, Error> {
        self.prepare_with(policy())
    }
    fn prepare_with(&self, policy: CheckPolicy) -> Result<SplitPreparation, Error> {
        SplitPreparation::open(
            &self.temp.0,
            TARGET.into(),
            &self.step1,
            self.verified(),
            FORK,
            context(),
            self.sender.subscribe(),
            Box::new(self.chains.clone()),
            policy,
        )
    }
    /// The step-1 coordinator on the same journal (the preparation must be
    /// dropped first: both hold its lock).
    fn step1_coordinator(&self) -> Step1Coordinator {
        Step1Coordinator::open_split(
            &self.temp.0,
            TARGET.into(),
            &self.step1,
            self.verified(),
            FORK,
            context(),
            self.sender.subscribe(),
            Box::new(self.chains.clone()),
            policy(),
            true,
        )
        .unwrap()
    }
    fn prevouts(&self) -> Vec<OutPoint> {
        self.step1.claimed_prevouts()
    }
}

fn not_ready(result: Result<ForeignStep2Authorization, SplitCheckError>) -> Assessment {
    match result {
        Err(SplitCheckError::Coordinator(Error::NotReady(assessment))) => assessment,
        other => panic!("expected NotReady, got {:?}", other),
    }
}

/// No token below six confirmations of the tracked (signed) step-1 txid;
/// at six, a token bound to that txid and the claimed prevouts. The check
/// records the inclusion (Tracking) like Claim's preparation.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_needs_six_confirmations_of_the_tracked_txid() {
    assert_eq!(MIN_CONFIRMATIONS, 6);
    let h = Harness::new(5).await;
    let mut preparation = h.prepare().unwrap();
    assert_eq!(preparation.tracked_txid(), h.signed.compute_txid());
    assert_eq!(
        not_ready(preparation.check_signing(&context()).await),
        Assessment::WaitingForDepth { confirmations: 5 }
    );
    // No unspent read is spent on a check that cannot pass.
    assert_eq!(h.chains.unspent_reads.load(Ordering::SeqCst), 0);

    h.chains.edit(|view| view.set_depth(6));
    let token = preparation.check_signing(&context()).await.unwrap();
    assert!(token.is_live());
    assert_eq!(token.tracked_txid(), h.signed.compute_txid());
    assert_eq!(h.chains.unspent_reads.load(Ordering::SeqCst), 2);
    let journal = h.temp.journal();
    assert_eq!(journal["phase"], "Tracking");
    assert_eq!(journal["plan"]["previous_confirmation"]["height"], 100);
    // Nothing about step 2 is journaled by the check.
    assert!(journal.get("fork_sweep").is_none());
    assert!(token
        .redeem(
            ChainId::BitcoinBlake2b,
            7,
            &h.prevouts(),
            h.signed.compute_txid()
        )
        .is_ok());
}

/// A reorg at six confirmations: the first collection sees six, step 1 is
/// reorged out before the recheck at the tip, so no token. A new block or a
/// moved tip between the collections refuses the same way.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_recheck_at_the_tip_refuses_a_reorg_at_six() {
    let h = Harness::new(6).await;
    let mut preparation = h.prepare().unwrap();
    h.chains
        .edit(|view| view.between = Some(Box::new(|view| view.step1_block = None)));
    assert!(matches!(
        preparation.check_signing(&context()).await,
        Err(SplitCheckError::Coordinator(Error::ChangedReview))
    ));
    // Nothing was recorded from the refused view.
    assert!(h.temp.journal()["plan"]["previous_confirmation"].is_null());

    // Re-mined in a different block of the same height between the reads.
    h.chains.edit(|view| {
        view.step1_block = Some(BlockRef {
            height: 100,
            hash: hash(6),
        });
        view.set_depth(6);
        view.between = Some(Box::new(|view| {
            view.step1_block = Some(BlockRef {
                height: 100,
                hash: hash(9),
            })
        }));
    });
    assert!(matches!(
        preparation.check_signing(&context()).await,
        Err(SplitCheckError::Coordinator(Error::ChangedReview))
    ));

    // A new block between the reads.
    h.chains.edit(|view| {
        view.step1_block = Some(BlockRef {
            height: 100,
            hash: hash(6),
        });
        view.set_depth(6);
        view.between = Some(Box::new(|view| view.set_depth(7)));
    });
    assert!(matches!(
        preparation.check_signing(&context()).await,
        Err(SplitCheckError::Coordinator(Error::ChangedReview))
    ));
    // A stable view at seven passes.
    assert!(preparation.check_signing(&context()).await.is_ok());
}

/// Step 1 reorged out after it reached six: step 2 is blocked, and the
/// step-1 coordinator offers exactly the recorded bytes again after a fresh
/// preflight (the Claim resubmission review, reused).
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_blocked_when_step1_is_reorged_out_and_resend_is_exact() {
    let h = Harness::new(6).await;
    let mut preparation = h.prepare().unwrap();
    let earlier = preparation.check_signing(&context()).await.unwrap();
    h.chains.edit(|view| {
        view.step1_block = None;
        view.bitcoin_tip = BlockRef {
            height: 106,
            hash: hash(0x40),
        };
    });
    assert_eq!(
        not_ready(preparation.check_signing(&context()).await),
        Assessment::Reorged
    );
    // The refused check also killed the earlier token.
    assert!(!earlier.is_live());
    drop(preparation);

    let mut coordinator = h.step1_coordinator();
    assert_eq!(
        coordinator.reconcile(&context()).await.unwrap(),
        Status::Observation(Assessment::Reorged)
    );
    assert!(matches!(
        coordinator.prepare_reconfirmation(&context()).await,
        Err(Error::NotReady(Assessment::Reorged))
    ));
    let review = coordinator.prepare_resubmission(&context()).await.unwrap();
    assert_eq!(review.snapshot().transaction, h.signed);
    assert_eq!(review.previous_attempts(), 1);
}

/// Step 1 re-mined in another block: step 2 stays blocked until the
/// reconfirmation review of the step-1 coordinator is acknowledged; then the
/// gate counts depth from the new block.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_remined_step1_needs_the_reconfirmation_review() {
    let h = Harness::new(6).await;
    let mut preparation = h.prepare().unwrap();
    preparation.check_signing(&context()).await.unwrap();
    let moved = BlockRef {
        height: 101,
        hash: hash(9),
    };
    h.chains.edit(|view| {
        view.step1_block = Some(moved);
        view.set_depth(6);
    });
    assert_eq!(
        not_ready(preparation.check_signing(&context()).await),
        Assessment::Reorged
    );
    drop(preparation);

    let mut coordinator = h.step1_coordinator();
    let review = coordinator
        .prepare_reconfirmation(&context())
        .await
        .unwrap();
    assert_eq!(review.inclusion().previous.hash, hash(6));
    assert_eq!(review.inclusion().confirmed, moved);
    coordinator
        .confirm_reconfirmation(review, &context())
        .await
        .unwrap();
    drop(coordinator);

    let mut preparation = h.prepare().unwrap();
    let token = preparation.check_signing(&context()).await.unwrap();
    assert!(token.is_live());
    assert_eq!(
        h.temp.journal()["plan"]["previous_confirmation"]["height"],
        101
    );
}

/// A claimed prevout already spent on BTCB2 (a third party, or anyone) is
/// refused: step 2 could not sweep it. A failed read is not "spent".
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_refuses_a_claimed_coin_spent_on_btcb2() {
    let h = Harness::new(6).await;
    let mut preparation = h.prepare().unwrap();
    let spent = h.prevouts()[1];
    h.chains.edit(|view| {
        view.unspent.remove(&spent);
    });
    assert!(matches!(
        preparation.check_signing(&context()).await,
        Err(SplitCheckError::ClaimedCoinSpent(outpoint)) if outpoint == spent
    ));
    // A failed read refuses as unavailable, and supersedes the earlier token.
    h.chains.edit(|view| {
        view.unspent.insert(spent);
    });
    let earlier = preparation.check_signing(&context()).await.unwrap();
    h.chains.edit(|view| {
        view.unspent_fails = true;
    });
    assert!(matches!(
        preparation.check_signing(&context()).await,
        Err(SplitCheckError::Unavailable(_, FailureKind::Http(400)))
    ));
    assert!(!earlier.is_live());
}

/// RDTS inside the margin, the 36 h production margin, and step 1 seen on
/// the fork each refuse.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_refuses_rdts_margin_and_step1_on_the_fork() {
    let h = Harness::new(6).await;
    let mut preparation = h.prepare().unwrap();
    h.chains.edit(|view| view.rdts_expiry = MTP + 600);
    assert_eq!(
        not_ready(preparation.check_signing(&context()).await),
        Assessment::ExpiryMargin
    );
    h.chains.edit(|view| {
        view.rdts_expiry = 20_000;
        view.on_fork = true;
    });
    assert_eq!(
        not_ready(preparation.check_signing(&context()).await),
        Assessment::Step1AlreadyOnFork
    );
    drop(preparation);

    // D3: the production policy's 36 h margin.
    use crate::app::state::vault::claim::{CHECK_POLICY, EXPIRY_MARGIN_SECONDS};
    assert_eq!(EXPIRY_MARGIN_SECONDS, 36 * 3600);
    h.chains.edit(|view| {
        view.on_fork = false;
        view.rdts_expiry = MTP + EXPIRY_MARGIN_SECONDS;
    });
    let mut preparation = h.prepare_with(CHECK_POLICY).unwrap();
    assert_eq!(
        not_ready(preparation.check_signing(&context()).await),
        Assessment::ExpiryMargin
    );
    h.chains
        .edit(|view| view.rdts_expiry = MTP + EXPIRY_MARGIN_SECONDS + 1);
    assert!(preparation.check_signing(&context()).await.is_ok());
}

/// The token expires, is one-use, is superseded by a later check, and dies
/// with a generation change, a revocation (logout) or the preparation. It
/// redeems only for the checked chain, generation and prevouts.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_token_expires_is_one_use_and_revoked() {
    let h = Harness::new(6).await;
    let mut preparation = h.prepare().unwrap();
    let prevouts = h.prevouts();
    let tracked = h.signed.compute_txid();

    // Bound to the checked tracked step-1 txid (#626): the unsigned step-1
    // txid, or any other, does not redeem.
    let token = preparation.check_signing(&context()).await.unwrap();
    assert_ne!(h.step1.txid(), tracked);
    assert_eq!(
        token.redeem(ChainId::BitcoinBlake2b, 7, &prevouts, h.step1.txid()),
        Err(RedeemError::Mismatch)
    );

    // Bound to the checked prevouts, chain and generation. Each redeem
    // consumes the token, so every case takes a fresh one.
    let token = preparation.check_signing(&context()).await.unwrap();
    assert_eq!(
        token.redeem(ChainId::BitcoinBlake2b, 7, &prevouts[..1], tracked),
        Err(RedeemError::Mismatch)
    );
    let token = preparation.check_signing(&context()).await.unwrap();
    let mut extra = prevouts.clone();
    extra.push(OutPoint::new(Txid::from_byte_array([9; 32]), 0));
    assert_eq!(
        token.redeem(ChainId::BitcoinBlake2b, 7, &extra, tracked),
        Err(RedeemError::Mismatch)
    );
    let token = preparation.check_signing(&context()).await.unwrap();
    assert_eq!(
        token.redeem(ChainId::Bitcoin, 7, &prevouts, tracked),
        Err(RedeemError::Mismatch)
    );
    let token = preparation.check_signing(&context()).await.unwrap();
    assert_eq!(
        token.redeem(ChainId::BitcoinBlake2b, 8, &prevouts, tracked),
        Err(RedeemError::Mismatch)
    );
    // Order does not matter; the exact set redeems.
    let token = preparation.check_signing(&context()).await.unwrap();
    let reversed: Vec<_> = prevouts.iter().rev().copied().collect();
    assert_eq!(
        token.redeem(ChainId::BitcoinBlake2b, 7, &reversed, tracked),
        Ok(())
    );

    // Superseded by a later check, whatever its result.
    let first = preparation.check_signing(&context()).await.unwrap();
    let second = preparation.check_signing(&context()).await.unwrap();
    assert!(!first.is_live() && second.is_live());
    assert_eq!(
        first.redeem(ChainId::BitcoinBlake2b, 7, &prevouts, tracked),
        Err(RedeemError::Stale)
    );

    // Expiry: the deadline is the collection budget (2 s here) at most.
    let token = preparation.check_signing(&context()).await.unwrap();
    assert!(token.is_live());
    let deadline = token.not_after;
    assert!(deadline <= Instant::now() + Duration::from_secs(2));
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    assert!(!token.is_live());
    assert_eq!(
        token.redeem(ChainId::BitcoinBlake2b, 7, &prevouts, tracked),
        Err(RedeemError::Stale)
    );

    // Revocation (logout revokes synchronously).
    let token = preparation.check_signing(&context()).await.unwrap();
    preparation.revoker().revoke();
    assert!(!token.is_live());
    assert!(matches!(
        preparation.check_signing(&context()).await,
        Err(SplitCheckError::Coordinator(Error::Revoked))
    ));
    drop(preparation);

    // A generation change.
    let mut preparation = h.prepare().unwrap();
    let token = preparation.check_signing(&context()).await.unwrap();
    h.sender.send(8).unwrap();
    assert!(!token.is_live());
    assert_eq!(
        token.redeem(ChainId::BitcoinBlake2b, 7, &prevouts, tracked),
        Err(RedeemError::Stale)
    );
    drop(preparation);
    h.sender.send(7).unwrap();

    // Dropping the preparation (Cube closed).
    let mut preparation = h.prepare().unwrap();
    let token = preparation.check_signing(&context()).await.unwrap();
    drop(preparation);
    assert!(!token.is_live());
}

/// The preparation opens only a submitted Split step 1 that matches the
/// journal exactly, under the journal's context.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_preparation_admission() {
    let h = Harness::new(6).await;
    // Unsubmitted (phase Intent): nothing to confirm yet.
    let snapshot = h.temp.journal();
    h.temp.rewrite(|intent| {
        intent["phase"] = "Intent".into();
        intent["signed_txid"] = serde_json::Value::Null;
        intent["bitcoin_attempts"] = json!([]);
    });
    assert!(matches!(h.prepare(), Err(Error::InvalidBinding)));
    std::fs::write(
        h.temp.0.join("intent.json"),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();

    // Another construction (other coins) refuses.
    let other_wallet = wallet();
    let other = {
        let coins = [coin(&other_wallet.source, SplitBranch::External, 3, 90_000)];
        create_split_step1(
            &SplitInputs {
                chain: ChainId::Bitcoin,
                source: &other_wallet.source,
                coins: &coins,
                fork_height: FORK,
                destination: 5,
            },
            2,
            LockTime::from_height(STEP1_TIP).unwrap(),
            STEP1_TIP,
            hash(7),
        )
        .unwrap()
    };
    let other_signed = sign(&other, &other_wallet);
    assert!(SplitPreparation::open(
        &h.temp.0,
        TARGET.into(),
        &other,
        other_signed,
        FORK,
        context(),
        h.sender.subscribe(),
        Box::new(h.chains.clone()),
        policy(),
    )
    .is_err());

    // Another account's context, or a moved generation.
    let mut foreign = context();
    foreign.account = "another-account".into();
    assert!(SplitPreparation::open(
        &h.temp.0,
        TARGET.into(),
        &h.step1,
        h.verified(),
        FORK,
        foreign,
        h.sender.subscribe(),
        Box::new(h.chains.clone()),
        policy(),
    )
    .is_err());
    h.sender.send(8).unwrap();
    assert!(matches!(h.prepare(), Err(Error::InvalidBinding)));
    h.sender.send(7).unwrap();

    // One preparation at a time: the journal is locked.
    let held = h.prepare().unwrap();
    assert!(h.prepare().is_err());
    drop(held);
    assert!(h.prepare().is_ok());
}

/// The production constructor admits only the Connect origin, like the
/// step-1 Split production.
#[test]
fn split_fork_production_admits_only_a_connect_origin() {
    let (sender, _) = watch::channel(7);
    let client = |base: &str| {
        let mut client = CoincubeClient::new();
        client.base_url = base.into();
        client.set_token("synthetic-test-token");
        client
    };
    let ok = SplitForkProduction::new(
        client("https://connect.example/"),
        "synthetic-account".into(),
        7,
        sender.subscribe(),
    )
    .unwrap();
    assert_eq!(ok.context().generation, 7);
    for base in [
        "https://connect.example/api/",
        "https://user@connect.example/",
        "https://connect.example/?q=1",
    ] {
        assert!(SplitForkProduction::new(
            client(base),
            "synthetic-account".into(),
            7,
            sender.subscribe()
        )
        .is_err());
    }
    assert!(SplitForkProduction::new(
        client("https://connect.example/"),
        String::new(),
        7,
        sender.subscribe()
    )
    .is_err());
}

/// The production text of a source file: everything before its inline
/// `#[cfg(test)]\nmod tests {` module, which must then be the file's last
/// item (#626 re-review N1: a caller placed after the module is production
/// code too). rustfmt indents every line inside a module, so any other
/// column-0 line after the module's opening line is a later item. CRLF line
/// ends are accepted. A file that names `mod tests` in any other form is
/// refused rather than read whole: a miss would count the tests as
/// production (S3 item 7d).
pub(super) fn production_text<'a>(file: &str, text: &'a str) -> &'a str {
    let cut = ["#[cfg(test)]\nmod tests {", "#[cfg(test)]\r\nmod tests {"]
        .iter()
        .filter_map(|marker| text.find(marker))
        .min();
    let Some(cut) = cut else {
        assert!(
            !text.contains("mod tests"),
            "{}: `mod tests` is not an inline `#[cfg(test)]` module",
            file
        );
        return text;
    };
    let after: Vec<&str> = text[cut..]
        .lines()
        .skip(2)
        .filter(|line| !line.is_empty() && !line.starts_with(char::is_whitespace))
        .collect();
    assert_eq!(after, ["}"], "{file}: `mod tests` is not the last item");
    &text[..cut]
}

/// D1: step 2 is dormant. The gate, the step-2 construction, its handoff,
/// coordinator and transport (`fork::split` and `fork::split::step2`), and
/// the completion evidence and `split_from` writer (B5a) are named only in
/// `fork::split` itself; nothing else in the crate reaches them (B3b-2 adds
/// the panel, reachable only by resuming a journal; B5b adds its completion
/// stage). B4b-3a adds the unified fallback's coordinator and reconciler,
/// and the fork-only observation path they run, which only its own module
/// (`claim_observation`) defines.
#[test]
fn split_step2_gate_has_no_gui_caller() {
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
    let mut unexpected = Vec::new();
    for (file, text) in &files {
        if file.starts_with("src/services/claim_coordinator/fork/split") {
            continue;
        }
        for ident in [
            "SplitPreparation",
            "SplitForkProduction",
            "ForeignStep2Authorization",
            "SplitCheckError",
            // B3b: step-2 target, construction, handoff and submission.
            "SplitStep2Production",
            "SplitStep2Coordinator",
            "Step2Transport",
            "TargetError",
            "Step2Error",
            "RESERVATION_BOUND",
            "reserve_target",
            "prove_target",
            "construct_step2",
            "submit_verified_split_step2",
            "for_split_step2",
            // B3b-2: routes, restart and reconcile.
            "Step2Routes",
            "Step2Daemon",
            "SplitStep2Reconciler",
            "verify_split_step2_transaction",
            // P3-3: the reviewed resend and its restart.
            "Step2ResubmissionReview",
            "ResendError",
            "prepare_step2_resubmission",
            "confirm_step2_resubmission",
            "resume_uncertain",
            // B5a: completion evidence, the `split_from` writer and its
            // reconciliation.
            "SplitCompletionEvidence",
            "CompletionTarget",
            "SplitCompletionReconciliation",
            "check_completion",
            "reconcile_split_completion",
            // #568 S4: step 1 reorged after the step-2 submission.
            "Step1AfterStep2",
            "SweepReconcile",
            "Step1ReconfirmationReview",
            "prepare_step1_reconfirmation",
            "confirm_step1_reconfirmation",
            // B4b-3a: the unified fallback (fork-only route).
            "UnifiedCoordinator",
            "UnifiedError",
            "UnifiedReconciler",
            "UnifiedReconcile",
            "UnifiedReview",
            "UnifiedTransport",
            "submit_unified_connect",
            "submit_unified_node",
            "collect_fork_sweep",
            "ForkSweepObservation",
            "ForkAnchorView",
            "fork_anchor",
        ] {
            // The Daemon trait declares the step-2 transport and the
            // embedded daemon forwards it; neither is a caller.
            let transport = ["src/daemon/mod.rs", "src/daemon/embedded.rs"]
                .contains(&file.as_str())
                && ident.starts_with("submit_verified_split_step2");
            // B3b-2b: the panel's step-2 layer wraps these and is reached
            // only through the Split panel (`app::state::vault::split::
            // step2::tests::step2_panel_layer_is_reached_only_through_the_
            // split_panel`), itself only resuming a journal. P3-3 adds the
            // resend review and its restart there.
            let panel = file.starts_with("src/app/state/vault/split/step2")
                && [
                    "SplitPreparation",
                    "SplitForkProduction",
                    "ForeignStep2Authorization",
                    "SplitCheckError",
                    "SplitStep2Production",
                    "SplitStep2Coordinator",
                    "SplitStep2Reconciler",
                    "TargetError",
                    "Step2Error",
                    "RESERVATION_BOUND",
                    "reserve_target",
                    "prove_target",
                    "construct_step2",
                    "Step2ResubmissionReview",
                    "ResendError",
                    "prepare_step2_resubmission",
                    "confirm_step2_resubmission",
                    "resume_uncertain",
                    // B5b: the panel's completion stage.
                    "SplitCompletionEvidence",
                    "CompletionTarget",
                    "SplitCompletionReconciliation",
                    "check_completion",
                    "reconcile_split_completion",
                ]
                .contains(&ident);
            // Claim's own completion check (`fork.rs`), its tests and its
            // panel caller name `check_completion` too; Split's is a method
            // of the reconciler, which is guarded by its own name.
            // #568 S4: the panel's reconcile warning reads what became of
            // step 1 after the step-2 submission. Its O1 acknowledgement is
            // S4b's: no panel file names the reconfirmation.
            let warning = file.starts_with("src/app/state/vault/split/")
                && ["Step1AfterStep2", "SweepReconcile"].contains(&ident);
            // The fork-only observation path is defined in its own module.
            let observation = file == "src/services/claim_observation/mod.rs"
                && [
                    "collect_fork_sweep",
                    "ForkSweepObservation",
                    "ForkAnchorView",
                    "fork_anchor",
                ]
                .contains(&ident);
            let claim = ident == "check_completion"
                && (file == "src/services/claim_coordinator/fork.rs"
                    || file.starts_with("src/services/claim_coordinator/fork/tests")
                    || file.starts_with("src/app/state/vault/claim/"));
            // Core's `ForeignUnifiedError` (the seed signer's) is not the
            // coordinator's `UnifiedError`.
            let named = if ident == "UnifiedError" {
                text.replace("ForeignUnifiedError", "").contains(ident)
            } else {
                text.contains(ident)
            };
            if named && !transport && !panel && !warning && !claim && !observation {
                unexpected.push((file.clone(), ident));
            }
        }
    }
    assert!(unexpected.is_empty(), "{:?}", unexpected);
    // The token's one redeemer is the step-2 construction. `foreign_psbt.rs`
    // (its previous, scan-wide redeemer) no longer names the token in
    // production code, including anything placed after its tests module.
    let foreign_psbt = &files
        .iter()
        .find(|(file, _)| file == "src/services/foreign_psbt.rs")
        .unwrap()
        .1;
    let production = production_text("foreign_psbt.rs", foreign_psbt);
    assert!(
        production.len() < foreign_psbt.len(),
        "foreign_psbt has a tests module"
    );
    for ident in ["ForeignStep2Authorization", "redeem(", "SplitPreparation"] {
        assert!(
            !production.contains(ident),
            "foreign_psbt.rs names {}",
            ident
        );
    }
    let step2 = production_text("fork/split/step2.rs", include_str!("step2.rs"));
    assert_eq!(step2.matches(".redeem(").count(), 1);
}

/// The cut refuses a production item after the tests module, and accepts a
/// file whose tests module is last.
#[test]
fn production_text_refuses_an_item_after_the_tests_module() {
    let last = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn b() {}\n}\n";
    assert_eq!(production_text("last", last), "fn a() {}\n");
    let after = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn b() {}\n}\nfn caller() {}\n";
    assert!(std::panic::catch_unwind(|| production_text("after", after)).is_err());
    assert_eq!(production_text("none", "fn a() {}\n"), "fn a() {}\n");
}

/// S3 item 7d: the cut finds a CRLF tests module (and still refuses an item
/// after it), and a file that names `mod tests` in a form it does not cut at
/// is refused instead of being read whole.
#[test]
fn production_text_handles_crlf_and_refuses_a_missing_marker() {
    let crlf = "fn a() {}\r\n#[cfg(test)]\r\nmod tests {\r\n    fn b() {}\r\n}\r\n";
    assert_eq!(production_text("crlf", crlf), "fn a() {}\r\n");
    let after = "fn a() {}\r\n#[cfg(test)]\r\nmod tests {\r\n    fn b() {}\r\n}\r\nfn c() {}\r\n";
    assert!(std::panic::catch_unwind(|| production_text("crlf after", after)).is_err());
    for missed in [
        "fn a() {}\n#[cfg(test)]\n#[allow(unused)]\nmod tests {\n    fn b() {}\n}\n",
        "fn a() {}\n#[cfg(all(test, unix))]\nmod tests {\n    fn b() {}\n}\n",
        "fn a() {}\n#[cfg(test)]\nmod tests;\n",
    ] {
        assert!(
            std::panic::catch_unwind(|| production_text("missed", missed)).is_err(),
            "{:?}",
            missed
        );
    }
}

mod step2;
