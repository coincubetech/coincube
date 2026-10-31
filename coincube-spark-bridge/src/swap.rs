//! BTC ⇄ USDB swaps the user asks for (#950 S3b): [`Method::PrepareSwap`],
//! [`Method::Swap`] and [`Method::GetConversionLimits`].
//!
//! A swap is a self-transfer to the wallet's own Spark address carrying
//! conversion options, which the SDK runs as a
//! `ConversionPurpose::SelfTransfer` (`conversion_purpose_for_identity` at
//! the pin), the same shape as an auto-convert ([`crate::auto_convert`]).
//!
//! - **Out of USDB** the SDK takes an exact token input: the request names the
//!   USDB amount, and the SDK prepares `AmountIn` ([`usdb_request`]).
//! - **Out of bitcoin** the SDK takes only a USDB target. The sats are turned
//!   into a target by auto-convert's own sizing ([`fitted_quote`]): a probe
//!   prices the pool, the target is scaled to the sats and re-prepared until
//!   the quote fits. The checked prepare then spends exactly the quoted sats
//!   ([`auto_convert::exact_input`]).
//!
//! Both directions are therefore exact inputs at send time, and the SDK
//! re-prices them then: it simulates the swap again and signs that output
//! less the slippage cap as the minimum (`resolve_conversion` for `AmountIn`
//! in `flashnet.rs` at the pin). The cap alone bounds slippage against the
//! price at send, not against the estimate the user reviewed. So [`execute`]
//! re-prices the swap just before sending it and refuses one whose price has
//! moved against the user by more than the cap since the estimate
//! ([`within_cap`]).
//!
//! Every refusal before the SDK is asked to execute is
//! [`Refusal::BadRequest`]: nothing was sent. Once `send_payment` is called,
//! every error is [`Refusal::Sdk`], an unknown outcome
//! ([`Method::Swap`]).
//!
//! Known limit: a swap and an auto-convert pass are not coordinated. Both
//! spend from the same balances, so when they overlap one or both may fail,
//! or both may go through against the balance available. The balance re-read
//! right before a swap is sent ([`execute`]) is the only guard, and it reads
//! the SDK's cached balance (`ensure_synced` waits only for the first sync).
//!
//! The bitcoin a USDB → bitcoin swap delivers isn't auto-converted back:
//! passes are held while it is sent ([`AutoConvert::begin_swap`]), and the
//! swap's receive is taken back out if the SDK reported it before marking it
//! a conversion's. A swap whose send ends in an error can't say which receive
//! was its own, and the pool may have run it: every receive counted while it
//! was sent is taken back, and none count until the switch next changes
//! ([`SwapEnd::Unknown`]). That hold is in memory, so it ends with the bridge.
//!
//! [`Method::PrepareSwap`]: coincube_spark_protocol::Method::PrepareSwap
//! [`Method::Swap`]: coincube_spark_protocol::Method::Swap
//! [`Method::GetConversionLimits`]: coincube_spark_protocol::Method::GetConversionLimits

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use breez_sdk_spark::{AmountAdjustmentReason, ConversionProvider};
use breez_sdk_spark::{
    ConversionDetails, ConversionEstimate, ConversionInfo, ConversionOptions, ConversionSide,
    ConversionStatus, ConversionType, FetchConversionLimitsRequest, GetInfoRequest,
    GetPaymentRequest, Payment, PaymentDetails, PaymentRequest, PaymentStatus,
    PrepareSendPaymentRequest, PrepareSendPaymentResponse, SendPaymentMethod, SendPaymentRequest,
};
use coincube_spark_protocol::{
    AmountAdjustment, AssetAmount, ConversionAmounts, ConversionLimits, ConversionQuote,
    Event as ProtocolEvent, PaymentConversionSummary, SwapAmount, SwapStatus, WalletAsset,
    CONVERSION_MAX_SLIPPAGE_BPS,
};
use tokio::sync::Mutex;

use crate::auto_convert::{
    self, fitted_quote, AutoConvert, Converter, Limits, SdkConverter, Shortfall, SwapEnd,
};
use crate::sdk_adapter::USDB_MAINNET_TOKEN_IDENTIFIER;

/// The slippage caps a swap accepts, the same range as
/// [`coincube_spark_protocol::PrepareCrossChainParams::max_slippage_bps`].
const SLIPPAGE_RANGE_BPS: std::ops::RangeInclusive<u32> = 10..=500;

/// The cap a swap prepares at: [`CONVERSION_MAX_SLIPPAGE_BPS`] for `None`, an
/// explicit cap as asked, or a refusal outside [`SLIPPAGE_RANGE_BPS`].
pub fn slippage_cap(requested: Option<u32>) -> Result<u32, String> {
    match requested {
        None => Ok(CONVERSION_MAX_SLIPPAGE_BPS),
        Some(bps) if SLIPPAGE_RANGE_BPS.contains(&bps) => Ok(bps),
        Some(bps) => Err(format!(
            "max_slippage_bps must be between {} and {}, got {bps}",
            SLIPPAGE_RANGE_BPS.start(),
            SLIPPAGE_RANGE_BPS.end()
        )),
    }
}

/// Why a swap call didn't succeed.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Refused before anything was sent: a definite failure.
    BadRequest(String),
    /// The SDK failed. On a prepare nothing moved; on [`execute`] the swap
    /// may still complete.
    Sdk(String),
}

/// Synced balances: sats, and USDB base units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Balances {
    pub sats: u64,
    pub usdb_units: u128,
}

/// Minimums for a USDB → bitcoin conversion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsdbLimits {
    /// In USDB base units.
    pub min_from_units: Option<u128>,
    /// In sats.
    pub min_to_sats: Option<u128>,
}

/// The SDK calls a swap needs, on top of auto-convert's bitcoin → USDB
/// quote ([`Converter::quote`], checked and exact-input). The server
/// implements it on [`SdkConverter`]; tests use a fake.
#[async_trait]
pub trait SwapBackend: Converter<Prepared = PrepareSendPaymentResponse> {
    /// Both balances, read again (`ensure_synced`: after the first sync this
    /// is the SDK's cached balance).
    async fn balances(&self) -> Result<Balances, String>;
    async fn usdb_limits(&self) -> Result<UsdbLimits, String>;
    /// Prepare (without sending) a conversion of exactly `usdb_units` into
    /// bitcoin, checked with [`checked_usdb_prepare`].
    async fn quote_usdb(&self, usdb_units: u128) -> Result<PrepareSendPaymentResponse, String>;
    /// Send a prepared swap. Moves funds: an error doesn't prove nothing
    /// moved. Returns the payment the SDK answers with, the conversion's
    /// receive leg for a self-transfer.
    async fn execute(&self, prepared: PrepareSendPaymentResponse) -> Result<Payment, String>;
    async fn payment(&self, payment_id: &str) -> Result<Payment, String>;
}

/// The prepare for a swap out of USDB: a self-transfer to the wallet's own
/// Spark address that converts exactly `usdb_units` USDB base units into
/// bitcoin. With a token identifier and `ToBitcoin` options the SDK reads
/// the amount in token units and prepares `AmountIn`
/// (`is_token_denominated` in `sdk/payments/conversion.rs` at the pin).
pub fn usdb_request(
    own_spark_address: &str,
    usdb_units: u128,
    max_slippage_bps: u32,
) -> PrepareSendPaymentRequest {
    PrepareSendPaymentRequest {
        payment_request: PaymentRequest::Input {
            input: own_spark_address.to_string(),
        },
        amount: Some(usdb_units),
        token_identifier: Some(USDB_MAINNET_TOKEN_IDENTIFIER.to_string()),
        conversion_options: Some(ConversionOptions {
            conversion_type: ConversionType::ToBitcoin {
                from_token_identifier: USDB_MAINNET_TOKEN_IDENTIFIER.to_string(),
            },
            max_slippage_bps: Some(max_slippage_bps),
            // The SDK default: 30 s to see the converted sats arrive.
            completion_timeout_secs: None,
        }),
        fee_policy: None,
    }
}

/// The estimate on a swap's prepare, after checking it converts out of
/// `from` into the other asset at `max_slippage_bps`.
fn checked_estimate(
    prepared: &PrepareSendPaymentResponse,
    from: WalletAsset,
    max_slippage_bps: u32,
) -> Result<&ConversionEstimate, String> {
    let estimate = prepared
        .conversion_estimate
        .as_ref()
        .ok_or("the SDK prepared no conversion")?;
    let direction = match (&estimate.options.conversion_type, from) {
        // Out of bitcoin, the prepare names the USDB it targets.
        (ConversionType::FromBitcoin, WalletAsset::Bitcoin) => {
            prepared.token_identifier.as_deref() == Some(USDB_MAINNET_TOKEN_IDENTIFIER)
        }
        // Out of USDB, the SDK clears the token identifier: the output is
        // sats (`response_token_identifier` at the pin).
        (
            ConversionType::ToBitcoin {
                from_token_identifier,
            },
            WalletAsset::Usdb,
        ) => {
            from_token_identifier == USDB_MAINNET_TOKEN_IDENTIFIER
                && prepared.token_identifier.is_none()
        }
        _ => false,
    };
    if !direction || estimate.options.max_slippage_bps != Some(max_slippage_bps) {
        return Err(format!(
            "the SDK prepared a different conversion: {:?} for token {:?}",
            estimate.options, prepared.token_identifier
        ));
    }
    Ok(estimate)
}

/// Check a prepare made from [`usdb_request`] before it can be held for a
/// swap: a self-transfer to `own_spark_address`, out of USDB at
/// `max_slippage_bps`, converting `usdb_units`, or more only with an
/// adjustment the SDK reports. It must also send an exact input: the SDK
/// converts `AmountIn` only while the prepared amount is at least the
/// estimate's output (`uses_amount_in` at the pin).
pub fn checked_usdb_prepare(
    prepared: &PrepareSendPaymentResponse,
    own_spark_address: &str,
    usdb_units: u128,
    max_slippage_bps: u32,
) -> Result<(), String> {
    match &prepared.payment_method {
        SendPaymentMethod::SparkAddress { address, .. } if address == own_spark_address => {}
        other => {
            return Err(format!(
                "the swap was prepared for an unexpected destination: {other:?}"
            ))
        }
    }
    let estimate = checked_estimate(prepared, WalletAsset::Usdb, max_slippage_bps)?;
    let amount_ok = match estimate.amount_adjustment {
        None => estimate.amount_in == usdb_units,
        Some(_) => estimate.amount_in >= usdb_units,
    };
    if !amount_ok {
        return Err(format!(
            "the SDK prepared a conversion of {} USDB base units, asked for {usdb_units}",
            estimate.amount_in
        ));
    }
    if prepared.amount < estimate.amount_out {
        return Err("the SDK prepared a conversion that isn't an exact input".to_string());
    }
    Ok(())
}

