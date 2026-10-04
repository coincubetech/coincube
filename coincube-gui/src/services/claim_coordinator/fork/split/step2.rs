//! Split (#568 B3b) step 2: reserve and prove the target, build the BTCB2
//! sweep under a redeemed [`ForeignStep2Authorization`], finalize the signed
//! PSBT and submit it through a checked route.
//!
//! The flow, all on the [`SplitPreparation`] that owns the Split journal:
//!
//! 1. **Target.** Once a check has tracked step 1's inclusion,
//!    [`SplitPreparation::reserve_target`] asks the target
//!    Vault's daemon for one receive address, under a wall-clock bound
//!    (#592 N2), checks it is that Vault's own receive derivation and records
//!    the index and script in the journal. A recorded reservation is reused
//!    (I12): another is refused until the recorded one is *proven used*, and
//!    only a strictly higher index may replace it.
//!    [`SplitPreparation::prove_target`] then reads, fresh from Connect, the
//!    address's history on BTCB2 *and* Bitcoin (I10, I11). Neither read
//!    depends on a daemon poll (N1), so a Connect-backed Vault and one on a
//!    managed Knots node prove freshness the same way and neither is refused
//!    up front (N3, reconciled with owner decision P4).
//! 2. **Construction.** [`SplitPreparation::construct_step2`] needs a live
//!    token from the latest `check_signing` (six confirmations, RDTS margin,
//!    claimed coins unspent on BTCB2), a target proven within the observation
//!    age, and the Connect BTCB2 six-block fee (D4; none is a refusal). It
//!    redeems the token for exactly the journal's claimed prevouts and
//!    tracked step-1 txid, then builds step 2 with the core builder off the
//!    executor (#614 G1): no change, one output, the reserved target. The
//!    unsigned step 2 is recorded in the journal, and a recorded one is
//!    rebuilt byte for byte on restart instead of rebuilt anew.
//! 3. **Handoff.** [`SplitPreparation::finish`] verifies the signed PSBT
//!    against that construction (`finalize_split_step2`) and moves the
//!    journal lock into a [`SplitStep2Coordinator`], as Claim's
//!    `Preparation::finish` does.
//! 4. **Review and submit.** The coordinator collects both chains twice
//!    around a BTCB2 preflight of the exact witness, requires the six
//!    confirmations again, records the submission intent and submits once.
//!    A refused preflight records nothing and keeps the verified signed step
//!    2 for another review.
//! 5. **Resend (P3-3, `resend`).** Any non-exact result after the intent
//!    is `Uncertain`, including a refusal before any byte left. The ordinary
//!    review then refuses and reconcile only observes, so only a distinct,
//!    explicitly reviewed resend of exactly the recorded bytes, after fresh
//!    evidence that they are absent from BTCB2 and their coins unspent, can
//!    send them again; live, or after a restart that rebuilds and verifies
//!    them ([`SplitStep2Coordinator::resume_uncertain`]).
//! 6. **Completion (B5a, `completion`).** On the reconciler, a reconcile
//!    that finds step 2 six deep on BTCB2 with step 1 still six deep on
//!    Bitcoin and absent from BTCB2, rechecked by a second collection,
//!    mints short-lived [`SplitCompletionEvidence`]; it records a
//!    digest-only `split_from` entry on the target Cube and only then
//!    permits deleting the foreign descriptors from the journal (P2,
//!    D18). A later reconcile clears the entry when the chains take the
//!    completion back (D17).
//!
//! Routes ([`SplitStep2Production`]): the target Vault daemon on exactly the
//! Connect BTCB2 Esplora at the Split's own Connect origin, preflighted by
//! Connect; or (P4) the daemon's bound Knots node, preflighted by that node
//! at the Connect-observed BTCB2 tip and sent to it through the backend
//! binding captured at review. The daemon transport itself checks that the
//! one output is its Vault's receive address at the recorded index.
//!
//! Nothing here is reachable from the GUI yet (D1).
use super::*;
use crate::{
    daemon::model::GetAddressResult,
    services::{
        claim_preflight::NodePolicy,
        foreign_psbt::{btcb2_sweep_feerate, SweepFeeSource},
    },
};
use coincube_core::{
    descriptors::CoincubeDescriptor,
    foreign_split::{
        create_split_step2, finalize_split_step2, reconstruct_split_step2, SplitCoin,
        SplitStep2Inputs, VerifiedSplitStep2,
    },
    miniscript::bitcoin::{
        absolute::LockTime, bip32::ChildNumber, psbt::Psbt, secp256k1, ScriptBuf,
    },
};

mod resend;
pub use resend::{ResendError, Step2ResubmissionReview};
mod completion;
pub use completion::{CompletionTarget, SplitCompletionEvidence, SplitCompletionReconciliation};

/// The wall-clock bound on a target reservation (#592 N2). The daemon call
/// runs on its own task, so a reservation stuck behind the daemon's locks
/// still returns here on time. A reservation that completes after the bound
/// still uses up that receive index; the gap is bounded by user retries.
pub const RESERVATION_BOUND: Duration = Duration::from_secs(10);

/// The reserved target as last proven by this preparation.
#[derive(Default)]
pub(super) struct TargetState {
    proven: Option<ProvenTarget>,
    /// The recorded index, proven used by a fresh read.
    used: Option<u32>,
}
struct ProvenTarget {
    index: u32,
    script: ScriptBuf,
    descriptor: CoincubeDescriptor,
    /// The oldest fresh read's stamp.
    observed_at: i64,
}

