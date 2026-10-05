//! "Sign with connected device" in the Split panel (#568 B4b-3b; owner
//! decisions D7 and P6, #646 F3).
//!
//! In a sign stage (step 1's [`Stage::Sign`], step 2's
//! [`Step2Stage::Sign`]) the panel can open a session-only device listing
//! for the foreign wallet's policy and ask one listed device at a time to
//! sign. The listing is [`HardwareWallets::new`] on Bitcoin mainnet opened
//! with [`HardwareWallets::with_split_policy`] for that policy, and never
//! with a loaded wallet (F3: a wallet's binding would win over the Split
//! policy). It persists nothing: no BitBox02 pairing, no registration token
//! (a Ledger's HMAC dies with its session, P6). Building it and running the
//! device happen in tasks, never on the UI thread; hidapi enumeration runs
//! on the blocking pool (`split_hardware::bind`).
//!
//! A device's output is untrusted. It enters exactly where a signed file
//! does: step 1's [`SplitPanel::import_psbts`] (`step1::import`) and step 2's
//! [`SplitPanel::step2_import_psbts`] (`combine` and the preparation's
//! `verify_signed`, then the handoff). Nothing here finalizes.
//!
//! A result from a request the panel moved on from (a revocation, a close,
//! another request) carries an old sequence number and is dropped by
//! [`SplitPanel::apply`]. Like the rest of the panel, this is reachable only
//! from a resumed journal or a started panel, which only the sweep review's
//! "Start split" creates (#568 B5c-2).

use std::sync::Arc;

use coincube_core::{foreign_split::SplitSource, miniscript::bitcoin::Network};
use iced::{Subscription, Task};

use super::{SplitEvent, SplitMessage, SplitPanel, Stage, Step2Stage, Work};
use crate::{
    app::{message::Message, view},
    dir::CoincubeDirectory,
    hw::{HardwareWallet, HardwareWalletMessage, HardwareWallets},
    services::foreign_scan::{Branch, ScanDescriptor, SigningRoutes},
    split_hardware::{
        bind::{HidLedgerOpener, LedgerOpener},
        flow::DeviceSigning,
        policy::{split_descriptor, split_policy},
        sign::{
            registration_notice, DevicePolicy, DEVICE_SHOWS_BITCOIN_WARNING,
            STEP1_DATA_OUTPUT_NOTE, STEP2_DEVICE_SHOWS_BITCOIN_WARNING,
        },
    },
};

/// The step a device signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceStep {
    Step1,
    Step2,
}

/// Device intents, inside [`SplitMessage::Device`].
#[derive(Debug, Clone)]
pub enum DeviceMessage {
    /// Open the device listing for this step's policy.
    Open,
    /// The listing's refresh subscription.
    Devices(HardwareWalletMessage),
    /// Sign with the listed device of this id.
    Sign(String),
    /// Close the listing.
    Cancel,
}

/// The copy shown above the device list: what the device will show.
pub fn step_copy(step: DeviceStep) -> &'static [&'static str] {
    match step {
        DeviceStep::Step1 => &[STEP1_DATA_OUTPUT_NOTE],
        DeviceStep::Step2 => &[
            DEVICE_SHOWS_BITCOIN_WARNING,
            STEP2_DEVICE_SHOWS_BITCOIN_WARNING,
        ],
    }
}

/// A session-only listing bound to the policy it was built for.
pub struct Listing {
    devices: HardwareWallets,
    policy: DevicePolicy,
}

impl std::fmt::Debug for Listing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listing")
            .field("policy", &self.policy)
            .field("devices", &self.devices.list.len())
            .finish()
    }
}

/// The listing for `source`'s policy under `datadir`: new, on Bitcoin
/// mainnet, bound to the policy, and with no wallet loaded (F3). Called in a
/// blocking task.
pub fn build_listing(datadir: CoincubeDirectory, source: &SplitSource) -> Result<Listing, String> {
    let policy = split_policy(source).map_err(|error| error.to_string())?;
    let descriptor = split_descriptor(source)
        .map_err(|error| error.to_string())?
        .to_string();
    let devices = HardwareWallets::new(datadir, Network::Bitcoin)
        .with_split_policy(policy.name().to_string(), descriptor);
    Ok(Listing { devices, policy })
}

