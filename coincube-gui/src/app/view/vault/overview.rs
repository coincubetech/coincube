use chrono::{DateTime, Local, Utc};
use std::{collections::HashMap, time::Duration, vec};

use iced::{
    widget::{Container, Row, Space},
    Alignment::{self, Center},
    Length,
};

use coincube_core::miniscript::bitcoin;
use coincube_ui::{
    component::{amount::*, button, card, form, spinner, text::*},
    icon::{self, cross_icon},
    theme,
    widget::{Button, Column, ColumnExt, Element},
};

use crate::{
    app::{
        cache::Cache,
        menu::{self, Menu, VaultSubMenu},
        settings::display::DisplayMode,
        view::{
            dashboard, loading_placeholder,
            message::Message,
            shared::activity_row::{
                activity_row, ActivityDirection, ActivityIdentity, ActivityRow, ActivityStatus,
            },
            shared::landing::{
                amount_cell, landing_balance_section, landing_button, landing_control_id,
                landing_subtle_button_style, AmountCell, AmountTier,
            },
            vault::coins,
            vault::label,
            wallet_header::{wallet_header, HeaderVariant, SyncState, WalletHeaderProps},
            FiatAmountConverter,
        },
        wallet::SyncStatus,
    },
    daemon::model::{HistoryTransaction, Payment, PaymentKind, TransactionKind},
};

const RESCAN_DATE_PROMPT: &str = "This Vault was restored from a Recovery Kit that doesn't record when the wallet was created, so its past transactions can't be found without a start date. Pick one to scan the blockchain from.";

fn rescan_date_prompt<'a>(cube_id: &str) -> Element<'a, Message> {
    Container::new(
        Column::new()
            .spacing(10)
            .push(
                Row::new()
                    .spacing(5)
                    .push(icon::warning_icon().style(theme::text::warning))
                    .push(text(RESCAN_DATE_PROMPT).style(theme::text::warning))
                    .align_y(Center),
            )
            .push(
                Row::new()
                    .spacing(5)
                    .push(Space::new().width(Length::Fill))
                    .push(landing_button(
                        landing_control_id(cube_id, "Vault", "rescan-pick-date"),
                        "Pick a date",
                        None,
                        Message::FocusAction,
                        Some(Message::Menu(Menu::Vault(menu::VaultSubMenu::Settings(
                            Some(menu::SettingsOption::Node),
                        )))),
                        theme::button::secondary,
                    ))
                    .push(landing_button(
                        landing_control_id(cube_id, "Vault", "rescan-dismiss"),
                        "Dismiss",
                        Some(cross_icon()),
                        Message::FocusAction,
                        Some(Message::HideRescanPrompt),
                        theme::button::secondary,
                    )),
            ),
    )
    .padding(25)
    .style(theme::card::border)
    .into()
}

/// How the balance header should present the Vault's balance.
///
/// An empty balance is only shown as an amount when the Vault knows its history.
/// While a full scan is still loading it, or syncing keeps failing before it has
/// loaded, or a restore is waiting on a rescan date, the empty database says nothing
/// about the funds — and a confident "$0.00" is exactly what led a user with a
/// funded, restored Vault to believe the funds were gone.
pub(crate) fn balance_sync_state(
    sync_status: &SyncStatus,
    total_balance: bitcoin::Amount,
    awaiting_rescan_date: bool,
) -> SyncState {
    let empty = total_balance == bitcoin::Amount::ZERO;
    let awaiting_label = "Balance unknown until this Vault's history is scanned. \
                          Pick the date it was created to start.";
    match sync_status {
        // However far the node or the wallet has synced, a restore awaiting its
        // rescan date has no history: its zero is not a balance.
        SyncStatus::Synced | SyncStatus::LatestWalletSync if empty && awaiting_rescan_date => {
            SyncState::Unknown {
                progress: None,
                label: awaiting_label.to_string(),
                failing: true,
            }
        }
        SyncStatus::BlockchainSync(progress) if empty && awaiting_rescan_date => {
            SyncState::Unknown {
                progress: Some(*progress),
                label: "Syncing blockchain; the balance stays unknown until this Vault's \
                        history is scanned"
                    .to_string(),
                failing: false,
            }
        }
        SyncStatus::Synced => SyncState::Synced,
        SyncStatus::BlockchainSync(progress) => SyncState::Syncing {
            progress: Some(*progress),
            label: "Syncing blockchain".to_string(),
        },
        SyncStatus::WalletFullScan { progress } if empty => SyncState::Unknown {
            progress: *progress,
            label: "Loading Vault history".to_string(),
            failing: false,
        },
        SyncStatus::WalletFullScan { progress } => SyncState::Syncing {
            progress: *progress,
            label: "Syncing".to_string(),
        },
        SyncStatus::LatestWalletSync => SyncState::Checking,
        SyncStatus::SyncFailing {
            message,
            since,
            retry_progress,
        } if empty => SyncState::Unknown {
            progress: None,
            label: format!(
                "Balance unavailable: syncing has failed since {}{}. {}",
                format_failure_time(*since),
                retry_note(*retry_progress),
                failure_detail(message),
            ),
            failing: true,
        },
        SyncStatus::SyncFailing {
            message,
            since,
            retry_progress,
        } => SyncState::Stale {
            message: format!(
                "Last known balance: syncing has failed since {}{}. {}",
                format_failure_time(*since),
                retry_note(*retry_progress),
                failure_detail(message),
            ),
        },
    }
}

