//! Recovery Kit views shared with Cube creation through a message mapper.
//! Authentication, password policy and removal gates remain in the state machine.

use coincube_ui::{
    component::tenshu::{self, ActionButton, ActionKind, Tone},
    image, theme,
    widget::{CheckBox, Element, TextInput},
};
use iced::widget::{progress_bar, Column, Row, Space};
use iced::{Alignment, Length};
use zeroize::Zeroizing;

use super::{general::backup_methods_present, page};
use crate::app::state::settings::recovery_kit::{PhoneKey, RecoveryKitState};
use crate::app::view::message::{
    Message, RecoveryKitMessage, RecoveryProtectionMode, SettingsMessage,
};
use crate::pin_input::PinInput;
use crate::services::coincube::RecoveryKitStatus;
use crate::services::recovery::{score_password, MIN_PASSWORD_LEN};

fn wrap(msg: RecoveryKitMessage) -> Message {
    Message::Settings(SettingsMessage::RecoveryKit(msg))
}

/// Keep the task aligned with Settings. The ink illustration is supporting art,
/// never an action or the only indicator of a consequential state.
fn flow<'a, M: 'a>(
    title: &'a str,
    detail: &'a str,
    removing: bool,
) -> Column<'a, M, theme::Theme, iced::Renderer> {
    Column::new()
        .width(Length::Fill)
        .max_width(640)
        .spacing(24)
        .push(Space::new().height(8))
        .push(
            Row::new()
                .width(Length::Fill)
                .spacing(24)
                .align_y(Alignment::Center)
                .push(
                    Column::new()
                        .width(Length::Fill)
                        .spacing(6)
                        .push(tenshu::heading(title))
                        .push(tenshu::body(detail).style(theme::tenshu::text::muted)),
                )
                .push(
                    iced::widget::image(if removing {
                        image::recovery_kit_withdrawn()
                    } else {
                        image::recovery_kit_protected()
                    })
                    .width(88)
                    .height(88)
                    .content_fit(iced::ContentFit::Contain),
                ),
        )
}

fn error_note<'a, M: 'a>(message: &'a str) -> Element<'a, M> {
    page::note(
        Tone::Bad,
        tenshu::body(message).style(|t| theme::tenshu::text::tone(t, Tone::Bad)),
        None,
    )
}

fn actions<'a, M: Clone + 'a>(
    label: &'a str,
    enabled: bool,
    message: RecoveryKitMessage,
    on: fn(RecoveryKitMessage) -> M,
) -> Element<'a, M> {
    Column::new()
        .spacing(16)
        .push(page::rule())
        .push(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(page::plain_button(
                    "Cancel",
                    ActionKind::Secondary,
                    Some(on(RecoveryKitMessage::Cancel)),
                ))
                .push(Space::new().width(Length::Fill))
                .push(page::plain_button(
                    label,
                    ActionKind::Primary,
                    enabled.then(|| on(message)),
                )),
        )
        .into()
}

pub fn pin_entry_view<'a>(pin: &'a PinInput, error: Option<&'a str>) -> Element<'a, Message> {
    let mut content = flow(
        "Confirm your Cube PIN",
        "Enter your Cube PIN to unlock your Master Seed Phrase. We'll encrypt it on-device before uploading.",
        false,
    ).push(Column::new().spacing(8).push(tenshu::label("Cube PIN"))
        .push(pin.face_view().map(|m| wrap(RecoveryKitMessage::PinInput(m)))));
    if let Some(error) = error {
        content = content.push(error_note(error));
    }
    content
        .push(actions(
            "Unlock",
            pin.is_complete(),
            RecoveryKitMessage::VerifyPin,
            wrap,
        ))
        .into()
}

