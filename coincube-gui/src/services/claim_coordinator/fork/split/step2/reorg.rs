//! Split (#568 S4): step 1's Bitcoin reorg after the step-2 submission, with
//! no step-1 resend (owner decision D13 = A).
//!
//! Once step 2 is submitted, step 1 can't be rebuilt (the claimed coins may
//! already be spent on BTCB2, which the coins' authentication refuses), and
//! the step-1 coordinator never reopens the journal again. Every reconcile
//! of the recorded step 2 (the coordinator's or the reconciler's) therefore
//! reports what became of step 1 on Bitcoin, [`Step1AfterStep2`], from the
//! same fresh collection:
//!
//! - `Eligible`: six deep in its recorded block, absent from BTCB2. Bitcoin
//!   replay protection for step 2 stands.
//! - `Shallow`: in its recorded block, below six confirmations.
//! - O1 `Remined`: confirmed in another block than the recorded one. Only an
//!   explicit, one-use acknowledgement on the reconciler or the coordinator
//!   ([`Step1ReconfirmationReview`]) records the new block; nothing is sent.
//! - O2 `InMempool`: out of every block, waiting in the Bitcoin mempool.
//! - O3 `Missing`: out of every block and absent from the mempool.
//! - O4 `Conflict`: missing, and fresh reads found a claimed coin spent on
//!   Bitcoin by another transaction, so step 1 can never confirm and the
//!   split can't complete. Read in this order, each fresh within
//!   [`MAX_EVIDENCE_AGE_SECONDS`]: step 1 absent; each claimed prevout's
//!   address from its txid-checked previous transaction, then that address's
//!   Bitcoin unspent outputs; step 1 absent again (its own spend in the
//!   mempool is not a conflict). A failed or stale read never reports one.
//!   The conflict is recorded in the journal and is terminal (S4-D2). The
//!   spender is not named (S4-D1).
//! - `Unknown`: the collection could not place step 1.
//!
//! While step 1 is not confirmed, step 2's recorded bytes could be mined on
//! Bitcoin: resend, completion and the descriptor deletion are withheld in
//! every outcome but `Eligible`.
//!
//! A fork-only (`kind: Unified`) record has no step 1: these entry points
//! refuse it before any read.
use super::*;
use crate::services::{
    claim_observation::SweepObservation,
    claim_workflow::{Reconfirmation, SplitKind, Step1Conflict},
};
use coincube_core::claim::{BlockRef, ClaimPlan, ForkTransactionPresence, TransactionLocation};

/// What became of step 1 on Bitcoin after the step-2 submission; see the
/// module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step1AfterStep2 {
    /// Six deep in its recorded block, absent from BTCB2.
    Eligible,
    /// In its recorded block with fewer than six confirmations.
    Shallow { confirmations: u64 },
    /// O1: confirmed in another block than the recorded one.
    Remined {
        previous: BlockRef,
        confirmed: BlockRef,
    },
    /// O2: in no block, in the Bitcoin mempool.
    InMempool,
    /// O3: in no block and not in the Bitcoin mempool.
    Missing,
    /// O4: a claimed coin spent on Bitcoin by another transaction; terminal.
    Conflict(Step1Conflict),
    /// The collection could not place step 1.
    Unknown,
}

/// One reconcile of the recorded step 2: the journal's status, step 2 on
/// BTCB2, step 1's own Bitcoin read (absence kept apart from the mempool)
/// and what that means after the step-2 submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepReconcile {
    pub status: Status,
    /// The recorded step 2 on BTCB2.
    pub step2: claim_observation::TransactionObservation,
    /// Step 1's own Bitcoin read.
    pub step1: claim_observation::TransactionObservation,
    pub after_step2: Step1AfterStep2,
}

