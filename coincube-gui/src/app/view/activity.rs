//! Compact, cross-wallet Activity presentation (`P-Activity`). Amounts are
//! shown in their source units; a current spot quote is never presented as
//! historical fiat.

use chrono::{DateTime, Local, NaiveDate, Utc};
use coincube_spark_protocol::PaymentSummary;
use coincube_ui::{
    component::tenshu::{self, FieldSize, PageWidth},
    font, theme,
    widget::{action::Action, Column, Element, Row},
};
#[allow(deprecated)]
use iced::{
    advanced::widget::Id,
    widget::{column, component, container, row, rule, scrollable, text, Component, Space},
    Alignment, Length,
};

use crate::app::{
    breez_liquid::assets::{format_usdt_display, USDT_ASSET_ID_MAINNET},
    breez_spark::assets::format_token_display,
    cache::Cache,
    state::activity::{Activity, ActivityTarget},
    wallets::{
        DomainPayment, DomainPaymentDetails, DomainPaymentDirection, DomainPaymentStatus,
        DomainRefundableSwap,
    },
};
use crate::daemon::model::{HistoryTransaction, LabelItem, SpendTx};

use super::{shared::glyph, Message};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WalletFilter {
    #[default]
    All,
    Vault,
    Spark,
    Liquid,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DirectionFilter {
    #[default]
    All,
    Received,
    Sent,
    Transfers,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StatusFilter {
    #[default]
    All,
    Pending,
    Confirmed,
    Failed,
}

impl std::fmt::Display for WalletFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::All => "All wallets",
                Self::Vault => "Vault",
                Self::Spark => "Spark",
                Self::Liquid => "Liquid",
            }
        )
    }
}
impl std::fmt::Display for DirectionFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::All => "All",
                Self::Received => "Received",
                Self::Sent => "Sent",
                Self::Transfers => "Transfers",
            }
        )
    }
}
impl std::fmt::Display for StatusFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::All => "All",
                Self::Pending => "Pending",
                Self::Confirmed => "Confirmed",
                Self::Failed => "Failed",
            }
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub wallet: WalletFilter,
    pub direction: DirectionFilter,
    pub status: StatusFilter,
    pub date_range: String,
    pub search: String,
}

impl Filters {
    fn count(&self) -> usize {
        usize::from(self.wallet != WalletFilter::All)
            + usize::from(self.direction != DirectionFilter::All)
            + usize::from(self.status != StatusFilter::All)
            + usize::from(!self.date_range.is_empty())
            + usize::from(!self.search.is_empty())
    }

    fn dates(&self) -> Result<(Option<NaiveDate>, Option<NaiveDate>), ()> {
        fn parse(value: &str) -> Result<Option<NaiveDate>, ()> {
            if value.is_empty() {
                Ok(None)
            } else {
                NaiveDate::parse_from_str(value, "%Y-%m-%d")
                    .map(Some)
                    .map_err(|_| ())
            }
        }
        if self.date_range.trim().is_empty() {
            return Ok((None, None));
        }
        let (start, end) = self.date_range.split_once(" to ").ok_or(())?;
        let start = parse(start)?;
        let end = parse(end)?;
        if start.zip(end).is_some_and(|(start, end)| start > end) {
            return Err(());
        }
        Ok((start, end))
    }

