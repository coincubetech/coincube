use std::{collections::HashSet, path::Path, time::Duration};

use crate::{
    app::settings::{self, SettingsError, WalletSettings},
    dir::NetworkDirectory,
    services::connect::{
        client::{
            auth::AuthClient,
            cache::{self, ConnectCacheError},
            get_service_config,
        },
        login::{connect_with_credentials, BackendState},
    },
    signer,
};

pub enum DeleteError {
    Io(std::io::Error),
    Settings(SettingsError),
    ConnectCache(ConnectCacheError),
    Connect(String),
    /// The wallet's daemon is still stopping after a lock; nothing was removed.
    DaemonStopping,
}

impl std::fmt::Display for DeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{}", e),
            Self::Settings(e) => write!(f, "{}", e),
            Self::ConnectCache(e) => write!(f, "{}", e),
            Self::Connect(e) => write!(f, "{}", e),
            Self::DaemonStopping => write!(f, "{}", crate::daemon::stopping::STILL_STOPPING),
        }
    }
}

impl From<std::io::Error> for DeleteError {
    fn from(value: std::io::Error) -> Self {
        DeleteError::Io(value)
    }
}

fn ignore_not_found<T>(result: std::io::Result<T>) -> std::io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Leave a directory whose daemon is still stopping alone: on Windows nothing
/// else keeps a delete out of it.
async fn ensure_daemon_stopped(dir: &Path, limit: Duration) -> Result<(), DeleteError> {
    if crate::daemon::stopping::wait_for_async_within(dir.to_path_buf(), limit).await {
        Ok(())
    } else {
        Err(DeleteError::DaemonStopping)
    }
}

pub async fn delete_failed_install(
    network_dir: &NetworkDirectory,
    wallet_id: &settings::WalletId,
) -> Result<(), DeleteError> {
    delete_failed_install_within(network_dir, wallet_id, crate::daemon::stopping::WAIT_LIMIT).await
}

async fn delete_failed_install_within(
    network_dir: &NetworkDirectory,
    wallet_id: &settings::WalletId,
    limit: Duration,
) -> Result<(), DeleteError> {
    let coincubed_directory = network_dir.coincubed_data_directory(wallet_id);
    ensure_daemon_stopped(coincubed_directory.path(), limit).await?;

    if !wallet_id.is_legacy() {
        ignore_not_found(tokio::fs::remove_dir_all(coincubed_directory.path()).await)?;
    } else {
        // if this is a legacy wallet, then it is the only wallet in the network directory.
        ignore_not_found(tokio::fs::remove_file(coincubed_directory.sqlite_db_file_path()).await)?;
        ignore_not_found(
            tokio::fs::remove_dir_all(coincubed_directory.coincubed_watchonly_wallet_path()).await,
        )?;
        ignore_not_found(
            tokio::fs::remove_file(coincubed_directory.path().join("daemon.toml")).await,
        )?;
    }

    let mut remaining_accounts = HashSet::<String>::new();
    settings::update_settings_file(network_dir, |mut settings| {
        settings
            .wallets
            .retain(|settings| settings.wallet_id() != *wallet_id);
        remaining_accounts = settings
            .wallets
            .iter()
            .filter_map(|settings| {
                settings
                    .remote_backend_auth
                    .as_ref()
                    .map(|auth| auth.email.clone())
            })
            .collect();
        // Delete file if both cubes and wallets are empty
        if settings.cubes.is_empty() && settings.wallets.is_empty() {
            None
        } else {
            Some(settings)
        }
    })
    .await
    .map_err(DeleteError::Settings)?;

    cache::filter_connect_cache(network_dir, &remaining_accounts)
        .await
        .map_err(DeleteError::ConnectCache)?;

    signer::delete_wallet_mnemonics(
        network_dir,
        &wallet_id.descriptor_checksum,
        wallet_id.timestamp,
    )
    .map_err(DeleteError::Io)?;

    Ok(())
}

