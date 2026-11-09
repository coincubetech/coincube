//! Inline "Sign in to Connect" prompt shared by the Connect-requiring
//! feature pages — Spark → Settings → Lightning Address, Cube →
//! Settings → Avatar, Cube → Settings → Members.
//!
//! Renders a small card with one-line guidance and a "Sign In" button.
//! Clicking the button fires [`crate::app::view::Message::OpenConnectSignIn`],
//! which bubbles up to the pane so the Home tab can take focus and
//! land the user on the Connect login form.

use coincube_ui::locale::copy as localized_copy;
use coincube_ui::localized_format;
use coincube_ui::{
    color,
    component::{button, text},
    theme,
    widget::{Column, Container, Element, Row},
};
use iced::{widget::Space, Alignment, Length};

use crate::app::menu::{Menu, SparkSettingsOption, SparkSubMenu};
use crate::app::view::Message;

/// Build the prompt using the action the user wants to take, such as
/// "claim a Lightning Address".
pub fn sign_in_prompt<'a>(feature_label: &'a str) -> Element<'a, Message> {
    let card = Container::new(
        Column::new()
            .spacing(12)
            .push(text::p1_bold(localized_copy("Sign in to Connect")).style(theme::text::primary))
            .push(
                text::p2_regular(localized_format!(
                    "Sign in to your Connect account to {}.",
                    "Inicia sesión en tu cuenta de Connect para {}.",
                    localized_copy(feature_label)
                ))
                .color(color::GREY_3),
            )
            .push(Space::new().height(Length::Fixed(4.0)))
            .push(
                Row::new()
                    .align_y(Alignment::Center)
                    .push(Space::new().width(Length::Fill))
                    .push(
                        button::primary(None, localized_copy("Sign In"))
                            .on_press(Message::OpenConnectSignIn)
                            .width(Length::Fixed(140.0)),
                    ),
            ),
    )
    .padding(20)
    .style(theme::card::simple)
    .width(Length::Fill);

    Column::new().spacing(0).push(card).into()
}

/// Avatar's empty-state precondition: prompt the user to claim a
/// Lightning Address first (the avatar feature is LNURL profile-image
/// based and requires an LN address). The button navigates straight
/// to Spark → Settings → Lightning Address.
pub fn claim_ln_address_prompt<'a>() -> Element<'a, Message> {
    let card = Container::new(
        Column::new()
            .spacing(12)
            .push(text::p1_bold("Claim a Lightning Address first").style(theme::text::primary))
            .push(
                text::p2_regular(localized_copy(
                    "Your Avatar is bound to your Lightning Address. Claim one to set \
                     up an Avatar.",
                ))
                .color(color::GREY_3),
            )
            .push(Space::new().height(Length::Fixed(4.0)))
            .push(
                Row::new()
                    .align_y(Alignment::Center)
                    .push(Space::new().width(Length::Fill))
                    .push(
                        button::primary(None, localized_copy("Claim Lightning Address"))
                            .on_press(Message::Menu(Menu::Spark(SparkSubMenu::Settings(Some(
                                SparkSettingsOption::LightningAddress,
                            )))))
                            .width(Length::Fixed(220.0)),
                    ),
            ),
    )
    .padding(20)
    .style(theme::card::simple)
    .width(Length::Fill);

    Column::new().spacing(0).push(card).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::view::shared::landing::capture;
    use coincube_ui::locale::{render_in, Language};

    #[test]
    fn spanish_connect_sign_in_explains_the_purpose() {
        let view = render_in(Language::Spanish, || {
            sign_in_prompt("claim a Lightning Address")
        });
        let shown = capture::inspect(
            "spanish-connect-purpose",
            view,
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(shown.contains(
            "Inicia sesión en tu cuenta de Connect para obtener una dirección Lightning."
        ));
    }
}
