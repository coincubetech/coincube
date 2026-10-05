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
//! - O4 `Conflict`: missing, and fresh reads found a claimed coin missing
//!   from its address's Bitcoin unspent outputs. Read in this order, each
//!   fresh within [`MAX_EVIDENCE_AGE_SECONDS`]: step 1 absent; each claimed
//!   prevout's address from its txid-checked previous transaction, then
//!   that address's Bitcoin unspent outputs; step 1 absent again (its own
//!   spend in the mempool is not a conflict). A failed or stale read never
//!   records, changes or clears one. Connect's unspent outputs also leave
//!   out coins a mempool transaction spends, and no read names the spender
//!   (S4-D1), so the conflict is recorded **provisional** at the current
//!   Bitcoin tip (S4-D5): it is cleared when a fresh read shows step 1 on
//!   Bitcoin again (mempool or block) or the coin unspent again, and becomes
//!   **terminal** only when a later reconcile finds the same coin still
//!   missing and step 1 still absent at a tip at least six blocks higher.
//!   Then step 1 can never confirm and the split can't complete (S4-D2),
//!   unless a later collection finds step 1 eligible after all (six deep in
//!   its recorded block), which disproves the conflict and clears it
//!   (S4-D6).
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
    /// O4: a claimed coin missing from Bitcoin's unspent outputs,
    /// provisional or terminal ([`Step1Conflict::is_terminal`], S4-D5).
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

/// One read of step 1 on Bitcoin, for the conflict reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step1Read {
    /// A fresh read answered it absent.
    Absent,
    /// A fresh read answered it in the mempool or a block.
    Seen,
    /// The read failed or was stale: nothing follows from it.
    Inconclusive,
}
async fn read_step1(source: &dyn ObservationSource, txid: Txid) -> Step1Read {
    let Ok(read) = source.transaction(ChainId::Bitcoin, txid).await else {
        return Step1Read::Inconclusive;
    };
    if !fresh(source, read.observed_at()) {
        return Step1Read::Inconclusive;
    }
    if *read.value() == claim_observation::TransactionObservation::Absent {
        Step1Read::Absent
    } else {
        Step1Read::Seen
    }
}
fn fresh(source: &dyn ObservationSource, observed_at: i64) -> bool {
    observed_at >= 0
        && source
            .now()
            .checked_sub(observed_at)
            .is_some_and(|age| (0..=MAX_EVIDENCE_AGE_SECONDS).contains(&age))
}

/// What O4's fresh reads show; see the module documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Probe {
    /// A read failed, was stale or inconsistent: nothing follows.
    Inconclusive,
    /// Step 1 is on Bitcoin (mempool or block) after all.
    Step1Seen,
    /// Step 1 absent at both reads, every coin read: the claimed coins
    /// missing from their addresses' unspent outputs (none: all unspent).
    Missing(Vec<OutPoint>),
}
async fn probe(services: &dyn SplitForkServices, plan: &ClaimPlan) -> Probe {
    if plan.bitcoin_chain != ChainId::Bitcoin {
        return Probe::Inconclusive;
    }
    let source = services.source();
    let step1 = plan.step1_txid();
    match read_step1(source, step1).await {
        Step1Read::Absent => {}
        Step1Read::Seen => return Probe::Step1Seen,
        Step1Read::Inconclusive => return Probe::Inconclusive,
    }
    let mut missing = Vec::new();
    for outpoint in &plan.claimed_prevouts {
        let Some(address) = prevout_address(services, *outpoint).await else {
            return Probe::Inconclusive;
        };
        let Ok(unspent) = services.bitcoin_unspent(&address).await else {
            return Probe::Inconclusive;
        };
        if !fresh(source, unspent.observed_at()) {
            return Probe::Inconclusive;
        }
        if !unspent.value().contains(outpoint) {
            missing.push(*outpoint);
        }
    }
    if !missing.is_empty() {
        // Step 1 spends the claimed coins too: seen again (re-broadcast, or
        // re-mined) since the first read, a missing coin may be its own.
        match read_step1(source, step1).await {
            Step1Read::Absent => {}
            Step1Read::Seen => return Probe::Step1Seen,
            Step1Read::Inconclusive => return Probe::Inconclusive,
        }
    }
    Probe::Missing(missing)
}
/// The Bitcoin address `outpoint` pays, from its txid-checked previous
/// transaction.
async fn prevout_address(services: &dyn SplitForkServices, outpoint: OutPoint) -> Option<String> {
    let previous = services.previous_transaction(outpoint.txid).await.ok()?;
    if previous.compute_txid() != outpoint.txid {
        return None;
    }
    let output = previous.output.get(usize::try_from(outpoint.vout).ok()?)?;
    Address::from_script(&output.script_pubkey, Network::Bitcoin)
        .ok()
        .map(|address| address.to_string())
}

