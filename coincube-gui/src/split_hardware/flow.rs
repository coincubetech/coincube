//! In-app device signing for a Split PSBT (#568 Track B, slice B4b-2; owner
//! decision D7): open a B4a [`SigningSession`] on a listed device, register
//! the policy when the device does not report it, and sign.
//!
//! One call is one session on one device. A multisig needs one call per
//! signing device; each returns that device's signatures on an otherwise
//! unchanged copy of the PSBT, and the registration token a Ledger returned
//! is dropped with the session (P6). The output is an
//! [`UnverifiedDeviceSignatures`]: the caller pushes its PSBT into the
//! verified import seam of the step it is signing (`step1::import` for step
//! 1, `panel2::combine` for step 2), exactly as a signed file would be, and
//! never finalizes it directly (B4a F5).
//!
//! The listing is a [`HardwareWallets`] opened with
//! [`HardwareWallets::with_split_policy`]; [`ProductionBinder`] is built from
//! it. Nothing here is reachable from the GUI (D1): no panel calls
//! [`sign_with_device`] yet.

use std::{fmt, sync::Arc};

use coincube_core::miniscript::bitcoin::Psbt;

use super::{
    bind::{HidLedgerOpener, LedgerOpener, ProductionBinder},
    sign::{DevicePolicy, SignError, SigningSession, UnverifiedDeviceSignatures},
};
use crate::hw::{HardwareWallet, HardwareWallets};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowError {
    /// No device with this id is in the listing.
    DeviceNotListed,
    /// The listing was opened for another policy than the one to sign.
    ListingBoundToAnotherWallet,
    Sign(SignError),
}

impl From<SignError> for FlowError {
    fn from(error: SignError) -> Self {
        Self::Sign(error)
    }
}

