use std::collections::HashSet;
use std::convert::From;
use std::sync::Arc;

use iced::{Subscription, Task};

use coincube_core::{descriptors::CoincubeDescriptor, miniscript::bitcoin::bip32::Fingerprint};

use coincube_ui::{
    component::form,
    widget::{modal, Element},
};

use crate::{
    app::{
        cache::Cache,
        error::Error,
        menu::Menu,
        message::Message,
        settings::{self, update_settings_file},
        state::{vault::export::VaultExportModal, State},
        view,
        wallet::Wallet,
        Config,
    },
    daemon::{Daemon, DaemonBackend},
    dir::CoincubeDirectory,
    export::{ImportExportMessage, ImportExportType},
    hw::{HardwareWallet, HardwareWalletConfig, HardwareWallets},
    services::connect::client::backend::api::WALLET_ALIAS_MAXIMUM_LENGTH,
};

enum Modal {
    None,
    RegisterWallet(RegisterWalletModal),
    ImportExport(VaultExportModal),
}

impl Modal {
    fn is_none(&self) -> bool {
        matches!(self, Modal::None)
    }
}

pub struct WalletSettingsState {
    data_dir: CoincubeDirectory,
    warning: Option<Error>,
    descriptor: CoincubeDescriptor,
    keys_aliases: Vec<(Fingerprint, form::Value<String>)>,
    wallet: Arc<Wallet>,
    wallet_alias: form::Value<String>,
    modal: Modal,
    processing: bool,
    updated: bool,
    _config: Arc<Config>,
}

impl WalletSettingsState {
    pub fn new(data_dir: CoincubeDirectory, wallet: Arc<Wallet>, config: Arc<Config>) -> Self {
        WalletSettingsState {
            data_dir,
            descriptor: wallet.main_descriptor.clone(),
            keys_aliases: Self::keys_aliases(&wallet),
            wallet_alias: form::Value {
                value: wallet.alias.clone().unwrap_or_default(),
                warning: None,
                valid: true,
            },
            wallet,
            warning: None,
            modal: Modal::None,
            processing: false,
            updated: false,
            _config: config,
        }
    }

    fn keys_aliases(wallet: &Wallet) -> Vec<(Fingerprint, form::Value<String>)> {
        let mut keys_aliases: Vec<(Fingerprint, form::Value<String>)> = wallet
            .keys_aliases
            .clone()
            .into_iter()
            .map(|(fg, name)| {
                (
                    fg,
                    form::Value {
                        value: name,
                        warning: None,
                        valid: true,
                    },
                )
            })
            .collect();

        for fingerprint in wallet.descriptor_keys().into_iter() {
            if !wallet.keys_aliases.contains_key(&fingerprint) {
                keys_aliases.push((fingerprint, form::Value::default()));
            }
        }

        keys_aliases.sort_by_key(|(fg1, _)| *fg1);
        keys_aliases
    }
}

