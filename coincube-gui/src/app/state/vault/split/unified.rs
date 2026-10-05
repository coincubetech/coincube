//! The single-step (fork-only) route in the Split panel (#568 B4b-3c; owner
//! decisions U1-U7, C2-C6, P1, P7, P8). Like the rest of the panel it is
//! dormant (D1): the route is chosen only in a started panel, which nothing
//! in the GUI creates before B5c.
//!
//! - **Route choice.** A started panel offers the two-step split and, when
//!   the wallet's descriptors give it (`SigningRoutes::seed_unified`, U6:
//!   no `tr`, an origin on every ranged key), the single step signed with
//!   seeds. P7 applies to both (U7). A hardware wallet can't sign the single
//!   step (P1): it is never offered, and asking for it shows why.
//! - **Seeds.** Each seed and its passphrase are typed into `.secure(true)`
//!   inputs and held only in [`SeedText`], a zeroizing buffer with a
//!   redacted `Debug`. `SeedSet::add` runs once per seed, off the UI thread;
//!   the words and passphrase are moved out of the buffers into it, so the
//!   buffers are empty after every attempt. Signing (`SeedSet::sign_unified`,
//!   then the coordinator's finalizer) runs in one blocking
//!   task, and the set is cleared and dropped there whatever the result.
//!   Cancel, Close and every revocation clear the set, drop it and empty
//!   both buffers ([`UnifiedState::scrub`]).
//! - **Port.** [`UnifiedPort`], the panel's fourth port ([`ProductionUnified`]),
//!   opens the services of #654 for one Connect session: a
//!   `UnifiedCoordinator` (the C2 gate; the journal created only at
//!   confirmation, U3; one send, never a resend, U4) through the target
//!   Vault's daemon.
//! - **Review.** The Protected pill with `PROTECTED_LIMITATION`, the route
//!   label and, on the node route, a privacy note. #654 F2 (lead decision):
//!   the D4 fee is read again at the review, and a signed rate below it is
//!   refused before anything is journaled; the sweep is then built and
//!   signed again.
//! - **Not here (B4b-3c part 2):** restart by kind (reopening a fork-only
//!   record to reconcile it or back to seed entry) and the fork-only close
//!   (C6).
//!
//! Every Connect read, journal call and signature runs in a task.

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use iced::Task;
use tokio::sync::watch;
use zeroize::Zeroizing;

use coincube_core::{
    chain::ChainId,
    foreign_split::{SplitCoin, SplitSource, UnifiedReplayStatus},
    miniscript::bitcoin::{hashes::sha256, psbt::Psbt, secp256k1, Txid},
    psbt_unified::UnifiedPsbt,
    unified_foreign::ForeignUnifiedError,
};

use super::{
    step1::{self, Refusal, RevokeHandle, SplitConnect},
    step2::{self, describe_check, describe_target, PortIdentity, Step2Recovery, Step2Refusal},
    SplitEvent, SplitPanel, Stage, Work,
};
use crate::{
    app::{
        message::Message,
        split_intent::SplitIntent,
        state::vault::{
            claim::{ConnectSession, CHECK_POLICY},
            replay,
        },
    },
    daemon::Daemon,
    services::{
        claim_coordinator::{
            self,
            fork::split::{
                step2::{
                    SplitStep2Production, TargetError, UnifiedCoordinator, UnifiedError,
                    UnifiedReview, RESERVATION_BOUND,
                },
                SplitForkProduction,
            },
            Outcome, SubmissionRoute,
        },
        claim_observation::TransactionObservation,
        claim_workflow::{self, Context},
        foreign_psbt::{btcb2_sweep_feerate, SweepFeeSource},
        foreign_scan::SigningRoutes,
        foreign_split_inventory::FreshIndex,
        split_evidence::{authenticate_outpoints, RecordedOutpoint, MAX_EVIDENCE_AGE_SECONDS},
        split_fees,
        split_seed::{SeedSet, SeedSetError},
        split_source::split_source,
    },
};

/// The route choice's two offers.
pub const ROUTE_TWO_STEP: &str = "Two steps: step 1 on Bitcoin, then step 2 on Bitcoin Blake2b. Sign with PSBT files or a connected hardware wallet.";
pub const ROUTE_SEEDS: &str = "One step on Bitcoin Blake2b only, signed here with this wallet's recovery phrases. Its signatures are invalid on Bitcoin, so it can't be replayed there. The phrases stay in memory for this sweep only and are never saved.";
/// P1: the single step is never offered for a hardware wallet.
pub const P1_HARDWARE: &str = "A hardware wallet can't sign the single-step sweep: it needs a Bitcoin Blake2b-only signature type that devices don't produce. To sign with a device, use the two-step split.";
/// U6: the wallet's descriptors give no seed route.
pub const SEEDS_NOT_OFFERED: &str = "This wallet's recovery phrases can't be matched to its keys: every key needs its origin, and Taproot wallets can't be signed here. Use the two-step split.";
/// No unified port, or one without a usable Vault daemon.
pub const UNIFIED_NEEDS_VAULT: &str = "The single-step sweep needs this Vault's wallet engine running on a route the sweep can be sent through: Connect's Bitcoin Blake2b server or this Vault's own Bitcoin Blake2b node. Nothing was built.";
/// #654 F2: no D4 fee at the review.
pub const FEE_UNAVAILABLE_AT_REVIEW: &str = "Connect has no Bitcoin Blake2b fee estimate right now, so the signed sweep's fee can't be checked. Nothing was recorded or sent; review it again shortly.";
/// #654 F2: the signed rate is below the fresh D4 estimate.
pub const FEE_BELOW_ESTIMATE: &str = "The signed sweep pays less than Connect's current Bitcoin Blake2b fee estimate, so it might not confirm. Nothing was recorded or sent. Enter the recovery phrases again to build and sign it at the current fee.";
/// The node route's privacy note on the review.
pub const UNIFIED_NODE_PRIVACY: &str = "The sweep will be sent through this Vault's own Bitcoin node. That node, which may be a remote one you configured, learns the transaction and this computer's network address before it relays it.";
/// Text typed into a `.secure(true)` seed input: zeroized when replaced or
/// dropped, never printed.
#[derive(Clone, Default)]
pub struct SeedText(Zeroizing<String>);
impl SeedText {
    /// The typed text, lent to the view's one `.secure(true)` input and to
    /// nothing else (`split_unified_holds_seeds_only_zeroized`).
    pub(in crate::app) fn expose_for_secure_input(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Move the text out, leaving the buffer empty.
    fn take(&mut self) -> Zeroizing<String> {
        std::mem::take(&mut self.0)
    }
    /// Empty the buffer; the old bytes are zeroized as they drop.
    fn clear(&mut self) {
        self.0 = Zeroizing::default();
    }
}
impl From<String> for SeedText {
    fn from(text: String) -> Self {
        Self(Zeroizing::new(text))
    }
}
impl fmt::Debug for SeedText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SeedText(<redacted>)")
    }
}

