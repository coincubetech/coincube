//! The Split panel's step-2 stages (#568 B3b-2b-2) over fake step-2 handles:
//! entry from a tracked step 1 at six confirmations (the step-1 driver is
//! released first), the live label, N4, the build gate, signed-file import
//! into the handoff, the review with the route label and privacy note,
//! submission, revocation of every handle, leaving for step 1, and a
//! restart that opens only the reconciler, or, for a resend the journal
//! allows, the coordinator and its one-use resend review (P3-3).
use super::*;
use crate::app::{
    message::Message,
    state::vault::split::{
        Restarted, SplitEvent, SplitMessage, SplitPanel, Stage, Step2Stage, Work,
    },
    view::vault::split::warning_lines,
};
use crate::services::split_psbt_file;
use iced::{futures::StreamExt, Task};
use std::sync::Mutex;

#[derive(Default)]
struct Counts {
    step1_dropped: usize,
    step1_opened: usize,
    revoked: usize,
    coord_dropped: usize,
    submits: usize,
    reconciles: usize,
    finish_failures: usize,
    terminal_finish: Option<Step2Refusal>,
    consume_finish: bool,
    finishes: usize,
    stale_target: bool,
    proofs: usize,
    builds: usize,
    /// The step-1 evidence the next step-2 reconciles return, in order
    /// (`None`: the check fails). Empty: `Status::Unchecked`.
    statuses: std::collections::VecDeque<Option<Status>>,
    /// P3-3: coordinators reopened for a resend, resend reviews and sends.
    reopened: usize,
    resend_reviews: usize,
    resends: usize,
    /// The next resend review refuses with this.
    refuse_resend_review: Option<Step2Refusal>,
    /// What the next resends return, in order. Empty: `Uncertain`.
    resend_results: std::collections::VecDeque<Result<Outcome, Step2Refusal>>,
    /// The shown resend review's evidence lapsed (its deadline, or the
    /// generation moved).
    resend_expired: bool,
    /// What the coordinator's reconciles see on BTCB2. `None`: absent.
    coord_seen: Option<TransactionObservation>,
    /// #568 S4: what the next reconciles report of step 1 after the step-2
    /// submission, in order. Empty: derived from the status.
    afters: std::collections::VecDeque<Step1AfterStep2>,
    /// How long the next resend reviews stay live, by the runtime's clock.
    /// `None`: 60 s.
    resend_lifetime: Option<std::time::Duration>,
    /// What the reconciler's reconciles see on BTCB2. `None`: unconfirmed.
    recon_seen: Option<TransactionObservation>,
    /// #568 B5b: completions run on a reconciler, and what the next return,
    /// in order. Empty: completed at [`COMPLETED_HEIGHT`].
    completes: usize,
    complete_results: std::collections::VecDeque<Result<SplitCompletion, Step2Refusal>>,
    /// D17 rechecks run, and what the next return. Empty: standing.
    rechecks: usize,
    recheck_results: std::collections::VecDeque<Result<CompletionStanding, Step2Refusal>>,
    /// Reconcilers the reconcile-only port opened, and how many
    /// coordinators had been dropped when each opened.
    recon_opened: usize,
    coord_dropped_at_recon_open: Vec<usize>,
}
/// The BTCB2 height the fake completions record.
const COMPLETED_HEIGHT: u64 = 1_000;
fn completed(seen_txid: Txid) -> SplitCompletion {
    SplitCompletion {
        source_digest: sha256::Hash::hash(b"split source"),
        completed_height: COMPLETED_HEIGHT,
        step2_txid: seen_txid,
    }
}
/// What the fakes report of step 1 after the step-2 submission, from the
/// status a test queued (#568 S4): the service classifies it from the
/// collection itself; these tests drive the panel only.
fn after_of(status: Status) -> Step1AfterStep2 {
    match status {
        Status::Observation(Assessment::ObservationsEligibleForPreflight) => {
            Step1AfterStep2::Eligible
        }
        Status::Observation(Assessment::Reorged) => Step1AfterStep2::Missing,
        Status::Observation(Assessment::WaitingForDepth { confirmations }) => {
            Step1AfterStep2::Shallow { confirmations }
        }
        Status::Observation(Assessment::WaitingForConfirmation) => Step1AfterStep2::InMempool,
        _ => Step1AfterStep2::Unknown,
    }
}
fn next_reconcile(
    shared: &Shared,
    seen: TransactionObservation,
) -> Result<(Status, TransactionObservation, Step1AfterStep2), Step2Refusal> {
    let mut counts = shared.lock().unwrap();
    counts.reconciles += 1;
    let after = counts.afters.pop_front();
    match counts.statuses.pop_front() {
        None => Ok((
            Status::Unchecked,
            seen,
            after.unwrap_or(after_of(Status::Unchecked)),
        )),
        Some(Some(status)) => Ok((status, seen, after.unwrap_or(after_of(status)))),
        Some(None) => Err(Step2Refusal {
            reason: "Connect couldn't be reached.".to_string(),
            retry: true,
            recovery: Step2Recovery::None,
        }),
    }
}
type Shared = Arc<Mutex<Counts>>;

async fn events(task: Task<Message>) -> Vec<SplitEvent> {
    let mut out = Vec::new();
    let Some(mut stream) = iced_runtime::task::into_stream(task) else {
        return out;
    };
    while let Some(action) = stream.next().await {
        if let iced_runtime::Action::Output(Message::Split(event)) = action {
            out.push(*event);
        }
    }
    out
}
async fn drive(panel: &mut SplitPanel, task: Task<Message>) {
    let mut pending = vec![task];
    while let Some(task) = pending.pop() {
        for event in events(task).await {
            pending.push(panel.apply(event));
        }
    }
}

struct Step1(Shared);
impl Drop for Step1 {
    fn drop(&mut self) {
        self.0.lock().unwrap().step1_dropped += 1;
    }
}
#[async_trait]
impl Step1Driver for Step1 {
    fn phase(&self) -> Phase {
        Phase::Tracking
    }
    fn revoke_handle(&self) -> RevokeHandle {
        Arc::new(|| {})
    }
    async fn review(&mut self, _: &Context) -> Result<step1::ReviewView, CoordinatorError> {
        unreachable!()
    }
    async fn submit(&mut self, _: &Context) -> Result<Outcome, CoordinatorError> {
        unreachable!()
    }
    async fn reconcile(&mut self, _: &Context) -> Result<Status, CoordinatorError> {
        unreachable!()
    }
    async fn recover(&mut self, _: &Context) -> Result<step1::Recovery, CoordinatorError> {
        unreachable!()
    }
    async fn acknowledge(&mut self, _: &Context) -> Result<(), CoordinatorError> {
        unreachable!()
    }
    async fn resend(&mut self, _: &Context) -> Result<Outcome, CoordinatorError> {
        unreachable!()
    }
}

struct PanelConnect(Shared);
#[async_trait]
impl SplitConnect for PanelConnect {
    fn context(&self) -> Context {
        context()
    }
    fn evidence(&self) -> &dyn SplitEvidenceSource {
        unreachable!()
    }
    async fn window(&self) -> Result<ForkWindow, String> {
        unreachable!()
    }
    async fn bitcoin_feerate(&self) -> Option<u64> {
        unreachable!()
    }
    async fn address_used(&self, _: ChainId, _: &str) -> Result<bool, FailureKind> {
        unreachable!()
    }
    fn open(&self, request: OpenRequest) -> Result<Box<dyn Step1Driver>, CoordinatorError> {
        assert!(request.resume);
        self.0.lock().unwrap().step1_opened += 1;
        Ok(Box::new(Step1(self.0.clone())))
    }
}

struct PanelPrep {
    shared: Shared,
    construction: Arc<SplitStep2>,
    live: Vec<Arc<std::sync::atomic::AtomicU64>>,
    /// The session's generation, alive while the preparation is.
    generation: watch::Sender<u64>,
}
#[async_trait]
impl Step2Prep for PanelPrep {
    fn revoke_handle(&self) -> RevokeHandle {
        let shared = self.shared.clone();
        Arc::new(move || shared.lock().unwrap().revoked += 1)
    }
    fn needs_reservation(&self) -> bool {
        true
    }
    async fn check(&mut self, _: &Context) -> Result<CannotReplay, Step2Refusal> {
        // A new check supersedes the last label, as `check_signing` does.
        for live in &self.live {
            live.store(0, Ordering::Release);
        }
        let (token, live) = ForeignStep2Authorization::for_test(
            &[OutPoint::new(Txid::from_byte_array([1; 32]), 0)],
            Txid::from_byte_array([2; 32]),
            self.generation.subscribe(),
        );
        self.live.push(live);
        Ok(evidence_of(&token))
    }
    async fn ensure_target(&mut self, _: &Context) -> Result<u32, Step2Refusal> {
        let mut counts = self.shared.lock().unwrap();
        counts.stale_target = false;
        counts.proofs += 1;
        Ok(3)
    }
    async fn build(&mut self, _: &Context, coins: Vec<SplitCoin>) -> Result<Psbt, Step2Refusal> {
        assert_eq!(coins.len(), 2, "the restored claimed coins");
        let mut counts = self.shared.lock().unwrap();
        counts.builds += 1;
        if counts.stale_target {
            return Err(describe_step2(Step2Error::TargetNotProven));
        }
        Ok(self.construction.psbt().clone())
    }
    fn verify_signed(&self, signed: &Psbt, coins: &[SplitCoin]) -> Result<bool, Step2Refusal> {
        // Use production cryptography and exact-construction checks, including
        // partial and surplus signatures; a nonempty map is not evidence.
        match finalize_split_step2(
            &self.construction,
            coins,
            self.construction.source(),
            signed,
            &Secp256k1::verification_only(),
        ) {
            Ok(_) => Ok(true),
            Err(coincube_core::foreign_split::FinalizeError::Unsatisfied) => Ok(false),
            Err(error) => Err(Step2Refusal::final_(format!(
                "Invalid signed file: {error}"
            ))),
        }
    }
    fn finish(
        self: Box<Self>,
        _: &Context,
        signed: &Psbt,
        _: &[SplitCoin],
    ) -> Result<Box<dyn Step2Coord>, FinishRefusal> {
        assert!(signed
            .inputs
            .iter()
            .all(|input| !input.partial_sigs.is_empty()));
        let terminal = self.shared.lock().unwrap().terminal_finish.take();
        if let Some(reason) = terminal {
            self.shared.lock().unwrap().finishes += 1;
            if self.shared.lock().unwrap().consume_finish {
                return Err((reason, None));
            }
            return Err((reason, Some(self)));
        }
        let refuse = {
            let mut counts = self.shared.lock().unwrap();
            counts.finishes += 1;
            if counts.finish_failures > 0 {
                counts.finish_failures -= 1;
                true
            } else {
                false
            }
        };
        if refuse {
            return Err((
                Step2Refusal::retry("Handoff unavailable. Try again."),
                Some(self),
            ));
        }
        Ok(Box::new(PanelCoord::new(&self.shared, None)))
    }
}

struct PanelCoord {
    shared: Shared,
    reviewed: bool,
    submitted: Option<Outcome>,
    /// P3-3: a resend review is held for the next confirmation.
    resend_reviewed: bool,
}
impl PanelCoord {
    fn new(shared: &Shared, submitted: Option<Outcome>) -> Self {
        Self {
            shared: shared.clone(),
            reviewed: false,
            submitted,
            resend_reviewed: false,
        }
    }
}
fn uncertain() -> Outcome {
    Outcome::Uncertain {
        txid: Txid::from_byte_array([5; 32]),
        wtxid: coincube_core::miniscript::bitcoin::Wtxid::from_byte_array([6; 32]),
    }
}
impl Drop for PanelCoord {
    fn drop(&mut self) {
        self.shared.lock().unwrap().coord_dropped += 1;
    }
}
fn node_route() -> SubmissionRoute {
    SubmissionRoute::BitcoinNode {
        address: "127.0.0.1:8332".parse().unwrap(),
        identity: crate::services::claim_coordinator::NodeIdentity::for_test(1),
    }
}
#[async_trait]
impl Step2Coord for PanelCoord {
    fn revoke_handle(&self) -> RevokeHandle {
        let shared = self.shared.clone();
        Arc::new(move || shared.lock().unwrap().revoked += 1)
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        self.submitted
    }
    async fn review(&mut self, _: &Context) -> Result<Step2ReviewView, Step2Refusal> {
        self.reviewed = true;
        let (route_label, privacy_note) = route_copy(node_route());
        Ok(Step2ReviewView {
            txid: Txid::from_byte_array([5; 32]),
            fee_sats: 300,
            vsize: 150,
            route: node_route(),
            route_label,
            privacy_note,
        })
    }
    async fn submit(&mut self, _: &Context) -> Result<Outcome, Step2Refusal> {
        assert!(std::mem::take(&mut self.reviewed));
        self.shared.lock().unwrap().submits += 1;
        let outcome = Outcome::UpstreamAccepted {
            txid: Txid::from_byte_array([5; 32]),
            wtxid: coincube_core::miniscript::bitcoin::Wtxid::from_byte_array([6; 32]),
        };
        self.submitted = Some(outcome);
        Ok(outcome)
    }
    async fn reconcile(
        &mut self,
        _: &Context,
    ) -> Result<(Status, TransactionObservation, Step1AfterStep2), Step2Refusal> {
        // A reconcile drops any resend review, as the driver does.
        self.resend_reviewed = false;
        let seen = self.shared.lock().unwrap().coord_seen;
        next_reconcile(&self.shared, seen.unwrap_or(TransactionObservation::Absent))
    }
    async fn review_resend(&mut self, _: &Context) -> Result<Step2ResendView, Step2Refusal> {
        self.resend_reviewed = false;
        let mut counts = self.shared.lock().unwrap();
        counts.resend_reviews += 1;
        counts.resend_expired = false;
        if let Some(refusal) = counts.refuse_resend_review.take() {
            return Err(refusal);
        }
        let attempt = counts.resends + 1;
        let lifetime = counts
            .resend_lifetime
            .unwrap_or(std::time::Duration::from_secs(60));
        drop(counts);
        self.resend_reviewed = true;
        let (route_label, privacy_note) = route_copy(node_route());
        let shared = self.shared.clone();
        Ok(Step2ResendView {
            txid: Txid::from_byte_array([5; 32]),
            route: node_route(),
            route_label,
            privacy_note,
            attempt,
            max_attempts: claim_workflow::MAX_SPLIT_STEP2_RESUBMISSIONS,
            expires_at: chrono::Local::now(),
            not_after: tokio::time::Instant::now().into_std() + lifetime,
            live: Arc::new(move || !shared.lock().unwrap().resend_expired),
        })
    }
    async fn confirm_resend(&mut self, _: &Context) -> Result<Outcome, Step2Refusal> {
        // Only the review on screen is sent, once.
        assert!(
            std::mem::take(&mut self.resend_reviewed),
            "a resend without its review"
        );
        let mut counts = self.shared.lock().unwrap();
        counts.resends += 1;
        counts.resend_results.pop_front().unwrap_or(Ok(uncertain()))
    }
}

