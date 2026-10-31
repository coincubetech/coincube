//! Bridge-driven auto-convert (#950 S2b): incoming bitcoin is swapped to USDB
//! by the bridge itself, with the SDK's Stable Balance left unconfigured.
//!
//! Why not Stable Balance: turning it on converts the existing bitcoin
//! balance, and turning it off (`StableBalanceActiveLabel::Unset`) queues the
//! whole USDB balance back to bitcoin (`stable_balance/mod.rs` at the SDK
//! pin). Here, off keeps USDB as USDB and only stops converting new bitcoin.
//! So [`crate::sdk_adapter::mainnet_config`] configures no Stable Balance,
//! the bridge never sends an `update_user_settings` label, and:
//!
//! - every successful incoming bitcoin payment ([`eligible_receive`]) adds its
//!   sats to the pending total;
//! - one worker ([`process`]) converts the pending sats, bounded by the
//!   bitcoin balance, once they reach the pool's minimum, with a
//!   self-transfer to the wallet's own Spark address ([`prepare_request`]);
//! - three failed conversions in a row turn auto-convert off and pause it
//!   ([`FailureTracker`]).
//!
//! The switch, the breaker and the pending sats persist per Cube in
//! [`STATE_FILE`], in the Cube's Spark storage directory.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use breez_sdk_spark::{
    ConversionOptions, ConversionType, FetchConversionLimitsRequest, GetInfoRequest, Payment,
    PaymentDetails, PaymentMethod, PaymentRequest, PaymentStatus, PaymentType,
    PrepareSendPaymentRequest, PrepareSendPaymentResponse, ReceivePaymentMethod,
    ReceivePaymentRequest, SendPaymentMethod, SendPaymentRequest,
};
use coincube_spark_protocol::{
    AutoConvertBreaker, AutoConvertState, Event as ProtocolEvent, CONVERSION_MAX_SLIPPAGE_BPS,
    STABLE_BALANCE_PAUSE_THRESHOLD,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OnceCell};

use crate::sdk_adapter::{SdkHandle, USDB_MAINNET_TOKEN_IDENTIFIER};

/// The state file, in the Cube's Spark storage directory.
pub const STATE_FILE: &str = "coincube-auto-convert.json";

/// Format of [`STATE_FILE`]. A file of any other version loads as off.
const STATE_VERSION: u32 = 1;

/// USDB target, in token base units, for the quote that prices a conversion
/// when the pool states no minimum output. Only the ratio it returns is used,
/// so it just has to be quotable.
const PROBE_TARGET_UNITS: u128 = 1_000_000;

/// Quotes allowed after the price probe. Each one after the first shrinks the
/// target by how far the previous quote overshot the received sats.
const SIZING_ROUNDS: usize = 3;

/// Receives remembered so a repeated `PaymentSucceeded` isn't counted twice.
const SEEN_PAYMENTS: usize = 256;

/// What [`STATE_FILE`] holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Saved {
    version: u32,
    enabled: bool,
    breaker: AutoConvertBreaker,
    /// Sats received while on and not converted yet: below the pool's minimum
    /// so far, or left over from the last conversion.
    #[serde(default)]
    pending_sats: u64,
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            enabled: false,
            breaker: AutoConvertBreaker::Ready,
            pending_sats: 0,
        }
    }
}

/// Read [`STATE_FILE`]. A missing file is a Cube that never turned
/// auto-convert on. An unreadable or unrecognised one loads as off: the safe
/// default, since off never moves funds.
fn load(path: &Path) -> Saved {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Saved::default(),
        Err(e) => {
            tracing::warn!(
                "could not read {}: {e}; auto-convert is off",
                path.display()
            );
            return Saved::default();
        }
    };
    match serde_json::from_slice::<Saved>(&bytes) {
        Ok(saved) if saved.version == STATE_VERSION => saved,
        Ok(saved) => {
            tracing::warn!(
                "{} has version {}, not {STATE_VERSION}; auto-convert is off",
                path.display(),
                saved.version
            );
            Saved::default()
        }
        Err(e) => {
            tracing::warn!(
                "could not parse {}: {e}; auto-convert is off",
                path.display()
            );
            Saved::default()
        }
    }
}

/// Write [`STATE_FILE`] in full: a temporary file, synced, then renamed over
/// the old one, so a crash leaves either the old state or the new one.
fn save(path: &Path, saved: &Saved) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let write = || -> io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(saved).map_err(io::Error::other)?)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    };
    if let Err(e) = write() {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    // The rename is what counts. Syncing the directory makes it durable on
    // filesystems that need it; a failure there doesn't undo the write.
    if let Some(dir) = path.parent() {
        let _ = fs::File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
}

/// Counts consecutive failed conversions and says when to pause.
/// A successful conversion ([`Self::record_success`]) clears the run, so
/// only failures with no success in between count toward the threshold.
#[derive(Debug, Default)]
pub struct FailureTracker {
    consecutive: u32,
}

/// What to do after one more failure was recorded.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Under the threshold: keep converting.
    Tolerate { failures: u32 },
    /// Threshold reached: pause auto-convert. The counter is reset so a
    /// re-enabled feature starts clean.
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

    /// Auto-convert was (re-)enabled: forget the history so the breaker
    /// gives the new session the full threshold.
    pub fn reset(&mut self) {
        self.consecutive = 0;
    }

    /// A conversion went through: the run of failures is over. Returns the
    /// number of failures that were forgotten, for the log line.
    pub fn record_success(&mut self) -> u32 {
        std::mem::take(&mut self.consecutive)
    }
}

/// The most recent receives counted, oldest first.
#[derive(Debug, Default)]
struct SeenPayments {
    order: VecDeque<String>,
    ids: HashSet<String>,
}

impl SeenPayments {
    /// `true` the first time `id` is seen.
    fn insert(&mut self, id: &str) -> bool {
        if !self.ids.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        if self.order.len() > SEEN_PAYMENTS {
            if let Some(oldest) = self.order.pop_front() {
                self.ids.remove(&oldest);
            }
        }
        true
    }
}

/// One incoming bitcoin payment that counts toward auto-convert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub payment_id: String,
    pub sats: u64,
}

