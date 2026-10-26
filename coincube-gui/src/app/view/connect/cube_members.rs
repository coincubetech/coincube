//! Cube-scoped members panel view (W8).
//!
//! Rendered when the user navigates to `ConnectSubMenu::CubeMembers` inside a
//! loaded cube. Consumes [`ConnectCubePanel`] state and emits
//! [`ConnectCubeMessage::Members`] messages.

use coincube_ui::{
    color,
    component::{tenshu::action::ActionButton, text, tooltip},
    icon::*,
    theme::{self, tenshu::button::ActionKind},
    widget::*,
};
use iced::{advanced::widget::Id, widget::container, Alignment, Length};

use crate::{
    app::{
        state::connect::ConnectCubePanel,
        view::{ConnectCubeMembersMessage, ConnectCubeMessage},
    },
    services::coincube::{member_joined_after_vault, CubeInviteSummary, CubeMember},
};

use super::{card_style, format_date};

pub(crate) const REMOVE_CONFLICT_DISMISS_ID: &str = "members.remove-conflict.dismiss";
pub(crate) const ERROR_DISMISS_ID: &str = "members.dismiss-error";
fn action_id(name: &str) -> Id {
    Id::from(format!("members.{name}"))
}
pub(crate) fn refresh_action_id() -> Id {
    action_id("refresh")
}
pub(crate) fn add_action_id() -> Id {
    action_id("add")
}
pub(crate) fn revoke_action_id(id: u64) -> Id {
    action_id(&format!("revoke.{id}"))
}
pub(crate) fn remove_action_id(id: u64) -> Id {
    action_id(&format!("remove.{id}"))
}
fn action<'a>(
    id: Id,
    label: &'a str,
    kind: ActionKind,
    message: Option<ConnectCubeMessage>,
) -> Element<'a, ConnectCubeMessage> {
    ActionButton::new(id, label, kind, ConnectCubeMessage::FocusAction)
        .compact()
        .on_press_maybe(message)
        .into()
}

fn row_action<'a>(
    id: Id,
    label: &'a str,
    message: ConnectCubeMessage,
) -> Element<'a, ConnectCubeMessage> {
    ActionButton::new(
        id,
        label,
        ActionKind::Secondary,
        ConnectCubeMessage::FocusAction,
    )
    .compact()
    .width(Length::Fixed(90.0))
    .on_press(message)
    .into()
}