/// ", retrying (x%)" while a full scan is retrying after failures, so a retry that
/// is getting somewhere does not read as failed throughout.
fn retry_note(retry_progress: Option<f64>) -> String {
    retry_progress
        .map(|p| format!(", retrying ({:.1}%)", 100.0 * p))
        .unwrap_or_default()
}

/// When a run of failed polls began, in local time.
fn format_failure_time(since: u32) -> String {
    DateTime::<Utc>::from_timestamp(i64::from(since), 0)
        .map(|at| at.with_timezone(&Local).format("%b %-d, %H:%M").to_string())
        .unwrap_or_else(|| "recently".to_string())
}

/// The daemon's failure message, trimmed to fit under the balance. The daemon log
/// keeps the full text.
fn failure_detail(message: &str) -> String {
    const MAX_CHARS: usize = 160;
    let message = message.trim();
    if message.chars().count() <= MAX_CHARS {
        return message.to_string();
    }
    let truncated: String = message.chars().take(MAX_CHARS).collect();
    format!("{}…", truncated.trim_end())
}

#[allow(clippy::too_many_arguments)]
pub fn vault_overview_view<'a>(
    balance: &'a bitcoin::Amount,
    unconfirmed_balance: &'a bitcoin::Amount,
    remaining_sequence: &Option<u32>,
    fiat_converter: Option<FiatAmountConverter>,
    expiring_coins: &[bitcoin::OutPoint],
    events: &'a [Payment],
    cube_id: &str,
    is_last_page: bool,
    processing: bool,
    loading: bool,
    history_loaded: bool,
    history_error: Option<&str>,
    sync_status: &SyncStatus,
    show_rescan_prompt: bool,
    awaiting_rescan_date: bool,
    bitcoin_unit: BitcoinDisplayUnit,
    node_bitcoind_sync_progress: Option<f64>,
    node_bitcoind_ibd: Option<bool>,
    _show_direction_badges: bool,
    display_mode: DisplayMode,
) -> Element<'a, Message> {
    // Show external-unconfirmed coins as part of the headline balance
    // (matching the Cube → Overview Vault card). The per-transaction
    // "Unconfirmed" pill in the recent-payments list below still
    // surfaces which inputs are pending — we deliberately omit the
    // `wallet_header` "+ X SATS unconfirmed" breakout here because
    // its leading `+` reads as additive next to an already-inclusive
    // headline.
    let total_balance = *balance + *unconfirmed_balance;
    let fiat_balance = fiat_converter.as_ref().map(|c| c.convert(total_balance));
    let sync = balance_sync_state(sync_status, total_balance, awaiting_rescan_date);
    let history_unknown = matches!(sync, SyncState::Unknown { .. });
    let btc_fiat_str = fiat_balance
        .as_ref()
        .map(|f| format!("{} {}", f.to_formatted_string(), f.currency()))
        .unwrap_or_default();
    let vault_btc_row = Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push(coincube_ui::image::asset_network_logo::<Message>(
            "btc", "bitcoin", 40.0,
        ))
        .push(text("BTC").size(P1_SIZE).bold().width(Length::Fixed(60.0)))
        .push(amount_cell::<Message>(
            if history_unknown {
                AmountCell::unknown("Vault history has not loaded")
            } else {
                AmountCell::shown(
                    format!(
                        "{} {}",
                        total_balance.to_formatted_string_with_unit(bitcoin_unit),
                        bitcoin_unit.label()
                    ),
                    (!btc_fiat_str.is_empty()).then_some(btc_fiat_str),
                )
            },
            AmountTier::Row,
        ))
        .push(Space::new().width(Length::Fill))
        .push(
            landing_button(
                landing_control_id(cube_id, "Vault", "send-btc"),
                "Send",
                None,
                Message::FocusAction,
                Some(Message::Menu(Menu::Vault(VaultSubMenu::Send))),
                theme::button::primary,
            )
            .width(Length::Fixed(90.0)),
        )
        .push(
            landing_button(
                landing_control_id(cube_id, "Vault", "receive-btc"),
                "Receive",
                None,
                Message::FocusAction,
                Some(Message::Menu(Menu::Vault(VaultSubMenu::Receive))),
                theme::button::orange_outline,
            )
            .width(Length::Fixed(90.0)),
        );
    Container::new(
        Column::new()
            .push(landing_balance_section(
                Column::new()
                    .spacing(16)
                    .push(
                        Column::new()
                            .spacing(8)
                            .push(h4_bold("Balance"))
                            .push(wallet_header::<Message>(WalletHeaderProps {
                                sats: total_balance,
                                fiat: fiat_balance,
                                balance_masked: false,
                                bitcoin_unit,
                                variant: HeaderVariant::Landing,
                                sync,
                                unconfirmed: None,
                                pending_send_sats: 0,
                                pending_receive_sats: 0,
                                display_mode,
                                on_swap: Some(Message::FlipDisplayMode),
                                on_swap_focus: Some((
                                    landing_control_id(cube_id, "Vault", "flip-balance"),
                                    Message::FocusAction,
                                )),
                            })),
                    )
                    .push(iced::widget::rule::horizontal(1))
                    .push(vault_btc_row),
            ))
            .push(show_rescan_prompt.then_some(rescan_date_prompt(cube_id)))
            .push(match (node_bitcoind_ibd, node_bitcoind_sync_progress) {
                (Some(true), Some(progress)) => Some(
                    Container::new(
                        Row::new()
                            .spacing(10)
                            .align_y(Alignment::Center)
                            .push(
                                text(format!(
                                    "Your local node is syncing — {:.1}% complete",
                                    100.0 * progress
                                ))
                                .style(theme::text::secondary)
                                .width(Length::Fill),
                            )
                            .push(spinner::typing_text_carousel(
                                "...",
                                true,
                                Duration::from_millis(2000),
                                |content| text(content).style(theme::text::secondary),
                            ))
                            .width(Length::Fill),
                    )
                    .padding(15)
                    .style(theme::card::border),
                ),
                _ => None,
            })
            .push(if expiring_coins.is_empty() {
                remaining_sequence.map(|sequence| {
                    Container::new(
                        Row::new()
                            .spacing(15)
                            .align_y(Alignment::Center)
                            .push(
                                h4_regular(format!(
                                    "≈ {} left before first recovery path becomes available.",
                                    coins::expire_message_units(sequence).join(", ")
                                ))
                                .width(Length::Fill),
                            )
                            .push(
                                icon::tooltip_icon()
                                    .size(20)
                                    .style(theme::text::secondary)
                                    .width(Length::Fixed(20.0)),
                            )
                            .width(Length::Fill),
                    )
                    .padding(25)
                    .style(theme::card::border)
                })
            } else {
                Some(
                    Container::new(
                        Row::new()
                            .spacing(15)
                            .align_y(Alignment::Center)
                            .push(
                                h4_regular(format!(
                                    "Recovery path is or will soon be available for {} coin(s).",
                                    expiring_coins.len(),
                                ))
                                .width(Length::Fill),
                            )
                            .push(landing_button(
                                landing_control_id(cube_id, "Vault", "refresh-coins"),
                                "Refresh coins",
                                Some(icon::arrow_repeat()),
                                Message::FocusAction,
                                Some(Message::Menu(Menu::Vault(
                                    crate::app::menu::VaultSubMenu::Coins(Some(
                                        expiring_coins.to_owned(),
                                    )),
                                ))),
                                theme::button::primary,
                            )),
                    )
                    .padding(25)
                    .style(theme::card::invalid),
                )
            })
            .push(
                Column::new()
                    .spacing(10)
                    .push(h4_bold("Recent activity"))
                    .push_maybe(history_error.map(|error| {
                        text(if history_loaded {
                            format!("Last known activity · {error}")
                        } else {
                            error.to_owned()
                        })
                        .size(P2_SIZE)
                        .style(theme::text::warning)
                    }))
                    .push_maybe(loading.then(|| {
                        loading_placeholder(
                            icon::receipt_icon().size(48),
                            if history_unknown {
                                // Nothing is loading yet: the list is read once the
                                // history is in, and saying "Loading" until then is
                                // what made an unfinished scan look like a slow page.
                                "Transactions appear once the Vault's history has loaded"
                            } else {
                                "Loading transactions"
                            },
                        )
                    }))
                    .push(events.iter().fold(Column::new().spacing(10), |col, event| {
                        // Change outputs are skipped — their transaction is already
                        // listed as the outgoing payment. A self-transfer has no
                        // such counterpart row, so it stays.
                        if event.kind != PaymentKind::SendToSelf {
                            col.push(event_list_view(
                                event,
                                cube_id,
                                bitcoin_unit,
                                fiat_converter,
                            ))
                        } else {
                            col
                        }
                    }))
                    .push(if !is_last_page && !events.is_empty() {
                        Some(
                            Container::new(
                                landing_button(
                                    landing_control_id(cube_id, "Vault", "see-more"),
                                    if processing {
                                        "Fetching ..."
                                    } else {
                                        "See more"
                                    },
                                    None,
                                    Message::FocusAction,
                                    (!processing).then_some(Message::Next),
                                    theme::button::transparent_border,
                                )
                                .width(Length::Fill)
                                .padding(15),
                            )
                            .width(Length::Fill)
                            .style(theme::card::simple),
                        )
                    } else {
                        None
                    }),
            )
            .push_maybe(if history_loaded {
                Some(
                    Container::new(landing_button(
                        landing_control_id(cube_id, "Vault", "history"),
                        "View All Transactions",
                        None,
                        Message::FocusAction,
                        Some(Message::Menu(Menu::Vault(VaultSubMenu::Transactions(None)))),
                        landing_subtle_button_style,
                    ))
                    .width(Length::Fill)
                    .center_x(Length::Fill),
                )
            } else {
                None
            })
            .push(Space::new().height(Length::Fixed(40.0)))
            .spacing(20),
    )
    .padding([0, 16])
    .into()
}

