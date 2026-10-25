use std::collections::HashMap;

use coincube_core::miniscript::bitcoin::Amount;
use iced::widget::image;

use crate::app::wallets::{
    DomainPayment, DomainPaymentDetails, DomainPaymentStatus, DomainRefundableSwap,
};

use coincube_ui::{
    component::{
        amount::DisplayAmount,
        button, card, form,
        pagination::pagination_controls,
        quote_display::{self, Quote, QuoteDisplayProps},
        text::*,
        transaction::{TransactionDirection, TransactionListItem},
    },
    icon, theme,
    widget::{ColumnExt, Element},
};
use iced::{
    widget::{scrollable, Column, Container, Row, Space},
    Alignment, Length,
};

use coincube_ui::image::asset_network_logo;

use crate::app::breez_liquid::assets::format_usdt_display;
use crate::app::breez_liquid::swap_status::{classify_payment, BtcSwapReceiveStatus};
use crate::app::menu::Menu;
use crate::app::state::liquid::transactions::{AssetFilter, InFlightRefund};
use crate::app::view::message::{FeeratePriority, Message};
use crate::app::view::FiatAmountConverter;
use crate::export::ImportExportMessage;
use crate::utils::{format_time_ago, format_timestamp, truncate_middle};

/// Styled status cell for the payment detail card. For BTC onchain swap
/// payments this routes through `classify_payment`, which gives us the full
/// Boltz lifecycle (including the `Failed` → `Refundable` upgrade when the SDK
/// still lists the swap as refundable and the `Pending` → `PendingConfirmation`
/// vs `PendingSwapCompletion` split once the lockup tx is seen). Direct Liquid
/// and Lightning payments fall back to raw SDK labels, because swap-specific
/// mappings like "Complete Send → Refunded" don't apply to them.
fn payment_status_text(
    payment: &DomainPayment,
    refundable_swap_addresses: &[String],
) -> Element<'static, Message> {
    if matches!(payment.details, DomainPaymentDetails::OnChainBitcoin { .. }) {
        let status = classify_payment(payment, refundable_swap_addresses);
        let style = match status {
            BtcSwapReceiveStatus::Completed | BtcSwapReceiveStatus::Refunded => {
                theme::text::success
            }
            BtcSwapReceiveStatus::Failed | BtcSwapReceiveStatus::Refundable => {
                theme::text::destructive
            }
            _ => theme::text::secondary,
        };
        return text(status.label()).style(style).into();
    }

    match payment.status {
        DomainPaymentStatus::Complete => text("Complete").style(theme::text::success).into(),
        DomainPaymentStatus::Pending => text("Pending").style(theme::text::secondary).into(),
        DomainPaymentStatus::Created => text("Created").style(theme::text::secondary).into(),
        DomainPaymentStatus::Failed => text("Failed").style(theme::text::destructive).into(),
        DomainPaymentStatus::TimedOut => text("Timed Out").style(theme::text::destructive).into(),
        DomainPaymentStatus::Refundable => {
            text("Refundable").style(theme::text::destructive).into()
        }
        DomainPaymentStatus::RefundPending => {
            text("Refund Pending").style(theme::text::secondary).into()
        }
        DomainPaymentStatus::WaitingFeeAcceptance => text("Waiting Fee Acceptance")
            .style(theme::text::secondary)
            .into(),
    }
}

/// Best-effort description for a USDt payment: prefer payer_note, then
/// invoice description, then fall back to "USDt Transfer".  Mirrors the
/// logic in `send.rs` so the transactions list and the send-confirmation
/// screen show the same label.
fn usdt_description(payment: &DomainPayment) -> String {
    let desc = payment.details.description();
    if desc.is_empty() {
        "USDt Transfer".to_owned()
    } else {
        desc.to_owned()
    }
}

/// Returns `Some(formatted_usdt_string)` when the payment is a USDt asset payment.
fn usdt_amount_str(payment: &DomainPayment, usdt_id: &str) -> Option<String> {
    if let DomainPaymentDetails::LiquidAsset {
        asset_id,
        asset_info,
        ..
    } = &payment.details
    {
        if !usdt_id.is_empty() && asset_id == usdt_id {
            let minor = asset_info
                .as_ref()
                .map(|i| i.amount_minor)
                .unwrap_or(payment.amount_sat);
            return Some(format!("{} USDt", format_usdt_display(minor)));
        }
    }
    None
}

// The SDK reports the L-BTC fee as a number of sats. Zero does not prove that
// the fee was paid in USDt; it may also mean no fee or unavailable fee data.
fn usdt_fee_caption(fees_sat: u64) -> &'static str {
    if fees_sat == 0 {
        "No L-BTC fee reported"
    } else {
        "Fees (L-BTC)"
    }
}