/// Why a reservation or freshness proof refused.
#[derive(Debug)]
pub enum TargetError {
    Coordinator(Error),
    /// A reservation is recorded and not proven used: reuse it (I12).
    AlreadyReserved,
    /// Step 1's inclusion is not tracked yet (no successful check): no
    /// receive index is reserved for a step 1 that may never confirm.
    NotTracking,
    /// No reservation is recorded.
    NoReservation,
    /// The daemon did not reserve an address within the bound, or failed.
    ReservationUnavailable,
    /// The address is not the target Vault's receive derivation at its index.
    NotTargetVault,
    /// Connect shows history for the address on this chain: it is not fresh.
    /// The index is now known used; a new reservation may replace it.
    Used(ChainId),
    /// Connect could not serve a fresh read. Not evidence of use.
    Unavailable(ChainId, FailureKind),
}
impl From<Error> for TargetError {
    fn from(error: Error) -> Self {
        Self::Coordinator(error)
    }
}
impl From<claim_workflow::Error> for TargetError {
    fn from(error: claim_workflow::Error) -> Self {
        Self::Coordinator(Error::Journal(error))
    }
}

/// Why step 2 was not built.
#[derive(Debug)]
pub enum Step2Error {
    Coordinator(Error),
    /// No Connect BTCB2 fee estimate (D4): fail closed.
    FeeUnavailable,
    /// No target proven fresh within the observation age, or the recorded
    /// reservation changed since.
    TargetNotProven,
    /// No successful `check_signing` since this preparation opened.
    NotChecked,
    /// The token was stale or for another check.
    Redeem(RedeemError),
    /// The foreign descriptors were deleted (completion).
    DescriptorsForgotten,
    Construction(coincube_core::foreign_split::Error),
}
impl From<Error> for Step2Error {
    fn from(error: Error) -> Self {
        Self::Coordinator(error)
    }
}
impl From<claim_workflow::Error> for Step2Error {
    fn from(error: claim_workflow::Error) -> Self {
        Self::Coordinator(Error::Journal(error))
    }
}

fn receive_script(descriptor: &CoincubeDescriptor, index: ChildNumber) -> ScriptBuf {
    descriptor
        .receive_descriptor()
        .derive(index, &secp256k1::Secp256k1::verification_only())
        .script_pubkey()
}

impl SplitPreparation {
    /// The recorded step-2 target index, if any. Restart data only.
    pub fn recorded_target(&self) -> Result<Option<u32>, Error> {
        Ok(self
            .controller
            .recorded_split()?
            .and_then(|record| record.target_index))
    }

    /// Whether [`Self::reserve_target`] may reserve: none is recorded, or
    /// the recorded one was proven used, and no step 2 is recorded.
    pub fn needs_reservation(&self) -> Result<bool, Error> {
        if self.controller.recorded_fork_sweep().is_some() {
            return Ok(false);
        }
        Ok(match self.recorded_target()? {
            None => true,
            Some(index) => self.target.used == Some(index),
        })
    }

    /// Reserve the step-2 target: one receive address from the target Vault
    /// daemon (`reserve`, its `get_new_address`) within `bound`, checked to be
    /// `vault`'s own receive derivation, recorded in the journal. Refused
    /// while a reservation is recorded and not proven used (I12), so a
    /// receive index is never consumed for nothing.
    pub async fn reserve_target<F>(
        &mut self,
        context: &Context,
        vault: &CoincubeDescriptor,
        reserve: F,
        bound: Duration,
    ) -> Result<u32, TargetError>
    where
        F: std::future::Future<Output = Result<GetAddressResult, DaemonError>> + Send + 'static,
    {
        self.current(context)?;
        if self.controller.phase() != Phase::Tracking {
            return Err(TargetError::NotTracking);
        }
        if !self.needs_reservation()? {
            return Err(TargetError::AlreadyReserved);
        }
        let replacing = self.recorded_target()?;
        self.target.proven = None;
        let reserved = match tokio::time::timeout(bound, tokio::spawn(reserve)).await {
            Ok(Ok(Ok(reserved))) => reserved,
            _ => return Err(TargetError::ReservationUnavailable),
        };
        self.current(context)?;
        let index = reserved.derivation_index;
        if index.is_hardened() {
            return Err(TargetError::NotTargetVault);
        }
        let script = receive_script(vault, index);
        if reserved.address.script_pubkey() != script {
            return Err(TargetError::NotTargetVault);
        }
        match replacing {
            Some(used) => self.controller.replace_used_split_target(
                context,
                used,
                u32::from(index),
                script,
            )?,
            None => self
                .controller
                .record_split_target(context, u32::from(index), script)?,
        }
        self.target = TargetState::default();
        Ok(u32::from(index))
    }

