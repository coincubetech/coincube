use coincube_ui::{
    component::{card, text::*},
    theme,
    widget::*,
};
use iced::{
    widget::{column, container, row, Space},
    Length,
};

use super::format_with_separators;
use super::order_card::OrderType;
use crate::app::view::{self, message::P2PMessage};
use coincube_ui::component::amount::format_f64_as_string;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TradeStatus {
    Pending,
    Active,
    WaitingPayment,
    WaitingBuyerInvoice,
    FiatSent,
    SettledHoldInvoice,
    PaymentFailed,
    Success,
    Canceled,
    CooperativelyCanceled,
    Dispute,
    Expired,
}

impl TradeStatus {
    pub fn label(&self) -> &'static str {
        match self {
            TradeStatus::Pending => "Pending",
            TradeStatus::Active => "Active",
            TradeStatus::WaitingPayment => "Locking Escrow",
            TradeStatus::WaitingBuyerInvoice => "Waiting Invoice",
            TradeStatus::FiatSent => "Fiat Sent",
            TradeStatus::SettledHoldInvoice => "Paying Sats",
            TradeStatus::PaymentFailed => "Payment Failed",
            TradeStatus::Success => "Success",
            TradeStatus::Canceled => "Canceled",
            TradeStatus::CooperativelyCanceled => "Canceling",
            TradeStatus::Dispute => "Dispute",
            TradeStatus::Expired => "Expired",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TradeStatus::Success | TradeStatus::Canceled | TradeStatus::Expired
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TradeRole {
    Creator,
    Taker,
}

impl TradeRole {
    pub fn label(&self) -> &'static str {
        match self {
            TradeRole::Creator => "Created by You",
            TradeRole::Taker => "Taken by You",
        }
    }
}

#[derive(Debug, Clone)]
pub struct P2PTrade {
    pub id: String,
    /// Buy or Sell from our perspective
    pub order_type: OrderType,
    pub status: TradeStatus,
    /// Whether we created or took this order
    pub role: TradeRole,
    pub fiat_amount: f64,
    pub min_amount: Option<f64>,
    pub max_amount: Option<f64>,
    pub fiat_currency: String,
    pub sats_amount: Option<u64>,
    pub premium_percent: Option<f64>,
    pub payment_method: String,
    pub counterparty_rating: Option<f32>,
    pub created_at_ts: i64,
    pub created_at: String,
    pub time_ago: String,
    /// Latest DM action from Mostro (e.g. "PayInvoice", "FiatSentOk")
    pub last_dm_action: Option<String>,
    /// Timestamp when the current countdown phase started (PayInvoice, AddInvoice, etc.)
    pub countdown_start_ts: Option<u64>,
    /// Counterparty's trade public key (hex)
    pub counterparty_pubkey: Option<String>,
    /// Admin/solver's trade public key (hex), set when admin takes dispute
    pub admin_pubkey: Option<String>,
    /// Hold invoice the seller must pay (extracted from PayInvoice DM payload)
    pub hold_invoice: Option<String>,
}

impl P2PTrade {
    pub fn is_range_order(&self) -> bool {
        self.min_amount.is_some() && self.max_amount.is_some()
    }

    pub fn is_fixed_price(&self) -> bool {
        self.sats_amount.is_some() && self.sats_amount != Some(0)
    }

    pub fn premium_text(&self) -> String {
        super::format_premium(self.premium_percent)
    }

    pub fn order_type_label(&self) -> &'static str {
        match self.order_type {
            OrderType::Buy => "BUY",
            OrderType::Sell => "SELL",
        }
    }
}

