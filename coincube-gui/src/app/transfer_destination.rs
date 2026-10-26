//! Private admission and receive-result boundary for a future cross-Cube
//! Transfer. This grants no source signing or broadcast authority.
//!
//! The source remains the only public `App`. A destination loader may give this
//! owner one verified temporary credential and resources it created, but must
//! never call `session::open` or replace the source signer. Every asynchronous
//! receive reply must carry its [`ReceiveRequest`]; a cancelled or replaced
//! operation cannot admit a reply from its predecessor.

use std::str::FromStr;

use coincube_core::miniscript::bitcoin::{Address, Network};
use zeroize::Zeroizing;

use super::{cache::AppGeneration, settings::WalletId};
use crate::chain::ChainId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    pub cube_id: String,
    pub chain: ChainId,
    pub wallet: WalletIdentity,
    pub app: AppGeneration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WalletIdentity {
    Vault(WalletId),
    /// A backend identity, checked after its restricted loader has opened.
    /// Cube metadata alone is insufficient to identify a Liquid/Spark wallet.
    Liquid(String),
    Spark(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Destination {
    pub cube_id: String,
    pub chain: ChainId,
    pub wallet: WalletIdentity,
}

/// Fresh catalog/backend evidence. The caller must re-read this at admission
/// and immediately before accepting a receive result; a stale selected row is
/// never authority. `ready` means that the selected receive backend is live.
#[derive(Clone, Debug)]
pub(crate) struct DestinationEvidence {
    pub identity: Destination,
    pub ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    SourceChanged,
    SelfCube,
    UnsupportedChain,
    ChainChanged,
    WalletChanged,
    BackendUnavailable,
    UnsupportedWallet,
    InvalidAddress,
    StaleResult,
    InvalidAmount,
}

impl Refusal {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::SourceChanged => "The source Cube changed. Start the transfer again.",
            Self::SelfCube => "Choose another Cube as the destination.",
            Self::UnsupportedChain => "Cross-Cube transfer is available only on Bitcoin mainnet.",
            Self::ChainChanged => "The destination Cube's network changed. Select it again.",
            Self::WalletChanged => "The destination wallet changed. Select it again.",
            Self::BackendUnavailable => "The destination wallet is not ready to receive.",
            Self::UnsupportedWallet => {
                "This destination wallet is not ready for cross-Cube transfer."
            }
            Self::InvalidAddress => "The destination did not return a Bitcoin mainnet address.",
            Self::StaleResult => "This transfer step expired. Start it again.",
            Self::InvalidAmount => "Enter a valid transfer amount.",
        }
    }
}

fn admit(
    source: &Source,
    selected: &Destination,
    current_source: &Source,
    evidence: &DestinationEvidence,
) -> Result<(), Refusal> {
    if source != current_source || source.cube_id.is_empty() {
        return Err(Refusal::SourceChanged);
    }
    if !matches!(&source.wallet, WalletIdentity::Vault(id) if !id.descriptor_checksum.is_empty()) {
        return Err(Refusal::UnsupportedWallet);
    }
    if selected.cube_id.is_empty() || source.cube_id == selected.cube_id {
        return Err(Refusal::SelfCube);
    }
    if source.chain != ChainId::Bitcoin || selected.chain != ChainId::Bitcoin {
        return Err(Refusal::UnsupportedChain);
    }
    if selected.cube_id != evidence.identity.cube_id || selected.chain != evidence.identity.chain {
        return Err(Refusal::ChainChanged);
    }
    if selected.wallet != evidence.identity.wallet {
        return Err(Refusal::WalletChanged);
    }
    if !evidence.ready {
        return Err(Refusal::BackendUnavailable);
    }
    if !matches!(&selected.wallet, WalletIdentity::Vault(id) if !id.descriptor_checksum.is_empty())
    {
        return Err(Refusal::UnsupportedWallet);
    }
    Ok(())
}

/// Only resources created for this operation implement this interface. An
/// external daemon or source-shared node must never be placed in this slot.
pub(crate) trait OwnedDestinationResource: Send {
    fn close(self: Box<Self>);
}

/// A temporary, verified destination unlock result. The verifier/duress and
/// passkey policy remain in their existing services; constructing this value
/// is reserved for a future restricted loader after successful verification.
pub(crate) struct TemporaryCredential(Zeroizing<Vec<u8>>);