/// The smallest bitcoin amount auto-convert's sizing can convert when the
/// pool's minimum input is `min_sats` ([`auto_convert::clears_min_input`]),
/// about 0.2% above it. A swap reports this as its minimum, and refuses
/// anything below it, because a quote under the minimum fails in a form the
/// bridge can't read.
///
/// A binary search: whether a budget clears only grows with the budget, and
/// twice the minimum plus a few sats always clears. A minimum too large for
/// that bound comes back as `u128::MAX`, which refuses every amount.
fn least_bitcoin_input(min_sats: u128) -> u128 {
    let clears = |budget| auto_convert::clears_min_input(budget, min_sats);
    let (mut low, mut high) = (min_sats, min_sats.saturating_mul(2).saturating_add(4));
    if !clears(high) {
        return u128::MAX;
    }
    while low < high {
        let mid = low + (high - low) / 2;
        if clears(mid) {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    low
}

fn bitcoin(amount: u128) -> AssetAmount {
    AssetAmount {
        asset: WalletAsset::Bitcoin,
        amount,
    }
}

fn usdb(amount: u128) -> AssetAmount {
    AssetAmount {
        asset: WalletAsset::Usdb,
        amount,
    }
}

fn adjustment(reason: Option<&AmountAdjustmentReason>) -> Option<AmountAdjustment> {
    reason.map(|reason| match reason {
        AmountAdjustmentReason::FlooredToMinLimit => AmountAdjustment::FlooredToMinLimit,
        AmountAdjustmentReason::IncreasedToAvoidDust => AmountAdjustment::IncreasedToAvoidDust,
    })
}

/// The minimums for a swap out of `from`, as the protocol reports them.
pub async fn limits<B: SwapBackend>(
    backend: &B,
    from: WalletAsset,
) -> Result<ConversionLimits, String> {
    Ok(match from {
        WalletAsset::Bitcoin => bitcoin_limits(backend.limits().await?),
        WalletAsset::Usdb => usdb_limits(backend.usdb_limits().await?),
    })
}

fn bitcoin_limits(limits: Limits) -> ConversionLimits {
    ConversionLimits {
        min_from: limits.min_from_sats.map(least_bitcoin_input).map(bitcoin),
        min_to: limits.min_to_units.map(usdb),
    }
}

fn usdb_limits(limits: UsdbLimits) -> ConversionLimits {
    ConversionLimits {
        min_from: limits.min_from_units.map(usdb),
        min_to: limits.min_to_sats.map(bitcoin),
    }
}

/// A swap prepared and not yet sent.
pub struct PreparedSwap {
    from: WalletAsset,
    max_slippage_bps: u32,
    /// What the swap spends, in `from`. Checked against the synced balance
    /// right before it is sent.
    amount_in: u128,
    /// The estimated output the user reviewed, in the other asset.
    amount_out: u128,
    prepared: PrepareSendPaymentResponse,
}

impl PreparedSwap {
    pub fn max_slippage_bps(&self) -> u32 {
        self.max_slippage_bps
    }
}

/// The answer to a prepare: the swap to hold, and what to show.
pub struct Prepared {
    pub swap: PreparedSwap,
    pub quote: ConversionQuote,
    pub limits: ConversionLimits,
}

fn sdk_refusal(what: &str) -> impl Fn(String) -> Refusal + '_ {
    move |e| Refusal::Sdk(format!("could not read {what}: {e}"))
}

/// Prepare a swap of `amount` at `max_slippage_bps`, which `backend`
/// prepares at too. Minimums and balances are checked before any prepare
/// where they are known, and a quote that turns out below a minimum is
/// refused before it is held.
pub async fn prepare<B: SwapBackend>(
    backend: &B,
    amount: SwapAmount,
    max_slippage_bps: u32,
) -> Result<Prepared, Refusal> {
    match amount.from_asset() {
        WalletAsset::Usdb => prepare_from_usdb(backend, amount, max_slippage_bps).await,
        WalletAsset::Bitcoin => prepare_from_bitcoin(backend, amount, max_slippage_bps).await,
    }
}

async fn prepare_from_usdb<B: SwapBackend>(
    backend: &B,
    amount: SwapAmount,
    max_slippage_bps: u32,
) -> Result<Prepared, Refusal> {
    let limits = backend
        .usdb_limits()
        .await
        .map_err(sdk_refusal("the conversion limits"))?;
    let balances = backend
        .balances()
        .await
        .map_err(sdk_refusal("the balances"))?;
    let usdb_in = match amount {
        SwapAmount::Exact { amount_in } => amount_in.amount,
        // The whole holding, so no dust is left.
        SwapAmount::Max { .. } => balances.usdb_units,
    };
    refuse_unaffordable(usdb_in, balances.usdb_units, "USDB")?;
    // No USDB figure: base units read as a different order of magnitude.
    // The form states the minimum from the limits.
    if limits.min_from_units.is_some_and(|min| usdb_in < min) {
        return Err(Refusal::BadRequest(
            "This is below the smallest USDB swap the pool accepts.".to_string(),
        ));
    }

    let prepared = backend.quote_usdb(usdb_in).await.map_err(Refusal::Sdk)?;
    let estimate =
        checked_estimate(&prepared, WalletAsset::Usdb, max_slippage_bps).map_err(Refusal::Sdk)?;
    // Only an adjustment the SDK reports can raise the input. None is made
    // up here: a remainder below the minimum stays USDB.
    if estimate.amount_in > balances.usdb_units {
        return Err(Refusal::BadRequest(
            "The SDK raised this swap above the USDB the Spark wallet holds.".to_string(),
        ));
    }
    refuse_small_output(estimate.amount_out, limits.min_to_sats, "bitcoin")?;
    let quote = ConversionQuote {
        amount_in: usdb(estimate.amount_in),
        amount_out: bitcoin(estimate.amount_out),
        // The SDK denominates the fee in the asset converted out of.
        fee: usdb(estimate.fee),
        amount_adjustment: adjustment(estimate.amount_adjustment.as_ref()),
        max_slippage_bps,
    };
    let (amount_in, amount_out) = (estimate.amount_in, estimate.amount_out);
    Ok(Prepared {
        swap: PreparedSwap {
            from: WalletAsset::Usdb,
            max_slippage_bps,
            amount_in,
            amount_out,
            prepared,
        },
        quote,
        limits: usdb_limits(limits),
    })
}

async fn prepare_from_bitcoin<B: SwapBackend>(
    backend: &B,
    amount: SwapAmount,
    max_slippage_bps: u32,
) -> Result<Prepared, Refusal> {
    let limits = backend
        .limits()
        .await
        .map_err(sdk_refusal("the conversion limits"))?;
    let balances = backend
        .balances()
        .await
        .map_err(sdk_refusal("the balances"))?;
    let sats_in = match amount {
        SwapAmount::Exact { amount_in } => amount_in.amount,
        // A self-transfer has no Spark fee, and the conversion fee is part
        // of the sats the quote spends, so all of them can be offered.
        SwapAmount::Max { .. } => u128::from(balances.sats),
    };
    refuse_unaffordable(sats_in, u128::from(balances.sats), "bitcoin")?;
    // Affordable, so it fits in a u64.
    let budget_sats = u64::try_from(sats_in).unwrap_or(balances.sats);
    if let Some(least) = limits
        .min_from_sats
        .map(least_bitcoin_input)
        .filter(|least| sats_in < *least)
    {
        return Err(Refusal::BadRequest(format!(
            "This is below the smallest bitcoin swap the pool accepts ({least} sats)."
        )));
    }

    let quote = fitted_quote(backend, budget_sats, limits)
        .await
        .map_err(|shortfall| match shortfall {
            Shortfall::TooSmall => Refusal::BadRequest(
                "This buys less USDB than the smallest swap the pool delivers.".to_string(),
            ),
            Shortfall::Failed { reason, .. } => Refusal::Sdk(reason),
            // Only auto-convert's switch cancels; a swap has none.
            Shortfall::Cancelled => Refusal::Sdk("the swap was cancelled".to_string()),
        })?;
    let estimate = checked_estimate(&quote.prepared, WalletAsset::Bitcoin, max_slippage_bps)
        .map_err(Refusal::Sdk)?;
    refuse_small_output(estimate.amount_out, limits.min_to_units, "USDB")?;
    let conversion_quote = ConversionQuote {
        // Fitted to the sats asked for, so at most that, and exactly what the
        // send spends.
        amount_in: bitcoin(quote.sats_in),
        amount_out: usdb(estimate.amount_out),
        fee: bitcoin(estimate.fee),
        amount_adjustment: adjustment(estimate.amount_adjustment.as_ref()),
        max_slippage_bps,
    };
    let amount_out = estimate.amount_out;
    Ok(Prepared {
        swap: PreparedSwap {
            from: WalletAsset::Bitcoin,
            max_slippage_bps,
            amount_in: quote.sats_in,
            amount_out,
            prepared: quote.prepared,
        },
        quote: conversion_quote,
        limits: bitcoin_limits(limits),
    })
}

fn refuse_unaffordable(amount: u128, held: u128, asset: &str) -> Result<(), Refusal> {
    if amount == 0 {
        return Err(Refusal::BadRequest(format!(
            "There is no {asset} to swap: the amount is zero."
        )));
    }
    if amount > held {
        return Err(Refusal::BadRequest(format!(
            "This is more {asset} than the Spark wallet holds."
        )));
    }
    Ok(())
}

fn refuse_small_output(amount_out: u128, min: Option<u128>, asset: &str) -> Result<(), Refusal> {
    match min {
        Some(min) if amount_out < min => Err(Refusal::BadRequest(format!(
            "This swap would deliver less {asset} than the pool's minimum."
        ))),
        _ => Ok(()),
    }
}

/// Whether a fresh price is within `max_slippage_bps` of the reviewed one:
/// its output per unit of input at most the cap below. Compared as cross
/// products, so the two quotes need not convert the same amount.
fn within_cap(reviewed: (u128, u128), fresh: (u128, u128), max_slippage_bps: u32) -> Option<bool> {
    let ((reviewed_in, reviewed_out), (fresh_in, fresh_out)) = (reviewed, fresh);
    // A quote with nothing in or nothing out prices nothing.
    if [reviewed_in, reviewed_out, fresh_in, fresh_out].contains(&0) {
        return None;
    }
    let fresh_side = fresh_out.checked_mul(reviewed_in)?.checked_mul(10_000)?;
    let reviewed_side = reviewed_out
        .checked_mul(fresh_in)?
        .checked_mul(10_000u128.checked_sub(u128::from(max_slippage_bps))?)?;
    Some(fresh_side >= reviewed_side)
}

/// The price of `swap` now: the input and output of a fresh prepare of the
/// same conversion.
async fn current_price<B: SwapBackend>(
    backend: &B,
    swap: &PreparedSwap,
) -> Result<(u128, u128), String> {
    let prepared = match swap.from {
        WalletAsset::Usdb => backend.quote_usdb(swap.amount_in).await?,
        // Out of bitcoin the SDK takes a USDB target: the reviewed output.
        // Only the price is compared, so the sats it quotes needn't match.
        WalletAsset::Bitcoin => backend.quote(swap.amount_out).await?.prepared,
    };
    let estimate = checked_estimate(&prepared, swap.from, swap.max_slippage_bps)?;
    Ok((estimate.amount_in, estimate.amount_out))
}

/// Send a prepared swap. Returns the id of its Activity row and its status.
///
/// Before sending, the swap is re-priced ([`within_cap`]) and its input
/// checked against a fresh balance read. Either refusal is
/// [`Refusal::BadRequest`]: nothing was sent. The prepare that is sent is
/// the reviewed one, so a swap out of bitcoin spends exactly the reviewed
/// sats.
///
/// A swap out of USDB holds `auto_convert`'s passes while it is sent, so the
/// bitcoin it delivers isn't converted back ([`AutoConvert::begin_swap`]).
/// If its send ends in an error, no receive counts until the switch next
/// changes ([`SwapEnd::Unknown`]). The caller wakes the auto-convert worker
/// afterwards.
pub async fn execute<B: SwapBackend>(
    backend: &B,
    swap: PreparedSwap,
    auto_convert: Option<&Mutex<AutoConvert>>,
) -> Result<(String, SwapStatus), Refusal> {
    let not_sent = |reason: String| Refusal::BadRequest(format!("{reason} Nothing was sent."));
    let fresh = current_price(backend, &swap).await.map_err(|e| {
        not_sent(format!(
            "Could not check the price again before swapping: {e}."
        ))
    })?;
    match within_cap(
        (swap.amount_in, swap.amount_out),
        fresh,
        swap.max_slippage_bps,
    ) {
        Some(true) => {}
        Some(false) => {
            return Err(not_sent(format!(
                "The price has moved by more than {} since the estimate. Get a new estimate.",
                percent(swap.max_slippage_bps)
            )))
        }
        None => {
            return Err(not_sent(
                "Could not compare the price with the estimate.".to_string(),
            ))
        }
    }

    let balances = backend
        .balances()
        .await
        .map_err(|e| not_sent(format!("Could not read the balance before swapping: {e}.")))?;
    let (held, asset) = match swap.from {
        WalletAsset::Bitcoin => (u128::from(balances.sats), "bitcoin"),
        WalletAsset::Usdb => (balances.usdb_units, "USDB"),
    };
    if held < swap.amount_in {
        return Err(not_sent(format!(
            "The Spark wallet no longer holds enough {asset} for this swap."
        )));
    }

    // Only the bitcoin out of a USDB swap could count as a receive.
    let guard = auto_convert.filter(|_| swap.from == WalletAsset::Usdb);
    if let Some(auto_convert) = guard {
        auto_convert.lock().await.begin_swap();
    }
    let sent = backend.execute(swap.prepared).await;
    if let Some(auto_convert) = guard {
        let end = match &sent {
            // A self-transfer answers with the receive that delivered it.
            Ok(payment) => SwapEnd::Delivered {
                payment_id: &payment.id,
            },
            Err(_) => SwapEnd::Unknown,
        };
        auto_convert.lock().await.end_swap(end);
    }
    let payment = sent.map_err(Refusal::Sdk)?;
    let status = swap_status(&payment);
    Ok((payment.id, status))
}

/// `50` → `0.5%`.
fn percent(bps: u32) -> String {
    let whole = bps / 100;
    match bps % 100 {
        0 => format!("{whole}%"),
        part if part % 10 == 0 => format!("{whole}.{}%", part / 10),
        part => format!("{whole}.{part:02}%"),
    }
}

/// The conversion information on a payment row itself.
fn row_conversion_info(details: Option<&PaymentDetails>) -> Option<&ConversionInfo> {
    match details? {
        PaymentDetails::Spark {
            conversion_info, ..
        }
        | PaymentDetails::Token {
            conversion_info, ..
        }
        | PaymentDetails::Lightning {
            conversion_info, ..
        } => conversion_info.as_ref(),
        PaymentDetails::Withdraw { .. } | PaymentDetails::Deposit { .. } => None,
    }
}

fn protocol_status(status: &ConversionStatus) -> SwapStatus {
    match status {
        ConversionStatus::Pending => SwapStatus::Pending,
        ConversionStatus::Completed => SwapStatus::Completed,
        ConversionStatus::Failed => SwapStatus::Failed,
        ConversionStatus::RefundNeeded => SwapStatus::RefundNeeded,
        ConversionStatus::Refunded => SwapStatus::Refunded,
    }
}

/// A payment's conversion status, never reported done before the SDK says
/// so.
///
/// From the payment's `conversion_details` when it has them. A
/// self-transfer has none at the pin: only a conversion for an ongoing
/// payment (`sdk/payments/conversion.rs`) and Stable Balance write the
/// metadata they come from (`persist/sqlite.rs`). So the row's own AMM
/// `conversion_info`, written by `update_payment_conversion_info` in
/// `flashnet.rs`, is next. A cross-chain provider's status is not a
/// conversion status here. Without either it is `Pending`.
///
/// `Completed` also needs the payment itself to be completed: until then it
/// is `Pending`, and on a payment that failed (a transfer the wallet never
/// claimed) it is `Failed`, since nothing arrived.
pub fn swap_status(payment: &Payment) -> SwapStatus {
    let status = payment
        .conversion_details
        .as_ref()
        .map(|details| protocol_status(&details.status))
        .or_else(|| match row_conversion_info(payment.details.as_ref()) {
            Some(ConversionInfo::Amm { status, .. }) => Some(protocol_status(status)),
            Some(ConversionInfo::Orchestra { .. } | ConversionInfo::Boltz { .. }) | None => None,
        })
        .unwrap_or(SwapStatus::Pending);
    match (status, &payment.status) {
        (SwapStatus::Completed, PaymentStatus::Pending) => SwapStatus::Pending,
        (SwapStatus::Completed, PaymentStatus::Failed) => SwapStatus::Failed,
        (other, _) => other,
    }
}

/// The asset on one side of a conversion step, if it is bitcoin or USDB.
fn side_asset(side: &ConversionSide) -> Option<WalletAsset> {
    match side.asset.identifier.as_deref() {
        None => Some(WalletAsset::Bitcoin),
        Some(USDB_MAINNET_TOKEN_IDENTIFIER) => Some(WalletAsset::Usdb),
        Some(_) => None,
    }
}

/// Both sides of the BTC ⇄ USDB AMM step in `details`, if there is one.
/// The SDK builds steps only for payments with `conversion_details`
/// (`enrich_payment_conversions` at the pin), so a self-transfer's row has
/// none.
fn amm_amounts(details: &ConversionDetails) -> Option<ConversionAmounts> {
    details.conversions.iter().find_map(|step| {
        if step.provider != ConversionProvider::Amm {
            return None;
        }
        let (from, to) = (side_asset(&step.from)?, side_asset(&step.to)?);
        (from != to).then_some(ConversionAmounts {
            from: AssetAmount {
                asset: from,
                amount: step.from.amount,
            },
            to: AssetAmount {
                asset: to,
                amount: step.to.amount,
            },
        })
    })
}

/// The BTC ⇄ USDB conversion Activity shows on a payment row, or `None`
/// when the row has none.
///
/// A row is a conversion's when it carries an AMM `conversion_info` of its
/// own (a swap's or auto-convert's row, or a refund), or SDK
/// `conversion_details` that aren't a cross-chain provider's alone (a send
/// funded by a conversion). The status is [`swap_status`]. The amounts come
/// only from an SDK BTC ⇄ USDB step; none are made up, so a swap's row
/// reports `None` and Activity shows the row's own amount.
pub fn conversion_summary(payment: &Payment) -> Option<PaymentConversionSummary> {
    let row_info = row_conversion_info(payment.details.as_ref());
    let amounts = payment.conversion_details.as_ref().and_then(amm_amounts);
    let cross_chain = matches!(
        row_info,
        Some(ConversionInfo::Orchestra { .. } | ConversionInfo::Boltz { .. })
    );
    let converted = matches!(row_info, Some(ConversionInfo::Amm { .. }))
        || amounts.is_some()
        || (payment.conversion_details.is_some() && !cross_chain);
    converted.then(|| PaymentConversionSummary {
        status: swap_status(payment),
        amounts,
    })
}

fn is_terminal(status: SwapStatus) -> bool {
    match status {
        SwapStatus::Completed | SwapStatus::Failed | SwapStatus::Refunded => true,
        // The SDK still has to refund it.
        SwapStatus::Pending | SwapStatus::RefundNeeded => false,
    }
}

/// This session's swaps that weren't finished when they were answered.
/// After a restart, Activity ([`conversion_summary`]) is how the gui learns
/// how they ended.
#[derive(Debug, Default)]
pub struct SwapTracker {
    unfinished: HashMap<String, SwapStatus>,
}

impl SwapTracker {
    /// Follow a swap answered with `status`. Finished ones aren't followed:
    /// the answer already said how they ended.
    pub fn track(&mut self, swap_id: &str, status: SwapStatus) {
        if !is_terminal(status) {
            self.unfinished.insert(swap_id.to_string(), status);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.unfinished.is_empty()
    }
}

/// Re-read every followed swap and emit [`ProtocolEvent::SwapStatusChanged`]
/// for each whose status changed. A finished one is no longer followed. A
/// swap that can't be read is kept for the next pass.
///
/// The SDK sends no event for a conversion's own legs (its
/// `TokenConversionMiddleware` at the pin), and a swap's row is one, so the
/// bridge reads them itself on every sync.
pub async fn refresh<B: SwapBackend>(
    tracker: &Mutex<SwapTracker>,
    backend: &B,
    emit: &(dyn Fn(ProtocolEvent) + Send + Sync),
) {
    let followed: Vec<(String, SwapStatus)> = tracker
        .lock()
        .await
        .unfinished
        .iter()
        .map(|(id, status)| (id.clone(), *status))
        .collect();
    for (swap_id, last) in followed {
        let status = match backend.payment(&swap_id).await {
            Ok(payment) => swap_status(&payment),
            Err(e) => {
                tracing::warn!("could not read swap {swap_id}: {e}");
                continue;
            }
        };
        let mut tracker = tracker.lock().await;
        if is_terminal(status) {
            tracker.unfinished.remove(&swap_id);
        } else {
            tracker.unfinished.insert(swap_id.clone(), status);
        }
        if status != last {
            emit(ProtocolEvent::SwapStatusChanged { swap_id, status });
        }
    }
}

/// The swap status worker: one [`refresh`] per wake-up (each SDK sync), so
/// passes never overlap. Wake-ups that queue during a pass collapse into
/// one more. `connected` is as for [`auto_convert::run_worker`]. Exits when
/// every wake sender is gone.
pub async fn run_status_worker<B, E, F, Fut>(
    mut wake: tokio::sync::mpsc::UnboundedReceiver<()>,
    tracker: Arc<Mutex<SwapTracker>>,
    mut connected: F,
) where
    B: SwapBackend,
    E: Fn(ProtocolEvent) + Send + Sync,
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<(B, E)>>,
{
    while wake.recv().await.is_some() {
        while wake.try_recv().is_ok() {}
        if tracker.lock().await.is_empty() {
            continue;
        }
        let Some((backend, emit)) = connected().await else {
            continue;
        };
        refresh(&tracker, &backend, &emit).await;
    }
}

#[async_trait]
impl SwapBackend for SdkConverter {
    async fn balances(&self) -> Result<Balances, String> {
        let info = self
            .sdk()
            .sdk
            .get_info(GetInfoRequest {
                ensure_synced: Some(true),
            })
            .await
            .map_err(|e| e.to_string())?;
        // Only the configured USDB token, under its own identifier, as the
        // balance the gui shows (`usdb_balance` in the server).
        let usdb_units = info
            .token_balances
            .get(USDB_MAINNET_TOKEN_IDENTIFIER)
            .filter(|tb| tb.token_metadata.identifier == USDB_MAINNET_TOKEN_IDENTIFIER)
            .map_or(0, |tb| tb.balance);
        Ok(Balances {
            sats: info.balance_sats,
            usdb_units,
        })
    }

    async fn usdb_limits(&self) -> Result<UsdbLimits, String> {
        let limits = self
            .sdk()
            .sdk
            .fetch_conversion_limits(FetchConversionLimitsRequest {
                conversion_type: ConversionType::ToBitcoin {
                    from_token_identifier: USDB_MAINNET_TOKEN_IDENTIFIER.to_string(),
                },
                // Named only for a conversion into a token.
                token_identifier: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(UsdbLimits {
            min_from_units: limits.min_from_amount,
            min_to_sats: limits.min_to_amount,
        })
    }

    async fn quote_usdb(&self, usdb_units: u128) -> Result<PrepareSendPaymentResponse, String> {
        let own_address = self.own_spark_address().await?;
        let prepared = self
            .sdk()
            .sdk
            .prepare_send_payment(usdb_request(
                own_address,
                usdb_units,
                self.max_slippage_bps(),
            ))
            .await
            .map_err(|e| e.to_string())?;
        checked_usdb_prepare(&prepared, own_address, usdb_units, self.max_slippage_bps())?;
        Ok(prepared)
    }

    async fn execute(&self, prepared: PrepareSendPaymentResponse) -> Result<Payment, String> {
        // No idempotency key: the SDK refuses one on a payment with a token
        // leg, which a conversion always has.
        self.sdk()
            .sdk
            .send_payment(SendPaymentRequest {
                prepare_response: prepared,
                options: None,
                idempotency_key: None,
            })
            .await
            .map(|response| response.payment)
            .map_err(|e| e.to_string())
    }

    async fn payment(&self, payment_id: &str) -> Result<Payment, String> {
        self.sdk()
            .sdk
            .get_payment(GetPaymentRequest {
                payment_id: payment_id.to_string(),
            })
            .await
            .map(|response| response.payment)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    use breez_sdk_spark::{
        Conversion, ConversionAsset, ConversionChain, FeePolicy, PaymentMethod, PaymentType,
        TokenMetadata, TokenTransactionType,
    };

    use crate::auto_convert::Receipt;

    const OWN: &str = "spark1ownaddressfortests";
    const CAP: u32 = CONVERSION_MAX_SLIPPAGE_BPS;
    /// 1 USDB (10^6 base units).
    const ONE_USDB: u128 = 1_000_000;

    /// Receives to count on an auto-convert state while a swap is sent.
    type Reports = (Arc<Mutex<AutoConvert>>, Vec<Receipt>);

    /// One recorded call to the fake.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Limits,
        UsdbLimits,
        Balances,
        Quote(u128),
        QuoteUsdb(u128),
        /// The sats or USDB the sent prepare converts.
        Execute(u128),
        Payment(String),
    }

    /// A fake SDK over one pool: `sats_per_usdb` sats buy 1 USDB, out of
    /// bitcoin padded by the cap as the SDK's `MinAmountOut` estimate is, and
    /// out of USDB less the cap as its `AmountIn` estimate is. Fees are 0.5%
    /// of the input.
    struct FakeSdk {
        balances: StdMutex<Balances>,
        btc_limits: Limits,
        usdb_limits: UsdbLimits,
        sats_per_usdb: StdMutex<u128>,
        /// Reported on every bitcoin → USDB estimate.
        adjustment: Option<AmountAdjustmentReason>,
        fail_quote: StdMutex<Option<String>>,
        execute_answer: StdMutex<Option<Result<Payment, String>>>,
        /// Receives the SDK reports to the listener while the swap is sent.
        reports_during_send: StdMutex<Option<Reports>>,
        payments: StdMutex<HashMap<String, Result<Payment, String>>>,
        calls: StdMutex<Vec<Call>>,
    }

    impl FakeSdk {
        fn new() -> Self {
            Self {
                balances: StdMutex::new(Balances {
                    sats: 200_000,
                    usdb_units: 50 * ONE_USDB,
                }),
                btc_limits: Limits {
                    min_from_sats: Some(1_000),
                    min_to_units: None,
                },
                usdb_limits: UsdbLimits {
                    min_from_units: Some(ONE_USDB),
                    min_to_sats: None,
                },
                sats_per_usdb: StdMutex::new(1_000),
                adjustment: None,
                fail_quote: StdMutex::new(None),
                execute_answer: StdMutex::new(None),
                reports_during_send: StdMutex::new(None),
                payments: StdMutex::new(HashMap::new()),
                calls: StdMutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        fn record(&self, call: Call) {
            self.calls.lock().unwrap().push(call);
        }

        fn executed(&self) -> bool {
            self.calls()
                .iter()
                .any(|call| matches!(call, Call::Execute(_)))
        }

        fn price(&self) -> u128 {
            *self.sats_per_usdb.lock().unwrap()
        }

        fn set_price(&self, sats_per_usdb: u128) {
            *self.sats_per_usdb.lock().unwrap() = sats_per_usdb;
        }

        fn quote_failure(&self) -> Result<(), String> {
            match self.fail_quote.lock().unwrap().take() {
                Some(reason) => Err(reason),
                None => Ok(()),
            }
        }
    }

    #[async_trait]
    impl Converter for FakeSdk {
        type Prepared = PrepareSendPaymentResponse;

        async fn limits(&self) -> Result<Limits, String> {
            self.record(Call::Limits);
            Ok(self.btc_limits)
        }

        async fn balance_sats(&self) -> Result<u64, String> {
            unreachable!("a swap reads both balances")
        }

        async fn quote(
            &self,
            usdb_target: u128,
        ) -> Result<auto_convert::Quote<Self::Prepared>, String> {
            self.record(Call::Quote(usdb_target));
            self.quote_failure()?;
            let base = (usdb_target * self.price()).div_ceil(ONE_USDB);
            let sats_in = base + (base * u128::from(CAP)).div_ceil(10_000);
            let mut prepared = PrepareSendPaymentResponse {
                payment_method: SendPaymentMethod::SparkAddress {
                    address: OWN.to_string(),
                    fee: 0,
                    token_identifier: Some(USDB_MAINNET_TOKEN_IDENTIFIER.to_string()),
                },
                amount: usdb_target,
                token_identifier: Some(USDB_MAINNET_TOKEN_IDENTIFIER.to_string()),
                conversion_estimate: Some(ConversionEstimate {
                    options: auto_convert::conversion_options_at(CAP),
                    amount_in: sats_in,
                    // As the SDK's `MinAmountOut` estimate: the simulated
                    // output of the padded input, above the target.
                    amount_out: sats_in * ONE_USDB / self.price(),
                    fee: sats_in / 200,
                    amount_adjustment: self.adjustment.clone(),
                }),
                fee_policy: FeePolicy::FeesExcluded,
            };
            // What `SdkConverter::quote` hands back: checked, exact input.
            let sats_in = auto_convert::checked_sats_in_at(&prepared, OWN, CAP)?;
            auto_convert::exact_input(&mut prepared);
            Ok(auto_convert::Quote { sats_in, prepared })
        }

        async fn send(&self, _prepared: Self::Prepared) -> Result<(), String> {
            unreachable!("a swap sends with `execute`")
        }
    }

    #[async_trait]
    impl SwapBackend for FakeSdk {
        async fn balances(&self) -> Result<Balances, String> {
            self.record(Call::Balances);
            Ok(*self.balances.lock().unwrap())
        }

        async fn usdb_limits(&self) -> Result<UsdbLimits, String> {
            self.record(Call::UsdbLimits);
            Ok(self.usdb_limits)
        }

        async fn quote_usdb(&self, usdb_units: u128) -> Result<PrepareSendPaymentResponse, String> {
            self.record(Call::QuoteUsdb(usdb_units));
            self.quote_failure()?;
            let sats = usdb_units * self.price() / ONE_USDB;
            let amount_out = sats * (10_000 - u128::from(CAP)) / 10_000;
            let prepared = PrepareSendPaymentResponse {
                payment_method: SendPaymentMethod::SparkAddress {
                    address: OWN.to_string(),
                    fee: 0,
                    token_identifier: None,
                },
                amount: amount_out,
                token_identifier: None,
                conversion_estimate: Some(ConversionEstimate {
                    options: usdb_request(OWN, usdb_units, CAP)
                        .conversion_options
                        .expect("a conversion"),
                    amount_in: usdb_units,
                    amount_out,
                    fee: usdb_units / 200,
                    amount_adjustment: None,
                }),
                fee_policy: FeePolicy::FeesExcluded,
            };
            checked_usdb_prepare(&prepared, OWN, usdb_units, CAP)?;
            Ok(prepared)
        }

        async fn execute(&self, prepared: PrepareSendPaymentResponse) -> Result<Payment, String> {
            let estimate = prepared.conversion_estimate.expect("a conversion");
            self.record(Call::Execute(estimate.amount_in));
            let reports = self.reports_during_send.lock().unwrap().take();
            if let Some((auto_convert, receipts)) = reports {
                for receipt in &receipts {
                    auto_convert.lock().await.note_receive(receipt);
                }
            }
            if let Some(answer) = self.execute_answer.lock().unwrap().take() {
                return answer;
            }
            Ok(match estimate.options.conversion_type {
                ConversionType::FromBitcoin => {
                    token_receive("recv-1", estimate.amount_out, ConversionStatus::Completed)
                }
                ConversionType::ToBitcoin { .. } => sats_receive(
                    "recv-1",
                    estimate.amount_out,
                    Some(ConversionStatus::Completed),
                ),
            })
        }

        async fn payment(&self, payment_id: &str) -> Result<Payment, String> {
            self.record(Call::Payment(payment_id.to_string()));
            self.payments
                .lock()
                .unwrap()
                .get(payment_id)
                .cloned()
                .unwrap_or_else(|| Err("no such payment".to_string()))
        }
    }

    fn amm(status: ConversionStatus) -> ConversionInfo {
        ConversionInfo::Amm {
            pool_id: "pool".to_string(),
            conversion_id: "conv".to_string(),
            status,
            fee: None,
            purpose: Some(breez_sdk_spark::ConversionPurpose::SelfTransfer),
            amount_adjustment: None,
            degradation: None,
        }
    }

    fn orchestra() -> ConversionInfo {
        serde_json::from_value(serde_json::json!({
            "type": "orchestra",
            "order_id": "order-1",
            "quote_id": "quote-1",
            "chain": "base",
            "recipient_address": "0xrecipient",
            "estimated_out": "1000000",
            "status": "Completed",
            "asset_decimals": 6,
        }))
        .expect("an orchestra conversion")
    }

    fn usdb_metadata() -> TokenMetadata {
        TokenMetadata {
            identifier: USDB_MAINNET_TOKEN_IDENTIFIER.to_string(),
            issuer_public_key: "02issuer".to_string(),
            name: "USDB".to_string(),
            ticker: "USDB".to_string(),
            decimals: 6,
            max_supply: u128::MAX,
            is_freezable: false,
        }
    }

    /// The USDB a swap out of bitcoin delivers, as the SDK answers it.
    fn token_receive(id: &str, units: u128, status: ConversionStatus) -> Payment {
        Payment {
            id: id.to_string(),
            payment_type: PaymentType::Receive,
            status: PaymentStatus::Completed,
            amount: units,
            fees: 0,
            timestamp: 1,
            method: PaymentMethod::Token,
            details: Some(PaymentDetails::Token {
                metadata: usdb_metadata(),
                tx_hash: "tx".to_string(),
                tx_type: TokenTransactionType::Transfer,
                invoice_details: None,
                conversion_info: Some(amm(status)),
            }),
            conversion_details: None,
        }
    }

    /// Sats received, by a swap out of USDB when `amm` is set.
    fn sats_receive(id: &str, sats: u128, amm_status: Option<ConversionStatus>) -> Payment {
        Payment {
            id: id.to_string(),
            payment_type: PaymentType::Receive,
            status: PaymentStatus::Completed,
            amount: sats,
            fees: 0,
            timestamp: 1,
            method: PaymentMethod::Spark,
            details: Some(PaymentDetails::Spark {
                invoice_details: None,
                htlc_details: None,
                conversion_info: amm_status.map(amm),
            }),
            conversion_details: None,
        }
    }

    fn details(status: ConversionStatus, conversions: Vec<Conversion>) -> ConversionDetails {
        ConversionDetails {
            status,
            conversions,
        }
    }

    fn side(identifier: Option<&str>, amount: u128) -> ConversionSide {
        ConversionSide {
            chain: ConversionChain::Spark,
            asset: ConversionAsset {
                ticker: if identifier.is_some() { "USDB" } else { "BTC" }.to_string(),
                identifier: identifier.map(str::to_string),
                decimals: if identifier.is_some() { 6 } else { 0 },
            },
            amount,
            fee: 0,
        }
    }

    fn step(provider: ConversionProvider, from: ConversionSide, to: ConversionSide) -> Conversion {
        Conversion {
            provider,
            status: ConversionStatus::Completed,
            from,
            to,
            amount_adjustment: None,
        }
    }

    fn exact(asset: WalletAsset, amount: u128) -> SwapAmount {
        SwapAmount::Exact {
            amount_in: AssetAmount { asset, amount },
        }
    }

    async fn prepared(fake: &FakeSdk, amount: SwapAmount) -> Prepared {
        match prepare(fake, amount, CAP).await {
            Ok(prepared) => prepared,
            Err(refusal) => panic!("refused: {refusal:?}"),
        }
    }

    fn refused_before_sending(refusal: Refusal) -> String {
        match refusal {
            Refusal::BadRequest(message) => message,
            other => panic!("expected a definite refusal, got {other:?}"),
        }
    }

    // ---- request building ----

    #[test]
    fn slippage_defaults_to_50_and_accepts_the_protocol_range() {
        assert_eq!(slippage_cap(None), Ok(50));
        assert_eq!(slippage_cap(None), Ok(CONVERSION_MAX_SLIPPAGE_BPS));
        for bps in [10, 50, 75, 500] {
            assert_eq!(slippage_cap(Some(bps)), Ok(bps));
        }
        for bps in [0, 9, 501, u32::MAX] {
            assert!(slippage_cap(Some(bps)).is_err(), "{bps}");
        }
    }

    /// Out of USDB: a self-transfer to the wallet's own Spark address naming
    /// the exact USDB amount, `ToBitcoin` out of USDB, at the cap.
    #[test]
    fn a_swap_out_of_usdb_is_an_exact_capped_self_transfer() {
        let request = usdb_request(OWN, 12 * ONE_USDB, 50);
        match &request.payment_request {
            PaymentRequest::Input { input } => assert_eq!(input, OWN),
            other => panic!("unexpected payment request {other:?}"),
        }
        assert_eq!(request.amount, Some(12 * ONE_USDB));
        assert_eq!(
            request.token_identifier.as_deref(),
            Some(USDB_MAINNET_TOKEN_IDENTIFIER)
        );
        let options = request.conversion_options.expect("a conversion");
        assert_eq!(
            options.conversion_type,
            ConversionType::ToBitcoin {
                from_token_identifier: USDB_MAINNET_TOKEN_IDENTIFIER.to_string()
            }
        );
        assert_eq!(options.max_slippage_bps, Some(50));
        assert_eq!(options.completion_timeout_secs, None);
        assert!(request.fee_policy.is_none());

        assert_eq!(
            usdb_request(OWN, ONE_USDB, 75)
                .conversion_options
                .and_then(|options| options.max_slippage_bps),
            Some(75)
        );
    }

    /// Out of bitcoin: auto-convert's request, at the swap's cap.
    #[test]
    fn a_swap_out_of_bitcoin_is_auto_converts_request_at_its_cap() {
        let request = auto_convert::prepare_request_at(OWN, 7 * ONE_USDB, 50);
        assert_eq!(request.amount, Some(7 * ONE_USDB));
        assert_eq!(
            request.token_identifier.as_deref(),
            Some(USDB_MAINNET_TOKEN_IDENTIFIER)
        );
        let options = request.conversion_options.expect("a conversion");
        assert_eq!(options.conversion_type, ConversionType::FromBitcoin);
        assert_eq!(options.max_slippage_bps, Some(50));
        assert_eq!(
            auto_convert::prepare_request_at(OWN, ONE_USDB, 120)
                .conversion_options
                .and_then(|options| options.max_slippage_bps),
            Some(120)
        );
    }

    fn usdb_prepare(
        address: &str,
        options: ConversionOptions,
        amount_in: u128,
    ) -> PrepareSendPaymentResponse {
        PrepareSendPaymentResponse {
            payment_method: SendPaymentMethod::SparkAddress {
                address: address.to_string(),
                fee: 0,
                token_identifier: None,
            },
            amount: 9_950,
            token_identifier: None,
            conversion_estimate: Some(ConversionEstimate {
                options,
                amount_in,
                amount_out: 9_950,
                fee: 50_000,
                amount_adjustment: None,
            }),
            fee_policy: FeePolicy::FeesExcluded,
        }
    }

    fn to_bitcoin(cap: u32) -> ConversionOptions {
        usdb_request(OWN, ONE_USDB, cap)
            .conversion_options
            .expect("a conversion")
    }

    #[test]
    fn only_the_usdb_prepare_that_was_asked_for_is_held() {
        let units = 10 * ONE_USDB;
        let good = usdb_prepare(OWN, to_bitcoin(50), units);
        assert_eq!(checked_usdb_prepare(&good, OWN, units, 50), Ok(()));

        // Another destination, cap, direction or token: refused.
        assert!(checked_usdb_prepare(
            &usdb_prepare("spark1other", to_bitcoin(50), units),
            OWN,
            units,
            50
        )
        .is_err());
        assert!(checked_usdb_prepare(&good, OWN, units, 75).is_err());
        let from_bitcoin = usdb_prepare(OWN, auto_convert::conversion_options_at(50), units);
        assert!(checked_usdb_prepare(&from_bitcoin, OWN, units, 50).is_err());
        let other_token = usdb_prepare(
            OWN,
            ConversionOptions {
                conversion_type: ConversionType::ToBitcoin {
                    from_token_identifier: "btkn1other".to_string(),
                },
                ..to_bitcoin(50)
            },
            units,
        );
        assert!(checked_usdb_prepare(&other_token, OWN, units, 50).is_err());
        let mut names_a_token = good.clone();
        names_a_token.token_identifier = Some(USDB_MAINNET_TOKEN_IDENTIFIER.to_string());
        assert!(checked_usdb_prepare(&names_a_token, OWN, units, 50).is_err());
        let mut none = good.clone();
        none.conversion_estimate = None;
        assert!(checked_usdb_prepare(&none, OWN, units, 50).is_err());

        // A different amount only with an adjustment the SDK reports, and
        // only upwards.
        assert!(checked_usdb_prepare(
            &usdb_prepare(OWN, to_bitcoin(50), units + 1),
            OWN,
            units,
            50
        )
        .is_err());
        let mut adjusted = usdb_prepare(OWN, to_bitcoin(50), units + 1);
        if let Some(estimate) = adjusted.conversion_estimate.as_mut() {
            estimate.amount_adjustment = Some(AmountAdjustmentReason::IncreasedToAvoidDust);
        }
        assert_eq!(checked_usdb_prepare(&adjusted, OWN, units, 50), Ok(()));
        assert!(checked_usdb_prepare(&adjusted, OWN, units + 2, 50).is_err());

        // Not an exact input: the SDK would convert `MinAmountOut` instead.
        let mut target = good.clone();
        target.amount = 9_949;
        assert!(checked_usdb_prepare(&target, OWN, units, 50).is_err());
    }

    // ---- prepare: out of USDB ----

    #[tokio::test]
    async fn an_exact_usdb_swap_converts_that_amount_and_states_every_denomination() {
        let fake = FakeSdk::new();
        let Prepared {
            swap,
            quote,
            limits,
        } = prepared(&fake, exact(WalletAsset::Usdb, 10 * ONE_USDB)).await;
        assert_eq!(
            fake.calls(),
            vec![
                Call::UsdbLimits,
                Call::Balances,
                Call::QuoteUsdb(10 * ONE_USDB)
            ]
        );
        // 10 USDB at 1,000 sats each, less the 0.5% cap.
        assert_eq!(
            quote,
            ConversionQuote {
                amount_in: usdb(10 * ONE_USDB),
                amount_out: bitcoin(9_950),
                fee: usdb(50_000),
                amount_adjustment: None,
                max_slippage_bps: 50,
            }
        );
        assert_eq!(
            limits,
            ConversionLimits {
                min_from: Some(usdb(ONE_USDB)),
                min_to: None,
            }
        );
        assert_eq!(
            (swap.from, swap.amount_in, swap.amount_out),
            (WalletAsset::Usdb, 10 * ONE_USDB, 9_950)
        );
    }

    #[tokio::test]
    async fn max_out_of_usdb_converts_the_whole_holding() {
        let fake = FakeSdk::new();
        let Prepared { quote, .. } = prepared(
            &fake,
            SwapAmount::Max {
                from: WalletAsset::Usdb,
            },
        )
        .await;
        assert!(fake.calls().contains(&Call::QuoteUsdb(50 * ONE_USDB)));
        assert_eq!(quote.amount_in, usdb(50 * ONE_USDB));
    }

    /// A remainder below the minimum stays USDB: no dust adjustment is made
    /// up, and the amount converted is the amount asked for.
    #[tokio::test]
    async fn no_dust_adjustment_is_made_up_out_of_usdb() {
        let fake = FakeSdk::new();
        let asked = 50 * ONE_USDB - (ONE_USDB - 1);
        let Prepared { quote, .. } = prepared(&fake, exact(WalletAsset::Usdb, asked)).await;
        assert_eq!(quote.amount_in, usdb(asked));
        assert_eq!(quote.amount_adjustment, None);
    }

    #[tokio::test]
    async fn usdb_below_the_minimum_or_above_the_holding_is_refused_before_any_prepare() {
        for amount in [0, ONE_USDB - 1, 50 * ONE_USDB + 1] {
            let fake = FakeSdk::new();
            let refusal = prepare(&fake, exact(WalletAsset::Usdb, amount), CAP)
                .await
                .err()
                .expect("refused");
            refused_before_sending(refusal);
            assert!(
                !fake
                    .calls()
                    .iter()
                    .any(|call| matches!(call, Call::QuoteUsdb(_))),
                "{amount}: {:?}",
                fake.calls()
            );
        }
        let fake = FakeSdk::new();
        fake.balances.lock().unwrap().usdb_units = 0;
        let refusal = prepare(
            &fake,
            SwapAmount::Max {
                from: WalletAsset::Usdb,
            },
            CAP,
        )
        .await
        .err()
        .expect("refused");
        assert!(refused_before_sending(refusal).contains("no USDB"));
    }

    /// Only the quote shows the bitcoin out: below the pool's minimum it is
    /// refused before a handle exists.
    #[tokio::test]
    async fn a_usdb_quote_below_the_minimum_output_is_refused() {
        let mut fake = FakeSdk::new();
        fake.usdb_limits.min_to_sats = Some(10_000);
        let refusal = prepare(&fake, exact(WalletAsset::Usdb, 10 * ONE_USDB), CAP)
            .await
            .err()
            .expect("refused");
        assert!(refused_before_sending(refusal).contains("minimum"));
        assert!(fake.calls().contains(&Call::QuoteUsdb(10 * ONE_USDB)));
    }

    // ---- prepare: out of bitcoin ----

    /// The sats are turned into a USDB target: a probe prices the pool, then
    /// the scaled target is re-prepared until it fits the sats. What is held
    /// spends exactly the quoted sats.
    #[tokio::test]
    async fn a_bitcoin_swap_derives_a_usdb_target_then_re_prepares_it() {
        let fake = FakeSdk::new();
        let typed = 100_000u128;
        let Prepared {
            swap,
            quote,
            limits,
        } = prepared(&fake, exact(WalletAsset::Bitcoin, typed)).await;
        let quotes: Vec<u128> = fake
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Quote(target) => Some(target),
                _ => None,
            })
            .collect();
        assert!(quotes.len() >= 2, "{quotes:?}");
        assert_eq!(quotes[0], auto_convert_probe());
        let target = *quotes.last().unwrap();

        assert!(quote.amount_in.amount <= typed, "{quote:?}");
        assert_eq!(quote.amount_in.asset, WalletAsset::Bitcoin);
        // The estimate's output, which is above the target it was fitted to.
        assert_eq!(quote.amount_out.asset, WalletAsset::Usdb);
        assert!(quote.amount_out.amount > target, "{quote:?}");
        assert_eq!(quote.fee.asset, WalletAsset::Bitcoin);
        assert_eq!(quote.max_slippage_bps, 50);
        assert_eq!(quote.amount_adjustment, None);
        assert_eq!(swap.amount_in, quote.amount_in.amount);
        // Exact input: the prepared amount is the estimate's output.
        assert_eq!(swap.prepared.amount, quote.amount_out.amount);
        assert_eq!(
            limits,
            ConversionLimits {
                min_from: Some(bitcoin(1_004)),
                min_to: None,
            }
        );
    }

    /// `PROBE_TARGET_UNITS`, as auto-convert's sizing probes first.
    fn auto_convert_probe() -> u128 {
        ONE_USDB
    }

    #[tokio::test]
    async fn max_out_of_bitcoin_sizes_to_the_whole_balance() {
        let fake = FakeSdk::new();
        let Prepared { quote, .. } = prepared(
            &fake,
            SwapAmount::Max {
                from: WalletAsset::Bitcoin,
            },
        )
        .await;
        assert!(quote.amount_in.amount <= 200_000);
        assert!(quote.amount_in.amount > 199_000, "{quote:?}");
    }

    #[test]
    fn the_least_bitcoin_input_is_the_first_budget_sizing_can_convert() {
        for min in [1, 999, 1_000, 1_001, 12_345, 1_000_000, u128::MAX / 2] {
            let least = least_bitcoin_input(min);
            assert!(auto_convert::clears_min_input(least, min), "{min}");
            assert!(!auto_convert::clears_min_input(least - 1, min), "{min}");
        }
        assert_eq!(least_bitcoin_input(1_000), 1_004);
        // A minimum too large to size refuses everything, without looping.
        assert_eq!(least_bitcoin_input(u128::MAX), u128::MAX);
    }

    /// Below the minimum the limits state, or above the balance: refused
    /// before any prepare. The minimum itself is accepted.
    #[tokio::test]
    async fn bitcoin_below_the_minimum_or_above_the_balance_is_refused_before_any_prepare() {
        for sats in [0, 1_000, 1_003, 200_001, u128::from(u64::MAX) + 1] {
            let fake = FakeSdk::new();
            let refusal = prepare(&fake, exact(WalletAsset::Bitcoin, sats), CAP)
                .await
                .err()
                .expect("refused");
            refused_before_sending(refusal);
            assert!(
                !fake
                    .calls()
                    .iter()
                    .any(|call| matches!(call, Call::Quote(_))),
                "{sats}: {:?}",
                fake.calls()
            );
        }
        let fake = FakeSdk::new();
        let Prepared { quote, .. } = prepared(&fake, exact(WalletAsset::Bitcoin, 1_004)).await;
        assert!(quote.amount_in.amount <= 1_004);
    }

    /// A minimum on the USDB side is known only from a price: the sats are
    /// refused once sizing finds they buy less, before anything is held.
    #[tokio::test]
    async fn bitcoin_that_buys_less_than_the_minimum_output_is_refused() {
        let mut fake = FakeSdk::new();
        fake.btc_limits = Limits {
            min_from_sats: None,
            min_to_units: Some(5 * ONE_USDB),
        };
        let refusal = prepare(&fake, exact(WalletAsset::Bitcoin, 4_000), CAP)
            .await
            .err()
            .expect("refused");
        assert!(refused_before_sending(refusal).contains("smallest swap"));
    }

    /// An adjustment the SDK reports out of bitcoin reaches the quote. The
    /// SDK at the pin reports none on this path; this pins the pass-through.
    #[tokio::test]
    async fn an_sdk_adjustment_out_of_bitcoin_is_reported() {
        let mut fake = FakeSdk::new();
        fake.adjustment = Some(AmountAdjustmentReason::FlooredToMinLimit);
        let Prepared { quote, .. } = prepared(&fake, exact(WalletAsset::Bitcoin, 50_000)).await;
        assert_eq!(
            quote.amount_adjustment,
            Some(AmountAdjustment::FlooredToMinLimit)
        );
    }

    #[tokio::test]
    async fn a_failed_prepare_is_an_sdk_error() {
        let fake = FakeSdk::new();
        *fake.fail_quote.lock().unwrap() = Some("pool unavailable".to_string());
        assert_eq!(
            prepare(&fake, exact(WalletAsset::Usdb, 10 * ONE_USDB), CAP)
                .await
                .err(),
            Some(Refusal::Sdk("pool unavailable".to_string()))
        );
    }

    // ---- execute ----

    #[tokio::test]
    async fn a_swap_is_re_priced_and_balance_checked_then_sent_as_reviewed() {
        let fake = FakeSdk::new();
        let Prepared { swap, .. } = prepared(&fake, exact(WalletAsset::Usdb, 10 * ONE_USDB)).await;
        fake.calls.lock().unwrap().clear();
        assert_eq!(
            execute(&fake, swap, None).await,
            Ok(("recv-1".to_string(), SwapStatus::Completed))
        );
        assert_eq!(
            fake.calls(),
            vec![
                Call::QuoteUsdb(10 * ONE_USDB),
                Call::Balances,
                Call::Execute(10 * ONE_USDB)
            ]
        );
    }

    /// Out of bitcoin the reviewed prepare is what is sent, so exactly the
    /// reviewed sats are spent even if the re-price quotes others.
    #[tokio::test]
    async fn a_bitcoin_swap_spends_exactly_the_reviewed_sats() {
        let fake = FakeSdk::new();
        let Prepared { swap, quote, .. } =
            prepared(&fake, exact(WalletAsset::Bitcoin, 100_000)).await;
        let target = swap.amount_out;
        // Cheaper bitcoin by 0.2%: within the cap.
        fake.set_price(1_002);
        fake.calls.lock().unwrap().clear();
        assert_eq!(
            execute(&fake, swap, None).await,
            Ok(("recv-1".to_string(), SwapStatus::Completed))
        );
        assert_eq!(
            fake.calls(),
            vec![
                Call::Quote(target),
                Call::Balances,
                Call::Execute(quote.amount_in.amount)
            ]
        );
    }

    /// A price that moved against the user by more than the cap since the
    /// estimate is refused, definitely, with nothing sent; a smaller move is
    /// not.
    #[tokio::test]
    async fn a_price_move_beyond_the_cap_is_refused_before_sending() {
        for (from, amount, price, refused) in [
            (WalletAsset::Usdb, 10 * ONE_USDB, 994, true),
            (WalletAsset::Usdb, 10 * ONE_USDB, 996, false),
            (WalletAsset::Usdb, 10 * ONE_USDB, 1_100, false),
            (WalletAsset::Bitcoin, 100_000, 1_006, true),
            (WalletAsset::Bitcoin, 100_000, 1_004, false),
            (WalletAsset::Bitcoin, 100_000, 900, false),
        ] {
            let fake = FakeSdk::new();
            let Prepared { swap, .. } = prepared(&fake, exact(from, amount)).await;
            fake.set_price(price);
            let outcome = execute(&fake, swap, None).await;
            if refused {
                let message = refused_before_sending(outcome.expect_err("refused"));
                assert!(message.contains("moved by more than 0.5%"), "{message}");
                assert!(message.contains("Nothing was sent"), "{message}");
                assert!(!fake.executed(), "{from:?} at {price}");
            } else {
                assert!(outcome.is_ok(), "{from:?} at {price}: {outcome:?}");
            }
        }
    }

    #[tokio::test]
    async fn a_balance_that_dropped_or_a_failed_re_price_is_refused_before_sending() {
        let fake = FakeSdk::new();
        let Prepared { swap, .. } = prepared(&fake, exact(WalletAsset::Bitcoin, 100_000)).await;
        fake.balances.lock().unwrap().sats = 50_000;
        let message =
            refused_before_sending(execute(&fake, swap, None).await.expect_err("refused"));
        assert!(
            message.contains("no longer holds enough bitcoin"),
            "{message}"
        );
        assert!(!fake.executed());

        let fake = FakeSdk::new();
        let Prepared { swap, .. } = prepared(&fake, exact(WalletAsset::Usdb, 10 * ONE_USDB)).await;
        fake.balances.lock().unwrap().usdb_units = ONE_USDB;
        refused_before_sending(execute(&fake, swap, None).await.expect_err("refused"));
        assert!(!fake.executed());

        let fake = FakeSdk::new();
        let Prepared { swap, .. } = prepared(&fake, exact(WalletAsset::Usdb, 10 * ONE_USDB)).await;
        *fake.fail_quote.lock().unwrap() = Some("pool unavailable".to_string());
        let message =
            refused_before_sending(execute(&fake, swap, None).await.expect_err("refused"));
        assert!(message.contains("pool unavailable"), "{message}");
        assert!(!fake.executed());
    }

    /// Once the SDK is asked to send, an error is an unknown outcome.
    #[tokio::test]
    async fn an_error_from_the_send_is_an_sdk_error() {
        let fake = FakeSdk::new();
        let Prepared { swap, .. } = prepared(&fake, exact(WalletAsset::Usdb, 10 * ONE_USDB)).await;
        *fake.execute_answer.lock().unwrap() =
            Some(Err("Timeout waiting for conversion to complete".to_string()));
        assert_eq!(
            execute(&fake, swap, None).await,
            Err(Refusal::Sdk(
                "Timeout waiting for conversion to complete".to_string()
            ))
        );
        assert!(fake.executed());
    }

    #[tokio::test]
    async fn a_swap_answered_before_its_payment_completes_is_pending() {
        let fake = FakeSdk::new();
        let Prepared { swap, .. } = prepared(&fake, exact(WalletAsset::Usdb, 10 * ONE_USDB)).await;
        let mut receive = sats_receive("recv-9", 9_950, Some(ConversionStatus::Completed));
        receive.status = PaymentStatus::Pending;
        *fake.execute_answer.lock().unwrap() = Some(Ok(receive));
        assert_eq!(
            execute(&fake, swap, None).await,
            Ok(("recv-9".to_string(), SwapStatus::Pending))
        );
    }

    /// A fresh Cube state directory, removed on drop.
    struct StateDir(std::path::PathBuf);

    impl StateDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("cc-swap-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }
    }

    impl Drop for StateDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn auto_convert_on(dir: &StateDir) -> Arc<Mutex<AutoConvert>> {
        let mut auto_convert = AutoConvert::load(&dir.0);
        assert!(auto_convert.set_enabled(true).expect("saved"));
        Arc::new(Mutex::new(auto_convert))
    }

    fn receipt(id: &str, sats: u64) -> Receipt {
        Receipt {
            payment_id: id.to_string(),
            sats,
        }
    }

    /// The SDK can report the bitcoin a USDB swap delivers as a receive
    /// before it marks it a conversion's. It is taken back out of the pending
    /// sats, and can't count later; a real receive in the same window stays.
    #[tokio::test]
    async fn a_usdb_swaps_own_bitcoin_is_not_auto_converted() {
        let dir = StateDir::new();
        let auto_convert = auto_convert_on(&dir);
        let fake = FakeSdk::new();
        let Prepared { swap, quote, .. } =
            prepared(&fake, exact(WalletAsset::Usdb, 10 * ONE_USDB)).await;
        let delivered = u64::try_from(quote.amount_out.amount).unwrap();
        *fake.reports_during_send.lock().unwrap() = Some((
            Arc::clone(&auto_convert),
            vec![receipt("recv-1", delivered), receipt("gift", 5_000)],
        ));
        assert_eq!(
            execute(&fake, swap, Some(&auto_convert)).await,
            Ok(("recv-1".to_string(), SwapStatus::Completed))
        );
        let mut auto_convert = auto_convert.lock().await;
        assert_eq!(auto_convert.pending_sats(), 5_000);
        assert!(!auto_convert.note_receive(&receipt("recv-1", delivered)));
        assert_eq!(auto_convert.pending_sats(), 5_000);
    }

    /// The SDK can answer an error after the pool ran a USDB swap
    /// (`flashnet.rs` at the pin), with the swap's bitcoin reported as an
    /// ordinary receive either while it was sent or after the error. Neither
    /// is auto-converted.
    #[tokio::test]
    async fn a_usdb_swap_that_errors_after_running_leaves_its_bitcoin_alone() {
        for counted_before_the_error in [true, false] {
            let dir = StateDir::new();
            let auto_convert = auto_convert_on(&dir);
            let fake = FakeSdk::new();
            let Prepared { swap, quote, .. } =
                prepared(&fake, exact(WalletAsset::Usdb, 10 * ONE_USDB)).await;
            let delivered = u64::try_from(quote.amount_out.amount).unwrap();
            let error = "Convert token failed after the swap ran, delivering 9950 via t1: timeout";
            *fake.execute_answer.lock().unwrap() = Some(Err(error.to_string()));
            if counted_before_the_error {
                *fake.reports_during_send.lock().unwrap() = Some((
                    Arc::clone(&auto_convert),
                    vec![receipt("recv-1", delivered)],
                ));
            }
            assert_eq!(
                execute(&fake, swap, Some(&auto_convert)).await,
                Err(Refusal::Sdk(error.to_string()))
            );
            let mut auto_convert = auto_convert.lock().await;
            assert_eq!(auto_convert.pending_sats(), 0);
            assert!(!auto_convert.note_receive(&receipt("recv-1", delivered)));
            assert_eq!(auto_convert.pending_sats(), 0, "{counted_before_the_error}");
        }
    }

    /// A swap out of bitcoin delivers USDB, which never counts, so it
    /// doesn't touch auto-convert.
    #[tokio::test]
    async fn a_bitcoin_swap_leaves_auto_convert_alone() {
        let dir = StateDir::new();
        let auto_convert = auto_convert_on(&dir);
        let fake = FakeSdk::new();
        let Prepared { swap, .. } = prepared(&fake, exact(WalletAsset::Bitcoin, 100_000)).await;
        *fake.reports_during_send.lock().unwrap() =
            Some((Arc::clone(&auto_convert), vec![receipt("gift", 5_000)]));
        assert!(execute(&fake, swap, Some(&auto_convert)).await.is_ok());
        let mut auto_convert = auto_convert.lock().await;
        assert_eq!(auto_convert.pending_sats(), 5_000);
        // Not marked seen: a receive with the swap's id would still count.
        assert!(auto_convert.note_receive(&receipt("recv-1", 1)));
    }

    #[test]
    fn the_cap_check_compares_output_per_input() {
        // Same amounts: the boundary is the cap itself.
        assert_eq!(
            within_cap((10_000, 10_000), (10_000, 9_950), 50),
            Some(true)
        );
        assert_eq!(
            within_cap((10_000, 10_000), (10_000, 9_949), 50),
            Some(false)
        );
        // Different amounts, same price.
        assert_eq!(
            within_cap((10_000, 5_000), (20_000, 10_000), 50),
            Some(true)
        );
        // A better price always passes.
        assert_eq!(within_cap((10_000, 5_000), (10_000, 6_000), 50), Some(true));
        assert_eq!(within_cap((u128::MAX, 1), (1, 1), 50), None);
        // A quote of nothing prices nothing.
        assert_eq!(within_cap((10_000, 5_000), (0, 0), 50), None);
        assert_eq!(within_cap((10_000, 5_000), (10_000, 0), 50), None);
        assert_eq!(within_cap((10_000, 5_000), (0, 5_000), 50), None);
    }

    #[test]
    fn caps_read_as_percentages() {
        assert_eq!(percent(50), "0.5%");
        assert_eq!(percent(25), "0.25%");
        assert_eq!(percent(5), "0.05%");
        assert_eq!(percent(100), "1%");
        assert_eq!(percent(150), "1.5%");
    }

    // ---- status ----

    #[test]
    fn every_sdk_status_maps_to_its_swap_status() {
        for (sdk, ours) in [
            (ConversionStatus::Pending, SwapStatus::Pending),
            (ConversionStatus::Completed, SwapStatus::Completed),
            (ConversionStatus::Failed, SwapStatus::Failed),
            (ConversionStatus::RefundNeeded, SwapStatus::RefundNeeded),
            (ConversionStatus::Refunded, SwapStatus::Refunded),
        ] {
            assert_eq!(protocol_status(&sdk), ours);
            assert_eq!(
                swap_status(&sats_receive("r", 1, Some(sdk))),
                ours,
                "the row's own AMM status"
            );
        }
    }

    /// `conversion_details` first, then the row's own AMM status, then
    /// `Pending`.
    #[test]
    fn a_status_comes_from_details_then_the_rows_amm_info() {
        let mut payment = sats_receive("r", 1, Some(ConversionStatus::Completed));
        payment.conversion_details = Some(details(ConversionStatus::Refunded, vec![]));
        assert_eq!(swap_status(&payment), SwapStatus::Refunded);

        payment.conversion_details = None;
        assert_eq!(swap_status(&payment), SwapStatus::Completed);

        assert_eq!(
            swap_status(&sats_receive("r", 1, None)),
            SwapStatus::Pending
        );
    }

    /// A cross-chain provider's status isn't a conversion status here.
    #[test]
    fn a_cross_chain_status_is_not_a_swap_status() {
        let mut payment = sats_receive("r", 1, None);
        payment.details = Some(PaymentDetails::Spark {
            invoice_details: None,
            htlc_details: None,
            conversion_info: Some(orchestra()),
        });
        assert_eq!(swap_status(&payment), SwapStatus::Pending);
    }

    /// Never reported done before the payment itself is. A completed
    /// conversion whose receive failed delivered nothing: `Failed`, which is
    /// terminal, so it isn't followed for the rest of the session.
    #[test]
    fn completed_needs_a_completed_payment() {
        for (payment_status, expected) in [
            (PaymentStatus::Pending, SwapStatus::Pending),
            (PaymentStatus::Failed, SwapStatus::Failed),
        ] {
            let mut payment = token_receive("r", 1, ConversionStatus::Completed);
            payment.status = payment_status;
            assert_eq!(swap_status(&payment), expected);

            payment.details = None;
            payment.conversion_details = Some(details(ConversionStatus::Completed, vec![]));
            assert_eq!(swap_status(&payment), expected);

            // A failure is reported as it is.
            payment.conversion_details = Some(details(ConversionStatus::RefundNeeded, vec![]));
            assert_eq!(swap_status(&payment), SwapStatus::RefundNeeded);
        }
        let mut no_info = sats_receive("r", 1, None);
        no_info.status = PaymentStatus::Failed;
        assert_eq!(swap_status(&no_info), SwapStatus::Pending);
    }

    // ---- following unfinished swaps ----

    #[test]
    fn only_unfinished_swaps_are_followed() {
        let mut tracker = SwapTracker::default();
        for status in [
            SwapStatus::Completed,
            SwapStatus::Failed,
            SwapStatus::Refunded,
        ] {
            tracker.track("done", status);
        }
        assert!(tracker.is_empty());
        tracker.track("pending", SwapStatus::Pending);
        tracker.track("refund-needed", SwapStatus::RefundNeeded);
        assert_eq!(tracker.unfinished.len(), 2);
    }

    /// One event per actual change; a finished swap stops being followed,
    /// and one that can't be read is kept.
    #[tokio::test]
    async fn a_refresh_emits_only_status_changes() {
        let fake = FakeSdk::new();
        let tracker = Mutex::new(SwapTracker::default());
        {
            let mut tracker = tracker.lock().await;
            tracker.track("same", SwapStatus::Pending);
            tracker.track("refunding", SwapStatus::RefundNeeded);
            tracker.track("done", SwapStatus::Pending);
            tracker.track("unreadable", SwapStatus::Pending);
        }
        let mut pending = sats_receive("same", 1, Some(ConversionStatus::Completed));
        pending.status = PaymentStatus::Pending;
        {
            let mut payments = fake.payments.lock().unwrap();
            payments.insert("same".into(), Ok(pending));
            payments.insert(
                "refunding".into(),
                Ok(sats_receive(
                    "refunding",
                    1,
                    Some(ConversionStatus::Refunded),
                )),
            );
            payments.insert(
                "done".into(),
                Ok(token_receive("done", 1, ConversionStatus::Completed)),
            );
            payments.insert("unreadable".into(), Err("storage busy".into()));
        }
        let events = StdMutex::new(Vec::new());
        let emit = |event| events.lock().unwrap().push(event);
        refresh(&tracker, &fake, &emit).await;

        let mut events = events.into_inner().unwrap();
        events.sort_by_key(|event| format!("{event:?}"));
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events.iter().any(|event| matches!(
            event,
            ProtocolEvent::SwapStatusChanged { swap_id, status: SwapStatus::Completed } if swap_id == "done"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ProtocolEvent::SwapStatusChanged { swap_id, status: SwapStatus::Refunded } if swap_id == "refunding"
        )));
        let tracker = tracker.lock().await;
        let mut followed: Vec<&String> = tracker.unfinished.keys().collect();
        followed.sort();
        assert_eq!(followed, vec!["same", "unreadable"]);
    }

    /// With nothing to follow, a sync reads nothing.
    #[tokio::test]
    async fn the_worker_reads_nothing_while_nothing_is_followed() {
        let (wake_tx, wake_rx) = tokio::sync::mpsc::unbounded_channel();
        let tracker = Arc::new(Mutex::new(SwapTracker::default()));
        let connects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&connects);
        let worker = tokio::spawn(run_status_worker(
            wake_rx,
            Arc::clone(&tracker),
            move || {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { None::<(FakeSdk, fn(ProtocolEvent))> }
            },
        ));
        wake_tx.send(()).unwrap();
        wake_tx.send(()).unwrap();
        drop(wake_tx);
        worker.await.unwrap();
        assert_eq!(connects.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    // ---- Activity ----

    #[test]
    fn a_swap_row_reports_its_status_and_no_made_up_amounts() {
        assert_eq!(
            conversion_summary(&token_receive(
                "r",
                9 * ONE_USDB,
                ConversionStatus::Completed
            )),
            Some(PaymentConversionSummary {
                status: SwapStatus::Completed,
                amounts: None,
            })
        );
        // A refund is its own row.
        assert_eq!(
            conversion_summary(&sats_receive(
                "refund",
                9_950,
                Some(ConversionStatus::Refunded)
            )),
            Some(PaymentConversionSummary {
                status: SwapStatus::Refunded,
                amounts: None,
            })
        );
    }

    #[test]
    fn a_converted_send_reports_its_btc_usdb_step() {
        let mut send = sats_receive("send", 9_950, None);
        send.payment_type = PaymentType::Send;
        send.details = None;
        send.conversion_details = Some(details(
            ConversionStatus::Completed,
            vec![step(
                ConversionProvider::Amm,
                side(Some(USDB_MAINNET_TOKEN_IDENTIFIER), 10 * ONE_USDB),
                side(None, 9_950),
            )],
        ));
        assert_eq!(
            conversion_summary(&send),
            Some(PaymentConversionSummary {
                status: SwapStatus::Completed,
                amounts: Some(ConversionAmounts {
                    from: usdb(10 * ONE_USDB),
                    to: bitcoin(9_950),
                }),
            })
        );

        // Status from metadata before the steps are built: still a
        // conversion, with no amounts.
        send.conversion_details = Some(details(ConversionStatus::Pending, vec![]));
        assert_eq!(
            conversion_summary(&send),
            Some(PaymentConversionSummary {
                status: SwapStatus::Pending,
                amounts: None,
            })
        );

        // A step into another token is not BTC ⇄ USDB.
        send.conversion_details = Some(details(
            ConversionStatus::Completed,
            vec![step(
                ConversionProvider::Amm,
                side(Some("btkn1other"), 10),
                side(None, 9_950),
            )],
        ));
        assert_eq!(conversion_summary(&send).and_then(|s| s.amounts), None);
    }

    #[test]
    fn cross_chain_and_plain_rows_report_no_conversion() {
        assert_eq!(conversion_summary(&sats_receive("plain", 1, None)), None);

        let mut cross_chain = sats_receive("cc", 1, None);
        cross_chain.payment_type = PaymentType::Send;
        cross_chain.details = Some(PaymentDetails::Spark {
            invoice_details: None,
            htlc_details: None,
            conversion_info: Some(orchestra()),
        });
        cross_chain.conversion_details = Some(details(
            ConversionStatus::Completed,
            vec![step(
                ConversionProvider::Orchestra,
                side(None, 1),
                side(Some("0xusdc"), 1),
            )],
        ));
        assert_eq!(conversion_summary(&cross_chain), None);
    }
}