pub fn trade_card<'a>(trade: &'a P2PTrade, mask_amounts: bool) -> Button<'a, view::Message> {
    let type_badge_style = match trade.order_type {
        OrderType::Buy => theme::pill::success as fn(&_) -> _,
        OrderType::Sell => theme::pill::warning as fn(&_) -> _,
    };

    let status_badge_style = match trade.status {
        TradeStatus::Active | TradeStatus::FiatSent | TradeStatus::SettledHoldInvoice => {
            theme::pill::success as fn(&_) -> _
        }
        TradeStatus::Success => theme::pill::info as fn(&_) -> _,
        TradeStatus::Pending | TradeStatus::WaitingPayment | TradeStatus::WaitingBuyerInvoice => {
            theme::pill::simple as fn(&_) -> _
        }
        TradeStatus::Dispute => theme::pill::warning as fn(&_) -> _,
        TradeStatus::CooperativelyCanceled => theme::pill::warning as fn(&_) -> _,
        TradeStatus::PaymentFailed | TradeStatus::Canceled | TradeStatus::Expired => {
            theme::pill::error as fn(&_) -> _
        }
    };

    let content = card::simple(
        column![
            // Header: Order type badge, status badge, and timestamp
            row!(
                container(p2_regular(trade.order_type_label()))
                    .padding([4, 12])
                    .style(type_badge_style),
                container(p2_regular(trade.status.label()))
                    .padding([4, 12])
                    .style(status_badge_style),
                Space::new().width(Length::Fill),
                p2_regular(&trade.time_ago).style(theme::text::secondary)
            )
            .spacing(10)
            .align_y(iced::alignment::Vertical::Center),
            // Amount and currency
            if mask_amounts {
                row!(
                    h2("••••••").style(theme::text::primary),
                    p1_bold(format!(" {}", trade.fiat_currency)).style(theme::text::secondary)
                )
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center)
            } else if trade.is_range_order() {
                row!(
                    h2(format!(
                        "{} - {}",
                        format_f64_as_string(trade.min_amount.unwrap_or(0.0), ",", 0, false),
                        format_f64_as_string(trade.max_amount.unwrap_or(0.0), ",", 0, false)
                    ))
                    .style(theme::text::primary),
                    p1_bold(format!(" {}", trade.fiat_currency)).style(theme::text::secondary)
                )
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center)
            } else {
                row!(
                    h2(format_f64_as_string(trade.fiat_amount, ",", 2, false))
                        .style(theme::text::primary),
                    p1_bold(format!(" {}", trade.fiat_currency)).style(theme::text::secondary)
                )
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center)
            },
            // Sats amount or market price
            if trade.is_fixed_price() {
                row![
                    p2_regular("for").style(theme::text::secondary),
                    p2_bold(if mask_amounts {
                        "•••••• sats".to_string()
                    } else {
                        format!(
                            "{} sats",
                            format_with_separators(trade.sats_amount.unwrap_or(0))
                        )
                    })
                ]
                .spacing(8)
            } else {
                row![
                    p2_regular("Market Price").style(theme::text::secondary),
                    p2_bold(trade.premium_text())
                ]
                .spacing(8)
            },
            // Payment method
            container(p2_regular(&trade.payment_method))
                .padding(12)
                .width(Length::Fill)
                .style(theme::container::background),
            // Role and rating
            row![
                container(p2_regular(trade.role.label()))
                    .padding([4, 12])
                    .style(theme::pill::simple as fn(&_) -> _),
                Space::new().width(Length::Fill),
                if let Some(rating) = trade.counterparty_rating {
                    p2_regular(format!("Rating: {:.1}", rating))
                } else {
                    p2_regular("No rating").style(theme::text::secondary)
                }
            ]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center),
        ]
        .spacing(12),
    )
    .width(Length::Fill);

    Button::new(content)
        .style(theme::button::container)
        .on_press(view::Message::P2P(P2PMessage::SelectTrade(
            trade.id.clone(),
        )))
        .width(Length::Fill)
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    use crate::app::view::shared::landing::capture;

    fn trade() -> P2PTrade {
        P2PTrade {
            id: "synthetic-trade".into(),
            order_type: OrderType::Buy,
            status: TradeStatus::Active,
            role: TradeRole::Taker,
            fiat_amount: 12_345.67,
            min_amount: None,
            max_amount: None,
            fiat_currency: "USD".into(),
            sats_amount: Some(98_765),
            premium_percent: None,
            payment_method: "Synthetic bank transfer".into(),
            counterparty_rating: None,
            created_at_ts: 0,
            created_at: "synthetic".into(),
            time_ago: "Just now".into(),
            last_dm_action: None,
            countdown_start_ts: None,
            counterparty_pubkey: None,
            admin_pubkey: None,
            hold_invoice: None,
        }
    }

    fn inspect(trade: &P2PTrade, mask_amounts: bool) -> capture::Inspection {
        capture::inspect(
            "p2p-my-trades-card",
            trade_card(trade, mask_amounts).into(),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        )
    }

    #[test]
    fn my_trades_card_masks_fixed_fiat_and_sats_only_when_hidden() {
        let trade = trade();
        let hidden = inspect(&trade, true);
        assert!(hidden.contains("••••••"));
        assert!(!hidden.contains("12,345.67"));
        assert!(!hidden.contains("98,765"));
        assert!(hidden.contains("Synthetic bank transfer"));

        let shown = inspect(&trade, false);
        assert!(shown.contains("12,345.67"));
        assert!(shown.contains("98,765 sats"));
    }

    #[test]
    fn my_trades_card_masks_both_range_endpoints() {
        let mut trade = trade();
        trade.min_amount = Some(23_456.0);
        trade.max_amount = Some(34_567.0);

        let hidden = inspect(&trade, true);
        assert!(!hidden.contains("23,456"));
        assert!(!hidden.contains("34,567"));
        assert!(!hidden.contains("98,765"));

        let shown = inspect(&trade, false);
        assert!(shown.contains("23,456 - 34,567"));
        assert!(shown.contains("98,765 sats"));
    }
}