struct PanelRecon(Shared);
#[async_trait]
impl Step2Recon for PanelRecon {
    fn revoke_handle(&self) -> RevokeHandle {
        let shared = self.0.clone();
        Arc::new(move || shared.lock().unwrap().revoked += 1)
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        Some(Outcome::Uncertain {
            txid: Txid::from_byte_array([5; 32]),
            wtxid: coincube_core::miniscript::bitcoin::Wtxid::from_byte_array([6; 32]),
        })
    }
    async fn reconcile(
        &mut self,
        _: &Context,
    ) -> Result<(Status, TransactionObservation, Step1AfterStep2), Step2Refusal> {
        let seen = self.0.lock().unwrap().recon_seen;
        next_reconcile(
            &self.0,
            seen.unwrap_or(TransactionObservation::Unconfirmed {
                txid: Txid::from_byte_array([5; 32]),
            }),
        )
    }
    async fn complete(&mut self, _: &Context) -> Result<SplitCompletion, Step2Refusal> {
        let mut counts = self.0.lock().unwrap();
        counts.completes += 1;
        counts
            .complete_results
            .pop_front()
            .unwrap_or(Ok(completed(Txid::from_byte_array([5; 32]))))
    }
    async fn completion_stands(&mut self, _: &Context) -> Result<CompletionStanding, Step2Refusal> {
        let mut counts = self.0.lock().unwrap();
        counts.rechecks += 1;
        counts
            .recheck_results
            .pop_front()
            .unwrap_or(Ok(CompletionStanding::Standing {
                status: Status::Observation(Assessment::ObservationsEligibleForPreflight),
                seen: confirmed_step2(),
            }))
    }
}
/// Step 2 confirmed on BTCB2, as the fakes report it.
fn confirmed_step2() -> TransactionObservation {
    TransactionObservation::Confirmed {
        txid: Txid::from_byte_array([5; 32]),
        block: coincube_core::claim::BlockRef {
            height: COMPLETED_HEIGHT,
            hash: hash(0x44),
        },
    }
}

struct PanelPort {
    shared: Shared,
    construction: Arc<SplitStep2>,
    /// The daemon instance this port stands for.
    daemon: usize,
    account: &'static str,
}
impl PanelPort {
    fn new(shared: &Shared, journal: &Journal, daemon: usize) -> Self {
        Self {
            shared: shared.clone(),
            construction: Arc::new(step2(&journal.wallet, &journal.step1)),
            daemon,
            account: "synthetic-account",
        }
    }
}
#[async_trait]
impl Step2Port for PanelPort {
    fn context(&self) -> Context {
        let mut context = context();
        context.account = self.account.into();
        context
    }
    fn identity(&self) -> PortIdentity {
        PortIdentity {
            context: self.context(),
            daemon: self.daemon,
        }
    }
    fn open_preparation(&self, open: Step2Open) -> Result<Box<dyn Step2Prep>, Step2Refusal> {
        // The step-1 driver was released before this open (#626).
        assert_eq!(self.shared.lock().unwrap().step1_dropped, 1);
        assert_eq!(open.target_cube, TARGET);
        Ok(Box::new(PanelPrep {
            shared: self.shared.clone(),
            construction: self.construction.clone(),
            live: Vec::new(),
            generation: watch::channel(7).0,
        }))
    }
    async fn reopen_for_resend(
        &self,
        _: Arc<dyn SplitConnect>,
        _: PathBuf,
        target_cube: String,
        _: sha256::Hash,
    ) -> Result<Box<dyn Step2Coord>, Step2Refusal> {
        assert_eq!(target_cube, TARGET);
        self.shared.lock().unwrap().reopened += 1;
        Ok(Box::new(PanelCoord::new(&self.shared, Some(uncertain()))))
    }
}

/// The session's reconcile-only port: no daemon behind it.
struct PanelReconPort(Shared);
impl ReconPort for PanelReconPort {
    fn context(&self) -> Context {
        context()
    }
    fn open_reconciler(
        &self,
        _: PathBuf,
        _: String,
        _: sha256::Hash,
    ) -> Result<Box<dyn Step2Recon>, Step2Refusal> {
        let mut counts = self.0.lock().unwrap();
        counts.recon_opened += 1;
        let dropped = counts.coord_dropped;
        counts.coord_dropped_at_recon_open.push(dropped);
        Ok(Box::new(PanelRecon(self.0.clone())))
    }
}

/// A resumed panel tracking step 1 at six confirmations, with a step-2 port.
fn tracked_panel(journal: &Journal) -> (SplitPanel, Shared) {
    let shared: Shared = Arc::default();
    let mut panel = SplitPanel::resume(
        TARGET.into(),
        journal.temp.0.parent().unwrap().to_path_buf(),
        journal.digest(),
        journal.temp.0.clone(),
    );
    panel.connect = Some(Arc::new(PanelConnect(shared.clone())));
    panel.step2_port = Some(Arc::new(PanelPort::new(&shared, journal, 1)));
    panel.stage = Stage::Tracking;
    panel.phase = Some(Phase::Tracking);
    panel.status = Some(Status::Observation(
        Assessment::ObservationsEligibleForPreflight,
    ));
    panel.construction = Some(Box::new(journal.step1.clone()));
    panel.signed = Some(sign1(&journal.step1, &journal.wallet).transaction().clone());
    panel.driver = Some(Box::new(Step1(shared.clone())));
    panel.coins = coins(&journal.wallet);
    (panel, shared)
}

/// Step 2 end to end through the panel: entry releases the step-1 driver
/// first; a check shows the live label; build waits for a reserved target
/// and a live label; N4 is the reserving stage; a partial import keeps the
/// preparation and a complete one hands over; the review carries the
/// node-route label and privacy note; submission once; revocation drops and
/// revokes every handle and the label.
#[tokio::test(flavor = "multi_thread")]
async fn panel_runs_step2_from_a_tracked_step1() {
    let journal = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&journal);
    // Not before six confirmations.
    panel.status = Some(Status::Observation(Assessment::WaitingForDepth {
        confirmations: 5,
    }));
    assert!(!panel.can_enter_step2());
    let task = panel.update(SplitMessage::EnterStep2);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Tracking);
    panel.status = Some(Status::Observation(
        Assessment::ObservationsEligibleForPreflight,
    ));
    assert!(panel.can_enter_step2());

    let task = panel.update(SplitMessage::EnterStep2);
    assert_eq!(panel.stage, Stage::Working(Work::Entering));
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Ready));
    assert_eq!(shared.lock().unwrap().step1_dropped, 1);
    assert!(panel.driver.is_none());

    // Build is gated on a reserved target and a live label: neither, then a
    // target without a label, builds nothing (#637 F3).
    let task = panel.update(SplitMessage::Step2Build);
    drive(&mut panel, task).await;
    assert!(panel.step2_psbt().is_none());
    let task = panel.update(SplitMessage::Step2Reserve);
    assert_eq!(panel.stage, Stage::Working(Work::Reserving), "N4");
    drive(&mut panel, task).await;
    assert_eq!(panel.target_index(), Some(3));
    let task = panel.update(SplitMessage::Step2Build);
    drive(&mut panel, task).await;
    assert!(panel.step2_psbt().is_none(), "no live label");
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Ready));
    let task = panel.update(SplitMessage::Step2Check);
    drive(&mut panel, task).await;
    assert_eq!(panel.replay_label(), Some(CANNOT_REPLAY));
    // A second check supersedes the first label.
    let earlier = panel.replay.clone().unwrap();
    let task = panel.update(SplitMessage::Step2Check);
    drive(&mut panel, task).await;
    assert_eq!(earlier.label(), None);
    assert_eq!(panel.replay_label(), Some(CANNOT_REPLAY));
    let task = panel.update(SplitMessage::Step2Build);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Sign));
    assert_eq!(panel.replay_label(), None, "the build redeemed the check");

    // A partial file keeps the preparation; a signed one hands over.
    let dir = journal.temp.0.parent().unwrap().to_path_buf();
    let unsigned = dir.join("unsigned.txt");
    std::fs::write(
        &unsigned,
        split_psbt_file::encode(
            panel.step2_psbt().unwrap(),
            split_psbt_file::Encoding::Base64,
        ),
    )
    .unwrap();
    let task = panel.step2_import_from(vec![unsigned]);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Sign));
    assert!(panel.prep.is_some());
    assert!(panel.notice().unwrap().contains("No new signatures"));
    let mut signed = panel.step2_psbt().unwrap().clone();
    signed
        .sign(&journal.wallet.signer, &Secp256k1::new())
        .unwrap();
    let signed_path = dir.join("signed.txt");
    std::fs::write(
        &signed_path,
        split_psbt_file::encode(&signed, split_psbt_file::Encoding::Base64),
    )
    .unwrap();
    let task = panel.step2_import_from(vec![signed_path]);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Signed));
    assert!(panel.prep.is_none() && panel.coord.is_some());

    let task = panel.update(SplitMessage::Step2Review);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Review));
    let review = panel.step2_review().unwrap();
    assert_eq!(review.route_label, "Your Bitcoin node");
    assert_eq!(review.privacy_note, Some(NODE_PRIVACY));
    let task = panel.update(SplitMessage::Step2Confirm);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Submitted));
    assert!(matches!(
        panel.step2_outcome(),
        Some(Outcome::UpstreamAccepted { .. })
    ));
    assert_eq!(shared.lock().unwrap().submits, 1);
    // Submitted: only reconcile.
    let task = panel.update(SplitMessage::Step2Confirm);
    drive(&mut panel, task).await;
    assert_eq!(shared.lock().unwrap().submits, 1);
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_eq!(panel.step2_seen(), Some(TransactionObservation::Absent));

    // Logout, Cube close or a backend switch (`App::revoke_claim`).
    let revoked = shared.lock().unwrap().revoked;
    panel.revoke();
    assert_eq!(shared.lock().unwrap().revoked, revoked + 1);
    assert_eq!(shared.lock().unwrap().coord_dropped, 1);
    assert!(panel.coord.is_none() && panel.prep.is_none() && panel.recon.is_none());
    assert_eq!(panel.replay_label(), None);
    assert_eq!(panel.stage, Stage::NeedsSession);
}

/// Leaving for step 1 (a reorg review) releases the preparation and rebinds
/// the step-1 driver; a revocation also clears a live label; another daemon's
/// port revokes the step-2 handles.
#[tokio::test(flavor = "multi_thread")]
async fn panel_leaves_step2_and_revokes_on_a_port_change() {
    let journal = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&journal);
    let task = panel.update(SplitMessage::EnterStep2);
    drive(&mut panel, task).await;
    let task = panel.update(SplitMessage::Step2Check);
    drive(&mut panel, task).await;
    assert!(panel.replay_label().is_some());
    let task = panel.update(SplitMessage::LeaveStep2);
    assert_eq!(panel.stage, Stage::Working(Work::Leaving));
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Tracking);
    assert!(panel.driver.is_some() && panel.prep.is_none());
    assert_eq!(shared.lock().unwrap().step1_opened, 1);
    assert_eq!(panel.replay_label(), None);

    // Back in. The App builds a new port on every Connect refresh: an
    // equivalent one (same session, same daemon) keeps the flow (#637 F1).
    shared.lock().unwrap().step1_dropped = 0;
    let task = panel.update(SplitMessage::EnterStep2);
    drive(&mut panel, task).await;
    let task = panel.update(SplitMessage::Step2Check);
    drive(&mut panel, task).await;
    let revoked = shared.lock().unwrap().revoked;
    for _ in 0..3 {
        panel.set_step2_port(Some(Arc::new(PanelPort::new(&shared, &journal, 1))));
    }
    assert_eq!(shared.lock().unwrap().revoked, revoked);
    assert!(panel.prep.is_some());
    assert_eq!(panel.replay_label(), Some(CANNOT_REPLAY));
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Ready));

    // Another account's session revokes every step-2 handle.
    let mut other_account = PanelPort::new(&shared, &journal, 1);
    other_account.account = "other-account";
    panel.set_step2_port(Some(Arc::new(other_account)));
    assert_eq!(shared.lock().unwrap().revoked, revoked + 1);
    assert!(panel.prep.is_none());
    assert_eq!(panel.replay_label(), None);
    assert_eq!(panel.stage, Stage::NeedsSession);
}

/// A restart after a recorded step-2 submission opens only the reconciler:
/// the recorded outcome is shown as recorded, and the only action is to
/// reconcile. #637 R1: that holds with no step-2 port at all (the Vault's
/// daemon unloaded, restarting, external or on a route step 2 can't be sent
/// through) as with one; the restart never rebuilds step 1.
#[tokio::test(flavor = "multi_thread")]
async fn panel_restart_after_a_recorded_step2_only_reconciles() {
    let journal = Journal::new(true);
    for daemon in [false, true] {
        let shared: Shared = Arc::default();
        let mut panel = SplitPanel::resume(
            TARGET.into(),
            journal.temp.0.parent().unwrap().to_path_buf(),
            journal.digest(),
            journal.temp.0.clone(),
        );
        panel.set_connect(Some(Arc::new(PanelConnect(shared.clone()))));
        if daemon {
            panel.set_step2_port(Some(Arc::new(PanelPort::new(&shared, &journal, 1))));
        }
        panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
        let task = panel.begin();
        assert_eq!(panel.stage, Stage::Working(Work::Restarting), "{}", daemon);
        drive(&mut panel, task).await;
        assert_eq!(
            panel.stage,
            Stage::Step2(Step2Stage::Reconcile),
            "{}",
            daemon
        );
        assert!(matches!(
            panel.step2_outcome(),
            Some(Outcome::Uncertain { .. })
        ));
        assert_eq!(shared.lock().unwrap().step1_opened, 0, "{}", daemon);
        for message in [
            SplitMessage::Step2Confirm,
            SplitMessage::Step2Review,
            SplitMessage::EnterStep2,
            SplitMessage::Retry,
        ] {
            let task = panel.update(message);
            drive(&mut panel, task).await;
            assert_eq!(
                panel.stage,
                Stage::Step2(Step2Stage::Reconcile),
                "{}",
                daemon
            );
        }
        let task = panel.update(SplitMessage::Step2Reconcile);
        drive(&mut panel, task).await;
        assert!(matches!(
            panel.step2_seen(),
            Some(TransactionObservation::Unconfirmed { .. })
        ));
        let counts = shared.lock().unwrap();
        assert_eq!(counts.reconciles, 1, "{}", daemon);
        assert_eq!((counts.step1_opened, counts.submits), (0, 0), "{}", daemon);
    }
}

/// #637 R1: a recorded step 2 with a session but no reconciler for it fails
/// closed, retryably: nothing reopens step 1 or opens a preparation, and a
/// retry stays refused until the session's reconcile-only port arrives,
/// which then reconciles. Without a session it waits for one.
#[tokio::test(flavor = "multi_thread")]
async fn panel_restart_without_a_reconciler_refuses_instead_of_step1() {
    let journal = Journal::new(true);
    let shared: Shared = Arc::default();
    let mut panel = SplitPanel::resume(
        TARGET.into(),
        journal.temp.0.parent().unwrap().to_path_buf(),
        journal.digest(),
        journal.temp.0.clone(),
    );
    panel.set_step2_port(Some(Arc::new(PanelPort::new(&shared, &journal, 1))));
    panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
    let task = panel.begin();
    assert_eq!(panel.stage, Stage::NeedsSession);
    drive(&mut panel, task).await;
    assert_eq!(
        panel.stage,
        Stage::NeedsSession,
        "no session, nothing opened"
    );

    panel.set_recon_port(None);
    panel.set_connect(Some(Arc::new(PanelConnect(shared.clone()))));
    for _ in 0..2 {
        let task = match panel.stage {
            Stage::NeedsSession => panel.begin(),
            _ => panel.update(SplitMessage::Retry),
        };
        drive(&mut panel, task).await;
        match &panel.stage {
            Stage::Refused(refusal) => {
                assert!(refusal.retry);
                assert_eq!(refusal.reason, RECONCILE_UNAVAILABLE);
            }
            other => panic!("{:?}", other),
        }
        assert_eq!(shared.lock().unwrap().step1_opened, 0);
        assert!(panel.driver.is_none() && panel.prep.is_none() && panel.recon.is_none());
    }

    // The session's port arrives (the App's next refresh): reconcile only.
    panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
    let task = panel.update(SplitMessage::Retry);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert_eq!(shared.lock().unwrap().step1_opened, 0);
}

