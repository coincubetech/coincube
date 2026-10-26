use coincube_ui::widget::action::Action;
use coincube_ui::{
    component::{button, card, form, text::*},
    icon, theme,
    widget::*,
};
use iced::{
    advanced::widget::Id,
    widget::{column, container, row, Space},
    Alignment, Length,
};

use crate::app::view::{self, message::P2PMessage};

use coincube_ui::component::amount::format_f64_as_string;

use super::format_with_separators;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderType {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PricingMode {
    Market,
    Fixed,
}

#[derive(Debug, Clone)]
pub struct P2POrder {
    pub id: String,
    pub order_type: OrderType,
    pub fiat_amount: f64,
    pub fiat_currency: String,
    pub min_amount: Option<f64>,
    pub max_amount: Option<f64>,
    pub sats_amount: Option<u64>,
    pub premium_percent: Option<f64>,
    pub payment_methods: Vec<String>,
    pub seller_rating: Option<f32>,
    pub seller_reviews: Option<u32>,
    pub seller_days_old: Option<u32>,
    pub created_at: String,
    pub created_at_ts: u64,
    pub time_ago: String,
    pub is_mine: bool,
}

impl P2POrder {
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
            OrderType::Buy => "BUYING",
            OrderType::Sell => "SELLING",
        }
    }
}

/// Columns shared by the heading and every order button at wide widths.
fn book_columns<'a>(
    side: impl Into<Element<'a, view::Message>>,
    amount: impl Into<Element<'a, view::Message>>,
    payment: impl Into<Element<'a, view::Message>>,
    price: impl Into<Element<'a, view::Message>>,
    reputation: impl Into<Element<'a, view::Message>>,
    trailing: impl Into<Element<'a, view::Message>>,
) -> Row<'a, view::Message> {
    row![
        container(side).width(Length::FillPortion(2)),
        container(amount).width(Length::FillPortion(2)),
        container(payment).width(Length::FillPortion(3)),
        container(price).width(Length::FillPortion(2)),
        container(reputation).width(Length::FillPortion(2)),
        container(trailing).width(Length::Fixed(16.0)),
    ]
    .spacing(12)
    .align_y(iced::alignment::Vertical::Center)
    .width(Length::Fill)
}

pub fn order_table_header<'a>() -> Element<'a, view::Message> {
    container(book_columns(
        caption("Side"),
        caption("Amount"),
        caption("Payment Method"),
        caption("Market Price"),
        caption("Creator Reputation"),
        caption(""),
    ))
    .padding([8, 12])
    .width(Length::Fill)
    .into()
}

fn book_amount(order: &P2POrder) -> String {
    let amount = match (order.min_amount, order.max_amount) {
        (Some(min), Some(max)) => format!(
            "{} – {}",
            format_f64_as_string(min, ",", 0, false),
            format_f64_as_string(max, ",", 0, false)
        ),
        _ => format_f64_as_string(order.fiat_amount, ",", 2, false),
    };
    format!("{} {}", amount, order.fiat_currency)
}

fn book_price(order: &P2POrder) -> String {
    if let Some(sats) = order.sats_amount.filter(|sats| *sats > 0) {
        format!("Fixed · {} sats", format_with_separators(sats))
    } else {
        match order.premium_percent {
            Some(premium) if premium > 0.0 => format!("Market +{}%", premium),
            Some(premium) if premium < 0.0 => format!("Market {}%", premium),
            _ => "Market".to_string(),
        }
    }
}

fn book_reputation(order: &P2POrder) -> String {
    match (order.seller_rating, order.seller_days_old) {
        (Some(rating), Some(days)) => format!("★ {:.1} · {} days", rating, days),
        (Some(rating), None) => format!("★ {:.1}", rating),
        (None, Some(days)) => format!("No rating · {} days", days),
        (None, None) => "No rating".to_string(),
    }
}

fn book_side_label(order: &P2POrder) -> &'static str {
    match (&order.order_type, order.is_mine) {
        (OrderType::Buy, true) => "YOU ARE BUYING",
        (OrderType::Sell, true) => "YOU ARE SELLING",
        (OrderType::Sell, false) => "BUYING",
        (OrderType::Buy, false) => "SELLING",
    }
}