fn event_list_view(
    event: &Payment,
    cube_id: &str,
    bitcoin_unit: BitcoinDisplayUnit,
    fiat_converter: Option<FiatAmountConverter>,
) -> Element<'static, Message> {
    let direction = match event.kind {
        PaymentKind::Incoming => ActivityDirection::Incoming,
        PaymentKind::SelfTransfer | PaymentKind::SendToSelf => ActivityDirection::SelfTransfer,
        PaymentKind::Outgoing => ActivityDirection::Outgoing,
    };

    let label = event
        .label
        .clone()
        .or_else(|| {
            event
                .address_label
                .as_ref()
                .map(|label| format!("address label: {label}"))
        })
        .unwrap_or_else(|| match direction {
            ActivityDirection::Incoming => "Received".into(),
            ActivityDirection::Outgoing => "Sent".into(),
            ActivityDirection::SelfTransfer => "Self-transfer".into(),
        });
    let amount = format!(
        "{} {}",
        event.amount.to_formatted_string_with_unit(bitcoin_unit),
        bitcoin_unit.label()
    );
    let amount = match direction {
        ActivityDirection::Outgoing => format!("−{amount}"),
        ActivityDirection::Incoming => format!("+{amount}"),
        ActivityDirection::SelfTransfer => amount,
    };
    let fiat_amount = fiat_converter.map(|converter| {
        let fiat = converter.convert(event.amount);
        format!(
            "{} {} · current rate",
            fiat.to_formatted_string(),
            fiat.currency()
        )
    });
    activity_row(
        ActivityRow {
            identity: ActivityIdentity {
                cube: cube_id.to_owned(),
                wallet: "Vault".into(),
                payment: Some(format!("{}:{}", event.outpoint.txid, event.outpoint.vout)),
            },
            rail: Some("Bitcoin on-chain".into()),
            date: event
                .time
                .as_ref()
                .map(|time| {
                    time.with_timezone(&Local)
                        .format("%b %-d, %H:%M")
                        .to_string()
                })
                .unwrap_or_else(|| "Time unknown".into()),
            timezone_tooltip: Some("Local time".into()),
            direction,
            label,
            wallet_caption: None,
            amount: AmountCell::shown(amount, fiat_amount),
            status: if event.time.is_some() {
                ActivityStatus::Complete
            } else {
                ActivityStatus::Unconfirmed
            },
        },
        false,
        Some(Message::SelectPayment(event.outpoint)),
        Message::FocusAction,
    )
}

