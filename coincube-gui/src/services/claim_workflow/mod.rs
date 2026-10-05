//! Restart-safe intent bookkeeping only. No signing/broadcast/UI entry point.
mod ancestry;
pub(crate) use ancestry::RecoveryObservation;
mod journal;
mod recovery;
mod reorg;
mod split;
use super::claim_observation::{CollectedAssessment, Failure, ObservationBundle};
use coincube_core::{
    chain::ChainId,
    claim::{self, Assessment, BlockRef, ClaimPlan, Poison, Policy, TransactionLocation},
    miniscript::bitcoin::{
        consensus,
        hashes::{sha256, Hash},
        Transaction, Txid, Wtxid,
    },
};
use journal::Journal;
pub use recovery::BitcoinSubmissionAttempt;
pub use reorg::Reconfirmation;
pub(crate) use split::Step2ReturnHold;
pub use split::{
    split_identity, RecordedSplit, SplitKind, Step1Conflict, MAX_SPLIT_STEP2_RESUBMISSIONS,
    SPLIT_TOMBSTONE,
};

/// Upper bound on how long [`Controller::reopen_settling`] waits out `Busy`.
pub const REOPEN_BUSY_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);
/// Interval between [`Controller::reopen_settling`] attempts.
pub const REOPEN_BUSY_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// Establish platform-specific journal privacy before constructing a controller.
pub fn prepare_directory(directory: &std::path::Path) -> Result<(), Error> {
    journal::prepare_directory(directory)
}
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Busy,
    Conflict,
    InvalidJournal,
    InvalidPlan,
    WrongIdentity,
    Revoked,
    LateObservation,
    Unchecked,
    UnsupportedPlatform,
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// There is deliberately no value or constructor for step-two authorization.
/// Observation eligibility, a saved phase and a txid cannot create one.
pub enum Step2Authorization {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalletIdentity {
    pub bitcoin_cube: String,
    pub fork_cube: String,
    pub descriptor_digest: sha256::Hash,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub generation: u64,
    /// Opaque non-secret account and exact provider identity, never a JWT.
    pub account: String,
    pub provider: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Intent,
    BroadcastUncertain,
    Tracking,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ancestry: Option<ancestry::StoredAncestry>,
    identity: WalletIdentity,
    plan: ClaimPlan,
    unsigned_digest: sha256::Hash,
    context_digest: sha256::Hash,
    signed_txid: Option<Txid>,
    phase: Phase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fork_sweep: Option<Transaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fork_change_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bitcoin_change_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fork_submission: Option<RecordedForkSubmission>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    inclusion_history: Vec<Reconfirmation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bitcoin_transaction: Option<Transaction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bitcoin_attempts: Vec<BitcoinSubmissionAttempt>,
    /// Split (#568) only, and only in a version-8 (two-step) or version-9
    /// (fork-only) intent. Every Claim intent leaves it absent, so Claim
    /// journals serialize exactly as before, and binaries without it refuse
    /// a Split journal (`deny_unknown_fields`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    split: Option<split::SplitRecord>,
}
/// A possible submission, not evidence of acceptance or confirmation. Reading
/// this journal record never permits a retry, even after an app restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedForkSubmission {
    txid: Txid,
    wtxid: Wtxid,
}
impl RecordedForkSubmission {
    pub fn txid(&self) -> Txid {
        self.txid
    }
    pub fn wtxid(&self) -> Wtxid {
        self.wtxid
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Unchecked,
    Unavailable,
    Observation(Assessment),
}
/// One-use, controller-specific request identity; fields cannot be forged by UI.
pub struct Ticket {
    controller: u64,
    revision: u64,
    context: Context,
    digest: sha256::Hash,
}

static NEXT_CONTROLLER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn controller_id() -> Result<u64, Error> {
    NEXT_CONTROLLER
        .fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |id| id.checked_add(1),
        )
        .map_err(|_| Error::Revoked)
}

