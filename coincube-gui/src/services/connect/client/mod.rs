pub mod auth;
pub mod cache;

use serde::Deserialize;

const CONNECT_GRPC_URL_ENV: &str = "COINCUBE_CONNECT_GRPC_URL";
const LEGACY_GRPC_URL_ENV: &str = "COINCUBE_GRPC_URL";

/// COINCUBE-owned Connect service config (`GET /api/v1/connect/service-config`).
/// Source of the Connect signing gRPC endpoint for **all** networks. The
/// endpoint is network-agnostic (one gRPC deployment serves every network), so
/// we don't pass a `network` query.
#[derive(Debug, Clone, Deserialize)]
struct CoincubeServiceConfig {
    #[serde(rename = "grpcUrl")]
    grpc_url: Option<String>,
    #[serde(default = "default_connect_grpc_tls")]
    tls: bool,
}

fn default_connect_grpc_tls() -> bool {
    true
}

/// Resolve the Connect signing gRPC endpoint, COINCUBE-only, in precedence order:
///   1. runtime env override (staging/local),
///   2. COINCUBE service-config endpoint (env-specific, all networks),
///   3. compile-time baked default (resilience if the endpoint is down).
pub async fn resolve_connect_grpc_url() -> Option<String> {
    resolve_connect_grpc_url_for(&crate::services::coincube_api_base_url()).await
}

/// As [`resolve_connect_grpc_url`], for the Connect API at `base_url`: the
/// service config is fetched from that base URL. The baked default belongs to
/// the app's configured API, so it is a fallback only when `base_url` is that
/// one; a Cube whose session is on another endpoint is never handed the
/// default endpoint's gRPC address.
pub async fn resolve_connect_grpc_url_for(base_url: &str) -> Option<String> {
    if let Some(url) = reject_plaintext_in_release(runtime_grpc_url_override()) {
        return Some(url);
    }
    grpc_url_or_baked_default(
        coincube_grpc_url(base_url).await,
        is_configured_base_url(base_url),
        || reject_plaintext_in_release(buildtime_grpc_url_default()),
    )
}

/// The fetched gRPC address, or the baked default when nothing was fetched
/// and the endpoint asked is the app's configured one.
fn grpc_url_or_baked_default(
    fetched: Option<String>,
    configured_endpoint: bool,
    baked_default: impl FnOnce() -> Option<String>,
) -> Option<String> {
    match fetched {
        Some(url) => Some(url),
        None if configured_endpoint => baked_default(),
        None => None,
    }
}

/// Whether `base_url` is the app's configured Connect API, ignoring a
/// trailing slash.
fn is_configured_base_url(base_url: &str) -> bool {
    same_base_url(base_url, &crate::services::coincube_api_base_url())
}

fn same_base_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// Fetch the Connect signing gRPC URL from coincube-api. Best-effort: any
/// network/parse failure yields `None` so the caller can fall back. The
/// returned URL is normalized to a tonic-ready form (`https://…` when TLS,
/// `http://…` otherwise) so `create_channel` enables TLS correctly.
async fn coincube_grpc_url(base_url: &str) -> Option<String> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/api/v1/connect/service-config");
    let https_only = !base.starts_with("http://");
    let client = reqwest::Client::builder()
        .https_only(https_only)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;
    let cfg: CoincubeServiceConfig = client.get(url).send().await.ok()?.json().await.ok()?;
    let raw = cfg.grpc_url?;
    reject_plaintext_in_release(normalize_grpc_url(&raw, cfg.tls))
}

/// Release builds drop a cleartext gRPC URL instead of dialing it. Debug
/// builds keep `http://` for a local signer.
fn reject_plaintext_in_release(url: Option<String>) -> Option<String> {
    #[cfg(not(debug_assertions))]
    if url.as_deref().is_some_and(|u| u.starts_with("http://")) {
        tracing::error!("refusing a plaintext Connect gRPC URL in a release build");
        return None;
    }
    url
}

/// Compile-time baked gRPC URL (`COINCUBE_CONNECT_GRPC_URL` at build time),
/// used only when the service-config endpoint is unreachable. `None` when not
/// baked in.
fn buildtime_grpc_url_default() -> Option<String> {
    // Normalize like the service-config path so a baked bare `host:port`
    // still enables TLS in `create_channel` (which keys on the `https://`
    // prefix). A scheme-qualified value is passed through unchanged;
    // default to TLS otherwise — plaintext dev endpoints must spell out
    // `http://`.
    option_env!("COINCUBE_CONNECT_GRPC_URL").and_then(|v| normalize_grpc_url(v, true))
}

