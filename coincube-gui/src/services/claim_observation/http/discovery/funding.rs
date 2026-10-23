//! Fresh, paired funding observations for the ancestry split. These checks
//! establish provider-reported chain status, not source-wallet ownership,
//! reservation, signer admission or broadcast authority.

use super::*;
use crate::services::foreign_split_inventory::{FreshIndex, SplitInventory};
use coincube_core::{
    claim::{PreflightTips, MIN_CONFIRMATIONS},
    claim_ancestry::retained::RetainedPath,
    claim_spend::{
        create_ancestry_fork_sweep_with_marker, create_ancestry_self_transfer,
        AncestrySelfTransfer, ClaimForkSweep,
    },
    descriptors::CoincubeDescriptor,
    foreign_split::{
        create_ancestry_split_step1, create_ancestry_split_step2, AncestrySplitInputs,
        AncestrySplitStep1, AncestrySplitStep2, AncestrySplitStep2Inputs, SplitBranch, SplitCoin,
        SplitSource,
    },
    miniscript::bitcoin::{
        absolute::LockTime,
        bip32::ChildNumber,
        consensus::{deserialize, serialize},
        psbt::Psbt,
        Script, Transaction, TxOut,
    },
    spend::{CandidateCoin, TxGetter},
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
    if spent {
        return Err(FailureKind::Changed);
    }
    check_funding_position(block, root, tip, canonical)
}

fn check_funding_position(
    block: BlockRef,
    root: BlockRef,
    tip: BlockRef,
    canonical: BlockHash,
) -> Result<(), FailureKind> {
    if block.height < root.height || block.height > tip.height || canonical != block.hash {
        return Err(FailureKind::Changed);
    }
    let depth = tip.height - block.height + 1;
    let maturity = tip.height - root.height + 1;
    if depth < MIN_CONFIRMATIONS || maturity < COINBASE_MATURITY {
        return Err(FailureKind::Changed);
    }
    Ok(())
}

fn post_bitcoin_input_shape(
    bitcoin: &Transaction,
    bitcoin_marker: OutPoint,
    fork_marker: OutPoint,
    shared: &BTreeSet<OutPoint>,
) -> bool {
    let inputs: BTreeSet<_> = bitcoin
        .input
        .iter()
        .map(|input| input.previous_output)
        .collect();
    let mut expected = shared.clone();
    expected.insert(bitcoin_marker);
    !shared.is_empty()
        && !shared.contains(&bitcoin_marker)
        && !shared.contains(&fork_marker)
        && bitcoin_marker != fork_marker
        && inputs.len() == bitcoin.input.len()
        && inputs == expected
        && bitcoin.output.len() == 1
        && !bitcoin.output[0].script_pubkey.is_op_return()
}

fn confirmed_bitcoin_step(
    transaction: &Transaction,
    status: TransactionObservation,
    canonical: BlockHash,
    tip: BlockRef,
    raw: &[u8],
) -> Result<BlockRef, FailureKind> {
    let TransactionObservation::Confirmed { txid, block } = status else {
        return Err(FailureKind::Changed);
    };
    if txid != transaction.compute_txid()
        || block.hash != canonical
        || block.height > tip.height
        || tip.height - block.height + 1 < MIN_CONFIRMATIONS
        || raw != serialize(transaction)
    {
        return Err(FailureKind::Changed);
    }
    Ok(block)
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
        if coin.deriv_index.is_hardened() {
            return Err(FailureKind::Changed);
        }
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
    shared_outputs: Vec<(OutPoint, TxOut)>,
    observed_at: i64,
}

/// Stage-two observations after the exact Bitcoin transaction is confirmed.
/// Its shared and Bitcoin-only inputs are necessarily spent on Bitcoin while
/// the corresponding shared inputs and BTCB2-only coin must remain unspent on
/// BTCB2. This type is deliberately distinct from `CollectedSpendInputs` so
/// it cannot accidentally satisfy the pre-broadcast signer validator.
#[derive(Debug)]
#[allow(dead_code)] // Stage-two coordinator and signer admission are still gated.
pub struct CollectedPostBitcoinInputs {
    signed_bitcoin: Transaction,
    bitcoin: CollectedFundingCoin,
    fork: CollectedFundingCoin,
    bitcoin_confirmation: BlockRef,
    shared: Vec<(OutPoint, BlockRef)>,
    shared_outputs: Vec<(OutPoint, TxOut)>,
    observed_at: i64,
}

impl CollectedPostBitcoinInputs {
    /// Canonical pre-fork Bitcoin blocks for the shared outputs checked by
    /// this collection. The caller must first validate this observation for
    /// the exact signed pair and live session before using the block mapping.
    pub(crate) fn shared_blocks(&self) -> &[(OutPoint, BlockRef)] {
        &self.shared
    }

    /// Recover the spent Bitcoin-only input from the retained path after the
    /// canonical Bitcoin spend has been observed. The caller must first bind
    /// this collection to the exact signed pair and current session. This
    /// returns structural input data, never current UTXO or signing authority.
    pub(crate) fn bitcoin_funding_coin(
        &self,
        path: &RetainedPath,
        source: &SplitSource,
        index: u32,
    ) -> Result<SplitCoin, FailureKind> {
        let raw = path.links().first().ok_or(FailureKind::Malformed)?;
        let previous: Transaction =
            deserialize(&raw.transaction).map_err(|_| FailureKind::Malformed)?;
        let coin = SplitCoin {
            outpoint: path.selected(),
            branch: SplitBranch::External,
            index,
            previous,
            bitcoin_block: Some(self.bitcoin.block()),
            btcb2_block: None,
        };
        self.bitcoin.validate_candidate(path, &coin)?;
        if !source_owns_coin(source, &coin) {
            return Err(FailureKind::Changed);
        }
        Ok(coin)
    }

    pub fn observed_at(&self) -> i64 {
        self.observed_at
    }

