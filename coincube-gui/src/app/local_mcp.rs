//! Trusted Cube settings and read-only projections. Never serializes wallet objects.
use super::{view, wallet::Wallet};
use crate::daemon::Daemon;
#[cfg(any(unix, windows))]
use coincube_mcp as wire;
#[cfg(feature = "mcp-proposals")]
use coincube_ui::component::text::{h3, text};
use coincube_ui::widget::Element;
#[cfg(feature = "mcp-proposals")]
use iced::widget::{button, checkbox, column, text_input};
use std::sync::Arc;
#[cfg(any(unix, windows))]
use std::time::Duration;
mod pairings;
mod presentation;

/// Remove only the selected Cube's pairing files, never its keystore/seed.
pub(crate) fn remove_cube_pairings(
    root: &std::path::Path,
    cube: &super::settings::CubeSettings,
) -> std::io::Result<()> {
    pairings::remove_cube(root, cube.network, &cube.id)
}

struct PairedRuntime {
    id: String,
    locator: std::path::PathBuf,
    server: wire::ipc::Server,
    #[cfg(feature = "mcp-proposals")]
    proposals: Option<Arc<wire::proposal::Store>>,
    // Last field: ownership outlives locator cleanup and server teardown.
    #[cfg(feature = "mcp-transactions")]
    transactions: Option<Arc<transactions::Service>>,
    _registry_owner: Arc<std::fs::File>,
}
impl Drop for PairedRuntime {
    fn drop(&mut self) {
        self.server.grant.revoke();
        #[cfg(feature = "mcp-transactions")]
        if let Some(service) = &self.transactions {
            service.revoke();
        }
        #[cfg(feature = "mcp-proposals")]
        if let Some(store) = &self.proposals {
            store.revoke();
        }
        let _ = std::fs::remove_file(&self.locator);
    }
}