/// Normalize a gRPC endpoint to a scheme-qualified URL tonic accepts. Returns
/// `None` for an empty value. An already-qualified `http(s)://` value is kept
/// as-is; otherwise the scheme is chosen from `tls`.
fn normalize_grpc_url(raw: &str, tls: bool) -> Option<String> {
    let raw = raw.trim().trim_end_matches('/');
    if raw.is_empty() {
        return None;
    }
    if raw.starts_with("http://") || raw.starts_with("https://") {
        Some(raw.to_string())
    } else if tls {
        Some(format!("https://{raw}"))
    } else {
        Some(format!("http://{raw}"))
    }
}

fn runtime_grpc_url_override() -> Option<String> {
    // Normalize like the service-config path so a bare `host:port` from the
    // env override still enables TLS in `create_channel` (which keys on the
    // `https://` prefix). Scheme-qualified values pass through unchanged;
    // default to TLS otherwise — plaintext/local endpoints must spell out
    // `http://`.
    std::env::var(CONNECT_GRPC_URL_ENV)
        .ok()
        .or_else(|| std::env::var(LEGACY_GRPC_URL_ENV).ok())
        .and_then(|v| normalize_grpc_url(&v, true))
}

#[cfg(test)]
mod tests {
    use super::{
        coincube_grpc_url, default_connect_grpc_tls, grpc_url_or_baked_default,
        is_configured_base_url, normalize_grpc_url, same_base_url, CoincubeServiceConfig,
    };

    /// The service config is fetched from the base URL the caller gives, so
    /// a Cube whose session is on another endpoint asks that endpoint.
    #[tokio::test]
    async fn the_grpc_address_is_fetched_from_the_given_base_url() {
        let server = httpmock::MockServer::start_async().await;
        let config = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::GET)
                    .path("/api/v1/connect/service-config");
                then.status(200).json_body(serde_json::json!({
                    "grpcUrl": "grpc.example.test:50051",
                    "tls": false
                }));
            })
            .await;

        // A trailing slash on the base URL must not double the path's slash.
        for base in [server.base_url(), format!("{}/", server.base_url())] {
            assert_eq!(
                coincube_grpc_url(&base).await.as_deref(),
                Some("http://grpc.example.test:50051")
            );
        }
        config.assert_hits_async(2).await;
        assert!(!is_configured_base_url(&server.base_url()));
    }

    /// The baked default is the configured API's gRPC address. An endpoint
    /// that is not the configured one never gets it.
    #[test]
    fn the_baked_default_is_a_fallback_for_the_configured_endpoint_only() {
        let baked = || Some("https://baked.example.test:443".to_string());
        let fetched = Some("https://fetched.example.test:443".to_string());

        assert_eq!(
            grpc_url_or_baked_default(fetched.clone(), true, baked),
            fetched
        );
        assert_eq!(
            grpc_url_or_baked_default(fetched.clone(), false, baked),
            fetched
        );
        assert_eq!(
            grpc_url_or_baked_default(None, true, baked).as_deref(),
            Some("https://baked.example.test:443")
        );
        assert_eq!(grpc_url_or_baked_default(None, false, baked), None);
    }

    #[test]
    fn base_urls_compare_without_their_trailing_slash() {
        assert!(same_base_url(
            "https://api.example.test",
            "https://api.example.test/"
        ));
        assert!(!same_base_url(
            "https://api.example.test",
            "https://other.example.test"
        ));
        assert!(is_configured_base_url(
            &crate::services::coincube_api_base_url()
        ));
    }

    #[test]
    fn normalize_adds_https_when_tls() {
        assert_eq!(
            normalize_grpc_url("grpc.coincube.io:443", true).as_deref(),
            Some("https://grpc.coincube.io:443")
        );
    }

    #[test]
    fn normalize_adds_http_when_not_tls() {
        assert_eq!(
            normalize_grpc_url("localhost:50051", false).as_deref(),
            Some("http://localhost:50051")
        );
    }

    #[test]
    fn normalize_keeps_existing_scheme_and_trims_slash() {
        assert_eq!(
            normalize_grpc_url("https://grpc.coincube.io:443/", false).as_deref(),
            Some("https://grpc.coincube.io:443")
        );
        assert_eq!(
            normalize_grpc_url("http://host:1/", true).as_deref(),
            Some("http://host:1")
        );
    }

    #[test]
    fn normalize_rejects_empty() {
        assert_eq!(normalize_grpc_url("   ", true), None);
        assert_eq!(normalize_grpc_url("", false), None);
    }

    #[test]
    fn service_config_defaults_missing_tls_to_secure() {
        let config: CoincubeServiceConfig =
            serde_json::from_str(r#"{"grpcUrl":"grpc.coincube.io:443"}"#).unwrap();

        assert!(config.tls);
        assert!(default_connect_grpc_tls());
    }
}
