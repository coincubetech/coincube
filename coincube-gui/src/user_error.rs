//! User-facing error presentation.
//!
//! # The rule this module exists to enforce
//!
//! **Views render a [`UserError`]. `Display` on an error type is for logs.**
//! The UI must never call `.to_string()` on an error and put the result on
//! screen.
//!
//! Before this module, it did — in ~107 places. The sign-in screen greeted a
//! token-refresh timeout with
//! `Network error: reqwest::Error { kind: Request, url: "https://api.coincube.io/api/v1/auth/token/refresh", source: TimedOut }`,
//! which tells the user nothing, tells them nothing to *do*, and hands an
//! attacker the API host and endpoint path for free.
//!
//! # Shape
//!
//! A [`UserError`] carries what a person needs and nothing else:
//!
//! - `title` — what happened, in plain language ("Can't reach COINCUBE | Connect")
//! - `guidance` — what to do next ("Check your internet connection and try again.")
//! - `reference` — a short opaque code (`CC-NET-TIMEOUT`) shown as `Ref: …`
//! - `retryable` — whether re-running the same action could plausibly work,
//!   which is what decides if a retry button renders at all
//!
//! The raw technical detail is deliberately **not** a field. It goes to the log
//! file, tagged with the same `reference`, via [`UserError::logged`]. That is
//! the industry-standard split (Stripe, Google, AWS all pair plain copy with a
//! stable code): the screen stays clean and leak-free, while support can still
//! correlate a user's `Ref:` to a log line carrying the real cause.
//!
//! # References
//!
//! `CC-<DOMAIN>-<REASON>`, with domains `NET`, `API`, `AUTH`, `DMN` (daemon),
//! `LQD` (Liquid), `SPK` (Spark), `HW` (hardware wallet).
//!
//! When the server already sent one of its ~70 taxonomy codes
//! (`coincube-api/services/keychain/responses/responses.go`), use that verbatim
//! instead of inventing a parallel one — it makes a single `Ref:` greppable
//! across both repos' logs.
//!
//! # Naming things in copy
//!
//! Two products, two names, and they are not interchangeable:
//!
//! - **Tenshu** is this desktop app. Anything local — its configuration, its
//!   process, its version, the files it exports — is Tenshu.
//! - **COINCUBE | Connect** is the server-side account service. Anything that
//!   failed over the network to `coincube-api` names Connect.
//!
//! Getting this backwards sends the user to the wrong place: "restart COINCUBE"
//! is not an action anyone can take against a web service, and "check your
//! internet connection" is wrong advice for a local config file.
//!
//! # Who logs
//!
//! Exactly one place per failure, and it is never a view. [`UserError::logged`]
//! covers the conversions that run in `update`; the conversions that run inside
//! a `view` (iced rebuilds those every frame) are pure, and the state that
//! stores the error calls [`report`] instead. See [`From<&crate::app::error::Error>`]
//! for the whole argument.

use std::fmt::Display;

use crate::{
    app::error::Error,
    daemon::{client::error::RpcErrorCode, DaemonError},
};

// ── Network / transport ─────────────────────────────────────────────────────
pub const CC_NET_TIMEOUT: &str = "CC-NET-TIMEOUT";
pub const CC_NET_OFFLINE: &str = "CC-NET-OFFLINE";
pub const CC_NET_TLS: &str = "CC-NET-TLS";
pub const CC_NET_UNKNOWN: &str = "CC-NET-UNKNOWN";

// ── API responses ───────────────────────────────────────────────────────────
pub const CC_API_5XX: &str = "CC-API-5XX";
pub const CC_API_BADRESP: &str = "CC-API-BADRESP";
pub const CC_API_NOTFOUND: &str = "CC-API-NOTFOUND";
pub const CC_API_STREAM: &str = "CC-API-STREAM";

// ── Auth ────────────────────────────────────────────────────────────────────
pub const CC_AUTH_EXPIRED: &str = "CC-AUTH-EXPIRED";
pub const CC_AUTH_RATELIMIT: &str = "CC-AUTH-RATELIMIT";

