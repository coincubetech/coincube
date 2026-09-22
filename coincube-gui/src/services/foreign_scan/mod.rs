//! Scan-only public descriptor discovery. No import, persistence or spend permission.
//! A history gap bounds discovery; it cannot prove that later addresses are unused.
mod http;
#[cfg(test)]
mod tests;

use super::coincube::CoincubeClient;
use async_trait::async_trait;
use coincube_core::{
    chain::ChainId,
    miniscript::{
        bitcoin::{self, BlockHash, OutPoint, ScriptBuf, Transaction, Txid},
        descriptor::{DescriptorType, Wildcard},
        Descriptor, DescriptorPublicKey, ForEachKey,
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

/// This matrix describes discovery only. No supported descriptor grants signing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub scan: bool,
    pub unified_signing: bool,
    pub claim_authorization: bool,
}

/// Explicit single branch, not an ambiguous multipath expansion. Private keys
/// are never accepted by the public-key parser; hardened origins are labels,
/// whereas hardened public derivation suffixes cannot be derived and refuse.
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
        if !matches!(
            descriptor.desc_type(),
            DescriptorType::Pkh
                | DescriptorType::Wpkh
                | DescriptorType::ShWpkh
                | DescriptorType::Wsh
                | DescriptorType::WshSortedMulti
                | DescriptorType::Tr
        ) || descriptor.sanity_check().is_err()
        {
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
        Capabilities {
            scan: true,
            unified_signing: false,
            claim_authorization: false,
        }
    }
    fn script(&self, index: u32) -> Result<ScriptBuf, ScanError> {
        self.descriptor
            .at_derivation_index(index)
            .map(|d| d.script_pubkey())
            .map_err(|_| ScanError::Descriptor)
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
}
/// Complete only within the caller's bounded history-gap policy. No exclusive
/// funds classification and no conversion into a signing or Claim capability.
pub struct ScanReport {
    chain: ChainId,
    generation: u64,
    tip: BlockHash,
    addresses: u32,
    coins: Vec<DiscoveredCoin>,
}
impl ScanReport {
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
    async fn anchor(&self) -> Result<BlockHash, ScanError>;
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
    if plan.chain == ChainId::BitcoinBlake2b && source.anchor().await? != before {
        return Err(ScanError::Changed);
    }
    let mut report = ScanReport {
        chain: plan.chain,
        generation,
        tip: before,
        addresses: 0,
        coins: Vec::new(),
    };
    let mut outpoints = BTreeSet::new();
    let mut scripts = BTreeSet::new();
    for range in &plan.branches {
        let mut gap = 0;
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
            gap = if stats.used() { 0 } else { gap + 1 };
            if gap >= plan.gap || !range.descriptor.descriptor.has_wildcard() {
                finished = true;
                break;
            }
        }
        if !finished {
            return Err(ScanError::RangeLimit);
        }
    }
    if before != source.tip(plan.chain).await?
        || (plan.chain == ChainId::BitcoinBlake2b && source.anchor().await? != before)
    {
        return Err(ScanError::Changed);
    }
    Ok(report)
}
