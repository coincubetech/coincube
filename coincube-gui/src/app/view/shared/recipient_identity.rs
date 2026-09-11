//! Compact, neutral identity attribution, adjacent to the reviewed recipient.
use crate::{app::view::Message, services::branta::RecipientIdentity};
use coincube_ui::{
    component::{card as cards, text::*},
    theme::{self, palette::ThemeMode},
    widget::{Column, Element, Row},
};
use iced::{
    widget::{button, image, text::Wrapping, Space},
    Alignment, Length,
};

pub fn card<'a>(
    identity: &'a RecipientIdentity,
    on_open: Message,
    mode: ThemeMode,
) -> Element<'a, Message> {
    if !identity.is_current() {
        return Space::new().into();
    }
    let logo = match mode {
        ThemeMode::Light => identity.logo_light.as_ref().or(identity.logo.as_ref()),
        ThemeMode::Dark => identity.logo.as_ref(),
    };
    let mut heading = Row::new().spacing(10).align_y(Alignment::Center);
    if let Some(logo) = logo {
        heading = heading.push(image(logo.clone()).width(32).height(32));
    }
    heading = heading.push(
        p1_regular(&identity.platform)
            .width(Length::Fill)
            .wrapping(Wrapping::WordOrGlyph),
    );
    let mut content = Column::new()
        .spacing(8)
        .push(p2_regular("Recipient identified"))
        .push(heading);
    if let Some(description) = identity.description.as_deref() {
        content = content.push(
            p2_regular(description)
                .width(Length::Fill)
                .wrapping(Wrapping::WordOrGlyph),
        );
    }
    cards::simple(
        content.push(
            button(p2_regular("View with Branta"))
                .style(theme::button::secondary)
                .on_press(on_open),
        ),
    )
    .width(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_both_themes_and_disabled_without_exposing_url() {
        let ticket = crate::services::branta::test_ticket(true);
        let identity = crate::services::branta::test_identity(&ticket, "Merchant");
        let _ = card(
            &identity,
            Message::OpenVaultRecipientIdentity(0, 0),
            ThemeMode::Dark,
        );
        let _ = card(
            &identity,
            Message::OpenVaultRecipientIdentity(0, 0),
            ThemeMode::Light,
        );
        assert!(!format!("{identity:?}").contains("https://"));
        ticket.set_test_enabled(false);
        let _ = card(
            &identity,
            Message::OpenVaultRecipientIdentity(0, 0),
            ThemeMode::Dark,
        );
        assert!(!identity.is_current());
    }
}
