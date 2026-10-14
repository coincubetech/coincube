//! Fresh, paired funding observations for the ancestry split. These checks
//! establish provider-reported chain status, not source-wallet ownership,
//! reservation, signer admission or broadcast authority.

use super::*;
use coincube_core::{
    claim::{PreflightTips, MIN_CONFIRMATIONS},
    claim_ancestry::retained::RetainedPath,
    claim_spend::{AncestrySelfTransfer, ClaimForkSweep},
    descriptors::CoincubeDescriptor,
    foreign_split::{AncestrySplitStep1, AncestrySplitStep2, SplitCoin},
    miniscript::bitcoin::{consensus::deserialize, Transaction},
    spend::CandidateCoin,
};
use std::collections::BTreeSet;

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

    pub(crate) fn validate_candidate(
        &self,
        path: &RetainedPath,
        candidate: &SplitCoin,
    ) -> Result<(), FailureKind> {
        let raw = path.links().first().ok_or(FailureKind::Malformed)?;
        let selected_tx: Transaction =
            deserialize(&raw.transaction).map_err(|_| FailureKind::Malformed)?;
        if path.selected() != self.selected
            || self
                .ancestry
                .retained_path()
                .map_err(|_| FailureKind::Malformed)?
                .encode()
                != path.encode()
            || candidate.outpoint != self.selected
            || candidate.previous != selected_tx
            || candidate.previous.compute_txid() != self.selected.txid
            || candidate
                .previous
                .output
                .get(self.selected.vout as usize)
                .is_none()
        {
            return Err(FailureKind::Changed);
        }
        match self.chain {
            ChainId::Bitcoin
                if candidate.bitcoin_block == Some(self.block)
                    && candidate.btcb2_block.is_none() =>
            {
                Ok(())
            }
            ChainId::BitcoinBlake2b
                if candidate.btcb2_block == Some(self.block)
                    && candidate.bitcoin_block.is_none() =>
            {
                Ok(())
            }
            _ => Err(FailureKind::Changed),
        }
    }

    fn validate_claim_coin(
        &self,
        path: &RetainedPath,
        coin: &CandidateCoin,
        descriptor: &CoincubeDescriptor,
    ) -> Result<(), FailureKind> {
        let raw = path.links().first().ok_or(FailureKind::Malformed)?;
        let previous: Transaction =
            deserialize(&raw.transaction).map_err(|_| FailureKind::Malformed)?;
        let output = previous
            .output
            .get(self.selected.vout as usize)
            .ok_or(FailureKind::Malformed)?;
        let derived = if coin.is_change {
            descriptor.change_descriptor()
        } else {
            descriptor.receive_descriptor()
        }
        .derive(
            coin.deriv_index,
            &coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only(),
        );
        if path.selected() != self.selected
            || self
                .ancestry
                .retained_path()
                .map_err(|_| FailureKind::Malformed)?
                .encode()
                != path.encode()
            || coin.outpoint != self.selected
            || previous.compute_txid() != self.selected.txid
            || coin.amount != output.value
            || derived.script_pubkey() != output.script_pubkey
        {
            return Err(FailureKind::Changed);
        }
        Ok(())
    }
}

/// Current two-chain review context. A stored journal must not persist this
/// as authority: regenerate it from a fresh session and fresh chain tips.
#[derive(Clone, Copy)]
pub struct FundingContext<'a> {
    pub provider: &'a str,
    pub generation: u64,
    pub now: i64,
    pub max_age_seconds: i64,
    pub tips: PreflightTips,
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

/// One read-only collection covering both chain-specific funding coins and
/// every shared pre-fork input. It is bound to one account generation and two
/// stable tips; it is not a reservation or a signing capability.
#[derive(Debug)]
pub struct CollectedSpendInputs {
    funding: CollectedFundingPair,
    shared: Vec<(OutPoint, BlockRef)>,
    observed_at: i64,
}

impl CollectedSpendInputs {
    pub fn funding(&self) -> &CollectedFundingPair {
        &self.funding
    }

    pub fn shared(&self) -> &[(OutPoint, BlockRef)] {
        &self.shared
    }

    pub fn observed_at(&self) -> i64 {
        self.observed_at
    }