    fn includes(&self, entry: &Entry) -> bool {
        if self.wallet != WalletFilter::All && self.wallet != entry.wallet {
            return false;
        }
        if self.direction != DirectionFilter::All && self.direction != entry.direction {
            return false;
        }
        if self.status != StatusFilter::All && self.status != entry.status {
            return false;
        }
        if !self.search.trim().is_empty() {
            // "Label, txid, hash or swap ID": the title carries the label.
            let needle = self.search.trim().to_lowercase();
            let found = entry.title.to_lowercase().contains(&needle)
                || entry.id.to_lowercase().contains(&needle)
                || entry
                    .identifiers
                    .iter()
                    .any(|(_, value)| value.to_lowercase().contains(&needle));
            if !found {
                return false;
            }
        }
        let Ok((start, end)) = self.dates() else {
            return false;
        };
        let date = (entry.timestamp != 0)
            .then(|| DateTime::<Utc>::from_timestamp(entry.timestamp as i64, 0))
            .flatten()
            .map(|time| time.with_timezone(&Local).date_naive());
        if start.is_some_and(|start| date.is_none_or(|date| date < start)) {
            return false;
        }
        if end.is_some_and(|end| date.is_none_or(|date| date > end)) {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone)]
pub enum ActivityMessage {
    Wallet(WalletFilter),
    Direction(DirectionFilter),
    Status(StatusFilter),
    DateRange(String),
    Search(String),
    Clear,
    Select(String),
    CloseDetail,
    Retry,
    LoadMore,
    OpenDetail(ActivityTarget),
    /// Open or close the selected row's rail details (`.disc`).
    ToggleDetails(bool),
    /// The selected row's label draft.
    LabelEdited(String),
    /// Save the selected row's label draft.
    SaveLabel,
    /// A wallet page's "View all transactions": Activity, filtered to it.
    ViewAll(WalletFilter),
    /// A wallet page's Retry: that wallet's rows only, kept on screen.
    RetryWallet(WalletFilter),
}

pub fn row_focus_id(entry_id: &str) -> Id {
    Id::from(format!("tenshu.activity.row:{}:{entry_id}", entry_id.len()))
}

pub const DETAIL_CLOSE_ID: &str = "tenshu.activity.detail.close";
pub const DETAIL_LABEL_ID: &str = "tenshu.activity.detail.label";
const DETAIL_RAIL_ID: &str = "tenshu.activity.detail.rail";

/// `.dl` key and value: the label names what the value is.
pub type Fact = (&'static str, String);

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub wallet: WalletFilter,
    /// The rail after the wallet name (`.dw`), e.g. "Spark → Lightning".
    pub rail: &'static str,
    pub timestamp: u64,
    pub direction: DirectionFilter,
    pub status: StatusFilter,
    pub status_label: String,
    /// What a settled status adds in the detail pane (`.dst`), e.g.
    /// "payment settled".
    pub status_note: Option<&'static str>,
    pub title: String,
    /// The amount's figure, grouped by thousands (`4,200`), unsigned.
    pub amount: String,
    /// The amount's unit (`.u`): "SATS", "USDt", a token ticker.
    pub unit: String,
    pub fee: Option<String>,
    /// "Destination" or "Counterparty", and its value.
    pub peer: Option<Fact>,
    pub target: ActivityTarget,
    /// Copyable identifiers (`.ids`), most specific first.
    pub identifiers: Vec<Fact>,
    /// The rail disclosure (`.disc`): its title and its facts.
    pub rail_details: Option<(&'static str, Vec<Fact>)>,
    /// Where this row's label is stored, when the wallet stores labels.
    pub label_key: Option<LabelItem>,
    pub signer_status: Option<String>,
}

/// `4200` → `4,200`.
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn sats(value: u64) -> String {
    format!("{} SATS", grouped(value))
}

/// A long invoice or identifier, cut in the middle for one line.
fn short_identifier(identifier: &str) -> String {
    let count = identifier.chars().count();
    if count <= 28 {
        return identifier.to_owned();
    }
    let start: String = identifier.chars().take(16).collect();
    let end: String = identifier.chars().skip(count - 8).collect();
    format!("{start}…{end}")
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

impl Entry {
    pub fn vault(tx: HistoryTransaction) -> Self {
        let id = format!("vault:onchain:{}", tx.txid);
        let direction = if tx.is_send_to_self() {
            DirectionFilter::Transfers
        } else if tx.is_external() {
            DirectionFilter::Received
        } else {
            DirectionFilter::Sent
        };
        let amount = if tx.is_external() || tx.is_send_to_self() {
            tx.incoming_amount
        } else {
            tx.outgoing_amount
        };
        // The title reads the single payment's label before the txid's, so
        // an edit is stored where the title will read it back.
        // Edit the key the title reads: the payment's own label, else the
        // transaction's, else (nothing stored yet) the payment's.
        let has = |key: &str| tx.labels.get(key).is_some_and(|label| !label.is_empty());
        let label_key = match tx.is_single_payment() {
            Some(outpoint) if has(&outpoint.to_string()) || !has(&tx.txid.to_string()) => {
                LabelItem::OutPoint(outpoint)
            }
            _ => LabelItem::Txid(tx.txid),
        };
        let label = tx
            .is_single_payment()
            .and_then(|outpoint| tx.labels.get(&outpoint.to_string()))
            .or_else(|| tx.labels.get(&tx.txid.to_string()))
            .filter(|label| !label.is_empty())
            .cloned();
        let title = label.unwrap_or_else(|| {
            match direction {
                DirectionFilter::Received => "Received Bitcoin",
                DirectionFilter::Sent => "Sent Bitcoin",
                _ => "Self-transfer",
            }
            .into()
        });
        let status = if tx.time.is_some() {
            StatusFilter::Confirmed
        } else {
            StatusFilter::Pending
        };
        // A single outgoing payment names its one recipient.
        let peer = (direction == DirectionFilter::Sent)
            .then(|| tx.is_single_payment())
            .flatten()
            .and_then(|outpoint| tx.tx.output.get(outpoint.vout as usize))
            .and_then(|output| {
                coincube_core::miniscript::bitcoin::Address::from_script(
                    &output.script_pubkey,
                    tx.network,
                )
                .ok()
            })
            .map(|address| ("Destination", address.to_string()));
        let fee = match (tx.fee_amount, direction) {
            (Some(fee), _) => Some(sats(fee.to_sat())),
            (None, DirectionFilter::Received) => Some("Paid by sender".to_owned()),
            (None, _) => None,
        };
        let mut facts = Vec::new();
        if let Some(height) = tx.height {
            facts.push(("Block", grouped(height.max(0) as u64)));
        }
        facts.push((
            "Inputs",
            match tx.tx.input.len() {
                1 => "1 coin".to_owned(),
                count => format!("{count} coins"),
            },
        ));
        facts.push(("Outputs", tx.tx.output.len().to_string()));
        Self {
            id,
            wallet: WalletFilter::Vault,
            rail: "Bitcoin on-chain",
            timestamp: tx.time.unwrap_or(0) as u64,
            direction,
            status,
            status_label: if status == StatusFilter::Pending {
                "Unconfirmed"
            } else {
                "Confirmed"
            }
            .into(),
            status_note: None,
            title,
            amount: grouped(amount.to_sat()),
            unit: "SATS".into(),
            fee,
            peer,
            identifiers: vec![("Txid", tx.txid.to_string())],
            rail_details: Some(("On-chain details", facts)),
            label_key: Some(label_key),
            target: ActivityTarget::Vault(tx),
            signer_status: None,
        }
    }

    pub fn psbt(tx: SpendTx) -> Self {
        let txid = tx.psbt.unsigned_tx.compute_txid();
        let sigs = tx.sigs.primary_path();
        let signer_status = if sigs.sigs_count >= sigs.threshold {
            format!(
                "Ready for review · {} of {} signatures",
                sigs.sigs_count, sigs.threshold
            )
        } else {
            format!(
                "Awaiting signatures · {} of {} collected",
                sigs.sigs_count, sigs.threshold
            )
        };
        Self {
            id: format!("vault:psbt:{txid}"),
            wallet: WalletFilter::Vault,
            rail: "PSBT and signers",
            timestamp: tx.updated_at.unwrap_or(0) as u64,
            direction: DirectionFilter::Sent,
            status: StatusFilter::Pending,
            status_label: format!("{}/{} signed", sigs.sigs_count, sigs.threshold),
            status_note: None,
            title: tx
                .labels
                .get(&txid.to_string())
                .filter(|label| !label.is_empty())
                .cloned()
                .unwrap_or_else(|| "Vault transaction".into()),
            amount: grouped(tx.spend_amount.to_sat()),
            unit: "SATS".into(),
            fee: tx.fee_amount.map(|fee| sats(fee.to_sat())),
            peer: None,
            // Not broadcast: its txid names nothing on chain yet.
            identifiers: Vec::new(),
            rail_details: None,
            label_key: Some(LabelItem::Txid(txid)),
            target: ActivityTarget::Psbt(tx),
            signer_status: Some(signer_status),
        }
    }

    pub fn spark(payment: PaymentSummary) -> Self {
        let direction = if payment.direction.eq_ignore_ascii_case("receive") {
            DirectionFilter::Received
        } else {
            DirectionFilter::Sent
        };
        let received = direction == DirectionFilter::Received;
        let status = match payment.status.to_ascii_lowercase().as_str() {
            "completed" | "complete" | "succeeded" | "success" => StatusFilter::Confirmed,
            "failed" | "timeout" | "timedout" => StatusFilter::Failed,
            _ => StatusFilter::Pending,
        };
        let details = &payment.details;
        let invoice = non_empty(details.invoice.as_deref());
        let address = non_empty(details.lightning_address.as_deref());
        let tx_id = non_empty(details.tx_id.as_deref());
        let ticker = payment.token_ticker.as_deref().unwrap_or("token");
        let (rail, fallback_title, status_note) = match (payment.method.as_str(), received) {
            ("lightning", false) => (
                "Spark → Lightning",
                match (&address, &invoice) {
                    (Some(address), _) => format!("Lightning address · {address}"),
                    (None, Some(invoice)) => {
                        format!("Lightning invoice · {}", short_identifier(invoice))
                    }
                    (None, None) => "Lightning payment".to_owned(),
                },
                "payment settled",
            ),
            ("lightning", true) => (
                "Lightning → Spark",
                "Lightning invoice paid".to_owned(),
                "payment received",
            ),
            ("deposit", _) => (
                "Bitcoin on-chain → Spark deposit",
                "Bitcoin deposit".to_owned(),
                "claimed into Spark",
            ),
            ("withdraw", _) => (
                "Spark → Bitcoin on-chain",
                "Bitcoin withdrawal".to_owned(),
                "sent on-chain",
            ),
            ("token", true) => ("Spark token", format!("Received {ticker}"), "received"),
            ("token", false) => ("Spark token", format!("Sent {ticker}"), "sent"),
            (_, true) => (
                "Spark",
                "Received on Spark".to_owned(),
                "received instantly",
            ),
            (_, false) => ("Spark", "Sent on Spark".to_owned(), "sent instantly"),
        };
        let (amount, unit) = if let Some(minor) = payment.token_amount {
            (
                format_token_display(minor, payment.token_decimals.unwrap_or(0)),
                ticker.to_owned(),
            )
        } else {
            (
                grouped(payment.amount_sat.unsigned_abs()),
                "SATS".to_owned(),
            )
        };
        let mut identifiers = Vec::new();
        if let Some(hash) = non_empty(details.payment_hash.as_deref()) {
            identifiers.push(("Payment hash", hash));
        }
        match payment.method.as_str() {
            "deposit" | "withdraw" => {
                identifiers.extend(tx_id.clone().map(|txid| ("Txid", txid)));
            }
            "token" => {
                identifiers.extend(tx_id.clone().map(|txid| ("Token transaction", txid)));
            }
            "lightning" => {}
            _ => identifiers.push(("Spark transfer ID", payment.id.clone())),
        }
        if identifiers.is_empty() {
            identifiers.push(("Payment ID", payment.id.clone()));
        }
        let peer = match (payment.method.as_str(), received) {
            ("lightning", false) => address
                .clone()
                .or_else(|| invoice.as_deref().map(short_identifier))
                .map(|value| ("Destination", value)),
            ("lightning", true) => {
                Some(("Counterparty", "Lightning payer · not disclosed".to_owned()))
            }
            _ => None,
        };
        let description = non_empty(payment.description.as_deref());
        let mut facts = Vec::new();
        if let Some(invoice) = &invoice {
            facts.push(("Invoice", short_identifier(invoice)));
        }
        if let Some(description) = &description {
            facts.push(("Description", description.clone()));
        }
        if identifiers.iter().all(|(name, _)| *name != "Payment ID") {
            facts.push(("Payment ID", payment.id.clone()));
        }
        let rail_details = match payment.method.as_str() {
            "lightning" => Some(("Lightning details", facts)),
            "deposit" | "withdraw" => Some(("On-chain details", facts)),
            _ => Some(("Spark details", facts)),
        };
        Self {
            id: format!("spark:{}:{}", payment.method, payment.id),
            wallet: WalletFilter::Spark,
            rail,
            timestamp: payment.timestamp,
            direction,
            status,
            status_label: match status {
                StatusFilter::Confirmed => "Confirmed",
                StatusFilter::Failed => "Failed",
                _ => "Pending",
            }
            .into(),
            status_note: (status == StatusFilter::Confirmed).then_some(status_note),
            title: description.unwrap_or(fallback_title),
            amount,
            unit,
            // Token fees are not reported by the bridge.
            fee: payment
                .token_amount
                .is_none()
                .then(|| sats(payment.fees_sat)),
            peer,
            identifiers,
            rail_details,
            label_key: None,
            target: ActivityTarget::Spark(payment),
            signer_status: None,
        }
    }

    pub fn liquid(payment: DomainPayment) -> Self {
        let source_id = payment
            .stable_identity()
            .map(|identity| format!("{identity:?}"))
            .or_else(|| payment.tx_id.clone())
            .unwrap_or_else(|| {
                format!(
                    "unidentified:{}:{}:{:?}:{}",
                    payment.timestamp,
                    payment.amount_sat,
                    payment.direction,
                    payment.destination.as_deref().unwrap_or("")
                )
            });
        let swap_id = match &payment.details {
            DomainPaymentDetails::Lightning { swap_id, .. }
            | DomainPaymentDetails::OnChainBitcoin { swap_id, .. } => swap_id.clone(),
            _ => None,
        };
        let mut identifiers = Vec::new();
        identifiers.extend(non_empty(payment.tx_id.as_deref()).map(|txid| ("Txid", txid)));
        identifiers.extend(non_empty(swap_id.as_deref()).map(|swap| ("Swap ID", swap)));
        let direction = if payment.direction == DomainPaymentDirection::Receive {
            DirectionFilter::Received
        } else {
            DirectionFilter::Sent
        };
        let status = match payment.status {
            DomainPaymentStatus::Complete => StatusFilter::Confirmed,
            DomainPaymentStatus::Failed
            | DomainPaymentStatus::TimedOut
            | DomainPaymentStatus::Refundable => StatusFilter::Failed,
            _ => StatusFilter::Pending,
        };
        let status_label = match payment.status {
            DomainPaymentStatus::Complete => "Confirmed",
            DomainPaymentStatus::Created => "Created",
            DomainPaymentStatus::Pending => "Pending",
            DomainPaymentStatus::Failed => "Failed",
            DomainPaymentStatus::TimedOut => "Timed out",
            DomainPaymentStatus::Refundable => "Refundable",
            DomainPaymentStatus::RefundPending => "Refund pending",
            DomainPaymentStatus::WaitingFeeAcceptance => "Awaiting fee",
        };
        let (amount, unit, asset) = match &payment.details {
            DomainPaymentDetails::LiquidAsset {
                asset_id,
                asset_info,
                ..
            } => {
                let usdt = asset_id == USDT_ASSET_ID_MAINNET;
                match asset_info {
                    Some(info) => (
                        format_token_display(info.amount_minor, u32::from(info.precision)),
                        if usdt { "USDt" } else { asset_id.as_str() }.to_owned(),
                        if usdt {
                            "USDt on Liquid".to_owned()
                        } else {
                            asset_id.clone()
                        },
                    ),
                    None if usdt => (
                        format_usdt_display(payment.amount_sat),
                        "USDt".to_owned(),
                        "USDt on Liquid".to_owned(),
                    ),
                    None => (
                        grouped(payment.amount_sat),
                        format!("base units · {asset_id}"),
                        asset_id.clone(),
                    ),
                }
            }
            _ => (
                grouped(payment.amount_sat),
                "SATS".to_owned(),
                "L-BTC".to_owned(),
            ),
        };
        let (rail, details_title) = match &payment.details {
            DomainPaymentDetails::Lightning { .. } => ("Lightning on Liquid", "Lightning details"),
            DomainPaymentDetails::OnChainBitcoin { .. } => ("Bitcoin swap", "Swap details"),
            DomainPaymentDetails::LiquidAsset { .. } => ("Liquid asset", "Liquid details"),
        };
        let mut facts = Vec::new();
        match &payment.details {
            DomainPaymentDetails::Lightning {
                description,
                payer_note,
                ..
            } => {
                facts.extend(non_empty(Some(description)).map(|value| ("Description", value)));
                facts.extend(non_empty(payer_note.as_deref()).map(|value| ("Payer note", value)));
            }
            DomainPaymentDetails::OnChainBitcoin {
                bitcoin_address,
                lockup_tx_id,
                claim_tx_id,
                refund_tx_id,
                refund_tx_amount_sat,
                ..
            } => {
                facts.extend(
                    non_empty(bitcoin_address.as_deref()).map(|value| ("Swap address", value)),
                );
                facts.extend(
                    non_empty(lockup_tx_id.as_deref())
                        .map(|value| ("Lockup txid", short_identifier(&value))),
                );
                facts.extend(
                    non_empty(claim_tx_id.as_deref())
                        .map(|value| ("Claim txid", short_identifier(&value))),
                );
                facts.extend(
                    non_empty(refund_tx_id.as_deref())
                        .map(|value| ("Refund txid", short_identifier(&value))),
                );
                facts.extend(refund_tx_amount_sat.map(|value| ("Refund amount", sats(value))));
            }
            DomainPaymentDetails::LiquidAsset { .. } => facts.push(("Asset", asset)),
        }
        let title = payment
            .details
            .description()
            .trim()
            .chars()
            .take(100)
            .collect::<String>();
        let title = if title.is_empty() {
            rail.to_owned()
        } else {
            title
        };
        let peer = (direction == DirectionFilter::Sent)
            .then(|| non_empty(payment.destination.as_deref()))
            .flatten()
            .map(|destination| ("Destination", short_identifier(&destination)));
        let fee = match (payment.fees_sat, direction) {
            (0, DirectionFilter::Received) => "Paid by sender".to_owned(),
            (fee, _) => sats(fee),
        };
        Self {
            id: format!("liquid:{source_id}"),
            wallet: WalletFilter::Liquid,
            rail,
            timestamp: payment.timestamp as u64,
            direction,
            status,
            status_label: status_label.into(),
            status_note: None,
            title,
            amount,
            unit,
            fee: Some(fee),
            peer,
            identifiers,
            rail_details: (!facts.is_empty()).then_some((details_title, facts)),
            label_key: None,
            target: ActivityTarget::Liquid(payment),
            signer_status: None,
        }
    }

    pub fn refundable(swap: DomainRefundableSwap) -> Self {
        let address = swap.swap_address.clone();
        Self {
            id: format!("liquid:refund:{address}"),
            wallet: WalletFilter::Liquid,
            rail: "Bitcoin swap refund",
            timestamp: swap.timestamp as u64,
            direction: DirectionFilter::Received,
            status: StatusFilter::Failed,
            status_label: "Refundable".into(),
            status_note: Some("refund it from the Liquid wallet"),
            title: "Refund available".into(),
            amount: grouped(swap.amount_sat),
            unit: "SATS".into(),
            fee: None,
            peer: None,
            identifiers: vec![("Swap address", address)],
            rail_details: None,
            label_key: None,
            target: ActivityTarget::Refundable(swap),
            signer_status: None,
        }
    }
}

const MASK: &str = "••••••";

/// A Lightning invoice or offer: its prefix encodes the amount (`lnbc42u1…`
/// is 4,200 sats), so it is hidden with the balances.
fn shows_invoice(value: &str) -> bool {
    value.split_whitespace().any(|word| {
        let word = word.to_ascii_lowercase();
        ["lnbc", "lntb", "lnsb", "lno1", "lni1"]
            .iter()
            .any(|prefix| word.starts_with(prefix))
    })
}

/// The row's title while balances may be hidden.
fn shown_title(entry: &Entry, masked: bool) -> String {
    if masked && shows_invoice(&entry.title) {
        "Lightning payment".to_owned()
    } else {
        entry.title.clone()
    }
}

/// A detail value while balances may be hidden: amounts go, and so does
/// any invoice (a Spark invoice can carry an amount too) or payment URI
/// naming one.
fn private_fact(key: &str, value: &str, masked: bool) -> String {
    let names_amount = matches!(key, "Refund amount" | "Invoice")
        || shows_invoice(value)
        || value.to_ascii_lowercase().contains("amount=");
    if masked && names_amount {
        MASK.to_owned()
    } else {
        value.to_owned()
    }
}

fn fee_detail(fee: &str, amounts_masked: bool) -> String {
    if amounts_masked {
        MASK.to_owned()
    } else {
        fee.to_owned()
    }
}

fn vault_confirmation_detail(height: i32, tip: i32) -> String {
    if tip <= 0 || tip < height {
        "Unavailable".to_owned()
    } else {
        grouped((i64::from(tip) - i64::from(height) + 1) as u64)
    }
}

// `.cols`: Time 44, Wallet 112, Details (at least 180 here), Amount 128 and
// Status 132, 12px apart, inside the row's 12px insets. The list's own side
// insets come from the page gutter (`list_inset`).
const MIN_ACTIVITY_ROWS_WIDTH: f32 = 44.0 + 112.0 + 180.0 + 128.0 + 132.0 + 4.0 * 12.0 + 2.0 * 12.0;
// `.row`'s side padding: row text lands on the page gutter.
const ROW_INSET: f32 = 12.0;

/// `.lst`'s side inset at a content width: the page gutter less the row's own
/// 12px, so day headings and row text start on the gutter like every page title.
fn list_inset(width: f32) -> f32 {
    super::page_gutter(width) - ROW_INSET
}
// `.det { width: 420px }`.
const SELECTED_DETAIL_WIDTH: f32 = 420.0;
const ACTIVITY_LIST_SCROLL_ID: &str = "tenshu.activity.list.scroll";
const ACTIVITY_DETAIL_SCROLL_ID: &str = "tenshu.activity.detail.scroll";
const ROW_HEIGHT: f32 = 56.0;

fn detail_beside_list(width: f32) -> bool {
    width >= MIN_ACTIVITY_ROWS_WIDTH + 2.0 * list_inset(width) + DETAIL_SPAN
}

/// The pane and the rule before it.
const DETAIL_SPAN: f32 = SELECTED_DETAIL_WIDTH + 1.0;

/// Where Activity's page column sits in a pane `width` wide: `left` and
/// `right` are its distances from the pane's edges.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Frame {
    left: f32,
    column: f32,
    right: f32,
}

/// Activity's header, filters and rows share the page column every Cube page
/// uses (`tenshu::page`), centred. A detail pane beside the list sits at the
/// pane's right edge. The column keeps its place while it clears the pane by a
/// gutter, then slides left, then narrows once it reaches the left gutter, so
/// it never jumps as the window is resized.
fn column_frame(width: f32, detail_beside: bool) -> Frame {
    let gutter = super::page_gutter(width);
    let (centred_left, centred_column) = tenshu::content_frame(width, PageWidth::Reading, gutter);
    let (left, column) = if detail_beside {
        let space = width - DETAIL_SPAN;
        let column = centred_column.min((space - 2.0 * gutter).max(0.0));
        (
            centred_left.min(space - gutter - column).max(gutter),
            column,
        )
    } else {
        (centred_left, centred_column)
    };
    Frame {
        left,
        column,
        right: width - left - column,
    }
}

/// `.day`: a relative name, then the date in the muted ink.
fn day_heading(day: Option<NaiveDate>, today: NaiveDate) -> (String, String) {
    let Some(day) = day else {
        return ("Pending".to_owned(), String::new());
    };
    let date = day.format("%a %-d %b %Y").to_string();
    let age = (today - day).num_days();
    let relative = match age {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        2..=6 => day.format("%A").to_string(),
        7..=13 => "Last week".to_owned(),
        _ => "Earlier".to_owned(),
    };
    (relative, date)
}

fn signed_amount(entry: &Entry, masked: bool) -> String {
    if masked {
        return "••••••".to_owned();
    }
    let sign = match (&entry.target, entry.direction) {
        // A refundable swap is available to reclaim; the user has not received it yet.
        (ActivityTarget::Refundable(_), _) => "",
        (_, DirectionFilter::Received) => "+",
        (_, DirectionFilter::Sent) => "−",
        (_, DirectionFilter::Transfers | DirectionFilter::All) => "",
    };
    format!("{sign}{}", entry.amount)
}

fn wallet_glyph(wallet: WalletFilter) -> Option<&'static str> {
    match wallet {
        WalletFilter::Vault => Some(glyph::SHIELD),
        WalletFilter::Spark => Some(glyph::SPARK_STROKE),
        WalletFilter::Liquid => Some(glyph::DROPLET),
        WalletFilter::All => None,
    }
}

fn direction_glyph(direction: DirectionFilter) -> Option<&'static str> {
    match direction {
        DirectionFilter::Received => Some(glyph::RECEIVED),
        DirectionFilter::Sent => Some(glyph::SENT),
        DirectionFilter::Transfers => Some(glyph::TRANSFER),
        DirectionFilter::All => None,
    }
}

/// A PSBT waiting on signers reads as pending with the signing mark.
fn status_tone(entry: &Entry) -> (&'static str, fn(&theme::Theme) -> iced::Color) {
    if entry.signer_status.is_some() {
        return (glyph::SIGNING, glyph::warn);
    }
    match entry.status {
        StatusFilter::Confirmed => (glyph::CHECK_CIRCLE, glyph::good),
        StatusFilter::Failed => (glyph::X_CIRCLE, glyph::bad),
        StatusFilter::Pending => (glyph::PENDING, glyph::warn),
        StatusFilter::All => (glyph::PENDING, glyph::muted),
    }
}

fn words<'a>(
    value: impl Into<String>,
    size: f32,
    weight: iced::Font,
    color: fn(&theme::Theme) -> iced::Color,
) -> iced::widget::Text<'a, theme::Theme> {
    text(value.into())
        .size(size)
        .font(weight)
        .style(move |theme: &theme::Theme| iced::widget::text::Style {
            color: Some(color(theme)),
        })
}

/// One line that never wraps; long text is clipped at its cell.
fn one_line<'a>(content: iced::widget::Text<'a, theme::Theme>) -> Element<'a, Message> {
    container(content.wrapping(iced::widget::text::Wrapping::None))
        .clip(true)
        .width(Length::Fill)
        .into()
}

