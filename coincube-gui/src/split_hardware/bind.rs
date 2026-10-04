//! The production [`PolicyBinder`] for Split device signing (#568 Track B,
//! slice B4b-2; owner decisions D7 and P6; default C5).
//!
//! B4a's [`SigningSession`](super::sign::SigningSession) reaches some device
//! classes only through a handle bound to the policy: Coldcard (the policy
//! name) and BitBox02 (the policy) for multisig, and Ledger for every shape
//! (its unnamed default policy for singlesig; the registered policy and the
//! session HMAC for multisig). This binder supplies those handles:
//!
//! - Coldcard and BitBox02: a listing opened with
//!   [`HardwareWallets::with_split_policy`] binds the policy into the handle
//!   when the device is opened, so the binder returns that listed handle,
//!   after checking that the listing was bound to the requested policy and
//!   lists the requested device (kind and master fingerprint).
//! - Ledger: `with_wallet` consumes a concrete handle, and the HMAC exists
//!   only after this session's registration, so the binder opens the device
//!   again through a [`LedgerOpener`] (HID enumeration, next to the open
//!   handle the listing holds, plus the Speculos simulator), selects the
//!   candidate of the requested kind that reports the requested master
//!   fingerprint, and binds it with the name, the policy and the HMAC. The
//!   session rechecks the fingerprint and the registration on the handle it
//!   gets back (B4a).
//!
//! The HMAC is copied into the bound async-hwi handle, which does not
//! zeroize it (B4a note). That copy lives only as long as the handle, which
//! `SigningSession::sign` drops when it returns. Nothing is persisted.
//!
//! Nothing here is reachable from the GUI (D1). The tests use fake devices
//! and a fake opener; real transports, including a second HID open of a
//! listed Ledger, need device QA before go-live (#600).

use std::sync::Arc;

use async_hwi::{ledger, DeviceKind, Error, HWI};
use coincube_core::miniscript::bitcoin::bip32::Fingerprint;
use tracing::debug;

use super::sign::{BindRequest, PolicyBinder};
use crate::hw::{HardwareWallet, HardwareWallets, SplitPolicyBinding};

/// The listing was opened for another policy than the one being signed.
pub const LISTED_FOR_ANOTHER_WALLET: &str = "the device list was opened for another wallet";

/// An unbound Ledger handle the binder can select and bind.
#[async_trait::async_trait]
pub trait LedgerCandidate: Send + Sync {
    fn kind(&self) -> DeviceKind;

    async fn fingerprint(&self) -> Result<Fingerprint, Error>;

    /// Binds the policy, and the session HMAC when there is one, into the
    /// handle.
    fn bind(
        self: Box<Self>,
        name: &str,
        policy: &str,
        hmac: Option<[u8; 32]>,
    ) -> Result<Arc<dyn HWI + Send + Sync>, Error>;
}

#[async_trait::async_trait]
impl<T: ledger::Transport + Send + Sync + 'static> LedgerCandidate for ledger::Ledger<T> {
    fn kind(&self) -> DeviceKind {
        self.device_kind()
    }

    async fn fingerprint(&self) -> Result<Fingerprint, Error> {
        self.get_master_fingerprint().await
    }

    fn bind(
        self: Box<Self>,
        name: &str,
        policy: &str,
        hmac: Option<[u8; 32]>,
    ) -> Result<Arc<dyn HWI + Send + Sync>, Error> {
        Ok(Arc::new((*self).with_wallet(name, policy, hmac)?))
    }
}

/// Opens every connected Ledger as an unbound candidate.
#[async_trait::async_trait]
pub trait LedgerOpener: Send + Sync {
    async fn candidates(&self) -> Result<Vec<Box<dyn LedgerCandidate>>, Error>;
}

/// The production opener: a fresh HID enumeration (hidapi allows several
/// contexts, and its default open is shared, so the listing's own handle
/// stays open) plus the Speculos simulator.
pub struct HidLedgerOpener;