impl fmt::Display for FlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceNotListed => f.write_str("The hardware wallet is no longer connected."),
            Self::ListingBoundToAnotherWallet => {
                f.write_str("The connected devices were listed for another wallet. Reconnect them for this one.")
            }
            Self::Sign(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for FlowError {}

/// One device's signing of one PSBT, prepared from the listing so it owns
/// everything it needs and can run off the UI thread.
pub struct DeviceSigning {
    device: HardwareWallet,
    policy: DevicePolicy,
    psbt: Psbt,
    binder: Arc<ProductionBinder>,
}

impl DeviceSigning {
    /// Prepares signing on the listed device `id`, with the production
    /// Ledger opener.
    pub fn prepare(
        listing: &HardwareWallets,
        id: &str,
        policy: &DevicePolicy,
        psbt: &Psbt,
    ) -> Result<Self, FlowError> {
        Self::prepare_with(listing, id, policy, psbt, Arc::new(HidLedgerOpener))
    }

    pub fn prepare_with(
        listing: &HardwareWallets,
        id: &str,
        policy: &DevicePolicy,
        psbt: &Psbt,
        opener: Arc<dyn LedgerOpener>,
    ) -> Result<Self, FlowError> {
        let device = listing
            .list
            .iter()
            .find(|hw| hw.id() == id)
            .cloned()
            .ok_or(FlowError::DeviceNotListed)?;
        // A listing bound to another policy would sign with the wrong
        // wallet name or policy on the listed handle: refuse before the
        // session opens, so no device is asked to register anything.
        if let Some(binding) = listing.split_policy_binding() {
            if binding.name() != policy.name() {
                return Err(FlowError::ListingBoundToAnotherWallet);
            }
        }
        Ok(Self {
            device,
            policy: policy.clone(),
            psbt: psbt.clone(),
            binder: Arc::new(ProductionBinder::with_opener(listing, opener)),
        })
    }

    /// Open, register when the device does not report the policy, sign.
    pub async fn run(self) -> Result<UnverifiedDeviceSignatures, FlowError> {
        let mut session =
            SigningSession::from_listed(&self.device, self.policy, self.binder).await?;
        session.register().await?;
        Ok(session.sign(&self.psbt).await?)
    }
}

/// Signs `psbt` with the listed device `id` for `policy`: one session,
/// opened, registered when needed, and dropped with its token afterwards.
pub async fn sign_with_device(
    listing: &HardwareWallets,
    id: &str,
    policy: &DevicePolicy,
    psbt: &Psbt,
) -> Result<UnverifiedDeviceSignatures, FlowError> {
    DeviceSigning::prepare(listing, id, policy, psbt)?
        .run()
        .await
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, sync::atomic::Ordering};

    use async_hwi::DeviceKind;
    use coincube_core::miniscript::bitcoin::Network;

    use super::*;
    use crate::{
        app::state::vault::split::step1::{self, Imported},
        dir::CoincubeDirectory,
        services::split_test_wallets::{Shape, SHAPES},
        split_hardware::{
            bind::tests::{construction, fake, listing, sig_count, temp_root, FakeOpener},
            policy::{split_descriptor, split_policy},
        },
    };

    /// Every shape: the flow's output, pushed into step 1's verified import
    /// like a signed file, completes the construction (one device for
    /// singlesig; a Coldcard and a Ledger for the 2-of-3 shapes).
    #[tokio::test]
    async fn device_flow_output_enters_verified_import() {
        for shape in SHAPES {
            let step1 = construction(shape);
            let policy = split_policy(step1.source()).unwrap();
            let descriptor = split_descriptor(step1.source()).unwrap().to_string();
            let multisig = policy.shape().is_multisig();
            let coldcard = fake(DeviceKind::Coldcard, 1);
            let ledger = fake(DeviceKind::Ledger, 2);
            let mut devices = vec![("coldcard-1", &coldcard)];
            if multisig {
                devices.push(("ledger-1", &ledger));
            }
            let opener = FakeOpener::new(vec![ledger.clone()]);
            let root = temp_root();
            let listing = listing(&root, Some((policy.name(), &descriptor)), &devices);

            let mut files = Vec::new();
            for (id, _) in &devices {
                let signed = DeviceSigning::prepare_with(
                    &listing,
                    id,
                    &policy,
                    step1.psbt(),
                    opener.clone(),
                )
                .unwrap()
                .run()
                .await
                .unwrap()
                .into_psbt();
                assert_eq!(signed.unsigned_tx, step1.psbt().unsigned_tx);
                assert_eq!(
                    sig_count(&signed),
                    step1.psbt().inputs.len(),
                    "{shape:?} {id}"
                );
                files.push(signed);
                if multisig && files.len() == 1 {
                    // One of two needed signers: valid, not yet complete.
                    assert!(
                        matches!(step1::import(&step1, &files).unwrap(), Imported::Partial),
                        "{:?}",
                        shape
                    );
                }
            }
            assert!(
                matches!(
                    step1::import(&step1, &files).unwrap(),
                    Imported::Complete(..)
                ),
                "{:?}",
                shape
            );
            if multisig {
                assert_eq!(coldcard.shared.register_calls.load(Ordering::SeqCst), 1);
                assert_eq!(ledger.shared.register_calls.load(Ordering::SeqCst), 1);
            } else {
                assert_eq!(coldcard.shared.register_calls.load(Ordering::SeqCst), 0);
            }

            // Refusals before any device is used: an id that is not listed,
            // and a listing opened for another wallet.
            assert_eq!(
                sign_with_device(&listing, "nope", &policy, step1.psbt())
                    .await
                    .unwrap_err(),
                FlowError::DeviceNotListed
            );
            let other = self::listing(
                &root,
                Some((&format!("{}x", policy.name()), &descriptor)),
                &devices,
            );
            let registered_before = coldcard.shared.register_calls.load(Ordering::SeqCst);
            assert_eq!(
                sign_with_device(&other, "coldcard-1", &policy, step1.psbt())
                    .await
                    .unwrap_err(),
                FlowError::ListingBoundToAnotherWallet
            );
            assert_eq!(
                coldcard.shared.register_calls.load(Ordering::SeqCst),
                registered_before
            );
            assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
            fs::remove_dir(root).unwrap();
        }
    }

    /// A Split listing is session-only: no pairing persistence, nothing under
    /// the datadir after multisig sessions on every bound device class, no
    /// key material in Debug output, and a Ledger registers again in a new
    /// session (P6).
    #[tokio::test]
    async fn split_listing_with_policy_persists_nothing() {
        let step1 = construction(Shape::WshMulti);
        let inputs = step1.psbt().inputs.len();
        let policy = split_policy(step1.source()).unwrap();
        let descriptor = split_descriptor(step1.source()).unwrap().to_string();
        let root = temp_root();
        let coldcard = fake(DeviceKind::Coldcard, 1);
        let bitbox = fake(DeviceKind::BitBox02, 3);
        let ledger = fake(DeviceKind::Ledger, 2);
        // Not `.ephemeral()`: a Split listing is session-only by construction.
        let mut listing =
            HardwareWallets::new(CoincubeDirectory::new(root.clone()), Network::Bitcoin)
                .with_split_policy(policy.name().to_string(), descriptor.clone());
        assert!(!listing.persists_pairing());
        let binding = listing.split_policy_binding().unwrap();
        assert_eq!(binding.bound(), Some((policy.name(), descriptor.as_str())));
        for (id, device) in [
            ("coldcard-1", &coldcard),
            ("bitbox-1", &bitbox),
            ("ledger-1", &ledger),
        ] {
            listing.list.push(HardwareWallet::Supported {
                id: id.to_string(),
                device: device.arc(),
                kind: device.kind,
                fingerprint: crate::split_hardware::bind::tests::fp(device.seed),
                version: None,
                registered: None,
                alias: None,
            });
        }
        let opener = FakeOpener::new(vec![ledger.clone()]);
        for id in ["coldcard-1", "bitbox-1", "ledger-1", "ledger-1"] {
            let signed =
                DeviceSigning::prepare_with(&listing, id, &policy, step1.psbt(), opener.clone())
                    .unwrap()
                    .run()
                    .await
                    .unwrap()
                    .into_psbt();
            assert_eq!(sig_count(&signed), inputs, "{id}");
        }
        // The token never outlives its session: the second Ledger session
        // registered again, the stored registrations were asked once.
        assert_eq!(ledger.shared.register_calls.load(Ordering::SeqCst), 2);
        assert_eq!(coldcard.shared.register_calls.load(Ordering::SeqCst), 1);
        assert_eq!(bitbox.shared.register_calls.load(Ordering::SeqCst), 1);
        assert!(!listing.persists_pairing());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let debug = format!("{listing:?} {:?}", listing.split_policy_binding());
        assert!(
            !debug.contains("xpub") && !debug.contains("wsh") && !debug.contains("multi"),
            "{}",
            debug
        );
        fs::remove_dir(root).unwrap();
    }

    /// D1: the device flow, the binder, the policy builders and the listing
    /// binding are named only under `split_hardware/` (the binding also in
    /// `hw.rs`, which owns it). No panel, view or App reaches them yet.
    #[test]
    fn split_device_flow_has_no_gui_caller() {
        fn walk(dir: &Path, files: &mut Vec<(String, String)>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, files);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push((
                        path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                        fs::read_to_string(&path).unwrap(),
                    ));
                }
            }
        }
        let mut files = Vec::new();
        walk(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        assert!(files.iter().any(|(file, _)| file == "src/hw.rs"));
        assert!(files
            .iter()
            .any(|(file, _)| file == "src/split_hardware/flow.rs"));
        let mut unexpected = Vec::new();
        for (file, text) in &files {
            if file.starts_with("src/split_hardware/") {
                continue;
            }
            for ident in [
                "sign_with_device",
                "DeviceSigning",
                "FlowError",
                "ProductionBinder",
                "LedgerOpener",
                "LedgerCandidate",
                "split_descriptor(",
                "PolicyError",
            ] {
                if text.contains(ident) {
                    unexpected.push(format!("{file}: {ident}"));
                }
            }
            // `split_policy(` is the policy builder; `with_split_policy(`
            // is the listing binding `hw.rs` owns.
            if text.matches("split_policy(").count() != text.matches("with_split_policy(").count() {
                unexpected.push(format!("{file}: split_policy("));
            }
            for ident in [
                "with_split_policy",
                "SplitPolicyBinding",
                "split_policy_binding",
            ] {
                if text.contains(ident) && file != "src/hw.rs" {
                    unexpected.push(format!("{file}: {ident}"));
                }
            }
        }
        assert!(unexpected.is_empty(), "{:?}", unexpected);
    }
}