impl State for WalletSettingsState {
    fn view<'a>(&'a self, menu: &'a Menu, cache: &'a Cache) -> Element<'a, view::Message> {
        let content = view::vault::settings::wallet_settings(
            menu,
            cache,
            &self.descriptor,
            self.wallet.id_fingerprint(),
            &self.wallet_alias,
            &self.keys_aliases,
            &self.wallet.provider_keys,
            self.processing,
            self.updated,
            !self.wallet.chain.is_blake2b(),
        );

        match &self.modal {
            Modal::None => content,
            Modal::RegisterWallet(m) => modal::Modal::new(content, m.view())
                .on_blur(Some(view::Message::Close))
                .into(),
            Modal::ImportExport(m) => m.view(content),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        match &self.modal {
            Modal::None => Subscription::none(),
            Modal::RegisterWallet(modal) => modal.subscription(),
            Modal::ImportExport(modal) => {
                if let Some(sub) = modal.subscription() {
                    sub.map(|m| {
                        Message::View(view::Message::Settings(
                            view::SettingsMessage::ImportExport(ImportExportMessage::Progress(m)),
                        ))
                    })
                } else {
                    Subscription::none()
                }
            }
        }
    }

    fn update(
        &mut self,
        daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        let Some(daemon) = daemon else {
            tracing::warn!("WalletSettingsState::update called without daemon");
            return Task::none();
        };
        match message {
            Message::WalletUpdated(res) => {
                self.processing = false;
                if let Modal::RegisterWallet(modal) = &mut self.modal {
                    modal.update(Some(daemon.clone()), cache, Message::WalletUpdated(res))
                } else {
                    match res {
                        Ok(wallet) => {
                            self.keys_aliases = Self::keys_aliases(&wallet);
                            self.wallet = wallet;
                            self.updated = true;
                            Task::none()
                        }
                        Err(e) => {
                            let err_msg = crate::user_error::report(&e);
                            self.warning = Some(e);
                            Task::done(Message::View(view::Message::ShowError(err_msg)))
                        }
                    }
                }
            }
            Message::View(view::Message::Settings(view::SettingsMessage::WalletAliasEdited(
                alias,
            ))) => {
                self.wallet_alias.valid = alias.len() < WALLET_ALIAS_MAXIMUM_LENGTH;
                self.wallet_alias.value = alias;
                Task::none()
            }
            Message::View(view::Message::Settings(
                view::SettingsMessage::FingerprintAliasEdited(fg, value),
            )) => {
                if let Some((_, name)) = self
                    .keys_aliases
                    .iter_mut()
                    .find(|(fingerprint, _)| fg == *fingerprint)
                {
                    name.value = value;
                }
                Task::none()
            }
            Message::View(view::Message::Settings(view::SettingsMessage::Save)) => {
                self.modal = Modal::None;
                self.processing = true;
                self.updated = false;
                Task::perform(
                    update_aliases(
                        self.data_dir.clone(),
                        self.wallet.clone(),
                        match self
                            .wallet
                            .alias
                            .as_ref()
                            .map(|a| *a == self.wallet_alias.value)
                        {
                            Some(true) => None,
                            Some(false) => Some(self.wallet_alias.value.clone()),
                            None => {
                                if self.wallet_alias.value.is_empty() {
                                    None
                                } else {
                                    Some(self.wallet_alias.value.clone())
                                }
                            }
                        },
                        self.keys_aliases
                            .iter()
                            .map(|(fg, name)| (*fg, name.value.to_owned()))
                            .collect(),
                        daemon,
                    ),
                    Message::WalletUpdated,
                )
            }
            Message::View(view::Message::Close) => {
                self.modal = Modal::None;
                Task::none()
            }
            Message::View(view::Message::Settings(view::SettingsMessage::RegisterWallet)) => {
                if self.wallet.chain.is_blake2b() {
                    self.warning = Some(Error::Unexpected(
                        "Hardware wallet registration is unavailable for Bitcoin Blake2b.".into(),
                    ));
                    return Task::none();
                }
                self.modal = Modal::RegisterWallet(RegisterWalletModal::new(
                    self.data_dir.clone(),
                    self.wallet.clone(),
                ));
                Task::none()
            }

            Message::View(view::Message::ImportExport(ImportExportMessage::UpdateAliases(
                aliases,
            ))) => {
                self.processing = true;
                self.updated = false;
                Task::perform(
                    update_aliases(
                        self.data_dir.clone(),
                        self.wallet.clone(),
                        None,
                        aliases.into_iter().map(|(fg, ks)| (fg, ks.name)).collect(),
                        daemon,
                    ),
                    Message::WalletUpdated,
                )
            }
            Message::View(view::Message::ImportExport(ImportExportMessage::Close)) => {
                if let Modal::ImportExport(_) = &self.modal {
                    self.modal = Modal::None;
                }
                Task::none()
            }
            Message::View(view::Message::ImportExport(m)) => {
                if let Modal::ImportExport(modal) = &mut self.modal {
                    modal.update(m)
                } else {
                    Task::none()
                }
            }
            Message::View(view::Message::Settings(view::SettingsMessage::ImportExport(m))) => {
                if let Modal::ImportExport(modal) = &mut self.modal {
                    modal.update(m)
                } else {
                    Task::none()
                }
            }
            Message::View(view::Message::Settings(
                view::SettingsMessage::ExportEncryptedDescriptor,
            )) => {
                if self.modal.is_none() {
                    let descriptor = self.wallet.main_descriptor.clone();
                    let modal = VaultExportModal::new(
                        Some(daemon),
                        ImportExportType::ExportEncryptedDescriptor(Box::new(descriptor)),
                    );
                    let launch = modal.launch(true);
                    self.modal = Modal::ImportExport(modal);
                    return launch;
                }
                Task::none()
            }
            _ => match &mut self.modal {
                Modal::RegisterWallet(m) => m.update(Some(daemon.clone()), cache, message),
                _ => Task::none(),
            },
        }
    }

    fn reload(
        &mut self,
        daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        wallet: Option<Arc<Wallet>>,
    ) -> Task<Message> {
        let Some(daemon) = daemon else {
            tracing::warn!("WalletSettingsState::reload called without daemon");
            return Task::none();
        };
        let Some(wallet) = wallet else {
            tracing::warn!("WalletSettingsState::reload called without wallet");
            return Task::none();
        };
        self.descriptor = wallet.main_descriptor.clone();
        self.keys_aliases = Self::keys_aliases(&wallet);
        self.wallet = wallet;
        Task::perform(
            async move { daemon.get_info().await.map_err(|e| e.into()) },
            Message::Info,
        )
    }
}

