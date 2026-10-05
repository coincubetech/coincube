//! Split (#568 S4) reorg recovery after the step-2 submission, D13 = A: step
//! 1 is never resent. Step 2 is submitted through the coordinator over the
//! Connect route, then the journal is reopened as a restart does; the
//! synthetic view says where step 1 is on Bitcoin.
use super::*;
use crate::services::{
    claim_coordinator::fork::split::step2::{ResendError, SplitStep2Reconciler, Step1AfterStep2},
    claim_workflow::{Reconfirmation, Step1Conflict},
};

/// Step 2 reviewed and submitted through the coordinator, which is then
/// dropped: a journal with a recorded step-2 submission, and its txid.
async fn submitted() -> (Harness, Txid) {
    let s = Step2::new().await;
    let signed_tx = s.signed_tx();
    let (transport, _server, _, _) = transport(&s.h, &signed_tx, true).await;
    let (h, mut coordinator) = finish(s, transport);
    let review = coordinator.prepare_review(&context()).await.unwrap();
    coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    drop(coordinator);
    (h, signed_tx.compute_txid())
}
/// The journal reopened by the reconciler over `services`.
fn reopen(h: &Harness, services: Box<dyn SplitForkServices>) -> SplitStep2Reconciler {
    SplitStep2Reconciler::open(
        &h.temp.0,
        TARGET.into(),
        h.step1.source().digest(),
        context(),
        h.sender.subscribe(),
        services,
        policy(),
    )
    .unwrap()
}
/// The step-1 coordinator's own open on `h`'s journal, as a restart's
/// resume reaches it.
fn resume_step1(h: &Harness) -> Result<Step1Coordinator, Error> {
    Step1Coordinator::open_split(
        &h.temp.0,
        TARGET.into(),
        &h.step1,
        h.verified(),
        FORK,
        context(),
        h.sender.subscribe(),
        Box::new(h.chains.clone()),
        policy(),
        true,
    )
}
/// `h`'s journal reopened and verified as the step-1 coordinator's own
/// controller, without the coordinator's open.
fn step1_controller(h: &Harness) -> Controller {
    let identity = claim_workflow::split_identity(TARGET.into(), h.step1.source().digest());
    let mut controller =
        Controller::reopen_settling_blocking(&h.temp.0, &identity, context()).unwrap();
    controller
        .revalidate_split_construction(&context(), &h.step1, FORK)
        .unwrap();
    controller
        .bind_recovered_split_transaction(&context(), &h.verified())
        .unwrap();
    controller
}
/// Step 1 reorged out of Bitcoin, with a tip the step-1 preflight mock
/// answers for: a resend of step 1 would be reviewable.
fn step1_gone(h: &Harness) {
    h.chains.edit(|view| {
        view.step1_block = None;
        view.bitcoin_tip = BlockRef {
            height: 106,
            hash: hash(0x40),
        };
    });
}

/// S4 guard: once a step-2 submission is recorded, the step-1 coordinator
/// never reopens the journal, so step 1 can't be reviewed, resent or
/// reconfirmed there again. Before it, the same open succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn split_resume_refuses_a_recorded_step2_submission() {
    let before = Harness::new(6).await;
    assert!(resume_step1(&before).is_ok());

    let (h, _) = submitted().await;
    assert!(matches!(
        resume_step1(&h),
        Err(Error::SubmissionAlreadyRecorded)
    ));
    // Nothing was written by the refused open, and the reconciler still
    // opens it.
    let journal = h.temp.journal();
    assert!(journal["fork_submission"].is_object());
    drop(reopen(&h, Box::new(h.chains.clone())));
    assert_eq!(h.temp.journal(), journal);
}