/// The routes a started panel offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    TwoStep,
    Seeds,
    /// P1: never offered; asking for it shows why.
    Hardware,
}

/// Where the single-step route is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnifiedStage {
    /// Enter the seeds the wallet's threshold needs, then build and sign.
    EnterSeeds,
    /// Built and signed, Protected: review on request.
    Signed,
    /// A review is on screen; confirming submits exactly it.
    Review,
    /// A submission may exist: reconcile only.
    Submitted,
}

/// Single-step intents, inside [`super::SplitMessage::Unified`].
#[derive(Debug, Clone)]
pub enum UnifiedMessage {
    Choose(Route),
    Words(SeedText),
    Passphrase(SeedText),
    AddSeed,
    ClearSeeds,
    BuildAndSign,
    Review,
    Confirm,
    Reconcile,
    /// Leave the single step: clear the seeds and drop the route.
    Cancel,
}

/// What the review screen shows. A display copy only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepReviewView {
    pub txid: Txid,
    pub fee_sats: u64,
    pub vsize: usize,
    pub route_label: &'static str,
    pub privacy_note: Option<&'static str>,
    /// The Protected pill and its limitation.
    pub protected: String,
    pub limitation: &'static str,
}

/// The single-step coordinator, through its driver.
#[async_trait]
pub trait UnifiedFlow: Send {
    fn revoke_handle(&self) -> RevokeHandle;
    fn recorded_outcome(&self) -> Option<Outcome>;
    /// Reserve (if needed) and prove the target, then build the sweep.
    async fn build(&mut self, context: &Context) -> Result<(), Step2Refusal>;
    /// Sign the built sweep with `seeds` and verify it Protected. CPU-bound:
    /// callers use `spawn_blocking`.
    fn sign(&mut self, context: &Context, seeds: &SeedSet) -> Result<(), Step2Refusal>;
    async fn review(&mut self, context: &Context) -> Result<SweepReviewView, Step2Refusal>;
    /// Submit exactly what the last review showed.
    async fn submit(&mut self, context: &Context) -> Result<Outcome, Step2Refusal>;
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<TransactionObservation, Step2Refusal>;
}

/// Everything a single-step coordinator is opened with.
pub struct UnifiedOpen {
    pub directory: PathBuf,
    pub target_cube: String,
    pub source: SplitSource,
    /// Freshly authenticated against `fork_height`.
    pub coins: Vec<SplitCoin>,
    pub fork_height: u64,
}

/// The panel's fourth port: the single-step services for one Connect
/// session, the coordinator through the target Vault's daemon.
pub trait UnifiedPort: Send + Sync {
    fn context(&self) -> Context;
    /// As [`step2::Step2Port::identity`]; no daemon reads as 0.
    fn identity(&self) -> PortIdentity;
    /// A new coordinator. Blocking: callers use `spawn_blocking`.
    fn open(&self, open: UnifiedOpen) -> Result<Box<dyn UnifiedFlow>, Step2Refusal>;
}

/// Copy for a seed refusal. None names a fingerprint, and none repeats what
/// was typed.
pub fn describe_seed(error: &SeedSetError) -> String {
    match error {
        SeedSetError::UnsupportedPolicy => SEEDS_NOT_OFFERED.to_string(),
        SeedSetError::Seed(_) => "That is not a valid recovery phrase. Check the words and try again; nothing was kept.".to_string(),
        SeedSetError::UnknownOrigin => "This recovery phrase, with this passphrase, is not one of this wallet's keys. Check the words and the passphrase; nothing was kept.".to_string(),
        SeedSetError::Duplicate => "This recovery phrase is already entered.".to_string(),
        SeedSetError::Full { threshold } => {
            format!("All {threshold} recovery phrase(s) this wallet needs are entered.")
        }
        SeedSetError::Incomplete { have, need } => {
            format!("{have} of {need} recovery phrases entered. Enter the rest to sign.")
        }
        // #647 O3: a phrase can match a key's origin without deriving that
        // key (a wrong passphrase, or a collision).
        SeedSetError::Signing(ForeignUnifiedError::DerivedPublicKeyMismatch { .. }) => "A recovery phrase entered doesn't derive this wallet's key at its path, which a wrong passphrase can cause. Nothing was signed or sent. Clear the seeds and enter them again.".to_string(),
        SeedSetError::Signing(error) => {
            format!("Signing was refused ({error}). Nothing was signed or sent. Clear the seeds and enter them again.")
        }
    }
}

