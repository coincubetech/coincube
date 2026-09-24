//! Claim step 1 — the poison self-transfer on Bitcoin (Lane B1.5, slice 1).
//!
//! One panel, five stages: preconditions → build → sign → review → track.
//! The panel owns **no signing code and no broadcaster**: signing is the
//! Vault's own [`PsbtState`] flow (hot key, hardware, Keychain — unchanged),
//! and submission is `services::claim_coordinator`, which journals the
//! intent before it hands the verified transaction to the embedded daemon.
//!
//! What this slice does *not* do: input poison (the journal only represents
//! `Poison::OpReturn`), step 2 (`Step2Authorization` stays uninhabited), and
//! resuming a journaled intent after a restart.

use std::{
    collections::{HashMap, HashSet},
    convert::TryFrom,
    fmt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use iced::Task;
use tokio::sync::watch;

use coincube_core::{
    chain::ChainId,
    claim::{self, Assessment, ForkTransactionPresence, Policy},
    claim_finalize::finalize_poison_transfer,
    claim_spend::{create_poison_self_transfer, PoisonSelfTransfer},
    descriptors::PathInfo,
    miniscript::bitcoin::{
        absolute::LockTime, address, bip32::ChildNumber, hashes::Hash, psbt::Psbt, secp256k1,
        Address, BlockHash, Network, OutPoint, Transaction, Txid,
    },
    spend::{CandidateCoin, TxGetter},
};
use coincube_ui::widget::Element;
use coincubed::{
    bip329::Labels,
    commands::{CoinStatus, LabelItem, UpdateDerivIndexesResult},
};

use crate::{
    app::{
        cache::Cache,
        menu::Menu,
        message::Message,
        state::{vault::psbt::PsbtState, State},
        view,
        wallet::Wallet,
    },
    daemon::{
        model::{self, Coin, SpendTx},
        Daemon, DaemonBackend, DaemonError,
    },
    dir::CoincubeDirectory,
    hw::HardwareWalletConfig,
    services::{
        claim_coordinator::{
            self, CheckPolicy, Coordinator, Outcome, Production, Review, ReviewSnapshot, Revoker,
        },
        claim_observation::{
            http::HttpObservationSource, project_anchor, CollectionContext, ObservationSource,
        },
        claim_preflight::FreshnessPolicy,
        claim_workflow::{Context, Phase, Status},
        coincube::CoincubeClient,
        feeestimation::fee_estimation::FeeEstimator,
    },
};

/// How long before BIP-110's expiry the OP_RETURN poison stops being offered
/// (invariant I11). Step 1 must confirm on Bitcoin, then step 2 must be
/// built, signed and confirmed on the fork, all while the fork still
/// enforces the rule that makes step 1 invalid there; a day and a half
/// covers a slow confirmation, a reorg and a night's sleep. The comparison
/// itself is [`coincube_core::claim::assess_deployment`]'s, against the
/// fork's median-time-past — this constant only sets the margin.
pub const EXPIRY_MARGIN_SECONDS: i64 = 36 * 60 * 60;

/// The observation policy step 1 runs under. Anchor and Esplora reads older
/// than this are stale (`coincubed::connect::MAX_ANCHOR_AGE` is 90 s, so the
/// daemon and the wizard age the same evidence out together).
pub const CHECK_POLICY: CheckPolicy = CheckPolicy {
    observations: Policy {
        max_observation_age_seconds: 90,
        expiry_margin_seconds: EXPIRY_MARGIN_SECONDS,
    },
    preflight: FreshnessPolicy {
        max_age_seconds: 60,
        max_future_skew_seconds: 5,
    },
    collection_budget: Duration::from_secs(20),
};

/// The Connect session the panel works under: the account's authenticated
/// client and its opaque account id. Handed in by the App, which owns the
/// Connect panel; refreshed on every Connect message so a sign-in made while
/// this panel is open is seen.
#[derive(Clone)]
pub struct ConnectSession {
    pub client: CoincubeClient,
    pub account: String,
}

/// The fork's RDTS window as observed through the authenticated anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkWindow {
    /// First block under the fork's rules: coins confirmed below it exist on
    /// both chains and are what step 1 splits.
    pub fork_height: u64,
    /// The fork block's hash on the fork chain, carried in the OP_RETURN
    /// payload as a label (never as evidence).
    pub fork_hash: BlockHash,
    pub tip_height: u64,
    pub median_time_past: i64,
    /// BIP-110's expiry as the fork reports it (a median-time-past).
    pub expires_at: i64,
    /// [`claim::assess_deployment`]'s verdict for this window.
    pub rdts: Result<(), Assessment>,
}

/// The Vault's coins, partitioned against the fork height.
#[derive(Debug, Clone, Default)]
pub struct CoinSet {
    /// Confirmed, unspent, mature, and confirmed **below** the fork height.
    pub pre_fork: Vec<Coin>,
    /// Confirmed at or above the fork height — Bitcoin-only coins. Counted
    /// for the copy; slice 2's input poison is where they become useful.
    pub post_fork: usize,
    pub tip_height: i32,
}

/// What the async preconditions probe found. Each part fails on its own so
/// the panel can say which one, and offer a retry for the transient ones.
#[derive(Debug)]
pub struct Checked {
    pub window: Result<ForkWindow, String>,
    pub coins: Result<CoinSet, String>,
    pub feerate_vb: Result<u64, String>,
    /// [`Production::new`]'s answer for this daemon and client: the one
    /// place the backend and endpoint constraints are decided.
    pub backend: Result<(), String>,
}

/// The preconditions stage's state.
#[derive(Debug, Default)]
pub struct Preconditions {
    pub checking: bool,
    /// The claim target's Cube id, read from the fork chain's settings file
    /// on every entry (#503) — never from a cached flag.
    pub target: Option<String>,
    /// Whether the Vault has a shape the coordinator admits.
    pub shape: Option<String>,
    pub checked: Option<Checked>,
}