// ── Vault daemon ────────────────────────────────────────────────────────────
pub const CC_DMN_DOWN: &str = "CC-DMN-DOWN";
pub const CC_DMN_RPC: &str = "CC-DMN-RPC";
pub const CC_DMN_START: &str = "CC-DMN-START";
pub const CC_DMN_COINSELECT: &str = "CC-DMN-COINSELECT";
pub const CC_DMN_UNSUPPORTED: &str = "CC-DMN-UNSUPPORTED";

// ── Liquid / Spark wallets ──────────────────────────────────────────────────
pub const CC_LQD_SDK: &str = "CC-LQD-SDK";
pub const CC_LQD_CONN: &str = "CC-LQD-CONN";
pub const CC_LQD_SIGNER: &str = "CC-LQD-SIGNER";
pub const CC_LQD_UNSUPPORTED: &str = "CC-LQD-UNSUPPORTED";
pub const CC_SPK: &str = "CC-SPK";

// ── Wallet / signing / misc ─────────────────────────────────────────────────
pub const CC_WALLET: &str = "CC-WALLET";
pub const CC_HW: &str = "CC-HW";
pub const CC_DESC: &str = "CC-DESC";
pub const CC_SPEND: &str = "CC-SPEND";
pub const CC_BACKUP: &str = "CC-BACKUP";
pub const CC_FIAT: &str = "CC-FIAT";
pub const CC_CONFIG: &str = "CC-CONFIG";
pub const CC_UNEXPECTED: &str = "CC-UNEXPECTED";

/// Guidance line used wherever the only honest advice is "try again".
pub const RETRY_GUIDANCE: &str = "Check your internet connection and try again.";

/// A failure, described the way a person needs to hear it.
///
/// Construct with [`UserError::logged`] whenever a raw error exists — that
/// sends the technical detail to the log under the same `reference` the user
/// sees. Use [`UserError::new`] only when there is no underlying detail to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserError {
    /// What happened, in plain language. No jargon, no identifiers, no URLs.
    pub title: String,
    /// What the user should do next. Always actionable, even if the action is
    /// "contact support with this reference".
    pub guidance: String,
    /// Opaque support reference, rendered as `Ref: …`.
    pub reference: String,
    /// Whether re-running the same action unchanged could succeed. Drives
    /// whether a retry button is offered; a `false` here means retrying just
    /// reproduces the same failure.
    pub retryable: bool,
}

impl UserError {
    /// Build without logging. For failures that have no underlying technical
    /// detail worth recording (a validation refusal the user can see and fix).
    pub fn new(
        title: impl Into<String>,
        guidance: impl Into<String>,
        reference: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            title: title.into(),
            guidance: guidance.into(),
            reference: reference.into(),
            retryable,
        }
    }

    /// Build and send `detail` to the log, tagged with `reference`.
    ///
    /// This is the pairing that makes the whole scheme work: the user reads a
    /// clean sentence and a short code, and the same code appears in the log
    /// beside the real cause. Always prefer this over [`UserError::new`] when
    /// an underlying error exists — the detail is otherwise lost.
    pub fn logged(
        title: impl Into<String>,
        guidance: impl Into<String>,
        reference: impl Into<String>,
        retryable: bool,
        detail: impl Display,
    ) -> Self {
        let reference = reference.into();
        log::error!("[{}] {}", reference, detail);
        Self::new(title, guidance, reference, retryable)
    }

    /// Catch-all for a failure with no specific mapping yet. Deliberately
    /// points the user at support with the reference rather than leaving them
    /// at a dead end like the old bare `"Unknown error"`.
    pub fn unexpected(detail: impl Display) -> Self {
        Self::logged(
            "Something went wrong",
            "Try again. If this keeps happening, contact support and quote the reference below.",
            CC_UNEXPECTED,
            true,
            detail,
        )
    }

    /// The retry message to offer, when this error is worth retrying at all.
    /// Returns `None` for settled failures so callers can't accidentally offer
    /// a button that re-runs a request the server has already refused.
    pub fn retry_action<T>(&self, message: T) -> Option<(&'static str, T)> {
        self.retryable.then_some(("Try again", message))
    }

    /// One line for the toast overlay, which has room for a single text run.
    ///
    /// The card gets three separate fields; a toast cannot, so this is the one
    /// place the three are joined. The reference stays in: a toast is often the
    /// *only* thing a user sees for a transient failure, and without the code
    /// there is nothing for support to correlate.
    pub fn toast(&self) -> String {
        format!(
            "{} — {} (Ref: {})",
            self.title, self.guidance, self.reference
        )
    }
}

