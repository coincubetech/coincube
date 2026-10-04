//! Split step 1 panel (#568 B1b), Vault-scoped in the target BTCB2 Cube.
//!
//! One panel, two ways in:
//! - [`SplitPanel::start`], a fresh split from Home's two-chain scan:
//!   preconditions → build → export the unsigned PSBT file → import and
//!   combine the signed files → finalize → record (journal) → review →
//!   submit (Connect only, D5) → track. Nothing in production calls it yet
//!   (D1): the review overlay stays read-only until B5 adds "Start split".
//! - [`SplitPanel::resume`], the only constructor production uses: a Split
//!   journal already exists in this Vault's `split/` directory (which no one
//!   can have before go-live). It rebuilds the recorded step 1 from freshly
//!   authenticated coins and resumes with the recorded signed bytes. A
//!   recorded submission is only reconciled, never retried; an unsubmitted
//!   one is reviewed again only on an explicit request, and may be abandoned
//!   after a chain check.
//!
//! Once step 1 is submitted the panel tracks it (#568 B2): each check shows
//! its Bitcoin confirmations as N of 6, the depth step 2 needs. A step 1 that
//! left its block blocks step 2 and is reviewed explicitly: re-mined in
//! another block, the user acknowledges the new block; dropped from the
//! chain, the exact recorded bytes may be sent again after a fresh preflight;
//! dropped and its coins spent elsewhere, a new step 1 is needed.
//!
//! Step 2 (#568 B3b-2b-2, [`panel2`]) continues the same resumed journal once
//! step 1 has six confirmations: the step-1 driver is released before the
//! step-2 preparation opens (and the preparation before a step-1 reorg
//! review), the target Vault reserves and proves its address, step 2 is built
//! under a fresh check, signed through PSBT files, handed to the submission
//! coordinator, reviewed (route label, node-route privacy note), submitted
//! once and reconciled. A restart after a recorded step-2 submission opens
//! only the reconciler, under the Connect session alone: the Vault's daemon
//! is needed only to enter step 2, and only on a route step 2 can be sent
//! through. It is still reachable only by resuming a journal (D1).
//!
//! The panel owns no keys and never signs: signatures come back in PSBT
//! files (D6). Every Connect read, build, file operation and journal call
//! runs in a task, off the UI thread. A session end (sign-out, account
//! change, Cube close) revokes the coordinator synchronously; a recorded
//! split survives on disk and continues under the next session.

mod panel2;
pub mod step1;
pub mod step2;

use std::{
    fmt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use iced::Task;

use coincube_core::{
    claim::{Assessment, BlockRef, MIN_CONFIRMATIONS},
    foreign_split::{SplitCoin, SplitStep1},
    miniscript::bitcoin::{
        consensus::encode::serialize_hex, hashes::sha256, psbt::Psbt, OutPoint, Transaction, Txid,
    },
};

use crate::{
    app::{message::Message, split_intent::SplitIntent},
    services::{
        claim_coordinator::Outcome,
        claim_observation::TransactionObservation,
        claim_workflow::{Phase, Status},
        split_psbt_file::{self, Encoding},
    },
};

use step1::{
    Imported, OpenRequest, Prepared, Recovery, Refusal, ReviewView, RevokeHandle, SplitConnect,
    Step1Driver,
};

/// What the panel is doing or waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// Waiting for a Connect session to work under.
    NeedsSession,
    /// A task is running.
    Working(Work),
    /// Built: export the unsigned PSBT, then import the signed file(s).
    Sign,
    /// Recorded, nothing submitted: review on request.
    Ready,
    /// A review is on screen; confirming submits exactly it.
    Review,
    /// A submission may exist: reconcile only.
    Tracking,
    /// Step 1 was mined again in another block: acknowledge it.
    Reconfirm {
        previous: BlockRef,
        confirmed: BlockRef,
    },
    /// Step 1 was dropped from the chain: a review of exactly the recorded
    /// bytes is on screen; confirming sends them again.
    Resend,
    Refused(Refusal),
    /// The unsubmitted journal was deleted.
    Abandoned,
    /// #625 F2 (A1 = A): closed in its step-2 dead end. The journal is kept
    /// with its tombstone; nothing more happens here.
    Closed,
    /// Step 2 (B3b-2b): a step-2 handle holds the journal.
    Step2(Step2Stage),
}

/// Where step 2 is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step2Stage {
    /// The preparation is open: check, reserve the target, build.
    Ready,
    /// Built: export the unsigned PSBT, then import the signed file(s).
    Sign,
    /// Signed and handed to the coordinator: review on request.
    Signed,
    /// A step-2 review is on screen; confirming submits exactly it.
    Review,
    /// A step-2 submission may exist: reconcile only.
    Submitted,
    /// Restarted after a recorded step-2 submission: reconcile only.
    Reconcile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    Checking,
    Building,
    Exporting,
    Importing,
    Recording,
    Restoring,
    Reviewing,
    Submitting,
    Reconciling,
    Recovering,
    Acknowledging,
    Resending,
    CheckingAbandon,
    Abandoning,
    /// #625 F2: a step-2 dead end checked on both chains, then closed.
    CheckingClose,
    Closing,
    /// Step 2.
    Restarting,
    Entering,
    Leaving,
    Step2Checking,
    /// N4: the Vault reserves and Connect proves a fresh address.
    Reserving,
    Step2Building,
    Step2Exporting,
    Step2Importing,
    Finishing,
    Step2Reviewing,
    Step2Submitting,
    Step2Reconciling,
}

/// The coordinator in transit between the panel and a task.
pub struct Driver(pub Box<dyn Step1Driver>);
impl fmt::Debug for Driver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Driver").field(&self.0.phase()).finish()
    }
}

/// A journal rebuilt and resumed at restart.
#[derive(Debug)]
pub struct Resumed {
    pub construction: SplitStep1,
    pub signed: Transaction,
    pub phase: Phase,
    pub claimed: Vec<(OutPoint, String)>,
    /// The claimed coins as authenticated: step 2 is built from them.
    pub coins: Vec<SplitCoin>,
}

