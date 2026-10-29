use crate::dir::NetworkDirectory;
use async_fd_lock::LockWrite;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::SeekFrom;
use tokio::fs::OpenOptions;
use tokio::io::AsyncSeekExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::auth::{AccessTokenResponse, AuthClient, AuthError};

pub const CONNECT_CACHE_FILENAME: &str = "connect.json";

/// Opens `connect.json` for read/write, creating it owner-only. An existing
/// file is tightened to `0600` as well: the cache holds refresh tokens.
async fn open_owner_cache(path: &std::path::Path) -> std::io::Result<tokio::fs::File> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    if !path.exists() {
        // Create owner-only before the tokio open. Tokio's OpenOptions on
        // this toolchain cannot set the mode itself.
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
    }
    let file = opts.open(path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .await?;
    }
    Ok(file)
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct ConnectCache {
    #[serde(default)]
    pub active_email: Option<String>,
    pub accounts: Vec<Account>,
}

impl ConnectCache {
    /// Returns the explicitly active account. Legacy single-account caches are
    /// unambiguous and remain eligible for automatic restoration.
    pub fn active_account(&self) -> Option<&Account> {
        match self.active_email.as_deref() {
            Some(email) => self.accounts.iter().find(|account| account.email == email),
            None if self.accounts.len() == 1 => self.accounts.first(),
            None => None,
        }
    }

    fn upsert_credential(
        &mut self,
        email: &str,
        tokens: AccessTokenResponse,
        activate_account: bool,
    ) {
        if let Some(c) = self.accounts.iter_mut().find(|c| c.email == email) {
            c.tokens = tokens;
        } else {
            self.accounts.push(Account {
                email: email.to_string(),
                tokens,
                device_id: None,
                last_seen_event_seq: None,
                reregister_claim: None,
            })
        }
        if activate_account {
            // A new foreground login, including a login for the same email,
            // supersedes any re-registration started by the prior session.
            if let Some(account) = self.accounts.iter_mut().find(|a| a.email == email) {
                account.reregister_claim = None;
            }
            self.active_email = Some(email.to_string());
        }
    }

    pub fn from_file(network_dir: &NetworkDirectory) -> Result<Self, ConnectCacheError> {
        let mut path = network_dir.path().to_path_buf();
        path.push(CONNECT_CACHE_FILENAME);

        std::fs::read(path)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => ConnectCacheError::NotFound,
                _ => ConnectCacheError::ReadingFile(format!("Reading settings file: {}", e)),
            })
            .and_then(|file_content| {
                serde_json::from_slice::<ConnectCache>(&file_content).map_err(|e| {
                    ConnectCacheError::ReadingFile(format!("Parsing settings file: {}", e))
                })
            })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Account {
    pub email: String,
    pub tokens: AccessTokenResponse,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub last_seen_event_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reregister_claim: Option<String>,
}

impl Account {
    pub fn from_cache(
        network_dir: &NetworkDirectory,
        email: &str,
    ) -> Result<Option<Self>, ConnectCacheError> {
        ConnectCache::from_file(network_dir)
            .map(|cache| cache.accounts.into_iter().find(|c| c.email == email))
    }
}

