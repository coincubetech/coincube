//! Bounded, read-only paired inventory for a known COINCUBE Vault.
//! Scan reports are hints until live ancestry, source-wallet state and spend
//! admission all agree. Neither this type nor a foreign scanner grants signing.

use super::{
    claim_observation::http::{
        ClaimPairError, DiscoveryError, FundingContext, HttpObservationSource, OwnedClaimFunding,
    },
    coincube::CoincubeClient,
    foreign_scan::{Branch, BranchRange, ScanDescriptor, ScanError, ScanPlan},
    foreign_split_inventory::{self, FreshDepositAddresses, InventoryError, TwoChainScan},
};
use crate::daemon::{model, Daemon};
use coincube_core::{
    chain::ChainId,
    claim::Policy,
    claim_spend::{AncestrySelfTransfer, ClaimForkSweep},
    descriptors::CoincubeDescriptor,
    foreign_split::{SplitBranch, SplitCoin},
    miniscript::bitcoin::{
        bip32::ChildNumber, secp256k1::Secp256k1, Network, OutPoint, Transaction, Txid,
    },
    spend::{CandidateCoin, TxGetter},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    convert::TryFrom,
};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimInventoryError {
    Descriptor(ScanError),
    Scan(InventoryError),
    Ownership,
    TooManyFundingCandidates,
    WalletState,
}

/// The full paired scan is retained for status and coverage reporting. The
/// candidate slices contain no live ancestry proof or durable reservation.
#[derive(Debug, Clone)]
pub struct ClaimInventory {
    descriptor: CoincubeDescriptor,
    scan: TwoChainScan,
    shared: Vec<CandidateCoin>,
    bitcoin_funding: Vec<CandidateCoin>,
    fork_funding: Vec<CandidateCoin>,
}

struct ScannedTransactions(BTreeMap<Txid, Transaction>);

impl TxGetter for ScannedTransactions {
    fn get_tx(&mut self, txid: &Txid) -> Option<Transaction> {
        self.0.get(txid).cloned()
    }
}

fn owned_candidate(
    descriptor: &CoincubeDescriptor,
    coin: &SplitCoin,
) -> Result<CandidateCoin, ClaimInventoryError> {
    let output = coin
        .previous
        .output
        .get(coin.outpoint.vout as usize)
        .ok_or(ClaimInventoryError::Ownership)?;
    if coin.index >= (1 << 31) || coin.previous.compute_txid() != coin.outpoint.txid {
        return Err(ClaimInventoryError::Ownership);
    }
    let (is_change, branch) = match coin.branch {
        SplitBranch::External => (false, descriptor.receive_descriptor()),
        SplitBranch::Internal => (true, descriptor.change_descriptor()),
    };
    let index = ChildNumber::Normal { index: coin.index };
    if branch
        .derive(index, &Secp256k1::verification_only())
        .script_pubkey()
        != output.script_pubkey
    {
        return Err(ClaimInventoryError::Ownership);
    }
    Ok(CandidateCoin {
        outpoint: coin.outpoint,
        amount: output.value,
        deriv_index: index,
        is_change,
        must_select: true,
        sequence: None,
        ancestor_info: None,
    })
}