fn status_cell<'a>(entry: &Entry, size: f32, note: bool) -> Element<'a, Message> {
    let (mark, tone) = status_tone(entry);
    let label = match entry.status_note.filter(|_| note) {
        Some(note) => format!("{} · {note}", entry.status_label),
        None => entry.status_label.clone(),
    };
    row![
        glyph::glyph(mark, 16.0, tone),
        words(label, size, font::MEDIUM, tone).wrapping(iced::widget::text::Wrapping::WordOrGlyph)
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
}

/// `.dl > div`: a 116px muted key beside its value, over a rule.
fn fact<'a>(key: &'static str, value: String, mono: bool) -> Element<'a, Message> {
    let value = text(value)
        .size(if mono { 12.5 } else { 13.5 })
        .font(if mono {
            font::MONO_REGULAR
        } else {
            font::REGULAR
        })
        .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
        .width(Length::Fill);
    column![
        row![
            words(key, 13.5, font::REGULAR, glyph::muted).width(Length::Fixed(116.0)),
            value
        ]
        .spacing(12)
        .padding([8, 0]),
        rule::horizontal(1),
    ]
    .into()
}

/// The ordinary Iced button used by the shared Collapse component does not
/// register a keyboard focus target. Activity's completed PSBT disclosure
/// keeps its own view state so Tab, Enter, and Space work without app state.
struct PsbtDisclosure {
    entry_id: String,
    status: String,
    required: usize,
    collected: usize,
}

#[derive(Clone)]
enum PsbtDisclosureEvent {
    Focus(Id),
    Toggle,
}

#[allow(deprecated)]
impl Component<Message, theme::Theme, iced::Renderer> for PsbtDisclosure {
    type State = Option<(String, bool)>;
    type Event = PsbtDisclosureEvent;

    fn update(&mut self, state: &mut Self::State, event: Self::Event) -> Option<Message> {
        match event {
            PsbtDisclosureEvent::Focus(id) => Some(Message::FocusAction(id)),
            PsbtDisclosureEvent::Toggle => {
                let expanded = state
                    .as_ref()
                    .is_some_and(|(id, expanded)| id == &self.entry_id && *expanded);
                *state = Some((self.entry_id.clone(), !expanded));
                None
            }
        }
    }

    fn view(&self, state: &Self::State) -> Element<'_, Self::Event> {
        let expanded = state
            .as_ref()
            .is_some_and(|(id, expanded)| id == &self.entry_id && *expanded);
        let header = Action::new(
            Id::from("tenshu.activity.detail.psbt.disclosure"),
            row![
                words("PSBT and signers", 14.0, font::SEMIBOLD, glyph::ink).width(Length::Fill),
                words(&self.status, 12.0, font::REGULAR, glyph::muted),
                glyph::glyph(
                    if expanded {
                        glyph::CHEVRON_UP
                    } else {
                        glyph::CHEVRON_DOWN
                    },
                    16.0,
                    glyph::muted
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            PsbtDisclosureEvent::Focus,
        )
        .style(theme::button::transparent_border)
        .padding([12, 0])
        .width(Length::Fill)
        .on_press(PsbtDisclosureEvent::Toggle);
        let mut content = Column::new().push(header).push(rule::horizontal(1));
        if expanded {
            for (key, value) in [
                ("Required", format!("{} signatures", self.required)),
                ("Collected", format!("{} signatures", self.collected)),
            ] {
                content = content.push(column![
                    row![
                        words(key, 13.5, font::REGULAR, glyph::muted).width(Length::Fixed(116.0)),
                        text(value).size(13.5)
                    ]
                    .spacing(12)
                    .padding([8, 0]),
                    rule::horizontal(1),
                ]);
            }
        }
        content.into()
    }
}

#[allow(deprecated)]
fn completed_psbt_disclosure<'a>(
    entry_id: &str,
    status: &str,
    required: usize,
    collected: usize,
) -> Element<'a, Message> {
    component(PsbtDisclosure {
        entry_id: entry_id.to_owned(),
        status: status.to_owned(),
        required,
        collected,
    })
}

pub fn render<'a>(activity: &'a Activity, cache: &'a Cache) -> Element<'a, Message> {
    iced::widget::responsive(move |size: iced::Size| render_at_width(activity, cache, size.width))
        .into()
}

/// The Cube's coin before its name (`.idl > img`).
fn network_coin<'a>(cache: &Cache) -> Element<'a, Message> {
    let light = matches!(
        cache.theme_mode,
        coincube_ui::theme::palette::ThemeMode::Light
    );
    iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(
        crate::home::launcher_coin::widget_source(cache.chain(), 16.0, 1.0, light).into_bytes(),
    ))
    .width(Length::Fixed(16.0))
    .height(Length::Fixed(16.0))
    .into()
}

/// `.lbtn`: an accent text button.
fn link_button<'a>(
    id: &'static str,
    label: &'static str,
    message: Message,
) -> Element<'a, Message> {
    Action::new(
        id,
        words(label, 13.0, font::SEMIBOLD, glyph::accent),
        Message::FocusAction,
    )
    .padding([4, 6])
    .style(theme::button::transparent_border)
    .on_press(message)
    .into()
}

/// The wallet filter's options. Liquid is one only while the Liquid gate
/// shows it.
fn wallet_filters(activity: &Activity) -> Vec<WalletFilter> {
    [
        WalletFilter::All,
        WalletFilter::Spark,
        WalletFilter::Vault,
        WalletFilter::Liquid,
    ]
    .iter()
    .copied()
    .filter(|wallet| *wallet != WalletFilter::Liquid || activity.liquid_shown())
    .collect()
}

fn filters_bar<'a>(activity: &'a Activity, frame: Frame) -> Element<'a, Message> {
    let wallet = tenshu::select(
        "tenshu.activity.filter.wallet",
        "Wallet",
        wallet_filters(activity)
            .into_iter()
            .map(|value| (value, value.to_string())),
        Some(activity.filters.wallet),
        Message::FocusAction,
    )
    .width(Length::Fixed(180.0))
    .on_select(|value| Message::Activity(ActivityMessage::Wallet(value)));
    let direction = tenshu::select(
        "tenshu.activity.filter.direction",
        "Direction",
        [
            DirectionFilter::All,
            DirectionFilter::Received,
            DirectionFilter::Sent,
            DirectionFilter::Transfers,
        ]
        .map(|value| (value, value.to_string())),
        Some(activity.filters.direction),
        Message::FocusAction,
    )
    .width(Length::Fixed(140.0))
    .on_select(|value| Message::Activity(ActivityMessage::Direction(value)));
    let status = tenshu::select(
        "tenshu.activity.filter.status",
        "Status",
        [
            StatusFilter::All,
            StatusFilter::Pending,
            StatusFilter::Confirmed,
            StatusFilter::Failed,
        ]
        .map(|value| (value, value.to_string())),
        Some(activity.filters.status),
        Message::FocusAction,
    )
    .width(Length::Fixed(140.0))
    .on_select(|value| Message::Activity(ActivityMessage::Status(value)));
    let dates = tenshu::text_field(
        "tenshu.activity.filter.dates",
        "Date range",
        &activity.filters.date_range,
        Message::FocusAction,
    )
    .size(FieldSize::Compact)
    .placeholder("YYYY-MM-DD to YYYY-MM-DD")
    .error_maybe(
        activity
            .filters
            .dates()
            .is_err()
            .then_some("Use YYYY-MM-DD to YYYY-MM-DD"),
    )
    .width(Length::Fixed(220.0))
    .on_input(|value| Message::Activity(ActivityMessage::DateRange(value)));
    let search = container(
        tenshu::search_field(
            "tenshu.activity.filter.search",
            "Search label or id",
            &activity.filters.search,
            Message::FocusAction,
        )
        .placeholder("Label, txid, hash or swap ID")
        .on_input(|value| Message::Activity(ActivityMessage::Search(value))),
    )
    .width(Length::Fill)
    .max_width(280);
    // `.filters`: one row when it fits, else selects over the text fields.
    let controls: Element<'a, Message> = if frame.column < 920.0 {
        column![
            row![wallet, direction, status].spacing(12),
            row![dates, search].spacing(12)
        ]
        .spacing(12)
        .into()
    } else {
        row![wallet, direction, status, dates, search]
            .spacing(12)
            .align_y(Alignment::Start)
            .into()
    };
    column![
        container(controls)
            .padding(iced::Padding {
                top: 16.0,
                right: frame.right,
                bottom: 16.0,
                left: frame.left,
            })
            .width(Length::Fill),
        rule::horizontal(1)
    ]
    .into()
}

fn header<'a>(
    activity: &'a Activity,
    cache: &'a Cache,
    shown: usize,
    loaded: usize,
    frame: Frame,
) -> Element<'a, Message> {
    let count = activity.filters.count();
    let title = column![
        words("Activity", 22.0, font::SEMIBOLD, glyph::ink),
        row![
            network_coin(cache),
            words(
                format!(
                    "{} · {}",
                    if cache.cube_name.is_empty() {
                        "Cube"
                    } else {
                        cache.cube_name.as_str()
                    },
                    super::nav::shell::network_label(cache.chain())
                ),
                13.0,
                font::REGULAR,
                glyph::muted
            )
        ]
        .spacing(6)
        .align_y(Alignment::Center)
    ]
    .spacing(4);
    let mut actions = Row::new().spacing(10).align_y(Alignment::Center);
    if count > 0 {
        actions = actions
            .push(words(
                if count == 1 {
                    "1 filter active".to_owned()
                } else {
                    format!("{count} filters active")
                },
                13.0,
                font::MEDIUM,
                glyph::ink,
            ))
            .push(link_button(
                "tenshu.activity.clear",
                "Clear filters",
                Message::Activity(ActivityMessage::Clear),
            ));
    }
    let more = activity.vault_has_more || activity.spark_has_more || activity.liquid_has_more;
    actions = actions.push(words(
        if more {
            format!("Showing {shown} of {loaded} loaded")
        } else {
            format!("Showing {shown} of {loaded}")
        },
        13.0,
        font::REGULAR,
        glyph::muted,
    ));
    let body: Element<'a, Message> = if frame.column < 660.0 {
        column![title, actions].spacing(8).into()
    } else {
        row![title, Space::new().width(Length::Fill), actions]
            .align_y(Alignment::End)
            .into()
    };
    // `.phd`: the page's 28px top, on the page column like every Cube page.
    container(body)
        .padding(iced::Padding {
            top: 28.0,
            right: frame.right,
            bottom: 0.0,
            left: frame.left,
        })
        .into()
}

fn columns<'a>() -> Element<'a, Message> {
    let heading = |label: &'static str| words(label, 12.0, font::REGULAR, glyph::muted);
    column![
        container(
            row![
                heading("Time").width(Length::Fixed(44.0)),
                heading("Wallet").width(Length::Fixed(112.0)),
                heading("Details").width(Length::Fill),
                heading("Amount")
                    .align_x(iced::alignment::Horizontal::Right)
                    .width(Length::Fixed(128.0)),
                heading("Status").width(Length::Fixed(132.0)),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .padding([0, 12])
        .height(Length::Fixed(36.0))
        .align_y(iced::alignment::Vertical::Center),
        rule::horizontal(1)
    ]
    .into()
}

fn activity_row<'a>(entry: &Entry, selected: bool, masked: bool) -> Element<'a, Message> {
    let time = (entry.timestamp != 0)
        .then(|| DateTime::<Utc>::from_timestamp(entry.timestamp as i64, 0))
        .flatten()
        .map_or_else(
            || "—".to_owned(),
            |time| time.with_timezone(&Local).format("%H:%M").to_string(),
        );
    entry_row(entry, selected, masked, time, 44.0, true)
}

/// One `.tr`: time, wallet (Activity only), details, amount and status.
fn entry_row<'a>(
    entry: &Entry,
    selected: bool,
    masked: bool,
    time: String,
    time_width: f32,
    with_wallet: bool,
) -> Element<'a, Message> {
    let failed = entry.status == StatusFilter::Failed && entry.signer_status.is_none();
    let mut wallet = Row::new().spacing(8).align_y(Alignment::Center);
    if let Some(mark) = wallet_glyph(entry.wallet) {
        wallet = wallet.push(glyph::glyph(mark, 16.0, glyph::muted));
    }
    wallet = wallet.push(one_line(words(
        entry.wallet.to_string(),
        13.0,
        font::REGULAR,
        glyph::ink,
    )));
    let mut details = Row::new().spacing(8).align_y(Alignment::Center);
    if let Some(mark) = direction_glyph(entry.direction) {
        details = details.push(glyph::glyph(mark, 16.0, glyph::muted));
    }
    details = details.push(one_line(words(
        shown_title(entry, masked),
        14.0,
        font::REGULAR,
        glyph::ink,
    )));
    // `.amt`: the figure in 600 with its unit in muted 12px; `.amt.void` for
    // a payment that moved nothing.
    let mut amount = Row::new().spacing(3).align_y(Alignment::End).push(words(
        signed_amount(entry, masked),
        14.0,
        if failed { font::MEDIUM } else { font::SEMIBOLD },
        if failed { glyph::muted } else { glyph::ink },
    ));
    if !masked {
        amount = amount.push(
            words(entry.unit.clone(), 12.0, font::REGULAR, glyph::muted)
                .wrapping(iced::widget::text::Wrapping::None),
        );
    }
    let mut body = Row::new().push(
        words(time, 13.0, font::REGULAR, glyph::muted)
            .wrapping(iced::widget::text::Wrapping::None)
            .width(Length::Fixed(time_width)),
    );
    if with_wallet {
        body = body.push(container(wallet).width(Length::Fixed(112.0)).clip(true));
    }
    let body = body
        .push(container(details).width(Length::Fill).clip(true))
        .push(
            container(amount)
                .width(Length::Fixed(128.0))
                .align_x(iced::alignment::Horizontal::Right)
                .clip(true),
        )
        .push(
            container(status_cell(entry, 13.0, false))
                .width(Length::Fixed(132.0))
                .clip(true),
        )
        .spacing(12)
        .height(Length::Fixed(ROW_HEIGHT - 1.0))
        .align_y(Alignment::Center);
    column![
        Action::new(row_focus_id(&entry.id), body, Message::FocusAction)
            .width(Length::Fill)
            .padding([0, 12])
            .style(move |theme, status| {
                let mut style = theme::button::transparent_border(theme, status);
                style.border.radius = 0.0.into();
                if selected {
                    // `.row.sel.kf`: the selected fill inside a 2px focus ring.
                    style.background = Some(iced::Background::Color(theme.colors.shell.selected));
                    style.border.color = theme.colors.shell.focus;
                    style.border.width = 2.0;
                    style.border.radius = 6.0.into();
                }
                style
            })
            .on_press(Message::Activity(ActivityMessage::Select(entry.id.clone()))),
        rule::horizontal(1)
    ]
    .into()
}

/// Rows a wallet page's "Recent activity" shows.
pub const RECENT_ACTIVITY_ROWS: usize = 5;
/// `.tr .tm` on a wallet page: no day headings, so the time carries its day.
const RECENT_TIME_WIDTH: f32 = 104.0;

/// "Today 09:18", "Yesterday 11:05", "Oct 19, 21:15".
fn recent_when(timestamp: u64, today: NaiveDate) -> String {
    let Some(time) = (timestamp != 0)
        .then(|| DateTime::<Utc>::from_timestamp(timestamp as i64, 0))
        .flatten()
        .map(|time| time.with_timezone(&Local))
    else {
        return "Pending".to_owned();
    };
    match (today - time.date_naive()).num_days() {
        0 => time.format("Today %H:%M").to_string(),
        1 => time.format("Yesterday %H:%M").to_string(),
        _ => time.format("%b %-d, %H:%M").to_string(),
    }
}