pub async fn update_connect_cache(
    network_dir: &NetworkDirectory,
    current_tokens: &AccessTokenResponse,
    client: &AuthClient,
    refresh: bool,
    activate_account: bool,
) -> Result<AccessTokenResponse, ConnectCacheError> {
    let email = &client.email;
    let mut path = network_dir.path().to_path_buf();
    path.push(CONNECT_CACHE_FILENAME);

    let file_exists = tokio::fs::try_exists(&path).await.unwrap_or(false);

    let mut file = open_owner_cache(&path)
        .await
        .map_err(|e| ConnectCacheError::ReadingFile(format!("Opening file: {}", e)))?
        .lock_write()
        .await
        .map_err(|e| ConnectCacheError::ReadingFile(format!("Locking file: {:?}", e)))?;

    let mut cache = if file_exists {
        let mut file_content = Vec::new();
        file.read_to_end(&mut file_content)
            .await
            .map_err(|e| ConnectCacheError::ReadingFile(format!("Reading file content: {}", e)))?;

        match serde_json::from_slice::<ConnectCache>(&file_content) {
            Ok(cache) => cache,
            Err(e) => {
                tracing::warn!("Something wrong with Liana-Connect cache file: {:?}", e);
                tracing::warn!("Liana-Connect cache file is reset");
                ConnectCache::default()
            }
        }
    } else {
        ConnectCache::default()
    };

    let newer_cached_tokens = cache
        .accounts
        .iter()
        .find(|cred| cred.email == *email)
        .filter(|account| current_tokens.expires_at < account.tokens.expires_at)
        .map(|account| account.tokens.clone());

    let tokens = if let Some(tokens) = newer_cached_tokens {
        tracing::debug!("Liana-Connect authentication tokens are up to date");
        tokens
    } else if refresh {
        client
            .refresh_token(&current_tokens.refresh_token)
            .await
            .map_err(ConnectCacheError::Updating)?
    } else {
        current_tokens.clone()
    };

    cache.upsert_credential(email, tokens.clone(), activate_account);

    let content = serde_json::to_vec_pretty(&cache).map_err(|e| {
        ConnectCacheError::WritingFile(format!("Failed to serialize settings: {}", e))
    })?;

    file.seek(SeekFrom::Start(0)).await.map_err(|e| {
        ConnectCacheError::WritingFile(format!("Failed to seek to start of file: {}", e))
    })?;

    file.write_all(&content).await.map_err(|e| {
        tracing::warn!("failed to write to file: {:?}", e);
        ConnectCacheError::WritingFile(e.to_string())
    })?;

    file.inner_mut()
        .set_len(content.len() as u64)
        .await
        .map_err(|e| ConnectCacheError::WritingFile(format!("Failed to truncate file: {}", e)))?;

    Ok(tokens)
}

/// Persist (or clear) the gRPC-issued `device_id` for the account matching
/// `email`. Pass `Some(id)` to set, `None` to clear so the next
/// `ensure_device_registered` call takes the register path instead of
/// short-circuiting on a stale entry.
///
/// Acquires a write lock on the cache file so the update is atomic against
/// concurrent `update_connect_cache` calls. If the account is not present
/// in the cache (caller never logged in, or the cache was cleared), this
/// is a silent no-op — `ensure_device_registered` retries next launch.
pub async fn set_device_id_for_email(
    network_dir: &NetworkDirectory,
    email: &str,
    device_id: Option<&str>,
) -> Result<(), ConnectCacheError> {
    mutate_device_registration(
        network_dir,
        email,
        DeviceRegistrationUpdate::Set {
            device_id,
            claim: None,
        },
    )
    .await
    .map(|_| ())
}

/// Atomically clear the current id and claim the replacement for this login.
/// A subsequent foreground login invalidates the claim, even for the same email.
pub async fn begin_device_reregistration(
    network_dir: &NetworkDirectory,
    email: &str,
    expected_id: &str,
) -> Result<Option<String>, ConnectCacheError> {
    let claim = uuid::Uuid::new_v4().to_string();
    let applied = mutate_device_registration(
        network_dir,
        email,
        DeviceRegistrationUpdate::Begin {
            expected_id,
            claim: &claim,
        },
    )
    .await?;
    Ok(applied.then_some(claim))
}

/// Commit or roll back only while the exact re-registration claim still owns
/// the cache. A stale RPC cannot write into a newer login's account record.
pub async fn finish_device_reregistration(
    network_dir: &NetworkDirectory,
    email: &str,
    claim: &str,
    device_id: Option<&str>,
) -> Result<bool, ConnectCacheError> {
    mutate_device_registration(
        network_dir,
        email,
        DeviceRegistrationUpdate::Set {
            device_id,
            claim: Some(claim),
        },
    )
    .await
}

