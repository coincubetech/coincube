use iced::{
    widget::{rule, text::Wrapping},
    Alignment, Length,
};

use coincube_core::descriptors::PathSpendInfo;
use coincube_ui::component::amount::BitcoinDisplayUnit;
use coincube_ui::{
    component::{amount::*, button, card, text::*},
    icon::{self, receipt_icon},
    theme,
    widget::{Button, Column, ColumnExt, Container, Element, Row},
};

use crate::{
    app::{
        menu::{Menu, VaultSubMenu},
        view::placeholder,
    },
    daemon::model::{SpendStatus, SpendTx},
};

use super::super::message::*;

const PROPOSAL_SHARE: u16 = 4;
const AMOUNT_SHARE: u16 = 3;
const RECIPIENTS_SHARE: u16 = 2;
const SIGNATURES_SHARE: u16 = 2;
const STATUS_SHARE: u16 = 3;

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
            .push(header_cell("Proposal", Length::FillPortion(PROPOSAL_SHARE)))
            .push(header_cell("Amount", Length::FillPortion(AMOUNT_SHARE)))
            .push(header_cell(
                "Recipients",
                Length::FillPortion(RECIPIENTS_SHARE),
            ))
            .push(header_cell(
                "Signatures",
                Length::FillPortion(SIGNATURES_SHARE),
            ))
            .push(header_cell("Status", Length::FillPortion(STATUS_SHARE))),
    )
    .padding(14)
    .width(Length::Fill)
}

fn header_cell(label: &'static str, width: Length) -> Container<'static, Message> {
    Container::new(p2_regular(label).bold().style(theme::text::secondary)).width(width)
}

fn most_signed_path<'a>(
    primary: &'a PathSpendInfo,
    recovery: impl Iterator<Item = &'a PathSpendInfo>,
) -> &'a PathSpendInfo {
    recovery.fold(primary, |best, path| {
        if path.sigs_count > best.sigs_count {
            path
        } else {
            best
        }
    })
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
    let fee: Option<Element<'_, Message>> = tx.fee_amount.as_ref().map(|fee| {
        let fee_amount: Element<'_, Message> = if mask_amounts {
            p2_regular("••••••").into()
        } else {
            amount_with_size_and_unit(fee, P2_SIZE, bitcoin_unit).into()
        };
        Row::new()
            .spacing(4)
            .push(p2_regular("Fee").style(theme::text::secondary))
            .push(fee_amount)
            .into()
    });
    let recipients = tx
        .psbt
        .unsigned_tx
        .output
        .len()
        .saturating_sub(tx.change_indexes.len());
    let path = tx.path_ready().unwrap_or_else(|| {
        most_signed_path(tx.sigs.primary_path(), tx.sigs.recovery_paths().values())
    });
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
                            .push(
                                p2_regular(proposal)
                                    .bold()
                                    .wrapping(Wrapping::WordOrGlyph)
                                    .width(Length::Fill),
                            ),
                    )
                    .width(Length::FillPortion(PROPOSAL_SHARE)),
                )
                .push(
                    Container::new(Column::new().spacing(4).push(amount).push_maybe(fee))
                        .width(Length::FillPortion(AMOUNT_SHARE)),
                )
                .push(
                    Container::new(p2_regular(recipients.to_string()))
                        .width(Length::FillPortion(RECIPIENTS_SHARE)),
                )
                .push(
                    Container::new(p2_regular(signatures))
                        .width(Length::FillPortion(SIGNATURES_SHARE)),
                )
                .push(
                    Container::new(
                        Container::new(p2_regular(status))
                            .padding([7, 10])
                            .style(status_style),
                    )
                    .width(Length::FillPortion(STATUS_SHARE)),
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

#[cfg(test)]
mod tests {
    use super::most_signed_path;
    use coincube_core::descriptors::PathSpendInfo;
    use std::collections::HashMap;

    fn path(sigs_count: usize, threshold: usize) -> PathSpendInfo {
        PathSpendInfo {
            threshold,
            sigs_count,
            signed_pubkeys: HashMap::new(),
        }
    }

    #[test]
    fn partial_recovery_signatures_are_visible_when_primary_has_none() {
        let primary = path(0, 2);
        let recovery = path(1, 2);
        let displayed = most_signed_path(&primary, std::iter::once(&recovery));
        assert_eq!((displayed.sigs_count, displayed.threshold), (1, 2));
    }
}
