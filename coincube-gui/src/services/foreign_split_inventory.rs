//! Two-chain inventory of a foreign (non-Cube) wallet for the Split tool.
//!
//! Scans the same public descriptors on Bitcoin and on Bitcoin Blake2b with the
//! bounded foreign scanner and joins the results by outpoint. The result is
//! evidence for display and for later slices; it is not signing, poison or
//! spend authority. Both chains are read through Connect's Esplora proxy by the
//! scanner's anonymous HTTP client, so no account credential reaches an Esplora
//! route (#542). Only the BTCB2 fork-height observation uses the account.
//!
//! The one exception to "display only" is [`SplitInventory::splittable_coins`]:
//! the pre-fork coins present, unspent and identically confirmed on both
//! chains, carried as `coincube_core::foreign_split::SplitCoin` inputs for
//! Split step 1 construction. They are scan evidence of the moment, not proof
//! that a coin is still unspent when a transaction is built or broadcast; a
//! restart re-authenticates through `authenticate_outpoints` in `split_evidence`.
//!
//! Absence on the other chain is display evidence only. In particular a
//! Bitcoin output confirmed after the fork is at most a *candidate* input
//! poison: `coincube_core::claim` states that an absent txid or a post-fork
//! height is never poison proof, and `PoisonCandidate` deliberately carries
//! nothing a transaction builder could spend.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
};

use coincube_core::{
    chain::ChainId,
    claim::{BlockRef, PreflightTips},
    foreign_split::{SplitBranch, SplitCoin, SplitSource},
    miniscript::bitcoin::{BlockHash, OutPoint},
};
use tokio::sync::watch;

use super::{
    coincube::CoincubeClient,
    foreign_scan::{
        self, Branch, BranchRange, DiscoveredCoin, ForkSide, ScanError, ScanPlan, ScanReport,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryError {
    /// Either chain's bounded scan did not complete: never read as zero funds.
    Scan(ChainId, ScanError),
    /// The plan was not a BTCB2 plan, or a report came from the wrong chain.
    WrongChain,
    /// A report belongs to another scan generation (cancelled or superseded).
    Stale,
    /// The authenticated BTCB2 observation carried no active fork height.
    ForkHeightUnknown,
    /// The same outpoint has a different scriptPubKey or amount on each chain.
    PrevoutMismatch(OutPoint),
    /// A pre-fork outpoint whose shared confirming block, or derivation,
    /// differs between the chains.
    Inconsistent(OutPoint),
    /// An outpoint's address lies outside the other chain's proven walk, so
    /// its absence there says nothing.
    Coverage(ChainId, OutPoint),
}

/// Display row. It carries no scriptPubKey, previous transaction or `TxOut`:
/// no prevout data a transaction builder needs. That is the only barrier; the
/// outpoint could still be looked up again, so this type grants nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventoryCoin {
    pub outpoint: OutPoint,
    pub sats: u64,
    pub block_height: Option<u32>,
    pub branch: Branch,
    pub index: u32,
}

impl InventoryCoin {
    fn of(coin: &DiscoveredCoin) -> Self {
        Self {
            outpoint: coin.outpoint,
            sats: coin.output.value.to_sat(),
            block_height: coin.block_height,
            branch: coin.branch,
            index: coin.index,
        }
    }
}

/// An output confirmed at or after the observed fork height and seen only on
/// one chain within both scans' address coverage. This is a display hint,
/// never proof that the transaction cannot be replayed onto the other chain.
/// It carries no prevout data, and cannot be used for transaction construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoisonCandidate(InventoryCoin);

impl PoisonCandidate {
    pub fn display(&self) -> &InventoryCoin {
        &self.0
    }
}

/// A receive index whose address had no chain or mempool history on either
/// chain inside both proven walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshIndex {
    Proven(u32),
    /// A fixed (non-wildcard) descriptor has only one address: no fresh one.
    FixedDescriptor,
    /// The walks do not jointly prove any index unused.
    NotProven,
}

/// Three distinct addresses proved unused inside both complete source-wallet
/// scans. These indices are hints until a durable wizard record reserves them
/// and Connect rechecks their history immediately before showing deposits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FreshFundingAddresses {
    pub bitcoin_deposit: u32,
    pub btcb2_deposit: u32,
    pub bitcoin_return: u32,
}

/// Two distinct receive indices unused on both chains within the complete
/// bounded scans. Claim must persist and recheck these before displaying them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FreshDepositAddresses {
    pub bitcoin_deposit: u32,
    pub btcb2_deposit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitInventory {
    generation: u64,
    fork_height: u64,
    btcb2_tip: BlockHash,
    bitcoin_tip: BlockHash,
    btcb2_tip_height: u32,
    bitcoin_tip_height: u32,
    splittable: Vec<InventoryCoin>,
    /// Same coins and order as `splittable`, with step-1 inputs.
    splittable_coins: Vec<SplitCoin>,
    spent_on_bitcoin: Vec<InventoryCoin>,
    spent_on_btcb2: Vec<InventoryCoin>,
    btcb2_post_fork: Vec<InventoryCoin>,
    bitcoin_only_post_fork: Vec<PoisonCandidate>,
    btcb2_only_post_fork: Vec<PoisonCandidate>,
    /// Previous transactions retained for later live proof binding. These are
    /// scan-time candidates, never authorization to spend.
    bitcoin_funding_candidates: BTreeMap<OutPoint, SplitCoin>,
    btcb2_funding_candidates: BTreeMap<OutPoint, SplitCoin>,
    pending: Vec<InventoryCoin>,
    fresh_receive: FreshIndex,
    fresh_funding_addresses: Option<FreshFundingAddresses>,
    fresh_deposit_addresses: Option<FreshDepositAddresses>,
}