/// S4 guard: the journal itself refuses a step-1 resend once a step-2
/// submission is recorded, whatever coordinator holds it. A step-1
/// coordinator opened before the record (its controller replaced by one
/// reopened on a journal with step 2 recorded) can neither prepare a resend
/// nor confirm one it reviewed before; nothing is recorded or sent.
#[tokio::test(flavor = "multi_thread")]
async fn split_resubmission_is_refused_after_a_step2_submission() {
    // A journal with step 2 recorded, and step 1 reorged out of Bitcoin.
    let (recorded, _) = submitted().await;
    step1_gone(&recorded);
    let before = recorded.temp.journal();

    // Prepare: a coordinator opened on a journal without step 2, then
    // holding the recorded one.
    let fresh = Harness::new(6).await;
    step1_gone(&fresh);
    let mut coordinator = resume_step1(&fresh).unwrap();
    drop(std::mem::replace(
        &mut coordinator.controller,
        step1_controller(&recorded),
    ));
    assert!(matches!(
        coordinator.prepare_resubmission(&context()).await,
        Err(Error::NotReady(_))
    ));
    drop(coordinator);

    // Confirm: the resend was reviewed while the journal had no step 2.
    let mut coordinator = resume_step1(&fresh).unwrap();
    let review = coordinator.prepare_resubmission(&context()).await.unwrap();
    assert_eq!(review.snapshot().transaction, fresh.signed);
    drop(std::mem::replace(
        &mut coordinator.controller,
        step1_controller(&recorded),
    ));
    // The step-1 services panic on any submission.
    assert!(matches!(
        coordinator.confirm_resubmission(review, &context()).await,
        Err(Error::NotReady(_))
    ));
    drop(coordinator);
    let after = recorded.temp.journal();
    assert_eq!(after["bitcoin_attempts"], before["bitcoin_attempts"]);
    assert_eq!(after, before);
}

/// Step 2's BTCB2 block at exactly [`MIN_CONFIRMATIONS`] below the fork tip.
const STEP2_HEIGHT: u64 = FORK_TIP + 1 - MIN_CONFIRMATIONS;
fn step2_confirmed(txid: Txid) -> TransactionObservation {
    TransactionObservation::Confirmed {
        txid,
        block: BlockRef {
            height: STEP2_HEIGHT,
            hash: hash(2),
        },
    }
}
/// Step 1's recorded Bitcoin block (the harness view's).
fn recorded_block() -> BlockRef {
    BlockRef {
        height: 100,
        hash: hash(6),
    }
}
/// Another block step 1 is re-mined in.
fn moved() -> BlockRef {
    BlockRef {
        height: 101,
        hash: hash(9),
    }
}
/// The Bitcoin tip once step 1 is reorged out (the step-1 preflight mock's).
fn gone_tip() -> BlockRef {
    BlockRef {
        height: 106,
        hash: hash(0x40),
    }
}

type ExtraHook = Box<dyn FnOnce(&mut Extra) + Send>;
/// A conflict-read case: its name, how to set it and undo it, and what the
/// reconcile reports.
type ReadCase = (
    &'static str,
    fn(&mut Extra),
    fn(&mut Extra),
    Step1AfterStep2,
);
/// What the harness view does not model of Bitcoin: step 1 waiting in the
/// mempool, claimed coins spent there by another transaction, and the reads
/// a step-1 conflict check makes.
#[derive(Default)]
struct Extra {
    /// Step 1 in the Bitcoin mempool while it is in no block.
    mempool: bool,
    /// Claimed coins spent on Bitcoin by another transaction.
    spent: BTreeSet<OutPoint>,
    unspent_fails: bool,
    /// Seconds subtracted from each Bitcoin unspent read's stamp.
    unspent_age: i64,
    previous_fails: bool,
    /// The previous transaction served is not the one asked for.
    previous_tampered: bool,
    /// Run once, at the next Bitcoin unspent read.
    on_unspent: Option<ExtraHook>,
    unspent_reads: usize,
    previous: Vec<Transaction>,
    claimed: Vec<OutPoint>,
}
#[derive(Clone)]
struct Step1View {
    inner: Chains,
    step1: Txid,
    extra: Arc<Mutex<Extra>>,
}
impl Step1View {
    fn new(h: &Harness) -> Self {
        Self {
            inner: h.chains.clone(),
            step1: h.signed.compute_txid(),
            extra: Arc::new(Mutex::new(Extra {
                previous: coins(&h.wallet).into_iter().map(|c| c.previous).collect(),
                claimed: h.prevouts(),
                ..Extra::default()
            })),
        }
    }
    fn edit(&self, edit: impl FnOnce(&mut Extra)) {
        edit(&mut self.extra.lock().unwrap());
    }
    /// Step 1 in no Bitcoin block.
    fn step1_gone(&self) {
        self.inner.edit(|view| {
            view.step1_block = None;
            view.bitcoin_tip = gone_tip();
        });
    }
    /// Step 1 in `block`, `depth` deep.
    fn step1_in(&self, block: BlockRef, depth: u64) {
        self.inner.edit(|view| {
            view.step1_block = Some(block);
            view.set_depth(depth);
        });
    }
}
#[async_trait]
impl ObservationSource for Step1View {
    fn now(&self) -> i64 {
        self.inner.now()
    }
    async fn anchor(&self, chain: ChainId) -> Result<NetworkAnchorStatus, FailureKind> {
        self.inner.anchor(chain).await
    }
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        self.inner.tip(chain).await
    }
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        let read = self.inner.transaction(chain, txid).await?;
        if chain == ChainId::Bitcoin
            && txid == self.step1
            && *read.value() == TransactionObservation::Absent
            && self.extra.lock().unwrap().mempool
        {
            return Chains::read(chain, TransactionObservation::Unconfirmed { txid });
        }
        Ok(read)
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        self.inner.hash_at_height(chain, height).await
    }
}
#[async_trait]
impl SplitForkServices for Step1View {
    fn source(&self) -> &dyn ObservationSource {
        self
    }
    fn origin(&self) -> &str {
        ORIGIN
    }
    async fn btcb2_unspent(&self, address: &str) -> Result<FreshRead<Vec<OutPoint>>, FailureKind> {
        self.inner.btcb2_unspent(address).await
    }
    async fn address_used(
        &self,
        chain: ChainId,
        address: &str,
    ) -> Result<FreshRead<bool>, FailureKind> {
        self.inner.address_used(chain, address).await
    }
    async fn bitcoin_unspent(&self, _: &str) -> Result<FreshRead<Vec<OutPoint>>, FailureKind> {
        let mut extra = self.extra.lock().unwrap();
        extra.unspent_reads += 1;
        if let Some(hook) = extra.on_unspent.take() {
            hook(&mut extra);
        }
        if extra.unspent_fails {
            return Err(FailureKind::Http(503));
        }
        let unspent = extra
            .claimed
            .iter()
            .filter(|outpoint| !extra.spent.contains(outpoint))
            .copied()
            .collect();
        Chains::read_aged(ChainId::Bitcoin, unspent, extra.unspent_age)
    }
    async fn previous_transaction(&self, txid: Txid) -> Result<Transaction, FailureKind> {
        let extra = self.extra.lock().unwrap();
        if extra.previous_fails {
            return Err(FailureKind::Http(503));
        }
        let mut previous = extra
            .previous
            .iter()
            .find(|tx| tx.compute_txid() == txid)
            .cloned()
            .ok_or(FailureKind::Http(404))?;
        if extra.previous_tampered {
            previous.output[0].value = Amount::from_sat(1);
        }
        Ok(previous)
    }
}