/// Actions emitted only by the trusted desktop AI access settings.
#[derive(Debug, Clone)]
pub enum Message {
    /// Enable or disable MCP for the current unlocked session.
    Enable(bool),
    /// Select whether the current Vault may be shared.
    Wallet(bool),
    /// Edit balance, history or policy access by capability index.
    Capability(usize, bool),
    /// Set the local display label; it is not a client authentication identity.
    ClientName(String),
    /// Mint a fresh, expiring grant after validating the current selection.
    Approve,
    /// Invalidate the current grant immediately.
    Revoke,
    /// Copy the companion command and temporary credential for a trusted client.
    CopyConfig,
    /// Pair this client persistently with exactly the displayed scope.
    Pair,
    /// Separate consent for unsigned Regtest transaction preparation.
    #[cfg(feature = "mcp-transactions")]
    Transactions(bool),
    /// Native-only opening of the exact prepared unsigned PSBT.
    #[cfg(feature = "mcp-transactions")]
    ReviewTransaction {
        client_id: String,
        draft_id: String,
    },
    /// Revoke one stored client; ordinary locks never perform this action.
    RevokePair(String),
    /// Show the durable-pairing revocation decision before changing storage.
    RequestRevokePair(String),
    CancelRevokePair,
    /// Copy a stable private configuration for a stored client.
    CopyPair(String),
    /// Explicit separate consent for an empty Regtest Cube's proposal session.
    #[cfg(feature = "mcp-proposals")]
    Proposals(bool),
    /// Explicit public-key assembly consent, separate from topology-only access.
    #[cfg(feature = "mcp-proposals")]
    Creation(bool),
    /// Native-only transition into the existing key selection builder.
    #[cfg(feature = "mcp-proposals")]
    ReviewProposal,
    #[cfg(feature = "mcp-proposals")]
    ReviewPairedProposal(String),
}
/// Native consent and active endpoints. The default grants no access.
/// Durable authorization is encrypted; runtime access requires an unlocked Cube.
#[derive(Default)]
pub struct Settings {
    enabled: bool,
    wallet_allowed: bool,
    capabilities: [bool; 3],
    client_name: String,
    approved_name: String,
    failed: bool,
    #[cfg(feature = "mcp-proposals")]
    proposals_allowed: bool,
    #[cfg(feature = "mcp-proposals")]
    creation_allowed: bool,
    #[cfg(feature = "mcp-proposals")]
    proposals: Option<Arc<wire::proposal::Store>>,
    #[cfg(any(unix, windows))]
    server: Option<wire::ipc::Server>,
    #[cfg(feature = "mcp-transactions")]
    transactions_allowed: bool,
    pairing_location: Option<pairings::Location>,
    pairing_store: Option<pairings::Store>,
    paired: Vec<PairedRuntime>,
    pairing_error: Option<&'static str>,
    pairing_poisoned: bool,
    pairing_generation: Option<u64>,
    revoke_confirmation: Option<String>,
}
pub(crate) struct PairingContext<'a> {
    pub root: &'a std::path::Path,
    pub chain: crate::chain::ChainId,
    pub cube_id: &'a str,
    pub wallet: Option<Arc<Wallet>>,
    pub daemon: Option<Arc<dyn Daemon + Send + Sync>>,
    pub supported: bool,
    pub proposal_supported: bool,
    #[cfg(feature = "mcp-transactions")]
    pub last_poll_at_startup: Option<u32>,
}
impl Settings {
    #[cfg(feature = "mcp-proposals")]
    fn paired_clients_view(&self) -> Element<'_, view::Message> {
        presentation::paired_clients(self)
    }
    /// Stop the server and clear its approved display label synchronously.
    pub fn revoke(&mut self) {
        // Runtime suspension only. Explicit durable revocation is RevokePair.
        self.revoke_confirmation = None;
        self.paired.clear();
        self.revoke_legacy();
    }
    fn revoke_legacy(&mut self) {
        #[cfg(any(unix, windows))]
        {
            self.server.take();
        }
        #[cfg(feature = "mcp-proposals")]
        if let Some(store) = self.proposals.take() {
            store.revoke();
        }
        self.approved_name.clear();
    }
    fn persistence_failed(&mut self) {
        self.revoke();
        self.pairing_poisoned = true;
        // A failed registry write must not resurrect a removed authorization
        // from disk on restart. Invalidating only this registry's wrapping key
        // makes its ciphertext inert; no Cube unlock material is affected.
        self.pairing_error = Some(
            if self
                .pairing_location
                .as_ref()
                .is_some_and(|v| v.invalidate_key().is_ok())
            {
                "Pairing storage failed. All paired clients are suspended and their stored authorization is invalid. Pairing recovery is required."
            } else {
                "Could not persist revocation or invalidate its storage key. Clients are suspended now; durable revocation is NOT confirmed. Retry before closing Tenshu."
            },
        );
    }
    /// Load persistent authorization only after ordinary unlock. Every restored
    /// runtime is bound to the current generation and exact approved wallet.
    pub(crate) fn ensure_pairings(&mut self, context: PairingContext<'_>) {
        let PairingContext {
            root,
            chain,
            cube_id,
            wallet,
            daemon,
            supported,
            proposal_supported,
            #[cfg(feature = "mcp-transactions")]
            last_poll_at_startup,
        } = context;
        if super::session::generation_for(cube_id).is_none() || self.pairing_poisoned {
            return;
        }
        let Some(generation) = super::session::generation_for(cube_id) else {
            return;
        };
        if self.pairing_generation != Some(generation) {
            self.paired.clear();
            self.pairing_generation = Some(generation);
        }
        if self
            .pairing_location
            .as_ref()
            .is_some_and(|v| !v.matches(root, chain, cube_id))
        {
            self.revoke();
            self.pairing_poisoned = true;
            self.pairing_error = Some("Pairing context changed. No client access was restored.");
            return;
        }
        if self.pairing_location.is_none() {
            let Ok(location) = pairings::Location::new(root, chain, cube_id) else {
                return;
            };
            if location.exists() {
                match location.open(false) {
                    Ok(store) => {
                        self.enabled = store.enabled();
                        self.pairing_store = Some(store);
                    }
                    Err(_) => {
                        self.pairing_error = Some("Stored client pairing is unavailable or invalid. No client access was restored.");
                        self.pairing_poisoned = true;
                    }
                }
            }
            self.pairing_location = Some(location);
        }
        if self.pairing_poisoned || !self.enabled {
            return;
        }
        let Some(store) = &self.pairing_store else {
            return;
        };
        if !store.enabled() {
            return;
        }
        self.paired
            .retain(|runtime| store.clients().iter().any(|client| client.id == runtime.id));
        for client in store.clients() {
            // Keep an existing (including consumed) proposal session intact.
            if self.paired.iter().any(|runtime| runtime.id == client.id) {
                continue;
            }
            let cube = cube_id.to_owned();
            let guard = move || super::session::generation_for(&cube) == Some(generation);
            #[cfg(feature = "mcp-proposals")]
            let proposals = matches!(
                client.scope,
                pairings::Scope::Proposal | pairings::Scope::WalletCreation
            )
            .then(|| Arc::new(wire::proposal::Store::default()));
            #[cfg(feature = "mcp-transactions")]
            let mut transactions = None;
            let built: Option<(Arc<dyn wire::Reader>, wire::ipc::Grant)> = match &client.scope {
                pairings::Scope::Read {
                    wallet: expected_wallet,
                    descriptor,
                    opaque,
                    capabilities,
                } if supported => match (&wallet, &daemon) {
                    (Some(wallet), Some(daemon))
                        if wallet.id().to_string() == *expected_wallet
                            && pairings::descriptor_identity(wallet) == *descriptor
                            && wallet.chain == chain
                            && wallet.main_descriptor.as_vault().is_some()
                            && daemon.backend().is_embedded() =>
                    {
                        wire::ipc::Grant::paired(
                            opaque.clone(),
                            *capabilities,
                            client.credential.clone(),
                            guard,
                        )
                        .ok()
                        .map(|grant| {
                            let reader: Arc<dyn wire::Reader> = Arc::new(VaultReader {
                                read_slots: Arc::new(tokio::sync::Semaphore::new(2)),
                                descriptor: wallet
                                    .main_descriptor
                                    .as_vault()
                                    .expect("checked Vault")
                                    .clone(),
                                chain,
                                daemon: daemon.clone(),
                                wallet_id: opaque.clone(),
                            });
                            (reader, grant)
                        })
                    }
                    _ => None,
                },
                #[cfg(feature = "mcp-transactions")]
                pairings::Scope::TransactionPreparation {
                    wallet: expected_wallet,
                    descriptor,
                    opaque,
                } if supported && chain == crate::chain::ChainId::Regtest => {
                    match (&wallet, &daemon) {
                        (Some(wallet), Some(daemon))
                            if wallet.id().to_string() == *expected_wallet
                                && pairings::descriptor_identity(wallet) == *descriptor
                                && wallet.chain == chain
                                && wallet.main_descriptor.as_vault().is_some()
                                && daemon.backend().is_embedded() =>
                        {
                            wire::ipc::Grant::paired_transactions(
                                opaque.clone(),
                                client.credential.clone(),
                                guard,
                            )
                            .ok()
                            .map(|grant| {
                                let service = Arc::new(transactions::Service::new(
                                    wallet
                                        .main_descriptor
                                        .as_vault()
                                        .expect("checked Vault")
                                        .clone(),
                                    daemon.clone(),
                                    opaque.clone(),
                                    last_poll_at_startup,
                                ));
                                let reader: Arc<dyn wire::Reader> = service.clone();
                                transactions = Some(service);
                                (reader, grant)
                            })
                        }
                        _ => None,
                    }
                }
                pairings::Scope::Proposal if proposal_supported => {
                    #[cfg(feature = "mcp-proposals")]
                    {
                        wire::ipc::Grant::paired_proposals(client.credential.clone(), guard)
                            .ok()
                            .map(|grant| {
                                let reader: Arc<dyn wire::Reader> =
                                    proposals.as_ref().expect("proposal store").clone();
                                (reader, grant)
                            })
                    }
                    #[cfg(not(feature = "mcp-proposals"))]
                    {
                        None
                    }
                }
                pairings::Scope::WalletCreation if proposal_supported => {
                    #[cfg(feature = "mcp-proposals")]
                    {
                        wire::ipc::Grant::paired_creation(client.credential.clone(), guard)
                            .ok()
                            .map(|grant| {
                                let reader: Arc<dyn wire::Reader> =
                                    Arc::new(crate::installer::creation::CreationReader::new(
                                        proposals.as_ref().expect("creation store").clone(),
                                    ));
                                (reader, grant)
                            })
                    }
                    #[cfg(not(feature = "mcp-proposals"))]
                    {
                        None
                    }
                }
                _ => None,
            };
            let Some((reader, grant)) = built else {
                continue;
            };
            match wire::ipc::Server::start(reader, grant) {
                Ok(server) => {
                    let locator = store.location.locator(&client.id);
                    if !server.grant.active()
                        || wire::pairing::publish(&locator, &server.socket).is_err()
                    {
                        self.pairing_error = Some("Could not publish a paired client connection. No access was advertised.");
                        continue;
                    }
                    self.paired.push(PairedRuntime {
                        id: client.id.clone(),
                        locator,
                        server,
                        #[cfg(feature = "mcp-proposals")]
                        proposals,
                        #[cfg(feature = "mcp-transactions")]
                        transactions,
                        _registry_owner: store.owner(),
                    });
                }
                Err(_) => self.pairing_error = Some("Could not start a paired client connection."),
            }
        }
    }
    fn pair_client(
        &mut self,
        wallet: Option<Arc<Wallet>>,
        daemon: Option<Arc<dyn Daemon + Send + Sync>>,
        supported: bool,
        cube_id: &str,
    ) {
        let Some(generation) = super::session::generation_for(cube_id) else {
            return;
        };
        if self.pairing_poisoned
            || !supported
            || !self.enabled
            || self.client_name.trim().is_empty()
        {
            return;
        }
        let scope = match (&wallet, &daemon) {
            #[cfg(feature = "mcp-transactions")]
            (Some(wallet), Some(daemon))
                if self.transactions_allowed
                    && self.wallet_allowed
                    && !self.capabilities.iter().any(|v| *v)
                    && wallet.chain == crate::chain::ChainId::Regtest
                    && wallet.main_descriptor.as_vault().is_some()
                    && daemon.backend().is_embedded()
                    && self
                        .pairing_location
                        .as_ref()
                        .is_some_and(|v| v.chain_matches(wallet.chain)) =>
            {
                pairings::Scope::TransactionPreparation {
                    wallet: wallet.id().to_string(),
                    descriptor: pairings::descriptor_identity(wallet),
                    opaque: uuid::Uuid::new_v4().to_string(),
                }
            }
            (Some(wallet), Some(daemon))
                if self.wallet_allowed
                    && self.capabilities.iter().any(|v| *v)
                    && wallet.main_descriptor.as_vault().is_some()
                    && daemon.backend().is_embedded()
                    && self
                        .pairing_location
                        .as_ref()
                        .is_some_and(|v| v.chain_matches(wallet.chain)) =>
            {
                pairings::Scope::Read {
                    wallet: wallet.id().to_string(),
                    descriptor: pairings::descriptor_identity(wallet),
                    opaque: uuid::Uuid::new_v4().to_string(),
                    capabilities: self.capabilities,
                }
            }
            #[cfg(feature = "mcp-proposals")]
            (None, None)
                if (self.proposals_allowed || self.creation_allowed)
                    && self
                        .pairing_location
                        .as_ref()
                        .is_some_and(|v| v.chain_matches(crate::chain::ChainId::Regtest)) =>
            {
                if self.creation_allowed {
                    pairings::Scope::WalletCreation
                } else {
                    pairings::Scope::Proposal
                }
            }
            _ => return,
        };
        if self.pairing_store.is_none() {
            let Some(location) = &self.pairing_location else {
                return;
            };
            match location.open(true) {
                Ok(store) => self.pairing_store = Some(store),
                Err(_) => {
                    self.pairing_error =
                        Some("Could not protect the client pairing. No client was paired.");
                    return;
                }
            }
        }
        let client_name = self.client_name.trim().to_owned();
        let store = self
            .pairing_store
            .as_mut()
            .expect("loaded pairing registry");
        if store.clients().len() >= pairings::MAX_CLIENTS {
            self.pairing_error = Some(
                "The 16-client limit was reached. Revoke an unused client before pairing another.",
            );
            return;
        }
        let result = super::session::admit_generation(cube_id, generation, || {
            Some(store.add(client_name, scope))
        });
        self.finish_pairing(result);
    }
    fn finish_pairing(&mut self, result: Option<Result<String, pairings::AddError>>) {
        match result {
            None => {
                self.pairing_error =
                    Some("Cube locked or changed before pairing was saved. No client was paired.")
            }
            Some(Err(pairings::AddError::Unchanged)) => {
                self.pairing_error = Some(
                    "Could not save the new client. Existing pairings are unchanged; retry pairing.",
                );
            }
            Some(Err(pairings::AddError::Uncertain)) => self.persistence_failed(),
            Some(Ok(_)) => {
                self.pairing_error = None;
                self.client_name.clear();
                // Every new client must receive its own explicit wallet and
                // capability choices; the previous grant is not a template.
                self.wallet_allowed = false;
                self.capabilities = [false; 3];
            }
        }
    }
    /// Apply a trusted UI action, returning private client JSON only for CopyConfig.
    /// Selection edits revoke old credentials; approval requires an unlocked Vault
    /// on the embedded backend and captures its current session generation.
    pub fn update(
        &mut self,
        message: Message,
        wallet: Option<Arc<Wallet>>,
        daemon: Option<Arc<dyn Daemon + Send + Sync>>,
        supported: bool,
        cube_id: &str,
    ) -> Option<String> {
        match message {
            #[cfg(feature = "mcp-transactions")]
            Message::ReviewTransaction { .. } => return None,
            Message::Enable(value) => {
                self.revoke();
                self.wallet_allowed = false;
                self.capabilities = [false; 3];
                self.client_name.clear();
                self.enabled = value && cfg!(any(unix, windows));
                if let Some(store) = &mut self.pairing_store {
                    if store.set_enabled(self.enabled).is_err() {
                        self.persistence_failed();
                    }
                }
            }
            Message::Wallet(value) => {
                self.revoke_legacy();
                self.wallet_allowed = value;
            }
            Message::Capability(index, value) if index < 3 => {
                self.revoke_legacy();
                self.capabilities[index] = value;
                #[cfg(feature = "mcp-transactions")]
                if value {
                    self.transactions_allowed = false;
                }
            }
            Message::ClientName(value) => {
                self.revoke_legacy();
                self.client_name = value.chars().filter(|c| !c.is_control()).take(64).collect();
            }
            #[cfg(feature = "mcp-proposals")]
            Message::Proposals(value) => {
                self.revoke_legacy();
                self.proposals_allowed = value;
                if value {
                    self.creation_allowed = false;
                }
            }
            #[cfg(feature = "mcp-proposals")]
            Message::Creation(value) => {
                self.revoke_legacy();
                self.creation_allowed = value;
                if value {
                    self.proposals_allowed = false;
                }
            }
            #[cfg(feature = "mcp-transactions")]
            Message::Transactions(value) => {
                self.revoke_legacy();
                self.transactions_allowed = value;
                if value {
                    self.capabilities = [false; 3];
                }
            }
            Message::Revoke => self.revoke(),
            Message::Pair => {
                self.pair_client(wallet, daemon, supported, cube_id);
                return None;
            }
            Message::RevokePair(id) => {
                if self.revoke_confirmation.as_deref() != Some(id.as_str()) {
                    return None;
                }
                self.revoke_confirmation = None;
                self.paired.retain(|runtime| runtime.id != id);
                if let Some(store) = &mut self.pairing_store {
                    if store.remove(&id).is_err() {
                        self.persistence_failed();
                    }
                }
                return None;
            }
            Message::RequestRevokePair(id) => {
                self.revoke_confirmation = self
                    .pairing_store
                    .as_ref()
                    .and_then(|store| store.clients().iter().find(|client| client.id == id))
                    .map(|_| id);
                return None;
            }
            Message::CancelRevokePair => {
                self.revoke_confirmation = None;
                return None;
            }
            Message::CopyPair(id) => {
                if self.pairing_poisoned {
                    return None;
                }
                // The private bearer configuration is only revealed while its
                // exact runtime grant is active in this unlocked session.
                if !self
                    .paired
                    .iter()
                    .any(|runtime| runtime.id == id && runtime.server.grant.active())
                {
                    return None;
                }
                let store = self.pairing_store.as_ref()?;
                let client = store.clients().iter().find(|client| client.id == id)?;
                let executable = std::env::current_exe()
                    .ok()?
                    .parent()?
                    .join(format!("tenshu-mcp{}", std::env::consts::EXE_SUFFIX));
                return serde_json::to_string_pretty(&serde_json::json!({"mcpServers": {"tenshu": {
                    "command": executable,
                    "env": {"TENSHU_MCP_PAIRING": store.location.locator(&id), "TENSHU_MCP_CREDENTIAL": client.credential}
                }}})).ok();
            }
            Message::Approve => {
                self.revoke_legacy();
                self.failed = false;
                #[cfg(feature = "mcp-proposals")]
                if supported
                    && wallet.is_none()
                    && daemon.is_none()
                    && self.enabled
                    && self.proposals_allowed
                    && !self.client_name.trim().is_empty()
                {
                    let generation = super::session::generation_for(cube_id)?;
                    let cube_id = cube_id.to_owned();
                    let store = Arc::new(wire::proposal::Store::default());
                    let grant =
                        wire::ipc::Grant::for_proposals(Duration::from_secs(3600), move || {
                            super::session::generation_for(&cube_id) == Some(generation)
                        });
                    match wire::ipc::Server::start(store.clone(), grant) {
                        Ok(server) => {
                            self.server = Some(server);
                            self.proposals = Some(store);
                            self.approved_name = self.client_name.clone();
                        }
                        Err(_) => self.failed = true,
                    }
                    return None;
                }
                if !supported
                    || !self.enabled
                    || !self.wallet_allowed
                    || self.client_name.trim().is_empty()
                    || !self.capabilities.iter().any(|v| *v)
                {
                    return None;
                }
                #[cfg(any(unix, windows))]
                if let (Some(wallet), Some(daemon)) = (wallet, daemon) {
                    if wallet.main_descriptor.as_vault().is_none()
                        || !daemon.backend().is_embedded()
                    {
                        return None;
                    }
                    let generation = super::session::generation_for(cube_id)?;
                    let cube_id = cube_id.to_owned();
                    let wallet_id = uuid::Uuid::new_v4().to_string();
                    let grant = wire::ipc::Grant::new(
                        wallet_id.clone(),
                        self.capabilities,
                        Duration::from_secs(3600),
                    )
                    .with_session_guard(move || {
                        super::session::generation_for(&cube_id) == Some(generation)
                    });
                    let reader = Arc::new(VaultReader {
                        read_slots: Arc::new(tokio::sync::Semaphore::new(2)),
                        descriptor: wallet.main_descriptor.as_vault()?.clone(),
                        chain: wallet.chain,
                        daemon,
                        wallet_id,
                    });
                    match wire::ipc::Server::start(reader, grant) {
                        Ok(server) => {
                            self.server = Some(server);
                            self.approved_name = self.client_name.clone();
                        }
                        Err(_) => self.failed = true,
                    }
                }
                #[cfg(not(any(unix, windows)))]
                let _ = (wallet, daemon, cube_id);
            }
            Message::CopyConfig => {
                #[cfg(any(unix, windows))]
                if let Some(server) = self.server.as_ref().filter(|s| s.grant.active()) {
                    let executable = std::env::current_exe()
                        .ok()?
                        .parent()?
                        .join(format!("tenshu-mcp{}", std::env::consts::EXE_SUFFIX));
                    return serde_json::to_string_pretty(&serde_json::json!({"mcpServers": {"tenshu": {
                        "command": executable,
                        "env": {"TENSHU_MCP_SOCKET": server.socket, "TENSHU_MCP_CREDENTIAL": server.grant.credential()}
                    }}})).ok();
                }
            }
            _ => {}
        }
        None
    }
    /// Native handoff consumes Settings ownership, preserving only this session.
    #[cfg(feature = "mcp-proposals")]
    pub(crate) fn proposal_session(
        &self,
        cube_id: &str,
    ) -> Option<crate::installer::proposal::Session> {
        let server = self.server.as_ref().filter(|s| s.grant.active())?;
        let store = self.proposals.as_ref()?;
        let proposal = store.pending().ok()??;
        Some(crate::installer::proposal::Session::new(
            cube_id.to_owned(),
            super::session::generation_for(cube_id)?,
            server.grant.clone(),
            store.clone(),
            proposal,
        ))
    }

    #[cfg(feature = "mcp-proposals")]
    pub(crate) fn paired_proposal_session(
        &self,
        cube_id: &str,
        id: &str,
    ) -> Option<crate::installer::proposal::Session> {
        let runtime = self
            .paired
            .iter()
            .find(|v| v.id == id && v.server.grant.active())?;
        let store = runtime.proposals.as_ref()?;
        let proposal = store.pending().ok()??;
        Some(crate::installer::proposal::Session::new(
            cube_id.to_owned(),
            super::session::generation_for(cube_id)?,
            runtime.server.grant.clone(),
            store.clone(),
            proposal,
        ))
    }

    #[cfg(feature = "mcp-transactions")]
    pub(crate) fn prepared_transaction(
        &self,
        client_id: &str,
        draft_id: &str,
        wallet: &Wallet,
    ) -> Option<transactions::NativeDraft> {
        if wallet.chain != crate::chain::ChainId::Regtest {
            return None;
        }
        let client = self
            .pairing_store
            .as_ref()?
            .clients()
            .iter()
            .find(|c| c.id == client_id)?;
        match &client.scope {
            pairings::Scope::TransactionPreparation {
                wallet: expected,
                descriptor,
                ..
            } if *expected == wallet.id().to_string()
                && *descriptor == pairings::descriptor_identity(wallet) => {}
            _ => return None,
        }
        let runtime = self
            .paired
            .iter()
            .find(|v| v.id == client_id && v.server.grant.active())?;
        let native = runtime.transactions.as_ref()?.native(draft_id)?;
        runtime.server.grant.active().then_some(native)
    }

    fn transaction_selection(&self) -> bool {
        #[cfg(feature = "mcp-transactions")]
        {
            self.transactions_allowed
                && self
                    .pairing_location
                    .as_ref()
                    .is_some_and(|v| v.chain_matches(crate::chain::ChainId::Regtest))
        }
        #[cfg(not(feature = "mcp-transactions"))]
        false
    }
    /// Render consent controls and warn that the configured AI provider may receive data.
    pub fn view_for_cube(
        &self,
        supported: bool,
        proposal_supported: bool,
    ) -> Element<'_, view::Message> {
        #[cfg(feature = "mcp-proposals")]
        if proposal_supported {
            return self.proposal_view();
        }
        #[cfg(not(feature = "mcp-proposals"))]
        let _ = proposal_supported;
        presentation::view_for_cube(self, supported)
    }
}

