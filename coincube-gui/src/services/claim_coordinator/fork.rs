//! Fork-side confirmation and submission. No signing keys or automatic retry.
use super::*;
mod source;
use coincube_core::claim_spend::AncestrySelfTransfer;
use coincube_core::{claim_finalize::VerifiedClaimForkSweep, claim_spend::ClaimForkSweep};
use source::Source;

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
    fn ancestry_source(&self) -> Option<&HttpObservationSource> {
        None
    }
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
    fn ancestry_source(&self) -> Option<&HttpObservationSource> {
        Some(&self.0.source)
    }
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
    completion_revoker: Revoker,
    #[cfg(test)]
    completion_cleanup_barrier: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
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
    /// Reopen an owned ancestry claim without restoring eligibility. Signing
    /// requires fresh live proof, depth checks and explicit one-use dispatch.
    #[allow(clippy::too_many_arguments)]
    pub fn resume_ancestry(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        source: &AncestrySelfTransfer,
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
    fn open<'a>(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        source: impl Into<Source<'a>>,
        construction: ClaimForkSweep,
        verified: VerifiedClaimForkSweep,
        context: Context,
        generation: watch::Receiver<u64>,
        services: Box<dyn ForkServices>,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        let source = source.into();
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
        let mut controller =
            Controller::reopen_settling_blocking(directory, &identity, context.clone())?;
        source.revalidate(&mut controller, &context)?;
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
            completion_revoker: Revoker::new(),
            #[cfg(test)]
            completion_cleanup_barrier: None,
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
    async fn collect(&self) -> Result<Collected, Error> {
        if let Some(path) = self.controller.recorded_ancestry()? {
            let source = self.services.ancestry_source().ok_or(Error::Unsupported)?;
            let proof = source
                .collect_ancestry(
                    &path,
                    &self.controller.plan(),
                    self.policy.observations,
                    self.policy.collection_budget,
                )
                .await
                .map_err(Error::Observation)?;
            return Collected::ancestry(
                proof,
                &path,
                &self.controller.plan(),
                &self.context,
                self.policy.observations,
                source.now(),
            );
        }
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
        .map(Collected::ordinary)
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
        let last_data = last.data;
        let status = last.apply(
            &mut self.controller,
            ticket,
            context,
            self.policy.observations,
            now,
        )?;
        if status != Status::Observation(Assessment::ObservationsEligibleForPreflight) {
            return Err(Error::NotReady(last_data.assessment));
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
            observations: last_data.observations,
            route: SubmissionRoute::Connect,
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
    /// Obtain short-lived evidence for saving historical completion metadata.
    /// The record remains subject to reorg checks and never authorizes a spend.
    pub async fn check_completion(
        &mut self,
        context: &Context,
    ) -> Result<Option<CompletionEvidence>, Error> {
        let origin = Instant::now();
        let (_, checked) = self.checked_sweep(context).await?;
        // Confirmed conflicting spends separate the chains even after RDTS
        // expires. This is historical metadata, never signing authority.
        let plan = self.controller.plan();
        // New ancestry completion and GUI signer dispatch share one production
        // authorization. Existing markers can still be reconciled below.
        if plan.poison == coincube_core::claim::Poison::InputAncestry
            && crate::services::claim_ancestry_gate::authorization().is_none()
        {
            return Ok(None);
        }
        let observations = checked.assessment().observations;
        if !completion_bitcoin_confirmed(&plan, observations.bitcoin)
            || observations.fork.chain != plan.fork_chain
            || observations.fork.step1_txid != plan.step1.compute_txid()
            || observations.fork.step1_presence
                != coincube_core::claim::ForkTransactionPresence::NotObserved
        {
            return Ok(None);
        }
        let claim_observation::TransactionObservation::Confirmed { txid, block } =
            checked.transaction()
        else {
            return Ok(None);
        };
        let not_after = evidence_deadline(
            self.policy,
            checked.assessment().observations,
            checked.observed_at(),
            self.services.source().now(),
            origin,
        )?;
        Ok(Some(CompletionEvidence {
            descriptor_fingerprint: crate::app::wallet::descriptor_id_fingerprint(
                self.construction.descriptor(),
            )
            .to_string(),
            descriptor_checksum: crate::app::settings::WalletId::generate(
                self.construction.descriptor(),
            )
            .descriptor_checksum,
            check_revoker: self.completion_revoker.clone(),
            wallet: self.controller.identity().clone(),
            block,
            txid,
            fork_chain: self.construction.chain(),
            lifetime: Arc::downgrade(&self.lifetime),
            generation: self.generation.clone(),
            expected_generation: context.generation,
            revoker: self.revoker.clone(),
            not_after,
        }))
    }

    /// Check the recorded fork sweep's current chain inclusion together with
    /// Bitcoin poison validity. A saved submission only identifies what to look
    /// up; it never substitutes for current confirmation or allows a retry.
    pub async fn reconcile_sweep(
        &mut self,
        context: &Context,
    ) -> Result<(Status, claim_observation::TransactionObservation), Error> {
        let (status, checked) = self.checked_sweep(context).await?;
        Ok((status, checked.transaction()))
    }
    async fn checked_sweep(
        &mut self,
        context: &Context,
    ) -> Result<(Status, claim_observation::SweepObservation), Error> {
        self.completion_revoker.revoke();
        self.completion_revoker = Revoker::new();
        self.current(context)?;
        let submission = self
            .controller
            .recorded_fork_submission()
            .ok_or(Error::InvalidBinding)?;
        let ticket = self.controller.begin_check(context)?;
        let (collected, ancestry) = if let Some(path) = self.controller.recorded_ancestry()? {
            let source = self.services.ancestry_source().ok_or(Error::Unsupported)?;
            source
                .validate_context(&context.provider, context.generation)
                .map_err(|kind| {
                    Error::Observation(claim_observation::Failure {
                        stage: claim_observation::Stage::Context,
                        kind,
                    })
                })?;
            let (proof, sweep) = source
                .collect_ancestry_sweep(
                    &path,
                    &self.controller.plan(),
                    submission.txid(),
                    self.policy.observations,
                    self.policy.collection_budget,
                )
                .await
                .map_err(Error::Observation)?
                .into_parts();
            (sweep, Some(proof))
        } else {
            (
                claim_observation::collect_sweep(
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
                .map_err(Error::Observation)?,
                None,
            )
        };
        self.current(context)?;
        let status = match ancestry {
            Some(proof) => self.controller.apply_ancestry_observation(
                ticket,
                context,
                Ok(proof),
                self.policy.observations,
                self.services.source().now(),
            ),
            None => self.controller.apply_observation(
                ticket,
                context,
                Ok(collected.assessment()),
                self.policy.observations,
                self.services.source().now(),
            ),
        }
        .map_err(Error::Journal)?;
        Ok((status, collected))
    }

    /// Continues checking Bitcoin poison validity after submission; does not
    /// claim the fork sweep is confirmed or persist split completion.
    pub async fn reconcile(&mut self, context: &Context) -> Result<Status, Error> {
        self.completion_revoker.revoke();
        self.current(context)?;
        let ticket = self.controller.begin_check(context)?;
        let collected = self.collect().await?;
        self.current(context)?;
        collected
            .apply(
                &mut self.controller,
                ticket,
                context,
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
    /// Reopen an owned ancestry claim without restoring eligibility. Signing
    /// requires fresh live proof, depth checks and explicit one-use dispatch.
    #[allow(clippy::too_many_arguments)]
    pub fn resume_ancestry(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        source: &AncestrySelfTransfer,
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
    fn open<'a>(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        source: impl Into<Source<'a>>,
        construction: ClaimForkSweep,
        context: Context,
        generation: watch::Receiver<u64>,
        services: Box<dyn ForkServices>,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        let source = source.into();
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
        let mut controller =
            Controller::reopen_settling_blocking(directory, &identity, context.clone())?;
        source.revalidate(&mut controller, &context)?;
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
    async fn collect(&self) -> Result<Collected, Error> {
        if let Some(path) = self.controller.recorded_ancestry()? {
            let source = self.services.ancestry_source().ok_or(Error::Unsupported)?;
            let proof = source
                .collect_ancestry(
                    &path,
                    &self.controller.plan(),
                    self.policy.observations,
                    self.policy.collection_budget,
                )
                .await
                .map_err(Error::Observation)?;
            return Collected::ancestry(
                proof,
                &path,
                &self.controller.plan(),
                &self.context,
                self.policy.observations,
                source.now(),
            );
        }
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
        .map(Collected::ordinary)
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
        let last_data = last.data;
        let status = last.apply(
            &mut self.controller,
            ticket,
            context,
            self.policy.observations,
            now,
        )?;
        if status != Status::Observation(Assessment::ObservationsEligibleForPreflight) {
            return Err(Error::NotReady(last_data.assessment));
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
        let signing = current_psbt.psbt().clone();
        // Validation and cloning consume the same one-use deadline. A session
        // change during that work must not hand a signer an obsolete request.
        self.current(context)?;
        if Instant::now() >= check.not_after {
            return Err(Error::ExpiredEvidence);
        }
        Ok(signing)
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
            completion_revoker: Revoker::new(),
            #[cfg(test)]
            completion_cleanup_barrier: None,
        })
    }
}

/// Positive, short-lived poison-split evidence for one owned construction.
/// Only fresh Claim checks in this module can construct it. No serialization or
/// public arbitrary-data constructor; equality means check identity, not freshness.
pub struct SplitEvidence {
    signing_consumed: std::sync::atomic::AtomicBool,
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
    /// All Arc clones share one signer-dispatch allowance. Review evidence is
    /// created already consumed: it is display evidence, not a signing check.
    pub(crate) fn consume_for_signing(
        &self,
        psbt: &coincube_core::psbt_unified::UnifiedPsbt,
    ) -> bool {
        self.matches(psbt)
            && self
                .signing_consumed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
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
            signing_consumed: std::sync::atomic::AtomicBool::new(false),
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
            signing_consumed: std::sync::atomic::AtomicBool::new(true),
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

/// Non-serializable, generation- and lifetime-bound evidence that the recorded
/// sweep is currently confirmed and the Bitcoin poison still has required depth.
/// Only the coordinator's fresh completion check constructs this value.
pub struct CompletionEvidence {
    descriptor_fingerprint: String,
    descriptor_checksum: String,
    check_revoker: Revoker,
    wallet: WalletIdentity,
    block: coincube_core::claim::BlockRef,
    txid: Txid,
    fork_chain: ChainId,
    lifetime: std::sync::Weak<()>,
    generation: watch::Receiver<u64>,
    expected_generation: u64,
    revoker: Revoker,
    not_after: Instant,
}

/// Result of reconciling already persisted completion state. Stable ancestry
/// invalidation has no truthful sweep transaction observation, so it is kept
/// distinct from the ordinary observation result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionReconciliation {
    Observed {
        status: Status,
        transaction: claim_observation::TransactionObservation,
    },
    AncestryInvalidated {
        kind: claim_observation::FailureKind,
    },
}
impl CompletionEvidence {
    pub fn is_live(&self) -> bool {
        self.lifetime.upgrade().is_some()
            && !self.check_revoker.is_revoked()
            && !self.revoker.is_revoked()
            && self.generation.has_changed().is_ok()
            && *self.generation.borrow() == self.expected_generation
            && Instant::now() < self.not_after
    }
    pub fn wallet(&self) -> &WalletIdentity {
        &self.wallet
    }
    pub fn block(&self) -> coincube_core::claim::BlockRef {
        self.block
    }
    pub fn txid(&self) -> Txid {
        self.txid
    }
    pub fn fork_chain(&self) -> ChainId {
        self.fork_chain
    }
}

impl CompletionEvidence {
    /// Save the historical height on both paired Cubes. Every retry requires
    /// live evidence; a partial I/O failure is returned and never reported as
    /// paired success. Each chain keeps its existing cooperating-writer lock.
    pub async fn persist(
        &self,
        root: &crate::dir::CoincubeDirectory,
    ) -> Result<(), crate::app::settings::SettingsError> {
        self.persist_with_hook(root, std::future::ready(())).await
    }
    async fn persist_with_hook<F: std::future::Future<Output = ()>>(
        &self,
        root: &crate::dir::CoincubeDirectory,
        between_files: F,
    ) -> Result<(), crate::app::settings::SettingsError> {
        use crate::app::settings::{update_settings_file_checked, SettingsError};
        let bitcoin_chain = match self.fork_chain {
            ChainId::BitcoinBlake2b => ChainId::Bitcoin,
            ChainId::BitcoinBlake2bTestnet4 => ChainId::Testnet4,
            _ => return Err(SettingsError::Unexpected("Invalid Claim chain pair".into())),
        };
        let pair = [
            (self.fork_chain, &self.wallet.fork_cube),
            (bitcoin_chain, &self.wallet.bitcoin_cube),
        ];
        // Refuse missing/mismatched Cubes before either file is changed. The
        // same checks run again under each writer lock to detect concurrent edits.
        for (chain, id) in pair {
            let mut settings =
                crate::app::settings::Settings::from_file(&root.network_directory(chain))?;
            self.apply_completion(&mut settings, chain, id)?;
        }
        let mut between_files = Some(between_files);
        for (chain, id) in pair {
            update_settings_file_checked(&root.network_directory(chain), |mut settings| {
                self.apply_completion(&mut settings, chain, id)?;
                Ok(Some(settings))
            })
            .await?;
            if let Some(hook) = between_files.take() {
                hook.await;
            }
        }
        if !self.is_live() {
            return Err(SettingsError::Unexpected(
                "Claim confirmation expired while saving; check both chains again".into(),
            ));
        }
        Ok(())
    }
    fn apply_completion(
        &self,
        settings: &mut crate::app::settings::Settings,
        chain: ChainId,
        id: &str,
    ) -> Result<(), crate::app::settings::SettingsError> {
        use crate::app::settings::SettingsError;
        if !self.is_live() {
            return Err(SettingsError::Unexpected(
                "Claim confirmation expired or changed; check both chains again".into(),
            ));
        }
        let cube = matching_completion_cube(
            settings,
            chain,
            id,
            &self.descriptor_fingerprint,
            &self.descriptor_checksum,
        )?;
        cube.split_completed_at_height = Some(self.block.height);
        cube.split_completion_txid = Some(self.txid);
        Ok(())
    }
}

fn matching_completion_cube<'a>(
    settings: &'a mut crate::app::settings::Settings,
    chain: ChainId,
    id: &str,
    descriptor_fingerprint: &str,
    descriptor_checksum: &str,
) -> Result<&'a mut crate::app::settings::CubeSettings, crate::app::settings::SettingsError> {
    use crate::app::settings::SettingsError;
    if settings.cubes.iter().filter(|cube| cube.id == id).count() != 1 {
        return Err(SettingsError::Unexpected(
            "Claim Cube is missing or ambiguous".into(),
        ));
    }
    let cube = settings
        .cubes
        .iter_mut()
        .find(|cube| cube.id == id)
        .ok_or_else(|| SettingsError::Unexpected("Claim Cube is missing".into()))?;
    if cube.network != chain
        || cube.vault_fingerprint.as_deref() != Some(descriptor_fingerprint)
        || cube
            .vault_wallet_id
            .as_ref()
            .is_none_or(|wallet| wallet.descriptor_checksum != descriptor_checksum)
    {
        return Err(SettingsError::Unexpected(
            "Claim Cube no longer matches its Vault".into(),
        ));
    }
    Ok(cube)
}

// Positive inclusion evidence is required: absence of a known loss also covers
// unknown observations and must never be enough to record completion.
fn completion_bitcoin_confirmed(
    plan: &coincube_core::claim::ClaimPlan,
    bitcoin: coincube_core::claim::BitcoinObservation,
) -> bool {
    use coincube_core::claim::{TransactionLocation, MIN_CONFIRMATIONS};
    let TransactionLocation::Confirmed {
        txid,
        block,
        best_chain_hash_at_height,
    } = bitcoin.location
    else {
        return false;
    };
    bitcoin.chain == plan.bitcoin_chain
        && txid == plan.step1.compute_txid()
        && block.hash == best_chain_hash_at_height
        && plan
            .previous_confirmation
            .is_none_or(|previous| previous == block)
        && bitcoin
            .tip
            .height
            .checked_sub(block.height)
            .and_then(|depth| depth.checked_add(1))
            .is_some_and(|depth| depth >= MIN_CONFIRMATIONS)
}

// Completion reconciliation must inspect inclusion independently of deployment:
// an expired RDTS window can mask a Bitcoin reorg in the preflight assessment.
fn completion_bitcoin_loss(
    plan: &coincube_core::claim::ClaimPlan,
    bitcoin: coincube_core::claim::BitcoinObservation,
) -> Option<Assessment> {
    use coincube_core::claim::{TransactionLocation, MIN_CONFIRMATIONS};
    if bitcoin.chain != plan.bitcoin_chain {
        return None;
    }
    match bitcoin.location {
        TransactionLocation::Unknown => None,
        TransactionLocation::Unconfirmed => Some(if plan.previous_confirmation.is_some() {
            Assessment::Reorged
        } else {
            Assessment::WaitingForConfirmation
        }),
        TransactionLocation::Confirmed {
            txid,
            block,
            best_chain_hash_at_height,
        } => {
            if txid != plan.step1.compute_txid() {
                return None;
            }
            if block.hash != best_chain_hash_at_height
                || plan
                    .previous_confirmation
                    .is_some_and(|previous| previous != block)
            {
                return Some(Assessment::Reorged);
            }
            let depth = bitcoin
                .tip
                .height
                .checked_sub(block.height)?
                .checked_add(1)?;
            (depth < MIN_CONFIRMATIONS).then_some(Assessment::WaitingForDepth {
                confirmations: depth,
            })
        }
    }
}

impl Coordinator {
    /// Whether tracking must remain read-only pending full ancestry acceptance.
    pub fn is_ancestry(&self) -> bool {
        self.controller.plan().poison == coincube_core::claim::Poison::InputAncestry
    }

    /// Reconcile saved markers after a fresh loss of fork inclusion or Bitcoin
    /// poison depth. Transport failures leave historical data alone and return
    /// an error; a marker belonging to another sweep is never cleared.
    pub async fn reconcile_completion(
        &mut self,
        context: &Context,
        root: &crate::dir::CoincubeDirectory,
    ) -> Result<CompletionReconciliation, Error> {
        let origin = Instant::now();
        let plan = self.controller.plan();
        if plan.poison == coincube_core::claim::Poison::InputAncestry {
            if !self.has_matching_completion_marker(root)? {
                self.current(context)?;
                return Err(Error::Unsupported);
            }
            let source_guard = self
                .services
                .ancestry_source()
                .ok_or(Error::Unsupported)?
                .context_guard();
            source_guard
                .validate(&context.provider, context.generation)
                .map_err(|kind| {
                    Error::Observation(claim_observation::Failure {
                        stage: claim_observation::Stage::Context,
                        kind,
                    })
                })?;
            return match self.checked_sweep(context).await {
                Err(Error::Observation(claim_observation::Failure {
                    kind:
                        kind @ (claim_observation::FailureKind::AncestryRootChanged { observed_at }
                        | claim_observation::FailureKind::AncestryRootShared { observed_at }),
                    ..
                })) => {
                    self.current(context)?;
                    let deadline = observation_deadline(
                        self.policy,
                        observed_at,
                        self.services.source().now(),
                        origin,
                    )?;
                    self.clear_matching_completion_markers(
                        context,
                        root,
                        deadline,
                        Some(&source_guard),
                    )
                    .await?;
                    Ok(CompletionReconciliation::AncestryInvalidated { kind })
                }
                Ok(_) => Err(Error::Unsupported),
                Err(error) => Err(error),
            };
        }
        let (status, checked) = self.checked_sweep(context).await?;
        let bitcoin_loss =
            completion_bitcoin_loss(&plan, checked.assessment().observations.bitcoin);
        let status = bitcoin_loss.map(Status::Observation).unwrap_or(status);
        let transaction = checked.transaction();
        let lost = !matches!(
            transaction,
            claim_observation::TransactionObservation::Confirmed { .. }
        ) || matches!(
            status,
            Status::Observation(
                Assessment::Reorged
                    | Assessment::WaitingForConfirmation
                    | Assessment::WaitingForDepth { .. }
            )
        );
        if !lost {
            return Ok(CompletionReconciliation::Observed {
                status,
                transaction,
            });
        }
        let deadline = evidence_deadline(
            self.policy,
            checked.assessment().observations,
            checked.observed_at(),
            self.services.source().now(),
            origin,
        )?;
        self.clear_matching_completion_markers(context, root, deadline, None)
            .await?;
        Ok(CompletionReconciliation::Observed {
            status,
            transaction,
        })
    }

    fn has_matching_completion_marker(
        &self,
        root: &crate::dir::CoincubeDirectory,
    ) -> Result<bool, Error> {
        use crate::app::settings::{Settings, WalletId, SETTINGS_FILE_NAME};
        let wallet = self.controller.identity().clone();
        let fork = self.construction.chain();
        let bitcoin = self.controller.plan().bitcoin_chain;
        let txid = self.verified.transaction().compute_txid();
        let fingerprint =
            crate::app::wallet::descriptor_id_fingerprint(self.construction.descriptor())
                .to_string();
        let checksum = WalletId::generate(self.construction.descriptor()).descriptor_checksum;
        let pair = [(fork, &wallet.fork_cube), (bitcoin, &wallet.bitcoin_cube)];
        for (chain, id) in pair {
            let directory = root.network_directory(chain);
            if !directory.path().join(SETTINGS_FILE_NAME).exists() {
                continue;
            }
            let mut settings = Settings::from_file(&directory)
                .map_err(|error| Error::CompletionPersistence(error.to_string()))?;
            let marked = settings
                .cubes
                .iter()
                .any(|cube| cube.id == *id && cube.split_completion_txid == Some(txid));
            if !marked {
                continue;
            }
            matching_completion_cube(&mut settings, chain, id, &fingerprint, &checksum)
                .map_err(|error| Error::CompletionPersistence(error.to_string()))?;
            return Ok(true);
        }
        Ok(false)
    }

    async fn clear_matching_completion_markers(
        &mut self,
        context: &Context,
        root: &crate::dir::CoincubeDirectory,
        deadline: Instant,
        source_guard: Option<&claim_observation::http::ObservationContextGuard>,
    ) -> Result<(), Error> {
        use crate::app::settings::{
            update_settings_file_checked, Settings, SettingsError, WalletId,
        };
        let wallet = self.controller.identity().clone();
        let fork = self.construction.chain();
        let bitcoin = self.controller.plan().bitcoin_chain;
        let txid = self.verified.transaction().compute_txid();
        let fingerprint =
            crate::app::wallet::descriptor_id_fingerprint(self.construction.descriptor())
                .to_string();
        let checksum = WalletId::generate(self.construction.descriptor()).descriptor_checksum;
        let pair = [(fork, &wallet.fork_cube), (bitcoin, &wallet.bitcoin_cube)];
        let apply = |settings: &mut Settings, chain, id: &str| -> Result<bool, SettingsError> {
            if let Some(source_guard) = source_guard {
                source_guard
                    .validate(&context.provider, context.generation)
                    .map_err(|_| {
                        SettingsError::Unexpected(
                            "Claim ancestry observation source changed".into(),
                        )
                    })?;
            }
            if self.revoker.is_revoked()
                || self.generation.has_changed().is_err()
                || *self.generation.borrow() != context.generation
                || Instant::now() >= deadline
            {
                return Err(SettingsError::Unexpected(
                    "Claim reorg check expired or changed".into(),
                ));
            }
            let cube = matching_completion_cube(settings, chain, id, &fingerprint, &checksum)?;
            if cube.split_completion_txid == Some(txid) {
                cube.split_completed_at_height = None;
                cube.split_completion_txid = None;
                return Ok(true);
            }
            Ok(false)
        };
        let mut snapshots = Vec::new();
        for (chain, id) in pair {
            let settings = Settings::from_file(&root.network_directory(chain))
                .map_err(|error| Error::CompletionPersistence(error.to_string()))?;
            snapshots.push((chain, id, settings));
        }
        let marked = snapshots.iter().any(|(_, id, settings)| {
            settings
                .cubes
                .iter()
                .any(|cube| cube.id == **id && cube.split_completion_txid == Some(txid))
        });
        if !marked {
            self.current(context)?;
            if Instant::now() >= deadline {
                return Err(Error::ExpiredEvidence);
            }
            return Ok(());
        }
        for (chain, id, mut settings) in snapshots {
            apply(&mut settings, chain, id)
                .map_err(|error| Error::CompletionPersistence(error.to_string()))?;
        }
        #[cfg(test)]
        if let Some((reached, release)) = &self.completion_cleanup_barrier {
            reached.notify_one();
            release.notified().await;
        }
        for (chain, id) in pair {
            update_settings_file_checked(&root.network_directory(chain), |mut settings| {
                Ok(apply(&mut settings, chain, id)?.then_some(settings))
            })
            .await
            .map_err(|error| Error::CompletionPersistence(error.to_string()))?;
        }
        self.current(context)?;
        if Instant::now() >= deadline {
            return Err(Error::ExpiredEvidence);
        }
        Ok(())
    }
}
