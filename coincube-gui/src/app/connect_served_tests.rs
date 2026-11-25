//! A Connect-served Bitcoin Blake2b Cube saves the session it was opened with
//! and runs the signer pipeline from it, pinned to that session (Desktop plan
//! PR 3, master invariant I3).
//!
//! Every test runs in its own temporary data directory and checks that what a
//! Bitcoin Cube owns (`bitcoin/connect.json`, `bitcoin/connect_transport_key`
//! and the saved sign-in in the OS keyring) is byte-identical afterwards, and
//! that nothing was written into the working directory.

use super::*;
use crate::services::connect::client::auth::AccessTokenResponse;
use crate::services::connect::client::cache::{Account, ConnectCache, CONNECT_CACHE_FILENAME};
use crate::services::connect::grpc::stream::ConnectStreamConfig;
use iced::futures::StreamExt;

/// The fixture's admitted session (`tests::fork_app`).
const EMAIL: &str = "synthetic@example.invalid";
const CUBE_TOKEN: &str = "synthetic-test-token";
const CUBE_BASE: &str = "https://connect.example/";

/// How `connect.json` records the fixture Cube's host (`CUBE_BASE`), which
/// is not the configured Connect API.
fn cube_issuer() -> String {
    crate::services::connect::client::cache::issuing_api(
        CUBE_BASE,
        &crate::services::coincube_api_base_url(),
    )
    .expect("the fixture Cube's host is not the configured API")
}

fn tokens(access: &str, refresh: &str) -> AccessTokenResponse {
    AccessTokenResponse {
        access_token: access.into(),
        refresh_token: refresh.into(),
        expires_at: 2000,
    }
}

