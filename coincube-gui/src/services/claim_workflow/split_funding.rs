//! Immutable pre-deposit address reservation for a foreign-wallet Split.
//! This record is deliberately separate from an ancestry transaction intent:
//! users need the deposit addresses before either funding coin exists.
//! Loading it does not grant signing or broadcasting authority.

use super::{journal::Journal, Context, Error};
use crate::services::{
    claim_observation::http::{DiscoveryError, HttpObservationSource, OwnedSplitFunding},
    foreign_split_inventory::{FreshFundingAddresses, SplitInventory},
};
use coincube_core::{
    claim::Policy,
    foreign_split::SplitSource,
    miniscript::{
        bitcoin::{hashes::sha256, Address, Network},
        Descriptor, DescriptorPublicKey,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

#[derive(Debug)]
pub enum SplitFundingDiscoveryError {
    InvalidReservation(Error),
    Changed,
    Discovery(DiscoveryError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitFundingReservation {
    version: u32,
    pub source_digest: sha256::Hash,
    pub target_cube: String,
    pub account: String,
    pub provider: String,
    external: String,
    internal: Option<String>,
    pub indices: FreshFundingAddresses,
    /// The initial unused-history check passed before these addresses were
    /// shown. Later deposits make history nonempty, so restart must reuse the
    /// exact saved addresses rather than applying that first-use check again.
    #[serde(default)]
    released: bool,
}

impl SplitFundingReservation {
    fn new(
        source: &SplitSource,
        target_cube: &str,
        context: &Context,
        indices: FreshFundingAddresses,
    ) -> Result<Self, Error> {
        let record = Self {
            version: 2,
            source_digest: source.digest(),
            target_cube: target_cube.to_owned(),
            account: context.account.clone(),
            provider: context.provider.clone(),
            external: source.external().to_string(),
            internal: source.internal().map(ToString::to_string),
            indices,
            released: false,
        };
        record.validate()?;
        Ok(record)
    }

    pub(super) fn validate(&self) -> Result<(), Error> {
        if !matches!(self.version, 1 | 2)
            || (self.version == 1 && self.released)
            || self.target_cube.is_empty()
            || self.account.is_empty()
            || self.provider.is_empty()
            || self.target_cube.len() > 256
            || self.account.len() > 256
            || self.provider.len() > 2048
            || self.external.len() > 16_384
            || self
                .internal
                .as_ref()
                .is_some_and(|text| text.len() > 16_384)
        {
            return Err(Error::InvalidPlan);
        }
        let [bitcoin, fork, returning] = [
            self.indices.bitcoin_deposit,
            self.indices.btcb2_deposit,
            self.indices.bitcoin_return,
        ];
        if [bitcoin, fork, returning]
            .iter()
            .any(|index| *index >= 1 << 31)
            || bitcoin == fork
            || bitcoin == returning
            || fork == returning
        {
            return Err(Error::InvalidPlan);
        }
        let source = self.source()?;
        if !source.external().has_wildcard() || source.digest() != self.source_digest {
            return Err(Error::InvalidPlan);
        }
        for index in [bitcoin, fork, returning] {
            self.address(index)?;
        }
        Ok(())
    }

    pub fn source(&self) -> Result<SplitSource, Error> {
        let external = Descriptor::<DescriptorPublicKey>::from_str(&self.external)
            .map_err(|_| Error::InvalidPlan)?;
        let internal = self
            .internal
            .as_deref()
            .map(Descriptor::<DescriptorPublicKey>::from_str)
            .transpose()
            .map_err(|_| Error::InvalidPlan)?;
        SplitSource::new(external, internal).map_err(|_| Error::InvalidPlan)
    }

    fn address(&self, index: u32) -> Result<String, Error> {
        let script = self
            .source()?
            .external()
            .at_derivation_index(index)
            .map_err(|_| Error::InvalidPlan)?
            .script_pubkey();
        Address::from_script(&script, Network::Bitcoin)
            .map(|address| address.to_string())
            .map_err(|_| Error::InvalidPlan)
    }

    /// These strings are safe to show only after fresh, authenticated checks
    /// prove all three addresses unused on both chains under the current
    /// Connect account. The reservation itself is never that proof.
    pub fn addresses(&self) -> Result<[String; 3], Error> {
        Ok([
            self.address(self.indices.bitcoin_deposit)?,
            self.address(self.indices.btcb2_deposit)?,
            self.address(self.indices.bitcoin_return)?,
        ])
    }

    pub fn is_released(&self) -> bool {
        self.released
    }

    pub(super) fn released_copy(&self) -> Self {
        let mut copy = self.clone();
        copy.version = 2;
        copy.released = true;
        copy
    }

    pub fn matches(&self, source: &SplitSource, target_cube: &str, context: &Context) -> bool {
        self.source_digest == source.digest()
            && self.target_cube == target_cube
            && self.account == context.account
            && self.provider == context.provider
            && self.source().is_ok_and(|recorded| recorded == *source)
    }

    fn checked_reserved_inventory(
        &self,
        directory: &Path,
        source: &SplitSource,
        context: &Context,
        inventory: &SplitInventory,
    ) -> Result<SplitInventory, SplitFundingDiscoveryError> {
        if !self.is_released() {
            return Err(SplitFundingDiscoveryError::Changed);
        }
        let saved = load_split_funding(directory, source, &self.target_cube, context)
            .map_err(SplitFundingDiscoveryError::InvalidReservation)?
            .ok_or(SplitFundingDiscoveryError::Changed)?;
        if saved != *self || !saved.matches(source, &self.target_cube, context) {
            return Err(SplitFundingDiscoveryError::Changed);
        }
        Ok(inventory.funding_at_reserved_addresses(self.indices))
    }

    /// Reload the exact pre-deposit selection and search only its two receive
    /// indices under one live observation context. This is read-only evidence,
    /// not a source-wallet reconciliation or signer capability.
    pub async fn discover_reserved_funding(
        &self,
        directory: PathBuf,
        source: &SplitSource,
        context: &Context,
        inventory: &SplitInventory,
        observations: &HttpObservationSource,
        policy: Policy,
    ) -> Result<Option<OwnedSplitFunding>, SplitFundingDiscoveryError> {
        observations
            .validate_context(&context.provider, context.generation)
            .map_err(|_| SplitFundingDiscoveryError::Changed)?;
        // A foreign wallet scan has its own generation, independent of the
        // Connect account generation used by the observation source.
        let reservation = self.clone();
        let source_for_load = source.clone();
        let context_for_load = context.clone();
        let inventory_for_load = inventory.clone();
        let reserved = tokio::task::spawn_blocking(move || {
            reservation.checked_reserved_inventory(
                &directory,
                &source_for_load,
                &context_for_load,
                &inventory_for_load,
            )
        })
        .await
        .map_err(|_| SplitFundingDiscoveryError::Changed)??;
        if reserved
            .funding_candidates(coincube_core::chain::ChainId::Bitcoin)
            .is_empty()
            || reserved
                .funding_candidates(coincube_core::chain::ChainId::BitcoinBlake2b)
                .is_empty()
        {
            return Ok(None);
        }
        let found = observations
            .discover_owned_split_funding(&reserved, source, policy)
            .await
            .map_err(SplitFundingDiscoveryError::Discovery)?;
        observations
            .validate_context(&context.provider, context.generation)
            .map_err(|_| SplitFundingDiscoveryError::Changed)?;
        Ok(found)
    }
}

/// Save the exact three addresses before showing deposit instructions. Once
/// saved, a new scan cannot choose new indices for this source/target pair.
/// The first display requires unused history on both chains and a durable
/// release; later displays reuse those same addresses after identity checks.
/// Funding proofs must still be recollected before a spend.
pub fn reserve_split_funding(
    directory: &Path,
    source: &SplitSource,
    target_cube: &str,
    context: &Context,
    inventory: &SplitInventory,
) -> Result<SplitFundingReservation, Error> {
    super::prepare_directory(directory)?;
    let mut journal = Journal::open(directory)?;
    if journal.load()?.is_some()
        || journal.load_claim_funding_reservation()?.is_some()
        || std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok()
    {
        return Err(Error::Conflict);
    }
    if let Some(saved) = journal.load_split_funding_reservation()? {
        return if saved.matches(source, target_cube, context) {
            Ok(saved)
        } else {
            Err(Error::WrongIdentity)
        };
    }
    let indices = inventory
        .fresh_funding_addresses()
        .ok_or(Error::InvalidPlan)?;
    let requested = SplitFundingReservation::new(source, target_cube, context, indices)?;
    journal.store_split_funding_reservation(&requested)?;
    Ok(requested)
}

/// Recover the immutable address selection after restart. The caller must
/// match the saved account/provider and source before making any address
/// visible. A released reservation may have nonempty deposit history.
pub fn load_split_funding(
    directory: &Path,
    source: &SplitSource,
    target_cube: &str,
    context: &Context,
) -> Result<Option<SplitFundingReservation>, Error> {
    let journal = Journal::open(directory)?;
    if journal.load_claim_funding_reservation()?.is_some()
        || std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok()
    {
        return Err(Error::Conflict);
    }
    let saved = journal.load_split_funding_reservation()?;
    match saved {
        Some(saved) if saved.matches(source, target_cube, context) => Ok(Some(saved)),
        Some(_) => Err(Error::WrongIdentity),
        None => Ok(None),
    }
}

/// Read a funding-only reservation under the journal lock before the
/// account-specific checks. Callers must still compare its target, source and
/// Connect identity before displaying an address or acting on a deposit.
pub fn peek_split_funding(directory: &Path) -> Result<Option<SplitFundingReservation>, Error> {
    let journal = Journal::open(directory)?;
    if journal.load()?.is_some()
        || journal.load_claim_funding_reservation()?.is_some()
        || std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok()
    {
        return Err(Error::Conflict);
    }
    journal.load_split_funding_reservation()
}

/// Record that the initial two-chain unused-address check succeeded. The
/// journal lock compares the exact reservation before atomically replacing it.
pub fn release_split_funding(
    directory: &Path,
    expected: &SplitFundingReservation,
) -> Result<SplitFundingReservation, Error> {
    let mut journal = Journal::open(directory)?;
    journal.release_split_funding_reservation(expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        chain::ChainId,
        services::foreign_scan::{Branch, BranchCoverage, ScanReport},
    };
    use coincube_core::miniscript::bitcoin::{
        bip32::{Xpriv, Xpub},
        hashes::Hash,
        secp256k1::Secp256k1,
        BlockHash,
    };

    fn source() -> SplitSource {
        let secp = Secp256k1::new();
        let private = Xpriv::new_master(Network::Bitcoin, &[7; 64]).unwrap();
        let public = Xpub::from_priv(&secp, &private);
        let external = Descriptor::from_str(&format!("wpkh({public}/0/*)")).unwrap();
        let internal = Descriptor::from_str(&format!("wpkh({public}/1/*)")).unwrap();
        SplitSource::new(external, Some(internal)).unwrap()
    }

    fn context() -> Context {
        Context {
            generation: 1,
            account: "account-a".into(),
            provider: "https://connect.example".into(),
        }
    }

    fn indices() -> FreshFundingAddresses {
        FreshFundingAddresses {
            bitcoin_deposit: 4,
            btcb2_deposit: 5,
            bitcoin_return: 6,
        }
    }

    fn inventory(last_used: u32) -> SplitInventory {
        let report = |chain, byte| {
            ScanReport::for_test(chain, 1, BlockHash::from_byte_array([byte; 32]), Vec::new())
                .with_coverage(vec![BranchCoverage {
                    branch: Branch::External,
                    start: 0,
                    end_exclusive: 20,
                    last_used: Some(last_used),
                }])
        };
        SplitInventory::join(
            &report(ChainId::BitcoinBlake2b, 3).with_fork_height(Some(2)),
            &report(ChainId::Bitcoin, 4),
            1,
            true,
        )
        .unwrap()
    }

    #[test]
    fn split_funding_reservation_survives_restart_and_cannot_move() {
        let directory = std::env::temp_dir().join(format!(
            "split-funding-{}-{}",
            std::process::id(),
            super::super::controller_id().unwrap()
        ));
        let source = source();
        let saved = reserve_split_funding(&directory, &source, "cube-a", &context(), &inventory(3))
            .unwrap();
        let mut old = serde_json::to_value(&saved).unwrap();
        old.as_object_mut().unwrap().remove("released");
        old["version"] = serde_json::json!(1);
        let old: SplitFundingReservation = serde_json::from_value(old).unwrap();
        assert!(!old.is_released());
        assert_eq!(saved.addresses().unwrap().len(), 3);
        let released = release_split_funding(&directory, &saved).unwrap();
        assert!(released.is_released());
        assert_eq!(released.addresses().unwrap(), saved.addresses().unwrap());
        assert!(matches!(
            release_split_funding(&directory, &saved),
            Err(Error::Conflict)
        ));
        let mut restarted = context();
        restarted.generation += 1;
        assert_eq!(
            load_split_funding(&directory, &source, "cube-a", &restarted)
                .unwrap()
                .unwrap(),
            released
        );
        assert_eq!(
            reserve_split_funding(&directory, &source, "cube-a", &restarted, &inventory(3))
                .unwrap(),
            released
        );
        assert_eq!(
            reserve_split_funding(&directory, &source, "cube-a", &context(), &inventory(4))
                .unwrap(),
            released
        );
        let mut other_account = context();
        other_account.account = "account-b".into();
        assert!(matches!(
            load_split_funding(&directory, &source, "cube-a", &other_account),
            Err(Error::WrongIdentity)
        ));
        std::fs::write(directory.join("split-funding.json"), b"{}").unwrap();
        assert!(matches!(
            load_split_funding(&directory, &source, "cube-a", &context()),
            Err(Error::InvalidJournal)
        ));
        assert!(matches!(
            reserve_split_funding(&directory, &source, "cube-a", &context(), &inventory(3)),
            Err(Error::InvalidJournal)
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn split_funding_requires_distinct_ranged_addresses() {
        let source = source();
        assert_eq!(inventory(3).fresh_funding_addresses(), Some(indices()));
        assert!(matches!(
            reserve_split_funding(
                Path::new("not-created"),
                &source,
                "cube-a",
                &context(),
                &inventory(18)
            ),
            Err(Error::InvalidPlan)
        ));
        let mut same = indices();
        same.bitcoin_return = same.btcb2_deposit;
        assert!(matches!(
            SplitFundingReservation::new(&source, "cube-a", &context(), same),
            Err(Error::InvalidPlan)
        ));
        let fixed = Descriptor::from_str(
            "wpkh(02c6047f9441ed7d6d3045406e95c07cd85aeb5c6b7a3c2e21b73cdb1e24ff3a64)",
        )
        .unwrap();
        let fixed = SplitSource::new(fixed, None).unwrap();
        assert!(matches!(
            SplitFundingReservation::new(&fixed, "cube-a", &context(), indices()),
            Err(Error::InvalidPlan)
        ));
    }

    #[test]
    fn reserved_discovery_requires_release_and_account_but_not_matching_scan_generation() {
        let directory = std::env::temp_dir().join(format!(
            "split-funding-discovery-{}-{}",
            std::process::id(),
            super::super::controller_id().unwrap()
        ));
        let source = source();
        let saved = reserve_split_funding(&directory, &source, "cube-a", &context(), &inventory(3))
            .unwrap();
        assert!(matches!(
            saved.checked_reserved_inventory(&directory, &source, &context(), &inventory(3)),
            Err(SplitFundingDiscoveryError::Changed)
        ));
        let released = release_split_funding(&directory, &saved).unwrap();
        let mut later_account = context();
        later_account.generation = 19;
        // The foreign scan generation stays at 1. It is separate from the
        // authenticated Connect account generation.
        assert!(released
            .checked_reserved_inventory(&directory, &source, &later_account, &inventory(3))
            .is_ok());
        later_account.account = "another-account".into();
        assert!(matches!(
            released.checked_reserved_inventory(&directory, &source, &later_account, &inventory(3)),
            Err(SplitFundingDiscoveryError::InvalidReservation(
                Error::WrongIdentity
            ))
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