/// Convert a Vault/daemon [`Error`] for display, logging its technical detail
/// once on the way through.
///
/// This is the counterpart to the pure `From<&Error>` conversion below. The
/// state layer holds the only place a given failure is handled exactly once, so
/// it is the only place that may log; call this wherever an `Error` is stored
/// or turned into a toast, and never log the same error again alongside it.
///
/// Returns the toast copy, so the common state-layer shape is a one-liner:
///
/// ```ignore
/// Err(e) => {
///     let err_msg = user_error::report(&e);
///     self.warning = Some(e);
///     return Task::done(Message::View(view::Message::ShowError(err_msg)));
/// }
/// ```
pub fn report(error: &Error) -> String {
    let user: UserError = error.into();
    log::error!("[{}] {}", user.reference, error);
    user.toast()
}

/// Maps a Vault/daemon [`Error`] onto copy a person can act on.
///
/// Two rules this conversion follows, both learned the hard way.
///
/// **It is pure — no logging.** `app::view::vault::warning::warn` calls it, and
/// `warn` is a *view* function: iced rebuilds the view after every event, so a
/// `log::error!` in here writes a line per frame for as long as the banner is
/// on screen. [`report`] is where the logging lives, at the state layer that
/// handles the failure once.
///
/// **Where the error's own `Display` is written for a person, it is the
/// guidance.** `SpendCreationError` says "Invalid feerate: 0 sats/vb.";
/// `CoincubeDescError` says which part of a descriptor failed. Replacing those
/// with "Check the amount and fee rate, then try again." throws away the only
/// sentence that says what is actually wrong — the same mistake, in the other
/// direction, as printing a `reqwest::Error` at the user. The arms whose
/// `Display` is developer-facing get hand-written copy instead.
impl From<&Error> for UserError {
    fn from(error: &Error) -> UserError {
        match error {
            Error::Config(_) => UserError::new(
                "Configuration problem",
                "Tenshu's configuration couldn't be read. Restart the app; if it keeps failing, contact support and quote the reference below.",
                CC_CONFIG,
                false,
            ),

            Error::Wallet(_) => UserError::new(
                "Couldn't load this wallet",
                "Restart the app. If the wallet still won't open, restore it from your backup.",
                CC_WALLET,
                true,
            ),

            Error::Daemon(e) => match e {
                // The daemon's own message is not user copy: `invalid_params`
                // names JSON-RPC parameters ("Missing 'destinations'
                // parameter.", "Invalid 'psbt' parameter."), which tells a
                // person nothing they can act on and exposes the wire API. It
                // goes to the log via `report`, like every other detail here.
                DaemonError::Rpc(code, _) => {
                    if *code == RpcErrorCode::JSONRPC2_INVALID_PARAMS as i32 {
                        UserError::new(
                            "Some details aren't valid",
                            "Check the values you entered and try again.",
                            CC_DMN_RPC,
                            false,
                        )
                    } else {
                        UserError::new(
                            "The wallet engine rejected that request",
                            "Try again. If it keeps failing, restart the app and contact support with the reference below.",
                            CC_DMN_RPC,
                            true,
                        )
                    }
                }

                DaemonError::Http(..) => UserError::new(
                    "Can't reach your Bitcoin backend",
                    "Check that your node or Electrum server is running and reachable, then try again.",
                    CC_DMN_DOWN,
                    true,
                ),

                DaemonError::Unexpected(_) => UserError::new(
                    "Something went wrong",
                    "Try again. If this keeps happening, contact support and quote the reference below.",
                    CC_UNEXPECTED,
                    true,
                ),

                DaemonError::Start(_) => UserError::new(
                    "The wallet engine didn't start",
                    "Restart the app. If it still won't start, check that no other Tenshu instance is running.",
                    CC_DMN_START,
                    true,
                ),

                DaemonError::ClientNotSupported => UserError::new(
                    "This wallet needs a newer version of Tenshu",
                    "Update to the latest version to open it.",
                    CC_DMN_UNSUPPORTED,
                    false,
                ),

                DaemonError::NoAnswer | DaemonError::RpcSocket(..) => UserError::new(
                    "Lost contact with the wallet engine",
                    "Restart the app to reconnect. Your funds are unaffected.",
                    CC_DMN_DOWN,
                    true,
                ),

                DaemonError::DaemonStopped => UserError::new(
                    "The wallet engine stopped",
                    "Restart the app to reconnect. Your funds are unaffected.",
                    CC_DMN_DOWN,
                    true,
                ),

                DaemonError::CoinSelectionError => UserError::new(
                    "Not enough spendable coins",
                    "Your balance can't cover this amount plus the network fee. Try a smaller amount, or a lower fee rate.",
                    CC_DMN_COINSELECT,
                    false,
                ),

                DaemonError::NotImplemented => UserError::new(
                    "Not supported by this backend",
                    "Switch to a different Bitcoin backend in Settings to use this feature.",
                    CC_DMN_UNSUPPORTED,
                    false,
                ),
            },

            Error::Unexpected(_) => UserError::new(
                "Something went wrong",
                "Try again. If this keeps happening, contact support and quote the reference below.",
                CC_UNEXPECTED,
                true,
            ),

            // Keeps the long-form guidance that already lived on `Error`'s own
            // `Display` — it is genuinely good advice and covers the common
            // causes in order.
            Error::HardwareWallet(_) => UserError::new(
                "Can't talk to your hardware wallet",
                "Check that the device is connected and unlocked, that the Bitcoin app is open for the right network, and that no other wallet software is using it.",
                CC_HW,
                true,
            ),

            // `CoincubeDescError` and `SpendCreationError` both write for a
            // person, and name the specific problem.
            Error::Desc(e) => UserError::new(
                "This descriptor isn't valid",
                format!("{e} Check the descriptor you entered and try again."),
                CC_DESC,
                false,
            ),

            Error::Spend(e) => UserError::new(
                "Couldn't build this transaction",
                format!("{e} Check the amount and fee rate, then try again."),
                CC_SPEND,
                false,
            ),

            Error::ImportExport(_) => UserError::new(
                "Import or export failed",
                "Check that the file is a Tenshu export and that you can read it, then try again.",
                CC_BACKUP,
                true,
            ),

            Error::RestoreBackup(e) => UserError::new(
                "Couldn't restore that backup",
                format!("{e}. Check that the backup file is complete and matches this wallet."),
                CC_BACKUP,
                true,
            ),

            Error::FiatPrice(_) => UserError::new(
                "Couldn't fetch the exchange rate",
                "Amounts are shown in bitcoin only. Check your internet connection and try again.",
                CC_FIAT,
                true,
            ),
        }
    }
}