/// Step 1's place on Bitcoin from one collection's observations, inclusion
/// read independently of deployment (an expired RDTS window must not mask
/// a reorg). The raw read tells the mempool from absence, which the
/// location collapses into `Unconfirmed`.
pub(super) fn classify(plan: &ClaimPlan, observations: ObservationBundle) -> Step1AfterStep2 {
    use claim_observation::TransactionObservation as Seen;
    let step1 = plan.step1_txid();
    if observations.bitcoin.chain != plan.bitcoin_chain {
        return Step1AfterStep2::Unknown;
    }
    match observations.bitcoin.location {
        TransactionLocation::Unknown => Step1AfterStep2::Unknown,
        TransactionLocation::Unconfirmed => match observations.bitcoin_transaction {
            Seen::Absent => Step1AfterStep2::Missing,
            Seen::Unconfirmed { txid } if txid == step1 => Step1AfterStep2::InMempool,
            _ => Step1AfterStep2::Unknown,
        },
        TransactionLocation::Confirmed {
            txid,
            block,
            best_chain_hash_at_height,
        } => {
            if txid != step1 || block.hash != best_chain_hash_at_height {
                return Step1AfterStep2::Unknown;
            }
            if let Some(previous) = plan.previous_confirmation.filter(|p| *p != block) {
                return Step1AfterStep2::Remined {
                    previous,
                    confirmed: block,
                };
            }
            let Some(confirmations) = observations
                .bitcoin
                .tip
                .height
                .checked_sub(block.height)
                .and_then(|depth| depth.checked_add(1))
            else {
                return Step1AfterStep2::Unknown;
            };
            if confirmations < coincube_core::claim::MIN_CONFIRMATIONS {
                return Step1AfterStep2::Shallow { confirmations };
            }
            if observations.fork.chain != plan.fork_chain
                || observations.fork.step1_txid != step1
                || observations.fork.step1_presence != ForkTransactionPresence::NotObserved
            {
                return Step1AfterStep2::Unknown;
            }
            Step1AfterStep2::Eligible
        }
    }
}

/// A fresh read of step 1 on Bitcoin that answers it absent.
async fn step1_absent(source: &dyn ObservationSource, txid: Txid) -> bool {
    let Ok(read) = source.transaction(ChainId::Bitcoin, txid).await else {
        return false;
    };
    fresh(source, read.observed_at())
        && *read.value() == claim_observation::TransactionObservation::Absent
}
fn fresh(source: &dyn ObservationSource, observed_at: i64) -> bool {
    observed_at >= 0
        && source
            .now()
            .checked_sub(observed_at)
            .is_some_and(|age| (0..=MAX_EVIDENCE_AGE_SECONDS).contains(&age))
}

/// O4's fresh reads; see the module documentation. `None` unless they
/// prove a conflict: any failed, stale or inconsistent read, step 1 seen,
/// or every claimed coin unspent.
async fn probe_conflict(
    services: &dyn SplitForkServices,
    plan: &ClaimPlan,
    bitcoin_tip: BlockRef,
) -> Option<Step1Conflict> {
    if plan.bitcoin_chain != ChainId::Bitcoin {
        return None;
    }
    let source = services.source();
    let step1 = plan.step1_txid();
    if !step1_absent(source, step1).await {
        return None;
    }
    let mut spent = None;
    for outpoint in &plan.claimed_prevouts {
        let previous = services.previous_transaction(outpoint.txid).await.ok()?;
        if previous.compute_txid() != outpoint.txid {
            return None;
        }
        let output = previous.output.get(usize::try_from(outpoint.vout).ok()?)?;
        let address = Address::from_script(&output.script_pubkey, Network::Bitcoin).ok()?;
        let unspent = services.bitcoin_unspent(&address.to_string()).await.ok()?;
        if !fresh(source, unspent.observed_at()) {
            return None;
        }
        if !unspent.value().contains(outpoint) {
            spent = Some(*outpoint);
            break;
        }
    }
    let outpoint = spent?;
    // Step 1 spends the claimed coins too: seen again (re-broadcast, or
    // re-mined) since the first read, the missing coin may be its own.
    if !step1_absent(source, step1).await {
        return None;
    }
    Some(Step1Conflict::new(outpoint, bitcoin_tip))
}

fn session_current(context: &Context, generation: &watch::Receiver<u64>) -> bool {
    *generation.borrow() == context.generation && generation.has_changed().is_ok()
}