/// Whether `wallet`'s history is loading, and a failed read with what it
/// read. A Vault's history is its transactions: its pending proposals load
/// beside it and report their own failure.
fn wallet_history_state(
    activity: &Activity,
    wallet: WalletFilter,
) -> (bool, Option<(&'static str, &str)>) {
    match wallet {
        WalletFilter::Vault => (
            activity.vault_loading,
            activity
                .vault_error
                .as_deref()
                .map(|error| ("Vault history", error))
                .or(activity
                    .psbt_error
                    .as_deref()
                    .map(|error| ("Vault proposals", error))),
        ),
        WalletFilter::Spark => (
            activity.spark_loading,
            activity
                .spark_error
                .as_deref()
                .map(|error| ("Spark history", error)),
        ),
        WalletFilter::Liquid => (
            activity.liquid_loading,
            activity
                .liquid_error
                .as_deref()
                .map(|error| ("Liquid history", error))
                .or(activity
                    .refundables_error
                    .as_deref()
                    .map(|error| ("Liquid refunds", error))),
        ),
        WalletFilter::All => (false, None),
    }
}

/// The selected row, while it is one of the rows `wallet`'s page shows.
fn wallet_selection(activity: &Activity, wallet: WalletFilter) -> Option<Entry> {
    let id = activity.selected_id.as_deref()?;
    activity
        .wallet_entries(wallet)
        .into_iter()
        .take(RECENT_ACTIVITY_ROWS)
        .find(|entry| entry.id == id)
}

/// A wallet page's "Recent activity" (`P-Overview` Vault and Spark): its
/// newest rows in Activity's own row, a row press opens the detail pane
/// beside the page, and "View all transactions" opens Activity filtered to
/// the wallet.
///
/// `unknown_history` replaces the empty-list copy while the wallet's own
/// history is not known yet (a Vault before its scan completes), so an
/// empty answer never reads as "No transactions yet".
pub fn wallet_recent_activity<'a>(
    activity: &'a Activity,
    cache: &'a Cache,
    wallet: WalletFilter,
    unknown_history: Option<&'static str>,
) -> Element<'a, Message> {
    let masked = cache.balance_amounts_masked();
    let entries = activity.wallet_entries(wallet);
    let selected = activity.selected_id.as_deref();
    let (loading, error) = wallet_history_state(activity, wallet);
    let today = Local::now().date_naive();

    let mut content = Column::new()
        .spacing(10)
        .push(coincube_ui::component::text::h4_bold("Recent activity"));
    if let Some((source, error)) = error {
        content = content.push(
            row![
                glyph::glyph(glyph::WARNING, 16.0, glyph::warn),
                words(
                    if entries.is_empty() {
                        format!("{source} couldn't load")
                    } else {
                        format!("Last known activity · {source} couldn't load: {error}")
                    },
                    13.0,
                    font::REGULAR,
                    glyph::warn
                )
                .width(Length::Fill),
                Action::new(
                    Id::from(format!("tenshu.recent.{wallet}.retry")),
                    text("Retry").size(13).font(font::MEDIUM),
                    Message::FocusAction,
                )
                .style(theme::button::secondary)
                .padding([6, 12])
                .on_press(Message::Activity(ActivityMessage::RetryWallet(wallet))),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );
    }
    let mut rows = Column::new().push(rule::horizontal(1));
    if entries.is_empty() {
        let note = if let Some(note) = unknown_history {
            note
        } else if loading {
            "Loading payment history…"
        } else if error.is_some() {
            "No transactions to show"
        } else {
            "No transactions yet"
        };
        rows = rows.push(column![
            container(words(note, 14.0, font::REGULAR, glyph::muted))
                .height(Length::Fixed(ROW_HEIGHT - 1.0))
                .align_y(iced::alignment::Vertical::Center)
                .padding([0, 12]),
            rule::horizontal(1)
        ]);
    }
    for entry in entries.iter().take(RECENT_ACTIVITY_ROWS) {
        rows = rows.push(entry_row(
            entry,
            selected == Some(entry.id.as_str()),
            masked,
            recent_when(entry.timestamp, today),
            RECENT_TIME_WIDTH,
            false,
        ));
    }
    content = content.push(rows);
    if !entries.is_empty() {
        content = content.push(
            Action::new(
                Id::from(format!("tenshu.recent.{wallet}.all")),
                words("View all transactions", 13.0, font::SEMIBOLD, glyph::accent),
                Message::FocusAction,
            )
            .padding([4, 0])
            .style(theme::button::transparent_border)
            .on_press(Message::Activity(ActivityMessage::ViewAll(wallet))),
        );
    }
    content.into()
}

/// The detail pane for a wallet page's selected row: Activity's own pane, so
/// labels, identifiers and the hand-off to the wallet's screen behave alike.
pub fn wallet_activity_detail<'a>(
    activity: &'a Activity,
    cache: &'a Cache,
    wallet: WalletFilter,
) -> Option<Element<'a, Message>> {
    let entry = wallet_selection(activity, wallet)?;
    Some(detail_surface(detail_pane(activity, &entry, cache)).into())
}

/// Whether a wallet page at `width` keeps the detail pane beside it; below
/// this it slides over the page from the right edge instead.
pub fn wallet_detail_beside(width: f32) -> bool {
    width
        >= RECENT_ROWS_MIN_WIDTH
            + 2.0 * (super::page_gutter(width) + WALLET_PAGE_INSET)
            + 1.0
            + SELECTED_DETAIL_WIDTH
}

/// The Vault and Spark overviews' own side padding inside the page.
const WALLET_PAGE_INSET: f32 = 16.0;

// `.tr` on a wallet page: Time 104, Details (at least 160 here), Amount 128 and
// Status 132, 12px apart, inside the row's 12px insets.
const RECENT_ROWS_MIN_WIDTH: f32 =
    RECENT_TIME_WIDTH + 160.0 + 128.0 + 132.0 + 3.0 * 12.0 + 2.0 * ROW_INSET;

/// `.det`: the 420px pane at the content's right edge, scrolling on its own.
pub fn detail_column<'a>(detail: Element<'a, Message>) -> Element<'a, Message> {
    container(
        scrollable(detail)
            .id(Id::from(ACTIVITY_DETAIL_SCROLL_ID))
            .height(Length::Fill),
    )
    .width(Length::Fixed(SELECTED_DETAIL_WIDTH))
    .height(Length::Fill)
    .style(|theme: &theme::Theme| iced::widget::container::Style {
        background: Some(iced::Background::Color(theme.colors.shell.panel)),
        ..Default::default()
    })
    .into()
}

fn identifier_row<'a>(index: usize, name: &'static str, value: &str) -> Element<'a, Message> {
    let copy = Action::new(
        Id::from(format!("tenshu.activity.detail.copy.{index}")),
        container(glyph::glyph(glyph::COPY, 16.0, glyph::ink))
            .center_x(Length::Fixed(28.0))
            .center_y(Length::Fixed(28.0)),
        Message::FocusAction,
    )
    .padding(0)
    .style(|theme: &theme::Theme, status| {
        let shell = theme.colors.shell;
        let hovered = matches!(
            status,
            iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed
        );
        iced::widget::button::Style {
            background: hovered.then_some(iced::Background::Color(shell.selected)),
            text_color: shell.ink,
            border: iced::Border {
                color: shell.line,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        }
    })
    .on_press(Message::CopyWithToast(
        value.to_owned(),
        format!("{name} copied"),
    ));
    let copy = iced::widget::tooltip(
        copy,
        container(words(
            format!("Copy {name}"),
            12.0,
            font::REGULAR,
            glyph::muted,
        ))
        .padding([6, 10])
        .style(theme::container::shell_menu),
        iced::widget::tooltip::Position::Left,
    );
    column![
        row![
            column![
                words(name, 12.0, font::REGULAR, glyph::muted),
                one_line(
                    text(short_identifier(value))
                        .size(12.5)
                        .font(font::MONO_REGULAR)
                ),
            ]
            .spacing(2)
            .width(Length::Fill),
            copy,
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .padding([8, 0]),
        rule::horizontal(1),
    ]
    .into()
}

/// What a row asks of the user next, handed to the owning wallet's screen,
/// which keeps its signing, refund and replacement safeguards. Settled rows
/// ask nothing; a PSBT still collecting signatures shows "Continue signing"
/// with its signers instead.
fn next_step(entry: &Entry, replaceable: bool) -> Option<&'static str> {
    match &entry.target {
        ActivityTarget::Refundable(_) => Some("Refund"),
        ActivityTarget::Psbt(tx) => {
            let path = tx.sigs.primary_path();
            (path.sigs_count >= path.threshold).then_some("Review and broadcast")
        }
        // The Vault screen offers a replacement only on a wallet that
        // supports it, while unconfirmed, and with a known fee (otherwise
        // coins may be missing).
        ActivityTarget::Vault(tx) => (replaceable && tx.time.is_none() && tx.fee_amount.is_some())
            .then_some("Bump fee or cancel"),
        ActivityTarget::Spark(_) | ActivityTarget::Liquid(_) => None,
    }
}

pub const DETAIL_NEXT_STEP_ID: &str = "tenshu.activity.detail.next";

fn detail_pane<'a>(activity: &'a Activity, entry: &Entry, cache: &'a Cache) -> Column<'a, Message> {
    let masked = cache.balance_amounts_masked();
    let needs_signatures = matches!(
        &entry.target,
        ActivityTarget::Psbt(tx) if {
            let path = tx.sigs.primary_path();
            path.sigs_count < path.threshold
        }
    );
    let close = Action::new(
        DETAIL_CLOSE_ID,
        container(glyph::glyph(glyph::CLOSE, 18.0, glyph::muted))
            .center_x(Length::Fixed(32.0))
            .center_y(Length::Fixed(32.0)),
        Message::FocusAction,
    )
    .padding(0)
    .style(theme::button::transparent_border)
    .on_press(Message::Activity(ActivityMessage::CloseDetail));
    let close = iced::widget::tooltip(
        close,
        container(words("Close details", 12.0, font::REGULAR, glyph::muted))
            .padding([6, 10])
            .style(theme::container::shell_menu),
        iced::widget::tooltip::Position::Bottom,
    );
    let mut heading = Row::new().spacing(10).align_y(Alignment::Start);
    if let Some(mark) = direction_glyph(entry.direction) {
        heading = heading.push(container(glyph::glyph(mark, 18.0, glyph::muted)).padding(
            iced::Padding {
                top: 3.0,
                ..Default::default()
            },
        ));
    }
    heading = heading
        .push(
            words(shown_title(entry, masked), 17.0, font::SEMIBOLD, glyph::ink)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                .width(Length::Fill),
        )
        .push(close);
    let failed = entry.status == StatusFilter::Failed && entry.signer_status.is_none();
    let mut figure = Row::new().spacing(4).align_y(Alignment::End).push(words(
        signed_amount(entry, masked),
        28.0,
        font::SEMIBOLD,
        if failed { glyph::muted } else { glyph::ink },
    ));
    if !masked {
        figure = figure.push(
            container(words(entry.unit.clone(), 15.0, font::MEDIUM, glyph::muted)).padding(
                iced::Padding {
                    bottom: 4.0,
                    ..Default::default()
                },
            ),
        );
    }
    let mut detail = Column::new()
        .push(heading)
        .push(Space::new().height(Length::Fixed(4.0)))
        .push(words(
            format!("{} · {}", entry.wallet, entry.rail),
            13.0,
            font::REGULAR,
            glyph::muted,
        ))
        .push(Space::new().height(Length::Fixed(16.0)))
        .push(figure)
        .push(Space::new().height(Length::Fixed(2.0)))
        .push(words(
            "Historical fiat amount unavailable",
            13.0,
            font::REGULAR,
            glyph::muted,
        ))
        .push(Space::new().height(Length::Fixed(12.0)))
        .push(status_cell(entry, 13.5, true));

    // A PSBT still collecting signatures leads with its signers (`.dsec`).
    if let (true, ActivityTarget::Psbt(tx)) = (needs_signatures, &entry.target) {
        let path = tx.sigs.primary_path();
        detail = detail
            .push(Space::new().height(Length::Fixed(20.0)))
            .push(words("PSBT and signers", 14.0, font::SEMIBOLD, glyph::ink))
            .push(Space::new().height(Length::Fixed(4.0)))
            .push(words(
                entry.signer_status.clone().unwrap_or_default(),
                13.0,
                font::MEDIUM,
                glyph::warn,
            ))
            .push(Space::new().height(Length::Fixed(8.0)))
            .push(rule::horizontal(1))
            .push(fact(
                "Required",
                format!("{} signatures", path.threshold),
                false,
            ))
            .push(fact(
                "Collected",
                format!("{} signatures", path.sigs_count),
                false,
            ))
            .push(fact(
                "Remaining",
                format!(
                    "{} signatures",
                    path.threshold.saturating_sub(path.sigs_count)
                ),
                false,
            ))
            .push(Space::new().height(Length::Fixed(16.0)))
            .push(
                Action::new(
                    Id::from("tenshu.activity.detail.sign"),
                    text("Continue signing").size(14).font(font::SEMIBOLD),
                    Message::FocusAction,
                )
                .style(theme::button::primary)
                .padding([8, 16])
                .on_press(Message::Activity(ActivityMessage::OpenDetail(
                    entry.target.clone(),
                ))),
            );
    }

    // `.dl.dsec`: the facts under a rule.
    detail = detail
        .push(Space::new().height(Length::Fixed(20.0)))
        .push(rule::horizontal(1));
    if let Some(time) = (entry.timestamp != 0)
        .then(|| DateTime::<Utc>::from_timestamp(entry.timestamp as i64, 0))
        .flatten()
    {
        let local = time.with_timezone(&Local);
        detail = detail.push(iced::widget::tooltip(
            fact(
                "Date",
                local.format("%a %-d %b %Y, %H:%M").to_string(),
                false,
            ),
            container(words(
                format!("UTC{} · device time zone", local.format("%:z")),
                12.0,
                font::REGULAR,
                glyph::muted,
            ))
            .padding([6, 10])
            .style(theme::container::shell_menu),
            iced::widget::tooltip::Position::Bottom,
        ));
    }
    if let Some(fee) = &entry.fee {
        detail = detail.push(fact("Fee", fee_detail(fee, masked), false));
    }
    if let ActivityTarget::Vault(tx) = &entry.target {
        if let Some(height) = tx.height {
            detail = detail.push(fact(
                "Confirmations",
                vault_confirmation_detail(height, cache.blockheight()),
                false,
            ));
        }
    }
    if let Some((key, value)) = &entry.peer {
        detail = detail.push(fact(key, private_fact(key, value, masked), true));
    }

    // `.lblf`: wallets that store labels take an edit here.
    if entry.label_key.is_some() {
        let draft = activity.label_draft_for(&entry.id);
        let stored = activity.stored_label(entry);
        let value = draft.unwrap_or(stored);
        let saving = activity.label_saving_for(&entry.id);
        let mut field = column![
            words("Label", 13.5, font::SEMIBOLD, glyph::ink),
            tenshu::text_field(DETAIL_LABEL_ID, "Label", value, Message::FocusAction)
                .size(FieldSize::Compact)
                .hide_label()
                .placeholder("Add a note for this transaction")
                .on_input_maybe((!saving).then_some(|value: String| {
                    Message::Activity(ActivityMessage::LabelEdited(value))
                }))
                .on_submit(Message::Activity(ActivityMessage::SaveLabel)),
        ]
        .spacing(6);
        type Tone = fn(&theme::Theme) -> iced::Color;
        let note: Option<(&str, Tone)> = if saving {
            Some(("Saving…", glyph::muted))
        } else if let Some(error) = activity.label_error_for(&entry.id) {
            Some((error, glyph::bad))
        } else if draft.is_some_and(|draft| draft != stored) {
            Some(("Press Enter to save", glyph::muted))
        } else {
            None
        };
        if let Some((note, tone)) = note {
            field = field.push(words(note, 12.0, font::REGULAR, tone));
        }
        detail = detail.push(column![
            container(field).padding([14, 0]),
            rule::horizontal(1)
        ]);
    }

    for (index, (name, value)) in entry.identifiers.iter().enumerate() {
        detail = detail.push(identifier_row(index, name, value));
    }

    if let Some((title, facts)) = entry
        .rail_details
        .as_ref()
        .filter(|(_, facts)| !facts.is_empty())
    {
        // `.disc`: a 44px row, the title in 14px semibold and the chevron
        // at its right edge, turned up while open.
        let expanded = activity.details_open_for(&entry.id);
        detail = detail.push(
            Action::new(
                DETAIL_RAIL_ID,
                row![
                    words(*title, 14.0, font::SEMIBOLD, glyph::ink).width(Length::Fill),
                    glyph::glyph(
                        if expanded {
                            glyph::CHEVRON_UP
                        } else {
                            glyph::CHEVRON_DOWN
                        },
                        16.0,
                        glyph::muted
                    ),
                ]
                .spacing(8)
                .height(Length::Fixed(43.0))
                .align_y(Alignment::Center),
                Message::FocusAction,
            )
            .padding(0)
            .width(Length::Fill)
            .style(theme::button::transparent_border)
            .on_press(Message::Activity(ActivityMessage::ToggleDetails(!expanded))),
        );
        detail = detail.push(rule::horizontal(1));
        if expanded {
            for (key, value) in facts {
                detail = detail.push(fact(key, private_fact(key, value, masked), false));
            }
        }
    }

    if let Some(label) = next_step(entry, activity.vault_supports_replacement()) {
        detail = detail.push(Space::new().height(Length::Fixed(16.0))).push(
            Action::new(
                DETAIL_NEXT_STEP_ID,
                text(label).size(14).font(font::SEMIBOLD),
                Message::FocusAction,
            )
            .style(theme::button::primary)
            .padding([8, 16])
            .on_press(Message::Activity(ActivityMessage::OpenDetail(
                entry.target.clone(),
            ))),
        );
    }

    if let (Some(signer_status), ActivityTarget::Psbt(tx)) = (
        entry.signer_status.as_ref().filter(|_| !needs_signatures),
        &entry.target,
    ) {
        let path = tx.sigs.primary_path();
        detail = detail.push(completed_psbt_disclosure(
            &entry.id,
            signer_status,
            path.threshold,
            path.sigs_count,
        ));
    }
    detail
}

