use coincube_ui::locale::copy as localized_copy;
use coincube_ui::localized_format;
use std::collections::{HashMap, HashSet};

use iced::{
    widget::{checkbox, tooltip, Space},
    Alignment, Length,
};

use coincube_core::miniscript::bitcoin::{
    bip32::{DerivationPath, Fingerprint},
    Amount,
};

use coincube_ui::{
    component::{amount::*, button, text::*},
    theme,
    widget::{Column, Container, Element, Row},
};

use crate::app::{
    cache::Cache,
    menu::Menu,
    view::{
        dashboard,
        message::{CreateSpendMessage, Message},
    },
};

#[allow(clippy::too_many_arguments)]
pub fn recovery<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    recovery_paths: Vec<Element<'a, Message>>,
    selected_path: Option<usize>,
    next_recovery_sequence: Option<u32>,
) -> Element<'a, Message> {
    let no_recovery_paths = recovery_paths.is_empty();
    const INFO_TEXT: &str = "Use a recovery path to send funds to another wallet after its time lock has elapsed. A backup does not unlock a time lock.";
    dashboard(
        menu,
        cache,
        Column::new()
            .push(Container::new(h3(localized_copy("Recovery path")).bold()).width(Length::Fill))
            .push(Container::new(text(INFO_TEXT)))
            .push(
                cache
                    .unswept_recovery_notice()
                    .map(|notice| text(localized_copy(notice))),
            )
            .push(Space::new().height(Length::Fixed(20.0)))
            .push(
                Container::new(
                    Column::new()
                        .push(
                            text(if no_recovery_paths {
                                localized_copy("No recovery path is currently available.")
                                    .to_string()
                            } else if recovery_paths.len() == 1 {
                                localized_format!(
                                    "{} recovery path is available:",
                                    "Hay {} ruta de recuperación disponible:",
                                    1
                                )
                            } else {
                                localized_format!(
                                    "{} recovery paths are available:",
                                    "Hay {} rutas de recuperación disponibles:",
                                    recovery_paths.len()
                                )
                            })
                            .width(Length::Fill),
                        )
                        .push(next_recovery_sequence.map(|sequence| {
                            let days = super::coins::blocks_to_days_ceil(sequence);
                            text(match (no_recovery_paths, days == 1) {
                                (true, true) => localized_format!(
                                    "Recovery path available in ≈ {} day.",
                                    "Ruta de recuperación disponible en ≈ {} día.",
                                    days
                                ),
                                (true, false) => localized_format!(
                                    "Recovery path available in ≈ {} days.",
                                    "Ruta de recuperación disponible en ≈ {} días.",
                                    days
                                ),
                                (false, true) => localized_format!(
                                    "Next recovery path available in ≈ {} day.",
                                    "Próxima ruta de recuperación disponible en ≈ {} día.",
                                    days
                                ),
                                (false, false) => localized_format!(
                                    "Next recovery path available in ≈ {} days.",
                                    "Próxima ruta de recuperación disponible en ≈ {} días.",
                                    days
                                ),
                            })
                        }))
                        .push((!no_recovery_paths).then_some(Space::new().height(20)))
                        .push(Column::with_children(recovery_paths).spacing(20))
                        .spacing(10),
                )
                .style(theme::card::simple)
                .padding(20),
            )
            .push(if no_recovery_paths {
                None
            } else {
                Some(
                    Row::new()
                        .push(Space::new().width(Length::Fill))
                        .push(
                            button::primary(None, localized_copy("Next"))
                                .on_press_maybe(selected_path.map(|_| Message::Next))
                                .width(Length::Fixed(200.0)),
                        )
                        .spacing(20)
                        .align_y(Alignment::Center),
                )
            })
            .spacing(20),
    )
}

pub fn recovery_path_view<'a>(
    index: usize,
    threshold: usize,
    origins: &'a [(Fingerprint, HashSet<DerivationPath>)],
    total_amount: Amount,
    number_of_coins: usize,
    key_aliases: &'a HashMap<Fingerprint, String>,
    selected: bool,
) -> Element<'a, Message> {
    Row::new()
        .push(
            checkbox(selected)
                .on_toggle(move |_| Message::CreateSpend(CreateSpendMessage::SelectPath(index))),
        )
        .push(
            Column::new()
                .push(
                    Row::new()
                        .align_y(Alignment::Center)
                        .spacing(10)
                        .push(
                            text(localized_format!(
                                "{} signature{} from",
                                "{} firma{} de",
                                threshold,
                                if threshold > 1 { "s" } else { "" }
                            ))
                            .bold(),
                        )
                        .push(origins.iter().fold(
                            Row::new().align_y(Alignment::Center).spacing(5),
                            |row, (fg, _)| {
                                row.push(if let Some(alias) = key_aliases.get(fg) {
                                    Container::new(
                                        tooltip::Tooltip::new(
                                            Container::new(text(alias))
                                                .padding(5)
                                                .style(theme::pill::simple),
                                            coincube_ui::widget::Text::new(fg.to_string()),
                                            tooltip::Position::Top,
                                        )
                                        .style(theme::card::simple),
                                    )
                                } else {
                                    Container::new(text(fg.to_string()))
                                        .padding(5)
                                        .style(theme::pill::simple)
                                })
                            },
                        )),
                )
                .push(
                    Row::new()
                        .spacing(5)
                        .push(text(localized_copy("can recover")))
                        .push(text(localized_format!(
                            "{} coin{} totalling",
                            "{} moneda{} con un total de",
                            number_of_coins,
                            if number_of_coins > 1 { "s" } else { "" }
                        )))
                        .push(amount(&total_amount)),
                )
                .spacing(5),
        )
        .width(Length::Fill)
        .align_y(Alignment::Center)
        .spacing(20)
        .into()
}
