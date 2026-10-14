//! Restart data for the two ordinary signed ancestry transactions. This is
//! deliberately part of the same atomically replaced intent as the unsigned
//! plan, so a crash cannot leave a signed pair detached from its wallet and
//! destination binding. Loading it never restores proof, signer authority or
//! permission to send either transaction again.

use super::{
    paired_recovery::BitcoinConfirmation, split, validate as validate_intent, Context, Controller,
    Error, Intent, Phase,
};
use crate::services::claim_observation::http::{CollectedPostBitcoinInputs, FundingContext};
use crate::services::claim_preflight::{Evidence, FreshnessPolicy, PairEvidence};
use coincube_core::{
    chain::ChainId,
    claim::{Policy, PreflightTips},
    claim_finalize::VerifiedAncestryClaimPair,
    foreign_split::VerifiedAncestrySplitPair,
    miniscript::bitcoin::{BlockHash, ScriptBuf, Transaction, Txid, Wtxid},
};
use serde::{Deserialize, Serialize};

const MAX_PROOF_AGE_SECONDS: i64 = 30;

/// A live, controller-bound result of checking both chain-specific and all
/// shared inputs against the exact signed pair. Never serialized or restored.
pub(crate) struct InitialAncestryProof {
    controller: u64,
    context: Context,
    bitcoin: (Txid, Wtxid),
    fork: (Txid, Wtxid),
    tips: PreflightTips,
    observed_at: i64,
    max_age_seconds: i64,
}

impl InitialAncestryProof {
    /// Only call after the Claim or Split finalizer validates the collected
    /// inputs, saved intent, wallet ownership and both finalized signatures.
    #[allow(dead_code)] // Called by the gated paired signer path.
    pub(super) fn after_finalization(
        controller: &Controller,
        current: &Context,
        bitcoin: &Transaction,
        fork: &Transaction,
        tips: PreflightTips,
        observed_at: i64,
        max_age_seconds: i64,
    ) -> Self {
        Self {
            controller: controller.id,
            context: current.clone(),
            bitcoin: (bitcoin.compute_txid(), bitcoin.compute_wtxid()),
            fork: (fork.compute_txid(), fork.compute_wtxid()),
            tips,
            observed_at,
            max_age_seconds,
        }
    }

    fn accepted_for(
        &self,
        controller: &Controller,
        current: &Context,
        record: &PairedSubmissionRecord,
        preflight: &PairEvidence,
        now: i64,
    ) -> bool {
        self.controller == controller.id
            && self.context == *current
            && self.bitcoin == record.bitcoin_identity()
            && self.fork == record.fork_identity()
            && self.tips.bitcoin.hash == preflight.bitcoin().tip()
            && self.tips.fork.hash == preflight.fork().tip()
            && proof_fresh(self.observed_at, now, self.max_age_seconds)
    }
}

/// New proof after Bitcoin confirmation. It cannot be substituted for the
/// initial all-unspent proof, and a restart cannot reconstitute it.
pub(crate) struct PostBitcoinAncestryProof {
    controller: u64,
    context: Context,
    bitcoin: (Txid, Wtxid),
    fork: (Txid, Wtxid),
    tips: PreflightTips,
    observed_at: i64,
    max_age_seconds: i64,
}

impl PostBitcoinAncestryProof {
    fn accepted_for(
        &self,
        controller: &Controller,
        current: &Context,
        record: &PairedSubmissionRecord,
        preflight: &Evidence,
        now: i64,
    ) -> bool {
        self.controller == controller.id
            && self.context == *current
            && self.bitcoin == record.bitcoin_identity()
            && self.fork == record.fork_identity()
            && self.tips.fork.hash == preflight.tip()
            && proof_fresh(self.observed_at, now, self.max_age_seconds)
    }
}

fn proof_fresh(observed_at: i64, now: i64, max_age_seconds: i64) -> bool {
    observed_at >= 0
        && max_age_seconds > 0
        && now
            .checked_sub(observed_at)
            .is_some_and(|age| (0..=max_age_seconds.min(MAX_PROOF_AGE_SECONDS)).contains(&age))
}

#[cfg(test)]
pub(super) fn initial_proof_for_test(
    controller: &Controller,
    current: &Context,
    bitcoin: &Transaction,
    fork: &Transaction,
    tip: BlockHash,
    observed_at: i64,
) -> InitialAncestryProof {
    use coincube_core::claim::BlockRef;
    InitialAncestryProof::after_finalization(
        controller,
        current,
        bitcoin,
        fork,
        PreflightTips {
            bitcoin: BlockRef {
                height: 1,
                hash: tip,
            },
            fork: BlockRef {
                height: 1,
                hash: tip,
            },
        },
        observed_at,
        MAX_PROOF_AGE_SECONDS,
    )
}