/// What became of step 1, from a reconcile's applied collection. A recorded
/// conflict stands whatever the chains show now (S4-D2). A newly missing
/// step 1 gets O4's fresh reads, and a proven conflict is recorded before it
/// is reported.
pub(super) async fn after_step2(
    controller: &mut Controller,
    services: &dyn SplitForkServices,
    context: &Context,
    generation: &watch::Receiver<u64>,
    observations: ObservationBundle,
) -> Result<Step1AfterStep2, Error> {
    if let Some(conflict) = controller.split_step1_conflict() {
        return Ok(Step1AfterStep2::Conflict(conflict));
    }
    let plan = controller.plan();
    let after = classify(&plan, observations);
    if after != Step1AfterStep2::Missing {
        return Ok(after);
    }
    let Some(conflict) = probe_conflict(services, &plan, observations.bitcoin.tip).await else {
        return Ok(Step1AfterStep2::Missing);
    };
    if !session_current(context, generation) {
        controller.invalidate();
        return Err(Error::Revoked);
    }
    controller.record_split_step1_conflict(context, conflict)?;
    Ok(Step1AfterStep2::Conflict(conflict))
}

/// A two-step record only: a fork-only record has no step 1 (#650), so the
/// reorg entry points refuse it before any read.
fn two_step_only(controller: &Controller) -> Result<(), Error> {
    match controller.recorded_split()? {
        Some(record) if record.kind == SplitKind::Split => Ok(()),
        _ => Err(Error::InvalidBinding),
    }
}

/// O1: one-use review of step 1 re-mined in another Bitcoin block after the
/// step-2 submission. Bound to the reconciler or coordinator that made it,
/// its revision, the old and new block, the collection's observations and a
/// short deadline. No Clone; confirming consumes it. Never submission
/// authority.
pub struct Step1ReconfirmationReview {
    owner: u64,
    revision: u64,
    inclusion: Reconfirmation,
    observations: ObservationBundle,
    not_after: Instant,
}
impl std::fmt::Debug for Step1ReconfirmationReview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Step1ReconfirmationReview")
            .field("inclusion", &self.inclusion)
            .finish_non_exhaustive()
    }
}
impl Step1ReconfirmationReview {
    /// The recorded block and the one step 1 is now confirmed in.
    pub fn inclusion(&self) -> Reconfirmation {
        self.inclusion
    }
    #[cfg(test)]
    pub(crate) fn expire_for_test(&mut self) {
        self.not_after = Instant::now();
    }
}

/// One collection of the recorded step 2 for a step-1 reconfirmation, its
/// step-2 sighting recorded as a reconcile records it.
async fn collect_for_reconfirmation(
    controller: &mut Controller,
    services: &dyn SplitForkServices,
    policy: CheckPolicy,
    context: &Context,
    generation: &watch::Receiver<u64>,
    txid: Txid,
) -> Result<SweepObservation, Error> {
    let collected =
        collect_recorded(controller, services, policy, context, generation, txid).await?;
    controller.record_split_step2_observed(context, collected.transaction())?;
    Ok(collected)
}

pub(super) async fn prepare_reconfirmation(
    controller: &mut Controller,
    services: &dyn SplitForkServices,
    policy: CheckPolicy,
    context: &Context,
    generation: &watch::Receiver<u64>,
    owner: (u64, u64),
) -> Result<Step1ReconfirmationReview, Error> {
    two_step_only(controller)?;
    let txid = recorded_submission(controller)?;
    let collected =
        collect_for_reconfirmation(controller, services, policy, context, generation, txid).await?;
    let data = collected.assessment();
    let now = services.source().now();
    let inclusion = controller
        .check_reconfirmation(data, policy.observations, now)
        .map_err(|error| recovery_check_error(error, data.assessment))?;
    let not_after = evidence_deadline(
        policy,
        data.observations,
        collected.observed_at(),
        now,
        Instant::now(),
    )?;
    Ok(Step1ReconfirmationReview {
        owner: owner.0,
        revision: owner.1,
        inclusion,
        observations: data.observations,
        not_after,
    })
}

