//! Owned Claim coordination. Signing keys and explicit user consent stay with callers.
mod ancestry;
mod collection;
use collection::Collected;
mod route;
use route::RoutedEvidence;
pub use route::{NodeIdentity, SubmissionRoute};
mod step1;
use step1::VerifiedStep1;
pub mod fork;
mod recovery;
mod reorg;
pub mod split;
use super::{
    claim_observation::{
        self, http::HttpObservationSource, CollectionContext, ObservationBundle, ObservationSource,
    },
    claim_preflight::{self, Evidence, FreshnessPolicy, NodePolicy, PreflightClient},
    claim_workflow::{self, Context, Controller, Phase, Status, WalletIdentity},
    coincube::CoincubeClient,
};
use crate::daemon::{Daemon, DaemonError};
use async_trait::async_trait;
use coincube_core::{
    chain::ChainId,
    claim::{Assessment, Policy},
    claim_finalize::VerifiedPoisonTransfer,
    claim_spend::PoisonSelfTransfer,
    descriptors::PathInfo,
    miniscript::bitcoin::{
        hashes::{sha256, Hash},
        BlockHash, Transaction, Txid, Wtxid,
    },
};
use coincubed::poison_broadcast::{SubmissionGate, SubmissionOutcome, SubmissionRevoker};
pub use recovery::ResubmissionReview;
pub use reorg::ReconfirmationReview;
use std::{
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// Claim submission uses a bound Connect transport even when ordinary wallet
/// reads have public fallbacks. Admit only the exact default provider layout
/// (or the legacy single anonymous endpoint), with this session's JWT.
pub(crate) fn admitted_connect_esplora(
    selection: &coincubed::config::EsploraConfig,
    fallback: Option<&coincubed::config::EsploraConfig>,
    endpoint: &str,
    chain: ChainId,
    session_token: Option<&str>,
) -> bool {
    if selection.addr.trim_end_matches('/') != endpoint || fallback.is_some() {
        return false;
    }
    let anonymous = selection.token.is_none()
        && selection.fallback_addr.is_none()
        && selection.fallback_token.is_none()
        && selection.secondary_fallback_addr.is_none()
        && selection.secondary_fallback_token.is_none();
    if anonymous {
        return true;
    }
    if selection.token.as_deref() != session_token || session_token.is_none() {
        return false;
    }
    if chain == ChainId::BitcoinBlake2b {
        return selection.fallback_addr.is_none()
            && selection.fallback_token.is_none()
            && selection.secondary_fallback_addr.is_none()
            && selection.secondary_fallback_token.is_none();
    }
    chain == ChainId::Bitcoin
        && selection.fallback_addr.as_deref()
            == Some(
                crate::installer::public_esplora_url(
                    coincube_core::miniscript::bitcoin::Network::Bitcoin,
                )
                .as_str(),
            )
        && selection.fallback_token.is_none()
        && selection.secondary_fallback_addr.as_deref()
            == crate::installer::public_esplora_fallback_url(
                coincube_core::miniscript::bitcoin::Network::Bitcoin,
            )
            .as_deref()
        && selection.secondary_fallback_token.is_none()
}

#[derive(Debug)]
pub enum Error {
    Unsupported,
    InvalidBinding,
    /// The signed Claim fork sweep retains legacy signatures that could satisfy
    /// an input without its unified one (`#582`, `#607`). Nothing was finalized.
    UnsafeLegacyAlternative,
    Revoked,
    InvalidReview,
    ChangedReview,
    Journal(claim_workflow::Error),
    Observation(claim_observation::Failure),
    Preflight(claim_preflight::Error),
    PolicyRejected(NodePolicy),
    NotReady(Assessment),
    SubmissionAlreadyRecorded,
    ExpiredEvidence,
    CompletionPersistence(String),
}
fn recovery_check_error(error: claim_workflow::Error, assessment: Assessment) -> Error {
    match error {
        claim_workflow::Error::Unchecked => Error::NotReady(assessment),
        other => Error::Journal(other),
    }
}

impl From<claim_workflow::Error> for Error {
    fn from(e: claim_workflow::Error) -> Self {
        Self::Journal(e)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CheckPolicy {
    pub observations: Policy,
    pub preflight: FreshnessPolicy,
    pub collection_budget: Duration,
}
impl CheckPolicy {
    fn valid(self) -> bool {
        self.observations.max_observation_age_seconds > 0
            && self.observations.expiry_margin_seconds > 0
            && self.preflight.max_age_seconds > 0
            && (1..=claim_preflight::MAX_FUTURE_SKEW_SECONDS)
                .contains(&self.preflight.max_future_skew_seconds)
            && !self.collection_budget.is_zero()
            && self.collection_budget <= claim_observation::MAX_COLLECTION_TIME
    }
}
/// Information a future confirmation screen must display. Never a permission token.
#[derive(Debug, Clone)]
pub struct ReviewSnapshot {
    pub transaction: Transaction,
    pub wallet: WalletIdentity,
    pub txid: Txid,
    pub wtxid: Wtxid,
    pub fee_sats: u64,
    pub vsize: usize,
    pub observations: ObservationBundle,
    pub route: SubmissionRoute,
    not_after: Instant,
}
/// One-use review identity. No Clone, deserialization or public field construction.
/// Calling confirm_and_submit must correspond to explicit user confirmation of this view.
pub struct Review {
    coordinator: u64,
    revision: u64,
    snapshot: ReviewSnapshot,
}
impl Review {
    pub fn snapshot(&self) -> &ReviewSnapshot {
        &self.snapshot
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A journal records submission, but no witness transaction was recovered yet.
    Recorded { txid: Txid },
    /// An upstream acknowledgement, not confirmation or chain exclusivity.
    UpstreamAccepted { txid: Txid, wtxid: Wtxid },
    /// Intent was durably recorded. Never automatically retry, even if cancellation
    /// probably preceded the network call. Reconcile this exact transaction.
    Uncertain { txid: Txid, wtxid: Wtxid },
}

// Injection is private to this module/tests; production cannot replace checked
// evidence with an arbitrary boolean or a public fake authorization adapter.
#[async_trait]
trait Services: Send + Sync {
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
    ) -> Result<SubmissionOutcome, DaemonError>;
}

/// Immutable admitted session plus the exact Connect endpoint.
/// Account id comes from the caller's admitted session, never unverified JWT parsing.
pub struct Production {
    source: HttpObservationSource,
    preflight: PreflightClient,
    /// The trait object, not `Arc<EmbeddedDaemon>`: every call this type makes
    /// — `config()` and `submit_verified_poison()` — is a `Daemon` trait
    /// method, and the GUI holds its daemon as `Arc<dyn Daemon>`. The concrete
    /// type used to be what encoded "embedded only"; [`Production::new`] now
    /// says so explicitly, and says it *before* anything is journaled.
    daemon: Arc<dyn Daemon + Send + Sync>,
    bound_node: Option<route::BoundNode>,
    connect_origin: String,
    /// Claim sessions use the dedicated bound transports.
    bound_transport: bool,
    backend_binding: std::sync::OnceLock<coincubed::poison_broadcast::ClaimBackendBinding>,
    context: Context,
    generation: watch::Receiver<u64>,
}
impl Production {
    pub fn new(
        client: CoincubeClient,
        daemon: Arc<dyn Daemon + Send + Sync>,
        account: String,
        expected_generation: u64,
        generation: watch::Receiver<u64>,
    ) -> Result<Self, Error> {
        Self::for_chain(
            client,
            daemon,
            account,
            expected_generation,
            generation,
            ChainId::Bitcoin,
        )
    }
    fn for_chain(
        client: CoincubeClient,
        daemon: Arc<dyn Daemon + Send + Sync>,
        account: String,
        expected_generation: u64,
        generation: watch::Receiver<u64>,
        chain: ChainId,
    ) -> Result<Self, Error> {
        let route = match chain {
            ChainId::Bitcoin => "bitcoin/mainnet",
            ChainId::BitcoinBlake2b => "bitcoin-blake2b/mainnet",
            _ => return Err(Error::Unsupported),
        };
        // Refused here, before any journal write, and not left to fail at the
        // submit call. `confirm_and_submit` records the broadcast intent
        // *before* it submits, so a backend that answers `config()` but cannot
        // carry the verified artifact would journal a durable intent and then
        // take `ClientNotSupported` — an `Uncertain` outcome that can never be
        // retried, for a case that should never have been admitted.
        if !daemon.backend().is_embedded() {
            return Err(Error::Unsupported);
        }
        let config = daemon.config().ok_or(Error::Unsupported)?;
        let origin = reqwest::Url::parse(&client.base_url).map_err(|_| Error::InvalidBinding)?;
        if origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || !origin.username().is_empty()
            || origin.password().is_some()
        {
            return Err(Error::InvalidBinding);
        }
        let endpoint = format!(
            "{}/api/v1/esplora/{}",
            origin.as_str().trim_end_matches('/'),
            route
        );
        if config.bitcoin_config.chain != chain
            || config.bitcoin_config.network != coincube_core::miniscript::bitcoin::Network::Bitcoin
            || account.is_empty()
        {
            return Err(Error::Unsupported);
        }
        let bound_transport = match config.bitcoin_backend.as_ref() {
            Some(coincubed::config::BitcoinBackend::Esplora(selection)) => {
                if !admitted_connect_esplora(
                    selection,
                    config.fallback_esplora.as_ref(),
                    &endpoint,
                    chain,
                    client.token(),
                ) {
                    return Err(Error::Unsupported);
                }
                true
            }
            Some(
                coincubed::config::BitcoinBackend::Bitcoind(_)
                | coincubed::config::BitcoinBackend::Electrum(_),
            ) if chain == ChainId::Bitcoin => true,
            _ => return Err(Error::Unsupported),
        };
        let cc = || CollectionContext {
            expected_generation,
            generation: generation.clone(),
        };
        let source =
            HttpObservationSource::new(client, ChainId::Bitcoin, ChainId::BitcoinBlake2b, cc())
                .map_err(|_| Error::InvalidBinding)?;
        // Only exact chain+endpoint selection enters identity, never Debug/config
        // serialization, bearer tokens, RPC credentials or device metadata.
        let context = Context {
            generation: expected_generation,
            account,
            // The persisted Claim identity binds the pair through its Bitcoin
            // endpoint. Fork transport independently admits the fixed sibling
            // endpoint at this same origin; changing origins cannot reopen it.
            provider: source.provider_identity(),
        };
        let preflight = PreflightClient::new(origin.as_str(), cc()).map_err(Error::Preflight)?;
        let bound_node = match &config.bitcoin_backend {
            Some(coincubed::config::BitcoinBackend::Bitcoind(node)) => {
                Some(route::BoundNode::new(node.clone()))
            }
            _ => None,
        };
        Ok(Self {
            source,
            preflight,
            bound_node,
            connect_origin: origin.as_str().to_owned(),
            bound_transport,
            backend_binding: std::sync::OnceLock::new(),
            daemon,
            context,
            generation,
        })
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
}
#[async_trait]
impl Services for Production {
    fn ancestry_source(&self) -> Option<&HttpObservationSource> {
        Some(&self.source)
    }
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
    async fn routed_preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<RoutedEvidence, claim_preflight::Error> {
        if self.bound_transport {
            let current = self
                .daemon
                .claim_backend_binding()
                .await
                .map_err(|_| claim_preflight::Error::BackendChanged)?;
            if self.backend_binding.get_or_init(|| current.clone()) != &current {
                return Err(claim_preflight::Error::BackendChanged);
            }
        }
        let local = match (
            &self.bound_node,
            self.daemon
                .config()
                .and_then(|c| c.bitcoin_backend.as_ref()),
        ) {
            (Some(bound), Some(coincubed::config::BitcoinBackend::Bitcoind(node)))
                if bound.matches(node) =>
            {
                Some(bound.clone())
            }
            (
                None,
                Some(
                    coincubed::config::BitcoinBackend::Esplora(_)
                    | coincubed::config::BitcoinBackend::Electrum(_),
                ),
            ) => None,
            _ => return Err(claim_preflight::Error::BackendChanged),
        };
        route::preflight(
            local,
            &self.preflight,
            tx,
            tip,
            policy,
            CollectionContext {
                expected_generation: self.context.generation,
                generation: self.generation.clone(),
            },
        )
        .await
    }
    async fn submit_route(
        &self,
        route: SubmissionRoute,
        tx: VerifiedStep1,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        if route == SubmissionRoute::Connect {
            if !self.bound_transport {
                return self.submit(tx, gate).await;
            }
            let binding = self
                .backend_binding
                .get()
                .ok_or(DaemonError::ClientNotSupported)?
                .clone();
            return match tx {
                VerifiedStep1::OpReturn(tx) => {
                    self.daemon
                        .submit_verified_poison_to_connect(
                            tx,
                            self.connect_origin.clone(),
                            binding,
                            gate,
                        )
                        .await
                }
                VerifiedStep1::Ancestry(tx) => {
                    self.daemon
                        .submit_verified_ancestry_to_connect(
                            tx,
                            self.connect_origin.clone(),
                            binding,
                            gate,
                        )
                        .await
                }
                // A Split has no daemon route; only the Split production sends it.
                VerifiedStep1::Split(_) => Err(DaemonError::ClientNotSupported),
            };
        }
        let node = self
            .bound_node
            .as_ref()
            .ok_or(DaemonError::ClientNotSupported)?;
        if node.route() != route {
            return Err(DaemonError::ClientNotSupported);
        }
        let binding = self
            .backend_binding
            .get()
            .ok_or(DaemonError::ClientNotSupported)?
            .clone();
        match tx {
            VerifiedStep1::OpReturn(tx) => {
                self.daemon
                    .submit_verified_poison_to_node(tx, binding, gate)
                    .await
            }
            VerifiedStep1::Ancestry(tx) => {
                self.daemon
                    .submit_verified_ancestry_to_node(tx, binding, gate)
                    .await
            }
            VerifiedStep1::Split(_) => Err(DaemonError::ClientNotSupported),
        }
    }
    async fn submit(
        &self,
        tx: VerifiedStep1,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        // Node/Electrum sessions must carry the selected route and frozen
        // backend binding through submit_route, never the generic retrying path.
        if self.bound_transport {
            return Err(DaemonError::ClientNotSupported);
        }
        // The transport adapter owns its blocking worker and checks the revocable
        // gate under the actual backend lock immediately before submission.
        match tx {
            VerifiedStep1::OpReturn(tx) => self.daemon.submit_verified_poison(tx, gate).await,
            VerifiedStep1::Ancestry(tx) => self.daemon.submit_verified_ancestry(tx, gate).await,
            VerifiedStep1::Split(_) => Err(DaemonError::ClientNotSupported),
        }
    }
}

/// Producer-side cancellation handle. Logout/provider/Cube changes must call
/// revoke synchronously before replacing their context, then advance generation.
/// Watch notification alone is not a synchronous queued-submission barrier.
#[derive(Clone)]
pub struct Revoker(Arc<Mutex<Revocation>>);
struct Revocation {
    revoked: bool,
    gate: Option<SubmissionRevoker>,
}
impl Revoker {
    pub fn revoke(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.revoked = true;
        if let Some(gate) = &state.gate {
            gate.revoke();
        }
    }
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Revocation {
            revoked: false,
            gate: None,
        })))
    }
    pub(crate) fn is_revoked(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).revoked
    }
    fn register(&self, revoker: SubmissionRevoker) -> Result<(), Error> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.revoked {
            revoker.revoke();
            return Err(Error::Revoked);
        }
        state.gate = Some(revoker);
        Ok(())
    }
}

