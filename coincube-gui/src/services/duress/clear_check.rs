//! The locked screen's check that asks Connect whether duress has been
//! cleared (BTCB2 Connect signing master plan, decision 26).
//!
//! The locked screen offers no sign-in, so it can only ask with a session
//! already saved on this computer. Which sessions it may trust depends on
//! whether the lock records the account it belongs to
//! ([`DuressLocalState::lock_account`], a Connect user id: the account whose
//! stream delivered a remote lock, else the enrolling account):
//!
//! * **An account is recorded.** Saved sessions are tried in this order: the
//!   one in the OS keyring, then every account entry (not only the active one)
//!   in every chain folder's `connect.json`. Only sessions of the recorded
//!   account are trusted. The keyring session carries its user id; a file
//!   entry stores an email and tokens, so its account is looked up with that
//!   session (`GET /api/v1/user`) before it is asked anything. A session the
//!   server refuses is skipped and the next one tried: 401/403, and at the
//!   lookup any other definitive 4xx (a deleted account's 404). A network
//!   failure, a rate limit, a 5xx or an unusable answer stops the check. The first trusted answer decides: every trusted session belongs
//!   to the same account, so the check never goes on looking for a "cleared"
//!   answer once one has said "active".
//! * **No account is recorded.** The check stays exactly as it was before
//!   decision 26: the active account of the launch network's folder, and
//!   nothing else. Another account's "not active" must not clear the lock.
//!
//! A remote lock raised from a Connect-served Cube also records where its
//! account lives ([`DuressLocalState::lock_origin`]): that Cube's Connect API
//! and chain folder. The check then asks that API instead of the configured
//! one, since a user id means nothing on another host, and for a lock with no
//! recorded account reads that chain's folder instead of the launch
//! network's (a Bitcoin Blake2b Cube's session is never in `bitcoin/`). The
//! keyring's session was issued by the configured API, so it is not asked
//! on another host.
//!
//! A saved session is only ever sent to the host that issued it: each
//! `connect.json` entry records its host ([`Account::api_base_url`], none for
//! the configured API), and the check passes over every entry another host
//! issued, whichever API it is asking.

use std::collections::HashSet;

use coincube_core::miniscript::bitcoin::Network;

use crate::chain::ChainId;
use crate::dir::CoincubeDirectory;
use crate::services::coincube::{CoincubeClient, DuressCheckOutcome};
use crate::services::connect::client::cache::{issuing_api, Account, ConnectCache};

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
    let origin = state.lock_origin();
    let configured_api_base_url = api_base_url;
    let (api_base_url, keyring) = match origin {
        Some(origin) if origin.api_base_url != api_base_url => (origin.api_base_url.as_str(), None),
        _ => (api_base_url, keyring),
    };
    // Only entries this host issued are sent to it.
    let issuer = issuing_api(api_base_url, configured_api_base_url);
    let issuer = issuer.as_deref();
    match state.lock_account() {
        Some(account_id) => {
            recorded_account_check(datadir, account_id, keyring, api_base_url, issuer).await
        }
        None => {
            let chain = match origin {
                Some(origin) => origin.chain,
                None => ChainId::from(launch_network?),
            };
            launch_folder_check(datadir, chain, api_base_url, issuer).await
        }
    }
}

/// The check for a lock with no recorded account, unchanged from before
/// decision 26.
async fn launch_folder_check(
    datadir: &CoincubeDirectory,
    chain: ChainId,
    api_base_url: &str,
    issuer: Option<&str>,
) -> Option<bool> {
    let cache = ConnectCache::from_file(&datadir.network_directory(chain)).ok()?;
    let account = cache
        .active_account()
        .filter(|account| account.issued_by(issuer))?;
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
    /// A failed duress query: 401/403 refuse the session, anything else stops.
    fn from_err(e: &crate::services::coincube::CoincubeError) -> Self {
        match DuressCheckOutcome::from_err(e) {
            DuressCheckOutcome::Unauthorized => Self::Refused,
            _ => Self::Unavailable,
        }
    }

    /// A failed account lookup: any definitive client error (a 4xx that is
    /// not 408 or 429, such as the 404 of a deleted account) refuses the
    /// session; network failures, rate limits, 5xx and unusable answers stop.
    fn from_lookup_err(e: &crate::services::coincube::CoincubeError) -> Self {
        use crate::services::coincube::CoincubeError;
        let client_error = match e {
            CoincubeError::Unsuccessful(info) => (400..500).contains(&info.status_code),
            CoincubeError::NotFound => true,
            _ => false,
        };
        if e.is_auth_error() || (client_error && !e.is_transient()) {
            Self::Refused
        } else {
            Self::Unavailable
        }
    }
}

