//! Split (#568 B3b) step-2 tests: target reservation and freshness, the
//! construction under the token, the finish handoff, review and submission.
//! The harness is the B2 gate's: a real step 1 of a P2PKH foreign wallet (its
//! scriptSigs change every txid), a real journal, a synthetic two-chain view.
//! Step 2 is built by the core builder and signed by rust-bitcoin's reference
//! PSBT signer. The BTCB2 preflight is a local HTTP mock of Connect's.
use super::*;
use crate::daemon::model::GetAddressResult;
use crate::services::{
    claim_coordinator::fork::split::step2::{
        SplitStep2Coordinator, SplitStep2Production, Step2Error, Step2Transport, TargetError,
    },
    foreign_psbt::SweepFeeSource,
};
use coincube_core::{
    descriptors::CoincubeDescriptor,
    foreign_split::{finalize_split_step2, VerifiedSplitStep2},
    miniscript::bitcoin::{bip32::ChildNumber, psbt::Psbt},
};

const VAULT: &str = "wsh(or_d(multi(2,[ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<0;1>/*,[de6eb005/48'/1'/0'/2']tpubDFGuYfS2JwiUSEXiQuNGdT3R7WTDhbaE6jbUhgYSSdhmfQcSx7ZntMPPv7nrkvAqjpj3jX9wbhSGMeKVao4qAzhbNyBi7iQmv5xxQk6H6jz/<0;1>/*),and_v(v:pkh([ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<2;3>/*),older(3))))#p9ax3xxp";

fn vault() -> CoincubeDescriptor {
    CoincubeDescriptor::from_str(VAULT).unwrap()
}
fn other_vault() -> CoincubeDescriptor {
    CoincubeDescriptor::from_str(
        &VAULT
            .split('#')
            .next()
            .unwrap()
            .replace("older(3)", "older(4)"),
    )
    .unwrap()
}
fn address(descriptor: &CoincubeDescriptor, index: u32) -> Address {
    descriptor
        .receive_descriptor()
        .derive(
            ChildNumber::from_normal_idx(index).unwrap(),
            &Secp256k1::verification_only(),
        )
        .address(Network::Bitcoin)
}
/// The daemon's `get_new_address` answering `address` at `index`, as a
/// future the preparation spawns; counts how often it is polled.
fn reply(
    address: Address,
    index: u32,
    polls: &Arc<AtomicUsize>,
) -> impl std::future::Future<Output = Result<GetAddressResult, DaemonError>> + Send + 'static {
    let result = GetAddressResult::new(address, ChildNumber::from_normal_idx(index).unwrap());
    let polls = polls.clone();
    async move {
        polls.fetch_add(1, Ordering::SeqCst);
        Ok(result)
    }
}
fn reserved(
    descriptor: &CoincubeDescriptor,
    index: u32,
    polls: &Arc<AtomicUsize>,
) -> impl std::future::Future<Output = Result<GetAddressResult, DaemonError>> + Send + 'static {
    reply(address(descriptor, index), index, polls)
}
/// The harness wallet's claimed coins, exactly as step 1 was built from.
fn coins(wallet: &Wallet) -> Vec<SplitCoin> {
    vec![
        coin(&wallet.source, SplitBranch::External, 0, 150_000),
        coin(&wallet.source, SplitBranch::Internal, 1, 70_000),
    ]
}
struct Fees(Option<u64>);
#[async_trait]
impl SweepFeeSource for Fees {
    fn chain(&self) -> ChainId {
        ChainId::BitcoinBlake2b
    }
    async fn mid_priority_sat_vb(&self) -> Option<u64> {
        self.0
    }
}

/// A Connect-route transport: Connect's BTCB2 preflight (mocked over HTTP)
/// and a submission recorder.
struct Transport {
    origin: String,
    descriptor: CoincubeDescriptor,
    preflight: PreflightClient,
    submits: Arc<Mutex<Vec<(SubmissionRoute, Txid, ChildNumber)>>>,
}
#[async_trait]
impl Step2Transport for Transport {
    fn origin(&self) -> &str {
        &self.origin
    }
    fn descriptor(&self) -> &CoincubeDescriptor {
        &self.descriptor
    }
    async fn preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<RoutedEvidence, claim_preflight::Error> {
        self.preflight
            .observe(ChainId::BitcoinBlake2b, tx, tip, policy)
            .await
            .map(RoutedEvidence::Connect)
    }
    async fn submit(
        &self,
        route: SubmissionRoute,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        _gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        let tx = verified.transaction();
        self.submits
            .lock()
            .unwrap()
            .push((route, tx.compute_txid(), target));
        Ok(SubmissionOutcome::UpstreamAccepted {
            txid: tx.compute_txid(),
            wtxid: tx.compute_wtxid(),
        })
    }
}