/// The caller has checked the review is its own and current and moved its
/// revision on, so the review is used up whatever the result.
pub(super) async fn confirm_reconfirmation(
    controller: &mut Controller,
    services: &dyn SplitForkServices,
    policy: CheckPolicy,
    context: &Context,
    generation: &watch::Receiver<u64>,
    review: Step1ReconfirmationReview,
) -> Result<(), Error> {
    if Instant::now() >= review.not_after {
        return Err(Error::ExpiredEvidence);
    }
    two_step_only(controller)?;
    let txid = recorded_submission(controller)?;
    let ticket = controller.begin_check(context)?;
    let collected =
        collect_for_reconfirmation(controller, services, policy, context, generation, txid).await?;
    if Instant::now() >= review.not_after {
        return Err(Error::ExpiredEvidence);
    }
    let data = collected.assessment();
    if !same_view(review.observations, data.observations)
        || controller
            .check_reconfirmation(data, policy.observations, services.source().now())
            .map_err(|error| recovery_check_error(error, data.assessment))?
            != review.inclusion
    {
        return Err(Error::ChangedReview);
    }
    controller.acknowledge_reconfirmation(
        ticket,
        context,
        data,
        policy.observations,
        services.source().now(),
    )?;
    Ok(())
}

impl SplitStep2Reconciler {
    /// O1: review step 1 re-mined in another Bitcoin block after the step-2
    /// submission. Collects the recorded step 2 and step 1 fresh (recording
    /// a step-2 sighting); refused unless step 1 is confirmed in a block
    /// other than the recorded one, on the best chain, with the step-1 gate
    /// otherwise passing (S4-D4: an RDTS window past its margin refuses).
    /// Records nothing about step 1 and sends nothing. Supersedes any
    /// earlier review and completion evidence of this reconciler.
    pub async fn prepare_step1_reconfirmation(
        &mut self,
        context: &Context,
    ) -> Result<Step1ReconfirmationReview, Error> {
        self.completion_revoker.revoke();
        self.current(context)?;
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        prepare_reconfirmation(
            &mut self.controller,
            self.services.as_ref(),
            self.policy,
            context,
            &self.generation,
            (self.id, self.revision),
        )
        .await
    }
    /// O1: the explicit acknowledgement of exactly that review, after
    /// another fresh collection that must show the same view and the same
    /// blocks: step 1's inclusion history gets one entry and the new block
    /// becomes its recorded one. Bookkeeping only; nothing is sent, and a
    /// later reconcile must still find step 1 six deep in it.
    pub async fn confirm_step1_reconfirmation(
        &mut self,
        review: Step1ReconfirmationReview,
        context: &Context,
    ) -> Result<(), Error> {
        self.completion_revoker.revoke();
        self.current(context)?;
        if review.owner != self.id || review.revision != self.revision {
            return Err(Error::InvalidReview);
        }
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        confirm_reconfirmation(
            &mut self.controller,
            self.services.as_ref(),
            self.policy,
            context,
            &self.generation,
            review,
        )
        .await
    }
}

impl SplitStep2Coordinator {
    /// O1 on the coordinator that submitted step 2; see
    /// [`SplitStep2Reconciler::prepare_step1_reconfirmation`]. Supersedes
    /// any earlier review of this coordinator.
    pub async fn prepare_step1_reconfirmation(
        &mut self,
        context: &Context,
    ) -> Result<Step1ReconfirmationReview, Error> {
        self.current(context)?;
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        prepare_reconfirmation(
            &mut self.controller,
            self.services.as_ref(),
            self.policy,
            context,
            &self.generation,
            (self.id, self.revision),
        )
        .await
    }
    /// See [`SplitStep2Reconciler::confirm_step1_reconfirmation`].
    pub async fn confirm_step1_reconfirmation(
        &mut self,
        review: Step1ReconfirmationReview,
        context: &Context,
    ) -> Result<(), Error> {
        self.current(context)?;
        if review.owner != self.id || review.revision != self.revision {
            return Err(Error::InvalidReview);
        }
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        confirm_reconfirmation(
            &mut self.controller,
            self.services.as_ref(),
            self.policy,
            context,
            &self.generation,
            review,
        )
        .await
    }
}
