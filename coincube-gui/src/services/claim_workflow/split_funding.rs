//! Immutable pre-deposit address reservation for a foreign-wallet Split.
//! This record is deliberately separate from an ancestry transaction intent:
//! users need the deposit addresses before either funding coin exists.
//! Loading it does not grant signing or broadcasting authority.

use super::paired_signing_permit::PairedSigningPermit;
use super::{journal::Journal, Context, Controller, Error};
use crate::app::settings::WalletId;
use crate::services::{
    claim_observation::{
        http::{
            CollectedPostBitcoinInputs, DiscoveryError, FundingContext, HttpObservationSource,
            OwnedSplitFunding, SplitPairError,
        },
        FailureKind, ObservationSource,
    },
    foreign_psbt::TargetAddressEvidence,
    foreign_split_inventory::{
        FreshFundingAddresses, InventoryError, SplitInventory, TwoChainScan,
    },
};
use coincube_core::{
    chain::ChainId,
    claim::{Policy, PreflightTips},
    claim_ancestry::retained::RetainedPath,
    descriptors::CoincubeDescriptor,
    foreign_split::{
        finalize_ancestry_split_step1, finalize_ancestry_split_step2,
        validate_ancestry_split_pair_signing, AncestrySplitInputs, AncestrySplitStep1,
        AncestrySplitStep2, AncestrySplitStep2Inputs, FinalizeError, SplitCoin, SplitSource,
        VerifiedAncestrySplitPair,
    },
    miniscript::{
        bitcoin::{
            bip32::ChildNumber,
            hashes::{sha256, Hash},
            psbt::Psbt,
            Address, Network, OutPoint, ScriptBuf, Transaction,
        },
        Descriptor, DescriptorPublicKey,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Debug)]
pub enum SplitFundingDiscoveryError {
    InvalidReservation(Error),
    Changed,
    Discovery(DiscoveryError),
}

/// Read-only evidence for the exact reviewed pair. Signer dispatch must
/// re-run this against a freshly completed two-chain source scan and apply
/// its own wallet, session and preflight checks before exposing either PSBT.
#[derive(Debug)]
pub enum SplitPairRequalificationError {
    Discovery(SplitFundingDiscoveryError),
    Construction(SplitPairError),
    Changed,
}

#[derive(Debug)]
pub enum SplitPostBitcoinError {
    Changed,
    Inventory(InventoryError),
    Observation(FailureKind),
}

/// Fresh source scan, target binding, saved-address reservation and two-chain
/// requalification for one exact ordinary Split pair. It grants no signer or
/// broadcast access by itself.
#[derive(Debug)]
#[allow(dead_code)] // The production Split signer transition is still gated.
pub(crate) struct RequalifiedInitialSplit {
    pub(super) funding: OwnedSplitFunding,
    target_descriptor: CoincubeDescriptor,
    bitcoin_psbt: Psbt,
    fork_psbt: Psbt,
    shared: Vec<SplitCoin>,
    context: Context,
    tips: PreflightTips,
    checked_at: Instant,
}

#[allow(dead_code)] // Used by the staged paired Split signer transition.
impl RequalifiedInitialSplit {
    pub(crate) fn funding(&self) -> &OwnedSplitFunding {
        &self.funding
    }

    pub(crate) fn context(&self) -> &Context {
        &self.context
    }

    pub(crate) fn tips(&self) -> PreflightTips {
        self.tips
    }

    pub(super) fn matches_pair(
        &self,
        target: &CoincubeDescriptor,
        bitcoin: &AncestrySplitStep1,
        fork: &AncestrySplitStep2,
    ) -> bool {
        self.target_descriptor == *target
            && self.bitcoin_psbt == *bitcoin.psbt()
            && self.fork_psbt == *fork.psbt()
    }

    /// Admit one signer operation after the exact source wallet, two funding
    /// coins and current chain tips have been checked again. This does not
    /// admit a signed return or grant submission authority.
    pub(crate) fn signer_permit(
        &self,
        chain: ChainId,
        bitcoin: &AncestrySplitStep1,
        fork: &AncestrySplitStep2,
        current: &Psbt,
        generation: watch::Receiver<u64>,
    ) -> Option<PairedSigningPermit> {
        if !self.matches_pair(&self.target_descriptor, bitcoin, fork)
            || self.checked_at.elapsed() >= Duration::from_secs(10)
            || generation.has_changed().is_err()
            || *generation.borrow() != self.context.generation
        {
            return None;
        }
        let bitcoin_dependency = self.funding.bitcoin_path.reverify().ok()?;
        let fork_dependency = self.funding.fork_path.reverify().ok()?;
        let bitcoin_inputs = AncestrySplitInputs {
            chain: bitcoin.chain(),
            source: bitcoin.source(),
            shared: &self.shared,
            funding: &self.funding.bitcoin,
            dependency: &bitcoin_dependency,
            fork_height: bitcoin.fork_height(),
            destination: bitcoin.destination(),
        };
        let fork_inputs = AncestrySplitStep2Inputs {
            chain: fork.chain(),
            source: fork.source(),
            shared: &self.shared,
            funding: &self.funding.fork,
            dependency: &fork_dependency,
            fork_height: fork.fork_height(),
            claimed: bitcoin.claimed_prevouts(),
            target: fork
                .psbt()
                .unsigned_tx
                .output
                .first()?
                .script_pubkey
                .as_script(),
        };
        let bitcoin_signed = if chain == ChainId::Bitcoin {
            current
        } else {
            bitcoin.psbt()
        };
        let fork_signed = if chain == ChainId::BitcoinBlake2b {
            current
        } else {
            fork.psbt()
        };
        validate_ancestry_split_pair_signing(
            bitcoin,
            &bitcoin_inputs,
            bitcoin_signed,
            fork,
            &fork_inputs,
            fork_signed,
        )
        .ok()?;
        let secp = coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only();
        let valid = match chain {
            ChainId::Bitcoin => matches!(
                finalize_ancestry_split_step1(bitcoin, &bitcoin_inputs, current, &secp),
                Ok(_) | Err(FinalizeError::Unsatisfied)
            ),
            ChainId::BitcoinBlake2b => matches!(
                finalize_ancestry_split_step2(fork, &fork_inputs, current, &secp),
                Ok(_) | Err(FinalizeError::Unsatisfied)
            ),
            _ => false,
        };
        valid.then(|| PairedSigningPermit::new(chain, current, generation, self.context.generation))
    }