/// Step-2 handles in transit between the panel and a task.
pub struct Prep(pub Box<dyn step2::Step2Prep>);
pub struct Coord(pub Box<dyn step2::Step2Coord>);
pub struct Recon(pub Box<dyn step2::Step2Recon>);
impl fmt::Debug for Prep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Prep")
    }
}
impl fmt::Debug for Coord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Coord")
    }
}
impl fmt::Debug for Recon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Recon")
    }
}
/// What a restart opened.
#[derive(Debug)]
pub enum Restarted {
    Step1,
    /// The reconciler, with the journal's step-2 dead end if it is in one.
    Reconcile(Recon, Option<step2::DeadEnd>),
    /// #625 F2: closed in its step-2 dead end.
    Closed,
}
/// A step-2 handoff refused, with the preparation when still usable.
pub type FinishResult = Result<Coord, (step2::Step2Refusal, Option<Prep>)>;
/// A step-2 reconcile's result.
pub type Seen = Result<(Status, TransactionObservation), step2::Step2Refusal>;

/// Why a restart did not resume, with what it still has.
#[derive(Debug)]
pub struct Unresumed {
    pub refusal: Refusal,
    /// What was rebuilt before the coordinator refused, if anything.
    pub resumed: Option<Box<Resumed>>,
    /// The rebuild itself was refused finally (#625 F2): the journal read
    /// again for abandonment only, or why that is not possible either.
    pub abandon: Option<Result<step1::AbandonOnly, Refusal>>,
}

/// A resumed journal and its coordinator, or why not.
pub type ResumeResult = Result<(Box<Resumed>, Driver), Box<Unresumed>>;

/// Results of the panel's tasks. Each carries the sequence number of the
/// request that started it; a stale result is dropped (and any coordinator
/// it carries released).
#[derive(Debug)]
pub enum SplitEvent {
    Checked(u64, Result<Box<Prepared>, Refusal>),
    Built(u64, Result<Box<SplitStep1>, String>),
    Exported(u64, Result<Option<PathBuf>, String>),
    Imported(u64, Result<(Vec<Psbt>, Imported), String>),
    Recorded(u64, Result<Driver, String>),
    Resumed(u64, ResumeResult),
    Reviewed(u64, Driver, Result<ReviewView, String>),
    Submitted(u64, Driver, Result<Outcome, String>),
    Reconciled(u64, Driver, Result<Status, String>),
    Recovered(u64, Driver, Recovered),
    Acknowledged(u64, Driver, Result<(), String>),
    Resent(u64, Driver, Result<Outcome, String>),
    AbandonChecked(u64, Result<(), Refusal>),
    Abandoned(u64, Result<(), String>),
    /// #625 F2: a step-2 dead end closed, or why not.
    Closed(u64, Result<(), String>),
    SignedExported(u64, Result<Option<PathBuf>, String>),
    /// A file dialog answered (`None`: cancelled).
    ExportChosen(u64, Option<PathBuf>, Encoding),
    ImportChosen(u64, Option<Vec<PathBuf>>),
    SignedExportChosen(u64, Option<PathBuf>),
    /// Step 2.
    Restarted(u64, Result<Restarted, step2::Step2Refusal>),
    Step2Entered(u64, Result<Prep, step2::Step2Refusal>),
    Step2Left(u64, Result<Driver, Refusal>),
    Step2Checked(u64, Prep, Result<step2::CannotReplay, step2::Step2Refusal>),
    Step2Reserved(u64, Prep, Result<u32, step2::Step2Refusal>),
    Step2Built(u64, Prep, Result<Psbt, step2::Step2Refusal>),
    Step2ExportChosen(u64, Option<PathBuf>, Encoding),
    Step2Exported(u64, Result<Option<PathBuf>, String>),
    Step2ImportChosen(u64, Option<Vec<PathBuf>>),
    /// The loaded files, their combination, and whether it is complete.
    Step2Imported(
        u64,
        Option<Prep>,
        Result<(Vec<Psbt>, Psbt, bool), step2::Step2Refusal>,
    ),
    Step2Finished(u64, FinishResult),
    Step2Reviewed(
        u64,
        Coord,
        Result<step2::Step2ReviewView, step2::Step2Refusal>,
    ),
    Step2Submitted(u64, Coord, Result<Outcome, step2::Step2Refusal>),
    Step2Reconciled(u64, Coord, Seen),
    ReconReconciled(u64, Recon, Seen),
}

/// What a reorg check concluded.
#[derive(Debug)]
pub enum Recovered {
    Review(Recovery),
    /// Dropped, and a claimed coin spent on Bitcoin by another transaction.
    NewStep1Needed,
    Refused(String),
}

/// User intents. There is deliberately no "start" message: a fresh split is
/// not reachable from the GUI before B5.
#[derive(Debug, Clone)]
pub enum SplitMessage {
    Retry,
    ExportUnsigned(Encoding),
    ImportSigned,
    Review,
    Confirm,
    Reconcile,
    /// After a check found step 1 reorged: review what happened.
    CheckReorg,
    /// Acknowledge the block step 1 was mined again in.
    AcknowledgeReconfirmation,
    /// Send exactly the recorded step 1 again, as reviewed.
    ConfirmResend,
    CheckAbandon,
    ConfirmAbandon,
    ExportSigned,
    /// Cancel: revoke the coordinator and hide the panel until the Cube is
    /// opened again. Nothing recorded is deleted.
    Close,
    /// Step 2: release the step-1 driver and open the step-2 preparation.
    EnterStep2,
    /// Step 2: back to the step-1 driver (for a reorg review).
    LeaveStep2,
    Step2Check,
    /// N4: reserve and prove the Vault's fresh address.
    Step2Reserve,
    Step2Build,
    Step2Export(Encoding),
    Step2Import,
    Step2Review,
    Step2Confirm,
    Step2Reconcile,
}

