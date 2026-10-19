//! Public-descriptor Bitcoin wallet setup and runtime for one Cube.
//!
//! The Cube's own seed and recovery kit remain separate from the imported
//! wallet's signers. This panel never constructs a Vault wallet or a Vault
//! `SpendTx` from the imported descriptors.

use std::{
    convert::TryFrom,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use coincube_core::wallet_descriptor::{ImportedWalletDescriptor, WalletDescriptor};
use coincube_ui::{
    component::{button, text::*},
    theme,
    widget::{Column, ColumnExt, Element, Row},
};
use coincubed::{
    commands::CoinStatus,
    config::{BitcoinBackend, BitcoinConfig, Config, ElectrumConfig, EsploraConfig},
};
use iced::{widget::text_input, Subscription, Task};

use crate::{
    app::{
        cache::Cache,
        menu::Menu,
        message::Message,
        settings::{self, BitcoinImport, BitcoinImportStage, ImportedWalletAttachment},
        state::State,
        view,
    },
    chain::ChainId,
    daemon::{embedded::EmbeddedDaemon, model, Daemon},
    dir::NetworkDirectory,
    services::{bitcoin_import, coincube::CoincubeClient},
    wallet_source_form::{self, Source, SourceFormCopy, WalletSourceForm},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Connect,
    Public,
    Esplora,
    Electrum,
}

impl Provider {
    fn label(self) -> &'static str {
        match self {
            Self::Connect => "COINCUBE Connect",
            Self::Public => "Public Esplora (mempool.space)",
            Self::Esplora => "My Esplora server",
            Self::Electrum => "My Electrum server",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ImportMessage {
    Source(wallet_source_form::Message),
    BirthdayEdited(String),
    ScanFromBeginning(bool),
    LookaheadEdited(String),
    ProviderSelected(Provider),
    EndpointEdited(String),
    Review,
    Edit,
    Confirm,
    Retry,
    Refresh,
    Receive,
    CopyAddress,
}

#[derive(Debug)]
pub enum ImportEvent {
    Start,
    Installed(u64, Result<(), String>),
    Resumed(u64, Result<Option<settings::WalletId>, String>),
    Opened(u64, Result<Arc<dyn Daemon + Send + Sync>, String>),
    Refreshed(
        u64,
        Result<
            (
                model::GetInfoResult,
                model::ListCoinsResult,
                Vec<model::HistoryTransaction>,
                BitcoinImportStage,
            ),
            String,
        >,
    ),
    Address(u64, Result<model::GetAddressResult, String>),
}

pub struct ImportedBitcoinPanel {
    directory: NetworkDirectory,
    cube_id: String,
    import: BitcoinImport,
    source: WalletSourceForm,
    birthday: String,
    scan_from_beginning: bool,
    lookahead: String,
    provider: Provider,
    endpoint: String,
    review: Option<ImportedWalletAttachment>,
    review_connect_epoch: Option<u64>,
    daemon: Option<Arc<dyn Daemon + Send + Sync>>,
    generation: u64,
    generation_gate: Arc<AtomicU64>,
    busy: bool,
    error: Option<String>,
    info: Option<model::GetInfoResult>,
    coins: Option<model::ListCoinsResult>,
    history: Vec<model::HistoryTransaction>,
    address: Option<model::GetAddressResult>,
}

impl ImportedBitcoinPanel {
    pub fn new(directory: NetworkDirectory, cube_id: String, import: BitcoinImport) -> Self {
        let review = import.attachment().cloned();
        Self {
            directory,
            cube_id,
            import,
            source: WalletSourceForm::default(),
            birthday: String::new(),
            scan_from_beginning: true,
            lookahead: "200".into(),
            provider: Provider::Public,
            endpoint: String::new(),
            review,
            review_connect_epoch: None,
            daemon: None,
            generation: 1,
            generation_gate: Arc::new(AtomicU64::new(1)),
            busy: false,
            error: None,
            info: None,
            coins: None,
            history: Vec::new(),
            address: None,
        }
    }

    pub fn set_hardware_root(&mut self, root: crate::dir::CoincubeDirectory) {
        self.source.set_hardware_root(root);
    }

    pub fn is_ready(&self) -> bool {
        self.import.is_ready()
            && self.info.as_ref().is_some_and(|info| {
                info.imported_wallet.as_ref().is_some_and(|status| {
                    status.history_complete
                        && status.chain_id == ChainId::Bitcoin
                        && self.import.active_attachment().is_some_and(|wallet| {
                            wallet.wallet_id.descriptor_checksum
                                == format!("imported-v1-{}", status.wallet_identity)
                                && wallet.birthday == status.birthday
                                && status.initial_lookahead >= wallet.initial_lookahead
                        })
                })
            })
    }

    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.generation_gate
            .store(self.generation, Ordering::SeqCst);
        self.review = None;
        self.review_connect_epoch = None;
        self.error = None;
    }

    fn parse_review(&self) -> Result<ImportedWalletAttachment, String> {
        let (receive, change) = if self.source.source() == Source::Hardware {
            let account = self
                .source
                .hardware_account()
                .ok_or("Connect and read a Bitcoin account from your hardware wallet first.")?;
            (
                account.external.canonical(),
                Some(account.internal.canonical()),
            )
        } else {
            let change = self.source.internal().trim();
            (
                self.source.external().trim().to_owned(),
                (!change.is_empty()).then(|| change.to_owned()),
            )
        };
        let descriptor =
            ImportedWalletDescriptor::parse(&receive, change.as_deref(), ChainId::Bitcoin)
                .map_err(|e| e.to_string())?;
        let birthday = if self.scan_from_beginning {
            0
        } else {
            let date = chrono::NaiveDate::parse_from_str(self.birthday.trim(), "%Y-%m-%d")
                .map_err(|_| "Enter the wallet creation date in YYYY-MM-DD format.".to_string())?;
            if date < chrono::NaiveDate::from_ymd_opt(2009, 1, 3).unwrap()
                || date > chrono::Utc::now().date_naive()
            {
                return Err(
                    "Choose a date from January 3, 2009 through today, or scan from the beginning."
                        .into(),
                );
            }
            let timestamp = date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
            u32::try_from(timestamp).map_err(|_| "The wallet creation date is out of range.")?
        };
        let lookahead = self.lookahead.trim().parse::<u32>().map_err(|_| {
            "Enter an initial discovery window between 1 and 10,000 addresses per branch."
                .to_string()
        })?;
        ImportedWalletAttachment::new(descriptor, ChainId::Bitcoin, birthday, lookahead)
    }

    fn backend(&self, client: Option<&CoincubeClient>) -> Result<BitcoinBackend, String> {
        let backend = match self.provider {
            Provider::Connect => {
                let token = client
                    .and_then(CoincubeClient::token)
                    .ok_or("Sign in to Connect or choose another Bitcoin server.")?;
                BitcoinBackend::Esplora(crate::installer::connect_esplora_config(
                    ChainId::Bitcoin,
                    token,
                ))
            }
            Provider::Public => BitcoinBackend::Esplora(EsploraConfig {
                addr: crate::installer::public_esplora_url(
                    coincube_core::miniscript::bitcoin::Network::Bitcoin,
                ),
                token: None,
                fallback_addr: crate::installer::public_esplora_fallback_url(
                    coincube_core::miniscript::bitcoin::Network::Bitcoin,
                ),
                fallback_token: None,
                secondary_fallback_addr: None,
                secondary_fallback_token: None,
            }),
            Provider::Esplora => {
                let endpoint = self.endpoint.trim();
                if !endpoint.starts_with("https://") && !endpoint.starts_with("http://localhost") {
                    return Err("Enter an HTTPS Esplora URL, or a localhost HTTP URL.".into());
                }
                BitcoinBackend::Esplora(EsploraConfig {
                    addr: endpoint.into(),
                    token: None,
                    fallback_addr: None,
                    fallback_token: None,
                    secondary_fallback_addr: None,
                    secondary_fallback_token: None,
                })
            }
            Provider::Electrum => {
                let endpoint = self.endpoint.trim();
                if !endpoint.starts_with("ssl://") && !endpoint.starts_with("tcp://127.0.0.1") {
                    return Err("Enter an SSL Electrum URL, or a local TCP URL.".into());
                }
                BitcoinBackend::Electrum(ElectrumConfig {
                    addr: endpoint.into(),
                    validate_domain: true,
                })
            }
        };
        Ok(backend)
    }

    fn config(
        &self,
        wallet: &ImportedWalletAttachment,
        client: Option<&CoincubeClient>,
    ) -> Result<Config, String> {
        let backend = self.backend(client)?;
        let interval = if self.provider == Provider::Public {
            coincubed::config::ESPLORA_POLL_INTERVAL_SECS
        } else {
            30
        };
        Ok(Config::new(
            BitcoinConfig::new(ChainId::Bitcoin, Duration::from_secs(interval)),
            Some(backend),
            log::LevelFilter::Info,
            WalletDescriptor::Imported(wallet.descriptor.clone()),
            self.directory.coincubed_data_directory(&wallet.wallet_id),
        ))
    }

    pub fn update_view(
        &mut self,
        message: ImportMessage,
        client: Option<CoincubeClient>,
        connect_epoch: u64,
    ) -> Task<Message> {
        if self.busy {
            return Task::none();
        }
        if matches!(&self.import.stage, BitcoinImportStage::Installing { .. })
            && matches!(
                &message,
                ImportMessage::Source(_)
                    | ImportMessage::BirthdayEdited(_)
                    | ImportMessage::ScanFromBeginning(_)
                    | ImportMessage::LookaheadEdited(_)
                    | ImportMessage::Edit
                    | ImportMessage::Review
            )
        {
            self.error = Some("This interrupted import is bound to the reviewed wallet. Retry it or return to your Cube.".into());
            return Task::none();
        }
        match message {
            ImportMessage::Source(message) => {
                let task = match message {
                    wallet_source_form::Message::SourceSelected(source) => {
                        if self.source.select_source(source) {
                            self.invalidate();
                        }
                        Task::none()
                    }
                    wallet_source_form::Message::ExternalEdited(value) => {
                        self.source.edit_external(value);
                        self.invalidate();
                        Task::none()
                    }
                    wallet_source_form::Message::InternalEdited(value) => {
                        self.source.edit_internal(value);
                        self.invalidate();
                        Task::none()
                    }
                    wallet_source_form::Message::Hardware(message) => {
                        let (task, invalidated) = self.source.update_hardware(message);
                        if invalidated {
                            self.invalidate();
                        }
                        task
                    }
                };
                task.map(|m| {
                    Message::View(view::Message::ImportedBitcoin(ImportMessage::Source(m)))
                })
            }
            ImportMessage::BirthdayEdited(value) => {
                self.birthday = value;
                self.invalidate();
                Task::none()
            }
            ImportMessage::ScanFromBeginning(value) => {
                self.scan_from_beginning = value;
                self.invalidate();
                Task::none()
            }
            ImportMessage::LookaheadEdited(value) => {
                self.lookahead = value;
                self.invalidate();
                Task::none()
            }
            ImportMessage::ProviderSelected(value) => {
                self.provider = value;
                if matches!(&self.import.stage, BitcoinImportStage::Installing { .. }) {
                    self.review_connect_epoch =
                        (value == Provider::Connect).then_some(connect_epoch);
                    self.error = None;
                } else {
                    self.invalidate();
                }
                Task::none()
            }
            ImportMessage::EndpointEdited(value) => {
                self.endpoint = value;
                if matches!(&self.import.stage, BitcoinImportStage::Installing { .. }) {
                    self.error = None;
                } else {
                    self.invalidate();
                }
                Task::none()
            }
            ImportMessage::Review => {
                match self.parse_review().and_then(|wallet| {
                    self.backend(client.as_ref())?;
                    Ok(wallet)
                }) {
                    Ok(wallet) => {
                        self.review = Some(wallet);
                        self.review_connect_epoch =
                            (self.provider == Provider::Connect).then_some(connect_epoch);
                    }
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            ImportMessage::Edit => {
                self.review = None;
                self.error = None;
                Task::none()
            }
            ImportMessage::Confirm | ImportMessage::Retry => {
                if self.provider == Provider::Connect
                    && self
                        .review_connect_epoch
                        .is_some_and(|epoch| epoch != connect_epoch)
                {
                    self.invalidate();
                    self.error = Some("Your Connect session changed. Review the Bitcoin server again before importing.".into());
                    return Task::none();
                }
                if self.busy || self.import.is_ready() {
                    return Task::none();
                }
                let Some(wallet) = self.review.clone() else {
                    self.error = Some("Review a public Bitcoin wallet before importing it.".into());
                    return Task::none();
                };
                let config = match self.config(&wallet, client.as_ref()) {
                    Ok(config) => config,
                    Err(error) => {
                        self.error = Some(error);
                        return Task::none();
                    }
                };
                self.busy = true;
                self.error = None;
                let generation = self.generation;
                let directory = self.directory.clone();
                let cube_id = self.cube_id.clone();
                let operation_id = self.import.operation_id;
                Task::perform(
                    async move {
                        bitcoin_import::install(&directory, &cube_id, operation_id, wallet, config)
                            .await
                            .map(|_| ())
                    },
                    move |result| {
                        Message::ImportedBitcoin(ImportEvent::Installed(generation, result))
                    },
                )
            }
            ImportMessage::Refresh => {
                if self.busy {
                    Task::none()
                } else if self.daemon.is_some() {
                    self.refresh()
                } else {
                    self.open_daemon()
                }
            }
            ImportMessage::Receive => {
                if !self.is_ready() {
                    self.error =
                        Some("Wait for the complete Bitcoin history scan before receiving.".into());
                    return Task::none();
                }
                let Some(daemon) = self.daemon.clone() else {
                    return Task::none();
                };
                let generation = self.generation;
                Task::perform(
                    async move { daemon.get_new_address().await.map_err(|e| e.to_string()) },
                    move |result| {
                        Message::ImportedBitcoin(ImportEvent::Address(generation, result))
                    },
                )
            }
            ImportMessage::CopyAddress => {
                self.address.as_ref().map_or_else(Task::none, |address| {
                    iced::clipboard::write(address.address.to_string())
                })
            }
        }
    }

    pub fn update_event(&mut self, event: ImportEvent) -> Task<Message> {
        match event {
            ImportEvent::Start => match &self.import.stage {
                BitcoinImportStage::PendingSource => Task::none(),
                BitcoinImportStage::Installing { .. } => {
                    let generation = self.generation;
                    let directory = self.directory.clone();
                    let cube_id = self.cube_id.clone();
                    let operation_id = self.import.operation_id;
                    self.busy = true;
                    Task::perform(
                        async move { bitcoin_import::resume(&directory, &cube_id, operation_id).await },
                        move |result| {
                            Message::ImportedBitcoin(ImportEvent::Resumed(generation, result))
                        },
                    )
                }
                BitcoinImportStage::Syncing { .. } | BitcoinImportStage::Ready { .. } => {
                    self.open_daemon()
                }
            },
            ImportEvent::Installed(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok(()) => {
                        if let Some(wallet) = self.review.clone() {
                            self.import.stage = BitcoinImportStage::Syncing { wallet };
                        }
                        self.open_daemon()
                    }
                    Err(error) => {
                        self.error = Some(error);
                        Task::none()
                    }
                }
            }
            ImportEvent::Resumed(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok(Some(_)) => {
                        if let Some(wallet) = self.review.clone() {
                            self.import.stage = BitcoinImportStage::Syncing { wallet };
                        }
                        self.open_daemon()
                    }
                    Ok(None) => {
                        self.error = Some(
                            "Review the Bitcoin server and confirm this interrupted import again."
                                .into(),
                        );
                        Task::none()
                    }
                    Err(error) => {
                        self.error = Some(error);
                        Task::none()
                    }
                }
            }
            ImportEvent::Opened(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok(daemon) => {
                        self.daemon = Some(daemon);
                        self.refresh()
                    }
                    Err(error) => {
                        self.error = Some(error);
                        Task::none()
                    }
                }
            }
            ImportEvent::Refreshed(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok((info, coins, history, stage)) => {
                        self.info = Some(info);
                        self.coins = Some(coins);
                        self.history = history;
                        self.import.stage = stage;
                        self.error = None;
                    }
                    Err(error) => {
                        self.info = None;
                        self.coins = None;
                        self.history.clear();
                        self.address = None;
                        self.error = Some(error);
                    }
                }
                Task::none()
            }
            ImportEvent::Address(generation, result) if generation == self.generation => {
                match result {
                    Ok(address) => self.address = Some(address),
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            ImportEvent::Opened(_, Ok(daemon)) => Task::perform(
                async move {
                    if let Err(error) = daemon.stop().await {
                        log::error!("Could not stop stale imported Bitcoin daemon: {error}");
                    }
                },
                |_| Message::Tick,
            ),
            _ => Task::none(),
        }
    }

    fn open_daemon(&mut self) -> Task<Message> {
        let Some(wallet) = self.import.active_attachment() else {
            return Task::none();
        };
        let path = self
            .directory
            .coincubed_data_directory(&wallet.wallet_id)
            .path()
            .join("daemon.toml");
        let generation = self.generation;
        let gate = self.generation_gate.clone();
        self.busy = true;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let config = Config::from_file(Some(path)).map_err(|e| e.to_string())?;
                    let daemon = EmbeddedDaemon::start(config)
                        .map(|d| Arc::new(d) as Arc<dyn Daemon + Send + Sync>)
                        .map_err(|e| e.to_string())?;
                    if gate.load(Ordering::SeqCst) != generation {
                        tokio::runtime::Handle::current()
                            .block_on(async { daemon.stop().await })
                            .map_err(|e| e.to_string())?;
                        return Err("The Cube was closed before the wallet opened.".into());
                    }
                    Ok(daemon)
                })
                .await
                .map_err(|e| e.to_string())?
            },
            move |result| Message::ImportedBitcoin(ImportEvent::Opened(generation, result)),
        )
    }

    fn refresh(&mut self) -> Task<Message> {
        if self.busy {
            return Task::none();
        }
        let Some(daemon) = self.daemon.clone() else {
            return Task::none();
        };
        let generation = self.generation;
        let directory = self.directory.clone();
        let cube_id = self.cube_id.clone();
        let operation_id = self.import.operation_id;
        self.busy = true;
        Task::perform(
            async move {
                daemon.request_sync().await.map_err(|e| e.to_string())?;
                let info = daemon.get_info().await.map_err(|e| e.to_string())?;
                bitcoin_import::reconcile_sync(&directory, &cube_id, operation_id, &info).await?;
                let stage = settings::Settings::from_file(&directory)
                    .map_err(|e| e.to_string())?
                    .cubes
                    .into_iter()
                    .find(|cube| cube.id == cube_id)
                    .and_then(|cube| cube.bitcoin_import)
                    .filter(|import| import.operation_id == operation_id)
                    .ok_or("The imported wallet is no longer attached to this Cube.")?
                    .stage;
                let coins = daemon
                    .list_coins(&[CoinStatus::Confirmed, CoinStatus::Unconfirmed], &[])
                    .await
                    .map_err(|e| e.to_string())?;
                let history = daemon
                    .list_history_txs(0, u32::MAX, 20)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok((info, coins, history, stage))
            },
            move |result| Message::ImportedBitcoin(ImportEvent::Refreshed(generation, result)),
        )
    }

    pub fn stop(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.generation_gate
            .store(self.generation, Ordering::SeqCst);
        self.busy = false;
        self.source.clear_transient();
        if let Some(daemon) = self.daemon.take() {
            let runtime = tokio::runtime::Handle::try_current().ok();
            let result = std::thread::spawn(move || {
                if let Some(handle) = runtime {
                    handle
                        .block_on(async { daemon.stop().await })
                        .map_err(|error| error.to_string())
                } else {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| error.to_string())?
                        .block_on(async { daemon.stop().await })
                        .map_err(|error| error.to_string())
                }
            })
            .join();
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log::error!("Could not stop imported Bitcoin daemon: {error}"),
                Err(_) => log::error!("Imported Bitcoin daemon shutdown panicked"),
            }
        }
    }

    fn setup_view(&self) -> Element<'_, view::Message> {
        let mut content = Column::new()
            .spacing(16)
            .push(h2("Import a Bitcoin wallet"))
            .push(p1_regular("This Cube keeps its own unlock and backup. The imported wallet's signing keys stay with their original seed or devices."));
        if self.review.is_none() {
            let beginning = if self.scan_from_beginning {
                button::primary(None, "Scan from beginning")
            } else {
                button::secondary(None, "Scan from beginning")
            };
            let dated = if self.scan_from_beginning {
                button::secondary(None, "Use wallet creation date")
            } else {
                button::primary(None, "Use wallet creation date")
            };
            content = content
                .push(wallet_source_form::view(&self.source, &SourceFormCopy::import()).map(|m| {
                    view::Message::ImportedBitcoin(ImportMessage::Source(m))
                }))
                .push(p1_bold("Wallet history starts"))
                .push(Row::new().spacing(10)
                    .push(beginning
                        .on_press(view::Message::ImportedBitcoin(ImportMessage::ScanFromBeginning(true))))
                    .push(dated
                        .on_press(view::Message::ImportedBitcoin(ImportMessage::ScanFromBeginning(false)))))
                .push_maybe((!self.scan_from_beginning).then(|| {
                    text_input("YYYY-MM-DD", &self.birthday)
                        .on_input(|s| view::Message::ImportedBitcoin(ImportMessage::BirthdayEdited(s)))
                        .padding(10)
                }))
                .push(caption(if self.scan_from_beginning {
                    "Scanning from the beginning includes all Bitcoin history and can take longer."
                } else {
                    "Choose the date the original wallet was created. An incorrect later date can miss older funds."
                }))
                .push(p1_bold("Initial address window per branch"))
                .push(text_input("200", &self.lookahead)
                    .on_input(|s| view::Message::ImportedBitcoin(ImportMessage::LookaheadEdited(s)))
                    .padding(10))
                .push(p1_bold("Bitcoin server"));
            let mut providers = Row::new().spacing(8);
            for provider in [
                Provider::Connect,
                Provider::Public,
                Provider::Esplora,
                Provider::Electrum,
            ] {
                let button = if self.provider == provider {
                    button::primary(None, provider.label())
                } else {
                    button::secondary(None, provider.label())
                };
                providers = providers.push(button.on_press(view::Message::ImportedBitcoin(
                    ImportMessage::ProviderSelected(provider),
                )));
            }
            content = content.push(providers);
            if matches!(self.provider, Provider::Esplora | Provider::Electrum) {
                content = content.push(
                    text_input("Server URL", &self.endpoint)
                        .on_input(|s| {
                            view::Message::ImportedBitcoin(ImportMessage::EndpointEdited(s))
                        })
                        .padding(10),
                );
            }
            content = content.push(
                button::primary(None, "Review wallet")
                    .on_press(view::Message::ImportedBitcoin(ImportMessage::Review)),
            );
        } else if let Some(wallet) = &self.review {
            let retry = matches!(&self.import.stage, BitcoinImportStage::Installing { .. });
            content = content
                .push(p1_bold("Review public wallet"))
                .push(p1_regular(format!("Wallet identity: {}", wallet.wallet_id.descriptor_checksum)))
                .push(p1_regular(format!("Receive: {}", wallet.descriptor.receive_descriptor())))
                .push(p1_regular(format!("Change: {}", wallet.descriptor.change_descriptor())))
                .push(p1_regular(format!("History starts: {} · Initial window: {} addresses per branch",
                    if wallet.birthday == 0 { "Beginning".to_string() } else {
                        chrono::DateTime::from_timestamp(i64::from(wallet.birthday), 0)
                            .map(|date| date.format("%Y-%m-%d").to_string())
                            .unwrap_or_else(|| "Invalid date".into())
                    }, wallet.initial_lookahead)))
                .push(caption("A full history scan must finish before the balance can be trusted. The Cube backup must be refreshed after this wallet is attached."))
                .push(p1_bold("Bitcoin server"))
                .push(p1_regular(self.provider.label()));
            if retry {
                content = content.push(caption("The wallet identity is locked to this Cube. Choose the Bitcoin server to finish an interrupted import."));
                let mut providers = Row::new().spacing(8);
                for provider in [
                    Provider::Connect,
                    Provider::Public,
                    Provider::Esplora,
                    Provider::Electrum,
                ] {
                    let option = if self.provider == provider {
                        button::primary(None, provider.label())
                    } else {
                        button::secondary(None, provider.label())
                    };
                    providers = providers.push(option.on_press_maybe((!self.busy).then_some(
                        view::Message::ImportedBitcoin(ImportMessage::ProviderSelected(provider)),
                    )));
                }
                content = content.push(providers);
                if matches!(self.provider, Provider::Esplora | Provider::Electrum) {
                    content = content.push(
                        text_input("Server URL", &self.endpoint)
                            .on_input(|value| {
                                view::Message::ImportedBitcoin(ImportMessage::EndpointEdited(value))
                            })
                            .padding(10),
                    );
                }
            }
            if matches!(self.provider, Provider::Esplora | Provider::Electrum) {
                content = content.push(caption(&self.endpoint));
            }
            if self.provider == Provider::Public {
                content = content.push(caption("Public server: mempool.space, with blockstream.info fallback. This reveals wallet addresses to those providers."));
            }
            if self.provider == Provider::Connect {
                content = content.push(caption("Connect server: COINCUBE's authenticated Bitcoin proxy, with public fallback servers."));
            }
            content = content.push(
                Row::new()
                    .spacing(10)
                    .push(button::secondary(None, "Edit wallet").on_press_maybe(
                        (!retry).then_some(view::Message::ImportedBitcoin(ImportMessage::Edit)),
                    ))
                    .push(
                        button::primary(
                            None,
                            if self.busy {
                                "Importing…"
                            } else {
                                if retry {
                                    "Retry import"
                                } else {
                                    "Confirm import"
                                }
                            },
                        )
                        .on_press_maybe(
                            (!self.busy)
                                .then_some(view::Message::ImportedBitcoin(ImportMessage::Confirm)),
                        ),
                    ),
            );
        }
        if let Some(error) = &self.error {
            content = content.push(p1_regular(error).style(theme::text::error));
        }
        content.into()
    }

    fn wallet_view(&self) -> Element<'_, view::Message> {
        let mut content = Column::new().spacing(16).push(h2("Bitcoin wallet"));
        let ready = self.is_ready();
        if let Some(info) = &self.info {
            if let Some(status) = &info.imported_wallet {
                content = content.push(p1_regular(format!(
                    "{} · receive {} · change {} addresses checked",
                    if ready {
                        "History complete"
                    } else {
                        "Scanning history"
                    },
                    status.receive_scanned,
                    status.change_scanned,
                )));
            }
        }
        if ready {
            if let Some(coins) = &self.coins {
                let balance: u64 = coins
                    .coins
                    .iter()
                    .filter(|coin| coin.spend_info.is_none())
                    .map(|coin| coin.amount.to_sat())
                    .sum();
                content = content.push(h3(format!("{} sats", balance)));
            }
            content = content
                .push(p1_regular("The signing keys remain in your original wallet or hardware devices. Your Cube recovery seed cannot sign for this Bitcoin wallet."))
                .push(p1_regular("Update the Cube Recovery Kit now that this wallet is attached."))
                .push(Row::new().spacing(10)
                    .push(button::primary(None, "Receive").on_press(view::Message::ImportedBitcoin(ImportMessage::Receive)))
                    .push(button::secondary(None, "Refresh").on_press(view::Message::ImportedBitcoin(ImportMessage::Refresh))));
            if let Some(address) = &self.address {
                content = content.push(p1_regular(address.address.to_string())).push(
                    button::secondary(None, "Copy address")
                        .on_press(view::Message::ImportedBitcoin(ImportMessage::CopyAddress)),
                );
            }
            content = content.push(p1_bold("Recent transactions"));
            if self.history.is_empty() {
                content = content.push(caption("No transactions found in the completed scan."));
            } else {
                for tx in self.history.iter().take(20) {
                    content = content.push(caption(tx.txid.to_string()));
                }
            }
        } else {
            content = content
                .push(p1_regular(
                    "Checking Bitcoin history. A partial scan is not a zero balance.",
                ))
                .push(
                    button::secondary(None, "Check progress")
                        .on_press(view::Message::ImportedBitcoin(ImportMessage::Refresh)),
                );
        }
        if let Some(error) = &self.error {
            content = content.push(p1_regular(error).style(theme::text::error));
        }
        content.into()
    }
}

impl State for ImportedBitcoinPanel {
    fn view<'a>(&'a self, menu: &'a Menu, cache: &'a Cache) -> Element<'a, view::Message> {
        let body = if self.import.active_attachment().is_some() {
            self.wallet_view()
        } else {
            self.setup_view()
        };
        view::dashboard(menu, cache, body)
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subscriptions = Vec::new();
        if self.review.is_none() && !self.busy && self.import.active_attachment().is_none() {
            subscriptions.push(
                self.source.subscription().map(|m| {
                    Message::View(view::Message::ImportedBitcoin(ImportMessage::Source(m)))
                }),
            );
        }
        if self.import.active_attachment().is_some() {
            let interval = if self.is_ready() { 120 } else { 20 };
            subscriptions
                .push(iced::time::every(Duration::from_secs(interval)).map(|_| {
                    Message::View(view::Message::ImportedBitcoin(ImportMessage::Refresh))
                }));
        }
        Subscription::batch(subscriptions)
    }

    fn interrupt(&mut self) {
        // Navigation does not cancel a durable install or daemon. Source edits
        // do revoke review evidence through `invalidate()` above.
    }
}
