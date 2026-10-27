use coincube_ui::{
    color,
    component::{button, text::*},
    icon::{arrow_right, clipboard_icon, previous_icon},
    image::usdt_network_logo,
    theme,
    widget::{ColumnExt, Element, RowExt},
};
use iced::{
    widget::{Column, Container, Row, Space, TextInput},
    Alignment, Length,
};

use crate::app::breez_liquid::assets::format_usdt_display;
use crate::app::state::liquid::sideshift_send::SendPhase;
use crate::app::view::liquid::RecentTransaction;
use crate::app::view::{SideshiftSendMessage, SideshiftShiftType};
use crate::app::wallets::DomainPaymentDetails;
use crate::services::sideshift::{ShiftResponse, ShiftStatusKind, SideshiftNetwork};

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn sideshift_send_view<'a>(
    phase: &SendPhase,
    selected_network: Option<&SideshiftNetwork>,
    detected_networks: &[SideshiftNetwork],
    shift_type: &SideshiftShiftType,
    recipient_address: &'a str,
    amount_input: &'a str,
    usdt_balance: u64,
    recent_transactions: &'a [RecentTransaction],
    shift: Option<&'a ShiftResponse>,
    shift_status: Option<&'a ShiftStatusKind>,
    confirmed_settle_amount: Option<&'a str>,
    loading: bool,
    error: Option<&'a str>,
    usdt_asset_id: &str,
    mask_amounts: bool,
) -> Element<'a, SideshiftSendMessage> {
    match phase {
        SendPhase::AddressInput => address_input_view(
            recipient_address,
            selected_network,
            detected_networks,
            usdt_balance,
            recent_transactions,
            error,
            usdt_asset_id,
            mask_amounts,
        ),
        SendPhase::NetworkDisambiguation => disambiguation_view(
            recipient_address,
            selected_network,
            detected_networks,
            error,
        ),
        SendPhase::AmountInput => amount_input_view(
            selected_network,
            shift_type,
            amount_input,
            usdt_balance,
            loading,
            error,
            mask_amounts,
        ),
        SendPhase::FetchingAffiliate
        | SendPhase::FetchingQuote
        | SendPhase::CreatingShift
        | SendPhase::Sending => loading_view(phase),
        SendPhase::Review => {
            review_view(selected_network, shift, amount_input, error, mask_amounts)
        }
        SendPhase::Sent => sent_view(
            selected_network,
            shift,
            shift_status,
            confirmed_settle_amount,
            mask_amounts,
        ),
        SendPhase::Failed => error_view(error),
    }
}

