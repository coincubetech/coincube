//! AI access presentation. Authorization and pairing state stay in `local_mcp`.
//! The available controls map directly to its existing messages; the product
//! concepts still in design are rendered as inert, explicitly planned content.

use super::{pairings, Message, Settings};
use crate::app::view;
#[cfg(feature = "mcp-transactions")]
use coincube_mcp::transaction::Prepared;
use coincube_ui::font;
use coincube_ui::theme::{self, tenshu::Tone};
use coincube_ui::widget::{action::Action, Element};
use iced::widget::{column, container, row, text, text_input, Column, Space};
use iced::{Background, Border, Length};
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

pub(super) fn view_for_cube(settings: &Settings, supported: bool) -> Ui<'_> {
    use crate::app::view::settings::page;
    let mut rows: Vec<Ui<'_>> = paired_client_rows(settings);
    let available = cfg!(any(unix, windows)) && supported;
    rows.push(
        page::row("Pair a client")
            .description("Pair once; access lasts until you revoke it. Each client gets only the Vault and permissions you choose.")
            .control(if settings.pairing_open {
                page::button(PAIR_OPEN_ID, "Close", Some(message(Message::TogglePairing)))
            } else {
                page::primary(
                    PAIR_OPEN_ID,
                    "Open AI access",
                    available.then(|| message(Message::TogglePairing)),
                )
            })
            .into(),
    );
    let mut content = Column::new().width(Length::Fill);
    if let Some(error) = settings.pairing_error {
        content = content.push(page::note(Tone::Bad, text(error).size(13), None));
        content = content.push(Space::new().height(Length::Fixed(12.0)));
    }
    content = content.push(page::note(
        Tone::Warn,
        column![
            heading("Your AI provider may receive financial information"),
            muted("The connection is local, but your AI client may send what it reads to its provider. History shares activity order, coarse type and confirmed status, but omits exact transaction amounts, times, block heights and chain ids. This reduces metadata; it does not guarantee anonymity. Balances and policy shape are included when selected. Pair only a client you trust. Never paste its private configuration into a chat."),
        ]
        .spacing(5),
        None,
    ));
    content = content.push(page::rows(rows));
    if !available {
        content = content.push(page::note(
            Tone::Warn,
            text("AI access currently supports a Bitcoin Vault served by the embedded daemon on macOS, Linux and Windows. This wallet is unavailable for MCP.").size(13),
            None,
        ));
    } else if settings.pairing_open {
        content = content.push(
            container(pairing_card(settings))
                .padding(iced::Padding::default().top(16.0))
                .width(Length::Fill),
        );
    }
    content = content.push(page::caption(
        "A local connection is not a private one: anything a paired client reads can be sent to its AI provider. Locking this Cube pauses a client; its pairing stays until you revoke it.",
    ));
    page::group(
        "AI access",
        Some("Let the AI you already use read this Cube through Tenshu's local connection. Access is scoped to one active embedded Vault. Seeds, keys, PINs, passwords and passkeys never enter the conversation.".into()),
        content,
    )
}

const PAIR_OPEN_ID: &str = "tenshu.ai-access.open";

/// One ruled row per paired client: its name, scope, permissions and state,
/// with "Manage" opening its controls beneath.
fn paired_client_rows(settings: &Settings) -> Vec<Ui<'_>> {
    use crate::app::view::settings::page;
    let Some(store) = &settings.pairing_store else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for client in store.clients() {
        let runtime = settings
            .paired
            .iter()
            .find(|runtime| runtime.id == client.id);
        let available =
            settings.enabled && runtime.is_some_and(|runtime| runtime.server.grant.active());
        let status = if !settings.enabled {
            "Paused, AI access is off"
        } else if available {
            "Available"
        } else {
            "Paused, Cube locked or wallet unavailable"
        };
        let scope = match &client.scope {
            pairings::Scope::Read { .. } => "Vault · Read wallet information",
            pairings::Scope::TransactionPreparation { .. } => {
                "Vault · Prepare unsigned Regtest transactions"
            }
            pairings::Scope::Proposal => "Regtest wallet proposal experiment",
            pairings::Scope::WalletCreation => "Regtest public-key wallet preparation",
        };
        let mut detail = scope.to_owned();
        if let pairings::Scope::Read { capabilities, .. } = &client.scope {
            let granted: Vec<&str> = capabilities
                .iter()
                .zip(["Balances", "Confirmed history", "Policy summary"])
                .filter(|(enabled, _)| **enabled)
                .map(|(_, label)| label)
                .collect();
            if !granted.is_empty() {
                detail.push_str(&format!(" ({})", granted.join(", ")));
            }
        }
        detail.push_str(&format!(" · {}", status));
        let open = settings.managing.as_deref() == Some(client.id.as_str())
            || settings.revoke_confirmation.as_deref() == Some(client.id.as_str());
        rows.push(
            page::row(client.name.clone())
                .description(detail)
                .control(
                    Action::new(
                        iced::advanced::widget::Id::from(format!(
                            "tenshu.ai-access.client.{}.manage",
                            client.id
                        )),
                        text(if open { "Close" } else { "Manage" })
                            .size(13)
                            .font(font::MEDIUM),
                        view::Message::FocusAction,
                    )
                    .padding([0, 12])
                    .height(Length::Fixed(32.0))
                    .focus_radius(8.0)
                    .style(|theme, status| {
                        theme::tenshu::button::action(
                            theme,
                            status,
                            theme::tenshu::button::ActionKind::Secondary,
                            theme::tenshu::button::ActionShape::Compact,
                        )
                    })
                    .on_press(message(Message::ManageClient(client.id.clone()))),
                )
                .into(),
        );
        if open {
            rows.push(client_controls(settings, client, runtime, available));
        }
    }
    rows
}

