//! Immutable pre-deposit reservation for a paired Bitcoin/BTCB2 Vault Claim.
//! It pins address selection and identity across restart; it is never proof
//! of chain exclusivity or a permission to sign.

use super::{
    journal::Journal, paired_signing_permit::PairedSigningPermit, Context, Controller, Error,
    WalletIdentity,
};
use crate::daemon::{model, Daemon};
use crate::services::{
    claim_inventory::{ClaimInventory, ClaimInventoryError, ClaimPairSnapshotError},
    claim_observation::{
        http::{
            ClaimPairError, CollectedPostBitcoinInputs, DiscoveryError, FundingContext,
            HttpObservationSource, OwnedClaimFunding,
        },
        CollectionContext, FailureKind, ObservationSource,
    },
    coincube::CoincubeClient,
    foreign_psbt::btcb2_sweep_feerate,
    foreign_split_inventory::FreshDepositAddresses,
    split_evidence::ConnectEsplora,
    split_fees::{bitcoin_step1_feerate, btcb2_fee_source, ConnectBitcoinFees},
};
use async_trait::async_trait;
use coincube_core::{
    chain::ChainId,
    claim::{Policy, PreflightTips},
    claim_ancestry::retained::RetainedPath,
    claim_finalize::{validate_ancestry_transfer_signing, validate_claim_fork_signing},
    claim_spend::{AncestrySelfTransfer, ClaimForkSweep},
    descriptors::CoincubeDescriptor,
    miniscript::bitcoin::{
        bip32::ChildNumber,
        hashes::{sha256, Hash},
        psbt::Psbt,
        Address, Network, OutPoint, ScriptBuf, Transaction,
    },
    psbt_unified::UnifiedPsbt,
    spend::CandidateCoin,
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

const ADDRESS_CHECK_BUDGET: Duration = Duration::from_secs(20);
const MAX_ADDRESS_AGE_SECONDS: i64 = 60;
// A remote wallet exposes durable get-new-address but not Coincubed's bulk
// update-derivation-index command. Refuse a pathological gap instead of
// issuing an unbounded series of remote writes from the Claim screen.
const MAX_REMOTE_RECEIVE_ADVANCES: u32 = 256;

/// Fresh complete-wallet and two-chain requalification for one exact Claim
/// pair. Only the saved deposit reservation can produce it. Holding the
/// PSBTs here prevents a later finalizer from applying this check to a
/// different transaction with the same funding outpoints.
#[derive(Debug)]
#[allow(dead_code)] // The paired signer panel has not yet retained this result.
pub(crate) struct RequalifiedInitialClaim {
    pub(super) funding: OwnedClaimFunding,
    bitcoin_psbt: Psbt,
    fork_psbt: Psbt,
    context: Context,
    tips: coincube_core::claim::PreflightTips,
    checked_at: std::time::Instant,
}

impl RequalifiedInitialClaim {
    /// Reauthenticate the journaled Claim witnesses after a fresh complete
    /// wallet and two-chain scan. The result can populate a new broadcast
    /// review, but is not policy acceptance, approval or a transport gate.
    #[allow(dead_code)] // The paired submission review will consume this boundary.
    pub(crate) fn reverify_recorded_pair_for_review(
        &self,
        controller: &mut Controller,
        bitcoin: &AncestrySelfTransfer,
        fork: &ClaimForkSweep,
        policy: Policy,
        now: i64,
    ) -> Result<
        (
            coincube_core::claim_finalize::VerifiedAncestryClaimPair,
            super::paired_submission::InitialAncestryProof,
        ),
        Error,
    > {
        if !self.matches_pair(bitcoin, fork) || self.checked_at.elapsed() >= Duration::from_secs(10)
        {
            return Err(Error::WrongIdentity);
        }
        controller.validate_ancestry_pair_observation(
            &self.context,
            &self.funding.inputs,
            &self.funding.bitcoin,
            &self.funding.fork,
            bitcoin,
            fork,
            policy,
            now,
            self.tips,
        )?;
        let verified =
            controller.verify_recorded_ancestry_claim_pair(&self.context, bitcoin, fork)?;
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

    pub(crate) fn funding(&self) -> &OwnedClaimFunding {
        &self.funding
    }

    pub(crate) fn context(&self) -> &Context {
        &self.context
    }

    pub(crate) fn tips(&self) -> coincube_core::claim::PreflightTips {
        self.tips
    }

    pub(super) fn matches_pair(
        &self,
        bitcoin: &AncestrySelfTransfer,
        fork: &ClaimForkSweep,
    ) -> bool {
        self.bitcoin_psbt == *bitcoin.psbt() && self.fork_psbt == *fork.psbt()
    }

    /// Issue a one-use permit only for a current, structurally unchanged
    /// signer PSBT of the exact pair that passed the wallet and chain check.
    /// The caller must run `requalify_exact_pair` again for every dispatch.
    #[allow(dead_code)] // The paired signer panel is staged behind the migration gate.
    pub(crate) fn signer_permit(
        &self,
        chain: ChainId,
        bitcoin: &AncestrySelfTransfer,
        fork: &ClaimForkSweep,
        current: &Psbt,
        generation: watch::Receiver<u64>,
    ) -> Option<PairedSigningPermit> {
        if !self.matches_pair(bitcoin, fork)
            || self.checked_at.elapsed() >= Duration::from_secs(10)
            || *generation.borrow() != self.context.generation
            || generation.has_changed().is_err()
        {
            return None;
        }
        let valid = match chain {
            ChainId::Bitcoin => validate_ancestry_transfer_signing(bitcoin, current).is_ok(),
            ChainId::BitcoinBlake2b => UnifiedPsbt::from_psbt(current.clone())
                .is_ok_and(|unified| validate_claim_fork_signing(fork, &unified).is_ok()),
            _ => false,
        };
        valid.then(|| PairedSigningPermit::new(chain, current, generation, self.context.generation))
    }
}

fn at_reserved_receive_index(coin: &CandidateCoin, index: u32) -> bool {
    !coin.is_change && coin.deriv_index == ChildNumber::Normal { index }
}

fn select_reserved_candidates(
    bitcoin: &[CandidateCoin],
    fork: &[CandidateCoin],
    indices: FreshDepositAddresses,
) -> (Vec<CandidateCoin>, Vec<CandidateCoin>) {
    (
        bitcoin
            .iter()
            .filter(|coin| at_reserved_receive_index(coin, indices.bitcoin_deposit))
            .copied()
            .collect(),
        fork.iter()
            .filter(|coin| at_reserved_receive_index(coin, indices.btcb2_deposit))
            .copied()
            .collect(),
    )
}

fn fee_eligible_candidates(
    candidates: Vec<CandidateCoin>,
    minimum_sats: u64,
) -> Vec<CandidateCoin> {
    candidates
        .into_iter()
        .filter(|coin| coin.amount.to_sat() >= minimum_sats)
        .collect()
}

fn eligible_selected_candidates(
    candidates: Vec<CandidateCoin>,
    minimum_sats: u64,
    selected: Option<OutPoint>,
) -> Vec<CandidateCoin> {
    fee_eligible_candidates(candidates, minimum_sats)
        .into_iter()
        .filter(|coin| selected.is_none_or(|outpoint| coin.outpoint == outpoint))
        .collect()
}

fn minimum_claim_deposits(
    descriptor: &CoincubeDescriptor,
    indices: FreshDepositAddresses,
    shared_count: usize,
    bitcoin_rate: u64,
    btcb2_rate: u64,
) -> Result<(u64, u64), ClaimAddressError> {
    let size = coincube_core::claim_spend::estimate_ancestry_self_transfer_vbytes(
        descriptor,
        shared_count,
    )
    .map_err(|_| ClaimAddressError::FeeUnavailable)?;
    let secp = coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only();
    let minimum = |index: u32, rate: u64| {
        let script = descriptor
            .receive_descriptor()
            .derive(ChildNumber::Normal { index }, &secp)
            .script_pubkey();
        size.checked_mul(rate)
            .map(|fee| fee.max(script.minimal_non_dust().to_sat()))
            .ok_or(ClaimAddressError::FeeUnavailable)
    };
    Ok((
        minimum(indices.bitcoin_deposit, bitcoin_rate)?,
        minimum(indices.btcb2_deposit, btcb2_rate)?,
    ))
}

/// Fresh fee floors for the two saved receive indices. These only filter
/// read-only deposit candidates; exact construction still checks the fee.
pub async fn quote_claim_deposit_minimums(
    descriptor: &CoincubeDescriptor,
    indices: FreshDepositAddresses,
    shared_count: usize,
    client: CoincubeClient,
) -> Result<(u64, u64), ClaimAddressError> {
    quote_claim_deposit_terms(descriptor, indices, shared_count, client)
        .await
        .map(|(minimums, _)| minimums)
}

/// Return the fresh fee rates together with the candidate floors so a review
/// builds its exact PSBTs from the same quotes used to admit the deposits.
pub async fn quote_claim_deposit_terms(
    descriptor: &CoincubeDescriptor,
    indices: FreshDepositAddresses,
    shared_count: usize,
    client: CoincubeClient,
) -> Result<((u64, u64), (u64, u64)), ClaimAddressError> {
    let bitcoin_fees = ConnectBitcoinFees::new(&client).ok_or(ClaimAddressError::FeeUnavailable)?;
    let bitcoin_rate = bitcoin_step1_feerate(&bitcoin_fees)
        .await
        .ok_or(ClaimAddressError::FeeUnavailable)?;
    let btcb2_fees = btcb2_fee_source(Some(client));
    let btcb2_rate = btcb2_sweep_feerate(&*btcb2_fees)
        .await
        .ok_or(ClaimAddressError::FeeUnavailable)?;
    Ok((
        minimum_claim_deposits(descriptor, indices, shared_count, bitcoin_rate, btcb2_rate)?,
        (bitcoin_rate, btcb2_rate),
    ))
}

fn ensure_claim_wallet_ready(
    info: &model::GetInfoResult,
    saved: &ClaimFundingReservation,
) -> Result<(), ClaimAddressError> {
    if info.network != Network::Bitcoin
        || info.chain_id != Some(coincube_core::chain::ChainId::Bitcoin)
        || info.descriptors.main.to_string() != saved.descriptor
        || info.block_height <= 0
        || !info.sync.is_finite()
        || info.sync < 0.9999
        || info.rescan_progress.is_some()
        || info.chain_divergence
        || info.refused_reorg_depth.is_some()
        || info.history_sync.full_scan_in_progress
        || info.history_sync.last_failure.is_some()
    {
        return Err(ClaimAddressError::WalletState);
    }
    Ok(())
}

/// Make both already reserved receive addresses visible to the wallet's own
/// scanner. Connect's remote backend advances one address at a time; local
/// Coincubed can advance directly. Either way, read back the exact two
/// derivations before marking the journal reservation released.
async fn reveal_claim_receive_addresses(
    daemon: &(dyn Daemon + Send + Sync),
    saved: &ClaimFundingReservation,
    generation: &watch::Receiver<u64>,
    expected: u64,
) -> Result<(), ClaimAddressError> {
    let mut info = daemon
        .get_info()
        .await
        .map_err(|_| ClaimAddressError::WalletState)?;
    ensure_claim_wallet_ready(&info, saved)?;
    let highest = saved
        .indices
        .bitcoin_deposit
        .max(saved.indices.btcb2_deposit);
    if !current(generation, expected) {
        return Err(ClaimAddressError::ChangedSession);
    }
    if info.receive_index < highest {
        if daemon.backend().is_remote() {
            let advances = highest - info.receive_index;
            if advances > MAX_REMOTE_RECEIVE_ADVANCES {
                return Err(ClaimAddressError::WalletState);
            }
            let descriptor = saved
                .descriptor()
                .map_err(|_| ClaimAddressError::InvalidReservation)?;
            let mut last = info.receive_index;
            for _ in 0..advances {
                if !current(generation, expected) {
                    return Err(ClaimAddressError::ChangedSession);
                }
                let generated = daemon
                    .get_new_address()
                    .await
                    .map_err(|_| ClaimAddressError::WalletState)?;
                let ChildNumber::Normal { index } = generated.derivation_index else {
                    return Err(ClaimAddressError::WalletState);
                };
                if index <= last
                    || generated.address.to_string()
                        != saved
                            .address(&descriptor, index)
                            .map_err(|_| ClaimAddressError::InvalidReservation)?
                {
                    return Err(ClaimAddressError::WalletState);
                }
                last = index;
                if last >= highest {
                    break;
                }
            }
            if last < highest {
                return Err(ClaimAddressError::WalletState);
            }
        } else {
            let updated = daemon
                .update_deriv_indexes(Some(highest), None)
                .await
                .map_err(|_| ClaimAddressError::WalletState)?;
            if updated.receive < highest {
                return Err(ClaimAddressError::WalletState);
            }
        }
    }
    if !current(generation, expected) {
        return Err(ClaimAddressError::ChangedSession);
    }
    info = daemon
        .get_info()
        .await
        .map_err(|_| ClaimAddressError::WalletState)?;
    ensure_claim_wallet_ready(&info, saved)?;
    if info.receive_index < highest {
        return Err(ClaimAddressError::WalletState);
    }
    let addresses = saved
        .addresses()
        .map_err(|_| ClaimAddressError::InvalidReservation)?;
    for (index, address) in [
        (saved.indices.bitcoin_deposit, &addresses[0]),
        (saved.indices.btcb2_deposit, &addresses[1]),
    ] {
        if !current(generation, expected) {
            return Err(ClaimAddressError::ChangedSession);
        }
        let listed = daemon
            .list_revealed_addresses(false, false, 1, Some(ChildNumber::Normal { index }))
            .await
            .map_err(|_| ClaimAddressError::WalletState)?;
        if !listed.addresses.iter().any(|entry| {
            entry.index == ChildNumber::Normal { index } && entry.address.to_string() == *address
        }) {
            return Err(ClaimAddressError::WalletState);
        }
    }
    if !current(generation, expected) {
        return Err(ClaimAddressError::ChangedSession);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimAddressError {
    InvalidReservation,
    WrongIdentity,
    MissingReservation,
    ChangedSession,
    Used,
    Unavailable,
    FeeUnavailable,
    WalletState,
    Stale,
    Deadline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedClaimFundingAddresses {
    pub bitcoin_deposit: String,
    pub btcb2_deposit: String,
    pub bitcoin_minimum_sats: u64,
    pub btcb2_minimum_sats: u64,
    pub bitcoin_rate_sat_vb: u64,
    pub btcb2_rate_sat_vb: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckedClaimAddresses {
    bitcoin_deposit: String,
    btcb2_deposit: String,
}

#[derive(Debug)]
pub enum ClaimFundingDiscoveryError {
    InvalidReservation(Error),
    WalletState(ClaimInventoryError),
    Discovery(DiscoveryError),
}

#[async_trait]
trait AddressHistory: Send + Sync {
    async fn used(
        &self,
        chain: coincube_core::chain::ChainId,
        address: &str,
    ) -> Result<(bool, i64), FailureKind>;
}

#[async_trait]
impl AddressHistory for ConnectEsplora {
    async fn used(
        &self,
        chain: coincube_core::chain::ChainId,
        address: &str,
    ) -> Result<(bool, i64), FailureKind> {
        let read = self.address_used(chain, address).await?;
        Ok((*read.value(), read.observed_at()))
    }
}

fn current(generation: &watch::Receiver<u64>, expected: u64) -> bool {
    *generation.borrow() == expected && generation.has_changed().is_ok()
}

async fn checked_history(
    source: &dyn AddressHistory,
    addresses: [String; 2],
    generation: &watch::Receiver<u64>,
    expected: u64,
    require_unused: bool,
) -> Result<CheckedClaimAddresses, ClaimAddressError> {
    use coincube_core::chain::ChainId;
    if !current(generation, expected) {
        return Err(ClaimAddressError::ChangedSession);
    }
    tokio::time::timeout(ADDRESS_CHECK_BUDGET, async {
        for address in &addresses {
            for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
                let (used, observed_at) = source
                    .used(chain, address)
                    .await
                    .map_err(|_| ClaimAddressError::Unavailable)?;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| ClaimAddressError::Stale)?
                    .as_secs() as i64;
                let age = now
                    .checked_sub(observed_at)
                    .ok_or(ClaimAddressError::Stale)?;
                if !(0..=MAX_ADDRESS_AGE_SECONDS).contains(&age) {
                    return Err(ClaimAddressError::Stale);
                }
                if used && require_unused {
                    return Err(ClaimAddressError::Used);
                }
                if !current(generation, expected) {
                    return Err(ClaimAddressError::ChangedSession);
                }
            }
        }
        Ok(CheckedClaimAddresses {
            bitcoin_deposit: addresses[0].clone(),
            btcb2_deposit: addresses[1].clone(),
        })
    })
    .await
    .map_err(|_| ClaimAddressError::Deadline)?
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimFundingReservation {
    version: u32,
    identity: WalletIdentity,
    account: String,
    provider: String,
    descriptor: String,
    pub indices: FreshDepositAddresses,
    /// Set only after both addresses passed the initial unused-history check
    /// and the Bitcoin daemon acknowledged their receive indices. Later
    /// deposits make the address history nonempty, so restart must not repeat
    /// that initial check or silently rotate addresses.
    #[serde(default)]
    released: bool,
    /// Present only after both wallet backends durably advanced separate
    /// change indices. A partial reservation burns its index but never writes
    /// this pair, so a restart cannot infer authority from one side alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    change: Option<ClaimChangeIndices>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimChangeIndices {
    pub bitcoin: u32,
    pub fork: u32,
}

#[derive(Debug)]
pub enum ClaimChangeError {
    Journal(Error),
    WalletState,
    Cancelled,
}

/// A fresh exact-pair recheck is a prerequisite for signer dispatch, not a
/// durable authorization. The signer and submission coordinators must repeat
/// it at their respective boundaries and apply their own policy checks.
#[derive(Debug)]
#[allow(dead_code)] // The paired signer dispatch is not wired yet.
pub enum ClaimPairRequalificationError {
    Discovery(ClaimFundingDiscoveryError),
    Construction(ClaimPairError),
    Snapshot(ClaimPairSnapshotError),
    Observation(FailureKind),
    Changed,
}

#[derive(Debug)]
pub enum ClaimPostBitcoinError {
    Changed,
    Observation(FailureKind),
    Wallet(ClaimInventoryError),
}

/// Only the full saved-reservation, source-observation and BTCB2 Vault
/// requalification below can create this value. It still expires and does
/// not by itself authorize signing or a network attempt.
#[allow(dead_code)] // The paired stage-two coordinator is not wired yet.
pub struct RequalifiedPostBitcoinClaim {
    pub(super) observed: CollectedPostBitcoinInputs,
}

fn change_wallet_ready(
    info: &model::GetInfoResult,
    chain: coincube_core::chain::ChainId,
    descriptor: &CoincubeDescriptor,
    reserved: Option<u32>,
) -> bool {
    info.network == Network::Bitcoin
        && info.chain_id == Some(chain)
        && info.descriptors.main == *descriptor
        && info.rescan_progress.is_none()
        && !info.chain_divergence
        && info.refused_reorg_depth.is_none()
        && !info.history_sync.full_scan_in_progress
        && info.history_sync.last_failure.is_none()
        && reserved.is_none_or(|index| info.change_index >= index)
}

impl ClaimFundingReservation {
    pub(super) fn new(
        descriptor: &CoincubeDescriptor,
        identity: &WalletIdentity,
        context: &Context,
        indices: FreshDepositAddresses,
    ) -> Result<Self, Error> {
        let saved = Self {
            version: 2,
            identity: identity.clone(),
            account: context.account.clone(),
            provider: context.provider.clone(),
            descriptor: descriptor.to_string(),
            indices,
            released: false,
            change: None,
        };
        saved.validate()?;
        Ok(saved)
    }

    pub(super) fn validate(&self) -> Result<(), Error> {
        if !matches!(self.version, 1..=3)
            || (self.version == 1 && self.released)
            || (self.version == 3) != self.change.is_some()
            || self.change.is_some_and(|change| {
                !self.released
                    || change.bitcoin >= 1 << 31
                    || change.fork >= 1 << 31
                    || change.bitcoin == change.fork
            })
            || self.identity.bitcoin_cube.is_empty()
            || self.identity.fork_cube.is_empty()
            || self.identity.bitcoin_cube.len() > 256
            || self.identity.fork_cube.len() > 256
            || self.account.is_empty()
            || self.account.len() > 256
            || self.provider.is_empty()
            || self.provider.len() > 2048
            || self.descriptor.len() > 16_384
            || self.indices.bitcoin_deposit >= 1 << 31
            || self.indices.btcb2_deposit >= 1 << 31
            || self.indices.bitcoin_deposit == self.indices.btcb2_deposit
        {
            return Err(Error::InvalidPlan);
        }
        let descriptor = self.descriptor()?;
        if sha256::Hash::hash(descriptor.to_string().as_bytes()) != self.identity.descriptor_digest
        {
            return Err(Error::InvalidPlan);
        }
        let bitcoin_address = self.address(&descriptor, self.indices.bitcoin_deposit)?;
        let fork_address = self.address(&descriptor, self.indices.btcb2_deposit)?;
        if bitcoin_address == fork_address {
            return Err(Error::InvalidPlan);
        }
        Ok(())
    }

    fn descriptor(&self) -> Result<CoincubeDescriptor, Error> {
        CoincubeDescriptor::from_str(&self.descriptor).map_err(|_| Error::InvalidPlan)
    }

    fn address(&self, descriptor: &CoincubeDescriptor, index: u32) -> Result<String, Error> {
        let script = descriptor
            .receive_descriptor()
            .derive(
                coincube_core::miniscript::bitcoin::bip32::ChildNumber::Normal { index },
                &coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only(),
            )
            .script_pubkey();
        Address::from_script(&script, Network::Bitcoin)
            .map(|address| address.to_string())
            .map_err(|_| Error::InvalidPlan)
    }

    /// Display only after a fresh, authenticated two-chain address-history
    /// check under the current account confirms both addresses are unused.
    pub fn addresses(&self) -> Result<[String; 2], Error> {
        let descriptor = self.descriptor()?;
        Ok([
            self.address(&descriptor, self.indices.bitcoin_deposit)?,
            self.address(&descriptor, self.indices.btcb2_deposit)?,
        ])
    }

    pub(super) fn is_released(&self) -> bool {
        self.released
    }

    pub(super) fn released_copy(&self) -> Self {
        let mut copy = self.clone();
        copy.version = if copy.change.is_some() { 3 } else { 2 };
        copy.released = true;
        copy
    }

    pub fn change_indices(&self) -> Option<ClaimChangeIndices> {
        self.change
    }

    pub(super) fn with_change_indices(&self, change: ClaimChangeIndices) -> Result<Self, Error> {
        if !self.released || self.change.is_some() {
            return Err(Error::InvalidPlan);
        }
        let mut copy = self.clone();
        copy.version = 3;
        copy.change = Some(change);
        copy.validate()?;
        Ok(copy)
    }

    fn matches(
        &self,
        descriptor: &CoincubeDescriptor,
        identity: &WalletIdentity,
        context: &Context,
    ) -> bool {
        self.identity == *identity
            && self.account == context.account
            && self.provider == context.provider
            && self.descriptor().is_ok_and(|saved| saved == *descriptor)
    }

    fn reserved_candidates(
        &self,
        inventory: &ClaimInventory,
        identity: &WalletIdentity,
        context: &Context,
    ) -> Result<(Vec<CandidateCoin>, Vec<CandidateCoin>), Error> {
        self.validate()?;
        if !self.is_released()
            || !self.matches(inventory.descriptor(), identity, context)
            || inventory.scan_report().bitcoin.generation() != context.generation
            || inventory.scan_report().btcb2.generation() != context.generation
        {
            return Err(Error::WrongIdentity);
        }
        Ok(select_reserved_candidates(
            inventory.bitcoin_funding(),
            inventory.fork_funding(),
            self.indices,
        ))
    }

    /// Display-only candidate presence at the two exact released addresses.
    /// Neither a scan hit nor this status proves exclusive ancestry.
    pub fn candidate_presence(
        &self,
        inventory: &ClaimInventory,
        identity: &WalletIdentity,
        context: &Context,
    ) -> Result<(bool, bool), Error> {
        let (bitcoin, fork) = self.reserved_candidates(inventory, identity, context)?;
        Ok((!bitcoin.is_empty(), !fork.is_empty()))
    }

    /// Whether either reserved address has a coin large enough to cover the
    /// current estimated fee. This is still only a scan-time display hint.
    pub fn fee_eligible_candidate_presence(
        &self,
        inventory: &ClaimInventory,
        identity: &WalletIdentity,
        context: &Context,
        minimums: (u64, u64),
    ) -> Result<(bool, bool), Error> {
        let (bitcoin, fork) = self.reserved_candidates(inventory, identity, context)?;
        Ok((
            bitcoin
                .iter()
                .any(|coin| coin.amount.to_sat() >= minimums.0),
            fork.iter().any(|coin| coin.amount.to_sat() >= minimums.1),
        ))
    }

    /// Search only the exact two receive addresses reserved before deposits.
    /// Other post-fork Vault outputs cannot silently replace the user's
    /// funding plan. The returned pair is still read-only: source-wallet
    /// reconciliation and a fresh signer preflight remain mandatory.
    #[allow(clippy::too_many_arguments)] // Identity, daemon, and observations remain explicit admission inputs.
    pub async fn discover_reserved_funding(
        &self,
        directory: PathBuf,
        inventory: &ClaimInventory,
        identity: &WalletIdentity,
        context: &Context,
        daemon: &(dyn Daemon + Send + Sync),
        source: &HttpObservationSource,
        policy: Policy,
        minimums: (u64, u64),
    ) -> Result<Option<OwnedClaimFunding>, ClaimFundingDiscoveryError> {
        self.discover_reserved_funding_at_outpoints(
            directory, inventory, identity, context, daemon, source, policy, minimums, None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn discover_reserved_funding_at_outpoints(
        &self,
        directory: PathBuf,
        inventory: &ClaimInventory,
        identity: &WalletIdentity,
        context: &Context,
        daemon: &(dyn Daemon + Send + Sync),
        source: &HttpObservationSource,
        policy: Policy,
        minimums: (u64, u64),
        selected: Option<(OutPoint, OutPoint)>,
    ) -> Result<Option<OwnedClaimFunding>, ClaimFundingDiscoveryError> {
        let descriptor = inventory.descriptor().clone();
        let owner = identity.clone();
        let account = context.clone();
        let saved = tokio::task::spawn_blocking(move || {
            load_claim_funding(&directory, &descriptor, &owner, &account)
        })
        .await
        .map_err(|_| ClaimFundingDiscoveryError::InvalidReservation(Error::InvalidJournal))?
        .map_err(ClaimFundingDiscoveryError::InvalidReservation)?
        .ok_or(ClaimFundingDiscoveryError::InvalidReservation(
            Error::InvalidPlan,
        ))?;
        if saved != *self {
            return Err(ClaimFundingDiscoveryError::InvalidReservation(
                Error::InvalidPlan,
            ));
        }
        let (bitcoin, fork) = self
            .reserved_candidates(inventory, identity, context)
            .map_err(ClaimFundingDiscoveryError::InvalidReservation)?;
        let bitcoin =
            eligible_selected_candidates(bitcoin, minimums.0, selected.map(|pair| pair.0));
        let fork = eligible_selected_candidates(fork, minimums.1, selected.map(|pair| pair.1));
        tokio::time::timeout(
            ADDRESS_CHECK_BUDGET,
            inventory.reconcile_daemon(daemon, self.indices.bitcoin_deposit),
        )
        .await
        .map_err(|_| ClaimFundingDiscoveryError::WalletState(ClaimInventoryError::WalletState))?
        .map_err(ClaimFundingDiscoveryError::WalletState)?;
        if bitcoin.is_empty() || fork.is_empty() || inventory.shared().is_empty() {
            return Ok(None);
        }
        source
            .discover_owned_claim_funding(
                inventory.descriptor(),
                &bitcoin,
                &fork,
                inventory.shared(),
                policy,
            )
            .await
            .map_err(ClaimFundingDiscoveryError::Discovery)
    }

    /// Recollect the two exact funding outpoints, rebuild the saved ordinary
    /// pair using the already reserved change indices, and compare every PSBT
    /// field before rereading both complete wallet coin sets and both tips.
    /// A prior preview or a different deposit at either address cannot stand
    /// in for this check. The result remains read-only evidence: every signer
    /// dispatch must rerun it, and no network send is permitted by it.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)] // The paired signer dispatch is not wired yet.
    pub(crate) async fn requalify_exact_pair(
        &self,
        directory: PathBuf,
        inventory: &ClaimInventory,
        identity: &WalletIdentity,
        context: &Context,
        bitcoin_daemon: &(dyn Daemon + Send + Sync),
        fork_daemon: &(dyn Daemon + Send + Sync),
        source: &HttpObservationSource,
        policy: Policy,
        minimums: (u64, u64),
        bitcoin: &AncestrySelfTransfer,
        fork: &ClaimForkSweep,
        rates: (u64, u64),
    ) -> Result<RequalifiedInitialClaim, ClaimPairRequalificationError> {
        source
            .validate_context(&context.provider, context.generation)
            .map_err(ClaimPairRequalificationError::Observation)?;
        let change = self.change.ok_or(ClaimPairRequalificationError::Changed)?;
        if !self.released
            || u32::from(bitcoin.change_index()) != change.bitcoin
            || u32::from(fork.change_index()) != change.fork
        {
            return Err(ClaimPairRequalificationError::Changed);
        }
        let selected = (
            bitcoin.poison_input(),
            fork.fork_marker()
                .ok_or(ClaimPairRequalificationError::Changed)?,
        );
        let funding = self
            .discover_reserved_funding_at_outpoints(
                directory.clone(),
                inventory,
                identity,
                context,
                bitcoin_daemon,
                source,
                policy,
                minimums,
                Some(selected),
            )
            .await
            .map_err(ClaimPairRequalificationError::Discovery)?
            .ok_or(ClaimPairRequalificationError::Changed)?;
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
            now: source.now(),
            max_age_seconds: policy.max_observation_age_seconds,
            tips,
        };
        let (rebuilt_bitcoin, rebuilt_fork) = inventory
            .construct_pair_for_review(
                &funding,
                ChildNumber::Normal {
                    index: change.bitcoin,
                },
                ChildNumber::Normal { index: change.fork },
                rates.0,
                rates.1,
                review,
            )
            .map_err(ClaimPairRequalificationError::Construction)?;
        if rebuilt_bitcoin.psbt() != bitcoin.psbt() || rebuilt_fork.psbt() != fork.psbt() {
            return Err(ClaimPairRequalificationError::Changed);
        }
        inventory
            .reconcile_pair_snapshot(
                bitcoin_daemon,
                fork_daemon,
                self.indices.bitcoin_deposit,
                self.indices.btcb2_deposit,
                &funding,
                bitcoin,
                fork,
                source,
                review,
            )
            .await
            .map_err(ClaimPairRequalificationError::Snapshot)?;
        let saved_descriptor = inventory.descriptor().clone();
        let saved_identity = identity.clone();
        let saved_context = context.clone();
        let current = tokio::task::spawn_blocking(move || {
            load_claim_funding(
                &directory,
                &saved_descriptor,
                &saved_identity,
                &saved_context,
            )
        })
        .await
        .map_err(|_| ClaimPairRequalificationError::Changed)?
        .map_err(|_| ClaimPairRequalificationError::Changed)?;
        if current.as_ref() != Some(self) {
            return Err(ClaimPairRequalificationError::Changed);
        }
        source
            .validate_context(&context.provider, context.generation)
            .map_err(ClaimPairRequalificationError::Observation)?;
        Ok(RequalifiedInitialClaim {
            funding,
            bitcoin_psbt: bitcoin.psbt().clone(),
            fork_psbt: fork.psbt().clone(),
            context: context.clone(),
            tips,
            checked_at: std::time::Instant::now(),
        })
    }

    /// After the exact Bitcoin spend confirms, check that the paired BTCB2
    /// Vault still owns every shared input and the reserved BTCB2-only input.
    /// This cannot reuse the initial all-unspent collection. The result is
    /// fresh read-only evidence; it is not a signer or broadcast capability.
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)] // The paired Claim stage-two coordinator is not wired yet.
    pub async fn requalify_post_bitcoin_fork(
        &self,
        directory: PathBuf,
        inventory: &ClaimInventory,
        identity: &WalletIdentity,
        context: &Context,
        fork_daemon: &(dyn Daemon + Send + Sync),
        source: &HttpObservationSource,
        policy: Policy,
        bitcoin: &AncestrySelfTransfer,
        fork: &ClaimForkSweep,
        signed_bitcoin: &Transaction,
        bitcoin_path: &RetainedPath,
        fork_path: &RetainedPath,
    ) -> Result<RequalifiedPostBitcoinClaim, ClaimPostBitcoinError> {
        source
            .validate_context(&context.provider, context.generation)
            .map_err(ClaimPostBitcoinError::Observation)?;
        let change = self.change.ok_or(ClaimPostBitcoinError::Changed)?;
        let funding = fork.fork_marker().ok_or(ClaimPostBitcoinError::Changed)?;
        let mut unsigned_bitcoin = signed_bitcoin.clone();
        for input in &mut unsigned_bitcoin.input {
            input.script_sig = ScriptBuf::new();
            input.witness.clear();
        }
        if !self.released
            || !self.matches(inventory.descriptor(), identity, context)
            || bitcoin.chain() != coincube_core::chain::ChainId::Bitcoin
            || fork.chain() != coincube_core::chain::ChainId::BitcoinBlake2b
            || bitcoin.descriptor() != inventory.descriptor()
            || fork.descriptor() != inventory.descriptor()
            || bitcoin.change_index()
                != (ChildNumber::Normal {
                    index: change.bitcoin,
                })
            || fork.change_index() != (ChildNumber::Normal { index: change.fork })
            || bitcoin.poison_input() != bitcoin_path.selected()
            || funding != fork_path.selected()
            || bitcoin.poison_derivation()
                != (
                    ChildNumber::Normal {
                        index: self.indices.bitcoin_deposit,
                    },
                    false,
                )
            || unsigned_bitcoin != bitcoin.psbt().unsigned_tx
        {
            return Err(ClaimPostBitcoinError::Changed);
        }
        let saved_descriptor = inventory.descriptor().clone();
        let saved_identity = identity.clone();
        let saved_context = context.clone();
        let saved_directory = directory.clone();
        let saved = tokio::task::spawn_blocking(move || {
            load_claim_funding(
                &saved_directory,
                &saved_descriptor,
                &saved_identity,
                &saved_context,
            )
        })
        .await
        .map_err(|_| ClaimPostBitcoinError::Changed)?
        .map_err(|_| ClaimPostBitcoinError::Changed)?;
        if saved.as_ref() != Some(self) {
            return Err(ClaimPostBitcoinError::Changed);
        }
        let collected = source
            .collect_post_bitcoin_inputs(
                bitcoin_path,
                fork_path,
                bitcoin.claimed_prevouts(),
                signed_bitcoin,
                policy,
            )
            .await
            .map_err(ClaimPostBitcoinError::Observation)?;
        let (bitcoin_tip, fork_tip) = tokio::try_join!(
            source.tip(coincube_core::chain::ChainId::Bitcoin),
            source.tip(coincube_core::chain::ChainId::BitcoinBlake2b),
        )
        .map_err(ClaimPostBitcoinError::Observation)?;
        let tips = PreflightTips {
            bitcoin: *bitcoin_tip.value(),
            fork: *fork_tip.value(),
        };
        let scan = inventory.scan_report();
        if scan.bitcoin.tip() != tips.bitcoin.hash
            || u64::from(scan.bitcoin.tip_height()) != tips.bitcoin.height
            || scan.btcb2.tip() != tips.fork.hash
            || u64::from(scan.btcb2.tip_height()) != tips.fork.height
            || scan.bitcoin.generation() != context.generation
            || scan.btcb2.generation() != context.generation
        {
            return Err(ClaimPostBitcoinError::Changed);
        }
        collected
            .validate_for_fork_psbt(
                signed_bitcoin,
                fork.psbt(),
                bitcoin.claimed_prevouts(),
                FundingContext {
                    provider: &context.provider,
                    generation: context.generation,
                    now: source.now(),
                    max_age_seconds: policy.max_observation_age_seconds,
                    tips,
                },
            )
            .map_err(ClaimPostBitcoinError::Observation)?;
        inventory
            .reconcile_post_bitcoin_fork_daemon(
                fork_daemon,
                bitcoin.claimed_prevouts(),
                funding,
                self.indices.btcb2_deposit,
            )
            .await
            .map_err(ClaimPostBitcoinError::Wallet)?;
        let saved_descriptor = inventory.descriptor().clone();
        let saved_identity = identity.clone();
        let saved_context = context.clone();
        let current = tokio::task::spawn_blocking(move || {
            load_claim_funding(
                &directory,
                &saved_descriptor,
                &saved_identity,
                &saved_context,
            )
        })
        .await
        .map_err(|_| ClaimPostBitcoinError::Changed)?
        .map_err(|_| ClaimPostBitcoinError::Changed)?;
        if current.as_ref() != Some(self) {
            return Err(ClaimPostBitcoinError::Changed);
        }
        let (last_bitcoin, last_fork) = tokio::try_join!(
            source.tip(coincube_core::chain::ChainId::Bitcoin),
            source.tip(coincube_core::chain::ChainId::BitcoinBlake2b),
        )
        .map_err(ClaimPostBitcoinError::Observation)?;
        if last_bitcoin.value() != &tips.bitcoin || last_fork.value() != &tips.fork {
            return Err(ClaimPostBitcoinError::Changed);
        }
        source
            .validate_context(&context.provider, context.generation)
            .map_err(ClaimPostBitcoinError::Observation)?;
        collected
            .validate_for_fork_psbt(
                signed_bitcoin,
                fork.psbt(),
                bitcoin.claimed_prevouts(),
                FundingContext {
                    provider: &context.provider,
                    generation: context.generation,
                    now: source.now(),
                    max_age_seconds: policy.max_observation_age_seconds,
                    tips,
                },
            )
            .map_err(ClaimPostBitcoinError::Observation)?;
        Ok(RequalifiedPostBitcoinClaim {
            observed: collected,
        })
    }

    pub(super) fn matches_pair(
        &self,
        identity: &WalletIdentity,
        context: &Context,
        bitcoin: &super::ancestry::StoredAncestry,
        fork: &super::ancestry::StoredAncestry,
        change: ClaimChangeIndices,
    ) -> bool {
        self.released
            && self.change == Some(change)
            && self.identity == *identity
            && self.account == context.account
            && self.provider == context.provider
            && bitcoin.derivation.as_ref().is_some_and(|derivation| {
                !derivation.is_change && derivation.index == self.indices.bitcoin_deposit
            })
            && fork.derivation.as_ref().is_some_and(|derivation| {
                !derivation.is_change && derivation.index == self.indices.btcb2_deposit
            })
    }
}

/// Pin both deposit addresses before they can be shown. A later scan cannot
/// silently rotate them. A fresh address-history check and wallet-state
/// reconciliation are separate prerequisites to displaying or spending.
pub fn reserve_claim_funding(
    directory: &Path,
    descriptor: &CoincubeDescriptor,
    identity: &WalletIdentity,
    context: &Context,
    inventory: &ClaimInventory,
) -> Result<ClaimFundingReservation, Error> {
    if inventory.descriptor() != descriptor
        || inventory.scan_report().bitcoin.generation() != context.generation
        || inventory.scan_report().btcb2.generation() != context.generation
    {
        return Err(Error::InvalidPlan);
    }
    super::prepare_directory(directory)?;
    let mut journal = Journal::open(directory)?;
    if journal.load()?.is_some()
        || journal.load_split_funding_reservation()?.is_some()
        || std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok()
    {
        return Err(Error::Conflict);
    }
    if let Some(saved) = journal.load_claim_funding_reservation()? {
        return if saved.matches(descriptor, identity, context) {
            Ok(saved)
        } else {
            Err(Error::WrongIdentity)
        };
    }
    let indices = inventory
        .fresh_deposit_addresses()
        .ok_or(Error::InvalidPlan)?;
    let requested = ClaimFundingReservation::new(descriptor, identity, context, indices)?;
    journal.store_claim_funding_reservation(&requested)?;
    Ok(requested)
}

/// Reload the same addresses under the exact Vault and Connect identity.
/// This does not assert that either address is still unused.
pub fn load_claim_funding(
    directory: &Path,
    descriptor: &CoincubeDescriptor,
    identity: &WalletIdentity,
    context: &Context,
) -> Result<Option<ClaimFundingReservation>, Error> {
    let journal = Journal::open(directory)?;
    if journal.load_split_funding_reservation()?.is_some()
        || std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok()
    {
        return Err(Error::Conflict);
    }
    match journal.load_claim_funding_reservation()? {
        Some(saved) if saved.matches(descriptor, identity, context) => Ok(Some(saved)),
        Some(_) => Err(Error::WrongIdentity),
        None => Ok(None),
    }
}

/// Ask each wallet backend to atomically advance its own change branch, then
/// bind the resulting two indices to the released deposit record under the
/// Claim journal lock. An index consumed before an error is deliberately
/// burned. Reopening a completed reservation checks that both backends still
/// report the bound chain, descriptor and at least the recorded indices.
/// This does not prove destination address history or authorize signing.
#[allow(clippy::too_many_arguments)]
pub async fn reserve_claim_change_indices(
    directory: PathBuf,
    descriptor: &CoincubeDescriptor,
    identity: &WalletIdentity,
    context: &Context,
    bitcoin_daemon: &(dyn Daemon + Send + Sync),
    fork_daemon: &(dyn Daemon + Send + Sync),
    generation: &watch::Receiver<u64>,
    fork_generation: &watch::Receiver<u64>,
    fork_expected: u64,
) -> Result<ClaimChangeIndices, ClaimChangeError> {
    use coincube_core::chain::ChainId;
    let sessions_current =
        || current(generation, context.generation) && current(fork_generation, fork_expected);
    if !sessions_current() {
        return Err(ClaimChangeError::Cancelled);
    }
    let descriptor = descriptor.clone();
    let identity = identity.clone();
    let context = context.clone();
    let saved = {
        let directory = directory.clone();
        let descriptor = descriptor.clone();
        let identity = identity.clone();
        let context = context.clone();
        tokio::task::spawn_blocking(move || {
            let journal = Journal::open(&directory)?;
            if journal.load()?.is_some()
                || journal.load_split_funding_reservation()?.is_some()
                || std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok()
            {
                return Err(Error::Conflict);
            }
            let saved = journal
                .load_claim_funding_reservation()?
                .ok_or(Error::InvalidPlan)?;
            if !saved.matches(&descriptor, &identity, &context) {
                return Err(Error::WrongIdentity);
            }
            Ok(saved)
        })
        .await
        .map_err(|_| ClaimChangeError::Journal(Error::InvalidJournal))?
        .map_err(ClaimChangeError::Journal)?
    };
    if !saved.is_released() {
        return Err(ClaimChangeError::Journal(Error::InvalidPlan));
    }
    if !sessions_current() {
        return Err(ClaimChangeError::Cancelled);
    }
    let (bitcoin_info, fork_info) =
        tokio::try_join!(bitcoin_daemon.get_info(), fork_daemon.get_info(),)
            .map_err(|_| ClaimChangeError::WalletState)?;
    if !sessions_current() {
        return Err(ClaimChangeError::Cancelled);
    }
    if !change_wallet_ready(&bitcoin_info, ChainId::Bitcoin, &descriptor, None)
        || !change_wallet_ready(&fork_info, ChainId::BitcoinBlake2b, &descriptor, None)
    {
        return Err(ClaimChangeError::WalletState);
    }
    if let Some(change) = saved.change_indices() {
        return if change_wallet_ready(
            &bitcoin_info,
            ChainId::Bitcoin,
            &descriptor,
            Some(change.bitcoin),
        ) && change_wallet_ready(
            &fork_info,
            ChainId::BitcoinBlake2b,
            &descriptor,
            Some(change.fork),
        ) {
            Ok(change)
        } else {
            Err(ClaimChangeError::WalletState)
        };
    }

    if !sessions_current() {
        return Err(ClaimChangeError::Cancelled);
    }
    let bitcoin = bitcoin_daemon
        .reserve_claim_change(ChainId::Bitcoin, &descriptor)
        .await
        .map_err(|_| ClaimChangeError::WalletState)?;
    if !sessions_current() {
        return Err(ClaimChangeError::Cancelled);
    }
    let mut fork = fork_daemon
        .reserve_claim_change(ChainId::BitcoinBlake2b, &descriptor)
        .await
        .map_err(|_| ClaimChangeError::WalletState)?;
    if !sessions_current() {
        return Err(ClaimChangeError::Cancelled);
    }
    if bitcoin == fork {
        fork = fork_daemon
            .reserve_claim_change(ChainId::BitcoinBlake2b, &descriptor)
            .await
            .map_err(|_| ClaimChangeError::WalletState)?;
        if !sessions_current() {
            return Err(ClaimChangeError::Cancelled);
        }
    }
    if bitcoin.is_hardened() || fork.is_hardened() || bitcoin == fork {
        return Err(ClaimChangeError::WalletState);
    }
    let change = ClaimChangeIndices {
        bitcoin: u32::from(bitcoin),
        fork: u32::from(fork),
    };
    let (bitcoin_info, fork_info) =
        tokio::try_join!(bitcoin_daemon.get_info(), fork_daemon.get_info(),)
            .map_err(|_| ClaimChangeError::WalletState)?;
    if !sessions_current() {
        return Err(ClaimChangeError::Cancelled);
    }
    if !change_wallet_ready(
        &bitcoin_info,
        ChainId::Bitcoin,
        &descriptor,
        Some(change.bitcoin),
    ) || !change_wallet_ready(
        &fork_info,
        ChainId::BitcoinBlake2b,
        &descriptor,
        Some(change.fork),
    ) {
        return Err(ClaimChangeError::WalletState);
    }
    let generation = generation.clone();
    let fork_generation = fork_generation.clone();
    let expected = context.generation;
    tokio::task::spawn_blocking(move || {
        let mut journal = Journal::open(&directory).map_err(ClaimChangeError::Journal)?;
        if !current(&generation, expected) || !current(&fork_generation, fork_expected) {
            return Err(ClaimChangeError::Cancelled);
        }
        journal
            .record_claim_change_indices(&saved, change)
            .map_err(ClaimChangeError::Journal)?;
        Ok(change)
    })
    .await
    .map_err(|_| ClaimChangeError::Journal(Error::InvalidJournal))?
}

/// Release saved deposit addresses for display only after authenticated
/// identity and fresh Bitcoin/BTCB2 address histories still show them unused.
/// A successful result is not a proof that either later deposit is exclusive.
#[allow(clippy::too_many_arguments)]
async fn check_reserved_claim_addresses(
    directory: PathBuf,
    descriptor: CoincubeDescriptor,
    identity: WalletIdentity,
    context: Context,
    client: CoincubeClient,
    generation: watch::Receiver<u64>,
) -> Result<CheckedClaimAddresses, ClaimAddressError> {
    if !current(&generation, context.generation)
        || client.token().is_none_or(str::is_empty)
        || client.base_url.trim_end_matches('/') != context.provider.trim_end_matches('/')
    {
        return Err(ClaimAddressError::ChangedSession);
    }
    let expected = context.generation;
    let saved = tokio::task::spawn_blocking(move || {
        load_claim_funding(&directory, &descriptor, &identity, &context)
    })
    .await
    .map_err(|_| ClaimAddressError::Unavailable)?
    .map_err(|error| match error {
        Error::WrongIdentity => ClaimAddressError::WrongIdentity,
        _ => ClaimAddressError::InvalidReservation,
    })?
    .ok_or(ClaimAddressError::MissingReservation)?;
    let addresses = saved
        .addresses()
        .map_err(|_| ClaimAddressError::InvalidReservation)?;
    let esplora = ConnectEsplora::new(
        &client,
        CollectionContext {
            expected_generation: expected,
            generation: generation.clone(),
        },
    )
    .map_err(|_| ClaimAddressError::Unavailable)?;
    checked_history(&esplora, addresses, &generation, expected, true).await
}

/// Prepare the Bitcoin Vault to notice deposits at both reserved receive
/// indices, then release the already checked instructions. Revealing an index
/// does not authorize a spend or assert chain-exclusive ancestry.
#[allow(clippy::too_many_arguments)]
pub async fn prepare_claim_deposit_instructions(
    directory: PathBuf,
    descriptor: CoincubeDescriptor,
    identity: WalletIdentity,
    context: Context,
    client: CoincubeClient,
    generation: watch::Receiver<u64>,
    daemon: &(dyn Daemon + Send + Sync),
    shared_count: usize,
) -> Result<CheckedClaimFundingAddresses, ClaimAddressError> {
    let expected = context.generation;
    if !current(&generation, expected)
        || client.token().is_none_or(str::is_empty)
        || client.base_url.trim_end_matches('/') != context.provider.trim_end_matches('/')
    {
        return Err(ClaimAddressError::ChangedSession);
    }
    let for_load = (
        directory.clone(),
        descriptor.clone(),
        identity.clone(),
        context.clone(),
    );
    let saved = tokio::task::spawn_blocking(move || {
        let (directory, descriptor, identity, context) = for_load;
        load_claim_funding(&directory, &descriptor, &identity, &context)
    })
    .await
    .map_err(|_| ClaimAddressError::Unavailable)?
    .map_err(|_| ClaimAddressError::InvalidReservation)?
    .ok_or(ClaimAddressError::MissingReservation)?;
    let addresses = saved
        .addresses()
        .map_err(|_| ClaimAddressError::InvalidReservation)?;
    let checked = if saved.is_released() {
        // Once a deposit arrives, nonempty address history is expected. The
        // durable release records that the initial unused check succeeded.
        // Read both chains again to authenticate the current Connect session
        // and address history, without treating deposits as a new-address
        // violation or as spend authority.
        let esplora = ConnectEsplora::new(
            &client,
            CollectionContext {
                expected_generation: expected,
                generation: generation.clone(),
            },
        )
        .map_err(|_| ClaimAddressError::Unavailable)?;
        checked_history(&esplora, addresses.clone(), &generation, expected, false).await?
    } else {
        let checked = check_reserved_claim_addresses(
            directory.clone(),
            descriptor.clone(),
            identity,
            context,
            client.clone(),
            generation.clone(),
        )
        .await?;
        if addresses
            != [
                checked.bitcoin_deposit.clone(),
                checked.btcb2_deposit.clone(),
            ]
        {
            return Err(ClaimAddressError::InvalidReservation);
        }
        checked
    };
    let bitcoin_fees = ConnectBitcoinFees::new(&client).ok_or(ClaimAddressError::FeeUnavailable)?;
    let bitcoin_rate = bitcoin_step1_feerate(&bitcoin_fees)
        .await
        .ok_or(ClaimAddressError::FeeUnavailable)?;
    let btcb2_fees = btcb2_fee_source(Some(client.clone()));
    let btcb2_rate = btcb2_sweep_feerate(&*btcb2_fees)
        .await
        .ok_or(ClaimAddressError::FeeUnavailable)?;
    if !current(&generation, expected) {
        return Err(ClaimAddressError::ChangedSession);
    }
    let (bitcoin_minimum_sats, btcb2_minimum_sats) = minimum_claim_deposits(
        &descriptor,
        saved.indices,
        shared_count,
        bitcoin_rate,
        btcb2_rate,
    )?;
    let quoted = CheckedClaimFundingAddresses {
        bitcoin_deposit: checked.bitcoin_deposit,
        btcb2_deposit: checked.btcb2_deposit,
        bitcoin_minimum_sats,
        btcb2_minimum_sats,
        bitcoin_rate_sat_vb: bitcoin_rate,
        btcb2_rate_sat_vb: btcb2_rate,
    };
    reveal_claim_receive_addresses(daemon, &saved, &generation, expected).await?;
    if !current(&generation, expected) {
        return Err(ClaimAddressError::ChangedSession);
    }
    if !saved.is_released() {
        // An address could acquire history while a remote wallet is advancing
        // its receive branch. Do not make that earlier unused check durable.
        let esplora = ConnectEsplora::new(
            &client,
            CollectionContext {
                expected_generation: expected,
                generation: generation.clone(),
            },
        )
        .map_err(|_| ClaimAddressError::Unavailable)?;
        checked_history(&esplora, addresses, &generation, expected, true).await?;
        tokio::task::spawn_blocking(move || {
            let mut journal = Journal::open(&directory)?;
            journal.release_claim_funding_reservation(&saved)
        })
        .await
        .map_err(|_| ClaimAddressError::Unavailable)?
        .map_err(|_| ClaimAddressError::InvalidReservation)?;
    }
    if !current(&generation, expected) {
        return Err(ClaimAddressError::ChangedSession);
    }
    Ok(quoted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        daemon::{DaemonBackend, DaemonError},
        dir::CoincubeDirectory,
    };
    use coincube_core::miniscript::bitcoin::{address, psbt::Psbt, Address, Txid};
    use coincube_core::{
        bip39::Mnemonic,
        chain::ChainId,
        descriptors::{CoincubePolicy, PathInfo},
        miniscript::{
            bitcoin::{bip32::DerivationPath, secp256k1, Amount, Network, OutPoint},
            DescriptorPublicKey,
        },
        signer::MasterSigner,
    };
    use coincubed::{
        bip329::Labels,
        commands::{CoinStatus, LabelItem, UpdateDerivIndexesResult},
    };
    use std::{
        collections::{HashMap, HashSet},
        sync::Mutex,
    };

    #[test]
    fn reserved_funding_rejects_other_indices_and_change_outputs() {
        let coin = CandidateCoin {
            outpoint: OutPoint::null(),
            amount: Amount::from_sat(10_000),
            deriv_index: ChildNumber::Normal { index: 4 },
            is_change: false,
            must_select: true,
            sequence: None,
            ancestor_info: None,
        };
        let at_five = CandidateCoin {
            deriv_index: ChildNumber::Normal { index: 5 },
            ..coin
        };
        let change_at_four = CandidateCoin {
            is_change: true,
            ..coin
        };
        let (bitcoin, fork) = select_reserved_candidates(
            &[coin, at_five, change_at_four],
            &[coin, at_five],
            FreshDepositAddresses {
                bitcoin_deposit: 4,
                btcb2_deposit: 5,
            },
        );
        assert_eq!(bitcoin.len(), 1);
        assert_eq!(bitcoin[0].deriv_index, ChildNumber::Normal { index: 4 });
        assert_eq!(fork.len(), 1);
        assert_eq!(fork[0].deriv_index, ChildNumber::Normal { index: 5 });
        let small = CandidateCoin {
            amount: Amount::from_sat(500),
            ..coin
        };
        assert_eq!(
            fee_eligible_candidates(vec![small, coin], 1_000),
            vec![coin]
        );
        let second_deposit = CandidateCoin {
            outpoint: OutPoint::new(Txid::all_zeros(), 1),
            ..coin
        };
        assert_eq!(
            eligible_selected_candidates(vec![coin, second_deposit], 1_000, Some(coin.outpoint)),
            vec![coin],
            "a second coin at the same reserved address cannot replace the signed pair's funding input"
        );
        assert!(eligible_selected_candidates(vec![coin], 20_000, Some(coin.outpoint)).is_empty());
    }

    struct FakeHistory {
        stamp: i64,
        used_on: Option<ChainId>,
        calls: std::sync::Mutex<Vec<(ChainId, String)>>,
    }

    #[async_trait]
    impl AddressHistory for FakeHistory {
        async fn used(&self, chain: ChainId, address: &str) -> Result<(bool, i64), FailureKind> {
            self.calls.lock().unwrap().push((chain, address.into()));
            Ok((self.used_on == Some(chain), self.stamp))
        }
    }

    #[tokio::test]
    async fn deposit_addresses_require_fresh_unused_history_on_both_chains() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let (sender, generation) = watch::channel(7);
        let make = |stamp, used_on| FakeHistory {
            stamp,
            used_on,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let addresses = || ["bitcoin-address".into(), "btcb2-address".into()];
        let clean = make(now, None);
        let checked = checked_history(&clean, addresses(), &generation, 7, true)
            .await
            .unwrap();
        assert_eq!(checked.bitcoin_deposit, "bitcoin-address");
        assert_eq!(checked.btcb2_deposit, "btcb2-address");
        assert_eq!(clean.calls.lock().unwrap().len(), 4);
        assert_eq!(
            checked_history(
                &make(now, Some(ChainId::BitcoinBlake2b)),
                addresses(),
                &generation,
                7,
                true,
            )
            .await,
            Err(ClaimAddressError::Used)
        );
        assert!(checked_history(
            &make(now, Some(ChainId::BitcoinBlake2b)),
            addresses(),
            &generation,
            7,
            false,
        )
        .await
        .is_ok());
        assert_eq!(
            checked_history(&make(now - 61, None), addresses(), &generation, 7, true).await,
            Err(ClaimAddressError::Stale)
        );
        sender.send(8).unwrap();
        assert_eq!(
            checked_history(&clean, addresses(), &generation, 7, true).await,
            Err(ClaimAddressError::ChangedSession)
        );
    }

    fn descriptor() -> CoincubeDescriptor {
        let secp = secp256k1::Secp256k1::new();
        let keys: Vec<_> = [61_u8, 62]
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
        CoincubeDescriptor::new(
            CoincubePolicy::new_legacy(
                PathInfo::Single(keys[0].clone()),
                std::iter::once((46, PathInfo::Single(keys[1].clone()))).collect(),
            )
            .unwrap(),
        )
    }

    #[derive(Debug)]
    struct ReceiveWallet {
        descriptor: CoincubeDescriptor,
        backend: DaemonBackend,
        receive: Mutex<u32>,
        wrong_generated_address: bool,
        omit_revealed_address: bool,
    }

    impl ReceiveWallet {
        fn new(descriptor: CoincubeDescriptor, backend: DaemonBackend) -> Self {
            Self {
                descriptor,
                backend,
                receive: Mutex::new(3),
                wrong_generated_address: false,
                omit_revealed_address: false,
            }
        }

        fn address(&self, index: u32) -> Address {
            self.descriptor
                .receive_descriptor()
                .derive(
                    ChildNumber::Normal { index },
                    &secp256k1::Secp256k1::verification_only(),
                )
                .address(Network::Bitcoin)
        }
    }

    #[async_trait]
    impl Daemon for ReceiveWallet {
        fn backend(&self) -> DaemonBackend {
            self.backend.clone()
        }

        fn config(&self) -> Option<&coincubed::config::Config> {
            None
        }

        async fn is_alive(&self, _: &CoincubeDirectory, _: Network) -> Result<(), DaemonError> {
            Ok(())
        }

        async fn stop(&self) -> Result<(), DaemonError> {
            Ok(())
        }

        async fn get_info(&self) -> Result<model::GetInfoResult, DaemonError> {
            Ok(model::GetInfoResult {
                version: String::new(),
                network: Network::Bitcoin,
                chain_id: Some(ChainId::Bitcoin),
                block_height: 100,
                sync: 1.0,
                descriptors: coincubed::commands::GetInfoDescriptors {
                    main: self.descriptor.clone(),
                },
                rescan_progress: None,
                refused_reorg_depth: None,
                chain_divergence: false,
                timestamp: 0,
                last_poll_timestamp: None,
                history_sync: Default::default(),
                receive_index: *self.receive.lock().unwrap(),
                change_index: 0,
            })
        }

        async fn request_sync(&self) -> Result<(), DaemonError> {
            Ok(())
        }

        async fn get_new_address(&self) -> Result<model::GetAddressResult, DaemonError> {
            let mut current = self.receive.lock().unwrap();
            *current += 1;
            let index = *current;
            Ok(model::GetAddressResult {
                address: self.address(if self.wrong_generated_address {
                    0
                } else {
                    index
                }),
                derivation_index: ChildNumber::Normal { index },
            })
        }

        async fn list_revealed_addresses(
            &self,
            _: bool,
            _: bool,
            _: usize,
            start_index: Option<ChildNumber>,
        ) -> Result<model::ListRevealedAddressesResult, DaemonError> {
            let index = match start_index {
                Some(ChildNumber::Normal { index }) => index,
                _ => return Err(DaemonError::NotImplemented),
            };
            let addresses = if index <= *self.receive.lock().unwrap() && !self.omit_revealed_address
            {
                vec![coincubed::commands::ListRevealedAddressesEntry {
                    index: ChildNumber::Normal { index },
                    address: self.address(index),
                    label: None,
                    used_count: 0,
                }]
            } else {
                Vec::new()
            };
            Ok(model::ListRevealedAddressesResult {
                addresses,
                continue_from: None,
            })
        }

        async fn update_deriv_indexes(
            &self,
            receive: Option<u32>,
            _: Option<u32>,
        ) -> Result<UpdateDerivIndexesResult, DaemonError> {
            if self.backend.is_remote() {
                return Err(DaemonError::NotImplemented);
            }
            let mut current = self.receive.lock().unwrap();
            *current = (*current).max(receive.unwrap_or(*current));
            Ok(UpdateDerivIndexesResult {
                receive: *current,
                change: 0,
            })
        }

        async fn list_coins(
            &self,
            _: &[CoinStatus],
            _: &[OutPoint],
        ) -> Result<model::ListCoinsResult, DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn list_spend_txs(&self) -> Result<model::ListSpendResult, DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn create_spend_tx(
            &self,
            _: &[OutPoint],
            _: &HashMap<Address<address::NetworkUnchecked>, u64>,
            _: u64,
            _: Option<Address<address::NetworkUnchecked>>,
        ) -> Result<model::CreateSpendResult, DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn rbf_psbt(
            &self,
            _: &Txid,
            _: bool,
            _: Option<u64>,
        ) -> Result<model::CreateSpendResult, DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn update_spend_tx(&self, _: &Psbt) -> Result<(), DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn delete_spend_tx(&self, _: &Txid) -> Result<(), DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn broadcast_spend_tx(&self, _: &Txid) -> Result<(), DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn start_rescan(&self, _: u32) -> Result<(), DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn list_confirmed_txs(
            &self,
            _: u32,
            _: u32,
            _: u64,
        ) -> Result<model::ListTransactionsResult, DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn create_recovery(
            &self,
            _: Address<address::NetworkUnchecked>,
            _: &[OutPoint],
            _: u64,
            _: Option<u16>,
        ) -> Result<Psbt, DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn list_txs(&self, _: &[Txid]) -> Result<model::ListTransactionsResult, DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn get_labels(
            &self,
            _: &HashSet<LabelItem>,
        ) -> Result<HashMap<String, String>, DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn update_labels(
            &self,
            _: &HashMap<LabelItem, Option<String>>,
        ) -> Result<(), DaemonError> {
            Err(DaemonError::NotImplemented)
        }

        async fn get_labels_bip329(&self, _: u32, _: u32) -> Result<Labels, DaemonError> {
            Err(DaemonError::NotImplemented)
        }
    }

    #[tokio::test]
    async fn connect_claim_reveals_and_reads_back_both_reserved_receive_addresses() {
        let descriptor = descriptor();
        let identity = WalletIdentity {
            bitcoin_cube: "bitcoin-cube".into(),
            fork_cube: "fork-cube".into(),
            descriptor_digest: sha256::Hash::hash(descriptor.to_string().as_bytes()),
        };
        let saved = ClaimFundingReservation::new(
            &descriptor,
            &identity,
            &Context {
                generation: 1,
                account: "account".into(),
                provider: "https://connect.example".into(),
            },
            FreshDepositAddresses {
                bitcoin_deposit: 4,
                btcb2_deposit: 5,
            },
        )
        .unwrap();
        let (sender, generation) = watch::channel(1);
        let remote = ReceiveWallet::new(descriptor.clone(), DaemonBackend::RemoteBackend);
        reveal_claim_receive_addresses(&remote, &saved, &generation, 1)
            .await
            .unwrap();
        assert_eq!(*remote.receive.lock().unwrap(), 5);

        let mut wrong = ReceiveWallet::new(descriptor.clone(), DaemonBackend::RemoteBackend);
        wrong.wrong_generated_address = true;
        assert_eq!(
            reveal_claim_receive_addresses(&wrong, &saved, &generation, 1).await,
            Err(ClaimAddressError::WalletState)
        );
        let mut missing = ReceiveWallet::new(descriptor.clone(), DaemonBackend::RemoteBackend);
        missing.omit_revealed_address = true;
        assert_eq!(
            reveal_claim_receive_addresses(&missing, &saved, &generation, 1).await,
            Err(ClaimAddressError::WalletState)
        );
        let embedded = ReceiveWallet::new(descriptor, DaemonBackend::EmbeddedCoincubed(None));
        reveal_claim_receive_addresses(&embedded, &saved, &generation, 1)
            .await
            .unwrap();
        assert_eq!(*embedded.receive.lock().unwrap(), 5);
        sender.send(2).unwrap();
        assert_eq!(
            reveal_claim_receive_addresses(&remote, &saved, &generation, 1).await,
            Err(ClaimAddressError::ChangedSession)
        );
    }

    #[test]
    fn deposit_instructions_require_the_matching_healthy_bitcoin_wallet() {
        let descriptor = descriptor();
        let identity = WalletIdentity {
            bitcoin_cube: "bitcoin-cube".into(),
            fork_cube: "fork-cube".into(),
            descriptor_digest: sha256::Hash::hash(descriptor.to_string().as_bytes()),
        };
        let saved = ClaimFundingReservation::new(
            &descriptor,
            &identity,
            &Context {
                generation: 1,
                account: "account".into(),
                provider: "https://connect.example".into(),
            },
            FreshDepositAddresses {
                bitcoin_deposit: 4,
                btcb2_deposit: 5,
            },
        )
        .unwrap();
        let mut info = model::GetInfoResult {
            version: "test".into(),
            network: Network::Bitcoin,
            chain_id: Some(ChainId::Bitcoin),
            block_height: 1,
            sync: 1.0,
            descriptors: coincubed::commands::GetInfoDescriptors {
                main: descriptor.clone(),
            },
            rescan_progress: None,
            refused_reorg_depth: None,
            chain_divergence: false,
            timestamp: 0,
            last_poll_timestamp: None,
            history_sync: Default::default(),
            receive_index: 0,
            change_index: 0,
        };
        assert_eq!(ensure_claim_wallet_ready(&info, &saved), Ok(()));
        info.chain_id = None;
        assert_eq!(
            ensure_claim_wallet_ready(&info, &saved),
            Err(ClaimAddressError::WalletState)
        );
        info.chain_id = Some(ChainId::BitcoinBlake2b);
        assert_eq!(
            ensure_claim_wallet_ready(&info, &saved),
            Err(ClaimAddressError::WalletState)
        );
        info.chain_id = Some(ChainId::Bitcoin);
        info.chain_divergence = true;
        assert_eq!(
            ensure_claim_wallet_ready(&info, &saved),
            Err(ClaimAddressError::WalletState)
        );
        info.chain_divergence = false;
        info.history_sync.full_scan_in_progress = true;
        assert_eq!(
            ensure_claim_wallet_ready(&info, &saved),
            Err(ClaimAddressError::WalletState)
        );
        info.history_sync.full_scan_in_progress = false;
        info.block_height = 0;
        assert_eq!(
            ensure_claim_wallet_ready(&info, &saved),
            Err(ClaimAddressError::WalletState)
        );
        info.block_height = 1;
        info.network = Network::Testnet;
        assert_eq!(
            ensure_claim_wallet_ready(&info, &saved),
            Err(ClaimAddressError::WalletState)
        );
    }

    #[test]
    fn claim_deposit_reservation_survives_restart_and_rejects_tampering() {
        let directory = std::env::temp_dir().join(format!(
            "claim-funding-{}-{}",
            std::process::id(),
            super::super::controller_id().unwrap()
        ));
        let descriptor = descriptor();
        let identity = WalletIdentity {
            bitcoin_cube: "bitcoin-cube".into(),
            fork_cube: "btcb2-cube".into(),
            descriptor_digest: sha256::Hash::hash(descriptor.to_string().as_bytes()),
        };
        let context = Context {
            generation: 1,
            account: "account-a".into(),
            provider: "https://connect.example".into(),
        };
        let indices = FreshDepositAddresses {
            bitcoin_deposit: 4,
            btcb2_deposit: 5,
        };
        let saved =
            ClaimFundingReservation::new(&descriptor, &identity, &context, indices).unwrap();
        let mut old = serde_json::to_value(&saved).unwrap();
        old.as_object_mut().unwrap().remove("released");
        old["version"] = serde_json::json!(1);
        let old: ClaimFundingReservation = serde_json::from_value(old).unwrap();
        assert!(!old.is_released());
        assert_eq!(old.indices, saved.indices);
        let addresses = saved.addresses().unwrap();
        assert_ne!(addresses[0], addresses[1]);
        super::super::prepare_directory(&directory).unwrap();
        let recorded = {
            let mut journal = Journal::open(&directory).unwrap();
            journal.store_claim_funding_reservation(&saved).unwrap();
            let released = journal.release_claim_funding_reservation(&saved).unwrap();
            assert!(released.is_released());
            assert_eq!(released.addresses().unwrap(), addresses);
            assert_eq!(
                journal
                    .release_claim_funding_reservation(&released)
                    .unwrap(),
                released
            );
            assert!(matches!(
                journal.release_claim_funding_reservation(&saved),
                Err(Error::Conflict)
            ));
            let change = ClaimChangeIndices {
                bitcoin: 8,
                fork: 9,
            };
            let recorded = journal
                .record_claim_change_indices(&released, change)
                .unwrap();
            assert_eq!(recorded.change_indices(), Some(change));
            assert_eq!(recorded.version, 3);
            assert_eq!(
                journal
                    .record_claim_change_indices(&recorded, change)
                    .unwrap(),
                recorded
            );
            assert!(matches!(
                journal.record_claim_change_indices(
                    &released,
                    ClaimChangeIndices {
                        bitcoin: 8,
                        fork: 10,
                    },
                ),
                Err(Error::Conflict)
            ));
            recorded
        };
        let mut restarted = context.clone();
        restarted.generation += 1;
        assert_eq!(
            load_claim_funding(&directory, &descriptor, &identity, &restarted)
                .unwrap()
                .unwrap(),
            recorded
        );
        let mut other = identity.clone();
        other.fork_cube = "other-cube".into();
        assert!(matches!(
            load_claim_funding(&directory, &descriptor, &other, &context),
            Err(Error::WrongIdentity)
        ));
        let mut bad = saved.clone();
        bad.indices.btcb2_deposit = bad.indices.bitcoin_deposit;
        assert!(matches!(bad.validate(), Err(Error::InvalidPlan)));
        for change in [
            ClaimChangeIndices {
                bitcoin: 8,
                fork: 8,
            },
            ClaimChangeIndices {
                bitcoin: 1 << 31,
                fork: 9,
            },
        ] {
            assert!(matches!(
                saved.released_copy().with_change_indices(change),
                Err(Error::InvalidPlan)
            ));
        }
        std::fs::write(directory.join("claim-funding.json"), b"{}").unwrap();
        assert!(matches!(
            load_claim_funding(&directory, &descriptor, &identity, &context),
            Err(Error::InvalidJournal)
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
