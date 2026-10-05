//! The Split panel's step-2 stages (#568 B3b-2b-2). See the parent module
//! for the flow; [`super::step2`] holds the ports, ordering and copy.
//!
//! Lock ordering (#626) is the step-2 layer's: entering drops the step-1
//! driver before the preparation opens, leaving drops the preparation before
//! the step-1 driver reopens, `finish` hands the journal to the coordinator,
//! and every open runs off the UI thread. A revocation (sign-out, account
//! change, Cube close or lock, node backend switch: `App::revoke_claim`)
//! revokes and drops every step-2 handle at once, and the live "cannot
//! replay" label dies with its check.

use std::{path::PathBuf, sync::Arc};

use iced::Task;

use coincube_core::{
    claim::MIN_CONFIRMATIONS,
    foreign_split::verify_split_step1_transaction,
    miniscript::bitcoin::{psbt::Psbt, secp256k1},
};

use super::{
    step1::{OpenRequest, Refusal, RefusalRecovery, SplitConnect},
    step2::{self, ReconPort, Step2Open, Step2Port, Step2Refusal},
    Coord, Driver, Held, Incoming, Prep, Recon, Restarted, Seen, SplitEvent, SplitMessage,
    SplitPanel, Stage, Step2Stage, Work,
};
use crate::{
    app::message::Message,
    services::{
        claim_coordinator::{fork::split::step2::Step1AfterStep2, Outcome},
        claim_observation::TransactionObservation,
        split_psbt_file::{self, Encoding},
    },
};

fn refusal(refusal: Step2Refusal) -> Refusal {
    Refusal {
        reason: refusal.reason,
        retry: refusal.retry,
        recovery: match refusal.recovery {
            step2::Step2Recovery::ReopenCube => RefusalRecovery::ReopenCube,
            _ => RefusalRecovery::None,
        },
    }
}

