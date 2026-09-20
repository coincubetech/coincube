use coincubed::bip329::Labels;
use coincubed::commands::UpdateDerivIndexesResult;
use std::collections::{HashMap, HashSet};
use tokio::sync::Mutex;

use super::{model::*, node, Daemon, DaemonBackend, DaemonError};
use crate::dir::CoincubeDirectory;
use async_trait::async_trait;
use coincube_core::miniscript::bitcoin::{
    address, bip32::ChildNumber, psbt::Psbt, Address, Network, OutPoint, Txid,
};
use coincubed::{
    commands::{CoinStatus, LabelItem},
    config::Config,
    DaemonControl, DaemonHandle,
};

// If an async caller abandons startup, the blocking result still owns cleanup.
// DaemonHandle itself does not stop its poller on drop.
struct PendingConnectDaemon(Option<DaemonHandle>);
impl Drop for PendingConnectDaemon {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            let _ = handle.stop();
        }
    }
}

pub struct EmbeddedDaemon {
    config: Config,
    handle: Mutex<Option<DaemonHandle>>,
    connect_session:
        Option<std::sync::Arc<crate::services::coincube::network_anchor::ConnectAnchorSession>>,
}

impl EmbeddedDaemon {
    pub fn start(config: Config) -> Result<EmbeddedDaemon, DaemonError> {
        let handle =
            DaemonHandle::start_default(config.clone(), false).map_err(DaemonError::Start)?;
        Ok(Self {
            handle: Mutex::new(Some(handle)),
            config,
            connect_session: None,
        })
    }

    /// Only explicit authenticated Connect authority can admit a fork daemon.
    /// The retained configuration is safe for existing UI persistence callers.
    pub async fn start_authenticated(
        config: Config,
        client: crate::services::coincube::CoincubeClient,
    ) -> Result<Self, DaemonError> {
        let chain = config.bitcoin_config.chain;
        if !chain.is_blake2b() || config.pending_bitcoind.is_some() {
            return Err(DaemonError::Unexpected(
                "Invalid Connect-only chain configuration".to_string(),
            ));
        }
        let endpoint = match &config.bitcoin_backend {
            Some(coincubed::config::BitcoinBackend::Esplora(esplora))
                if esplora.fallback_addr.is_none() && esplora.secondary_fallback_addr.is_none() =>
            {
                esplora.addr.clone()
            }
            _ => {
                return Err(DaemonError::Unexpected(
                    "Bitcoin Blake2b requires one authenticated Connect indexer".to_string(),
                ))
            }
        };
        let (backend, session) = client
            .authenticated_backend(chain, &endpoint)
            .await
            .map_err(|e| DaemonError::Unexpected(e.to_string()))?;
        let retained_config = config.for_persistence();
        let result = tokio::task::spawn_blocking(move || {
            DaemonHandle::start_with_connect(config, backend, false)
                .map(|handle| PendingConnectDaemon(Some(handle)))
        })
        .await
        .map_err(|_| DaemonError::Unexpected("Connect daemon startup task failed".to_string()))?;
        match result {
            Ok(mut pending) => Ok(Self {
                config: retained_config,
                handle: Mutex::new(pending.0.take()),
                connect_session: Some(session),
            }),
            Err(error) => {
                session.invalidate();
                Err(DaemonError::Start(error))
            }
        }
    }

    pub async fn command<T, F>(&self, method: F) -> Result<T, DaemonError>
    where
        F: FnOnce(&mut DaemonControl) -> Result<T, DaemonError>,
    {
        match self.handle.lock().await.as_mut() {
            Some(DaemonHandle::Controller { control, .. }) => method(control),
            Some(_) => unreachable!("No coincubed rpc server must be started"),
            None => Err(DaemonError::DaemonStopped),
        }
    }
}

impl Drop for EmbeddedDaemon {
    fn drop(&mut self) {
        // Existing Bitcoin lifecycle is unchanged. A canceled fork startup or
        // abandoned GUI owner must not retain a poller and its authentication.
        if let Some(session) = &self.connect_session {
            session.invalidate();
            if let Some(handle) = self.handle.get_mut().take() {
                let _ = handle.stop();
            }
        }
    }
}

impl<T> From<std::sync::PoisonError<T>> for DaemonError {
    fn from(value: std::sync::PoisonError<T>) -> Self {
        DaemonError::Unexpected(format!("Daemon panic: {}", value))
    }
}

impl std::fmt::Debug for EmbeddedDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonHandle").finish()
    }
}

#[async_trait]
impl Daemon for EmbeddedDaemon {
    fn backend(&self) -> DaemonBackend {
        let node_type = self
            .config
            .bitcoin_backend
            .as_ref()
            .map(node::NodeType::from);
        DaemonBackend::EmbeddedCoincubed(node_type)
    }

    fn config(&self) -> Option<&Config> {
        Some(&self.config)
    }

    fn invalidate_connect_session(&self) {
        if let Some(session) = &self.connect_session {
            session.invalidate();
        }
    }

    async fn is_alive(
        &self,
        _datadir: &CoincubeDirectory,
        _network: Network,
    ) -> Result<(), DaemonError> {
        let mut handle = self.handle.lock().await;
        if let Some(h) = handle.as_ref() {
            if h.is_alive() {
                return Ok(());
            }
        }
        // if the daemon poller is not alive, we try to terminate it to fetch the error.
        if let Some(h) = handle.take() {
            h.stop()
                .map_err(|e| DaemonError::Unexpected(e.to_string()))?;
        }
        Ok(())
    }