/// The harness transport, counting sends; `refuse`: every send comes back
/// refused before any byte leaves (the step-2 dead end a resend follows).
struct Sending {
    inner: Transport,
    refuse: bool,
    sends: Arc<AtomicUsize>,
}
#[async_trait]
impl Step2Transport for Sending {
    fn origin(&self) -> &str {
        self.inner.origin()
    }
    fn descriptor(&self) -> &CoincubeDescriptor {
        self.inner.descriptor()
    }
    async fn preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<RoutedEvidence, claim_preflight::Error> {
        self.inner.preflight(tx, tip, policy).await
    }
    async fn submit(
        &self,
        route: SubmissionRoute,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        if self.refuse {
            return Err(DaemonError::PoisonSubmission(
                coincubed::poison_broadcast::SubmissionError::BackendUnavailable,
            ));
        }
        self.inner.submit(route, verified, target, gate).await
    }
}

/// Step 2 checked, built, signed, reviewed and submitted through the
/// coordinator, every read through a [`Step1View`]; the coordinator is kept.
struct Submitted {
    h: Harness,
    view: Step1View,
    coordinator: SplitStep2Coordinator,
    txid: Txid,
    sends: Arc<AtomicUsize>,
    _server: MockServer,
}
async fn submitted_over(refuse: bool) -> Submitted {
    let h = Harness::new(6).await;
    let view = Step1View::new(&h);
    let mut preparation = SplitPreparation::open(
        &h.temp.0,
        TARGET.into(),
        &h.step1,
        h.verified(),
        FORK,
        context(),
        h.sender.subscribe(),
        Box::new(view.clone()),
        policy(),
    )
    .unwrap();
    preparation.check_signing(&context()).await.unwrap();
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
    let psbt = preparation
        .construct_step2(&context(), token, coins(&h.wallet), &Fees(Some(2)))
        .await
        .unwrap();
    let s = Step2 {
        h,
        preparation,
        psbt,
    };
    let signed_tx = s.signed_tx();
    let (inner, server, _, _) = transport(&s.h, &signed_tx, true).await;
    let sends = Arc::new(AtomicUsize::new(0));
    let psbt = s.signed();
    let coins = coins(&s.h.wallet);
    let Step2 { h, preparation, .. } = s;
    let mut coordinator = preparation
        .finish_with(
            &context(),
            &psbt,
            &coins,
            Box::new(Sending {
                inner,
                refuse,
                sends: sends.clone(),
            }),
        )
        .unwrap();
    let review = coordinator.prepare_review(&context()).await.unwrap();
    let outcome = coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    assert_eq!(
        matches!(outcome, Outcome::Uncertain { .. }),
        refuse,
        "{:?}",
        outcome
    );
    if refuse {
        assert_eq!(h.temp.journal()["split"]["step2_returned"], true);
    }
    Submitted {
        h,
        view,
        coordinator,
        txid: signed_tx.compute_txid(),
        sends,
        _server: server,
    }
}

