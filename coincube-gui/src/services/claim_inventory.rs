//! Bounded, read-only paired inventory for a known COINCUBE Vault.
//! Scan reports are hints until live ancestry, source-wallet state and spend
//! admission all agree. Neither this type nor a foreign scanner grants signing.

use super::{
    claim_observation::http::{DiscoveryError, HttpObservationSource, OwnedClaimFunding},
    coincube::CoincubeClient,
    foreign_scan::{Branch, BranchRange, ScanDescriptor, ScanError, ScanPlan},
    foreign_split_inventory::{self, FreshDepositAddresses, InventoryError, TwoChainScan},
};
use coincube_core::{
    chain::ChainId,
    claim::Policy,
    descriptors::CoincubeDescriptor,
    foreign_split::{SplitBranch, SplitCoin},
    miniscript::bitcoin::{bip32::ChildNumber, secp256k1::Secp256k1},
    spend::CandidateCoin,
};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimInventoryError {
    Descriptor(ScanError),
    Scan(InventoryError),
    Ownership,
    TooManyFundingCandidates,
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
    use coincube_core::{
        bip39::Mnemonic,
        descriptors::{CoincubePolicy, PathInfo},
        miniscript::{
            bitcoin::{
                absolute::LockTime, bip32::DerivationPath, secp256k1, transaction::Version, Amount,
                Network, OutPoint, Transaction, TxIn, TxOut,
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
}
