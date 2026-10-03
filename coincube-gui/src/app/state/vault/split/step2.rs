//! Split step 2 (#568 B3b-2b): what the panel does for step 2 that is not
//! iced state. The panel stages, view and App hooks that drive it are
//! B3b-2b-2; nothing in the GUI calls this module yet (D1).
//!
//! - **Ports.** [`Step2Port`] opens the step-2 submission side of a Split
//!   journal through the target Vault's daemon: a [`Step2Prep`] (target
//!   reservation and proof, construction under the step-2 token, the
//!   signed-PSBT handoff). Its production implementation, [`ProductionStep2`],
//!   wraps `claim_coordinator::fork::split` and exists only for a daemon on a
//!   route step 2 can be sent through (#637 R2). [`ReconPort`] opens, after a
//!   recorded step-2 submission, a [`Step2Recon`] that can only reconcile; its
//!   production implementation, [`ProductionRecon`], needs only the Connect
//!   session, never the daemon (#637 R1).
//! - **Journal-lock ordering** (#626). The step-1 driver and the step-2
//!   preparation each hold the journal lock. [`enter_step2`] drops the step-1
//!   driver *before* opening the preparation, off the UI thread;
//!   [`leave_for_step1`] drops the preparation before reopening the step-1
//!   driver for a reorg review; the preparation's `finish` hands its lock to
//!   the submission coordinator. [`restart`] opens a [`Step2Recon`] instead of
//!   the step-1 driver when the journal already records a step-2 submission,
//!   because the claimed coins may be spent on BTCB2 by then and step 1 can
//!   no longer be rebuilt from them. That decision needs only the Connect
//!   session, so it holds whatever state the Vault daemon is in.
//! - **Copy.** Every target and construction refusal ([`describe_target`],
//!   [`describe_step2`]), the waiting state while the Vault reserves its
//!   address ([`RESERVING`], #592 N4), the route label with a privacy note on
//!   the node route ([`route_copy`]) and the "Split — cannot replay" label,
//!   which only a live six-confirmation check can produce ([`CannotReplay`]).
//!
//! Every blocking call here runs off the UI thread.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use tokio::sync::watch;

use coincube_core::{
    descriptors::CoincubeDescriptor,
    foreign_split::{SplitCoin, SplitStep1, VerifiedSplitStep1},
    miniscript::bitcoin::{hashes::sha256, psbt::Psbt, Txid},
};

use super::step1::{self, OpenRequest, Refusal, RevokeHandle, SplitConnect, Step1Driver};
use crate::{
    app::state::vault::claim::{ConnectSession, CHECK_POLICY},
    daemon::Daemon,
    services::{
        claim_coordinator::{
            self,
            fork::split::{
                step2::{
                    SplitStep2Coordinator, SplitStep2Production, SplitStep2Reconciler, Step2Error,
                    TargetError, RESERVATION_BOUND,
                },
                ForeignStep2Authorization, SplitCheckError, SplitForkProduction, SplitPreparation,
                Step2Liveness,
            },
            Outcome, Review, SubmissionRoute,
        },
        claim_observation::TransactionObservation,
        claim_workflow::{self, Context, Controller, Status},
        foreign_psbt::SweepFeeSource,
        split_fees,
    },
};

/// N4: shown while the target Vault reserves its fresh receive address and
/// Connect proves it unused, for at most [`RESERVATION_BOUND`].
pub const RESERVING: &str = "Reserving a fresh receive address in this Vault and checking with Connect that it has never been used on either chain. This takes a few seconds.";
/// The live replay-protection label (see [`CannotReplay`]).
pub const CANNOT_REPLAY: &str = "Split — cannot replay";
/// The privacy note shown with the node route.
pub const NODE_PRIVACY: &str = "Step 2 will be sent through this Vault's own Bitcoin node. That node, which may be a remote one you configured, learns the transaction and this computer's network address before it relays it.";
/// A restart found a recorded step-2 submission but has no reconciler for
/// this session (#637 R1): nothing else is opened.
pub const RECONCILE_UNAVAILABLE: &str = "Step 2 of this split was already sent or may have been. Its status can't be checked with Connect right now, so nothing was rebuilt or sent. Try again.";