    /// Recheck two returned signer PSBTs against this fresh pair, finalize
    /// every input, and bind the exact witnesses to the saved version-11
    /// journal. This does not write a possible send or enter a transport gate.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finalize_signed_pair_for_record(
        &self,
        controller: &mut Controller,
        target: &CoincubeDescriptor,
        bitcoin: &AncestrySplitStep1,
        bitcoin_signed: &Psbt,
        fork: &AncestrySplitStep2,
        fork_signed: &Psbt,
        policy: Policy,
        now: i64,
    ) -> Result<VerifiedAncestrySplitPair, Error> {
        if !self.matches_pair(target, bitcoin, fork)
            || self.checked_at.elapsed() >= Duration::from_secs(10)
        {
            return Err(Error::WrongIdentity);
        }
        let combined = self.combine_signer_returns(
            target,
            bitcoin,
            std::slice::from_ref(bitcoin_signed),
            fork,
            std::slice::from_ref(fork_signed),
        )?;
        if !combined.complete {
            return Err(Error::Unchecked);
        }
        let bitcoin_dependency = self
            .funding
            .bitcoin_path
            .reverify()
            .map_err(|_| Error::Unchecked)?;
        let fork_dependency = self
            .funding
            .fork_path
            .reverify()
            .map_err(|_| Error::Unchecked)?;
        let bitcoin_inputs = AncestrySplitInputs {
            chain: bitcoin.chain(),
            source: bitcoin.source(),
            shared: &self.shared,
            funding: &self.funding.bitcoin,
            dependency: &bitcoin_dependency,
            fork_height: bitcoin.fork_height(),
            destination: bitcoin.destination(),
        };
        let fork_inputs = AncestrySplitStep2Inputs {
            chain: fork.chain(),
            source: fork.source(),
            shared: &self.shared,
            funding: &self.funding.fork,
            dependency: &fork_dependency,
            fork_height: fork.fork_height(),
            claimed: bitcoin.claimed_prevouts(),
            target: fork
                .psbt()
                .unsigned_tx
                .output
                .first()
                .ok_or(Error::InvalidPlan)?
                .script_pubkey
                .as_script(),
        };
        let (verified, _) = controller.finalize_ancestry_split_pair_for_review(
            &self.context,
            self,
            target,
            bitcoin,
            &bitcoin_inputs,
            &combined.bitcoin,
            fork,
            &fork_inputs,
            &combined.fork,
            policy,
            now,
            self.tips,
        )?;
        Ok(verified)
    }

    /// Reauthenticate both journaled witnesses for a new submission review.
    /// This repeats the complete source, reservation and two-chain check used
    /// for signing, then checks the saved raw signatures against the rebuilt
    /// PSBTs. The result is evidence only; policy, approval and a durable
    /// possible-attempt record are still required before transport.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)] // The paired submission review will consume this boundary.
    pub(crate) fn reverify_recorded_pair_for_review(
        &self,
        controller: &mut Controller,
        target: &CoincubeDescriptor,
        bitcoin: &AncestrySplitStep1,
        fork: &AncestrySplitStep2,
        policy: Policy,
        now: i64,
    ) -> Result<
        (
            VerifiedAncestrySplitPair,
            super::paired_submission::InitialAncestryProof,
        ),
        Error,
    > {
        if !self.matches_pair(target, bitcoin, fork)
            || self.checked_at.elapsed() >= Duration::from_secs(10)
        {
            return Err(Error::WrongIdentity);
        }
        let bitcoin_dependency = self
            .funding
            .bitcoin_path
            .reverify()
            .map_err(|_| Error::Unchecked)?;
        let fork_dependency = self
            .funding
            .fork_path
            .reverify()
            .map_err(|_| Error::Unchecked)?;
        let bitcoin_inputs = AncestrySplitInputs {
            chain: bitcoin.chain(),
            source: bitcoin.source(),
            shared: &self.shared,
            funding: &self.funding.bitcoin,
            dependency: &bitcoin_dependency,
            fork_height: bitcoin.fork_height(),
            destination: bitcoin.destination(),
        };
        let fork_inputs = AncestrySplitStep2Inputs {
            chain: fork.chain(),
            source: fork.source(),
            shared: &self.shared,
            funding: &self.funding.fork,
            dependency: &fork_dependency,
            fork_height: fork.fork_height(),
            claimed: bitcoin.claimed_prevouts(),
            target: fork
                .psbt()
                .unsigned_tx
                .output
                .first()
                .ok_or(Error::InvalidPlan)?
                .script_pubkey
                .as_script(),
        };
        controller.validate_ancestry_split_observation(
            &self.context,
            &self.funding.inputs,
            target,
            &self.funding.bitcoin,
            &self.funding.fork,
            bitcoin,
            fork,
            policy,
            now,
            self.tips,
        )?;
        let verified = controller.verify_recorded_ancestry_split_pair(
            &self.context,
            bitcoin,
            &bitcoin_inputs,
            fork,
            &fork_inputs,
        )?;
        let proof = super::paired_submission::InitialAncestryProof::after_finalization(
            controller,
            &self.context,
            verified.bitcoin().transaction(),
            verified.fork().transaction(),
            self.tips,
            self.funding.inputs.observed_at(),
            policy.max_observation_age_seconds,
        );
        Ok((verified, proof))
    }

    /// Authenticate independent file or device returns under the current
    /// two-chain source scan. This result is only a signature aggregate;
    /// final recording repeats the live scan and journal check.
    pub(crate) fn combine_signer_returns(
        &self,
        target: &CoincubeDescriptor,
        bitcoin: &AncestrySplitStep1,
        bitcoin_returns: &[Psbt],
        fork: &AncestrySplitStep2,
        fork_returns: &[Psbt],
    ) -> Result<crate::services::split_psbt_file::CombinedAncestrySplit, Error> {
        if !self.matches_pair(target, bitcoin, fork)
            || self.checked_at.elapsed() >= Duration::from_secs(10)
        {
            return Err(Error::WrongIdentity);
        }
        let bitcoin_dependency = self
            .funding
            .bitcoin_path
            .reverify()
            .map_err(|_| Error::Unchecked)?;
        let fork_dependency = self
            .funding
            .fork_path
            .reverify()
            .map_err(|_| Error::Unchecked)?;
        let bitcoin_inputs = AncestrySplitInputs {
            chain: bitcoin.chain(),
            source: bitcoin.source(),
            shared: &self.shared,
            funding: &self.funding.bitcoin,
            dependency: &bitcoin_dependency,
            fork_height: bitcoin.fork_height(),
            destination: bitcoin.destination(),
        };
        let fork_inputs = AncestrySplitStep2Inputs {
            chain: fork.chain(),
            source: fork.source(),
            shared: &self.shared,
            funding: &self.funding.fork,
            dependency: &fork_dependency,
            fork_height: fork.fork_height(),
            claimed: bitcoin.claimed_prevouts(),
            target: fork
                .psbt()
                .unsigned_tx
                .output
                .first()
                .ok_or(Error::InvalidPlan)?
                .script_pubkey
                .as_script(),
        };
        crate::services::split_psbt_file::combine_ancestry_split_pair(
            bitcoin,
            &bitcoin_inputs,
            bitcoin_returns,
            fork,
            &fork_inputs,
            fork_returns,
        )
        .map_err(|_| Error::InvalidPlan)
    }
}

