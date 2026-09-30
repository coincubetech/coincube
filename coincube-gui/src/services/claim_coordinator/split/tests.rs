//! Split (#568 B0b) coordinator tests. Real step-1 constructions from the
//! core builder, signed by rust-bitcoin's reference PSBT signer and verified
//! by `finalize_split_step1`. Observations come from a synthetic source;
//! operator preflight and the Connect submit endpoint are local HTTP mocks.
use super::*;
use crate::services::{
    claim_observation::{FailureKind, FreshRead, TransactionObservation},
    coincube::{
        network_anchor::{AnchorState, NetworkAnchor, NetworkAnchorStatus},
        network_status::{ForkActivation, NetworkObservation, RdtsFlagday, RdtsStatus},
    },
};
use coincube_core::{
    claim::BlockRef,
    foreign_split::{
        create_split_step1, finalize_split_step1, SplitBranch, SplitCoin, SplitInputs, SplitSource,
    },
    miniscript::{
        bitcoin::{
            absolute::LockTime,
            bip32::{DerivationPath, Xpriv, Xpub},
            secp256k1::Secp256k1,
            transaction, Amount, Network, OutPoint, TxIn, TxOut,
        },
        Descriptor,
    },
};
use coincubed::poison_broadcast::{SubmissionError, SubmissionState};
use httpmock::prelude::*;
use reqwest::header::HeaderMap;
use serde_json::json;
use std::{
    path::PathBuf,
    str::FromStr,
    sync::atomic::{AtomicI64, AtomicUsize},
    time::{SystemTime, UNIX_EPOCH},
};