enum DeviceRegistrationUpdate<'a> {
    Begin {
        expected_id: &'a str,
        claim: &'a str,
    },
    Set {
        device_id: Option<&'a str>,
        claim: Option<&'a str>,
    },
}

async fn mutate_device_registration(
    network_dir: &NetworkDirectory,
    email: &str,
    update: DeviceRegistrationUpdate<'_>,
) -> Result<bool, ConnectCacheError> {
    let mut path = network_dir.path().to_path_buf();
    path.push(CONNECT_CACHE_FILENAME);

    let mut file = open_owner_cache(&path)
        .await
        .map_err(|e| ConnectCacheError::ReadingFile(format!("Opening file: {}", e)))?
        .lock_write()
        .await
        .map_err(|e| ConnectCacheError::ReadingFile(format!("Locking file: {:?}", e)))?;

    let mut file_content = Vec::new();
    file.read_to_end(&mut file_content)
        .await
        .map_err(|e| ConnectCacheError::ReadingFile(format!("Reading file content: {}", e)))?;
    let mut cache = if file_content.is_empty() {
        // Freshly `create(true)`d (or never written) — start empty.
        ConnectCache::default()
    } else {
        match serde_json::from_slice::<ConnectCache>(&file_content) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "Connect cache at {:?} is corrupt ({}); continuing with an \
                     empty cache for this update",
                    path,
                    e,
                );
                ConnectCache::default()
            }
        }
    };

    let Some(account) = cache.accounts.iter_mut().find(|a| a.email == email) else {
        return Ok(false);
    };
    let changed = match update {
        DeviceRegistrationUpdate::Begin { expected_id, claim } => {
            if cache.active_email.as_deref() != Some(email)
                || account.device_id.as_deref() != Some(expected_id)
                || account.reregister_claim.is_some()
            {
                return Ok(false);
            }
            account.device_id = None;
            account.reregister_claim = Some(claim.to_string());
            true
        }
        DeviceRegistrationUpdate::Set { device_id, claim } => {
            if let Some(claim) = claim {
                if cache.active_email.as_deref() != Some(email)
                    || account.reregister_claim.as_deref() != Some(claim)
                    || account.device_id.is_some()
                {
                    return Ok(false);
                }
                account.device_id = device_id.map(str::to_string);
                account.reregister_claim = None;
                true
            } else {
                if account.device_id.as_deref() == device_id && account.reregister_claim.is_none() {
                    return Ok(true);
                }
                account.device_id = device_id.map(str::to_string);
                // Normal bootstrap can recover an abandoned claim after a
                // crash; its registration supersedes the in-flight request.
                account.reregister_claim = None;
                true
            }
        }
    };
    if !changed {
        return Ok(false);
    }

    let content = serde_json::to_vec_pretty(&cache)
        .map_err(|e| ConnectCacheError::WritingFile(format!("Failed to serialize cache: {}", e)))?;

    file.seek(SeekFrom::Start(0)).await.map_err(|e| {
        ConnectCacheError::WritingFile(format!("Failed to seek to start of file: {}", e))
    })?;
    file.write_all(&content).await.map_err(|e| {
        tracing::warn!("failed to write to file: {:?}", e);
        ConnectCacheError::WritingFile(e.to_string())
    })?;
    file.inner_mut()
        .set_len(content.len() as u64)
        .await
        .map_err(|e| ConnectCacheError::WritingFile(format!("Failed to truncate file: {}", e)))?;
    Ok(true)
}