#[cfg(feature = "mcp-proposals")]
impl Settings {
    fn proposal_view(&self) -> Element<'_, view::Message> {
        let msg = |m| view::Message::LocalMcp(m);
        let mut content = column![h3("AI wallet preparation — Regtest experiment"),
            text("Choose topology-only access, or allow the assistant to assemble public account keys for a 3-of-5 wallet with explicitly chosen recovery. Your AI provider may receive the public keys you give it. Neither permission allows secrets, signing, sending coins or creation approval."),
            text("Tenshu validates the descriptor and fills its builder. You verify keys on the source devices, handle hardware, seed backups and PIN/passkey steps, and authorize creation here. Pairing survives lock and restart; each draft ends on lock/close or creation admission. Wallet reads require their own permission."),
            checkbox(self.enabled).label("Enable AI access for this Cube").on_toggle(move |v| msg(Message::Enable(v)))].spacing(16);
        if self.enabled {
            content = content
                .push(
                    text_input("Client label (for your reference)", &self.client_name)
                        .on_input(move |v| msg(Message::ClientName(v))),
                )
                .push(
                    checkbox(self.proposals_allowed)
                        .label("Allow this client to prepare a Regtest wallet proposal")
                        .on_toggle(move |v| msg(Message::Proposals(v))),
                )
                .push(
                    checkbox(self.creation_allowed)
                        .label("Allow this client to prepare a Regtest wallet using public account keys")
                        .on_toggle(move |v| msg(Message::Creation(v))),
                )
                .push(
                    button("Pair setup client until revoked").on_press_maybe(
                        ((self.proposals_allowed || self.creation_allowed) && !self.client_name.trim().is_empty())
                            .then(|| msg(Message::Pair)),
                    ),
                );
            if let Some(server) = &self.server {
                content = content
                    .push(text(if server.grant.active() {
                        "Proposal client authorized"
                    } else {
                        "Inactive — approve again"
                    }))
                    .push(button("Revoke access now").on_press(msg(Message::Revoke)));
                if server.grant.active() {
                    content = content.push(button("Copy private client configuration").on_press(msg(Message::CopyConfig)))
                        .push(text("Save the temporary grant only in your client's private local configuration. Never paste it into chat."));
                    if let Some(proposal) = self
                        .proposals
                        .as_ref()
                        .and_then(|s| s.pending().ok())
                        .flatten()
                    {
                        content = content.push(text(format!("3-of-5 primary; recovery {}-of-{} after {} blocks. Actual keys and descriptor are not selected yet.", proposal.topology.recovery.quorum.threshold, proposal.topology.recovery.quorum.key_count, proposal.topology.recovery.relative_delay_blocks)))
                            .push(button("Review proposal and select keys in Tenshu").on_press(msg(Message::ReviewProposal)));
                    } else {
                        content = content.push(text("Waiting for a proposal from your client."));
                    }
                }
            } else if self
                .pairing_store
                .as_ref()
                .is_none_or(|s| s.clients().is_empty())
            {
                content = content.push(text("No proposal client is authorized."));
            }
        }
        if self.failed {
            content = content.push(text("Could not start local MCP. No access was granted."));
        }
        content.push(self.paired_clients_view()).into()
    }
}

