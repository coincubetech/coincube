//! Scan-only public descriptor discovery. No import, persistence or spend permission.
//! A history gap bounds discovery; it cannot prove that later addresses are unused.
mod http;
pub mod known;
#[cfg(test)]
mod tests;

use super::coincube::CoincubeClient;
use async_trait::async_trait;
use coincube_core::{
    chain::ChainId,
    miniscript::{
        bitcoin::{self, BlockHash, OutPoint, ScriptBuf, Transaction, Txid},
        descriptor::{ShInner, Wildcard, WshInner},
        Descriptor, DescriptorPublicKey, ForEachKey, Terminal,
    },
};
use iced::futures::future::join_all;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
    time::Duration,
};
use tokio::sync::watch;

pub const MAX_ADDRESSES: u32 = 200;
/// Imported wallets may have a daemon-attested history window far beyond the
/// ordinary gap scan. This bound applies only to their explicit full walk.
pub const MAX_IMPORTED_ADDRESSES: u32 = 20_000;
pub const MAX_IMPORTED_BRANCH_END: u32 = 10_000;
pub const MAX_DURATION: Duration = Duration::from_secs(30);
/// A known Vault scan covers both branches on two chains and may need more
/// time than the interactive foreign-wallet discovery limit.
pub const CLAIM_SCAN_DURATION: Duration = Duration::from_secs(90);
const MAX_UTXOS: usize = 1000;
// Keep the remote Connect scan within its deadline without flooding the API.
// Results are still admitted in address order and every address gets a second
// read; the batch is discovery only, never spend authority.
const ADDRESS_BATCH: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanError {
    UnsupportedChain,
    Descriptor,
    InvalidLimits,
    Cancelled,
    Deadline,
    Unavailable,
    Http(u16),
    RateLimited(Duration),
    Malformed,
    Freshness,
    Changed(ChangedReason),
    RangeLimit,
    AddressLimit,
    BodyLimit,
    Prevout,
}

/// A non-sensitive cause for a changed scan. No address, transaction or wallet
/// identity is retained in the error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangedReason {
    TipNoLongerBest,
    TipMoved,
    AnchorMismatch,
    AnchorChanged,
    AddressState,
    HeightBehindUtxo,
}

/// Read-only, single-address deposit visibility. It is deliberately not a
/// two-chain ancestry proof or permission to spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AddressFunds {
    pub confirmed_sats: u64,
    pub pending_sats: u64,
    pub mature_sats: u64,
    pub largest_confirmed_sats: u64,
    pub largest_mature_sats: u64,
    /// Conservative depth when several unspent deposits share this address.
    pub min_confirmations: u64,
}

pub async fn address_funds(
    client: CoincubeClient,
    chain: ChainId,
    address: &str,
) -> Result<AddressFunds, ScanError> {
    let source = http::HttpSource::new(client)?;
    let utxos = source.utxos(chain, address).await?;
    if utxos.len() > MAX_UTXOS {
        return Err(ScanError::BodyLimit);
    }
    let height = if utxos.iter().any(|coin| coin.status.confirmed) {
        Some(source.current_height(chain).await?)
    } else {
        None
    };
    summarize_address_funds(&utxos, height)
}

