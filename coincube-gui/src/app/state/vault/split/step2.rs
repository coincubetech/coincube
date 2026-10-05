//! Split step 2 (#568 B3b-2b): what the panel does for step 2 that is not
//! iced state. The panel's step-2 stages (`panel2`), its view and the App's
//! port handoff drive it, and like the panel it is reached only by resuming
//! an existing Split journal: nothing in the GUI starts a split before B5
//! (D1).
//!
//! - **Ports.** [`Step2Port`] opens the step-2 submission side of a Split
//!   journal through the target Vault's daemon: a [`Step2Prep`] (target
//!   reservation and proof, construction under the step-2 token, the
//!   signed-PSBT handoff). Its production implementation, [`ProductionStep2`],
//!   wraps `claim_coordinator::fork::split` and exists only for a daemon on a
//!   route step 2 can be sent through (#637 R2). [`ReconPort`] opens, after a
//!   recorded step-2 submission, a [`Step2Recon`] that can only reconcile; its
//!   production implementation, [`ProductionRecon`], needs only the Connect
//!   session, never the daemon (#637 R1).
//! - **Journal-lock ordering** (#626). The step-1 driver and the step-2
//!   preparation each hold the journal lock. [`enter_step2`] drops the step-1
//!   driver *before* opening the preparation, off the UI thread;
//!   [`leave_for_step1`] drops the preparation before reopening the step-1
//!   driver for a reorg review; the preparation's `finish` hands its lock to
//!   the submission coordinator. [`restart`] opens a [`Step2Recon`] instead of
//!   the step-1 driver when the journal already records a step-2 submission,
//!   because the claimed coins may be spent on BTCB2 by then and step 1 can
//!   no longer be rebuilt from them. That decision needs only the Connect
//!   session, so it holds whatever state the Vault daemon is in.
//! - **Resend** (P3-3). When the journal also allows a reviewed resend (its
//!   latest attempt came back without the route's acceptance, the step 2
//!   was never seen on BTCB2, under the limit) and the Vault daemon gives a
//!   step-2 port, [`restart`] opens the submission coordinator again through
//!   [`Step2Port::reopen_for_resend`] (step 1 rebuilt from freshly
//!   authenticated coins, then `resume_uncertain`) instead of the
//!   reconciler; otherwise, or if that open refuses, the reconciler as
//!   before. The coordinator reconciles, and resends only after an explicit
//!   one-use review ([`Step2Coord::review_resend`],
//!   [`Step2Coord::confirm_resend`]), which a reconcile, its deadline, the
//!   session's revocation or a generation change drops.
//! - **Copy.** Every target, construction and resend refusal
//!   ([`describe_target`], [`describe_step2`], [`describe_resend`]), the
//!   waiting state while the Vault reserves its address ([`RESERVING`], #592
//!   N4), the route label with a privacy note on the node route
//!   ([`route_copy`]) and the "Split — cannot replay" label, which only a
//!   live six-confirmation check can produce ([`CannotReplay`]).
//!
//! - **Closing a dead end** (#625 F2, A1 = A). A recorded step 2 that no
//!   resend can follow and no read ever saw ([`DeadEnd`]) may be closed after
//!   [`check_close`]: [`close`] leaves the journal, recorded bytes included,
//!   and writes its tombstone, so discovery skips it and a new split of the
//!   same source stays refused until the owner removes the tombstone.
//!
//! Every blocking call here runs off the UI thread.

use std::{
    convert::TryFrom,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use async_trait::async_trait;
use tokio::sync::watch;

use coincube_core::{
    chain::ChainId,
    claim::MIN_CONFIRMATIONS,
    descriptors::CoincubeDescriptor,
    foreign_split::{SplitCoin, SplitStep1, VerifiedSplitStep1},
    miniscript::bitcoin::{hashes::sha256, psbt::Psbt, Address, Network, OutPoint, Txid},
};

use super::step1::{self, OpenRequest, Refusal, RevokeHandle, SplitConnect, Step1Driver};
use crate::{
    app::state::vault::claim::{ConnectSession, CHECK_POLICY},
    daemon::Daemon,
    services::{
        claim_coordinator::{
            self,
            fork::split::{
                step2::{
                    ResendError, SplitStep2Coordinator, SplitStep2Production, SplitStep2Reconciler,
                    Step1AfterStep2, Step2Error, Step2ResubmissionReview, SweepReconcile,
                    TargetError, RESERVATION_BOUND,
                },
                ForeignStep2Authorization, SplitCheckError, SplitForkProduction, SplitPreparation,
                Step2Liveness,
            },
            Outcome, Review, SubmissionRoute,
        },
        claim_observation::{FailureKind, TransactionObservation},
        claim_workflow::{self, Context, Controller, Status},
        foreign_psbt::SweepFeeSource,
        split_evidence::{SplitEvidenceSource, MAX_EVIDENCE_AGE_SECONDS},
        split_fees,
    },
};

/// N4: shown while the target Vault reserves its fresh receive address and
/// Connect proves it unused, for at most [`RESERVATION_BOUND`].
pub const RESERVING: &str = "Reserving a fresh receive address in this Vault and checking with Connect that it has never been used on either chain. This takes a few seconds.";
/// The live replay-protection label (see [`CannotReplay`]).
pub const CANNOT_REPLAY: &str = "Split — cannot replay";
/// The privacy note shown with the node route.
pub const NODE_PRIVACY: &str = "Step 2 will be sent through this Vault's own Bitcoin node. That node, which may be a remote one you configured, learns the transaction and this computer's network address before it relays it.";
/// A restart found a recorded step-2 submission but has no reconciler for
/// this session (#637 R1): nothing else is opened.
pub const RECONCILE_UNAVAILABLE: &str = "Step 2 of this split was already sent or may have been. Its status can't be checked with Connect right now, so nothing was rebuilt or sent. Try again.";
/// A reconcile after the step-2 submission found step 1 reorged out of its
/// Bitcoin block and out of the mempool (#637 r4172242637; #568 S4, O3). No
/// recovery is offered: D13 = A never resends step 1. A recorded
/// submission may not have reached the transport (`Outcome::Uncertain`), so
/// the copy doesn't say step 2 was sent (#637 Copilot review 5401909718).
pub const STEP1_REORGED_AFTER_STEP2: &str = "Bitcoin reorganized after a submission of step 2 was recorded; it was sent or may have been sent. Step 1 is no longer in any Bitcoin block, and Connect doesn't see it waiting to be mined, so Bitcoin replay protection for step 2 is no longer established. Until step 1 has 6 Bitcoin confirmations again, step 2's recorded bytes could also be mined on Bitcoin. This version has no recovery for this. Nothing was rebuilt, and step 2 is not sent automatically. Check status again later.";
/// What every reorg warning after the step-2 submission adds while step 1
/// isn't confirmed (#568 S4).
const STEP2_EXPOSED: &str =
    "Until step 1 has 6 Bitcoin confirmations again, step 2's recorded bytes could also be mined on Bitcoin.";
/// A restart found a resend the journal allows, but no step-2 port to send
/// it through (P3-3): only the reconciler was opened.
pub const RESEND_NEEDS_VAULT: &str = "Sending step 2 again needs this Vault's wallet engine running on a route step 2 can be sent through. Its status can still be checked.";
/// A restart's resend reopen found a claimed coin no longer unspent on
/// BTCB2 (#648 R1). Step 2 is recorded as having come back unaccepted, so it
/// may itself be the spender (relayed anyway, or sent from another copy of
/// this Cube); either way it is never sent again.
pub const RESEND_COIN_SPENT: &str = "Step 2 can't be sent again: a coin this split claims is no longer unspent on Bitcoin Blake2b, and this step 2 may itself have spent it. Nothing was sent; check its status.";
/// `ResendError::Unsettled`: the journal does not record that the latest
/// attempt came back unaccepted, so it may have left (P3-3). Only the #625
/// F2 abandon or reset path gets out of this.
pub const RESEND_UNSETTLED: &str = "This version can't send step 2 again. Its last send, or a check of it, ended without a clear answer (it may have been accepted, or it was cancelled, timed out or interrupted), so step 2 may have reached the network. Nothing was sent; check its status. If step 2 never appears on Bitcoin Blake2b, the way out is to abandon or reset this split.";

/// What a reconcile after the step-2 submission found of step 1 on Bitcoin
/// means (#637 r4172242637; #568 S4): nothing while it is still eligible
/// (six deep in its recorded block, absent from BTCB2); otherwise one
/// warning per outcome, each naming the exposure (while step 1 isn't
/// confirmed, step 2's recorded bytes could be mined on Bitcoin; for a
/// terminal conflict, step 1 can never confirm and the split can't
/// complete; a provisional one may still be unconfirmed, S4-D5). A reorg
/// is named only for O1 to O4. The only action each leaves is checking
/// status again: none offers acknowledging a new block, closing the split or
/// resending step 1 (those controls are S4b's, S4-D3), and none shows the
/// "cannot replay" label.
pub fn reconcile_warning(after: Step1AfterStep2) -> Option<String> {
    const RECORDED: &str = "Bitcoin reorganized after a submission of step 2 was recorded; it was sent or may have been sent.";
    const UNCHANGED: &str =
        "Nothing was rebuilt, and step 2 is not sent automatically. Check status again later.";
    match after {
        Step1AfterStep2::Eligible => None,
        Step1AfterStep2::Shallow { confirmations } => Some(format!(
            "At the last check step 1 had {} of {MIN_CONFIRMATIONS} Bitcoin confirmations. Until it has all {MIN_CONFIRMATIONS} again, Bitcoin replay protection for step 2 is not established, and step 2's recorded bytes could also be mined on Bitcoin. Check status again later.",
            confirmations.min(MIN_CONFIRMATIONS)
        )),
        Step1AfterStep2::Remined { previous, confirmed } => Some(format!(
            "{RECORDED} Step 1 is now confirmed in a different Bitcoin block (height {}) from the one recorded for it (height {}), so Bitcoin replay protection for step 2 is not established until this split records the new block. Until then, step 2's recorded bytes could also be mined on Bitcoin if step 1 leaves that block too. {UNCHANGED}",
            confirmed.height, previous.height
        )),
        Step1AfterStep2::InMempool => Some(format!(
            "{RECORDED} Step 1 is no longer in any Bitcoin block; it is waiting to be mined again, so Bitcoin replay protection for step 2 is no longer established. {STEP2_EXPOSED} {UNCHANGED}"
        )),
        Step1AfterStep2::Missing => Some(STEP1_REORGED_AFTER_STEP2.to_string()),
        Step1AfterStep2::Conflict(conflict) if conflict.is_terminal() => Some(format!(
            "{RECORDED} Step 1 is no longer in any Bitcoin block, and a coin this split claims ({}) was spent on Bitcoin by another transaction, so step 1 can never confirm and this split can't complete. {UNCHANGED}",
            conflict.outpoint()
        )),
        // S4-D5: a missing coin is a conflict only once it stays missing.
        Step1AfterStep2::Conflict(conflict) => Some(format!(
            "{RECORDED} Step 1 is no longer in any Bitcoin block, and a coin this split claims ({}) appears spent on Bitcoin by another transaction (it may still be unconfirmed). If that spend confirms, step 1 can't. {STEP2_EXPOSED} {UNCHANGED}",
            conflict.outpoint()
        )),
        Step1AfterStep2::Unknown => Some(format!(
            "The last check couldn't confirm step 1 on Bitcoin with fresh evidence. That is not a sign of a reorg, but Bitcoin replay protection for step 2 isn't confirmed until a check sees step 1 at full depth again. {STEP2_EXPOSED} Check status again later."
        )),
    }
}

/// Recovery guidance or state invalidation required by a refusal, independent of copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step2Recovery {
    None,
    RefreshTarget,
    ReopenCube,
    /// No resend can follow in this coordinator (an unsettled last attempt,
    /// or the attempt limit): the panel reads the journal again through
    /// [`restart`], so a dead end comes with its reconciler and its close
    /// (#648 X1).
    Restart,
}