/// What a refused step-2 operation means for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step2Refusal {
    pub reason: String,
    pub retry: bool,
}
impl Step2Refusal {
    fn final_(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retry: false,
        }
    }
    fn retry(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retry: true,
        }
    }
}

fn chain_name(chain: coincube_core::chain::ChainId) -> &'static str {
    match chain {
        coincube_core::chain::ChainId::Bitcoin => "Bitcoin",
        _ => "Bitcoin Blake2b",
    }
}

/// Copy for a target reservation or proof refusal.
pub fn describe_target(error: TargetError) -> Step2Refusal {
    match error {
        TargetError::Coordinator(error) => describe_check(error),
        TargetError::AlreadyReserved => Step2Refusal::retry(
            "This Vault already has a reserved address for this split; it is reused. Check it again.",
        ),
        TargetError::NotTracking => Step2Refusal::retry(
            "Step 1 has not been seen on Bitcoin yet, so no address is reserved for step 2. Check status again later.",
        ),
        TargetError::NoReservation => {
            Step2Refusal::retry("No address is reserved for step 2 yet. Reserve one first.")
        }
        TargetError::ReservationUnavailable => Step2Refusal::retry(
            "This Vault did not reserve a receive address in time. Nothing was built; try again.",
        ),
        TargetError::NotTargetVault => Step2Refusal::final_(
            "The reserved address does not belong to this Cube's Vault. Nothing was built or sent.",
        ),
        TargetError::Used(chain) => Step2Refusal::retry(format!(
            "The address reserved for step 2 already has history on {}, so it is not fresh. It won't be used; try again to reserve a new one.",
            chain_name(chain)
        )),
        TargetError::Unavailable(chain, kind) => Step2Refusal::retry(format!(
            "Connect couldn't prove the reserved address unused on {} ({kind:?}). This is a Connect limit, not a sign the address was used. Try again.",
            chain_name(chain)
        )),
    }
}

/// Copy for a step-2 construction refusal.
pub fn describe_step2(error: Step2Error) -> Step2Refusal {
    match error {
        Step2Error::Coordinator(error) => describe_check(error),
        Step2Error::FeeUnavailable => Step2Refusal::retry(
            "Connect has no Bitcoin Blake2b fee estimate right now, so step 2 can't be priced. Nothing was built; try again shortly.",
        ),
        Step2Error::TargetNotProven => Step2Refusal::retry(
            "The reserved address must be checked again before step 2 is built. Try again.",
        ),
        Step2Error::NotChecked | Step2Error::Redeem(_) => Step2Refusal::retry(
            "The step-2 check expired before step 2 was built. Nothing was built; try again.",
        ),
        Step2Error::DescriptorsForgotten => Step2Refusal::final_(step1::COMPLETED),
        Step2Error::Construction(error) => Step2Refusal::final_(format!(
            "Split couldn't build step 2 ({error}). Nothing was signed or sent."
        )),
    }
}

/// Copy for the six-confirmation check.
pub fn describe_split_check(error: SplitCheckError) -> Step2Refusal {
    match error {
        SplitCheckError::Coordinator(error) => describe_check(error),
        SplitCheckError::ClaimedCoinSpent(_) => Step2Refusal::final_(
            "A coin this split claims was already spent on Bitcoin Blake2b, so step 2 can't sweep it. Nothing was built or sent.",
        ),
        SplitCheckError::Unavailable(_, kind) => Step2Refusal::retry(format!(
            "Connect couldn't read Bitcoin Blake2b for this split ({kind:?}). This is a Connect or indexer limit, not a sign a coin was spent. Try again later."
        )),
    }
}

