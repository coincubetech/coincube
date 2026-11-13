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
    if let Some(url) = reject_plaintext_in_release(runtime_grpc_url_override()) {
        return Some(url);
    }
    coincube_grpc_url()
        .await
        .or_else(|| reject_plaintext_in_release(buildtime_grpc_url_default()))
}

/// Fetch the Connect signing gRPC URL from coincube-api. Best-effort: any
/// network/parse failure yields `None` so the caller can fall back. The
/// returned URL is normalized to a tonic-ready form (`https://…` when TLS,
/// `http://…` otherwise) so `create_channel` enables TLS correctly.
async fn coincube_grpc_url() -> Option<String> {
    let base = crate::services::coincube_api_base_url();
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
    use super::{default_connect_grpc_tls, normalize_grpc_url, CoincubeServiceConfig};

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