const FORK: u64 = 900;
const TIP: u32 = 960;
const TARGET: &str = "btcb2-target-cube";

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "split-coordinator-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(p)
    }
    fn journal(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.0.join("intent.json")).unwrap()).unwrap()
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
fn stamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Native segwit: the signed txid equals the unsigned one.
    Wpkh,
    /// Legacy: the scriptSig signatures change the txid.
    Pkh,
}
struct Wallet {
    source: SplitSource,
    signer: Xpriv,
}
fn make_wallet(shape: Shape, seed: u8) -> Wallet {
    let secp = Secp256k1::new();
    let signer = Xpriv::new_master(Network::Bitcoin, &[seed; 32]).unwrap();
    let (script, path) = match shape {
        Shape::Wpkh => ("wpkh", "m/84'/0'/0'"),
        Shape::Pkh => ("pkh", "m/44'/0'/0'"),
    };
    let child = signer
        .derive_priv(&secp, &DerivationPath::from_str(path).unwrap())
        .unwrap();
    let key = format!(
        "[{}/{}]{}",
        signer.fingerprint(&secp),
        path.trim_start_matches("m/"),
        Xpub::from_priv(&secp, &child)
    );
    let branch = |b: u32| Descriptor::from_str(&format!("{script}({key}/{b}/*)")).unwrap();
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
fn construction(wallet: &Wallet, chain: ChainId, feerate: u64) -> SplitStep1 {
    let coins = [
        coin(&wallet.source, SplitBranch::External, 0, 150_000),
        coin(&wallet.source, SplitBranch::Internal, 1, 70_000),
    ];
    create_split_step1(
        &SplitInputs {
            chain,
            source: &wallet.source,
            coins: &coins,
            fork_height: FORK,
            destination: 5,
        },
        feerate,
        LockTime::from_height(TIP).unwrap(),
        TIP,
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

/// A synthetic, fresh two-chain view in which step 1 is absent (fault 0),
/// the read fails (3), or step 1 is confirmed on Bitcoin (6).
struct Fixture {
    preflight: PreflightClient,
    stamp: i64,
    clock: Arc<AtomicI64>,
    fault: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    reached: Arc<tokio::sync::Notify>,
    submitted: Arc<Mutex<Vec<(Txid, SubmissionState)>>>,
    directory: PathBuf,
}
impl Fixture {
    fn read<T>(&self, chain: ChainId, value: T) -> Result<FreshRead<T>, FailureKind> {
        if self.fault.load(Ordering::SeqCst) == 3 {
            return Err(FailureKind::Http(503));
        }
        let mut headers = HeaderMap::new();
        headers.insert("x-cache", "BYPASS".parse().unwrap());
        headers.insert("cache-control", "no-store".parse().unwrap());
        FreshRead::from_response(chain, value, self.stamp, &headers)
    }
}
#[async_trait]
impl ObservationSource for Fixture {
    fn now(&self) -> i64 {
        self.clock.load(Ordering::SeqCst)
    }
    async fn anchor(&self, chain: ChainId) -> Result<NetworkAnchorStatus, FailureKind> {
        Ok(NetworkAnchorStatus {
            network: chain,
            state: AnchorState::Available,
            anchor: Some(NetworkAnchor {
                tip_hash: hash(2),
                tip_height: 100,
                tip_median_time_past: 8000,
                observed_at: self.stamp,
                observation: NetworkObservation {
                    tip_height: 100,
                    fork: Some(ForkActivation {
                        height: 90,
                        active: true,
                    }),
                    rdts: RdtsStatus::Flagday {
                        flagday: RdtsFlagday {
                            height: 90,
                            expiry_time: 20000,
                            active: true,
                        },
                    },
                },
            }),
        })
    }
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        self.read(
            chain,
            BlockRef {
                height: 105,
                hash: hash(1),
            },
        )
    }
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        let observation = if chain == ChainId::Bitcoin && self.fault.load(Ordering::SeqCst) == 6 {
            TransactionObservation::Confirmed {
                txid,
                block: BlockRef {
                    height: 100,
                    hash: hash(6),
                },
            }
        } else {
            TransactionObservation::Absent
        };
        self.read(chain, observation)
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        _height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        let fault = self.fault.load(Ordering::SeqCst);
        self.read(
            chain,
            hash(if chain.is_blake2b() {
                2
            } else if fault == 6 {
                6
            } else {
                1
            }),
        )
    }
}
#[async_trait]
impl Services for Fixture {
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
    /// Only the Split variant reaches transport, after the Split intent is
    /// durable (fault 4: transport error, 5: never answers).
    async fn submit(
        &self,
        tx: VerifiedStep1,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        assert!(matches!(tx, VerifiedStep1::Split(_)));
        let journal: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.directory.join("intent.json")).unwrap())
                .unwrap();
        assert_eq!(journal["version"], 8);
        assert_eq!(journal["phase"], "BroadcastUncertain");
        assert_eq!(
            journal["bitcoin_attempts"].as_array().unwrap().len(),
            self.calls.load(Ordering::SeqCst) + 1
        );
        assert_eq!(
            journal["signed_txid"],
            tx.transaction().compute_txid().to_string()
        );
        self.submitted
            .lock()
            .unwrap()
            .push((tx.transaction().compute_txid(), gate.state()));
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.reached.notify_one();
        match self.fault.load(Ordering::SeqCst) {
            5 => std::future::pending::<()>().await,
            4 => return Err(DaemonError::DaemonStopped),
            _ => {}
        }
        Ok(SubmissionOutcome::UpstreamAccepted {
            txid: tx.transaction().compute_txid(),
            wtxid: tx.transaction().compute_wtxid(),
        })
    }
}