/// Top-level Members view. Composes the invite form, member list, and
/// pending-invite list. Dialogs (error banner, stranded-vault conflict) are
/// rendered at the bottom.
pub fn cube_members_ux<'a>(state: &'a ConnectCubePanel) -> Element<'a, ConnectCubeMessage> {
    let panel = &state.members;
    let ready = state.client.is_some() && state.server_cube_id.is_some();

    let header = Row::new()
        .push(
            Column::new()
                .push(text::h4_bold("Members").style(theme::text::primary))
                .push(
                    text::p2_regular(format!(
                        "{} · {}",
                        state.cube_name,
                        network_name(&state.cube_network)
                    ))
                    .color(color::GREY_3),
                )
                .push(
                    text::p2_regular("People who can view this Cube and contribute signing keys.")
                        .color(color::GREY_3),
                )
                .spacing(2),
        )
        .push(iced::widget::Space::new().width(Length::Fill))
        .push(action(
            refresh_action_id(),
            "Refresh",
            ActionKind::Secondary,
            (ready && !panel.loading).then_some(ConnectCubeMessage::Members(
                ConnectCubeMembersMessage::Reload,
            )),
        ))
        .align_y(Alignment::Center);

    let mut col = Column::new()
        .push(header)
        .push(iced::widget::Space::new().height(Length::Fixed(16.0)))
        .spacing(0)
        .width(Length::Fill);

    // The server Cube id is the boundary for every membership operation. A
    // registering Cube must not present an invite form that cannot submit.
    if !ready {
        let reason = if state.owned_by_other_account {
            "This Cube is registered to a different Connect account."
        } else if state.registration_error.is_some() {
            "Connect couldn't register this Cube. Retry, or check your Connect account if it keeps failing."
        } else if state.client.is_none() {
            "Sign in to Connect to manage this Cube's members."
        } else {
            "Not ready — the Cube is still registering."
        };
        let mut not_ready = Column::new().push(text::p1_regular(reason)).spacing(12);
        if state.registration_error.is_some()
            && !state.owned_by_other_account
            && !state.is_registering()
        {
            not_ready = not_ready.push(action(
                action_id("retry-registration"),
                "Retry",
                ActionKind::Secondary,
                Some(ConnectCubeMessage::RetryRegistration),
            ));
        }
        return col
            .push(
                container(not_ready)
                    .padding(20)
                    .style(card_style)
                    .width(Length::Fill),
            )
            .into();
    }

    col = col
        .push(text::p1_bold("Add Member").style(theme::text::primary))
        .push(iced::widget::Space::new().height(Length::Fixed(8.0)))
        .push(invite_form(panel))
        .push(iced::widget::Space::new().height(Length::Fixed(20.0)));

    // Pending invites section
    if !panel.pending_invites.is_empty() {
        col = col.push(text::p1_bold("Pending Invites").style(theme::text::primary));
        col = col.push(iced::widget::Space::new().height(Length::Fixed(8.0)));
        col = col.push(iced::widget::rule::horizontal(1));
        for invite in &panel.pending_invites {
            col = col.push(pending_invite_card(invite));
        }
        col = col.push(iced::widget::Space::new().height(Length::Fixed(24.0)));
    }

    // Members list. Skip the "No members yet" placeholder when there's
    // a *load* error — we don't actually know the cube is empty, the
    // fetch just failed. An action error (e.g. invite validation) is
    // unrelated to list shape, so it doesn't gate the empty state.
    if panel.members.is_empty()
        && panel.pending_invites.is_empty()
        && !panel.loading
        && panel.load_error.is_none()
    {
        col = col.push(empty_state());
    } else if !panel.members.is_empty() {
        col = col.push(text::p1_bold("Members").style(theme::text::primary));
        col = col.push(iced::widget::Space::new().height(Length::Fixed(8.0)));
        col = col.push(iced::widget::rule::horizontal(1));
        let vault_created_at = panel.vault_created_at.as_deref();
        for member in &panel.members {
            col = col.push(member_card(member, vault_created_at));
        }
    } else if panel.loading {
        col = col.push(text::p1_regular("Loading\u{2026}").color(color::GREY_3));
    }

    // Load errors take precedence — they describe a structural issue
    // (can't render data) and are more severe than a transient action
    // failure. An action error below is surfaced only when there's no
    // load error to supersede it.
    if let Some(err) = panel.load_error.as_deref() {
        col = col.push(iced::widget::Space::new().height(Length::Fixed(12.0)));
        col = col.push(error_banner(err));
    } else if let Some(err) = panel.error.as_deref() {
        col = col.push(iced::widget::Space::new().height(Length::Fixed(12.0)));
        col = col.push(error_banner(err));
    }

    if let Some(conflict) = panel.remove_conflict.as_deref() {
        col = col.push(iced::widget::Space::new().height(Length::Fixed(12.0)));
        col = col.push(remove_conflict_banner(conflict));
    }

    col.into()
}

// =============================================================================
// Invite Form
// =============================================================================

fn invite_form<'a>(
    panel: &'a crate::app::state::connect::ConnectCubeMembersState,
) -> Element<'a, ConnectCubeMessage> {
    let can_submit = !panel.invite_sending;

    let submit: Element<ConnectCubeMessage> = if panel.invite_sending {
        iced::widget::button(
            container(text::p1_regular("Sending\u{2026}").color(color::GREY_3))
                .center_x(Length::Fill)
                .center_y(Length::Fill),
        )
        .width(Length::Shrink)
        .height(Length::Fixed(44.0))
        .style(theme::button::primary)
        .into()
    } else {
        action(
            add_action_id(),
            "Add Member",
            ActionKind::Primary,
            can_submit.then_some(ConnectCubeMessage::Members(
                ConnectCubeMembersMessage::SubmitInvite,
            )),
        )
    };

    let input = TextInput::new("email@example.com", &panel.invite_email)
        .on_input(|s| ConnectCubeMessage::Members(ConnectCubeMembersMessage::InviteEmailChanged(s)))
        .on_submit_maybe(can_submit.then_some(ConnectCubeMessage::Members(
            ConnectCubeMembersMessage::SubmitInvite,
        )))
        .size(16)
        .padding(15)
        .width(Length::Fill);

    let row = Row::new()
        .push(input)
        .push(iced::widget::Space::new().width(Length::Fixed(8.0)))
        .push(submit)
        .align_y(Alignment::Center);

    Column::new()
        .push(
            text::p2_regular(
                "Invite by email. Existing contacts are added immediately; new emails \
                 get an invite and join on accept.",
            )
            .color(color::GREY_3),
        )
        .push(iced::widget::Space::new().height(Length::Fixed(10.0)))
        .push(row)
        .push_maybe(
            panel
                .invite_email_invalid
                .then(|| text::p2_regular("Please enter a valid email address").color(color::RED)),
        )
        .spacing(0)
        .width(Length::Fill)
        .into()
}

