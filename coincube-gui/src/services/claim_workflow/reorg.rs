//! Explicit acknowledgement of changed inclusion. This never enables submission.
use super::*;

const MAX_INCLUSION_CHANGES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reconfirmation {
    pub previous: BlockRef,
    pub confirmed: BlockRef,
}

pub(super) fn validate_history(intent: &Intent) -> Result<(), Error> {
    let history = &intent.inclusion_history;
    if (!matches!(intent.version, 5 | 6) && !history.is_empty())
        || history.len() > MAX_INCLUSION_CHANGES
        || history
            .iter()
            .any(|entry| entry.previous == entry.confirmed)
        || history
            .windows(2)
            .any(|pair| pair[0].confirmed != pair[1].previous)
        || history.last().is_some_and(|entry| {
            Some(entry.confirmed) != intent.plan.previous_confirmation
                || intent.signed_txid.is_none()
        })
    {
        return Err(Error::InvalidJournal);
    }
    Ok(())
}

impl Controller {
    /// Recompute against the original transaction and both fresh chain views.
    /// The old inclusion is ignored only in this temporary assessment; persisted
    /// history and any submission record remain untouched until acknowledgement.
    pub(crate) fn check_reconfirmation(
        &self,
        collected: CollectedAssessment,
        policy: Policy,
        now: i64,
    ) -> Result<Reconfirmation, Error> {
        if collected.generation != self.context.generation
            || self.intent.signed_txid.is_none()
            || self.intent.phase != Phase::Tracking
            || self.intent.inclusion_history.len() >= MAX_INCLUSION_CHANGES
        {
            return Err(Error::Unchecked);
        }
        let previous = self
            .intent
            .plan
            .previous_confirmation
            .ok_or(Error::Unchecked)?;
        let o = collected.observations;
        let TransactionLocation::Confirmed { block, .. } = o.bitcoin.location else {
            return Err(Error::Unchecked);
        };
        if block == previous
            || o.preflight.bitcoin != o.bitcoin.tip
            || o.preflight.fork != o.fork.tip
        {
            return Err(Error::Unchecked);
        }
        let mut plan = self.intent.plan.clone();
        plan.previous_confirmation = None;
        if !matches!(
            claim::assess(
                &plan,
                o.bitcoin,
                o.fork,
                o.deployment,
                policy,
                now,
                Some(o.preflight)
            ),
            Assessment::WaitingForDepth { .. } | Assessment::ObservationsEligibleForPreflight
        ) {
            return Err(Error::Unchecked);
        }
        Ok(Reconfirmation {
            previous,
            confirmed: block,
        })
    }

    /// A coordinator calls this only after an explicit, one-use review and a
    /// second fresh collection. It consumes the controller's observation ticket.
    pub(crate) fn acknowledge_reconfirmation(
        &mut self,
        ticket: Ticket,
        current: &Context,
        collected: CollectedAssessment,
        policy: Policy,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        let matches = ticket.controller == self.id
            && self.pending
            && ticket.revision == self.revision
            && ticket.context == self.context
            && ticket.digest == self.intent.unsigned_digest;
        self.clear_check();
        if !matches {
            return Err(Error::LateObservation);
        }
        let inclusion = self.check_reconfirmation(collected, policy, now)?;
        let mut next = self.intent.clone();
        next.version = next.version.max(5);
        next.inclusion_history.push(inclusion);
        next.plan.previous_confirmation = Some(inclusion.confirmed);
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        // Acknowledgement is bookkeeping only. A later ordinary check must
        // establish depth and eligibility; no old review/signing authority lives.
        Ok(())
    }
}