impl From<WalletSettingsState> for Box<dyn State> {
    fn from(s: WalletSettingsState) -> Box<dyn State> {
        Box::new(s)
    }
}

pub struct RegisterWalletModal {
    data_dir: CoincubeDirectory,
    wallet: Arc<Wallet>,
    warning: Option<Error>,
    chosen_hw: Option<usize>,
    hws: HardwareWallets,
    registered: HashSet<Fingerprint>,
    processing: bool,
}

impl RegisterWalletModal {
    pub fn new(data_dir: CoincubeDirectory, wallet: Arc<Wallet>) -> Self {
        let mut registered = HashSet::new();
        for hw in &wallet.hardware_wallets {
            registered.insert(hw.fingerprint);
        }
        Self {
            data_dir: data_dir.clone(),
            warning: None,
            chosen_hw: None,
            hws: HardwareWallets::new(data_dir, wallet.chain.bitcoin_network())
                .with_wallet(wallet.clone()),
            wallet,
            processing: false,
            registered,
        }
    }
}

impl RegisterWalletModal {
    fn view(&self) -> Element<'_, view::Message> {
        view::vault::settings::register_wallet_modal(
            &self.hws.list,
            self.processing,
            self.chosen_hw,
            &self.registered,
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        self.hws.refresh().map(Message::HardwareWallets)
    }

    fn update(
        &mut self,
        daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        _cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        let Some(daemon) = daemon else {
            tracing::warn!("RegisterWalletModal::update called without daemon");
            return Task::none();
        };
        match message {
            Message::View(view::Message::Reload) => {
                self.chosen_hw = None;
                self.warning = None;
                Task::none()
            }
            Message::HardwareWallets(msg) => match self.hws.update(msg) {
                Ok(cmd) => cmd.map(Message::HardwareWallets),
                Err(e) => {
                    let err: Error = e.into();
                    let err_msg = crate::user_error::report(&err);
                    self.warning = Some(err);
                    Task::done(Message::View(view::Message::ShowError(err_msg)))
                }
            },
            Message::WalletUpdated(res) => {
                self.processing = false;
                self.chosen_hw = None;
                match res {
                    Ok(wallet) => {
                        self.registered = HashSet::new();
                        for hw in &wallet.hardware_wallets {
                            self.registered.insert(hw.fingerprint);
                        }
                        self.wallet = wallet;
                    }
                    Err(e) => {
                        if !matches!(e, Error::HardwareWallet(async_hwi::Error::UserRefused)) {
                            let err_msg = crate::user_error::report(&e);
                            self.warning = Some(e);
                            return Task::done(Message::View(view::Message::ShowError(err_msg)));
                        }
                    }
                }
                Task::none()
            }
            Message::View(view::Message::SelectHardwareWallet(i)) => {
                if let Some(HardwareWallet::Supported {
                    fingerprint,
                    device,
                    ..
                }) = self.hws.list.get(i)
                {
                    self.chosen_hw = Some(i);
                    self.processing = true;
                    Task::perform(
                        register_wallet(
                            self.data_dir.clone(),
                            device.clone(),
                            *fingerprint,
                            self.wallet.clone(),
                            daemon,
                        ),
                        Message::WalletUpdated,
                    )
                } else {
                    Task::none()
                }
            }
            _ => Task::none(),
        }
    }
}

