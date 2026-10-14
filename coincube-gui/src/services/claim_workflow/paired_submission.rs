//! Restart data for the two ordinary signed ancestry transactions. This is
//! deliberately part of the same atomically replaced intent as the unsigned
//! plan, so a crash cannot leave a signed pair detached from its wallet and
//! destination binding. Loading it never restores proof, signer authority or
//! permission to send either transaction again.

use super::{
    claim_funding::RequalifiedPostBitcoinClaim, paired_recovery::BitcoinConfirmation, split,
    split_funding::RequalifiedPostBitcoinSplit, validate as validate_intent, Context, Controller,
    Error, Intent, Phase,
};
use crate::services::claim_observation::http::{CollectedPostBitcoinInputs, FundingContext};
use crate::services::claim_preflight::{
    self, Evidence, FreshnessPolicy, PairEvidence, PreflightClient,
};
use coincube_core::{
    chain::ChainId,
    claim::{BlockRef, Policy, PreflightTips},
    claim_finalize::VerifiedAncestryClaimPair,
    claim_spend::{AncestrySelfTransfer, ClaimForkSweep},
    foreign_split::{
        AncestrySplitInputs, AncestrySplitStep1, AncestrySplitStep2, AncestrySplitStep2Inputs,
        VerifiedAncestrySplitPair,
    },
    miniscript::bitcoin::{psbt::Psbt, BlockHash, OutPoint, ScriptBuf, Transaction, Txid, Wtxid},
};
use coincubed::poison_broadcast::{self, SubmissionGate, SubmissionOutcome, SubmissionRevoker};
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    confirmed_bitcoin_block: BlockRef,
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
        confirmation: &BitcoinConfirmation,
        now: i64,
    ) -> bool {
        self.controller == controller.id
            && self.context == *current
            && self.bitcoin == record.bitcoin_identity()
            && self.fork == record.fork_identity()
            && self.tips.fork.hash == preflight.tip()
            && self.confirmed_bitcoin_block == confirmation.block()
            && self.tips.bitcoin == confirmation.tip()
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

/// Two ordinary finalized transactions ready for an explicit first-send
/// review. The variants retain their cryptographic pair type and cannot be
/// assembled from arbitrary transaction bytes.
#[allow(dead_code)] // The first-send production review is not exposed in the wizard yet.
pub(crate) enum InitialSignedPair {
    Claim(VerifiedAncestryClaimPair),
    Split(VerifiedAncestrySplitPair),
}

/// Saved construction hints for restoring the exact initial pair from a new
/// complete source scan. The presence of a signed journal is not fresh chain
/// proof and this value cannot enter a broadcast gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecordedInitialPairPlan {
    pub bitcoin_funding: OutPoint,
    pub fork_funding: OutPoint,
    pub bitcoin_rate_vb: u64,
    pub fork_rate_vb: u64,
}

#[allow(dead_code)] // The first-send production review is not exposed in the wizard yet.
impl InitialSignedPair {
    fn bitcoin(&self) -> &Transaction {
        match self {
            Self::Claim(pair) => pair.bitcoin().transaction(),
            Self::Split(pair) => pair.bitcoin().transaction(),
        }
    }

    fn fork(&self) -> &Transaction {
        match self {
            Self::Claim(pair) => pair.fork().transaction(),
            Self::Split(pair) => pair.fork().transaction(),
        }
    }

    fn fees(&self) -> (u64, u64) {
        match self {
            Self::Claim(pair) => (pair.bitcoin().fee().to_sat(), pair.fork().fee().to_sat()),
            Self::Split(pair) => (pair.bitcoin().fee().to_sat(), pair.fork().fee().to_sat()),
        }
    }
}

/// Exact facts the user must approve before the first Bitcoin send. Each
/// destination comes from the finalized witness, not a previously rendered
/// PSBT or a fee quote. The BTCB2 transaction is displayed now but receives
/// its own later approval after six Bitcoin confirmations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialPairReviewSnapshot {
    pub bitcoin: Transaction,
    pub fork: Transaction,
    pub bitcoin_fee_sats: u64,
    pub fork_fee_sats: u64,
}

