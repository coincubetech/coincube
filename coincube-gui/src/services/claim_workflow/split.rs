//! Split (#568) step 1 in the Claim journal: a version-8 intent whose source
//! is a foreign wallet's public descriptors, not a Bitcoin Cube.
//!
//! What differs from Claim, and only for an intent carrying a [`SplitRecord`]:
//! - The identity has no Bitcoin Cube. Its fork Cube is the BTCB2 target Cube
//!   and its descriptor digest is the source digest (owner decision D9).
//! - The signed step 1 is recorded from creation, and its own txid is the
//!   plan's tracked txid. P2PKH and P2SH-P2WPKH inputs carry their signatures
//!   in scriptSigs, which the txid commits to, so every observation uses
//!   [`ClaimPlan::step1_txid`], never the unsigned txid.
//! - Signatures (scriptSig or witness) appear only in that recorded signed
//!   transaction. The plan's step 1 and any fork sweep stay unsigned.
//! - A fork sweep's single output must be the recorded target script, native
//!   P2WSH or P2TR: the target Vault receive address reserved for step 2
//!   (B3b). The reservation is recorded once and reused until it is proven
//!   used (#592 I12); only then may a strictly higher index replace it, and
//!   never once a step 2 is recorded.
//! - Step 2's signed bytes are recorded with its submission intent, and the
//!   submission names their own txid: like step 1, a P2PKH or P2SH-P2WPKH
//!   input's scriptSig changes it.
//! - The foreign public descriptors are kept until completion (owner decision
//!   P2), in the same owner-only (0600) journal, and then deleted.
//! - After an uncertain step-2 submission (P3-3), an explicitly reviewed
//!   resend of exactly the recorded signed step 2 is possible only once the
//!   latest attempt's return without the route's acceptance was recorded,
//!   after control came back from a completed send. An accepted attempt, or
//!   one whose return is not on disk (cancelled, timed out, interrupted, or
//!   a failed write), is never resent. Every fresh BTCB2 read of the
//!   recorded step 2 runs with that record durably withdrawn
//!   ([`Step2ReturnHold`]), restored only after a read without a sighting,
//!   so a sighting the journal then fails to record still ends the resend.
//!   Each resend is recorded before it is attempted, and a step 2 ever seen
//!   on BTCB2 is recorded as observed: it left, so no resend is offered
//!   again. These fields are absent until
//!   used, so a journal without them serializes exactly as before. A binary
//!   that predates them refuses one that has them (`deny_unknown_fields`),
//!   and an ordinary send that comes back refused, or a reconcile that sees
//!   step 2, writes one, so a downgrade after either refuses the journal. A
//!   submission recorded before these fields existed has no recorded return
//!   and is never resent.
//! - A fork-only record (B4b-1b, `kind: Unified`, version 9) is the unified
//!   fallback: one BTCB2-only sweep of the splittable coins into the target
//!   Vault, signed `ALL|UNIFIED` (B4b-1a), with no step 1. Its plan's `step1`
//!   is the canonical empty transaction, it has no `bitcoin_transaction`, it
//!   is Tracking from creation with its target reserved from creation, and
//!   the sweep reuses the step-2 fields (`fork_sweep`, `step2_transaction`,
//!   `fork_submission`). A recorded submission reopens through the
//!   fork-only reconciler (B4b-3a, `UnifiedReconciler`); the step-2
//!   reconciler refuses the record (U2). Its only writers are
//!   [`Controller::create_unified_split`] and
//!   [`Controller::record_unified_broadcast_intent`], which take core's
//!   typed unified sweep and verified unified sweep (B4b-3a, Reviewer-650
//!   F2): only a sweep core built is recorded, and only a sweep core's
//!   finalizer verified `ALL|UNIFIED` (Protected) on every input is
//!   journaled as a submission. Every two-step writer refuses it, and the
//!   reverse. A two-step record stays version 8 and serializes exactly as
//!   before; a binary that predates the fork-only record refuses one by
//!   version (and by its `kind` field). Its abandonment, and the close of its dead end, are B4b-3's decisions, so
//!   both are refused here and the journal is kept.
//!
//! Nothing here signs, broadcasts, or grants step-2 authority. A reopened
//! Split intent is Unchecked like a Claim one, and a recorded uncertain
//! submission can only be reconciled, or resent after a fresh review.
use super::*;
use crate::services::claim_observation::TransactionObservation;
use coincube_core::{
    foreign_split::{
        SplitSource, SplitStep1, SplitStep2, UnifiedSweep, VerifiedSplitStep1, VerifiedSplitStep2,
        VerifiedUnifiedSweep,
    },
    miniscript::{
        bitcoin::{OutPoint, ScriptBuf},
        Descriptor, DescriptorPublicKey,
    },
};
use std::str::FromStr;

/// The newest journal version of a Split intent. A record is written at the
/// lowest version that represents it ([`SplitKind::version`]): a two-step
/// record stays at 8, byte-identical to before, so a binary that predates
/// the fork-only record keeps reading it; a fork-only record is 9, which
/// that binary refuses by version (and by the `kind` field it does not
/// know). Binaries that predate Split refuse both: `split` is an unknown
/// field to them, and 8 and 9 are outside every version they validate.
pub(super) const VERSION: u32 = 9;
/// Every version a Split intent may carry; `claim_workflow`'s reader sends
/// an intent at any of them to [`validate`].
pub(super) const VERSIONS: std::ops::RangeInclusive<u32> = 8..=VERSION;
/// Far above any supported descriptor (a 3-key `wsh(sortedmulti)` is under
/// 400 bytes); a bound on untrusted journal text, not a policy.
const MAX_DESCRIPTOR_BYTES: usize = 4096;
/// #625 F2 (A1 = A): the tombstone a split closed in its step-2 dead end
/// leaves in its journal directory. The journal stays; a new split is never
/// created in a directory that holds one (anything by that name), so a
/// partly reset directory can't hide a new journal behind an old tombstone.
pub const SPLIT_TOMBSTONE: &str = "closed.json";
/// Explicit step-2 resends a journal may record (P3-3). With the submission
/// intent, step 2 has at most step 1's attempt bound.
pub const MAX_SPLIT_STEP2_RESUBMISSIONS: usize = recovery::MAX_BITCOIN_ATTEMPTS - 1;

