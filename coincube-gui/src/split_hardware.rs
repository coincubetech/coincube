//! Hardware-wallet account source for the Split panel (canonical BTCB2 plan
//! PR 8, scan only).
//!
//! A connected device exports one standard singlesig account xpub
//! (BIP84/BIP49/BIP44, `m/purpose'/0'/account'`). The xpub is checked against
//! the device's master fingerprint, mainnet, depth and child number before the
//! existing [`AccountXpubSource`] boundary derives public receive/change
//! descriptors. Nothing here signs, and nothing is persisted or logged: the
//! device list is session-only (no BitBox02 pairing written) and the account
//! lives in memory until cancel, disconnect or a changed choice clears it.
//! Taproot (BIP86) is deliberately not offered.
//!
//! [`sign`] holds the separate hardware signing session (slice B4a). It has
//! no GUI caller yet, and the account source above still never signs.

pub mod policy;
pub mod sign;

use std::sync::Arc;

use async_hwi::HWI;
use coincube_core::miniscript::bitcoin::{
    bip32::{ChildNumber, DerivationPath, Fingerprint, Xpub},
    Network, NetworkKind,
};
use coincube_ui::{
    component::{button, text::*},
    theme,
    widget::{Column, Element, Row},
};
use iced::{widget::text_input, Alignment, Length, Subscription, Task};

use crate::{
    dir::CoincubeDirectory,
    hw::{HardwareWallet, HardwareWalletMessage, HardwareWallets},
    services::{
        foreign_scan::ScanDescriptor,
        foreign_wallet_source::{AccountXpubSource, StandardSinglesig},
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Bip84,
    Bip49,
    Bip44,
}

pub const PURPOSES: [Purpose; 3] = [Purpose::Bip84, Purpose::Bip49, Purpose::Bip44];

impl Purpose {
    fn number(self) -> u32 {
        match self {
            Self::Bip84 => 84,
            Self::Bip49 => 49,
            Self::Bip44 => 44,
        }
    }

    fn standard(self) -> StandardSinglesig {
        match self {
            Self::Bip84 => StandardSinglesig::Bip84,
            Self::Bip49 => StandardSinglesig::Bip49,
            Self::Bip44 => StandardSinglesig::Bip44,
        }
    }

    /// `m/purpose'/0'/account'` (mainnet coin type only).
    pub fn path(self, account: u32) -> Result<DerivationPath, HardwareSourceError> {
        let account =
            ChildNumber::from_hardened_idx(account).map_err(|_| HardwareSourceError::Account)?;
        Ok(DerivationPath::from(vec![
            ChildNumber::Hardened {
                index: self.number(),
            },
            ChildNumber::Hardened { index: 0 },
            account,
        ]))
    }
}

impl std::fmt::Display for Purpose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bip84 => write!(f, "Native SegWit (BIP84, m/84'/0'/n')"),
            Self::Bip49 => write!(f, "Nested SegWit (BIP49, m/49'/0'/n')"),
            Self::Bip44 => write!(f, "Legacy (BIP44, m/44'/0'/n')"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareSourceError {
    /// The device reported a master fingerprint other than the listed one.
    Fingerprint,
    Network,
    /// Depth or child number do not match `m/purpose'/0'/account'`.
    Depth,
    /// The parent fingerprint cannot belong to a depth-3 account key.
    ParentFingerprint,
    Account,
    Descriptor,
    Device,
    Disconnected,
}

fn error_copy(error: HardwareSourceError) -> &'static str {
    match error {
        HardwareSourceError::Fingerprint => "The device reported a different wallet fingerprint than the one listed. Reconnect the device and try again.",
        HardwareSourceError::Network => "The device returned a non-mainnet key. Only Bitcoin mainnet accounts can be scanned.",
        HardwareSourceError::Depth | HardwareSourceError::ParentFingerprint => "The device returned a key that does not match the requested account path. Nothing was scanned.",
        HardwareSourceError::Account => "Enter an account number between 0 and 2147483647.",
        HardwareSourceError::Descriptor => "The account could not be turned into a supported public descriptor.",
        HardwareSourceError::Device => "The device did not return the account. Unlock it, open its Bitcoin app, and try again.",
        HardwareSourceError::Disconnected => "The device is no longer connected.",
    }
}

pub fn parse_account(value: &str) -> Result<u32, HardwareSourceError> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(HardwareSourceError::Account);
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|account| *account < (1 << 31))
        .ok_or(HardwareSourceError::Account)
}