/// Copy for a refused single-step operation.
pub fn describe_unified(error: UnifiedError) -> Step2Refusal {
    match error {
        UnifiedError::Coordinator(error) => describe_check(error),
        UnifiedError::Target(error) => describe_target(error),
        UnifiedError::FeeUnavailable => Step2Refusal::retry(
            "Connect has no Bitcoin Blake2b fee estimate right now, so the sweep can't be priced. Nothing was built; try again shortly.",
        ),
        UnifiedError::TargetNotProven => Step2Refusal {
            reason: "The fresh address proof expired before the sweep was built. Nothing was built; try again.".to_string(),
            retry: true,
            recovery: Step2Recovery::RefreshTarget,
        },
        UnifiedError::NotBuilt | UnifiedError::NotSigned => Step2Refusal {
            reason: "The sweep must be built and signed again. Nothing was sent.".to_string(),
            retry: true,
            recovery: Step2Recovery::RefreshTarget,
        },
        UnifiedError::ForkHeightChanged { .. } => Step2Refusal::final_(step1::STALE_ANCHOR),
        UnifiedError::CoinSpent(outpoint) => Step2Refusal::final_(format!(
            "A coin of this split ({outpoint}) is no longer unspent on Bitcoin Blake2b, so the sweep can't spend it. Nothing was sent."
        )),
        UnifiedError::Unavailable(_, kind) => Step2Refusal::retry(format!(
            "Connect couldn't read Bitcoin Blake2b's unspent coins for this split ({kind:?}). This is a Connect or indexer limit, not a sign a coin was spent. Nothing was sent; try again later."
        )),
        UnifiedError::SweepSeen(_) => Step2Refusal::final_(
            "Bitcoin Blake2b already shows this sweep, though no submission of it is recorded here. Nothing was sent.",
        ),
        UnifiedError::Construction(error) => Step2Refusal::final_(format!(
            "Split couldn't build the sweep ({error}). Nothing was signed or sent."
        )),
        UnifiedError::Finalize(error) => Step2Refusal {
            reason: format!(
                "The signatures don't complete the sweep ({error:?}). Nothing was sent. Clear the seeds and enter them again."
            ),
            retry: true,
            recovery: Step2Recovery::RefreshTarget,
        },
    }
}

/// The routes the wallet's descriptors give (#653 `SigningRoutes`, read,
/// not recomputed): both branches must give a route.
pub fn intent_routes(intent: &SplitIntent) -> SigningRoutes {
    let external = intent.external.capabilities().signing;
    let Some(internal) = intent.internal.as_ref().map(|i| i.capabilities().signing) else {
        return external;
    };
    SigningRoutes {
        psbt_file: external.psbt_file && internal.psbt_file,
        in_app_hardware: external.in_app_hardware && internal.in_app_hardware,
        seed_unified: external.seed_unified && internal.seed_unified,
    }
}

/// The checks before a single-step coordinator opens: the seed route (U6),
/// P7 (U7), pre-fork coins, the fork height, and every coin authenticated
/// afresh on both chains.
pub async fn preconditions(
    connect: &dyn SplitConnect,
    intent: &SplitIntent,
    journal_root: &Path,
    target_cube: String,
) -> Result<UnifiedOpen, Refusal> {
    if !intent_routes(intent).seed_unified {
        return Err(Refusal::final_(SEEDS_NOT_OFFERED));
    }
    let source = split_source(&intent.external, intent.internal.as_ref())
        .map_err(|error| Refusal::final_(error.to_string()))?;
    let inventory = &intent.inventory;
    match inventory.fresh_receive() {
        FreshIndex::Proven(_) => {}
        FreshIndex::FixedDescriptor => return Err(Refusal::final_(step1::FIXED_WALLET)),
        FreshIndex::NotProven => return Err(Refusal::final_(step1::WATCH_ONLY_DEFERRED)),
    }
    let coins = inventory.splittable_coins();
    if coins.is_empty() {
        return Err(Refusal::final_(step1::NO_PRE_FORK_COINS));
    }
    let window = connect.window().await.map_err(|reason| {
        Refusal::retry(format!(
            "Couldn't read Bitcoin Blake2b's status from Connect ({reason})."
        ))
    })?;
    if window.fork_height != inventory.fork_height() {
        return Err(Refusal::final_(step1::STALE_ANCHOR));
    }
    let recorded: Vec<_> = coins
        .iter()
        .map(|coin| RecordedOutpoint {
            outpoint: coin.outpoint,
            branch: coin.branch,
            index: coin.index,
        })
        .collect();
    let authenticated = authenticate_outpoints(
        connect.evidence(),
        &recorded,
        window.fork_height,
        MAX_EVIDENCE_AGE_SECONDS,
    )
    .await
    .map_err(step1::evidence_refusal)?;
    Ok(UnifiedOpen {
        directory: step1::journal_directory(journal_root, source.digest()),
        target_cube,
        source,
        coins: authenticated.coins,
        fork_height: window.fork_height,
    })
}

/// What the driver needs from a single-step coordinator: the production
/// one is [`LiveCore`]; tests substitute a fake to pin the driver's own
/// logic (target replacement, the D4 re-read, the Protected review).
#[async_trait]
pub(super) trait UnifiedCore: Send {
    fn revoke_handle(&self) -> RevokeHandle;
    fn target_index(&self) -> Option<u32>;
    fn recorded_outcome(&self) -> Option<Outcome>;
    async fn reserve(&mut self, context: &Context) -> Result<u32, TargetError>;
    async fn prove(&mut self, context: &Context) -> Result<(), TargetError>;
    async fn build(&mut self, context: &Context) -> Result<Psbt, UnifiedError>;
    fn verify_signed(
        &mut self,
        context: &Context,
        signed: &UnifiedPsbt,
    ) -> Result<UnifiedReplayStatus, UnifiedError>;
    /// A fresh review, kept for [`Self::confirm`].
    async fn review(&mut self, context: &Context) -> Result<ReviewFacts, UnifiedError>;
    fn drop_review(&mut self);
    async fn confirm(&mut self, context: &Context) -> Result<Outcome, UnifiedError>;
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<TransactionObservation, UnifiedError>;
    /// The D4 fee, read fresh.
    async fn feerate(&self) -> Option<u64>;
}

