//! P3-3 (#568): an explicitly reviewed resend of exactly the recorded signed
//! step 2 after a submission whose outcome is uncertain, live or after a
//! restart.
//!
//! Once its intent is recorded, every result but the route's exact
//! acceptance is `Uncertain`, including a daemon refusal before any byte
//! left (a backend switch during the send, `BackendUnavailable`). The
//! ordinary review then refuses (`SubmissionAlreadyRecorded`) and reconcile
//! only observes, so a step 2 that never reached the network could not
//! complete. This is step 2's counterpart of step 1's `prepare_resubmission`:
//!
//! - **One distinct, one-use review.** Only
//!   [`SplitStep2Coordinator::prepare_step2_resubmission`] creates a
//!   [`Step2ResubmissionReview`]; review, refresh, reconcile and restart
//!   never do. It needs the journal's record that the latest attempt came
//!   back from a completed send without the route's acceptance, written
//!   only after control returned. So an accepted attempt, or one cancelled,
//!   timed out or interrupted, or whose record failed to write, is never
//!   resent, before or after a restart. The review's reads of the recorded
//!   step 2 run with that record durably withdrawn and give it back only
//!   when they found no sighting, so a sighting the journal fails to record
//!   still ends the resend. A step 2 ever seen on BTCB2 (by
//!   this review or by any reconcile, recorded in the journal) never gets
//!   one either: it left.
//! - **Fresh evidence**, collected around the route's preflight of the exact
//!   bytes and again at confirmation: the recorded signed step 2 absent from
//!   BTCB2 (stable reads keyed by its own txid), every claimed coin still
//!   among its address's BTCB2 unspent outputs, step 1 still six deep on
//!   Bitcoin and absent from BTCB2, RDTS outside the margin, every read
//!   within the observation age. Anything unavailable, stale, changed,
//!   reorged or spent refuses; there is no route fallback.
//! - **Bindings.** The review names this coordinator and its revision (so an
//!   older review refuses), and the session context and generation are
//!   checked again at confirmation. It also binds the exact bytes, the
//!   wallet, the route and its backend binding (the transport's own check),
//!   the attempt count and a deadline.
//! - **One send per confirmation.** The attempt is recorded in the journal
//!   before the send, and a failed write sends nothing. Anything but the
//!   route's exact acceptance is `Uncertain` again. Nothing retries.
//!
//! After a restart, [`SplitStep2Coordinator::resume_uncertain`] rebuilds the
//! recorded unsigned step 2 from freshly authenticated coins at the current
//! BTCB2 tip (the recorded amount, locktime and target), verifies the
//! recorded signed bytes against it with the core verifier, and binds a
//! transport to the target Vault and the Split's Connect origin. It never
//! signs, reserves an address, or builds anything new.
use super::*;
use crate::services::claim_workflow::Step2ReturnHold;
use coincube_core::foreign_split::{
    verify_split_step2_transaction, SplitStep1, VerifiedSplitStep1,
};

/// Explicit, one-use consent to resend exactly the recorded signed step 2.
/// No Clone, serialization or public construction; neither review, refresh,
/// reconcile nor restart creates it.
pub struct Step2ResubmissionReview {
    coordinator: u64,
    revision: u64,
    snapshot: ReviewSnapshot,
    previous_attempts: usize,
}
impl Step2ResubmissionReview {
    #[cfg(test)]
    pub(crate) fn expire_for_test(&mut self) {
        self.snapshot.not_after = Instant::now();
    }
    pub fn snapshot(&self) -> &ReviewSnapshot {
        &self.snapshot
    }
    /// Resends already recorded; the submission intent is not counted.
    pub fn previous_attempts(&self) -> usize {
        self.previous_attempts
    }
}