#[allow(clippy::too_many_arguments)]
pub fn liquid_transactions_view<'a>(
    payments: &'a [DomainPayment],
    refundables: &'a [DomainRefundableSwap],
    in_flight_refunds: &'a HashMap<String, InFlightRefund>,
    _balance: &'a Amount,
    fiat_converter: Option<FiatAmountConverter>,
    loading: bool,
    bitcoin_unit: coincube_ui::component::amount::BitcoinDisplayUnit,
    usdt_id: &'a str,
    asset_filter: AssetFilter,
    show_direction_badges: bool,
    empty_state_quote: &'a Quote,
    empty_state_image_handle: &'a image::Handle,
    current_page: u32,
    is_last_page: bool,
    processing: bool,
    swap_tx_ids: &'a std::collections::HashSet<String>,
) -> Element<'a, Message> {
    let mut content = Column::new().spacing(20).width(Length::Fill);

    // Keep the asset filter beside the title, as in the Liquid artboard.
    // The source state still owns selection, server filtering and paging.
    let mut filter_row = Row::new().spacing(4);
    for (label, filter) in [
        ("All", AssetFilter::All),
        ("L-BTC", AssetFilter::LbtcOnly),
        ("USDt", AssetFilter::UsdtOnly),
    ] {
        filter_row = filter_row.push(
            iced::widget::button(Container::new(text(label).size(P2_SIZE)).padding([6, 12]))
                .on_press(Message::SetAssetFilter(filter))
                .style(if asset_filter == filter {
                    theme::button::primary
                } else {
                    theme::button::transparent_border
                }),
        );
    }
    content = content.push(
        Row::new()
            .spacing(12)
            .align_y(Alignment::Center)
            .push(Container::new(h3("Transactions").bold()))
            .push(Space::new().width(Length::Fill))
            .push(filter_row)
            .push(
                button::secondary(Some(icon::backup_icon()), "Export")
                    .on_press(ImportExportMessage::Open.into()),
            ),
    );

    // A refundable BTC → L-BTC swap is still activity even when the regular
    // payment page is empty. The USDt tab must never show these BTC refunds.
    let show_refundables = refundables_visible(refundables.len(), asset_filter);

    // Put recoverable funds first so they are not buried after a paged list.
    if show_refundables {
        content = content.push(
            Column::new()
                .spacing(10)
                .push(Container::new(h4_bold("Refundable Transactions")))
                .push(refundables.iter().enumerate().fold(
                    Column::new().spacing(10),
                    |col, (i, refundable)| {
                        col.push(refundable_row(
                            i,
                            refundable,
                            in_flight_refunds.get(&refundable.swap_address),
                            fiat_converter,
                            bitcoin_unit,
                        ))
                    },
                )),
        );
    }

    // On page 0 with no rows we render the empty state; on later pages an
    // empty response just means we've paged past the end — keep the list
    // shell and pagination controls visible so the user can Prev back.
    if loading && payments.is_empty() && current_page == 0 && !show_refundables {
        content =
            content.push(p1_regular("Loading Liquid transactions…").style(theme::text::secondary));
    } else if payments.is_empty() && current_page == 0 && !show_refundables {
        // Empty state with Kage quote
        content = content.push(
            Column::new()
                .spacing(20)
                .width(Length::Fill)
                .align_x(Alignment::Center)
                .push(Space::new().height(Length::Fixed(40.0)))
                .push(quote_display::display(
                    &QuoteDisplayProps::new("empty-wallet", empty_state_quote, empty_state_image_handle),
                ))
                .push(Space::new().height(Length::Fixed(10.0)))
                .push(
                    text(match asset_filter {
                        AssetFilter::All => "Your Liquid wallet is ready. Once you send or receive\nfunds, they'll show up here.",
                        AssetFilter::LbtcOnly => "No L-BTC transactions on this page.",
                        AssetFilter::UsdtOnly => "No USDt transactions on this page.",
                    })
                        .size(16)
                        .style(theme::text::secondary)
                        .wrapping(iced::widget::text::Wrapping::Word)
                        .align_x(iced::alignment::Horizontal::Center),
                )
                .push_maybe(matches!(asset_filter, AssetFilter::All).then(||
                    Row::new()
                        .spacing(15)
                        .push(
                            button::primary(None, "Send sats")
                                .on_press(Message::Menu(Menu::Liquid(crate::app::menu::LiquidSubMenu::Send)))
                                .padding(15)
                                .width(Length::Fixed(150.0)),
                        )
                        .push(
                            button::transparent_border(None, "Receive sats")
                                .on_press(Message::Menu(Menu::Liquid(crate::app::menu::LiquidSubMenu::Receive)))
                                .padding(15)
                                .width(Length::Fixed(150.0)),
                        ),
                )),
        );
    } else if !payments.is_empty() || current_page > 0 {
        // The dashboard already wraps the whole panel in a page-level
        // scrollable, so the list is pushed directly — nesting another
        // scrollable here would make its `Fill` height resolve against an
        // unbounded parent and break layout. Pagination keeps each page
        // bounded (PAGE_SIZE rows), so the page scroll is enough.
        let refundable_swap_addresses: Vec<String> = refundables
            .iter()
            .map(|swap| swap.swap_address.clone())
            .collect();
        let list =
            payments
                .iter()
                .enumerate()
                .fold(Column::new().spacing(10), |col, (i, payment)| {
                    let is_swap = payment
                        .tx_id
                        .as_ref()
                        .is_some_and(|id| swap_tx_ids.contains(id));
                    col.push(transaction_row(
                        i,
                        payment,
                        fiat_converter,
                        bitcoin_unit,
                        usdt_id,
                        show_direction_badges,
                        is_swap,
                        &refundable_swap_addresses,
                    ))
                });
        content = content.push(list);
        content = content.push(pagination_controls(
            Message::LiquidPrevPage,
            Message::LiquidNextPage,
            current_page > 0,
            !is_last_page,
            processing,
            current_page,
        ));
    }

    content = content.push(Space::new().height(Length::Fixed(30.0)));
    content.into()
}