/// Why the panel refuses to go on, and whether asking again could change it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub reason: String,
    pub retry: bool,
}

/// A live coordinator plus everything a task needs to drive it. Moved into
/// each async step and handed back with the result, so exactly one owner
/// ever touches the coordinator. The revoker is cloned out before the move,
/// so revocation never waits for a task to return.
pub struct ClaimSession {
    coordinator: Coordinator,
    context: Context,
    review: Option<Review>,
}

impl fmt::Debug for ClaimSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimSession")
            .field("phase", &self.coordinator.phase())
            .field("reviewed", &self.review.is_some())
            .finish_non_exhaustive()
    }
}

impl ClaimSession {
    pub fn phase(&self) -> Phase {
        self.coordinator.phase()
    }
}

/// Results of the panel's async work. Every variant that carries the
/// session carries it back to the one panel that sent it out.
#[derive(Debug)]
pub enum ClaimEvent {
    /// The preconditions probe finished. The sequence number matches it to
    /// the request that started it; a reply to an older request is dropped.
    Checked(u64, Box<Checked>),
    /// The poison self-transfer was (or could not be) built.
    Built(Result<Box<PoisonSelfTransfer>, String>),
    /// The signed construction was finalised and journaled as an intent —
    /// or refused, in which case the construction comes back so the user can
    /// keep signing.
    Ready(Result<Box<ClaimSession>, (Box<PoisonSelfTransfer>, String)>),
    /// `prepare_review` finished; the review lives inside the session.
    Reviewed(Box<ClaimSession>, Result<ReviewSnapshot, String>),
    /// `confirm_and_submit` finished.
    Submitted(Box<ClaimSession>, Result<Outcome, String>),
    /// `reconcile` finished.
    Tracked(Box<ClaimSession>, Result<Status, String>),
}

enum Stage {
    Preconditions,
    /// Built and shown; nothing signed yet.
    Plan {
        built: Box<PoisonSelfTransfer>,
    },
    /// In the Vault's signing flow. `built` is `None` only while the
    /// finalise task holds it.
    Sign {
        built: Option<Box<PoisonSelfTransfer>>,
        psbt: Box<PsbtState>,
        finalizing: bool,
        error: Option<String>,
    },
    /// Journaled as an intent; preparing or showing the review. `session` is
    /// `None` only while a task holds it.
    Review {
        session: Option<Box<ClaimSession>>,
        snapshot: Option<ReviewSnapshot>,
        busy: bool,
        error: Option<String>,
    },
    /// Submitted (or uncertain); tracking the transaction on both chains.
    Track {
        session: Option<Box<ClaimSession>>,
        outcome: Outcome,
        status: Option<Status>,
        busy: bool,
        error: Option<String>,
    },
}