/// The harness policy with the widest collection budget. Every review and
/// completion evidence is bounded by an evidence deadline
/// (`evidence_deadline`): the budget, capped at 30 s, measured on the real
/// monotonic clock from the start of its check. The harness's 2 s left a
/// heavily loaded run (four test binaries of 16 threads beside a cargo
/// build) expiring reviews and evidence before their use
/// (`ExpiredEvidence`, "expired while saving"); 30 s outlasts any such run.
/// Expiry itself is tested with `expire_for_test` or on a reconciler that
/// keeps the 2 s budget, not by widening it.
fn wide_policy() -> CheckPolicy {
    CheckPolicy {
        collection_budget: claim_observation::MAX_COLLECTION_TIME,
        ..policy()
    }
}
/// A preparation whose first check tracked step 1 (reservation needs it).
async fn tracked(h: &Harness) -> SplitPreparation {
    tracked_with(h, policy()).await
}
/// [`tracked`] under `policy`, which the coordinator it finishes into keeps.
async fn tracked_with(h: &Harness, policy: CheckPolicy) -> SplitPreparation {
    let mut preparation = h.prepare_with(policy).unwrap();
    let polls = Arc::new(AtomicUsize::new(0));
    assert!(matches!(
        preparation
            .reserve_target(
                &context(),
                &vault(),
                reserved(&vault(), INDEX, &polls),
                Duration::from_secs(5)
            )
            .await,
        Err(TargetError::NotTracking)
    ));
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    preparation.check_signing(&context()).await.unwrap();
    preparation
}

/// The harness plus a reserved, proven, checked and constructed step 2.
struct Step2 {
    h: Harness,
    preparation: SplitPreparation,
    psbt: Psbt,
}
const INDEX: u32 = 3;
impl Step2 {
    async fn new() -> Self {
        Self::with_policy(policy()).await
    }
    /// [`Self::new`] under `policy`, which the preparation and the
    /// coordinator it finishes into keep.
    async fn with_policy(policy: CheckPolicy) -> Self {
        let h = Harness::new(6).await;
        let mut preparation = tracked_with(&h, policy).await;
        let polls = Arc::new(AtomicUsize::new(0));
        assert_eq!(
            preparation
                .reserve_target(
                    &context(),
                    &vault(),
                    reserved(&vault(), INDEX, &polls),
                    Duration::from_secs(5)
                )
                .await
                .unwrap(),
            INDEX
        );
        preparation
            .prove_target(&context(), &vault())
            .await
            .unwrap();
        let token = preparation.check_signing(&context()).await.unwrap();
        let psbt = preparation
            .construct_step2(&context(), token, coins(&h.wallet), &Fees(Some(2)))
            .await
            .unwrap();
        Self {
            h,
            preparation,
            psbt,
        }
    }
    fn signed(&self) -> Psbt {
        let mut psbt = self.psbt.clone();
        psbt.sign(&self.h.wallet.signer, &Secp256k1::new()).unwrap();
        psbt
    }
}
/// A transport whose BTCB2 preflight answers for `tx` (`allowed` or not).
async fn transport(
    h: &Harness,
    tx: &Transaction,
    allowed: bool,
) -> (
    Transport,
    MockServer,
    Arc<Mutex<Vec<(SubmissionRoute, Txid, ChildNumber)>>>,
    usize,
) {
    let server = MockServer::start_async().await;
    let mock = mock_preflight(&server, tx, allowed).await;
    let submits = Arc::new(Mutex::new(Vec::new()));
    (
        Transport {
            origin: ORIGIN.to_owned(),
            descriptor: vault(),
            preflight: PreflightClient::new(
                &server.base_url(),
                CollectionContext {
                    expected_generation: 7,
                    generation: h.sender.subscribe(),
                },
            )
            .unwrap(),
            submits: submits.clone(),
        },
        server,
        submits,
        mock,
    )
}
async fn mock_preflight(server: &MockServer, tx: &Transaction, allowed: bool) -> usize {
    let stamp = now();
    let (txid, wtxid) = (tx.compute_txid(), tx.compute_wtxid());
    server.mock_async(|when, then| {
        when.method(POST).path("/api/v1/esplora/bitcoin-blake2b/mainnet/tx/preflight");
        then.status(200).header("cache-control", "no-store").json_body(json!({"success":true,"data":{"network":"bitcoin-blake2b","state":"available","result":{"txid":txid,"wtxid":wtxid,"tip_hash":hash(2),"observed_at":stamp,"allowed":allowed,"reject_reason":if allowed { serde_json::Value::Null } else { json!("policy-rejected") }}}}));
    }).await.id
}
fn finish(s: Step2, transport: Transport) -> (Harness, SplitStep2Coordinator) {
    let signed = s.signed();
    let coins = coins(&s.h.wallet);
    let coordinator = s
        .preparation
        .finish_with(&context(), &signed, &coins, Box::new(transport))
        .unwrap();
    (s.h, coordinator)
}
impl Step2 {
    /// The verified signed step 2 the coordinator will submit.
    fn signed_tx(&self) -> Transaction {
        finalize_split_step2(
            self.preparation.step2.as_ref().unwrap(),
            &coins(&self.h.wallet),
            &self.h.wallet.source,
            &self.signed(),
            &Secp256k1::verification_only(),
        )
        .unwrap()
        .transaction()
        .clone()
    }
}