pub struct SplitPanel {
    target_cube: String,
    journal_root: PathBuf,
    connect: Option<Arc<dyn SplitConnect>>,
    seq: u64,
    stage: Stage,
    hidden: bool,
    /// Fresh start only.
    intent: Option<Arc<SplitIntent>>,
    prepared: Option<Box<Prepared>>,
    construction: Option<Box<SplitStep1>>,
    files: Vec<Psbt>,
    exported: Option<PathBuf>,
    /// The verified signed step 1, kept through every refusal for export.
    signed: Option<Transaction>,
    /// This panel's journal: source digest and directory.
    journal: Option<(sha256::Hash, PathBuf)>,
    driver: Option<Box<dyn Step1Driver>>,
    revoke: Option<RevokeHandle>,
    phase: Option<Phase>,
    review: Option<ReviewView>,
    outcome: Option<Outcome>,
    status: Option<Status>,
    /// Claimed prevouts and their Bitcoin addresses, for the abandon check.
    claimed: Vec<(OutPoint, String)>,
    abandon_checked: bool,
    /// An unsubmitted journal that can't be rebuilt, read for abandonment
    /// only (#625 F2). Never alongside a construction or a driver.
    abandon_only: Option<step1::AbandonOnly>,
    /// The reopened journal's step-2 dead end, which may be closed after a
    /// check (#625 F2).
    dead_end: Option<step2::DeadEnd>,
    /// The session flag of a confirmed abandon or close in flight: a
    /// revocation sets it, and the task refuses under the journal's lock
    /// before deleting or closing anything (#644 r4176212750).
    ending: Option<Arc<AtomicBool>>,
    /// The last refusal while the flow keeps its state. Never the step-1
    /// evidence's warning after the step-2 submission: that is derived from
    /// `step2_status` ([`Self::step2_warning`]) so no notice replaces it.
    notice: Option<String>,
    /// The stage to return to after a check.
    resume_stage: Option<Stage>,
    /// Step 2 (B3b-2b): the target Vault's port, when its daemon is loaded
    /// on a route step 2 can be sent through.
    step2_port: Option<Arc<dyn step2::Step2Port>>,
    /// The session's reconcile-only port, whatever the daemon (#637 R1).
    recon_port: Option<Arc<dyn step2::ReconPort>>,
    prep: Option<Box<dyn step2::Step2Prep>>,
    coord: Option<Box<dyn step2::Step2Coord>>,
    recon: Option<Box<dyn step2::Step2Recon>>,
    step2_revoke: Option<RevokeHandle>,
    /// The live "cannot replay" label from the last successful check.
    replay: Option<step2::CannotReplay>,
    target_index: Option<u32>,
    step2_psbt: Option<Psbt>,
    step2_files: Vec<Psbt>,
    step2_exported: Option<PathBuf>,
    step2_review: Option<step2::Step2ReviewView>,
    step2_outcome: Option<Outcome>,
    step2_seen: Option<TransactionObservation>,
    /// The step-1 evidence of the last step-2 reconcile (#637 r4172242637).
    step2_status: Option<Status>,
    /// The authenticated claimed coins from the restore.
    coins: Vec<SplitCoin>,
}

impl fmt::Debug for SplitPanel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SplitPanel")
            .field("stage", &self.stage)
            .field("phase", &self.phase)
            .field("bound", &self.driver.is_some())
            .finish_non_exhaustive()
    }
}

impl SplitPanel {
    fn empty(target_cube: String, journal_root: PathBuf) -> Self {
        Self {
            target_cube,
            journal_root,
            connect: None,
            seq: 0,
            stage: Stage::NeedsSession,
            hidden: false,
            intent: None,
            prepared: None,
            construction: None,
            files: Vec::new(),
            exported: None,
            signed: None,
            journal: None,
            driver: None,
            revoke: None,
            phase: None,
            review: None,
            outcome: None,
            status: None,
            claimed: Vec::new(),
            abandon_checked: false,
            abandon_only: None,
            dead_end: None,
            ending: None,
            notice: None,
            resume_stage: None,
            step2_port: None,
            recon_port: None,
            prep: None,
            coord: None,
            recon: None,
            step2_revoke: None,
            replay: None,
            target_index: None,
            step2_psbt: None,
            step2_files: Vec::new(),
            step2_exported: None,
            step2_review: None,
            step2_outcome: None,
            step2_seen: None,
            step2_status: None,
            coins: Vec::new(),
        }
    }

    /// A fresh split of the scanned wallet into `target_cube`'s Vault. No
    /// production caller before B5 (D1).
    #[allow(dead_code)] // B5 adds the one "Start split" caller.
    pub(crate) fn start(target_cube: String, journal_root: PathBuf, intent: SplitIntent) -> Self {
        let mut panel = Self::empty(target_cube, journal_root);
        panel.intent = Some(Arc::new(intent));
        panel
    }

    /// Resume the existing journal `directory` (source `digest`). The only
    /// constructor reachable in production, from journal discovery.
    pub fn resume(
        target_cube: String,
        journal_root: PathBuf,
        digest: sha256::Hash,
        directory: PathBuf,
    ) -> Self {
        let mut panel = Self::empty(target_cube, journal_root);
        panel.journal = Some((digest, directory));
        panel
    }