#[cfg(any(unix, windows))]
/// Read projection retaining a descriptor and daemon, never a wallet signer.
#[derive(Clone)]
struct VaultReader {
    read_slots: Arc<tokio::sync::Semaphore>,
    descriptor: coincube_core::descriptors::CoincubeDescriptor,
    chain: crate::chain::ChainId,
    daemon: Arc<dyn Daemon + Send + Sync>,
    wallet_id: String,
}
#[cfg(any(unix, windows))]
impl wire::Reader for VaultReader {
    /// Bound blocking database work to two workers independently of IPC deadlines.
    fn read(
        &self,
        request: wire::Request,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<wire::Response, wire::Error>> + Send + '_>,
    > {
        Box::pin(async move {
            // Existing daemon reads may block on SQLite. Keep them off the async
            // executor so deadlines and revocation remain responsive. The permit
            // belongs to the blocking worker: timing out must not admit unlimited
            // replacement reads while old database calls are still running.
            let permit = self
                .read_slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| wire::Error::Unavailable)?;
            let reader = self.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                tokio::runtime::Handle::current().block_on(reader.read_wallet(request))
            })
            .await
            .map_err(|_| wire::Error::Unavailable)?
        })
    }
}
#[cfg(any(unix, windows))]
impl VaultReader {
    /// Validate the opaque wallet scope and daemon identity before projecting public data.
    async fn read_wallet(&self, request: wire::Request) -> Result<wire::Response, wire::Error> {
        if matches!(
            &request,
            wire::Request::PrepareProposal(_)
                | wire::Request::PrepareCreation(_)
                | wire::Request::Proposal(_)
                | wire::Request::PrepareTransaction(_)
                | wire::Request::Transaction(_)
        ) {
            return Err(wire::Error::AccessDenied);
        }
        // Defense in depth: the IPC gate enforces the same opaque identity.
        if request.scope().is_some_and(|(id, _)| id != self.wallet_id) {
            return Err(wire::Error::AccessDenied);
        }
        if !request.valid() {
            return Err(wire::Error::InvalidInput);
        }
        let descriptor = &self.descriptor;
        let info = self
            .daemon
            .get_info()
            .await
            .map_err(|_| wire::Error::Unavailable)?;
        if info.descriptors.main.as_vault() != Some(descriptor) || info.chain_id != Some(self.chain)
        {
            return Err(wire::Error::Unavailable);
        }
        match request {
            wire::Request::PrepareProposal(_)
            | wire::Request::PrepareCreation(_)
            | wire::Request::Proposal(_)
            | wire::Request::PrepareTransaction(_)
            | wire::Request::Transaction(_) => Err(wire::Error::AccessDenied),
            wire::Request::List(_) => Ok(wire::Response::List(wire::WalletList {
                wallets: vec![wire::WalletMetadata {
                    wallet_id: self.wallet_id.clone(),
                    display_name: "Vault".into(),
                    wallet_type: "vault".into(),
                }],
            })),
            wire::Request::Summary(_) => {
                let coins = self
                    .daemon
                    .list_coins(&[], &[])
                    .await
                    .map_err(|_| wire::Error::Unavailable)?
                    .coins;
                let sync = super::wallet::sync_status(
                    self.daemon.backend(),
                    info.block_height,
                    info.sync,
                    info.last_poll_timestamp,
                    None,
                    &info.history_sync,
                );
                let sync_state = match sync {
                    super::wallet::SyncStatus::Synced => "synced",
                    super::wallet::SyncStatus::SyncFailing { .. } => "sync_failing",
                    _ => "syncing",
                }
                .to_owned();
                let unspent = || coins.iter().filter(|c| c.spend_info.is_none());
                Ok(wire::Response::Summary(wire::Summary {
                    wallet_id: self.wallet_id.clone(),
                    network: info.network.to_string(),
                    wallet_type: "vault".into(),
                    confirmed_sats: unspent()
                        .filter(|c| c.block_height.is_some())
                        .map(|c| c.amount.to_sat())
                        .sum(),
                    unconfirmed_sats: unspent()
                        .filter(|c| c.block_height.is_none())
                        .map(|c| c.amount.to_sat())
                        .sum(),
                    available_sats: unspent()
                        .filter(|c| !c.is_immature && crate::daemon::model::coin_is_owned(c))
                        .map(|c| c.amount.to_sat())
                        .sum(),
                    sync_state,
                }))
            }
            wire::Request::History(input) => {
                let transactions = self
                    .daemon
                    .list_history_txs(0, u32::MAX, u64::from(input.limit))
                    .await
                    .map_err(|_| wire::Error::Unavailable)?;
                Ok(wire::Response::History(wire::History {
                    wallet_id: self.wallet_id.clone(),
                    confirmed_only: true,
                    transactions: transactions
                        .into_iter()
                        .take(usize::from(input.limit))
                        .map(project_transaction)
                        .collect(),
                }))
            }
            wire::Request::Policy(_) => {
                let policy = descriptor.policy();
                let path = |p: &coincube_core::descriptors::PathInfo, delay| {
                    let (threshold, key_count) = match p {
                        coincube_core::descriptors::PathInfo::Single(_) => (1, 1),
                        coincube_core::descriptors::PathInfo::Multi(k, keys) => (*k, keys.len()),
                    };
                    wire::PolicyPath {
                        threshold,
                        key_count,
                        relative_delay_blocks: delay,
                    }
                };
                Ok(wire::Response::Policy(wire::Policy {
                    wallet_id: self.wallet_id.clone(),
                    deterministic: true,
                    script_type: if descriptor.is_taproot() {
                        "p2tr"
                    } else {
                        "p2wsh"
                    }
                    .into(),
                    primary: path(policy.primary_path(), 0),
                    recovery: policy
                        .recovery_paths()
                        .iter()
                        .map(|(delay, p)| path(p, *delay))
                        .collect(),
                }))
            }
        }
    }
}

