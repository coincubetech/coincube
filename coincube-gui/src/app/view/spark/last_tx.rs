//! Shared "Last Transactions" section for the Spark Send and Receive
//! panels. Produces the same row layout as the Spark Overview / Spark
//! Transactions pages so users see a consistent list across the wallet.

use coincube_ui::component::text::Text;
use coincube_ui::locale::copy as localized_copy;
use coincube_ui::{
    color,
    component::{
        amount::{BitcoinDisplayUnit, DisplayAmount},
        text::*,
        transaction::{TransactionDirection, TransactionListItem},
    },
    icon::{self, receipt_icon},
    image::asset_network_logo,
    theme,
    widget::*,
};
use iced::{
    widget::{button as iced_button, container, Column, Container, Row, Space},
    Background, Length,
};

#[cfg(test)]
use coincube_core::miniscript::bitcoin::Amount;
use coincube_ui::component::empty_placeholder;

use crate::app::view::spark::{SparkPaymentMethod, SparkRecentTransaction};
use crate::app::wallets::DomainPaymentStatus;

pub fn last_transactions_section<'a, M: 'a + Clone + 'static>(
    recent: &'a [SparkRecentTransaction],
    bitcoin_unit: BitcoinDisplayUnit,
    show_direction_badges: bool,
    mask_amounts: bool,
    on_select: impl Fn(usize) -> M + 'a,
    on_history: M,
) -> Element<'a, M> {
    let mut content = Column::new()
        .spacing(10)
        .push(h4_bold(localized_copy("Last transactions")));

    if recent.is_empty() {
        content = content.push(empty_placeholder(
            receipt_icon().size(80),
            localized_copy("No transactions yet"),
            localized_copy(
                "Your transaction history will appear here once you send or receive coins.",
            ),
        ));
        return content.into();
    }

    for (idx, tx) in recent.iter().enumerate() {
        let direction = if tx.is_incoming {
            TransactionDirection::Incoming
        } else {
            TransactionDirection::Outgoing
        };

        let tx_icon: Element<'_, M> = match tx.method {
            SparkPaymentMethod::Lightning => asset_network_logo("btc", "lightning", 40.0),
            SparkPaymentMethod::OnChainBitcoin => asset_network_logo("btc", "bitcoin", 40.0),
            SparkPaymentMethod::Spark => asset_network_logo("btc", "spark", 40.0),
            SparkPaymentMethod::StableBalance => asset_network_logo("btc", "spark", 40.0),
        };

        let display_amount = tx.display_amount();

        let mut item = TransactionListItem::new(direction, &display_amount, bitcoin_unit)
            .with_custom_icon(tx_icon)
            .with_show_direction_badge(show_direction_badges)
            .with_label(tx.description.clone())
            .with_time_ago(tx.time_ago.clone());
        if mask_amounts {
            item = item.with_amount_override("••••••".into());
        }

        if let Some(token_str) = tx.token_display.as_ref().filter(|_| !mask_amounts) {
            item = item.with_amount_override(token_str.clone());
        }
        if let Some(fiat) = tx.fiat_amount.as_ref().filter(|_| !mask_amounts) {
            item = item.with_fiat_amount(format!(
                "{} {}",
                fiat.to_formatted_string(),
                fiat.currency()
            ));
        }

        if matches!(tx.status, DomainPaymentStatus::Pending) {
            let (bg, fg) = (color::GREY_3, color::BLACK);
            let pending_badge = Container::new(
                Row::new()
                    .push(
                        icon::warning_icon()
                            .size(14)
                            .style(move |_| iced::widget::text::Style { color: Some(fg) }),
                    )
                    .push(
                        text(localized_copy("Pending"))
                            .bold()
                            .size(14)
                            .style(move |_| iced::widget::text::Style { color: Some(fg) }),
                    )
                    .spacing(4),
            )
            .padding([2, 8])
            .style(move |_| iced::widget::container::Style {
                background: Some(iced::Background::Color(bg)),
                border: iced::Border {
                    radius: 12.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });
            item = item.with_custom_status(pending_badge.into());
        }

        content = content.push(item.view(on_select(idx)));
    }

    let view_all_button = {
        let the_icon = icon::history_icon()
            .size(18)
            .style(|_theme: &theme::Theme| iced::widget::text::Style {
                color: Some(color::ORANGE),
            });

        let label = text(localized_copy("View All Transactions"))
            .size(15)
            .style(|_theme: &theme::Theme| iced::widget::text::Style {
                color: Some(color::ORANGE),
            });

        let button_content = Row::new()
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center)
            .push(the_icon)
            .push(label);

        iced_button(Container::new(button_content).padding([10, 20]).style(
            |_theme: &theme::Theme| container::Style {
                background: Some(Background::Color(color::TRANSPARENT)),
                border: iced::Border {
                    color: color::ORANGE,
                    width: 1.5,
                    radius: 8.0.into(),
                },
                ..Default::default()
            },
        ))
        .style(|_theme: &theme::Theme, _| iced_button::Style {
            background: Some(Background::Color(color::TRANSPARENT)),
            text_color: color::ORANGE,
            border: iced::Border {
                radius: 8.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .on_press(on_history)
    };

    Column::new()
        .spacing(10)
        .push(content)
        .push(Space::new().height(Length::Fixed(20.0)))
        .push(
            Container::new(view_all_button)
                .width(Length::Fill)
                .center_x(Length::Fill),
        )
        .push(Space::new().height(Length::Fixed(40.0)))
        .into()
}

/// Rows the Send and Receive rail lists before "See all in Activity".
pub const RAIL_RECENT_ROWS: usize = 5;

/// "Recent in Spark" for the Send and Receive rail: the same payments as
/// [`last_transactions_section`], in rows narrow enough for the 352px rail
/// (coin, label and time, amount and fiat), then a link to Activity.
pub fn recent_rail_section<'a, M: 'a + Clone + 'static>(
    recent: &'a [SparkRecentTransaction],
    bitcoin_unit: BitcoinDisplayUnit,
    mask_amounts: bool,
    on_select: impl Fn(usize) -> M + 'a,
    on_history: M,
) -> Element<'a, M> {
    let see_all = iced_button(text(localized_copy("See all in Activity")).size(P2_SIZE))
        .padding([4, 0])
        .style(|theme: &theme::Theme, status| {
            let mut style = theme::button::link(theme, status);
            style.text_color = theme.colors.shell.accent_ink;
            style
        })
        .on_press(on_history);
    let mut list = Column::new().push(
        Row::new()
            .spacing(12)
            .align_y(iced::Alignment::Center)
            .push(p1_bold(localized_copy("Recent in Spark")).width(Length::Fill))
            .push(see_all),
    );
    if recent.is_empty() {
        return list
            .spacing(8)
            .push(p2_regular(localized_copy("No transactions yet.")).style(theme::text::secondary))
            .into();
    }
    for (idx, tx) in recent.iter().take(RAIL_RECENT_ROWS).enumerate() {
        let logo: Element<'_, M> = match tx.method {
            SparkPaymentMethod::Lightning => asset_network_logo("btc", "lightning", 30.0),
            SparkPaymentMethod::OnChainBitcoin => asset_network_logo("btc", "bitcoin", 30.0),
            SparkPaymentMethod::Spark | SparkPaymentMethod::StableBalance => {
                asset_network_logo("btc", "spark", 30.0)
            }
        };
        let figure = if mask_amounts {
            "••••••".to_owned()
        } else if let Some(token) = tx.token_display.as_ref() {
            token.clone()
        } else {
            format!(
                "{} {}",
                tx.display_amount()
                    .to_formatted_string_with_unit(bitcoin_unit),
                if matches!(bitcoin_unit, BitcoinDisplayUnit::BTC) {
                    "BTC"
                } else {
                    "sats"
                }
            )
        };
        let sign = if tx.is_incoming { "+ " } else { "− " };
        let when = if matches!(tx.status, DomainPaymentStatus::Pending) {
            format!("{} · {}", tx.time_ago, localized_copy("Pending"))
        } else {
            tx.time_ago.clone()
        };
        let fiat = tx
            .fiat_amount
            .as_ref()
            .filter(|_| !mask_amounts)
            .map(|fiat| format!("{} {}", fiat.to_formatted_string(), fiat.currency()));
        let row = Row::new()
            .spacing(12)
            .align_y(iced::Alignment::Center)
            .push(logo)
            .push(
                Column::new()
                    .spacing(2)
                    .width(Length::Fill)
                    .push(p2_regular(tx.description.clone()))
                    .push(caption(when).style(theme::text::secondary)),
            )
            .push(
                Column::new()
                    .spacing(2)
                    .align_x(iced::Alignment::End)
                    .push(p2_regular(format!("{sign}{figure}")))
                    .push_maybe(fiat.map(|fiat| caption(fiat).style(theme::text::secondary))),
            );
        list = list.push(iced::widget::rule::horizontal(1)).push(
            iced_button(row)
                .padding([10, 0])
                .width(Length::Fill)
                .style(theme::button::transparent)
                .on_press(on_select(idx)),
        );
    }
    list.spacing(6).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::view::shared::landing::capture;

    #[test]
    fn hidden_rail_row_never_mounts_btc_token_or_fiat_amount() {
        let mut payment = tx(true, 98_765_432, 0);
        payment.token_display = Some("123456.78 USDB".into());
        let mounted = capture::inspect(
            "spark-hidden-rail-recent",
            recent_rail_section(&[payment], BitcoinDisplayUnit::Sats, true, |_| 1_u8, 2_u8),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(mounted.contains("Recent in Spark"));
        assert!(mounted.contains("••••••"));
        assert!(!mounted.contains("98,765,432"));
        assert!(!mounted.contains("123456.78"));
    }

    fn tx(is_incoming: bool, amount_sat: u64, fees_sat: u64) -> SparkRecentTransaction {
        SparkRecentTransaction {
            id: "payment".to_string(),
            description: "Spark transfer".to_string(),
            time_ago: "just now".to_string(),
            timestamp: 1_700_000_000,
            amount: Amount::from_sat(amount_sat),
            fees_sat: Amount::from_sat(fees_sat),
            fiat_amount: None,
            is_incoming,
            status: DomainPaymentStatus::Complete,
            method: SparkPaymentMethod::Spark,
            token_display: None,
        }
    }

    #[test]
    fn row_display_amount_keeps_incoming_net_amount() {
        assert_eq!(tx(true, 5_000, 21).display_amount().to_sat(), 5_000);
    }

    #[test]
    fn row_display_amount_adds_outgoing_fees() {
        assert_eq!(tx(false, 5_000, 21).display_amount().to_sat(), 5_021);
    }

    #[test]
    fn hidden_last_transaction_never_mounts_btc_token_or_fiat_amount() {
        let mut payment = tx(true, 98_765_432, 0);
        payment.token_display = Some("123456.78 USDB".into());
        let mounted = capture::inspect(
            "spark-hidden-last-transaction",
            last_transactions_section(
                &[payment],
                BitcoinDisplayUnit::Sats,
                true,
                true,
                |_| 1_u8,
                2_u8,
            ),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(mounted.contains("••••••"));
        assert!(!mounted.contains("98,765,432"));
        assert!(!mounted.contains("123456.78"));
    }
}
