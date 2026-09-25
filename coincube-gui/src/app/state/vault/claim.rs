//! Claim step 1 — the poison self-transfer on Bitcoin (Lane B1.5, slice 1).
//!
//! One panel, five stages: preconditions → build → sign → review → track.
//! The panel owns **no signing code and no broadcaster**: signing is the
//! Vault's own [`PsbtState`] flow (hot key, hardware, Keychain — unchanged),
//! and submission is `services::claim_coordinator`, which journals the
//! intent before it hands the verified transaction to the embedded daemon.
//!
//! A claim's context can end under it — a Connect sign-out or account
//! change, a node backend switch — and the App revokes the coordinator
//! synchronously when it does. A journaled intent is never abandoned by that:
//! the panel keeps the construction and the signatures, and re-binds the
//! journal under the new context (`Coordinator::resume`, the same account and
//! provider) on the next sign-in or return to the panel.
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
        claim_workflow::{self, Context, Phase, Status},
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

/// The context the coordinator was created under ended while a task held
/// it, or the coordinator refused for that reason; the intent is journaled.
/// The generic wording — a sign-out and a backend switch have their own.
pub const SESSION_ENDED: &str =
    "The claim session ended. This claim is recorded on this device; read again to continue.";
/// Signed out of Connect at Review or Track. Only a sign-in in this tab
/// continues the claim: not returning to the panel, not a cached account
/// callback — a sign-out in another tab revokes the claim here too, and the
/// account this tab still shows is not a session.
pub const SIGNED_OUT_AT_REVIEW: &str = "Signed out of Connect. This claim is recorded on this device; sign in again in this tab to continue.";
/// Another Connect account signed in from another tab (or one signed in
/// while this tab had none). The claim is held here until this tab signs in.
pub const SIGNED_IN_ELSEWHERE: &str = "Connect signed in with a different account in another tab. This claim is recorded on this device; sign in again in this tab to continue.";
/// The node backend is being replaced: nothing is probed, built, finalised
/// or re-bound until the App reports how the switch settled.
pub const BACKEND_SWITCHING: &str = "The Bitcoin node backend is switching. This claim is recorded on this device and continues once the switch completes.";
/// The switch task panicked: the App keeps the pre-switch daemon in an
/// unknown state, and the claim is not re-bound to it.
pub const BACKEND_UNKNOWN: &str = "The node backend switch did not complete and the node's state is unknown. This claim is recorded on this device; switch the backend again under Vault → Settings → Node to continue.";
/// A fully signed construction is waiting for a session to be recorded under.
pub const SIGNED_OUT_AT_SIGN: &str =
    "Signed out of Connect. Sign in again to record and submit the claim.";
/// The session ended between the last signature and the journal write: the
/// construction and every signature are kept, nothing was recorded.
pub const SIGNED_OUT_BEFORE_RECORD: &str =
    "Signed out of Connect before the claim was recorded. Sign in again to record and submit it.";
/// The journal belongs to another Connect account than the one signed in.
pub const OTHER_ACCOUNT: &str = "This claim was recorded under a different Connect account. Sign in with that account to continue.";
/// A step that reads or writes through the Vault's daemon was asked for
/// while the App has none (a failed backend switch leaves it that way).
pub const NODE_UNAVAILABLE: &str = "The Vault's node isn't available right now, so this step can't run. Check Vault → Settings → Node, then try again.";
/// The claim target's Cube left this device while a claim was journaled.
pub const TARGET_GONE: &str =
    "The claim target is no longer on this device. Create it again, then come back.";
/// `Production::new`'s backend refusal, in the words a user can act on.
const BACKEND_UNSUPPORTED: &str = "This Vault must use Coincube's Bitcoin service as its node backend for a claim. Change it under Vault → Settings → Node, then come back.";

/// Where the build's fee rate comes from. The estimator asks public fee
/// APIs; a fixed rate is for tests, which must never reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeerateSource {
    Estimator,
    Fixed(u64),
}

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
    /// Confirmed at or above the fork height: left out of this step, and
    /// not known to be Bitcoin-only — a transaction can be replayed onto the
    /// fork, so they may still be entangled. Counted for the copy; slice 2's
    /// input poison is where they become useful.
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