#[async_trait::async_trait]
impl LedgerOpener for HidLedgerOpener {
    async fn candidates(&self) -> Result<Vec<Box<dyn LedgerCandidate>>, Error> {
        let mut candidates: Vec<Box<dyn LedgerCandidate>> = Vec::new();
        let api = ledger::HidApi::new().map_err(|error| Error::Device(error.to_string()))?;
        for info in ledger::Ledger::<ledger::TransportHID>::enumerate(&api) {
            match ledger::Ledger::<ledger::TransportHID>::connect(&api, info) {
                Ok(device) => candidates.push(Box::new(device)),
                Err(error) => debug!(
                    "split: ledger {:?} not opened for binding: {}",
                    info.path(),
                    error
                ),
            }
        }
        drop(api);
        if let Ok(simulator) = ledger::LedgerSimulator::try_connect().await {
            candidates.push(Box::new(simulator));
        }
        Ok(candidates)
    }
}

struct Listed {
    kind: DeviceKind,
    fingerprint: Fingerprint,
    device: Arc<dyn HWI + Send + Sync>,
}

/// The binder for a Split device listing: a snapshot of its supported
/// devices and its policy binding, plus the opener for Ledger.
pub struct ProductionBinder {
    binding: Option<SplitPolicyBinding>,
    listed: Vec<Listed>,
    opener: Arc<dyn LedgerOpener>,
}

impl ProductionBinder {
    pub fn from_listing(listing: &HardwareWallets) -> Self {
        Self::with_opener(listing, Arc::new(HidLedgerOpener))
    }

    pub fn with_opener(listing: &HardwareWallets, opener: Arc<dyn LedgerOpener>) -> Self {
        let listed = listing
            .list
            .iter()
            .filter_map(|hw| match hw {
                HardwareWallet::Supported {
                    kind,
                    fingerprint,
                    device,
                    ..
                } => Some(Listed {
                    kind: *kind,
                    fingerprint: *fingerprint,
                    device: device.clone(),
                }),
                _ => None,
            })
            .collect();
        Self {
            binding: listing.split_policy_binding().cloned(),
            listed,
            opener,
        }
    }

    /// The listed handle of this device, bound at listing time.
    fn listed(&self, request: &BindRequest<'_>) -> Result<Arc<dyn HWI + Send + Sync>, Error> {
        let binding = self.binding.as_ref().ok_or(Error::UnimplementedMethod)?;
        if binding.bound() != Some((request.name, request.policy)) {
            return Err(Error::Device(LISTED_FOR_ANOTHER_WALLET.into()));
        }
        self.listed
            .iter()
            .find(|listed| listed.kind == request.kind && listed.fingerprint == request.fingerprint)
            .map(|listed| listed.device.clone())
            .ok_or(Error::DeviceNotFound)
    }

    /// A fresh handle to the one connected Ledger of the requested kind that
    /// reports the requested fingerprint, bound to the policy and token.
    async fn ledger(&self, request: BindRequest<'_>) -> Result<Arc<dyn HWI + Send + Sync>, Error> {
        let mut first_error = None;
        for candidate in self.opener.candidates().await? {
            if candidate.kind() != request.kind {
                continue;
            }
            match candidate.fingerprint().await {
                Ok(fingerprint) if fingerprint == request.fingerprint => {
                    return candidate.bind(request.name, request.policy, request.hmac.copied());
                }
                Ok(_) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.unwrap_or(Error::DeviceNotFound))
    }
}

#[async_trait::async_trait]
impl PolicyBinder for ProductionBinder {
    fn can_bind(&self, kind: DeviceKind) -> bool {
        match kind {
            DeviceKind::Ledger | DeviceKind::LedgerSimulator => true,
            DeviceKind::Coldcard | DeviceKind::BitBox02 => self
                .binding
                .as_ref()
                .is_some_and(|binding| binding.bound().is_some()),
            DeviceKind::Jade | DeviceKind::Specter | DeviceKind::SpecterSimulator => false,
        }
    }

