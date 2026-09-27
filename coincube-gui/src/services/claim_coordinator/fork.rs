//! Fork-side confirmation and submission. No signing keys or automatic retry.
use super::*;
use coincube_core::{claim_finalize::VerifiedClaimForkSweep, claim_spend::ClaimForkSweep};

/// Fork backend admitted at the original Claim's paired API origin.
/// This type cannot be used as Bitcoin step-one production services.
pub struct ForkProduction(Production);
impl ForkProduction {
    pub fn new(
        client: CoincubeClient,
        daemon: Arc<dyn Daemon + Send + Sync>,
        account: String,
        expected_generation: u64,
        generation: watch::Receiver<u64>,
    ) -> Result<Self, Error> {
        Production::for_chain(
            client,
            daemon,
            account,
            expected_generation,
            generation,
            ChainId::BitcoinBlake2b,
        )
        .map(Self)
    }
    pub fn context(&self) -> &Context {
        &self.0.context
    }
}
#[async_trait]
trait ForkServices: Send + Sync {
    fn source(&self) -> &dyn ObservationSource;
    async fn preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<Evidence, claim_preflight::Error>;
    async fn submit(
        &self,
        tx: Arc<VerifiedClaimForkSweep>,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError>;
}
#[async_trait]
impl ForkServices for ForkProduction {
    fn source(&self) -> &dyn ObservationSource {
        &self.0.source
    }
    async fn preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<Evidence, claim_preflight::Error> {
        self.0
            .preflight
            .observe(ChainId::BitcoinBlake2b, tx, tip, policy)
            .await
    }
    async fn submit(
        &self,
        tx: Arc<VerifiedClaimForkSweep>,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        self.0.daemon.submit_verified_claim_fork(tx, gate).await
    }
}

/// Owns the same journal lock as step one. The previous coordinator must be
/// dropped before opening this stage. Reopening never restores review authority.
pub struct Coordinator {
    id: u64,
    revision: u64,
    context: Context,
    generation: watch::Receiver<u64>,
    controller: Controller,
    construction: Arc<ClaimForkSweep>,
    verified: Arc<VerifiedClaimForkSweep>,
    services: Box<dyn ForkServices>,
    policy: CheckPolicy,
    revoker: Revoker,
    lifetime: Arc<()>,
}
impl Coordinator {
    /// Caller reconstructs both owned constructions and obtains signing consent.
    /// This opens an existing step-one journal; it never creates a claim record.
    #[allow(clippy::too_many_arguments)]
    pub fn resume(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        source: &PoisonSelfTransfer,
        construction: ClaimForkSweep,
        verified: VerifiedClaimForkSweep,
        production: ForkProduction,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        if production
            .0
            .daemon
            .config()
            .is_none_or(|c| c.main_descriptor != *source.descriptor())
        {
            return Err(Error::InvalidBinding);
        }
        let context = production.0.context.clone();
        let generation = production.0.generation.clone();
        Self::open(
            directory,
            bitcoin_cube,
            fork_cube,
            source,
            construction,
            verified,
            context,
            generation,
            Box::new(production),
            policy,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn open(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        source: &PoisonSelfTransfer,
        construction: ClaimForkSweep,
        verified: VerifiedClaimForkSweep,
        context: Context,
        generation: watch::Receiver<u64>,
        services: Box<dyn ForkServices>,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        if !policy.valid()
            || source.chain() != ChainId::Bitcoin
            || construction.chain() != ChainId::BitcoinBlake2b
            || !admits_descriptor(source.descriptor())
        {
            return Err(Error::Unsupported);
        }
        let mut unsigned = verified.transaction().clone();
        for input in &mut unsigned.input {
            input.witness.clear();
        }
        if construction.descriptor() != source.descriptor()
            || verified.descriptor() != construction.descriptor()
            || construction.bitcoin_step1() != source.psbt().unsigned_tx.compute_txid()
            || verified.bitcoin_step1() != construction.bitcoin_step1()
            || verified.chain() != construction.chain()
            || unsigned != construction.psbt().unsigned_tx
            || *generation.borrow() != context.generation
            || generation.has_changed().is_err()
        {
            return Err(Error::InvalidBinding);
        }
        let identity = WalletIdentity {
            bitcoin_cube,
            fork_cube,
            descriptor_digest: sha256::Hash::hash(source.descriptor().to_string().as_bytes()),
        };
        let mut controller = Controller::reopen(directory, &identity, context.clone())?;
        controller.revalidate_construction(&context, source)?;
        if controller.signed_txid() != Some(construction.bitcoin_step1()) {
            return Err(Error::InvalidBinding);
        }
        if let Some(recorded) = controller.recorded_fork_sweep() {
            if recorded != &unsigned {
                return Err(Error::InvalidBinding);
            }
        }
        if let Some(recorded) = controller.recorded_fork_submission() {
            if recorded.txid() != verified.transaction().compute_txid()
                || recorded.wtxid() != verified.transaction().compute_wtxid()
            {
                return Err(Error::InvalidBinding);
            }
        }
        Ok(Self {
            id: NEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .map_err(|_| Error::Revoked)?,
            revision: 0,
            context,
            generation,
            controller,
            construction: Arc::new(construction),
            verified: Arc::new(verified),
            services,
            policy,
            revoker: Revoker::new(),
            lifetime: Arc::new(()),
        })
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
    pub fn revoker(&self) -> Revoker {
        self.revoker.clone()
    }
    pub fn invalidate(&mut self) {
        self.revoker.revoke();
        self.controller.invalidate();
    }
    pub fn recorded_outcome(&self) -> Option<Outcome> {
        self.controller
            .recorded_fork_submission()
            .map(|s| Outcome::Uncertain {
                txid: s.txid(),
                wtxid: s.wtxid(),
            })
    }
    fn current(&mut self, context: &Context) -> Result<(), Error> {
        if self.revoker.is_revoked()
            || context != &self.context
            || *self.generation.borrow() != context.generation
            || self.generation.has_changed().is_err()
        {
            self.invalidate();
            return Err(Error::Revoked);
        }
        Ok(())
    }
    async fn collect(&self) -> Result<claim_observation::CollectedAssessment, Error> {
        claim_observation::collect(
            self.services.source(),
            &self.controller.plan(),
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
    async fn fresh_snapshot(&mut self, context: &Context) -> Result<ReviewSnapshot, Error> {
        self.current(context)?;
        if self.recorded_outcome().is_some() {
            return Err(Error::SubmissionAlreadyRecorded);
        }
        let ticket = self.controller.begin_check(context)?;
        let first = self.collect().await?;
        if first.assessment != Assessment::ObservationsEligibleForPreflight {
            return Err(Error::NotReady(first.assessment));
        }
        let tx = self.verified.transaction();
        let evidence = self
            .services
            .preflight(tx, first.observations.fork.tip.hash, self.policy.preflight)
            .await
            .map_err(Error::Preflight)?;
        let last = self.collect().await?;
        self.current(context)?;
        if !same_view(first.observations, last.observations) {
            return Err(Error::ChangedReview);
        }
        let tx = self.verified.transaction();
        if evidence.chain() != ChainId::BitcoinBlake2b
            || evidence.txid() != tx.compute_txid()
            || evidence.wtxid() != tx.compute_wtxid()
            || evidence.tip() != last.observations.fork.tip.hash
            || evidence.generation() != context.generation
        {
            return Err(Error::InvalidBinding);
        }
        if evidence.node_policy() != &NodePolicy::Accepted {
            return Err(Error::PolicyRejected(evidence.node_policy().clone()));
        }
        let now = self.services.source().now();
        // Includes wall-clock freshness and monotonic queue/persistence budget.
        let not_after = evidence_deadline(
            self.policy,
            last.observations,
            evidence.observed_at(),
            now,
            Instant::now(),
        )?;
        let status = self.controller.apply_observation(
            ticket,
            context,
            Ok(last),
            self.policy.observations,
            now,
        )?;
        if status != Status::Observation(Assessment::ObservationsEligibleForPreflight) {
            return Err(Error::NotReady(last.assessment));
        }
        if self.controller.recorded_fork_sweep().is_none() {
            self.controller.prepare_fork_sweep(
                context,
                &self.construction,
                self.policy.observations,
                now,
            )?;
        }
        Ok(ReviewSnapshot {
            transaction: tx.clone(),
            wallet: self.controller.identity().clone(),
            txid: tx.compute_txid(),
            wtxid: tx.compute_wtxid(),
            fee_sats: self.verified.fee().to_sat(),
            vsize: tx.vsize(),
            observations: last.observations,
            not_after,
        })
    }
    pub async fn prepare_review(&mut self, context: &Context) -> Result<Review, Error> {
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        let snapshot = self.fresh_snapshot(context).await?;
        Ok(Review {
            coordinator: self.id,
            revision: self.revision,
            snapshot,
        })
    }
    /// Explicit user confirmation of this one-use view, with both chains and
    /// fork policy checked again. Any recorded attempt requires reconciliation.
    pub async fn confirm_and_submit(
        &mut self,
        review: Review,
        context: &Context,
    ) -> Result<Outcome, Error> {
        self.current(context)?;
        if review.coordinator != self.id || review.revision != self.revision {
            return Err(Error::InvalidReview);
        }
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        let refreshed = self.fresh_snapshot(context).await?;
        if review.snapshot.wallet != refreshed.wallet
            || review.snapshot.txid != refreshed.txid
            || review.snapshot.wtxid != refreshed.wtxid
            || !same_view(review.snapshot.observations, refreshed.observations)
        {
            return Err(Error::ChangedReview);
        }
        self.current(context)?;
        if Instant::now() >= refreshed.not_after {
            return Err(Error::ExpiredEvidence);
        }
        self.controller.record_fork_broadcast_intent(
            context,
            &self.verified,
            self.policy.observations,
            self.services.source().now(),
        )?;
        let uncertain = Outcome::Uncertain {
            txid: refreshed.txid,
            wtxid: refreshed.wtxid,
        };
        let (gate, revoker) = SubmissionGate::for_claim_fork(&self.verified, refreshed.not_after);
        let _pending = PendingGate(revoker.clone());
        if self.revoker.register(revoker).is_err() || self.current(context).is_err() {
            return Ok(uncertain);
        }
        let mut generation = self.generation.clone();
        let expected = self.context.generation;
        let cancelled = async {
            loop {
                if generation.changed().await.is_err()
                    || *generation.borrow_and_update() != expected
                {
                    break;
                }
            }
        };
        let result = tokio::select! { biased;
            _ = cancelled => None,
            result = tokio::time::timeout(Duration::from_secs(30), self.services.submit(self.verified.clone(), Arc::new(gate))) => result.ok(),
        };
        if self.current(context).is_err() {
            return Ok(uncertain);
        }
        match result {
            Some(Ok(SubmissionOutcome::UpstreamAccepted { txid, wtxid }))
                if txid == refreshed.txid && wtxid == refreshed.wtxid =>
            {
                Ok(Outcome::UpstreamAccepted { txid, wtxid })
            }
            _ => Ok(uncertain),
        }
    }
    /// Check the recorded fork sweep's current chain inclusion together with
    /// Bitcoin poison validity. A saved submission only identifies what to look
    /// up; it never substitutes for current confirmation or allows a retry.
    pub async fn reconcile_sweep(
        &mut self,
        context: &Context,
    ) -> Result<(Status, claim_observation::TransactionObservation), Error> {
        self.current(context)?;
        let submission = self
            .controller
            .recorded_fork_submission()
            .ok_or(Error::InvalidBinding)?;
        let ticket = self.controller.begin_check(context)?;
        let collected = claim_observation::collect_sweep(
            self.services.source(),
            &self.controller.plan(),
            submission.txid(),
            self.policy.observations,
            self.policy.collection_budget,
            CollectionContext {
                expected_generation: context.generation,
                generation: self.generation.clone(),
            },
        )
        .await
        .map_err(Error::Observation)?;
        self.current(context)?;
        let status = self
            .controller
            .apply_observation(
                ticket,
                context,
                Ok(collected.assessment()),
                self.policy.observations,
                self.services.source().now(),
            )
            .map_err(Error::Journal)?;
        Ok((status, collected.transaction()))
    }

    /// Continues checking Bitcoin poison validity after submission; does not
    /// claim the fork sweep is confirmed or persist split completion.
    pub async fn reconcile(&mut self, context: &Context) -> Result<Status, Error> {
        self.current(context)?;
        let ticket = self.controller.begin_check(context)?;
        let collected = self.collect().await?;
        self.current(context)?;
        self.controller
            .apply_observation(
                ticket,
                context,
                Ok(collected),
                self.policy.observations,
                self.services.source().now(),
            )
            .map_err(Error::Journal)
    }
}
impl Drop for Coordinator {
    fn drop(&mut self) {
        self.revoker.revoke();
    }
}

#[cfg(all(test, unix))]
mod tests;

/// Fresh, one-use permission to hand this exact construction to the signing UI.
/// It is neither a signature, a replay-status assertion nor broadcast authority.
/// The caller must still obtain the user's explicit signing consent.
pub struct SigningCheck {
    preparation: u64,
    revision: u64,
    not_after: Instant,
}

/// Owns the unsigned fork construction and the Claim journal while signatures
/// are collected. Every new signing dispatch needs another fresh chain check.
pub struct Preparation {
    id: u64,
    revision: u64,
    context: Context,
    generation: watch::Receiver<u64>,
    controller: Controller,
    construction: Arc<ClaimForkSweep>,
    services: Box<dyn ForkServices>,
    policy: CheckPolicy,
    revoker: Revoker,
    lifetime: Arc<()>,
}
impl Preparation {
    #[allow(clippy::too_many_arguments)]
    pub fn resume(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        source: &PoisonSelfTransfer,
        construction: ClaimForkSweep,
        production: ForkProduction,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        if production
            .0
            .daemon
            .config()
            .is_none_or(|c| c.main_descriptor != *source.descriptor())
        {
            return Err(Error::InvalidBinding);
        }
        let context = production.0.context.clone();
        let generation = production.0.generation.clone();
        Self::open(
            directory,
            bitcoin_cube,
            fork_cube,
            source,
            construction,
            context,
            generation,
            Box::new(production),
            policy,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn open(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        source: &PoisonSelfTransfer,
        construction: ClaimForkSweep,
        context: Context,
        generation: watch::Receiver<u64>,
        services: Box<dyn ForkServices>,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        if !policy.valid()
            || source.chain() != ChainId::Bitcoin
            || construction.chain() != ChainId::BitcoinBlake2b
            || !admits_descriptor(source.descriptor())
        {
            return Err(Error::Unsupported);
        }
        if construction.descriptor() != source.descriptor()
            || construction.bitcoin_step1() != source.psbt().unsigned_tx.compute_txid()
            || *generation.borrow() != context.generation
            || generation.has_changed().is_err()
        {
            return Err(Error::InvalidBinding);
        }
        let identity = WalletIdentity {
            bitcoin_cube,
            fork_cube,
            descriptor_digest: sha256::Hash::hash(source.descriptor().to_string().as_bytes()),
        };
        let mut controller = Controller::reopen(directory, &identity, context.clone())?;
        controller.revalidate_construction(&context, source)?;
        if controller.signed_txid() != Some(construction.bitcoin_step1()) {
            return Err(Error::InvalidBinding);
        }
        if controller.recorded_fork_submission().is_some() {
            return Err(Error::SubmissionAlreadyRecorded);
        }
        if controller
            .recorded_fork_sweep()
            .is_some_and(|tx| tx != &construction.psbt().unsigned_tx)
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
            construction: Arc::new(construction),
            services,
            policy,
            revoker: Revoker::new(),
            lifetime: Arc::new(()),
        })
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
    pub fn revoker(&self) -> Revoker {
        self.revoker.clone()
    }
    fn current(&mut self, context: &Context) -> Result<(), Error> {
        if self.revoker.is_revoked()
            || context != &self.context
            || *self.generation.borrow() != context.generation
            || self.generation.has_changed().is_err()
        {
            self.revoker.revoke();
            self.controller.invalidate();
            return Err(Error::Revoked);
        }
        Ok(())
    }
    async fn collect(&self) -> Result<claim_observation::CollectedAssessment, Error> {
        claim_observation::collect(
            self.services.source(),
            &self.controller.plan(),
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
    /// Re-checks six Bitcoin confirmations, RDTS validity and both tips. No
    /// unsigned transaction is treated as having passed mempool preflight.
    pub async fn check_signing(&mut self, context: &Context) -> Result<SigningCheck, Error> {
        self.current(context)?;
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        let ticket = self.controller.begin_check(context)?;
        let first = self.collect().await?;
        if first.assessment != Assessment::ObservationsEligibleForPreflight {
            return Err(Error::NotReady(first.assessment));
        }
        let last = self.collect().await?;
        self.current(context)?;
        if !same_view(first.observations, last.observations) {
            return Err(Error::ChangedReview);
        }
        let origin = Instant::now();
        let now = self.services.source().now();
        let mut remaining = self.policy.collection_budget.min(Duration::from_secs(30));
        for stamp in [
            last.observations.bitcoin.observed_at,
            last.observations.fork.observed_at,
            last.observations.deployment.observed_at,
        ] {
            let age = now
                .checked_sub(stamp)
                .filter(|age| *age >= 0 && stamp >= 0)
                .ok_or(Error::ExpiredEvidence)?;
            let seconds = self
                .policy
                .observations
                .max_observation_age_seconds
                .checked_sub(age)
                .and_then(|s| s.checked_sub(1))
                .filter(|s| *s > 0)
                .ok_or(Error::ExpiredEvidence)?;
            remaining = remaining.min(Duration::from_secs(seconds as u64));
        }
        let not_after = origin
            .checked_add(remaining)
            .ok_or(Error::ExpiredEvidence)?;
        let status = self.controller.apply_observation(
            ticket,
            context,
            Ok(last),
            self.policy.observations,
            now,
        )?;
        if status != Status::Observation(Assessment::ObservationsEligibleForPreflight) {
            return Err(Error::NotReady(last.assessment));
        }
        self.controller.prepare_fork_sweep(
            context,
            &self.construction,
            self.policy.observations,
            now,
        )?;
        Ok(SigningCheck {
            preparation: self.id,
            revision: self.revision,
            not_after,
        })
    }
    /// Called immediately at dispatch, after explicit consent. Consumes even an
    /// expired check; the UI must collect fresh evidence for another attempt.
    pub fn signing_psbt(
        &mut self,
        check: SigningCheck,
        current_psbt: &coincube_core::psbt_unified::UnifiedPsbt,
        context: &Context,
    ) -> Result<coincube_core::miniscript::bitcoin::psbt::Psbt, Error> {
        self.current(context)?;
        if check.preparation != self.id || check.revision != self.revision {
            return Err(Error::InvalidReview);
        }
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        if Instant::now() >= check.not_after {
            return Err(Error::ExpiredEvidence);
        }
        coincube_core::claim_finalize::validate_claim_fork_signing(
            &self.construction,
            current_psbt,
        )
        .map_err(|_| Error::InvalidBinding)?;
        Ok(current_psbt.psbt().clone())
    }
    /// Verifies imported signing additions against the owned construction, then
    /// transfers the journal lock into the submission coordinator. This does not
    /// bypass that coordinator's fresh review, confirmation or fork preflight.
    pub fn finish(
        mut self,
        signed: &coincube_core::psbt_unified::UnifiedPsbt,
        context: &Context,
    ) -> Result<Coordinator, Error> {
        self.current(context)?;
        if self.controller.recorded_fork_sweep() != Some(&self.construction.psbt().unsigned_tx) {
            return Err(Error::InvalidReview);
        }
        let verified = coincube_core::claim_finalize::finalize_claim_fork_sweep(
            &self.construction,
            signed,
            &coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only(),
        )
        .map_err(|_| Error::InvalidBinding)?;
        Ok(Coordinator {
            id: self.id,
            revision: self.revision,
            context: self.context,
            generation: self.generation,
            controller: self.controller,
            construction: self.construction,
            verified: Arc::new(verified),
            services: self.services,
            policy: self.policy,
            revoker: self.revoker,
            lifetime: self.lifetime,
        })
    }
}

/// Positive, short-lived poison-split evidence for one owned construction.
/// Only fresh Claim checks in this module can construct it. No serialization or
/// public arbitrary-data constructor; equality means check identity, not freshness.
pub struct SplitEvidence {
    check: (u64, u64),
    construction: Arc<ClaimForkSweep>,
    generation: watch::Receiver<u64>,
    expected_generation: u64,
    revoker: Revoker,
    not_after: Instant,
    owner: std::sync::Weak<()>,
}
impl std::fmt::Debug for SplitEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SplitEvidence").finish_non_exhaustive()
    }
}
impl PartialEq for SplitEvidence {
    fn eq(&self, other: &Self) -> bool {
        self.check == other.check
    }
}
impl Eq for SplitEvidence {}
impl SplitEvidence {
    pub(crate) fn is_live(&self) -> bool {
        self.owner.upgrade().is_some()
            && !self.revoker.is_revoked()
            && *self.generation.borrow() == self.expected_generation
            && self.generation.has_changed().is_ok()
            && Instant::now() < self.not_after
    }
    pub(crate) fn matches(&self, psbt: &coincube_core::psbt_unified::UnifiedPsbt) -> bool {
        self.is_live()
            && coincube_core::claim_finalize::validate_claim_fork_signing(&self.construction, psbt)
                .is_ok()
    }
}
/// The exact PSBT to dispatch plus evidence for the Claim screen's replay label.
/// The evidence does not replace signature verification or fresh submission review.
pub struct SigningDispatch {
    pub psbt: coincube_core::miniscript::bitcoin::psbt::Psbt,
    pub split: Arc<SplitEvidence>,
}
impl Preparation {
    pub fn signing_dispatch(
        &mut self,
        check: SigningCheck,
        current_psbt: &coincube_core::psbt_unified::UnifiedPsbt,
        context: &Context,
    ) -> Result<SigningDispatch, Error> {
        let identity = (check.preparation, check.revision);
        let not_after = check.not_after;
        let psbt = self.signing_psbt(check, current_psbt, context)?;
        let split = Arc::new(SplitEvidence {
            owner: Arc::downgrade(&self.lifetime),
            check: identity,
            construction: self.construction.clone(),
            generation: self.generation.clone(),
            expected_generation: self.context.generation,
            revoker: self.revoker.clone(),
            not_after,
        });
        Ok(SigningDispatch { psbt, split })
    }
}
impl Coordinator {
    /// Refresh the replay label from the same fully checked review shown to the
    /// user. No observation or signature work is skipped by this accessor.
    pub fn split_evidence(
        &mut self,
        review: &Review,
        context: &Context,
    ) -> Result<Arc<SplitEvidence>, Error> {
        self.current(context)?;
        if review.coordinator != self.id || review.revision != self.revision {
            return Err(Error::InvalidReview);
        }
        if Instant::now() >= review.snapshot.not_after {
            return Err(Error::ExpiredEvidence);
        }
        Ok(Arc::new(SplitEvidence {
            owner: Arc::downgrade(&self.lifetime),
            check: (review.coordinator, review.revision),
            construction: self.construction.clone(),
            generation: self.generation.clone(),
            expected_generation: self.context.generation,
            revoker: self.revoker.clone(),
            not_after: review.snapshot.not_after,
        }))
    }
}
