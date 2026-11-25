//! A local Bitcoin Blake2b Cube (on its own node, or with no Vault yet) runs
//! the Bitcoin Connect path unchanged (Desktop plan PR 5): its sign-in
//! commits into its own chain folder, it restores that session at reopen,
//! and its tab follows the account as a Bitcoin tab does.
//!
//! Every test runs in its own temporary data directory and checks that what a
//! Bitcoin Cube owns on disk (`bitcoin/connect.json`,
//! `bitcoin/connect_transport_key`) is byte-identical afterwards, and that
//! nothing was written into the working directory. The saved sign-in in the
//! OS keyring is the account's, shared by every Cube: a sign-in inside a
//! local Cube saves it, exactly as one inside a Bitcoin Cube does.

use super::*;
use crate::services::connect::client::auth::AccessTokenResponse;
use crate::services::connect::client::cache::{ConnectCache, CONNECT_CACHE_FILENAME};
use crate::services::connect::grpc::stream::ConnectStreamConfig;
use iced::futures::StreamExt;

const EMAIL: &str = "local@example.invalid";
const TOKEN: &str = "local-sign-in-token";

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Every message a task produces, in order.
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

fn seed_bitcoin(root: &std::path::Path) -> BitcoinState {
    let dir = root.join("bitcoin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(CONNECT_CACHE_FILENAME),
        serde_json::to_vec(&serde_json::json!({
            "active_email": "bitcoin@example.invalid",
            "accounts": [{"email": "bitcoin@example.invalid", "tokens": {
                "access_token": "bitcoin-access", "refresh_token": "bitcoin-refresh", "expires_at": 2000
            }, "device_id": "bitcoin-device"}]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("connect_transport_key"), [9u8; 32]).unwrap();
    state::connect::write_connect_secret(state::connect::CONNECT_KEYRING_USER, b"saved-sign-in")
        .unwrap();
    bitcoin_state(root)
}

fn assert_bitcoin_untouched(root: &std::path::Path, before: &BitcoinState) {
    let now = bitcoin_state(root);
    assert_eq!(now.connect_json, before.connect_json);
    assert_eq!(now.transport_key, before.transport_key);
    for chain_dir in ["bitcoin", "bitcoin-blake2b"] {
        let stray = std::path::Path::new(chain_dir).join(CONNECT_CACHE_FILENAME);
        assert!(
            std::fs::read_to_string(&stray).map_or(true, |text| !text.contains(TOKEN)),
            "a session was written under the working directory: {}",
            stray.display()
        );
    }
}

fn login() -> crate::services::coincube::LoginResponse {
    crate::services::coincube::LoginResponse {
        requires_2fa: false,
        token: TOKEN.into(),
        refresh_token: "local-sign-in-refresh".into(),
        user: crate::services::coincube::User {
            id: 11,
            email: EMAIL.into(),
            email_verified: Some(true),
        },
    }
}

fn chain_dir(app: &App) -> crate::dir::NetworkDirectory {
    app.datadir.network_directory(app.cache.chain())
}

/// A Vault-less Bitcoin Blake2b Cube opened signed out.
fn vault_less(root: &std::path::Path) -> App {
    let chain = crate::chain::ChainId::BitcoinBlake2b;
    let signer = coincube_core::signer::MasterSigner::generate(chain.bitcoin_network()).unwrap();
    let cek = Arc::new(crate::services::connect::crypto::CubeEncryptionKey::derive(
        &signer,
        chain.bitcoin_network(),
    ));
    let (app, startup) = App::new_fork_without_wallet(
        cek,
        None,
        Config::new(false),
        CoincubeDirectory::new(root.to_path_buf()),
        settings::CubeSettings::new("Fork".into(), chain),
    )
    .unwrap();
    drop(startup);
    app
}

/// Commits a sign-in as the panel's deferred commit does: an OTP sign-in
/// (`refreshed == false`) or a keyring restore. Returns every message the
/// App answered with along the way.
async fn commit_sign_in(app: &mut App, refreshed: bool) -> Vec<Message> {
    let epoch = app.panels.connect.account.auth_epoch();
    app.panels.connect.account.begin_auth_commit_for_test();
    let previous_refresh_token = if refreshed {
        app.in_app_restore_cache_snapshot = Some(RestoreAuthSnapshot {
            cache: ConnectCache::from_file(&chain_dir(app)).unwrap_or_default(),
            email: app.connect_email.clone(),
            auth: None,
        });
        Some("saved-refresh".to_string())
    } else {
        app.in_app_otp_cache_snapshot =
            Some(ConnectCache::from_file(&chain_dir(app)).unwrap_or_default());
        None
    };
    let mut seen = Vec::new();
    let mut next = vec![Message::InAppConnectAuthReady {
        login: login(),
        epoch,
        previous_refresh_token,
    }];
    while let Some(message) = next.pop() {
        for answer in outputs(app.update(message)).await {
            let feed = matches!(
                &answer,
                Message::InAppConnectAuthPersisted { .. }
                    | Message::View(view::Message::ConnectAccount(
                        view::ConnectAccountMessage::AuthCommitAccepted { .. }
                    ))
            );
            if feed {
                next.push(answer);
            } else {
                seen.push(answer);
            }
        }
    }
    seen
}

fn assert_no_refusal(answers: &[Message]) {
    assert!(!answers.iter().any(|m| matches!(
        m,
        Message::View(view::Message::ShowError(_))
            | Message::View(view::Message::ConnectAccount(
                view::ConnectAccountMessage::AuthCommitRejected { .. }
            ))
    )));
}

/// On a local-node Cube and on a Vault-less Cube, an OTP sign-in and a keyring
/// restore each commit through the App with no refusal (driven from the
/// panel's deferred commit, `InAppConnectAuthReady`; the panel's own OTP and
/// refresh requests need the network and are not run): the session lands in
/// `bitcoin-blake2b/connect.json`, the App installs it, and the login
/// completion that registers the signer device is asked for. Nothing under
/// `bitcoin/` changes.
#[test]
fn a_local_cube_signs_in_through_the_bitcoin_path_into_its_own_folder() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        for (kind, refreshed) in [
            ("local-node", false),
            ("local-node", true),
            ("vault-less", false),
            ("vault-less", true),
        ] {
            let root = tempfile::tempdir().unwrap();
            let before = seed_bitcoin(root.path());
            let mut app = if kind == "local-node" {
                tests::local_app(root.path())
            } else {
                vault_less(root.path())
            };
            assert!(app.is_local_fork(), "{}", kind);
            let answers = commit_sign_in(&mut app, refreshed).await;
            assert_no_refusal(&answers);
            // The login completion that registers the signer device and
            // starts the stream is asked for, as on Bitcoin. (An OTP sign-in
            // asks for it from the panel; a refresh from the App's commit.)
            assert!(
                answers
                    .iter()
                    .any(|m| matches!(m, Message::InAppConnectLoginCompleted { email, .. } if email == EMAIL)),
                "{} {}",
                kind,
                refreshed
            );
            let file = ConnectCache::from_file(&chain_dir(&app)).unwrap();
            let active = file.active_account().unwrap();
            assert_eq!(active.email, EMAIL, "{} {}", kind, refreshed);
            assert_eq!(active.tokens.access_token, TOKEN, "{} {}", kind, refreshed);
            assert!(chain_dir(&app).path().ends_with("bitcoin-blake2b"));
            let auth = app.connect_auth.clone().expect("the commit is installed");
            assert_eq!(auth.read().await.access_token, TOKEN);
            assert_eq!(app.connect_email.as_deref(), Some(EMAIL));
            assert_bitcoin_untouched(root.path(), &before);
            // The account's saved sign-in, as a Bitcoin Cube's sign-in saves it.
            assert!(
                String::from_utf8(bitcoin_state(root.path()).keyring.unwrap())
                    .unwrap()
                    .contains(TOKEN)
            );
        }
    });
}