/// Acceptance: only the claimed prevouts are spent, into the reserved target,
/// with no change; the journal records the reservation, the unsigned step 2,
/// then the signed bytes and a submission naming *their* txid (P2PKH changes
/// it); the route submits once, with the recorded index.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_spends_only_the_claimed_prevouts_into_the_reserved_target() {
    let s = Step2::new().await;
    let tx = &s.psbt.unsigned_tx;
    let spent: BTreeSet<_> = tx.input.iter().map(|i| i.previous_output).collect();
    assert_eq!(spent, s.h.prevouts().into_iter().collect());
    assert_eq!(tx.input.len(), s.h.prevouts().len());
    assert_eq!(tx.output.len(), 1);
    assert_eq!(
        tx.output[0].script_pubkey,
        address(&vault(), INDEX).script_pubkey()
    );
    let journal = s.h.temp.journal();
    assert_eq!(journal["split"]["target_index"], INDEX);
    assert_eq!(
        journal["fork_sweep"],
        serde_json::to_value(tx).unwrap(),
        "the unsigned step 2 is the restart record"
    );
    assert!(journal.get("fork_submission").is_none());

    let signed_tx = s.signed_tx();
    assert_ne!(signed_tx.compute_txid(), tx.compute_txid(), "pkh");
    let (transport, _server, submits, _) = transport(&s.h, &signed_tx, true).await;
    let (h, mut coordinator) = finish(s, transport);
    assert_eq!(coordinator.transaction(), &signed_tx);
    let review = coordinator.prepare_review(&context()).await.unwrap();
    assert_eq!(review.snapshot().txid, signed_tx.compute_txid());
    assert_eq!(review.snapshot().route, SubmissionRoute::Connect);
    assert!(h.temp.journal().get("fork_submission").is_none());
    let outcome = coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    assert_eq!(
        outcome,
        Outcome::UpstreamAccepted {
            txid: signed_tx.compute_txid(),
            wtxid: signed_tx.compute_wtxid(),
        }
    );
    assert_eq!(
        *submits.lock().unwrap(),
        vec![(
            SubmissionRoute::Connect,
            signed_tx.compute_txid(),
            ChildNumber::from_normal_idx(INDEX).unwrap()
        )]
    );
    let journal = h.temp.journal();
    assert_eq!(
        journal["fork_submission"]["txid"],
        signed_tx.compute_txid().to_string()
    );
    assert_eq!(
        journal["split"]["step2_transaction"],
        serde_json::to_value(&signed_tx).unwrap()
    );
    // Recorded: never sent again, only reconciled.
    assert!(matches!(
        coordinator.prepare_review(&context()).await,
        Err(Error::SubmissionAlreadyRecorded)
    ));
    assert_eq!(submits.lock().unwrap().len(), 1);
    drop(coordinator);
    // A reopened preparation refuses a recorded step-2 submission.
    assert!(matches!(h.prepare(), Err(Error::SubmissionAlreadyRecorded)));
}