async fn recorded_account_check(
    datadir: &CoincubeDirectory,
    account_id: &str,
    keyring: Option<KeyringSession>,
    api_base_url: &str,
    issuer: Option<&str>,
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
            // Another host's session is never presented to this one.
            if !account.issued_by(issuer) {
                continue;
            }
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
                Err(e) => match Answer::from_lookup_err(&e) {
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
        write_connect_json_issued_by(root, chain, active, accounts, None)
    }

    /// As [`write_connect_json`], every entry issued by `issuer`
    /// ([`Account::api_base_url`]; `None`: the configured API).
    pub(crate) fn write_connect_json_issued_by(
        root: &std::path::Path,
        chain: ChainId,
        active: Option<&str>,
        accounts: &[(&str, &str)],
        issuer: Option<&str>,
    ) {
        let dir = root.join(chain.dir_name());
        std::fs::create_dir_all(&dir).unwrap();
        let accounts: Vec<_> = accounts
            .iter()
            .map(|(email, token)| {
                json!({
                    "email": email,
                    "tokens": {"access_token": token, "expires_at": 0, "refresh_token": ""},
                    "api_base_url": issuer,
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
    async fn a_deleted_accounts_entry_is_skipped_but_a_rate_limit_stops() {
        let root = TempRoot::new("clear-lookup-404");
        write_lock(&root.0, Some("7"));
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            None,
            &[("gone@example.com", "gone"), ("a@example.com", "mine")],
        );
        let server = MockServer::start();
        let gone = mock_status(&server, "/api/v1/user", "gone", 404);
        mock_user(&server, "mine", 7);
        mock_duress(&server, "mine", false);
        assert_eq!(check(&root, None, None, &server).await, Some(false));
        gone.assert();

        let server = MockServer::start();
        mock_status(&server, "/api/v1/user", "gone", 429);
        let mine = mock_user(&server, "mine", 7);
        assert_eq!(check(&root, None, None, &server).await, None);
        mine.assert_hits(0);
    }

    #[tokio::test]
    async fn a_token_saved_in_the_keyring_and_a_folder_is_asked_once() {
        let root = TempRoot::new("clear-dedupe");
        write_lock(&root.0, Some("7"));
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            Some("a@example.com"),
            &[("a@example.com", "shared")],
        );
        write_connect_json(
            &root.0,
            ChainId::BitcoinBlake2b,
            Some("a@example.com"),
            &[("a@example.com", "shared")],
        );
        let server = MockServer::start();
        let refused = mock_status(&server, "/api/v1/connect/duress", "shared", 401);
        let lookups = mock_user(&server, "shared", 7);
        assert_eq!(
            check(&root, None, keyring(7, "shared"), &server).await,
            None
        );
        refused.assert_hits(1);
        lookups.assert_hits(0);
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

    /// A remote lock raised by account 9 on a computer enrolled as account 7.
    fn remote_lock_by_9_enrolled_as_7(root: &TempRoot) {
        write_lock(&root.0, Some("7"));
        let mut state = DuressLocalState::load(&root.0).unwrap();
        state.record_remote_lock_account(Some("9".into()), None);
        state.save(&root.0).unwrap();
    }

    #[tokio::test]
    async fn a_remote_lock_by_another_account_is_not_cleared_by_the_enrolling_accounts_session() {
        let root = TempRoot::new("clear-locked-by-other");
        remote_lock_by_9_enrolled_as_7(&root);
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            Some("a@example.com"),
            &[("a@example.com", "enrolling")],
        );
        let server = MockServer::start();
        mock_user(&server, "enrolling", 7);
        let enrolling = mock_duress(&server, "enrolling", false);
        let keyring_enrolling = mock_duress(&server, "kr-enrolling", false);
        assert_eq!(
            check(
                &root,
                Some(Network::Bitcoin),
                keyring(7, "kr-enrolling"),
                &server
            )
            .await,
            None
        );
        enrolling.assert_hits(0);
        keyring_enrolling.assert_hits(0);
    }

    #[tokio::test]
    async fn a_remote_lock_is_confirmed_by_the_account_that_raised_it() {
        for (active, expected) in [(false, Some(false)), (true, Some(true))] {
            let root = TempRoot::new("clear-locked-by-raiser");
            remote_lock_by_9_enrolled_as_7(&root);
            write_connect_json(
                &root.0,
                ChainId::BitcoinBlake2b,
                Some("a@example.com"),
                &[("a@example.com", "enrolling"), ("b@example.com", "raiser")],
            );
            let server = MockServer::start();
            mock_user(&server, "enrolling", 7);
            let enrolling = mock_duress(&server, "enrolling", false);
            mock_user(&server, "raiser", 9);
            mock_duress(&server, "raiser", active);
            assert_eq!(
                check(&root, Some(Network::Bitcoin), keyring(7, "kr"), &server).await,
                expected
            );
            enrolling.assert_hits(0);
        }
    }

    #[tokio::test]
    async fn a_remote_lock_whose_account_is_unknown_is_not_judged_by_the_enrolment() {
        let root = TempRoot::new("clear-remote-unknown");
        write_lock(&root.0, Some("7"));
        let mut state = DuressLocalState::load(&root.0).unwrap();
        state.record_remote_lock_account(None, None);
        state.save(&root.0).unwrap();
        assert_eq!(state.lock_account(), None);
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            Some("b@example.com"),
            &[("b@example.com", "launch"), ("a@example.com", "enrolling")],
        );
        let server = MockServer::start();
        let launch = mock_duress(&server, "launch", true);
        let enrolling = mock_duress(&server, "enrolling", false);
        let keyring_enrolling = mock_duress(&server, "kr", false);
        // The check for a lock with no recorded account: the launch folder's
        // active account only.
        assert_eq!(
            check(&root, Some(Network::Bitcoin), keyring(7, "kr"), &server).await,
            Some(true)
        );
        launch.assert();
        enrolling.assert_hits(0);
        keyring_enrolling.assert_hits(0);
    }

    /// A remote lock raised from a Connect-served Bitcoin Blake2b Cube whose
    /// Connect API is `cube_host`, by `account` (unknown when `None`).
    fn remote_lock_from_served_cube(
        root: &TempRoot,
        account: Option<&str>,
        cube_host: &MockServer,
    ) {
        write_lock(&root.0, Some("7"));
        let mut state = DuressLocalState::load(&root.0).unwrap();
        state.record_remote_lock_account(
            account.map(str::to_string),
            Some(super::super::LockOrigin {
                api_base_url: cube_host.base_url(),
                chain: ChainId::BitcoinBlake2b,
            }),
        );
        state.save(&root.0).unwrap();
    }

    #[tokio::test]
    async fn a_served_cubes_lock_is_asked_of_the_cubes_host_not_the_configured_one() {
        let root = TempRoot::new("clear-served-host");
        let cube_host = MockServer::start();
        remote_lock_from_served_cube(&root, Some("9"), &cube_host);
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            Some("other@example.com"),
            &[("other@example.com", "configured")],
        );
        let cube_issuer = issuing_api(&cube_host.base_url(), "https://configured.invalid");
        write_connect_json_issued_by(
            &root.0,
            ChainId::BitcoinBlake2b,
            Some("cube@example.com"),
            &[("cube@example.com", "cube")],
            cube_issuer.as_deref(),
        );
        // On the configured API, user 9 is a different account that is not
        // in duress, and the keyring's sign-in is user 9 there too.
        let configured = MockServer::start();
        let configured_user = mock_user(&configured, "configured", 9);
        let configured_clear = mock_duress(&configured, "configured", false);
        let keyring_clear = mock_duress(&configured, "kr", false);
        // The configured API's token is never presented to the Cube's host.
        let configured_on_cube_host = mock_user(&cube_host, "configured", 9);
        let cube_keyring = mock_duress(&cube_host, "kr", false);
        mock_user(&cube_host, "cube", 9);
        let cube_active = mock_duress(&cube_host, "cube", true);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), keyring(9, "kr"), &configured).await,
            Some(true)
        );
        cube_active.assert();
        cube_keyring.assert_hits(0);
        configured_on_cube_host.assert_hits(0);
        configured_user.assert_hits(0);
        configured_clear.assert_hits(0);
        keyring_clear.assert_hits(0);
    }

    /// In the Cube's own folder, an entry the configured API issued (a local
    /// Cube's session of the same account) is never presented to the Cube's
    /// host either: the folder alone does not say which host issued a token.
    #[tokio::test]
    async fn a_served_cubes_lock_never_sends_the_configured_apis_entries_to_its_host() {
        let root = TempRoot::new("clear-served-same-folder");
        let cube_host = MockServer::start();
        remote_lock_from_served_cube(&root, Some("9"), &cube_host);
        write_connect_json(
            &root.0,
            ChainId::BitcoinBlake2b,
            Some("cube@example.com"),
            &[("cube@example.com", "configured-local")],
        );
        let configured = MockServer::start();
        let sent_to_cube_host = mock_user(&cube_host, "configured-local", 9);
        let cube_clear = mock_duress(&cube_host, "configured-local", false);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), None, &configured).await,
            None,
            "no session of the Cube's host: the lock stays"
        );
        sent_to_cube_host.assert_hits(0);
        cube_clear.assert_hits(0);
    }

    /// A lock asked of the configured API never presents a session another
    /// host issued, wherever it is saved.
    #[tokio::test]
    async fn the_configured_api_is_never_sent_another_hosts_session() {
        let root = TempRoot::new("clear-configured-other-host");
        write_lock(&root.0, Some("9"));
        let configured = MockServer::start();
        let elsewhere = issuing_api("https://cube.example", &configured.base_url());
        write_connect_json_issued_by(
            &root.0,
            ChainId::BitcoinBlake2b,
            Some("cube@example.com"),
            &[("cube@example.com", "cube-host-token")],
            elsewhere.as_deref(),
        );
        let lookup = mock_user(&configured, "cube-host-token", 9);
        let clear = mock_duress(&configured, "cube-host-token", false);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), None, &configured).await,
            None
        );
        lookup.assert_hits(0);
        clear.assert_hits(0);
    }

    #[tokio::test]
    async fn a_served_cubes_lock_with_no_account_reads_the_cubes_folder_on_its_host() {
        let root = TempRoot::new("clear-served-folder");
        let cube_host = MockServer::start();
        remote_lock_from_served_cube(&root, None, &cube_host);
        assert_eq!(
            DuressLocalState::load(&root.0).unwrap().lock_account(),
            None
        );
        write_connect_json(
            &root.0,
            ChainId::Bitcoin,
            Some("other@example.com"),
            &[("other@example.com", "launch")],
        );
        let cube_issuer = issuing_api(&cube_host.base_url(), "https://configured.invalid");
        write_connect_json_issued_by(
            &root.0,
            ChainId::BitcoinBlake2b,
            Some("cube@example.com"),
            &[("cube@example.com", "cube")],
            cube_issuer.as_deref(),
        );
        let configured = MockServer::start();
        let launch_clear = mock_duress(&configured, "launch", false);
        let launch_on_cube_host = mock_duress(&cube_host, "launch", false);
        let cube_active = mock_duress(&cube_host, "cube", true);
        assert_eq!(
            check(&root, Some(Network::Bitcoin), keyring(7, "kr"), &configured).await,
            Some(true)
        );
        cube_active.assert();
        launch_clear.assert_hits(0);
        launch_on_cube_host.assert_hits(0);
    }

    #[test]
    fn a_lock_origin_is_recorded_with_its_account_and_cleared_with_the_lock() {
        let origin = |url: &str| super::super::LockOrigin {
            api_base_url: url.to_string(),
            chain: ChainId::BitcoinBlake2b,
        };
        let mut state = DuressLocalState::default();
        // An unknown account records where to look while none is recorded.
        state.record_remote_lock_account(None, Some(origin("https://a")));
        assert_eq!(state.lock_origin(), Some(&origin("https://a")));
        // A known account replaces the origin with its own, even none.
        state.record_remote_lock_account(Some("9".into()), Some(origin("https://b")));
        assert_eq!(state.lock_origin(), Some(&origin("https://b")));
        // An unknown account then changes neither.
        state.record_remote_lock_account(None, Some(origin("https://c")));
        assert_eq!(state.lock_origin(), Some(&origin("https://b")));
        state.record_remote_lock_account(Some("8".into()), None);
        assert_eq!(state.lock_origin(), None);
        state.record_remote_lock_account(Some("9".into()), Some(origin("https://b")));
        state.record_local_lock();
        assert_eq!(state.lock_origin(), None);
        state.record_remote_lock_account(Some("9".into()), Some(origin("https://b")));
        state.clear_lock();
        assert_eq!(state.locked_by_origin, None);
    }

    #[test]
    fn an_unknown_account_does_not_replace_the_one_recorded_for_the_lock() {
        let mut state = DuressLocalState::default();
        state.record_remote_lock_account(Some("9".into()), None);
        state.record_remote_lock_account(None, None);
        assert_eq!(state.lock_account(), Some("9"));
        state.clear_lock();
        assert!(!state.remote_lock);
        assert_eq!(state.locked_by_account_id, None);
    }

    #[test]
    fn a_state_file_from_before_the_locked_by_field_loads() {
        let root = TempRoot::new("clear-old-state");
        std::fs::write(
            DuressLocalState::path(&root.0),
            br#"{"enrolled": true, "active": true, "accountId": "7"}"#,
        )
        .unwrap();
        let state = DuressLocalState::load(&root.0).unwrap();
        assert_eq!(state.locked_by_account_id, None);
        assert!(!state.remote_lock);
        assert_eq!(state.lock_account(), Some("7"));
        assert_eq!(state.lock_origin(), None);
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