/// What a Split journal records (B4b-1b).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitKind {
    /// The two-step split: a signed Bitcoin step 1 tracked on both chains,
    /// then the BTCB2 step 2 into the target. Version 8; the field is
    /// absent from the journal, which serializes exactly as before.
    #[default]
    Split,
    /// The unified fallback (B4b): one BTCB2-only sweep of the splittable
    /// coins into the target, signed `ALL|UNIFIED`. There is no step 1, so
    /// the record's `step1` is the canonical empty transaction, it has no
    /// `bitcoin_transaction`, it is Tracking from creation and carries its
    /// target from creation; the sweep reuses the step-2 fields. Version 9.
    Unified,
}
impl SplitKind {
    fn is_split(&self) -> bool {
        matches!(self, Self::Split)
    }
    /// The journal version a record of this kind is written at: the lowest
    /// that represents it.
    fn version(self) -> u32 {
        match self {
            Self::Split => 8,
            Self::Unified => VERSION,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SplitRecord {
    /// Absent for [`SplitKind::Split`] (the version-8 shape); written for a
    /// fork-only record, so a binary without it refuses one.
    #[serde(default, skip_serializing_if = "SplitKind::is_split")]
    kind: SplitKind,
    source_digest: sha256::Hash,
    /// Public descriptors only (P2). Absent once deleted at completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    descriptors: Option<StoredDescriptors>,
    /// From the authenticated BTCB2 network anchor the construction used.
    fork_height: u64,
    /// Receive index of step 1's destination in the foreign wallet.
    destination: u32,
    target_cube: String,
    /// Reserved for step 2 (B3b): the target Vault's receive index and
    /// script. Both or neither; see [`Controller::record_split_target`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_script: Option<ScriptBuf>,
    /// The signed step 2, recorded with its submission intent (B3b). Its own
    /// txid is the recorded fork submission's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    step2_transaction: Option<Transaction>,
    /// Explicitly reviewed resends of that signed step 2 (P3-3), each
    /// recorded before it was attempted. The submission intent is the first
    /// attempt and is not listed here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    step2_resubmissions: Vec<Step2Resubmission>,
    /// A fresh read saw the recorded step 2 on BTCB2, in a mempool or a
    /// block (P3-3). It left: no resend is offered again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    step2_observed: bool,
    /// The latest attempt (the submission intent or the last resend) came
    /// back from a completed send without the route's acceptance, recorded
    /// after it returned (P3-3). Only then may a resend be reviewed;
    /// recording a resend clears it before that resend is sent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    step2_returned: bool,
    /// #568 S4, O4: after step 2 was submitted, fresh reads found step 1
    /// absent from Bitcoin and a claimed coin spent there by another
    /// transaction, so step 1 can never confirm and the split can't
    /// complete. Terminal (S4-D2): never cleared or replaced. Absent until
    /// recorded, so a journal without it serializes exactly as before and
    /// stays at version 8; a binary without the field refuses one that has
    /// it (`deny_unknown_fields`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    step1_conflict: Option<Step1Conflict>,
}

/// #568 S4, O4: the claimed coin a fresh Bitcoin read found spent by
/// another transaction while step 1 was absent, and the Bitcoin tip of the
/// reconcile that found it. The spender is not named (S4-D1: Connect does
/// not serve `/tx/{txid}/outspend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step1Conflict {
    outpoint: OutPoint,
    bitcoin_tip: BlockRef,
}
impl Step1Conflict {
    pub(crate) fn new(outpoint: OutPoint, bitcoin_tip: BlockRef) -> Self {
        Self {
            outpoint,
            bitcoin_tip,
        }
    }
    /// The claimed coin spent on Bitcoin by another transaction.
    pub fn outpoint(&self) -> OutPoint {
        self.outpoint
    }
    /// The Bitcoin tip of the reconcile that found it.
    pub fn bitcoin_tip(&self) -> BlockRef {
        self.bitcoin_tip
    }
}

/// The step-2 resend permission (`step2_returned`), durably withdrawn for the
/// span of one fresh BTCB2 read of the recorded step 2 (P3-3). Only a read
/// that found no sighting gives it back
/// ([`Controller::release_split_step2_return`]), or a resend consumes it
/// ([`Controller::record_split_step2_resubmission`]). Dropped otherwise (a
/// sighting, a failed write, an interruption, a revoked session), it stays
/// withdrawn. No Clone; it names the controller and the resends recorded
/// when it was taken.
pub(crate) struct Step2ReturnHold {
    controller: u64,
    resubmissions: usize,
}

/// One explicitly reviewed resend of the recorded signed step 2 (P3-3).
/// Every resend is of exactly the recorded bytes, so this names their wtxid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Step2Resubmission {
    wtxid: Wtxid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDescriptors {
    external: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    internal: Option<String>,
}

impl StoredDescriptors {
    fn new(source: &SplitSource) -> Self {
        Self {
            external: source.external().to_string(),
            internal: source.internal().map(ToString::to_string),
        }
    }
    /// Parse back into a checked [`SplitSource`]. The text must be exactly
    /// the canonical form written, so a parse cannot silently normalize a
    /// tampered file into something that hashes the same.
    fn source(&self) -> Result<SplitSource, Error> {
        let parse = |text: &str| -> Result<Descriptor<DescriptorPublicKey>, Error> {
            if text.len() > MAX_DESCRIPTOR_BYTES {
                return Err(Error::InvalidJournal);
            }
            let descriptor = Descriptor::from_str(text).map_err(|_| Error::InvalidJournal)?;
            if descriptor.to_string() != text {
                return Err(Error::InvalidJournal);
            }
            Ok(descriptor)
        };
        let internal = self.internal.as_deref().map(parse).transpose()?;
        SplitSource::new(parse(&self.external)?, internal).map_err(|_| Error::InvalidJournal)
    }
}

/// A Split journal's recorded context. Untrusted restart data: a caller must
/// reauthenticate the coins, rebuild the exact step 1 and pass it through
/// [`Controller::revalidate_split_construction`] before relying on any of it.
#[derive(Debug, Clone)]
pub struct RecordedSplit {
    pub kind: SplitKind,
    pub source_digest: sha256::Hash,
    /// `None` once the descriptors were deleted at completion.
    pub source: Option<SplitSource>,
    pub fork_height: u64,
    pub destination: u32,
    pub target_cube: String,
    pub target_index: Option<u32>,
    pub target_script: Option<ScriptBuf>,
}

/// The identity a Split intent is opened with: no Bitcoin Cube, the target
/// BTCB2 Cube, and the source digest (D9).
pub fn split_identity(target_cube: String, source_digest: sha256::Hash) -> WalletIdentity {
    WalletIdentity {
        bitcoin_cube: String::new(),
        fork_cube: target_cube,
        descriptor_digest: source_digest,
    }
}

fn fork_chain(bitcoin: ChainId) -> Option<ChainId> {
    match bitcoin {
        ChainId::Bitcoin => Some(ChainId::BitcoinBlake2b),
        ChainId::Testnet4 => Some(ChainId::BitcoinBlake2bTestnet4),
        _ => None,
    }
}
fn bitcoin_chain(fork: ChainId) -> Option<ChainId> {
    match fork {
        ChainId::BitcoinBlake2b => Some(ChainId::Bitcoin),
        ChainId::BitcoinBlake2bTestnet4 => Some(ChainId::Testnet4),
        _ => None,
    }
}

/// A fork-only record's `step1`: there is none, so its plan carries the
/// canonical empty transaction (no inputs, no outputs, version 2, lock time
/// 0) and nothing is ever tracked on Bitcoin.
fn empty_step1() -> Transaction {
    Transaction {
        version: coincube_core::miniscript::bitcoin::transaction::Version::TWO,
        lock_time: coincube_core::miniscript::bitcoin::absolute::LockTime::ZERO,
        input: Vec::new(),
        output: Vec::new(),
    }
}

/// Step 1 with every scriptSig and witness removed.
fn unsigned(tx: &Transaction) -> Transaction {
    let mut unsigned = tx.clone();
    for input in &mut unsigned.input {
        input.script_sig = ScriptBuf::new();
        input.witness.clear();
    }
    unsigned
}

/// Whether `signed` is the recorded unsigned step 1 with a signature on
/// every input and nothing else changed.
fn signs(signed: &Transaction, unsigned_digest: sha256::Hash) -> bool {
    digest(&unsigned(signed)) == unsigned_digest
        && signed
            .input
            .iter()
            .all(|input| !input.script_sig.is_empty() || !input.witness.is_empty())
}

pub(super) fn validate(intent: &Intent) -> Result<(), Error> {
    let record = intent.split.as_ref().ok_or(Error::InvalidPlan)?;
    let p = &intent.plan;
    let inputs: std::collections::BTreeSet<_> =
        p.step1.input.iter().map(|i| i.previous_output).collect();
    // What both kinds share: the kind's version, the identity, the chains,
    // no change hints, and a reservation of the right shape.
    if intent.version != record.kind.version()
        || intent.ancestry.is_some()
        || p.poison != Poison::OpReturn
        || !intent.identity.bitcoin_cube.is_empty()
        || intent.identity.fork_cube.is_empty()
        || intent.identity.fork_cube.len() > 256
        || intent.identity.fork_cube != record.target_cube
        || intent.identity.descriptor_digest != record.source_digest
        || fork_chain(p.bitcoin_chain) != Some(p.fork_chain)
        || inputs.len() != p.step1.input.len()
        || p.claimed_prevouts
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != p.claimed_prevouts.len()
        || intent.unsigned_digest != digest(&p.step1)
        || intent.fork_change_index.is_some()
        || intent.bitcoin_change_index.is_some()
        || record.fork_height == 0
        || record.destination >= 1 << 31
        || record.target_index.is_some() != record.target_script.is_some()
        || record.target_index.is_some_and(|index| index >= 1 << 31)
        || record
            .target_script
            .as_ref()
            .is_some_and(|script| !script.is_p2wsh() && !script.is_p2tr())
    {
        return Err(Error::InvalidPlan);
    }
    match record.kind {
        SplitKind::Split => {
            if p.step1.input.is_empty()
                || p.step1.input.iter().any(|i| {
                    !i.script_sig.is_empty() || !i.witness.is_empty() || i.previous_output.is_null()
                })
                || (intent.phase == Phase::Intent) != intent.signed_txid.is_none()
                || intent
                    .signed_txid
                    .is_some_and(|id| Some(id) != p.tracked_txid)
            {
                return Err(Error::InvalidPlan);
            }
            // Every input claimed, each exactly once, with the OP_RETURN poison.
            ancestry::validate_poison(intent)?;
        }
        SplitKind::Unified => {
            // No step 1 at all: the canonical empty transaction, nothing
            // signed, tracked or attempted on Bitcoin, Tracking from
            // creation, the target reserved and the sweep recorded from
            // creation. The claimed prevouts are the sweep's inputs
            // (checked with the sweep below).
            if p.step1 != empty_step1()
                || p.claimed_prevouts.is_empty()
                || p.claimed_prevouts.iter().any(|o| o.is_null())
                || p.tracked_txid.is_some()
                || p.previous_confirmation.is_some()
                || intent.signed_txid.is_some()
                || intent.phase != Phase::Tracking
                || intent.bitcoin_transaction.is_some()
                || !intent.bitcoin_attempts.is_empty()
                || !intent.inclusion_history.is_empty()
                || record.destination != 0
                || record.target_script.is_none()
                || intent.fork_sweep.is_none()
            {
                return Err(Error::InvalidPlan);
            }
        }
    }
    if let Some(descriptors) = &record.descriptors {
        if descriptors.source()?.digest() != record.source_digest {
            return Err(Error::InvalidJournal);
        }
    }
    if record.kind == SplitKind::Split {
        // The signed step 1 is the only place a signature may appear, and
        // its own txid is the one tracked on both chains.
        let signed = intent
            .bitcoin_transaction
            .as_ref()
            .ok_or(Error::InvalidJournal)?;
        let wtxid = signed.compute_wtxid();
        if !signs(signed, intent.unsigned_digest)
            || p.tracked_txid != Some(signed.compute_txid())
            || (intent.phase == Phase::Intent) != intent.bitcoin_attempts.is_empty()
            || intent.bitcoin_attempts.len() > recovery::MAX_BITCOIN_ATTEMPTS
            || intent
                .bitcoin_attempts
                .iter()
                .any(|attempt| attempt.wtxid != Some(wtxid))
        {
            return Err(Error::InvalidJournal);
        }
    }
    // A two-step record's step 1 must be tracked before the sweep and the
    // reservation; a fork-only record is Tracking from creation (above).
    let tracked = intent.phase == Phase::Tracking
        && (record.kind == SplitKind::Unified || intent.signed_txid.is_some());
    if let Some(sweep) = &intent.fork_sweep {
        let sweep_inputs: std::collections::BTreeSet<_> =
            sweep.input.iter().map(|i| i.previous_output).collect();
        let claimed: std::collections::BTreeSet<_> = p.claimed_prevouts.iter().copied().collect();
        if !tracked
            || sweep_inputs.len() != sweep.input.len()
            || sweep_inputs != claimed
            || sweep
                .input
                .iter()
                .any(|i| !i.script_sig.is_empty() || !i.witness.is_empty())
            || sweep.output.len() != 1
            || record.target_script.as_ref() != Some(&sweep.output[0].script_pubkey)
            || sweep.output[0].value == coincube_core::miniscript::bitcoin::Amount::ZERO
        {
            return Err(Error::InvalidPlan);
        }
    }
    // A reservation is only made once step 1 is tracked.
    if record.target_index.is_some() && !tracked {
        return Err(Error::InvalidPlan);
    }
    // The signed step 2 and its submission are recorded together; the
    // submission names the signed bytes' own txid and wtxid, and those bytes
    // are the recorded sweep with a signature on every input. Resends and an
    // observation exist only for a recorded step 2, and every resend is of
    // exactly its bytes.
    match (&intent.fork_submission, &record.step2_transaction) {
        (None, None) => {
            if !record.step2_resubmissions.is_empty()
                || record.step2_observed
                || record.step2_returned
            {
                return Err(Error::InvalidPlan);
            }
        }
        (Some(submission), Some(signed)) => {
            let sweep = intent.fork_sweep.as_ref().ok_or(Error::InvalidPlan)?;
            if unsigned(signed) != *sweep
                || signed
                    .input
                    .iter()
                    .any(|input| input.script_sig.is_empty() && input.witness.is_empty())
                || submission.txid != signed.compute_txid()
                || submission.wtxid != signed.compute_wtxid()
                || record.step2_resubmissions.len() > MAX_SPLIT_STEP2_RESUBMISSIONS
                || record
                    .step2_resubmissions
                    .iter()
                    .any(|attempt| attempt.wtxid != submission.wtxid)
            {
                return Err(Error::InvalidPlan);
            }
        }
        _ => return Err(Error::InvalidPlan),
    }
    // O4 (#568 S4): a two-step record's claimed coin, only after step 2 was
    // submitted.
    if record.step1_conflict.is_some_and(|conflict| {
        record.kind != SplitKind::Split
            || intent.fork_submission.is_none()
            || !p.claimed_prevouts.contains(&conflict.outpoint)
    }) {
        return Err(Error::InvalidPlan);
    }
    reorg::validate_history(intent)
}

impl Controller {
    /// Record a Split step 1 before any submission. `construction` is the
    /// opaque core builder output and `signed` its verified finalization;
    /// neither can come from a journal. `fork_height` is the authenticated
    /// anchor's fork height, and must be the one the construction was built
    /// with ([`SplitStep1::fork_height`]). Creates a
    /// version-8 intent (`kind: Split`) with the public descriptors (P2) and
    /// refuses an existing intent in `directory`. The result is not
    /// submission authority: the coordinator still needs fresh observations.
    pub fn create_split(
        directory: &Path,
        target_cube: String,
        construction: &SplitStep1,
        signed: &VerifiedSplitStep1,
        fork_height: u64,
        context: Context,
    ) -> Result<Self, Error> {
        let bitcoin_chain = construction.chain();
        let fork_chain = fork_chain(bitcoin_chain).ok_or(Error::InvalidPlan)?;
        let step1 = construction.psbt().unsigned_tx.clone();
        let unsigned_digest = digest(&step1);
        if fork_height != construction.fork_height()
            || signed.chain() != bitcoin_chain
            || signed.construction_txid() != construction.txid()
            || !signs(signed.transaction(), unsigned_digest)
        {
            return Err(Error::WrongIdentity);
        }
        let source = construction.source();
        let source_digest = source.digest();
        let intent = Intent {
            version: SplitKind::Split.version(),
            ancestry: None,
            identity: split_identity(target_cube.clone(), source_digest),
            plan: ClaimPlan {
                bitcoin_chain,
                fork_chain,
                claimed_prevouts: construction.claimed_prevouts(),
                step1,
                poison: Poison::OpReturn,
                previous_confirmation: None,
                tracked_txid: Some(signed.transaction().compute_txid()),
            },
            unsigned_digest,
            context_digest: context_digest(&context),
            signed_txid: None,
            phase: Phase::Intent,
            fork_sweep: None,
            fork_change_index: None,
            bitcoin_change_index: None,
            fork_submission: None,
            inclusion_history: Vec::new(),
            bitcoin_transaction: Some(signed.transaction().clone()),
            bitcoin_attempts: Vec::new(),
            split: Some(SplitRecord {
                kind: SplitKind::Split,
                source_digest,
                descriptors: Some(StoredDescriptors::new(source)),
                fork_height,
                destination: construction.destination(),
                target_cube,
                target_index: None,
                target_script: None,
                step2_transaction: None,
                step2_resubmissions: Vec::new(),
                step2_observed: false,
                step2_returned: false,
                step1_conflict: None,
            }),
        };
        Self::admit_split(directory, intent, context)
    }

    /// Record the unified fallback (B4b, `kind: Unified`): one BTCB2-only
    /// sweep of the splittable coins into the target Vault, with no step 1.
    /// `sweep` is core's opaque unified construction, built from freshly
    /// authenticated coins (B4b-1a), and `target_index` the target Vault's
    /// receive index its one output pays (B4b-3a, Reviewer-650 F2: no
    /// caller-described construction is accepted). The journal records its
    /// unsigned transaction, chain, source, fork height and target, checks
    /// its shape, and reserves the target from creation. Creates a version-9
    /// intent with the public descriptors (P2) that is Tracking from
    /// creation, since nothing is ever tracked on Bitcoin, and refuses an
    /// existing intent or a tombstone in `directory`. The result is not submission authority: the caller's
    /// gate (C2) holds the fresh evidence, and the signed bytes are recorded
    /// only with their submission intent
    /// ([`Self::record_unified_broadcast_intent`]).
    pub fn create_unified_split(
        directory: &Path,
        target_cube: String,
        sweep: &UnifiedSweep,
        target_index: u32,
        context: Context,
    ) -> Result<Self, Error> {
        let fork_chain = sweep.chain();
        let bitcoin_chain = bitcoin_chain(fork_chain).ok_or(Error::InvalidPlan)?;
        let unsigned = &sweep.psbt().unsigned_tx;
        if unsigned.input.is_empty()
            || unsigned
                .input
                .iter()
                .any(|i| !i.script_sig.is_empty() || !i.witness.is_empty())
            || unsigned.output.len() != 1
            || unsigned.output[0].script_pubkey.as_script() != sweep.target()
        {
            return Err(Error::InvalidPlan);
        }
        let source_digest = sweep.source().digest();
        let step1 = empty_step1();
        let unsigned_digest = digest(&step1);
        let intent = Intent {
            version: SplitKind::Unified.version(),
            ancestry: None,
            identity: split_identity(target_cube.clone(), source_digest),
            plan: ClaimPlan {
                bitcoin_chain,
                fork_chain,
                claimed_prevouts: unsigned.input.iter().map(|i| i.previous_output).collect(),
                step1,
                poison: Poison::OpReturn,
                previous_confirmation: None,
                tracked_txid: None,
            },
            unsigned_digest,
            context_digest: context_digest(&context),
            signed_txid: None,
            phase: Phase::Tracking,
            fork_sweep: Some(unsigned.clone()),
            fork_change_index: None,
            bitcoin_change_index: None,
            fork_submission: None,
            inclusion_history: Vec::new(),
            bitcoin_transaction: None,
            bitcoin_attempts: Vec::new(),
            split: Some(SplitRecord {
                kind: SplitKind::Unified,
                source_digest,
                descriptors: Some(StoredDescriptors::new(sweep.source())),
                fork_height: sweep.fork_height(),
                destination: 0,
                target_cube,
                target_index: Some(target_index),
                target_script: Some(sweep.target().to_owned()),
                step2_transaction: None,
                step2_resubmissions: Vec::new(),
                step2_observed: false,
                step2_returned: false,
                step1_conflict: None,
            }),
        };
        Self::admit_split(directory, intent, context)
    }

    /// Validate a new Split intent of either kind and write it as the only
    /// journal in `directory`, under the journal's lock.
    fn admit_split(directory: &Path, intent: Intent, context: Context) -> Result<Self, Error> {
        validate(&intent)?;
        Self::valid_context(&context)?;
        let mut journal = journal::Journal::open(directory)?;
        // Checked under the journal's lock, which the close holds to write it.
        if journal.load()?.is_some()
            || std::fs::symlink_metadata(directory.join(SPLIT_TOMBSTONE)).is_ok()
        {
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

    fn split_record(&self) -> Result<&SplitRecord, Error> {
        self.intent.split.as_ref().ok_or(Error::WrongIdentity)
    }

    /// The Split record if it is of `kind`. A Claim intent or the other
    /// kind refuses, so a two-step writer never touches a fork-only record
    /// and the reverse.
    fn record_of(&self, kind: SplitKind) -> Result<&SplitRecord, Error> {
        match &self.intent.split {
            Some(record) if record.kind == kind => Ok(record),
            _ => Err(Error::WrongIdentity),
        }
    }

    /// Untrusted restart record; see [`RecordedSplit`]. `None` for Claim.
    pub fn recorded_split(&self) -> Result<Option<RecordedSplit>, Error> {
        let Some(record) = &self.intent.split else {
            return Ok(None);
        };
        Ok(Some(RecordedSplit {
            kind: record.kind,
            source_digest: record.source_digest,
            source: record
                .descriptors
                .as_ref()
                .map(StoredDescriptors::source)
                .transpose()?,
            fork_height: record.fork_height,
            destination: record.destination,
            target_cube: record.target_cube.clone(),
            target_index: record.target_index,
            target_script: record.target_script.clone(),
        }))
    }

    /// Restart never restores the construction. The caller rebuilds step 1
    /// from freshly authenticated coins (`reconstruct_split_step1`) and this
    /// checks it is exactly the recorded one: same chain, unsigned bytes,
    /// source digest (and stored descriptors while kept), destination and
    /// fork height, both as supplied and as the construction was built. Anything else refuses and leaves the intent unverified.
    /// A fork-only record refuses it
    /// ([`Self::revalidate_unified_construction`] is its check).
    pub fn revalidate_split_construction(
        &mut self,
        current: &Context,
        construction: &SplitStep1,
        fork_height: u64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        self.record_of(SplitKind::Split)?;
        self.construction_verified = false;
        let record = self.record_of(SplitKind::Split)?;
        if construction.chain() != self.intent.plan.bitcoin_chain
            || digest(&construction.psbt().unsigned_tx) != self.intent.unsigned_digest
            || construction.source().digest() != record.source_digest
            || construction.destination() != record.destination
            || fork_height != record.fork_height
            || construction.fork_height() != record.fork_height
            || construction.claimed_prevouts() != self.intent.plan.claimed_prevouts
        {
            return Err(Error::WrongIdentity);
        }
        if let Some(descriptors) = &record.descriptors {
            if &descriptors.source()? != construction.source() {
                return Err(Error::WrongIdentity);
            }
        }
        self.construction_verified = true;
        Ok(())
    }

    /// Bind a freshly verified signed step 1 after
    /// [`Self::revalidate_split_construction`]. Once a submission was
    /// recorded or an inclusion observed, the signed bytes are immutable:
    /// only the identical transaction binds. Before that (phase Intent,
    /// nothing sent or seen) a re-signed transaction replaces the recorded
    /// one and its txid becomes the tracked txid.
    pub fn bind_recovered_split_transaction(
        &mut self,
        current: &Context,
        signed: &VerifiedSplitStep1,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.record_of(SplitKind::Split)?;
        if !self.construction_verified {
            return Err(Error::Unchecked);
        }
        let tx = signed.transaction();
        if signed.chain() != self.intent.plan.bitcoin_chain
            || !signs(tx, self.intent.unsigned_digest)
        {
            return Err(Error::WrongIdentity);
        }
        if self.intent.bitcoin_transaction.as_ref() == Some(tx) {
            return Ok(());
        }
        // Same rule as abandon_split: once a submission was recorded or an
        // inclusion observed, the recorded bytes may be on chain (#622 F1).
        if self.intent.phase != Phase::Intent
            || self.intent.plan.previous_confirmation.is_some()
            || !self.intent.inclusion_history.is_empty()
        {
            return Err(Error::Conflict);
        }
        let mut next = self.intent.clone();
        next.bitcoin_transaction = Some(tx.clone());
        next.plan.tracked_txid = Some(tx.compute_txid());
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        self.clear_check();
        Ok(())
    }

    /// The Split counterpart of [`Self::record_broadcast_intent`]: durably
    /// record a possible submission of exactly the recorded, verified signed
    /// step 1 before it is attempted, after a fresh assessment keyed by its
    /// own txid. Returns no broadcast authority and performs no network I/O.
    pub fn record_split_broadcast_intent(
        &mut self,
        current: &Context,
        signed: &VerifiedSplitStep1,
        policy: Policy,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        if self.record_of(SplitKind::Split).is_err() {
            self.clear_check();
            return Err(Error::WrongIdentity);
        }
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
        let tx = signed.transaction();
        if signed.chain() != self.intent.plan.bitcoin_chain
            || self.intent.bitcoin_transaction.as_ref() != Some(tx)
        {
            return Err(Error::InvalidPlan);
        }
        let mut next = self.intent.clone();
        next.signed_txid = Some(tx.compute_txid());
        next.phase = Phase::BroadcastUncertain;
        next.bitcoin_attempts.push(BitcoinSubmissionAttempt {
            wtxid: Some(tx.compute_wtxid()),
        });
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }

    /// Delete the foreign descriptors once the split is complete (P2). The
    /// txids, signed bytes and inclusion history stay. Only a tracked step 1
    /// can be completed; the completion evidence itself (B5) is checked by
    /// the caller, which is the only intended one. Dropping the descriptors
    /// removes restart data and grants nothing. A fork-only record refuses
    /// it: its completion is B4b-3's decision. A recorded step-1 conflict
    /// (O4, #568 S4) refuses it too: that split never completes.
    pub fn forget_split_descriptors(&mut self, current: &Context) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        // O4 (#568 S4): a split whose step 1 can never confirm never
        // completes, so its descriptors are never forgotten.
        if self.record_of(SplitKind::Split)?.step1_conflict.is_some() {
            return Err(Error::Conflict);
        }
        if self.intent.phase != Phase::Tracking || self.intent.signed_txid.is_none() {
            return Err(Error::Unchecked);
        }
        let mut next = self.intent.clone();
        if let Some(record) = next.split.as_mut() {
            if record.descriptors.take().is_none() {
                return Ok(());
            }
        }
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }

    /// Record the step-2 target: the target Vault's receive `index` and its
    /// `script`. A tracked step 1 only, before any step 2 is recorded. The
    /// reservation is kept and reused (#592 I12): recording the same one
    /// again is a no-op and any other is a [`Error::Conflict`]; replacing it
    /// needs [`Self::replace_used_split_target`]. The caller derives the
    /// script from the target Vault's own descriptor and proves it unused;
    /// this checks only its shape. A fork-only record refuses it: its
    /// target is fixed at creation.
    pub fn record_split_target(
        &mut self,
        current: &Context,
        index: u32,
        script: ScriptBuf,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        let record = self.record_of(SplitKind::Split)?;
        if record.target_index == Some(index) && record.target_script.as_ref() == Some(&script) {
            return Ok(());
        }
        if record.target_index.is_some() {
            return Err(Error::Conflict);
        }
        self.store_split_target(index, script)
    }

    /// Replace a reservation the caller proved used (an address with history
    /// cannot be step 2's fresh target). `used` must be the recorded index
    /// and `index` strictly higher, so a used index is never reserved again;
    /// refused once a step 2 is recorded.
    pub fn replace_used_split_target(
        &mut self,
        current: &Context,
        used: u32,
        index: u32,
        script: ScriptBuf,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        let record = self.record_of(SplitKind::Split)?;
        if record.target_index != Some(used) || index <= used {
            return Err(Error::Conflict);
        }
        self.store_split_target(index, script)
    }

    fn store_split_target(&mut self, index: u32, script: ScriptBuf) -> Result<(), Error> {
        if self.intent.fork_sweep.is_some() || self.intent.fork_submission.is_some() {
            return Err(Error::Conflict);
        }
        let mut next = self.intent.clone();
        if let Some(record) = next.split.as_mut() {
            record.target_index = Some(index);
            record.target_script = Some(script);
        }
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }

    /// Record the unsigned step 2 (the fork sweep) after a fresh assessment:
    /// the restart record, not signing or broadcast permission. It must spend
    /// exactly step 1's claimed prevouts into the reserved target, from the
    /// recorded source. An identical record is a no-op; any other refuses.
    pub fn prepare_split_step2(
        &mut self,
        current: &Context,
        construction: &SplitStep2,
        policy: Policy,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        if self.record_of(SplitKind::Split).is_err() {
            self.clear_check();
            return Err(Error::WrongIdentity);
        }
        let observations = self.fresh.take().ok_or(Error::Unchecked)?;
        self.status = Status::Unchecked;
        if !self.construction_verified || self.intent.phase != Phase::Tracking {
            return Err(Error::Unchecked);
        }
        let record = self.split_record()?;
        let claimed: std::collections::BTreeSet<_> =
            self.intent.plan.claimed_prevouts.iter().copied().collect();
        let spent: std::collections::BTreeSet<_> =
            construction.claimed_prevouts().into_iter().collect();
        if construction.chain() != self.intent.plan.fork_chain
            || construction.source().digest() != record.source_digest
            || spent != claimed
            || record.target_script.as_deref() != Some(construction.target())
        {
            return Err(Error::WrongIdentity);
        }
        if self.assess_fresh(&observations, policy, now)?
            != Assessment::ObservationsEligibleForPreflight
        {
            return Err(Error::Unchecked);
        }
        if self.intent.fork_submission.is_some() {
            return Err(Error::Conflict);
        }
        let transaction = &construction.psbt().unsigned_tx;
        if let Some(recorded) = &self.intent.fork_sweep {
            return if recorded == transaction {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        let mut next = self.intent.clone();
        next.fork_sweep = Some(transaction.clone());
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }

    /// Durably record a possible submission of exactly this verified signed
    /// step 2 before it is attempted, after another fresh assessment. Like
    /// [`Self::record_fork_broadcast_intent`], a saved intent never permits a
    /// retry; it can only be reconciled.
    pub fn record_split_step2_broadcast_intent(
        &mut self,
        current: &Context,
        signed: &VerifiedSplitStep2,
        policy: Policy,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        if self.record_of(SplitKind::Split).is_err() {
            self.clear_check();
            return Err(Error::WrongIdentity);
        }
        let observations = self.fresh.take().ok_or(Error::Unchecked)?;
        self.status = Status::Unchecked;
        if !self.construction_verified || self.intent.phase != Phase::Tracking {
            return Err(Error::Unchecked);
        }
        if self.intent.fork_submission.is_some() {
            return Err(Error::Conflict);
        }
        let tx = signed.transaction();
        if signed.chain() != self.intent.plan.fork_chain
            || self.intent.fork_sweep.as_ref() != Some(&unsigned(tx))
        {
            return Err(Error::InvalidPlan);
        }
        if self.assess_fresh(&observations, policy, now)?
            != Assessment::ObservationsEligibleForPreflight
        {
            return Err(Error::Unchecked);
        }
        let mut next = self.intent.clone();
        next.fork_submission = Some(RecordedForkSubmission {
            txid: tx.compute_txid(),
            wtxid: tx.compute_wtxid(),
        });
        if let Some(record) = next.split.as_mut() {
            record.step2_transaction = Some(tx.clone());
        }
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }

    /// The recorded signed step 2, once its submission intent was recorded.
    /// Untrusted restart data: it identifies what to reconcile, nothing more.
    pub fn recorded_split_step2(&self) -> Option<&Transaction> {
        self.intent
            .split
            .as_ref()
            .and_then(|record| record.step2_transaction.as_ref())
    }

    /// The explicit step-2 resends recorded so far (P3-3); the submission
    /// intent is not counted. At [`MAX_SPLIT_STEP2_RESUBMISSIONS`] no more
    /// are recorded.
    pub fn split_step2_resubmissions(&self) -> usize {
        self.intent
            .split
            .as_ref()
            .map_or(0, |record| record.step2_resubmissions.len())
    }

    /// Whether a fresh read ever saw the recorded step 2 on BTCB2 (P3-3).
    pub fn split_step2_observed(&self) -> bool {
        self.intent
            .split
            .as_ref()
            .is_some_and(|record| record.step2_observed)
    }

    /// Whether the latest step-2 attempt is recorded as having come back
    /// without the route's acceptance (P3-3); only then may a resend be
    /// reviewed.
    pub fn split_step2_returned(&self) -> bool {
        self.intent
            .split
            .as_ref()
            .is_some_and(|record| record.step2_returned)
    }

    /// A recorded step-2 submission that no resend can follow and that no
    /// read ever saw on BTCB2 (#625 F2, #639 N1): the resend permission is
    /// withdrawn (an accepted, cancelled, timed-out or interrupted send, or a
    /// read that didn't give it back), or the resend limit is reached. Only
    /// such a journal may be closed after a fresh chain check. Read-only; it
    /// grants nothing. Never for a fork-only record: the close checks step
    /// 1 on Bitcoin, which it does not have, and its close is B4b-3's
    /// decision.
    pub fn split_step2_dead_end(&self) -> bool {
        self.intent.split.as_ref().is_some_and(|record| {
            record.kind == SplitKind::Split
                && self.intent.fork_submission.is_some()
                && record.step2_transaction.is_some()
                && !record.step2_observed
                && (!record.step2_returned
                    || record.step2_resubmissions.len() >= MAX_SPLIT_STEP2_RESUBMISSIONS)
        })
    }

    /// #568 S4, O4: the recorded step-1 conflict, if any. Terminal: once
    /// recorded it is never cleared, and the split can't complete.
    pub fn split_step1_conflict(&self) -> Option<Step1Conflict> {
        self.intent
            .split
            .as_ref()
            .and_then(|record| record.step1_conflict)
    }

    /// #568 S4, O4: record that fresh Bitcoin reads after the step-2
    /// submission found step 1 absent and `conflict`'s coin spent by another
    /// transaction. The step-2 reconciler's fresh reads are the only caller;
    /// a read failure never comes here. A two-step record with a recorded
    /// step-2 submission only. Terminal (S4-D2): a recorded conflict is kept
    /// and never replaced, so recording again is a no-op. Grants nothing
    /// and leaves the current check alone.
    pub(crate) fn record_split_step1_conflict(
        &mut self,
        current: &Context,
        conflict: Step1Conflict,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        let record = self.record_of(SplitKind::Split)?;
        if self.intent.fork_submission.is_none() {
            return Err(Error::InvalidPlan);
        }
        if record.step1_conflict.is_some() {
            return Ok(());
        }
        let mut next = self.intent.clone();
        if let Some(record) = next.split.as_mut() {
            record.step1_conflict = Some(conflict);
        }
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }

    /// Record that the latest step-2 attempt came back from a completed send
    /// without the route's acceptance (P3-3). The coordinator's send is the
    /// only caller, after control returns: never after an acceptance, a
    /// cancellation or an expired bound, so an attempt that may have been
    /// accepted is never marked. It grants nothing by itself; a resend still
    /// needs its own review and fresh evidence.
    pub(crate) fn record_split_step2_returned(&mut self, current: &Context) -> Result<(), Error> {
        self.ensure_context(current)?;
        let record = self.split_record()?;
        if self.intent.fork_submission.is_none() {
            return Err(Error::InvalidPlan);
        }
        if record.step2_returned {
            return Ok(());
        }
        let mut next = self.intent.clone();
        if let Some(record) = next.split.as_mut() {
            record.step2_returned = true;
        }
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }

    /// Before a fresh BTCB2 read of the recorded step 2: durably withdraw the
    /// resend permission, so the read cannot leave it standing if it finds
    /// the step 2 and that sighting then fails to record. `None` when there
    /// is no permission to withdraw. Leaves the current check alone.
    pub(crate) fn hold_split_step2_return(
        &mut self,
        current: &Context,
    ) -> Result<Option<Step2ReturnHold>, Error> {
        self.ensure_context(current)?;
        let record = self.split_record()?;
        if !record.step2_returned {
            return Ok(None);
        }
        let hold = Step2ReturnHold {
            controller: self.id,
            resubmissions: record.step2_resubmissions.len(),
        };
        let mut next = self.intent.clone();
        if let Some(record) = next.split.as_mut() {
            record.step2_returned = false;
        }
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(Some(hold))
    }

    /// After that read found no sighting: give the withdrawn permission
    /// back. Refused for another controller's hold, or once anything was
    /// recorded since (a sighting or a resend).
    pub(crate) fn release_split_step2_return(
        &mut self,
        current: &Context,
        hold: Step2ReturnHold,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        let record = self.split_record()?;
        if hold.controller != self.id
            || self.intent.fork_submission.is_none()
            || record.step2_returned
            || record.step2_observed
            || record.step2_resubmissions.len() != hold.resubmissions
        {
            return Err(Error::Conflict);
        }
        let mut next = self.intent.clone();
        if let Some(record) = next.split.as_mut() {
            record.step2_returned = true;
        }
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }

    /// Record that a fresh read keyed by the recorded signed step 2's own
    /// txid saw it on BTCB2 (`seen`, mempool or block). Monotonic, and an
    /// absence is a no-op. It only takes the resend away and grants nothing,
    /// so it leaves the current check alone: the caller still applies the
    /// collection the read came from.
    pub fn record_split_step2_observed(
        &mut self,
        current: &Context,
        seen: TransactionObservation,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        let record = self.split_record()?;
        let txid = match seen {
            TransactionObservation::Absent => return Ok(()),
            TransactionObservation::Unconfirmed { txid }
            | TransactionObservation::Confirmed { txid, .. } => txid,
        };
        if self.intent.fork_submission.map(|s| s.txid) != Some(txid) {
            return Err(Error::InvalidPlan);
        }
        if record.step2_observed {
            return Ok(());
        }
        let mut next = self.intent.clone();
        if let Some(record) = next.split.as_mut() {
            record.step2_observed = true;
        }
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }

    /// P3-3: durably record one explicitly reviewed resend of exactly the
    /// recorded signed step 2 before it is attempted. Needs another fresh
    /// assessment (the coordinator's resend review, applied with a ticket)
    /// and `step2`, that same collection's read of the recorded step 2 on
    /// BTCB2, which must be absent. Refused for any other bytes, once the
    /// step 2 was ever seen there, and at the attempt limit. `hold` is the
    /// resend permission (the latest attempt's recorded return) withdrawn
    /// for that read: this consumes it whatever the result, so this resend's
    /// own return must be recorded before another. Like the submission
    /// intent, a recorded resend never permits another; each one needs its
    /// own review. Performs no network I/O.
    pub(crate) fn record_split_step2_resubmission(
        &mut self,
        current: &Context,
        signed: &VerifiedSplitStep2,
        step2: TransactionObservation,
        hold: Step2ReturnHold,
        policy: Policy,
        now: i64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        if self.record_of(SplitKind::Split).is_err() {
            self.clear_check();
            return Err(Error::WrongIdentity);
        }
        let observations = self.fresh.take().ok_or(Error::Unchecked)?;
        self.status = Status::Unchecked;
        if !self.construction_verified
            || self.intent.phase != Phase::Tracking
            || self.intent.fork_submission.is_none()
            || step2 != TransactionObservation::Absent
        {
            return Err(Error::Unchecked);
        }
        let record = self.split_record()?;
        let tx = signed.transaction();
        if signed.chain() != self.intent.plan.fork_chain
            || record.step2_transaction.as_ref() != Some(tx)
        {
            return Err(Error::InvalidPlan);
        }
        if record.step2_observed
            || record.step2_returned
            || hold.controller != self.id
            || hold.resubmissions != record.step2_resubmissions.len()
            || record.step2_resubmissions.len() >= MAX_SPLIT_STEP2_RESUBMISSIONS
        {
            return Err(Error::Conflict);
        }
        if self.assess_fresh(&observations, policy, now)?
            != Assessment::ObservationsEligibleForPreflight
        {
            return Err(Error::Unchecked);
        }
        let mut next = self.intent.clone();
        if let Some(record) = next.split.as_mut() {
            // The permission stays withdrawn (`hold`): this resend's own
            // return must be recorded before another.
            record.step2_resubmissions.push(Step2Resubmission {
                wtxid: tx.compute_wtxid(),
            });
        }
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }

    /// Restart never restores the construction. The caller rebuilds the
    /// unified sweep from freshly authenticated coins (core's
    /// `reconstruct_unified_sweep`) and this checks it is exactly the
    /// recorded one: same fork chain, unsigned bytes, source digest (and
    /// stored descriptors while kept), fork height, target script and
    /// `target_index`.
    /// Anything else refuses and leaves the intent unverified. A two-step
    /// record refuses it ([`Self::revalidate_split_construction`] is its
    /// check).
    pub fn revalidate_unified_construction(
        &mut self,
        current: &Context,
        sweep: &UnifiedSweep,
        target_index: u32,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        self.record_of(SplitKind::Unified)?;
        self.construction_verified = false;
        let record = self.record_of(SplitKind::Unified)?;
        if sweep.chain() != self.intent.plan.fork_chain
            || self.intent.fork_sweep.as_ref() != Some(&sweep.psbt().unsigned_tx)
            || sweep.source().digest() != record.source_digest
            || sweep.fork_height() != record.fork_height
            || record.target_index != Some(target_index)
            || record.target_script.as_deref() != Some(sweep.target())
        {
            return Err(Error::WrongIdentity);
        }
        if let Some(descriptors) = &record.descriptors {
            if &descriptors.source()? != sweep.source() {
                return Err(Error::WrongIdentity);
            }
        }
        self.construction_verified = true;
        Ok(())
    }

    /// Durably record a possible submission of exactly the signed unified
    /// sweep before it is attempted: the recorded unsigned sweep with a
    /// signature on every input, on the record's fork chain, once the
    /// construction was verified in this session
    /// ([`Self::create_unified_split`] or
    /// [`Self::revalidate_unified_construction`]). `verified` is core's
    /// verified unified sweep (B4b-1a), whose finalizer is the only check of
    /// the signatures and of their `ALL|UNIFIED` type: it exists only for a
    /// sweep every input of which is Protected, so a mis-signed sweep can
    /// never be journaled (B4b-3a, Reviewer-650 F2). This checks its chain,
    /// construction and unsigned identity, like
    /// [`Self::record_broadcast_intent`]. As for step 2, the submission
    /// names the signed bytes' own txid, and a saved intent never permits a
    /// retry: it can only be reconciled. The journal cannot assess a
    /// fork-only record (there is no step 1 to observe), so no fresh
    /// assessment is consumed here: the caller's gate (C2) holds the fresh
    /// evidence. Returns no broadcast authority and performs no network I/O.
    pub fn record_unified_broadcast_intent(
        &mut self,
        current: &Context,
        verified: &VerifiedUnifiedSweep,
    ) -> Result<(), Error> {
        let (chain, signed) = (verified.chain(), verified.transaction());
        self.ensure_context(current)?;
        self.clear_check();
        self.record_of(SplitKind::Unified)?;
        if !self.construction_verified {
            return Err(Error::Unchecked);
        }
        if self.intent.fork_submission.is_some() {
            return Err(Error::Conflict);
        }
        if chain != self.intent.plan.fork_chain
            || self.intent.fork_sweep.as_ref() != Some(&unsigned(signed))
            || self
                .intent
                .fork_sweep
                .as_ref()
                .map(Transaction::compute_txid)
                != Some(verified.construction_txid())
            || signed
                .input
                .iter()
                .any(|input| input.script_sig.is_empty() && input.witness.is_empty())
        {
            return Err(Error::InvalidPlan);
        }
        let mut next = self.intent.clone();
        next.fork_submission = Some(RecordedForkSubmission {
            txid: signed.compute_txid(),
            wtxid: signed.compute_wtxid(),
        });
        if let Some(record) = next.split.as_mut() {
            record.step2_transaction = Some(signed.clone());
        }
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }

    /// Abandon a Split that was never submitted: delete the whole intent,
    /// descriptors included (P2). Refused once a submission was recorded or
    /// an inclusion observed (the signed bytes may have been sent from
    /// elsewhere), since step 1 may then be on chain and must stay tracked.
    /// Absence of an observed inclusion is not proof it is not on chain; a
    /// caller offering this checks the chain first. A fork-only record is
    /// always refused, and kept: it is Tracking from creation, and its
    /// abandonment is B4b-3's decision.
    pub fn abandon_split(mut self, current: &Context) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.split_record()?;
        if self.record_of(SplitKind::Split).is_err() {
            return Err(Error::Conflict);
        }
        if self.intent.phase != Phase::Intent
            || !self.intent.bitcoin_attempts.is_empty()
            || self.intent.plan.previous_confirmation.is_some()
        {
            return Err(Error::Conflict);
        }
        self.journal.remove()
    }
}

#[cfg(all(test, any(unix, windows)))]
pub(super) mod tests;
