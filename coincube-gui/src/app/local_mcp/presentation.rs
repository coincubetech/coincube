//! AI access presentation. Authorization and pairing state stay in `local_mcp`.
//! The available controls map directly to its existing messages; the product
//! concepts still in design are rendered as inert, explicitly planned content.

use super::{pairings, Message, Settings};
use crate::app::view;
use coincube_ui::font;
use coincube_ui::theme::{self, tenshu::Tone};
use coincube_ui::widget::{action::Action, modal::Modal, Element};
use iced::widget::{column, container, row, text, text_input, Column, Row, Space};
use iced::{Alignment, Background, Border, Length};
use std::sync::atomic::Ordering;

type Ui<'a> = Element<'a, view::Message>;

fn message(value: Message) -> view::Message {
    view::Message::LocalMcp(value)
}

fn muted<'a>(
    value: impl iced::widget::text::IntoFragment<'a>,
) -> iced::widget::Text<'a, theme::Theme> {
    text(value).size(13).style(theme::text::secondary)
}

fn heading<'a>(
    value: impl iced::widget::text::IntoFragment<'a>,
) -> iced::widget::Text<'a, theme::Theme> {
    text(value).size(16).font(font::SEMIBOLD)
}

fn card<'a>(content: impl Into<Ui<'a>>) -> Ui<'a> {
    container(content)
        .padding([16, 18])
        .width(Length::Fill)
        .style(|theme: &theme::Theme| iced::widget::container::Style {
            text_color: Some(theme.colors.shell.ink),
            background: Some(Background::Color(theme.colors.shell.panel)),
            border: Border {
                color: theme.colors.shell.line,
                width: 1.0,
                radius: 10.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn notice<'a>(content: impl Into<Ui<'a>>, tone: Tone) -> Ui<'a> {
    container(content)
        .padding([11, 13])
        .width(Length::Fill)
        .style(move |theme: &theme::Theme| iced::widget::container::Style {
            text_color: Some(theme.colors.shell.ink),
            background: Some(Background::Color(theme.colors.shell.panel)),
            border: Border {
                color: tone.color(theme),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn chip<'a>(label: impl iced::widget::text::IntoFragment<'a>, tone: Tone) -> Ui<'a> {
    container(
        text(label)
            .size(12)
            .style(move |theme: &theme::Theme| iced::widget::text::Style {
                color: Some(tone.color(theme)),
            }),
    )
    .padding([4, 9])
    .style(|theme: &theme::Theme| iced::widget::container::Style {
        border: Border {
            color: theme.colors.shell.line,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn action<'a>(
    id: impl Into<iced::advanced::widget::Id>,
    label: &'static str,
    intent: Option<view::Message>,
    kind: theme::tenshu::button::ActionKind,
) -> Ui<'a> {
    use theme::tenshu::button::{action, ActionShape};
    Action::new(
        id,
        text(label).size(13).font(font::MEDIUM),
        view::Message::FocusAction,
    )
    .padding([8, 12])
    .focus_radius(8.0)
    .style(move |theme, status| action(theme, status, kind, ActionShape::Compact))
    .on_press_maybe(intent)
    .into()
}

fn selection<'a>(id: &'static str, label: &'static str, checked: bool, intent: Message) -> Ui<'a> {
    use theme::tenshu::button::{action, ActionKind, ActionShape};
    Action::new(
        id,
        text(format!(
            "{} {label} · {}",
            if checked { "☑" } else { "☐" },
            if checked { "Selected" } else { "Not selected" }
        ))
        .size(13),
        view::Message::FocusAction,
    )
    .padding([8, 12])
    .focus_radius(8.0)
    .style(move |theme, status| action(theme, status, ActionKind::Secondary, ActionShape::Compact))
    .on_press(message(intent))
    .into()
}

fn section_title<'a>(title: &'static str, status: &'static str, tone: Tone) -> Ui<'a> {
    row![heading(title), chip(status, tone)]
        .spacing(9)
        .align_y(Alignment::Center)
        .into()
}

pub(super) fn view_for_cube(settings: &Settings, supported: bool) -> Ui<'_> {
    let mut page = Column::new()
        .spacing(16)
        .width(Length::Fill)
        .push(text("AI access").size(22).font(font::SEMIBOLD))
        .push(muted("Connect a local AI client to this Cube. Access is scoped to one active embedded Vault and lasts until you revoke that client."))
        .push(notice(
            column![
                heading("Your AI provider may receive financial information"),
                muted("The connection is local, but your AI client may send what it reads to its provider. Pair only a client you trust. Never paste its private configuration into a chat."),
            ]
            .spacing(5),
            Tone::Warn,
        ))
        .push(paired_clients(settings))
        .push(card(
            column![
                heading("Paired is not the same as available"),
                muted("Locking this Cube pauses protected operations; its pairing stays. After a normal unlock or relaunch, an eligible client reconnects without another approval. A client can never unlock a Cube."),
            ]
            .spacing(6),
        ))
        .push(section_title("Pair a client", "Today · manual configuration", Tone::Good));

    if !cfg!(any(unix, windows)) || !supported {
        page = page.push(notice(
            text("AI access currently supports a Bitcoin Vault served by the embedded daemon on macOS, Linux and Windows. This wallet is unavailable for MCP.").size(13),
            Tone::Warn,
        ));
    } else {
        page = page.push(pairing_card(settings));
    }

    page = page
        .push(planned_card())
        .push(muted("Today · Persistent per-client read access to one active embedded Vault. Planned · Guided pairing, multiple wallets, managed setup and general transaction preparation."));

    let content: Ui<'_> = container(page)
        .width(Length::Fill)
        .max_width(930)
        .padding([20, 24])
        .into();
    let Some(id) = settings.revoke_confirmation.as_ref() else {
        return content;
    };
    let Some(client) = settings
        .pairing_store
        .as_ref()
        .and_then(|store| store.clients().iter().find(|client| &client.id == id))
    else {
        return content;
    };
    Modal::new(content, revoke_confirmation(&client.name, id))
        .on_blur(Some(message(Message::CancelRevokePair)))
        .into()
}

pub(super) fn revoke_confirmation<'a>(client_name: &str, id: &str) -> Ui<'a> {
    container(column![
        heading("Revoke this client's access?"),
        muted(format!("{client_name} will lose access immediately. This cannot be undone; pairing again creates a new private configuration.")),
        muted("Information already read may remain with the AI provider. Revocation cannot erase data it has retained."),
        row![
            action(
                "tenshu.ai-access.revoke.keep",
                "Keep paired",
                Some(message(Message::CancelRevokePair)),
                theme::tenshu::button::ActionKind::Secondary,
            ),
            action(
                "tenshu.ai-access.revoke.confirm",
                "Revoke access",
                Some(message(Message::RevokePair(id.to_owned()))),
                theme::tenshu::button::ActionKind::Destructive,
            ),
        ].spacing(8),
    ].spacing(14))
    .width(Length::Fixed(460.0))
    .padding(22)
    .style(|theme: &theme::Theme| iced::widget::container::Style {
        text_color: Some(theme.colors.shell.ink),
        background: Some(Background::Color(theme.colors.shell.panel)),
        border: Border {
            color: theme.colors.shell.line,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn pairing_card(settings: &Settings) -> Ui<'_> {
    let mut body = Column::new()
        .spacing(11)
        .width(Length::Fill)
        .push(heading("Choose this client's access"))
        .push(muted("Each client needs a name, this Vault, and at least one read permission. Nothing is selected automatically for the next client."))
        .push(selection(
            "tenshu.ai-access.enable",
            "Enable AI access for this Cube",
            settings.enabled,
            Message::Enable(!settings.enabled),
        ));

    if settings.enabled {
        body = body
            .push(
                text_input("Client name for your reference", &settings.client_name)
                    .on_input(|value| message(Message::ClientName(value))),
            )
            .push(muted("This label is not a verified application identity."))
            .push(selection(
                "tenshu.ai-access.wallet",
                "This Cube · active embedded Vault",
                settings.wallet_allowed,
                Message::Wallet(!settings.wallet_allowed),
            ))
            .push(muted(
                "Read wallet information · choose exactly which data this client may see:",
            ));

        for (index, label) in ["Balances", "Confirmed history", "Spending-policy summary"]
            .iter()
            .enumerate()
        {
            let id = [
                "tenshu.ai-access.permission.balances",
                "tenshu.ai-access.permission.history",
                "tenshu.ai-access.permission.policy",
            ][index];
            body = body.push(selection(
                id,
                *label,
                settings.capabilities[index],
                Message::Capability(index, !settings.capabilities[index]),
            ));
        }

        #[cfg(feature = "mcp-transactions")]
        if settings
            .pairing_location
            .as_ref()
            .is_some_and(|location| location.chain_matches(crate::chain::ChainId::Regtest))
        {
            body = body
                .push(selection(
                    "tenshu.ai-access.transactions",
                    "Experimental · prepare unsigned Regtest transactions (separate scope)",
                    settings.transactions_allowed,
                    Message::Transactions(!settings.transactions_allowed),
                ))
                .push(muted("Exact satoshi recipients and fee limits only. Native review and signing remain in Tenshu. No self-send, recovery selection or fiat conversion; up to eight construction attempts per client and unlock."));
        }

        let can_pair = settings.wallet_allowed
            && (settings.capabilities.iter().any(|allowed| *allowed)
                || settings.transaction_selection())
            && !settings.client_name.trim().is_empty();
        body = body
            .push(action(
                "tenshu.ai-access.pair",
                "Pair client until revoked",
                can_pair.then(|| message(Message::Pair)),
                theme::tenshu::button::ActionKind::Primary,
            ))
            .push(muted("Copy the private configuration into the client's local settings, then clear the clipboard. Guided one-time-code pairing is planned."));

        #[cfg(any(unix, windows))]
        if let Some(server) = &settings.server {
            let available = server.grant.active();
            let calls = server.grant.successful_calls.load(Ordering::Relaxed);
            body = body.push(notice(
                column![
                    heading("Temporary session grant"),
                    muted(format!(
                        "{} · {} · successful reads this session: {}",
                        settings.approved_name,
                        if available {
                            "Authorized"
                        } else {
                            "Inactive — approve again"
                        },
                        calls
                    )),
                    row![
                        action(
                            "tenshu.ai-access.temporary.copy",
                            "Copy private client configuration",
                            available.then(|| message(Message::CopyConfig)),
                            theme::tenshu::button::ActionKind::Secondary
                        ),
                        action(
                            "tenshu.ai-access.temporary.revoke",
                            "Revoke access now",
                            Some(message(Message::Revoke)),
                            theme::tenshu::button::ActionKind::Destructive
                        ),
                    ]
                    .spacing(8),
                ]
                .spacing(8),
                Tone::Muted,
            ));
        }
    }

    if settings.failed {
        body = body.push(notice(
            text("Could not start local MCP. No access was granted.").size(13),
            Tone::Bad,
        ));
    }
    card(body)
}

pub(super) fn paired_clients(settings: &Settings) -> Ui<'_> {
    let mut section = Column::new()
        .spacing(12)
        .width(Length::Fill)
        .push(section_title("Paired clients", "Today", Tone::Good))
        .push(muted("A private configuration can impersonate its client until revoked. Client names are labels for your reference."));
    if let Some(error) = settings.pairing_error {
        section = section.push(notice(text(error).size(13), Tone::Bad));
    }
    let Some(store) = &settings.pairing_store else {
        return section.push(empty_card()).into();
    };
    if store.clients().is_empty() {
        return section.push(empty_card()).into();
    }

    for client in store.clients() {
        let runtime = settings
            .paired
            .iter()
            .find(|runtime| runtime.id == client.id);
        let available =
            settings.enabled && runtime.is_some_and(|runtime| runtime.server.grant.active());
        let status = if !settings.enabled {
            "Paused · AI access is off"
        } else if available {
            "Available while unlocked"
        } else {
            "Paused · Cube locked or wallet unavailable"
        };
        let status_tone = if available { Tone::Good } else { Tone::Warn };
        let scope = match &client.scope {
            pairings::Scope::Read { .. } => "This Cube · active embedded Vault",
            pairings::Scope::TransactionPreparation { .. } => {
                "This Cube · unsigned Regtest transaction preparation"
            }
            pairings::Scope::Proposal => "This Cube · Regtest wallet proposal experiment",
            pairings::Scope::WalletCreation => "This Cube · Regtest public-key wallet preparation",
        };
        let mut body = Column::new()
            .spacing(9)
            .width(Length::Fill)
            .push(
                row![
                    heading(client.name.clone()),
                    Space::new().width(Length::Fill),
                    chip(status, status_tone),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .push(muted(scope));

        if let pairings::Scope::Read { capabilities, .. } = &client.scope {
            let mut chips = Row::new().spacing(6);
            for (enabled, label) in
                capabilities
                    .iter()
                    .zip(["Balances", "Confirmed history", "Policy summary"])
            {
                if *enabled {
                    chips = chips.push(chip(label, Tone::Neutral));
                }
            }
            body = body.push(chips.wrap());
        }
        body = body
            .push(muted(format!(
                "Successful operations this session: {}",
                runtime.map_or(0, |runtime| runtime
                    .server
                    .grant
                    .successful_calls
                    .load(Ordering::Relaxed))
            )))
            .push(
                row![
                    action(
                        iced::advanced::widget::Id::from(format!(
                            "tenshu.ai-access.client.{}.copy",
                            client.id
                        )),
                        "Copy private configuration",
                        (!settings.pairing_poisoned && available)
                            .then(|| message(Message::CopyPair(client.id.clone()))),
                        theme::tenshu::button::ActionKind::Secondary
                    ),
                    action(
                        iced::advanced::widget::Id::from(format!(
                            "tenshu.ai-access.client.{}.revoke",
                            client.id
                        )),
                        "Revoke",
                        Some(message(Message::RequestRevokePair(client.id.clone()))),
                        theme::tenshu::button::ActionKind::Destructive
                    ),
                ]
                .spacing(8),
            );

        #[cfg(feature = "mcp-transactions")]
        if let Some(service) = runtime
            .filter(|_| available)
            .and_then(|runtime| runtime.transactions.as_ref())
        {
            for draft in service.summaries() {
                body = body
                    .push(muted(format!("Unsigned transaction: {} sats to {} recipient(s), fee {} sats. Review every address and amount before signing.", draft.amount_sats, draft.recipients.len(), draft.fee_sats)))
                    .push(action(iced::advanced::widget::Id::from(format!("tenshu.ai-access.client.{}.transaction.{}", client.id, draft.draft_id)), "Review prepared transaction in Tenshu", Some(message(Message::ReviewTransaction { client_id: client.id.clone(), draft_id: draft.draft_id })), theme::tenshu::button::ActionKind::Secondary));
            }
        }
        #[cfg(feature = "mcp-proposals")]
        if let Some(proposal) = runtime
            .filter(|_| available)
            .and_then(|runtime| runtime.proposals.as_ref())
            .and_then(|store| store.pending().ok())
            .flatten()
        {
            let keys_ready = runtime
                .and_then(|runtime| runtime.proposals.as_ref())
                .is_some_and(|store| {
                    store
                        .creation_input(&proposal.proposal_id)
                        .ok()
                        .flatten()
                        .is_some()
                });
            body = body
                .push(muted(format!(
                    "3-of-5 primary, recovery {}-of-{} after {} blocks. {}",
                    proposal.topology.recovery.quorum.threshold,
                    proposal.topology.recovery.quorum.key_count,
                    proposal.topology.recovery.relative_delay_blocks,
                    if keys_ready {
                        "Public keys and descriptor validated. Verify device ownership and backups."
                    } else {
                        "Keys are not yet selected."
                    }
                )))
                .push(action(
                    iced::advanced::widget::Id::from(format!(
                        "tenshu.ai-access.client.{}.proposal",
                        client.id
                    )),
                    if keys_ready {
                        "Review prepared wallet in Tenshu"
                    } else {
                        "Review proposal and select keys"
                    },
                    Some(message(Message::ReviewPairedProposal(client.id.clone()))),
                    theme::tenshu::button::ActionKind::Secondary,
                ));
        }
        section = section.push(card(body));
    }
    section.into()
}

fn empty_card<'a>() -> Ui<'a> {
    card(column![
        heading("No clients paired"),
        muted("Pair one client with a name and explicit read permissions. Its access lasts until you revoke it."),
    ]
    .spacing(5))
}

fn planned_card<'a>() -> Ui<'a> {
    card(column![
        section_title("More ways to work with AI", "Planned", Tone::Muted),
        muted("Guided pairing with a one-time code, multiple wallet scopes, editing permissions, AI-managed wallet setup and general unsigned transaction preparation are not available here yet."),
        Row::new()
            .spacing(6)
            .push(chip("Guided pairing · Planned", Tone::Muted))
            .push(chip("Multiple wallets · Planned", Tone::Muted))
            .push(chip("Manage setup · Planned", Tone::Muted))
            .push(chip("Prepare transactions · Planned", Tone::Muted))
            .wrap(),
    ]
    .spacing(9))
}
