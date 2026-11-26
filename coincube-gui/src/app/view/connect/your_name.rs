//! Connect sheet › Account › Your name: one optional field for the name shown
//! beside this account's email to the people in its Cubes.

use coincube_ui::locale::copy as localized_copy;
use coincube_ui::{
    component::tenshu::text as tenshu_text,
    icon::*,
    theme::tenshu::{button::ActionKind, Tone},
    widget::*,
};
use iced::{Alignment, Length};

use super::{group_title, lead, sheet_button, status_line};
use crate::app::{
    state::connect::{account::ConnectAccountPanel, your_name::YourNameNotice},
    view::{ConnectAccountMessage, YourNameMessage},
};

pub const YOUR_NAME_INPUT_ID: &str = "tenshu.connect.account.your-name";

fn msg(m: YourNameMessage) -> ConnectAccountMessage {
    ConnectAccountMessage::YourName(m)
}

/// The field, or `None` while the account's profile has not been read or when
/// the server has no display names: the Account section is then unchanged.
pub(super) fn section<'a>(
    state: &'a ConnectAccountPanel,
) -> Option<Element<'a, ConnectAccountMessage>> {
    let name = &state.your_name;
    name.saved.as_ref()?;
    let can_save = name.can_save();
    let mut input = TextInput::new(localized_copy("Add your name"), name.value())
        .id(YOUR_NAME_INPUT_ID)
        .on_submit_maybe(can_save.then(|| msg(YourNameMessage::Save)))
        .size(15)
        .padding(10)
        .width(Length::Fixed(320.0));
    if !name.saving {
        input = input.on_input(|v| msg(YourNameMessage::Edited(v)));
    }
    let save = sheet_button(
        localized_copy(if name.saving { "Saving…" } else { "Save" }),
        ActionKind::Secondary,
        false,
    )
    .on_press_maybe(can_save.then(|| msg(YourNameMessage::Save)));
    let mut column = Column::new()
        .push(group_title(localized_copy("Your name")))
        .push(iced::widget::Space::new().height(Length::Fixed(4.0)))
        .push(lead(localized_copy(
            "Optional. People in your Cubes see it beside your email.",
        )))
        .push(iced::widget::Space::new().height(Length::Fixed(10.0)))
        .push(
            Row::new()
                .push(input)
                .push(save)
                .spacing(12)
                .align_y(Alignment::Center),
        );
    if let Some(notice) = name.notice {
        let (icon, label, tone) = match notice {
            YourNameNotice::Saved => (check_circle_icon(), "Name saved.", Tone::Good),
            YourNameNotice::Removed => (check_circle_icon(), "Name removed.", Tone::Good),
            YourNameNotice::TooLong => (x_circle_icon(), "Use 60 characters or fewer.", Tone::Bad),
            YourNameNotice::NotAllowed => (
                x_circle_icon(),
                "That name can't be used. Use letters or numbers, and leave out @, < and >.",
                Tone::Bad,
            ),
            YourNameNotice::NotSaved => (
                x_circle_icon(),
                "Your name wasn't saved. Try again.",
                Tone::Bad,
            ),
        };
        column = column
            .push(iced::widget::Space::new().height(Length::Fixed(8.0)))
            .push(status_line(icon, localized_copy(label), tone));
    }
    Some(column.width(Length::Fill).into())
}

/// The two lines that name a person in a list: their display name, when they
/// set one, above their email. The email is always shown, in full, on its own
/// line, so a name never stands in for it where something is decided.
pub(super) fn person_lines<'a, M: 'a>(display_name: Option<&str>, email: &'a str) -> Column<'a, M> {
    match crate::services::coincube::display_name::shown(display_name) {
        Some(name) => Column::new()
            .push(tenshu_text::label(name))
            .push(lead(email))
            .spacing(2),
        None => Column::new().push(tenshu_text::label(email)),
    }
}
