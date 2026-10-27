use iced::{widget::rule, Alignment, Length};

use coincube_ui::component::amount::BitcoinDisplayUnit;
use coincube_ui::{
    component::{amount::*, button, card, text::*},
    icon::{self, receipt_icon},
    theme,
    widget::{Button, Column, Container, Element, Row},
};

use crate::{
    app::{
        menu::{Menu, VaultSubMenu},
        view::placeholder,
    },
    daemon::model::{SpendStatus, SpendTx},
};

use super::super::message::*;

const AMOUNT_WIDTH: f32 = 150.0;
const RECIPIENTS_WIDTH: f32 = 100.0;
const SIGNATURES_WIDTH: f32 = 135.0;
const STATUS_WIDTH: f32 = 130.0;

pub fn import_psbt_success_view<'a>() -> Element<'a, Message> {
    Column::new()
        .push(
            card::simple(Container::new(
                text("PSBT is imported").style(theme::text::success),
            ))
            .padding(50),
        )
        .width(Length::Fixed(400.0))
        .align_x(Alignment::Center)
        .into()
}

pub fn psbts_view(
    spend_txs: &[SpendTx],
    bitcoin_unit: BitcoinDisplayUnit,
    mask_amounts: bool,
) -> Element<'_, Message> {
    Column::new()
        .push(
            Row::new()
                .align_y(Alignment::Center)
                .spacing(10)
                .push(Container::new(h3("PSBT proposals").bold()).width(Length::Fill))
                .push(
                    button::secondary(Some(icon::restore_icon()), "Import PSBT")
                        .on_press(Message::ImportPsbt),
                )
                .push(
                    button::primary(Some(icon::plus_icon()), "Create PSBT")
                        .on_press(Message::Menu(Menu::Vault(VaultSubMenu::Send))),
                ),
        )
        .push(spend_txs.is_empty().then(|| {
            placeholder(
                receipt_icon().size(80),
                "No PSBTs yet",
                "Create a proposal or import a PSBT from another signer. Imported signatures are checked before they can be broadcast.",
            )
        }))
        .push((!spend_txs.is_empty()).then(|| {
            Container::new(
                Column::new().push(table_header()).push(
                    spend_txs
                        .iter()
                        .enumerate()
                        .fold(Column::new(), |col, (i, tx)| {
                            col.push(rule::horizontal(1))
                                .push(spend_tx_list_view(i, tx, bitcoin_unit, mask_amounts))
                        }),
                ),
            )
            .style(theme::card::simple)
            .width(Length::Fill)
        }))
        .align_x(Alignment::Center)
        .spacing(25)
        .into()
}

fn table_header<'a>() -> Container<'a, Message> {
    Container::new(
        Row::new()
            .align_y(Alignment::Center)
            .spacing(12)
            .push(header_cell("Proposal", Length::Fill))
            .push(header_cell("Amount", Length::Fixed(AMOUNT_WIDTH)))
            .push(header_cell("Recipients", Length::Fixed(RECIPIENTS_WIDTH)))
            .push(header_cell("Signatures", Length::Fixed(SIGNATURES_WIDTH)))
            .push(header_cell("Status", Length::Fixed(STATUS_WIDTH))),
    )
    .padding(14)
    .width(Length::Fill)
}

fn header_cell(label: &'static str, width: Length) -> Container<'static, Message> {
    Container::new(p2_regular(label).bold().style(theme::text::secondary)).width(width)
}

fn spend_tx_list_view(
    i: usize,
    tx: &SpendTx,
    bitcoin_unit: BitcoinDisplayUnit,
    mask_amounts: bool,
) -> Element<'_, Message> {
    let txid = tx.psbt.unsigned_tx.compute_txid().to_string();
    let proposal = tx
        .labels
        .get(&txid)
        .filter(|label| !label.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("PSBT {}…", &txid[..8]));
    let amount: Element<'_, Message> = if tx.is_send_to_self() {
        p2_regular("Self-transfer").into()
    } else if mask_amounts {
        p2_regular("••••••").into()
    } else {
        amount_with_size_and_unit(&tx.spend_amount, P2_SIZE, bitcoin_unit).into()
    };
    let recipients = tx
        .psbt
        .unsigned_tx
        .output
        .len()
        .saturating_sub(tx.change_indexes.len());
    let path = tx.path_ready().unwrap_or_else(|| tx.sigs.primary_path());
    let signatures = format!(
        "{} of {}",
        path.sigs_count.min(path.threshold),
        path.threshold
    );
    let (status, status_style): (&str, fn(&theme::Theme) -> iced::widget::container::Style) =
        match tx.status {
            SpendStatus::Pending if tx.path_ready().is_some() => {
                ("Fully signed", theme::pill::success)
            }
            SpendStatus::Pending if !tx.signers().is_empty() => {
                ("Partially signed", theme::pill::warning)
            }
            SpendStatus::Pending => ("Not signed", theme::pill::simple),
            SpendStatus::Broadcast => ("Unconfirmed", theme::pill::warning),
            SpendStatus::Spent => ("Spent", theme::pill::success),
            SpendStatus::Deprecated => ("Deprecated", theme::pill::simple),
        };

    Container::new(
        Button::new(
            Row::new()
                .align_y(Alignment::Center)
                .spacing(12)
                .push(
                    Container::new(
                        Row::new()
                            .align_y(Alignment::Center)
                            .spacing(8)
                            .push(icon::receipt_icon().size(P2_SIZE))
                            .push(p2_regular(proposal).bold()),
                    )
                    .width(Length::Fill),
                )
                .push(Container::new(amount).width(Length::Fixed(AMOUNT_WIDTH)))
                .push(
                    Container::new(p2_regular(recipients.to_string()))
                        .width(Length::Fixed(RECIPIENTS_WIDTH)),
                )
                .push(Container::new(p2_regular(signatures)).width(Length::Fixed(SIGNATURES_WIDTH)))
                .push(
                    Container::new(
                        Container::new(p2_regular(status))
                            .padding([7, 10])
                            .style(status_style),
                    )
                    .width(Length::Fixed(STATUS_WIDTH)),
                ),
        )
        .padding(14)
        .width(Length::Fill)
        .on_press(Message::Select(i))
        .style(theme::button::transparent_border),
    )
    .width(Length::Fill)
    .into()
}
