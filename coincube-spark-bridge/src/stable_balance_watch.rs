//! Circuit breaker for the Spark SDK's Stable Balance auto-conversion loop.
//!
//! The SDK's `AutoConvert` worker has no backoff: when a BTC→USDB swap
//! fails at the AMM, the sats are refunded as an incoming Spark transfer,
//! that refund is a sats receive, and the receive re-queues the same
//! conversion (`breez-sdk/core/src/stable_balance/queue.rs`, SDK 0.19.0).
//! Seen in the wild as one failed attempt every ~6 s for as long as the
//! app runs, each leaving a ±N-sat pair in the payment history. The SDK
//! emits no event for the failure — only a `WARN` log line — so the only
//! way to notice from outside is to listen to that log line.
//!
//! [`FailureWatchLayer`] is a `tracing` layer that forwards the message of
//! every matching warning over a channel. [`FailureTracker`] counts them and
//! reports when the threshold is reached; the server then deactivates Stable
//! Balance (the only lever that stops the loop) and tells the gui.

use tokio::sync::mpsc::UnboundedSender;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

pub use coincube_spark_protocol::STABLE_BALANCE_PAUSE_THRESHOLD;

/// Log target of the SDK's conversion worker. Matched by prefix so a
/// future SDK that moves the warning to a sibling module in the same
/// subsystem still trips the breaker.
const SDK_TARGET_PREFIX: &str = "breez_sdk_spark::stable_balance";

/// The worker's failure line, verbatim from the SDK
/// (`warn!("Auto-conversion failed: {e:?}")`).
const FAILURE_PREFIX: &str = "Auto-conversion failed";

/// The SDK's success lines, verbatim (`info!("Auto-conversion completed:
/// converted ...")` in the worker, and the per-receive variant that runs
/// the same AMM swap for a single incoming payment). Either one proves the
/// swap works again, which is what clears the failure run.
const SUCCESS_PREFIXES: [&str; 2] = [
    "Auto-conversion completed",
    "Per-receive conversion completed",
];

/// One conversion attempt the SDK reported through its logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversionOutcome {
    /// The swap failed; carries the reason text, cleaned up for display.
    Failed(String),
    /// The swap went through.
    Succeeded,
}

/// Decide whether one log record is the SDK reporting a conversion
/// outcome — the failure warning, or one of the success lines.
pub fn classify(target: &str, level: Level, message: &str) -> Option<ConversionOutcome> {
    if !target.starts_with(SDK_TARGET_PREFIX) {
        return None;
    }
    match level {
        Level::WARN => message
            .strip_prefix(FAILURE_PREFIX)
            .map(|rest| ConversionOutcome::Failed(display_reason(rest))),
        Level::INFO => SUCCESS_PREFIXES
            .iter()
            .any(|prefix| message.starts_with(prefix))
            .then_some(ConversionOutcome::Succeeded),
        _ => None,
    }
}

/// Trim the Rust `Debug` scaffolding off the SDK's error so the gui can
/// show it as a sentence: `: ConversionFailed("Convert token failed,
/// refund in progress: Pool has no liquidity")` → `Pool has no liquidity`.
/// Falls back to the raw text when the shape isn't recognised — an
/// unfamiliar error is still better shown than hidden.
fn display_reason(rest: &str) -> String {
    let raw = rest.trim_start_matches(':').trim();
    let inner = raw
        .find('(')
        .zip(raw.rfind(')'))
        .filter(|(open, close)| open < close)
        .map(|(open, close)| raw[open + 1..close].trim_matches('"'))
        .unwrap_or(raw);
    // The SDK nests its causes with ": " — the last segment is the one a
    // person can act on ("Pool has no liquidity").
    let leaf = inner.rsplit(": ").next().unwrap_or(inner).trim();
    match (leaf.is_empty(), raw.is_empty()) {
        (false, _) => leaf.to_string(),
        (true, false) => raw.to_string(),
        (true, true) => "unknown error".to_string(),
    }
}