    pub fn bitcoin_confirmation(&self) -> BlockRef {
        self.bitcoin_confirmation
    }

    /// Bind the post-confirmation observations to the same exact signed
    /// Bitcoin witness and intended fork input set. Ownership, reservations,
    /// finalized fork signatures and fresh node policy are separate checks.
    #[allow(dead_code)] // Consumed by the future stage-two coordinator.
    pub fn validate_for_transactions(
        &self,
        signed_bitcoin: &Transaction,
        fork: &Transaction,
        claimed: &[OutPoint],
        context: FundingContext<'_>,
    ) -> Result<(), FailureKind> {
        let roots = self.bitcoin.ancestry.pair();
        if context.max_age_seconds <= 0
            || !super::super::super::fresh(self.observed_at, context.now, context.max_age_seconds)
        {
            return Err(FailureKind::Stale);
        }
        if context.provider.trim_end_matches('/') != self.bitcoin.ancestry.provider
            || self.bitcoin.ancestry.provider != self.fork.ancestry.provider
            || context.generation != roots.generation()
            || !session_current(&self.bitcoin.ancestry.live_generation, context.generation)
            || !session_current(&self.fork.ancestry.live_generation, context.generation)
        {
            return Err(FailureKind::Cancelled);
        }
        if context.tips.bitcoin != roots.bitcoin().tip
            || context.tips.fork != roots.fork().tip
            || self.bitcoin.tip != context.tips.bitcoin
            || self.fork.tip != context.tips.fork
            || self.bitcoin_confirmation.height > context.tips.bitcoin.height
            || context.tips.bitcoin.height - self.bitcoin_confirmation.height + 1
                < MIN_CONFIRMATIONS
            || &self.signed_bitcoin != signed_bitcoin
            || self.bitcoin.chain != ChainId::Bitcoin
            || self.fork.chain != ChainId::BitcoinBlake2b
            || roots.root_chain() != ChainId::Bitcoin
            || self.fork.ancestry.pair().root_chain() != ChainId::BitcoinBlake2b
        {
            return Err(FailureKind::Changed);
        }
        let shared: BTreeSet<_> = self.shared.iter().map(|(outpoint, _)| *outpoint).collect();
        let claimed: BTreeSet<_> = claimed.iter().copied().collect();
        let fork_inputs: BTreeSet<_> = fork
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();
        let mut expected_fork = shared.clone();
        expected_fork.insert(self.fork.selected);
        if shared.is_empty()
            || shared.len() != self.shared.len()
            || claimed.len() != self.shared.len()
            || claimed != shared
            || self.shared_outputs.len() != shared.len()
            || self
                .shared_outputs
                .iter()
                .map(|(outpoint, _)| *outpoint)
                .collect::<BTreeSet<_>>()
                != shared
            || !post_bitcoin_input_shape(
                signed_bitcoin,
                self.bitcoin.selected,
                self.fork.selected,
                &shared,
            )
            || fork_inputs.len() != fork.input.len()
            || fork_inputs != expected_fork
            || fork.output.len() != 1
            || fork.output[0].script_pubkey.is_op_return()
        {
            return Err(FailureKind::Changed);
        }
        Ok(())
    }

    /// At the fork-send boundary, authenticate the saved fork construction's
    /// shared and chain-specific prevout metadata against the freshly read
    /// transactions. A transaction-only comparison would allow a caller to
    /// attach unrelated PSBT amounts or scripts after the Bitcoin spend.
    /// Wallet ownership and its durable address reservation remain separate.
    #[allow(dead_code)] // Consumed by the future stage-two coordinator.
    pub fn validate_for_fork_psbt(
        &self,
        signed_bitcoin: &Transaction,
        fork: &Psbt,
        claimed: &[OutPoint],
        context: FundingContext<'_>,
    ) -> Result<(), FailureKind> {
        self.validate_for_transactions(signed_bitcoin, &fork.unsigned_tx, claimed, context)?;
        validate_shared_psbt(fork, &self.shared_outputs)?;
        let path = self
            .fork
            .ancestry
            .retained_path()
            .map_err(|_| FailureKind::Malformed)?;
        let raw = path.links().first().ok_or(FailureKind::Malformed)?;
        let previous: Transaction =
            deserialize(&raw.transaction).map_err(|_| FailureKind::Malformed)?;
        if previous.compute_txid() != self.fork.selected.txid {
            return Err(FailureKind::Changed);
        }
        let output = previous
            .output
            .get(self.fork.selected.vout as usize)
            .ok_or(FailureKind::Changed)?;
        validate_shared_psbt(fork, &[(self.fork.selected, output.clone())])
    }
}

/// Read-only foreign Split discovery, joined to the exact scan-derived coins
/// and source descriptor. Neither this object nor its paths reserve an input
/// or grant signer access; the two ordinary constructions and journal must
/// still be checked against fresh observations at admission.
#[derive(Debug)]
pub struct OwnedSplitFunding {
    pub bitcoin: SplitCoin,
    pub fork: SplitCoin,
    pub bitcoin_path: RetainedPath,
    pub fork_path: RetainedPath,
    pub inputs: CollectedSpendInputs,
}

/// Read-only Claim discovery with both funding inputs and every shared coin
/// bound to the same Vault descriptor. It is not a coin reservation or signer
/// capability; the daemon and paired BTCB2 inventory must be refreshed at
/// admission and after restart.
#[derive(Debug)]
pub struct OwnedClaimFunding {
    pub bitcoin: CandidateCoin,
    pub fork: CandidateCoin,
    pub bitcoin_path: RetainedPath,
    pub fork_path: RetainedPath,
    pub inputs: CollectedSpendInputs,
}

#[derive(Debug)]
pub enum ClaimPairError {
    Changed,
    InvalidConstruction(coincube_core::claim_spend::Error),
    Observation(FailureKind),
}

