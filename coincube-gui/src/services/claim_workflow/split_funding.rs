//! Immutable pre-deposit address reservation for a foreign-wallet Split.
//! This record is deliberately separate from an ancestry transaction intent:
//! users need the deposit addresses before either funding coin exists.
//! Loading it does not grant signing or broadcasting authority.

use super::{journal::Journal, Context, Error};
use crate::services::foreign_split_inventory::{FreshFundingAddresses, SplitInventory};
use coincube_core::{
    foreign_split::SplitSource,
    miniscript::{
        bitcoin::{hashes::sha256, Address, Network},
        Descriptor, DescriptorPublicKey,
    },
};
use serde::{Deserialize, Serialize};
use std::{path::Path, str::FromStr};

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
}

impl SplitFundingReservation {
    fn new(
        source: &SplitSource,
        target_cube: &str,
        context: &Context,
        indices: FreshFundingAddresses,
    ) -> Result<Self, Error> {
        let record = Self {
            version: 1,
            source_digest: source.digest(),
            target_cube: target_cube.to_owned(),
            account: context.account.clone(),
            provider: context.provider.clone(),
            external: source.external().to_string(),
            internal: source.internal().map(ToString::to_string),
            indices,
        };
        record.validate()?;
        Ok(record)
    }

    pub(super) fn validate(&self) -> Result<(), Error> {
        if self.version != 1
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

    pub fn matches(&self, source: &SplitSource, target_cube: &str, context: &Context) -> bool {
        self.source_digest == source.digest()
            && self.target_cube == target_cube
            && self.account == context.account
            && self.provider == context.provider
            && self.source().is_ok_and(|recorded| recorded == *source)
    }
}

/// Save the exact three addresses before showing deposit instructions. Once
/// saved, a new scan cannot choose new indices for this source/target pair.
/// A caller must still recheck all addresses' history on both chains before
/// displaying them, and must recollect funding proofs before a spend.
pub fn reserve_split_funding(
    directory: &Path,
    source: &SplitSource,
    target_cube: &str,
    context: &Context,
    inventory: &SplitInventory,
) -> Result<SplitFundingReservation, Error> {
    let indices = inventory
        .fresh_funding_addresses()
        .ok_or(Error::InvalidPlan)?;
    let requested = SplitFundingReservation::new(source, target_cube, context, indices)?;
    super::prepare_directory(directory)?;
    let mut journal = Journal::open(directory)?;
    if journal.load()?.is_some()
        || std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok()
    {
        return Err(Error::Conflict);
    }
    if let Some(saved) = journal.load_split_funding_reservation()? {
        return if saved == requested {
            Ok(saved)
        } else {
            Err(Error::Conflict)
        };
    }
    journal.store_split_funding_reservation(&requested)?;
    Ok(requested)
}

/// Recover the immutable address selection after restart. The caller must
/// match the saved account/provider and source before making any address
/// visible, then check current two-chain address history again.
pub fn load_split_funding(
    directory: &Path,
    source: &SplitSource,
    target_cube: &str,
    context: &Context,
) -> Result<Option<SplitFundingReservation>, Error> {
    let journal = Journal::open(directory)?;
    if std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok() {
        return Err(Error::Conflict);
    }
    let saved = journal.load_split_funding_reservation()?;
    match saved {
        Some(saved) if saved.matches(source, target_cube, context) => Ok(Some(saved)),
        Some(_) => Err(Error::WrongIdentity),
        None => Ok(None),
    }
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
        assert_eq!(saved.addresses().unwrap().len(), 3);
        let mut restarted = context();
        restarted.generation += 1;
        assert_eq!(
            load_split_funding(&directory, &source, "cube-a", &restarted)
                .unwrap()
                .unwrap(),
            saved
        );
        assert_eq!(
            reserve_split_funding(&directory, &source, "cube-a", &restarted, &inventory(3))
                .unwrap(),
            saved
        );
        assert!(matches!(
            reserve_split_funding(&directory, &source, "cube-a", &context(), &inventory(4)),
            Err(Error::Conflict)
        ));
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
}