/// #653 F4: the routes `source`'s descriptors give, read from #653's
/// `SigningRoutes` (U6: no `tr`, an origin on every ranged key), not
/// recomputed. Both branches must give a route; a descriptor the scanner
/// does not accept gives none.
pub fn signing_routes(source: &SplitSource) -> SigningRoutes {
    let routes = |branch, text: String| {
        ScanDescriptor::parse(branch, &text)
            .map(|descriptor| descriptor.capabilities().signing)
            .unwrap_or(SigningRoutes::NONE)
    };
    let external = routes(Branch::External, source.external().to_string());
    let Some(internal) = source
        .internal()
        .map(|d| routes(Branch::Internal, d.to_string()))
    else {
        return external;
    };
    SigningRoutes {
        psbt_file: external.psbt_file && internal.psbt_file,
        in_app_hardware: external.in_app_hardware && internal.in_app_hardware,
        seed_unified: external.seed_unified && internal.seed_unified,
    }
}

/// One listed device, as the view shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub label: String,
    /// The id to sign with: a supported device that holds a key of this
    /// wallet. `None` for anything else (locked, unsupported, a stranger).
    pub sign: Option<String>,
    /// What a multisig registration on this device means (P6).
    pub notice: Option<&'static str>,
}

/// The panel's device signing state.
pub struct DeviceSigner {
    /// The App's datadir, handed over with the panel at discovery. The
    /// listing writes nothing under it.
    datadir: Option<CoincubeDirectory>,
    opener: Arc<dyn LedgerOpener>,
    /// The step the listing is open (or opening) for.
    step: Option<DeviceStep>,
    listing: Option<Listing>,
}

impl Default for DeviceSigner {
    fn default() -> Self {
        Self {
            datadir: None,
            opener: Arc::new(HidLedgerOpener),
            step: None,
            listing: None,
        }
    }
}

impl DeviceSigner {
    /// Drop the listing (its subscription ends with it).
    pub(super) fn close(&mut self) {
        self.step = None;
        self.listing = None;
    }

    pub fn step(&self) -> Option<DeviceStep> {
        self.step
    }

    pub fn is_open(&self) -> bool {
        self.listing.is_some()
    }

    /// The listed devices. Only a supported device whose fingerprint is one
    /// of the policy's keys is offered for signing.
    pub fn rows(&self) -> Vec<DeviceRow> {
        let Some(listing) = &self.listing else {
            return Vec::new();
        };
        let multisig = listing.policy.shape().is_multisig();
        listing
            .devices
            .list
            .iter()
            .map(|hw| match hw {
                HardwareWallet::Supported {
                    id,
                    kind,
                    fingerprint,
                    alias,
                    ..
                } => {
                    let ours = listing.policy.contains(*fingerprint);
                    let alias = alias
                        .as_ref()
                        .map(|alias| format!(" ({alias})"))
                        .unwrap_or_default();
                    DeviceRow {
                        label: if ours {
                            format!("{kind} {fingerprint}{alias}")
                        } else {
                            format!("{kind} {fingerprint}{alias}: holds none of this wallet's keys")
                        },
                        sign: ours.then(|| id.clone()),
                        notice: (ours && multisig).then(|| registration_notice(*kind)),
                    }
                }
                HardwareWallet::Locked {
                    kind, pairing_code, ..
                } => DeviceRow {
                    label: match pairing_code {
                        Some(code) => format!("{kind}: confirm pairing code {code} on the device"),
                        None => format!("{kind}: unlock the device"),
                    },
                    sign: None,
                    notice: None,
                },
                HardwareWallet::Unsupported { kind, .. } => DeviceRow {
                    label: format!("{kind}: not supported for Split"),
                    sign: None,
                    notice: None,
                },
            })
            .collect()
    }
}

fn devices_message(message: HardwareWalletMessage) -> SplitMessage {
    SplitMessage::Device(DeviceMessage::Devices(message))
}

impl SplitPanel {
    /// The App's datadir, for the device listing (#568 B4b-3b). Set once,
    /// at discovery, with the panel.
    pub fn set_device_datadir(&mut self, datadir: CoincubeDirectory) {
        self.device.datadir = Some(datadir);
    }