fn summarize_address_funds(utxos: &[Utxo], height: Option<u32>) -> Result<AddressFunds, ScanError> {
    let mut funds = AddressFunds::default();
    for coin in utxos {
        if coin.value > bitcoin::Amount::MAX_MONEY.to_sat()
            || (coin.status.confirmed
                && (coin.status.block_hash.is_none() || coin.status.block_height.is_none()))
            || (!coin.status.confirmed
                && (coin.status.block_hash.is_some() || coin.status.block_height.is_some()))
        {
            return Err(ScanError::Malformed);
        }
        let total = if coin.status.confirmed {
            &mut funds.confirmed_sats
        } else {
            &mut funds.pending_sats
        };
        *total = total.checked_add(coin.value).ok_or(ScanError::BodyLimit)?;
        if coin.status.confirmed {
            let block_height = coin.status.block_height.ok_or(ScanError::Malformed)?;
            let depth = u64::from(
                height
                    .ok_or(ScanError::Malformed)?
                    .checked_sub(block_height)
                    .ok_or(ScanError::Changed(ChangedReason::HeightBehindUtxo))?,
            ) + 1;
            funds.largest_confirmed_sats = funds.largest_confirmed_sats.max(coin.value);
            funds.min_confirmations = if funds.min_confirmations == 0 {
                depth
            } else {
                funds.min_confirmations.min(depth)
            };
            if depth >= coincube_core::claim::MIN_CONFIRMATIONS {
                funds.mature_sats = funds
                    .mature_sats
                    .checked_add(coin.value)
                    .ok_or(ScanError::BodyLimit)?;
                funds.largest_mature_sats = funds.largest_mature_sats.max(coin.value);
            }
        }
    }
    Ok(funds)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Branch {
    External,
    Internal,
}

/// Discovery plus an explicit per-route signing matrix. A route that is not
/// `true` here has no implementation for the descriptor shape and must fail
/// closed; no route grants Claim authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub scan: bool,
    pub signing: SigningRoutes,
    pub claim_authorization: bool,
}

/// Which foreign-wallet signing routes exist for a descriptor shape.
/// `psbt_file`: an external wallet signs the exported PSBT (ECDSA
/// `SIGHASH_ALL` only). `in_app_hardware` (a connected device signs) and
/// `seed_unified` (the user's seeds sign in app) need every key matched to a
/// signer, so they are `true` only when every key is an origin-tagged
/// (master fingerprint and path), ranged xpub (#568 decision U6; a fixed key
/// is refused by P7). `tr` is scan-only: it has no signing route at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SigningRoutes {
    pub psbt_file: bool,
    pub in_app_hardware: bool,
    pub seed_unified: bool,
}

impl SigningRoutes {
    pub const NONE: Self = Self {
        psbt_file: false,
        in_app_hardware: false,
        seed_unified: false,
    };
}

/// Explicit single branch, not an ambiguous multipath expansion. Private keys
/// are never accepted by the public-key parser; hardened origins are labels,
/// whereas hardened public derivation suffixes cannot be derived and refuse.
#[derive(Debug, Clone)]
pub struct ScanDescriptor {
    descriptor: Descriptor<DescriptorPublicKey>,
    branch: Branch,
}
impl ScanDescriptor {
    /// Scan a known COINCUBE Vault policy without applying the narrower
    /// foreign-wallet signing-shape filter. `CoincubeDescriptor` has already
    /// validated the complete policy and split its public receive/change
    /// paths. This grants only address discovery, never foreign signing or
    /// Claim authorization.
    pub(crate) fn for_cube(
        branch: Branch,
        cube: &coincube_core::descriptors::CoincubeDescriptor,
    ) -> Result<Self, ScanError> {
        let text = match branch {
            Branch::External => cube.receive_descriptor().to_string(),
            Branch::Internal => cube.change_descriptor().to_string(),
        };
        let descriptor = Descriptor::<DescriptorPublicKey>::from_str(&text)
            .map_err(|_| ScanError::Descriptor)?;
        descriptor
            .sanity_check()
            .map_err(|_| ScanError::Descriptor)?;
        if descriptor.to_string() != text {
            return Err(ScanError::Descriptor);
        }
        Ok(Self { descriptor, branch })
    }