    /// Prove the recorded target belongs to `vault` (its receive derivation
    /// at the recorded index) and, until step 2 is recorded, that Connect
    /// shows no history for it on either chain. A used address marks its
    /// index used; [`Self::reserve_target`] may then replace it.
    pub async fn prove_target(
        &mut self,
        context: &Context,
        vault: &CoincubeDescriptor,
    ) -> Result<(), TargetError> {
        self.current(context)?;
        self.target.proven = None;
        let record = self
            .controller
            .recorded_split()?
            .ok_or(Error::InvalidBinding)?;
        let (Some(index), Some(script)) = (record.target_index, record.target_script) else {
            return Err(TargetError::NoReservation);
        };
        let child = ChildNumber::from_normal_idx(index).map_err(|_| TargetError::NotTargetVault)?;
        if receive_script(vault, child) != script {
            return Err(TargetError::NotTargetVault);
        }
        let address = Address::from_script(&script, Network::Bitcoin)
            .map_err(|_| TargetError::NotTargetVault)?
            .to_string();
        let plan = self.controller.plan();
        let mut oldest = self.services.source().now();
        // Once step 2 is recorded its target is fixed: only ownership is
        // checked again, so a later payment to the address cannot strand it.
        if self.controller.recorded_fork_sweep().is_none() {
            for chain in [plan.fork_chain, plan.bitcoin_chain] {
                let read = self
                    .services
                    .address_used(chain, &address)
                    .await
                    .map_err(|kind| TargetError::Unavailable(chain, kind))?;
                let now = self.services.source().now();
                if read.observed_at() < 0
                    || !now.checked_sub(read.observed_at()).is_some_and(|age| {
                        (0..=self.policy.observations.max_observation_age_seconds).contains(&age)
                    })
                {
                    return Err(TargetError::Unavailable(chain, FailureKind::Stale));
                }
                if *read.value() {
                    self.target.used = Some(index);
                    return Err(TargetError::Used(chain));
                }
                oldest = oldest.min(read.observed_at());
            }
        }
        self.current(context)?;
        self.target.proven = Some(ProvenTarget {
            index,
            script,
            descriptor: vault.clone(),
            observed_at: oldest,
        });
        Ok(())
    }

    /// Build step 2 under `authorization`; see the module documentation.
    /// `coins` are the claimed coins freshly authenticated
    /// (B1a's per-outpoint authentication). Returns the unsigned PSBT
    /// to sign. The token is redeemed (consumed) before construction.
    pub async fn construct_step2(
        &mut self,
        context: &Context,
        authorization: ForeignStep2Authorization,
        coins: Vec<SplitCoin>,
        fees: &dyn SweepFeeSource,
    ) -> Result<Psbt, Step2Error> {
        self.current(context)?;
        self.step2 = None;
        let recorded = self.controller.recorded_fork_sweep().cloned();
        let feerate = match recorded {
            Some(_) => None,
            None => Some(
                btcb2_sweep_feerate(fees)
                    .await
                    .ok_or(Step2Error::FeeUnavailable)?,
            ),
        };
        self.current(context)?;
        let tip = self.fork_tip.ok_or(Step2Error::NotChecked)?;
        let tip_height = u32::try_from(tip.height).map_err(|_| Step2Error::NotChecked)?;
        let locktime = LockTime::from_height(tip_height).map_err(|_| Step2Error::NotChecked)?;
        let record = self
            .controller
            .recorded_split()?
            .ok_or(Error::InvalidBinding)?;
        let target = self
            .target
            .proven
            .as_ref()
            .ok_or(Step2Error::TargetNotProven)?;
        let now = self.services.source().now();
        if record.target_index != Some(target.index)
            || record.target_script.as_ref() != Some(&target.script)
            || target.observed_at < 0
            || !now.checked_sub(target.observed_at).is_some_and(|age| {
                (0..=self.policy.observations.max_observation_age_seconds).contains(&age)
            })
        {
            return Err(Step2Error::TargetNotProven);
        }
        let source = record.source.ok_or(Step2Error::DescriptorsForgotten)?;
        let plan = self.controller.plan();
        // Only this preparation's latest check authorizes its construction.
        if authorization.check != (self.id, self.revision) {
            return Err(Step2Error::Redeem(RedeemError::Stale));
        }
        authorization
            .redeem(
                plan.fork_chain,
                self.context.generation,
                &plan.claimed_prevouts,
                self.tracked_txid,
            )
            .map_err(Step2Error::Redeem)?;
        let (chain, fork_height, claimed, script) = (
            plan.fork_chain,
            record.fork_height,
            plan.claimed_prevouts.clone(),
            target.script.clone(),
        );
        // #614 G1: the source-script window check derives up to a few
        // hundred scripts; keep it off the executor.
        let construction = tokio::task::spawn_blocking(move || {
            let inputs = SplitStep2Inputs {
                chain,
                source: &source,
                coins: &coins,
                fork_height,
                claimed: &claimed,
                target: &script,
            };
            match (recorded, feerate) {
                (Some(recorded), _) => reconstruct_split_step2(&inputs, &recorded, tip_height),
                (None, Some(feerate)) => create_split_step2(&inputs, feerate, locktime, tip_height),
                (None, None) => unreachable!("a fee is resolved whenever none is recorded"),
            }
        })
        .await
        .map_err(|_| Error::Revoked)?
        .map_err(Step2Error::Construction)?;
        self.current(context)?;
        let now = self.services.source().now();
        self.controller.prepare_split_step2(
            context,
            &construction,
            self.policy.observations,
            now,
        )?;
        let psbt = construction.psbt().clone();
        self.step2 = Some(Arc::new(construction));
        Ok(psbt)
    }

