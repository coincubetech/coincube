//! Preserve live ancestry proof through coordinator review and journal checks.
use super::*;
use claim_observation::{http::CollectedAncestry, CollectedAssessment};

pub(super) struct Collected {
    pub data: CollectedAssessment,
    pub ancestry: Option<CollectedAncestry>,
}
impl std::ops::Deref for Collected {
    type Target = CollectedAssessment;
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}
impl Collected {
    pub fn ancestry(
        proof: CollectedAncestry,
        path: &coincube_core::claim_ancestry::retained::RetainedPath,
        plan: &coincube_core::claim::ClaimPlan,
        context: &Context,
        policy: Policy,
        now: i64,
    ) -> Result<Self, Error> {
        let data = proof
            .assess_verified_observations(
                path,
                plan,
                claim_observation::http::AncestryContext {
                    provider: &context.provider,
                    generation: context.generation,
                    policy,
                    now,
                    tips: proof.assessment().observations.preflight,
                },
            )
            .map_err(|_| Error::ChangedReview)?;
        Ok(Self {
            data,
            ancestry: Some(proof),
        })
    }
    pub fn ordinary(data: CollectedAssessment) -> Self {
        Self {
            data,
            ancestry: None,
        }
    }
    pub fn recovery(&self) -> claim_workflow::RecoveryObservation<'_> {
        match self.ancestry.as_ref() {
            Some(proof) => proof.into(),
            None => self.data.into(),
        }
    }
    pub fn apply(
        self,
        controller: &mut Controller,
        ticket: claim_workflow::Ticket,
        context: &Context,
        policy: Policy,
        now: i64,
    ) -> Result<Status, claim_workflow::Error> {
        match self.ancestry {
            Some(proof) => {
                controller.apply_ancestry_observation(ticket, context, Ok(proof), policy, now)
            }
            None => controller.apply_observation(ticket, context, Ok(self.data), policy, now),
        }
    }
}
