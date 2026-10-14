//! Claim step 1 on Bitcoin. New transactions wait for the two-chain
//! ancestry-funded construction; recorded claims remain readable.
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
//! New marker transactions cannot be built, signed or submitted. An
//! unbroadcast recorded intent stays readable but cannot be signed; a
//! recorded submission is recovered only for tracking.

pub mod construction;
use construction::Construction;
pub mod fork_load;
pub mod fork_panel;
pub mod pairing;
mod preferred;
mod signing;

use std::{
    collections::{HashMap, HashSet},
    convert::TryFrom,
    fmt,
    path::PathBuf,
    sync::{Arc, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use iced::Task;
use tokio::sync::watch;

use coincube_core::{
    chain::ChainId,
    claim::{self, Assessment, ForkTransactionPresence, Policy},
    claim_finalize::{finalize_poison_transfer, verify_poison_transaction},
    claim_spend::{
        reconstruct_poison_self_transfer, AncestrySelfTransfer, ClaimForkSweep, PoisonSelfTransfer,
    },
    descriptors::CoincubeDescriptor,
    miniscript::bitcoin::{
        address, bip32::ChildNumber, hashes::Hash, psbt::Psbt, secp256k1, Address, BlockHash,
        Network, OutPoint, Transaction, Txid,
    },
    psbt_unified::UnifiedPsbt,
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
        claim_workflow::{
            self, Context, InitialPairReviewSnapshot, InitialPairSubmissionOutcome, Phase, Status,
        },
        coincube::CoincubeClient,
        feeestimation::fee_estimation::FeeEstimator,
    },
};

/// Legacy deployment margin retained for authenticating recorded claims.
/// New chain-specific input proofs do not depend on the OP_RETURN window.
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
/// Another Connect account signed in from another tab. The claim is held
/// here until this tab signs in.
pub const SIGNED_IN_ELSEWHERE: &str = "Connect signed in with a different account in another tab. This claim is recorded on this device; sign in again in this tab to continue.";
/// A Connect sign-in happened in another tab while this tab had no session
/// of its own. The claim is held here until this tab signs in.
pub const SIGNED_IN_ELSEWHERE_NO_SESSION: &str = "Connect signed in in another tab while this tab had no session. This claim is recorded on this device; sign in in this tab to continue.";
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
/// A signed Claim fork sweep retained legacy signatures that could satisfy it
/// without the unified one, so finalization refused it (`#607`). Wording
/// follows `replay::UNSAFE_LEGACY_POLICY_COPY`. Reopening Claim resumes signing
/// from the unsigned construction (`fork_load::load`), so the next step is real.
pub const UNSAFE_LEGACY_ALTERNATIVE: &str = "The signed claim retains legacy signatures that can independently spend its coins, so it was not finalized and nothing was submitted. Reopen Claim to sign again from the unsigned transaction, collecting only the signatures it needs. Legacy signatures already held by another device or an exported copy can't be revoked.";
/// The journal belongs to another Connect account than the one signed in.
pub const OTHER_ACCOUNT: &str = "This claim was recorded under a different Connect account. Sign in with that account to continue.";
/// A step that reads or writes through the Vault's daemon was asked for
/// while the App has none (a failed backend switch leaves it that way).
pub const NODE_UNAVAILABLE: &str = "The Vault's node isn't available right now, so this step can't run. Check Vault → Settings → Node, then try again.";
/// The claim target is missing or no longer identifies one valid Cube.
pub const TARGET_GONE: &str =
    "The claim target is missing or ambiguous. Check the paired Bitcoin Blake2b Cube on this device, then try again.";
/// `Production::new`'s backend refusal, in the words a user can act on.
const BACKEND_UNSUPPORTED: &str = "This Vault's node configuration doesn't support this claim. Check the wallet network and Vault → Settings → Node, then try again.";

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
    /// The fork block's hash on the fork chain; retained for recorded plans.
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
    /// Candidates only: positive ancestry, ownership and current spend checks
    /// are still required before including any of these in a Claim transfer.
    pub ancestry_candidates: Vec<Coin>,
    pub tip_height: i32,
}

/// What the async preconditions probe found. Each part fails on its own so
/// the panel can say which one, and offer a retry for the transient ones.
#[derive(Debug)]
pub struct Checked {
    pub window: Result<ForkWindow, String>,
    pub coins: Result<CoinSet, String>,
    pub feerate_vb: Result<u64, String>,
    /// Whether the current Connect session and Bitcoin Vault can support the
    /// read-only two-chain funding flow. Submission is admitted separately.
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
    reconfirmation: Option<claim_coordinator::ReconfirmationReview>,
    resubmission: Option<claim_coordinator::ResubmissionReview>,
    /// Kept for a re-bind: `Coordinator::resume` re-validates the
    /// construction against the journal and re-verifies the signatures.
    built: Box<Construction>,
    signed: Psbt,
    recovered: Option<Transaction>,
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

/// Deposit scan hints are displayed separately from the paired ancestry
/// proof. A candidate never enables signing.
#[derive(Debug)]
pub struct ClaimDepositProgress {
    pub bitcoin_candidate: bool,
    pub btcb2_candidate: bool,
    pub both_verified: bool,
    /// Derived from the exact ordinary PSBT pair. It is never signer authority.
    pub review: Option<view::vault::ancestry_coins::VerifiedCoinPairReview>,
    prepared: Option<Box<PreparedClaimPair>>,
}

/// The exact pair and selected ancestry paths from the last deposit review.
/// Its short lifetime is for a future explicit signer transition, not a
/// substitute for fresh wallet, UTXO and two-chain admission at dispatch.
#[derive(Debug, Clone)]
struct PreparedClaimPair {
    bitcoin: AncestrySelfTransfer,
    fork: ClaimForkSweep,
    bitcoin_funding: coincube_core::miniscript::bitcoin::OutPoint,
    fork_funding: coincube_core::miniscript::bitcoin::OutPoint,
    bitcoin_rate_vb: u64,
    fork_rate_vb: u64,
}

impl PreparedClaimPair {
    fn matches_review(&self, review: &view::vault::ancestry_coins::VerifiedCoinPairReview) -> bool {
        self.bitcoin_rate_vb > 0
            && self.fork_rate_vb > 0
            && view::vault::ancestry_coins::CoinPairReview::from_psbts(
                self.bitcoin.psbt(),
                self.fork.psbt(),
                self.bitcoin.claimed_prevouts(),
                self.bitcoin_funding,
                self.fork_funding,
            )
            .is_ok_and(|coins| coins == review.coins)
    }
}

/// Authenticate a device or file return against both fixed constructions and
/// the signatures already collected on this side before the ordinary picker
/// can merge it. Bitcoin's generic PSBT merge is intentionally permissive;
/// Claim must reject a conflicting or altered signer return first.
fn check_paired_signer_return(
    pair: &PreparedClaimPair,
    side: ChainId,
    current: &Psbt,
    returned: &Psbt,
) -> Result<(), String> {
    let signed = |psbt: &Psbt| {
        psbt.inputs
            .iter()
            .any(|input| !input.partial_sigs.is_empty())
    };
    let mut bitcoin_returns = Vec::new();
    let mut fork_returns = Vec::new();
    match side {
        ChainId::Bitcoin => {
            if signed(current) {
                bitcoin_returns.push(current.clone());
            }
            bitcoin_returns.push(returned.clone());
        }
        ChainId::BitcoinBlake2b => {
            if signed(current) {
                fork_returns.push(UnifiedPsbt::from_psbt(current.clone()).map_err(|_| {
                    "The current BTCB2 signing state is invalid. Reopen this Claim.".to_string()
                })?);
            }
            fork_returns.push(
                UnifiedPsbt::from_psbt(returned.clone())
                    .map_err(|_| "The BTCB2 signer returned an invalid PSBT.".to_string())?,
            );
        }
        _ => return Err("The signer returned a transaction for the wrong chain.".into()),
    }
    crate::services::claim_pair_signing::combine_ancestry_claim_pair(
        &pair.bitcoin,
        &bitcoin_returns,
        &pair.fork,
        &fork_returns,
    )
    .map(|_| ())
    .map_err(|error| format!("The signer result did not match the exact Claim pair: {error:?}"))
}

/// A borrowed fork daemon is valid only while the tab that supplied it stays
/// open under the same session. The weak reference does not keep a closed
/// Cube's daemon alive during the Bitcoin-side scan.
#[derive(Clone)]
pub(crate) struct ClaimForkBinding {
    daemon: Weak<dyn Daemon + Sync + Send>,
    wallet: Weak<Wallet>,
    generation: watch::Receiver<u64>,
    expected: u64,
}

impl ClaimForkBinding {
    pub(crate) fn new(
        daemon: &Arc<dyn Daemon + Sync + Send>,
        wallet: &Arc<Wallet>,
        generation: watch::Receiver<u64>,
    ) -> Self {
        let expected = *generation.borrow();
        Self {
            daemon: Arc::downgrade(daemon),
            wallet: Arc::downgrade(wallet),
            generation,
            expected,
        }
    }

    pub(crate) fn current(&self) -> bool {
        matches!(self.generation.has_changed(), Ok(false))
            && *self.generation.borrow() == self.expected
            && self.daemon.strong_count() > 0
            && self.wallet.strong_count() > 0
    }

    fn upgrade(&self) -> Option<Arc<dyn Daemon + Sync + Send>> {
        self.current().then(|| self.daemon.upgrade()).flatten()
    }

    fn wallet(&self) -> Option<Arc<Wallet>> {
        self.current().then(|| self.wallet.upgrade()).flatten()
    }

    fn generation_guard(&self) -> (&watch::Receiver<u64>, u64) {
        (&self.generation, self.expected)
    }
}

pub(crate) fn funding_proof_refusal(
    error: crate::services::claim_observation::http::DiscoveryError,
) -> &'static str {
    use crate::services::claim_observation::{http::DiscoveryError, FailureKind};
    match error {
        DiscoveryError::Observation(FailureKind::AncestryRootShared { .. }) => {
            "A deposit's coinbase ancestry is present on both chains. It cannot prove a Bitcoin-only or BTCB2-only input."
        }
        DiscoveryError::Observation(FailureKind::AncestryRootChanged { .. }) => {
            "A chain reorganized during the ancestry check. Wait for confirmations and check both deposits again."
        }
        DiscoveryError::Observation(FailureKind::Changed) => {
            "A deposit or shared coin changed, was spent, or is not sufficiently confirmed. Check both chains again after confirmation."
        }
        DiscoveryError::Observation(
            FailureKind::Unavailable
            | FailureKind::Http(_)
            | FailureKind::Stale
            | FailureKind::FreshnessUnverified,
        ) => "Fresh data from both chains is unavailable. Retry when both nodes and indexers are current.",
        DiscoveryError::Observation(FailureKind::Deadline | FailureKind::CollectionLimit) => {
            "The bounded two-chain ancestry check could not finish. Retry after the services recover."
        }
        DiscoveryError::Observation(FailureKind::Cancelled) => {
            "The Connect session changed during the ancestry check. Sign in and retry."
        }
        DiscoveryError::Structural(_) => {
            "A deposit's path to a chain-specific post-fork coinbase could not be authenticated."
        }
        _ => "The deposits do not yet have a complete two-chain ancestry proof.",
    }
}

/// Results of the panel's async work. Every variant that carries the
/// session carries it back to the one panel that sent it out.
#[derive(Debug)]
pub enum ClaimEvent {
    FundingPrepared(
        u64,
        u64,
        u64,
        Result<claim_workflow::CheckedClaimFundingAddresses, String>,
    ),
    FundingChecked(u64, u64, u64, Result<ClaimDepositProgress, String>),
    /// A read-only ancestry status is only a short-lived observation.
    FundingProofExpired(u64, u64, u64),
    PairedSigningPrepared(u64, u64, u64, Result<(), String>),
    PairedPsbt(u64, ChainId, Box<Message>),
    PairedPermitChecked(
        u64,
        u64,
        u64,
        ChainId,
        Box<Message>,
        Result<crate::services::claim_workflow::paired_signing_permit::PairedSigningPermit, String>,
    ),
    PairedAuthorizedPsbt(u64, ChainId, Box<Message>),
    PairedSigned(u64, u64, u64, Result<SignedPairDetails, String>),
    PairedBitcoinReviewed(u64, u64, u64, Result<InitialPairReviewSnapshot, String>),
    PairedBitcoinSubmitted(u64, u64, u64, Result<InitialPairSubmissionOutcome, String>),
    PairedForkReviewed(u64, u64, u64, Result<InitialPairReviewSnapshot, String>),
    PairedForkSubmitted(u64, u64, u64, Result<InitialPairSubmissionOutcome, String>),
    Restarted(u64, Result<RestartedClaim, String>),
    /// The preconditions probe finished. The sequence number matches it to
    /// the request that started it; a reply to an older request is dropped.
    Checked(u64, Box<Checked>),
    /// The poison self-transfer was (or could not be) built.
    Built(u64, Result<Box<Construction>, String>),
    SigningChecked(
        u64,
        Box<Construction>,
        Result<signing::CheckedInputs, String>,
    ),
    /// The signed construction was finalised and journaled as an intent —
    /// or refused, in which case the construction comes back so the user can
    /// keep signing. The number names the finalisation attempt it answers.
    Ready(u64, Result<Box<ClaimSession>, (Box<Construction>, String)>),
    /// A revoked session was re-bound under the current context — or could
    /// not be, in which case it comes back unbound with the reason. The
    /// number is the panel's revocation count when the re-bind was sent.
    Rebound(u64, Box<ClaimSession>, Result<(), String>),
    /// `prepare_review` finished; the review lives inside the session. The
    /// number is the panel's revocation count when the review was sent: a
    /// review whose context ended while it ran is not shown as actionable.
    Reviewed(u64, Box<ClaimSession>, Result<ReviewSnapshot, String>),
    /// `confirm_and_submit` finished.
    Submitted(u64, Box<ClaimSession>, Result<Outcome, String>),
    /// `reconcile` finished; the number as for `Reviewed`.
    Tracked(u64, Box<ClaimSession>, Result<Status, String>),
    Reconfirmed(u64, Box<ClaimSession>, Result<(), String>, bool),
    Resubmitted(u64, Box<ClaimSession>, Result<Option<Outcome>, String>),
}

#[derive(Debug)]
pub struct RestartedClaim {
    context: Context,
    state: RestartedState,
}
#[derive(Debug)]
enum RestartedState {
    Intent(Box<Construction>),
    Bound(Box<ClaimSession>),
    Unavailable(Txid, String),
    PairedFunding(claim_workflow::CheckedClaimFundingAddresses),
    PairedRecovery(PairedClaimRecovery),
}

/// Restart status for the two ordinary transactions. A status read cannot
/// authorize signing, retrying or moving to the next chain; an unattempted
/// Bitcoin send requires a separate live proof and explicit review.
#[derive(Debug, Clone)]
pub struct PairedClaimRecovery {
    pub bitcoin_unattempted: bool,
    pub fork_ready_for_review: bool,
    pub bitcoin_txid: Option<Txid>,
    pub fork_txid: Option<Txid>,
    pub bitcoin_output: Option<super::paired_summary::PairedOutputSummary>,
    pub fork_output: Option<super::paired_summary::PairedOutputSummary>,
    pub bitcoin_status: String,
    pub fork_status: String,
}