/// #592 I12: the reservation is recorded once and reused. A second
/// reservation is refused without asking the daemon for another index, in
/// this preparation and after a reopen; the same index proves again.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_reservation_is_recorded_once_and_reused() {
    let h = Harness::new(6).await;
    let mut preparation = tracked(&h).await;
    assert!(preparation.needs_reservation().unwrap());
    assert!(matches!(
        preparation.prove_target(&context(), &vault()).await,
        Err(TargetError::NoReservation)
    ));
    let polls = Arc::new(AtomicUsize::new(0));
    preparation
        .reserve_target(
            &context(),
            &vault(),
            reserved(&vault(), INDEX, &polls),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert!(!preparation.needs_reservation().unwrap());
    assert!(matches!(
        preparation
            .reserve_target(
                &context(),
                &vault(),
                reserved(&vault(), INDEX + 1, &polls),
                Duration::from_secs(5)
            )
            .await,
        Err(TargetError::AlreadyReserved)
    ));
    assert_eq!(polls.load(Ordering::SeqCst), 1, "no index consumed");
    drop(preparation);
    let mut preparation = h.prepare().unwrap();
    assert_eq!(preparation.recorded_target().unwrap(), Some(INDEX));
    assert!(!preparation.needs_reservation().unwrap());
    assert!(matches!(
        preparation
            .reserve_target(
                &context(),
                &vault(),
                reserved(&vault(), INDEX + 1, &polls),
                Duration::from_secs(5)
            )
            .await,
        Err(TargetError::AlreadyReserved)
    ));
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    preparation
        .prove_target(&context(), &vault())
        .await
        .unwrap();
    // A reservation that is not this Vault's receive derivation refuses
    // and records nothing: another Vault's address, or another index's
    // address labelled with this one.
    let h = Harness::new(6).await;
    let mut preparation = tracked(&h).await;
    for (wrong, index) in [
        (address(&other_vault(), INDEX), INDEX),
        (address(&vault(), INDEX + 1), INDEX),
    ] {
        assert!(matches!(
            preparation
                .reserve_target(
                    &context(),
                    &vault(),
                    reply(wrong, index, &polls),
                    Duration::from_secs(5)
                )
                .await,
            Err(TargetError::NotTargetVault)
        ));
    }
    assert_eq!(preparation.recorded_target().unwrap(), None);
    // The wrong Vault cannot prove a recorded target either.
    preparation
        .reserve_target(
            &context(),
            &vault(),
            reserved(&vault(), INDEX, &polls),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert!(matches!(
        preparation.prove_target(&context(), &other_vault()).await,
        Err(TargetError::NotTargetVault)
    ));
}

/// #592 I10, I11, N1: freshness is a fresh Connect read of the address's
/// history on BTCB2 and on Bitcoin (a shared descriptor's Bitcoin-side use is
/// visible), not a daemon poll. A used address refuses on either chain; the
/// used index is never reserved again, and only a strictly higher index
/// replaces it. A failed read is not evidence of use.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_used_or_unproven_target_is_refused_and_never_reused() {
    for chain in [ChainId::BitcoinBlake2b, ChainId::Bitcoin] {
        let h = Harness::new(6).await;
        let mut preparation = tracked(&h).await;
        let polls = Arc::new(AtomicUsize::new(0));
        preparation
            .reserve_target(
                &context(),
                &vault(),
                reserved(&vault(), INDEX, &polls),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        // Both chains are read on every proof.
        preparation
            .prove_target(&context(), &vault())
            .await
            .unwrap();
        assert_eq!(h.chains.address_reads.load(Ordering::SeqCst), 2);
        let used = address(&vault(), INDEX).to_string();
        h.chains.edit(|view| {
            view.used.push((chain, used.clone()));
        });
        assert!(matches!(
            preparation.prove_target(&context(), &vault()).await,
            Err(TargetError::Used(c)) if c == chain
        ));
        // Not proven: no construction, even with a live token.
        let token = preparation.check_signing(&context()).await.unwrap();
        assert!(matches!(
            preparation
                .construct_step2(&context(), token, coins(&h.wallet), &Fees(Some(2)))
                .await,
            Err(Step2Error::TargetNotProven)
        ));
        assert!(h.temp.journal().get("fork_sweep").is_none());
        // The used index is not reserved again, nor a lower one.
        assert!(preparation.needs_reservation().unwrap());
        for index in [INDEX, INDEX - 1] {
            assert!(matches!(
                preparation
                    .reserve_target(
                        &context(),
                        &vault(),
                        reserved(&vault(), index, &polls),
                        Duration::from_secs(5)
                    )
                    .await,
                Err(TargetError::Coordinator(Error::Journal(
                    claim_workflow::Error::Conflict
                )))
            ));
            assert_eq!(h.temp.journal()["split"]["target_index"], INDEX);
        }
        // A higher index replaces it and proves fresh.
        preparation
            .reserve_target(
                &context(),
                &vault(),
                reserved(&vault(), INDEX + 1, &polls),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(h.temp.journal()["split"]["target_index"], INDEX + 1);
        preparation
            .prove_target(&context(), &vault())
            .await
            .unwrap();
        // ... and is then itself kept.
        assert!(matches!(
            preparation
                .reserve_target(
                    &context(),
                    &vault(),
                    reserved(&vault(), INDEX + 2, &polls),
                    Duration::from_secs(5)
                )
                .await,
            Err(TargetError::AlreadyReserved)
        ));
    }
    // A failed read refuses as unavailable, not used: the index is kept.
    let h = Harness::new(6).await;
    let mut preparation = tracked(&h).await;
    let polls = Arc::new(AtomicUsize::new(0));
    preparation
        .reserve_target(
            &context(),
            &vault(),
            reserved(&vault(), INDEX, &polls),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    h.chains.edit(|view| view.address_fails = true);
    assert!(matches!(
        preparation.prove_target(&context(), &vault()).await,
        Err(TargetError::Unavailable(
            ChainId::BitcoinBlake2b,
            FailureKind::Http(503)
        ))
    ));
    assert!(!preparation.needs_reservation().unwrap());
}

/// #592 N2: the reservation has a real wall-clock bound, even when the
/// daemon call blocks its thread (the embedded daemon's lock). Nothing is
/// recorded on a timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn split_step2_reservation_has_a_wall_clock_bound() {
    let h = Harness::new(6).await;
    let mut preparation = tracked(&h).await;
    for blocking in [false, true] {
        let started = Instant::now();
        let stuck = async move {
            if blocking {
                std::thread::sleep(Duration::from_secs(3));
            } else {
                std::future::pending::<()>().await;
            }
            Err::<GetAddressResult, _>(DaemonError::DaemonStopped)
        };
        assert!(matches!(
            preparation
                .reserve_target(&context(), &vault(), stuck, Duration::from_millis(200))
                .await,
            Err(TargetError::ReservationUnavailable)
        ));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "blocking={blocking}: {:?}",
            started.elapsed()
        );
    }
    assert_eq!(preparation.recorded_target().unwrap(), None);
    // A daemon error refuses the same way.
    assert!(matches!(
        preparation
            .reserve_target(
                &context(),
                &vault(),
                async { Err(DaemonError::DaemonStopped) },
                Duration::from_secs(5)
            )
            .await,
        Err(TargetError::ReservationUnavailable)
    ));
}

/// D4 and the token: no Connect BTCB2 fee is a refusal; a token from a
/// superseded check, or before any check, does not build; nothing is
/// journaled. The token is consumed by the attempt either way.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_needs_a_fee_a_live_token_and_a_check() {
    let h = Harness::new(6).await;
    let mut preparation = tracked(&h).await;
    let polls = Arc::new(AtomicUsize::new(0));
    preparation
        .reserve_target(
            &context(),
            &vault(),
            reserved(&vault(), INDEX, &polls),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    preparation
        .prove_target(&context(), &vault())
        .await
        .unwrap();
    // A token shaped for this journal's prevouts and txid, but minted by no
    // check of this preparation, does not build.
    let stray = || {
        ForeignStep2Authorization::for_test(
            &h.prevouts(),
            h.signed.compute_txid(),
            h.sender.subscribe(),
        )
    };
    let (token, _live) = stray();
    assert!(matches!(
        preparation
            .construct_step2(&context(), token, coins(&h.wallet), &Fees(Some(2)))
            .await,
        Err(Step2Error::Redeem(RedeemError::Stale))
    ));
    // Reopened: no check yet, so no BTCB2 tip for the locktime.
    drop(preparation);
    let mut preparation = h.prepare().unwrap();
    preparation
        .prove_target(&context(), &vault())
        .await
        .unwrap();
    let (token, _live) = stray();
    assert!(matches!(
        preparation
            .construct_step2(&context(), token, coins(&h.wallet), &Fees(Some(2)))
            .await,
        Err(Step2Error::NotChecked)
    ));
    let token = preparation.check_signing(&context()).await.unwrap();
    assert!(matches!(
        preparation
            .construct_step2(&context(), token, coins(&h.wallet), &Fees(None))
            .await,
        Err(Step2Error::FeeUnavailable)
    ));
    let first = preparation.check_signing(&context()).await.unwrap();
    let _second = preparation.check_signing(&context()).await.unwrap();
    assert!(matches!(
        preparation
            .construct_step2(&context(), first, coins(&h.wallet), &Fees(Some(2)))
            .await,
        Err(Step2Error::Redeem(RedeemError::Stale))
    ));
    // Only claimed coins: a coin set missing one refuses in the core builder.
    let token = preparation.check_signing(&context()).await.unwrap();
    assert!(matches!(
        preparation
            .construct_step2(
                &context(),
                token,
                coins(&h.wallet)[..1].to_vec(),
                &Fees(Some(2))
            )
            .await,
        Err(Step2Error::Construction(_))
    ));
    assert!(h.temp.journal().get("fork_sweep").is_none());
    // And then it builds.
    let token = preparation.check_signing(&context()).await.unwrap();
    preparation
        .construct_step2(&context(), token, coins(&h.wallet), &Fees(Some(2)))
        .await
        .unwrap();
    assert!(h.temp.journal().get("fork_sweep").is_some());
}

/// A reopened preparation rebuilds the recorded step 2 byte for byte (no
/// fee read, the recorded value and locktime) under a fresh token.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_restart_rebuilds_the_recorded_step2_exactly() {
    let s = Step2::new().await;
    // Keep the external signer's original file across the restart.
    let signed_before_restart = s.signed();
    let recorded = s.psbt.unsigned_tx.clone();
    let Step2 { h, preparation, .. } = s;
    drop(preparation);
    let mut preparation = h.prepare().unwrap();
    assert!(!preparation.needs_reservation().unwrap());
    preparation
        .prove_target(&context(), &vault())
        .await
        .unwrap();
    let token = preparation.check_signing(&context()).await.unwrap();
    let psbt = preparation
        .construct_step2(&context(), token, coins(&h.wallet), &Fees(None))
        .await
        .unwrap();
    assert_eq!(psbt.unsigned_tx, recorded);
    assert_eq!(
        preparation.check_signed(&signed_before_restart, &coins(&h.wallet)),
        Ok(()),
        "the original signer file remains valid without re-signing"
    );
    // A recorded step 2 fixes its target: ownership is checked, freshness
    // no longer (a later payment to the address cannot strand step 2).
    let used = address(&vault(), INDEX).to_string();
    h.chains.edit(|view| {
        view.used.push((ChainId::BitcoinBlake2b, used));
    });
    let reads = h.chains.address_reads.load(Ordering::SeqCst);
    preparation
        .prove_target(&context(), &vault())
        .await
        .unwrap();
    assert_eq!(h.chains.address_reads.load(Ordering::SeqCst), reads);
    assert!(matches!(
        preparation.prove_target(&context(), &other_vault()).await,
        Err(TargetError::NotTargetVault)
    ));
}

