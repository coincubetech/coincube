//! AI access presentation. Authorization and pairing state stay in `local_mcp`.
//! The available controls map directly to its existing messages; the product
//! concepts still in design are rendered as inert, explicitly planned content.

use super::{pairings, Message, Settings};
use crate::app::view;
#[cfg(feature = "mcp-transactions")]
use coincube_mcp::transaction::Prepared;
use coincube_ui::font;
use coincube_ui::locale::copy as localized_copy;
use coincube_ui::localized_format;
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
        text(localized_copy(label)).size(13).font(font::MEDIUM),
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
            "{} {} · {}",
            if checked { "☑" } else { "☐" },
            localized_copy(label),
            localized_copy(if checked { "Selected" } else { "Not selected" })
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
    let pair = if settings.pairing_open {
        page::button(
            PAIR_OPEN_ID,
            localized_copy("Close"),
            Some(message(Message::TogglePairing)),
        )
    } else {
        page::primary(
            PAIR_OPEN_ID,
            localized_copy("Pair a client"),
            available.then(|| message(Message::TogglePairing)),
        )
    };
    // State first: an empty list says so, with the way to pair beside it.
    rows.push(
        page::row(localized_copy(if rows.is_empty() {
            "No clients paired"
        } else {
            "Pair another client"
        }))
        .description(localized_copy(PAIR_HINT))
        .control(pair)
        .into(),
    );
    let mut clients = Column::new().width(Length::Fill);
    if let Some(error) = settings.pairing_error {
        clients = clients.push(page::note(
            Tone::Bad,
            text(localized_copy(error)).size(13),
            None,
        ));
        clients = clients.push(Space::new().height(Length::Fixed(12.0)));
    }
    clients = clients.push(page::rows(rows));
    if !available {
        clients = clients.push(page::note(
            Tone::Warn,
            text(localized_copy("AI access currently supports a Bitcoin Vault served by the embedded daemon on macOS, Linux and Windows. This wallet is unavailable for MCP.")).size(13),
            None,
        ));
    } else if settings.pairing_open {
        clients = clients.push(
            container(pairing_card(settings))
                .padding(iced::Padding::default().top(16.0))
                .width(Length::Fill),
        );
    }

    let list = |title: &'static str, items: &'static [&'static str]| -> Ui<'_> {
        items
            .iter()
            .fold(
                Column::new()
                    .spacing(6)
                    .width(Length::FillPortion(1))
                    .push(text(localized_copy(title)).size(13).font(font::SEMIBOLD)),
                |column, item| column.push(muted(localized_copy(item))),
            )
            .into()
    };
    let reads = Column::new()
        .width(Length::Fill)
        .push(page::rule())
        .push(
            container(
                row![
                    list("Shared with the client", SHARED),
                    list("Never shared", NEVER_SHARED)
                ]
                .spacing(32)
                .width(Length::Fill),
            )
            .padding([12, 0]),
        )
        .push(page::rule())
        .push(page::note(Tone::Warn, muted(localized_copy(CAUTION)), None));

    Column::new()
        .width(Length::Fill)
        .push(Space::new().height(Length::Fixed(page::GROUP_GAP)))
        .push(page::muted(localized_copy(INTRO)))
        .push(page::group(localized_copy("Paired clients"), None, clients))
        .push(page::group(
            localized_copy("What a paired client can read"),
            None,
            reads,
        ))
        .into()
}

const INTRO: &str = "Let an AI client you already use read this Cube through Tenshu's local connection. Each client gets one embedded Vault and only the permissions you choose.";
const PAIR_HINT: &str =
    "Pair once; access lasts until you revoke it. Locking this Cube pauses every client.";
