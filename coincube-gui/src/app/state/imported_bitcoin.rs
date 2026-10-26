//! Public-descriptor Bitcoin wallet setup and runtime for one Cube.
//!
//! The Cube's own seed and recovery kit remain separate from the imported
//! wallet's signers. This panel never constructs a Vault wallet or a Vault
//! `SpendTx` from the imported descriptors.

use std::{
    collections::{BTreeSet, HashMap},
    convert::TryFrom,
    str::FromStr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use coincube_core::{
    foreign_split::FinalizeError,
    imported_spend::{finalize_imported_spend, verify_imported_signatures, ImportedFinalizeError},
    miniscript::{
        bitcoin::{address, psbt::Psbt, secp256k1, Address, Network, Txid},
        DescriptorPublicKey, ForEachKey,
    },
    wallet_descriptor::{ImportedWalletDescriptor, WalletDescriptor},
};
use coincube_ui::{
    component::{button, date_picker, spinner, text::*},
    theme,
    widget::{Column, ColumnExt, Element, Row, RowExt},
};
use coincubed::{
    commands::{CoinStatus, CreateSpendResult, ListSpendEntry},
    config::{BitcoinBackend, BitcoinConfig, Config, ElectrumConfig, EsploraConfig},
};
use iced::{
    widget::{progress_bar, text_input},
    Subscription, Task,
};

use crate::{
    app::{
        cache::Cache,
        menu::{Menu, SettingsOption, VaultSubMenu},
        message::Message,
        settings::{self, BitcoinImport, BitcoinImportStage, ImportedWalletAttachment},
        state::State,
        view,
    },
    chain::ChainId,
    daemon::{embedded::EmbeddedDaemon, model, Daemon},
    dir::{CoincubeDirectory, NetworkDirectory},
    hw::{HardwareWallet, HardwareWalletMessage, HardwareWallets},
    services::{bitcoin_import, coincube::CoincubeClient, psbt_file},
    split_hardware::{bind::ProductionBinder, policy::imported_policy, sign::SigningSession},
    wallet_source_form::{self, Source, SourceFormCopy, WalletSourceForm},
};

/// Coordinate local imported receive allocation with Claim's address selection.
/// Source and paired target descriptors have the same source digest. The guard
/// is released before callers request a poll; it never owns daemon shutdown.
pub(crate) async fn reserve_receive_address(
    root: CoincubeDirectory,
    descriptor: WalletDescriptor,
    daemon: Arc<dyn Daemon + Send + Sync>,
) -> Result<model::GetAddressResult, String> {
    let source = descriptor.as_imported().and_then(|imported| {
        coincube_core::foreign_split::SplitSource::new(
            imported.receive_descriptor().clone(),
            Some(imported.change_descriptor().clone()),
        )
        .ok()
    });
    // Scan-only policies cannot enter Claim and retain ordinary Receive.
    let _guard = match source {
        Some(source) => Some(
            tokio::task::spawn_blocking(move || {
                bitcoin_import::pairing::legacy::source_reservation_lock(&root, &source)
            })
            .await
            .map_err(|_| "Reserving the receive address was interrupted.".to_string())??,
        ),
        None => None,
    };
    daemon
        .get_new_address()
        .await
        .map_err(|error| error.to_string())
}

fn birthday_timestamp(date: chrono::NaiveDate) -> Result<u32, String> {
    if date < chrono::NaiveDate::from_ymd_opt(2009, 1, 3).unwrap()
        || date > chrono::Utc::now().date_naive()
    {
        return Err(
            "Choose a date from January 3, 2009 through today, or scan from the beginning.".into(),
        );
    }
    let timestamp = date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
    u32::try_from(timestamp).map_err(|_| "The wallet creation date is out of range.".into())
}

static NEXT_PANEL_GENERATION: AtomicU64 = AtomicU64::new(1);

fn fresh_generation() -> u64 {
    NEXT_PANEL_GENERATION.fetch_add(1, Ordering::SeqCst)
}

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

fn public_server_label(endpoint: &str) -> String {
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return "Configured server".into();
    };
    let Some(host) = url.host_str() else {
        return "Configured server".into();
    };
    let port = url
        .port()
        .map_or_else(String::new, |port| format!(":{port}"));
    format!("{}://{host}{port}", url.scheme())
}

fn wallet_summary(descriptor: &ImportedWalletDescriptor) -> (&'static str, String, String) {
    let receive = descriptor.receive_descriptor().to_string();
    let script = if receive.starts_with("sh(wpkh(") {
        "Nested SegWit"
    } else if receive.starts_with("wpkh(") {
        "Native SegWit"
    } else if receive.starts_with("pkh(") {
        "Legacy"
    } else {
        "Multisig SegWit"
    };
    let mut fingerprints = BTreeSet::new();
    let mut signer_keys = 0usize;
    descriptor.receive_descriptor().for_each_key(|key| {
        signer_keys += 1;
        if let DescriptorPublicKey::XPub(xpub) = key {
            if let Some((fingerprint, _)) = &xpub.origin {
                fingerprints.insert(fingerprint.to_string());
            }
        }
        true
    });
    let threshold = receive
        .split_once("multi(")
        .and_then(|(_, rest)| rest.split_once(','))
        .map_or_else(
            || "1 signer".to_string(),
            |(required, _)| format!("{required} of {signer_keys} signers"),
        );
    let fingerprints = fingerprints.into_iter().collect::<Vec<_>>().join(", ");
    (script, threshold, fingerprints)
}

fn sample_receive_address(descriptor: &ImportedWalletDescriptor) -> Result<String, String> {
    let script = descriptor
        .receive_descriptor()
        .at_derivation_index(0)
        .map_err(|error| error.to_string())?
        .script_pubkey();
    Address::from_script(&script, descriptor.chain().bitcoin_network())
        .map(|address| address.to_string())
        .map_err(|error| error.to_string())
}

fn unsigned_construction(psbt: &Psbt) -> Psbt {
    let mut original = psbt.clone();
    for input in &mut original.inputs {
        input.partial_sigs.clear();
        input.tap_key_sig = None;
        input.tap_script_sigs.clear();
        input.final_script_sig = None;
        input.final_script_witness = None;
    }
    original
}

fn checked_draft(
    wallet: &ImportedWalletDescriptor,
    psbt: Psbt,
    warnings: Vec<String>,
) -> Result<ImportedDraft, String> {
    let original = unsigned_construction(&psbt);
    let secp = secp256k1::Secp256k1::verification_only();
    let signatures_per_input =
        verify_imported_signatures(wallet, &original, &psbt, &secp).map_err(|e| e.to_string())?;
    let final_txid = match finalize_imported_spend(wallet, &original, &psbt, &secp) {
        Ok(verified) => Some(verified.transaction().compute_txid()),
        Err(ImportedFinalizeError::Signatures(FinalizeError::Unsatisfied)) => None,
        Err(error) => return Err(error.to_string()),
    };
    Ok(ImportedDraft {
        psbt,
        signatures_per_input,
        final_txid,
        warnings,
    })
}

fn import_signed_psbt(
    wallet: &ImportedWalletDescriptor,
    current: &Psbt,
    text: &str,
) -> Result<(Psbt, Vec<usize>, Option<Txid>), String> {
    if text.len() > crate::services::psbt_file::MAX_FILE_BYTES as usize * 2 {
        return Err("The signed PSBT is too large.".into());
    }
    let signed = Psbt::from_str(text.trim()).map_err(|_| "Paste a base64 Bitcoin PSBT.")?;
    import_signed_psbt_value(wallet, current, &signed)
}

fn import_signed_psbt_value(
    wallet: &ImportedWalletDescriptor,
    current: &Psbt,
    signed: &Psbt,
) -> Result<(Psbt, Vec<usize>, Option<Txid>), String> {
    if signed.inputs.iter().any(|input| {
        input.final_script_sig.is_some()
            || input.final_script_witness.is_some()
            || input.tap_key_sig.is_some()
            || !input.tap_script_sigs.is_empty()
    }) {
        return Err("Import an unfinalized Bitcoin PSBT containing only signer signatures.".into());
    }
    let original = unsigned_construction(current);
    let secp = secp256k1::Secp256k1::verification_only();
    let incoming = verify_imported_signatures(wallet, &original, signed, &secp)
        .map_err(|error| error.to_string())?;
    if incoming.iter().all(|count| *count == 0) {
        return Err("The PSBT has no valid signer signatures.".into());
    }
    let mut combined = current.clone();
    for (target, source) in combined.inputs.iter_mut().zip(&signed.inputs) {
        for (key, signature) in &source.partial_sigs {
            if target
                .partial_sigs
                .get(key)
                .is_some_and(|previous| previous != signature)
            {
                return Err("A signer returned a conflicting signature for this wallet.".into());
            }
            target.partial_sigs.insert(*key, *signature);
        }
    }
    let checked = checked_draft(wallet, combined, Vec::new())?;
    Ok((
        checked.psbt,
        checked.signatures_per_input,
        checked.final_txid,
    ))
}

#[derive(Debug, Clone)]
pub enum ImportMessage {
    Source(wallet_source_form::Message),
    BirthdayEdited(String),
    BirthdayCalendarOpen,
    BirthdayCalendarCancel,
    BirthdaySelected(chrono::NaiveDate),
    ScanFromBeginning(bool),
    LookaheadEdited(String),
    ProviderSelected(Provider),
    EndpointEdited(String),
    Review,
    Edit,
    Confirm,
    Retry,
    Refresh,
    RetrySynchronization,
    PollProgress,
    ExpandDiscovery,
    Receive,
    CopyAddress,
    ToggleDetails,
    Abandon,
    ShowSend,
    CloseSend,
    RecipientEdited(String),
    AmountEdited(String),
    FeerateEdited(String),
    CreateDraft,
    SelectDraft(Txid),
    CopyPsbt,
    SignedPsbtEdited(String),
    ImportSignedPsbt,
    SavePsbt,
    LoadSignedPsbt,
    Broadcast,
    Hardware(HardwareWalletMessage),
    SignWithDevice(String),
}

type RefreshSnapshot = (
    model::GetInfoResult,
    model::ListCoinsResult,
    Vec<model::HistoryTransaction>,
    Vec<ListSpendEntry>,
    BitcoinImportStage,
);

type SignedSpend = (Psbt, Vec<usize>, Option<Txid>);

#[derive(Debug)]
pub enum ImportEvent {
    Start,
    Installed(u64, Result<(), String>),
    Resumed(u64, Result<Option<settings::WalletId>, String>),
    Opened(u64, Result<Arc<dyn Daemon + Send + Sync>, String>),
    Refreshed(u64, Result<RefreshSnapshot, String>),
    SynchronizationRequested(u64, Result<(), String>),
    ProgressChecked(u64, Result<model::GetInfoResult, String>),
    Address(u64, Result<model::GetAddressResult, String>),
    Abandoned(u64, Result<(), String>),
    DraftCreated(u64, Result<(Psbt, Vec<String>), String>),
    SignedSaved(u64, Txid, Result<SignedSpend, String>),
    Broadcasted(u64, Result<(Txid, bool), String>),
    HardwareSigned(u64, Txid, Result<SignedSpend, String>),
    Expanded(u64, Result<u32, String>),
    FileSaved(u64, Txid, Result<bool, String>),
    FileLoaded(u64, Txid, Result<Option<SignedSpend>, String>),
}

struct ImportedDraft {
    psbt: Psbt,
    signatures_per_input: Vec<usize>,
    final_txid: Option<Txid>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ScanDisplay {
    Starting,
    Scanning {
        checked: u64,
        expected: u64,
        progress: f64,
    },
    Saving,
    Retrying,
    NeedsAttention,
}

impl ScanDisplay {
    fn from_info(info: Option<&model::GetInfoResult>, sync_failed: bool) -> Self {
        if sync_failed {
            return Self::Retrying;
        }
        let Some(info) = info else {
            return Self::Starting;
        };
        if info.chain_divergence || info.refused_reorg_depth.is_some() {
            return Self::NeedsAttention;
        }
        if info.history_sync.full_scan_in_progress {
            let history = &info.history_sync;
            if history.addresses_expected > 0 {
                return Self::Scanning {
                    checked: history.addresses_checked,
                    expected: history.addresses_expected,
                    progress: history.full_scan_progress().unwrap_or(0.0),
                };
            }
            return Self::Starting;
        }
        if info.history_sync.last_failure.is_some() {
            return Self::Retrying;
        }
        if info.history_sync.full_scan_completed_at.is_some()
            || info
                .imported_wallet
                .as_ref()
                .is_some_and(|status| status.history_complete)
        {
            Self::Saving
        } else {
            Self::Starting
        }
    }
}

pub struct ImportedBitcoinPanel {
    directory: NetworkDirectory,
    cube_id: String,
    import: BitcoinImport,
    source: WalletSourceForm,
    birthday: String,
    birthday_calendar_open: bool,
    scan_from_beginning: bool,
    lookahead: String,
    provider: Provider,
    provider_known: bool,
    endpoint: String,
    review: Option<ImportedWalletAttachment>,
    review_connect_epoch: Option<u64>,
    daemon: Option<Arc<dyn Daemon + Send + Sync>>,
    /// True only before the App adopts a newly installed daemon. Once bound,
    /// the App owns shutdown and cache polling for the shared Vault runtime.
    owns_daemon: bool,
    generation: u64,
    generation_gate: Arc<AtomicU64>,
    busy: bool,
    address_reserving: bool,
    sync_requesting: bool,
    progress_polling: bool,
    snapshot_failed: bool,
    error: Option<String>,
    sync_failed: bool,
    info: Option<model::GetInfoResult>,
    coins: Option<model::ListCoinsResult>,
    history: Vec<model::HistoryTransaction>,
    address: Option<model::GetAddressResult>,
    show_details: bool,
    abandoned: bool,
    send_recipient: String,
    send_amount: String,
    send_feerate: String,
    signed_psbt_text: String,
    spend_drafts: Vec<ListSpendEntry>,
    draft: Option<ImportedDraft>,
    last_broadcast_txid: Option<Txid>,
    broadcast_notice: Option<String>,
    file_notice: Option<String>,
    connect_token: Option<String>,
    hardware_root: Option<CoincubeDirectory>,
    hardware_wallets: Option<HardwareWallets>,
}

impl ImportedBitcoinPanel {
    fn chain(&self) -> ChainId {
        self.import
            .attachment()
            .map(|w| w.descriptor.chain())
            .unwrap_or(ChainId::Bitcoin)
    }
    pub fn new(directory: NetworkDirectory, cube_id: String, import: BitcoinImport) -> Self {
        let review = import.attachment().cloned();
        let generation = fresh_generation();
        Self {
            directory,
            cube_id,
            import,
            source: WalletSourceForm::default(),
            birthday: String::new(),
            birthday_calendar_open: false,
            scan_from_beginning: true,
            lookahead: "200".into(),
            provider: Provider::Public,
            provider_known: true,
            endpoint: String::new(),
            review,
            review_connect_epoch: None,
            daemon: None,
            owns_daemon: false,
            generation,
            generation_gate: Arc::new(AtomicU64::new(generation)),
            busy: false,
            address_reserving: false,
            sync_requesting: false,
            progress_polling: false,
            snapshot_failed: false,
            error: None,
            sync_failed: false,
            info: None,
            coins: None,
            history: Vec::new(),
            address: None,
            show_details: false,
            abandoned: false,
            send_recipient: String::new(),
            send_amount: String::new(),
            send_feerate: "5".into(),
            signed_psbt_text: String::new(),
            spend_drafts: Vec::new(),
            draft: None,
            last_broadcast_txid: None,
            broadcast_notice: None,
            file_notice: None,
            connect_token: None,
            hardware_root: None,
            hardware_wallets: None,
        }
    }

