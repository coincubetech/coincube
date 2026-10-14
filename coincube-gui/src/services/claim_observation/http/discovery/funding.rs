//! Fresh, paired funding observations for the ancestry split. These checks
//! establish provider-reported chain status, not source-wallet ownership,
//! reservation, signer admission or broadcast authority.

use super::*;
use coincube_core::{claim::MIN_CONFIRMATIONS, claim_ancestry::retained::RetainedPath};

const COINBASE_MATURITY: u64 = 100;

fn check_funding_status(
    block: BlockRef,
    root: BlockRef,
    tip: BlockRef,
    canonical: BlockHash,
    spent: bool,
) -> Result<(), FailureKind> {
    if block.height < root.height || block.height > tip.height || canonical != block.hash || spent {
        return Err(FailureKind::Changed);
    }
    let depth = tip.height - block.height + 1;
    let maturity = tip.height - root.height + 1;
    if depth < MIN_CONFIRMATIONS || maturity < COINBASE_MATURITY {
        return Err(FailureKind::Changed);
    }
    Ok(())
}

#[derive(Debug)]
pub struct CollectedFundingCoin {
    ancestry: DiscoveredAncestry,
    chain: ChainId,
    selected: OutPoint,
    block: BlockRef,
    tip: BlockRef,
    observed_at: i64,
}

impl CollectedFundingCoin {
    pub fn ancestry(&self) -> &DiscoveredAncestry {
        &self.ancestry
    }
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    pub fn selected(&self) -> OutPoint {
        self.selected
    }
    pub fn block(&self) -> BlockRef {
        self.block
    }
    pub fn tip(&self) -> BlockRef {
        self.tip
    }
    pub fn observed_at(&self) -> i64 {
        self.observed_at
    }
}

/// Both chain-specific inputs observed under one provider and session, with
/// the same two tips and fork anchor throughout collection. This is still
/// read-only evidence: a caller must bind the exact descriptors, full source
/// UTXO set, intent, current reservations and fresh preflight at use.
#[derive(Debug)]
pub struct CollectedFundingPair {
    bitcoin: CollectedFundingCoin,
    fork: CollectedFundingCoin,
    observed_at: i64,
}

impl CollectedFundingPair {
    pub fn bitcoin(&self) -> &CollectedFundingCoin {
        &self.bitcoin
    }
    pub fn fork(&self) -> &CollectedFundingCoin {
        &self.fork
    }
    pub fn observed_at(&self) -> i64 {
        self.observed_at
    }
}

impl HttpObservationSource {
    /// Requalify one retained path, then bracket selected-output confirmation
    /// and outspend reads with the same canonical tip used by its root pair.
    /// No 404 or missing status is interpreted as an unspent coin.
    pub async fn collect_funding_coin(
        &self,
        path: &RetainedPath,
        chain: ChainId,
        policy: Policy,
    ) -> Result<CollectedFundingCoin, FailureKind> {
        if !matches!(chain, ChainId::Bitcoin | ChainId::BitcoinBlake2b)
            || policy.max_observation_age_seconds <= 0
        {
            return Err(FailureKind::InvalidPlan);
        }
        let snapshot = self.discovery_snapshot();
        tokio::time::timeout(MAX_COLLECTION_TIME, async {
            let first = snapshot.tip(chain).await?;
            let ancestry = match chain {
                ChainId::Bitcoin => snapshot.requalify_ancestry(path, policy).await,
                ChainId::BitcoinBlake2b => snapshot.requalify_fork_ancestry(path, policy).await,
                _ => return Err(FailureKind::WrongChain),
            }
            .map_err(|error| match error {
                DiscoveryError::Structural(_) => FailureKind::Malformed,
                DiscoveryError::Observation(kind) => kind,
            })?;
            let pair = ancestry.pair();
            let root = match chain {
                ChainId::Bitcoin => pair.bitcoin(),
                ChainId::BitcoinBlake2b => pair.fork(),
                _ => return Err(FailureKind::WrongChain),
            };
            if pair.root_chain() != chain || first.value() != &root.tip {
                return Err(FailureKind::Changed);
            }
            let selected = path.selected();
            let (transaction, transaction_stamp) = super::super::super::read(
                snapshot.transaction(chain, selected.txid).await?,
                chain,
                &snapshot,
                policy,
                Stage::BitcoinInclusion,
            )
            .map_err(|error| error.kind)?;
            let TransactionObservation::Confirmed { block, .. } = transaction else {
                return Err(FailureKind::Changed);
            };
            let (canonical, canonical_stamp) = super::super::super::read(
                snapshot.hash_at_height(chain, block.height).await?,
                chain,
                &snapshot,
                policy,
                Stage::BitcoinInclusion,
            )
            .map_err(|error| error.kind)?;
            let (outspend, outspend_stamp) = super::super::super::read(
                snapshot.outspend(chain, selected).await?,
                chain,
                &snapshot,
                policy,
                Stage::Preflight,
            )
            .map_err(|error| error.kind)?;
            check_funding_status(block, root.block, root.tip, canonical, outspend.spent)?;
            let last = snapshot.tip(chain).await?;
            if last.value() != first.value() {
                return Err(FailureKind::Changed);
            }
            let observed_at = first
                .observed_at()
                .min(ancestry.observed_at())
                .min(transaction_stamp)
                .min(canonical_stamp)
                .min(outspend_stamp)
                .min(last.observed_at());
            if !super::super::super::fresh(
                observed_at,
                snapshot.now(),
                policy.max_observation_age_seconds,
            ) {
                return Err(FailureKind::Stale);
            }
            snapshot.validate_context(&ancestry.provider, ancestry.generation())?;
            Ok(CollectedFundingCoin {
                ancestry,
                chain,
                selected,
                block,
                tip: *first.value(),
                observed_at,
            })
        })
        .await
        .map_err(|_| FailureKind::Deadline)?
    }