impl SplitInventory {
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// From the BTCB2 scan's authenticated network anchor; never a constant.
    pub fn fork_height(&self) -> u64 {
        self.fork_height
    }
    pub fn btcb2_tip(&self) -> BlockHash {
        self.btcb2_tip
    }
    pub fn bitcoin_tip(&self) -> BlockHash {
        self.bitcoin_tip
    }
    /// Height of [`Self::btcb2_tip`].
    pub fn btcb2_tip_height(&self) -> u32 {
        self.btcb2_tip_height
    }
    /// Height of [`Self::bitcoin_tip`]; the step-1 locktime bound.
    pub fn bitcoin_tip_height(&self) -> u32 {
        self.bitcoin_tip_height
    }
    /// The splittable coins as step-1 inputs: the authenticated previous
    /// transaction and the confirming block on each chain, which are the same
    /// block below the fork height. Post-fork, pending, one-chain and
    /// disagreeing coins are never here. This is scan-time evidence, not
    /// proof the coins are still unspent.
    pub fn splittable_coins(&self) -> Vec<SplitCoin> {
        self.splittable_coins.clone()
    }
    /// Pre-fork outpoints unspent on both chains with identical prevouts and
    /// the same shared confirming block.
    pub fn splittable(&self) -> &[InventoryCoin] {
        &self.splittable
    }
    /// Pre-fork, unspent on BTCB2, absent from the Bitcoin UTXO set of a
    /// scanned address. Display only.
    pub fn spent_on_bitcoin(&self) -> &[InventoryCoin] {
        &self.spent_on_bitcoin
    }
    /// Pre-fork, unspent on Bitcoin, absent on BTCB2. Display only.
    pub fn spent_on_btcb2(&self) -> &[InventoryCoin] {
        &self.spent_on_btcb2
    }
    /// Confirmed on BTCB2 at or after the fork; never swept by Split.
    pub fn btcb2_post_fork(&self) -> &[InventoryCoin] {
        &self.btcb2_post_fork
    }
    /// Display-only input-poison candidates. Not poison proof.
    pub fn bitcoin_only_post_fork(&self) -> &[PoisonCandidate] {
        &self.bitcoin_only_post_fork
    }
    /// Display-only candidates seen on BTCB2 but not in Bitcoin's covered
    /// address range. Positive two-chain ancestry is still required.
    pub fn btcb2_only_post_fork(&self) -> &[PoisonCandidate] {
        &self.btcb2_only_post_fork
    }
    /// Candidate input for a later ancestry proof. The caller must collect a
    /// fresh, canonical two-chain funding proof and recheck ownership before
    /// constructing, signing or submitting either transaction.
    pub fn funding_candidate(&self, chain: ChainId, outpoint: OutPoint) -> Option<SplitCoin> {
        match chain {
            ChainId::Bitcoin => self.bitcoin_funding_candidates.get(&outpoint),
            ChainId::BitcoinBlake2b => self.btcb2_funding_candidates.get(&outpoint),
            _ => None,
        }
        .cloned()
    }
    /// All scan-time funding candidates in deterministic outpoint order.
    /// These remain hints; only the live paired collector can qualify one
    /// candidate on each chain for review.
    pub fn funding_candidates(&self, chain: ChainId) -> Vec<SplitCoin> {
        match chain {
            ChainId::Bitcoin => self.bitcoin_funding_candidates.values(),
            ChainId::BitcoinBlake2b => self.btcb2_funding_candidates.values(),
            _ => return Vec::new(),
        }
        .cloned()
        .collect()
    }
    /// Restrict a proof search to the two addresses durably reserved before
    /// the user sent deposits. A later scan can discover unrelated post-fork
    /// outputs, but none of them may silently become this split's marker.
    pub fn funding_at_reserved_addresses(&self, indices: FreshFundingAddresses) -> Self {
        let mut restricted = self.clone();
        restricted.bitcoin_funding_candidates.retain(|_, coin| {
            coin.branch == SplitBranch::External && coin.index == indices.bitcoin_deposit
        });
        restricted.btcb2_funding_candidates.retain(|_, coin| {
            coin.branch == SplitBranch::External && coin.index == indices.btcb2_deposit
        });
        restricted
    }
    /// Narrow an already reserved funding search to the exact two reviewed
    /// outpoints. A second deposit to either saved address must never replace
    /// a coin between review and signer admission.
    pub fn funding_at_outpoints(&self, bitcoin: OutPoint, btcb2: OutPoint) -> Self {
        let mut restricted = self.clone();
        restricted
            .bitcoin_funding_candidates
            .retain(|outpoint, _| *outpoint == bitcoin);
        restricted
            .btcb2_funding_candidates
            .retain(|outpoint, _| *outpoint == btcb2);
        restricted
    }
    /// Exclude deposits that cannot individually pay the estimated fee for
    /// their chain. A later eligible output to the same reserved address must
    /// remain selectable even if an earlier, smaller output sorts first.
    /// This is only candidate filtering; the paired live proof and exact
    /// transaction construction still decide whether the coin is usable.
    pub fn funding_covering_estimated_fees(
        &self,
        bitcoin_minimum_sats: u64,
        btcb2_minimum_sats: u64,
    ) -> Self {
        let mut eligible = self.clone();
        eligible.bitcoin_funding_candidates.retain(|_, coin| {
            coin.previous
                .output
                .get(coin.outpoint.vout as usize)
                .is_some_and(|output| output.value.to_sat() >= bitcoin_minimum_sats)
        });
        eligible.btcb2_funding_candidates.retain(|_, coin| {
            coin.previous
                .output
                .get(coin.outpoint.vout as usize)
                .is_some_and(|output| output.value.to_sat() >= btcb2_minimum_sats)
        });
        eligible
    }
    /// Unconfirmed on either chain, or present on both after the fork.
    pub fn pending(&self) -> &[InventoryCoin] {
        &self.pending
    }
    pub fn fresh_receive(&self) -> FreshIndex {
        self.fresh_receive
    }
    /// Read-only candidates for the two deposits and Bitcoin self-transfer.
    /// The distinct indexes prevent an address reused across chains from
    /// being mistaken for chain-specific funding evidence.
    pub fn fresh_funding_addresses(&self) -> Option<FreshFundingAddresses> {
        self.fresh_funding_addresses
    }
    pub fn fresh_deposit_addresses(&self) -> Option<FreshDepositAddresses> {
        self.fresh_deposit_addresses
    }

    /// Join two complete reports of the same generation and descriptors.
    pub fn join(
        btcb2: &ScanReport,
        bitcoin: &ScanReport,
        expected: u64,
        external_ranged: bool,
    ) -> Result<Self, InventoryError> {
        if btcb2.chain() != ChainId::BitcoinBlake2b || bitcoin.chain() != ChainId::Bitcoin {
            return Err(InventoryError::WrongChain);
        }
        if btcb2.generation() != expected || bitcoin.generation() != expected {
            return Err(InventoryError::Stale);
        }
        // Only the BTCB2 report carries the authenticated fork observation.
        // A Bitcoin report's field is ignored even if set.
        let fork = btcb2
            .fork_height()
            .ok_or(InventoryError::ForkHeightUnknown)?;
        let on_bitcoin: BTreeMap<OutPoint, &DiscoveredCoin> = bitcoin
            .coins()
            .iter()
            .map(|coin| (coin.outpoint, coin))
            .collect();
        let on_btcb2: BTreeSet<OutPoint> = btcb2.coins().iter().map(|c| c.outpoint).collect();
        let mut inventory = Self {
            generation: expected,
            fork_height: fork,
            btcb2_tip: btcb2.tip(),
            bitcoin_tip: bitcoin.tip(),
            btcb2_tip_height: btcb2.tip_height(),
            bitcoin_tip_height: bitcoin.tip_height(),
            splittable: Vec::new(),
            splittable_coins: Vec::new(),
            spent_on_bitcoin: Vec::new(),
            spent_on_btcb2: Vec::new(),
            btcb2_post_fork: Vec::new(),
            bitcoin_only_post_fork: Vec::new(),
            btcb2_only_post_fork: Vec::new(),
            bitcoin_funding_candidates: BTreeMap::new(),
            btcb2_funding_candidates: BTreeMap::new(),
            pending: Vec::new(),
            fresh_receive: fresh_receive(btcb2, bitcoin, external_ranged),
            fresh_deposit_addresses: fresh_receive_indices(btcb2, bitcoin, external_ranged, 2)
                .and_then(|indices| {
                    Some(FreshDepositAddresses {
                        bitcoin_deposit: *indices.first()?,
                        btcb2_deposit: *indices.get(1)?,
                    })
                }),
            fresh_funding_addresses: fresh_receive_indices(btcb2, bitcoin, external_ranged, 3)
                .and_then(|indices| {
                    Some(FreshFundingAddresses {
                        bitcoin_deposit: *indices.first()?,
                        btcb2_deposit: *indices.get(1)?,
                        bitcoin_return: *indices.get(2)?,
                    })
                }),
        };
        for coin in btcb2.coins() {
            let fork_side = btcb2.fork_side(coin);
            let Some(other) = on_bitcoin.get(&coin.outpoint) else {
                match fork_side {
                    ForkSide::PreFork => {
                        covered(bitcoin, coin)?;
                        inventory.spent_on_bitcoin.push(InventoryCoin::of(coin));
                    }
                    ForkSide::PostFork => {
                        inventory.btcb2_post_fork.push(InventoryCoin::of(coin));
                        if covered(bitcoin, coin).is_ok() {
                            inventory.btcb2_funding_candidates.insert(
                                coin.outpoint,
                                funding_coin(coin, ChainId::BitcoinBlake2b)?,
                            );
                            inventory
                                .btcb2_only_post_fork
                                .push(PoisonCandidate(InventoryCoin::of(coin)));
                        }
                    }
                    ForkSide::Unconfirmed | ForkSide::Unknown => {
                        inventory.pending.push(InventoryCoin::of(coin))
                    }
                }
                continue;
            };
            if other.output != coin.output {
                return Err(InventoryError::PrevoutMismatch(coin.outpoint));
            }
            if (other.branch, other.index) != (coin.branch, coin.index) {
                return Err(InventoryError::Inconsistent(coin.outpoint));
            }
            let other_side = side(fork, other);
            if fork_side == ForkSide::PreFork || other_side == ForkSide::PreFork {
                // Pre-fork history is shared: both chains must name the same block.
                if fork_side != other_side
                    || other.block_height != coin.block_height
                    || other.block_hash != coin.block_hash
                {
                    return Err(InventoryError::Inconsistent(coin.outpoint));
                }
                inventory.splittable_coins.push(split_coin(other, coin)?);
                inventory.splittable.push(InventoryCoin::of(coin));
            } else {
                // Unconfirmed, or replayed onto both chains after the fork.
                inventory.pending.push(InventoryCoin::of(coin));
            }
        }
        for coin in bitcoin.coins() {
            if on_btcb2.contains(&coin.outpoint) {
                continue;
            }
            match side(fork, coin) {
                ForkSide::PreFork => {
                    covered(btcb2, coin)?;
                    inventory.spent_on_btcb2.push(InventoryCoin::of(coin));
                }
                ForkSide::PostFork => {
                    if covered(btcb2, coin).is_ok() {
                        inventory
                            .bitcoin_funding_candidates
                            .insert(coin.outpoint, funding_coin(coin, ChainId::Bitcoin)?);
                        inventory
                            .bitcoin_only_post_fork
                            .push(PoisonCandidate(InventoryCoin::of(coin)));
                    }
                }
                ForkSide::Unconfirmed | ForkSide::Unknown => {
                    inventory.pending.push(InventoryCoin::of(coin))
                }
            }
        }
        Ok(inventory)
    }
}