/// What a refused step-2 operation means for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step2Refusal {
    pub reason: String,
    pub retry: bool,
    pub recovery: Step2Recovery,
}
impl Step2Refusal {
    fn final_(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retry: false,
            recovery: Step2Recovery::None,
        }
    }
    pub(super) fn retry(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retry: true,
            recovery: Step2Recovery::None,
        }
    }
}

fn chain_name(chain: coincube_core::chain::ChainId) -> &'static str {
    match chain {
        coincube_core::chain::ChainId::Bitcoin => "Bitcoin",
        _ => "Bitcoin Blake2b",
    }
}

/// Copy for a target reservation or proof refusal.
pub fn describe_target(error: TargetError) -> Step2Refusal {
    match error {
        TargetError::Coordinator(error) => describe_check(error),
        TargetError::AlreadyReserved => Step2Refusal::retry(
            "This Vault already has a reserved address for this split; it is reused. Check it again.",
        ),
        TargetError::NotTracking => Step2Refusal::retry(
            "Step 1 has not been seen on Bitcoin yet, so no address is reserved for step 2. Check status again later.",
        ),
        TargetError::NoReservation => {
            Step2Refusal::retry("No address is reserved for step 2 yet. Reserve one first.")
        }
        TargetError::ReservationUnavailable => Step2Refusal::retry(
            "This Vault did not reserve a receive address in time. Nothing was built; try again.",
        ),
        TargetError::NotTargetVault => Step2Refusal::final_(
            "The reserved address does not belong to this Cube's Vault. Nothing was built or sent.",
        ),
        TargetError::Used(chain) => Step2Refusal::retry(format!(
            "The address reserved for step 2 already has history on {}, so it is not fresh. It won't be used; try again to reserve a new one.",
            chain_name(chain)
        )),
        TargetError::Unavailable(chain, kind) => Step2Refusal::retry(format!(
            "Connect couldn't prove the reserved address unused on {} ({kind:?}). This is a Connect limit, not a sign the address was used. Try again.",
            chain_name(chain)
        )),
    }
}

/// Copy for a step-2 construction refusal.
pub fn describe_step2(error: Step2Error) -> Step2Refusal {
    match error {
        Step2Error::Coordinator(error) => describe_check(error),
        Step2Error::FeeUnavailable => Step2Refusal::retry(
            "Connect has no Bitcoin Blake2b fee estimate right now, so step 2 can't be priced. Nothing was built; try again shortly.",
        ),
        Step2Error::TargetNotProven => Step2Refusal {
            reason: "The address proof expired or is unavailable. Select Reserve address, then Check confirmations, then Build step 2. An unused reserved address will be reused.".to_string(),
            retry: true,
            recovery: Step2Recovery::RefreshTarget,
        },
        Step2Error::NotChecked | Step2Error::Redeem(_) => Step2Refusal::retry(
            "The step-2 check expired before step 2 was built. Nothing was built; try again.",
        ),
        Step2Error::DescriptorsForgotten => Step2Refusal::final_(step1::COMPLETED),
        Step2Error::Construction(error) => Step2Refusal::final_(format!(
            "Split couldn't build step 2 ({error}). Nothing was signed or sent."
        )),
    }
}