impl Display for UserError {
    /// For logs and tests. The UI renders the fields separately — it never
    /// formats a `UserError` as one string.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — {} [{}]", self.title, self.guidance, self.reference)
    }
}

/// Maps a Liquid (Breez SDK) failure onto copy a person can act on.
///
/// `BreezError`'s own `Display` is written for a log — "SDK request failed:
/// …" wraps whatever string the SDK produced, which is frequently a GraphQL
/// envelope or a swap-service trace. That string reached the Liquid panels'
/// toasts verbatim.
///
/// The SDK's own message is never surfaced. Unlike the `coincube-api`
/// envelope, it is not authored for users and carries no stable code to key
/// copy off, so there is nothing here to prefer over hand-written guidance.
impl From<&crate::app::breez_liquid::BreezError> for UserError {
    fn from(e: &crate::app::breez_liquid::BreezError) -> UserError {
        use crate::app::breez_liquid::BreezError as B;

        match e {
            B::NetworkNotSupported(network) => UserError::new(
                "Liquid isn't available on this network",
                format!("Your wallet is on {network}, which the Liquid wallet doesn't support. Switch to a supported network to use it."),
                CC_LQD_UNSUPPORTED,
                false,
            ),

            B::Connection(detail) => UserError::logged(
                "Can't reach the Liquid network",
                "Check your internet connection and try again.",
                CC_LQD_CONN,
                true,
                detail,
            ),

            B::Sdk(detail) => UserError::logged(
                "The Liquid wallet couldn't complete that",
                "Try again. If it keeps failing, contact support and quote the reference below.",
                CC_LQD_SDK,
                true,
                detail,
            ),

            B::SignerNotFound(_) | B::SignerError(_) => UserError::logged(
                "Couldn't unlock your Liquid wallet",
                "Restart Tenshu and unlock your Cube again. If it keeps failing, contact support and quote the reference below.",
                CC_LQD_SIGNER,
                false,
                e,
            ),
        }
    }
}