    pub fn parse(branch: Branch, text: &str) -> Result<Self, ScanError> {
        if text.len() > 16_384 {
            return Err(ScanError::Descriptor);
        }
        let descriptor =
            Descriptor::<DescriptorPublicKey>::from_str(text).map_err(|_| ScanError::Descriptor)?;
        // PR 8's source matrix is intentionally narrower than everything
        // miniscript can parse. In particular, arbitrary wsh policies and
        // Taproot script trees are discovery-capable in principle but have no
        // agreed foreign-wallet signing route. Accepting them here would make
        // the UI promise a later step that cannot be completed safely.
        let supported_shape = match &descriptor {
            Descriptor::Pkh(_) | Descriptor::Wpkh(_) => true,
            Descriptor::Sh(sh) => matches!(sh.as_inner(), ShInner::Wpkh(_)),
            Descriptor::Wsh(wsh) => match wsh.as_inner() {
                WshInner::SortedMulti(_) => true,
                WshInner::Ms(ms) => matches!(ms.as_inner(), Terminal::Multi(_)),
            },
            Descriptor::Tr(tr) => tr.tap_tree().is_none(),
            Descriptor::Bare(_) => false,
        };
        if !supported_shape || descriptor.sanity_check().is_err() {
            return Err(ScanError::Descriptor);
        }
        if !descriptor.for_each_key(|key| match key {
            DescriptorPublicKey::Single(_) => true,
            DescriptorPublicKey::XPub(k) => {
                k.wildcard != Wildcard::Hardened
                    && k.derivation_path.as_ref().iter().all(|n| !n.is_hardened())
                    && k.xkey.network == bitcoin::NetworkKind::Main
            }
            DescriptorPublicKey::MultiXPub(_) => false,
        }) {
            return Err(ScanError::Descriptor);
        }
        Ok(Self { descriptor, branch })
    }
    pub fn capabilities(&self) -> Capabilities {
        let signing = if self.is_taproot() {
            SigningRoutes::NONE
        } else {
            let matched = self.keys_have_origins();
            SigningRoutes {
                psbt_file: true,
                in_app_hardware: matched,
                seed_unified: matched,
            }
        };
        Capabilities {
            scan: true,
            signing,
            claim_authorization: false,
        }
    }
    /// Whether the descriptor has a wildcard, i.e. more than one address.
    pub fn is_ranged(&self) -> bool {
        self.descriptor.has_wildcard()
    }
    pub fn is_taproot(&self) -> bool {
        matches!(self.descriptor, Descriptor::Tr(_))
    }
    /// Every key is a ranged xpub carrying its origin, so a device or a seed
    /// can be matched to it (U6, P7).
    fn keys_have_origins(&self) -> bool {
        self.descriptor.for_each_key(|key| match key {
            DescriptorPublicKey::XPub(xkey) => {
                xkey.origin.is_some() && xkey.wildcard == Wildcard::Unhardened
            }
            DescriptorPublicKey::Single(_) | DescriptorPublicKey::MultiXPub(_) => false,
        })
    }
    /// Select the only valid range for a fixed descriptor while preserving the
    /// caller's explicit bound for wildcard discovery.
    pub fn end_exclusive(&self, wildcard_end_exclusive: u32) -> u32 {
        if self.descriptor.has_wildcard() {
            wildcard_end_exclusive
        } else {
            1
        }
    }
    /// The parsed public descriptor.
    pub(crate) fn descriptor(&self) -> &Descriptor<DescriptorPublicKey> {
        &self.descriptor
    }
    pub(crate) fn canonical(&self) -> String {
        self.descriptor.to_string()
    }
    pub(crate) fn branch(&self) -> Branch {
        self.branch
    }
    pub(crate) fn derive(
        &self,
        index: u32,
    ) -> Result<Descriptor<coincube_core::miniscript::descriptor::DefiniteDescriptorKey>, ScanError>
    {
        self.descriptor
            .at_derivation_index(index)
            .map_err(|_| ScanError::Descriptor)
    }
    pub(crate) fn script(&self, index: u32) -> Result<ScriptBuf, ScanError> {
        self.derive(index).map(|d| d.script_pubkey())
    }
}

