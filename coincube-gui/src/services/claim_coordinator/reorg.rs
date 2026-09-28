use super::*;
use crate::services::claim_workflow::Reconfirmation;

/// One-use acknowledgement of a re-mined original transaction. This token is
/// deliberately distinct from a transaction submission review.
pub struct ReconfirmationReview {
    coordinator: u64,
    revision: u64,
    inclusion: Reconfirmation,
    observations: ObservationBundle,
    not_after: Instant,
}
impl ReconfirmationReview {
    #[cfg(test)]
    pub(super) fn expire_for_test(&mut self) {
        self.not_after = Instant::now();
    }
    pub fn inclusion(&self) -> Reconfirmation {
        self.inclusion
    }
}

impl Coordinator {
    pub async fn prepare_reconfirmation(
        &mut self,
        context: &Context,
    ) -> Result<ReconfirmationReview, Error> {
        self.current(context)?;
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        let collected = self.collect().await?;
        self.current(context)?;
        let now = self.services.source().now();
        let inclusion = self
            .controller
            .check_reconfirmation(collected.recovery(), self.policy.observations, now)
            .map_err(|error| recovery_check_error(error, collected.assessment))?;
        let not_after = evidence_deadline(
            self.policy,
            collected.observations,
            now,
            now,
            Instant::now(),
        )?;
        Ok(ReconfirmationReview {
            coordinator: self.id,
            revision: self.revision,
            inclusion,
            observations: collected.observations,
            not_after,
        })
    }

    /// Must correspond to explicit user acknowledgement of the displayed old
    /// and new blocks. Fresh reads must still match that review. No transaction
    /// is signed, submitted or made retryable by this operation.
    pub async fn confirm_reconfirmation(
        &mut self,
        review: ReconfirmationReview,
        context: &Context,
    ) -> Result<(), Error> {
        self.current(context)?;
        if review.coordinator != self.id || review.revision != self.revision {
            return Err(Error::InvalidReview);
        }
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        if Instant::now() >= review.not_after {
            return Err(Error::ExpiredEvidence);
        }
        let ticket = self.controller.begin_check(context)?;
        let collected = self.collect().await?;
        self.current(context)?;
        if Instant::now() >= review.not_after {
            return Err(Error::ExpiredEvidence);
        }
        if !same_view(review.observations, collected.observations)
            || self
                .controller
                .check_reconfirmation(
                    collected.recovery(),
                    self.policy.observations,
                    self.services.source().now(),
                )
                .map_err(|error| recovery_check_error(error, collected.assessment))?
                != review.inclusion
        {
            return Err(Error::ChangedReview);
        }
        self.controller.acknowledge_reconfirmation(
            ticket,
            context,
            collected.recovery(),
            self.policy.observations,
            self.services.source().now(),
        )?;
        Ok(())
    }
}
