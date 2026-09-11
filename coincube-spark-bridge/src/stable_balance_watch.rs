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

/// Decide whether one log record is the SDK's auto-conversion failure.
/// Returns the reason text (the part after the prefix, cleaned up for
/// display) when it is.
pub fn classify(target: &str, level: Level, message: &str) -> Option<String> {
    if level != Level::WARN || !target.starts_with(SDK_TARGET_PREFIX) {
        return None;
    }
    let rest = message.strip_prefix(FAILURE_PREFIX)?;
    Some(display_reason(rest))
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

/// A `tracing` layer that reports each SDK auto-conversion failure over
/// `tx`. Installed alongside the `fmt` layer in `main`, so the warning
/// still reaches stderr as before.
pub struct FailureWatchLayer {
    tx: UnboundedSender<String>,
}

impl FailureWatchLayer {
    pub fn new(tx: UnboundedSender<String>) -> Self {
        Self { tx }
    }
}

impl<S: Subscriber> Layer<S> for FailureWatchLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // Cheap pre-filter before visiting fields: the SDK logs at high
        // volume and this runs on every record.
        if *meta.level() != Level::WARN || !meta.target().starts_with(SDK_TARGET_PREFIX) {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        if let Some(reason) = classify(meta.target(), *meta.level(), &visitor.message) {
            // A closed receiver means the server loop is gone; nothing to do.
            let _ = self.tx.send(reason);
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
            )
            .as_deref(),
            Some("Pool has no liquidity")
        );
    }

    #[test]
    fn classify_ignores_other_levels_targets_and_messages() {
        let target = "breez_sdk_spark::stable_balance::queue";
        assert_eq!(classify(target, Level::INFO, SDK_MESSAGE), None);
        assert_eq!(
            classify("breez_sdk_spark::sdk", Level::WARN, SDK_MESSAGE),
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
            )
            .as_deref(),
            Some("Timeout")
        );
        assert_eq!(
            classify(
                "breez_sdk_spark::stable_balance::queue",
                Level::WARN,
                "Auto-conversion failed"
            )
            .as_deref(),
            Some("unknown error")
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
    fn layer_forwards_only_the_sdk_failure_warning() {
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
        });

        assert_eq!(rx.try_recv().ok().as_deref(), Some("Pool has no liquidity"));
        assert!(
            rx.try_recv().is_err(),
            "only the failure warning is forwarded"
        );
    }
}