/// One-use in-memory consent handoff. Only a saved exact signed pair plus a
/// fresh, controller-bound two-chain proof can prepare it. Reopening a journal
/// never restores this value.
#[allow(dead_code)] // The first-send production review is not exposed in the wizard yet.
pub(crate) struct InitialPairReview {
    pair: InitialSignedPair,
    proof: InitialAncestryProof,
    snapshot: InitialPairReviewSnapshot,
    context: Context,
    prepared_at: Instant,
}

#[derive(Debug)]
#[allow(dead_code)] // The first-send production review is not exposed in the wizard yet.
pub(crate) enum InitialPairSubmissionError {
    Workflow(Error),
    Preflight(claim_preflight::Error),
    SnapshotChanged,
    InvalidReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // The first-send production review is not exposed in the wizard yet.
pub enum InitialPairSubmissionOutcome {
    UpstreamAccepted {
        txid: Txid,
        wtxid: Wtxid,
    },
    /// A possible send is durable; absence or a timeout is never retry
    /// authority. Recovery must inspect the exact saved witness independently.
    Uncertain {
        txid: Txid,
        wtxid: Wtxid,
    },
}

#[allow(dead_code)] // The first-send production review is not exposed in the wizard yet.
fn current_time() -> Result<i64, InitialPairSubmissionError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .ok_or(InitialPairSubmissionError::InvalidReview)
}

impl Controller {
    /// Expose only the saved chain-specific inputs and fee rates needed to
    /// reconstruct an unattempted signed pair. The caller must rescan both
    /// chains, restore the exact PSBTs and obtain new live requalification.
    #[allow(dead_code)] // The resumed paired wizard will consume this hint.
    pub(crate) fn recorded_initial_pair_plan(
        &mut self,
        current: &Context,
    ) -> Result<RecordedInitialPairPlan, Error> {
        self.ensure_context(current)?;
        let signed = self
            .intent
            .paired_submission
            .as_ref()
            .ok_or(Error::Unchecked)?;
        if signed.bitcoin_attempt.is_some() || signed.fork_attempt.is_some() {
            return Err(Error::Unchecked);
        }
        let (bitcoin_funding, fork_funding, bitcoin_rate_vb, fork_rate_vb) =
            match self.intent.version {
                10 => {
                    let bitcoin = self.intent.ancestry.as_ref().ok_or(Error::InvalidJournal)?;
                    let fork = self
                        .intent
                        .funding_plan
                        .as_ref()
                        .ok_or(Error::InvalidJournal)?;
                    (
                        bitcoin.selected,
                        fork.fork.selected,
                        fork.bitcoin_feerate_vb,
                        fork.fork_feerate_vb,
                    )
                }
                11 => split::ancestry_funding_plan(&self.intent).ok_or(Error::InvalidJournal)?,
                _ => return Err(Error::WrongIdentity),
            };
        if bitcoin_funding == fork_funding {
            return Err(Error::InvalidPlan);
        }
        Ok(RecordedInitialPairPlan {
            bitcoin_funding,
            fork_funding,
            bitcoin_rate_vb,
            fork_rate_vb,
        })
    }