/// Copy for the six-confirmation check.
pub fn describe_split_check(error: SplitCheckError) -> Step2Refusal {
    match error {
        SplitCheckError::Coordinator(error) => describe_check(error),
        SplitCheckError::ClaimedCoinSpent(_) => Step2Refusal::final_(
            "A coin this split claims was already spent on Bitcoin Blake2b, so step 2 can't sweep it. Nothing was built or sent.",
        ),
        SplitCheckError::Unavailable(_, kind) => Step2Refusal::retry(format!(
            "Connect couldn't read Bitcoin Blake2b for this split ({kind:?}). This is a Connect or indexer limit, not a sign a coin was spent. Try again later."
        )),
    }
}

/// Copy for a refused resend review or confirmation (P3-3). None of these
/// sent anything; the coordinator's own refusals read as for step 2.
pub fn describe_resend(error: ResendError) -> Step2Refusal {
    match error {
        ResendError::Coordinator(error) => describe_step2(Step2Error::Coordinator(error)),
        ResendError::NotRecorded => Step2Refusal::final_(
            "No submission of step 2 is recorded, so there is nothing to send again. Nothing was sent.",
        ),
        ResendError::Observed => Step2Refusal::final_(
            "Step 2 was seen on Bitcoin Blake2b, so it left this device and is never sent again. Nothing was sent; check its status.",
        ),
        ResendError::Unsettled => Step2Refusal {
            recovery: Step2Recovery::Restart,
            ..Step2Refusal::final_(RESEND_UNSETTLED)
        },
        ResendError::AttemptsExhausted => Step2Refusal {
            recovery: Step2Recovery::Restart,
            ..Step2Refusal::final_(format!(
                "Step 2 was already sent again {} times, the most this version allows. Nothing was sent; check its status. If step 2 never appears on Bitcoin Blake2b, the way out is to abandon or reset this split.",
                claim_workflow::MAX_SPLIT_STEP2_RESUBMISSIONS
            ))
        },
        ResendError::ClaimedCoinSpent(outpoint) => Step2Refusal::final_(format!(
            "A coin this split claims ({outpoint}) is already spent on Bitcoin Blake2b, so the recorded step 2 can never confirm. Nothing was sent."
        )),
        ResendError::Unavailable(_, kind) => Step2Refusal::retry(format!(
            "Connect couldn't read Bitcoin Blake2b's unspent coins for this split ({kind:?}). This is a Connect or indexer limit, not a sign a coin was spent. Nothing was sent; try again later."
        )),
        ResendError::Step1ConflictRecorded(conflict) if conflict.is_terminal() => {
            Step2Refusal::final_(format!(
                "Step 2 can't be sent again: a coin this split claims ({}) was spent on Bitcoin by another transaction, so step 1 can never confirm and this split can't complete. Nothing was sent.",
                conflict.outpoint()
            ))
        }
        ResendError::Step1ConflictRecorded(conflict) => Step2Refusal::final_(format!(
            "Step 2 isn't sent again while a coin this split claims ({}) appears spent on Bitcoin by another transaction (it may still be unconfirmed). Nothing was sent; check its status.",
            conflict.outpoint()
        )),
    }
}

fn describe_check(error: claim_coordinator::Error) -> Step2Refusal {
    use claim_coordinator::Error as E;
    let recovery = match error {
        E::Unsupported | E::InvalidBinding | E::Journal(claim_workflow::Error::WrongIdentity) => {
            Step2Recovery::ReopenCube
        }
        _ => Step2Recovery::None,
    };
    let retry = recovery != Step2Recovery::ReopenCube;
    let reason = match error {
        E::NotReady(coincube_core::claim::Assessment::WaitingForDepth { confirmations }) => {
            format!(
                "Step 1 has {confirmations} of {} Bitcoin confirmations. Step 2 waits for all of them.",
                coincube_core::claim::MIN_CONFIRMATIONS
            )
        }
        E::Preflight(crate::services::claim_preflight::Error::BackendChanged) => {
            "This Vault's connection changed since the review, so nothing was sent. Review step 2 again.".to_string()
        }
        other => step1::describe(other),
    };
    Step2Refusal {
        reason,
        retry,
        recovery,
    }
}

/// The review screen's route label, and a privacy note for the node route.
pub fn route_copy(route: SubmissionRoute) -> (&'static str, Option<&'static str>) {
    match route {
        SubmissionRoute::Connect => (route.label(), None),
        SubmissionRoute::BitcoinNode { .. } => (route.label(), Some(NODE_PRIVACY)),
    }
}

/// "Split — cannot replay", only from live evidence: a successful
/// six-confirmation check (step 1 six deep on Bitcoin at the tip, absent
/// from BTCB2, RDTS margin, every claimed coin unspent on BTCB2) minted it.
/// It carries that check's liveness (#636 P3-2), so the label disappears as
/// soon as the evidence does: a later check starts (whatever its result),
/// the session is revoked (logout, Cube close, backend switch), the
/// generation changes, the preparation is dropped, or the deadline passes.
/// No saved phase, journal record or earlier check can produce it.
#[derive(Debug, Clone)]
pub struct CannotReplay {
    tracked: Txid,
    live: Step2Liveness,
}
impl CannotReplay {
    /// The label while the check's evidence is live; afterwards `None`.
    pub fn label(&self) -> Option<&'static str> {
        self.live.is_live().then_some(CANNOT_REPLAY)
    }
    pub fn tracked_txid(&self) -> Txid {
        self.tracked
    }
}

/// The label a token's check supports. The token itself never leaves the
/// driver.
fn evidence_of(token: &ForeignStep2Authorization) -> CannotReplay {
    CannotReplay {
        tracked: token.tracked_txid(),
        live: token.liveness(),
    }
}

/// A refused handoff: why, and the preparation when it is still usable.
pub type FinishRefusal = (Step2Refusal, Option<Box<dyn Step2Prep>>);

/// The step-2 preparation over one Split journal; holds its lock.
#[async_trait]
pub trait Step2Prep: Send {
    fn revoke_handle(&self) -> RevokeHandle;
    /// Whether a new target reservation is needed (none, or the recorded one
    /// proven used). Restart data only.
    fn needs_reservation(&self) -> bool;
    /// The six-confirmation check; on success the live label, and the token
    /// is kept for the next [`Self::build`].
    async fn check(&mut self, context: &Context) -> Result<CannotReplay, Step2Refusal>;
    /// Reserve (if needed) and prove the target: the N4 waiting state. A
    /// target proven used is replaced once by a new reservation.
    async fn ensure_target(&mut self, context: &Context) -> Result<u32, Step2Refusal>;
    /// Build step 2 under the token of the last check; the unsigned PSBT to
    /// sign.
    async fn build(
        &mut self,
        context: &Context,
        coins: Vec<SplitCoin>,
    ) -> Result<Psbt, Step2Refusal>;
    /// Whether a (possibly partially) signed PSBT would finish, without
    /// giving up the journal: `Ok(false)` while more signatures are needed.
    /// CPU-bound: callers use `spawn_blocking`.
    fn verify_signed(&self, signed: &Psbt, coins: &[SplitCoin]) -> Result<bool, Step2Refusal>;
    /// Verify the signed PSBT and hand the journal to the coordinator.
    /// CPU-bound and blocking: callers use `spawn_blocking`.
    fn finish(
        self: Box<Self>,
        context: &Context,
        signed: &Psbt,
        coins: &[SplitCoin],
    ) -> Result<Box<dyn Step2Coord>, FinishRefusal>;
}

/// A review shown for step 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step2ReviewView {
    pub txid: Txid,
    pub fee_sats: u64,
    pub vsize: usize,
    pub route: SubmissionRoute,
    pub route_label: &'static str,
    pub privacy_note: Option<&'static str>,
}

/// Whether a resend review's evidence and session are still current.
pub type ResendLiveness = Arc<dyn Fn() -> bool + Send + Sync>;