// =============================================================================
// Member & Invite cards
// =============================================================================

fn member_card<'a>(
    member: &'a CubeMember,
    vault_created_at: Option<&str>,
) -> Element<'a, ConnectCubeMessage> {
    let email = &member.user.email;
    let initial = email
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();

    let avatar = container(text::p1_bold(initial).style(theme::text::primary))
        .width(Length::Fixed(36.0))
        .height(Length::Fixed(36.0))
        .center_x(Length::Fixed(36.0))
        .center_y(Length::Fixed(36.0))
        .style(|t| container::Style {
            background: Some(iced::Background::Color(t.colors.cards.simple.background)),
            border: iced::Border {
                radius: 18.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });

    // W16-desktop: show a subdued "Joined after Vault" badge on members
    // who landed after the attached Vault's signing quorum was sealed.
    // Their keys can attach to a future Vault rebuild but won't sign
    // the current one. Tooltip spells out the implication on hover.
    let joined_after_vault =
        vault_created_at.is_some_and(|v| member_joined_after_vault(&member.joined_at, v));
    let badge_row: Option<Element<'_, ConnectCubeMessage>> = if joined_after_vault {
        Some(
            Row::new()
                .push(text::p2_regular("Joined after Vault").color(color::GREY_3))
                .push(iced::widget::Space::new().width(Length::Fixed(4.0)))
                .push(tooltip(
                    "This member can attach keys but won't be in the current Vault's \
                     signing quorum. Their keys will be available when the Vault is rebuilt.",
                ))
                .align_y(Alignment::Center)
                .into(),
        )
    } else {
        None
    };

    let joined = format_date(&member.joined_at);
    let mut info = Column::new().push(text::p1_regular(email.as_str()).style(theme::text::primary));
    if let Some(badge) = badge_row {
        info = info.push(badge);
    }
    let info = info
        .push(text::p2_regular(format!("Joined {}", joined)).color(color::GREY_3))
        .spacing(2)
        .width(Length::Fill);

    let member_id = member.id;
    let remove_btn = row_action(
        remove_action_id(member_id),
        "Remove",
        ConnectCubeMessage::Members(ConnectCubeMembersMessage::RemoveMember(member_id)),
    );

    Column::new()
        .push(
            Row::new()
                .push(avatar)
                .push(iced::widget::Space::new().width(Length::Fixed(12.0)))
                .push(info)
                .push(iced::widget::Space::new().width(Length::Fixed(12.0)))
                .push(remove_btn)
                .align_y(Alignment::Center)
                .padding([12, 0]),
        )
        .push(iced::widget::rule::horizontal(1))
        .width(Length::Fill)
        .into()
}

fn pending_invite_card<'a>(invite: &'a CubeInviteSummary) -> Element<'a, ConnectCubeMessage> {
    let expiry_elem = expiry_element(&invite.expires_at);

    let info = Column::new()
        .push(text::p1_regular(invite.email.as_str()).style(theme::text::primary))
        .push(
            Row::new()
                .push(text::p2_regular(invite.status.as_str()).color(status_color(&invite.status)))
                .push(iced::widget::Space::new().width(Length::Fixed(12.0)))
                .push(expiry_elem)
                .align_y(Alignment::Center),
        )
        .spacing(2)
        .width(Length::Fill);

    let invite_id = invite.id;
    let mut row = Row::new().push(info);
    if invite.status == "pending" {
        row = row.push(row_action(
            revoke_action_id(invite_id),
            "Revoke",
            ConnectCubeMessage::Members(ConnectCubeMembersMessage::RevokeInvite(invite_id)),
        ));
    }

    Column::new()
        .push(row.align_y(Alignment::Center).padding([12, 0]))
        .push(iced::widget::rule::horizontal(1))
        .width(Length::Fill)
        .into()
}

