use std::collections::{HashMap, HashSet};
use std::convert::TryInto;

use chrono::{DateTime, Local, Utc};
use iced::{
    widget::{tooltip, Space},
    Alignment, Length,
};

use coincube_ui::{
    component::{
        amount::*, button, card, form, pagination::pagination_controls, text::*,
        transaction::TransactionDirection,
    },
    icon::{self, receipt_icon},
    theme,
    widget::{Button, Column, Container, Element, Row},
};

use crate::{
    app::{
        cache::Cache,
        menu::{Menu, VaultSubMenu},
        view::{
            dashboard, loading_placeholder,
            message::{CreateRbfMessage, Message},
            placeholder,
            vault::label,
            FiatAmountConverter,
        },
    },
    daemon::model::{HistoryTransaction, Txid},
    export::ImportExportMessage,
};

pub fn transactions_view<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    txs: &'a [HistoryTransaction],
    current_page: u32,
    is_last_page: bool,
    processing: bool,
) -> Element<'a, Message> {
    let fiat_converter = cache.fiat_price.as_ref().and_then(|p| p.try_into().ok());

    // While the initial page-0 fetch is in flight, `txs` is empty but the
    // wallet may well have history — show a loading state instead of the
    // "No transactions" empty state, which would otherwise flash for a few
    // seconds before the data lands.
    let is_initial_empty = txs.is_empty() && current_page == 0;
    let show_loading = is_initial_empty && processing;
    let show_empty_state = is_initial_empty && !processing;

    let list = txs
        .iter()
        .enumerate()
        .fold(Column::new().spacing(2), |col, (i, tx)| {
            col.push(tx_list_view(
                i,
                tx,
                cache.bitcoin_unit,
                fiat_converter,
                cache.show_direction_badges,
                cache.balance_amounts_masked(),
            ))
        });

    let body: Element<'a, Message> = if show_loading {
        loading_placeholder(receipt_icon().size(80), "Loading transactions")
    } else if show_empty_state {
        placeholder(
            receipt_icon().size(80),
            "No transactions yet",
            "Your transaction history will appear here once you send or receive coins.",
        )
    } else {
        // The dashboard already wraps the whole panel in a page-level
        // scrollable, so the list is pushed directly — nesting another
        // scrollable here would make its `Fill` height resolve against an
        // unbounded parent and break layout. Pagination keeps each page
        // bounded (PAGE_SIZE rows), so the page scroll is enough.
        Column::new()
            .spacing(12)
            .push(
                Row::new()
                    .spacing(12)
                    .push(
                        Container::new(text("Time").size(CAPTION_SIZE)).width(Length::Fixed(104.0)),
                    )
                    .push(Container::new(text("Details").size(CAPTION_SIZE)).width(Length::Fill))
                    .push(
                        Container::new(text("Amount").size(CAPTION_SIZE))
                            .width(Length::Fixed(190.0)),
                    )
                    .push(
                        Container::new(text("Status").size(CAPTION_SIZE))
                            .width(Length::Fixed(90.0)),
                    ),
            )
            .push(list)
            .push(pagination_controls(
                Message::VaultPrevPage,
                Message::VaultNextPage,
                current_page > 0,
                !is_last_page,
                processing,
                current_page,
            ))
            .push(Space::new().height(Length::Fixed(30.0)))
            .into()
    };

    dashboard(
        menu,
        cache,
        Column::new()
            .push(
                Row::new()
                    .push(Container::new(h3("Activity").bold()))
                    .push(Space::new().width(Length::Fill))
                    .push(
                        button::secondary(Some(icon::backup_icon()), "Export")
                            .on_press(ImportExportMessage::Open.into()),
                    ),
            )
            .push(body)
            .align_x(Alignment::Center)
            .spacing(16),
    )
}

