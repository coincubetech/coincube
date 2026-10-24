use coincube_core::miniscript::bitcoin::Amount;

use crate::app::wallets::{DomainPayment, DomainPaymentStatus};
use coincube_ui::{
    color,
    component::{amount::*, text::*},
    icon::{self, receipt_icon},
    theme,
    widget::Element,
};
use iced::{
    widget::{container, Column, Container, Row},
    Alignment, Length,
};

use crate::app::breez_liquid::assets::format_usdt_display;
use crate::app::settings::display::DisplayMode;
use crate::app::view::shared::activity_row::{
    activity_row, ActivityDirection, ActivityIdentity, ActivityRow,
};
use crate::app::view::shared::landing::{
    amount_cell, landing_balance_section, landing_button, landing_control_id,
    landing_subtle_button_style, AmountCell, AmountTier,
};
use crate::app::view::wallet_header::{wallet_header, HeaderVariant, SyncState, WalletHeaderProps};
use crate::app::view::{liquid::RecentTransaction, FiatAmountConverter, LiquidOverviewMessage};

fn payment_display_time(payment: Option<&DomainPayment>) -> (String, Option<String>) {
    // Pending swap timestamps are creation times. Direct unconfirmed Liquid
    // transfers can instead carry the SDK's synthesized observation time.
    let historical_date = payment
        .filter(|payment| {
            payment.timestamp > 0
                && (payment.status == DomainPaymentStatus::Complete
                    || !matches!(
                        &payment.details,
                        crate::app::wallets::DomainPaymentDetails::LiquidAsset { .. }
                    ))
        })
        .and_then(|payment| {
            chrono::DateTime::<chrono::Utc>::from_timestamp(i64::from(payment.timestamp), 0)
        })
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%b %-d, %H:%M")
                .to_string()
        });
    match historical_date {
        Some(date) => (date, Some("Local time".into())),
        None => (
            match payment.map(|payment| payment.status) {
                Some(
                    DomainPaymentStatus::Created
                    | DomainPaymentStatus::Pending
                    | DomainPaymentStatus::RefundPending
                    | DomainPaymentStatus::WaitingFeeAcceptance,
                ) => "Time pending",
                _ => "Time unknown",
            }
            .into(),
            None,
        ),
    }
}

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
                on_swap_focus: Some((
                    landing_control_id(cube_id, "Liquid", "flip-balance"),
                    LiquidOverviewMessage::FocusAction,
                )),
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
                format!(
                    "{} {}",
                    btc_balance.to_formatted_string_with_unit(bitcoin_unit),
                    bitcoin_unit.label()
                ),
                (!lbtc_fiat_str.is_empty()).then_some(lbtc_fiat_str),
            ),
            AmountTier::Row,
        ))
        .push(iced::widget::Space::new().width(Length::Fill))
        .push(
            landing_button(
                landing_control_id(cube_id, "Liquid", "send-lbtc"),
                "Send",
                None,
                LiquidOverviewMessage::FocusAction,
                Some(LiquidOverviewMessage::SendLbtc),
                theme::button::primary,
            )
            .width(Length::Fixed(90.0)),
        )
        .push(
            landing_button(
                landing_control_id(cube_id, "Liquid", "receive-lbtc"),
                "Receive",
                None,
                LiquidOverviewMessage::FocusAction,
                Some(LiquidOverviewMessage::ReceiveLbtc),
                theme::button::orange_outline,
            )
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
            landing_button(
                landing_control_id(cube_id, "Liquid", "send-usdt"),
                "Send",
                None,
                LiquidOverviewMessage::FocusAction,
                Some(LiquidOverviewMessage::SendUsdt),
                theme::button::primary,
            )
            .width(Length::Fixed(90.0)),
        )
        .push(
            landing_button(
                landing_control_id(cube_id, "Liquid", "receive-usdt"),
                "Receive",
                None,
                LiquidOverviewMessage::FocusAction,
                Some(LiquidOverviewMessage::ReceiveUsdt),
                theme::button::orange_outline,
            )
            .width(Length::Fixed(90.0)),
        );

    // Cross-asset swap entry point. SideSwap is mainnet-only, so the
    // button is only shown where `swap_supported` (the Overview's mirror
    // of `cross_asset_supported`) holds.
    let mut portfolio_col = Column::new()
        .spacing(16)
        .push(total_col)
        .push(iced::widget::rule::horizontal(1))
        .push(lbtc_row)
        .push(iced::widget::rule::horizontal(1))
        .push(usdt_row);
    if swap_supported {
        portfolio_col = portfolio_col.push(
            Container::new(
                landing_button(
                    landing_control_id(cube_id, "Liquid", "swap"),
                    "Swap",
                    Some(icon::arrow_down_up_icon()),
                    LiquidOverviewMessage::FocusAction,
                    Some(LiquidOverviewMessage::Swap),
                    landing_subtle_button_style,
                )
                .width(Length::Fixed(100.0)),
            )
            .width(Length::Fill)
            .center_x(Length::Fill),
        );
    }

    let balance_section = landing_balance_section(portfolio_col);

    content = content.push(balance_section);

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
            let (date, timezone_tooltip) = payment_display_time(source);
            let amount = tx.usdt_display.clone().unwrap_or_else(|| {
                format!(
                    "{} {}",
                    (if tx.is_incoming {
                        tx.amount
                    } else {
                        tx.amount + tx.fees_sat
                    })
                    .to_formatted_string_with_unit(bitcoin_unit),
                    bitcoin_unit.label()
                )
            });
            let amount = if tx.is_incoming {
                format!("+{amount}")
            } else {
                format!("−{amount}")
            };
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
                    timezone_tooltip,
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
                LiquidOverviewMessage::FocusAction,
            ));
        }
    } else {
        content = content.push(placeholder(
            receipt_icon().size(80),
            "No transactions yet",
            "Your transaction history will appear here once you send or receive coins.",
        ));
    }

    let view_transactions_button = landing_button(
        landing_control_id(cube_id, "Liquid", "history"),
        "View All Transactions",
        None,
        LiquidOverviewMessage::FocusAction,
        Some(LiquidOverviewMessage::History),
        landing_subtle_button_style,
    );

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

    Container::new(content).padding([0, 16]).into()
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

