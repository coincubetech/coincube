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
//! - **Completion** (#568 B5b). After a reconcile that saw step 2 confirmed
//!   with step 1 eligible, [`Step2Recon::complete`] checks both chains once
//!   more, records the digest-only completion on the target Cube
//!   ([`CompletionSite`]), then deletes the source's descriptors (D18). The
//!   submission coordinator has no completion check:
//!   [`complete_from_coordinator`] drops it, then completes through the
//!   reconciler. A refused completion is "check again", never terminal
//!   (#645 P3-1); [`Step2Recon::completion_stands`] clears the record after
//!   a reorg (D17).
//!
//! - **Closing a dead end** (#625 F2, A1 = A). A recorded step 2 that no
//!   resend can follow and no read ever saw ([`DeadEnd`]) may be closed after
//!   [`check_close`]: [`close`] leaves the journal, recorded bytes included,
//!   and writes its tombstone, so discovery skips it and a new split of the
//!   same source stays refused until the owner removes the tombstone.
//! - **The O1 acknowledgement** (#568 S4b, S4-D3). When a reconcile finds
//!   step 1 re-mined in another Bitcoin block (`Remined`), the handle the
//!   panel holds (the coordinator or the reconciler) reviews it
//!   ([`Step2Coord::review_reconfirmation`],
//!   [`Step2Recon::review_reconfirmation`]): one-use, expiring
//!   ([`ReconfirmationView`]), refused past the RDTS margin (S4-D4,
//!   [`describe_reconfirmation`]). Confirming exactly it records the new
//!   block for step 1. Nothing is sent.
//! - **The O4 exit** (#568 S4b, Legolas F3). A recorded *terminal* step-1
//!   conflict is a dead end too ([`DeadEnd::conflict`]): step 1 can never
//!   confirm, so no resend can follow and the split can't complete. Its
//!   close does not rest on step 1's depth: [`check_close`] reads Bitcoin
//!   fresh (step 1 absent, the conflicting coin still missing from its
//!   address's unspent outputs, step 1 absent again) and the same tombstone
//!   is written. Step 2's bytes on BTCB2 stand; nothing is sent. A
//!   provisional conflict is no dead end (S4-D5).
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
    claim::{Assessment, BlockRef, MIN_CONFIRMATIONS},
    descriptors::CoincubeDescriptor,
    foreign_split::{SplitCoin, SplitStep1, VerifiedSplitStep1},
    miniscript::bitcoin::{hashes::sha256, psbt::Psbt, Address, Network, OutPoint, Txid},
};

use super::step1::{self, OpenRequest, Refusal, RevokeHandle, SplitConnect, Step1Driver};
use crate::{
    app::{
        settings::{CubeSettings, SettingsError, SplitFromRecord},
        state::vault::claim::{
            describe_duration, ConnectSession, CHECK_POLICY, EXPIRY_MARGIN_SECONDS,
        },
    },
    daemon::Daemon,
    dir::CoincubeDirectory,
    services::{
        claim_coordinator::{
            self,
            fork::split::{
                step2::{
                    CompletionTarget, ResendError, SplitCompletionEvidence,
                    SplitCompletionReconciliation, SplitStep2Coordinator, SplitStep2Production,
                    SplitStep2Reconciler, Step1AfterStep2, Step1ReconfirmationReview, Step2Error,
                    Step2ResubmissionReview, SweepReconcile, TargetError, RESERVATION_BOUND,
                },
                ForeignStep2Authorization, SplitCheckError, SplitForkProduction, SplitPreparation,
                Step2Liveness,
            },
            Outcome, Review, SubmissionRoute,
        },
        claim_observation::{FailureKind, TransactionObservation},
        claim_workflow::{self, Context, Controller, Status, Step1Conflict},
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
/// Why the panel has no step-2 port under a session (S3-D4): the App says,
/// from the port build, and the copy names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step2Unavailable {
    /// No Vault daemon to send step 2 through: none loaded, a backend
    /// switch in flight, or one that failed or panicked.
    NoDaemon,
    /// The daemon's route can't carry step 2 (`Unsupported`): neither
    /// Connect's Bitcoin Blake2b Esplora nor a bound Bitcoin Blake2b node.
    UnsupportedRoute,
    /// The port was refused otherwise (the session or its generation).
    Refused,
}
pub const STEP2_NEEDS_VAULT: &str = "Step 1 has the confirmations step 2 needs. Step 2 needs this Vault's wallet engine running; the split stays recorded on this device.";
pub const STEP2_UNSUPPORTED_ROUTE: &str = "Step 1 has the confirmations step 2 needs. Step 2 can be sent only through Connect's Bitcoin Blake2b server or this Vault's own Bitcoin Blake2b node, and this Vault's wallet engine uses neither. Switch its backend to one of them to continue; the split stays recorded on this device.";
pub const STEP2_REFUSED: &str = "Step 1 has the confirmations step 2 needs. Step 2 can't be opened under this session; close and reopen the Cube to try again. The split stays recorded on this device.";

/// The copy for a missing step-2 port.
pub fn unavailable_copy(reason: Step2Unavailable) -> &'static str {
    match reason {
        Step2Unavailable::NoDaemon => STEP2_NEEDS_VAULT,
        Step2Unavailable::UnsupportedRoute => STEP2_UNSUPPORTED_ROUTE,
        Step2Unavailable::Refused => STEP2_REFUSED,
    }
}

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