    pub fn stage(&self) -> &Stage {
        &self.stage
    }
    pub fn is_hidden(&self) -> bool {
        self.hidden
    }
    pub fn phase(&self) -> Option<Phase> {
        self.phase
    }
    pub fn construction(&self) -> Option<&SplitStep1> {
        self.construction.as_deref()
    }
    pub fn prepared(&self) -> Option<&Prepared> {
        self.prepared.as_deref()
    }
    pub fn signed(&self) -> Option<&Transaction> {
        self.signed.as_ref()
    }
    pub fn tracked_txid(&self) -> Option<Txid> {
        self.signed.as_ref().map(Transaction::compute_txid)
    }
    pub fn review(&self) -> Option<&ReviewView> {
        self.review.as_ref()
    }
    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }
    pub fn status(&self) -> Option<Status> {
        self.status
    }
    /// Step 1's Bitcoin confirmations at the last check, when it saw a
    /// depth: 0 unconfirmed, then N of [`MIN_CONFIRMATIONS`] (capped).
    pub fn confirmations(&self) -> Option<u64> {
        match self.status? {
            Status::Observation(Assessment::WaitingForConfirmation) => Some(0),
            Status::Observation(Assessment::WaitingForDepth { confirmations }) => {
                Some(confirmations.min(MIN_CONFIRMATIONS))
            }
            Status::Observation(Assessment::ObservationsEligibleForPreflight) => {
                Some(MIN_CONFIRMATIONS)
            }
            _ => None,
        }
    }
    /// The last check found step 1 out of the block it was confirmed in:
    /// step 2 is blocked until the reorg is reviewed.
    pub fn reorged(&self) -> bool {
        self.status == Some(Status::Observation(Assessment::Reorged))
    }
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
    pub fn files(&self) -> usize {
        self.files.len()
    }
    pub fn exported(&self) -> Option<&PathBuf> {
        self.exported.as_ref()
    }
    pub fn journal_directory(&self) -> Option<&PathBuf> {
        self.journal.as_ref().map(|(_, directory)| directory)
    }
    pub fn journal_root(&self) -> &PathBuf {
        &self.journal_root
    }
    pub fn is_bound(&self) -> bool {
        self.driver.is_some()
    }
    /// An unsubmitted journal that can't be rebuilt, offered for abandonment
    /// only (#625 F2).
    pub fn abandon_only(&self) -> Option<&step1::AbandonOnly> {
        self.abandon_only.as_ref()
    }
    /// The step 1 an abandon check looks for on Bitcoin: the signed one, or
    /// for a journal that can't be rebuilt the recorded txid.
    fn abandon_tracked(&self) -> Option<Txid> {
        self.tracked_txid()
            .or_else(|| self.abandon_only.as_ref().map(|only| only.tracked))
    }
    /// Abandon is offered only for an unsubmitted journal, and confirmed only
    /// after a chain check passed.
    pub fn can_check_abandon(&self) -> bool {
        self.phase == Some(Phase::Intent)
            && !self.claimed.is_empty()
            && self.abandon_tracked().is_some()
            && matches!(self.stage, Stage::Ready | Stage::Review | Stage::Refused(_))
    }
    pub fn can_confirm_abandon(&self) -> bool {
        self.can_check_abandon() && self.abandon_checked
    }
    /// The reopened journal's step-2 dead end (#625 F2).
    pub fn dead_end(&self) -> Option<&step2::DeadEnd> {
        self.dead_end.as_ref()
    }
    /// #625 F2: a step-2 dead end may be closed from the reconcile-only
    /// stage, once a reconcile saw step 2 absent from BTCB2 (an accepted
    /// send may still be in a mempool), and only after a check on both
    /// chains passed.
    pub fn can_check_close(&self) -> bool {
        self.dead_end.is_some()
            && self.connect.is_some()
            && self.step2_seen == Some(TransactionObservation::Absent)
            && self.stage == Stage::Step2(Step2Stage::Reconcile)
    }
    pub fn can_confirm_close(&self) -> bool {
        self.can_check_close() && self.abandon_checked
    }

    /// Install (or clear) the Connect session. A different session revokes
    /// the coordinator first; the journal stays and continues under the next
    /// session through [`Self::begin`].
    pub fn set_connect(&mut self, connect: Option<Arc<dyn SplitConnect>>) {
        let same = match (&self.connect, &connect) {
            (Some(a), Some(b)) => a.context() == b.context(),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.revoke();
        }
        self.connect = connect;
    }

    /// Synchronously revoke any coordinator and drop in-flight results. A
    /// recorded split is kept on disk; an unrecorded one keeps its
    /// construction, files and signed transaction in memory.
    pub fn revoke(&mut self) {
        if let Some(revoke) = self.revoke.take() {
            revoke();
        }
        self.revoke_step2();
        self.driver = None;
        self.review = None;
        self.abandon_checked = false;
        // Read under this session: the next one reads the journal again.
        self.abandon_only = None;
        self.dead_end = None;
        if let Some(ending) = self.ending.take() {
            ending.store(true, Ordering::SeqCst);
        }
        self.seq = self.seq.wrapping_add(1);
        if self.journal.is_some() && !matches!(self.stage, Stage::Abandoned | Stage::Closed) {
            self.stage = Stage::NeedsSession;
            self.notice = Some(
                "The split session ended. It is recorded on this device and continues after you sign in again.".to_string(),
            );
        } else if self.connect.is_some() && self.intent.is_some() {
            self.stage = Stage::NeedsSession;
        }
    }

    /// Start or continue under the current session: restore an existing
    /// journal, or run the fresh preconditions. Never submits.
    pub fn begin(&mut self) -> Task<Message> {
        if self.hidden {
            return Task::none();
        }
        let Some(connect) = self.connect.clone() else {
            self.stage = Stage::NeedsSession;
            return Task::none();
        };
        if self.stage != Stage::NeedsSession {
            return Task::none();
        }
        self.notice = None;
        if let Some((digest, directory)) = self.journal.clone() {
            // A recorded step-2 submission reopens only the reconciler. The
            // decision needs only the session, never the Vault daemon, so a
            // daemon that is unloaded or on an unsupported route can't send
            // a recorded step 2 back to step 1 (#637 R1).
            self.stage = Stage::Working(Work::Restarting);
            let target = self.target_cube.clone();
            let (context, recon) = (connect.context(), self.recon_port.clone());
            return self.spawn(
                async move {
                    step2::restart(context, recon, directory, target, digest)
                        .await
                        .map(|restart| match restart {
                            step2::Restart::Step1 => Restarted::Step1,
                            step2::Restart::Reconcile(recon, dead_end) => {
                                Restarted::Reconcile(Recon(recon), dead_end)
                            }
                            step2::Restart::Closed => Restarted::Closed,
                        })
                },
                SplitEvent::Restarted,
            );
        }
        self.resume_journal(connect)
    }

    /// Resume the journal's step 1 (the step-1 driver).
    fn resume_journal(&mut self, connect: Arc<dyn SplitConnect>) -> Task<Message> {
        if let Some((digest, directory)) = self.journal.clone() {
            self.stage = Stage::Working(Work::Restoring);
            let target = self.target_cube.clone();
            return self.spawn(
                async move { resume(connect, directory, target, digest).await },
                SplitEvent::Resumed,
            );
        }
        if self.construction.is_some() {
            // A session ended before recording: the construction and any
            // signatures are kept; continue signing or record again.
            self.stage = Stage::Sign;
            return self.maybe_record();
        }
        let Some(intent) = self.intent.clone() else {
            return Task::none();
        };
        self.stage = Stage::Working(Work::Checking);
        self.spawn(
            async move { step1::preconditions(&*connect, &intent).await.map(Box::new) },
            SplitEvent::Checked,
        )
    }

    fn spawn<T: Send + 'static>(
        &mut self,
        future: impl std::future::Future<Output = T> + Send + 'static,
        wrap: impl Fn(u64, T) -> SplitEvent + Send + 'static,
    ) -> Task<Message> {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        Task::perform(future, move |result| {
            Message::Split(Box::new(wrap(seq, result)))
        })
    }

    fn take_driver(&mut self, work: Work) -> Option<(Box<dyn Step1Driver>, Arc<dyn SplitConnect>)> {
        let connect = self.connect.clone()?;
        let driver = self.driver.take()?;
        self.stage = Stage::Working(work);
        Some((driver, connect))
    }

    /// Export the unsigned construction to `path`.
    pub fn export_to(&mut self, path: PathBuf, encoding: Encoding) -> Task<Message> {
        let Some(construction) = self.construction.clone() else {
            return Task::none();
        };
        self.stage = Stage::Working(Work::Exporting);
        self.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    split_psbt_file::save(&path, &construction, encoding)
                        .map(|()| Some(path))
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|_| "The export was interrupted.".to_string())?
            },
            SplitEvent::Exported,
        )
    }

    /// Load the signed files at `paths`, combine them with those already
    /// loaded and finalize when every input is satisfied.
    pub fn import_from(&mut self, paths: Vec<PathBuf>) -> Task<Message> {
        let Some(construction) = self.construction.clone() else {
            return Task::none();
        };
        let mut files = self.files.clone();
        self.stage = Stage::Working(Work::Importing);
        self.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    for path in &paths {
                        files.push(split_psbt_file::load(path).map_err(|error| error.to_string())?);
                    }
                    let imported =
                        step1::import(&construction, &files).map_err(|error| error.to_string())?;
                    Ok((files, imported))
                })
                .await
                .map_err(|_| "The import was interrupted.".to_string())?
            },
            SplitEvent::Imported,
        )
    }

    /// Save the verified signed step 1 as raw transaction hex, e.g. after a
    /// refusal, so it is never lost.
    pub fn export_signed_to(&mut self, path: PathBuf) -> Task<Message> {
        let Some(signed) = self.signed.clone() else {
            return Task::none();
        };
        self.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    std::fs::write(&path, serialize_hex(&signed))
                        .map(|()| Some(path))
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|_| "The export was interrupted.".to_string())?
            },
            SplitEvent::SignedExported,
        )
    }

    /// Record the verified step 1 once the imported files satisfy it.
    fn maybe_record(&mut self) -> Task<Message> {
        let (Some(construction), Some(connect)) = (self.construction.clone(), self.connect.clone())
        else {
            return Task::none();
        };
        if self.signed.is_none() || self.files.is_empty() {
            return Task::none();
        }
        let files = self.files.clone();
        let fork_height = construction.fork_height();
        let digest = construction.source().digest();
        let directory = step1::journal_directory(&self.journal_root, digest);
        let target_cube = self.target_cube.clone();
        self.stage = Stage::Working(Work::Recording);
        self.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    // Re-finalized from the kept files: the verified value is
                    // consumed by the coordinator and has no Clone.
                    let verified = match step1::import(&construction, &files) {
                        Ok(Imported::Complete(verified, _)) => *verified,
                        _ => return Err("The signed files no longer finalize.".to_string()),
                    };
                    connect
                        .open(OpenRequest {
                            directory,
                            target_cube,
                            construction: *construction,
                            verified,
                            fork_height,
                            resume: false,
                        })
                        .map(Driver)
                        .map_err(step1::describe)
                })
                .await
                .map_err(|_| "Recording the split was interrupted.".to_string())?
            },
            SplitEvent::Recorded,
        )
    }

    fn bind(&mut self, driver: Box<dyn Step1Driver>) {
        self.revoke = Some(driver.revoke_handle());
        self.phase = Some(driver.phase());
        self.driver = Some(driver);
    }

    fn settle(&mut self) {
        self.stage = match self.phase {
            Some(Phase::Intent) => Stage::Ready,
            Some(_) => Stage::Tracking,
            None => Stage::Sign,
        };
    }

    pub fn update(&mut self, message: SplitMessage) -> Task<Message> {
        let seq = self.seq;
        match message {
            SplitMessage::Retry => {
                if matches!(self.stage, Stage::Refused(Refusal { retry: true, .. })) {
                    self.stage = Stage::NeedsSession;
                }
                self.begin()
            }
            SplitMessage::ExportUnsigned(encoding) if self.stage == Stage::Sign => {
                let name = match encoding {
                    Encoding::Binary => "split-step1.psbt",
                    Encoding::Base64 => "split-step1.txt",
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
                        Message::Split(Box::new(SplitEvent::ExportChosen(seq, path, encoding)))
                    },
                )
            }
            SplitMessage::ImportSigned if self.stage == Stage::Sign => Task::perform(
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
                move |paths| Message::Split(Box::new(SplitEvent::ImportChosen(seq, paths))),
            ),
            SplitMessage::ExportSigned if self.signed.is_some() => Task::perform(
                async move {
                    rfd::AsyncFileDialog::new()
                        .set_file_name("split-step1-signed.txt")
                        .save_file()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                move |path| Message::Split(Box::new(SplitEvent::SignedExportChosen(seq, path))),
            ),
            SplitMessage::Review if matches!(self.stage, Stage::Ready | Stage::Review) => {
                self.review = None;
                let Some(destination) = self
                    .construction
                    .as_deref()
                    .and_then(step1::construction_destination)
                else {
                    return Task::none();
                };
                let Some((mut driver, connect)) = self.take_driver(Work::Reviewing) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        // The destination must still be unused on both
                        // chains when the review is shown.
                        let result = match step1::destination_unused(&*connect, &destination).await
                        {
                            Ok(()) => driver
                                .review(&connect.context())
                                .await
                                .map_err(step1::describe),
                            Err(refusal) => Err(refusal.reason),
                        };
                        (Driver(driver), result)
                    },
                    |seq, (driver, result)| SplitEvent::Reviewed(seq, driver, result),
                )
            }
            SplitMessage::Confirm if self.stage == Stage::Review => {
                self.review = None;
                let Some((mut driver, connect)) = self.take_driver(Work::Submitting) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = driver
                            .submit(&connect.context())
                            .await
                            .map_err(step1::describe);
                        (Driver(driver), result)
                    },
                    |seq, (driver, result)| SplitEvent::Submitted(seq, driver, result),
                )
            }
            SplitMessage::Reconcile
                if matches!(
                    self.stage,
                    Stage::Tracking | Stage::Ready | Stage::Reconfirm { .. } | Stage::Resend
                ) =>
            {
                self.review = None;
                let Some((mut driver, connect)) = self.take_driver(Work::Reconciling) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = driver
                            .reconcile(&connect.context())
                            .await
                            .map_err(step1::describe);
                        (Driver(driver), result)
                    },
                    |seq, (driver, result)| SplitEvent::Reconciled(seq, driver, result),
                )
            }
            SplitMessage::CheckReorg if self.stage == Stage::Tracking && self.reorged() => {
                let (Some(tracked), claimed) = (self.tracked_txid(), self.claimed.clone()) else {
                    return Task::none();
                };
                let Some((mut driver, connect)) = self.take_driver(Work::Recovering) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let recovered = match driver.recover(&connect.context()).await {
                            Ok(recovery) => Recovered::Review(recovery),
                            Err(error) => {
                                let reason = step1::describe(error);
                                match step1::step1_double_spent(&*connect, tracked, &claimed).await
                                {
                                    Ok(true) => Recovered::NewStep1Needed,
                                    Ok(false) => Recovered::Refused(reason),
                                    Err(refusal) => Recovered::Refused(refusal.reason),
                                }
                            }
                        };
                        (Driver(driver), recovered)
                    },
                    |seq, (driver, recovered)| SplitEvent::Recovered(seq, driver, recovered),
                )
            }
            SplitMessage::AcknowledgeReconfirmation
                if matches!(self.stage, Stage::Reconfirm { .. }) =>
            {
                let Some((mut driver, connect)) = self.take_driver(Work::Acknowledging) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = driver
                            .acknowledge(&connect.context())
                            .await
                            .map_err(step1::describe);
                        (Driver(driver), result)
                    },
                    |seq, (driver, result)| SplitEvent::Acknowledged(seq, driver, result),
                )
            }
            SplitMessage::ConfirmResend if self.stage == Stage::Resend => {
                self.review = None;
                let Some((mut driver, connect)) = self.take_driver(Work::Resending) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = driver
                            .resend(&connect.context())
                            .await
                            .map_err(step1::describe);
                        (Driver(driver), result)
                    },
                    |seq, (driver, result)| SplitEvent::Resent(seq, driver, result),
                )
            }
            SplitMessage::CheckAbandon if self.can_check_abandon() => {
                let (Some(connect), Some(tracked)) = (self.connect.clone(), self.abandon_tracked())
                else {
                    return Task::none();
                };
                let claimed = self.claimed.clone();
                self.abandon_checked = false;
                let back =
                    std::mem::replace(&mut self.stage, Stage::Working(Work::CheckingAbandon));
                self.resume_stage = Some(back);
                self.spawn(
                    async move { step1::check_abandon(&*connect, tracked, &claimed).await },
                    SplitEvent::AbandonChecked,
                )
            }
            SplitMessage::ConfirmAbandon if self.can_confirm_abandon() => {
                let (Some(connect), Some(tracked), Some((digest, directory))) = (
                    self.connect.clone(),
                    self.abandon_tracked(),
                    self.journal.clone(),
                ) else {
                    return Task::none();
                };
                // Release the coordinator and its journal lock first.
                if let Some(revoke) = self.revoke.take() {
                    revoke();
                }
                self.driver = None;
                self.abandon_checked = false;
                let claimed = self.claimed.clone();
                let target = self.target_cube.clone();
                let ended = Arc::new(AtomicBool::new(false));
                self.ending = Some(ended.clone());
                self.stage = Stage::Working(Work::Abandoning);
                self.spawn(
                    async move {
                        step1::check_abandon(&*connect, tracked, &claimed)
                            .await
                            .map_err(|refusal| refusal.reason)?;
                        let context = connect.context();
                        tokio::task::spawn_blocking(move || {
                            step1::abandon(&directory, &target, digest, context, &ended).map_err(
                                |error| match error {
                                    crate::services::claim_workflow::Error::Revoked => {
                                        step1::ENDED_BEFORE_ABANDON.to_string()
                                    }
                                    error => {
                                        format!("The split could not be abandoned ({error:?}).")
                                    }
                                },
                            )
                        })
                        .await
                        .map_err(|_| "Abandoning was interrupted.".to_string())?
                    },
                    SplitEvent::Abandoned,
                )
            }
            SplitMessage::Close => {
                // Cancel: release the coordinator and hide. A recorded split
                // stays on disk and continues the next time the Cube opens.
                self.revoke();
                self.hidden = true;
                Task::none()
            }
            other => self.update_step2(other),
        }
    }

    /// Apply a task's result. A result from an older request is dropped; a
    /// coordinator it carries is released (it was revoked with that request).
    pub fn apply(&mut self, event: SplitEvent) -> Task<Message> {
        if event.seq() != self.seq {
            return Task::none();
        }
        match event {
            SplitEvent::Checked(_, Ok(prepared)) => {
                self.stage = Stage::Working(Work::Building);
                let for_build = prepared.clone();
                self.prepared = Some(prepared);
                self.spawn(
                    async move {
                        tokio::task::spawn_blocking(move || step1::build(&for_build).map(Box::new))
                            .await
                            .map_err(|_| "Building step 1 was interrupted.".to_string())?
                    },
                    SplitEvent::Built,
                )
            }
            SplitEvent::Checked(_, Err(refusal)) => {
                self.stage = Stage::Refused(refusal);
                Task::none()
            }
            SplitEvent::Built(_, Ok(construction)) => {
                self.construction = Some(construction);
                self.files.clear();
                self.signed = None;
                self.stage = Stage::Sign;
                Task::none()
            }
            SplitEvent::Built(_, Err(reason)) => {
                self.stage = Stage::Refused(Refusal::final_(reason));
                Task::none()
            }
            SplitEvent::ExportChosen(_, Some(path), encoding) => self.export_to(path, encoding),
            SplitEvent::ImportChosen(_, Some(paths)) if !paths.is_empty() => {
                self.import_from(paths)
            }
            SplitEvent::SignedExportChosen(_, Some(path)) => self.export_signed_to(path),
            SplitEvent::ExportChosen(..)
            | SplitEvent::ImportChosen(..)
            | SplitEvent::SignedExportChosen(..) => Task::none(),
            SplitEvent::Exported(_, result) => {
                match result {
                    Ok(path) => self.exported = path.or(self.exported.take()),
                    Err(reason) => self.notice = Some(reason),
                }
                self.stage = Stage::Sign;
                Task::none()
            }
            SplitEvent::Imported(_, Ok((files, imported))) => {
                self.files = files;
                self.notice = None;
                match imported {
                    Imported::Complete(verified, _) => {
                        self.signed = Some(verified.transaction().clone());
                        self.maybe_record()
                    }
                    Imported::Partial => {
                        self.notice = Some(
                            "Signatures loaded. More are needed: import the other signers' files."
                                .to_string(),
                        );
                        self.stage = Stage::Sign;
                        Task::none()
                    }
                }
            }
            SplitEvent::Imported(_, Err(reason)) => {
                self.notice = Some(reason);
                self.stage = Stage::Sign;
                Task::none()
            }
            SplitEvent::Recorded(_, Ok(Driver(driver))) => {
                if let Some(construction) = &self.construction {
                    let digest = construction.source().digest();
                    self.journal =
                        Some((digest, step1::journal_directory(&self.journal_root, digest)));
                    self.claimed = step1::claimed_addresses(construction);
                }
                self.notice = None;
                self.bind(driver);
                self.settle();
                Task::none()
            }
            SplitEvent::Recorded(_, Err(reason)) => {
                // The journal may have been written before the coordinator
                // refused: continue from it, never from a second record.
                if let Some(construction) = &self.construction {
                    let digest = construction.source().digest();
                    if let Some(found) = step1::discover(&self.journal_root)
                        .into_iter()
                        .find(|(found, _)| *found == digest)
                    {
                        self.journal = Some(found);
                    }
                }
                self.stage = Stage::Refused(Refusal::retry(reason));
                Task::none()
            }
            SplitEvent::Resumed(_, Ok((resumed, Driver(driver)))) => {
                self.install(*resumed);
                self.bind(driver);
                self.settle();
                Task::none()
            }
            SplitEvent::Resumed(_, Err(unresumed)) => {
                let Unresumed {
                    mut refusal,
                    resumed,
                    abandon,
                } = *unresumed;
                if let Some(resumed) = resumed {
                    self.install(*resumed);
                }
                match abandon {
                    // #625 F2: nothing to rebuild, review or send; only the
                    // check-then-confirm abandon.
                    Some(Ok(only)) => {
                        self.phase = Some(Phase::Intent);
                        self.claimed = only.claimed.clone();
                        self.abandon_only = Some(only);
                        self.notice = Some(step1::UNREBUILDABLE.to_string());
                    }
                    // The journal is kept; say why it can't be abandoned,
                    // unless that is the refusal already shown.
                    Some(Err(why)) => {
                        refusal.retry |= why.retry;
                        if why.reason != refusal.reason {
                            self.notice = Some(why.reason);
                        }
                    }
                    None => {}
                }
                self.stage = Stage::Refused(refusal);
                Task::none()
            }
            SplitEvent::Reviewed(_, Driver(driver), result) => {
                self.bind(driver);
                match result {
                    Ok(review) => {
                        self.notice = None;
                        self.review = Some(review);
                        self.stage = Stage::Review;
                    }
                    Err(reason) => {
                        self.notice = Some(reason);
                        self.settle();
                    }
                }
                Task::none()
            }
            SplitEvent::Submitted(_, Driver(driver), result) => {
                self.bind(driver);
                match result {
                    Ok(outcome) => {
                        self.notice = None;
                        self.outcome = Some(outcome);
                    }
                    // The signed step 1 stays for export with the reason.
                    Err(reason) => self.notice = Some(reason),
                }
                self.settle();
                Task::none()
            }
            SplitEvent::Reconciled(_, Driver(driver), result) => {
                self.bind(driver);
                match result {
                    Ok(status) => {
                        self.notice = None;
                        self.status = Some(status);
                    }
                    Err(reason) => self.notice = Some(reason),
                }
                self.settle();
                Task::none()
            }
            SplitEvent::Recovered(_, Driver(driver), recovered) => {
                self.bind(driver);
                match recovered {
                    Recovered::Review(Recovery::Reconfirmed {
                        previous,
                        confirmed,
                    }) => {
                        self.notice = None;
                        self.stage = Stage::Reconfirm {
                            previous,
                            confirmed,
                        };
                    }
                    Recovered::Review(Recovery::Resend(review)) => {
                        self.notice = None;
                        self.review = Some(review);
                        self.stage = Stage::Resend;
                    }
                    Recovered::NewStep1Needed => {
                        self.stage = Stage::Refused(Refusal::final_(step1::NEW_POISON_NEEDED));
                    }
                    Recovered::Refused(reason) => {
                        self.notice = Some(reason);
                        self.settle();
                    }
                }
                Task::none()
            }
            SplitEvent::Acknowledged(_, Driver(driver), result) => {
                self.bind(driver);
                match result {
                    Ok(()) => {
                        // Depth now counts from the new block: check again.
                        self.status = None;
                        self.notice = Some(
                            "The new block is recorded. Check status again to count its confirmations."
                                .to_string(),
                        );
                    }
                    Err(reason) => self.notice = Some(reason),
                }
                self.settle();
                Task::none()
            }
            SplitEvent::Resent(_, Driver(driver), result) => {
                self.bind(driver);
                match result {
                    Ok(outcome) => {
                        self.notice = None;
                        self.status = None;
                        self.outcome = Some(outcome);
                    }
                    Err(reason) => self.notice = Some(reason),
                }
                self.settle();
                Task::none()
            }
            SplitEvent::AbandonChecked(_, result) => {
                match result {
                    Ok(()) => self.abandon_checked = true,
                    Err(refusal) => self.notice = Some(refusal.reason),
                }
                self.stage = self.resume_stage.take().unwrap_or(Stage::Ready);
                Task::none()
            }
            SplitEvent::Abandoned(_, Ok(())) => {
                self.ending = None;
                self.journal = None;
                self.phase = None;
                self.claimed.clear();
                self.abandon_only = None;
                self.notice = None;
                self.stage = Stage::Abandoned;
                Task::none()
            }
            SplitEvent::Abandoned(_, Err(reason)) => {
                self.ending = None;
                self.stage = Stage::Refused(Refusal::retry(reason));
                Task::none()
            }
            SplitEvent::SignedExported(_, result) => {
                self.notice = Some(match result {
                    Ok(Some(path)) => format!("Signed step 1 saved to {}.", path.display()),
                    Ok(None) => return Task::none(),
                    Err(reason) => reason,
                });
                Task::none()
            }
            other => self.apply_step2(other),
        }
    }

    fn install(&mut self, resumed: Resumed) {
        self.construction = Some(Box::new(resumed.construction));
        self.signed = Some(resumed.signed);
        self.phase = Some(resumed.phase);
        self.claimed = resumed.claimed;
        self.coins = resumed.coins;
    }
}