fn shared(access: &str, refresh: &str) -> Arc<tokio::sync::RwLock<AccessTokenResponse>> {
    Arc::new(tokio::sync::RwLock::new(tokens(access, refresh)))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Every message a task produces, in order. The fixture endpoints do not
/// resolve, so a task that reaches the network ends quickly.
async fn outputs(task: Task<Message>) -> Vec<Message> {
    let Some(mut stream) = iced_runtime::task::into_stream(task) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while let Ok(Some(action)) =
        tokio::time::timeout(std::time::Duration::from_secs(20), stream.next()).await
    {
        if let iced_runtime::Action::Output(message) = action {
            out.push(message);
        }
    }
    out
}

/// What a Bitcoin Cube owns, captured byte for byte.
#[derive(Debug, PartialEq, Eq)]
struct BitcoinState {
    connect_json: Option<Vec<u8>>,
    transport_key: Option<Vec<u8>>,
    keyring: Option<Vec<u8>>,
}

fn bitcoin_state(root: &std::path::Path) -> BitcoinState {
    let dir = root.join("bitcoin");
    BitcoinState {
        connect_json: std::fs::read(dir.join(CONNECT_CACHE_FILENAME)).ok(),
        transport_key: std::fs::read(dir.join("connect_transport_key")).ok(),
        keyring: state::connect::read_connect_secret(state::connect::CONNECT_KEYRING_USER),
    }
}

/// A Bitcoin Cube's session for the same account, its transport key, and a
/// saved sign-in. `saved_token` is the saved sign-in's access token: the
/// Cube's own when the saved sign-in is the session it was opened with.
fn seed_bitcoin(root: &std::path::Path, saved_token: &str) -> BitcoinState {
    let dir = root.join("bitcoin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(CONNECT_CACHE_FILENAME),
        serde_json::to_vec(&serde_json::json!({
            "active_email": EMAIL,
            "accounts": [{"email": EMAIL, "tokens": tokens("bitcoin-access", "bitcoin-refresh"),
                "device_id": "bitcoin-device", "last_seen_event_seq": 3}]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("connect_transport_key"), [7u8; 32]).unwrap();
    let saved = crate::services::coincube::LoginResponse {
        requires_2fa: false,
        token: saved_token.into(),
        refresh_token: "saved-refresh".into(),
        user: crate::services::coincube::User {
            id: 7,
            email: EMAIL.into(),
            email_verified: Some(true),
        },
    };
    state::connect::write_connect_secret(
        state::connect::CONNECT_KEYRING_USER,
        &serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    bitcoin_state(root)
}

fn fork_dir(root: &std::path::Path) -> crate::dir::NetworkDirectory {
    CoincubeDirectory::new(root.to_path_buf())
        .network_directory(crate::chain::ChainId::BitcoinBlake2b)
}

fn write_fork_cache(root: &std::path::Path, cache: serde_json::Value) {
    let dir = fork_dir(root);
    std::fs::create_dir_all(dir.path()).unwrap();
    std::fs::write(
        dir.path().join(CONNECT_CACHE_FILENAME),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
}

fn fork_entry(root: &std::path::Path) -> Option<Account> {
    Account::from_cache(&fork_dir(root), EMAIL).ok().flatten()
}

/// Bitcoin's state is untouched, and nothing landed in the working
/// directory (the tests run from the crate directory).
fn assert_bitcoin_untouched(root: &std::path::Path, before: &BitcoinState) {
    assert_eq!(&bitcoin_state(root), before);
    for chain_dir in ["bitcoin", "bitcoin-blake2b"] {
        let stray = std::path::Path::new(chain_dir).join(CONNECT_CACHE_FILENAME);
        assert!(
            std::fs::read_to_string(&stray).map_or(true, |text| !text.contains(CUBE_TOKEN)),
            "a session was written under the working directory: {}",
            stray.display()
        );
    }
}

/// Walks the admitted panel to its dashboard as the runtime does: its user
/// loads, then the duress gate clears. Returns what the last step answered
/// with; the user load's own requests are not run.
async fn reach_dashboard(app: &mut App) -> Vec<Message> {
    let account = &app.panels.connect.account;
    let epoch = account.auth_epoch();
    let generation = account.session_generation();
    let user = account.user.clone().unwrap();
    app.panels.connect.account.step = state::connect::account::ConnectFlowStep::CheckingSession;
    drop(app.update(Message::View(view::Message::ConnectAccount(
        view::ConnectAccountMessage::AdmittedUserLoaded {
            user: Ok(user),
            generation,
            epoch,
        },
    ))));
    let clear: crate::services::coincube::DuressState = serde_json::from_value(
        serde_json::json!({"active": false, "enrolled": false, "this_device_registered": false}),
    )
    .unwrap();
    let generation = app.panels.connect.account.session_generation();
    outputs(app.update(Message::View(view::Message::ConnectAccount(
        view::ConnectAccountMessage::DuressStateChecked(
            crate::services::coincube::DuressCheckOutcome::Ok(clear),
            generation,
            0,
        ),
    ))))
    .await
}

/// Runs the save at open and hands its result back to the App, as the
/// runtime does. Returns what the App answered with.
async fn save_at_open(app: &mut App) -> Task<Message> {
    let saved = outputs(app.save_served_session())
        .await
        .into_iter()
        .find(|m| matches!(m, Message::ConnectServedSessionSaved { .. }))
        .expect("the save at open answers");
    app.update(saved)
}

fn stream_config(
    tokens: Arc<tokio::sync::RwLock<AccessTokenResponse>>,
    base_url: &str,
    device_id: &str,
) -> ConnectStreamConfig {
    ConnectStreamConfig {
        base_url: base_url.into(),
        grpc_url: "https://grpc.connect.example".into(),
        tokens,
        device_id: device_id.into(),
        user_agent: "test".into(),
        vault_ids: vec![],
        last_seen_seq: 0,
    }
}

/// Opens a Connect-served Cube whose session is saved and whose login
/// completion handed back `TriggerConnectStreamReady`, then installs the
/// stream with `device_id` as the id the bootstrap carried.
async fn served_cube_with_stream(root: &std::path::Path, device_id: &str) -> App {
    let mut app = tests::fork_app(root, true);
    assert!(app.cache.connect_served);
    let completion = outputs(save_at_open(&mut app).await).await;
    let trigger = completion
        .into_iter()
        .find(|m| matches!(m, Message::TriggerConnectStreamReady { .. }))
        .expect("the login completion triggers the stream");
    drop(app.update(trigger));
    let auth = app
        .connect_auth
        .clone()
        .expect("the trigger installs the auth");
    assert_eq!(auth.read().await.access_token, CUBE_TOKEN);
    drop(app.update(Message::ConnectStreamReady(Some((
        stream_config(auth, CUBE_BASE, device_id),
        EMAIL.into(),
    )))));
    app
}

/// The session the Cube was opened with is written to its own chain folder:
/// exactly its access token, the saved sign-in's refresh token because that
/// is the same session, and the account made active. A different token
/// stored for the account is replaced; its device id, cursor and the other
/// account stay.
#[test]
fn a_connect_served_cube_saves_exactly_its_own_session_at_open() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        write_fork_cache(
            root.path(),
            serde_json::json!({
                "active_email": "other@example.invalid",
                "accounts": [
                    {"email": EMAIL, "tokens": tokens("older-open", "older-refresh"),
                        "device_id": "stored-device", "last_seen_event_seq": 9,
                        "api_base_url": cube_issuer()},
                    {"email": "other@example.invalid", "tokens": tokens("other-access", "other-refresh")}
                ]
            }),
        );
        let mut app = tests::fork_app(root.path(), true);
        // Through the App's own route: the admitted panel reaches its
        // dashboard with the account's email.
        let routed = reach_dashboard(&mut app).await;
        assert!(app.panels.connect.account.is_authenticated());
        assert_eq!(app.served_session, ServedSession::Saving);
        assert_eq!(
            app.cache.signer_pipeline_start,
            cache::SignerPipelineStart::Starting
        );
        let saved = routed
            .into_iter()
            .find(|m| matches!(m, Message::ConnectServedSessionSaved { .. }))
            .expect("the dashboard starts the save");
        let Message::ConnectServedSessionSaved { saved: true, .. } = &saved else {
            panic!("the save was refused");
        };

        let file = ConnectCache::from_file(&fork_dir(root.path())).unwrap();
        assert_eq!(file.active_email.as_deref(), Some(EMAIL));
        let entry = file.active_account().unwrap();
        assert_eq!(entry.tokens.access_token, CUBE_TOKEN);
        assert_eq!(entry.tokens.refresh_token, "saved-refresh");
        assert_eq!(entry.device_id.as_deref(), Some("stored-device"));
        assert_eq!(entry.last_seen_event_seq, Some(9));
        assert_eq!(entry.api_base_url.as_deref(), Some(cube_issuer().as_str()));
        let other = file
            .accounts
            .iter()
            .find(|a| a.email == "other@example.invalid")
            .unwrap();
        assert_eq!(other.tokens.access_token, "other-access");

        // The completion that follows is the Bitcoin login completion: it
        // owns the session it just wrote, so it carries on to the stream.
        let completion = outputs(app.update(saved)).await;
        assert!(matches!(
            app.served_session,
            ServedSession::Saved { ref email, .. } if email == EMAIL
        ));
        assert!(completion
            .iter()
            .any(|m| matches!(m, Message::TriggerConnectStreamReady { email, .. } if email == EMAIL)));
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// When the saved sign-in is another session, nothing of it is written: the
/// refresh token is left empty, which this Cube never sends anywhere.
#[test]
fn another_saved_sign_in_lends_the_cube_no_refresh_token() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), "a-later-sign-in");
        let mut app = tests::fork_app(root.path(), true);
        drop(save_at_open(&mut app).await);
        let entry = fork_entry(root.path()).unwrap();
        assert_eq!(entry.tokens.access_token, CUBE_TOKEN);
        assert_eq!(entry.tokens.refresh_token, "");
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// The pipeline reaches `ConnectStreamReady` and fills the same cache fields
/// as on a Bitcoin Cube, on the Cube's own endpoint and token. A reopened
/// Cube reuses the device id stored for its account.
#[test]
fn a_connect_served_cube_fills_the_same_stream_fields_as_a_bitcoin_cube() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        write_fork_cache(
            root.path(),
            serde_json::json!({
                "active_email": EMAIL,
                "accounts": [{"email": EMAIL, "tokens": tokens("older-open", ""),
                    "device_id": "stored-device", "last_seen_event_seq": 12,
                    "api_base_url": cube_issuer()}]
            }),
        );
        let app = served_cube_with_stream(root.path(), "").await;
        let auth = app.connect_auth.clone().unwrap();

        let bitcoin_root = tempfile::tempdir().unwrap();
        let (mut bitcoin, _) = super::shell_tests::bitcoin_app(bitcoin_root.path());
        bitcoin.cache.datadir_path = CoincubeDirectory::new(bitcoin_root.path().to_path_buf());
        let dir = bitcoin
            .cache
            .datadir_path
            .network_directory(bitcoin.cache.chain());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join(CONNECT_CACHE_FILENAME),
            serde_json::to_vec(&serde_json::json!({
                "active_email": EMAIL,
                "accounts": [{"email": EMAIL, "tokens": tokens("bitcoin-access", "bitcoin-refresh"),
                    "device_id": "stored-device", "last_seen_event_seq": 12}]
            }))
            .unwrap(),
        )
        .unwrap();
        let bitcoin_auth = shared("bitcoin-access", "bitcoin-refresh");
        bitcoin.connect_auth = Some(bitcoin_auth.clone());
        bitcoin.connect_email = Some(EMAIL.into());
        drop(bitcoin.update(Message::ConnectStreamReady(Some((
            stream_config(bitcoin_auth.clone(), CUBE_BASE, "old-device"),
            EMAIL.into(),
        )))));

        for (cube, owner) in [(&app, &auth), (&bitcoin, &bitcoin_auth)] {
            let cfg = cube
                .connect_stream_config
                .as_ref()
                .expect("the stream mounts");
            assert!(Arc::ptr_eq(&cfg.tokens, owner));
            assert_eq!(cfg.device_id, "stored-device");
            assert_eq!(cfg.last_seen_seq, 12);
            assert_eq!(cfg.base_url, CUBE_BASE);
            assert!(Arc::ptr_eq(
                cube.cache.connect_tokens.as_ref().unwrap(),
                owner
            ));
            assert_eq!(
                cube.cache.connect_device_id.as_deref(),
                Some("stored-device")
            );
            assert_eq!(
                cube.cache.connect_grpc_url.as_deref(),
                Some("https://grpc.connect.example")
            );
            assert_eq!(cube.cache.connect_base_url.as_deref(), Some(CUBE_BASE));
            assert_eq!(cube.cache.connect_email.as_deref(), Some(EMAIL));
            assert_eq!(
                cube.cache.connect_stream_status,
                ConnectionStatus::Connecting
            );
            assert_eq!(
                cube.cache.signer_pipeline_start,
                cache::SignerPipelineStart::Idle
            );
        }
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// The shared file moves on to another token for the same account while
/// the Cube is open (a local Cube of the account refreshed it): the Cube's
/// stream stays on its own token, a later `EnsureConnectReady` does not
/// adopt the file's token, and a bootstrap carrying it is refused.
#[test]
fn a_later_token_in_the_shared_file_never_reaches_the_pipeline() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        let mut app = served_cube_with_stream(root.path(), "returned-device").await;
        let owner = app.connect_auth.clone().unwrap();
        write_fork_cache(
            root.path(),
            serde_json::json!({
                "active_email": EMAIL,
                "accounts": [{"email": EMAIL, "tokens": tokens("local-cube-token", "local-refresh"),
                    "device_id": "returned-device"}]
            }),
        );

        for message in outputs(app.update(Message::EnsureConnectReady)).await {
            // Whatever the bootstrap answers, feed it back as the runtime would.
            drop(app.update(message));
        }
        let foreign = shared("local-cube-token", "local-refresh");
        drop(app.update(Message::TriggerConnectStreamReady {
            chain: app.cache.chain(),
            datadir: app.cache.datadir_path.clone(),
            tokens: foreign.clone(),
            email: EMAIL.into(),
            cube_uuid: None,
            device_id: None,
        }));
        drop(app.update(Message::ConnectStreamReady(Some((
            stream_config(foreign, CUBE_BASE, "returned-device"),
            EMAIL.into(),
        )))));

        assert!(Arc::ptr_eq(app.connect_auth.as_ref().unwrap(), &owner));
        let cfg = app.connect_stream_config.as_ref().unwrap();
        assert!(Arc::ptr_eq(&cfg.tokens, &owner));
        assert_eq!(cfg.tokens.read().await.access_token, CUBE_TOKEN);
        assert_eq!(
            app.cache.connect_device_id.as_deref(),
            Some("returned-device")
        );
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// A write the guarded writer refuses (here, a file it cannot parse) leaves
/// the Cube open and says Mobile signing could not start; `EnsureConnectReady`
/// tries again and, once the file is readable, saves the session.
#[test]
fn a_refused_save_leaves_the_cube_open_with_a_retry() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        let dir = fork_dir(root.path());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join(CONNECT_CACHE_FILENAME), b"{not json").unwrap();
        let mut app = tests::fork_app(root.path(), true);

        let answered = outputs(save_at_open(&mut app).await).await;
        assert_eq!(app.served_session, ServedSession::Failed);
        // The open signer list is told at once, so its retry shows without
        // waiting for an unrelated message.
        assert!(answered.iter().any(|m| matches!(
            m,
            Message::KeychainSign(
                state::vault::keychain_sign::KeychainSignMessage::StreamHealth(_)
            )
        )));
        assert_eq!(
            app.cache.signer_pipeline_start,
            cache::SignerPipelineStart::Failed
        );
        assert!(app.fork_connect_client.is_some());
        assert!(app.panels.connect.account.authenticated_client().is_some());
        assert!(app.connect_stream_config.is_none());
        assert_eq!(
            std::fs::read(dir.path().join(CONNECT_CACHE_FILENAME)).unwrap(),
            b"{not json"
        );

        std::fs::remove_file(dir.path().join(CONNECT_CACHE_FILENAME)).unwrap();
        let retry = outputs(app.update(Message::EnsureConnectReady)).await;
        assert_eq!(app.served_session, ServedSession::Saving);
        let saved = retry
            .into_iter()
            .find(|m| matches!(m, Message::ConnectServedSessionSaved { saved: true, .. }))
            .expect("the retry saves the session");
        drop(app.update(saved));
        assert!(matches!(app.served_session, ServedSession::Saved { .. }));
        assert_eq!(
            fork_entry(root.path()).unwrap().tokens.access_token,
            CUBE_TOKEN
        );
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// The three login messages stay refused on a Connect-served Cube, whatever
/// sends them: a sign-in started inside it would replace its session (I3).
#[test]
fn a_connect_served_cube_still_refuses_every_login_message() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        let mut app = tests::fork_app(root.path(), true);
        let epoch = app.panels.connect.account.auth_epoch();
        let login = crate::services::coincube::LoginResponse {
            requires_2fa: false,
            token: "in-cube-sign-in".into(),
            refresh_token: "in-cube-refresh".into(),
            user: crate::services::coincube::User {
                id: 7,
                email: EMAIL.into(),
                email_verified: Some(true),
            },
        };
        for (message, answers_commit) in [
            (
                Message::InAppConnectLoginCompleted {
                    token: CUBE_TOKEN.into(),
                    refresh_token: String::new(),
                    email: EMAIL.into(),
                    epoch,
                },
                false,
            ),
            (
                Message::InAppConnectAuthReady {
                    login: login.clone(),
                    epoch,
                    previous_refresh_token: None,
                },
                true,
            ),
            (
                Message::InAppConnectAuthPersisted {
                    login: login.clone(),
                    epoch,
                    previous_refresh_token: None,
                    tokens: Some(tokens("in-cube-sign-in", "in-cube-refresh")),
                },
                false,
            ),
        ] {
            let answered = outputs(app.update(message)).await;
            assert!(answered
                .iter()
                .any(|m| matches!(m, Message::View(view::Message::ShowError(_)))));
            assert_eq!(
                answered.iter().any(|m| matches!(
                    m,
                    Message::View(view::Message::ConnectAccount(
                        view::ConnectAccountMessage::AuthCommitRejected { .. }
                    ))
                )),
                answers_commit
            );
            assert!(!answered
                .iter()
                .any(|m| matches!(m, Message::TriggerConnectStreamReady { .. })));
        }
        assert!(app.connect_auth.is_none());
        assert_eq!(app.served_session, ServedSession::NotSaved);
        assert_eq!(
            app.fork_connect_client.as_ref().and_then(|c| c.token()),
            Some(CUBE_TOKEN)
        );
        assert!(fork_entry(root.path()).is_none());
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// An ended session ends a Connect-served Cube's stream for good, with the
/// reopen notice; a Bitcoin Cube keeps today's retry.
#[test]
fn an_ended_session_ends_a_connect_served_stream_without_a_reconnect_loop() {
    use crate::services::connect::grpc::ConnectStreamMessage as M;
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        let mut app = served_cube_with_stream(root.path(), "returned-device").await;
        drop(app.update(Message::ConnectStream(M::Connected)));
        drop(app.update(Message::ConnectStream(M::SessionEnded(
            "token expired".into(),
        ))));
        assert!(app.connect_stream_config.is_none());
        assert!(app.cache.connect_device_id.is_none());
        assert!(app.cache.connect_grpc_url.is_none());
        assert!(app.cache.connect_tokens.is_none());
        assert_eq!(app.cache.connect_stream_status, ConnectionStatus::Inactive);
        assert!(app.fork_connect_client.is_none());
        assert!(app.panels.connect.account.requires_authenticated_reopen());
        // The session saved in the chain folder stays, as a Bitcoin Cube's does.
        assert_eq!(
            fork_entry(root.path()).unwrap().tokens.access_token,
            CUBE_TOKEN
        );
        assert_bitcoin_untouched(root.path(), &before);

        let bitcoin_root = tempfile::tempdir().unwrap();
        let (mut bitcoin, _) = super::shell_tests::bitcoin_app(bitcoin_root.path());
        let auth = shared("bitcoin-access", "bitcoin-refresh");
        bitcoin.connect_auth = Some(auth.clone());
        bitcoin.connect_email = Some(EMAIL.into());
        bitcoin.connect_stream_config = Some(stream_config(auth, CUBE_BASE, "bitcoin-device"));
        bitcoin.cache.connect_stream_status = ConnectionStatus::Connected;
        drop(bitcoin.update(Message::ConnectStream(M::SessionEnded(
            "token expired".into(),
        ))));
        assert!(bitcoin.connect_stream_config.is_some());
        assert_eq!(
            bitcoin.cache.connect_stream_status,
            ConnectionStatus::Reconnecting
        );
    });
}