fn describe_check(error: claim_coordinator::Error) -> Step2Refusal {
    use claim_coordinator::Error as E;
    let retry = !matches!(
        error,
        E::Unsupported | E::InvalidBinding | E::Journal(claim_workflow::Error::WrongIdentity)
    );
    let reason = match error {
        E::NotReady(coincube_core::claim::Assessment::WaitingForDepth { confirmations }) => {
            format!(
                "Step 1 has {confirmations} of {} Bitcoin confirmations. Step 2 waits for all of them.",
                coincube_core::claim::MIN_CONFIRMATIONS
            )
        }
        E::Preflight(crate::services::claim_preflight::Error::BackendChanged) => {
            "This Vault's connection changed since the review, so nothing was sent. Review step 2 again.".to_string()
        }
        other => step1::describe(other),
    };
    Step2Refusal { reason, retry }
}

/// The review screen's route label, and a privacy note for the node route.
pub fn route_copy(route: SubmissionRoute) -> (&'static str, Option<&'static str>) {
    match route {
        SubmissionRoute::Connect => (route.label(), None),
        SubmissionRoute::BitcoinNode { .. } => (route.label(), Some(NODE_PRIVACY)),
    }
}

/// "Split — cannot replay", only from live evidence: a successful
/// six-confirmation check (step 1 six deep on Bitcoin at the tip, absent
/// from BTCB2, RDTS margin, every claimed coin unspent on BTCB2) minted it.
/// It carries that check's liveness (#636 P3-2), so the label disappears as
/// soon as the evidence does: a later check starts (whatever its result),
/// the session is revoked (logout, Cube close, backend switch), the
/// generation changes, the preparation is dropped, or the deadline passes.
/// No saved phase, journal record or earlier check can produce it.
#[derive(Debug, Clone)]
pub struct CannotReplay {
    tracked: Txid,
    live: Step2Liveness,
}
impl CannotReplay {
    /// The label while the check's evidence is live; afterwards `None`.
    pub fn label(&self) -> Option<&'static str> {
        self.live.is_live().then_some(CANNOT_REPLAY)
    }
    pub fn tracked_txid(&self) -> Txid {
        self.tracked
    }
}

/// The label a token's check supports. The token itself never leaves the
/// driver.
fn evidence_of(token: &ForeignStep2Authorization) -> CannotReplay {
    CannotReplay {
        tracked: token.tracked_txid(),
        live: token.liveness(),
    }
}

/// A refused handoff: why, and the preparation when it is still usable.
pub type FinishRefusal = (Step2Refusal, Option<Box<dyn Step2Prep>>);

/// The step-2 preparation over one Split journal; holds its lock.
#[async_trait]
pub trait Step2Prep: Send {
    fn revoke_handle(&self) -> RevokeHandle;
    /// Whether a new target reservation is needed (none, or the recorded one
    /// proven used). Restart data only.
    fn needs_reservation(&self) -> bool;
    /// The six-confirmation check; on success the live label, and the token
    /// is kept for the next [`Self::build`].
    async fn check(&mut self, context: &Context) -> Result<CannotReplay, Step2Refusal>;
    /// Reserve (if needed) and prove the target: the N4 waiting state. A
    /// target proven used is replaced once by a new reservation.
    async fn ensure_target(&mut self, context: &Context) -> Result<u32, Step2Refusal>;
    /// Build step 2 under the token of the last check; the unsigned PSBT to
    /// sign.
    async fn build(
        &mut self,
        context: &Context,
        coins: Vec<SplitCoin>,
    ) -> Result<Psbt, Step2Refusal>;
    /// Whether a (possibly partially) signed PSBT would finish, without
    /// giving up the journal: `Ok(false)` while more signatures are needed.
    /// CPU-bound: callers use `spawn_blocking`.
    fn verify_signed(&self, signed: &Psbt, coins: &[SplitCoin]) -> Result<bool, Step2Refusal>;
    /// Verify the signed PSBT and hand the journal to the coordinator.
    /// CPU-bound and blocking: callers use `spawn_blocking`.
    fn finish(
        self: Box<Self>,
        context: &Context,
        signed: &Psbt,
        coins: &[SplitCoin],
    ) -> Result<Box<dyn Step2Coord>, FinishRefusal>;
}