    async fn bind(&self, request: BindRequest<'_>) -> Result<Arc<dyn HWI + Send + Sync>, Error> {
        match request.kind {
            DeviceKind::Ledger | DeviceKind::LedgerSimulator => self.ledger(request).await,
            DeviceKind::Coldcard | DeviceKind::BitBox02 => self.listed(&request),
            DeviceKind::Jade | DeviceKind::Specter | DeviceKind::SpecterSimulator => {
                Err(Error::UnimplementedMethod)
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{
        path::Path,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Mutex,
        },
    };

    use coincube_core::{
        chain::ChainId,
        foreign_split::{create_split_step1, SplitInputs, SplitStep1},
        miniscript::bitcoin::{
            absolute::LockTime,
            bip32::{DerivationPath, Xpriv, Xpub},
            hashes::{sha256, Hash},
            secp256k1::Secp256k1,
            BlockHash, Network, Psbt,
        },
    };

    use super::*;
    use crate::{
        dir::CoincubeDirectory,
        services::{
            foreign_split_inventory::FreshIndex,
            split_source::split_source,
            split_test_wallets::{self as fixture, Shape},
        },
        split_hardware::{
            policy::{split_descriptor, split_policy},
            sign::{RegistrationOutcome, SignError, SigningHandle, SigningSession},
        },
    };

    pub(crate) fn master(seed: u8) -> Xpriv {
        fixture::master(seed)
    }

    pub(crate) fn fp(seed: u8) -> Fingerprint {
        master(seed).fingerprint(&Secp256k1::new())
    }

    pub(crate) fn token(name: &str, policy: &str) -> [u8; 32] {
        sha256::Hash::hash(format!("hmac|{name}|{policy}").as_bytes()).to_byte_array()
    }

    #[derive(Debug, Default)]
    pub(crate) struct Shared {
        pub(crate) stored: Mutex<Vec<(String, String)>>,
        pub(crate) register_calls: AtomicUsize,
        pub(crate) sign_calls: AtomicUsize,
        pub(crate) bind_calls: AtomicUsize,
        /// The name and token of every bind of this device.
        pub(crate) bound_with: Mutex<Vec<(String, Option<[u8; 32]>)>>,
    }

    /// A device that really signs with `master(seed)`. Ledger kinds return a
    /// token on registration and sign only through a bound handle; the
    /// others store the registration and sign on any handle.
    #[derive(Debug, Clone)]
    pub(crate) struct Fake {
        pub(crate) kind: DeviceKind,
        pub(crate) seed: u8,
        pub(crate) shared: Arc<Shared>,
        bound: Option<(String, String, Option<[u8; 32]>)>,
    }

    pub(crate) fn fake(kind: DeviceKind, seed: u8) -> Fake {
        Fake {
            kind,
            seed,
            shared: Arc::default(),
            bound: None,
        }
    }

    impl Fake {
        pub(crate) fn arc(&self) -> Arc<dyn HWI + Send + Sync> {
            Arc::new(self.clone())
        }

        fn token_device(&self) -> bool {
            matches!(self.kind, DeviceKind::Ledger | DeviceKind::LedgerSimulator)
        }

        fn registered(&self, name: &str, policy: &str) -> bool {
            if self.token_device() {
                self.bound.as_ref().is_some_and(|(n, p, h)| {
                    n == name && p == policy && *h == Some(token(name, policy))
                })
            } else {
                self.shared
                    .stored
                    .lock()
                    .unwrap()
                    .contains(&(name.to_string(), policy.to_string()))
            }
        }
    }

    #[async_trait::async_trait]
    impl HWI for Fake {
        fn device_kind(&self) -> DeviceKind {
            self.kind
        }
        async fn get_version(&self) -> Result<async_hwi::Version, Error> {
            Err(Error::UnimplementedMethod)
        }
        async fn get_master_fingerprint(&self) -> Result<Fingerprint, Error> {
            Ok(fp(self.seed))
        }
        async fn get_extended_pubkey(&self, _: &DerivationPath) -> Result<Xpub, Error> {
            Err(Error::UnimplementedMethod)
        }
        async fn register_wallet(
            &self,
            name: &str,
            policy: &str,
        ) -> Result<Option<[u8; 32]>, Error> {
            self.shared.register_calls.fetch_add(1, Ordering::SeqCst);
            if self.token_device() {
                return Ok(Some(token(name, policy)));
            }
            self.shared
                .stored
                .lock()
                .unwrap()
                .push((name.to_string(), policy.to_string()));
            Ok(None)
        }
        async fn is_wallet_registered(&self, name: &str, policy: &str) -> Result<bool, Error> {
            Ok(self.registered(name, policy))
        }
        async fn display_address(&self, _: &async_hwi::AddressScript) -> Result<(), Error> {
            Err(Error::UnimplementedMethod)
        }
        async fn sign_tx(&self, psbt: &mut Psbt) -> Result<(), Error> {
            self.shared.sign_calls.fetch_add(1, Ordering::SeqCst);
            if self.token_device() && self.bound.is_none() {
                // Ledger cannot sign without a policy.
                return Err(Error::UnimplementedMethod);
            }
            let multisig = psbt.inputs.iter().any(|i| i.witness_script.is_some());
            if multisig {
                let ok = match &self.bound {
                    Some((n, p, _)) if self.token_device() => self.registered(n, p),
                    _ => !self.shared.stored.lock().unwrap().is_empty(),
                };
                if !ok {
                    return Err(Error::Device("policy not registered".into()));
                }
            }
            let _ = psbt.sign(&master(self.seed), &Secp256k1::new());
            Ok(())
        }
    }

    /// A connected but unbound Ledger, as the opener hands it out.
    pub(crate) struct FakeCandidate(Fake);

    #[async_trait::async_trait]
    impl LedgerCandidate for FakeCandidate {
        fn kind(&self) -> DeviceKind {
            self.0.kind
        }
        async fn fingerprint(&self) -> Result<Fingerprint, Error> {
            Ok(fp(self.0.seed))
        }
        fn bind(
            self: Box<Self>,
            name: &str,
            policy: &str,
            hmac: Option<[u8; 32]>,
        ) -> Result<Arc<dyn HWI + Send + Sync>, Error> {
            let mut device = self.0;
            device.shared.bind_calls.fetch_add(1, Ordering::SeqCst);
            device
                .shared
                .bound_with
                .lock()
                .unwrap()
                .push((name.to_string(), hmac));
            device.bound = Some((name.to_string(), policy.to_string(), hmac));
            Ok(Arc::new(device))
        }
    }

    /// Hands out its devices as candidates, in order, on every open.
    #[derive(Default)]
    pub(crate) struct FakeOpener {
        devices: Vec<Fake>,
        pub(crate) opens: AtomicUsize,
    }

    impl FakeOpener {
        pub(crate) fn new(devices: Vec<Fake>) -> Arc<Self> {
            Arc::new(Self {
                devices,
                opens: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait::async_trait]
    impl LedgerOpener for FakeOpener {
        async fn candidates(&self) -> Result<Vec<Box<dyn LedgerCandidate>>, Error> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .devices
                .iter()
                .cloned()
                .map(|device| Box::new(FakeCandidate(device)) as Box<dyn LedgerCandidate>)
                .collect())
        }
    }

    /// A step-1 construction over the fixture wallet of `shape`: two coins,
    /// signable by `master(1)` (and `master(2)` for the 2-of-3 shapes).
    pub(crate) fn construction(shape: Shape) -> SplitStep1 {
        let wallet = fixture::wallet(shape);
        let inventory = fixture::inventory(&wallet);
        let coins = inventory.splittable_coins();
        let source = split_source(&wallet.external, Some(&wallet.internal)).unwrap();
        let FreshIndex::Proven(destination) = inventory.fresh_receive() else {
            panic!("fixture has a fresh index");
        };
        let tip = inventory.bitcoin_tip_height();
        create_split_step1(
            &SplitInputs {
                chain: ChainId::Bitcoin,
                source: &source,
                coins: &coins,
                fork_height: inventory.fork_height(),
                destination,
            },
            2,
            LockTime::from_height(tip).unwrap(),
            tip,
            BlockHash::from_byte_array([7; 32]),
        )
        .unwrap()
    }

    /// An ephemeral listing rooted at `root`, bound to `policy` when given,
    /// listing `devices` as supported.
    pub(crate) fn listing(
        root: &Path,
        policy: Option<(&str, &str)>,
        devices: &[(&str, &Fake)],
    ) -> HardwareWallets {
        let base =
            HardwareWallets::new(CoincubeDirectory::new(root.to_path_buf()), Network::Bitcoin);
        let mut listing = match policy {
            Some((name, descriptor)) => {
                base.with_split_policy(name.to_string(), descriptor.to_string())
            }
            None => base.ephemeral(),
        };
        for (id, device) in devices {
            listing.list.push(HardwareWallet::Supported {
                id: id.to_string(),
                device: device.arc(),
                kind: device.kind,
                fingerprint: fp(device.seed),
                version: None,
                registered: None,
                alias: None,
            });
        }
        listing
    }

    pub(crate) fn temp_root() -> std::path::PathBuf {
        crate::split_hardware::tests::unique_temp_root("coincube-split-b4b2")
    }

    pub(crate) fn sig_count(psbt: &Psbt) -> usize {
        psbt.inputs.iter().map(|i| i.partial_sigs.len()).sum()
    }

    fn request<'a>(
        kind: DeviceKind,
        seed: u8,
        name: &'a str,
        policy: &'a str,
        hmac: Option<&'a [u8; 32]>,
    ) -> BindRequest<'a> {
        BindRequest {
            kind,
            fingerprint: fp(seed),
            name,
            policy,
            hmac,
        }
    }

    #[tokio::test]
    async fn binder_returns_listed_bound_handles_for_coldcard_and_bitbox() {
        let step1 = construction(Shape::WshSortedMulti);
        let inputs = step1.psbt().inputs.len();
        let policy = split_policy(step1.source()).unwrap();
        let descriptor = split_descriptor(step1.source()).unwrap().to_string();
        let root = temp_root();
        let coldcard = fake(DeviceKind::Coldcard, 1);
        let bitbox = fake(DeviceKind::BitBox02, 2);
        let listing = listing(
            &root,
            Some((policy.name(), &descriptor)),
            &[("coldcard-1", &coldcard), ("bitbox-1", &bitbox)],
        );
        let binder = Arc::new(ProductionBinder::with_opener(
            &listing,
            FakeOpener::new(Vec::new()),
        ));
        for kind in [
            DeviceKind::Coldcard,
            DeviceKind::BitBox02,
            DeviceKind::Ledger,
            DeviceKind::LedgerSimulator,
        ] {
            assert!(binder.can_bind(kind), "{}", kind);
        }
        for kind in [
            DeviceKind::Jade,
            DeviceKind::Specter,
            DeviceKind::SpecterSimulator,
        ] {
            assert!(!binder.can_bind(kind), "{}", kind);
        }

        // The bound handle is the listed handle itself: bound at listing.
        for (hw, kind, seed) in [
            (&listing.list[0], DeviceKind::Coldcard, 1),
            (&listing.list[1], DeviceKind::BitBox02, 2),
        ] {
            let HardwareWallet::Supported { device, .. } = hw else {
                panic!("listed as supported");
            };
            let bound = binder
                .bind(request(kind, seed, policy.name(), &descriptor, None))
                .await
                .unwrap();
            assert!(Arc::ptr_eq(&bound, device), "{}", kind);
        }

        // Full sessions on both: register, then sign; two signatures per
        // input satisfy the 2-of-3.
        let mut psbt = step1.psbt().clone();
        for hw in &listing.list {
            let mut session = SigningSession::from_listed(hw, policy.clone(), binder.clone())
                .await
                .unwrap();
            assert_eq!(session.handle(), SigningHandle::Bound);
            assert_eq!(
                session.register().await.unwrap(),
                RegistrationOutcome::Registered
            );
            psbt = session.sign(&psbt).await.unwrap().into_psbt();
        }
        assert_eq!(sig_count(&psbt), 2 * inputs);
        assert_eq!(coldcard.shared.register_calls.load(Ordering::SeqCst), 1);
        assert_eq!(bitbox.shared.register_calls.load(Ordering::SeqCst), 1);

        // A device the listing does not hold, and another policy than the
        // listing was opened for.
        assert!(matches!(
            binder
                .bind(request(
                    DeviceKind::Coldcard,
                    9,
                    policy.name(),
                    &descriptor,
                    None
                ))
                .await,
            Err(Error::DeviceNotFound)
        ));
        assert!(matches!(
            binder
                .bind(request(
                    DeviceKind::BitBox02,
                    1,
                    policy.name(),
                    &descriptor,
                    None
                ))
                .await,
            Err(Error::DeviceNotFound)
        ));
        let other = format!("{}x", policy.name());
        assert!(matches!(
            binder
                .bind(request(DeviceKind::Coldcard, 1, &other, &descriptor, None))
                .await,
            Err(Error::Device(message)) if message == LISTED_FOR_ANOTHER_WALLET
        ));

        // A listing opened without the policy cannot bind these devices: the
        // session refuses at open, before any registration prompt (B4a F1).
        let unbound = self::listing(&root, None, &[("coldcard-1", &coldcard)]);
        let binder = Arc::new(ProductionBinder::with_opener(
            &unbound,
            FakeOpener::new(Vec::new()),
        ));
        assert!(!binder.can_bind(DeviceKind::Coldcard));
        assert!(!binder.can_bind(DeviceKind::BitBox02));
        assert_eq!(
            SigningSession::from_listed(&unbound.list[0], policy.clone(), binder)
                .await
                .unwrap_err(),
            SignError::NeedsPolicyBinding(DeviceKind::Coldcard)
        );
        assert_eq!(coldcard.shared.register_calls.load(Ordering::SeqCst), 1);

        // An unnamed (singlesig) binding binds nothing into these devices.
        let single = construction(Shape::Wpkh);
        let single_descriptor = split_descriptor(single.source()).unwrap().to_string();
        let single_listing = self::listing(
            &root,
            Some(("", &single_descriptor)),
            &[("coldcard-1", &coldcard)],
        );
        let binder = ProductionBinder::with_opener(&single_listing, FakeOpener::new(Vec::new()));
        assert!(!binder.can_bind(DeviceKind::Coldcard));
        assert!(!binder.can_bind(DeviceKind::BitBox02));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir(root).unwrap();
    }

    #[tokio::test]
    async fn binder_rebinds_ledger_with_session_hmac_and_rechecks_fingerprint() {
        let step1 = construction(Shape::WshMulti);
        let inputs = step1.psbt().inputs.len();
        let policy = split_policy(step1.source()).unwrap();
        let descriptor = split_descriptor(step1.source()).unwrap().to_string();
        let root = temp_root();
        let ledger = fake(DeviceKind::Ledger, 2);
        let wrong = fake(DeviceKind::Ledger, 9);
        let simulator = fake(DeviceKind::LedgerSimulator, 2);
        // The requested device is enumerated last, behind another Ledger
        // and a simulator with the same fingerprint.
        let opener = FakeOpener::new(vec![wrong.clone(), simulator.clone(), ledger.clone()]);
        let listing = listing(&root, None, &[("ledger-1", &ledger)]);
        let binder = Arc::new(ProductionBinder::with_opener(&listing, opener.clone()));
        let hmac = token(policy.name(), &descriptor);

        let bound = binder
            .bind(request(
                DeviceKind::Ledger,
                2,
                policy.name(),
                &descriptor,
                Some(&hmac),
            ))
            .await
            .unwrap();
        assert_eq!(bound.device_kind(), DeviceKind::Ledger);
        assert_eq!(bound.get_master_fingerprint().await.unwrap(), fp(2));
        // Bound with the session token, it reports the registration.
        assert!(bound
            .is_wallet_registered(policy.name(), &descriptor)
            .await
            .unwrap());
        assert_eq!(
            ledger.shared.bound_with.lock().unwrap().clone(),
            vec![(policy.name().to_string(), Some(hmac))]
        );
        assert_eq!(wrong.shared.bind_calls.load(Ordering::SeqCst), 0);
        assert_eq!(simulator.shared.bind_calls.load(Ordering::SeqCst), 0);
        assert_eq!(opener.opens.load(Ordering::SeqCst), 1);

        // Only other devices connected: refused, nothing bound.
        let others = ProductionBinder::with_opener(
            &listing,
            FakeOpener::new(vec![wrong.clone(), simulator.clone()]),
        );
        assert!(matches!(
            others
                .bind(request(
                    DeviceKind::Ledger,
                    2,
                    policy.name(),
                    &descriptor,
                    Some(&hmac)
                ))
                .await,
            Err(Error::DeviceNotFound)
        ));
        assert_eq!(wrong.shared.bind_calls.load(Ordering::SeqCst), 0);
        assert_eq!(simulator.shared.bind_calls.load(Ordering::SeqCst), 0);

        // A full multisig session: the listed (unbound) handle registers and
        // returns the token; signing goes through the rebound handle.
        let mut session = SigningSession::from_listed(&listing.list[0], policy.clone(), binder)
            .await
            .unwrap();
        assert_eq!(session.handle(), SigningHandle::Bound);
        assert_eq!(
            session.register().await.unwrap(),
            RegistrationOutcome::Registered
        );
        let signed = session.sign(step1.psbt()).await.unwrap().into_psbt();
        assert_eq!(sig_count(&signed), inputs);
        assert_eq!(ledger.shared.bind_calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            ledger.shared.bound_with.lock().unwrap().last().cloned(),
            Some((policy.name().to_string(), Some(hmac)))
        );

        // The same session with only other devices connected fails at sign,
        // and nothing was asked to sign.
        let mut session = SigningSession::from_listed(
            &listing.list[0],
            policy.clone(),
            Arc::new(ProductionBinder::with_opener(
                &listing,
                FakeOpener::new(vec![wrong.clone()]),
            )),
        )
        .await
        .unwrap();
        session.register().await.unwrap();
        assert!(matches!(
            session.sign(step1.psbt()).await.unwrap_err(),
            SignError::Device(_)
        ));
        assert_eq!(wrong.shared.sign_calls.load(Ordering::SeqCst), 0);
        assert_eq!(ledger.shared.sign_calls.load(Ordering::SeqCst), 1);

        // Singlesig: the unnamed default policy, no token.
        let single = construction(Shape::Wpkh);
        let single_policy = split_policy(single.source()).unwrap();
        let ledger1 = fake(DeviceKind::Ledger, 1);
        let single_listing = self::listing(&root, None, &[("ledger-1", &ledger1)]);
        let binder = Arc::new(ProductionBinder::with_opener(
            &single_listing,
            FakeOpener::new(vec![ledger1.clone()]),
        ));
        let mut session =
            SigningSession::from_listed(&single_listing.list[0], single_policy, binder)
                .await
                .unwrap();
        assert_eq!(
            session.register().await.unwrap(),
            RegistrationOutcome::NotNeeded
        );
        let signed = session.sign(single.psbt()).await.unwrap().into_psbt();
        assert_eq!(sig_count(&signed), single.psbt().inputs.len());
        assert_eq!(
            ledger1.shared.bound_with.lock().unwrap().clone(),
            vec![(String::new(), None)]
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir(root).unwrap();
    }
}