/// A local-node Cube with a saved session opens signed in, as a Bitcoin Cube
/// with a Vault does, and reaches `ConnectStreamReady` on it.
#[test]
fn a_local_node_cube_restores_its_saved_session_at_open() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path());
        let datadir = CoincubeDirectory::new(root.path().to_path_buf());
        let dir = datadir.network_directory(crate::chain::ChainId::BitcoinBlake2b);
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join(CONNECT_CACHE_FILENAME),
            serde_json::to_vec(&serde_json::json!({
                "active_email": EMAIL,
                "accounts": [{"email": EMAIL, "tokens": {
                    "access_token": TOKEN, "refresh_token": "r", "expires_at": 2000
                }, "device_id": "local-device", "last_seen_event_seq": 5}]
            }))
            .unwrap(),
        )
        .unwrap();
        let restored =
            crate::gui::tab::saved_connect_session(&datadir, crate::chain::ChainId::BitcoinBlake2b);
        assert!(restored.is_some());
        let mut app = tests::local_app_with(root.path(), restored);
        let auth = app.connect_auth.clone().expect("restored at open");
        assert!(app.cache.has_connect_session);
        assert!(Arc::ptr_eq(
            app.cache.connect_tokens.as_ref().unwrap(),
            &auth
        ));
        drop(app.update(Message::ConnectStreamReady(Some((
            ConnectStreamConfig {
                base_url: app.connect_base_url(),
                grpc_url: "https://grpc.connect.example".into(),
                tokens: auth.clone(),
                device_id: "stale".into(),
                user_agent: "test".into(),
                vault_ids: vec![],
                last_seen_seq: 0,
            },
            EMAIL.into(),
        )))));
        let cfg = app
            .connect_stream_config
            .as_ref()
            .expect("the stream mounts");
        assert_eq!(cfg.device_id, "local-device");
        assert_eq!(cfg.last_seen_seq, 5);
        assert_bitcoin_untouched(root.path(), &before);
    });
}

