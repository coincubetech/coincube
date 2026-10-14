//! Restart data for the two ordinary signed ancestry transactions. This is
//! deliberately part of the same atomically replaced intent as the unsigned
//! plan, so a crash cannot leave a signed pair detached from its wallet and
//! destination binding. Loading it never restores proof, signer authority or
//! permission to send either transaction again.

use super::{split, validate as validate_intent, Context, Controller, Error, Intent, Phase};
use coincube_core::{
    claim_finalize::VerifiedAncestryClaimPair,
    foreign_split::VerifiedAncestrySplitPair,
    miniscript::bitcoin::{ScriptBuf, Transaction, Txid, Wtxid},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PairedSubmissionRecord {
    bitcoin: Transaction,
    bitcoin_txid: Txid,
    bitcoin_wtxid: Wtxid,
    fork: Transaction,
    fork_txid: Txid,
    fork_wtxid: Wtxid,
}

impl PairedSubmissionRecord {
    fn new(bitcoin: Transaction, fork: Transaction) -> Self {
        Self {
            bitcoin_txid: bitcoin.compute_txid(),
            bitcoin_wtxid: bitcoin.compute_wtxid(),
            fork_txid: fork.compute_txid(),
            fork_wtxid: fork.compute_wtxid(),
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
}