pub fn payment_view<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    tx: &'a HistoryTransaction,
    output_index: usize,
    labels_editing: &'a HashMap<String, form::Value<String>>,
) -> Element<'a, Message> {
    let txid = tx.tx.compute_txid().to_string();
    let outpoint = bitcoin::OutPoint {
        txid: tx.tx.compute_txid(),
        vout: output_index as u32,
    }
    .to_string();
    dashboard(
        menu,
        cache,
        Column::new()
            .push(match tx.kind {
                TransactionKind::OutgoingSinglePayment(_)
                | TransactionKind::OutgoingPaymentBatch(_) => {
                    Container::new(h3("Outgoing payment")).width(Length::Fill)
                }
                TransactionKind::IncomingSinglePayment(_)
                | TransactionKind::IncomingPaymentBatch(_) => {
                    Container::new(h3("Incoming payment")).width(Length::Fill)
                }
                _ => Container::new(h3("Payment")).width(Length::Fill),
            })
            .push(if tx.is_single_payment().is_some() {
                // if the payment is a payment of a single payment transaction then
                // the label of the transaction is attached to the label of the payment outpoint
                if let Some(label) = labels_editing.get(&outpoint) {
                    label::label_editing(vec![outpoint.clone(), txid.clone()], label, H3_SIZE)
                } else {
                    label::label_editable(
                        vec![outpoint.clone(), txid.clone()],
                        tx.labels.get(&outpoint),
                        H3_SIZE,
                    )
                }
            } else if let Some(label) = labels_editing.get(&outpoint) {
                label::label_editing(vec![outpoint.clone()], label, H3_SIZE)
            } else {
                label::label_editable(vec![outpoint.clone()], tx.labels.get(&outpoint), H3_SIZE)
            })
            .push(Container::new(amount_with_size(
                &tx.tx.output[output_index].value,
                H3_SIZE,
            )))
            .push(Space::new().height(H3_SIZE))
            .push(Container::new(h3("Transaction")).width(Length::Fill))
            .push(if tx.is_batch() {
                if let Some(label) = labels_editing.get(&txid) {
                    Some(label::label_editing(vec![txid.clone()], label, H3_SIZE))
                } else {
                    Some(label::label_editable(
                        vec![txid.clone()],
                        tx.labels.get(&txid),
                        H3_SIZE,
                    ))
                }
            } else {
                None
            })
            .push(tx.fee_amount.map(|fee_amount| {
                Row::new()
                    .align_y(Alignment::Center)
                    .push(h3("Miner fee: ").style(theme::text::secondary))
                    .push(amount_with_size(&fee_amount, H3_SIZE))
                    .push(text(" ").size(H3_SIZE))
                    .push(
                        text(format!(
                            "({} sats/vbyte)",
                            fee_amount.to_sat() / tx.tx.vsize() as u64
                        ))
                        .size(H4_SIZE)
                        .style(theme::text::secondary),
                    )
            }))
            .push(card::simple(
                Column::new()
                    .push(tx.time.map(|t| {
                        let date = DateTime::<Utc>::from_timestamp(t as i64, 0)
                            .unwrap()
                            .with_timezone(&Local)
                            .format("%b. %d, %Y - %T");
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
                                    .push(Container::new(
                                        text(format!("{}", tx.tx.compute_txid())).small(),
                                    ))
                                    .push(
                                        Button::new(icon::clipboard_icon())
                                            .on_press(Message::Clipboard(
                                                tx.tx.compute_txid().to_string(),
                                            ))
                                            .style(theme::button::transparent_border),
                                    )
                                    .width(Length::Shrink),
                            ),
                    )
                    .spacing(5),
            ))
            .push(
                button::secondary(None, "See transaction details").on_press(Message::Menu(
                    Menu::Vault(VaultSubMenu::Transactions(Some(tx.tx.compute_txid()))),
                )),
            )
            .spacing(20),
    )
}