/// Everything a Split coordinator in these tests is opened with.
struct Harness {
    server: MockServer,
    temp: Temp,
    sender: watch::Sender<u64>,
    fault: Arc<AtomicUsize>,
    clock: Arc<AtomicI64>,
    calls: Arc<AtomicUsize>,
    reached: Arc<tokio::sync::Notify>,
    submitted: Arc<Mutex<Vec<(Txid, SubmissionState)>>>,
    wallet: Wallet,
    step1: SplitStep1,
}
impl Harness {
    /// A created Split coordinator over a fresh journal, and its harness.
    async fn new(shape: Shape) -> (Self, Coordinator) {
        let wallet = make_wallet(shape, 1);
        let step1 = construction(&wallet, ChainId::Bitcoin, 2);
        let verified = sign(&step1, &wallet);
        let server = MockServer::start_async().await;
        let stamp = stamp();
        let tx = verified.transaction();
        server.mock_async(|when, then| {
            when.method(POST).path("/api/v1/esplora/bitcoin/mainnet/tx/preflight");
            then.status(200).header("cache-control", "no-store").json_body(json!({"success":true,"data":{"network":"mainnet","state":"available","result":{"txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),"tip_hash":hash(1),"observed_at":stamp,"allowed":true,"reject_reason":serde_json::Value::Null}}}));
        }).await;
        let h = Self {
            server,
            temp: Temp::new(),
            sender: watch::channel(7).0,
            fault: Arc::new(AtomicUsize::new(0)),
            clock: Arc::new(AtomicI64::new(stamp)),
            calls: Arc::new(AtomicUsize::new(0)),
            reached: Arc::new(tokio::sync::Notify::new()),
            submitted: Arc::new(Mutex::new(Vec::new())),
            wallet,
            step1,
        };
        let coordinator = h.open(verified, false).unwrap();
        (h, coordinator)
    }
    fn services(&self) -> Fixture {
        Fixture {
            preflight: PreflightClient::new(
                &self.server.base_url(),
                CollectionContext {
                    expected_generation: 7,
                    generation: self.sender.subscribe(),
                },
            )
            .unwrap(),
            stamp: self.clock.load(Ordering::SeqCst),
            clock: self.clock.clone(),
            fault: self.fault.clone(),
            calls: self.calls.clone(),
            reached: self.reached.clone(),
            submitted: self.submitted.clone(),
            directory: self.temp.0.clone(),
        }
    }
    fn open(&self, verified: VerifiedSplitStep1, resume: bool) -> Result<Coordinator, Error> {
        self.open_with(&self.step1, verified, resume)
    }
    fn open_with(
        &self,
        step1: &SplitStep1,
        verified: VerifiedSplitStep1,
        resume: bool,
    ) -> Result<Coordinator, Error> {
        Coordinator::open_split(
            &self.temp.0,
            TARGET.into(),
            step1,
            verified,
            FORK,
            context(),
            self.sender.subscribe(),
            Box::new(self.services()),
            policy(),
            resume,
        )
    }
    fn signed(&self) -> VerifiedSplitStep1 {
        sign(&self.step1, &self.wallet)
    }
}

fn client(server: &MockServer) -> CoincubeClient {
    let mut client = CoincubeClient::new();
    client.base_url = format!("{}/", server.base_url());
    client.set_token("synthetic-test-token");
    client
}
fn production(server: &MockServer, generation: watch::Receiver<u64>) -> SplitProduction {
    SplitProduction::new(
        client(server),
        "synthetic-account".into(),
        7,
        generation,
        ChainId::Bitcoin,
    )
    .unwrap()
}

/// (d) The Split review is the Claim review over the signed step 1: it is
/// keyed by the signed txid (a P2PKH scriptSig changes it), records the Split
/// intent (not the Claim one, which refuses it) before transport, and hands
/// transport the Split artifact with a pending gate, once.
#[tokio::test]
async fn split_review_records_the_split_intent_before_one_connect_submission() {
    for shape in [Shape::Pkh, Shape::Wpkh] {
        let (h, mut coordinator) = Harness::new(shape).await;
        let signed = h.signed();
        let txid = signed.transaction().compute_txid();
        assert_eq!(shape == Shape::Pkh, txid != h.step1.txid());
        let review = coordinator.prepare_review(&context()).await.unwrap();
        let snapshot = review.snapshot();
        assert_eq!(snapshot.txid, txid);
        assert_eq!(snapshot.wtxid, signed.transaction().compute_wtxid());
        assert_eq!(snapshot.route, SubmissionRoute::Connect);
        assert_eq!(snapshot.fee_sats, signed.fee().to_sat());
        assert_eq!(snapshot.vsize, signed.vsize());
        assert_eq!(
            snapshot.wallet,
            claim_workflow::split_identity(TARGET.into(), h.step1.source().digest())
        );
        assert_eq!(h.temp.journal()["phase"], "Intent");
        let outcome = coordinator
            .confirm_and_submit(review, &context())
            .await
            .unwrap();
        assert_eq!(
            outcome,
            Outcome::UpstreamAccepted {
                txid,
                wtxid: signed.transaction().compute_wtxid()
            }
        );
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *h.submitted.lock().unwrap(),
            vec![(txid, SubmissionState::Pending)]
        );
        assert_eq!(coordinator.phase(), Phase::BroadcastUncertain);
        assert_eq!(h.temp.journal()["signed_txid"], txid.to_string());
        assert!(matches!(
            coordinator.prepare_review(&context()).await,
            Err(Error::SubmissionAlreadyRecorded)
        ));
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    }
}

/// (d) Resume is Unchecked and an uncertain submission only reconciles: a
/// failed or cancelled transport leaves BroadcastUncertain; the resumed
/// coordinator reports the recorded outcome, refuses a new review, and
/// reconcile tracks the signed txid without any further transport call.
#[tokio::test]
async fn split_resume_is_unchecked_and_an_uncertain_submission_only_reconciles() {
    for fault in [4, 5] {
        let (h, mut coordinator) = Harness::new(Shape::Pkh).await;
        let signed = h.signed();
        let txid = signed.transaction().compute_txid();
        let wtxid = signed.transaction().compute_wtxid();
        let review = coordinator.prepare_review(&context()).await.unwrap();
        h.fault.store(fault, Ordering::SeqCst);
        let sender = h.sender.clone();
        let reached = h.reached.clone();
        // Fault 5: the transport never answers and the session generation
        // changes mid-submit.
        let cancel = async move {
            if fault == 5 {
                reached.notified().await;
                sender.send(8).unwrap();
            }
        };
        let current = context();
        let (outcome, ()) = tokio::join!(coordinator.confirm_and_submit(review, &current), cancel);
        assert_eq!(outcome.unwrap(), Outcome::Uncertain { txid, wtxid });
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
        assert_eq!(h.temp.journal()["phase"], "BroadcastUncertain");
        // The journal lock is released with the coordinator.
        drop(coordinator);
        if fault == 5 {
            // A revoked session cannot resume under its old generation.
            assert!(matches!(
                h.open(h.signed(), true),
                Err(Error::InvalidBinding)
            ));
            h.sender.send_replace(7);
        }

        let mut resumed = h.open(h.signed(), true).unwrap();
        assert_eq!(resumed.phase(), Phase::BroadcastUncertain);
        assert_eq!(resumed.controller.status(), Status::Unchecked);
        assert_eq!(
            resumed.recorded_outcome(),
            Some(Outcome::Uncertain { txid, wtxid })
        );
        assert!(matches!(
            resumed.prepare_review(&context()).await,
            Err(Error::SubmissionAlreadyRecorded)
        ));
        h.fault.store(6, Ordering::SeqCst);
        assert_eq!(
            resumed.reconcile(&context()).await.unwrap(),
            Status::Observation(Assessment::ObservationsEligibleForPreflight)
        );
        assert_eq!(h.temp.journal()["signed_txid"], txid.to_string());
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    }
}

/// (d) A resume before any submission is Unchecked too: nothing is carried
/// over, and a review still needs fresh observations and preflight.
#[tokio::test]
async fn split_resume_at_intent_requires_a_fresh_review() {
    let (h, coordinator) = Harness::new(Shape::Pkh).await;
    let identity = coordinator.wallet_identity().clone();
    drop(coordinator);
    let mut resumed = h.open(h.signed(), true).unwrap();
    assert_eq!(resumed.wallet_identity(), &identity);
    assert_eq!(resumed.phase(), Phase::Intent);
    assert_eq!(resumed.controller.status(), Status::Unchecked);
    assert_eq!(resumed.recorded_outcome(), None);
    h.fault.store(3, Ordering::SeqCst);
    assert!(resumed.prepare_review(&context()).await.is_err());
    h.fault.store(0, Ordering::SeqCst);
    let review = resumed.prepare_review(&context()).await.unwrap();
    assert_eq!(review.snapshot().route, SubmissionRoute::Connect);
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
}

/// (c) Coordinator admission refusals, each before any journal write or
/// transport: a non-Bitcoin construction, a signed step 1 of another
/// construction, a stale generation, another source's journal on resume, and
/// a changed construction on resume.
#[tokio::test]
async fn split_admission_refuses_chain_binding_generation_source_and_construction() {
    let (h, coordinator) = Harness::new(Shape::Wpkh).await;
    drop(coordinator);
    let recorded = std::fs::read(h.temp.0.join("intent.json")).unwrap();
    let fresh = || {
        let temp = Temp::new();
        let open = |step1: &SplitStep1, verified, generation| {
            Coordinator::open_split(
                &temp.0,
                TARGET.into(),
                step1,
                verified,
                FORK,
                context(),
                generation,
                Box::new(h.services()),
                policy(),
                false,
            )
        };
        let testnet = construction(&h.wallet, ChainId::Testnet4, 2);
        let result = open(&testnet, sign(&testnet, &h.wallet), h.sender.subscribe());
        assert!(matches!(result, Err(Error::Unsupported)));
        let other = construction(&h.wallet, ChainId::Bitcoin, 3);
        assert!(matches!(
            open(&h.step1, sign(&other, &h.wallet), h.sender.subscribe()),
            Err(Error::InvalidBinding)
        ));
        let (_stale_sender, stale) = watch::channel(6);
        assert!(matches!(
            open(&h.step1, h.signed(), stale),
            Err(Error::InvalidBinding)
        ));
        let (closed_sender, closed) = watch::channel(7);
        drop(closed_sender);
        assert!(matches!(
            open(&h.step1, h.signed(), closed),
            Err(Error::InvalidBinding)
        ));
        let mut invalid = policy();
        invalid.collection_budget = Duration::ZERO;
        assert!(matches!(
            Coordinator::open_split(
                &temp.0,
                TARGET.into(),
                &h.step1,
                h.signed(),
                FORK,
                context(),
                h.sender.subscribe(),
                Box::new(h.services()),
                invalid,
                false,
            ),
            Err(Error::Unsupported)
        ));
        assert!(!temp.0.join("intent.json").exists());
    };
    fresh();

    // A second create over the recorded journal is refused.
    assert!(matches!(
        h.open(h.signed(), false),
        Err(Error::Journal(claim_workflow::Error::Conflict))
    ));
    // Resume with another source: the identity (source digest) differs.
    let foreign = make_wallet(Shape::Wpkh, 2);
    let foreign_step1 = construction(&foreign, ChainId::Bitcoin, 2);
    assert_ne!(foreign_step1.source().digest(), h.step1.source().digest());
    assert!(matches!(
        h.open_with(&foreign_step1, sign(&foreign_step1, &foreign), true),
        Err(Error::Journal(claim_workflow::Error::WrongIdentity))
    ));
    // Resume with the same source but another construction (fee).
    let changed = construction(&h.wallet, ChainId::Bitcoin, 3);
    assert_ne!(changed.txid(), h.step1.txid());
    assert!(matches!(
        h.open_with(&changed, sign(&changed, &h.wallet), true),
        Err(Error::Journal(claim_workflow::Error::WrongIdentity))
    ));
    // Resume with another fork height than recorded.
    assert!(matches!(
        Coordinator::open_split(
            &h.temp.0,
            TARGET.into(),
            &h.step1,
            h.signed(),
            FORK + 1,
            context(),
            h.sender.subscribe(),
            Box::new(h.services()),
            policy(),
            true,
        ),
        Err(Error::Journal(claim_workflow::Error::WrongIdentity))
    ));
    // Resume under another target Cube.
    assert!(matches!(
        Coordinator::open_split(
            &h.temp.0,
            "another-cube".into(),
            &h.step1,
            h.signed(),
            FORK,
            context(),
            h.sender.subscribe(),
            Box::new(h.services()),
            policy(),
            true,
        ),
        Err(Error::Journal(claim_workflow::Error::WrongIdentity))
    ));
    assert_eq!(
        std::fs::read(h.temp.0.join("intent.json")).unwrap(),
        recorded
    );
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
}

/// (c) `SplitProduction` admits only the Bitcoin mainnet Connect route with
/// the Claim origin checks, and its identity is the same Connect provider.
#[tokio::test]
async fn split_production_admits_only_the_bitcoin_connect_origin() {
    let server = MockServer::start_async().await;
    let (_sender, generation) = watch::channel(7u64);
    for chain in [
        ChainId::Testnet4,
        ChainId::BitcoinBlake2b,
        ChainId::BitcoinBlake2bTestnet4,
    ] {
        assert!(matches!(
            SplitProduction::new(
                client(&server),
                "synthetic-account".into(),
                7,
                generation.clone(),
                chain
            ),
            Err(Error::Unsupported)
        ));
    }
    assert!(matches!(
        SplitProduction::new(
            client(&server),
            String::new(),
            7,
            generation.clone(),
            ChainId::Bitcoin
        ),
        Err(Error::Unsupported)
    ));
    let origin = format!("{}/", server.base_url());
    for invalid in [
        format!("{}unexpected", origin),
        format!("{}?token=synthetic", origin),
        format!("{}#fragment", origin),
        origin.replace("http://", "http://synthetic:password@"),
        "file:///tmp/synthetic".into(),
        "not a url".into(),
    ] {
        let mut client = client(&server);
        client.base_url = invalid;
        assert!(matches!(
            SplitProduction::new(
                client,
                "synthetic-account".into(),
                7,
                generation.clone(),
                ChainId::Bitcoin
            ),
            Err(Error::InvalidBinding)
        ));
    }
    // Observation needs the admitted session's token.
    let mut anonymous = CoincubeClient::new();
    anonymous.base_url = origin.clone();
    assert!(matches!(
        SplitProduction::new(
            anonymous,
            "synthetic-account".into(),
            7,
            generation.clone(),
            ChainId::Bitcoin
        ),
        Err(Error::InvalidBinding)
    ));
    let production = production(&server, generation);
    assert_eq!(
        production.context().provider,
        format!(
            "bitcoin|{}/api/v1/esplora/bitcoin/mainnet",
            server.base_url()
        )
    );
    assert_eq!(production.context().generation, 7);
    assert_eq!(production.context().account, "synthetic-account");
}

/// (c)(e) Through `SplitProduction`, the Split artifact goes to Connect once
/// with its exact bytes, via the blocking worker; a node route and a gate of
/// another transaction are refused before any HTTP request.
#[tokio::test]
async fn split_production_submits_once_to_connect_and_refuses_a_node_route() {
    let server = MockServer::start_async().await;
    let (_sender, generation) = watch::channel(7u64);
    let production = production(&server, generation);
    let wallet = make_wallet(Shape::Pkh, 1);
    let step1 = construction(&wallet, ChainId::Bitcoin, 2);
    let verified = Arc::new(sign(&step1, &wallet));
    let tx = verified.transaction().clone();
    let endpoint = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/api/v1/esplora/bitcoin/mainnet/tx")
                .body(coincube_core::miniscript::bitcoin::consensus::encode::serialize_hex(&tx));
            then.status(200).body(tx.compute_txid().to_string());
        })
        .await;
    let deadline = Instant::now() + Duration::from_secs(30);

    let node = route::BoundNode::new(coincubed::config::BitcoindConfig {
        addr: *server.address(),
        rpc_auth: coincubed::config::BitcoindRpcAuth::UserPass(
            "synthetic".into(),
            "fixture".into(),
        ),
    })
    .route();
    let (gate, _revoker) = SubmissionGate::for_split_step1(&verified, deadline);
    let gate = Arc::new(gate);
    assert!(matches!(
        production
            .submit_route(node, VerifiedStep1::Split(verified.clone()), gate.clone())
            .await,
        Err(DaemonError::ClientNotSupported)
    ));
    assert_eq!(gate.state(), SubmissionState::Pending);

    let other_wallet = make_wallet(Shape::Pkh, 2);
    let other_step1 = construction(&other_wallet, ChainId::Bitcoin, 2);
    let other = sign(&other_step1, &other_wallet);
    let (foreign, _) = SubmissionGate::for_split_step1(&other, deadline);
    let foreign = Arc::new(foreign);
    assert!(matches!(
        production
            .submit_route(
                SubmissionRoute::Connect,
                VerifiedStep1::Split(verified.clone()),
                foreign.clone()
            )
            .await,
        Err(DaemonError::PoisonSubmission(SubmissionError::GateMismatch))
    ));
    assert_eq!(foreign.state(), SubmissionState::Pending);
    endpoint.assert_hits_async(0).await;

    assert_eq!(
        production
            .submit_route(
                SubmissionRoute::Connect,
                VerifiedStep1::Split(verified.clone()),
                gate.clone()
            )
            .await
            .unwrap(),
        SubmissionOutcome::UpstreamAccepted {
            txid: tx.compute_txid(),
            wtxid: tx.compute_wtxid()
        }
    );
    assert_eq!(gate.state(), SubmissionState::Started);
    assert!(matches!(
        production
            .submit(VerifiedStep1::Split(verified.clone()), gate.clone())
            .await,
        Err(DaemonError::PoisonSubmission(
            SubmissionError::AlreadyStarted
        ))
    ));
    endpoint.assert_hits_async(1).await;
}

/// (c) End to end through `create_split` / `resume_split` with the real
/// production: a stale generation refuses before the journal; resume
/// reopens the recorded Split and refuses another source.
#[tokio::test]
async fn split_create_and_resume_with_the_connect_production() {
    let server = MockServer::start_async().await;
    let temp = Temp::new();
    let wallet = make_wallet(Shape::Pkh, 1);
    let step1 = construction(&wallet, ChainId::Bitcoin, 2);
    let (sender, generation) = watch::channel(8u64);
    // The session already moved past the admitted generation 7.
    assert!(matches!(
        Coordinator::create_split(
            &temp.0,
            TARGET.into(),
            &step1,
            sign(&step1, &wallet),
            FORK,
            production(&server, generation.clone()),
            policy(),
        ),
        Err(Error::InvalidBinding)
    ));
    assert!(!temp.0.join("intent.json").exists());
    sender.send(7).unwrap();
    let created = Coordinator::create_split(
        &temp.0,
        TARGET.into(),
        &step1,
        sign(&step1, &wallet),
        FORK,
        production(&server, sender.subscribe()),
        policy(),
    )
    .unwrap();
    assert_eq!(created.phase(), Phase::Intent);
    assert!(matches!(created.verified, VerifiedStep1::Split(_)));
    assert_eq!(
        temp.journal()["plan"]["tracked_txid"],
        sign(&step1, &wallet)
            .transaction()
            .compute_txid()
            .to_string()
    );
    drop(created);
    let resumed = Coordinator::resume_split(
        &temp.0,
        TARGET.into(),
        &step1,
        sign(&step1, &wallet),
        FORK,
        production(&server, sender.subscribe()),
        policy(),
    )
    .unwrap();
    assert_eq!(resumed.controller.status(), Status::Unchecked);
    drop(resumed);
    let foreign = make_wallet(Shape::Pkh, 2);
    let foreign_step1 = construction(&foreign, ChainId::Bitcoin, 2);
    assert!(matches!(
        Coordinator::resume_split(
            &temp.0,
            TARGET.into(),
            &foreign_step1,
            sign(&foreign_step1, &foreign),
            FORK,
            production(&server, sender.subscribe()),
            policy(),
        ),
        Err(Error::Journal(claim_workflow::Error::WrongIdentity))
    ));
}

/// (d) The Claim resubmission review is reused unchanged: after an uncertain
/// Split submission, only an explicit fresh review resends exactly the
/// recorded signed step 1, recording the second attempt before transport.
#[tokio::test]
async fn split_resubmission_needs_an_explicit_review_and_resends_the_same_bytes() {
    let (h, mut coordinator) = Harness::new(Shape::Pkh).await;
    let signed = h.signed();
    let review = coordinator.prepare_review(&context()).await.unwrap();
    h.fault.store(4, Ordering::SeqCst);
    assert!(matches!(
        coordinator.confirm_and_submit(review, &context()).await,
        Ok(Outcome::Uncertain { .. })
    ));
    h.fault.store(0, Ordering::SeqCst);
    coordinator.reconcile(&context()).await.unwrap();
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    let review = coordinator.prepare_resubmission(&context()).await.unwrap();
    assert_eq!(review.previous_attempts(), 1);
    assert_eq!(review.snapshot().transaction, *signed.transaction());
    assert_eq!(review.snapshot().route, SubmissionRoute::Connect);
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        coordinator.confirm_resubmission(review, &context()).await,
        Ok(Outcome::UpstreamAccepted { .. })
    ));
    assert_eq!(h.calls.load(Ordering::SeqCst), 2);
    let txid = signed.transaction().compute_txid();
    assert_eq!(
        *h.submitted.lock().unwrap(),
        vec![
            (txid, SubmissionState::Pending),
            (txid, SubmissionState::Pending)
        ]
    );
    let attempts = h.temp.journal()["bitcoin_attempts"].clone();
    assert_eq!(attempts.as_array().unwrap().len(), 2);
    coordinator.reconcile(&context()).await.unwrap();
    assert_eq!(h.calls.load(Ordering::SeqCst), 2);
}
