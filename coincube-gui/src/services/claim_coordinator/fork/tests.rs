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
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        self.read(
            chain,
            if chain == ChainId::Bitcoin && self.fault.load(Ordering::SeqCst) != 10 {
                TransactionObservation::Confirmed {
                    txid,
                    block: BlockRef {
                        height: if self.fault.load(Ordering::SeqCst) == 6 {
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
        server.mock_async(|when,then| { when.method(POST).path("/api/v1/esplora/bitcoin-blake2b/mainnet/tx/preflight"); then.status(200).header("cache-control","no-store").json_body(json!({"success":true,"data":{"network":"bitcoin-blake2b","state":"available","result":{"txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),"tip_hash":hash(2),"observed_at":stamp,"allowed":accepted,"reject_reason":if accepted { serde_json::Value::Null } else { json!("policy-rejected") }}}})); }).await;
        let fault = Arc::new(AtomicUsize::new(10));
        let calls = Arc::new(AtomicUsize::new(0));
        let reached = Arc::new(tokio::sync::Notify::new());
        let fixture = Fixture {
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
