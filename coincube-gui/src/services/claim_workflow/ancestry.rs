//! Retained ancestry is structural data, never restored spend authority.
use super::*;
use coincube_core::{
    claim_ancestry::retained::{RetainedPath, MAX_ENCODED_BYTES},
    claim_finalize::{verify_ancestry_transaction, VerifiedAncestryTransfer},
    claim_spend::{
        reconstruct_ancestry_fork_sweep, reconstruct_ancestry_fork_sweep_with_marker,
        reconstruct_ancestry_self_transfer, AncestrySelfTransfer, ClaimForkSweep,
    },
    descriptors::CoincubeDescriptor,
    miniscript::bitcoin::OutPoint,
    spend::{CandidateCoin, TxGetter},
};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredAncestry {
    pub(super) selected: OutPoint,
    pub(super) raw: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) derivation: Option<StoredDerivation>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredDerivation {
    pub(super) index: u32,
    pub(super) is_change: bool,
}
/// Version-10 Claim metadata. Both paths are structural records only; a
/// restart must rebuild and recollect live proof before any signing or send.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredFundingPlan {
    pub(super) fork: StoredAncestry,
    pub(super) step2_unsigned: Transaction,
    pub(super) bitcoin_feerate_vb: u64,
    pub(super) fork_feerate_vb: u64,
    pub(super) fork_change_index: u32,
}

impl StoredFundingPlan {
    pub(super) fn fork_selected(&self) -> OutPoint {
        self.fork.selected
    }
    fn validate(&self, intent: &Intent, bitcoin: &StoredAncestry) -> Result<(), Error> {
        let p = &intent.plan;
        let bitcoin_path = bitcoin.decode()?;
        let fork_path = self.fork.decode()?;
        let shared: BTreeSet<_> = p.claimed_prevouts.iter().copied().collect();
        let mut first: BTreeSet<_> = p.step1.input.iter().map(|i| i.previous_output).collect();
        let mut second: BTreeSet<_> = self
            .step2_unsigned
            .input
            .iter()
            .map(|i| i.previous_output)
            .collect();
        if !matches!(
            (p.bitcoin_chain, p.fork_chain),
            (ChainId::Bitcoin, ChainId::BitcoinBlake2b)
                | (ChainId::Testnet4, ChainId::BitcoinBlake2bTestnet4)
        ) || p.poison != Poison::InputAncestry
            || p.tracked_txid.is_some()
            || bitcoin.derivation.is_none()
            || self.fork.derivation.is_none()
            || self.fork_change_index >= (1 << 31)
            || !(1..=coincube_core::spend::MAX_FEERATE).contains(&self.bitcoin_feerate_vb)
            || !(1..=coincube_core::spend::MAX_FEERATE).contains(&self.fork_feerate_vb)
            || shared.is_empty()
            || shared.len() != p.claimed_prevouts.len()
            || first.len() != p.step1.input.len()
            || second.len() != self.step2_unsigned.input.len()
            || !first.remove(&bitcoin_path.selected())
            || !second.remove(&fork_path.selected())
            || bitcoin_path.selected() == fork_path.selected()
            || first != shared
            || second != shared
            || self.step2_unsigned.output.len() != 1
            || !self.step2_unsigned.output[0].script_pubkey.is_p2wsh()
            || self.step2_unsigned.output[0].value
                == coincube_core::miniscript::bitcoin::Amount::ZERO
            || self.step2_unsigned.input.iter().any(|input| {
                input.previous_output.is_null()
                    || !input.script_sig.is_empty()
                    || !input.witness.is_empty()
            })
            || p.step1.output.len() != 1
            || !p.step1.output[0].script_pubkey.is_p2wsh()
            || p.step1.output[0].value == coincube_core::miniscript::bitcoin::Amount::ZERO
            || intent
                .fork_change_index
                .is_some_and(|i| i != self.fork_change_index)
            || intent
                .fork_sweep
                .as_ref()
                .is_some_and(|sweep| sweep != &self.step2_unsigned)
        {
            return Err(Error::InvalidPlan);
        }
        Ok(())
    }
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

    fn owned_input(
        &self,
        descriptor: &CoincubeDescriptor,
    ) -> Result<Option<(CandidateCoin, Transaction)>, Error> {
        let path = self.decode()?;
        let Some(hint) = self.derivation.as_ref() else {
            return Ok(None);
        };
        let index =
            coincube_core::miniscript::bitcoin::bip32::ChildNumber::from_normal_idx(hint.index)
                .map_err(|_| Error::InvalidJournal)?;
        let raw = path.links().first().ok_or(Error::InvalidJournal)?;
        let transaction: Transaction =
            coincube_core::miniscript::bitcoin::consensus::deserialize(&raw.transaction)
                .map_err(|_| Error::InvalidJournal)?;
        if transaction.compute_txid() != self.selected.txid {
            return Err(Error::InvalidJournal);
        }
        let output = transaction
            .output
            .get(self.selected.vout as usize)
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
        Ok(Some((
            CandidateCoin {
                outpoint: self.selected,
                amount: output.value,
                deriv_index: index,
                is_change: hint.is_change,
                must_select: true,
                sequence: None,
                ancestor_info: None,
            },
            transaction,
        )))
    }
}

