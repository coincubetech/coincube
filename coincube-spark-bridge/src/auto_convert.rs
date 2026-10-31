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
//!   sats to the pending total as soon as the SDK reports it
//!   ([`AutoConvert::note_receive`]);
//! - one worker ([`convert_pending`]) converts the pending sats, bounded by
//!   the bitcoin balance, once they reach the pool's minimum, with a
//!   self-transfer to the wallet's own Spark address ([`prepare_request`]);
//! - three failed conversions in a row turn auto-convert off and pause it
//!   ([`FailureTracker`]).
//!
//! The switch, the breaker and the pending sats persist per Cube in
//! [`STATE_FILE`], in the Cube's Spark storage directory. A Cube without
//! that file adopts the "on" it had under SDK Stable Balance
//! ([`legacy_stable_balance_label`]).

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use breez_sdk_spark::{
    default_storage_path, ConversionOptions, ConversionType, FetchConversionLimitsRequest,
    GetInfoRequest, Network as SparkNetwork, Payment, PaymentDetails, PaymentMethod,
    PaymentRequest, PaymentStatus, PaymentType, PrepareSendPaymentRequest,
    PrepareSendPaymentResponse, PublicKey, ReceivePaymentMethod, ReceivePaymentRequest,
    SendPaymentMethod, SendPaymentRequest, SqliteStorage, Storage,
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

/// Larger probes tried when the pool states only a bitcoin-side minimum and
/// the probe is refused, each [`PROBE_GROWTH`] times the last (up to 1,000
/// USDB). A probe is only a prepare: nothing is sent, and the SDK checks no
/// balance for it.
const PROBE_ESCALATIONS: usize = 3;
const PROBE_GROWTH: u128 = 10;

/// Quotes allowed after the price probe. Each one after the first shrinks the
/// target by how far the previous quote overshot the received sats.
const SIZING_ROUNDS: usize = 3;

/// Receives remembered so a repeated `PaymentSucceeded` isn't counted twice.
const SEEN_PAYMENTS: usize = 256;

/// Where SDK Stable Balance saved its on/off choice: the SDK's
/// `STABLE_BALANCE_ACTIVE_LABEL_KEY` (`persist/mod.rs` at the pin), a
/// `settings` row in its SQLite store. The value is the active token's label.
const LEGACY_LABEL_KEY: &str = "stable_balance_active_label";

/// The label the bridge gave USDB when it configured SDK Stable Balance.
pub const LEGACY_USDB_LABEL: &str = "USDB";

/// What [`STATE_FILE`] holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Saved {
    version: u32,
    enabled: bool,
    breaker: AutoConvertBreaker,
    /// Sats received while on and not converted yet: below the pool's minimum
    /// so far, or handed back by an attempt that failed before sending.
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

/// The sats a conversion of `budget` aims to spend: a little under it, so the
/// pool's rounding doesn't push a quote over. A send spends exactly the
/// quote's sats ([`exact_input`]), so this margin is only for sizing. What it
/// leaves stays bitcoin (see [`AutoConvert::record_success`]).
fn sizing_aim(budget: u128) -> u128 {
    budget.saturating_sub(budget / 1_000 + 1)
}

/// Whether a conversion of `budget` sats clears the pool's minimum input
/// `min_sats`, with the sizing margin below its aim as well as above it, so
/// a quote's rounding can't land under the minimum. The pool refuses such a
/// quote, and the refusal doesn't say so in a form the bridge can rely on
/// (a Flashnet error string at the pin), so it would count as a failure.
/// Short of that, the sats wait for the next receive.
fn clears_min_input(budget: u128, min_sats: u128) -> bool {
    sizing_aim(sizing_aim(budget)) >= min_sats
}

/// Make a checked auto-convert prepare spend exactly its quoted sats.
///
/// As prepared, the send converts toward a USDB target, and sending resolves
/// the conversion again and spends whatever sats the pool asks for then
/// (`FlashnetTokenConverter::convert` → `resolve_conversion` at the pin), so
/// a price move could spend more than was received. The SDK sends an exact
/// input, `ConversionAmount::AmountIn(estimate.amount_in)`, when the prepared
/// amount is at least the estimate's output (`uses_amount_in` in
/// `sdk/payments/conversion.rs` at the pin): the same exact-sats conversion
/// its own Stable Balance ran per receive. So the amount is set to the
/// output, in the same USDB units. The pool then takes exactly `amount_in`
/// sats, and the slippage cap bounds the USDB out at send time.
pub fn exact_input(prepared: &mut PrepareSendPaymentResponse) {
    if let Some(estimate) = &prepared.conversion_estimate {
        prepared.amount = estimate.amount_out;
    }
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
pub struct SdkConverter {
    sdk: SdkHandle,
    own_address: Arc<OnceCell<String>>,
}

impl SdkConverter {
    /// `own_address` caches the wallet's Spark address across conversions.
    pub fn new(sdk: SdkHandle, own_address: Arc<OnceCell<String>>) -> Self {
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
impl Converter for SdkConverter {
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
        // Synced, so a pass right after start-up doesn't size from a stale
        // balance.
        self.sdk
            .sdk
            .get_info(GetInfoRequest {
                ensure_synced: Some(true),
            })
            .await
            .map(|info| info.balance_sats)
            .map_err(|e| e.to_string())
    }

    async fn quote(&self, usdb_target: u128) -> Result<Quote<Self::Prepared>, String> {
        let own_address = self.own_address().await?;
        let mut prepared = self
            .sdk
            .sdk
            .prepare_send_payment(prepare_request(own_address, usdb_target))
            .await
            .map_err(|e| e.to_string())?;
        let sats_in = checked_sats_in(&prepared, own_address)?;
        exact_input(&mut prepared);
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
    /// The switch changed before sending: nothing was sent.
    Cancelled,
}

fn not_sent(reason: String) -> Shortfall {
    Shortfall::Failed {
        reason,
        sent: false,
    }
}

/// Convert at most `budget_sats` into USDB. Returns the sats the quote spent.
///
/// A quote at a probe target gives the price. The target is scaled to the
/// budget and re-quoted, shrinking it while the pool's rounding and slippage
/// pad overshoot, and is sent only once a quote fits the budget.
///
/// Flashnet states a minimum for one side of a pair only
/// (`get_min_amounts` at the pin). With a minimum output, the probe asks for
/// at least that. With only a minimum input, a probe that costs fewer sats is
/// refused, so a refused probe is retried larger ([`PROBE_ESCALATIONS`]).
///
/// The send runs under the send gate ([`AutoConvert::send_gate`]), with the
/// switch checked under the state lock once the gate is held. Every switch
/// change takes the gate first, so a switch either lands before the check
/// and stops the send, or waits until the send has finished: Off is never
/// acknowledged while a conversion it would have stopped can still start.
async fn convert<C: Converter>(
    converter: &C,
    budget_sats: u64,
    limits: Limits,
    auto_convert: &Mutex<AutoConvert>,
    epoch: u64,
) -> Result<u128, Shortfall> {
    let budget = u128::from(budget_sats);
    let aim = sizing_aim(budget);
    let mut probe_target = limits.min_to_units.unwrap_or(0).max(PROBE_TARGET_UNITS);
    let mut probe = converter.quote(probe_target).await;
    if limits.min_to_units.is_none() && limits.min_from_sats.is_some() {
        for _ in 0..PROBE_ESCALATIONS {
            if probe.is_ok() {
                break;
            }
            probe_target *= PROBE_GROWTH;
            probe = converter.quote(probe_target).await;
        }
    }
    let probe = probe.map_err(not_sent)?;
    let mut target = scale_target(probe_target, probe.sats_in, aim)
        .ok_or_else(|| not_sent("the pool quoted no bitcoin input".to_string()))?;
    for _ in 0..SIZING_ROUNDS {
        if target == 0 || limits.min_to_units.is_some_and(|min| target < min) {
            return Err(Shortfall::TooSmall);
        }
        let quote = converter.quote(target).await.map_err(not_sent)?;
        if quote.sats_in <= budget {
            let gate = auto_convert.lock().await.send_gate();
            let _sending = gate.lock().await;
            if !auto_convert.lock().await.still_wanted(epoch) {
                return Err(Shortfall::Cancelled);
            }
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
    /// The last [`Self::adopt`] couldn't save: memory is ahead of the file,
    /// so the next change must be written even if it matches memory.
    unsaved: bool,
    /// Held across a conversion's final switch check and its send, and taken
    /// by every switch change before it changes anything. See [`convert`].
    /// Separate from the state lock, which the SDK listener needs while a
    /// send waits on the SDK's own payment event.
    send_gate: Arc<Mutex<()>>,
}

impl AutoConvert {
    /// Whether the Cube has a bridge state file yet. Without one, it may
    /// have an SDK Stable Balance choice to adopt ([`Self::load_or_adopt`]).
    pub fn has_state_file(storage_dir: &Path) -> bool {
        storage_dir.join(STATE_FILE).exists()
    }

    /// Load the Cube's state; if it has no state file yet and SDK Stable
    /// Balance was on (`legacy_on`), start on, as the user had it. Only
    /// bitcoin received from now on converts: nothing is pending, and the
    /// existing balances stay as they are.
    pub fn load_or_adopt(storage_dir: &Path, legacy_on: bool) -> Self {
        let first_run = !Self::has_state_file(storage_dir);
        let mut ac = Self::load(storage_dir);
        if first_run && legacy_on {
            tracing::info!("auto-convert starts on: SDK Stable Balance was on for this Cube");
            ac.adopt(Saved {
                enabled: true,
                ..Saved::default()
            });
        }
        ac
    }

    /// Load the Cube's state from its Spark storage directory.
    pub fn load(storage_dir: &Path) -> Self {
        let path = storage_dir.join(STATE_FILE);
        Self {
            saved: load(&path),
            path,
            failures: FailureTracker::default(),
            seen: SeenPayments::default(),
            epoch: 0,
            unsaved: false,
            send_gate: Arc::new(Mutex::new(())),
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
        if next != self.saved || self.unsaved {
            save(&self.path, &next)?;
            self.saved = next;
            self.unsaved = false;
        }
        Ok(())
    }

    /// Persist `next` if possible, and adopt it either way. For changes the
    /// bridge makes on its own, which must hold for this session even when
    /// the disk refuses them.
    fn adopt(&mut self, next: Saved) {
        self.unsaved = match save(&self.path, &next) {
            Ok(()) => false,
            Err(e) => {
                tracing::error!("could not save {}: {e}", self.path.display());
                true
            }
        };
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
                // Already on. Memory may be ahead of the file (an adoption
                // that couldn't be saved): save it before acknowledging.
                if self.unsaved {
                    self.commit(self.saved.clone())?;
                }
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

    /// Count an incoming payment, against the switch as it is now. Ignored
    /// while off, and the second time the same payment is seen. Returns
    /// whether it counted, i.e. whether to wake [`convert_pending`].
    ///
    /// Called from the SDK's event listener as each payment is reported, so
    /// a receive is saved even while a conversion runs or the bridge stops:
    /// the SDK reports a payment once.
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

    /// The gate a switch change takes before changing anything, so it can't
    /// be acknowledged while a conversion is between its final check and the
    /// end of its send (see [`convert`]). Take it before the state lock.
    pub fn send_gate(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.send_gate)
    }

    /// Whether a conversion decided at `epoch` may still be sent.
    fn still_wanted(&self, epoch: u64) -> bool {
        self.epoch == epoch && self.saved.enabled
    }

    pub fn has_pending(&self) -> bool {
        self.saved.enabled && self.saved.pending_sats > 0
    }

    /// Decide the next attempt from the current bitcoin balance and the
    /// pool's minimum input. The budget is the pending sats, never more than
    /// the balance. A conversion takes its budget out of the pending total,
    /// on disk, before it runs, so a restart mid-conversion can't convert it
    /// twice. If that can't be saved, nothing is taken and nothing may be
    /// sent.
    fn take(&mut self, balance_sats: u64, min_from_sats: Option<u128>) -> io::Result<Step> {
        if !self.has_pending() {
            return Ok(Step::Idle);
        }
        let budget_sats = self.saved.pending_sats.min(balance_sats);
        if budget_sats == 0 {
            return Ok(Step::Idle);
        }
        if min_from_sats.is_some_and(|min| !clears_min_input(u128::from(budget_sats), min)) {
            return Ok(Step::Defer);
        }
        let next = Saved {
            pending_sats: self.saved.pending_sats - budget_sats,
            ..self.saved.clone()
        };
        self.commit(next)?;
        Ok(Step::Convert { budget_sats })
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

    /// A conversion went through. Nothing of its budget goes back to the
    /// pending total. The send spends the quoted `spent_sats` only through
    /// the SDK's exact-input selection ([`exact_input`]), and the actual
    /// debit isn't observed. Booking the quote's leftover could then later
    /// sweep bitcoin the user held before turning auto-convert on. The few
    /// received sats left over stay bitcoin.
    fn record_success(&mut self, budget_sats: u64, spent_sats: u128) {
        let forgotten = self.failures.record_success();
        if forgotten > 0 {
            tracing::info!("auto-convert succeeded; forgetting {forgotten} earlier failure(s)");
        }
        tracing::debug!("auto-convert budget {budget_sats} sats, quoted {spent_sats}");
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

/// Convert what is pending, if it is enough. Run after
/// [`AutoConvert::note_receive`] counts a receive; callers run one at a time.
///
/// `emit` sends a protocol event. On a pause it gets the new state and the
/// legacy [`ProtocolEvent::StableBalancePaused`] (the current gui records
/// "off" from it, so its next reconcile doesn't turn auto-convert back on).
/// Both are emitted under the lock, so they can't land after a switch the
/// user made just after the pause.
///
/// The lock is held only between SDK calls, so the user's switch, its reads
/// and new receives never wait for a conversion.
pub async fn convert_pending<C: Converter>(
    auto_convert: &Mutex<AutoConvert>,
    converter: &C,
    emit: &(dyn Fn(ProtocolEvent) + Send + Sync),
) {
    if !auto_convert.lock().await.has_pending() {
        return;
    }

    let failed = |reason: String| async move {
        let mut ac = auto_convert.lock().await;
        if ac.saved.enabled {
            trip(&mut ac, 0, &reason, false, emit);
        }
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
            Ok(Step::Convert { budget_sats }) => (budget_sats, ac.epoch),
            Ok(Step::Idle | Step::Defer) => return,
            Err(e) => {
                // The pending sats stay pending, and nothing is sent.
                let reason = format!("could not save the auto-convert state: {e}");
                trip(&mut ac, 0, &reason, false, emit);
                return;
            }
        }
    };

    let outcome = convert(converter, budget_sats, limits, auto_convert, epoch).await;
    let mut ac = auto_convert.lock().await;
    if ac.epoch != epoch {
        // Switched off (and maybe on again) meanwhile, which dropped the
        // pending sats and reset the breaker. This outcome belongs to the
        // old setting.
        tracing::info!("auto-convert changed during a conversion: {outcome:?}");
        return;
    }
    match outcome {
        Ok(spent_sats) => {
            tracing::info!("auto-converted about {spent_sats} sats to USDB");
            ac.record_success(budget_sats, spent_sats);
        }
        Err(Shortfall::TooSmall) => ac.give_back(budget_sats),
        // Only after the switch changed, which the epoch check above
        // already returns for.
        Err(Shortfall::Cancelled) => {}
        Err(Shortfall::Failed { reason, sent }) => {
            trip(&mut ac, budget_sats, &reason, sent, emit);
        }
    }
}

/// The auto-convert worker: one [`convert_pending`] pass per wake-up, so
/// conversions never overlap. Wake-ups that queue during a pass collapse into
/// one more.
///
/// `connected` gives the converter and the event emitter for a pass, or
/// `None` before the SDK is ready. A pass skipped for that keeps everything
/// pending, so the server wakes the worker again once the SDK is published.
/// Exits when every wake sender is gone.
pub async fn run_worker<C, E, F, Fut>(
    mut wake: tokio::sync::mpsc::UnboundedReceiver<()>,
    auto_convert: Arc<Mutex<AutoConvert>>,
    mut connected: F,
) where
    C: Converter,
    E: Fn(ProtocolEvent) + Send + Sync,
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<(C, E)>>,
{
    while wake.recv().await.is_some() {
        while wake.try_recv().is_ok() {}
        let Some((converter, emit)) = connected().await else {
            continue;
        };
        convert_pending(&auto_convert, &converter, &emit).await;
    }
}

/// The token label SDK Stable Balance saved as active for the wallet with
/// identity `identity_pubkey` (hex), or `None` if it saved none.
///
/// Read-only: it opens the SDK's own SQLite store for that identity, under
/// `storage_dir`, as the SDK's builder does (`default_storage_path`,
/// `SqliteStorage::new`, already migrated by the running SDK), and reads one
/// `settings` row. Nothing is written or deleted, and no Stable Balance is
/// configured, so nothing converts. A missing store is `None`, without
/// creating one.
pub async fn legacy_stable_balance_label(
    storage_dir: &str,
    identity_pubkey: &str,
) -> Result<Option<String>, String> {
    let identity = PublicKey::from_str(identity_pubkey).map_err(|e| e.to_string())?;
    let db_dir = default_storage_path(storage_dir, &SparkNetwork::Mainnet, &identity)
        .map_err(|e| e.to_string())?;
    if !db_dir.exists() {
        return Ok(None);
    }
    SqliteStorage::new(&db_dir)
        .map_err(|e| e.to_string())?
        .get_cached_item(LEGACY_LABEL_KEY.to_string())
        .await
        .map_err(|e| e.to_string())
}

/// Record a failure and, when it pauses auto-convert, emit the pause. The
/// caller holds the lock.
fn trip(
    ac: &mut AutoConvert,
    budget_sats: u64,
    reason: &str,
    sent: bool,
    emit: &(dyn Fn(ProtocolEvent) + Send + Sync),
) {
    if let Some(failures) = ac.record_failure(budget_sats, reason, sent) {
        emit(ProtocolEvent::AutoConvertStateChanged { state: ac.state() });
        emit(ProtocolEvent::StableBalancePaused {
            reason: reason.to_string(),
            failures,
        });
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

    /// What the server does: the listener counts each receipt as it
    /// arrives, and the worker, woken by one that counted, converts. Returns
    /// the events emitted.
    async fn process<C: Converter>(
        ac: &Mutex<AutoConvert>,
        converter: Option<&C>,
        receipts: &[Receipt],
    ) -> Vec<ProtocolEvent> {
        let mut woken = false;
        for receipt in receipts {
            woken |= ac.lock().await.note_receive(receipt);
        }
        let events = StdMutex::new(Vec::new());
        if let (true, Some(converter)) = (woken, converter) {
            convert_pending(ac, converter, &|event| events.lock().unwrap().push(event)).await;
        }
        events.into_inner().unwrap()
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
        /// Refuse a quote costing fewer sats than `limits.min_from_sats`, as
        /// Flashnet's `simulate_swap` does.
        refuse_below_min_input: bool,
        calls: StdMutex<Vec<Call>>,
        /// Sats each send spent: exactly the quote's, as the SDK's exact-input
        /// conversion does ([`exact_input`]).
        spent: StdMutex<Vec<u128>>,
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
                refuse_below_min_input: false,
                calls: StdMutex::new(Vec::new()),
                spent: StdMutex::new(Vec::new()),
            }
        }

        /// Flashnet's shape for bitcoin → USDB with a bitcoin-side minimum:
        /// a minimum input only (`get_min_amounts` never states both), which
        /// every quote must cost.
        fn with_min_input_only(balance_sats: u64, min_sats: u128) -> Self {
            Self {
                limits: Limits {
                    min_from_sats: Some(min_sats),
                    min_to_units: None,
                },
                refuse_below_min_input: true,
                ..Self::new(balance_sats)
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
            let sats_in = self.sats_for(usdb_target);
            if let Some(min) = self.limits.min_from_sats {
                if self.refuse_below_min_input && sats_in < min {
                    // Flashnet's wording (`ensure_min_amounts` at the pin).
                    return Err(format!(
                        "Amount in {sats_in} is less than minimum required {min} for asset btc"
                    ));
                }
            }
            Ok(Quote {
                sats_in,
                prepared: usdb_target,
            })
        }

        async fn send(&self, prepared: u128) -> Result<(), String> {
            self.calls.lock().unwrap().push(Call::Send(prepared));
            if let Some(reason) = self.fail_send.lock().unwrap().pop_front() {
                return Err(reason);
            }
            self.spent.lock().unwrap().push(self.sats_for(prepared));
            Ok(())
        }
    }

    /// A shared pool, for the worker tests that run in a spawned task.
    #[async_trait]
    impl Converter for Arc<FakePool> {
        type Prepared = u128;
        async fn limits(&self) -> Result<Limits, String> {
            self.as_ref().limits().await
        }
        async fn balance_sats(&self) -> Result<u64, String> {
            self.as_ref().balance_sats().await
        }
        async fn quote(&self, target: u128) -> Result<Quote<u128>, String> {
            self.as_ref().quote(target).await
        }
        async fn send(&self, prepared: u128) -> Result<(), String> {
            self.as_ref().send(prepared).await
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
        assert_eq!(ac.take(1_000_000, Some(1_000)).unwrap(), Step::Idle);

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
        // The few sats left over stay bitcoin: the actual debit isn't
        // observed, so the quote's leftover isn't known to be unspent.
        assert_eq!(ac.lock().await.saved.pending_sats, 0);
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

    /// Flashnet states only a bitcoin-side minimum for bitcoin → USDB and
    /// refuses any quote under it, the 1 USDB probe included (1,005 sats
    /// here). The probe is retried larger, and the receive converts once,
    /// within its sats, with no failure counted (#965 F1).
    #[tokio::test]
    async fn an_input_side_minimum_prices_with_a_larger_probe() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::with_min_input_only(100_000, 2_000);
        assert!(pool.sats_for(PROBE_TARGET_UNITS) < 2_000);

        let events = process(&ac, Some(&pool), &[receipt("a", 20_000)]).await;
        assert!(events.is_empty(), "{events:?}");

        let calls = pool.calls();
        assert_eq!(
            calls[2..4],
            [Call::Quote(1_000_000), Call::Quote(10_000_000)]
        );
        let sent = pool.sends();
        assert_eq!(sent.len(), 1);
        let spent = pool.sats_for(sent[0]);
        assert!((19_800..=20_000).contains(&spent), "spent {spent}");
        let ac = ac.lock().await;
        assert_eq!(ac.failures.consecutive, 0, "no failure counted");
        assert_eq!(ac.saved.pending_sats, 0);
    }

    /// A receive of exactly the minimum input waits for the next one, unquoted
    /// and without a failure; three in a row never pause (#965 F1).
    #[tokio::test]
    async fn receives_at_the_input_minimum_wait_without_a_failure() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::with_min_input_only(100_000, 2_000);
        let quotes = || {
            pool.calls()
                .iter()
                .filter(|c| matches!(c, Call::Quote(_)))
                .count()
        };

        process(&ac, Some(&pool), &[receipt("a", 2_000)]).await;
        assert_eq!(quotes(), 0, "nothing quoted");
        assert_eq!(ac.lock().await.saved.pending_sats, 2_000);

        // With the second, 4,000 sats are pending, and they convert.
        process(&ac, Some(&pool), &[receipt("b", 2_000)]).await;
        let sent = pool.sends();
        assert_eq!(sent.len(), 1);
        let spent = pool.sats_for(sent[0]);
        assert!((2_000..=4_000).contains(&spent), "spent {spent}");

        process(&ac, Some(&pool), &[receipt("c", 2_000)]).await;
        let ac = ac.lock().await;
        assert_eq!(
            ac.state(),
            AutoConvertState {
                enabled: true,
                breaker: AutoConvertBreaker::Ready,
            }
        );
        assert_eq!(ac.failures.consecutive, 0);
        assert_eq!(ac.saved.pending_sats, 2_000);
        assert_eq!(pool.sends().len(), 1);
    }

    /// Just over the minimum input, a budget whose sizing aim could round a
    /// quote under it waits unquoted; from the first one clear of that, it
    /// converts at once. Neither way does a refused quote count (#965 F1).
    #[tokio::test]
    async fn budgets_just_over_the_input_minimum_never_fail() {
        let (mut waited, mut converted) = (0, 0);
        for budget in 2_000..=2_030u64 {
            let dir = TempDir::new();
            let ac = Mutex::new(enabled(&dir));
            let pool = FakePool::with_min_input_only(100_000, 2_000);
            process(&ac, Some(&pool), &[receipt("a", budget)]).await;
            let ac = ac.lock().await;
            assert_eq!(ac.failures.consecutive, 0, "budget {budget}");
            match pool.sends()[..] {
                [] => {
                    waited += 1;
                    assert_eq!(ac.saved.pending_sats, budget);
                    assert!(
                        !pool.calls().iter().any(|c| matches!(c, Call::Quote(_))),
                        "budget {budget} was quoted"
                    );
                }
                [target] => {
                    converted += 1;
                    let spent = pool.sats_for(target);
                    assert!(
                        (2_000..=u128::from(budget)).contains(&spent),
                        "budget {budget} spent {spent}"
                    );
                }
                ref more => panic!("budget {budget}: {more:?}"),
            }
        }
        assert_eq!((waited, converted), (6, 25), "2,000-2,005 wait");
    }

    /// A probe refused at every size is one ordinary failure, after a bounded
    /// number of quotes, and the unsent sats stay pending.
    #[tokio::test]
    async fn a_probe_refused_at_every_size_is_one_failure() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        // 1,000 USDB costs 1,005,000 sats, still under this minimum.
        let pool = FakePool::with_min_input_only(5_000_000, 2_000_000);

        process(&ac, Some(&pool), &[receipt("a", 3_000_000)]).await;
        let quotes: Vec<Call> = pool
            .calls()
            .into_iter()
            .filter(|c| matches!(c, Call::Quote(_)))
            .collect();
        assert_eq!(
            quotes,
            [
                Call::Quote(1_000_000),
                Call::Quote(10_000_000),
                Call::Quote(100_000_000),
                Call::Quote(1_000_000_000),
            ]
        );
        assert!(pool.sends().is_empty());
        let ac = ac.lock().await;
        assert_eq!(ac.failures.consecutive, 1);
        assert_eq!(ac.saved.pending_sats, 3_000_000, "not sent: pending again");
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

    /// The send spends exactly the quoted sats: the prepared amount becomes
    /// the estimate's output, which makes the SDK convert
    /// `AmountIn(estimate.amount_in)` instead of re-pricing a target. Its
    /// selector is `amount >= amount_out` (`uses_amount_in` at the pin).
    #[test]
    fn a_send_spends_exactly_the_quoted_sats() {
        let mut prepared = prepared(
            OWN,
            Some(USDB_MAINNET_TOKEN_IDENTIFIER),
            conversion_options(),
        );
        let estimate = prepared.conversion_estimate.clone().unwrap();
        assert!(
            prepared.amount < estimate.amount_out,
            "a target, as prepared"
        );
        exact_input(&mut prepared);
        assert_eq!(prepared.amount, estimate.amount_out);
        assert!(prepared.amount >= estimate.amount_out);
        // Still the checked self-transfer, at the same sats.
        assert_eq!(checked_sats_in(&prepared, OWN), Ok(estimate.amount_in));

        let mut plain = prepared.clone();
        plain.conversion_estimate = None;
        plain.amount = 7;
        exact_input(&mut plain);
        assert_eq!(plain.amount, 7, "nothing to do without a conversion");
    }

    /// Turning auto-convert off while a quote is in flight stops the send:
    /// the switch is checked under the lock right before sending.
    #[tokio::test]
    async fn turning_it_off_during_a_quote_stops_the_send() {
        struct OffDuringQuote<'a> {
            pool: &'a FakePool,
            ac: &'a Mutex<AutoConvert>,
        }
        #[async_trait]
        impl Converter for OffDuringQuote<'_> {
            type Prepared = u128;
            async fn limits(&self) -> Result<Limits, String> {
                self.pool.limits().await
            }
            async fn balance_sats(&self) -> Result<u64, String> {
                self.pool.balance_sats().await
            }
            async fn quote(&self, target: u128) -> Result<Quote<u128>, String> {
                let quote = self.pool.quote(target).await?;
                if target != 1_000_000 {
                    self.ac.lock().await.set_enabled(false).unwrap();
                }
                Ok(quote)
            }
            async fn send(&self, prepared: u128) -> Result<(), String> {
                self.pool.send(prepared).await
            }
        }

        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::new(100_000);
        let off = OffDuringQuote {
            pool: &pool,
            ac: &ac,
        };
        let events = process(&ac, Some(&off), &[receipt("a", 20_000)]).await;
        assert!(events.is_empty());
        assert!(pool.sends().is_empty(), "sent after Off");
        let ac = ac.lock().await;
        assert!(!ac.state().enabled);
        assert_eq!(ac.saved.pending_sats, 0);
        assert_eq!(ac.failures.consecutive, 0);
    }

    /// A conversion that was running when the user switched off (and on
    /// again) books nothing: the switch already dropped what was pending,
    /// and its prepare failure must not hand those sats back.
    #[tokio::test]
    async fn a_switch_flipped_mid_conversion_drops_its_outcome() {
        struct FlipsTheSwitch<'a> {
            pool: &'a FakePool,
            ac: &'a Mutex<AutoConvert>,
        }
        #[async_trait]
        impl Converter for FlipsTheSwitch<'_> {
            type Prepared = u128;
            async fn limits(&self) -> Result<Limits, String> {
                self.pool.limits().await
            }
            async fn balance_sats(&self) -> Result<u64, String> {
                self.pool.balance_sats().await
            }
            async fn quote(&self, target: u128) -> Result<Quote<u128>, String> {
                let mut ac = self.ac.lock().await;
                ac.set_enabled(false).unwrap();
                ac.set_enabled(true).unwrap();
                drop(ac);
                let _ = self.pool.quote(target).await;
                Err("Pool has no liquidity".into())
            }
            async fn send(&self, prepared: u128) -> Result<(), String> {
                self.pool.send(prepared).await
            }
        }

        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::new(100_000);
        let flips = FlipsTheSwitch {
            pool: &pool,
            ac: &ac,
        };
        process(&ac, Some(&flips), &[receipt("a", 20_000)]).await;
        let ac = ac.lock().await;
        assert_eq!(ac.saved.pending_sats, 0, "bitcoin from before the switch");
        assert_eq!(
            ac.failures.consecutive, 0,
            "not counted against the new run"
        );
    }

    /// A receive that lands while a conversion runs is counted (and saved)
    /// at once, and is still pending afterwards.
    #[tokio::test]
    async fn a_receive_during_a_conversion_is_kept() {
        struct ReceivesMidSend<'a> {
            pool: &'a FakePool,
            ac: &'a Mutex<AutoConvert>,
        }
        #[async_trait]
        impl Converter for ReceivesMidSend<'_> {
            type Prepared = u128;
            async fn limits(&self) -> Result<Limits, String> {
                self.pool.limits().await
            }
            async fn balance_sats(&self) -> Result<u64, String> {
                self.pool.balance_sats().await
            }
            async fn quote(&self, target: u128) -> Result<Quote<u128>, String> {
                self.pool.quote(target).await
            }
            async fn send(&self, prepared: u128) -> Result<(), String> {
                assert!(self.ac.lock().await.note_receive(&receipt("late", 3_000)));
                self.pool.send(prepared).await
            }
        }

        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::new(100_000);
        let mid = ReceivesMidSend {
            pool: &pool,
            ac: &ac,
        };
        process(&ac, Some(&mid), &[receipt("a", 20_000)]).await;
        let pending = ac.lock().await.saved.pending_sats;
        assert_eq!(pending, 3_000);
        assert_eq!(AutoConvert::load(&dir.0).saved.pending_sats, pending);
    }

    /// A pause the disk refused is written by the next switch, even when
    /// that switch matches what is in memory.
    #[test]
    fn a_pause_that_could_not_be_saved_is_saved_by_the_next_switch() {
        let dir = TempDir::new();
        let cube = dir.0.join("cube");
        fs::create_dir_all(&cube).unwrap();
        let mut ac = AutoConvert::load(&cube);
        ac.set_enabled(true).unwrap();

        let moved = dir.0.join("moved");
        fs::rename(&cube, &moved).unwrap();
        for _ in 0..STABLE_BALANCE_PAUSE_THRESHOLD {
            ac.record_failure(0, "busy", false);
        }
        assert!(!ac.state().enabled);
        fs::rename(&moved, &cube).unwrap();
        assert!(
            AutoConvert::load(&cube).state().enabled,
            "the pause wasn't saved"
        );

        assert!(!ac.set_enabled(false).unwrap(), "already off in memory");
        assert!(
            !AutoConvert::load(&cube).state().enabled,
            "off is on disk now"
        );
    }

    /// A conversion's pending debit must be on disk before it sends. If it
    /// can't be saved, nothing is sent and the sats stay pending; after a
    /// restart they convert once, not twice.
    #[tokio::test]
    async fn a_pending_debit_that_cannot_be_saved_sends_nothing() {
        let dir = TempDir::new();
        let cube = dir.0.join("cube");
        fs::create_dir_all(&cube).unwrap();
        let mut first = AutoConvert::load(&cube);
        first.set_enabled(true).unwrap();
        assert!(first.note_receive(&receipt("a", 20_000)));
        let ac = Mutex::new(first);
        let pool = FakePool::new(100_000);

        let moved = dir.0.join("moved");
        fs::rename(&cube, &moved).unwrap();
        let events = StdMutex::new(Vec::new());
        convert_pending(&ac, &pool, &|event| events.lock().unwrap().push(event)).await;
        assert!(pool.sends().is_empty(), "sent without a saved debit");
        assert_eq!(ac.lock().await.saved.pending_sats, 20_000);
        assert_eq!(
            ac.lock().await.failures.consecutive,
            1,
            "reported as a failure"
        );
        fs::rename(&moved, &cube).unwrap();

        // Restart: the same sats load as pending and convert exactly once.
        let ac = Mutex::new(AutoConvert::load(&cube));
        assert_eq!(ac.lock().await.saved.pending_sats, 20_000);
        convert_pending(&ac, &pool, &|_| {}).await;
        convert_pending(&ac, &pool, &|_| {}).await;
        assert_eq!(pool.sends().len(), 1);
    }

    /// The pause events go out while the lock is held, so a switch the user
    /// makes right after can't be overtaken by them.
    #[tokio::test]
    async fn the_pause_is_emitted_under_the_lock() {
        let dir = TempDir::new();
        let ac = Mutex::new(enabled(&dir));
        let pool = FakePool::new(100_000);
        for _ in 0..STABLE_BALANCE_PAUSE_THRESHOLD {
            pool.fail_quote.lock().unwrap().push_back("busy".into());
        }
        let held = StdMutex::new(Vec::new());
        for id in ["a", "b", "c"] {
            assert!(ac.lock().await.note_receive(&receipt(id, 5_000)));
            convert_pending(&ac, &pool, &|_| {
                held.lock().unwrap().push(ac.try_lock().is_err())
            })
            .await;
        }
        assert_eq!(held.into_inner().unwrap(), vec![true, true]);
    }

    #[test]
    fn the_sizing_aim_leaves_room_for_rounding() {
        assert_eq!(sizing_aim(20_000), 20_000 - 20 - 1);
        assert_eq!(sizing_aim(1_300), 1_300 - 1 - 1);
        assert_eq!(sizing_aim(1), 0);
        assert_eq!(sizing_aim(0), 0);
    }

    // ---- the worker ----

    /// Poll until `done` holds, for up to a second.
    async fn eventually(done: impl Fn() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("condition not met within a second");
    }

    type Emitted = Arc<StdMutex<Vec<ProtocolEvent>>>;

    /// Start [`run_worker`] with `pool` as the converter once `connected` is
    /// set. Returns the wake sender.
    fn start_worker(
        ac: &Arc<Mutex<AutoConvert>>,
        pool: &Arc<FakePool>,
        connected: &Arc<std::sync::atomic::AtomicBool>,
        emitted: &Emitted,
    ) -> tokio::sync::mpsc::UnboundedSender<()> {
        let (wake, wakes) = tokio::sync::mpsc::unbounded_channel();
        let (pool, connected, emitted) = (pool.clone(), connected.clone(), emitted.clone());
        tokio::spawn(run_worker(wakes, ac.clone(), move || {
            let (pool, connected, emitted) = (pool.clone(), connected.clone(), emitted.clone());
            async move {
                connected
                    .load(std::sync::atomic::Ordering::SeqCst)
                    .then(|| {
                        let emit = move |event| emitted.lock().unwrap().push(event);
                        (pool, emit)
                    })
            }
        }));
        wake
    }

    /// A receive the listener counted before the SDK was published wakes a
    /// pass that can't convert. The sats stay pending, and the wake `init`
    /// sends after publishing the SDK converts them, with no new receive.
    #[tokio::test]
    async fn a_wake_before_the_sdk_is_published_is_not_lost() {
        let dir = TempDir::new();
        let ac = Arc::new(Mutex::new(enabled(&dir)));
        let pool = Arc::new(FakePool::new(100_000));
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wake = start_worker(&ac, &pool, &connected, &Emitted::default());

        assert!(ac.lock().await.note_receive(&receipt("early", 20_000)));
        wake.send(()).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(pool.calls().is_empty());
        assert_eq!(ac.lock().await.saved.pending_sats, 20_000);

        // `init`: publish the SDK, then wake.
        connected.store(true, std::sync::atomic::Ordering::SeqCst);
        wake.send(()).unwrap();
        eventually(|| pool.sends().len() == 1).await;
    }

    /// Sats saved pending by the last session convert on the start-up wake.
    #[tokio::test]
    async fn sats_pending_from_the_last_session_convert_at_start_up() {
        let dir = TempDir::new();
        {
            let mut last = enabled(&dir);
            assert!(last.note_receive(&receipt("before-restart", 9_000)));
        }
        let ac = Arc::new(Mutex::new(AutoConvert::load(&dir.0)));
        assert_eq!(ac.lock().await.saved.pending_sats, 9_000);
        let pool = Arc::new(FakePool::new(100_000));
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let wake = start_worker(&ac, &pool, &connected, &Emitted::default());

        wake.send(()).unwrap();
        eventually(|| pool.sends().len() == 1).await;
        let spent = pool.spent.lock().unwrap()[0];
        assert!((8_900..=9_000).contains(&spent), "spent {spent}");
    }

    // ---- migrating SDK Stable Balance's choice ----

    /// The secp256k1 generator, as a stand-in identity.
    const IDENTITY: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    /// The label is read from the same SQLite store, row and key the SDK
    /// wrote it to. Nothing is created where there was no store.
    #[tokio::test]
    async fn the_legacy_label_is_read_from_the_sdk_store() {
        let dir = TempDir::new();
        let root = dir.0.to_str().unwrap();
        assert_eq!(legacy_stable_balance_label(root, IDENTITY).await, Ok(None));
        assert_eq!(
            fs::read_dir(&dir.0).unwrap().count(),
            0,
            "no store is created"
        );

        let identity = PublicKey::from_str(IDENTITY).unwrap();
        let db_dir = default_storage_path(root, &SparkNetwork::Mainnet, &identity).unwrap();
        let sdk_store = SqliteStorage::new(&db_dir).unwrap();
        assert_eq!(legacy_stable_balance_label(root, IDENTITY).await, Ok(None));
        // Written the way the SDK's `save_stable_balance_active_label` does,
        // under its own key, spelled out here so a drifting constant fails.
        sdk_store
            .set_cached_item("stable_balance_active_label".into(), "USDB".into())
            .await
            .unwrap();
        assert_eq!(
            legacy_stable_balance_label(root, IDENTITY).await,
            Ok(Some(LEGACY_USDB_LABEL.to_string()))
        );
        // Read-only: the SDK's row is still there.
        assert_eq!(
            sdk_store
                .get_cached_item("stable_balance_active_label".into())
                .await
                .unwrap()
                .as_deref(),
            Some(LEGACY_USDB_LABEL)
        );
    }

    /// A Cube that had SDK Stable Balance on and no bridge state yet starts
    /// on, with nothing pending; a Cube with a state file keeps its own.
    #[test]
    fn a_legacy_on_choice_is_adopted_only_on_the_first_run() {
        let dir = TempDir::new();
        let ac = AutoConvert::load_or_adopt(&dir.0, true);
        assert_eq!(
            ac.state(),
            AutoConvertState {
                enabled: true,
                breaker: AutoConvertBreaker::Ready,
            }
        );
        assert_eq!(ac.saved.pending_sats, 0);
        assert!(AutoConvert::load(&dir.0).state().enabled, "saved");

        let mut ac = ac;
        ac.set_enabled(false).unwrap();
        assert!(
            !AutoConvert::load_or_adopt(&dir.0, true).state().enabled,
            "the user's later off wins over the old SDK choice"
        );

        let fresh = TempDir::new();
        assert!(!AutoConvert::load_or_adopt(&fresh.0, false).state().enabled);
        assert!(!fresh.file().exists(), "nothing to save");
    }

    /// An adoption that couldn't be saved leaves memory on with no file. An
    /// On then saves it before it is acknowledged, or fails (#965 F8).
    #[test]
    fn on_over_an_unsaved_adoption_saves_it_or_fails() {
        let dir = TempDir::new();
        // No directory yet, so the adoption's write fails.
        let cube = dir.0.join("cube");
        let mut ac = AutoConvert::load_or_adopt(&cube, true);
        assert!(ac.state().enabled);
        assert!(ac.unsaved);

        assert!(ac.set_enabled(true).is_err(), "still unsaveable");
        fs::create_dir_all(&cube).unwrap();
        assert!(!ac.set_enabled(true).unwrap(), "already on");
        assert!(!ac.unsaved);
        assert!(AutoConvert::load(&cube).state().enabled, "on is on disk");
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