/// The same sign-in, sign-out and reopen on a Bitcoin Cube and on a
/// local-node Bitcoin Blake2b Cube give the same Connect state, each in its
/// own chain folder: sign-out ends the stream and keeps the saved session,
/// and the reopen restores it.
#[test]
fn bitcoin_and_a_local_blake2b_cube_follow_the_same_sign_in_sequence() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let mut states = Vec::new();
        for kind in ["bitcoin", "local-blake2b"] {
            let root = tempfile::tempdir().unwrap();
            let mut app = if kind == "bitcoin" {
                let (mut app, _) = super::shell_tests::bitcoin_app(root.path());
                app.cache.datadir_path = CoincubeDirectory::new(root.path().to_path_buf());
                app
            } else {
                tests::local_app(root.path())
            };
            let answers = commit_sign_in(&mut app, false).await;
            assert_no_refusal(&answers);
            let signed_in = (
                app.connect_email.clone(),
                app.cache.has_connect_session,
                answers
                    .iter()
                    .any(|m| matches!(m, Message::InAppConnectLoginCompleted { .. })),
            );
            drop(app.update(Message::View(view::Message::ConnectAccount(
                view::ConnectAccountMessage::LogOut,
            ))));
            let file = ConnectCache::from_file(&chain_dir(&app)).unwrap();
            let kept = file.active_account().map(|a| a.tokens.access_token.clone());
            let reopened = crate::gui::tab::saved_connect_session(&app.datadir, app.cache.chain())
                .map(|(_, email)| email);
            states.push((
                signed_in,
                app.connect_stream_config.is_none(),
                app.cache.connect_email.clone(),
                kept,
                reopened,
            ));
        }
        assert_eq!(states[0], states[1]);
        assert_eq!(states[0].3.as_deref(), Some(TOKEN));
    });
}

