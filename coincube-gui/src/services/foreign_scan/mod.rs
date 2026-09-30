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
use serde::Deserialize;
use std::{collections::BTreeSet, str::FromStr, time::Duration};
use tokio::sync::watch;

pub const MAX_ADDRESSES: u32 = 200;
pub const MAX_DURATION: Duration = Duration::from_secs(30);
const MAX_UTXOS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanError {
    UnsupportedChain,
    Descriptor,
    InvalidLimits,
    Cancelled,
    Deadline,
    Unavailable,
    Http(u16),
    Malformed,
    Freshness,
    Changed,
    RangeLimit,
    AddressLimit,
    BodyLimit,
    Prevout,
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
/// `SIGHASH_ALL` only). `in_app_hardware` and `seed_unified` are not
/// implemented for foreign descriptors yet, so they are `false` for every
/// shape. `tr` is scan-only: it has no signing route at all.
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
            SigningRoutes {
                psbt_file: true,
                ..SigningRoutes::NONE
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
    /// Select the only valid range for a fixed descriptor while preserving the
    /// caller's explicit bound for wildcard discovery.
    pub fn end_exclusive(&self, wildcard_end_exclusive: u32) -> u32 {
        if self.descriptor.has_wildcard() {
            wildcard_end_exclusive
        } else {
            1
        }
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
    /// Fork activation height from the authenticated BTCB2 network anchor
    /// observed at the scan tip. Never a local constant.
    fork_height: Option<u64>,
    addresses: u32,
    coverage: Vec<BranchCoverage>,
    coins: Vec<DiscoveredCoin>,
}
impl ScanReport {
    pub fn coverage(&self, branch: Branch) -> Option<BranchCoverage> {
        self.coverage.iter().copied().find(|c| c.branch == branch)
    }
    #[cfg(test)]
    pub(crate) fn with_coverage(mut self, coverage: Vec<BranchCoverage>) -> Self {
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
            fork_height: None,
            addresses: 1,
            coverage: Vec::new(),
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

#[async_trait]
trait Source: Send + Sync {
    async fn tip(&self, chain: ChainId) -> Result<BlockHash, ScanError>;
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
    mut generation: watch::Receiver<u64>,
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
        result = tokio::time::timeout(MAX_DURATION, collect(&source, &plan, expected)) => result.map_err(|_| ScanError::Deadline)?,
    };
    if *generation.borrow() != expected || generation.has_changed().is_err() {
        return Err(ScanError::Cancelled);
    }
    result
}

async fn collect(
    source: &impl Source,
    plan: &ScanPlan,
    generation: u64,
) -> Result<ScanReport, ScanError> {
    plan.validate()?;
    let before = source.tip(plan.chain).await?;
    let mut fork_height = None;
    if plan.chain == ChainId::BitcoinBlake2b {
        let (anchor, fork) = source.anchor().await?;
        if anchor != before {
            return Err(ScanError::Changed);
        }
        fork_height = fork;
    }
    let mut report = ScanReport {
        chain: plan.chain,
        generation,
        tip: before,
        fork_height,
        addresses: 0,
        coverage: Vec::new(),
        coins: Vec::new(),
    };
    let mut outpoints = BTreeSet::new();
    let mut scripts = BTreeSet::new();
    for range in &plan.branches {
        let mut gap = 0;
        let mut last_used = None;
        let mut finished = false;
        for index in range.start..range.end_exclusive {
            if report.addresses >= MAX_ADDRESSES {
                return Err(ScanError::AddressLimit);
            }
            let script = range.descriptor.script(index)?;
            if !scripts.insert(script.clone()) {
                return Err(ScanError::Descriptor);
            }
            let address = bitcoin::Address::from_script(&script, bitcoin::Network::Bitcoin)
                .map_err(|_| ScanError::Descriptor)?
                .to_string();
            let stats = source.stats(plan.chain, &address).await?;
            let utxos = source.utxos(plan.chain, &address).await?;
            if utxos.len() > MAX_UTXOS.saturating_sub(report.coins.len()) {
                return Err(ScanError::BodyLimit);
            }
            if !stats.used() && !utxos.is_empty() {
                return Err(ScanError::Malformed);
            }
            for coin in &utxos {
                let outpoint = OutPoint {
                    txid: coin.txid,
                    vout: coin.vout,
                };
                if outpoint.is_null()
                    || !outpoints.insert(outpoint)
                    || coin.value > bitcoin::Amount::MAX_MONEY.to_sat()
                    || (coin.status.confirmed
                        && (coin.status.block_hash.is_none() || coin.status.block_height.is_none()))
                    || (!coin.status.confirmed
                        && (coin.status.block_hash.is_some() || coin.status.block_height.is_some()))
                {
                    return Err(ScanError::Malformed);
                }
                let previous = source.transaction(plan.chain, coin.txid).await?;
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
            // A same-tip mempool change must not turn an empty UTXO list into an
            // "unused" address. Compare both dynamic observations once more.
            if stats != source.stats(plan.chain, &address).await?
                || utxos != source.utxos(plan.chain, &address).await?
            {
                return Err(ScanError::Changed);
            }
            report.addresses += 1;
            if stats.used() {
                last_used = Some(index);
            }
            gap = if stats.used() { 0 } else { gap + 1 };
            if gap >= plan.gap || !range.descriptor.descriptor.has_wildcard() {
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
        if !finished {
            return Err(ScanError::RangeLimit);
        }
    }
    if before != source.tip(plan.chain).await?
        || (plan.chain == ChainId::BitcoinBlake2b
            && source.anchor().await? != (before, fork_height))
    {
        return Err(ScanError::Changed);
    }
    Ok(report)
}