/// Step one supports native P2WSH primary paths, including k-of-n multisig.
/// The panel and coordinator share this admission rule; Taproot remains unsupported.
pub fn admits_descriptor(descriptor: &coincube_core::descriptors::CoincubeDescriptor) -> bool {
    !descriptor.is_taproot()
}

fn primary_threshold(descriptor: &coincube_core::descriptors::CoincubeDescriptor) -> usize {
    match descriptor.policy().primary_path() {
        PathInfo::Single(_) => 1,
        PathInfo::Multi(threshold, _) => *threshold,
    }
}

static NEXT: AtomicU64 = AtomicU64::new(1);
pub struct Coordinator {
    id: u64,
    revision: u64,
    context: Context,
    generation: watch::Receiver<u64>,
    controller: Controller,
    verified: VerifiedStep1,
    services: Box<dyn Services>,
    policy: CheckPolicy,
    revoker: Revoker,
}
/// Revokes a queued transport even when the coordinator future is dropped.
struct PendingGate(SubmissionRevoker);
impl Drop for PendingGate {
    fn drop(&mut self) {
        self.0.revoke();
    }
}

impl Coordinator {
    /// Builder creation/change reservation and explicit signing consent belong to
    /// the caller. This constructor receives already verified signatures; it never signs.
    pub fn create(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        construction: &PoisonSelfTransfer,
        verified: VerifiedPoisonTransfer,
        production: Production,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        if production
            .daemon
            .config()
            .is_none_or(|c| c.main_descriptor != *construction.descriptor())
        {
            return Err(Error::InvalidBinding);
        }
        let context = production.context.clone();
        let generation = production.generation.clone();
        Self::open(
            directory,
            bitcoin_cube,
            fork_cube,
            construction,
            verified,
            context,
            generation,
            Box::new(production),
            policy,
            false,
        )
    }
    /// Resume is Unchecked. A recorded uncertain attempt can only be reconciled,
    /// never retried through prepare_review/confirm_and_submit.
    pub fn resume(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        construction: &PoisonSelfTransfer,
        verified: VerifiedPoisonTransfer,
        production: Production,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        if production
            .daemon
            .config()
            .is_none_or(|c| c.main_descriptor != *construction.descriptor())
        {
            return Err(Error::InvalidBinding);
        }
        let context = production.context.clone();
        let generation = production.generation.clone();
        Self::open(
            directory,
            bitcoin_cube,
            fork_cube,
            construction,
            verified,
            context,
            generation,
            Box::new(production),
            policy,
            true,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn open(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        construction: &PoisonSelfTransfer,
        verified: VerifiedPoisonTransfer,
        context: Context,
        generation: watch::Receiver<u64>,
        services: Box<dyn Services>,
        policy: CheckPolicy,
        resume: bool,
    ) -> Result<Self, Error> {
        if !policy.valid()
            || construction.chain() != ChainId::Bitcoin
            || !admits_descriptor(construction.descriptor())
            || construction
                .psbt()
                .unsigned_tx
                .input
                .iter()
                .any(|i| i.sequence.is_relative_lock_time())
            || verified
                .signatures_per_input()
                .iter()
                .any(|n| *n != primary_threshold(construction.descriptor()))
        {
            return Err(Error::Unsupported);
        }
        let mut unsigned = verified.transaction().clone();
        for input in &mut unsigned.input {
            input.witness.clear();
        }
        if verified.chain() != construction.chain()
            || verified.descriptor() != construction.descriptor()
            || unsigned != construction.psbt().unsigned_tx
            || verified.construction_txid() != construction.psbt().unsigned_tx.compute_txid()
            || *generation.borrow() != context.generation
            || generation.has_changed().is_err()
        {
            return Err(Error::InvalidBinding);
        }
        let identity = WalletIdentity {
            bitcoin_cube,
            fork_cube,
            descriptor_digest: sha256::Hash::hash(construction.descriptor().to_string().as_bytes()),
        };
        let mut controller = if resume {
            Controller::reopen_settling_blocking(directory, &identity, context.clone())?
        } else {
            Controller::create(
                directory,
                identity.bitcoin_cube,
                identity.fork_cube,
                construction,
                context.clone(),
            )?
        };
        controller.revalidate_construction(&context, construction)?;
        controller.bind_recovered_bitcoin_transaction(&context, &verified)?;
        let id = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| Error::Revoked)?;
        Ok(Self {
            id,
            revision: 0,
            context,
            generation,
            controller,
            verified: VerifiedStep1::OpReturn(Arc::new(verified)),
            services,
            policy,
            revoker: Revoker::new(),
        })
    }
    pub fn phase(&self) -> Phase {
        self.controller.phase()
    }
    pub fn recorded_outcome(&self) -> Option<Outcome> {
        (self.phase() != Phase::Intent).then(|| Outcome::Uncertain {
            txid: self.verified.transaction().compute_txid(),
            wtxid: self.verified.transaction().compute_wtxid(),
        })
    }
    pub(crate) fn wallet_identity(&self) -> &WalletIdentity {
        self.controller.identity()
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
    fn collection_context(&self) -> CollectionContext {
        CollectionContext {
            expected_generation: self.context.generation,
            generation: self.generation.clone(),
        }
    }
    async fn collect(&self) -> Result<Collected, Error> {
        if matches!(self.verified, VerifiedStep1::Ancestry(_)) {
            let path = self
                .controller
                .recorded_ancestry()?
                .ok_or(Error::InvalidBinding)?;
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
            self.collection_context(),
        )
        .await
        .map(Collected::ordinary)
        .map_err(Error::Observation)
    }
    fn fresh_evidence(&self, evidence: &RoutedEvidence, tip: BlockHash) -> Result<(), Error> {
        let tx = self.verified.transaction();
        let age = self
            .services
            .source()
            .now()
            .checked_sub(evidence.observed_at())
            .ok_or(Error::Preflight(claim_preflight::Error::Stale))?;
        if evidence.chain() != ChainId::Bitcoin
            || evidence.txid() != tx.compute_txid()
            || evidence.wtxid() != tx.compute_wtxid()
            || evidence.tip() != tip
            || evidence.generation() != self.context.generation
        {
            return Err(Error::InvalidBinding);
        }
        if self.services.source().now() < 0
            || evidence.observed_at() < 0
            || age < -self.policy.preflight.max_future_skew_seconds
            || age > self.policy.preflight.max_age_seconds
        {
            return Err(Error::Preflight(claim_preflight::Error::Stale));
        }
        if evidence.node_policy() != &NodePolicy::Accepted {
            return Err(Error::PolicyRejected(evidence.node_policy().clone()));
        }
        Ok(())
    }
    async fn fresh_snapshot(&mut self, context: &Context) -> Result<ReviewSnapshot, Error> {
        self.current(context)?;
        if self.controller.phase() != Phase::Intent {
            return Err(Error::SubmissionAlreadyRecorded);
        }
        let ticket = self.controller.begin_check(context)?;
        let first = self.collect().await?;
        if first.assessment != Assessment::WaitingForConfirmation {
            return Err(Error::NotReady(first.assessment));
        }
        let evidence = self
            .services
            .routed_preflight(
                self.verified.transaction(),
                first.observations.bitcoin.tip.hash,
                self.policy.preflight,
            )
            .await
            .map_err(Error::Preflight)?;
        let last = self.collect().await?;
        self.current(context)?;
        if !same_view(first.observations, last.observations) {
            return Err(Error::ChangedReview);
        }
        self.fresh_evidence(&evidence, last.observations.bitcoin.tip.hash)?;
        let last_data = last.data;
        let status = last.apply(
            &mut self.controller,
            ticket,
            context,
            self.policy.observations,
            self.services.source().now(),
        )?;
        if status != Status::Observation(Assessment::WaitingForConfirmation) {
            return Err(Error::NotReady(last_data.assessment));
        }
        // Capture the monotonic origin before reading wall time or persisting intent.
        // Slow durable writes and backend queues consume this same remaining budget.
        let origin = Instant::now();
        let not_after = evidence_deadline(
            self.policy,
            last_data.observations,
            evidence.observed_at(),
            self.services.source().now(),
            origin,
        )?;
        Ok(ReviewSnapshot {
            transaction: self.verified.transaction().clone(),
            wallet: self.controller.identity().clone(),
            txid: self.verified.transaction().compute_txid(),
            wtxid: self.verified.transaction().compute_wtxid(),
            fee_sats: self.verified.fee().to_sat(),
            vsize: self.verified.vsize(),
            observations: last_data.observations,
            route: evidence.route(),
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
    pub async fn confirm_and_submit(
        &mut self,
        review: Review,
        context: &Context,
    ) -> Result<Outcome, Error> {
        self.current(context)?;
        if review.coordinator != self.id || review.revision != self.revision {
            return Err(Error::InvalidReview);
        }
        // Consume before any await. Cancellation/errors cannot reuse this confirmation.
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        let refreshed = self.fresh_snapshot(context).await?;
        if review.snapshot.wallet != refreshed.wallet
            || review.snapshot.txid != refreshed.txid
            || review.snapshot.wtxid != refreshed.wtxid
            || review.snapshot.route != refreshed.route
            || !same_view(review.snapshot.observations, refreshed.observations)
        {
            return Err(Error::ChangedReview);
        }
        self.current(context)?;
        if Instant::now() >= refreshed.not_after {
            return Err(Error::ExpiredEvidence);
        }
        if let VerifiedStep1::Split(verified) = &self.verified {
            // The Claim journal method refuses a Split intent (#568).
            let verified = verified.clone();
            self.record_split_intent(context, &verified)?;
        } else {
            self.controller.record_broadcast_intent(
                context,
                self.verified.transaction(),
                self.policy.observations,
                self.services.source().now(),
            )?;
        }
        self.submit_recorded(context, refreshed).await
    }
    // Both initial submission and an explicitly reviewed resend arrive here only
    // after their durable attempt record has been written.
    async fn submit_recorded(
        &mut self,
        context: &Context,
        refreshed: ReviewSnapshot,
    ) -> Result<Outcome, Error> {
        let uncertain = Outcome::Uncertain {
            txid: refreshed.txid,
            wtxid: refreshed.wtxid,
        };
        let (gate, revoker) = self.verified.gate(refreshed.not_after);
        let _pending = PendingGate(revoker.clone());
        if self.revoker.register(revoker).is_err() {
            return Ok(uncertain);
        }
        if self.current(context).is_err() {
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
        // This bounds waiting for an answer; timeout means Uncertain. Evidence
        // freshness is enforced by SubmissionGate::enter under the backend lock,
        // before transport starts, not by this coordinator wait bound.
        let result = tokio::select! { biased;
            _ = cancelled => None,
            result = tokio::time::timeout(Duration::from_secs(30), self.services.submit_route(refreshed.route, self.verified.clone(), Arc::new(gate))) => result.ok(),
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
    /// Fresh tracking only; does not restore a review token or retry a submission.
    pub async fn reconcile(&mut self, context: &Context) -> Result<Status, Error> {
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
/// UNIX-second assertions have up to one second of quantization uncertainty.
/// Subtract that second; future timestamps within skew never extend the budget.
fn evidence_deadline(
    policy: CheckPolicy,
    bundle: ObservationBundle,
    preflight_at: i64,
    now: i64,
    origin: Instant,
) -> Result<Instant, Error> {
    if now < 0 {
        return Err(Error::ExpiredEvidence);
    }
    let mut remaining = policy.collection_budget.min(Duration::from_secs(30));
    for (stamp, max_age, skew) in [
        (
            bundle.bitcoin.observed_at,
            policy.observations.max_observation_age_seconds,
            0,
        ),
        (
            bundle.fork.observed_at,
            policy.observations.max_observation_age_seconds,
            0,
        ),
        (
            bundle.deployment.observed_at,
            policy.observations.max_observation_age_seconds,
            0,
        ),
        (
            preflight_at,
            policy.preflight.max_age_seconds,
            policy.preflight.max_future_skew_seconds,
        ),
    ] {
        let age = now.checked_sub(stamp).ok_or(Error::ExpiredEvidence)?;
        if stamp < 0 || age < -skew {
            return Err(Error::ExpiredEvidence);
        }
        let seconds = max_age
            .checked_sub(age.max(0))
            .and_then(|v| v.checked_sub(1))
            .filter(|v| *v > 0)
            .ok_or(Error::ExpiredEvidence)?;
        remaining = remaining.min(Duration::from_secs(seconds as u64));
    }
    if remaining.is_zero() {
        return Err(Error::ExpiredEvidence);
    }
    origin.checked_add(remaining).ok_or(Error::ExpiredEvidence)
}

/// Bind a stable negative ancestry observation to the same short, monotonic
/// lifetime used by positive completion evidence. The observation's original
/// timestamp determines the remaining lifetime; reaching the coordinator does
/// not grant it a fresh collection window.
fn observation_deadline(
    policy: CheckPolicy,
    observed_at: i64,
    now: i64,
    origin: Instant,
) -> Result<Instant, Error> {
    if now < 0 || observed_at < 0 {
        return Err(Error::ExpiredEvidence);
    }
    let age = now
        .checked_sub(observed_at)
        .filter(|age| *age >= 0)
        .ok_or(Error::ExpiredEvidence)?;
    let seconds = policy
        .observations
        .max_observation_age_seconds
        .checked_sub(age)
        .and_then(|remaining| remaining.checked_sub(1))
        .filter(|remaining| *remaining > 0)
        .ok_or(Error::ExpiredEvidence)?;
    let remaining = policy
        .collection_budget
        .min(Duration::from_secs(30))
        .min(Duration::from_secs(seconds as u64));
    if remaining.is_zero() {
        return Err(Error::ExpiredEvidence);
    }
    origin.checked_add(remaining).ok_or(Error::ExpiredEvidence)
}

fn same_view(mut a: ObservationBundle, mut b: ObservationBundle) -> bool {
    a.bitcoin.observed_at = 0;
    a.fork.observed_at = 0;
    a.deployment.observed_at = 0;
    b.bitcoin.observed_at = 0;
    b.fork.observed_at = 0;
    b.deployment.observed_at = 0;
    a == b
}
#[cfg(all(test, unix))]
mod tests;
