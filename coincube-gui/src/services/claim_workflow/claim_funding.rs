//! Immutable pre-deposit reservation for a paired Bitcoin/BTCB2 Vault Claim.
//! It pins address selection and identity across restart; it is never proof
//! of chain exclusivity or a permission to sign.

use super::{journal::Journal, Context, Error, WalletIdentity};
use crate::services::{
    claim_inventory::ClaimInventory,
    claim_observation::{CollectionContext, FailureKind},
    coincube::CoincubeClient,
    foreign_split_inventory::FreshDepositAddresses,
    split_evidence::ConnectEsplora,
};
use async_trait::async_trait;
use coincube_core::{
    descriptors::CoincubeDescriptor,
    miniscript::bitcoin::{
        hashes::{sha256, Hash},
        Address, Network,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

const ADDRESS_CHECK_BUDGET: Duration = Duration::from_secs(20);
const MAX_ADDRESS_AGE_SECONDS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimAddressError {
    InvalidReservation,
    WrongIdentity,
    MissingReservation,
    ChangedSession,
    Used,
    Unavailable,
    Stale,
    Deadline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedClaimFundingAddresses {
    pub bitcoin_deposit: String,
    pub btcb2_deposit: String,
}

#[async_trait]
trait AddressHistory: Send + Sync {
    async fn used(
        &self,
        chain: coincube_core::chain::ChainId,
        address: &str,
    ) -> Result<(bool, i64), FailureKind>;
}

#[async_trait]
impl AddressHistory for ConnectEsplora {
    async fn used(
        &self,
        chain: coincube_core::chain::ChainId,
        address: &str,
    ) -> Result<(bool, i64), FailureKind> {
        let read = self.address_used(chain, address).await?;
        Ok((*read.value(), read.observed_at()))
    }
}

fn current(generation: &watch::Receiver<u64>, expected: u64) -> bool {
    *generation.borrow() == expected && generation.has_changed().is_ok()
}

async fn checked_history(
    source: &dyn AddressHistory,
    addresses: [String; 2],
    generation: &watch::Receiver<u64>,
    expected: u64,
) -> Result<CheckedClaimFundingAddresses, ClaimAddressError> {
    use coincube_core::chain::ChainId;
    if !current(generation, expected) {
        return Err(ClaimAddressError::ChangedSession);
    }
    tokio::time::timeout(ADDRESS_CHECK_BUDGET, async {
        for address in &addresses {
            for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
                let (used, observed_at) = source
                    .used(chain, address)
                    .await
                    .map_err(|_| ClaimAddressError::Unavailable)?;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| ClaimAddressError::Stale)?
                    .as_secs() as i64;
                let age = now
                    .checked_sub(observed_at)
                    .ok_or(ClaimAddressError::Stale)?;
                if !(0..=MAX_ADDRESS_AGE_SECONDS).contains(&age) {
                    return Err(ClaimAddressError::Stale);
                }
                if used {
                    return Err(ClaimAddressError::Used);
                }
                if !current(generation, expected) {
                    return Err(ClaimAddressError::ChangedSession);
                }
            }
        }
        Ok(CheckedClaimFundingAddresses {
            bitcoin_deposit: addresses[0].clone(),
            btcb2_deposit: addresses[1].clone(),
        })
    })
    .await
    .map_err(|_| ClaimAddressError::Deadline)?
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimFundingReservation {
    version: u32,
    identity: WalletIdentity,
    account: String,
    provider: String,
    descriptor: String,
    pub indices: FreshDepositAddresses,
}

impl ClaimFundingReservation {
    pub(super) fn new(
        descriptor: &CoincubeDescriptor,
        identity: &WalletIdentity,
        context: &Context,
        indices: FreshDepositAddresses,
    ) -> Result<Self, Error> {
        let saved = Self {
            version: 1,
            identity: identity.clone(),
            account: context.account.clone(),
            provider: context.provider.clone(),
            descriptor: descriptor.to_string(),
            indices,
        };
        saved.validate()?;
        Ok(saved)
    }

    pub(super) fn validate(&self) -> Result<(), Error> {
        if self.version != 1
            || self.identity.bitcoin_cube.is_empty()
            || self.identity.fork_cube.is_empty()
            || self.identity.bitcoin_cube.len() > 256
            || self.identity.fork_cube.len() > 256
            || self.account.is_empty()
            || self.account.len() > 256
            || self.provider.is_empty()
            || self.provider.len() > 2048
            || self.descriptor.len() > 16_384
            || self.indices.bitcoin_deposit >= 1 << 31
            || self.indices.btcb2_deposit >= 1 << 31
            || self.indices.bitcoin_deposit == self.indices.btcb2_deposit
        {
            return Err(Error::InvalidPlan);
        }
        let descriptor = self.descriptor()?;
        if sha256::Hash::hash(descriptor.to_string().as_bytes()) != self.identity.descriptor_digest
        {
            return Err(Error::InvalidPlan);
        }
        let bitcoin_address = self.address(&descriptor, self.indices.bitcoin_deposit)?;
        let fork_address = self.address(&descriptor, self.indices.btcb2_deposit)?;
        if bitcoin_address == fork_address {
            return Err(Error::InvalidPlan);
        }
        Ok(())
    }

    fn descriptor(&self) -> Result<CoincubeDescriptor, Error> {
        CoincubeDescriptor::from_str(&self.descriptor).map_err(|_| Error::InvalidPlan)
    }

    fn address(&self, descriptor: &CoincubeDescriptor, index: u32) -> Result<String, Error> {
        let script = descriptor
            .receive_descriptor()
            .derive(
                coincube_core::miniscript::bitcoin::bip32::ChildNumber::Normal { index },
                &coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::verification_only(),
            )
            .script_pubkey();
        Address::from_script(&script, Network::Bitcoin)
            .map(|address| address.to_string())
            .map_err(|_| Error::InvalidPlan)
    }

    /// Display only after a fresh, authenticated two-chain address-history
    /// check under the current account confirms both addresses are unused.
    pub fn addresses(&self) -> Result<[String; 2], Error> {
        let descriptor = self.descriptor()?;
        Ok([
            self.address(&descriptor, self.indices.bitcoin_deposit)?,
            self.address(&descriptor, self.indices.btcb2_deposit)?,
        ])
    }

    fn matches(
        &self,
        descriptor: &CoincubeDescriptor,
        identity: &WalletIdentity,
        context: &Context,
    ) -> bool {
        self.identity == *identity
            && self.account == context.account
            && self.provider == context.provider
            && self.descriptor().is_ok_and(|saved| saved == *descriptor)
    }

    pub(super) fn matches_pair(
        &self,
        identity: &WalletIdentity,
        context: &Context,
        bitcoin: &super::ancestry::StoredAncestry,
        fork: &super::ancestry::StoredAncestry,
    ) -> bool {
        self.identity == *identity
            && self.account == context.account
            && self.provider == context.provider
            && bitcoin.derivation.as_ref().is_some_and(|derivation| {
                !derivation.is_change && derivation.index == self.indices.bitcoin_deposit
            })
            && fork.derivation.as_ref().is_some_and(|derivation| {
                !derivation.is_change && derivation.index == self.indices.btcb2_deposit
            })
    }
}

/// Pin both deposit addresses before they can be shown. A later scan cannot
/// silently rotate them. A fresh address-history check and wallet-state
/// reconciliation are separate prerequisites to displaying or spending.
pub fn reserve_claim_funding(
    directory: &Path,
    descriptor: &CoincubeDescriptor,
    identity: &WalletIdentity,
    context: &Context,
    inventory: &ClaimInventory,
) -> Result<ClaimFundingReservation, Error> {
    if inventory.descriptor() != descriptor
        || inventory.scan_report().bitcoin.generation() != context.generation
        || inventory.scan_report().btcb2.generation() != context.generation
    {
        return Err(Error::InvalidPlan);
    }
    let indices = inventory
        .fresh_deposit_addresses()
        .ok_or(Error::InvalidPlan)?;
    let requested = ClaimFundingReservation::new(descriptor, identity, context, indices)?;
    super::prepare_directory(directory)?;
    let mut journal = Journal::open(directory)?;
    if journal.load()?.is_some()
        || journal.load_split_funding_reservation()?.is_some()
        || std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok()
    {
        return Err(Error::Conflict);
    }
    if let Some(saved) = journal.load_claim_funding_reservation()? {
        return if saved == requested {
            Ok(saved)
        } else {
            Err(Error::Conflict)
        };
    }
    journal.store_claim_funding_reservation(&requested)?;
    Ok(requested)
}

/// Reload the same addresses under the exact Vault and Connect identity.
/// This does not assert that either address is still unused.
pub fn load_claim_funding(
    directory: &Path,
    descriptor: &CoincubeDescriptor,
    identity: &WalletIdentity,
    context: &Context,
) -> Result<Option<ClaimFundingReservation>, Error> {
    let journal = Journal::open(directory)?;
    if journal.load_split_funding_reservation()?.is_some()
        || std::fs::symlink_metadata(directory.join(super::SPLIT_TOMBSTONE)).is_ok()
    {
        return Err(Error::Conflict);
    }
    match journal.load_claim_funding_reservation()? {
        Some(saved) if saved.matches(descriptor, identity, context) => Ok(Some(saved)),
        Some(_) => Err(Error::WrongIdentity),
        None => Ok(None),
    }
}

/// Release saved deposit addresses for display only after authenticated
/// identity and fresh Bitcoin/BTCB2 address histories still show them unused.
/// A successful result is not a proof that either later deposit is exclusive.
#[allow(clippy::too_many_arguments)]
pub async fn check_reserved_claim_addresses(
    directory: PathBuf,
    descriptor: CoincubeDescriptor,
    identity: WalletIdentity,
    context: Context,
    client: CoincubeClient,
    generation: watch::Receiver<u64>,
) -> Result<CheckedClaimFundingAddresses, ClaimAddressError> {
    if !current(&generation, context.generation)
        || client.token().is_none_or(str::is_empty)
        || client.base_url.trim_end_matches('/') != context.provider.trim_end_matches('/')
    {
        return Err(ClaimAddressError::ChangedSession);
    }
    let expected = context.generation;
    let saved = tokio::task::spawn_blocking(move || {
        load_claim_funding(&directory, &descriptor, &identity, &context)
    })
    .await
    .map_err(|_| ClaimAddressError::Unavailable)?
    .map_err(|error| match error {
        Error::WrongIdentity => ClaimAddressError::WrongIdentity,
        _ => ClaimAddressError::InvalidReservation,
    })?
    .ok_or(ClaimAddressError::MissingReservation)?;
    let addresses = saved
        .addresses()
        .map_err(|_| ClaimAddressError::InvalidReservation)?;
    let esplora = ConnectEsplora::new(
        &client,
        CollectionContext {
            expected_generation: expected,
            generation: generation.clone(),
        },
    )
    .map_err(|_| ClaimAddressError::Unavailable)?;
    checked_history(&esplora, addresses, &generation, expected).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::{
        bip39::Mnemonic,
        chain::ChainId,
        descriptors::{CoincubePolicy, PathInfo},
        miniscript::{
            bitcoin::{bip32::DerivationPath, secp256k1, Network},
            DescriptorPublicKey,
        },
        signer::MasterSigner,
    };

    struct FakeHistory {
        stamp: i64,
        used_on: Option<ChainId>,
        calls: std::sync::Mutex<Vec<(ChainId, String)>>,
    }

    #[async_trait]
    impl AddressHistory for FakeHistory {
        async fn used(&self, chain: ChainId, address: &str) -> Result<(bool, i64), FailureKind> {
            self.calls.lock().unwrap().push((chain, address.into()));
            Ok((self.used_on == Some(chain), self.stamp))
        }
    }

    #[tokio::test]
    async fn deposit_addresses_require_fresh_unused_history_on_both_chains() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let (sender, generation) = watch::channel(7);
        let make = |stamp, used_on| FakeHistory {
            stamp,
            used_on,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let addresses = || ["bitcoin-address".into(), "btcb2-address".into()];
        let clean = make(now, None);
        let checked = checked_history(&clean, addresses(), &generation, 7)
            .await
            .unwrap();
        assert_eq!(checked.bitcoin_deposit, "bitcoin-address");
        assert_eq!(checked.btcb2_deposit, "btcb2-address");
        assert_eq!(clean.calls.lock().unwrap().len(), 4);
        assert_eq!(
            checked_history(
                &make(now, Some(ChainId::BitcoinBlake2b)),
                addresses(),
                &generation,
                7
            )
            .await,
            Err(ClaimAddressError::Used)
        );
        assert_eq!(
            checked_history(&make(now - 61, None), addresses(), &generation, 7).await,
            Err(ClaimAddressError::Stale)
        );
        sender.send(8).unwrap();
        assert_eq!(
            checked_history(&clean, addresses(), &generation, 7).await,
            Err(ClaimAddressError::ChangedSession)
        );
    }

    fn descriptor() -> CoincubeDescriptor {
        let secp = secp256k1::Secp256k1::new();
        let keys: Vec<_> = [61_u8, 62]
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
    fn claim_deposit_reservation_survives_restart_and_rejects_tampering() {
        let directory = std::env::temp_dir().join(format!(
            "claim-funding-{}-{}",
            std::process::id(),
            super::super::controller_id().unwrap()
        ));
        let descriptor = descriptor();
        let identity = WalletIdentity {
            bitcoin_cube: "bitcoin-cube".into(),
            fork_cube: "btcb2-cube".into(),
            descriptor_digest: sha256::Hash::hash(descriptor.to_string().as_bytes()),
        };
        let context = Context {
            generation: 1,
            account: "account-a".into(),
            provider: "https://connect.example".into(),
        };
        let indices = FreshDepositAddresses {
            bitcoin_deposit: 4,
            btcb2_deposit: 5,
        };
        let saved =
            ClaimFundingReservation::new(&descriptor, &identity, &context, indices).unwrap();
        let addresses = saved.addresses().unwrap();
        assert_ne!(addresses[0], addresses[1]);
        super::super::prepare_directory(&directory).unwrap();
        {
            let mut journal = Journal::open(&directory).unwrap();
            journal.store_claim_funding_reservation(&saved).unwrap();
        }
        let mut restarted = context.clone();
        restarted.generation += 1;
        assert_eq!(
            load_claim_funding(&directory, &descriptor, &identity, &restarted)
                .unwrap()
                .unwrap(),
            saved
        );
        let mut other = identity.clone();
        other.fork_cube = "other-cube".into();
        assert!(matches!(
            load_claim_funding(&directory, &descriptor, &other, &context),
            Err(Error::WrongIdentity)
        ));
        let mut bad = saved.clone();
        bad.indices.btcb2_deposit = bad.indices.bitcoin_deposit;
        assert!(matches!(bad.validate(), Err(Error::InvalidPlan)));
        std::fs::write(directory.join("claim-funding.json"), b"{}").unwrap();
        assert!(matches!(
            load_claim_funding(&directory, &descriptor, &identity, &context),
            Err(Error::InvalidJournal)
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
