//! The single-step (fork-only) route in the Split panel (#568 B4b-3c; owner
//! decisions U1-U7, C2-C6, P1, P7, P8). Like the rest of the panel it is
//! dormant (D1): the route is chosen only in a started panel, which nothing
//! in the GUI creates before B5c, and a fork-only journal is reopened only
//! by restart.
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
//!   Vault's daemon, and a `UnifiedReconciler` with the session alone.
//! - **Review.** The Protected pill with `PROTECTED_LIMITATION`, the route
//!   label and, on the node route, a privacy note. #654 F2 (lead decision):
//!   the D4 fee is read again at the review, and a signed rate below it is
//!   refused before anything is journaled; the sweep is then built and
//!   signed again.
//! - **Restart by kind.** `step2::restart` hands a fork-only record here:
//!   with a recorded submission it opens the reconciler; without one it is
//!   revalidated (coins authenticated afresh, the recorded sweep rebuilt
//!   exactly) and goes back to seed entry, or it is closed.
//! - **Fork-only close** (C6, U4). It writes the tombstone and keeps the
//!   journal. An unsubmitted record needs no chain check (no signed bytes
//!   were ever recorded); a submitted one only after a reconcile under this
//!   session saw the sweep absent, then a fresh check: the sweep absent,
//!   every coin unspent on BTCB2, the sweep absent again ([`check_close`]).
//!
//! Every Connect read, journal call and signature runs in a task.

use std::{
    convert::TryFrom,
    fmt,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
};

use async_trait::async_trait;
use iced::Task;
use tokio::sync::watch;

use coincube_core::{
    chain::ChainId,
    foreign_split::{SplitCoin, SplitSource, UnifiedReplayStatus},
    miniscript::bitcoin::{
        hashes::sha256, psbt::Psbt, secp256k1, Address, Network, OutPoint, Txid,
    },
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
                    UnifiedReconciler, UnifiedReview, RESERVATION_BOUND,
                },
                SplitForkProduction,
            },
            Outcome, SubmissionRoute,
        },
        claim_observation::{FailureKind, TransactionObservation},
        claim_workflow::{self, Context, Controller},
        foreign_psbt::{btcb2_sweep_feerate, SweepFeeSource},
        foreign_scan::SigningRoutes,
        foreign_split_inventory::FreshIndex,
        split_evidence::{
            authenticate_outpoints, RecordedOutpoint, SplitEvidenceSource, MAX_EVIDENCE_AGE_SECONDS,
        },
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
/// A recorded fork-only submission with no reconciler under the session.
pub const UNIFIED_RECONCILE_UNAVAILABLE: &str = "This split's single-step sweep was already sent or may have been. Its status can't be checked with Connect right now, so nothing was rebuilt or sent. Try again.";
/// #654 F2: no D4 fee at the review.
pub const FEE_UNAVAILABLE_AT_REVIEW: &str = "Connect has no Bitcoin Blake2b fee estimate right now, so the signed sweep's fee can't be checked. Nothing was recorded or sent; review it again shortly.";
/// #654 F2: the signed rate is below the fresh D4 estimate.
pub const FEE_BELOW_ESTIMATE: &str = "The signed sweep pays less than Connect's current Bitcoin Blake2b fee estimate, so it might not confirm. Nothing was recorded or sent. Enter the recovery phrases again to build and sign it at the current fee.";
/// The node route's privacy note on the review.
pub const UNIFIED_NODE_PRIVACY: &str = "The sweep will be sent through this Vault's own Bitcoin node. That node, which may be a remote one you configured, learns the transaction and this computer's network address before it relays it.";
/// An abandon of a fork-only record refused with `Conflict`.
pub const FORK_ONLY_ABANDON: &str = "This is a single-step Bitcoin Blake2b split, which can't be abandoned like a two-step split. Open it again to continue it or close it from its own screen.";
/// The close of an unsubmitted record.
pub const CLOSE_UNSUBMITTED: &str = "No sweep of this split was ever sent, so it can be closed without a check. Closing keeps its record on this device, and a new split of this wallet stays refused until that record is reset.";
/// The close of a submitted record, after its check.
pub const CLOSE_CHECKED: &str = "Bitcoin Blake2b shows neither this sweep nor any spend of its coins. Closing keeps its record, with the signed sweep, on this device, and a new split of this wallet stays refused until that record is reset.";
/// The close check found the sweep.
pub const SWEEP_SEEN: &str = "Bitcoin Blake2b shows this sweep, so it left and this split can't be closed. It stays tracked here.";
/// The close check found a coin spent.
pub const COIN_SPENT_ON_BTCB2: &str = "A coin of this split is no longer unspent on Bitcoin Blake2b, possibly spent by this sweep. This split can't be closed; it stays tracked here.";