pub struct Controller {
    id: u64,
    revoked: bool,
    construction_verified: bool,
    journal: Journal,
    intent: Intent,
    context: Context,
    revision: u64,
    pending: bool,
    status: Status,
    fresh: Option<FreshObservation>,
}
/// An ancestry check retains its live, non-serializable proof alongside the
/// exact observations. Journal recovery never reconstructs this authority.
struct FreshObservation {
    observations: ObservationBundle,
    ancestry: Option<crate::services::claim_observation::http::CollectedAncestry>,
}
fn digest(tx: &Transaction) -> sha256::Hash {
    sha256::Hash::hash(&consensus::serialize(tx))
}
fn context_digest(context: &Context) -> sha256::Hash {
    let mut bytes = (context.account.len() as u64).to_be_bytes().to_vec();
    bytes.extend_from_slice(context.account.as_bytes());
    bytes.extend_from_slice(&(context.provider.len() as u64).to_be_bytes());
    bytes.extend_from_slice(context.provider.as_bytes());
    sha256::Hash::hash(&bytes)
}
fn validate(intent: &Intent) -> Result<(), Error> {
    // A Split intent has its own rules (no Bitcoin Cube, signed scriptSigs,
    // a tracked signed txid). Every Claim check below stays as it was.
    if intent.split.is_some() || split::VERSIONS.contains(&intent.version) {
        return split::validate(intent);
    }
    let p = &intent.plan;
    if !matches!(
        (
            intent.version,
            intent.fork_sweep.is_some(),
            intent.fork_change_index,
            intent.bitcoin_change_index,
        ),
        (1 | 5 | 6, false, None, None)
            | (2 | 5 | 6, true, None, None)
            | (3 | 5 | 6, true, Some(0..=0x7fff_ffff), None)
            | (4..=7, false, None, Some(0..=0x7fff_ffff))
            | (4..=7, true, Some(0..=0x7fff_ffff), Some(0..=0x7fff_ffff))
    ) || intent.identity.bitcoin_cube.is_empty()
        || intent.identity.fork_cube.is_empty()
        || intent.identity.bitcoin_cube.len() > 256
        || intent.identity.fork_cube.len() > 256
        || intent.identity.bitcoin_cube == intent.identity.fork_cube
        || !matches!(
            (p.bitcoin_chain, p.fork_chain),
            (ChainId::Bitcoin, ChainId::BitcoinBlake2b)
                | (ChainId::Testnet4, ChainId::BitcoinBlake2bTestnet4)
        )
        || p.step1.input.is_empty()
        || p.step1.input.iter().any(|i| {
            !i.script_sig.is_empty() || !i.witness.is_empty() || i.previous_output.is_null()
        })
        || p.step1
            .input
            .iter()
            .map(|i| i.previous_output)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != p.step1.input.len()
        || p.claimed_prevouts
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != p.claimed_prevouts.len()
        || intent.unsigned_digest != digest(&p.step1)
        || intent
            .signed_txid
            .is_some_and(|id| id != p.step1.compute_txid())
        || (intent.phase == Phase::Intent) != intent.signed_txid.is_none()
        || p.tracked_txid.is_some()
    {
        return Err(Error::InvalidPlan);
    }
    ancestry::validate_poison(intent)?;
    if let Some(sweep) = &intent.fork_sweep {
        let inputs: std::collections::BTreeSet<_> =
            sweep.input.iter().map(|i| i.previous_output).collect();
        let claimed: std::collections::BTreeSet<_> = p.claimed_prevouts.iter().copied().collect();
        if intent.phase != Phase::Tracking
            || intent.signed_txid.is_none()
            || inputs.len() != sweep.input.len()
            || inputs != claimed
            || sweep
                .input
                .iter()
                .any(|i| !i.script_sig.is_empty() || !i.witness.is_empty())
            || sweep.output.len() != 1
            || !sweep.output[0].script_pubkey.is_p2wsh()
            || sweep.output[0].value == coincube_core::miniscript::bitcoin::Amount::ZERO
        {
            return Err(Error::InvalidPlan);
        }
    }
    if let Some(submission) = intent.fork_submission {
        if intent
            .fork_sweep
            .as_ref()
            .is_none_or(|sweep| sweep.compute_txid() != submission.txid)
        {
            return Err(Error::InvalidPlan);
        }
    }
    reorg::validate_history(intent)?;
    recovery::validate_record(intent)?;
    Ok(())
}
impl Controller {
    /// Initial admission requires the core builder's opaque owned native-P2WSH
    /// artifact. It cannot be deserialized from a journal or caller-owned boolean.
    pub fn create(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        artifact: &coincube_core::claim_spend::PoisonSelfTransfer,
        context: Context,
    ) -> Result<Self, Error> {
        let bitcoin_chain = artifact.chain();
        let fork_chain = match bitcoin_chain {
            ChainId::Bitcoin => ChainId::BitcoinBlake2b,
            ChainId::Testnet4 => ChainId::BitcoinBlake2bTestnet4,
            _ => return Err(Error::InvalidPlan),
        };
        let identity = WalletIdentity {
            bitcoin_cube,
            fork_cube,
            descriptor_digest: sha256::Hash::hash(artifact.descriptor().to_string().as_bytes()),
        };
        let step1 = artifact.psbt().unsigned_tx.clone();
        let claimed_prevouts = step1.input.iter().map(|i| i.previous_output).collect();
        Self::create_intent(
            directory,
            identity,
            ClaimPlan {
                bitcoin_chain,
                fork_chain,
                step1,
                claimed_prevouts,
                poison: Poison::OpReturn,
                previous_confirmation: None,
                tracked_txid: None,
            },
            context,
            Some(u32::from(artifact.change_index())),
        )
    }
    /// Restart never restores the builder artifact. A caller must reconstruct it
    /// through ownership/prevout checks before recording any new broadcast intent.
    pub fn revalidate_construction(
        &mut self,
        current: &Context,
        artifact: &coincube_core::claim_spend::PoisonSelfTransfer,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        if self.intent.split.is_some() {
            self.construction_verified = false;
            return Err(Error::WrongIdentity);
        }
        if artifact.chain() != self.intent.plan.bitcoin_chain
            || digest(&artifact.psbt().unsigned_tx) != self.intent.unsigned_digest
            || sha256::Hash::hash(artifact.descriptor().to_string().as_bytes())
                != self.intent.identity.descriptor_digest
            || self
                .intent
                .bitcoin_change_index
                .is_some_and(|index| index != u32::from(artifact.change_index()))
        {
            self.construction_verified = false;
            return Err(Error::WrongIdentity);
        }
        // Older records gain the hint only after the owned builder has
        // reproduced the exact transaction. A v2 fork plan must first acquire
        // its fork index through prepare_fork_sweep before upgrading to v4.
        if self.intent.bitcoin_change_index.is_none()
            && (self.intent.fork_sweep.is_none() || self.intent.fork_change_index.is_some())
        {
            let mut next = self.intent.clone();
            next.version = next.version.max(4);
            next.bitcoin_change_index = Some(u32::from(artifact.change_index()));
            validate(&next)?;
            self.journal.store(&next)?;
            self.intent = next;
        }
        self.construction_verified = true;
        Ok(())
    }
    /// Untrusted derivation hint only. Reconstruct the complete owned Bitcoin
    /// transaction and revalidate it before using the journal for any action.
    pub fn recorded_bitcoin_change_index(
        &self,
    ) -> Option<coincube_core::miniscript::bitcoin::bip32::ChildNumber> {
        self.intent.bitcoin_change_index.and_then(|index| {
            coincube_core::miniscript::bitcoin::bip32::ChildNumber::from_normal_idx(index).ok()
        })
    }
    pub fn identity(&self) -> &WalletIdentity {
        &self.intent.identity
    }