fn funding_coin(coin: &DiscoveredCoin, chain: ChainId) -> Result<SplitCoin, InventoryError> {
    let (Some(height), Some(hash)) = (coin.block_height, coin.block_hash) else {
        return Err(InventoryError::Inconsistent(coin.outpoint));
    };
    if !coin.confirmed
        || coin.previous.compute_txid() != coin.outpoint.txid
        || coin.previous.output.get(coin.outpoint.vout as usize) != Some(&coin.output)
    {
        return Err(InventoryError::Inconsistent(coin.outpoint));
    }
    let block = BlockRef {
        height: u64::from(height),
        hash,
    };
    Ok(SplitCoin {
        outpoint: coin.outpoint,
        branch: match coin.branch {
            Branch::External => SplitBranch::External,
            Branch::Internal => SplitBranch::Internal,
        },
        index: coin.index,
        previous: coin.previous.clone(),
        bitcoin_block: (chain == ChainId::Bitcoin).then_some(block),
        btcb2_block: (chain == ChainId::BitcoinBlake2b).then_some(block),
    })
}

/// A shared pre-fork coin as a step-1 input. Both previous transactions are
/// txid-authenticated by the scanner and both confirmations were checked
/// equal by the caller; refuse anyway if either is missing.
fn split_coin(
    bitcoin: &DiscoveredCoin,
    btcb2: &DiscoveredCoin,
) -> Result<SplitCoin, InventoryError> {
    let block = |coin: &DiscoveredCoin| match (coin.confirmed, coin.block_height, coin.block_hash) {
        (true, Some(height), Some(hash)) => Ok(BlockRef {
            height: u64::from(height),
            hash,
        }),
        _ => Err(InventoryError::Inconsistent(coin.outpoint)),
    };
    let (bitcoin_block, btcb2_block) = (block(bitcoin)?, block(btcb2)?);
    if bitcoin_block != btcb2_block || btcb2.previous.compute_txid() != btcb2.outpoint.txid {
        return Err(InventoryError::Inconsistent(btcb2.outpoint));
    }
    Ok(SplitCoin {
        outpoint: btcb2.outpoint,
        branch: match btcb2.branch {
            Branch::External => SplitBranch::External,
            Branch::Internal => SplitBranch::Internal,
        },
        index: btcb2.index,
        previous: btcb2.previous.clone(),
        bitcoin_block: Some(bitcoin_block),
        btcb2_block: Some(btcb2_block),
    })
}

fn side(fork: u64, coin: &DiscoveredCoin) -> ForkSide {
    if !coin.confirmed {
        return ForkSide::Unconfirmed;
    }
    match (coin.block_height, coin.block_hash) {
        (Some(height), Some(_)) if u64::from(height) < fork => ForkSide::PreFork,
        (Some(_), Some(_)) => ForkSide::PostFork,
        _ => ForkSide::Unknown,
    }
}

fn covered(other: &ScanReport, coin: &DiscoveredCoin) -> Result<(), InventoryError> {
    if other
        .coverage(coin.branch)
        .is_some_and(|coverage| coverage.contains(coin.index))
    {
        Ok(())
    } else {
        Err(InventoryError::Coverage(other.chain(), coin.outpoint))
    }
}

fn fresh_receive(btcb2: &ScanReport, bitcoin: &ScanReport, external_ranged: bool) -> FreshIndex {
    if !external_ranged {
        return FreshIndex::FixedDescriptor;
    }
    fresh_receive_indices(btcb2, bitcoin, true, 1)
        .and_then(|indices| indices.first().copied())
        .map_or(FreshIndex::NotProven, FreshIndex::Proven)
}

fn fresh_receive_indices(
    btcb2: &ScanReport,
    bitcoin: &ScanReport,
    external_ranged: bool,
    count: u32,
) -> Option<Vec<u32>> {
    if !external_ranged || count == 0 || count > 3 {
        return None;
    }
    let (Some(a), Some(b)) = (
        btcb2.coverage(Branch::External),
        bitcoin.coverage(Branch::External),
    ) else {
        return None;
    };
    // Indices below a nonzero start were never observed.
    if a.start != 0 || b.start != 0 {
        return None;
    }
    let next = match a.last_used.max(b.last_used) {
        Some(index) => index.checked_add(1),
        None => Some(0),
    };
    let first = next?;
    let mut indices = Vec::with_capacity(count as usize);
    for offset in 0..count {
        let index = first.checked_add(offset)?;
        if !a.contains(index)
            || !b.contains(index)
            || btcb2
                .coins()
                .iter()
                .chain(bitcoin.coins())
                .any(|c| c.branch == Branch::External && c.index == index)
        {
            return None;
        }
        indices.push(index);
    }
    Some(indices)
}

/// The Bitcoin mirror of a BTCB2 plan: same descriptors, ranges and gap.
pub fn bitcoin_plan(btcb2: &ScanPlan) -> Result<ScanPlan, InventoryError> {
    if btcb2.chain != ChainId::BitcoinBlake2b {
        return Err(InventoryError::WrongChain);
    }
    Ok(ScanPlan {
        chain: ChainId::Bitcoin,
        branches: btcb2
            .branches
            .iter()
            .map(|range| BranchRange {
                descriptor: range.descriptor.clone(),
                start: range.start,
                end_exclusive: range.end_exclusive,
            })
            .collect(),
        gap: btcb2.gap,
    })
}

/// Both chains' reports, retained for the handoff, plus the joined inventory.
#[derive(Debug, Clone)]
pub struct TwoChainScan {
    pub btcb2: ScanReport,
    pub bitcoin: ScanReport,
    pub inventory: SplitInventory,
}

/// Source-owned BTCB2 inputs recovered from a post-Bitcoin scan. The Bitcoin
/// block references must come from a fresh canonical post-Bitcoin collector;
/// these coins are construction data, never signing or send authority.
#[derive(Debug)]
pub struct PostBitcoinSplitCoins {
    pub shared: Vec<SplitCoin>,
    pub funding: SplitCoin,
}

impl TwoChainScan {
    /// Rebuild the fork-side input data after the Bitcoin spend has removed
    /// the shared outputs from that chain's UTXO scan. Require the collector's
    /// canonical Bitcoin blocks to agree exactly with the BTCB2 scan.
    #[allow(dead_code)] // Used when the stage-two coordinator opens the pair.
    #[allow(clippy::too_many_arguments)]
    pub fn post_bitcoin_fork_coins(
        &self,
        source: &SplitSource,
        shared_blocks: &[(OutPoint, BlockRef)],
        fork_funding: OutPoint,
        fork_funding_index: u32,
        expected_generation: u64,
        tips: PreflightTips,
    ) -> Result<PostBitcoinSplitCoins, InventoryError> {
        let shared_outpoints: Vec<_> = shared_blocks
            .iter()
            .map(|(outpoint, _)| *outpoint)
            .collect();
        self.reconcile_post_bitcoin_split(
            source,
            &shared_outpoints,
            fork_funding,
            fork_funding_index,
            expected_generation,
            tips,
        )?;
        let mut shared = Vec::with_capacity(shared_blocks.len());
        for (outpoint, bitcoin_block) in shared_blocks {
            let coin = self
                .btcb2
                .coins()
                .iter()
                .find(|coin| coin.outpoint == *outpoint)
                .ok_or(InventoryError::Stale)?;
            let mut reconstructed = funding_coin(coin, ChainId::BitcoinBlake2b)?;
            if reconstructed.btcb2_block != Some(*bitcoin_block)
                || bitcoin_block.height >= self.inventory.fork_height()
            {
                return Err(InventoryError::Inconsistent(*outpoint));
            }
            reconstructed.bitcoin_block = Some(*bitcoin_block);
            shared.push(reconstructed);
        }
        let saved_funding = self
            .inventory
            .funding_candidate(ChainId::BitcoinBlake2b, fork_funding)
            .ok_or(InventoryError::Stale)?;
        let current_funding = self
            .btcb2
            .coins()
            .iter()
            .find(|coin| coin.outpoint == fork_funding)
            .ok_or(InventoryError::Stale)?;
        let funding = funding_coin(current_funding, ChainId::BitcoinBlake2b)?;
        if funding != saved_funding {
            return Err(InventoryError::Stale);
        }
        Ok(PostBitcoinSplitCoins { shared, funding })
    }