    #[cfg(test)]
    pub(crate) fn device_datadir(&self) -> Option<&CoincubeDirectory> {
        self.device.datadir.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn set_device_opener(&mut self, opener: Arc<dyn LedgerOpener>) {
        self.device.opener = opener;
    }

    pub fn device(&self) -> &DeviceSigner {
        &self.device
    }

    /// The step a device could sign now, if any: step 1's construction or
    /// step 2's build is waiting for signatures.
    pub fn device_step(&self) -> Option<DeviceStep> {
        match self.stage {
            Stage::Sign if self.construction.is_some() => Some(DeviceStep::Step1),
            Stage::Step2(Step2Stage::Sign)
                if self.step2_psbt.is_some() && self.construction.is_some() =>
            {
                Some(DeviceStep::Step2)
            }
            _ => None,
        }
    }

    /// Whether a device listing may be opened here: only for a wallet whose
    /// descriptors give the in-app hardware route (#653 F4).
    pub fn can_open_device(&self) -> bool {
        self.device_step().is_some()
            && self.device.datadir.is_some()
            && !self.device.is_open()
            && self
                .construction
                .as_deref()
                .is_some_and(|construction| signing_routes(construction.source()).in_app_hardware)
    }

    /// The listing's refresh, while it is open for the step on screen or a
    /// device is working on it.
    pub fn subscription(&self) -> Subscription<SplitMessage> {
        let live = match self.stage {
            Stage::Working(Work::SigningOnDevice) => true,
            _ => self.device_step().is_some() && self.device_step() == self.device.step,
        };
        match &self.device.listing {
            Some(listing) if live && !self.hidden => listing.devices.refresh().map(devices_message),
            _ => Subscription::none(),
        }
    }

    fn back_to(&mut self, step: DeviceStep) {
        self.stage = match step {
            DeviceStep::Step1 => Stage::Sign,
            DeviceStep::Step2 => Stage::Step2(Step2Stage::Sign),
        };
    }

    pub(super) fn update_device(&mut self, message: DeviceMessage) -> Task<Message> {
        match message {
            DeviceMessage::Open if self.can_open_device() => {
                let (Some(step), Some(datadir), Some(construction)) = (
                    self.device_step(),
                    self.device.datadir.clone(),
                    self.construction.clone(),
                ) else {
                    return Task::none();
                };
                self.device.step = Some(step);
                self.stage = Stage::Working(Work::ListingDevices);
                self.spawn(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            build_listing(datadir, construction.source()).map(Box::new)
                        })
                        .await
                        .map_err(|_| "Listing the devices was interrupted.".to_string())?
                    },
                    SplitEvent::DeviceListed,
                )
            }
            DeviceMessage::Devices(message) => {
                let Some(listing) = self.device.listing.as_mut() else {
                    return Task::none();
                };
                match listing.devices.update(message) {
                    Ok(task) => task.map(|message| {
                        Message::View(view::Message::Split(devices_message(message)))
                    }),
                    Err(_) => Task::none(),
                }
            }
            DeviceMessage::Sign(id) => {
                let (Some(step), Some(listing)) =
                    (self.device_step(), self.device.listing.as_ref())
                else {
                    return Task::none();
                };
                if self.device.step != Some(step) {
                    return Task::none();
                }
                let psbt = match step {
                    DeviceStep::Step1 => self.construction.as_deref().map(|c| c.psbt().clone()),
                    DeviceStep::Step2 => self.step2_psbt.clone(),
                };
                let Some(psbt) = psbt else {
                    return Task::none();
                };
                let signing = match DeviceSigning::prepare_with(
                    &listing.devices,
                    &id,
                    &listing.policy,
                    &psbt,
                    self.device.opener.clone(),
                ) {
                    Ok(signing) => signing,
                    Err(error) => {
                        self.notice = Some(error.to_string());
                        return Task::none();
                    }
                };
                self.notice = None;
                self.stage = Stage::Working(Work::SigningOnDevice);
                self.spawn(
                    async move {
                        signing
                            .run()
                            .await
                            .map(|signed| Box::new(signed.into_psbt()))
                            .map_err(|error| error.to_string())
                    },
                    SplitEvent::DeviceSigned,
                )
            }
            DeviceMessage::Cancel if !matches!(self.stage, Stage::Working(_)) => {
                self.device.close();
                Task::none()
            }
            _ => Task::none(),
        }
    }

    /// A device task's result. Stale ones were dropped by sequence.
    pub(super) fn apply_device(&mut self, event: SplitEvent) -> Task<Message> {
        let Some(step) = self.device.step else {
            return Task::none();
        };
        match event {
            SplitEvent::DeviceListed(_, result) => {
                self.back_to(step);
                match result {
                    Ok(listing) => self.device.listing = Some(*listing),
                    Err(reason) => {
                        self.device.close();
                        self.notice = Some(reason);
                    }
                }
                Task::none()
            }
            SplitEvent::DeviceSigned(_, result) => {
                self.back_to(step);
                match result {
                    // The verified import seams, exactly as a signed file.
                    Ok(signed) => match step {
                        DeviceStep::Step1 => self.import_psbts(vec![*signed]),
                        DeviceStep::Step2 => self.step2_import_psbts(vec![*signed]),
                    },
                    Err(reason) => {
                        self.notice = Some(reason);
                        Task::none()
                    }
                }
            }
            _ => Task::none(),
        }
    }
}

#[cfg(all(test, unix))]
pub(super) mod tests;