fn claim_owns_output(
    descriptor: &CoincubeDescriptor,
    coin: &CandidateCoin,
    output: &TxOut,
) -> bool {
    if coin.deriv_index.is_hardened() {
        return false;
    }
    let derived = if coin.is_change {
        descriptor.change_descriptor()
    } else {
        descriptor.receive_descriptor()
    }
    .derive(
        coin.deriv_index,
        &coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only(),
    );
    coin.amount == output.value && derived.script_pubkey() == output.script_pubkey
}

impl HttpObservationSource {
    /// Search both supplied wallet-owned candidate inventories as one bounded
    /// collection. No partial result is returned if either chain lacks a
    /// qualifying funding coin. The caller must obtain these inventories from
    /// current Bitcoin and BTCB2 wallet reads; this method authenticates the
    /// selected previous transactions and all shared output bytes to the
    /// descriptor before returning a review-only pair.
    pub async fn discover_owned_claim_funding(
        &self,
        descriptor: &CoincubeDescriptor,
        bitcoin_candidates: &[CandidateCoin],
        fork_candidates: &[CandidateCoin],
        shared: &[CandidateCoin],
        policy: Policy,
    ) -> Result<Option<OwnedClaimFunding>, DiscoveryError> {
        let invalid = || DiscoveryError::Observation(FailureKind::InvalidPlan);
        let outpoints = |coins: &[CandidateCoin]| coins.iter().map(|coin| coin.outpoint).collect();
        let bitcoin_outpoints: Vec<_> = outpoints(bitcoin_candidates);
        let fork_outpoints: Vec<_> = outpoints(fork_candidates);
        let shared_outpoints: Vec<_> = outpoints(shared);
        let Some(inputs) = self
            .discover_spend_inputs(
                &bitcoin_outpoints,
                &fork_outpoints,
                &shared_outpoints,
                policy,
            )
            .await?
        else {
            return Ok(None);
        };
        let bitcoin_selected = inputs.funding().bitcoin().selected();
        let fork_selected = inputs.funding().fork().selected();
        let bitcoin = bitcoin_candidates
            .iter()
            .find(|coin| coin.outpoint == bitcoin_selected)
            .copied()
            .ok_or_else(invalid)?;
        let fork = fork_candidates
            .iter()
            .find(|coin| coin.outpoint == fork_selected)
            .copied()
            .ok_or_else(invalid)?;
        let bitcoin_path = inputs
            .funding()
            .bitcoin()
            .ancestry()
            .retained_path()
            .map_err(DiscoveryError::Structural)?;
        let fork_path = inputs
            .funding()
            .fork()
            .ancestry()
            .retained_path()
            .map_err(DiscoveryError::Structural)?;
        inputs
            .funding()
            .bitcoin()
            .validate_claim_coin(&bitcoin_path, &bitcoin, descriptor)
            .inspect_err(|error| {
                self.report_funding(
                    ChainId::Bitcoin,
                    FundingProofStatus::Failed(DiscoveryError::Observation(*error)),
                )
            })
            .map_err(DiscoveryError::Observation)?;
        inputs
            .funding()
            .fork()
            .validate_claim_coin(&fork_path, &fork, descriptor)
            .inspect_err(|error| {
                self.report_funding(
                    ChainId::BitcoinBlake2b,
                    FundingProofStatus::Failed(DiscoveryError::Observation(*error)),
                )
            })
            .map_err(DiscoveryError::Observation)?;
        if inputs.shared_outputs.len() != shared.len()
            || inputs.shared_outputs.iter().any(|(outpoint, output)| {
                !shared.iter().any(|coin| {
                    coin.outpoint == *outpoint && claim_owns_output(descriptor, coin, output)
                })
            })
        {
            return Err(invalid());
        }
        Ok(Some(OwnedClaimFunding {
            bitcoin,
            fork,
            bitcoin_path,
            fork_path,
            inputs,
        }))
    }
}

impl OwnedClaimFunding {
    /// Build both ordinary, wallet-owned Claim transactions for review from
    /// this paired observation. The caller supplies current shared wallet
    /// coins and previous transactions plus two independently reserved change
    /// indices. A signer must recheck all observations and reservations at
    /// admission; these PSBTs alone confer no authorization.
    #[allow(clippy::too_many_arguments)]
    pub fn construct_claim_pair(
        &self,
        descriptor: &CoincubeDescriptor,
        tx_getter: &mut impl TxGetter,
        shared: &[CandidateCoin],
        bitcoin_change: ChildNumber,
        fork_change: ChildNumber,
        bitcoin_feerate_vb: u64,
        fork_feerate_vb: u64,
        context: FundingContext<'_>,
    ) -> Result<(AncestrySelfTransfer, ClaimForkSweep), ClaimPairError> {
        self.inputs
            .funding()
            .validate_session(context)
            .map_err(ClaimPairError::Observation)?;
        let expected: BTreeSet<_> = self
            .inputs
            .shared()
            .iter()
            .map(|(outpoint, _)| *outpoint)
            .collect();
        let supplied: BTreeSet<_> = shared.iter().map(|coin| coin.outpoint).collect();
        if expected.is_empty()
            || bitcoin_change.is_hardened()
            || fork_change.is_hardened()
            || expected.len() != shared.len()
            || expected != supplied
            || self.inputs.shared_outputs.iter().any(|(outpoint, output)| {
                !shared.iter().any(|coin| {
                    coin.outpoint == *outpoint && claim_owns_output(descriptor, coin, output)
                })
            })
        {
            return Err(ClaimPairError::Changed);
        }
        let secp = coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only();
        let bitcoin_dependency = self
            .bitcoin_path
            .reverify()
            .map_err(|_| ClaimPairError::Changed)?;
        let fork_dependency = self
            .fork_path
            .reverify()
            .map_err(|_| ClaimPairError::Changed)?;
        let bitcoin_locktime = LockTime::from_height(
            u32::try_from(context.tips.bitcoin.height).map_err(|_| ClaimPairError::Changed)?,
        )
        .map_err(|_| ClaimPairError::Changed)?;
        let fork_locktime = LockTime::from_height(
            u32::try_from(context.tips.fork.height).map_err(|_| ClaimPairError::Changed)?,
        )
        .map_err(|_| ClaimPairError::Changed)?;
        let mut bitcoin_coins = shared.to_vec();
        bitcoin_coins.push(self.bitcoin);
        let bitcoin = create_ancestry_self_transfer(
            ChainId::Bitcoin,
            descriptor,
            &secp,
            tx_getter,
            &bitcoin_coins,
            bitcoin_change,
            bitcoin_feerate_vb,
            bitcoin_locktime,
            &bitcoin_dependency,
        )
        .map_err(ClaimPairError::InvalidConstruction)?;
        let mut fork_coins = shared.to_vec();
        fork_coins.push(self.fork);
        let fork = create_ancestry_fork_sweep_with_marker(
            &bitcoin,
            ChainId::BitcoinBlake2b,
            &secp,
            tx_getter,
            &fork_coins,
            fork_change,
            fork_feerate_vb,
            fork_locktime,
            &fork_dependency,
        )
        .map_err(ClaimPairError::InvalidConstruction)?;
        self.inputs
            .validate_for_claim(
                &self.bitcoin_path,
                &self.fork_path,
                &self.bitcoin,
                &self.fork,
                &bitcoin,
                &fork,
                context,
            )
            .map_err(ClaimPairError::Observation)?;
        Ok((bitcoin, fork))
    }
}

