//! Retained ancestry is structural data, never restored spend authority.
use super::*;
use coincube_core::{
    claim_ancestry::retained::{RetainedPath, MAX_ENCODED_BYTES},
    claim_finalize::{verify_ancestry_transaction, VerifiedAncestryTransfer},
    claim_spend::{
        reconstruct_ancestry_fork_sweep, reconstruct_ancestry_self_transfer, AncestrySelfTransfer,
        ClaimForkSweep,
    },
    descriptors::CoincubeDescriptor,
    miniscript::bitcoin::OutPoint,
    spend::{CandidateCoin, TxGetter},
};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredAncestry {
    selected: OutPoint,
    raw: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    derivation: Option<StoredDerivation>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDerivation {
    index: u32,
    is_change: bool,
}
impl StoredAncestry {
    fn decode(&self) -> Result<RetainedPath, Error> {
        if self.raw.len() > 2 * MAX_ENCODED_BYTES
            || self
                .derivation
                .as_ref()
                .is_some_and(|hint| hint.index >= (1 << 31))
        {
            return Err(Error::InvalidJournal);
        }
        let raw = hex::decode(&self.raw).map_err(|_| Error::InvalidJournal)?;
        RetainedPath::decode(self.selected, &raw).map_err(|_| Error::InvalidJournal)
    }
}
pub(super) fn validate_poison(intent: &Intent) -> Result<(), Error> {
    let p = &intent.plan;
    let inputs: BTreeSet<_> = p.step1.input.iter().map(|i| i.previous_output).collect();
    let claimed: BTreeSet<_> = p.claimed_prevouts.iter().copied().collect();
    match (&intent.ancestry, p.poison) {
        (None, Poison::OpReturn) if intent.version != 7 => {
            if inputs != claimed
                || !p
                    .step1
                    .output
                    .iter()
                    .any(|o| o.script_pubkey.is_op_return() && o.script_pubkey.len() > 83)
            {
                return Err(Error::InvalidPlan);
            }
        }
        (Some(stored), Poison::InputAncestry) if intent.version == 7 => {
            stored.decode()?;
            let mut shared = inputs;
            if p.bitcoin_chain != ChainId::Bitcoin
                || p.fork_chain != ChainId::BitcoinBlake2b
                || !shared.remove(&stored.selected)
                || shared.is_empty()
                || shared != claimed
                || p.step1.output.len() != 1
                || !p.step1.output[0].script_pubkey.is_p2wsh()
                || p.step1.output[0].value == coincube_core::miniscript::bitcoin::Amount::ZERO
            {
                return Err(Error::InvalidPlan);
            }
        }
        _ => return Err(Error::InvalidPlan),
    }
    Ok(())
}
/// Borrowed live proof for recovery checks; this value is never journaled.
#[derive(Clone, Copy)]
pub(crate) enum RecoveryObservation<'a> {
    Ordinary(CollectedAssessment),
    Ancestry(&'a crate::services::claim_observation::http::CollectedAncestry),
}
impl From<CollectedAssessment> for RecoveryObservation<'_> {
    fn from(value: CollectedAssessment) -> Self {
        Self::Ordinary(value)
    }
}
impl<'a> From<&'a crate::services::claim_observation::http::CollectedAncestry>
    for RecoveryObservation<'a>
{
    fn from(value: &'a crate::services::claim_observation::http::CollectedAncestry) -> Self {
        Self::Ancestry(value)
    }
}
impl RecoveryObservation<'_> {
    pub(super) fn data(self) -> CollectedAssessment {
        match self {
            Self::Ordinary(value) => value,
            Self::Ancestry(value) => value.assessment(),
        }
    }
}
impl Controller {
    pub(super) fn assess_recovery(
        &self,
        collected: RecoveryObservation<'_>,
        plan: &ClaimPlan,
        policy: Policy,
        now: i64,
    ) -> Result<Assessment, Error> {
        let o = collected.data().observations;
        match collected {
            RecoveryObservation::Ancestry(proof) => {
                if !self.construction_verified {
                    return Err(Error::Unchecked);
                }
                let path = self.recorded_ancestry()?.ok_or(Error::WrongIdentity)?;
                proof
                    .assess_verified_observations(
                        &path,
                        plan,
                        crate::services::claim_observation::http::AncestryContext {
                            provider: &self.context.provider,
                            generation: self.context.generation,
                            policy,
                            now,
                            tips: o.preflight,
                        },
                    )
                    .map(|result| result.assessment)
                    .map_err(|_| Error::Unchecked)
            }
            RecoveryObservation::Ordinary(_) => Ok(claim::assess(
                plan,
                o.bitcoin,
                o.fork,
                o.deployment,
                policy,
                now,
                Some(o.preflight),
            )),
        }
    }
    pub(super) fn assess_fresh(
        &self,
        fresh: &FreshObservation,
        policy: Policy,
        now: i64,
    ) -> Result<Assessment, Error> {
        let o = fresh.observations;
        match fresh.ancestry.as_ref() {
            Some(collected) => {
                if !self.construction_verified {
                    return Err(Error::Unchecked);
                }
                let path = self.recorded_ancestry()?.ok_or(Error::WrongIdentity)?;
                collected
                    .assess_verified_observations(
                        &path,
                        &self.intent.plan,
                        crate::services::claim_observation::http::AncestryContext {
                            provider: &self.context.provider,
                            generation: self.context.generation,
                            policy,
                            now,
                            tips: o.preflight,
                        },
                    )
                    .map(|checked| checked.assessment)
                    .map_err(|_| Error::Unchecked)
            }
            None => Ok(claim::assess(
                &self.intent.plan,
                o.bitcoin,
                o.fork,
                o.deployment,
                policy,
                now,
                Some(o.preflight),
            )),
        }
    }

