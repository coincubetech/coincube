//! One bounded collection joins fresh ancestry with transaction/deployment views.
use super::*;
use coincube_core::claim_ancestry::retained::RetainedPath;

/// Observation data only. Eligibility stays InputProofUnsupported until the
/// complete ancestry integration and proof receive independent acceptance.
#[derive(Debug)]
pub struct CollectedAncestry {
    ancestry: DiscoveredAncestry,
    assessment: CollectedAssessment,
    observed_at: i64,
}
impl CollectedAncestry {
    pub fn ancestry(&self) -> &DiscoveredAncestry {
        &self.ancestry
    }
    pub fn assessment(&self) -> CollectedAssessment {
        self.assessment
    }
    pub fn observed_at(&self) -> i64 {
        self.observed_at
    }
}
impl HttpObservationSource {
    /// Requalify the retained path, collect the ordinary transaction/deployment
    /// views, then require the same tips and anchor state across both. One outer
    /// deadline and shared request/response budget cover the entire operation.
    /// No retry, signing, cached eligibility, or submission permission is added.
    pub async fn collect_ancestry(
        &self,
        path: &RetainedPath,
        plan: &ClaimPlan,
        policy: Policy,
        budget: Duration,
    ) -> Result<CollectedAncestry, Failure> {
        super::super::super::validate_plan_shape(plan, policy, budget)?;
        binding::validate_partition(path, plan).map_err(|kind| failure(Stage::Plan, kind))?;
        path.reverify()
            .map_err(|_| failure(Stage::Plan, FailureKind::Malformed))?;
        let snapshot = self.discovery_snapshot();
        tokio::time::timeout(budget, async {
            let ancestry = snapshot
                .requalify_ancestry(path, policy)
                .await
                .map_err(|error| {
                    failure(
                        Stage::BitcoinInclusion,
                        match error {
                            DiscoveryError::Structural(_) => FailureKind::Malformed,
                            DiscoveryError::Observation(kind) => kind,
                        },
                    )
                })?;
            let (mut assessment, anchor) = super::super::super::collect_inner_with_anchor(
                &snapshot,
                plan,
                policy,
                snapshot.expected,
            )
            .await?;
            let o = assessment.observations;
            ancestry
                .validate_for_plan(
                    path,
                    plan,
                    AncestryContext {
                        provider: &snapshot.provider_identity(),
                        generation: snapshot.expected,
                        policy,
                        now: snapshot.now(),
                        tips: o.preflight,
                    },
                )
                .map_err(|kind| failure(Stage::Preflight, kind))?;
            let prior = ancestry.pair().anchor();
            if prior.tip_hash != anchor.tip_hash
                || prior.tip_height != anchor.tip_height
                || prior.tip_median_time_past != anchor.tip_median_time_past
                || prior.observation != anchor.observation
            {
                return Err(failure(Stage::Preflight, FailureKind::Changed));
            }
            let observed_at = ancestry
                .observed_at()
                .min(o.bitcoin.observed_at)
                .min(o.fork.observed_at)
                .min(o.deployment.observed_at);
            if !super::super::super::fresh(
                observed_at,
                snapshot.now(),
                policy.max_observation_age_seconds,
            ) {
                return Err(failure(Stage::Preflight, FailureKind::Stale));
            }
            // Preserve the oldest timestamp through downstream review deadlines.
            assessment.observations.bitcoin.observed_at = observed_at;
            assessment.observations.fork.observed_at = observed_at;
            assessment.observations.deployment.observed_at = observed_at;
            Ok(CollectedAncestry {
                ancestry,
                assessment,
                observed_at,
            })
        })
        .await
        .map_err(|_| failure(Stage::Context, FailureKind::Deadline))?
    }
}