/// #637 R1 (step 1 unchanged): a journal with no recorded step 2 restarts
/// into its step-1 resume whether or not there is a step-2 or a
/// reconcile-only port.
#[tokio::test(flavor = "multi_thread")]
async fn panel_restart_of_a_step1_journal_resumes_step1_without_a_daemon() {
    let journal = Journal::new(false);
    for (step2_port, recon_port) in [(false, false), (false, true), (true, true)] {
        let shared: Shared = Arc::default();
        let mut panel = SplitPanel::resume(
            TARGET.into(),
            journal.temp.0.parent().unwrap().to_path_buf(),
            journal.digest(),
            journal.temp.0.clone(),
        );
        panel.set_connect(Some(Arc::new(PanelConnect(shared.clone()))));
        if step2_port {
            panel.set_step2_port(Some(Arc::new(PanelPort::new(&shared, &journal, 1))));
        }
        if recon_port {
            panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
        }
        let task = panel.begin();
        assert_eq!(panel.stage, Stage::Working(Work::Restarting));
        let mut out = events(task).await;
        assert_eq!(out.len(), 1);
        let restarted = out.remove(0);
        assert!(
            matches!(restarted, SplitEvent::Restarted(_, Ok(Restarted::Step1))),
            "{:?}",
            restarted
        );
        // The step-1 resume (rebuilt from fresh chain evidence, which this
        // fake has none of) is what runs next.
        let _resume = panel.apply(restarted);
        assert_eq!(
            panel.stage,
            Stage::Working(Work::Restoring),
            "{} {}",
            step2_port,
            recon_port
        );
        assert!(panel.recon.is_none());
        assert_eq!(shared.lock().unwrap().reconciles, 0);
    }
}

/// #637 R1 through the production ports: a panel resumed on a real journal
/// whose step 2 is recorded, under the session's production Connect and
/// reconcile-only ports and with no Vault daemon at all, opens the
/// production reconciler over that journal (which then holds its lock) and
/// never step 1. Revoking it releases the journal.
#[tokio::test(flavor = "multi_thread")]
async fn panel_restart_reconciles_through_the_production_ports_without_a_daemon() {
    let (_sender, generation) = watch::channel(7);
    let recon = ProductionRecon::new(session(ORIGIN), generation.clone(), None).unwrap();
    let connect = step1::ProductionConnect::new(session(ORIGIN), generation.clone()).unwrap();
    assert_eq!(ReconPort::context(&recon), SplitConnect::context(&connect));
    let journal = Journal::under(true, &ReconPort::context(&recon));
    let mut panel = SplitPanel::resume(
        TARGET.into(),
        journal.temp.0.parent().unwrap().to_path_buf(),
        journal.digest(),
        journal.temp.0.clone(),
    );
    panel.set_connect(Some(Arc::new(connect)));
    panel.set_recon_port(Some(Arc::new(recon)));
    assert!(!panel.step2_available());
    let task = panel.begin();
    assert_eq!(panel.stage, Stage::Working(Work::Restarting));
    drive(&mut panel, task).await;
    assert_eq!(
        panel.stage,
        Stage::Step2(Step2Stage::Reconcile),
        "{:?}",
        panel.notice()
    );
    assert!(panel.recon.is_some() && panel.driver.is_none() && panel.prep.is_none());
    assert!(panel.construction().is_none(), "step 1 was not rebuilt");
    assert!(matches!(
        panel.step2_outcome(),
        Some(Outcome::Uncertain { .. })
    ));
    let reopen = || {
        Controller::reopen(
            &journal.temp.0,
            &claim_workflow::split_identity(TARGET.into(), journal.digest()),
            ReconPort::context(
                &ProductionRecon::new(session(ORIGIN), generation.clone(), None).unwrap(),
            ),
        )
    };
    assert!(matches!(reopen(), Err(claim_workflow::Error::Busy)));
    panel.revoke();
    assert!(panel.recon.is_none());
    assert!(reopen().is_ok());
}

/// #637 R1: the App installs the session's reconcile-only port on every
/// Connect refresh. An equivalent one (same session context) keeps a bound
/// reconciler; another session's, or none, revokes it.
#[tokio::test(flavor = "multi_thread")]
async fn panel_keeps_its_reconciler_across_an_equivalent_recon_port() {
    struct OtherAccount;
    impl ReconPort for OtherAccount {
        fn context(&self) -> Context {
            let mut context = context();
            context.account = "other-account".into();
            context
        }
        fn open_reconciler(
            &self,
            _: PathBuf,
            _: String,
            _: sha256::Hash,
        ) -> Result<Box<dyn Step2Recon>, Step2Refusal> {
            unreachable!("never opened in this test")
        }
    }
    let journal = Journal::new(true);
    let shared: Shared = Arc::default();
    let mut panel = SplitPanel::resume(
        TARGET.into(),
        journal.temp.0.parent().unwrap().to_path_buf(),
        journal.digest(),
        journal.temp.0.clone(),
    );
    panel.set_connect(Some(Arc::new(PanelConnect(shared.clone()))));
    panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    for _ in 0..3 {
        panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
    }
    assert!(panel.recon.is_some());
    assert_eq!(shared.lock().unwrap().revoked, 0);
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));

    panel.set_recon_port(Some(Arc::new(OtherAccount)));
    assert_eq!(shared.lock().unwrap().revoked, 1);
    assert!(panel.recon.is_none());
    assert_eq!(panel.stage, Stage::NeedsSession);

    panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    panel.set_recon_port(None);
    assert_eq!(shared.lock().unwrap().revoked, 2);
    assert!(panel.recon.is_none());
    assert_eq!(panel.stage, Stage::NeedsSession);
}

/// Rebind a tracked step 1 at six confirmations, as resuming the journal
/// under the next session does (the panel's own resume needs chain evidence
/// this fake has none of). The next entry drops this driver.
fn rebind_step1(panel: &mut SplitPanel, shared: &Shared) {
    panel.stage = Stage::Tracking;
    panel.status = Some(Status::Observation(
        Assessment::ObservationsEligibleForPreflight,
    ));
    panel.driver = Some(Box::new(Step1(shared.clone())));
    shared.lock().unwrap().step1_dropped = 0;
}

/// #637 r4172150937: a target proof belongs to the preparation that made it.
/// A revocation drops it with the preparation, and a new preparation starts
/// with none (also after one a refused handoff released without a
/// revocation), so Build comes back only once the target is reserved and
/// proven again, never after a check alone.
#[tokio::test(flavor = "multi_thread")]
async fn panel_needs_a_new_target_proof_for_each_preparation() {
    let journal = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&journal);
    let task = panel.update(SplitMessage::EnterStep2);
    drive(&mut panel, task).await;
    let task = panel.update(SplitMessage::Step2Reserve);
    drive(&mut panel, task).await;
    assert_eq!(panel.target_index(), Some(3));

    // Logout, Cube close or a backend switch.
    panel.revoke();
    assert_eq!(panel.target_index(), None);

    // A new preparation under the next session. An index left from any
    // earlier preparation does not carry over into it.
    rebind_step1(&mut panel, &shared);
    panel.target_index = Some(9);
    let task = panel.update(SplitMessage::EnterStep2);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Ready));
    assert_eq!(panel.target_index(), None);
    let task = panel.update(SplitMessage::Step2Check);
    drive(&mut panel, task).await;
    assert!(panel.replay_label().is_some());
    let task = panel.update(SplitMessage::Step2Build);
    drive(&mut panel, task).await;
    assert!(panel.step2_psbt().is_none(), "built on an unproven target");
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Ready));

    // Reserved and proven in this preparation: it builds.
    let task = panel.update(SplitMessage::Step2Reserve);
    drive(&mut panel, task).await;
    assert_eq!(panel.target_index(), Some(3));
    let task = panel.update(SplitMessage::Step2Build);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Sign));
}

/// #637 r4172150954: a new build forgets the earlier export, so the Sign
/// stage never names a file that holds an earlier PSBT.
#[tokio::test(flavor = "multi_thread")]
async fn panel_forgets_an_earlier_export_on_a_new_build() {
    async fn build(panel: &mut SplitPanel) {
        for message in [
            SplitMessage::EnterStep2,
            SplitMessage::Step2Reserve,
            SplitMessage::Step2Check,
            SplitMessage::Step2Build,
        ] {
            let task = panel.update(message);
            drive(panel, task).await;
        }
        assert_eq!(panel.stage, Stage::Step2(Step2Stage::Sign));
    }
    let journal = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&journal);
    build(&mut panel).await;
    let exported = journal.temp.0.parent().unwrap().join("first.txt");
    let task = panel.step2_export_to(exported.clone(), split_psbt_file::Encoding::Base64);
    drive(&mut panel, task).await;
    assert_eq!(panel.step2_exported(), Some(&exported));

    // Back to step 1 (a reorg review), then a new preparation and build.
    let task = panel.update(SplitMessage::LeaveStep2);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Tracking);
    shared.lock().unwrap().step1_dropped = 0;
    build(&mut panel).await;
    assert_eq!(panel.step2_exported(), None);
}

/// #637 r4174164844: a step-2 import holds at most `MAX_COMBINED_FILES`
/// files in all, as step 1's does. More, in one selection or added to the
/// files already loaded, is refused before any file is read, anything is
/// cloned or the preparation is taken: the loaded files, the preparation and
/// the Sign stage stay usable, and exactly the cap still imports.
#[tokio::test(flavor = "multi_thread")]
async fn panel_caps_step2_imports_before_reading_any_file() {
    use split_psbt_file::MAX_COMBINED_FILES;
    /// Refused by the cap: nothing ran and nothing changed but the notice.
    async fn refused(panel: &mut SplitPanel, paths: Vec<PathBuf>) {
        let (seq, loaded) = (panel.seq, panel.step2_files());
        let task = panel.step2_import_from(paths);
        assert!(events(task).await.is_empty(), "a file was read");
        assert_eq!(panel.seq, seq);
        assert_eq!(
            panel.notice(),
            Some(
                split_psbt_file::FileError::TooManyFiles
                    .to_string()
                    .as_str()
            )
        );
        assert_eq!(panel.step2_files(), loaded);
        assert!(panel.prep.is_some());
        assert_eq!(panel.stage, Stage::Step2(Step2Stage::Sign));
    }
    let journal = Journal::new(false);
    let (mut panel, _shared) = tracked_panel(&journal);
    for message in [
        SplitMessage::EnterStep2,
        SplitMessage::Step2Reserve,
        SplitMessage::Step2Check,
        SplitMessage::Step2Build,
    ] {
        let task = panel.update(message);
        drive(&mut panel, task).await;
    }
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Sign));
    let dir = journal.temp.0.parent().unwrap().to_path_buf();
    // Files that don't exist: reading any would refuse with a read error.
    let missing = |n: usize| {
        (0..n)
            .map(|i| dir.join(format!("missing-{}.txt", i)))
            .collect::<Vec<_>>()
    };

    // One selection over the cap.
    refused(&mut panel, missing(MAX_COMBINED_FILES + 1)).await;
    assert_eq!(panel.step2_files(), 0);

    // A retained list from the old importer may include duplicates.
    // Keep the pre-read cap counting every retained file even in that case.
    let mut signed = panel.step2_psbt().unwrap().clone();
    signed
        .sign(&journal.wallet.signer, &Secp256k1::new())
        .unwrap();
    let mut partial = signed.clone();
    partial.inputs[1].partial_sigs.clear();
    panel.step2_files = vec![partial; MAX_COMBINED_FILES - 1];
    refused(&mut panel, missing(2)).await;
    let path = dir.join("signed.txt");
    std::fs::write(
        &path,
        split_psbt_file::encode(&signed, split_psbt_file::Encoding::Base64),
    )
    .unwrap();
    _shared.lock().unwrap().finish_failures = 1;
    let task = panel.step2_import_from(vec![path]);
    drive(&mut panel, task).await;
    assert_eq!(panel.step2_files(), MAX_COMBINED_FILES);
    assert!(panel.can_retry_step2_handoff());
    refused(&mut panel, missing(1)).await;
    // Retry is independent of the file cap and does not submit anything.
    let task = panel.update(SplitMessage::Step2RetryHandoff);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Signed));
    assert_eq!(panel.step2_files(), MAX_COMBINED_FILES);
    assert_eq!(_shared.lock().unwrap().submits, 0);
    assert_eq!(_shared.lock().unwrap().finishes, 2);
}

/// Every message other than reconcile, none of which may act after a
/// step-2 submission: no step-1 signing, rebuild, reset or resend. A step-2
/// resend review is offered only from the Reconcile stage with a reopened
/// coordinator (P3-3), so neither the live coordinator nor the reconciler
/// acts on one.
const AFTER_SUBMISSION: [SplitMessage; 15] = [
    SplitMessage::Step2RetryHandoff,
    SplitMessage::Step2Confirm,
    SplitMessage::Step2Review,
    SplitMessage::Step2Check,
    SplitMessage::Step2Reserve,
    SplitMessage::Step2Build,
    SplitMessage::EnterStep2,
    SplitMessage::LeaveStep2,
    SplitMessage::Retry,
    SplitMessage::Reconcile,
    SplitMessage::CheckReorg,
    SplitMessage::ConfirmResend,
    SplitMessage::ConfirmAbandon,
    SplitMessage::Step2ReviewResend,
    SplitMessage::Step2ConfirmResend,
];

/// #637 r4172242637, live coordinator: each reconcile after the step-2
/// submission keeps its step-1 evidence beside the BTCB2 observation. A
/// reorg of step 1 warns that Bitcoin replay protection is no longer
/// established and drops a stale "cannot replay" label. Fewer confirmations
/// or an unreadable check warn without calling it a reorg. A failed check
/// keeps the last evidence and its warning. A later eligible check clears
/// the warning. Nothing but reconcile acts.
#[tokio::test(flavor = "multi_thread")]
async fn panel_warns_when_step1_loses_bitcoin_confirmation_after_step2() {
    use crate::app::state::vault::split::step2::STEP1_REORGED_AFTER_STEP2;
    let journal = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&journal);
    let outcome = Outcome::UpstreamAccepted {
        txid: Txid::from_byte_array([5; 32]),
        wtxid: coincube_core::miniscript::bitcoin::Wtxid::from_byte_array([6; 32]),
    };
    panel.driver = None;
    panel.coord = Some(Box::new(PanelCoord::new(&shared, Some(outcome))));
    panel.step2_outcome = Some(outcome);
    panel.stage = Stage::Step2(Step2Stage::Submitted);
    // A label left from a check before the submission.
    let (_sender, generation) = watch::channel(7);
    let (token, _latest) = ForeignStep2Authorization::for_test(
        &[OutPoint::new(Txid::from_byte_array([1; 32]), 0)],
        Txid::from_byte_array([2; 32]),
        generation,
    );
    panel.replay = Some(evidence_of(&token));
    assert!(panel.replay_label().is_some());
    shared.lock().unwrap().statuses.extend([
        Some(Status::Observation(Assessment::Reorged)),
        None,
        Some(Status::Observation(Assessment::WaitingForDepth {
            confirmations: 3,
        })),
        Some(Status::Unavailable),
        Some(Status::Observation(
            Assessment::ObservationsEligibleForPreflight,
        )),
    ]);
    let reconcile = |panel: &mut SplitPanel| panel.update(SplitMessage::Step2Reconcile);

    let task = reconcile(&mut panel);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Submitted));
    assert_eq!(
        panel.step2_status(),
        Some(Status::Observation(Assessment::Reorged))
    );
    assert_eq!(
        panel.step2_warning().as_deref(),
        Some(STEP1_REORGED_AFTER_STEP2)
    );
    assert_eq!(panel.notice(), None);
    assert_eq!(warning_lines(&panel), [STEP1_REORGED_AFTER_STEP2]);
    assert_eq!(panel.step2_seen(), Some(TransactionObservation::Absent));
    assert_eq!(panel.replay_label(), None);
    for message in AFTER_SUBMISSION {
        let task = panel.update(message);
        drive(&mut panel, task).await;
        assert_eq!(panel.stage, Stage::Step2(Step2Stage::Submitted));
    }
    {
        let counts = shared.lock().unwrap();
        assert_eq!((counts.submits, counts.step1_opened), (0, 0));
        assert_eq!(counts.reconciles, 1);
    }
    assert!(panel.prep.is_none() && panel.driver.is_none() && panel.coord.is_some());

    // A failed check keeps the evidence and its warning; the failure has
    // its own line.
    let task = reconcile(&mut panel);
    drive(&mut panel, task).await;
    assert_eq!(
        panel.step2_status(),
        Some(Status::Observation(Assessment::Reorged))
    );
    assert_eq!(
        warning_lines(&panel),
        [STEP1_REORGED_AFTER_STEP2, "Connect couldn't be reached."]
    );

    // Fewer confirmations, or no fresh evidence: warned, not a reorg.
    let task = reconcile(&mut panel);
    drive(&mut panel, task).await;
    assert_eq!(panel.notice(), None);
    let warning = panel.step2_warning().unwrap();
    assert!(
        warning.contains("3 of 6 Bitcoin confirmations"),
        "{}",
        warning
    );
    assert!(warning.contains("replay protection"), "{}", warning);
    assert!(!warning.contains("reorganized"), "{}", warning);
    let task = reconcile(&mut panel);
    drive(&mut panel, task).await;
    let warning = panel.step2_warning().unwrap();
    assert!(warning.contains("not a sign of a reorg"), "{}", warning);
    assert_eq!(panel.step2_status(), Some(Status::Unavailable));

    // Eligible again: no warning; the observation is still shown.
    let task = reconcile(&mut panel);
    drive(&mut panel, task).await;
    assert_eq!(panel.step2_warning(), None);
    assert!(warning_lines(&panel).is_empty());
    assert_eq!(
        panel.step2_status(),
        Some(Status::Observation(
            Assessment::ObservationsEligibleForPreflight
        ))
    );
    assert_eq!(panel.step2_seen(), Some(TransactionObservation::Absent));
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Submitted));
}