/// O1: step 1 re-mined in another Bitcoin block after the step-2
/// submission. A reconcile reports it; a review names the recorded block and
/// the new one; its acknowledgement adds one inclusion-history entry and
/// makes the new block step 1's recorded one, on the live coordinator and on
/// the reconciler of a restart alike. Nothing is sent, and the step-2
/// records (the submission, the resend permission, the attempts) and step
/// 1's bytes are untouched. Afterwards step 1 is eligible again.
#[tokio::test(flavor = "multi_thread")]
async fn split_step1_reconfirmation_after_step2_grows_inclusion_history_and_sends_nothing() {
    for live in [true, false] {
        let Submitted {
            h,
            view,
            mut coordinator,
            sends,
            ..
        } = submitted_over(true).await;
        let before = h.temp.journal();
        assert_eq!(
            before["plan"]["previous_confirmation"],
            serde_json::to_value(recorded_block()).unwrap()
        );
        assert!(before
            .get("inclusion_history")
            .is_none_or(|h| h == &json!([])));
        view.step1_in(moved(), 6);
        let remined = Step1AfterStep2::Remined {
            previous: recorded_block(),
            confirmed: moved(),
        };
        let inclusion = Reconfirmation {
            previous: recorded_block(),
            confirmed: moved(),
        };
        if live {
            let reconciled = coordinator.reconcile_sweep(&context()).await.unwrap();
            assert_eq!(reconciled.after_step2, remined);
            assert_eq!(reconciled.status, Status::Observation(Assessment::Reorged));
            let review = coordinator
                .prepare_step1_reconfirmation(&context())
                .await
                .unwrap();
            assert_eq!(review.inclusion(), inclusion);
            coordinator
                .confirm_step1_reconfirmation(review, &context())
                .await
                .unwrap();
            let reconciled = coordinator.reconcile_sweep(&context()).await.unwrap();
            assert_eq!(reconciled.after_step2, Step1AfterStep2::Eligible);
        } else {
            drop(coordinator);
            let mut reconciler = reopen(&h, Box::new(view.clone()));
            let reconciled = reconciler.reconcile_sweep(&context()).await.unwrap();
            assert_eq!(reconciled.after_step2, remined);
            let review = reconciler
                .prepare_step1_reconfirmation(&context())
                .await
                .unwrap();
            assert_eq!(review.inclusion(), inclusion);
            reconciler
                .confirm_step1_reconfirmation(review, &context())
                .await
                .unwrap();
            let reconciled = reconciler.reconcile_sweep(&context()).await.unwrap();
            assert_eq!(reconciled.after_step2, Step1AfterStep2::Eligible);
        }
        let after = h.temp.journal();
        assert_eq!(
            after["inclusion_history"],
            json!([serde_json::to_value(inclusion).unwrap()]),
            "live: {}",
            live
        );
        assert_eq!(
            after["plan"]["previous_confirmation"],
            serde_json::to_value(moved()).unwrap()
        );
        for key in [
            "phase",
            "signed_txid",
            "bitcoin_transaction",
            "bitcoin_attempts",
            "fork_submission",
            "fork_sweep",
        ] {
            assert_eq!(after[key], before[key], "{}", key);
        }
        assert_eq!(after["split"], before["split"]);
        assert_eq!(after["split"]["step2_returned"], true);
        assert_eq!(sends.load(Ordering::SeqCst), 1, "only the original send");
    }
}