impl TemporaryCredential {
    pub(crate) fn verified(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReceiveRequest {
    source: Source,
    destination: Destination,
    operation: AppGeneration,
    preparation: AppGeneration,
    amount_sat: u64,
}

impl ReceiveRequest {
    pub(crate) fn destination(&self) -> &Destination {
        &self.destination
    }
}

/// A checked receive address, bound to one source preparation. No invoice or
/// Lightning route is admitted by this first contract; those need separate
/// amount, expiry and payment-hash checks before exposure.
pub(crate) struct ReceiveArtifact {
    pub(crate) request: ReceiveRequest,
    pub(crate) address: Address,
}

pub(crate) struct TransferDestinationOwner {
    source: Source,
    destination: Destination,
    operation: AppGeneration,
    credential: TemporaryCredential,
    owned: Option<Box<dyn OwnedDestinationResource>>,
    active: bool,
}

impl TransferDestinationOwner {
    pub(crate) fn admit(
        source: Source,
        selected: Destination,
        current_source: &Source,
        evidence: &DestinationEvidence,
        credential: TemporaryCredential,
        owned: Option<Box<dyn OwnedDestinationResource>>,
    ) -> Result<Self, Refusal> {
        // Failed admission still drops and zeroizes the credential and closes
        // only a resource explicitly passed as owned.
        if let Err(error) = admit(&source, &selected, current_source, evidence) {
            if let Some(resource) = owned {
                resource.close();
            }
            return Err(error);
        }
        Ok(Self {
            source,
            destination: selected,
            operation: AppGeneration::next(),
            credential,
            owned,
            active: true,
        })
    }

    pub(crate) fn request(
        &self,
        preparation: AppGeneration,
        amount_sat: u64,
    ) -> Result<ReceiveRequest, Refusal> {
        if !self.active {
            return Err(Refusal::StaleResult);
        }
        if amount_sat == 0 {
            return Err(Refusal::InvalidAmount);
        }
        Ok(ReceiveRequest {
            source: self.source.clone(),
            destination: self.destination.clone(),
            operation: self.operation,
            preparation,
            amount_sat,
        })
    }

    /// Validate a backend result against the newest source and destination
    /// evidence before any quote can consume it. The caller must also compare
    /// `preparation` with its current amount/route/recipient generation.
    pub(crate) fn accept_onchain(
        &self,
        request: ReceiveRequest,
        current_source: &Source,
        evidence: &DestinationEvidence,
        current_preparation: AppGeneration,
        address: &str,
    ) -> Result<ReceiveArtifact, Refusal> {
        if !self.active
            || request.operation != self.operation
            || request.source != self.source
            || request.destination != self.destination
            || request.preparation != current_preparation
        {
            return Err(Refusal::StaleResult);
        }
        admit(&self.source, &self.destination, current_source, evidence)?;
        let address = Address::from_str(address)
            .map_err(|_| Refusal::InvalidAddress)?
            .require_network(Network::Bitcoin)
            .map_err(|_| Refusal::InvalidAddress)?;
        Ok(ReceiveArtifact { request, address })
    }

    /// Cancel/back/lock/idle/duress/sign-out/Cube-switch/App teardown all use
    /// this barrier. The drop fallback makes an unplanned replacement safe.
    pub(crate) fn revoke(&mut self) {
        self.active = false;
        if let Some(resource) = self.owned.take() {
            resource.close();
        }
        // Replacing the credential zeroizes its old bytes immediately.
        self.credential = TemporaryCredential::verified(Vec::new());
    }
}

impl Drop for TransferDestinationOwner {
    fn drop(&mut self) {
        self.revoke();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct Resource(Arc<AtomicUsize>);
    impl OwnedDestinationResource for Resource {
        fn close(self: Box<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn fixture() -> (Source, Destination, DestinationEvidence) {
        let source = Source {
            cube_id: "synthetic-a".into(),
            chain: ChainId::Bitcoin,
            wallet: WalletIdentity::Vault(WalletId {
                timestamp: Some(2),
                descriptor_checksum: "source-synthetic".into(),
            }),
            app: AppGeneration::next(),
        };
        let destination = Destination {
            cube_id: "synthetic-b".into(),
            chain: ChainId::Bitcoin,
            wallet: WalletIdentity::Vault(WalletId {
                timestamp: Some(1),
                descriptor_checksum: "synthetic-checksum".into(),
            }),
        };
        let evidence = DestinationEvidence {
            identity: destination.clone(),
            ready: true,
        };
        (source, destination, evidence)
    }

    fn owner(count: Arc<AtomicUsize>) -> (TransferDestinationOwner, Source, DestinationEvidence) {
        let (source, destination, evidence) = fixture();
        let owner = TransferDestinationOwner::admit(
            source.clone(),
            destination,
            &source,
            &evidence,
            TemporaryCredential::verified(vec![1, 2, 3]),
            Some(Box::new(Resource(count))),
        )
        .unwrap();
        (owner, source, evidence)
    }

    #[test]
    fn admission_fails_closed_and_closes_owned_resource() {
        let source = fixture().0;
        let cases = [
            Refusal::SelfCube,
            Refusal::UnsupportedChain,
            Refusal::ChainChanged,
            Refusal::WalletChanged,
            Refusal::BackendUnavailable,
            Refusal::UnsupportedWallet,
            Refusal::SourceChanged,
        ];
        for expected in cases {
            let mut current_source = source.clone();
            let (_, mut destination, mut evidence) = fixture();
            match expected {
                Refusal::SelfCube => destination.cube_id = source.cube_id.clone(),
                Refusal::UnsupportedChain => destination.chain = ChainId::BitcoinBlake2b,
                Refusal::ChainChanged => evidence.identity.chain = ChainId::Testnet4,
                Refusal::WalletChanged => {
                    evidence.identity.wallet = WalletIdentity::Spark("other".into())
                }
                Refusal::BackendUnavailable => evidence.ready = false,
                Refusal::UnsupportedWallet => {
                    destination.wallet = WalletIdentity::Liquid("liquid-synthetic".into());
                    evidence.identity = destination.clone();
                }
                Refusal::SourceChanged => current_source.app = AppGeneration::next(),
                _ => unreachable!(),
            }
            let count = Arc::new(AtomicUsize::new(0));
            let result = TransferDestinationOwner::admit(
                source.clone(),
                destination.clone(),
                &current_source,
                &evidence,
                TemporaryCredential::verified(vec![7]),
                Some(Box::new(Resource(count.clone()))),
            );
            assert_eq!(result.err(), Some(expected));
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn stale_receive_and_replacement_cannot_bind() {
        let count = Arc::new(AtomicUsize::new(0));
        let (mut first, source, evidence) = owner(count.clone());
        let prep = AppGeneration::next();
        let request = first.request(prep, 1000).unwrap();
        assert_eq!(first.request(prep, 0).err(), Some(Refusal::InvalidAmount));
        assert_eq!(
            first
                .accept_onchain(
                    request.clone(),
                    &source,
                    &evidence,
                    AppGeneration::next(),
                    "not-an-address"
                )
                .err(),
            Some(Refusal::StaleResult)
        );
        first.revoke();
        first.revoke();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(first.credential.0.is_empty());
        assert_eq!(
            first
                .accept_onchain(request, &source, &evidence, prep, "not-an-address")
                .err(),
            Some(Refusal::StaleResult)
        );
        let (replacement, _, _) = owner(count.clone());
        let old = ReceiveRequest {
            operation: first.operation,
            ..replacement.request(prep, 1000).unwrap()
        };
        assert_eq!(
            replacement
                .accept_onchain(old, &source, &evidence, prep, "not-an-address")
                .err(),
            Some(Refusal::StaleResult)
        );
        drop(replacement);
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn receive_address_is_bound_to_exact_destination_and_mainnet() {
        let count = Arc::new(AtomicUsize::new(0));
        let (owner, source, mut evidence) = owner(count);
        let prep = AppGeneration::next();
        let request = owner.request(prep, 1_000).unwrap();
        let synthetic_key = coincube_core::miniscript::bitcoin::PublicKey::from_str(
            "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
        )
        .unwrap();
        let mainnet = Address::p2pkh(synthetic_key, Network::Bitcoin).to_string();
        let testnet = Address::p2pkh(synthetic_key, Network::Testnet).to_string();
        assert_eq!(
            owner
                .accept_onchain(request.clone(), &source, &evidence, prep, &testnet)
                .err(),
            Some(Refusal::InvalidAddress)
        );
        evidence.identity.wallet = WalletIdentity::Spark("replacement".into());
        assert_eq!(
            owner
                .accept_onchain(request.clone(), &source, &evidence, prep, &mainnet)
                .err(),
            Some(Refusal::WalletChanged)
        );
        evidence.identity = request.destination.clone();
        let accepted = owner
            .accept_onchain(request.clone(), &source, &evidence, prep, &mainnet)
            .unwrap();
        assert_eq!(accepted.request, request);
        assert_eq!(accepted.address.to_string(), mainnet);
    }
}