#[allow(clippy::too_many_arguments)]
fn transaction_row<'a>(
    i: usize,
    payment: &'a DomainPayment,
    fiat_converter: Option<FiatAmountConverter>,
    bitcoin_unit: coincube_ui::component::amount::BitcoinDisplayUnit,
    usdt_id: &str,
    show_direction_badges: bool,
    is_swap: bool,
    refundable_swap_addresses: &[String],
) -> Element<'a, Message> {
    let is_receive = payment.is_incoming();
    let usdt_str = usdt_amount_str(payment, usdt_id);

    // Extract description — prefer payer_note/description, fall back to "USDt
    // Transfer". A locally-recorded swap overrides the label with "Swap".
    let is_usdt = usdt_str.is_some();
    let description: String = if is_swap {
        "Swap".to_owned()
    } else if is_usdt {
        usdt_description(payment)
    } else {
        payment.details.description().to_owned()
    };

    let time_ago = format_time_ago(payment.timestamp.into());

    let direction = if is_receive {
        TransactionDirection::Incoming
    } else {
        TransactionDirection::Outgoing
    };

    // Determine the combo icon based on payment type
    let combo_icon: Element<'_, Message> = if is_usdt {
        asset_network_logo("usdt", "liquid", 40.0)
    } else {
        match &payment.details {
            DomainPaymentDetails::Lightning { .. } => asset_network_logo("btc", "lightning", 40.0),
            DomainPaymentDetails::LiquidAsset { .. } => asset_network_logo("lbtc", "liquid", 40.0),
            DomainPaymentDetails::OnChainBitcoin { .. } => {
                asset_network_logo("btc", "bitcoin", 40.0)
            }
        }
    };

    let status = payment_list_status(payment, refundable_swap_addresses);

    if let Some(ref usdt_display) = usdt_str {
        let mut item = TransactionListItem::new(direction, &Amount::ZERO, bitcoin_unit)
            .with_label(description.clone())
            .with_time_ago(time_ago)
            .with_custom_icon(combo_icon)
            .with_show_direction_badge(show_direction_badges)
            .with_amount_override(usdt_display.clone());
        if let Some((label, style)) = status {
            item = item.with_custom_status(status_badge(label, style));
        }
        return item.view(Message::Select(i)).into();
    }

    let mut btc_amount = Amount::from_sat(payment.amount_sat);
    if !is_receive {
        btc_amount += Amount::from_sat(payment.fees_sat);
    }

    let mut item = TransactionListItem::new(direction, &btc_amount, bitcoin_unit)
        .with_label(description)
        .with_time_ago(time_ago)
        .with_custom_icon(combo_icon)
        .with_show_direction_badge(show_direction_badges);

    if let Some(fiat_amount) = fiat_converter.map(|converter| {
        let fiat = converter.convert(btc_amount);
        format!(
            "{} {} · current rate",
            fiat.to_formatted_string(),
            fiat.currency()
        )
    }) {
        item = item.with_fiat_amount(fiat_amount);
    }

    if let Some((label, style)) = status {
        item = item.with_custom_status(status_badge(label, style));
    }

    item.view(Message::Select(i)).into()
}