// ---------------------------------------------------------------------------
// Address input with balance (initial screen)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)] // Existing flow inputs plus shared privacy state.
fn address_input_view<'a>(
    recipient_address: &'a str,
    selected_network: Option<&SideshiftNetwork>,
    detected_networks: &[SideshiftNetwork],
    usdt_balance: u64,
    recent_transactions: &'a [RecentTransaction],
    error: Option<&'a str>,
    usdt_asset_id: &str,
    mask_amounts: bool,
) -> Element<'a, SideshiftSendMessage> {
    let mut content = Column::new().spacing(20).align_x(Alignment::Center);

    // Balance card
    let balance_col = Column::new()
        .spacing(4)
        .push(
            Row::new()
                .spacing(10)
                .align_y(Alignment::Center)
                .push(
                    text(if mask_amounts {
                        "••••••".into()
                    } else {
                        format_usdt_display(usdt_balance)
                    })
                    .size(H2_SIZE)
                    .bold(),
                )
                .push(text("USDt").size(H2_SIZE).color(color::GREY_3)),
        )
        .push(
            text("Liquid Network")
                .size(P1_SIZE)
                .style(theme::text::secondary),
        );
    let balance_inner = Column::new()
        .spacing(8)
        .push(h4_bold("Balance"))
        .push(balance_col);
    content = content.push(crate::app::view::balance_header_card(balance_inner));

    // Address input with inline detection chip and arrow button
    let can_proceed = !recipient_address.trim().is_empty()
        && !detected_networks.is_empty()
        && (detected_networks.len() == 1 || selected_network.is_some());

    // Build the detection chip that sits inside the input row
    let detection_chip: Option<Element<'a, SideshiftSendMessage>> =
        if recipient_address.trim().is_empty() {
            None
        } else if detected_networks.is_empty() {
            Some(
                Container::new(text("Unknown").size(P2_SIZE).color(color::RED))
                    .padding([4, 8])
                    .style(theme::pill::error)
                    .into(),
            )
        } else if detected_networks.len() == 1 {
            let net = &detected_networks[0];
            Some(
                Container::new(
                    Row::new()
                        .spacing(6)
                        .align_y(Alignment::Center)
                        .push(usdt_network_logo(net.network_slug(), 24.0))
                        .push(text(net.network_name()).size(P2_SIZE).bold()),
                )
                .padding([0, 12])
                .height(Length::Fixed(50.0))
                .center_y(Length::Fixed(50.0))
                .style(theme::pill::simple)
                .into(),
            )
        } else {
            // Multiple networks — inline disambiguator pills
            let mut row = Row::new().spacing(4).align_y(Alignment::Center);
            for network in detected_networks {
                let is_selected = selected_network == Some(network);
                let pill =
                    Container::new(
                        Row::new()
                            .spacing(4)
                            .align_y(Alignment::Center)
                            .push(usdt_network_logo(network.network_slug(), 22.0))
                            .push(text(network.network_name()).size(P2_SIZE).color(
                                if is_selected {
                                    color::ORANGE
                                } else {
                                    color::GREY_3
                                },
                            )),
                    )
                    .padding([0, 8])
                    .height(Length::Fixed(50.0))
                    .center_y(Length::Fixed(50.0))
                    .style(if is_selected {
                        |t: &theme::Theme| squared(theme::pill::selected(t))
                    } else {
                        |t: &theme::Theme| squared(theme::pill::simple(t))
                    });

                let btn = iced::widget::button(pill)
                    .on_press(SideshiftSendMessage::DisambiguateNetwork(*network))
                    .style(theme::button::transparent);
                row = row.push(btn);
            }
            Some(row.into())
        };

    let mut input_row = Row::new().spacing(10).align_y(Alignment::Center).push(
        TextInput::new("Paste USDt address (any network)…", recipient_address)
            .on_input(SideshiftSendMessage::RecipientAddressEdited)
            .padding(15)
            .size(16),
    );

    if let Some(chip) = detection_chip {
        input_row = input_row.push(chip);
    }

    input_row = input_row.push(
        Container::new(
            iced::widget::button(
                Container::new(arrow_right())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(Alignment::Center)
                    .align_y(Alignment::Center),
            )
            .on_press_maybe(if can_proceed {
                Some(SideshiftSendMessage::Next)
            } else {
                None
            })
            .width(Length::Fixed(50.0))
            .height(Length::Fixed(50.0))
            .style(theme::button::primary),
        )
        .width(Length::Fixed(50.0))
        .height(Length::Fixed(50.0)),
    );

    let input_section = Column::new()
        .spacing(20)
        .width(Length::Fill)
        .push(
            Container::new(h4_bold(
                "Enter USDt Address (Liquid, Ethereum, Tron, Binance, or Solana)",
            ))
            .padding(iced::Padding::new(0.0).top(5))
            .width(Length::Fill),
        )
        .push(input_row);

    content = content.push(input_section);

    // Error
    if let Some(err) = error {
        content = content.push(
            Container::new(text(err).size(P2_SIZE).color(color::RED))
                .padding([8, 12])
                .style(theme::card::error),
        );
    }

    // Recent USDt transactions
    if !recent_transactions.is_empty() {
        let mut tx_list = Column::new().spacing(8);
        tx_list = tx_list.push(h4_bold("Last transactions"));

        for tx in recent_transactions.iter().take(5) {
            let is_usdt = matches!(&tx.details,
                DomainPaymentDetails::LiquidAsset { asset_id, .. }
                    if !usdt_asset_id.is_empty() && asset_id == usdt_asset_id);
            if !is_usdt {
                continue;
            }
            let amount_text = if mask_amounts {
                "••••••".into()
            } else {
                format!(
                    "{}{} USDt",
                    if tx.is_incoming { "+ " } else { "- " },
                    format_usdt_display(tx.amount.to_sat())
                )
            };

            let amount_color = if tx.is_incoming {
                color::GREEN
            } else {
                color::RED
            };

            let row = Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(
                    Column::new()
                        .spacing(2)
                        .width(Length::Fill)
                        .push(text(&tx.description).size(P1_SIZE))
                        .push(
                            text(&tx.time_ago)
                                .size(P2_SIZE)
                                .style(theme::text::secondary),
                        ),
                )
                .push(text(amount_text).size(P1_SIZE).color(amount_color));

            tx_list = tx_list.push(
                Container::new(row)
                    .padding([10, 12])
                    .width(Length::Fill)
                    .style(theme::card::simple),
            );
        }

        content = content.push(tx_list);
    }

    content.into()
}

// ---------------------------------------------------------------------------
// Network disambiguation (full-screen fallback for ambiguous addresses)
// ---------------------------------------------------------------------------