    fn create_intent(
        directory: &Path,
        identity: WalletIdentity,
        plan: ClaimPlan,
        context: Context,
        bitcoin_change_index: Option<u32>,
    ) -> Result<Self, Error> {
        Self::create_intent_with_ancestry(
            directory,
            identity,
            plan,
            context,
            bitcoin_change_index,
            None,
        )
    }
    fn create_intent_with_ancestry(
        directory: &Path,
        identity: WalletIdentity,
        plan: ClaimPlan,
        context: Context,
        bitcoin_change_index: Option<u32>,
        ancestry: Option<ancestry::StoredAncestry>,
    ) -> Result<Self, Error> {
        let intent = Intent {
            version: if ancestry.is_some() {
                7
            } else if bitcoin_change_index.is_some() {
                4
            } else {
                1
            },
            ancestry,
            identity,
            unsigned_digest: digest(&plan.step1),
            context_digest: context_digest(&context),
            plan,
            signed_txid: None,
            phase: Phase::Intent,
            fork_sweep: None,
            fork_change_index: None,
            bitcoin_change_index,
            fork_submission: None,
            inclusion_history: Vec::new(),
            bitcoin_transaction: None,
            bitcoin_attempts: Vec::new(),
            split: None,
        };
        validate(&intent)?;
        Self::valid_context(&context)?;
        let mut journal = Journal::open(directory)?;
        if journal.load()?.is_some() {
            return Err(Error::Conflict);
        }
        journal.store(&intent)?;
        Ok(Self {
            id: controller_id()?,
            revoked: false,
            construction_verified: true,
            journal,
            intent,
            context,
            revision: 0,
            pending: false,
            status: Status::Unchecked,
            fresh: None,
        })
    }
    pub fn reopen(
        directory: &Path,
        identity: &WalletIdentity,
        context: Context,
    ) -> Result<Self, Error> {
        Self::valid_context(&context)?;
        let journal = Journal::open(directory)?;
        let intent = journal.load()?.ok_or(Error::InvalidJournal)?;
        validate(&intent)?;
        if &intent.identity != identity || intent.context_digest != context_digest(&context) {
            return Err(Error::WrongIdentity);
        }
        Ok(Self {
            id: controller_id()?,
            revoked: false,
            construction_verified: false,
            journal,
            intent,
            context,
            revision: 0,
            pending: false,
            status: Status::Unchecked,
            fresh: None,
        })
    }
    /// [`Self::reopen`] that waits out a lock about to be released, for the
    /// Claim loaders (`#607`). The journal is reopened right after its previous
    /// owner was dropped. On Unix a child this process spawns in that window
    /// (Spark bridge, bitcoind, Tor) holds a duplicate of the dropped owner's
    /// `claim.lock` descriptor until it execs, and `flock` belongs to the open
    /// file, so the first reopen can see [`Error::Busy`] for a moment (`#586`).
    ///
    /// Only `Busy` is retried, every [`REOPEN_BUSY_POLL`] until
    /// [`REOPEN_BUSY_BUDGET`] of wall-clock time has passed, with an async sleep
    /// so no thread is blocked. Every other result returns at once. A lock that
    /// is still held at the deadline, such as a live Claim in another tab, is
    /// reported as `Busy` as before.
    pub async fn reopen_settling(
        directory: &Path,
        identity: &WalletIdentity,
        context: Context,
    ) -> Result<Self, Error> {
        // Wall clock, not tokio's: a paused test clock must not end the wait
        // before a real lock holder has had any real time to let go.
        let deadline = std::time::Instant::now() + REOPEN_BUSY_BUDGET;
        loop {
            match Self::reopen(directory, identity, context.clone()) {
                Err(Error::Busy) if std::time::Instant::now() < deadline => {
                    tokio::time::sleep(REOPEN_BUSY_POLL).await
                }
                other => return other,
            }
        }
    }
    /// [`Self::reopen_settling`] for the synchronous Claim coordinator
    /// constructors. Same retry and budget. The first attempt runs inline; only
    /// a `Busy` wait blocks, and on a multi-thread tokio runtime (iced's
    /// executor) that wait runs under [`tokio::task::block_in_place`], so the
    /// runtime hands this worker's other tasks to another thread instead of
    /// stalling them (`#610` review F1). Elsewhere, including a current-thread
    /// test runtime, it sleeps the calling thread. It still must never run in
    /// an iced `update`: every production caller builds its coordinator in a
    /// `Task::perform` future. The wait is not cancellable, but it is bounded
    /// by [`REOPEN_BUSY_BUDGET`].
    pub fn reopen_settling_blocking(
        directory: &Path,
        identity: &WalletIdentity,
        context: Context,
    ) -> Result<Self, Error> {
        let deadline = std::time::Instant::now() + REOPEN_BUSY_BUDGET;
        let settle = || loop {
            match Self::reopen(directory, identity, context.clone()) {
                Err(Error::Busy) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(REOPEN_BUSY_POLL)
                }
                other => return other,
            }
        };
        match Self::reopen(directory, identity, context.clone()) {
            Err(Error::Busy) => {
                use tokio::runtime::{Handle, RuntimeFlavor};
                match Handle::try_current().map(|h| h.runtime_flavor()) {
                    Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(settle),
                    _ => settle(),
                }
            }
            other => other,
        }
    }
    /// Claim-only operations take Claim artifacts; a Split intent refuses them
    /// before any check could compare a Claim artifact with a Split record.
    fn claim_only(&mut self) -> Result<(), Error> {
        if self.intent.split.is_some() {
            self.clear_check();
            return Err(Error::WrongIdentity);
        }
        Ok(())
    }
    fn valid_context(context: &Context) -> Result<(), Error> {
        if context.account.is_empty() || context.provider.is_empty() {
            Err(Error::Revoked)
        } else {
            Ok(())
        }
    }
    fn ensure_context(&mut self, current: &Context) -> Result<(), Error> {
        if self.revoked || current != &self.context {
            self.invalidate();
            return Err(Error::Revoked);
        }
        Ok(())
    }
    pub fn invalidate(&mut self) {
        self.revoked = true;
        self.clear_check();
    }
    fn clear_check(&mut self) {
        self.pending = false;
        self.fresh = None;
        self.status = Status::Unchecked;
    }
    pub fn status(&self) -> Status {
        self.status
    }
    pub fn phase(&self) -> Phase {
        self.intent.phase
    }
    pub fn plan(&self) -> ClaimPlan {
        self.intent.plan.clone()
    }
    pub fn signed_txid(&self) -> Option<Txid> {
        self.intent.signed_txid
    }
    pub fn last_inclusion(&self) -> Option<BlockRef> {
        self.intent.plan.previous_confirmation
    }
    pub fn begin_check(&mut self, current: &Context) -> Result<Ticket, Error> {
        self.ensure_context(current)?;
        self.clear_check();
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        self.pending = true;
        Ok(Ticket {
            controller: self.id,
            revision: self.revision,
            context: current.clone(),
            digest: self.intent.unsigned_digest,
        })
    }
    pub fn apply_observation(
        &mut self,
        ticket: Ticket,
        current: &Context,
        result: Result<CollectedAssessment, Failure>,
        policy: Policy,
        now: i64,
    ) -> Result<Status, Error> {
        self.apply_collected(
            ticket,
            current,
            result.map(|collected| (collected, None)),
            policy,
            now,
        )
    }

    /// Consume a fresh collection rather than accepting an ancestry eligibility
    /// boolean. Provider, generation, path and plan are checked against this
    /// controller, and the proof is retained only for the current check.
    pub fn apply_ancestry_observation(
        &mut self,
        ticket: Ticket,
        current: &Context,
        result: Result<crate::services::claim_observation::http::CollectedAncestry, Failure>,
        policy: Policy,
        now: i64,
    ) -> Result<Status, Error> {
        self.apply_collected(
            ticket,
            current,
            result.map(|collected| (collected.assessment(), Some(collected))),
            policy,
            now,
        )
    }

    fn apply_collected(
        &mut self,
        ticket: Ticket,
        current: &Context,
        result: Result<
            (
                CollectedAssessment,
                Option<crate::services::claim_observation::http::CollectedAncestry>,
            ),
            Failure,
        >,
        policy: Policy,
        now: i64,
    ) -> Result<Status, Error> {
        self.ensure_context(current)?;
        if ticket.controller != self.id
            || !self.pending
            || ticket.revision != self.revision
            || ticket.context != self.context
            || ticket.digest != self.intent.unsigned_digest
        {
            self.clear_check();
            return Err(Error::LateObservation);
        }
        self.clear_check();
        let (result, ancestry) = match result {
            Ok(result) => result,
            Err(_) => {
                self.status = Status::Unavailable;
                return Ok(self.status);
            }
        };
        if result.generation != current.generation {
            return Err(Error::Revoked);
        }
        let o = result.observations;
        // Never trust the result's cached assessment, or its caller's plan/policy.
        if o.preflight.bitcoin != o.bitcoin.tip || o.preflight.fork != o.fork.tip {
            self.status = Status::Observation(Assessment::NeedsPreflightRecheck);
            return Ok(self.status);
        }
        let fresh = FreshObservation {
            observations: o,
            ancestry,
        };
        let assessment = self.assess_fresh(&fresh, policy, now)?;
        self.status = Status::Observation(assessment);
        if matches!(
            assessment,
            Assessment::WaitingForConfirmation
                | Assessment::WaitingForDepth { .. }
                | Assessment::ObservationsEligibleForPreflight
        ) {
            let mut next = self.intent.clone();
            if let TransactionLocation::Confirmed { block, .. } = o.bitcoin.location {
                next.plan.previous_confirmation = Some(block);
                if next.signed_txid.is_some() {
                    next.phase = Phase::Tracking;
                }
            }
            let changed = next.plan.previous_confirmation != self.intent.plan.previous_confirmation
                || next.phase != self.intent.phase;
            if changed {
                if let Err(error) = self.journal.store(&next) {
                    self.clear_check();
                    return Err(error);
                }
                self.intent = next;
            } else if let Err(error) = self.journal.ensure_current() {
                self.clear_check();
                return Err(error);
            }
            self.fresh = Some(fresh);
        }
        Ok(self.status)
    }
    /// Persist the exact fork-side unsigned plan only after a fresh depth/tip
    /// assessment of the Bitcoin poison. This is a restart record, not signing
    /// or broadcast permission. Replacing an existing plan is refused.
    pub fn prepare_fork_sweep(
        &mut self,
        current: &Context,
        sweep: &coincube_core::claim_spend::ClaimForkSweep,
        policy: Policy,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.claim_only()?;
        let observations = self.fresh.take().ok_or(Error::Unchecked)?;
        self.status = Status::Unchecked;
        if !self.construction_verified || self.intent.phase != Phase::Tracking {
            return Err(Error::Unchecked);
        }
        if sweep.chain() != self.intent.plan.fork_chain
            || Some(sweep.bitcoin_step1()) != self.intent.signed_txid
            || sha256::Hash::hash(sweep.descriptor().to_string().as_bytes())
                != self.intent.identity.descriptor_digest
        {
            return Err(Error::WrongIdentity);
        }
        let assessment = self.assess_fresh(&observations, policy, now)?;
        if assessment != Assessment::ObservationsEligibleForPreflight {
            return Err(Error::Unchecked);
        }
        if self.intent.fork_submission.is_some() {
            return Err(Error::Conflict);
        }
        let transaction = &sweep.psbt().unsigned_tx;
        let change_index = u32::from(sweep.change_index());
        if let Some(recorded) = &self.intent.fork_sweep {
            if recorded != transaction
                || self
                    .intent
                    .fork_change_index
                    .is_some_and(|index| index != change_index)
            {
                return Err(Error::Conflict);
            }
            if self.intent.fork_change_index.is_some() {
                return Ok(());
            }
            // Upgrade a v2 plan only after the same fresh observations and
            // authenticated construction required for initial admission.
        }
        let mut next = self.intent.clone();
        next.version = next.version.max(if next.bitcoin_change_index.is_some() {
            4
        } else {
            3
        });
        next.fork_sweep = Some(transaction.clone());
        next.fork_change_index = Some(change_index);
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }
    /// An untrusted restart record. The ordinary fork builder and current chain
    /// checks must reconstruct/revalidate it before use; this returns no authority.
    pub fn recorded_fork_sweep(&self) -> Option<&Transaction> {
        self.intent.fork_sweep.as_ref()
    }

    /// An untrusted derivation hint for rebuilding the recorded fork output.
    /// Recovery must derive the script and match the complete transaction; this
    /// index is not evidence of ownership or permission to reuse an address.
    /// Older v2 records have no hint and require wallet-based discovery.
    pub fn recorded_fork_change_index(
        &self,
    ) -> Option<coincube_core::miniscript::bitcoin::bip32::ChildNumber> {
        self.intent.fork_change_index.and_then(|index| {
            coincube_core::miniscript::bitcoin::bip32::ChildNumber::from_normal_idx(index).ok()
        })
    }

    /// Durably mark a fork submission as uncertain before the coordinator can
    /// perform network I/O. Requires the exact verified signed construction and
    /// another fresh Bitcoin depth/tip assessment after signing. The coordinator
    /// must additionally enforce current fork-backend policy and generation.
    /// Failure to write the journal must prevent the send; a saved intent must
    /// never be interpreted as permission to retry after an ambiguous outcome.
    pub fn record_fork_broadcast_intent(
        &mut self,
        current: &Context,
        signed: &coincube_core::claim_finalize::VerifiedClaimForkSweep,
        policy: Policy,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.claim_only()?;
        let observations = self.fresh.take().ok_or(Error::Unchecked)?;
        self.status = Status::Unchecked;
        if !self.construction_verified || self.intent.phase != Phase::Tracking {
            return Err(Error::Unchecked);
        }
        if self.intent.fork_submission.is_some() {
            return Err(Error::Conflict);
        }
        if signed.chain() != self.intent.plan.fork_chain
            || Some(signed.bitcoin_step1()) != self.intent.signed_txid
            || sha256::Hash::hash(signed.descriptor().to_string().as_bytes())
                != self.intent.identity.descriptor_digest
        {
            return Err(Error::WrongIdentity);
        }
        let mut unsigned = signed.transaction().clone();
        for input in &mut unsigned.input {
            input.witness.clear();
        }
        if self.intent.fork_sweep.as_ref() != Some(&unsigned) {
            return Err(Error::InvalidPlan);
        }
        if self.assess_fresh(&observations, policy, now)?
            != Assessment::ObservationsEligibleForPreflight
        {
            return Err(Error::Unchecked);
        }
        let mut next = self.intent.clone();
        next.fork_submission = Some(RecordedForkSubmission {
            txid: signed.transaction().compute_txid(),
            wtxid: signed.transaction().compute_wtxid(),
        });
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }
    pub fn recorded_fork_submission(&self) -> Option<RecordedForkSubmission> {
        self.intent.fork_submission
    }

    /// Durably record a possible external broadcast *before* it is attempted.
    /// This checks unsigned identity only, NOT witness validity or mempool policy.
    /// It returns no broadcast or step-two authorization and performs no network I/O.
    pub fn record_broadcast_intent(
        &mut self,
        current: &Context,
        signed: &Transaction,
        policy: Policy,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.claim_only()?;
        if !self.construction_verified {
            self.clear_check();
            return Err(Error::Unchecked);
        }
        let observations = self.fresh.take().ok_or(Error::Unchecked)?;
        self.status = Status::Unchecked;
        let assessment = self.assess_fresh(&observations, policy, now)?;
        if assessment != Assessment::WaitingForConfirmation || self.intent.phase != Phase::Intent {
            return Err(Error::Unchecked);
        }
        let mut unsigned = signed.clone();
        for input in &mut unsigned.input {
            input.witness.clear();
        }
        if digest(&unsigned) != self.intent.unsigned_digest
            || signed.input.iter().any(|i| i.witness.is_empty())
        {
            return Err(Error::InvalidPlan);
        }
        let mut next = self.intent.clone();
        next.signed_txid = Some(signed.compute_txid());
        next.phase = Phase::BroadcastUncertain;
        next.version = next.version.max(6);
        next.bitcoin_transaction = Some(signed.clone());
        next.bitcoin_attempts.push(BitcoinSubmissionAttempt {
            wtxid: Some(signed.compute_wtxid()),
        });
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }
}
#[cfg(all(test, any(unix, windows)))]
mod tests;
