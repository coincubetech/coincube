use super::*;
use crate::services::{
    claim_observation::{FailureKind, FreshRead, TransactionObservation},
    coincube::{
        network_anchor::{AnchorState, NetworkAnchor, NetworkAnchorStatus},
        network_status::{ForkActivation, NetworkObservation, RdtsFlagday, RdtsStatus},
    },
};
use coincube_core::{
    bip39::Mnemonic,
    claim::BlockRef,
    claim_finalize::finalize_poison_transfer,
    claim_spend::create_poison_self_transfer,
    descriptors::{CoincubeDescriptor, CoincubePolicy},
    miniscript::{
        bitcoin::{
            absolute,
            bip32::{ChildNumber, DerivationPath},
            secp256k1, transaction, Amount, Network, OutPoint, TxIn, TxOut,
        },
        DescriptorPublicKey,
    },
    signer::MasterSigner,
    spend::{CandidateCoin, TxGetter},
};
use httpmock::prelude::*;
use reqwest::header::HeaderMap;
use serde_json::json;
use std::{
    path::PathBuf,
    str::FromStr,
    sync::atomic::{AtomicI64, AtomicUsize},
    time::{SystemTime, UNIX_EPOCH},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "claim-coordinator-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(p)
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
fn artifact(
    chain: ChainId,
    multi: bool,
    change: u32,
) -> (PoisonSelfTransfer, VerifiedPoisonTransfer) {
    let secp = secp256k1::Secp256k1::new();
    let signers: Vec<_> = (40..43)
        .map(|b| {
            MasterSigner::from_mnemonic(Network::Bitcoin, Mnemonic::from_entropy(&[b; 16]).unwrap())
                .unwrap()
        })
        .collect();
    let keys: Vec<_> = signers
        .iter()
        .map(|s| {
            DescriptorPublicKey::from_str(&format!(
                "[{}]{}/<0;1>/*",
                s.fingerprint(&secp),
                s.xpub_at(&DerivationPath::default(), &secp)
            ))
            .unwrap()
        })
        .collect();
    let primary = if multi {
        PathInfo::Multi(1, keys[..2].to_vec())
    } else {
        PathInfo::Single(keys[0].clone())
    };
    let desc = CoincubeDescriptor::new(
        CoincubePolicy::new_legacy(
            primary,
            std::iter::once((46, PathInfo::Single(keys[2].clone()))).collect(),
        )
        .unwrap(),
    );
    let verify = secp256k1::Secp256k1::verification_only();
    let previous = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn::default()],
        output: vec![TxOut {
            value: Amount::from_sat(100000),
            script_pubkey: desc
                .receive_descriptor()
                .derive(0.into(), &verify)
                .script_pubkey(),
        }],
    };
    struct Getter(Transaction);
    impl TxGetter for Getter {
        fn get_tx(&mut self, id: &Txid) -> Option<Transaction> {
            (self.0.compute_txid() == *id).then(|| self.0.clone())
        }
    }
    let coin = CandidateCoin {
        outpoint: OutPoint::new(previous.compute_txid(), 0),
        amount: previous.output[0].value,
        deriv_index: 0.into(),
        is_change: false,
        must_select: true,
        sequence: None,
        ancestor_info: None,
    };
    let built = create_poison_self_transfer(
        chain,
        &desc,
        &verify,
        &mut Getter(previous),
        &[coin],
        ChildNumber::from_normal_idx(change).unwrap(),
        5,
        absolute::LockTime::ZERO,
        hash(42),
    )
    .unwrap();
    let signed = signers[0].sign_psbt(built.psbt().clone(), &secp).unwrap();
    let final_tx = finalize_poison_transfer(&built, &signed, &verify).unwrap();
    (built, final_tx)
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
struct Fixture {
    preflight: PreflightClient,
    stamp: i64,
    clock: Arc<AtomicI64>,
    fault: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    reached: Arc<tokio::sync::Notify>,
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
        let fault = self.fault.load(Ordering::SeqCst);
        Ok(NetworkAnchorStatus {
            network: chain,
            state: AnchorState::Available,
            anchor: Some(NetworkAnchor {
                tip_hash: hash(2),
                tip_height: 100,
                tip_median_time_past: 8000 + i64::from(fault == 2),
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
                            active: fault != 1,
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
        _txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        self.read(chain, TransactionObservation::Absent)
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        _height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        self.read(chain, hash(if chain.is_blake2b() { 2 } else { 1 }))
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
    async fn submit(
        &self,
        tx: Arc<VerifiedPoisonTransfer>,
        _gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        let journal: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.directory.join("intent.json")).unwrap())
                .unwrap();
        assert_eq!(journal["phase"], "BroadcastUncertain");
        assert_eq!(
            journal["signed_txid"],
            tx.transaction().compute_txid().to_string()
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.reached.notify_one();
        if self.fault.load(Ordering::SeqCst) == 5 {
            std::future::pending::<()>().await;
        }
        if self.fault.load(Ordering::SeqCst) == 4 {
            return Err(DaemonError::DaemonStopped);
        }
        Ok(SubmissionOutcome::UpstreamAccepted {
            txid: tx.transaction().compute_txid(),
            wtxid: tx.transaction().compute_wtxid(),
        })
    }
}
struct Harness {
    _server: MockServer,
    temp: Temp,
    sender: watch::Sender<u64>,
    fault: Arc<AtomicUsize>,
    clock: Arc<AtomicI64>,
    calls: Arc<AtomicUsize>,
    reached: Arc<tokio::sync::Notify>,
    coordinator: Coordinator,
}
impl Harness {
    async fn new() -> Self {
        Self::with_acceptance(true).await
    }
    async fn with_acceptance(accepted: bool) -> Self {
        let temp = Temp::new();
        let server = MockServer::start_async().await;
        let (sender, generation) = watch::channel(7);
        let (built, verified) = artifact(ChainId::Bitcoin, false, 12);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let tx = verified.transaction();
        server.mock_async(|when,then| { when.method(POST).path("/api/v1/esplora/bitcoin/mainnet/tx/preflight"); then.status(200).header("cache-control","no-store").json_body(json!({"success":true,"data":{"network":"bitcoin","state":"available","result":{"txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),"tip_hash":hash(1),"observed_at":stamp,"allowed":accepted,"reject_reason":if accepted { serde_json::Value::Null } else { json!("policy-rejected") }}}})); }).await;
        let clock = Arc::new(AtomicI64::new(stamp));
        let fault = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let reached = Arc::new(tokio::sync::Notify::new());
        let services = Fixture {
            preflight: PreflightClient::new(
                &server.base_url(),
                CollectionContext {
                    expected_generation: 7,
                    generation: generation.clone(),
                },
            )
            .unwrap(),
            stamp,
            clock: clock.clone(),
            fault: fault.clone(),
            calls: calls.clone(),
            reached: reached.clone(),
            directory: temp.0.clone(),
        };
        let coordinator = Coordinator::open(
            &temp.0,
            "bitcoin-cube".into(),
            "fork-cube".into(),
            &built,
            verified,
            context(),
            generation,
            Box::new(services),
            policy(),
            false,
        )
        .unwrap();
        Self {
            _server: server,
            temp,
            sender,
            fault,
            clock,
            calls,
            reached,
            coordinator,
        }
    }
}
#[tokio::test]
async fn real_evidence_and_final_witness_join_before_durable_submission() {
    let mut h = Harness::new().await;
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    assert_eq!(
        review.snapshot().fee_sats,
        h.coordinator.verified.fee().to_sat()
    );
    let outcome = h
        .coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    assert!(matches!(outcome, Outcome::UpstreamAccepted { .. }));
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    assert_eq!(h.coordinator.phase(), Phase::BroadcastUncertain);
    assert!(matches!(
        h.coordinator.prepare_review(&context()).await,
        Err(Error::SubmissionAlreadyRecorded)
    ));
    let identity = h.coordinator.controller.identity().clone();
    drop(h.coordinator);
    let reopened = Controller::reopen(&h.temp.0, &identity, context()).unwrap();
    assert_eq!(reopened.phase(), Phase::BroadcastUncertain);
    assert_eq!(reopened.status(), Status::Unchecked);
}
#[tokio::test]
async fn changed_snapshot_and_account_refuse_without_submission() {
    let mut h = Harness::new().await;
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    h.fault.store(2, Ordering::SeqCst);
    assert!(matches!(
        h.coordinator.confirm_and_submit(review, &context()).await,
        Err(Error::ChangedReview)
    ));
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    let mut changed = context();
    changed.account.push('x');
    assert!(matches!(
        h.coordinator.prepare_review(&changed).await,
        Err(Error::Revoked)
    ));
    assert!(matches!(
        h.coordinator.prepare_review(&context()).await,
        Err(Error::Revoked)
    ));
}
#[tokio::test]
async fn stale_inactive_unavailable_and_revoked_never_produce_review() {
    for fault in [1, 3] {
        let mut h = Harness::new().await;
        h.fault.store(fault, Ordering::SeqCst);
        assert!(h.coordinator.prepare_review(&context()).await.is_err());
        assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    }
    let mut h = Harness::new().await;
    h.clock.fetch_add(61, Ordering::SeqCst);
    assert!(h.coordinator.prepare_review(&context()).await.is_err());
    let mut h = Harness::new().await;
    h.sender.send(8).unwrap();
    assert!(matches!(
        h.coordinator.prepare_review(&context()).await,
        Err(Error::Revoked)
    ));
}
#[tokio::test]
async fn failure_to_persist_prevents_transport() {
    let mut h = Harness::new().await;
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    let moved = h.temp.0.with_extension("moved");
    std::fs::rename(&h.temp.0, &moved).unwrap();
    assert!(matches!(
        h.coordinator.confirm_and_submit(review, &context()).await,
        Err(Error::Journal(_))
    ));
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    std::fs::rename(moved, &h.temp.0).unwrap();
}
#[tokio::test]
async fn submission_failure_or_cancellation_remains_uncertain() {
    for fault in [4, 5] {
        let mut h = Harness::new().await;
        let review = h.coordinator.prepare_review(&context()).await.unwrap();
        h.fault.store(fault, Ordering::SeqCst);
        let sender = h.sender.clone();
        let reached = h.reached.clone();
        let cancel = async move {
            if fault == 5 {
                reached.notified().await;
                sender.send(8).unwrap();
            }
        };
        let current = context();
        let (outcome, ()) =
            tokio::join!(h.coordinator.confirm_and_submit(review, &current), cancel);
        assert!(matches!(outcome.unwrap(), Outcome::Uncertain { .. }));
        assert_eq!(h.coordinator.phase(), Phase::BroadcastUncertain);
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn a_review_from_another_intent_is_not_confirmation() {
    let mut first = Harness::new().await;
    let review = first.coordinator.prepare_review(&context()).await.unwrap();
    let mut second = Harness::new().await;
    assert!(matches!(
        second
            .coordinator
            .confirm_and_submit(review, &context())
            .await,
        Err(Error::InvalidReview)
    ));
    assert_eq!(second.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn unsupported_multisig_and_testnet_refuse_before_journal_creation() {
    for (chain, multi) in [(ChainId::Testnet4, false), (ChainId::Bitcoin, true)] {
        let h = Harness::new().await;
        let (built, verified) = artifact(chain, multi, 12);
        let temp = Temp::new();
        let result = Coordinator::open(
            &temp.0,
            "btc".into(),
            "fork".into(),
            &built,
            verified,
            context(),
            h.sender.subscribe(),
            h.coordinator.services,
            policy(),
            false,
        );
        assert!(matches!(result, Err(Error::Unsupported)));
        assert!(!temp.0.join("intent.json").exists());
    }
}

#[test]
fn synchronous_revocation_covers_both_gate_registration_orderings() {
    use coincubed::poison_broadcast::SubmissionState;
    for revoke_first in [false, true] {
        let (_, verified) = artifact(ChainId::Bitcoin, false, 12);
        let (gate, gate_revoker) =
            SubmissionGate::new(&verified, Instant::now() + Duration::from_secs(30));
        let revoker = Revoker::new();
        if revoke_first {
            revoker.revoke();
            assert!(revoker.register(gate_revoker).is_err());
        } else {
            revoker.register(gate_revoker).unwrap();
            revoker.revoke();
        }
        assert!(revoker.is_revoked());
        assert_eq!(gate.state(), SubmissionState::Revoked);
    }
}
#[tokio::test]
async fn synchronous_revoker_invalidates_a_prepared_review_without_watch_polling() {
    let mut h = Harness::new().await;
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    h.coordinator.revoker().revoke();
    assert!(matches!(
        h.coordinator.confirm_and_submit(review, &context()).await,
        Err(Error::Revoked)
    ));
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn concurrent_gate_registration_cannot_escape_revocation() {
    use coincubed::poison_broadcast::SubmissionState;
    let (_, verified) = artifact(ChainId::Bitcoin, false, 12);
    for _ in 0..16 {
        let (gate, gate_revoker) =
            SubmissionGate::new(&verified, Instant::now() + Duration::from_secs(30));
        let revoker = Revoker::new();
        let registering = revoker.clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let other = barrier.clone();
        let thread = std::thread::spawn(move || {
            other.wait();
            let _ = registering.register(gate_revoker);
        });
        barrier.wait();
        revoker.revoke();
        thread.join().unwrap();
        assert_eq!(gate.state(), SubmissionState::Revoked);
    }
}

#[tokio::test]
async fn rejected_node_policy_never_becomes_review_or_submission() {
    let mut h = Harness::with_acceptance(false).await;
    assert!(matches!(
        h.coordinator.prepare_review(&context()).await,
        Err(Error::PolicyRejected(NodePolicy::Rejected { .. }))
    ));
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    assert_eq!(h.coordinator.phase(), Phase::Intent);
}
#[tokio::test]
async fn changed_finalized_construction_refuses_before_journal() {
    let h = Harness::new().await;
    let (built, _) = artifact(ChainId::Bitcoin, false, 12);
    let (_, different) = artifact(ChainId::Bitcoin, false, 13);
    let temp = Temp::new();
    let result = Coordinator::open(
        &temp.0,
        "btc".into(),
        "fork".into(),
        &built,
        different,
        context(),
        h.sender.subscribe(),
        h.coordinator.services,
        policy(),
        false,
    );
    assert!(matches!(result, Err(Error::InvalidBinding)));
    assert!(!temp.0.join("intent.json").exists());
}

#[tokio::test]
async fn dispatch_deadline_uses_oldest_evidence_and_never_extends_for_skew() {
    let mut h = Harness::new().await;
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    let mut bundle = review.snapshot.observations;
    let now = bundle.bitcoin.observed_at;
    let origin = Instant::now();
    let mut p = policy();
    p.collection_budget = Duration::from_secs(30);
    bundle.fork.observed_at = now - 58;
    assert_eq!(
        evidence_deadline(p, bundle, now, now, origin).unwrap(),
        origin + Duration::from_secs(1)
    );
    bundle.fork.observed_at = now - 59;
    assert!(matches!(
        evidence_deadline(p, bundle, now, now, origin),
        Err(Error::ExpiredEvidence)
    ));
    bundle.fork.observed_at = now;
    p.preflight.max_age_seconds = 10;
    assert_eq!(
        evidence_deadline(p, bundle, now + 2, now, origin).unwrap(),
        origin + Duration::from_secs(9)
    );
    assert!(matches!(
        evidence_deadline(p, bundle, now + 3, now, origin),
        Err(Error::ExpiredEvidence)
    ));
    bundle.deployment.observed_at = now + 1;
    assert!(matches!(
        evidence_deadline(p, bundle, now, now, origin),
        Err(Error::ExpiredEvidence)
    ));
    bundle.deployment.observed_at = now;
    assert!(matches!(
        evidence_deadline(p, bundle, now - 9, now, origin),
        Err(Error::ExpiredEvidence)
    ));
    assert!(matches!(
        evidence_deadline(p, bundle, now, -1, origin),
        Err(Error::ExpiredEvidence)
    ));
}