/// A review shown for step 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step2ReviewView {
    pub txid: Txid,
    pub fee_sats: u64,
    pub vsize: usize,
    pub route: SubmissionRoute,
    pub route_label: &'static str,
    pub privacy_note: Option<&'static str>,
}

/// The step-2 submission coordinator over one Split journal.
#[async_trait]
pub trait Step2Coord: Send {
    fn revoke_handle(&self) -> RevokeHandle;
    fn recorded_outcome(&self) -> Option<Outcome>;
    async fn review(&mut self, context: &Context) -> Result<Step2ReviewView, Step2Refusal>;
    /// Submit exactly what the last review showed.
    async fn submit(&mut self, context: &Context) -> Result<Outcome, Step2Refusal>;
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<(Status, TransactionObservation), Step2Refusal>;
}

/// After a recorded step-2 submission: reconcile only.
#[async_trait]
pub trait Step2Recon: Send {
    fn revoke_handle(&self) -> RevokeHandle;
    fn recorded_outcome(&self) -> Option<Outcome>;
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<(Status, TransactionObservation), Step2Refusal>;
}

/// Everything the preparation is opened with: the step 1 rebuilt at restore.
pub struct Step2Open {
    pub directory: PathBuf,
    pub target_cube: String,
    pub construction: SplitStep1,
    pub verified: VerifiedSplitStep1,
    pub fork_height: u64,
}

/// Opens the step-2 submission side of a Split journal for one session,
/// through the target Vault's daemon.
pub trait Step2Port: Send + Sync {
    fn context(&self) -> Context;
    /// What makes two ports the same: the session context (account,
    /// provider, generation) and the Vault daemon instance. The panel keeps
    /// its step-2 handles across an equivalent port and revokes them on any
    /// other (#637 F1).
    fn identity(&self) -> PortIdentity;
    /// Blocking: callers use `spawn_blocking`, after dropping any step-1
    /// driver on the same journal.
    fn open_preparation(&self, open: Step2Open) -> Result<Box<dyn Step2Prep>, Step2Refusal>;
}

/// Opens the reconcile-only side of a Split journal for one Connect session
/// (#637 R1). It needs no Vault daemon: a recorded step 2 is reconciled
/// whether the daemon is loaded, restarting, external or on a route step 2
/// can't be sent through.
pub trait ReconPort: Send + Sync {
    fn context(&self) -> Context;
    /// Blocking: callers use `spawn_blocking`.
    fn open_reconciler(
        &self,
        directory: PathBuf,
        target_cube: String,
        digest: sha256::Hash,
    ) -> Result<Box<dyn Step2Recon>, Step2Refusal>;
}

/// See [`Step2Port::identity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortIdentity {
    pub context: Context,
    /// The daemon handle's address: a restarted or switched daemon is a new
    /// instance.
    pub daemon: usize,
}

/// #626 ordering: release the step-1 driver (and its journal lock), *then*
/// open the step-2 preparation off the UI thread. Opening while the step-1
/// driver still held the lock would wait 2 s and refuse as busy.
pub async fn enter_step2(
    step1: Box<dyn Step1Driver>,
    port: Arc<dyn Step2Port>,
    open: Step2Open,
) -> Result<Box<dyn Step2Prep>, Step2Refusal> {
    drop(step1);
    tokio::task::spawn_blocking(move || port.open_preparation(open))
        .await
        .map_err(|_| Step2Refusal::retry("Opening step 2 was interrupted. Try again."))?
}

/// #626 ordering: drop the step-2 preparation *before* reopening the step-1
/// driver, for a reorg review (re-mined or dropped step 1) that only the
/// step-1 coordinator offers.
pub async fn leave_for_step1(
    prep: Box<dyn Step2Prep>,
    connect: Arc<dyn SplitConnect>,
    open: OpenRequest,
) -> Result<Box<dyn Step1Driver>, Refusal> {
    drop(prep);
    tokio::task::spawn_blocking(move || connect.open(open))
        .await
        .map_err(|_| Refusal::retry("Reopening step 1 was interrupted. Try again."))?
        .map_err(|error| Refusal::retry(step1::describe(error)))
}