/// Shared with Cube creation. Keep the exact length, strength, match and
/// acknowledgement gates, and wrap secret input at the message boundary.
pub fn password_entry_view<'a, M: Clone + 'a>(
    password: &'a Zeroizing<String>,
    confirm: &'a Zeroizing<String>,
    acknowledged: bool,
    error: Option<&'a str>,
    on: fn(RecoveryKitMessage) -> M,
) -> Element<'a, M> {
    let (strength, hint) = score_password(password, &[]);
    let can_submit = password.len() >= MIN_PASSWORD_LEN
        && password.as_str() == confirm.as_str()
        && strength.is_acceptable()
        && acknowledged;
    let mut fields = Column::new()
        .spacing(8)
        .width(Length::Fill)
        .push(tenshu::label("Recovery password"))
        .push(
            TextInput::new("Choose a password", password.as_str())
                .on_input(move |value| {
                    on(RecoveryKitMessage::PasswordChanged(Zeroizing::new(value)))
                })
                .secure(true)
                .size(14)
                .font(coincube_ui::font::REGULAR)
                .padding([10, 12])
                .width(Length::Fill)
                .style(|t, status| theme::tenshu::control::field(t, status, 8.0, false)),
        )
        .push(
            progress_bar(0.0..=1.0, strength.fraction())
                .girth(4)
                .style(theme::progress_bar::primary),
        )
        .push(tenshu::caption(format!("Strength: {}", strength.label())));
    if let Some(hint) = hint {
        fields = fields.push(tenshu::caption(hint));
    }
    fields = fields
        .push(Space::new().height(8))
        .push(tenshu::label("Confirm recovery password"))
        .push(
            TextInput::new("Re-enter password", confirm.as_str())
                .on_input(move |value| {
                    on(RecoveryKitMessage::ConfirmChanged(Zeroizing::new(value)))
                })
                .secure(true)
                .size(14)
                .font(coincube_ui::font::REGULAR)
                .padding([10, 12])
                .width(Length::Fill)
                .style(move |t, status| {
                    theme::tenshu::control::field(
                        t,
                        status,
                        8.0,
                        !confirm.is_empty() && password.as_str() != confirm.as_str(),
                    )
                }),
        );
    if !confirm.is_empty() && password.as_str() != confirm.as_str() {
        fields = fields.push(
            tenshu::caption("Passwords don't match.")
                .style(|t| theme::tenshu::text::tone(t, Tone::Bad)),
        );
    }
    let mut content = flow("Choose a recovery password",
        "This is separate from your Cube PIN — write it down somewhere safe. COINCUBE cannot recover it for you.", false)
        .push(fields)
        .push(CheckBox::new(acknowledged)
            .label("I've written this password down somewhere I can find it")
            .on_toggle(move |value| on(RecoveryKitMessage::AcknowledgeToggled(value)))
            .text_size(14).size(18).spacing(10).width(Length::Fill).style(theme::checkbox::primary));
    if let Some(error) = error {
        content = content.push(error_note(error));
    }
    content
        .push(actions(
            "Back up Recovery Kit",
            can_submit,
            RecoveryKitMessage::SubmitPassword,
            on,
        ))
        .into()
}

pub fn protection_choice_view<'a>(
    phone: &'a PhoneKey,
    error: Option<&'a str>,
    password_backed: bool,
    keychain_backed: bool,
) -> Element<'a, Message> {
    let mut content = flow(
        "Choose how to protect your Recovery Kit",
        "Use a recovery password, your phone (Keychain), or both so either one restores.",
        false,
    );
    if password_backed || keychain_backed {
        let mut status = Row::new().spacing(16).align_y(Alignment::Center);
        if password_backed {
            status = status.push(page::status(page::Status::Ok, "Password backed up"));
        }
        if keychain_backed {
            status = status.push(page::status(page::Status::Ok, "Keychain backed up"));
        }
        content = content.push(status);
    }
    let phone_ready = matches!(phone, PhoneKey::Present);
    content = content.push(page::rows([
        page::row("Recovery password")
            .description("Restore by typing your recovery password.")
            .control(page::primary(
                "tenshu.kit.password",
                "Use password",
                Some(wrap(RecoveryKitMessage::SelectProtectionMode(
                    RecoveryProtectionMode::Password,
                ))),
            ))
            .into(),
        page::row("Phone (Keychain)")
            .description("Restore by approving on your phone, with no password to remember.")
            .control(page::button(
                "tenshu.kit.phone",
                "Use my phone",
                phone_ready.then(|| {
                    wrap(RecoveryKitMessage::SelectProtectionMode(
                        RecoveryProtectionMode::Phone,
                    ))
                }),
            ))
            .into(),
        page::row("Password and phone")
            .description("Back up both ways so either one restores.")
            .control(page::button(
                "tenshu.kit.both",
                "Use both",
                phone_ready.then(|| {
                    wrap(RecoveryKitMessage::SelectProtectionMode(
                        RecoveryProtectionMode::Both,
                    ))
                }),
            ))
            .into(),
    ]));
    content = content.push(match phone {
        PhoneKey::Checking => page::status(page::Status::Busy,"Checking for your phone key…"),
        PhoneKey::Present => page::caption("Phone key found. Choose “Use my phone” or “Use both” — you'll restore by approving on your Keychain."),
        PhoneKey::Absent => page::caption("No phone key yet. Create a recovery key in your Keychain app — signed in to this same Connect account — to enable phone recovery. You'll need this phone (or its recovery phrase) to restore."),
        PhoneKey::Error(error) => error_note(error),
    });
    if matches!(phone, PhoneKey::Absent | PhoneKey::Error(_)) {
        content = content.push(page::link(
            "tenshu.kit.recheck",
            "Check again",
            wrap(RecoveryKitMessage::ProvisionPhone),
        ));
    }
    if let Some(error) = error {
        content = content.push(error_note(error));
    }
    content
        .push(page::button(
            "tenshu.kit.choice.cancel",
            "Cancel",
            Some(wrap(RecoveryKitMessage::Cancel)),
        ))
        .into()
}