fn disambiguation_view<'a>(
    recipient_address: &'a str,
    selected_network: Option<&SideshiftNetwork>,
    detected_networks: &[SideshiftNetwork],
    error: Option<&'a str>,
) -> Element<'a, SideshiftSendMessage> {
    let back_btn = button::secondary(Some(previous_icon()), "Back")
        .width(Length::Fixed(150.0))
        .on_press(SideshiftSendMessage::Back);

    let title = Column::new().spacing(4).push(h3("Select Network")).push(
        text("This address is compatible with multiple networks. Please select one.")
            .size(P1_SIZE)
            .style(theme::text::secondary),
    );

    let addr_preview = Container::new(
        text(recipient_address)
            .size(P2_SIZE)
            .font(iced::Font::MONOSPACE),
    )
    .padding([8, 12])
    .style(theme::card::simple)
    .width(Length::Fill);

    let mut network_list = Column::new().spacing(8);
    for network in detected_networks {
        let is_selected = selected_network == Some(network);

        let row_content = Row::new()
            .spacing(12)
            .align_y(Alignment::Center)
            .push(usdt_network_logo(network.network_slug(), 36.0))
            .push(
                Column::new()
                    .spacing(2)
                    .push(text(network.display_name()).size(P1_SIZE).bold())
                    .push(
                        text(network.standard_label())
                            .size(P2_SIZE)
                            .style(theme::text::secondary),
                    ),
            )
            .push(Space::new().width(Length::Fill))
            .push_maybe(if is_selected {
                Some(
                    Container::new(text("Selected").size(P2_SIZE).color(color::ORANGE))
                        .padding([2, 8])
                        .style(theme::pill::simple),
                )
            } else {
                None
            });

        let card = Container::new(row_content)
            .padding([12, 16])
            .width(Length::Fill)
            .style(if is_selected {
                |t: &theme::Theme| squared(theme::pill::selected(t))
            } else {
                |t: &theme::Theme| squared(theme::card::simple(t))
            });

        let btn = iced::widget::button(card)
            .on_press(SideshiftSendMessage::DisambiguateNetwork(*network))
            .style(theme::button::transparent_border)
            .width(Length::Fill);

        network_list = network_list.push(btn);
    }

    let next_btn = if selected_network.is_some() {
        button::primary(None, "Continue")
            .on_press(SideshiftSendMessage::Next)
            .width(Length::Fill)
    } else {
        button::primary(None, "Continue")
            .width(Length::Fill)
            .style(theme::button::primary)
    };

    let mut col = Column::new()
        .spacing(16)
        .push(back_btn)
        .push(title)
        .push(addr_preview)
        .push(network_list);

    if let Some(err) = error {
        col = col.push(
            Container::new(text(err).size(P2_SIZE).color(color::RED))
                .padding([8, 12])
                .style(theme::card::error),
        );
    }

    Container::new(col.push(next_btn).max_width(520).width(Length::Fill))
        .center_x(Length::Fill)
        .into()
}

// ---------------------------------------------------------------------------
// Amount input
// ---------------------------------------------------------------------------