/// Full-screen celebration view when a vault payment is received.
pub fn received_celebration_page<'a>(
    context: &str,
    amount_display: &'a str,
    quote: &'a coincube_ui::component::quote_display::Quote,
    image_handle: &'a iced::widget::image::Handle,
) -> Element<'a, Message> {
    coincube_ui::component::received_celebration_page(
        context,
        amount_display,
        quote,
        image_handle,
        "has arrived.",
        Message::DismissReceivedCelebration,
    )
}

#[cfg(test)]
mod balance_state_tests {
    use super::{balance_sync_state, failure_detail};
    use crate::app::{view::wallet_header::SyncState, wallet::SyncStatus};
    use coincube_core::miniscript::bitcoin::Amount;

    /// The incident: a restored Vault's history was still loading and the header said
    /// "$0.00" as though that were its balance. An empty balance during a full scan is
    /// now shown as unknown.
    #[test]
    fn an_empty_balance_is_unknown_while_the_history_loads() {
        let state = balance_sync_state(
            &SyncStatus::WalletFullScan {
                progress: Some(0.25),
            },
            Amount::ZERO,
            false,
        );
        assert!(matches!(
            state,
            SyncState::Unknown {
                progress: Some(p),
                failing: false,
                ..
            } if p == 0.25
        ));
    }