pub fn phone_sealing_view() -> Element<'static, Message> {
    flow(
        "Sealing to your phone…",
        "Encrypting your recovery material to your Keychain key and uploading.",
        false,
    )
    .push(page::status(page::Status::Busy, "Backing up…"))
    .into()
}

pub fn passkey_reauth_view() -> Element<'static, Message> {
    flow("Confirm with Touch ID", "Your Recovery Kit carries this Cube's master seed, so we ask for your passkey again before backing it up.", false)
        .push(page::status(page::Status::Busy,"Waiting for Touch ID…"))
        .push(page::rule())
        .push(page::button("tenshu.kit.passkey.cancel", "Cancel", Some(wrap(RecoveryKitMessage::Cancel)))).into()
}

pub fn uploading_view() -> Element<'static, Message> {
    flow("Encrypting and uploading…", "This takes a moment.", false)
        .push(page::status(page::Status::Busy, "Backing up…"))
        .into()
}

/// Name exactly the hosted paths being removed, always password first.
pub(crate) fn confirm_remove_paths(password: bool, keychain: bool) -> Vec<&'static str> {
    let mut paths = Vec::new();
    if password {
        paths.push("Your password-encrypted Recovery Kit");
    }
    if keychain {
        paths.push("The copy sealed to your phone (Keychain)");
    }
    paths
}

pub fn confirm_remove_view<'a>(password: bool, keychain: bool) -> Element<'a, Message> {
    let paths = confirm_remove_paths(password, keychain)
        .into_iter()
        .map(|path| page::row(path).into());
    flow("Remove your Recovery Kit?", "This will delete from your Connect account:", true)
        .push(page::rows(paths))
        .push(tenshu::body("COINCUBE can't undo this. Your Cube and the recovery phrase you wrote down stay exactly as they are — this only removes the copies hosted in Connect. Afterwards, if you lose this device you won't be able to restore this Cube from Connect; you'll need your local recovery phrase."))
        .push(page::rule())
        .push(Row::new().spacing(8).align_y(Alignment::Center)
            .push(page::button("tenshu.kit.remove.cancel", "Cancel", Some(wrap(RecoveryKitMessage::Cancel))))
            .push(Space::new().width(Length::Fill))
            .push(ActionButton::new("tenshu.kit.remove.confirm", "Remove backup", ActionKind::Destructive, Message::FocusAction)
                .compact().on_press(wrap(RecoveryKitMessage::ConfirmRemove))))
        .into()
}

pub fn removing_view() -> Element<'static, Message> {
    flow("Removing Recovery Kit from Connect…", "", true)
        .push(page::status(page::Status::Busy, "Removing…"))
        .into()
}

pub fn completed_view(
    updated_at: &str,
    has_seed: bool,
    has_descriptor: bool,
) -> Element<'static, Message> {
    let subtitle = match (has_seed,has_descriptor) {
        (true,true) => "Both your Master Seed Phrase and Wallet Descriptor are backed up.",
        (true,false) => "Your Master Seed Phrase is backed up. Add your Wallet Descriptor once you have a Vault.",
        (false,true) => "Your Wallet Descriptor is backed up.",
        (false,false) => "Nothing is currently backed up.",
    };
    flow("Recovery Kit backed up", subtitle, false)
        .push(page::status(
            if has_seed || has_descriptor {
                page::Status::Ok
            } else {
                page::Status::Off
            },
            if has_seed || has_descriptor {
                "Backed up"
            } else {
                "Not backed up"
            },
        ))
        .push(tenshu::caption(format!("Last updated: {}", updated_at)))
        .push(page::rule())
        .push(page::primary(
            "tenshu.kit.done",
            "Back to Settings",
            Some(wrap(RecoveryKitMessage::DismissCompleted)),
        ))
        .into()
}