/// The shared file moved on before the stream started. The stream still
/// starts on the Cube's own token: with the id stored for the account when
/// the entry is the Cube's host's, or with the id registration returned, or
/// the signer list says Mobile signing could not start. An id another host
/// stored (a local Cube's sign-in on the configured API) is never used.
#[test]
fn the_stream_starts_on_the_cube_token_after_the_file_moved_on() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let cube_host = Some(cube_issuer());
        for (stored, issuer, carried, expected) in [
            (Some("stored-device"), cube_host.clone(), "", Some("stored-device")),
            (Some("stored-device"), None, "", None),
            (
                Some("stored-device"),
                None,
                "returned-device",
                Some("returned-device"),
            ),
            (None, None, "returned-device", Some("returned-device")),
            (None, None, "", None),
        ] {
            let root = tempfile::tempdir().unwrap();
            let before = seed_bitcoin(root.path(), CUBE_TOKEN);
            let mut app = tests::fork_app(root.path(), true);
            let completion = outputs(save_at_open(&mut app).await).await;
            let trigger = completion
                .into_iter()
                .find(|m| matches!(m, Message::TriggerConnectStreamReady { .. }))
                .unwrap();
            // Another Cube of the account signs in and moves the file on.
            write_fork_cache(
                root.path(),
                serde_json::json!({
                    "active_email": EMAIL,
                    "accounts": [{"email": EMAIL, "tokens": tokens("local-cube-token", "local-refresh"),
                        "device_id": stored, "api_base_url": issuer}]
                }),
            );
            drop(app.update(trigger));
            let auth = app.connect_auth.clone().expect("pinned to the Cube, not the file");
            drop(app.update(Message::ConnectStreamReady(Some((
                stream_config(auth.clone(), CUBE_BASE, carried),
                EMAIL.into(),
            )))));
            match expected {
                Some(id) => {
                    let cfg = app.connect_stream_config.as_ref().unwrap();
                    assert_eq!(cfg.device_id, id);
                    assert_eq!(cfg.tokens.read().await.access_token, CUBE_TOKEN);
                    assert_eq!(
                        app.cache.signer_pipeline_start,
                        cache::SignerPipelineStart::Idle
                    );
                }
                None => {
                    assert!(app.connect_stream_config.is_none());
                    assert_eq!(
                        app.cache.signer_pipeline_start,
                        cache::SignerPipelineStart::Failed
                    );
                }
            }
            // The file keeps the other Cube's token; this Cube adopted none.
            assert_eq!(
                fork_entry(root.path()).unwrap().tokens.access_token,
                "local-cube-token"
            );
            assert_bitcoin_untouched(root.path(), &before);
        }
    });
}

