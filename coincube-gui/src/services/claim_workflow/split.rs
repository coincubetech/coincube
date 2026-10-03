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
//!
//! Nothing here signs, broadcasts, or grants step-2 authority. A reopened
//! Split intent is Unchecked like a Claim one, and a recorded uncertain
//! submission can only be reconciled, or resent after a fresh review.
use super::*;
use crate::services::claim_observation::TransactionObservation;
use coincube_core::{
    foreign_split::{SplitSource, SplitStep1, SplitStep2, VerifiedSplitStep1, VerifiedSplitStep2},
    miniscript::{bitcoin::ScriptBuf, Descriptor, DescriptorPublicKey},
};
use std::str::FromStr;

/// The journal version of a Split intent. Binaries that predate it refuse
/// the file: `split` is an unknown field to them, and 8 is outside every
/// version they validate.
pub(super) const VERSION: u32 = 8;
/// Far above any supported descriptor (a 3-key `wsh(sortedmulti)` is under
/// 400 bytes); a bound on untrusted journal text, not a policy.
const MAX_DESCRIPTOR_BYTES: usize = 4096;
/// Explicit step-2 resends a journal may record (P3-3). With the submission
/// intent, step 2 has at most step 1's attempt bound.
pub const MAX_SPLIT_STEP2_RESUBMISSIONS: usize = recovery::MAX_BITCOIN_ATTEMPTS - 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SplitRecord {
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
    if intent.version != VERSION
        || intent.ancestry.is_some()
        || p.poison != Poison::OpReturn
        || !intent.identity.bitcoin_cube.is_empty()
        || intent.identity.fork_cube.is_empty()
        || intent.identity.fork_cube.len() > 256
        || intent.identity.fork_cube != record.target_cube
        || intent.identity.descriptor_digest != record.source_digest
        || fork_chain(p.bitcoin_chain) != Some(p.fork_chain)
        || p.step1.input.is_empty()
        || p.step1.input.iter().any(|i| {
            !i.script_sig.is_empty() || !i.witness.is_empty() || i.previous_output.is_null()
        })
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
        || (intent.phase == Phase::Intent) != intent.signed_txid.is_none()
        || intent
            .signed_txid
            .is_some_and(|id| Some(id) != p.tracked_txid)
    {
        return Err(Error::InvalidPlan);
    }
    // Every input claimed, each exactly once, with the OP_RETURN poison.
    ancestry::validate_poison(intent)?;
    if let Some(descriptors) = &record.descriptors {
        if descriptors.source()?.digest() != record.source_digest {
            return Err(Error::InvalidJournal);
        }
    }
    // The signed step 1 is the only place a signature may appear, and its
    // own txid is the one tracked on both chains.
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
    if let Some(sweep) = &intent.fork_sweep {
        let sweep_inputs: std::collections::BTreeSet<_> =
            sweep.input.iter().map(|i| i.previous_output).collect();
        let claimed: std::collections::BTreeSet<_> = p.claimed_prevouts.iter().copied().collect();
        if intent.phase != Phase::Tracking
            || intent.signed_txid.is_none()
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
    if record.target_index.is_some()
        && (intent.phase != Phase::Tracking || intent.signed_txid.is_none())
    {
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
    reorg::validate_history(intent)
}

impl Controller {
    /// Record a Split step 1 before any submission. `construction` is the
    /// opaque core builder output and `signed` its verified finalization;
    /// neither can come from a journal. `fork_height` is the authenticated
    /// anchor's fork height, and must be the one the construction was built
    /// with ([`SplitStep1::fork_height`]). Creates a
    /// version-8 intent with the public descriptors (P2) and refuses an
    /// existing intent in `directory`. The result is not submission
    /// authority: the coordinator still needs fresh observations.
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
            version: VERSION,
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
            }),
        };
        validate(&intent)?;
        Self::valid_context(&context)?;
        let mut journal = journal::Journal::open(directory)?;
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

    fn split_record(&self) -> Result<&SplitRecord, Error> {
        self.intent.split.as_ref().ok_or(Error::WrongIdentity)
    }

    /// Untrusted restart record; see [`RecordedSplit`]. `None` for Claim.
    pub fn recorded_split(&self) -> Result<Option<RecordedSplit>, Error> {
        let Some(record) = &self.intent.split else {
            return Ok(None);
        };
        Ok(Some(RecordedSplit {
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
    pub fn revalidate_split_construction(
        &mut self,
        current: &Context,
        construction: &SplitStep1,
        fork_height: u64,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        self.construction_verified = false;
        let record = self.split_record()?;
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
        self.split_record()?;
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
        if self.intent.split.is_none() {
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
    /// removes restart data and grants nothing.
    pub fn forget_split_descriptors(&mut self, current: &Context) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        self.split_record()?;
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
    /// this checks only its shape.
    pub fn record_split_target(
        &mut self,
        current: &Context,
        index: u32,
        script: ScriptBuf,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        let record = self.split_record()?;
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
        let record = self.split_record()?;
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
        if self.intent.split.is_none() {
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
        if self.intent.split.is_none() {
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
    /// grants nothing.
    pub fn split_step2_dead_end(&self) -> bool {
        self.intent.split.as_ref().is_some_and(|record| {
            self.intent.fork_submission.is_some()
                && record.step2_transaction.is_some()
                && !record.step2_observed
                && (!record.step2_returned
                    || record.step2_resubmissions.len() >= MAX_SPLIT_STEP2_RESUBMISSIONS)
        })
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
        if self.intent.split.is_none() {
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

    /// Abandon a Split that was never submitted: delete the whole intent,
    /// descriptors included (P2). Refused once a submission was recorded or
    /// an inclusion observed (the signed bytes may have been sent from
    /// elsewhere), since step 1 may then be on chain and must stay tracked.
    /// Absence of an observed inclusion is not proof it is not on chain; a
    /// caller offering this checks the chain first.
    pub fn abandon_split(mut self, current: &Context) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.split_record()?;
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