/// Which side of the journal a restart reopens.
pub enum Restart {
    /// No step-2 submission recorded: resume step 1 as before
    /// (`SplitPanel::resume`).
    Step1,
    /// A step-2 submission is recorded: reconcile only.
    Reconcile(Box<dyn Step2Recon>),
}

/// Restart decision: read the journal under the Connect session's `context`
/// (lock released at once) and, when it records a step-2 submission, open
/// the reconciler instead of rebuilding step 1 (whose claimed coins may
/// already be spent on BTCB2 by step 2). No reconciler for the session
/// refuses; it never falls back to step 1 (#637 R1).
pub async fn restart(
    context: Context,
    recon: Option<Arc<dyn ReconPort>>,
    directory: PathBuf,
    target_cube: String,
    digest: sha256::Hash,
) -> Result<Restart, Step2Refusal> {
    let identity = claim_workflow::split_identity(target_cube.clone(), digest);
    let recorded = {
        let controller = Controller::reopen_settling(&directory, &identity, context)
            .await
            .map_err(|error| {
                Step2Refusal::retry(step1::describe(claim_coordinator::Error::Journal(error)))
            })?;
        controller.recorded_split_step2().is_some()
        // The controller, and the journal lock, end here.
    };
    if !recorded {
        return Ok(Restart::Step1);
    }
    let port = recon.ok_or_else(|| Step2Refusal::retry(RECONCILE_UNAVAILABLE))?;
    tokio::task::spawn_blocking(move || port.open_reconciler(directory, target_cube, digest))
        .await
        .map_err(|_| Step2Refusal::retry("Reopening the split was interrupted. Try again."))?
        .map(Restart::Reconcile)
}

/// The session's Split fork production, built fresh for each open.
fn fork_production(
    session: &ConnectSession,
    expected: u64,
    generation: &watch::Receiver<u64>,
) -> Result<SplitForkProduction, Step2Refusal> {
    SplitForkProduction::new(
        session.client.clone(),
        session.account.clone(),
        expected,
        generation.clone(),
    )
    .map_err(|error| Step2Refusal::retry(step1::describe(error)))
}

/// The production step-2 port for the target Vault's daemon and one Connect
/// session.
pub struct ProductionStep2 {
    session: ConnectSession,
    generation: watch::Receiver<u64>,
    expected: u64,
    context: Context,
    daemon: Arc<dyn Daemon + Send + Sync>,
    vault: CoincubeDescriptor,
}
impl ProductionStep2 {
    /// Refused without an account, after the generation moved, and for any
    /// route the submission transport does not admit (#637 R2): a daemon
    /// that is not embedded or not on BTCB2 mainnet, or a backend other than
    /// exactly Connect's BTCB2 Esplora at this session's origin or a bound
    /// Bitcoind node. So nothing is reserved, built or signed for a step 2
    /// that could not be sent. `finish` admits the route again at the
    /// handoff, and the review binds it.
    pub fn new(
        session: ConnectSession,
        generation: watch::Receiver<u64>,
        daemon: Arc<dyn Daemon + Send + Sync>,
    ) -> Result<Self, claim_coordinator::Error> {
        let expected = *generation.borrow();
        SplitStep2Production::new(
            &session.client,
            daemon.clone(),
            expected,
            generation.clone(),
        )?;
        let vault = daemon
            .config()
            .map(|config| config.main_descriptor.clone())
            .ok_or(claim_coordinator::Error::Unsupported)?;
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
            vault,
        })
    }
    fn production(&self) -> Result<SplitForkProduction, Step2Refusal> {
        fork_production(&self.session, self.expected, &self.generation)
    }
}
impl Step2Port for ProductionStep2 {
    fn context(&self) -> Context {
        self.context.clone()
    }
    fn identity(&self) -> PortIdentity {
        PortIdentity {
            context: self.context.clone(),
            daemon: Arc::as_ptr(&self.daemon) as *const () as usize,
        }
    }
    fn open_preparation(&self, open: Step2Open) -> Result<Box<dyn Step2Prep>, Step2Refusal> {
        let preparation = SplitPreparation::resume(
            &open.directory,
            open.target_cube,
            &open.construction,
            open.verified,
            open.fork_height,
            self.production()?,
            CHECK_POLICY,
        )
        .map_err(describe_check)?;
        Ok(Box::new(PreparationDriver {
            core: LivePrep {
                preparation,
                daemon: self.daemon.clone(),
                vault: self.vault.clone(),
                fees: split_fees::btcb2_fee_source(Some(self.session.client.clone())),
            },
            token: None,
            finish: Some(FinishDeps {
                client: self.session.client.clone(),
                expected: self.expected,
                generation: self.generation.clone(),
            }),
        }))
    }
}

