//! The Connect session account-scoped reads work under (the unswept-coins
//! notice): the account's authenticated client and its opaque account id.

use crate::services::coincube::CoincubeClient;

/// Handed out by the App, which owns the Connect panel; refreshed on every
/// Connect message so a sign-in made while a read is pending is seen.
#[derive(Clone)]
pub struct ConnectSession {
    pub client: CoincubeClient,
    pub account: String,
}

/// The same Connect session: account, endpoint and credential.
pub(crate) fn same_session(a: &ConnectSession, b: &ConnectSession) -> bool {
    a.account == b.account
        && a.client.base_url == b.client.base_url
        && a.client.token() == b.client.token()
}
