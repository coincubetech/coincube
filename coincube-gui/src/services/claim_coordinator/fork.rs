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
    construction: ClaimForkSweep,
    verified: Arc<VerifiedClaimForkSweep>,
    services: Box<dyn ForkServices>,
    policy: CheckPolicy,
    revoker: Revoker,
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
            construction,
            verified: Arc::new(verified),
            services,
            policy,
            revoker: Revoker::new(),
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