fn tx_list_view(
    i: usize,
    tx: &HistoryTransaction,
    bitcoin_unit: BitcoinDisplayUnit,
    fiat_converter: Option<FiatAmountConverter>,
    show_direction_badges: bool,
    mask_amounts: bool,
) -> Element<'_, Message> {
    let direction = if tx.is_external() {
        TransactionDirection::Incoming
    } else if tx.is_send_to_self() {
        TransactionDirection::SelfTransfer
    } else {
        TransactionDirection::Outgoing
    };

    // A self-transfer has no outgoing amount by definition — everything it
    // sends lands back in this wallet — so show what it moved (inputs less
    // the fee) rather than a flat zero.
    let amount = if tx.is_external() || tx.is_send_to_self() {
        &tx.incoming_amount
    } else {
        &tx.outgoing_amount
    };

    let label = if let Some(outpoint) = tx.is_single_payment() {
        tx.labels.get(&outpoint.to_string())
    } else {
        tx.labels.get(&tx.tx.compute_txid().to_string())
    };
    let title = label.filter(|label| !label.is_empty()).map_or_else(
        || match direction {
            TransactionDirection::Incoming => "Received Bitcoin",
            TransactionDirection::Outgoing => "Sent Bitcoin",
            TransactionDirection::SelfTransfer => "Self-transfer",
        },
        String::as_str,
    );
    let date = tx
        .time
        .and_then(|t| DateTime::<Utc>::from_timestamp(t as i64, 0))
        .map(|t| t.with_timezone(&Local).format("%b %d, %H:%M").to_string())
        .unwrap_or_else(|| "Pending".to_owned());
    let status = if tx.time.is_none() {
        "Unconfirmed"
    } else {
        "Confirmed"
    };
    let sign = match direction {
        TransactionDirection::Incoming => "+",
        TransactionDirection::Outgoing => "−",
        TransactionDirection::SelfTransfer => "",
    };
    let mut details = Row::new().spacing(8).align_y(Alignment::Center).push(
        coincube_ui::image::asset_network_logo("btc", "bitcoin", 24.0),
    );
    if show_direction_badges {
        details = details.push(text(match direction {
            TransactionDirection::Incoming => "↓",
            TransactionDirection::Outgoing => "↑",
            TransactionDirection::SelfTransfer => "↺",
        }));
    }
    details = details.push(
        Column::new().spacing(2).push(text(title).bold()).push(
            text(if tx.is_batch() {
                "Batch transaction"
            } else {
                "Bitcoin on-chain"
            })
            .size(CAPTION_SIZE)
            .style(theme::text::secondary),
        ),
    );
    let mut amount_column = Column::new().align_x(Alignment::End).spacing(2).push(
        Row::new()
            .spacing(3)
            .align_y(Alignment::Center)
            .push(text(if mask_amounts { "" } else { sign }))
            .push(if mask_amounts {
                Row::new().push(text("••••••").size(P2_SIZE))
            } else {
                amount_with_size_and_unit(amount, P2_SIZE, bitcoin_unit)
            }),
    );
    if let Some(converter) = fiat_converter.filter(|_| !mask_amounts) {
        let fiat = converter.convert(*amount);
        amount_column = amount_column.push(
            text(format!(
                "≈ {} {} · current rate",
                fiat.to_formatted_string(),
                fiat.currency()
            ))
            .size(CAPTION_SIZE)
            .style(theme::text::secondary),
        );
    }
    Container::new(
        Button::new(
            Row::new()
                .spacing(12)
                .align_y(Alignment::Center)
                .push(Container::new(text(date).size(CAPTION_SIZE)).width(Length::Fixed(104.0)))
                .push(details.width(Length::Fill))
                .push(Container::new(amount_column).width(Length::Fixed(190.0)))
                .push(Container::new(text(status).size(CAPTION_SIZE)).width(Length::Fixed(90.0))),
        )
        .padding(10)
        .width(Length::Fill)
        .on_press(Message::Select(i))
        .style(theme::button::transparent_border),
    )
    .style(theme::card::simple)
    .into()
}