/// A resend review is live until `not_after`, while `revoked` says no and
/// the session's generation is still `expected` (and its sender alive).
fn resend_liveness(
    revoked: impl Fn() -> bool + Send + Sync + 'static,
    generation: watch::Receiver<u64>,
    expected: u64,
    not_after: Instant,
) -> ResendLiveness {
    Arc::new(move || {
        !revoked()
            && *generation.borrow() == expected
            && generation.has_changed().is_ok()
            && Instant::now() < not_after
    })
}

/// A resend review shown for step 2 (P3-3): exactly the recorded signed
/// step 2, on the route the review bound.
#[derive(Clone)]
pub struct Step2ResendView {
    pub txid: Txid,
    pub route: SubmissionRoute,
    pub route_label: &'static str,
    pub privacy_note: Option<&'static str>,
    /// This resend's number (the resends already recorded, plus one) and
    /// the most the journal records.
    pub attempt: usize,
    pub max_attempts: usize,
    /// When the review's evidence lapses, for display.
    pub expires_at: chrono::DateTime<chrono::Local>,
    live: ResendLiveness,
}
impl std::fmt::Debug for Step2ResendView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Step2ResendView")
            .field("txid", &self.txid)
            .field("attempt", &self.attempt)
            .finish_non_exhaustive()
    }
}
impl Step2ResendView {
    /// Until its deadline, while the coordinator that made it is not revoked
    /// and the session's generation has not moved. Afterwards the panel
    /// drops the review; a confirmation would refuse anyway.
    pub fn is_live(&self) -> bool {
        (self.live)()
    }
}

/// The step-2 submission coordinator over one Split journal.
#[async_trait]
pub trait Step2Coord: Send {
    fn revoke_handle(&self) -> RevokeHandle;
    fn recorded_outcome(&self) -> Option<Outcome>;
    async fn review(&mut self, context: &Context) -> Result<Step2ReviewView, Step2Refusal>;
    /// Submit exactly what the last review showed.
    async fn submit(&mut self, context: &Context) -> Result<Outcome, Step2Refusal>;
    /// Drops any resend review.
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<(Status, TransactionObservation, Step1AfterStep2), Step2Refusal>;
    /// P3-3: a fresh resend review of exactly the recorded signed step 2,
    /// replacing any earlier one. Records nothing and sends nothing.
    async fn review_resend(&mut self, context: &Context) -> Result<Step2ResendView, Step2Refusal>;
    /// Send the recorded step 2 again, once, as the last resend review
    /// showed. That review is used up whatever the result; another resend
    /// needs another review.
    async fn confirm_resend(&mut self, context: &Context) -> Result<Outcome, Step2Refusal>;
}

/// After a recorded step-2 submission: reconcile only.
#[async_trait]
pub trait Step2Recon: Send {
    fn revoke_handle(&self) -> RevokeHandle;
    fn recorded_outcome(&self) -> Option<Outcome>;
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<(Status, TransactionObservation, Step1AfterStep2), Step2Refusal>;
}

/// Everything the preparation is opened with: the step 1 rebuilt at restore.
pub struct Step2Open {
    pub directory: PathBuf,
    pub target_cube: String,
    pub construction: SplitStep1,
    pub verified: VerifiedSplitStep1,
    pub fork_height: u64,
}

/// Opens the step-2 submission side of a Split journal for one session,
/// through the target Vault's daemon.
#[async_trait]
pub trait Step2Port: Send + Sync {
    fn context(&self) -> Context;
    /// What makes two ports the same: the session context (account,
    /// provider, generation) and the Vault daemon instance. The panel keeps
    /// its step-2 handles across an equivalent port and revokes them on any
    /// other (#637 F1).
    fn identity(&self) -> PortIdentity;
    /// Blocking: callers use `spawn_blocking`, after dropping any step-1
    /// driver on the same journal.
    fn open_preparation(&self, open: Step2Open) -> Result<Box<dyn Step2Prep>, Step2Refusal>;
    /// P3-3, at restart only: the submission coordinator of the journal's
    /// recorded step 2, rebuilt from it (step 1 restored through `connect`
    /// from freshly authenticated coins, then the recorded signed step 2
    /// verified against its own rebuild). It never signs, reserves or
    /// builds anything new; it reconciles, and resends only after an
    /// explicit review. Called only when no other handle holds the journal.
    async fn reopen_for_resend(
        &self,
        connect: Arc<dyn SplitConnect>,
        directory: PathBuf,
        target_cube: String,
        digest: sha256::Hash,
    ) -> Result<Box<dyn Step2Coord>, Step2Refusal>;
}

/// Opens the reconcile-only side of a Split journal for one Connect session
/// (#637 R1). It needs no Vault daemon: a recorded step 2 is reconciled
/// whether the daemon is loaded, restarting, external or on a route step 2
/// can't be sent through.
pub trait ReconPort: Send + Sync {
    fn context(&self) -> Context;
    /// Blocking: callers use `spawn_blocking`.
    fn open_reconciler(
        &self,
        directory: PathBuf,
        target_cube: String,
        digest: sha256::Hash,
    ) -> Result<Box<dyn Step2Recon>, Step2Refusal>;
}

/// See [`Step2Port::identity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortIdentity {
    pub context: Context,
    /// The daemon handle's address: a restarted or switched daemon is a new
    /// instance.
    pub daemon: usize,
}

/// #626 ordering: release the step-1 driver (and its journal lock), *then*
/// open the step-2 preparation off the UI thread. Opening while the step-1
/// driver still held the lock would wait 2 s and refuse as busy.
pub async fn enter_step2(
    step1: Box<dyn Step1Driver>,
    port: Arc<dyn Step2Port>,
    open: Step2Open,
) -> Result<Box<dyn Step2Prep>, Step2Refusal> {
    drop(step1);
    tokio::task::spawn_blocking(move || port.open_preparation(open))
        .await
        .map_err(|_| Step2Refusal::retry("Opening step 2 was interrupted. Try again."))?
}

/// #626 ordering: drop the step-2 preparation *before* reopening the step-1
/// driver, for a reorg review (re-mined or dropped step 1) that only the
/// step-1 coordinator offers.
pub async fn leave_for_step1(
    prep: Box<dyn Step2Prep>,
    connect: Arc<dyn SplitConnect>,
    open: OpenRequest,
) -> Result<Box<dyn Step1Driver>, Refusal> {
    drop(prep);
    tokio::task::spawn_blocking(move || connect.open(open))
        .await
        .map_err(|_| Refusal::retry("Reopening step 1 was interrupted. Try again."))?
        .map_err(|error| Refusal::retry(step1::describe(error)))
}

/// Which side of the journal a restart reopens.
pub enum Restart {
    /// No step-2 submission recorded: resume step 1 as before
    /// (`SplitPanel::resume`).
    Step1,
    /// A step-2 submission is recorded: reconcile only, with its dead end
    /// when it is in one (#625 F2), or else why a resend the journal allows
    /// could not be opened (P3-3). At most one is set: a journal in a dead
    /// end allows no resend.
    Reconcile(Box<dyn Step2Recon>, Option<DeadEnd>, Option<String>),
    /// A step-2 submission is recorded and the journal allows a reviewed
    /// resend (P3-3): its coordinator, reopened from the recorded bytes.
    Resend(Box<dyn Step2Coord>),
    /// #625 F2: the split was closed in its step-2 dead end. Nothing opens.
    Closed,
}

/// #625 F2: a recorded step 2 that no resend can follow and no read ever
/// saw on BTCB2 ([`Controller::split_step2_dead_end`]), as the restart read
/// it. What [`check_close`] looks for on both chains; it grants nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadEnd {
    /// The recorded signed step 1's own txid.
    pub step1: Txid,
    /// The recorded signed step 2's own txid.
    pub step2: Txid,
    /// Step 1's claimed inputs: the coins step 2 spends on BTCB2.
    pub claimed: Vec<OutPoint>,
}