/// Construction from live paired proof is still a review artifact. An
/// independent signer admission must recheck the same proof against current
/// reservations, source coins and a fresh preflight before exposing either
/// PSBT to a hardware or software signer.
#[derive(Debug)]
pub enum SplitPairError {
    Changed,
    InvalidConstruction(coincube_core::foreign_split::Error),
    Observation(FailureKind),
}

impl OwnedSplitFunding {
    /// Build both ordinary transactions from one qualified pair. The source
    /// wallet's fresh receive index and the target Cube's reserved script are
    /// supplied by their respective owners; neither can be inferred from a
    /// funding observation. The same shared inputs appear in both PSBTs.
    #[allow(clippy::too_many_arguments)]
    pub fn construct_split_pair(
        &self,
        inventory: &SplitInventory,
        source: &SplitSource,
        bitcoin_return_index: u32,
        target_script: &Script,
        paired_target_index: Option<u32>,
        bitcoin_feerate_vb: u64,
        fork_feerate_vb: u64,
        context: FundingContext<'_>,
    ) -> Result<(AncestrySplitStep1, AncestrySplitStep2), SplitPairError> {
        // The return index belongs to the durable funding plan. After the
        // deposits appear in a new scan, the first unused index advances; it
        // must be exactly the return address the user already reviewed.
        if inventory.fresh_receive() != FreshIndex::Proven(bitcoin_return_index) {
            return Err(SplitPairError::Changed);
        }
        let shared = inventory.splittable_coins();
        let expected: BTreeSet<_> = shared.iter().map(|coin| coin.outpoint).collect();
        let observed: BTreeSet<_> = self
            .inputs
            .shared()
            .iter()
            .map(|(outpoint, _)| *outpoint)
            .collect();
        if shared.is_empty()
            || expected.len() != shared.len()
            || expected != observed
            || inventory.fork_height()
                != self
                    .inputs
                    .funding()
                    .bitcoin()
                    .ancestry()
                    .pair()
                    .anchor()
                    .observation
                    .fork
                    .as_ref()
                    .map_or(0, |fork| fork.height)
            || inventory.bitcoin_tip() != context.tips.bitcoin.hash
            || inventory.btcb2_tip() != context.tips.fork.hash
            || inventory.bitcoin_tip_height() as u64 != context.tips.bitcoin.height
            || inventory.btcb2_tip_height() as u64 != context.tips.fork.height
            || !source_owns_coin(source, &self.bitcoin)
            || !source_owns_coin(source, &self.fork)
            || shared.iter().any(|coin| !source_owns_coin(source, coin))
        {
            return Err(SplitPairError::Changed);
        }
        let bitcoin_dependency = self
            .bitcoin_path
            .reverify()
            .map_err(|_| SplitPairError::Changed)?;
        let fork_dependency = self
            .fork_path
            .reverify()
            .map_err(|_| SplitPairError::Changed)?;
        let bitcoin_tip = inventory.bitcoin_tip_height();
        let fork_tip = inventory.btcb2_tip_height();
        let bitcoin_locktime =
            LockTime::from_height(bitcoin_tip).map_err(|_| SplitPairError::Changed)?;
        let fork_locktime = LockTime::from_height(fork_tip).map_err(|_| SplitPairError::Changed)?;
        let bitcoin = create_ancestry_split_step1(
            &AncestrySplitInputs {
                chain: ChainId::Bitcoin,
                source,
                shared: &shared,
                funding: &self.bitcoin,
                dependency: &bitcoin_dependency,
                fork_height: inventory.fork_height(),
                destination: bitcoin_return_index,
            },
            bitcoin_feerate_vb,
            bitcoin_locktime,
            bitcoin_tip,
        )
        .map_err(SplitPairError::InvalidConstruction)?;
        let fork = create_ancestry_split_step2(
            &AncestrySplitStep2Inputs {
                chain: ChainId::BitcoinBlake2b,
                source,
                shared: &shared,
                funding: &self.fork,
                dependency: &fork_dependency,
                fork_height: inventory.fork_height(),
                claimed: bitcoin.claimed_prevouts(),
                target: target_script,
                paired_target_index,
            },
            fork_feerate_vb,
            fork_locktime,
            fork_tip,
        )
        .map_err(SplitPairError::InvalidConstruction)?;
        self.inputs
            .validate_for_split(
                &self.bitcoin_path,
                &self.fork_path,
                &self.bitcoin,
                &self.fork,
                &bitcoin,
                &fork,
                context,
            )
            .map_err(SplitPairError::Observation)?;
        Ok((bitcoin, fork))
    }
}