fn amount_input_view<'a>(
    selected_network: Option<&SideshiftNetwork>,
    shift_type: &SideshiftShiftType,
    amount_input: &'a str,
    usdt_balance: u64,
    loading: bool,
    error: Option<&'a str>,
    mask_amounts: bool,
) -> Element<'a, SideshiftSendMessage> {
    let network = selected_network
        .copied()
        .unwrap_or(SideshiftNetwork::Ethereum);

    let back_btn = button::secondary(Some(previous_icon()), "Back")
        .width(Length::Fixed(150.0))
        .on_press(SideshiftSendMessage::Back);

    let title = Column::new()
        .spacing(4)
        .push(h3(format!("Send to {}", network.display_name())))
        .push(
            text(format!(
                "Your Liquid USDt will be swapped and delivered as {} USDt.",
                network.standard_label()
            ))
            .size(P1_SIZE)
            .style(theme::text::secondary),
        );

    // Detected network badge
    let network_badge = Container::new(
        Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .push(usdt_network_logo(network.network_slug(), 28.0))
            .push(text("Network:").size(P2_SIZE).style(theme::text::secondary))
            .push(text(network.display_name()).size(P2_SIZE).bold()),
    )
    .padding([4, 10])
    .style(theme::pill::simple);

    // Balance display
    let balance_row = Row::new()
        .spacing(6)
        .align_y(Alignment::Center)
        .push(
            text("Available:")
                .size(P2_SIZE)
                .style(theme::text::secondary),
        )
        .push(
            text(if mask_amounts {
                "••••••".into()
            } else {
                format!("{} USDt", format_usdt_display(usdt_balance))
            })
            .size(P2_SIZE)
            .bold(),
        );

    let amount_section = Column::new()
        .spacing(6)
        .push(text("Amount to send (USDt)").size(P2_SIZE).bold())
        .push(
            text("Enter the amount of USDt to send. A fixed rate is locked at confirmation.")
                .size(P2_SIZE)
                .style(theme::text::secondary),
        )
        .push(
            TextInput::new("e.g. 50.00", amount_input)
                .on_input(SideshiftSendMessage::AmountInput)
                .padding([10, 14])
                .size(P1_SIZE),
        );

    let rate_indicator = Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(
            Container::new(
                text(match shift_type {
                    SideshiftShiftType::Fixed => "Fixed Rate",
                    SideshiftShiftType::Variable => "Variable Rate",
                })
                .size(P2_SIZE)
                .color(match shift_type {
                    SideshiftShiftType::Fixed => color::ORANGE,
                    SideshiftShiftType::Variable => color::GREY_3,
                }),
            )
            .padding([4, 10])
            .style(theme::pill::simple),
        )
        .push(
            text(match shift_type {
                SideshiftShiftType::Fixed => "Rate locked at confirmation",
                SideshiftShiftType::Variable => "Rate determined at settlement",
            })
            .size(P2_SIZE)
            .style(theme::text::secondary),
        );

    let review_btn = if loading {
        button::primary(None, "Preparing…")
            .width(Length::Fill)
            .style(theme::button::primary)
    } else {
        button::primary(None, "Review Swap")
            .on_press(SideshiftSendMessage::Generate)
            .width(Length::Fill)
    };

    let mut col = Column::new()
        .spacing(16)
        .push(back_btn)
        .push(title)
        .push(network_badge)
        .push(balance_row)
        .push(amount_section)
        .push(rate_indicator);

    if let Some(err) = error {
        col = col.push(
            Container::new(text(err).size(P2_SIZE).color(color::RED))
                .padding([8, 12])
                .style(theme::card::error),
        );
    }

    Container::new(col.push(review_btn).max_width(520).width(Length::Fill))
        .center_x(Length::Fill)
        .into()
}

// ---------------------------------------------------------------------------
// Loading view
// ---------------------------------------------------------------------------

fn loading_view(phase: &SendPhase) -> Element<'static, SideshiftSendMessage> {
    let label = match phase {
        SendPhase::FetchingAffiliate => "Connecting to SideShift…",
        SendPhase::FetchingQuote => "Fetching quote…",
        SendPhase::CreatingShift => "Creating swap…",
        SendPhase::Sending => "Sending Liquid USDt…",
        _ => "Loading…",
    };

    Container::new(
        Column::new()
            .spacing(20)
            .align_x(Alignment::Center)
            .push(Space::new().height(Length::Fixed(60.0)))
            .push(text(label).size(P1_SIZE).style(theme::text::secondary))
            .max_width(520)
            .width(Length::Fill),
    )
    .center_x(Length::Fill)
    .into()
}

// ---------------------------------------------------------------------------
// Review — show fees before confirming
// ---------------------------------------------------------------------------

fn review_view<'a>(
    selected_network: Option<&SideshiftNetwork>,
    shift: Option<&'a ShiftResponse>,
    amount_input: &'a str,
    error: Option<&'a str>,
    mask_amounts: bool,
) -> Element<'a, SideshiftSendMessage> {
    let Some(shift) = shift else {
        return error_view(Some("Swap data missing."));
    };

    let network = selected_network
        .copied()
        .unwrap_or(SideshiftNetwork::Ethereum);

    let title = Column::new().spacing(4).push(h3("Review Swap")).push(
        text(format!(
            "You will send Liquid USDt. SideShift delivers {} USDt to the recipient.",
            network.network_name()
        ))
        .size(P1_SIZE)
        .style(theme::text::secondary),
    );

    let info = info_card(shift, amount_input, mask_amounts);

    let warning = Container::new(
        text(format!(
            "SideShift will deliver {} ({}) to the recipient address you provided.",
            network.display_name(),
            network.standard_label()
        ))
        .size(P2_SIZE)
        .style(theme::text::secondary),
    )
    .padding([10, 14])
    .style(theme::card::simple)
    .width(Length::Fill);

    let back_btn = button::secondary(Some(previous_icon()), "Back")
        .on_press(SideshiftSendMessage::Back)
        .width(Length::FillPortion(1));

    let confirm_btn = button::primary(None, "Confirm & Send")
        .on_press(SideshiftSendMessage::ConfirmSend)
        .width(Length::FillPortion(1));

    let action_row = Row::new()
        .spacing(10)
        .push(back_btn)
        .push(confirm_btn)
        .width(Length::Fill);

    let mut col = Column::new()
        .spacing(16)
        .push(title)
        .push(info)
        .push(warning);

    if let Some(err) = error {
        col = col.push(
            Container::new(text(err).size(P2_SIZE).color(color::RED))
                .padding([8, 12])
                .style(theme::card::error),
        );
    }

    Container::new(col.push(action_row).max_width(520).width(Length::Fill))
        .center_x(Length::Fill)
        .into()
}