/// As [`report`], for a Liquid failure.
pub fn report_liquid(error: &crate::app::breez_liquid::BreezError) -> String {
    let user: UserError = error.into();
    user.toast()
}

/// Maps a Spark (bridge subprocess) failure onto copy a person can act on.
///
/// `SparkClientError`'s `Display` names the bridge and its error kinds —
/// "Spark bridge error (not connected): …" — which describes our process
/// topology, not anything the user can act on.
impl From<&crate::app::breez_spark::client::SparkClientError> for UserError {
    fn from(e: &crate::app::breez_spark::client::SparkClientError) -> UserError {
        UserError::logged(
            "The Spark wallet couldn't complete that",
            "Try again. If it keeps failing, restart Tenshu and contact support with the reference below.",
            CC_SPK,
            true,
            e,
        )
    }
}

/// As [`report`], for a Spark failure.
pub fn report_spark(error: &crate::app::breez_spark::client::SparkClientError) -> String {
    UserError::from(error).toast()
}

/// Maps an import/export failure onto copy a person can act on.
///
/// `export::Error`'s `Display` is a developer log line — "ImportExport:
/// subprocess handle lost", "ImportExport fail to handle.join()" — and one arm
/// still tells users to contact Wizardsardine, the upstream vendor this app was
/// forked from. None of it belongs on screen.
///
/// The arms that describe something the user did (a file for the wrong network,
/// a PSBT that isn't theirs) keep their own sentence, because it names the
/// actual problem.
impl From<&crate::export::Error> for UserError {
    fn from(e: &crate::export::Error) -> UserError {
        use crate::export::Error as E;

        match e {
            E::XpubNetwork => UserError::new(
                "That key is for a different network",
                "Export the key again from the device with the right network selected.",
                CC_BACKUP,
                false,
            ),
            E::TxidNotMatch => UserError::new(
                "That PSBT is for a different transaction",
                "Check you picked the right file, then import it again.",
                CC_BACKUP,
                false,
            ),
            E::OutpointNotOwned => UserError::new(
                "That PSBT isn't for this wallet",
                "It either belongs to another wallet or its coins are already spent. Check the file and try again.",
                CC_BACKUP,
                false,
            ),
            E::UnknownFormat | E::ParsePsbt | E::ParseDescriptor | E::ParseXpub => {
                UserError::logged(
                    "Tenshu couldn't read that file",
                    "Check that it's the file you meant and that it isn't damaged, then try again.",
                    CC_BACKUP,
                    false,
                    e,
                )
            }
            E::InsanePsbt => UserError::new(
                "That PSBT isn't valid",
                "Re-export it from the wallet that created it, then import it again.",
                CC_BACKUP,
                false,
            ),
            // Everything else is plumbing: a lost subprocess, a dead channel,
            // an IO or daemon failure. Retrying is the only honest advice.
            other => UserError::logged(
                "Import or export failed",
                "Try again. If it keeps failing, contact support and quote the reference below.",
                CC_BACKUP,
                true,
                other,
            ),
        }
    }
}

