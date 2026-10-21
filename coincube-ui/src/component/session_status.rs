//! Shared sidebar status for a signed-in or restoring Connect session.
use super::text;
use crate::{
    icon, theme,
    widget::{Container, Element, Row},
};
use iced::{Alignment, Length};

pub fn indicator<'a, M: 'a>(checking: bool) -> Element<'a, M> {
    let (label, style): (_, fn(&theme::Theme) -> iced::widget::text::Style) = if checking {
        ("Checking…", theme::text::warning)
    } else {
        ("Signed in", theme::text::success)
    };
    Container::new(
        Row::new()
            .spacing(10)
            .align_y(Alignment::Center)
            .push(icon::dot_icon().size(8).style(style))
            .push(text::caption(label).style(theme::text::secondary)),
    )
    .padding(iced::Padding {
        top: 10.0,
        right: 20.0,
        bottom: 14.0,
        left: 20.0,
    })
    .width(Length::Fill)
    .into()
}