/// O1's review is one use and bound: a later review supersedes it, it
/// expires, another owner's review (same revision) is refused, and a changed
/// view (a new block, or step 1 re-mined elsewhere again) between review and
/// acknowledgement refuses. None of these records anything. A step 1 still
/// in its recorded block is not reviewable, nor (S4-D4) one past the RDTS
/// margin. A step-2 sighting during the review is recorded, which ends the
/// resend, and the acknowledgement still goes through.
#[tokio::test(flavor = "multi_thread")]
async fn split_step1_reconfirmation_review_is_one_use_expires_and_refuses_a_changed_view() {
    let Submitted {
        h,
        view,
        coordinator,
        txid,
        ..
    } = submitted_over(true).await;
    drop(coordinator);
    let mut reconciler = reopen(&h, Box::new(view.clone()));
    // Revision 1: still in its recorded block.
    assert!(matches!(
        reconciler.prepare_step1_reconfirmation(&context()).await,
        Err(Error::NotReady(
            Assessment::ObservationsEligibleForPreflight
        ))
    ));
    view.step1_in(moved(), 6);
    let journal = h.temp.journal();

    // Another reconciler's review at the same revision (1).
    let other = submitted_over(true).await;
    other.view.step1_in(moved(), 6);
    drop(other.coordinator);
    let mut foreign = reopen(&other.h, Box::new(other.view.clone()));
    let foreign_review = foreign
        .prepare_step1_reconfirmation(&context())
        .await
        .unwrap();
    assert!(matches!(
        reconciler
            .confirm_step1_reconfirmation(foreign_review, &context())
            .await,
        Err(Error::InvalidReview)
    ));

    // Superseded by a later review.
    let first = reconciler
        .prepare_step1_reconfirmation(&context())
        .await
        .unwrap();
    let mut second = reconciler
        .prepare_step1_reconfirmation(&context())
        .await
        .unwrap();
    assert!(matches!(
        reconciler
            .confirm_step1_reconfirmation(first, &context())
            .await,
        Err(Error::InvalidReview)
    ));
    // Expired; and used up by the refused acknowledgement.
    second.expire_for_test();
    assert!(matches!(
        reconciler
            .confirm_step1_reconfirmation(second, &context())
            .await,
        Err(Error::ExpiredEvidence)
    ));
    // A new Bitcoin block between review and acknowledgement.
    let review = reconciler
        .prepare_step1_reconfirmation(&context())
        .await
        .unwrap();
    view.step1_in(moved(), 7);
    assert!(matches!(
        reconciler
            .confirm_step1_reconfirmation(review, &context())
            .await,
        Err(Error::ChangedReview)
    ));
    // Re-mined elsewhere again.
    let review = reconciler
        .prepare_step1_reconfirmation(&context())
        .await
        .unwrap();
    let again = BlockRef {
        height: 102,
        hash: hash(10),
    };
    view.step1_in(again, 6);
    assert!(matches!(
        reconciler
            .confirm_step1_reconfirmation(review, &context())
            .await,
        Err(Error::ChangedReview)
    ));
    // S4-D4: past the RDTS margin the acknowledgement is refused.
    view.inner.edit(|view| view.rdts_expiry = MTP + 100);
    assert!(matches!(
        reconciler.prepare_step1_reconfirmation(&context()).await,
        Err(Error::NotReady(Assessment::ExpiryMargin))
    ));
    view.inner.edit(|view| view.rdts_expiry = 20_000);
    assert_eq!(h.temp.journal(), journal, "nothing recorded");

    // A step-2 sighting during the review is recorded and ends the resend.
    view.inner
        .edit(|view| view.on_btcb2 = vec![(txid, TransactionObservation::Unconfirmed { txid })]);
    let review = reconciler
        .prepare_step1_reconfirmation(&context())
        .await
        .unwrap();
    assert_eq!(review.inclusion().confirmed, again);
    let sighted = h.temp.journal();
    assert_eq!(sighted["split"]["step2_observed"], true);
    assert!(sighted["split"].get("step2_returned").is_none());
    reconciler
        .confirm_step1_reconfirmation(review, &context())
        .await
        .unwrap();
    assert_eq!(
        h.temp.journal()["plan"]["previous_confirmation"],
        serde_json::to_value(again).unwrap()
    );
}