/// A sign-in or sign-out in another tab neither closes an open local Cube
/// nor signs it out, as on a Bitcoin tab, and its fork task generation is not
/// bumped (that bump is what aborts a fork tab's in-flight work). A
/// Connect-served tab still closes (covered in `connect_served_tests`).
#[test]
fn a_global_auth_change_leaves_a_local_tab_open_and_signed_in() {
    use crate::gui::tab::{AuthChange, State, Tab};
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        for change in [
            AuthChange::LogOut,
            AuthChange::SignIn {
                user_id: 99,
                token: zeroize::Zeroizing::new("sibling".into()),
            },
        ] {
            let root = tempfile::tempdir().unwrap();
            let before = seed_bitcoin(root.path());
            let mut app = tests::local_app(root.path());
            drop(commit_sign_in(&mut app, false).await);
            let auth = app.connect_auth.clone().unwrap();
            let mut tab = Tab::new(1, State::App(app));
            assert!(tab.accepts_fork_generation(0));
            drop(tab.invalidate_fork_session(change, false));
            let State::App(app) = &tab.state else {
                panic!("a local tab stays open");
            };
            assert!(Arc::ptr_eq(app.connect_auth.as_ref().unwrap(), &auth));
            assert_eq!(app.connect_email.as_deref(), Some(EMAIL));
            // Its in-flight work was not cancelled.
            assert!(tab.accepts_fork_generation(0));
            assert_bitcoin_untouched(root.path(), &before);
        }
    });
}

/// Two Cubes of one account share a signer device, and the server keeps one
/// stream per device: the newer supersedes the older, whose stream ends
/// without telling the App (`connect_stream`, `StreamAttemptOutcome::
/// Superseded`). On a Bitcoin Cube nothing restarts it: a later bootstrap
/// installs the same device id and gRPC address, which is the stream
/// subscription's identity, so the ended subscription is not replaced until
/// the Cube is reopened or signs out and in. A local Bitcoin Blake2b Cube
/// does exactly the same.
#[test]
fn a_superseded_stream_is_not_restarted_on_either_chain() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let mut identities = Vec::new();
        for kind in ["bitcoin", "local-blake2b"] {
            let root = tempfile::tempdir().unwrap();
            let mut app = if kind == "bitcoin" {
                let (mut app, _) = super::shell_tests::bitcoin_app(root.path());
                app.cache.datadir_path = CoincubeDirectory::new(root.path().to_path_buf());
                app
            } else {
                tests::local_app(root.path())
            };
            drop(commit_sign_in(&mut app, false).await);
            crate::services::connect::client::cache::set_device_id_for_email(
                &chain_dir(&app),
                EMAIL,
                Some("shared-device"),
            )
            .await
            .unwrap();
            let auth = app.connect_auth.clone().unwrap();
            let ready = |auth: Arc<tokio::sync::RwLock<AccessTokenResponse>>| {
                Message::ConnectStreamReady(Some((
                    ConnectStreamConfig {
                        base_url: "https://connect.example".into(),
                        grpc_url: "https://grpc.connect.example".into(),
                        tokens: auth,
                        device_id: "shared-device".into(),
                        user_agent: "test".into(),
                        vault_ids: vec![],
                        last_seen_seq: 0,
                    },
                    EMAIL.into(),
                )))
            };
            drop(app.update(ready(auth.clone())));
            drop(app.update(Message::ConnectStream(
                crate::services::connect::grpc::ConnectStreamMessage::Connected,
            )));
            let identity = |app: &App| {
                app.connect_stream_config
                    .as_ref()
                    .map(|cfg| (cfg.device_id.clone(), cfg.grpc_url.clone()))
                    .zip(Some(app.connect_stream_restart))
            };
            let before = identity(&app);
            // The superseded stream says nothing to the App. A later
            // bootstrap lands on the same identity, so nothing restarts
            // the stream; the status it leaves reads "Connecting…" on both
            // chains.
            drop(app.update(ready(auth)));
            assert_eq!(identity(&app), before, "{}", kind);
            identities.push((before.is_some(), app.cache.connect_stream_status));
        }
        assert_eq!(identities[0], identities[1]);
    });
}

