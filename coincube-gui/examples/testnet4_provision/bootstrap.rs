use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use coincube_gui::{
    app::state::connect::account::read_session_for_integration,
    services::{
        coincube::{LoginResponse, User},
        connect::{
            client::{auth::AccessTokenResponse, cache::Account},
            crypto::DeviceTransportKey,
            grpc::{self, device::GrpcDeviceClient, interceptor::AuthInterceptor},
        },
    },
};

pub(super) trait Backend {
    fn session(&self, namespace: &str) -> Result<LoginResponse>;
    async fn user(&self, token: &str) -> Result<User>;
    async fn refresh(&self, token: &str) -> Result<LoginResponse>;
    async fn register(&self, token: &str, name: String, public_key: Vec<u8>) -> Result<String>;
}
pub(super) struct Production;
impl Backend for Production {
    fn session(&self, namespace: &str) -> Result<LoginResponse> {
        read_session_for_integration(namespace)
    }
    async fn user(&self, token: &str) -> Result<User> {
        let mut client = CoincubeClient::new();
        client.set_token(token);
        client.get_user().await.map_err(rest_error)
    }
    async fn refresh(&self, token: &str) -> Result<LoginResponse> {
        CoincubeClient::new()
            .refresh_login(token)
            .await
            .map_err(rest_error)
    }
    async fn register(&self, token: &str, name: String, public_key: Vec<u8>) -> Result<String> {
        let channel = grpc::create_channel("https://grpc.coincube.io:443")
            .await
            .map_err(|_| "connect_unavailable")?;
        let mut client = GrpcDeviceClient::new(channel, AuthInterceptor::new(token));
        client
            .register_device(
                name,
                env!("CARGO_PKG_VERSION").into(),
                std::env::consts::OS.into(),
                public_key,
            )
            .await
            .map(|response| response.device_id)
            .map_err(|_| "device_registration_uncertain")
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    version: u32,
    run_id: String,
    profile_dir: PathBuf,
    email: String,
    user_id: u32,
    transport_pubkey: Option<String>,
    device_id: Option<String>,
}
fn expiry(token: &str) -> Result<i64> {
    let parts: Vec<_> = token.split('.').collect();
    check(parts.len() == 3, "desktop_token_invalid")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| "desktop_token_invalid")?;
    let claims: Value = serde_json::from_slice(&bytes).map_err(|_| "desktop_token_invalid")?;
    claims["exp"].as_i64().ok_or("desktop_token_invalid")
}
fn validate_user(user: &User, email: &str, previous_id: Option<u32>) -> Result<()> {
    check(
        user.id > 0 && user.email == email && previous_id.is_none_or(|id| id == user.id),
        "connect_account_mismatch",
    )
}
fn tokens(login: LoginResponse) -> Result<AccessTokenResponse> {
    check(
        !login.requires_2fa && !login.token.is_empty(),
        "desktop_session_incomplete",
    )?;
    Ok(AccessTokenResponse {
        expires_at: expiry(&login.token)?,
        access_token: login.token,
        refresh_token: login.refresh_token,
    })
}
fn present(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("private_file_unavailable"),
    }
}