/// Return the modal view for a new RBF transaction.
///
/// `descendant_txids` contains the IDs of any transactions from this wallet that are
/// direct descendants of the transaction to be replaced.
pub fn create_rbf_modal<'a>(
    is_cancel: bool,
    descendant_txids: &HashSet<Txid>,
    feerate: &form::Value<String>,
    replacement_txid: Option<Txid>,
) -> Element<'a, Message> {
    let mut confirm_button = button::secondary(None, "Confirm").width(Length::Fixed(200.0));
    if feerate.valid || is_cancel {
        confirm_button =
            confirm_button.on_press(Message::CreateRbf(super::super::CreateRbfMessage::Confirm));
    }
    let help_text = if is_cancel {
        "Replace the transaction with one paying a higher feerate \
        that sends the coins back to your wallet. There is no guarantee \
        the original transaction won't get mined first. New inputs may \
        be used for the replacement transaction."
    } else {
        "Replace the transaction with one paying a higher feerate \
        to incentivize faster confirmation. New inputs may be used \
        for the replacement transaction."
    };
    card::simple(
        Column::new()
            .spacing(10)
            .push(Container::new(h4_bold("Transaction replacement")).width(Length::Fill))
            .push(Row::new().push(text(help_text)))
            .push(if descendant_txids.is_empty() {
                None
            } else {
                Some(
                    descendant_txids.iter().fold(
                        Column::new()
                            .spacing(5)
                            .push(Row::new().spacing(10).push(icon::warning_icon()).push(text(
                                if descendant_txids.len() > 1 {
                                    "WARNING: Replacing this transaction \
                                    will invalidate some later payments."
                                } else {
                                    "WARNING: Replacing this transaction \
                                    will invalidate a later payment."
                                },
                            )))
                            .push(Row::new().padding([0, 30]).push(text(
                                if descendant_txids.len() > 1 {
                                    "The following transactions are \
                                    spending one or more outputs \
                                    from the transaction to be replaced \
                                    and will be dropped when the replacement \
                                    is broadcast, along with any other \
                                    transactions that depend on them:"
                                } else {
                                    "The following transaction is \
                                    spending one or more outputs \
                                    from the transaction to be replaced \
                                    and will be dropped when the replacement \
                                    is broadcast, along with any other \
                                    transactions that depend on it:"
                                },
                            ))),
                        |col, txid| {
                            col.push(
                                Row::new()
                                    .padding([0, 30])
                                    .spacing(5)
                                    .align_y(Alignment::Center)
                                    .push(text(txid.to_string()))
                                    .push(
                                        Button::new(
                                            icon::clipboard_icon().style(theme::text::secondary),
                                        )
                                        .on_press(Message::Clipboard(txid.to_string()))
                                        .style(theme::button::transparent_border),
                                    ),
                            )
                        },
                    ),
                )
            })
            .push(if !is_cancel {
                Some(
                    Row::new()
                        .push(Container::new(p1_bold("Feerate")).padding(10))
                        .spacing(10)
                        .push(
                            if replacement_txid.is_none() {
                                form::Form::new_trimmed("", feerate, move |msg| {
                                    Message::CreateRbf(CreateRbfMessage::FeerateEdited(msg))
                                })
                                .warning(
                                    "Feerate must be greater than previous value and \
                                    less than or equal to 1000 sats/vbyte",
                                )
                            } else {
                                form::Form::new_disabled("", feerate)
                            }
                            .size(P1_SIZE)
                            .padding(10),
                        )
                        .width(Length::Fill),
                )
            } else {
                None
            })
            .push(Row::new().push(if replacement_txid.is_none() {
                Row::new().push(confirm_button)
            } else {
                Row::new()
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .push(icon::square_check_icon().style(theme::text::secondary))
                    .push(
                        text("Replacement PSBT created successfully and ready to be signed")
                            .style(theme::text::success),
                    )
            }))
            .push(replacement_txid.map(|id| {
                Row::new().push(
                    button::primary(None, "Go to replacement")
                        .width(Length::Fixed(200.0))
                        .on_press(Message::Menu(Menu::Vault(VaultSubMenu::PSBTs(Some(id))))),
                )
            })),
    )
    .width(Length::Fixed(800.0))
    .into()
}