const SHARED: &[&str] = &[
    "Activity order, coarse type and confirmed status",
    "Balances and policy shape, only when you allow them",
];
const NEVER_SHARED: &[&str] = &[
    "Seeds, keys, PINs, passwords and passkeys",
    "Exact transaction amounts, times, block heights and chain IDs",
];
pub(super) const CAUTION: &str = "Local is not private. Anything a client reads can be sent to its AI provider. Less metadata is not anonymity. Pair only clients you trust, and never paste a client's private configuration into a chat.";

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
                "Vault · Prepare unsigned transactions"
            }
            pairings::Scope::Proposal => "Wallet policy proposal",
            pairings::Scope::WalletCreation => "Public-key wallet preparation",
        };
        let mut detail = localized_copy(scope).to_owned();
        if let pairings::Scope::Read { capabilities, .. } = &client.scope {
            let granted: Vec<&str> = capabilities
                .iter()
                .zip(["Balances", "Confirmed history", "Policy summary"])
                .filter(|(enabled, _)| **enabled)
                .map(|(_, label)| localized_copy(label))
                .collect();
            if !granted.is_empty() {
                detail.push_str(&format!(" ({})", granted.join(", ")));
            }
        }
        detail.push_str(&format!(" · {}", localized_copy(status)));
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
                        text(localized_copy(if open { "Close" } else { "Manage" }))
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
        .push(muted(localized_copy("A private configuration can impersonate its client until revoked. Client names are labels for your reference.")))
        .push(muted(localized_format!(
            "Successful operations this session: {}",
            "Operaciones correctas en esta sesión: {}",
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
            .push(muted(localized_format!(
                "3-of-5 primary, recovery {}-of-{} after {} blocks. {}",
                "Principal 3 de 5, recuperación {} de {} después de {} bloques. {}",
                proposal.topology.recovery.quorum.threshold,
                proposal.topology.recovery.quorum.key_count,
                proposal.topology.recovery.relative_delay_blocks,
                localized_copy(if keys_ready {
                    "Public keys and descriptor validated. Verify device ownership and backups."
                } else {
                    "Keys are not yet selected."
                })
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

/// The only live transaction state is an unsigned chain-bound construction. Keep
/// the artboard's review rhythm while deriving every value from the MCP result.
#[cfg(feature = "mcp-transactions")]
fn transaction_card(client_id: &str, draft: Prepared) -> Ui<'static> {
    use crate::app::view::settings::page;

    let chain = draft
        .network
        .parse::<crate::chain::ChainId>()
        .ok()
        .filter(|chain| super::transactions::supported_chain(*chain));
    let ready = chain.is_some()
        && draft.status == "unsigned_ready_for_native_review"
        && draft.signing_required;
    let total = draft.amount_sats.checked_add(draft.fee_sats);
    let mut details: Column<'static, view::Message, theme::Theme> = Column::new()
        .spacing(0)
        .width(Length::Fill)
        .push(transaction_field(
            localized_copy("From"),
            chain
                .map(crate::chain::canonical_network_name)
                .unwrap_or_else(|| localized_copy("Unsupported network")),
            false,
        ));
    for (index, recipient) in draft.recipients.iter().enumerate() {
        let label = if draft.recipients.len() == 1 {
            localized_copy("Recipient").to_owned()
        } else {
            localized_format!("Recipient {}", "Destinatario {}", index + 1)
        };
        let amount_label = if draft.recipients.len() == 1 {
            localized_copy("Amount").to_owned()
        } else {
            localized_format!("Amount {}", "Importe {}", index + 1)
        };
        details = details
            .push(transaction_field(label, recipient.address.clone(), true))
            .push(transaction_field(
                amount_label,
                transaction_sats(recipient.amount_sats),
                true,
            ));
    }
    details = details
        .push(transaction_field(
            localized_copy("Payment total"),
            localized_format!(
                "{} · exact amount requested",
                "{} · importe exacto solicitado",
                transaction_sats(draft.amount_sats)
            ),
            true,
        ))
        .push(transaction_field(
            localized_copy("Miner fee"),
            localized_format!(
                "{} · estimated {} sat/vB · added on top",
                "{} · estimada en {} sat/vB · se suma al importe",
                transaction_sats(draft.fee_sats),
                draft.fee_rate_sat_vb
            ),
            true,
        ))
        .push(transaction_field(
            localized_copy("Total leaving Vault"),
            total.map_or_else(
                || localized_copy("Unavailable").to_owned(),
                transaction_sats,
            ),
            true,
        ))
        .push(transaction_field(
            localized_copy("Change"),
            if draft.change_sats == 0 {
                localized_copy("No change output").to_owned()
            } else {
                localized_format!(
                    "{} returned to this Vault",
                    "{} vuelven a este Vault",
                    transaction_sats(draft.change_sats)
                )
            },
            true,
        ))
        .push(transaction_field(
            localized_copy("Inputs"),
            localized_format!(
                "{} selected by Tenshu",
                "{} seleccionadas por Tenshu",
                draft.input_count
            ),
            false,
        ))
        .push(transaction_field(
            localized_copy("Signatures"),
            localized_copy("None at preparation · current signing status is not shown here"),
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
            row![
                heading(localized_copy("Prepared transaction")),
                page::scope_tag(match chain {
                    Some(crate::chain::ChainId::Bitcoin) => "BTC · MAINNET",
                    Some(crate::chain::ChainId::Testnet4) => "BTC · TESTNET4",
                    Some(crate::chain::ChainId::Regtest) => "REGTEST",
                    Some(crate::chain::ChainId::BitcoinBlake2b) => "BTCB2 · MAINNET",
                    Some(crate::chain::ChainId::BitcoinBlake2bTestnet4) => "BTCB2 · TESTNET4",
                    _ => localized_copy("UNSUPPORTED"),
                })
            ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
        )
        .push(muted(localized_copy("Prepared by this paired client with Tenshu's coin selection and fee engine. Preparing does not send bitcoin; check every recipient address and amount before signing. The assistant cannot spend.")))
        .push(page::status(
            if review { page::Status::Ok } else { page::Status::Warn },
            localized_copy(if review { "Unsigned when prepared · current status is not tracked here" } else { "Draft unavailable for native review" }),
        ))
        .push(details)
        .push(action(
            action_id,
            "Review and sign in Tenshu",
            review_message,
            theme::tenshu::button::ActionKind::Primary,
        ))
        .push(muted(localized_copy("This card does not update after native review. Fiat conversion, quote refresh, and signer availability are planned. This tool accepts exact satoshi amounts only. The native Send flow owns signing and broadcast.")));
    card(content)
}

#[cfg(feature = "mcp-transactions")]
fn transaction_sats(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{grouped} SATS")
}

#[cfg(feature = "mcp-transactions")]
fn transaction_field(
    label: impl Into<String>,
    value: impl Into<String>,
    mono: bool,
) -> Ui<'static> {
    use crate::app::view::settings::page;
    let value = text(value.into())
        .size(13)
        .width(Length::Fill)
        .wrapping(iced::widget::text::Wrapping::WordOrGlyph);
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

#[cfg(all(test, feature = "mcp-transactions"))]
mod mcp_transaction_presentation_tests {
    use super::*;
    use crate::app::view::shared::landing::capture;
    use coincube_core::miniscript::bitcoin::{
        hashes::Hash, Address, Network, ScriptBuf, WScriptHash,
    };
    use coincube_mcp::transaction::Recipient;
    use coincube_ui::widget::Renderer;
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{operation, operation::focusable, operation::Scrollable, Operation, Tree},
        Layout,
    };
    use iced::{Rectangle, Size, Vector};

    const SCROLL_ID: &str = "r25.transaction.qa-scroll";
    const REVIEW_ID: &str = "tenshu.ai-access.client.synthetic-client.transaction.synthetic-draft";

    #[derive(Default)]
    struct Probe {
        scroll: Option<(Rectangle, Rectangle, Vector)>,
        focus: Vec<(iced::advanced::widget::Id, Rectangle, bool)>,
    }

    impl Operation for Probe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn scrollable(
            &mut self,
            id: Option<&iced::advanced::widget::Id>,
            bounds: Rectangle,
            content: Rectangle,
            translation: Vector,
            _state: &mut dyn Scrollable,
        ) {
            if id == Some(&iced::advanced::widget::Id::from(SCROLL_ID)) {
                self.scroll = Some((bounds, content, translation));
            }
        }

        fn focusable(
            &mut self,
            id: Option<&iced::advanced::widget::Id>,
            bounds: Rectangle,
            state: &mut dyn operation::Focusable,
        ) {
            if let Some(id) = id {
                self.focus.push((id.clone(), bounds, state.is_focused()));
            }
        }
    }

    fn probe(
        element: &mut Ui<'_>,
        tree: &mut Tree,
        node: &layout::Node,
        renderer: &Renderer,
    ) -> Probe {
        let mut probe = Probe::default();
        element
            .as_widget_mut()
            .operate(tree, Layout::new(node), renderer, &mut probe);
        probe
    }

    fn operate(
        element: &mut Ui<'_>,
        tree: &mut Tree,
        node: &layout::Node,
        renderer: &Renderer,
        operation: impl Operation<()> + 'static,
    ) {
        let mut operation: Box<dyn Operation<()>> = Box::new(operation);
        loop {
            element.as_widget_mut().operate(
                tree,
                Layout::new(node),
                renderer,
                &mut operation::black_box(operation.as_mut()),
            );
            match operation.finish() {
                operation::Outcome::Chain(next) => operation = next,
                _ => break,
            }
        }
    }

    // Address-valid display fixture. Fee, rate and input count are independent
    // presentation values, not an output of the native construction engine.
    fn address_valid_display_draft() -> Prepared {
        Prepared {
            wallet_id: "synthetic-wallet".into(),
            draft_id: "synthetic-draft".into(),
            network: "regtest".into(),
            status: "unsigned_ready_for_native_review".into(),
            recipients: (1..=8)
                .map(|index| {
                    let script = ScriptBuf::new_p2wsh(&WScriptHash::from_byte_array([index; 32]));
                    Recipient {
                        address: Address::from_script(&script, Network::Regtest)
                            .unwrap()
                            .to_string(),
                        amount_sats: 10_000,
                    }
                })
                .collect(),
            amount_sats: 80_000,
            fee_sats: 500,
            fee_rate_sat_vb: 42,
            change_sats: 50_000,
            input_count: 12,
            signing_required: true,
        }
    }

    fn schema_boundary_draft() -> Prepared {
        // PrepareInput accepts this 90-character ASCII boundary, but native
        // address validation rejects it. This fixture stresses layout only.
        let address = format!("bcrt1{}", "q".repeat(85));
        assert_eq!(address.len(), 90);
        let mut draft = address_valid_display_draft();
        draft.recipients[0].address = address;
        draft.recipients[0].amount_sats = 123_456_789;
        draft.amount_sats = 123_526_789;
        draft.fee_sats = 12_345;
        draft.change_sats = 2_100_000_000_000;
        draft
    }

    #[test]
    fn transaction_card_boundary_capture_and_keyboard() {
        let draft = address_valid_display_draft();
        let review_id = iced::advanced::widget::Id::from(REVIEW_ID);
        for (appearance, palette) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            let inspection = capture::inspect(
                "r25-eight-address-valid-display-only",
                transaction_card("synthetic-client", draft.clone()),
                (1024, 768),
                appearance,
                palette,
            );
            assert!(inspection.contains("Recipient 8"));
            assert!(inspection.contains("Amount 8"));
            assert!(inspection.contains("80,000 SATS"));
            assert!(inspection.contains("80,500 SATS"));
            assert!(inspection.contains("50,000 SATS"));
            assert!(inspection.has_focus(review_id.clone()));
            let schema = capture::inspect(
                "r25-90-char-schema-layout-only",
                transaction_card("synthetic-client", schema_boundary_draft()),
                (1024, 768),
                appearance,
                if appearance == "dark" {
                    theme::Theme::dark()
                } else {
                    theme::Theme::light()
                },
            );
            assert!(schema.contains("123,526,789 SATS"));
            assert!(schema.contains("123,539,134 SATS"));
            assert!(schema.contains("2,100,000,000,000 SATS"));
        }
        for key in [
            iced::keyboard::key::Named::Enter,
            iced::keyboard::key::Named::Space,
        ] {
            let emitted = capture::activate_with_key(
                transaction_card("synthetic-client", draft.clone()),
                review_id.clone(),
                (1024, 768),
                false,
                key,
            );
            assert!(emitted.iter().any(|intent| matches!(intent,
                view::Message::LocalMcp(Message::ReviewTransaction { client_id, draft_id })
                    if client_id == "synthetic-client" && draft_id == "synthetic-draft"
            )));
        }
        let mut unavailable = draft;
        unavailable.change_sats = 0;
        let no_change = capture::inspect(
            "r25-zero-change",
            transaction_card("synthetic-client", unavailable.clone()),
            (1024, 768),
            "light",
            theme::Theme::light(),
        );
        assert!(no_change.contains("No change output"));
        unavailable.network = "mainnet".into();
        let inspection = capture::inspect(
            "r25-mainnet-gated",
            transaction_card("synthetic-client", unavailable.clone()),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(inspection.contains("Draft unavailable for native review"));
        assert!(!inspection.has_focus(review_id));
    }

    #[test]
    fn transaction_card_tab_order_and_scroll_viewport() {
        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let mut renderer = iced::futures::executor::block_on(<Renderer as Headless>::new(
            font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        // The 1024px shell leaves a 652px content column and a 668px viewport.
        let size = Size::new(652.0, 668.0);
        let content = iced::widget::column![
            action(
                "r25.transaction.previous",
                "Previous control",
                Some(message(Message::TogglePairing)),
                theme::tenshu::button::ActionKind::Secondary,
            ),
            transaction_card("synthetic-client", address_valid_display_draft()),
        ];
        let mut element: Ui<'_> = iced::widget::scrollable(content)
            .id(SCROLL_ID)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let initial = probe(&mut element, &mut tree, &node, &renderer);
        let (viewport, content, _) = initial.scroll.expect("mounted scrollable");
        assert!(
            content.height > viewport.height,
            "eight recipients need vertical scroll"
        );
        let review = iced::advanced::widget::Id::from(REVIEW_ID);
        let review_bounds = initial
            .focus
            .iter()
            .find(|(id, _, _)| id == &review)
            .expect("native review CTA in Tab order")
            .1;
        assert!(review_bounds.y > viewport.y + viewport.height);

        operate(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            focusable::focus_next::<()>(),
        );
        assert!(probe(&mut element, &mut tree, &node, &renderer)
            .focus
            .iter()
            .any(|(id, _, focused)| id
                == &iced::advanced::widget::Id::from("r25.transaction.previous")
                && *focused));
        operate(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            focusable::focus_next::<()>(),
        );
        assert!(probe(&mut element, &mut tree, &node, &renderer)
            .focus
            .iter()
            .any(|(id, _, focused)| id == &review && *focused));
        operate(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            focusable::focus_previous::<()>(),
        );
        assert!(probe(&mut element, &mut tree, &node, &renderer)
            .focus
            .iter()
            .any(|(id, _, focused)| id
                == &iced::advanced::widget::Id::from("r25.transaction.previous")
                && *focused));

        let offset = review_bounds.y - viewport.y - viewport.height + review_bounds.height + 12.0;
        operate(
            &mut element,
            &mut tree,
            &node,
            &renderer,
            operation::scrollable::scroll_to::<()>(
                iced::advanced::widget::Id::from(SCROLL_ID),
                operation::scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(offset),
                },
            ),
        );
        let scrolled = probe(&mut element, &mut tree, &node, &renderer);
        let (viewport, _, translation) = scrolled.scroll.expect("scroll still mounted");
        assert!(translation.y > 0.0, "scroll offset must move");
        let cta = scrolled
            .focus
            .iter()
            .find(|(id, _, _)| id == &review)
            .expect("review CTA after scroll")
            .1;
        let visible_y = cta.y - translation.y;
        assert!(
            visible_y >= viewport.y && visible_y + cta.height <= viewport.y + viewport.height,
            "CTA {:?}, viewport {:?}, translation {:?}",
            cta,
            viewport,
            translation
        );
        if let Ok(directory) = std::env::var("R06A_CAPTURE_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            // Iced Button computes Active/Disabled on RedrawRequested. Drawing
            // the initial tree directly would falsely show every Action faded.
            let mut messages = Vec::new();
            element.as_widget_mut().update(
                &mut tree,
                &iced::Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                )),
                Layout::new(&node),
                iced::mouse::Cursor::Unavailable,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut iced::advanced::Shell::new(&mut messages),
                &Rectangle::with_size(size),
            );
            for (appearance, palette) in [
                ("dark", theme::Theme::dark()),
                ("light", theme::Theme::light()),
            ] {
                element.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &palette,
                    &iced::advanced::renderer::Style {
                        text_color: palette.colors.shell.ink,
                    },
                    Layout::new(&node),
                    iced::mouse::Cursor::Unavailable,
                    &Rectangle::with_size(size),
                );
                let pixels = renderer.screenshot(
                    Size::new(size.width as u32, size.height as u32),
                    1.0,
                    palette.colors.general.background,
                );
                image::save_buffer(
                    std::path::Path::new(&directory).join(format!(
                        "r25-eight-address-valid-display-only-scrolled-{appearance}-1024x768-content-only-synthetic.png"
                    )),
                    &pixels,
                    size.width as u32,
                    size.height as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
    }
}

pub(super) fn revoke_confirmation<'a>(client_name: &str, id: &str) -> Ui<'a> {
    container(column![
        heading(localized_copy("Revoke this client's access?")),
        muted(localized_format!(
            "{client_name} will lose access immediately. This cannot be undone; pairing again creates a new private configuration.",
            "{client_name} perderá el acceso de inmediato. Esto no se puede deshacer; si lo vinculas de nuevo, se creará una nueva configuración privada."
        )),
        muted(localized_copy("Information already read may remain with the AI provider. Revocation cannot erase data it has retained.")),
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
        .push(heading(localized_copy("Choose this client's access")))
        .push(muted(localized_copy("Each client needs a name, this Vault, and an explicit permission. Nothing is selected automatically for the next client.")))
        .push(selection(
            "tenshu.ai-access.enable",
            "Enable AI access for this Cube",
            settings.enabled,
            Message::Enable(!settings.enabled),
        ));

    if settings.enabled {
        body = body
            .push(
                text_input(
                    localized_copy("Client name for your reference"),
                    &settings.client_name,
                )
                .on_input(|value| message(Message::ClientName(value))),
            )
            .push(muted(localized_copy(
                "This label is not a verified application identity.",
            )))
            .push(selection(
                "tenshu.ai-access.wallet",
                "This Cube · active embedded Vault",
                settings.wallet_allowed,
                Message::Wallet(!settings.wallet_allowed),
            ))
            .push(muted(localized_copy(
                "Read wallet information · choose exactly which data this client may see:",
            )));

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
            .is_some_and(|location| location.transaction_supported())
        {
            body = body
                .push(selection(
                    "tenshu.ai-access.transactions",
                    "Prepare unsigned transactions (separate scope)",
                    settings.transactions_allowed,
                    Message::Transactions(!settings.transactions_allowed),
                ))
                .push(muted(localized_copy("Exact satoshi recipients and fee limits only. Native review and signing remain in Tenshu. No self-send, recovery selection or fiat conversion; up to eight construction attempts per client and unlock.")));
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
            .push(muted(localized_copy("Copy the private configuration into the client's local settings, then clear the clipboard. Guided one-time-code pairing is planned.")));

        #[cfg(any(unix, windows))]
        if let Some(server) = &settings.server {
            let available = server.grant.active();
            let calls = server.grant.successful_calls.load(Ordering::Relaxed);
            body = body.push(notice(
                column![
                    heading(localized_copy("Temporary session grant")),
                    muted(localized_format!(
                        "{} · {} · successful reads this session: {}",
                        "{} · {} · lecturas correctas en esta sesión: {}",
                        settings.approved_name,
                        localized_copy(if available {
                            "Authorized"
                        } else {
                            "Inactive — approve again"
                        }),
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
            text(localized_copy(
                "Could not start local MCP. No access was granted.",
            ))
            .size(13),
            Tone::Bad,
        ));
    }
    card(body)
}

/// The paired-client rows on their own, for the proposal view.
#[cfg(feature = "mcp-proposals")]
pub(super) fn paired_clients(settings: &Settings) -> Ui<'_> {
    crate::app::view::settings::page::rows(paired_client_rows(settings))
}

#[cfg(test)]
mod ai_access_tests {
    use super::*;
    use crate::app::view::shared::landing::capture;

    /// State first: an empty Cube says "No clients paired" beside "Pair a
    /// client", then what a client can and can't read, then one caution.
    #[test]
    fn an_empty_cube_leads_with_its_state_and_lists_what_is_shared() {
        let settings = Settings::default();
        let probe = capture::inspect(
            "ai-access-empty",
            view_for_cube(&settings, true),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        for label in [
            "Paired clients",
            "No clients paired",
            "Pair a client",
            "Locking this Cube pauses every client.",
            "Shared with the client",
            "Never shared",
            "Seeds, keys, PINs, passwords and passkeys",
            "Local is not private.",
        ] {
            assert!(probe.contains(label), "{}", label);
        }
        assert!(!probe.contains("Your AI provider may receive financial information"));
        assert!(!probe.contains("Open AI access"));
        let opened = capture::activate(
            view_for_cube(&settings, true),
            iced::advanced::widget::Id::new(PAIR_OPEN_ID),
            (1440, 900),
            true,
        );
        assert!(opened
            .iter()
            .any(|m| matches!(m, view::Message::LocalMcp(Message::TogglePairing))));
    }
}