impl SplitEvent {
    fn seq(&self) -> u64 {
        match self {
            Self::Checked(seq, _)
            | Self::Built(seq, _)
            | Self::Exported(seq, _)
            | Self::Imported(seq, _)
            | Self::Recorded(seq, _)
            | Self::Resumed(seq, _)
            | Self::Reviewed(seq, ..)
            | Self::Submitted(seq, ..)
            | Self::Reconciled(seq, ..)
            | Self::Recovered(seq, ..)
            | Self::Acknowledged(seq, ..)
            | Self::Resent(seq, ..)
            | Self::AbandonChecked(seq, _)
            | Self::Abandoned(seq, _)
            | Self::Closed(seq, _)
            | Self::SignedExported(seq, _)
            | Self::ExportChosen(seq, ..)
            | Self::ImportChosen(seq, _)
            | Self::SignedExportChosen(seq, _)
            | Self::Restarted(seq, _)
            | Self::Step2Entered(seq, _)
            | Self::Step2Left(seq, _)
            | Self::Step2Checked(seq, ..)
            | Self::Step2Reserved(seq, ..)
            | Self::Step2Built(seq, ..)
            | Self::Step2ExportChosen(seq, ..)
            | Self::Step2Exported(seq, _)
            | Self::Step2ImportChosen(seq, _)
            | Self::Step2Imported(seq, ..)
            | Self::Step2Finished(seq, _)
            | Self::Step2Reviewed(seq, ..)
            | Self::Step2Submitted(seq, ..)
            | Self::Step2Reconciled(seq, ..)
            | Self::ReconReconciled(seq, ..) => *seq,
        }
    }
}