/// Acceptance: a refused BTCB2 preflight records nothing and keeps the
/// verified signed step 2; a later accepted preflight reviews the same bytes.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_preflight_refusal_keeps_the_signed_step2() {
    let s = Step2::new().await;
    let signed_tx = s.signed_tx();
    let (transport, server, submits, refusing) = transport(&s.h, &signed_tx, false).await;

    let (h, mut coordinator) = finish(s, transport);
    assert!(matches!(
        coordinator.prepare_review(&context()).await,
        Err(Error::PolicyRejected(NodePolicy::Rejected { .. }))
    ));
    assert_eq!(coordinator.transaction(), &signed_tx);
    assert!(h.temp.journal().get("fork_submission").is_none());
    assert!(submits.lock().unwrap().is_empty());
    httpmock::Mock::new(refusing, &server).delete_async().await;
    mock_preflight(&server, &signed_tx, true).await;
    let review = coordinator.prepare_review(&context()).await.unwrap();
    assert_eq!(review.snapshot().transaction, signed_tx);
}

/// Review re-applies the six-confirmation gate: a step 1 reorged out after
/// signing, or seen on BTCB2, blocks submission before anything is recorded.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_review_refuses_a_reorg_after_signing() {
    let s = Step2::new().await;
    let signed_tx = s.signed_tx();
    let (transport, _server, submits, _) = transport(&s.h, &signed_tx, true).await;
    let (h, mut coordinator) = finish(s, transport);
    h.chains.edit(|view| view.set_depth(5));
    assert!(matches!(
        coordinator.prepare_review(&context()).await,
        Err(Error::NotReady(_))
    ));
    h.chains.edit(|view| {
        view.set_depth(6);
        view.on_fork = true;
    });
    assert!(coordinator.prepare_review(&context()).await.is_err());
    h.chains.edit(|view| view.on_fork = false);
    let review = coordinator.prepare_review(&context()).await.unwrap();
    // The view changed between review and confirmation: refused.
    h.chains.edit(|view| view.set_depth(7));
    assert!(matches!(
        coordinator.confirm_and_submit(review, &context()).await,
        Err(Error::ChangedReview)
    ));
    assert!(h.temp.journal().get("fork_submission").is_none());
    assert!(submits.lock().unwrap().is_empty());
}