/// The production reconcile-only port: one Connect session and no Vault
/// daemon (#637 R1).
pub struct ProductionRecon {
    session: ConnectSession,
    generation: watch::Receiver<u64>,
    expected: u64,
    context: Context,
}
impl ProductionRecon {
    /// Refused without an account, for an unusable origin, or after the
    /// generation moved.
    pub fn new(
        session: ConnectSession,
        generation: watch::Receiver<u64>,
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
        })
    }
}
impl ReconPort for ProductionRecon {
    fn context(&self) -> Context {
        self.context.clone()
    }
    fn open_reconciler(
        &self,
        directory: PathBuf,
        target_cube: String,
        digest: sha256::Hash,
    ) -> Result<Box<dyn Step2Recon>, Step2Refusal> {
        let reconciler = SplitStep2Reconciler::resume(
            &directory,
            target_cube,
            digest,
            fork_production(&self.session, self.expected, &self.generation)?,
            CHECK_POLICY,
        )
        .map_err(describe_check)?;
        Ok(Box::new(ReconcilerDriver(reconciler)))
    }
}

/// What the driver needs from a step-2 preparation: the production one is
/// [`LivePrep`] over `SplitPreparation`; tests substitute a fake to pin the
/// driver's own logic (#636 P3-1).
#[async_trait]
trait PrepCore: Send {
    fn revoke_handle(&self) -> RevokeHandle;
    fn needs_reservation(&self) -> Result<bool, claim_coordinator::Error>;
    fn recorded_target(&self) -> Result<Option<u32>, claim_coordinator::Error>;
    async fn check_signing(
        &mut self,
        context: &Context,
    ) -> Result<ForeignStep2Authorization, SplitCheckError>;
    async fn reserve(&mut self, context: &Context) -> Result<u32, TargetError>;
    async fn prove(&mut self, context: &Context) -> Result<(), TargetError>;
    async fn construct(
        &mut self,
        context: &Context,
        token: ForeignStep2Authorization,
        coins: Vec<SplitCoin>,
    ) -> Result<Psbt, Step2Error>;
    fn check_signed(
        &self,
        signed: &Psbt,
        coins: &[SplitCoin],
    ) -> Result<(), coincube_core::foreign_split::FinalizeError>;
}

/// The production preparation and what it reserves and prices with.
struct LivePrep {
    preparation: SplitPreparation,
    daemon: Arc<dyn Daemon + Send + Sync>,
    vault: CoincubeDescriptor,
    fees: Arc<dyn SweepFeeSource>,
}
#[async_trait]
impl PrepCore for LivePrep {
    fn revoke_handle(&self) -> RevokeHandle {
        let revoker = self.preparation.revoker();
        Arc::new(move || revoker.revoke())
    }
    fn needs_reservation(&self) -> Result<bool, claim_coordinator::Error> {
        self.preparation.needs_reservation()
    }
    fn recorded_target(&self) -> Result<Option<u32>, claim_coordinator::Error> {
        self.preparation.recorded_target()
    }
    async fn check_signing(
        &mut self,
        context: &Context,
    ) -> Result<ForeignStep2Authorization, SplitCheckError> {
        self.preparation.check_signing(context).await
    }
    async fn reserve(&mut self, context: &Context) -> Result<u32, TargetError> {
        let daemon = self.daemon.clone();
        self.preparation
            .reserve_target(
                context,
                &self.vault,
                async move { daemon.get_new_address().await },
                RESERVATION_BOUND,
            )
            .await
    }
    async fn prove(&mut self, context: &Context) -> Result<(), TargetError> {
        self.preparation.prove_target(context, &self.vault).await
    }
    async fn construct(
        &mut self,
        context: &Context,
        token: ForeignStep2Authorization,
        coins: Vec<SplitCoin>,
    ) -> Result<Psbt, Step2Error> {
        self.preparation
            .construct_step2(context, token, coins, &*self.fees)
            .await
    }
    fn check_signed(
        &self,
        signed: &Psbt,
        coins: &[SplitCoin],
    ) -> Result<(), coincube_core::foreign_split::FinalizeError> {
        self.preparation.check_signed(signed, coins)
    }
}