    /// A Vault that already holds coins keeps showing them, pulsing, while it rescans.
    #[test]
    fn a_known_balance_stays_visible_while_rescanning() {
        let state = balance_sync_state(
            &SyncStatus::WalletFullScan { progress: None },
            Amount::from_sat(1),
            false,
        );
        assert!(matches!(state, SyncState::Syncing { .. }));
    }

    #[test]
    fn failing_sync_hides_an_empty_balance_and_flags_a_known_one() {
        let failing = SyncStatus::SyncFailing {
            message: "Esplora client error".into(),
            since: 0,
            retry_progress: None,
        };
        match balance_sync_state(&failing, Amount::ZERO, false) {
            SyncState::Unknown {
                failing: true,
                label,
                ..
            } => assert!(label.contains("Esplora client error"), "{}", label),
            _ => panic!("an empty balance must not be shown while syncing fails"),
        }
        match balance_sync_state(&failing, Amount::from_sat(5), false) {
            SyncState::Stale { message } => {
                assert!(message.starts_with("Last known balance"), "{}", message)
            }
            _ => panic!("a known balance must be flagged as possibly stale"),
        }
    }

    /// A restore still waiting for a rescan date has no history at all: its "0" is
    /// not a balance either.
    #[test]
    fn a_vault_awaiting_its_rescan_date_has_no_balance_to_show() {
        assert!(matches!(
            balance_sync_state(&SyncStatus::Synced, Amount::ZERO, true),
            SyncState::Unknown { failing: true, .. }
        ));
        assert!(matches!(
            balance_sync_state(&SyncStatus::Synced, Amount::from_sat(5), true),
            SyncState::Synced
        ));
        assert!(matches!(
            balance_sync_state(&SyncStatus::Synced, Amount::ZERO, false),
            SyncState::Synced
        ));
        // Nor while the node or the wallet is still syncing: the blockchain's
        // progress is kept, but no zero is shown.
        assert!(matches!(
            balance_sync_state(&SyncStatus::BlockchainSync(0.4), Amount::ZERO, true),
            SyncState::Unknown {
                progress: Some(p),
                failing: false,
                ..
            } if p == 0.4
        ));
        assert!(matches!(
            balance_sync_state(&SyncStatus::LatestWalletSync, Amount::ZERO, true),
            SyncState::Unknown { failing: true, .. }
        ));
        // A funded Vault keeps showing its coins while syncing.
        assert!(matches!(
            balance_sync_state(&SyncStatus::BlockchainSync(0.4), Amount::from_sat(5), true),
            SyncState::Syncing { .. }
        ));
        assert!(matches!(
            balance_sync_state(&SyncStatus::LatestWalletSync, Amount::from_sat(5), true),
            SyncState::Checking
        ));
    }