async fn register_wallet(
    data_dir: CoincubeDirectory,
    hw: std::sync::Arc<dyn async_hwi::HWI + Send + Sync>,
    fingerprint: Fingerprint,
    wallet: Arc<Wallet>,
    daemon: Arc<dyn Daemon + Sync + Send>,
) -> Result<Arc<Wallet>, Error> {
    if wallet.chain.is_blake2b() {
        return Err(Error::Unexpected(
            "Hardware wallet registration is unavailable for Bitcoin Blake2b.".into(),
        ));
    }
    let hmac = hw
        .register_wallet(&wallet.name, &wallet.main_descriptor.to_string())
        .await
        .map_err(Error::from)?;

    if let Some(hmac) = hmac {
        let kind = hw.device_kind().to_string();
        let hw_cfg = HardwareWalletConfig {
            kind: kind.clone(),
            token: hex::encode(hmac),
            fingerprint,
        };

        if daemon.backend() != DaemonBackend::RemoteBackend {
            let network_dir = data_dir.network_directory(wallet.chain);
            let wallet_id = wallet.id();
            update_settings_file(&network_dir, |mut settings| {
                if let Some(wallet_setting) = settings
                    .wallets
                    .iter_mut()
                    .find(|w| w.wallet_id() == wallet_id)
                {
                    if let Some(hw_config) = wallet_setting
                        .hardware_wallets
                        .iter_mut()
                        .find(|cfg| cfg.kind == kind && cfg.fingerprint == fingerprint)
                    {
                        *hw_config = hw_cfg.clone();
                    } else {
                        wallet_setting.hardware_wallets.push(hw_cfg.clone())
                    }
                }

                Some(settings)
            })
            .await?;
        }

        let mut wallet = wallet.as_ref().clone();
        if let Some(hw_config) = wallet
            .hardware_wallets
            .iter_mut()
            .find(|cfg| cfg.kind == kind && cfg.fingerprint == fingerprint)
        {
            *hw_config = hw_cfg.clone();
        } else {
            wallet.hardware_wallets.push(hw_cfg)
        }
        daemon
            .update_wallet_metadata(None, &wallet.keys_aliases, &wallet.hardware_wallets)
            .await?;
        return Ok(Arc::new(wallet));
    }

    Ok(wallet)
}

