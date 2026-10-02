//! The Split panel's step-2 stages (#568 B3b-2b-2) over fake step-2 handles:
//! entry from a tracked step 1 at six confirmations (the step-1 driver is
//! released first), the live label, N4, the build gate, signed-file import
//! into the handoff, the review with the route label and privacy note,
//! submission, revocation of every handle, leaving for step 1, and a
//! restart that opens only the reconciler.
use super::*;
use crate::app::{
    message::Message,
    state::vault::split::{SplitEvent, SplitMessage, SplitPanel, Stage, Step2Stage, Work},
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
    psbt: Psbt,
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
        Ok(3)
    }
    async fn build(&mut self, _: &Context, coins: Vec<SplitCoin>) -> Result<Psbt, Step2Refusal> {
        assert_eq!(coins.len(), 2, "the restored claimed coins");
        Ok(self.psbt.clone())
    }
    fn verify_signed(&self, signed: &Psbt, _: &[SplitCoin]) -> Result<bool, Step2Refusal> {
        Ok(signed
            .inputs
            .iter()
            .all(|input| !input.partial_sigs.is_empty()))
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
        Ok(Box::new(PanelCoord {
            shared: self.shared.clone(),
            reviewed: false,
            submitted: None,
        }))
    }
}

struct PanelCoord {
    shared: Shared,
    reviewed: bool,
    submitted: Option<Outcome>,
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
    ) -> Result<(Status, TransactionObservation), Step2Refusal> {
        self.shared.lock().unwrap().reconciles += 1;
        Ok((Status::Unchecked, TransactionObservation::Absent))
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
    ) -> Result<(Status, TransactionObservation), Step2Refusal> {
        self.0.lock().unwrap().reconciles += 1;
        Ok((
            Status::Unchecked,
            TransactionObservation::Unconfirmed {
                txid: Txid::from_byte_array([5; 32]),
            },
        ))
    }
}

struct PanelPort {
    shared: Shared,
    psbt: Psbt,
}
impl Step2Port for PanelPort {
    fn context(&self) -> Context {
        context()
    }
    fn open_preparation(&self, open: Step2Open) -> Result<Box<dyn Step2Prep>, Step2Refusal> {
        // The step-1 driver was released before this open (#626).
        assert_eq!(self.shared.lock().unwrap().step1_dropped, 1);
        assert_eq!(open.target_cube, TARGET);
        Ok(Box::new(PanelPrep {
            shared: self.shared.clone(),
            psbt: self.psbt.clone(),
            live: Vec::new(),
            generation: watch::channel(7).0,
        }))
    }
    fn open_reconciler(
        &self,
        _: PathBuf,
        _: String,
        _: sha256::Hash,
    ) -> Result<Box<dyn Step2Recon>, Step2Refusal> {
        Ok(Box::new(PanelRecon(self.shared.clone())))
    }
}

/// A resumed panel tracking step 1 at six confirmations, with a step-2 port.
fn tracked_panel(journal: &Journal) -> (SplitPanel, Shared) {
    let shared: Shared = Arc::default();
    let construction = step2(&journal.wallet, &journal.step1);
    let mut panel = SplitPanel::resume(
        TARGET.into(),
        journal.temp.0.parent().unwrap().to_path_buf(),
        journal.digest(),
        journal.temp.0.clone(),
    );
    panel.connect = Some(Arc::new(PanelConnect(shared.clone())));
    panel.step2_port = Some(Arc::new(PanelPort {
        shared: shared.clone(),
        psbt: construction.psbt().clone(),
    }));
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

    // Build is gated on a live label and a reserved target.
    let task = panel.update(SplitMessage::Step2Build);
    drive(&mut panel, task).await;
    assert!(panel.step2_psbt().is_none());
    let task = panel.update(SplitMessage::Step2Check);
    drive(&mut panel, task).await;
    assert_eq!(panel.replay_label(), Some(CANNOT_REPLAY));
    let task = panel.update(SplitMessage::Step2Reserve);
    assert_eq!(panel.stage, Stage::Working(Work::Reserving), "N4");
    drive(&mut panel, task).await;
    assert_eq!(panel.target_index(), Some(3));
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
    assert!(panel.notice().unwrap().contains("More are needed"));
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

    // Back in, then the Vault's daemon changes: a new port revokes.
    shared.lock().unwrap().step1_dropped = 0;
    let task = panel.update(SplitMessage::EnterStep2);
    drive(&mut panel, task).await;
    assert!(panel.prep.is_some());
    let revoked = shared.lock().unwrap().revoked;
    let other = Arc::new(PanelPort {
        shared: shared.clone(),
        psbt: step2(&journal.wallet, &journal.step1).psbt().clone(),
    });
    panel.set_step2_port(Some(other));
    assert_eq!(shared.lock().unwrap().revoked, revoked + 1);
    assert!(panel.prep.is_none());
    assert_eq!(panel.stage, Stage::NeedsSession);
}

/// A restart after a recorded step-2 submission opens only the reconciler:
/// the recorded outcome is shown as recorded, and the only action is to
/// reconcile.
#[tokio::test(flavor = "multi_thread")]
async fn panel_restart_after_a_recorded_step2_only_reconciles() {
    let journal = Journal::new(true);
    let shared: Shared = Arc::default();
    let mut panel = SplitPanel::resume(
        TARGET.into(),
        journal.temp.0.parent().unwrap().to_path_buf(),
        journal.digest(),
        journal.temp.0.clone(),
    );
    panel.set_connect(Some(Arc::new(PanelConnect(shared.clone()))));
    panel.set_step2_port(Some(Arc::new(PanelPort {
        shared: shared.clone(),
        psbt: step2(&journal.wallet, &journal.step1).psbt().clone(),
    })));
    let task = panel.begin();
    assert_eq!(panel.stage, Stage::Working(Work::Restarting));
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    assert!(matches!(
        panel.step2_outcome(),
        Some(Outcome::Uncertain { .. })
    ));
    assert_eq!(shared.lock().unwrap().step1_opened, 0);
    for message in [
        SplitMessage::Step2Confirm,
        SplitMessage::Step2Review,
        SplitMessage::EnterStep2,
    ] {
        let task = panel.update(message);
        drive(&mut panel, task).await;
        assert_eq!(panel.stage, Stage::Step2(Step2Stage::Reconcile));
    }
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert!(matches!(
        panel.step2_seen(),
        Some(TransactionObservation::Unconfirmed { .. })
    ));
    assert_eq!(shared.lock().unwrap().reconciles, 1);
}
