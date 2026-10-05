//! Split (#568 B4b-3a): the unified fallback's services, the fork-only route.
//! One BTCB2 sweep of the foreign wallet's splittable coins into the target
//! Vault, signed `ALL|UNIFIED` (B4b-1a) and so invalid on Bitcoin, with no
//! step 1. Dormant (D1): no GUI caller.
//!
//! [`UnifiedCoordinator`] owns the C2 gate:
//!
//! 1. **Target.** [`UnifiedCoordinator::reserve_target`] asks the target
//!    Vault's daemon for one receive address within [`RESERVATION_BOUND`]
//!    and checks it is that Vault's own receive derivation (the transport's
//!    descriptor). Nothing is journaled yet (U3): a reserved index the user
//!    then abandons is a tolerated gap. [`UnifiedCoordinator::prove_target`]
//!    reads, fresh from Connect, the address's history on BTCB2 and on
//!    Bitcoin (I10, I11); a used address may be replaced by a strictly
//!    higher index.
//! 2. **Build.** [`UnifiedCoordinator::build`] needs the target proven within
//!    the observation age, a fresh BTCB2 anchor naming the fork height the
//!    coins were authenticated against (D10), and the Connect BTCB2 six-block
//!    fee (D4; none is a refusal). Core builds the sweep off the executor,
//!    with the anchor's tip as its locktime. A reopened unsubmitted record is
//!    rebuilt byte for byte instead and revalidated against the journal.
//! 3. **Sign.** The caller signs the PSBT (seeds, B4b-3c) and
//!    [`UnifiedCoordinator::verify_signed`] keeps core's verified sweep:
//!    every input Protected, or nothing.
//! 4. **Review.** Two fork-only collections of the signed sweep
//!    (`claim_observation::collect_fork_sweep`: the anchor around two reads
//!    of the sweep, nothing on Bitcoin) must agree. Between them: a fresh
//!    BTCB2 unspent read of every coin, the target's freshness again on both
//!    chains and a preflight of the exact witness on the admitted route
//!    (Connect, or the P4 node at the observed BTCB2 tip). The anchor must
//!    still name the fork height, and the sweep must be absent from BTCB2.
//!    The review's deadline comes from its oldest read.
//! 5. **Confirm.** The review is checked again; then the fork-only journal is
//!    created (U3), the submission intent is recorded with the verified sweep
//!    (Reviewer-650 F2) under that same lock, and the sweep is sent once.
//!    Anything but the route's exact acceptance is `Uncertain` and is only
//!    ever reconciled: there is no unified resend (U4).
//!
//! [`UnifiedReconciler`] reopens a fork-only journal whose submission is
//! recorded; like the coordinator after its send, it can only observe the
//! sweep on BTCB2 (`collect_fork_sweep`) and records any sighting. It holds
//! no transport. It has no step 1, so nothing here reports one.
use super::*;
use crate::services::claim_observation::{
    collect_fork_sweep, fork_anchor, ForkSweepObservation, TransactionObservation,
};
use coincube_core::{
    claim::BlockRef,
    foreign_split::{
        create_unified_sweep, finalize_unified_sweep, reconstruct_unified_sweep, SplitSource,
        UnifiedInputs, UnifiedReplayStatus, UnifiedSweep, UnifiedSweepFinalizeError,
        VerifiedUnifiedSweep,
    },
    psbt_unified::UnifiedPsbt,
};
use std::path::PathBuf;

/// The unified sweep's chains: it spends on BTCB2 mainnet; its target's
/// freshness is also read on Bitcoin.
const FORK_CHAIN: ChainId = ChainId::BitcoinBlake2b;