    /// Persist the owned construction and its structurally verified raw path in
    /// the same atomic intent. Fresh qualification is deliberately not stored.
    pub fn create_ancestry(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        artifact: &AncestrySelfTransfer,
        path: &RetainedPath,
        context: Context,
    ) -> Result<Self, Error> {
        path.reverify().map_err(|_| Error::InvalidPlan)?;
        if path.selected() != artifact.poison_input() {
            return Err(Error::WrongIdentity);
        }
        let identity = WalletIdentity {
            bitcoin_cube,
            fork_cube,
            descriptor_digest: sha256::Hash::hash(artifact.descriptor().to_string().as_bytes()),
        };
        Self::create_intent_with_ancestry(
            directory,
            identity,
            ClaimPlan {
                bitcoin_chain: artifact.chain(),
                fork_chain: ChainId::BitcoinBlake2b,
                step1: artifact.psbt().unsigned_tx.clone(),
                claimed_prevouts: artifact.claimed_prevouts().to_vec(),
                poison: Poison::InputAncestry,
                previous_confirmation: None,
                tracked_txid: None,
            },
            context,
            Some(u32::from(artifact.change_index())),
            Some(StoredAncestry {
                selected: path.selected(),
                raw: hex::encode(path.encode()),
                derivation: Some(StoredDerivation {
                    index: u32::from(artifact.poison_derivation().0),
                    is_change: artifact.poison_derivation().1,
                }),
            }),
        )
    }
    /// Reverified structural links only. Callers must obtain new positive chain
    /// observations and rebuild owned construction before any further action.
    pub fn recorded_ancestry(&self) -> Result<Option<RetainedPath>, Error> {
        self.intent
            .ancestry
            .as_ref()
            .map(StoredAncestry::decode)
            .transpose()
    }
    /// Restore only the excluded input's owned metadata without a fork lookup.
    /// The persisted hint is checked against the descriptor and txid-verified
    /// retained prevout. Missing legacy hints return None. This establishes no
    /// maturity, unspentness, chain exclusivity, signature or spending authority.
    pub fn recorded_ancestry_input(
        &mut self,
        current: &Context,
        descriptor: &CoincubeDescriptor,
    ) -> Result<Option<(CandidateCoin, Transaction)>, Error> {
        self.ensure_context(current)?;
        self.clear_check();
        if sha256::Hash::hash(descriptor.to_string().as_bytes())
            != self.intent.identity.descriptor_digest
        {
            return Err(Error::WrongIdentity);
        }
        let Some(stored) = self.intent.ancestry.as_ref() else {
            return Ok(None);
        };
        let path = stored.decode()?;
        let Some(hint) = stored.derivation.as_ref() else {
            return Ok(None);
        };
        let index =
            coincube_core::miniscript::bitcoin::bip32::ChildNumber::from_normal_idx(hint.index)
                .map_err(|_| Error::InvalidJournal)?;
        let raw = path.links().first().ok_or(Error::InvalidJournal)?;
        let transaction: Transaction =
            coincube_core::miniscript::bitcoin::consensus::deserialize(&raw.transaction)
                .map_err(|_| Error::InvalidJournal)?;
        if transaction.compute_txid() != stored.selected.txid {
            return Err(Error::InvalidJournal);
        }
        let output = transaction
            .output
            .get(stored.selected.vout as usize)
            .ok_or(Error::InvalidJournal)?;
        let derived = if hint.is_change {
            descriptor.change_descriptor()
        } else {
            descriptor.receive_descriptor()
        }
        .derive(
            index,
            &coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only(),
        );
        if derived.script_pubkey() != output.script_pubkey {
            return Err(Error::InvalidJournal);
        }
        let coin = CandidateCoin {
            outpoint: stored.selected,
            amount: output.value,
            deriv_index: index,
            is_change: hint.is_change,
            must_select: true,
            sequence: None,
            ancestor_info: None,
        };
        Ok(Some((coin, transaction)))
    }

