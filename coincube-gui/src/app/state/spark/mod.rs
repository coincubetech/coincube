//! Spark wallet panel state machines.
//!
//! One module per Menu::Spark entry — Overview, Send, Receive,
//! Transactions, Settings. Each holds an `Option<Arc<SparkBackend>>`
//! (None when the cube has no Spark signer or the bridge subprocess
//! failed to spawn) and renders an "unavailable" stub in that case.

pub mod cross_chain;
pub mod esplora;
pub mod overview;
pub mod receive;
pub mod send;
pub mod settings;
pub mod sideshift_receive;
pub mod swap;
pub mod transactions;

pub use overview::SparkOverview;
pub use receive::{SparkReceive, SparkReceiveMethod, SparkReceivePhase};
pub use send::{SparkSend, SparkSendPhase};
pub use settings::SparkSettings;
pub use transactions::SparkTransactions;

use std::sync::Arc;

use coincube_spark_protocol::{PaymentSummary, UsdbBalanceSnapshot};
use iced::Task;

use crate::app::cache::Cache;
use crate::app::message::Message;
use crate::app::wallets::SparkBackend;

/// The latest five of `payments` as Send, Receive and Swap list them in their
/// side panel, priced at the cache's fiat rate.
pub(crate) fn recent_transactions_from(
    payments: &[PaymentSummary],
    cache: &Cache,
) -> Vec<crate::app::view::spark::SparkRecentTransaction> {
    use std::convert::TryInto;
    let fiat: Option<crate::app::view::FiatAmountConverter> =
        cache.fiat_price.as_ref().and_then(|p| p.try_into().ok());
    payments
        .iter()
        .take(5)
        .map(|p| overview::payment_summary_to_recent_tx(p, fiat.as_ref()))
        .collect()
}

/// A completed incoming Lightning payment, for tests of the panels that list
/// payments.
#[cfg(test)]
pub(crate) fn test_payment(id: &str, amount_sat: i64) -> PaymentSummary {
    PaymentSummary {
        id: id.to_string(),
        amount_sat,
        fees_sat: 0,
        token_amount: None,
        token_decimals: None,
        token_ticker: None,
        timestamp: 1_700_000_000,
        status: "completed".to_string(),
        direction: "incoming".to_string(),
        method: "lightning".to_string(),
        description: None,
        details: Default::default(),
    }
}

/// Fire a `list_payments` RPC on the Spark bridge and route the result
/// through caller-supplied message constructors. Shared by the Send
/// and Receive panels so the "Last transactions" feed stays identical
/// across wallet screens — callers differ only in which panel message
/// they wrap the response in.
pub(crate) fn fetch_payments_task(
    backend: Option<Arc<SparkBackend>>,
    on_loaded: impl FnOnce(Vec<PaymentSummary>) -> Message + Send + 'static,
    on_failed: impl FnOnce(String) -> Message + Send + 'static,
) -> Task<Message> {
    let Some(backend) = backend else {
        return Task::none();
    };
    Task::perform(
        async move { backend.list_payments(Some(20), None).await },
        move |result| match result {
            Ok(list) => on_loaded(list.payments),
            Err(e) => on_failed(e.to_string()),
        },
    )
}

/// Fetch the Spark wallet balance via `get_info`, for the two-card
/// "YOU SEND / YOU RECEIVE" balance line: the BTC balance plus the validated
/// USDB holding (`usdb_holding`), which the caller folds into a sats total with
/// [`unified_spark_balance_sats`]. Best-effort UI polish — an error resolves to
/// `None` so the caller keeps its last value rather than surfacing a failure.
pub(crate) fn fetch_balance_task(
    backend: Option<Arc<SparkBackend>>,
    on_result: impl FnOnce(Option<(u64, Option<UsdbBalanceSnapshot>)>) -> Message + Send + 'static,
) -> Task<Message> {
    let Some(backend) = backend else {
        return Task::none();
    };
    Task::perform(async move { backend.get_info().await }, move |result| {
        on_result(result.ok().map(|info| {
            (
                info.balance_sats,
                crate::app::breez_spark::assets::usdb_holding(&info),
            )
        }))
    })
}

/// The unified Spark balance in sats: the BTC balance plus any Stable Balance
/// (USDB) holding, converted at the cache's BTC/USD reference price. Shared by
/// the Home card and the Send/Receive card subtitles so every screen shows the
/// same figure — Stable Balance auto-sweeps BTC into USDB, so a BTC-only
/// reading would understate a funded wallet.
pub(crate) fn unified_spark_balance_sats(
    btc_sats: u64,
    stable: Option<&UsdbBalanceSnapshot>,
    cache: &Cache,
) -> u64 {
    let reference_price = reference_btc_usd_price(cache);
    let usdb_as_sats = stable
        .map(|sb| {
            crate::app::breez_spark::assets::stable_token_as_sats_u128(
                sb.balance,
                sb.decimals,
                reference_price,
            )
        })
        .unwrap_or(0);
    btc_sats.saturating_add(usdb_as_sats)
}

/// The BTC/USD price used to value USD-pegged tokens (USDB / USDT / USDC) in
/// sats. The app always fetches a USD price for exactly this, regardless of the
/// user's display fiat (see the `gui` loop's "Always fetch BTC/USD" path), so
/// this just reads `cache.btc_usd_price`. `None` only in the brief window before
/// that price lands (or if it fails); callers then show the token amount without
/// a sats figure rather than valuing it at a non-USD rate — the user-fiat price
/// (BTC/EUR, …) is *not* BTC/USD and would misprice a USD-pegged token.
pub(crate) fn reference_btc_usd_price(cache: &Cache) -> Option<f64> {
    cache.btc_usd_price
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_spark_protocol::UsdbBalanceSnapshot;

    fn usdb_balance(balance: u64, decimals: u32) -> UsdbBalanceSnapshot {
        UsdbBalanceSnapshot {
            token_identifier: "btkn1synthetic".into(),
            balance: balance.into(),
            decimals,
            ticker: "USDB".to_string(),
        }
    }

    #[test]
    fn reference_btc_usd_price_reads_cache_usd_price() {
        let mut cache = Cache::default();
        assert_eq!(reference_btc_usd_price(&cache), None);

        cache.btc_usd_price = Some(63_000.0);
        assert_eq!(reference_btc_usd_price(&cache), Some(63_000.0));
    }

    #[test]
    fn unified_spark_balance_uses_btc_balance_without_reference_price() {
        let cache = Cache::default();
        let stable = usdb_balance(2_500_000, 6);

        assert_eq!(
            unified_spark_balance_sats(42_000, Some(&stable), &cache),
            42_000
        );
    }

    #[test]
    fn unified_spark_balance_folds_stable_balance_using_usd_reference_price() {
        let cache = Cache {
            btc_usd_price: Some(50_000.0),
            ..Cache::default()
        };
        let stable = usdb_balance(2_500_000, 6);

        assert_eq!(
            unified_spark_balance_sats(123, Some(&stable), &cache),
            5_123
        );
    }

    #[test]
    fn unified_spark_balance_saturates_when_stable_value_overflows() {
        let cache = Cache {
            btc_usd_price: Some(100_000.0),
            ..Cache::default()
        };
        let stable = usdb_balance(1_000_000, 6);

        assert_eq!(
            unified_spark_balance_sats(u64::MAX - 2, Some(&stable), &cache),
            u64::MAX
        );
    }
}
