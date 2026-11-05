//! Lightning Address claim / manage UX.
//!
//! [`settings_view`] renders Spark › Settings › Lightning address: the claim
//! form when no address is bound, else "Your Lightning address" with Copy
//! and Change, where Change opens the rename form in place. That form says
//! what happens to the old address before Save, so it changes the address
//! without a confirmation.
//!
//! Owns `LN_ADDRESS_DOMAIN`, the only string in the codebase that must
//! match the Connect backend's `lightningAddressDomain`.
//!
//! State and message types are unchanged from the per-Cube Connect
//! location this UX was lifted from — the form still reads
//! [`ConnectCubePanel`] and emits [`ConnectCubeMessage`] so the
//! existing dispatch path through `ConnectPanel::update` keeps
//! working.

use coincube_ui::{
    component::{
        tenshu,
        tenshu::{text_field, ActionButton, ActionKind, Tone},
    },
    icon::clipboard_icon,
    widget::{Column, Container, Element, Row},
};
use iced::{widget::Space, Alignment, Length};

use crate::app::view::settings::page;
use crate::app::{state::connect::ConnectCubePanel, view::ConnectCubeMessage};

/// Domain suffix displayed in the Lightning Address claim form.
/// Must match the backend's `lightningAddressDomain`.
pub(crate) const LN_ADDRESS_DOMAIN: &str = "@coincube.io";

pub const COPY_ID: &str = "tenshu.settings.spark.ln-address.copy";
pub const CHANGE_ID: &str = "tenshu.settings.spark.ln-address.change";
pub const USERNAME_ID: &str = "tenshu.settings.spark.ln-address.username";
pub const SAVE_ID: &str = "tenshu.settings.spark.ln-address.save";
pub const CANCEL_ID: &str = "tenshu.settings.spark.ln-address.cancel";
pub const CLAIM_ID: &str = "tenshu.settings.spark.ln-address.claim";
pub const RETRY_ID: &str = "tenshu.settings.spark.ln-address.retry";

/// The username rules, as the server enforces them.
pub const USERNAME_RULES: &str = "3–64 characters: lowercase letters, numbers, dots, \
     underscores and hyphens. Start and end with a letter or number, with no two of . _ - in \
     a row.";

/// What saving a new username does to the old address, shown beside the form
/// before Save.
pub fn release_warning(old_address: &str) -> String {
    format!(
        "{old_address} stops working when you save. You give it up, and anyone can claim it \
         after you, so payments sent to it could reach them. Update it anywhere you have \
         shared it."
    )
}

fn action<'a>(
    id: &'static str,
    label: &'a str,
    kind: ActionKind,
    on_press: Option<ConnectCubeMessage>,
) -> ActionButton<'a, ConnectCubeMessage> {
    ActionButton::new(id, label, kind, ConnectCubeMessage::FocusAction)
        .compact()
        .on_press_maybe(on_press)
}

/// The availability line under the username field: checking, taken or
/// invalid (the field shows those as its error), or available.
fn availability<'a>(
    state: &'a ConnectCubePanel,
    current: Option<&str>,
) -> Element<'a, ConnectCubeMessage> {
    let username = state.ln_username_input.as_str();
    if username.is_empty() || state.ln_username_error.is_some() {
        Space::new().into()
    } else if state.ln_checking {
        page::status(page::Status::Busy, "Checking…")
    } else if !lightning_username_differs(current, username) {
        page::status(page::Status::Off, "This is your current username")
    } else if state.ln_username_available == Some(true) {
        page::status(
            page::Status::Ok,
            format!("{username}{LN_ADDRESS_DOMAIN} is available"),
        )
    } else {
        Space::new().into()
    }
}