#[cfg(test)]
pub(super) fn post_bitcoin_proof_for_test(
    controller: &Controller,
    current: &Context,
    bitcoin: &Transaction,
    fork: &Transaction,
    tip: BlockHash,
    observed_at: i64,
) -> PostBitcoinAncestryProof {
    use coincube_core::claim::BlockRef;
    PostBitcoinAncestryProof {
        controller: controller.id,
        context: current.clone(),
        bitcoin: (bitcoin.compute_txid(), bitcoin.compute_wtxid()),
        fork: (fork.compute_txid(), fork.compute_wtxid()),
        tips: PreflightTips {
            bitcoin: BlockRef {
                height: 1,
                hash: tip,
            },
            fork: BlockRef {
                height: 1,
                hash: tip,
            },
        },
        observed_at,
        max_age_seconds: MAX_PROOF_AGE_SECONDS,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PairedSubmissionRecord {
    bitcoin: Transaction,
    bitcoin_txid: Txid,
    bitcoin_wtxid: Wtxid,
    fork: Transaction,
    fork_txid: Txid,
    fork_wtxid: Wtxid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bitcoin_attempt: Option<PossibleAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fork_attempt: Option<PossibleAttempt>,
}

/// A possible send of the exact signed witness, durably recorded before I/O.
/// It says nothing about whether a node saw or accepted the transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PossibleAttempt {
    txid: Txid,
    wtxid: Wtxid,
    preflight_tip: BlockHash,
    preflight_observed_at: i64,
}

impl PossibleAttempt {
    #[allow(dead_code)] // The paired recovery screen has not been opened yet.
    pub(crate) fn identity(self) -> (Txid, Wtxid) {
        (self.txid, self.wtxid)
    }

    #[allow(dead_code)] // For reconciliation display only, never proof reuse.
    pub(crate) fn preflight(self) -> (BlockHash, i64) {
        (self.preflight_tip, self.preflight_observed_at)
    }
}

impl PairedSubmissionRecord {
    fn new(bitcoin: Transaction, fork: Transaction) -> Self {
        Self {
            bitcoin_txid: bitcoin.compute_txid(),
            bitcoin_wtxid: bitcoin.compute_wtxid(),
            fork_txid: fork.compute_txid(),
            fork_wtxid: fork.compute_wtxid(),
            bitcoin_attempt: None,
            fork_attempt: None,
            bitcoin,
            fork,
        }
    }

    #[allow(dead_code)] // Reconciliation reader; paired submission is still gated.
    pub(crate) fn bitcoin(&self) -> &Transaction {
        &self.bitcoin
    }

    #[allow(dead_code)] // Reconciliation reader; paired submission is still gated.
    pub(crate) fn fork(&self) -> &Transaction {
        &self.fork
    }

    pub(crate) fn bitcoin_identity(&self) -> (Txid, Wtxid) {
        (self.bitcoin_txid, self.bitcoin_wtxid)
    }

    pub(crate) fn fork_identity(&self) -> (Txid, Wtxid) {
        (self.fork_txid, self.fork_wtxid)
    }

    #[allow(dead_code)] // Reconciliation reader; the paired coordinator is gated.
    pub(crate) fn possible_attempt(&self, chain: ChainId) -> Option<PossibleAttempt> {
        match chain {
            ChainId::Bitcoin => self.bitcoin_attempt,
            ChainId::BitcoinBlake2b => self.fork_attempt,
            _ => None,
        }
    }
}

fn unsigned(transaction: &Transaction) -> Transaction {
    let mut unsigned = transaction.clone();
    for input in &mut unsigned.input {
        input.script_sig = ScriptBuf::new();
        input.witness.clear();
    }
    unsigned
}

fn signed_shape(transaction: &Transaction) -> bool {
    transaction.weight().to_wu() <= 4_000_000
        && !transaction.input.is_empty()
        && transaction
            .input
            .iter()
            .all(|input| !input.script_sig.is_empty() || !input.witness.is_empty())
}

pub(super) fn validate(intent: &Intent) -> Result<(), Error> {
    let Some(record) = &intent.paired_submission else {
        return Ok(());
    };
    let fork_unsigned = match intent.version {
        10 => intent
            .funding_plan
            .as_ref()
            .map(|funding| &funding.step2_unsigned),
        11 => split::ancestry_step2_unsigned(intent),
        _ => None,
    }
    .ok_or(Error::InvalidPlan)?;
    if intent.phase != Phase::Intent
        || intent.signed_txid.is_some()
        || intent.fork_submission.is_some()
        || record.bitcoin_identity()
            != (
                record.bitcoin.compute_txid(),
                record.bitcoin.compute_wtxid(),
            )
        || record.fork_identity() != (record.fork.compute_txid(), record.fork.compute_wtxid())
        || !signed_shape(&record.bitcoin)
        || !signed_shape(&record.fork)
        || unsigned(&record.bitcoin) != intent.plan.step1
        || unsigned(&record.fork) != *fork_unsigned
        || record.bitcoin_attempt.is_some_and(|attempt| {
            (attempt.txid, attempt.wtxid) != record.bitcoin_identity()
                || attempt.preflight_observed_at < 0
        })
        || record.fork_attempt.is_some_and(|attempt| {
            (attempt.txid, attempt.wtxid) != record.fork_identity()
                || attempt.preflight_observed_at < 0
        })
        || (record.fork_attempt.is_some() && record.bitcoin_attempt.is_none())
    {
        return Err(Error::InvalidPlan);
    }
    Ok(())
}

impl Controller {
    fn record_signed_pair(
        &mut self,
        current: &Context,
        version: u32,
        record: PairedSubmissionRecord,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        if self.intent.version != version || self.intent.paired_submission.is_some() {
            return Err(Error::Conflict);
        }
        let mut next = self.intent.clone();
        next.paired_submission = Some(record);
        validate_intent(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }

    /// Persist both cryptographically finalized Claim witnesses before either
    /// network attempt. This checks the exact saved unsigned templates; it
    /// neither refreshes ancestry proof nor records a broadcast attempt.
    #[allow(dead_code)] // Staged with the disabled paired Claim signer route.
    pub(crate) fn record_signed_ancestry_claim_pair(
        &mut self,
        current: &Context,
        pair: &VerifiedAncestryClaimPair,
    ) -> Result<(), Error> {
        self.record_signed_pair(
            current,
            10,
            PairedSubmissionRecord::new(
                pair.bitcoin().transaction().clone(),
                pair.fork().transaction().clone(),
            ),
        )
    }

    /// Persist both cryptographically finalized foreign Split witnesses.
    /// A reopened record is for reconciliation only, never a signing cache.
    #[allow(dead_code)] // Staged with the disabled paired Split signer route.
    pub(crate) fn record_signed_ancestry_split_pair(
        &mut self,
        current: &Context,
        pair: &VerifiedAncestrySplitPair,
    ) -> Result<(), Error> {
        self.record_signed_pair(
            current,
            11,
            PairedSubmissionRecord::new(
                pair.bitcoin().transaction().clone(),
                pair.fork().transaction().clone(),
            ),
        )
    }

    #[allow(dead_code)] // Restart reconciliation reader; no current UI caller.
    pub(crate) fn recorded_signed_ancestry_pair(&self) -> Option<&PairedSubmissionRecord> {
        self.intent.paired_submission.as_ref()
    }

    /// Recheck the post-Bitcoin collector against the saved signed pair and
    /// canonical Bitcoin confirmation. The collector checks spent Bitcoin
    /// inputs and unspent BTCB2 inputs; this result expires and is not journaled.
    #[allow(dead_code)] // Stage-two coordinator remains gated.
    pub(super) fn post_bitcoin_ancestry_proof(
        &mut self,
        current: &Context,
        observed: &CollectedPostBitcoinInputs,
        confirmation: &BitcoinConfirmation,
        policy: Policy,
        now: i64,
        tips: PreflightTips,
    ) -> Result<PostBitcoinAncestryProof, Error> {
        self.ensure_context(current)?;
        self.clear_check();
        if !proof_fresh(
            observed.observed_at(),
            now,
            policy.max_observation_age_seconds,
        ) {
            return Err(Error::LateObservation);
        }
        let saved = self
            .intent
            .paired_submission
            .as_ref()
            .ok_or(Error::Unchecked)?;
        if saved.bitcoin_attempt.is_none()
            || saved.fork_attempt.is_some()
            || !confirmation.accepted_for(current, saved.bitcoin(), now)
            || confirmation.block() != observed.bitcoin_confirmation()
        {
            return Err(Error::Conflict);
        }
        observed
            .validate_for_transactions(
                saved.bitcoin(),
                saved.fork(),
                &self.intent.plan.claimed_prevouts,
                FundingContext {
                    provider: &current.provider,
                    generation: current.generation,
                    now,
                    max_age_seconds: policy.max_observation_age_seconds,
                    tips,
                },
            )
            .map_err(|_| Error::Unchecked)?;
        Ok(PostBitcoinAncestryProof {
            controller: self.id,
            context: current.clone(),
            bitcoin: saved.bitcoin_identity(),
            fork: saved.fork_identity(),
            tips,
            observed_at: observed.observed_at(),
            max_age_seconds: policy.max_observation_age_seconds,
        })
    }

    /// Initial Bitcoin attempt requires both exact signed witnesses to have
    /// passed one fresh paired preflight. No previous attempt can be retried.
    #[allow(dead_code)] // Staged with the disabled paired submission coordinator.
    pub(crate) fn record_possible_ancestry_bitcoin_attempt(
        &mut self,
        current: &Context,
        ancestry: &InitialAncestryProof,
        preflight: &PairEvidence,
        policy: FreshnessPolicy,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        let saved = self
            .intent
            .paired_submission
            .as_ref()
            .ok_or(Error::Unchecked)?;
        if !ancestry.accepted_for(self, current, saved, preflight, now) {
            return Err(Error::LateObservation);
        }
        if !preflight.accepted_for(
            saved.bitcoin(),
            saved.fork(),
            current.generation,
            policy,
            now,
        ) {
            return Err(Error::LateObservation);
        }
        self.record_possible_ancestry_attempt(
            current,
            ChainId::Bitcoin,
            preflight.bitcoin(),
            policy,
            now,
        )
    }

    /// The BTCB2 attempt needs a new policy read of its exact witness after
    /// Bitcoin confirms. A fresh Bitcoin mempool preflight would be invalid
    /// once that transaction is mined. The coordinator must also establish a
    /// fresh two-chain ancestry proof and explicit approval. The confirmation
    /// token is rechecked here against this exact signed Bitcoin witness.
    #[allow(dead_code)] // Staged with the disabled paired submission coordinator.
    pub(crate) fn record_possible_ancestry_fork_attempt(
        &mut self,
        current: &Context,
        ancestry: &PostBitcoinAncestryProof,
        preflight: &Evidence,
        confirmation: &BitcoinConfirmation,
        policy: FreshnessPolicy,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        let saved = self
            .intent
            .paired_submission
            .as_ref()
            .ok_or(Error::Unchecked)?;
        if !ancestry.accepted_for(self, current, saved, preflight, now) {
            return Err(Error::LateObservation);
        }
        if !confirmation.accepted_for(current, saved.bitcoin(), now) {
            return Err(Error::LateObservation);
        }
        self.record_possible_ancestry_attempt(
            current,
            ChainId::BitcoinBlake2b,
            preflight,
            policy,
            now,
        )
    }

    /// Persist uncertainty before entering the one-use transport gate. This
    /// function performs no network I/O and grants no send authority.
    fn record_possible_ancestry_attempt(
        &mut self,
        current: &Context,
        chain: ChainId,
        evidence: &Evidence,
        policy: FreshnessPolicy,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        if self.intent.plan.bitcoin_chain != ChainId::Bitcoin
            || self.intent.plan.fork_chain != ChainId::BitcoinBlake2b
        {
            return Err(Error::WrongIdentity);
        }
        let saved = self
            .intent
            .paired_submission
            .as_ref()
            .ok_or(Error::Unchecked)?;
        let transaction = match chain {
            ChainId::Bitcoin => saved.bitcoin(),
            ChainId::BitcoinBlake2b => saved.fork(),
            _ => return Err(Error::WrongIdentity),
        };
        if !evidence.accepted_for(chain, transaction, current.generation, policy, now) {
            return Err(Error::LateObservation);
        }
        match chain {
            ChainId::Bitcoin if saved.bitcoin_attempt.is_none() => {}
            ChainId::BitcoinBlake2b
                if saved.bitcoin_attempt.is_some() && saved.fork_attempt.is_none() => {}
            ChainId::Bitcoin | ChainId::BitcoinBlake2b => return Err(Error::Conflict),
            _ => return Err(Error::WrongIdentity),
        }
        let attempt = PossibleAttempt {
            txid: evidence.txid(),
            wtxid: evidence.wtxid(),
            preflight_tip: evidence.tip(),
            preflight_observed_at: evidence.observed_at(),
        };
        let mut next = self.intent.clone();
        let record = next.paired_submission.as_mut().ok_or(Error::Unchecked)?;
        match chain {
            ChainId::Bitcoin => record.bitcoin_attempt = Some(attempt),
            ChainId::BitcoinBlake2b => record.fork_attempt = Some(attempt),
            _ => unreachable!(),
        }
        validate_intent(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }
}