/// O2 against O3: step 1 out of every Bitcoin block is told apart by its own
/// read, which the location collapses: waiting in the mempool, or missing.
/// In its recorded block below six it is shallow; at six, eligible. The
/// same from the live coordinator and from a restart's reconciler. A missing
/// step 1 gets the conflict reads (every coin unspent here: still missing);
/// one in the mempool does not.
#[tokio::test(flavor = "multi_thread")]
async fn split_reconcile_reports_step1_absent_versus_in_mempool() {
    let Submitted {
        h,
        view,
        mut coordinator,
        ..
    } = submitted_over(false).await;
    let step1 = h.signed.compute_txid();
    let reconciled = coordinator.reconcile_sweep(&context()).await.unwrap();
    assert_eq!(reconciled.after_step2, Step1AfterStep2::Eligible);
    assert_eq!(
        reconciled.step1,
        TransactionObservation::Confirmed {
            txid: step1,
            block: recorded_block()
        }
    );
    view.step1_in(recorded_block(), 4);
    assert_eq!(
        coordinator
            .reconcile_sweep(&context())
            .await
            .unwrap()
            .after_step2,
        Step1AfterStep2::Shallow { confirmations: 4 }
    );
    view.step1_gone();
    let mut coordinator = Some(coordinator);
    let mut reconciler = None;
    for live in [true, false] {
        if !live {
            // A restart: the coordinator goes, the reconciler reopens.
            drop(coordinator.take());
            reconciler = Some(reopen(&h, Box::new(view.clone())));
        }
        let mut reconcile = async || match (&mut coordinator, &mut reconciler) {
            (Some(coordinator), _) => coordinator.reconcile_sweep(&context()).await.unwrap(),
            (None, Some(reconciler)) => reconciler.reconcile_sweep(&context()).await.unwrap(),
            (None, None) => unreachable!(),
        };
        view.edit(|extra| extra.mempool = true);
        let reads = view.extra.lock().unwrap().unspent_reads;
        let reconciled = reconcile().await;
        assert_eq!(reconciled.after_step2, Step1AfterStep2::InMempool);
        assert_eq!(
            reconciled.step1,
            TransactionObservation::Unconfirmed { txid: step1 }
        );
        assert_eq!(reconciled.status, Status::Observation(Assessment::Reorged));
        assert_eq!(view.extra.lock().unwrap().unspent_reads, reads);

        view.edit(|extra| extra.mempool = false);
        let reconciled = reconcile().await;
        assert_eq!(reconciled.after_step2, Step1AfterStep2::Missing);
        assert_eq!(reconciled.step1, TransactionObservation::Absent);
        assert_eq!(reconciled.status, Status::Observation(Assessment::Reorged));
        assert_eq!(
            view.extra.lock().unwrap().unspent_reads,
            reads + h.prevouts().len()
        );
        assert!(h.temp.journal()["split"].get("step1_conflict").is_none());
    }
}

/// O4: a claimed coin spent on Bitcoin by another transaction while step 1
/// is missing is a conflict, recorded and terminal. A failed, stale or
/// tampered read never records one, nor step 1 waiting in the mempool, nor
/// step 1 seen again between the reads (it spends the coins itself). Once
/// recorded, the conflict stands through step 1 coming back and a restart.
#[tokio::test(flavor = "multi_thread")]
async fn split_step1_conflict_is_terminal_and_never_recorded_from_a_failed_or_stale_read() {
    let Submitted {
        h,
        view,
        coordinator,
        ..
    } = submitted_over(false).await;
    drop(coordinator);
    let mut reconciler = reopen(&h, Box::new(view.clone()));
    let spent = h.prevouts()[1];
    view.step1_gone();
    view.edit(|extra| {
        extra.spent.insert(spent);
    });
    let no_conflict = |case: &str| {
        assert!(
            h.temp.journal()["split"].get("step1_conflict").is_none(),
            "{}",
            case
        );
    };
    let cases: [ReadCase; 5] = [
        (
            "unspent read fails",
            |e| e.unspent_fails = true,
            |e| e.unspent_fails = false,
            Step1AfterStep2::Missing,
        ),
        (
            "stale unspent read",
            |e| e.unspent_age = 3_600,
            |e| e.unspent_age = 0,
            Step1AfterStep2::Missing,
        ),
        (
            "previous transaction unavailable",
            |e| e.previous_fails = true,
            |e| e.previous_fails = false,
            Step1AfterStep2::Missing,
        ),
        (
            "previous transaction is not the prevout's",
            |e| e.previous_tampered = true,
            |e| e.previous_tampered = false,
            Step1AfterStep2::Missing,
        ),
        (
            "step 1 in the mempool spends the coin itself",
            |e| e.mempool = true,
            |e| e.mempool = false,
            Step1AfterStep2::InMempool,
        ),
    ];
    for (case, set, reset, expected) in cases {
        view.edit(set);
        let reconciled = reconciler.reconcile_sweep(&context()).await.unwrap();
        assert_eq!(reconciled.after_step2, expected, "{}", case);
        no_conflict(case);
        view.edit(reset);
    }
    // Step 1 re-broadcast during the reads: the second step-1 read sees it.
    view.edit(|extra| extra.on_unspent = Some(Box::new(|extra| extra.mempool = true)));
    let reconciled = reconciler.reconcile_sweep(&context()).await.unwrap();
    assert_eq!(reconciled.after_step2, Step1AfterStep2::Missing);
    no_conflict("step 1 seen again between the reads");
    view.edit(|extra| extra.mempool = false);

    let conflict = Step1Conflict::new(spent, gone_tip());
    let reconciled = reconciler.reconcile_sweep(&context()).await.unwrap();
    assert_eq!(reconciled.after_step2, Step1AfterStep2::Conflict(conflict));
    assert_eq!(reconciled.step1, TransactionObservation::Absent);
    let journal = h.temp.journal();
    assert_eq!(
        journal["split"]["step1_conflict"],
        serde_json::to_value(conflict).unwrap()
    );
    assert_eq!(journal["version"], 8);

    // Terminal: step 1 back six deep in its recorded block, the coin
    // unspent again, and after a restart.
    view.step1_in(recorded_block(), 6);
    view.edit(|extra| extra.spent.clear());
    let reconciled = reconciler.reconcile_sweep(&context()).await.unwrap();
    assert_eq!(reconciled.after_step2, Step1AfterStep2::Conflict(conflict));
    drop(reconciler);
    let mut reconciler = reopen(&h, Box::new(view.clone()));
    assert_eq!(
        reconciler
            .reconcile_sweep(&context())
            .await
            .unwrap()
            .after_step2,
        Step1AfterStep2::Conflict(conflict)
    );
    assert_eq!(
        h.temp.journal()["split"]["step1_conflict"],
        journal["split"]["step1_conflict"]
    );
}