// ---------------------------------------------------------------------------
// Sent / polling view
// ---------------------------------------------------------------------------

fn sent_view<'a>(
    selected_network: Option<&SideshiftNetwork>,
    shift: Option<&'a ShiftResponse>,
    shift_status: Option<&'a ShiftStatusKind>,
    confirmed_settle_amount: Option<&'a str>,
    mask_amounts: bool,
) -> Element<'a, SideshiftSendMessage> {
    let network = selected_network
        .copied()
        .unwrap_or(SideshiftNetwork::Ethereum);

    let status_label = sent_status_label(shift_status);

    let (status_style, status_icon): (fn(&theme::Theme) -> iced::widget::text::Style, &str) =
        match shift_status {
            Some(ShiftStatusKind::Settled) => (theme::text::success, "✓"),
            Some(ShiftStatusKind::Expired | ShiftStatusKind::Error | ShiftStatusKind::Refunded) => {
                (theme::text::error, "✗")
            }
            Some(ShiftStatusKind::Review | ShiftStatusKind::Refunding) => {
                (theme::text::warning, "!")
            }
            _ => (theme::text::warning, "⟳"),
        };

    let title = Column::new().spacing(4).push(h3("Swap Submitted")).push(
        text(format!(
            "SideShift swap from Liquid USDt to {}.",
            network.display_name()
        ))
        .size(P1_SIZE)
        .style(theme::text::secondary),
    );

    let status_badge = Container::new(
        Row::new()
            .spacing(6)
            .align_y(Alignment::Center)
            .push(text(status_icon).size(P1_SIZE).style(status_style))
            .push(text(status_label).size(P1_SIZE).style(status_style)),
    )
    .padding([8, 16])
    .style(theme::card::simple);

    let mut col = Column::new()
        .spacing(20)
        .push(title)
        .push(status_badge)
        .push(swap_timeline(shift_status));

    let outcome_note = match shift_status {
        Some(ShiftStatusKind::Review) => Some("SideShift has put this swap on hold for review. Contact SideShift with the swap ID for help."),
        Some(ShiftStatusKind::Refunding) => Some("A refund is in progress. Check the swap status again later."),
        Some(ShiftStatusKind::Refunded) => Some("This swap was refunded. The recipient was not paid."),
        Some(ShiftStatusKind::Expired) => Some("This swap expired. Check the swap details before trying again."),
        Some(ShiftStatusKind::Error) => Some("SideShift reported an error. Check the swap details before trying again."),
        Some(ShiftStatusKind::Unknown(_)) => Some("SideShift returned an unrecognized status. Check the swap details before taking further action."),
        _ => None,
    };
    if let Some(note) = outcome_note {
        col = col.push(
            Container::new(text(note).size(P2_SIZE).style(theme::text::secondary))
                .padding([10, 14])
                .width(Length::Fill)
                .style(theme::card::simple),
        );
    }

    if let Some(shift) = shift {
        col = col.push(
            Column::new()
                .spacing(6)
                .push(
                    Row::new()
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(
                            text("Swap ID")
                                .size(P2_SIZE)
                                .style(theme::text::secondary)
                                .width(Length::Fixed(80.0)),
                        )
                        .push(text(&shift.id).size(P2_SIZE).font(iced::Font::MONOSPACE))
                        .push(
                            iced::widget::button(clipboard_icon().size(16))
                                .on_press(SideshiftSendMessage::Copy)
                                .style(theme::button::transparent_border),
                        ),
                )
                .push_maybe(shift.settle_address.as_ref().map(|addr| {
                    Row::new()
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(
                            text("Recipient")
                                .size(P2_SIZE)
                                .style(theme::text::secondary)
                                .width(Length::Fixed(80.0)),
                        )
                        .push(text(addr).size(P2_SIZE).font(iced::Font::MONOSPACE))
                }))
                .push_maybe(
                    recipient_amount_to_show(
                        shift_status,
                        confirmed_settle_amount,
                        shift.settle_amount.as_deref(),
                    )
                    .map(|(label, amount)| {
                        Row::new()
                            .spacing(8)
                            .align_y(Alignment::Center)
                            .push(
                                text(label)
                                    .size(P2_SIZE)
                                    .style(theme::text::secondary)
                                    .width(Length::Fixed(80.0)),
                            )
                            .push(
                                text(if mask_amounts {
                                    "••••••".to_string()
                                } else {
                                    format!("{} {}", amount, network.display_name())
                                })
                                .size(P2_SIZE),
                            )
                    }),
                ),
        );
    }

    let done_btn = button::primary(None, "Done")
        .on_press(SideshiftSendMessage::Reset)
        .width(Length::Fill);

    Container::new(col.push(done_btn).max_width(520).width(Length::Fill))
        .center_x(Length::Fill)
        .into()
}