    /// Review the Claim's owned constructions against the complete fresh
    /// shared set and both chain-specific funding coins together.
    #[allow(clippy::too_many_arguments)]
    pub fn validate_for_claim(
        &self,
        bitcoin_path: &RetainedPath,
        fork_path: &RetainedPath,
        bitcoin_coin: &CandidateCoin,
        fork_coin: &CandidateCoin,
        bitcoin: &AncestrySelfTransfer,
        fork: &ClaimForkSweep,
        context: FundingContext<'_>,
    ) -> Result<(), FailureKind> {
        self.funding.validate_for_claim(
            bitcoin_path,
            fork_path,
            bitcoin_coin,
            fork_coin,
            bitcoin,
            fork,
            context,
        )?;
        self.validate_for_transactions(
            &bitcoin.psbt().unsigned_tx,
            &fork.psbt().unsigned_tx,
            bitcoin.claimed_prevouts(),
            context,
        )
    }

    /// Review a foreign Split's exact ordinary constructions under the same
    /// complete observation. No result from this method authorizes signing.
    #[allow(clippy::too_many_arguments)]
    pub fn validate_for_split(
        &self,
        bitcoin_path: &RetainedPath,
        fork_path: &RetainedPath,
        bitcoin_coin: &SplitCoin,
        fork_coin: &SplitCoin,
        bitcoin: &AncestrySplitStep1,
        fork: &AncestrySplitStep2,
        context: FundingContext<'_>,
    ) -> Result<(), FailureKind> {
        self.funding.validate_for_split(
            bitcoin_path,
            fork_path,
            bitcoin_coin,
            fork_coin,
            bitcoin,
            fork,
            context,
        )?;
        self.validate_for_transactions(
            &bitcoin.psbt().unsigned_tx,
            &fork.psbt().unsigned_tx,
            bitcoin.claimed_prevouts(),
            context,
        )
    }

    /// Bind the collected shared set to both exact unsigned transactions and
    /// the current session. Ownership still comes from the core builders;
    /// reservations and a new preflight are required before any signing.
    pub fn validate_for_transactions(
        &self,
        bitcoin: &Transaction,
        fork: &Transaction,
        claimed: &[OutPoint],
        context: FundingContext<'_>,
    ) -> Result<(), FailureKind> {
        self.funding.validate_session(context)?;
        if !super::super::super::fresh(self.observed_at, context.now, context.max_age_seconds) {
            return Err(FailureKind::Stale);
        }
        let shared: BTreeSet<_> = self.shared.iter().map(|(outpoint, _)| *outpoint).collect();
        let claimed: BTreeSet<_> = claimed.iter().copied().collect();
        let mut bitcoin_inputs: BTreeSet<_> = bitcoin
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();
        let mut fork_inputs: BTreeSet<_> = fork
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();
        if shared.is_empty()
            || shared.len() != self.shared.len()
            || claimed.len() != self.shared.len()
            || claimed != shared
            || bitcoin_inputs.len() != bitcoin.input.len()
            || fork_inputs.len() != fork.input.len()
            || !bitcoin_inputs.remove(&self.funding.bitcoin.selected)
            || !fork_inputs.remove(&self.funding.fork.selected)
            || bitcoin_inputs != shared
            || fork_inputs != shared
        {
            return Err(FailureKind::Changed);
        }
        Ok(())
    }
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

    fn validate_session(&self, context: FundingContext<'_>) -> Result<(), FailureKind> {
        let a = self.bitcoin.ancestry.pair();
        if context.max_age_seconds <= 0
            || !super::super::super::fresh(self.observed_at, context.now, context.max_age_seconds)
        {
            return Err(FailureKind::Stale);
        }
        if context.provider.trim_end_matches('/') != self.bitcoin.ancestry.provider
            || self.bitcoin.ancestry.provider != self.fork.ancestry.provider
            || context.generation != a.generation()
            || *self.bitcoin.ancestry.live_generation.borrow() != context.generation
            || self.bitcoin.ancestry.live_generation.has_changed().is_err()
            || *self.fork.ancestry.live_generation.borrow() != context.generation
            || self.fork.ancestry.live_generation.has_changed().is_err()
        {
            return Err(FailureKind::Cancelled);
        }
        if context.tips.bitcoin != a.bitcoin().tip
            || context.tips.fork != a.fork().tip
            || self.bitcoin.tip != a.bitcoin().tip
            || self.fork.tip != a.fork().tip
        {
            return Err(FailureKind::Changed);
        }
        Ok(())
    }