    /// Prepare a first-send review after a new complete wallet/source scan,
    /// saved-witness re-verification and live two-chain proof. It makes no
    /// network request and does not record a possible attempt.
    #[allow(dead_code)] // The first-send production review is not exposed in the wizard yet.
    pub(crate) fn prepare_initial_pair_review(
        &mut self,
        current: &Context,
        pair: InitialSignedPair,
        proof: InitialAncestryProof,
        now: i64,
    ) -> Result<InitialPairReview, Error> {
        self.ensure_context(current)?;
        self.clear_check();
        let saved = self
            .intent
            .paired_submission
            .as_ref()
            .ok_or(Error::Unchecked)?;
        if saved.bitcoin_attempt.is_some()
            || saved.fork_attempt.is_some()
            || saved.bitcoin_identity()
                != (
                    pair.bitcoin().compute_txid(),
                    pair.bitcoin().compute_wtxid(),
                )
            || saved.fork_identity() != (pair.fork().compute_txid(), pair.fork().compute_wtxid())
            || !matches!(
                (&pair, self.intent.version),
                (InitialSignedPair::Claim(_), 10) | (InitialSignedPair::Split(_), 11)
            )
            || proof.controller != self.id
            || proof.context != *current
            || proof.bitcoin != saved.bitcoin_identity()
            || proof.fork != saved.fork_identity()
            || !proof_fresh(proof.observed_at, now, proof.max_age_seconds)
        {
            return Err(Error::WrongIdentity);
        }
        let (bitcoin_fee_sats, fork_fee_sats) = pair.fees();
        let snapshot = InitialPairReviewSnapshot {
            bitcoin: pair.bitcoin().clone(),
            fork: pair.fork().clone(),
            bitcoin_fee_sats,
            fork_fee_sats,
        };
        Ok(InitialPairReview {
            pair,
            proof,
            snapshot,
            context: current.clone(),
            prepared_at: Instant::now(),
        })
    }
}

impl InitialPairReview {
    pub(crate) fn snapshot(&self) -> &InitialPairReviewSnapshot {
        &self.snapshot
    }

    /// The user approves exact finalized bytes and fees, not merely a txid
    /// or an address copied from a previous screen. A later live scan may
    /// reconstruct the review, but it cannot change what this approval sends.
    pub(crate) async fn confirm_exact_snapshot_and_submit(
        self,
        expected: &InitialPairReviewSnapshot,
        controller: &mut Controller,
        current: &Context,
        preflight: &PreflightClient,
        policy: FreshnessPolicy,
    ) -> Result<InitialPairSubmissionOutcome, InitialPairSubmissionError> {
        if &self.snapshot != expected {
            return Err(InitialPairSubmissionError::SnapshotChanged);
        }
        self.confirm_and_submit(controller, current, preflight, policy)
            .await
    }

    /// Consume explicit approval. Policy is reread for *both* exact signed
    /// witnesses. The journal's possible Bitcoin send is stored before the
    /// one-use exact-byte Connect gate is entered. From that point every
    /// cancelled, timed-out or rejected transport answer remains uncertain.
    pub(crate) async fn confirm_and_submit(
        self,
        controller: &mut Controller,
        current: &Context,
        preflight: &PreflightClient,
        policy: FreshnessPolicy,
    ) -> Result<InitialPairSubmissionOutcome, InitialPairSubmissionError> {
        if crate::services::claim_ancestry_gate::authorization().is_none()
            || self.context != *current
            || self.prepared_at.elapsed() >= Duration::from_secs(30)
            || !preflight.matches_provider(&current.provider)
        {
            return Err(InitialPairSubmissionError::InvalidReview);
        }
        let evidence = match &self.pair {
            InitialSignedPair::Claim(pair) => {
                preflight
                    .observe_verified_claim_pair(
                        pair,
                        self.proof.tips.bitcoin.hash,
                        self.proof.tips.fork.hash,
                        policy,
                    )
                    .await
            }
            InitialSignedPair::Split(pair) => {
                preflight
                    .observe_verified_split_pair(
                        pair,
                        self.proof.tips.bitcoin.hash,
                        self.proof.tips.fork.hash,
                        policy,
                    )
                    .await
            }
        }
        .map_err(InitialPairSubmissionError::Preflight)?;
        let now = current_time()?;
        if self.prepared_at.elapsed() >= Duration::from_secs(30)
            || self.snapshot.bitcoin != *self.pair.bitcoin()
            || self.snapshot.fork != *self.pair.fork()
        {
            return Err(InitialPairSubmissionError::InvalidReview);
        }
        let durable = controller
            .record_possible_ancestry_bitcoin_attempt(current, &self.proof, &evidence, policy, now)
            .map_err(InitialPairSubmissionError::Workflow)?;
        let txid = self.pair.bitcoin().compute_txid();
        let wtxid = self.pair.bitcoin().compute_wtxid();
        let uncertain = InitialPairSubmissionOutcome::Uncertain { txid, wtxid };
        let not_after = Instant::now() + Duration::from_secs(10);
        let gate = match &self.pair {
            InitialSignedPair::Claim(pair) => durable.into_claim_gate(pair, not_after),
            InitialSignedPair::Split(pair) => durable.into_split_gate(pair, not_after),
        };
        let Ok((gate, revoker)) = gate else {
            // The durable possible-attempt record already exists. A
            // post-journal failure cannot be reported as a safe no-send.
            return Ok(uncertain);
        };
        let mut generation = preflight.session_generation();
        if generation.has_changed().is_err() || *generation.borrow() != current.generation {
            revoker.revoke();
            return Ok(uncertain);
        }
        let origin = preflight.submission_origin().to_owned();
        let submit = tokio::task::spawn_blocking(move || match self.pair {
            InitialSignedPair::Claim(pair) => {
                poison_broadcast::submit_ancestry_claim_bitcoin_to_connect(&pair, &gate, &origin)
            }
            InitialSignedPair::Split(pair) => {
                poison_broadcast::submit_ancestry_split_bitcoin_to_connect(&pair, &gate, &origin)
            }
        });
        let cancelled = async {
            loop {
                if generation.changed().await.is_err()
                    || *generation.borrow_and_update() != current.generation
                {
                    break;
                }
            }
        };
        let result = tokio::select! { biased;
            _ = cancelled => None,
            result = tokio::time::timeout(Duration::from_secs(30), submit) => result.ok().and_then(Result::ok),
        };
        revoker.revoke();
        if generation.has_changed().is_err() || *generation.borrow() != current.generation {
            return Ok(uncertain);
        }
        match result {
            Some(Ok(SubmissionOutcome::UpstreamAccepted {
                txid: observed_txid,
                wtxid: observed_wtxid,
            })) if observed_txid == txid && observed_wtxid == wtxid => {
                Ok(InitialPairSubmissionOutcome::UpstreamAccepted { txid, wtxid })
            }
            _ => Ok(uncertain),
        }
    }
}