fn sent_status_label(status: Option<&ShiftStatusKind>) -> &str {
    status
        .map(ShiftStatusKind::display)
        .unwrap_or("Payment sent · checking swap")
}

fn recipient_amount_to_show<'a>(
    status: Option<&ShiftStatusKind>,
    confirmed: Option<&'a str>,
    quote: Option<&'a str>,
) -> Option<(&'static str, &'a str)> {
    if status == Some(&ShiftStatusKind::Settled) {
        confirmed.map(|amount| ("Recipient receives", amount))
    } else {
        quote.map(|amount| ("Quoted recipient amount", amount))
    }
}

fn swap_timeline_stage(status: Option<&ShiftStatusKind>) -> Option<usize> {
    match status {
        Some(ShiftStatusKind::Waiting) | None => Some(0),
        Some(ShiftStatusKind::Pending) => Some(1),
        Some(ShiftStatusKind::Processing) => Some(2),
        Some(ShiftStatusKind::Settling) => Some(3),
        Some(ShiftStatusKind::Settled) => Some(4),
        // Holds, refunds, failures and unknown statuses are not forward progress.
        _ => None,
    }
}

fn swap_timeline(status: Option<&ShiftStatusKind>) -> Element<'static, SideshiftSendMessage> {
    const STAGES: [&str; 5] = [
        "Waiting for deposit",
        "Deposit detected",
        "Processing",
        "Settling",
        "Settled",
    ];

    let current = swap_timeline_stage(status);

    let mut stages = Column::new().spacing(10).push(h4_bold("Swap status"));
    for (index, label) in STAGES.iter().enumerate() {
        let complete =
            current.is_some_and(|step| index < step) || (index == 4 && current == Some(4));
        let active = current == Some(index) && !complete;
        let (marker, tone): (&str, fn(&theme::Theme) -> iced::widget::text::Style) = if complete {
            ("✓", theme::text::success)
        } else if active {
            ("●", theme::text::warning)
        } else {
            ("○", theme::text::secondary)
        };
        stages = stages.push(
            Row::new()
                .spacing(10)
                .align_y(Alignment::Center)
                .push(text(marker).size(P1_SIZE).style(tone))
                .push(text(*label).size(P1_SIZE).style(tone)),
        );
    }

    Container::new(stages)
        .padding([12, 16])
        .width(Length::Fill)
        .style(theme::card::simple)
        .into()
}

#[cfg(test)]
mod status_presentation_tests {
    use super::{
        recipient_amount_to_show, sent_status_label, swap_timeline_stage, ShiftStatusKind,
    };

    #[test]
    fn local_payment_and_server_deposit_have_distinct_statuses() {
        assert_eq!(sent_status_label(None), "Payment sent · checking swap");
        assert_eq!(swap_timeline_stage(None), Some(0));

        let detected = ShiftStatusKind::Pending;
        assert_eq!(sent_status_label(Some(&detected)), "Deposit detected");
        assert_eq!(swap_timeline_stage(Some(&detected)), Some(1));

        let waiting = ShiftStatusKind::Waiting;
        assert_eq!(swap_timeline_stage(Some(&waiting)), Some(0));
    }

    #[test]
    fn interrupted_swap_does_not_advance_the_timeline() {
        assert_eq!(swap_timeline_stage(Some(&ShiftStatusKind::Review)), None);
        assert_eq!(swap_timeline_stage(Some(&ShiftStatusKind::Refunding)), None);
        assert_eq!(swap_timeline_stage(Some(&ShiftStatusKind::Expired)), None);
    }