/// Why the unified route refused.
#[derive(Debug)]
pub enum UnifiedError {
    /// A coordinator refusal: revoked session, stale or changed view, a
    /// refused preflight (`PolicyRejected`, the verified sweep is kept), an
    /// expired review, a journal refusal.
    Coordinator(Error),
    /// The target reservation or its freshness proof.
    Target(TargetError),
    /// No Connect BTCB2 fee estimate (D4): fail closed.
    FeeUnavailable,
    /// No target proven fresh within the observation age.
    TargetNotProven,
    /// No sweep built in this coordinator.
    NotBuilt,
    /// No verified signed sweep.
    NotSigned,
    /// D10: the fresh BTCB2 anchor names another fork height than the one
    /// the coins were authenticated against.
    ForkHeightChanged {
        authenticated: u64,
        anchor: u64,
    },
    /// A coin is not among its address's fresh BTCB2 unspent outputs.
    CoinSpent(OutPoint),
    /// Connect could not serve a fresh BTCB2 unspent read. Not a spend.
    Unavailable(OutPoint, FailureKind),
    /// Before any submission was recorded here, a fresh read found the sweep
    /// on BTCB2 already (mempool or block).
    SweepSeen(TransactionObservation),
    Construction(coincube_core::foreign_split::Error),
    Finalize(UnifiedSweepFinalizeError),
}
impl From<Error> for UnifiedError {
    fn from(error: Error) -> Self {
        Self::Coordinator(error)
    }
}
impl From<claim_workflow::Error> for UnifiedError {
    fn from(error: claim_workflow::Error) -> Self {
        Self::Coordinator(Error::Journal(error))
    }
}
impl From<TargetError> for UnifiedError {
    fn from(error: TargetError) -> Self {
        match error {
            TargetError::Coordinator(error) => Self::Coordinator(error),
            other => Self::Target(other),
        }
    }
}
impl From<SplitCheckError> for UnifiedError {
    fn from(error: SplitCheckError) -> Self {
        match error {
            SplitCheckError::Coordinator(error) => Self::Coordinator(error),
            SplitCheckError::ClaimedCoinSpent(outpoint) => Self::CoinSpent(outpoint),
            SplitCheckError::Unavailable(outpoint, kind) => Self::Unavailable(outpoint, kind),
        }
    }
}

/// The unified route: preflight and submission over the target Vault daemon.
#[async_trait]
pub(in super::super) trait UnifiedTransport: Send + Sync {
    /// The Connect origin this transport is bound to.
    fn origin(&self) -> &str;
    /// The target Vault daemon's main descriptor.
    fn descriptor(&self) -> &CoincubeDescriptor;
    /// The route reviews and submissions use, for the review label.
    fn route(&self) -> SubmissionRoute;
    async fn preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<RoutedEvidence, claim_preflight::Error>;
    async fn submit(
        &self,
        route: SubmissionRoute,
        verified: Arc<VerifiedUnifiedSweep>,
        target: ChildNumber,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError>;
}
/// Step 2's routes carry the unified sweep too: the same admission, the same
/// binding captured at the first review and checked at every later review
/// and by the daemon at the send.
#[async_trait]
impl<D: Step2Daemon> UnifiedTransport for Step2Routes<D> {
    fn origin(&self) -> &str {
        &self.origin
    }
    fn descriptor(&self) -> &CoincubeDescriptor {
        &self.descriptor
    }
    fn route(&self) -> SubmissionRoute {
        Step2Routes::route(self)
    }
    async fn preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<RoutedEvidence, claim_preflight::Error> {
        Step2Transport::preflight(self, tx, tip, policy).await
    }
    async fn submit(
        &self,
        route: SubmissionRoute,
        verified: Arc<VerifiedUnifiedSweep>,
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
                    .submit_unified_connect(verified, target, binding, gate)
                    .await
            }
            (Step2Route::Node(bound), SubmissionRoute::BitcoinNode { .. })
                if bound.route() == route =>
            {
                self.daemon
                    .submit_unified_node(verified, target, binding, gate)
                    .await
            }
            _ => Err(DaemonError::ClientNotSupported),
        }
    }
}
#[async_trait]
impl UnifiedTransport for SplitStep2Production {
    fn origin(&self) -> &str {
        UnifiedTransport::origin(&self.0)
    }
    fn descriptor(&self) -> &CoincubeDescriptor {
        UnifiedTransport::descriptor(&self.0)
    }
    fn route(&self) -> SubmissionRoute {
        self.0.route()
    }
    async fn preflight(
        &self,
        tx: &Transaction,
        tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<RoutedEvidence, claim_preflight::Error> {
        UnifiedTransport::preflight(&self.0, tx, tip, policy).await
    }
    async fn submit(
        &self,
        route: SubmissionRoute,
        verified: Arc<VerifiedUnifiedSweep>,
        target: ChildNumber,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        UnifiedTransport::submit(&self.0, route, verified, target, gate).await
    }
}

/// The target as reserved and last proven in this coordinator.
#[derive(Default)]
struct Target {
    reserved: Option<(u32, ScriptBuf)>,
    /// The reserved index, proven used by a fresh read.
    used: Option<u32>,
    /// The oldest stamp of the last successful freshness proof.
    proven_at: Option<i64>,
}

/// Information the review screen shows. Never a permission token.
#[derive(Debug, Clone)]
pub struct UnifiedReviewSnapshot {
    pub transaction: Transaction,
    pub txid: Txid,
    pub wtxid: Wtxid,
    pub fee_sats: u64,
    pub vsize: usize,
    /// The BTCB2 tip both collections saw.
    pub fork_tip: BlockRef,
    pub target_index: u32,
    pub route: SubmissionRoute,
    /// Protected, by core's construction of the verified sweep.
    pub replay: UnifiedReplayStatus,
    not_after: Instant,
}
/// One-use review identity. No Clone, deserialization or public field
/// construction; confirming it must correspond to explicit user
/// confirmation of this view.
pub struct UnifiedReview {
    coordinator: u64,
    revision: u64,
    snapshot: UnifiedReviewSnapshot,
}
impl UnifiedReview {
    pub fn snapshot(&self) -> &UnifiedReviewSnapshot {
        &self.snapshot
    }
}

/// A fork-only reconcile: the recorded sweep on BTCB2 and the tip it was
/// observed at. There is no step 1 to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnifiedReconcile {
    pub sweep: TransactionObservation,
    pub fork_tip: BlockRef,
}