/// Only the saved foreign source/target reservation and fresh stage-two
/// requalification below can create this value. It remains read-only.
#[allow(dead_code)] // The paired stage-two coordinator is not wired yet.
pub struct RequalifiedPostBitcoinSplit {
    pub(super) observed: CollectedPostBitcoinInputs,
}

/// The destination Vault address already authenticated and reserved when
/// the user accepted the Split handoff. This is an immutable destination
/// binding, not a fresh unused-address proof at signing time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitTargetBinding {
    vault_id: WalletId,
    descriptor: String,
    receive_index: u32,
    script_hex: String,
}

impl SplitTargetBinding {
    pub fn from_evidence(
        target: &TargetAddressEvidence,
        descriptor: &CoincubeDescriptor,
    ) -> Result<Self, Error> {
        let index = target.derivation_index();
        if index.is_hardened() {
            return Err(Error::InvalidPlan);
        }
        let binding = Self {
            vault_id: target.vault_id().clone(),
            descriptor: descriptor.to_string(),
            receive_index: u32::from(index),
            script_hex: hex::encode(target.script_pubkey().as_bytes()),
        };
        binding.validate()?;
        if binding.script()?.as_script() != target.script_pubkey() {
            return Err(Error::InvalidPlan);
        }
        Ok(binding)
    }

    fn validate(&self) -> Result<(), Error> {
        if self.descriptor.len() > 16_384
            || self.script_hex.len() > 20_000
            || self.receive_index >= 1 << 31
            || self.vault_id.descriptor_checksum.is_empty()
        {
            return Err(Error::InvalidPlan);
        }
        if self
            .descriptor
            .split_once('#')
            .map(|(_, checksum)| checksum)
            != Some(self.vault_id.descriptor_checksum.as_str())
        {
            return Err(Error::InvalidPlan);
        }
        self.script().map(|_| ())
    }

    fn script(&self) -> Result<ScriptBuf, Error> {
        let descriptor =
            CoincubeDescriptor::from_str(&self.descriptor).map_err(|_| Error::InvalidPlan)?;
        let script = descriptor
            .receive_descriptor()
            .derive(
                ChildNumber::Normal {
                    index: self.receive_index,
                },
                &coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only(),
            )
            .script_pubkey();
        if hex::encode(script.as_bytes()) != self.script_hex {
            return Err(Error::InvalidPlan);
        }
        Ok(script)
    }

    /// A reopened funding plan must still belong to the loaded target Vault.
    pub fn matches_vault(&self, vault_id: &WalletId, descriptor: &CoincubeDescriptor) -> bool {
        self.vault_id == *vault_id
            && self.descriptor == descriptor.to_string()
            && self.validate().is_ok()
    }

    pub fn receive_index(&self) -> u32 {
        self.receive_index
    }

    pub fn script_pubkey(&self) -> Result<ScriptBuf, Error> {
        self.script()
    }