/// The username field with the domain attached, its availability and the
/// rules.
fn username_field<'a>(
    state: &'a ConnectCubePanel,
    label: &'static str,
    current: Option<&str>,
    busy: bool,
    on_submit: Option<ConnectCubeMessage>,
) -> Element<'a, ConnectCubeMessage> {
    let mut field = text_field(
        USERNAME_ID,
        label,
        &state.ln_username_input,
        ConnectCubeMessage::FocusAction,
    )
    .suffix(LN_ADDRESS_DOMAIN)
    .placeholder("satoshi")
    .error_maybe(state.ln_username_error.as_deref())
    .on_input_maybe((!busy).then_some(ConnectCubeMessage::LnUsernameChanged));
    if let Some(message) = on_submit {
        field = field.on_submit(message);
    }
    Column::new()
        .spacing(6)
        .width(Length::Fill)
        .push(field)
        .push(availability(state, current))
        .push(page::muted(USERNAME_RULES))
        .into()
}

/// Spark › Settings › Lightning address. No heading repeats the tab name: a
/// bound address is headed "Your Lightning address", an unbound one offers
/// the claim form.
pub fn settings_view<'a>(state: &'a ConnectCubePanel) -> Element<'a, ConnectCubeMessage> {
    let current_address = state
        .lightning_address
        .as_ref()
        .and_then(|la| la.lightning_address.as_deref())
        .map(normalize_lightning_address);
    let Some(address) = current_address else {
        return claim_view(state);
    };
    let current_username = lightning_username(&address);

    let row = Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(
            tenshu::heading(address.clone())
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                .width(Length::Fill),
        )
        .push(
            action(
                COPY_ID,
                "Copy",
                ActionKind::Primary,
                Some(ConnectCubeMessage::CopyToClipboard(address.clone())),
            )
            .icon(clipboard_icon()),
        )
        .push(action(
            CHANGE_ID,
            "Change",
            ActionKind::Secondary,
            (!state.ln_editing && !state.ln_changing)
                .then_some(ConnectCubeMessage::BeginEditLightningAddress),
        ));

    let mut content = Column::new()
        .width(Length::Fill)
        .push(Space::new().height(Length::Fixed(page::GROUP_GAP)))
        .push(page::heading("Your Lightning address"))
        .push(Space::new().height(Length::Fixed(12.0)))
        .push(page::rule())
        .push(
            Container::new(Column::new().spacing(4).push(row).push(page::muted(
                "Anyone can send you bitcoin with this address.",
            )))
            .padding([16, 0]),
        );

    if let Some(err) = state.ln_reconcile_needs_reregister.as_deref() {
        // API↔SDK divergence — the DB confirms the address but the Spark SDK
        // isn't bound to it on this device. Retry does an idempotent SDK
        // delete followed by a fresh register against the DB-confirmed name.
        content = content.push(page::note(
            Tone::Bad,
            Column::new()
                .spacing(4)
                .push(tenshu::label("This address needs re-registration"))
                .push(page::muted(err)),
            Some(
                action(
                    RETRY_ID,
                    if state.ln_reregistering {
                        "Retrying…"
                    } else {
                        "Retry"
                    },
                    ActionKind::Secondary,
                    (!state.ln_reregistering)
                        .then_some(ConnectCubeMessage::RetryLightningAddressReregister),
                )
                .into(),
            ),
        ));
    }

    if state.ln_editing {
        let username = state.ln_username_input.as_str();
        let can_save = can_change_lightning_address(
            username,
            state.ln_username_error.as_deref(),
            state.ln_username_available,
            current_username.as_deref(),
            state.ln_changing,
        );
        let save = can_save.then_some(ConnectCubeMessage::SaveLightningAddress);
        let form = Row::new()
            .spacing(32)
            .align_y(Alignment::Start)
            .push(
                Container::new(username_field(
                    state,
                    "New username",
                    current_username.as_deref(),
                    state.ln_changing,
                    save.clone(),
                ))
                .width(Length::FillPortion(1)),
            )
            .push(
                // Top-aligned with the input, below the field's label.
                Column::new()
                    .width(Length::FillPortion(1))
                    .push(Space::new().height(Length::Fixed(14.0)))
                    .push(page::note(
                        Tone::Warn,
                        page::muted(release_warning(&address)),
                        None,
                    )),
            );
        let buttons = Row::new()
            .spacing(8)
            .push(action(
                SAVE_ID,
                if state.ln_changing {
                    "Saving…"
                } else {
                    "Save new address"
                },
                ActionKind::Primary,
                save,
            ))
            .push(action(
                CANCEL_ID,
                "Cancel",
                ActionKind::Secondary,
                (!state.ln_changing).then_some(ConnectCubeMessage::CancelEditLightningAddress),
            ));
        let mut edit = Column::new()
            .spacing(16)
            .width(Length::Fill)
            .push(form)
            .push(buttons);
        if let Some(err) = state.ln_claim_error.as_deref() {
            edit = edit.push(page::status(page::Status::Bad, err));
        }
        content = content.push(Container::new(edit).padding([16, 0]));
    }
    content.push(page::rule()).into()
}