/// What `finish` needs to admit the transport.
struct FinishDeps {
    client: crate::services::coincube::CoincubeClient,
    expected: u64,
    generation: watch::Receiver<u64>,
}

struct PreparationDriver<P> {
    core: P,
    token: Option<ForeignStep2Authorization>,
    finish: Option<FinishDeps>,
}
impl<P: PrepCore> PreparationDriver<P> {
    /// A new check supersedes and clears any earlier token first, whatever
    /// its own result (#636 P3-1).
    async fn check_inner(&mut self, context: &Context) -> Result<CannotReplay, Step2Refusal> {
        self.token = None;
        let token = self
            .core
            .check_signing(context)
            .await
            .map_err(describe_split_check)?;
        let label = evidence_of(&token);
        self.token = Some(token);
        Ok(label)
    }
    /// Reserve when the journal needs one (a journal error refuses; it is
    /// never read as "no reservation needed"), prove, and replace a target
    /// proven used exactly once. A second used target refuses with no third
    /// reservation; an unavailable or stale proof never replaces anything.
    async fn ensure_target_inner(&mut self, context: &Context) -> Result<u32, Step2Refusal> {
        if self.core.needs_reservation().map_err(describe_check)? {
            self.core.reserve(context).await.map_err(describe_target)?;
        }
        match self.core.prove(context).await {
            Ok(()) => {}
            Err(TargetError::Used(_)) => {
                self.core.reserve(context).await.map_err(describe_target)?;
                self.core.prove(context).await.map_err(describe_target)?;
            }
            Err(error) => return Err(describe_target(error)),
        }
        self.core
            .recorded_target()
            .map_err(describe_check)?
            .ok_or_else(|| describe_target(TargetError::NoReservation))
    }
    /// One build per check: the token is taken before construction, so a
    /// failed construction also needs a new check.
    async fn build_inner(
        &mut self,
        context: &Context,
        coins: Vec<SplitCoin>,
    ) -> Result<Psbt, Step2Refusal> {
        let token = self.token.take().ok_or_else(|| {
            Step2Refusal::retry("Check step 1's confirmations again before building step 2.")
        })?;
        self.core
            .construct(context, token, coins)
            .await
            .map_err(describe_step2)
    }
    fn verify_inner(&self, signed: &Psbt, coins: &[SplitCoin]) -> Result<bool, Step2Refusal> {
        use coincube_core::foreign_split::FinalizeError;
        match self.core.check_signed(signed, coins) {
            Ok(()) => Ok(true),
            Err(FinalizeError::Unsatisfied) => Ok(false),
            Err(error) => Err(Step2Refusal::final_(format!(
                "The signed step 2 does not match what was built ({error}). Nothing was sent."
            ))),
        }
    }
}
#[async_trait]
impl Step2Prep for PreparationDriver<LivePrep> {
    fn revoke_handle(&self) -> RevokeHandle {
        self.core.revoke_handle()
    }
    /// Display and restart data only: a journal error reads as `false` here
    /// and decides nothing (`ensure_target` asks the journal itself).
    fn needs_reservation(&self) -> bool {
        self.core.needs_reservation().unwrap_or(false)
    }
    async fn check(&mut self, context: &Context) -> Result<CannotReplay, Step2Refusal> {
        self.check_inner(context).await
    }
    async fn ensure_target(&mut self, context: &Context) -> Result<u32, Step2Refusal> {
        self.ensure_target_inner(context).await
    }
    async fn build(
        &mut self,
        context: &Context,
        coins: Vec<SplitCoin>,
    ) -> Result<Psbt, Step2Refusal> {
        self.build_inner(context, coins).await
    }
    fn verify_signed(&self, signed: &Psbt, coins: &[SplitCoin]) -> Result<bool, Step2Refusal> {
        self.verify_inner(signed, coins)
    }
    fn finish(
        self: Box<Self>,
        context: &Context,
        signed: &Psbt,
        coins: &[SplitCoin],
    ) -> Result<Box<dyn Step2Coord>, FinishRefusal> {
        let Some(deps) = self.finish.as_ref() else {
            return Err((
                Step2Refusal::final_("This preparation cannot hand over."),
                Some(self),
            ));
        };
        let transport = match SplitStep2Production::new(
            &deps.client,
            self.core.daemon.clone(),
            deps.expected,
            deps.generation.clone(),
        ) {
            Ok(transport) => transport,
            Err(error) => return Err((describe_check(error), Some(self))),
        };
        let route = transport.route();
        // `finish` consumes the preparation: a refused handoff releases the
        // journal, and the panel reopens it to try again.
        self.core
            .preparation
            .finish(context, signed, coins, transport)
            .map(|coordinator| {
                Box::new(CoordinatorDriver {
                    coordinator,
                    review: None,
                    route,
                }) as Box<dyn Step2Coord>
            })
            .map_err(|error| (describe_check(error), None))
    }
}

