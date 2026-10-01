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
//!   P2WSH or P2TR (reserved for step 2, B3b).
//! - The foreign public descriptors are kept until completion (owner decision
//!   P2), in the same owner-only (0600) journal, and then deleted.
//!
//! Nothing here signs, broadcasts, or grants step-2 authority. A reopened
//! Split intent is Unchecked like a Claim one, and a recorded uncertain
//! submission can only be reconciled.
use super::*;
use coincube_core::{
    foreign_split::{SplitSource, SplitStep1, VerifiedSplitStep1},
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
    /// script. Both or neither. Nothing in this slice writes them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_script: Option<ScriptBuf>,
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
    if let Some(submission) = intent.fork_submission {
        if intent
            .fork_sweep
            .as_ref()
            .is_none_or(|sweep| sweep.compute_txid() != submission.txid)
        {
            return Err(Error::InvalidPlan);
        }
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