    /// Reconcile a fresh foreign-source scan after the Bitcoin side of an
    /// ordinary Split confirms. The exact shared inputs must now be absent
    /// from Bitcoin's scanned UTXO set but remain confirmed and unspent in
    /// the BTCB2 source; the reserved BTCB2-only input must also remain there.
    /// Chain ancestry, the exact signed Bitcoin spend, target reservation and
    /// BTCB2 policy are separate checks for the stage-two coordinator.
    #[allow(dead_code)] // The stage-two coordinator is not wired yet.
    pub fn reconcile_post_bitcoin_split(
        &self,
        source: &SplitSource,
        shared: &[OutPoint],
        fork_funding: OutPoint,
        fork_funding_index: u32,
        expected_generation: u64,
        tips: PreflightTips,
    ) -> Result<(), InventoryError> {
        let expected: BTreeSet<_> = shared.iter().copied().collect();
        let spent_on_bitcoin: BTreeSet<_> = self
            .inventory
            .spent_on_bitcoin()
            .iter()
            .map(|coin| coin.outpoint)
            .collect();
        let fork_pre_fork: BTreeSet<_> = self
            .btcb2
            .coins()
            .iter()
            .filter(|coin| self.btcb2.fork_side(coin) == ForkSide::PreFork)
            .map(|coin| coin.outpoint)
            .collect();
        if self.bitcoin.chain() != ChainId::Bitcoin
            || self.btcb2.chain() != ChainId::BitcoinBlake2b
            || self.bitcoin.generation() != expected_generation
            || self.btcb2.generation() != expected_generation
            || self.inventory.generation() != expected_generation
            || self.bitcoin.tip() != tips.bitcoin.hash
            || u64::from(self.bitcoin.tip_height()) != tips.bitcoin.height
            || self.btcb2.tip() != tips.fork.hash
            || u64::from(self.btcb2.tip_height()) != tips.fork.height
            || expected.is_empty()
            || expected.len() != shared.len()
            || expected.contains(&fork_funding)
            || expected != spent_on_bitcoin
            || expected != fork_pre_fork
        {
            return Err(InventoryError::Stale);
        }
        let funding = self
            .inventory
            .funding_candidate(ChainId::BitcoinBlake2b, fork_funding)
            .ok_or(InventoryError::Stale)?;
        if funding.branch != SplitBranch::External || funding.index != fork_funding_index {
            return Err(InventoryError::Stale);
        }
        for outpoint in expected
            .iter()
            .copied()
            .chain(std::iter::once(fork_funding))
        {
            let coin = self
                .btcb2
                .coins()
                .iter()
                .find(|coin| coin.outpoint == outpoint)
                .ok_or(InventoryError::Stale)?;
            let other_coverage = self
                .bitcoin
                .coverage(coin.branch)
                .ok_or(InventoryError::Stale)?;
            let fork_coverage = self
                .btcb2
                .coverage(coin.branch)
                .ok_or(InventoryError::Stale)?;
            let descriptor = match coin.branch {
                Branch::External => source.external(),
                Branch::Internal => source.internal().ok_or(InventoryError::Stale)?,
            };
            let owned_script = descriptor
                .at_derivation_index(coin.index)
                .map_err(|_| InventoryError::Stale)?
                .script_pubkey();
            if !other_coverage.contains(coin.index)
                || !fork_coverage.contains(coin.index)
                || !coin.confirmed
                || coin
                    .block_height
                    .is_none_or(|height| height > self.btcb2.tip_height())
                || coin.previous.compute_txid() != outpoint.txid
                || coin.previous.output.get(outpoint.vout as usize) != Some(&coin.output)
                || coin.output.script_pubkey != owned_script
                || (outpoint == fork_funding
                    && (coin.branch != Branch::External
                        || coin.index != fork_funding_index
                        || self.btcb2.fork_side(coin) != ForkSide::PostFork))
                || (outpoint != fork_funding && self.btcb2.fork_side(coin) != ForkSide::PreFork)
            {
                return Err(InventoryError::Stale);
            }
        }
        Ok(())
    }
}

