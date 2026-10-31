//! Thin wrapper around [`breez_sdk_spark::BreezSdk`] with a coincube-friendly
//! constructor and an `Arc`-friendly handle.
//!
//! Everything the bridge's server loop (or the smoke-test harness) needs
//! from the SDK goes through here so the SDK's type surface doesn't leak
//! into the JSON-RPC layer.

use std::sync::Arc;

use breez_sdk_spark::{
    default_config, BreezSdk, CrossChainConfig, MaxFee, Network as SparkNetwork, SdkBuilder, Seed,
};

/// Mainnet USDB token identifier. Published by Breez in the Stable
/// Balance guide at
/// <https://sdk-doc-spark.breez.technology/guide/stable_balance.html>.
/// Using a hardcoded constant rather than an env var is deliberate:
/// the identifier is stable across deployments and leaking the
/// knob to ops would just mean a way to silently misconfigure a
/// production wallet.
pub const USDB_MAINNET_TOKEN_IDENTIFIER: &str =
    "btkn1xgrvjwey5ngcagvap2dzzvsy4uk8ua9x69k82dwvt5e7ef9drm9qztux87";

/// Default LNURL domain for production builds. The Breez-hosted LNURL
/// server issues Lightning Addresses of the form `<username>@<this>`.
/// Override at launch time via `COINCUBE_LNURL_DOMAIN` when pointing
/// staging builds at an alternate allowlisted domain.
const DEFAULT_LNURL_DOMAIN: &str = "coincube.io";

fn configured_lnurl_domain(raw: Option<String>) -> String {
    raw.map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_LNURL_DOMAIN.to_string())
}

/// Cloneable SDK handle. The inner [`BreezSdk`] is `Send + Sync`, so the
/// bridge can freely share it across async tasks serving different
/// JSON-RPC requests concurrently.
#[derive(Clone)]
pub struct SdkHandle {
    pub sdk: Arc<BreezSdk>,
}

/// Build a mainnet Spark SDK config with the given API key.
///
/// SDK Stable Balance stays unconfigured (#950 S2b): auto-convert is the
/// bridge's own ([`crate::auto_convert`]). Without a
/// `stable_balance_config` the SDK builds no Stable Balance at all
/// (`build_stable_balance` in `sdk_builder.rs` at the pin), so it never
/// sweeps bitcoin, never reads a Stable Balance setting or pending
/// deactivation it saved before, and never converts the USDB balance back
/// to bitcoin. Those saved entries stay in its storage, unread.
pub fn mainnet_config(api_key: String) -> breez_sdk_spark::Config {
    let mut config = default_config(SparkNetwork::Mainnet);
    config.api_key = Some(api_key);
    config.stable_balance_config = None;
    // Treat an unset / empty / whitespace-only env var as "not
    // configured" and fall back to the default. An empty string
    // would otherwise be handed to the SDK as a valid domain and
    // produce nonsense Lightning Addresses like `user@`.
    config.lnurl_domain = Some(configured_lnurl_domain(
        std::env::var("COINCUBE_LNURL_DOMAIN").ok(),
    ));
    // Cross-chain stablecoin send (USDT/USDC to EVM/Solana/Tron). Opt-in: the
    // providers (Orchestra, Boltz) run background work such as websockets, so
    // the SDK leaves them off unless this is set. Mainnet-only, which is all
    // this config builds.
    //
    // Both bounds are left at the SDK defaults — 100 bps (1%) slippage and a
    // 15 bps overpay pad — because they're what the gui's advanced disclosure
    // is calibrated around. A per-send override still rides on the prepare
    // request; this is only the fallback.
    config.cross_chain_config = Some(CrossChainConfig {
        default_slippage_bps: None,
        default_target_overpay_bps: None,
    });
    // The SDK's `default_config` caps the *background* deposit auto-claim at
    // `Rate { sat_per_vbyte: 1 }` (see `default_config` in the SDK). At 1
    // sat/vbyte the claim tx can't confirm at any realistic mempool level, so
    // every mature on-chain deposit — SideShift swap settlements included —
    // fails its auto-claim with `MaxDepositClaimFeeExceeded` and gets parked in
    // the "Pending deposits" list until the user manually hits Retry (which
    // *does* work, because `claim_deposit` overrides the cap per request).
    //
    // Raise the config cap to the same adaptive policy the manual path uses so
    // deposits claim themselves: `NetworkRecommended` tracks the mempool's
    // fastest-fee estimate plus a small leeway, which comfortably covers the
    // few-sat/vbyte a static-deposit claim actually needs.
    config.max_deposit_claim_fee = Some(MaxFee::NetworkRecommended {
        leeway_sat_per_vbyte: 5,
    });
    config
}

/// Connect to Spark mainnet with the given mnemonic.
///
/// `storage_dir` must be a writable directory — the SDK uses it for its
/// internal sqlite database, so picking the same dir twice from two
/// processes will collide.
pub async fn connect_mainnet(
    api_key: String,
    mnemonic: String,
    passphrase: Option<String>,
    storage_dir: String,
) -> anyhow::Result<SdkHandle> {
    let config = mainnet_config(api_key);
    let seed = Seed::Mnemonic {
        mnemonic,
        passphrase,
    };
    // 0.19.0 replaced the free `connect(ConnectRequest)` fn with `SdkBuilder`.
    // `with_default_storage` is the builder's equivalent of the old
    // `ConnectRequest::storage_dir` — same sqlite store, same directory.
    let sdk = SdkBuilder::new(config, seed)
        .with_default_storage(storage_dir)
        .build()
        .await?;
    Ok(SdkHandle { sdk: Arc::new(sdk) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_lnurl_domain_falls_back_for_missing_or_blank_values() {
        assert_eq!(configured_lnurl_domain(None), DEFAULT_LNURL_DOMAIN);
        assert_eq!(
            configured_lnurl_domain(Some(String::new())),
            DEFAULT_LNURL_DOMAIN
        );
        assert_eq!(
            configured_lnurl_domain(Some(" \t\n ".to_string())),
            DEFAULT_LNURL_DOMAIN
        );
    }

    #[test]
    fn configured_lnurl_domain_trims_custom_domain() {
        assert_eq!(
            configured_lnurl_domain(Some(" staging.coincube.io ".to_string())),
            "staging.coincube.io"
        );
    }

    /// SDK Stable Balance is never configured: with it, turning the
    /// feature off converts the whole USDB balance back to bitcoin.
    #[test]
    fn mainnet_config_leaves_stable_balance_off_and_wires_claim_fee_defaults() {
        let config = mainnet_config("api-key".to_string());

        assert_eq!(config.api_key.as_deref(), Some("api-key"));
        assert!(config.stable_balance_config.is_none());

        let cross_chain = config
            .cross_chain_config
            .as_ref()
            .expect("cross-chain config should be enabled");
        assert!(cross_chain.default_slippage_bps.is_none());
        assert!(cross_chain.default_target_overpay_bps.is_none());

        assert!(matches!(
            config.max_deposit_claim_fee,
            Some(MaxFee::NetworkRecommended {
                leeway_sat_per_vbyte: 5
            })
        ));
    }
}