pub fn error_view(message: &str) -> Element<'static, Message> {
    flow("Couldn't complete Recovery Kit action", "", false)
        .push(page::note(
            Tone::Bad,
            tenshu::body(message.to_owned()).style(|t| theme::tenshu::text::tone(t, Tone::Bad)),
            None,
        ))
        .push(page::rule())
        .push(page::primary(
            "tenshu.kit.error.back",
            "Back to Settings",
            Some(wrap(RecoveryKitMessage::Cancel)),
        ))
        .into()
}

/// Returns `Some(wizard)` when the flow is active and should take
/// over the settings page, `None` when the card should render inline.
pub fn dispatch<'a>(
    state: &'a RecoveryKitState,
    pin: &'a PinInput,
    status: Option<&'a RecoveryKitStatus>,
) -> Option<Element<'a, Message>> {
    match state {
        RecoveryKitState::None => None,
        RecoveryKitState::PinEntry { error, .. } => Some(pin_entry_view(pin, error.as_deref())),
        RecoveryKitState::ProtectionChoice { phone, error, .. } => {
            // Surface what's already backed up so the user knows what an Update
            // will re-encrypt / add to (empty in Create mode → no pills).
            let (password_backed, keychain_backed) = backup_methods_present(status);
            Some(protection_choice_view(
                phone,
                error.as_deref(),
                password_backed,
                keychain_backed,
            ))
        }
        RecoveryKitState::PhoneSealing { .. } => Some(phone_sealing_view()),
        RecoveryKitState::PasskeyReauth { .. } => Some(passkey_reauth_view()),
        RecoveryKitState::PasswordEntry {
            password,
            confirm,
            acknowledged,
            error,
            ..
        } => Some(password_entry_view(
            password,
            confirm,
            *acknowledged,
            error.as_deref(),
            wrap,
        )),
        RecoveryKitState::Uploading { .. } => Some(uploading_view()),
        RecoveryKitState::ConfirmRemove => {
            // Name exactly which backup paths the confirm will delete, from the
            // current per-method presence (master F5).
            let (password, keychain) = backup_methods_present(status);
            Some(confirm_remove_view(password, keychain))
        }
        RecoveryKitState::Removing => Some(removing_view()),
        RecoveryKitState::Completed {
            updated_at,
            now_has_seed,
            now_has_descriptor,
        } => Some(completed_view(
            updated_at,
            *now_has_seed,
            *now_has_descriptor,
        )),
        RecoveryKitState::Error { message } => Some(error_view(message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::app::view::shared::landing::capture;

    #[test]
    fn kit_flow_fits_desktop_sizes_and_themes() {
        let pin = PinInput::new();
        let empty = Zeroizing::new(String::new());
        let sample = Zeroizing::new("SYNTHETIC recovery fixture 2026!".to_owned());
        let mismatch = Zeroizing::new("SYNTHETIC mismatch fixture".to_owned());
        for (appearance, palette) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for size in [(1024, 768), (1440, 900)] {
                let cases = vec![
                    ("pin", pin_entry_view(&pin, None)),
                    (
                        "pin-error",
                        pin_entry_view(&pin, Some("Incorrect PIN. Please try again.")),
                    ),
                    (
                        "password",
                        password_entry_view(&empty, &empty, false, None, wrap),
                    ),
                    (
                        "password-mismatch",
                        password_entry_view(
                            &sample,
                            &mismatch,
                            true,
                            Some("Couldn't upload. Try again."),
                            wrap,
                        ),
                    ),
                    (
                        "password-ready",
                        password_entry_view(&sample, &sample, true, None, wrap),
                    ),
                    (
                        "choice-ready",
                        protection_choice_view(&PhoneKey::Present, None, true, true),
                    ),
                    (
                        "choice-absent",
                        protection_choice_view(&PhoneKey::Absent, None, false, false),
                    ),
                    (
                        "choice-checking",
                        protection_choice_view(&PhoneKey::Checking, None, false, false),
                    ),
                    ("phone-sealing", phone_sealing_view()),
                    ("passkey", passkey_reauth_view()),
                    ("uploading", uploading_view()),
                    ("remove-both", confirm_remove_view(true, true)),
                    ("remove-password", confirm_remove_view(true, false)),
                    ("remove-phone", confirm_remove_view(false, true)),
                    ("removing", removing_view()),
                    (
                        "complete",
                        completed_view("5 Nov 2026, 12:00 UTC", true, true),
                    ),
                    (
                        "error",
                        error_view(
                            "The connection was interrupted. Return to Settings and try again.",
                        ),
                    ),
                ];
                for (name, content) in cases {
                    let inspection = capture::inspect(
                        &format!("kit-flow-{}", name),
                        content,
                        size,
                        appearance,
                        palette,
                    );
                    assert!(
                        !inspection.text.is_empty(),
                        "{} must explain its state",
                        name
                    );
                    assert!(
                        inspection
                            .text
                            .iter()
                            .all(|(_, bounds)| bounds.y + bounds.height <= (size.1 - 100) as f32),
                        "{} must fit the minimum desktop height",
                        name
                    );
                }
            }
        }
    }

    #[test]
    fn phone_choices_stay_disabled_until_key_is_present() {
        use iced::advanced::widget::Id;
        for phone in [
            PhoneKey::Checking,
            PhoneKey::Absent,
            PhoneKey::Error("Unable to check phone key.".into()),
            PhoneKey::Present,
        ] {
            let ready = matches!(phone, PhoneKey::Present);
            let inspection = capture::inspect(
                "kit-phone-gates",
                protection_choice_view(&phone, None, true, true),
                (1024, 768),
                "dark",
                theme::Theme::dark(),
            );
            assert!(inspection.has_focus(Id::new("tenshu.kit.password")));
            for id in ["tenshu.kit.phone", "tenshu.kit.both"] {
                assert_eq!(inspection.has_focus(Id::new(id)), ready);
            }
            assert_eq!(
                inspection.has_focus(Id::new("tenshu.kit.recheck")),
                matches!(phone, PhoneKey::Absent | PhoneKey::Error(_))
            );
        }
    }

    #[test]
    fn removal_keeps_both_cancel_and_explicit_destructive_confirmation() {
        for (password, phone) in [(true, false), (false, true), (true, true)] {
            let inspection = capture::inspect(
                "kit-removal-gates",
                confirm_remove_view(password, phone),
                (1024, 768),
                "dark",
                theme::Theme::dark(),
            );
            assert_eq!(
                inspection.contains("Your password-encrypted Recovery Kit"),
                password
            );
            assert_eq!(
                inspection.contains("The copy sealed to your phone (Keychain)"),
                phone
            );
            assert!(inspection.contains("COINCUBE can't undo this."));
            assert!(inspection.contains("only removes the copies hosted in Connect"));
            for (id, expected) in [
                ("tenshu.kit.remove.cancel", RecoveryKitMessage::Cancel),
                (
                    "tenshu.kit.remove.confirm",
                    RecoveryKitMessage::ConfirmRemove,
                ),
            ] {
                for pointer in [false, true] {
                    let messages = capture::activate(
                        confirm_remove_view(password, phone),
                        iced::advanced::widget::Id::new(id),
                        (1024, 768),
                        pointer,
                    );
                    assert!(messages.iter().any(|message| matches!(message, Message::Settings(SettingsMessage::RecoveryKit(action)) if std::mem::discriminant(action) == std::mem::discriminant(&expected))));
                }
            }
        }
    }

    // The confirm screen must name exactly the backup paths that will be
    // deleted, in a deterministic order, for each method combination (plan
    // §PR2). Testing the pure path list is the Element-free equivalent of the
    // three-combination confirm-view snapshot.

    #[test]
    fn confirm_remove_paths_password_only() {
        assert_eq!(
            confirm_remove_paths(true, false),
            vec!["Your password-encrypted Recovery Kit"]
        );
    }

    #[test]
    fn confirm_remove_paths_keychain_only() {
        assert_eq!(
            confirm_remove_paths(false, true),
            vec!["The copy sealed to your phone (Keychain)"]
        );
    }

    #[test]
    fn confirm_remove_paths_both_lists_password_first() {
        assert_eq!(
            confirm_remove_paths(true, true),
            vec![
                "Your password-encrypted Recovery Kit",
                "The copy sealed to your phone (Keychain)",
            ]
        );
    }
}