type StatusTextStyle = fn(&theme::Theme) -> iced::widget::text::Style;

fn refundables_visible(count: usize, filter: AssetFilter) -> bool {
    count > 0 && matches!(filter, AssetFilter::All | AssetFilter::LbtcOnly)
}

/// List status uses the same swap classifier as detail, so a failed payment
/// that remains in the SDK refundable set reads "Refundable" in both places.
fn payment_list_status(
    payment: &DomainPayment,
    refundable_swap_addresses: &[String],
) -> Option<(&'static str, StatusTextStyle)> {
    if matches!(payment.details, DomainPaymentDetails::OnChainBitcoin { .. }) {
        let status = classify_payment(payment, refundable_swap_addresses);
        return match status {
            BtcSwapReceiveStatus::Completed => None,
            BtcSwapReceiveStatus::Refunded => Some(("Refunded", theme::text::success)),
            BtcSwapReceiveStatus::Failed | BtcSwapReceiveStatus::Refundable => {
                Some((status.label(), theme::text::error))
            }
            status => Some((status.label(), theme::text::warning)),
        };
    }
    match payment.status {
        DomainPaymentStatus::Complete => None,
        DomainPaymentStatus::Pending => Some(("Pending", theme::text::warning)),
        DomainPaymentStatus::Created => Some(("Created", theme::text::secondary)),
        DomainPaymentStatus::Failed => Some(("Failed", theme::text::error)),
        DomainPaymentStatus::TimedOut => Some(("Timed Out", theme::text::error)),
        DomainPaymentStatus::Refundable => Some(("Refundable", theme::text::error)),
        DomainPaymentStatus::RefundPending => Some(("Refund Pending", theme::text::warning)),
        DomainPaymentStatus::WaitingFeeAcceptance => {
            Some(("Waiting Fee Acceptance", theme::text::warning))
        }
    }
}