/// Public account material read from a device. Debug output is redacted so
/// the xpub never reaches a log line through message tracing.
pub struct HardwareAccount {
    pub device_id: String,
    pub fingerprint: Fingerprint,
    pub purpose: Purpose,
    pub account: u32,
    pub external: ScanDescriptor,
    pub internal: ScanDescriptor,
}

impl std::fmt::Debug for HardwareAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HardwareAccount")
            .field("fingerprint", &self.fingerprint)
            .field("purpose", &self.purpose)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

/// Checks a device-exported account xpub and derives its descriptors, with
/// the device's master fingerprint recorded as the key origin.
pub fn account_from_device(
    device_id: String,
    purpose: Purpose,
    account: u32,
    listed: Fingerprint,
    reported: Fingerprint,
    xpub: Xpub,
) -> Result<HardwareAccount, HardwareSourceError> {
    if reported != listed {
        return Err(HardwareSourceError::Fingerprint);
    }
    if xpub.network != NetworkKind::Main {
        return Err(HardwareSourceError::Network);
    }
    let expected = purpose.path(account)?;
    if xpub.depth != 3 || Some(&xpub.child_number) != expected.as_ref().last() {
        return Err(HardwareSourceError::Depth);
    }
    // A depth-3 key's parent is the depth-2 coin-type key: its fingerprint is
    // never zero, never the master's, and never the key's own.
    if xpub.parent_fingerprint == Fingerprint::default()
        || xpub.parent_fingerprint == listed
        || xpub.fingerprint() == listed
        || xpub.parent_fingerprint == xpub.fingerprint()
    {
        return Err(HardwareSourceError::ParentFingerprint);
    }
    let descriptors = AccountXpubSource::new(purpose.standard(), account, listed, xpub)
        .map_err(|_| HardwareSourceError::Depth)?
        .descriptors()
        .map_err(|_| HardwareSourceError::Descriptor)?;
    Ok(HardwareAccount {
        device_id,
        fingerprint: descriptors.fingerprint,
        purpose,
        account,
        external: descriptors.external,
        internal: descriptors.internal,
    })
}

/// Reads the account through the same HWI export the installer uses
/// (`installer::step::descriptor::editor::key::get_extended_pubkey`), after a
/// fresh master-fingerprint read so a swapped device is refused.
pub async fn read_account(
    device: Arc<dyn HWI + Send + Sync>,
    device_id: String,
    listed: Fingerprint,
    purpose: Purpose,
    account: u32,
) -> Result<HardwareAccount, HardwareSourceError> {
    let path = purpose.path(account)?;
    let reported = device
        .get_master_fingerprint()
        .await
        .map_err(|_| HardwareSourceError::Device)?;
    if reported != listed {
        return Err(HardwareSourceError::Fingerprint);
    }
    let xpub = device
        .get_extended_pubkey(&path)
        .await
        .map_err(|_| HardwareSourceError::Device)?;
    account_from_device(device_id, purpose, account, listed, reported, xpub)
}

#[derive(Debug, Clone)]
pub enum HardwareMessage {
    Devices(HardwareWalletMessage),
    PurposeSelected(Purpose),
    AccountEdited(String),
    Read(String),
    Loaded(Result<Arc<HardwareAccount>, HardwareSourceError>, u64),
}