/// No address yet: the username field and Claim.
fn claim_view<'a>(state: &'a ConnectCubePanel) -> Element<'a, ConnectCubeMessage> {
    let can_claim = can_claim_lightning_address(
        &state.ln_username_input,
        state.ln_username_error.as_deref(),
        state.ln_username_available,
        state.ln_claiming,
    );
    let claim = can_claim.then_some(ConnectCubeMessage::ClaimLightningAddress);
    let mut body = Column::new()
        .spacing(16)
        .width(Length::Fill)
        .push(
            Container::new(username_field(
                state,
                "Username",
                None,
                state.ln_claiming,
                claim.clone(),
            ))
            .max_width(480),
        )
        .push(action(
            CLAIM_ID,
            if state.ln_claiming {
                "Claiming…"
            } else {
                "Claim address"
            },
            ActionKind::Primary,
            claim,
        ));
    if let Some(err) = state.ln_claim_error.as_deref() {
        body = body.push(page::status(page::Status::Bad, err));
        if state.registration_error.is_some() {
            body = body.push(action(
                RETRY_ID,
                "Retry connection",
                ActionKind::Secondary,
                Some(ConnectCubeMessage::RetryRegistration),
            ));
        }
    }
    page::group(
        "Claim a Lightning address",
        Some("A free address anyone can use to send you bitcoin.".into()),
        body,
    )
}

fn normalize_lightning_address(address: &str) -> String {
    if address.contains('@') {
        address.to_string()
    } else {
        format!("{address}{LN_ADDRESS_DOMAIN}")
    }
}

fn lightning_username(address: &str) -> Option<String> {
    address.split('@').next().map(str::to_string)
}

fn lightning_username_differs(current_username: Option<&str>, proposed: &str) -> bool {
    current_username
        .map(|current| current != proposed)
        .unwrap_or(true)
}

fn can_claim_lightning_address(
    username: &str,
    username_error: Option<&str>,
    username_available: Option<bool>,
    claiming: bool,
) -> bool {
    !username.is_empty()
        && username_error.is_none()
        && username_available == Some(true)
        && !claiming
}