fn status_badge(label: impl Into<String>, style: StatusTextStyle) -> Element<'static, Message> {
    Container::new(text(label.into()).size(CAPTION_SIZE).bold().style(style))
        .padding([2, 8])
        .style(|theme: &theme::Theme| iced::widget::container::Style {
            border: iced::Border {
                color: theme.colors.rule,
                width: 1.0,
                radius: 10.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn refundable_row<'a>(
    i: usize,
    refundable: &'a DomainRefundableSwap,
    in_flight: Option<&'a InFlightRefund>,
    fiat_converter: Option<FiatAmountConverter>,
    bitcoin_unit: coincube_ui::component::amount::BitcoinDisplayUnit,
) -> Element<'a, Message> {
    let btc_amount = Amount::from_sat(refundable.amount_sat);
    let time_ago = format_time_ago(refundable.timestamp.into());

    // This is a recoverable amount, not money already received. Keep it
    // unsigned and name the broadcast state.
    let status = match in_flight {
        Some(InFlightRefund {
            refund_txid: Some(txid),
            ..
        }) => format!("Refund broadcast · {}", truncate_middle(txid, 6, 6)),
        Some(_) => "Refund broadcasting…".to_string(),
        None => "Refundable".to_string(),
    };

    let amount = btc_amount.to_formatted_string_with_unit(bitcoin_unit);
    let mut amount_column = Column::new()
        .spacing(4)
        .align_x(Alignment::End)
        .push(p1_bold(amount));
    if let Some(converter) = fiat_converter {
        let fiat = converter.convert(btc_amount);
        amount_column = amount_column.push(
            p2_regular(format!(
                "{} {} · current rate",
                fiat.to_formatted_string(),
                fiat.currency()
            ))
            .style(theme::text::secondary),
        );
    }
    let row = Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .push(asset_network_logo::<Message>("lbtc", "liquid", 40.0))
        .push(
            Column::new()
                .spacing(4)
                .width(Length::Fill)
                .push(p1_regular("Refundable Swap"))
                .push(
                    Row::new()
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(p2_regular(time_ago).style(theme::text::secondary))
                        .push(status_badge(status, theme::text::warning)),
                ),
        )
        .push(amount_column);
    Container::new(
        iced::widget::button(row.padding(10))
            .on_press(Message::SelectRefundable(i))
            .style(theme::button::transparent_border),
    )
    .style(theme::card::simple)
    .into()
}

pub fn transaction_detail_view<'a>(
    payment: &'a DomainPayment,
    fiat_converter: Option<FiatAmountConverter>,
    bitcoin_unit: coincube_ui::component::amount::BitcoinDisplayUnit,
    usdt_id: &str,
    refundable_swap_addresses: &[String],
) -> Element<'a, Message> {
    let is_receive = payment.is_incoming();
    let usdt_str = usdt_amount_str(payment, usdt_id);
    let btc_amount = Amount::from_sat(payment.amount_sat);
    let fees_sat = Amount::from_sat(payment.fees_sat);
    let mut total_amount = btc_amount;

    if !is_receive {
        total_amount += fees_sat;
    }

    // Format full date/time
    let date_text =
        format_timestamp(payment.timestamp as u64).unwrap_or_else(|| "Unknown".to_string());

    // Extract description — prefer payer_note/description, fall back to "USDt Transfer"
    let description: String = if usdt_str.is_some() {
        usdt_description(payment)
    } else {
        payment.details.description().to_owned()
    };

    let title = if is_receive {
        "Incoming payment"
    } else {
        "Outgoing payment"
    };

    // Helper: combo icon for detail view based on payment type
    let make_detail_icon =
        |is_usdt: bool, details: &DomainPaymentDetails| -> (&'static str, &'static str) {
            if is_usdt {
                ("usdt", "liquid")
            } else {
                match details {
                    DomainPaymentDetails::Lightning { .. } => ("btc", "lightning"),
                    DomainPaymentDetails::LiquidAsset { .. } => ("lbtc", "liquid"),
                    DomainPaymentDetails::OnChainBitcoin { .. } => ("btc", "bitcoin"),
                }
            }
        };

    if let Some(ref usdt_display) = usdt_str {
        // USDt detail view: show USDt amount + L-BTC fees
        let usdt_num = match &payment.details {
            DomainPaymentDetails::LiquidAsset { asset_info, .. } => asset_info
                .as_ref()
                .map(|i| format_usdt_display(i.amount_minor))
                .unwrap_or_else(|| format_usdt_display(payment.amount_sat)),
            _ => format_usdt_display(payment.amount_sat),
        };
        let amount_row = if is_receive {
            Row::new()
                .spacing(5)
                .align_y(Alignment::Center)
                .push(text("+").size(H1_SIZE))
                .push(text(usdt_num).size(H1_SIZE).bold())
                .push(text("USDt").size(H1_SIZE).color(coincube_ui::color::GREY_3))
        } else {
            Row::new()
                .spacing(5)
                .align_y(Alignment::Center)
                .push(text("-").size(H1_SIZE))
                .push(
                    Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(text(usdt_num).size(H1_SIZE).bold())
                        .push(text("USDt").size(H1_SIZE).color(coincube_ui::color::GREY_3)),
                )
        };
        return Column::new()
            .spacing(20)
            .push(detail_back_button())
            .push(Container::new(h3(title)).width(Length::Fill))
            .push({
                let (a, n) = make_detail_icon(true, &payment.details);
                Row::new()
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .push(asset_network_logo::<Message>(a, n, 32.0))
                    .push(p1_regular(description))
            })
            .push(
                Column::new()
                    .spacing(20)
                    .push(Column::new().push(Container::new(amount_row))),
            )
            .push(card::simple(
                Column::new()
                    .push(
                        Row::new()
                            .push(
                                Column::new()
                                    .width(Length::FillPortion(1))
                                    .push(text("Date").bold()),
                            )
                            .push(
                                Column::new()
                                    .width(Length::FillPortion(2))
                                    .push(text(date_text)),
                            )
                            .spacing(20),
                    )
                    .push(
                        Row::new()
                            .push(
                                Column::new()
                                    .width(Length::FillPortion(1))
                                    .push(text("Status").bold()),
                            )
                            .push(
                                Column::new()
                                    .width(Length::FillPortion(2))
                                    .push(payment_status_text(payment, refundable_swap_addresses)),
                            )
                            .spacing(20),
                    )
                    .push(
                        Row::new()
                            .push(
                                Column::new()
                                    .width(Length::FillPortion(1))
                                    .push(text("Asset Amount").bold()),
                            )
                            .push(
                                Column::new()
                                    .width(Length::FillPortion(2))
                                    .push(text(usdt_display.clone())),
                            )
                            .spacing(20),
                    )
                    .push(if fees_sat.to_sat() == 0 {
                        Row::new()
                            .push(
                                Column::new()
                                    .width(Length::FillPortion(1))
                                    .push(text(usdt_fee_caption(0)).bold()),
                            )
                            .push(Column::new().width(Length::FillPortion(2)).push(text("—")))
                            .spacing(20)
                    } else {
                        Row::new()
                            .push(
                                Column::new()
                                    .width(Length::FillPortion(1))
                                    .push(text(usdt_fee_caption(fees_sat.to_sat())).bold()),
                            )
                            .push(
                                Column::new().width(Length::FillPortion(2)).push(text(
                                    fees_sat.to_formatted_string_with_unit(bitcoin_unit),
                                )),
                            )
                            .spacing(20)
                    })
                    .spacing(15),
            ))
            .into();
    }

    Column::new()
        .spacing(20)
        .push(detail_back_button())
        .push(if is_receive {
            Container::new(h3("Incoming payment")).width(Length::Fill)
        } else {
            Container::new(h3("Outgoing payment")).width(Length::Fill)
        })
        .push({
            let (a, n) = make_detail_icon(false, &payment.details);
            Row::new()
                .spacing(10)
                .align_y(Alignment::Center)
                .push(asset_network_logo::<Message>(a, n, 32.0))
                .push(p1_regular(description))
        })
        .push(
            Column::new().spacing(20).push(
                Column::new()
                    .push(if is_receive {
                        Container::new(coincube_ui::component::amount::amount_with_size_and_unit(
                            &btc_amount,
                            H1_SIZE,
                            bitcoin_unit,
                        ))
                    } else {
                        Container::new(Row::new().spacing(5).push(text("-").size(H1_SIZE)).push(
                            coincube_ui::component::amount::amount_with_size_and_unit(
                                &total_amount,
                                H1_SIZE,
                                bitcoin_unit,
                            ),
                        ))
                    })
                    .push_maybe(fiat_converter.map(|converter| {
                        // Use total_amount for outgoing payments to match headline (includes fees)
                        let amount_for_conversion =
                            if is_receive { btc_amount } else { total_amount };
                        let fiat = converter.convert(amount_for_conversion);
                        Row::new().align_y(Alignment::Center).push(
                            text(format!(
                                "≈ {} {} · current rate",
                                fiat.to_formatted_string(),
                                fiat.currency()
                            ))
                            .size(H2_SIZE)
                            .style(theme::text::secondary),
                        )
                    })),
            ),
        )
        .push(card::simple(
            Column::new()
                .push(
                    Row::new()
                        .push(
                            Column::new()
                                .width(Length::FillPortion(1))
                                .push(text("Date").bold()),
                        )
                        .push(
                            Column::new()
                                .width(Length::FillPortion(2))
                                .push(text(date_text)),
                        )
                        .spacing(20),
                )
                .push(
                    Row::new()
                        .push(
                            Column::new()
                                .width(Length::FillPortion(1))
                                .push(text("Status").bold()),
                        )
                        .push(
                            Column::new()
                                .width(Length::FillPortion(2))
                                .push(payment_status_text(payment, refundable_swap_addresses)),
                        )
                        .spacing(20),
                )
                .push(
                    Row::new()
                        .push(
                            Column::new()
                                .width(Length::FillPortion(1))
                                .push(text("Amount").bold()),
                        )
                        .push(
                            Column::new()
                                .width(Length::FillPortion(2))
                                .push(text(btc_amount.to_formatted_string_with_unit(bitcoin_unit))),
                        )
                        .spacing(20),
                )
                .push(
                    Row::new()
                        .push(
                            Column::new()
                                .width(Length::FillPortion(1))
                                .push(text("Fees").bold()),
                        )
                        .push(
                            Column::new()
                                .width(Length::FillPortion(2))
                                .push(text(fees_sat.to_formatted_string_with_unit(bitcoin_unit))),
                        )
                        .spacing(20),
                )
                .spacing(15),
        ))
        .into()
}