// #568 B4b-3c (Robert, on #660, Reviewer-660661e D2): the typed seed text
// lives in its own private module, so its `Zeroizing` field is private to
// that module's impl and nothing here can read or copy it except through
// its methods.
mod seed_text;
pub use seed_text::SeedText;

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
    /// Restarted after a recorded submission: reconcile, or close.
    Reconcile,
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
    CheckClose,
    ConfirmClose,
    /// Leave the single step: clear the seeds and drop the route.
    Cancel,
}

/// What a fork-only journal recorded, as a restart read it: what the close
/// rests on. It grants nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedRecord {
    /// The recorded sweep's own txid, once its submission was recorded.
    pub sweep: Option<Txid>,
    /// The coins the sweep spends.
    pub claimed: Vec<OutPoint>,
}
impl UnifiedRecord {
    pub(super) fn of(controller: &Controller) -> Self {
        Self {
            sweep: controller.recorded_fork_submission().map(|s| s.txid()),
            claimed: controller.plan().claimed_prevouts,
        }
    }
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

/// After a recorded fork-only submission: reconcile only.
#[async_trait]
pub trait UnifiedRecon: Send {
    fn revoke_handle(&self) -> RevokeHandle;
    fn recorded_outcome(&self) -> Option<Outcome>;
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
    /// A new coordinator, or (`resume`) an unsubmitted record's. Blocking:
    /// callers use `spawn_blocking`.
    fn open(&self, open: UnifiedOpen, resume: bool) -> Result<Box<dyn UnifiedFlow>, Step2Refusal>;
    /// The reconciler of a recorded submission. Blocking.
    fn open_reconciler(
        &self,
        directory: PathBuf,
        target_cube: String,
        digest: sha256::Hash,
    ) -> Result<Box<dyn UnifiedRecon>, Step2Refusal>;
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

/// #660: the single step has no step 1 and no step 2. A coordinator
/// refusal reads as the two-step route's ([`describe_check`]) except where
/// that copy names step 1 or step 2.
pub fn describe_unified_check(error: claim_coordinator::Error) -> Step2Refusal {
    use claim_coordinator::Error as E;
    let reason = match &error {
        E::NotReady(coincube_core::claim::Assessment::WaitingForDepth { confirmations }) => {
            format!(
                "The sweep isn't ready yet ({confirmations} of {} confirmations). Check status again.",
                coincube_core::claim::MIN_CONFIRMATIONS
            )
        }
        E::NotReady(coincube_core::claim::Assessment::Reorged) => {
            "Bitcoin Blake2b reorganized since the last check, so nothing was sent. Check status again.".to_string()
        }
        E::Preflight(crate::services::claim_preflight::Error::BackendChanged) => {
            "This Vault's connection changed since the review, so nothing was sent. Review the sweep again.".to_string()
        }
        _ => return describe_check(error),
    };
    Step2Refusal {
        reason,
        ..describe_check(error)
    }
}

/// #660: a target reservation or proof refusal on the single step. Where
/// the two-step copy ([`describe_target`]) names step 1 or step 2, this one
/// names the sweep; the rest reads as there.
pub fn describe_unified_target(error: TargetError) -> Step2Refusal {
    match error {
        TargetError::Coordinator(error) => describe_unified_check(error),
        TargetError::NotTracking => Step2Refusal::retry(
            "No address can be reserved for the sweep yet. Check status again.",
        ),
        TargetError::NoReservation => {
            Step2Refusal::retry("No address is reserved for the sweep yet. Try again.")
        }
        TargetError::Used(chain) => Step2Refusal::retry(format!(
            "The address reserved for the sweep already has history on {}, so it is not fresh. It won't be used; try again to reserve a new one.",
            step2::chain_name(chain)
        )),
        other => describe_target(other),
    }
}

/// Copy for a refused single-step operation.
pub fn describe_unified(error: UnifiedError) -> Step2Refusal {
    match error {
        UnifiedError::Coordinator(error) => describe_unified_check(error),
        UnifiedError::Target(error) => describe_unified_target(error),
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

fn journal_refusal(error: claim_workflow::Error) -> Refusal {
    Refusal::retry(step1::describe(claim_coordinator::Error::Journal(error)))
}

/// Restart of an unsubmitted fork-only record: read it, authenticate its
/// coins afresh and hand back what the coordinator is reopened with; it
/// rebuilds the recorded sweep exactly and revalidates it.
pub async fn restore(
    connect: &dyn SplitConnect,
    directory: PathBuf,
    target_cube: String,
    digest: sha256::Hash,
) -> Result<UnifiedOpen, Refusal> {
    let identity = claim_workflow::split_identity(target_cube.clone(), digest);
    let (record, claimed) = {
        let controller = Controller::reopen_settling(&directory, &identity, connect.context())
            .await
            .map_err(journal_refusal)?;
        let record = controller
            .recorded_split()
            .map_err(journal_refusal)?
            .filter(|record| record.kind == claim_workflow::SplitKind::Unified)
            .ok_or_else(|| Refusal::final_("The journal here is not a single-step split."))?;
        if controller.recorded_fork_submission().is_some() {
            return Err(Refusal::final_(UNIFIED_RECONCILE_UNAVAILABLE));
        }
        (record, controller.plan().claimed_prevouts)
        // The controller, and the journal lock, end here.
    };
    let source = record
        .source
        .ok_or_else(|| Refusal::final_(step1::COMPLETED))?;
    if source.digest() != digest || record.source_digest != digest {
        return Err(Refusal::final_(
            "The split journal is in the wrong directory.",
        ));
    }
    let window = connect.window().await.map_err(|reason| {
        Refusal::retry(format!(
            "Couldn't read Bitcoin Blake2b's status from Connect ({reason})."
        ))
    })?;
    if window.fork_height != record.fork_height {
        return Err(Refusal::final_(step1::STALE_ANCHOR));
    }
    let (recorded, _) = step1::resolve_outpoints(connect.evidence(), &source, &claimed).await?;
    let authenticated = authenticate_outpoints(
        connect.evidence(),
        &recorded,
        record.fork_height,
        MAX_EVIDENCE_AGE_SECONDS,
    )
    .await
    .map_err(step1::evidence_refusal)?;
    Ok(UnifiedOpen {
        directory,
        target_cube,
        source,
        coins: authenticated.coins,
        fork_height: record.fork_height,
    })
}

fn fresh(evidence: &dyn SplitEvidenceSource, observed_at: i64) -> bool {
    evidence
        .now()
        .checked_sub(observed_at)
        .is_some_and(|age| (0..=MAX_EVIDENCE_AGE_SECONDS).contains(&age))
}
fn unavailable(kind: FailureKind) -> Refusal {
    Refusal::retry(format!(
        "Connect couldn't check Bitcoin Blake2b for this split ({kind:?}), so it can't be closed yet. This is not a sign that the sweep left or that a coin was spent. Try again later."
    ))
}
async fn sweep_absent(evidence: &dyn SplitEvidenceSource, sweep: Txid) -> Result<(), Refusal> {
    let seen = evidence
        .transaction(ChainId::BitcoinBlake2b, sweep)
        .await
        .map_err(unavailable)?;
    if !fresh(evidence, seen.observed_at()) {
        return Err(unavailable(FailureKind::Stale));
    }
    if *seen.value() != TransactionObservation::Absent {
        return Err(Refusal::final_(SWEEP_SEEN));
    }
    Ok(())
}

/// C6: before a submitted fork-only record may be closed, fresh reads must
/// show, in this order: the recorded sweep absent from BTCB2 (a read keyed
/// by its own txid); every claimed coin among its address's BTCB2 unspent
/// outputs (the address from its previous transaction, checked against its
/// txid); and the sweep absent again. Anything else refuses and the
/// journal is kept. There is no step 1 to check.
pub async fn check_close(
    connect: &dyn SplitConnect,
    record: &UnifiedRecord,
) -> Result<(), Refusal> {
    let sweep = record
        .sweep
        .ok_or_else(|| Refusal::final_(CLOSE_UNSUBMITTED))?;
    let evidence = connect.evidence();
    sweep_absent(evidence, sweep).await?;
    for outpoint in &record.claimed {
        let previous = evidence
            .previous_transaction(ChainId::Bitcoin, outpoint.txid)
            .await
            .map_err(unavailable)?;
        let address = (previous.compute_txid() == outpoint.txid)
            .then(|| usize::try_from(outpoint.vout).ok())
            .flatten()
            .and_then(|vout| previous.output.get(vout))
            .and_then(|output| Address::from_script(&output.script_pubkey, Network::Bitcoin).ok())
            .ok_or_else(|| Refusal::final_(step1::UNIDENTIFIED))?;
        let unspent = evidence
            .unspent_outputs(ChainId::BitcoinBlake2b, &address.to_string())
            .await
            .map_err(unavailable)?;
        if !fresh(evidence, unspent.observed_at()) {
            return Err(unavailable(FailureKind::Stale));
        }
        if !unspent.value().contains(outpoint) {
            return Err(Refusal::final_(COIN_SPENT_ON_BTCB2));
        }
    }
    sweep_absent(evidence, sweep).await
}

/// The abandon path's copy for a journal refusal: a `Conflict` on a
/// fork-only record names the single-step route. Blocking (it reads the
/// journal again).
pub(super) fn abandon_refusal(
    directory: &Path,
    target_cube: &str,
    digest: sha256::Hash,
    context: Context,
    error: claim_workflow::Error,
) -> String {
    if matches!(error, claim_workflow::Error::Conflict) {
        let identity = claim_workflow::split_identity(target_cube.to_owned(), digest);
        let unified = Controller::reopen_settling_blocking(directory, &identity, context)
            .ok()
            .and_then(|controller| controller.recorded_split().ok().flatten())
            .is_some_and(|record| record.kind == claim_workflow::SplitKind::Unified);
        if unified {
            return FORK_ONLY_ABANDON.to_string();
        }
    }
    format!("The split could not be abandoned ({error:?}).")
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
            self.core
                .reserve(context)
                .await
                .map_err(describe_unified_target)?;
        }
        match self.core.prove(context).await {
            Ok(()) => Ok(()),
            Err(TargetError::Used(_)) => {
                self.core
                    .reserve(context)
                    .await
                    .map_err(describe_unified_target)?;
                self.core
                    .prove(context)
                    .await
                    .map_err(describe_unified_target)
            }
            Err(error) => Err(describe_unified_target(error)),
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

struct ReconDriver(UnifiedReconciler);
#[async_trait]
impl UnifiedRecon for ReconDriver {
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
    ) -> Result<TransactionObservation, Step2Refusal> {
        self.0
            .reconcile_sweep(context)
            .await
            .map(|reconciled| reconciled.sweep)
            .map_err(describe_unified_check)
    }
}

/// The production unified port: one Connect session and, for a
/// coordinator, the target Vault's daemon on a route the sweep can be sent
/// through (admitted again at every open, as for step 2).
pub struct ProductionUnified {
    session: ConnectSession,
    generation: watch::Receiver<u64>,
    expected: u64,
    context: Context,
    daemon: Option<Arc<dyn Daemon + Send + Sync>>,
}
impl ProductionUnified {
    /// Refused without an account, for an unusable origin, or after the
    /// generation moved. Without a daemon it reconciles only.
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
    fn open(&self, open: UnifiedOpen, resume: bool) -> Result<Box<dyn UnifiedFlow>, Step2Refusal> {
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
            error => describe_unified_check(error),
        })?;
        let production = step2::fork_production(&self.session, self.expected, &self.generation)?;
        let UnifiedOpen {
            directory,
            target_cube,
            source,
            coins,
            fork_height,
        } = open;
        let coordinator = if resume {
            UnifiedCoordinator::resume(
                &directory,
                target_cube,
                source,
                coins,
                fork_height,
                production,
                transport,
                CHECK_POLICY,
            )
        } else {
            // The journal is created only at confirmation (U3); its private
            // directory may be made now.
            claim_workflow::prepare_directory(&directory).map_err(|error| {
                describe_unified_check(claim_coordinator::Error::Journal(error))
            })?;
            UnifiedCoordinator::new(
                &directory,
                target_cube,
                source,
                coins,
                fork_height,
                production,
                transport,
                CHECK_POLICY,
            )
        }
        .map_err(describe_unified_check)?;
        Ok(Box::new(UnifiedDriver::new(LiveCore {
            coordinator,
            daemon,
            fees: split_fees::btcb2_fee_source(Some(self.session.client.clone())),
            review: None,
        })))
    }
    fn open_reconciler(
        &self,
        directory: PathBuf,
        target_cube: String,
        digest: sha256::Hash,
    ) -> Result<Box<dyn UnifiedRecon>, Step2Refusal> {
        let reconciler = UnifiedReconciler::resume(
            &directory,
            target_cube,
            digest,
            step2::fork_production(&self.session, self.expected, &self.generation)?,
            CHECK_POLICY,
        )
        .map_err(describe_unified_check)?;
        Ok(Box::new(ReconDriver(reconciler)))
    }
}

/// A coordinator in transit between the panel and a task.
pub struct Flow(pub Box<dyn UnifiedFlow>);
impl fmt::Debug for Flow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UnifiedFlow")
    }
}
/// A reconciler in transit.
pub struct Recon(pub Box<dyn UnifiedRecon>);
impl fmt::Debug for Recon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UnifiedRecon")
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
    ReconOpened(Result<Recon, Step2Refusal>),
    ReconReconciled(Recon, Result<TransactionObservation, Step2Refusal>),
    CloseChecked(Result<(), Refusal>),
}