/// Why a resend the journal allows was not reopened (P3-3). Restoring step 1
/// for it can find a claimed coin spent on BTCB2, which step 1's copy blames
/// on something other than step 2 (#648 R1); and a final refusal is not
/// "right now".
fn resend_unavailable(refusal: &Step2Refusal) -> String {
    let spent = step1::evidence_refusal(crate::services::split_evidence::EvidenceError {
        outpoint: None,
        failure: crate::services::split_evidence::EvidenceFailure::Btcb2Spent,
    });
    if refusal.reason == spent.reason {
        RESEND_COIN_SPENT.to_string()
    } else if refusal.retry {
        format!(
            "Step 2 can't be sent again right now: {} Its status can still be checked.",
            refusal.reason
        )
    } else {
        format!(
            "Step 2 can't be sent again: {} Its status can still be checked.",
            refusal.reason
        )
    }
}

/// Whether the journal allows a reviewed resend of its recorded step 2
/// (P3-3): the latest attempt is recorded as having come back without the
/// route's acceptance, the step 2 was never seen on BTCB2, and the resends
/// recorded are under the limit. This only chooses which handle a restart
/// opens; the coordinator checks it again, with fresh evidence, before any
/// resend.
fn resend_allowed(controller: &Controller) -> bool {
    controller.split_step2_returned()
        && !controller.split_step2_observed()
        && controller.split_step2_resubmissions() < claim_workflow::MAX_SPLIT_STEP2_RESUBMISSIONS
}

/// Restart decision: read the journal under the Connect session's `context`
/// (lock released at once) and, when it records a step-2 submission, open
/// the reconciler instead of rebuilding step 1 (whose claimed coins may
/// already be spent on BTCB2 by step 2). No reconciler for the session
/// refuses; it never falls back to step 1 (#637 R1). A closed split opens
/// nothing (#625 F2), including one closed while this restart waited for
/// the journal's lock (#644 G2).
///
/// P3-3: when the journal also allows a resend and `resend` gives the
/// target Vault's step-2 port for the same session, the coordinator is
/// reopened through it instead; if that refuses (or there is no such port)
/// the reconciler is opened as before, with the reason kept for the panel.
pub async fn restart(
    context: Context,
    recon: Option<Arc<dyn ReconPort>>,
    resend: Option<(Arc<dyn Step2Port>, Arc<dyn SplitConnect>)>,
    directory: PathBuf,
    target_cube: String,
    digest: sha256::Hash,
) -> Result<Restart, Step2Refusal> {
    if step1::is_closed(&directory) {
        return Ok(Restart::Closed);
    }
    let identity = claim_workflow::split_identity(target_cube.clone(), digest);
    let (recorded, resendable, dead_end) = {
        let controller = Controller::reopen_settling(&directory, &identity, context.clone())
            .await
            .map_err(|error| {
                Step2Refusal::retry(step1::describe(claim_coordinator::Error::Journal(error)))
            })?;
        // #644 G2: a close that held the journal's lock while this restart
        // waited for it has written its tombstone by now. The check above
        // stays: it keeps a closed split from waiting for the lock at all.
        if step1::is_closed(&directory) {
            return Ok(Restart::Closed);
        }
        (
            controller.recorded_split_step2().is_some(),
            resend_allowed(&controller),
            dead_end(&controller),
        )
        // The controller, and the journal lock, end here.
    };
    if !recorded {
        return Ok(Restart::Step1);
    }
    let mut unavailable = None;
    if resendable {
        let reopened = match resend {
            Some((port, connect)) if port.context() == context && connect.context() == context => {
                port.reopen_for_resend(connect, directory.clone(), target_cube.clone(), digest)
                    .await
                    .map_err(|refusal| resend_unavailable(&refusal))
            }
            _ => Err(RESEND_NEEDS_VAULT.to_string()),
        };
        match reopened {
            Ok(coord) => return Ok(Restart::Resend(coord)),
            Err(reason) => unavailable = Some(reason),
        }
    }
    let port = recon.ok_or_else(|| Step2Refusal::retry(RECONCILE_UNAVAILABLE))?;
    tokio::task::spawn_blocking(move || port.open_reconciler(directory, target_cube, digest))
        .await
        .map_err(|_| Step2Refusal::retry("Reopening the split was interrupted. Try again."))?
        .map(|recon| Restart::Reconcile(recon, dead_end, unavailable))
}

/// The journal's step-2 dead end, if it is in one.
fn dead_end(controller: &Controller) -> Option<DeadEnd> {
    if !controller.split_step2_dead_end() {
        return None;
    }
    let plan = controller.plan();
    Some(DeadEnd {
        step1: plan.step1_txid(),
        step2: controller.recorded_split_step2()?.compute_txid(),
        claimed: plan.claimed_prevouts,
    })
}

/// #625 F2: a dead-end split whose step 2 Bitcoin Blake2b shows.
pub const STEP2_SEEN: &str = "Bitcoin Blake2b shows this step 2, so it left and this split can't be abandoned. It stays tracked here.";
/// #625 F2: step 1 is not (or no longer) six deep on Bitcoin.
pub const STEP1_NOT_DEEP: &str = "Step 1 isn't six confirmations deep on Bitcoin right now, so this split can't be abandoned yet. Check again later.";
/// #625 F2: a claimed coin is no longer unspent on BTCB2.
pub const COIN_SPENT_ON_BTCB2: &str = "A coin of this split is no longer unspent on Bitcoin Blake2b, possibly spent by this step 2. This split can't be abandoned; it stays tracked here.";
/// #625 F2: the journal changed between the check and the close.
pub const CHANGED_SINCE_CHECK: &str =
    "This split changed since it was checked, so it was not abandoned. Check again.";

fn fresh(evidence: &dyn SplitEvidenceSource, observed_at: i64) -> bool {
    evidence
        .now()
        .checked_sub(observed_at)
        .is_some_and(|age| (0..=MAX_EVIDENCE_AGE_SECONDS).contains(&age))
}
fn unavailable(kind: FailureKind) -> Refusal {
    Refusal::retry(format!(
        "Connect couldn't check the chains for this split ({kind:?}), so it can't be abandoned yet. This is not a sign that step 2 left or that a coin was spent. Try again later."
    ))
}
/// One fresh BTCB2 read keyed by the recorded step 2's own txid: absent.
async fn step2_absent(evidence: &dyn SplitEvidenceSource, step2: Txid) -> Result<(), Refusal> {
    let seen = evidence
        .transaction(ChainId::BitcoinBlake2b, step2)
        .await
        .map_err(unavailable)?;
    if !fresh(evidence, seen.observed_at()) {
        return Err(unavailable(FailureKind::Stale));
    }
    if *seen.value() != TransactionObservation::Absent {
        return Err(Refusal::final_(STEP2_SEEN));
    }
    Ok(())
}

