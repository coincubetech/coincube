//! Split (#568) step-1 coordination. A Split's source is a foreign wallet's
//! public descriptors, not a Bitcoin Cube, so there is no daemon (D5):
//! [`SplitProduction`] observes through the same Connect origin checks as
//! the Claim [`Production`], preflights through Connect only and submits the
//! exact verified bytes with the daemonless
//! `coincubed::poison_broadcast::submit_verified_split_step1_to_connect`.
//!
//! The coordinator's review, submission, reconcile, resubmission and
//! reconfirmation flows are the Claim ones; only admission and the durable
//! intent record differ. Nothing here signs, and nothing calls it from the
//! GUI yet (D1): B1b adds the Split panel.
use super::*;
use coincube_core::foreign_split::{SplitStep1, VerifiedSplitStep1};

/// The daemonless Connect production for Split step 1. Bitcoin mainnet only:
/// Connect has no Testnet4 route and no other chain carries step 1.
/// Account id comes from the caller's admitted session, never JWT parsing.
pub struct SplitProduction {
    source: HttpObservationSource,
    preflight: PreflightClient,
    connect_origin: String,
    context: Context,
    generation: watch::Receiver<u64>,
}
impl SplitProduction {
    /// Refused before anything is journaled: any chain other than Bitcoin
    /// mainnet, an empty account, or an origin that is not exactly
    /// `scheme://host[:port]/` (the checks of [`Production`]'s constructor).
    pub fn new(
        client: CoincubeClient,
        account: String,
        expected_generation: u64,
        generation: watch::Receiver<u64>,
        chain: ChainId,
    ) -> Result<Self, Error> {
        if chain != ChainId::Bitcoin {
            return Err(Error::Unsupported);
        }
        let origin = reqwest::Url::parse(&client.base_url).map_err(|_| Error::InvalidBinding)?;
        if origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || !origin.username().is_empty()
            || origin.password().is_some()
        {
            return Err(Error::InvalidBinding);
        }
        if account.is_empty() {
            return Err(Error::Unsupported);
        }
        let cc = || CollectionContext {
            expected_generation,
            generation: generation.clone(),
        };
        let source =
            HttpObservationSource::new(client, ChainId::Bitcoin, ChainId::BitcoinBlake2b, cc())
                .map_err(|_| Error::InvalidBinding)?;
        let context = Context {
            generation: expected_generation,
            account,
            provider: source.provider_identity(),
        };
        let preflight = PreflightClient::new(origin.as_str(), cc()).map_err(Error::Preflight)?;
        Ok(Self {
            source,
            preflight,
            connect_origin: origin.as_str().to_owned(),
            context,
            generation,
        })
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
    /// The admitted observation source, context and generation, for the
    /// Split step-2 confirmation check (`fork::split`, #568 B2), which
    /// observes through exactly the same Connect origin checks.
    pub(super) fn into_observation(self) -> (HttpObservationSource, Context, watch::Receiver<u64>) {
        (self.source, self.context, self.generation)
    }
}
#[async_trait]
impl Services for SplitProduction {
    fn source(&self) -> &dyn ObservationSource {
        &self.source
    }
    async fn preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<Evidence, claim_preflight::Error> {
        self.preflight
            .observe(ChainId::Bitcoin, tx, tip, policy)
            .await
    }
    /// Connect only: no node is bound, so no local route is ever selected.
    async fn routed_preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<RoutedEvidence, claim_preflight::Error> {
        self.preflight(tx, tip, policy)
            .await
            .map(RoutedEvidence::Connect)
    }
    async fn submit_route(
        &self,
        route: SubmissionRoute,
        tx: VerifiedStep1,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        if route != SubmissionRoute::Connect {
            return Err(DaemonError::ClientNotSupported);
        }
        self.submit(tx, gate).await
    }
    async fn submit(
        &self,
        tx: VerifiedStep1,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        let VerifiedStep1::Split(verified) = tx else {
            return Err(DaemonError::ClientNotSupported);
        };
        let txid = verified.transaction().compute_txid();
        let wtxid = verified.transaction().compute_wtxid();
        let origin = self.connect_origin.clone();
        // Blocking HTTP stays off the executor. Dropping this future does not
        // stop a started worker: the coordinator revokes the gate first and
        // keeps the recorded intent uncertain after Started.
        tokio::task::spawn_blocking(move || {
            coincubed::poison_broadcast::submit_verified_split_step1_to_connect(
                &verified, &gate, &origin,
            )
        })
        .await
        .map_err(|_| {
            DaemonError::PoisonSubmission(coincubed::poison_broadcast::SubmissionError::Uncertain {
                txid,
                wtxid,
            })
        })?
        .map_err(DaemonError::PoisonSubmission)
    }
}

/// Step 1 with every scriptSig and witness removed: the construction a
/// verified step 1 must sign.
fn unsigned(tx: &Transaction) -> Transaction {
    let mut unsigned = tx.clone();
    for input in &mut unsigned.input {
        input.script_sig = Default::default();
        input.witness.clear();
    }
    unsigned
}

impl Coordinator {
    /// Record a new Split step 1 and admit it for review. `construction` is
    /// the opaque core builder output, `verified` its finalization and
    /// `fork_height` the authenticated anchor's fork height it was built
    /// with. Explicit signing consent belongs to the caller; this never signs.
    #[allow(clippy::too_many_arguments)]
    pub fn create_split(
        directory: &Path,
        target_cube: String,
        construction: &SplitStep1,
        verified: VerifiedSplitStep1,
        fork_height: u64,
        production: SplitProduction,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        let context = production.context.clone();
        let generation = production.generation.clone();
        Self::open_split(
            directory,
            target_cube,
            construction,
            verified,
            fork_height,
            context,
            generation,
            Box::new(production),
            policy,
            false,
        )
    }
    /// Resume is Unchecked: the rebuilt construction and signed bytes must
    /// match the journal exactly, and a recorded uncertain submission can
    /// only be reconciled, never retried through prepare_review. A journal
    /// with a recorded step-2 submission refuses (`SubmissionAlreadyRecorded`):
    /// step 1 is then closed for good (#568 D13 = A), and only the step-2
    /// reconciler opens it.
    #[allow(clippy::too_many_arguments)]
    pub fn resume_split(
        directory: &Path,
        target_cube: String,
        construction: &SplitStep1,
        verified: VerifiedSplitStep1,
        fork_height: u64,
        production: SplitProduction,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        let context = production.context.clone();
        let generation = production.generation.clone();
        Self::open_split(
            directory,
            target_cube,
            construction,
            verified,
            fork_height,
            context,
            generation,
            Box::new(production),
            policy,
            true,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_split(
        directory: &Path,
        target_cube: String,
        construction: &SplitStep1,
        verified: VerifiedSplitStep1,
        fork_height: u64,
        context: Context,
        generation: watch::Receiver<u64>,
        services: Box<dyn Services>,
        policy: CheckPolicy,
        resume: bool,
    ) -> Result<Self, Error> {
        if !policy.valid() || construction.chain() != ChainId::Bitcoin {
            return Err(Error::Unsupported);
        }
        if verified.chain() != construction.chain()
            || verified.construction_txid() != construction.txid()
            || unsigned(verified.transaction()) != construction.psbt().unsigned_tx
            || *generation.borrow() != context.generation
            || generation.has_changed().is_err()
        {
            return Err(Error::InvalidBinding);
        }
        let mut controller = if resume {
            // The identity binds the source digest: another source's journal
            // (or none) refuses here.
            let identity =
                claim_workflow::split_identity(target_cube, construction.source().digest());
            let controller =
                Controller::reopen_settling_blocking(directory, &identity, context.clone())?;
            // After step 2 was submitted, step 1 is never reviewed, resent or
            // reconfirmed here again (#568 S4): the claimed coins may already
            // be spent on BTCB2, and its reorg recovery is the step-2
            // reconciler's. The step-2 preparation's open refuses the same
            // way.
            if controller.recorded_fork_submission().is_some() {
                return Err(Error::SubmissionAlreadyRecorded);
            }
            controller
        } else {
            Controller::create_split(
                directory,
                target_cube,
                construction,
                &verified,
                fork_height,
                context.clone(),
            )?
        };
        controller.revalidate_split_construction(&context, construction, fork_height)?;
        controller.bind_recovered_split_transaction(&context, &verified)?;
        let id = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| Error::Revoked)?;
        Ok(Self {
            id,
            revision: 0,
            context,
            generation,
            controller,
            verified: VerifiedStep1::Split(Arc::new(verified)),
            services,
            policy,
            revoker: Revoker::new(),
        })
    }
    /// The Split counterpart of the Claim durable intent in
    /// `confirm_and_submit`, after the same fresh review.
    pub(super) fn record_split_intent(
        &mut self,
        context: &Context,
        verified: &VerifiedSplitStep1,
    ) -> Result<(), Error> {
        self.controller.record_split_broadcast_intent(
            context,
            verified,
            self.policy.observations,
            self.services.source().now(),
        )?;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests;
