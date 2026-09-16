use async_trait::async_trait;
use reqwest::Response;
use serde::Deserialize;

/// Matches `{"success":false,"error":{"code":"...","message":"..."}}` error
/// bodies returned by the coincube-api on non-2xx responses.
#[derive(Debug, Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorBody,
}

#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    /// The API's machine-readable taxonomy code (`responses.go` defines ~70).
    /// Previously discarded here and re-parsed by a second, separate struct in
    /// `services::coincube`; deserialising it once means every caller can key
    /// user-facing copy — and the support reference — off a stable value
    /// instead of matching on English prose.
    #[serde(default)]
    code: String,
    message: String,
}

/// Information about an unsuccessful response.
#[derive(Debug, Clone)]
pub struct NotSuccessResponseInfo {
    pub status_code: u16,
    pub text: String,
}

impl NotSuccessResponseInfo {
    /// The server's human-readable message, **only** when the body is the
    /// standard coincube-api envelope
    /// (`{"success":false,"error":{"code","message"}}`).
    ///
    /// Returns `None` for anything else. This is deliberate and is the point of
    /// the type: the previous implementation fell back to `self.text.clone()`,
    /// so a body that wasn't our envelope — an nginx HTML error page, a raw
    /// upstream Mavapay/Meld payload, the legacy rate-limiter's
    /// `{"status":"ERROR","reason":…}`, or a Go handler that passed a GORM
    /// error through `err.Error()` — was rendered to the user verbatim. That
    /// one line was what turned every server-side leak into on-screen text.
    ///
    /// Callers that need the body for diagnostics use [`Self::raw_text`], which
    /// is log-only by convention.
    pub fn message(&self) -> Option<String> {
        self.envelope().map(|env| env.error.message)
    }

    /// The envelope's machine-readable `code`, when the body is our envelope
    /// and carries one. Used as the support reference so a single value is
    /// greppable in both the desktop log and the API log.
    pub fn code(&self) -> Option<String> {
        self.envelope()
            .map(|env| env.error.code)
            .filter(|c| !c.is_empty())
    }

    /// The raw response body. **Log-only** — never render this to a user; see
    /// [`Self::message`] for why.
    pub fn raw_text(&self) -> &str {
        &self.text
    }

    fn envelope(&self) -> Option<ApiErrorEnvelope> {
        serde_json::from_str::<ApiErrorEnvelope>(&self.text).ok()
    }
}

#[async_trait]
pub trait ResponseExt {
    async fn check_success(self) -> Result<Self, NotSuccessResponseInfo>
    where
        Self: Sized;
}

#[async_trait]
impl ResponseExt for Response {
    async fn check_success(self) -> Result<Self, NotSuccessResponseInfo> {
        let status = self.status();
        if !status.is_success() {
            return Err(NotSuccessResponseInfo {
                status_code: status.as_u16(),
                text: self
                    .text()
                    .await
                    .unwrap_or_else(|_| "Failed to read response text".to_string()),
            });
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::NotSuccessResponseInfo;

    #[test]
    fn message_unwraps_standard_api_error_envelope() {
        let info = NotSuccessResponseInfo {
            status_code: 400,
            text: r#"{"success":false,"error":{"code":"bad_request","message":"Invalid cube"}}"#
                .to_string(),
        };

        assert_eq!(info.message().as_deref(), Some("Invalid cube"));
        assert_eq!(info.code().as_deref(), Some("bad_request"));
    }

    #[test]
    fn non_envelope_body_never_reaches_the_user() {
        // Previously this returned the body verbatim, which is how raw
        // upstream payloads and database errors ended up on screen.
        let info = NotSuccessResponseInfo {
            status_code: 500,
            text: "plain upstream failure".to_string(),
        };

        assert_eq!(info.message(), None);
        assert_eq!(info.code(), None);
        assert_eq!(info.raw_text(), "plain upstream failure");
    }

    #[test]
    fn legacy_rate_limit_body_is_not_surfaced() {
        // The auth rate limiter emits this non-envelope shape on every 429.
        let info = NotSuccessResponseInfo {
            status_code: 429,
            text: r#"{"status":"ERROR","reason":"Too many attempts. Please try again later."}"#
                .to_string(),
        };

        assert_eq!(info.message(), None);
    }

    #[test]
    fn envelope_without_a_code_reports_no_code() {
        let info = NotSuccessResponseInfo {
            status_code: 400,
            text: r#"{"success":false,"error":{"message":"Invalid cube"}}"#.to_string(),
        };

        assert_eq!(info.message().as_deref(), Some("Invalid cube"));
        assert_eq!(info.code(), None);
    }
}