/// A Vault-less Cube still on the session it was opened with keeps it when
/// another tab signs in or out, as an open Bitcoin Cube keeps its session
/// (the plan's per-event list: the tab's local arm no longer runs for it). Its
/// own sign-out ends it, and the panel then signs in through the Bitcoin
/// path. A promoted Connect-served Cube in the same call still loses its
/// Connect services and keeps the reopen notice.
#[test]
fn another_tabs_auth_change_leaves_an_opening_session_and_revokes_a_promoted_one() {
    use crate::gui::tab::{AuthChange, State, Tab};
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path());
        let chain = crate::chain::ChainId::BitcoinBlake2b;
        let signer =
            coincube_core::signer::MasterSigner::generate(chain.bitcoin_network()).unwrap();
        let cek = Arc::new(crate::services::connect::crypto::CubeEncryptionKey::derive(
            &signer,
            chain.bitcoin_network(),
        ));
        let mut client = crate::services::coincube::CoincubeClient::for_test("http://127.0.0.1:9");
        client.set_token("opening-token");
        let (app, startup) = App::new_fork_without_wallet(
            cek,
            Some(client),
            Config::new(false),
            CoincubeDirectory::new(root.path().to_path_buf()),
            settings::CubeSettings::new("Fork".into(), chain),
        )
        .unwrap();
        drop(startup);
        assert!(app.is_local_fork());
        let mut tab = Tab::new(1, State::App(app));
        drop(tab.invalidate_fork_session(
            AuthChange::SignIn {
                user_id: 99,
                token: zeroize::Zeroizing::new("home-sign-in".into()),
            },
            false,
        ));
        let State::App(app) = &mut tab.state else {
            panic!("a local tab stays open");
        };
        assert_eq!(
            app.fork_connect_client.as_ref().and_then(|c| c.token()),
            Some("opening-token")
        );
        drop(app.update(Message::View(view::Message::ConnectAccount(
            view::ConnectAccountMessage::LogOut,
        ))));
        assert!(app.fork_connect_client.is_none());
        assert!(!app.panels.connect.account.requires_authenticated_reopen());
        assert_bitcoin_untouched(root.path(), &before);

        // A promoted Connect-served Cube, through the same tab call.
        let promoted_root = tempfile::tempdir().unwrap();
        let mut promoted = tests::local_app(promoted_root.path());
        let mut client = crate::services::coincube::CoincubeClient::for_test("http://127.0.0.1:9");
        client.set_token("served-token");
        promoted
            .panels
            .connect
            .install_admitted_client(client.clone());
        promoted.fork_connect_client = Some(client);
        promoted.cache.connect_served = true;
        let mut tab = Tab::new(2, State::App(promoted));
        drop(tab.invalidate_fork_session(AuthChange::LogOut, false));
        let State::App(promoted) = &tab.state else {
            panic!("a promoted tab stays open on its node");
        };
        assert!(promoted.fork_connect_client.is_none());
        assert!(promoted
            .panels
            .connect
            .account
            .requires_authenticated_reopen());
    });
}

/// A local Cube on its opening session saves it only as a whole session:
/// when the saved sign-in is another session, it writes nothing rather than
/// an access token with no refresh token that later Cubes would restore.
#[test]
fn a_local_cube_saves_its_opening_session_only_with_its_refresh_token() {
    let _guard = crate::app::session::test_guard();
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let before = seed_bitcoin(root.path());
        let chain = crate::chain::ChainId::BitcoinBlake2b;
        let signer =
            coincube_core::signer::MasterSigner::generate(chain.bitcoin_network()).unwrap();
        let cek = Arc::new(crate::services::connect::crypto::CubeEncryptionKey::derive(
            &signer,
            chain.bitcoin_network(),
        ));
        let mut client = crate::services::coincube::CoincubeClient::for_test("http://127.0.0.1:9");
        client.set_token("opening-token");
        let (mut app, startup) = App::new_fork_without_wallet(
            cek,
            Some(client),
            Config::new(false),
            CoincubeDirectory::new(root.path().to_path_buf()),
            settings::CubeSettings::new("Fork".into(), chain),
        )
        .unwrap();
        drop(startup);
        app.panels.connect.account.user = Some(crate::services::coincube::User {
            id: 11,
            email: EMAIL.into(),
            email_verified: Some(true),
        });
        app.panels.connect.account.step = state::connect::account::ConnectFlowStep::Dashboard;
        // The keyring holds another session (seed_bitcoin's), not this one.
        assert!(iced_runtime::task::into_stream(app.save_served_session()).is_none());
        assert_eq!(app.served_session, ServedSession::NotSaved);
        assert!(!chain_dir(&app).path().join(CONNECT_CACHE_FILENAME).exists());
        assert_bitcoin_untouched(root.path(), &before);
    });
}