impl SplitPanel {
    /// Install (or clear) the target Vault's step-2 port. The App builds a
    /// new port on every Connect refresh: an equivalent one (same session
    /// context and daemon instance) is ignored, so a flow in progress keeps
    /// its handles; any other (another daemon, account, provider or
    /// generation, or none) revokes every step-2 handle first (#637 F1),
    /// including one a task holds ([`Self::step2_engaged`]).
    pub fn set_step2_port(&mut self, port: Option<Arc<dyn Step2Port>>) {
        let same = match (&self.step2_port, &port) {
            (Some(a), Some(b)) => a.identity() == b.identity(),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if self.step2_engaged() {
            self.revoke();
        }
        self.step2_port = port;
    }

    /// A step-2 handle is held, or may be in a task (S3-D1): one moved into
    /// a task is not held (`take_prep`, `take_coord`, a restart or entry
    /// still opening it), but its revoke handle stays bound or the stage
    /// shows the task. A port or session change then revokes it, and the
    /// sequence bump drops the task's result, as for a held one.
    fn step2_engaged(&self) -> bool {
        self.prep.is_some()
            || self.coord.is_some()
            || self.recon.is_some()
            || self.step2_revoke.is_some()
            || matches!(self.stage, Stage::Working(_))
    }

    /// The reconciler is held, or may be in a task (S3-D1): a restart still
    /// deciding (it opens the reconciler from the old port) or a step-2
    /// reconcile running (from the reconciler, or the coordinator, which is
    /// revoked with it: a reconcile is never more than a read).
    fn recon_engaged(&self) -> bool {
        self.recon.is_some()
            || matches!(
                self.stage,
                Stage::Working(
                    Work::Restarting
                        | Work::Step2Reconciling
                        | Work::Step2Completing
                        | Work::Step2CompletionChecking
                        | Work::Step2ReconfirmationReviewing
                        | Work::Step2Reconfirming
                )
            )
    }

    /// Install (or clear) the session's reconcile-only port (#637 R1). The
    /// App builds one on every Connect refresh, whether or not the Vault's
    /// daemon gives a step-2 port: an equivalent one (same session context)
    /// is ignored; any other revokes a reconciler opened under the old one,
    /// held or in a task ([`Self::recon_engaged`]).
    pub fn set_recon_port(&mut self, port: Option<Arc<dyn ReconPort>>) {
        let same = match (&self.recon_port, &port) {
            (Some(a), Some(b)) => a.context() == b.context(),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if self.recon_engaged() {
            self.revoke();
        }
        self.recon_port = port;
    }

    /// Revoke and drop every step-2 handle (releasing the journal) and the
    /// label; called from [`SplitPanel::revoke`]. The target proof dies with
    /// the preparation that made it (#637 r4172150937).
    pub(super) fn revoke_step2(&mut self) {
        if let Some(revoke) = self.step2_revoke.take() {
            revoke();
        }
        self.prep = None;
        self.coord = None;
        self.recon = None;
        self.replay = None;
        self.step2_review = None;
        self.step2_resend = None;
        self.reconfirmation = None;
        self.reconfirmation_final = false;
        self.target_index = None;
        self.step2_handoff_ready = false;
        self.completable = false;
        self.completion = None;
    }

    /// #568 B5c-1: the target Cube's `split_from` records, as the App read
    /// them with its settings. Only this split's (by source digest) are
    /// kept; a restart that reopens the reconciler for a step 2 one of them
    /// names opens in Completed and checks it at once (D17).
    pub fn set_split_from(&mut self, records: &[crate::app::settings::SplitFromRecord]) {
        let digest = self.journal.as_ref().map(|(digest, _)| *digest);
        self.recorded_completion = records
            .iter()
            .filter(|record| Some(record.descriptor_digest) == digest)
            .cloned()
            .map(step2::SplitCompletion::from)
            .collect();
    }

    /// Whether the Cube's `split_from`, as handed to this panel, records a
    /// completion of this split's step 2 `step2_txid` (#662 R7: the App's
    /// wiring is tested through it).
    pub fn records_completion_of(
        &self,
        step2_txid: &coincube_core::miniscript::bitcoin::Txid,
    ) -> bool {
        self.recorded_completion
            .iter()
            .any(|completion| &completion.step2_txid == step2_txid)
    }

    /// The recorded completion of the step 2 `outcome` names, if this
    /// split's `split_from` holds one.
    fn recorded_completion_of(&self, outcome: Option<Outcome>) -> Option<step2::SplitCompletion> {
        let txid = match outcome? {
            Outcome::Recorded { txid }
            | Outcome::UpstreamAccepted { txid, .. }
            | Outcome::Uncertain { txid, .. } => txid,
        };
        self.recorded_completion
            .iter()
            .find(|completion| completion.step2_txid == txid)
            .cloned()
    }

    pub fn step2_available(&self) -> bool {
        self.step2_port.is_some()
    }
    /// Record why the App's port build gave no step-2 port (S3-D4).
    pub fn note_step2_unavailable(&mut self, reason: Option<step2::Step2Unavailable>) {
        self.step2_missing = reason;
    }
    /// S3-G3 (CodeRabbit r4179804722): the App's port build ended without
    /// ports (its blocking task panicked). The step-2 port reads as
    /// [`step2::Step2Unavailable::Refused`]. Ports the panel still holds
    /// (an unchanged session keeps them) stay; a panel left without a
    /// Connect session is refused with a retry, which the App answers by
    /// building the ports again ([`Self::take_ports_retry`]), so it never
    /// waits for an unrelated refresh.
    pub fn note_ports_failed(&mut self) {
        if self.step2_port.is_none() {
            self.step2_missing = Some(step2::Step2Unavailable::Refused);
        }
        // #656 F3: an abandoned or closed split stays so; nothing is left to
        // build ports for.
        if self.connect.is_none()
            && !self.hidden
            && !matches!(self.stage, Stage::Abandoned | Stage::Closed)
        {
            self.ports_failed = true;
            self.stage = Stage::Refused(Refusal::retry(super::PORTS_INTERRUPTED));
        }
    }
    /// The retry of a failed port build was asked for: `true` (once) when
    /// the App should build the ports again; the panel then waits for them.
    pub fn take_ports_retry(&mut self) -> bool {
        if !std::mem::take(&mut self.ports_failed) || self.connect.is_some() {
            return false;
        }
        self.stage = Stage::NeedsSession;
        true
    }
    /// Why step 2 can't be entered for want of a step-2 port, in words; with
    /// no reason recorded, the Vault daemon is missing.
    pub fn step2_unavailable_copy(&self) -> Option<&'static str> {
        if self.step2_port.is_some() {
            return None;
        }
        Some(step2::unavailable_copy(
            self.step2_missing
                .unwrap_or(step2::Step2Unavailable::NoDaemon),
        ))
    }
    /// A recorded step 2 can be reconciled under the current session.
    pub fn reconcile_available(&self) -> bool {
        self.recon_port.is_some()
    }
    /// The live "Split — cannot replay" label, if a check's evidence is.
    pub fn replay_label(&self) -> Option<&'static str> {
        self.replay.as_ref().and_then(step2::CannotReplay::label)
    }
    pub fn target_index(&self) -> Option<u32> {
        self.target_index
    }
    pub fn step2_psbt(&self) -> Option<&Psbt> {
        self.step2_psbt.as_ref()
    }
    pub fn can_retry_step2_handoff(&self) -> bool {
        self.stage == Stage::Step2(Step2Stage::Sign)
            && self.prep.is_some()
            && self.step2_handoff_ready
            && self.step2_outcome.is_none()
    }
    pub fn step2_files(&self) -> usize {
        self.step2_files.len()
    }
    pub fn step2_exported(&self) -> Option<&PathBuf> {
        self.step2_exported.as_ref()
    }
    pub fn step2_review(&self) -> Option<&step2::Step2ReviewView> {
        self.step2_review.as_ref()
    }
    /// The resend review on screen while it is live (P3-3): its deadline,
    /// the session's revocation or a generation change drops it.
    pub fn step2_resend_review(&self) -> Option<&step2::Step2ResendView> {
        self.step2_resend.as_ref().filter(|view| view.is_live())
    }
    /// A resend may be reviewed (P3-3): only from the Reconcile stage, only
    /// when the restart reopened the coordinator for a resend the journal
    /// allows (the reconciler can't send), not once a resend was accepted,
    /// and not once a reconcile under this session saw step 2 on BTCB2,
    /// which the review could only refuse (#648 R2). Nor while the last
    /// reconcile found step 1 anything but eligible (#568 S4b, Legolas F4):
    /// shallow, re-mined, unconfirmed, missing, in conflict or unknown, the
    /// service refuses every resend. Before any reconcile the review's own
    /// fresh reads decide. The coordinator checks everything again with
    /// fresh evidence.
    pub fn can_review_resend(&self) -> bool {
        self.stage == Stage::Step2(Step2Stage::Reconcile)
            && self.coord.is_some()
            && self.connect.is_some()
            && !matches!(
                self.step2_outcome,
                Some(crate::services::claim_coordinator::Outcome::UpstreamAccepted { .. })
            )
            && matches!(
                self.step2_seen_here,
                None | Some(crate::services::claim_observation::TransactionObservation::Absent)
            )
            && self
                .step2_after
                .is_none_or(|after| after == Step1AfterStep2::Eligible)
    }
    /// #568 S4b, O1: the review of step 1's new block on screen while it is
    /// live (its deadline, its handle's revocation or a generation change
    /// drops it).
    pub fn reconfirmation_review(&self) -> Option<&step2::ReconfirmationView> {
        self.reconfirmation.as_ref().filter(|view| view.is_live())
    }
    /// #568 S4b, O1 (S4-D3): step 1's new block may be reviewed when the
    /// last reconcile found step 1 re-mined in another Bitcoin block, from
    /// whichever handle the panel holds after the step-2 submission (the
    /// coordinator, or the reconciler), and not beside a live resend review.
    /// The service checks everything again, refuses past the RDTS margin
    /// (S4-D4) and sends nothing.
    pub fn can_review_reconfirmation(&self) -> bool {
        let handle = match self.stage {
            Stage::Step2(Step2Stage::Submitted) => self.coord.is_some(),
            Stage::Step2(Step2Stage::Reconcile) => self.coord.is_some() || self.recon.is_some(),
            _ => false,
        };
        handle
            && self.connect.is_some()
            && !self.reconfirmation_final
            && matches!(self.step2_after, Some(Step1AfterStep2::Remined { .. }))
            && self.step2_resend_review().is_none()
    }
    pub fn step2_outcome(&self) -> Option<crate::services::claim_coordinator::Outcome> {
        self.step2_outcome
    }
    /// #568 B5b: the completion this panel recorded, for the history row.
    pub fn completion(&self) -> Option<&step2::SplitCompletion> {
        self.completion.as_ref()
    }
    /// #568 B5b: completion is offered only from the reconcile-only stages,
    /// after a reconcile under this session saw step 2 confirmed on BTCB2
    /// with step 1 eligible and nothing to warn about: never during a
    /// provisional or terminal step-1 conflict, a re-mined, unconfirmed or
    /// missing step 1, a shallow one or an unknown one (#568 S4), nor beside
    /// a live resend review. A coordinator completes through the session's
    /// reconcile-only port, which must be there.
    pub fn can_complete(&self) -> bool {
        let handle = match self.stage {
            Stage::Step2(Step2Stage::Submitted) => {
                self.coord.is_some() && self.recon_port.is_some()
            }
            Stage::Step2(Step2Stage::Reconcile) => {
                self.recon.is_some() || (self.coord.is_some() && self.recon_port.is_some())
            }
            _ => false,
        };
        handle
            && self.completable
            && self.completion.is_none()
            && self.connect.is_some()
            && self.journal.is_some()
            && matches!(
                self.step2_seen_here,
                Some(TransactionObservation::Confirmed { .. })
            )
            && self.step2_after == Some(Step1AfterStep2::Eligible)
            && self.step2_warning().is_none()
            && self.step2_resend_review().is_none()
    }
    pub fn step2_seen(&self) -> Option<crate::services::claim_observation::TransactionObservation> {
        self.step2_seen
    }
    /// The step-1 evidence of the last step-2 reconcile.
    pub fn step2_status(&self) -> Option<crate::services::claim_workflow::Status> {
        self.step2_status
    }
    /// What the last step-2 reconcile found of step 1 after the step-2
    /// submission (#568 S4).
    pub fn step2_after(
        &self,
    ) -> Option<crate::services::claim_coordinator::fork::split::step2::Step1AfterStep2> {
        self.step2_after
    }
    /// What that evidence warns about (#637 r4172242637), derived from it
    /// rather than kept in the notice: an operation's notice (a saved file, a
    /// failed export or check, an ended session) never replaces it, and only
    /// new evidence changes it (#637 review 5971166062 F1).
    pub fn step2_warning(&self) -> Option<String> {
        self.step2_after.and_then(step2::reconcile_warning)
    }
    /// Step 2 may be entered: step 1 tracked at six confirmations, the
    /// step-1 driver bound, a step-2 port and a Connect session.
    pub fn can_enter_step2(&self) -> bool {
        self.stage == Stage::Tracking
            && self.confirmations() == Some(MIN_CONFIRMATIONS)
            && !self.reorged()
            && self.driver.is_some()
            && self.step2_port.is_some()
            && self.connect.is_some()
            && self.construction.is_some()
            && self.signed.is_some()
    }