/// Another Cube of the account received later events: this Cube's stream
/// resumes from its own cursor, not past events it has not received.
#[test]
fn the_stream_resumes_from_this_cubes_own_cursor() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        let mut app = tests::fork_app(root.path(), true);
        let cube_id = app.cube_settings.id.clone();
        assert!(!cube_id.is_empty());
        let completion = outputs(save_at_open(&mut app).await).await;
        let trigger = completion
            .into_iter()
            .find(|m| matches!(m, Message::TriggerConnectStreamReady { .. }))
            .unwrap();
        write_fork_cache(
            root.path(),
            serde_json::json!({
                "active_email": EMAIL,
                "accounts": [{"email": EMAIL, "tokens": tokens(CUBE_TOKEN, ""),
                    "device_id": "stored-device", "last_seen_event_seq": 50,
                    "cube_event_seqs": {"a-sibling-cube": 50, cube_id: 12},
                    "api_base_url": cube_issuer()}]
            }),
        );
        drop(app.update(trigger));
        let auth = app.connect_auth.clone().unwrap();
        drop(app.update(Message::ConnectStreamReady(Some((
            stream_config(auth, CUBE_BASE, ""),
            EMAIL.into(),
        )))));
        let cfg = app
            .connect_stream_config
            .as_ref()
            .expect("the stream mounts");
        assert_eq!(cfg.device_id, "stored-device");
        assert_eq!(cfg.last_seen_seq, 12);
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// The shared file moved on between the save at open and the login
/// completion's check (another Cube of the account saved its own session).
/// The first start still triggers the stream on the Cube's own token; it is
/// not answered with `ConnectStreamReady(None)`.
#[test]
fn the_first_start_survives_the_file_moving_on_after_the_save() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        let mut app = tests::fork_app(root.path(), true);
        let completion = save_at_open(&mut app).await;
        // Another Cube of the account signs in and moves the file on before
        // the completion reads it.
        write_fork_cache(
            root.path(),
            serde_json::json!({
                "active_email": EMAIL,
                "accounts": [{"email": EMAIL, "tokens": tokens("local-cube-token", "local-refresh")}]
            }),
        );
        let answered = outputs(completion).await;
        assert!(
            !answered
                .iter()
                .any(|m| matches!(m, Message::ConnectStreamReady(None))),
            "the first start must not fail on the shared file"
        );
        let Some(Message::TriggerConnectStreamReady { tokens, email, .. }) = answered
            .into_iter()
            .find(|m| matches!(m, Message::TriggerConnectStreamReady { .. }))
        else {
            panic!("the login completion triggers the stream");
        };
        assert_eq!(email, EMAIL);
        assert_eq!(tokens.read().await.access_token, CUBE_TOKEN);
        // The file keeps the other Cube's token; this Cube adopted none.
        assert_eq!(
            fork_entry(root.path()).unwrap().tokens.access_token,
            "local-cube-token"
        );
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// An id held only in memory goes with the session when Connect refuses the
/// device as revoked, as a stored one does. Only the Cube's own chain folder
/// is touched: the same email in `bitcoin/` may be an account on another
/// Connect host, so a Bitcoin Cube clears that id itself when its own stream
/// is refused.
#[test]
fn an_unsaved_device_id_is_dropped_on_a_revoked_device() {
    use crate::services::connect::grpc::trouble::Refusal;
    use crate::services::connect::grpc::ConnectStreamMessage as M;
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        let mut app = served_cube_with_stream(root.path(), "returned-device").await;
        assert!(fork_entry(root.path()).unwrap().device_id.is_none());
        assert_eq!(
            app.cache.connect_device_id.as_deref(),
            Some("returned-device")
        );
        for message in
            outputs(app.update(Message::ConnectStream(M::Refused(Refusal::DeviceRevoked)))).await
        {
            drop(app.update(message));
        }
        assert!(app.cache.connect_device_id.is_none());
        assert!(app.connect_stream_config.is_none());
        assert!(fork_entry(root.path()).unwrap().device_id.is_none());
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// After the stream started, the file moving on changes nothing: the stream
/// stays on the Cube's token and keeps running.
#[test]
fn a_running_stream_survives_the_file_moving_on() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        let mut app = served_cube_with_stream(root.path(), "returned-device").await;
        let owner = app.connect_auth.clone().unwrap();
        write_fork_cache(
            root.path(),
            serde_json::json!({
                "active_email": EMAIL,
                "accounts": [{"email": EMAIL, "tokens": tokens("local-cube-token", "local-refresh")}]
            }),
        );
        drop(app.update(Message::ConnectStream(
            crate::services::connect::grpc::ConnectStreamMessage::Connected,
        )));
        let cfg = app.connect_stream_config.as_ref().unwrap();
        assert!(Arc::ptr_eq(&cfg.tokens, &owner));
        assert_eq!(cfg.device_id, "returned-device");
        assert_eq!(app.cache.connect_stream_status, ConnectionStatus::Connected);
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// A managed-node Cube that was opened Connect-served (promoted while open),
/// its admitted panel on its dashboard.
fn promoted_cube(root: &std::path::Path) -> App {
    let mut app = tests::local_app(root);
    app.cache.datadir_path = CoincubeDirectory::new(root.to_path_buf());
    let mut client = crate::services::coincube::CoincubeClient::for_test(CUBE_BASE);
    client.set_token(CUBE_TOKEN);
    app.panels.connect.install_admitted_client(client.clone());
    app.panels.connect.account.user = Some(crate::services::coincube::User {
        id: 7,
        email: EMAIL.into(),
        email_verified: Some(true),
    });
    app.panels.connect.account.step = state::connect::account::ConnectFlowStep::Dashboard;
    app.fork_connect_client = Some(client);
    app.cache.connect_served = true;
    assert!(!app.requires_connect());
    app
}

/// However a promoted Cube loses its session (its own sign-out, Connect
/// refusing the device, or a sign-in or sign-out in another tab), it shows
/// the reopen notice, never a sign-in form: a sign-in inside a Cube opened
/// Connect-served stays refused (I3). The saved sign-in is not touched.
#[test]
fn a_promoted_cube_never_offers_an_in_cube_sign_in() {
    use crate::gui::tab::{AuthChange, State, Tab};
    use crate::services::connect::grpc::trouble::Refusal;
    use crate::services::connect::grpc::ConnectStreamMessage as M;
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        for way in [
            "log-out",
            "device-revoked",
            "sibling-sign-in",
            "sibling-log-out",
        ] {
            let root = tempfile::tempdir().unwrap();
            let before = seed_bitcoin(root.path(), CUBE_TOKEN);
            let mut app = promoted_cube(root.path());
            let app = match way {
                "log-out" => {
                    drop(app.update(Message::View(view::Message::ConnectAccount(
                        view::ConnectAccountMessage::LogOut,
                    ))));
                    app
                }
                "device-revoked" => {
                    for message in outputs(
                        app.update(Message::ConnectStream(M::Refused(Refusal::DeviceRevoked))),
                    )
                    .await
                    {
                        drop(app.update(message));
                    }
                    app
                }
                sibling => {
                    let change = if sibling == "sibling-sign-in" {
                        AuthChange::SignIn {
                            user_id: 8,
                            token: zeroize::Zeroizing::new("sibling-token".into()),
                        }
                    } else {
                        AuthChange::LogOut
                    };
                    let mut tab = Tab::new(1, State::App(app));
                    drop(tab.invalidate_fork_session(change, false));
                    let State::App(app) = tab.state else {
                        panic!("{}: a promoted Cube stays open", way);
                    };
                    app
                }
            };
            let account = &app.panels.connect.account;
            assert!(account.requires_authenticated_reopen(), "{}", way);
            assert!(account.authenticated_client().is_none(), "{}", way);
            // The ended session leaves the panel's memory too.
            assert!(account.client.token().is_none(), "{}", way);
            assert!(account.user.is_none(), "{}", way);
            assert!(app.fork_connect_client.is_none(), "{}", way);
            assert_bitcoin_untouched(root.path(), &before);
        }
    });
}