#[derive(Debug)]
pub struct SignedPairDetails {
    bitcoin_txid: Txid,
    fork_txid: Txid,
    bitcoin_output: Option<super::paired_summary::PairedOutputSummary>,
    fork_output: Option<super::paired_summary::PairedOutputSummary>,
}

fn paired_side_status(report: claim_workflow::PairedSideReport) -> String {
    use claim_workflow::{PairedChainState as State, PairedSideReport as Side};
    match report {
        Side::NotAttempted => "No broadcast attempt recorded.".into(),
        Side::Observed(State::Absent) =>
            "A possible broadcast was recorded, but this chain does not currently show the transaction. It will not be retried automatically.".into(),
        Side::Observed(State::Unconfirmed) => "Transaction seen; awaiting confirmation.".into(),
        Side::Observed(State::Confirmed { confirmations, .. }) =>
            format!("Confirmed in the current chain view ({confirmations} confirmations)."),
        Side::Indeterminate(_) =>
            "The recorded transaction could not be verified against this chain. Check again when its node and indexer are available; no retry was sent.".into(),
    }
}

enum Stage {
    Preconditions,
    FundingChecking,
    FundingDeposits(claim_workflow::CheckedClaimFundingAddresses),
    PairedSign {
        pair: Box<PreparedClaimPair>,
        bitcoin: Box<PsbtState>,
        fork: Box<PsbtState>,
        active: ChainId,
        recording: bool,
        error: Option<String>,
    },
    PairedRecovery(PairedClaimRecovery),
    PairedBitcoinReviewing(PairedClaimRecovery),
    PairedBitcoinReview(PairedClaimRecovery, InitialPairReviewSnapshot),
    PairedBitcoinSubmitting(PairedClaimRecovery),
    PairedForkReviewing(PairedClaimRecovery),
    PairedForkReview(PairedClaimRecovery, InitialPairReviewSnapshot),
    PairedForkSubmitting(PairedClaimRecovery),
    /// Built and shown; nothing signed yet.
    Plan {
        built: Box<Construction>,
    },
    CheckingSign(FinalizeAttempt),
    /// In the Vault's signing flow. `built` is `None` only while the
    /// finalise task holds it, and `finalizing` names that task.
    Sign {
        built: Option<Box<Construction>>,
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
    FundingChecking,
    FundingDeposits(&'a claim_workflow::CheckedClaimFundingAddresses),
    PairedSign {
        bitcoin: &'a PsbtState,
        fork: &'a PsbtState,
        session: u64,
        active: ChainId,
        recording: bool,
        error: Option<&'a str>,
    },
    PairedRecovery(&'a PairedClaimRecovery, bool, Option<&'a str>),
    PairedBitcoinReview(&'a InitialPairReviewSnapshot),
    PairedBitcoinSubmitting,
    PairedForkReview(&'a InitialPairReviewSnapshot),
    PairedForkSubmitting,
    Plan(&'a Construction),
    CheckingSign,
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
    /// A paired, currently open BTCB2 wallet. Weak ownership ensures closing
    /// its tab cannot leave a stopped daemon held alive by the Bitcoin Cube.
    fork_daemon: Option<ClaimForkBinding>,
    pre: Preconditions,
    funding_error: Option<String>,
    funding_status: Option<String>,
    funding_review: Option<view::vault::ancestry_coins::VerifiedCoinPairReview>,
    prepared_pair: Option<Box<PreparedClaimPair>>,
    funding_checking: bool,
    paired_signing_pending: bool,
    paired_sign_seq: u64,
    paired_dispatch_seq: u64,
    paired_broadcast_seq: u64,
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
    building: Option<FinalizeAttempt>,
    feerate: FeerateSource,
    restart_pending: bool,
    handoff_pending: bool,
    restarting: Option<FinalizeAttempt>,
    restart_error: Option<String>,
    resuming: bool,
    #[cfg(test)]
    legacy_flow_fixture: bool,
}

impl ClaimStep1Panel {
    pub fn new(
        wallet: Arc<Wallet>,
        datadir: CoincubeDirectory,
        bitcoin_cube: String,
        generation: watch::Receiver<u64>,
        connect: Option<ConnectSession>,
    ) -> Self {
        let restart_pending = !matches!(std::fs::symlink_metadata(
            journal_directory(&datadir, &wallet).join("intent.json"),
        ), Err(e) if e.kind() == std::io::ErrorKind::NotFound);
        let mut panel = Self {
            wallet,
            datadir,
            bitcoin_cube,
            generation,
            connect,
            fork_daemon: None,
            pre: Preconditions::default(),
            funding_error: None,
            funding_status: None,
            funding_review: None,
            prepared_pair: None,
            funding_checking: false,
            paired_signing_pending: false,
            paired_sign_seq: 0,
            paired_dispatch_seq: 0,
            paired_broadcast_seq: 0,
            stage: Stage::Preconditions,
            revoker: None,
            revoked: false,
            revocations: 0,
            backend: BackendState::Ready,
            check_seq: 0,
            finalize_attempts: 0,
            building: None,
            feerate: FeerateSource::Estimator,
            restart_pending,
            handoff_pending: false,
            restarting: None,
            restart_error: None,
            resuming: false,
            #[cfg(test)]
            legacy_flow_fixture: false,
        };
        panel.refresh_static_preconditions();
        panel
    }

    /// Replace the fee-rate source (tests: a fixed rate, no network).
    pub fn with_feerate_source(mut self, feerate: FeerateSource) -> Self {
        self.feerate = feerate;
        self
    }

    fn legacy_flow_fixture(&self) -> bool {
        #[cfg(test)]
        {
            self.legacy_flow_fixture
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    /// This is the retired, one-sided signer. The paired ancestry signer will
    /// have its own admission path; opening its gate must not enable this one.
    pub fn can_sign_or_submit(&self) -> bool {
        self.legacy_flow_fixture()
    }

    pub fn can_prepare_funding(&self) -> bool {
        matches!(self.stage, Stage::Preconditions)
            && self.pre.target.is_some()
            && self.pre.shape.is_none()
            && !self.restart_pending
            && !self.pre.checking
            && self
                .pre
                .checked
                .as_ref()
                .is_some_and(|checked| checked.backend.is_ok())
            && self.backend_ready()
            && self.connect.is_some()
            && !self.legacy_flow_fixture()
    }

    /// The deposit workflow reads both chains through the current Connect
    /// account, regardless of whether this Vault's daemon is embedded or a
    /// Connect wallet backend. The latter must still report a positive Bitcoin
    /// chain identity before any address is prepared; the old submission
    /// backend admission is deliberately not reused for a read-only scan.
    fn funding_context(
        connect: &ConnectSession,
        expected: u64,
        generation: watch::Receiver<u64>,
    ) -> Result<Context, String> {
        if connect.account.is_empty()
            || generation.has_changed().is_err()
            || *generation.borrow() != expected
        {
            return Err("The Connect claim session is unavailable.".into());
        }
        let source = HttpObservationSource::new(
            connect.client.clone(),
            ChainId::Bitcoin,
            ChainId::BitcoinBlake2b,
            CollectionContext {
                expected_generation: expected,
                generation,
            },
        )
        .map_err(|_| "The Connect two-chain observation source is unavailable.".to_string())?;
        Ok(Context {
            generation: expected,
            account: connect.account.clone(),
            provider: source.provider_identity(),
        })
    }

    pub fn funding_error(&self) -> Option<&str> {
        self.funding_error.as_deref()
    }

    pub fn funding_status(&self) -> Option<&str> {
        if self.funding_review.is_some() && !self.fork_pair_current() {
            return Some("The paired BTCB2 Cube changed. Reopen it and check both deposits again.");
        }
        self.funding_status.as_deref()
    }

    pub fn funding_review(&self) -> Option<&view::vault::ancestry_coins::VerifiedCoinPairReview> {
        let review = self.funding_review.as_ref()?;
        (self.fork_pair_current()
            && self
                .prepared_pair
                .as_ref()
                .is_some_and(|pair| pair.matches_review(review)))
        .then_some(review)
    }

    pub fn paired_signing_pending(&self) -> bool {
        self.paired_signing_pending
    }

    fn fork_pair_current(&self) -> bool {
        self.fork_daemon
            .as_ref()
            .is_some_and(ClaimForkBinding::current)
    }

    pub fn funding_checking(&self) -> bool {
        self.funding_checking
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
            self.fork_daemon = None;
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
        self.fork_daemon = None;
        self.revoked = true;
        self.revocations = self.revocations.wrapping_add(1);
        if matches!(
            self.stage,
            Stage::PairedRecovery(_)
                | Stage::PairedBitcoinReviewing(_)
                | Stage::PairedBitcoinReview(..)
                | Stage::PairedBitcoinSubmitting(_)
                | Stage::PairedForkReviewing(_)
                | Stage::PairedForkReview(..)
                | Stage::PairedForkSubmitting(_)
                | Stage::PairedSign { .. }
        ) {
            self.restart_pending = true;
            self.stage = Stage::Preconditions;
            self.restart_error = Some(SESSION_ENDED.into());
        }
        self.paired_sign_seq = self.paired_sign_seq.wrapping_add(1);
        self.paired_broadcast_seq = self.paired_broadcast_seq.wrapping_add(1);
        self.paired_signing_pending = false;
        if matches!(
            self.stage,
            Stage::FundingChecking | Stage::FundingDeposits(_)
        ) {
            self.stage = Stage::Preconditions;
            self.funding_error = None;
            self.funding_status = None;
            self.funding_review = None;
            self.prepared_pair = None;
            self.funding_checking = false;
        }
        if self.building.take().is_some() {
            self.pre.checking = false;
        }
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
                session.reconfirmation = None;
                session.resubmission = None;
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

    /// Only the GUI's cross-tab pairing path sets this for the next deposit
    /// check. It validates Cube, descriptor and Connect identity first.
    pub(crate) fn bind_fork_daemon(&mut self, binding: Option<ClaimForkBinding>) {
        self.fork_daemon = binding;
    }

    pub(crate) fn funding_pair_target(&self, session: &ConnectSession) -> Option<&str> {
        (matches!(
            self.stage,
            Stage::FundingDeposits(_)
                | Stage::PairedRecovery(_)
                | Stage::PairedBitcoinReviewing(_)
                | Stage::PairedBitcoinReview(..)
                | Stage::PairedForkReviewing(_)
                | Stage::PairedForkReview(..)
        ) && self
            .connect
            .as_ref()
            .is_some_and(|bound| same_session(bound, session)))
        .then_some(self.pre.target.as_deref())
        .flatten()
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
        if self.handoff_pending {
            return Task::none();
        }
        let Some(daemon) = daemon else {
            return Task::none();
        };
        // Not while the App's daemon is being replaced, or is in a state no
        // claim should bind to: the App calls again once it settles.
        if !self.backend_ready() {
            return Task::none();
        }
        if self.restart_pending {
            return self.restart(daemon);
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
            Stage::FundingChecking => StageView::FundingChecking,
            Stage::FundingDeposits(addresses) => StageView::FundingDeposits(addresses),
            Stage::PairedSign {
                bitcoin,
                fork,
                active,
                recording,
                error,
                ..
            } => StageView::PairedSign {
                bitcoin,
                fork,
                session: self.paired_sign_seq,
                active: *active,
                recording: *recording,
                error: error.as_deref(),
            },
            Stage::PairedRecovery(recovery) => StageView::PairedRecovery(
                recovery,
                self.restarting.is_some(),
                self.restart_error.as_deref(),
            ),
            Stage::PairedBitcoinReview(_, review) => StageView::PairedBitcoinReview(review),
            Stage::PairedBitcoinReviewing(recovery) => {
                StageView::PairedRecovery(recovery, true, self.restart_error.as_deref())
            }
            Stage::PairedBitcoinSubmitting(_) => StageView::PairedBitcoinSubmitting,
            Stage::PairedForkReview(_, review) => StageView::PairedForkReview(review),
            Stage::PairedForkReviewing(recovery) => {
                StageView::PairedRecovery(recovery, true, self.restart_error.as_deref())
            }
            Stage::PairedForkSubmitting(_) => StageView::PairedForkSubmitting,
            Stage::Plan { built } => StageView::Plan(built),
            Stage::CheckingSign(_) => StageView::CheckingSign,
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
    /// observation validity, coins, fee rate. `None` when construction may
    /// attempt positive ancestry.
    pub fn refusal(&self) -> Option<Refusal> {
        let refuse = |reason: &str, retry: bool| {
            Some(Refusal {
                reason: reason.to_string(),
                retry,
            })
        };
        if self.pre.target.is_none() {
            return refuse(
                "No unique valid Bitcoin Blake2b Cube on this device matches this Vault. Check the paired Cube before continuing.",
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
        if self.restart_pending {
            return refuse(
                self.restart_error
                    .as_deref()
                    .unwrap_or("A claim is recorded on this device. Read it again to continue."),
                true,
            );
        }
        let Some(checked) = &self.pre.checked else {
            return None;
        };
        if let Err(reason) = &checked.backend {
            return refuse(reason, false);
        }
        if let Err(reason) = &checked.window {
            return refuse(
                &format!("Couldn't read Bitcoin Blake2b's status: {reason}"),
                true,
            );
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
        // Existing records may still be reconciled. The one-sided builder is
        // a test fixture only; paired Claim has separate deposit stages.
        if !self.legacy_flow_fixture() {
            return refuse(
                crate::services::claim_ancestry_gate::NEW_CLAIM_MESSAGE,
                false,
            );
        }
        None
    }

    /// Whether a build may search for a Bitcoin-only input. While the
    /// production ancestry gate is closed it never does.
    pub fn ancestry_discovery_open(&self) -> bool {
        self.connect
            .as_ref()
            .is_some_and(|c| crate::services::claim_ancestry_gate::discovery_open(&c.client))
    }

    /// Whether the Build button may be offered: every precondition holds and
    /// no probe is running.
    pub fn can_build(&self) -> bool {
        matches!(self.stage, Stage::Preconditions)
            && !self.pre.checking
            && !self.restart_pending
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

    pub fn can_continue_on_fork(&self) -> bool {
        self.legacy_flow_fixture()
            && !self.revoked
            && !self.handoff_pending
            && self.backend_ready()
            && self.connect.is_some()
            && matches!(&self.stage, Stage::Track { session: Some(session), busy: false,
                status: Some(Status::Observation(Assessment::ObservationsEligibleForPreflight | Assessment::NeedsPreflightRecheck)), .. }
                if session.coordinator.as_ref().is_some_and(|c| c.wallet_identity().bitcoin_cube == self.bitcoin_cube))
    }
    /// Release the Bitcoin journal owner before the paired Cube opens. The
    /// next stage obtains all authority from fresh checks after normal unlock.
    pub fn take_fork_handoff(&mut self) -> Result<crate::app::claim_intent::ForkHandoff, String> {
        if !self.can_continue_on_fork() {
            return Err(
                "Read both chains again and wait for six Bitcoin confirmations before continuing."
                    .into(),
            );
        }
        let identity = match &self.stage {
            Stage::Track {
                session: Some(session),
                ..
            } => session
                .coordinator
                .as_ref()
                .map(|c| c.wallet_identity().clone()),
            _ => None,
        }
        .ok_or_else(|| SESSION_ENDED.to_string())?;
        let handoff = crate::app::claim_intent::ForkHandoff::new(
            &self.datadir,
            identity.bitcoin_cube,
            identity.fork_cube,
        )?;
        handoff.resolve_target(&self.datadir)?;
        self.revoke();
        if let Stage::Track {
            session, status, ..
        } = &mut self.stage
        {
            *session = None;
            *status = None;
        }
        self.handoff_pending = true;
        self.restart_pending = true;
        Ok(handoff)
    }

    /// The source tab may still be open after its forward handoff. Returning
    /// releases the navigation hold; recovery still reopens the saved journal.
    pub fn return_from_fork(&mut self) {
        self.handoff_pending = false;
    }

    pub fn is_resuming(&self) -> bool {
        self.resuming
    }

    fn restart(&mut self, daemon: Arc<dyn Daemon + Sync + Send>) -> Task<Message> {
        if self.restarting.is_some() || !self.backend_ready() {
            return Task::none();
        }
        self.refresh_static_preconditions();
        let (Some(connect), Some(target)) = (self.connect.clone(), self.pre.target.clone()) else {
            return Task::none();
        };
        let attempt = FinalizeAttempt {
            id: self.finalize_attempts.wrapping_add(1),
            generation: *self.generation.borrow(),
            revocations: self.revocations,
        };
        self.finalize_attempts = attempt.id;
        self.restarting = Some(attempt);
        self.pre.checking = true;
        let wallet = self.wallet.clone();
        let directory = journal_directory(&self.datadir, &wallet);
        let bitcoin_cube = self.bitcoin_cube.clone();
        let generation = self.generation.clone();
        Task::perform(
            async move {
                tokio::time::timeout(
                    CHECK_POLICY.collection_budget,
                    restore_recorded_claim(
                        daemon,
                        wallet,
                        connect,
                        bitcoin_cube,
                        target,
                        directory,
                        attempt.generation,
                        generation,
                    ),
                )
                .await
                .unwrap_or_else(|_| {
                    Err("Reading the recorded claim timed out. Read it again to continue.".into())
                })
            },
            move |result| Message::Claim(ClaimEvent::Restarted(attempt.id, result)),
        )
    }

    fn probe(&mut self, daemon: Arc<dyn Daemon + Sync + Send>) -> Task<Message> {
        if self.restart_pending {
            return self.restart(daemon);
        }
        self.refresh_static_preconditions();
        // A refusal no probe can change: don't spend the network on it. Nor
        // a daemon that is being replaced: `refusal` says so.
        if self.pre.target.is_none() || self.pre.shape.is_some() || !self.backend_ready() {
            return Task::none();
        }
        let Some(connect) = self.connect.clone() else {
            return Task::none();
        };
        // A new inventory invalidates any construction still being collected.
        self.building = None;
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

    fn prepare_funding(&mut self, daemon: Arc<dyn Daemon + Sync + Send>) -> Task<Message> {
        if !self.can_prepare_funding() {
            return Task::none();
        }
        let (Some(connect), Some(fork_cube)) = (self.connect.clone(), self.pre.target.clone())
        else {
            return Task::none();
        };
        self.check_seq = self.check_seq.wrapping_add(1);
        let seq = self.check_seq;
        let expected = *self.generation.borrow();
        let revocations = self.revocations;
        let generation = self.generation.clone();
        let descriptor = self.wallet.main_descriptor.clone();
        let bitcoin_cube = self.bitcoin_cube.clone();
        let directory = journal_directory(&self.datadir, &self.wallet);
        self.funding_error = None;
        self.stage = Stage::FundingChecking;
        Task::perform(
            async move {
                use coincube_core::miniscript::bitcoin::hashes::sha256;
                let context = Self::funding_context(&connect, expected, generation.clone())?;
                let identity = claim_workflow::WalletIdentity {
                    bitcoin_cube,
                    fork_cube,
                    descriptor_digest: sha256::Hash::hash(descriptor.to_string().as_bytes()),
                };
                let info = daemon
                    .get_info()
                    .await
                    .map_err(|_| "The Bitcoin Vault is unavailable.".to_string())?;
                if !bitcoin_funding_wallet_ready(&info, &descriptor) {
                    return Err(
                        "The Bitcoin Vault is not ready to reserve deposit addresses.".into(),
                    );
                }
                let highest = info.receive_index.max(info.change_index);
                let end_exclusive = highest.saturating_add(128).max(256);
                if end_exclusive > 20_000 {
                    return Err(
                        "The Vault's address range is too large for this bounded scan.".into(),
                    );
                }
                let inventory = crate::services::claim_inventory::ClaimInventory::scan(
                    connect.client.clone(),
                    &descriptor,
                    end_exclusive,
                    64,
                    expected,
                    generation.clone(),
                )
                .await
                .map_err(|_| "The two-chain Vault scan did not complete.".to_string())?;
                if inventory.shared().is_empty() {
                    return Err("No shared pre-fork coins were found in this Vault.".into());
                }
                let shared_count = inventory.shared().len();
                let for_reservation = (
                    directory.clone(),
                    descriptor.clone(),
                    identity.clone(),
                    context.clone(),
                );
                tokio::task::spawn_blocking(move || {
                    let (directory, descriptor, identity, context) = for_reservation;
                    claim_workflow::reserve_claim_funding(
                        &directory,
                        &descriptor,
                        &identity,
                        &context,
                        &inventory,
                    )
                })
                .await
                .map_err(|_| "Saving the Claim deposit addresses was interrupted.".to_string())?
                .map_err(|_| "The Claim deposit addresses could not be reserved.".to_string())?;
                claim_workflow::prepare_claim_deposit_instructions(
                    directory,
                    descriptor,
                    identity,
                    context,
                    connect.client,
                    generation,
                    &*daemon,
                    shared_count,
                )
                .await
                .map_err(|error| if error == claim_workflow::ClaimAddressError::FeeUnavailable {
                    "Current Bitcoin and BTCB2 fee quotes are needed before showing deposit amounts.".to_string()
                } else {
                    "The reserved Claim addresses could not be checked on both chains or prepared in this Vault.".to_string()
                })
            },
            move |result| {
                Message::Claim(ClaimEvent::FundingPrepared(
                    seq,
                    expected,
                    revocations,
                    result,
                ))
            },
        )
    }

    /// Reload the exact saved address reservation, then search both chains
    /// for independently proved funding. This is status only: the result is
    /// discarded after rendering and never becomes signer admission.
    fn check_funding(&mut self, daemon: Arc<dyn Daemon + Sync + Send>) -> Task<Message> {
        if !matches!(self.stage, Stage::FundingDeposits(_))
            || self.funding_checking
            || !self.backend_ready()
        {
            return Task::none();
        }
        let Some(connect) = self.connect.clone() else {
            return Task::none();
        };
        self.check_seq = self.check_seq.wrapping_add(1);
        let seq = self.check_seq;
        let expected = *self.generation.borrow();
        let revocations = self.revocations;
        let generation = self.generation.clone();
        let descriptor = self.wallet.main_descriptor.clone();
        let bitcoin_cube = self.bitcoin_cube.clone();
        let fork_cube = self.pre.target.clone();
        let fork_binding = self.fork_daemon.clone();
        let directory = journal_directory(&self.datadir, &self.wallet);
        self.funding_checking = true;
        self.funding_status = None;
        self.funding_review = None;
        self.prepared_pair = None;
        Task::perform(
            async move {
                use coincube_core::miniscript::bitcoin::hashes::sha256;
                let fork_cube =
                    fork_cube.ok_or_else(|| "The paired BTCB2 Cube is missing.".to_string())?;
                let context = Self::funding_context(&connect, expected, generation.clone())?;
                let identity = claim_workflow::WalletIdentity {
                    bitcoin_cube,
                    fork_cube,
                    descriptor_digest: sha256::Hash::hash(descriptor.to_string().as_bytes()),
                };
                let info = daemon
                    .get_info()
                    .await
                    .map_err(|_| "The Bitcoin Vault is unavailable.".to_string())?;
                if !bitcoin_funding_wallet_ready(&info, &descriptor) {
                    return Err(
                        "The Bitcoin Vault is not ready for a two-chain deposit check.".into(),
                    );
                }
                let highest = info.receive_index.max(info.change_index);
                let end_exclusive = highest.saturating_add(128).max(256);
                if end_exclusive > 20_000 {
                    return Err(
                        "The Vault's address range is too large for this bounded scan.".into(),
                    );
                }
                let inventory = crate::services::claim_inventory::ClaimInventory::scan(
                    connect.client.clone(),
                    &descriptor,
                    end_exclusive,
                    64,
                    expected,
                    generation.clone(),
                )
                .await
                .map_err(|_| "The two-chain Vault scan did not complete.".to_string())?;
                let for_load = (
                    directory.clone(),
                    descriptor.clone(),
                    identity.clone(),
                    context.clone(),
                );
                let saved = tokio::task::spawn_blocking(move || {
                    let (directory, descriptor, identity, context) = for_load;
                    claim_workflow::load_claim_funding(&directory, &descriptor, &identity, &context)
                })
                .await
                .map_err(|_| "Loading the saved deposit addresses was interrupted.".to_string())?
                .map_err(|_| {
                    "The saved deposit addresses no longer match this Cube and account.".to_string()
                })?
                .ok_or_else(|| "The saved deposit addresses are missing.".to_string())?;
                let (bitcoin_candidate, btcb2_candidate) = saved
                    .candidate_presence(&inventory, &identity, &context)
                    .map_err(|_| {
                        "The saved deposit addresses no longer match this Vault and account."
                            .to_string()
                    })?;
                let (minimums, rates) = if bitcoin_candidate && btcb2_candidate {
                    claim_workflow::quote_claim_deposit_terms(
                        &descriptor,
                        saved.indices,
                        inventory.shared().len(),
                        connect.client.clone(),
                    )
                    .await
                    .map_err(|_| {
                        "Current Bitcoin and BTCB2 fee quotes are needed to check the deposits."
                            .to_string()
                    })?
                } else {
                    ((0, 0), (0, 0))
                };
                if generation.has_changed().is_err() || *generation.borrow() != expected {
                    return Err("The Connect session changed during the deposit check.".into());
                }
                let (bitcoin_eligible, btcb2_eligible) = saved
                    .fee_eligible_candidate_presence(&inventory, &identity, &context, minimums)
                    .map_err(|_| "The saved deposit reservation changed.".to_string())?;
                if bitcoin_candidate && !bitcoin_eligible {
                    return Err(format!(
                        "No Bitcoin deposit candidate at the reserved address covers the current minimum of {} sats. A separate eligible deposit may be sent to that same address.",
                        minimums.0
                    ));
                }
                if btcb2_candidate && !btcb2_eligible {
                    return Err(format!(
                        "No BTCB2 deposit candidate at the reserved address covers the current minimum of {} sats. A separate eligible deposit may be sent to that same address.",
                        minimums.1
                    ));
                }
                let source = HttpObservationSource::new(
                    connect.client,
                    ChainId::Bitcoin,
                    ChainId::BitcoinBlake2b,
                    CollectionContext {
                        expected_generation: expected,
                        generation: generation.clone(),
                    },
                )
                .map_err(|_| "The two-chain observation source is unavailable.".to_string())?;
                let proof = saved
                    .discover_reserved_funding(
                        directory.clone(),
                        &inventory,
                        &identity,
                        &context,
                        &*daemon,
                        &source,
                        CHECK_POLICY.observations,
                        minimums,
                    )
                    .await
                    .map_err(|error| match error {
                        claim_workflow::ClaimFundingDiscoveryError::Discovery(error) =>
                            funding_proof_refusal(error).to_string(),
                        claim_workflow::ClaimFundingDiscoveryError::WalletState(_) =>
                            "The Bitcoin Vault's coins or sync state changed. Reopen it and check deposits again.".to_string(),
                        claim_workflow::ClaimFundingDiscoveryError::InvalidReservation(_) =>
                            "The saved deposit reservation no longer matches this Vault and Connect session.".to_string(),
                    })?;
                let (review, prepared) = if let Some(proof) = proof {
                    let fork_binding = fork_binding.as_ref().ok_or_else(|| {
                        "Open the paired BTCB2 Cube under this Connect account, then check both deposits again."
                            .to_string()
                    })?;
                    let fork_daemon = fork_binding.upgrade().ok_or_else(|| {
                        "The paired BTCB2 Cube closed or its Connect session changed. Reopen it and check deposits again."
                            .to_string()
                    })?;
                    let fork_wallet = fork_binding.wallet().ok_or_else(|| {
                        "The paired BTCB2 Vault closed or its Connect session changed. Reopen it and check deposits again."
                            .to_string()
                    })?;
                    if fork_wallet.chain != ChainId::BitcoinBlake2b
                        || fork_wallet.main_descriptor != descriptor
                    {
                        return Err("The paired BTCB2 Vault no longer matches this Claim. Reopen both Cubes and check deposits again.".into());
                    }
                    inventory
                        .reconcile_fork_daemon(&*fork_daemon, saved.indices.btcb2_deposit)
                        .await
                        .map_err(|_| {
                            "The paired BTCB2 Vault's complete coin set or sync state changed. Reopen it and check deposits again."
                                .to_string()
                        })?;
                    if generation.has_changed().is_err() || *generation.borrow() != expected {
                        return Err("The Connect session changed during the deposit check.".into());
                    }
                    if !fork_binding.current() {
                        return Err("The paired BTCB2 Cube changed during the deposit check. Reopen it and check deposits again.".into());
                    }
                    let (fork_generation, fork_expected) = fork_binding.generation_guard();
                    let change = claim_workflow::reserve_claim_change_indices(
                        directory,
                        &descriptor,
                        &identity,
                        &context,
                        &*daemon,
                        &*fork_daemon,
                        &generation,
                        fork_generation,
                        fork_expected,
                    )
                    .await
                    .map_err(|error| match error {
                        claim_workflow::ClaimChangeError::Cancelled =>
                            "The Connect session changed before change addresses were reserved.".to_string(),
                        claim_workflow::ClaimChangeError::WalletState =>
                            "The paired Bitcoin or BTCB2 Vault is unavailable or changed. Reopen both Cubes and check deposits again.".to_string(),
                        claim_workflow::ClaimChangeError::Journal(_) =>
                            "The saved Claim reservation changed. Reopen the Claim and check deposits again.".to_string(),
                    })?;
                    if !fork_binding.current() {
                        return Err("The paired BTCB2 Cube changed during the deposit check. Reopen it and check deposits again.".into());
                    }
                    let tips = claim::PreflightTips {
                        bitcoin: proof
                            .inputs
                            .funding()
                            .bitcoin()
                            .ancestry()
                            .pair()
                            .bitcoin()
                            .tip,
                        fork: proof.inputs.funding().fork().ancestry().pair().fork().tip,
                    };
                    let review_context = crate::services::claim_observation::http::FundingContext {
                        provider: &context.provider,
                        generation: context.generation,
                        now: source.now(),
                        max_age_seconds: CHECK_POLICY.observations.max_observation_age_seconds,
                        tips,
                    };
                    let (bitcoin, fork) = inventory
                        .construct_pair_for_review(
                            &proof,
                            ChildNumber::Normal {
                                index: change.bitcoin,
                            },
                            ChildNumber::Normal { index: change.fork },
                            rates.0,
                            rates.1,
                            review_context,
                        )
                        .map_err(|error| {
                            format!("The exact Claim pair could not be reviewed: {error:?}")
                        })?;
                    let coins = view::vault::ancestry_coins::CoinPairReview::from_psbts(
                        bitcoin.psbt(),
                        fork.psbt(),
                        bitcoin.claimed_prevouts(),
                        proof.bitcoin.outpoint,
                        proof.fork.outpoint,
                    )
                    .map_err(str::to_owned)?;
                    // Reserving both change branches and constructing the
                    // PSBTs can race a wallet update. This same paired
                    // snapshot check is reusable at the signer boundary,
                    // where it must follow a fresh proof collection.
                    inventory
                        .reconcile_pair_snapshot(
                            &*daemon,
                            &*fork_daemon,
                            saved.indices.bitcoin_deposit,
                            saved.indices.btcb2_deposit,
                            &proof,
                            &bitcoin,
                            &fork,
                            &source,
                            review_context,
                        )
                        .await
                        .map_err(|error| match error {
                            crate::services::claim_inventory::ClaimPairSnapshotError::WalletState =>
                                "The Bitcoin or paired BTCB2 Vault changed while preparing the Claim preview. Check deposits again."
                                    .to_string(),
                            crate::services::claim_inventory::ClaimPairSnapshotError::Observation(error) =>
                                funding_proof_refusal(crate::services::claim_observation::http::DiscoveryError::Observation(error)).to_string(),
                            crate::services::claim_inventory::ClaimPairSnapshotError::Changed =>
                                "The two-chain Claim proof changed while preparing the preview. Check deposits again."
                                    .to_string(),
                        })?;
                    let review = view::vault::ancestry_coins::VerifiedCoinPairReview::new(
                        coins,
                        proof.inputs.funding().bitcoin().block(),
                        tips.bitcoin,
                        proof.inputs.funding().fork().block(),
                        tips.fork,
                    )
                    .map_err(str::to_owned)?;
                    (
                        Some(review),
                        Some(Box::new(PreparedClaimPair {
                            bitcoin,
                            fork,
                            bitcoin_funding: proof.bitcoin.outpoint,
                            fork_funding: proof.fork.outpoint,
                            bitcoin_rate_vb: rates.0,
                            fork_rate_vb: rates.1,
                        })),
                    )
                } else {
                    (None, None)
                };
                if generation.has_changed().is_err() || *generation.borrow() != expected {
                    return Err("The Connect session changed during the deposit check.".into());
                }
                if review.is_some()
                    && fork_binding
                        .as_ref()
                        .is_some_and(|binding| !binding.current())
                {
                    return Err("The paired BTCB2 Cube changed during the deposit check. Reopen it and check deposits again.".into());
                }
                Ok(ClaimDepositProgress {
                    bitcoin_candidate,
                    btcb2_candidate,
                    both_verified: review.is_some(),
                    review,
                    prepared,
                })
            },
            move |result| {
                Message::Claim(ClaimEvent::FundingChecked(
                    seq,
                    expected,
                    revocations,
                    result,
                ))
            },
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
        let window = window.clone();
        let wallet = self.wallet.clone();
        let Some(connect) = self.connect.clone() else {
            return Task::none();
        };
        let expected = *self.generation.borrow();
        let generation = self.generation.clone();
        let legacy_fixture = self.legacy_flow_fixture();
        self.finalize_attempts = self.finalize_attempts.wrapping_add(1);
        let id = self.finalize_attempts;
        self.building = Some(FinalizeAttempt {
            id,
            generation: expected,
            revocations: self.revocations,
        });
        self.pre.checking = true;
        Task::perform(
            async move {
                #[cfg(test)]
                if legacy_fixture {
                    return build_legacy_for_test(
                        daemon,
                        wallet,
                        coins,
                        feerate_vb,
                        window.fork_hash,
                    )
                    .await;
                }
                let _ = legacy_fixture;
                preferred::build_preferred(
                    daemon, wallet, coins, feerate_vb, window, connect, expected, generation,
                )
                .await
            },
            move |built| Message::Claim(ClaimEvent::Built(id, built)),
        )
    }

    /// Rebuild and verify a saved signed pair for an explicit Bitcoin review.
    /// The displayed snapshot carries no send authority.
    fn review_paired_bitcoin(&mut self, daemon: Arc<dyn Daemon + Send + Sync>) -> Task<Message> {
        let Stage::PairedRecovery(recovery) = &self.stage else {
            return Task::none();
        };
        if !recovery.bitcoin_unattempted
            || crate::services::claim_ancestry_gate::authorization().is_none()
            || !self.backend_ready()
        {
            return Task::none();
        }
        let (Some(connect), Some(fork_binding), Some(fork_cube)) = (
            self.connect.clone(),
            self.fork_daemon.clone(),
            self.pre.target.clone(),
        ) else {
            return Task::done(Message::View(view::Message::ShowError(
                "Open the paired BTCB2 Cube under this Connect account, then review the Bitcoin broadcast again."
                    .into(),
            )));
        };
        let recovery = recovery.clone();
        let directory = journal_directory(&self.datadir, &self.wallet);
        let bitcoin_cube = self.bitcoin_cube.clone();
        let wallet = self.wallet.clone();
        let expected = *self.generation.borrow();
        let generation = self.generation.clone();
        let revocations = self.revocations;
        self.paired_broadcast_seq = self.paired_broadcast_seq.wrapping_add(1);
        let seq = self.paired_broadcast_seq;
        self.restart_error = None;
        self.stage = Stage::PairedBitcoinReviewing(recovery);
        Task::perform(
            async move {
                signing::prepare_recorded_initial_review(
                    directory,
                    bitcoin_cube,
                    fork_cube,
                    wallet,
                    daemon,
                    fork_binding,
                    connect,
                    expected,
                    generation,
                )
                .await
                .and_then(|(_, review)| {
                    let snapshot = review.snapshot().clone();
                    if super::paired_summary::PairedOutputSummary::from_signed(&snapshot.bitcoin)
                        .is_none()
                        || super::paired_summary::PairedOutputSummary::from_signed(&snapshot.fork)
                            .is_none()
                    {
                        return Err(
                            "The signed Claim destinations cannot be displayed for review.".into(),
                        );
                    }
                    Ok(snapshot)
                })
            },
            move |result| {
                Message::Claim(ClaimEvent::PairedBitcoinReviewed(
                    seq,
                    expected,
                    revocations,
                    result,
                ))
            },
        )
    }

    fn confirm_paired_bitcoin(&mut self, daemon: Arc<dyn Daemon + Send + Sync>) -> Task<Message> {
        if crate::services::claim_ancestry_gate::authorization().is_none() || !self.backend_ready()
        {
            return Task::none();
        }
        let Stage::PairedBitcoinReview(recovery, displayed) = &self.stage else {
            return Task::none();
        };
        let (Some(connect), Some(fork_binding), Some(fork_cube)) = (
            self.connect.clone(),
            self.fork_daemon.clone(),
            self.pre.target.clone(),
        ) else {
            return Task::done(Message::View(view::Message::ShowError(
                "Open the paired BTCB2 Cube under this Connect account, then review the Bitcoin broadcast again."
                    .into(),
            )));
        };
        let recovery = recovery.clone();
        let displayed = displayed.clone();
        let directory = journal_directory(&self.datadir, &self.wallet);
        let bitcoin_cube = self.bitcoin_cube.clone();
        let wallet = self.wallet.clone();
        let expected = *self.generation.borrow();
        let generation = self.generation.clone();
        let revocations = self.revocations;
        self.paired_broadcast_seq = self.paired_broadcast_seq.wrapping_add(1);
        let seq = self.paired_broadcast_seq;
        self.stage = Stage::PairedBitcoinSubmitting(recovery);
        Task::perform(
            async move {
                signing::confirm_recorded_first_broadcast(
                    directory,
                    bitcoin_cube,
                    fork_cube,
                    wallet,
                    daemon,
                    fork_binding,
                    connect,
                    expected,
                    generation,
                    displayed,
                )
                .await
            },
            move |result| {
                Message::Claim(ClaimEvent::PairedBitcoinSubmitted(
                    seq,
                    expected,
                    revocations,
                    result,
                ))
            },
        )
    }

    fn review_paired_fork(&mut self, daemon: Arc<dyn Daemon + Send + Sync>) -> Task<Message> {
        let Stage::PairedRecovery(recovery) = &self.stage else {
            return Task::none();
        };
        if !recovery.fork_ready_for_review
            || crate::services::claim_ancestry_gate::authorization().is_none()
            || !self.backend_ready()
        {
            return Task::none();
        }
        let (Some(connect), Some(fork_binding), Some(fork_cube)) = (
            self.connect.clone(),
            self.fork_daemon.clone(),
            self.pre.target.clone(),
        ) else {
            return Task::done(Message::View(view::Message::ShowError(
                "Open the paired BTCB2 Cube under this Connect account, then review the BTCB2 broadcast again."
                    .into(),
            )));
        };
        let recovery = recovery.clone();
        let directory = journal_directory(&self.datadir, &self.wallet);
        let bitcoin_cube = self.bitcoin_cube.clone();
        let wallet = self.wallet.clone();
        let expected = *self.generation.borrow();
        let generation = self.generation.clone();
        let revocations = self.revocations;
        self.paired_broadcast_seq = self.paired_broadcast_seq.wrapping_add(1);
        let seq = self.paired_broadcast_seq;
        self.restart_error = None;
        self.stage = Stage::PairedForkReviewing(recovery);
        Task::perform(
            async move {
                signing::prepare_recorded_post_bitcoin_review(
                    directory,
                    bitcoin_cube,
                    fork_cube,
                    wallet,
                    daemon,
                    fork_binding,
                    connect,
                    expected,
                    generation,
                )
                .await
                .and_then(|(_, review)| {
                    let snapshot = review.snapshot().clone();
                    if super::paired_summary::PairedOutputSummary::from_signed(&snapshot.bitcoin)
                        .is_none()
                        || super::paired_summary::PairedOutputSummary::from_signed(&snapshot.fork)
                            .is_none()
                    {
                        return Err(
                            "The signed Claim destinations cannot be displayed for review.".into(),
                        );
                    }
                    Ok(snapshot)
                })
            },
            move |result| {
                Message::Claim(ClaimEvent::PairedForkReviewed(
                    seq,
                    expected,
                    revocations,
                    result,
                ))
            },
        )
    }

    fn confirm_paired_fork(&mut self, daemon: Arc<dyn Daemon + Send + Sync>) -> Task<Message> {
        if crate::services::claim_ancestry_gate::authorization().is_none() || !self.backend_ready()
        {
            return Task::none();
        }
        let Stage::PairedForkReview(recovery, displayed) = &self.stage else {
            return Task::none();
        };
        let (Some(connect), Some(fork_binding), Some(fork_cube)) = (
            self.connect.clone(),
            self.fork_daemon.clone(),
            self.pre.target.clone(),
        ) else {
            return Task::done(Message::View(view::Message::ShowError(
                "Open the paired BTCB2 Cube under this Connect account, then review the BTCB2 broadcast again."
                    .into(),
            )));
        };
        let recovery = recovery.clone();
        let displayed = displayed.clone();
        let directory = journal_directory(&self.datadir, &self.wallet);
        let bitcoin_cube = self.bitcoin_cube.clone();
        let wallet = self.wallet.clone();
        let expected = *self.generation.borrow();
        let generation = self.generation.clone();
        let revocations = self.revocations;
        self.paired_broadcast_seq = self.paired_broadcast_seq.wrapping_add(1);
        let seq = self.paired_broadcast_seq;
        self.stage = Stage::PairedForkSubmitting(recovery);
        Task::perform(
            async move {
                signing::confirm_recorded_second_broadcast(
                    directory,
                    bitcoin_cube,
                    fork_cube,
                    wallet,
                    daemon,
                    fork_binding,
                    connect,
                    expected,
                    generation,
                    displayed,
                )
                .await
            },
            move |result| {
                Message::Claim(ClaimEvent::PairedForkSubmitted(
                    seq,
                    expected,
                    revocations,
                    result,
                ))
            },
        )
    }

    /// The paired transition rechecks both complete Vaults, two-chain
    /// ancestry, reserved inputs, exact PSBTs and fee quotes before it opens
    /// either ordinary signer. A later device request repeats that check.
    fn request_paired_signing(&mut self, daemon: Arc<dyn Daemon + Send + Sync>) -> Task<Message> {
        if crate::services::claim_ancestry_gate::authorization().is_none()
            || !matches!(self.stage, Stage::FundingDeposits(_))
            || self.paired_signing_pending
            || self.funding_checking
            || self.funding_review().is_none()
            || !self.backend_ready()
        {
            return Task::none();
        }
        let Some(pair) = self.prepared_pair.as_ref().map(|pair| (**pair).clone()) else {
            return Task::none();
        };
        let (Some(connect), Some(fork_binding), Some(fork_cube)) = (
            self.connect.clone(),
            self.fork_daemon.clone(),
            self.pre.target.clone(),
        ) else {
            return Task::done(Message::View(view::Message::ShowError(
                "Open the paired BTCB2 Cube under this Connect account, then check both deposits again.".into(),
            )));
        };
        self.paired_sign_seq = self.paired_sign_seq.wrapping_add(1);
        let seq = self.paired_sign_seq;
        let expected = *self.generation.borrow();
        let revocations = self.revocations;
        let generation = self.generation.clone();
        let directory = journal_directory(&self.datadir, &self.wallet);
        let bitcoin_cube = self.bitcoin_cube.clone();
        let wallet = self.wallet.clone();
        self.paired_signing_pending = true;
        Task::perform(
            async move {
                signing::prepare_exact_pair_intent(
                    pair,
                    directory,
                    bitcoin_cube,
                    fork_cube,
                    wallet,
                    daemon,
                    fork_binding,
                    connect,
                    expected,
                    generation,
                )
                .await
                .map(|_| ())
            },
            move |result| {
                Message::Claim(ClaimEvent::PairedSigningPrepared(
                    seq,
                    expected,
                    revocations,
                    result,
                ))
            },
        )
    }

    fn install_paired_signing(&mut self) -> Result<(), String> {
        let fork_wallet = self
            .fork_daemon
            .as_ref()
            .and_then(ClaimForkBinding::wallet)
            .ok_or_else(|| {
                "The paired BTCB2 Vault closed. Reopen it and check deposits again.".to_string()
            })?;
        if fork_wallet.chain != ChainId::BitcoinBlake2b
            || fork_wallet.main_descriptor != self.wallet.main_descriptor
        {
            return Err("The paired BTCB2 Vault changed. Check both deposits again.".into());
        }
        let pair = self.prepared_pair.take().ok_or_else(|| {
            "The paired transaction review expired. Check both deposits again.".to_string()
        })?;
        let secp = secp256k1::Secp256k1::verification_only();
        let bitcoin = SpendTx::new(
            None,
            pair.bitcoin.psbt().clone(),
            Vec::new(),
            &self.wallet.main_descriptor,
            &secp,
            self.wallet.chain.bitcoin_network(),
        );
        let fork = SpendTx::new(
            None,
            pair.fork.psbt().clone(),
            Vec::new(),
            &fork_wallet.main_descriptor,
            &secp,
            fork_wallet.chain.bitcoin_network(),
        );
        let mut bitcoin = PsbtState::new(self.wallet.clone(), bitcoin, false);
        let mut fork = PsbtState::new(fork_wallet, fork, false);
        if !bitcoin.require_paired_signing_checks() || !fork.require_paired_signing_checks() {
            self.prepared_pair = Some(pair);
            return Err("The two paired signer wallets are unavailable.".into());
        }
        bitcoin.disable_keychain_signing();
        fork.disable_keychain_signing();
        self.stage = Stage::PairedSign {
            pair,
            bitcoin: Box::new(bitcoin),
            fork: Box::new(fork),
            active: ChainId::Bitcoin,
            recording: false,
            error: None,
        };
        Ok(())
    }

    fn request_paired_permit(
        &mut self,
        daemon: Arc<dyn Daemon + Send + Sync>,
        side: ChainId,
        message: Box<Message>,
    ) -> Task<Message> {
        let (pair, current_psbt) = match &self.stage {
            Stage::PairedSign {
                pair,
                bitcoin,
                fork,
                ..
            } => (
                (**pair).clone(),
                match side {
                    ChainId::Bitcoin => bitcoin.tx.psbt.clone(),
                    ChainId::BitcoinBlake2b => fork.tx.psbt.clone(),
                    _ => return Task::none(),
                },
            ),
            _ => return Task::none(),
        };
        let (Some(connect), Some(fork_binding), Some(fork_cube)) = (
            self.connect.clone(),
            self.fork_daemon.clone(),
            self.pre.target.clone(),
        ) else {
            return Task::done(Message::View(view::Message::ShowError(
                "The paired BTCB2 Vault or Connect session is unavailable.".into(),
            )));
        };
        self.paired_dispatch_seq = self.paired_dispatch_seq.wrapping_add(1);
        let seq = self.paired_dispatch_seq;
        let expected = *self.generation.borrow();
        let revocations = self.revocations;
        let generation = self.generation.clone();
        let directory = journal_directory(&self.datadir, &self.wallet);
        let bitcoin_cube = self.bitcoin_cube.clone();
        let wallet = self.wallet.clone();
        Task::perform(
            async move {
                let requalified = signing::prepare_exact_pair_intent(
                    pair.clone(),
                    directory,
                    bitcoin_cube,
                    fork_cube,
                    wallet,
                    daemon,
                    fork_binding,
                    connect,
                    expected,
                    generation.clone(),
                )
                .await?;
                requalified
                    .signer_permit(side, &pair.bitcoin, &pair.fork, &current_psbt, generation)
                    .ok_or_else(|| {
                        "The paired signing check expired or the PSBT changed. Check both deposits again."
                            .to_string()
                    })
            },
            move |result| {
                Message::Claim(ClaimEvent::PairedPermitChecked(
                    seq,
                    expected,
                    revocations,
                    side,
                    message,
                    result,
                ))
            },
        )
    }

    fn update_paired_psbt(
        &mut self,
        bitcoin_daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        cache: &Cache,
        side: ChainId,
        message: Message,
        authorised: bool,
    ) -> Task<Message> {
        let daemon_for_record = bitcoin_daemon.clone();
        let check_complete = matches!(
            message,
            Message::Updated(Ok(()))
                | Message::Export(crate::export::ImportExportMessage::Progress(
                    crate::export::Progress::Psbt(_)
                ))
        );
        if !authorised && super::psbt::claim_signing_dispatch(&message) {
            return match bitcoin_daemon {
                Some(daemon) => self.request_paired_permit(daemon, side, Box::new(message)),
                None => Task::done(Message::View(view::Message::ShowError(
                    NODE_UNAVAILABLE.into(),
                ))),
            };
        }
        let Some(daemon) = (match side {
            ChainId::Bitcoin => bitcoin_daemon,
            ChainId::BitcoinBlake2b => self
                .fork_daemon
                .as_ref()
                .and_then(ClaimForkBinding::upgrade),
            _ => None,
        }) else {
            return Task::done(Message::View(view::Message::ShowError(
                "The paired signer node is unavailable.".into(),
            )));
        };
        let Stage::PairedSign {
            pair,
            bitcoin,
            fork,
            recording,
            error,
            ..
        } = &mut self.stage
        else {
            return Task::none();
        };
        if *recording && super::psbt::claim_signing_dispatch(&message) {
            return Task::none();
        }
        let session = self.paired_sign_seq;
        let state = if side == ChainId::Bitcoin {
            bitcoin
        } else {
            fork
        };
        let signer_return = match &message {
            Message::Signed(_, Ok(returned))
            | Message::Export(crate::export::ImportExportMessage::Progress(
                crate::export::Progress::Psbt(returned),
            )) => Some(returned),
            _ => None,
        };
        if let Some(returned) = signer_return {
            if let Err(reason) = check_paired_signer_return(pair, side, &state.tx.psbt, returned) {
                *error = Some(reason.clone());
                return match message {
                    Message::Signed(fingerprint, _) => state
                        .update(
                            Arc::new(SigningOnlyDaemon(daemon)),
                            cache,
                            Message::Signed(
                                fingerprint,
                                Err(crate::app::error::Error::Unexpected(reason)),
                            ),
                        )
                        .map(move |message| {
                            Message::Claim(ClaimEvent::PairedPsbt(session, side, Box::new(message)))
                        }),
                    _ => Task::done(Message::View(view::Message::ShowError(reason))),
                };
            }
        }
        let signing: Arc<dyn Daemon + Sync + Send> = Arc::new(SigningOnlyDaemon(daemon));
        let task = state.update(signing, cache, message).map({
            move |message| Message::Claim(ClaimEvent::PairedPsbt(session, side, Box::new(message)))
        });
        if check_complete {
            Task::batch([task, self.maybe_record_paired_signed(daemon_for_record)])
        } else {
            task
        }
    }

    /// A complete signature set is still only a candidate. Re-read both
    /// wallets and ancestry, then finalize and atomically journal both exact
    /// witnesses before leaving the signer stage. No network send occurs.
    fn maybe_record_paired_signed(
        &mut self,
        bitcoin_daemon: Option<Arc<dyn Daemon + Sync + Send>>,
    ) -> Task<Message> {
        let Stage::PairedSign {
            pair,
            bitcoin,
            fork,
            recording,
            error,
            ..
        } = &mut self.stage
        else {
            return Task::none();
        };
        if *recording {
            return Task::none();
        }
        let bitcoin_signed = bitcoin.tx.psbt.clone();
        let fork_signed = fork.tx.psbt.clone();
        let complete = UnifiedPsbt::from_psbt(fork_signed.clone())
            .ok()
            .and_then(|fork| {
                crate::services::claim_pair_signing::combine_ancestry_claim_pair(
                    &pair.bitcoin,
                    std::slice::from_ref(&bitcoin_signed),
                    &pair.fork,
                    &[fork],
                )
                .ok()
            })
            .is_some_and(|combined| combined.complete);
        if !complete {
            return Task::none();
        }
        let Some(daemon) = bitcoin_daemon else {
            *error = Some(NODE_UNAVAILABLE.into());
            return Task::none();
        };
        let (Some(connect), Some(fork_binding), Some(fork_cube)) = (
            self.connect.clone(),
            self.fork_daemon.clone(),
            self.pre.target.clone(),
        ) else {
            *error = Some("The paired BTCB2 Vault or Connect session is unavailable.".into());
            return Task::none();
        };
        *recording = true;
        *error = None;
        let pair = (**pair).clone();
        let directory = journal_directory(&self.datadir, &self.wallet);
        let bitcoin_cube = self.bitcoin_cube.clone();
        let wallet = self.wallet.clone();
        let expected = *self.generation.borrow();
        let generation = self.generation.clone();
        let revocations = self.revocations;
        let session = self.paired_sign_seq;
        Task::perform(
            async move {
                signing::record_exact_signed_pair(
                    pair,
                    bitcoin_signed,
                    fork_signed,
                    directory,
                    bitcoin_cube,
                    fork_cube,
                    wallet,
                    daemon,
                    fork_binding,
                    connect,
                    expected,
                    generation,
                )
                .await
            },
            move |result| {
                Message::Claim(ClaimEvent::PairedSigned(
                    session,
                    expected,
                    revocations,
                    result,
                ))
            },
        )
    }

    fn request_signing(&mut self, daemon: Arc<dyn Daemon + Send + Sync>) -> Task<Message> {
        if !self.legacy_flow_fixture() {
            return Task::done(Message::View(view::Message::ShowError(
                crate::services::claim_ancestry_gate::NEW_CLAIM_MESSAGE.into(),
            )));
        }
        let Stage::Plan { built } = &self.stage else {
            return Task::none();
        };
        if let Err(error) = built.verified_return_address(&self.wallet) {
            return Task::done(Message::View(view::Message::ShowError(format!(
                "Cannot sign: the return address could not be verified against this Vault. {error}"
            ))));
        }
        #[cfg(test)]
        if self.legacy_flow_fixture() && built.selected_ancestry_input().is_none() {
            self.start_signing();
            return Task::none();
        }
        if built.selected_ancestry_input().is_none() && !self.legacy_flow_fixture() {
            return Task::done(Message::View(view::Message::ShowError(
                crate::services::claim_ancestry_gate::NEW_CLAIM_MESSAGE.into(),
            )));
        }
        let Some(connect) = self.connect.clone() else {
            return Task::none();
        };
        if !self.backend_ready() {
            return Task::none();
        }
        self.finalize_attempts = self.finalize_attempts.wrapping_add(1);
        let attempt = FinalizeAttempt {
            id: self.finalize_attempts,
            generation: *self.generation.borrow(),
            revocations: self.revocations,
        };
        let Stage::Plan { built } =
            std::mem::replace(&mut self.stage, Stage::CheckingSign(attempt))
        else {
            unreachable!()
        };
        let wallet = self.wallet.clone();
        let generation = self.generation.clone();
        Task::perform(
            async move {
                let result = signing::check(
                    &built,
                    daemon,
                    wallet,
                    connect,
                    attempt.generation,
                    generation,
                )
                .await;
                (built, result)
            },
            move |(built, result)| {
                Message::Claim(ClaimEvent::SigningChecked(attempt.id, built, result))
            },
        )
    }

    #[cfg(test)]
    fn start_signing(&mut self) {
        let Stage::Plan { .. } = &self.stage else {
            return;
        };
        let Stage::Plan { built } = std::mem::replace(&mut self.stage, Stage::Preconditions) else {
            unreachable!("matched above");
        };
        let coins = self
            .coins()
            .map(|c| {
                c.pre_fork
                    .iter()
                    .chain(c.ancestry_candidates.iter())
                    .filter(|coin| {
                        built
                            .psbt()
                            .unsigned_tx
                            .input
                            .iter()
                            .any(|input| input.previous_output == coin.outpoint)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        self.install_signing(built, coins);
    }

    fn install_signing(&mut self, built: Box<Construction>, coins: Vec<Coin>) {
        let secp = secp256k1::Secp256k1::verification_only();
        let tx = SpendTx::new(
            None,
            built.psbt().clone(),
            coins,
            &self.wallet.main_descriptor,
            &secp,
            self.wallet.chain.bitcoin_network(),
        );
        let mut psbt = PsbtState::new(self.wallet.clone(), tx, false);
        psbt.disable_keychain_signing();
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
        let legacy_fixture = self.legacy_flow_fixture();
        if !legacy_fixture {
            return Task::none();
        }
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
        if !legacy_fixture
            && built
                .as_ref()
                .is_some_and(|construction| construction.selected_ancestry_input().is_none())
        {
            *error = Some(crate::services::claim_ancestry_gate::NEW_CLAIM_MESSAGE.into());
            return Task::none();
        }
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
        let resume = self.resuming;
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
                    resume,
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
        let revocations = self.revocations;
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
            move |(session, result)| {
                Message::Claim(ClaimEvent::Reviewed(revocations, session, result))
            },
        )
    }

    fn confirm(&mut self) -> Task<Message> {
        let legacy_fixture = self.legacy_flow_fixture();
        if !legacy_fixture {
            if let Stage::Review { error, .. } = &mut self.stage {
                *error = Some(crate::services::claim_ancestry_gate::NEW_CLAIM_MESSAGE.into());
            }
            return Task::none();
        }
        if let Stage::Review {
            session: Some(session),
            error,
            ..
        } = &mut self.stage
        {
            if session.built.selected_ancestry_input().is_none() && !legacy_fixture {
                *error = Some(crate::services::claim_ancestry_gate::NEW_CLAIM_MESSAGE.into());
                return Task::none();
            }
        }
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
        let revocations = self.revocations;
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
            move |(session, result)| {
                Message::Claim(ClaimEvent::Submitted(revocations, session, result))
            },
        )
    }

    pub fn reconfirmation(&self) -> Option<claim_workflow::Reconfirmation> {
        if self.revoked {
            return None;
        }
        match &self.stage {
            Stage::Track {
                session: Some(session),
                busy: false,
                ..
            } => session
                .reconfirmation
                .as_ref()
                .map(|review| review.inclusion()),
            _ => None,
        }
    }

    pub fn resubmission(&self) -> Option<(ReviewSnapshot, usize)> {
        if self.revoked {
            return None;
        }
        match &self.stage {
            Stage::Track {
                session: Some(session),
                busy: false,
                ..
            } if self.legacy_flow_fixture() => session
                .resubmission
                .as_ref()
                .map(|review| (review.snapshot().clone(), review.previous_attempts())),
            _ => None,
        }
    }

    pub fn can_broadcast_review(&self) -> bool {
        matches!(&self.stage, Stage::Review { session: Some(_), .. }
            if self.legacy_flow_fixture())
    }

    fn resubmit(&mut self, confirm: bool) -> Task<Message> {
        let legacy_fixture = self.legacy_flow_fixture();
        if !legacy_fixture {
            if let Stage::Track { error, .. } = &mut self.stage {
                *error = Some(crate::services::claim_ancestry_gate::NEW_CLAIM_MESSAGE.into());
            }
            return Task::none();
        }
        if let Stage::Track {
            session: Some(session),
            error,
            ..
        } = &mut self.stage
        {
            if session.built.selected_ancestry_input().is_none() && !legacy_fixture {
                *error = Some(crate::services::claim_ancestry_gate::NEW_CLAIM_MESSAGE.into());
                return Task::none();
            }
        }
        if self.revoked
            || self.reconfirmation().is_some()
            || self.connect.is_none()
            || !self.backend_ready()
            || !matches!(
                &self.stage,
                Stage::Track {
                    busy: false,
                    status: Some(Status::Observation(
                        claim::Assessment::Reorged | claim::Assessment::WaitingForConfirmation
                    )),
                    ..
                }
            )
        {
            return Task::none();
        }
        let Some(mut session) = self.take_session() else {
            return Task::none();
        };
        session.reconfirmation = None;
        let revocations = self.revocations;
        Task::perform(
            async move {
                let context = session.context.clone();
                let result = match session.coordinator.as_mut() {
                    None => Err(SESSION_ENDED.to_string()),
                    Some(coordinator) if confirm => match session.resubmission.take() {
                        Some(review) => coordinator
                            .confirm_resubmission(review, &context)
                            .await
                            .map(Some)
                            .map_err(describe),
                        None => Err("Review the original transaction before resending it.".into()),
                    },
                    Some(coordinator) => {
                        session.resubmission = None;
                        match coordinator.prepare_resubmission(&context).await {
                            Ok(review) => {
                                session.resubmission = Some(review);
                                Ok(None)
                            }
                            Err(error) => Err(describe(error)),
                        }
                    }
                };
                (session, result)
            },
            move |(session, result)| {
                Message::Claim(ClaimEvent::Resubmitted(revocations, session, result))
            },
        )
    }

    fn reconfirm(&mut self, confirm: bool) -> Task<Message> {
        if self.revoked
            || self.resubmission().is_some()
            || self.connect.is_none()
            || !self.backend_ready()
            || !matches!(
                &self.stage,
                Stage::Track {
                    status: Some(Status::Observation(claim::Assessment::Reorged)),
                    busy: false,
                    ..
                }
            )
        {
            return Task::none();
        }
        let Some(mut session) = self.take_session() else {
            return Task::none();
        };
        let revocations = self.revocations;
        Task::perform(
            async move {
                let context = session.context.clone();
                let result = match session.coordinator.as_mut() {
                    None => Err(SESSION_ENDED.to_string()),
                    Some(coordinator) if confirm => match session.reconfirmation.take() {
                        Some(review) => coordinator
                            .confirm_reconfirmation(review, &context)
                            .await
                            .map_err(describe),
                        None => Err("Review the new confirmation first.".into()),
                    },
                    Some(coordinator) => {
                        session.reconfirmation = None;
                        session.resubmission = None;
                        match coordinator.prepare_reconfirmation(&context).await {
                            Ok(review) => {
                                session.reconfirmation = Some(review);
                                Ok(())
                            }
                            Err(error) => Err(describe(error)),
                        }
                    }
                };
                (session, result)
            },
            move |(session, result)| {
                Message::Claim(ClaimEvent::Reconfirmed(
                    revocations,
                    session,
                    result,
                    confirm,
                ))
            },
        )
    }

    fn reconcile(&mut self) -> Task<Message> {
        if !matches!(&self.stage, Stage::Track { .. }) {
            return Task::none();
        }
        let Some(mut session) = self.take_session() else {
            return Task::none();
        };
        session.reconfirmation = None;
        session.resubmission = None;
        let revocations = self.revocations;
        Task::perform(
            async move {
                let context = session.context.clone();
                let result = match session.coordinator.as_mut() {
                    Some(coordinator) => coordinator.reconcile(&context).await.map_err(describe),
                    None => Err(SESSION_ENDED.to_string()),
                };
                (session, result)
            },
            move |(session, result)| {
                Message::Claim(ClaimEvent::Tracked(revocations, session, result))
            },
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
            Stage::Plan { .. } | Stage::CheckingSign(_) | Stage::Sign { .. } => {
                if self.resuming {
                    self.restart_pending = true;
                }
                self.stage = Stage::Preconditions;
                // The reserved change index is not reused: the daemon's
                // reservation is durable, so the next build reserves anew.
                self.pre.checked = None;
            }
            Stage::FundingChecking | Stage::FundingDeposits(_) => {
                self.check_seq = self.check_seq.wrapping_add(1);
                self.paired_sign_seq = self.paired_sign_seq.wrapping_add(1);
                self.paired_signing_pending = false;
                self.funding_checking = false;
                self.funding_status = None;
                self.funding_review = None;
                self.prepared_pair = None;
                self.stage = Stage::Preconditions;
            }
            Stage::PairedRecovery(_)
            | Stage::PairedBitcoinReviewing(_)
            | Stage::PairedBitcoinReview(..)
            | Stage::PairedBitcoinSubmitting(_)
            | Stage::PairedForkReviewing(_)
            | Stage::PairedForkReview(..)
            | Stage::PairedForkSubmitting(_) => {
                self.restart_pending = true;
                self.stage = Stage::Preconditions;
            }
            Stage::PairedSign { .. } => {
                self.restart_pending = true;
                self.stage = Stage::Preconditions;
            }
            Stage::Preconditions | Stage::Review { .. } | Stage::Track { .. } => {}
        }
    }

    /// Every completion carries what it needs; none needs the daemon, so a
    /// session travelling inside one is never dropped for want of it.
    fn apply(&mut self, event: ClaimEvent) -> Task<Message> {
        match event {
            ClaimEvent::PairedBitcoinReviewed(seq, generation, revocations, result) => {
                if seq != self.paired_broadcast_seq {
                    return Task::none();
                }
                let Stage::PairedBitcoinReviewing(recovery) = &self.stage else {
                    return Task::none();
                };
                if !self.authorized(generation, revocations) {
                    self.restart_pending = true;
                    self.stage = Stage::Preconditions;
                    return Task::none();
                }
                let recovery = recovery.clone();
                match result {
                    Ok(review) => self.stage = Stage::PairedBitcoinReview(recovery, review),
                    Err(reason) => {
                        self.restart_error = Some(reason);
                        self.stage = Stage::PairedRecovery(recovery);
                    }
                }
                Task::none()
            }
            ClaimEvent::PairedBitcoinSubmitted(seq, generation, revocations, result) => {
                if seq != self.paired_broadcast_seq {
                    return Task::none();
                }
                let Stage::PairedBitcoinSubmitting(recovery) = &self.stage else {
                    return Task::none();
                };
                if !self.authorized(generation, revocations) {
                    self.restart_pending = true;
                    self.stage = Stage::Preconditions;
                    return Task::none();
                }
                let mut recovery = recovery.clone();
                match result {
                    Ok(_) => {
                        recovery.bitcoin_unattempted = false;
                        self.restart_error = Some(
                            "The Bitcoin send was recorded. Check both chains for its current status."
                                .into(),
                        );
                    }
                    Err(reason) => self.restart_error = Some(reason),
                }
                self.stage = Stage::PairedRecovery(recovery);
                Task::none()
            }
            ClaimEvent::PairedForkReviewed(seq, generation, revocations, result) => {
                if seq != self.paired_broadcast_seq {
                    return Task::none();
                }
                let Stage::PairedForkReviewing(recovery) = &self.stage else {
                    return Task::none();
                };
                if !self.authorized(generation, revocations) {
                    self.restart_pending = true;
                    self.stage = Stage::Preconditions;
                    return Task::none();
                }
                let recovery = recovery.clone();
                match result {
                    Ok(review) => self.stage = Stage::PairedForkReview(recovery, review),
                    Err(reason) => {
                        self.restart_error = Some(reason);
                        self.stage = Stage::PairedRecovery(recovery);
                    }
                }
                Task::none()
            }
            ClaimEvent::PairedForkSubmitted(seq, generation, revocations, result) => {
                if seq != self.paired_broadcast_seq {
                    return Task::none();
                }
                let Stage::PairedForkSubmitting(recovery) = &self.stage else {
                    return Task::none();
                };
                if !self.authorized(generation, revocations) {
                    self.restart_pending = true;
                    self.stage = Stage::Preconditions;
                    return Task::none();
                }
                let mut recovery = recovery.clone();
                match result {
                    Ok(_) => {
                        recovery.fork_ready_for_review = false;
                        self.restart_error = Some(
                            "The BTCB2 send was recorded. Check both chains for its current status."
                                .into(),
                        );
                    }
                    Err(reason) => self.restart_error = Some(reason),
                }
                self.stage = Stage::PairedRecovery(recovery);
                Task::none()
            }
            ClaimEvent::PairedSigned(session, generation, revocations, result) => {
                if session != self.paired_sign_seq {
                    return Task::none();
                }
                let authorized = self.authorized(generation, revocations);
                let Stage::PairedSign {
                    recording, error, ..
                } = &mut self.stage
                else {
                    return Task::none();
                };
                *recording = false;
                if !authorized {
                    self.restart_pending = true;
                    self.stage = Stage::Preconditions;
                    return Task::none();
                }
                match result {
                    Ok(details) => {
                        self.stage = Stage::PairedRecovery(PairedClaimRecovery {
                            bitcoin_unattempted: true,
                            fork_ready_for_review: false,
                            bitcoin_txid: Some(details.bitcoin_txid),
                            fork_txid: Some(details.fork_txid),
                            bitcoin_output: details.bitcoin_output,
                            fork_output: details.fork_output,
                            bitcoin_status:
                                "Signed and recorded; no Bitcoin broadcast attempt has been made."
                                    .into(),
                            fork_status:
                                "Signed and recorded; no BTCB2 broadcast attempt has been made."
                                    .into(),
                        });
                        Task::none()
                    }
                    Err(reason) => {
                        *error = Some(reason.clone());
                        Task::done(Message::View(view::Message::ShowError(reason)))
                    }
                }
            }
            ClaimEvent::PairedSigningPrepared(seq, generation, revocations, result) => {
                if seq != self.paired_sign_seq {
                    return Task::none();
                }
                self.paired_signing_pending = false;
                if !matches!(self.stage, Stage::FundingDeposits(_))
                    || !self.authorized(generation, revocations)
                    || self.funding_review().is_none()
                {
                    return Task::none();
                }
                match result.and_then(|()| self.install_paired_signing()) {
                    Ok(()) => Task::none(),
                    Err(error) => {
                        self.funding_error = Some(error.clone());
                        Task::done(Message::View(view::Message::ShowError(error)))
                    }
                }
            }
            ClaimEvent::PairedPermitChecked(
                seq,
                generation,
                revocations,
                side,
                message,
                result,
            ) => {
                if seq != self.paired_dispatch_seq
                    || !self.authorized(generation, revocations)
                    || !self.backend_ready()
                {
                    return Task::none();
                }
                let Stage::PairedSign {
                    bitcoin,
                    fork,
                    error,
                    ..
                } = &mut self.stage
                else {
                    return Task::none();
                };
                match result {
                    Ok(permit) => {
                        let state = if side == ChainId::Bitcoin {
                            bitcoin
                        } else if side == ChainId::BitcoinBlake2b {
                            fork
                        } else {
                            return Task::none();
                        };
                        if !state.set_paired_signing_permit(permit) {
                            *error =
                                Some("The signer PSBT changed during the two-chain check.".into());
                            return Task::none();
                        }
                        *error = None;
                        Task::done(Message::Claim(ClaimEvent::PairedAuthorizedPsbt(
                            self.paired_sign_seq,
                            side,
                            message,
                        )))
                    }
                    Err(reason) => {
                        *error = Some(reason.clone());
                        Task::done(Message::View(view::Message::ShowError(reason)))
                    }
                }
            }
            ClaimEvent::PairedPsbt(..) | ClaimEvent::PairedAuthorizedPsbt(..) => Task::none(),
            ClaimEvent::FundingChecked(seq, generation, revocations, mut result) => {
                if seq != self.check_seq || !matches!(self.stage, Stage::FundingDeposits(_)) {
                    return Task::none();
                }
                self.funding_checking = false;
                if !self.authorized(generation, revocations) || !self.backend_ready() {
                    self.stage = Stage::Preconditions;
                    self.funding_status = None;
                    self.funding_review = None;
                    self.prepared_pair = None;
                    self.funding_error =
                        Some("The Claim session changed during the deposit check.".into());
                    return Task::none();
                }
                let proof_complete = matches!(&result, Ok(progress) if progress.both_verified);
                if proof_complete && !self.fork_pair_current() {
                    self.funding_review = None;
                    self.prepared_pair = None;
                    self.funding_status = Some(
                        "The paired BTCB2 Cube changed during the deposit check. Reopen it and check both deposits again."
                            .into(),
                    );
                    return Task::none();
                }
                self.funding_review = result
                    .as_ref()
                    .ok()
                    .and_then(|progress| progress.review.clone());
                self.prepared_pair = result
                    .as_mut()
                    .ok()
                    .and_then(|progress| progress.prepared.take());
                self.funding_status = Some(match result {
                    Ok(progress) => format!(
                        "Bitcoin deposit: {}. Bitcoin Blake2b deposit: {}. {}",
                        if progress.bitcoin_candidate {
                            "post-fork candidate found"
                        } else {
                            "no confirmed candidate at the reserved address"
                        },
                        if progress.btcb2_candidate {
                            "post-fork candidate found"
                        } else {
                            "no confirmed candidate at the reserved address"
                        },
                        if progress.both_verified {
                            "Both deposits have current two-chain ancestry proofs."
                        } else {
                            "Candidates are not yet spend authority."
                        }
                    ),
                    Err(reason) => reason,
                });
                if proof_complete {
                    Task::perform(
                        async { tokio::time::sleep(std::time::Duration::from_secs(30)).await },
                        move |()| {
                            Message::Claim(ClaimEvent::FundingProofExpired(
                                seq,
                                generation,
                                revocations,
                            ))
                        },
                    )
                } else {
                    Task::none()
                }
            }
            ClaimEvent::FundingProofExpired(seq, generation, revocations) => {
                if seq == self.check_seq
                    && matches!(self.stage, Stage::FundingDeposits(_))
                    && self.authorized(generation, revocations)
                {
                    self.funding_review = None;
                    self.prepared_pair = None;
                    self.funding_status = Some(
                        "The deposit proof has expired. Check both deposits again for current chain state and fees."
                            .into(),
                    );
                }
                Task::none()
            }
            ClaimEvent::FundingPrepared(seq, generation, revocations, result) => {
                if seq != self.check_seq || !matches!(self.stage, Stage::FundingChecking) {
                    return Task::none();
                }
                if !self.authorized(generation, revocations) || !self.backend_ready() {
                    self.stage = Stage::Preconditions;
                    self.funding_error =
                        Some("The Claim session changed during the address check.".into());
                    return Task::none();
                }
                match result {
                    Ok(addresses) => self.stage = Stage::FundingDeposits(addresses),
                    Err(reason) => {
                        self.stage = Stage::Preconditions;
                        self.funding_error = Some(reason);
                    }
                }
                Task::none()
            }
            ClaimEvent::Restarted(id, result) => {
                let Some(attempt) = self.restarting.filter(|a| a.id == id) else {
                    return Task::none();
                };
                self.restarting = None;
                self.pre.checking = false;
                if !self.authorized(attempt.generation, attempt.revocations)
                    || !self.backend_ready()
                {
                    self.restart_error = Some(self.ended_copy());
                    return Task::none();
                }
                let restored = match result {
                    Ok(restored) if restored.context.generation == attempt.generation => restored,
                    Ok(_) => {
                        self.restart_error = Some(SESSION_ENDED.into());
                        return Task::none();
                    }
                    Err(error) => {
                        self.restart_error = Some(error.clone());
                        if let Stage::Track { error: slot, .. } = &mut self.stage {
                            *slot = Some(error);
                        }
                        return Task::none();
                    }
                };
                self.restart_error = None;
                self.resuming = true;
                match restored.state {
                    RestartedState::Intent(built) => {
                        self.restart_pending = false;
                        self.stage = Stage::Plan { built };
                    }
                    RestartedState::Bound(session) => {
                        self.restart_pending = false;
                        self.revoked = false;
                        self.revoker = session.coordinator.as_ref().map(|c| c.revoker());
                        let tx = session.recovered.as_ref().expect("restored witness");
                        let outcome = Outcome::Uncertain {
                            txid: tx.compute_txid(),
                            wtxid: tx.compute_wtxid(),
                        };
                        self.stage = Stage::Track {
                            session: Some(session),
                            outcome,
                            status: None,
                            busy: false,
                            error: None,
                        };
                        return self.reconcile();
                    }
                    RestartedState::Unavailable(txid, error) => {
                        self.stage = Stage::Track {
                            session: None,
                            outcome: Outcome::Recorded { txid },
                            status: None,
                            busy: false,
                            error: Some(error),
                        };
                    }
                    RestartedState::PairedRecovery(recovery) => {
                        self.restart_pending = false;
                        self.stage = Stage::PairedRecovery(recovery);
                    }
                    RestartedState::PairedFunding(addresses) => {
                        self.restart_pending = false;
                        self.funding_review = None;
                        self.prepared_pair = None;
                        self.funding_status = Some(
                            "The recorded Claim pair is unsigned. Check both deposits again before signing."
                                .into(),
                        );
                        self.stage = Stage::FundingDeposits(addresses);
                    }
                }
                Task::none()
            }
            ClaimEvent::Checked(seq, checked) => {
                if seq != self.check_seq {
                    return Task::none();
                }
                self.pre.checking = false;
                self.pre.checked = Some(*checked);
                Task::none()
            }
            ClaimEvent::SigningChecked(id, built, result) => {
                let Stage::CheckingSign(attempt) = self.stage else {
                    return Task::none();
                };
                if attempt.id != id {
                    return Task::none();
                }
                let authorized = self.authorized(attempt.generation, attempt.revocations)
                    && self.backend_ready();
                self.stage = Stage::Plan { built };
                if !authorized {
                    return Task::done(Message::View(view::Message::ShowError(
                        SESSION_ENDED.into(),
                    )));
                }
                match result {
                    Ok(checked) => {
                        let Stage::Plan { built } = &self.stage else {
                            unreachable!()
                        };
                        let coins = match checked.consume(
                            built,
                            &self.wallet,
                            self.connect.as_ref(),
                            *self.generation.borrow(),
                        ) {
                            Ok(coins) => coins,
                            Err(error) => {
                                return Task::done(Message::View(view::Message::ShowError(error)))
                            }
                        };
                        let Stage::Plan { built } =
                            std::mem::replace(&mut self.stage, Stage::Preconditions)
                        else {
                            unreachable!()
                        };
                        self.install_signing(built, coins);
                        Task::none()
                    }
                    Err(error) => Task::done(Message::View(view::Message::ShowError(error))),
                }
            }
            ClaimEvent::Built(id, result) => {
                let Some(attempt) = self.building.filter(|attempt| attempt.id == id) else {
                    return Task::none();
                };
                self.building = None;
                self.pre.checking = false;
                if !self.authorized(attempt.generation, attempt.revocations)
                    || !self.backend_ready()
                {
                    return Task::none();
                }
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
                        if let Some(outcome) = session
                            .coordinator
                            .as_ref()
                            .and_then(|c| c.recorded_outcome())
                        {
                            self.stage = Stage::Track {
                                session: Some(session),
                                outcome,
                                status: None,
                                busy: false,
                                error: (!authorized).then_some(ended),
                            };
                            return if authorized {
                                self.reconcile()
                            } else {
                                Task::none()
                            };
                        }
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
            ClaimEvent::Reviewed(revocations, mut session, result) => {
                // A review whose context ended while it ran (a sign-out, a
                // revocation) is never shown as something to confirm, and
                // never erases what the stage says about that end: the
                // session comes back, the review token inside it goes.
                let authorized = self.authorized(session.context.generation, revocations);
                let ended = self.ended_copy();
                if !authorized {
                    session.review = None;
                    session.reconfirmation = None;
                    session.resubmission = None;
                }
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
                        Ok(fresh) if authorized => {
                            *snapshot = Some(fresh);
                            *error = None;
                        }
                        Ok(_) => {
                            *snapshot = None;
                            if error.is_none() {
                                *error = Some(ended);
                            }
                        }
                        Err(reason) => {
                            *snapshot = None;
                            if authorized || error.is_none() {
                                *error = Some(reason);
                            }
                        }
                    }
                }
                Task::none()
            }
            ClaimEvent::Submitted(revocations, session, result) => {
                let authorized = self.authorized(session.context.generation, revocations);
                // A recorded submission still belongs in Track, even when
                // its context ended while the completion was queued. Keep
                // the current hold instruction and wait for a rebind before
                // asking the revoked coordinator for status.
                let held = if authorized {
                    None
                } else {
                    let current = match &self.stage {
                        Stage::Review { error, .. } | Stage::Track { error, .. } => error.clone(),
                        _ => None,
                    };
                    Some(current.unwrap_or_else(|| self.ended_copy()))
                };
                match result {
                    Ok(outcome) => {
                        self.stage = Stage::Track {
                            session: Some(session),
                            outcome,
                            status: None,
                            busy: false,
                            error: held,
                        };
                        if authorized {
                            return self.reconcile();
                        }
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
                            *error = held.or(Some(reason));
                        }
                    }
                }
                Task::none()
            }
            ClaimEvent::Resubmitted(revocations, mut session, result) => {
                let authorized = self.authorized(session.context.generation, revocations);
                let submitted = matches!(&result, Ok(Some(_)));
                let ended = self.ended_copy();
                if !authorized {
                    session.resubmission = None;
                }
                if let Stage::Track {
                    session: slot,
                    busy,
                    status,
                    error,
                    outcome,
                } = &mut self.stage
                {
                    *slot = Some(session);
                    *busy = false;
                    match result {
                        Ok(Some(sent)) => {
                            *outcome = sent;
                            *status = None;
                            if authorized {
                                *error = None;
                            }
                        }
                        Ok(None) if authorized => *error = None,
                        Err(reason) if authorized => *error = Some(reason),
                        _ => {}
                    }
                    if !authorized && error.is_none() {
                        *error = Some(ended);
                    }
                }
                if submitted && authorized {
                    self.reconcile()
                } else {
                    Task::none()
                }
            }
            ClaimEvent::Reconfirmed(revocations, mut session, result, confirmed) => {
                let authorized = self.authorized(session.context.generation, revocations);
                let success = authorized && confirmed && result.is_ok();
                let ended = self.ended_copy();
                if !authorized {
                    session.reconfirmation = None;
                    session.resubmission = None;
                }
                if let Stage::Track {
                    session: slot,
                    busy,
                    status,
                    error,
                    ..
                } = &mut self.stage
                {
                    *slot = Some(session);
                    *busy = false;
                    if authorized {
                        *error = result.err();
                        if success {
                            *status = Some(Status::Unchecked);
                        }
                    } else if error.is_none() {
                        *error = Some(ended);
                    }
                }
                if success {
                    self.reconcile()
                } else {
                    Task::none()
                }
            }
            ClaimEvent::Tracked(revocations, session, result) => {
                // A status read is information whichever context it came
                // from; what the stage says about a context that ended stays.
                let authorized = self.authorized(session.context.generation, revocations);
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
                            if authorized {
                                *error = None;
                            }
                        }
                        Err(reason) => {
                            if authorized || error.is_none() {
                                *error = Some(reason);
                            }
                        }
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
            Stage::PairedSign { bitcoin, fork, .. } => iced::Subscription::batch([
                bitcoin
                    .subscription()
                    .with(self.paired_sign_seq)
                    .map(|(session, message)| {
                        Message::Claim(ClaimEvent::PairedPsbt(
                            session,
                            ChainId::Bitcoin,
                            Box::new(message),
                        ))
                    }),
                fork.subscription()
                    .with(self.paired_sign_seq)
                    .map(|(session, message)| {
                        Message::Claim(ClaimEvent::PairedPsbt(
                            session,
                            ChainId::BitcoinBlake2b,
                            Box::new(message),
                        ))
                    }),
            ]),
            _ => iced::Subscription::none(),
        }
    }

    fn interrupt(&mut self) {
        if let Stage::Sign { psbt, .. } = &mut self.stage {
            psbt.interrupt();
        } else if let Stage::PairedSign { bitcoin, fork, .. } = &mut self.stage {
            bitcoin.interrupt();
            fork.interrupt();
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
            Message::View(view::Message::ClaimPairedSigner(session, side, inner)) => {
                if session != self.paired_sign_seq {
                    return Task::none();
                }
                self.update_paired_psbt(daemon, cache, side, Message::View(*inner), false)
            }
            Message::View(view::Message::Claim(intent)) => {
                if intent == view::ClaimMessage::Refresh {
                    self.handoff_pending = false;
                }
                match intent {
                    view::ClaimMessage::ReviewPairedBitcoin => match daemon {
                        Some(daemon) => self.review_paired_bitcoin(daemon),
                        None => node_unavailable(),
                    },
                    view::ClaimMessage::ConfirmPairedBitcoin => match daemon {
                        Some(daemon) => self.confirm_paired_bitcoin(daemon),
                        None => node_unavailable(),
                    },
                    view::ClaimMessage::BackPairedBitcoin => {
                        if let Stage::PairedBitcoinReview(recovery, _) = &self.stage {
                            self.stage = Stage::PairedRecovery(recovery.clone());
                        }
                        Task::none()
                    }
                    view::ClaimMessage::ReviewPairedFork => match daemon {
                        Some(daemon) => self.review_paired_fork(daemon),
                        None => node_unavailable(),
                    },
                    view::ClaimMessage::ConfirmPairedFork => match daemon {
                        Some(daemon) => self.confirm_paired_fork(daemon),
                        None => node_unavailable(),
                    },
                    view::ClaimMessage::BackPairedFork => {
                        if let Stage::PairedForkReview(recovery, _) = &self.stage {
                            self.stage = Stage::PairedRecovery(recovery.clone());
                        }
                        Task::none()
                    }
                    view::ClaimMessage::StartFunding => match daemon {
                        Some(daemon) => self.prepare_funding(daemon),
                        None => node_unavailable(),
                    },
                    view::ClaimMessage::CheckFunding => match daemon {
                        Some(daemon) => self.check_funding(daemon),
                        None => node_unavailable(),
                    },
                    view::ClaimMessage::SignPair => match daemon {
                        Some(daemon) => self.request_paired_signing(daemon),
                        None => node_unavailable(),
                    },
                    view::ClaimMessage::ShowPairBitcoin => {
                        if let Stage::PairedSign { active, .. } = &mut self.stage {
                            *active = ChainId::Bitcoin;
                        }
                        Task::none()
                    }
                    view::ClaimMessage::ShowPairBtcb2 => {
                        if let Stage::PairedSign { active, .. } = &mut self.stage {
                            *active = ChainId::BitcoinBlake2b;
                        }
                        Task::none()
                    }
                    view::ClaimMessage::Recheck => match daemon {
                        Some(daemon) => self.probe(daemon),
                        None => node_unavailable(),
                    },
                    view::ClaimMessage::Build => match daemon {
                        Some(daemon) => self.build(daemon),
                        None => node_unavailable(),
                    },
                    view::ClaimMessage::Sign => match daemon {
                        Some(daemon) => self.request_signing(daemon),
                        None => node_unavailable(),
                    },
                    view::ClaimMessage::Confirm => self.confirm(),
                    view::ClaimMessage::ReviewResubmission => self.resubmit(false),
                    view::ClaimMessage::ConfirmResubmission => self.resubmit(true),
                    view::ClaimMessage::ReviewReconfirmation => self.reconfirm(false),
                    view::ClaimMessage::ConfirmReconfirmation => self.reconfirm(true),
                    view::ClaimMessage::Refresh if self.restart_pending => match daemon {
                        Some(daemon) => self.recover(Some(daemon)),
                        None => node_unavailable(),
                    },
                    view::ClaimMessage::Refresh
                        if matches!(self.stage, Stage::PairedRecovery(_)) =>
                    {
                        match daemon {
                            Some(daemon) => self.restart(daemon),
                            None => node_unavailable(),
                        }
                    }
                    view::ClaimMessage::Refresh => match &self.stage {
                        Stage::Review { .. } | Stage::Track { .. } if self.revoked => {
                            match (self.backend.copy(), daemon) {
                                // Held by the App's backend state: say so, bind nothing.
                                (Some(copy), _) => Task::done(Message::View(
                                    view::Message::ShowError(copy.to_string()),
                                )),
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
                }
            }
            Message::Claim(ClaimEvent::PairedPsbt(session, side, message)) => {
                if session != self.paired_sign_seq {
                    return Task::none();
                }
                if matches!(
                    *message,
                    Message::View(
                        view::Message::ShowError(_)
                            | view::Message::ShowToast(_, _)
                            | view::Message::Clipboard(_)
                            | view::Message::CopyWithToast(_, _)
                            | view::Message::OpenUrl(_)
                    )
                ) {
                    return Task::done(*message);
                }
                self.update_paired_psbt(daemon, cache, side, *message, false)
            }
            Message::Claim(ClaimEvent::PairedAuthorizedPsbt(session, side, message)) => {
                if session != self.paired_sign_seq {
                    return Task::none();
                }
                self.update_paired_psbt(daemon, cache, side, *message, true)
            }
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
        self.handoff_pending = false;
        if let Some(wallet) = wallet {
            self.wallet = wallet;
        }
        let Some(daemon) = daemon else {
            return Task::none();
        };
        if self.restart_pending {
            return self.restart(daemon);
        }
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
            Stage::PairedRecovery(_)
            | Stage::PairedBitcoinReviewing(_)
            | Stage::PairedBitcoinReview(..)
            | Stage::PairedBitcoinSubmitting(_)
            | Stage::PairedForkReviewing(_)
            | Stage::PairedForkReview(..)
            | Stage::PairedForkSubmitting(_) => self.restart(daemon),
            Stage::FundingChecking
            | Stage::FundingDeposits(_)
            | Stage::PairedSign { .. }
            | Stage::Plan { .. }
            | Stage::CheckingSign(_)
            | Stage::Review { .. } => Task::none(),
        }
    }
}

/// The same Connect session: account, endpoint and credential.
pub(crate) fn same_session(a: &ConnectSession, b: &ConnectSession) -> bool {
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
        "This Vault’s primary spending path is not supported for Claim step 1.".to_string()
    })
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
        ancestry_candidates: Vec::new(),
        tip_height,
    };
    for coin in coins {
        if coin.spend_info.is_some() || coin.is_immature {
            continue;
        }
        let Some(height) = coin.block_height else {
            continue;
        };
        if height < 0 || height > tip_height {
            continue;
        }
        if (height as u64) < fork_height {
            set.pre_fork.push(coin);
        } else {
            set.post_fork += 1;
            set.ancestry_candidates.push(coin);
        }
    }
    set.pre_fork.sort_by_key(|coin| coin.outpoint);
    set.ancestry_candidates.sort_by_key(|coin| coin.outpoint);
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

/// Funding admission depends on authenticated wallet identity and current
/// state, not on which daemon implementation supplies those reads. In
/// particular a Connect wallet may pass once its API reports `chain_id`.
fn bitcoin_funding_wallet_ready(
    info: &model::GetInfoResult,
    descriptor: &CoincubeDescriptor,
) -> bool {
    info.network == Network::Bitcoin
        && info.chain_id == Some(ChainId::Bitcoin)
        && &info.descriptors.main == descriptor
        && info.block_height > 0
        && info.sync.is_finite()
        && info.sync >= 0.9999
        && info.rescan_progress.is_none()
        && !info.chain_divergence
        && info.refused_reorg_depth.is_none()
        && !info.history_sync.full_scan_in_progress
        && info.history_sync.last_failure.is_none()
}

async fn probe(
    daemon: Arc<dyn Daemon + Sync + Send>,
    connect: ConnectSession,
    wallet: Arc<Wallet>,
    generation: watch::Receiver<u64>,
    feerate: FeerateSource,
) -> Checked {
    let expected = *generation.borrow();
    let funding_context = ClaimStep1Panel::funding_context(&connect, expected, generation.clone());
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
        let info = daemon.get_info().await.map_err(|e| e.to_string())?;
        Ok::<_, String>((coins, info))
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
    let backend = funding_context.and_then(|_| match &coins {
        Ok((_, info)) if bitcoin_funding_wallet_ready(info, &wallet.main_descriptor) => Ok(()),
        _ => Err("The Bitcoin Vault must identify its chain and be fully synchronized before preparing deposits.".into()),
    });
    let coins = match (&window, coins) {
        (Ok(window), Ok((coins, info))) => Ok(partition_coins(
            coins,
            window.fork_height,
            info.block_height,
        )),
        (Err(_), Ok((coins, info))) => Ok(CoinSet {
            pre_fork: Vec::new(),
            post_fork: coins.len(),
            ancestry_candidates: Vec::new(),
            tip_height: info.block_height,
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

// Historical workflow fixtures still construct the retired transaction so
// journal recovery and revocation assertions keep exercising exact bytes.
// This function is absent from production builds.
#[cfg(test)]
async fn build_legacy_for_test(
    daemon: Arc<dyn Daemon + Sync + Send>,
    wallet: Arc<Wallet>,
    coins: CoinSet,
    feerate_vb: u64,
    fork_hash: BlockHash,
) -> Result<Box<Construction>, String> {
    use coincube_core::{
        claim_spend::create_poison_self_transfer, miniscript::bitcoin::absolute::LockTime,
    };
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
    create_poison_self_transfer(
        wallet.chain,
        &wallet.main_descriptor,
        &secp256k1::Secp256k1::verification_only(),
        &mut getter,
        &candidates,
        change_index,
        feerate_vb,
        locktime,
        fork_hash,
    )
    .map(|built| Box::new(Construction::OpReturn(built)))
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
async fn restore_recorded_claim(
    daemon: Arc<dyn Daemon + Sync + Send>,
    wallet: Arc<Wallet>,
    connect: ConnectSession,
    bitcoin_cube: String,
    fork_cube: String,
    directory: PathBuf,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<RestartedClaim, String> {
    use coincube_core::miniscript::bitcoin::hashes::sha256;
    if wallet.chain != ChainId::Bitcoin {
        return Err("The recorded claim does not match this Bitcoin Vault.".into());
    }
    // A version-10 pair has no legacy Coincubed construction or submission
    // path. Reopen its journal under the authenticated two-chain identity and
    // reconcile exact saved witnesses before consulting the old coordinator.
    let context = ClaimStep1Panel::funding_context(&connect, expected, generation.clone())?;
    let identity = claim_workflow::WalletIdentity {
        bitcoin_cube: bitcoin_cube.clone(),
        fork_cube: fork_cube.clone(),
        descriptor_digest: sha256::Hash::hash(wallet.main_descriptor.to_string().as_bytes()),
    };
    let mut controller =
        claim_workflow::Controller::reopen_settling(&directory, &identity, context.clone())
            .await
            .map_err(|error| describe(claim_coordinator::Error::Journal(error)))?;
    if controller.is_paired_ancestry_claim() {
        if controller.recorded_signed_ancestry_pair().is_none() {
            let shared_count = controller.plan().claimed_prevouts.len();
            drop(controller);
            let addresses = claim_workflow::prepare_claim_deposit_instructions(
                directory,
                wallet.main_descriptor.clone(),
                identity,
                context.clone(),
                connect.client,
                generation.clone(),
                &*daemon,
                shared_count,
            )
            .await
            .map_err(|error| format!("The saved Claim deposits could not be checked: {error:?}"))?;
            if generation.has_changed().is_err() || *generation.borrow() != expected {
                return Err(SESSION_ENDED.into());
            }
            return Ok(RestartedClaim {
                context,
                state: RestartedState::PairedFunding(addresses),
            });
        }
        let source = HttpObservationSource::new(
            connect.client.clone(),
            ChainId::Bitcoin,
            ChainId::BitcoinBlake2b,
            CollectionContext {
                expected_generation: expected,
                generation: generation.clone(),
            },
        )
        .map_err(|_| "The two-chain status source is unavailable.".to_string())?;
        let (
            bitcoin_unattempted,
            fork_ready_for_review,
            bitcoin_txid,
            fork_txid,
            bitcoin_output,
            fork_output,
            bitcoin_status,
            fork_status,
        ) = if let Some(record) = controller.recorded_signed_ancestry_pair() {
            let bitcoin_unattempted = record.possible_attempt(ChainId::Bitcoin).is_none();
            let fork_unattempted = record.possible_attempt(ChainId::BitcoinBlake2b).is_none();
            let bitcoin_txid = record.bitcoin_identity().0;
            let fork_txid = record.fork_identity().0;
            let bitcoin_output =
                super::paired_summary::PairedOutputSummary::from_signed(record.bitcoin());
            let fork_output =
                super::paired_summary::PairedOutputSummary::from_signed(record.fork());
            let report = controller
                .reconcile_possible_ancestry_attempts(
                    &context,
                    &source,
                    CHECK_POLICY.observations.max_observation_age_seconds,
                )
                .await
                .map_err(|_| {
                    "The paired transaction status could not be read. No transaction was retried."
                        .to_string()
                })?;
            (
                bitcoin_unattempted,
                !bitcoin_unattempted && fork_unattempted && report.bitcoin_confirmation().is_some(),
                Some(bitcoin_txid),
                Some(fork_txid),
                bitcoin_output,
                fork_output,
                paired_side_status(report.bitcoin),
                paired_side_status(report.fork),
            )
        } else {
            (
                false,
                false,
                None,
                None,
                None,
                None,
                "No signed Bitcoin transaction is recorded.".into(),
                "No signed BTCB2 transaction is recorded.".into(),
            )
        };
        if generation.has_changed().is_err() || *generation.borrow() != expected {
            return Err(SESSION_ENDED.into());
        }
        return Ok(RestartedClaim {
            context,
            state: RestartedState::PairedRecovery(PairedClaimRecovery {
                bitcoin_unattempted,
                fork_ready_for_review,
                bitcoin_txid,
                fork_txid,
                bitcoin_output,
                fork_output,
                bitcoin_status,
                fork_status,
            }),
        });
    }
    drop(controller);
    if daemon
        .config()
        .is_none_or(|c| c.main_descriptor != wallet.main_descriptor)
    {
        return Err("The recorded claim does not match this Bitcoin Vault.".into());
    }
    let production = Production::new(
        connect.client,
        daemon.clone(),
        connect.account,
        expected,
        generation.clone(),
    )
    .map_err(describe_production)?;
    let context = production.context().clone();
    let identity = claim_workflow::WalletIdentity {
        bitcoin_cube: bitcoin_cube.clone(),
        fork_cube: fork_cube.clone(),
        descriptor_digest: sha256::Hash::hash(wallet.main_descriptor.to_string().as_bytes()),
    };
    let mut controller =
        claim_workflow::Controller::reopen_settling(&directory, &identity, context.clone())
            .await
            .map_err(|e| describe(claim_coordinator::Error::Journal(e)))?;
    let plan = controller.plan();
    let change_hint = controller.recorded_bitcoin_change_index();
    let phase = controller.phase();
    let txid = controller.signed_txid();
    let stored_transaction = controller.recorded_bitcoin_transaction().cloned();
    let check = || {
        if *generation.borrow() != expected || generation.has_changed().is_err() {
            Err(SESSION_ENDED.to_string())
        } else {
            Ok(())
        }
    };
    check()?;
    // An unavailable recorded submission must remain a hold even when its inputs
    // have already disappeared from the wallet's unspent set.
    let recovered = if let Some(txid) = txid {
        let transaction = if stored_transaction.is_some() {
            stored_transaction
        } else {
            daemon.list_txs(&[txid]).await.ok().and_then(|result| {
                result
                    .transactions
                    .into_iter()
                    .find(|entry| entry.tx.compute_txid() == txid)
                    .map(|entry| entry.tx)
            })
        };
        check()?;
        let Some(transaction) = transaction else {
            return Ok(RestartedClaim { context, state: RestartedState::Unavailable(txid,
                "The recorded transaction is not available from the Bitcoin service yet. Read again to track it; no submission will be retried.".into()) });
        };
        Some(transaction)
    } else {
        None
    };
    let built = if plan.poison == claim::Poison::InputAncestry {
        construction::restore_ancestry(
            &mut controller,
            &context,
            daemon,
            &wallet,
            expected,
            &generation,
        )
        .await
    } else {
        reconstruct_recorded_claim(daemon, &wallet, &plan, change_hint, expected, &generation).await
    };
    drop(controller);
    check()?;
    let built = match built {
        Ok(built) => built,
        Err(error) => {
            return match txid {
                Some(txid) => Ok(RestartedClaim {
                    context,
                    state: RestartedState::Unavailable(txid, error),
                }),
                None => Err(error),
            }
        }
    };
    if phase == Phase::Intent {
        return Ok(RestartedClaim {
            context,
            state: RestartedState::Intent(built),
        });
    }
    let transaction =
        recovered.ok_or_else(|| "The journal is missing its recorded transaction.".to_string())?;
    let verified = built.verify(built.psbt(), Some(&transaction))?;
    let coordinator = built.open(
        &directory,
        bitcoin_cube,
        fork_cube,
        verified,
        production,
        true,
    )?;
    let signed = built.psbt().clone();
    Ok(RestartedClaim {
        context: context.clone(),
        state: RestartedState::Bound(Box::new(ClaimSession {
            phase: coordinator.phase(),
            coordinator: Some(coordinator),
            context,
            review: None,
            reconfirmation: None,
            resubmission: None,
            built,
            signed,
            recovered: Some(transaction),
        })),
    })
}

async fn reconstruct_recorded_claim(
    daemon: Arc<dyn Daemon + Sync + Send>,
    wallet: &Wallet,
    plan: &claim::ClaimPlan,
    change_hint: Option<ChildNumber>,
    expected: u64,
    generation: &watch::Receiver<u64>,
) -> Result<Box<Construction>, String> {
    let coins = daemon
        .list_coins(&[], &plan.claimed_prevouts)
        .await
        .map_err(|e| e.to_string())?
        .coins;
    if coins.len() != plan.claimed_prevouts.len() {
        return Err("Some recorded inputs are missing from this Vault.".into());
    }
    let by_outpoint: HashMap<_, _> = coins
        .into_iter()
        .map(|coin| (coin.outpoint, coin))
        .collect();
    let candidates = plan
        .step1
        .input
        .iter()
        .map(|input| {
            let coin = by_outpoint
                .get(&input.previous_output)
                .ok_or_else(|| "A recorded input is not owned by this Vault.".to_string())?;
            Ok(CandidateCoin {
                outpoint: coin.outpoint,
                amount: coin.amount,
                deriv_index: coin.derivation_index,
                is_change: coin.is_change,
                must_select: true,
                sequence: None,
                ancestor_info: None,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let txids: Vec<_> = plan
        .claimed_prevouts
        .iter()
        .map(|p| p.txid)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let previous = daemon.list_txs(&txids).await.map_err(|e| e.to_string())?;
    let mut getter = TxMap(
        previous
            .transactions
            .into_iter()
            .map(|entry| (entry.tx.compute_txid(), entry.tx))
            .collect(),
    );
    let output = plan
        .step1
        .output
        .get(1)
        .ok_or_else(|| "The recorded claim has no change output.".to_string())?;
    let mut cursor = None;
    let change = if let Some(index) = change_hint {
        // The journal index is only a hint. The core builder below derives the
        // owned script and verifies the entire recorded transaction and inputs.
        index
    } else {
        loop {
            if *generation.borrow() != expected || generation.has_changed().is_err() {
                return Err(SESSION_ENDED.into());
            }
            let page = daemon
                .list_revealed_addresses(true, false, 100, cursor)
                .await
                .map_err(|e| e.to_string())?;
            if let Some(entry) = page
                .addresses
                .iter()
                .find(|entry| entry.address.script_pubkey() == output.script_pubkey)
            {
                break entry.index;
            }
            match page.continue_from {
                Some(next) if cursor.is_none_or(|old| next < old) && !next.is_hardened() => {
                    cursor = Some(next)
                }
                _ => {
                    return Err(
                        "The recorded change address is not a revealed address of this Vault."
                            .into(),
                    )
                }
            }
        }
    };
    if *generation.borrow() != expected || generation.has_changed().is_err() {
        return Err(SESSION_ENDED.into());
    }
    reconstruct_poison_self_transfer(
        wallet.chain,
        &wallet.main_descriptor,
        &secp256k1::Secp256k1::verification_only(),
        &mut getter,
        &candidates,
        change,
        &plan.step1,
    )
    .map(|built| Box::new(Construction::OpReturn(built)))
    .map_err(|e| e.to_string())
}

/// Finalise and journal under the context captured at dispatch (`expected`).
/// If the generation moved in between — the App signed out — the attempt is
/// refused here, before the journal directory is touched, and the
/// construction goes back with every signature; `Coordinator::open` would
/// refuse the same binding, this just says why first.
#[allow(clippy::too_many_arguments)]
async fn finalize_and_journal(
    built: Box<Construction>,
    signed: Psbt,
    daemon: Arc<dyn Daemon + Sync + Send>,
    connect: ConnectSession,
    bitcoin_cube: String,
    fork_cube: String,
    directory: PathBuf,
    expected: u64,
    generation: watch::Receiver<u64>,
    resume: bool,
) -> Result<Box<ClaimSession>, (Box<Construction>, String)> {
    if *generation.borrow() != expected || generation.has_changed().is_err() {
        return Err((built, SIGNED_OUT_BEFORE_RECORD.to_string()));
    }
    let verified = match built.verify(&signed, None) {
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
    match built.open(
        &directory,
        bitcoin_cube,
        fork_cube,
        verified,
        production,
        resume,
    ) {
        Ok(coordinator) => Ok(Box::new(ClaimSession {
            phase: coordinator.phase(),
            coordinator: Some(coordinator),
            context,
            review: None,
            reconfirmation: None,
            resubmission: None,
            built,
            signed,
            recovered: None,
        })),
        Err(error) => Err((built, error)),
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
    session.reconfirmation = None;
    session.resubmission = None;
    let production = Production::new(
        connect.client,
        daemon,
        connect.account,
        expected,
        generation,
    )
    .map_err(describe_production)?;
    let context = production.context().clone();
    let verified = session
        .built
        .verify(&session.signed, session.recovered.as_ref())?;
    let coordinator = session.built.open(
        &directory,
        bitcoin_cube,
        fork_cube,
        verified,
        production,
        true,
    )?;
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
    claim_workflow::prepare_directory(directory).map_err(|error| {
        std::io::Error::other(format!("Private Claim journal unavailable: {error:?}"))
    })
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
        E::UnsafeLegacyAlternative => UNSAFE_LEGACY_ALTERNATIVE.to_string(),
        E::Revoked => SESSION_ENDED.to_string(),
        E::InvalidReview | E::ChangedReview => {
            "What you reviewed has changed since. Review the transaction again.".to_string()
        }
        E::Journal(claim_workflow::Error::WrongIdentity) => OTHER_ACCOUNT.into(),
        E::Journal(claim_workflow::Error::Conflict) => {
            "A claim is already recorded on this device. Reopen Claim to continue it.".into()
        }
        E::Journal(error) => format!("Couldn't record the claim on this device ({error:?})."),
        E::Observation(failure) => format!(
            "Couldn't observe the chains ({:?} while reading {:?}).",
            failure.kind, failure.stage
        ),
        E::Preflight(crate::services::claim_preflight::Error::BothRoutesRejected {
            local,
            connect,
        }) => {
            format!("Your Bitcoin node rejected the transaction: {local}. Connect also rejected it: {connect}. This check did not broadcast anything.")
        }
        E::Preflight(crate::services::claim_preflight::Error::BackendChanged) => {
            "This Vault's backend changed or stopped. Check its connection, then reopen Claim."
                .into()
        }
        E::Preflight(error) => format!("The transaction preflight check failed ({error:?})."),
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
        E::CompletionPersistence(error) => {
            log::warn!("Unable to update Claim completion record: {error}");
            "Couldn't update the saved Claim status on this device. Check confirmation again."
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