/// What a review found.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReviewFacts {
    pub txid: Txid,
    pub fee_sats: u64,
    pub vsize: usize,
    pub route: SubmissionRoute,
    pub replay: UnifiedReplayStatus,
}

/// The production core: the coordinator, the daemon it reserves through and
/// the D4 fee source.
struct LiveCore {
    coordinator: UnifiedCoordinator,
    daemon: Arc<dyn Daemon + Send + Sync>,
    fees: Arc<dyn SweepFeeSource>,
    review: Option<UnifiedReview>,
}
#[async_trait]
impl UnifiedCore for LiveCore {
    fn revoke_handle(&self) -> RevokeHandle {
        let revoker = self.coordinator.revoker();
        Arc::new(move || revoker.revoke())
    }
    fn target_index(&self) -> Option<u32> {
        self.coordinator.target_index()
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        self.coordinator.recorded_outcome()
    }
    async fn reserve(&mut self, context: &Context) -> Result<u32, TargetError> {
        let daemon = self.daemon.clone();
        self.coordinator
            .reserve_target(
                context,
                async move { daemon.get_new_address().await },
                RESERVATION_BOUND,
            )
            .await
    }
    async fn prove(&mut self, context: &Context) -> Result<(), TargetError> {
        self.coordinator.prove_target(context).await
    }
    async fn build(&mut self, context: &Context) -> Result<Psbt, UnifiedError> {
        self.review = None;
        self.coordinator.build(context, &*self.fees).await
    }
    fn verify_signed(
        &mut self,
        context: &Context,
        signed: &UnifiedPsbt,
    ) -> Result<UnifiedReplayStatus, UnifiedError> {
        self.review = None;
        self.coordinator.verify_signed(context, signed)
    }
    async fn review(&mut self, context: &Context) -> Result<ReviewFacts, UnifiedError> {
        self.review = None;
        let review = self.coordinator.prepare_review(context).await?;
        let snapshot = review.snapshot();
        let facts = ReviewFacts {
            txid: snapshot.txid,
            fee_sats: snapshot.fee_sats,
            vsize: snapshot.vsize,
            route: snapshot.route,
            replay: snapshot.replay,
        };
        self.review = Some(review);
        Ok(facts)
    }
    fn drop_review(&mut self) {
        self.review = None;
    }
    async fn confirm(&mut self, context: &Context) -> Result<Outcome, UnifiedError> {
        let review = self.review.take().ok_or(UnifiedError::NotSigned)?;
        self.coordinator.confirm_and_submit(review, context).await
    }
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<TransactionObservation, UnifiedError> {
        self.review = None;
        Ok(self.coordinator.reconcile_sweep(context).await?.sweep)
    }
    async fn feerate(&self) -> Option<u64> {
        btcb2_sweep_feerate(&*self.fees).await
    }
}

/// The panel's single-step driver over a [`UnifiedCore`].
pub(super) struct UnifiedDriver<C> {
    core: C,
    /// The sweep built last, to sign.
    psbt: Option<Psbt>,
}
impl<C: UnifiedCore> UnifiedDriver<C> {
    pub(super) fn new(core: C) -> Self {
        Self { core, psbt: None }
    }
    /// Reserve when none is held, prove, and replace a target proven used
    /// exactly once; an unavailable or stale proof never replaces anything.
    async fn ensure_target(&mut self, context: &Context) -> Result<(), Step2Refusal> {
        if self.core.target_index().is_none() {
            self.core.reserve(context).await.map_err(describe_target)?;
        }
        match self.core.prove(context).await {
            Ok(()) => Ok(()),
            Err(TargetError::Used(_)) => {
                self.core.reserve(context).await.map_err(describe_target)?;
                self.core.prove(context).await.map_err(describe_target)
            }
            Err(error) => Err(describe_target(error)),
        }
    }
}
#[async_trait]
impl<C: UnifiedCore + 'static> UnifiedFlow for UnifiedDriver<C> {
    fn revoke_handle(&self) -> RevokeHandle {
        self.core.revoke_handle()
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        self.core.recorded_outcome()
    }
    async fn build(&mut self, context: &Context) -> Result<(), Step2Refusal> {
        self.psbt = None;
        self.ensure_target(context).await?;
        self.psbt = Some(self.core.build(context).await.map_err(describe_unified)?);
        Ok(())
    }
    fn sign(&mut self, context: &Context, seeds: &SeedSet) -> Result<(), Step2Refusal> {
        let psbt = self.psbt.clone().ok_or_else(|| {
            Step2Refusal::retry("Build the sweep again before signing. Nothing was signed.")
        })?;
        let unsigned = UnifiedPsbt::from_psbt(psbt).map_err(|error| {
            Step2Refusal::final_(format!(
                "The built sweep can't be signed here ({error:?}). Nothing was signed or sent."
            ))
        })?;
        let secp = secp256k1::Secp256k1::new();
        let signed = seeds
            .sign_unified(&unsigned, ChainId::BitcoinBlake2b, &secp)
            .map_err(|error| Step2Refusal {
                reason: describe_seed(&error),
                retry: true,
                recovery: Step2Recovery::RefreshTarget,
            })?;
        match self
            .core
            .verify_signed(context, &signed)
            .map_err(describe_unified)?
        {
            UnifiedReplayStatus::Protected => Ok(()),
        }
    }
    async fn review(&mut self, context: &Context) -> Result<SweepReviewView, Step2Refusal> {
        let facts = self.core.review(context).await.map_err(describe_unified)?;
        // #654 F2 (lead decision): the D4 fee again, before anything can be
        // journaled. A refusal drops the review, so nothing can confirm it.
        let Some(rate) = self.core.feerate().await else {
            self.core.drop_review();
            return Err(Step2Refusal::retry(FEE_UNAVAILABLE_AT_REVIEW));
        };
        let floor = u128::from(rate).saturating_mul(facts.vsize as u128);
        if u128::from(facts.fee_sats) < floor {
            self.core.drop_review();
            self.psbt = None;
            return Err(Step2Refusal {
                reason: FEE_BELOW_ESTIMATE.to_string(),
                retry: true,
                recovery: Step2Recovery::RefreshTarget,
            });
        }
        // Core's verified sweep is Protected by construction; the review
        // shows exactly that and nothing a replayable spend needs.
        let protected = match facts.replay {
            UnifiedReplayStatus::Protected => replay::ReplayStatus::Protected,
        };
        let privacy_note = match facts.route {
            SubmissionRoute::Connect => None,
            SubmissionRoute::BitcoinNode { .. } => Some(UNIFIED_NODE_PRIVACY),
        };
        Ok(SweepReviewView {
            txid: facts.txid,
            fee_sats: facts.fee_sats,
            vsize: facts.vsize,
            route_label: facts.route.label(),
            privacy_note,
            protected: replay::pill_copy(&protected, &[]).0,
            limitation: replay::PROTECTED_LIMITATION,
        })
    }
    async fn submit(&mut self, context: &Context) -> Result<Outcome, Step2Refusal> {
        self.core.confirm(context).await.map_err(describe_unified)
    }
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<TransactionObservation, Step2Refusal> {
        self.core.reconcile(context).await.map_err(describe_unified)
    }
}