/// #568 B5b: the Completed stage's copy.
pub const SPLIT_COMPLETED: &str = "This split is complete. Step 2 has at least 6 confirmations on Bitcoin Blake2b and step 1 at least 6 on Bitcoin. This Cube records the split only by a digest (a hash) of the source wallet's descriptor, and the source wallet's descriptors were deleted from this device.";
/// The completion check found the split not complete (yet).
pub const COMPLETION_NOT_YET: &str = "This split isn't complete yet: step 2 needs 6 confirmations on Bitcoin Blake2b while step 1 stays 6 deep in its block on Bitcoin. Nothing was recorded or deleted. Check status again later.";
/// #645 P3-1: the completion's evidence lapsed while it was being saved.
/// Never terminal: a fresh check completes it. The record may already be
/// written (`persist` checks the evidence again after its write, #656 F2);
/// a re-completion does not write it twice.
pub const COMPLETION_EXPIRED: &str = "The completion check expired while it was being saved, so the completion may already be recorded in this Cube. Nothing was deleted. Check status again, then complete the split.";
/// #656 F1: the completion check's own evidence lapsed before anything was
/// saved.
pub const COMPLETION_CHECK_EXPIRED: &str = "The completion check expired before anything was saved, so nothing was recorded or deleted. Check status again, then complete the split.";
/// #656 F1: the target Cube's settings could not be read or updated for
/// this split's completion record (an unreadable settings file, or the Cube
/// or its Vault no longer matching). The settings layer's own messages name
/// a "Claim Cube"; this copy is Split's.
pub const COMPLETION_RECORD_UNAVAILABLE: &str = "This Cube's completion record for this split couldn't be read or updated: its settings file couldn't be read or written, or the Cube or its Vault no longer matches this split. Nothing was deleted. Check status again.";
/// The target Cube's settings refused the record while its evidence was
/// still live. The settings layer's own messages name a "Claim Cube"
/// (`matching_completion_cube`); this copy is Split's.
pub const COMPLETION_NOT_RECORDED: &str = "This Cube's settings couldn't record the completion: the Cube, or the Vault it names, no longer matches this split, or its settings file couldn't be written. Nothing was deleted. Check status again.";
/// The record was written but its evidence lapsed before the descriptors
/// were deleted: a fresh check finishes it (the record is not written
/// twice).
pub const COMPLETION_NOT_FORGOTTEN: &str = "The completion is recorded in this Cube, but the check expired before the source wallet's descriptors were deleted from this device. Check status again, then complete the split to finish.";
/// The target Cube's settings name no Vault: there is nothing to record a
/// completion into.
pub const COMPLETION_NO_VAULT: &str = "This Cube's settings don't name its Vault, so this split's completion can't be recorded here. Nothing was recorded or deleted.";
/// The completion task ended without returning its reconciler.
pub const COMPLETION_INTERRUPTED: &str =
    "Completing the split was interrupted. Its status can still be checked.";
/// D17: after completion, a check found step 2 out of the block the record
/// names, or step 1 below its depth.
pub const COMPLETION_LOST: &str = "A reorganization undid this split's completion: step 2 left the Bitcoin Blake2b block its completion was recorded at, or step 1 lost its Bitcoin confirmations. The completion record was removed from this Cube; the source wallet's descriptors stay deleted. Check status again.";

/// #568 B5b: what a completion recorded on the target Cube, the panel's
/// history row (D16: panel only). Digest only, as the settings record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitCompletion {
    pub source_digest: sha256::Hash,
    pub completed_height: u64,
    pub step2_txid: Txid,
}
impl From<SplitFromRecord> for SplitCompletion {
    fn from(record: SplitFromRecord) -> Self {
        Self {
            source_digest: record.descriptor_digest,
            completed_height: record.completed_height,
            step2_txid: record.step2_txid,
        }
    }
}
impl SplitCompletion {
    /// The source digest's short form: its first 16 hex digits.
    pub fn short_digest(&self) -> String {
        self.source_digest.to_string().chars().take(16).collect()
    }
    /// The history row: short source digest, BTCB2 height, step-2 txid.
    pub fn history_row(&self) -> String {
        format!(
            "Split from source {}… · Bitcoin Blake2b height {} · step 2 {}",
            self.short_digest(),
            self.completed_height,
            self.step2_txid
        )
    }
}

/// #568 B5b: where a completion is recorded. The datadir whose Bitcoin
/// Blake2b network directory holds the target Cube's settings, and the
/// target Cube with the Vault its settings name.
#[derive(Debug, Clone)]
pub struct CompletionSite {
    root: CoincubeDirectory,
    target: CompletionTarget,
}
impl CompletionSite {
    /// The completion site of `cube` under `root`; `None` while the Cube's
    /// settings name no Vault.
    pub fn of(root: CoincubeDirectory, cube: &CubeSettings) -> Option<Self> {
        Some(Self {
            root,
            target: CompletionTarget::of(cube)?,
        })
    }
}

/// D17: what a check of a completed split found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionStanding {
    /// Step 2 is still in the recorded block and step 1 still deep.
    Standing {
        status: Status,
        seen: TransactionObservation,
    },
    /// The completion was undone; its record was removed when `cleared`.
    Lost {
        status: Status,
        seen: TransactionObservation,
        cleared: bool,
    },
}

