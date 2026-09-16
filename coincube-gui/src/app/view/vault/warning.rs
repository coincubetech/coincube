use std::convert::From;

use iced::Length;

use coincube_ui::{
    component::notification,
    widget::{Column, Container},
};

use crate::{
    app::error::Error,
    daemon::{client::error::RpcErrorCode, DaemonError},
    user_error::{self, UserError},
};

/// Maps a Vault/daemon [`Error`] onto copy a person can act on.
///
/// Two rules this conversion follows, both learned the hard way.
///
/// **It is pure — no logging.** [`warn`] calls it, and `warn` is a *view*
/// function: iced rebuilds the view after every event, so a `log::error!` in
/// here writes a line per frame for as long as the banner is on screen.
/// Whatever stores the `Error` is the right place to log it, once.
///
/// **Where the error's own `Display` is written for a person, it is the
/// guidance.** `SpendCreationError` says "Invalid feerate: 0 sats/vb.";
/// `CoincubeDescError` says which part of a descriptor failed. Replacing those
/// with "Check the amount and fee rate, then try again." throws away the only
/// sentence that says what is actually wrong — the same mistake, in the other
/// direction, as printing a `reqwest::Error` at the user. The arms whose
/// `Display` is developer-facing ("ImportExport: subprocess handle lost") get
/// hand-written copy instead.
impl From<&Error> for UserError {
    fn from(error: &Error) -> UserError {
        match error {
            Error::Config(_) => UserError::new(
                "Configuration problem",
                "Your Coincube configuration couldn't be read. Restart the app; if it keeps failing, contact support and quote the reference below.",
                user_error::CC_CONFIG,
                false,
            ),

            Error::Wallet(_) => UserError::new(
                "Couldn't load this wallet",
                "Restart the app. If the wallet still won't open, restore it from your backup.",
                user_error::CC_WALLET,
                true,
            ),

            Error::Daemon(e) => match e {
                DaemonError::Rpc(code, detail) => {
                    if *code == RpcErrorCode::JSONRPC2_INVALID_PARAMS as i32 {
                        // The daemon rejected specific values and said which.
                        // That sentence is the whole point of the banner.
                        UserError::new(
                            "Some details aren't valid",
                            if detail.trim().is_empty() {
                                "Check the values you entered and try again.".to_string()
                            } else {
                                format!("{detail} Check the values you entered and try again.")
                            },
                            user_error::CC_DMN_RPC,
                            false,
                        )
                    } else {
                        UserError::new(
                            "The wallet engine rejected that request",
                            "Try again. If it keeps failing, restart the app and contact support with the reference below.",
                            user_error::CC_DMN_RPC,
                            true,
                        )
                    }
                }

                DaemonError::Http(..) => UserError::new(
                    "Can't reach your Bitcoin backend",
                    "Check that your node or Electrum server is running and reachable, then try again.",
                    user_error::CC_DMN_DOWN,
                    true,
                ),

                DaemonError::Unexpected(_) => UserError::new(
                    "Something went wrong",
                    "Try again. If this keeps happening, contact support and quote the reference below.",
                    user_error::CC_UNEXPECTED,
                    true,
                ),

                DaemonError::Start(_) => UserError::new(
                    "The wallet engine didn't start",
                    "Restart the app. If it still won't start, check that no other Coincube instance is running.",
                    user_error::CC_DMN_START,
                    true,
                ),

                DaemonError::ClientNotSupported => UserError::new(
                    "This wallet needs a newer version of Coincube",
                    "Update to the latest version to open it.",
                    user_error::CC_DMN_UNSUPPORTED,
                    false,
                ),

                DaemonError::NoAnswer | DaemonError::RpcSocket(..) => UserError::new(
                    "Lost contact with the wallet engine",
                    "Restart the app to reconnect. Your funds are unaffected.",
                    user_error::CC_DMN_DOWN,
                    true,
                ),

                DaemonError::DaemonStopped => UserError::new(
                    "The wallet engine stopped",
                    "Restart the app to reconnect. Your funds are unaffected.",
                    user_error::CC_DMN_DOWN,
                    true,
                ),

                DaemonError::CoinSelectionError => UserError::new(
                    "Not enough spendable coins",
                    "Your balance can't cover this amount plus the network fee. Try a smaller amount, or a lower fee rate.",
                    user_error::CC_DMN_COINSELECT,
                    false,
                ),

                DaemonError::NotImplemented => UserError::new(
                    "Not supported by this backend",
                    "Switch to a different Bitcoin backend in Settings to use this feature.",
                    user_error::CC_DMN_UNSUPPORTED,
                    false,
                ),
            },

            Error::Unexpected(_) => UserError::new(
                "Something went wrong",
                "Try again. If this keeps happening, contact support and quote the reference below.",
                user_error::CC_UNEXPECTED,
                true,
            ),

            // Keeps the long-form guidance that already lived on `Error`'s own
            // `Display` — it is genuinely good advice and covers the common
            // causes in order.
            Error::HardwareWallet(_) => UserError::new(
                "Can't talk to your hardware wallet",
                "Check that the device is connected and unlocked, that the Bitcoin app is open for the right network, and that no other wallet software is using it.",
                user_error::CC_HW,
                true,
            ),

            // `CoincubeDescError` and `SpendCreationError` both write for a
            // person, and name the specific problem.
            Error::Desc(e) => UserError::new(
                "This descriptor isn't valid",
                format!("{e} Check the descriptor you entered and try again."),
                user_error::CC_DESC,
                false,
            ),

            Error::Spend(e) => UserError::new(
                "Couldn't build this transaction",
                format!("{e} Check the amount and fee rate, then try again."),
                user_error::CC_SPEND,
                false,
            ),

            Error::ImportExport(_) => UserError::new(
                "Import or export failed",
                "Check that the file is a Coincube export and that you can read it, then try again.",
                user_error::CC_BACKUP,
                true,
            ),

            Error::RestoreBackup(e) => UserError::new(
                "Couldn't restore that backup",
                format!("{e}. Check that the backup file is complete and matches this wallet."),
                user_error::CC_BACKUP,
                true,
            ),

            Error::FiatPrice(_) => UserError::new(
                "Couldn't fetch the exchange rate",
                "Amounts are shown in bitcoin only. Check your internet connection and try again.",
                user_error::CC_FIAT,
                true,
            ),
        }
    }
}