    /// Whether `signed` would finish: the same verification as
    /// [`Self::finish`] against the construction built under the token,
    /// without consuming the preparation (a partially signed import keeps
    /// the journal open). `Unsatisfied` means more signatures are needed.
    pub fn check_signed(
        &self,
        signed: &Psbt,
        coins: &[SplitCoin],
    ) -> Result<(), coincube_core::foreign_split::FinalizeError> {
        use coincube_core::foreign_split::FinalizeError;
        let construction = self
            .step2
            .as_ref()
            .ok_or(FinalizeError::ConstructionChanged)?;
        let source = self
            .controller
            .recorded_split()
            .ok()
            .flatten()
            .and_then(|record| record.source)
            .ok_or(FinalizeError::ConstructionChanged)?;
        finalize_split_step2(
            construction,
            coins,
            &source,
            signed,
            &secp256k1::Secp256k1::verification_only(),
        )
        .map(|_| ())
    }

    /// Verify the signed step 2 against the construction built under the
    /// token, then move the journal lock into the submission coordinator
    /// (Claim's `Preparation::finish` handoff). `coins` are the current
    /// authenticated claimed coins. The transport must be bound to the same
    /// Connect origin and to the Vault the target was proven for. This
    /// verifies signatures (CPU-bound); call it off the UI thread.
    pub fn finish(
        self,
        context: &Context,
        signed: &Psbt,
        coins: &[SplitCoin],
        transport: SplitStep2Production,
    ) -> Result<SplitStep2Coordinator, Error> {
        self.finish_with(context, signed, coins, Box::new(transport))
    }

    pub(super) fn finish_with(
        mut self,
        context: &Context,
        signed: &Psbt,
        coins: &[SplitCoin],
        transport: Box<dyn Step2Transport>,
    ) -> Result<SplitStep2Coordinator, Error> {
        self.current(context)?;
        let construction = self.step2.clone().ok_or(Error::InvalidReview)?;
        if self.controller.recorded_fork_sweep() != Some(&construction.psbt().unsigned_tx) {
            return Err(Error::InvalidReview);
        }
        let target = self.target.proven.as_ref().ok_or(Error::InvalidBinding)?;
        if transport.descriptor() != &target.descriptor
            || transport.origin() != self.services.origin()
            || construction.target() != target.script.as_script()
        {
            return Err(Error::InvalidBinding);
        }
        let target_index =
            ChildNumber::from_normal_idx(target.index).map_err(|_| Error::InvalidBinding)?;
        let source = self
            .controller
            .recorded_split()?
            .and_then(|record| record.source)
            .ok_or(Error::InvalidBinding)?;
        let verified = finalize_split_step2(
            &construction,
            coins,
            &source,
            signed,
            &secp256k1::Secp256k1::verification_only(),
        )
        .map_err(|_| Error::InvalidBinding)?;
        // Every token of the preparation dies here: its revoker is revoked
        // and its check counter is dropped with it. The coordinator gets its
        // own revoker.
        self.revoker.revoke();
        let SplitPreparation {
            id,
            context,
            generation,
            controller,
            claimed,
            services,
            policy,
            ..
        } = self;
        Ok(SplitStep2Coordinator {
            id,
            revision: 0,
            context,
            generation,
            controller,
            verified: Arc::new(verified),
            target_index,
            claimed,
            services,
            transport,
            policy,
            revoker: Revoker::new(),
        })
    }
}