/// Read-only view of the panel's stage, for rendering.
pub enum StageView<'a> {
    Preconditions(&'a Preconditions, Option<Refusal>),
    Plan(&'a PoisonSelfTransfer),
    Sign {
        psbt: &'a PsbtState,
        finalizing: bool,
        error: Option<&'a str>,
    },
    Review {
        snapshot: Option<&'a ReviewSnapshot>,
        busy: bool,
        error: Option<&'a str>,
    },
    Track {
        outcome: Outcome,
        phase: Option<Phase>,
        status: Option<Status>,
        busy: bool,
        error: Option<&'a str>,
    },
}

pub struct ClaimStep1Panel {
    wallet: Arc<Wallet>,
    datadir: CoincubeDirectory,
    bitcoin_cube: String,
    generation: watch::Receiver<u64>,
    connect: Option<ConnectSession>,
    pre: Preconditions,
    stage: Stage,
    /// The live coordinator's revoker, kept outside the session so a
    /// revocation lands synchronously even while a task holds the session.
    revoker: Option<Revoker>,
    check_seq: u64,
}

impl ClaimStep1Panel {
    pub fn new(
        wallet: Arc<Wallet>,
        datadir: CoincubeDirectory,
        bitcoin_cube: String,
        generation: watch::Receiver<u64>,
        connect: Option<ConnectSession>,
    ) -> Self {
        let mut panel = Self {
            wallet,
            datadir,
            bitcoin_cube,
            generation,
            connect,
            pre: Preconditions::default(),
            stage: Stage::Preconditions,
            revoker: None,
            check_seq: 0,
        };
        panel.refresh_static_preconditions();
        panel
    }

    /// The App's hook for a Connect session change. A sign-out revokes any
    /// live coordinator (the App also advances the generation, which every
    /// in-flight coordinator call checks); a sign-in only makes the next
    /// probe possible.
    pub fn set_connect(&mut self, connect: Option<ConnectSession>) {
        let signed_out = connect.is_none() && self.connect.is_some();
        self.connect = connect;
        if signed_out {
            self.revoke();
        }
    }

    /// Revoke the live coordinator, synchronously. Called by the App before
    /// it replaces the context the coordinator was created under (Connect
    /// sign-out, Cube lock or close), and by `Drop`. Idempotent.
    pub fn revoke(&mut self) {
        if let Some(revoker) = &self.revoker {
            revoker.revoke();
        }
        match &mut self.stage {
            Stage::Review {
                session: Some(session),
                ..
            }
            | Stage::Track {
                session: Some(session),
                ..
            } => session.coordinator.invalidate(),
            _ => {}
        }
    }

    pub fn stage(&self) -> StageView<'_> {
        match &self.stage {
            Stage::Preconditions => StageView::Preconditions(&self.pre, self.refusal()),
            Stage::Plan { built } => StageView::Plan(built),
            Stage::Sign {
                psbt,
                finalizing,
                error,
                ..
            } => StageView::Sign {
                psbt,
                finalizing: *finalizing,
                error: error.as_deref(),
            },
            Stage::Review {
                snapshot,
                busy,
                error,
                ..
            } => StageView::Review {
                snapshot: snapshot.as_ref(),
                busy: *busy,
                error: error.as_deref(),
            },
            Stage::Track {
                session,
                outcome,
                status,
                busy,
                error,
            } => StageView::Track {
                outcome: *outcome,
                phase: session.as_ref().map(|s| s.phase()),
                status: *status,
                busy: *busy,
                error: error.as_deref(),
            },
        }
    }

    pub fn wallet(&self) -> &Wallet {
        &self.wallet
    }

    pub fn coins(&self) -> Option<&CoinSet> {
        self.pre
            .checked
            .as_ref()
            .and_then(|c| c.coins.as_ref().ok())
    }

    pub fn window(&self) -> Option<&ForkWindow> {
        self.pre
            .checked
            .as_ref()
            .and_then(|c| c.window.as_ref().ok())
    }

    pub fn feerate_vb(&self) -> Option<u64> {
        self.pre
            .checked
            .as_ref()
            .and_then(|c| c.feerate_vb.as_ref().ok().copied())
    }

    /// The first precondition that fails, in the order a user can act on
    /// them: target, Vault shape, Connect session, node backend, the fork's
    /// RDTS window, coins, fee rate. `None` when everything holds.
    pub fn refusal(&self) -> Option<Refusal> {
        let refuse = |reason: &str, retry: bool| {
            Some(Refusal {
                reason: reason.to_string(),
                retry,
            })
        };
        if self.pre.target.is_none() {
            return refuse(
                "No Bitcoin Blake2b Cube on this device reuses this Vault yet. Create the claim target first.",
                false,
            );
        }
        if let Some(shape) = &self.pre.shape {
            return refuse(shape, false);
        }
        if self.connect.is_none() {
            return refuse(
                "Sign in to Connect to claim. Step 1 reads Bitcoin Blake2b's status through your account.",
                false,
            );
        }
        let Some(checked) = &self.pre.checked else {
            return None;
        };
        if let Err(reason) = &checked.backend {
            return refuse(reason, false);
        }
        match &checked.window {
            Err(reason) => {
                return refuse(
                    &format!("Couldn't read Bitcoin Blake2b's status: {reason}"),
                    true,
                )
            }
            Ok(window) => {
                if let Err(assessment) = window.rdts {
                    return refuse(&rdts_refusal(assessment, window), false);
                }
            }
        }
        match &checked.coins {
            Err(reason) => {
                return refuse(&format!("Couldn't read this Vault's coins: {reason}"), true)
            }
            Ok(coins) if coins.pre_fork.is_empty() => {
                return refuse(
                    "Nothing to split: this Vault holds no confirmed coins from before the fork.",
                    true,
                )
            }
            Ok(_) => {}
        }
        if let Err(reason) = &checked.feerate_vb {
            return refuse(&format!("Couldn't fetch a fee rate: {reason}"), true);
        }
        None
    }

    /// Whether the Build button may be offered: every precondition holds and
    /// no probe is running.
    pub fn can_build(&self) -> bool {
        matches!(self.stage, Stage::Preconditions)
            && !self.pre.checking
            && self.pre.checked.is_some()
            && self.refusal().is_none()
    }

    /// The preconditions that need no network: the target on disk and the
    /// Vault's shape.
    fn refresh_static_preconditions(&mut self) {
        self.pre.target =
            crate::app::claim_target_cube_id(&self.datadir, &self.wallet.descriptor_checksum);
        self.pre.shape = vault_shape_refusal(&self.wallet);
    }

    fn probe(&mut self, daemon: Arc<dyn Daemon + Sync + Send>) -> Task<Message> {
        self.refresh_static_preconditions();
        // A refusal no probe can change: don't spend the network on it.
        if self.pre.target.is_none() || self.pre.shape.is_some() {
            return Task::none();
        }
        let Some(connect) = self.connect.clone() else {
            return Task::none();
        };
        self.check_seq += 1;
        let seq = self.check_seq;
        self.pre.checking = true;
        let wallet = self.wallet.clone();
        let generation = self.generation.clone();
        Task::perform(
            async move { Box::new(probe(daemon, connect, wallet, generation).await) },
            move |checked| Message::Claim(ClaimEvent::Checked(seq, checked)),
        )
    }

    fn build(&mut self, daemon: Arc<dyn Daemon + Sync + Send>) -> Task<Message> {
        if !self.can_build() {
            return Task::none();
        }
        let (Some(coins), Some(window), Some(feerate_vb)) =
            (self.coins(), self.window(), self.feerate_vb())
        else {
            return Task::none();
        };
        let coins = coins.clone();
        let fork_hash = window.fork_hash;
        let wallet = self.wallet.clone();
        self.pre.checking = true;
        Task::perform(
            async move { build(daemon, wallet, coins, feerate_vb, fork_hash).await },
            |built| Message::Claim(ClaimEvent::Built(built)),
        )
    }

    fn start_signing(&mut self) {
        let Stage::Plan { .. } = &self.stage else {
            return;
        };
        let Stage::Plan { built } = std::mem::replace(&mut self.stage, Stage::Preconditions) else {
            unreachable!("matched above");
        };
        let coins = self.coins().map(|c| c.pre_fork.clone()).unwrap_or_default();
        let secp = secp256k1::Secp256k1::verification_only();
        let tx = SpendTx::new(
            None,
            built.psbt().clone(),
            coins,
            &self.wallet.main_descriptor,
            &secp,
            self.wallet.chain.bitcoin_network(),
        );
        let psbt = PsbtState::new(self.wallet.clone(), tx, false);
        self.stage = Stage::Sign {
            built: Some(built),
            psbt: Box::new(psbt),
            finalizing: false,
            error: None,
        };
    }

    /// After every message the signing flow saw: once the primary path is
    /// satisfied and the picker has closed, finalise and journal. The
    /// construction leaves with the task and comes back if anything refuses.
    fn maybe_finalize(&mut self, daemon: Arc<dyn Daemon + Sync + Send>) -> Task<Message> {
        let target = self.pre.target.clone();
        let connect = self.connect.clone();
        let directory = journal_directory(&self.datadir, &self.wallet);
        let bitcoin_cube = self.bitcoin_cube.clone();
        let generation = self.generation.clone();
        let Stage::Sign {
            built,
            psbt,
            finalizing,
            error,
        } = &mut self.stage
        else {
            return Task::none();
        };
        if *finalizing || built.is_none() || psbt.modal.is_some() || psbt.tx.path_ready().is_none()
        {
            return Task::none();
        }
        let (Some(target), Some(connect)) = (target, connect) else {
            // Only the session is missing: keep the construction and every
            // signature, and say what is needed. The App hands a new session
            // in as soon as the account signs in again.
            let missing = if self.pre.target.is_none() {
                "The claim target is no longer on this device. Create it again, then come back."
            } else {
                "Signed out of Connect. Sign in again to record and submit the claim."
            };
            if error.as_deref() != Some(missing) {
                *error = Some(missing.to_string());
            }
            return Task::none();
        };
        let built = built.take().expect("checked above");
        *finalizing = true;
        *error = None;
        let signed = psbt.tx.psbt.clone();
        Task::perform(
            async move {
                finalize_and_journal(
                    built,
                    signed,
                    daemon,
                    connect,
                    bitcoin_cube,
                    target,
                    directory,
                    generation,
                )
                .await
            },
            |ready| Message::Claim(ClaimEvent::Ready(ready)),
        )
    }

    fn take_session(&mut self) -> Option<Box<ClaimSession>> {
        match &mut self.stage {
            Stage::Review { session, busy, .. } | Stage::Track { session, busy, .. } if !*busy => {
                let taken = session.take();
                if taken.is_some() {
                    *busy = true;
                }
                taken
            }
            _ => None,
        }
    }

    fn prepare_review(&mut self) -> Task<Message> {
        let Some(mut session) = self.take_session() else {
            return Task::none();
        };
        Task::perform(
            async move {
                let context = session.context.clone();
                let result = session.coordinator.prepare_review(&context).await;
                let result = match result {
                    Ok(review) => {
                        let snapshot = review.snapshot().clone();
                        session.review = Some(review);
                        Ok(snapshot)
                    }
                    Err(error) => Err(describe(error)),
                };
                (session, result)
            },
            |(session, result)| Message::Claim(ClaimEvent::Reviewed(session, result)),
        )
    }

    fn confirm(&mut self) -> Task<Message> {
        if !matches!(
            &self.stage,
            Stage::Review {
                snapshot: Some(_),
                busy: false,
                ..
            }
        ) {
            return Task::none();
        }
        let Some(mut session) = self.take_session() else {
            return Task::none();
        };
        Task::perform(
            async move {
                let context = session.context.clone();
                let result = match session.review.take() {
                    Some(review) => session
                        .coordinator
                        .confirm_and_submit(review, &context)
                        .await
                        .map_err(describe),
                    None => Err(
                        "There is no review to confirm. Review the transaction again.".to_string(),
                    ),
                };
                (session, result)
            },
            |(session, result)| Message::Claim(ClaimEvent::Submitted(session, result)),
        )
    }

    fn reconcile(&mut self) -> Task<Message> {
        if !matches!(&self.stage, Stage::Track { .. }) {
            return Task::none();
        }
        let Some(mut session) = self.take_session() else {
            return Task::none();
        };
        Task::perform(
            async move {
                let context = session.context.clone();
                let result = session
                    .coordinator
                    .reconcile(&context)
                    .await
                    .map_err(describe);
                (session, result)
            },
            |(session, result)| Message::Claim(ClaimEvent::Tracked(session, result)),
        )
    }

    /// Abandon an attempt that has not been journaled. A journaled intent is
    /// never abandoned from here: it is reconciled.
    fn cancel(&mut self) {
        match &self.stage {
            Stage::Plan { .. } | Stage::Sign { .. } => {
                self.stage = Stage::Preconditions;
                // The reserved change index is not reused: the daemon's
                // reservation is durable, so the next build reserves anew.
                self.pre.checked = None;
            }
            Stage::Preconditions | Stage::Review { .. } | Stage::Track { .. } => {}
        }
    }

    fn apply(
        &mut self,
        daemon: Arc<dyn Daemon + Sync + Send>,
        cache: &Cache,
        event: ClaimEvent,
    ) -> Task<Message> {
        match event {
            ClaimEvent::Checked(seq, checked) => {
                if seq != self.check_seq {
                    return Task::none();
                }
                self.pre.checking = false;
                self.pre.checked = Some(*checked);
                Task::none()
            }
            ClaimEvent::Built(result) => {
                self.pre.checking = false;
                match result {
                    Ok(built) if matches!(self.stage, Stage::Preconditions) => {
                        self.stage = Stage::Plan { built };
                    }
                    Ok(_) => {}
                    Err(reason) => {
                        return Task::done(Message::View(view::Message::ShowError(format!(
                            "Couldn't build the transaction: {reason}"
                        ))));
                    }
                }
                Task::none()
            }
            ClaimEvent::Ready(result) => match result {
                Ok(session) => {
                    self.revoker = Some(session.coordinator.revoker());
                    self.stage = Stage::Review {
                        session: Some(session),
                        snapshot: None,
                        busy: false,
                        error: None,
                    };
                    self.prepare_review()
                }
                Err((built, reason)) => {
                    if let Stage::Sign {
                        built: slot,
                        finalizing,
                        error,
                        ..
                    } = &mut self.stage
                    {
                        *slot = Some(built);
                        *finalizing = false;
                        *error = Some(reason.clone());
                    }
                    Task::done(Message::View(view::Message::ShowError(reason)))
                }
            },
            ClaimEvent::Reviewed(session, result) => {
                if let Stage::Review {
                    session: slot,
                    snapshot,
                    busy,
                    error,
                } = &mut self.stage
                {
                    *slot = Some(session);
                    *busy = false;
                    match result {
                        Ok(fresh) => {
                            *snapshot = Some(fresh);
                            *error = None;
                        }
                        Err(reason) => {
                            *snapshot = None;
                            *error = Some(reason);
                        }
                    }
                }
                Task::none()
            }
            ClaimEvent::Submitted(session, result) => {
                match result {
                    Ok(outcome) => {
                        self.stage = Stage::Track {
                            session: Some(session),
                            outcome,
                            status: None,
                            busy: false,
                            error: None,
                        };
                        return self.reconcile();
                    }
                    Err(reason) => {
                        if let Stage::Review {
                            session: slot,
                            snapshot,
                            busy,
                            error,
                        } = &mut self.stage
                        {
                            // A refused submission consumed the review; the
                            // next confirmation needs a fresh one.
                            *slot = Some(session);
                            *snapshot = None;
                            *busy = false;
                            *error = Some(reason);
                        }
                    }
                }
                Task::none()
            }
            ClaimEvent::Tracked(session, result) => {
                if let Stage::Track {
                    session: slot,
                    status,
                    busy,
                    error,
                    ..
                } = &mut self.stage
                {
                    *slot = Some(session);
                    *busy = false;
                    match result {
                        Ok(fresh) => {
                            *status = Some(fresh);
                            *error = None;
                        }
                        Err(reason) => *error = Some(reason),
                    }
                }
                let _ = (daemon, cache);
                Task::none()
            }
        }
    }
}