/// A single keyboard-actionable order entry. The label describes the taker's
/// action, while `order_type` remains the maker's side for selection/trading.
pub fn order_card<'a>(order: &'a P2POrder, compact: bool) -> Element<'a, view::Message> {
    let side = book_side_label(order);
    let side_style = match (&order.order_type, order.is_mine) {
        (OrderType::Sell, false) | (OrderType::Buy, true) => theme::text::success as fn(&_) -> _,
        (OrderType::Buy, false) | (OrderType::Sell, true) => theme::text::warning as fn(&_) -> _,
    };
    let payment = if order.payment_methods.is_empty() {
        "Unavailable".to_string()
    } else {
        order.payment_methods.join(", ")
    };
    let amount = book_amount(order);
    let price = book_price(order);
    let reputation = book_reputation(order);
    let content: Element<'a, view::Message> = if compact {
        column![
            row![
                p2_bold(side).style(side_style),
                Space::new().width(Length::Fill),
                p2_bold(amount),
                p2_regular("›").style(theme::text::secondary),
            ]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center),
            p2_regular(payment),
            row![
                p2_regular(price).style(theme::text::secondary),
                Space::new().width(Length::Fill),
                p2_regular(reputation).style(theme::text::secondary),
            ]
            .spacing(8),
        ]
        .spacing(6)
        .into()
    } else {
        book_columns(
            p2_bold(side).style(side_style),
            p2_bold(amount),
            p2_regular(payment),
            p2_regular(price),
            p2_regular(reputation),
            p2_regular("›").style(theme::text::secondary),
        )
        .into()
    };

    Action::new(
        Id::from(format!("p2p-book-order-{}", order.id)),
        container(content).padding([10, 12]).width(Length::Fill),
        view::Message::FocusAction,
    )
    .style(theme::button::transparent)
    .padding(0)
    .focus_radius(0.0)
    .on_press(view::Message::P2P(P2PMessage::SelectOrder(
        order.id.clone(),
    )))
    .width(Length::Fill)
    .into()
}

/// Detail view for a selected order.
/// State passed into order_detail for inline take-order fields.
pub struct TakeOrderState<'a> {
    pub amount: &'a form::Value<String>,
    pub invoice: &'a form::Value<String>,
    pub submitting: bool,
}