/// The finish handoff binds the transport to the Split's Connect origin and
/// the Vault the target was proven for, verifies the signatures against the
/// token-built construction, and kills the preparation's tokens.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_finish_binds_origin_vault_and_signatures() {
    for case in 0..4 {
        let s = Step2::new().await;
        let signed_tx = s.signed_tx();
        let (mut transport, _server, _, _) = transport(&s.h, &signed_tx, true).await;
        let mut signed = s.signed();
        match case {
            0 => transport.origin = "https://other.example/".into(),
            1 => transport.descriptor = other_vault(),
            2 => signed.inputs[0].partial_sigs.clear(),
            _ => {}
        }
        let coins = coins(&s.h.wallet);
        let mut preparation = s.preparation;
        let token = preparation.check_signing(&context()).await.unwrap();
        let result = preparation.finish_with(&context(), &signed, &coins, Box::new(transport));
        match case {
            0..=2 => assert!(matches!(result, Err(Error::InvalidBinding)), "{}", case),
            _ => {
                assert!(result.is_ok());
                // The preparation is gone with its tokens.
                assert!(!token.is_live());
            }
        }
    }
    // No step 2 built yet: nothing to finish.
    let h = Harness::new(6).await;
    let preparation = h.prepare().unwrap();
    let (transport, _server, _, _) = transport(&h, &h.signed, true).await;
    assert!(matches!(
        preparation.finish_with(&context(), h.step1.psbt(), &[], Box::new(transport)),
        Err(Error::InvalidReview)
    ));
}

/// Route admission (owner decision P4 reconciled with #592 N3): the target
/// Vault daemon must be embedded, on BTCB2 mainnet, and either on exactly
/// Connect's BTCB2 Esplora at the Split's origin (no token, no fallback) or
/// on a bound Bitcoind (managed Knots) node. Neither route is refused for
/// freshness reasons: proofs come from Connect, not the daemon.
#[test]
fn split_step2_production_admits_the_connect_and_node_routes_only() {
    use coincubed::config::{
        BitcoinBackend, BitcoinConfig, BitcoindConfig, BitcoindRpcAuth, Config, EsploraConfig,
    };
    let origin = "https://connect.example/";
    let endpoint = "https://connect.example/api/v1/esplora/bitcoin-blake2b/mainnet";
    let esplora = |addr: &str| EsploraConfig {
        addr: addr.to_owned(),
        token: None,
        fallback_addr: None,
        fallback_token: None,
        secondary_fallback_addr: None,
        secondary_fallback_token: None,
    };
    let node = BitcoinBackend::Bitcoind(BitcoindConfig {
        addr: "127.0.0.1:8332".parse().unwrap(),
        rpc_auth: BitcoindRpcAuth::CookieFile("/synthetic/.cookie".into()),
    });
    let config = |chain: ChainId, backend: Option<BitcoinBackend>| {
        Config::new(
            BitcoinConfig::new(chain, Duration::from_secs(30)),
            backend,
            log::LevelFilter::Off,
            vault(),
            coincubed::datadir::DataDirectory::new(std::path::PathBuf::from(
                "/synthetic-unused-split-step2",
            )),
        )
    };
    let admit = |config: Config, embedded: bool, base: &str| {
        let daemon: Arc<dyn Daemon + Send + Sync> = Arc::new(AdmissionDaemon { config, embedded });
        let mut client = CoincubeClient::new();
        client.base_url = base.into();
        SplitStep2Production::new(&client, daemon, 7, watch::channel(7).1)
    };
    let connect = || Some(BitcoinBackend::Esplora(esplora(endpoint)));
    // Admitted.
    let admitted = admit(config(ChainId::BitcoinBlake2b, connect()), true, origin).unwrap();
    assert_eq!(admitted.origin(), origin);
    assert_eq!(admitted.descriptor(), &vault());
    assert!(admit(
        config(ChainId::BitcoinBlake2b, Some(node.clone())),
        true,
        origin
    )
    .is_ok());
    // Refused.
    let mut fallback = config(ChainId::BitcoinBlake2b, connect());
    fallback.fallback_esplora = Some(esplora("https://mempool.example/api"));
    let mut token = esplora(endpoint);
    token.token = Some("synthetic".into());
    let mut secondary = esplora(endpoint);
    secondary.fallback_addr = Some("https://other.example/api".into());
    for (config, embedded, base) in [
        (config(ChainId::BitcoinBlake2b, connect()), false, origin),
        (config(ChainId::Bitcoin, connect()), true, origin),
        (
            config(ChainId::BitcoinBlake2bTestnet4, connect()),
            true,
            origin,
        ),
        (
            config(ChainId::BitcoinBlake2b, connect()),
            true,
            "https://other.example/",
        ),
        (
            config(ChainId::BitcoinBlake2b, connect()),
            true,
            "https://connect.example/api/",
        ),
        (
            config(
                ChainId::BitcoinBlake2b,
                Some(BitcoinBackend::Esplora(esplora(
                    "https://connect.example/api/v1/esplora/bitcoin/mainnet",
                ))),
            ),
            true,
            origin,
        ),
        (
            config(
                ChainId::BitcoinBlake2b,
                Some(BitcoinBackend::Esplora(token)),
            ),
            true,
            origin,
        ),
        (
            config(
                ChainId::BitcoinBlake2b,
                Some(BitcoinBackend::Esplora(secondary)),
            ),
            true,
            origin,
        ),
        (fallback, true, origin),
        (config(ChainId::BitcoinBlake2b, None), true, origin),
    ] {
        assert!(admit(config, embedded, base).is_err());
    }
}