    /// Review-only binding for a paired Claim. The candidate metadata is
    /// checked against each raw funding transaction and the source Vault;
    /// neither this result nor the retained paths grants signing authority.
    #[allow(clippy::too_many_arguments)]
    pub fn validate_for_claim(
        &self,
        bitcoin_path: &RetainedPath,
        fork_path: &RetainedPath,
        bitcoin_coin: &CandidateCoin,
        fork_coin: &CandidateCoin,
        bitcoin: &AncestrySelfTransfer,
        fork: &ClaimForkSweep,
        context: FundingContext<'_>,
    ) -> Result<(), FailureKind> {
        self.validate_session(context)?;
        if self.bitcoin.chain != ChainId::Bitcoin
            || self.fork.chain != ChainId::BitcoinBlake2b
            || self.bitcoin.ancestry.pair().root_chain() != ChainId::Bitcoin
            || self.fork.ancestry.pair().root_chain() != ChainId::BitcoinBlake2b
            || bitcoin.chain() != ChainId::Bitcoin
            || fork.chain() != ChainId::BitcoinBlake2b
            || fork.descriptor() != bitcoin.descriptor()
            || bitcoin.poison_input() != self.bitcoin.selected
            || fork.fork_marker() != Some(self.fork.selected)
            || fork.bitcoin_step1() != bitcoin.psbt().unsigned_tx.compute_txid()
            || bitcoin.poison_derivation() != (bitcoin_coin.deriv_index, bitcoin_coin.is_change)
            || bitcoin.claimed_prevouts().is_empty()
            || self.bitcoin.selected == self.fork.selected
        {
            return Err(FailureKind::Changed);
        }
        let shared: BTreeSet<_> = bitcoin.claimed_prevouts().iter().copied().collect();
        let mut first: BTreeSet<_> = bitcoin
            .psbt()
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();
        let mut second: BTreeSet<_> = fork
            .psbt()
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();
        if shared.len() != bitcoin.claimed_prevouts().len()
            || first.len() != bitcoin.psbt().unsigned_tx.input.len()
            || second.len() != fork.psbt().unsigned_tx.input.len()
            || !first.remove(&self.bitcoin.selected)
            || !second.remove(&self.fork.selected)
            || first != shared
            || second != shared
        {
            return Err(FailureKind::Changed);
        }
        self.bitcoin
            .validate_claim_coin(bitcoin_path, bitcoin_coin, bitcoin.descriptor())?;
        self.fork
            .validate_claim_coin(fork_path, fork_coin, bitcoin.descriptor())?;
        Ok(())
    }

