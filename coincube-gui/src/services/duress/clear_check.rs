//! The locked screen's check that asks Connect whether duress has been
//! cleared (BTCB2 Connect signing master plan, decision 26).
//!
//! The locked screen offers no sign-in, so it can only ask with a session
//! already saved on this computer. Which sessions it may trust depends on
//! whether the lock records the account it belongs to
//! ([`DuressLocalState::account_id`], the Connect user id):
//!
//! * **An account is recorded.** Saved sessions are tried in this order: the
//!   one in the OS keyring, then every account entry (not only the active one)
//!   in every chain folder's `connect.json`. Only sessions of the recorded
//!   account are trusted. The keyring session carries its user id; a file
//!   entry stores an email and tokens, so its account is looked up with that
//!   session (`GET /api/v1/user`) before it is asked anything. A session the
//!   server refuses is skipped and the next one tried. A network failure stops
//!   the check. The first trusted answer decides: every trusted session belongs
//!   to the same account, so the check never goes on looking for a "cleared"
//!   answer once one has said "active".
//! * **No account is recorded.** The check stays exactly as it was before
//!   decision 26: the active account of the launch network's folder, and
//!   nothing else. Another account's "not active" must not clear the lock.

use std::collections::HashSet;

use coincube_core::miniscript::bitcoin::Network;

use crate::chain::ChainId;
use crate::dir::CoincubeDirectory;
use crate::services::coincube::{CoincubeClient, DuressCheckOutcome};
use crate::services::connect::client::cache::{Account, ConnectCache};

use super::DuressLocalState;

/// The saved sign-in from the OS keyring, as far as this check needs it.
#[derive(Clone)]
pub struct KeyringSession {
    /// The Connect user id the session was issued to.
    pub user_id: u32,
    pub access_token: String,
}

impl std::fmt::Debug for KeyringSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyringSession")
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

/// Whether duress is still active for the locked account: `Some(active)` from
/// a trusted session's answer, `None` when no trusted session could answer
/// (no session, every one refused, a network failure, an unreadable duress
/// state). The locked screen treats `None` as "still locked".
pub async fn duress_still_active(
    datadir: &CoincubeDirectory,
    launch_network: Option<Network>,
    keyring: Option<KeyringSession>,
    api_base_url: &str,
) -> Option<bool> {
    // Fail closed: a lock whose state can't be read can't say whose it is.
    let state = match DuressLocalState::load(datadir.path()) {
        Ok(state) => state,
        Err(e) => {
            log::error!("duress: reading duress state for the clear check failed: {e}");
            return None;
        }
    };
    match state.account_id {
        Some(account_id) => {
            recorded_account_check(datadir, &account_id, keyring, api_base_url).await
        }
        None => launch_folder_check(datadir, launch_network?, api_base_url).await,
    }
}

/// The check for a lock with no recorded account, unchanged from before
/// decision 26.
async fn launch_folder_check(
    datadir: &CoincubeDirectory,
    network: Network,
    api_base_url: &str,
) -> Option<bool> {
    let cache = ConnectCache::from_file(&datadir.network_directory(network)).ok()?;
    let account = cache.active_account()?;
    let client = client_for(api_base_url, &account.tokens.access_token);
    client.get_duress_state().await.ok().map(|s| s.active)
}

/// What one trusted session's question came back with.
enum Answer {
    /// The server's duress state for the account.
    Active(bool),
    /// The server refused this session: try the next.
    Refused,
    /// The server could not be reached or answered something unusable: stop.
    Unavailable,
}

impl Answer {
    fn from_err(e: &crate::services::coincube::CoincubeError) -> Self {
        match DuressCheckOutcome::from_err(e) {
            DuressCheckOutcome::Unauthorized => Self::Refused,
            _ => Self::Unavailable,
        }
    }
}