fn source_owns_coin(source: &SplitSource, coin: &SplitCoin) -> bool {
    let Some(output) = coin.previous.output.get(coin.outpoint.vout as usize) else {
        return false;
    };
    let descriptor = match coin.branch {
        SplitBranch::External => Some(source.external()),
        SplitBranch::Internal => source.internal(),
    };
    coin.previous.compute_txid() == coin.outpoint.txid
        && coin.index < (1 << 31)
        && descriptor
            .and_then(|descriptor| descriptor.at_derivation_index(coin.index).ok())
            .is_some_and(|descriptor| descriptor.script_pubkey() == output.script_pubkey)
}

impl HttpObservationSource {
    /// Search both candidate inventories in one bounded collection, then bind
    /// the selected raw transactions and derivations to the foreign source.
    /// A missing path on either chain returns no pair, never a one-sided plan.
    pub async fn discover_owned_split_funding(
        &self,
        inventory: &SplitInventory,
        source: &SplitSource,
        policy: Policy,
    ) -> Result<Option<OwnedSplitFunding>, DiscoveryError> {
        let invalid = || DiscoveryError::Observation(FailureKind::InvalidPlan);
        let bitcoin = inventory.funding_candidates(ChainId::Bitcoin);
        let fork = inventory.funding_candidates(ChainId::BitcoinBlake2b);
        let shared = inventory.splittable_coins();
        if bitcoin.len() > MAX_ANCESTRY_CANDIDATES || fork.len() > MAX_ANCESTRY_CANDIDATES {
            return Err(DiscoveryError::Observation(FailureKind::CollectionLimit));
        }
        if shared.is_empty()
            || bitcoin
                .iter()
                .chain(&fork)
                .chain(&shared)
                .any(|coin| !source_owns_coin(source, coin))
        {
            return Err(invalid());
        }
        let bitcoin_outpoints: Vec<_> = bitcoin.iter().map(|coin| coin.outpoint).collect();
        let fork_outpoints: Vec<_> = fork.iter().map(|coin| coin.outpoint).collect();
        let shared_outpoints: Vec<_> = shared.iter().map(|coin| coin.outpoint).collect();
        let Some(inputs) = self
            .discover_spend_inputs(
                &bitcoin_outpoints,
                &fork_outpoints,
                &shared_outpoints,
                policy,
            )
            .await?
        else {
            return Ok(None);
        };
        let bitcoin_selected = inputs.funding().bitcoin().selected();
        let fork_selected = inputs.funding().fork().selected();
        let bitcoin_coin = bitcoin
            .into_iter()
            .find(|coin| coin.outpoint == bitcoin_selected)
            .ok_or_else(invalid)?;
        let fork_coin = fork
            .into_iter()
            .find(|coin| coin.outpoint == fork_selected)
            .ok_or_else(invalid)?;
        let bitcoin_path = inputs
            .funding()
            .bitcoin()
            .ancestry()
            .retained_path()
            .map_err(DiscoveryError::Structural)?;
        let fork_path = inputs
            .funding()
            .fork()
            .ancestry()
            .retained_path()
            .map_err(DiscoveryError::Structural)?;
        inputs
            .funding()
            .bitcoin()
            .validate_candidate(&bitcoin_path, &bitcoin_coin)
            .and_then(|_| {
                inputs
                    .funding()
                    .fork()
                    .validate_candidate(&fork_path, &fork_coin)
            })
            .map_err(DiscoveryError::Observation)?;
        Ok(Some(OwnedSplitFunding {
            bitcoin: bitcoin_coin,
            fork: fork_coin,
            bitcoin_path,
            fork_path,
            inputs,
        }))
    }
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
        )?;
        validate_shared_psbt(bitcoin.psbt(), &self.shared_outputs)?;
        validate_shared_psbt(fork.psbt(), &self.shared_outputs)
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
        )?;
        validate_shared_psbt(bitcoin.psbt(), &self.shared_outputs)?;
        validate_shared_psbt(fork.psbt(), &self.shared_outputs)
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

fn validate_shared_psbt(
    psbt: &Psbt,
    shared_outputs: &[(OutPoint, TxOut)],
) -> Result<(), FailureKind> {
    if psbt.unsigned_tx.input.len() != psbt.inputs.len() {
        return Err(FailureKind::Changed);
    }
    for (outpoint, output) in shared_outputs {
        let Some((_, metadata)) = psbt
            .unsigned_tx
            .input
            .iter()
            .zip(&psbt.inputs)
            .find(|(input, _)| input.previous_output == *outpoint)
        else {
            return Err(FailureKind::Changed);
        };
        let non_witness = metadata.non_witness_utxo.as_ref().and_then(|tx| {
            (tx.compute_txid() == outpoint.txid)
                .then(|| tx.output.get(outpoint.vout as usize))
                .flatten()
        });
        if metadata
            .witness_utxo
            .as_ref()
            .is_some_and(|utxo| utxo != output)
            || (metadata.non_witness_utxo.is_some() && non_witness.is_none())
            || non_witness.is_some_and(|utxo| utxo != output)
            || (metadata.witness_utxo.is_none() && non_witness.is_none())
        {
            return Err(FailureKind::Changed);
        }
    }
    Ok(())
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
            || !session_current(&self.bitcoin.ancestry.live_generation, context.generation)
            || !session_current(&self.fork.ancestry.live_generation, context.generation)
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
                .await
                .inspect_err(|error| {
                    snapshot.report_funding(
                        ChainId::Bitcoin,
                        FundingProofStatus::Failed(DiscoveryError::Observation(*error)),
                    )
                })?;
            let fork = snapshot
                .collect_funding_coin(fork_path, ChainId::BitcoinBlake2b, policy)
                .await
                .inspect_err(|error| {
                    snapshot.report_funding(
                        ChainId::BitcoinBlake2b,
                        FundingProofStatus::Failed(DiscoveryError::Observation(*error)),
                    )
                })?;
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
            let mut shared_outputs = Vec::with_capacity(unique.len());
            for outpoint in unique {
                let raw = snapshot
                    .ancestry_transaction(ChainId::Bitcoin, outpoint.txid)
                    .await?;
                let transaction: Transaction =
                    deserialize(&raw).map_err(|_| FailureKind::Malformed)?;
                let output = transaction
                    .output
                    .get(outpoint.vout as usize)
                    .ok_or(FailureKind::Changed)?
                    .clone();
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
                shared_outputs.push((outpoint, output));
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
                shared_outputs,
                observed_at,
            })
        })
        .await
        .map_err(|_| {
            if let Some(trace) = &snapshot.ancestry_trace {
                trace.emit(
                    ChainId::Bitcoin,
                    diagnostics::Outcome::CollectionDeadline30s,
                );
                trace.emit(
                    ChainId::BitcoinBlake2b,
                    diagnostics::Outcome::CollectionDeadline30s,
                );
            }
            FailureKind::Deadline
        })?
    }
}