impl Drop for ClaimStep1Panel {
    fn drop(&mut self) {
        self.revoke();
    }
}

impl State for ClaimStep1Panel {
    fn view<'a>(&'a self, menu: &'a Menu, cache: &'a Cache) -> Element<'a, view::Message> {
        view::vault::claim::view(menu, cache, self)
    }

    fn subscription(&self) -> iced::Subscription<Message> {
        match &self.stage {
            Stage::Sign { psbt, .. } => psbt.subscription(),
            _ => iced::Subscription::none(),
        }
    }

    fn interrupt(&mut self) {
        if let Stage::Sign { psbt, .. } = &mut self.stage {
            psbt.interrupt();
        }
    }

    fn update(
        &mut self,
        daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        let daemon = daemon.expect("Daemon required for the claim panel");
        match message {
            Message::View(view::Message::Claim(intent)) => match intent {
                view::ClaimMessage::Recheck => self.probe(daemon),
                view::ClaimMessage::Build => self.build(daemon),
                view::ClaimMessage::Sign => {
                    self.start_signing();
                    Task::none()
                }
                view::ClaimMessage::Confirm => self.confirm(),
                view::ClaimMessage::Refresh => match &self.stage {
                    Stage::Review { .. } => self.prepare_review(),
                    _ => self.reconcile(),
                },
                view::ClaimMessage::Cancel => {
                    self.cancel();
                    Task::none()
                }
            },
            Message::Claim(event) => self.apply(daemon, cache, event),
            other => {
                let Stage::Sign { psbt, .. } = &mut self.stage else {
                    return Task::none();
                };
                // The signing flow talks to a daemon that cannot store or
                // broadcast: the claim PSBT never enters the Vault's spend
                // list, so the PSBTs panel can never offer it a Broadcast
                // button of its own.
                let signing: Arc<dyn Daemon + Sync + Send> =
                    Arc::new(SigningOnlyDaemon(daemon.clone()));
                let task = psbt.update(signing, cache, other);
                Task::batch([task, self.maybe_finalize(daemon)])
            }
        }
    }

    fn reload(
        &mut self,
        daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        wallet: Option<Arc<Wallet>>,
    ) -> Task<Message> {
        if let Some(wallet) = wallet {
            self.wallet = wallet;
        }
        let Some(daemon) = daemon else {
            return Task::none();
        };
        match &self.stage {
            Stage::Preconditions => self.probe(daemon),
            Stage::Sign { psbt, .. } => psbt.load(Arc::new(SigningOnlyDaemon(daemon))),
            Stage::Track { .. } => self.reconcile(),
            Stage::Plan { .. } | Stage::Review { .. } => Task::none(),
        }
    }
}