/// Restore the journal and resume its coordinator with the recorded bytes.
/// A rebuild refused finally reads the journal again for abandonment only
/// (#625 F2); a refusal that may pass on retry does not.
async fn resume(
    connect: Arc<dyn SplitConnect>,
    directory: PathBuf,
    target_cube: String,
    digest: sha256::Hash,
) -> ResumeResult {
    let unresumed = |refusal, resumed, abandon| {
        Box::new(Unresumed {
            refusal,
            resumed,
            abandon,
        })
    };
    let restored = match step1::restore(&*connect, &directory, &target_cube, digest).await {
        Ok(restored) => restored,
        Err(refusal) if refusal.retry => return Err(unresumed(refusal, None, None)),
        Err(refusal) => {
            let abandon = step1::abandon_only(&*connect, &directory, &target_cube, digest).await;
            return Err(unresumed(refusal, None, Some(abandon)));
        }
    };
    let step1::Restored {
        construction,
        verified,
        fork_height,
        phase,
        claimed,
        coins,
    } = restored;
    let resumed = Box::new(Resumed {
        construction: construction.clone(),
        signed: verified.transaction().clone(),
        phase,
        claimed,
        coins,
    });
    let opened = tokio::task::spawn_blocking(move || {
        connect.open(OpenRequest {
            directory,
            target_cube,
            construction,
            verified,
            fork_height,
            resume: true,
        })
    })
    .await;
    match opened {
        Ok(Ok(driver)) => Ok((resumed, Driver(driver))),
        Ok(Err(error)) => Err(unresumed(
            Refusal::retry(step1::describe(error)),
            Some(resumed),
            None,
        )),
        Err(_) => Err(unresumed(
            Refusal::retry("Resuming the split was interrupted. Try again."),
            Some(resumed),
            None,
        )),
    }
}

#[cfg(all(test, unix))]
mod tests;