/// A `tracing` layer that reports each SDK conversion outcome over `tx`.
/// Installed alongside the `fmt` layer in `main`, so the lines still
/// reach stderr as before.
pub struct FailureWatchLayer {
    tx: UnboundedSender<ConversionOutcome>,
}

impl FailureWatchLayer {
    pub fn new(tx: UnboundedSender<ConversionOutcome>) -> Self {
        Self { tx }
    }
}

impl<S: Subscriber> Layer<S> for FailureWatchLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // Cheap pre-filter before visiting fields: the SDK logs at high
        // volume and this runs on every record.
        let level = *meta.level();
        if (level != Level::WARN && level != Level::INFO)
            || !meta.target().starts_with(SDK_TARGET_PREFIX)
        {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        if let Some(outcome) = classify(meta.target(), level, &visitor.message) {
            // A closed receiver means the server loop is gone; nothing to do.
            let _ = self.tx.send(outcome);
        }
    }
}

/// Captures the `message` field of a `tracing` event as a string.
#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }
}

/// Counts consecutive auto-conversion failures and says when to pause.
/// A successful conversion ([`Self::record_success`]) clears the run, so
/// only failures with no success in between count toward the threshold.
#[derive(Debug, Default)]
pub struct FailureTracker {
    consecutive: u32,
}

/// What the server should do after one more failure was recorded.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Under the threshold — keep watching.
    Tolerate { failures: u32 },
    /// Threshold reached: deactivate Stable Balance and tell the gui.
    /// The counter is reset so a re-enabled feature starts clean.
    Pause { failures: u32 },
}

impl FailureTracker {
    pub fn record_failure(&mut self) -> Verdict {
        self.consecutive = self.consecutive.saturating_add(1);
        if self.consecutive >= STABLE_BALANCE_PAUSE_THRESHOLD {
            let failures = self.consecutive;
            self.consecutive = 0;
            Verdict::Pause { failures }
        } else {
            Verdict::Tolerate {
                failures: self.consecutive,
            }
        }
    }

    /// The user (re-)enabled Stable Balance: forget the history so the
    /// breaker gives the new session the full threshold.
    pub fn reset(&mut self) {
        self.consecutive = 0;
    }