impl From<ClaimStep1Panel> for Box<dyn State> {
    fn from(panel: ClaimStep1Panel) -> Box<dyn State> {
        Box::new(panel)
    }
}

/// Why this Vault's shape is outside what the coordinator admits, if it is.
/// The coordinator refuses these itself (`Coordinator::open`), before any
/// journal write; saying so here saves the user a build and a signature.
pub fn vault_shape_refusal(wallet: &Wallet) -> Option<String> {
    let descriptor = &wallet.main_descriptor;
    if descriptor.is_taproot() {
        return Some(
            "Claim step 1 supports native SegWit (P2WSH) Vaults for now; this Vault is Taproot."
                .to_string(),
        );
    }
    if !matches!(descriptor.policy().primary_path(), PathInfo::Single(_)) {
        return Some(
            "Claim step 1 supports a single-key primary spending path for now; this Vault's primary path needs several signatures."
                .to_string(),
        );
    }
    None
}

/// Copy for an RDTS window that refuses the OP_RETURN poison.
fn rdts_refusal(assessment: Assessment, window: &ForkWindow) -> String {
    match assessment {
        Assessment::RdtsExpired => {
            "Bitcoin Blake2b's replay protection (BIP-110) has expired, so an OP_RETURN split is no longer possible."
                .to_string()
        }
        Assessment::ExpiryMargin => {
            let left = window
                .expires_at
                .saturating_sub(window.median_time_past)
                .max(0);
            format!(
                "Bitcoin Blake2b's replay protection expires too soon for a safe split: about {} left, {} needed.",
                describe_duration(left),
                describe_duration(EXPIRY_MARGIN_SECONDS)
            )
        }
        Assessment::RdtsScheduled | Assessment::RdtsInactive => {
            "Bitcoin Blake2b's replay protection (BIP-110) isn't active yet.".to_string()
        }
        Assessment::Deployment(state) => {
            format!("Bitcoin Blake2b's status isn't usable right now ({state:?}).")
        }
        other => format!("Bitcoin Blake2b's status couldn't be assessed ({other:?})."),
    }
}

