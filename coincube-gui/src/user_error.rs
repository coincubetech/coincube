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
//! - `title` — what happened, in plain language ("Can't reach COINCUBE")
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

use std::fmt::Display;

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
}

impl Display for UserError {
    /// For logs and tests. The UI renders the fields separately — it never
    /// formats a `UserError` as one string.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — {} [{}]", self.title, self.guidance, self.reference)
    }
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