/// A Cube opened Connect-served keeps that kind when promoted to its managed
/// node: its admitted panel is not handed to local mode, and an ended session
/// costs it its Connect services only.
#[test]
fn a_promoted_cube_keeps_its_kind_and_loses_only_connect_services_at_session_end() {
    use crate::services::connect::grpc::ConnectStreamMessage as M;
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        let mut app = promoted_cube(root.path());

        // A panel message does not hand the admitted panel to local mode;
        // the panel reaching its dashboard saves the session, as when the
        // Cube was still on Connect.
        let routed = reach_dashboard(&mut app).await;
        assert!(app.panels.connect.account.authenticated_client().is_some());
        assert!(app.fork_connect_client.is_some());
        let saved = routed
            .into_iter()
            .find(|m| matches!(m, Message::ConnectServedSessionSaved { saved: true, .. }))
            .expect("the session is saved");
        let completion = outputs(app.update(saved)).await;
        if let Some(trigger) = completion
            .into_iter()
            .find(|m| matches!(m, Message::TriggerConnectStreamReady { .. }))
        {
            drop(app.update(trigger));
        }
        let auth = app.connect_auth.clone().unwrap();
        drop(app.update(Message::ConnectStreamReady(Some((
            stream_config(auth, CUBE_BASE, "returned-device"),
            EMAIL.into(),
        )))));
        assert!(app.connect_stream_config.is_some());

        drop(app.update(Message::ConnectStream(M::SessionEnded(
            "token expired".into(),
        ))));
        assert!(app.connect_stream_config.is_none());
        assert!(app.fork_connect_client.is_none());
        // No in-Cube sign-in until it is reopened: the reopen notice.
        assert!(app.panels.connect.account.requires_authenticated_reopen());
        // Its node is its own: the Vault stays on it, and the Cube stays open
        // on the local feature check, not on a Connect client.
        assert!(app.daemon.is_some());
        assert!(!app.requires_connect());
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// Two opens of one account, saved with an empty refresh token, differ only
/// in their access tokens: the login completion's ownership check tells them
/// apart (the device-id guard is covered in the cache's own tests).
#[test]
fn the_login_completion_tells_two_opens_with_an_empty_refresh_token_apart() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let (mut app, _) = super::shell_tests::bitcoin_app(root.path());
        app.cache.datadir_path = CoincubeDirectory::new(root.path().to_path_buf());
        let dir = app.cache.datadir_path.network_directory(app.cache.chain());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join(CONNECT_CACHE_FILENAME),
            serde_json::to_vec(&serde_json::json!({
                "active_email": EMAIL,
                "accounts": [{"email": EMAIL, "tokens": tokens("second-open", "")}]
            }))
            .unwrap(),
        )
        .unwrap();
        app.panels.connect.account.client.set_token("first-open");
        let epoch = app.panels.connect.account.auth_epoch();
        let answered = outputs(app.connect_login_completed(
            "first-open".into(),
            String::new(),
            EMAIL.into(),
            epoch,
        ))
        .await;
        assert!(matches!(&answered[..], [Message::ConnectStreamReady(None)]));
    });
}

