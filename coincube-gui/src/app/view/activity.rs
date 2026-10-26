//! Compact, cross-wallet Activity presentation. Amounts are shown in their
//! source units; a current spot quote is never presented as historical fiat.

use chrono::{DateTime, Local, NaiveDate, Utc};
use coincube_spark_protocol::PaymentSummary;
use coincube_ui::{
    component::{button, text::*},
    widget::{Column, Element},
};
use iced::{
    widget::{column, container, pick_list, row, text_input},
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
use crate::daemon::model::{HistoryTransaction, SpendTx};

use super::Message;

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
                Self::All => "All directions",
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
                Self::All => "All statuses",
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
    pub date_start: String,
    pub date_end: String,
    pub search: String,
}

impl Filters {
    fn count(&self) -> usize {
        usize::from(self.wallet != WalletFilter::All)
            + usize::from(self.direction != DirectionFilter::All)
            + usize::from(self.status != StatusFilter::All)
            + usize::from(!self.date_start.is_empty() || !self.date_end.is_empty())
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
        Ok((parse(&self.date_start)?, parse(&self.date_end)?))
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
            let needle = self.search.trim().to_lowercase();
            if !entry.title.to_lowercase().contains(&needle)
                && !entry.id.to_lowercase().contains(&needle)
            {
                return false;
            }
        }
        if let Ok((start, end)) = self.dates() {
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
        }
        true
    }
}

#[derive(Debug, Clone)]
pub enum ActivityMessage {
    Wallet(WalletFilter),
    Direction(DirectionFilter),
    Status(StatusFilter),
    DateStart(String),
    DateEnd(String),
    Search(String),
    Clear,
    Select(String),
    CloseDetail,
    Retry,
    LoadMore,
    OpenDetail(ActivityTarget),
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub wallet: WalletFilter,
    pub rail: &'static str,
    pub timestamp: u64,
    pub direction: DirectionFilter,
    pub status: StatusFilter,
    pub status_label: String,
    pub title: String,
    pub amount: String,
    pub fee: Option<String>,
    pub destination: Option<String>,
    pub target: ActivityTarget,
    pub identifier: String,
    pub identifier_label: &'static str,
    pub signer_status: Option<String>,
}