pub fn order_detail<'a>(
    order: &'a P2POrder,
    take_state: Option<TakeOrderState<'a>>,
) -> Container<'a, view::Message> {
    let badge_style = match order.order_type {
        OrderType::Buy => theme::pill::success as fn(&_) -> _,
        OrderType::Sell => theme::pill::warning as fn(&_) -> _,
    };

    let heading = if order.is_mine {
        match order.order_type {
            OrderType::Buy => "You are buying",
            OrderType::Sell => "You are selling",
        }
    } else {
        match order.order_type {
            OrderType::Buy => "Someone is buying",
            OrderType::Sell => "Someone is selling",
        }
    };

    // Colored premium
    let premium_style = match order.premium_percent {
        Some(p) if p > 0.0 => theme::text::success as fn(&_) -> _,
        Some(p) if p < 0.0 => theme::text::warning as fn(&_) -> _,
        _ => theme::text::secondary as fn(&_) -> _,
    };

    // Amount section
    let amount_card = card::simple(
        column![
            container(p2_regular(order.order_type_label()))
                .padding([4, 12])
                .style(badge_style),
            p2_regular(heading).style(theme::text::secondary),
            if order.is_range_order() {
                row!(
                    h1(format!(
                        "{} - {}",
                        format_f64_as_string(order.min_amount.unwrap_or(0.0), ",", 0, false),
                        format_f64_as_string(order.max_amount.unwrap_or(0.0), ",", 0, false)
                    ))
                    .style(theme::text::primary),
                    h3(format!(" {}", order.fiat_currency)).style(theme::text::secondary)
                )
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center)
            } else {
                row!(
                    h1(format_f64_as_string(order.fiat_amount, ",", 2, false))
                        .style(theme::text::primary),
                    h3(format!(" {}", order.fiat_currency)).style(theme::text::secondary)
                )
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center)
            },
            if order.is_fixed_price() {
                row![
                    p2_regular("for").style(theme::text::secondary),
                    p2_bold(format!(
                        "{} sats",
                        format_with_separators(order.sats_amount.unwrap_or(0))
                    ))
                ]
                .spacing(8)
            } else {
                row![
                    p2_regular("Market Price").style(theme::text::secondary),
                    p2_bold(order.premium_text()).style(premium_style)
                ]
                .spacing(8)
            },
        ]
        .spacing(8),
    )
    .width(Length::Fill);

    // Payment method section with icon
    let payment_card = card::simple(
        row![
            icon::cash_icon().style(theme::text::secondary),
            column![
                p2_regular("Payment Method").style(theme::text::secondary),
                p1_bold(order.payment_methods.join(", ")),
            ]
            .spacing(4),
        ]
        .spacing(12)
        .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fill);

    // Created time with icon + time remaining
    let expires_at = order.created_at_ts + 86400; // 24 hours
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let created_date = {
        let dt = chrono::DateTime::from_timestamp(order.created_at_ts as i64, 0)
            .unwrap_or_default()
            .with_timezone(&chrono::Local);
        dt.format("%b %d, %Y %H:%M").to_string()
    };

    let time_card = card::simple(
        row![
            icon::clock_icon().style(theme::text::secondary),
            column![
                p2_regular("Created On").style(theme::text::secondary),
                p1_bold(format!("Created on: {}", created_date)),
            ]
            .spacing(4),
        ]
        .spacing(12)
        .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fill);

    // Order ID with icon and copy button
    let order_id_card = card::simple(
        row![
            icon::clipboard_icon().style(theme::text::secondary),
            column![
                p2_regular("Order ID").style(theme::text::secondary),
                row![
                    p2_regular(&order.id).style(theme::text::secondary),
                    Space::new().width(Length::Fill),
                    button::secondary_compact(None, "Copy").on_press(view::Message::P2P(
                        P2PMessage::CopyOrderId(order.id.clone(),)
                    )),
                ]
                .spacing(8)
                .align_y(iced::alignment::Vertical::Center),
            ]
            .spacing(4)
            .width(Length::Fill),
        ]
        .spacing(12)
        .align_y(iced::alignment::Vertical::Center),
    )
    .width(Length::Fill);

    // Creator reputation — three-column centered layout
    let reputation_card = card::simple(
        column![
            p2_regular("Creator Reputation").style(theme::text::secondary),
            if let (Some(rating), Some(reviews), Some(days)) = (
                order.seller_rating,
                order.seller_reviews,
                order.seller_days_old
            ) {
                row![
                    column![
                        row![
                            icon::star_fill_icon().style(theme::text::secondary),
                            p1_bold(format!("{:.1}", rating)),
                        ]
                        .spacing(4)
                        .align_y(iced::alignment::Vertical::Center),
                        caption("Rating").style(theme::text::secondary),
                    ]
                    .spacing(4)
                    .align_x(Alignment::Center)
                    .width(Length::Fill),
                    column![
                        row![
                            icon::person_icon().style(theme::text::secondary),
                            p1_bold(format!("{}", reviews)),
                        ]
                        .spacing(4)
                        .align_y(iced::alignment::Vertical::Center),
                        caption("Reviews").style(theme::text::secondary),
                    ]
                    .spacing(4)
                    .align_x(Alignment::Center)
                    .width(Length::Fill),
                    column![
                        row![
                            icon::calendar_icon().style(theme::text::secondary),
                            p1_bold(format!("{}", days)),
                        ]
                        .spacing(4)
                        .align_y(iced::alignment::Vertical::Center),
                        caption("Days").style(theme::text::secondary),
                    ]
                    .spacing(4)
                    .align_x(Alignment::Center)
                    .width(Length::Fill),
                ]
                .spacing(12)
            } else {
                row![p2_regular("No rating available").style(theme::text::secondary)]
            },
        ]
        .spacing(12),
    )
    .width(Length::Fill);

    // Action buttons + info note
    let mut detail_col = column![
        amount_card,
        payment_card,
        time_card,
        order_id_card,
        reputation_card,
    ]
    .spacing(12);

    // Countdown timer for pending orders (24h expiry)
    if expires_at > now {
        let remaining = expires_at - now;
        let hours = remaining / 3600;
        let minutes = (remaining % 3600) / 60;
        let seconds = remaining % 60;
        detail_col = detail_col.push(
            container(
                column![
                    icon::clock_icon().size(24).style(theme::text::secondary),
                    p1_bold(format!("{:02}:{:02}:{:02}", hours, minutes, seconds)),
                    caption("Time remaining").style(theme::text::secondary),
                ]
                .spacing(4)
                .align_x(Alignment::Center),
            )
            .width(Length::Fill)
            .center_x(Length::Fill)
            .padding([12, 0]),
        );
    }

    if order.is_mine {
        detail_col = detail_col.push(
            column![
                p2_regular("This order will be published for 24 hours.")
                    .style(theme::text::secondary),
                p2_regular("You can cancel it anytime before someone takes it.")
                    .style(theme::text::secondary),
            ]
            .spacing(4),
        );

        let close_btn = button::secondary(None, "Close")
            .on_press(view::Message::P2P(P2PMessage::CloseOrderDetail))
            .width(Length::Fill);
        let cancel_btn = button::alert(None, "Cancel Order")
            .on_press(view::Message::P2P(P2PMessage::CancelOrder(
                order.id.clone(),
            )))
            .width(Length::Fill);
        detail_col = detail_col.push(row![close_btn, cancel_btn].spacing(8));
    } else {
        // Is user buying? (taking a sell order)
        let is_buying = order.order_type == OrderType::Sell;

        if let Some(state) = take_state {
            // Inline take order fields
            if order.is_range_order() {
                detail_col = detail_col.push(
                    column![
                        p2_regular(format!(
                            "Enter fiat amount ({} - {} {})",
                            format_f64_as_string(order.min_amount.unwrap_or(0.0), ",", 0, false),
                            format_f64_as_string(order.max_amount.unwrap_or(0.0), ",", 0, false),
                            order.fiat_currency
                        ))
                        .style(theme::text::secondary),
                        form::Form::new_amount_numeric("Fiat amount", state.amount, |v| {
                            view::Message::P2P(P2PMessage::TakeOrderAmountEdited(v))
                        })
                        .padding(10),
                    ]
                    .spacing(8),
                );
            }

            if is_buying {
                detail_col = detail_col.push(
                    column![
                        p2_regular("Lightning invoice or address (optional)")
                            .style(theme::text::secondary),
                        form::Form::new_trimmed(
                            "Enter lightning invoice or address",
                            state.invoice,
                            |v| view::Message::P2P(P2PMessage::TakeOrderInvoiceEdited(v)),
                        )
                        .padding(10),
                    ]
                    .spacing(8),
                );
            }

            let can_confirm = if order.is_range_order() {
                state
                    .amount
                    .value
                    .parse::<f64>()
                    .map(|v| {
                        let min = order.min_amount.unwrap_or(0.0);
                        let max = order.max_amount.unwrap_or(f64::MAX);
                        v >= min && v <= max
                    })
                    .unwrap_or(false)
            } else {
                true
            };

            let action_label = if is_buying { "Buy" } else { "Sell" };
            let confirm_btn = if can_confirm && !state.submitting {
                button::primary(None, action_label)
                    .on_press(view::Message::P2P(P2PMessage::ConfirmTakeOrder))
                    .width(Length::Fill)
            } else {
                button::primary(None, action_label).width(Length::Fill)
            };

            detail_col = detail_col.push(
                row![
                    button::secondary(None, "Close")
                        .on_press(view::Message::P2P(P2PMessage::CloseOrderDetail))
                        .width(Length::Fill),
                    confirm_btn,
                ]
                .spacing(8),
            );
        } else {
            // Initial state: show Buy/Sell button that activates take order fields
            let close_btn = button::secondary(None, "Close")
                .on_press(view::Message::P2P(P2PMessage::CloseOrderDetail))
                .width(Length::Fill);
            let action_label = if is_buying { "Buy" } else { "Sell" };
            let action_btn = button::primary(None, action_label)
                .on_press(view::Message::P2P(P2PMessage::TakeOrder))
                .width(Length::Fill);
            detail_col = detail_col.push(row![close_btn, action_btn].spacing(8));
        }
    }

    container(detail_col).width(Length::Fill)
}