pub fn describe_duration(seconds: i64) -> String {
    let hours = seconds / 3600;
    if hours >= 48 {
        format!("{} days", hours / 24)
    } else if hours >= 1 {
        format!("{hours} hours")
    } else {
        format!("{} minutes", seconds / 60)
    }
}

/// Partition the Vault's confirmed, unspent, mature coins against the fork
/// height. A coin confirmed **below** the fork height exists on both chains
/// (the fork block is the first block the chains disagree on); anything at or
/// above it is Bitcoin-only.
pub fn partition_coins(coins: Vec<Coin>, fork_height: u64, tip_height: i32) -> CoinSet {
    let mut set = CoinSet {
        pre_fork: Vec::new(),
        post_fork: 0,
        tip_height,
    };
    for coin in coins {
        if coin.spend_info.is_some() || coin.is_immature {
            continue;
        }
        let Some(height) = coin.block_height else {
            continue;
        };
        if height >= 0 && (height as u64) < fork_height {
            set.pre_fork.push(coin);
        } else {
            set.post_fork += 1;
        }
    }
    set.pre_fork.sort_by_key(|coin| coin.outpoint);
    set
}

/// Evaluate the fork's anchor into a [`ForkWindow`]: the same projection and
/// the same deployment gate the coordinator applies later, run before a
/// transaction exists. The txid and presence handed to `project_anchor` are
/// placeholders: only the fork tip and the deployment state are read.
pub fn evaluate_anchor(
    status: crate::services::coincube::network_anchor::NetworkAnchorStatus,
    fork_hash: BlockHash,
    now: i64,
) -> Result<ForkWindow, String> {
    let fork_height = status
        .anchor
        .as_ref()
        .and_then(|a| a.observation.fork.as_ref())
        .map(|f| f.height)
        .ok_or_else(|| "the fork's activation height is missing".to_string())?;
    let (fork, deployment) = project_anchor(
        status,
        ChainId::BitcoinBlake2b,
        Txid::all_zeros(),
        ForkTransactionPresence::Unknown,
        CHECK_POLICY.observations,
        now,
    )
    .map_err(|failure| format!("{:?} at {:?}", failure.kind, failure.stage))?;
    let expires_at = match deployment.state {
        claim::DeploymentState::Flagday { expiry_time, .. } => expiry_time,
        _ => 0,
    };
    Ok(ForkWindow {
        fork_height,
        fork_hash,
        tip_height: fork.tip.height,
        median_time_past: fork.median_time_past,
        expires_at,
        rdts: claim::assess_deployment(&fork, &deployment, CHECK_POLICY.observations),
    })
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(-1)
}

async fn probe(
    daemon: Arc<dyn Daemon + Sync + Send>,
    connect: ConnectSession,
    wallet: Arc<Wallet>,
    generation: watch::Receiver<u64>,
) -> Checked {
    let expected = *generation.borrow();
    // The backend and endpoint constraints, decided by the one function that
    // owns them. A `Production` built here is dropped: it is a probe.
    let backend = Production::new(
        connect.client.clone(),
        daemon.clone(),
        connect.account.clone(),
        expected,
        generation.clone(),
    )
    .map(|_| ())
    .map_err(|error| match error {
        claim_coordinator::Error::Unsupported => {
            "This Vault must use Coincube's Bitcoin service as its node backend for a claim. Change it under Vault → Settings → Node, then come back."
                .to_string()
        }
        other => describe(other),
    });
    let window = async {
        let source = HttpObservationSource::new(
            connect.client.clone(),
            ChainId::Bitcoin,
            ChainId::BitcoinBlake2b,
            CollectionContext {
                expected_generation: expected,
                generation: generation.clone(),
            },
        )
        .map_err(|kind| format!("{kind:?}"))?;
        let status = source
            .anchor(ChainId::BitcoinBlake2b)
            .await
            .map_err(|kind| format!("{kind:?}"))?;
        let fork_height = status
            .anchor
            .as_ref()
            .and_then(|a| a.observation.fork.as_ref())
            .map(|f| f.height)
            .ok_or_else(|| "the fork's activation height is missing".to_string())?;
        let fork_hash = source
            .hash_at_height(ChainId::BitcoinBlake2b, fork_height)
            .await
            .map_err(|kind| format!("{kind:?}"))?;
        evaluate_anchor(status, *fork_hash.value(), unix_now())
    };
    let coins = async {
        let mut coins = daemon
            .list_coins(&[CoinStatus::Confirmed], &[])
            .await
            .map_err(|e| e.to_string())?
            .coins;
        wallet.apply_coin_overrides(&mut coins);
        let tip = daemon
            .get_info()
            .await
            .map_err(|e| e.to_string())?
            .block_height;
        Ok::<_, String>((coins, tip))
    };
    let feerate = async {
        FeeEstimator::new()
            .get_mid_priority_rate()
            .await
            .map(|rate| rate as u64)
            .map_err(|e| e.to_string())
    };
    let (window, coins, feerate_vb) = tokio::join!(window, coins, feerate);
    let coins = match (&window, coins) {
        (Ok(window), Ok((coins, tip))) => Ok(partition_coins(coins, window.fork_height, tip)),
        (Err(_), Ok((coins, tip))) => Ok(CoinSet {
            pre_fork: Vec::new(),
            post_fork: coins.len(),
            tip_height: tip,
        }),
        (_, Err(reason)) => Err(reason),
    };
    Checked {
        window,
        coins,
        feerate_vb,
        backend,
    }
}