/// Persist the highest event_seq observed on the realtime stream for the
/// account matching `email`. Idempotent — only writes when the new value is
/// strictly greater, so concurrent stream writers can't move the cursor
/// backwards.
pub async fn set_last_seen_event_seq_for_email(
    network_dir: &NetworkDirectory,
    email: &str,
    event_seq: i64,
) -> Result<(), ConnectCacheError> {
    let mut path = network_dir.path().to_path_buf();
    path.push(CONNECT_CACHE_FILENAME);

    let mut file = open_owner_cache(&path)
        .await
        .map_err(|e| ConnectCacheError::ReadingFile(format!("Opening file: {}", e)))?
        .lock_write()
        .await
        .map_err(|e| ConnectCacheError::ReadingFile(format!("Locking file: {:?}", e)))?;

    let mut file_content = Vec::new();
    file.read_to_end(&mut file_content)
        .await
        .map_err(|e| ConnectCacheError::ReadingFile(format!("Reading file content: {}", e)))?;
    let mut cache = if file_content.is_empty() {
        // Freshly `create(true)`d (or never written) — start empty.
        ConnectCache::default()
    } else {
        match serde_json::from_slice::<ConnectCache>(&file_content) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "Connect cache at {:?} is corrupt ({}); continuing with an \
                     empty cache for this update",
                    path,
                    e,
                );
                ConnectCache::default()
            }
        }
    };

    let Some(account) = cache.accounts.iter_mut().find(|a| a.email == email) else {
        return Ok(());
    };
    if account.last_seen_event_seq.unwrap_or(0) >= event_seq {
        return Ok(());
    }
    account.last_seen_event_seq = Some(event_seq);

    let content = serde_json::to_vec_pretty(&cache)
        .map_err(|e| ConnectCacheError::WritingFile(format!("Failed to serialize cache: {}", e)))?;
    file.seek(SeekFrom::Start(0)).await.map_err(|e| {
        ConnectCacheError::WritingFile(format!("Failed to seek to start of file: {}", e))
    })?;
    file.write_all(&content)
        .await
        .map_err(|e| ConnectCacheError::WritingFile(e.to_string()))?;
    file.inner_mut()
        .set_len(content.len() as u64)
        .await
        .map_err(|e| ConnectCacheError::WritingFile(format!("Failed to truncate file: {}", e)))?;
    Ok(())
}

pub async fn filter_connect_cache(
    network_dir: &NetworkDirectory,
    emails: &HashSet<String>,
) -> Result<(), ConnectCacheError> {
    let mut path = network_dir.path().to_path_buf();
    path.push(CONNECT_CACHE_FILENAME);

    let file_exists = tokio::fs::try_exists(&path).await.unwrap_or(false);

    let mut file = open_owner_cache(&path)
        .await
        .map_err(|e| ConnectCacheError::ReadingFile(format!("Opening file: {}", e)))?
        .lock_write()
        .await
        .map_err(|e| ConnectCacheError::ReadingFile(format!("Locking file: {:?}", e)))?;

    let mut cache = if file_exists {
        let mut file_content = Vec::new();
        file.read_to_end(&mut file_content)
            .await
            .map_err(|e| ConnectCacheError::ReadingFile(format!("Reading file content: {}", e)))?;

        match serde_json::from_slice::<ConnectCache>(&file_content) {
            Ok(cache) => cache,
            Err(e) => {
                tracing::warn!("Something wrong with Liana-Connect cache file: {:?}", e);
                tracing::warn!("Liana-Connect cache file is reset");
                ConnectCache::default()
            }
        }
    } else {
        ConnectCache::default()
    };

    cache.accounts.retain(|a| emails.contains(&a.email));
    if cache
        .active_email
        .as_ref()
        .is_some_and(|email| !emails.contains(email))
    {
        cache.active_email = None;
    }

    let content = serde_json::to_vec_pretty(&cache).map_err(|e| {
        ConnectCacheError::WritingFile(format!("Failed to serialize settings: {}", e))
    })?;

    file.seek(SeekFrom::Start(0)).await.map_err(|e| {
        ConnectCacheError::WritingFile(format!("Failed to seek to start of file: {}", e))
    })?;

    file.write_all(&content).await.map_err(|e| {
        tracing::warn!("failed to write to file: {:?}", e);
        ConnectCacheError::WritingFile(e.to_string())
    })?;

    file.inner_mut()
        .set_len(content.len() as u64)
        .await
        .map_err(|e| ConnectCacheError::WritingFile(format!("Failed to truncate file: {}", e)))?;

    Ok(())
}