/// With a Bitcoin Cube and a Connect-served Bitcoin Blake2b Cube open in two
/// tabs, an accepted sign-in or sign-out elsewhere closes the Blake2b tab to
/// Home as it does today, and leaves the Bitcoin tab's stream as it was: no
/// stream state crosses tabs.
#[test]
fn a_global_auth_change_closes_the_connect_served_tab_and_leaves_the_bitcoin_one() {
    use crate::gui::tab::{AuthChange, State, Tab};
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        for change in [
            AuthChange::LogOut,
            AuthChange::SignIn {
                user_id: 8,
                token: zeroize::Zeroizing::new("home-sign-in".into()),
            },
        ] {
            let root = tempfile::tempdir().unwrap();
            let before = seed_bitcoin(root.path(), CUBE_TOKEN);
            let fork = served_cube_with_stream(root.path(), "returned-device").await;
            assert!(fork.connect_stream_config.is_some());

            let bitcoin_root = tempfile::tempdir().unwrap();
            let (mut bitcoin, _) = super::shell_tests::bitcoin_app(bitcoin_root.path());
            let auth = shared("bitcoin-access", "bitcoin-refresh");
            bitcoin.connect_auth = Some(auth.clone());
            bitcoin.connect_email = Some(EMAIL.into());
            bitcoin.connect_stream_config =
                Some(stream_config(auth.clone(), CUBE_BASE, "bitcoin-device"));

            let mut fork_tab = Tab::new(1, State::App(fork));
            let mut bitcoin_tab = Tab::new(2, State::App(bitcoin));
            drop(fork_tab.invalidate_fork_session(change.clone(), false));
            drop(bitcoin_tab.invalidate_fork_session(change.clone(), false));

            assert!(matches!(fork_tab.state, State::Home(_)));
            let State::App(bitcoin) = &bitcoin_tab.state else {
                panic!("the Bitcoin tab closed");
            };
            let cfg = bitcoin.connect_stream_config.as_ref().unwrap();
            assert!(Arc::ptr_eq(&cfg.tokens, &auth));
            assert_eq!(cfg.device_id, "bitcoin-device");
            // The Blake2b tab's session stays saved in its own folder only.
            assert_eq!(
                fork_entry(root.path()).unwrap().tokens.access_token,
                CUBE_TOKEN
            );
            assert_bitcoin_untouched(root.path(), &before);
        }
    });
}