    pub fn set_hardware_root(&mut self, root: crate::dir::CoincubeDirectory) {
        self.source.set_hardware_root(root.clone());
        self.hardware_root = Some(root);
    }

    pub fn set_connect_client(&mut self, client: Option<&CoincubeClient>) {
        self.connect_token = client.and_then(CoincubeClient::token).map(str::to_owned);
    }

    pub fn is_ready(&self) -> bool {
        self.import.is_ready()
            && self.coins.is_some()
            && self.info.as_ref().is_some_and(|info| {
                info.imported_wallet.as_ref().is_some_and(|status| {
                    status.history_complete
                        && info.chain_id == Some(self.chain())
                        && info.network == self.chain().bitcoin_network()
                        && status.chain_id == self.chain()
                        && status.discovery_target >= status.initial_lookahead
                        && status.discovery_target <= 10_000
                        // The daemon attests to the initial/restored/wider full
                        // scan. A newly reserved address changes allocation,
                        // not the completed historical discovery target.
                        && status.receive_scanned >= status.discovery_target
                        && status.change_scanned >= status.discovery_target
                        && info.sync.is_finite()
                        && info.sync >= 1.0
                        && info.rescan_progress.is_none()
                        && !info.history_sync.full_scan_in_progress
                        && info.history_sync.last_failure.is_none()
                        && !info.chain_divergence
                        && info.refused_reorg_depth.is_none()
                        && self.import.active_attachment().is_some_and(|wallet| {
                            wallet.wallet_id.descriptor_checksum
                                == format!("imported-v1-{}", status.wallet_identity)
                                && wallet.birthday == status.birthday
                                && status.initial_lookahead >= wallet.initial_lookahead
                        })
                })
            })
    }

    #[cfg(test)]
    pub(crate) fn reviewer_accept_completed_scan(&mut self) {
        let wallet = self
            .import
            .active_attachment()
            .expect("ready imported fixture")
            .clone();
        let chain = self.chain();
        let descriptor = wallet.descriptor.clone();
        let window = wallet.initial_lookahead;
        self.info = Some(model::GetInfoResult {
            version: "synthetic completed scan".into(),
            network: chain.bitcoin_network(),
            chain_id: Some(chain),
            block_height: 1,
            sync: 1.0,
            descriptors: coincubed::commands::GetInfoDescriptors {
                main: coincube_core::wallet_descriptor::WalletDescriptor::Imported(
                    descriptor.clone(),
                ),
            },
            imported_wallet: Some(coincubed::commands::ImportedWalletSync {
                wallet_identity: descriptor.identity(chain).unwrap().to_string(),
                chain_id: chain,
                birthday: wallet.birthday,
                initial_lookahead: window,
                discovery_target: window,
                receive_scanned: window,
                change_scanned: window,
                history_complete: true,
            }),
            rescan_progress: None,
            refused_reorg_depth: None,
            chain_divergence: false,
            timestamp: 0,
            last_poll_timestamp: Some(1),
            history_sync: Default::default(),
            receive_index: 0,
            change_index: 0,
        });
        self.coins = Some(model::ListCoinsResult { coins: Vec::new() });
        assert!(self.is_ready());
    }

    /// Bind a claim to the active public policy and the daemon's completed
    /// discovery coverage. Allocated receive/change indices also need to fit
    /// inside the fork scan, including the current next address.
    pub fn claim_source(
        &self,
        cube: &settings::CubeSettings,
    ) -> Result<crate::imported_claim::ImportedClaimSource, String> {
        if cube.id != self.cube_id
            || cube.network != ChainId::Bitcoin
            || cube.bitcoin_import.as_deref() != Some(&self.import)
            || !self.is_ready()
            || self.snapshot_failed
            || self.sync_failed
        {
            return Err("The imported Bitcoin wallet is not ready for a claim. Finish its scan and refresh the wallet.".into());
        }
        let wallet = self
            .import
            .active_attachment()
            .ok_or_else(|| "The imported Bitcoin attachment is unavailable.".to_string())?;
        wallet.validate(ChainId::Bitcoin)?;
        let info = self
            .info
            .as_ref()
            .ok_or_else(|| "The imported Bitcoin scan is unavailable.".to_string())?;
        if !self.info_matches_attachment(info) {
            return Err("The imported Bitcoin scan no longer matches this wallet.".into());
        }
        let status = info
            .imported_wallet
            .as_ref()
            .ok_or_else(|| "The imported Bitcoin scan is unavailable.".to_string())?;
        let address_window = status
            .discovery_target
            .max(status.receive_scanned)
            .max(status.change_scanned)
            .max(
                info.receive_index
                    .checked_add(1)
                    .ok_or_else(|| "The receive address index is out of range.".to_string())?,
            )
            .max(
                info.change_index
                    .checked_add(1)
                    .ok_or_else(|| "The change address index is out of range.".to_string())?,
            );
        if address_window == 0 || address_window > 10_000 {
            return Err(
                "The imported Bitcoin address window exceeds the supported claim range.".into(),
            );
        }
        Ok(crate::imported_claim::ImportedClaimSource {
            cube_id: cube.id.clone(),
            name: cube.name.clone(),
            external: wallet.descriptor.receive_descriptor().to_string(),
            internal: wallet.descriptor.change_descriptor().to_string(),
            address_window,
        })
    }

    pub fn authoritative_balance(&self) -> Option<coincube_core::miniscript::bitcoin::Amount> {
        self.is_ready()
            .then(|| {
                self.coins.as_ref().map(|coins| {
                    // Match the ordinary Vault balance: pending external
                    // deposits remain pending, while self-change is owned.
                    super::coins_summary(&coins.coins, 0, None).0
                })
            })
            .flatten()
    }

    /// Last durably allocated addresses, only after the daemon proves it is
    /// reporting for this exact Cube attachment. Historical scan bounds are
    /// deliberately not used as address-allocation state.
    pub fn verified_address_state(&self) -> Option<(u32, u32)> {
        let wallet = self.import.active_attachment()?;
        let info = self.info.as_ref()?;
        let status = info.imported_wallet.as_ref()?;
        (info.chain_id == Some(wallet.descriptor.chain())
            && status.chain_id == wallet.descriptor.chain()
            && wallet.wallet_id.descriptor_checksum
                == format!("imported-v1-{}", status.wallet_identity)
            && wallet.birthday == status.birthday
            && status.initial_lookahead >= wallet.initial_lookahead)
            .then_some((info.receive_index, info.change_index))
    }

    fn info_matches_attachment(&self, info: &model::GetInfoResult) -> bool {
        let (Some(wallet), Some(status)) = (
            self.import.active_attachment(),
            info.imported_wallet.as_ref(),
        ) else {
            return false;
        };
        matches!(&info.descriptors.main, WalletDescriptor::Imported(descriptor) if descriptor == &wallet.descriptor)
            && info.network == wallet.descriptor.chain().bitcoin_network()
            && info.chain_id == Some(wallet.descriptor.chain())
            && status.chain_id == wallet.descriptor.chain()
            && wallet.wallet_id.descriptor_checksum
                == format!("imported-v1-{}", status.wallet_identity)
            && status.birthday == wallet.birthday
            && status.initial_lookahead >= wallet.initial_lookahead
            && status.discovery_target >= status.initial_lookahead
            && status.discovery_target <= 10_000
    }

    pub fn backup_daemon(&self) -> Option<Arc<dyn Daemon + Send + Sync>> {
        self.import.active_attachment()?;
        self.daemon.clone()
    }

    pub fn runtime_daemon(&self) -> Option<Arc<dyn Daemon + Send + Sync>> {
        self.import.active_attachment()?;
        self.daemon.clone()
    }

    /// Bind the daemon opened by the ordinary Vault loader. The panel shares
    /// that handle for import progress and PSBT operations; App owns shutdown.
    pub fn bind_runtime(&mut self, daemon: Arc<dyn Daemon + Send + Sync>) -> Task<Message> {
        if self
            .daemon
            .as_ref()
            .is_some_and(|current| !Arc::ptr_eq(current, &daemon))
        {
            self.error = Some("A different Bitcoin wallet is already open for this Cube.".into());
            return Task::none();
        }
        // An external daemon does not expose Config. Its installed attachment
        // still has the exact persisted settings used by the loader.
        let persisted_config = if daemon.config().is_none() {
            self.import.active_attachment().and_then(|wallet| {
                let path = self
                    .directory
                    .coincubed_data_directory(&wallet.wallet_id)
                    .path()
                    .join("daemon.toml");
                Config::from_file(Some(path)).ok()
            })
        } else {
            None
        };
        if let Some(config) = daemon.config().or(persisted_config.as_ref()) {
            if let Err(error) = self.hydrate_provider_from_config(config) {
                log::warn!("Imported Bitcoin provider metadata unavailable: {error}");
                self.provider_known = false;
                self.endpoint.clear();
            }
        } else {
            self.provider_known = false;
            self.endpoint.clear();
        }
        self.generation = fresh_generation();
        self.generation_gate
            .store(self.generation, Ordering::SeqCst);
        self.busy = false;
        self.address_reserving = false;
        self.sync_requesting = false;
        self.progress_polling = false;
        self.snapshot_failed = false;
        self.daemon = Some(daemon);
        self.owns_daemon = false;
        if self.import.active_attachment().is_some() {
            self.refresh()
        } else {
            Task::none()
        }
    }

    /// Hand the panel-started daemon to App after an exact-Cube attachment is
    /// validated. Both sides retain an Arc, but only App will stop it.
    pub fn handoff_runtime(&mut self) -> Option<Arc<dyn Daemon + Send + Sync>> {
        let daemon = self.runtime_daemon()?;
        self.owns_daemon = false;
        Some(daemon)
    }

    /// Seed the normal Vault cache only from a complete, identity-bound scan.
    /// A partial scan never becomes a zero balance in the shared dashboard.
    pub fn sync_cache(&self, cache: &mut Cache) -> bool {
        if !self.is_ready() {
            return false;
        }
        let (Some(info), Some(coins)) = (&self.info, &self.coins) else {
            return false;
        };
        cache.daemon_cache = crate::app::cache::DaemonCache {
            blockheight: info.block_height,
            coins: coins.coins.clone(),
            rescan_progress: info.rescan_progress,
            sync_progress: info.sync,
            last_poll_timestamp: info.last_poll_timestamp,
            history_sync: info.history_sync.clone(),
            last_tick: std::time::Instant::now(),
        };
        true
    }

    pub fn handles_route(&self, menu: &Menu) -> bool {
        match menu {
            Menu::Vault(VaultSubMenu::Recovery | VaultSubMenu::Claim) => false,
            Menu::Vault(VaultSubMenu::Settings(Some(SettingsOption::LocalSigning))) => false,
            Menu::Vault(_) if !self.is_ready() => true,
            Menu::Vault(
                VaultSubMenu::Send | VaultSubMenu::PSBTs(_) | VaultSubMenu::Settings(_),
            ) => true,
            _ => false,
        }
    }

    pub fn enter_route(&mut self, menu: &Menu) -> Task<Message> {
        if !self.handles_route(menu) {
            return Task::none();
        }
        if !self.is_ready() {
            return if self.import.active_attachment().is_some() && self.daemon.is_none() {
                self.open_daemon()
            } else if self.daemon.is_some() {
                self.refresh()
            } else {
                Task::none()
            };
        }
        match menu {
            Menu::Vault(VaultSubMenu::Send | VaultSubMenu::PSBTs(_)) => {
                if self.chain().is_blake2b() {
                    self.error = Some("Sending from paired imported BTCB2 wallets is not available yet. Use the paired Claim flow.".into());
                    return Task::none();
                }
                self.error = self.ensure_hardware_wallets().err();
                if let Menu::Vault(VaultSubMenu::PSBTs(Some(id))) = menu {
                    self.select_saved_draft(*id);
                }
                self.refresh()
            }
            _ => self.refresh(),
        }
    }

    pub fn abandoned(&self) -> bool {
        self.abandoned
    }

    fn synchronization_action_label(&self) -> &'static str {
        if matches!(
            ScanDisplay::from_info(self.info.as_ref(), self.sync_failed),
            ScanDisplay::Retrying | ScanDisplay::NeedsAttention
        ) {
            "Retry synchronization"
        } else {
            "Refresh status"
        }
    }

    fn import_descriptor(&self) -> Result<ImportedWalletDescriptor, String> {
        self.import
            .active_attachment()
            .map(|wallet| wallet.descriptor.clone())
            .ok_or_else(|| "The imported wallet is not installed yet.".into())
    }

    fn send_request(&self) -> Result<(Address<address::NetworkUnchecked>, u64, u64), String> {
        let unchecked = Address::from_str(self.send_recipient.trim())
            .map_err(|_| "Enter a valid Bitcoin address.".to_string())?;
        unchecked
            .clone()
            .require_network(self.chain().bitcoin_network())
            .map_err(|_| "The recipient address belongs to another Bitcoin network.".to_string())?;
        let amount = self
            .send_amount
            .trim()
            .parse::<u64>()
            .map_err(|_| "Enter the amount in satoshis.".to_string())?;
        if amount == 0 {
            return Err("The amount must be greater than zero.".into());
        }
        let feerate = self
            .send_feerate
            .trim()
            .parse::<u64>()
            .map_err(|_| "Enter a fee rate in sat/vB.".to_string())?;
        if feerate == 0 {
            return Err("The fee rate must be at least 1 sat/vB.".into());
        }
        Ok((unchecked, amount, feerate))
    }