impl Entry {
    pub fn vault(tx: HistoryTransaction) -> Self {
        let id = format!("vault:onchain:{}", tx.txid);
        let identifier = tx.txid.to_string();
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
            title,
            amount: format!("{} sats", amount.to_sat()),
            fee: tx.fee_amount.map(|fee| format!("{} sats", fee.to_sat())),
            destination: None,
            target: ActivityTarget::Vault(tx),
            identifier,
            identifier_label: "Txid",
            signer_status: None,
        }
    }

    pub fn psbt(tx: SpendTx) -> Self {
        let txid = tx.psbt.unsigned_tx.compute_txid().to_string();
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
            status_label: signer_status.clone(),
            title: tx
                .labels
                .get(&txid)
                .cloned()
                .unwrap_or_else(|| "Vault transaction".into()),
            amount: format!("{} sats", tx.spend_amount.to_sat()),
            fee: tx.fee_amount.map(|fee| format!("{} sats", fee.to_sat())),
            destination: None,
            target: ActivityTarget::Psbt(tx),
            identifier: txid,
            identifier_label: "PSBT transaction ID",
            signer_status: Some(signer_status),
        }
    }

    pub fn spark(payment: PaymentSummary) -> Self {
        let identifier = payment.id.clone();
        let direction = if payment.direction.eq_ignore_ascii_case("receive") {
            DirectionFilter::Received
        } else {
            DirectionFilter::Sent
        };
        let status = match payment.status.to_ascii_lowercase().as_str() {
            "completed" | "complete" | "succeeded" | "success" => StatusFilter::Confirmed,
            "failed" | "timeout" | "timedout" => StatusFilter::Failed,
            _ => StatusFilter::Pending,
        };
        let rail = match payment.method.as_str() {
            "lightning" => "Lightning",
            "deposit" | "withdraw" => "Bitcoin on-chain",
            "token" => "Spark token",
            _ => "Spark",
        };
        let amount = if let Some(minor) = payment.token_amount {
            format!(
                "{} {}",
                format_token_display(minor, payment.token_decimals.unwrap_or(0)),
                payment.token_ticker.as_deref().unwrap_or("token")
            )
        } else {
            format!("{} sats", payment.amount_sat.unsigned_abs())
        };
        Self {
            id: format!("spark:{}:{}", payment.method, payment.id),
            wallet: WalletFilter::Spark,
            rail,
            timestamp: payment.timestamp,
            direction,
            status,
            status_label: payment.status.clone(),
            title: payment
                .description
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| rail.to_owned()),
            amount,
            fee: (payment.fees_sat > 0).then(|| format!("{} sats", payment.fees_sat)),
            destination: None,
            target: ActivityTarget::Spark(payment),
            identifier,
            identifier_label: "Payment ID",
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
        let identifier = payment.tx_id.clone().or(swap_id).unwrap_or_default();
        let identifier_label = if payment.tx_id.is_some() {
            "Txid"
        } else {
            "Swap ID"
        };
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
        let rail = match &payment.details {
            DomainPaymentDetails::Lightning { .. } => "Lightning on Liquid",
            DomainPaymentDetails::OnChainBitcoin { .. } => "Bitcoin swap",
            DomainPaymentDetails::LiquidAsset { .. } => "Liquid asset",
        };
        let amount = match &payment.details {
            DomainPaymentDetails::LiquidAsset {
                asset_id,
                asset_info,
                ..
            } => match asset_info {
                Some(info) => {
                    let ticker = if asset_id == USDT_ASSET_ID_MAINNET {
                        "USDt"
                    } else {
                        asset_id.as_str()
                    };
                    format!(
                        "{} {}",
                        format_token_display(info.amount_minor, u32::from(info.precision)),
                        ticker
                    )
                }
                None if asset_id == USDT_ASSET_ID_MAINNET => {
                    format!("{} USDt", format_usdt_display(payment.amount_sat))
                }
                None => format!("{} base units · {}", payment.amount_sat, asset_id),
            },
            _ => format!("{} sats", payment.amount_sat),
        };
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
        Self {
            id: format!("liquid:{source_id}"),
            wallet: WalletFilter::Liquid,
            rail,
            timestamp: payment.timestamp as u64,
            direction,
            status,
            status_label: status_label.into(),
            title,
            amount,
            fee: (payment.fees_sat > 0).then(|| format!("{} sats", payment.fees_sat)),
            destination: payment.destination.clone(),
            target: ActivityTarget::Liquid(payment),
            identifier,
            identifier_label,
            signer_status: None,
        }
    }

    pub fn refundable(swap: DomainRefundableSwap) -> Self {
        let identifier = swap.swap_address.clone();
        Self {
            id: format!("liquid:refund:{identifier}"),
            wallet: WalletFilter::Liquid,
            rail: "Bitcoin swap refund",
            timestamp: swap.timestamp as u64,
            direction: DirectionFilter::Received,
            status: StatusFilter::Failed,
            status_label: "Refundable".into(),
            title: "Refund available".into(),
            amount: format!("{} sats", swap.amount_sat),
            fee: None,
            destination: Some(identifier.clone()),
            target: ActivityTarget::Refundable(swap),
            identifier,
            identifier_label: "Swap address",
            signer_status: None,
        }
    }
}

fn fee_detail(fee: &str, amounts_masked: bool) -> String {
    if amounts_masked {
        "Fee · ••••••".to_owned()
    } else {
        format!("Fee · {fee}")
    }
}

fn vault_confirmation_detail(height: i32, tip: i32) -> String {
    if tip <= 0 || tip < height {
        "Confirmations · Unavailable".to_owned()
    } else {
        format!("Confirmations · {}", i64::from(tip) - i64::from(height) + 1)
    }
}

// The fixed columns and four gaps need 467px before the Details column.
// Reserve 200px for Details when the 420px detail pane sits beside the list.
const MIN_ACTIVITY_LIST_WIDTH: f32 = 70.0 + 75.0 + 200.0 + 190.0 + 100.0 + 4.0 * 8.0;
const SELECTED_DETAIL_WIDTH: f32 = 420.0;
const DETAIL_GAP: f32 = 16.0;

fn detail_beside_list(width: f32) -> bool {
    width >= MIN_ACTIVITY_LIST_WIDTH + SELECTED_DETAIL_WIDTH + DETAIL_GAP
}