impl ClaimInventory {
    /// Read both chains using the same validated Vault descriptor. Bounds are
    /// caller-supplied so the wallet's known derivation range can be covered.
    /// The scanner stops after its configured history gap; its result must be
    /// reconciled with the wallet's full UTXO set before spending.
    pub async fn scan(
        client: CoincubeClient,
        descriptor: &CoincubeDescriptor,
        end_exclusive: u32,
        gap: u32,
        expected: u64,
        generation: watch::Receiver<u64>,
    ) -> Result<Self, ClaimInventoryError> {
        let branches = [Branch::External, Branch::Internal]
            .iter()
            .map(|branch| {
                Ok(BranchRange {
                    descriptor: ScanDescriptor::for_cube(*branch, descriptor)
                        .map_err(ClaimInventoryError::Descriptor)?,
                    start: 0,
                    end_exclusive,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let scan = foreign_split_inventory::scan(
            client,
            ScanPlan {
                chain: ChainId::BitcoinBlake2b,
                branches,
                gap,
            },
            expected,
            generation,
        )
        .await
        .map_err(ClaimInventoryError::Scan)?;
        Self::from_scan(descriptor, scan)
    }

    fn from_scan(
        descriptor: &CoincubeDescriptor,
        scan: TwoChainScan,
    ) -> Result<Self, ClaimInventoryError> {
        let shared = scan
            .inventory
            .splittable_coins()
            .iter()
            .map(|coin| owned_candidate(descriptor, coin))
            .collect::<Result<Vec<_>, _>>()?;
        let bitcoin_funding = scan
            .inventory
            .funding_candidates(ChainId::Bitcoin)
            .iter()
            .map(|coin| owned_candidate(descriptor, coin))
            .collect::<Result<Vec<_>, _>>()?;
        let fork_funding = scan
            .inventory
            .funding_candidates(ChainId::BitcoinBlake2b)
            .iter()
            .map(|coin| owned_candidate(descriptor, coin))
            .collect::<Result<Vec<_>, _>>()?;
        // The paired ancestry collector deliberately refuses a truncated
        // candidate set. A larger wallet needs a wider admission design, not
        // silently selecting the first 32 coins.
        if bitcoin_funding.len() > crate::services::claim_observation::http::MAX_ANCESTRY_CANDIDATES
            || fork_funding.len()
                > crate::services::claim_observation::http::MAX_ANCESTRY_CANDIDATES
        {
            return Err(ClaimInventoryError::TooManyFundingCandidates);
        }
        Ok(Self {
            descriptor: descriptor.clone(),
            scan,
            shared,
            bitcoin_funding,
            fork_funding,
        })
    }

    pub fn scan_report(&self) -> &TwoChainScan {
        &self.scan
    }
    pub fn descriptor(&self) -> &CoincubeDescriptor {
        &self.descriptor
    }
    /// Read-only candidate indices; reserve durably and recheck both chains'
    /// address histories before showing any deposit instructions.
    pub fn fresh_deposit_addresses(&self) -> Option<FreshDepositAddresses> {
        self.scan.inventory.fresh_deposit_addresses()
    }
    pub fn shared(&self) -> &[CandidateCoin] {
        &self.shared
    }
    pub fn bitcoin_funding(&self) -> &[CandidateCoin] {
        &self.bitcoin_funding
    }
    pub fn fork_funding(&self) -> &[CandidateCoin] {
        &self.fork_funding
    }

    fn scanned_transactions(&self) -> Result<ScannedTransactions, ClaimPairError> {
        let mut transactions = BTreeMap::new();
        for coin in self
            .scan
            .inventory
            .splittable_coins()
            .into_iter()
            .chain(self.scan.inventory.funding_candidates(ChainId::Bitcoin))
            .chain(
                self.scan
                    .inventory
                    .funding_candidates(ChainId::BitcoinBlake2b),
            )
        {
            if coin.previous.compute_txid() != coin.outpoint.txid
                || coin
                    .previous
                    .output
                    .get(coin.outpoint.vout as usize)
                    .is_none()
            {
                return Err(ClaimPairError::Changed);
            }
            match transactions.insert(coin.outpoint.txid, coin.previous.clone()) {
                Some(previous) if previous != coin.previous => return Err(ClaimPairError::Changed),
                _ => {}
            }
        }
        Ok(ScannedTransactions(transactions))
    }

    /// Build an exact pair for a read-only review from the transactions in
    /// the same paired scan that supplied the owned coins. The two change
    /// indices are supplied by the wallet owner; this does not reserve them
    /// or admit either PSBT for signing.
    #[allow(clippy::too_many_arguments)]
    pub fn construct_pair_for_review(
        &self,
        funding: &OwnedClaimFunding,
        bitcoin_change: ChildNumber,
        fork_change: ChildNumber,
        bitcoin_feerate_vb: u64,
        fork_feerate_vb: u64,
        context: FundingContext<'_>,
    ) -> Result<(AncestrySelfTransfer, ClaimForkSweep), ClaimPairError> {
        funding.construct_claim_pair(
            &self.descriptor,
            &mut self.scanned_transactions()?,
            &self.shared,
            bitcoin_change,
            fork_change,
            bitcoin_feerate_vb,
            fork_feerate_vb,
            context,
        )
    }

    /// Compare the paired scan with the Bitcoin Vault daemon's full wallet
    /// state. A bounded address walk may miss an older pre-fork coin outside
    /// its range; such a coin must stop the all-coins Claim. Callers must use
    /// an unfiltered `list_coins` response, not a selected-coin subset.
    pub fn reconcile_wallet_snapshot(
        &self,
        info: &model::GetInfoResult,
        coins: &[model::Coin],
        bitcoin_funding_index: u32,
    ) -> Result<(), ClaimInventoryError> {
        self.reconcile_chain_snapshot(ChainId::Bitcoin, info, coins, bitcoin_funding_index)
    }

    /// Apply the same complete-wallet check to the paired BTCB2 Vault. The
    /// fork has its own block heights and funding coin; Bitcoin's wallet
    /// snapshot cannot stand in for this read.
    pub fn reconcile_fork_wallet_snapshot(
        &self,
        info: &model::GetInfoResult,
        coins: &[model::Coin],
        fork_funding_index: u32,
    ) -> Result<(), ClaimInventoryError> {
        self.reconcile_chain_snapshot(ChainId::BitcoinBlake2b, info, coins, fork_funding_index)
    }

    fn reconcile_chain_snapshot(
        &self,
        chain: ChainId,
        info: &model::GetInfoResult,
        coins: &[model::Coin],
        funding_index: u32,
    ) -> Result<(), ClaimInventoryError> {
        let stale = || ClaimInventoryError::WalletState;
        let (report, funding_candidates) = match chain {
            ChainId::Bitcoin => (&self.scan.bitcoin, &self.bitcoin_funding),
            ChainId::BitcoinBlake2b => (&self.scan.btcb2, &self.fork_funding),
            _ => return Err(stale()),
        };
        let reports = [&self.scan.bitcoin, &self.scan.btcb2];
        if info.network != Network::Bitcoin
            || info.chain_id != Some(chain)
            || info.descriptors.main != self.descriptor
            || !info.sync.is_finite()
            || info.sync < 0.9999
            || info.block_height < 0
            || u64::try_from(info.block_height).map_err(|_| stale())?
                < u64::from(report.tip_height())
            || info.rescan_progress.is_some()
            || info.refused_reorg_depth.is_some()
            || info.chain_divergence
            || info.history_sync.full_scan_in_progress
            || info.history_sync.last_failure.is_some()
        {
            return Err(stale());
        }
        for report in reports {
            for (branch, revealed) in [
                (Branch::External, info.receive_index),
                (Branch::Internal, info.change_index),
            ] {
                let coverage = report.coverage(branch).ok_or_else(stale)?;
                if coverage.start != 0 || !coverage.contains(revealed) {
                    return Err(stale());
                }
            }
        }
        let fork_height = self.scan.inventory.fork_height();
        let expected: BTreeMap<OutPoint, &CandidateCoin> = self
            .shared
            .iter()
            .map(|coin| (coin.outpoint, coin))
            .collect();
        let scan_shared = self.scan.inventory.splittable_coins();
        if expected.len() != self.shared.len() || expected.len() != scan_shared.len() {
            return Err(stale());
        }
        let mut all = BTreeMap::new();
        let mut current_shared = BTreeSet::new();
        for coin in coins {
            if all.insert(coin.outpoint, coin).is_some() {
                return Err(stale());
            }
            if coin.spend_info.is_none()
                && coin
                    .block_height
                    .is_some_and(|height| height >= 0 && (height as u64) < fork_height)
            {
                if coin.is_immature || !expected.contains_key(&coin.outpoint) {
                    return Err(stale());
                }
                current_shared.insert(coin.outpoint);
            }
        }
        if current_shared.len() != expected.len() {
            return Err(stale());
        }
        for scanned in scan_shared {
            let coin = all.get(&scanned.outpoint).ok_or_else(stale)?;
            let candidate = expected.get(&scanned.outpoint).ok_or_else(stale)?;
            let height = match chain {
                ChainId::Bitcoin => scanned.bitcoin_block,
                ChainId::BitcoinBlake2b => scanned.btcb2_block,
                _ => None,
            }
            .ok_or_else(stale)?
            .height;
            let output = scanned
                .previous
                .output
                .get(scanned.outpoint.vout as usize)
                .ok_or_else(stale)?;
            if scanned.btcb2_block.is_none()
                || coin.spend_info.is_some()
                || coin.is_immature
                || coin.block_height != i32::try_from(height).ok()
                || coin.amount != candidate.amount
                || coin.derivation_index != candidate.deriv_index
                || coin.is_change != candidate.is_change
                || coin.address.script_pubkey() != output.script_pubkey
            {
                return Err(stale());
            }
        }
        let mut checked_funding = false;
        for candidate in funding_candidates.iter().filter(|candidate| {
            !candidate.is_change
                && candidate.deriv_index
                    == ChildNumber::Normal {
                        index: funding_index,
                    }
        }) {
            let coin = all.get(&candidate.outpoint).ok_or_else(stale)?;
            let scanned = report
                .coins()
                .iter()
                .find(|coin| coin.outpoint == candidate.outpoint)
                .ok_or_else(stale)?;
            if coin.spend_info.is_some()
                || coin.is_immature
                || coin.block_height != scanned.block_height.and_then(|h| i32::try_from(h).ok())
                || !coin
                    .block_height
                    .is_some_and(|h| h >= 0 && (h as u64) >= fork_height)
                || coin.amount != candidate.amount
                || coin.derivation_index != candidate.deriv_index
                || coin.is_change
                || coin.address.script_pubkey() != scanned.output.script_pubkey
            {
                return Err(stale());
            }
            checked_funding = true;
        }
        if !checked_funding {
            return Err(stale());
        }
        Ok(())
    }

    /// Read the daemon's complete coin set for paired proof discovery or
    /// preview construction. This is a review prerequisite, not signer
    /// admission: tips, ancestry and unspent state need a fresh recheck when
    /// Sign is pressed.
    pub async fn reconcile_daemon(
        &self,
        daemon: &(dyn Daemon + Send + Sync),
        bitcoin_funding_index: u32,
    ) -> Result<(), ClaimInventoryError> {
        let info = daemon
            .get_info()
            .await
            .map_err(|_| ClaimInventoryError::WalletState)?;
        let coins = daemon
            .list_coins(&[], &[])
            .await
            .map_err(|_| ClaimInventoryError::WalletState)?;
        self.reconcile_wallet_snapshot(&info, &coins.coins, bitcoin_funding_index)
    }

    /// Check the complete paired fork wallet before showing an exact Claim
    /// pair. This remains a read-only preview prerequisite; signer admission
    /// must repeat it after a fresh two-chain collection.
    pub async fn reconcile_fork_daemon(
        &self,
        daemon: &(dyn Daemon + Send + Sync),
        fork_funding_index: u32,
    ) -> Result<(), ClaimInventoryError> {
        let info = daemon
            .get_info()
            .await
            .map_err(|_| ClaimInventoryError::WalletState)?;
        let coins = daemon
            .list_coins(&[], &[])
            .await
            .map_err(|_| ClaimInventoryError::WalletState)?;
        self.reconcile_fork_wallet_snapshot(&info, &coins.coins, fork_funding_index)
    }

    /// Bind this scan's candidates to one fresh paired ancestry collection.
    /// The selected result remains review-only; the complete wallet UTXO set,
    /// reservations and current spend preflight still need separate admission.
    pub async fn discover_funding(
        &self,
        source: &HttpObservationSource,
        policy: Policy,
    ) -> Result<Option<OwnedClaimFunding>, DiscoveryError> {
        source
            .discover_owned_claim_funding(
                &self.descriptor,
                &self.bitcoin_funding,
                &self.fork_funding,
                &self.shared,
                policy,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::foreign_scan::{BranchCoverage, DiscoveredCoin, ScanReport};
    use coincube_core::{
        bip39::Mnemonic,
        descriptors::{CoincubePolicy, PathInfo},
        miniscript::{
            bitcoin::{
                absolute::LockTime, bip32::DerivationPath, hashes::Hash, secp256k1,
                transaction::Version, Address, Amount, BlockHash, Network, OutPoint, Transaction,
                TxIn, TxOut,
            },
            DescriptorPublicKey,
        },
        signer::MasterSigner,
    };
    use std::str::FromStr;

    fn descriptor() -> CoincubeDescriptor {
        let secp = secp256k1::Secp256k1::new();
        let keys: Vec<_> = [71_u8, 72]
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

    #[test]
    fn known_cube_policy_scans_both_branches_and_authenticates_coins() {
        let descriptor = descriptor();
        let verify = Secp256k1::verification_only();
        for (branch, is_change) in [(Branch::External, false), (Branch::Internal, true)] {
            let scan = ScanDescriptor::for_cube(branch, &descriptor).unwrap();
            let expected = if is_change {
                descriptor.change_descriptor()
            } else {
                descriptor.receive_descriptor()
            }
            .derive(3.into(), &verify)
            .script_pubkey();
            assert_eq!(scan.script(3).unwrap(), expected);
            let previous = Transaction {
                version: Version::TWO,
                lock_time: LockTime::ZERO,
                input: vec![TxIn::default()],
                output: vec![TxOut {
                    value: Amount::from_sat(50_000),
                    script_pubkey: expected,
                }],
            };
            let mut coin = SplitCoin {
                outpoint: OutPoint::new(previous.compute_txid(), 0),
                branch: if is_change {
                    SplitBranch::Internal
                } else {
                    SplitBranch::External
                },
                index: 3,
                previous,
                bitcoin_block: None,
                btcb2_block: None,
            };
            let owned = owned_candidate(&descriptor, &coin).unwrap();
            assert_eq!(owned.amount, Amount::from_sat(50_000));
            assert_eq!(owned.is_change, is_change);
            assert_eq!(owned.deriv_index, ChildNumber::Normal { index: 3 });
            coin.index = 4;
            assert_eq!(
                owned_candidate(&descriptor, &coin),
                Err(ClaimInventoryError::Ownership)
            );
        }
    }

    #[test]
    fn wallet_reconciliation_refuses_unscanned_shared_coin_and_incomplete_coverage() {
        let descriptor = descriptor();
        let script = descriptor
            .receive_descriptor()
            .derive(3.into(), &Secp256k1::verification_only())
            .script_pubkey();
        let previous = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(50_000),
                script_pubkey: script.clone(),
            }],
        };
        let outpoint = OutPoint::new(previous.compute_txid(), 0);
        let shared = DiscoveredCoin {
            branch: Branch::External,
            index: 3,
            outpoint,
            output: previous.output[0].clone(),
            previous,
            confirmed: true,
            block_height: Some(90),
            block_hash: Some(BlockHash::all_zeros()),
        };
        let funding_script = descriptor
            .receive_descriptor()
            .derive(4.into(), &Secp256k1::verification_only())
            .script_pubkey();
        let funding_previous = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey: funding_script.clone(),
            }],
        };
        let funding_outpoint = OutPoint::new(funding_previous.compute_txid(), 0);
        let funding = DiscoveredCoin {
            branch: Branch::External,
            index: 4,
            outpoint: funding_outpoint,
            output: funding_previous.output[0].clone(),
            previous: funding_previous,
            confirmed: true,
            block_height: Some(110),
            block_hash: Some(BlockHash::all_zeros()),
        };
        let fork_script = descriptor
            .receive_descriptor()
            .derive(5.into(), &Secp256k1::verification_only())
            .script_pubkey();
        let fork_previous = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(12_000),
                script_pubkey: fork_script.clone(),
            }],
        };
        let fork_outpoint = OutPoint::new(fork_previous.compute_txid(), 0);
        let fork_funding = DiscoveredCoin {
            branch: Branch::External,
            index: 5,
            outpoint: fork_outpoint,
            output: fork_previous.output[0].clone(),
            previous: fork_previous,
            confirmed: true,
            block_height: Some(111),
            block_hash: Some(BlockHash::all_zeros()),
        };
        let coverage = [Branch::External, Branch::Internal]
            .iter()
            .copied()
            .map(|branch| BranchCoverage {
                branch,
                start: 0,
                end_exclusive: 10,
                last_used: None,
            })
            .collect::<Vec<_>>();
        let tip = BlockHash::all_zeros();
        let bitcoin = ScanReport::for_test(ChainId::Bitcoin, 1, tip, vec![shared.clone(), funding])
            .with_tip_height(130)
            .with_coverage(coverage.clone());
        let btcb2 =
            ScanReport::for_test(ChainId::BitcoinBlake2b, 1, tip, vec![shared, fork_funding])
                .with_fork_height(Some(100))
                .with_tip_height(130)
                .with_coverage(coverage);
        let inventory =
            foreign_split_inventory::SplitInventory::join(&btcb2, &bitcoin, 1, true).unwrap();
        let scan = ClaimInventory::from_scan(
            &descriptor,
            TwoChainScan {
                bitcoin,
                btcb2,
                inventory,
            },
        )
        .unwrap();
        let mut previous = scan.scanned_transactions().unwrap();
        assert_eq!(
            previous.get_tx(&outpoint.txid).unwrap().output[0].value,
            Amount::from_sat(50_000)
        );
        assert_eq!(
            previous.get_tx(&funding_outpoint.txid).unwrap().output[0].value,
            Amount::from_sat(10_000)
        );
        let mut info = model::GetInfoResult {
            version: "test".into(),
            network: Network::Bitcoin,
            chain_id: Some(coincube_core::chain::ChainId::Bitcoin),
            block_height: 130,
            sync: 1.0,
            descriptors: coincubed::commands::GetInfoDescriptors {
                main: descriptor.clone(),
            },
            rescan_progress: None,
            refused_reorg_depth: None,
            chain_divergence: false,
            timestamp: 0,
            last_poll_timestamp: Some(1),
            history_sync: Default::default(),
            receive_index: 3,
            change_index: 3,
        };
        let coin = model::Coin {
            amount: Amount::from_sat(50_000),
            outpoint,
            address: Address::from_script(&script, Network::Bitcoin).unwrap(),
            block_height: Some(90),
            derivation_index: ChildNumber::Normal { index: 3 },
            spend_info: None,
            is_immature: false,
            is_change: false,
            is_from_self: false,
        };
        let funding_coin = model::Coin {
            amount: Amount::from_sat(10_000),
            outpoint: funding_outpoint,
            address: Address::from_script(&funding_script, Network::Bitcoin).unwrap(),
            block_height: Some(110),
            derivation_index: ChildNumber::Normal { index: 4 },
            spend_info: None,
            is_immature: false,
            is_change: false,
            is_from_self: false,
        };
        let fork_coin = model::Coin {
            amount: Amount::from_sat(12_000),
            outpoint: fork_outpoint,
            address: Address::from_script(&fork_script, Network::Bitcoin).unwrap(),
            block_height: Some(111),
            derivation_index: ChildNumber::Normal { index: 5 },
            spend_info: None,
            is_immature: false,
            is_change: false,
            is_from_self: false,
        };
        assert_eq!(
            scan.reconcile_wallet_snapshot(&info, &[coin.clone(), funding_coin.clone()], 4),
            Ok(())
        );
        let mut fork_info = info.clone();
        fork_info.chain_id = Some(ChainId::BitcoinBlake2b);
        assert_eq!(
            scan.reconcile_fork_wallet_snapshot(&fork_info, &[coin.clone(), fork_coin.clone()], 5),
            Ok(())
        );
        assert_eq!(
            scan.reconcile_fork_wallet_snapshot(&fork_info, std::slice::from_ref(&fork_coin), 5),
            Err(ClaimInventoryError::WalletState)
        );
        assert_eq!(
            scan.reconcile_fork_wallet_snapshot(&fork_info, std::slice::from_ref(&coin), 5),
            Err(ClaimInventoryError::WalletState)
        );
        assert_eq!(
            scan.reconcile_fork_wallet_snapshot(&fork_info, &[coin.clone(), fork_coin.clone()], 6),
            Err(ClaimInventoryError::WalletState)
        );
        let mut wrong_fork_coin = fork_coin.clone();
        wrong_fork_coin.block_height = Some(110);
        assert_eq!(
            scan.reconcile_fork_wallet_snapshot(&fork_info, &[coin.clone(), wrong_fork_coin], 5),
            Err(ClaimInventoryError::WalletState)
        );
        fork_info.chain_id = Some(ChainId::Bitcoin);
        assert_eq!(
            scan.reconcile_fork_wallet_snapshot(&fork_info, &[coin.clone(), fork_coin], 5),
            Err(ClaimInventoryError::WalletState)
        );
        assert_eq!(
            scan.reconcile_wallet_snapshot(&info, std::slice::from_ref(&funding_coin), 4),
            Err(ClaimInventoryError::WalletState)
        );
        assert_eq!(
            scan.reconcile_wallet_snapshot(&info, std::slice::from_ref(&coin), 4),
            Err(ClaimInventoryError::WalletState)
        );
        let extra = model::Coin {
            outpoint: OutPoint::null(),
            ..coin.clone()
        };
        assert_eq!(
            scan.reconcile_wallet_snapshot(&info, &[coin.clone(), funding_coin.clone(), extra], 4),
            Err(ClaimInventoryError::WalletState)
        );
        assert_eq!(
            scan.reconcile_wallet_snapshot(
                &info,
                &[
                    model::Coin {
                        spend_info: Some(coincubed::commands::LCSpendInfo {
                            txid: coincube_core::miniscript::bitcoin::Txid::all_zeros(),
                            height: None,
                        }),
                        ..coin.clone()
                    },
                    funding_coin.clone()
                ],
                4
            ),
            Err(ClaimInventoryError::WalletState)
        );
        assert_eq!(
            scan.reconcile_wallet_snapshot(
                &info,
                &[
                    coin.clone(),
                    model::Coin {
                        spend_info: Some(coincubed::commands::LCSpendInfo {
                            txid: coincube_core::miniscript::bitcoin::Txid::all_zeros(),
                            height: None,
                        }),
                        ..funding_coin.clone()
                    }
                ],
                4
            ),
            Err(ClaimInventoryError::WalletState)
        );
        info.chain_id = None;
        assert_eq!(
            scan.reconcile_wallet_snapshot(&info, &[coin.clone(), funding_coin.clone()], 4),
            Err(ClaimInventoryError::WalletState)
        );
        info.chain_id = Some(ChainId::BitcoinBlake2b);
        assert_eq!(
            scan.reconcile_wallet_snapshot(&info, &[coin.clone(), funding_coin.clone()], 4),
            Err(ClaimInventoryError::WalletState)
        );
        info.chain_id = Some(ChainId::Bitcoin);
        info.receive_index = 10;
        assert_eq!(
            scan.reconcile_wallet_snapshot(&info, &[coin, funding_coin], 4),
            Err(ClaimInventoryError::WalletState)
        );
    }
}