/// Low/Medium/High fee priority buttons. While an async fee fetch is in
/// flight for a given priority, that button renders with a "…" label and is
/// disabled, so the user can tell something is happening — before this, the
/// buttons silently triggered a slow mempool fetch and the user was left
/// wondering if they had actually been clicked.
fn fee_priority_buttons(pending: Option<FeeratePriority>) -> Element<'static, Message> {
    fn one(
        label: &'static str,
        priority: FeeratePriority,
        width: f32,
        pending: Option<FeeratePriority>,
    ) -> Element<'static, Message> {
        if pending == Some(priority) {
            // Pending: show "…" and no on_press so the button is visually
            // disabled while the async fee fetch resolves.
            button::secondary(None, "…")
                .width(Length::Fixed(width))
                .into()
        } else {
            button::secondary(None, label)
                .on_press(Message::RefundFeeratePrioritySelected(priority))
                .width(Length::Fixed(width))
                .into()
        }
    }

    Row::new()
        .spacing(5)
        .push(one("Low", FeeratePriority::Low, 80.0, pending))
        .push(one("Medium", FeeratePriority::Medium, 100.0, pending))
        .push(one("High", FeeratePriority::High, 80.0, pending))
        .into()
}

#[allow(clippy::too_many_arguments)]
pub fn refundable_detail_view<'a>(
    refundable: &'a DomainRefundableSwap,
    fiat_converter: Option<FiatAmountConverter>,
    bitcoin_unit: coincube_ui::component::amount::BitcoinDisplayUnit,
    refund_address: &'a form::Value<String>,
    refund_feerate: &'a form::Value<String>,
    refunding: bool,
    pending_fee_priority: Option<FeeratePriority>,
    in_flight: Option<&'a InFlightRefund>,
    has_vault: bool,
) -> Element<'a, Message> {
    let btc_amount = Amount::from_sat(refundable.amount_sat);

    let date_text =
        format_timestamp(refundable.timestamp as u64).unwrap_or_else(|| "Unknown".to_string());

    let can_refund = refund_address.valid
        && !refund_address.value.trim().is_empty()
        && refund_feerate.valid
        && !refund_feerate.value.trim().is_empty()
        && refund_feerate.value.parse::<u32>().is_ok()
        && in_flight.is_none();

    let header_status = match in_flight {
        Some(InFlightRefund {
            refund_txid: Some(txid),
            ..
        }) => {
            format!("Refund broadcast · {}", truncate_middle(txid, 6, 6))
        }
        Some(_) => "Refund broadcasting…".to_string(),
        None => "This swap can be refunded".to_string(),
    };

    Column::new()
        .spacing(20)
        .push(detail_back_button())
        .push(Container::new(h3("Refundable Swap")).width(Length::Fill))
        .push(
            Column::new()
                .push(p1_regular(header_status).style(theme::text::warning))
                .spacing(10),
        )
        .push(
            Column::new().spacing(20).push(
                Column::new()
                    .push(Container::new(Row::new().spacing(5).push(
                        coincube_ui::component::amount::amount_with_size_and_unit(
                            &btc_amount,
                            H1_SIZE,
                            bitcoin_unit,
                        ),
                    )))
                    .push_maybe(fiat_converter.map(|converter| {
                        let fiat = converter.convert(btc_amount);
                        Row::new().align_y(Alignment::Center).push(
                            fiat.to_text()
                                .size(H2_SIZE)
                                .color(coincube_ui::color::GREY_2),
                        )
                    })),
            ),
        )
        .push(card::simple(
            Column::new()
                .push(
                    Row::new()
                        .push(
                            Column::new()
                                .width(Length::FillPortion(1))
                                .push(text("Date").bold()),
                        )
                        .push(
                            Column::new()
                                .width(Length::FillPortion(2))
                                .push(text(date_text)),
                        )
                        .spacing(20),
                )
                .push(
                    Row::new()
                        .push(
                            Column::new()
                                .width(Length::FillPortion(1))
                                .push(text("Swap Address").bold()),
                        )
                        .push(
                            Column::new()
                                .width(Length::FillPortion(2))
                                // Long taproot swap addresses overflow the
                                // card otherwise. Show a middle-truncated
                                // preview with a copy button so the user can
                                // still grab the full address.
                                .push(
                                    Row::new()
                                        .align_y(Alignment::Center)
                                        .spacing(8)
                                        .push(Container::new(
                                            scrollable(
                                                text(truncate_middle(
                                                    &refundable.swap_address,
                                                    10,
                                                    10,
                                                ))
                                                .size(14),
                                            )
                                            .direction(scrollable::Direction::Horizontal(
                                                scrollable::Scrollbar::new()
                                                    .width(2)
                                                    .scroller_width(2),
                                            )),
                                        ))
                                        .push(
                                            iced::widget::button(
                                                icon::clipboard_icon()
                                                    .style(theme::text::secondary),
                                            )
                                            .on_press(Message::Clipboard(
                                                refundable.swap_address.clone(),
                                            ))
                                            .style(theme::button::transparent_border),
                                        ),
                                ),
                        )
                        .spacing(20),
                )
                .push(
                    Row::new()
                        .push(
                            Column::new()
                                .width(Length::FillPortion(1))
                                .push(text("Amount").bold()),
                        )
                        .push(
                            Column::new()
                                .width(Length::FillPortion(2))
                                .push(text(btc_amount.to_formatted_string_with_unit(bitcoin_unit))),
                        )
                        .spacing(20),
                )
                .spacing(15),
        ))
        .push_maybe((in_flight.is_none() && !refunding).then(|| {
            Column::new()
                .spacing(15)
                .push(Container::new(h4_bold("Refund Details")).width(Length::Fill))
                .push(
                    Column::new()
                        .spacing(10)
                        .push(text("Bitcoin Address").bold())
                        .push(
                            form::Form::new(
                                "Enter Bitcoin address for refund",
                                refund_address,
                                Message::RefundAddressEdited,
                            )
                            .size(14)
                            .padding(12),
                        )
                        .push_maybe(has_vault.then(|| {
                            Row::new().spacing(8).push(
                                button::transparent_border(None, "Use Vault address")
                                    .on_press(Message::GenerateVaultRefundAddress)
                                    .padding([6, 14]),
                            )
                        })),
                )
                .push(
                    Column::new()
                        .spacing(10)
                        .push(text("Fee Rate (sat/vB)").bold())
                        .push(
                            Row::new()
                                .spacing(10)
                                .push(
                                    form::Form::new_amount_sats(
                                        "Enter fee rate",
                                        refund_feerate,
                                        Message::RefundFeerateEdited,
                                    )
                                    .size(14)
                                    .padding(12),
                                )
                                .push(fee_priority_buttons(pending_fee_priority)),
                        ),
                )
                .push(
                    Row::new().spacing(10).push(
                        button::primary(None, "Refund")
                            .on_press_maybe(can_refund.then_some(Message::SubmitRefund))
                            .width(Length::Fill),
                    ),
                )
                .push_maybe((!can_refund).then(|| {
                    p2_regular("Enter a valid Bitcoin address and fee rate to enable Refund.")
                        .style(theme::text::secondary)
                }))
        }))
        .into()
}