    #[test]
    fn settled_amount_comes_from_status_response_and_never_falls_back_to_quote() {
        let settled = ShiftStatusKind::Settled;
        assert_eq!(
            recipient_amount_to_show(Some(&settled), Some("4.91"), Some("4.93")),
            Some(("Recipient receives", "4.91"))
        );
        assert_eq!(
            recipient_amount_to_show(Some(&settled), None, Some("4.93")),
            None
        );
        assert_eq!(
            recipient_amount_to_show(Some(&ShiftStatusKind::Processing), None, Some("4.93")),
            Some(("Quoted recipient amount", "4.93"))
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn info_card(
    shift: &ShiftResponse,
    amount_input: &str,
    mask_amounts: bool,
) -> Element<'static, SideshiftSendMessage> {
    let mut rows = Column::new().spacing(8);

    if let Some(dep) = &shift.deposit_amount {
        rows = rows.push(masked_info_row(
            "You send",
            &format!("{} Liquid USDt", dep),
            mask_amounts,
        ));
    } else if !amount_input.is_empty() {
        rows = rows.push(masked_info_row(
            "You send",
            &format!("{} Liquid USDt", amount_input),
            mask_amounts,
        ));
    }
    if let Some(settle) = &shift.settle_amount {
        rows = rows.push(masked_info_row("Recipient receives", settle, mask_amounts));
    }
    if let Some(rate) = &shift.rate {
        rows = rows.push(info_row("Rate", rate));
    }
    if let Some(fee) = &shift.network_fee_usd {
        rows = rows.push(masked_info_row(
            "Network fee",
            &format!("${}", fee),
            mask_amounts,
        ));
    }
    if let Some(aff) = &shift.affiliate_fee_percent {
        rows = rows.push(info_row("Service fee", &format!("{}%", aff)));
    }
    if let Some(addr) = &shift.settle_address {
        rows = rows.push(info_row("To", addr));
    }
    rows = rows.push(info_row("Swap ID", &shift.id));

    Container::new(rows)
        .padding([12, 16])
        .width(Length::Fill)
        .style(theme::card::simple)
        .into()
}

fn masked_info_row(
    label: &str,
    value: &str,
    mask_amounts: bool,
) -> Element<'static, SideshiftSendMessage> {
    info_row(
        label,
        if mask_amounts {
            "••••••"
        } else {
            value
        },
    )
}

fn info_row(label: &str, value: &str) -> Element<'static, SideshiftSendMessage> {
    Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(
            text(label.to_string())
                .size(P2_SIZE)
                .style(theme::text::secondary)
                .width(Length::FillPortion(2)),
        )
        .push(
            text(value.to_string())
                .size(P2_SIZE)
                .font(iced::Font::MONOSPACE)
                .width(Length::FillPortion(3)),
        )
        .into()
}

fn error_view(error: Option<&str>) -> Element<SideshiftSendMessage> {
    let msg = error.unwrap_or("An unknown error occurred.");

    Container::new(
        Column::new()
            .spacing(16)
            .push(
                Container::new(text(msg).size(P1_SIZE).color(color::RED))
                    .padding([12, 16])
                    .style(theme::card::error)
                    .width(Length::Fill),
            )
            .push(
                button::primary(None, "Try Again")
                    .on_press(SideshiftSendMessage::Reset)
                    .width(Length::Fill),
            )
            .max_width(520)
            .width(Length::Fill),
    )
    .center_x(Length::Fill)
    .into()
}

/// Network choices are buttons, so they take the square 8px corner every
/// button has since canvas v122, over their pill or card colours.
fn squared(mut style: iced::widget::container::Style) -> iced::widget::container::Style {
    style.border.radius = 8.0.into();
    style
}

#[cfg(test)]
mod privacy_tests {
    use super::*;
    use crate::app::view::shared::landing::capture;
    use crate::app::wallets::DomainPaymentStatus;
    use coincube_core::miniscript::bitcoin::Amount;