    fn bind_prep(&mut self, prep: Box<dyn step2::Step2Prep>) {
        self.step2_revoke = Some(prep.revoke_handle());
        self.prep = Some(prep);
    }
    fn bind_coord(&mut self, coord: Box<dyn step2::Step2Coord>) {
        self.step2_revoke = Some(coord.revoke_handle());
        self.coord = Some(coord);
    }
    fn bind_recon(&mut self, recon: Box<dyn step2::Step2Recon>) {
        self.step2_revoke = Some(recon.revoke_handle());
        self.recon = Some(recon);
    }

    /// The verified step 1 for opening either side, rebuilt off the UI
    /// thread from the kept construction and signed bytes.
    fn step1_verified(
        &self,
    ) -> Option<
        impl std::future::Future<
                Output = Result<
                    (
                        coincube_core::foreign_split::SplitStep1,
                        coincube_core::foreign_split::VerifiedSplitStep1,
                    ),
                    String,
                >,
            > + Send
            + 'static,
    > {
        let construction = self.construction.as_deref()?.clone();
        let signed = self.signed.clone()?;
        Some(async move {
            tokio::task::spawn_blocking(move || {
                let secp = secp256k1::Secp256k1::verification_only();
                verify_split_step1_transaction(&construction, &signed, &secp)
                    .map(|verified| (construction, verified))
                    .map_err(|error| format!("The recorded step 1 does not verify ({error})."))
            })
            .await
            .map_err(|_| "Opening the split was interrupted.".to_string())?
        })
    }

    fn take_prep(
        &mut self,
        work: Work,
    ) -> Option<(Box<dyn step2::Step2Prep>, Arc<dyn SplitConnect>)> {
        let connect = self.connect.clone()?;
        let prep = self.prep.take()?;
        self.stage = Stage::Working(work);
        Some((prep, connect))
    }
    fn take_coord(
        &mut self,
        work: Work,
    ) -> Option<(Box<dyn step2::Step2Coord>, Arc<dyn SplitConnect>)> {
        let connect = self.connect.clone()?;
        let coord = self.coord.take()?;
        self.stage = Stage::Working(work);
        Some((coord, connect))
    }
    /// #568 S4b: the step-2 handle the panel holds after the submission
    /// (the coordinator first), with the stage to return to.
    fn take_held(&mut self, work: Work) -> Option<(Held, Arc<dyn SplitConnect>, Step2Stage)> {
        let Stage::Step2(back) = self.stage else {
            return None;
        };
        let connect = self.connect.clone()?;
        let held = match (self.coord.take(), self.recon.take()) {
            (Some(coord), recon) => {
                self.recon = recon;
                Held::Coord(Coord(coord))
            }
            (None, Some(recon)) => Held::Recon(Recon(recon)),
            (None, None) => return None,
        };
        self.stage = Stage::Working(work);
        Some((held, connect, back))
    }
    fn bind_held(&mut self, held: Held) {
        match held {
            Held::Coord(Coord(coord)) => self.bind_coord(coord),
            Held::Recon(Recon(recon)) => self.bind_recon(recon),
        }
    }