// =============================================================================
// Empty state, error banners
// =============================================================================

fn empty_state<'a>() -> Element<'a, ConnectCubeMessage> {
    container(
        Column::new()
            .push(person_icon().size(40).color(color::GREY_3))
            .push(iced::widget::Space::new().height(Length::Fixed(12.0)))
            .push(text::p1_bold("No members yet").style(theme::text::primary))
            .push(iced::widget::Space::new().height(Length::Fixed(4.0)))
            .push(
                text::p2_regular(
                    "Add a contact by email to let them contribute a signing key to this Cube.",
                )
                .color(color::GREY_3),
            )
            .align_x(Alignment::Center)
            .padding(24)
            .spacing(0),
    )
    .style(card_style)
    .width(Length::Fill)
    .into()
}

fn error_banner<'a>(err: &str) -> Element<'a, ConnectCubeMessage> {
    container(
        Row::new()
            .push(text::p2_regular(err.to_string()).color(color::RED))
            .push(iced::widget::Space::new().width(Length::Fill))
            .push(action(
                Id::new(ERROR_DISMISS_ID),
                "Dismiss",
                ActionKind::Quiet,
                Some(ConnectCubeMessage::Members(
                    ConnectCubeMembersMessage::DismissError,
                )),
            ))
            .align_y(Alignment::Center)
            .padding(12),
    )
    .style(|t| container::Style {
        background: Some(iced::Background::Color(t.colors.cards.simple.background)),
        border: iced::Border {
            color: color::RED,
            width: 0.5,
            radius: 8.0.into(),
        },
        ..Default::default()
    })
    .width(Length::Fill)
    .into()
}

