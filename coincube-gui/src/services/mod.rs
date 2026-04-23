pub mod connect;
pub mod feeestimation;
pub mod fiat;

pub mod http;
pub mod keys;

pub mod coincube;
pub mod mavapay;
pub mod meld;
pub mod passkey;
pub mod sideshift;

/// Resolves the Coincube API base URL with this precedence:
/// 1. Runtime `std::env::var("COINCUBE_API_URL")` — picked up from the shell or
///    the `.env` loaded in `main()`; change and restart without rebuilding.
/// 2. Compile-time `option_env!("COINCUBE_API_URL")` — values baked in by
///    `build.rs` from `.env` at build time. Release builds started from a
///    directory that has no `.env` still work via this path.
/// 3. Hardcoded `https://dev-api.coincube.io` as a debug fallback. Release
///    builds require the env var at build time via `env!`, so they never
///    reach step 3.
///
/// Any trailing slashes are trimmed so callers can safely use
/// `format!("{}/api/v1/...", base)` without producing double-slash paths.
pub fn coincube_api_base_url() -> String {
    let raw: String = if let Ok(v) = std::env::var("COINCUBE_API_URL") {
        if !v.is_empty() {
            v
        } else if let Some(v) = option_env!("COINCUBE_API_URL") {
            v.to_string()
        } else {
            default_base_url()
        }
    } else if let Some(v) = option_env!("COINCUBE_API_URL") {
        v.to_string()
    } else {
        default_base_url()
    };
    raw.trim_end_matches('/').to_string()
}

fn default_base_url() -> String {
    #[cfg(debug_assertions)]
    {
        "https://dev-api.coincube.io".to_string()
    }
    #[cfg(not(debug_assertions))]
    {
        // Release builds must have COINCUBE_API_URL set at build time.
        env!("COINCUBE_API_URL").to_string()
    }
}

/// Resolves the LNURL domain the bridge binds Lightning Addresses
/// against. Mirrors the bridge's `COINCUBE_LNURL_DOMAIN` lookup in
/// `coincube-spark-bridge/src/sdk_adapter.rs`: the env var wins
/// when set to a non-empty value, otherwise the default matches
/// the bridge's `DEFAULT_LNURL_DOMAIN` constant.
///
/// Used by the fallback formatters that append `@<domain>` to a
/// bare username when the stored record doesn't include one.
/// Keeping this lookup in a single place means the UI and the SDK
/// advertise the same domain under every environment override —
/// a mismatch would show the user an address that doesn't resolve.
///
/// Cached after first resolution (env vars don't change at
/// runtime and this is called from render paths).
pub fn lnurl_domain() -> &'static str {
    static DOMAIN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DOMAIN
        .get_or_init(|| {
            std::env::var("COINCUBE_LNURL_DOMAIN")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "pay.coincube.io".to_string())
        })
        .as_str()
}

/// `@<domain>` form of [`lnurl_domain`] for formatters that append
/// it to a bare username. Cached.
pub fn lnurl_at_suffix() -> &'static str {
    static SUFFIX: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SUFFIX
        .get_or_init(|| format!("@{}", lnurl_domain()))
        .as_str()
}