/// #568 S4: the reconciler's warning has one case per outcome of step 1
/// after the step-2 submission. Each names the exposure (step 2's recorded
/// bytes could be mined on Bitcoin; for a conflict, step 1 can never confirm
/// and the split can't complete), names a reorg only for O1 to O4, and
/// leaves checking status again as the only action: no acknowledgement,
/// close or step-1 resend is offered or named, nor the "cannot replay"
/// label. Eligible evidence clears it.
#[tokio::test(flavor = "multi_thread")]
async fn step2_reconcile_warning_has_one_case_per_outcome_and_never_the_replay_label() {
    use crate::services::claim_workflow::Step1Conflict;
    use coincube_core::claim::BlockRef;
    let block = |height: u64, n: u8| BlockRef {
        height,
        hash: coincube_core::miniscript::bitcoin::BlockHash::from_byte_array([n; 32]),
    };
    let spent = OutPoint::new(Txid::from_byte_array([7; 32]), 1);
    let outcomes = [
        (Step1AfterStep2::Shallow { confirmations: 3 }, false),
        (
            Step1AfterStep2::Remined {
                previous: block(100, 6),
                confirmed: block(101, 9),
            },
            true,
        ),
        (Step1AfterStep2::InMempool, true),
        (Step1AfterStep2::Missing, true),
        (
            Step1AfterStep2::Conflict(Step1Conflict::new(spent, block(120, 4))),
            true,
        ),
        (
            Step1AfterStep2::Conflict(
                Step1Conflict::new(spent, block(120, 4))
                    .terminal(block(126, 5))
                    .unwrap(),
            ),
            true,
        ),
        (Step1AfterStep2::Unknown, false),
    ];
    let journal = Journal::new(true);
    let shared: Shared = Arc::default();
    let mut panel = SplitPanel::resume(
        TARGET.into(),
        journal.temp.0.parent().unwrap().to_path_buf(),
        journal.digest(),
        journal.temp.0.clone(),
    );
    panel.set_connect(Some(Arc::new(PanelConnect(shared.clone()))));
    panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    let mut seen = std::collections::BTreeSet::new();
    for (after, reorg) in outcomes {
        shared.lock().unwrap().afters.push_back(after);
        let task = panel.update(SplitMessage::Step2Reconcile);
        drive(&mut panel, task).await;
        assert_eq!(panel.step2_after(), Some(after));
        let warning = panel.step2_warning().expect("a warning");
        assert_eq!(warning_lines(&panel), [warning.as_str()]);
        assert!(seen.insert(warning.clone()), "one case each: {:?}", after);
        assert!(
            warning.ends_with("Check status again later."),
            "{}",
            warning
        );
        if let Step1AfterStep2::Conflict(conflict) = after {
            assert!(warning.contains(&conflict.outpoint().to_string()));
        }
        if matches!(after, Step1AfterStep2::Conflict(c) if c.is_terminal()) {
            assert!(
                warning.contains("step 1 can never confirm and this split can't complete"),
                "{}",
                warning
            );
        } else {
            assert!(
                warning.contains("step 2's recorded bytes could also be mined on Bitcoin"),
                "{}",
                warning
            );
            assert!(!warning.contains("can never confirm"), "{}", warning);
        }
        if matches!(after, Step1AfterStep2::Conflict(c) if !c.is_terminal()) {
            assert!(
                warning.contains(
                    "appears spent on Bitcoin by another transaction (it may still be unconfirmed)"
                ),
                "{}",
                warning
            );
        }
        assert_eq!(warning.contains("reorganized"), reorg, "{}", warning);
        if let Step1AfterStep2::Remined { .. } = after {
            assert!(warning.contains("(height 101)") && warning.contains("(height 100)"));
        }
        for never in [
            "cknowledg",
            "lose",
            "bandon",
            "resend",
            "send step 1",
            "sent again",
            CANNOT_REPLAY,
        ] {
            assert!(!warning.contains(never), "{}: {}", never, warning);
        }
        assert_eq!(panel.replay_label(), None);
        assert!(!panel.can_check_close() && !panel.can_review_resend());
        for message in AFTER_SUBMISSION {
            let task = panel.update(message);
            drive(&mut panel, task).await;
            assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
        }
        assert_eq!(shared.lock().unwrap().step1_opened, 0);
    }
    assert_eq!(seen.len(), 7);
    shared
        .lock()
        .unwrap()
        .afters
        .push_back(Step1AfterStep2::Eligible);
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_eq!(panel.step2_warning(), None);
    assert!(warning_lines(&panel).is_empty());
}

/// #637 r4172242637, reopened reconciler: the same after a restart with a
/// recorded step 2. The reorg warning comes with the BTCB2 observation,
/// only reconcile acts, and a later eligible check clears the warning.
#[tokio::test(flavor = "multi_thread")]
async fn panel_reconciler_warns_when_step1_loses_bitcoin_confirmation() {
    use crate::app::state::vault::split::step2::STEP1_REORGED_AFTER_STEP2;
    let journal = Journal::new(true);
    let shared: Shared = Arc::default();
    let mut panel = SplitPanel::resume(
        TARGET.into(),
        journal.temp.0.parent().unwrap().to_path_buf(),
        journal.digest(),
        journal.temp.0.clone(),
    );
    panel.set_connect(Some(Arc::new(PanelConnect(shared.clone()))));
    panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    shared.lock().unwrap().statuses.extend([
        Some(Status::Observation(Assessment::Reorged)),
        Some(Status::Observation(
            Assessment::ObservationsEligibleForPreflight,
        )),
    ]);

    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert_eq!(
        panel.step2_status(),
        Some(Status::Observation(Assessment::Reorged))
    );
    assert_eq!(warning_lines(&panel), [STEP1_REORGED_AFTER_STEP2]);
    assert!(matches!(
        panel.step2_seen(),
        Some(TransactionObservation::Unconfirmed { .. })
    ));
    for message in AFTER_SUBMISSION {
        let task = panel.update(message);
        drive(&mut panel, task).await;
        assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    }
    assert_eq!(shared.lock().unwrap().step1_opened, 0);
    assert!(panel.recon.is_some() && panel.prep.is_none() && panel.driver.is_none());

    // A revocation (logout, Cube close, a backend switch) and a reopen
    // keep the last evidence, and its warning comes back with the
    // reconciler instead of leaving the observation unexplained
    // (#637 r4172729359).
    // While the session is gone, its notice doesn't hide the warning
    // (#637 review 5971166062 F1).
    panel.revoke();
    assert_eq!(panel.stage, Stage::NeedsSession);
    let lines = warning_lines(&panel);
    assert_eq!(lines.len(), 2, "{:?}", lines);
    assert_eq!(lines[0], STEP1_REORGED_AFTER_STEP2);
    assert!(
        lines[1].starts_with("The split session ended."),
        "{}",
        lines[1]
    );
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert_eq!(
        panel.step2_status(),
        Some(Status::Observation(Assessment::Reorged))
    );
    assert!(panel.step2_seen().is_some());
    assert_eq!(warning_lines(&panel), [STEP1_REORGED_AFTER_STEP2]);
    assert_eq!(shared.lock().unwrap().step1_opened, 0);

    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert!(warning_lines(&panel).is_empty());
    assert_eq!(shared.lock().unwrap().reconciles, 2);
    // Eligible evidence reopens without a warning.
    panel.revoke();
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert!(warning_lines(&panel).is_empty());
}

/// #637 review 5971166062 F1: after a step-1 reorg is found, saving the
/// signed step 1 (it succeeds, fails or is cancelled) reports on its own
/// line, and the warning stays. So it does through a revocation and a
/// reopen, which keep the signed step 1 offered for saving, until new
/// evidence clears it.
#[tokio::test(flavor = "multi_thread")]
async fn panel_keeps_the_step1_warning_beside_every_notice() {
    use crate::app::state::vault::split::step2::STEP1_REORGED_AFTER_STEP2;
    // The journal records step 2, so a reopen only reconciles.
    let journal = Journal::new(true);
    let (mut panel, shared) = tracked_panel(&journal);
    let outcome = Outcome::UpstreamAccepted {
        txid: Txid::from_byte_array([5; 32]),
        wtxid: coincube_core::miniscript::bitcoin::Wtxid::from_byte_array([6; 32]),
    };
    panel.driver = None;
    panel.coord = Some(Box::new(PanelCoord::new(&shared, Some(outcome))));
    panel.step2_outcome = Some(outcome);
    panel.stage = Stage::Step2(Step2Stage::Submitted);
    panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
    shared.lock().unwrap().statuses.extend([
        Some(Status::Observation(Assessment::Reorged)),
        Some(Status::Observation(
            Assessment::ObservationsEligibleForPreflight,
        )),
    ]);
    let root = journal.temp.0.parent().unwrap().to_path_buf();
    // Save the signed step 1 to `path`; the warning lines afterwards.
    async fn save(panel: &mut SplitPanel, path: PathBuf) -> Vec<String> {
        let task = panel.export_signed_to(path);
        drive(panel, task).await;
        assert_rendered_feedback(panel).await;
        warning_lines(panel)
    }

    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_rendered_feedback(&panel).await;
    assert_eq!(warning_lines(&panel), [STEP1_REORGED_AFTER_STEP2]);
    assert!(panel.signed().is_some());

    let saved = root.join("signed-1.txt");
    let lines = save(&mut panel, saved.clone()).await;
    assert!(saved.exists());
    assert_eq!(lines.len(), 2, "{:?}", lines);
    assert_eq!(lines[0], STEP1_REORGED_AFTER_STEP2);
    assert!(
        lines[1].starts_with("Signed step 1 saved to"),
        "{}",
        lines[1]
    );

    let lines = save(&mut panel, root.join("missing").join("signed.txt")).await;
    assert_eq!(lines.len(), 2, "{:?}", lines);
    assert_eq!(lines[0], STEP1_REORGED_AFTER_STEP2);
    assert!(
        !lines[1].starts_with("Signed step 1 saved to"),
        "{}",
        lines[1]
    );
    assert_eq!(panel.notice(), Some(lines[1].as_str()));

    // A cancelled dialog changes nothing.
    let seq = panel.seq;
    let task = panel.apply(SplitEvent::SignedExportChosen(seq, None));
    drive(&mut panel, task).await;
    assert_rendered_feedback(&panel).await;
    assert_eq!(warning_lines(&panel), lines);
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Submitted));

    // Revoked: the session's notice and the warning, each on its line.
    panel.revoke();
    assert_eq!(panel.stage, Stage::NeedsSession);
    assert_rendered_feedback(&panel).await;
    let lines = warning_lines(&panel);
    assert_eq!(lines.len(), 2, "{:?}", lines);
    assert_eq!(lines[0], STEP1_REORGED_AFTER_STEP2);
    assert!(
        lines[1].starts_with("The split session ended."),
        "{}",
        lines[1]
    );

    // Reopened: only the reconciler, with the warning; saving still keeps
    // it.
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert!(panel.recon.is_some() && panel.coord.is_none());
    assert_rendered_feedback(&panel).await;
    assert_eq!(warning_lines(&panel), [STEP1_REORGED_AFTER_STEP2]);
    let saved = root.join("signed-2.txt");
    let lines = save(&mut panel, saved.clone()).await;
    assert!(saved.exists());
    assert_eq!(lines.len(), 2, "{:?}", lines);
    assert_eq!(lines[0], STEP1_REORGED_AFTER_STEP2);
    assert!(
        lines[1].starts_with("Signed step 1 saved to"),
        "{}",
        lines[1]
    );
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));

    // New eligible evidence clears the warning; a later save shows only
    // its own result.
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_rendered_feedback(&panel).await;
    assert!(warning_lines(&panel).is_empty());
    let lines = save(&mut panel, root.join("signed-3.txt")).await;
    assert_eq!(lines.len(), 1, "{:?}", lines);
    assert!(
        lines[0].starts_with("Signed step 1 saved to"),
        "{}",
        lines[0]
    );
    let counts = shared.lock().unwrap();
    assert_eq!((counts.submits, counts.step1_opened), (0, 0));
    assert_eq!(counts.reconciles, 2);
}

/// #637 F1: the Vault's daemon restarting or switching (a new daemon
/// instance behind an otherwise equal session) revokes every step-2 handle.
#[tokio::test(flavor = "multi_thread")]
async fn panel_revokes_step2_when_the_vault_daemon_changes() {
    let journal = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&journal);
    let task = panel.update(SplitMessage::EnterStep2);
    drive(&mut panel, task).await;
    assert!(panel.prep.is_some());
    let revoked = shared.lock().unwrap().revoked;
    panel.set_step2_port(Some(Arc::new(PanelPort::new(&shared, &journal, 2))));
    assert_eq!(shared.lock().unwrap().revoked, revoked + 1);
    assert!(panel.prep.is_none());
    assert_eq!(panel.stage, Stage::NeedsSession);
    // No port at all (the daemon unloaded) revokes the same way.
    let (mut panel, shared) = tracked_panel(&journal);
    let task = panel.update(SplitMessage::EnterStep2);
    drive(&mut panel, task).await;
    let revoked = shared.lock().unwrap().revoked;
    panel.set_step2_port(None);
    assert_eq!(shared.lock().unwrap().revoked, revoked + 1);
    assert!(panel.prep.is_none());
}

