use std::fmt::Display;

use crate::{
    color,
    component::{collapse, text},
    icon, theme,
    widget::{Button, Column, Container, Element, Row},
};
use iced::{
    widget::{column, container, row},
    Alignment, Length,
};

pub fn warning<'a, T: 'a + Clone>(message: String, error: String) -> Container<'a, T> {
    // Always show both the title and the error string, stacked. Earlier
    // versions of this component used a `Collapse` widget built on the
    // deprecated `iced::widget::Component` API, which on some themes rendered
    // as a blank orange rectangle with no clickable element. A simple Column
    // is more reliable and self-explanatory.
    Container::new(
        Column::new()
            .spacing(4)
            .push(text::p1_bold(message).color(color::LIGHT_BLACK))
            .push(text::p2_regular(error).color(color::LIGHT_BLACK)),
    )
    .padding(15)
    .style(theme::banner::warning)
    .width(Length::Fill)
}

pub fn processing_hardware_wallet<'a, T: 'a, K: Display, V: Display, F: Display>(
    kind: K,
    version: Option<V>,
    fingerprint: F,
    alias: Option<&'a str>,
) -> Container<'a, T> {
    container(
        row(vec![
            column(vec![
                Row::new()
                    .spacing(5)
                    .push(alias.map(text::p1_bold))
                    .push(text::p1_regular(format!("#{}", fingerprint)))
                    .into(),
                Row::new()
                    .spacing(5)
                    .push(text::caption(kind.to_string()))
                    .push(version.map(|v| text::caption(v.to_string())))
                    .into(),
            ])
            .width(Length::Fill)
            .into(),
            column(vec![
                text::p2_regular("Processing...").into(),
                text::p2_regular("Please check your device").into(),
            ])
            .into(),
        ])
        .align_y(Alignment::Center),
    )
    .style(theme::notification::pending)
    .padding(10)
}

pub fn processing_hardware_wallet_error<'a, T: 'a + Clone>(
    message: String,
    error: String,
) -> Container<'a, T> {
    let message_clone = message.clone();
    Container::new(Container::new(collapse::Collapse::new(
        move || {
            Button::new(
                Row::new()
                    .push(
                        Container::new(
                            text::p1_bold(message_clone.to_string()).color(color::LIGHT_BLACK),
                        )
                        .width(Length::Fill),
                    )
                    .push(
                        Row::new()
                            .align_y(Alignment::Center)
                            .spacing(10)
                            .push(text::p1_bold("Learn more").color(color::LIGHT_BLACK))
                            .push(icon::collapse_icon().color(color::LIGHT_BLACK)),
                    ),
            )
            .style(theme::button::transparent)
        },
        move || {
            Button::new(
                Row::new()
                    .push(
                        Container::new(text::p1_bold(message.to_owned()).color(color::LIGHT_BLACK))
                            .width(Length::Fill),
                    )
                    .push(
                        Row::new()
                            .align_y(Alignment::Center)
                            .spacing(10)
                            .push(text::p1_bold("Learn more").color(color::LIGHT_BLACK))
                            .push(icon::collapsed_icon().color(color::LIGHT_BLACK)),
                    ),
            )
            .style(theme::button::transparent)
        },
        move || Element::<'a, T>::from(text::p2_regular(error.to_owned())),
    )))
    .padding(10)
    .style(theme::notification::error)
    .width(Length::Fill)
}

/// A blocking error card: a plain-language headline, a line saying what to do
/// next, an optional action button, and a short support reference.
///
/// This is the component every user-visible failure should route through. The
/// raw technical detail (a `reqwest::Error`, an SDK string, an HTTP body)
/// deliberately has no parameter here — it belongs in the log file, not on
/// screen. `reference` is the opaque code that ties what the user sees to the
/// logged detail, so support can triage without asking for a log dump.
///
/// Built from a plain `Column` for the same reason [`warning`] is: the
/// `collapse::Collapse` widget rides the deprecated `iced::widget::Component`
/// API and renders as a blank rectangle on some themes.
pub fn error_card<'a, T: 'a + Clone>(
    title: impl Into<String>,
    guidance: impl Into<String>,
    reference: impl Into<String>,
    action: Option<(&'static str, T)>,
) -> Container<'a, T> {
    let mut col = Column::new()
        .spacing(6)
        .push(
            Row::new()
                .spacing(10)
                .align_y(Alignment::Center)
                .push(icon::warning_icon().color(color::RED))
                .push(text::p1_bold(title.into()).color(color::RED)),
        )
        .push(text::p2_regular(guidance.into()).color(color::GREY_3));

    if let Some((label, message)) = action {
        col = col
            .push(iced::widget::Space::new().height(Length::Fixed(6.0)))
            .push(
                Button::new(text::p2_regular(label))
                    .style(theme::button::primary)
                    .on_press(message),
            );
    }

    Container::new(
        col.push(text::caption(format!("Ref: {}", reference.into())).color(color::GREY_3)),
    )
    .padding(15)
    .style(theme::card::error)
    .width(Length::Fill)
}