    #[test]
    fn a_retrying_scan_shows_its_progress_alongside_the_failure() {
        let retrying = SyncStatus::SyncFailing {
            message: "Esplora client error".into(),
            since: 0,
            retry_progress: Some(0.5),
        };
        match balance_sync_state(&retrying, Amount::ZERO, false) {
            SyncState::Unknown { label, .. } => {
                assert!(label.contains("retrying (50.0%)"), "{}", label)
            }
            _ => panic!("an empty balance must not be shown while syncing fails"),
        }
    }

    #[test]
    fn long_failure_messages_are_trimmed() {
        let long = "x".repeat(500);
        let detail = failure_detail(&long);
        assert_eq!(detail.chars().count(), 161);
        assert!(detail.ends_with('…'));
        assert_eq!(failure_detail("  short  "), "short");
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    use crate::app::view::shared::landing::capture;
    use coincube_core::miniscript::bitcoin::{OutPoint, Txid};
    use std::str::FromStr;

    fn event(known_time: bool) -> Payment {
        Payment {
            label: Some("Long Vault beneficiary label that should wrap safely on a compact landing while the amount remains right aligned".into()),
            address: None,
            address_label: None,
            amount: bitcoin::Amount::from_sat(99_999_999_999),
            outpoint: OutPoint {
                txid: Txid::from_str(&"1".repeat(64)).unwrap(),
                vout: 0,
            },
            time: known_time.then(|| DateTime::<Utc>::from_timestamp(1_722_000_000, 0).unwrap()),
            kind: PaymentKind::Incoming,
        }
    }

    #[test]
    fn r06a_vault_headless_content_matrix() {
        let normal_events = vec![event(true), event(false)];
        let balance = bitcoin::Amount::from_sat(250_000);
        let zero = bitcoin::Amount::ZERO;
        let none_sequence = None;
        let failing = SyncStatus::SyncFailing {
            message: "Esplora refresh failed".into(),
            since: 1_722_000_000,
            retry_progress: Some(0.5),
        };
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for outer in [(1024, 768), (1440, 900)] {
                macro_rules! render {
                    ($balance:expr, $events:expr, $sync:expr, $history_loaded:expr, $rescan:expr, $error:expr $(,)?) => {
                        vault_overview_view(
                            $balance,
                            &zero,
                            &none_sequence,
                            None,
                            &[],
                            $events,
                            "synthetic-cube",
                            true,
                            false,
                            false,
                            $history_loaded,
                            $error,
                            $sync,
                            $rescan,
                            $rescan,
                            BitcoinDisplayUnit::Sats,
                            None,
                            None,
                            true,
                            DisplayMode::BitcoinNative,
                        )
                    };
                }
                let normal = capture::inspect(
                    "vault-normal",
                    render!(
                        &balance,
                        &normal_events,
                        &SyncStatus::Synced,
                        true,
                        false,
                        None,
                    ),
                    outer,
                    appearance,
                    theme,
                );
                assert!(normal.contains("Recent activity"));
                assert!(normal.contains("View All Transactions"));
                assert!(normal.contains("Unconfirmed"));
                for control in ["flip-balance", "send-btc", "receive-btc", "history"] {
                    assert!(normal.has_focus(landing_control_id(
                        "synthetic-cube",
                        "Vault",
                        control
                    )));
                }

                let empty = capture::inspect(
                    "vault-known-zero-loaded-empty",
                    render!(&zero, &[], &SyncStatus::Synced, true, false, None),
                    outer,
                    appearance,
                    theme,
                );
                assert!(empty.contains("View All Transactions"));

                let unfinished = capture::inspect(
                    "vault-rescan-date-unknown",
                    render!(
                        &zero,
                        &[],
                        &SyncStatus::WalletFullScan {
                            progress: Some(0.25),
                        },
                        false,
                        true,
                        None,
                    ),
                    outer,
                    appearance,
                    theme,
                );
                assert!(unfinished.contains("Pick a date"));
                assert!(unfinished.contains("Vault history has not loaded"));

                let stale = capture::inspect(
                    "vault-retained-success-error",
                    render!(
                        &balance,
                        &normal_events,
                        &failing,
                        true,
                        false,
                        Some("history refresh failed"),
                    ),
                    outer,
                    appearance,
                    theme,
                );
                assert!(stale.contains("Last known balance"));
                assert!(stale.contains("Last known activity"));
            }
        }
    }
}