/// Traverse the actual laid-out view, including the production warning loop.
async fn rendered_labels(panel: &SplitPanel) -> Vec<String> {
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{Id, Operation, Tree},
        Layout,
    };
    #[derive(Default)]
    struct Labels(Vec<String>);
    impl Operation for Labels {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(&mut self, _: Option<&Id>, bounds: iced::Rectangle, text: &str) {
            assert!(bounds.width > 0.0 && bounds.height > 0.0);
            self.0.push(text.to_owned());
        }
    }
    let renderer = <iced::Renderer as Headless>::new(
        iced::Font::DEFAULT,
        iced::Pixels(16.0),
        Some("tiny-skia"),
    )
    .await
    .expect("software renderer for Split view regression");
    let mut element = crate::app::view::vault::split::split_panel(panel);
    let mut tree = Tree::new(element.as_widget());
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(iced::Size::ZERO, iced::Size::new(1200.0, 1600.0)),
    );
    let mut labels = Labels::default();
    element
        .as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
    labels.0
}
async fn assert_rendered_feedback(panel: &SplitPanel) {
    let labels = rendered_labels(panel).await;
    for expected in warning_lines(panel) {
        assert_eq!(
            labels.iter().filter(|text| **text == expected).count(),
            1,
            "warning/notice missing or duplicated in actual view: {expected}"
        );
    }
    if panel.step2_warning().is_none() {
        assert!(!labels.iter().any(|text| text == STEP1_REORGED_AFTER_STEP2));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn panel_recovers_from_expired_target_proof_without_replacing_address() {
    let journal = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&journal);
    for message in [
        SplitMessage::EnterStep2,
        SplitMessage::Step2Reserve,
        SplitMessage::Step2Check,
    ] {
        let task = panel.update(message);
        drive(&mut panel, task).await;
    }
    shared.lock().unwrap().stale_target = true;
    let task = panel.update(SplitMessage::Step2Build);
    drive(&mut panel, task).await;
    assert_eq!(panel.target_index(), None);
    assert_eq!(panel.replay_label(), None);
    assert!(panel
        .notice()
        .unwrap()
        .contains("Reserve address, then Check confirmations, then Build step 2"));
    let labels = rendered_labels(&panel).await;
    assert!(!labels
        .iter()
        .any(|text| text.contains("Fresh Vault address reserved")));
    assert!(!labels.iter().any(|text| text == "Build step 2"));
    let task = panel.update(SplitMessage::Step2Check);
    drive(&mut panel, task).await;
    let task = panel.update(SplitMessage::Step2Build);
    drive(&mut panel, task).await;
    assert_eq!(
        shared.lock().unwrap().builds,
        1,
        "a new check cannot restore target proof"
    );
    let task = panel.update(SplitMessage::Step2Reserve);
    drive(&mut panel, task).await;
    assert_eq!(panel.target_index(), Some(3));
    let task = panel.update(SplitMessage::Step2Build);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Sign));
    assert_eq!(shared.lock().unwrap().proofs, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn panel_ignores_noop_files_and_retries_complete_handoff_without_import() {
    let journal = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&journal);
    for message in [
        SplitMessage::EnterStep2,
        SplitMessage::Step2Reserve,
        SplitMessage::Step2Check,
        SplitMessage::Step2Build,
    ] {
        let task = panel.update(message);
        drive(&mut panel, task).await;
    }
    assert!(!panel.can_retry_step2_handoff());
    let mut signed = panel.step2_psbt().unwrap().clone();
    signed
        .sign(&journal.wallet.signer, &Secp256k1::new())
        .unwrap();
    let mut first = signed.clone();
    first.inputs[1].partial_sigs.clear();
    let mut second = signed.clone();
    second.inputs[0].partial_sigs.clear();
    let dir = journal.temp.0.parent().unwrap();
    let save = |name: &str, psbt: &Psbt| {
        let path = dir.join(name);
        std::fs::write(
            &path,
            split_psbt_file::encode(psbt, split_psbt_file::Encoding::Base64),
        )
        .unwrap();
        path
    };
    let unsigned = save("unsigned.txt", panel.step2_psbt().unwrap());
    let first = save("first.txt", &first);
    let second = save("second.txt", &second);
    let task = panel.step2_import_from(vec![unsigned.clone(), first.clone(), first.clone()]);
    drive(&mut panel, task).await;
    assert_eq!(panel.step2_files(), 1);
    assert!(!panel.can_retry_step2_handoff());
    for _ in 0..20 {
        let task = panel.step2_import_from(vec![first.clone(), unsigned.clone()]);
        drive(&mut panel, task).await;
        assert_eq!(panel.step2_files(), 1);
    }
    // A bad file rejects the entire selection, even after a useful one.
    let mut wrong = signed.clone();
    wrong.unsigned_tx.output[0].value = Amount::from_sat(1);
    let wrong = save("wrong.txt", &wrong);
    let retained = panel.step2_files.clone();
    let task = panel.step2_import_from(vec![second.clone(), wrong]);
    drive(&mut panel, task).await;
    assert_eq!(panel.step2_files, retained);
    assert!(!panel.can_retry_step2_handoff());
    let task = panel.update(SplitMessage::Step2RetryHandoff);
    drive(&mut panel, task).await;
    assert_eq!(shared.lock().unwrap().finishes, 0);
    shared.lock().unwrap().finish_failures = 1;
    let task = panel.step2_import_from(vec![second]);
    drive(&mut panel, task).await;
    assert_eq!(panel.step2_files(), 2);
    assert!(panel.can_retry_step2_handoff());
    assert!(rendered_labels(&panel)
        .await
        .iter()
        .any(|s| s == "Retry handoff"));
    let task = panel.update(SplitMessage::Step2RetryHandoff);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Signed));
    assert_eq!(shared.lock().unwrap().finishes, 2);
    assert_eq!(shared.lock().unwrap().submits, 0);
    let task = panel.update(SplitMessage::Step2Confirm);
    drive(&mut panel, task).await;
    assert_eq!(shared.lock().unwrap().submits, 0, "review remains required");
    panel.revoke();
    assert!(!panel.can_retry_step2_handoff());
}

#[tokio::test(flavor = "multi_thread")]
async fn panel_explains_final_entry_refusals_without_enabling_retry() {
    for error in [
        CoordinatorError::Unsupported,
        CoordinatorError::InvalidBinding,
        CoordinatorError::Journal(claim_workflow::Error::WrongIdentity),
    ] {
        let journal = Journal::new(false);
        let (mut panel, shared) = tracked_panel(&journal);
        let entering = panel.update(SplitMessage::EnterStep2);
        let events = events(entering).await;
        drop(events); // release the preparation just as a refused open does
        let reason = describe_check(error);
        assert!(!reason.retry);
        let task = panel.apply(SplitEvent::Step2Entered(panel.seq, Err(reason)));
        drive(&mut panel, task).await;
        let labels = rendered_labels(&panel).await;
        assert!(labels
            .iter()
            .any(|s| s.contains("Close and reopen the Cube")));
        assert!(!labels.iter().any(|s| s == "Try again"));
        let task = panel.update(SplitMessage::Retry);
        drive(&mut panel, task).await;
        assert!(matches!(panel.stage, Stage::Refused(_)));
        assert!(panel.prep.is_none() && panel.driver.is_none());
        assert_eq!(shared.lock().unwrap().submits, 0);
        let task = panel.update(SplitMessage::Close);
        drive(&mut panel, task).await;
        assert!(panel.is_hidden());
        assert!(journal.temp.0.exists());
    }
}

/// Copilot r4174940015: the exact file must be checked before merge can
/// supply or replace metadata. A bad later file rolls back useful earlier
/// files in the same selection. Cryptographic checks use the real core.
#[tokio::test(flavor = "multi_thread")]
async fn panel_rejects_raw_invalid_files_before_combining() {
    use coincube_core::miniscript::bitcoin::secp256k1::{Message as SecpMessage, SecretKey};
    for case in 0..3 {
        let journal = Journal::new(false);
        let (mut panel, shared) = tracked_panel(&journal);
        for message in [
            SplitMessage::EnterStep2,
            SplitMessage::Step2Reserve,
            SplitMessage::Step2Check,
            SplitMessage::Step2Build,
        ] {
            let task = panel.update(message);
            drive(&mut panel, task).await;
        }
        let mut signed = panel.step2_psbt().unwrap().clone();
        signed
            .sign(&journal.wallet.signer, &Secp256k1::new())
            .unwrap();
        let mut first = signed.clone();
        first.inputs[1].partial_sigs.clear();
        let mut second = signed.clone();
        second.inputs[0].partial_sigs.clear();
        let save = |name: &str, psbt: &Psbt| {
            let path = journal.temp.0.parent().unwrap().join(name);
            std::fs::write(
                &path,
                split_psbt_file::encode(psbt, split_psbt_file::Encoding::Base64),
            )
            .unwrap();
            path
        };
        let task = panel.step2_import_from(vec![save("first.txt", &first)]);
        drive(&mut panel, task).await;
        assert_eq!(panel.step2_files(), 1);
        let retained = panel.step2_files.clone();
        let mut invalid = first.clone();
        match case {
            // Same public-key slot as an already retained valid signature.
            0 => {
                invalid.inputs[0]
                    .partial_sigs
                    .values_mut()
                    .next()
                    .unwrap()
                    .signature = Secp256k1::new().sign_ecdsa(
                    &SecpMessage::from_digest([42; 32]),
                    &SecretKey::from_slice(&[42; 32]).unwrap(),
                )
            }
            1 => {
                assert!(invalid.inputs[0].non_witness_utxo.take().is_some());
            }
            2 => invalid.inputs[0].bip32_derivation.clear(),
            _ => unreachable!(),
        }
        assert!(panel
            .prep
            .as_ref()
            .unwrap()
            .verify_signed(&invalid, &panel.coins)
            .is_err());
        let invalid_path = save("invalid.txt", &invalid);
        let second_path = save("second.txt", &second);
        // Includes a valid duplicate control and a useful complementary file.
        let task = panel.step2_import_from(vec![
            save("duplicate.txt", &first),
            second_path.clone(),
            invalid_path.clone(),
        ]);
        drive(&mut panel, task).await;
        assert_eq!(panel.stage, Stage::Step2(Step2Stage::Sign), "case {case}");
        assert_eq!(
            panel.step2_files, retained,
            "case {case}: partial batch retained"
        );
        assert!(panel.prep.is_some());
        assert!(!panel.can_retry_step2_handoff());
        assert_eq!(shared.lock().unwrap().finishes, 0);
        assert_eq!(shared.lock().unwrap().submits, 0);
        // Reversing order also refuses; then valid complementary signatures work.
        let task = panel.step2_import_from(vec![invalid_path, second_path.clone()]);
        drive(&mut panel, task).await;
        assert_eq!(panel.step2_files, retained);
        let task = panel.step2_import_from(vec![second_path]);
        drive(&mut panel, task).await;
        assert_eq!(panel.stage, Stage::Step2(Step2Stage::Signed));
        assert_eq!(shared.lock().unwrap().submits, 0);
    }
}

/// Copilot r4174940044: terminal domain refusals keep their own recovery
/// instructions; they must not acquire session/identity/reopen advice.
#[tokio::test(flavor = "multi_thread")]
async fn panel_rejects_reopen_advice_for_terminal_domain_refusals() {
    use crate::app::state::vault::split::step1::Refusal;
    let journal = Journal::new(false);
    let (mut panel, _) = tracked_panel(&journal);
    let spent = describe_split_check(SplitCheckError::ClaimedCoinSpent(OutPoint::null())).reason;
    for reason in [
        step1::NO_PRE_FORK_COINS,
        step1::DESTINATION_USED,
        step1::STALE_ANCHOR,
        step1::NEW_POISON_NEEDED,
        step1::COMPLETED,
        spent.as_str(),
    ] {
        panel.stage = Stage::Refused(Refusal::final_(reason));
        let labels = rendered_labels(&panel).await;
        assert!(labels.iter().any(|label| label == reason));
        assert!(
            !labels
                .iter()
                .any(|label| label.contains("Close and reopen the Cube")),
            "{}",
            reason
        );
        assert!(!labels.iter().any(|label| label == "Try again"));
    }
    // A step-2-specific terminal domain error travels through the entry arm.
    let task = panel.apply(SplitEvent::Step2Entered(
        panel.seq,
        Err(describe_step2(Step2Error::DescriptorsForgotten)),
    ));
    drive(&mut panel, task).await;
    let labels = rendered_labels(&panel).await;
    assert!(labels.iter().any(|label| label == step1::COMPLETED));
    assert!(!labels
        .iter()
        .any(|label| label.contains("Close and reopen the Cube")));
}

/// A duplicate or unsigned import must not bypass a terminal handoff refusal.
#[tokio::test(flavor = "multi_thread")]
async fn panel_terminal_handoff_blocks_import_and_retry() {
    for consumed in [false, true] {
        for error in [
            CoordinatorError::Unsupported,
            CoordinatorError::InvalidBinding,
        ] {
            let journal = Journal::new(false);
            let (mut panel, shared) = tracked_panel(&journal);
            for message in [
                SplitMessage::EnterStep2,
                SplitMessage::Step2Reserve,
                SplitMessage::Step2Check,
                SplitMessage::Step2Build,
            ] {
                let task = panel.update(message);
                drive(&mut panel, task).await;
            }
            let unsigned = panel.step2_psbt().unwrap().clone();
            let mut signed = unsigned.clone();
            signed
                .sign(&journal.wallet.signer, &Secp256k1::new())
                .unwrap();
            let dir = journal.temp.0.parent().unwrap();
            let save = |name: &str, psbt: &Psbt| {
                let path = dir.join(name);
                std::fs::write(
                    &path,
                    split_psbt_file::encode(psbt, split_psbt_file::Encoding::Base64),
                )
                .unwrap();
                path
            };
            let signed_path = save("terminal-signed.txt", &signed);
            let unsigned_path = save("terminal-unsigned.txt", &unsigned);
            shared.lock().unwrap().terminal_finish = Some(describe_check(error));
            shared.lock().unwrap().consume_finish = consumed;
            let task = panel.step2_import_from(vec![signed_path.clone()]);
            drive(&mut panel, task).await;
            assert_eq!(shared.lock().unwrap().finishes, 1);
            assert!(!panel.can_retry_step2_handoff());
            assert!(
                matches!(&panel.stage, Stage::Refused(reason) if !reason.retry),
                "terminal classification lost (consumed={})",
                consumed
            );
            // Exercise the reported bypass before inspecting presentation state.
            for path in [signed_path, unsigned_path] {
                let task = panel.step2_import_from(vec![path]);
                drive(&mut panel, task).await;
                let finishes = shared.lock().unwrap().finishes;
                assert_eq!(finishes, 1, "terminal refusal retried through import");
            }
            for message in [SplitMessage::Step2RetryHandoff, SplitMessage::Retry] {
                let task = panel.update(message);
                drive(&mut panel, task).await;
            }
            assert!(matches!(panel.stage, Stage::Refused(_)));
            assert!(panel.prep.is_none());
            if !consumed {
                assert!(shared.lock().unwrap().revoked > 0);
            }
            assert!(panel.target_index().is_none());
            assert!(panel.replay_label().is_none());
            assert!(!panel.can_retry_step2_handoff());
            assert_eq!(shared.lock().unwrap().finishes, 1);
            assert_eq!(shared.lock().unwrap().submits, 0);
            let labels = rendered_labels(&panel).await;
            assert!(labels
                .iter()
                .any(|s| s.contains("Close and reopen the Cube")));
            assert!(!labels
                .iter()
                .any(|s| s == "Retry handoff" || s == "Try again"));
            assert!(journal.temp.0.exists());
        }
    }
}

/// A panel resumed on `journal` with a Connect session, the reconcile-only
/// port and, with `step2_port`, the Vault's step-2 port, restarted.
async fn restarted(journal: &Journal, step2_port: bool) -> (SplitPanel, Shared) {
    let shared: Shared = Arc::default();
    let mut panel = SplitPanel::resume(
        TARGET.into(),
        journal.temp.0.parent().unwrap().to_path_buf(),
        journal.digest(),
        journal.temp.0.clone(),
    );
    panel.set_connect(Some(Arc::new(PanelConnect(shared.clone()))));
    if step2_port {
        panel.set_step2_port(Some(Arc::new(PanelPort::new(&shared, journal, 1))));
    }
    panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    (panel, shared)
}