#[derive(Debug)]
struct AdmissionDaemon {
    config: coincubed::config::Config,
    embedded: bool,
}
#[async_trait::async_trait]
impl Daemon for AdmissionDaemon {
    fn backend(&self) -> crate::daemon::DaemonBackend {
        if self.embedded {
            crate::daemon::DaemonBackend::EmbeddedCoincubed(None)
        } else {
            crate::daemon::DaemonBackend::ExternalCoincubed
        }
    }

    fn config(&self) -> Option<&coincubed::config::Config> {
        Some(&self.config)
    }

    async fn is_alive(
        &self,
        _datadir: &crate::dir::CoincubeDirectory,
        _network: coincube_core::miniscript::bitcoin::Network,
    ) -> Result<(), DaemonError> {
        unreachable!("_is_alive: admission makes no daemon call")
    }
    async fn stop(&self) -> Result<(), DaemonError> {
        unreachable!("_stop: admission makes no daemon call")
    }
    async fn get_info(&self) -> Result<crate::daemon::model::GetInfoResult, DaemonError> {
        unreachable!("_get_info: admission makes no daemon call")
    }
    async fn request_sync(&self) -> Result<(), DaemonError> {
        unreachable!("_request_sync: admission makes no daemon call")
    }
    async fn get_new_address(&self) -> Result<crate::daemon::model::GetAddressResult, DaemonError> {
        unreachable!("_get_new_address: admission makes no daemon call")
    }
    async fn list_revealed_addresses(
        &self,
        _is_change: bool,
        _exclude_used: bool,
        _limit: usize,
        _start_index: Option<coincube_core::miniscript::bitcoin::bip32::ChildNumber>,
    ) -> Result<crate::daemon::model::ListRevealedAddressesResult, DaemonError> {
        unreachable!("_list_revealed_addresses: admission makes no daemon call")
    }
    async fn update_deriv_indexes(
        &self,
        _receive: Option<u32>,
        _change: Option<u32>,
    ) -> Result<coincubed::commands::UpdateDerivIndexesResult, DaemonError> {
        unreachable!("_update_deriv_indexes: admission makes no daemon call")
    }
    async fn list_coins(
        &self,
        _statuses: &[coincubed::commands::CoinStatus],
        _outpoints: &[coincube_core::miniscript::bitcoin::OutPoint],
    ) -> Result<crate::daemon::model::ListCoinsResult, DaemonError> {
        unreachable!("_list_coins: admission makes no daemon call")
    }
    async fn list_spend_txs(&self) -> Result<crate::daemon::model::ListSpendResult, DaemonError> {
        unreachable!("_list_spend_txs: admission makes no daemon call")
    }
    async fn create_spend_tx(
        &self,
        _coins_outpoints: &[coincube_core::miniscript::bitcoin::OutPoint],
        _destinations: &std::collections::HashMap<
            coincube_core::miniscript::bitcoin::Address<
                coincube_core::miniscript::bitcoin::address::NetworkUnchecked,
            >,
            u64,
        >,
        _feerate_vb: u64,
        _change_address: Option<
            coincube_core::miniscript::bitcoin::Address<
                coincube_core::miniscript::bitcoin::address::NetworkUnchecked,
            >,
        >,
    ) -> Result<crate::daemon::model::CreateSpendResult, DaemonError> {
        unreachable!("_create_spend_tx: admission makes no daemon call")
    }
    async fn rbf_psbt(
        &self,
        _txid: &coincube_core::miniscript::bitcoin::Txid,
        _is_cancel: bool,
        _feerate_vb: Option<u64>,
    ) -> Result<crate::daemon::model::CreateSpendResult, DaemonError> {
        unreachable!("_rbf_psbt: admission makes no daemon call")
    }
    async fn update_spend_tx(
        &self,
        _psbt: &coincube_core::miniscript::bitcoin::psbt::Psbt,
    ) -> Result<(), DaemonError> {
        unreachable!("_update_spend_tx: admission makes no daemon call")
    }
    async fn delete_spend_tx(
        &self,
        _txid: &coincube_core::miniscript::bitcoin::Txid,
    ) -> Result<(), DaemonError> {
        unreachable!("_delete_spend_tx: admission makes no daemon call")
    }
    async fn broadcast_spend_tx(
        &self,
        _txid: &coincube_core::miniscript::bitcoin::Txid,
    ) -> Result<(), DaemonError> {
        unreachable!("_broadcast_spend_tx: admission makes no daemon call")
    }
    async fn start_rescan(&self, _t: u32) -> Result<(), DaemonError> {
        unreachable!("_start_rescan: admission makes no daemon call")
    }
    async fn list_confirmed_txs(
        &self,
        _start: u32,
        _end: u32,
        _limit: u64,
    ) -> Result<crate::daemon::model::ListTransactionsResult, DaemonError> {
        unreachable!("_list_confirmed_txs: admission makes no daemon call")
    }
    async fn create_recovery(
        &self,
        _address: coincube_core::miniscript::bitcoin::Address<
            coincube_core::miniscript::bitcoin::address::NetworkUnchecked,
        >,
        _coins_outpoints: &[coincube_core::miniscript::bitcoin::OutPoint],
        _feerate_vb: u64,
        _sequence: Option<u16>,
    ) -> Result<coincube_core::miniscript::bitcoin::psbt::Psbt, DaemonError> {
        unreachable!("_create_recovery: admission makes no daemon call")
    }
    async fn list_txs(
        &self,
        _txid: &[coincube_core::miniscript::bitcoin::Txid],
    ) -> Result<crate::daemon::model::ListTransactionsResult, DaemonError> {
        unreachable!("_list_txs: admission makes no daemon call")
    }
    async fn get_labels(
        &self,
        _labels: &std::collections::HashSet<coincubed::commands::LabelItem>,
    ) -> Result<std::collections::HashMap<String, String>, DaemonError> {
        unreachable!("_get_labels: admission makes no daemon call")
    }
    async fn update_labels(
        &self,
        _labels: &std::collections::HashMap<coincubed::commands::LabelItem, Option<String>>,
    ) -> Result<(), DaemonError> {
        unreachable!("_update_labels: admission makes no daemon call")
    }
    async fn get_labels_bip329(
        &self,
        _offset: u32,
        _limit: u32,
    ) -> Result<coincubed::bip329::Labels, DaemonError> {
        unreachable!("_get_labels_bip329: admission makes no daemon call")
    }
}