pub struct BranchRange {
    pub descriptor: ScanDescriptor,
    pub start: u32,
    pub end_exclusive: u32,
}
pub struct ScanPlan {
    pub chain: ChainId,
    pub branches: Vec<BranchRange>,
    pub gap: u32,
}
impl ScanPlan {
    fn validate(&self) -> Result<(), ScanError> {
        if !matches!(self.chain, ChainId::Bitcoin | ChainId::BitcoinBlake2b) {
            return Err(ScanError::UnsupportedChain);
        }
        if self.gap == 0 || self.gap > 100 || self.branches.is_empty() || self.branches.len() > 2 {
            return Err(ScanError::InvalidLimits);
        }
        let mut seen = BTreeSet::new();
        for range in &self.branches {
            if !seen.insert(range.descriptor.branch)
                || range.start >= range.end_exclusive
                || range.end_exclusive > (1 << 31)
                || (!range.descriptor.descriptor.has_wildcard()
                    && (range.start != 0 || range.end_exclusive != 1))
            {
                return Err(ScanError::InvalidLimits);
            }
        }
        Ok(())
    }
}

/// A full imported-wallet walk has two matching ranged branches. Its caller
/// supplies the already-attested discovery target; a history gap cannot
/// shorten this range.
pub(crate) fn validate_imported_plan(plan: &ScanPlan) -> Result<u32, ScanError> {
    plan.validate()?;
    if plan.chain != ChainId::BitcoinBlake2b {
        return Err(ScanError::InvalidLimits);
    }
    validate_imported_shape(plan)
}

fn validate_imported_shape(plan: &ScanPlan) -> Result<u32, ScanError> {
    if plan.branches.len() != 2 {
        return Err(ScanError::InvalidLimits);
    }
    let external = plan
        .branches
        .iter()
        .find(|range| range.descriptor.branch() == Branch::External);
    let internal = plan
        .branches
        .iter()
        .find(|range| range.descriptor.branch() == Branch::Internal);
    let (Some(external), Some(internal)) = (external, internal) else {
        return Err(ScanError::InvalidLimits);
    };
    if external.start != 0
        || internal.start != 0
        || !external.descriptor.is_ranged()
        || !internal.descriptor.is_ranged()
        || external.end_exclusive != internal.end_exclusive
        || external.end_exclusive == 0
        || external.end_exclusive > MAX_IMPORTED_BRANCH_END
    {
        return Err(ScanError::InvalidLimits);
    }
    Ok(external.end_exclusive)
}

#[derive(Debug, Clone)]
pub struct DiscoveredCoin {
    pub branch: Branch,
    pub index: u32,
    pub outpoint: OutPoint,
    pub output: bitcoin::TxOut,
    pub previous: Transaction,
    pub confirmed: bool,
    /// Confirming block from the same fresh scan observation; `None` exactly
    /// when unconfirmed.
    pub block_height: Option<u32>,
    pub block_hash: Option<BlockHash>,
}

/// Where a coin sits relative to the observed BTCB2 fork activation height.
/// Only `PreFork` coins can exist on both chains and be swept by Split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkSide {
    PreFork,
    PostFork,
    Unconfirmed,
    /// No observed fork height, or no confirming height: fail closed.
    Unknown,
}

/// The exact index range one branch walk proved, and the highest index whose
/// address had any chain or mempool history. Indices in
/// `start..end_exclusive` above `last_used` were observed unused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchCoverage {
    pub branch: Branch,
    pub start: u32,
    pub end_exclusive: u32,
    pub last_used: Option<u32>,
}

impl BranchCoverage {
    pub fn contains(&self, index: u32) -> bool {
        (self.start..self.end_exclusive).contains(&index)
    }
}