    pub(super) fn update_step2(&mut self, message: SplitMessage) -> Task<Message> {
        let seq = self.seq;
        match message {
            SplitMessage::EnterStep2 if self.can_enter_step2() => {
                let (Some(port), Some(verified), Some((_, directory))) = (
                    self.step2_port.clone(),
                    self.step1_verified(),
                    self.journal.clone(),
                ) else {
                    return Task::none();
                };
                // #626: the step-1 driver is released before the
                // preparation opens (inside `enter_step2`).
                let Some(driver) = self.driver.take() else {
                    return Task::none();
                };
                self.revoke = None;
                let fork_height = self
                    .construction
                    .as_deref()
                    .map(|c| c.fork_height())
                    .unwrap_or_default();
                let target_cube = self.target_cube.clone();
                self.stage = Stage::Working(Work::Entering);
                self.spawn(
                    async move {
                        let (construction, verified) = match verified.await {
                            Ok(pair) => pair,
                            Err(reason) => {
                                drop(driver);
                                return Err(Step2Refusal {
                                    reason,
                                    retry: true,
                                    recovery: step2::Step2Recovery::None,
                                });
                            }
                        };
                        step2::enter_step2(
                            driver,
                            port,
                            Step2Open {
                                directory,
                                target_cube,
                                construction,
                                verified,
                                fork_height,
                            },
                        )
                        .await
                        .map(Prep)
                    },
                    SplitEvent::Step2Entered,
                )
            }
            SplitMessage::LeaveStep2
                if matches!(
                    self.stage,
                    Stage::Step2(Step2Stage::Ready | Step2Stage::Sign)
                ) =>
            {
                let (Some(verified), Some((_, directory))) =
                    (self.step1_verified(), self.journal.clone())
                else {
                    return Task::none();
                };
                let Some((prep, connect)) = self.take_prep(Work::Leaving) else {
                    return Task::none();
                };
                self.step2_revoke = None;
                self.replay = None;
                let target_cube = self.target_cube.clone();
                let fork_height = self
                    .construction
                    .as_deref()
                    .map(|c| c.fork_height())
                    .unwrap_or_default();
                self.spawn(
                    async move {
                        let (construction, verified) = match verified.await {
                            Ok(pair) => pair,
                            Err(reason) => {
                                drop(prep);
                                return Err(Refusal::retry(reason));
                            }
                        };
                        step2::leave_for_step1(
                            prep,
                            connect,
                            OpenRequest {
                                directory,
                                target_cube,
                                construction,
                                verified,
                                fork_height,
                                resume: true,
                            },
                        )
                        .await
                        .map(Driver)
                    },
                    SplitEvent::Step2Left,
                )
            }
            SplitMessage::Step2Check if self.stage == Stage::Step2(Step2Stage::Ready) => {
                // A new check supersedes the last label whatever its result.
                self.replay = None;
                let Some((mut prep, connect)) = self.take_prep(Work::Step2Checking) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = prep.check(&connect.context()).await;
                        (Prep(prep), result)
                    },
                    |seq, (prep, result)| SplitEvent::Step2Checked(seq, prep, result),
                )
            }
            SplitMessage::Step2Reserve if self.stage == Stage::Step2(Step2Stage::Ready) => {
                let Some((mut prep, connect)) = self.take_prep(Work::Reserving) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = prep.ensure_target(&connect.context()).await;
                        (Prep(prep), result)
                    },
                    |seq, (prep, result)| SplitEvent::Step2Reserved(seq, prep, result),
                )
            }
            SplitMessage::Step2Build
                if self.stage == Stage::Step2(Step2Stage::Ready)
                    && self.target_index.is_some()
                    && self.replay_label().is_some() =>
            {
                let coins = self.coins.clone();
                let Some((mut prep, connect)) = self.take_prep(Work::Step2Building) else {
                    return Task::none();
                };
                // The build redeems the check's token: the label goes with it.
                self.replay = None;
                self.spawn(
                    async move {
                        let result = prep.build(&connect.context(), coins).await;
                        (Prep(prep), result)
                    },
                    |seq, (prep, result)| SplitEvent::Step2Built(seq, prep, result),
                )
            }
            SplitMessage::Step2Export(encoding) if self.stage == Stage::Step2(Step2Stage::Sign) => {
                let name = match encoding {
                    Encoding::Binary => "split-step2.psbt",
                    Encoding::Base64 => "split-step2.txt",
                };
                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_file_name(name)
                            .save_file()
                            .await
                            .map(|handle| handle.path().to_path_buf())
                    },
                    move |path| {
                        Message::Split(Box::new(SplitEvent::Step2ExportChosen(seq, path, encoding)))
                    },
                )
            }
            SplitMessage::Step2Import if self.stage == Stage::Step2(Step2Stage::Sign) => {
                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .pick_files()
                            .await
                            .map(|handles| {
                                handles
                                    .iter()
                                    .map(|handle| handle.path().to_path_buf())
                                    .collect::<Vec<_>>()
                            })
                    },
                    move |paths| {
                        Message::Split(Box::new(SplitEvent::Step2ImportChosen(seq, paths)))
                    },
                )
            }
            SplitMessage::Step2RetryHandoff if self.can_retry_step2_handoff() => {
                let Some(base) = self.step2_psbt.as_ref() else {
                    return Task::none();
                };
                match combine(base, &self.step2_files) {
                    Ok(signed) => self.finish(signed),
                    Err(reason) => {
                        self.notice = Some(reason.reason);
                        Task::none()
                    }
                }
            }
            SplitMessage::Step2Review
                if matches!(
                    self.stage,
                    Stage::Step2(Step2Stage::Signed | Step2Stage::Review)
                ) =>
            {
                self.step2_review = None;
                let Some((mut coord, connect)) = self.take_coord(Work::Step2Reviewing) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = coord.review(&connect.context()).await;
                        (Coord(coord), result)
                    },
                    |seq, (coord, result)| SplitEvent::Step2Reviewed(seq, coord, result),
                )
            }
            SplitMessage::Step2Confirm if self.stage == Stage::Step2(Step2Stage::Review) => {
                self.step2_review = None;
                let Some((mut coord, connect)) = self.take_coord(Work::Step2Submitting) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = coord.submit(&connect.context()).await;
                        (Coord(coord), result)
                    },
                    |seq, (coord, result)| SplitEvent::Step2Submitted(seq, coord, result),
                )
            }
            // The live coordinator after a submission, or the one a restart
            // reopened for a resend (P3-3). A reconcile drops any resend
            // review.
            SplitMessage::Step2Reconcile
                if self.stage == Stage::Step2(Step2Stage::Submitted)
                    || (self.stage == Stage::Step2(Step2Stage::Reconcile)
                        && self.coord.is_some()) =>
            {
                let Stage::Step2(back) = self.stage else {
                    return Task::none();
                };
                self.step2_resend = None;
                self.reconfirmation = None;
                let Some((mut coord, connect)) = self.take_coord(Work::Step2Reconciling) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = coord.reconcile(&connect.context()).await;
                        (Coord(coord), result)
                    },
                    move |seq, (coord, result)| {
                        SplitEvent::Step2Reconciled(seq, coord, result, back)
                    },
                )
            }
            SplitMessage::Step2ReviewResend if self.can_review_resend() => {
                self.step2_resend = None;
                self.reconfirmation = None;
                let Some((mut coord, connect)) = self.take_coord(Work::Step2ResendReviewing) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = coord.review_resend(&connect.context()).await;
                        (Coord(coord), result)
                    },
                    |seq, (coord, result)| SplitEvent::Step2ResendReviewed(seq, coord, result),
                )
            }
            // Exactly the live review on screen, once: it is used up here
            // whatever the result.
            SplitMessage::Step2ConfirmResend
                if self.can_review_resend() && self.step2_resend_review().is_some() =>
            {
                self.step2_resend = None;
                let Some((mut coord, connect)) = self.take_coord(Work::Step2Resending) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = coord.confirm_resend(&connect.context()).await;
                        (Coord(coord), result)
                    },
                    |seq, (coord, result)| SplitEvent::Step2Resent(seq, coord, result),
                )
            }
            // #568 S4b, O1: a review of step 1's new block, then exactly
            // that review acknowledged (used up here whatever the result),
            // on whichever handle the panel holds.
            SplitMessage::Step2ReviewReconfirmation if self.can_review_reconfirmation() => {
                self.reconfirmation = None;
                let Some((held, connect, back)) =
                    self.take_held(Work::Step2ReconfirmationReviewing)
                else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let (held, result) = match held {
                            Held::Coord(Coord(mut coord)) => {
                                let result = coord.review_reconfirmation(&connect.context()).await;
                                (Held::Coord(Coord(coord)), result)
                            }
                            Held::Recon(Recon(mut recon)) => {
                                let result = recon.review_reconfirmation(&connect.context()).await;
                                (Held::Recon(Recon(recon)), result)
                            }
                        };
                        (held, result, back)
                    },
                    |seq, (held, result, back)| {
                        SplitEvent::Step2ReconfirmationReviewed(seq, held, result, back)
                    },
                )
            }
            SplitMessage::Step2ConfirmReconfirmation
                if self.can_review_reconfirmation() && self.reconfirmation_review().is_some() =>
            {
                self.reconfirmation = None;
                let Some((held, connect, back)) = self.take_held(Work::Step2Reconfirming) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let (held, result) = match held {
                            Held::Coord(Coord(mut coord)) => {
                                let result = coord.confirm_reconfirmation(&connect.context()).await;
                                (Held::Coord(Coord(coord)), result)
                            }
                            Held::Recon(Recon(mut recon)) => {
                                let result = recon.confirm_reconfirmation(&connect.context()).await;
                                (Held::Recon(Recon(recon)), result)
                            }
                        };
                        (held, result, back)
                    },
                    |seq, (held, result, back)| {
                        SplitEvent::Step2Reconfirmed(seq, held, result, back)
                    },
                )
            }
            SplitMessage::Step2Reconcile if self.stage == Stage::Step2(Step2Stage::Reconcile) => {
                let (Some(connect), Some(mut recon)) = (self.connect.clone(), self.recon.take())
                else {
                    return Task::none();
                };
                self.reconfirmation = None;
                self.stage = Stage::Working(Work::Step2Reconciling);
                self.spawn(
                    async move {
                        let result = recon.reconcile(&connect.context()).await;
                        (Recon(recon), result)
                    },
                    |seq, (recon, result)| SplitEvent::ReconReconciled(seq, recon, result),
                )
            }
            SplitMessage::Step2Complete if self.can_complete() => {
                let Some(connect) = self.connect.clone() else {
                    return Task::none();
                };
                let context = connect.context();
                if let Some(mut recon) = self.recon.take() {
                    self.stage = Stage::Working(Work::Step2Completing);
                    return self.spawn(
                        async move {
                            let result = recon.complete(&context).await;
                            (Some(Recon(recon)), result)
                        },
                        |seq, (recon, result)| SplitEvent::Step2Completed(seq, recon, result),
                    );
                }
                // The coordinator has no completion check: it is dropped,
                // releasing the journal, and the reconciler completes.
                let (Some(port), Some((digest, directory))) =
                    (self.recon_port.clone(), self.journal.clone())
                else {
                    return Task::none();
                };
                let Some(coord) = self.coord.take() else {
                    return Task::none();
                };
                self.step2_resend = None;
                let slot = step2::RevokeSlot::default();
                self.step2_revoke = Some(slot.handle());
                let target = self.target_cube.clone();
                self.stage = Stage::Working(Work::Step2Completing);
                self.spawn(
                    async move {
                        let (recon, result) = step2::complete_from_coordinator(
                            coord, port, directory, target, digest, context, slot,
                        )
                        .await;
                        (recon.map(Recon), result)
                    },
                    |seq, (recon, result)| SplitEvent::Step2Completed(seq, recon, result),
                )
            }
            // D17: a completed split's Refresh asks whether the completion
            // still stands.
            SplitMessage::Step2Reconcile if self.stage == Stage::Step2(Step2Stage::Completed) => {
                let (Some(connect), Some(mut recon)) = (self.connect.clone(), self.recon.take())
                else {
                    return Task::none();
                };
                self.stage = Stage::Working(Work::Step2CompletionChecking);
                self.spawn(
                    async move {
                        let result = recon.completion_stands(&connect.context()).await;
                        (Recon(recon), result)
                    },
                    |seq, (recon, result)| SplitEvent::Step2CompletionRechecked(seq, recon, result),
                )
            }
            SplitMessage::CheckAbandon if self.can_check_close() => {
                let (Some(connect), Some(dead_end)) = (self.connect.clone(), self.dead_end.clone())
                else {
                    return Task::none();
                };
                self.abandon_checked = false;
                let work = if dead_end.conflict.is_some() {
                    Work::CheckingConflictClose
                } else {
                    Work::CheckingClose
                };
                let back = std::mem::replace(&mut self.stage, Stage::Working(work));
                self.resume_stage = Some(back);
                self.spawn(
                    async move { step2::check_close(&*connect, &dead_end).await },
                    SplitEvent::AbandonChecked,
                )
            }
            SplitMessage::ConfirmAbandon if self.can_confirm_close() => {
                let (Some(connect), Some(dead_end), Some((digest, directory))) = (
                    self.connect.clone(),
                    self.dead_end.clone(),
                    self.journal.clone(),
                ) else {
                    return Task::none();
                };
                // Release the reconciler and its journal lock first.
                if let Some(revoke) = self.step2_revoke.take() {
                    revoke();
                }
                self.recon = None;
                self.coord = None;
                self.abandon_checked = false;
                let target = self.target_cube.clone();
                let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
                self.ending = Some(ended.clone());
                self.stage = Stage::Working(if dead_end.conflict.is_some() {
                    Work::ClosingConflict
                } else {
                    Work::Closing
                });
                self.spawn(
                    async move {
                        step2::check_close(&*connect, &dead_end)
                            .await
                            .map_err(|refusal| refusal.reason)?;
                        let context = connect.context();
                        let closed_at = connect.evidence().now();
                        tokio::task::spawn_blocking(move || {
                            step2::close(
                                &directory, &target, digest, context, &dead_end, closed_at, &ended,
                            )
                        })
                        .await
                        .map_err(|_| "Abandoning was interrupted.".to_string())?
                    },
                    SplitEvent::Closed,
                )
            }
            _ => Task::none(),
        }
    }

    /// Save the unsigned step 2 to `path`.
    pub fn step2_export_to(&mut self, path: PathBuf, encoding: Encoding) -> Task<Message> {
        let Some(psbt) = self.step2_psbt.clone() else {
            return Task::none();
        };
        self.stage = Stage::Working(Work::Step2Exporting);
        self.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    std::fs::write(&path, split_psbt_file::encode(&psbt, encoding))
                        .map(|()| Some(path))
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|_| "The export was interrupted.".to_string())?
            },
            SplitEvent::Step2Exported,
        )
    }

    /// Load the signed files at `paths`, combine them with those loaded and
    /// check them against the built step 2 without giving up the journal.
    /// At most [`split_psbt_file::MAX_COMBINED_FILES`] files in all, as for
    /// step 1: more is refused before any file is read, anything is cloned
    /// or the preparation is taken, keeping the loaded files
    /// (#637 r4174164844).
    pub fn step2_import_from(&mut self, paths: Vec<PathBuf>) -> Task<Message> {
        self.step2_import(paths.into_iter().map(Incoming::Path).collect())
    }

    /// The same verified step-2 import for signed PSBTs already in memory: a
    /// connected device's output (#568 B4b-3b) is checked, combined and
    /// handed over exactly as a signed file is, never finalized directly.
    pub fn step2_import_psbts(&mut self, psbts: Vec<Psbt>) -> Task<Message> {
        self.step2_import(psbts.into_iter().map(Incoming::Psbt).collect())
    }

    fn step2_import(&mut self, incoming: Vec<Incoming>) -> Task<Message> {
        if self.stage != Stage::Step2(Step2Stage::Sign) {
            return Task::none();
        }
        if self.step2_files.len().saturating_add(incoming.len())
            > split_psbt_file::MAX_COMBINED_FILES
        {
            self.notice = Some(split_psbt_file::FileError::TooManyFiles.to_string());
            return Task::none();
        }
        let (Some(base), Some(prep)) = (self.step2_psbt.clone(), self.prep.take()) else {
            return Task::none();
        };
        let mut files = self.step2_files.clone();
        let coins = self.coins.clone();
        self.stage = Stage::Working(Work::Step2Importing);
        self.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    let result =
                        (|| {
                            let mut combined = combine(&base, &files)?;
                            for item in incoming {
                                let file = item
                                    .load()
                                    .map_err(|error| Step2Refusal::retry(error.to_string()))?;
                                // Combining can supply missing fields or resolve conflicts.
                                // Check the exact input first, including no-op files, so
                                // retained metadata/signatures cannot mask malformed input.
                                prep.verify_signed(&file, &coins)?;
                                let candidate = combine(&combined, std::slice::from_ref(&file))?;
                                prep.verify_signed(&candidate, &coins)?;
                                // Only a validated new signature consumes a retained slot.
                                let adds_signature =
                                    candidate.inputs.iter().zip(&combined.inputs).any(
                                        |(new, old)| {
                                            new.partial_sigs
                                                .keys()
                                                .any(|key| !old.partial_sigs.contains_key(key))
                                        },
                                    );
                                if adds_signature {
                                    files.push(file);
                                    combined = candidate;
                                }
                            }
                            let complete = prep.verify_signed(&combined, &coins)?;
                            Ok((files, combined, complete))
                        })();
                    (Some(Prep(prep)), result)
                })
                .await
                .unwrap_or_else(|_| {
                    // The task ended without returning the preparation: the
                    // journal was released with it.
                    (
                        None,
                        Err(Step2Refusal {
                            reason: "The import was interrupted. Open the split again.".to_string(),
                            retry: true,
                            recovery: step2::Step2Recovery::None,
                        }),
                    )
                })
            },
            |seq, (prep, result)| SplitEvent::Step2Imported(seq, prep, result),
        )
    }

    fn finish(&mut self, signed: Psbt) -> Task<Message> {
        let (Some(prep), Some(connect)) = (self.prep.take(), self.connect.clone()) else {
            return Task::none();
        };
        self.step2_revoke = None;
        let coins = self.coins.clone();
        self.stage = Stage::Working(Work::Finishing);
        self.spawn(
            async move {
                let context = connect.context();
                match tokio::task::spawn_blocking(move || {
                    prep.finish(&context, &signed, &coins)
                        .map(Coord)
                        .map_err(|(refusal, prep)| (refusal, prep.map(Prep)))
                })
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err((
                        Step2Refusal {
                            reason: "Handing step 2 over was interrupted. Open the split again."
                                .to_string(),
                            retry: true,
                            recovery: step2::Step2Recovery::None,
                        },
                        None,
                    )),
                }
            },
            SplitEvent::Step2Finished,
        )
    }

    /// A reconcile after the step-2 submission, from the live coordinator or
    /// the reopened reconciler alike (#637 r4172242637). The BTCB2
    /// observation and the step-1 evidence are both kept. Evidence that no
    /// longer shows step 1 eligible on Bitcoin warns through
    /// [`Self::step2_warning`], one case per outcome (#568 S4), and drops
    /// any "cannot replay" label. A failed check keeps the last evidence and
    /// its warning; the notice carries only the failure. Nothing new is
    /// offered: the only action after a step-2 submission is still to
    /// reconcile.
    fn reconciled(&mut self, result: Seen) {
        match result {
            Ok((status, seen, after)) => {
                self.step2_status = Some(status);
                self.step2_after = Some(after);
                self.step2_seen = Some(seen);
                self.step2_seen_here = Some(seen);
                self.notice = None;
                // #568 B5b: only this reconcile's own evidence offers
                // completion.
                self.completable = matches!(seen, TransactionObservation::Confirmed { .. })
                    && after == Step1AfterStep2::Eligible;
                self.dead_end = self
                    .dead_end
                    .take()
                    .filter(|dead_end| match dead_end.conflict {
                        // Seen on BTCB2: it left, so it is no dead end (#625 F2).
                        None => seen == TransactionObservation::Absent,
                        // O4 (#568 S4b): step 2's bytes stand wherever they are;
                        // the dead end lasts while the reconcile reports its
                        // conflict (an eligible step 1 disproved it, S4-D6).
                        Some(conflict) => after == Step1AfterStep2::Conflict(conflict),
                    });
            }
            Err(reason) => {
                self.completable = false;
                self.notice = Some(reason.reason);
            }
        }
        if self.step2_warning().is_some() || self.notice.is_some() {
            self.replay = None;
        }
    }

    /// #568 S4b, O4: the last reconcile reported a terminal step-1 conflict
    /// that the panel holds no dead end for (it became terminal under this
    /// session, or was read before): the journal is read again, as a
    /// restart, so its dead end comes with the reconciler.
    fn conflict_unread(&self) -> bool {
        match self.step2_after {
            Some(Step1AfterStep2::Conflict(conflict)) if conflict.is_terminal() => {
                self.dead_end
                    .as_ref()
                    .and_then(|dead_end| dead_end.conflict)
                    != Some(conflict)
            }
            _ => false,
        }
    }

    pub(super) fn apply_step2(&mut self, event: SplitEvent) -> Task<Message> {
        match event {
            SplitEvent::Restarted(_, Ok(Restarted::Step1)) => match self.connect.clone() {
                Some(connect) => self.resume_journal(connect),
                None => {
                    self.stage = Stage::NeedsSession;
                    Task::none()
                }
            },
            SplitEvent::Restarted(
                _,
                Ok(Restarted::Reconcile(Recon(recon), dead_end, unavailable, forgotten)),
            ) => {
                self.outcome = None;
                self.step2_outcome = recon.recorded_outcome();
                self.bind_recon(recon);
                self.dead_end = dead_end;
                // Why a resend the journal allows was not opened (P3-3).
                // Otherwise the notice stays: `begin` cleared it, unless the
                // coordinator's refusal asked for this restart (#648 X1).
                if unavailable.is_some() {
                    self.notice = unavailable;
                }
                // #568 B5c-1: a completion this Cube records for this step 2
                // opens in Completed, and is checked at once (D17), so a
                // reorg since clears the record without a Refresh. A dead
                // end is never shown as completed, nor (#662 F1) a split
                // whose descriptors are still on this device: a record
                // written before its deletion failed is finished from
                // Reconcile by completing again (the record is not written
                // twice), and Completed's copy says they were deleted.
                if self.dead_end.is_none() && forgotten {
                    if let Some(completion) = self.recorded_completion_of(self.step2_outcome) {
                        self.completion = Some(completion);
                        self.completable = false;
                        self.stage = Stage::Step2(Step2Stage::Completed);
                        return self.update_step2(SplitMessage::Step2Reconcile);
                    }
                }
                // The last reconcile's step-1 evidence is kept through the
                // revocation along with its BTCB2 observation, so its
                // warning stays until a new reconcile replaces it
                // (#637 r4172729359).
                self.stage = Stage::Step2(Step2Stage::Reconcile);
                Task::none()
            }
            SplitEvent::Restarted(_, Ok(Restarted::Resend(Coord(coord)))) => {
                // P3-3: the coordinator, for a resend the journal allows. It
                // reconciles like the reconciler; a resend needs a review.
                // A journal that allows a resend is in no dead end.
                self.outcome = None;
                self.step2_outcome = coord.recorded_outcome();
                self.bind_coord(coord);
                self.dead_end = None;
                self.stage = Stage::Step2(Step2Stage::Reconcile);
                Task::none()
            }
            // B4b-3c: a fork-only record opens by kind.
            SplitEvent::Restarted(_, Ok(Restarted::Unified(record))) => {
                self.restart_unified(record)
            }
            SplitEvent::Restarted(_, Ok(Restarted::Closed)) => {
                self.notice = None;
                self.stage = Stage::Closed;
                Task::none()
            }
            SplitEvent::Restarted(_, Err(reason)) => {
                self.stage = Stage::Refused(refusal(reason));
                Task::none()
            }
            SplitEvent::Closed(_, Ok(())) => {
                self.ending = None;
                self.dead_end = None;
                self.notice = None;
                self.stage = Stage::Closed;
                Task::none()
            }
            SplitEvent::Closed(_, Err(reason)) => {
                self.ending = None;
                // The reconciler was released for the close: reopen it.
                self.stage = Stage::Refused(Refusal::retry(reason));
                Task::none()
            }
            SplitEvent::Step2Entered(_, Ok(Prep(prep))) => {
                self.notice = None;
                // A new preparation has proven no target yet, whatever an
                // earlier one did (#637 r4172150937): reserve, then build.
                self.target_index = None;
                self.bind_prep(prep);
                self.stage = Stage::Step2(Step2Stage::Ready);
                Task::none()
            }
            SplitEvent::Step2Entered(_, Err(reason)) => {
                // The step-1 driver was released, so the panel holds no
                // handle; the journal is kept. A retryable refusal offers
                // Try again, which reopens it; a final one (`Unsupported`,
                // `InvalidBinding`, `WrongIdentity`) says to close and reopen
                // the Cube, whose next open resumes it (#637 review E3).
                self.stage = Stage::Refused(refusal(reason));
                Task::none()
            }
            SplitEvent::Step2Left(_, Ok(Driver(driver))) => {
                self.bind(driver);
                self.target_index = None;
                self.step2_psbt = None;
                self.step2_files.clear();
                self.step2_handoff_ready = false;
                self.settle();
                Task::none()
            }
            SplitEvent::Step2Left(_, Err(reason)) => {
                self.stage = Stage::Refused(reason);
                Task::none()
            }
            SplitEvent::Step2Checked(_, Prep(prep), result) => {
                self.bind_prep(prep);
                match result {
                    Ok(label) => {
                        self.notice = None;
                        self.replay = Some(label);
                    }
                    Err(reason) => {
                        self.replay = None;
                        self.notice = Some(reason.reason);
                    }
                }
                self.stage = Stage::Step2(Step2Stage::Ready);
                Task::none()
            }
            SplitEvent::Step2Reserved(_, Prep(prep), result) => {
                self.bind_prep(prep);
                match result {
                    Ok(index) => {
                        self.notice = None;
                        self.target_index = Some(index);
                    }
                    Err(reason) => {
                        self.target_index = None;
                        self.notice = Some(reason.reason);
                    }
                }
                self.stage = Stage::Step2(Step2Stage::Ready);
                Task::none()
            }
            SplitEvent::Step2Built(_, Prep(prep), result) => {
                self.bind_prep(prep);
                match result {
                    Ok(psbt) => {
                        self.notice = None;
                        self.step2_psbt = Some(psbt);
                        self.step2_files.clear();
                        self.step2_handoff_ready = false;
                        // An earlier export holds an earlier PSBT
                        // (#637 r4172150954).
                        self.step2_exported = None;
                        self.stage = Stage::Step2(Step2Stage::Sign);
                    }
                    Err(reason) => {
                        if reason.recovery == step2::Step2Recovery::RefreshTarget {
                            self.target_index = None;
                        }
                        self.notice = Some(reason.reason);
                        self.stage = Stage::Step2(Step2Stage::Ready);
                    }
                }
                Task::none()
            }
            SplitEvent::Step2ExportChosen(_, Some(path), encoding) => {
                self.step2_export_to(path, encoding)
            }
            SplitEvent::Step2ImportChosen(_, Some(paths)) if !paths.is_empty() => {
                self.step2_import_from(paths)
            }
            SplitEvent::Step2ExportChosen(..) | SplitEvent::Step2ImportChosen(..) => Task::none(),
            SplitEvent::Step2Exported(_, result) => {
                match result {
                    Ok(path) => self.step2_exported = path.or(self.step2_exported.take()),
                    Err(reason) => self.notice = Some(reason),
                }
                self.stage = Stage::Step2(Step2Stage::Sign);
                Task::none()
            }
            SplitEvent::Step2Imported(_, None, result) => {
                let reason = result.err().map(|r| r.reason).unwrap_or_default();
                self.stage = Stage::Refused(Refusal::retry(reason));
                Task::none()
            }
            SplitEvent::Step2Imported(_, Some(Prep(prep)), result) => {
                self.bind_prep(prep);
                self.stage = Stage::Step2(Step2Stage::Sign);
                match result {
                    Ok((files, combined, complete)) => {
                        let added = files.len() > self.step2_files.len();
                        self.step2_files = files;
                        self.step2_handoff_ready = complete;
                        if complete {
                            self.notice = None;
                            self.device.close();
                            return self.finish(combined);
                        }
                        self.notice = Some(
                            if added {
                                "Signatures loaded. More are needed: import the other signers' files."
                            } else {
                                "No new signatures found. Import the other signers' files."
                            }.to_string(),
                        );
                    }
                    Err(reason) => self.notice = Some(reason.reason),
                }
                Task::none()
            }
            SplitEvent::Step2Finished(_, Ok(Coord(coord))) => {
                self.notice = None;
                self.bind_coord(coord);
                self.step2_handoff_ready = false;
                self.stage = Stage::Step2(Step2Stage::Signed);
                Task::none()
            }
            SplitEvent::Step2Finished(_, Err((reason, Some(Prep(prep))))) => {
                self.bind_prep(prep);
                if reason.retry {
                    self.step2_handoff_ready = true;
                    self.notice = Some(reason.reason);
                    self.stage = Stage::Step2(Step2Stage::Sign);
                } else {
                    // Import also initiates handoff once retained signatures are
                    // complete. A terminal refusal must disable that path too.
                    self.revoke_step2();
                    self.notice = None;
                    self.stage = Stage::Refused(refusal(reason));
                }
                Task::none()
            }
            SplitEvent::Step2Finished(_, Err((reason, None))) => {
                // Consumption releases the journal, but does not change whether
                // the failure is recoverable by retrying under this session.
                if !reason.retry {
                    self.revoke_step2();
                    self.notice = None;
                }
                self.stage = Stage::Refused(refusal(reason));
                Task::none()
            }
            SplitEvent::Step2Reviewed(_, Coord(coord), result) => {
                self.bind_coord(coord);
                match result {
                    Ok(view) => {
                        self.notice = None;
                        self.step2_review = Some(view);
                        self.stage = Stage::Step2(Step2Stage::Review);
                    }
                    Err(reason) => {
                        self.notice = Some(reason.reason);
                        self.stage = Stage::Step2(Step2Stage::Signed);
                    }
                }
                Task::none()
            }
            SplitEvent::Step2Submitted(_, Coord(coord), result) => {
                let recorded = coord.recorded_outcome();
                self.bind_coord(coord);
                match result {
                    Ok(outcome) => {
                        self.notice = None;
                        self.step2_outcome = Some(outcome);
                    }
                    Err(reason) => self.notice = Some(reason.reason),
                }
                // Once an intent is recorded, only reconcile.
                self.stage = if self.step2_outcome.is_some() || recorded.is_some() {
                    self.step2_outcome = self.step2_outcome.or(recorded);
                    Stage::Step2(Step2Stage::Submitted)
                } else {
                    Stage::Step2(Step2Stage::Signed)
                };
                Task::none()
            }
            SplitEvent::Step2Reconciled(_, Coord(coord), result, back) => {
                self.bind_coord(coord);
                let succeeded = result.is_ok();
                self.reconciled(result);
                self.stage = Stage::Step2(back);
                if succeeded && self.conflict_unread() {
                    return self.restart_step2(None);
                }
                Task::none()
            }
            SplitEvent::ReconReconciled(_, Recon(recon), result) => {
                self.bind_recon(recon);
                let succeeded = result.is_ok();
                self.reconciled(result);
                self.stage = Stage::Step2(Step2Stage::Reconcile);
                if succeeded && self.conflict_unread() {
                    return self.restart_step2(None);
                }
                Task::none()
            }
            SplitEvent::Step2ResendReviewed(_, Coord(coord), result) => {
                self.bind_coord(coord);
                match result {
                    Ok(view) => {
                        self.notice = None;
                        self.step2_resend = Some(view);
                    }
                    Err(reason) => {
                        self.step2_resend = None;
                        if reason.recovery == step2::Step2Recovery::Restart {
                            return self.restart_step2(Some(reason.reason));
                        }
                        self.notice = Some(reason.reason);
                    }
                }
                self.stage = Stage::Step2(Step2Stage::Reconcile);
                Task::none()
            }
            SplitEvent::Step2Resent(_, Coord(coord), result) => {
                // Whatever happened, the review was used up; anything but
                // the route's exact acceptance is uncertain again and only
                // reconciles, or is reviewed again.
                self.bind_coord(coord);
                self.step2_resend = None;
                match result {
                    Ok(outcome @ Outcome::UpstreamAccepted { .. }) => {
                        self.notice = None;
                        self.step2_outcome = Some(outcome);
                    }
                    // #648 X1b: an attempt that did not come back accepted
                    // (timed out or interrupted: no return is recorded, or
                    // the last one allowed) may leave the journal with no
                    // resend. Read it again, as a refusal that says so does,
                    // so a dead end is shown without another review.
                    Ok(outcome) => {
                        self.step2_outcome = Some(outcome);
                        return self.restart_step2(None);
                    }
                    Err(reason) if reason.recovery == step2::Step2Recovery::Restart => {
                        return self.restart_step2(Some(reason.reason));
                    }
                    Err(reason) => self.notice = Some(reason.reason),
                }
                self.stage = Stage::Step2(Step2Stage::Reconcile);
                Task::none()
            }
            SplitEvent::Step2ReconfirmationReviewed(_, held, result, back) => {
                self.bind_held(held);
                match result {
                    Ok(view) => {
                        self.notice = None;
                        // The new review moved the handle's revision.
                        self.step2_resend = None;
                        self.reconfirmation = Some(view);
                    }
                    // #658 P3-3: as the resend handler, a lost handle reads
                    // the journal again; a final refusal (S4-D4) withdraws
                    // the offer.
                    Err(reason) if reason.recovery == step2::Step2Recovery::Restart => {
                        return self.restart_step2(Some(reason.reason));
                    }
                    Err(reason) => {
                        self.reconfirmation = None;
                        self.reconfirmation_final |= !reason.retry;
                        self.notice = Some(reason.reason);
                    }
                }
                self.stage = Stage::Step2(back);
                Task::none()
            }
            SplitEvent::Step2Reconfirmed(_, held, result, back) => {
                self.bind_held(held);
                self.reconfirmation = None;
                self.stage = Stage::Step2(back);
                match result {
                    // The new block is step 1's recorded one now: reconcile
                    // at once, so what the panel shows of step 1 is read
                    // against it.
                    Ok(()) => {
                        self.notice = None;
                        self.update_step2(SplitMessage::Step2Reconcile)
                    }
                    Err(reason) if reason.recovery == step2::Step2Recovery::Restart => {
                        self.restart_step2(Some(reason.reason))
                    }
                    Err(reason) => {
                        self.reconfirmation_final |= !reason.retry;
                        self.notice = Some(reason.reason);
                        Task::none()
                    }
                }
            }
            SplitEvent::Step2Completed(_, Some(Recon(recon)), result) => {
                // The coordinator, if completion started from it, is gone:
                // the reconciler holds the journal now.
                self.bind_recon(recon);
                self.completable = false;
                match result {
                    Ok(completion) => {
                        self.notice = None;
                        self.replay = None;
                        // A later restart under this panel opens it in
                        // Completed (#568 B5c-1).
                        self.recorded_completion
                            .retain(|recorded| recorded.step2_txid != completion.step2_txid);
                        self.recorded_completion.push(completion.clone());
                        self.completion = Some(completion);
                        self.stage = Stage::Step2(Step2Stage::Completed);
                    }
                    Err(reason) if reason.recovery == step2::Step2Recovery::Restart => {
                        return self.restart_step2(Some(reason.reason));
                    }
                    // #645 P3-1: check again; nothing ends here.
                    Err(reason) => {
                        self.notice = Some(reason.reason);
                        self.stage = Stage::Step2(Step2Stage::Reconcile);
                    }
                }
                Task::none()
            }
            SplitEvent::Step2Completed(_, None, result) => {
                // No reconciler came back: read the journal again.
                let reason = match result {
                    Ok(_) => step2::COMPLETION_INTERRUPTED.to_string(),
                    Err(reason) => reason.reason,
                };
                self.restart_step2(Some(reason))
            }
            SplitEvent::Step2CompletionRechecked(_, Recon(recon), result) => {
                self.bind_recon(recon);
                match result {
                    Ok(step2::CompletionStanding::Standing { status, seen }) => {
                        self.step2_status = Some(status);
                        self.step2_seen = Some(seen);
                        self.step2_seen_here = Some(seen);
                        self.notice = None;
                        self.stage = Stage::Step2(Step2Stage::Completed);
                    }
                    // D17: the record is gone (or was never written); back
                    // to reconciling, the descriptors still deleted.
                    Ok(step2::CompletionStanding::Lost { status, seen, .. }) => {
                        if let Some(lost) = self.completion.as_ref() {
                            self.recorded_completion
                                .retain(|recorded| recorded.step2_txid != lost.step2_txid);
                        }
                        self.step2_status = Some(status);
                        self.step2_seen = Some(seen);
                        self.step2_seen_here = Some(seen);
                        self.completion = None;
                        self.completable = false;
                        self.notice = Some(step2::COMPLETION_LOST.to_string());
                        self.stage = Stage::Step2(Step2Stage::Reconcile);
                    }
                    Err(reason) => {
                        self.notice = Some(reason.reason);
                        self.stage = Stage::Step2(Step2Stage::Completed);
                    }
                }
                Task::none()
            }
            _ => Task::none(),
        }
    }
}

/// Every loaded file combined onto the built step 2. Each must be exactly
/// the built transaction; signature checks are the preparation's.
fn combine(base: &Psbt, files: &[Psbt]) -> Result<Psbt, Step2Refusal> {
    let mut combined = base.clone();
    for file in files {
        if file.unsigned_tx != base.unsigned_tx {
            return Err(Step2Refusal {
                reason: "A loaded file is not this split's step 2.".to_string(),
                retry: true,
                recovery: step2::Step2Recovery::None,
            });
        }
        combined.combine(file.clone()).map_err(|_| Step2Refusal {
            reason: "A loaded file could not be combined with the others.".to_string(),
            retry: true,
            recovery: step2::Step2Recovery::None,
        })?;
    }
    Ok(combined)
}