/// Why a resend was not reviewed or not confirmed.
#[derive(Debug)]
pub enum ResendError {
    /// The same refusals as the ordinary review: not deep enough, reorged,
    /// RDTS margin, step 1 on BTCB2, changed view, refused or stale
    /// preflight, changed backend, revoked session, expired or old review.
    Coordinator(Error),
    /// No step-2 submission is recorded: the ordinary review applies.
    NotRecorded,
    /// The recorded step 2 was seen on BTCB2, now or by an earlier read
    /// (recorded in the journal): it left. Reconcile only.
    Observed,
    /// The latest attempt's return without the route's acceptance is not
    /// recorded: it was accepted, or cancelled, timed out or interrupted
    /// while it may have left. Reconcile only.
    Unsettled,
    /// The journal records the most resends it allows.
    AttemptsExhausted,
    /// A claimed coin is not among its address's fresh BTCB2 unspent
    /// outputs: spent there, so the recorded step 2 can never confirm.
    ClaimedCoinSpent(OutPoint),
    /// Connect could not serve a fresh BTCB2 unspent read. Not evidence of a
    /// spend.
    Unavailable(OutPoint, FailureKind),
}
impl From<Error> for ResendError {
    fn from(error: Error) -> Self {
        Self::Coordinator(error)
    }
}
impl From<claim_workflow::Error> for ResendError {
    fn from(error: claim_workflow::Error) -> Self {
        Self::Coordinator(Error::Journal(error))
    }
}
impl From<SplitCheckError> for ResendError {
    fn from(error: SplitCheckError) -> Self {
        match error {
            SplitCheckError::Coordinator(error) => Self::Coordinator(error),
            SplitCheckError::ClaimedCoinSpent(outpoint) => Self::ClaimedCoinSpent(outpoint),
            SplitCheckError::Unavailable(outpoint, kind) => Self::Unavailable(outpoint, kind),
        }
    }
}