/// A separate, one-use approval for the BTCB2 half after the exact Bitcoin
/// witness has six canonical confirmations. It cannot be created from the
/// initial all-unspent proof or a status string.
#[allow(dead_code)] // The staged second-chain wizard will consume this review.
pub(crate) struct PostBitcoinPairReview {
    pair: InitialSignedPair,
    proof: PostBitcoinAncestryProof,
    confirmation: BitcoinConfirmation,
    snapshot: InitialPairReviewSnapshot,
    context: Context,
    prepared_at: Instant,
}

impl Controller {
    /// The caller first rechecks the complete post-Bitcoin wallet/source,
    /// target reservation and retained signatures to obtain `proof` and the
    /// typed pair. This boundary binds those artifacts to the signed journal
    /// and exact six-confirmation token; it makes no network request.
    #[allow(dead_code)] // The staged second-chain wizard will consume this review.
    pub(crate) fn prepare_post_bitcoin_pair_review(
        &mut self,
        current: &Context,
        pair: InitialSignedPair,
        proof: PostBitcoinAncestryProof,
        confirmation: BitcoinConfirmation,
        now: i64,
    ) -> Result<PostBitcoinPairReview, Error> {
        self.ensure_context(current)?;
        self.clear_check();
        let saved = self
            .intent
            .paired_submission
            .as_ref()
            .ok_or(Error::Unchecked)?;
        if saved.bitcoin_attempt.is_none()
            || saved.fork_attempt.is_some()
            || saved.bitcoin_identity()
                != (
                    pair.bitcoin().compute_txid(),
                    pair.bitcoin().compute_wtxid(),
                )
            || saved.fork_identity() != (pair.fork().compute_txid(), pair.fork().compute_wtxid())
            || !matches!(
                (&pair, self.intent.version),
                (InitialSignedPair::Claim(_), 10) | (InitialSignedPair::Split(_), 11)
            )
            || proof.controller != self.id
            || proof.context != *current
            || proof.bitcoin != saved.bitcoin_identity()
            || proof.fork != saved.fork_identity()
            || proof.confirmed_bitcoin_block != confirmation.block()
            || proof.tips.bitcoin != confirmation.tip()
            || !confirmation.accepted_for(current, saved.bitcoin(), now)
            || !proof_fresh(proof.observed_at, now, proof.max_age_seconds)
        {
            return Err(Error::WrongIdentity);
        }
        let (bitcoin_fee_sats, fork_fee_sats) = pair.fees();
        let snapshot = InitialPairReviewSnapshot {
            bitcoin: pair.bitcoin().clone(),
            fork: pair.fork().clone(),
            bitcoin_fee_sats,
            fork_fee_sats,
        };
        Ok(PostBitcoinPairReview {
            pair,
            proof,
            confirmation,
            snapshot,
            context: current.clone(),
            prepared_at: Instant::now(),
        })
    }
}