struct CoordinatorDriver {
    coordinator: SplitStep2Coordinator,
    review: Option<Review>,
    route: SubmissionRoute,
}
#[async_trait]
impl Step2Coord for CoordinatorDriver {
    fn revoke_handle(&self) -> RevokeHandle {
        let revoker = self.coordinator.revoker();
        Arc::new(move || revoker.revoke())
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        self.coordinator.recorded_outcome()
    }
    async fn review(&mut self, context: &Context) -> Result<Step2ReviewView, Step2Refusal> {
        self.review = None;
        let review = self
            .coordinator
            .prepare_review(context)
            .await
            .map_err(describe_check)?;
        let snapshot = review.snapshot();
        debug_assert_eq!(snapshot.route, self.route);
        let (route_label, privacy_note) = route_copy(snapshot.route);
        let view = Step2ReviewView {
            txid: snapshot.txid,
            fee_sats: snapshot.fee_sats,
            vsize: snapshot.vsize,
            route: snapshot.route,
            route_label,
            privacy_note,
        };
        self.review = Some(review);
        Ok(view)
    }
    async fn submit(&mut self, context: &Context) -> Result<Outcome, Step2Refusal> {
        let review = self
            .review
            .take()
            .ok_or_else(|| Step2Refusal::retry("Review step 2 again before confirming."))?;
        self.coordinator
            .confirm_and_submit(review, context)
            .await
            .map_err(describe_check)
    }
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<(Status, TransactionObservation), Step2Refusal> {
        self.review = None;
        self.coordinator
            .reconcile_sweep(context)
            .await
            .map_err(describe_check)
    }
}

struct ReconcilerDriver(SplitStep2Reconciler);
#[async_trait]
impl Step2Recon for ReconcilerDriver {
    fn revoke_handle(&self) -> RevokeHandle {
        let revoker = self.0.revoker();
        Arc::new(move || revoker.revoke())
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        self.0.recorded_outcome()
    }
    async fn reconcile(
        &mut self,
        context: &Context,
    ) -> Result<(Status, TransactionObservation), Step2Refusal> {
        self.0
            .reconcile_sweep(context)
            .await
            .map_err(describe_check)
    }
}

#[cfg(all(test, unix))]
mod tests;