/// #625 F2 (A1 = A): before a split in its step-2 dead end may be closed,
/// fresh reads must show, in this order: the recorded step 2 absent from
/// BTCB2 (a read keyed by its own txid); step 1 confirmed on Bitcoin in a
/// block still canonical at its height, at least [`MIN_CONFIRMATIONS`] deep
/// against the tip; every claimed input among its address's BTCB2 unspent
/// outputs (the address from its previous transaction, checked against its
/// txid); and the recorded step 2 absent again. A sighting, a spend, a
/// shallow or reorged step 1, or a failed or stale read refuses, and the
/// journal is kept. A sighting here is not recorded in the journal: the dead
/// end has no resend for it to end.
pub async fn check_close(connect: &dyn SplitConnect, dead_end: &DeadEnd) -> Result<(), Refusal> {
    let evidence = connect.evidence();
    step2_absent(evidence, dead_end.step2).await?;

    let status = evidence
        .transaction(ChainId::Bitcoin, dead_end.step1)
        .await
        .map_err(unavailable)?;
    if !fresh(evidence, status.observed_at()) {
        return Err(unavailable(FailureKind::Stale));
    }
    let TransactionObservation::Confirmed { block, .. } = *status.value() else {
        return Err(Refusal::retry(STEP1_NOT_DEEP));
    };
    let tip = evidence.tip(ChainId::Bitcoin).await.map_err(unavailable)?;
    let canonical = evidence
        .hash_at_height(ChainId::Bitcoin, block.height)
        .await
        .map_err(unavailable)?;
    if !fresh(evidence, tip.observed_at()) || !fresh(evidence, canonical.observed_at()) {
        return Err(unavailable(FailureKind::Stale));
    }
    let deep = tip
        .value()
        .height
        .checked_sub(block.height)
        .is_some_and(|below| below.saturating_add(1) >= MIN_CONFIRMATIONS);
    if *canonical.value() != block.hash || !deep {
        return Err(Refusal::retry(STEP1_NOT_DEEP));
    }

    for outpoint in &dead_end.claimed {
        let previous = evidence
            .previous_transaction(ChainId::Bitcoin, outpoint.txid)
            .await
            .map_err(unavailable)?;
        let address = (previous.compute_txid() == outpoint.txid)
            .then(|| usize::try_from(outpoint.vout).ok())
            .flatten()
            .and_then(|vout| previous.output.get(vout))
            .and_then(|output| Address::from_script(&output.script_pubkey, Network::Bitcoin).ok())
            .ok_or_else(|| Refusal::final_(step1::UNIDENTIFIED))?;
        let unspent = evidence
            .unspent_outputs(ChainId::BitcoinBlake2b, &address.to_string())
            .await
            .map_err(unavailable)?;
        if !fresh(evidence, unspent.observed_at()) {
            return Err(unavailable(FailureKind::Stale));
        }
        if !unspent.value().contains(outpoint) {
            return Err(Refusal::final_(COIN_SPENT_ON_BTCB2));
        }
    }

    step2_absent(evidence, dead_end.step2).await
}

/// #625 F2 (A1 = A): close a split in its step-2 dead end, after
/// [`check_close`] passed again. Under the journal's lock it must still be
/// in exactly that dead end; then its tombstone ([`step1::CLOSED`]) is
/// written atomically next to the journal, naming the source, the target
/// Cube, both recorded txids and the claimed inputs. The journal itself,
/// with the recorded signed step 2, is not changed or deleted. A failed
/// write closes nothing. `ended` is the panel's session flag: set by a
/// revocation after the close was confirmed, it refuses under the journal's
/// lock, right before the write (#644 r4176212750). Blocking: off the UI
/// thread, with every handle on the journal dropped first.
pub fn close(
    directory: &Path,
    target_cube: &str,
    digest: sha256::Hash,
    context: Context,
    dead_end: &DeadEnd,
    closed_at: i64,
    ended: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    let identity = claim_workflow::split_identity(target_cube.to_owned(), digest);
    let controller = Controller::reopen_settling_blocking(directory, &identity, context)
        .map_err(|error| step1::describe(claim_coordinator::Error::Journal(error)))?;
    if self::dead_end(&controller).as_ref() != Some(dead_end) {
        return Err(CHANGED_SINCE_CHECK.to_string());
    }
    if ended.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(step1::ENDED_BEFORE_ABANDON.to_string());
    }
    let tombstone = serde_json::json!({
        "version": 1,
        "source_digest": digest.to_string(),
        "target_cube": target_cube,
        "step1_txid": dead_end.step1.to_string(),
        "step2_txid": dead_end.step2.to_string(),
        "claimed_prevouts": dead_end
            .claimed
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        "closed_at": closed_at,
    });
    let bytes = serde_json::to_vec_pretty(&tombstone).map_err(|error| error.to_string())?;
    write_tombstone(directory, &bytes)
        .map_err(|error| format!("The split could not be abandoned ({error})."))
    // The controller, and the journal lock, end here.
}

