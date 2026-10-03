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
    step1::{OpenRequest, Refusal, SplitConnect},
    step2::{self, ReconPort, Step2Open, Step2Port, Step2Refusal},
    Coord, Driver, Prep, Recon, Restarted, Seen, SplitEvent, SplitMessage, SplitPanel, Stage,
    Step2Stage, Work,
};
use crate::{
    app::message::Message,
    services::split_psbt_file::{self, Encoding},
};

fn refusal(refusal: Step2Refusal) -> Refusal {
    Refusal {
        reason: refusal.reason,
        retry: refusal.retry,
    }
}

impl SplitPanel {
    /// Install (or clear) the target Vault's step-2 port. The App builds a
    /// new port on every Connect refresh: an equivalent one (same session
    /// context and daemon instance) is ignored, so a flow in progress keeps
    /// its handles; any other (another daemon, account, provider or
    /// generation, or none) revokes every step-2 handle first (#637 F1).
    pub fn set_step2_port(&mut self, port: Option<Arc<dyn Step2Port>>) {
        let same = match (&self.step2_port, &port) {
            (Some(a), Some(b)) => a.identity() == b.identity(),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if self.prep.is_some() || self.coord.is_some() || self.recon.is_some() {
            self.revoke();
        }
        self.step2_port = port;
    }

    /// Install (or clear) the session's reconcile-only port (#637 R1). The
    /// App builds one on every Connect refresh, whether or not the Vault's
    /// daemon gives a step-2 port: an equivalent one (same session context)
    /// is ignored; any other revokes a reconciler opened under the old one.
    pub fn set_recon_port(&mut self, port: Option<Arc<dyn ReconPort>>) {
        let same = match (&self.recon_port, &port) {
            (Some(a), Some(b)) => a.context() == b.context(),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if self.recon.is_some() {
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
        self.target_index = None;
        self.step2_handoff_ready = false;
    }

    pub fn step2_available(&self) -> bool {
        self.step2_port.is_some()
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
    pub fn step2_outcome(&self) -> Option<crate::services::claim_coordinator::Outcome> {
        self.step2_outcome
    }
    pub fn step2_seen(&self) -> Option<crate::services::claim_observation::TransactionObservation> {
        self.step2_seen
    }
    /// The step-1 evidence of the last step-2 reconcile.
    pub fn step2_status(&self) -> Option<crate::services::claim_workflow::Status> {
        self.step2_status
    }
    /// What that evidence warns about (#637 r4172242637), derived from it
    /// rather than kept in the notice: an operation's notice (a saved file, a
    /// failed export or check, an ended session) never replaces it, and only
    /// new evidence changes it (#637 review 5971166062 F1).
    pub fn step2_warning(&self) -> Option<String> {
        self.step2_status.and_then(step2::reconcile_warning)
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
            SplitMessage::Step2Reconcile if self.stage == Stage::Step2(Step2Stage::Submitted) => {
                let Some((mut coord, connect)) = self.take_coord(Work::Step2Reconciling) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = coord.reconcile(&connect.context()).await;
                        (Coord(coord), result)
                    },
                    |seq, (coord, result)| SplitEvent::Step2Reconciled(seq, coord, result),
                )
            }
            SplitMessage::Step2Reconcile if self.stage == Stage::Step2(Step2Stage::Reconcile) => {
                let (Some(connect), Some(mut recon)) = (self.connect.clone(), self.recon.take())
                else {
                    return Task::none();
                };
                self.stage = Stage::Working(Work::Step2Reconciling);
                self.spawn(
                    async move {
                        let result = recon.reconcile(&connect.context()).await;
                        (Recon(recon), result)
                    },
                    |seq, (recon, result)| SplitEvent::ReconReconciled(seq, recon, result),
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
        if self.stage != Stage::Step2(Step2Stage::Sign) {
            return Task::none();
        }
        if self.step2_files.len().saturating_add(paths.len()) > split_psbt_file::MAX_COMBINED_FILES
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
                            for path in &paths {
                                let file = split_psbt_file::load(path)
                                    .map_err(|error| Step2Refusal::retry(error.to_string()))?;
                                let candidate = combine(&combined, std::slice::from_ref(&file))?;
                                // Verify even a no-op file before ignoring it. Only a
                                // validated new signature consumes a retained slot.
                                prep.verify_signed(&candidate, &coins)?;
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
    /// [`Self::step2_warning`], naming a reorg only for `Reorged`, and drops
    /// any "cannot replay" label. A failed check keeps the last evidence and
    /// its warning; the notice carries only the failure. Nothing new is
    /// offered: the only action after a step-2 submission is still to
    /// reconcile.
    fn reconciled(&mut self, result: Seen) {
        match result {
            Ok((status, seen)) => {
                self.step2_status = Some(status);
                self.step2_seen = Some(seen);
                self.notice = None;
            }
            Err(reason) => self.notice = Some(reason.reason),
        }
        if self.step2_warning().is_some() || self.notice.is_some() {
            self.replay = None;
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
            SplitEvent::Restarted(_, Ok(Restarted::Reconcile(Recon(recon)))) => {
                self.outcome = None;
                self.step2_outcome = recon.recorded_outcome();
                self.bind_recon(recon);
                // The last reconcile's step-1 evidence is kept through the
                // revocation along with its BTCB2 observation, so its
                // warning stays until a new reconcile replaces it
                // (#637 r4172729359).
                self.stage = Stage::Step2(Step2Stage::Reconcile);
                Task::none()
            }
            SplitEvent::Restarted(_, Err(reason)) => {
                self.stage = Stage::Refused(refusal(reason));
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
                // The step-1 driver was released: reopen from the journal.
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
                self.step2_handoff_ready = reason.retry;
                self.bind_prep(prep);
                self.notice = Some(reason.reason);
                self.stage = Stage::Step2(Step2Stage::Sign);
                Task::none()
            }
            SplitEvent::Step2Finished(_, Err((reason, None))) => {
                // The preparation released the journal: reopen it.
                self.stage = Stage::Refused(Refusal::retry(reason.reason));
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
            SplitEvent::Step2Reconciled(_, Coord(coord), result) => {
                self.bind_coord(coord);
                self.reconciled(result);
                self.stage = Stage::Step2(Step2Stage::Submitted);
                Task::none()
            }
            SplitEvent::ReconReconciled(_, Recon(recon), result) => {
                self.bind_recon(recon);
                self.reconciled(result);
                self.stage = Stage::Step2(Step2Stage::Reconcile);
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