#[derive(Debug, Clone)]
pub enum HardwareStatus {
    Idle,
    Reading(String),
    Ready(Arc<HardwareAccount>),
    Failed(HardwareSourceError),
}

pub struct HardwareSource {
    devices: Option<HardwareWallets>,
    pub purpose: Purpose,
    pub account: String,
    status: HardwareStatus,
    generation: u64,
}

impl Default for HardwareSource {
    fn default() -> Self {
        Self {
            devices: None,
            purpose: Purpose::Bip84,
            account: "0".to_string(),
            status: HardwareStatus::Idle,
            generation: 0,
        }
    }
}

impl HardwareSource {
    /// Session-only device listing on Bitcoin mainnet with no wallet loaded:
    /// no pairing persistence, phone discovery or identity creation.
    pub fn set_root(&mut self, datadir: CoincubeDirectory) {
        if self.devices.is_none() {
            self.devices = Some(HardwareWallets::new(datadir, Network::Bitcoin).ephemeral());
        }
    }

    pub fn account(&self) -> Option<&HardwareAccount> {
        match &self.status {
            HardwareStatus::Ready(account) => Some(account),
            _ => None,
        }
    }

    pub fn status(&self) -> &HardwareStatus {
        &self.status
    }

    #[cfg(test)]
    pub(crate) fn ready_for_test(&mut self, account: HardwareAccount) {
        self.status = HardwareStatus::Ready(Arc::new(account));
    }

    /// Drops any read or in-flight account. A later `Loaded` is stale.
    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.status = HardwareStatus::Idle;
    }

    pub fn subscription(&self) -> Subscription<HardwareMessage> {
        self.devices
            .as_ref()
            .map(|devices| devices.refresh().map(HardwareMessage::Devices))
            .unwrap_or_else(Subscription::none)
    }

    fn supported(&self, id: &str) -> Option<(Arc<dyn HWI + Send + Sync>, Fingerprint)> {
        self.devices.as_ref()?.list.iter().find_map(|hw| match hw {
            HardwareWallet::Supported {
                id: candidate,
                device,
                fingerprint,
                ..
            } if candidate == id => Some((device.clone(), *fingerprint)),
            _ => None,
        })
    }

    fn selected_device(&self) -> Option<&str> {
        match &self.status {
            HardwareStatus::Reading(id) => Some(id),
            HardwareStatus::Ready(account) => Some(&account.device_id),
            _ => None,
        }
    }

    /// Returns the follow-up task and whether previously read account
    /// material (and so any scan built from it) was invalidated.
    pub fn update(&mut self, message: HardwareMessage) -> (Task<HardwareMessage>, bool) {
        match message {
            HardwareMessage::Devices(message) => {
                let Some(devices) = self.devices.as_mut() else {
                    return (Task::none(), false);
                };
                let task = devices
                    .update(message)
                    .map(|task| task.map(HardwareMessage::Devices))
                    .unwrap_or_else(|_| Task::none());
                let gone = self
                    .selected_device()
                    .map(str::to_string)
                    .is_some_and(|id| self.supported(&id).is_none());
                if gone {
                    self.clear();
                    self.status = HardwareStatus::Failed(HardwareSourceError::Disconnected);
                }
                (task, gone)
            }
            HardwareMessage::PurposeSelected(purpose) => {
                self.purpose = purpose;
                self.clear();
                (Task::none(), true)
            }
            HardwareMessage::AccountEdited(value) => {
                self.account = value;
                self.clear();
                (Task::none(), true)
            }
            HardwareMessage::Read(id) => {
                self.clear();
                let account = match parse_account(&self.account) {
                    Ok(account) => account,
                    Err(error) => {
                        self.status = HardwareStatus::Failed(error);
                        return (Task::none(), true);
                    }
                };
                let Some((device, listed)) = self.supported(&id) else {
                    self.status = HardwareStatus::Failed(HardwareSourceError::Disconnected);
                    return (Task::none(), true);
                };
                let generation = self.generation;
                let purpose = self.purpose;
                self.status = HardwareStatus::Reading(id.clone());
                (
                    Task::perform(
                        read_account(device, id, listed, purpose, account),
                        move |r| HardwareMessage::Loaded(r.map(Arc::new), generation),
                    ),
                    true,
                )
            }
            HardwareMessage::Loaded(result, generation) => {
                if generation != self.generation
                    || !matches!(self.status, HardwareStatus::Reading(_))
                {
                    return (Task::none(), false);
                }
                self.status = match result {
                    Ok(account) if self.supported(&account.device_id).is_some() => {
                        HardwareStatus::Ready(account)
                    }
                    Ok(_) => HardwareStatus::Failed(HardwareSourceError::Disconnected),
                    Err(error) => HardwareStatus::Failed(error),
                };
                (Task::none(), true)
            }
        }
    }
}