struct TxMap(HashMap<Txid, Transaction>);
impl TxGetter for TxMap {
    fn get_tx(&mut self, txid: &Txid) -> Option<Transaction> {
        self.0.get(txid).cloned()
    }
}

async fn build(
    daemon: Arc<dyn Daemon + Sync + Send>,
    wallet: Arc<Wallet>,
    coins: CoinSet,
    feerate_vb: u64,
    fork_hash: BlockHash,
) -> Result<Box<PoisonSelfTransfer>, String> {
    let change_index = daemon
        .reserve_change()
        .await
        .map_err(|e| format!("couldn't reserve a change address: {e}"))?;
    let txids: Vec<Txid> = coins
        .pre_fork
        .iter()
        .map(|coin| coin.outpoint.txid)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let previous = daemon
        .list_txs(&txids)
        .await
        .map_err(|e| format!("couldn't read the coins' transactions: {e}"))?;
    let mut getter = TxMap(
        previous
            .transactions
            .into_iter()
            .map(|info| (info.tx.compute_txid(), info.tx))
            .collect(),
    );
    let candidates: Vec<CandidateCoin> = coins
        .pre_fork
        .iter()
        .map(|coin| CandidateCoin {
            outpoint: coin.outpoint,
            amount: coin.amount,
            deriv_index: coin.derivation_index,
            is_change: coin.is_change,
            must_select: true,
            sequence: None,
            ancestor_info: None,
        })
        .collect();
    let locktime = u32::try_from(coins.tip_height)
        .ok()
        .and_then(|height| LockTime::from_height(height).ok())
        .unwrap_or(LockTime::ZERO);
    let secp = secp256k1::Secp256k1::verification_only();
    create_poison_self_transfer(
        wallet.chain,
        &wallet.main_descriptor,
        &secp,
        &mut getter,
        &candidates,
        change_index,
        feerate_vb,
        locktime,
        fork_hash,
    )
    .map(Box::new)
    .map_err(|e| e.to_string())
}

/// Where this Vault's claim journal lives: beside the daemon's own data for
/// the wallet, owner-only, as the journal requires.
pub fn journal_directory(datadir: &CoincubeDirectory, wallet: &Wallet) -> PathBuf {
    datadir
        .network_directory(ChainId::Bitcoin)
        .coincubed_data_directory(&wallet.id())
        .path()
        .join("claim")
}

#[allow(clippy::too_many_arguments)]
async fn finalize_and_journal(
    built: Box<PoisonSelfTransfer>,
    signed: Psbt,
    daemon: Arc<dyn Daemon + Sync + Send>,
    connect: ConnectSession,
    bitcoin_cube: String,
    fork_cube: String,
    directory: PathBuf,
    generation: watch::Receiver<u64>,
) -> Result<Box<ClaimSession>, (Box<PoisonSelfTransfer>, String)> {
    let secp = secp256k1::Secp256k1::verification_only();
    let verified = match finalize_poison_transfer(&built, &signed, &secp) {
        Ok(verified) => verified,
        Err(error) => return Err((built, error.to_string())),
    };
    if let Err(error) = prepare_journal_directory(&directory) {
        return Err((
            built,
            format!("couldn't prepare the claim journal: {error}"),
        ));
    }
    let expected = *generation.borrow();
    let production = match Production::new(
        connect.client,
        daemon,
        connect.account,
        expected,
        generation,
    ) {
        Ok(production) => production,
        Err(error) => return Err((built, describe(error))),
    };
    let context = production.context().clone();
    match Coordinator::create(
        &directory,
        bitcoin_cube,
        fork_cube,
        &built,
        verified,
        production,
        CHECK_POLICY,
    ) {
        Ok(coordinator) => Ok(Box::new(ClaimSession {
            coordinator,
            context,
            review: None,
        })),
        Err(error) => Err((built, describe(error))),
    }
}

