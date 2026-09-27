//! Durable public transaction bytes and attempt history, never retry authority.
use super::*;

const MAX_BITCOIN_ATTEMPTS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BitcoinSubmissionAttempt {
    /// Legacy journals recorded only the txid. Recovering a valid witness later
    /// cannot establish which witness the original network attempt used.
    pub(super) wtxid: Option<Wtxid>,
}
impl BitcoinSubmissionAttempt {
    pub fn wtxid(&self) -> Option<Wtxid> {
        self.wtxid
    }
}

pub(super) fn validate_record(intent: &Intent) -> Result<(), Error> {
    if intent.version != 6 {
        return if intent.bitcoin_transaction.is_none() && intent.bitcoin_attempts.is_empty() {
            Ok(())
        } else {
            Err(Error::InvalidJournal)
        };
    }
    let tx = intent
        .bitcoin_transaction
        .as_ref()
        .ok_or(Error::InvalidJournal)?;
    let witness_id = tx.compute_wtxid();
    let mut unsigned = tx.clone();
    for input in &mut unsigned.input {
        input.witness.clear();
    }
    if intent.phase == Phase::Intent
        || intent.signed_txid != Some(tx.compute_txid())
        || digest(&unsigned) != intent.unsigned_digest
        || tx.input.iter().any(|input| input.witness.is_empty())
        || intent.bitcoin_attempts.is_empty()
        || intent.bitcoin_attempts.len() > MAX_BITCOIN_ATTEMPTS
        || intent
            .bitcoin_attempts
            .iter()
            .enumerate()
            .any(|(index, attempt)| match attempt.wtxid {
                Some(id) => id != witness_id,
                None => index != 0,
            })
    {
        return Err(Error::InvalidJournal);
    }
    Ok(())
}

impl Controller {
    /// Untrusted stored bytes. Callers must reconstruct the owned transaction and
    /// verify its signatures before restoring a coordinator. Never submission authority.
    pub fn recorded_bitcoin_transaction(&self) -> Option<&Transaction> {
        self.intent.bitcoin_transaction.as_ref()
    }
    pub fn bitcoin_submission_attempts(&self) -> &[BitcoinSubmissionAttempt] {
        &self.intent.bitcoin_attempts
    }
    /// Called after the coordinator validates the opaque signed artifact against
    /// the owned construction. The original recorded witness is immutable.
    pub(crate) fn bind_recovered_bitcoin_transaction(
        &mut self,
        current: &Context,
        verified: &coincube_core::claim_finalize::VerifiedPoisonTransfer,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        if !self.construction_verified {
            return Err(Error::Unchecked);
        }
        if self.intent.phase == Phase::Intent {
            return Ok(());
        }
        let tx = verified.transaction();
        if let Some(recorded) = &self.intent.bitcoin_transaction {
            return if recorded == tx {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        let mut next = self.intent.clone();
        next.version = 6;
        next.bitcoin_transaction = Some(tx.clone());
        next.bitcoin_attempts
            .push(BitcoinSubmissionAttempt { wtxid: None });
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        self.clear_check();
        Ok(())
    }
}

impl Controller {
    pub(crate) fn check_resubmission(
        &self,
        collected: CollectedAssessment,
        signed: &Transaction,
        policy: Policy,
        now: i64,
    ) -> Result<(), Error> {
        use crate::services::claim_observation::TransactionObservation;
        if !self.construction_verified
            || collected.generation != self.context.generation
            || self.intent.phase == Phase::Intent
            || self.intent.bitcoin_transaction.as_ref() != Some(signed)
            || self.intent.bitcoin_attempts.len() >= MAX_BITCOIN_ATTEMPTS
            || collected.observations.bitcoin_transaction != TransactionObservation::Absent
        {
            return Err(Error::Unchecked);
        }
        let o = collected.observations;
        if o.bitcoin.location != TransactionLocation::Unconfirmed
            || o.preflight.bitcoin != o.bitcoin.tip
            || o.preflight.fork != o.fork.tip
        {
            return Err(Error::Unchecked);
        }
        let mut plan = self.intent.plan.clone();
        // Ignore the old inclusion only for this fresh absence assessment. The
        // persisted inclusion and any fork submission remain unchanged.
        plan.previous_confirmation = None;
        if claim::assess(
            &plan,
            o.bitcoin,
            o.fork,
            o.deployment,
            policy,
            now,
            Some(o.preflight),
        ) != Assessment::WaitingForConfirmation
        {
            return Err(Error::Unchecked);
        }
        Ok(())
    }

    pub(crate) fn record_resubmission(
        &mut self,
        ticket: Ticket,
        current: &Context,
        collected: CollectedAssessment,
        signed: &Transaction,
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
        self.check_resubmission(collected, signed, policy, now)?;
        let mut next = self.intent.clone();
        next.bitcoin_attempts.push(BitcoinSubmissionAttempt {
            wtxid: Some(signed.compute_wtxid()),
        });
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }
}