#[cfg(test)]
mod book_tests {
    use super::*;

    fn order(order_type: OrderType) -> P2POrder {
        P2POrder {
            id: "order-1".into(),
            order_type,
            fiat_amount: 100.0,
            fiat_currency: "EUR".into(),
            min_amount: Some(50.0),
            max_amount: Some(200.0),
            sats_amount: None,
            premium_percent: Some(2.0),
            payment_methods: vec!["SEPA instant".into()],
            seller_rating: None,
            seller_reviews: None,
            seller_days_old: Some(3),
            created_at: String::new(),
            created_at_ts: 0,
            time_ago: String::new(),
            is_mine: false,
        }
    }

    #[test]
    fn book_labels_taker_side_and_only_displays_known_values() {
        let mut sell = order(OrderType::Sell);
        assert_eq!(book_side_label(&sell), "BUYING");
        assert_eq!(book_amount(&sell), "50 – 200 EUR");
        assert_eq!(book_price(&sell), "Market +2%");
        assert_eq!(book_reputation(&sell), "No rating · 3 days");

        let mut buy = order(OrderType::Buy);
        buy.sats_amount = Some(450_000);
        buy.seller_rating = Some(4.8);
        assert_eq!(book_side_label(&buy), "SELLING");
        assert_eq!(book_price(&buy), "Fixed · 450,000 sats");
        assert_eq!(book_reputation(&buy), "★ 4.8 · 3 days");

        buy.is_mine = true;
        assert_eq!(book_side_label(&buy), "YOU ARE BUYING");
        sell.is_mine = true;
        assert_eq!(book_side_label(&sell), "YOU ARE SELLING");
    }
}