#[allow(dead_code)] // The staged second-chain wizard will consume this review.
impl PostBitcoinPairReview {
    pub(crate) fn snapshot(&self) -> &InitialPairReviewSnapshot {
        &self.snapshot
    }

    /// A second-chain approval must still name the same finalized pair the
    /// user saw, including its BTCB2 destination and fee. A fresh canonical
    /// Bitcoin confirmation never permits changed BTCB2 bytes implicitly.
    pub(crate) async fn confirm_exact_snapshot_and_submit(
        self,
        expected: &InitialPairReviewSnapshot,
        controller: &mut Controller,
        current: &Context,
        preflight: &PreflightClient,
        policy: FreshnessPolicy,
    ) -> Result<InitialPairSubmissionOutcome, InitialPairSubmissionError> {
        if &self.snapshot != expected {
            return Err(InitialPairSubmissionError::SnapshotChanged);
        }
        self.confirm_and_submit(controller, current, preflight, policy)
            .await
    }

    /// BTCB2 receives its own live policy read, explicit approval and durable
    /// possible-attempt record. Once journaled, every ambiguous outcome is
    /// uncertain and never a reason to retry the same witness automatically.
    pub(crate) async fn confirm_and_submit(
        self,
        controller: &mut Controller,
        current: &Context,
        preflight: &PreflightClient,
        policy: FreshnessPolicy,
    ) -> Result<InitialPairSubmissionOutcome, InitialPairSubmissionError> {
        if crate::services::claim_ancestry_gate::authorization().is_none()
            || self.context != *current
            || self.prepared_at.elapsed() >= Duration::from_secs(30)
            || !preflight.matches_provider(&current.provider)
        {
            return Err(InitialPairSubmissionError::InvalidReview);
        }
        let evidence = match &self.pair {
            InitialSignedPair::Claim(pair) => {
                preflight
                    .observe_verified_claim_fork(pair, self.proof.tips.fork.hash, policy)
                    .await
            }
            InitialSignedPair::Split(pair) => {
                preflight
                    .observe_verified_split_fork(pair, self.proof.tips.fork.hash, policy)
                    .await
            }
        }
        .map_err(InitialPairSubmissionError::Preflight)?;
        let now = current_time()?;
        if self.prepared_at.elapsed() >= Duration::from_secs(30)
            || self.snapshot.bitcoin != *self.pair.bitcoin()
            || self.snapshot.fork != *self.pair.fork()
        {
            return Err(InitialPairSubmissionError::InvalidReview);
        }
        let durable = controller
            .record_possible_ancestry_fork_attempt(
                current,
                &self.proof,
                &evidence,
                &self.confirmation,
                policy,
                now,
            )
            .map_err(InitialPairSubmissionError::Workflow)?;
        let txid = self.pair.fork().compute_txid();
        let wtxid = self.pair.fork().compute_wtxid();
        let uncertain = InitialPairSubmissionOutcome::Uncertain { txid, wtxid };
        let not_after = Instant::now() + Duration::from_secs(10);
        let gate = match &self.pair {
            InitialSignedPair::Claim(pair) => durable.into_claim_gate(pair, not_after),
            InitialSignedPair::Split(pair) => durable.into_split_gate(pair, not_after),
        };
        let Ok((gate, revoker)) = gate else {
            return Ok(uncertain);
        };
        let mut generation = preflight.session_generation();
        if generation.has_changed().is_err() || *generation.borrow() != current.generation {
            revoker.revoke();
            return Ok(uncertain);
        }
        let origin = preflight.submission_origin().to_owned();
        let submit = tokio::task::spawn_blocking(move || match self.pair {
            InitialSignedPair::Claim(pair) => {
                poison_broadcast::submit_ancestry_claim_fork_to_connect(&pair, &gate, &origin)
            }
            InitialSignedPair::Split(pair) => {
                poison_broadcast::submit_ancestry_split_fork_to_connect(&pair, &gate, &origin)
            }
        });
        let cancelled = async {
            loop {
                if generation.changed().await.is_err()
                    || *generation.borrow_and_update() != current.generation
                {
                    break;
                }
            }
        };
        let result = tokio::select! { biased;
            _ = cancelled => None,
            result = tokio::time::timeout(Duration::from_secs(30), submit) => result.ok().and_then(Result::ok),
        };
        revoker.revoke();
        if generation.has_changed().is_err() || *generation.borrow() != current.generation {
            return Ok(uncertain);
        }
        match result {
            Some(Ok(SubmissionOutcome::UpstreamAccepted {
                txid: observed_txid,
                wtxid: observed_wtxid,
            })) if observed_txid == txid && observed_wtxid == wtxid => {
                Ok(InitialPairSubmissionOutcome::UpstreamAccepted { txid, wtxid })
            }
            _ => Ok(uncertain),
        }
    }
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
    confirmation: &BitcoinConfirmation,
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
            bitcoin: confirmation.tip(),
            fork: BlockRef {
                height: 1,
                hash: tip,
            },
        },
        confirmed_bitcoin_block: confirmation.block(),
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