impl SplitStep2Coordinator {
    /// Restart after a recorded step-2 submission, for an explicit resend.
    /// `construction`, `verified` and `fork_height` are step 1 rebuilt from
    /// freshly authenticated coins and its recorded signed bytes, as for
    /// [`SplitPreparation::resume`]; `coins` are the claimed coins freshly
    /// authenticated (B1a). `transport` must be bound to the target Vault
    /// the recorded target derives from and to the Split's Connect origin.
    /// Refused unless step 2's signed bytes and submission are recorded and
    /// verify against the step 2 rebuilt from the journal. The coordinator
    /// reconciles, or offers [`Self::prepare_step2_resubmission`]; its
    /// ordinary review refuses.
    #[allow(clippy::too_many_arguments)]
    pub async fn resume_uncertain(
        directory: &Path,
        target_cube: String,
        construction: &SplitStep1,
        verified: VerifiedSplitStep1,
        fork_height: u64,
        coins: Vec<SplitCoin>,
        production: SplitForkProduction,
        transport: SplitStep2Production,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        let context = production.context.clone();
        let generation = production.generation.clone();
        Self::open_uncertain(
            directory,
            target_cube,
            construction,
            verified,
            fork_height,
            coins,
            context,
            generation,
            Box::new(production),
            Box::new(transport),
            policy,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::services::claim_coordinator::fork::split) async fn open_uncertain(
        directory: &Path,
        target_cube: String,
        construction: &SplitStep1,
        verified: VerifiedSplitStep1,
        fork_height: u64,
        coins: Vec<SplitCoin>,
        context: Context,
        generation: watch::Receiver<u64>,
        services: Box<dyn SplitForkServices>,
        transport: Box<dyn Step2Transport>,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        if !policy.valid() || construction.chain() != ChainId::Bitcoin {
            return Err(Error::Unsupported);
        }
        let mut unsigned = verified.transaction().clone();
        for input in &mut unsigned.input {
            input.script_sig = Default::default();
            input.witness.clear();
        }
        if verified.chain() != construction.chain()
            || verified.construction_txid() != construction.txid()
            || unsigned != construction.psbt().unsigned_tx
            || *generation.borrow() != context.generation
            || generation.has_changed().is_err()
        {
            return Err(Error::InvalidBinding);
        }
        let claimed = claimed_addresses(construction).ok_or(Error::InvalidBinding)?;
        let identity = claim_workflow::split_identity(target_cube, construction.source().digest());
        let mut controller =
            Controller::reopen_settling(directory, &identity, context.clone()).await?;
        controller.revalidate_split_construction(&context, construction, fork_height)?;
        controller.bind_recovered_split_transaction(&context, &verified)?;
        let tracked_txid = verified.transaction().compute_txid();
        let plan = controller.plan();
        let record = controller.recorded_split()?.ok_or(Error::InvalidBinding)?;
        let submission = controller
            .recorded_fork_submission()
            .ok_or(Error::InvalidBinding)?;
        let (Some(signed), Some(sweep)) = (
            controller.recorded_split_step2().cloned(),
            controller.recorded_fork_sweep().cloned(),
        ) else {
            return Err(Error::InvalidBinding);
        };
        if controller.phase() != Phase::Tracking
            || controller.signed_txid() != Some(tracked_txid)
            || plan.step1_txid() != tracked_txid
            || plan.bitcoin_chain != ChainId::Bitcoin
            || claimed.iter().map(|(o, _)| *o).collect::<BTreeSet<_>>()
                != plan.claimed_prevouts.iter().copied().collect()
            || submission.txid() != signed.compute_txid()
            || submission.wtxid() != signed.compute_wtxid()
        {
            return Err(Error::InvalidBinding);
        }
        // The recorded target: the transport's Vault's own receive
        // derivation at the recorded index, at the Split's Connect origin.
        let (Some(index), Some(target), Some(source)) =
            (record.target_index, record.target_script, record.source)
        else {
            return Err(Error::InvalidBinding);
        };
        let target_index =
            ChildNumber::from_normal_idx(index).map_err(|_| Error::InvalidBinding)?;
        if receive_script(transport.descriptor(), target_index) != target
            || transport.origin() != services.origin()
        {
            return Err(Error::InvalidBinding);
        }
        // The current BTCB2 tip from a fresh collection: the recorded
        // locktime must not exceed it.
        let collected = claim_observation::collect(
            services.source(),
            &plan,
            policy.observations,
            policy.collection_budget,
            CollectionContext {
                expected_generation: context.generation,
                generation: generation.clone(),
            },
        )
        .await
        .map_err(Error::Observation)?;
        let tip_height = u32::try_from(collected.observations.fork.tip.height)
            .map_err(|_| Error::InvalidBinding)?;
        let (chain, record_fork_height, claimed_prevouts, recorded) = (
            plan.fork_chain,
            record.fork_height,
            plan.claimed_prevouts.clone(),
            signed.clone(),
        );
        // #614 G1: the rebuild derives the source window and the verifier
        // replays every signature; keep both off the executor.
        let rebuilt = tokio::task::spawn_blocking(move || {
            let inputs = SplitStep2Inputs {
                chain,
                source: &source,
                coins: &coins,
                fork_height: record_fork_height,
                claimed: &claimed_prevouts,
                target: &target,
            };
            let construction = reconstruct_split_step2(&inputs, &sweep, tip_height)
                .map_err(|_| Error::InvalidBinding)?;
            verify_split_step2_transaction(
                &construction,
                &recorded,
                &secp256k1::Secp256k1::verification_only(),
            )
            .map_err(|_| Error::InvalidBinding)
        })
        .await
        .map_err(|_| Error::Revoked)??;
        if rebuilt.chain() != plan.fork_chain
            || rebuilt.transaction() != &signed
            || *generation.borrow() != context.generation
            || generation.has_changed().is_err()
        {
            return Err(Error::InvalidBinding);
        }
        Ok(Self {
            id: NEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .map_err(|_| Error::Revoked)?,
            revision: 0,
            context,
            generation,
            controller,
            verified: Arc::new(rebuilt),
            target_index,
            claimed,
            services,
            transport,
            policy,
            revoker: Revoker::new(),
        })
    }

    /// The journal's side of a resend: a recorded submission of exactly this
    /// coordinator's verified step 2, never seen on BTCB2, whose latest
    /// attempt is recorded as returned without acceptance, under the limit.
    fn resendable(&self) -> Result<(), ResendError> {
        let Some(submission) = self.controller.recorded_fork_submission() else {
            return Err(ResendError::NotRecorded);
        };
        let tx = self.verified.transaction();
        if self.controller.recorded_split_step2() != Some(tx)
            || submission.txid() != tx.compute_txid()
            || submission.wtxid() != tx.compute_wtxid()
        {
            return Err(Error::InvalidBinding.into());
        }
        if self.controller.split_step2_observed() {
            return Err(ResendError::Observed);
        }
        if !self.controller.split_step2_returned() {
            return Err(ResendError::Unsettled);
        }
        if self.controller.split_step2_resubmissions()
            >= claim_workflow::MAX_SPLIT_STEP2_RESUBMISSIONS
        {
            return Err(ResendError::AttemptsExhausted);
        }
        Ok(())
    }

    /// Both chains, with the recorded step 2 read by its own txid on BTCB2.
    async fn collect_step2(&self) -> Result<claim_observation::SweepObservation, Error> {
        claim_observation::collect_sweep(
            self.services.source(),
            &self.controller.plan(),
            self.verified.transaction().compute_txid(),
            self.policy.observations,
            self.policy.collection_budget,
            CollectionContext {
                expected_generation: self.context.generation,
                generation: self.generation.clone(),
            },
        )
        .await
        .map_err(Error::Observation)
    }

    /// The recorded step 2 absent from BTCB2, and step 1 eligible: six deep
    /// on Bitcoin, absent from BTCB2, RDTS outside the margin. A step 2 seen
    /// on BTCB2 is recorded as observed before refusing.
    fn absent(
        &mut self,
        context: &Context,
        collected: &claim_observation::SweepObservation,
        sighted: &mut bool,
    ) -> Result<(), ResendError> {
        let seen = collected.transaction();
        if seen != claim_observation::TransactionObservation::Absent {
            // Set before the write: the withdrawn resend permission is not
            // given back whether or not the sighting records.
            *sighted = true;
            self.controller.record_split_step2_observed(context, seen)?;
            return Err(ResendError::Observed);
        }
        let data = collected.assessment();
        if data.assessment != Assessment::ObservationsEligibleForPreflight
            || !self.deep_enough(&data.observations)
        {
            return Err(Error::NotReady(data.assessment).into());
        }
        Ok(())
    }

    /// The fresh evidence for a resend; see the module documentation.
    /// Applies the last collection to the journal and returns that
    /// collection's read of the recorded step 2 (absent).
    async fn resend_snapshot(
        &mut self,
        context: &Context,
    ) -> Result<
        (
            ReviewSnapshot,
            claim_observation::TransactionObservation,
            Step2ReturnHold,
        ),
        ResendError,
    > {
        self.current(context)?;
        self.resendable()?;
        let hold = self
            .controller
            .hold_split_step2_return(context)?
            .ok_or(ResendError::Unsettled)?;
        let mut sighted = false;
        match self.held_evidence(context, &mut sighted).await {
            Ok((snapshot, step2)) => Ok((snapshot, step2, hold)),
            Err(error) => {
                if !sighted {
                    self.restore(context, hold);
                }
                Err(error)
            }
        }
    }

    /// Give back a resend permission withdrawn for reads that found no
    /// sighting, if the session is still current. A failed write leaves it
    /// withdrawn, which only refuses later resends; the journal is then
    /// poisoned and every later step refuses anyway.
    fn restore(&mut self, context: &Context, hold: Step2ReturnHold) {
        if self.current(context).is_ok() {
            let _withdrawn_on_failure = self.controller.release_split_step2_return(context, hold);
        }
    }

    /// The evidence itself, under a withdrawn resend permission; `sighted`
    /// is set by any read that saw the recorded step 2 on BTCB2.
    async fn held_evidence(
        &mut self,
        context: &Context,
        sighted: &mut bool,
    ) -> Result<(ReviewSnapshot, claim_observation::TransactionObservation), ResendError> {
        let ticket = self.controller.begin_check(context)?;
        let first = self.collect_step2().await?;
        self.current(context)?;
        self.absent(context, &first, sighted)?;
        let unspent_at =
            claimed_unspent_on_fork(self.services.as_ref(), &self.claimed, self.policy).await?;
        let tx = self.verified.transaction().clone();
        let evidence = self
            .transport
            .preflight(
                &tx,
                first.assessment().observations.fork.tip.hash,
                self.policy.preflight,
            )
            .await
            .map_err(Error::Preflight)?;
        let last = self.collect_step2().await?;
        self.current(context)?;
        let observations = last.assessment().observations;
        if !same_view(first.assessment().observations, observations) {
            return Err(Error::ChangedReview.into());
        }
        self.absent(context, &last, sighted)?;
        let chain_ok = match &evidence {
            RoutedEvidence::Connect(evidence) => evidence.chain() == ChainId::BitcoinBlake2b,
            // The node's best block was the BTCB2 tip observed via Connect.
            RoutedEvidence::BitcoinNode(..) => true,
        };
        if !chain_ok
            || evidence.txid() != tx.compute_txid()
            || evidence.wtxid() != tx.compute_wtxid()
            || evidence.tip() != observations.fork.tip.hash
            || evidence.generation() != context.generation
        {
            return Err(Error::InvalidBinding.into());
        }
        if evidence.node_policy() != &NodePolicy::Accepted {
            return Err(Error::PolicyRejected(evidence.node_policy().clone()).into());
        }
        let now = self.services.source().now();
        let origin = Instant::now();
        let not_after = evidence_deadline(
            self.policy,
            observations,
            evidence.observed_at(),
            now,
            origin,
        )?
        .min(observation_deadline(
            self.policy,
            first.observed_at().min(last.observed_at()).min(unspent_at),
            now,
            origin,
        )?);
        let status = self.controller.apply_observation(
            ticket,
            context,
            Ok(last.assessment()),
            self.policy.observations,
            now,
        )?;
        if status != Status::Observation(Assessment::ObservationsEligibleForPreflight) {
            return Err(Error::NotReady(last.assessment().assessment).into());
        }
        Ok((
            ReviewSnapshot {
                transaction: tx.clone(),
                wallet: self.controller.identity().clone(),
                txid: tx.compute_txid(),
                wtxid: tx.compute_wtxid(),
                fee_sats: self.verified.fee().to_sat(),
                vsize: tx.vsize(),
                observations,
                route: evidence.route(),
                not_after,
            },
            last.transaction(),
        ))
    }

    /// A fresh resend review of the recorded signed step 2, after a recorded
    /// submission whose outcome is uncertain. Records nothing; refused while
    /// no submission is recorded, once the step 2 was ever seen on BTCB2,
    /// until the latest attempt is recorded as returned without acceptance,
    /// at the attempt limit, and on any stale or failing evidence.
    pub async fn prepare_step2_resubmission(
        &mut self,
        context: &Context,
    ) -> Result<Step2ResubmissionReview, ResendError> {
        self.current(context)?;
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        let (snapshot, _, hold) = self.resend_snapshot(context).await?;
        self.controller.release_split_step2_return(context, hold)?;
        Ok(Step2ResubmissionReview {
            coordinator: self.id,
            revision: self.revision,
            snapshot,
            previous_attempts: self.controller.split_step2_resubmissions(),
        })
    }

    /// Explicit user confirmation of this one-use resend review. All the
    /// evidence is collected again on the same route and must show the same
    /// view; the attempt is recorded before the one send. `Uncertain` again
    /// means reconcile, or another explicit review.
    pub async fn confirm_step2_resubmission(
        &mut self,
        review: Step2ResubmissionReview,
        context: &Context,
    ) -> Result<Outcome, ResendError> {
        self.current(context)?;
        if review.coordinator != self.id || review.revision != self.revision {
            return Err(Error::InvalidReview.into());
        }
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        if Instant::now() >= review.snapshot.not_after {
            return Err(Error::ExpiredEvidence.into());
        }
        let (mut refreshed, step2, hold) = self.resend_snapshot(context).await?;
        if review.snapshot.transaction != refreshed.transaction
            || review.snapshot.wallet != refreshed.wallet
            || review.snapshot.txid != refreshed.txid
            || review.snapshot.wtxid != refreshed.wtxid
            || review.snapshot.route != refreshed.route
            || review.previous_attempts != self.controller.split_step2_resubmissions()
            || !same_view(review.snapshot.observations, refreshed.observations)
        {
            self.restore(context, hold);
            return Err(Error::ChangedReview.into());
        }
        refreshed.not_after = refreshed.not_after.min(review.snapshot.not_after);
        self.current(context)?;
        if Instant::now() >= refreshed.not_after {
            self.restore(context, hold);
            return Err(Error::ExpiredEvidence.into());
        }
        self.controller.record_split_step2_resubmission(
            context,
            &self.verified,
            step2,
            hold,
            self.policy.observations,
            self.services.source().now(),
        )?;
        Ok(self.send_recorded(context, refreshed).await)
    }
}