/// P3-3 through the panel: a restart of a journal whose step 2 may be
/// resent reopens the coordinator (still reconcile first, the Reconcile
/// stage). A resend needs an explicit review on screen (route and privacy
/// note, resend N of the limit, the recorded txid), is sent once per review
/// and comes back to Reconcile, uncertain again; another needs another
/// review. A reconcile, the review's lapse (its deadline or a generation
/// change) and a revocation drop the review. A refused review or send keeps
/// Reconcile with its reason. Nothing reopens step 1, builds or submits.
#[tokio::test(flavor = "multi_thread")]
async fn panel_resends_step2_only_from_an_explicit_review_after_a_restart() {
    async fn send(panel: &mut SplitPanel) {
        let task = panel.update(SplitMessage::Step2ConfirmResend);
        drive(panel, task).await;
        assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    }
    async fn review(panel: &mut SplitPanel) {
        let task = panel.update(SplitMessage::Step2ReviewResend);
        assert_eq!(panel.stage, Stage::Working(Work::Step2ResendReviewing));
        drive(panel, task).await;
        assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    }
    let resends = |shared: &Shared| shared.lock().unwrap().resends;
    let journal = Journal::returned(false);
    let (mut panel, shared) = restarted(&journal, true).await;
    assert_eq!(shared.lock().unwrap().reopened, 1);
    assert!(panel.coord.is_some() && panel.recon.is_none());
    assert!(panel.prep.is_none() && panel.driver.is_none());
    assert_eq!(panel.notice(), None);
    assert!(matches!(
        panel.step2_outcome(),
        Some(Outcome::Uncertain { .. })
    ));
    assert!(panel.can_review_resend());
    assert!(panel.step2_resend_review().is_none());
    // Nothing is sent without a review.
    send(&mut panel).await;
    assert_eq!(resends(&shared), 0);

    review(&mut panel).await;
    let view = panel.step2_resend_review().unwrap();
    assert_eq!(view.txid, Txid::from_byte_array([5; 32]));
    assert_eq!(view.route_label, "Your Bitcoin node");
    assert_eq!(view.privacy_note, Some(NODE_PRIVACY));
    assert_eq!(
        (view.attempt, view.max_attempts),
        (1, claim_workflow::MAX_SPLIT_STEP2_RESUBMISSIONS)
    );
    let task = panel.update(SplitMessage::Step2ConfirmResend);
    assert_eq!(panel.stage, Stage::Working(Work::Step2Resending));
    drive(&mut panel, task).await;
    assert_eq!(resends(&shared), 1);
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert!(panel.step2_resend_review().is_none());
    assert!(matches!(
        panel.step2_outcome(),
        Some(Outcome::Uncertain { .. })
    ));
    assert_eq!(panel.notice(), None);
    // The uncertain send read the journal again (#648 X1b): it still allows
    // a resend, so the coordinator was reopened.
    assert_eq!(shared.lock().unwrap().reopened, 2);
    assert!(panel.coord.is_some() && panel.recon.is_none());
    // Another resend needs another review.
    send(&mut panel).await;
    assert_eq!(resends(&shared), 1);

    // A reconcile drops the review, on the reopened coordinator. It finds
    // step 1 eligible, so a resend may be reviewed again (#568 S4b).
    review(&mut panel).await;
    assert_eq!(panel.step2_resend_review().unwrap().attempt, 2);
    shared
        .lock()
        .unwrap()
        .afters
        .push_back(Step1AfterStep2::Eligible);
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert_eq!(shared.lock().unwrap().reconciles, 1);
    assert!(panel.step2_resend_review().is_none());
    send(&mut panel).await;
    assert_eq!(resends(&shared), 1);

    // Its deadline, or the session's generation moving, drops it.
    review(&mut panel).await;
    assert!(panel.step2_resend_review().is_some());
    shared.lock().unwrap().resend_expired = true;
    assert!(panel.step2_resend_review().is_none());
    send(&mut panel).await;
    assert_eq!(resends(&shared), 1);

    // A refused review: its reason, and no review. (A refusal that leaves
    // no resend reads the journal again: #648 X1, in its own test.)
    shared.lock().unwrap().refuse_resend_review = Some(describe_resend(ResendError::Unavailable(
        OutPoint::new(Txid::from_byte_array([1; 32]), 0),
        FailureKind::Http(503),
    )));
    review(&mut panel).await;
    assert!(panel.notice().unwrap().contains("Connect couldn't read"));
    assert!(panel.step2_resend_review().is_none());
    assert!(panel.coord.is_some());

    // A refused send: its reason, back to Reconcile, the review used up.
    // The review granted first takes the refusal above off the screen
    // (#648 R3b).
    review(&mut panel).await;
    assert!(panel.step2_resend_review().is_some());
    assert_eq!(panel.notice(), None);
    let spent = OutPoint::new(Txid::from_byte_array([1; 32]), 0);
    shared
        .lock()
        .unwrap()
        .resend_results
        .push_back(Err(describe_resend(ResendError::ClaimedCoinSpent(spent))));
    send(&mut panel).await;
    assert_eq!(resends(&shared), 2);
    assert!(panel.notice().unwrap().contains("already spent"));
    assert!(panel.step2_resend_review().is_none());

    // A revocation (logout, Cube close, a backend switch) drops the review
    // with the coordinator.
    review(&mut panel).await;
    assert!(panel.step2_resend_review().is_some());
    panel.revoke();
    assert!(panel.step2_resend_review().is_none() && panel.coord.is_none());
    assert_eq!(panel.stage, Stage::NeedsSession);

    // Under the next session, an accepted resend is shown as accepted and
    // offers no further resend.
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    review(&mut panel).await;
    let accepted = Outcome::UpstreamAccepted {
        txid: Txid::from_byte_array([5; 32]),
        wtxid: coincube_core::miniscript::bitcoin::Wtxid::from_byte_array([6; 32]),
    };
    shared
        .lock()
        .unwrap()
        .resend_results
        .push_back(Ok(accepted));
    send(&mut panel).await;
    assert_eq!(resends(&shared), 3);
    assert_eq!(panel.step2_outcome(), Some(accepted));
    assert!(!panel.can_review_resend());
    let reviews = shared.lock().unwrap().resend_reviews;
    let task = panel.update(SplitMessage::Step2ReviewResend);
    drive(&mut panel, task).await;
    assert_eq!(shared.lock().unwrap().resend_reviews, reviews);

    let counts = shared.lock().unwrap();
    assert_eq!(
        (counts.step1_opened, counts.submits, counts.builds),
        (0, 0, 0)
    );
    // The first restart, the uncertain send's (#648 X1b) and the next
    // session's; the accepted send restarts nothing.
    assert_eq!(counts.reopened, 3);
}

/// P3-3: a resend is offered only where a restart reopened the coordinator.
/// Without the Vault's step-2 port the restart of a resendable journal opens
/// the reconciler and says why there is no resend; when the App's next
/// refresh brings the port, the reconciler is revoked and the restart
/// reopens the coordinator, until a reconcile sees step 2 on BTCB2. A step 2
/// seen there before the restart opens the reconciler even with the port.
/// The live coordinator after an uncertain submission (the Submitted stage)
/// offers none either.
#[tokio::test(flavor = "multi_thread")]
async fn panel_offers_a_resend_only_where_a_restart_reopened_the_coordinator() {
    let journal = Journal::returned(false);
    let (mut panel, shared) = restarted(&journal, false).await;
    assert!(panel.recon.is_some() && panel.coord.is_none());
    assert_eq!(panel.notice(), Some(RESEND_NEEDS_VAULT));
    assert!(!panel.can_review_resend());
    let task = panel.update(SplitMessage::Step2ReviewResend);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert_eq!(shared.lock().unwrap().resend_reviews, 0);

    // The App's next refresh: the Vault's step-2 port, then `begin`.
    panel.set_step2_port(Some(Arc::new(PanelPort::new(&shared, &journal, 1))));
    assert!(panel.recon.is_none());
    assert_eq!(panel.stage, Stage::NeedsSession);
    // A dead end read before does not outlive a restart into a resend: a
    // journal that allows one is in no dead end.
    panel.dead_end = Some(DeadEnd {
        step1: Txid::from_byte_array([1; 32]),
        step2: Txid::from_byte_array([5; 32]),
        claimed: Vec::new(),
    });
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert!(panel.coord.is_some() && panel.recon.is_none());
    assert_eq!(panel.notice(), None);
    assert!(panel.dead_end().is_none());
    assert!(panel.can_review_resend());
    assert_eq!(shared.lock().unwrap().reopened, 1);
    // A reconcile on it that sees step 2 on BTCB2: no resend is offered any
    // more, since its review could only refuse (#648 R2).
    shared.lock().unwrap().coord_seen = Some(TransactionObservation::Unconfirmed {
        txid: Txid::from_byte_array([5; 32]),
    });
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert!(matches!(
        panel.step2_seen(),
        Some(TransactionObservation::Unconfirmed { .. })
    ));
    assert!(panel.coord.is_some());
    assert!(!panel.can_review_resend());
    let task = panel.update(SplitMessage::Step2ReviewResend);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert_eq!(shared.lock().unwrap().resend_reviews, 0);

    let observed = Journal::returned(true);
    let (panel, shared) = restarted(&observed, true).await;
    assert!(panel.recon.is_some() && panel.coord.is_none());
    assert_eq!(panel.notice(), None);
    assert!(!panel.can_review_resend());
    assert_eq!(shared.lock().unwrap().reopened, 0);

    // The live coordinator after an uncertain submission: only the Reconcile
    // stage offers a resend, so neither message acts here.
    let live = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&live);
    panel.driver = None;
    panel.coord = Some(Box::new(PanelCoord::new(&shared, Some(uncertain()))));
    panel.step2_outcome = Some(uncertain());
    panel.stage = Stage::Step2(Step2Stage::Submitted);
    assert!(!panel.can_review_resend());
    for message in [
        SplitMessage::Step2ReviewResend,
        SplitMessage::Step2ConfirmResend,
    ] {
        let task = panel.update(message);
        drive(&mut panel, task).await;
        assert_eq!(panel.stage, Stage::Step2(Step2Stage::Submitted));
    }
    assert!(panel.step2_resend_review().is_none());
    let counts = shared.lock().unwrap();
    assert_eq!((counts.resend_reviews, counts.resends), (0, 0));
}

/// P3-3: a resend review or send still running when the session is revoked
/// (logout, Cube close, a backend switch) lands on nothing. Its coordinator
/// is not bound again, no review or outcome is taken from it, and the panel
/// stays without a session (#648 R3c).
#[tokio::test(flavor = "multi_thread")]
async fn panel_drops_a_resend_result_that_lands_after_a_revocation() {
    let journal = Journal::returned(false);
    let (mut panel, shared) = restarted(&journal, true).await;
    let task = panel.update(SplitMessage::Step2ReviewResend);
    assert_eq!(panel.stage, Stage::Working(Work::Step2ResendReviewing));
    panel.revoke();
    drive(&mut panel, task).await;
    assert_eq!(shared.lock().unwrap().resend_reviews, 1);
    assert_eq!(panel.stage, Stage::NeedsSession);
    assert!(panel.coord.is_none() && panel.step2_resend_review().is_none());

    // Under the next session: a review, then a send revoked while it runs.
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    let task = panel.update(SplitMessage::Step2ReviewResend);
    drive(&mut panel, task).await;
    assert!(panel.step2_resend_review().is_some());
    let accepted = Outcome::UpstreamAccepted {
        txid: Txid::from_byte_array([5; 32]),
        wtxid: coincube_core::miniscript::bitcoin::Wtxid::from_byte_array([6; 32]),
    };
    shared
        .lock()
        .unwrap()
        .resend_results
        .push_back(Ok(accepted));
    let task = panel.update(SplitMessage::Step2ConfirmResend);
    assert_eq!(panel.stage, Stage::Working(Work::Step2Resending));
    panel.revoke();
    drive(&mut panel, task).await;
    assert_eq!(shared.lock().unwrap().resends, 1);
    assert_eq!(panel.stage, Stage::NeedsSession);
    assert!(panel.coord.is_none() && panel.step2_resend_review().is_none());
    assert!(matches!(
        panel.step2_outcome(),
        Some(Outcome::Uncertain { .. })
    ));
    assert_eq!(shared.lock().unwrap().reopened, 2);
}

/// Withdraw the latest step-2 attempt's recorded return on disk, as an
/// interrupted or timed-out resend leaves it.
fn withdraw_return(journal: &Journal) {
    let path = journal.temp.0.join("intent.json");
    let mut intent: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    intent["split"]["step2_returned"] = serde_json::Value::Bool(false);
    std::fs::write(&path, serde_json::to_vec(&intent).unwrap()).unwrap();
}

/// #648 X1: when the coordinator refuses a resend review or send because no
/// resend can follow (the last attempt unsettled, or the attempt limit),
/// the panel reads the journal again. Its dead end then comes with the
/// reconciler, and the close follows a new reconcile; the refusal stays on
/// screen. No other refusal restarts.
#[tokio::test(flavor = "multi_thread")]
async fn panel_reopens_the_dead_end_when_no_resend_can_follow() {
    for refused_send in [false, true] {
        let journal = Journal::returned(false);
        let (mut panel, shared) = restarted(&journal, true).await;
        assert!(panel.coord.is_some() && panel.dead_end().is_none());
        // A reconcile saw step 2 absent (and step 1 eligible) before the
        // resend: once the journal is read again, the close waits for a new
        // one.
        shared
            .lock()
            .unwrap()
            .afters
            .push_back(Step1AfterStep2::Eligible);
        let task = panel.update(SplitMessage::Step2Reconcile);
        drive(&mut panel, task).await;
        assert_eq!(panel.step2_seen(), Some(TransactionObservation::Absent));
        withdraw_return(&journal);
        let expected = if refused_send {
            let task = panel.update(SplitMessage::Step2ReviewResend);
            drive(&mut panel, task).await;
            assert!(panel.step2_resend_review().is_some());
            let refusal = describe_resend(ResendError::AttemptsExhausted);
            shared
                .lock()
                .unwrap()
                .resend_results
                .push_back(Err(refusal.clone()));
            let task = panel.update(SplitMessage::Step2ConfirmResend);
            drive(&mut panel, task).await;
            assert_eq!(shared.lock().unwrap().resends, 1);
            refusal.reason
        } else {
            let refusal = describe_resend(ResendError::Unsettled);
            shared.lock().unwrap().refuse_resend_review = Some(refusal.clone());
            let task = panel.update(SplitMessage::Step2ReviewResend);
            drive(&mut panel, task).await;
            refusal.reason
        };
        assert_eq!(
            panel.stage,
            Stage::Step2(Step2Stage::Reconcile),
            "{}",
            refused_send
        );
        assert!(panel.recon.is_some() && panel.coord.is_none());
        assert!(panel.dead_end().is_some(), "{}", refused_send);
        assert_eq!(panel.notice(), Some(expected.as_str()));
        assert!(!panel.can_review_resend() && !panel.can_check_close());
        assert_eq!(shared.lock().unwrap().reopened, 1);
    }

    // Any other refusal keeps the coordinator.
    let journal = Journal::returned(false);
    let (mut panel, shared) = restarted(&journal, true).await;
    shared.lock().unwrap().refuse_resend_review = Some(describe_resend(ResendError::Unavailable(
        OutPoint::new(Txid::from_byte_array([1; 32]), 0),
        FailureKind::Http(503),
    )));
    let task = panel.update(SplitMessage::Step2ReviewResend);
    drive(&mut panel, task).await;
    assert!(panel.coord.is_some() && panel.recon.is_none());
    assert!(panel.can_review_resend());
}