/// Owns the unified route from reservation to submission; see the module
/// documentation. Reopening an unsubmitted record restores no authority: it
/// is rebuilt, revalidated and reviewed again.
pub struct UnifiedCoordinator {
    id: u64,
    revision: u64,
    context: Context,
    generation: watch::Receiver<u64>,
    directory: PathBuf,
    target_cube: String,
    source: SplitSource,
    coins: Vec<SplitCoin>,
    /// Each coin and the address its output pays.
    claimed: Vec<(OutPoint, String)>,
    fork_height: u64,
    /// The fork-only journal: none until confirmation creates it (U3), or the
    /// unsubmitted record a restart reopened.
    controller: Option<Controller>,
    services: Box<dyn SplitForkServices>,
    transport: Box<dyn UnifiedTransport>,
    policy: CheckPolicy,
    revoker: Revoker,
    target: Target,
    sweep: Option<Arc<UnifiedSweep>>,
    verified: Option<Arc<VerifiedUnifiedSweep>>,
    /// Test-only: how far confirmation's expiry check sees the monotonic
    /// clock ahead of now (`skew_clock_for_test`), to expire a review.
    #[cfg(test)]
    clock_skew: Duration,
}

impl UnifiedCoordinator {
    /// A new unified route for `source`'s `coins`, freshly authenticated by
    /// the caller (B1a) against `fork_height`, into the Vault behind
    /// `transport`. No journal is created before confirmation (U3); one that
    /// exists then refuses it.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        directory: &Path,
        target_cube: String,
        source: SplitSource,
        coins: Vec<SplitCoin>,
        fork_height: u64,
        production: SplitForkProduction,
        transport: SplitStep2Production,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        let context = production.context.clone();
        let generation = production.generation.clone();
        Self::open(
            directory,
            target_cube,
            source,
            coins,
            fork_height,
            context,
            generation,
            Box::new(production),
            Box::new(transport),
            policy,
            false,
        )
    }
    /// Reopen an unsubmitted fork-only record (a confirmation that created
    /// the journal and stopped before its intent was recorded). The
    /// recorded target is kept; [`Self::build`] rebuilds the recorded sweep
    /// exactly and revalidates it. A recorded submission refuses: only
    /// [`UnifiedReconciler`] opens it.
    #[allow(clippy::too_many_arguments)]
    pub fn resume(
        directory: &Path,
        target_cube: String,
        source: SplitSource,
        coins: Vec<SplitCoin>,
        fork_height: u64,
        production: SplitForkProduction,
        transport: SplitStep2Production,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        let context = production.context.clone();
        let generation = production.generation.clone();
        Self::open(
            directory,
            target_cube,
            source,
            coins,
            fork_height,
            context,
            generation,
            Box::new(production),
            Box::new(transport),
            policy,
            true,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(in super::super) fn open(
        directory: &Path,
        target_cube: String,
        source: SplitSource,
        coins: Vec<SplitCoin>,
        fork_height: u64,
        context: Context,
        generation: watch::Receiver<u64>,
        services: Box<dyn SplitForkServices>,
        transport: Box<dyn UnifiedTransport>,
        policy: CheckPolicy,
        resume: bool,
    ) -> Result<Self, Error> {
        if !policy.valid()
            || *generation.borrow() != context.generation
            || generation.has_changed().is_err()
            || transport.origin() != services.origin()
            || fork_height == 0
        {
            return Err(Error::InvalidBinding);
        }
        let claimed = coin_addresses(&coins).ok_or(Error::InvalidBinding)?;
        if claimed.is_empty()
            || claimed
                .iter()
                .map(|(o, _)| *o)
                .collect::<BTreeSet<_>>()
                .len()
                != claimed.len()
        {
            return Err(Error::InvalidBinding);
        }
        let mut target = Target::default();
        let controller = if resume {
            let identity = claim_workflow::split_identity(target_cube.clone(), source.digest());
            let controller =
                Controller::reopen_settling_blocking(directory, &identity, context.clone())?;
            if controller.recorded_fork_submission().is_some() {
                return Err(Error::SubmissionAlreadyRecorded);
            }
            let record = controller
                .recorded_split()?
                .filter(|record| record.kind == claim_workflow::SplitKind::Unified)
                .ok_or(Error::InvalidBinding)?;
            let (Some(index), Some(script)) = (record.target_index, record.target_script) else {
                return Err(Error::InvalidBinding);
            };
            let child = ChildNumber::from_normal_idx(index).map_err(|_| Error::InvalidBinding)?;
            if record.fork_height != fork_height
                || receive_script(transport.descriptor(), child) != script
            {
                return Err(Error::InvalidBinding);
            }
            target.reserved = Some((index, script));
            Some(controller)
        } else {
            None
        };
        Ok(Self {
            id: NEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .map_err(|_| Error::Revoked)?,
            revision: 0,
            context,
            generation,
            directory: directory.to_owned(),
            target_cube,
            source,
            coins,
            claimed,
            fork_height,
            controller,
            services,
            transport,
            policy,
            revoker: Revoker::new(),
            target,
            sweep: None,
            verified: None,
            #[cfg(test)]
            clock_skew: Duration::ZERO,
        })
    }
    #[cfg(not(test))]
    fn clock_skew(&self) -> Duration {
        Duration::ZERO
    }
    #[cfg(test)]
    fn clock_skew(&self) -> Duration {
        self.clock_skew
    }
    /// Test-only: move the clock confirmation's expiry check reads ahead.
    #[cfg(test)]
    pub(in super::super) fn skew_clock_for_test(&mut self, by: Duration) {
        self.clock_skew = by;
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
    pub fn revoker(&self) -> Revoker {
        self.revoker.clone()
    }
    /// The route the review is labelled with (`SubmissionRoute::label`).
    pub fn route(&self) -> SubmissionRoute {
        self.transport.route()
    }
    /// The reserved target index, if any.
    pub fn target_index(&self) -> Option<u32> {
        self.target.reserved.as_ref().map(|(index, _)| *index)
    }
    /// The verified signed sweep, kept across refused reviews.
    pub fn transaction(&self) -> Option<&Transaction> {
        self.verified
            .as_deref()
            .map(VerifiedUnifiedSweep::transaction)
    }
    /// The recorded possible submission: what to reconcile, never resend.
    pub fn recorded_outcome(&self) -> Option<Outcome> {
        self.controller
            .as_ref()?
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
            if let Some(controller) = self.controller.as_mut() {
                controller.invalidate();
            }
            return Err(Error::Revoked);
        }
        Ok(())
    }

    /// Reserve the target: one receive address from the target Vault daemon
    /// (`reserve`, its `get_new_address`) within `bound`, checked to be the
    /// transport's Vault's own receive derivation. Refused while a
    /// reservation is held and not proven used, and once a journal records
    /// the target.
    pub async fn reserve_target<F>(
        &mut self,
        context: &Context,
        reserve: F,
        bound: Duration,
    ) -> Result<u32, TargetError>
    where
        F: std::future::Future<Output = Result<GetAddressResult, DaemonError>> + Send + 'static,
    {
        self.current(context)?;
        let replacing = match &self.target.reserved {
            None => None,
            Some((index, _)) if self.controller.is_none() && self.target.used == Some(*index) => {
                Some(*index)
            }
            Some(_) => return Err(TargetError::AlreadyReserved),
        };
        let reserved = match tokio::time::timeout(bound, tokio::spawn(reserve)).await {
            Ok(Ok(Ok(reserved))) => reserved,
            _ => return Err(TargetError::ReservationUnavailable),
        };
        self.current(context)?;
        let index = reserved.derivation_index;
        if index.is_hardened() {
            return Err(TargetError::NotTargetVault);
        }
        let script = receive_script(self.transport.descriptor(), index);
        if reserved.address.script_pubkey() != script {
            return Err(TargetError::NotTargetVault);
        }
        let index = u32::from(index);
        // A replacement takes a strictly higher index, never the used one.
        if replacing.is_some_and(|used| index <= used) {
            return Err(TargetError::ReservationUnavailable);
        }
        self.target = Target {
            reserved: Some((index, script)),
            used: None,
            proven_at: None,
        };
        self.sweep = None;
        self.verified = None;
        Ok(index)
    }

    /// Prove the reserved target belongs to the transport's Vault and that
    /// Connect shows no history for it on BTCB2 or Bitcoin. A used address
    /// marks its index used, so [`Self::reserve_target`] may replace it.
    pub async fn prove_target(&mut self, context: &Context) -> Result<(), TargetError> {
        self.current(context)?;
        self.target.proven_at = None;
        let proven = self.target_unused().await?;
        self.current(context)?;
        self.target.proven_at = Some(proven);
        Ok(())
    }

    /// The reserved target's freshness on both chains; returns the oldest
    /// read's stamp. A used target is marked used.
    async fn target_unused(&mut self) -> Result<i64, TargetError> {
        let (index, script) = self
            .target
            .reserved
            .clone()
            .ok_or(TargetError::NoReservation)?;
        let child = ChildNumber::from_normal_idx(index).map_err(|_| TargetError::NotTargetVault)?;
        if receive_script(self.transport.descriptor(), child) != script {
            return Err(TargetError::NotTargetVault);
        }
        let address = Address::from_script(&script, Network::Bitcoin)
            .map_err(|_| TargetError::NotTargetVault)?
            .to_string();
        let mut oldest = self.services.source().now();
        for chain in [FORK_CHAIN, ChainId::Bitcoin] {
            let read = self
                .services
                .address_used(chain, &address)
                .await
                .map_err(|kind| TargetError::Unavailable(chain, kind))?;
            if !self.fresh(read.observed_at()) {
                return Err(TargetError::Unavailable(chain, FailureKind::Stale));
            }
            if *read.value() {
                self.target.used = Some(index);
                self.target.proven_at = None;
                return Err(TargetError::Used(chain));
            }
            oldest = oldest.min(read.observed_at());
        }
        Ok(oldest)
    }
    fn fresh(&self, stamp: i64) -> bool {
        let now = self.services.source().now();
        stamp >= 0
            && now.checked_sub(stamp).is_some_and(|age| {
                (0..=self.policy.observations.max_observation_age_seconds).contains(&age)
            })
    }
    /// D10: the anchor names the fork height the coins were authenticated
    /// against.
    fn check_fork_height(&self, anchor: u64) -> Result<(), UnifiedError> {
        if anchor != self.fork_height {
            return Err(UnifiedError::ForkHeightChanged {
                authenticated: self.fork_height,
                anchor,
            });
        }
        Ok(())
    }

    /// Build the unsigned unified sweep; see the module documentation.
    /// Returns the PSBT to sign. A reopened record is rebuilt exactly and
    /// revalidated, so it needs no fee.
    pub async fn build(
        &mut self,
        context: &Context,
        fees: &dyn SweepFeeSource,
    ) -> Result<Psbt, UnifiedError> {
        self.current(context)?;
        self.sweep = None;
        self.verified = None;
        let (index, target) = self
            .target
            .reserved
            .clone()
            .ok_or(UnifiedError::TargetNotProven)?;
        if !self.target.proven_at.is_some_and(|stamp| self.fresh(stamp)) {
            return Err(UnifiedError::TargetNotProven);
        }
        let recorded = self
            .controller
            .as_ref()
            .and_then(|c| c.recorded_fork_sweep().cloned());
        let feerate = match recorded {
            Some(_) => None,
            None => Some(
                btcb2_sweep_feerate(fees)
                    .await
                    .ok_or(UnifiedError::FeeUnavailable)?,
            ),
        };
        self.current(context)?;
        let anchor = fork_anchor(self.services.source(), FORK_CHAIN, self.policy.observations)
            .await
            .map_err(Error::Observation)?;
        self.current(context)?;
        self.check_fork_height(anchor.fork_height)?;
        let tip = u32::try_from(anchor.tip.height)
            .map_err(|_| Error::Observation(observation_failure()))?;
        let locktime =
            LockTime::from_height(tip).map_err(|_| Error::Observation(observation_failure()))?;
        let (source, coins, fork_height) =
            (self.source.clone(), self.coins.clone(), self.fork_height);
        let sweep = tokio::task::spawn_blocking(move || {
            let inputs = UnifiedInputs {
                chain: FORK_CHAIN,
                source: &source,
                coins: &coins,
                fork_height,
                target: &target,
            };
            match (recorded, feerate) {
                (Some(recorded), _) => reconstruct_unified_sweep(&inputs, &recorded, tip),
                (None, Some(feerate)) => create_unified_sweep(&inputs, feerate, locktime, tip),
                (None, None) => unreachable!("a fee is resolved whenever none is recorded"),
            }
        })
        .await
        .map_err(|_| Error::Revoked)?
        .map_err(UnifiedError::Construction)?;
        self.current(context)?;
        if let Some(controller) = self.controller.as_mut() {
            controller.revalidate_unified_construction(context, &sweep, index)?;
        }
        let psbt = sweep.psbt().clone();
        self.sweep = Some(Arc::new(sweep));
        Ok(psbt)
    }

    /// Verify the signed PSBT against the sweep built here (core's unified
    /// finalizer: every input `ALL|UNIFIED`, Protected) and keep it for
    /// review. CPU-bound (signature verification); call it off the UI
    /// thread.
    pub fn verify_signed(
        &mut self,
        context: &Context,
        signed: &UnifiedPsbt,
    ) -> Result<UnifiedReplayStatus, UnifiedError> {
        self.current(context)?;
        self.verified = None;
        let sweep = self.sweep.as_ref().ok_or(UnifiedError::NotBuilt)?;
        let verified = finalize_unified_sweep(
            sweep,
            signed,
            &coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only(),
        )
        .map_err(UnifiedError::Finalize)?;
        let status = verified.replay_status();
        self.verified = Some(Arc::new(verified));
        Ok(status)
    }

    async fn collect(&self, txid: Txid) -> Result<ForkSweepObservation, Error> {
        collect_fork_sweep(
            self.services.source(),
            FORK_CHAIN,
            txid,
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
    /// The sweep is absent from BTCB2 and the anchor names the fork height.
    fn check_view(&self, view: &ForkSweepObservation) -> Result<(), UnifiedError> {
        if view.transaction() != TransactionObservation::Absent {
            return Err(UnifiedError::SweepSeen(view.transaction()));
        }
        self.check_fork_height(view.anchor().fork_height)
    }

    async fn fresh_snapshot(
        &mut self,
        context: &Context,
    ) -> Result<UnifiedReviewSnapshot, UnifiedError> {
        self.current(context)?;
        if self.recorded_outcome().is_some() {
            return Err(Error::SubmissionAlreadyRecorded.into());
        }
        let verified = self.verified.clone().ok_or(UnifiedError::NotSigned)?;
        let target_index = self.target_index().ok_or(UnifiedError::TargetNotProven)?;
        let tx = verified.transaction().clone();
        let (txid, wtxid) = (tx.compute_txid(), tx.compute_wtxid());
        let first = self.collect(txid).await?;
        self.check_view(&first)?;
        let unspent_at =
            claimed_unspent_on_fork(self.services.as_ref(), &self.claimed, self.policy).await?;
        let target_at = self.target_unused().await?;
        let evidence = self
            .transport
            .preflight(&tx, first.anchor().tip.hash, self.policy.preflight)
            .await
            .map_err(Error::Preflight)?;
        let last = self.collect(txid).await?;
        self.current(context)?;
        if first.anchor().tip != last.anchor().tip
            || first.anchor().median_time_past != last.anchor().median_time_past
            || first.anchor().deployment != last.anchor().deployment
            || first.transaction() != last.transaction()
        {
            return Err(Error::ChangedReview.into());
        }
        self.check_view(&last)?;
        let chain_ok = match &evidence {
            RoutedEvidence::Connect(evidence) => evidence.chain() == FORK_CHAIN,
            // The node's best block was the BTCB2 tip observed via Connect.
            RoutedEvidence::BitcoinNode(..) => true,
        };
        if !chain_ok
            || evidence.txid() != txid
            || evidence.wtxid() != wtxid
            || evidence.tip() != last.anchor().tip.hash
            || evidence.generation() != context.generation
        {
            return Err(Error::InvalidBinding.into());
        }
        if evidence.node_policy() != &NodePolicy::Accepted {
            return Err(Error::PolicyRejected(evidence.node_policy().clone()).into());
        }
        let not_after = review_deadline(
            self.policy,
            [
                first.observed_at(),
                last.observed_at(),
                unspent_at,
                target_at,
            ],
            evidence.observed_at(),
            self.services.source().now(),
            Instant::now(),
        )?;
        Ok(UnifiedReviewSnapshot {
            transaction: tx.clone(),
            txid,
            wtxid,
            fee_sats: verified.fee().to_sat(),
            vsize: verified.vsize(),
            fork_tip: last.anchor().tip,
            target_index,
            route: evidence.route(),
            replay: verified.replay_status(),
            not_after,
        })
    }

    /// A fresh review of the verified sweep (the C2 gate). A refused
    /// preflight returns `PolicyRejected`, creates no journal and keeps the
    /// verified sweep.
    pub async fn prepare_review(
        &mut self,
        context: &Context,
    ) -> Result<UnifiedReview, UnifiedError> {
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        let snapshot = self.fresh_snapshot(context).await?;
        Ok(UnifiedReview {
            coordinator: self.id,
            revision: self.revision,
            snapshot,
        })
    }

    /// Explicit user confirmation of this one-use view. The review is
    /// checked again on the same route; then the journal is created (U3) and
    /// the submission intent recorded under its lock before the one send.
    /// Anything after the intent can only be reconciled (U4).
    pub async fn confirm_and_submit(
        &mut self,
        review: UnifiedReview,
        context: &Context,
    ) -> Result<Outcome, UnifiedError> {
        self.current(context)?;
        if review.coordinator != self.id || review.revision != self.revision {
            return Err(Error::InvalidReview.into());
        }
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        let refreshed = self.fresh_snapshot(context).await?;
        if review.snapshot.txid != refreshed.txid
            || review.snapshot.wtxid != refreshed.wtxid
            || review.snapshot.route != refreshed.route
            || review.snapshot.fork_tip != refreshed.fork_tip
            || review.snapshot.target_index != refreshed.target_index
        {
            return Err(Error::ChangedReview.into());
        }
        self.current(context)?;
        if Instant::now() + self.clock_skew() >= refreshed.not_after {
            return Err(Error::ExpiredEvidence.into());
        }
        let sweep = self.sweep.clone().ok_or(UnifiedError::NotBuilt)?;
        let verified = self.verified.clone().ok_or(UnifiedError::NotSigned)?;
        let controller = match self.controller.take() {
            Some(controller) => controller,
            None => Controller::create_unified_split(
                &self.directory,
                self.target_cube.clone(),
                &sweep,
                refreshed.target_index,
                self.context.clone(),
            )?,
        };
        let controller = self.controller.insert(controller);
        controller.record_unified_broadcast_intent(context, &verified)?;
        Ok(self.send_recorded(context, refreshed, verified).await)
    }

    /// The one send of the attempt the journal has just recorded, on the
    /// reviewed route under its deadline. Anything but the route's exact
    /// acceptance is `Uncertain`; nothing here retries, and nothing records
    /// a return: the unified sweep is never resent (U4).
    async fn send_recorded(
        &mut self,
        context: &Context,
        refreshed: UnifiedReviewSnapshot,
        verified: Arc<VerifiedUnifiedSweep>,
    ) -> Outcome {
        let uncertain = Outcome::Uncertain {
            txid: refreshed.txid,
            wtxid: refreshed.wtxid,
        };
        let Ok(target) = ChildNumber::from_normal_idx(refreshed.target_index) else {
            return uncertain;
        };
        let (gate, revoker) = SubmissionGate::for_unified_sweep(&verified, refreshed.not_after);
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
        let submit = self
            .transport
            .submit(refreshed.route, verified, target, Arc::new(gate));
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
            _ => uncertain,
        }
    }

    /// After the submission: the recorded sweep on BTCB2. Never resends.
    pub async fn reconcile_sweep(&mut self, context: &Context) -> Result<UnifiedReconcile, Error> {
        self.current(context)?;
        let controller = self.controller.as_mut().ok_or(Error::InvalidBinding)?;
        reconcile_unified(
            controller,
            self.services.as_ref(),
            self.policy,
            context,
            &self.generation,
        )
        .await
    }
}
impl Drop for UnifiedCoordinator {
    fn drop(&mut self) {
        self.revoker.revoke();
    }
}

fn observation_failure() -> claim_observation::Failure {
    claim_observation::Failure {
        stage: claim_observation::Stage::ForkAnchor,
        kind: FailureKind::Malformed,
    }
}

/// Each coin's outpoint and the address its output pays, from its
/// txid-bound previous transaction. `None` if any coin has none.
fn coin_addresses(coins: &[SplitCoin]) -> Option<Vec<(OutPoint, String)>> {
    coins
        .iter()
        .map(|coin| {
            if coin.previous.compute_txid() != coin.outpoint.txid {
                return None;
            }
            let output = coin
                .previous
                .output
                .get(usize::try_from(coin.outpoint.vout).ok()?)?;
            let address = Address::from_script(&output.script_pubkey, Network::Bitcoin).ok()?;
            Some((coin.outpoint, address.to_string()))
        })
        .collect()
}

/// The review's deadline: the collection budget (at most 30 s), shortened
/// so that no read it rests on outlives its maximum age. UNIX-second stamps
/// have a second of quantization, which is subtracted; a preflight stamp
/// within its future skew never extends the budget.
fn review_deadline(
    policy: CheckPolicy,
    reads: [i64; 4],
    preflight_at: i64,
    now: i64,
    origin: Instant,
) -> Result<Instant, Error> {
    if now < 0 {
        return Err(Error::ExpiredEvidence);
    }
    let mut remaining = policy.collection_budget.min(Duration::from_secs(30));
    let max_age = policy.observations.max_observation_age_seconds;
    for (stamp, max_age, skew) in reads.iter().map(|stamp| (*stamp, max_age, 0)).chain([(
        preflight_at,
        policy.preflight.max_age_seconds,
        policy.preflight.max_future_skew_seconds,
    )]) {
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

/// One fork-only collection of the recorded sweep, keyed by its signed txid.
/// A read that saw it (mempool or block) is recorded as observed, even when
/// the collection it belongs to then failed: the sweep left. This never
/// resends or authorizes anything.
async fn reconcile_unified(
    controller: &mut Controller,
    services: &dyn SplitForkServices,
    policy: CheckPolicy,
    context: &Context,
    generation: &watch::Receiver<u64>,
) -> Result<UnifiedReconcile, Error> {
    let txid = recorded_submission(controller)?;
    let fork = controller.plan().fork_chain;
    let probe = SightingProbe::new(services.source(), fork, txid);
    let collected = collect_fork_sweep(
        &probe,
        fork,
        txid,
        policy.observations,
        policy.collection_budget,
        CollectionContext {
            expected_generation: context.generation,
            generation: generation.clone(),
        },
    )
    .await;
    if *generation.borrow() != context.generation || generation.has_changed().is_err() {
        controller.invalidate();
        return Err(Error::Revoked);
    }
    let seen = match &collected {
        Ok(view) if view.transaction() != TransactionObservation::Absent => {
            Some(view.transaction())
        }
        _ if probe.sighted() => Some(TransactionObservation::Unconfirmed { txid }),
        _ => None,
    };
    if let Some(seen) = seen {
        controller.record_split_step2_observed(context, seen)?;
    }
    let collected = collected.map_err(Error::Observation)?;
    Ok(UnifiedReconcile {
        sweep: collected.transaction(),
        fork_tip: collected.anchor().tip,
    })
}

/// Restart after a recorded unified submission: owns the fork-only journal
/// and can only reconcile. It needs no construction, coins or signatures,
/// holds no transport, and so cannot send anything again (U4).
pub struct UnifiedReconciler {
    context: Context,
    generation: watch::Receiver<u64>,
    controller: Controller,
    services: Box<dyn SplitForkServices>,
    policy: CheckPolicy,
    revoker: Revoker,
}
impl UnifiedReconciler {
    /// Reopen the fork-only journal of `source_digest` under `target_cube`.
    /// Refused unless it is a fork-only record whose signed sweep and
    /// submission are recorded.
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
    pub(in super::super) fn open(
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
        if controller
            .recorded_split()?
            .is_none_or(|record| record.kind != claim_workflow::SplitKind::Unified)
        {
            return Err(Error::InvalidBinding);
        }
        recorded_submission(&controller)?;
        Ok(Self {
            context,
            generation,
            controller,
            services,
            policy,
            revoker: Revoker::new(),
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
    /// The recorded sweep on BTCB2; see [`reconcile_unified`].
    pub async fn reconcile_sweep(&mut self, context: &Context) -> Result<UnifiedReconcile, Error> {
        if self.revoker.is_revoked()
            || context != &self.context
            || *self.generation.borrow() != context.generation
            || self.generation.has_changed().is_err()
        {
            self.revoker.revoke();
            self.controller.invalidate();
            return Err(Error::Revoked);
        }
        reconcile_unified(
            &mut self.controller,
            self.services.as_ref(),
            self.policy,
            context,
            &self.generation,
        )
        .await
    }
}