async fn recorded_account_check(
    datadir: &CoincubeDirectory,
    account_id: &str,
    keyring: Option<KeyringSession>,
    api_base_url: &str,
) -> Option<bool> {
    // One question per token: the keyring's session is usually also saved in
    // a folder, and the same account may be saved in several folders.
    let mut asked: HashSet<String> = HashSet::new();

    if let Some(session) = keyring {
        if session.user_id.to_string() == account_id {
            asked.insert(session.access_token.clone());
            let client = client_for(api_base_url, &session.access_token);
            match ask_duress_state(&client).await {
                Answer::Active(active) => return Some(active),
                Answer::Refused => {}
                Answer::Unavailable => return None,
            }
        }
    }

    for chain in ChainId::ALL {
        let Ok(cache) = ConnectCache::from_file(&datadir.network_directory(chain)) else {
            continue;
        };
        for account in accounts_active_first(&cache) {
            let token = &account.tokens.access_token;
            if token.is_empty() || !asked.insert(token.clone()) {
                continue;
            }
            let client = client_for(api_base_url, token);
            // The file stores no user id: look the session's account up with
            // the session itself before trusting anything it says.
            match client.get_user().await {
                Ok(user) if user.id.to_string() == account_id => {}
                Ok(_) => continue,
                Err(e) => match Answer::from_err(&e) {
                    Answer::Refused => continue,
                    _ => return None,
                },
            }
            match ask_duress_state(&client).await {
                Answer::Active(active) => return Some(active),
                Answer::Refused => continue,
                Answer::Unavailable => return None,
            }
        }
    }
    None
}

async fn ask_duress_state(client: &CoincubeClient) -> Answer {
    match client.get_duress_state().await {
        Ok(state) => Answer::Active(state.active),
        Err(e) => Answer::from_err(&e),
    }
}

/// The file's active account first, then its other entries in file order.
fn accounts_active_first(cache: &ConnectCache) -> Vec<&Account> {
    let active = cache.active_account();
    active
        .into_iter()
        .chain(
            cache
                .accounts
                .iter()
                .filter(move |a| active.is_none_or(|act| !std::ptr::eq(*a, act))),
        )
        .collect()
}