#[cfg(test)]
mod repair_tests {
    use super::{liquid_overview_view, payment_display_time};
    use crate::app::settings::display::DisplayMode;
    use crate::app::view::liquid::RecentTransaction;
    use crate::app::view::shared::landing::{capture, landing_control_id};
    use crate::app::wallets::{
        DomainPayment, DomainPaymentDetails, DomainPaymentDirection, DomainPaymentStatus,
    };
    use coincube_core::miniscript::bitcoin::Amount;
    use coincube_ui::{component::amount::BitcoinDisplayUnit, theme};

    fn payment(
        details: DomainPaymentDetails,
        status: DomainPaymentStatus,
        timestamp: u32,
    ) -> DomainPayment {
        DomainPayment {
            tx_id: None,
            destination: None,
            timestamp,
            amount_sat: 100,
            fees_sat: 0,
            direction: DomainPaymentDirection::Receive,
            status,
            details,
        }
    }

    fn swap() -> DomainPaymentDetails {
        DomainPaymentDetails::Lightning {
            swap_id: Some("swap-1".into()),
            description: "Swap".into(),
            payer_note: None,
        }
    }

    fn direct() -> DomainPaymentDetails {
        DomainPaymentDetails::LiquidAsset {
            asset_id: "asset".into(),
            asset_info: None,
            description: "Direct".into(),
            payer_note: None,
        }
    }

    #[test]
    fn known_swap_creation_times_survive_noncomplete_statuses() {
        for status in [
            DomainPaymentStatus::Pending,
            DomainPaymentStatus::Failed,
            DomainPaymentStatus::TimedOut,
            DomainPaymentStatus::Refundable,
            DomainPaymentStatus::RefundPending,
        ] {
            let swap = payment(swap(), status, 1_722_000_000);
            let (date, tooltip) = payment_display_time(Some(&swap));
            assert_ne!(date, "Time pending");
            assert_ne!(date, "Time unknown");
            assert_eq!(tooltip.as_deref(), Some("Local time"));
        }
    }

    #[test]
    fn direct_unconfirmed_and_missing_times_never_claim_history() {
        let pending_direct = payment(direct(), DomainPaymentStatus::Pending, 1_722_000_000);
        assert_eq!(
            payment_display_time(Some(&pending_direct)),
            ("Time pending".into(), None)
        );
        let failed_direct = payment(direct(), DomainPaymentStatus::Failed, 1_722_000_000);
        assert_eq!(
            payment_display_time(Some(&failed_direct)),
            ("Time unknown".into(), None)
        );
        let pending_swap_zero = payment(swap(), DomainPaymentStatus::Pending, 0);
        assert_eq!(
            payment_display_time(Some(&pending_swap_zero)),
            ("Time pending".into(), None)
        );
        let complete_direct = payment(direct(), DomainPaymentStatus::Complete, 1_722_000_000);
        assert_eq!(
            payment_display_time(Some(&complete_direct)).1.as_deref(),
            Some("Local time")
        );
    }