    /// Collect both markers with a shared request budget. Their paired root
    /// views, session and tips must agree; a final two-chain tip read brackets
    /// the entire operation. Neither candidate is promoted to spend authority.
    pub async fn collect_funding_pair(
        &self,
        bitcoin_path: &RetainedPath,
        fork_path: &RetainedPath,
        policy: Policy,
    ) -> Result<CollectedFundingPair, FailureKind> {
        if bitcoin_path.selected() == fork_path.selected() {
            return Err(FailureKind::InvalidPlan);
        }
        let snapshot = self.discovery_snapshot();
        tokio::time::timeout(MAX_COLLECTION_TIME, async {
            let bitcoin = snapshot
                .collect_funding_coin(bitcoin_path, ChainId::Bitcoin, policy)
                .await?;
            let fork = snapshot
                .collect_funding_coin(fork_path, ChainId::BitcoinBlake2b, policy)
                .await?;
            let a = bitcoin.ancestry.pair();
            let b = fork.ancestry.pair();
            let anchor_a = a.anchor();
            let anchor_b = b.anchor();
            if bitcoin.ancestry.provider != fork.ancestry.provider
                || a.generation() != b.generation()
                || a.bitcoin().tip != b.bitcoin().tip
                || a.fork().tip != b.fork().tip
                || anchor_a.tip_hash != anchor_b.tip_hash
                || anchor_a.tip_height != anchor_b.tip_height
                || anchor_a.tip_median_time_past != anchor_b.tip_median_time_past
                || anchor_a.observation != anchor_b.observation
            {
                return Err(FailureKind::Changed);
            }
            let bitcoin_tip = snapshot.tip(ChainId::Bitcoin).await?;
            let fork_tip = snapshot.tip(ChainId::BitcoinBlake2b).await?;
            if bitcoin_tip.value() != &a.bitcoin().tip || fork_tip.value() != &a.fork().tip {
                return Err(FailureKind::Changed);
            }
            let observed_at = bitcoin
                .observed_at
                .min(fork.observed_at)
                .min(bitcoin_tip.observed_at())
                .min(fork_tip.observed_at());
            if !super::super::super::fresh(
                observed_at,
                snapshot.now(),
                policy.max_observation_age_seconds,
            ) {
                return Err(FailureKind::Stale);
            }
            snapshot.validate_context(&bitcoin.ancestry.provider, a.generation())?;
            Ok(CollectedFundingPair {
                bitcoin,
                fork,
                observed_at,
            })
        })
        .await
        .map_err(|_| FailureKind::Deadline)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::hashes::Hash;

    fn at(height: u64, byte: u8) -> BlockRef {
        BlockRef {
            height,
            hash: BlockHash::from_byte_array([byte; 32]),
        }
    }

    #[test]
    fn funding_status_requires_canonical_depth_maturity_and_unspentness() {
        let root = at(100, 1);
        let block = at(194, 2);
        let tip = at(199, 3);
        assert_eq!(
            check_funding_status(block, root, tip, block.hash, false),
            Ok(())
        );
        for (block, root, tip, hash, spent) in [
            (at(195, 2), root, tip, block.hash, false), // five confirmations
            (at(101, 2), at(101, 1), tip, block.hash, false), // immature root
            (at(99, 2), root, tip, block.hash, false),  // before root
            (at(200, 2), root, tip, block.hash, false), // beyond tip
            (block, root, tip, at(194, 9).hash, false), // wrong canonical block
            (block, root, tip, block.hash, true),       // already spent
        ] {
            assert_eq!(
                check_funding_status(block, root, tip, hash, spent),
                Err(FailureKind::Changed)
            );
        }
    }
}