/// `.det`: the panel colour, a rule on its left, 20/22/28 insets.
fn detail_surface<'a>(
    content: Column<'a, Message>,
) -> iced::widget::Container<'a, Message, theme::Theme> {
    container(content.width(Length::Fill))
        .padding(iced::Padding {
            top: 20.0,
            right: 22.0,
            bottom: 28.0,
            left: 22.0,
        })
        .width(Length::Fill)
        .style(|theme: &theme::Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(theme.colors.shell.panel)),
            ..Default::default()
        })
}

fn render_at_width<'a>(
    activity: &'a Activity,
    cache: &'a Cache,
    width: f32,
) -> Element<'a, Message> {
    let entries = activity.entries();
    let masked = cache.balance_amounts_masked();
    let cutoff = activity.complete_through();
    let loaded = entries.len();
    let visible: Vec<_> = entries
        .iter()
        .filter(|entry| {
            (entry.timestamp == 0 || cutoff.is_none_or(|cutoff| entry.timestamp > cutoff))
                && activity.filters.includes(entry)
        })
        .collect();
    // A retained selection may be outside the current filter or the complete
    // history window. Only a row in the visible feed may own the detail pane.
    let selected = activity
        .selected_id
        .as_ref()
        .and_then(|id| visible.iter().copied().find(|entry| &entry.id == id));
    let beside = detail_beside_list(width);
    let frame = column_frame(width, beside && selected.is_some());
    let shown = visible.len();
    let loading = activity.vault_loading
        || activity.psbt_loading
        || activity.spark_loading
        || activity.liquid_loading
        || activity.refundables_loading;

    let mut list = Column::new().spacing(0);
    if cutoff.is_some() {
        list = list.push(
            container(words(
                "Older days appear as each wallet's history loads.",
                13.0,
                font::REGULAR,
                glyph::muted,
            ))
            .padding([10, 12]),
        );
    }
    // Liquid's banners only while the Liquid gate shows it.
    let liquid = activity.liquid_shown();
    for (wallet, error) in [
        ("Vault", activity.vault_error.as_deref()),
        ("Vault proposals", activity.psbt_error.as_deref()),
        ("Spark", activity.spark_error.as_deref()),
        (
            "Liquid",
            activity.liquid_error.as_deref().filter(|_| liquid),
        ),
        (
            "Liquid refunds",
            activity.refundables_error.as_deref().filter(|_| liquid),
        ),
    ] {
        if error.is_none() {
            continue;
        }
        // `.hbanner`: the load failure with its Retry.
        list = list.push(
            container(
                container(
                    row![
                        glyph::glyph(glyph::WARNING, 16.0, glyph::warn),
                        words(
                            format!("{wallet} history couldn't load"),
                            14.0,
                            font::MEDIUM,
                            glyph::ink
                        )
                        .width(Length::Fill),
                        Action::new(
                            Id::from(format!("tenshu.activity.retry.{wallet}")),
                            text("Retry").size(13).font(font::MEDIUM),
                            Message::FocusAction,
                        )
                        .style(theme::button::secondary)
                        .padding([6, 12])
                        .on_press(Message::Activity(ActivityMessage::Retry)),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .padding([10, 12])
                .width(Length::Fill)
                .style(|theme: &theme::Theme| iced::widget::container::Style {
                    background: Some(iced::Background::Color(theme.colors.shell.panel)),
                    border: iced::Border {
                        color: theme.colors.shell.warn,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    ..Default::default()
                }),
            )
            .padding(iced::Padding {
                top: 12.0,
                right: 0.0,
                bottom: 4.0,
                left: 0.0,
            }),
        );
    }
    list = list.push(columns());
    if loading {
        list = list.push(column![
            container(words(
                "Loading payment history…",
                14.0,
                font::REGULAR,
                glyph::muted
            ))
            .height(Length::Fixed(ROW_HEIGHT - 1.0))
            .align_y(iced::alignment::Vertical::Center)
            .padding([0, 12]),
            rule::horizontal(1)
        ]);
    }
    if visible.is_empty() && !loading {
        let message = if cutoff.is_some() {
            "Load older activity to complete this date range"
        } else if entries.is_empty() {
            "No transactions yet"
        } else {
            "No activity matches these filters"
        };
        let mut empty = Column::new()
            .spacing(12)
            .align_x(Alignment::Center)
            .push(words(message, 16.0, font::SEMIBOLD, glyph::ink));
        if activity.filters.count() > 0 {
            empty = empty.push(
                Action::new(
                    Id::from("tenshu.activity.empty.clear"),
                    text("Clear filters").size(14).font(font::MEDIUM),
                    Message::FocusAction,
                )
                .style(theme::button::secondary)
                .padding([8, 16])
                .on_press(Message::Activity(ActivityMessage::Clear)),
            );
        }
        list = list.push(
            container(empty)
                .width(Length::Fill)
                .padding([64, 12])
                .align_x(iced::alignment::Horizontal::Center),
        );
    }
    let mut selected_detail = selected.map(|entry| detail_pane(activity, entry, cache));
    let today = Local::now().date_naive();
    let mut previous_day = None;
    let mut first_day = true;
    for entry in visible {
        let is_selected = selected.is_some_and(|selected| selected.id == entry.id);
        let day = (entry.timestamp != 0)
            .then(|| DateTime::<Utc>::from_timestamp(entry.timestamp as i64, 0))
            .flatten()
            .map(|time| time.with_timezone(&Local).date_naive());
        if first_day || day != previous_day {
            let (relative, date) = day_heading(day, today);
            list = list.push(column![
                container(
                    row![
                        words(relative, 13.0, font::SEMIBOLD, glyph::ink),
                        words(date, 13.0, font::MEDIUM, glyph::muted)
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center)
                )
                .padding(iced::Padding {
                    top: 20.0,
                    right: 12.0,
                    bottom: 8.0,
                    left: 12.0,
                }),
                rule::horizontal(1)
            ]);
            previous_day = day;
            first_day = false;
        }
        list = list.push(activity_row(entry, is_selected, masked));
        if is_selected && !beside {
            // Keep compact detail in the selected row's scroll position and
            // immediately after it in keyboard traversal, even deep in history.
            if let Some(detail) = selected_detail.take() {
                list = list.push(column![detail_surface(detail), rule::horizontal(1)]);
            }
        }
    }
    if activity.vault_has_more || activity.spark_has_more || activity.liquid_has_more {
        let mut more = Action::new(
            Id::from("tenshu.activity.more"),
            text("Load older activity").size(14).font(font::MEDIUM),
            Message::FocusAction,
        )
        .style(theme::button::secondary)
        .padding([8, 16]);
        if !loading {
            more = more.on_press(Message::Activity(ActivityMessage::LoadMore));
        }
        list = list.push(container(more).padding([16, 12]));
    }
    // `.lst`: row text on the page column and 24px below the last row. The
    // list ends where the detail pane starts, so its right inset loses it.
    let pane = if beside && selected_detail.is_some() {
        DETAIL_SPAN
    } else {
        0.0
    };
    let list = scrollable(container(list).padding(iced::Padding {
        top: 0.0,
        right: (frame.right - pane - ROW_INSET).max(0.0),
        bottom: 24.0,
        left: frame.left - ROW_INSET,
    }))
    .id(Id::from(ACTIVITY_LIST_SCROLL_ID))
    .width(Length::Fill)
    .height(Length::Fill);
    // Keep the list's widget tag stable when selection adds the pane;
    // otherwise Iced discards the list's current scroll offset.
    let mut split = Row::new().push(list).height(Length::Fill);
    if let Some(detail) = selected_detail.filter(|_| beside) {
        split = split.push(rule::vertical(1)).push(
            container(
                scrollable(detail_surface(detail))
                    .id(Id::from(ACTIVITY_DETAIL_SCROLL_ID))
                    .height(Length::Fill),
            )
            .width(Length::Fixed(SELECTED_DETAIL_WIDTH))
            .height(Length::Fill)
            .style(|theme: &theme::Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(theme.colors.shell.panel)),
                ..Default::default()
            }),
        );
    }
    column![
        header(activity, cache, shown, loaded, frame),
        filters_bar(activity, frame),
        split
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::wallets::DomainLiquidAssetInfo;
    use crate::app::{
        state::{
            preferences::{BalancePrivacy, BalancePrivacyChrome},
            State,
        },
        view::shared::landing::capture,
    };
    use crate::chain::ChainId;

    /// Activity fills the shell's content area below the top bar and its
    /// 1px rule.
    fn mount<M: 'static>(
        scenario: &str,
        content: Element<'_, M>,
        outer: (u32, u32),
        appearance: &str,
        theme: coincube_ui::theme::Theme,
        width: u32,
    ) -> capture::Inspection {
        capture::inspect_filled(
            scenario,
            content,
            outer,
            appearance,
            theme,
            (
                width,
                outer.1 - crate::app::view::nav::shell::TOP_BAR_HEIGHT as u32 - 1,
            ),
        )
    }

    fn spark_payment() -> PaymentSummary {
        PaymentSummary {
            id: "pay-42".into(),
            amount_sat: -4200,
            fees_sat: 2,
            token_amount: None,
            token_decimals: None,
            token_ticker: None,
            timestamp: 1_760_000_000,
            status: "failed".into(),
            direction: "send".into(),
            method: "lightning".into(),
            description: Some("Coffee".into()),
            details: Default::default(),
        }
    }

    #[test]
    fn spark_identity_and_failure_filter_keep_source_provenance() {
        let row = Entry::spark(spark_payment());
        assert_eq!(row.id, "spark:lightning:pay-42");
        assert_eq!(row.identifiers, vec![("Payment ID", "pay-42".to_owned())]);
        assert_eq!(row.status, StatusFilter::Failed);
        let filters = Filters {
            wallet: WalletFilter::Spark,
            status: StatusFilter::Failed,
            search: "coffee".into(),
            ..Default::default()
        };
        assert!(filters.includes(&row));
        assert!(!(Filters {
            status: StatusFilter::Confirmed,
            ..filters.clone()
        })
        .includes(&row));
        assert!(!(Filters {
            search: "other".into(),
            ..filters
        })
        .includes(&row));
    }

    #[test]
    fn date_filter_requires_complete_iso_dates() {
        let row = Entry::spark(spark_payment());
        let day = DateTime::<Utc>::from_timestamp(row.timestamp as i64, 0)
            .unwrap()
            .with_timezone(&Local)
            .date_naive();
        let mut filters = Filters {
            date_range: format!("{day} to {day}"),
            ..Default::default()
        };
        assert!(filters.includes(&row));
        filters.date_range = "2026-13-99 to 2026-12-31".into();
        assert!(filters.dates().is_err());
        assert!(!filters.includes(&row));
        filters.date_range = "2026-12-31 to 2026-01-01".into();
        assert!(filters.dates().is_err());
    }

    #[test]
    fn asset_amounts_use_source_precision() {
        let mut spark = spark_payment();
        spark.token_amount = Some(1_580_000);
        spark.token_decimals = Some(6);
        spark.token_ticker = Some("USDB".into());
        let spark = Entry::spark(spark);
        assert_eq!(
            (spark.amount.as_str(), spark.unit.as_str()),
            ("1.58", "USDB")
        );
        assert_eq!(spark.fee, None, "the bridge reports no token fees");

        let liquid = DomainPayment {
            tx_id: Some("liquid-tx".into()),
            destination: None,
            timestamp: 1_760_000_000,
            amount_sat: 0,
            fees_sat: 0,
            direction: DomainPaymentDirection::Receive,
            status: DomainPaymentStatus::Complete,
            details: DomainPaymentDetails::LiquidAsset {
                asset_id: USDT_ASSET_ID_MAINNET.into(),
                asset_info: Some(DomainLiquidAssetInfo {
                    amount_minor: 500_000_000,
                    precision: 8,
                }),
                description: String::new(),
                payer_note: None,
            },
        };
        let liquid = Entry::liquid(liquid);
        assert_eq!(
            (liquid.amount.as_str(), liquid.unit.as_str()),
            ("5.00", "USDt")
        );
        assert_eq!(liquid.fee.as_deref(), Some("Paid by sender"));
    }

    #[test]
    fn liquid_asset_without_metadata_never_invents_sats() {
        let mut payment = DomainPayment {
            tx_id: Some("liquid-tx".into()),
            destination: None,
            timestamp: 1_760_000_000,
            amount_sat: 500_000_000,
            fees_sat: 0,
            direction: DomainPaymentDirection::Receive,
            status: DomainPaymentStatus::Complete,
            details: DomainPaymentDetails::LiquidAsset {
                asset_id: USDT_ASSET_ID_MAINNET.into(),
                asset_info: None,
                description: String::new(),
                payer_note: None,
            },
        };
        assert_eq!(Entry::liquid(payment.clone()).amount, "5.00");
        if let DomainPaymentDetails::LiquidAsset { asset_id, .. } = &mut payment.details {
            *asset_id = "unknown-asset-id".into();
        }
        let unknown = Entry::liquid(payment);
        assert_eq!(
            (unknown.amount.as_str(), unknown.unit.as_str()),
            ("500,000,000", "base units · unknown-asset-id")
        );
    }

    #[test]
    fn spark_rail_preserves_receive_and_send_direction() {
        let mut payment = spark_payment();
        payment.method = "spark".into();
        payment.direction = "Receive".into();
        let received = Entry::spark(payment.clone());
        assert_eq!(received.direction, DirectionFilter::Received);
        assert!(Filters {
            direction: DirectionFilter::Received,
            ..Default::default()
        }
        .includes(&received));
        payment.direction = "Send".into();
        let sent = Entry::spark(payment);
        assert_eq!(sent.direction, DirectionFilter::Sent);
        assert!(!Filters {
            direction: DirectionFilter::Transfers,
            ..Default::default()
        }
        .includes(&sent));
    }

    #[test]
    fn fee_detail_respects_cube_amount_privacy() {
        assert_eq!(fee_detail("123 sats", true), "••••••");
        assert_eq!(fee_detail("123 sats", false), "123 sats");
    }

    #[test]
    fn activity_amount_signs_never_expose_masked_value() {
        let mut payment = spark_payment();
        payment.direction = "receive".into();
        let received = Entry::spark(payment.clone());
        assert_eq!(signed_amount(&received, false), "+4,200");
        assert_eq!(signed_amount(&received, true), "••••••");

        payment.direction = "send".into();
        let sent = Entry::spark(payment);
        assert_eq!(signed_amount(&sent, false), "−4,200");
        assert_eq!(signed_amount(&sent, true), "••••••");
    }

    #[test]
    fn detail_abbreviates_long_identifier_for_display() {
        let full = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(short_identifier(full), "0123456789abcdef…89abcdef");
        assert_eq!(short_identifier("pay-42"), "pay-42");
    }

    #[test]
    fn vault_confirmations_require_a_known_chain_tip() {
        assert_eq!(vault_confirmation_detail(800_000, 0), "Unavailable");
        assert_eq!(vault_confirmation_detail(800_000, 799_999), "Unavailable");
        assert_eq!(vault_confirmation_detail(800_000, 800_000), "1");
        assert_eq!(vault_confirmation_detail(800_000, 800_004), "5");
    }

    #[test]
    fn selected_detail_stacks_until_list_and_pane_fit() {
        // Activity fills the shell's content area edge to edge (`.main`).
        let activity_content =
            |viewport: f32| viewport - super::super::nav::split::sidebar_width_for_window(viewport);
        assert!(!detail_beside_list(activity_content(1024.0)));
        assert!(detail_beside_list(activity_content(1440.0)));
        // The canvas's 1440px frame: a 248px rail, the list and the 420px pane.
        assert_eq!(activity_content(1440.0), 1192.0);
        // Rows, their gutter insets and the pane with its rule fit at 1440
        // (1151.4px needed of 1192), not at a 1100px pane (1145.9px needed).
        let needed = |pane: f32| MIN_ACTIVITY_ROWS_WIDTH + 2.0 * list_inset(pane) + DETAIL_SPAN;
        assert!(needed(1192.0) <= 1192.0);
        assert!(!detail_beside_list(1100.0));
        assert!(needed(1100.0) > 1100.0);
    }

    #[test]
    fn activity_sits_on_the_same_column_as_every_page() {
        for pane in [816.0, 952.0, 1192.0, 2312.0] {
            let (left, column) =
                tenshu::content_frame(pane, PageWidth::Reading, super::super::page_gutter(pane));
            assert_eq!(
                column_frame(pane, false),
                Frame {
                    left,
                    column,
                    right: pane - left - column
                }
            );
        }
    }

    #[test]
    fn the_detail_pane_moves_the_column_only_when_it_would_cover_it() {
        // A 2560px window: the 421px pane fits in the right margin.
        assert_eq!(column_frame(2312.0, true), column_frame(2312.0, false));
        // 1440px: the column centres in what the pane leaves.
        let beside = column_frame(1192.0, true);
        assert!(beside.left < column_frame(1192.0, false).left);
        assert_eq!(beside.left + beside.column + beside.right, 1192.0);
        assert!(beside.right >= DETAIL_SPAN);
    }

    #[test]
    fn the_column_never_jumps_as_a_window_with_the_detail_pane_is_resized() {
        // 1897px once moved the column about 211px left of where 1898px put it.
        let mut previous = column_frame(1000.0, true);
        for width in 1001..=2600 {
            let width = width as f32;
            let frame = column_frame(width, true);
            assert!(
                (frame.left - previous.left).abs() <= 1.0
                    && (frame.column - previous.column).abs() <= 1.0,
                "{:?} -> {:?} at {}",
                previous,
                frame,
                width
            );
            assert!(
                frame.right >= DETAIL_SPAN + super::super::page_gutter(width) - 0.01,
                "the pane covers the column at {}",
                width
            );
            previous = frame;
        }
        // From the first width the pane sits beside, its rows get their minimum.
        let first = (1000..=2600)
            .map(|width| width as f32)
            .find(|width| detail_beside_list(*width))
            .unwrap();
        assert!(column_frame(first, true).column + 2.0 * ROW_INSET >= MIN_ACTIVITY_ROWS_WIDTH);
    }

    #[test]
    fn amounts_group_thousands_like_the_canvas() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(4_200), "4,200");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(sats(2), "2 SATS");
    }

    #[test]
    fn activity_dates_do_not_pad_single_digit_days() {
        let today = NaiveDate::from_ymd_opt(2025, 10, 9).unwrap();
        let yesterday = today.pred_opt().unwrap();
        let heading = |days: u64| {
            let (relative, date) = day_heading(Some(today - chrono::Days::new(days)), today);
            format!("{relative} | {date}")
        };
        assert_eq!(heading(0), "Today | Thu 9 Oct 2025");
        assert_eq!(heading(1), "Yesterday | Wed 8 Oct 2025");
        assert_eq!(heading(3), "Monday | Mon 6 Oct 2025");
        assert_eq!(heading(8), "Last week | Wed 1 Oct 2025");
        assert_eq!(heading(14), "Earlier | Thu 25 Sep 2025");
        assert_eq!(day_heading(Some(yesterday), today).0, "Yesterday");
        assert_eq!(day_heading(None, today).0, "Pending");
    }

    #[test]
    fn activity_rows_mount_at_compact_and_wide_widths_in_both_themes() {
        // Both chains share Bitcoin address encoding, so Activity must use the Cube chain.
        let cache = Cache {
            fiat_chain: ChainId::BitcoinBlake2b,
            balance_privacy: Some(BalancePrivacyChrome {
                privacy: BalancePrivacy::Shown,
                busy: false,
                interactive: false,
            }),
            ..Default::default()
        };
        let mut activity = Activity::new(None, None);
        let payment = spark_payment();
        let id = Entry::spark(payment.clone()).id;
        drop(activity.update(
            None,
            &cache,
            crate::app::message::Message::ActivitySparkLoaded(0, Ok((vec![payment], false))),
        ));
        for selected in [false, true] {
            activity.selected_id = selected.then(|| id.clone());
            for (appearance, theme) in [
                ("dark", coincube_ui::theme::Theme::dark()),
                ("light", coincube_ui::theme::Theme::light()),
            ] {
                for (outer, width) in [((1024, 768), 768), ((1440, 900), 1192)] {
                    let mounted = mount(
                        if selected {
                            "r09-activity-selected"
                        } else {
                            "r09-activity-list"
                        },
                        render_at_width(&activity, &cache, width as f32),
                        outer,
                        appearance,
                        theme,
                        width,
                    );
                    assert!(mounted.contains("Coffee"));
                    assert!(mounted.contains("Failed"));
                    assert!(mounted.contains("Wallet"));
                    assert!(mounted.contains("Direction"));
                    assert!(mounted.contains("Status"));
                    assert!(mounted.contains("Date range"));
                    assert!(mounted.contains("Search label or id"));
                    assert!(mounted.contains("−4,200"));
                    assert!(mounted.contains("SATS"));
                    assert!(mounted.contains("Cube · Bitcoin Blake2b mainnet"));
                    assert!(mounted.has_focus(row_focus_id(&id)));
                    assert_eq!(mounted.has_focus(Id::from(DETAIL_CLOSE_ID)), selected);
                    assert_eq!(
                        mounted.contains("Historical fiat amount unavailable"),
                        selected
                    );
                    // The canvas's pane: Date, Fee, the identifier, no hand-off.
                    assert_eq!(mounted.contains("2 SATS"), selected);
                    assert_eq!(mounted.contains("Payment ID"), selected);
                    assert!(!mounted.contains("Open wallet details and actions"));
                }
            }
        }
    }

    #[test]
    fn compact_detail_follows_a_deep_selected_row_in_focus_order() {
        let cache = Cache::default();
        let mut activity = Activity::new(None, None);
        let payments: Vec<_> = (0..30)
            .map(|index| {
                let mut payment = spark_payment();
                payment.id = format!("pay-{index}");
                payment.description = Some(format!("Payment {index}"));
                payment.timestamp -= index * 60;
                payment
            })
            .collect();
        let selected_id = Entry::spark(payments[15].clone()).id;
        let next_id = Entry::spark(payments[16].clone()).id;
        drop(activity.update(
            None,
            &cache,
            crate::app::message::Message::ActivitySparkLoaded(0, Ok((payments, false))),
        ));
        activity.selected_id = Some(selected_id.clone());
        let mounted = mount(
            "r09-activity-deep-compact-detail",
            render_at_width(&activity, &cache, 768.0),
            (1024, 768),
            "dark",
            coincube_ui::theme::Theme::dark(),
            768,
        );
        let position = |id: Id| mounted.focus.iter().position(|(actual, _)| *actual == id);
        let selected = position(row_focus_id(&selected_id)).expect("selected row mounted");
        let close = position(Id::from(DETAIL_CLOSE_ID)).expect("detail mounted");
        let next = position(row_focus_id(&next_id)).expect("next row mounted");
        assert!(selected < close && close < next);
    }

    #[test]
    fn wide_deep_selection_keeps_detail_visible_while_list_scrolls() {
        use iced::advanced::{
            layout,
            renderer::Headless,
            widget::{operation, operation::Scrollable, Operation, Tree},
            Layout,
        };
        use iced::{Rectangle, Size, Vector};

        #[derive(Default)]
        struct Probe {
            panes: Vec<(Id, Rectangle, Rectangle, Vector)>,
            rows: Vec<(Id, Rectangle)>,
        }
        impl Operation for Probe {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn scrollable(
                &mut self,
                id: Option<&Id>,
                bounds: Rectangle,
                content: Rectangle,
                translation: Vector,
                _state: &mut dyn Scrollable,
            ) {
                if let Some(id) = id {
                    self.panes.push((id.clone(), bounds, content, translation));
                }
            }
            fn focusable(
                &mut self,
                id: Option<&Id>,
                bounds: Rectangle,
                _state: &mut dyn operation::Focusable,
            ) {
                if let Some(id) = id {
                    self.rows.push((id.clone(), bounds));
                }
            }
        }

        let cache = Cache::default();
        let mut activity = Activity::new(None, None);
        let payments: Vec<_> = (0..30)
            .map(|index| {
                let mut payment = spark_payment();
                payment.id = format!("wide-pay-{index}");
                payment.description = Some(format!("Payment {index}"));
                payment.timestamp -= index * 60;
                payment
            })
            .collect();
        let selected_id = Entry::spark(payments[20].clone()).id;
        drop(activity.update(
            None,
            &cache,
            crate::app::message::Message::ActivitySparkLoaded(0, Ok((payments, false))),
        ));
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut element: Element<'_, Message> = render_at_width(&activity, &cache, 1192.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(1192.0, 800.0)),
        );
        let inspect = |element: &mut Element<'_, Message>, tree: &mut Tree| {
            let mut probe = Probe::default();
            element
                .as_widget_mut()
                .operate(tree, Layout::new(&node), &renderer, &mut probe);
            probe
        };
        let before = inspect(&mut element, &mut tree);
        fn pane<'p>(probe: &'p Probe, id: &'static str) -> &'p (Id, Rectangle, Rectangle, Vector) {
            probe
                .panes
                .iter()
                .find(|(actual, _, _, _)| *actual == Id::from(id))
                .expect("independent Activity pane")
        }
        let (_, list_bounds, list_content, _) = pane(&before, ACTIVITY_LIST_SCROLL_ID);
        assert!(list_content.height > list_bounds.height + 500.0);
        let row_bounds = before
            .rows
            .iter()
            .find(|(id, _)| *id == row_focus_id(&selected_id))
            .expect("deep selected row")
            .1;
        assert!(row_bounds.y > list_bounds.y + list_bounds.height);
        let offset = row_bounds.y - list_bounds.y - 100.0;
        let mut scroll = operation::scrollable::scroll_to::<()>(
            Id::from(ACTIVITY_LIST_SCROLL_ID),
            operation::scrollable::AbsoluteOffset {
                x: None,
                y: Some(offset),
            },
        );
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut scroll);
        let scrolled = inspect(&mut element, &mut tree);
        let (_, _, _, list_scrolled) = pane(&scrolled, ACTIVITY_LIST_SCROLL_ID);
        assert!(list_scrolled.y > 500.0);
        drop(element);

        // Match the real no-selection → selection rerender. The list scroll
        // state must survive Iced's tree diff when the detail pane appears.
        activity.selected_id = Some(selected_id.clone());
        let mut selected_element: Element<'_, Message> = render_at_width(&activity, &cache, 1192.0);
        tree.diff(selected_element.as_widget());
        let selected_node = selected_element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(1192.0, 800.0)),
        );
        let mut after = Probe::default();
        selected_element.as_widget_mut().operate(
            &mut tree,
            Layout::new(&selected_node),
            &renderer,
            &mut after,
        );
        let (_, selected_list_bounds, _, list_after) = pane(&after, ACTIVITY_LIST_SCROLL_ID);
        let (_, detail_bounds, _, detail_after) = pane(&after, ACTIVITY_DETAIL_SCROLL_ID);
        assert!((list_after.y - list_scrolled.y).abs() < 1.0);
        assert_eq!(detail_after.y, 0.0);
        assert!((selected_list_bounds.y - detail_bounds.y).abs() < 1.0);
        assert!(detail_bounds.y + detail_bounds.height <= 800.0);
        let selected_row = after
            .rows
            .iter()
            .find(|(id, _)| *id == row_focus_id(&selected_id))
            .expect("selected row after tree diff")
            .1;
        let selected_visible_y = selected_row.y - list_after.y;
        assert!(selected_visible_y >= selected_list_bounds.y);
        assert!(selected_visible_y < selected_list_bounds.y + selected_list_bounds.height);
        for (appearance, theme) in [
            ("dark", coincube_ui::theme::Theme::dark()),
            ("light", coincube_ui::theme::Theme::light()),
        ] {
            let mounted = mount(
                "r09-activity-deep-wide-detail",
                render_at_width(&activity, &cache, 1192.0),
                (1440, 900),
                appearance,
                theme,
                1192,
            );
            assert!(mounted.contains("Payment 20"));
            assert!(mounted.has_focus(Id::from(DETAIL_CLOSE_ID)));
        }
    }

    #[test]
    #[allow(deprecated)]
    fn completed_psbt_disclosure_is_keyboard_focusable_and_resets_per_selection() {
        let mut disclosure = PsbtDisclosure {
            entry_id: "psbt-a".into(),
            status: "Ready for review · 2 of 2 signatures".into(),
            required: 2,
            collected: 2,
        };
        let mut state = None;
        let inspect = |disclosure: &PsbtDisclosure, state: &Option<(String, bool)>| {
            mount(
                "r09-activity-completed-psbt-disclosure",
                disclosure.view(state),
                (1024, 768),
                "dark",
                coincube_ui::theme::Theme::dark(),
                768,
            )
        };
        let collapsed = inspect(&disclosure, &state);
        assert!(collapsed.has_focus(Id::from("tenshu.activity.detail.psbt.disclosure")));
        assert!(collapsed.contains("PSBT and signers"));
        assert!(!collapsed.contains("Required"));
        assert!(!collapsed.contains("Continue signing"));
        assert!(disclosure
            .update(&mut state, PsbtDisclosureEvent::Toggle)
            .is_none());
        let expanded = inspect(&disclosure, &state);
        assert!(expanded.contains("Required"));
        assert!(expanded.contains("Collected"));
        disclosure.entry_id = "psbt-b".into();
        assert!(!inspect(&disclosure, &state).contains("Required"));
    }

    #[test]
    fn detail_actions_follow_the_visible_row_across_filter_changes() {
        let cache = Cache::default();
        let mut activity = Activity::new(None, None);
        let payment = spark_payment();
        let id = Entry::spark(payment.clone()).id;
        drop(activity.update(
            None,
            &cache,
            crate::app::message::Message::ActivitySparkLoaded(0, Ok((vec![payment], false))),
        ));
        let apply = |activity: &mut Activity, event| {
            drop(activity.update(
                None,
                &cache,
                crate::app::message::Message::View(Message::Activity(event)),
            ));
        };
        apply(&mut activity, ActivityMessage::Select(id.clone()));
        for (exclude, restore) in [
            (
                ActivityMessage::Wallet(WalletFilter::Vault),
                ActivityMessage::Wallet(WalletFilter::All),
            ),
            (
                ActivityMessage::Direction(DirectionFilter::Received),
                ActivityMessage::Direction(DirectionFilter::All),
            ),
            (
                ActivityMessage::Status(StatusFilter::Confirmed),
                ActivityMessage::Status(StatusFilter::All),
            ),
            (
                ActivityMessage::DateRange("2026-01-01 to 2026-12-31".into()),
                ActivityMessage::DateRange(String::new()),
            ),
            (
                ActivityMessage::Search("another payment".into()),
                ActivityMessage::Search(String::new()),
            ),
        ] {
            apply(&mut activity, exclude);
            let filtered = mount(
                "r09-activity-filtered-detail",
                render_at_width(&activity, &cache, 1192.0),
                (1440, 900),
                "dark",
                coincube_ui::theme::Theme::dark(),
                1192,
            );
            assert!(!filtered.has_focus(row_focus_id(&id)));
            assert!(!filtered.has_focus(Id::from(DETAIL_CLOSE_ID)));
            assert_eq!(activity.selected_id.as_deref(), Some(id.as_str()));

            apply(&mut activity, restore);
            let restored = mount(
                "r09-activity-restored-detail",
                render_at_width(&activity, &cache, 1192.0),
                (1440, 900),
                "dark",
                coincube_ui::theme::Theme::dark(),
                1192,
            );
            assert!(restored.has_focus(row_focus_id(&id)));
            assert!(restored.has_focus(Id::from(DETAIL_CLOSE_ID)));
        }
    }

    fn lightning_send() -> PaymentSummary {
        PaymentSummary {
            status: "completed".into(),
            description: None,
            details: coincube_spark_protocol::PaymentDetailSummary {
                payment_hash: Some("4c8dsyntheticpaymenthash77b0".into()),
                invoice: Some("lnbc42u1psyntheticinvoiceforactivitytests".into()),
                lightning_address: Some("shop@example.test".into()),
                tx_id: None,
                conversion: None,
            },
            ..spark_payment()
        }
    }

    #[test]
    fn spark_lightning_send_shows_the_canvas_fields() {
        let entry = Entry::spark(lightning_send());
        assert_eq!(entry.title, "Lightning address · shop@example.test");
        assert_eq!(entry.rail, "Spark → Lightning");
        assert_eq!(entry.status_note, Some("payment settled"));
        assert_eq!(entry.fee.as_deref(), Some("2 SATS"));
        assert_eq!(
            entry.peer,
            Some(("Destination", "shop@example.test".to_owned()))
        );
        assert_eq!(
            entry.identifiers,
            vec![("Payment hash", "4c8dsyntheticpaymenthash77b0".to_owned())]
        );
        let (title, facts) = entry.rail_details.clone().expect("Lightning details");
        assert_eq!(title, "Lightning details");
        assert!(facts.iter().any(|(key, _)| *key == "Invoice"));
        assert!(facts.contains(&("Payment ID", "pay-42".to_owned())));
        // The hash is searchable, as the filter's placeholder promises.
        assert!(Filters {
            search: "paymenthash77".into(),
            ..Default::default()
        }
        .includes(&entry));

        let cache = Cache::default();
        let mut activity = Activity::new(None, None);
        drop(activity.update(
            None,
            &cache,
            crate::app::message::Message::ActivitySparkLoaded(
                0,
                Ok((vec![lightning_send()], false)),
            ),
        ));
        activity.selected_id = Some(entry.id.clone());
        let mounted = mount(
            "r09-activity-lightning-detail",
            render_at_width(&activity, &cache, 1192.0),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
            1192,
        );
        for expected in [
            "Confirmed · payment settled",
            "Spark · Spark → Lightning",
            "Destination",
            "Payment hash",
            "Lightning details",
        ] {
            assert!(mounted.contains(expected), "{}", expected);
        }
        // Collapsed until asked: the disclosure, not its facts.
        assert!(!mounted.contains("Invoice"));
        assert!(!mounted.contains("Open wallet details and actions"));
        // No label store for Spark payments, so no Label field.
        assert!(!mounted.has_focus(Id::from(DETAIL_LABEL_ID)));

        drop(activity.update(
            None,
            &cache,
            crate::app::message::Message::View(Message::Activity(ActivityMessage::ToggleDetails(
                true,
            ))),
        ));
        let opened = mount(
            "r09-activity-lightning-detail-open",
            render_at_width(&activity, &cache, 1192.0),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
            1192,
        );
        assert!(opened.contains("Invoice"));
        assert!(opened.contains("Payment ID"));
        // Another selection starts with its details closed.
        drop(activity.update(
            None,
            &cache,
            crate::app::message::Message::View(Message::Activity(ActivityMessage::Select(
                "another".into(),
            ))),
        ));
        assert!(!activity.details_open_for(&entry.id));
    }

    fn outgoing_vault_payment() -> HistoryTransaction {
        use crate::daemon::model::Coin;
        use coincube_core::miniscript::bitcoin::{
            absolute, bip32, hashes::Hash, transaction, Address, Amount, Network, OutPoint,
            ScriptBuf, Transaction, TxIn, TxOut, Txid, WScriptHash,
        };
        let script = ScriptBuf::new_p2wsh(&WScriptHash::all_zeros());
        let previous_output = OutPoint {
            txid: Txid::all_zeros(),
            vout: 1,
        };
        let coin = Coin {
            amount: Amount::from_sat(5_000_000),
            outpoint: previous_output,
            address: Address::from_script(&script, Network::Regtest).unwrap(),
            block_height: Some(1),
            derivation_index: bip32::ChildNumber::from(0),
            spend_info: None,
            is_immature: false,
            is_change: false,
            is_from_self: false,
        };
        // Output 0 is change back to the Vault, output 1 pays the recipient.
        let tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output,
                ..Default::default()
            }],
            output: vec![
                TxOut {
                    value: Amount::from_sat(1_000_000),
                    script_pubkey: script,
                },
                TxOut {
                    value: Amount::from_sat(3_999_000),
                    script_pubkey: ScriptBuf::new_p2wsh(&WScriptHash::from_byte_array([1; 32])),
                },
            ],
        };
        HistoryTransaction::new(
            tx,
            Some(103),
            Some(1_760_000_000),
            vec![coin],
            vec![0],
            Network::Regtest,
        )
    }

    #[test]
    fn outgoing_vault_payment_names_its_destination_and_label_key() {
        use coincube_core::miniscript::bitcoin::{
            hashes::Hash, Address, Network, ScriptBuf, WScriptHash,
        };
        let tx = outgoing_vault_payment();
        let txid = tx.txid;
        let entry = Entry::vault(tx);
        assert_eq!(entry.direction, DirectionFilter::Sent);
        let recipient = Address::from_script(
            &ScriptBuf::new_p2wsh(&WScriptHash::from_byte_array([1; 32])),
            Network::Regtest,
        )
        .unwrap();
        assert_eq!(entry.peer, Some(("Destination", recipient.to_string())));
        assert_eq!(entry.identifiers, vec![("Txid", txid.to_string())]);
        assert_eq!(entry.fee.as_deref(), Some("1,000 SATS"));
        // The title reads the payment's outpoint label first, so it is stored there.
        assert_eq!(
            entry.label_key.map(|key| key.to_string()),
            Some(format!("{txid}:1"))
        );
        let (_, facts) = entry.rail_details.expect("On-chain details");
        assert!(facts.contains(&("Block", "103".to_owned())));
    }

    /// Every message a task emits, fed back through the panel.
    fn settle(activity: &mut Activity, cache: &Cache, task: iced::Task<AppMessage>) {
        use iced::futures::StreamExt;
        let Some(stream) = iced_runtime::task::into_stream(task) else {
            return;
        };
        let messages: Vec<_> = iced::futures::executor::block_on(
            stream
                .filter_map(|action| async move {
                    match action {
                        iced_runtime::Action::Output(message) => Some(message),
                        _ => None,
                    }
                })
                .collect(),
        );
        for message in messages {
            drop(activity.update(None, cache, message));
        }
    }

    type AppMessage = crate::app::message::Message;

    fn edit(id: &str, label: &str) -> [AppMessage; 2] {
        [
            AppMessage::View(Message::Activity(ActivityMessage::Select(id.into()))),
            AppMessage::View(Message::Activity(ActivityMessage::LabelEdited(
                label.into(),
            ))),
        ]
    }

    fn label_daemon(
        key: &str,
        label: &str,
    ) -> std::sync::Arc<dyn crate::daemon::Daemon + Sync + Send> {
        use crate::utils::mock::Daemon as MockDaemon;
        std::sync::Arc::new(crate::daemon::client::Coincubed::new(
            MockDaemon::new(vec![(
                Some(serde_json::json!({
                    "method": "updatelabels",
                    "params": [{ key: label }],
                })),
                Ok(serde_json::json!({})),
            )])
            .run(),
        ))
    }

    #[test]
    fn saving_a_label_writes_the_key_the_title_reads_and_retitles_the_row() {
        let cache = Cache::default();
        let mut activity = Activity::new(None, None);
        let tx = outgoing_vault_payment();
        let key = format!("{}:1", tx.txid);
        let id = Entry::vault(tx.clone()).id;
        drop(activity.update(
            None,
            &cache,
            AppMessage::ActivityVaultLoaded(0, Ok((vec![tx], false))),
        ));
        for message in edit(&id, "Roof repair deposit") {
            drop(activity.update(None, &cache, message));
        }
        let mounted = mount(
            "r09-activity-label-draft",
            render_at_width(&activity, &cache, 1192.0),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
            1192,
        );
        assert!(mounted.has_focus(Id::from(DETAIL_LABEL_ID)));
        assert!(mounted.contains("Press Enter to save"));

        let task = activity.update(
            Some(label_daemon(&key, "Roof repair deposit")),
            &cache,
            AppMessage::View(Message::Activity(ActivityMessage::SaveLabel)),
        );
        assert!(activity.label_saving_for(&id));
        settle(&mut activity, &cache, task);
        assert!(!activity.label_saving_for(&id));
        assert_eq!(activity.label_draft_for(&id), None);
        let entry = activity
            .entries()
            .into_iter()
            .find(|entry| entry.id == id)
            .unwrap();
        assert_eq!(entry.title, "Roof repair deposit");
        assert_eq!(activity.stored_label(&entry), "Roof repair deposit");
    }

    #[test]
    fn a_transaction_label_is_edited_where_the_title_reads_it() {
        let mut tx = outgoing_vault_payment();
        let txid = tx.txid.to_string();
        tx.labels.insert(txid.clone(), "Rent".into());
        let entry = Entry::vault(tx.clone());
        assert_eq!(entry.title, "Rent");
        assert_eq!(
            entry.label_key.as_ref().map(ToString::to_string),
            Some(txid)
        );
        let cache = Cache::default();
        let mut activity = Activity::new(None, None);
        drop(activity.update(
            None,
            &cache,
            AppMessage::ActivityVaultLoaded(0, Ok((vec![tx], false))),
        ));
        assert_eq!(activity.stored_label(&entry), "Rent");
    }

    #[test]
    fn a_label_save_never_strands_the_field() {
        let cache = Cache::default();
        let mut activity = Activity::new(None, None);
        let tx = outgoing_vault_payment();
        let id = Entry::vault(tx.clone()).id;
        let load = |activity: &mut Activity, tx: HistoryTransaction| {
            drop(activity.update(
                None,
                &cache,
                AppMessage::ActivityVaultLoaded(
                    activity_generation(activity),
                    Ok((vec![tx], false)),
                ),
            ))
        };
        load(&mut activity, tx.clone());

        // Too long for the daemon: refused here, with the limit.
        for message in edit(&id, &"x".repeat(101)) {
            drop(activity.update(None, &cache, message));
        }
        drop(activity.update(
            None,
            &cache,
            AppMessage::View(Message::Activity(ActivityMessage::SaveLabel)),
        ));
        assert_eq!(
            activity.label_error_for(&id),
            Some("Labels are limited to 100 characters.")
        );
        assert!(!activity.label_saving_for(&id));

        // No daemon: said so, not silently dropped.
        drop(activity.update(
            None,
            &cache,
            AppMessage::View(Message::Activity(ActivityMessage::LabelEdited(
                "Rent".into(),
            ))),
        ));
        assert_eq!(activity.label_error_for(&id), None);
        drop(activity.update(
            None,
            &cache,
            AppMessage::View(Message::Activity(ActivityMessage::SaveLabel)),
        ));
        assert!(activity.label_error_for(&id).is_some());

        // A reload while a save is out (its answer went to another panel)
        // leaves the field editable, and a late failure is not shown.
        let key = format!("{}:1", tx.txid);
        drop(activity.update(
            Some(label_daemon(&key, "Rent")),
            &cache,
            AppMessage::View(Message::Activity(ActivityMessage::SaveLabel)),
        ));
        assert!(activity.label_saving_for(&id));
        drop(activity.reload(None, None));
        assert!(!activity.label_saving_for(&id));
        // Retry keeps unsaved text.
        assert_eq!(activity.label_draft_for(&id), Some("Rent"));
        load(&mut activity, tx);
        drop(activity.update(
            None,
            &cache,
            AppMessage::ActivityLabelSaved(1, Err("late".into())),
        ));
        assert_eq!(activity.label_error_for(&id), None);
    }

    #[test]
    fn a_failed_save_survives_moving_to_another_row() {
        let cache = Cache::default();
        let mut activity = Activity::new(None, None);
        let tx = outgoing_vault_payment();
        let id = Entry::vault(tx.clone()).id;
        drop(activity.update(
            None,
            &cache,
            AppMessage::ActivityVaultLoaded(0, Ok((vec![tx.clone()], false))),
        ));
        for message in edit(&id, "Rent") {
            drop(activity.update(None, &cache, message));
        }
        let key = format!("{}:1", tx.txid);
        drop(activity.update(
            Some(label_daemon(&key, "Rent")),
            &cache,
            AppMessage::View(Message::Activity(ActivityMessage::SaveLabel)),
        ));
        drop(activity.update(
            None,
            &cache,
            AppMessage::View(Message::Activity(ActivityMessage::Select("other".into()))),
        ));
        drop(activity.update(
            None,
            &cache,
            AppMessage::ActivityLabelSaved(1, Err("offline".into())),
        ));
        drop(activity.update(
            None,
            &cache,
            AppMessage::View(Message::Activity(ActivityMessage::Select(id.clone()))),
        ));
        assert!(activity.label_error_for(&id).is_some());
        assert_eq!(activity.label_draft_for(&id), Some("Rent"));
        assert!(!activity.label_saving_for(&id));
    }

    fn activity_generation(activity: &Activity) -> u64 {
        // `reload` advances the generation once per call; tests start at 0.
        activity.generation_for_tests()
    }

    fn spark_receipts(count: u64) -> Vec<PaymentSummary> {
        (0..count)
            .map(|n| PaymentSummary {
                id: format!("recv-{n}"),
                amount_sat: 1_000 + n as i64,
                status: "completed".into(),
                direction: "receive".into(),
                description: Some(format!("Spark receipt {n}")),
                timestamp: 1_760_000_000 + n * 60,
                ..spark_payment()
            })
            .collect()
    }

    fn spark_activity(cache: &Cache, count: u64) -> Activity {
        let mut activity = Activity::new(None, None);
        drop(activity.update(
            None,
            cache,
            crate::app::message::Message::ActivitySparkLoaded(
                0,
                Ok((spark_receipts(count), false)),
            ),
        ));
        activity
    }

    /// Tenshu v175 hides Liquid, even for an account with the grant and a
    /// Liquid wallet on disk: no Liquid wallet filter and no Liquid banner.
    #[test]
    fn activity_offers_no_liquid_while_liquid_is_hidden() {
        let gate = crate::app::features::LiquidGate {
            server_enabled: true,
            local_state_exists: true,
        };
        let cache = Cache {
            liquid_gate: gate,
            ..Default::default()
        };
        let mut activity = Activity::new(None, None);
        activity.set_liquid_shown(gate.show());
        assert_eq!(
            wallet_filters(&activity),
            [WalletFilter::All, WalletFilter::Spark, WalletFilter::Vault]
        );
        // An error left from before Liquid was hidden draws no banner.
        activity.liquid_error = Some("synthetic".into());
        activity.refundables_error = Some("synthetic".into());
        let page = mount(
            "activity-liquid-hidden",
            render(&activity, &cache),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
            1440 - crate::app::view::nav::shell::FULL_WIDTH as u32,
        );
        assert!(!page.contains("Liquid"));
        assert!(page.contains("Wallet"), "the filter bar is drawn");

        // Shown, both come back, so their absence above is the gate's doing.
        activity.set_liquid_shown(true);
        assert!(wallet_filters(&activity).contains(&WalletFilter::Liquid));
        activity.liquid_error = Some("synthetic".into());
        let page = mount(
            "activity-liquid-shown",
            render(&activity, &cache),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
            1440 - crate::app::view::nav::shell::FULL_WIDTH as u32,
        );
        assert!(page.contains("Liquid history couldn't load"));
    }

    #[test]
    fn wallet_recent_activity_shows_that_wallets_newest_five_rows() {
        let cache = Cache {
            balance_privacy: Some(BalancePrivacyChrome {
                privacy: BalancePrivacy::Shown,
                busy: false,
                interactive: false,
            }),
            ..Default::default()
        };
        let activity = spark_activity(&cache, 7);
        for (appearance, theme) in [
            ("dark", coincube_ui::theme::Theme::dark()),
            ("light", coincube_ui::theme::Theme::light()),
        ] {
            let spark = mount(
                "wallet-recent-activity-spark",
                wallet_recent_activity(&activity, &cache, WalletFilter::Spark, None),
                (1440, 900),
                appearance,
                theme,
                680,
            );
            assert!(spark.contains("Recent activity"));
            assert!(spark.contains("View all transactions"));
            assert!(!spark.contains("Last transactions"));
            // The newest five, as Activity rows without a wallet column.
            for n in 2..7 {
                assert!(spark.contains(&format!("Spark receipt {n}")), "row {}", n);
                let id = Entry::spark(spark_receipts(7).remove(n)).id;
                assert!(spark.has_focus(row_focus_id(&id)), "row {} focus", n);
            }
            for n in 0..2 {
                assert!(!spark.contains(&format!("Spark receipt {n}")), "row {}", n);
            }
            assert!(spark.contains("+1,006"));
            assert!(spark.contains("Confirmed"));
        }

        // The Vault page lists none of Spark's rows.
        let vault = mount(
            "wallet-recent-activity-vault-empty",
            wallet_recent_activity(&activity, &cache, WalletFilter::Vault, None),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
            680,
        );
        assert!(vault.contains("No transactions yet"));
        assert!(!vault.contains("Spark receipt"));
        assert!(!vault.contains("View all transactions"));
    }

    #[test]
    fn an_unknown_wallet_history_never_reads_as_no_transactions() {
        let cache = Cache::default();
        let activity = Activity::new(None, None);
        let note = "Transactions appear once the Vault's history has loaded";
        let mounted = mount(
            "wallet-recent-activity-history-unknown",
            wallet_recent_activity(&activity, &cache, WalletFilter::Vault, Some(note)),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
            680,
        );
        assert!(mounted.contains(note));
        assert!(!mounted.contains("No transactions yet"));
    }

    #[test]
    fn vault_proposals_neither_hold_nor_fail_the_vault_history() {
        let cache = Cache::default();
        let mut activity = Activity::new(None, None);
        // History answered empty; proposals still loading.
        activity.psbt_loading = true;
        let mounted = |activity: &Activity, scenario: &str| {
            mount(
                scenario,
                wallet_recent_activity(activity, &cache, WalletFilter::Vault, None),
                (1440, 900),
                "dark",
                coincube_ui::theme::Theme::dark(),
                680,
            )
        };
        let waiting = mounted(&activity, "wallet-recent-activity-proposals-loading");
        assert!(waiting.contains("No transactions yet"));
        assert!(!waiting.contains("Loading payment history"));
        // Proposals failed after the history loaded: said as proposals.
        activity.psbt_loading = false;
        activity.psbt_error = Some("daemon busy".into());
        let failed = mounted(&activity, "wallet-recent-activity-proposals-failed");
        assert!(failed.contains("Vault proposals couldn't load"));
        assert!(!failed.contains("Vault history couldn't load"));
    }

    #[test]
    fn the_pane_closes_when_its_row_leaves_the_page() {
        let cache = Cache::default();
        let mut activity = spark_activity(&cache, 6);
        // recv-1 is the fifth newest of six: shown, so its pane opens.
        activity.selected_id = Some(Entry::spark(spark_receipts(6).remove(1)).id);
        assert!(wallet_activity_detail(&activity, &cache, WalletFilter::Spark).is_some());
        // recv-0 is sixth: not on the page, so no pane without its row.
        activity.selected_id = Some(Entry::spark(spark_receipts(6).remove(0)).id);
        assert!(wallet_activity_detail(&activity, &cache, WalletFilter::Spark).is_none());
    }

    #[test]
    fn wallet_recent_activity_hides_amounts_with_balances() {
        let cache = Cache {
            balance_privacy: Some(BalancePrivacyChrome {
                privacy: BalancePrivacy::Hidden,
                busy: false,
                interactive: false,
            }),
            ..Default::default()
        };
        assert!(cache.balance_amounts_masked());
        let activity = spark_activity(&cache, 2);
        let mounted = mount(
            "wallet-recent-activity-hidden",
            wallet_recent_activity(&activity, &cache, WalletFilter::Spark, None),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
            680,
        );
        assert!(mounted.contains(MASK));
        assert!(!mounted.contains("1,001"));
    }

    #[test]
    fn wallet_page_detail_opens_only_for_its_own_selection() {
        let cache = Cache::default();
        let mut activity = spark_activity(&cache, 3);
        assert!(wallet_activity_detail(&activity, &cache, WalletFilter::Spark).is_none());
        let id = Entry::spark(spark_receipts(3).remove(1)).id;
        drop(activity.update(
            None,
            &cache,
            crate::app::message::Message::View(Message::Activity(ActivityMessage::Select(
                id.clone(),
            ))),
        ));
        assert!(wallet_activity_detail(&activity, &cache, WalletFilter::Vault).is_none());
        let detail = wallet_activity_detail(&activity, &cache, WalletFilter::Spark)
            .expect("Spark's selected row has a pane on the Spark page");
        let mounted = mount(
            "wallet-recent-activity-detail",
            detail_column(detail),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
            420,
        );
        assert!(mounted.contains("Spark receipt 1"));
        assert!(mounted.has_focus(Id::from(DETAIL_CLOSE_ID)));

        let page = mount(
            "wallet-recent-activity-selected-row",
            wallet_recent_activity(&activity, &cache, WalletFilter::Spark, None),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
            680,
        );
        assert!(page.has_focus(row_focus_id(&id)));

        activity.clear_selection();
        assert!(wallet_activity_detail(&activity, &cache, WalletFilter::Spark).is_none());
    }

    #[test]
    fn wallet_detail_sits_beside_the_page_only_when_both_fit() {
        // 1024 window, compact menu: the pane slides over the page.
        assert!(!wallet_detail_beside(816.0));
        // 1440 window: page rows and the 420px pane side by side.
        assert!(wallet_detail_beside(1192.0));
        // The overview's 16px insets and the rule count: at the edge the
        // details column keeps its 160px.
        let edge = 1100.0;
        let details = edge
            - SELECTED_DETAIL_WIDTH
            - 1.0
            - 2.0 * (super::super::page_gutter(edge) + WALLET_PAGE_INSET)
            - (RECENT_TIME_WIDTH + 128.0 + 132.0 + 3.0 * 12.0 + 2.0 * ROW_INSET);
        assert_eq!(wallet_detail_beside(edge), details >= 160.0);
    }

    #[test]
    fn spark_refresh_replaces_rows_and_view_all_filters_activity() {
        let cache = Cache::default();
        let mut activity = spark_activity(&cache, 4);
        let generation = activity_generation(&activity);
        drop(activity.update(
            None,
            &cache,
            crate::app::message::Message::ActivitySparkRefreshed(
                generation,
                Ok((spark_receipts(2), false)),
            ),
        ));
        // A refresh is the newest page, not another "Load older" page.
        assert_eq!(activity.wallet_entries(WalletFilter::Spark).len(), 2);
        // An answer from before a reload is ignored.
        drop(activity.update(
            None,
            &cache,
            crate::app::message::Message::ActivitySparkRefreshed(
                generation.wrapping_add(1),
                Ok((spark_receipts(5), false)),
            ),
        ));
        assert_eq!(activity.wallet_entries(WalletFilter::Spark).len(), 2);

        activity.selected_id = Some("spark:lightning:recv-0".into());
        activity.filters.search = "coffee".into();
        drop(activity.update(
            None,
            &cache,
            crate::app::message::Message::View(Message::Activity(ActivityMessage::ViewAll(
                WalletFilter::Spark,
            ))),
        ));
        assert_eq!(activity.filters.wallet, WalletFilter::Spark);
        assert_eq!(activity.filters.count(), 1);
        assert!(activity.selected_id.is_none());
    }

    #[test]
    fn hidden_balances_hide_invoices_and_refund_amounts() {
        let entry = Entry::spark(PaymentSummary {
            details: coincube_spark_protocol::PaymentDetailSummary {
                invoice: Some("lnbc42u1psyntheticinvoiceforactivitytests".into()),
                ..Default::default()
            },
            ..lightning_send()
        });
        assert!(entry.title.starts_with("Lightning invoice · lnbc42u1"));
        assert_eq!(shown_title(&entry, true), "Lightning payment");
        assert_eq!(shown_title(&entry, false), entry.title);
        let (_, destination) = entry.peer.clone().unwrap();
        assert_eq!(private_fact("Destination", &destination, true), MASK);
        assert_eq!(
            private_fact("Destination", "shop@example.test", true),
            "shop@example.test"
        );
        assert_eq!(private_fact("Refund amount", "1,000 SATS", true), MASK);
        assert_eq!(
            private_fact("Refund amount", "1,000 SATS", false),
            "1,000 SATS"
        );
        assert_eq!(private_fact("Invoice", "spark1synthetic", true), MASK);
        assert_eq!(
            private_fact("Destination", "bitcoin:bc1qsynthetic?amount=0.001", true),
            MASK
        );
    }

    #[test]
    fn rows_that_need_the_user_offer_their_next_step() {
        let refund = Entry::refundable(DomainRefundableSwap {
            swap_address: "bc1qsyntheticswapaddress".into(),
            timestamp: 1_760_000_000,
            amount_sat: 20_000,
        });
        assert_eq!(next_step(&refund, false), Some("Refund"));
        let mut unconfirmed = outgoing_vault_payment();
        unconfirmed.time = None;
        let unconfirmed = Entry::vault(unconfirmed);
        assert_eq!(next_step(&unconfirmed, true), Some("Bump fee or cancel"));
        // The Vault screen would offer neither action.
        assert_eq!(next_step(&unconfirmed, false), None);
        assert_eq!(
            next_step(&Entry::vault(outgoing_vault_payment()), true),
            None
        );
        assert_eq!(next_step(&Entry::spark(spark_payment()), true), None);

        let cache = Cache::default();
        let mut activity = Activity::new(None, None);
        // A refundable swap is a Liquid row, listed only while the Liquid
        // gate shows Liquid (hidden since Tenshu v175).
        activity.set_liquid_shown(true);
        drop(activity.update(
            None,
            &cache,
            AppMessage::ActivityRefundablesLoaded(
                0,
                Ok(vec![DomainRefundableSwap {
                    swap_address: "bc1qsyntheticswapaddress".into(),
                    timestamp: 1_760_000_000,
                    amount_sat: 20_000,
                }]),
            ),
        ));
        activity.selected_id = Some(refund.id.clone());
        let mounted = mount(
            "r09-activity-refundable-detail",
            render_at_width(&activity, &cache, 1192.0),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
            1192,
        );
        assert!(mounted.has_focus(Id::from(DETAIL_NEXT_STEP_ID)));
        assert!(mounted.contains("Refund"));
    }
}