/// A journaled claim plus everything a task needs to drive it. Moved into
/// each async step and handed back with the result, so exactly one owner
/// ever touches the coordinator. The revoker is cloned out before the move,
/// so revocation never waits for a task to return.
pub struct ClaimSession {
    /// `None` between a re-bind that released a revoked coordinator (it
    /// holds the journal's lock) and the next successful one; the journal on
    /// disk is the record either way.
    coordinator: Option<Coordinator>,
    context: Context,
    review: Option<Review>,
    /// Kept for a re-bind: `Coordinator::resume` re-validates the
    /// construction against the journal and re-verifies the signatures.
    built: Box<PoisonSelfTransfer>,
    signed: Psbt,
    /// The journal phase as last read through a live coordinator.
    phase: Phase,
}

impl fmt::Debug for ClaimSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimSession")
            .field("phase", &self.phase())
            .field("bound", &self.coordinator.is_some())
            .field("reviewed", &self.review.is_some())
            .finish_non_exhaustive()
    }
}

impl ClaimSession {
    pub fn phase(&self) -> Phase {
        self.coordinator
            .as_ref()
            .map_or(self.phase, |coordinator| coordinator.phase())
    }

    /// Whether a coordinator is bound. A bound coordinator may still be
    /// revoked; only its own calls say so.
    pub fn is_bound(&self) -> bool {
        self.coordinator.is_some()
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
    /// keep signing. The number names the finalisation attempt it answers.
    Ready(
        u64,
        Result<Box<ClaimSession>, (Box<PoisonSelfTransfer>, String)>,
    ),
    /// A revoked session was re-bound under the current context — or could
    /// not be, in which case it comes back unbound with the reason. The
    /// number is the panel's revocation count when the re-bind was sent.
    Rebound(u64, Box<ClaimSession>, Result<(), String>),
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
    /// finalise task holds it, and `finalizing` names that task.
    Sign {
        built: Option<Box<PoisonSelfTransfer>>,
        psbt: Box<PsbtState>,
        finalizing: Option<FinalizeAttempt>,
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

/// What the App's daemon is, as far as a claim may bind to it. Set by the
/// App around a node backend switch; only `Ready` lets the panel probe,
/// build, finalise or re-bind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendState {
    Ready,
    /// A switch is in flight: the App still holds the daemon being replaced.
    Switching,
    /// The switch failed and nothing was recovered: the App has no daemon.
    Unavailable,
    /// The switch task panicked: the App keeps the pre-switch daemon in an
    /// unknown state. A later successful switch is the only way back.
    Unknown,
}

impl BackendState {
    /// The stage copy for a state that holds the claim, `None` for `Ready`.
    fn copy(self) -> Option<&'static str> {
        match self {
            BackendState::Ready => None,
            BackendState::Switching => Some(BACKEND_SWITCHING),
            BackendState::Unavailable => Some(NODE_UNAVAILABLE),
            BackendState::Unknown => Some(BACKEND_UNKNOWN),
        }
    }
}

/// One finalisation in flight: which attempt, and the context it was sent
/// under. Its result is authorised only if that context is still the
/// panel's when it arrives — the App may have signed out, or revoked, in
/// between, and the task has no coordinator to revoke until it returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FinalizeAttempt {
    id: u64,
    generation: u64,
    revocations: u64,
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
    /// Set by [`Self::revoke`] and by a completion that arrived after its
    /// context ended; cleared when a coordinator is bound under the current
    /// context. While set, a journaled session is re-bound rather than
    /// driven.
    revoked: bool,
    /// Counts every [`Self::revoke`], so a task sent before a revocation is
    /// told apart from one sent after it.
    revocations: u64,
    /// The App's daemon as a claim may bind to it; see [`Self::set_backend`].
    backend: BackendState,
    check_seq: u64,
    finalize_attempts: u64,
    feerate: FeerateSource,
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
            revoked: false,
            revocations: 0,
            backend: BackendState::Ready,
            check_seq: 0,
            finalize_attempts: 0,
            feerate: FeerateSource::Estimator,
        };
        panel.refresh_static_preconditions();
        panel
    }

    /// Replace the fee-rate source (tests: a fixed rate, no network).
    pub fn with_feerate_source(mut self, feerate: FeerateSource) -> Self {
        self.feerate = feerate;
        self
    }

    /// The App's hook for a Connect session change. Returns whether a
    /// session the panel was working under was removed or replaced — a
    /// sign-out, or a different account or client identity — in which case
    /// any live coordinator has been revoked synchronously and the App
    /// advances the generation (which every in-flight coordinator call
    /// checks). A first sign-in only makes the next step possible; the App
    /// then calls [`Self::recover`].
    pub fn set_connect(&mut self, connect: Option<ConnectSession>) -> bool {
        let replaced = match (&self.connect, &connect) {
            (Some(_), None) => true,
            (Some(old), Some(new)) => !same_session(old, new),
            (None, _) => false,
        };
        let signed_out = replaced && connect.is_none();
        self.connect = connect;
        if replaced {
            self.revoke();
            self.note_session_ended(signed_out);
        }
        replaced
    }

    /// Revoke the live coordinator, synchronously. Called by the App before
    /// it replaces the context the coordinator was created under (Connect
    /// sign-out or account change, node backend switch, Cube lock or close),
    /// and by `Drop`. Idempotent. A finalisation in flight has no
    /// coordinator yet: the count advanced here tells its result apart from
    /// one sent after the revocation.
    pub fn revoke(&mut self) {
        self.revoked = true;
        self.revocations = self.revocations.wrapping_add(1);
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
            } => {
                if let Some(coordinator) = &mut session.coordinator {
                    coordinator.invalidate();
                }
            }
            _ => {}
        }
    }

    /// Say on the current stage that the session it was working under
    /// ended. A review that was on screen is withdrawn: it can only be
    /// confirmed under the session it was prepared under.
    fn note_session_ended(&mut self, signed_out: bool) {
        if signed_out {
            self.note(Some(SIGNED_OUT_AT_SIGN), SIGNED_OUT_AT_REVIEW);
        } else {
            self.note(None, SESSION_ENDED);
        }
    }

    /// Revoke the live coordinator and withdraw a review on screen for a
    /// re-read under this panel's own session, which stands: the App's
    /// answer to a sibling tab's same-account sign-in or refresh. The next
    /// Refresh or entry re-binds.
    pub fn revoke_and_withdraw(&mut self) {
        self.revoke();
        self.note(None, SESSION_ENDED);
    }

    /// Put `copy` on the current stage after the App holds the claim for a
    /// reason of its own (see `App::on_global_auth_change`).
    pub fn note_hold(&mut self, copy: &str) {
        self.note(Some(copy), copy);
    }

    /// Put `at_review` on a Review (withdrawing its snapshot) or Track, and
    /// `at_sign` on a Sign stage that is not finalising.
    fn note(&mut self, at_sign: Option<&str>, at_review: &str) {
        match &mut self.stage {
            Stage::Sign {
                finalizing: None,
                error,
                ..
            } => {
                if let Some(copy) = at_sign {
                    *error = Some(copy.to_string());
                }
            }
            Stage::Review {
                snapshot, error, ..
            } => {
                *snapshot = None;
                *error = Some(at_review.to_string());
            }
            Stage::Track { error, .. } => *error = Some(at_review.to_string()),
            _ => {}
        }
    }

    /// The copy for a completion whose context ended while its task ran.
    fn ended_copy(&self) -> String {
        if self.connect.is_none() {
            SIGNED_OUT_AT_REVIEW.to_string()
        } else {
            self.backend.copy().unwrap_or(SESSION_ENDED).to_string()
        }
    }

    /// The App's hook around a node backend switch. `Switching` revokes the
    /// live coordinator — it is bound to the daemon being replaced — and
    /// holds every route that could bind a new one (probe, build, finalise,
    /// re-bind) until the App reports how the switch settled. `Ready` lets
    /// them run again, and the App calls [`Self::recover`] right after with
    /// the daemon it installed or recovered. `Unavailable` and `Unknown`
    /// keep holding, each with its own copy; `Unknown` in particular is not
    /// re-bound to the pre-switch daemon the App keeps, whose state nobody
    /// knows.
    pub fn set_backend(&mut self, state: BackendState) {
        self.backend = state;
        if state == BackendState::Switching {
            self.revoke();
        }
        match state.copy() {
            Some(copy) => self.note(Some(copy), copy),
            None => {
                let held = [BACKEND_SWITCHING, NODE_UNAVAILABLE, BACKEND_UNKNOWN];
                if let Stage::Sign { error, .. }
                | Stage::Review { error, .. }
                | Stage::Track { error, .. } = &mut self.stage
                {
                    if error.as_deref().is_some_and(|e| held.contains(&e)) {
                        *error = None;
                    }
                }
            }
        }
    }

    /// Whether the App's daemon may be bound to right now.
    fn backend_ready(&self) -> bool {
        self.backend == BackendState::Ready
    }

    /// Whether a task sent under `generation` and `revocations` may still
    /// act on this panel: the same context, a session present, and no
    /// revocation since.
    fn authorized(&self, generation: u64, revocations: u64) -> bool {
        self.connect.is_some()
            && *self.generation.borrow() == generation
            && self.revocations == revocations
    }

    /// After the Connect session or the daemon changed: at Sign, finalise
    /// the construction if it is ready and a session is back; at Review or
    /// Track with a revoked coordinator, re-bind the journaled claim under
    /// the current context. Called by the App after every session change
    /// and on entry, so a claim revoked by a sign-out or a backend switch
    /// continues once its context is back. Nothing to do without a daemon.
    pub fn recover(&mut self, daemon: Option<Arc<dyn Daemon + Sync + Send>>) -> Task<Message> {
        let Some(daemon) = daemon else {
            return Task::none();
        };
        // Not while the App's daemon is being replaced, or is in a state no
        // claim should bind to: the App calls again once it settles.
        if !self.backend_ready() {
            return Task::none();
        }
        match &self.stage {
            Stage::Sign { .. } => self.maybe_finalize(daemon),
            Stage::Review { .. } | Stage::Track { .. } if self.revoked => self.rebind(daemon),
            _ => Task::none(),
        }
    }

    /// Re-bind a revoked session: a new `Production` under the current
    /// generation and Connect session, `Coordinator::resume` on the journal
    /// the first coordinator wrote — the same account and provider reopen
    /// it, a different account is refused by the journal's own identity
    /// check — with the construction re-validated and the signatures
    /// re-verified. The revoked coordinator is released first (it holds the
    /// journal's lock), so a refused re-bind leaves the session unbound,
    /// to be tried again on the next sign-in or return to the panel.
    fn rebind(&mut self, daemon: Arc<dyn Daemon + Sync + Send>) -> Task<Message> {
        if !self.backend_ready() {
            return Task::none();
        }
        self.refresh_static_preconditions();
        let Some(connect) = self.connect.clone() else {
            return Task::none();
        };
        let Some(target) = self.pre.target.clone() else {
            if let Stage::Review { error, .. } | Stage::Track { error, .. } = &mut self.stage {
                *error = Some(TARGET_GONE.to_string());
            }
            return Task::none();
        };
        let Some(mut session) = self.take_session() else {
            return Task::none();
        };
        let expected = *self.generation.borrow();
        let revocations = self.revocations;
        let generation = self.generation.clone();
        let directory = journal_directory(&self.datadir, &self.wallet);
        let bitcoin_cube = self.bitcoin_cube.clone();
        Task::perform(
            async move {
                let result = rebind_session(
                    &mut session,
                    daemon,
                    connect,
                    bitcoin_cube,
                    target,
                    directory,
                    expected,
                    generation,
                );
                (session, result)
            },
            move |(session, result)| {
                Message::Claim(ClaimEvent::Rebound(revocations, session, result))
            },
        )
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
                finalizing: finalizing.is_some(),
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

    /// The account of the session the panel works under, for tests at the
    /// App level (the field itself is the panel's).
    #[cfg(test)]
    pub(crate) fn connect_account(&self) -> Option<&str> {
        self.connect.as_ref().map(|c| c.account.as_str())
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
        if let Some(copy) = self.backend.copy() {
            return refuse(copy, self.backend != BackendState::Unknown);
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
        // A refusal no probe can change: don't spend the network on it. Nor
        // a daemon that is being replaced: `refusal` says so.
        if self.pre.target.is_none() || self.pre.shape.is_some() || !self.backend_ready() {
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
        let feerate = self.feerate;
        Task::perform(
            async move { Box::new(probe(daemon, connect, wallet, generation, feerate).await) },
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
            finalizing: None,
            error: None,
        };
    }

    /// After every message the signing flow saw: once the primary path is
    /// satisfied and the picker has closed, finalise and journal. The
    /// construction leaves with the task and comes back if anything refuses.
    fn maybe_finalize(&mut self, daemon: Arc<dyn Daemon + Sync + Send>) -> Task<Message> {
        // A finalisation binds the daemon it is given: not one being
        // replaced. The construction and its signatures wait.
        if let Some(copy) = self.backend.copy() {
            if let Stage::Sign {
                finalizing: None,
                error,
                ..
            } = &mut self.stage
            {
                if error.as_deref() != Some(copy) {
                    *error = Some(copy.to_string());
                }
            }
            return Task::none();
        }
        let target = self.pre.target.clone();
        let connect = self.connect.clone();
        let directory = journal_directory(&self.datadir, &self.wallet);
        let bitcoin_cube = self.bitcoin_cube.clone();
        // The context this attempt is sent under, read here and not in the
        // task: a sign-out between dispatch and the task's first poll
        // advances the generation, and the old client must not be bound to
        // the new one.
        let expected = *self.generation.borrow();
        let revocations = self.revocations;
        let generation = self.generation.clone();
        let id = self.finalize_attempts.wrapping_add(1);
        let Stage::Sign {
            built,
            psbt,
            finalizing,
            error,
        } = &mut self.stage
        else {
            return Task::none();
        };
        if finalizing.is_some()
            || built.is_none()
            || psbt.modal.is_some()
            || psbt.tx.path_ready().is_none()
        {
            return Task::none();
        }
        let (Some(target), Some(connect)) = (target, connect) else {
            // Only the session is missing: keep the construction and every
            // signature, and say what is needed. The App hands a new session
            // in as soon as the account signs in again.
            let missing = if self.pre.target.is_none() {
                TARGET_GONE
            } else {
                SIGNED_OUT_AT_SIGN
            };
            if error.as_deref() != Some(missing) {
                *error = Some(missing.to_string());
            }
            return Task::none();
        };
        let built = built.take().expect("checked above");
        *finalizing = Some(FinalizeAttempt {
            id,
            generation: expected,
            revocations,
        });
        *error = None;
        self.finalize_attempts = id;
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
                    expected,
                    generation,
                )
                .await
            },
            move |ready| Message::Claim(ClaimEvent::Ready(id, ready)),
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
                let result = match session.coordinator.as_mut() {
                    Some(coordinator) => match coordinator.prepare_review(&context).await {
                        Ok(review) => {
                            let snapshot = review.snapshot().clone();
                            session.review = Some(review);
                            Ok(snapshot)
                        }
                        Err(error) => Err(describe(error)),
                    },
                    None => Err(SESSION_ENDED.to_string()),
                };
                (session, result)
            },
            |(session, result)| Message::Claim(ClaimEvent::Reviewed(session, result)),
        )
    }

    fn confirm(&mut self) -> Task<Message> {
        // A review is confirmed under the session it was prepared under; a
        // sign-out withdraws it (`note_session_ended`), so this is the guard
        // for a session that went missing some other way.
        if self.connect.is_none() {
            return Task::none();
        }
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
                let result = match (session.coordinator.as_mut(), session.review.take()) {
                    (Some(coordinator), Some(review)) => coordinator
                        .confirm_and_submit(review, &context)
                        .await
                        .map_err(describe),
                    (None, _) => Err(SESSION_ENDED.to_string()),
                    (Some(_), None) => Err(
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
                let result = match session.coordinator.as_mut() {
                    Some(coordinator) => coordinator.reconcile(&context).await.map_err(describe),
                    None => Err(SESSION_ENDED.to_string()),
                };
                (session, result)
            },
            |(session, result)| Message::Claim(ClaimEvent::Tracked(session, result)),
        )
    }

    /// Abandon an attempt that has not been journaled. A journaled intent is
    /// never abandoned from here: it is reconciled. Nothing is abandoned
    /// while a finalisation holds the construction either: its result
    /// decides whether the attempt was journaled.
    fn cancel(&mut self) {
        match &self.stage {
            Stage::Sign {
                finalizing: Some(_),
                ..
            } => {}
            Stage::Plan { .. } | Stage::Sign { .. } => {
                self.stage = Stage::Preconditions;
                // The reserved change index is not reused: the daemon's
                // reservation is durable, so the next build reserves anew.
                self.pre.checked = None;
            }
            Stage::Preconditions | Stage::Review { .. } | Stage::Track { .. } => {}
        }
    }

    /// Every completion carries what it needs; none needs the daemon, so a
    /// session travelling inside one is never dropped for want of it.
    fn apply(&mut self, event: ClaimEvent) -> Task<Message> {
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
            ClaimEvent::Ready(id, result) => {
                let attempt = match &self.stage {
                    Stage::Sign {
                        finalizing: Some(attempt),
                        ..
                    } if attempt.id == id => Some(*attempt),
                    _ => None,
                };
                match result {
                    Ok(mut session) => {
                        // The intent is journaled, so from here the session
                        // is the record whether or not it may be driven: a
                        // result whose context ended in flight is installed
                        // revoked, to be re-bound, never reviewed.
                        let authorized =
                            attempt.is_some_and(|a| self.authorized(a.generation, a.revocations));
                        if !authorized {
                            if let Some(coordinator) = &mut session.coordinator {
                                coordinator.invalidate();
                            }
                        }
                        self.revoker = session.coordinator.as_ref().map(|c| c.revoker());
                        self.revoked = !authorized;
                        let ended = self.ended_copy();
                        self.stage = Stage::Review {
                            session: Some(session),
                            snapshot: None,
                            busy: false,
                            error: (!authorized).then_some(ended),
                        };
                        if authorized {
                            self.prepare_review()
                        } else {
                            Task::none()
                        }
                    }
                    Err((built, reason)) => {
                        // A refusal for a superseded attempt has nothing to
                        // restore into (unreachable while cancel is refused
                        // during a finalisation; kept as the guard).
                        if attempt.is_none() {
                            return Task::none();
                        }
                        if let Stage::Sign {
                            built: slot,
                            finalizing,
                            error,
                            ..
                        } = &mut self.stage
                        {
                            *slot = Some(built);
                            *finalizing = None;
                            *error = Some(reason.clone());
                        }
                        Task::done(Message::View(view::Message::ShowError(reason)))
                    }
                }
            }
            ClaimEvent::Rebound(revocations, mut session, result) => {
                let bound = result.is_ok();
                let authorized = bound && self.authorized(session.context.generation, revocations);
                if bound && !authorized {
                    // Bound under a context that ended while the re-bind ran.
                    if let Some(coordinator) = &mut session.coordinator {
                        coordinator.invalidate();
                    }
                }
                if bound {
                    self.revoker = session.coordinator.as_ref().map(|c| c.revoker());
                }
                let error = match result {
                    Err(reason) => Some(reason),
                    Ok(()) if authorized => None,
                    Ok(()) => Some(self.ended_copy()),
                };
                match &mut self.stage {
                    Stage::Review {
                        session: slot,
                        snapshot,
                        busy,
                        error: slot_error,
                    } => {
                        *slot = Some(session);
                        *snapshot = None;
                        *busy = false;
                        *slot_error = error;
                    }
                    Stage::Track {
                        session: slot,
                        busy,
                        error: slot_error,
                        ..
                    } => {
                        *slot = Some(session);
                        *busy = false;
                        *slot_error = error;
                    }
                    _ => return Task::none(),
                }
                if !authorized {
                    return Task::none();
                }
                self.revoked = false;
                match &self.stage {
                    Stage::Review { .. } => self.prepare_review(),
                    _ => self.reconcile(),
                }
            }
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

    /// The App may have no daemon (a backend switch that failed without
    /// recovery leaves it so). A completion is processed regardless — a
    /// session travels inside it — and an intent that needs the daemon is
    /// refused with the reason, never with a panic.
    fn update(
        &mut self,
        daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        let node_unavailable = || {
            Task::done(Message::View(view::Message::ShowError(
                NODE_UNAVAILABLE.to_string(),
            )))
        };
        match message {
            Message::View(view::Message::Claim(intent)) => match intent {
                view::ClaimMessage::Recheck => match daemon {
                    Some(daemon) => self.probe(daemon),
                    None => node_unavailable(),
                },
                view::ClaimMessage::Build => match daemon {
                    Some(daemon) => self.build(daemon),
                    None => node_unavailable(),
                },
                view::ClaimMessage::Sign => {
                    self.start_signing();
                    Task::none()
                }
                view::ClaimMessage::Confirm => self.confirm(),
                view::ClaimMessage::Refresh => match &self.stage {
                    Stage::Review { .. } | Stage::Track { .. } if self.revoked => {
                        match (self.backend.copy(), daemon) {
                            // Held by the App's backend state: say so, bind nothing.
                            (Some(copy), _) => Task::done(Message::View(view::Message::ShowError(
                                copy.to_string(),
                            ))),
                            (None, Some(daemon)) => self.recover(Some(daemon)),
                            (None, None) => node_unavailable(),
                        }
                    }
                    Stage::Review { .. } => self.prepare_review(),
                    _ => self.reconcile(),
                },
                view::ClaimMessage::Cancel => {
                    self.cancel();
                    Task::none()
                }
            },
            Message::Claim(event) => self.apply(event),
            other => {
                let Stage::Sign { psbt, error, .. } = &mut self.stage else {
                    return Task::none();
                };
                let Some(daemon) = daemon else {
                    // The signing flow cannot be driven without a daemon;
                    // the construction and every signature are kept.
                    if error.as_deref() != Some(NODE_UNAVAILABLE) {
                        *error = Some(NODE_UNAVAILABLE.to_string());
                    }
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
            Stage::Sign { psbt, .. } => {
                let load = psbt.load(Arc::new(SigningOnlyDaemon(daemon.clone())));
                // A construction signed while the session was missing is
                // finalised now that one may be back.
                Task::batch([load, self.maybe_finalize(daemon)])
            }
            // A session revoked by a sign-out or a backend switch is
            // re-bound on entry, under whatever context is current now.
            Stage::Review { .. } | Stage::Track { .. } if self.revoked => {
                self.recover(Some(daemon))
            }
            Stage::Track { .. } => self.reconcile(),
            Stage::Plan { .. } | Stage::Review { .. } => Task::none(),
        }
    }
}

/// The same Connect session: account, endpoint and credential.
fn same_session(a: &ConnectSession, b: &ConnectSession) -> bool {
    a.account == b.account
        && a.client.base_url == b.client.base_url
        && a.client.token() == b.client.token()
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
    if claim_coordinator::admits_descriptor(descriptor) {
        return None;
    }
    // The verdict is the coordinator's; only the wording is chosen here.
    Some(if descriptor.is_taproot() {
        "Claim step 1 supports native SegWit (P2WSH) Vaults for now; this Vault is Taproot."
            .to_string()
    } else {
        "Claim step 1 supports a single-key primary spending path for now; this Vault's primary path needs several signatures."
            .to_string()
    })
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
/// (the fork block is the first block the chains disagree on) and is what
/// this step splits; anything at or above it is left out — not known to be
/// Bitcoin-only, since a transaction can be replayed onto the fork.
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
    feerate: FeerateSource,
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
    .map_err(describe_production);
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
        match feerate {
            FeerateSource::Fixed(rate) => Ok(rate),
            FeerateSource::Estimator => FeeEstimator::new()
                .get_mid_priority_rate()
                .await
                .map(|rate| rate as u64)
                .map_err(|e| e.to_string()),
        }
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

/// Finalise and journal under the context captured at dispatch (`expected`).
/// If the generation moved in between — the App signed out — the attempt is
/// refused here, before the journal directory is touched, and the
/// construction goes back with every signature; `Coordinator::open` would
/// refuse the same binding, this just says why first.
#[allow(clippy::too_many_arguments)]
async fn finalize_and_journal(
    built: Box<PoisonSelfTransfer>,
    signed: Psbt,
    daemon: Arc<dyn Daemon + Sync + Send>,
    connect: ConnectSession,
    bitcoin_cube: String,
    fork_cube: String,
    directory: PathBuf,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<Box<ClaimSession>, (Box<PoisonSelfTransfer>, String)> {
    if *generation.borrow() != expected || generation.has_changed().is_err() {
        return Err((built, SIGNED_OUT_BEFORE_RECORD.to_string()));
    }
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
    let production = match Production::new(
        connect.client,
        daemon,
        connect.account,
        expected,
        generation,
    ) {
        Ok(production) => production,
        Err(error) => return Err((built, describe_production(error))),
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
            phase: coordinator.phase(),
            coordinator: Some(coordinator),
            context,
            review: None,
            built,
            signed,
        })),
        Err(error) => Err((built, describe(error))),
    }
}

/// Re-bind `session` to the journal it wrote, under the current context.
/// Synchronous — file reads, signature verification, no network.
#[allow(clippy::too_many_arguments)]
fn rebind_session(
    session: &mut ClaimSession,
    daemon: Arc<dyn Daemon + Sync + Send>,
    connect: ConnectSession,
    bitcoin_cube: String,
    fork_cube: String,
    directory: PathBuf,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<(), String> {
    // The revoked coordinator holds the journal's lock: release it first.
    // Nothing of it is needed again; the journal on disk is the record.
    if let Some(old) = session.coordinator.take() {
        session.phase = old.phase();
        drop(old);
    }
    session.review = None;
    let production = Production::new(
        connect.client,
        daemon,
        connect.account,
        expected,
        generation,
    )
    .map_err(describe_production)?;
    let context = production.context().clone();
    let secp = secp256k1::Secp256k1::verification_only();
    let verified = finalize_poison_transfer(&session.built, &session.signed, &secp)
        .map_err(|error| error.to_string())?;
    let coordinator = Coordinator::resume(
        &directory,
        bitcoin_cube,
        fork_cube,
        &session.built,
        verified,
        production,
        CHECK_POLICY,
    )
    .map_err(|error| match error {
        claim_coordinator::Error::Journal(claim_workflow::Error::WrongIdentity) => {
            OTHER_ACCOUNT.to_string()
        }
        other => describe(other),
    })?;
    session.phase = coordinator.phase();
    session.context = context;
    session.coordinator = Some(coordinator);
    Ok(())
}

/// Copy for a `Production::new` refusal: the backend constraint gets the
/// wording a user can act on; the rest is [`describe`].
fn describe_production(error: claim_coordinator::Error) -> String {
    match error {
        claim_coordinator::Error::Unsupported => BACKEND_UNSUPPORTED.to_string(),
        other => describe(other),
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
        E::Revoked => SESSION_ENDED.to_string(),
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