/// The Split step-2 route: preflight and submission.
#[async_trait]
pub(super) trait Step2Transport: Send + Sync {
    /// The Connect origin this transport is bound to.
    fn origin(&self) -> &str;
    /// The target Vault daemon's main descriptor.
    fn descriptor(&self) -> &CoincubeDescriptor;
    async fn preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<RoutedEvidence, claim_preflight::Error>;
    async fn submit(
        &self,
        route: SubmissionRoute,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError>;
}

enum Step2Route {
    Connect,
    /// P4: the daemon's bound Bitcoind node (any `Bitcoind` backend on the
    /// BTCB2 Vault, owner decision recorded on #568).
    Node(route::BoundNode),
}

/// What a step-2 route needs from the target Vault daemon. Production is the
/// GUI daemon handle; tests substitute a binding they can switch.
#[async_trait]
pub(super) trait Step2Daemon: Send + Sync + 'static {
    /// Identity of the daemon's backend instance and configuration.
    type Binding: Clone + PartialEq + Send + Sync + 'static;
    async fn binding(&self) -> Result<Self::Binding, DaemonError>;
    /// The daemon's current Bitcoind node, if its backend is one.
    fn node(&self) -> Option<coincubed::config::BitcoindConfig>;
    async fn submit_connect(
        &self,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        binding: Self::Binding,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError>;
    async fn submit_node(
        &self,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        binding: Self::Binding,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError>;
}
#[async_trait]
impl Step2Daemon for Arc<dyn Daemon + Send + Sync> {
    type Binding = coincubed::poison_broadcast::ClaimBackendBinding;
    async fn binding(&self) -> Result<Self::Binding, DaemonError> {
        self.claim_backend_binding().await
    }
    fn node(&self) -> Option<coincubed::config::BitcoindConfig> {
        match self.config()?.bitcoin_backend.as_ref()? {
            coincubed::config::BitcoinBackend::Bitcoind(node) => Some(node.clone()),
            _ => None,
        }
    }
    async fn submit_connect(
        &self,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        binding: Self::Binding,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        self.submit_verified_split_step2(verified, target, binding, gate)
            .await
    }
    async fn submit_node(
        &self,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        binding: Self::Binding,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        self.submit_verified_split_step2_to_node(verified, target, binding, gate)
            .await
    }
}

/// Both step-2 routes over a target Vault daemon. The daemon's backend
/// binding is captured at the first review on either route; every later
/// review must see the same binding (a backend switch, daemon restart or
/// node change refuses as `BackendChanged`, #630 F3), and submission passes
/// the captured binding to the daemon, which refuses a switched backend
/// before and after taking its backend lock (#568 B3b-2).
pub(super) struct Step2Routes<D: Step2Daemon> {
    daemon: D,
    preflight: PreflightClient,
    origin: String,
    descriptor: CoincubeDescriptor,
    route: Step2Route,
    binding: std::sync::OnceLock<D::Binding>,
    expected_generation: u64,
    generation: watch::Receiver<u64>,
}
impl<D: Step2Daemon> Step2Routes<D> {
    /// Test-only: the routes over a substitute daemon, admission skipped.
    #[cfg(test)]
    pub(super) fn for_test(
        daemon: D,
        preflight: PreflightClient,
        origin: String,
        descriptor: CoincubeDescriptor,
        node: Option<coincubed::config::BitcoindConfig>,
        expected_generation: u64,
        generation: watch::Receiver<u64>,
    ) -> Self {
        Self {
            daemon,
            preflight,
            origin,
            descriptor,
            route: match node {
                Some(node) => Step2Route::Node(route::BoundNode::new(node)),
                None => Step2Route::Connect,
            },
            binding: std::sync::OnceLock::new(),
            expected_generation,
            generation,
        }
    }
    /// The route this transport reviews and submits on, for the review
    /// screen's label (`SubmissionRoute::label`).
    pub fn route(&self) -> SubmissionRoute {
        match &self.route {
            Step2Route::Connect => SubmissionRoute::Connect,
            Step2Route::Node(bound) => bound.route(),
        }
    }
}
#[async_trait]
impl<D: Step2Daemon> Step2Transport for Step2Routes<D> {
    fn origin(&self) -> &str {
        &self.origin
    }
    fn descriptor(&self) -> &CoincubeDescriptor {
        &self.descriptor
    }
    async fn preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<RoutedEvidence, claim_preflight::Error> {
        let current = self
            .daemon
            .binding()
            .await
            .map_err(|_| claim_preflight::Error::BackendChanged)?;
        if self.binding.get_or_init(|| current.clone()) != &current {
            return Err(claim_preflight::Error::BackendChanged);
        }
        match &self.route {
            Step2Route::Connect => {
                if self.daemon.node().is_some() {
                    return Err(claim_preflight::Error::BackendChanged);
                }
                self.preflight
                    .observe(ChainId::BitcoinBlake2b, tx, tip, policy)
                    .await
                    .map(RoutedEvidence::Connect)
            }
            Step2Route::Node(bound) => {
                if !self.daemon.node().is_some_and(|node| bound.matches(&node)) {
                    return Err(claim_preflight::Error::BackendChanged);
                }
                route::node_preflight(
                    bound,
                    tx,
                    tip,
                    policy,
                    CollectionContext {
                        expected_generation: self.expected_generation,
                        generation: self.generation.clone(),
                    },
                )
                .await
            }
        }
    }
    async fn submit(
        &self,
        route: SubmissionRoute,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        let binding = self
            .binding
            .get()
            .ok_or(DaemonError::ClientNotSupported)?
            .clone();
        match (&self.route, route) {
            (Step2Route::Connect, SubmissionRoute::Connect) => {
                self.daemon
                    .submit_connect(verified, target, binding, gate)
                    .await
            }
            (Step2Route::Node(bound), SubmissionRoute::BitcoinNode { .. })
                if bound.route() == route =>
            {
                self.daemon
                    .submit_node(verified, target, binding, gate)
                    .await
            }
            _ => Err(DaemonError::ClientNotSupported),
        }
    }
}

/// The target BTCB2 Vault daemon as a step-2 route; see the module
/// documentation for admission.
pub struct SplitStep2Production(Step2Routes<Arc<dyn Daemon + Send + Sync>>);
impl SplitStep2Production {
    /// Refused before anything is reviewed: a non-embedded daemon, any chain
    /// but BTCB2 mainnet, a Connect origin that is not exactly
    /// `scheme://host[:port]/`, and any backend other than exactly Connect's
    /// BTCB2 Esplora at that origin (no token, no fallback) or a bound
    /// Bitcoind node.
    pub fn new(
        client: &CoincubeClient,
        daemon: Arc<dyn Daemon + Send + Sync>,
        expected_generation: u64,
        generation: watch::Receiver<u64>,
    ) -> Result<Self, Error> {
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
        if config.bitcoin_config.chain != ChainId::BitcoinBlake2b
            || config.bitcoin_config.network != Network::Bitcoin
        {
            return Err(Error::Unsupported);
        }
        let endpoint = format!(
            "{}/api/v1/esplora/bitcoin-blake2b/mainnet",
            origin.as_str().trim_end_matches('/')
        );
        let route = match config.bitcoin_backend.as_ref() {
            Some(coincubed::config::BitcoinBackend::Esplora(selection))
                if selection.addr.trim_end_matches('/') == endpoint
                    && selection.token.is_none()
                    && selection.fallback_addr.is_none()
                    && selection.fallback_token.is_none()
                    && selection.secondary_fallback_addr.is_none()
                    && selection.secondary_fallback_token.is_none()
                    && config.fallback_esplora.is_none() =>
            {
                Step2Route::Connect
            }
            Some(coincubed::config::BitcoinBackend::Bitcoind(node)) => {
                Step2Route::Node(route::BoundNode::new(node.clone()))
            }
            _ => return Err(Error::Unsupported),
        };
        let preflight = PreflightClient::new(
            origin.as_str(),
            CollectionContext {
                expected_generation,
                generation: generation.clone(),
            },
        )
        .map_err(Error::Preflight)?;
        Ok(Self(Step2Routes {
            descriptor: config.main_descriptor.clone(),
            daemon,
            preflight,
            origin: origin.as_str().to_owned(),
            route,
            binding: std::sync::OnceLock::new(),
            expected_generation,
            generation,
        }))
    }
    /// The route this transport uses; see [`SubmissionRoute::label`]. The
    /// node route sends the transaction to the Vault's own node, which learns
    /// it (and this machine's address) before it relays (B3b-2b privacy note).
    pub fn route(&self) -> SubmissionRoute {
        self.0.route()
    }
}
#[async_trait]
impl Step2Transport for SplitStep2Production {
    fn origin(&self) -> &str {
        self.0.origin()
    }
    fn descriptor(&self) -> &CoincubeDescriptor {
        self.0.descriptor()
    }
    async fn preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<RoutedEvidence, claim_preflight::Error> {
        self.0.preflight(tx, tip, policy).await
    }
    async fn submit(
        &self,
        route: SubmissionRoute,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        self.0.submit(route, verified, target, gate).await
    }
}

/// Owns the Split journal while the verified step 2 is reviewed and
/// submitted. Reopening never restores review authority: a reopened
/// uncertain step 2 ([`Self::resume_uncertain`]) can only be reconciled or,
/// after a fresh resend review, resent.
pub struct SplitStep2Coordinator {
    id: u64,
    revision: u64,
    context: Context,
    generation: watch::Receiver<u64>,
    controller: Controller,
    verified: Arc<VerifiedSplitStep2>,
    target_index: ChildNumber,
    /// Each claimed prevout and the address its output pays, for the fresh
    /// BTCB2 unspent reads of a resend review.
    claimed: Vec<(OutPoint, String)>,
    services: Box<dyn SplitForkServices>,
    transport: Box<dyn Step2Transport>,
    policy: CheckPolicy,
    revoker: Revoker,
}
impl SplitStep2Coordinator {
    pub fn context(&self) -> &Context {
        &self.context
    }
    pub fn revoker(&self) -> Revoker {
        self.revoker.clone()
    }
    /// The verified signed step 2, kept across refused reviews.
    pub fn transaction(&self) -> &Transaction {
        self.verified.transaction()
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
            self.revoker.revoke();
            self.controller.invalidate();
            return Err(Error::Revoked);
        }
        Ok(())
    }
    async fn collect(&self) -> Result<Collected, Error> {
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
    /// Step 1 still six deep at the tip, still absent on BTCB2.
    fn deep_enough(&self, observations: &ObservationBundle) -> bool {
        let plan = self.controller.plan();
        completion_bitcoin_confirmed(&plan, observations.bitcoin)
            && observations.fork.chain == plan.fork_chain
            && observations.fork.step1_txid == plan.step1_txid()
            && observations.fork.step1_presence
                == coincube_core::claim::ForkTransactionPresence::NotObserved
    }
    async fn fresh_snapshot(&mut self, context: &Context) -> Result<ReviewSnapshot, Error> {
        self.current(context)?;
        if self.recorded_outcome().is_some() {
            return Err(Error::SubmissionAlreadyRecorded);
        }
        let ticket = self.controller.begin_check(context)?;
        let first = self.collect().await?;
        if first.assessment != Assessment::ObservationsEligibleForPreflight
            || !self.deep_enough(&first.observations)
        {
            return Err(Error::NotReady(first.assessment));
        }
        let tx = self.verified.transaction().clone();
        let evidence = self
            .transport
            .preflight(&tx, first.observations.fork.tip.hash, self.policy.preflight)
            .await
            .map_err(Error::Preflight)?;
        let last = self.collect().await?;
        self.current(context)?;
        if !same_view(first.observations, last.observations) {
            return Err(Error::ChangedReview);
        }
        let chain_ok = match &evidence {
            RoutedEvidence::Connect(evidence) => evidence.chain() == ChainId::BitcoinBlake2b,
            // The node's best block was the BTCB2 tip observed via Connect.
            RoutedEvidence::BitcoinNode(..) => true,
        };
        if !chain_ok
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
        if last.assessment != Assessment::ObservationsEligibleForPreflight
            || !self.deep_enough(&last.observations)
        {
            return Err(Error::NotReady(last.assessment));
        }
        let now = self.services.source().now();
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
        Ok(ReviewSnapshot {
            transaction: tx.clone(),
            wallet: self.controller.identity().clone(),
            txid: tx.compute_txid(),
            wtxid: tx.compute_wtxid(),
            fee_sats: self.verified.fee().to_sat(),
            vsize: tx.vsize(),
            observations: last_data.observations,
            route: evidence.route(),
            not_after,
        })
    }
    /// A fresh review of the verified step 2. A refused preflight returns
    /// `PolicyRejected` and records nothing; the signed step 2 is kept.
    pub async fn prepare_review(&mut self, context: &Context) -> Result<Review, Error> {
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        let snapshot = self.fresh_snapshot(context).await?;
        Ok(Review {
            coordinator: self.id,
            revision: self.revision,
            snapshot,
        })
    }
    /// Explicit user confirmation of this one-use view. Both chains and the
    /// preflight are checked again on the same route; the submission intent
    /// is recorded before the one attempt. Any recorded attempt can only be
    /// reconciled.
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
            || review.snapshot.route != refreshed.route
            || !same_view(review.snapshot.observations, refreshed.observations)
        {
            return Err(Error::ChangedReview);
        }
        self.current(context)?;
        if Instant::now() >= refreshed.not_after {
            return Err(Error::ExpiredEvidence);
        }
        self.controller.record_split_step2_broadcast_intent(
            context,
            &self.verified,
            self.policy.observations,
            self.services.source().now(),
        )?;
        Ok(self.send_recorded(context, refreshed).await)
    }

    /// The one send of an attempt the journal has just recorded (the
    /// submission intent or a resend), on the reviewed route under its
    /// deadline. Anything but the route's exact acceptance is `Uncertain`;
    /// nothing here retries. Only a send that completed without that
    /// acceptance is recorded as returned, which a resend review needs.
    async fn send_recorded(&mut self, context: &Context, refreshed: ReviewSnapshot) -> Outcome {
        let uncertain = Outcome::Uncertain {
            txid: refreshed.txid,
            wtxid: refreshed.wtxid,
        };
        let (gate, revoker) = SubmissionGate::for_split_step2(&self.verified, refreshed.not_after);
        let _pending = PendingGate(revoker.clone());
        if self.revoker.register(revoker).is_err() || self.current(context).is_err() {
            return uncertain;
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
        let submit = self.transport.submit(
            refreshed.route,
            self.verified.clone(),
            self.target_index,
            Arc::new(gate),
        );
        let result = tokio::select! { biased;
            _ = cancelled => None,
            result = tokio::time::timeout(Duration::from_secs(30), submit) => result.ok(),
        };
        if self.current(context).is_err() {
            return uncertain;
        }
        match result {
            Some(Ok(SubmissionOutcome::UpstreamAccepted { txid, wtxid }))
                if txid == refreshed.txid && wtxid == refreshed.wtxid =>
            {
                Outcome::UpstreamAccepted { txid, wtxid }
            }
            Some(_) => {
                // The send completed without the route's acceptance (a
                // refusal, a lost upstream answer, another txid): record that
                // it returned, the one record that lets a resend be reviewed
                // (P3-3). If this write fails, the journal still holds the
                // attempt as unreturned, which never resends, so the outcome
                // stands either way.
                let _unreturned_refuses = self.controller.record_split_step2_returned(context);
                uncertain
            }
            // Cancelled or past the bound: the send may still be under way
            // and may yet be accepted. It stays unreturned: never resent.
            None => uncertain,
        }
    }
}
impl Drop for SplitStep2Coordinator {
    fn drop(&mut self) {
        self.revoker.revoke();
    }
}

/// An observation source that notes any read of one transaction on one
/// chain that answered it present (mempool or block), whatever becomes of the
/// collection the read belongs to: a collection can fail after one of its
/// reads saw the recorded step 2 (P3-3). Every other read passes through.
pub(super) struct SightingProbe<'a> {
    inner: &'a dyn ObservationSource,
    chain: ChainId,
    txid: Txid,
    sighted: std::sync::atomic::AtomicBool,
}
impl<'a> SightingProbe<'a> {
    pub(super) fn new(inner: &'a dyn ObservationSource, chain: ChainId, txid: Txid) -> Self {
        Self {
            inner,
            chain,
            txid,
            sighted: std::sync::atomic::AtomicBool::new(false),
        }
    }
    /// Whether any read answered the transaction present.
    pub(super) fn sighted(&self) -> bool {
        self.sighted.load(Ordering::SeqCst)
    }
}
#[async_trait]
impl ObservationSource for SightingProbe<'_> {
    fn now(&self) -> i64 {
        self.inner.now()
    }
    async fn anchor(
        &self,
        chain: ChainId,
    ) -> Result<crate::services::coincube::network_anchor::NetworkAnchorStatus, FailureKind> {
        self.inner.anchor(chain).await
    }
    async fn tip(
        &self,
        chain: ChainId,
    ) -> Result<FreshRead<coincube_core::claim::BlockRef>, FailureKind> {
        self.inner.tip(chain).await
    }
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<claim_observation::TransactionObservation>, FailureKind> {
        let read = self.inner.transaction(chain, txid).await;
        if chain == self.chain
            && txid == self.txid
            && read.as_ref().is_ok_and(|read| {
                *read.value() != claim_observation::TransactionObservation::Absent
            })
        {
            self.sighted.store(true, Ordering::SeqCst);
        }
        read
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        self.inner.hash_at_height(chain, height).await
    }
}

