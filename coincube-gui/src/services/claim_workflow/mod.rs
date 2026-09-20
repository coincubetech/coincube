//! Restart-safe intent bookkeeping only. No signing/broadcast/UI entry point.
mod journal;
use super::claim_observation::{CollectedAssessment, Failure, ObservationBundle};
use coincube_core::{
    chain::ChainId,
    claim::{self, Assessment, BlockRef, ClaimPlan, Poison, Policy, TransactionLocation},
    miniscript::bitcoin::{
        consensus,
        hashes::{sha256, Hash},
        Transaction, Txid,
    },
};
use journal::Journal;
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
    identity: WalletIdentity,
    plan: ClaimPlan,
    unsigned_digest: sha256::Hash,
    context_digest: sha256::Hash,
    signed_txid: Option<Txid>,
    phase: Phase,
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
    fresh: Option<ObservationBundle>,
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
    let p = &intent.plan;
    if intent.version != 1
        || intent.identity.bitcoin_cube.is_empty()
        || intent.identity.fork_cube.is_empty()
        || intent.identity.bitcoin_cube.len() > 256
        || intent.identity.fork_cube.len() > 256
        || intent.identity.bitcoin_cube == intent.identity.fork_cube
        || !matches!(
            (p.bitcoin_chain, p.fork_chain),
            (ChainId::Bitcoin, ChainId::BitcoinBlake2b)
                | (ChainId::Testnet4, ChainId::BitcoinBlake2bTestnet4)
        )
        || p.poison != Poison::OpReturn
        || p.step1.input.is_empty()
        || p.step1.input.iter().any(|i| {
            !i.script_sig.is_empty() || !i.witness.is_empty() || i.previous_output.is_null()
        })
        || p.claimed_prevouts.len() != p.step1.input.len()
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
        || p.step1
            .input
            .iter()
            .any(|i| !p.claimed_prevouts.contains(&i.previous_output))
        || !p
            .step1
            .output
            .iter()
            .any(|o| o.script_pubkey.is_op_return() && o.script_pubkey.len() > 83)
        || intent.unsigned_digest != digest(&p.step1)
        || intent
            .signed_txid
            .is_some_and(|id| id != p.step1.compute_txid())
        || (intent.phase == Phase::Intent) != intent.signed_txid.is_none()
    {
        return Err(Error::InvalidPlan);
    }
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
            },
            context,
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
        if artifact.chain() != self.intent.plan.bitcoin_chain
            || digest(&artifact.psbt().unsigned_tx) != self.intent.unsigned_digest
            || sha256::Hash::hash(artifact.descriptor().to_string().as_bytes())
                != self.intent.identity.descriptor_digest
        {
            self.construction_verified = false;
            return Err(Error::WrongIdentity);
        }
        self.construction_verified = true;
        Ok(())
    }
    pub fn identity(&self) -> &WalletIdentity {
        &self.intent.identity
    }

    fn create_intent(
        directory: &Path,
        identity: WalletIdentity,
        plan: ClaimPlan,
        context: Context,
    ) -> Result<Self, Error> {
        let intent = Intent {
            version: 1,
            identity,
            unsigned_digest: digest(&plan.step1),
            context_digest: context_digest(&context),
            plan,
            signed_txid: None,
            phase: Phase::Intent,
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
        let result = match result {
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
        let assessment = claim::assess(
            &self.intent.plan,
            o.bitcoin,
            o.fork,
            o.deployment,
            policy,
            now,
            Some(o.preflight),
        );
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
            if let Err(error) = self.journal.store(&next) {
                self.clear_check();
                return Err(error);
            }
            self.intent = next;
            self.fresh = Some(o);
        }
        Ok(self.status)
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
        if !self.construction_verified {
            self.clear_check();
            return Err(Error::Unchecked);
        }
        let observations = self.fresh.take().ok_or(Error::Unchecked)?;
        self.status = Status::Unchecked;
        let assessment = claim::assess(
            &self.intent.plan,
            observations.bitcoin,
            observations.fork,
            observations.deployment,
            policy,
            now,
            Some(observations.preflight),
        );
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
        validate(&next)?;
        self.journal.store(&next)?;
        self.intent = next;
        Ok(())
    }
}
#[cfg(all(test, unix))]
mod tests;