    fn select_saved_draft(&mut self, id: Txid) {
        self.draft = None;
        self.file_notice = None;
        self.last_broadcast_txid = None;
        self.broadcast_notice = None;
        self.signed_psbt_text.clear();
        let Some(entry) = self
            .spend_drafts
            .iter()
            .find(|entry| entry.psbt.unsigned_tx.compute_txid() == id)
        else {
            self.error = Some("This PSBT is no longer available.".into());
            return;
        };
        match self
            .import_descriptor()
            .and_then(|wallet| checked_draft(&wallet, entry.psbt.clone(), Vec::new()))
        {
            Ok(draft) => {
                self.last_broadcast_txid = entry.broadcast_txid;
                self.draft = Some(draft);
                self.signed_psbt_text.clear();
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn ensure_hardware_wallets(&mut self) -> Result<(), String> {
        if self.hardware_wallets.is_some() {
            return Ok(());
        }
        let root = self
            .hardware_root
            .clone()
            .ok_or("Hardware wallet access is unavailable.")?;
        let wallet = self.import_descriptor()?;
        let policy = imported_policy(&wallet).map_err(|error| error.to_string())?;
        self.hardware_wallets = Some(
            HardwareWallets::new(root, self.chain().bitcoin_network())
                .with_public_policy(policy.name().to_owned(), policy.descriptor().to_owned()),
        );
        Ok(())
    }

    fn invalidate(&mut self) {
        self.birthday_calendar_open = false;
        self.generation = fresh_generation();
        self.generation_gate
            .store(self.generation, Ordering::SeqCst);
        self.progress_polling = false;
        self.sync_requesting = false;
        self.snapshot_failed = false;
        self.address_reserving = false;
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
            birthday_timestamp(date)?
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
        if self.chain().is_blake2b() && matches!(message, ImportMessage::ExpandDiscovery) {
            self.error = Some(
                "Search more addresses in the original Bitcoin Cube before continuing paired Claim."
                    .into(),
            );
            return Task::none();
        }
        self.set_connect_client(client.as_ref());
        if self.chain().is_blake2b()
            && matches!(
                message,
                ImportMessage::ShowSend
                    | ImportMessage::CreateDraft
                    | ImportMessage::SelectDraft(_)
                    | ImportMessage::CopyPsbt
                    | ImportMessage::SignedPsbtEdited(_)
                    | ImportMessage::ImportSignedPsbt
                    | ImportMessage::SavePsbt
                    | ImportMessage::LoadSignedPsbt
                    | ImportMessage::Broadcast
                    | ImportMessage::Hardware(_)
                    | ImportMessage::SignWithDevice(_)
            )
        {
            self.error = Some("Sending from paired imported BTCB2 wallets is not available yet. Use the paired Claim flow.".into());
            return Task::none();
        }
        if self.busy && (!matches!(&message, ImportMessage::PollProgress) || self.address_reserving)
        {
            return Task::none();
        }
        if matches!(&self.import.stage, BitcoinImportStage::Installing { .. })
            && matches!(
                &message,
                ImportMessage::Source(_)
                    | ImportMessage::BirthdayEdited(_)
                    | ImportMessage::BirthdayCalendarOpen
                    | ImportMessage::BirthdayCalendarCancel
                    | ImportMessage::BirthdaySelected(_)
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
                    wallet_source_form::Message::Focus(id) => iced::widget::operation::focus(id),
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
            ImportMessage::BirthdayCalendarOpen => {
                self.birthday_calendar_open = !self.scan_from_beginning
                    && self.review.is_none()
                    && self.import.active_attachment().is_none();
                Task::none()
            }
            ImportMessage::BirthdayCalendarCancel => {
                self.birthday_calendar_open = false;
                Task::none()
            }
            ImportMessage::BirthdaySelected(date) => {
                if !self.birthday_calendar_open {
                    return Task::none();
                }
                self.birthday_calendar_open = false;
                match birthday_timestamp(date) {
                    Ok(_) => {
                        self.birthday = date.format("%Y-%m-%d").to_string();
                        self.invalidate();
                    }
                    Err(error) => self.error = Some(error),
                }
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
                self.provider_known = true;
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
                self.birthday_calendar_open = false;
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
                self.birthday_calendar_open = false;
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
                        Message::ImportedBitcoin(Box::new(ImportEvent::Installed(
                            generation, result,
                        )))
                    },
                )
            }
            ImportMessage::Refresh => {
                if self.busy {
                    Task::none()
                } else {
                    // This is the deliberate/slow refresh path. A failed full
                    // snapshot must not be retried by each 2-second status tick.
                    self.snapshot_failed = false;
                    if self.daemon.is_some() {
                        self.refresh()
                    } else {
                        self.open_daemon()
                    }
                }
            }
            ImportMessage::RetrySynchronization => self.retry_synchronization(),
            ImportMessage::PollProgress => self.poll_progress(),
            ImportMessage::ExpandDiscovery => {
                let Some(wallet) = self.import.active_attachment().cloned() else {
                    return Task::none();
                };
                if self.verified_address_state().is_none() {
                    self.error =
                        Some("Refresh this wallet before expanding its history search.".into());
                    return Task::none();
                }
                let Some(status) = self
                    .info
                    .as_ref()
                    .and_then(|info| info.imported_wallet.as_ref())
                else {
                    return Task::none();
                };
                let target = status
                    .discovery_target
                    .max(wallet.initial_lookahead)
                    .saturating_add(200)
                    .min(10_000);
                if target <= status.discovery_target {
                    self.error = Some("The maximum discovery range has been reached.".into());
                    return Task::none();
                }
                // Expansion requires the daemon to be stopped even when App
                // owns the shared runtime. Revoke late callbacks first, then
                // stop it in the task before changing its durable scan target.
                self.generation = fresh_generation();
                self.generation_gate
                    .store(self.generation, Ordering::SeqCst);
                self.progress_polling = false;
                self.sync_requesting = false;
                self.address_reserving = false;
                let daemon = self.daemon.take();
                self.owns_daemon = false;
                self.info = None;
                self.coins = None;
                self.history.clear();
                self.address = None;
                self.spend_drafts.clear();
                self.draft = None;
                self.import.stage = BitcoinImportStage::Syncing {
                    wallet: wallet.clone(),
                };
                self.busy = true;
                self.error = None;
                let generation = self.generation;
                let data_dir = self.directory.coincubed_data_directory(&wallet.wallet_id);
                Task::perform(
                    async move {
                        if let Some(daemon) = daemon {
                            daemon.stop().await.map_err(|error| error.to_string())?;
                        }
                        tokio::task::spawn_blocking(move || {
                            let config =
                                Config::from_file(Some(data_dir.path().join("daemon.toml")))
                                    .map_err(|error| error.to_string())?;
                            coincubed::expand_imported_discovery(&config, &data_dir, target)
                                .map_err(|error| error.to_string())
                        })
                        .await
                        .map_err(|error| error.to_string())?
                    },
                    move |result| {
                        Message::ImportedBitcoin(Box::new(ImportEvent::Expanded(
                            generation, result,
                        )))
                    },
                )
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
                let Some(root) = self.hardware_root.clone() else {
                    self.error =
                        Some("Reopen this Cube before reserving a receive address.".into());
                    return Task::none();
                };
                let Some(wallet) = self.import.active_attachment() else {
                    return Task::none();
                };
                let descriptor = WalletDescriptor::Imported(wallet.descriptor.clone());
                // A successful reservation changes the Kit fingerprint. Hide
                // the previous daemon snapshot until its new durable index is
                // read back, even if the address request fails midway.
                self.generation = fresh_generation();
                self.generation_gate
                    .store(self.generation, Ordering::SeqCst);
                self.progress_polling = false;
                self.sync_requesting = false;
                self.info = None;
                self.address_reserving = true;
                self.busy = true;
                let generation = self.generation;
                Task::perform(
                    async move { reserve_receive_address(root, descriptor, daemon).await },
                    move |result| {
                        Message::ImportedBitcoin(Box::new(ImportEvent::Address(generation, result)))
                    },
                )
            }
            ImportMessage::CopyAddress => {
                self.address.as_ref().map_or_else(Task::none, |address| {
                    iced::clipboard::write(address.address.to_string())
                })
            }
            ImportMessage::ToggleDetails => {
                self.show_details = !self.show_details;
                Task::none()
            }
            ImportMessage::ShowSend => {
                if !self.is_ready() {
                    self.error =
                        Some("Wait for the complete Bitcoin history scan before sending.".into());
                    Task::none()
                } else {
                    self.error = self.ensure_hardware_wallets().err();
                    Task::done(Message::View(view::Message::Menu(Menu::Vault(
                        VaultSubMenu::Send,
                    ))))
                }
            }
            ImportMessage::CloseSend => {
                self.error = None;
                Task::done(Message::View(view::Message::Menu(Menu::Vault(
                    VaultSubMenu::Overview,
                ))))
            }
            ImportMessage::RecipientEdited(value) => {
                self.send_recipient = value;
                self.error = None;
                Task::none()
            }
            ImportMessage::AmountEdited(value) => {
                self.send_amount = value;
                self.error = None;
                Task::none()
            }
            ImportMessage::FeerateEdited(value) => {
                self.send_feerate = value;
                self.error = None;
                Task::none()
            }
            ImportMessage::CreateDraft => {
                if !self.is_ready() {
                    self.error = Some(
                        "The complete history scan must finish before a spend can be drafted."
                            .into(),
                    );
                    return Task::none();
                }
                let (address, amount, feerate) = match self.send_request() {
                    Ok(request) => request,
                    Err(error) => {
                        self.error = Some(error);
                        return Task::none();
                    }
                };
                let Some(daemon) = self.daemon.clone() else {
                    return Task::none();
                };
                self.busy = true;
                self.error = None;
                let generation = self.generation;
                Task::perform(
                    async move {
                        let destinations = HashMap::from([(address, amount)]);
                        match daemon
                            .create_spend_tx(&[], &destinations, feerate, None)
                            .await
                            .map_err(|error| error.to_string())?
                        {
                            CreateSpendResult::Success { psbt, warnings } => {
                                daemon
                                    .update_spend_tx(&psbt)
                                    .await
                                    .map_err(|error| error.to_string())?;
                                Ok((psbt, warnings))
                            }
                            CreateSpendResult::InsufficientFunds { missing } => Err(format!(
                                "Insufficient funds: {missing} more satoshis are needed."
                            )),
                        }
                    },
                    move |result| {
                        Message::ImportedBitcoin(Box::new(ImportEvent::DraftCreated(
                            generation, result,
                        )))
                    },
                )
            }
            ImportMessage::SelectDraft(id) => {
                self.select_saved_draft(id);
                Task::none()
            }
            ImportMessage::CopyPsbt => self.draft.as_ref().map_or_else(Task::none, |draft| {
                iced::clipboard::write(draft.psbt.to_string())
            }),
            ImportMessage::SignedPsbtEdited(value) => {
                self.signed_psbt_text = value;
                self.error = None;
                Task::none()
            }
            ImportMessage::ImportSignedPsbt => {
                let Some(draft) = &self.draft else {
                    self.error =
                        Some("Create or select a spend before importing signatures.".into());
                    return Task::none();
                };
                let wallet = match self.import_descriptor() {
                    Ok(wallet) => wallet,
                    Err(error) => {
                        self.error = Some(error);
                        return Task::none();
                    }
                };
                let Some(daemon) = self.daemon.clone() else {
                    return Task::none();
                };
                let current = draft.psbt.clone();
                let draft_id = current.unsigned_tx.compute_txid();
                let text = self.signed_psbt_text.clone();
                self.busy = true;
                let generation = self.generation;
                let gate = self.generation_gate.clone();
                Task::perform(
                    async move {
                        let (psbt, counts, final_txid) =
                            import_signed_psbt(&wallet, &current, &text)?;
                        if gate.load(Ordering::SeqCst) != generation {
                            return Err(
                                "This Cube was closed before the signatures were saved.".into()
                            );
                        }
                        daemon
                            .update_spend_tx(&psbt)
                            .await
                            .map_err(|error| error.to_string())?;
                        Ok((psbt, counts, final_txid))
                    },
                    move |result| {
                        Message::ImportedBitcoin(Box::new(ImportEvent::SignedSaved(
                            generation, draft_id, result,
                        )))
                    },
                )
            }
            ImportMessage::SavePsbt => {
                let Some(draft) = &self.draft else {
                    return Task::none();
                };
                let psbt = draft.psbt.clone();
                let id = psbt.unsigned_tx.compute_txid();
                let generation = self.generation;
                let gate = self.generation_gate.clone();
                self.busy = true;
                self.error = None;
                Task::perform(
                    async move {
                        let name = format!("bitcoin-spend-{}.psbt", &id.to_string()[..12]);
                        let Some(handle) = rfd::AsyncFileDialog::new()
                            .set_title("Save unsigned Bitcoin PSBT")
                            .set_file_name(&name)
                            .save_file()
                            .await
                        else {
                            return Ok(false);
                        };
                        if gate.load(Ordering::SeqCst) != generation {
                            return Err("This Cube was closed before the PSBT was saved.".into());
                        }
                        let bytes = psbt_file::encode(&psbt, psbt_file::Encoding::Binary);
                        if bytes.len() as u64 > psbt_file::MAX_FILE_BYTES {
                            return Err("The PSBT is too large to save.".into());
                        }
                        tokio::fs::write(handle.path(), bytes)
                            .await
                            .map_err(|error| error.to_string())?;
                        Ok(true)
                    },
                    move |result| {
                        Message::ImportedBitcoin(Box::new(ImportEvent::FileSaved(
                            generation, id, result,
                        )))
                    },
                )
            }
            ImportMessage::LoadSignedPsbt => {
                let Some(draft) = &self.draft else {
                    return Task::none();
                };
                let wallet = match self.import_descriptor() {
                    Ok(wallet) => wallet,
                    Err(error) => {
                        self.error = Some(error);
                        return Task::none();
                    }
                };
                let Some(daemon) = self.daemon.clone() else {
                    return Task::none();
                };
                let current = draft.psbt.clone();
                let draft_id = current.unsigned_tx.compute_txid();
                let generation = self.generation;
                let gate = self.generation_gate.clone();
                self.busy = true;
                self.error = None;
                Task::perform(
                    async move {
                        let Some(handle) = rfd::AsyncFileDialog::new()
                            .set_title("Choose a signed Bitcoin PSBT")
                            .add_filter("Bitcoin PSBT", &["psbt", "txt"])
                            .pick_file()
                            .await
                        else {
                            return Ok(None);
                        };
                        if gate.load(Ordering::SeqCst) != generation {
                            return Err(
                                "This Cube was closed before the signed PSBT was loaded.".into()
                            );
                        }
                        let path = handle.path().to_path_buf();
                        let signed = tokio::task::spawn_blocking(move || psbt_file::load(&path))
                            .await
                            .map_err(|error| error.to_string())?
                            .map_err(|error| format!("Could not read signed PSBT: {error:?}"))?;
                        let (psbt, counts, final_txid) =
                            import_signed_psbt_value(&wallet, &current, &signed)?;
                        if gate.load(Ordering::SeqCst) != generation {
                            return Err(
                                "This Cube was closed before the signatures were saved.".into()
                            );
                        }
                        daemon
                            .update_spend_tx(&psbt)
                            .await
                            .map_err(|error| error.to_string())?;
                        Ok(Some((psbt, counts, final_txid)))
                    },
                    move |result| {
                        Message::ImportedBitcoin(Box::new(ImportEvent::FileLoaded(
                            generation, draft_id, result,
                        )))
                    },
                )
            }
            ImportMessage::Broadcast => {
                if !self.is_ready() {
                    self.error =
                        Some("Refresh the complete wallet history before broadcasting.".into());
                    return Task::none();
                }
                let Some(draft) = &self.draft else {
                    return Task::none();
                };
                if self.spend_drafts.iter().any(|entry| {
                    entry.psbt.unsigned_tx.compute_txid() == draft.psbt.unsigned_tx.compute_txid()
                        && entry.broadcast_txid.is_some()
                }) {
                    self.error = Some("This transaction was already broadcast.".into());
                    return Task::none();
                }
                let Some(final_txid) = draft.final_txid else {
                    self.error = Some("More signer signatures are needed before broadcast.".into());
                    return Task::none();
                };
                let wallet = match self.import_descriptor() {
                    Ok(wallet) => wallet,
                    Err(error) => {
                        self.error = Some(error);
                        return Task::none();
                    }
                };
                let Some(daemon) = self.daemon.clone() else {
                    return Task::none();
                };
                let signed = draft.psbt.clone();
                let draft_id = signed.unsigned_tx.compute_txid();
                self.busy = true;
                let generation = self.generation;
                Task::perform(
                    async move {
                        let original = unsigned_construction(&signed);
                        let secp = secp256k1::Secp256k1::verification_only();
                        let verified = finalize_imported_spend(&wallet, &original, &signed, &secp)
                            .map_err(|error| error.to_string())?;
                        if verified.transaction().compute_txid() != final_txid {
                            return Err(
                                "The finalized transaction changed. Review the spend again.".into(),
                            );
                        }
                        let before = daemon
                            .list_spend_txs()
                            .await
                            .map_err(|error| error.to_string())?
                            .spend_txs
                            .into_iter()
                            .find(|entry| entry.psbt.unsigned_tx.compute_txid() == draft_id)
                            .ok_or("The saved transaction no longer exists.")?;
                        if before.broadcast_txid.is_some() {
                            return Err("This transaction was already broadcast.".into());
                        }
                        if before.psbt != signed {
                            return Err("The saved signatures changed. Reopen and review this PSBT before broadcasting.".into());
                        }
                        daemon
                            .broadcast_spend_tx(&draft_id)
                            .await
                            .map_err(|error| error.to_string())?;
                        let actual = daemon.list_spend_txs().await.map_err(|error| error.to_string())?
                            .spend_txs.into_iter().find(|entry| entry.psbt.unsigned_tx.compute_txid() == draft_id)
                            .and_then(|entry| entry.broadcast_txid)
                            .ok_or("Broadcast was sent, but its final transaction ID could not be confirmed. Refresh before trying again.")?;
                        Ok((actual, actual != final_txid))
                    },
                    move |result| {
                        Message::ImportedBitcoin(Box::new(ImportEvent::Broadcasted(
                            generation, result,
                        )))
                    },
                )
            }
            ImportMessage::Hardware(message) => {
                let Some(listing) = &mut self.hardware_wallets else {
                    return Task::none();
                };
                match listing.update(message) {
                    Ok(task) => task.map(|message| {
                        Message::View(view::Message::ImportedBitcoin(ImportMessage::Hardware(
                            message,
                        )))
                    }),
                    Err(error) => {
                        self.error = Some(format!("Hardware wallet: {error}"));
                        Task::none()
                    }
                }
            }
            ImportMessage::SignWithDevice(id) => {
                if !self.is_ready() {
                    self.error = Some("Refresh the complete wallet history before signing.".into());
                    return Task::none();
                }
                let Some(draft) = &self.draft else {
                    self.error = Some("Create or select a PSBT before signing.".into());
                    return Task::none();
                };
                let wallet = match self.import_descriptor() {
                    Ok(wallet) => wallet,
                    Err(error) => {
                        self.error = Some(error);
                        return Task::none();
                    }
                };
                let policy = match imported_policy(&wallet) {
                    Ok(policy) => policy,
                    Err(error) => {
                        self.error = Some(error.to_string());
                        return Task::none();
                    }
                };
                let Some(listing) = &self.hardware_wallets else {
                    self.error = Some("Connect a hardware wallet first.".into());
                    return Task::none();
                };
                let Some(device) = listing
                    .list
                    .iter()
                    .find(|device| device.id() == &id)
                    .cloned()
                else {
                    self.error =
                        Some("The selected hardware wallet is no longer connected.".into());
                    return Task::none();
                };
                let binder = ProductionBinder::from_listing(listing);
                let Some(daemon) = self.daemon.clone() else {
                    return Task::none();
                };
                let current = draft.psbt.clone();
                let draft_id = current.unsigned_tx.compute_txid();
                let generation = self.generation;
                let gate = self.generation_gate.clone();
                self.busy = true;
                self.error = None;
                Task::perform(
                    async move {
                        if gate.load(Ordering::SeqCst) != generation {
                            return Err("The Cube was closed before hardware signing began.".into());
                        }
                        let mut session =
                            SigningSession::from_listed(&device, policy, Arc::new(binder))
                                .await
                                .map_err(|error| error.to_string())?;
                        session
                            .register()
                            .await
                            .map_err(|error| error.to_string())?;
                        if gate.load(Ordering::SeqCst) != generation {
                            return Err("The Cube was closed before hardware signing began.".into());
                        }
                        let signed = session
                            .sign(&current)
                            .await
                            .map_err(|error| error.to_string())?
                            .into_psbt();
                        if gate.load(Ordering::SeqCst) != generation {
                            return Err(
                                "The Cube was closed before hardware signing completed.".into()
                            );
                        }
                        let (psbt, counts, final_txid) =
                            import_signed_psbt(&wallet, &current, &signed.to_string())?;
                        if gate.load(Ordering::SeqCst) != generation {
                            return Err(
                                "The Cube was closed before the signatures were saved.".into()
                            );
                        }
                        daemon
                            .update_spend_tx(&psbt)
                            .await
                            .map_err(|error| error.to_string())?;
                        Ok((psbt, counts, final_txid))
                    },
                    move |result| {
                        Message::ImportedBitcoin(Box::new(ImportEvent::HardwareSigned(
                            generation, draft_id, result,
                        )))
                    },
                )
            }
            ImportMessage::Abandon => {
                if self.import.active_attachment().is_some() {
                    self.error =
                        Some("An installed Bitcoin wallet cannot be replaced here.".into());
                    return Task::none();
                }
                self.busy = true;
                let generation = self.generation;
                let directory = self.directory.clone();
                let cube_id = self.cube_id.clone();
                let operation_id = self.import.operation_id;
                Task::perform(
                    async move {
                        bitcoin_import::abandon_pending(&directory, &cube_id, operation_id).await
                    },
                    move |result| {
                        Message::ImportedBitcoin(Box::new(ImportEvent::Abandoned(
                            generation, result,
                        )))
                    },
                )
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
                            Message::ImportedBitcoin(Box::new(ImportEvent::Resumed(
                                generation, result,
                            )))
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
                        self.owns_daemon = true;
                        self.sync_failed = false;
                        self.refresh()
                    }
                    Err(_) => {
                        // Backend errors can contain an entire HTML error page, endpoint,
                        // or authentication details. Keep those out of the wallet screen.
                        log::warn!(
                            "Imported Bitcoin daemon could not connect to its configured server"
                        );
                        self.sync_failed = true;
                        self.error = Some("Could not connect to this wallet's Bitcoin server. Check the server and your connection, then retry synchronization.".into());
                        Task::none()
                    }
                }
            }
            ImportEvent::Refreshed(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok((info, coins, history, spend_drafts, stage)) => {
                        self.snapshot_failed = false;
                        self.info = Some(info);
                        self.coins = Some(coins);
                        self.history = history;
                        self.spend_drafts = spend_drafts;
                        self.import.stage = stage;
                        self.error = None;
                        self.sync_failed = false;
                        if let Some(selected) = self.draft.take() {
                            let id = selected.psbt.unsigned_tx.compute_txid();
                            let current = self
                                .spend_drafts
                                .iter()
                                .find(|entry| entry.psbt.unsigned_tx.compute_txid() == id)
                                .map(|entry| entry.psbt.clone());
                            match current {
                                Some(psbt) => match self.import_descriptor().and_then(|wallet| {
                                    checked_draft(&wallet, psbt, selected.warnings)
                                }) {
                                    Ok(draft) => self.draft = Some(draft),
                                    Err(error) => self.error = Some(error),
                                },
                                None => {
                                    self.error = Some(
                                        "The saved transaction is no longer available.".into(),
                                    );
                                }
                            }
                        }
                    }
                    Err(_) => {
                        log::warn!("Imported Bitcoin synchronization failed");
                        self.snapshot_failed = true;
                        self.info = None;
                        self.coins = None;
                        self.history.clear();
                        self.address = None;
                        self.sync_failed = true;
                        self.error = Some("Could not synchronize this Bitcoin wallet. Check the Bitcoin server and your connection, then retry synchronization.".into());
                    }
                }
                Task::none()
            }
            ImportEvent::SynchronizationRequested(generation, result)
                if generation == self.generation =>
            {
                self.sync_requesting = false;
                match result {
                    Ok(()) => self.poll_progress(),
                    Err(_) => {
                        self.snapshot_failed = true;
                        self.sync_failed = true;
                        self.error = Some("Could not retry this wallet's Bitcoin server. Check your connection and try again.".into());
                        Task::none()
                    }
                }
            }
            ImportEvent::ProgressChecked(generation, result) if generation == self.generation => {
                self.progress_polling = false;
                // A receive reservation deliberately clears the old address
                // snapshot until its new high-water index is read back.
                if self.is_ready() || self.address_reserving {
                    return Task::none();
                }
                // A status-only response cannot reauthorize coins from an
                // earlier scan. The full refresh must pair completed history
                // with a fresh authoritative coin snapshot.
                self.coins = None;
                match result {
                    Ok(info) => {
                        if !self.info_matches_attachment(&info) {
                            self.info = None;
                            self.sync_failed = true;
                            self.error = Some("This Bitcoin server reported a different wallet or chain. Check the wallet setup and retry synchronization.".into());
                            return Task::none();
                        }
                        let history_complete = info
                            .imported_wallet
                            .as_ref()
                            .is_some_and(|status| status.history_complete);
                        self.info = Some(info);
                        if self.snapshot_failed {
                            // Keep the full-snapshot error actionable until a
                            // user retry or the normal 20-second refresh.
                            return Task::none();
                        }
                        self.sync_failed = false;
                        if self.error.as_deref().is_some_and(|error| {
                            error.starts_with("Could not synchronize this Bitcoin wallet")
                                || error
                                    .starts_with("This Bitcoin server reported a different wallet")
                        }) {
                            self.error = None;
                        }
                        // Only the full refresh may mark the Cube ready: it also
                        // obtains the authoritative coin and transaction snapshot.
                        if history_complete && !self.busy && self.verified_address_state().is_some()
                        {
                            return self.refresh();
                        }
                    }
                    Err(_) => {
                        log::warn!("Imported Bitcoin scan status could not be refreshed");
                        self.sync_failed = true;
                        self.error = Some("Could not synchronize this Bitcoin wallet. Check the Bitcoin server and your connection, then retry synchronization.".into());
                    }
                }
                Task::none()
            }
            ImportEvent::Address(generation, result) if generation == self.generation => {
                self.busy = false;
                self.address_reserving = false;
                match result {
                    Ok(address) => {
                        self.address = Some(address);
                        return self.refresh();
                    }
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            ImportEvent::Abandoned(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok(()) => self.abandoned = true,
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            ImportEvent::DraftCreated(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok((psbt, warnings)) => match self
                        .import_descriptor()
                        .and_then(|wallet| checked_draft(&wallet, psbt, warnings))
                    {
                        Ok(draft) => {
                            self.draft = Some(draft);
                            self.signed_psbt_text.clear();
                            self.error = None;
                            return self.refresh();
                        }
                        Err(error) => self.error = Some(error),
                    },
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            ImportEvent::SignedSaved(generation, draft_id, result)
            | ImportEvent::HardwareSigned(generation, draft_id, result)
                if generation == self.generation =>
            {
                self.busy = false;
                if self
                    .draft
                    .as_ref()
                    .is_none_or(|draft| draft.psbt.unsigned_tx.compute_txid() != draft_id)
                {
                    return Task::none();
                }
                match result {
                    Ok((psbt, counts, final_txid)) => {
                        let warnings = self
                            .draft
                            .as_ref()
                            .map_or_else(Vec::new, |draft| draft.warnings.clone());
                        self.draft = Some(ImportedDraft {
                            psbt,
                            signatures_per_input: counts,
                            final_txid,
                            warnings,
                        });
                        self.signed_psbt_text.clear();
                        self.error = None;
                        return self.refresh();
                    }
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            ImportEvent::Broadcasted(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok((txid, changed)) => {
                        self.last_broadcast_txid = Some(txid);
                        self.broadcast_notice = changed.then(|| "Another valid signer signature changed the final transaction ID. The ID shown here comes from the broadcast wallet record.".into());
                        self.error = None;
                        return self.refresh();
                    }
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            ImportEvent::Expanded(generation, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok(_) => self.open_daemon(),
                    Err(error) => {
                        self.error = Some(format!(
                            "Could not expand the wallet history search: {error}"
                        ));
                        Task::none()
                    }
                }
            }
            ImportEvent::FileSaved(generation, id, result) if generation == self.generation => {
                self.busy = false;
                match result {
                    Ok(true)
                        if self
                            .draft
                            .as_ref()
                            .is_some_and(|draft| draft.psbt.unsigned_tx.compute_txid() == id) =>
                    {
                        self.file_notice = Some("PSBT saved. Open it in the original signing wallet, then load the signed file here.".into());
                    }
                    Ok(_) => {}
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            ImportEvent::FileLoaded(generation, draft_id, result)
                if generation == self.generation =>
            {
                self.busy = false;
                if self
                    .draft
                    .as_ref()
                    .is_none_or(|draft| draft.psbt.unsigned_tx.compute_txid() != draft_id)
                {
                    return Task::none();
                }
                match result {
                    Ok(Some((psbt, counts, final_txid))) => {
                        let warnings = self
                            .draft
                            .as_ref()
                            .map_or_else(Vec::new, |draft| draft.warnings.clone());
                        self.draft = Some(ImportedDraft {
                            psbt,
                            signatures_per_input: counts,
                            final_txid,
                            warnings,
                        });
                        self.file_notice = Some("Signed PSBT loaded and verified.".into());
                        self.error = None;
                        return self.refresh();
                    }
                    Ok(None) => {}
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
        if self.daemon.is_some() {
            return self.refresh();
        }
        if self.chain().is_blake2b() {
            self.sync_failed = true;
            self.error = Some(
                "Reopen this paired Cube to restore its authenticated Bitcoin Blake2b session."
                    .into(),
            );
            return Task::none();
        }
        let Some(wallet) = self.import.active_attachment() else {
            return Task::none();
        };
        let path = self
            .directory
            .coincubed_data_directory(&wallet.wallet_id)
            .path()
            .join("daemon.toml");
        let mut config = match Config::from_file(Some(path)) {
            Ok(config) => config,
            Err(_) => {
                log::warn!("Imported Bitcoin daemon settings could not be loaded");
                self.sync_failed = true;
                self.error = Some("Could not read this wallet's Bitcoin server settings. Retry synchronization or check the wallet setup.".into());
                return Task::none();
            }
        };
        if let Err(error) = crate::loader::authenticate_imported_connect_provider(
            &mut config,
            self.chain(),
            self.connect_token.as_deref(),
        ) {
            self.sync_failed = true;
            self.error = Some(error.to_string());
            return Task::none();
        }
        if let Err(error) = self.hydrate_provider_from_config(&config) {
            self.sync_failed = true;
            self.error = Some(error.into());
            return Task::none();
        }
        let generation = self.generation;
        let gate = self.generation_gate.clone();
        self.busy = true;
        self.error = None;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
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
            move |result| {
                Message::ImportedBitcoin(Box::new(ImportEvent::Opened(generation, result)))
            },
        )
    }

    fn hydrate_provider_from_config(&mut self, config: &Config) -> Result<(), &'static str> {
        match config.bitcoin_backend.as_ref() {
            Some(BitcoinBackend::Esplora(esplora))
                if esplora.addr == crate::installer::connect_url(self.chain()) =>
            {
                self.provider = Provider::Connect;
                self.endpoint = esplora.addr.clone();
            }
            Some(BitcoinBackend::Esplora(esplora))
                if esplora.addr == crate::installer::public_esplora_url(Network::Bitcoin) =>
            {
                self.provider = Provider::Public;
                self.endpoint = esplora.addr.clone();
            }
            Some(BitcoinBackend::Esplora(esplora)) => {
                self.provider = Provider::Esplora;
                self.endpoint = esplora.addr.clone();
            }
            Some(BitcoinBackend::Electrum(electrum)) => {
                self.provider = Provider::Electrum;
                self.endpoint = electrum.addr.clone();
            }
            _ => return Err("This imported wallet's Bitcoin server is unsupported."),
        }
        self.provider_known = true;
        Ok(())
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
                let spend_drafts = daemon
                    .list_spend_txs()
                    .await
                    .map_err(|e| e.to_string())?
                    .spend_txs;
                Ok((info, coins, history, spend_drafts, stage))
            },
            move |result| {
                Message::ImportedBitcoin(Box::new(ImportEvent::Refreshed(generation, result)))
            },
        )
    }

    fn poll_progress(&mut self) -> Task<Message> {
        if self.progress_polling || self.is_ready() {
            return Task::none();
        }
        let Some(daemon) = self.daemon.clone() else {
            return Task::none();
        };
        self.progress_polling = true;
        let generation = self.generation;
        Task::perform(
            async move { daemon.get_info().await.map_err(|error| error.to_string()) },
            move |result| {
                Message::ImportedBitcoin(Box::new(ImportEvent::ProgressChecked(generation, result)))
            },
        )
    }

    fn retry_synchronization(&mut self) -> Task<Message> {
        if self.sync_requesting || self.import.active_attachment().is_none() {
            return Task::none();
        }
        self.snapshot_failed = false;
        let Some(daemon) = self.daemon.clone() else {
            return self.open_daemon();
        };
        self.sync_requesting = true;
        let generation = self.generation;
        Task::perform(
            async move {
                daemon
                    .request_sync()
                    .await
                    .map_err(|error| error.to_string())
            },
            move |result| {
                Message::ImportedBitcoin(Box::new(ImportEvent::SynchronizationRequested(
                    generation, result,
                )))
            },
        )
    }

    pub fn stop(&mut self) {
        self.birthday_calendar_open = false;
        self.generation = fresh_generation();
        self.generation_gate
            .store(self.generation, Ordering::SeqCst);
        self.busy = false;
        self.address_reserving = false;
        self.sync_requesting = false;
        self.progress_polling = false;
        self.snapshot_failed = false;
        self.source.clear_transient();
        let owns_daemon = self.owns_daemon;
        self.owns_daemon = false;
        if let Some(daemon) = self.daemon.take().filter(|_| owns_daemon) {
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
            .max_width(800)
            .push(h2(if self.review.is_none() { "Import a Bitcoin wallet" } else { "Review Bitcoin wallet" }))
            .push(p1_regular("Use your existing Bitcoin wallet in this Cube. Signing stays with your original wallet or hardware device.").style(theme::text::secondary));
        if self.review.is_none() {
            let beginning = if self.scan_from_beginning {
                button::primary(None, "From the beginning")
            } else {
                button::secondary(None, "From the beginning")
            };
            let dated = if self.scan_from_beginning {
                button::secondary(None, "From a creation date")
            } else {
                button::primary(None, "From a creation date")
            };
            content = content
                .push(wallet_source_form::view(&self.source, &SourceFormCopy::import()).map(|m| {
                    view::Message::ImportedBitcoin(ImportMessage::Source(m))
                }))
                .push(iced::widget::Space::new().height(8))
                .push(p1_bold("Where should the history scan start?"))
                .push(Row::new().spacing(10)
                    .push(beginning
                        .width(iced::Length::Fill)
                        .on_press(view::Message::ImportedBitcoin(ImportMessage::ScanFromBeginning(true))))
                    .push(dated
                        .width(iced::Length::Fill)
                        .on_press(view::Message::ImportedBitcoin(ImportMessage::ScanFromBeginning(false)))))
                .push_maybe((!self.scan_from_beginning).then(|| {
                    Row::new()
                        .spacing(10)
                        .align_y(iced::Alignment::Center)
                        .push(text_input("YYYY-MM-DD", &self.birthday)
                            .on_input(|s| view::Message::ImportedBitcoin(ImportMessage::BirthdayEdited(s)))
                            .padding(10))
                        .push(date_picker::calendar_button(
                            self.birthday_calendar_open,
                            chrono::NaiveDate::parse_from_str(self.birthday.trim(), "%Y-%m-%d")
                                .unwrap_or_else(|_| chrono::Utc::now().date_naive()),
                            ImportMessage::BirthdayCalendarOpen,
                            ImportMessage::BirthdayCalendarCancel,
                            ImportMessage::BirthdaySelected,
                        ).map(view::Message::ImportedBitcoin))
                }))
                .push(caption(if self.scan_from_beginning {
                    "Starts at Bitcoin's first block. Choose this if you do not know when the wallet was created; it may take longer."
                } else {
                    "Choose the original wallet's creation date or an earlier date so older transactions are included."
                }).style(theme::text::secondary))
                .push(iced::widget::Space::new().height(8))
                .push(p1_bold("Addresses to search per branch"))
                .push(text_input("200", &self.lookahead)
                    .on_input(|s| view::Message::ImportedBitcoin(ImportMessage::LookaheadEdited(s)))
                    .padding(10))
                .push(caption("Start with 200 receive and 200 change addresses. You can search further after the scan finishes.").style(theme::text::secondary))
                .push(iced::widget::Space::new().height(8))
                .push(p1_bold("Bitcoin server"));
            let mut providers = Column::new().spacing(8);
            let mut provider_row = Row::new().spacing(8);
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
                provider_row = provider_row.push(button.width(iced::Length::Fill).on_press(
                    view::Message::ImportedBitcoin(ImportMessage::ProviderSelected(provider)),
                ));
                if matches!(provider, Provider::Public | Provider::Electrum) {
                    providers = providers.push(provider_row);
                    provider_row = Row::new().spacing(8);
                }
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
            let (script, threshold, fingerprints) = wallet_summary(&wallet.descriptor);
            let sample_address = sample_receive_address(&wallet.descriptor)
                .unwrap_or_else(|_| "Unable to derive a sample address".into());
            content = content
                .push(p1_bold("Wallet details"))
                .push(p1_regular(format!("Bitcoin mainnet · {script} · {threshold}")))
                .push(p1_regular(format!("Signer fingerprints: {}", if fingerprints.is_empty() { "Not provided" } else { &fingerprints })))
                .push(p1_bold("First receive address"))
                .push(p1_regular(sample_address.clone()).wrapping(iced::widget::text::Wrapping::WordOrGlyph))
                .push(button::secondary_compact(None, "Copy address")
                    .on_press(view::Message::CopyWithToast(sample_address, "Address copied".into())))
                .push(caption("Compare this address with your original wallet before importing.").style(theme::text::secondary))
                .push(p1_regular(format!("History starts: {}",
                    if wallet.birthday == 0 { "Beginning".to_string() } else {
                        chrono::DateTime::from_timestamp(i64::from(wallet.birthday), 0)
                            .map(|date| date.format("%Y-%m-%d").to_string())
                            .unwrap_or_else(|| "Invalid date".into())
                    })))
                .push(p1_regular(format!("Address search: {} receive and {} change addresses", wallet.initial_lookahead, wallet.initial_lookahead)))
                .push(caption("Your balance and transactions will appear after the scan finishes. Then update the Cube Recovery Kit to include this wallet.").style(theme::text::secondary))
                .push(button::secondary_compact(None, if self.show_details { "Hide descriptor details" } else { "Show descriptor details" })
                    .on_press(view::Message::ImportedBitcoin(ImportMessage::ToggleDetails)));
            if self.show_details {
                content = content
                    .push(caption(format!(
                        "Wallet identity: {}",
                        wallet.wallet_id.descriptor_checksum
                    )))
                    .push(
                        caption(format!(
                            "Receive: {}",
                            wallet.descriptor.receive_descriptor()
                        ))
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                        .style(theme::text::secondary),
                    )
                    .push(
                        caption(format!("Change: {}", wallet.descriptor.change_descriptor()))
                            .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                            .style(theme::text::secondary),
                    );
            }
            content = content
                .push(p1_bold("Bitcoin server"))
                .push(p1_regular(self.provider.label()));
            if retry {
                content = content.push(caption("The wallet identity is locked to this Cube. Choose the Bitcoin server to finish an interrupted import."));
                let mut providers = Column::new().spacing(8);
                let mut provider_row = Row::new().spacing(8);
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
                    provider_row =
                        provider_row.push(option.width(iced::Length::Fill).on_press_maybe(
                            (!self.busy).then_some(view::Message::ImportedBitcoin(
                                ImportMessage::ProviderSelected(provider),
                            )),
                        ));
                    if matches!(provider, Provider::Public | Provider::Electrum) {
                        providers = providers.push(provider_row);
                        provider_row = Row::new().spacing(8);
                    }
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
                content = content.push(caption(public_server_label(&self.endpoint)));
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
                                    "Import and scan wallet"
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
        content = content.push(
            button::secondary_compact(None, "Use a new Vault instead").on_press_maybe(
                (!self.busy).then_some(view::Message::ImportedBitcoin(ImportMessage::Abandon)),
            ),
        );
        content.into()
    }

    fn wallet_view(&self, mask_amounts: bool) -> Element<'_, view::Message> {
        let ready = self.is_ready();
        let mut content = Column::new()
            .spacing(16)
            .max_width(if ready { 1500 } else { 800 })
            .push(h2("Vault"));
        if ready {
            if let Some(balance) = self.authoritative_balance() {
                content = content.push(h3(format!(
                    "{} available",
                    passive_wallet_amount(balance.to_sat(), mask_amounts)
                )));
            }
            content = content
                .push(p1_regular("The signing keys remain in your original wallet or hardware devices. Your Cube recovery seed cannot sign for this Bitcoin wallet."))
                .push(p1_regular("Update the Cube Recovery Kit now that this wallet is attached."))
                .push(Row::new().spacing(10)
                    .push_maybe((!self.chain().is_blake2b()).then(||button::primary(None, "Send").on_press(view::Message::ImportedBitcoin(ImportMessage::ShowSend))))
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
                    let (direction, amount) = match tx.kind {
                        model::TransactionKind::IncomingSinglePayment(_)
                        | model::TransactionKind::IncomingPaymentBatch(_) => {
                            ("Received", tx.incoming_amount.to_sat())
                        }
                        model::TransactionKind::SendToSelf => {
                            ("Moved within wallet", tx.incoming_amount.to_sat())
                        }
                        model::TransactionKind::OutgoingSinglePayment(_)
                        | model::TransactionKind::OutgoingPaymentBatch(_) => {
                            ("Sent", tx.outgoing_amount.to_sat())
                        }
                    };
                    let date = tx
                        .time
                        .and_then(|time| {
                            chrono::DateTime::from_timestamp(i64::from(time), 0)
                                .map(|date| date.format("%Y-%m-%d").to_string())
                        })
                        .unwrap_or_else(|| "Date unknown".into());
                    let confirmation = match (tx.height, self.info.as_ref()) {
                        (Some(height), Some(info)) if height > 0 && info.block_height >= height => {
                            format!("{} confirmations", info.block_height - height + 1)
                        }
                        _ => "Unconfirmed".into(),
                    };
                    content = content
                        .push(p1_regular(format!(
                            "{direction} · {} · {date} · {confirmation}",
                            passive_wallet_amount(amount, mask_amounts)
                        )))
                        .push(caption(tx.txid.to_string()));
                }
            }
            if let Some(status) = self
                .info
                .as_ref()
                .and_then(|info| info.imported_wallet.as_ref())
            {
                if !self.chain().is_blake2b() && status.discovery_target < 10_000 {
                    content = content
                        .push(iced::widget::Space::new().height(8))
                        .push(p1_bold("Missing an older transaction?"))
                        .push(caption("Search 200 more receive and change addresses. This starts another history scan.").style(theme::text::secondary))
                        .push(button::secondary_compact(None, "Search more addresses")
                            .on_press_maybe((!self.busy).then_some(
                                view::Message::ImportedBitcoin(ImportMessage::ExpandDiscovery)
                            )));
                }
            }
        } else {
            let scan = ScanDisplay::from_info(self.info.as_ref(), self.sync_failed);
            let label = match scan {
                ScanDisplay::Starting => "Finding your wallet's Bitcoin history",
                ScanDisplay::Scanning { .. } => "Scanning your Bitcoin wallet history",
                ScanDisplay::Saving => "Finishing your Bitcoin history scan",
                ScanDisplay::Retrying => "Bitcoin history scan was interrupted",
                ScanDisplay::NeedsAttention => "Bitcoin history needs attention",
            };
            content = content.push(h3(label));
            if matches!(
                scan,
                ScanDisplay::Starting | ScanDisplay::Scanning { .. } | ScanDisplay::Saving
            ) {
                content = content.push(
                    p1_regular("Your balance and transactions will appear when the scan finishes.")
                        .style(theme::text::secondary),
                );
            }
            match scan {
                ScanDisplay::Scanning {
                    checked,
                    expected,
                    progress,
                } => {
                    content = content
                        .push(
                            progress_bar(0.0..=1.0, progress as f32)
                                .style(theme::progress_bar::primary),
                        )
                        .push(
                            caption(format!(
                                "{:.0}% · {checked} addresses checked (about {expected} expected)",
                                progress * 100.0
                            ))
                            .style(theme::text::secondary),
                        );
                }
                ScanDisplay::Starting | ScanDisplay::Saving => {
                    content = content.push(spinner::typing_text_carousel(
                        "...",
                        true,
                        Duration::from_millis(2000),
                        |value| caption(value).style(theme::text::secondary),
                    ));
                }
                ScanDisplay::Retrying | ScanDisplay::NeedsAttention => {
                    if self.error.is_none() {
                        content = content.push(
                            p1_regular(
                                "Check your connection and Bitcoin server, then retry the scan.",
                            )
                            .style(theme::text::secondary),
                        );
                    }
                    content = content.push(
                        button::secondary_compact(
                            None,
                            if self.sync_requesting {
                                "Retrying…"
                            } else {
                                self.synchronization_action_label()
                            },
                        )
                        .on_press_maybe(
                            (!self.busy && !self.sync_requesting).then_some(
                                view::Message::ImportedBitcoin(ImportMessage::RetrySynchronization),
                            ),
                        ),
                    );
                }
            }
            if matches!(
                scan,
                ScanDisplay::Starting | ScanDisplay::Scanning { .. } | ScanDisplay::Saving
            ) {
                content = content.push(
                    caption("Progress updates automatically. Keep Tenshu open while it scans.")
                        .style(theme::text::secondary),
                );
            }
        }
        if let Some(error) = &self.error {
            content = content.push(p1_regular(error).style(theme::text::error));
        }
        content.into()
    }

    fn send_view(&self, psbts_only: bool) -> Element<'_, view::Message> {
        if self.chain().is_blake2b() {
            return Column::new().spacing(16).push(h2("Send")).push(p1_regular("Sending from paired imported BTCB2 wallets is not available yet. Use the paired Claim flow.")).into();
        }
        let mut content = Column::new()
            .spacing(14)
            .push(h2(if psbts_only { "PSBTs" } else { "Send Bitcoin" }))
            .push(p1_regular("Spend from the imported wallet using its original signer or hardware device. Your Cube seed cannot sign this transaction."))
            .push(button::secondary(None, "Back to overview")
                .on_press(view::Message::Menu(Menu::Vault(VaultSubMenu::Overview))));
        if let Some(txid) = self.last_broadcast_txid {
            content = content.push(p1_bold(format!("Broadcast transaction: {txid}")));
        }
        if let Some(notice) = &self.broadcast_notice {
            content = content.push(caption(notice));
        }
        if let Some(notice) = &self.file_notice {
            content = content.push(caption(notice));
        }
        if let Some(draft) = &self.draft {
            content = content
                .push(p1_bold("Review transaction"))
                .push(caption(format!(
                    "Draft ID: {}",
                    draft.psbt.unsigned_tx.compute_txid()
                )));
            for output in &draft.psbt.unsigned_tx.output {
                let address = Address::from_script(&output.script_pubkey, Network::Bitcoin)
                    .map(|address| address.to_string())
                    .unwrap_or_else(|_| "Unsupported output script".into());
                content = content.push(p1_regular(format!(
                    "{} sats → {address}",
                    output.value.to_sat()
                )));
            }
            if let Ok(fee) = draft.psbt.fee() {
                content = content.push(p1_regular(format!("Network fee: {} sats", fee.to_sat())));
            }
            for warning in &draft.warnings {
                content = content.push(caption(warning));
            }
            content = content
                .push(caption(format!("Valid signer signatures per input: {}",
                    draft.signatures_per_input.iter().map(|count| count.to_string()).collect::<Vec<_>>().join(", "))))
                .push(p1_regular("Save or copy this PSBT for the original signing wallet. Load the signed file or paste its base64 below. For multisig, repeat with each signer."))
                .push(Row::new().spacing(10)
                    .push(button::secondary(None, "Save PSBT file")
                        .on_press_maybe((!self.busy).then_some(view::Message::ImportedBitcoin(ImportMessage::SavePsbt))))
                    .push(button::secondary(None, "Load signed PSBT")
                        .on_press_maybe((!self.busy).then_some(view::Message::ImportedBitcoin(ImportMessage::LoadSignedPsbt))))
                    .push(button::secondary(None, "Copy PSBT")
                        .on_press(view::Message::ImportedBitcoin(ImportMessage::CopyPsbt))))
                .push(p1_bold("Sign with a connected device"));
            if let Some(listing) = &self.hardware_wallets {
                if listing.list.is_empty() {
                    content = content.push(caption(
                        "Connect and unlock a hardware wallet to sign here.",
                    ));
                }
                for device in &listing.list {
                    let label = match device {
                        HardwareWallet::Supported {
                            kind, fingerprint, ..
                        } => {
                            format!("{kind} · {fingerprint}")
                        }
                        HardwareWallet::Locked { kind, .. } => format!("{kind} · unlock device"),
                        HardwareWallet::Unsupported { kind, .. } => format!("{kind} · unsupported"),
                    };
                    content = content.push(caption(label));
                    if device.is_supported() {
                        content =
                            content.push(button::secondary(None, "Sign PSBT").on_press_maybe(
                                (!self.busy).then_some(view::Message::ImportedBitcoin(
                                    ImportMessage::SignWithDevice(device.id().clone()),
                                )),
                            ));
                    }
                }
            }
            content = content
                .push(
                    text_input("Paste signed PSBT (base64)", &self.signed_psbt_text)
                        .on_input(|value| {
                            view::Message::ImportedBitcoin(ImportMessage::SignedPsbtEdited(value))
                        })
                        .padding(10),
                )
                .push(button::secondary(None, "Import signatures").on_press_maybe(
                    (!self.busy && !self.signed_psbt_text.trim().is_empty()).then_some(
                        view::Message::ImportedBitcoin(ImportMessage::ImportSignedPsbt),
                    ),
                ));
            let draft_id = draft.psbt.unsigned_tx.compute_txid();
            let already_broadcast = self.spend_drafts.iter().any(|entry| {
                entry.psbt.unsigned_tx.compute_txid() == draft_id && entry.broadcast_txid.is_some()
            });
            if let Some(final_txid) = draft.final_txid {
                content = content
                    .push(p1_bold(format!(
                        "Ready to broadcast · Final transaction ID: {final_txid}"
                    )))
                    .push(
                        button::primary(
                            None,
                            if self.busy {
                                "Broadcasting…"
                            } else {
                                "Broadcast transaction"
                            },
                        )
                        .on_press_maybe(
                            (!self.busy && self.is_ready() && !already_broadcast).then_some(
                                view::Message::ImportedBitcoin(ImportMessage::Broadcast),
                            ),
                        ),
                    );
                if already_broadcast {
                    content =
                        content.push(caption("This saved transaction was already broadcast."));
                }
            } else {
                content = content.push(caption(
                    "Waiting for enough valid signatures on every input.",
                ));
            }
        } else if !psbts_only {
            content = content
                .push(p1_bold("New transaction"))
                .push(
                    text_input("Bitcoin recipient address", &self.send_recipient)
                        .on_input(|value| {
                            view::Message::ImportedBitcoin(ImportMessage::RecipientEdited(value))
                        })
                        .padding(10),
                )
                .push(
                    text_input("Amount in satoshis", &self.send_amount)
                        .on_input(|value| {
                            view::Message::ImportedBitcoin(ImportMessage::AmountEdited(value))
                        })
                        .padding(10),
                )
                .push(
                    text_input("Fee rate in sat/vB", &self.send_feerate)
                        .on_input(|value| {
                            view::Message::ImportedBitcoin(ImportMessage::FeerateEdited(value))
                        })
                        .padding(10),
                )
                .push(
                    button::primary(
                        None,
                        if self.busy {
                            "Preparing…"
                        } else {
                            "Review PSBT"
                        },
                    )
                    .on_press_maybe(
                        (!self.busy && self.is_ready())
                            .then_some(view::Message::ImportedBitcoin(ImportMessage::CreateDraft)),
                    ),
                );
        }
        if psbts_only && self.draft.is_none() {
            content = content.push(
                button::primary(None, "New transaction")
                    .on_press(view::Message::Menu(Menu::Vault(VaultSubMenu::Send))),
            );
        }
        if !self.spend_drafts.is_empty() {
            content = content.push(p1_bold("Saved transactions"));
            for entry in self.spend_drafts.iter().take(10) {
                let id = entry.psbt.unsigned_tx.compute_txid();
                content = content.push(caption(format!("Draft {id}"))).push(
                    button::secondary(None, "Open saved PSBT").on_press_maybe(
                        (!self.busy).then_some(view::Message::ImportedBitcoin(
                            ImportMessage::SelectDraft(id),
                        )),
                    ),
                );
            }
        }
        if let Some(error) = &self.error {
            content = content.push(p1_regular(error).style(theme::text::error));
        }
        content.into()
    }

    fn settings_view(&self, option: Option<SettingsOption>) -> Element<'_, view::Message> {
        let mut content = Column::new().spacing(16).push(h2("Vault settings"));
        match option.unwrap_or(SettingsOption::Node) {
            SettingsOption::Node => {
                content = content
                    .push(p1_bold("Bitcoin server"))
                    .push(p1_regular(if self.provider_known {
                        self.provider.label()
                    } else {
                        "Server settings unavailable"
                    }))
                    .push(caption(public_server_label(&self.endpoint)))
                    .push(
                        button::secondary(None, "Retry synchronization").on_press_maybe(
                            (!self.busy)
                                .then_some(view::Message::ImportedBitcoin(ImportMessage::Refresh)),
                        ),
                    );
                if let Some(status) = self
                    .info
                    .as_ref()
                    .and_then(|info| info.imported_wallet.as_ref())
                {
                    content = content.push(caption(format!(
                        "History checked: receive {} · change {} · target {} per branch",
                        status.receive_scanned, status.change_scanned, status.discovery_target
                    )));
                    if !self.chain().is_blake2b() && status.discovery_target < 10_000 {
                        content = content.push(
                            button::secondary(None, "Search 200 more addresses").on_press_maybe(
                                (self.is_ready() && !self.busy).then_some(
                                    view::Message::ImportedBitcoin(ImportMessage::ExpandDiscovery),
                                ),
                            ),
                        );
                    }
                }
            }
            SettingsOption::Wallet | SettingsOption::ImportExport => {
                content = content
                    .push(p1_regular("This Vault uses the imported wallet's original signer keys. The Cube seed unlocks the Cube but cannot sign its Bitcoin transactions."))
                    .push(p1_regular("Keep the Cube Recovery Kit current after importing this wallet or reserving a new receive address."));
                if let Some(wallet) = self.import.active_attachment() {
                    content = content.push(caption(format!(
                        "Wallet ID: {}",
                        wallet.wallet_id.descriptor_checksum
                    )));
                    if self.show_details {
                        content = content
                            .push(caption(format!(
                                "Receive: {}",
                                wallet.descriptor.receive_descriptor()
                            )))
                            .push(caption(format!(
                                "Change: {}",
                                wallet.descriptor.change_descriptor()
                            )));
                    }
                    content = content.push(
                        button::secondary(
                            None,
                            if self.show_details {
                                "Hide public descriptors"
                            } else {
                                "Show public descriptors"
                            },
                        )
                        .on_press(view::Message::ImportedBitcoin(ImportMessage::ToggleDetails)),
                    );
                }
            }
            SettingsOption::LocalSigning => {}
        }
        if let Some(error) = &self.error {
            content = content.push(p1_regular(error).style(theme::text::error));
        }
        content.into()
    }
}

fn passive_wallet_amount(sats: u64, mask_amounts: bool) -> String {
    if mask_amounts {
        "••••".to_owned()
    } else {
        format!("{sats} sats")
    }
}

impl State for ImportedBitcoinPanel {
    fn view<'a>(&'a self, menu: &'a Menu, cache: &'a Cache) -> Element<'a, view::Message> {
        let body = if self.import.active_attachment().is_none() {
            self.setup_view()
        } else if !self.is_ready() {
            self.wallet_view(cache.balance_amounts_masked())
        } else {
            match menu {
                Menu::Vault(VaultSubMenu::Send) => self.send_view(false),
                Menu::Vault(VaultSubMenu::PSBTs(_)) => self.send_view(true),
                Menu::Vault(VaultSubMenu::Settings(option)) => self.settings_view(*option),
                _ => self.wallet_view(cache.balance_amounts_masked()),
            }
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
            if !self.is_ready() && self.daemon.is_some() {
                subscriptions.push(iced::time::every(Duration::from_secs(2)).map(|_| {
                    Message::View(view::Message::ImportedBitcoin(ImportMessage::PollProgress))
                }));
            }
        }
        if let Some(hardware_wallets) = &self.hardware_wallets {
            subscriptions.push(hardware_wallets.refresh().map(|m| {
                Message::View(view::Message::ImportedBitcoin(ImportMessage::Hardware(m)))
            }));
        }
        Subscription::batch(subscriptions)
    }

    fn interrupt(&mut self) {
        // Navigation does not cancel a durable install or daemon. Source edits
        // do revoke review evidence through `invalidate()` above.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_source_keyboard_actions_keep_import_setup_operable() {
        use crate::app::view::shared::landing::capture;
        use iced::{advanced::widget::Id, keyboard::key::Named};

        let directory = NetworkDirectory::new(
            std::env::temp_dir().join(format!("import-source-keyboard-{}", uuid::Uuid::new_v4())),
        );
        let mut panel =
            ImportedBitcoinPanel::new(directory, "keyboard".into(), BitcoinImport::pending());
        let size = (1024, 768);
        let mounted = capture::inspect_full_window(
            "import-source-keyboard",
            panel.setup_view(),
            size,
            "light",
            theme::Theme::light(),
        );
        for id in [
            "tenshu.source.descriptor",
            "tenshu.source.hardware",
            "tenshu.source.receive",
            "tenshu.source.change",
        ] {
            assert!(
                mounted.has_focus(Id::new(id)),
                "import source focus: {}",
                id
            );
        }
        for key in [Named::Enter, Named::Space] {
            assert!(matches!(
                capture::activate_with_key(
                    panel.setup_view(),
                    Id::new("tenshu.source.hardware"),
                    size,
                    false,
                    key
                )
                .as_slice(),
                [view::Message::ImportedBitcoin(ImportMessage::Source(
                    wallet_source_form::Message::SourceSelected(Source::Hardware)
                ))]
            ));
        }
        let focus = panel.update_view(
            ImportMessage::Source(wallet_source_form::Message::Focus(Id::new(
                "tenshu.source.hardware",
            ))),
            None,
            0,
        );
        assert!(focus.units() > 0);
        assert_eq!(
            panel.source.source(),
            Source::Descriptor,
            "focus never changes import source"
        );
        let _ = panel.update_view(
            ImportMessage::Source(wallet_source_form::Message::SourceSelected(
                Source::Hardware,
            )),
            None,
            0,
        );
        let hardware = capture::inspect_full_window(
            "import-source-keyboard-hardware",
            panel.setup_view(),
            size,
            "dark",
            theme::Theme::dark(),
        );
        assert!(hardware.has_focus(Id::new("tenshu.hardware.purpose.bip84")));
        assert!(hardware.has_focus(Id::new("tenshu.hardware.account")));
    }

    #[test]
    fn imported_setup_hardware_read_restores_focus_after_loaded_result() {
        use crate::services::public_hardware_source::tests::{
            add_fake_device, assert_focus_task, loaded_for_test, unique_temp_root,
        };
        use crate::split_hardware::HardwareMessage;
        use iced::advanced::widget::Id;

        let root = unique_temp_root("import-keyboard-read-return");
        for succeeds in [true, false] {
            let directory = NetworkDirectory::new(root.join(format!("import-{succeeds}")));
            let mut panel =
                ImportedBitcoinPanel::new(directory, "keyboard".into(), BitcoinImport::pending());
            panel.source.select_source(Source::Hardware);
            add_fake_device(panel.source.hardware_mut(), &root);
            let reading = panel.update_view(
                ImportMessage::Source(wallet_source_form::Message::Hardware(
                    HardwareMessage::Read("dev-1".into()),
                )),
                None,
                0,
            );
            assert_focus_task(
                reading,
                panel.setup_view().map(Message::View),
                Id::new("tenshu.hardware.purpose.bip84"),
            );
            let generation = panel.source.hardware_mut().generation_for_test();
            let loaded = panel.update_view(
                ImportMessage::Source(wallet_source_form::Message::Hardware(loaded_for_test(
                    generation, succeeds,
                ))),
                None,
                0,
            );
            assert_focus_task(
                loaded,
                panel.setup_view().map(Message::View),
                Id::from("tenshu.hardware.read:5:dev-1".to_string()),
            );
            assert_eq!(panel.source.hardware_account().is_some(), succeeds);
            if succeeds {
                let _ = panel.update_view(
                    ImportMessage::Source(wallet_source_form::Message::Hardware(
                        HardwareMessage::PurposeSelected(crate::split_hardware::Purpose::Bip84),
                    )),
                    None,
                    0,
                );
                assert!(panel.source.hardware_account().is_some());
                let _ = panel.update_view(
                    ImportMessage::Source(wallet_source_form::Message::Hardware(
                        HardwareMessage::PurposeSelected(crate::split_hardware::Purpose::Bip49),
                    )),
                    None,
                    0,
                );
                assert!(panel.source.hardware_account().is_none());
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }

    use coincube_core::miniscript::bitcoin::{
        absolute::LockTime,
        bip32::{ChildNumber, DerivationPath, Xpriv, Xpub},
        secp256k1::Secp256k1,
        transaction, Amount, OutPoint, Transaction, Txid,
    };
    use coincubed::commands::{
        GetInfoDescriptors, ImportedWalletSync, LCSpendInfo, ListCoinsEntry, ListCoinsResult,
    };
    use serde::{de::DeserializeOwned, Serialize};

    #[derive(Debug)]
    struct NoRpcClient;

    impl crate::daemon::client::Client for NoRpcClient {
        type Error = crate::daemon::DaemonError;

        fn request<S: Serialize + std::fmt::Debug, D: DeserializeOwned + std::fmt::Debug>(
            &self,
            _method: &str,
            _params: Option<S>,
        ) -> Result<D, Self::Error> {
            unreachable!("binding a daemon must not make an RPC request")
        }
    }

    #[test]
    fn import_calendar_preserves_text_until_selection_and_validates_dates() {
        let directory = NetworkDirectory::new(std::env::temp_dir().join("import-calendar-test"));
        let mut panel =
            ImportedBitcoinPanel::new(directory, "calendar".into(), BitcoinImport::pending());
        macro_rules! send {
            ($message:expr) => {
                let _ = panel.update_view($message, None, 0);
            };
        }
        send!(ImportMessage::ScanFromBeginning(false));
        send!(ImportMessage::BirthdayEdited("2024-02-".into()));
        send!(ImportMessage::BirthdayCalendarOpen);
        assert!(panel.birthday_calendar_open);
        send!(ImportMessage::BirthdayCalendarCancel);
        assert_eq!(panel.birthday, "2024-02-");
        assert!(!panel.birthday_calendar_open);
        send!(ImportMessage::BirthdayCalendarOpen);
        let leap_day = chrono::NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
        send!(ImportMessage::BirthdaySelected(leap_day));
        assert_eq!(panel.birthday, "2024-02-29");
        assert!(!panel.birthday_calendar_open);
        let timestamp = birthday_timestamp(leap_day).unwrap();
        assert_eq!(
            chrono::DateTime::from_timestamp(i64::from(timestamp), 0)
                .unwrap()
                .date_naive(),
            leap_day
        );
        assert!(birthday_timestamp(chrono::NaiveDate::from_ymd_opt(2009, 1, 3).unwrap()).is_ok());
        send!(ImportMessage::BirthdayCalendarOpen);
        send!(ImportMessage::BirthdaySelected(
            chrono::Utc::now().date_naive().succ_opt().unwrap()
        ));
        assert_eq!(panel.birthday, "2024-02-29");
        assert!(panel.error.is_some());
        assert!(birthday_timestamp(chrono::NaiveDate::from_ymd_opt(2008, 1, 1).unwrap()).is_err());
        send!(ImportMessage::BirthdayCalendarOpen);
        send!(ImportMessage::ScanFromBeginning(true));
        assert!(!panel.birthday_calendar_open);
        send!(ImportMessage::BirthdaySelected(
            chrono::NaiveDate::from_ymd_opt(2020, 1, 1).unwrap()
        ));
        assert_eq!(panel.birthday, "2024-02-29");
        send!(ImportMessage::BirthdayCalendarOpen);
        assert!(!panel.birthday_calendar_open);
    }

    #[test]
    fn shared_runtime_provider_metadata_labels_custom_esplora() {
        let secp = Secp256k1::new();
        let master = Xpriv::new_master(Network::Bitcoin, &[44; 32]).unwrap();
        let account = master
            .derive_priv(&secp, &DerivationPath::from_str("m/84'/0'/0'").unwrap())
            .unwrap();
        let key = format!(
            "[{}/84'/0'/0']{}",
            master.fingerprint(&secp),
            Xpub::from_priv(&secp, &account)
        );
        let descriptor = ImportedWalletDescriptor::parse(
            &format!("wpkh({key}/0/*)"),
            Some(&format!("wpkh({key}/1/*)")),
            ChainId::Bitcoin,
        )
        .unwrap();
        let directory = NetworkDirectory::new(
            std::env::temp_dir().join(format!("import-provider-test-{}", uuid::Uuid::new_v4())),
        );
        let wallet =
            ImportedWalletAttachment::new(descriptor.clone(), ChainId::Bitcoin, 0, 200).unwrap();
        let mut import = BitcoinImport::pending();
        import.stage = BitcoinImportStage::Ready {
            wallet: wallet.clone(),
        };
        let mut panel =
            ImportedBitcoinPanel::new(directory.clone(), "cube-1".into(), import.clone());
        let mut config = Config::new(
            BitcoinConfig::new(ChainId::Bitcoin, Duration::from_secs(30)),
            Some(BitcoinBackend::Esplora(EsploraConfig {
                addr: "http://localhost:18184".into(),
                token: None,
                fallback_addr: None,
                fallback_token: None,
                secondary_fallback_addr: None,
                secondary_fallback_token: None,
            })),
            log::LevelFilter::Info,
            WalletDescriptor::Imported(descriptor.clone()),
            directory.coincubed_data_directory(&wallet.wallet_id),
        );
        config.import_birthday = Some(0);
        config.import_initial_lookahead = Some(200);

        // A loader-owned daemon shares its handle without starting a second
        // daemon, while the Vault settings still reflect its actual server.
        let daemon: Arc<dyn Daemon + Send + Sync> =
            Arc::new(EmbeddedDaemon::unstarted_for_test(config.clone(), None));
        let _refresh = panel.bind_runtime(daemon.clone());
        assert!(Arc::ptr_eq(panel.daemon.as_ref().unwrap(), &daemon));
        assert!(!panel.owns_daemon);
        assert_eq!(panel.provider, Provider::Esplora);
        assert_eq!(
            public_server_label(&panel.endpoint),
            "http://localhost:18184"
        );
        assert_eq!(panel.provider.label(), "My Esplora server");

        // The external daemon exposes no Config. Binding reads the same
        // installed metadata without starting another daemon or making RPCs.
        let path = directory
            .coincubed_data_directory(&wallet.wallet_id)
            .path()
            .join("daemon.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, toml::to_string(&config.for_persistence()).unwrap()).unwrap();
        let mut external_panel =
            ImportedBitcoinPanel::new(directory.clone(), "cube-1".into(), import);
        let external: Arc<dyn Daemon + Send + Sync> =
            Arc::new(crate::daemon::client::Coincubed::new(NoRpcClient));
        let _refresh = external_panel.bind_runtime(external.clone());
        assert!(Arc::ptr_eq(
            external_panel.daemon.as_ref().unwrap(),
            &external
        ));
        assert_eq!(external_panel.provider, Provider::Esplora);
        assert_eq!(
            public_server_label(&external_panel.endpoint),
            "http://localhost:18184"
        );
        std::fs::remove_dir_all(directory.path()).unwrap();
    }

    #[test]
    fn server_caption_omits_credentials_path_and_query() {
        assert_eq!(
            public_server_label("https://name:secret@example.com:8443/token-path?api_key=secret"),
            "https://example.com:8443"
        );
    }

    fn ready_import_panel() -> ImportedBitcoinPanel {
        let secp = Secp256k1::new();
        let master = Xpriv::new_master(Network::Bitcoin, &[43; 32]).unwrap();
        let account = master
            .derive_priv(&secp, &DerivationPath::from_str("m/84'/0'/0'").unwrap())
            .unwrap();
        let key = format!(
            "[{}/84'/0'/0']{}",
            master.fingerprint(&secp),
            Xpub::from_priv(&secp, &account)
        );
        let descriptor = ImportedWalletDescriptor::parse(
            &format!("wpkh({key}/0/*)"),
            Some(&format!("wpkh({key}/1/*)")),
            ChainId::Bitcoin,
        )
        .unwrap();
        let wallet =
            ImportedWalletAttachment::new(descriptor.clone(), ChainId::Bitcoin, 0, 200).unwrap();
        let mut import = BitcoinImport::pending();
        import.stage = BitcoinImportStage::Ready { wallet };
        let directory = NetworkDirectory::new(std::env::temp_dir().join("import-ready-test"));
        let mut panel = ImportedBitcoinPanel::new(directory, "cube-1".into(), import);
        panel.set_hardware_root(CoincubeDirectory::new(
            std::env::temp_dir().join("import-ready-root-test"),
        ));
        panel.info = Some(model::GetInfoResult {
            version: "test".into(),
            network: Network::Bitcoin,
            chain_id: Some(ChainId::Bitcoin),
            block_height: 1,
            sync: 1.0,
            descriptors: GetInfoDescriptors {
                main: WalletDescriptor::Imported(descriptor.clone()),
            },
            imported_wallet: Some(ImportedWalletSync {
                wallet_identity: descriptor.identity(ChainId::Bitcoin).unwrap().to_string(),
                chain_id: ChainId::Bitcoin,
                birthday: 0,
                initial_lookahead: 200,
                discovery_target: 200,
                receive_scanned: 200,
                change_scanned: 200,
                history_complete: true,
            }),
            rescan_progress: None,
            refused_reorg_depth: None,
            chain_divergence: false,
            timestamp: 0,
            last_poll_timestamp: Some(1),
            history_sync: Default::default(),
            receive_index: 0,
            change_index: 0,
        });
        panel.coins = Some(ListCoinsResult { coins: Vec::new() });
        panel
    }

    #[tokio::test]
    async fn imported_receive_shares_source_claim_lock_and_releases_after_daemon_error() {
        let panel = ready_import_panel();
        let imported = panel.import.active_attachment().unwrap().descriptor.clone();
        let source = coincube_core::foreign_split::SplitSource::new(
            imported.receive_descriptor().clone(),
            Some(imported.change_descriptor().clone()),
        )
        .unwrap();
        let root = CoincubeDirectory::new(
            std::env::temp_dir().join(format!("receive-claim-lock-{}", uuid::Uuid::new_v4())),
        );
        let config = Config::new(
            BitcoinConfig::new(ChainId::Bitcoin, Duration::from_secs(30)),
            None,
            log::LevelFilter::Off,
            WalletDescriptor::Imported(imported.clone()),
            root.network_directory(ChainId::Bitcoin)
                .coincubed_data_directory(&panel.import.active_attachment().unwrap().wallet_id),
        );
        let daemon: Arc<dyn Daemon + Send + Sync> =
            Arc::new(EmbeddedDaemon::unstarted_for_test(config, None));
        let guard =
            bitcoin_import::pairing::legacy::source_reservation_lock(&root, &source).unwrap();
        for descriptor in [imported.clone(), imported.paired_blake2b().unwrap()] {
            let error = reserve_receive_address(root.clone(), descriptor.into(), daemon.clone())
                .await
                .unwrap_err();
            assert!(error.contains("reserving addresses"), "{}", error);
        }
        drop(guard);
        // A failed daemon request must drop the allocation guard too. The
        // source and paired target can each retry rather than keep a stale lock.
        for descriptor in [imported.clone(), imported.paired_blake2b().unwrap()] {
            let error = reserve_receive_address(root.clone(), descriptor.into(), daemon.clone())
                .await
                .unwrap_err();
            assert!(!error.contains("reserving addresses"), "{}", error);
            assert!(
                bitcoin_import::pairing::legacy::source_reservation_lock(&root, &source).is_ok()
            );
        }
        std::fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn paired_imported_discovery_refusal_preserves_shared_runtime_and_snapshot() {
        let mut panel = ready_import_panel();
        let original = panel.import.active_attachment().unwrap();
        let descriptor = original.descriptor.paired_blake2b().unwrap();
        let wallet = ImportedWalletAttachment::new(
            descriptor.clone(),
            ChainId::BitcoinBlake2b,
            original.birthday,
            original.initial_lookahead,
        )
        .unwrap();
        panel.import.stage = BitcoinImportStage::Ready { wallet };
        let info = panel.info.as_mut().unwrap();
        info.chain_id = Some(ChainId::BitcoinBlake2b);
        info.network = ChainId::BitcoinBlake2b.bitcoin_network();
        info.descriptors.main = WalletDescriptor::Imported(descriptor.clone());
        let status = info.imported_wallet.as_mut().unwrap();
        status.chain_id = ChainId::BitcoinBlake2b;
        status.wallet_identity = descriptor
            .identity(ChainId::BitcoinBlake2b)
            .unwrap()
            .to_string();
        let daemon: Arc<dyn Daemon + Send + Sync> =
            Arc::new(crate::daemon::client::Coincubed::new(NoRpcClient));
        panel.daemon = Some(daemon.clone());
        panel.owns_daemon = false;
        assert!(panel.is_ready());
        assert!(panel.verified_address_state().is_some());
        let generation = panel.generation;
        let before = serde_json::to_value(panel.info.as_ref().unwrap()).unwrap();
        let _ = panel.update_view(ImportMessage::ExpandDiscovery, None, 0);
        assert!(Arc::ptr_eq(panel.daemon.as_ref().unwrap(), &daemon));
        assert!(!panel.owns_daemon);
        assert_eq!(panel.generation, generation);
        assert_eq!(panel.generation_gate.load(Ordering::SeqCst), generation);
        assert_eq!(
            serde_json::to_value(panel.info.as_ref().unwrap()).unwrap(),
            before
        );
        assert!(panel.coins.is_some());
        assert!(panel.is_ready());
        assert!(!panel.busy);
        assert!(panel
            .error
            .as_deref()
            .unwrap()
            .contains("original Bitcoin Cube"));

        // The original Bitcoin wallet retains the supported expansion route.
        let mut bitcoin = ready_import_panel();
        bitcoin.daemon = Some(daemon);
        let generation = bitcoin.generation;
        let _ = bitcoin.update_view(ImportMessage::ExpandDiscovery, None, 0);
        assert!(bitcoin.daemon.is_none());
        assert_ne!(bitcoin.generation, generation);
        assert!(bitcoin.info.is_none());
        assert!(bitcoin.busy);
    }

    #[test]
    fn scan_display_uses_only_reported_progress_and_never_claims_completion_early() {
        let mut info = ready_import_panel().info.unwrap();
        info.history_sync = coincubed::commands::HistorySync {
            full_scan_in_progress: true,
            addresses_checked: 100,
            addresses_expected: 400,
            ..Default::default()
        };
        assert_eq!(
            ScanDisplay::from_info(Some(&info), false),
            ScanDisplay::Scanning {
                checked: 100,
                expected: 400,
                progress: 0.25
            }
        );
        info.history_sync.addresses_checked = 600;
        assert_eq!(
            ScanDisplay::from_info(Some(&info), false),
            ScanDisplay::Scanning {
                checked: 600,
                expected: 400,
                progress: 0.99
            }
        );
        info.history_sync.addresses_expected = 0;
        assert_eq!(
            ScanDisplay::from_info(Some(&info), false),
            ScanDisplay::Starting
        );
        info.history_sync.full_scan_in_progress = false;
        info.history_sync.full_scan_completed_at = Some(1);
        assert_eq!(
            ScanDisplay::from_info(Some(&info), false),
            ScanDisplay::Saving
        );
        assert_eq!(
            ScanDisplay::from_info(Some(&info), true),
            ScanDisplay::Retrying
        );
    }

    #[test]
    fn progress_poll_keeps_partial_balance_unknown_and_recovers_after_error() {
        let mut panel = ready_import_panel();
        panel.coins = None;
        let mut scanning = panel.info.take().unwrap();
        scanning.imported_wallet.as_mut().unwrap().history_complete = false;
        scanning.history_sync = coincubed::commands::HistorySync {
            full_scan_in_progress: true,
            addresses_checked: 20,
            addresses_expected: 400,
            ..Default::default()
        };
        panel.progress_polling = true;
        let generation = panel.generation;
        let _ = panel.update_event(ImportEvent::ProgressChecked(
            generation,
            Ok(scanning.clone()),
        ));
        assert!(!panel.progress_polling);
        assert!(!panel.is_ready());
        assert!(panel.authoritative_balance().is_none());
        assert!(matches!(
            ScanDisplay::from_info(panel.info.as_ref(), panel.sync_failed),
            ScanDisplay::Scanning { progress, .. } if (progress - 0.05).abs() < f64::EPSILON
        ));

        panel.progress_polling = true;
        let _ = panel.update_event(ImportEvent::ProgressChecked(
            generation,
            Err("server error".into()),
        ));
        assert!(panel.sync_failed);
        assert!(!panel.progress_polling);
        assert_eq!(
            panel.synchronization_action_label(),
            "Retry synchronization"
        );
        assert!(panel
            .error
            .as_deref()
            .unwrap()
            .contains("retry synchronization"));

        panel.progress_polling = true;
        let _ = panel.update_event(ImportEvent::ProgressChecked(generation, Ok(scanning)));
        assert!(!panel.sync_failed);
        assert!(panel.error.is_none());
        assert!(!panel.is_ready());
    }

    #[test]
    fn progress_status_from_another_wallet_keeps_retry_actionable() {
        let mut panel = ready_import_panel();
        panel.info = None;
        panel.coins = None;
        let mut wrong = ready_import_panel().info.unwrap();
        wrong.imported_wallet.as_mut().unwrap().wallet_identity = "another-wallet".into();
        let generation = panel.generation;
        let _ = panel.update_event(ImportEvent::ProgressChecked(generation, Ok(wrong)));
        assert!(panel.sync_failed);
        assert!(panel.info.is_none());
        assert!(panel.error.as_deref().unwrap().contains("different wallet"));
        assert_eq!(
            panel.synchronization_action_label(),
            "Retry synchronization"
        );

        let mut correct = ready_import_panel().info.unwrap();
        correct.imported_wallet.as_mut().unwrap().history_complete = false;
        let _ = panel.update_event(ImportEvent::ProgressChecked(generation, Ok(correct)));
        assert!(!panel.sync_failed);
        assert!(panel.error.is_none());
        assert!(!panel.is_ready());
        panel.progress_polling = true;
        panel.invalidate();
        assert!(!panel.progress_polling);
    }

    #[test]
    fn completed_status_alone_cannot_reuse_an_old_coin_snapshot() {
        let mut panel = ready_import_panel();
        let complete = panel.info.take().unwrap();
        assert!(panel.coins.is_some());
        let generation = panel.generation;
        let _ = panel.update_event(ImportEvent::ProgressChecked(generation, Ok(complete)));
        assert!(panel.coins.is_none());
        assert!(!panel.is_ready());
        assert!(panel.authoritative_balance().is_none());
    }

    #[test]
    fn explicit_retry_wakes_sync_without_blocking_live_status_polling() {
        let mut panel = ready_import_panel();
        panel.info.as_mut().unwrap().history_sync.last_failure =
            Some(coincubed::commands::PollFailure {
                message: "server unavailable".into(),
                since: 1,
                last_at: 1,
                consecutive: 1,
            });
        let wallet = panel.import.active_attachment().unwrap();
        let config = Config::new(
            BitcoinConfig::new(ChainId::Bitcoin, Duration::from_secs(600)),
            None,
            log::LevelFilter::Info,
            WalletDescriptor::Imported(wallet.descriptor.clone()),
            panel.directory.coincubed_data_directory(&wallet.wallet_id),
        );
        panel.daemon = Some(Arc::new(EmbeddedDaemon::unstarted_for_test(config, None)));

        let _ = panel.update_view(ImportMessage::RetrySynchronization, None, 0);
        assert!(panel.sync_requesting);
        assert!(!panel.busy);
        let generation = panel.generation;
        let _ = panel.update_event(ImportEvent::SynchronizationRequested(generation, Ok(())));
        assert!(!panel.sync_requesting);
        assert!(panel.progress_polling);
    }

    #[test]
    fn failed_full_snapshot_waits_for_retry_or_normal_refresh() {
        let mut panel = ready_import_panel();
        let complete = panel.info.as_ref().unwrap().clone();
        let wallet = panel.import.active_attachment().unwrap();
        let config = Config::new(
            BitcoinConfig::new(ChainId::Bitcoin, Duration::from_secs(600)),
            None,
            log::LevelFilter::Info,
            WalletDescriptor::Imported(wallet.descriptor.clone()),
            panel.directory.coincubed_data_directory(&wallet.wallet_id),
        );
        panel.daemon = Some(Arc::new(EmbeddedDaemon::unstarted_for_test(config, None)));
        let generation = panel.generation;
        let _ = panel.update_event(ImportEvent::Refreshed(generation, Err("disk error".into())));
        assert!(panel.snapshot_failed);
        let failure = panel.error.clone();

        for _ in 0..2 {
            let _ = panel.update_event(ImportEvent::ProgressChecked(
                generation,
                Ok(complete.clone()),
            ));
            assert!(
                !panel.busy,
                "status polling must not requeue a full snapshot"
            );
            assert!(panel.snapshot_failed);
            assert_eq!(panel.error, failure);
            assert!(panel.coins.is_none());
        }

        let _ = panel.update_view(ImportMessage::RetrySynchronization, None, 0);
        assert!(!panel.snapshot_failed);
        let _ = panel.update_event(ImportEvent::SynchronizationRequested(generation, Ok(())));
        let _ = panel.update_event(ImportEvent::ProgressChecked(generation, Ok(complete)));
        assert!(
            panel.busy,
            "explicit retry may start the full snapshot again"
        );

        let _ = panel.update_event(ImportEvent::Refreshed(generation, Err("disk error".into())));
        assert!(panel.snapshot_failed);
        let _ = panel.update_view(ImportMessage::Refresh, None, 0);
        assert!(!panel.snapshot_failed);
        assert!(
            panel.busy,
            "the normal slower refresh may retry the snapshot"
        );
    }

    #[test]
    fn busy_ready_startup_tracks_scan_but_old_status_cannot_undo_receive_reservation() {
        let mut panel = ready_import_panel();
        let mut scanning = panel.info.take().unwrap();
        scanning.imported_wallet.as_mut().unwrap().history_complete = false;
        scanning.history_sync = coincubed::commands::HistorySync {
            full_scan_in_progress: true,
            addresses_checked: 50,
            addresses_expected: 400,
            ..Default::default()
        };
        panel.busy = true;
        let generation = panel.generation;
        let _ = panel.update_event(ImportEvent::ProgressChecked(generation, Ok(scanning)));
        assert_eq!(
            panel.info.as_ref().unwrap().history_sync.addresses_checked,
            50
        );

        panel.busy = false;
        let wallet = panel.import.active_attachment().unwrap();
        let config = Config::new(
            BitcoinConfig::new(ChainId::Bitcoin, Duration::from_secs(600)),
            None,
            log::LevelFilter::Info,
            WalletDescriptor::Imported(wallet.descriptor.clone()),
            panel.directory.coincubed_data_directory(&wallet.wallet_id),
        );
        panel.daemon = Some(Arc::new(EmbeddedDaemon::unstarted_for_test(config, None)));
        // The real Receive action is available only after history is complete.
        let completed = ready_import_panel().info.unwrap();
        panel.info = Some(completed.clone());
        panel.coins = Some(ListCoinsResult { coins: Vec::new() });
        let old_generation = panel.generation;
        panel.progress_polling = true;
        panel.sync_requesting = true;
        let _ = panel.update_view(ImportMessage::Receive, None, 0);
        assert!(panel.address_reserving);
        assert!(!panel.sync_requesting);
        assert_ne!(panel.generation, old_generation);
        assert!(panel.info.is_none());
        let new_generation = panel.generation;
        let _ = panel.update_event(ImportEvent::Address(new_generation, Err("retry".into())));
        assert!(!panel.address_reserving);
        let _ = panel.update_event(ImportEvent::ProgressChecked(old_generation, Ok(completed)));
        assert!(panel.info.is_none());
        assert!(panel.verified_address_state().is_none());
    }

    #[test]
    fn completed_import_stays_ready_after_receive_or_change_allocation() {
        let mut panel = ready_import_panel();
        assert!(panel.is_ready());

        // Ordinary address allocation advances durable high-water indices but
        // does not invalidate the attested, completed historical scan.
        panel.info.as_mut().unwrap().receive_index = 1;
        panel.info.as_mut().unwrap().change_index = 1;
        assert!(panel.is_ready());

        // A restored or wider scan clears the durable completion marker first.
        panel
            .info
            .as_mut()
            .unwrap()
            .imported_wallet
            .as_mut()
            .unwrap()
            .history_complete = false;
        assert!(!panel.is_ready());
        let status = panel
            .info
            .as_mut()
            .unwrap()
            .imported_wallet
            .as_mut()
            .unwrap();
        status.history_complete = true;
        status.discovery_target = 300;
        assert!(!panel.is_ready());
    }

    #[test]
    fn claim_source_requires_attested_completed_scan_and_covers_allocated_indices() {
        let mut panel = ready_import_panel();
        let mut cube = settings::CubeSettings::new_with_raw_id(
            "cube-1".into(),
            "Imported".into(),
            ChainId::Bitcoin,
        );
        cube.bitcoin_import = Some(Box::new(panel.import.clone()));
        let source = panel.claim_source(&cube).unwrap();
        assert_eq!(source.address_window, 200);
        assert_eq!(source.cube_id, cube.id);
        assert_eq!(
            source.external,
            panel
                .import
                .active_attachment()
                .unwrap()
                .descriptor
                .receive_descriptor()
                .to_string()
        );
        assert_eq!(
            source.internal,
            panel
                .import
                .active_attachment()
                .unwrap()
                .descriptor
                .change_descriptor()
                .to_string()
        );

        // A harmless background refresh can be in flight while the last
        // completed snapshot is still authoritative. Claim scans afresh.
        panel.busy = true;
        panel.sync_requesting = true;
        assert!(panel.claim_source(&cube).is_ok());
        panel.busy = false;
        panel.sync_requesting = false;

        panel.info.as_mut().unwrap().receive_index = 250;
        assert_eq!(panel.claim_source(&cube).unwrap().address_window, 251);
        // Completed discovery can extend beyond both the configured gap and
        // allocated indices. Claim includes that entire attested history.
        {
            let status = panel
                .info
                .as_mut()
                .unwrap()
                .imported_wallet
                .as_mut()
                .unwrap();
            status.receive_scanned = 450;
            status.change_scanned = 500;
        }
        assert_eq!(panel.claim_source(&cube).unwrap().address_window, 500);
        panel.info.as_mut().unwrap().change_index = 10_000;
        assert!(panel.claim_source(&cube).is_err());
        panel.info.as_mut().unwrap().change_index = 0;
        panel
            .info
            .as_mut()
            .unwrap()
            .history_sync
            .full_scan_in_progress = true;
        assert!(panel.claim_source(&cube).is_err());
        panel
            .info
            .as_mut()
            .unwrap()
            .history_sync
            .full_scan_in_progress = false;
        panel.snapshot_failed = true;
        assert!(panel.claim_source(&cube).is_err());
        panel.snapshot_failed = false;
        cube.name = "Renamed".into();
        assert_eq!(panel.claim_source(&cube).unwrap().name, "Renamed");
        cube.bitcoin_import = Some(Box::new(BitcoinImport::pending()));
        assert!(panel.claim_source(&cube).is_err());
    }

    #[test]
    fn imported_home_available_balance_matches_vault_coin_ownership() {
        let mut panel = ready_import_panel();
        let txid =
            Txid::from_str("f7bd1b2a995b689d326e51eb742eb1088c4a8f110d9cb56128fd553acc9f88e5")
                .unwrap();
        let address = coincube_core::miniscript::bitcoin::Address::from_str(
            "bc1qvrl2849aggm6qry9ea7xqp2kk39j8vaa8r3cwg",
        )
        .unwrap()
        .assume_checked();
        let coin = |vout, sats, block_height, is_from_self, is_change, spend_info| ListCoinsEntry {
            outpoint: OutPoint::new(txid, vout),
            amount: Amount::from_sat(sats),
            address: address.clone(),
            derivation_index: ChildNumber::Normal { index: vout },
            block_height,
            is_immature: false,
            is_change,
            is_from_self,
            spend_info,
        };
        panel.coins = Some(ListCoinsResult {
            coins: vec![
                coin(0, 1_000, Some(1), false, false, None), // confirmed incoming
                coin(1, 2_000, None, false, false, None),    // pending external incoming
                coin(2, 3_000, None, true, true, None),      // pending self-change
                coin(
                    3,
                    4_000,
                    Some(1),
                    false,
                    false,
                    Some(LCSpendInfo {
                        txid,
                        height: Some(2),
                    }),
                ), // spent coin
            ],
        });

        let mut cache = Cache::default();
        assert!(panel.sync_cache(&mut cache));
        assert_eq!(cache.coins().len(), 4);
        let (available, pending, _, _) = crate::app::state::coins_summary(cache.coins(), 1, None);
        assert_eq!(available, Amount::from_sat(4_000));
        assert_eq!(pending, Amount::from_sat(2_000));
        assert_eq!(panel.authoritative_balance(), Some(available));
    }

    #[test]
    fn late_event_from_closed_panel_cannot_mutate_reopened_cube() {
        let directory =
            NetworkDirectory::new(std::env::temp_dir().join("import-panel-generation-test"));
        let import = BitcoinImport::pending();
        let mut old = ImportedBitcoinPanel::new(directory.clone(), "cube-1".into(), import.clone());
        let old_generation = old.generation;
        old.stop();

        let mut reopened = ImportedBitcoinPanel::new(directory, "cube-1".into(), import);
        assert_ne!(old_generation, reopened.generation);
        let _ = reopened.update_event(ImportEvent::Abandoned(old_generation, Ok(())));
        assert!(!reopened.abandoned());
    }

    #[test]
    fn backend_error_pages_are_not_shown_and_offer_synchronization_retry() {
        let directory = NetworkDirectory::new(std::env::temp_dir().join("import-panel-error-test"));
        let mut panel =
            ImportedBitcoinPanel::new(directory, "cube-1".into(), BitcoinImport::pending());
        let generation = panel.generation;
        let raw = "Error setting up Esplora: <!DOCTYPE HTML><html>404</html>".to_string();

        let _ = panel.update_event(ImportEvent::Opened(generation, Err(raw.clone())));
        assert_eq!(
            panel.synchronization_action_label(),
            "Retry synchronization"
        );
        assert!(!panel.error.as_deref().unwrap().contains("<!DOCTYPE"));
        assert!(panel.error.as_deref().unwrap().contains("Bitcoin server"));

        let _ = panel.update_event(ImportEvent::Refreshed(generation, Err(raw)));
        assert_eq!(
            panel.synchronization_action_label(),
            "Retry synchronization"
        );
        assert!(!panel.error.as_deref().unwrap().contains("<!DOCTYPE"));
    }

    #[test]
    fn pending_import_uses_vault_routes_but_never_inheritance_actions() {
        let directory = NetworkDirectory::new(std::env::temp_dir().join("import-panel-route-test"));
        let panel = ImportedBitcoinPanel::new(directory, "cube-1".into(), BitcoinImport::pending());
        assert!(panel.handles_route(&Menu::Vault(VaultSubMenu::Overview)));
        assert!(panel.handles_route(&Menu::Vault(VaultSubMenu::Receive)));
        assert!(panel.handles_route(&Menu::Vault(VaultSubMenu::Send)));
        assert!(!panel.handles_route(&Menu::Vault(VaultSubMenu::Recovery)));
        assert!(!panel.handles_route(&Menu::Vault(VaultSubMenu::Claim)));
        assert!(
            !panel.handles_route(&Menu::Vault(VaultSubMenu::Settings(Some(
                SettingsOption::LocalSigning,
            ))))
        );
    }

    #[test]
    fn selecting_missing_saved_transaction_clears_previous_draft() {
        let directory = NetworkDirectory::new(std::env::temp_dir().join("import-panel-draft-test"));
        let mut panel =
            ImportedBitcoinPanel::new(directory, "cube-1".into(), BitcoinImport::pending());
        let psbt = Psbt::from_unsigned_tx(Transaction {
            version: transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: Vec::new(),
            output: Vec::new(),
        })
        .unwrap();
        let id = psbt.unsigned_tx.compute_txid();
        panel.draft = Some(ImportedDraft {
            psbt,
            signatures_per_input: Vec::new(),
            final_txid: Some(id),
            warnings: Vec::new(),
        });
        panel.select_saved_draft(id);
        assert!(panel.draft.is_none());
        assert!(panel
            .error
            .as_deref()
            .unwrap()
            .contains("no longer available"));
    }

    #[test]
    fn late_signer_results_cannot_replace_a_different_selected_psbt() {
        let directory =
            NetworkDirectory::new(std::env::temp_dir().join("import-panel-signing-test"));
        let mut panel =
            ImportedBitcoinPanel::new(directory, "cube-1".into(), BitcoinImport::pending());
        let make_psbt = |version| {
            Psbt::from_unsigned_tx(Transaction {
                version,
                lock_time: LockTime::ZERO,
                input: Vec::new(),
                output: Vec::new(),
            })
            .unwrap()
        };
        let prior = make_psbt(transaction::Version::ONE);
        let selected = make_psbt(transaction::Version::TWO);
        let prior_id = prior.unsigned_tx.compute_txid();
        let selected_id = selected.unsigned_tx.compute_txid();
        panel.draft = Some(ImportedDraft {
            psbt: selected,
            signatures_per_input: Vec::new(),
            final_txid: None,
            warnings: Vec::new(),
        });
        let generation = panel.generation;
        let _ = panel.update_event(ImportEvent::SignedSaved(
            generation,
            prior_id,
            Ok((prior.clone(), vec![], None)),
        ));
        assert_eq!(
            panel
                .draft
                .as_ref()
                .unwrap()
                .psbt
                .unsigned_tx
                .compute_txid(),
            selected_id
        );
        let _ = panel.update_event(ImportEvent::HardwareSigned(
            generation,
            prior_id,
            Ok((prior.clone(), vec![], None)),
        ));
        assert_eq!(
            panel
                .draft
                .as_ref()
                .unwrap()
                .psbt
                .unsigned_tx
                .compute_txid(),
            selected_id
        );
        let _ = panel.update_event(ImportEvent::FileLoaded(
            generation,
            prior_id,
            Ok(Some((prior, vec![], None))),
        ));
        assert_eq!(
            panel
                .draft
                .as_ref()
                .unwrap()
                .psbt
                .unsigned_tx
                .compute_txid(),
            selected_id
        );
    }
}
