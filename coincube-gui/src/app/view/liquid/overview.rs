use coincube_core::miniscript::bitcoin::Amount;

use crate::app::wallets::{DomainPayment, DomainPaymentStatus};
use coincube_ui::{
    color,
    component::{amount::*, button, text::*},
    icon::{self, receipt_icon},
    theme,
    widget::Element,
};
use iced::{
    widget::{button as iced_button, container, Column, Container, Row},
    Alignment, Background, Length,
};

use crate::app::breez_liquid::assets::format_usdt_display;
use crate::app::settings::display::DisplayMode;
use crate::app::view::shared::activity_row::{
    activity_row, ActivityDirection, ActivityIdentity, ActivityRow,
};
use crate::app::view::shared::landing::{amount_cell, AmountCell, AmountTier};
use crate::app::view::wallet_header::{wallet_header, HeaderVariant, SyncState, WalletHeaderProps};
use crate::app::view::{liquid::RecentTransaction, FiatAmountConverter, LiquidOverviewMessage};

#[allow(clippy::too_many_arguments)]
pub fn liquid_overview_view<'a>(
    btc_balance: Amount,
    usdt_balance: u64,
    fiat_converter: Option<FiatAmountConverter>,
    recent_transaction: &[RecentTransaction],
    recent_payments: &[DomainPayment],
    error: Option<&'a str>,
    has_loaded_snapshot: bool,
    loading: bool,
    cube_id: &str,
    bitcoin_unit: BitcoinDisplayUnit,
    btc_usd_price: Option<f64>,
    _show_direction_badges: bool,
    display_mode: DisplayMode,
    // Whether cross-asset swaps (SideSwap) are available on this network.
    // Drives the Swap button — hidden off-mainnet where SideSwap can't run.
    swap_supported: bool,
) -> Element<'a, LiquidOverviewMessage> {
    if !has_loaded_snapshot {
        let status = error.unwrap_or(if loading {
            "Loading Liquid balances and activity…"
        } else {
            "Liquid balance has not loaded yet"
        });
        return Column::new()
            .spacing(12)
            .push(h2("Liquid"))
            .push(text(status).size(P1_SIZE).style(theme::text::secondary))
            .into();
    }
    let mut content = Column::new().spacing(20);

    let btc_fiat = fiat_converter.as_ref().map(|c| c.convert(btc_balance));

    // Only sum BTC pending amounts; USDt transactions have usdt_display set
    // and their amount field holds USDt base units, not BTC sats.
    let pending_outgoing_sats: u64 = recent_transaction
        .iter()
        .filter(|t| {
            !t.is_incoming
                && t.usdt_display.is_none()
                && matches!(t.status, DomainPaymentStatus::Pending)
        })
        .map(|t| (t.amount + t.fees_sat).to_sat())
        .sum();

    let pending_incoming_sats: u64 = recent_transaction
        .iter()
        .filter(|t| {
            t.is_incoming
                && t.usdt_display.is_none()
                && matches!(t.status, DomainPaymentStatus::Pending)
        })
        .map(|t| t.amount.to_sat())
        .sum();

    // ── Unified portfolio card ─────────────────────────────────────────────
    // USDt is pegged to USD, so always use BTC/USD price for conversion.
    let usable_usd_quote = btc_usd_price.filter(|price| *price > 0.0 && price.is_finite());
    let usdt_fiat_value = usdt_balance as f64 / 1e8; // USDt base units → dollars
    let usdt_as_sats = usable_usd_quote
        .map(|btc_price_usd| (usdt_fiat_value / btc_price_usd * 1e8) as u64)
        .unwrap_or(0);
    let total_sats = btc_balance.to_sat().saturating_add(usdt_as_sats);
    let total_balance = Amount::from_sat(total_sats);
    let unvalued_usdt = usdt_balance > 0 && usable_usd_quote.is_none();
    let total_fiat = (!unvalued_usdt)
        .then(|| fiat_converter.as_ref().map(|c| c.convert(total_balance)))
        .flatten();

    // Total balance header
    let total_col = Column::new()
        .spacing(4)
        .push(h4_bold("Balance"))
        .push(if unvalued_usdt {
            amount_cell::<LiquidOverviewMessage>(
                AmountCell::unavailable("Combined L-BTC and USDt valuation needs a BTC/USD quote"),
                AmountTier::Card,
            )
        } else {
            wallet_header::<LiquidOverviewMessage>(WalletHeaderProps {
                sats: total_balance,
                fiat: total_fiat,
                balance_masked: false,
                bitcoin_unit,
                variant: HeaderVariant::Landing,
                sync: SyncState::Synced,
                unconfirmed: None,
                pending_send_sats: pending_outgoing_sats,
                pending_receive_sats: pending_incoming_sats,
                display_mode,
                on_swap: Some(LiquidOverviewMessage::FlipDisplayMode),
            })
            .into()
        });

    // L-BTC asset row
    let lbtc_fiat_str = btc_fiat
        .as_ref()
        .map(|f| format!("{} {}", f.to_formatted_string(), f.currency()))
        .unwrap_or_default();
    let lbtc_row = Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push(coincube_ui::image::asset_network_logo::<
            LiquidOverviewMessage,
        >("lbtc", "liquid", 40.0))
        .push(
            text("L-BTC")
                .size(P1_SIZE)
                .bold()
                .width(Length::Fixed(60.0)),
        )
        .push(amount_cell::<LiquidOverviewMessage>(
            AmountCell::shown(
                btc_balance.to_formatted_string_with_unit(bitcoin_unit),
                (!lbtc_fiat_str.is_empty()).then_some(lbtc_fiat_str),
            ),
            AmountTier::Row,
        ))
        .push(iced::widget::Space::new().width(Length::Fill))
        .push(
            button::primary(None, "Send")
                .on_press(LiquidOverviewMessage::SendLbtc)
                .width(Length::Fixed(90.0)),
        )
        .push(
            button::orange_outline(None, "Receive")
                .on_press(LiquidOverviewMessage::ReceiveLbtc)
                .width(Length::Fixed(90.0)),
        );

    // USDt asset row
    let usdt_display = format!("{} USDt", format_usdt_display(usdt_balance));
    let usdt_row = Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push(coincube_ui::image::asset_network_logo::<
            LiquidOverviewMessage,
        >("usdt", "liquid", 40.0))
        .push(text("USDt").size(P1_SIZE).bold().width(Length::Fixed(60.0)))
        .push(amount_cell::<LiquidOverviewMessage>(
            AmountCell::shown(usdt_display, None),
            AmountTier::Row,
        ))
        .push(iced::widget::Space::new().width(Length::Fill))
        .push(
            button::primary(None, "Send")
                .on_press(LiquidOverviewMessage::SendUsdt)
                .width(Length::Fixed(90.0)),
        )
        .push(
            button::orange_outline(None, "Receive")
                .on_press(LiquidOverviewMessage::ReceiveUsdt)
                .width(Length::Fixed(90.0)),
        );

    // Cross-asset swap entry point. SideSwap is mainnet-only, so the
    // button is only shown where `swap_supported` (the Overview's mirror
    // of `cross_asset_supported`) holds.
    let mut portfolio_col = Column::new()
        .spacing(16)
        .push(total_col)
        .push(lbtc_row)
        .push(usdt_row);
    if swap_supported {
        portfolio_col = portfolio_col.push(
            Container::new(
                button::secondary(Some(icon::arrow_down_up_icon()), "Swap")
                    .on_press(LiquidOverviewMessage::Swap)
                    .width(Length::Fixed(200.0)),
            )
            .width(Length::Fill)
            .center_x(Length::Fill),
        );
    }

    let portfolio_card = Container::new(portfolio_col)
        .padding(20)
        .width(Length::Fill)
        .style(|t| container::Style {
            background: Some(Background::Color(t.colors.cards.simple.background)),
            border: iced::Border {
                color: color::ORANGE,
                width: 0.2,
                radius: 25.0.into(),
            },
            ..Default::default()
        });

    content = content.push(portfolio_card);

    content = content.push(Column::new().spacing(10).push(h4_bold("Last transactions")));

    if !recent_transaction.is_empty() {
        for (idx, tx) in recent_transaction.iter().enumerate() {
            // Both slices come from the same recent vector in the state
            // adapter. Selection resolves against the latest raw vector by
            // SDK identity, never by this display index.
            let source = recent_payments.get(idx);
            let payment_id = source.and_then(DomainPayment::stable_identity);
            let open = payment_id
                .as_ref()
                .filter(|identity| {
                    crate::app::wallets::types::find_unique_payment(recent_payments, identity)
                        .is_some()
                })
                .cloned()
                .map(LiquidOverviewMessage::SelectTransaction);
            // The SDK can report `now()` for a direct unconfirmed transfer.
            // Do not present that value as historical transaction time.
            let confirmed_date = source
                .filter(|payment| {
                    payment.status == DomainPaymentStatus::Complete && payment.timestamp > 0
                })
                .and_then(|payment| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp(i64::from(payment.timestamp), 0)
                })
                .map(|date| {
                    date.with_timezone(&chrono::Local)
                        .format("%b %-d, %H:%M")
                        .to_string()
                });
            let date = confirmed_date.clone().unwrap_or_else(|| {
                match source.map(|payment| payment.status) {
                    Some(
                        DomainPaymentStatus::Created
                        | DomainPaymentStatus::Pending
                        | DomainPaymentStatus::RefundPending
                        | DomainPaymentStatus::WaitingFeeAcceptance,
                    ) => "Time pending",
                    _ => "Time unknown",
                }
                .to_owned()
            });
            let amount = tx.usdt_display.clone().unwrap_or_else(|| {
                (if tx.is_incoming {
                    tx.amount
                } else {
                    tx.amount + tx.fees_sat
                })
                .to_formatted_string_with_unit(bitcoin_unit)
            });
            let secondary = tx.fiat_amount.as_ref().map(|fiat| {
                format!(
                    "{} {} · current rate",
                    fiat.to_formatted_string(),
                    fiat.currency()
                )
            });
            let rail = match &tx.details {
                crate::app::wallets::DomainPaymentDetails::Lightning { .. } => "Lightning",
                crate::app::wallets::DomainPaymentDetails::LiquidAsset { .. } => "Liquid",
                crate::app::wallets::DomainPaymentDetails::OnChainBitcoin { .. } => "Bitcoin swap",
            };
            content = content.push(activity_row(
                ActivityRow {
                    identity: ActivityIdentity {
                        cube: cube_id.to_owned(),
                        wallet: "Liquid".into(),
                        payment: payment_id.map(|identity| format!("{identity:?}")),
                    },
                    rail: Some(rail.into()),
                    date,
                    timezone_tooltip: confirmed_date.map(|_| "Local time".into()),
                    direction: if tx.is_incoming {
                        ActivityDirection::Incoming
                    } else {
                        ActivityDirection::Outgoing
                    },
                    label: tx.description.clone(),
                    wallet_caption: None,
                    amount: AmountCell::shown(amount, secondary),
                    status: tx.status.into(),
                },
                false,
                open,
            ));
        }
    } else {
        content = content.push(placeholder(
            receipt_icon().size(80),
            "No transactions yet",
            "Your transaction history will appear here once you send or receive coins.",
        ));
    }

    let view_transactions_button = {
        let icon = icon::history_icon()
            .size(18)
            .style(|_theme: &theme::Theme| iced::widget::text::Style {
                color: Some(color::ORANGE),
            });

        let label = text("View All Transactions")
            .size(15)
            .style(|_theme: &theme::Theme| iced::widget::text::Style {
                color: Some(color::ORANGE),
            });

        let button_content = Row::new()
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center)
            .push(icon)
            .push(label);

        iced_button(Container::new(button_content).padding([10, 20]).style(
            |_theme: &theme::Theme| container::Style {
                background: Some(Background::Color(color::TRANSPARENT)),
                border: iced::Border {
                    color: color::ORANGE,
                    width: 1.5,
                    radius: 20.0.into(),
                },
                ..Default::default()
            },
        ))
        .style(|_theme: &theme::Theme, _| iced_button::Style {
            background: Some(Background::Color(color::TRANSPARENT)),
            text_color: color::ORANGE,
            border: iced::Border {
                radius: 20.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .on_press(LiquidOverviewMessage::History)
    };

    content = content
        .push(iced::widget::Space::new().height(Length::Fixed(20.0)))
        .push(
            Container::new(view_transactions_button)
                .width(Length::Fill)
                .center_x(Length::Fill),
        )
        .push(iced::widget::Space::new().height(Length::Fixed(40.0)));

    if loading {
        content = content.push(
            text("Refreshing Liquid balances and activity…")
                .size(P2_SIZE)
                .style(theme::text::secondary),
        );
    }

    if let Some(err) = error {
        content = content.push(
            Container::new(
                text(format!("Last known data · {err}"))
                    .size(14)
                    .color(color::RED),
            )
            .padding(10)
            .style(theme::card::invalid)
            .width(Length::Fill)
            .max_width(800),
        );
    }

    content.into()
}

pub fn placeholder<'a, T: Into<Element<'a, LiquidOverviewMessage>>>(
    icon: T,
    title: &'a str,
    subtitle: &'a str,
) -> Element<'a, LiquidOverviewMessage> {
    let content = Column::new()
        .push(icon)
        .push(text(title).style(theme::text::secondary).bold())
        .push(
            text(subtitle)
                .size(P2_SIZE)
                .style(theme::text::secondary)
                .align_x(Alignment::Center),
        )
        .spacing(16)
        .align_x(Alignment::Center);

    Container::new(content)
        .width(Length::Fill)
        .padding(60)
        .center_x(Length::Fill)
        .style(|t| container::Style {
            background: Some(iced::Background::Color(t.colors.cards.simple.background)),
            border: iced::Border {
                radius: 20.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}