/// A paired client's controls, inset under its row: session use, copying
/// its private configuration and revoking it.
fn client_controls<'a>(
    settings: &'a Settings,
    client: &'a pairings::Client,
    runtime: Option<&'a super::PairedRuntime>,
    available: bool,
) -> Ui<'a> {
    use crate::app::view::settings::page;
    #[allow(unused_mut)]
    let mut body = Column::new()
        .spacing(9)
        .width(Length::Fill)
        .push(muted("A private configuration can impersonate its client until revoked. Client names are labels for your reference."))
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
            body = body.push(transaction_card(&client.id, draft));
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
    Column::new()
        .push(page::disclosure_body([container(body)
            .padding([12, 0])
            .width(Length::Fill)
            .into()]))
        .push(page::rule())
        .into()
}

/// The only live transaction state is an unsigned Regtest construction. Keep
/// the artboard's review rhythm while deriving every value from the MCP result.
#[cfg(feature = "mcp-transactions")]
fn transaction_card(client_id: &str, draft: Prepared) -> Ui<'static> {
    use crate::app::view::settings::page;

    let ready = draft.network == "regtest"
        && draft.status == "unsigned_ready_for_native_review"
        && draft.signing_required;
    let total = draft.amount_sats.checked_add(draft.fee_sats);
    let mut details: Column<'static, view::Message, theme::Theme> = Column::new()
        .spacing(0)
        .width(Length::Fill)
        .push(transaction_field(
            "From",
            "Active Vault · Bitcoin Regtest",
            false,
        ));
    for (index, recipient) in draft.recipients.iter().enumerate() {
        let label = if draft.recipients.len() == 1 {
            "Recipient".to_owned()
        } else {
            format!("Recipient {}", index + 1)
        };
        details = details.push(transaction_field(
            label,
            format!(
                "{} · {} sats · Regtest address",
                recipient.address, recipient.amount_sats
            ),
            true,
        ));
    }
    details = details
        .push(transaction_field(
            "Payment total",
            format!("{} sats · exact amount requested", draft.amount_sats),
            true,
        ))
        .push(transaction_field(
            "Miner fee",
            format!(
                "{} sats · estimated {} sat/vB · added on top",
                draft.fee_sats, draft.fee_rate_sat_vb
            ),
            true,
        ))
        .push(transaction_field(
            "Total leaving Vault",
            total.map_or_else(|| "Unavailable".to_owned(), |sats| format!("{sats} sats")),
            true,
        ))
        .push(transaction_field(
            "Change",
            if draft.change_sats == 0 {
                "No change output".to_owned()
            } else {
                format!("{} sats returned to this Vault", draft.change_sats)
            },
            true,
        ))
        .push(transaction_field(
            "Inputs",
            format!("{} selected by Tenshu", draft.input_count),
            false,
        ))
        .push(transaction_field(
            "Signatures",
            "None collected · native signing required",
            false,
        ));

    let review = ready && total.is_some();
    let action_id = iced::advanced::widget::Id::from(format!(
        "tenshu.ai-access.client.{}.transaction.{}",
        client_id, draft.draft_id
    ));
    let review_message = review.then(|| {
        message(Message::ReviewTransaction {
            client_id: client_id.to_owned(),
            draft_id: draft.draft_id,
        })
    });
    let content = Column::new()
        .spacing(12)
        .width(Length::Fill)
        .push(
            row![heading("Prepared transaction"), page::scope_tag("TODAY"), page::scope_tag("REGTEST")]
                .spacing(8)
                .align_y(iced::Alignment::Center),
        )
        .push(muted("Prepared by this paired client with Tenshu's coin selection and fee engine. Preparing does not send bitcoin; inspect the full recipient and amounts before signing."))
        .push(page::status(
            if review { page::Status::Ok } else { page::Status::Warn },
            if review { "Ready for native review · unsigned" } else { "Draft unavailable for native review" },
        ))
        .push(details)
        .push(action(
            action_id,
            "Review and sign in Tenshu",
            review_message,
            theme::tenshu::button::ActionKind::Primary,
        ))
        .push(muted("Fiat conversion, quote refresh, signer availability, and signed or broadcast status are planned. This experimental tool accepts exact satoshi amounts only. The native Send flow owns signing and broadcast."));
    card(content)
}

#[cfg(feature = "mcp-transactions")]
fn transaction_field(
    label: impl Into<String>,
    value: impl Into<String>,
    mono: bool,
) -> Ui<'static> {
    use crate::app::view::settings::page;
    let value = text(value.into()).size(13).width(Length::Fill);
    let value = if mono {
        value.font(font::MONO_REGULAR)
    } else {
        value
    };
    column![
        page::rule(),
        container(
            row![muted(label.into()).width(Length::Fixed(142.0)), value,]
                .spacing(12)
                .width(Length::Fill),
        )
        .padding([9, 0]),
    ]
    .width(Length::Fill)
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
        .push(muted("Each client needs a name, this Vault, and an explicit permission. Nothing is selected automatically for the next client."))
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
                label,
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

/// The paired-client rows on their own, for the Regtest proposal view.
#[cfg(feature = "mcp-proposals")]
pub(super) fn paired_clients(settings: &Settings) -> Ui<'_> {
    crate::app::view::settings::page::rows(paired_client_rows(settings))
}