/// #648 X1b (Legolas's probe, review 5985131761): a resend that comes back
/// uncertain and leaves the journal with no resend (its return withdrawn, as
/// a timed-out send leaves it) reads the journal again, as a refusal that
/// says so does. The dead end follows without another Review resend, and no
/// notice is added. A journal that still allows a resend reopens the
/// coordinator through the same restart.
#[tokio::test(flavor = "multi_thread")]
async fn panel_reads_the_journal_again_after_an_uncertain_resend() {
    let journal = Journal::returned(false);
    let (mut panel, shared) = restarted(&journal, true).await;
    let task = panel.update(SplitMessage::Step2ReviewResend);
    drive(&mut panel, task).await;
    assert!(panel.step2_resend_review().is_some());
    withdraw_return(&journal);
    // The fake's default send result: Uncertain.
    let task = panel.update(SplitMessage::Step2ConfirmResend);
    drive(&mut panel, task).await;
    assert!(matches!(
        panel.step2_outcome(),
        Some(Outcome::Uncertain { .. })
    ));
    assert!(
        panel.dead_end().is_some() && panel.coord.is_none(),
        "after an uncertain resend with no resend left: coordinator kept = {}, Review resend offered = {}",
        panel.coord.is_some(),
        panel.can_review_resend()
    );
    assert!(panel.recon.is_some());
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert!(!panel.can_review_resend());
    assert_eq!(panel.notice(), None);
    assert_eq!(shared.lock().unwrap().reopened, 1);

    // The journal still allows a resend: the restart reopens the
    // coordinator, and a resend needs a new review.
    let journal = Journal::returned(false);
    let (mut panel, shared) = restarted(&journal, true).await;
    let task = panel.update(SplitMessage::Step2ReviewResend);
    drive(&mut panel, task).await;
    let task = panel.update(SplitMessage::Step2ConfirmResend);
    drive(&mut panel, task).await;
    assert_eq!(shared.lock().unwrap().resends, 1);
    assert_eq!(shared.lock().unwrap().reopened, 2);
    assert!(panel.coord.is_some() && panel.dead_end().is_none());
    assert!(panel.can_review_resend() && panel.step2_resend_review().is_none());
    assert_eq!(panel.notice(), None);
}

/// S3-D1 (PortIdentity hardening): a step-2 handle moved into a task is not
/// held by the panel, yet a port change while the task runs still revokes
/// it, and the task's result lands on nothing. Covered: a check with the
/// preparation taken (its revoke handle still bound), an entry still opening
/// the preparation (nothing bound yet) and a restart still deciding.
#[tokio::test(flavor = "multi_thread")]
async fn port_change_during_an_in_flight_step2_task_revokes_it() {
    // A check in flight.
    let journal = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&journal);
    let task = panel.update(SplitMessage::EnterStep2);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Ready));
    let task = panel.update(SplitMessage::Step2Check);
    assert_eq!(panel.stage, Stage::Working(Work::Step2Checking));
    assert!(panel.prep.is_none() && panel.coord.is_none() && panel.recon.is_none());
    let revoked = shared.lock().unwrap().revoked;
    panel.set_step2_port(Some(Arc::new(PanelPort::new(&shared, &journal, 2))));
    assert_eq!(shared.lock().unwrap().revoked, revoked + 1);
    assert_eq!(panel.stage, Stage::NeedsSession);
    drive(&mut panel, task).await;
    assert!(panel.prep.is_none() && panel.replay_label().is_none());
    assert_eq!(panel.stage, Stage::NeedsSession);

    // An entry still opening the preparation.
    let journal = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&journal);
    let task = panel.update(SplitMessage::EnterStep2);
    assert_eq!(panel.stage, Stage::Working(Work::Entering));
    assert!(panel.step2_revoke.is_none());
    panel.set_step2_port(Some(Arc::new(PanelPort::new(&shared, &journal, 2))));
    assert_eq!(panel.stage, Stage::NeedsSession);
    drive(&mut panel, task).await;
    assert!(panel.prep.is_none() && panel.step2_revoke.is_none());
    assert_eq!(panel.stage, Stage::NeedsSession);

    // A restart still deciding, when the Vault's port goes away.
    let journal = Journal::returned(false);
    let shared: Shared = Arc::default();
    let mut panel = SplitPanel::resume(
        TARGET.into(),
        journal.temp.0.parent().unwrap().to_path_buf(),
        journal.digest(),
        journal.temp.0.clone(),
    );
    panel.set_connect(Some(Arc::new(PanelConnect(shared.clone()))));
    panel.set_step2_port(Some(Arc::new(PanelPort::new(&shared, &journal, 1))));
    panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
    let task = panel.begin();
    assert_eq!(panel.stage, Stage::Working(Work::Restarting));
    panel.set_step2_port(None);
    assert_eq!(panel.stage, Stage::NeedsSession);
    drive(&mut panel, task).await;
    assert!(panel.coord.is_none() && panel.recon.is_none());
    assert_eq!(panel.stage, Stage::NeedsSession);
    // The restart under the remaining ports opens only the reconciler.
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert!(panel.recon.is_some() && panel.coord.is_none());

    // A reconcile in flight on the reconciler, when the session's
    // reconcile-only port goes away.
    let task = panel.update(SplitMessage::Step2Reconcile);
    assert_eq!(panel.stage, Stage::Working(Work::Step2Reconciling));
    assert!(panel.recon.is_none());
    panel.set_recon_port(None);
    assert_eq!(panel.stage, Stage::NeedsSession);
    drive(&mut panel, task).await;
    assert!(panel.recon.is_none() && panel.step2_revoke.is_none());
    assert_eq!(panel.stage, Stage::NeedsSession);
}

/// S3 item 5: the "cannot replay" label lapses at its deadline on screen
/// with no input: the armed timer fires, the panel drops it, and Build is
/// gated off. No real waiting: the runtime's clock is paused and jumps to
/// the timer.
#[tokio::test(start_paused = true)]
async fn replay_label_and_build_lapse_at_the_deadline_without_input() {
    let journal = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&journal);
    for message in [
        SplitMessage::EnterStep2,
        SplitMessage::Step2Reserve,
        SplitMessage::Step2Check,
    ] {
        let task = panel.update(message);
        drive(&mut panel, task).await;
    }
    assert_eq!(panel.replay_label(), Some(CANNOT_REPLAY));
    let timer = panel.arm_deadline();
    // Armed once: the same deadline arms nothing more.
    assert!(iced_runtime::task::into_stream(panel.arm_deadline()).is_none());
    let start = tokio::time::Instant::now();
    drive(&mut panel, timer).await;
    assert!(tokio::time::Instant::now() - start >= std::time::Duration::from_secs(59));
    assert!(panel.replay.is_none());
    assert_eq!(panel.replay_label(), None);
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Ready));
    let task = panel.update(SplitMessage::Step2Build);
    drive(&mut panel, task).await;
    assert_eq!(shared.lock().unwrap().builds, 0);
    assert!(iced_runtime::task::into_stream(panel.arm_deadline()).is_none());
}

/// S3 item 5: a timer whose deadline was replaced (a new review) or
/// revoked with the session lands on nothing: what is on screen stays, and
/// no second timer is armed for it.
#[tokio::test(start_paused = true)]
async fn deadline_timer_is_dropped_by_replacement_and_revocation() {
    let lifetime = |shared: &Shared, secs: u64| {
        shared.lock().unwrap().resend_lifetime = Some(std::time::Duration::from_secs(secs));
    };
    let journal = Journal::returned(false);
    let (mut panel, shared) = restarted(&journal, true).await;

    // Replaced: a 60 s review, then a 120 s one.
    lifetime(&shared, 60);
    let task = panel.update(SplitMessage::Step2ReviewResend);
    drive(&mut panel, task).await;
    let first = panel.arm_deadline();
    lifetime(&shared, 120);
    let task = panel.update(SplitMessage::Step2ReviewResend);
    drive(&mut panel, task).await;
    let second = panel.arm_deadline();
    drive(&mut panel, first).await;
    assert!(panel.step2_resend.is_some());
    assert!(
        iced_runtime::task::into_stream(panel.arm_deadline()).is_none(),
        "a replaced timer re-armed the review's"
    );

    // Revoked: the session ends, and the next one's review outlives the
    // old timer.
    panel.revoke();
    assert!(panel.step2_resend.is_none());
    assert!(iced_runtime::task::into_stream(panel.arm_deadline()).is_none());
    let task = panel.begin();
    drive(&mut panel, task).await;
    lifetime(&shared, 600);
    let task = panel.update(SplitMessage::Step2ReviewResend);
    drive(&mut panel, task).await;
    let third = panel.arm_deadline();
    drive(&mut panel, second).await;
    assert!(panel.step2_resend_review().is_some());
    assert!(
        iced_runtime::task::into_stream(panel.arm_deadline()).is_none(),
        "a revoked timer re-armed the next review's"
    );
    // Its own timer still drops it.
    drive(&mut panel, third).await;
    assert!(panel.step2_resend.is_none());
}

/// S3 item 5: the resend review lapses at its deadline on screen with no
/// input; Send again then does nothing, and a new review may be asked for.
#[tokio::test(start_paused = true)]
async fn resend_review_lapses_at_its_deadline() {
    let journal = Journal::returned(false);
    let (mut panel, shared) = restarted(&journal, true).await;
    let task = panel.update(SplitMessage::Step2ReviewResend);
    drive(&mut panel, task).await;
    assert!(panel.step2_resend_review().is_some());
    let timer = panel.arm_deadline();
    drive(&mut panel, timer).await;
    assert!(panel.step2_resend.is_none());
    assert!(panel.can_review_resend());
    let task = panel.update(SplitMessage::Step2ConfirmResend);
    drive(&mut panel, task).await;
    assert_eq!(shared.lock().unwrap().resends, 0);
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
}

/// S3-D4: with step 1 six deep but no step-2 port, the panel says why: no
/// Vault daemon, a daemon on a route step 2 can't be sent through (naming
/// the two that can), or a port refused otherwise. With no reason recorded,
/// the daemon is missing; with a port, step 2 is offered instead.
#[tokio::test(flavor = "multi_thread")]
async fn step2_unavailable_copy_names_the_reason() {
    let journal = Journal::new(false);
    let (mut panel, _) = tracked_panel(&journal);
    assert!(panel.can_enter_step2());
    assert_eq!(panel.step2_unavailable_copy(), None);
    let labels = rendered_labels(&panel).await;
    assert!(labels.iter().any(|s| s.contains("Continue to step 2")));

    panel.step2_port = None;
    assert!(!panel.can_enter_step2());
    for (reason, copy) in [
        (None, STEP2_NEEDS_VAULT),
        (Some(Step2Unavailable::NoDaemon), STEP2_NEEDS_VAULT),
        (
            Some(Step2Unavailable::UnsupportedRoute),
            STEP2_UNSUPPORTED_ROUTE,
        ),
        (Some(Step2Unavailable::Refused), STEP2_REFUSED),
    ] {
        panel.note_step2_unavailable(reason);
        assert_eq!(panel.step2_unavailable_copy(), Some(copy));
        let labels = rendered_labels(&panel).await;
        assert_eq!(
            labels.iter().filter(|s| *s == copy).count(),
            1,
            "{:?}: {:?}",
            reason,
            labels
        );
    }
    assert!(STEP2_UNSUPPORTED_ROUTE.contains("Connect's Bitcoin Blake2b server"));
    assert!(STEP2_UNSUPPORTED_ROUTE.contains("this Vault's own Bitcoin Blake2b node"));
    assert!(!STEP2_UNSUPPORTED_ROUTE.contains("running"));
}

/// Consuming the preparation does not make a transient failure terminal.
#[tokio::test(flavor = "multi_thread")]
async fn panel_consumed_retryable_handoff_preserves_retry() {
    let journal = Journal::new(false);
    let (mut panel, _) = tracked_panel(&journal);
    for message in [
        SplitMessage::EnterStep2,
        SplitMessage::Step2Reserve,
        SplitMessage::Step2Check,
        SplitMessage::Step2Build,
    ] {
        let task = panel.update(message);
        drive(&mut panel, task).await;
    }
    drop(panel.prep.take());
    panel.step2_revoke = None;
    let task = panel.apply(SplitEvent::Step2Finished(
        panel.seq,
        Err((Step2Refusal::retry("Handoff interrupted; retry."), None)),
    ));
    drive(&mut panel, task).await;
    assert!(matches!(&panel.stage, Stage::Refused(reason) if reason.retry));
    let labels = rendered_labels(&panel).await;
    assert!(labels.iter().any(|s| s == "Try again"));
    assert!(!labels
        .iter()
        .any(|s| s.contains("Close and reopen the Cube")));
}

// ---- #568 B5b: the completion stage ----

/// One reconcile through the panel whose evidence is step 2 `seen` on
/// BTCB2 and step 1 `after` the step-2 submission.
async fn reconcile_seeing(
    panel: &mut SplitPanel,
    shared: &Shared,
    seen: TransactionObservation,
    after: Step1AfterStep2,
) {
    {
        let mut counts = shared.lock().unwrap();
        counts.recon_seen = Some(seen);
        counts.coord_seen = Some(seen);
        counts.afters.push_back(after);
    }
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(panel, task).await;
}
/// `Step2Complete` does nothing: no completion runs and the stage stays.
async fn complete_is_inert(panel: &mut SplitPanel, shared: &Shared, case: &str) {
    assert!(!panel.can_complete(), "{}", case);
    let stage = panel.stage.clone();
    let (completes, opened) = {
        let counts = shared.lock().unwrap();
        (counts.completes, counts.recon_opened)
    };
    let task = panel.update(SplitMessage::Step2Complete);
    drive(panel, task).await;
    assert_eq!(panel.stage, stage, "{}", case);
    let counts = shared.lock().unwrap();
    assert_eq!(
        (counts.completes, counts.recon_opened),
        (completes, opened),
        "{}",
        case
    );
}

/// B5b: completion is offered only after a reconcile under this session saw
/// step 2 confirmed on BTCB2 with step 1 eligible and no warning: not
/// before any reconcile, not for an unconfirmed or absent step 2, not in
/// any S4 outcome (shallow, re-mined, in the mempool, missing, a provisional
/// or terminal conflict, unknown), not after a failed reconcile, and not
/// after a revocation. Each refusal to offer is inert.
#[tokio::test(flavor = "multi_thread")]
async fn completion_is_offered_only_after_a_confirmed_reconcile() {
    use crate::services::claim_workflow::Step1Conflict;
    use coincube_core::claim::BlockRef;
    let block = |height: u64, n: u8| BlockRef {
        height,
        hash: hash(n),
    };
    let spent = OutPoint::new(Txid::from_byte_array([7; 32]), 1);
    let journal = Journal::new(true);
    let (mut panel, shared) = restarted(&journal, false).await;
    complete_is_inert(&mut panel, &shared, "before any reconcile").await;
    assert!(!rendered_labels(&panel)
        .await
        .iter()
        .any(|label| label == "Complete split"));

    let txid = Txid::from_byte_array([5; 32]);
    for (seen, case) in [
        (TransactionObservation::Absent, "absent"),
        (TransactionObservation::Unconfirmed { txid }, "unconfirmed"),
    ] {
        reconcile_seeing(&mut panel, &shared, seen, Step1AfterStep2::Eligible).await;
        complete_is_inert(&mut panel, &shared, case).await;
    }
    for after in [
        Step1AfterStep2::Shallow { confirmations: 5 },
        Step1AfterStep2::Remined {
            previous: block(100, 6),
            confirmed: block(101, 9),
        },
        Step1AfterStep2::InMempool,
        Step1AfterStep2::Missing,
        Step1AfterStep2::Conflict(Step1Conflict::new(spent, block(120, 4))),
        Step1AfterStep2::Conflict(
            Step1Conflict::new(spent, block(120, 4))
                .terminal(block(126, 5))
                .unwrap(),
        ),
        Step1AfterStep2::Unknown,
    ] {
        reconcile_seeing(&mut panel, &shared, confirmed_step2(), after).await;
        assert!(panel.step2_warning().is_some());
        complete_is_inert(&mut panel, &shared, &format!("{after:?}")).await;
    }

    // Confirmed with step 1 eligible: offered, and rendered.
    reconcile_seeing(
        &mut panel,
        &shared,
        confirmed_step2(),
        Step1AfterStep2::Eligible,
    )
    .await;
    assert!(panel.can_complete());
    assert!(rendered_labels(&panel)
        .await
        .iter()
        .any(|label| label == "Complete split"));
    // A failed reconcile withdraws it, though it keeps the last evidence.
    shared.lock().unwrap().statuses.push_back(None);
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_eq!(panel.step2_seen(), Some(confirmed_step2()));
    complete_is_inert(&mut panel, &shared, "after a failed reconcile").await;
    // Offered again by the next good one; a revocation withdraws it, and
    // the restart under the next session needs its own reconcile.
    reconcile_seeing(
        &mut panel,
        &shared,
        confirmed_step2(),
        Step1AfterStep2::Eligible,
    )
    .await;
    assert!(panel.can_complete());
    panel.revoke();
    assert!(!panel.can_complete());
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    complete_is_inert(&mut panel, &shared, "restarted, not reconciled").await;
    assert_eq!(shared.lock().unwrap().completes, 0);
}