    async fn stop(&self) -> Result<(), DaemonError> {
        self.invalidate_connect_session();
        let mut handle = self.handle.lock().await;
        if let Some(h) = handle.take() {
            h.stop()
                .map_err(|e| DaemonError::Unexpected(e.to_string()))?;
        }
        Ok(())
    }

    async fn get_info(&self) -> Result<GetInfoResult, DaemonError> {
        self.command(|daemon| Ok(daemon.get_info())).await
    }

    async fn request_sync(&self) -> Result<(), DaemonError> {
        self.command(|daemon| {
            daemon.request_sync();
            Ok(())
        })
        .await
    }

    async fn get_new_address(&self) -> Result<GetAddressResult, DaemonError> {
        self.command(|daemon| Ok(daemon.get_new_address())).await
    }

    async fn list_revealed_addresses(
        &self,
        is_change: bool,
        exclude_used: bool,
        limit: usize,
        start_index: Option<ChildNumber>,
    ) -> Result<ListRevealedAddressesResult, DaemonError> {
        self.command(|daemon| {
            daemon
                .list_revealed_addresses(is_change, exclude_used, limit, start_index)
                .map_err(|e| DaemonError::Unexpected(e.to_string()))
        })
        .await
    }

    async fn update_deriv_indexes(
        &self,
        receive: Option<u32>,
        change: Option<u32>,
    ) -> Result<UpdateDerivIndexesResult, DaemonError> {
        self.command(|daemon| {
            daemon
                .update_deriv_indexes(receive, change)
                .map_err(|e| DaemonError::Unexpected(e.to_string()))
        })
        .await
    }

    async fn list_coins(
        &self,
        statuses: &[CoinStatus],
        outpoints: &[OutPoint],
    ) -> Result<ListCoinsResult, DaemonError> {
        self.command(|daemon| Ok(daemon.list_coins(statuses, outpoints)))
            .await
    }

    async fn list_spend_txs(&self) -> Result<ListSpendResult, DaemonError> {
        self.command(|daemon| {
            daemon
                .list_spend(None)
                .map_err(|e| DaemonError::Unexpected(e.to_string()))
        })
        .await
    }

    async fn list_confirmed_txs(
        &self,
        start: u32,
        end: u32,
        limit: u64,
    ) -> Result<ListTransactionsResult, DaemonError> {
        self.command(|daemon| Ok(daemon.list_confirmed_transactions(start, end, limit)))
            .await
    }

    async fn list_txs(&self, txids: &[Txid]) -> Result<ListTransactionsResult, DaemonError> {
        self.command(|daemon| Ok(daemon.list_transactions(txids)))
            .await
    }

    async fn create_spend_tx(
        &self,
        coins_outpoints: &[OutPoint],
        destinations: &HashMap<Address<address::NetworkUnchecked>, u64>,
        feerate_vb: u64,
        change_address: Option<Address<address::NetworkUnchecked>>,
    ) -> Result<CreateSpendResult, DaemonError> {
        self.command(|daemon| {
            daemon
                .create_spend(destinations, coins_outpoints, feerate_vb, change_address)
                .map_err(|e| DaemonError::Unexpected(e.to_string()))
        })
        .await
    }

    async fn rbf_psbt(
        &self,
        txid: &Txid,
        is_cancel: bool,
        feerate_vb: Option<u64>,
    ) -> Result<CreateSpendResult, DaemonError> {
        self.command(|daemon| {
            daemon
                .rbf_psbt(txid, is_cancel, feerate_vb)
                .map_err(|e| DaemonError::Unexpected(e.to_string()))
        })
        .await
    }

    async fn update_spend_tx(&self, psbt: &Psbt) -> Result<(), DaemonError> {
        self.command(|daemon| {
            daemon
                .update_spend(psbt.clone())
                .map_err(|e| DaemonError::Unexpected(e.to_string()))
        })
        .await
    }

    async fn delete_spend_tx(&self, txid: &Txid) -> Result<(), DaemonError> {
        self.command(|daemon| {
            daemon.delete_spend(txid);
            Ok(())
        })
        .await
    }

    async fn broadcast_spend_tx(&self, txid: &Txid) -> Result<(), DaemonError> {
        self.command(|daemon| {
            daemon
                .broadcast_spend(txid)
                .map_err(|e| DaemonError::Unexpected(e.to_string()))
        })
        .await
    }

    async fn start_rescan(&self, t: u32) -> Result<(), DaemonError> {
        self.command(|daemon| {
            daemon
                .start_rescan(t)
                .map_err(|e| DaemonError::Unexpected(e.to_string()))
        })
        .await
    }

    async fn create_recovery(
        &self,
        address: Address<address::NetworkUnchecked>,
        coins_outpoints: &[OutPoint],
        feerate_vb: u64,
        sequence: Option<u16>,
    ) -> Result<Psbt, DaemonError> {
        self.command(|daemon| {
            daemon
                .create_recovery(address, coins_outpoints, feerate_vb, sequence)
                .map(|res| res.psbt)
                .map_err(|e| DaemonError::Unexpected(e.to_string()))
        })
        .await
    }

    async fn get_labels(
        &self,
        items: &HashSet<LabelItem>,
    ) -> Result<HashMap<String, String>, DaemonError> {
        self.command(|daemon| Ok(daemon.get_labels(items).labels))
            .await
    }

    async fn update_labels(
        &self,
        items: &HashMap<LabelItem, Option<String>>,
    ) -> Result<(), DaemonError> {
        self.command(|daemon| {
            daemon.update_labels(items);
            Ok(())
        })
        .await
    }

    async fn get_labels_bip329(&self, offset: u32, limit: u32) -> Result<Labels, DaemonError> {
        self.command(|daemon| Ok(daemon.get_labels_bip329(offset, limit).labels))
            .await
    }
}