pub fn render<'a>(activity: &'a Activity, cache: &'a Cache) -> Element<'a, Message> {
    iced::widget::responsive(move |size: iced::Size| render_at_width(activity, cache, size.width))
        .height(Length::Shrink)
        .into()
}

fn render_at_width<'a>(
    activity: &'a Activity,
    cache: &'a Cache,
    width: f32,
) -> Element<'a, Message> {
    let entries = activity.entries();
    let selected = activity
        .selected_id
        .as_ref()
        .and_then(|id| entries.iter().find(|entry| &entry.id == id));
    let count = activity.filters.count();
    let compact = width < 900.0;
    let wallet = pick_list(
        [
            WalletFilter::All,
            WalletFilter::Vault,
            WalletFilter::Spark,
            WalletFilter::Liquid,
        ],
        Some(activity.filters.wallet),
        |value| Message::Activity(ActivityMessage::Wallet(value)),
    )
    .width(Length::Fixed(if compact { 120.0 } else { 130.0 }));
    let direction = pick_list(
        [
            DirectionFilter::All,
            DirectionFilter::Received,
            DirectionFilter::Sent,
            DirectionFilter::Transfers,
        ],
        Some(activity.filters.direction),
        |value| Message::Activity(ActivityMessage::Direction(value)),
    )
    .width(Length::Fixed(if compact { 140.0 } else { 150.0 }));
    let status = pick_list(
        [
            StatusFilter::All,
            StatusFilter::Pending,
            StatusFilter::Confirmed,
            StatusFilter::Failed,
        ],
        Some(activity.filters.status),
        |value| Message::Activity(ActivityMessage::Status(value)),
    )
    .width(Length::Fixed(if compact { 120.0 } else { 130.0 }));
    let from = text_input("From YYYY-MM-DD", &activity.filters.date_start)
        .on_input(|value| Message::Activity(ActivityMessage::DateStart(value)))
        .width(Length::Fixed(if compact { 130.0 } else { 145.0 }));
    let to = text_input("To YYYY-MM-DD", &activity.filters.date_end)
        .on_input(|value| Message::Activity(ActivityMessage::DateEnd(value)))
        .width(Length::Fixed(if compact { 130.0 } else { 145.0 }));
    let search = text_input("Search label or ID", &activity.filters.search)
        .on_input(|value| Message::Activity(ActivityMessage::Search(value)))
        .width(Length::Fill);
    let controls: Element<'a, Message> = if compact {
        column![
            row![wallet, direction, status].spacing(8),
            row![from, to, search].spacing(8)
        ]
        .spacing(8)
        .into()
    } else {
        row![wallet, direction, status, from, to, search]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
    };
    let mut filters = Column::new().push(controls);
    if count > 0 {
        filters = filters.push(
            row![
                text(format!("{count} filters")),
                button::secondary(None, "Clear filters")
                    .on_press(Message::Activity(ActivityMessage::Clear))
            ]
            .spacing(8),
        );
    }
    let mut list = Column::new().spacing(8).push(h3("Activity")).push(filters);
    if activity.filters.dates().is_err() {
        list = list
            .push(text("Use YYYY-MM-DD for dates.").style(coincube_ui::theme::text::destructive));
    }
    for error in [
        activity.vault_error.as_deref(),
        activity.psbt_error.as_deref(),
        activity.spark_error.as_deref(),
        activity.liquid_error.as_deref(),
        activity.refundables_error.as_deref(),
    ]
    .iter()
    .flatten()
    {
        list = list.push(
            row![
                text(format!("History unavailable: {error}")),
                button::secondary(None, "Retry")
                    .on_press(Message::Activity(ActivityMessage::Retry))
            ]
            .spacing(8),
        );
    }
    if activity.vault_loading
        || activity.psbt_loading
        || activity.spark_loading
        || activity.liquid_loading
        || activity.refundables_loading
    {
        list = list.push(text("Loading payment history…"));
    }
    list = list.push(
        row![
            text("Time").width(Length::Fixed(70.0)),
            text("Wallet").width(Length::Fixed(75.0)),
            text("Details").width(Length::Fill),
            text("Amount").width(Length::Fixed(190.0)),
            text("Status").width(Length::Fixed(100.0)),
        ]
        .spacing(8),
    );
    let visible: Vec<_> = entries
        .iter()
        .filter(|entry| activity.filters.includes(entry))
        .collect();
    if visible.is_empty()
        && !activity.vault_loading
        && !activity.psbt_loading
        && !activity.spark_loading
        && !activity.liquid_loading
        && !activity.refundables_loading
    {
        list = list.push(text(if entries.is_empty() {
            "No transactions yet"
        } else {
            "No activity matches these filters"
        }));
    }
    let mut previous_day = None;
    let mut first_day = true;
    for entry in visible {
        let day = (entry.timestamp != 0)
            .then(|| DateTime::<Utc>::from_timestamp(entry.timestamp as i64, 0))
            .flatten()
            .map(|time| time.with_timezone(&Local).date_naive());
        if first_day || day != previous_day {
            list = list.push(
                text(day.map_or_else(
                    || "Pending".to_owned(),
                    |day| day.format("%A, %B %e").to_string(),
                ))
                .bold(),
            );
            previous_day = day;
            first_day = false;
        }
        let time = (entry.timestamp != 0)
            .then(|| DateTime::<Utc>::from_timestamp(entry.timestamp as i64, 0))
            .flatten()
            .map_or_else(
                || "—".to_owned(),
                |time| time.with_timezone(&Local).format("%H:%M").to_string(),
            );
        let amount = if cache.balance_amounts_masked() {
            "••••••".to_owned()
        } else {
            entry.amount.clone()
        };
        list = list.push(
            iced::widget::button(
                row![
                    text(time).width(Length::Fixed(70.0)),
                    text(entry.wallet.to_string()).width(Length::Fixed(75.0)),
                    text(format!("{} · {}", entry.title, entry.rail)).width(Length::Fill),
                    Column::new()
                        .push(text(amount))
                        .push(
                            text("Historical fiat unavailable")
                                .size(11)
                                .style(coincube_ui::theme::text::secondary)
                        )
                        .width(Length::Fixed(190.0)),
                    text(entry.status_label.clone()).width(Length::Fixed(100.0)),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .on_press(Message::Activity(ActivityMessage::Select(entry.id.clone()))),
        );
    }
    if activity.vault_has_more || activity.spark_has_more || activity.liquid_has_more {
        let mut more = button::secondary(None, "Load older activity");
        if !activity.vault_loading
            && !activity.psbt_loading
            && !activity.spark_loading
            && !activity.liquid_loading
            && !activity.refundables_loading
        {
            more = more.on_press(Message::Activity(ActivityMessage::LoadMore));
        }
        list = list.push(more);
    }
    let main: Element<'a, Message> = list.width(Length::Fill).into();
    if let Some(entry) = selected {
        let mut detail = Column::new()
            .spacing(12)
            .push(
                row![
                    h3(entry.title.clone()),
                    button::secondary(None, "Close")
                        .on_press(Message::Activity(ActivityMessage::CloseDetail))
                ]
                .spacing(12),
            )
            .push(text(format!("{} · {}", entry.wallet, entry.rail)))
            .push(text(if cache.balance_amounts_masked() {
                "••••••".to_owned()
            } else {
                entry.amount.clone()
            }))
            .push(text("Historical fiat amount unavailable"))
            .push(text(entry.status_label.clone()));
        if entry.signer_status.is_some() {
            detail = detail.push(button::secondary(None, "Continue signing").on_press(
                Message::Activity(ActivityMessage::OpenDetail(entry.target.clone())),
            ));
        }
        if let Some(time) = (entry.timestamp != 0)
            .then(|| DateTime::<Utc>::from_timestamp(entry.timestamp as i64, 0))
            .flatten()
        {
            detail = detail.push(text(format!(
                "Date · {}",
                time.with_timezone(&Local).format("%b %e, %Y %H:%M %Z")
            )));
        }
        if let Some(fee) = &entry.fee {
            detail = detail.push(text(fee_detail(fee, cache.balance_amounts_masked())));
        }
        if let ActivityTarget::Vault(tx) = &entry.target {
            if let Some(height) = tx.height {
                detail = detail.push(text(vault_confirmation_detail(height, cache.blockheight())));
            }
        }
        if let Some(destination) = &entry.destination {
            detail = detail.push(text(format!("Destination · {destination}")));
        }
        if entry.signer_status.is_none() && !entry.identifier.is_empty() {
            detail = detail.push(
                row![
                    text(format!("{} · {}", entry.identifier_label, entry.identifier)),
                    button::secondary(None, "Copy").on_press(Message::CopyWithToast(
                        entry.identifier.clone(),
                        "Identifier copied".into()
                    ))
                ]
                .spacing(8),
            );
        }
        detail = detail.push(
            button::secondary(None, "Open wallet details and actions").on_press(Message::Activity(
                ActivityMessage::OpenDetail(entry.target.clone()),
            )),
        );
        let detail = container(detail).padding(20);
        if !detail_beside_list(width) {
            column![main, detail.width(Length::Fill)].spacing(16).into()
        } else {
            row![main, detail.width(Length::Fixed(SELECTED_DETAIL_WIDTH))]
                .spacing(DETAIL_GAP)
                .into()
        }
    } else {
        main
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::wallets::DomainLiquidAssetInfo;

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
        }
    }

    #[test]
    fn spark_identity_and_failure_filter_keep_source_provenance() {
        let row = Entry::spark(spark_payment());
        assert_eq!(row.id, "spark:lightning:pay-42");
        assert_eq!(row.identifier, "pay-42");
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
            date_start: day.format("%Y-%m-%d").to_string(),
            date_end: day.format("%Y-%m-%d").to_string(),
            ..Default::default()
        };
        assert!(filters.includes(&row));
        filters.date_start = "2026-13-99".into();
        assert!(filters.dates().is_err());
    }

    #[test]
    fn asset_amounts_use_source_precision() {
        let mut spark = spark_payment();
        spark.token_amount = Some(1_580_000);
        spark.token_decimals = Some(6);
        spark.token_ticker = Some("USDB".into());
        assert_eq!(Entry::spark(spark).amount, "1.58 USDB");

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
        assert_eq!(Entry::liquid(liquid).amount, "5.00 USDt");
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
        assert_eq!(Entry::liquid(payment.clone()).amount, "5.00 USDt");
        if let DomainPaymentDetails::LiquidAsset { asset_id, .. } = &mut payment.details {
            *asset_id = "unknown-asset-id".into();
        }
        assert_eq!(
            Entry::liquid(payment).amount,
            "500000000 base units · unknown-asset-id"
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
        assert_eq!(fee_detail("123 sats", true), "Fee · ••••••");
        assert_eq!(fee_detail("123 sats", false), "Fee · 123 sats");
    }

    #[test]
    fn vault_confirmations_require_a_known_chain_tip() {
        assert_eq!(
            vault_confirmation_detail(800_000, 0),
            "Confirmations · Unavailable"
        );
        assert_eq!(
            vault_confirmation_detail(800_000, 799_999),
            "Confirmations · Unavailable"
        );
        assert_eq!(
            vault_confirmation_detail(800_000, 800_000),
            "Confirmations · 1"
        );
        assert_eq!(
            vault_confirmation_detail(800_000, 800_004),
            "Confirmations · 5"
        );
    }

    #[test]
    fn selected_detail_stacks_until_list_and_pane_fit() {
        // The Activity route uses the full shell content with 24px gutters.
        let activity_content = |viewport: f32| {
            viewport
                - super::super::nav::split::sidebar_width_for_window(viewport)
                - 2.0 * super::super::ACTIVITY_HORIZONTAL_PADDING
        };
        assert_eq!(activity_content(1024.0), 768.0);
        assert_eq!(activity_content(1200.0), 904.0);
        assert_eq!(activity_content(1440.0), 1144.0);
        assert!(!detail_beside_list(activity_content(1024.0)));
        assert!(!detail_beside_list(activity_content(1200.0)));
        assert!(detail_beside_list(activity_content(1440.0)));
        assert!(
            activity_content(1440.0) - SELECTED_DETAIL_WIDTH - DETAIL_GAP
                >= MIN_ACTIVITY_LIST_WIDTH
        );
        assert!(detail_beside_list(
            MIN_ACTIVITY_LIST_WIDTH + SELECTED_DETAIL_WIDTH + DETAIL_GAP
        ));
    }
}