/// #630 F1 (I10, N1): every address-history read must itself be fresh. A
/// read an hour old on BTCB2, or on Bitcoin, refuses as unavailable and
/// proves nothing (no index is marked used); the control with fresh reads
/// on both chains proves the target.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_target_proof_needs_fresh_reads_on_both_chains() {
    let h = Harness::new(6).await;
    let mut preparation = tracked(&h).await;
    let polls = Arc::new(AtomicUsize::new(0));
    preparation
        .reserve_target(
            &context(),
            &vault(),
            reserved(&vault(), INDEX, &polls),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    for chain in [ChainId::BitcoinBlake2b, ChainId::Bitcoin] {
        h.chains.edit(|view| view.address_stale = Some(chain));
        assert!(
            matches!(
                preparation.prove_target(&context(), &vault()).await,
                Err(TargetError::Unavailable(c, FailureKind::Stale)) if c == chain
            ),
            "{:?}",
            chain
        );
        assert!(!preparation.needs_reservation().unwrap());
        // Not proven: no construction.
        let token = preparation.check_signing(&context()).await.unwrap();
        assert!(matches!(
            preparation
                .construct_step2(&context(), token, coins(&h.wallet), &Fees(Some(2)))
                .await,
            Err(Step2Error::TargetNotProven)
        ));
    }
    // Control: fresh on both chains.
    h.chains.edit(|view| view.address_stale = None);
    preparation
        .prove_target(&context(), &vault())
        .await
        .unwrap();
}

/// #630 F2 (I10): construction needs a target proof no older than the
/// observation age (60 s here). With the services' clock past that age the
/// proof refuses as `TargetNotProven` and nothing is journaled; the control,
/// at the proof's own time, builds.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_construction_refuses_a_target_proof_past_the_observation_age() {
    let h = Harness::new(6).await;
    let mut preparation = tracked(&h).await;
    let polls = Arc::new(AtomicUsize::new(0));
    preparation
        .reserve_target(
            &context(),
            &vault(),
            reserved(&vault(), INDEX, &polls),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    preparation
        .prove_target(&context(), &vault())
        .await
        .unwrap();
    let token = preparation.check_signing(&context()).await.unwrap();
    // The clock moves past the proof's age between the check and the build.
    h.chains.edit(|view| view.clock_offset = 61);
    assert!(matches!(
        preparation
            .construct_step2(&context(), token, coins(&h.wallet), &Fees(Some(2)))
            .await,
        Err(Step2Error::TargetNotProven)
    ));
    assert!(h.temp.journal().get("fork_sweep").is_none());
    // Control: the same preparation, the clock back, a fresh token.
    h.chains.edit(|view| view.clock_offset = 0);
    let token = preparation.check_signing(&context()).await.unwrap();
    preparation
        .construct_step2(&context(), token, coins(&h.wallet), &Fees(Some(2)))
        .await
        .unwrap();
    assert!(h.temp.journal().get("fork_sweep").is_some());
}

/// #637 F2: `check_signed` verifies a signed step 2 against the construction
/// built under the token without consuming the preparation: the full
/// signature set is Ok, the unsigned (partial) PSBT is `Unsatisfied`, a
/// file of another transaction is refused, and before any construction
/// there is nothing to check against.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_check_signed_tells_complete_partial_and_wrong_apart() {
    use coincube_core::foreign_split::FinalizeError;
    let s = Step2::new().await;
    let coins = coins(&s.h.wallet);
    assert_eq!(s.preparation.check_signed(&s.signed(), &coins), Ok(()));
    assert_eq!(
        s.preparation.check_signed(&s.psbt, &coins),
        Err(FinalizeError::Unsatisfied)
    );
    let mut other = s.signed();
    other.unsigned_tx.output[0].value =
        Amount::from_sat(other.unsigned_tx.output[0].value.to_sat() - 1);
    assert!(s.preparation.check_signed(&other, &coins).is_err());
    assert!(s
        .preparation
        .check_signed(&s.signed(), &coins[..1])
        .is_err());
    // It consumed nothing: the preparation still hands over.
    let signed_tx = s.signed_tx();
    let (transport, _server, _, _) = transport(&s.h, &signed_tx, true).await;
    let (_h, coordinator) = finish(s, transport);
    assert_eq!(coordinator.transaction(), &signed_tx);
    // No construction yet: refused.
    let h = Harness::new(6).await;
    let preparation = tracked(&h).await;
    assert_eq!(
        preparation.check_signed(&h.step1.psbt().clone(), &coins),
        Err(FinalizeError::ConstructionChanged)
    );
}

mod completion;
mod reorg;
mod routes;
mod unified_journal;