fn can_change_lightning_address(
    username: &str,
    username_error: Option<&str>,
    username_available: Option<bool>,
    current_username: Option<&str>,
    changing: bool,
) -> bool {
    can_claim_lightning_address(username, username_error, username_available, changing)
        && lightning_username_differs(current_username, username)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::view::shared::landing::capture;
    use crate::services::coincube::LightningAddress;

    fn panel(address: Option<&str>) -> ConnectCubePanel {
        let mut connect = ConnectCubePanel::new(
            None,
            "synthetic-cube".into(),
            "Test Cube".into(),
            "bitcoin".into(),
            false,
        );
        connect.lightning_address = address.map(|a| LightningAddress {
            lightning_address: Some(a.into()),
        });
        connect
    }

    fn inspect(state: &ConnectCubePanel) -> capture::Inspection {
        capture::inspect(
            "spark-settings-ln-address",
            settings_view(state),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
        )
    }

    /// One heading, the address, what it is for, Copy and Change; nothing
    /// says payments need Tenshu open (they arrive while it is closed).
    #[test]
    fn settings_shows_one_heading_and_the_address() {
        let state = panel(Some("robert@coincube.io"));
        let probe = inspect(&state);
        for label in [
            "Your Lightning address",
            "robert@coincube.io",
            "Anyone can send you bitcoin with this address.",
            "Copy",
            "Change",
        ] {
            assert!(probe.contains(label), "{}", label);
        }
        assert!(!probe.contains("open and active"));
        assert!(!probe.text.iter().any(|(t, _)| t == "Lightning address"));
        let copied = capture::activate(
            settings_view(&state),
            iced::advanced::widget::Id::new(COPY_ID),
            (1440, 900),
            true,
        );
        assert!(copied.iter().any(|m| matches!(
            m,
            ConnectCubeMessage::CopyToClipboard(a) if a == "robert@coincube.io"
        )));
    }

    /// Change opens in place: the field with the domain attached, live
    /// availability, the rules, and the release warning before Save. Save
    /// changes the address directly, with no confirmation modal.
    #[test]
    fn change_expands_in_place_and_saves_without_a_modal() {
        let mut state = panel(Some("robert@coincube.io"));
        state.ln_editing = true;
        state.ln_username_input = "robert-allen".into();
        state.ln_username_available = Some(true);
        let probe = inspect(&state);
        for label in [
            "New username",
            "@coincube.io",
            "robert-allen@coincube.io is available",
            USERNAME_RULES,
            "robert@coincube.io stops working when you save.",
            "Save new address",
            "Cancel",
        ] {
            assert!(probe.contains(label), "{}", label);
        }
        assert!(!probe.contains("Change your Lightning Address"));
        let saved = capture::activate(
            settings_view(&state),
            iced::advanced::widget::Id::new(SAVE_ID),
            (1440, 900),
            true,
        );
        assert!(saved
            .iter()
            .any(|m| matches!(m, ConnectCubeMessage::SaveLightningAddress)));

        assert!(probe.has_focus(iced::advanced::widget::Id::new(SAVE_ID)));

        // The current name can't be saved again: Save is disabled.
        state.ln_username_input = "robert".into();
        let same = inspect(&state);
        assert!(same.contains("This is your current username"));
        assert!(!same.has_focus(iced::advanced::widget::Id::new(SAVE_ID)));
    }

    #[test]
    fn without_an_address_settings_offers_the_claim_form() {
        let probe = inspect(&panel(None));
        assert!(probe.contains("Claim a Lightning address"));
        assert!(probe.contains("Claim address"));
        assert!(probe.contains(USERNAME_RULES));
    }

    #[test]
    fn normalize_lightning_address_adds_domain_only_for_bare_usernames() {
        assert_eq!(
            normalize_lightning_address("founder"),
            "founder@coincube.io"
        );
        assert_eq!(
            normalize_lightning_address("founder@example.com"),
            "founder@example.com"
        );
    }

    #[test]
    fn lightning_username_extracts_the_local_part() {
        assert_eq!(
            lightning_username("founder@coincube.io").as_deref(),
            Some("founder")
        );
        assert_eq!(lightning_username("founder").as_deref(), Some("founder"));
    }

    #[test]
    fn claim_eligibility_requires_valid_available_idle_username() {
        assert!(can_claim_lightning_address(
            "founder",
            None,
            Some(true),
            false
        ));
        assert!(!can_claim_lightning_address("", None, Some(true), false));
        assert!(!can_claim_lightning_address(
            "founder",
            Some("bad username"),
            Some(true),
            false
        ));
        assert!(!can_claim_lightning_address(
            "founder",
            None,
            Some(false),
            false
        ));
        assert!(!can_claim_lightning_address(
            "founder",
            None,
            Some(true),
            true
        ));
    }

    #[test]
    fn change_eligibility_also_requires_a_different_username() {
        assert!(can_change_lightning_address(
            "newname",
            None,
            Some(true),
            Some("oldname"),
            false
        ));
        assert!(!can_change_lightning_address(
            "oldname",
            None,
            Some(true),
            Some("oldname"),
            false
        ));
        assert!(!can_change_lightning_address(
            "newname",
            None,
            Some(true),
            Some("oldname"),
            true
        ));
    }
}