/// A remote duress lock delivered by a Connect-served Cube's stream records
/// the account that stream runs on, so the locked screen trusts only that
/// account's sessions to clear it. The Cube's session was saved from its
/// panel's user, so the panel names the account even when the saved keyring
/// sign-in is another account's; nothing falls back to the launch folder.
#[test]
fn a_remote_lock_from_a_connect_served_stream_names_the_cubes_account() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        seed_bitcoin(root.path(), CUBE_TOKEN);
        // The saved sign-in is now a different account's.
        let other = crate::services::coincube::LoginResponse {
            requires_2fa: false,
            token: "other-access".into(),
            refresh_token: "other-refresh".into(),
            user: crate::services::coincube::User {
                id: 5,
                email: "other@example.invalid".into(),
                email_verified: Some(true),
            },
        };
        state::connect::write_connect_secret(
            state::connect::CONNECT_KEYRING_USER,
            &serde_json::to_vec(&other).unwrap(),
        )
        .unwrap();
        let before = bitcoin_state(root.path());

        let mut app = served_cube_with_stream(root.path(), "cube-device").await;
        assert_eq!(app.connect_email.as_deref(), Some(EMAIL));
        assert_eq!(app.duress_activating_account().as_deref(), Some("7"));

        let answered = outputs(app.update(Message::ConnectStream(
            crate::services::connect::grpc::ConnectStreamMessage::DuressActivated {
                unlock_at: None,
                source: "remote".into(),
            },
        )))
        .await;
        assert!(answered
            .iter()
            .any(|m| matches!(m, Message::View(view::Message::DuressLockRemote))));

        let lock = crate::services::duress::DuressLocalState::load(root.path()).unwrap();
        assert!(lock.active);
        assert!(lock.remote_lock);
        assert_eq!(lock.locked_by_account_id.as_deref(), Some("7"));
        assert_eq!(lock.lock_account(), Some("7"));
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// A remote duress lock delivered on a Connect-served Cube's stream records
/// where its account lives, the Cube's Connect API and chain folder, so the
/// locked screen asks that host and reads that folder rather than the
/// configured API and `bitcoin/`. A Bitcoin Cube's lock records none.
#[test]
fn a_remote_lock_on_a_connect_served_cube_records_the_cubes_host_and_folder() {
    use crate::services::connect::grpc::ConnectStreamMessage as M;
    use crate::services::duress::{DuressLocalState, LockOrigin};
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        let mut app = served_cube_with_stream(root.path(), "returned-device").await;
        assert_eq!(
            app.duress_lock_origin(),
            Some(LockOrigin {
                api_base_url: CUBE_BASE.into(),
                chain: crate::chain::ChainId::BitcoinBlake2b,
            })
        );
        let lock = app.update(Message::ConnectStream(M::DuressActivated {
            unlock_at: None,
            source: "test".into(),
        }));
        drop(outputs(lock).await);
        let state = DuressLocalState::load(app.datadir.path()).unwrap();
        assert!(state.active);
        assert_eq!(state.lock_origin(), app.duress_lock_origin().as_ref());
        assert_bitcoin_untouched(root.path(), &before);

        let bitcoin_root = tempfile::tempdir().unwrap();
        let (bitcoin, _) = super::shell_tests::bitcoin_app(bitcoin_root.path());
        assert_eq!(bitcoin.duress_lock_origin(), None);
    });
}

