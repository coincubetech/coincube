use std::collections::{HashMap, HashSet};
use std::convert::TryInto;

use chrono::{DateTime, Local, Utc};
use iced::{
    advanced::widget::Id,
    widget::{scrollable, text::Wrapping, tooltip, Space},
    Alignment, Length,
};

use coincube_ui::{
    component::{
        amount::*, button, card, form, pagination::pagination_controls, text::*,
        transaction::TransactionDirection,
    },
    icon::{self, receipt_icon},
    theme,
    widget::{action::Action, Button, Column, Container, Element, Row},
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
    processing: bool,
) -> Element<'a, Message> {
    let created = replacement_txid.is_some();
    let mut confirm_button = button::primary_action(
        Id::new("vault-rbf-confirm"),
        None,
        "Confirm",
        Message::FocusAction,
    );
    if !created && !processing && (feerate.valid || is_cancel) {
        confirm_button = confirm_button.on_press(Message::CreateRbf(CreateRbfMessage::Confirm));
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
    let mut body = Column::new()
        .spacing(16)
        .width(Length::Fill)
        .push(h4_bold("Transaction replacement"));

    if created {
        body = body
            .push(card::simple(
                Row::new()
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .push(icon::square_check_icon().style(theme::text::success))
                    .push(
                        text("Replacement PSBT created successfully and ready to be signed")
                            .width(Length::Fill)
                            .wrapping(Wrapping::WordOrGlyph)
                            .style(theme::text::success),
                    ),
            ))
            .push(
                Row::new()
                    .spacing(10)
                    .push(Space::new().width(Length::Fill))
                    .push(
                        button::secondary_action(
                            Id::new("vault-rbf-close"),
                            None,
                            "Close",
                            Message::FocusAction,
                        )
                        .on_press(Message::CreateRbf(CreateRbfMessage::Cancel)),
                    )
                    .push(
                        button::primary_action(
                            Id::new("vault-rbf-go-to-replacement"),
                            None,
                            "Go to replacement",
                            Message::FocusAction,
                        )
                        .on_press(Message::Menu(Menu::Vault(
                            VaultSubMenu::PSBTs(replacement_txid),
                        ))),
                    ),
            );
    } else {
        body = body
            .push(p1_bold(if is_cancel {
                "Cancel transaction"
            } else {
                "Bump fee"
            }))
            .push(
                text(help_text)
                    .width(Length::Fill)
                    .wrapping(Wrapping::WordOrGlyph),
            );

        if !descendant_txids.is_empty() {
            let mut descendants: Vec<_> = descendant_txids.iter().collect();
            descendants.sort();
            let list = descendants
                .into_iter()
                .fold(Column::new().spacing(6), |col, txid| {
                    let id = txid.to_string();
                    col.push(
                        Row::new()
                            .spacing(8)
                            .align_y(Alignment::Center)
                            .push(
                                text(id.clone())
                                    .size(CAPTION_SIZE)
                                    .width(Length::Fill)
                                    .wrapping(Wrapping::Glyph),
                            )
                            .push(
                                Action::new(
                                    Id::from(format!("vault-rbf-copy-{id}")),
                                    icon::clipboard_icon().style(theme::text::secondary),
                                    Message::FocusAction,
                                )
                                .on_press(Message::Clipboard(id))
                                .style(theme::button::transparent_border),
                            ),
                    )
                });
            body = body.push(Container::new(Row::new()
                .spacing(10)
                .push(icon::warning_icon())
                .push(text(format!(
                    "Replacing this transaction will invalidate {} later {} when the replacement is broadcast. Transactions depending on them may also be dropped.",
                    descendant_txids.len(),
                    if descendant_txids.len() == 1 { "payment" } else { "payments" },
                )).width(Length::Fill).wrapping(Wrapping::WordOrGlyph)))
                .padding(12)
                .width(Length::Fill)
                .style(theme::card::warning))
                .push(scrollable(list).height(Length::Fixed((descendant_txids.len().min(3) * 56) as f32)));
        }

        body = body.push(if is_cancel {
            Column::new()
                .spacing(5)
                .push(p1_bold("Feerate"))
                .push(text("Set automatically when the cancellation PSBT is created.").style(theme::text::secondary))
        } else {
            Column::new()
                .spacing(5)
                .push(p1_bold("Feerate (sats/vbyte)"))
                .push(
                    if processing {
                        form::Form::new_disabled("", feerate)
                    } else {
                        form::Form::new_trimmed("", feerate, |msg| {
                            Message::CreateRbf(CreateRbfMessage::FeerateEdited(msg))
                        })
                    }
                    .warning("Feerate must be greater than the previous value and at most 1000 sats/vbyte")
                    .size(P1_SIZE)
                    .padding(10),
                )
        });
        if processing {
            body = body.push(text("Creating replacement PSBT…").style(theme::text::secondary));
        }
        body = body.push(
            Row::new()
                .spacing(10)
                .push(Space::new().width(Length::Fill))
                .push(
                    button::secondary_action(
                        Id::new("vault-rbf-back"),
                        None,
                        "Back",
                        Message::FocusAction,
                    )
                    .on_press_maybe(
                        (!processing).then_some(Message::CreateRbf(CreateRbfMessage::Cancel)),
                    ),
                )
                .push(confirm_button),
        );
    }

    card::modal(body).width(Length::Fill).max_width(720).into()
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

#[cfg(test)]
mod rbf_view_tests {
    use super::*;
    use crate::app::view::shared::landing::capture;
    use std::str::FromStr;

    fn txid(hex: char) -> Txid {
        Txid::from_str(&hex.to_string().repeat(64)).unwrap()
    }

    fn rate(value: &str, valid: bool) -> form::Value<String> {
        form::Value {
            value: value.into(),
            valid,
            warning: None,
        }
    }

    #[test]
    fn rbf_review_states_fit_compact_and_wide_in_both_themes() {
        let descendants = HashSet::from([txid('a'), txid('b')]);
        let none = HashSet::new();
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for outer in [(1024, 768), (1440, 900)] {
                let inspect = |scenario,
                               cancel,
                               ids: &HashSet<Txid>,
                               rate: &form::Value<String>,
                               created,
                               processing| {
                    capture::inspect_full_window(
                        scenario,
                        create_rbf_modal(cancel, ids, rate, created, processing),
                        outer,
                        appearance,
                        theme,
                    )
                };
                let bump = inspect(
                    "vault-rbf-bump",
                    false,
                    &none,
                    &rate("12", true),
                    None,
                    false,
                );
                assert!(bump.contains("Bump fee"));
                assert!(bump.contains("Feerate (sats/vbyte)"));
                assert!(bump.has_focus(Id::new("vault-rbf-confirm")));

                let invalid = inspect(
                    "vault-rbf-invalid",
                    false,
                    &none,
                    &rate("3", false),
                    None,
                    false,
                );
                assert!(invalid.contains("Feerate must be greater"));
                assert!(!invalid.has_focus(Id::new("vault-rbf-confirm")));
                assert!(invalid.has_focus(Id::new("vault-rbf-back")));

                let cancel = inspect(
                    "vault-rbf-cancel",
                    true,
                    &none,
                    &rate("5", true),
                    None,
                    false,
                );
                assert!(cancel.contains("Cancel transaction"));
                assert!(cancel.contains("Set automatically"));
                assert!(!cancel.contains("Feerate (sats/vbyte)"));
                assert!(cancel.has_focus(Id::new("vault-rbf-confirm")));

                let warning = inspect(
                    "vault-rbf-descendants",
                    false,
                    &descendants,
                    &rate("12", true),
                    None,
                    false,
                );
                assert!(warning.contains("invalidate 2 later payments"));
                assert!(warning.contains(&txid('a').to_string()));
                assert!(warning.contains(&txid('b').to_string()));
                assert!(warning.has_focus(Id::from(format!("vault-rbf-copy-{}", txid('a')))));

                let created = inspect(
                    "vault-rbf-created",
                    false,
                    &none,
                    &rate("12", true),
                    Some(txid('c')),
                    false,
                );
                assert!(created.contains("Replacement PSBT created successfully"));
                assert!(created.has_focus(Id::new("vault-rbf-go-to-replacement")));
                assert!(!created.has_focus(Id::new("vault-rbf-confirm")));

                for is_cancel in [false, true] {
                    let pending = inspect(
                        "vault-rbf-pending",
                        is_cancel,
                        &none,
                        &rate("12", true),
                        None,
                        true,
                    );
                    assert!(pending.contains("Creating replacement PSBT"));
                    assert!(!pending.has_focus(Id::new("vault-rbf-back")));
                    assert!(!pending.has_focus(Id::new("vault-rbf-confirm")));
                }
            }
        }
    }

    #[test]
    fn rbf_review_keyboard_actions_keep_existing_routes() {
        let none = HashSet::new();
        let valid = rate("12", true);
        let activate = |cancel, rate: &form::Value<String>, created, target: &str| {
            capture::activate(
                create_rbf_modal(cancel, &none, rate, created, false),
                Id::from(target.to_owned()),
                (1440, 900),
                false,
            )
        };
        assert!(activate(false, &valid, None, "vault-rbf-confirm")
            .iter()
            .any(|m| matches!(m, Message::CreateRbf(CreateRbfMessage::Confirm))));
        assert!(activate(true, &valid, None, "vault-rbf-confirm")
            .iter()
            .any(|m| matches!(m, Message::CreateRbf(CreateRbfMessage::Confirm))));
        assert!(activate(false, &valid, None, "vault-rbf-back")
            .iter()
            .any(|m| matches!(m, Message::CreateRbf(CreateRbfMessage::Cancel))));
        let descendants = HashSet::from([txid('a')]);
        assert!(capture::activate(
            create_rbf_modal(false, &descendants, &valid, None, false),
            Id::from(format!("vault-rbf-copy-{}", txid('a'))),
            (1440, 900),
            false,
        )
        .iter()
        .any(|m| matches!(m, Message::Clipboard(id) if id == &txid('a').to_string())));
        assert!(activate(false, &valid, Some(txid('c')), "vault-rbf-go-to-replacement")
            .iter().any(|m| matches!(m, Message::Menu(Menu::Vault(VaultSubMenu::PSBTs(Some(id)))) if *id == txid('c'))));
    }
}