/// Write `bytes` to `directory/closed.json` atomically: a private temporary
/// file, synced, renamed over the final name, then the directory synced.
fn write_tombstone(directory: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    // Unique per write: a temporary file a crashed run left behind is never
    // reused.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let temporary = directory.join(format!(".closed-{}-{}.tmp", std::process::id(), nonce));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, directory.join(step1::CLOSED))?;
        #[cfg(unix)]
        std::fs::File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// The session's Split fork production, built fresh for each open.
fn fork_production(
    session: &ConnectSession,
    expected: u64,
    generation: &watch::Receiver<u64>,
) -> Result<SplitForkProduction, Step2Refusal> {
    SplitForkProduction::new(
        session.client.clone(),
        session.account.clone(),
        expected,
        generation.clone(),
    )
    .map_err(|error| Step2Refusal::retry(step1::describe(error)))
}

/// The production step-2 port for the target Vault's daemon and one Connect
/// session.
pub struct ProductionStep2 {
    session: ConnectSession,
    generation: watch::Receiver<u64>,
    expected: u64,
    context: Context,
    daemon: Arc<dyn Daemon + Send + Sync>,
    vault: CoincubeDescriptor,
}
impl ProductionStep2 {
    /// Refused without an account, after the generation moved, and for any
    /// route the submission transport does not admit (#637 R2): a daemon
    /// that is not embedded or not on BTCB2 mainnet, or a backend other than
    /// exactly Connect's BTCB2 Esplora at this session's origin or a bound
    /// Bitcoind node. So nothing is reserved, built or signed for a step 2
    /// that could not be sent. `finish` admits the route again at the
    /// handoff, and the review binds it.
    pub fn new(
        session: ConnectSession,
        generation: watch::Receiver<u64>,
        daemon: Arc<dyn Daemon + Send + Sync>,
    ) -> Result<Self, claim_coordinator::Error> {
        let expected = *generation.borrow();
        SplitStep2Production::new(
            &session.client,
            daemon.clone(),
            expected,
            generation.clone(),
        )?;
        let vault = daemon
            .config()
            .map(|config| config.main_descriptor.clone())
            .ok_or(claim_coordinator::Error::Unsupported)?;
        let context = SplitForkProduction::new(
            session.client.clone(),
            session.account.clone(),
            expected,
            generation.clone(),
        )?
        .context()
        .clone();
        Ok(Self {
            session,
            generation,
            expected,
            context,
            daemon,
            vault,
        })
    }
    fn production(&self) -> Result<SplitForkProduction, Step2Refusal> {
        fork_production(&self.session, self.expected, &self.generation)
    }
}
#[async_trait]
impl Step2Port for ProductionStep2 {
    fn context(&self) -> Context {
        self.context.clone()
    }
    fn identity(&self) -> PortIdentity {
        PortIdentity {
            context: self.context.clone(),
            daemon: Arc::as_ptr(&self.daemon) as *const () as usize,
        }
    }
    fn open_preparation(&self, open: Step2Open) -> Result<Box<dyn Step2Prep>, Step2Refusal> {
        let preparation = SplitPreparation::resume(
            &open.directory,
            open.target_cube,
            &open.construction,
            open.verified,
            open.fork_height,
            self.production()?,
            CHECK_POLICY,
        )
        .map_err(describe_check)?;
        Ok(Box::new(PreparationDriver {
            core: LivePrep {
                preparation,
                daemon: self.daemon.clone(),
                vault: self.vault.clone(),
                fees: split_fees::btcb2_fee_source(Some(self.session.client.clone())),
            },
            token: None,
            finish: Some(FinishDeps {
                client: self.session.client.clone(),
                expected: self.expected,
                generation: self.generation.clone(),
            }),
        }))
    }
    async fn reopen_for_resend(
        &self,
        connect: Arc<dyn SplitConnect>,
        directory: PathBuf,
        target_cube: String,
        digest: sha256::Hash,
    ) -> Result<Box<dyn Step2Coord>, Step2Refusal> {
        // The route is admitted again, as at the handoff (#637 R2).
        let transport = SplitStep2Production::new(
            &self.session.client,
            self.daemon.clone(),
            self.expected,
            self.generation.clone(),
        )
        .map_err(describe_check)?;
        let route = transport.route();
        let restored = step1::restore(&*connect, &directory, &target_cube, digest)
            .await
            .map_err(|refusal| Step2Refusal {
                reason: refusal.reason,
                retry: refusal.retry,
                recovery: match refusal.recovery {
                    step1::RefusalRecovery::ReopenCube => Step2Recovery::ReopenCube,
                    step1::RefusalRecovery::None => Step2Recovery::None,
                },
            })?;
        let coordinator = SplitStep2Coordinator::resume_uncertain(
            &directory,
            target_cube,
            &restored.construction,
            restored.verified,
            restored.fork_height,
            restored.coins,
            self.production()?,
            transport,
            CHECK_POLICY,
        )
        .await
        .map_err(describe_check)?;
        Ok(Box::new(CoordinatorDriver::new(
            coordinator,
            route,
            self.generation.clone(),
            self.expected,
        )))
    }
}

/// The production reconcile-only port: one Connect session and no Vault
/// daemon (#637 R1).
pub struct ProductionRecon {
    session: ConnectSession,
    generation: watch::Receiver<u64>,
    expected: u64,
    context: Context,
}
impl ProductionRecon {
    /// Refused without an account, for an unusable origin, or after the
    /// generation moved.
    pub fn new(
        session: ConnectSession,
        generation: watch::Receiver<u64>,
    ) -> Result<Self, claim_coordinator::Error> {
        let expected = *generation.borrow();
        let context = SplitForkProduction::new(
            session.client.clone(),
            session.account.clone(),
            expected,
            generation.clone(),
        )?
        .context()
        .clone();
        Ok(Self {
            session,
            generation,
            expected,
            context,
        })
    }
}
impl ReconPort for ProductionRecon {
    fn context(&self) -> Context {
        self.context.clone()
    }
    fn open_reconciler(
        &self,
        directory: PathBuf,
        target_cube: String,
        digest: sha256::Hash,
    ) -> Result<Box<dyn Step2Recon>, Step2Refusal> {
        let reconciler = SplitStep2Reconciler::resume(
            &directory,
            target_cube,
            digest,
            fork_production(&self.session, self.expected, &self.generation)?,
            CHECK_POLICY,
        )
        .map_err(describe_check)?;
        Ok(Box::new(ReconcilerDriver(reconciler)))
    }
}

/// What the driver needs from a step-2 preparation: the production one is
/// [`LivePrep`] over `SplitPreparation`; tests substitute a fake to pin the
/// driver's own logic (#636 P3-1).
#[async_trait]
trait PrepCore: Send {
    fn revoke_handle(&self) -> RevokeHandle;
    fn needs_reservation(&self) -> Result<bool, claim_coordinator::Error>;
    fn recorded_target(&self) -> Result<Option<u32>, claim_coordinator::Error>;
    async fn check_signing(
        &mut self,
        context: &Context,
    ) -> Result<ForeignStep2Authorization, SplitCheckError>;
    async fn reserve(&mut self, context: &Context) -> Result<u32, TargetError>;
    async fn prove(&mut self, context: &Context) -> Result<(), TargetError>;
    async fn construct(
        &mut self,
        context: &Context,
        token: ForeignStep2Authorization,
        coins: Vec<SplitCoin>,
    ) -> Result<Psbt, Step2Error>;
    fn check_signed(
        &self,
        signed: &Psbt,
        coins: &[SplitCoin],
    ) -> Result<(), coincube_core::foreign_split::FinalizeError>;
}

/// The production preparation and what it reserves and prices with.
struct LivePrep {
    preparation: SplitPreparation,
    daemon: Arc<dyn Daemon + Send + Sync>,
    vault: CoincubeDescriptor,
    fees: Arc<dyn SweepFeeSource>,
}
#[async_trait]
impl PrepCore for LivePrep {
    fn revoke_handle(&self) -> RevokeHandle {
        let revoker = self.preparation.revoker();
        Arc::new(move || revoker.revoke())
    }
    fn needs_reservation(&self) -> Result<bool, claim_coordinator::Error> {
        self.preparation.needs_reservation()
    }
    fn recorded_target(&self) -> Result<Option<u32>, claim_coordinator::Error> {
        self.preparation.recorded_target()
    }
    async fn check_signing(
        &mut self,
        context: &Context,
    ) -> Result<ForeignStep2Authorization, SplitCheckError> {
        self.preparation.check_signing(context).await
    }
    async fn reserve(&mut self, context: &Context) -> Result<u32, TargetError> {
        let daemon = self.daemon.clone();
        self.preparation
            .reserve_target(
                context,
                &self.vault,
                async move { daemon.get_new_address().await },
                RESERVATION_BOUND,
            )
            .await
    }
    async fn prove(&mut self, context: &Context) -> Result<(), TargetError> {
        self.preparation.prove_target(context, &self.vault).await
    }
    async fn construct(
        &mut self,
        context: &Context,
        token: ForeignStep2Authorization,
        coins: Vec<SplitCoin>,
    ) -> Result<Psbt, Step2Error> {
        self.preparation
            .construct_step2(context, token, coins, &*self.fees)
            .await
    }
    fn check_signed(
        &self,
        signed: &Psbt,
        coins: &[SplitCoin],
    ) -> Result<(), coincube_core::foreign_split::FinalizeError> {
        self.preparation.check_signed(signed, coins)
    }
}

/// What `finish` needs to admit the transport.
struct FinishDeps {
    client: crate::services::coincube::CoincubeClient,
    expected: u64,
    generation: watch::Receiver<u64>,
}

struct PreparationDriver<P> {
    core: P,
    token: Option<ForeignStep2Authorization>,
    finish: Option<FinishDeps>,
}
impl<P: PrepCore> PreparationDriver<P> {
    /// A new check supersedes and clears any earlier token first, whatever
    /// its own result (#636 P3-1).
    async fn check_inner(&mut self, context: &Context) -> Result<CannotReplay, Step2Refusal> {
        self.token = None;
        let token = self
            .core
            .check_signing(context)
            .await
            .map_err(describe_split_check)?;
        let label = evidence_of(&token);
        self.token = Some(token);
        Ok(label)
    }
    /// Reserve when the journal needs one (a journal error refuses; it is
    /// never read as "no reservation needed"), prove, and replace a target
    /// proven used exactly once. A second used target refuses with no third
    /// reservation; an unavailable or stale proof never replaces anything.
    async fn ensure_target_inner(&mut self, context: &Context) -> Result<u32, Step2Refusal> {
        if self.core.needs_reservation().map_err(describe_check)? {
            self.core.reserve(context).await.map_err(describe_target)?;
        }
        match self.core.prove(context).await {
            Ok(()) => {}
            Err(TargetError::Used(_)) => {
                self.core.reserve(context).await.map_err(describe_target)?;
                self.core.prove(context).await.map_err(describe_target)?;
            }
            Err(error) => return Err(describe_target(error)),
        }
        self.core
            .recorded_target()
            .map_err(describe_check)?
            .ok_or_else(|| describe_target(TargetError::NoReservation))
    }
    /// One build per check: the token is taken before construction, so a
    /// failed construction also needs a new check.
    async fn build_inner(
        &mut self,
        context: &Context,
        coins: Vec<SplitCoin>,
    ) -> Result<Psbt, Step2Refusal> {
        let token = self.token.take().ok_or_else(|| {
            Step2Refusal::retry("Check step 1's confirmations again before building step 2.")
        })?;
        self.core
            .construct(context, token, coins)
            .await
            .map_err(describe_step2)
    }
    fn verify_inner(&self, signed: &Psbt, coins: &[SplitCoin]) -> Result<bool, Step2Refusal> {
        use coincube_core::foreign_split::FinalizeError;
        match self.core.check_signed(signed, coins) {
            Ok(()) => Ok(true),
            Err(FinalizeError::Unsatisfied) => Ok(false),
            Err(error) => Err(Step2Refusal::final_(format!(
                "The signed step 2 does not match what was built ({error}). Nothing was sent."
            ))),
        }
    }
}
#[async_trait]
impl Step2Prep for PreparationDriver<LivePrep> {
    fn revoke_handle(&self) -> RevokeHandle {
        self.core.revoke_handle()
    }
    /// Display and restart data only: a journal error reads as `false` here
    /// and decides nothing (`ensure_target` asks the journal itself).
    fn needs_reservation(&self) -> bool {
        self.core.needs_reservation().unwrap_or(false)
    }
    async fn check(&mut self, context: &Context) -> Result<CannotReplay, Step2Refusal> {
        self.check_inner(context).await
    }
    async fn ensure_target(&mut self, context: &Context) -> Result<u32, Step2Refusal> {
        self.ensure_target_inner(context).await
    }
    async fn build(
        &mut self,
        context: &Context,
        coins: Vec<SplitCoin>,
    ) -> Result<Psbt, Step2Refusal> {
        self.build_inner(context, coins).await
    }
    fn verify_signed(&self, signed: &Psbt, coins: &[SplitCoin]) -> Result<bool, Step2Refusal> {
        self.verify_inner(signed, coins)
    }
    fn finish(
        self: Box<Self>,
        context: &Context,
        signed: &Psbt,
        coins: &[SplitCoin],
    ) -> Result<Box<dyn Step2Coord>, FinishRefusal> {
        let Some(deps) = self.finish.as_ref() else {
            return Err((
                Step2Refusal::final_("This preparation cannot hand over."),
                Some(self),
            ));
        };
        let transport = match SplitStep2Production::new(
            &deps.client,
            self.core.daemon.clone(),
            deps.expected,
            deps.generation.clone(),
        ) {
            Ok(transport) => transport,
            Err(error) => return Err((describe_check(error), Some(self))),
        };
        let route = transport.route();
        let (generation, expected) = (deps.generation.clone(), deps.expected);
        // `finish` consumes the preparation: a refused handoff releases the
        // journal, and the panel reopens it to try again.
        self.core
            .preparation
            .finish(context, signed, coins, transport)
            .map(|coordinator| {
                Box::new(CoordinatorDriver::new(
                    coordinator,
                    route,
                    generation,
                    expected,
                )) as Box<dyn Step2Coord>
            })
            .map_err(|error| (describe_check(error), None))
    }
}

struct CoordinatorDriver {
    coordinator: SplitStep2Coordinator,
    review: Option<Review>,
    /// P3-3: the last resend review, used up by its confirmation.
    resend: Option<Step2ResubmissionReview>,
    route: SubmissionRoute,
    /// The session generation the coordinator works under, for the resend
    /// review's liveness.
    generation: watch::Receiver<u64>,
    expected: u64,
}
impl CoordinatorDriver {
    fn new(
        coordinator: SplitStep2Coordinator,
        route: SubmissionRoute,
        generation: watch::Receiver<u64>,
        expected: u64,
    ) -> Self {
        Self {
            coordinator,
            review: None,
            resend: None,
            route,
            generation,
            expected,
        }
    }
}
#[async_trait]
impl Step2Coord for CoordinatorDriver {
    fn revoke_handle(&self) -> RevokeHandle {
        let revoker = self.coordinator.revoker();
        Arc::new(move || revoker.revoke())
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        self.coordinator.recorded_outcome()
    }
    async fn review(&mut self, context: &Context) -> Result<Step2ReviewView, Step2Refusal> {
        self.review = None;
        let review = self
            .coordinator
            .prepare_review(context)
            .await
            .map_err(describe_check)?;
        let snapshot = review.snapshot();
        debug_assert_eq!(snapshot.route, self.route);
        let (route_label, privacy_note) = route_copy(snapshot.route);
        let view = Step2ReviewView {
            txid: snapshot.txid,
            fee_sats: snapshot.fee_sats,
            vsize: snapshot.vsize,
            route: snapshot.route,
            route_label,
            privacy_note,
        };
        self.review = Some(review);
        Ok(view)
    }
    async fn submit(&mut self, context: &Context) -> Result<Outcome, Step2Refusal> {
        let review = self
            .review
            .take()
            .ok_or_else(|| Step2Refusal::retry("Review step 2 again before confirming."))?;
        self.coordinator
            .confirm_and_submit(review, context)
            .await
            .map_err(describe_check)
    }
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<(Status, TransactionObservation, Step1AfterStep2), Step2Refusal> {
        self.review = None;
        self.resend = None;
        self.coordinator
            .reconcile_sweep(context)
            .await
            .map(reconciled)
            .map_err(describe_check)
    }
    async fn review_resend(&mut self, context: &Context) -> Result<Step2ResendView, Step2Refusal> {
        self.review = None;
        self.resend = None;
        let review = self
            .coordinator
            .prepare_step2_resubmission(context)
            .await
            .map_err(describe_resend)?;
        let snapshot = review.snapshot();
        debug_assert_eq!(snapshot.route, self.route);
        let (route_label, privacy_note) = route_copy(snapshot.route);
        let not_after = review.not_after();
        let left = not_after.saturating_duration_since(Instant::now());
        let revoker = self.coordinator.revoker();
        let view = Step2ResendView {
            txid: snapshot.txid,
            route: snapshot.route,
            route_label,
            privacy_note,
            attempt: review.previous_attempts().saturating_add(1),
            max_attempts: claim_workflow::MAX_SPLIT_STEP2_RESUBMISSIONS,
            expires_at: chrono::Local::now()
                + chrono::Duration::from_std(left).unwrap_or_else(|_| chrono::Duration::zero()),
            live: resend_liveness(
                move || revoker.is_revoked(),
                self.generation.clone(),
                self.expected,
                not_after,
            ),
        };
        self.resend = Some(review);
        Ok(view)
    }
    async fn confirm_resend(&mut self, context: &Context) -> Result<Outcome, Step2Refusal> {
        self.review = None;
        let review = self
            .resend
            .take()
            .ok_or_else(|| Step2Refusal::retry("Review the resend again before sending."))?;
        self.coordinator
            .confirm_step2_resubmission(review, context)
            .await
            .map_err(describe_resend)
    }
}

struct ReconcilerDriver(SplitStep2Reconciler);
#[async_trait]
impl Step2Recon for ReconcilerDriver {
    fn revoke_handle(&self) -> RevokeHandle {
        let revoker = self.0.revoker();
        Arc::new(move || revoker.revoke())
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        self.0.recorded_outcome()
    }
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<(Status, TransactionObservation, Step1AfterStep2), Step2Refusal> {
        self.0
            .reconcile_sweep(context)
            .await
            .map(reconciled)
            .map_err(describe_check)
    }
}

/// What the panel keeps of a reconcile: the journal's status, step 2 on
/// BTCB2, and what became of step 1 after the step-2 submission.
fn reconciled(r: SweepReconcile) -> (Status, TransactionObservation, Step1AfterStep2) {
    (r.status, r.step2, r.after_step2)
}

#[cfg(all(test, unix))]
mod tests;