/// The production unified port: one Connect session and the target
/// Vault's daemon on a route the sweep can be sent through (admitted again
/// at every open, as for step 2).
pub struct ProductionUnified {
    session: ConnectSession,
    generation: watch::Receiver<u64>,
    expected: u64,
    context: Context,
    daemon: Option<Arc<dyn Daemon + Send + Sync>>,
}
impl ProductionUnified {
    /// Refused without an account, for an unusable origin, or after the
    /// generation moved. Without a daemon every open refuses.
    pub fn new(
        session: ConnectSession,
        generation: watch::Receiver<u64>,
        daemon: Option<Arc<dyn Daemon + Send + Sync>>,
    ) -> Result<Self, claim_coordinator::Error> {
        let expected = *generation.borrow();
        let context = SplitForkProduction::new(
            session.client.clone(),
            session.account.clone(),
            expected,
            generation.clone(),
        )?
        .context()
        .clone();
        Ok(Self {
            session,
            generation,
            expected,
            context,
            daemon,
        })
    }
}
impl UnifiedPort for ProductionUnified {
    fn context(&self) -> Context {
        self.context.clone()
    }
    fn identity(&self) -> PortIdentity {
        PortIdentity {
            context: self.context.clone(),
            daemon: self
                .daemon
                .as_ref()
                .map_or(0, |daemon| Arc::as_ptr(daemon) as *const () as usize),
        }
    }
    fn open(&self, open: UnifiedOpen) -> Result<Box<dyn UnifiedFlow>, Step2Refusal> {
        let daemon = self
            .daemon
            .clone()
            .ok_or_else(|| Step2Refusal::retry(UNIFIED_NEEDS_VAULT))?;
        let transport = SplitStep2Production::new(
            &self.session.client,
            daemon.clone(),
            self.expected,
            self.generation.clone(),
        )
        .map_err(|error| match error {
            claim_coordinator::Error::Unsupported => Step2Refusal::final_(UNIFIED_NEEDS_VAULT),
            error => describe_check(error),
        })?;
        let production = step2::fork_production(&self.session, self.expected, &self.generation)?;
        let UnifiedOpen {
            directory,
            target_cube,
            source,
            coins,
            fork_height,
        } = open;
        // The journal is created only at confirmation (U3); its private
        // directory may be made now.
        claim_workflow::prepare_directory(&directory)
            .map_err(|error| describe_check(claim_coordinator::Error::Journal(error)))?;
        let coordinator = UnifiedCoordinator::new(
            &directory,
            target_cube,
            source,
            coins,
            fork_height,
            production,
            transport,
            CHECK_POLICY,
        )
        .map_err(describe_check)?;
        Ok(Box::new(UnifiedDriver::new(LiveCore {
            coordinator,
            daemon,
            fees: split_fees::btcb2_fee_source(Some(self.session.client.clone())),
            review: None,
        })))
    }
}

/// A coordinator in transit between the panel and a task.
pub struct Flow(pub Box<dyn UnifiedFlow>);
impl fmt::Debug for Flow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UnifiedFlow")
    }
}
/// Results of the single-step tasks, inside [`SplitEvent::Unified`].
#[derive(Debug)]
pub enum UnifiedEvent {
    /// A coordinator opened, with an empty seed set for its wallet and the
    /// wallet (public descriptors).
    Opened(Result<(Flow, SeedSet, SplitSource), Step2Refusal>),
    /// A seed added, or why not (copy); the set comes back unless the task
    /// was lost.
    SeedAdded(Option<SeedSet>, Result<(), String>),
    /// Built and signed. The seeds come back only if nothing was signed
    /// (the build refused); otherwise they were cleared in the task. No
    /// coordinator comes back from a task that panicked.
    Signed(Option<Flow>, Option<SeedSet>, Result<(), Step2Refusal>),
    Reviewed(Flow, Result<SweepReviewView, Step2Refusal>),
    Submitted(Flow, Result<Outcome, Step2Refusal>),
    Reconciled(Flow, Result<TransactionObservation, Step2Refusal>),
}