    pub fn matches_destination(
        &self,
        descriptor_digest: sha256::Hash,
        index: u32,
        script: &coincube_core::miniscript::bitcoin::Script,
    ) -> bool {
        sha256::Hash::hash(self.descriptor.as_bytes()) == descriptor_digest
            && self.receive_index == index
            && self.script().is_ok_and(|saved| saved.as_script() == script)
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        vault_id: WalletId,
        descriptor: &CoincubeDescriptor,
        index: u32,
    ) -> Self {
        let script = descriptor
            .receive_descriptor()
            .derive(
                ChildNumber::Normal { index },
                &coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only(),
            )
            .script_pubkey();
        Self {
            vault_id,
            descriptor: descriptor.to_string(),
            receive_index: index,
            script_hex: hex::encode(script.as_bytes()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitFundingReservation {
    version: u32,
    pub source_digest: sha256::Hash,
    pub target_cube: String,
    pub account: String,
    pub provider: String,
    external: String,
    internal: Option<String>,
    pub indices: FreshFundingAddresses,
    /// Present for new production splits. Older deposit-only records remain
    /// readable, but cannot create an ordinary pair without a bound target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_binding: Option<SplitTargetBinding>,
    /// The initial unused-history check passed before these addresses were
    /// shown. Later deposits make history nonempty, so restart must reuse the
    /// exact saved addresses rather than applying that first-use check again.
    #[serde(default)]
    released: bool,
}

impl SplitFundingReservation {
    fn new(
        source: &SplitSource,
        target_cube: &str,
        context: &Context,
        indices: FreshFundingAddresses,
    ) -> Result<Self, Error> {
        let record = Self {
            version: 2,
            source_digest: source.digest(),
            target_cube: target_cube.to_owned(),
            account: context.account.clone(),
            provider: context.provider.clone(),
            external: source.external().to_string(),
            internal: source.internal().map(ToString::to_string),
            indices,
            target_binding: None,
            released: false,
        };
        record.validate()?;
        Ok(record)
    }

    pub(super) fn validate(&self) -> Result<(), Error> {
        if !matches!(self.version, 1..=3)
            || (self.version == 1 && self.released)
            || (self.version < 3 && self.target_binding.is_some())
            || (self.version == 3 && self.target_binding.is_none())
            || self.target_cube.is_empty()
            || self.account.is_empty()
            || self.provider.is_empty()
            || self.target_cube.len() > 256
            || self.account.len() > 256
            || self.provider.len() > 2048
            || self.external.len() > 16_384
            || self
                .internal
                .as_ref()
                .is_some_and(|text| text.len() > 16_384)
        {
            return Err(Error::InvalidPlan);
        }
        let [bitcoin, fork, returning] = [
            self.indices.bitcoin_deposit,
            self.indices.btcb2_deposit,
            self.indices.bitcoin_return,
        ];
        if [bitcoin, fork, returning]
            .iter()
            .any(|index| *index >= 1 << 31)
            || bitcoin == fork
            || bitcoin == returning
            || fork == returning
        {
            return Err(Error::InvalidPlan);
        }
        let source = self.source()?;
        if let Some(target) = &self.target_binding {
            target.validate()?;
        }
        if !source.external().has_wildcard() || source.digest() != self.source_digest {
            return Err(Error::InvalidPlan);
        }
        for index in [bitcoin, fork, returning] {
            self.address(index)?;
        }
        Ok(())
    }

    pub fn source(&self) -> Result<SplitSource, Error> {
        let external = Descriptor::<DescriptorPublicKey>::from_str(&self.external)
            .map_err(|_| Error::InvalidPlan)?;
        let internal = self
            .internal
            .as_deref()
            .map(Descriptor::<DescriptorPublicKey>::from_str)
            .transpose()
            .map_err(|_| Error::InvalidPlan)?;
        SplitSource::new(external, internal).map_err(|_| Error::InvalidPlan)
    }

    fn address(&self, index: u32) -> Result<String, Error> {
        let script = self
            .source()?
            .external()
            .at_derivation_index(index)
            .map_err(|_| Error::InvalidPlan)?
            .script_pubkey();
        Address::from_script(&script, Network::Bitcoin)
            .map(|address| address.to_string())
            .map_err(|_| Error::InvalidPlan)
    }

    /// These strings are safe to show only after fresh, authenticated checks
    /// prove all three addresses unused on both chains under the current
    /// Connect account. The reservation itself is never that proof.
    pub fn addresses(&self) -> Result<[String; 3], Error> {
        Ok([
            self.address(self.indices.bitcoin_deposit)?,
            self.address(self.indices.btcb2_deposit)?,
            self.address(self.indices.bitcoin_return)?,
        ])
    }

    pub fn is_released(&self) -> bool {
        self.released
    }

    pub(super) fn released_copy(&self) -> Self {
        let mut copy = self.clone();
        copy.version = if copy.target_binding.is_some() { 3 } else { 2 };
        copy.released = true;
        copy
    }

    pub fn matches(&self, source: &SplitSource, target_cube: &str, context: &Context) -> bool {
        self.source_digest == source.digest()
            && self.target_cube == target_cube
            && self.account == context.account
            && self.provider == context.provider
            && self.source().is_ok_and(|recorded| recorded == *source)
    }

    fn checked_reserved_inventory(
        &self,
        directory: &Path,
        source: &SplitSource,
        context: &Context,
        inventory: &SplitInventory,
        minimums: (u64, u64),
    ) -> Result<SplitInventory, SplitFundingDiscoveryError> {
        if !self.is_released() {
            return Err(SplitFundingDiscoveryError::Changed);
        }
        let saved = load_split_funding(directory, source, &self.target_cube, context)
            .map_err(SplitFundingDiscoveryError::InvalidReservation)?
            .ok_or(SplitFundingDiscoveryError::Changed)?;
        if saved != *self || !saved.matches(source, &self.target_cube, context) {
            return Err(SplitFundingDiscoveryError::Changed);
        }
        Ok(inventory
            .funding_at_reserved_addresses(self.indices)
            .funding_covering_estimated_fees(minimums.0, minimums.1))
    }

    /// Reload the exact pre-deposit selection and search only its two receive
    /// indices under one live observation context. This is read-only evidence,
    /// not a source-wallet reconciliation or signer capability.
    #[allow(clippy::too_many_arguments)] // Keep the saved selection and live evidence explicit.
    pub async fn discover_reserved_funding(
        &self,
        directory: PathBuf,
        source: &SplitSource,
        context: &Context,
        inventory: &SplitInventory,
        observations: &HttpObservationSource,
        policy: Policy,
        minimums: (u64, u64),
    ) -> Result<Option<OwnedSplitFunding>, SplitFundingDiscoveryError> {
        self.discover_reserved_funding_at_outpoints(
            directory,
            source,
            context,
            inventory,
            observations,
            policy,
            minimums,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn discover_reserved_funding_at_outpoints(
        &self,
        directory: PathBuf,
        source: &SplitSource,
        context: &Context,
        inventory: &SplitInventory,
        observations: &HttpObservationSource,
        policy: Policy,
        minimums: (u64, u64),
        selected: Option<(OutPoint, OutPoint)>,
    ) -> Result<Option<OwnedSplitFunding>, SplitFundingDiscoveryError> {
        observations
            .validate_context(&context.provider, context.generation)
            .map_err(|_| SplitFundingDiscoveryError::Changed)?;
        // A foreign wallet scan has its own generation, independent of the
        // Connect account generation used by the observation source.
        let reservation = self.clone();
        let source_for_load = source.clone();
        let context_for_load = context.clone();
        let inventory_for_load = inventory.clone();
        let mut reserved = tokio::task::spawn_blocking(move || {
            reservation.checked_reserved_inventory(
                &directory,
                &source_for_load,
                &context_for_load,
                &inventory_for_load,
                minimums,
            )
        })
        .await
        .map_err(|_| SplitFundingDiscoveryError::Changed)??;
        if let Some((bitcoin, fork)) = selected {
            reserved = reserved.funding_at_outpoints(bitcoin, fork);
        }
        if reserved
            .funding_candidates(coincube_core::chain::ChainId::Bitcoin)
            .is_empty()
            || reserved
                .funding_candidates(coincube_core::chain::ChainId::BitcoinBlake2b)
                .is_empty()
        {
            return Ok(None);
        }
        let found = observations
            .discover_owned_split_funding(&reserved, source, policy)
            .await
            .map_err(SplitFundingDiscoveryError::Discovery)?;
        observations
            .validate_context(&context.provider, context.generation)
            .map_err(|_| SplitFundingDiscoveryError::Changed)?;
        Ok(found)
    }

    /// Recollect only the exact two selected deposits, reconstruct both
    /// ordinary transactions and compare complete PSBTs. The caller must
    /// supply a newly completed two-chain source scan, not a saved preview.
    /// This is a prerequisite for signer admission, not an authorization.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn requalify_exact_pair(
        &self,
        directory: PathBuf,
        source: &SplitSource,
        target_cube: &str,
        target_vault: (&WalletId, &CoincubeDescriptor),
        context: &Context,
        inventory: &SplitInventory,
        observations: &HttpObservationSource,
        policy: Policy,
        minimums: (u64, u64),
        bitcoin: &AncestrySplitStep1,
        fork: &AncestrySplitStep2,
        rates: (u64, u64),
    ) -> Result<RequalifiedInitialSplit, SplitPairRequalificationError> {
        observations
            .validate_context(&context.provider, context.generation)
            .map_err(|_| SplitPairRequalificationError::Changed)?;
        let binding = self
            .target_binding
            .as_ref()
            .filter(|binding| binding.matches_vault(target_vault.0, target_vault.1))
            .ok_or(SplitPairRequalificationError::Changed)?;
        let target = binding
            .script_pubkey()
            .map_err(|_| SplitPairRequalificationError::Changed)?;
        if !self.released
            || !self.matches(source, target_cube, context)
            || bitcoin.source() != source
            || fork.source() != source
            || bitcoin.destination() != self.indices.bitcoin_return
            || bitcoin.funding_derivation()
                != Some((
                    coincube_core::foreign_split::SplitBranch::External,
                    self.indices.bitcoin_deposit,
                ))
            || fork.funding_derivation()
                != Some((
                    coincube_core::foreign_split::SplitBranch::External,
                    self.indices.btcb2_deposit,
                ))
            || fork.target() != target.as_script()
        {
            return Err(SplitPairRequalificationError::Changed);
        }
        let selected = (bitcoin.funding_prevout(), fork.funding_prevout());
        let funding = self
            .discover_reserved_funding_at_outpoints(
                directory.clone(),
                source,
                context,
                inventory,
                observations,
                policy,
                minimums,
                Some(selected),
            )
            .await
            .map_err(SplitPairRequalificationError::Discovery)?
            .ok_or(SplitPairRequalificationError::Changed)?;
        let tips = PreflightTips {
            bitcoin: funding
                .inputs
                .funding()
                .bitcoin()
                .ancestry()
                .pair()
                .bitcoin()
                .tip,
            fork: funding.inputs.funding().fork().ancestry().pair().fork().tip,
        };
        let review = FundingContext {
            provider: &context.provider,
            generation: context.generation,
            now: observations.now(),
            max_age_seconds: policy.max_observation_age_seconds,
            tips,
        };
        let (rebuilt_bitcoin, rebuilt_fork) = funding
            .construct_split_pair(
                inventory,
                source,
                self.indices.bitcoin_return,
                target.as_script(),
                rates.0,
                rates.1,
                review,
            )
            .map_err(SplitPairRequalificationError::Construction)?;
        if rebuilt_bitcoin.psbt() != bitcoin.psbt() || rebuilt_fork.psbt() != fork.psbt() {
            return Err(SplitPairRequalificationError::Changed);
        }
        let (bitcoin_tip, fork_tip) = tokio::try_join!(
            observations.tip(ChainId::Bitcoin),
            observations.tip(ChainId::BitcoinBlake2b),
        )
        .map_err(|_| SplitPairRequalificationError::Changed)?;
        let age = observations.now().checked_sub(funding.inputs.observed_at());
        if bitcoin_tip.value() != &tips.bitcoin
            || fork_tip.value() != &tips.fork
            || !age.is_some_and(|age| (0..=policy.max_observation_age_seconds).contains(&age))
        {
            return Err(SplitPairRequalificationError::Changed);
        }
        let saved_source = source.clone();
        let saved_target = self.target_cube.clone();
        let saved_context = context.clone();
        let current = tokio::task::spawn_blocking(move || {
            load_split_funding(&directory, &saved_source, &saved_target, &saved_context)
        })
        .await
        .map_err(|_| SplitPairRequalificationError::Changed)?
        .map_err(|_| SplitPairRequalificationError::Changed)?;
        if current.as_ref() != Some(self) {
            return Err(SplitPairRequalificationError::Changed);
        }
        observations
            .validate_context(&context.provider, context.generation)
            .map_err(|_| SplitPairRequalificationError::Changed)?;
        Ok(RequalifiedInitialSplit {
            funding,
            target_descriptor: target_vault.1.clone(),
            bitcoin_psbt: bitcoin.psbt().clone(),
            fork_psbt: fork.psbt().clone(),
            shared: inventory.splittable_coins(),
            context: context.clone(),
            tips,
            checked_at: Instant::now(),
        })
    }

    /// Recheck the saved foreign source and target after Bitcoin has spent
    /// the shared coins. The initial all-unspent collection cannot be reused:
    /// only the BTCB2 funding coin and shared coins remain unspent there.
    /// This returns fresh read-only observations, not signer or send authority.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)] // The stage-two Split coordinator is not wired yet.
    pub async fn requalify_post_bitcoin_fork(
        &self,
        directory: PathBuf,
        source: &SplitSource,
        target_cube: &str,
        target_vault: (&WalletId, &CoincubeDescriptor),
        context: &Context,
        scan: &TwoChainScan,
        observations: &HttpObservationSource,
        policy: Policy,
        bitcoin: &AncestrySplitStep1,
        fork: &AncestrySplitStep2,
        signed_bitcoin: &Transaction,
        bitcoin_path: &RetainedPath,
        fork_path: &RetainedPath,
    ) -> Result<RequalifiedPostBitcoinSplit, SplitPostBitcoinError> {
        observations
            .validate_context(&context.provider, context.generation)
            .map_err(|_| SplitPostBitcoinError::Changed)?;
        let target = self
            .target_binding
            .as_ref()
            .filter(|binding| binding.matches_vault(target_vault.0, target_vault.1))
            .ok_or(SplitPostBitcoinError::Changed)?
            .script_pubkey()
            .map_err(|_| SplitPostBitcoinError::Changed)?;
        let mut unsigned_bitcoin = signed_bitcoin.clone();
        for input in &mut unsigned_bitcoin.input {
            input.script_sig = ScriptBuf::new();
            input.witness.clear();
        }
        if !self.released
            || !self.matches(source, target_cube, context)
            || bitcoin.chain() != ChainId::Bitcoin
            || fork.chain() != ChainId::BitcoinBlake2b
            || bitcoin.source() != source
            || fork.source() != source
            || bitcoin.destination() != self.indices.bitcoin_return
            || bitcoin.funding_prevout() != bitcoin_path.selected()
            || fork.funding_prevout() != fork_path.selected()
            || bitcoin.funding_derivation()
                != Some((
                    coincube_core::foreign_split::SplitBranch::External,
                    self.indices.bitcoin_deposit,
                ))
            || fork.funding_derivation()
                != Some((
                    coincube_core::foreign_split::SplitBranch::External,
                    self.indices.btcb2_deposit,
                ))
            || bitcoin.claimed_prevouts() != fork.claimed_prevouts()
            || fork.target() != target.as_script()
            || unsigned_bitcoin != bitcoin.psbt().unsigned_tx
        {
            return Err(SplitPostBitcoinError::Changed);
        }
        let saved_source = source.clone();
        let saved_target = target_cube.to_owned();
        let saved_context = context.clone();
        let saved_directory = directory.clone();
        let saved = tokio::task::spawn_blocking(move || {
            load_split_funding(
                &saved_directory,
                &saved_source,
                &saved_target,
                &saved_context,
            )
        })
        .await
        .map_err(|_| SplitPostBitcoinError::Changed)?
        .map_err(|_| SplitPostBitcoinError::Changed)?;
        if saved.as_ref() != Some(self) {
            return Err(SplitPostBitcoinError::Changed);
        }
        let collected = observations
            .collect_post_bitcoin_inputs(
                bitcoin_path,
                fork_path,
                bitcoin.claimed_prevouts(),
                signed_bitcoin,
                policy,
            )
            .await
            .map_err(SplitPostBitcoinError::Observation)?;
        let (bitcoin_tip, fork_tip) = tokio::try_join!(
            observations.tip(ChainId::Bitcoin),
            observations.tip(ChainId::BitcoinBlake2b),
        )
        .map_err(SplitPostBitcoinError::Observation)?;
        let tips = PreflightTips {
            bitcoin: *bitcoin_tip.value(),
            fork: *fork_tip.value(),
        };
        scan.reconcile_post_bitcoin_split(
            source,
            bitcoin.claimed_prevouts(),
            fork.funding_prevout(),
            self.indices.btcb2_deposit,
            context.generation,
            tips,
        )
        .map_err(SplitPostBitcoinError::Inventory)?;
        collected
            .validate_for_fork_psbt(
                signed_bitcoin,
                fork.psbt(),
                bitcoin.claimed_prevouts(),
                FundingContext {
                    provider: &context.provider,
                    generation: context.generation,
                    now: observations.now(),
                    max_age_seconds: policy.max_observation_age_seconds,
                    tips,
                },
            )
            .map_err(SplitPostBitcoinError::Observation)?;
        let saved_source = source.clone();
        let saved_target = target_cube.to_owned();
        let saved_context = context.clone();
        let current = tokio::task::spawn_blocking(move || {
            load_split_funding(&directory, &saved_source, &saved_target, &saved_context)
        })
        .await
        .map_err(|_| SplitPostBitcoinError::Changed)?
        .map_err(|_| SplitPostBitcoinError::Changed)?;
        if current.as_ref() != Some(self) {
            return Err(SplitPostBitcoinError::Changed);
        }
        observations
            .validate_context(&context.provider, context.generation)
            .map_err(|_| SplitPostBitcoinError::Changed)?;
        let (last_bitcoin, last_fork) = tokio::try_join!(
            observations.tip(ChainId::Bitcoin),
            observations.tip(ChainId::BitcoinBlake2b),
        )
        .map_err(SplitPostBitcoinError::Observation)?;
        if last_bitcoin.value() != &tips.bitcoin || last_fork.value() != &tips.fork {
            return Err(SplitPostBitcoinError::Changed);
        }
        collected
            .validate_for_fork_psbt(
                signed_bitcoin,
                fork.psbt(),
                bitcoin.claimed_prevouts(),
                FundingContext {
                    provider: &context.provider,
                    generation: context.generation,
                    now: observations.now(),
                    max_age_seconds: policy.max_observation_age_seconds,
                    tips,
                },
            )
            .map_err(SplitPostBitcoinError::Observation)?;
        Ok(RequalifiedPostBitcoinSplit {
            observed: collected,
        })
    }
}

/// Save the exact three addresses before showing deposit instructions. Once
/// saved, a new scan cannot choose new indices for this source/target pair.
/// The first display requires unused history on both chains and a durable
/// release; later displays reuse those same addresses after identity checks.
/// Funding proofs must still be recollected before a spend.
pub fn reserve_split_funding(
    directory: &Path,
    source: &SplitSource,
    target_cube: &str,
    context: &Context,
    inventory: &SplitInventory,
) -> Result<SplitFundingReservation, Error> {
    reserve_split_funding_inner(directory, source, target_cube, context, inventory, None)
}

/// Production reservation binds the already authenticated target Vault
/// address to the same durable record as the three source addresses.
pub fn reserve_split_funding_with_target(
    directory: &Path,
    source: &SplitSource,
    target_cube: &str,
    context: &Context,
    inventory: &SplitInventory,
    target: &SplitTargetBinding,
) -> Result<SplitFundingReservation, Error> {
    target.validate()?;
    reserve_split_funding_inner(
        directory,
        source,
        target_cube,
        context,
        inventory,
        Some(target),
    )
}

fn reserve_split_funding_inner(
    directory: &Path,
    source: &SplitSource,
    target_cube: &str,
    context: &Context,
    inventory: &SplitInventory,
    target: Option<&SplitTargetBinding>,
) -> Result<SplitFundingReservation, Error> {
    super::prepare_directory(directory)?;
    let mut journal = Journal::open(directory)?;
    if journal.load()?.is_some()
        || journal.load_claim_funding_reservation()?.is_some()
        || std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok()
    {
        return Err(Error::Conflict);
    }
    if let Some(saved) = journal.load_split_funding_reservation()? {
        return if saved.matches(source, target_cube, context)
            && target.is_none_or(|expected| saved.target_binding.as_ref() == Some(expected))
        {
            Ok(saved)
        } else {
            Err(Error::WrongIdentity)
        };
    }
    let indices = inventory
        .fresh_funding_addresses()
        .ok_or(Error::InvalidPlan)?;
    let mut requested = SplitFundingReservation::new(source, target_cube, context, indices)?;
    if let Some(target) = target {
        requested.version = 3;
        requested.target_binding = Some(target.clone());
        requested.validate()?;
    }
    journal.store_split_funding_reservation(&requested)?;
    Ok(requested)
}

/// Recover the immutable address selection after restart. The caller must
/// match the saved account/provider and source before making any address
/// visible. A released reservation may have nonempty deposit history.
pub fn load_split_funding(
    directory: &Path,
    source: &SplitSource,
    target_cube: &str,
    context: &Context,
) -> Result<Option<SplitFundingReservation>, Error> {
    let journal = Journal::open(directory)?;
    if journal.load_claim_funding_reservation()?.is_some()
        || std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok()
    {
        return Err(Error::Conflict);
    }
    let saved = journal.load_split_funding_reservation()?;
    match saved {
        Some(saved) if saved.matches(source, target_cube, context) => Ok(Some(saved)),
        Some(_) => Err(Error::WrongIdentity),
        None => Ok(None),
    }
}

/// Read a funding-only reservation under the journal lock before the
/// account-specific checks. Callers must still compare its target, source and
/// Connect identity before displaying an address or acting on a deposit.
pub fn peek_split_funding(directory: &Path) -> Result<Option<SplitFundingReservation>, Error> {
    let journal = Journal::open(directory)?;
    if journal.load()?.is_some()
        || journal.load_claim_funding_reservation()?.is_some()
        || std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok()
    {
        return Err(Error::Conflict);
    }
    journal.load_split_funding_reservation()
}

/// Record that the initial two-chain unused-address check succeeded. The
/// journal lock compares the exact reservation before atomically replacing it.
pub fn release_split_funding(
    directory: &Path,
    expected: &SplitFundingReservation,
) -> Result<SplitFundingReservation, Error> {
    let mut journal = Journal::open(directory)?;
    journal.release_split_funding_reservation(expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        chain::ChainId,
        services::foreign_scan::{Branch, BranchCoverage, ScanReport},
    };
    use coincube_core::miniscript::bitcoin::{
        bip32::{Xpriv, Xpub},
        hashes::Hash,
        secp256k1::Secp256k1,
        BlockHash,
    };
    use coincube_core::{
        bip39::Mnemonic,
        descriptors::{CoincubePolicy, PathInfo},
        miniscript::{bitcoin::bip32::DerivationPath, DescriptorPublicKey},
        signer::MasterSigner,
    };

    fn source() -> SplitSource {
        let secp = Secp256k1::new();
        let private = Xpriv::new_master(Network::Bitcoin, &[7; 64]).unwrap();
        let public = Xpub::from_priv(&secp, &private);
        let external = Descriptor::from_str(&format!("wpkh({public}/0/*)")).unwrap();
        let internal = Descriptor::from_str(&format!("wpkh({public}/1/*)")).unwrap();
        SplitSource::new(external, Some(internal)).unwrap()
    }

    fn context() -> Context {
        Context {
            generation: 1,
            account: "account-a".into(),
            provider: "https://connect.example".into(),
        }
    }

    fn indices() -> FreshFundingAddresses {
        FreshFundingAddresses {
            bitcoin_deposit: 4,
            btcb2_deposit: 5,
            bitcoin_return: 6,
        }
    }

    fn inventory(last_used: u32) -> SplitInventory {
        let report = |chain, byte| {
            ScanReport::for_test(chain, 1, BlockHash::from_byte_array([byte; 32]), Vec::new())
                .with_coverage(vec![BranchCoverage {
                    branch: Branch::External,
                    start: 0,
                    end_exclusive: 20,
                    last_used: Some(last_used),
                }])
        };
        SplitInventory::join(
            &report(ChainId::BitcoinBlake2b, 3).with_fork_height(Some(2)),
            &report(ChainId::Bitcoin, 4),
            1,
            true,
        )
        .unwrap()
    }

    fn target_binding() -> (SplitTargetBinding, WalletId, CoincubeDescriptor) {
        let secp = Secp256k1::new();
        let keys: Vec<_> = [81_u8, 82]
            .iter()
            .map(|byte| {
                let signer = MasterSigner::from_mnemonic(
                    Network::Bitcoin,
                    Mnemonic::from_entropy(&[*byte; 16]).unwrap(),
                )
                .unwrap();
                DescriptorPublicKey::from_str(&format!(
                    "[{}]{}/<0;1>/*",
                    signer.fingerprint(&secp),
                    signer.xpub_at(&DerivationPath::default(), &secp)
                ))
                .unwrap()
            })
            .collect();
        let descriptor = CoincubeDescriptor::new(
            CoincubePolicy::new_legacy(
                PathInfo::Single(keys[0].clone()),
                std::iter::once((46, PathInfo::Single(keys[1].clone()))).collect(),
            )
            .unwrap(),
        );
        let vault_id = WalletId::new(
            descriptor
                .to_string()
                .split_once('#')
                .unwrap()
                .1
                .to_string(),
            Some(42),
        );
        let script = descriptor
            .receive_descriptor()
            .derive(
                ChildNumber::Normal { index: 11 },
                &Secp256k1::verification_only(),
            )
            .script_pubkey();
        let binding = SplitTargetBinding {
            vault_id: vault_id.clone(),
            descriptor: descriptor.to_string(),
            receive_index: 11,
            script_hex: hex::encode(script.as_bytes()),
        };
        binding.validate().unwrap();
        (binding, vault_id, descriptor)
    }

    #[test]
    fn split_target_binding_is_durable_and_cannot_rotate_on_retry() {
        let directory = std::env::temp_dir().join(format!(
            "split-target-{}-{}",
            std::process::id(),
            super::super::controller_id().unwrap()
        ));
        let source = source();
        let (binding, wallet_id, descriptor) = target_binding();
        let saved = reserve_split_funding_with_target(
            &directory,
            &source,
            "cube-a",
            &context(),
            &inventory(3),
            &binding,
        )
        .unwrap();
        assert_eq!(saved.target_binding.as_ref(), Some(&binding));
        assert!(binding.matches_vault(&wallet_id, &descriptor));
        let released = release_split_funding(&directory, &saved).unwrap();
        assert_eq!(released.target_binding.as_ref(), Some(&binding));
        assert!(released.is_released());
        let mut later = context();
        later.generation += 1;
        assert_eq!(
            load_split_funding(&directory, &source, "cube-a", &later)
                .unwrap()
                .unwrap(),
            released
        );
        let mut changed = binding.clone();
        changed.receive_index = 12;
        assert!(matches!(changed.validate(), Err(Error::InvalidPlan)));
        changed = binding.clone();
        changed.vault_id.timestamp = Some(43);
        assert!(!changed.matches_vault(&wallet_id, &descriptor));
        assert!(matches!(
            reserve_split_funding_with_target(
                &directory,
                &source,
                "cube-a",
                &later,
                &inventory(3),
                &changed,
            ),
            Err(Error::WrongIdentity)
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn split_funding_reservation_survives_restart_and_cannot_move() {
        let directory = std::env::temp_dir().join(format!(
            "split-funding-{}-{}",
            std::process::id(),
            super::super::controller_id().unwrap()
        ));
        let source = source();
        let saved = reserve_split_funding(&directory, &source, "cube-a", &context(), &inventory(3))
            .unwrap();
        let mut old = serde_json::to_value(&saved).unwrap();
        old.as_object_mut().unwrap().remove("released");
        old["version"] = serde_json::json!(1);
        let old: SplitFundingReservation = serde_json::from_value(old).unwrap();
        assert!(!old.is_released());
        assert_eq!(saved.addresses().unwrap().len(), 3);
        let released = release_split_funding(&directory, &saved).unwrap();
        assert!(released.is_released());
        assert_eq!(released.addresses().unwrap(), saved.addresses().unwrap());
        assert!(matches!(
            release_split_funding(&directory, &saved),
            Err(Error::Conflict)
        ));
        let mut restarted = context();
        restarted.generation += 1;
        assert_eq!(
            load_split_funding(&directory, &source, "cube-a", &restarted)
                .unwrap()
                .unwrap(),
            released
        );
        assert_eq!(
            reserve_split_funding(&directory, &source, "cube-a", &restarted, &inventory(3))
                .unwrap(),
            released
        );
        assert_eq!(
            reserve_split_funding(&directory, &source, "cube-a", &context(), &inventory(4))
                .unwrap(),
            released
        );
        let mut other_account = context();
        other_account.account = "account-b".into();
        assert!(matches!(
            load_split_funding(&directory, &source, "cube-a", &other_account),
            Err(Error::WrongIdentity)
        ));
        std::fs::write(directory.join("split-funding.json"), b"{}").unwrap();
        assert!(matches!(
            load_split_funding(&directory, &source, "cube-a", &context()),
            Err(Error::InvalidJournal)
        ));
        assert!(matches!(
            reserve_split_funding(&directory, &source, "cube-a", &context(), &inventory(3)),
            Err(Error::InvalidJournal)
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn split_funding_requires_distinct_ranged_addresses() {
        let directory = std::env::temp_dir().join(format!(
            "split-invalid-funding-{}-{}",
            std::process::id(),
            super::super::controller_id().unwrap()
        ));
        let source = source();
        assert_eq!(inventory(3).fresh_funding_addresses(), Some(indices()));
        assert!(matches!(
            reserve_split_funding(&directory, &source, "cube-a", &context(), &inventory(18)),
            Err(Error::InvalidPlan)
        ));
        let mut same = indices();
        same.bitcoin_return = same.btcb2_deposit;
        assert!(matches!(
            SplitFundingReservation::new(&source, "cube-a", &context(), same),
            Err(Error::InvalidPlan)
        ));
        let fixed = Descriptor::from_str(
            "wpkh(02c6047f9441ed7d6d3045406e95c07cd85aeb5c6b7a3c2e21b73cdb1e24ff3a64)",
        )
        .unwrap();
        let fixed = SplitSource::new(fixed, None).unwrap();
        assert!(matches!(
            SplitFundingReservation::new(&fixed, "cube-a", &context(), indices()),
            Err(Error::InvalidPlan)
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reserved_discovery_requires_release_and_account_but_not_matching_scan_generation() {
        let directory = std::env::temp_dir().join(format!(
            "split-funding-discovery-{}-{}",
            std::process::id(),
            super::super::controller_id().unwrap()
        ));
        let source = source();
        let saved = reserve_split_funding(&directory, &source, "cube-a", &context(), &inventory(3))
            .unwrap();
        assert!(matches!(
            saved.checked_reserved_inventory(
                &directory,
                &source,
                &context(),
                &inventory(3),
                (0, 0)
            ),
            Err(SplitFundingDiscoveryError::Changed)
        ));
        let released = release_split_funding(&directory, &saved).unwrap();
        let mut later_account = context();
        later_account.generation = 19;
        // The foreign scan generation stays at 1. It is separate from the
        // authenticated Connect account generation.
        assert!(released
            .checked_reserved_inventory(&directory, &source, &later_account, &inventory(3), (0, 0))
            .is_ok());
        later_account.account = "another-account".into();
        assert!(matches!(
            released.checked_reserved_inventory(
                &directory,
                &source,
                &later_account,
                &inventory(3),
                (0, 0)
            ),
            Err(SplitFundingDiscoveryError::InvalidReservation(
                Error::WrongIdentity
            ))
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