pub async fn update_aliases(
    data_dir: CoincubeDirectory,
    wallet: Arc<Wallet>,
    wallet_alias: Option<String>,
    keys_aliases: Vec<(Fingerprint, String)>,
    daemon: Arc<dyn Daemon + Sync + Send>,
) -> Result<Arc<Wallet>, Error> {
    let mut wallet = wallet.as_ref().clone();

    if let Some(wallet_alias) = wallet_alias.as_ref() {
        wallet = wallet.with_alias(Some(wallet_alias.clone()));
        let network_dir = data_dir.network_directory(wallet.chain);
        let wallet_id = wallet.id();
        update_settings_file(&network_dir, |mut settings| {
            if let Some(wallet_setting) = settings
                .wallets
                .iter_mut()
                .find(|w| w.wallet_id() == wallet_id)
            {
                wallet_setting.alias = Some(wallet_alias.clone());
            }

            Some(settings)
        })
        .await?;
    }

    if daemon.backend() != DaemonBackend::RemoteBackend {
        let network_dir = data_dir.network_directory(wallet.chain);
        let wallet_id = wallet.id();
        update_settings_file(&network_dir, |mut settings| {
            if let Some(wallet_setting) = settings
                .wallets
                .iter_mut()
                .find(|w| w.wallet_id() == wallet_id)
            {
                wallet_setting.keys = keys_aliases
                    .iter()
                    .map(|(master_fingerprint, name)| settings::KeySetting {
                        master_fingerprint: *master_fingerprint,
                        name: name.clone(),
                        provider_key: wallet.provider_keys.get(master_fingerprint).cloned(),
                        is_border_wallet: wallet
                            .border_wallet_fingerprints
                            .contains(master_fingerprint),
                        // Carried through verbatim: this rewrite is about
                        // aliases, and re-deriving the provenance from nothing
                        // would silently downgrade every key to unrecorded.
                        grid_seed_source: wallet
                            .border_wallet_grid_seed
                            .get(master_fingerprint)
                            .copied(),
                        replay_protected: wallet.replay_marks.get(master_fingerprint).copied(),
                        keychain_key_id: wallet.keychain_key_ids.get(master_fingerprint).copied(),
                    })
                    .collect();
            }

            Some(settings)
        })
        .await?;
    }

    wallet.keys_aliases = keys_aliases.into_iter().collect();

    daemon
        .update_wallet_metadata(wallet_alias, &wallet.keys_aliases, &wallet.hardware_wallets)
        .await?;

    Ok(Arc::new(wallet))
}

#[cfg(test)]
mod chain_tests {
    use super::*;
    use crate::chain::ChainId;

