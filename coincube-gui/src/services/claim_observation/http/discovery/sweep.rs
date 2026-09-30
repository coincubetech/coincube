//! Requalify ancestry around the exact recorded sweep under one collection budget.
use super::*;
use crate::services::claim_observation::{read_sweep_transactions, SweepObservation};
use coincube_core::claim_ancestry::retained::RetainedPath;

/// Transient paired proof and inclusion observations, never completion authority.
#[derive(Debug)]
pub struct CollectedAncestrySweep {
    ancestry: CollectedAncestry,
    sweep: SweepObservation,
}
impl CollectedAncestrySweep {
    pub fn ancestry(&self) -> &CollectedAncestry {
        &self.ancestry
    }
    pub fn sweep(&self) -> SweepObservation {
        self.sweep
    }
    pub fn into_parts(self) -> (CollectedAncestry, SweepObservation) {
        (self.ancestry, self.sweep)
    }
}
impl HttpObservationSource {
    pub async fn collect_ancestry_sweep(
        &self,
        path: &RetainedPath,
        plan: &ClaimPlan,
        sweep: Txid,
        policy: Policy,
        budget: Duration,
    ) -> Result<CollectedAncestrySweep, Failure> {
        super::super::super::validate_plan_shape(plan, policy, budget)?;
        binding::validate_partition(path, plan).map_err(|kind| failure(Stage::Plan, kind))?;
        if sweep == plan.step1_txid() {
            return Err(failure(Stage::Plan, FailureKind::InvalidPlan));
        }
        let snapshot = self.discovery_snapshot();
        tokio::time::timeout(budget, async {
            let first = snapshot
                .collect_ancestry(path, plan, policy, budget)
                .await?;
            let (transaction, stamps) = read_sweep_transactions(
                &snapshot,
                plan.fork_chain,
                sweep,
                first.assessment().observations.fork.tip.height,
                policy,
            )
            .await?;
            let last = snapshot
                .collect_ancestry(path, plan, policy, budget)
                .await?;
            let a = first.assessment().observations;
            let b = last.assessment().observations;
            let first_anchor = first.ancestry().pair().anchor();
            let last_anchor = last.ancestry().pair().anchor();
            if a.bitcoin_transaction != b.bitcoin_transaction
                || a.bitcoin.tip != b.bitcoin.tip
                || a.bitcoin.location != b.bitcoin.location
                || a.fork.tip != b.fork.tip
                || a.fork.step1_presence != b.fork.step1_presence
                || a.fork.median_time_past != b.fork.median_time_past
                || a.deployment.state != b.deployment.state
                || first_anchor.observation != last_anchor.observation
                || (
                    first.ancestry().pair().bitcoin().block,
                    first.ancestry().pair().bitcoin().txid,
                ) != (
                    last.ancestry().pair().bitcoin().block,
                    last.ancestry().pair().bitcoin().txid,
                )
                || (
                    first.ancestry().pair().fork().block,
                    first.ancestry().pair().fork().txid,
                ) != (
                    last.ancestry().pair().fork().block,
                    last.ancestry().pair().fork().txid,
                )
            {
                return Err(failure(Stage::ForkTransaction, FailureKind::Changed));
            }
            let observed_at = stamps
                .into_iter()
                .chain([first.observed_at(), last.observed_at()])
                .min()
                .ok_or_else(|| failure(Stage::ForkTransaction, FailureKind::Unavailable))?;
            if !super::super::super::fresh(
                observed_at,
                snapshot.now(),
                policy.max_observation_age_seconds,
            ) {
                return Err(failure(Stage::ForkTransaction, FailureKind::Stale));
            }
            if *snapshot.generation.borrow() != snapshot.expected
                || snapshot.generation.has_changed().is_err()
            {
                return Err(failure(Stage::Context, FailureKind::Cancelled));
            }
            let mut assessment = last.assessment();
            assessment.observations.bitcoin.observed_at = observed_at;
            assessment.observations.fork.observed_at = observed_at;
            assessment.observations.deployment.observed_at = observed_at;
            Ok(CollectedAncestrySweep {
                ancestry: last,
                sweep: SweepObservation {
                    assessment,
                    transaction,
                    observed_at,
                },
            })
        })
        .await
        .map_err(|_| failure(Stage::Context, FailureKind::Deadline))?
    }
}