pub(super) async fn bootstrap(input: &Input, backend: &impl Backend) -> Result<Value> {
    let email = input
        .expected_email
        .as_deref()
        .ok_or("expected_email_required")?;
    check(
        !email.is_empty() && email.trim() == email && email.contains('@'),
        "invalid_expected_email",
    )?;
    let namespace = input.session_namespace.as_deref().unwrap_or("development");
    check(
        matches!(namespace, "development" | "production"),
        "invalid_session_namespace",
    )?;
    // Both REST and gRPC remain production, independent of the keyring namespace.
    std::env::set_var("COINCUBE_API_URL", "https://api.coincube.io");
    let network = CoincubeDirectory::new(input.profile_dir.clone()).network_directory(NETWORK);
    let cache_path = network.path().join("connect.json");
    let key_path = network.path().join("connect_transport_key");
    let binding_path = input.run_dir.join("connect-bootstrap.json");
    let previous: Option<Binding> = if present(&binding_path)? {
        Some(
            serde_json::from_str(&private_read(&binding_path)?)
                .map_err(|_| "bootstrap_binding_invalid")?,
        )
    } else {
        None
    };
    if let Some(binding) = &previous {
        check(
            binding.version == 1
                && binding.run_id == input.run_id
                && binding.profile_dir == input.profile_dir
                && binding.email == email,
            "bootstrap_binding_mismatch",
        )?;
    }
    let mut expected_id = previous.as_ref().map(|binding| binding.user_id);
    let mut cache = if present(&cache_path)? {
        let cache: ConnectCache = serde_json::from_str(&private_read(&cache_path)?)
            .map_err(|_| "connect_cache_invalid")?;
        check(
            cache.accounts.len() == 1 && cache.active_account().is_some_and(|a| a.email == email),
            "connect_account_mismatch",
        )?;
        cache
    } else {
        check(
            previous
                .as_ref()
                .is_none_or(|b| b.transport_pubkey.is_none() && b.device_id.is_none()),
            "bootstrap_cache_missing",
        )?;
        let login = backend.session(namespace)?;
        validate_user(&login.user, email, expected_id)?;
        expected_id = Some(login.user.id);
        ConnectCache {
            active_email: Some(email.to_owned()),
            accounts: vec![Account {
                email: email.to_owned(),
                tokens: tokens(login)?,
                device_id: None,
                last_seen_event_seq: None,
            }],
        }
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock_error")?
        .as_secs() as i64;
    let account = &mut cache.accounts[0];
    // Do not trust the old UI's approximate 30-day cache expiry.
    account.tokens.expires_at = expiry(&account.tokens.access_token)?;
    if account.tokens.expires_at <= now + 30 {
        check(
            !account.tokens.refresh_token.is_empty(),
            "connect_auth_expired",
        )?;
        let login = backend.refresh(&account.tokens.refresh_token).await?;
        validate_user(&login.user, email, expected_id)?;
        expected_id = Some(login.user.id);
        account.tokens = tokens(login)?;
    }
    check(account.tokens.expires_at > now + 30, "connect_auth_expired")?;
    let user = backend.user(&account.tokens.access_token).await?;
    validate_user(&user, email, expected_id)?;
    // No file creation, transport generation, or registration happens until
    // the server has confirmed the exact expected account.
    let mut binding = previous.unwrap_or(Binding {
        version: 1,
        run_id: input.run_id.clone(),
        profile_dir: input.profile_dir.clone(),
        email: email.to_owned(),
        user_id: user.id,
        transport_pubkey: None,
        device_id: None,
    });
    check(
        binding.device_id.is_none() || binding.device_id == account.device_id,
        "bootstrap_device_mismatch",
    )?;
    let registered = account
        .device_id
        .as_ref()
        .is_some_and(|id| !id.trim().is_empty());
    check(
        account.device_id.is_none() || registered,
        "bootstrap_device_invalid",
    )?;
    let key_exists = present(&key_path)?;
    check(
        key_exists || (!registered && binding.transport_pubkey.is_none()),
        "registered_transport_missing",
    )?;
    fs::create_dir_all(network.path()).map_err(|_| "private_write_failed")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(network.path(), fs::Permissions::from_mode(0o700))
            .map_err(|_| "private_write_failed")?;
    }
    save(&binding_path, &binding)?;
    save(&cache_path, &cache)?;
    // load_or_create normally replaces corrupt keys: only call it for absence.
    if key_exists {
        let _ = private_read(&key_path)?;
    } else {
        DeviceTransportKey::load_or_create(&network).map_err(|_| "transport_write_failed")?;
    }
    let key = DeviceTransportKey::load_existing_for_integration(&network)
        .map_err(|_| "transport_invalid")?;
    let public_key = hex::encode(key.public_key());
    check(
        binding
            .transport_pubkey
            .as_ref()
            .is_none_or(|expected| expected == &public_key),
        "bootstrap_transport_mismatch",
    )?;
    fs::File::open(&key_path)
        .and_then(|file| file.sync_all())
        .map_err(|_| "transport_write_failed")?;
    fs::File::open(network.path())
        .and_then(|file| file.sync_all())
        .map_err(|_| "transport_write_failed")?;
    binding.transport_pubkey = Some(public_key);
    save(&binding_path, &binding)?;
    let device_id = if registered {
        cache.accounts[0]
            .device_id
            .clone()
            .ok_or("bootstrap_device_invalid")?
    } else {
        // Production RegisterDevice deduplicates by (user_id, transport_pubkey).
        // A lost response leaves this same durable key for the next attempt.
        backend
            .register(
                &cache.accounts[0].tokens.access_token,
                format!("Testnet4 QA {}", input.run_id),
                key.public_key().to_vec(),
            )
            .await?
    };
    check(!device_id.trim().is_empty(), "bootstrap_device_invalid")?;
    cache.accounts[0].device_id = Some(device_id.clone());
    save(&cache_path, &cache)?;
    binding.device_id = Some(device_id.clone());
    save(&binding_path, &binding)?;
    let reread: ConnectCache =
        serde_json::from_str(&private_read(&cache_path)?).map_err(|_| "connect_cache_invalid")?;
    check(
        reread
            .active_account()
            .is_some_and(|a| a.email == email && a.device_id.as_ref() == Some(&device_id)),
        "bootstrap_cache_mismatch",
    )?;
    Ok(
        json!({"ok":true,"network":"testnet4","email":email,"user_id":user.id,"device_id":device_id}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    struct Fixture {
        directory: PathBuf,
        input: Input,
    }
    impl Fixture {
        fn new() -> Self {
            let directory =
                std::env::temp_dir().join(format!("coincube-bootstrap-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&directory).unwrap();
            fs::create_dir(directory.join("profile")).unwrap();
            let input =
                serde_json::from_value(json!({"action":"bootstrap_auth", "network":"testnet4",
                "profile_dir":directory.join("profile"), "run_dir":directory,
                "run_id":uuid::Uuid::new_v4().to_string(), "expected_email":"qa@example.invalid"}))
                .unwrap();
            Self { directory, input }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
    struct Fake {
        email: String,
        live_email: Option<String>,
        sessions: Cell<usize>,
        registrations: Cell<usize>,
        lose_response: Cell<bool>,
        devices: RefCell<HashMap<Vec<u8>, String>>,
    }
    impl Default for Fake {
        fn default() -> Self {
            Self {
                email: "qa@example.invalid".into(),
                live_email: None,
                sessions: Cell::new(0),
                registrations: Cell::new(0),
                lose_response: Cell::new(false),
                devices: RefCell::new(HashMap::new()),
            }
        }
    }
    impl Backend for Fake {
        fn session(&self, _: &str) -> Result<LoginResponse> {
            self.sessions.set(self.sessions.get() + 1);
            Ok(LoginResponse {
                requires_2fa: false,
                token: format!(
                    "header.{}.signature",
                    URL_SAFE_NO_PAD.encode(br#"{"exp":4102444800}"#)
                ),
                refresh_token: "synthetic-not-a-credential".into(),
                user: User {
                    id: 42,
                    email: self.email.clone(),
                    email_verified: Some(true),
                },
            })
        }
        async fn user(&self, _: &str) -> Result<User> {
            Ok(User {
                id: 42,
                email: self
                    .live_email
                    .clone()
                    .unwrap_or_else(|| self.email.clone()),
                email_verified: Some(true),
            })
        }
        async fn refresh(&self, _: &str) -> Result<LoginResponse> {
            Err("unexpected_refresh")
        }
        async fn register(&self, _: &str, _: String, public_key: Vec<u8>) -> Result<String> {
            self.registrations.set(self.registrations.get() + 1);
            let mut devices = self.devices.borrow_mut();
            let id = devices
                .entry(public_key)
                .or_insert_with(|| "run-device-1".into())
                .clone();
            if self.lose_response.replace(false) {
                Err("device_registration_uncertain")
            } else {
                Ok(id)
            }
        }
    }
    #[tokio::test]
    async fn mismatched_account_does_not_write_or_register() {
        let fixture = Fixture::new();
        let backend = Fake {
            email: "someone-else@example.invalid".into(),
            ..Default::default()
        };
        assert_eq!(
            bootstrap(&fixture.input, &backend).await.unwrap_err(),
            "connect_account_mismatch"
        );
        assert_eq!(backend.registrations.get(), 0);
        assert!(!fixture.directory.join("connect-bootstrap.json").exists());
        assert_eq!(fs::read_dir(&fixture.input.profile_dir).unwrap().count(), 0);
    }
    #[tokio::test]
    async fn server_account_mismatch_does_not_write_or_register() {
        let fixture = Fixture::new();
        let backend = Fake {
            live_email: Some("someone-else@example.invalid".into()),
            ..Default::default()
        };
        assert_eq!(
            bootstrap(&fixture.input, &backend).await.unwrap_err(),
            "connect_account_mismatch"
        );
        assert_eq!(backend.registrations.get(), 0);
        assert!(!fixture.directory.join("connect-bootstrap.json").exists());
        assert_eq!(fs::read_dir(&fixture.input.profile_dir).unwrap().count(), 0);
    }
    #[tokio::test]
    async fn cached_resume_does_not_read_global_session_or_register_twice() {
        let fixture = Fixture::new();
        let backend = Fake::default();
        let first = bootstrap(&fixture.input, &backend).await.unwrap();
        let second = bootstrap(&fixture.input, &backend).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(backend.sessions.get(), 1);
        assert_eq!(backend.registrations.get(), 1);
        let key_path = fixture
            .input
            .profile_dir
            .join("testnet4/connect_transport_key");
        fs::remove_file(key_path).unwrap();
        assert_eq!(
            bootstrap(&fixture.input, &backend).await.unwrap_err(),
            "registered_transport_missing"
        );
        assert_eq!(backend.registrations.get(), 1);
    }
    #[tokio::test]
    async fn lost_registration_response_retries_same_durable_public_key() {
        let fixture = Fixture::new();
        let backend = Fake::default();
        backend.lose_response.set(true);
        assert_eq!(
            bootstrap(&fixture.input, &backend).await.unwrap_err(),
            "device_registration_uncertain"
        );
        let key_path = fixture
            .input
            .profile_dir
            .join("testnet4/connect_transport_key");
        let original = fs::read(&key_path).unwrap();
        let result = bootstrap(&fixture.input, &backend).await.unwrap();
        assert_eq!(result["device_id"], "run-device-1");
        assert_eq!(backend.devices.borrow().len(), 1);
        assert_eq!(backend.registrations.get(), 2);
        assert_eq!(fs::read(key_path).unwrap(), original);
    }
    #[tokio::test]
    async fn corrupt_existing_transport_is_never_replaced() {
        let fixture = Fixture::new();
        let backend = Fake::default();
        backend.lose_response.set(true);
        let _ = bootstrap(&fixture.input, &backend).await;
        let key_path = fixture
            .input
            .profile_dir
            .join("testnet4/connect_transport_key");
        write_private(&key_path, "corrupt").unwrap();
        assert_eq!(
            bootstrap(&fixture.input, &backend).await.unwrap_err(),
            "transport_invalid"
        );
        assert_eq!(fs::read_to_string(key_path).unwrap(), "corrupt");
        assert_eq!(backend.registrations.get(), 1);
    }
}