/// Withholding in O1 to O4. A resend (after a send that came back refused)
/// is reviewable while step 1 is eligible and refused in each outcome,
/// including a recorded conflict with step 1 seen six deep again.
/// Completion (step 2 six deep on BTCB2) is minted while step 1 is
/// eligible and refused in each outcome, including a recorded conflict with
/// step 1 seen six deep again. The descriptors are never forgotten once a
/// conflict is recorded.
#[tokio::test(flavor = "multi_thread")]
async fn split_resend_completion_and_forget_are_refused_in_o1_to_o4() {
    // The outcomes, each set up from step 1 eligible.
    type Setup = fn(&Step1View, OutPoint);
    let outcomes: [(&str, Setup); 4] = [
        ("O1 re-mined", |view, _| view.step1_in(moved(), 6)),
        ("O2 in the mempool", |view, _| {
            view.step1_gone();
            view.edit(|extra| extra.mempool = true);
        }),
        ("O3 missing", |view, _| view.step1_gone()),
        ("O4 conflict", |view, spent| {
            view.step1_gone();
            view.edit(|extra| {
                extra.spent.insert(spent);
            });
        }),
    ];
    let eligible = |view: &Step1View| {
        view.step1_in(recorded_block(), 6);
        view.edit(|extra| {
            extra.mempool = false;
            extra.spent.clear();
        });
    };

    // Resend.
    let Submitted {
        h,
        view,
        mut coordinator,
        sends,
        ..
    } = submitted_over(true).await;
    let spent = h.prevouts()[0];
    assert!(coordinator
        .prepare_step2_resubmission(&context())
        .await
        .is_ok());
    for (case, setup) in outcomes {
        setup(&view, spent);
        let reconciled = coordinator.reconcile_sweep(&context()).await.unwrap();
        assert_ne!(
            reconciled.after_step2,
            Step1AfterStep2::Eligible,
            "{}",
            case
        );
        assert!(
            coordinator
                .prepare_step2_resubmission(&context())
                .await
                .is_err(),
            "{}",
            case
        );
        if case != "O4 conflict" {
            eligible(&view);
        }
    }
    // The conflict is terminal (S4-D2): with step 1 six deep again in its
    // recorded block, the reconcile still reports it and the resend is
    // refused from the journal, before any read.
    eligible(&view);
    assert!(matches!(
        coordinator
            .reconcile_sweep(&context())
            .await
            .unwrap()
            .after_step2,
        Step1AfterStep2::Conflict(_)
    ));
    let reads = view.inner.unspent_reads.load(Ordering::SeqCst);
    assert!(matches!(
        coordinator.prepare_step2_resubmission(&context()).await,
        Err(ResendError::Coordinator(Error::Journal(
            claim_workflow::Error::Conflict
        )))
    ));
    assert_eq!(view.inner.unspent_reads.load(Ordering::SeqCst), reads);
    assert_eq!(h.temp.journal()["split"]["step2_returned"], true);
    assert!(h.temp.journal()["split"]
        .get("step2_resubmissions")
        .is_none());
    assert_eq!(sends.load(Ordering::SeqCst), 1);

    // Completion and forget, on a restart's reconciler.
    let Submitted {
        h,
        view,
        coordinator,
        txid,
        ..
    } = submitted_over(false).await;
    drop(coordinator);
    let spent = h.prevouts()[0];
    view.inner
        .edit(|view| view.on_btcb2 = vec![(txid, step2_confirmed(txid))]);
    let mut reconciler = reopen(&h, Box::new(view.clone()));
    assert!(reconciler
        .check_completion(&context())
        .await
        .unwrap()
        .is_some());
    for (case, setup) in outcomes {
        setup(&view, spent);
        assert!(
            reconciler
                .check_completion(&context())
                .await
                .unwrap()
                .is_none(),
            "{}",
            case
        );
        if case != "O4 conflict" {
            eligible(&view);
        }
    }
    // The conflict is terminal: step 1 six deep again, still refused.
    eligible(&view);
    assert!(
        reconciler
            .check_completion(&context())
            .await
            .unwrap()
            .is_none(),
        "O4 with step 1 back"
    );
    drop(reconciler);
    let identity = claim_workflow::split_identity(TARGET.into(), h.step1.source().digest());
    let mut controller =
        Controller::reopen_settling_blocking(&h.temp.0, &identity, context()).unwrap();
    assert!(matches!(
        controller.forget_split_descriptors(&context()),
        Err(claim_workflow::Error::Conflict)
    ));
    assert!(h.temp.journal()["split"]["descriptors"].is_object());
}