/// The panel's single-step state.
#[derive(Default)]
pub struct UnifiedState {
    port: Option<Arc<dyn UnifiedPort>>,
    /// The route a started panel took.
    route: Option<Route>,
    flow: Option<Box<dyn UnifiedFlow>>,
    recon: Option<Box<dyn UnifiedRecon>>,
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
    /// What this session's last reconcile saw; the close rests on it.
    seen_here: Option<TransactionObservation>,
    /// The fork-only record a restart read (the close's data).
    record: Option<UnifiedRecord>,
    /// The wallet signed for (public descriptors).
    source: Option<SplitSource>,
    close_checked: bool,
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
    pub fn record(&self) -> Option<&UnifiedRecord> {
        self.record.as_ref()
    }
    pub fn has_port(&self) -> bool {
        self.port.is_some()
    }
    fn engaged(&self) -> bool {
        self.flow.is_some() || self.recon.is_some() || self.revoke.is_some()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
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

    /// C6, submitted: from the reconcile-only stage, once a reconcile under
    /// this session saw the sweep absent.
    pub fn can_check_unified_close(&self) -> bool {
        self.stage == Stage::Unified(UnifiedStage::Reconcile)
            && self.connect.is_some()
            && self
                .unified
                .record
                .as_ref()
                .is_some_and(|r| r.sweep.is_some())
            && self.unified.seen_here == Some(TransactionObservation::Absent)
    }

    /// C6: an unsubmitted record closes without a check; a submitted one
    /// only after [`Self::can_check_unified_close`] and its check passed.
    pub fn can_confirm_unified_close(&self) -> bool {
        let Some(record) = self.unified.record.as_ref() else {
            return false;
        };
        self.connect.is_some()
            && self.journal.is_some()
            && match record.sweep {
                None => matches!(
                    self.stage,
                    Stage::Unified(UnifiedStage::EnterSeeds) | Stage::Refused(_)
                ),
                Some(_) => self.can_check_unified_close() && self.unified.close_checked,
            }
    }

    /// Revoke and drop every single-step handle and scrub the seeds; called
    /// from [`SplitPanel::revoke`].
    pub(super) fn revoke_unified(&mut self) {
        if let Some(revoke) = self.unified.revoke.take() {
            revoke();
        }
        self.unified.flow = None;
        self.unified.recon = None;
        self.unified.review = None;
        self.unified.seen_here = None;
        self.unified.close_checked = false;
        self.unified.scrub();
    }

    fn bind_flow(&mut self, flow: Box<dyn UnifiedFlow>) {
        self.unified.revoke = Some(flow.revoke_handle());
        self.unified.flow = Some(flow);
    }
    fn bind_unified_recon(&mut self, recon: Box<dyn UnifiedRecon>) {
        self.unified.revoke = Some(recon.revoke_handle());
        self.unified.recon = Some(recon);
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

    /// Restart found a fork-only record (`step2::Restart::Unified`).
    pub(super) fn restart_unified(&mut self, record: UnifiedRecord) -> Task<Message> {
        let (Some((digest, directory)), Some(connect)) =
            (self.journal.clone(), self.connect.clone())
        else {
            self.stage = Stage::NeedsSession;
            return Task::none();
        };
        let submitted = record.sweep.is_some();
        self.unified.record = Some(record);
        let Some(port) = self.unified.port.clone() else {
            self.stage = Stage::Refused(Refusal::retry(if submitted {
                UNIFIED_RECONCILE_UNAVAILABLE
            } else {
                UNIFIED_NEEDS_VAULT
            }));
            return Task::none();
        };
        let target = self.target_cube.clone();
        if submitted {
            self.stage = Stage::Working(Work::SweepOpening);
            return self.spawn(
                async move {
                    tokio::task::spawn_blocking(move || {
                        port.open_reconciler(directory, target, digest).map(Recon)
                    })
                    .await
                    .map_err(|_| {
                        Step2Refusal::retry("Reopening the split was interrupted. Try again.")
                    })?
                },
                |seq, result| Self::unified_event(seq, UnifiedEvent::ReconOpened(result)),
            );
        }
        self.stage = Stage::Working(Work::SweepOpening);
        self.spawn(
            async move {
                let open = restore(&*connect, directory, target, digest)
                    .await
                    .map_err(|refusal| Step2Refusal {
                        reason: refusal.reason,
                        retry: refusal.retry,
                        recovery: Step2Recovery::None,
                    })?;
                open_flow(port, open, true).await
            },
            |seq, result| Self::unified_event(seq, UnifiedEvent::Opened(result)),
        )
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
            UnifiedMessage::Reconcile if self.stage == Stage::Unified(UnifiedStage::Reconcile) => {
                let (Some(context), Some(mut recon)) = (
                    self.connect.as_ref().map(|c| c.context()),
                    self.unified.recon.take(),
                ) else {
                    return Task::none();
                };
                self.unified.close_checked = false;
                self.stage = Stage::Working(Work::SweepReconciling);
                self.spawn(
                    async move {
                        let result = recon.reconcile(&context).await;
                        (Recon(recon), result)
                    },
                    |seq, (recon, result)| {
                        Self::unified_event(seq, UnifiedEvent::ReconReconciled(recon, result))
                    },
                )
            }
            UnifiedMessage::CheckClose if self.can_check_unified_close() => {
                let (Some(connect), Some(record)) =
                    (self.connect.clone(), self.unified.record.clone())
                else {
                    return Task::none();
                };
                self.unified.close_checked = false;
                let back = std::mem::replace(&mut self.stage, Stage::Working(Work::CheckingClose));
                self.resume_stage = Some(back);
                self.spawn(
                    async move { check_close(&*connect, &record).await },
                    |seq, result| Self::unified_event(seq, UnifiedEvent::CloseChecked(result)),
                )
            }
            UnifiedMessage::ConfirmClose if self.can_confirm_unified_close() => {
                let (Some(connect), Some(record), Some((digest, directory))) = (
                    self.connect.clone(),
                    self.unified.record.clone(),
                    self.journal.clone(),
                ) else {
                    return Task::none();
                };
                // Release every handle on the journal, and the seeds, first.
                self.revoke_unified();
                let target = self.target_cube.clone();
                let ended = Arc::new(AtomicBool::new(false));
                self.ending = Some(ended.clone());
                self.stage = Stage::Working(Work::Closing);
                self.spawn(
                    async move {
                        if record.sweep.is_some() {
                            check_close(&*connect, &record)
                                .await
                                .map_err(|refusal| refusal.reason)?;
                        }
                        let context = connect.context();
                        tokio::task::spawn_blocking(move || {
                            step2::close_unified(
                                &directory,
                                &target,
                                digest,
                                context,
                                &record,
                                now_secs(),
                                &ended,
                            )
                        })
                        .await
                        .map_err(|_| "Closing was interrupted.".to_string())?
                    },
                    SplitEvent::Closed,
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
                open_flow(port, open, false).await
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
                    // The journal exists now: a restart reconciles it.
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
                // #661 F4: the restart's record said no sweep was sent; one
                // is recorded now, so no close offer may read it as
                // unsubmitted.
                if let (Some(record), Some(outcome)) =
                    (self.unified.record.as_mut(), self.unified.outcome)
                {
                    record.sweep = Some(match outcome {
                        Outcome::Recorded { txid }
                        | Outcome::UpstreamAccepted { txid, .. }
                        | Outcome::Uncertain { txid, .. } => txid,
                    });
                }
                Task::none()
            }
            UnifiedEvent::Reconciled(Flow(flow), result) => {
                self.bind_flow(flow);
                self.reconciled_unified(result);
                self.stage = Stage::Unified(UnifiedStage::Submitted);
                Task::none()
            }
            UnifiedEvent::ReconOpened(Ok(Recon(recon))) => {
                self.unified.outcome = recon.recorded_outcome();
                self.bind_unified_recon(recon);
                self.stage = Stage::Unified(UnifiedStage::Reconcile);
                Task::none()
            }
            UnifiedEvent::ReconOpened(Err(refusal)) => {
                self.stage = Stage::Refused(to_refusal(refusal));
                Task::none()
            }
            UnifiedEvent::ReconReconciled(Recon(recon), result) => {
                self.bind_unified_recon(recon);
                self.reconciled_unified(result);
                self.stage = Stage::Unified(UnifiedStage::Reconcile);
                Task::none()
            }
            UnifiedEvent::CloseChecked(result) => {
                match result {
                    Ok(()) => self.unified.close_checked = true,
                    Err(refusal) => self.notice = Some(refusal.reason),
                }
                self.stage = self
                    .resume_stage
                    .take()
                    .unwrap_or(Stage::Unified(UnifiedStage::Reconcile));
                Task::none()
            }
        }
    }

    fn reconciled_unified(&mut self, result: Result<TransactionObservation, Step2Refusal>) {
        match result {
            Ok(seen) => {
                self.notice = None;
                self.unified.seen = Some(seen);
                self.unified.seen_here = Some(seen);
            }
            Err(refusal) => {
                self.unified.seen_here = None;
                self.notice = Some(refusal.reason);
            }
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

    /// The wallet the route signs for: the started panel's scan, or the
    /// record a restart reopened.
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
    resume: bool,
) -> Result<(Flow, SeedSet, SplitSource), Step2Refusal> {
    tokio::task::spawn_blocking(move || {
        let seeds = SeedSet::new(&open.source)
            .map_err(|error| Step2Refusal::final_(describe_seed(&error)))?;
        let source = open.source.clone();
        let flow = port.open(open, resume)?;
        Ok((Flow(flow), seeds, source))
    })
    .await
    .map_err(|_| Step2Refusal::retry("Opening the single-step sweep was interrupted. Try again."))?
}

#[cfg(all(test, unix))]
mod tests;