/// Check the recorded step 2's chain inclusion together with step 1's on
/// Bitcoin, keyed by the recorded *signed* step-2 txid. A recorded submission
/// only identifies what to look up; this never resends or authorizes one. A
/// step 2 seen on BTCB2 is recorded as observed, which ends any resend
/// (P3-3). The read runs with the resend permission durably withdrawn and
/// gives it back only when no read of the collection saw the step 2 (even
/// one the collection then failed past) in the still-current session, so a
/// sighting that fails to record still ends the resend.
async fn reconcile_recorded(
    controller: &mut Controller,
    services: &dyn SplitForkServices,
    policy: CheckPolicy,
    context: &Context,
    generation: &watch::Receiver<u64>,
) -> Result<(Status, claim_observation::TransactionObservation), Error> {
    let submission = controller
        .recorded_fork_submission()
        .ok_or(Error::InvalidBinding)?;
    if controller
        .recorded_split_step2()
        .is_none_or(|signed| signed.compute_txid() != submission.txid())
    {
        return Err(Error::InvalidBinding);
    }
    let hold = controller.hold_split_step2_return(context)?;
    let ticket = controller.begin_check(context)?;
    let plan = controller.plan();
    let probe = SightingProbe::new(services.source(), plan.fork_chain, submission.txid());
    let collected = claim_observation::collect_sweep(
        &probe,
        &plan,
        submission.txid(),
        policy.observations,
        policy.collection_budget,
        CollectionContext {
            expected_generation: context.generation,
            generation: generation.clone(),
        },
    )
    .await;
    let current = *generation.borrow() == context.generation && generation.has_changed().is_ok();
    let sighted = probe.sighted()
        || collected
            .as_ref()
            .is_ok_and(|c| c.transaction() != claim_observation::TransactionObservation::Absent);
    if let (true, false, Some(hold)) = (current, sighted, hold) {
        controller.release_split_step2_return(context, hold)?;
    }
    let collected = collected.map_err(Error::Observation)?;
    if !current {
        controller.invalidate();
        return Err(Error::Revoked);
    }
    let status = controller.apply_observation(
        ticket,
        context,
        Ok(collected.assessment()),
        policy.observations,
        services.source().now(),
    )?;
    controller.record_split_step2_observed(context, collected.transaction())?;
    Ok((status, collected.transaction()))
}