/// One-use handoff created only after a possible send has been durably saved.
/// It binds a gate to the exact finalized witness and cannot be restored from
/// the journal. User approval and a short evidence-derived deadline remain
/// the paired coordinator's responsibility.
#[allow(dead_code)] // The production paired coordinator remains gated.
pub(crate) struct DurableAncestryAttempt {
    chain: ChainId,
    bitcoin: (Txid, Wtxid),
    fork: (Txid, Wtxid),
}

#[allow(dead_code)] // Consumed by the future paired coordinator.
impl DurableAncestryAttempt {
    fn matches(&self, bitcoin: &Transaction, fork: &Transaction) -> bool {
        self.bitcoin == (bitcoin.compute_txid(), bitcoin.compute_wtxid())
            && self.fork == (fork.compute_txid(), fork.compute_wtxid())
    }

    pub(crate) fn into_claim_gate(
        self,
        pair: &VerifiedAncestryClaimPair,
        not_after: Instant,
    ) -> Result<(SubmissionGate, SubmissionRevoker), Error> {
        if !self.matches(pair.bitcoin().transaction(), pair.fork().transaction()) {
            return Err(Error::WrongIdentity);
        }
        match self.chain {
            ChainId::Bitcoin => Ok(SubmissionGate::for_ancestry_claim_bitcoin(pair, not_after)),
            ChainId::BitcoinBlake2b => Ok(SubmissionGate::for_ancestry_claim_fork(pair, not_after)),
            _ => Err(Error::WrongIdentity),
        }
    }