#[cfg(any(unix, windows))]
fn project_transaction(tx: crate::daemon::model::HistoryTransaction) -> wire::Transaction {
    wire::Transaction {
        txid: tx.txid.to_string(),
        incoming_sats: tx.incoming_amount.to_sat(),
        // The model totals every non-wallet output even for a deposit. In that
        // case those outputs belong to the sender, not this Vault's spending.
        outgoing_sats: if tx.is_external() {
            0
        } else {
            tx.outgoing_amount.to_sat()
        },
        timestamp: tx.time,
        height: tx.height,
    }
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use coincube_core::{
        descriptors::{CoincubeDescriptor, CoincubePolicy, PathInfo},
        miniscript::descriptor::DescriptorPublicKey,
    };
    use std::str::FromStr;

    #[test]
    fn history_projection_attributes_only_wallet_spending() {
        use crate::daemon::model::{Coin, HistoryTransaction};
        use coincube_core::miniscript::bitcoin::{
            absolute, bip32, hashes::Hash, transaction, Address, Amount, Network, OutPoint,
            ScriptBuf, Transaction, TxIn, TxOut, WScriptHash,
        };
        let script = ScriptBuf::new_p2wsh(&WScriptHash::all_zeros());
        let previous_output = OutPoint {
            txid: coincube_core::miniscript::bitcoin::Txid::all_zeros(),
            vout: 1,
        };
        let coin = Coin {
            amount: Amount::from_sat(5_000_000_000),
            outpoint: previous_output,
            address: Address::from_script(&script, Network::Regtest).unwrap(),
            block_height: Some(1),
            derivation_index: bip32::ChildNumber::from(0),
            spend_info: None,
            is_immature: false,
            is_change: false,
            is_from_self: false,
        };
        // The first output belongs to the Vault; the second is sender change
        // for incoming payments and the recipient payment for outgoing ones.
        let transaction = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output,
                ..Default::default()
            }],
            output: vec![
                TxOut {
                    value: Amount::from_sat(1_000_000),
                    script_pubkey: script.clone(),
                },
                TxOut {
                    value: Amount::from_sat(4_998_999_844),
                    script_pubkey: ScriptBuf::new_p2wsh(&WScriptHash::from_byte_array([1; 32])),
                },
            ],
        };
        let mut batch = transaction.clone();
        batch.output.push(TxOut {
            value: Amount::from_sat(100_000),
            script_pubkey: batch.output[0].script_pubkey.clone(),
        });
        let batch_history = HistoryTransaction::new(
            batch,
            Some(103),
            Some(1234),
            vec![],
            vec![0, 2],
            Network::Regtest,
        );
        let projected_batch = project_transaction(batch_history);
        assert_eq!(projected_batch.incoming_sats, 1_100_000);
        assert_eq!(projected_batch.outgoing_sats, 0);
        for (wallet_inputs, wallet_outputs, expected_incoming, expected_outgoing) in [
            (vec![], vec![0], 1_000_000, 0),
            (vec![], vec![0, 1], 4_999_999_844, 0),
            (vec![coin.clone()], vec![0], 1_000_000, 4_998_999_844),
            (vec![coin], vec![0, 1], 4_999_999_844, 0),
        ] {
            let history = HistoryTransaction::new(
                transaction.clone(),
                Some(103),
                Some(1234),
                wallet_inputs,
                wallet_outputs,
                Network::Regtest,
            );
            let projected = project_transaction(history);
            assert_eq!(projected.txid, transaction.compute_txid().to_string());
            assert_eq!(projected.incoming_sats, expected_incoming);
            assert_eq!(projected.outgoing_sats, expected_outgoing);
            assert_eq!(
                (projected.height, projected.timestamp),
                (Some(103), Some(1234))
            );
        }
    }

    /// Create a disposable real SQLite wallet and matching GUI wallet wrapper.
    fn fixture() -> (
        std::path::PathBuf,
        Arc<Wallet>,
        Arc<dyn Daemon + Send + Sync>,
    ) {
        // Existing public-only test keys. No seed, xprv, production wallet or signing.
        let owner = DescriptorPublicKey::from_str("[aabbccdd]xpub68JJTXc1MWK8KLW4HGLXZBJknja7kDUJuFHnM424LbziEXsfkh1WQCiEjjHw4zLqSUm4rvhgyGkkuRowE9tCJSgt3TQB5J3SKAbZ2SdcKST/<0;1>/*").unwrap();
        let recovery = DescriptorPublicKey::from_str("[aabbccdd]xpub68JJTXc1MWK8PEQozKsRatrUHXKFNkD1Cb1BuQU9Xr5moCv87anqGyXLyUd4KpnDyZgo3gz4aN1r3NiaoweFW8UutBsBbgKHzaD5HkTkifK/<0;1>/*").unwrap();
        let descriptor = CoincubeDescriptor::new(
            CoincubePolicy::new_legacy(
                PathInfo::Single(owner),
                [(144, PathInfo::Single(recovery))]
                    .iter()
                    .cloned()
                    .collect(),
            )
            .unwrap(),
        );
        let root = std::env::temp_dir().join(format!("mcp-wallet-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let (config, handle) = coincubed::mcp_test_fixture::wallet(&root, descriptor.clone());
        let daemon =
            Arc::new(crate::daemon::embedded::EmbeddedDaemon::from_regtest_handle(config, handle));
        (root, Arc::new(Wallet::new(descriptor)), daemon)
    }
    /// Build a typed request using an opaque grant-scoped wallet identifier.
    fn input(id: &str) -> wire::WalletInput {
        wire::WalletInput {
            wallet_id: id.into(),
        }
    }

    #[test]
    fn successful_pair_requires_fresh_scope_for_next_client() {
        let mut settings = Settings {
            enabled: true,
            wallet_allowed: true,
            capabilities: [true, false, true],
            client_name: "first client".into(),
            ..Settings::default()
        };
        settings.finish_pairing(Some(Ok("synthetic-id".into())));
        assert!(settings.client_name.is_empty());
        assert!(!settings.wallet_allowed);
        assert_eq!(settings.capabilities, [false; 3]);

        settings.client_name = "second client".into();
        assert!(!settings.wallet_allowed && !settings.capabilities.iter().any(|v| *v));
    }

    #[test]
    #[ignore = "synthetic content-only capture; set R06A_CAPTURE_DIR for PNGs"]
    fn ai_access_synthetic_content_states_fit_and_disclose_scope() {
        use crate::app::view::shared::landing::capture;
        use coincube_ui::theme;
        let _guard = super::super::session::test_guard();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        let (root, wallet, daemon) = fixture();
        let cube = "mcp-visual-synthetic";
        let chain = wallet.chain;
        std::fs::create_dir_all(root.join(chain.dir_name())).unwrap();
        let location = pairings::Location::new(&root, chain, cube).unwrap();
        super::super::session::open_without_pin(cube);

        let empty = Settings::default();
        let mut paired = Settings {
            enabled: true,
            pairing_location: Some(location.clone()),
            pairing_store: Some(
                pairings::Store::open(location, zeroize::Zeroizing::new([51; 32]), true).unwrap(),
            ),
            ..Settings::default()
        };
        for message in [
            Message::ClientName("Synthetic local client".into()),
            Message::Wallet(true),
            Message::Capability(0, true),
            Message::Pair,
        ] {
            paired.update(
                message,
                Some(wallet.clone()),
                Some(daemon.clone()),
                true,
                cube,
            );
        }
        paired.ensure_pairings(PairingContext {
            root: &root,
            chain,
            cube_id: cube,
            wallet: Some(wallet.clone()),
            daemon: Some(daemon.clone()),
            supported: true,
            proposal_supported: false,
        });
        assert_eq!(paired.paired.len(), 1);
        let render = |scenario: &str, settings: &Settings| {
            for (size, appearance, palette) in [
                ((1024, 768), "dark", theme::Theme::dark()),
                ((1024, 768), "light", theme::Theme::light()),
                ((1440, 900), "dark", theme::Theme::dark()),
                ((1440, 900), "light", theme::Theme::light()),
            ] {
                let inspection = capture::inspect(
                    scenario,
                    settings.view_for_cube(true, false),
                    size,
                    appearance,
                    palette,
                );
                assert!(inspection.contains("Today · Persistent per-client read access"));
                assert!(inspection.contains("Your AI provider may receive financial information"));
                assert!(inspection.contains("Guided pairing · Planned"));
                assert!(!inspection.contains("TENSHU_MCP_CREDENTIAL"));
            }
        };
        render("r23-empty", &empty);
        let empty_focus = capture::inspect(
            "r23-empty-focus",
            empty.view_for_cube(true, false),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(empty_focus
            .focus
            .iter()
            .any(|(id, _)| { id == &iced::advanced::widget::Id::from("tenshu.ai-access.enable") }));
        for key in [
            iced::keyboard::key::Named::Enter,
            iced::keyboard::key::Named::Space,
        ] {
            let emitted = capture::activate_with_key(
                empty.view_for_cube(true, false),
                iced::advanced::widget::Id::from("tenshu.ai-access.enable"),
                (1024, 768),
                false,
                key,
            );
            assert!(emitted
                .iter()
                .any(|intent| matches!(intent, view::Message::LocalMcp(Message::Enable(true)))));
        }
        render("r23-paired", &paired);
        let paired_focus = capture::inspect(
            "r23-paired-focus",
            paired.view_for_cube(true, false),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        for id in [
            "tenshu.ai-access.enable",
            "tenshu.ai-access.wallet",
            "tenshu.ai-access.permission.balances",
            "tenshu.ai-access.permission.history",
            "tenshu.ai-access.permission.policy",
        ] {
            assert!(paired_focus
                .focus
                .iter()
                .any(|(focus_id, _)| focus_id == &iced::advanced::widget::Id::from(id)));
        }
        let client_id = paired.pairing_store.as_ref().unwrap().clients()[0]
            .id
            .clone();
        let copy_id =
            iced::advanced::widget::Id::from(format!("tenshu.ai-access.client.{client_id}.copy"));
        let revoke_id =
            iced::advanced::widget::Id::from(format!("tenshu.ai-access.client.{client_id}.revoke"));
        for key in [
            iced::keyboard::key::Named::Enter,
            iced::keyboard::key::Named::Space,
        ] {
            let emitted = capture::activate_with_key(
                paired.view_for_cube(true, false),
                copy_id.clone(),
                (1024, 768),
                false,
                key,
            );
            assert!(emitted.iter().any(|intent| matches!(intent,
                view::Message::LocalMcp(Message::CopyPair(id)) if id == &client_id
            )));
        }
        let emitted = capture::activate(
            paired.view_for_cube(true, false),
            revoke_id.clone(),
            (1024, 768),
            true,
        );
        assert!(emitted.iter().any(|intent| matches!(intent,
            view::Message::LocalMcp(Message::RequestRevokePair(id)) if id == &client_id
        )));
        for (id, expected) in [
            ("tenshu.ai-access.enable", Message::Enable(false)),
            ("tenshu.ai-access.wallet", Message::Wallet(true)),
            (
                "tenshu.ai-access.permission.balances",
                Message::Capability(0, true),
            ),
            (
                "tenshu.ai-access.permission.history",
                Message::Capability(1, true),
            ),
            (
                "tenshu.ai-access.permission.policy",
                Message::Capability(2, true),
            ),
        ] {
            for key in [
                iced::keyboard::key::Named::Enter,
                iced::keyboard::key::Named::Space,
            ] {
                let emitted = capture::activate_with_key(
                    paired.view_for_cube(true, false),
                    iced::advanced::widget::Id::from(id),
                    (1024, 768),
                    false,
                    key,
                );
                assert!(emitted.iter().any(|intent| matches!(intent,
                    view::Message::LocalMcp(actual) if match (actual, &expected) {
                        (Message::Enable(actual), Message::Enable(expected)) => actual == expected,
                        (Message::Wallet(actual), Message::Wallet(expected)) => actual == expected,
                        (Message::Capability(actual_index, actual_value), Message::Capability(expected_index, expected_value)) => actual_index == expected_index && actual_value == expected_value,
                        _ => false,
                    }
                )), "{id} should respond to {key:?}");
            }
        }
        paired.wallet_allowed = true;
        paired.capabilities[0] = true;
        paired.client_name = "Next synthetic client".into();
        for key in [
            iced::keyboard::key::Named::Enter,
            iced::keyboard::key::Named::Space,
        ] {
            let emitted = capture::activate_with_key(
                paired.view_for_cube(true, false),
                iced::advanced::widget::Id::from("tenshu.ai-access.pair"),
                (1024, 768),
                false,
                key,
            );
            assert!(emitted
                .iter()
                .any(|intent| matches!(intent, view::Message::LocalMcp(Message::Pair))));
        }
        let original_count = paired.pairing_store.as_ref().unwrap().clients().len();
        paired.update(
            Message::RequestRevokePair(client_id.clone()),
            None,
            None,
            true,
            cube,
        );
        assert_eq!(
            paired.pairing_store.as_ref().unwrap().clients().len(),
            original_count
        );
        for (size, appearance, palette) in [
            ((1024, 768), "dark", theme::Theme::dark()),
            ((1024, 768), "light", theme::Theme::light()),
            ((1440, 900), "dark", theme::Theme::dark()),
            ((1440, 900), "light", theme::Theme::light()),
        ] {
            let inspection = capture::inspect(
                "r23-revoke-confirm",
                presentation::revoke_confirmation("Synthetic local client", &client_id),
                size,
                appearance,
                palette,
            );
            assert!(inspection.contains("Revoke this client's access?"));
            assert!(inspection.contains("Information already read may remain"));
        }
        for (id, expected) in [
            ("tenshu.ai-access.revoke.keep", false),
            ("tenshu.ai-access.revoke.confirm", true),
        ] {
            for key in [
                iced::keyboard::key::Named::Enter,
                iced::keyboard::key::Named::Space,
            ] {
                let emitted = capture::activate_with_key(
                    presentation::revoke_confirmation("Synthetic local client", &client_id),
                    iced::advanced::widget::Id::from(id),
                    (1024, 768),
                    false,
                    key,
                );
                assert!(
                    emitted.iter().any(|intent| matches!(intent,
                        view::Message::LocalMcp(Message::RevokePair(_)) if expected
                    ) || matches!(intent,
                        view::Message::LocalMcp(Message::CancelRevokePair) if !expected
                    )),
                    "{id} should respond to {key:?}"
                );
            }
        }
        paired.update(Message::CancelRevokePair, None, None, true, cube);
        assert!(paired.revoke_confirmation.is_none());
        assert_eq!(
            paired.pairing_store.as_ref().unwrap().clients().len(),
            original_count
        );
        paired.update(
            Message::RevokePair(client_id.clone()),
            None,
            None,
            true,
            cube,
        );
        assert_eq!(
            paired.pairing_store.as_ref().unwrap().clients().len(),
            original_count
        );
        paired.revoke();
        super::super::session::close();
        render("r23-locked", &paired);
        let locked = capture::inspect(
            "r23-locked-focus",
            paired.view_for_cube(true, false),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(!locked.focus.iter().any(|(id, _)| id == &copy_id));
        assert!(locked.focus.iter().any(|(id, _)| id == &revoke_id));
        paired.update(
            Message::RequestRevokePair(client_id.clone()),
            None,
            None,
            true,
            cube,
        );
        assert_eq!(
            paired.revoke_confirmation.as_deref(),
            Some(client_id.as_str())
        );
        paired.update(Message::RevokePair(client_id), None, None, true, cube);
        assert!(paired.pairing_store.as_ref().unwrap().clients().is_empty());
        drop(paired);
        runtime.block_on(daemon.stop()).unwrap();
        drop(daemon);
        drop(wallet);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persistent_pairings_resume_after_lock_restart_and_revoke_independently() {
        use serde_json::Value;
        let _guard = super::super::session::test_guard();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (root, wallet, daemon) = fixture();
            let chain = wallet.chain;
            std::fs::create_dir_all(root.join(chain.dir_name())).unwrap();
            let location = pairings::Location::new(&root, chain, "mcp-paired-fixture").unwrap();
            // Synthetic native consent / test wrapping key. Actual packaged UI
            // and OS-keystore acceptance are separate signed-artifact checks.
            let attach = |create| Settings {
                enabled: true,
                pairing_location: Some(location.clone()),
                pairing_store: Some(
                    pairings::Store::open(
                        location.clone(),
                        zeroize::Zeroizing::new([23; 32]),
                        create,
                    )
                    .unwrap(),
                ),
                ..Settings::default()
            };
            let refresh = |settings: &mut Settings| {
                settings.ensure_pairings(PairingContext {
                    root: &root,
                    chain,
                    cube_id: "mcp-paired-fixture",
                    wallet: Some(wallet.clone()),
                    daemon: Some(daemon.clone()),
                    supported: true,
                    proposal_supported: false,
                    #[cfg(feature = "mcp-transactions")]
                    last_poll_at_startup: None,
                })
            };
            let configuration = |settings: &mut Settings, id: &str| -> Value {
                serde_json::from_str(
                    &settings
                        .update(
                            Message::CopyPair(id.into()),
                            None,
                            None,
                            true,
                            "mcp-paired-fixture",
                        )
                        .unwrap(),
                )
                .unwrap()
            };
            super::super::session::open_without_pin("mcp-paired-fixture");
            let mut settings = attach(true);
            for message in [
                Message::ClientName("Balance client".into()),
                Message::Wallet(true),
                Message::Capability(0, true),
                Message::Pair,
            ] {
                settings.update(
                    message,
                    Some(wallet.clone()),
                    Some(daemon.clone()),
                    true,
                    "mcp-paired-fixture",
                );
            }
            refresh(&mut settings);
            let first = settings.pairing_store.as_ref().unwrap().clients()[0]
                .id
                .clone();
            let original = configuration(&mut settings, &first);
            let vars = &original["mcpServers"]["tenshu"]["env"];
            let path = std::path::PathBuf::from(vars["TENSHU_MCP_PAIRING"].as_str().unwrap());
            let token = vars["TENSHU_MCP_CREDENTIAL"].as_str().unwrap();
            let socket = wire::pairing::resolve(&path).unwrap();
            let wire::Response::List(list) =
                wire::ipc::call(&socket, token, wire::Request::List(wire::Empty {}))
                    .await
                    .unwrap()
            else {
                panic!("list")
            };
            let opaque = list.wallets[0].wallet_id.clone();
            assert!(
                wire::ipc::call(&socket, token, wire::Request::Summary(input(&opaque)))
                    .await
                    .is_ok()
            );
            assert_eq!(
                wire::ipc::call(
                    &socket,
                    token,
                    wire::Request::History(wire::HistoryInput {
                        wallet_id: opaque.clone(),
                        limit: 1
                    })
                )
                .await
                .unwrap_err(),
                wire::Error::AccessDenied
            );
            for message in [
                Message::ClientName("History client".into()),
                Message::Wallet(true),
                Message::Capability(1, true),
                Message::Pair,
            ] {
                settings.update(
                    message,
                    Some(wallet.clone()),
                    Some(daemon.clone()),
                    true,
                    "mcp-paired-fixture",
                );
            }
            refresh(&mut settings);
            let second = settings.pairing_store.as_ref().unwrap().clients()[1]
                .id
                .clone();
            let second_cfg = configuration(&mut settings, &second);
            let second_vars = &second_cfg["mcpServers"]["tenshu"]["env"];
            let second_path =
                std::path::PathBuf::from(second_vars["TENSHU_MCP_PAIRING"].as_str().unwrap());
            let second_token = second_vars["TENSHU_MCP_CREDENTIAL"].as_str().unwrap();
            assert_eq!(
                wire::ipc::call(
                    &wire::pairing::resolve(&second_path).unwrap(),
                    token,
                    wire::Request::List(wire::Empty {})
                )
                .await
                .unwrap_err(),
                wire::Error::AccessDenied
            );
            super::super::session::close();
            assert!(
                settings
                    .update(
                        Message::CopyPair(first.clone()),
                        None,
                        None,
                        true,
                        "mcp-paired-fixture"
                    )
                    .is_none(),
                "locked Cube must not reveal a private client configuration"
            );
            assert_eq!(
                wire::ipc::call(
                    &wire::pairing::resolve(&path).unwrap(),
                    token,
                    wire::Request::Summary(input(&opaque))
                )
                .await
                .unwrap_err(),
                wire::Error::AccessDenied
            );
            drop(settings);
            assert!(wire::pairing::resolve(&path).is_err());
            super::super::session::open_without_pin("mcp-paired-fixture");
            let mut settings = attach(false);
            refresh(&mut settings);
            assert_eq!(
                configuration(&mut settings, &first),
                original,
                "no credential/config replacement after restart"
            );
            let socket = wire::pairing::resolve(&path).unwrap();
            assert!(
                wire::ipc::call(&socket, token, wire::Request::Summary(input(&opaque)))
                    .await
                    .is_ok()
            );
            settings.update(
                Message::RequestRevokePair(first.clone()),
                None,
                None,
                true,
                "mcp-paired-fixture",
            );
            settings.update(
                Message::RevokePair(first),
                None,
                None,
                true,
                "mcp-paired-fixture",
            );
            refresh(&mut settings);
            assert!(wire::pairing::resolve(&path).is_err());
            assert!(wire::ipc::call(
                &wire::pairing::resolve(&second_path).unwrap(),
                second_token,
                wire::Request::List(wire::Empty {})
            )
            .await
            .is_ok());
            drop(settings);
            let mut settings = attach(false);
            refresh(&mut settings);
            assert_eq!(settings.pairing_store.as_ref().unwrap().clients().len(), 1);
            assert!(
                wire::pairing::resolve(&path).is_err(),
                "revoked pairing must not return on restart"
            );
            assert!(wire::ipc::call(
                &wire::pairing::resolve(&second_path).unwrap(),
                second_token,
                wire::Request::List(wire::Empty {})
            )
            .await
            .is_ok());
            let cube = super::super::settings::CubeSettings::new_with_raw_id(
                "mcp-paired-fixture".into(),
                "fixture".into(),
                chain,
            );
            assert!(
                remove_cube_pairings(&root, &cube).is_err(),
                "active registry ownership must prevent deletion"
            );
            let retained_runtime = settings.paired.pop().unwrap();
            drop(settings);
            assert!(
                pairings::Store::open(location.clone(), zeroize::Zeroizing::new([23; 32]), false)
                    .is_err(),
                "runtime must retain registry ownership after Settings drops"
            );
            drop(retained_runtime);
            let mut replaced = (*wallet).clone();
            replaced.pinned_at = Some(wallet.pinned_at.unwrap_or(0) + 1);
            let mut settings = attach(false);
            settings.ensure_pairings(PairingContext {
                root: &root,
                chain,
                cube_id: "mcp-paired-fixture",
                wallet: Some(Arc::new(replaced)),
                daemon: Some(daemon.clone()),
                supported: true,
                proposal_supported: false,
                #[cfg(feature = "mcp-transactions")]
                last_poll_at_startup: None,
            });
            assert!(
                settings.paired.is_empty(),
                "replacement wallet cannot inherit read permissions"
            );
            assert!(wire::pairing::resolve(&second_path).is_err());
            drop(settings);
            remove_cube_pairings(&root, &cube).unwrap();
            assert!(
                !location.directory.exists(),
                "Cube deletion removes pairing authorization and locators"
            );
            daemon.stop().await.unwrap();
            drop(daemon);
            drop(wallet);
            super::super::session::close();
            std::fs::remove_dir_all(root).unwrap();
        });
    }

    // Runs separately after building the installation's companion executable.
    // Unlike the isolated protocol/IPC tests, this crosses both process and
    // wallet-service boundaries, using the configuration issued by Settings.
    #[test]
    #[ignore = "build tenshu-mcp and set TENSHU_MCP_TEST_ADAPTER; run explicitly in CI"]
    /// Exercise an actual companion process against SQLite and verify revocation.
    fn stdio_client_reads_real_wallet_and_observes_revocation() {
        use serde_json::{json, Value};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let _session_guard = super::super::session::test_guard();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            super::super::session::open_without_pin("mcp-fixture");
            let (root, wallet, daemon) = fixture();
            let mut settings = Settings::default();
            for message in [
                Message::Enable(true), Message::ClientName("stdio fixture".into()),
                Message::Wallet(true), Message::Capability(0, true),
                Message::Capability(1, true), Message::Capability(2, true), Message::Approve,
            ] {
                settings.update(message, Some(wallet.clone()), Some(daemon.clone()), true, "mcp-fixture");
            }
            let config: Value = serde_json::from_str(&settings.update(
                Message::CopyConfig, None, None, true, "mcp-fixture"
            ).unwrap()).unwrap();
            let environment = config["mcpServers"]["tenshu"]["env"].as_object().unwrap();
            let executable = std::env::var_os("TENSHU_MCP_TEST_ADAPTER")
                .expect("set TENSHU_MCP_TEST_ADAPTER to the built adapter");
            let mut child = tokio::process::Command::new(executable)
                .envs(environment.iter().map(|(k,v)| (k, v.as_str().unwrap())))
                .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().unwrap();
            let mut stdin = child.stdin.take().unwrap();
            let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
            /// Send a request and require the next JSON-RPC response to match its ID.
            async fn response(
                stdin: &mut tokio::process::ChildStdin,
                stdout: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
                request: Value,
            ) -> Value {
                let mut bytes = serde_json::to_vec(&request).unwrap();
                bytes.push(b'\n');
                stdin.write_all(&bytes).await.unwrap();
                let line = tokio::time::timeout(Duration::from_secs(10), stdout.next_line())
                    .await.unwrap().unwrap().unwrap();
                let value: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(value["id"], request["id"]);
                assert!(value.get("error").is_none(), "protocol error");
                value["result"].clone()
            }
            let initialized = response(&mut stdin, &mut stdout, json!({
                "jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                    "protocolVersion":"2024-11-05", "capabilities":{},
                    "clientInfo":{"name":"wallet-acceptance", "version":"1"}
                }
            })).await;
            assert_eq!(initialized["serverInfo"]["name"], "tenshu-mcp");
            stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await.unwrap();
            let discovered = response(&mut stdin, &mut stdout, json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"})).await;
            let tools = discovered["tools"].as_array().unwrap();
            let names: std::collections::BTreeSet<_> = tools.iter()
                .map(|tool| tool["name"].as_str().unwrap()).collect();
            assert_eq!(tools.len(), 5);
            assert_eq!(names, std::collections::BTreeSet::from([
                "tenshu_get_capabilities", "tenshu_list_wallets",
                "tenshu_get_wallet_summary", "tenshu_get_transactions",
                "tenshu_get_wallet_policy_summary",
            ]));
            let call = |id, name: &str, arguments: Value| json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{"name":name,"arguments":arguments}});
            let listed = response(&mut stdin, &mut stdout, call(3, "tenshu_list_wallets", json!({}))).await;
            let id = listed["structuredContent"]["wallets"][0]["wallet_id"].as_str().unwrap();
            let summary = response(&mut stdin, &mut stdout, call(4, "tenshu_get_wallet_summary", json!({"wallet_id":id}))).await;
            assert_eq!(summary["structuredContent"]["confirmed_sats"], 55_000);
            let history = response(&mut stdin, &mut stdout, call(5, "tenshu_get_transactions", json!({"wallet_id":id,"limit":10}))).await;
            assert_eq!(history["structuredContent"]["transactions"][0]["incoming_sats"], 55_000);
            assert_eq!(history["structuredContent"]["transactions"][0]["outgoing_sats"], 0);
            let policy = response(&mut stdin, &mut stdout, call(6, "tenshu_get_wallet_policy_summary", json!({"wallet_id":id}))).await;
            assert_eq!(policy["structuredContent"]["recovery"][0]["relative_delay_blocks"], 144);
            let denied = response(&mut stdin, &mut stdout, call(7, "tenshu_get_wallet_summary", json!({"wallet_id":"another-wallet"}))).await;
            assert_eq!(denied["isError"], true);
            assert_eq!(denied["structuredContent"]["error"]["code"], "access_denied");
            super::super::session::close();
            let locked = response(&mut stdin, &mut stdout, call(8, "tenshu_get_wallet_summary", json!({"wallet_id":id}))).await;
            assert_eq!(locked["structuredContent"]["error"]["code"], "access_denied");
            settings.update(Message::Revoke, None, None, true, "mcp-fixture");
            let revoked = response(&mut stdin, &mut stdout, call(9, "tenshu_list_wallets", json!({}))).await;
            assert_eq!(revoked["isError"], true);
            assert!(revoked["structuredContent"].get("wallets").is_none());
            drop(stdin);
            let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await.unwrap().unwrap();
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            drop(settings);
            daemon.stop().await.unwrap();
            drop(daemon);
            drop(wallet);
            std::fs::remove_dir_all(root).unwrap();
        });
    }

    #[test]
    /// Verify consent, all read projections and server teardown with a real database.
    fn real_sqlite_wallet_reads_require_ui_grant_and_revoke_on_settings_drop() {
        let _session_guard = super::super::session::test_guard();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            super::super::session::open_without_pin("mcp-fixture");
            let (root, wallet, daemon) = fixture();
            let mut settings = Settings::default();
            assert!(!settings.enabled && settings.server.is_none());
            settings.update(
                Message::Approve,
                Some(wallet.clone()),
                Some(daemon.clone()),
                true,
                "mcp-fixture",
            );
            assert!(settings.server.is_none());
            for message in [
                Message::Enable(true),
                Message::ClientName("fixture client".into()),
                Message::Wallet(true),
                Message::Capability(0, true),
                Message::Capability(1, true),
                Message::Capability(2, true),
                Message::Approve,
            ] {
                settings.update(
                    message,
                    Some(wallet.clone()),
                    Some(daemon.clone()),
                    true,
                    "mcp-fixture",
                );
            }
            let server = settings.server.as_ref().unwrap();
            let socket = server.socket.clone();
            let credential = server.grant.credential().to_owned();
            let response =
                wire::ipc::call(&socket, &credential, wire::Request::List(wire::Empty {}))
                    .await
                    .unwrap();
            let wire::Response::List(list) = response else {
                panic!("expected list")
            };
            assert_eq!(list.wallets.len(), 1);
            let id = &list.wallets[0].wallet_id;
            let summary = wire::ipc::call(&socket, &credential, wire::Request::Summary(input(id)))
                .await
                .unwrap();
            let wire::Response::Summary(summary) = summary else {
                panic!("expected summary")
            };
            assert_eq!(
                (
                    summary.confirmed_sats,
                    summary.available_sats,
                    summary.unconfirmed_sats
                ),
                (55_000, 55_000, 0)
            );
            let history = wire::ipc::call(
                &socket,
                &credential,
                wire::Request::History(wire::HistoryInput {
                    wallet_id: id.clone(),
                    limit: 10,
                }),
            )
            .await
            .unwrap();
            let wire::Response::History(history) = history else {
                panic!("expected history")
            };
            assert_eq!(history.transactions.len(), 1);
            assert_eq!(history.transactions[0].incoming_sats, 55_000);
            assert_eq!(history.transactions[0].outgoing_sats, 0);
            let policy = wire::ipc::call(&socket, &credential, wire::Request::Policy(input(id)))
                .await
                .unwrap();
            let wire::Response::Policy(policy) = policy else {
                panic!("expected policy")
            };
            assert_eq!((policy.primary.threshold, policy.primary.key_count), (1, 1));
            assert_eq!(policy.recovery[0].relative_delay_blocks, 144);
            let serialized = serde_json::to_string(&(summary, history, policy)).unwrap();
            for private in [
                "xpub",
                "descriptor",
                "script_pubkey",
                "labels",
                "seed",
                "private_key",
                "credential",
            ] {
                assert!(!serialized.contains(private));
            }
            assert_eq!(
                wire::ipc::call(
                    &socket,
                    &credential,
                    wire::Request::Summary(input("other-wallet"))
                )
                .await
                .unwrap_err(),
                wire::Error::AccessDenied
            );
            super::super::session::close();
            assert_eq!(
                wire::ipc::call(&socket, &credential, wire::Request::Summary(input(id)))
                    .await
                    .unwrap_err(),
                wire::Error::AccessDenied
            );
            super::super::session::open_without_pin("mcp-fixture");
            assert_eq!(
                wire::ipc::call(&socket, &credential, wire::Request::Summary(input(id)))
                    .await
                    .unwrap_err(),
                wire::Error::AccessDenied
            );
            // Reapprove in the new session so Settings drop is tested independently
            // of the old session's already-revoked grant.
            settings.update(
                Message::Approve,
                Some(wallet.clone()),
                Some(daemon.clone()),
                true,
                "mcp-fixture",
            );
            let server = settings.server.as_ref().unwrap();
            let grant = server.grant.clone();
            let socket = server.socket.clone();
            let credential = grant.credential().to_owned();
            wire::ipc::call(&socket, &credential, wire::Request::List(wire::Empty {}))
                .await
                .unwrap();
            assert!(grant.active());
            // App owns Settings; dropping it revokes synchronously on both OSes.
            // Windows pipe handles are closed when the aborted task is polled;
            // only Unix has a socket file that must be removed immediately.
            drop(settings);
            assert!(!grant.active());
            #[cfg(unix)]
            assert!(!socket.exists());
            assert!(
                wire::ipc::call(&socket, &credential, wire::Request::List(wire::Empty {}))
                    .await
                    .is_err()
            );
            daemon.stop().await.unwrap();
            drop(daemon);
            drop(wallet);
            super::super::session::close();
            std::fs::remove_dir_all(root).unwrap();
        });
    }

    #[test]
    /// Ensure disabling access or narrowing capabilities invalidates existing client JSON.
    fn disable_and_capability_edit_invalidate_old_configuration() {
        let _session_guard = super::super::session::test_guard();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            super::super::session::open_without_pin("mcp-fixture");
            let (root, wallet, daemon) = fixture();
            let mut settings = Settings::default();
            for message in [
                Message::Enable(true),
                Message::ClientName("client".into()),
                Message::Wallet(true),
                Message::Capability(0, true),
                Message::Approve,
            ] {
                settings.update(
                    message,
                    Some(wallet.clone()),
                    Some(daemon.clone()),
                    true,
                    "mcp-fixture",
                );
            }
            let permission = settings.server.as_ref().unwrap().grant.clone();
            settings.update(
                Message::Capability(1, true),
                None,
                None,
                true,
                "mcp-fixture",
            );
            assert!(!permission.active());
            assert!(settings.server.is_none());
            settings.update(
                Message::Approve,
                Some(wallet.clone()),
                Some(daemon.clone()),
                true,
                "mcp-fixture",
            );
            let permission = settings.server.as_ref().unwrap().grant.clone();
            settings.update(Message::Enable(false), None, None, true, "mcp-fixture");
            assert!(!permission.active());
            assert!(!settings.enabled);
            settings.update(
                Message::Approve,
                Some(wallet.clone()),
                Some(daemon.clone()),
                true,
                "mcp-fixture",
            );
            assert!(settings.server.is_none());
            daemon.stop().await.unwrap();
            drop(daemon);
            drop(wallet);
            drop(settings);
            super::super::session::close();
            std::fs::remove_dir_all(root).unwrap();
        });
    }

    #[test]
    /// Distinguish ordinary account refresh from a backend transition that must revoke.
    fn app_preserves_grant_on_connect_refresh_but_revokes_on_backend_switch() {
        use super::super::{message::Message as AppMessage, App};
        let _session_guard = super::super::session::test_guard();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        let (root, wallet, daemon) = fixture();
        let (mut app, startup) = App::new_inner(
            super::super::Cache::default(),
            wallet,
            None,
            None,
            super::super::Config::new(false),
            daemon.clone(),
            super::super::CoincubeDirectory::new(root.clone()),
            None,
            super::super::settings::CubeSettings::new(
                "MCP fixture".into(),
                crate::chain::ChainId::Bitcoin,
            ),
            None,
        );
        drop(startup);
        super::super::session::open_without_pin(app.cube_settings.id.clone());
        for message in [
            Message::Enable(true),
            Message::Wallet(true),
            Message::ClientName("client".into()),
            Message::Capability(0, true),
            Message::Approve,
        ] {
            drop(app.update(AppMessage::View(view::Message::LocalMcp(message))));
        }
        let grant = app.local_mcp.server.as_ref().unwrap().grant.clone();
        let generation = app.panels.connect.account.session_generation();
        drop(app.update(AppMessage::View(view::Message::ConnectAccount(
            view::ConnectAccountMessage::FeaturesLoaded(None, generation),
        ))));
        assert!(
            grant.active(),
            "routine Connect refresh must preserve the local grant"
        );
        app.revoke_claim();
        drop(app.update(AppMessage::SettingsSaved));
        assert!(
            grant.active(),
            "unrelated claim/settings changes must preserve access"
        );
        let config = daemon.config().unwrap().clone();
        drop(app.spawn_daemon_switch(config));
        assert!(
            !grant.active(),
            "backend switch must revoke before dispatch"
        );
        app.stop();
        drop(app);
        drop(daemon);
        super::super::session::close();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(all(test, feature = "mcp-proposals"))]