fn detail_back_button() -> Element<'static, Message> {
    button::secondary(None, "< Back")
        .width(Length::Fixed(150.0))
        .on_press(Message::Close)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::wallets::DomainPaymentDirection;

    fn payment(status: DomainPaymentStatus, details: DomainPaymentDetails) -> DomainPayment {
        DomainPayment {
            tx_id: None,
            destination: None,
            timestamp: 0,
            amount_sat: 9_000,
            fees_sat: 100,
            direction: DomainPaymentDirection::Receive,
            status,
            details,
        }
    }

    #[test]
    fn liquid_list_preserves_failure_and_fee_review_statuses() {
        for (state, expected) in [
            (DomainPaymentStatus::Pending, Some("Pending")),
            (DomainPaymentStatus::TimedOut, Some("Timed Out")),
            (DomainPaymentStatus::RefundPending, Some("Refund Pending")),
            (
                DomainPaymentStatus::WaitingFeeAcceptance,
                Some("Waiting Fee Acceptance"),
            ),
            (DomainPaymentStatus::Complete, None),
        ] {
            let row = payment(
                state,
                DomainPaymentDetails::Lightning {
                    swap_id: None,
                    description: String::new(),
                    payer_note: None,
                },
            );
            assert_eq!(
                payment_list_status(&row, &[]).map(|(label, _)| label),
                expected
            );
        }
    }

    #[test]
    fn failed_bitcoin_receive_still_refundable_is_labelled_refundable() {
        let address = "bc1qsyntheticrefund".to_string();
        let row = payment(
            DomainPaymentStatus::Failed,
            DomainPaymentDetails::OnChainBitcoin {
                swap_id: Some("synthetic-swap".into()),
                bitcoin_address: Some(address.clone()),
                description: String::new(),
                auto_accepted_fees: false,
                liquid_expiration_blockheight: 0,
                bitcoin_expiration_blockheight: 0,
                lockup_tx_id: Some("synthetic-lockup".into()),
                claim_tx_id: None,
                refund_tx_id: None,
                refund_tx_amount_sat: None,
            },
        );
        assert_eq!(
            payment_list_status(&row, &[address]).map(|(label, _)| label),
            Some("Refundable")
        );
    }

    #[test]
    fn refundables_gated_out_of_usdt_filter() {
        // Contract guard: refundables are BTC→L-BTC swap refunds, so they
        // should only surface under All / L-BTC. Regression from prior bug
        // where they appeared under the USDt tab.
        for (filter, expected) in [
            (AssetFilter::All, true),
            (AssetFilter::LbtcOnly, true),
            (AssetFilter::UsdtOnly, false),
        ] {
            let show = refundables_visible(1, filter);
            assert_eq!(
                show, expected,
                "refundables visibility for filter {:?}",
                filter
            );
        }
        assert!(!refundables_visible(0, AssetFilter::All));
    }

    #[test]
    fn zero_reported_lbtc_fee_does_not_claim_usdt_fee_payment() {
        assert_eq!(usdt_fee_caption(0), "No L-BTC fee reported");
        assert_eq!(usdt_fee_caption(42), "Fees (L-BTC)");
    }
}