    pub(crate) fn into_split_gate(
        self,
        pair: &VerifiedAncestrySplitPair,
        not_after: Instant,
    ) -> Result<(SubmissionGate, SubmissionRevoker), Error> {
        if !self.matches(pair.bitcoin().transaction(), pair.fork().transaction()) {
            return Err(Error::WrongIdentity);
        }
        match self.chain {
            ChainId::Bitcoin => Ok(SubmissionGate::for_ancestry_split_bitcoin(pair, not_after)),
            ChainId::BitcoinBlake2b => Ok(SubmissionGate::for_ancestry_split_fork(pair, not_after)),
            _ => Err(Error::WrongIdentity),
        }
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
    /// Reverify the two saved Claim witnesses against their exact rebuilt
    /// wallet constructions. This is read-only signature evidence; it grants
    /// no live chain proof and cannot re-enable a submission after restart.
    #[allow(dead_code)] // Stage-two coordinator will consume this after rebuilding the pair.
    pub(crate) fn verify_recorded_ancestry_claim_pair(
        &mut self,
        current: &Context,
        bitcoin: &AncestrySelfTransfer,
        fork: &ClaimForkSweep,
    ) -> Result<VerifiedAncestryClaimPair, Error> {
        self.ensure_context(current)?;
        self.clear_check();
        if self.intent.version != 10 || self.intent.split.is_some() {
            return Err(Error::WrongIdentity);
        }
        let saved = self
            .intent
            .paired_submission
            .as_ref()
            .ok_or(Error::Unchecked)?;
        if bitcoin.psbt().unsigned_tx != self.intent.plan.step1
            || self
                .intent
                .funding_plan
                .as_ref()
                .is_none_or(|funding| fork.psbt().unsigned_tx != funding.step2_unsigned)
        {
            return Err(Error::WrongIdentity);
        }
        coincube_core::claim_finalize::verify_ancestry_claim_pair_transactions(
            bitcoin,
            saved.bitcoin(),
            fork,
            saved.fork(),
            &coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only(),
        )
        .map_err(|_| Error::InvalidPlan)
    }

    /// Reverify the exact saved ordinary Split witnesses after restart. This
    /// reads no chain state, and deliberately returns no live ancestry proof
    /// or transport token. The caller must supply freshly reconstructed,
    /// owned source inputs before any later stage-two qualification.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)] // Stage-two coordinator will consume this after rebuilding the pair.
    pub(crate) fn verify_recorded_ancestry_split_pair(
        &mut self,
        current: &Context,
        bitcoin: &AncestrySplitStep1,
        bitcoin_inputs: &AncestrySplitInputs<'_>,
        fork: &AncestrySplitStep2,
        fork_inputs: &AncestrySplitStep2Inputs<'_>,
    ) -> Result<VerifiedAncestrySplitPair, Error> {
        self.ensure_context(current)?;
        self.clear_check();
        if self.intent.version != 11 || self.intent.split.is_none() {
            return Err(Error::WrongIdentity);
        }
        let saved = self
            .intent
            .paired_submission
            .as_ref()
            .ok_or(Error::Unchecked)?;
        if bitcoin.psbt().unsigned_tx != self.intent.plan.step1
            || Some(&fork.psbt().unsigned_tx) != split::ancestry_step2_unsigned(&self.intent)
        {
            return Err(Error::WrongIdentity);
        }
        coincube_core::foreign_split::verify_ancestry_split_pair_transactions(
            bitcoin,
            bitcoin_inputs,
            saved.bitcoin(),
            fork,
            fork_inputs,
            saved.fork(),
            &coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only(),
        )
        .map_err(|_| Error::InvalidPlan)
    }

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

    pub(crate) fn recorded_signed_ancestry_pair(&self) -> Option<&PairedSubmissionRecord> {
        self.intent.paired_submission.as_ref()
    }

    /// Only a Claim requalification that checked the saved funding reservation
    /// and complete BTCB2 Vault may enter the version-10 stage-two boundary.
    #[allow(clippy::too_many_arguments)]
    fn post_bitcoin_claim_proof(
        &mut self,
        current: &Context,
        requalified: &RequalifiedPostBitcoinClaim,
        confirmation: &BitcoinConfirmation,
        policy: Policy,
        now: i64,
    ) -> Result<(VerifiedAncestryClaimPair, PostBitcoinAncestryProof), Error> {
        if self.intent.version != 10 || self.intent.split.is_some() {
            return Err(Error::WrongIdentity);
        }
        let (pair, fork_psbt) = requalified.reverify_recorded_pair(self, current)?;
        let proof = self.post_bitcoin_ancestry_proof(
            current,
            &requalified.observed,
            &fork_psbt,
            confirmation,
            policy,
            now,
            requalified.tips(),
        )?;
        Ok((pair, proof))
    }

    /// Keep the post-Bitcoin wallet check, recovered signed pair, canonical
    /// confirmation and second-send review in one live controller session.
    /// Reopening a journal alone cannot produce this review.
    #[allow(dead_code)] // The paired Claim wizard will consume this boundary.
    pub(crate) fn prepare_post_bitcoin_claim_review(
        &mut self,
        current: &Context,
        requalified: &RequalifiedPostBitcoinClaim,
        confirmation: BitcoinConfirmation,
        policy: Policy,
        now: i64,
    ) -> Result<PostBitcoinPairReview, Error> {
        let (pair, proof) =
            self.post_bitcoin_claim_proof(current, requalified, &confirmation, policy, now)?;
        self.prepare_post_bitcoin_pair_review(
            current,
            InitialSignedPair::Claim(pair),
            proof,
            confirmation,
            now,
        )
    }

    /// Only a Split requalification that checked the saved source and target
    /// reservation may enter the version-11 stage-two boundary.
    fn post_bitcoin_split_proof(
        &mut self,
        current: &Context,
        requalified: &RequalifiedPostBitcoinSplit,
        confirmation: &BitcoinConfirmation,
        policy: Policy,
        now: i64,
    ) -> Result<(VerifiedAncestrySplitPair, PostBitcoinAncestryProof), Error> {
        if self.intent.version != 11 || self.intent.split.is_none() {
            return Err(Error::WrongIdentity);
        }
        let (pair, fork_psbt) = requalified.reverify_recorded_pair(self, current)?;
        let proof = self.post_bitcoin_ancestry_proof(
            current,
            &requalified.observed,
            &fork_psbt,
            confirmation,
            policy,
            now,
            requalified.tips(),
        )?;
        Ok((pair, proof))
    }

    /// Keep the refreshed source and target checks bound to the recorded
    /// version-11 witnesses through second-send review preparation.
    #[allow(dead_code)] // The paired Split wizard will consume this boundary.
    pub(crate) fn prepare_post_bitcoin_split_review(
        &mut self,
        current: &Context,
        requalified: &RequalifiedPostBitcoinSplit,
        confirmation: BitcoinConfirmation,
        policy: Policy,
        now: i64,
    ) -> Result<PostBitcoinPairReview, Error> {
        let (pair, proof) =
            self.post_bitcoin_split_proof(current, requalified, &confirmation, policy, now)?;
        self.prepare_post_bitcoin_pair_review(
            current,
            InitialSignedPair::Split(pair),
            proof,
            confirmation,
            now,
        )
    }

    /// Recheck post-Bitcoin evidence against the saved signed pair and
    /// canonical Bitcoin confirmation. This private boundary accepts only
    /// the observations inside a Claim or Split requalification.
    #[allow(clippy::too_many_arguments)]
    fn post_bitcoin_ancestry_proof(
        &mut self,
        current: &Context,
        observed: &CollectedPostBitcoinInputs,
        fork_psbt: &Psbt,
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
            || confirmation.tip() != tips.bitcoin
        {
            return Err(Error::Conflict);
        }
        observed
            .validate_for_fork_psbt(
                saved.bitcoin(),
                fork_psbt,
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
        if fork_psbt.unsigned_tx != unsigned(saved.fork()) {
            return Err(Error::WrongIdentity);
        }
        Ok(PostBitcoinAncestryProof {
            controller: self.id,
            context: current.clone(),
            bitcoin: saved.bitcoin_identity(),
            fork: saved.fork_identity(),
            tips,
            confirmed_bitcoin_block: confirmation.block(),
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
    ) -> Result<DurableAncestryAttempt, Error> {
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
    ) -> Result<DurableAncestryAttempt, Error> {
        self.ensure_context(current)?;
        self.clear_check();
        let saved = self
            .intent
            .paired_submission
            .as_ref()
            .ok_or(Error::Unchecked)?;
        if !ancestry.accepted_for(self, current, saved, preflight, confirmation, now) {
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
    ) -> Result<DurableAncestryAttempt, Error> {
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
        let bitcoin = record.bitcoin_identity();
        let fork = record.fork_identity();
        validate_intent(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(DurableAncestryAttempt {
            chain,
            bitcoin,
            fork,
        })
    }
}
