//! The paired Cube's restricted Claim resources while its Bitcoin source stays
//! the sole public App. No generic wallet navigation or process-global unlock
//! session is stored here.

use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::sync::watch;

use super::{
    cache::{AppGeneration, Cache},
    settings::CubeSettings,
    state::vault::claim::{self, fork_panel::ForkClaimPanel},
    wallet::Wallet,
    ClaimPairRequest,
};
use crate::{
    daemon::Daemon, dir::CoincubeDirectory, node::bitcoind::Bitcoind,
    services::coincube::CoincubeClient,
};

/// Owns a fork daemon and wallet for one admitted Claim only. Its generation
/// is independent of the source panel's phase generation: Continue and Return
/// revoke a panel without revoking the counterpart resource owner.
pub(crate) struct ClaimPairContext {
    pub(crate) owner: AppGeneration,
    pub(crate) root: CoincubeDirectory,
    pub(crate) source_cube: String,
    pub(crate) fork_cube: String,
    pub(crate) settings: CubeSettings,
    pub(crate) wallet: Arc<Wallet>,
    pub(crate) daemon: Arc<dyn Daemon + Sync + Send>,
    pub(crate) cache: Box<Cache>,
    pub(crate) client: Option<CoincubeClient>,
    verified_account: Option<u32>,
    pub(crate) panel: Option<ForkClaimPanel>,
    pub(crate) loading_panel: Option<u64>,
    bitcoind: Option<Bitcoind>,
    generation: watch::Sender<u64>,
}

impl ClaimPairContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        operation: AppGeneration,
        owner: AppGeneration,
        root: CoincubeDirectory,
        source_cube: String,
        settings: CubeSettings,
        wallet: Arc<Wallet>,
        daemon: Arc<dyn Daemon + Sync + Send>,
        cache: Cache,
        client: Option<CoincubeClient>,
        verified_account: Option<u32>,
        bitcoind: Option<Bitcoind>,
    ) -> Result<Self, String> {
        let wallet_identity_matches = settings.vault_wallet_id.as_ref() == Some(&wallet.id())
            || settings
                .bitcoin_import
                .as_deref()
                .and_then(super::settings::BitcoinImport::active_attachment)
                .is_some_and(|attachment| {
                    attachment.wallet_id == wallet.id()
                        && wallet.main_descriptor.as_imported() == Some(&attachment.descriptor)
                });
        if operation == owner
            || source_cube.is_empty()
            || settings.id.is_empty()
            || source_cube == settings.id
            || settings.network != crate::chain::ChainId::BitcoinBlake2b
            || wallet.chain != settings.network
            || cache.chain() != settings.network
            || cache.datadir_path != root
            || cache.app_generation == owner
            || !wallet_identity_matches
        {
            return Err("The paired Claim Cube changed while opening.".into());
        }
        let fork_cube = settings.id.clone();
        // Fork load replies carry this value. A fresh operation starts with a
        // distinct generation, so an old result cannot match a replacement
        // context even when both target the same Cube and journal.
        let (generation, _) = watch::channel(operation.value());
        Ok(Self {
            owner,
            root,
            source_cube,
            fork_cube,
            settings,
            wallet,
            daemon,
            cache: Box::new(cache),
            client,
            verified_account,
            panel: None,
            loading_panel: None,
            bitcoind,
            generation,
        })
    }

    pub(crate) fn generation(&self) -> watch::Receiver<u64> {
        self.generation.subscribe()
    }

    /// Recheck the account bound at admission before a visible fork action.
    /// A remotely authenticated scoped unlock may rotate its bearer, but its
    /// verified account and provider cannot change. A local fork retains the
    /// source session it was admitted under.
    pub(crate) fn matches_session(&self, session: &claim::ConnectSession) -> bool {
        match (self.client.as_ref(), self.verified_account) {
            (Some(client), Some(account)) => {
                account.to_string() == session.account
                    && client.base_url == session.client.base_url
                    && client.token().is_some_and(|token| !token.is_empty())
            }
            (Some(client), None) => {
                client.base_url == session.client.base_url
                    && client.token() == session.client.token()
            }
            (None, _) => false,
        }
    }

    pub(crate) fn bind(
        &self,
        request: &ClaimPairRequest,
        session: &claim::ConnectSession,
        feature_granted: bool,
    ) -> Option<claim::ClaimForkBinding> {
        let current = if self.verified_account.is_some() {
            if self
                .verified_account
                .is_none_or(|id| id.to_string() != session.account)
            {
                return None;
            }
            self.client.as_ref()?
        } else {
            &session.client
        };
        super::claim_fork_binding(
            &self.root,
            &self.settings,
            &self.wallet,
            &self.daemon,
            claim::BackendState::Ready,
            feature_granted,
            current,
            &session.account,
            self.client.as_ref(),
            self.generation(),
            request,
        )
    }

    /// Synchronously invalidates every outstanding weak binding and signer
    /// result before any resource can be reused by a different operation.
    pub(crate) fn revoke(&mut self) {
        if let Some(panel) = &mut self.panel {
            panel.revoke();
        }
        self.loading_panel = None;
        self.generation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// Explicit shutdown barrier. An external daemon or source-shared node is
    /// never ours to stop; an embedded fork daemon is.
    pub(crate) fn stop(mut self) {
        self.revoke();
        if self.daemon.backend().is_embedded() {
            let daemon = self.daemon.clone();
            let runtime = Handle::try_current().ok();
            let result = std::thread::spawn(move || match runtime {
                Some(handle) => handle
                    .block_on(async { daemon.stop().await })
                    .map_err(|error| error.to_string()),
                None => tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?
                    .block_on(async { daemon.stop().await })
                    .map_err(|error| error.to_string()),
            })
            .join();
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::error!("Claim fork daemon stop failed: {error}"),
                Err(_) => tracing::error!("Claim fork daemon stop panicked"),
            }
        }
        if let Some(bitcoind) = self.bitcoind.take() {
            bitcoind.stop();
        }
    }
}

impl Drop for ClaimPairContext {
    fn drop(&mut self) {
        // Drop is a defensive invalidation barrier. Call `stop` explicitly to
        // shut down an owned embedded daemon before the App is replaced.
        self.revoke();
    }
}