/// A fork-only record (#650) has no step 1, so the step-1 reconfirmation
/// entry points never reach it: since B4b-3a (U2) the step-2 reconciler
/// that carries them refuses to open the record at all, with
/// `InvalidBinding` and before any read (its services panic on any), and
/// records nothing. (The entry points' own kind check stays, unreachable for
/// a fork-only record.)
#[tokio::test]
async fn split_unified_record_refuses_reorg_entry_points_before_any_read() {
    let (sender, _) = watch::channel(7);
    let (temp, digest, _) = super::unified_journal::fork_only_journal(true);
    let journal = temp.journal();
    assert!(matches!(
        super::unified_journal::reconciler(&temp, digest, &sender),
        Err(Error::InvalidBinding)
    ));
    assert_eq!(temp.journal(), journal);
}

/// The step-1 conflict is absent until recorded, so a two-step journal
/// serializes exactly as before at version 8; once recorded it is written,
/// still at version 8, and reads back. The record refuses an unknown field
/// (`deny_unknown_fields`), which is how a binary without `step1_conflict`
/// refuses a journal that has one, and the journal refuses a conflict on a
/// coin the split doesn't claim.
#[tokio::test(flavor = "multi_thread")]
async fn split_journal_without_step1_conflict_serializes_unchanged() {
    let Submitted {
        h,
        view,
        coordinator,
        ..
    } = submitted_over(false).await;
    drop(coordinator);
    let journal = h.temp.journal();
    assert_eq!(journal["version"], 8);
    assert!(journal["split"].get("step1_conflict").is_none());
    let identity = claim_workflow::split_identity(TARGET.into(), h.step1.source().digest());
    let open = || Controller::reopen_settling_blocking(&h.temp.0, &identity, context());
    // A reconcile that finds no conflict writes nothing of it.
    let mut reconciler = reopen(&h, Box::new(view.clone()));
    view.step1_gone();
    reconciler.reconcile_sweep(&context()).await.unwrap();
    assert!(h.temp.journal()["split"].get("step1_conflict").is_none());
    assert_eq!(h.temp.journal()["version"], 8);
    view.edit(|extra| {
        extra.spent.insert(h.prevouts()[0]);
    });
    reconciler.reconcile_sweep(&context()).await.unwrap();
    drop(reconciler);
    let recorded = h.temp.journal();
    assert_eq!(recorded["version"], 8);
    let conflict = Step1Conflict::new(h.prevouts()[0], gone_tip());
    assert_eq!(open().unwrap().split_step1_conflict(), Some(conflict));

    // An unknown field in the record is refused.
    h.temp.rewrite(|intent| {
        intent["split"]["step1_conflict_later"] = intent["split"]["step1_conflict"].clone();
    });
    assert!(open().is_err());
    // A conflict on a coin the split doesn't claim is refused.
    h.temp.rewrite(|intent| {
        intent["split"]
            .as_object_mut()
            .unwrap()
            .remove("step1_conflict_later");
        intent["split"]["step1_conflict"]["outpoint"] =
            serde_json::to_value(OutPoint::new(Txid::from_byte_array([9; 32]), 0)).unwrap();
    });
    assert!(open().is_err());
    // Restored, it reads back.
    h.temp.rewrite(|intent| *intent = recorded.clone());
    assert_eq!(open().unwrap().split_step1_conflict(), Some(conflict));
}