/// The sats a payment adds to auto-convert, or `None` when it doesn't count.
///
/// Only a completed incoming bitcoin payment counts. That is the SDK's own
/// per-receive rule (`stable_balance::should_trigger_per_receive` at the
/// pin), made stricter: a payment carrying any conversion information is
/// left out, not only an AMM leg. So none of these can start a conversion:
/// a send, a token receive (the USDB a conversion delivers), a conversion's
/// own legs, a refunded conversion, or a cross-chain leg.
pub fn eligible_receive(payment: &Payment) -> Option<Receipt> {
    if payment.payment_type != PaymentType::Receive
        || payment.status != PaymentStatus::Completed
        || payment.method == PaymentMethod::Token
        || payment.is_conversion_child()
        || carries_conversion(payment.details.as_ref())
    {
        return None;
    }
    let sats = u64::try_from(payment.amount)
        .ok()
        .filter(|sats| *sats > 0)?;
    Some(Receipt {
        payment_id: payment.id.clone(),
        sats,
    })
}

fn carries_conversion(details: Option<&PaymentDetails>) -> bool {
    matches!(
        details,
        Some(
            PaymentDetails::Spark {
                conversion_info: Some(_),
                ..
            } | PaymentDetails::Token {
                conversion_info: Some(_),
                ..
            } | PaymentDetails::Lightning {
                conversion_info: Some(_),
                ..
            }
        )
    )
}

/// The conversion options on every auto-convert request: bitcoin to the
/// token named by the request, with the slippage cap the gui shows.
pub fn conversion_options() -> ConversionOptions {
    ConversionOptions {
        conversion_type: ConversionType::FromBitcoin,
        max_slippage_bps: Some(CONVERSION_MAX_SLIPPAGE_BPS),
        // The SDK default: 30 s to see the converted USDB arrive.
        completion_timeout_secs: None,
    }
}

/// The prepare for an auto-convert: a self-transfer to the wallet's own Spark
/// address that converts bitcoin into at least `usdb_target` USDB base units.
///
/// The SDK takes bitcoin → USDB only as a USDB target, never as an exact sats
/// input (`sdk/payments/conversion.rs` at the pin), and pads the sats it
/// spends by up to the slippage cap. [`convert`] sizes the target so the
/// sats spent stay within what was received.
pub fn prepare_request(own_spark_address: &str, usdb_target: u128) -> PrepareSendPaymentRequest {
    PrepareSendPaymentRequest {
        payment_request: PaymentRequest::Input {
            input: own_spark_address.to_string(),
        },
        amount: Some(usdb_target),
        token_identifier: Some(USDB_MAINNET_TOKEN_IDENTIFIER.to_string()),
        conversion_options: Some(conversion_options()),
        // Fees excluded: the SDK refuses `FeesIncluded` with `FromBitcoin`.
        fee_policy: None,
    }
}

/// The sats a prepared auto-convert spends, after checking the SDK prepared
/// what [`prepare_request`] asked for: a self-transfer to `own_spark_address`
/// carrying a bitcoin → USDB conversion at the 50 bps cap. Anything else is
/// refused before it can be sent.
pub fn checked_sats_in(
    prepared: &PrepareSendPaymentResponse,
    own_spark_address: &str,
) -> Result<u128, String> {
    match &prepared.payment_method {
        SendPaymentMethod::SparkAddress { address, .. } if address == own_spark_address => {}
        other => {
            return Err(format!(
                "the conversion was prepared for an unexpected destination: {other:?}"
            ))
        }
    }
    if prepared.token_identifier.as_deref() != Some(USDB_MAINNET_TOKEN_IDENTIFIER) {
        return Err("the conversion was prepared for a token other than USDB".to_string());
    }
    let estimate = prepared
        .conversion_estimate
        .as_ref()
        .ok_or("the SDK prepared no conversion")?;
    if !matches!(
        estimate.options.conversion_type,
        ConversionType::FromBitcoin
    ) || estimate.options.max_slippage_bps != Some(CONVERSION_MAX_SLIPPAGE_BPS)
    {
        return Err(format!(
            "the SDK prepared a different conversion: {:?}",
            estimate.options
        ));
    }
    Ok(estimate.amount_in)
}

/// `target` scaled to spend at most `budget_sats`, given that it was quoted
/// at `sats_in`. `None` when the quote is unusable.
fn scale_target(target: u128, sats_in: u128, budget_sats: u128) -> Option<u128> {
    if sats_in == 0 {
        return None;
    }
    Some(target.checked_mul(budget_sats)? / sats_in)
}

/// Minimum amounts for a bitcoin → USDB conversion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    /// In sats.
    pub min_from_sats: Option<u128>,
    /// In USDB base units.
    pub min_to_units: Option<u128>,
}

/// A prepared conversion and the sats it spends.
pub struct Quote<P> {
    pub sats_in: u128,
    pub prepared: P,
}

/// The SDK calls auto-convert needs. The server implements it on the SDK;
/// tests use a fake.
#[async_trait]
pub trait Converter: Send + Sync {
    type Prepared: Send;
    async fn limits(&self) -> Result<Limits, String>;
    async fn balance_sats(&self) -> Result<u64, String>;
    /// Prepare (without sending) a conversion into `usdb_target` base units.
    async fn quote(&self, usdb_target: u128) -> Result<Quote<Self::Prepared>, String>;
    /// Send a prepared conversion. Moves funds: an error doesn't prove
    /// nothing moved, because the SDK can time out waiting for a conversion
    /// that still completes.
    async fn send(&self, prepared: Self::Prepared) -> Result<(), String>;
}

/// [`Converter`] on the Spark SDK.
pub struct SdkConverter<'a> {
    sdk: &'a SdkHandle,
    own_address: &'a OnceCell<String>,
}

impl<'a> SdkConverter<'a> {
    /// `own_address` caches the wallet's Spark address across conversions.
    pub fn new(sdk: &'a SdkHandle, own_address: &'a OnceCell<String>) -> Self {
        Self { sdk, own_address }
    }

    async fn own_address(&self) -> Result<&str, String> {
        self.own_address
            .get_or_try_init(|| async {
                self.sdk
                    .sdk
                    .receive_payment(ReceivePaymentRequest {
                        payment_method: ReceivePaymentMethod::SparkAddress,
                    })
                    .await
                    .map(|response| response.payment_request)
                    .map_err(|e| format!("could not read the wallet's Spark address: {e}"))
            })
            .await
            .map(String::as_str)
    }
}