    fn recent(payment: &DomainPayment) -> RecentTransaction {
        RecentTransaction {
            description: payment.details.description().to_owned(),
            time_ago: "just now".into(),
            amount: Amount::from_sat(payment.amount_sat),
            fees_sat: Amount::from_sat(payment.fees_sat),
            fiat_amount: None,
            is_incoming: payment.is_incoming(),
            status: payment.status,
            details: payment.details.clone(),
            usdt_display: None,
        }
    }

    #[test]
    fn r06a_liquid_headless_content_matrix() {
        let mut known_swap = payment(swap(), DomainPaymentStatus::Pending, 1_722_000_000);
        known_swap.amount_sat = 4_200;
        let mut direct_pending = payment(direct(), DomainPaymentStatus::Pending, 1_722_000_000);
        direct_pending.tx_id = Some("direct-tx".into());
        let payments = vec![known_swap, direct_pending];
        let recent_rows = payments.iter().map(recent).collect::<Vec<_>>();
        let adverse_payments = [
            DomainPaymentStatus::Created,
            DomainPaymentStatus::Pending,
            DomainPaymentStatus::Complete,
            DomainPaymentStatus::Failed,
            DomainPaymentStatus::TimedOut,
            DomainPaymentStatus::Refundable,
            DomainPaymentStatus::RefundPending,
            DomainPaymentStatus::WaitingFeeAcceptance,
        ]
        .iter()
        .enumerate()
        .map(|(index, status)| {
            let mut p = payment(swap(), *status, 1_722_000_000);
            p.details = DomainPaymentDetails::Lightning {
                swap_id: Some(format!("swap-{index}")),
                description: "Long Liquid swap description with a beneficiary and note that should wrap safely on the compact landing".into(),
                payer_note: None,
            };
            p.amount_sat = 99_999_999_999;
            p
        })
        .collect::<Vec<_>>();
        let adverse_recent = adverse_payments.iter().map(recent).collect::<Vec<_>>();
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for outer in [(1024, 768), (1440, 900)] {
                let view = |rows: &[RecentTransaction],
                            sources: &[DomainPayment],
                            quote: Option<f64>,
                            error| {
                    let converter = quote.map(|price| {
                        super::FiatAmountConverter::new(
                            price,
                            Some(1_722_000_000),
                            crate::app::cache::FiatPriceRequest::new(
                                crate::services::fiat::PriceSource::Coincube,
                                crate::services::fiat::Currency::USD,
                            ),
                        )
                        .expect("positive synthetic BTC/USD quote")
                    });
                    liquid_overview_view(
                        Amount::from_sat(125_000),
                        250_000_000,
                        converter,
                        rows,
                        sources,
                        error,
                        true,
                        false,
                        "synthetic-cube",
                        BitcoinDisplayUnit::Sats,
                        quote,
                        true,
                        DisplayMode::FiatNative,
                        true,
                    )
                };
                let normal = capture::inspect(
                    "liquid-normal",
                    view(&recent_rows, &payments, Some(50_000.0), None),
                    outer,
                    appearance,
                    theme,
                );
                assert!(normal.contains("Last transactions"));
                assert!(normal.contains("Time pending")); // direct unconfirmed only
                assert!(normal.contains("USDt"));
                assert!(normal.contains("65.00"));
                assert!(normal.contains("130,000"));
                assert!(normal.contains("SATS"));
                for control in [
                    "flip-balance",
                    "send-lbtc",
                    "receive-lbtc",
                    "send-usdt",
                    "receive-usdt",
                    "swap",
                    "history",
                ] {
                    assert!(normal.has_focus(landing_control_id(
                        "synthetic-cube",
                        "Liquid",
                        control
                    )));
                }

                let no_quote = capture::inspect(
                    "liquid-missing-quote-retained-error",
                    view(&recent_rows, &payments, None, Some("refresh failed")),
                    outer,
                    appearance,
                    theme,
                );
                assert!(no_quote.contains("2.50 USDt"));
                assert!(no_quote.contains("needs a BTC/USD quote"));
                assert!(no_quote.contains("Last known data"));
                assert!(!no_quote.contains("130,000 SATS"));

                let empty = capture::inspect(
                    "liquid-loaded-empty",
                    view(&[], &[], Some(50_000.0), None),
                    outer,
                    appearance,
                    theme,
                );
                assert!(empty.contains("No transactions yet"));
                assert!(empty.contains("View All Transactions"));

                let statuses = capture::inspect(
                    "liquid-all-statuses-long-labels",
                    view(&adverse_recent, &adverse_payments, Some(50_000.0), None),
                    outer,
                    appearance,
                    theme,
                );
                assert!(statuses.contains("Refund available"));
                assert!(statuses.contains("Review fee"));
            }
        }
    }
}