mod proposal_consent_tests {
    use super::*;
    #[test]
    fn proposal_creation_pairing_restores_scope_but_never_old_drafts() {
        let _guard = crate::app::session::test_guard();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let root = crate::installer::proposal::tests::TestDir::new();
            std::fs::create_dir(root.path().join("regtest")).unwrap();
            let cube = "public-key-pairing-fixture";
            let location =
                pairings::Location::new(root.path(), crate::chain::ChainId::Regtest, cube).unwrap();
            let mut settings = Settings {
                enabled: true,
                proposals_allowed: true,
                pairing_location: Some(location.clone()),
                pairing_store: Some(
                    pairings::Store::open(
                        location.clone(),
                        zeroize::Zeroizing::new([46; 32]),
                        true,
                    )
                    .unwrap(),
                ),
                ..Settings::default()
            };
            crate::app::session::open_without_pin(cube);
            let refresh = |settings: &mut Settings| {
                settings.ensure_pairings(PairingContext {
                    root: root.path(),
                    chain: crate::chain::ChainId::Regtest,
                    cube_id: cube,
                    wallet: None,
                    daemon: None,
                    supported: false,
                    proposal_supported: true,
                    #[cfg(feature = "mcp-transactions")]
                    last_poll_at_startup: None,
                })
            };
            settings.update(Message::Creation(true), None, None, true, cube);
            assert!(!settings.proposals_allowed);
            settings.update(
                Message::ClientName("Creation fixture".into()),
                None,
                None,
                true,
                cube,
            );
            settings.update(Message::Pair, None, None, true, cube);
            refresh(&mut settings);
            assert_eq!(settings.paired.len(), 1);
            let credential = settings.paired[0].server.grant.credential().to_owned();
            let socket = settings.paired[0].server.socket.clone();
            let input = crate::installer::creation::tests::input();
            let wire::Response::Proposal(first) = wire::ipc::call(
                &socket,
                &credential,
                wire::Request::PrepareCreation(input.clone()),
            )
            .await
            .unwrap() else {
                panic!("validated proposal")
            };
            assert!(first.descriptor_validated);
            assert!(
                wire::ipc::call(&socket, &credential, wire::Request::List(wire::Empty {}))
                    .await
                    .is_err()
            );
            crate::app::session::close();
            assert!(wire::ipc::call(
                &socket,
                &credential,
                wire::Request::PrepareCreation(input.clone())
            )
            .await
            .is_err());
            settings.revoke();
            crate::app::session::open_without_pin(cube);
            refresh(&mut settings);
            assert_eq!(settings.paired.len(), 1);
            let socket = &settings.paired[0].server.socket;
            assert_eq!(settings.paired[0].server.grant.credential(), credential);
            assert!(wire::ipc::call(
                socket,
                &credential,
                wire::Request::Proposal(wire::proposal::ProposalInput {
                    proposal_id: first.proposal_id.clone()
                })
            )
            .await
            .is_err());
            let wire::Response::Proposal(second) =
                wire::ipc::call(socket, &credential, wire::Request::PrepareCreation(input))
                    .await
                    .unwrap()
            else {
                panic!("fresh validated proposal")
            };
            assert_ne!(first.proposal_id, second.proposal_id);
            drop(settings);
            let stored =
                pairings::Store::open(location, zeroize::Zeroizing::new([46; 32]), false).unwrap();
            assert!(matches!(
                stored.clients()[0].scope,
                pairings::Scope::WalletCreation
            ));
            drop(stored);
            crate::app::session::close();
        });
    }
    #[test]
    fn pairing_management_and_failed_add_preserve_other_clients_pending_proposals() {
        let _guard = crate::app::session::test_guard();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let cube = "paired-proposal-fixture";
            let root = std::env::temp_dir()
                .join(format!("tenshu-paired-proposals-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(root.join("regtest")).unwrap();
            let location =
                pairings::Location::new(&root, crate::chain::ChainId::Regtest, cube).unwrap();
            let mut settings = Settings {
                enabled: true,
                proposals_allowed: true,
                pairing_location: Some(location.clone()),
                pairing_store: Some(
                    pairings::Store::open(location, zeroize::Zeroizing::new([31; 32]), true)
                        .unwrap(),
                ),
                ..Settings::default()
            };
            crate::app::session::open_without_pin(cube);
            let refresh = |settings: &mut Settings| {
                settings.ensure_pairings(PairingContext {
                    root: &root,
                    chain: crate::chain::ChainId::Regtest,
                    cube_id: cube,
                    wallet: None,
                    daemon: None,
                    supported: false,
                    proposal_supported: true,
                    #[cfg(feature = "mcp-transactions")]
                    last_poll_at_startup: None,
                })
            };
            let pair = |settings: &mut Settings, name: &str| {
                settings.update(Message::ClientName(name.into()), None, None, true, cube);
                settings.update(Message::Pair, None, None, true, cube);
            };
            pair(&mut settings, "A");
            refresh(&mut settings);
            let a = settings.paired[0].id.clone();
            let socket = settings.paired[0].server.socket.clone();
            let token = settings.paired[0].server.grant.credential().to_owned();
            let input = wire::proposal::PrepareInput {
                request_id: "same-request".into(),
                topology: wire::proposal::Topology {
                    primary: wire::proposal::Quorum {
                        threshold: 3,
                        key_count: 5,
                    },
                    recovery: wire::proposal::Recovery {
                        quorum: wire::proposal::Quorum {
                            threshold: 2,
                            key_count: 3,
                        },
                        relative_delay_blocks: 144,
                    },
                },
            };
            let wire::Response::Proposal(original) = wire::ipc::call(
                &socket,
                &token,
                wire::Request::PrepareProposal(input.clone()),
            )
            .await
            .unwrap() else {
                panic!("proposal")
            };
            // Exercise the same result handler as a failed native Pair action.
            // Storage fault tests prove this result means the old document is exact.
            let owner = settings.pairing_store.as_ref().unwrap().owner();
            settings.finish_pairing(Some(Err(pairings::AddError::Unchanged)));
            assert!(!settings.pairing_poisoned);
            assert!(settings.pairing_error.is_some());
            assert!(settings.paired[0].server.grant.active());
            assert!(Arc::ptr_eq(
                &owner,
                &settings.pairing_store.as_ref().unwrap().owner()
            ));
            refresh(&mut settings);
            for request in [
                wire::Request::Proposal(wire::proposal::ProposalInput {
                    proposal_id: original.proposal_id.clone(),
                }),
                wire::Request::PrepareProposal(input.clone()),
            ] {
                let wire::Response::Proposal(after_failure) =
                    wire::ipc::call(&socket, &token, request).await.unwrap()
                else {
                    panic!("existing proposal remains available after failed add")
                };
                assert_eq!(after_failure, original);
            }
            drop(owner);
            pair(&mut settings, "B");
            refresh(&mut settings);
            let b = settings
                .paired
                .iter()
                .find(|v| v.id != a)
                .unwrap()
                .id
                .clone();
            settings.update(
                Message::RequestRevokePair(b.clone()),
                None,
                None,
                true,
                cube,
            );
            settings.update(Message::RevokePair(b), None, None, true, cube);
            refresh(&mut settings);
            let wire::Response::Proposal(retrieved) = wire::ipc::call(
                &socket,
                &token,
                wire::Request::Proposal(wire::proposal::ProposalInput {
                    proposal_id: original.proposal_id.clone(),
                }),
            )
            .await
            .unwrap() else {
                panic!("proposal")
            };
            assert_eq!(retrieved, original);
            let wire::Response::Proposal(retried) = wire::ipc::call(
                &socket,
                &token,
                wire::Request::PrepareProposal(input.clone()),
            )
            .await
            .unwrap() else {
                panic!("proposal")
            };
            assert_eq!(retried, original);
            settings.paired[0].server.grant.revoke();
            settings.paired[0].proposals.as_ref().unwrap().revoke();
            refresh(&mut settings);
            assert!(
                !settings.paired[0].server.grant.active(),
                "consumed authorization cannot be recreated in the same session"
            );
            assert!(
                wire::ipc::call(&socket, &token, wire::Request::PrepareProposal(input))
                    .await
                    .is_err()
            );
            drop(settings);
            crate::app::session::close();
            std::fs::remove_dir_all(root).unwrap();
        });
    }
    #[test]
    fn explicit_empty_cube_proposal_consent_cannot_elevate_wallet_reads() {
        let _guard = crate::app::session::test_guard();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            crate::app::session::open_without_pin("empty-proposal-fixture");
            let mut settings = Settings::default();
            for message in [
                Message::Enable(true),
                Message::ClientName("fixture".into()),
                Message::Proposals(true),
                Message::Approve,
            ] {
                settings.update(message, None, None, true, "empty-proposal-fixture");
            }
            let server = settings.server.as_ref().unwrap();
            let socket = server.socket.clone();
            let credential = server.grant.credential().to_owned();
            assert!(
                wire::ipc::call(&socket, &credential, wire::Request::List(wire::Empty {}))
                    .await
                    .is_err()
            );
            let proposal = wire::ipc::call(
                &socket,
                &credential,
                wire::Request::PrepareProposal(wire::proposal::PrepareInput {
                    request_id: "fixture".into(),
                    topology: wire::proposal::Topology {
                        primary: wire::proposal::Quorum {
                            threshold: 3,
                            key_count: 5,
                        },
                        recovery: wire::proposal::Recovery {
                            quorum: wire::proposal::Quorum {
                                threshold: 1,
                                key_count: 1,
                            },
                            relative_delay_blocks: 144,
                        },
                    },
                }),
            )
            .await
            .unwrap();
            assert!(matches!(proposal, wire::Response::Proposal(_)));
            assert!(settings.proposal_session("different-cube").is_none());
            assert!(settings
                .proposal_session("empty-proposal-fixture")
                .is_some());
            settings.update(
                Message::Proposals(false),
                None,
                None,
                true,
                "empty-proposal-fixture",
            );
            assert!(settings.server.is_none());
            assert!(settings.proposals.is_none());
            assert!(
                wire::ipc::call(&socket, &credential, wire::Request::List(wire::Empty {}))
                    .await
                    .is_err()
            );
            crate::app::session::close();
        });
    }
}