    /// Bind the two fresh funding observations to exactly the same ordinary
    /// constructions, retained paths and scan-derived wallet coins. This is a
    /// review check, not a signing capability: source ownership is established
    /// by the core builders, and reservations/fresh preflight remain required.
    #[allow(clippy::too_many_arguments)]
    pub fn validate_for_split(
        &self,
        bitcoin_path: &RetainedPath,
        fork_path: &RetainedPath,
        bitcoin_candidate: &SplitCoin,
        fork_candidate: &SplitCoin,
        step1: &AncestrySplitStep1,
        step2: &AncestrySplitStep2,
        context: FundingContext<'_>,
    ) -> Result<(), FailureKind> {
        self.validate_session(context)?;
        let a = self.bitcoin.ancestry.pair();
        if step1.chain() != ChainId::Bitcoin
            || step2.chain() != ChainId::BitcoinBlake2b
            || step1.source() != step2.source()
            || step1.funding_prevout() != self.bitcoin.selected
            || step2.funding_prevout() != self.fork.selected
            || step1.funding_derivation()
                != Some((bitcoin_candidate.branch, bitcoin_candidate.index))
            || step2.funding_derivation() != Some((fork_candidate.branch, fork_candidate.index))
            || step1.claimed_prevouts() != step2.claimed_prevouts()
            || step1.claimed_prevouts().is_empty()
            || step1.fork_height() != step2.fork_height()
            || step1.fork_height() != a.anchor().observation.fork.as_ref().map_or(0, |f| f.height)
        {
            return Err(FailureKind::Changed);
        }
        let shared: BTreeSet<_> = step1.claimed_prevouts().iter().copied().collect();
        let mut first: BTreeSet<_> = step1
            .psbt()
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();
        let mut second: BTreeSet<_> = step2
            .psbt()
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();
        if shared.len() != step1.claimed_prevouts().len()
            || first.len() != step1.psbt().unsigned_tx.input.len()
            || second.len() != step2.psbt().unsigned_tx.input.len()
            || !first.remove(&self.bitcoin.selected)
            || !second.remove(&self.fork.selected)
            || first != shared
            || second != shared
            || step1.psbt().unsigned_tx.output.len() != 1
            || step2.psbt().unsigned_tx.output.len() != 1
        {
            return Err(FailureKind::Changed);
        }
        self.bitcoin
            .validate_candidate(bitcoin_path, bitcoin_candidate)?;
        self.fork.validate_candidate(fork_path, fork_candidate)?;
        Ok(())
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

    /// Refresh both funding proofs and every shared input under the same
    /// provider generation and stable two-chain tips. For each shared coin,
    /// require a confirmed transaction in the same canonical pre-fork block
    /// on both chains and an explicit current unspent response on each.
    /// Missing, stale or spent responses never become evidence of safety.
    pub async fn collect_spend_inputs(
        &self,
        bitcoin_path: &RetainedPath,
        fork_path: &RetainedPath,
        shared: &[OutPoint],
        policy: Policy,
    ) -> Result<CollectedSpendInputs, FailureKind> {
        let unique: BTreeSet<_> = shared.iter().copied().collect();
        if unique.is_empty()
            || unique.len() != shared.len()
            || unique.contains(&bitcoin_path.selected())
            || unique.contains(&fork_path.selected())
            || unique.iter().any(OutPoint::is_null)
        {
            return Err(FailureKind::InvalidPlan);
        }
        let snapshot = self.discovery_snapshot();
        tokio::time::timeout(MAX_COLLECTION_TIME, async {
            let funding = snapshot
                .collect_funding_pair(bitcoin_path, fork_path, policy)
                .await?;
            let roots = funding.bitcoin.ancestry.pair();
            let fork_height = roots
                .anchor()
                .observation
                .fork
                .as_ref()
                .ok_or(FailureKind::InvalidPlan)?
                .height;
            let bitcoin_tip = roots.bitcoin().tip;
            let fork_tip = roots.fork().tip;
            let mut observed_at = funding.observed_at;
            let mut confirmed = Vec::with_capacity(unique.len());
            for outpoint in unique {
                let mut blocks = Vec::with_capacity(2);
                for (chain, tip) in [
                    (ChainId::Bitcoin, bitcoin_tip),
                    (ChainId::BitcoinBlake2b, fork_tip),
                ] {
                    let (status, status_stamp) = super::super::super::read(
                        snapshot.transaction(chain, outpoint.txid).await?,
                        chain,
                        &snapshot,
                        policy,
                        Stage::BitcoinInclusion,
                    )
                    .map_err(|error| error.kind)?;
                    let TransactionObservation::Confirmed { block, .. } = status else {
                        return Err(FailureKind::Changed);
                    };
                    if block.height >= fork_height || block.height > tip.height {
                        return Err(FailureKind::Changed);
                    }
                    let (canonical, canonical_stamp) = super::super::super::read(
                        snapshot.hash_at_height(chain, block.height).await?,
                        chain,
                        &snapshot,
                        policy,
                        Stage::BitcoinInclusion,
                    )
                    .map_err(|error| error.kind)?;
                    let (outspend, outspend_stamp) = super::super::super::read(
                        snapshot.outspend(chain, outpoint).await?,
                        chain,
                        &snapshot,
                        policy,
                        Stage::Preflight,
                    )
                    .map_err(|error| error.kind)?;
                    if canonical != block.hash || outspend.spent {
                        return Err(FailureKind::Changed);
                    }
                    observed_at = observed_at
                        .min(status_stamp)
                        .min(canonical_stamp)
                        .min(outspend_stamp);
                    blocks.push(block);
                }
                if blocks[0] != blocks[1] {
                    return Err(FailureKind::Changed);
                }
                confirmed.push((outpoint, blocks[0]));
            }
            let last_bitcoin = snapshot.tip(ChainId::Bitcoin).await?;
            let last_fork = snapshot.tip(ChainId::BitcoinBlake2b).await?;
            if last_bitcoin.value() != &bitcoin_tip || last_fork.value() != &fork_tip {
                return Err(FailureKind::Changed);
            }
            observed_at = observed_at
                .min(last_bitcoin.observed_at())
                .min(last_fork.observed_at());
            if !super::super::super::fresh(
                observed_at,
                snapshot.now(),
                policy.max_observation_age_seconds,
            ) {
                return Err(FailureKind::Stale);
            }
            snapshot.validate_context(&funding.bitcoin.ancestry.provider, roots.generation())?;
            Ok(CollectedSpendInputs {
                funding,
                shared: confirmed,
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