pub fn view(source: &HardwareSource) -> Element<'_, HardwareMessage> {
    let reading = matches!(source.status, HardwareStatus::Reading(_));
    let mut devices = Column::new().spacing(8);
    let list = source
        .devices
        .as_ref()
        .map(|devices| devices.list.as_slice())
        .unwrap_or_default();
    if list.is_empty() {
        devices = devices.push(
            p1_regular("Connect and unlock a hardware wallet.").style(theme::text::secondary),
        );
    }
    for hw in list {
        let row = match hw {
            HardwareWallet::Supported {
                id,
                kind,
                fingerprint,
                alias,
                ..
            } => Row::new()
                .spacing(10)
                .align_y(Alignment::Center)
                .push(
                    p1_regular(format!(
                        "{kind} {fingerprint}{}",
                        alias
                            .as_ref()
                            .map(|alias| format!(" ({alias})"))
                            .unwrap_or_default()
                    ))
                    .width(Length::Fill),
                )
                .push(
                    button::secondary(None, "Read account")
                        .on_press_maybe((!reading).then(|| HardwareMessage::Read(id.clone()))),
                ),
            HardwareWallet::Locked {
                kind, pairing_code, ..
            } => Row::new().push(p1_regular(match pairing_code {
                Some(code) => format!("{kind}: confirm pairing code {code} on the device"),
                None => format!("{kind}: unlock the device"),
            })),
            HardwareWallet::Unsupported { kind, .. } => Row::new().push(
                p1_regular(format!("{kind}: not supported for Split"))
                    .style(theme::text::secondary),
            ),
        };
        devices = devices.push(row);
    }

    let status: Element<HardwareMessage> = match &source.status {
        HardwareStatus::Idle => caption("Reading shares only this account's public key. The device is never asked to sign, and nothing is saved.")
            .style(theme::text::secondary)
            .into(),
        HardwareStatus::Reading(_) => p1_regular("Reading the account from the device…")
            .style(theme::text::secondary)
            .into(),
        HardwareStatus::Ready(account) => p1_bold(format!(
            "Ready to scan account {} of wallet {} ({}).",
            account.account, account.fingerprint, account.purpose
        ))
        .into(),
        HardwareStatus::Failed(error) => p1_regular(error_copy(*error))
            .style(theme::text::warning)
            .into(),
    };

    Column::new()
        .spacing(12)
        .push(p1_bold("Address type"))
        .push(
            iced::widget::pick_list(
                PURPOSES,
                Some(source.purpose),
                HardwareMessage::PurposeSelected,
            )
            .width(Length::Fill),
        )
        .push(p1_bold("Account"))
        .push(
            text_input("0", &source.account)
                .on_input(HardwareMessage::AccountEdited)
                .padding(10),
        )
        .push(p1_bold("Hardware wallet"))
        .push(devices)
        .push(status)
        .into()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::{bip32::Xpriv, secp256k1::Secp256k1, Psbt};
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    const SEED: [u8; 32] = [11; 32];

    fn master(network: Network) -> Xpriv {
        Xpriv::new_master(network, &SEED).unwrap()
    }

    fn xpub_at(network: Network, path: &DerivationPath) -> Xpub {
        let secp = Secp256k1::new();
        Xpub::from_priv(&secp, &master(network).derive_priv(&secp, path).unwrap())
    }

    fn fingerprint() -> Fingerprint {
        master(Network::Bitcoin).fingerprint(&Secp256k1::new())
    }

    #[derive(Debug)]
    struct FakeDevice {
        fingerprint: Fingerprint,
        network: Network,
        path_override: Option<DerivationPath>,
    }

    #[async_trait::async_trait]
    impl HWI for FakeDevice {
        fn device_kind(&self) -> async_hwi::DeviceKind {
            async_hwi::DeviceKind::Specter
        }
        async fn get_version(&self) -> Result<async_hwi::Version, async_hwi::Error> {
            Err(async_hwi::Error::UnimplementedMethod)
        }
        async fn get_master_fingerprint(&self) -> Result<Fingerprint, async_hwi::Error> {
            Ok(self.fingerprint)
        }
        async fn get_extended_pubkey(
            &self,
            path: &DerivationPath,
        ) -> Result<Xpub, async_hwi::Error> {
            Ok(xpub_at(
                self.network,
                self.path_override.as_ref().unwrap_or(path),
            ))
        }
        async fn register_wallet(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<[u8; 32]>, async_hwi::Error> {
            Err(async_hwi::Error::UnimplementedMethod)
        }
        async fn is_wallet_registered(&self, _: &str, _: &str) -> Result<bool, async_hwi::Error> {
            Err(async_hwi::Error::UnimplementedMethod)
        }
        async fn display_address(
            &self,
            _: &async_hwi::AddressScript,
        ) -> Result<(), async_hwi::Error> {
            Err(async_hwi::Error::UnimplementedMethod)
        }
        async fn sign_tx(&self, _: &mut Psbt) -> Result<(), async_hwi::Error> {
            panic!("the Split hardware source must never ask a device to sign");
        }
    }

    fn fake(network: Network, path_override: Option<&str>) -> Arc<dyn HWI + Send + Sync> {
        Arc::new(FakeDevice {
            fingerprint: fingerprint(),
            network,
            path_override: path_override.map(|p| p.parse().unwrap()),
        })
    }

    /// A fresh, empty directory. Nanoseconds alone collided when tests ran
    /// in parallel ("File exists"), so the name also carries the process id
    /// and a per-process counter.
    pub(crate) fn unique_temp_root(prefix: &str) -> std::path::PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}-{unique}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        root
    }

    fn temp_root() -> std::path::PathBuf {
        unique_temp_root("coincube-split-hw")
    }

    fn source_with(device: Arc<dyn HWI + Send + Sync>, root: &std::path::Path) -> HardwareSource {
        let mut source = HardwareSource::default();
        add_device(&mut source, device, root);
        source
    }

    /// Lists a supported fake device as `dev-1` so a `Read` would start a task.
    pub(crate) fn add_fake_device(source: &mut HardwareSource, root: &std::path::Path) {
        add_device(source, fake(Network::Bitcoin, None), root);
    }

    fn add_device(
        source: &mut HardwareSource,
        device: Arc<dyn HWI + Send + Sync>,
        root: &std::path::Path,
    ) {
        source.set_root(CoincubeDirectory::new(root.to_path_buf()));
        source
            .devices
            .as_mut()
            .unwrap()
            .list
            .push(HardwareWallet::Supported {
                id: "dev-1".into(),
                device,
                kind: async_hwi::DeviceKind::Specter,
                fingerprint: fingerprint(),
                version: None,
                registered: None,
                alias: None,
            });
    }

    #[test]
    fn split_hardware_purpose_maps_to_standard_path_and_descriptor() {
        for (purpose, path, prefix) in [
            (Purpose::Bip84, "m/84'/0'/2'", "wpkh("),
            (Purpose::Bip49, "m/49'/0'/2'", "sh(wpkh("),
            (Purpose::Bip44, "m/44'/0'/2'", "pkh("),
        ] {
            assert_eq!(purpose.path(2).unwrap(), path.parse().unwrap());
            let xpub = xpub_at(Network::Bitcoin, &purpose.path(2).unwrap());
            let account =
                account_from_device("dev".into(), purpose, 2, fingerprint(), fingerprint(), xpub)
                    .unwrap();
            let origin = format!("[{}/{}]", fingerprint(), purpose.path(2).unwrap());
            let has_origin =
                |d: &str| d.contains(&origin) || d.contains(&origin.replace('\'', "h"));
            let external = account.external.canonical();
            let internal = account.internal.canonical();
            assert!(external.starts_with(prefix), "{}", external);
            assert!(has_origin(&external), "{}", external);
            assert!(external.contains("/0/*"), "{}", external);
            assert!(has_origin(&internal) && internal.contains("/1/*"));
        }
        // Taproot is not offered.
        assert_eq!(PURPOSES.len(), 3);
        assert!(PURPOSES.iter().all(|p| p.number() != 86));
    }

    #[test]
    fn split_hardware_refuses_wrong_network_depth_and_fingerprint() {
        let path = Purpose::Bip84.path(0).unwrap();
        let good = xpub_at(Network::Bitcoin, &path);
        let check = |listed, reported, xpub| {
            account_from_device("dev".into(), Purpose::Bip84, 0, listed, reported, xpub)
                .map(|_| ())
                .unwrap_err()
        };
        assert_eq!(
            check(
                fingerprint(),
                fingerprint(),
                xpub_at(Network::Testnet, &path)
            ),
            HardwareSourceError::Network
        );
        for wrong in ["m/84'/0'", "m/84'/0'/1'", "m/84'/0'/0'/0"] {
            assert_eq!(
                check(
                    fingerprint(),
                    fingerprint(),
                    xpub_at(Network::Bitcoin, &wrong.parse().unwrap())
                ),
                HardwareSourceError::Depth,
                "{wrong}"
            );
        }
        let other = Fingerprint::from([1, 2, 3, 4]);
        assert_eq!(
            check(other, fingerprint(), good),
            HardwareSourceError::Fingerprint
        );
        let mut relabeled = good;
        relabeled.parent_fingerprint = Fingerprint::default();
        assert_eq!(
            check(fingerprint(), fingerprint(), relabeled),
            HardwareSourceError::ParentFingerprint
        );
        relabeled.parent_fingerprint = fingerprint();
        assert_eq!(
            check(fingerprint(), fingerprint(), relabeled),
            HardwareSourceError::ParentFingerprint
        );
        assert_eq!(
            parse_account("2147483648"),
            Err(HardwareSourceError::Account)
        );
        assert_eq!(parse_account("-1"), Err(HardwareSourceError::Account));
        assert_eq!(parse_account(" 7 "), Ok(7));
    }

    #[tokio::test]
    async fn split_hardware_read_refuses_swapped_device_and_wrong_path() {
        let listed = Fingerprint::from([9, 9, 9, 9]);
        assert_eq!(
            read_account(
                fake(Network::Bitcoin, None),
                "d".into(),
                listed,
                Purpose::Bip84,
                0
            )
            .await
            .unwrap_err(),
            HardwareSourceError::Fingerprint
        );
        assert_eq!(
            read_account(
                fake(Network::Bitcoin, Some("m/84'/0'/5'")),
                "d".into(),
                fingerprint(),
                Purpose::Bip84,
                0
            )
            .await
            .unwrap_err(),
            HardwareSourceError::Depth
        );
        let account = read_account(
            fake(Network::Bitcoin, None),
            "d".into(),
            fingerprint(),
            Purpose::Bip49,
            3,
        )
        .await
        .unwrap();
        assert_eq!((account.account, account.fingerprint), (3, fingerprint()));
        // Redacted: no key material in Debug output.
        let debug = format!("{account:?}");
        assert!(
            !debug.contains("xpub") && !debug.contains("wpkh"),
            "{}",
            debug
        );
    }

    fn read(source: &mut HardwareSource) -> u64 {
        let _ = source.update(HardwareMessage::Read("dev-1".into()));
        assert!(matches!(source.status(), HardwareStatus::Reading(_)));
        source.generation
    }

    async fn loaded(generation: u64) -> HardwareMessage {
        let account = read_account(
            fake(Network::Bitcoin, None),
            "dev-1".into(),
            fingerprint(),
            Purpose::Bip84,
            0,
        )
        .await
        .map(Arc::new);
        HardwareMessage::Loaded(account, generation)
    }

    /// T2: a result from a read superseded by an edit and a second read must
    /// not satisfy the second read.
    #[tokio::test]
    async fn split_hardware_stale_result_does_not_satisfy_a_newer_read() {
        let root = temp_root();
        let mut source = source_with(fake(Network::Bitcoin, None), &root);
        let first = read(&mut source);
        let _ = source.update(HardwareMessage::AccountEdited("1".into()));
        let second = read(&mut source);
        assert_ne!(first, second);

        let (_, invalidated) = source.update(loaded(first).await);
        assert!(!invalidated);
        assert!(source.account().is_none());
        assert!(matches!(source.status(), HardwareStatus::Reading(_)));

        let _ = source.update(loaded(second).await);
        assert!(source.account().is_some());
        fs::remove_dir(root).unwrap();
    }

    #[tokio::test]
    async fn split_hardware_cancel_disconnect_and_edits_clear_state_without_datadir_writes() {
        let root = temp_root();
        let mut source = source_with(fake(Network::Bitcoin, None), &root);

        let generation = read(&mut source);
        let (_, invalidated) = source.update(loaded(generation).await);
        assert!(invalidated && source.account().is_some());

        // Cancel clears; a late result for the cleared generation is dropped.
        source.clear();
        assert!(source.account().is_none());
        let (_, invalidated) = source.update(loaded(generation).await);
        assert!(!invalidated && source.account().is_none());

        // Changing the account type or number clears the read account.
        let generation = read(&mut source);
        let _ = source.update(loaded(generation).await);
        let (_, invalidated) = source.update(HardwareMessage::PurposeSelected(Purpose::Bip44));
        assert!(invalidated && source.account().is_none());
        let generation = read(&mut source);
        let _ = source.update(loaded(generation).await);
        let (_, invalidated) = source.update(HardwareMessage::AccountEdited("1".into()));
        assert!(invalidated && source.account().is_none());

        // Disconnect: the device vanishes from the list.
        let generation = read(&mut source);
        let _ = source.update(loaded(generation).await);
        assert!(source.account().is_some());
        source.devices.as_mut().unwrap().list.clear();
        let (_, invalidated) = source.update(HardwareMessage::Devices(
            HardwareWalletMessage::Error("unplugged".into()),
        ));
        assert!(invalidated);
        assert!(source.account().is_none());
        assert!(matches!(
            source.status(),
            HardwareStatus::Failed(HardwareSourceError::Disconnected)
        ));

        // A result for a device that disconnected mid-read is not accepted.
        let mut source = source_with(fake(Network::Bitcoin, None), &root);
        let generation = read(&mut source);
        source.devices.as_mut().unwrap().list.clear();
        let _ = source.update(loaded(generation).await);
        assert!(source.account().is_none());

        assert!(!source.devices.as_ref().unwrap().persists_pairing());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir(root).unwrap();
    }
}