impl SplitStep2Coordinator {
    /// After a submission: the recorded step 2's inclusion on BTCB2 and step
    /// 1's on Bitcoin. Never resends.
    pub async fn reconcile_sweep(
        &mut self,
        context: &Context,
    ) -> Result<(Status, claim_observation::TransactionObservation), Error> {
        self.current(context)?;
        reconcile_recorded(
            &mut self.controller,
            self.services.as_ref(),
            self.policy,
            context,
            &self.generation,
        )
        .await
    }
}

/// Restart after a recorded step-2 submission (#568 B3b-2): owns the Split
/// journal and can only reconcile. It needs no construction, coins or
/// signatures (the claimed coins may already be spent on BTCB2 by step 2),
/// holds no transport, and so cannot send anything again.
pub struct SplitStep2Reconciler {
    context: Context,
    generation: watch::Receiver<u64>,
    controller: Controller,
    services: Box<dyn SplitForkServices>,
    policy: CheckPolicy,
    revoker: Revoker,
    /// Completion evidence (`completion`) outlives nothing of this: a
    /// dropped reconciler kills every evidence it minted.
    lifetime: Arc<()>,
    /// Revoked at every check, so an earlier check's completion evidence
    /// never survives a later one (Claim's `completion_revoker`).
    completion_revoker: Revoker,
}
impl SplitStep2Reconciler {
    /// Reopen the Split journal of `source_digest` under `target_cube`.
    /// Refused unless step 2's signed bytes and submission are recorded.
    pub fn resume(
        directory: &Path,
        target_cube: String,
        source_digest: sha256::Hash,
        production: SplitForkProduction,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        let context = production.context.clone();
        let generation = production.generation.clone();
        Self::open(
            directory,
            target_cube,
            source_digest,
            context,
            generation,
            Box::new(production),
            policy,
        )
    }
    pub(super) fn open(
        directory: &Path,
        target_cube: String,
        source_digest: sha256::Hash,
        context: Context,
        generation: watch::Receiver<u64>,
        services: Box<dyn SplitForkServices>,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        if !policy.valid()
            || *generation.borrow() != context.generation
            || generation.has_changed().is_err()
        {
            return Err(Error::InvalidBinding);
        }
        let identity = claim_workflow::split_identity(target_cube, source_digest);
        let controller =
            Controller::reopen_settling_blocking(directory, &identity, context.clone())?;
        let submission = controller
            .recorded_fork_submission()
            .ok_or(Error::InvalidBinding)?;
        if controller.recorded_split()?.is_none()
            || controller.plan().bitcoin_chain != ChainId::Bitcoin
            || controller
                .recorded_split_step2()
                .is_none_or(|signed| signed.compute_txid() != submission.txid())
        {
            return Err(Error::InvalidBinding);
        }
        Ok(Self {
            context,
            generation,
            controller,
            services,
            policy,
            revoker: Revoker::new(),
            lifetime: Arc::new(()),
            completion_revoker: Revoker::new(),
        })
    }
    pub fn revoker(&self) -> Revoker {
        self.revoker.clone()
    }
    /// The recorded possible submission: what to reconcile, never resend.
    pub fn recorded_outcome(&self) -> Option<Outcome> {
        self.controller
            .recorded_fork_submission()
            .map(|s| Outcome::Uncertain {
                txid: s.txid(),
                wtxid: s.wtxid(),
            })
    }
    pub async fn reconcile_sweep(
        &mut self,
        context: &Context,
    ) -> Result<(Status, claim_observation::TransactionObservation), Error> {
        self.completion_revoker.revoke();
        if self.revoker.is_revoked()
            || context != &self.context
            || *self.generation.borrow() != context.generation
            || self.generation.has_changed().is_err()
        {
            self.revoker.revoke();
            self.controller.invalidate();
            return Err(Error::Revoked);
        }
        reconcile_recorded(
            &mut self.controller,
            self.services.as_ref(),
            self.policy,
            context,
            &self.generation,
        )
        .await
    }
}