/// A Connect-served Cube's sign-in saved its host's session on the account's
/// entry. A local Cube of the same account presents none of it to its own
/// Connect API: `EnsureConnectReady` registers neither the device nor the
/// Cube with that token and says no session was found.
#[test]
fn a_local_cube_never_presents_another_hosts_saved_session() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path(), CUBE_TOKEN);
        write_fork_cache(
            root.path(),
            serde_json::json!({
                "active_email": EMAIL,
                "accounts": [{"email": EMAIL, "tokens": tokens(CUBE_TOKEN, "cube-refresh"),
                    "device_id": "cube-host-device", "api_base_url": cube_issuer()}]
            }),
        );
        let mut app = tests::fork_app(root.path(), false);
        assert!(!app.cache.connect_served);
        assert!(app.fork_connect_client.is_none());
        assert_eq!(
            crate::services::connect::client::cache::issuing_api(
                &app.connect_base_url(),
                &crate::services::coincube_api_base_url(),
            ),
            None,
            "the local Cube talks to the configured API"
        );
        app.connect_email = Some(EMAIL.into());
        app.cube_settings.id = "local-cube".into();

        let answered = outputs(app.update(Message::EnsureConnectReady)).await;
        assert!(
            answered.iter().any(|m| matches!(
                m,
                Message::View(view::Message::ShowToast(_, text))
                    if text.starts_with("Couldn't find this Connect session")
            )),
            "the bootstrap stops at the session lookup"
        );
        assert!(
            answered.iter().any(|m| matches!(
                m,
                Message::View(view::Message::ConnectCube(
                    view::ConnectCubeMessage::CubeRegistered { result: Err(e), .. }
                )) if e.message.contains("no cached Connect session")
            )),
            "the Cube registration finds no session either"
        );
        assert!(!answered
            .iter()
            .any(|m| matches!(m, Message::TriggerConnectStreamReady { .. })));
        // The entry stays the other host's, untouched.
        let entry = fork_entry(root.path()).unwrap();
        assert_eq!(entry.tokens.access_token, CUBE_TOKEN);
        assert_eq!(entry.device_id.as_deref(), Some("cube-host-device"));
        assert_bitcoin_untouched(root.path(), &before);
    });
}