fn ordered_exact_coins(
    transaction: &Transaction,
    coins: &[CandidateCoin],
) -> Result<Vec<CandidateCoin>, Error> {
    let by_outpoint: std::collections::BTreeMap<_, _> =
        coins.iter().map(|coin| (coin.outpoint, coin)).collect();
    if by_outpoint.len() != coins.len() || coins.len() != transaction.input.len() {
        return Err(Error::InvalidPlan);
    }
    transaction
        .input
        .iter()
        .map(|input| {
            by_outpoint
                .get(&input.previous_output)
                .map(|coin| **coin)
                .ok_or(Error::InvalidPlan)
        })
        .collect()
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
        (Some(stored), Poison::InputAncestry) if intent.version == 10 => {
            intent
                .funding_plan
                .as_ref()
                .ok_or(Error::InvalidPlan)?
                .validate(intent, stored)?;
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
            None,
        )
    }

    /// Persist both ordinary transactions and the two raw dependency paths
    /// atomically. This is an unchecked intent, never cached spend authority.
    /// The fork marker's derivation is checked against the same Vault descriptor
    /// that constructed both PSBTs before the record is written.
    #[allow(clippy::too_many_arguments)]
    pub fn create_ancestry_pair(
        directory: &Path,
        bitcoin_cube: String,
        fork_cube: String,
        bitcoin: &AncestrySelfTransfer,
        bitcoin_path: &RetainedPath,
        fork: &ClaimForkSweep,
        fork_path: &RetainedPath,
        fork_marker_derivation: (coincube_core::miniscript::bitcoin::bip32::ChildNumber, bool),
        bitcoin_feerate_vb: u64,
        fork_feerate_vb: u64,
        context: Context,
    ) -> Result<Self, Error> {
        bitcoin_path.reverify().map_err(|_| Error::InvalidPlan)?;
        fork_path.reverify().map_err(|_| Error::InvalidPlan)?;
        if !matches!(
            (bitcoin.chain(), fork.chain()),
            (ChainId::Bitcoin, ChainId::BitcoinBlake2b)
                | (ChainId::Testnet4, ChainId::BitcoinBlake2bTestnet4)
        ) || bitcoin_path.selected() != bitcoin.poison_input()
            || fork.fork_marker() != Some(fork_path.selected())
            || fork.bitcoin_step1() != bitcoin.psbt().unsigned_tx.compute_txid()
            || fork.descriptor() != bitcoin.descriptor()
            || fork_marker_derivation.0.is_hardened()
        {
            return Err(Error::WrongIdentity);
        }
        let raw = fork_path.links().first().ok_or(Error::InvalidPlan)?;
        let previous: Transaction =
            coincube_core::miniscript::bitcoin::consensus::deserialize(&raw.transaction)
                .map_err(|_| Error::InvalidPlan)?;
        let output = previous
            .output
            .get(fork_path.selected().vout as usize)
            .ok_or(Error::InvalidPlan)?;
        let descriptor = if fork_marker_derivation.1 {
            bitcoin.descriptor().change_descriptor()
        } else {
            bitcoin.descriptor().receive_descriptor()
        };
        if descriptor
            .derive(
                fork_marker_derivation.0,
                &coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only(),
            )
            .script_pubkey()
            != output.script_pubkey
        {
            return Err(Error::WrongIdentity);
        }
        let identity = WalletIdentity {
            bitcoin_cube,
            fork_cube,
            descriptor_digest: sha256::Hash::hash(bitcoin.descriptor().to_string().as_bytes()),
        };
        Self::create_intent_with_ancestry(
            directory,
            identity,
            ClaimPlan {
                bitcoin_chain: bitcoin.chain(),
                fork_chain: fork.chain(),
                step1: bitcoin.psbt().unsigned_tx.clone(),
                claimed_prevouts: bitcoin.claimed_prevouts().to_vec(),
                poison: Poison::InputAncestry,
                previous_confirmation: None,
                tracked_txid: None,
            },
            context,
            Some(u32::from(bitcoin.change_index())),
            Some(StoredAncestry {
                selected: bitcoin_path.selected(),
                raw: hex::encode(bitcoin_path.encode()),
                derivation: Some(StoredDerivation {
                    index: u32::from(bitcoin.poison_derivation().0),
                    is_change: bitcoin.poison_derivation().1,
                }),
            }),
            Some(StoredFundingPlan {
                fork: StoredAncestry {
                    selected: fork_path.selected(),
                    raw: hex::encode(fork_path.encode()),
                    derivation: Some(StoredDerivation {
                        index: u32::from(fork_marker_derivation.0),
                        is_change: fork_marker_derivation.1,
                    }),
                },
                step2_unsigned: fork.psbt().unsigned_tx.clone(),
                bitcoin_feerate_vb,
                fork_feerate_vb,
                fork_change_index: u32::from(fork.change_index()),
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
        self.intent
            .ancestry
            .as_ref()
            .map(|stored| stored.owned_input(descriptor))
            .transpose()
            .map(Option::flatten)
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
    /// Rebuild both version-10 transactions from current owned coins and the
    /// two retained paths. This never restores signing or submission rights:
    /// fresh two-chain proof and reservation must follow the reconstruction.
    #[allow(clippy::too_many_arguments)]
    pub fn restore_ancestry_pair(
        &mut self,
        current: &Context,
        descriptor: &CoincubeDescriptor,
        bitcoin_getter: &mut impl TxGetter,
        bitcoin_coins: &[CandidateCoin],
        fork_getter: &mut impl TxGetter,
        fork_coins: &[CandidateCoin],
    ) -> Result<(AncestrySelfTransfer, ClaimForkSweep), Error> {
        self.ensure_context(current)?;
        self.clear_check();
        self.construction_verified = false;
        if self.intent.version != 10 || self.intent.phase != Phase::Intent {
            return Err(Error::Unchecked);
        }
        if sha256::Hash::hash(descriptor.to_string().as_bytes())
            != self.intent.identity.descriptor_digest
        {
            return Err(Error::WrongIdentity);
        }
        let bitcoin_stored = self.intent.ancestry.as_ref().ok_or(Error::InvalidJournal)?;
        let funding = self
            .intent
            .funding_plan
            .as_ref()
            .ok_or(Error::InvalidJournal)?;
        let bitcoin_path = bitcoin_stored.decode()?;
        let fork_path = funding.fork.decode()?;
        let bitcoin_dependency = bitcoin_path.reverify().map_err(|_| Error::InvalidJournal)?;
        let fork_dependency = fork_path.reverify().map_err(|_| Error::InvalidJournal)?;
        let bitcoin_marker = bitcoin_stored
            .owned_input(descriptor)?
            .ok_or(Error::InvalidJournal)?
            .0;
        let fork_marker = funding
            .fork
            .owned_input(descriptor)?
            .ok_or(Error::InvalidJournal)?
            .0;
        let bitcoin_ordered = ordered_exact_coins(&self.intent.plan.step1, bitcoin_coins)?;
        let fork_ordered = ordered_exact_coins(&funding.step2_unsigned, fork_coins)?;
        if !bitcoin_ordered.iter().any(|coin| {
            coin.outpoint == bitcoin_marker.outpoint
                && coin.amount == bitcoin_marker.amount
                && coin.deriv_index == bitcoin_marker.deriv_index
                && coin.is_change == bitcoin_marker.is_change
        }) || !fork_ordered.iter().any(|coin| {
            coin.outpoint == fork_marker.outpoint
                && coin.amount == fork_marker.amount
                && coin.deriv_index == fork_marker.deriv_index
                && coin.is_change == fork_marker.is_change
        }) {
            return Err(Error::InvalidPlan);
        }
        let bitcoin_index =
            coincube_core::miniscript::bitcoin::bip32::ChildNumber::from_normal_idx(
                self.intent
                    .bitcoin_change_index
                    .ok_or(Error::InvalidJournal)?,
            )
            .map_err(|_| Error::InvalidJournal)?;
        let fork_index = coincube_core::miniscript::bitcoin::bip32::ChildNumber::from_normal_idx(
            funding.fork_change_index,
        )
        .map_err(|_| Error::InvalidJournal)?;
        let secp = coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only();
        let bitcoin = reconstruct_ancestry_self_transfer(
            self.intent.plan.bitcoin_chain,
            descriptor,
            &secp,
            bitcoin_getter,
            &bitcoin_ordered,
            bitcoin_index,
            &bitcoin_dependency,
            &self.intent.plan.step1,
        )
        .map_err(|_| Error::InvalidPlan)?;
        if bitcoin.poison_derivation() != (bitcoin_marker.deriv_index, bitcoin_marker.is_change)
            || bitcoin.claimed_prevouts() != self.intent.plan.claimed_prevouts
        {
            return Err(Error::InvalidPlan);
        }
        let fork = reconstruct_ancestry_fork_sweep_with_marker(
            &bitcoin,
            self.intent.plan.fork_chain,
            &secp,
            fork_getter,
            &fork_ordered,
            fork_index,
            &fork_dependency,
            &funding.step2_unsigned,
        )
        .map_err(|_| Error::InvalidPlan)?;
        if fork.fork_marker() != Some(fork_marker.outpoint)
            || fork.bitcoin_step1() != bitcoin.psbt().unsigned_tx.compute_txid()
        {
            return Err(Error::InvalidPlan);
        }
        Ok((bitcoin, fork))
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
        if self.intent.version == 10 {
            return Err(Error::Unchecked);
        }
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
        if self.intent.version == 10 {
            return Err(Error::Unchecked);
        }
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
