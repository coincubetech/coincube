use super::*;
use crate::services::claim_observation::CollectedAssessment;

/// Explicit, one-use consent to resend exactly the recorded signed transaction.
/// Neither tracking nor restart creates this token.
pub struct ResubmissionReview {
    coordinator: u64,
    revision: u64,
    snapshot: ReviewSnapshot,
    previous_attempts: usize,
}
impl ResubmissionReview {
    #[cfg(test)]
    pub(super) fn expire_for_test(&mut self) {
        self.snapshot.not_after = Instant::now();
    }
    pub fn snapshot(&self) -> &ReviewSnapshot {
        &self.snapshot
    }
    pub fn previous_attempts(&self) -> usize {
        self.previous_attempts
    }
}
impl Coordinator {
    async fn resubmission_snapshot(
        &mut self,
        context: &Context,
    ) -> Result<(ReviewSnapshot, CollectedAssessment), Error> {
        self.current(context)?;
        let first = self.collect().await?;
        self.controller
            .check_resubmission(
                first,
                self.verified.transaction(),
                self.policy.observations,
                self.services.source().now(),
            )
            .map_err(|error| recovery_check_error(error, first.assessment))?;
        let evidence = self
            .services
            .routed_preflight(
                self.verified.transaction(),
                first.observations.bitcoin.tip.hash,
                self.policy.preflight,
            )
            .await
            .map_err(Error::Preflight)?;
        let last = self.collect().await?;
        self.current(context)?;
        if !same_view(first.observations, last.observations) {
            return Err(Error::ChangedReview);
        }
        self.controller
            .check_resubmission(
                last,
                self.verified.transaction(),
                self.policy.observations,
                self.services.source().now(),
            )
            .map_err(|error| recovery_check_error(error, last.assessment))?;
        self.fresh_evidence(&evidence, last.observations.bitcoin.tip.hash)?;
        let not_after = evidence_deadline(
            self.policy,
            last.observations,
            evidence.observed_at(),
            self.services.source().now(),
            Instant::now(),
        )?;
        Ok((
            ReviewSnapshot {
                transaction: self.verified.transaction().clone(),
                wallet: self.controller.identity().clone(),
                txid: self.verified.transaction().compute_txid(),
                wtxid: self.verified.transaction().compute_wtxid(),
                fee_sats: self.verified.fee().to_sat(),
                vsize: self.verified.vsize(),
                observations: last.observations,
                route: evidence.route(),
                not_after,
            },
            last,
        ))
    }
    pub async fn prepare_resubmission(
        &mut self,
        context: &Context,
    ) -> Result<ResubmissionReview, Error> {
        self.current(context)?;
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        let (snapshot, _) = self.resubmission_snapshot(context).await?;
        Ok(ResubmissionReview {
            coordinator: self.id,
            revision: self.revision,
            snapshot,
            previous_attempts: self.controller.bitcoin_submission_attempts().len(),
        })
    }
    pub async fn confirm_resubmission(
        &mut self,
        review: ResubmissionReview,
        context: &Context,
    ) -> Result<Outcome, Error> {
        self.current(context)?;
        if review.coordinator != self.id || review.revision != self.revision {
            return Err(Error::InvalidReview);
        }
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        if Instant::now() >= review.snapshot.not_after {
            return Err(Error::ExpiredEvidence);
        }
        let ticket = self.controller.begin_check(context)?;
        let (mut refreshed, collected) = self.resubmission_snapshot(context).await?;
        if review.snapshot.transaction != refreshed.transaction
            || review.snapshot.wallet != refreshed.wallet
            || review.previous_attempts != self.controller.bitcoin_submission_attempts().len()
            || review.snapshot.route != refreshed.route
            || !same_view(review.snapshot.observations, refreshed.observations)
        {
            return Err(Error::ChangedReview);
        }
        refreshed.not_after = refreshed.not_after.min(review.snapshot.not_after);
        self.current(context)?;
        if Instant::now() >= refreshed.not_after {
            return Err(Error::ExpiredEvidence);
        }
        self.controller.record_resubmission(
            ticket,
            context,
            collected,
            self.verified.transaction(),
            self.policy.observations,
            self.services.source().now(),
        )?;
        self.submit_recorded(context, refreshed).await
    }
}