    /// Bind a new positive ancestry collection to the saved intent. This clears
    /// prior checks and never makes the controller eligible by itself.
    pub fn validate_ancestry_observation(
        &mut self,
        current: &Context,
        observed: &crate::services::claim_observation::http::DiscoveredAncestry,
        policy: Policy,
        now: i64,
        tips: coincube_core::claim::PreflightTips,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        if !self.construction_verified {
            return Err(Error::Unchecked);
        }
        let path = self.recorded_ancestry()?.ok_or(Error::WrongIdentity)?;
        observed
            .validate_for_plan(
                &path,
                &self.intent.plan,
                crate::services::claim_observation::http::AncestryContext {
                    provider: &current.provider,
                    generation: current.generation,
                    policy,
                    now,
                    tips,
                },
            )
            .map_err(|_| Error::Unchecked)
    }
    /// Rebuild a reopened intent from current owned metadata and authenticate
    /// any recorded witness. No address is reserved, journal rewritten, or fresh
    /// chain eligibility restored. Callers must requalify the ancestry and check
    /// maturity/spendability before signing or resuming a submission workflow.
    pub fn restore_ancestry(
        &mut self,
        current: &Context,
        descriptor: &CoincubeDescriptor,
        tx_getter: &mut impl TxGetter,
        coins: &[CandidateCoin],
    ) -> Result<(AncestrySelfTransfer, Option<VerifiedAncestryTransfer>), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        self.construction_verified = false;
        let path = self.recorded_ancestry()?.ok_or(Error::WrongIdentity)?;
        let dependency = path.reverify().map_err(|_| Error::InvalidJournal)?;
        let index = self
            .intent
            .bitcoin_change_index
            .ok_or(Error::InvalidJournal)?;
        let index = coincube_core::miniscript::bitcoin::bip32::ChildNumber::from_normal_idx(index)
            .map_err(|_| Error::InvalidJournal)?;
        let secp = coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only();
        // Fork-side reconstruction combines shared wallet metadata with the
        // excluded input's retained metadata. Bind by outpoint, then preserve
        // the recorded input order without silently dropping extra/duplicate data.
        let by_outpoint: std::collections::BTreeMap<_, _> =
            coins.iter().map(|coin| (coin.outpoint, coin)).collect();
        if by_outpoint.len() != coins.len() || coins.len() != self.intent.plan.step1.input.len() {
            return Err(Error::InvalidPlan);
        }
        let ordered = self
            .intent
            .plan
            .step1
            .input
            .iter()
            .map(|input| {
                by_outpoint
                    .get(&input.previous_output)
                    .map(|coin| **coin)
                    .ok_or(Error::InvalidPlan)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let built = reconstruct_ancestry_self_transfer(
            self.intent.plan.bitcoin_chain,
            descriptor,
            &secp,
            tx_getter,
            &ordered,
            index,
            &dependency,
            &self.intent.plan.step1,
        )
        .map_err(|_| Error::InvalidPlan)?;
        let signed = self
            .intent
            .bitcoin_transaction
            .as_ref()
            .map(|tx| {
                verify_ancestry_transaction(&built, tx, &secp).map_err(|_| Error::InvalidJournal)
            })
            .transpose()?;
        self.revalidate_ancestry_construction(current, &built)?;
        Ok((built, signed))
    }

    /// Rebuild a saved fork sweep using only the owned shared inputs. The
    /// ancestry input remains excluded, including from transaction lookups.
    /// The Bitcoin source must match this journal; the saved fork output and
    /// amount must match a fresh owned reconstruction. No signing metadata from
    /// disk is trusted and no chain eligibility or submission right is restored.
    pub fn restore_ancestry_fork_sweep(
        &mut self,
        current: &Context,
        source: &AncestrySelfTransfer,
        tx_getter: &mut impl TxGetter,
        coins: &[CandidateCoin],
    ) -> Result<ClaimForkSweep, Error> {
        self.revalidate_ancestry_construction(current, source)?;
        self.construction_verified = false;
        if self.intent.phase != Phase::Tracking
            || self.intent.signed_txid != Some(source.psbt().unsigned_tx.compute_txid())
        {
            return Err(Error::Unchecked);
        }
        let recorded = self
            .intent
            .fork_sweep
            .as_ref()
            .ok_or(Error::InvalidJournal)?;
        let index = self
            .recorded_fork_change_index()
            .ok_or(Error::InvalidJournal)?;
        let secp = coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only();
        let sweep = reconstruct_ancestry_fork_sweep(
            source,
            self.intent.plan.fork_chain,
            &secp,
            tx_getter,
            coins,
            index,
            recorded,
        )
        .map_err(|_| Error::InvalidPlan)?;
        self.construction_verified = true;
        Ok(sweep)
    }

    pub fn revalidate_ancestry_construction(
        &mut self,
        current: &Context,
        artifact: &AncestrySelfTransfer,
    ) -> Result<(), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        self.construction_verified = false;
        let path = self.recorded_ancestry()?.ok_or(Error::WrongIdentity)?;
        if artifact.chain() != self.intent.plan.bitcoin_chain
            || path.selected() != artifact.poison_input()
            || artifact.claimed_prevouts() != self.intent.plan.claimed_prevouts
            || digest(&artifact.psbt().unsigned_tx) != self.intent.unsigned_digest
            || sha256::Hash::hash(artifact.descriptor().to_string().as_bytes())
                != self.intent.identity.descriptor_digest
            || self.intent.bitcoin_change_index != Some(u32::from(artifact.change_index()))
        {
            return Err(Error::WrongIdentity);
        }
        self.construction_verified = true;
        Ok(())
    }
}