fn remove_conflict_banner<'a>(msg: &str) -> Element<'a, ConnectCubeMessage> {
    container(
        Column::new()
            .push(text::p1_bold("Can't remove this member").color(color::RED))
            .push(iced::widget::Space::new().height(Length::Fixed(6.0)))
            .push(text::p2_regular(msg.to_string()).style(theme::text::primary))
            .push(iced::widget::Space::new().height(Length::Fixed(6.0)))
            .push(
                text::p2_regular("Wind down the Vault(s) first, then try again.")
                    .color(color::GREY_3),
            )
            .push(iced::widget::Space::new().height(Length::Fixed(10.0)))
            .push(
                Row::new()
                    .push(iced::widget::Space::new().width(Length::Fill))
                    .push(action(
                        Id::new(REMOVE_CONFLICT_DISMISS_ID),
                        "Dismiss",
                        ActionKind::Secondary,
                        Some(ConnectCubeMessage::Members(
                            ConnectCubeMembersMessage::DismissRemoveConflict,
                        )),
                    )),
            )
            .padding(16)
            .spacing(0),
    )
    .style(|t| container::Style {
        background: Some(iced::Background::Color(t.colors.cards.simple.background)),
        border: iced::Border {
            color: color::RED,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    })
    .width(Length::Fill)
    .into()
}

// =============================================================================
// Helpers
// =============================================================================

fn status_color(status: &str) -> iced::Color {
    match status {
        "pending" => color::ORANGE,
        "accepted" => color::GREEN,
        "revoked" | "expired" => color::GREY_3,
        _ => color::GREY_3,
    }
}

fn network_name(network: &str) -> &str {
    match network {
        "mainnet" => "Bitcoin mainnet",
        "testnet" => "Bitcoin testnet",
        "testnet4" => "Bitcoin testnet4",
        "signet" => "Bitcoin signet",
        "regtest" => "Bitcoin regtest",
        "bitcoin-blake2b" => "Bitcoin Blake2b mainnet",
        "bitcoin-blake2b-testnet4" => "Bitcoin Blake2b testnet4",
        other => other,
    }
}

fn expiry_element<'a>(expires_at: &str) -> Element<'a, ConnectCubeMessage> {
    let Ok(expiry) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
        return text::p2_regular("").into();
    };
    let now = chrono::Utc::now();
    // Check the full timestamp first. If we only compared
    // `expiry.date_naive() - now.date_naive()` we'd bucket an invite
    // that expired at 08:00 today as "Expires today" at 18:00 even
    // though it's already past.
    let expiry_utc = expiry.with_timezone(&chrono::Utc);
    if expiry_utc <= now {
        return text::p2_regular("Expired").color(color::RED).into();
    }
    let days = (expiry_utc.date_naive() - now.date_naive()).num_days();
    match days {
        0 => text::p2_regular("Expires today")
            .color(color::ORANGE)
            .into(),
        1 => text::p2_regular("Expires in 1 day")
            .color(color::GREY_3)
            .into(),
        d => text::p2_regular(format!("Expires in {} days", d))
            .color(color::GREY_3)
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::view::shared::landing::capture;
    use crate::services::coincube::CubeMemberUser;

    fn synthetic_rows<'a>(
        panel: &'a crate::app::state::connect::ConnectCubeMembersState,
        invite: &'a CubeInviteSummary,
        member: &'a CubeMember,
    ) -> Element<'a, ConnectCubeMessage> {
        Column::new()
            .push(action(
                refresh_action_id(),
                "Refresh",
                ActionKind::Secondary,
                Some(ConnectCubeMessage::Members(
                    ConnectCubeMembersMessage::Reload,
                )),
            ))
            .push(invite_form(panel))
            .push(pending_invite_card(invite))
            .push(member_card(member, None))
            .push(error_banner("Network unavailable"))
            .push(remove_conflict_banner("Keys are signing an active Vault"))
            .spacing(12)
            .width(Length::Fill)
            .into()
    }

    #[test]
    fn synthetic_members_actions_fit_and_mount_in_both_themes() {
        let mut panel = crate::app::state::connect::ConnectCubeMembersState::new();
        panel.invite_email = "jordan@example".into();
        panel.invite_email_invalid = true;
        let invite = CubeInviteSummary {
            id: 13,
            cube_id: 1,
            email: "jordan@example.test".into(),
            status: "pending".into(),
            expires_at: (chrono::Utc::now() + chrono::Duration::days(6)).to_rfc3339(),
            created_at: "2026-10-26T00:00:00Z".into(),
        };
        let member = CubeMember {
            id: 17,
            user_id: 4,
            user: CubeMemberUser {
                email: "morgan.with-a-long-household-address@family-office.example.test".into(),
            },
            joined_at: "2026-10-18T00:00:00Z".into(),
        };
        for size in [(1024, 768), (1440, 900)] {
            for (appearance, theme) in [
                ("dark", theme::Theme::dark()),
                ("light", theme::Theme::light()),
            ] {
                let mounted = capture::inspect(
                    "members-first-release",
                    synthetic_rows(&panel, &invite, &member),
                    size,
                    appearance,
                    theme,
                );
                assert!(mounted.contains("Please enter a valid email address"));
                for id in [
                    refresh_action_id(),
                    add_action_id(),
                    revoke_action_id(invite.id),
                    remove_action_id(member.id),
                    Id::new(ERROR_DISMISS_ID),
                    Id::new(REMOVE_CONFLICT_DISMISS_ID),
                ] {
                    assert!(mounted.has_focus(id));
                }
            }
        }
        let keyboard = capture::activate(
            synthetic_rows(&panel, &invite, &member),
            remove_action_id(member.id),
            (1024, 768),
            false,
        );
        assert!(keyboard.iter().any(|msg| matches!(
            msg,
            ConnectCubeMessage::Members(ConnectCubeMembersMessage::RemoveMember(17))
        )));
    }

    #[test]
    fn failed_registration_exposes_keyboard_retry_without_raw_error() {
        let mut state = ConnectCubePanel::new(
            None,
            "synthetic-cube".into(),
            "Household".into(),
            "mainnet".into(),
            true,
        );
        state.registration_error = Some("internal server detail".into());
        let mounted = capture::inspect(
            "members-registration-failed",
            cube_members_ux(&state),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(mounted.contains("Connect couldn't register this Cube"));
        assert!(!mounted.contains("internal server detail"));
        assert!(mounted.has_focus(action_id("retry-registration")));
        let keyboard = capture::activate(
            cube_members_ux(&state),
            action_id("retry-registration"),
            (1024, 768),
            false,
        );
        assert!(keyboard
            .iter()
            .any(|msg| matches!(msg, ConnectCubeMessage::RetryRegistration)));
    }
}