pub fn transaction_detail_view<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    tx: &'a HistoryTransaction,
    labels_editing: &'a HashMap<String, form::Value<String>>,
    bitcoin_unit: BitcoinDisplayUnit,
    supports_vault_rbf: bool,
) -> Element<'a, Message> {
    let txid = tx.tx.compute_txid().to_string();
    let display_amount = if tx.is_external() || tx.is_send_to_self() {
        &tx.incoming_amount
    } else {
        &tx.outgoing_amount
    };
    let status = if tx.time.is_some() {
        "Confirmed"
    } else {
        "Unconfirmed · awaiting a Bitcoin confirmation"
    };
    let back_btn: Element<'_, Message> = button::secondary(Some(icon::previous_icon()), "Back")
        .width(Length::Fixed(150.0))
        .on_press(Message::Close)
        .into();

    dashboard(
        menu,
        cache,
        Column::new()
            .push(back_btn)
            .push(if tx.is_send_to_self() {
                Container::new(h3("Self-transfer")).width(Length::Fill)
            } else if tx.is_external() {
                Container::new(h3("Received Bitcoin")).width(Length::Fill)
            } else {
                Container::new(h3("Sent Bitcoin")).width(Length::Fill)
            })
            .push(text("Vault · Bitcoin on-chain").size(P2_SIZE).style(theme::text::secondary))
            .push(if let Some(outpoint) = tx.is_single_payment() {
                // if the payment is a payment of a single payment transaction then
                // the label of the transaction is attached to the label of the payment outpoint
                let outpoint = outpoint.to_string();
                if let Some(label) = labels_editing.get(&outpoint) {
                    label::label_editing(vec![outpoint.clone(), txid.clone()], label, H3_SIZE)
                } else {
                    label::label_editable(
                        vec![outpoint.clone(), txid.clone()],
                        tx.labels.get(&outpoint),
                        H3_SIZE,
                    )
                }
            } else if let Some(label) = labels_editing.get(&txid) {
                label::label_editing(vec![txid.clone()], label, H3_SIZE)
            } else {
                label::label_editable(vec![txid.clone()], tx.labels.get(&txid), H1_SIZE)
            })
            .push(
                Column::new().spacing(20).push(
                    Column::new()
                        .push(Container::new(if cache.balance_amounts_masked() {
                            Row::new().push(text("••••••").size(H1_SIZE))
                        } else {
                            amount_with_size_and_unit(display_amount, H1_SIZE, bitcoin_unit)
                        }))
                        .push(cache.fiat_price.as_ref().and_then(|price| price.try_into().ok()).filter(|_| !cache.balance_amounts_masked()).map(|converter: FiatAmountConverter| {
                            let fiat = converter.convert(*display_amount);
                            text(format!("≈ {} {} · current rate", fiat.to_formatted_string(), fiat.currency()))
                                .size(P2_SIZE)
                                .style(theme::text::secondary)
                        }))
                        .push(text(status).size(P2_SIZE))
                        .push(tx.fee_amount.map(|fee_amount| {
                            Row::new()
                                .align_y(Alignment::Center)
                                .push(h3("Miner fee: ").style(theme::text::secondary))
                                .push(if cache.balance_amounts_masked() {
                                    Row::new().push(text("••••••").size(H3_SIZE))
                                } else {
                                    amount_with_size_and_unit(&fee_amount, H3_SIZE, bitcoin_unit)
                                })
                                .push(text(" ").size(H3_SIZE))
                                .push(
                                    text(if cache.balance_amounts_masked() {
                                        "••••••".to_string()
                                    } else {
                                        format!("({} sats/vbyte)", fee_amount.to_sat() / tx.tx.vsize() as u64)
                                    })
                                    .size(H4_SIZE)
                                    .style(theme::text::secondary),
                                )
                        })),
                ),
            )
            // If unconfirmed, give option to use RBF.
            // Check fee amount is some as otherwise we may be missing coins for this transaction.
            .push(if supports_vault_rbf && tx.time.is_none() && tx.fee_amount.is_some() {
                Some(
                    Row::new()
                        .push(
                            button::secondary(None, "Bump fee")
                                .width(Length::Fixed(200.0))
                                .on_press(Message::CreateRbf(super::super::CreateRbfMessage::New(false))),
                        )
                        .push(
                            tooltip::Tooltip::new(
                                button::secondary(None, "Cancel transaction")
                                .width(Length::Fixed(200.0))
                                .on_press(Message::CreateRbf(super::super::CreateRbfMessage::New(true))),
                                "Best effort attempt at double spending an unconfirmed outgoing transaction",
                                tooltip::Position::Top,
                            )
                        )
                        .spacing(10),
                )
            } else {
                None
            })
            .push(card::simple(
                Column::new()
                    .push(tx.time.and_then(|t| DateTime::<Utc>::from_timestamp(t as i64, 0)).map(|t| {
                        let date = t.with_timezone(&Local).format("%b. %d, %Y - %T %Z");
                        Row::new()
                            .width(Length::Fill)
                            .push(Container::new(text("Date:").bold()).width(Length::Fill))
                            .push(Container::new(text(format!("{}", date))).width(Length::Shrink))
                    }))
                    .push(
                        Row::new()
                            .width(Length::Fill)
                            .align_y(Alignment::Center)
                            .push(Container::new(text("Txid:").bold()).width(Length::Fill))
                            .push(
                                Row::new()
                                    .align_y(Alignment::Center)
                                    .push(Container::new(text(txid.clone()).small()))
                                    .push(
                                        Button::new(icon::clipboard_icon())
                                            .on_press(Message::Clipboard(txid.clone()))
                                            .style(theme::button::transparent_border),
                                    )
                                    .width(Length::Shrink),
                            ),
                    )
                    .spacing(5),
            ))
            .push(
                Column::new()
                    .spacing(20)
                    // We do not need to display inputs for external incoming transactions
                    .push(if tx.is_external() {
                        None
                    } else {
                        Some(super::psbt::history_inputs_view(
                            &tx.coins,
                            &tx.tx,
                            &tx.labels,
                            labels_editing,
                            bitcoin_unit,
                            cache.balance_amounts_masked(),
                        ))
                    })
                    .push(super::psbt::history_outputs_view(
                        &tx.tx,
                        cache.network,
                        &tx.change_indexes,
                        &tx.labels,
                        labels_editing,
                        bitcoin_unit,
                        tx.is_single_payment().is_some(),
                        tx.is_external(),
                        cache.balance_amounts_masked(),
                    )),
            )
            .spacing(20),
    )
}