/// A revoke handle bound after the fact (#568 B5b): the panel holds
/// [`Self::handle`] while a task opens the handle it stands for. A
/// revocation before the binding revokes the bound handle at once.
#[derive(Clone, Default)]
pub struct RevokeSlot(Arc<std::sync::Mutex<(bool, Option<RevokeHandle>)>>);
impl RevokeSlot {
    pub fn handle(&self) -> RevokeHandle {
        let slot = self.0.clone();
        Arc::new(move || {
            let bound = {
                let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
                slot.0 = true;
                slot.1.take()
            };
            if let Some(revoke) = bound {
                revoke();
            }
        })
    }
    /// Bind `revoke`; `false`, with `revoke` called, when the slot was
    /// revoked first.
    fn bind(&self, revoke: RevokeHandle) -> bool {
        let revoked = {
            let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if !slot.0 {
                slot.1 = Some(revoke.clone());
            }
            slot.0
        };
        if revoked {
            revoke();
        }
        !revoked
    }
}

/// What a reconcile after the step-2 submission found of step 1 on Bitcoin
/// means (#637 r4172242637; #568 S4): nothing while it is still eligible
/// (six deep in its recorded block, absent from BTCB2); otherwise one
/// warning per outcome, each naming the exposure (while step 1 isn't
/// confirmed, step 2's recorded bytes could be mined on Bitcoin; for a
/// terminal conflict, step 1 can never confirm and the split can't
/// complete; a provisional one may still be unconfirmed, S4-D5). A reorg
/// is named only for O1 to O4. Each leaves checking status again, except a
/// terminal conflict, whose way out is S4b's close after a fresh check of
/// Bitcoin (#658). None offers acknowledging a new block or resending step
/// 1 itself (those controls are S4b's, S4-D3), and none shows the "cannot
/// replay" label.
pub fn reconcile_warning(after: Step1AfterStep2) -> Option<String> {
    const RECORDED: &str = "Bitcoin reorganized after a submission of step 2 was recorded; it was sent or may have been sent.";
    const UNCHANGED: &str =
        "Nothing was rebuilt, and step 2 is not sent automatically. Check status again later.";
    const TERMINAL: &str = "Nothing was rebuilt, and step 2 is not sent automatically. You can close this split after a fresh check of Bitcoin.";
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
        // #658: the close beside it is the way out, not another check.
        Step1AfterStep2::Conflict(conflict) if conflict.is_terminal() => Some(format!(
            "{RECORDED} Step 1 is no longer in any Bitcoin block, and a coin this split claims ({}) was spent on Bitcoin by another transaction, so step 1 can never confirm and this split can't complete. {TERMINAL}",
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
    pub(super) fn final_(reason: impl Into<String>) -> Self {
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

pub(super) fn chain_name(chain: coincube_core::chain::ChainId) -> &'static str {
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

/// S4-D4: the O1 acknowledgement is refused once Bitcoin Blake2b's replay
/// protection is past its margin, so this split can't complete.
pub fn rdts_reconfirmation_refusal(expired: bool) -> String {
    if expired {
        "Step 1's new Bitcoin block can't be recorded: Bitcoin Blake2b's replay protection has expired, so this split can't complete. Nothing was recorded or sent.".to_string()
    } else {
        format!(
            "Step 1's new Bitcoin block can't be recorded: Bitcoin Blake2b's replay protection expires within {}, so this split can't complete. Nothing was recorded or sent.",
            describe_duration(EXPIRY_MARGIN_SECONDS)
        )
    }
}
/// The O1 review's evidence lapsed or the chains changed under it.
pub const RECONFIRMATION_AGAIN: &str = "The chains changed or the review of step 1's new block expired before it was acknowledged. Nothing was recorded or sent; review it again.";

/// Copy for a refused O1 review or acknowledgement (#568 S4b). Nothing was
/// sent; S4-D4's refusal past the RDTS margin is final.
pub fn describe_reconfirmation(error: claim_coordinator::Error) -> Step2Refusal {
    use claim_coordinator::Error as E;
    match error {
        E::NotReady(Assessment::ExpiryMargin) => {
            Step2Refusal::final_(rdts_reconfirmation_refusal(false))
        }
        E::NotReady(Assessment::RdtsExpired) => {
            Step2Refusal::final_(rdts_reconfirmation_refusal(true))
        }
        E::ExpiredEvidence | E::ChangedReview | E::InvalidReview => {
            Step2Refusal::retry(RECONFIRMATION_AGAIN)
        }
        other => describe_check(other),
    }
}

pub(super) fn describe_check(error: claim_coordinator::Error) -> Step2Refusal {
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

/// #656 F1: copy for a refused completion check, D17 recheck or descriptor
/// deletion. Expired evidence and the completion record's persistence are
/// Split's own lines here, never the Claim or submission copy
/// [`describe_check`] falls back to; everything else reads as there.
pub(super) fn describe_completion(error: claim_coordinator::Error) -> Step2Refusal {
    use claim_coordinator::Error as E;
    match error {
        E::ExpiredEvidence => Step2Refusal::retry(COMPLETION_CHECK_EXPIRED),
        E::CompletionPersistence(error) => {
            log::warn!("Unable to read or update the Split completion record: {error}");
            Step2Refusal::retry(COMPLETION_RECORD_UNAVAILABLE)
        }
        other => describe_check(other),
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
    /// When the label lapses at the latest (S3 item 5): the panel redraws
    /// then, without waiting for input.
    pub fn not_after(&self) -> Instant {
        self.live.not_after()
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
    /// The same deadline on the monotonic clock: the panel drops the review
    /// then, without waiting for input (S3 item 5).
    not_after: Instant,
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
    /// When the review lapses at the latest.
    pub fn not_after(&self) -> Instant {
        self.not_after
    }
}

/// O1 (#568 S4b): a review of step 1 re-mined in another Bitcoin block
/// after the step-2 submission, on screen until it is used, lapses or its
/// handle is revoked.
#[derive(Clone)]
pub struct ReconfirmationView {
    /// The block recorded for step 1.
    pub previous: BlockRef,
    /// The block step 1 is now confirmed in.
    pub confirmed: BlockRef,
    /// Step 1's confirmations in it at the review.
    pub confirmations: u64,
    /// When the review's evidence lapses, for display.
    pub expires_at: chrono::DateTime<chrono::Local>,
    /// The same deadline on the monotonic clock: the panel drops the review
    /// then (S3 item 5).
    not_after: Instant,
    live: ResendLiveness,
}
impl std::fmt::Debug for ReconfirmationView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconfirmationView")
            .field("previous", &self.previous)
            .field("confirmed", &self.confirmed)
            .finish_non_exhaustive()
    }
}
impl ReconfirmationView {
    /// The view of `review`, live while `live` says so (and before its
    /// deadline).
    fn of(review: &Step1ReconfirmationReview, live: ResendLiveness) -> Self {
        let not_after = review.not_after();
        let left = not_after.saturating_duration_since(Instant::now());
        let inclusion = review.inclusion();
        Self {
            previous: inclusion.previous,
            confirmed: inclusion.confirmed,
            confirmations: review.confirmations(),
            expires_at: chrono::Local::now()
                + chrono::Duration::from_std(left).unwrap_or_else(|_| chrono::Duration::zero()),
            not_after,
            live,
        }
    }
    /// Until its deadline, while its handle is not revoked and the
    /// session's generation has not moved.
    pub fn is_live(&self) -> bool {
        (self.live)()
    }
    /// When the review lapses at the latest.
    pub fn not_after(&self) -> Instant {
        self.not_after
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
    /// O1 (#568 S4b): a fresh one-use review of step 1 re-mined in another
    /// Bitcoin block, replacing any earlier one (and dropping any resend
    /// review). Records and sends nothing.
    async fn review_reconfirmation(
        &mut self,
        context: &Context,
    ) -> Result<ReconfirmationView, Step2Refusal>;
    /// O1: acknowledge exactly the last such review, which is used up
    /// whatever the result: the new block becomes step 1's recorded one.
    /// Sends nothing.
    async fn confirm_reconfirmation(&mut self, context: &Context) -> Result<(), Step2Refusal>;
}

/// After a recorded step-2 submission: reconcile, and complete (#568 B5b).
#[async_trait]
pub trait Step2Recon: Send {
    fn revoke_handle(&self) -> RevokeHandle;
    fn recorded_outcome(&self) -> Option<Outcome>;
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<(Status, TransactionObservation, Step1AfterStep2), Step2Refusal>;
    /// #568 B5b: a fresh completion check; with its evidence, the record on
    /// the target Cube, then (D18) the descriptors deleted from the journal
    /// (a blocking write, off the async executor). Every refusal is "check
    /// again" (#645 P3-1) except a Cube with no Vault to record into.
    async fn complete(&mut self, context: &Context) -> Result<SplitCompletion, Step2Refusal>;
    /// D17: whether the recorded completion still stands; a loss removes the
    /// record (the descriptors stay deleted).
    async fn completion_stands(
        &mut self,
        context: &Context,
    ) -> Result<CompletionStanding, Step2Refusal>;
    /// O1 (#568 S4b): see [`Step2Coord::review_reconfirmation`].
    async fn review_reconfirmation(
        &mut self,
        context: &Context,
    ) -> Result<ReconfirmationView, Step2Refusal>;
    /// O1: see [`Step2Coord::confirm_reconfirmation`].
    async fn confirm_reconfirmation(&mut self, context: &Context) -> Result<(), Step2Refusal>;
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

/// #568 B5b, the plan's `Step2Coord::complete`. The submission coordinator
/// has no completion check (only the reconciler mints completion evidence),
/// so completing from it drops the coordinator first, releasing its journal
/// lock, then opens the reconciler off the UI thread and completes through
/// it. `bound` is the panel's revoke handle for the task: a revocation
/// before the reconciler opens revokes it as soon as it does, and nothing
/// is completed. The reconciler comes back whatever the completion's
/// result, so the panel keeps reconciling; `None` when it could not be
/// opened or was revoked (the panel then reads the journal again).
pub async fn complete_from_coordinator(
    coord: Box<dyn Step2Coord>,
    port: Arc<dyn ReconPort>,
    directory: PathBuf,
    target_cube: String,
    digest: sha256::Hash,
    context: Context,
    bound: RevokeSlot,
) -> (
    Option<Box<dyn Step2Recon>>,
    Result<SplitCompletion, Step2Refusal>,
) {
    drop(coord);
    let opened =
        tokio::task::spawn_blocking(move || port.open_reconciler(directory, target_cube, digest))
            .await;
    let mut recon = match opened {
        Ok(Ok(recon)) => recon,
        Ok(Err(refusal)) => return (None, Err(refusal)),
        Err(_) => return (None, Err(Step2Refusal::retry(COMPLETION_INTERRUPTED))),
    };
    if !bound.bind(recon.revoke_handle()) {
        return (None, Err(Step2Refusal::retry(COMPLETION_INTERRUPTED)));
    }
    let result = recon.complete(&context).await;
    (Some(recon), result)
}

/// Which side of the journal a restart reopens.
pub enum Restart {
    /// No step-2 submission recorded: resume step 1 as before
    /// (`SplitPanel::resume`).
    Step1,
    /// A step-2 submission is recorded: reconcile only, with its dead end
    /// when it is in one (#625 F2, or O4's terminal conflict, #568 S4b), or
    /// else why a resend the journal allows could not be opened (P3-3). At
    /// most one is set: a journal in a dead end allows no resend.
    Reconcile(Box<dyn Step2Recon>, Option<DeadEnd>, Option<String>),
    /// A step-2 submission is recorded and the journal allows a reviewed
    /// resend (P3-3): its coordinator, reopened from the recorded bytes.
    Resend(Box<dyn Step2Coord>),
    /// #625 F2: the split was closed in its step-2 dead end. Nothing opens.
    Closed,
    /// #568 B4b-3c: a fork-only (`kind: Unified`) record. Neither step 1's
    /// restore nor the two-step reconciler can open it (U2): the panel opens
    /// it through its unified port, by whether a submission is recorded.
    Unified(super::unified::UnifiedRecord),
}

/// #625 F2: a recorded step 2 that no resend can follow and no read ever
/// saw on BTCB2 ([`Controller::split_step2_dead_end`]), as the restart read
/// it; or (#568 S4b, O4) a recorded step 2 whose step 1 has a recorded
/// terminal conflict, whatever became of step 2. What [`check_close`] looks
/// for; it grants nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadEnd {
    /// The recorded signed step 1's own txid.
    pub step1: Txid,
    /// The recorded signed step 2's own txid.
    pub step2: Txid,
    /// Step 1's claimed inputs: the coins step 2 spends on BTCB2.
    pub claimed: Vec<OutPoint>,
    /// O4: the recorded terminal step-1 conflict this dead end is for.
    /// `None`: #625's dead end of a step 2 no read ever saw.
    pub conflict: Option<Step1Conflict>,
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
        // #568 B4b-3c: restart by kind. A fork-only record has no step 1 to
        // restore and the two-step reconciler refuses it (U2), so it never
        // reaches either: what it has recorded decides how it reopens.
        let kind = controller
            .recorded_split()
            .map_err(|error| {
                Step2Refusal::retry(step1::describe(claim_coordinator::Error::Journal(error)))
            })?
            .map(|record| record.kind);
        if kind == Some(claim_workflow::SplitKind::Unified) {
            return Ok(Restart::Unified(super::unified::UnifiedRecord::of(
                &controller,
            )));
        }
        // A dead end allows no resend: a recorded terminal step-1 conflict
        // refuses every one (#568 S4b), whatever the journal's permission.
        let dead_end = dead_end(&controller);
        (
            controller.recorded_split_step2().is_some(),
            dead_end.is_none() && resend_allowed(&controller),
            dead_end,
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

/// The journal's step-2 dead end, if it is in one: a recorded terminal
/// step-1 conflict (O4, #568 S4b) first, else #625's. A provisional
/// conflict is none (S4-D5).
fn dead_end(controller: &Controller) -> Option<DeadEnd> {
    let conflict = controller
        .split_step1_conflict()
        .filter(Step1Conflict::is_terminal);
    if conflict.is_none() && !controller.split_step2_dead_end() {
        return None;
    }
    let plan = controller.plan();
    Some(DeadEnd {
        step1: plan.step1_txid(),
        step2: controller.recorded_split_step2()?.compute_txid(),
        claimed: plan.claimed_prevouts,
        conflict,
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
/// O4 (#568 S4b): Bitcoin shows step 1 again, so the conflict may not hold.
pub const CONFLICT_STEP1_SEEN: &str = "Bitcoin shows step 1 again, so this split was not closed: the conflict recorded for it may not hold. Check status again; a check that finds step 1 six deep in its block clears the conflict.";

/// O4: the copy beside the close of a split with a terminal step-1
/// conflict. It names the coin and says the split can't complete, and that
/// step 2's bytes on BTCB2 stand.
pub fn conflict_close_copy(conflict: &Step1Conflict) -> String {
    format!(
        "A coin this split claims ({}) was spent on Bitcoin by another transaction, so step 1 can never confirm and this split can't complete. Step 2's recorded bytes on Bitcoin Blake2b stand as they are: nothing is sent, resent or undone. You can close this split after a fresh check of Bitcoin.",
        conflict.outpoint()
    )
}
/// O4: shown once that check passed, beside the close.
pub fn conflict_closable_copy(conflict: &Step1Conflict) -> String {
    format!(
        "Bitcoin still shows step 1 in no block and not waiting to be mined, and the coin ({}) spent. Closing keeps this split's record and its signed step 2 on this device; a new split of this wallet stays refused until that record is reset.",
        conflict.outpoint()
    )
}

/// O4, #658 P3-4 (S4b-D1): the close refused because the conflict's coin is
/// unspent on Bitcoin again, so step 1 can confirm after all. It names the
/// way out: step 1's saved signed transaction broadcast again through any
/// Bitcoin node or service, or waiting. The wallet never sends step 1 again
/// (D13 = A).
pub fn conflict_coin_unspent_copy(outpoint: &OutPoint) -> String {
    format!(
        "The coin this split's conflict names ({outpoint}) is unspent on Bitcoin again, so this split was not closed: step 1 can still confirm. If you saved step 1's signed transaction, you can broadcast it again through any Bitcoin node or service, or wait for it to confirm. This wallet never sends step 1 again. Check status again."
    )
}

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
fn conflict_unavailable(kind: FailureKind) -> Refusal {
    Refusal::retry(format!(
        "Connect couldn't check Bitcoin for this split ({kind:?}), so it was not closed. This is not a sign that the conflict changed. Try again later."
    ))
}
/// The Bitcoin address `outpoint` pays, from its Bitcoin previous
/// transaction checked against its txid.
async fn prevout_address(
    evidence: &dyn SplitEvidenceSource,
    outpoint: &OutPoint,
    unavailable: fn(FailureKind) -> Refusal,
) -> Result<Address, Refusal> {
    let previous = evidence
        .previous_transaction(ChainId::Bitcoin, outpoint.txid)
        .await
        .map_err(unavailable)?;
    (previous.compute_txid() == outpoint.txid)
        .then(|| usize::try_from(outpoint.vout).ok())
        .flatten()
        .and_then(|vout| previous.output.get(vout))
        .and_then(|output| Address::from_script(&output.script_pubkey, Network::Bitcoin).ok())
        .ok_or_else(|| Refusal::final_(step1::UNIDENTIFIED))
}
/// O4: one fresh Bitcoin read keyed by step 1's own txid: absent (in no
/// block and not waiting to be mined).
async fn step1_absent(evidence: &dyn SplitEvidenceSource, step1: Txid) -> Result<(), Refusal> {
    let seen = evidence
        .transaction(ChainId::Bitcoin, step1)
        .await
        .map_err(conflict_unavailable)?;
    if !fresh(evidence, seen.observed_at()) {
        return Err(conflict_unavailable(FailureKind::Stale));
    }
    if *seen.value() != TransactionObservation::Absent {
        return Err(Refusal::retry(CONFLICT_STEP1_SEEN));
    }
    Ok(())
}

/// O4 (#568 S4b): before a split with a recorded terminal step-1 conflict
/// may be closed, fresh Bitcoin reads must show, in this order: step 1
/// absent; the conflicting coin still missing from its address's Bitcoin
/// unspent outputs (the address from its previous transaction, checked
/// against its txid); step 1 absent again. Step 1's depth is not read, and
/// step 2's BTCB2 state does not matter. Step 1 seen again or the coin
/// unspent again refuses (a reconcile then decides: S4-D6 clears a conflict
/// step 1 disproves), as does a failed or stale read; nothing is recorded.
async fn check_conflict_close(
    evidence: &dyn SplitEvidenceSource,
    step1: Txid,
    conflict: &Step1Conflict,
) -> Result<(), Refusal> {
    step1_absent(evidence, step1).await?;
    let outpoint = conflict.outpoint();
    let address = prevout_address(evidence, &outpoint, conflict_unavailable).await?;
    let unspent = evidence
        .unspent_outputs(ChainId::Bitcoin, &address.to_string())
        .await
        .map_err(conflict_unavailable)?;
    if !fresh(evidence, unspent.observed_at()) {
        return Err(conflict_unavailable(FailureKind::Stale));
    }
    if unspent.value().contains(&outpoint) {
        return Err(Refusal::retry(conflict_coin_unspent_copy(&outpoint)));
    }
    step1_absent(evidence, step1).await
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
/// end has no resend for it to end. O4's dead end is checked on Bitcoin
/// only ([`check_conflict_close`]).
pub async fn check_close(connect: &dyn SplitConnect, dead_end: &DeadEnd) -> Result<(), Refusal> {
    let evidence = connect.evidence();
    if let Some(conflict) = &dead_end.conflict {
        return check_conflict_close(evidence, dead_end.step1, conflict).await;
    }
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
        let address = prevout_address(evidence, outpoint, unavailable).await?;
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
/// in exactly that dead end (for O4, with the same terminal conflict); then
/// its tombstone ([`step1::CLOSED`]) is written atomically next to the
/// journal, naming the source, the target Cube, both recorded txids and the
/// claimed inputs, and for O4 the conflicting coin. The journal itself,
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
    let mut tombstone = serde_json::json!({
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
    if let Some(conflict) = &dead_end.conflict {
        tombstone["step1_conflict"] = serde_json::json!(conflict.outpoint().to_string());
    }
    let bytes = serde_json::to_vec_pretty(&tombstone).map_err(|error| error.to_string())?;
    write_tombstone(directory, &bytes)
        .map_err(|error| format!("The split could not be abandoned ({error})."))
    // The controller, and the journal lock, end here.
}

/// #568 B4b-3c (C6, U4): close a fork-only split. Under the journal's lock
/// it must still be the fork-only record `record` was read from: no
/// submission for an unsubmitted one (no signed bytes were ever recorded,
/// so nothing needs a chain check), and for a submitted one exactly the
/// recorded sweep the caller's fresh check found absent with every coin
/// unspent on BTCB2 ([`super::unified::check_close`]). Then its tombstone
/// is written as for a two-step dead end; the journal itself is kept.
/// `ended` refuses under the lock, right before the write, as for
/// [`close`]. Blocking: off the UI thread, with every handle on the journal
/// dropped first.
pub(super) fn close_unified(
    directory: &Path,
    target_cube: &str,
    digest: sha256::Hash,
    context: Context,
    record: &super::unified::UnifiedRecord,
    closed_at: i64,
    ended: &std::sync::atomic::AtomicBool,
) -> Result<(), String> {
    let identity = claim_workflow::split_identity(target_cube.to_owned(), digest);
    let controller = Controller::reopen_settling_blocking(directory, &identity, context)
        .map_err(|error| step1::describe(claim_coordinator::Error::Journal(error)))?;
    if controller
        .recorded_split()
        .ok()
        .flatten()
        .map(|split| split.kind)
        != Some(claim_workflow::SplitKind::Unified)
        || super::unified::UnifiedRecord::of(&controller) != *record
    {
        return Err(CHANGED_SINCE_CHECK.to_string());
    }
    if ended.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(step1::ENDED_BEFORE_ABANDON.to_string());
    }
    let tombstone = serde_json::json!({
        "version": 1,
        "kind": "unified",
        "source_digest": digest.to_string(),
        "target_cube": target_cube,
        "sweep_txid": record.sweep.map(|txid| txid.to_string()),
        "claimed_prevouts": record
            .claimed
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        "closed_at": closed_at,
    });
    let bytes = serde_json::to_vec_pretty(&tombstone).map_err(|error| error.to_string())?;
    write_tombstone(directory, &bytes)
        .map_err(|error| format!("The split could not be closed ({error})."))
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
pub(super) fn fork_production(
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
    /// Where a completion is recorded (#568 B5b); `None` while the Cube's
    /// settings name no Vault.
    site: Option<CompletionSite>,
}
impl ProductionRecon {
    /// Refused without an account, for an unusable origin, or after the
    /// generation moved.
    pub fn new(
        session: ConnectSession,
        generation: watch::Receiver<u64>,
        site: Option<CompletionSite>,
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
            site,
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
        Ok(Box::new(ReconcilerDriver::new(
            reconciler,
            self.site.clone(),
            self.generation.clone(),
            self.expected,
        )))
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
    /// O1 (#568 S4b): the last step-1 reconfirmation review, used up by its
    /// acknowledgement.
    reconfirmation: Option<Step1ReconfirmationReview>,
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
            reconfirmation: None,
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
        self.reconfirmation = None;
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
            not_after,
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
    async fn review_reconfirmation(
        &mut self,
        context: &Context,
    ) -> Result<ReconfirmationView, Step2Refusal> {
        // A new review moves the coordinator's revision: any earlier review
        // of either kind could only refuse.
        self.review = None;
        self.resend = None;
        self.reconfirmation = None;
        let review = self
            .coordinator
            .prepare_step1_reconfirmation(context)
            .await
            .map_err(describe_reconfirmation)?;
        let revoker = self.coordinator.revoker();
        let view = ReconfirmationView::of(
            &review,
            resend_liveness(
                move || revoker.is_revoked(),
                self.generation.clone(),
                self.expected,
                review.not_after(),
            ),
        );
        self.reconfirmation = Some(review);
        Ok(view)
    }
    async fn confirm_reconfirmation(&mut self, context: &Context) -> Result<(), Step2Refusal> {
        self.review = None;
        let review = self
            .reconfirmation
            .take()
            .ok_or_else(|| Step2Refusal::retry(RECONFIRMATION_AGAIN))?;
        self.coordinator
            .confirm_step1_reconfirmation(review, context)
            .await
            .map_err(describe_reconfirmation)
    }
}

/// What the reconciler driver completes with (#568 B5b): the production one
/// is the [`SplitStep2Reconciler`] and its [`SplitCompletionEvidence`];
/// tests substitute a fake to pin the driver's own order (check, record,
/// then forget, D18) and its copy.
#[async_trait]
trait CompletionCore: Send + 'static {
    type Evidence: Send + 'static;
    async fn check(
        &mut self,
        context: &Context,
    ) -> Result<Option<Self::Evidence>, claim_coordinator::Error>;
    fn record(evidence: &Self::Evidence) -> SplitFromRecord;
    fn live(evidence: &Self::Evidence) -> bool;
    async fn persist(
        &self,
        evidence: &Self::Evidence,
        site: &CompletionSite,
    ) -> Result<(), SettingsError>;
    /// Blocking: the journal write.
    fn forget(
        &mut self,
        evidence: Self::Evidence,
        context: &Context,
    ) -> Result<(), claim_coordinator::Error>;
}
#[async_trait]
impl CompletionCore for SplitStep2Reconciler {
    type Evidence = SplitCompletionEvidence;
    async fn check(
        &mut self,
        context: &Context,
    ) -> Result<Option<SplitCompletionEvidence>, claim_coordinator::Error> {
        self.check_completion(context).await
    }
    fn record(evidence: &SplitCompletionEvidence) -> SplitFromRecord {
        evidence.record()
    }
    fn live(evidence: &SplitCompletionEvidence) -> bool {
        evidence.is_live()
    }
    async fn persist(
        &self,
        evidence: &SplitCompletionEvidence,
        site: &CompletionSite,
    ) -> Result<(), SettingsError> {
        evidence.persist(&site.root, &site.target).await
    }
    fn forget(
        &mut self,
        evidence: SplitCompletionEvidence,
        context: &Context,
    ) -> Result<(), claim_coordinator::Error> {
        evidence.forget(self, context)
    }
}

/// The production reconciler and, for a completion, where it is recorded.
/// The reconciler is out of the driver only while its descriptor deletion
/// runs off the executor; a task that does not return it leaves the driver
/// refusing with [`COMPLETION_INTERRUPTED`] and a restart.
struct ReconcilerDriver<C = SplitStep2Reconciler> {
    core: Option<C>,
    site: Option<CompletionSite>,
    revoke: RevokeHandle,
    /// O1 (#568 S4b): the last step-1 reconfirmation review, used up by its
    /// acknowledgement, and the session generation its view's liveness
    /// follows.
    reconfirmation: Option<Step1ReconfirmationReview>,
    generation: watch::Receiver<u64>,
    expected: u64,
}
impl ReconcilerDriver {
    fn new(
        reconciler: SplitStep2Reconciler,
        site: Option<CompletionSite>,
        generation: watch::Receiver<u64>,
        expected: u64,
    ) -> Self {
        let revoker = reconciler.revoker();
        Self {
            core: Some(reconciler),
            site,
            revoke: Arc::new(move || revoker.revoke()),
            reconfirmation: None,
            generation,
            expected,
        }
    }
}
/// The driver lost its reconciler: read the journal again.
fn interrupted() -> Step2Refusal {
    Step2Refusal {
        recovery: Step2Recovery::Restart,
        ..Step2Refusal::retry(COMPLETION_INTERRUPTED)
    }
}
impl<C: CompletionCore> ReconcilerDriver<C> {
    /// Check, record on the target Cube, then forget the descriptors, in
    /// that order under one evidence (D18). A refused record or deletion is
    /// "check again" (#645 P3-1): expired evidence never ends the split.
    async fn complete_inner(&mut self, context: &Context) -> Result<SplitCompletion, Step2Refusal> {
        let site = self
            .site
            .clone()
            .ok_or_else(|| Step2Refusal::final_(COMPLETION_NO_VAULT))?;
        let core = self.core.as_mut().ok_or_else(interrupted)?;
        let evidence = core
            .check(context)
            .await
            .map_err(describe_completion)?
            .ok_or_else(|| Step2Refusal::retry(COMPLETION_NOT_YET))?;
        let record = C::record(&evidence);
        if core.persist(&evidence, &site).await.is_err() {
            return Err(Step2Refusal::retry(if C::live(&evidence) {
                COMPLETION_NOT_RECORDED
            } else {
                COMPLETION_EXPIRED
            }));
        }
        let mut core = self.core.take().ok_or_else(interrupted)?;
        let context = context.clone();
        let (core, forgotten) = tokio::task::spawn_blocking(move || {
            let forgotten = core.forget(evidence, &context);
            (core, forgotten)
        })
        .await
        .map_err(|_| interrupted())?;
        self.core = Some(core);
        forgotten.map_err(|error| match error {
            claim_coordinator::Error::ExpiredEvidence => {
                Step2Refusal::retry(COMPLETION_NOT_FORGOTTEN)
            }
            error => describe_completion(error),
        })?;
        Ok(record.into())
    }
}
#[async_trait]
impl Step2Recon for ReconcilerDriver {
    fn revoke_handle(&self) -> RevokeHandle {
        self.revoke.clone()
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        self.core.as_ref()?.recorded_outcome()
    }
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<(Status, TransactionObservation, Step1AfterStep2), Step2Refusal> {
        self.reconfirmation = None;
        self.core
            .as_mut()
            .ok_or_else(interrupted)?
            .reconcile_sweep(context)
            .await
            .map(reconciled)
            .map_err(describe_check)
    }
    async fn complete(&mut self, context: &Context) -> Result<SplitCompletion, Step2Refusal> {
        self.complete_inner(context).await
    }
    async fn completion_stands(
        &mut self,
        context: &Context,
    ) -> Result<CompletionStanding, Step2Refusal> {
        let site = self
            .site
            .clone()
            .ok_or_else(|| Step2Refusal::final_(COMPLETION_NO_VAULT))?;
        let standing = self
            .core
            .as_mut()
            .ok_or_else(interrupted)?
            .reconcile_split_completion(context, &site.root, &site.target)
            .await
            .map_err(describe_completion)?;
        Ok(match standing {
            SplitCompletionReconciliation::Standing {
                status,
                transaction,
            } => CompletionStanding::Standing {
                status,
                seen: transaction,
            },
            SplitCompletionReconciliation::Lost {
                status,
                transaction,
                cleared,
            } => CompletionStanding::Lost {
                status,
                seen: transaction,
                cleared,
            },
        })
    }
    async fn review_reconfirmation(
        &mut self,
        context: &Context,
    ) -> Result<ReconfirmationView, Step2Refusal> {
        self.reconfirmation = None;
        let core = self.core.as_mut().ok_or_else(interrupted)?;
        let review = core
            .prepare_step1_reconfirmation(context)
            .await
            .map_err(describe_reconfirmation)?;
        let revoker = core.revoker();
        let view = ReconfirmationView::of(
            &review,
            resend_liveness(
                move || revoker.is_revoked(),
                self.generation.clone(),
                self.expected,
                review.not_after(),
            ),
        );
        self.reconfirmation = Some(review);
        Ok(view)
    }
    async fn confirm_reconfirmation(&mut self, context: &Context) -> Result<(), Step2Refusal> {
        let review = self
            .reconfirmation
            .take()
            .ok_or_else(|| Step2Refusal::retry(RECONFIRMATION_AGAIN))?;
        self.core
            .as_mut()
            .ok_or_else(interrupted)?
            .confirm_step1_reconfirmation(review, context)
            .await
            .map_err(describe_reconfirmation)
    }
}

/// What the panel keeps of a reconcile: the journal's status, step 2 on
/// BTCB2, and what became of step 1 after the step-2 submission.
fn reconciled(r: SweepReconcile) -> (Status, TransactionObservation, Step1AfterStep2) {
    (r.status, r.step2, r.after_step2)
}

#[cfg(all(test, unix))]
mod tests;
