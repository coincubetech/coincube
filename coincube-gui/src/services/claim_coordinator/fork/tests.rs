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
    let signers: Vec<_> = (40..44)
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
        PathInfo::Multi(2, keys[..3].to_vec())
    } else {
        PathInfo::Single(keys[0].clone())
    };
    let desc = CoincubeDescriptor::new(
        CoincubePolicy::new_legacy(
            primary,
            std::iter::once((
                46,
                PathInfo::Single(keys[if multi { 3 } else { 2 }].clone()),
            ))
            .collect(),
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
    let signed = if multi {
        signers[1].sign_psbt(signed, &secp).unwrap()
    } else {
        signed
    };
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
    source_txid: Txid,
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
                            active: !matches!(fault, 1 | 14),
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
        let fault = self.fault.load(Ordering::SeqCst);
        if chain.is_blake2b() && txid != self.source_txid && matches!(fault, 11..=14) {
            return self.read(
                chain,
                if fault == 11 {
                    TransactionObservation::Unconfirmed { txid }
                } else {
                    TransactionObservation::Confirmed {
                        txid,
                        block: BlockRef {
                            height: 100,
                            hash: hash(2),
                        },
                    }
                },
            );
        }
        self.read(
            chain,
            if chain == ChainId::Bitcoin && self.fault.load(Ordering::SeqCst) != 10 {
                TransactionObservation::Confirmed {
                    txid,
                    block: BlockRef {
                        height: if matches!(self.fault.load(Ordering::SeqCst), 6 | 13 | 14) {
                            101
                        } else {
                            100
                        },
                        hash: hash(1),
                    },
                }
            } else {
                TransactionObservation::Absent
            },
        )
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
impl ForkServices for Fixture {
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
            .observe(ChainId::BitcoinBlake2b, tx, tip, policy)
            .await
    }
    async fn submit(
        &self,
        tx: Arc<VerifiedClaimForkSweep>,
        _gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        let journal: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.directory.join("intent.json")).unwrap())
                .unwrap();
        assert_eq!(
            journal["fork_submission"]["txid"],
            tx.transaction().compute_txid().to_string()
        );
        assert_eq!(
            journal["fork_submission"]["wtxid"],
            tx.transaction().compute_wtxid().to_string()
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
fn sweep(source: &PoisonSelfTransfer) -> (ClaimForkSweep, VerifiedClaimForkSweep) {
    struct Getter(Transaction);
    impl TxGetter for Getter {
        fn get_tx(&mut self, id: &Txid) -> Option<Transaction> {
            (self.0.compute_txid() == *id).then(|| self.0.clone())
        }
    }
    let input = &source.psbt().inputs[0];
    let secp = secp256k1::Secp256k1::new();
    let built = coincube_core::claim_spend::create_claim_fork_sweep(
        source,
        ChainId::BitcoinBlake2b,
        &secp256k1::Secp256k1::verification_only(),
        &mut Getter(input.non_witness_utxo.clone().unwrap()),
        &[CandidateCoin {
            outpoint: source.psbt().unsigned_tx.input[0].previous_output,
            amount: input.witness_utxo.as_ref().unwrap().value,
            deriv_index: 0.into(),
            is_change: false,
            must_select: true,
            sequence: None,
            ancestor_info: None,
        }],
        20.into(),
        3,
        absolute::LockTime::ZERO,
    )
    .unwrap();
    let signed = [40, 41]
        .iter()
        .copied()
        .fold(built.psbt().clone(), |psbt, b| {
            MasterSigner::from_mnemonic(Network::Bitcoin, Mnemonic::from_entropy(&[b; 16]).unwrap())
                .unwrap()
                .sign_psbt(psbt, &secp)
                .unwrap()
        });
    let verified = coincube_core::claim_finalize::finalize_claim_fork_sweep(
        &built,
        &coincube_core::psbt_unified::UnifiedPsbt::from_psbt(signed).unwrap(),
        &secp,
    )
    .unwrap();
    (built, verified)
}
struct Harness {
    preflight_mock_id: usize,
    coordinator: Coordinator,
    _server: MockServer,
    _temp: Temp,
    sender: watch::Sender<u64>,
    fault: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    reached: Arc<tokio::sync::Notify>,
}
impl Harness {
    async fn new(accepted: bool) -> Self {
        let temp = Temp::new();
        let server = MockServer::start_async().await;
        let (sender, generation) = watch::channel(7);
        let (source, bitcoin) = artifact(ChainId::Bitcoin, true, 12);
        let (construction, verified) = sweep(&source);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let tx = verified.transaction();
        let preflight_mock_id = server.mock_async(|when,then| { when.method(POST).path("/api/v1/esplora/bitcoin-blake2b/mainnet/tx/preflight"); then.status(200).header("cache-control","no-store").json_body(json!({"success":true,"data":{"network":"bitcoin-blake2b","state":"available","result":{"txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),"tip_hash":hash(2),"observed_at":stamp,"allowed":accepted,"reject_reason":if accepted { serde_json::Value::Null } else { json!("policy-rejected") }}}})); }).await.id;
        let fault = Arc::new(AtomicUsize::new(10));
        let calls = Arc::new(AtomicUsize::new(0));
        let reached = Arc::new(tokio::sync::Notify::new());
        let fixture = Fixture {
            source_txid: source.psbt().unsigned_tx.compute_txid(),
            preflight: PreflightClient::new(
                &server.base_url(),
                CollectionContext {
                    expected_generation: 7,
                    generation: generation.clone(),
                },
            )
            .unwrap(),
            stamp,
            clock: Arc::new(AtomicI64::new(stamp)),
            fault: fault.clone(),
            calls: calls.clone(),
            reached: reached.clone(),
            directory: temp.0.clone(),
        };
        let mut controller = Controller::create(
            &temp.0,
            "bitcoin-cube".into(),
            "fork-cube".into(),
            &source,
            context(),
        )
        .unwrap();
        let ticket = controller.begin_check(&context()).unwrap();
        let collected = claim_observation::collect(
            &fixture,
            &controller.plan(),
            policy().observations,
            policy().collection_budget,
            CollectionContext {
                expected_generation: 7,
                generation: generation.clone(),
            },
        )
        .await
        .unwrap();
        controller
            .apply_observation(
                ticket,
                &context(),
                Ok(collected),
                policy().observations,
                stamp,
            )
            .unwrap();
        controller
            .record_broadcast_intent(
                &context(),
                bitcoin.transaction(),
                policy().observations,
                stamp,
            )
            .unwrap();
        drop(controller);
        fault.store(0, Ordering::SeqCst);
        let coordinator = Coordinator::open(
            &temp.0,
            "bitcoin-cube".into(),
            "fork-cube".into(),
            &source,
            construction,
            verified,
            context(),
            generation,
            Box::new(fixture),
            policy(),
        )
        .unwrap();
        Self {
            preflight_mock_id,
            coordinator,
            _server: server,
            _temp: temp,
            sender,
            fault,
            calls,
            reached,
        }
    }
}
#[tokio::test]
async fn fork_confirmation_records_exact_intent_before_single_submission() {
    let mut h = Harness::new(true).await;
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    let evidence = h.coordinator.split_evidence(&review, &context()).unwrap();
    let unsigned = coincube_core::psbt_unified::UnifiedPsbt::from_psbt(
        h.coordinator.construction.psbt().clone(),
    )
    .unwrap();
    assert!(evidence.matches(&unsigned));
    assert!(!evidence.consume_for_signing(&unsigned)); // review evidence is display-only

    assert!(matches!(
        h.coordinator
            .confirm_and_submit(review, &context())
            .await
            .unwrap(),
        Outcome::UpstreamAccepted { .. }
    ));
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        h.coordinator.prepare_review(&context()).await,
        Err(Error::SubmissionAlreadyRecorded)
    ));
    assert!(matches!(
        h.coordinator.recorded_outcome(),
        Some(Outcome::Uncertain { .. })
    ));
}
#[tokio::test]
async fn fork_confirm_rechecks_depth_deployment_provider_and_generation() {
    for fault in [1, 2, 3, 6] {
        let mut h = Harness::new(true).await;
        let review = h.coordinator.prepare_review(&context()).await.unwrap();
        h.fault.store(fault, Ordering::SeqCst);
        assert!(h
            .coordinator
            .confirm_and_submit(review, &context())
            .await
            .is_err());
        assert_eq!(h.calls.load(Ordering::SeqCst), 0);
        assert!(h.coordinator.recorded_outcome().is_none());
    }
    let mut h = Harness::new(true).await;
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    h.sender.send(8).unwrap();
    assert!(matches!(
        h.coordinator.confirm_and_submit(review, &context()).await,
        Err(Error::Revoked)
    ));
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn fork_rejected_policy_and_lost_response_cannot_be_success_or_retry() {
    let mut h = Harness::new(false).await;
    assert!(matches!(
        h.coordinator.prepare_review(&context()).await,
        Err(Error::PolicyRejected(_))
    ));
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    let mut h = Harness::new(true).await;
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    h.fault.store(4, Ordering::SeqCst);
    assert!(matches!(
        h.coordinator
            .confirm_and_submit(review, &context())
            .await
            .unwrap(),
        Outcome::Uncertain { .. }
    ));
    assert!(matches!(
        h.coordinator.prepare_review(&context()).await,
        Err(Error::SubmissionAlreadyRecorded)
    ));
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn fork_cancellation_after_intent_keeps_uncertain_and_cannot_retry() {
    let mut h = Harness::new(true).await;
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    h.fault.store(5, Ordering::SeqCst);
    let reached = h.reached.clone();
    let sender = h.sender.clone();
    let cancel = async move {
        reached.notified().await;
        sender.send(8).unwrap();
    };
    let context = context();
    let (result, ()) = tokio::join!(h.coordinator.confirm_and_submit(review, &context), cancel);
    assert!(matches!(result.unwrap(), Outcome::Uncertain { .. }));
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        h.coordinator.recorded_outcome(),
        Some(Outcome::Uncertain { .. })
    ));
}
#[tokio::test]
async fn fork_journal_failure_foreign_review_and_sync_revocation_prevent_send() {
    let mut h = Harness::new(true).await;
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    let moved = h._temp.0.with_extension("moved");
    std::fs::rename(&h._temp.0, &moved).unwrap();
    let result = h.coordinator.confirm_and_submit(review, &context()).await;
    std::fs::rename(&moved, &h._temp.0).unwrap();
    assert!(result.is_err());
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    assert!(h.coordinator.prepare_review(&context()).await.is_err());
    // A failed durable write poisons that journal handle. A different test
    // claim supplies the foreign review; never make the damaged handle usable.
    let mut h = Harness::new(true).await;
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    let mut other = Harness::new(true).await;
    assert!(matches!(
        other
            .coordinator
            .confirm_and_submit(review, &context())
            .await,
        Err(Error::InvalidReview)
    ));
    assert_eq!(other.calls.load(Ordering::SeqCst), 0);
    let review = other.coordinator.prepare_review(&context()).await.unwrap();
    other.coordinator.revoker().revoke();
    assert!(matches!(
        other
            .coordinator
            .confirm_and_submit(review, &context())
            .await,
        Err(Error::Revoked)
    ));
    assert_eq!(other.calls.load(Ordering::SeqCst), 0);
}

struct PreparingHarness {
    preflight_mock_id: usize,
    preparation: Preparation,
    _server: MockServer,
    _temp: Temp,
    sender: watch::Sender<u64>,
    fault: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
}
impl PreparingHarness {
    fn psbt(&self) -> coincube_core::psbt_unified::UnifiedPsbt {
        coincube_core::psbt_unified::UnifiedPsbt::from_psbt(
            self.preparation.construction.psbt().clone(),
        )
        .unwrap()
    }
    async fn new() -> Self {
        let Harness {
            preflight_mock_id,
            coordinator,
            _server,
            _temp,
            sender,
            fault,
            calls,
            reached,
        } = Harness::new(true).await;
        let stamp = coordinator.services.source().now();
        drop(coordinator); // release the actual exclusive journal lock
        let (source, _) = artifact(ChainId::Bitcoin, true, 12);
        let (construction, _) = sweep(&source);
        let fixture = Fixture {
            source_txid: source.psbt().unsigned_tx.compute_txid(),
            preflight: PreflightClient::new(
                &_server.base_url(),
                CollectionContext {
                    expected_generation: 7,
                    generation: sender.subscribe(),
                },
            )
            .unwrap(),
            stamp,
            clock: Arc::new(AtomicI64::new(stamp)),
            fault: fault.clone(),
            calls: calls.clone(),
            reached,
            directory: _temp.0.clone(),
        };
        let preparation = Preparation::open(
            &_temp.0,
            "bitcoin-cube".into(),
            "fork-cube".into(),
            &source,
            construction,
            context(),
            sender.subscribe(),
            Box::new(fixture),
            policy(),
        )
        .unwrap();
        Self {
            preflight_mock_id,
            preparation,
            _server,
            _temp,
            sender,
            fault,
            calls,
        }
    }
}
fn sign_sweep(
    psbt: coincube_core::miniscript::bitcoin::psbt::Psbt,
    signers: usize,
) -> coincube_core::psbt_unified::UnifiedPsbt {
    let secp = secp256k1::Secp256k1::new();
    let signed = [40, 41]
        .iter()
        .copied()
        .take(signers)
        .fold(psbt, |psbt, b| {
            MasterSigner::from_mnemonic(Network::Bitcoin, Mnemonic::from_entropy(&[b; 16]).unwrap())
                .unwrap()
                .sign_psbt(psbt, &secp)
                .unwrap()
        });
    coincube_core::psbt_unified::UnifiedPsbt::from_psbt(signed).unwrap()
}
#[tokio::test]
async fn signing_requires_fresh_depth_then_transfers_lock_into_verified_submission() {
    let mut h = PreparingHarness::new().await;
    h.fault.store(6, Ordering::SeqCst);
    assert!(h.preparation.check_signing(&context()).await.is_err());
    assert!(h.preparation.controller.recorded_fork_sweep().is_none());
    h.fault.store(0, Ordering::SeqCst);
    let check = h.preparation.check_signing(&context()).await.unwrap();
    let psbt = h
        .preparation
        .signing_psbt(check, &h.psbt(), &context())
        .unwrap();
    assert_eq!(
        h.preparation.controller.recorded_fork_sweep(),
        Some(&psbt.unsigned_tx)
    );
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    let mut coordinator = h
        .preparation
        .finish(&sign_sweep(psbt, 2), &context())
        .unwrap();
    // The same controller owns the lock continuously through finalization.
    assert!(Controller::reopen(&h._temp.0, coordinator.controller.identity(), context()).is_err());
    let review = coordinator.prepare_review(&context()).await.unwrap();
    assert!(matches!(
        coordinator
            .confirm_and_submit(review, &context())
            .await
            .unwrap(),
        Outcome::UpstreamAccepted { .. }
    ));
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn signing_checks_are_bound_one_use_expiring_and_revocable() {
    let mut first = PreparingHarness::new().await;
    let mut second = PreparingHarness::new().await;
    let check = first.preparation.check_signing(&context()).await.unwrap();
    assert!(matches!(
        second
            .preparation
            .signing_psbt(check, &second.psbt(), &context()),
        Err(Error::InvalidReview)
    ));
    let old = first.preparation.check_signing(&context()).await.unwrap();
    let mut newest = first.preparation.check_signing(&context()).await.unwrap();
    assert!(matches!(
        first
            .preparation
            .signing_psbt(old, &first.psbt(), &context()),
        Err(Error::InvalidReview)
    ));
    newest.not_after = Instant::now();
    assert!(matches!(
        first
            .preparation
            .signing_psbt(newest, &first.psbt(), &context()),
        Err(Error::ExpiredEvidence)
    ));
    let check = first.preparation.check_signing(&context()).await.unwrap();
    first.preparation.revoker().revoke();
    assert!(matches!(
        first
            .preparation
            .signing_psbt(check, &first.psbt(), &context()),
        Err(Error::Revoked)
    ));
    let check = second.preparation.check_signing(&context()).await.unwrap();
    second.sender.send(8).unwrap();
    assert!(matches!(
        second
            .preparation
            .signing_psbt(check, &second.psbt(), &context()),
        Err(Error::Revoked)
    ));
    assert_eq!(first.calls.load(Ordering::SeqCst), 0);
    assert_eq!(second.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn signing_journal_failure_and_incomplete_signatures_never_advance() {
    let mut h = PreparingHarness::new().await;
    let moved = h._temp.0.with_extension("moved");
    std::fs::rename(&h._temp.0, &moved).unwrap();
    let checked = h.preparation.check_signing(&context()).await;
    std::fs::rename(&moved, &h._temp.0).unwrap();
    assert!(checked.is_err());
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
    let mut h = PreparingHarness::new().await;
    let check = h.preparation.check_signing(&context()).await.unwrap();
    let psbt = h
        .preparation
        .signing_psbt(check, &h.psbt(), &context())
        .unwrap();
    assert!(matches!(
        h.preparation.finish(&sign_sweep(psbt, 1), &context()),
        Err(Error::InvalidBinding)
    ));
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn signing_preserves_partial_signatures_and_refuses_replaced_metadata() {
    let mut h = PreparingHarness::new().await;
    let partial = sign_sweep(h.psbt().psbt().clone(), 1);
    let check = h.preparation.check_signing(&context()).await.unwrap();
    let continued = h
        .preparation
        .signing_psbt(check, &partial, &context())
        .unwrap();
    assert_eq!(&continued, partial.psbt());
    assert_eq!(continued.inputs[0].partial_sigs.len(), 1);
    for field in 0..3 {
        let mut altered = partial.psbt().clone();
        match field {
            0 => altered.unsigned_tx.output[0].value = Amount::from_sat(1),
            1 => altered.inputs[0].bip32_derivation.clear(),
            _ => altered.inputs[0].witness_utxo.as_mut().unwrap().value = Amount::from_sat(1),
        }
        let altered = coincube_core::psbt_unified::UnifiedPsbt::from_psbt(altered).unwrap();
        let check = h.preparation.check_signing(&context()).await.unwrap();
        assert!(matches!(
            h.preparation.signing_psbt(check, &altered, &context()),
            Err(Error::InvalidBinding)
        ));
    }
    let check = h.preparation.check_signing(&context()).await.unwrap();
    let continued = h
        .preparation
        .signing_psbt(check, &partial, &context())
        .unwrap();
    assert!(h
        .preparation
        .finish(&sign_sweep(continued, 2), &context())
        .is_ok());
    assert_eq!(h.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn split_proof_never_bypasses_finalization_or_owned_construction() {
    use crate::app::state::vault::replay::{replay_status, ReplayStatus};
    let mut h = PreparingHarness::new().await;
    let unsigned = h.psbt();
    let partial = sign_sweep(unsigned.psbt().clone(), 1);
    let signed = sign_sweep(unsigned.psbt().clone(), 2);
    let check = h.preparation.check_signing(&context()).await.unwrap();
    let dispatch = h
        .preparation
        .signing_dispatch(check, &unsigned, &context())
        .unwrap();
    let secp = secp256k1::Secp256k1::verification_only();
    for incomplete in [unsigned.psbt(), partial.psbt()] {
        assert!(matches!(
            replay_status(incomplete, &secp, Some(&dispatch.split)),
            ReplayStatus::Unknown(_)
        ));
    }
    assert_eq!(
        replay_status(signed.psbt(), &secp, Some(&dispatch.split)),
        ReplayStatus::Split
    );
    let mut tampered = signed.psbt().clone();
    tampered.unsigned_tx.output[0].value = Amount::from_sat(1);
    assert!(matches!(
        replay_status(&tampered, &secp, Some(&dispatch.split)),
        ReplayStatus::Unknown(_)
    ));
    let mut changed_origin = signed.psbt().clone();
    changed_origin.inputs[0].bip32_derivation.clear();
    assert!(matches!(
        replay_status(&changed_origin, &secp, Some(&dispatch.split)),
        ReplayStatus::Unknown(_)
    ));
    // Proof is tied to its journal owner, not merely a still-open generation channel.
    drop(h.preparation);
    assert!(!dispatch.split.is_live());
    assert!(matches!(
        replay_status(signed.psbt(), &secp, Some(&dispatch.split)),
        ReplayStatus::Unknown(_)
    ));
}
#[tokio::test]
async fn cached_split_review_expires_without_psbt_change_or_acknowledgement_escape() {
    use crate::app::state::vault::replay::{ReplayReview, ReplayStatus};
    let mut h = PreparingHarness::new().await;
    let signed = sign_sweep(h.psbt().psbt().clone(), 2);
    let check = h.preparation.check_signing(&context()).await.unwrap();
    let dispatch = h
        .preparation
        .signing_dispatch(check, &signed, &context())
        .unwrap();
    let secp = secp256k1::Secp256k1::verification_only();
    let mut review = ReplayReview::with_split(signed.psbt(), &secp, dispatch.split.clone());
    assert_eq!(review.status(), ReplayStatus::Split);
    assert!(review.broadcast_ready(&[]));
    tokio::time::sleep_until(
        tokio::time::Instant::from_std(dispatch.split.not_after) + Duration::from_millis(1),
    )
    .await;
    assert!(matches!(review.status(), ReplayStatus::Unknown(_)));
    review.set_acknowledged(true);
    assert!(!review.broadcast_ready(&[]));
    assert!(!review.refreshed(signed.psbt(), &secp).broadcast_ready(&[]));
}
#[tokio::test]
async fn psbt_state_and_pill_drop_split_readiness_on_synchronous_revocation() {
    use crate::app::{
        cache::Cache,
        state::vault::{
            psbt::PsbtState,
            replay::{pill_copy, ReplayStatus},
        },
        wallet::Wallet,
    };
    use crate::daemon::model::SpendTx;
    let mut h = PreparingHarness::new().await;
    let signed = sign_sweep(h.psbt().psbt().clone(), 2);
    let check = h.preparation.check_signing(&context()).await.unwrap();
    let dispatch = h
        .preparation
        .signing_dispatch(check, &signed, &context())
        .unwrap();
    let descriptor = h.preparation.construction.descriptor().clone();
    let wallet = Arc::new(Wallet::new(descriptor.clone()).with_chain(ChainId::BitcoinBlake2b));
    let tx = SpendTx::new(
        None,
        signed.psbt().clone(),
        Vec::new(),
        &descriptor,
        &secp256k1::Secp256k1::new(),
        Network::Bitcoin,
    );
    let mut state = PsbtState::new(wallet, tx, false);
    assert!(state.require_claim_signing_checks());
    assert!(!state.consume_claim_signing_permit());
    assert!(state.set_claim_split_evidence(dispatch.split.clone()));
    assert!(state.consume_claim_signing_permit());
    assert!(!state.consume_claim_signing_permit());
    assert!(state.set_claim_split_evidence(dispatch.split));
    assert!(!state.consume_claim_signing_permit()); // reattaching/cloning cannot renew it
    let check = h.preparation.check_signing(&context()).await.unwrap();
    let fresh = h
        .preparation
        .signing_dispatch(check, &signed, &context())
        .unwrap();
    assert!(state.set_claim_split_evidence(fresh.split));
    let cache = Cache::default();
    assert!(!state.broadcast_ready(&cache)); // only the Claim coordinator submits
    assert_eq!(
        state.replay_presentation(&cache).unwrap().review.status(),
        ReplayStatus::Split
    );
    h.preparation.revoker().revoke();
    assert!(!state.consume_claim_signing_permit());
    assert!(!state.broadcast_ready(&cache));
    let pill = state.replay_presentation(&cache).unwrap();
    assert!(matches!(pill.review.status(), ReplayStatus::Unknown(_)));
    assert_ne!(
        pill_copy(&pill.review.status(), &pill.entangled).0,
        "Split — cannot replay"
    );
}

#[tokio::test]
async fn completion_requires_current_fork_inclusion_and_bitcoin_depth_and_revokes_old_checks() {
    let mut h = Harness::new(true).await;
    assert!(h.coordinator.check_completion(&context()).await.is_err());
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    h.coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    for fault in [0, 11, 13] {
        h.fault.store(fault, Ordering::SeqCst);
        assert!(h
            .coordinator
            .check_completion(&context())
            .await
            .unwrap()
            .is_none());
    }
    h.fault.store(12, Ordering::SeqCst);
    let evidence = h
        .coordinator
        .check_completion(&context())
        .await
        .unwrap()
        .unwrap();
    assert!(evidence.is_live());
    assert_eq!(
        evidence.block(),
        BlockRef {
            height: 100,
            hash: hash(2)
        }
    );
    assert_eq!(evidence.wallet().bitcoin_cube, "bitcoin-cube");
    assert_eq!(evidence.wallet().fork_cube, "fork-cube");
    assert_eq!(
        evidence.txid(),
        h.coordinator.verified.transaction().compute_txid()
    );
    h.fault.store(13, Ordering::SeqCst);
    assert!(h
        .coordinator
        .check_completion(&context())
        .await
        .unwrap()
        .is_none());
    assert!(!evidence.is_live());
    h.fault.store(12, Ordering::SeqCst);
    let evidence = h
        .coordinator
        .check_completion(&context())
        .await
        .unwrap()
        .unwrap();
    h.coordinator.revoker().revoke();
    assert!(!evidence.is_live());
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn confirmed_completion_persists_both_cubes_and_refuses_changed_identity_without_writes() {
    use crate::app::settings::{update_settings_file, CubeSettings, Settings, VaultIdentity};
    let mut h = Harness::new(true).await;
    let root = crate::dir::CoincubeDirectory::new(h._temp.0.join("paired-settings"));
    let identity = VaultIdentity::generate(h.coordinator.construction.descriptor());
    for (chain, id) in [
        (ChainId::Bitcoin, "bitcoin-cube"),
        (ChainId::BitcoinBlake2b, "fork-cube"),
    ] {
        let cube =
            CubeSettings::new_with_raw_id(id.into(), id.into(), chain).with_vault(identity.clone());
        update_settings_file(&root.network_directory(chain), |mut settings| {
            settings.cubes.push(cube);
            Some(settings)
        })
        .await
        .unwrap();
    }
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    h.coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    h.fault.store(12, Ordering::SeqCst);
    let proof = h
        .coordinator
        .check_completion(&context())
        .await
        .unwrap()
        .unwrap();
    proof.persist(&root).await.unwrap();
    proof.persist(&root).await.unwrap(); // same confirmed record is retry-safe
    for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
        let settings = Settings::from_file(&root.network_directory(chain)).unwrap();
        assert_eq!(settings.cubes[0].split_completed_at_height, Some(100));
    }
    update_settings_file(&root.network_directory(ChainId::Bitcoin), |mut settings| {
        settings.cubes[0].vault_fingerprint = Some("changed".into());
        Some(settings)
    })
    .await
    .unwrap();
    let files: Vec<_> = [ChainId::Bitcoin, ChainId::BitcoinBlake2b]
        .iter()
        .copied()
        .map(|chain| root.network_directory(chain).path().join("settings.json"))
        .collect();
    let before: Vec<_> = files
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    assert!(proof.persist(&root).await.is_err());
    for (path, bytes) in files.iter().zip(&before) {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
    h.coordinator.revoker().revoke();
    assert!(proof
        .persist(&root)
        .await
        .unwrap_err()
        .to_string()
        .contains("expired or changed"));
    for (path, bytes) in files.iter().zip(&before) {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn fallible_settings_updates_leave_both_chain_files_unchanged_on_refusal() {
    use crate::app::settings::{
        update_settings_file, update_settings_file_checked, CubeSettings, SettingsError,
    };
    let temp = Temp::new();
    let root = crate::dir::CoincubeDirectory::new(temp.0.clone());
    for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
        let directory = root.network_directory(chain);
        update_settings_file(&directory, |mut settings| {
            settings
                .cubes
                .push(CubeSettings::new("unchanged".into(), chain));
            Some(settings)
        })
        .await
        .unwrap();
        let path = directory.path().join("settings.json");
        let before = std::fs::read(&path).unwrap();
        assert!(update_settings_file_checked(&directory, |mut settings| {
            settings.cubes.clear();
            Err(SettingsError::Unexpected("synthetic refusal".into()))
        })
        .await
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn partial_completion_write_is_reported_and_recoverable_with_fresh_evidence() {
    use crate::app::settings::{update_settings_file, CubeSettings, Settings, VaultIdentity};
    let mut h = Harness::new(true).await;
    let root = crate::dir::CoincubeDirectory::new(h._temp.0.join("partial-settings"));
    let identity = VaultIdentity::generate(h.coordinator.construction.descriptor());
    for (chain, id) in [
        (ChainId::Bitcoin, "bitcoin-cube"),
        (ChainId::BitcoinBlake2b, "fork-cube"),
    ] {
        let cube =
            CubeSettings::new_with_raw_id(id.into(), id.into(), chain).with_vault(identity.clone());
        update_settings_file(&root.network_directory(chain), |mut settings| {
            settings.cubes.push(cube);
            Some(settings)
        })
        .await
        .unwrap();
    }
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    h.coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    h.fault.store(12, Ordering::SeqCst);
    let proof = h
        .coordinator
        .check_completion(&context())
        .await
        .unwrap()
        .unwrap();
    let bitcoin_dir = root.network_directory(ChainId::Bitcoin);
    assert!(proof
        .persist_with_hook(&root, async {
            update_settings_file(&bitcoin_dir, |mut settings| {
                settings.cubes[0].vault_fingerprint = Some("concurrently-replaced".into());
                Some(settings)
            })
            .await
            .unwrap();
        })
        .await
        .is_err());
    assert_eq!(
        Settings::from_file(&bitcoin_dir).unwrap().cubes[0].split_completed_at_height,
        None
    );
    assert_eq!(
        Settings::from_file(&root.network_directory(ChainId::BitcoinBlake2b))
            .unwrap()
            .cubes[0]
            .split_completed_at_height,
        Some(100)
    );
    update_settings_file(&bitcoin_dir, |mut settings| {
        settings.cubes[0].vault_fingerprint = identity.fingerprint;
        Some(settings)
    })
    .await
    .unwrap();
    let fresh = h
        .coordinator
        .check_completion(&context())
        .await
        .unwrap()
        .unwrap();
    assert!(!proof.is_live());
    fresh.persist(&root).await.unwrap();
    assert_eq!(
        Settings::from_file(&bitcoin_dir).unwrap().cubes[0].split_completed_at_height,
        Some(100)
    );
    assert_eq!(h.calls.load(Ordering::SeqCst), 1);
}

async fn completed_pair() -> (Harness, crate::dir::CoincubeDirectory) {
    use crate::app::settings::{update_settings_file, CubeSettings, VaultIdentity};
    let mut h = Harness::new(true).await;
    let root = crate::dir::CoincubeDirectory::new(h._temp.0.join("reorg-settings"));
    let identity = VaultIdentity::generate(h.coordinator.construction.descriptor());
    for (chain, id) in [
        (ChainId::Bitcoin, "bitcoin-cube"),
        (ChainId::BitcoinBlake2b, "fork-cube"),
    ] {
        let cube =
            CubeSettings::new_with_raw_id(id.into(), id.into(), chain).with_vault(identity.clone());
        update_settings_file(&root.network_directory(chain), |mut settings| {
            settings.cubes.push(cube);
            Some(settings)
        })
        .await
        .unwrap();
    }
    let review = h.coordinator.prepare_review(&context()).await.unwrap();
    h.coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    h.fault.store(12, Ordering::SeqCst);
    h.coordinator
        .check_completion(&context())
        .await
        .unwrap()
        .unwrap()
        .persist(&root)
        .await
        .unwrap();
    (h, root)
}

#[tokio::test]
async fn fresh_confirmation_loss_clears_both_markers_but_transport_failure_does_not() {
    use crate::app::settings::Settings;
    for lost in [0, 13, 14] {
        let (mut h, root) = completed_pair().await;
        h.fault.store(3, Ordering::SeqCst);
        assert!(h
            .coordinator
            .reconcile_completion(&context(), &root)
            .await
            .is_err());
        for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
            let cube = Settings::from_file(&root.network_directory(chain))
                .unwrap()
                .cubes
                .remove(0);
            assert_eq!(cube.split_completed_at_height, Some(100));
            assert_eq!(
                cube.split_completion_txid,
                Some(h.coordinator.verified.transaction().compute_txid())
            );
        }
        h.fault.store(lost, Ordering::SeqCst);
        h.coordinator
            .reconcile_completion(&context(), &root)
            .await
            .unwrap();
        for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
            let cube = Settings::from_file(&root.network_directory(chain))
                .unwrap()
                .cubes
                .remove(0);
            assert_eq!(cube.split_completed_at_height, None);
            assert_eq!(cube.split_completion_txid, None);
        }
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn reorg_reconciliation_never_clears_another_sweeps_marker() {
    use crate::app::settings::{update_settings_file, Settings};
    let (mut h, root) = completed_pair().await;
    let other = Txid::from_byte_array([99; 32]);
    for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
        update_settings_file(&root.network_directory(chain), |mut settings| {
            settings.cubes[0].split_completed_at_height = Some(101);
            settings.cubes[0].split_completion_txid = Some(other);
            Some(settings)
        })
        .await
        .unwrap();
    }
    h.fault.store(0, Ordering::SeqCst);
    h.coordinator
        .reconcile_completion(&context(), &root)
        .await
        .unwrap();
    for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
        let cube = Settings::from_file(&root.network_directory(chain))
            .unwrap()
            .cubes
            .remove(0);
        assert_eq!(cube.split_completed_at_height, Some(101));
        assert_eq!(cube.split_completion_txid, Some(other));
    }
}

#[tokio::test]
async fn fork_panel_hot_signature_review_and_one_explicit_submission() {
    use crate::app::{
        cache::Cache,
        state::vault::claim::{fork_load::Loaded, fork_panel},
        wallet::Wallet,
    };
    use crate::utils::mock::Daemon as MockDaemon;
    let h = PreparingHarness::new().await;
    h.fault.store(0, Ordering::SeqCst);
    let construction = h.preparation.construction.clone();
    let psbt = h.psbt();
    let master =
        MasterSigner::from_mnemonic(Network::Bitcoin, Mnemonic::from_entropy(&[40; 16]).unwrap())
            .unwrap();
    let second =
        MasterSigner::from_mnemonic(Network::Bitcoin, Mnemonic::from_entropy(&[41; 16]).unwrap())
            .unwrap();
    let wallet = Arc::new(
        Wallet::new(construction.descriptor().clone())
            .with_chain(ChainId::BitcoinBlake2b)
            .with_signer(crate::signer::Signer::new(master)),
    );
    let daemon: Arc<dyn Daemon + Send + Sync> = Arc::new(crate::daemon::client::Coincubed::new(
        MockDaemon::new(vec![]).run(),
    ));
    let cache = Cache {
        network: Network::Bitcoin,
        fiat_chain: ChainId::BitcoinBlake2b,
        ..Cache::default()
    };
    let loaded = Loaded::Signing {
        preparation: h.preparation,
        psbt,
        context: context(),
    };
    let (panel, review_task, signed) = fork_panel::tests::signed_panel_awaiting_review(
        crate::dir::CoincubeDirectory::new(h._temp.0.clone()),
        wallet,
        loaded,
        daemon.clone(),
        &cache,
        second,
    )
    .await;
    // The actual GUI hot key uses the fork sighash; bind node acceptance to
    // this exact resulting witness, not the fixture's legacy-only witness.
    let verified = coincube_core::claim_finalize::finalize_claim_fork_sweep(
        &construction,
        &signed,
        &secp256k1::Secp256k1::verification_only(),
    )
    .unwrap();
    let tx = verified.transaction();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    httpmock::Mock::new(h.preflight_mock_id, &h._server)
        .delete_async()
        .await;
    h._server.mock_async(|when,then| {
        when.method(POST).path("/api/v1/esplora/bitcoin-blake2b/mainnet/tx/preflight");
        then.status(200).header("cache-control","no-store").json_body(json!({"success":true,"data":{"network":"bitcoin-blake2b","state":"available","result":{"txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),"tip_hash":hash(2),"observed_at":stamp,"allowed":true,"reject_reason":null}}}));
    }).await;
    fork_panel::tests::review_and_submit_once(panel, review_task, daemon, &cache, h.calls).await;
}