/// The panel's single-step state.
#[derive(Default)]
pub struct UnifiedState {
    port: Option<Arc<dyn UnifiedPort>>,
    /// The route a started panel took.
    route: Option<Route>,
    flow: Option<Box<dyn UnifiedFlow>>,
    revoke: Option<RevokeHandle>,
    /// The seeds entered so far; `None` while a task holds them.
    seeds: Option<SeedSet>,
    /// The threshold of the wallet being signed for.
    threshold: usize,
    held: usize,
    words: SeedText,
    passphrase: SeedText,
    review: Option<SweepReviewView>,
    outcome: Option<Outcome>,
    seen: Option<TransactionObservation>,
    /// The wallet signed for (public descriptors).
    source: Option<SplitSource>,
    /// The source digest and directory this route's journal is created in.
    journal: Option<(sha256::Hash, PathBuf)>,
}

impl UnifiedState {
    /// Clear the seed set (each seed scrubbed as it drops), drop it and empty
    /// both input buffers.
    pub(super) fn scrub(&mut self) {
        if let Some(seeds) = self.seeds.as_mut() {
            seeds.clear();
        }
        self.seeds = None;
        self.held = 0;
        self.words.clear();
        self.passphrase.clear();
    }
    pub fn route(&self) -> Option<Route> {
        self.route
    }
    pub fn threshold(&self) -> usize {
        self.threshold
    }
    /// Seeds held now.
    pub fn held(&self) -> usize {
        self.held
    }
    pub fn holds_seeds(&self) -> bool {
        self.seeds.is_some()
    }
    /// The typed buffers, lent to the view's seed inputs only.
    pub(in crate::app) fn typed_words(&self) -> &SeedText {
        &self.words
    }
    pub(in crate::app) fn typed_passphrase(&self) -> &SeedText {
        &self.passphrase
    }
    pub fn review(&self) -> Option<&SweepReviewView> {
        self.review.as_ref()
    }
    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }
    pub fn seen(&self) -> Option<TransactionObservation> {
        self.seen
    }
    pub fn has_port(&self) -> bool {
        self.port.is_some()
    }
    fn engaged(&self) -> bool {
        self.flow.is_some() || self.revoke.is_some()
    }
}