/// Complete only within the caller's bounded history-gap policy. No exclusive
/// funds classification and no conversion into a signing or Claim capability.
#[derive(Debug, Clone)]
pub struct ScanReport {
    chain: ChainId,
    generation: u64,
    tip: BlockHash,
    /// Height of `tip`, read at the start of the scan and bound to it: the
    /// block was in the best chain and the tip did not move before the scan
    /// finished.
    tip_height: u32,
    /// Fork activation height from the authenticated BTCB2 network anchor
    /// observed at the scan tip. Never a local constant.
    fork_height: Option<u64>,
    addresses: u32,
    coverage: Vec<BranchCoverage>,
    /// Exact addresses whose stable chain and mempool histories were empty.
    /// A later used address must not erase an earlier unused reservation.
    unused_addresses: BTreeSet<(Branch, u32)>,
    coins: Vec<DiscoveredCoin>,
}
impl ScanReport {
    pub fn coverage(&self, branch: Branch) -> Option<BranchCoverage> {
        self.coverage.iter().copied().find(|c| c.branch == branch)
    }
    pub(crate) fn observed_unused(&self, branch: Branch, index: u32) -> bool {
        self.coverage(branch)
            .is_some_and(|range| range.contains(index))
            && self.unused_addresses.contains(&(branch, index))
    }
    #[cfg(test)]
    pub(crate) fn with_coverage(mut self, coverage: Vec<BranchCoverage>) -> Self {
        // Synthetic coverage alone only proves the suffix above last_used.
        self.unused_addresses = coverage
            .iter()
            .flat_map(|range| {
                let start = range
                    .last_used
                    .and_then(|index| index.checked_add(1))
                    .unwrap_or(range.start)
                    .max(range.start);
                (start..range.end_exclusive).map(move |index| (range.branch, index))
            })
            .collect();
        self.coverage = coverage;
        self
    }
    pub fn fork_height(&self) -> Option<u64> {
        self.fork_height
    }
    pub fn fork_side(&self, coin: &DiscoveredCoin) -> ForkSide {
        if !coin.confirmed {
            return ForkSide::Unconfirmed;
        }
        match (self.fork_height, coin.block_height, coin.block_hash) {
            (Some(fork), Some(height), Some(_)) if u64::from(height) < fork => ForkSide::PreFork,
            (Some(_), Some(_), Some(_)) => ForkSide::PostFork,
            _ => ForkSide::Unknown,
        }
    }
    #[cfg(test)]
    pub(crate) fn with_fork_height(mut self, fork_height: Option<u64>) -> Self {
        self.fork_height = fork_height;
        self
    }
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn tip(&self) -> BlockHash {
        self.tip
    }
    /// Height of [`Self::tip`] on this report's chain.
    pub fn tip_height(&self) -> u32 {
        self.tip_height
    }
    #[cfg(test)]
    pub(crate) fn with_tip_height(mut self, tip_height: u32) -> Self {
        self.tip_height = tip_height;
        self
    }
    pub fn addresses_scanned(&self) -> u32 {
        self.addresses
    }
    pub fn coins(&self) -> &[DiscoveredCoin] {
        &self.coins
    }
    #[cfg(test)]
    pub(crate) fn for_test(
        chain: ChainId,
        generation: u64,
        tip: BlockHash,
        coins: Vec<DiscoveredCoin>,
    ) -> Self {
        Self {
            chain,
            generation,
            tip,
            tip_height: 0,
            fork_height: None,
            addresses: 1,
            coverage: Vec::new(),
            unused_addresses: BTreeSet::new(),
            coins,
        }
    }
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct Stats {
    chain_stats: Count,
    mempool_stats: Count,
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct Count {
    tx_count: u64,
}
impl Stats {
    fn used(&self) -> bool {
        self.chain_stats.tx_count != 0 || self.mempool_stats.tx_count != 0
    }
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct Utxo {
    txid: Txid,
    vout: u32,
    value: u64,
    status: Status,
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct Status {
    confirmed: bool,
    block_height: Option<u32>,
    block_hash: Option<BlockHash>,
}

/// Esplora's UTXO list has no order contract. Compare complete records by
/// outpoint, refusing duplicate records in either observation.
fn same_utxos(before: &[Utxo], after: &[Utxo]) -> Result<bool, ScanError> {
    fn by_outpoint(utxos: &[Utxo]) -> Result<BTreeMap<OutPoint, &Utxo>, ScanError> {
        if utxos.len() > MAX_UTXOS {
            return Err(ScanError::BodyLimit);
        }
        let mut by_outpoint = BTreeMap::new();
        for coin in utxos {
            let outpoint = OutPoint {
                txid: coin.txid,
                vout: coin.vout,
            };
            if outpoint.is_null() || by_outpoint.insert(outpoint, coin).is_some() {
                return Err(ScanError::Malformed);
            }
        }
        Ok(by_outpoint)
    }
    Ok(by_outpoint(before)? == by_outpoint(after)?)
}

#[async_trait]
trait Source: Send + Sync {
    async fn tip(&self, chain: ChainId) -> Result<BlockHash, ScanError>;
    /// Height of `tip`, which must be a block in the best chain.
    async fn tip_height(&self, chain: ChainId, tip: BlockHash) -> Result<u32, ScanError>;
    /// The BTCB2 anchor's tip and its active fork height, if reported.
    async fn anchor(&self) -> Result<(BlockHash, Option<u64>), ScanError>;
    async fn stats(&self, chain: ChainId, address: &str) -> Result<Stats, ScanError>;
    async fn utxos(&self, chain: ChainId, address: &str) -> Result<Vec<Utxo>, ScanError>;
    async fn transaction(&self, chain: ChainId, txid: Txid) -> Result<Transaction, ScanError>;
}

/// Recreate the client context on account/provider changes and increment the
/// supplied generation on logout/cancel. Caller must recheck generation when
/// consuming a delivered result. Errors mean incomplete discovery, never zero funds.
pub async fn scan(
    client: CoincubeClient,
    plan: ScanPlan,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<ScanReport, ScanError> {
    scan_with_timeout(client, plan, expected, generation, MAX_DURATION).await
}

pub async fn scan_with_timeout(
    client: CoincubeClient,
    plan: ScanPlan,
    expected: u64,
    generation: watch::Receiver<u64>,
    duration: Duration,
) -> Result<ScanReport, ScanError> {
    scan_mode_with_timeout(client, plan, expected, generation, duration, ScanMode::Gap).await
}

pub(crate) async fn scan_imported_window(
    client: CoincubeClient,
    plan: ScanPlan,
    expected: u64,
    generation: watch::Receiver<u64>,
) -> Result<ScanReport, ScanError> {
    plan.validate()?;
    let branch_end = validate_imported_shape(&plan)?;
    scan_mode_with_timeout(
        client,
        plan,
        expected,
        generation,
        imported_scan_duration(branch_end),
        ScanMode::FullImported,
    )
    .await
}

/// One second per eight requested addresses across the two branches, with
/// Claim's 90-second floor. Validation bounds each branch at 10,000, so this
/// deadline cannot exceed 2,500 seconds. Both chains scan concurrently and
/// each gets this same deadline.
fn imported_scan_duration(branch_end: u32) -> Duration {
    CLAIM_SCAN_DURATION.max(Duration::from_secs(u64::from(
        (branch_end * 2).div_ceil(ADDRESS_BATCH),
    )))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScanMode {
    Gap,
    FullImported,
}

async fn scan_mode_with_timeout(
    client: CoincubeClient,
    plan: ScanPlan,
    expected: u64,
    mut generation: watch::Receiver<u64>,
    duration: Duration,
    mode: ScanMode,
) -> Result<ScanReport, ScanError> {
    plan.validate()?;
    if *generation.borrow() != expected || generation.has_changed().is_err() {
        return Err(ScanError::Cancelled);
    }
    let source = http::HttpSource::new(client)?;
    let cancelled = async {
        loop {
            if generation.changed().await.is_err() || *generation.borrow_and_update() != expected {
                break;
            }
        }
    };
    let result = tokio::select! { biased;
        _ = cancelled => Err(ScanError::Cancelled),
        result = tokio::time::timeout(duration, collect_mode(&source, &plan, expected, mode)) => result.map_err(|_| ScanError::Deadline)?,
    };
    if *generation.borrow() != expected || generation.has_changed().is_err() {
        return Err(ScanError::Cancelled);
    }
    result
}

#[cfg(test)]
async fn collect(
    source: &impl Source,
    plan: &ScanPlan,
    generation: u64,
) -> Result<ScanReport, ScanError> {
    collect_mode(source, plan, generation, ScanMode::Gap).await
}

async fn collect_mode(
    source: &impl Source,
    plan: &ScanPlan,
    generation: u64,
    mode: ScanMode,
) -> Result<ScanReport, ScanError> {
    plan.validate()?;
    let before = source.tip(plan.chain).await?;
    // Bound to `before`: the closing tip recheck below proves it still held.
    let tip_height = source.tip_height(plan.chain, before).await?;
    let mut fork_height = None;
    if plan.chain == ChainId::BitcoinBlake2b {
        let (anchor, fork) = source.anchor().await?;
        if anchor != before {
            return Err(ScanError::Changed(ChangedReason::AnchorMismatch));
        }
        fork_height = fork;
    }
    let mut report = ScanReport {
        chain: plan.chain,
        generation,
        tip: before,
        tip_height,
        fork_height,
        addresses: 0,
        coverage: Vec::new(),
        unused_addresses: BTreeSet::new(),
        coins: Vec::new(),
    };
    let mut outpoints = BTreeSet::new();
    let mut scripts = BTreeSet::new();
    for range in &plan.branches {
        let mut gap = 0;
        let mut last_used = None;
        let mut finished = false;
        let mut next = range.start;
        while next < range.end_exclusive {
            let address_limit = if mode == ScanMode::FullImported {
                MAX_IMPORTED_ADDRESSES
            } else {
                MAX_ADDRESSES
            };
            if report.addresses >= address_limit {
                return Err(ScanError::AddressLimit);
            }
            // Never read beyond the current gap boundary or the address cap
            // merely to fill a batch. A used address resets the gap when its
            // result is admitted below, exactly as in the sequential scan.
            let gap_budget = if mode == ScanMode::Gap {
                plan.gap.saturating_sub(gap)
            } else {
                ADDRESS_BATCH
            };
            let count = ADDRESS_BATCH
                .min(gap_budget)
                .min(range.end_exclusive - next)
                .min(address_limit - report.addresses);
            let addresses = (next..next + count)
                .map(|index| {
                    let script = range.descriptor.script(index)?;
                    if !scripts.insert(script.clone()) {
                        return Err(ScanError::Descriptor);
                    }
                    let address = bitcoin::Address::from_script(&script, bitcoin::Network::Bitcoin)
                        .map_err(|_| ScanError::Descriptor)?
                        .to_string();
                    Ok((index, script, address))
                })
                .collect::<Result<Vec<_>, ScanError>>()?;
            let observations = join_all(addresses.iter().map(|(_, _, address)| async move {
                let (stats, utxos) = tokio::try_join!(
                    source.stats(plan.chain, address),
                    source.utxos(plan.chain, address),
                )?;
                if utxos.len() > MAX_UTXOS {
                    return Err(ScanError::BodyLimit);
                }
                if !stats.used() && !utxos.is_empty() {
                    return Err(ScanError::Malformed);
                }
                if utxos.iter().any(|coin| {
                    let outpoint = OutPoint {
                        txid: coin.txid,
                        vout: coin.vout,
                    };
                    outpoint.is_null()
                        || coin.value > bitcoin::Amount::MAX_MONEY.to_sat()
                        || (coin.status.confirmed
                            && (coin.status.block_hash.is_none()
                                || coin.status.block_height.is_none()))
                        || (!coin.status.confirmed
                            && (coin.status.block_hash.is_some()
                                || coin.status.block_height.is_some()))
                }) {
                    return Err(ScanError::Malformed);
                }
                let mut previous = Vec::with_capacity(utxos.len());
                for coin in &utxos {
                    previous.push(source.transaction(plan.chain, coin.txid).await?);
                }
                // A same-tip mempool change must not turn an empty UTXO list
                // into an "unused" address. Re-read after fetching prevouts.
                let (stats_again, utxos_again) = tokio::try_join!(
                    source.stats(plan.chain, address),
                    source.utxos(plan.chain, address),
                )?;
                Ok::<_, ScanError>((stats, utxos, previous, stats_again, utxos_again))
            }))
            .await;
            for ((index, script, _), observed) in addresses.into_iter().zip(observations) {
                let (stats, utxos, previous, stats_again, utxos_again) = observed?;
                if utxos.len() > MAX_UTXOS.saturating_sub(report.coins.len()) {
                    return Err(ScanError::BodyLimit);
                }
                if !stats.used() && !utxos.is_empty() {
                    return Err(ScanError::Malformed);
                }
                for (coin, previous) in utxos.iter().zip(previous) {
                    let outpoint = OutPoint {
                        txid: coin.txid,
                        vout: coin.vout,
                    };
                    if outpoint.is_null()
                        || !outpoints.insert(outpoint)
                        || coin.value > bitcoin::Amount::MAX_MONEY.to_sat()
                        || (coin.status.confirmed
                            && (coin.status.block_hash.is_none()
                                || coin.status.block_height.is_none()))
                        || (!coin.status.confirmed
                            && (coin.status.block_hash.is_some()
                                || coin.status.block_height.is_some()))
                    {
                        return Err(ScanError::Malformed);
                    }
                    let output = coincube_core::spend::authenticate_previous_output(
                        &outpoint,
                        Some(&previous),
                        None,
                    )
                    .map_err(|_| ScanError::Prevout)?;
                    if output.value.to_sat() != coin.value || output.script_pubkey != script {
                        return Err(ScanError::Prevout);
                    }
                    report.coins.push(DiscoveredCoin {
                        branch: range.descriptor.branch,
                        index,
                        outpoint,
                        output,
                        previous,
                        confirmed: coin.status.confirmed,
                        block_height: coin.status.block_height,
                        block_hash: coin.status.block_hash,
                    });
                }
                let utxos_same = same_utxos(&utxos, &utxos_again)?;
                if stats != stats_again || !utxos_same {
                    return Err(ScanError::Changed(ChangedReason::AddressState));
                }
                report.addresses += 1;
                if stats.used() {
                    last_used = Some(index);
                } else {
                    report
                        .unused_addresses
                        .insert((range.descriptor.branch, index));
                }
                gap = if stats.used() { 0 } else { gap + 1 };
                if mode == ScanMode::Gap
                    && (gap >= plan.gap || !range.descriptor.descriptor.has_wildcard())
                {
                    report.coverage.push(BranchCoverage {
                        branch: range.descriptor.branch,
                        start: range.start,
                        end_exclusive: index + 1,
                        last_used,
                    });
                    finished = true;
                    break;
                }
            }
            if finished {
                break;
            }
            next += count;
        }
        if mode == ScanMode::FullImported && next == range.end_exclusive {
            report.coverage.push(BranchCoverage {
                branch: range.descriptor.branch,
                start: range.start,
                end_exclusive: range.end_exclusive,
                last_used,
            });
            finished = true;
        }
        if !finished {
            return Err(ScanError::RangeLimit);
        }
    }
    if before != source.tip(plan.chain).await? {
        return Err(ScanError::Changed(ChangedReason::TipMoved));
    }
    if plan.chain == ChainId::BitcoinBlake2b {
        let (anchor, closing_fork_height) = source.anchor().await?;
        if anchor != before {
            return Err(ScanError::Changed(ChangedReason::AnchorMismatch));
        }
        if closing_fork_height != fork_height {
            return Err(ScanError::Changed(ChangedReason::AnchorChanged));
        }
    }
    Ok(report)
}