    #[tokio::test]
    async fn alias_updates_touch_only_exact_chain_directory() {
        let descriptor = crate::app::state::vault::test_support::unified::fixture().descriptor;
        for chain in [
            ChainId::BitcoinBlake2b,
            ChainId::BitcoinBlake2bTestnet4,
            ChainId::Bitcoin,
        ] {
            let root =
                std::env::temp_dir().join(format!("coincube-settings-{}", uuid::Uuid::new_v4()));
            let dir = CoincubeDirectory::new(root.clone());
            let wallet = Arc::new(Wallet::new(descriptor.clone()).with_chain(chain));
            let other = if chain.is_blake2b() {
                ChainId::from(chain.bitcoin_network())
            } else {
                ChainId::BitcoinBlake2b
            };
            let settings = settings::Settings {
                wallets: vec![settings::WalletSettings {
                    name: wallet.name.clone(),
                    alias: None,
                    descriptor_checksum: wallet.descriptor_checksum.clone(),
                    pinned_at: wallet.pinned_at,
                    keys: vec![],
                    hardware_wallets: vec![],
                    remote_backend_auth: None,
                    start_internal_bitcoind: None,
                    pending_rescan: None,
                    keychain_keys_recorded: false,
                }],
                ..Default::default()
            };
            let original = serde_json::to_vec(&settings).unwrap();
            for target in [chain, other] {
                let path = dir.network_directory(target);
                std::fs::create_dir_all(path.path()).unwrap();
                std::fs::write(path.path().join(settings::SETTINGS_FILE_NAME), &original).unwrap();
            }
            let daemon = Arc::new(crate::daemon::client::Coincubed::new(
                crate::utils::mock::Daemon::new(vec![]).run(),
            ));
            let fingerprint = Fingerprint::from([1, 2, 3, 4]);
            let updated = update_aliases(
                dir.clone(),
                wallet,
                Some("isolated".into()),
                vec![(fingerprint, "synthetic key".into())],
                daemon,
            )
            .await
            .unwrap();
            assert_eq!(updated.alias.as_deref(), Some("isolated"));
            let stored = settings::Settings::from_file(&dir.network_directory(chain)).unwrap();
            assert_eq!(stored.wallets[0].alias.as_deref(), Some("isolated"));
            assert_eq!(stored.wallets[0].keys[0].master_fingerprint, fingerprint);
            assert_eq!(
                std::fs::read(
                    dir.network_directory(other)
                        .path()
                        .join(settings::SETTINGS_FILE_NAME)
                )
                .unwrap(),
                original
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    /// Renaming a key rewrites the whole key list; the Keychain id must come
    /// through it, or the Pair panel would stop offering the renamed key.
    #[tokio::test]
    async fn alias_update_keeps_keychain_key_id() {
        use std::collections::HashMap;
        let descriptor = crate::app::state::vault::test_support::unified::fixture().descriptor;
        let root = std::env::temp_dir().join(format!("coincube-settings-{}", uuid::Uuid::new_v4()));
        let dir = CoincubeDirectory::new(root.clone());
        let phone = Fingerprint::from([1, 2, 3, 4]);
        let other = Fingerprint::from([5, 6, 7, 8]);
        let wallet = Arc::new(
            Wallet::new(descriptor)
                .with_chain(ChainId::Bitcoin)
                .with_keychain_keys(HashMap::from([(phone, 7)]), true),
        );
        let settings = settings::Settings {
            wallets: vec![settings::WalletSettings {
                name: wallet.name.clone(),
                alias: None,
                descriptor_checksum: wallet.descriptor_checksum.clone(),
                pinned_at: wallet.pinned_at,
                keys: vec![],
                hardware_wallets: vec![],
                remote_backend_auth: None,
                start_internal_bitcoind: None,
                pending_rescan: None,
                keychain_keys_recorded: true,
            }],
            ..Default::default()
        };
        let path = dir.network_directory(ChainId::Bitcoin);
        std::fs::create_dir_all(path.path()).unwrap();
        std::fs::write(
            path.path().join(settings::SETTINGS_FILE_NAME),
            serde_json::to_vec(&settings).unwrap(),
        )
        .unwrap();
        let daemon = Arc::new(crate::daemon::client::Coincubed::new(
            crate::utils::mock::Daemon::new(vec![]).run(),
        ));
        let updated = update_aliases(
            dir.clone(),
            wallet,
            None,
            vec![(phone, "Renamed phone".into()), (other, "Ledger".into())],
            daemon,
        )
        .await
        .unwrap();
        assert_eq!(updated.keychain_key_ids.get(&phone), Some(&7));

        let stored = settings::Settings::from_file(&path).unwrap();
        assert!(stored.wallets[0].keychain_keys_recorded);
        let keys: HashMap<_, _> = stored.wallets[0]
            .keys
            .iter()
            .map(|k| (k.master_fingerprint, k))
            .collect();
        assert_eq!(keys[&phone].name, "Renamed phone");
        assert_eq!(keys[&phone].keychain_key_id, Some(7));
        assert_eq!(keys[&other].keychain_key_id, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fork_hardware_registration_message_is_refused() {
        let wallet = Arc::new(
            Wallet::new(crate::app::state::vault::test_support::unified::fixture().descriptor)
                .with_chain(ChainId::BitcoinBlake2b),
        );
        let dir = CoincubeDirectory::new(
            std::env::temp_dir().join(format!("coincube-hw-refusal-{}", uuid::Uuid::new_v4())),
        );
        let mut state = WalletSettingsState::new(dir.clone(), wallet, Arc::new(Config::new(false)));
        let daemon = Arc::new(crate::daemon::client::Coincubed::new(
            crate::utils::mock::Daemon::new(vec![]).run(),
        ));
        let _task = state.update(
            Some(daemon),
            &Cache::default(),
            Message::View(view::Message::Settings(
                view::SettingsMessage::RegisterWallet,
            )),
        );
        assert!(state.warning.is_some());
        assert!(!matches!(state.modal, Modal::RegisterWallet(_)));
        assert!(!dir.path().exists());
    }
}