pub async fn delete_wallet<C: Into<crate::chain::ChainId>>(
    chain: C,
    network_dir: &NetworkDirectory,
    wallet: &WalletSettings,
    delete_liana_connect: bool,
) -> Result<(), DeleteError> {
    // `network` is only used to pick the legacy remote-backend service config
    // below — a Bitcoin-family service, reached for Cubes the launcher lists.
    let network = chain.into().bitcoin_network();
    let wallet_id = wallet.wallet_id();
    let coincubed_directory = network_dir.coincubed_data_directory(&wallet_id);
    // Deleting a Cube right after locking it: its daemon may still be stopping
    // and writing to this directory.
    ensure_daemon_stopped(
        coincubed_directory.path(),
        crate::daemon::stopping::WAIT_LIMIT,
    )
    .await?;

    if !wallet_id.is_legacy() {
        ignore_not_found(tokio::fs::remove_dir_all(coincubed_directory.path()).await)?;
    } else {
        // if this is a legacy wallet, then it is the only wallet in the network directory.
        ignore_not_found(tokio::fs::remove_file(coincubed_directory.sqlite_db_file_path()).await)?;
        ignore_not_found(
            tokio::fs::remove_dir_all(coincubed_directory.coincubed_watchonly_wallet_path()).await,
        )?;
        ignore_not_found(
            tokio::fs::remove_file(coincubed_directory.path().join("daemon.toml")).await,
        )?;
    }

    if delete_liana_connect {
        if let Some(auth) = &wallet.remote_backend_auth {
            let service_config = get_service_config(network)
                .await
                .map_err(|e| DeleteError::Connect(e.to_string()))?;

            let client = AuthClient::new(
                service_config.auth_api_url,
                service_config.auth_api_public_key,
                auth.email.to_string(),
            );
            if let BackendState::WalletExists(client, _, _) = connect_with_credentials(
                client,
                auth.wallet_id.clone(),
                service_config.backend_api_url,
                None,
                network,
                network_dir,
            )
            .await
            .map_err(|e| DeleteError::Connect(e.to_string()))?
            {
                tracing::info!("Deleting wallet on Liana-Connect {} backend", network);
                client
                    .delete_wallet()
                    .await
                    .map_err(|e| DeleteError::Connect(e.to_string()))?;
            } else {
                tracing::warn!("Wallet not found on the platform");
            }
        }
    }

    let mut remaining_accounts = HashSet::<String>::new();
    settings::update_settings_file(network_dir, |mut settings| {
        settings
            .wallets
            .retain(|settings| settings.wallet_id() != wallet_id);
        remaining_accounts = settings
            .wallets
            .iter()
            .filter_map(|settings| {
                settings
                    .remote_backend_auth
                    .as_ref()
                    .map(|auth| auth.email.clone())
            })
            .collect();
        // Delete file if both cubes and wallets are empty
        if settings.cubes.is_empty() && settings.wallets.is_empty() {
            None
        } else {
            Some(settings)
        }
    })
    .await
    .map_err(DeleteError::Settings)?;

    cache::filter_connect_cache(network_dir, &remaining_accounts)
        .await
        .map_err(DeleteError::ConnectCache)?;

    signer::delete_wallet_mnemonics(
        network_dir,
        &wallet_id.descriptor_checksum,
        wallet_id.timestamp,
    )
    .map_err(DeleteError::Io)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deleting a failed install while its daemon is still stopping: a wait
    /// that runs out removes nothing.
    #[tokio::test]
    async fn a_delete_refuses_while_the_daemon_is_still_stopping() {
        let root = std::env::temp_dir().join(format!("delete-refuse-{}", uuid::Uuid::new_v4()));
        let network_dir = crate::dir::CoincubeDirectory::new(root.clone())
            .network_directory(crate::chain::ChainId::Bitcoin);
        let wallet_id = settings::WalletId {
            timestamp: Some(1_720_000_000),
            descriptor_checksum: "kt6ht0kt".to_string(),
        };
        let wallet_dir = network_dir
            .coincubed_data_directory(&wallet_id)
            .path()
            .to_path_buf();
        std::fs::create_dir_all(&wallet_dir).unwrap();
        std::fs::write(wallet_dir.join("coincubed.sqlite3"), b"synthetic").unwrap();
        let (release, released) = std::sync::mpsc::channel::<()>();
        crate::daemon::stopping::run_in_background(Some(wallet_dir.clone()), move || {
            released.recv().unwrap();
        });

        let result =
            delete_failed_install_within(&network_dir, &wallet_id, Duration::from_millis(50)).await;
        assert!(matches!(result, Err(DeleteError::DaemonStopping)));
        assert!(wallet_dir.join("coincubed.sqlite3").is_file());

        release.send(()).unwrap();
        assert!(crate::daemon::stopping::wait_for(&wallet_dir));
        let _ = std::fs::remove_dir_all(&root);
    }
}