    /// A conversion went through: the run of failures is over. Returns the
    /// number of failures that were forgotten, for the log line.
    pub fn record_success(&mut self) -> u32 {
        std::mem::take(&mut self.consecutive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    const SDK_MESSAGE: &str = "Auto-conversion failed: ConversionFailed(\"Convert token failed, \
                               refund in progress: Pool has no liquidity\")";

    #[test]
    fn classify_extracts_the_actionable_reason_from_the_sdk_warning() {
        assert_eq!(
            classify(
                "breez_sdk_spark::stable_balance::queue",
                Level::WARN,
                SDK_MESSAGE
            ),
            Some(ConversionOutcome::Failed("Pool has no liquidity".into()))
        );
    }

    #[test]
    fn classify_recognises_the_sdk_success_lines() {
        let target = "breez_sdk_spark::stable_balance::conversions";
        assert_eq!(
            classify(
                target,
                Level::INFO,
                "Auto-conversion completed: converted 1000 sats (sent_payment_id=a, \
                 received_payment_id=b)"
            ),
            Some(ConversionOutcome::Succeeded)
        );
        assert_eq!(
            classify(
                target,
                Level::INFO,
                "Per-receive conversion completed: converted 10 sats for p (sent=a, received=b)"
            ),
            Some(ConversionOutcome::Succeeded)
        );
        // Triggered is not completed.
        assert_eq!(
            classify(
                target,
                Level::INFO,
                "Auto-conversion triggered: converting 1000 sats to USDB"
            ),
            None
        );
    }

    #[test]
    fn classify_ignores_other_levels_targets_and_messages() {
        let target = "breez_sdk_spark::stable_balance::queue";
        assert_eq!(classify(target, Level::INFO, SDK_MESSAGE), None);
        assert_eq!(
            classify(target, Level::DEBUG, "Auto-conversion completed: x"),
            None
        );
        assert_eq!(
            classify("breez_sdk_spark::sdk", Level::WARN, SDK_MESSAGE),
            None
        );
        assert_eq!(
            classify(
                "breez_sdk_spark::sdk",
                Level::INFO,
                "Auto-conversion completed: x"
            ),
            None
        );
        assert_eq!(
            classify(target, Level::WARN, "Deactivation conversion failed: x"),
            None
        );
    }

    #[test]
    fn classify_keeps_unrecognised_error_shapes_readable() {
        assert_eq!(
            classify(
                "breez_sdk_spark::stable_balance::queue",
                Level::WARN,
                "Auto-conversion failed: Timeout"
            ),
            Some(ConversionOutcome::Failed("Timeout".into()))
        );
        assert_eq!(
            classify(
                "breez_sdk_spark::stable_balance::queue",
                Level::WARN,
                "Auto-conversion failed"
            ),
            Some(ConversionOutcome::Failed("unknown error".into()))
        );
    }

    #[test]
    fn tracker_pauses_at_the_threshold_and_resets() {
        let mut tracker = FailureTracker::default();
        for expected in 1..STABLE_BALANCE_PAUSE_THRESHOLD {
            assert_eq!(
                tracker.record_failure(),
                Verdict::Tolerate { failures: expected }
            );
        }
        assert_eq!(
            tracker.record_failure(),
            Verdict::Pause {
                failures: STABLE_BALANCE_PAUSE_THRESHOLD
            }
        );
        // Counter restarted after the pause.
        assert_eq!(tracker.record_failure(), Verdict::Tolerate { failures: 1 });
    }

    #[test]
    fn tracker_reset_forgets_history() {
        let mut tracker = FailureTracker::default();
        tracker.record_failure();
        tracker.record_failure();
        tracker.reset();
        assert_eq!(tracker.record_failure(), Verdict::Tolerate { failures: 1 });
    }

    #[test]
    fn tracker_success_clears_the_run_so_only_consecutive_failures_count() {
        let mut tracker = FailureTracker::default();
        for _ in 1..STABLE_BALANCE_PAUSE_THRESHOLD {
            tracker.record_failure();
        }
        assert_eq!(tracker.record_success(), STABLE_BALANCE_PAUSE_THRESHOLD - 1);
        assert_eq!(tracker.record_success(), 0);
        // One more failure after a success is a fresh run, not the pause.
        assert_eq!(tracker.record_failure(), Verdict::Tolerate { failures: 1 });
    }

    #[test]
    fn layer_forwards_only_the_sdk_conversion_outcomes() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let subscriber = tracing_subscriber::registry().with(FailureWatchLayer::new(tx));

        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "breez_sdk_spark::stable_balance::queue", "{}", SDK_MESSAGE);
            tracing::info!(target: "breez_sdk_spark::stable_balance::queue", "{}", SDK_MESSAGE);
            tracing::warn!(target: "spark_wallet::wallet", "Failed to select leaves");
            tracing::warn!(
                target: "breez_sdk_spark::stable_balance::queue",
                "Deactivation conversion failed: Timeout"
            );
            tracing::info!(
                target: "breez_sdk_spark::stable_balance::conversions",
                "Auto-conversion completed: converted 1000 sats (sent_payment_id=a, \
                 received_payment_id=b)"
            );
            tracing::debug!(
                target: "breez_sdk_spark::stable_balance::queue",
                "Conversion worker: auto-convert done (converted=true)"
            );
        });

        assert_eq!(
            rx.try_recv().ok(),
            Some(ConversionOutcome::Failed("Pool has no liquidity".into()))
        );
        assert_eq!(rx.try_recv().ok(), Some(ConversionOutcome::Succeeded));
        assert!(
            rx.try_recv().is_err(),
            "only the failure warning and the success lines are forwarded"
        );
    }
}