#[derive(Debug, Clone)]
pub enum ConnectCacheError {
    NotFound,
    ReadingFile(String),
    WritingFile(String),
    Unexpected(String),
    Updating(AuthError),
}
impl std::fmt::Display for ConnectCacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "ConnectCache file not found"),
            Self::ReadingFile(e) => write!(f, "Error while reading file: {}", e),
            Self::WritingFile(e) => write!(f, "Error while writing file: {}", e),
            Self::Unexpected(e) => write!(f, "Unexpected error: {}", e),
            Self::Updating(e) => write!(f, "Error while updating cache file: {}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        begin_device_reregistration, finish_device_reregistration, set_device_id_for_email,
        AccessTokenResponse, ConnectCache, CONNECT_CACHE_FILENAME,
    };
    use crate::dir::NetworkDirectory;

    fn tokens(label: &str) -> AccessTokenResponse {
        AccessTokenResponse {
            access_token: format!("{label}-access"),
            refresh_token: format!("{label}-refresh"),
            expires_at: 123,
        }
    }

    fn test_network_dir() -> NetworkDirectory {
        let path = std::env::temp_dir().join(format!(
            "connect-registration-cache-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        NetworkDirectory::new(path)
    }

    #[tokio::test]
    async fn stale_reregistration_cannot_overwrite_same_email_login_or_rollback() {
        let dir = test_network_dir();
        let path = dir.path().join(CONNECT_CACHE_FILENAME);
        let email = "alice@example.com";
        let mut cache = ConnectCache::default();
        cache.upsert_credential(email, tokens("a"), true);
        cache.accounts[0].device_id = Some("device-a".into());
        std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();

        let claim = begin_device_reregistration(&dir, email, "device-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            ConnectCache::from_file(&dir)
                .unwrap()
                .active_account()
                .unwrap()
                .device_id,
            None
        );

        // B signs in to the same email before A's RegisterDevice RPC returns.
        let mut b_cache = ConnectCache::from_file(&dir).unwrap();
        b_cache.upsert_credential(email, tokens("b"), true);
        std::fs::write(&path, serde_json::to_vec(&b_cache).unwrap()).unwrap();
        assert!(
            !finish_device_reregistration(&dir, email, &claim, Some("late-a"))
                .await
                .unwrap()
        );
        set_device_id_for_email(&dir, email, Some("device-b"))
            .await
            .unwrap();
        assert!(
            !finish_device_reregistration(&dir, email, &claim, Some("device-a"))
                .await
                .unwrap()
        );

        // A fresh launch reads B's id from the actual cache file.
        let restored = ConnectCache::from_file(&dir).unwrap();
        assert_eq!(
            restored.active_account().unwrap().device_id.as_deref(),
            Some("device-b")
        );
        assert_eq!(
            restored.active_account().unwrap().tokens.access_token,
            "b-access"
        );
        std::fs::remove_dir_all(dir.path()).unwrap();
    }

    #[tokio::test]
    async fn failed_clear_does_not_claim_or_report_registration_ready() {
        let dir = test_network_dir();
        // A directory at the cache path deterministically prevents opening it
        // as a writable file, independent of the test user's permissions.
        let path = dir.path().join(CONNECT_CACHE_FILENAME);
        std::fs::create_dir(&path).unwrap();
        assert!(
            begin_device_reregistration(&dir, "alice@example.com", "device-a")
                .await
                .is_err()
        );
        std::fs::remove_dir_all(dir.path()).unwrap();
    }

    #[tokio::test]
    async fn failed_rpc_restores_old_id_only_for_own_claim() {
        let dir = test_network_dir();
        let path = dir.path().join(CONNECT_CACHE_FILENAME);
        let email = "alice@example.com";
        let mut cache = ConnectCache::default();
        cache.upsert_credential(email, tokens("a"), true);
        cache.accounts[0].device_id = Some("device-a".into());
        std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();

        let claim = begin_device_reregistration(&dir, email, "device-a")
            .await
            .unwrap()
            .unwrap();
        assert!(
            finish_device_reregistration(&dir, email, &claim, Some("device-a"))
                .await
                .unwrap()
        );
        assert!(
            !finish_device_reregistration(&dir, email, &claim, Some("late-a"))
                .await
                .unwrap()
        );
        assert_eq!(
            ConnectCache::from_file(&dir)
                .unwrap()
                .active_account()
                .unwrap()
                .device_id
                .as_deref(),
            Some("device-a")
        );
        std::fs::remove_dir_all(dir.path()).unwrap();
    }

    fn cache_from_json(json: &str) -> ConnectCache {
        serde_json::from_str(json).expect("valid Connect cache")
    }

    #[test]
    fn legacy_single_account_is_restored() {
        let cache = cache_from_json(
            r#"{
                "accounts": [{
                    "email": "alice@example.com",
                    "tokens": {
                        "access_token": "access",
                        "refresh_token": "refresh",
                        "expires_at": 123
                    }
                }]
            }"#,
        );

        assert_eq!(
            cache.active_account().map(|account| account.email.as_str()),
            Some("alice@example.com")
        );
    }

    #[test]
    fn legacy_multi_account_cache_is_not_restored_arbitrarily() {
        let cache = cache_from_json(
            r#"{
                "accounts": [
                    {
                        "email": "alice@example.com",
                        "tokens": {
                            "access_token": "alice-access",
                            "refresh_token": "alice-refresh",
                            "expires_at": 123
                        }
                    },
                    {
                        "email": "bob@example.com",
                        "tokens": {
                            "access_token": "bob-access",
                            "refresh_token": "bob-refresh",
                            "expires_at": 456
                        }
                    }
                ]
            }"#,
        );

        assert!(cache.active_account().is_none());
    }

    #[test]
    fn explicit_active_email_selects_matching_account() {
        let cache = cache_from_json(
            r#"{
                "active_email": "bob@example.com",
                "accounts": [
                    {
                        "email": "alice@example.com",
                        "tokens": {
                            "access_token": "alice-access",
                            "refresh_token": "alice-refresh",
                            "expires_at": 123
                        }
                    },
                    {
                        "email": "bob@example.com",
                        "tokens": {
                            "access_token": "bob-access",
                            "refresh_token": "bob-refresh",
                            "expires_at": 456
                        }
                    }
                ]
            }"#,
        );

        assert_eq!(
            cache.active_account().map(|account| account.email.as_str()),
            Some("bob@example.com")
        );
    }

    #[test]
    fn credential_upsert_marks_account_active() {
        let mut cache = ConnectCache::default();
        cache.upsert_credential(
            "alice@example.com",
            AccessTokenResponse {
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                expires_at: 123,
            },
            true,
        );

        assert_eq!(cache.active_email.as_deref(), Some("alice@example.com"));
        assert_eq!(
            cache.active_account().map(|account| account.email.as_str()),
            Some("alice@example.com")
        );
    }

    #[test]
    fn background_credential_upsert_preserves_active_account() {
        let mut cache = ConnectCache::default();
        cache.upsert_credential(
            "alice@example.com",
            AccessTokenResponse {
                access_token: "alice-access".to_string(),
                refresh_token: "alice-refresh".to_string(),
                expires_at: 123,
            },
            true,
        );
        cache.upsert_credential(
            "bob@example.com",
            AccessTokenResponse {
                access_token: "bob-access".to_string(),
                refresh_token: "bob-refresh".to_string(),
                expires_at: 456,
            },
            false,
        );

        assert_eq!(cache.active_email.as_deref(), Some("alice@example.com"));
        assert_eq!(cache.accounts.len(), 2);
        assert_eq!(
            cache.active_account().map(|account| account.email.as_str()),
            Some("alice@example.com")
        );
    }
}