#[async_trait]
impl Converter for SdkConverter<'_> {
    type Prepared = PrepareSendPaymentResponse;

    async fn limits(&self) -> Result<Limits, String> {
        let limits = self
            .sdk
            .sdk
            .fetch_conversion_limits(FetchConversionLimitsRequest {
                conversion_type: ConversionType::FromBitcoin,
                token_identifier: Some(USDB_MAINNET_TOKEN_IDENTIFIER.to_string()),
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(Limits {
            min_from_sats: limits.min_from_amount,
            min_to_units: limits.min_to_amount,
        })
    }

    async fn balance_sats(&self) -> Result<u64, String> {
        self.sdk
            .sdk
            .get_info(GetInfoRequest {
                ensure_synced: None,
            })
            .await
            .map(|info| info.balance_sats)
            .map_err(|e| e.to_string())
    }

    async fn quote(&self, usdb_target: u128) -> Result<Quote<Self::Prepared>, String> {
        let own_address = self.own_address().await?;
        let prepared = self
            .sdk
            .sdk
            .prepare_send_payment(prepare_request(own_address, usdb_target))
            .await
            .map_err(|e| e.to_string())?;
        let sats_in = checked_sats_in(&prepared, own_address)?;
        Ok(Quote { sats_in, prepared })
    }

    async fn send(&self, prepared: Self::Prepared) -> Result<(), String> {
        // No idempotency key: the SDK refuses one on a payment with a token
        // leg, which a conversion always has.
        self.sdk
            .sdk
            .send_payment(SendPaymentRequest {
                prepare_response: prepared,
                options: None,
                idempotency_key: None,
            })
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// How a conversion attempt ended short of a conversion.
#[derive(Debug, PartialEq, Eq)]
enum Shortfall {
    /// The pending sats buy less USDB than the pool's minimum output. Not a
    /// failure: they wait for the next receive.
    TooSmall,
    /// The attempt failed. `sent` is whether funds may have moved.
    Failed { reason: String, sent: bool },
}

fn not_sent(reason: String) -> Shortfall {
    Shortfall::Failed {
        reason,
        sent: false,
    }
}

/// Convert at most `budget_sats` into USDB. Returns the sats spent.
///
/// A quote at a probe target gives the price. The target is scaled to the
/// budget and re-quoted, shrinking it while the pool's rounding and slippage
/// pad overshoot, and is sent only once a quote fits the budget.
async fn convert<C: Converter>(
    converter: &C,
    budget_sats: u64,
    limits: Limits,
) -> Result<u128, Shortfall> {
    let budget = u128::from(budget_sats);
    // Aim a little under the budget, so the sats the pool rounds up don't
    // push a quote over it. What this leaves stays pending for next time.
    let aim = budget - (budget / 1_000 + 1).min(budget);
    let probe_target = limits.min_to_units.unwrap_or(0).max(PROBE_TARGET_UNITS);
    let probe = converter.quote(probe_target).await.map_err(not_sent)?;
    let mut target = scale_target(probe_target, probe.sats_in, aim)
        .ok_or_else(|| not_sent("the pool quoted no bitcoin input".to_string()))?;
    for _ in 0..SIZING_ROUNDS {
        if target == 0 || limits.min_to_units.is_some_and(|min| target < min) {
            return Err(Shortfall::TooSmall);
        }
        let quote = converter.quote(target).await.map_err(not_sent)?;
        if quote.sats_in <= budget {
            converter
                .send(quote.prepared)
                .await
                .map_err(|reason| Shortfall::Failed { reason, sent: true })?;
            return Ok(quote.sats_in);
        }
        target = scale_target(target, quote.sats_in, aim)
            .ok_or_else(|| not_sent("the pool quoted no bitcoin input".to_string()))?;
    }
    Err(not_sent(format!(
        "could not fit the conversion within the {budget_sats} sats received"
    )))
}

/// What the next conversion attempt should do.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Off, or nothing pending.
    Idle,
    /// Below the pool's minimum: keep the sats pending for the next receive.
    Defer,
    /// Convert this many sats. They are no longer pending.
    Convert { budget_sats: u64 },
}

/// Bridge-owned auto-convert for one Cube.
pub struct AutoConvert {
    path: PathBuf,
    saved: Saved,
    failures: FailureTracker,
    seen: SeenPayments,
    /// Bumped by every switch change, so a conversion that was running when
    /// the user flipped the switch doesn't book its outcome against the new
    /// setting.
    epoch: u64,
}

impl AutoConvert {
    /// Load the Cube's state from its Spark storage directory.
    pub fn load(storage_dir: &Path) -> Self {
        let path = storage_dir.join(STATE_FILE);
        Self {
            saved: load(&path),
            path,
            failures: FailureTracker::default(),
            seen: SeenPayments::default(),
            epoch: 0,
        }
    }

    pub fn state(&self) -> AutoConvertState {
        AutoConvertState {
            enabled: self.saved.enabled,
            breaker: self.saved.breaker.clone(),
        }
    }

    /// Persist `next`, then adopt it. If the write fails nothing changes.
    fn commit(&mut self, next: Saved) -> io::Result<()> {
        if next != self.saved {
            save(&self.path, &next)?;
            self.saved = next;
        }
        Ok(())
    }

    /// Persist `next` if possible, and adopt it either way. For changes the
    /// bridge makes on its own, which must hold for this session even when
    /// the disk refuses them.
    fn adopt(&mut self, next: Saved) {
        if let Err(e) = save(&self.path, &next) {
            tracing::error!("could not save {}: {e}", self.path.display());
        }
        self.saved = next;
    }

    /// Turn auto-convert on or off. Returns whether the reported state
    /// changed; on an error nothing changed and nothing may be acknowledged.
    ///
    /// On resets the breaker and starts with nothing pending, so only bitcoin
    /// received from now on converts. Off drops the pending sats; the
    /// existing USDB stays USDB. Off keeps a pause's reason readable.
    pub fn set_enabled(&mut self, enabled: bool) -> io::Result<bool> {
        let before = self.state();
        let next = if enabled {
            if self.saved.enabled {
                return Ok(false);
            }
            Saved {
                enabled: true,
                breaker: AutoConvertBreaker::Ready,
                pending_sats: 0,
                ..self.saved.clone()
            }
        } else {
            Saved {
                enabled: false,
                pending_sats: 0,
                ..self.saved.clone()
            }
        };
        self.commit(next)?;
        self.epoch += 1;
        if enabled {
            self.failures.reset();
        }
        Ok(self.state() != before)
    }

    /// Count an incoming payment. Ignored while off, and the second time the
    /// same payment is seen.
    pub fn note_receive(&mut self, receipt: &Receipt) -> bool {
        if !self.saved.enabled || !self.seen.insert(&receipt.payment_id) {
            return false;
        }
        let next = Saved {
            pending_sats: self.saved.pending_sats.saturating_add(receipt.sats),
            ..self.saved.clone()
        };
        self.adopt(next);
        true
    }

    fn has_pending(&self) -> bool {
        self.saved.enabled && self.saved.pending_sats > 0
    }

    /// Decide the next attempt from the current bitcoin balance and the
    /// pool's minimum input. The budget is the pending sats, never more than
    /// the balance. A conversion takes its budget out of the pending total
    /// before it runs, so a crash mid-conversion can't convert it twice.
    fn take(&mut self, balance_sats: u64, min_from_sats: Option<u128>) -> Step {
        if !self.has_pending() {
            return Step::Idle;
        }
        let budget_sats = self.saved.pending_sats.min(balance_sats);
        if budget_sats == 0 {
            return Step::Idle;
        }
        if min_from_sats.is_some_and(|min| u128::from(budget_sats) < min) {
            return Step::Defer;
        }
        let next = Saved {
            pending_sats: self.saved.pending_sats - budget_sats,
            ..self.saved.clone()
        };
        self.adopt(next);
        Step::Convert { budget_sats }
    }

    /// Return sats to the pending total, if auto-convert is still on.
    fn give_back(&mut self, sats: u64) {
        if sats == 0 || !self.saved.enabled {
            return;
        }
        let next = Saved {
            pending_sats: self.saved.pending_sats.saturating_add(sats),
            ..self.saved.clone()
        };
        self.adopt(next);
    }

    /// A conversion of `budget_sats` went through, spending `spent_sats`.
    /// The few sats left over stay pending.
    fn record_success(&mut self, budget_sats: u64, spent_sats: u128) {
        let forgotten = self.failures.record_success();
        if forgotten > 0 {
            tracing::info!("auto-convert succeeded; forgetting {forgotten} earlier failure(s)");
        }
        let spent = u64::try_from(spent_sats).unwrap_or(u64::MAX);
        self.give_back(budget_sats.saturating_sub(spent));
    }

    /// A conversion of `budget_sats` failed. Returns the number of failures
    /// when this one trips the breaker.
    ///
    /// Sats that were never sent go back to the pending total. Sats that
    /// were sent are dropped from it: the outcome is unknown, and sending
    /// them again could convert them twice.
    fn record_failure(&mut self, budget_sats: u64, reason: &str, sent: bool) -> Option<u32> {
        match self.failures.record_failure() {
            Verdict::Tolerate { failures } => {
                tracing::warn!("auto-convert failed ({failures} in a row): {reason}");
                if !sent {
                    self.give_back(budget_sats);
                }
                None
            }
            Verdict::Pause { failures } => {
                tracing::error!(
                    "auto-convert paused after {failures} consecutive failures: {reason}"
                );
                self.adopt(Saved {
                    enabled: false,
                    breaker: AutoConvertBreaker::Paused {
                        failures,
                        reason: reason.to_string(),
                    },
                    pending_sats: 0,
                    ..self.saved.clone()
                });
                Some(failures)
            }
        }
    }
}

/// Count `receipts`, then convert what is pending if it is enough. Returns
/// the events to emit: on a pause, the new state and the legacy
/// [`ProtocolEvent::StableBalancePaused`] (the current gui records "off"
/// from it, so its next reconcile doesn't turn auto-convert back on).
///
/// `converter` is `None` while the SDK isn't connected yet: the receipts
/// still count, and convert with the next receive.
///
/// The lock is held only between SDK calls, so the user's switch and its
/// reads never wait for a conversion. Callers run one `process` at a time.
pub async fn process<C: Converter>(
    auto_convert: &Mutex<AutoConvert>,
    converter: Option<&C>,
    receipts: &[Receipt],
) -> Vec<ProtocolEvent> {
    {
        let mut ac = auto_convert.lock().await;
        let counted = receipts.iter().filter(|r| ac.note_receive(r)).count();
        if counted == 0 || !ac.has_pending() {
            return Vec::new();
        }
    }
    let Some(converter) = converter else {
        return Vec::new();
    };

    let failed = |reason: String| async move {
        let mut ac = auto_convert.lock().await;
        if !ac.saved.enabled {
            return Vec::new();
        }
        trip_events(&mut ac, 0, &reason, false)
    };
    let limits = match converter.limits().await {
        Ok(limits) => limits,
        Err(e) => return failed(format!("could not read the conversion limits: {e}")).await,
    };
    let balance_sats = match converter.balance_sats().await {
        Ok(balance) => balance,
        Err(e) => return failed(format!("could not read the bitcoin balance: {e}")).await,
    };
    let (budget_sats, epoch) = {
        let mut ac = auto_convert.lock().await;
        match ac.take(balance_sats, limits.min_from_sats) {
            Step::Convert { budget_sats } => (budget_sats, ac.epoch),
            Step::Idle | Step::Defer => return Vec::new(),
        }
    };

    let outcome = convert(converter, budget_sats, limits).await;
    let mut ac = auto_convert.lock().await;
    if ac.epoch != epoch {
        // Switched off (and maybe on again) meanwhile, which dropped the
        // pending sats and reset the breaker. This outcome belongs to the
        // old setting.
        tracing::info!("auto-convert changed during a conversion: {outcome:?}");
        return Vec::new();
    }
    match outcome {
        Ok(spent_sats) => {
            tracing::info!("auto-converted {spent_sats} sats to USDB");
            ac.record_success(budget_sats, spent_sats);
            Vec::new()
        }
        Err(Shortfall::TooSmall) => {
            ac.give_back(budget_sats);
            Vec::new()
        }
        Err(Shortfall::Failed { reason, sent }) => trip_events(&mut ac, budget_sats, &reason, sent),
    }
}

fn trip_events(
    ac: &mut AutoConvert,
    budget_sats: u64,
    reason: &str,
    sent: bool,
) -> Vec<ProtocolEvent> {
    match ac.record_failure(budget_sats, reason, sent) {
        None => Vec::new(),
        Some(failures) => vec![
            ProtocolEvent::AutoConvertStateChanged { state: ac.state() },
            ProtocolEvent::StableBalancePaused {
                reason: reason.to_string(),
                failures,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque as Script;
    use std::sync::Mutex as StdMutex;

    use breez_sdk_spark::{
        ConversionEstimate, ConversionInfo, FeePolicy, SparkHtlcDetails, SparkHtlcStatus,
    };

    const OWN: &str = "spark1ownaddressfortests";

    /// A fresh directory for one test's state file, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("cc-auto-convert-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join(STATE_FILE)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn receipt(id: &str, sats: u64) -> Receipt {
        Receipt {
            payment_id: id.to_string(),
            sats,
        }
    }

    fn enabled(dir: &TempDir) -> AutoConvert {
        let mut ac = AutoConvert::load(&dir.0);
        assert!(ac.set_enabled(true).expect("saved"));
        ac
    }

    /// One recorded call to the fake.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Limits,
        Balance,
        Quote(u128),
        Send(u128),
    }

    /// A fake pool: `sats_per_unit_x1e6` sats buy 10^6 USDB base units, plus a
    /// `pad_bps` slippage pad, with optional scripted failures.
    struct FakePool {
        limits: Limits,
        balance_sats: u64,
        sats_per_unit_x1e6: u128,
        pad_bps: u128,
        fail_quote: StdMutex<Script<String>>,
        fail_send: StdMutex<Script<String>>,
        calls: StdMutex<Vec<Call>>,
    }

    impl FakePool {
        fn new(balance_sats: u64) -> Self {
            Self {
                limits: Limits {
                    min_from_sats: Some(1_000),
                    min_to_units: Some(500_000),
                },
                balance_sats,
                // 1 USDB (10^6 units) costs 1,000 sats.
                sats_per_unit_x1e6: 1_000,
                pad_bps: 50,
                fail_quote: StdMutex::new(Script::new()),
                fail_send: StdMutex::new(Script::new()),
                calls: StdMutex::new(Vec::new()),
            }
        }

        fn sats_for(&self, units: u128) -> u128 {
            let base = (units * self.sats_per_unit_x1e6).div_ceil(1_000_000);
            base + (base * self.pad_bps).div_ceil(10_000)
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        fn sends(&self) -> Vec<u128> {
            self.calls()
                .into_iter()
                .filter_map(|c| match c {
                    Call::Send(target) => Some(target),
                    _ => None,
                })
                .collect()
        }
    }

    #[async_trait]
    impl Converter for FakePool {
        type Prepared = u128;

        async fn limits(&self) -> Result<Limits, String> {
            self.calls.lock().unwrap().push(Call::Limits);
            Ok(self.limits)
        }

        async fn balance_sats(&self) -> Result<u64, String> {
            self.calls.lock().unwrap().push(Call::Balance);
            Ok(self.balance_sats)
        }

        async fn quote(&self, usdb_target: u128) -> Result<Quote<u128>, String> {
            self.calls.lock().unwrap().push(Call::Quote(usdb_target));
            if let Some(reason) = self.fail_quote.lock().unwrap().pop_front() {
                return Err(reason);
            }
            Ok(Quote {
                sats_in: self.sats_for(usdb_target),
                prepared: usdb_target,
            })
        }

        async fn send(&self, prepared: u128) -> Result<(), String> {
            self.calls.lock().unwrap().push(Call::Send(prepared));
            match self.fail_send.lock().unwrap().pop_front() {
                Some(reason) => Err(reason),
                None => Ok(()),
            }
        }
    }

    // ---- request building ----

    /// The exact request: a self-transfer to the wallet's own Spark address,
    /// USDB-denominated, bitcoin → USDB at the 50 bps cap.
    #[test]
    fn the_request_is_a_capped_self_transfer_into_usdb() {
        let request = prepare_request(OWN, 12_400_000);
        match &request.payment_request {
            PaymentRequest::Input { input } => assert_eq!(input, OWN),
            other => panic!("unexpected payment request {other:?}"),
        }
        assert_eq!(request.amount, Some(12_400_000));
        assert_eq!(
            request.token_identifier.as_deref(),
            Some(USDB_MAINNET_TOKEN_IDENTIFIER)
        );
        let options = request.conversion_options.expect("a conversion");
        assert!(matches!(
            options.conversion_type,
            ConversionType::FromBitcoin
        ));
        assert_eq!(options.max_slippage_bps, Some(50));
        assert_eq!(options.max_slippage_bps, Some(CONVERSION_MAX_SLIPPAGE_BPS));
        assert_eq!(options.completion_timeout_secs, None);
        assert!(request.fee_policy.is_none());
    }

    fn prepared(
        address: &str,
        token: Option<&str>,
        options: ConversionOptions,
    ) -> PrepareSendPaymentResponse {
        PrepareSendPaymentResponse {
            payment_method: SendPaymentMethod::SparkAddress {
                address: address.to_string(),
                fee: 0,
                token_identifier: token.map(str::to_string),
            },
            amount: 12_400_000,
            token_identifier: token.map(str::to_string),
            conversion_estimate: Some(ConversionEstimate {
                options,
                amount_in: 12_500,
                amount_out: 12_430_000,
                fee: 25,
                amount_adjustment: None,
            }),
            fee_policy: FeePolicy::FeesExcluded,
        }
    }

    #[test]
    fn only_the_prepare_that_was_asked_for_can_be_sent() {
        let usdb = Some(USDB_MAINNET_TOKEN_IDENTIFIER);
        assert_eq!(
            checked_sats_in(&prepared(OWN, usdb, conversion_options()), OWN),
            Ok(12_500)
        );

        // Somewhere else, another token, another direction or cap, or no
        // conversion at all: refused.
        assert!(checked_sats_in(
            &prepared("spark1someoneelse", usdb, conversion_options()),
            OWN
        )
        .is_err());
        assert!(checked_sats_in(
            &prepared(OWN, Some("btkn1other"), conversion_options()),
            OWN
        )
        .is_err());
        let to_bitcoin = ConversionOptions {
            conversion_type: ConversionType::ToBitcoin {
                from_token_identifier: USDB_MAINNET_TOKEN_IDENTIFIER.to_string(),
            },
            ..conversion_options()
        };
        assert!(checked_sats_in(&prepared(OWN, usdb, to_bitcoin), OWN).is_err());
        let default_cap = ConversionOptions {
            max_slippage_bps: None,
            ..conversion_options()
        };
        assert!(checked_sats_in(&prepared(OWN, usdb, default_cap), OWN).is_err());
        let mut plain = prepared(OWN, usdb, conversion_options());
        plain.conversion_estimate = None;
        assert!(checked_sats_in(&plain, OWN).is_err());
    }

    // ---- incoming-only filter ----

    fn payment(payment_type: PaymentType, method: PaymentMethod, amount: u128) -> Payment {
        Payment {
            id: "p1".to_string(),
            payment_type,
            status: PaymentStatus::Completed,
            amount,
            fees: 0,
            timestamp: 1,
            method,
            details: None,
            conversion_details: None,
        }
    }

    fn amm(status: breez_sdk_spark::ConversionStatus) -> ConversionInfo {
        ConversionInfo::Amm {
            pool_id: "pool".to_string(),
            conversion_id: "c1".to_string(),
            status,
            fee: None,
            purpose: None,
            amount_adjustment: None,
            degradation: None,
        }
    }

    #[test]
    fn only_completed_incoming_bitcoin_counts() {
        for method in [
            PaymentMethod::Lightning,
            PaymentMethod::Spark,
            PaymentMethod::Deposit,
        ] {
            assert_eq!(
                eligible_receive(&payment(PaymentType::Receive, method, 21_000)),
                Some(receipt("p1", 21_000)),
                "{method:?}"
            );
        }

        // Outgoing, token (the USDB a conversion delivers), unfinished, empty.
        assert_eq!(
            eligible_receive(&payment(PaymentType::Send, PaymentMethod::Spark, 21_000)),
            None
        );
        assert_eq!(
            eligible_receive(&payment(PaymentType::Receive, PaymentMethod::Token, 21_000)),
            None
        );
        let mut pending = payment(PaymentType::Receive, PaymentMethod::Spark, 21_000);
        pending.status = PaymentStatus::Pending;
        assert_eq!(eligible_receive(&pending), None);
        assert_eq!(
            eligible_receive(&payment(PaymentType::Receive, PaymentMethod::Spark, 0)),
            None
        );
    }

    /// A conversion's own sats leg and a refunded conversion carry AMM
    /// conversion info, so neither can start another conversion.
    #[test]
    fn conversion_legs_and_refunds_never_count() {
        use breez_sdk_spark::ConversionStatus;
        for status in [ConversionStatus::Completed, ConversionStatus::Refunded] {
            let mut leg = payment(PaymentType::Receive, PaymentMethod::Spark, 21_000);
            leg.details = Some(PaymentDetails::Spark {
                invoice_details: None,
                htlc_details: None,
                conversion_info: Some(amm(status.clone())),
            });
            assert_eq!(eligible_receive(&leg), None, "{status:?}");
        }

        let mut lightning_leg = payment(PaymentType::Receive, PaymentMethod::Lightning, 21_000);
        lightning_leg.details = Some(PaymentDetails::Lightning {
            description: None,
            invoice: "lnbc1".to_string(),
            destination_pubkey: "02ab".to_string(),
            htlc_details: SparkHtlcDetails {
                payment_hash: "h".to_string(),
                preimage: None,
                expiry_time: 0,
                status: SparkHtlcStatus::PreimageShared,
            },
            lnurl_pay_info: None,
            lnurl_withdraw_info: None,
            lnurl_receive_metadata: None,
            conversion_info: Some(amm(ConversionStatus::Completed)),
        });
        assert_eq!(eligible_receive(&lightning_leg), None);
    }

    // ---- persistence ----

    #[test]
    fn a_cube_that_never_chose_starts_off_and_ready() {
        let dir = TempDir::new();
        let ac = AutoConvert::load(&dir.0);
        assert_eq!(
            ac.state(),
            AutoConvertState {
                enabled: false,
                breaker: AutoConvertBreaker::Ready,
            }
        );
        assert!(!dir.file().exists(), "loading writes nothing");
    }

    #[test]
    fn the_switch_and_pending_sats_survive_a_restart() {
        let dir = TempDir::new();
        let mut ac = enabled(&dir);
        assert!(ac.note_receive(&receipt("a", 400)));
        drop(ac);

        let ac = AutoConvert::load(&dir.0);
        assert!(ac.state().enabled);
        assert_eq!(ac.saved.pending_sats, 400);
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.file()).unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "version": 1,
                "enabled": true,
                "breaker": { "status": "ready" },
                "pending_sats": 400,
            })
        );
    }

    #[test]
    fn an_unreadable_or_unknown_file_loads_as_off() {
        let dir = TempDir::new();
        for contents in [
            "not json".to_string(),
            serde_json::json!({ "version": 2, "enabled": true, "breaker": { "status": "ready" } })
                .to_string(),
        ] {
            fs::write(dir.file(), contents).unwrap();
            assert!(!AutoConvert::load(&dir.0).state().enabled);
        }
    }

    /// A switch the disk refuses is not made, so the caller can't
    /// acknowledge it.
    #[test]
    fn a_change_that_cannot_be_saved_is_not_made() {
        let dir = TempDir::new();
        // The storage "directory" is a file: every write under it fails.
        let not_a_dir = dir.0.join("blocker");
        fs::write(&not_a_dir, b"").unwrap();
        let mut ac = AutoConvert::load(&not_a_dir);

        assert!(ac.set_enabled(true).is_err());
        assert!(!ac.state().enabled);
        assert!(!ac.note_receive(&receipt("a", 5_000)), "still off");
    }

    // ---- switch semantics ----

    #[test]
    fn on_starts_clean_and_off_keeps_usdb_by_converting_nothing() {
        let dir = TempDir::new();
        let mut ac = enabled(&dir);
        assert!(!ac.set_enabled(true).unwrap(), "on again changes nothing");
        ac.note_receive(&receipt("a", 400));

        assert!(ac.set_enabled(false).unwrap());
        assert_eq!(ac.saved.pending_sats, 0, "off drops what was waiting");
        assert!(
            !ac.note_receive(&receipt("b", 9_000)),
            "off ignores receives"
        );
        assert_eq!(ac.take(1_000_000, Some(1_000)), Step::Idle);

        assert!(ac.set_enabled(true).unwrap());
        assert_eq!(ac.saved.pending_sats, 0, "on never sweeps old bitcoin");
    }

    // ---- the conversion pipeline ----

    #[tokio::test]
    async fn a_receive_converts_within_the_received_sats() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::new(100_000);

        let events = process(&ac, Some(&pool), &[receipt("a", 20_000)]).await;
        assert!(events.is_empty());

        let calls = pool.calls();
        assert_eq!(calls[..2], [Call::Limits, Call::Balance]);
        // Probe at the pool's minimum output, then the sized target.
        assert_eq!(calls[2], Call::Quote(1_000_000));
        let sent = pool.sends();
        assert_eq!(sent.len(), 1);
        let spent = pool.sats_for(sent[0]);
        assert!(spent <= 20_000, "spent {spent}");
        assert!(
            spent >= 19_800,
            "spent only {spent}: the target was undersized"
        );
        // The few sats left over wait for the next receive.
        assert_eq!(ac.lock().await.saved.pending_sats, 20_000 - spent as u64);
    }

    /// The slippage pad can push the first sized quote past the received sats;
    /// it is shrunk and re-quoted, never sent over budget.
    #[tokio::test]
    async fn an_overshooting_quote_is_shrunk_before_sending() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let mut pool = FakePool::new(100_000);
        pool.pad_bps = 200;
        // The probe sees no pad (a cheap probe), so the scaled target overshoots.
        struct Unpadded<'a>(&'a FakePool);
        #[async_trait]
        impl Converter for Unpadded<'_> {
            type Prepared = u128;
            async fn limits(&self) -> Result<Limits, String> {
                self.0.limits().await
            }
            async fn balance_sats(&self) -> Result<u64, String> {
                self.0.balance_sats().await
            }
            async fn quote(&self, target: u128) -> Result<Quote<u128>, String> {
                let mut quote = self.0.quote(target).await?;
                if target == 1_000_000 {
                    quote.sats_in = 1_000;
                }
                Ok(quote)
            }
            async fn send(&self, prepared: u128) -> Result<(), String> {
                self.0.send(prepared).await
            }
        }

        process(&ac, Some(&Unpadded(&pool)), &[receipt("a", 20_000)]).await;
        let quotes: Vec<_> = pool
            .calls()
            .into_iter()
            .filter(|c| matches!(c, Call::Quote(_)))
            .collect();
        assert_eq!(quotes.len(), 3, "probe, overshoot, shrunk: {quotes:?}");
        let sent = pool.sends();
        assert_eq!(sent.len(), 1);
        assert!(pool.sats_for(sent[0]) <= 20_000);
    }

    #[tokio::test]
    async fn below_the_minimum_waits_then_batches_with_the_next_receive() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::new(100_000);

        process(&ac, Some(&pool), &[receipt("a", 600)]).await;
        assert!(
            pool.sends().is_empty(),
            "600 sats is under the 1,000 minimum"
        );
        assert_eq!(ac.lock().await.saved.pending_sats, 600);

        // Survives a restart.
        let ac = Mutex::new(AutoConvert::load(&dir.0));
        assert_eq!(ac.lock().await.saved.pending_sats, 600);

        process(&ac, Some(&pool), &[receipt("b", 700)]).await;
        let sent = pool.sends();
        assert_eq!(sent.len(), 1, "600 + 700 converts as one");
        let spent = pool.sats_for(sent[0]);
        assert!((1_200..=1_300).contains(&spent), "spent {spent}");
    }

    /// Receives queued together convert once, and a payment reported twice
    /// counts once.
    #[tokio::test]
    async fn queued_receives_coalesce_and_duplicates_count_once() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::new(100_000);

        process(
            &ac,
            Some(&pool),
            &[
                receipt("a", 3_000),
                receipt("b", 4_000),
                receipt("a", 3_000),
            ],
        )
        .await;
        let sent = pool.sends();
        assert_eq!(sent.len(), 1);
        let spent = pool.sats_for(sent[0]);
        assert!((6_900..=7_000).contains(&spent), "spent {spent}");

        // A later duplicate adds nothing and converts nothing.
        let before = pool.calls().len();
        process(&ac, Some(&pool), &[receipt("b", 4_000)]).await;
        assert_eq!(pool.calls().len(), before);
    }

    /// Never more than the bitcoin balance, even if the pending total says so.
    #[tokio::test]
    async fn the_budget_never_exceeds_the_bitcoin_balance() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::new(5_000);

        process(&ac, Some(&pool), &[receipt("a", 20_000)]).await;
        let sent = pool.sends();
        assert_eq!(sent.len(), 1);
        assert!(pool.sats_for(sent[0]) <= 5_000);
    }

    #[tokio::test]
    async fn off_and_paused_make_no_request() {
        let dir = TempDir::new();
        let ac = Mutex::new(AutoConvert::load(&dir.0));
        let pool = FakePool::new(100_000);
        process(&ac, Some(&pool), &[receipt("a", 50_000)]).await;
        assert!(pool.calls().is_empty(), "off: {:?}", pool.calls());

        ac.lock().await.set_enabled(true).unwrap();
        ac.lock().await.record_failure(0, "x", false);
        ac.lock().await.record_failure(0, "x", false);
        ac.lock().await.record_failure(0, "x", false);
        assert!(!ac.lock().await.state().enabled);
        process(&ac, Some(&pool), &[receipt("b", 50_000)]).await;
        assert!(pool.calls().is_empty(), "paused: {:?}", pool.calls());
    }

    /// Before the SDK is connected receipts still count, and convert with
    /// the next receive.
    #[tokio::test]
    async fn receipts_before_the_sdk_is_ready_are_kept() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        process::<FakePool>(&ac, None, &[receipt("a", 5_000)]).await;
        assert_eq!(ac.lock().await.saved.pending_sats, 5_000);

        let pool = FakePool::new(100_000);
        process(&ac, Some(&pool), &[receipt("b", 5_000)]).await;
        let spent = pool.sats_for(pool.sends()[0]);
        assert!((9_900..=10_000).contains(&spent), "spent {spent}");
    }

    // ---- failures and the breaker ----

    /// A prepare failure moved nothing, so the sats stay pending for the next
    /// try. A failed send may have moved them, so they are never re-sent.
    #[tokio::test]
    async fn unsent_sats_are_retried_and_sent_ones_are_not() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::new(100_000);
        pool.fail_quote
            .lock()
            .unwrap()
            .push_back("Pool has no liquidity".into());
        process(&ac, Some(&pool), &[receipt("a", 5_000)]).await;
        assert!(pool.sends().is_empty());
        assert_eq!(ac.lock().await.saved.pending_sats, 5_000);

        pool.fail_send
            .lock()
            .unwrap()
            .push_back("Timeout waiting for conversion to complete".into());
        process(&ac, Some(&pool), &[receipt("b", 1_000)]).await;
        assert_eq!(pool.sends().len(), 1);
        assert_eq!(
            ac.lock().await.saved.pending_sats,
            0,
            "the 6,000 sats handed to the SDK are not pending again"
        );
    }

    #[tokio::test]
    async fn three_failures_in_a_row_pause_and_report_it() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::new(100_000);
        for _ in 0..3 {
            pool.fail_quote
                .lock()
                .unwrap()
                .push_back("Pool has no liquidity".into());
        }

        assert!(process(&ac, Some(&pool), &[receipt("a", 5_000)])
            .await
            .is_empty());
        assert!(process(&ac, Some(&pool), &[receipt("b", 5_000)])
            .await
            .is_empty());
        let events = process(&ac, Some(&pool), &[receipt("c", 5_000)]).await;

        let paused = AutoConvertState {
            enabled: false,
            breaker: AutoConvertBreaker::Paused {
                failures: 3,
                reason: "Pool has no liquidity".to_string(),
            },
        };
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(
            &events[0],
            ProtocolEvent::AutoConvertStateChanged { state } if *state == paused
        ));
        assert!(matches!(
            &events[1],
            ProtocolEvent::StableBalancePaused { reason, failures: 3 }
                if reason == "Pool has no liquidity"
        ));
        assert_eq!(ac.lock().await.state(), paused);
        assert_eq!(ac.lock().await.saved.pending_sats, 0);

        // The pause is on disk, so a restart doesn't resume converting.
        assert_eq!(AutoConvert::load(&dir.0).state(), paused);
        assert!(pool.sends().is_empty());
    }

    #[tokio::test]
    async fn a_success_clears_the_failure_run() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::new(100_000);
        for _ in 0..2 {
            pool.fail_quote.lock().unwrap().push_back("busy".into());
        }
        process(&ac, Some(&pool), &[receipt("a", 5_000)]).await;
        process(&ac, Some(&pool), &[receipt("b", 5_000)]).await;
        process(&ac, Some(&pool), &[receipt("c", 5_000)]).await;
        assert_eq!(pool.sends().len(), 1);

        // Two more failures after the success don't trip it.
        for _ in 0..2 {
            pool.fail_quote.lock().unwrap().push_back("busy".into());
        }
        process(&ac, Some(&pool), &[receipt("d", 5_000)]).await;
        let events = process(&ac, Some(&pool), &[receipt("e", 5_000)]).await;
        assert!(events.is_empty());
        assert!(ac.lock().await.state().enabled);
    }

    /// Turning it back on after a pause gives a ready breaker and the full
    /// threshold.
    #[tokio::test]
    async fn turning_it_on_resumes_a_paused_breaker() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::new(100_000);
        for _ in 0..3 {
            pool.fail_quote.lock().unwrap().push_back("busy".into());
        }
        for id in ["a", "b", "c"] {
            process(&ac, Some(&pool), &[receipt(id, 5_000)]).await;
        }
        assert!(!ac.lock().await.state().enabled);

        assert!(ac.lock().await.set_enabled(true).unwrap());
        assert_eq!(
            ac.lock().await.state(),
            AutoConvertState {
                enabled: true,
                breaker: AutoConvertBreaker::Ready,
            }
        );
        for _ in 0..2 {
            pool.fail_quote.lock().unwrap().push_back("busy".into());
        }
        process(&ac, Some(&pool), &[receipt("d", 5_000)]).await;
        let events = process(&ac, Some(&pool), &[receipt("e", 5_000)]).await;
        assert!(events.is_empty(), "two failures after a resume don't pause");
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
        assert_eq!(tracker.record_failure(), Verdict::Tolerate { failures: 1 });
    }

    #[test]
    fn tracker_reset_and_success_forget_history() {
        let mut tracker = FailureTracker::default();
        tracker.record_failure();
        tracker.record_failure();
        tracker.reset();
        assert_eq!(tracker.record_failure(), Verdict::Tolerate { failures: 1 });
        assert_eq!(tracker.record_success(), 1);
        assert_eq!(tracker.record_success(), 0);
    }

    /// No bridge code can change SDK Stable Balance. Every change to it,
    /// `Unset` included, goes through the SDK's `update_user_settings`, and
    /// nothing here calls that.
    #[test]
    fn nothing_calls_the_sdk_settings_setter() {
        let call = [".update_user_settings", "("].concat();
        for (name, source) in [
            ("main.rs", include_str!("main.rs")),
            ("server.rs", include_str!("server.rs")),
            ("sdk_adapter.rs", include_str!("sdk_adapter.rs")),
            ("auto_convert.rs", include_str!("auto_convert.rs")),
        ] {
            assert!(!source.contains(&call), "{name} calls update_user_settings");
        }
    }

    #[test]
    fn target_scaling_is_proportional_and_refuses_a_zero_quote() {
        assert_eq!(scale_target(1_000_000, 1_000, 20_000), Some(20_000_000));
        assert_eq!(scale_target(1_000_000, 1_005, 1_000), Some(995_024));
        assert_eq!(scale_target(1_000_000, 0, 1_000), None);
        assert_eq!(scale_target(u128::MAX, 2, 3), None);
    }
}