fn prepare_journal_directory(directory: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// User-facing copy for a coordinator refusal. Never a retry instruction for
/// an uncertain submission.
pub fn describe(error: claim_coordinator::Error) -> String {
    use claim_coordinator::Error as E;
    match error {
        E::Unsupported => "This Vault or its node backend isn't supported for a claim.".to_string(),
        E::InvalidBinding => {
            "The claim's chain binding didn't match this Vault. Reopen the Cube and try again."
                .to_string()
        }
        E::Revoked => {
            "The claim session ended (signed out, or the Cube changed). Start again.".to_string()
        }
        E::InvalidReview | E::ChangedReview => {
            "What you reviewed has changed since. Review the transaction again.".to_string()
        }
        E::Journal(error) => format!("Couldn't record the claim on this device ({error:?})."),
        E::Observation(failure) => format!(
            "Couldn't observe the chains ({:?} while reading {:?}).",
            failure.kind, failure.stage
        ),
        E::Preflight(error) => format!("The Bitcoin node's preflight check failed ({error:?})."),
        E::PolicyRejected(policy) => match policy {
            crate::services::claim_preflight::NodePolicy::Rejected { reason } => {
                format!("The Bitcoin node rejected the transaction: {reason}")
            }
            crate::services::claim_preflight::NodePolicy::Accepted => {
                "The Bitcoin node's answer was inconsistent.".to_string()
            }
        },
        E::NotReady(assessment) => format!("Not ready to submit: {assessment:?}."),
        E::SubmissionAlreadyRecorded => {
            "A submission is already recorded for this claim; it can only be tracked now."
                .to_string()
        }
        E::ExpiredEvidence => {
            "The evidence went stale before submission. Review the transaction again.".to_string()
        }
    }
}

/// The daemon the signing flow sees: everything a signer needs, and nothing
/// that could store, label, delete or broadcast the PSBT. The claim PSBT thus
/// never enters the Vault's spend list, where the PSBTs panel would offer it
/// a Broadcast button that bypasses the coordinator's journal and preflight.
#[derive(Debug)]
pub struct SigningOnlyDaemon(pub Arc<dyn Daemon + Sync + Send>);

#[async_trait::async_trait]
impl Daemon for SigningOnlyDaemon {
    fn backend(&self) -> DaemonBackend {
        self.0.backend()
    }
    fn config(&self) -> Option<&coincubed::config::Config> {
        self.0.config()
    }
    fn invalidate_connect_session(&self) {
        self.0.invalidate_connect_session()
    }
    async fn is_alive(
        &self,
        datadir: &CoincubeDirectory,
        network: Network,
    ) -> Result<(), DaemonError> {
        self.0.is_alive(datadir, network).await
    }
    async fn stop(&self) -> Result<(), DaemonError> {
        self.0.stop().await
    }
    async fn get_info(&self) -> Result<model::GetInfoResult, DaemonError> {
        self.0.get_info().await
    }
    async fn request_sync(&self) -> Result<(), DaemonError> {
        self.0.request_sync().await
    }
    async fn get_new_address(&self) -> Result<model::GetAddressResult, DaemonError> {
        self.0.get_new_address().await
    }
    async fn list_revealed_addresses(
        &self,
        is_change: bool,
        exclude_used: bool,
        limit: usize,
        start_index: Option<ChildNumber>,
    ) -> Result<model::ListRevealedAddressesResult, DaemonError> {
        self.0
            .list_revealed_addresses(is_change, exclude_used, limit, start_index)
            .await
    }
    async fn update_deriv_indexes(
        &self,
        receive: Option<u32>,
        change: Option<u32>,
    ) -> Result<UpdateDerivIndexesResult, DaemonError> {
        self.0.update_deriv_indexes(receive, change).await
    }
    async fn list_coins(
        &self,
        statuses: &[CoinStatus],
        outpoints: &[OutPoint],
    ) -> Result<model::ListCoinsResult, DaemonError> {
        self.0.list_coins(statuses, outpoints).await
    }
    async fn list_spend_txs(&self) -> Result<model::ListSpendResult, DaemonError> {
        self.0.list_spend_txs().await
    }
    async fn create_spend_tx(
        &self,
        coins_outpoints: &[OutPoint],
        destinations: &HashMap<Address<address::NetworkUnchecked>, u64>,
        feerate_vb: u64,
        change_address: Option<Address<address::NetworkUnchecked>>,
    ) -> Result<model::CreateSpendResult, DaemonError> {
        self.0
            .create_spend_tx(coins_outpoints, destinations, feerate_vb, change_address)
            .await
    }
    async fn rbf_psbt(
        &self,
        txid: &Txid,
        is_cancel: bool,
        feerate_vb: Option<u64>,
    ) -> Result<model::CreateSpendResult, DaemonError> {
        self.0.rbf_psbt(txid, is_cancel, feerate_vb).await
    }
    /// The signing flow persists the merged PSBT after every signature. The
    /// claim PSBT is held in memory by the panel instead: nothing is stored.
    async fn update_spend_tx(&self, _psbt: &Psbt) -> Result<(), DaemonError> {
        Ok(())
    }
    async fn delete_spend_tx(&self, _txid: &Txid) -> Result<(), DaemonError> {
        Ok(())
    }
    /// Only the coordinator submits, through `submit_verified_poison`.
    async fn broadcast_spend_tx(&self, _txid: &Txid) -> Result<(), DaemonError> {
        Err(DaemonError::ClientNotSupported)
    }
    async fn start_rescan(&self, t: u32) -> Result<(), DaemonError> {
        self.0.start_rescan(t).await
    }
    async fn list_confirmed_txs(
        &self,
        start: u32,
        end: u32,
        limit: u64,
    ) -> Result<model::ListTransactionsResult, DaemonError> {
        self.0.list_confirmed_txs(start, end, limit).await
    }
    async fn create_recovery(
        &self,
        address: Address<address::NetworkUnchecked>,
        coins_outpoints: &[OutPoint],
        feerate_vb: u64,
        sequence: Option<u16>,
    ) -> Result<Psbt, DaemonError> {
        self.0
            .create_recovery(address, coins_outpoints, feerate_vb, sequence)
            .await
    }
    async fn list_txs(&self, txid: &[Txid]) -> Result<model::ListTransactionsResult, DaemonError> {
        self.0.list_txs(txid).await
    }
    async fn get_labels(
        &self,
        labels: &HashSet<LabelItem>,
    ) -> Result<HashMap<String, String>, DaemonError> {
        self.0.get_labels(labels).await
    }
    /// Labels ride along with the first persist of an unsaved PSBT; there is
    /// no stored PSBT to label.
    async fn update_labels(
        &self,
        _labels: &HashMap<LabelItem, Option<String>>,
    ) -> Result<(), DaemonError> {
        Ok(())
    }
    async fn get_labels_bip329(&self, offset: u32, limit: u32) -> Result<Labels, DaemonError> {
        self.0.get_labels_bip329(offset, limit).await
    }
    async fn send_wallet_invitation(&self, email: &str) -> Result<(), DaemonError> {
        self.0.send_wallet_invitation(email).await
    }
    async fn update_wallet_metadata(
        &self,
        wallet_alias: Option<String>,
        fingerprint_aliases: &HashMap<
            coincube_core::miniscript::bitcoin::bip32::Fingerprint,
            String,
        >,
        hws: &[HardwareWalletConfig],
    ) -> Result<(), DaemonError> {
        self.0
            .update_wallet_metadata(wallet_alias, fingerprint_aliases, hws)
            .await
    }
}

#[cfg(test)]
mod tests;