fn session_current(context: &Context, generation: &watch::Receiver<u64>) -> bool {
    *generation.borrow() == context.generation && generation.has_changed().is_ok()
}
/// Before any conflict write: the session that read is still current.
fn still_current(
    controller: &mut Controller,
    context: &Context,
    generation: &watch::Receiver<u64>,
) -> Result<(), Error> {
    if session_current(context, generation) {
        return Ok(());
    }
    controller.invalidate();
    Err(Error::Revoked)
}

/// What became of step 1, from a reconcile's applied collection, with the
/// conflict record kept current (S4-D5). A terminal conflict stands until a
/// collection finds step 1 eligible (six deep in its recorded block), which
/// disproves it and clears it (S4-D6). A provisional one is cleared when
/// the collection or the conflict reads show step 1 on Bitcoin, or show its
/// coin unspent again; it becomes terminal when the coin is still missing
/// and step 1 still absent six blocks above where it was first seen; a
/// failed, stale or inconclusive read leaves it as it is. A newly missing
/// coin records a provisional conflict at the collection's Bitcoin tip.
pub(super) async fn after_step2(
    controller: &mut Controller,
    services: &dyn SplitForkServices,
    context: &Context,
    generation: &watch::Receiver<u64>,
    observations: ObservationBundle,
) -> Result<Step1AfterStep2, Error> {
    let recorded = controller.split_step1_conflict();
    let plan = controller.plan();
    let after = classify(&plan, observations);
    if let Some(terminal) = recorded.filter(Step1Conflict::is_terminal) {
        // S4-D6: step 1 six deep on Bitcoin disproves "step 1 can never
        // confirm" (the terminal conflict may rest on a mempool spend that
        // stayed unconfirmed for six blocks). Every other outcome keeps it.
        if after != Step1AfterStep2::Eligible {
            return Ok(Step1AfterStep2::Conflict(terminal));
        }
        still_current(controller, context, generation)?;
        controller.disprove_split_step1_conflict(context)?;
        return Ok(after);
    }
    match after {
        Step1AfterStep2::Missing => {}
        Step1AfterStep2::Unknown => {
            return Ok(recorded.map_or(after, Step1AfterStep2::Conflict));
        }
        // Step 1 is on Bitcoin: a provisional conflict was not one.
        _ => {
            if recorded.is_some() {
                still_current(controller, context, generation)?;
                controller.clear_split_step1_conflict(context)?;
            }
            return Ok(after);
        }
    }
    let tip = observations.bitcoin.tip;
    let missing = match probe(services, &plan).await {
        Probe::Inconclusive => {
            return Ok(recorded.map_or(Step1AfterStep2::Missing, Step1AfterStep2::Conflict));
        }
        Probe::Step1Seen => {
            if recorded.is_some() {
                still_current(controller, context, generation)?;
                controller.clear_split_step1_conflict(context)?;
            }
            return Ok(Step1AfterStep2::Missing);
        }
        Probe::Missing(missing) => missing,
    };
    if let Some(provisional) = recorded {
        if missing.contains(&provisional.outpoint()) {
            let Some(terminal) = provisional.terminal(tip) else {
                return Ok(Step1AfterStep2::Conflict(provisional));
            };
            still_current(controller, context, generation)?;
            controller.confirm_split_step1_conflict(context, tip)?;
            return Ok(Step1AfterStep2::Conflict(terminal));
        }
        // Its coin is unspent again.
        still_current(controller, context, generation)?;
        controller.clear_split_step1_conflict(context)?;
    }
    let Some(first) = missing.first() else {
        return Ok(Step1AfterStep2::Missing);
    };
    let conflict = Step1Conflict::new(*first, tip);
    still_current(controller, context, generation)?;
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
        two_step_only(&self.controller)?;
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
        two_step_only(&self.controller)?;
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
        two_step_only(&self.controller)?;
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
        two_step_only(&self.controller)?;
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