/// Scan both chains under one generation. Cancelling the generation cancels
/// both scans; any incomplete scan is an error.
pub async fn scan(
    client: CoincubeClient,
    plan: ScanPlan,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<TwoChainScan, InventoryError> {
    run(plan, expected, generation, None, move |plan, receiver| {
        foreign_scan::scan(client.clone(), plan, expected, receiver)
    })
    .await
}

/// The known-Vault Claim scan may cover a wider used range than a foreign
/// wallet preview. It retains the same completeness and freshness checks.
pub async fn scan_for_claim(
    client: CoincubeClient,
    plan: ScanPlan,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<TwoChainScan, InventoryError> {
    run(plan, expected, generation, None, move |plan, receiver| {
        foreign_scan::scan_with_timeout(
            client.clone(),
            plan,
            expected,
            receiver,
            foreign_scan::CLAIM_SCAN_DURATION,
        )
    })
    .await
}

/// Scan every address in both branches of an imported wallet's attested
/// discovery window, on both chains. A gap never shortens this walk, and
/// coverage is checked before any inventory is built. This remains evidence,
/// never signing or Claim authorization.
pub async fn scan_imported(
    client: CoincubeClient,
    plan: ScanPlan,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<TwoChainScan, InventoryError> {
    let end = foreign_scan::validate_imported_plan(&plan)
        .map_err(|error| InventoryError::Scan(ChainId::BitcoinBlake2b, error))?;
    run(
        plan,
        expected,
        generation,
        Some(end),
        move |plan, receiver| {
            foreign_scan::scan_imported_window(client.clone(), plan, expected, receiver)
        },
    )
    .await
}

async fn run<F, Fut>(
    plan: ScanPlan,
    expected: u64,
    generation: watch::Receiver<u64>,
    required_coverage: Option<u32>,
    scan_one: F,
) -> Result<TwoChainScan, InventoryError>
where
    F: Fn(ScanPlan, watch::Receiver<u64>) -> Fut,
    Fut: Future<Output = Result<ScanReport, ScanError>>,
{
    let mirror = bitcoin_plan(&plan)?;
    let external_ranged = plan
        .branches
        .iter()
        .find(|range| range.descriptor.branch() == Branch::External)
        .is_some_and(|range| range.descriptor.is_ranged());
    let (btcb2, bitcoin) = tokio::try_join!(
        async {
            scan_one(plan, generation.clone())
                .await
                .map_err(|e| InventoryError::Scan(ChainId::BitcoinBlake2b, e))
        },
        async {
            scan_one(mirror, generation.clone())
                .await
                .map_err(|e| InventoryError::Scan(ChainId::Bitcoin, e))
        },
    )?;
    if *generation.borrow() != expected {
        return Err(InventoryError::Stale);
    }
    if let Some(end) = required_coverage {
        for report in [&btcb2, &bitcoin] {
            for branch in [Branch::External, Branch::Internal] {
                let coverage = report.coverage(branch).ok_or(InventoryError::Stale)?;
                if coverage.start != 0 || coverage.end_exclusive != end {
                    return Err(InventoryError::Stale);
                }
            }
        }
    }
    let inventory = SplitInventory::join(&btcb2, &bitcoin, expected, external_ranged)?;
    Ok(TwoChainScan {
        btcb2,
        bitcoin,
        inventory,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::foreign_scan::{BranchCoverage, ScanDescriptor};
    use coincube_core::miniscript::bitcoin::{
        self, absolute,
        bip32::{Xpriv, Xpub},
        hashes::Hash,
        secp256k1::Secp256k1,
        transaction, Amount, Transaction, TxIn, TxOut,
    };
    use coincube_core::miniscript::{Descriptor, DescriptorPublicKey};
    use std::str::FromStr;

    const FORK: u64 = 100;

    fn descriptor_at(suffix: &str) -> ScanDescriptor {
        let xpub = Xpub::from_priv(
            &Secp256k1::new(),
            &Xpriv::new_master(bitcoin::Network::Bitcoin, &[42; 32]).unwrap(),
        );
        ScanDescriptor::parse(Branch::External, &format!("wpkh({xpub}/0/{suffix})")).unwrap()
    }

    fn descriptor() -> ScanDescriptor {
        descriptor_at("*")
    }

    fn source(seed: u8) -> SplitSource {
        let xpub = Xpub::from_priv(
            &Secp256k1::new(),
            &Xpriv::new_master(bitcoin::Network::Bitcoin, &[seed; 32]).unwrap(),
        );
        let external =
            Descriptor::<DescriptorPublicKey>::from_str(&format!("wpkh({xpub}/0/*)")).unwrap();
        SplitSource::new(external, None).unwrap()
    }

    fn hash(height: u32) -> BlockHash {
        BlockHash::from_byte_array([height as u8; 32])
    }

    /// A coin at `index` created by a transaction unique to `salt`.
    fn coin(salt: u32, index: u32, sats: u64, height: Option<u32>) -> DiscoveredCoin {
        let previous = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::from_consensus(salt),
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: descriptor().script(index).unwrap(),
            }],
        };
        DiscoveredCoin {
            branch: Branch::External,
            index,
            outpoint: OutPoint::new(previous.compute_txid(), 0),
            output: previous.output[0].clone(),
            previous,
            confirmed: height.is_some(),
            block_height: height,
            block_hash: height.map(hash),
        }
    }

    fn walk(end_exclusive: u32, last_used: Option<u32>) -> Vec<BranchCoverage> {
        vec![BranchCoverage {
            branch: Branch::External,
            start: 0,
            end_exclusive,
            last_used,
        }]
    }

    fn report(chain: ChainId, coins: Vec<DiscoveredCoin>, walk: Vec<BranchCoverage>) -> ScanReport {
        let report = ScanReport::for_test(chain, 5, hash(200), coins).with_coverage(walk);
        if chain == ChainId::BitcoinBlake2b {
            report.with_fork_height(Some(FORK))
        } else {
            report
        }
    }

    fn outpoints(rows: &[InventoryCoin]) -> Vec<OutPoint> {
        rows.iter().map(|row| row.outpoint).collect()
    }

    #[test]
    fn split_inventory_joins_by_outpoint_into_categories() {
        let both = coin(1, 0, 1_000, Some(90));
        let only_btcb2_pre = coin(2, 1, 2_000, Some(91));
        let only_bitcoin_pre = coin(3, 2, 3_000, Some(92));
        let bitcoin_post = coin(4, 3, 4_000, Some(150));
        let btcb2_post = coin(5, 4, 5_000, Some(151));
        let bitcoin_unconfirmed = coin(6, 5, 6_000, None);
        let replayed = coin(7, 5, 7_000, Some(160));
        let mut replayed_on_bitcoin = replayed.clone();
        (
            replayed_on_bitcoin.block_height,
            replayed_on_bitcoin.block_hash,
        ) = (Some(170), Some(hash(170)));
        let btcb2 = report(
            ChainId::BitcoinBlake2b,
            vec![
                both.clone(),
                only_btcb2_pre.clone(),
                btcb2_post.clone(),
                replayed.clone(),
            ],
            walk(26, Some(5)),
        );
        let bitcoin = report(
            ChainId::Bitcoin,
            vec![
                both.clone(),
                only_bitcoin_pre.clone(),
                bitcoin_post.clone(),
                bitcoin_unconfirmed.clone(),
                replayed_on_bitcoin,
            ],
            walk(26, Some(5)),
        );
        let inventory = SplitInventory::join(&btcb2, &bitcoin, 5, true).unwrap();
        assert_eq!(inventory.fork_height(), FORK);
        assert_eq!(outpoints(inventory.splittable()), vec![both.outpoint]);
        assert_eq!(inventory.splittable()[0].sats, 1_000);
        assert_eq!(
            outpoints(inventory.spent_on_bitcoin()),
            vec![only_btcb2_pre.outpoint]
        );
        assert_eq!(
            outpoints(inventory.spent_on_btcb2()),
            vec![only_bitcoin_pre.outpoint]
        );
        assert_eq!(
            outpoints(inventory.btcb2_post_fork()),
            vec![btcb2_post.outpoint]
        );
        assert_eq!(
            inventory
                .bitcoin_only_post_fork()
                .iter()
                .map(|c| c.display().outpoint)
                .collect::<Vec<_>>(),
            vec![bitcoin_post.outpoint]
        );
        assert_eq!(
            inventory
                .btcb2_only_post_fork()
                .iter()
                .map(|c| c.display().outpoint)
                .collect::<Vec<_>>(),
            vec![btcb2_post.outpoint]
        );
        let bitcoin_funding = inventory
            .funding_candidate(ChainId::Bitcoin, bitcoin_post.outpoint)
            .unwrap();
        assert_eq!(bitcoin_funding.previous, bitcoin_post.previous);
        assert_eq!(bitcoin_funding.bitcoin_block.unwrap().height, 150);
        assert!(bitcoin_funding.btcb2_block.is_none());
        assert_eq!(
            inventory.funding_candidates(ChainId::Bitcoin),
            vec![bitcoin_funding.clone()]
        );
        let fork_funding = inventory
            .funding_candidate(ChainId::BitcoinBlake2b, btcb2_post.outpoint)
            .unwrap();
        assert_eq!(fork_funding.previous, btcb2_post.previous);
        assert_eq!(fork_funding.btcb2_block.unwrap().height, 151);
        assert!(fork_funding.bitcoin_block.is_none());
        assert_eq!(
            inventory.funding_candidates(ChainId::BitcoinBlake2b),
            vec![fork_funding.clone()]
        );
        let reserved = inventory.funding_at_reserved_addresses(FreshFundingAddresses {
            bitcoin_deposit: 3,
            btcb2_deposit: 4,
            bitcoin_return: 6,
        });
        assert_eq!(reserved.funding_candidates(ChainId::Bitcoin).len(), 1);
        assert_eq!(
            reserved.funding_candidates(ChainId::BitcoinBlake2b).len(),
            1
        );
        let mut with_earlier_small_deposit = reserved.clone();
        let mut small = bitcoin_funding.clone();
        small.previous.output[0].value = Amount::from_sat(1_000);
        small.outpoint.txid = small.previous.compute_txid();
        with_earlier_small_deposit
            .bitcoin_funding_candidates
            .insert(small.outpoint, small.clone());
        let mut later = bitcoin_funding.clone();
        later.previous.output[0].value = Amount::from_sat(6_000);
        later.outpoint.txid = later.previous.compute_txid();
        with_earlier_small_deposit
            .bitcoin_funding_candidates
            .insert(later.outpoint, later.clone());
        let exact = with_earlier_small_deposit
            .funding_at_outpoints(bitcoin_funding.outpoint, fork_funding.outpoint);
        assert_eq!(
            exact.funding_candidates(ChainId::Bitcoin),
            vec![bitcoin_funding.clone()]
        );
        assert_eq!(
            exact.funding_candidates(ChainId::BitcoinBlake2b),
            vec![fork_funding.clone()]
        );
        assert!(with_earlier_small_deposit
            .funding_at_outpoints(OutPoint::null(), fork_funding.outpoint)
            .funding_candidates(ChainId::Bitcoin)
            .is_empty());
        let eligible = with_earlier_small_deposit
            .funding_covering_estimated_fees(4_000, 5_000)
            .funding_candidates(ChainId::Bitcoin);
        assert_eq!(eligible.len(), 2);
        assert!(eligible.contains(&bitcoin_funding));
        assert!(eligible.contains(&later));
        let bitcoin_too_small = reserved.funding_covering_estimated_fees(4_001, 5_000);
        assert!(bitcoin_too_small
            .funding_candidates(ChainId::Bitcoin)
            .is_empty());
        assert_eq!(
            bitcoin_too_small.funding_candidates(ChainId::BitcoinBlake2b),
            vec![fork_funding.clone()]
        );
        let fork_too_small = reserved.funding_covering_estimated_fees(4_000, 5_001);
        assert_eq!(
            fork_too_small.funding_candidates(ChainId::Bitcoin),
            vec![bitcoin_funding.clone()]
        );
        assert!(fork_too_small
            .funding_candidates(ChainId::BitcoinBlake2b)
            .is_empty());
        let elsewhere = inventory.funding_at_reserved_addresses(FreshFundingAddresses {
            bitcoin_deposit: 4,
            btcb2_deposit: 3,
            bitcoin_return: 6,
        });
        assert!(elsewhere.funding_candidates(ChainId::Bitcoin).is_empty());
        assert!(elsewhere
            .funding_candidates(ChainId::BitcoinBlake2b)
            .is_empty());
        assert!(inventory
            .funding_candidate(ChainId::Bitcoin, btcb2_post.outpoint)
            .is_none());
        assert!(inventory
            .funding_candidate(ChainId::BitcoinBlake2b, both.outpoint)
            .is_none());
        assert_eq!(
            outpoints(inventory.pending()),
            vec![replayed.outpoint, bitcoin_unconfirmed.outpoint]
        );
        assert_eq!(inventory.fresh_receive(), FreshIndex::Proven(6));
        assert_eq!(
            inventory.fresh_deposit_addresses(),
            Some(FreshDepositAddresses {
                bitcoin_deposit: 6,
                btcb2_deposit: 7,
            })
        );
        assert_eq!(
            inventory.fresh_funding_addresses(),
            Some(FreshFundingAddresses {
                bitcoin_deposit: 6,
                btcb2_deposit: 7,
                bitcoin_return: 8,
            })
        );
    }

    #[test]
    fn post_bitcoin_split_rechecks_exact_source_coins_and_fork_funding() {
        let shared = coin(1, 0, 10_000, Some(90));
        let funding = coin(2, 1, 10_000, Some(150));
        let bitcoin = report(ChainId::Bitcoin, vec![], walk(10, Some(1))).with_tip_height(200);
        let btcb2 = report(
            ChainId::BitcoinBlake2b,
            vec![shared.clone(), funding.clone()],
            walk(10, Some(1)),
        )
        .with_tip_height(200);
        let inventory = SplitInventory::join(&btcb2, &bitcoin, 5, true).unwrap();
        let scan = TwoChainScan {
            bitcoin,
            btcb2,
            inventory,
        };
        let tips = PreflightTips {
            bitcoin: BlockRef {
                height: 200,
                hash: hash(200),
            },
            fork: BlockRef {
                height: 200,
                hash: hash(200),
            },
        };
        assert_eq!(
            scan.reconcile_post_bitcoin_split(
                &source(42),
                &[shared.outpoint],
                funding.outpoint,
                1,
                5,
                tips,
            ),
            Ok(())
        );
        let shared_block = BlockRef {
            height: 90,
            hash: hash(90),
        };
        let recovered = scan
            .post_bitcoin_fork_coins(
                &source(42),
                &[(shared.outpoint, shared_block)],
                funding.outpoint,
                1,
                5,
                tips,
            )
            .unwrap();
        assert_eq!(recovered.shared.len(), 1);
        assert_eq!(recovered.shared[0].previous, shared.previous);
        assert_eq!(recovered.shared[0].bitcoin_block, Some(shared_block));
        assert_eq!(recovered.shared[0].btcb2_block, Some(shared_block));
        assert_eq!(recovered.funding.previous, funding.previous);
        assert!(recovered.funding.bitcoin_block.is_none());
        assert_eq!(
            scan.post_bitcoin_fork_coins(
                &source(42),
                &[(
                    shared.outpoint,
                    BlockRef {
                        height: 90,
                        hash: hash(89),
                    },
                )],
                funding.outpoint,
                1,
                5,
                tips,
            )
            .err(),
            Some(InventoryError::Inconsistent(shared.outpoint))
        );
        for (source, shared, index) in [
            (source(43), vec![shared.outpoint], 1),
            (source(42), vec![], 1),
            (source(42), vec![shared.outpoint], 2),
        ] {
            assert_eq!(
                scan.reconcile_post_bitcoin_split(
                    &source,
                    &shared,
                    funding.outpoint,
                    index,
                    5,
                    tips,
                ),
                Err(InventoryError::Stale)
            );
        }
        assert_eq!(
            scan.reconcile_post_bitcoin_split(
                &source(42),
                &[shared.outpoint],
                funding.outpoint,
                1,
                5,
                PreflightTips {
                    fork: BlockRef {
                        height: 201,
                        hash: hash(201),
                    },
                    ..tips
                },
            ),
            Err(InventoryError::Stale)
        );
        let spent_funding_scan = TwoChainScan {
            bitcoin: scan.bitcoin.clone(),
            btcb2: report(
                ChainId::BitcoinBlake2b,
                vec![shared.clone()],
                walk(10, Some(1)),
            )
            .with_tip_height(200),
            inventory: scan.inventory.clone(),
        };
        assert_eq!(
            spent_funding_scan.reconcile_post_bitcoin_split(
                &source(42),
                &[shared.outpoint],
                funding.outpoint,
                1,
                5,
                tips,
            ),
            Err(InventoryError::Stale)
        );
        let mut changed_funding = funding.clone();
        changed_funding.previous.output[0].value = Amount::from_sat(9_999);
        changed_funding.output.value = Amount::from_sat(9_999);
        let changed_funding_scan = TwoChainScan {
            bitcoin: scan.bitcoin.clone(),
            btcb2: report(
                ChainId::BitcoinBlake2b,
                vec![shared.clone(), changed_funding],
                walk(10, Some(1)),
            )
            .with_tip_height(200),
            inventory: scan.inventory.clone(),
        };
        assert!(changed_funding_scan
            .post_bitcoin_fork_coins(
                &source(42),
                &[(shared.outpoint, shared_block)],
                funding.outpoint,
                1,
                5,
                tips,
            )
            .is_err());
    }

    #[test]
    fn two_fresh_deposit_addresses_do_not_require_a_third_split_return() {
        let btcb2 = report(ChainId::BitcoinBlake2b, Vec::new(), walk(8, Some(5)));
        let bitcoin = report(ChainId::Bitcoin, Vec::new(), walk(8, Some(5)));
        let inventory = SplitInventory::join(&btcb2, &bitcoin, 5, true).unwrap();
        assert_eq!(
            inventory.fresh_deposit_addresses(),
            Some(FreshDepositAddresses {
                bitcoin_deposit: 6,
                btcb2_deposit: 7,
            })
        );
        assert_eq!(inventory.fresh_funding_addresses(), None);
    }

    #[test]
    fn post_fork_candidate_requires_other_chains_address_coverage() {
        let fork_coin = coin(8, 8, 2_000, Some(150));
        let bitcoin_coin = coin(9, 9, 3_000, Some(151));
        let btcb2 = report(
            ChainId::BitcoinBlake2b,
            vec![fork_coin.clone()],
            walk(9, Some(8)),
        );
        let bitcoin = report(ChainId::Bitcoin, vec![bitcoin_coin.clone()], walk(8, None));
        let inventory = SplitInventory::join(&btcb2, &bitcoin, 5, true).unwrap();
        assert_eq!(inventory.btcb2_post_fork().len(), 1);
        assert!(inventory.btcb2_only_post_fork().is_empty());
        assert!(inventory.bitcoin_only_post_fork().is_empty());
        assert!(inventory
            .funding_candidate(ChainId::BitcoinBlake2b, fork_coin.outpoint)
            .is_none());
        assert!(inventory
            .funding_candidate(ChainId::Bitcoin, bitcoin_coin.outpoint)
            .is_none());
    }

    #[test]
    fn malformed_funding_candidate_cannot_enter_transaction_inventory() {
        let mut wrong_previous = coin(10, 10, 10_000, Some(150));
        wrong_previous.previous.output[0].value = Amount::from_sat(9_999);
        let btcb2 = report(ChainId::BitcoinBlake2b, Vec::new(), walk(26, None));
        let bitcoin = report(
            ChainId::Bitcoin,
            vec![wrong_previous.clone()],
            walk(26, Some(10)),
        );
        assert_eq!(
            SplitInventory::join(&btcb2, &bitcoin, 5, true),
            Err(InventoryError::Inconsistent(wrong_previous.outpoint))
        );
    }

    /// #568 B1a: each splittable coin is a step-1 input with its previous
    /// transaction and the same pre-fork block on both chains; nothing else
    /// becomes one. Both tip heights are kept.
    #[test]
    fn split_splittable_coins_are_step1_inputs_with_both_blocks() {
        let both = coin(1, 0, 1_000, Some(90));
        let post = coin(2, 1, 2_000, Some(150));
        let pending = coin(3, 2, 3_000, None);
        let btcb2_only = coin(4, 3, 4_000, Some(91));
        let btcb2 = report(
            ChainId::BitcoinBlake2b,
            vec![both.clone(), post.clone(), pending.clone(), btcb2_only],
            walk(26, Some(3)),
        )
        .with_tip_height(210);
        let bitcoin = report(
            ChainId::Bitcoin,
            vec![both.clone(), post, pending],
            walk(26, Some(3)),
        )
        .with_tip_height(220);
        let inventory = SplitInventory::join(&btcb2, &bitcoin, 5, true).unwrap();
        assert_eq!(
            (inventory.btcb2_tip_height(), inventory.bitcoin_tip_height()),
            (210, 220)
        );
        let block = Some(BlockRef {
            height: 90,
            hash: hash(90),
        });
        assert_eq!(
            inventory.splittable_coins(),
            vec![SplitCoin {
                outpoint: both.outpoint,
                branch: SplitBranch::External,
                index: 0,
                previous: both.previous.clone(),
                bitcoin_block: block,
                btcb2_block: block,
            }]
        );
        assert_eq!(outpoints(inventory.splittable()), vec![both.outpoint]);
    }

    #[test]
    fn split_prevout_or_shared_block_mismatch_refuses() {
        let both = coin(1, 0, 1_000, Some(90));
        let walk = walk(21, Some(0));
        let btcb2 = report(ChainId::BitcoinBlake2b, vec![both.clone()], walk.clone());

        let mut amount = both.clone();
        amount.output.value = Amount::from_sat(999);
        let mut script = both.clone();
        script.output.script_pubkey = descriptor().script(1).unwrap();
        for other in [amount, script] {
            let bitcoin = report(ChainId::Bitcoin, vec![other], walk.clone());
            assert_eq!(
                SplitInventory::join(&btcb2, &bitcoin, 5, true),
                Err(InventoryError::PrevoutMismatch(both.outpoint))
            );
        }

        let mut other_block = both.clone();
        other_block.block_hash = Some(hash(7));
        let mut post_fork_on_bitcoin = both.clone();
        (
            post_fork_on_bitcoin.block_height,
            post_fork_on_bitcoin.block_hash,
        ) = (Some(150), Some(hash(150)));
        let mut unconfirmed_on_bitcoin = both.clone();
        (
            unconfirmed_on_bitcoin.confirmed,
            unconfirmed_on_bitcoin.block_height,
            unconfirmed_on_bitcoin.block_hash,
        ) = (false, None, None);
        let mut other_index = both.clone();
        other_index.index = 3;
        for other in [
            other_block,
            post_fork_on_bitcoin,
            unconfirmed_on_bitcoin,
            other_index,
        ] {
            let bitcoin = report(ChainId::Bitcoin, vec![other], walk.clone());
            assert_eq!(
                SplitInventory::join(&btcb2, &bitcoin, 5, true),
                Err(InventoryError::Inconsistent(both.outpoint))
            );
        }
    }

    #[test]
    fn split_absence_outside_the_other_walk_is_an_error() {
        let pre = coin(1, 30, 1_000, Some(90));
        let btcb2 = report(
            ChainId::BitcoinBlake2b,
            vec![pre.clone()],
            walk(51, Some(30)),
        );
        // The Bitcoin walk stopped at 21, so index 30 was never observed there.
        let bitcoin = report(ChainId::Bitcoin, vec![], walk(21, None));
        assert_eq!(
            SplitInventory::join(&btcb2, &bitcoin, 5, true),
            Err(InventoryError::Coverage(ChainId::Bitcoin, pre.outpoint))
        );
        let no_walk = report(ChainId::Bitcoin, vec![], vec![]);
        assert_eq!(
            SplitInventory::join(&btcb2, &no_walk, 5, true),
            Err(InventoryError::Coverage(ChainId::Bitcoin, pre.outpoint))
        );
        // Symmetric for a pre-fork Bitcoin coin absent on BTCB2.
        let bitcoin = report(ChainId::Bitcoin, vec![pre.clone()], walk(51, Some(30)));
        let btcb2 = report(ChainId::BitcoinBlake2b, vec![], walk(21, None));
        assert_eq!(
            SplitInventory::join(&btcb2, &bitcoin, 5, true),
            Err(InventoryError::Coverage(
                ChainId::BitcoinBlake2b,
                pre.outpoint
            ))
        );
    }

    #[test]
    fn split_fresh_index_is_unused_on_both_chains() {
        let fresh = |btcb2_walk, bitcoin_walk, ranged| {
            SplitInventory::join(
                &report(ChainId::BitcoinBlake2b, vec![], btcb2_walk),
                &report(ChainId::Bitcoin, vec![], bitcoin_walk),
                5,
                ranged,
            )
            .unwrap()
            .fresh_receive()
        };
        // Highest history on either chain decides: Bitcoin used 7, BTCB2 used 3.
        assert_eq!(
            fresh(walk(24, Some(3)), walk(28, Some(7)), true),
            FreshIndex::Proven(8)
        );
        // Index 8 lies beyond the BTCB2 walk: unobserved there, so not fresh.
        assert_eq!(
            fresh(walk(8, Some(3)), walk(28, Some(7)), true),
            FreshIndex::NotProven
        );
        assert_eq!(
            fresh(walk(20, None), walk(20, None), true),
            FreshIndex::Proven(0)
        );
        assert_eq!(
            fresh(walk(1, None), walk(1, None), false),
            FreshIndex::FixedDescriptor
        );
        assert_eq!(fresh(vec![], walk(20, None), true), FreshIndex::NotProven);
        let mut late = walk(20, None);
        late[0].start = 5;
        assert_eq!(fresh(late, walk(20, None), true), FreshIndex::NotProven);
        let too_narrow = SplitInventory::join(
            &report(ChainId::BitcoinBlake2b, vec![], walk(9, Some(7))),
            &report(ChainId::Bitcoin, vec![], walk(20, Some(7))),
            5,
            true,
        )
        .unwrap();
        assert_eq!(too_narrow.fresh_receive(), FreshIndex::Proven(8));
        assert_eq!(too_narrow.fresh_funding_addresses(), None);
        // A coin at the candidate index contradicts "unused": refuse it.
        let inventory = SplitInventory::join(
            &report(
                ChainId::BitcoinBlake2b,
                vec![coin(1, 0, 1_000, None)],
                walk(20, None),
            ),
            &report(ChainId::Bitcoin, vec![], walk(20, None)),
            5,
            true,
        )
        .unwrap();
        assert_eq!(inventory.fresh_receive(), FreshIndex::NotProven);
    }

    #[test]
    fn split_fork_height_comes_from_the_btcb2_observation() {
        let at_95 = coin(1, 0, 1_000, Some(95));
        let join = |fork: Option<u64>, bitcoin_fork: Option<u64>| {
            let btcb2 =
                ScanReport::for_test(ChainId::BitcoinBlake2b, 5, hash(200), vec![at_95.clone()])
                    .with_coverage(walk(21, Some(0)))
                    .with_fork_height(fork);
            let bitcoin = report(ChainId::Bitcoin, vec![at_95.clone()], walk(21, Some(0)))
                .with_fork_height(bitcoin_fork);
            SplitInventory::join(&btcb2, &bitcoin, 5, true)
        };
        // Observed 100: height 95 is shared pre-fork history.
        let inventory = join(Some(100), None).unwrap();
        assert_eq!(
            (inventory.fork_height(), inventory.splittable().len()),
            (100, 1)
        );
        // Observed 91: the same coin is post-fork on both, never splittable.
        let inventory = join(Some(91), None).unwrap();
        assert_eq!(
            (inventory.splittable().len(), inventory.pending().len()),
            (0, 1)
        );
        // A Bitcoin-side value is ignored; a missing BTCB2 observation refuses.
        assert_eq!(join(Some(100), Some(50)).unwrap().fork_height(), 100);
        assert_eq!(
            join(None, Some(100)),
            Err(InventoryError::ForkHeightUnknown)
        );
    }

    fn btcb2_plan() -> ScanPlan {
        ScanPlan {
            chain: ChainId::BitcoinBlake2b,
            branches: vec![BranchRange {
                descriptor: descriptor(),
                start: 0,
                end_exclusive: 50,
            }],
            gap: 20,
        }
    }

    #[test]
    fn split_bitcoin_plan_mirrors_descriptors_ranges_and_gap() {
        let plan = btcb2_plan();
        let mirror = bitcoin_plan(&plan).unwrap();
        assert_eq!(mirror.chain, ChainId::Bitcoin);
        assert_eq!(mirror.gap, plan.gap);
        assert_eq!(
            mirror.branches[0].descriptor.canonical(),
            plan.branches[0].descriptor.canonical()
        );
        assert_eq!(
            (mirror.branches[0].start, mirror.branches[0].end_exclusive),
            (0, 50)
        );
        assert_eq!(
            bitcoin_plan(&mirror).err(),
            Some(InventoryError::WrongChain)
        );
    }

    #[tokio::test]
    async fn split_incomplete_bitcoin_scan_is_an_error_never_zero() {
        let (_tx, rx) = watch::channel(5);
        let result = run(btcb2_plan(), 5, rx, None, |plan, _| async move {
            match plan.chain {
                ChainId::BitcoinBlake2b => {
                    Ok(report(ChainId::BitcoinBlake2b, vec![], walk(20, None)))
                }
                _ => Err(ScanError::RangeLimit),
            }
        })
        .await;
        assert_eq!(
            result.err(),
            Some(InventoryError::Scan(
                ChainId::Bitcoin,
                ScanError::RangeLimit
            ))
        );
        // Both complete and empty: an explicit, proven-empty inventory.
        let (_tx, rx) = watch::channel(5);
        let two = run(btcb2_plan(), 5, rx, None, |plan, _| async move {
            Ok(report(plan.chain, vec![], walk(20, None)))
        })
        .await
        .unwrap();
        assert!(two.inventory.splittable().is_empty());
        assert_eq!(two.btcb2.chain(), ChainId::BitcoinBlake2b);
    }

    #[tokio::test]
    async fn split_cancellation_and_stale_generation_discard_results() {
        // Superseded while both scans completed: the joined result is dropped.
        let (tx, rx) = watch::channel(5);
        let result = run(btcb2_plan(), 5, rx, None, |plan, _| {
            let _ = tx.send(6);
            async move { Ok(report(plan.chain, vec![], walk(20, None))) }
        })
        .await;
        assert_eq!(result.err(), Some(InventoryError::Stale));
        // A report from another generation never joins.
        let old = ScanReport::for_test(ChainId::Bitcoin, 4, hash(1), vec![]);
        let current = report(ChainId::BitcoinBlake2b, vec![], walk(20, None));
        assert_eq!(
            SplitInventory::join(&current, &old, 5, true),
            Err(InventoryError::Stale)
        );
        assert_eq!(
            SplitInventory::join(&current, &current, 5, true),
            Err(InventoryError::WrongChain)
        );
        // Already cancelled: refuses before any request (port 1 is unreachable).
        let (_tx, rx) = watch::channel(6);
        let client = CoincubeClient::for_test("http://127.0.0.1:1".to_owned());
        assert!(matches!(
            scan(client, btcb2_plan(), 5, rx).await,
            Err(InventoryError::Scan(_, ScanError::Cancelled))
        ));
    }

    #[tokio::test]
    async fn imported_scan_requires_exact_coverage_before_inventory_join() {
        let mut plan = btcb2_plan();
        plan.branches[0].end_exclusive = 40;
        let internal = ScanDescriptor::parse(
            Branch::Internal,
            &descriptor()
                .canonical()
                .split('#')
                .next()
                .unwrap()
                .replace("/0/*", "/1/*"),
        )
        .unwrap();
        plan.branches.push(BranchRange {
            descriptor: internal,
            start: 0,
            end_exclusive: 40,
        });
        let (_sender, receiver) = watch::channel(5);
        let result = run(plan, 5, receiver, Some(40), |plan, _| async move {
            Ok(report(
                plan.chain,
                vec![],
                vec![
                    BranchCoverage {
                        branch: Branch::External,
                        start: 0,
                        end_exclusive: 40,
                        last_used: None,
                    },
                    BranchCoverage {
                        branch: Branch::Internal,
                        start: 0,
                        end_exclusive: 39,
                        last_used: None,
                    },
                ],
            ))
        })
        .await;
        assert_eq!(result.err(), Some(InventoryError::Stale));
    }

    #[tokio::test]
    async fn split_bitcoin_side_scan_sends_no_connect_credentials() {
        use httpmock::prelude::*;
        let server = MockServer::start();
        let fixed = descriptor_at("0");
        let address =
            bitcoin::Address::from_script(&fixed.script(0).unwrap(), bitcoin::Network::Bitcoin)
                .unwrap();
        fn anonymous(r: &HttpMockRequest) -> bool {
            r.headers.as_ref().is_none_or(|headers| {
                headers.iter().all(|(name, _)| {
                    !["authorization", "cookie"]
                        .iter()
                        .any(|bad| name.eq_ignore_ascii_case(bad))
                })
            })
        }
        let mocks: Vec<_> = vec![
            ("blocks/tip/hash".to_string(), "11".repeat(32), 2),
            (
                format!("block/{}/status", "11".repeat(32)),
                r#"{"in_best_chain":true,"height":900000}"#.into(),
                1,
            ),
            (
                format!("address/{address}"),
                r#"{"chain_stats":{"tx_count":0},"mempool_stats":{"tx_count":0}}"#.into(),
                2,
            ),
            (format!("address/{address}/utxo"), "[]".into(), 2),
        ]
        .into_iter()
        .map(|(path, body, hits)| {
            let mock = server.mock(|when, then| {
                when.method(GET)
                    .path(format!("/api/v1/esplora/bitcoin/mainnet/{path}"))
                    .matches(anonymous);
                then.status(200)
                    .header("X-Coincube-Observation", "fresh")
                    .header("X-Cache", "BYPASS")
                    .header("Cache-Control", "no-store")
                    .body(body);
            });
            (mock, hits)
        })
        .collect();
        let mut plan = btcb2_plan();
        plan.branches[0] = BranchRange {
            descriptor: fixed,
            start: 0,
            end_exclusive: 1,
        };
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("synthetic-connect-token");
        let (_tx, rx) = watch::channel(1);
        let report = foreign_scan::scan(client, bitcoin_plan(&plan).unwrap(), 1, rx)
            .await
            .unwrap();
        assert_eq!(report.chain(), ChainId::Bitcoin);
        assert_eq!(report.fork_height(), None);
        assert_eq!(report.tip_height(), 900_000);
        for (mock, hits) in mocks {
            mock.assert_hits(hits);
        }
    }

    /// No code outside this module and the Split panel's display may name a
    /// poison candidate: absence on BTCB2 is never poison proof.
    #[test]
    fn split_funding_candidates_are_display_only() {
        fn walk_dir(dir: &std::path::Path, hits: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk_dir(&path, hits);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    if text.contains("PoisonCandidate")
                        || text.contains("bitcoin_only_post_fork")
                        || text.contains("btcb2_only_post_fork")
                    {
                        hits.push(path.file_name().unwrap().to_string_lossy().into_owned());
                    }
                }
            }
        }
        let mut hits = Vec::new();
        walk_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut hits,
        );
        hits.sort();
        assert_eq!(hits, vec!["foreign_split_inventory.rs", "split_wallet.rs"]);
        // The split panel may only count them for display.
        let panel = include_str!("../split_wallet.rs");
        let uses: Vec<_> = panel
            .lines()
            .filter(|line| {
                line.contains("bitcoin_only_post_fork") || line.contains("btcb2_only_post_fork")
            })
            .collect();
        assert!(!uses.is_empty());
        assert!(
            uses.iter().all(|line| line.contains(".len()")),
            "{:?}",
            uses
        );
    }
}