    #[test]
    fn sideshift_passive_balance_and_last_transactions_follow_global_mask() {
        let recent = [RecentTransaction {
            description: "USDt payment".into(),
            time_ago: "just now".into(),
            amount: Amount::from_sat(246_813_000_000),
            fees_sat: Amount::ZERO,
            fiat_amount: None,
            is_incoming: true,
            status: DomainPaymentStatus::Complete,
            details: DomainPaymentDetails::LiquidAsset {
                asset_id: "usdt".into(),
                asset_info: None,
                description: "USDt payment".into(),
                payer_note: None,
            },
            usdt_display: Some("2468.13 USDt".into()),
        }];
        let phase = SendPhase::AddressInput;
        let render = |mask_amounts| {
            sideshift_send_view(
                &phase,
                None,
                &[],
                &SideshiftShiftType::Fixed,
                "",
                "77.77",
                135_791_113,
                &recent,
                None,
                None,
                None,
                false,
                None,
                "usdt",
                mask_amounts,
            )
        };
        let hidden = capture::inspect(
            "sideshift-hidden",
            render(true),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(hidden.contains("••••••"));
        let usdt_balance = format_usdt_display(135_791_113);
        assert!(!hidden.contains(&usdt_balance));
        let usdt_recent = format_usdt_display(246_813_000_000);
        assert!(!hidden.contains(&usdt_recent));
        let shown = capture::inspect(
            "sideshift-shown",
            render(false),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(shown.contains(&usdt_balance));
        assert!(shown.contains(&usdt_recent));

        let amount_phase = SendPhase::AmountInput;
        let amount_hidden = capture::inspect(
            "sideshift-amount-hidden",
            sideshift_send_view(
                &amount_phase,
                None,
                &[],
                &SideshiftShiftType::Fixed,
                "",
                "77.77",
                135_791_113,
                &recent,
                None,
                None,
                None,
                false,
                None,
                "usdt",
                true,
            ),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(amount_hidden.contains("••••••"));
        assert!(!amount_hidden.contains("1.35 USDt"));
    }

    #[test]
    fn sideshift_review_masks_quote_values_but_keeps_destination_visible() {
        let shift = ShiftResponse {
            id: "synthetic-review-swap".into(),
            deposit_address: "synthetic-deposit".into(),
            deposit_coin: None,
            deposit_network: None,
            settle_address: Some("synthetic-recipient".into()),
            settle_coin: None,
            settle_network: None,
            deposit_min: Some("20.11".into()),
            deposit_max: Some("120.22".into()),
            deposit_amount: Some("91.27".into()),
            settle_amount: Some("90.13".into()),
            rate: Some("0.98765".into()),
            expires_at: None,
            created_at: None,
            status: None,
            affiliate_fee_percent: Some("0.55".into()),
            network_fee_usd: Some("0.77".into()),
        };
        let render = |mask_amounts| {
            sideshift_send_view(
                &SendPhase::Review,
                Some(&SideshiftNetwork::Ethereum),
                &[],
                &SideshiftShiftType::Fixed,
                "",
                "88.44",
                0,
                &[],
                Some(&shift),
                None,
                None,
                false,
                None,
                "usdt",
                mask_amounts,
            )
        };
        let hidden = capture::inspect(
            "sideshift-review-hidden",
            render(true),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(hidden.contains("••••••"));
        for value in ["91.27", "90.13", "0.77"] {
            assert!(!hidden.contains(value), "review leaked {}", value);
        }
        assert!(hidden.contains("0.98765"));
        assert!(hidden.contains("0.55%"));
        assert!(!hidden.contains("20.11"));
        assert!(!hidden.contains("120.22"));
        assert!(hidden.contains("synthetic-recipient"));
        assert!(hidden.contains("synthetic-review-swap"));

        let mut variable_shift = shift.clone();
        variable_shift.deposit_amount = None;
        let fallback_hidden = capture::inspect(
            "sideshift-review-fallback-hidden",
            review_view(
                Some(&SideshiftNetwork::Ethereum),
                Some(&variable_shift),
                "88.44",
                None,
                true,
            ),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(!fallback_hidden.contains("88.44"));

        let shown = capture::inspect(
            "sideshift-review-shown",
            render(false),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(shown.contains("91.27"));
        assert!(shown.contains("90.13"));
        assert!(shown.contains("0.98765"));
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            let compact = capture::inspect(
                "sideshift-review-compact-hidden",
                render(true),
                (1024, 768),
                appearance,
                theme,
            );
            assert!(compact.contains("••••••"));
            assert!(compact.contains("synthetic-recipient"));
        }
    }

    #[test]
    fn sideshift_sent_status_mounts_in_dark_and_light_themes() {
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for (phase, status) in [
                ("waiting", None),
                ("processing", Some(ShiftStatusKind::Processing)),
                ("settled", Some(ShiftStatusKind::Settled)),
                ("refunded", Some(ShiftStatusKind::Refunded)),
            ] {
                let mounted = capture::inspect(
                    &format!("sideshift-sent-status-{phase}"),
                    sent_view(
                        Some(&SideshiftNetwork::Ethereum),
                        None,
                        status.as_ref(),
                        None,
                        false,
                    ),
                    (1024, 768),
                    appearance,
                    theme.clone(),
                );
                assert!(mounted.contains("Swap status"));
                assert!(mounted.contains("Waiting for deposit"));
                assert!(mounted.contains("Settled"));
            }
        }
    }
}