fn client_for(api_base_url: &str, token: &str) -> CoincubeClient {
    let mut client = CoincubeClient::with_base_url(api_base_url.to_string());
    client.set_token(token);
    client
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use httpmock::{Method::GET, Mock, MockServer};
    use serde_json::json;

    /// A data directory under the system temp dir, removed on drop. Every
    /// fixture here writes only below it.
    pub(crate) struct TempRoot(pub std::path::PathBuf);

    impl TempRoot {
        pub(crate) fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{label}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub(crate) fn dir(&self) -> CoincubeDirectory {
            CoincubeDirectory::new(self.0.clone())
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The crate directory's top-level entries, to show a test wrote nothing
    /// there.
    pub(crate) fn crate_dir_entries() -> std::collections::BTreeSet<std::ffi::OsString> {
        std::fs::read_dir(env!("CARGO_MANIFEST_DIR"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect()
    }

    /// A lock as the duress state records it.
    pub(crate) fn write_lock(root: &std::path::Path, account_id: Option<&str>) {
        let state = DuressLocalState {
            active: true,
            account_id: account_id.map(str::to_string),
            ..DuressLocalState::default()
        };
        state.save(root).unwrap();
    }

    /// `<root>/<chain dir>/connect.json` with these `(email, access token)`
    /// entries and this active email.
    pub(crate) fn write_connect_json(
        root: &std::path::Path,
        chain: ChainId,
        active: Option<&str>,
        accounts: &[(&str, &str)],
    ) {
        let dir = root.join(chain.dir_name());
        std::fs::create_dir_all(&dir).unwrap();
        let accounts: Vec<_> = accounts
            .iter()
            .map(|(email, token)| {
                json!({
                    "email": email,
                    "tokens": {"access_token": token, "expires_at": 0, "refresh_token": ""},
                })
            })
            .collect();
        std::fs::write(
            dir.join("connect.json"),
            serde_json::to_vec(&json!({"active_email": active, "accounts": accounts})).unwrap(),
        )
        .unwrap();
    }

    pub(crate) fn mock_user<'a>(server: &'a MockServer, token: &str, id: u32) -> Mock<'a> {
        server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/user")
                .header("authorization", format!("Bearer {token}"));
            then.status(200)
                .json_body(json!({"id": id, "email": format!("user{id}@example.com")}));
        })
    }

    pub(crate) fn mock_duress<'a>(server: &'a MockServer, token: &str, active: bool) -> Mock<'a> {
        server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/connect/duress")
                .header("authorization", format!("Bearer {token}"));
            then.status(200).json_body(json!({
                "success": true,
                "data": {"active": active, "enrolled": true, "thisDeviceRegistered": true},
            }));
        })
    }

    pub(crate) fn mock_status<'a>(
        server: &'a MockServer,
        path: &str,
        token: &str,
        status: u16,
    ) -> Mock<'a> {
        server.mock(|when, then| {
            when.method(GET)
                .path(path)
                .header("authorization", format!("Bearer {token}"));
            then.status(status);
        })
    }

    fn keyring(user_id: u32, token: &str) -> Option<KeyringSession> {
        Some(KeyringSession {
            user_id,
            access_token: token.to_string(),
        })
    }

    async fn check(
        root: &TempRoot,
        network: Option<Network>,
        keyring: Option<KeyringSession>,
        server: &MockServer,
    ) -> Option<bool> {
        duress_still_active(&root.dir(), network, keyring, &server.base_url()).await
    }

    #[tokio::test]
    async fn the_keyring_session_confirms_a_clear_on_a_btcb2_only_data_directory() {
        let before = crate_dir_entries();
        let root = TempRoot::new("clear-keyring-btcb2");
        write_lock(&root.0, Some("7"));
        std::fs::create_dir_all(root.0.join("bitcoin-blake2b")).unwrap();
        let server = MockServer::start();
        let asked = mock_duress(&server, "kr", false);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), keyring(7, "kr"), &server).await,
            Some(false)
        );
        asked.assert();
        assert_eq!(crate_dir_entries(), before);
    }

    #[tokio::test]
    async fn with_the_keyring_empty_the_btcb2_folder_confirms_a_clear() {
        let before = crate_dir_entries();
        let root = TempRoot::new("clear-file-btcb2");
        write_lock(&root.0, Some("7"));
        write_connect_json(
            &root.0,
            ChainId::BitcoinBlake2b,
            Some("a@example.com"),
            &[("a@example.com", "b2")],
        );
        let server = MockServer::start();
        let looked_up = mock_user(&server, "b2", 7);
        let asked = mock_duress(&server, "b2", false);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), None, &server).await,
            Some(false)
        );
        looked_up.assert();
        asked.assert();
        assert_eq!(crate_dir_entries(), before);
    }

    #[tokio::test]
    async fn a_btcb2_lock_clears_with_only_a_bitcoin_folder_session() {
        let root = TempRoot::new("clear-bitcoin-only");
        write_lock(&root.0, Some("7"));
        std::fs::create_dir_all(root.0.join("bitcoin-blake2b")).unwrap();
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            Some("a@example.com"),
            &[("a@example.com", "btc")],
        );
        let server = MockServer::start();
        mock_user(&server, "btc", 7);
        mock_duress(&server, "btc", false);
        // The launch network says nothing about which folder holds it.
        assert_eq!(
            check(&root, Some(Network::Testnet4), None, &server).await,
            Some(false)
        );
    }

    #[tokio::test]
    async fn another_accounts_keyring_session_is_passed_over_for_the_folders() {
        let root = TempRoot::new("clear-keyring-other");
        write_lock(&root.0, Some("7"));
        write_connect_json(
            &root.0,
            ChainId::BitcoinBlake2b,
            Some("a@example.com"),
            &[("a@example.com", "mine")],
        );
        let server = MockServer::start();
        let other = mock_duress(&server, "theirs", false);
        mock_user(&server, "mine", 7);
        let mine = mock_duress(&server, "mine", false);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), keyring(9, "theirs"), &server).await,
            Some(false)
        );
        other.assert_hits(0);
        mine.assert();
    }

    #[tokio::test]
    async fn a_different_accounts_saved_sign_in_alone_never_clears() {
        let root = TempRoot::new("clear-other-only");
        write_lock(&root.0, Some("7"));
        let server = MockServer::start();
        let other = mock_duress(&server, "theirs", false);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), keyring(9, "theirs"), &server).await,
            None
        );
        other.assert_hits(0);
    }

    #[tokio::test]
    async fn a_different_accounts_folder_session_alone_never_clears() {
        let root = TempRoot::new("clear-other-file");
        write_lock(&root.0, Some("7"));
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            Some("b@example.com"),
            &[("b@example.com", "theirs")],
        );
        let server = MockServer::start();
        mock_user(&server, "theirs", 9);
        let other = mock_duress(&server, "theirs", false);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), None, &server).await,
            None
        );
        other.assert_hits(0);
    }

    #[tokio::test]
    async fn a_refused_keyring_session_is_skipped_for_a_working_folder_one() {
        let root = TempRoot::new("clear-refused-keyring");
        write_lock(&root.0, Some("7"));
        write_connect_json(
            &root.0,
            ChainId::BitcoinBlake2b,
            Some("a@example.com"),
            &[("a@example.com", "fresh")],
        );
        let server = MockServer::start();
        let refused = mock_status(&server, "/api/v1/connect/duress", "expired", 401);
        mock_user(&server, "fresh", 7);
        let fresh = mock_duress(&server, "fresh", false);
        assert_eq!(
            check(
                &root,
                Some(Network::Bitcoin),
                keyring(7, "expired"),
                &server
            )
            .await,
            Some(false)
        );
        refused.assert();
        fresh.assert();
    }

    #[tokio::test]
    async fn a_session_refused_at_the_account_lookup_is_skipped() {
        let root = TempRoot::new("clear-refused-lookup");
        write_lock(&root.0, Some("7"));
        write_connect_json(&root.0, ChainId::Bitcoin, None, &[("a@example.com", "old")]);
        write_connect_json(
            &root.0,
            ChainId::BitcoinBlake2b,
            None,
            &[("a@example.com", "new")],
        );
        let server = MockServer::start();
        mock_status(&server, "/api/v1/user", "old", 403);
        mock_user(&server, "new", 7);
        mock_duress(&server, "new", false);
        assert_eq!(check(&root, None, None, &server).await, Some(false));
    }

    #[tokio::test]
    async fn an_entry_of_the_enrolling_account_that_is_not_the_active_one_is_tried() {
        let root = TempRoot::new("clear-inactive-entry");
        write_lock(&root.0, Some("7"));
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            Some("b@example.com"),
            &[("a@example.com", "mine"), ("b@example.com", "theirs")],
        );
        let server = MockServer::start();
        let looked_up_other = mock_user(&server, "theirs", 9);
        let other = mock_duress(&server, "theirs", false);
        mock_user(&server, "mine", 7);
        let mine = mock_duress(&server, "mine", false);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), None, &server).await,
            Some(false)
        );
        // The active entry is looked up first, and passed over.
        looked_up_other.assert();
        other.assert_hits(0);
        mine.assert();
    }

    #[tokio::test]
    async fn a_network_failure_stops_the_check_and_tries_nothing_further() {
        let root = TempRoot::new("clear-network-failure");
        write_lock(&root.0, Some("7"));
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            Some("a@example.com"),
            &[("a@example.com", "next")],
        );
        let server = MockServer::start();
        let down = mock_status(&server, "/api/v1/connect/duress", "kr", 503);
        let next_lookup = mock_user(&server, "next", 7);
        let next = mock_duress(&server, "next", false);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), keyring(7, "kr"), &server).await,
            None
        );
        down.assert();
        next_lookup.assert_hits(0);
        next.assert_hits(0);
    }

    #[tokio::test]
    async fn an_unreachable_server_stops_the_check() {
        let root = TempRoot::new("clear-unreachable");
        write_lock(&root.0, Some("7"));
        // Nothing listens on a port that was just released.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert_eq!(
            duress_still_active(
                &root.dir(),
                Some(Network::Bitcoin),
                keyring(7, "kr"),
                &format!("http://127.0.0.1:{port}"),
            )
            .await,
            None
        );
    }

    #[tokio::test]
    async fn a_trusted_active_answer_ends_the_check_locked() {
        let root = TempRoot::new("clear-active-first");
        write_lock(&root.0, Some("7"));
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            Some("a@example.com"),
            &[("a@example.com", "second")],
        );
        let server = MockServer::start();
        mock_duress(&server, "kr", true);
        let second_lookup = mock_user(&server, "second", 7);
        let second = mock_duress(&server, "second", false);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), keyring(7, "kr"), &server).await,
            Some(true)
        );
        second_lookup.assert_hits(0);
        second.assert_hits(0);
    }

    #[tokio::test]
    async fn without_a_recorded_account_another_accounts_not_active_never_clears() {
        let root = TempRoot::new("clear-legacy-other");
        write_lock(&root.0, None);
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            Some("a@example.com"),
            &[("a@example.com", "locked")],
        );
        write_connect_json(
            &root.0,
            ChainId::BitcoinBlake2b,
            Some("b@example.com"),
            &[("b@example.com", "other-file")],
        );
        let server = MockServer::start();
        mock_duress(&server, "locked", true);
        let other_keyring = mock_duress(&server, "other-keyring", false);
        let other_file = mock_duress(&server, "other-file", false);
        assert_eq!(
            check(
                &root,
                Some(Network::Bitcoin),
                keyring(9, "other-keyring"),
                &server
            )
            .await,
            Some(true)
        );
        other_keyring.assert_hits(0);
        other_file.assert_hits(0);
    }

    #[tokio::test]
    async fn without_a_recorded_account_only_the_launch_folders_active_account_is_asked() {
        let root = TempRoot::new("clear-legacy-only");
        write_lock(&root.0, None);
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            Some("a@example.com"),
            &[("a@example.com", "active"), ("b@example.com", "inactive")],
        );
        write_connect_json(
            &root.0,
            ChainId::BitcoinBlake2b,
            Some("a@example.com"),
            &[("a@example.com", "btcb2")],
        );
        let server = MockServer::start();
        let active = mock_duress(&server, "active", false);
        let inactive = mock_duress(&server, "inactive", false);
        let btcb2 = mock_duress(&server, "btcb2", false);
        let keyring_asked = mock_duress(&server, "kr", false);
        let lookups = server.mock(|when, then| {
            when.method(GET).path("/api/v1/user");
            then.status(200)
                .json_body(json!({"id": 7, "email": "a@example.com"}));
        });
        assert_eq!(
            check(&root, Some(Network::Bitcoin), keyring(7, "kr"), &server).await,
            Some(false)
        );
        active.assert();
        inactive.assert_hits(0);
        btcb2.assert_hits(0);
        keyring_asked.assert_hits(0);
        lookups.assert_hits(0);

        // As before: a refused session, or no launch network, is no answer,
        // and nothing else is tried.
        let server = MockServer::start();
        mock_status(&server, "/api/v1/connect/duress", "active", 401);
        let btcb2 = mock_duress(&server, "btcb2", false);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), keyring(7, "kr"), &server).await,
            None
        );
        btcb2.assert_hits(0);
        assert_eq!(check(&root, None, keyring(7, "kr"), &server).await, None);
    }

    #[tokio::test]
    async fn an_unreadable_duress_state_is_no_answer() {
        let root = TempRoot::new("clear-unreadable-state");
        std::fs::write(DuressLocalState::path(&root.0), b"{not json").unwrap();
        let server = MockServer::start();
        let asked = mock_duress(&server, "kr", false);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), keyring(7, "kr"), &server).await,
            None
        );
        asked.assert_hits(0);
    }
}