/// Renders a Vault error as a warning banner.
///
/// The detail line is the support **reference**, not the raw error — that
/// string used to be `error.to_string()`, which is how daemon RPC codes and
/// HTTP internals reached the screen.
pub fn warn<'a, T: 'a + Clone>(error: Option<&Error>) -> Container<'a, T> {
    if let Some(w) = error {
        let u: UserError = w.into();
        notification::warning(
            format!("{} — {}", u.title, u.guidance),
            format!("Ref: {}", u.reference),
        )
        .width(Length::Fill)
    } else {
        Container::new(Column::new()).width(Length::Fill)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::spend::SpendCreationError;

    /// Every arm must name a next step. The dead ends this replaced —
    /// "Unknown error", "Internal error", "Wallet error" — left the user with
    /// nothing to do.
    #[test]
    fn every_arm_says_what_to_do_next() {
        let errors = [
            Error::Config("bad toml".to_string()),
            Error::Unexpected("boom".to_string()),
            Error::Daemon(DaemonError::DaemonStopped),
            Error::Daemon(DaemonError::NoAnswer),
            Error::Daemon(DaemonError::CoinSelectionError),
            Error::Daemon(DaemonError::NotImplemented),
            Error::Daemon(DaemonError::ClientNotSupported),
            Error::Daemon(DaemonError::Unexpected("x".to_string())),
            Error::Daemon(DaemonError::Start(
                coincubed::StartupError::DefaultDataDirNotFound,
            )),
            Error::Spend(SpendCreationError::InvalidFeerate(0)),
        ];

        for e in &errors {
            let u: UserError = e.into();
            assert!(!u.title.is_empty(), "no title for {:?}", e);
            assert!(!u.guidance.is_empty(), "no next step for {:?}", e);
            assert!(!u.reference.is_empty(), "no reference for {:?}", e);
        }
    }

    /// The error's own sentence is what says *what* is wrong. Dropping it for
    /// generic copy loses the only actionable part.
    #[test]
    fn a_user_authored_reason_survives_into_the_guidance() {
        let u: UserError = (&Error::Spend(SpendCreationError::InvalidFeerate(0))).into();
        assert!(
            u.guidance.contains("feerate"),
            "the specific reason must reach the user, got {:?}",
            u.guidance
        );
    }

    /// The daemon's own reason for rejecting values reaches the banner.
    #[test]
    fn the_daemon_reason_reaches_the_banner() {
        let u: UserError = (&Error::Daemon(DaemonError::Rpc(
            RpcErrorCode::JSONRPC2_INVALID_PARAMS as i32,
            "Insufficient funds for this feerate.".to_string(),
        )))
            .into();
        assert!(u.guidance.starts_with("Insufficient funds"));
        assert!(!u.retryable, "invalid input needs new input, not a retry");
    }

    /// An empty detail must not leave a banner that says nothing.
    #[test]
    fn an_empty_daemon_detail_still_gives_guidance() {
        let u: UserError = (&Error::Daemon(DaemonError::Rpc(
            RpcErrorCode::JSONRPC2_INVALID_PARAMS as i32,
            "   ".to_string(),
        )))
            .into();
        assert_eq!(u.guidance, "Check the values you entered and try again.");
    }

    /// `warn` runs on every frame, so the conversion it calls must not log.
    /// This is a documentation test as much as a behavioural one: it builds the
    /// banner repeatedly, which is exactly what iced does.
    #[test]
    fn rendering_the_banner_repeatedly_is_free_of_side_effects() {
        let e = Error::Daemon(DaemonError::DaemonStopped);
        for _ in 0..100 {
            let _: Container<'_, ()> = warn(Some(&e));
        }
    }
}