impl SplitPanel {
    /// Install (or clear) the session's unified port. An equivalent one (same
    /// session context and daemon instance) is ignored; any other revokes a
    /// single-step handle first, held or in a task.
    pub fn set_unified_port(&mut self, port: Option<Arc<dyn UnifiedPort>>) {
        let same = match (&self.unified.port, &port) {
            (Some(a), Some(b)) => a.identity() == b.identity(),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        if self.unified.engaged() || matches!(self.stage, Stage::Working(_)) {
            self.revoke();
        }
        self.unified.port = port;
    }

    pub fn unified(&self) -> &UnifiedState {
        &self.unified
    }

    /// The seed route is offered in a started panel's route choice (U6).
    pub fn seed_route_offered(&self) -> bool {
        self.stage == Stage::ChooseRoute
            && self
                .intent
                .as_deref()
                .is_some_and(|intent| intent_routes(intent).seed_unified)
    }

    /// The seeds cover the threshold: build and sign may run.
    pub fn can_build_unified(&self) -> bool {
        self.stage == Stage::Unified(UnifiedStage::EnterSeeds)
            && self.unified.flow.is_some()
            && self
                .unified
                .seeds
                .as_ref()
                .is_some_and(SeedSet::is_complete)
    }

    /// Revoke and drop every single-step handle and scrub the seeds; called
    /// from [`SplitPanel::revoke`].
    pub(super) fn revoke_unified(&mut self) {
        if let Some(revoke) = self.unified.revoke.take() {
            revoke();
        }
        self.unified.flow = None;
        self.unified.review = None;
        self.unified.scrub();
    }

    fn bind_flow(&mut self, flow: Box<dyn UnifiedFlow>) {
        self.unified.revoke = Some(flow.revoke_handle());
        self.unified.flow = Some(flow);
    }
    fn take_flow(&mut self, work: Work) -> Option<(Box<dyn UnifiedFlow>, Context)> {
        let context = self.connect.as_ref()?.context();
        let flow = self.unified.flow.take()?;
        self.stage = Stage::Working(work);
        Some((flow, context))
    }
    fn unified_event(seq: u64, event: UnifiedEvent) -> SplitEvent {
        SplitEvent::Unified(seq, event)
    }

    pub(super) fn update_unified(&mut self, message: UnifiedMessage) -> Task<Message> {
        match message {
            // Only a started panel's route choice reaches the route (D1).
            UnifiedMessage::Choose(route)
                if self.stage == Stage::ChooseRoute
                    && self.intent.is_some()
                    && self.journal.is_none() =>
            {
                match route {
                    Route::TwoStep => {
                        self.unified.route = Some(Route::TwoStep);
                        self.notice = None;
                        match self.connect.clone() {
                            Some(connect) => self.resume_journal(connect),
                            None => {
                                self.stage = Stage::NeedsSession;
                                Task::none()
                            }
                        }
                    }
                    // P1: never offered; asking for it only says why.
                    Route::Hardware => {
                        self.notice = Some(P1_HARDWARE.to_string());
                        Task::none()
                    }
                    Route::Seeds => self.choose_seeds(),
                }
            }
            UnifiedMessage::Words(text)
                if self.stage == Stage::Unified(UnifiedStage::EnterSeeds) =>
            {
                self.unified.words = text;
                Task::none()
            }
            UnifiedMessage::Passphrase(text)
                if self.stage == Stage::Unified(UnifiedStage::EnterSeeds) =>
            {
                self.unified.passphrase = text;
                Task::none()
            }
            UnifiedMessage::AddSeed
                if self.stage == Stage::Unified(UnifiedStage::EnterSeeds)
                    && !self.unified.words.is_empty() =>
            {
                let Some(mut seeds) = self.unified.seeds.take() else {
                    return Task::none();
                };
                // Moved out: the buffers are empty whatever the result.
                let (words, passphrase) =
                    (self.unified.words.take(), self.unified.passphrase.take());
                self.stage = Stage::Working(Work::AddingSeed);
                self.spawn(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let added = seeds.add(words, passphrase).map(|_| ());
                            (seeds, added)
                        })
                        .await
                    },
                    |seq, result| {
                        let (seeds, added) = match result {
                            Ok((seeds, added)) => {
                                (Some(seeds), added.map_err(|error| describe_seed(&error)))
                            }
                            Err(_) => (
                                None,
                                Err("Adding the recovery phrase was interrupted. Nothing was kept; enter the phrases again.".to_string()),
                            ),
                        };
                        Self::unified_event(seq, UnifiedEvent::SeedAdded(seeds, added))
                    },
                )
            }
            UnifiedMessage::ClearSeeds
                if self.stage == Stage::Unified(UnifiedStage::EnterSeeds) =>
            {
                if let Some(seeds) = self.unified.seeds.as_mut() {
                    seeds.clear();
                }
                self.unified.held = 0;
                self.unified.words.clear();
                self.unified.passphrase.clear();
                Task::none()
            }
            UnifiedMessage::BuildAndSign if self.can_build_unified() => {
                let Some(seeds) = self.unified.seeds.take() else {
                    return Task::none();
                };
                let Some((mut flow, context)) = self.take_flow(Work::SweepSigning) else {
                    self.unified.seeds = Some(seeds);
                    return Task::none();
                };
                self.unified.review = None;
                self.spawn(
                    async move {
                        if let Err(refusal) = flow.build(&context).await {
                            return (Some(Flow(flow)), Some(seeds), Err(refusal));
                        }
                        let signed = tokio::task::spawn_blocking(move || {
                            let mut seeds = seeds;
                            let signed = flow.sign(&context, &seeds);
                            // Signed or refused, the seeds are done with.
                            seeds.clear();
                            drop(seeds);
                            (flow, signed)
                        })
                        .await;
                        match signed {
                            Ok((flow, signed)) => (Some(Flow(flow)), None, signed),
                            // The coordinator was lost with the task (its
                            // drop revokes it); nothing was recorded (U3).
                            Err(_) => (
                                None,
                                None,
                                Err(Step2Refusal::retry(
                                    "Signing was interrupted. Nothing was recorded or sent.",
                                )),
                            ),
                        }
                    },
                    |seq, (flow, seeds, result)| {
                        Self::unified_event(seq, UnifiedEvent::Signed(flow, seeds, result))
                    },
                )
            }
            UnifiedMessage::Review
                if matches!(
                    self.stage,
                    Stage::Unified(UnifiedStage::Signed | UnifiedStage::Review)
                ) =>
            {
                self.unified.review = None;
                let Some((mut flow, context)) = self.take_flow(Work::SweepReviewing) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = flow.review(&context).await;
                        (Flow(flow), result)
                    },
                    |seq, (flow, result)| {
                        Self::unified_event(seq, UnifiedEvent::Reviewed(flow, result))
                    },
                )
            }
            UnifiedMessage::Confirm if self.stage == Stage::Unified(UnifiedStage::Review) => {
                self.unified.review = None;
                let Some((mut flow, context)) = self.take_flow(Work::SweepSubmitting) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = flow.submit(&context).await;
                        (Flow(flow), result)
                    },
                    |seq, (flow, result)| {
                        Self::unified_event(seq, UnifiedEvent::Submitted(flow, result))
                    },
                )
            }
            UnifiedMessage::Reconcile if self.stage == Stage::Unified(UnifiedStage::Submitted) => {
                let Some((mut flow, context)) = self.take_flow(Work::SweepReconciling) else {
                    return Task::none();
                };
                self.spawn(
                    async move {
                        let result = flow.reconcile(&context).await;
                        (Flow(flow), result)
                    },
                    |seq, (flow, result)| {
                        Self::unified_event(seq, UnifiedEvent::Reconciled(flow, result))
                    },
                )
            }
            UnifiedMessage::Cancel
                if matches!(
                    self.stage,
                    Stage::Unified(
                        UnifiedStage::EnterSeeds | UnifiedStage::Signed | UnifiedStage::Review
                    ) | Stage::ChooseRoute
                ) =>
            {
                self.revoke_unified();
                self.notice = None;
                if self.intent.is_some() && self.journal.is_none() {
                    self.unified.route = None;
                    self.stage = Stage::ChooseRoute;
                    Task::none()
                } else {
                    self.stage = Stage::NeedsSession;
                    self.begin()
                }
            }
            _ => Task::none(),
        }
    }

    /// The seed route of a started panel: preconditions, then the
    /// coordinator, off the UI thread.
    fn choose_seeds(&mut self) -> Task<Message> {
        let Some(intent) = self.intent.clone() else {
            return Task::none();
        };
        if !intent_routes(&intent).seed_unified {
            self.notice = Some(SEEDS_NOT_OFFERED.to_string());
            return Task::none();
        }
        let Some(port) = self.unified.port.clone() else {
            self.notice = Some(UNIFIED_NEEDS_VAULT.to_string());
            return Task::none();
        };
        let Some(connect) = self.connect.clone() else {
            self.stage = Stage::NeedsSession;
            return Task::none();
        };
        self.notice = None;
        self.unified.route = Some(Route::Seeds);
        self.stage = Stage::Working(Work::SweepOpening);
        let (root, target) = (self.journal_root.clone(), self.target_cube.clone());
        self.spawn(
            async move {
                let open = preconditions(&*connect, &intent, &root, target)
                    .await
                    .map_err(|refusal| Step2Refusal {
                        reason: refusal.reason,
                        retry: refusal.retry,
                        recovery: Step2Recovery::None,
                    })?;
                open_flow(port, open).await
            },
            |seq, result| Self::unified_event(seq, UnifiedEvent::Opened(result)),
        )
    }

    pub(super) fn apply_unified(&mut self, event: UnifiedEvent) -> Task<Message> {
        match event {
            UnifiedEvent::Opened(Ok((Flow(flow), seeds, source))) => {
                let digest = source.digest();
                self.unified.journal =
                    Some((digest, step1::journal_directory(&self.journal_root, digest)));
                self.unified.source = Some(source);
                self.unified.threshold = seeds.threshold();
                self.unified.held = seeds.len();
                self.unified.seeds = Some(seeds);
                self.unified.outcome = flow.recorded_outcome();
                self.bind_flow(flow);
                self.stage = Stage::Unified(UnifiedStage::EnterSeeds);
                Task::none()
            }
            UnifiedEvent::Opened(Err(refusal)) => {
                self.unified.scrub();
                self.stage = Stage::Refused(to_refusal(refusal));
                Task::none()
            }
            UnifiedEvent::SeedAdded(seeds, added) => {
                self.notice = added.err();
                self.back_to_seeds(seeds);
                Task::none()
            }
            UnifiedEvent::Signed(None, _, result) => {
                self.revoke_unified();
                let reason = result.err().map(|r| r.reason).unwrap_or_default();
                self.stage = Stage::Refused(Refusal::retry(reason));
                Task::none()
            }
            UnifiedEvent::Signed(Some(Flow(flow)), seeds, result) => {
                self.bind_flow(flow);
                match result {
                    Ok(()) => {
                        self.unified.scrub();
                        self.notice = None;
                        self.stage = Stage::Unified(UnifiedStage::Signed);
                    }
                    Err(refusal) => {
                        self.notice = Some(refusal.reason);
                        self.back_to_seeds(seeds);
                    }
                }
                Task::none()
            }
            UnifiedEvent::Reviewed(Flow(flow), result) => {
                self.bind_flow(flow);
                match result {
                    Ok(review) => {
                        self.notice = None;
                        self.unified.review = Some(review);
                        self.stage = Stage::Unified(UnifiedStage::Review);
                    }
                    Err(refusal) if refusal.recovery == Step2Recovery::RefreshTarget => {
                        self.notice = Some(refusal.reason);
                        self.back_to_seeds(None);
                    }
                    Err(refusal) => {
                        self.notice = Some(refusal.reason);
                        self.stage = Stage::Unified(UnifiedStage::Signed);
                    }
                }
                Task::none()
            }
            UnifiedEvent::Submitted(Flow(flow), result) => {
                let recorded = flow.recorded_outcome();
                self.bind_flow(flow);
                if recorded.is_some() || result.is_ok() {
                    // The journal exists now: the panel's next session reads
                    // it (reopening it by kind is B4b-3c part 2).
                    self.journal = self.journal.clone().or(self.unified.journal.clone());
                }
                match result {
                    Ok(outcome) => {
                        self.notice = None;
                        self.unified.outcome = Some(outcome);
                        self.stage = Stage::Unified(UnifiedStage::Submitted);
                    }
                    Err(refusal) => {
                        self.notice = Some(refusal.reason);
                        self.unified.outcome = recorded;
                        self.stage = Stage::Unified(if recorded.is_some() {
                            UnifiedStage::Submitted
                        } else {
                            UnifiedStage::Signed
                        });
                    }
                }
                Task::none()
            }
            UnifiedEvent::Reconciled(Flow(flow), result) => {
                self.bind_flow(flow);
                self.reconciled_unified(result);
                self.stage = Stage::Unified(UnifiedStage::Submitted);
                Task::none()
            }
        }
    }

    fn reconciled_unified(&mut self, result: Result<TransactionObservation, Step2Refusal>) {
        match result {
            Ok(seen) => {
                self.notice = None;
                self.unified.seen = Some(seen);
            }
            Err(refusal) => self.notice = Some(refusal.reason),
        }
    }

    /// Back to seed entry with `seeds`, or an empty set for the wallet.
    fn back_to_seeds(&mut self, seeds: Option<SeedSet>) {
        self.unified.review = None;
        match seeds {
            Some(seeds) => {
                self.unified.held = seeds.len();
                self.unified.seeds = Some(seeds);
            }
            None => {
                self.unified.scrub();
                self.unified.seeds = self.unified_source().and_then(|s| SeedSet::new(&s).ok());
            }
        }
        self.stage = Stage::Unified(UnifiedStage::EnterSeeds);
    }

    /// The wallet the route signs for: the started panel's scan.
    fn unified_source(&self) -> Option<SplitSource> {
        self.unified.source.clone()
    }
}

fn to_refusal(refusal: Step2Refusal) -> Refusal {
    Refusal {
        reason: refusal.reason,
        retry: refusal.retry,
        recovery: match refusal.recovery {
            Step2Recovery::ReopenCube => step1::RefusalRecovery::ReopenCube,
            _ => step1::RefusalRecovery::None,
        },
    }
}

/// Open the coordinator off the UI thread, with an empty seed set for its
/// wallet. A wallet whose keys can't be matched to seeds refuses here.
async fn open_flow(
    port: Arc<dyn UnifiedPort>,
    open: UnifiedOpen,
) -> Result<(Flow, SeedSet, SplitSource), Step2Refusal> {
    tokio::task::spawn_blocking(move || {
        let seeds = SeedSet::new(&open.source)
            .map_err(|error| Step2Refusal::final_(describe_seed(&error)))?;
        let source = open.source.clone();
        let flow = port.open(open)?;
        Ok((Flow(flow), seeds, source))
    })
    .await
    .map_err(|_| Step2Refusal::retry("Opening the single-step sweep was interrupted. Try again."))?
}

#[cfg(all(test, unix))]
mod tests;