impl HttpObservationSource {
    /// Recollect chain-specific ancestry after the exact Bitcoin half has
    /// confirmed. A canonical Bitcoin transaction spending `S+B` explains why
    /// those outputs are spent on Bitcoin; `S` and `F` must still be unspent on
    /// BTCB2. The result is read-only and cannot enter the initial signer path.
    #[allow(dead_code)] // Paired stage-two coordinator is not enabled yet.
    pub async fn collect_post_bitcoin_inputs(
        &self,
        bitcoin_path: &RetainedPath,
        fork_path: &RetainedPath,
        shared: &[OutPoint],
        signed_bitcoin: &Transaction,
        policy: Policy,
    ) -> Result<CollectedPostBitcoinInputs, FailureKind> {
        let unique: BTreeSet<_> = shared.iter().copied().collect();
        if unique.len() != shared.len()
            || unique.iter().any(OutPoint::is_null)
            || !post_bitcoin_input_shape(
                signed_bitcoin,
                bitcoin_path.selected(),
                fork_path.selected(),
                &unique,
            )
            || policy.max_observation_age_seconds <= 0
        {
            return Err(FailureKind::InvalidPlan);
        }
        let snapshot = self.discovery_snapshot();
        tokio::time::timeout(MAX_COLLECTION_TIME, async {
            let bitcoin_ancestry = snapshot
                .requalify_ancestry(bitcoin_path, policy)
                .await
                .map_err(|error| match error {
                    DiscoveryError::Structural(_) => FailureKind::Malformed,
                    DiscoveryError::Observation(kind) => kind,
                })?;
            let fork = snapshot
                .collect_funding_coin(fork_path, ChainId::BitcoinBlake2b, policy)
                .await?;
            let roots = bitcoin_ancestry.pair();
            let fork_roots = fork.ancestry.pair();
            let anchor_a = roots.anchor();
            let anchor_b = fork_roots.anchor();
            if roots.root_chain() != ChainId::Bitcoin
                || fork_roots.root_chain() != ChainId::BitcoinBlake2b
                || bitcoin_ancestry.provider != fork.ancestry.provider
                || roots.generation() != fork_roots.generation()
                || roots.bitcoin().tip != fork_roots.bitcoin().tip
                || roots.fork().tip != fork_roots.fork().tip
                || anchor_a.tip_hash != anchor_b.tip_hash
                || anchor_a.tip_height != anchor_b.tip_height
                || anchor_a.tip_median_time_past != anchor_b.tip_median_time_past
                || anchor_a.observation != anchor_b.observation
            {
                return Err(FailureKind::Changed);
            }
            let bitcoin_tip = roots.bitcoin().tip;
            let fork_tip = roots.fork().tip;
            let (marker_status, marker_stamp) = super::super::super::read(
                snapshot
                    .transaction(ChainId::Bitcoin, bitcoin_path.selected().txid)
                    .await?,
                ChainId::Bitcoin,
                &snapshot,
                policy,
                Stage::BitcoinInclusion,
            )
            .map_err(|error| error.kind)?;
            let TransactionObservation::Confirmed {
                block: marker_block,
                ..
            } = marker_status
            else {
                return Err(FailureKind::Changed);
            };
            let (marker_canonical, marker_canonical_stamp) = super::super::super::read(
                snapshot
                    .hash_at_height(ChainId::Bitcoin, marker_block.height)
                    .await?,
                ChainId::Bitcoin,
                &snapshot,
                policy,
                Stage::BitcoinInclusion,
            )
            .map_err(|error| error.kind)?;
            let (marker_spend, marker_spend_stamp) = super::super::super::read(
                snapshot
                    .outspend(ChainId::Bitcoin, bitcoin_path.selected())
                    .await?,
                ChainId::Bitcoin,
                &snapshot,
                policy,
                Stage::Preflight,
            )
            .map_err(|error| error.kind)?;
            if !marker_spend.spent {
                return Err(FailureKind::Changed);
            }
            check_funding_position(
                marker_block,
                roots.bitcoin().block,
                bitcoin_tip,
                marker_canonical,
            )?;
            let marker_observed_at = marker_stamp
                .min(marker_canonical_stamp)
                .min(marker_spend_stamp);

            let txid = signed_bitcoin.compute_txid();
            let (status, status_stamp) = super::super::super::read(
                snapshot.transaction(ChainId::Bitcoin, txid).await?,
                ChainId::Bitcoin,
                &snapshot,
                policy,
                Stage::BitcoinInclusion,
            )
            .map_err(|error| error.kind)?;
            let TransactionObservation::Confirmed { block, .. } = status else {
                return Err(FailureKind::Changed);
            };
            let (canonical, canonical_stamp) = super::super::super::read(
                snapshot
                    .hash_at_height(ChainId::Bitcoin, block.height)
                    .await?,
                ChainId::Bitcoin,
                &snapshot,
                policy,
                Stage::BitcoinInclusion,
            )
            .map_err(|error| error.kind)?;
            let raw = snapshot
                .ancestry_transaction(ChainId::Bitcoin, txid)
                .await?;
            let confirmation =
                confirmed_bitcoin_step(signed_bitcoin, status, canonical, bitcoin_tip, &raw)?;

            let fork_height = roots
                .anchor()
                .observation
                .fork
                .as_ref()
                .ok_or(FailureKind::InvalidPlan)?
                .height;
            let mut observed_at = bitcoin_ancestry
                .observed_at()
                .min(marker_observed_at)
                .min(fork.observed_at)
                .min(status_stamp)
                .min(canonical_stamp);
            let mut confirmed_shared = Vec::with_capacity(unique.len());
            let mut shared_outputs = Vec::with_capacity(unique.len());
            for outpoint in unique {
                let raw = snapshot
                    .ancestry_transaction(ChainId::Bitcoin, outpoint.txid)
                    .await?;
                let previous: Transaction =
                    deserialize(&raw).map_err(|_| FailureKind::Malformed)?;
                if previous.compute_txid() != outpoint.txid {
                    return Err(FailureKind::Changed);
                }
                let output = previous
                    .output
                    .get(outpoint.vout as usize)
                    .ok_or(FailureKind::Changed)?
                    .clone();
                let mut blocks = Vec::with_capacity(2);
                for (chain, tip, expected_spent) in [
                    (ChainId::Bitcoin, bitcoin_tip, true),
                    (ChainId::BitcoinBlake2b, fork_tip, false),
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
                    if canonical != block.hash || outspend.spent != expected_spent {
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
                confirmed_shared.push((outpoint, blocks[0]));
                shared_outputs.push((outpoint, output));
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
            snapshot.validate_context(&bitcoin_ancestry.provider, roots.generation())?;
            let bitcoin = CollectedFundingCoin {
                ancestry: bitcoin_ancestry,
                chain: ChainId::Bitcoin,
                selected: bitcoin_path.selected(),
                block: marker_block,
                tip: bitcoin_tip,
                observed_at: marker_observed_at,
            };
            Ok(CollectedPostBitcoinInputs {
                signed_bitcoin: signed_bitcoin.clone(),
                bitcoin,
                fork,
                bitcoin_confirmation: confirmation,
                shared: confirmed_shared,
                shared_outputs,
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

    #[test]
    fn confirmed_bitcoin_stage_requires_exact_shared_and_bitcoin_only_inputs() {
        use coincube_core::miniscript::bitcoin::{
            absolute::LockTime, transaction::Version, Amount, ScriptBuf, TxIn,
        };
        let shared = OutPoint::new(Txid::from_byte_array([1; 32]), 0);
        let bitcoin_marker = OutPoint::new(Txid::from_byte_array([2; 32]), 0);
        let fork_marker = OutPoint::new(Txid::from_byte_array([3; 32]), 0);
        let mut bitcoin = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: [shared, bitcoin_marker]
                .iter()
                .copied()
                .map(|previous_output| TxIn {
                    previous_output,
                    ..Default::default()
                })
                .collect(),
            output: vec![TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey: ScriptBuf::from_bytes(vec![0, 20, 1]),
            }],
        };
        let claimed = BTreeSet::from([shared]);
        assert!(post_bitcoin_input_shape(
            &bitcoin,
            bitcoin_marker,
            fork_marker,
            &claimed
        ));
        bitcoin.input[1].previous_output = fork_marker;
        assert!(!post_bitcoin_input_shape(
            &bitcoin,
            bitcoin_marker,
            fork_marker,
            &claimed
        ));
        bitcoin.input[1].previous_output = shared;
        assert!(!post_bitcoin_input_shape(
            &bitcoin,
            bitcoin_marker,
            fork_marker,
            &claimed
        ));
        bitcoin.input[1].previous_output = bitcoin_marker;
        bitcoin.output[0].script_pubkey = ScriptBuf::from_bytes(vec![0x6a]);
        assert!(!post_bitcoin_input_shape(
            &bitcoin,
            bitcoin_marker,
            fork_marker,
            &claimed
        ));
    }

    #[test]
    fn post_bitcoin_confirmation_rejects_reorg_shallow_depth_and_changed_witness() {
        use coincube_core::miniscript::bitcoin::{
            absolute::LockTime, transaction::Version, Amount, ScriptBuf, TxIn, Witness,
        };
        let transaction = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                witness: Witness::from_slice(&[vec![1]]),
                ..Default::default()
            }],
            output: vec![TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey: ScriptBuf::from_bytes(vec![0, 20, 1]),
            }],
        };
        let block = BlockRef {
            height: 100,
            hash: BlockHash::from_byte_array([4; 32]),
        };
        let tip = BlockRef {
            height: 105,
            hash: BlockHash::from_byte_array([5; 32]),
        };
        let status = TransactionObservation::Confirmed {
            txid: transaction.compute_txid(),
            block,
        };
        let raw = serialize(&transaction);
        assert_eq!(
            confirmed_bitcoin_step(&transaction, status, block.hash, tip, &raw),
            Ok(block)
        );
        assert_eq!(
            confirmed_bitcoin_step(
                &transaction,
                status,
                block.hash,
                BlockRef { height: 104, ..tip },
                &raw,
            ),
            Err(FailureKind::Changed)
        );
        assert_eq!(
            confirmed_bitcoin_step(&transaction, status, tip.hash, tip, &raw),
            Err(FailureKind::Changed)
        );
        let mut changed_witness = transaction.clone();
        changed_witness.input[0].witness = Witness::from_slice(&[vec![2]]);
        assert_eq!(changed_witness.compute_txid(), transaction.compute_txid());
        assert_eq!(
            confirmed_bitcoin_step(&changed_witness, status, block.hash, tip, &raw),
            Err(FailureKind::Changed)
        );
        assert_eq!(
            confirmed_bitcoin_step(
                &transaction,
                TransactionObservation::Unconfirmed {
                    txid: transaction.compute_txid(),
                },
                block.hash,
                tip,
                &raw,
            ),
            Err(FailureKind::Changed)
        );
    }

    #[test]
    fn claim_funding_output_must_match_owned_branch_index_and_amount() {
        use coincube_core::{
            bip39::Mnemonic,
            descriptors::{CoincubePolicy, PathInfo},
            miniscript::{
                bitcoin::{bip32::DerivationPath, secp256k1, Amount, Network},
                DescriptorPublicKey,
            },
            signer::MasterSigner,
        };
        use std::str::FromStr;

        let secp = secp256k1::Secp256k1::new();
        let keys: Vec<_> = [41_u8, 42]
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
        let script = descriptor
            .receive_descriptor()
            .derive(0.into(), &secp256k1::Secp256k1::verification_only())
            .script_pubkey();
        let mut output = TxOut {
            value: Amount::from_sat(50_000),
            script_pubkey: script,
        };
        let mut coin = CandidateCoin {
            outpoint: OutPoint::new(
                coincube_core::miniscript::bitcoin::Txid::from_byte_array([7; 32]),
                0,
            ),
            amount: output.value,
            deriv_index: 0.into(),
            is_change: false,
            must_select: true,
            sequence: None,
            ancestor_info: None,
        };
        assert!(claim_owns_output(&descriptor, &coin, &output));
        coin.amount = Amount::from_sat(49_999);
        assert!(!claim_owns_output(&descriptor, &coin, &output));
        coin.amount = output.value;
        coin.deriv_index = 1.into();
        assert!(!claim_owns_output(&descriptor, &coin, &output));
        coin.deriv_index = 0.into();
        coin.is_change = true;
        assert!(!claim_owns_output(&descriptor, &coin, &output));
        coin.is_change = false;
        coin.deriv_index =
            coincube_core::miniscript::bitcoin::bip32::ChildNumber::Hardened { index: 0 };
        assert!(!claim_owns_output(&descriptor, &coin, &output));
        coin.deriv_index = 0.into();
        output.script_pubkey = coincube_core::miniscript::bitcoin::ScriptBuf::new();
        assert!(!claim_owns_output(&descriptor, &coin, &output));
    }

    #[test]
    fn split_funding_must_match_source_descriptor_and_previous_transaction() {
        use coincube_core::{
            foreign_split::SplitBranch,
            miniscript::{
                bitcoin::{absolute::LockTime, transaction::Version, Amount, TxIn},
                Descriptor,
            },
        };
        use std::str::FromStr;
        let descriptor = Descriptor::from_str(
            "wpkh(02c6047f9441ed7d6d3045406e95c07cd85aeb5c6b7a3c2e21b73cdb1e24ff3a64)",
        )
        .unwrap();
        let source = SplitSource::new(descriptor, None).unwrap();
        let script = source
            .external()
            .at_derivation_index(0)
            .unwrap()
            .script_pubkey();
        let previous = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(50_000),
                script_pubkey: script,
            }],
        };
        let mut candidate = SplitCoin {
            outpoint: OutPoint::new(previous.compute_txid(), 0),
            branch: SplitBranch::External,
            index: 0,
            previous,
            bitcoin_block: None,
            btcb2_block: None,
        };
        assert!(source_owns_coin(&source, &candidate));
        candidate.previous.output[0].script_pubkey =
            coincube_core::miniscript::bitcoin::ScriptBuf::new();
        assert!(!source_owns_coin(&source, &candidate));
        candidate.previous.output[0].script_pubkey = source
            .external()
            .at_derivation_index(0)
            .unwrap()
            .script_pubkey();
        candidate.outpoint.vout = 1;
        assert!(!source_owns_coin(&source, &candidate));
        candidate.outpoint.vout = 0;
        candidate.branch = SplitBranch::Internal;
        assert!(!source_owns_coin(&source, &candidate));
    }

    #[test]
    fn shared_psbt_metadata_must_match_the_txid_authenticated_output() {
        use coincube_core::miniscript::bitcoin::{
            absolute::LockTime, psbt::Psbt, transaction::Version, Amount, ScriptBuf, TxIn,
        };
        let previous = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(50_000),
                script_pubkey: ScriptBuf::from_bytes(vec![0, 20, 1]),
            }],
        };
        let outpoint = OutPoint::new(previous.compute_txid(), 0);
        let spend = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: outpoint,
                ..Default::default()
            }],
            output: vec![TxOut {
                value: Amount::from_sat(49_000),
                script_pubkey: ScriptBuf::from_bytes(vec![0, 20, 2]),
            }],
        };
        let outputs = [(outpoint, previous.output[0].clone())];
        let mut psbt = Psbt::from_unsigned_tx(spend).unwrap();
        psbt.inputs[0].witness_utxo = Some(previous.output[0].clone());
        assert_eq!(validate_shared_psbt(&psbt, &outputs), Ok(()));
        psbt.inputs[0].non_witness_utxo = Some(previous.clone());
        assert_eq!(validate_shared_psbt(&psbt, &outputs), Ok(()));
        psbt.inputs[0].witness_utxo.as_mut().unwrap().value = Amount::from_sat(50_001);
        assert_eq!(
            validate_shared_psbt(&psbt, &outputs),
            Err(FailureKind::Changed)
        );
        psbt.inputs[0].witness_utxo = None;
        assert_eq!(validate_shared_psbt(&psbt, &outputs), Ok(()));
        psbt.inputs[0].non_witness_utxo.as_mut().unwrap().output[0].value =
            Amount::from_sat(50_001);
        assert_eq!(
            validate_shared_psbt(&psbt, &outputs),
            Err(FailureKind::Changed)
        );
        psbt.inputs[0].non_witness_utxo = None;
        assert_eq!(
            validate_shared_psbt(&psbt, &outputs),
            Err(FailureKind::Changed)
        );
    }

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