/// B5b: completing from the reconciler moves to Completed with the history
/// row (short source digest, BTCB2 height, step-2 txid) and the completion
/// copy, keeping the reconciler bound; from the live coordinator after a
/// submission, the coordinator is dropped before the reconciler opens
/// (it has no completion check and holds the journal), and the reconciler
/// then completes and stays bound.
#[tokio::test(flavor = "multi_thread")]
async fn completion_moves_to_completed_with_the_history_row() {
    let journal = Journal::new(true);
    let (mut panel, shared) = restarted(&journal, false).await;
    reconcile_seeing(
        &mut panel,
        &shared,
        confirmed_step2(),
        Step1AfterStep2::Eligible,
    )
    .await;
    let task = panel.update(SplitMessage::Step2Complete);
    assert_eq!(panel.stage, Stage::Working(Work::Step2Completing));
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Completed));
    let completion = completed(Txid::from_byte_array([5; 32]));
    assert_eq!(panel.completion(), Some(&completion));
    assert!(panel.recon.is_some() && panel.coord.is_none());
    assert_eq!(panel.notice(), None);
    assert!(!panel.can_complete());
    let row = completion.history_row();
    assert!(row.contains(&completion.short_digest()));
    assert_eq!(completion.short_digest().len(), 16);
    assert!(completion
        .source_digest
        .to_string()
        .starts_with(&completion.short_digest()));
    assert!(row.contains(&format!("height {COMPLETED_HEIGHT}")));
    assert!(row.contains(&Txid::from_byte_array([5; 32]).to_string()));
    let labels = rendered_labels(&panel).await;
    for expected in [SPLIT_COMPLETED, row.as_str()] {
        assert_eq!(
            labels.iter().filter(|l| *l == expected).count(),
            1,
            "{}",
            expected
        );
    }
    assert_eq!(shared.lock().unwrap().completes, 1);

    // From the live coordinator.
    let journal = Journal::new(false);
    let (mut panel, shared) = tracked_panel(&journal);
    let outcome = Outcome::UpstreamAccepted {
        txid: Txid::from_byte_array([5; 32]),
        wtxid: coincube_core::miniscript::bitcoin::Wtxid::from_byte_array([6; 32]),
    };
    panel.driver = None;
    panel.coord = Some(Box::new(PanelCoord::new(&shared, Some(outcome))));
    panel.step2_outcome = Some(outcome);
    panel.stage = Stage::Step2(Step2Stage::Submitted);
    // Without the session's reconcile-only port there is nothing to
    // complete through.
    reconcile_seeing(
        &mut panel,
        &shared,
        confirmed_step2(),
        Step1AfterStep2::Eligible,
    )
    .await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Submitted));
    complete_is_inert(&mut panel, &shared, "no reconcile-only port").await;
    panel.set_recon_port(Some(Arc::new(PanelReconPort(shared.clone()))));
    assert!(panel.can_complete());
    let task = panel.update(SplitMessage::Step2Complete);
    assert!(panel.coord.is_none());
    drive(&mut panel, task).await;
    {
        let counts = shared.lock().unwrap();
        assert_eq!(counts.recon_opened, 1);
        assert_eq!(
            counts.coord_dropped_at_recon_open,
            [1],
            "the coordinator is dropped before the reconciler opens"
        );
        assert_eq!(counts.completes, 1);
        assert_eq!(counts.submits, 0);
    }
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Completed));
    assert!(panel.recon.is_some() && panel.coord.is_none());
    assert_eq!(panel.completion(), Some(&completion));
}

/// B5b and #645 P3-1: a refused completion (not complete yet, expired while
/// saving, the Cube's settings refusing the record, the deletion's evidence
/// lapsing) keeps the reconcile-only stage with the reconciler bound and
/// shows its copy as a retryable notice beside the step-1 evidence; it is
/// never terminal, and it withdraws the offer until a new reconcile. None
/// of the copy is the settings layer's "Claim Cube".
#[tokio::test(flavor = "multi_thread")]
async fn completion_refusal_keeps_reconcile_and_the_copy() {
    let journal = Journal::new(true);
    let (mut panel, shared) = restarted(&journal, false).await;
    for copy in [
        COMPLETION_NOT_YET,
        COMPLETION_EXPIRED,
        COMPLETION_NOT_RECORDED,
        COMPLETION_NOT_FORGOTTEN,
    ] {
        reconcile_seeing(
            &mut panel,
            &shared,
            confirmed_step2(),
            Step1AfterStep2::Eligible,
        )
        .await;
        assert!(panel.can_complete());
        shared
            .lock()
            .unwrap()
            .complete_results
            .push_back(Err(Step2Refusal::retry(copy)));
        let task = panel.update(SplitMessage::Step2Complete);
        drive(&mut panel, task).await;
        assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile), "{}", copy);
        assert_eq!(panel.notice(), Some(copy));
        assert_eq!(warning_lines(&panel), [copy]);
        assert!(panel.recon.is_some());
        assert_eq!(panel.completion(), None);
        assert!(!panel.can_complete(), "check again first: {}", copy);
        assert!(!copy.contains("Claim"), "{}", copy);
        assert!(copy.contains("Check status again"), "{}", copy);
        assert!(!copy.contains("can't complete"), "{}", copy);
        assert_rendered_feedback(&panel).await;
        assert!(rendered_labels(&panel)
            .await
            .iter()
            .any(|label| label == "Refresh"));
    }
    // The next good reconcile offers it again, and it completes.
    reconcile_seeing(
        &mut panel,
        &shared,
        confirmed_step2(),
        Step1AfterStep2::Eligible,
    )
    .await;
    assert_eq!(panel.notice(), None);
    let task = panel.update(SplitMessage::Step2Complete);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Completed));
    assert_eq!(shared.lock().unwrap().completes, 5);

    // A completion that lost its reconciler reads the journal again.
    let journal = Journal::new(true);
    let (mut panel, shared) = restarted(&journal, false).await;
    reconcile_seeing(
        &mut panel,
        &shared,
        confirmed_step2(),
        Step1AfterStep2::Eligible,
    )
    .await;
    shared
        .lock()
        .unwrap()
        .complete_results
        .push_back(Err(Step2Refusal {
            recovery: Step2Recovery::Restart,
            ..Step2Refusal::retry(COMPLETION_INTERRUPTED)
        }));
    let opened = shared.lock().unwrap().recon_opened;
    let task = panel.update(SplitMessage::Step2Complete);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert_eq!(panel.notice(), Some(COMPLETION_INTERRUPTED));
    assert_eq!(shared.lock().unwrap().recon_opened, opened + 1);
}

/// B5b: the Completed stage offers only Refresh and Close. No other message
/// acts (no resend, review, submit, abandon, step-1 action or second
/// completion), and nothing else renders as an action. Its Refresh asks
/// whether the completion still stands (D17): standing keeps Completed; a
/// loss goes back to reconciling with its copy and the record gone; a
/// failed check keeps Completed with the failure.
#[tokio::test(flavor = "multi_thread")]
async fn completed_stage_offers_no_resend() {
    let journal = Journal::returned(false);
    let (mut panel, shared) = restarted(&journal, true).await;
    // The restart reopened the coordinator for a resend the journal allows:
    // completion runs through the reconcile-only port all the same.
    assert!(panel.coord.is_some());
    reconcile_seeing(
        &mut panel,
        &shared,
        confirmed_step2(),
        Step1AfterStep2::Eligible,
    )
    .await;
    assert!(!panel.can_review_resend());
    let task = panel.update(SplitMessage::Step2Complete);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Completed));
    assert!(panel.coord.is_none() && panel.recon.is_some());
    let before = {
        let counts = shared.lock().unwrap();
        (
            counts.submits,
            counts.resend_reviews,
            counts.resends,
            counts.completes,
            counts.step1_opened,
            counts.reconciles,
        )
    };
    for message in AFTER_SUBMISSION
        .iter()
        .chain(&[SplitMessage::Step2Complete, SplitMessage::CheckAbandon])
    {
        let task = panel.update(message.clone());
        drive(&mut panel, task).await;
        assert_eq!(
            panel.stage,
            Stage::Step2(Step2Stage::Completed),
            "{:?}",
            message
        );
    }
    {
        let counts = shared.lock().unwrap();
        assert_eq!(
            (
                counts.submits,
                counts.resend_reviews,
                counts.resends,
                counts.completes,
                counts.step1_opened,
                counts.reconciles,
            ),
            before
        );
    }
    let labels = rendered_labels(&panel).await;
    let buttons: Vec<_> = labels
        .iter()
        .filter(|label| {
            [
                "Refresh",
                "Close",
                "Complete split",
                "Review resend",
                "Send again",
                "Save signed transaction",
                "Check before abandoning",
                "Abandon split",
                "Try again",
            ]
            .contains(&label.as_str())
        })
        .cloned()
        .collect();
    assert_eq!(buttons, ["Refresh", "Close"]);

    // Refresh: standing.
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Completed));
    assert_eq!(shared.lock().unwrap().rechecks, 1);
    // A failed check keeps Completed with its reason.
    shared
        .lock()
        .unwrap()
        .recheck_results
        .push_back(Err(Step2Refusal::retry("Connect couldn't be reached.")));
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Completed));
    assert_eq!(panel.notice(), Some("Connect couldn't be reached."));
    assert!(panel.completion().is_some());
    // D17: lost.
    shared
        .lock()
        .unwrap()
        .recheck_results
        .push_back(Ok(CompletionStanding::Lost {
            status: Status::Observation(Assessment::Reorged),
            seen: TransactionObservation::Absent,
            cleared: true,
        }));
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert_eq!(panel.completion(), None);
    assert_eq!(panel.notice(), Some(COMPLETION_LOST));
    assert!(!panel.can_complete());
    assert_eq!(panel.step2_seen(), Some(TransactionObservation::Absent));
    assert_eq!(shared.lock().unwrap().rechecks, 3);
}

/// B5b: a completion result that lands after a revocation is dropped with
/// its reconciler, which the revocation reached first: the panel waits for
/// the next session, shows no completion and holds no handle. The
/// coordinator path's late-bound handle is revoked the same way.
#[tokio::test(flavor = "multi_thread")]
async fn stale_completion_result_is_dropped() {
    let journal = Journal::new(true);
    let (mut panel, shared) = restarted(&journal, false).await;
    reconcile_seeing(
        &mut panel,
        &shared,
        confirmed_step2(),
        Step1AfterStep2::Eligible,
    )
    .await;
    let revoked = shared.lock().unwrap().revoked;
    let task = panel.update(SplitMessage::Step2Complete);
    panel.revoke();
    assert_eq!(shared.lock().unwrap().revoked, revoked + 1);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::NeedsSession);
    assert_eq!(panel.completion(), None);
    assert!(panel.recon.is_none() && panel.coord.is_none());

    // The coordinator path: revoked before the reconciler opens, which is
    // then revoked as soon as it opens, and completes nothing.
    let journal = Journal::returned(false);
    let (mut panel, shared) = restarted(&journal, true).await;
    reconcile_seeing(
        &mut panel,
        &shared,
        confirmed_step2(),
        Step1AfterStep2::Eligible,
    )
    .await;
    assert!(panel.coord.is_some());
    let task = panel.update(SplitMessage::Step2Complete);
    let revoked = shared.lock().unwrap().revoked;
    panel.revoke();
    drive(&mut panel, task).await;
    let counts = shared.lock().unwrap();
    assert_eq!(counts.recon_opened, 1);
    assert_eq!(counts.revoked, revoked + 1, "the reconciler it opened");
    assert_eq!(counts.completes, 0);
    drop(counts);
    assert_eq!(panel.stage, Stage::NeedsSession);
    assert_eq!(panel.completion(), None);
}

/// RevokeSlot: a handle bound before the revocation is revoked by it; one
/// bound after is revoked at once and refused.
#[test]
fn revoke_slot_reaches_a_handle_bound_before_or_after() {
    let count = Arc::new(AtomicUsize::new(0));
    let handle = || {
        let count = count.clone();
        Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }) as RevokeHandle
    };
    let slot = RevokeSlot::default();
    assert!(slot.bind(handle()));
    slot.handle()();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let slot = RevokeSlot::default();
    slot.handle()();
    assert!(!slot.bind(handle()));
    assert_eq!(count.load(Ordering::SeqCst), 2);
}

/// #568 S4b (Legolas F4, probe P-C): after a restart into a resend, Review
/// resend is offered only while the last reconcile found step 1 eligible
/// (before any reconcile, the review's own reads decide). A reconcile that
/// finds step 1 shallow, re-mined, unconfirmed, missing, in a provisional
/// or terminal conflict, or can't place it hides it beside its warning; an
/// eligible one offers it again. Nothing is reviewed or sent meanwhile.
#[tokio::test(flavor = "multi_thread")]
async fn panel_offers_review_resend_only_while_step1_is_eligible() {
    use crate::services::claim_workflow::Step1Conflict;
    let journal = Journal::returned(false);
    let (mut panel, shared) = restarted(&journal, true).await;
    assert!(panel.coord.is_some());
    assert!(panel.can_review_resend(), "before any reconcile");
    let coin = OutPoint::new(Txid::from_byte_array([1; 32]), 0);
    let tip = coincube_core::claim::BlockRef {
        height: 106,
        hash: hash(0x40),
    };
    let provisional = Step1Conflict::new(coin, tip);
    let terminal = provisional
        .terminal(coincube_core::claim::BlockRef {
            height: 112,
            hash: hash(0x41),
        })
        .unwrap();
    let block = |height| coincube_core::claim::BlockRef {
        height,
        hash: hash(height as u8),
    };
    for after in [
        Step1AfterStep2::Shallow { confirmations: 4 },
        Step1AfterStep2::Remined {
            previous: block(100),
            confirmed: block(101),
        },
        Step1AfterStep2::InMempool,
        Step1AfterStep2::Missing,
        Step1AfterStep2::Conflict(provisional),
        Step1AfterStep2::Conflict(terminal),
        Step1AfterStep2::Unknown,
        Step1AfterStep2::Eligible,
    ] {
        shared.lock().unwrap().afters.push_back(after);
        let task = panel.update(SplitMessage::Step2Reconcile);
        drive(&mut panel, task).await;
        assert_eq!(panel.step2_after(), Some(after));
        let eligible = after == Step1AfterStep2::Eligible;
        assert_eq!(panel.step2_warning().is_none(), eligible, "{after:?}");
        assert_eq!(
            panel.can_review_resend(),
            eligible,
            "Review resend beside {after:?}"
        );
        let task = panel.update(SplitMessage::Step2ReviewResend);
        drive(&mut panel, task).await;
        assert_eq!(
            shared.lock().unwrap().resend_reviews,
            usize::from(eligible),
            "{after:?}"
        );
    }
    assert_eq!(shared.lock().unwrap().resends, 0);
}