/// As [`report`], for an import/export failure.
pub fn report_export(error: &crate::export::Error) -> String {
    UserError::from(error).toast()
}

/// Copy for a rejected Border Wallet grid selection.
///
/// Both the installer wizard and the PSBT signing screen render this, and both
/// previously used `format!("{:?}", e)` — putting Rust variant names like
/// `DuplicateCell { row: 3, col: 2 }` on screen. `BorderWalletError`'s own
/// `Display` is better but still developer-facing ("invalid pattern length:
/// expected 11 cells, got 12"); what a person needs is what to do about it.
pub fn border_wallet_cell_message(e: &coincube_core::border_wallet::BorderWalletError) -> String {
    use coincube_core::border_wallet::BorderWalletError as E;
    match e {
        E::InvalidPatternLength(_) => {
            "Your pattern already has all 11 cells. Deselect one before choosing another."
                .to_string()
        }
        E::DuplicateCell { .. } => {
            "You've already chosen that cell. Pick a different one.".to_string()
        }
        E::CellOutOfBounds { .. } => {
            "That cell is outside the grid. Pick one inside it.".to_string()
        }
        // Not reachable from a cell tap, but a dead end is never the right
        // fallback — say what to do next.
        other => {
            log::error!("[{}] border wallet: {}", CC_WALLET, other);
            "That selection couldn't be applied. Try a different cell.".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_action_is_offered_only_for_retryable_errors() {
        let transient = UserError::new("t", "g", CC_NET_TIMEOUT, true);
        assert_eq!(transient.retry_action(42), Some(("Try again", 42)));

        let settled = UserError::new("t", "g", CC_API_NOTFOUND, false);
        assert_eq!(settled.retry_action(42), None);
    }

    /// Every arm must name a next step. The dead ends this replaced —
    /// "Unknown error", "Internal error", "Wallet error" — left the user with
    /// nothing to do.
    #[test]
    fn every_vault_error_arm_says_what_to_do_next() {
        use coincube_core::spend::SpendCreationError;

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
        use coincube_core::spend::SpendCreationError;

        let u: UserError = (&Error::Spend(SpendCreationError::InvalidFeerate(0))).into();
        assert!(
            u.guidance.contains("feerate"),
            "the specific reason must reach the user, got {:?}",
            u.guidance
        );
    }

    /// The daemon's rejection message is wire-protocol text, not user copy:
    /// `invalid_params` names JSON-RPC parameters ("Missing 'destinations'
    /// parameter."). It must not reach the screen.
    #[test]
    fn the_daemon_wire_message_never_reaches_the_user() {
        let u: UserError = (&Error::Daemon(DaemonError::Rpc(
            RpcErrorCode::JSONRPC2_INVALID_PARAMS as i32,
            "Invalid 'psbt' parameter.".to_string(),
        )))
            .into();

        for field in [&u.title, &u.guidance] {
            assert!(!field.contains("psbt"), "wire field leaked: {:?}", field);
            assert!(
                !field.contains("parameter"),
                "wire field leaked: {:?}",
                field
            );
        }
        assert_eq!(u.guidance, "Check the values you entered and try again.");
        assert!(!u.retryable, "invalid input needs new input, not a retry");
    }

    /// Anything local is Tenshu; anything that failed over the network to the
    /// account service is COINCUBE | Connect. Naming the wrong one sends the
    /// user somewhere they cannot act.
    #[test]
    fn local_failures_name_tenshu_not_the_backend() {
        let local = [
            Error::Config("bad toml".to_string()),
            Error::Daemon(DaemonError::ClientNotSupported),
            Error::Daemon(DaemonError::Start(
                coincubed::StartupError::DefaultDataDirNotFound,
            )),
        ];

        for e in &local {
            let u: UserError = e.into();
            let copy = format!("{} {}", u.title, u.guidance);
            assert!(
                !copy.contains("COINCUBE") && !copy.contains("Coincube"),
                "a local failure must not name the backend: {:?}",
                copy
            );
        }

        let u: UserError = (&Error::Config("bad toml".to_string())).into();
        assert!(u.guidance.contains("Tenshu"), "got {:?}", u.guidance);
    }

    /// The toast has room for one text run, so the three fields are joined —
    /// but the reference has to survive, or a toast-only failure leaves support
    /// with nothing to correlate.
    #[test]
    fn the_toast_line_keeps_the_reference() {
        let u = UserError::new("Title", "Do this.", CC_NET_TIMEOUT, true);
        assert_eq!(u.toast(), "Title — Do this. (Ref: CC-NET-TIMEOUT)");
    }

    /// `report` is the state layer's entry point: it must hand back the toast
    /// copy and never the raw error.
    #[test]
    fn report_returns_sanitised_copy() {
        let line = report(&Error::Daemon(DaemonError::Rpc(
            RpcErrorCode::JSONRPC2_INVALID_PARAMS as i32,
            "Invalid 'psbt' parameter.".to_string(),
        )));
        assert!(!line.contains("psbt"), "raw detail leaked: {}", line);
        assert!(line.contains(CC_DMN_RPC));
    }

    /// The Liquid SDK's own strings are GraphQL/swap-service traces. None of
    /// them may reach the screen.
    #[test]
    fn the_liquid_sdk_string_never_reaches_the_user() {
        use crate::app::breez_liquid::BreezError;

        let leaky = BreezError::Sdk(
            "graphql error: Unable to reach https://swap.example/api: connection reset".to_string(),
        );
        let u: UserError = (&leaky).into();
        for field in [&u.title, &u.guidance, &u.reference] {
            for forbidden in ["graphql", "swap.example", "connection reset"] {
                assert!(
                    !field.contains(forbidden),
                    "{} leaked in {}",
                    forbidden,
                    field
                );
            }
        }
        assert_eq!(u.reference, CC_LQD_SDK);
        assert!(u.retryable);

        // A connection failure and an SDK refusal are different situations and
        // must not collapse into one reference.
        let offline: UserError = (&BreezError::Connection("dns failure".to_string())).into();
        assert_eq!(offline.reference, CC_LQD_CONN);
        assert!(!offline.guidance.contains("dns"));
    }

    /// `export::Error`'s `Display` is a developer log line, and one arm still
    /// names the upstream vendor this app was forked from.
    #[test]
    fn export_plumbing_errors_are_not_shown_verbatim() {
        let u: UserError = (&crate::export::Error::HandleLost).into();
        assert!(!u.title.contains("ImportExport"), "got {:?}", u.title);
        assert!(!u.guidance.contains("subprocess"), "got {:?}", u.guidance);

        let u: UserError = (&crate::export::Error::EncryptionFailed).into();
        for field in [&u.title, &u.guidance] {
            assert!(
                !field.contains("Wizarsardine") && !field.contains("Wizardsardine"),
                "the upstream vendor must not be named at users: {}",
                field
            );
        }

        // The arms that describe what the *user* did keep their own sentence.
        let u: UserError = (&crate::export::Error::XpubNetwork).into();
        assert!(u.title.contains("network"), "got {:?}", u.title);
    }

    /// The Spark bridge is our own process topology; naming it tells the user
    /// nothing.
    #[test]
    fn the_spark_bridge_is_not_described_to_the_user() {
        use crate::app::breez_spark::client::SparkClientError;

        let u: UserError = (&SparkClientError::Protocol("bad frame".to_string())).into();
        for field in [&u.title, &u.guidance] {
            assert!(!field.contains("bridge"), "topology leaked: {}", field);
            assert!(!field.contains("bad frame"), "detail leaked: {}", field);
        }
        assert_eq!(u.reference, CC_SPK);
    }

    #[test]
    fn unexpected_tells_the_user_what_to_do_next() {
        let e = UserError::unexpected("some internal detail");
        // The old behaviour was a bare "Unknown error" with no next step.
        assert!(!e.guidance.is_empty());
        assert_eq!(e.reference, CC_UNEXPECTED);
        // The raw detail must not ride along into anything the UI renders.
        assert!(!e.title.contains("internal detail"));
        assert!(!e.guidance.contains("internal detail"));
    }
}
