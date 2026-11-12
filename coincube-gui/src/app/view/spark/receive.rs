//! View renderer for [`crate::app::state::spark::receive::SparkReceive`].

use coincube_ui::locale::copy as localized_copy;
use coincube_ui::locale::Text as CopyText;
use coincube_ui::localized_format;
use std::collections::{HashMap, HashSet};

use coincube_core::miniscript::bitcoin::Network;
use coincube_ui::{
    color,
    component::{amount::BitcoinDisplayUnit, button, text::*},
    icon,
    image::{asset_network_logo, network_logo, usdt_network_logo},
    theme,
    widget::{Column, ColumnExt, Container, Element, Row},
};
use iced::{
    widget::{button as iced_button, container, qr_code, scrollable, text_input, QRCode, Space},
    Alignment, Background, Length,
};

use coincube_spark_protocol::DepositInfo;

use crate::app::state::spark::cross_chain::supported_on;
use crate::app::state::spark::esplora::DEPOSIT_MATURITY_CONFIRMATIONS;
use crate::app::state::spark::receive::{SparkReceiveMethod, SparkReceivePhase};
use crate::app::view::shared::picker::picker_modal_card;
use crate::app::view::spark::{last_tx::last_transactions_section, SparkRecentTransaction};
use crate::app::view::{Message, SparkReceiveMessage};
use crate::services::sideshift::{deposit_options, DepositOption};

pub struct SparkReceiveView<'a> {
    pub backend_available: bool,
    pub method: SparkReceiveMethod,
    pub amount_input: &'a str,
    pub description_input: &'a str,
    pub phase: &'a SparkReceivePhase,
    pub qr_data: Option<&'a qr_code::Data>,
    pub connect_signed_in: bool,
    pub lightning_address: Option<&'a str>,
    pub lightning_address_qr: Option<&'a qr_code::Data>,
    /// Phase 4f: list of pending on-chain deposits surfaced from the
    /// SDK's `list_unclaimed_deposits` RPC. Rendered as a dedicated
    /// card below the main phase body.
    pub pending_deposits: &'a [DepositInfo],
    /// Phase 4f: which deposits are currently being claimed (in-flight
    /// RPCs). Used to disable each row's button while waiting.
    pub claiming: &'a HashSet<(String, u32)>,
    /// Phase 4f: transient error from the most recent claim attempt.
    pub claim_error: Option<&'a str>,
    /// Deferred claims (SDK 0.28.0+), keyed by deposit so every waiting
    /// deposit keeps its own explanation on its row.
    pub claim_notices: &'a HashMap<(String, u32), String>,
    /// Live on-chain confirmation count per pending deposit, keyed by
    /// `(txid, vout)`. Sourced from a public Esplora and refreshed on a
    /// 30s tick; missing keys render the SDK's plain fallback text.
    pub pending_deposit_confirmations: &'a HashMap<(String, u32), u32>,
    /// Bitcoin network the cube is running on. Drives the Pending
    /// Deposits explanatory copy (live counts only available on mainnet).
    pub network: Network,
    pub cube_name: &'a str,
    pub received_amount_display: &'a str,
    pub received_celebration_context: &'a str,
    pub received_quote: &'a coincube_ui::component::quote_display::Quote,
    pub received_image_handle: &'a iced::widget::image::Handle,
    pub recent_transactions: &'a [SparkRecentTransaction],
    /// Unified balance (sats: BTC + Stable Balance), shown on the YOU RECEIVE card.
    pub balance_sats: u64,
    /// The side panel's balances, each in its own unit.
    pub balances: crate::app::view::spark::SparkBalances<'a>,
    pub bitcoin_unit: BitcoinDisplayUnit,
    pub show_direction_badges: bool,
    pub mask_amounts: bool,
    /// When the cross-network (SideShift) flow is active, its inline body —
    /// rendered below the two cards in place of the Bitcoin rail form.
    pub sideshift_body: Option<Element<'a, Message>>,
    /// The cross-network asset the SideShift flow is set to (for the THEY SEND
    /// card). `None` when a Bitcoin rail is selected.
    pub cross_network_selected: Option<DepositOption>,
    /// The pending deposit that is the active swap's own settle output. The
    /// swap body already shows it (amount, confirmations), so the Pending
    /// deposits card leaves it out instead of saying it twice.
    pub swap_deposit: Option<(&'a str, u32)>,
    /// Auto-convert is on: what arrives is converted to US Dollar (USDB),
    /// which the YOU RECEIVE tile shows.
    pub receive_usd: bool,
}

impl<'a> SparkReceiveView<'a> {
    pub fn render(self) -> Element<'a, Message> {
        if !self.backend_available {
            return Column::new()
                .spacing(20)
                .push(p1_regular(localized_copy(
                    "Spark is not available for this cube. Set up a Spark \
                     signer to receive payments.",
                )))
                .into();
        }

        // ── Full-screen celebration for received payments ─────────────
        if let SparkReceivePhase::Received { count, .. } = self.phase {
            let verb_phrase = if *count > 1 {
                CopyText::HaveArrived.get()
            } else {
                CopyText::HasArrived.get()
            };
            return coincube_ui::component::received_celebration_page(
                self.received_celebration_context,
                if self.mask_amounts {
                    "••••••"
                } else {
                    self.received_amount_display
                },
                self.received_quote,
                self.received_image_handle,
                verb_phrase,
                Message::SparkReceive(SparkReceiveMessage::Reset),
            );
        }

        // What the payer sends — asset and network, Bitcoin rail or stablecoin
        // — is one choice, made from the THEY SEND tile's picker, as on Send.
        let mut form = Column::new().spacing(0).push(
            Container::new(
                Column::new()
                    .spacing(14)
                    .push(h4_bold(localized_copy("Payment details")))
                    .push(spark_receive_cards(
                        self.method,
                        self.cross_network_selected,
                        self.receive_usd,
                        self.network,
                    ))
                    .push(arrival_note(self.network, self.receive_usd)),
            )
            .padding([18, 24]),
        );

        if let Some(body) = self.sideshift_body {
            // Cross-network (SideShift) selected: its refund/deposit flow renders
            // inline here — the same slot the Bitcoin rail form would occupy.
            form = form
                .push(iced::widget::rule::horizontal(1))
                .push(Container::new(body).padding([18, 24]));
        } else {
            if self.method == SparkReceiveMethod::LightningAddress {
                form = form.push(iced::widget::rule::horizontal(1)).push(
                    Container::new(lightning_address_body(
                        self.connect_signed_in,
                        self.lightning_address,
                        self.lightning_address_qr,
                    ))
                    .padding([18, 24]),
                );
            } else {
                // ── Bitcoin rail: method-specific inputs ──────────────────
                let method_form: Element<'a, Message> = if self.method == SparkReceiveMethod::Bolt11
                {
                    let amount = text_input("Amount in sats (optional)", self.amount_input)
                        .on_input(|v| {
                            Message::SparkReceive(
                                crate::app::view::SparkReceiveMessage::AmountInputChanged(v),
                            )
                        })
                        .padding(10);

                    let description = text_input(
                        "Description shown to the payer (optional)",
                        self.description_input,
                    )
                    .on_input(|v| {
                        Message::SparkReceive(
                            crate::app::view::SparkReceiveMessage::DescriptionInputChanged(v),
                        )
                    })
                    .padding(10);

                    Container::new(
                        Column::new()
                            .spacing(10)
                            .push(h4_bold(localized_copy("Invoice details")))
                            .push(amount)
                            .push(Space::new().height(Length::Fixed(6.0)))
                            .push(description),
                    )
                    .padding(16)
                    .style(theme::card::simple)
                    .into()
                } else if self.method == SparkReceiveMethod::OnchainBitcoin {
                    // On-chain: nothing to configure. Explain the deposit
                    // model so the user knows what to expect.
                    Container::new(
                        Column::new()
                            .spacing(8)
                            .push(h4_bold(localized_copy("Spark on-chain receive")))
                            .push(p2_regular(localized_format!(
                                "Deposits become spendable after {} on-chain confirmations.", "Puedes gastar los depósitos después de {} confirmaciones en la cadena.",
                                DEPOSIT_MATURITY_CONFIRMATIONS,
                            ))),
                    )
                    .padding(16)
                    .style(theme::card::simple)
                    .into()
                } else {
                    // Spark: static, reusable, identity-bound address. No
                    // amount/invoice form — mirrors the on-chain UX.
                    Container::new(
                        Column::new()
                            .spacing(8)
                            .push(h4_bold(localized_copy("Spark address")))
                            .push(p2_regular(localized_copy(
                                "Share this reusable address to receive Bitcoin on Spark.",
                            ))),
                    )
                    .padding(16)
                    .style(theme::card::simple)
                    .into()
                };
                // Spark and on-chain addresses need no input: their generated
                // panel says what they are, so no explainer card goes first.
                let auto_address = crate::app::state::spark::receive::auto_generates(self.method);
                if !auto_address && !matches!(self.phase, SparkReceivePhase::Generated(_)) {
                    form = form
                        .push(iced::widget::rule::horizontal(1))
                        .push(Container::new(method_form).padding([18, 24]));
                }

                // ── Phase-specific body ───────────────────────────────────
                form = form.push(iced::widget::rule::horizontal(1)).push(
                    Container::new(phase_body(
                        self.phase,
                        self.qr_data,
                        self.method,
                        self.amount_input,
                        self.description_input,
                    ))
                    .padding([18, 24]),
                );
            }
        }

        let aside = Column::new()
            .spacing(12)
            .push(self.balances.view())
            .push(iced::widget::rule::horizontal(1))
            .push(
                button::transparent_border(None, localized_copy("See all in Activity")).on_press(
                    Message::Menu(crate::app::menu::Menu::Spark(
                        crate::app::menu::SparkSubMenu::Transactions(None),
                    )),
                ),
            );
        let mut content = Column::new()
            .spacing(20)
            .width(Length::Fill)
            // The title, the Cube line and Back to Spark are in the page head.
            .push(
                Row::new()
                    .spacing(40)
                    .align_y(Alignment::Start)
                    .push(
                        Container::new(form)
                            .width(Length::Fixed(680.0))
                            .style(payment_card_style),
                    )
                    .push(Container::new(aside).width(Length::Fixed(240.0)))
                    .wrap()
                    .vertical_spacing(20),
            );

        // ── Phase 4f: pending on-chain deposits card ──────────────────
        // Renders only when there's something to show. The list
        // refreshes automatically on `DepositsChanged` events from
        // the bridge, so the card appears the moment the SDK
        // observes an incoming deposit (no manual refresh needed).
        // A card-level claim error may be about the swap's own deposit, so
        // then nothing is left out and its row (with Retry) stays visible.
        let claim_error = self.claim_error;
        let swap_deposit = self.swap_deposit.filter(|_| claim_error.is_none());
        let is_swap_deposit = move |d: &DepositInfo| {
            swap_deposit.is_some_and(|(txid, vout)| d.txid == txid && d.vout == vout)
        };
        let other_deposits = self
            .pending_deposits
            .iter()
            .filter(|d| !is_swap_deposit(d))
            .count();
        if other_deposits > 0 || self.claim_error.is_some() {
            content = content.push(pending_deposits_card(
                self.pending_deposits,
                swap_deposit,
                self.claiming,
                self.claim_error,
                self.claim_notices,
                self.pending_deposit_confirmations,
                self.network,
                self.mask_amounts,
            ));
        }

        // ── Last transactions ─────────────────────────────────────────
        content = content.push(last_transactions_section(
            self.recent_transactions,
            self.bitcoin_unit,
            self.show_direction_badges,
            self.mask_amounts,
            |idx| Message::SparkReceive(SparkReceiveMessage::SelectTransaction(idx)),
            Message::SparkReceive(SparkReceiveMessage::History),
        ));

        Container::new(Container::new(content).width(Length::Fill).max_width(960.0))
            .width(Length::Fill)
            .into()
    }
}

/// "Payments arrive as bitcoin" (master I4): Spark receives bitcoin only, and
/// USDB appears only after a conversion. Where conversions run (mainnet), the
/// note says how to hold US Dollar instead: swap afterwards, or turn the
/// setting on in Wallet settings (P-Receive).
pub(crate) const ARRIVAL_NOTE: &str = "Payments arrive as bitcoin.";
/// With auto-convert on (US Dollar picked under YOU RECEIVE).
pub(crate) const ARRIVAL_NOTE_CONVERTED: &str =
    "Payments arrive as bitcoin and convert to USDB automatically.";
pub(crate) const ARRIVAL_NOTE_USD: &str =
    "To hold them in US Dollar, pick US Dollar under You receive, or swap to USDB afterwards.";
const WALLET_SETTINGS_ID: &str = "tenshu.spark.receive.wallet-settings";
const SWAP_ID: &str = "tenshu.spark.receive.swap";

fn note_link<'a>(
    id: &'static str,
    label: &'static str,
    menu: crate::app::menu::Menu,
) -> Element<'a, Message> {
    coincube_ui::widget::action::Action::new(
        iced::advanced::widget::Id::new(id),
        p2_regular(label),
        Message::FocusAction,
    )
    .padding([4, 0])
    .style(|theme: &theme::Theme, status| {
        let mut style = theme::button::link(theme, status);
        style.text_color = theme.colors.shell.accent_ink;
        style
    })
    .on_press(Message::Menu(menu))
    .into()
}

fn arrival_note<'a>(network: Network, receive_usd: bool) -> Element<'a, Message> {
    use crate::app::menu::{Menu, SparkSettingsOption, SparkSubMenu};
    // Conversions, and so USDB, exist where Swap does.
    let usd = crate::app::state::spark::swap::swap_available(network);
    if usd && receive_usd {
        return Column::new()
            .spacing(2)
            .push(p2_regular(ARRIVAL_NOTE_CONVERTED).style(theme::text::secondary))
            .push(note_link(
                WALLET_SETTINGS_ID,
                "Wallet settings",
                Menu::Spark(SparkSubMenu::Settings(Some(SparkSettingsOption::General))),
            ))
            .into();
    }
    Column::new()
        .spacing(2)
        .push(
            p2_regular(if usd {
                format!("{} {}", ARRIVAL_NOTE, ARRIVAL_NOTE_USD)
            } else {
                ARRIVAL_NOTE.to_owned()
            })
            .style(theme::text::secondary),
        )
        .push_maybe(usd.then(|| {
            Row::new()
                .spacing(16)
                .push(note_link(
                    SWAP_ID,
                    "Swap to USDB",
                    Menu::Spark(SparkSubMenu::Swap),
                ))
                .push(note_link(
                    WALLET_SETTINGS_ID,
                    "Wallet settings",
                    Menu::Spark(SparkSubMenu::Settings(Some(SparkSettingsOption::General))),
                ))
        }))
        .into()
}

fn lightning_address_settings() -> Message {
    Message::Menu(crate::app::menu::Menu::Spark(
        crate::app::menu::SparkSubMenu::Settings(Some(
            crate::app::menu::SparkSettingsOption::LightningAddress,
        )),
    ))
}

fn lightning_address_body<'a>(
    signed_in: bool,
    address: Option<&'a str>,
    qr: Option<&'a qr_code::Data>,
) -> Element<'a, Message> {
    if !signed_in {
        return Column::new()
            .spacing(8)
            .push(crate::app::view::connect::sign_in_prompt::sign_in_prompt(
                "use your Lightning address",
            ))
            .push(p2_regular(localized_copy("Other methods work signed out.")))
            .into();
    }
    let Some(address) = address else {
        return Column::new()
            .spacing(12)
            .push(h4_bold(localized_copy("Claim your Lightning address")))
            .push(p2_regular(localized_copy(
                "Receive Bitcoin with a readable address.",
            )))
            .push(
                button::primary(None, localized_copy("Claim in Settings"))
                    .on_press(lightning_address_settings()),
            )
            .into();
    };
    let copy = Message::SparkReceive(SparkReceiveMessage::CopyPaymentRequest(address.to_owned()));
    let details = Column::new()
        .spacing(10)
        .width(Length::Fill)
        .push(p1_bold(localized_copy("Lightning address")))
        .push(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(text_input("", address).padding(10).width(Length::Fill))
                .push(
                    button::primary(None, localized_copy("Copy"))
                        .on_press(copy)
                        .width(Length::Fixed(80.0)),
                ),
        )
        .push(p2_regular(localized_copy(
            "Anyone with a Lightning wallet can pay this address, even while Tenshu is closed.",
        )))
        .push(
            button::transparent_border(None, localized_copy("Change in Settings"))
                .on_press(lightning_address_settings()),
        );
    let ready: Element<'a, Message> = if let Some(qr) = qr {
        Row::new()
            .spacing(24)
            .align_y(Alignment::Start)
            .width(Length::Fill)
            .push(QRCode::<theme::Theme>::new(qr).total_size(160))
            .push(details)
            .into()
    } else {
        Column::new()
            .spacing(10)
            .push(p2_regular(localized_copy(
                "QR unavailable · copy the address instead.",
            )))
            .push(details)
            .into()
    };
    ready
}

fn payment_card_style(theme: &theme::Theme) -> container::Style {
    let shell = &theme.colors.shell;
    container::Style {
        background: Some(Background::Color(shell.panel)),
        border: iced::Border {
            color: shell.line,
            width: 1.0,
            radius: 16.0.into(),
        },
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn pending_deposits_card<'a>(
    deposits: &'a [DepositInfo],
    hidden: Option<(&'a str, u32)>,
    claiming: &'a HashSet<(String, u32)>,
    claim_error: Option<&'a str>,
    claim_notices: &'a HashMap<(String, u32), String>,
    confirmations: &'a HashMap<(String, u32), u32>,
    network: Network,
    mask_amounts: bool,
) -> Element<'a, Message> {
    let mut card = Column::new()
        .spacing(12)
        .push(h4_bold(localized_copy("Pending deposits")))
        .push(p2_regular(localized_format!(
            "Spark notices incoming on-chain transactions automatically. \
             Each deposit becomes claimable after {} on-chain confirmations, \
             at which point the SDK claims it into your wallet.", "Spark nota la entrada de transacciones en cadena automáticamente. Cada depósito se hace reclamable después de las confirmaciones de {} en cadena, en cuyo punto el SDK lo reclama en su cartera.",
            DEPOSIT_MATURITY_CONFIRMATIONS,
        )));

    if let Some(err) = claim_error {
        card = card.push(p2_regular(localized_format!(
            "Claim failed: {}",
            "Fallo de la reclamación: {}",
            err
        )));
    }
    for deposit in deposits
        .iter()
        .filter(|d| !hidden.is_some_and(|(txid, vout)| d.txid == txid && d.vout == vout))
    {
        let confs = confirmations
            .get(&(deposit.txid.clone(), deposit.vout))
            .copied();
        let notice = claim_notices
            .get(&(deposit.txid.clone(), deposit.vout))
            .map(String::as_str);
        card = card.push(deposit_row(
            deposit,
            claiming,
            notice,
            confs,
            network,
            mask_amounts,
        ));
    }

    Container::new(card)
        .padding(16)
        .style(theme::card::simple)
        .into()
}

fn deposit_row<'a>(
    deposit: &'a DepositInfo,
    claiming: &'a HashSet<(String, u32)>,
    claim_notice: Option<&'a str>,
    confirmations: Option<u32>,
    network: Network,
    mask_amounts: bool,
) -> Element<'a, Message> {
    let is_being_claimed = claiming.contains(&(deposit.txid.clone(), deposit.vout));

    let amount_label = if mask_amounts {
        "•••••• sats".to_string()
    } else {
        format!("{} sats", deposit.amount_sat)
    };
    let txid_short = if deposit.txid.len() > 16 {
        format!(
            "{}…{}",
            &deposit.txid[..8],
            &deposit.txid[deposit.txid.len() - 8..]
        )
    } else {
        deposit.txid.clone()
    };
    let txid_label = format!("{}:{}", txid_short, deposit.vout);

    // Right-side action: either a Claim button (mature, idle), a
    // disabled "Claiming…" button (in-flight), or a status text
    // (immature / errored).
    let action: Element<'_, Message> = if is_being_claimed {
        // Disabled button — `on_press_maybe(None)` keeps the visual
        // weight without firing on click.
        button::primary(None, localized_copy("Claiming…"))
            .on_press_maybe(None)
            .width(Length::Fixed(140.0))
            .into()
    } else if !deposit.is_mature {
        // Immature — show "X / N confirmations" if Esplora has
        // answered, otherwise the SDK-driven fallback wording. The
        // fallback also applies on networks where no public Esplora
        // is available (regtest).
        let label = match confirmations {
            Some(c) if network == Network::Bitcoin => {
                format!("{} / {} confirmations", c, DEPOSIT_MATURITY_CONFIRMATIONS)
            }
            _ => localized_format!(
                "Waiting for confirmations ({} required)",
                "Esperando las confirmaciones ({} requerida)",
                DEPOSIT_MATURITY_CONFIRMATIONS
            ),
        };
        p2_regular(label).into()
    } else if let Some(err) = &deposit.claim_error {
        // Previous claim attempt failed for a reason the SDK
        // surfaces. Show a short hint + Retry button.
        Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .push(p2_regular(format!("Error: {}", short_error(err))))
            .push(
                button::secondary(None, CopyText::Retry.get())
                    .on_press(Message::SparkReceive(
                        crate::app::view::SparkReceiveMessage::ClaimDepositRequested {
                            txid: deposit.txid.clone(),
                            vout: deposit.vout,
                        },
                    ))
                    .width(Length::Fixed(100.0)),
            )
            .into()
    } else {
        button::primary(None, localized_copy("Claim"))
            .on_press(Message::SparkReceive(
                crate::app::view::SparkReceiveMessage::ClaimDepositRequested {
                    txid: deposit.txid.clone(),
                    vout: deposit.vout,
                },
            ))
            .width(Length::Fixed(140.0))
            .into()
    };

    Container::new(
        Row::new()
            .spacing(12)
            .align_y(Alignment::Center)
            .push(
                Column::new()
                    .spacing(4)
                    .width(Length::Fill)
                    .push(p1_regular(amount_label))
                    .push(p2_regular(txid_label))
                    .push_maybe(claim_notice.map(p2_regular)),
            )
            .push(action),
    )
    .padding(10)
    .style(theme::card::simple)
    .into()
}

/// Truncate a long SDK error string for inline display. The full
/// error stays in the panel state; this just keeps the row from
/// blowing up vertically.
fn short_error(err: &str) -> String {
    const MAX_CHARS: usize = 60;
    if err.chars().count() <= MAX_CHARS {
        err.to_string()
    } else {
        format!("{}…", err.chars().take(MAX_CHARS).collect::<String>())
    }
}

// ── Two-card "YOU RECEIVE ← THEY SEND" selector + picker modal ──────────────

/// Badge slug (for the composited logo) + where the bitcoin travels, as it
/// reads after "Bitcoin" on the THEY SEND tile ("Bitcoin on Lightning").
fn rail_display(method: SparkReceiveMethod) -> (&'static str, &'static str) {
    match method {
        SparkReceiveMethod::Bolt11 => ("lightning", "on Lightning"),
        SparkReceiveMethod::LightningAddress => ("lightning", "on Lightning"),
        SparkReceiveMethod::OnchainBitcoin => ("bitcoin", "on-chain"),
        SparkReceiveMethod::Spark => ("spark", "on Spark"),
    }
}

/// Hover-highlight style shared by the tappable selector cards.
fn card_button_style(
    _: &theme::Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    iced::widget::button::Style {
        background: Some(Background::Color(color::TRANSPARENT)),
        border: iced::Border {
            color: if matches!(status, iced::widget::button::Status::Hovered) {
                color::ORANGE
            } else {
                color::TRANSPARENT
            },
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

/// The "THEY SEND → YOU RECEIVE (Bitcoin)" pair. YOU RECEIVE is fixed —
/// Spark only ever receives bitcoin — so only THEY SEND is tappable: it opens
/// the picker for what the payer sends, Bitcoin rail or stablecoin, as THEY
/// RECEIVE does on Send. A chevron and a hover outline say it's a choice.
fn spark_receive_cards<'a>(
    method: SparkReceiveMethod,
    cross: Option<DepositOption>,
    receive_usd: bool,
    network: Network,
) -> Element<'a, Message> {
    // Where conversions run, YOU RECEIVE is a choice too: keep bitcoin, or
    // convert it to US Dollar as it arrives (auto-convert).
    let choosable = crate::app::state::spark::swap::swap_available(network);
    let receive_usd = receive_usd && choosable;
    let you_receive_body = Container::new(super::route_tile_body(
        if receive_usd {
            asset_network_logo("usdb", "spark", 36.0)
        } else {
            asset_network_logo("btc", "spark", 36.0)
        },
        "You receive",
        None,
        if receive_usd {
            localized_copy("US Dollar").to_string()
        } else {
            "Bitcoin".to_string()
        },
        localized_copy("in Spark").to_string(),
        choosable,
    ))
    .width(Length::Fill)
    .style(route_card_style);
    let you_receive: Element<'a, Message> = if choosable {
        coincube_ui::widget::action::Action::new(
            iced::advanced::widget::Id::new(RECEIVER_PICKER_ACTION_ID),
            you_receive_body,
            Message::FocusAction,
        )
        .padding(0)
        .width(Length::Fill)
        .focus_radius(8.0)
        .style(card_button_style)
        .on_press(Message::SparkReceive(
            SparkReceiveMessage::OpenReceiverPicker,
        ))
        .into()
    } else {
        you_receive_body.into()
    };

    // THEY SEND reflects the current pick: a Bitcoin rail, or a cross-network
    // asset while the SideShift flow is active.
    let they_send_body = match cross {
        Some(option) => super::route_tile_body(
            cross_network_logo(&option, 36.0),
            "They send",
            None,
            asset_display_name(option.coin).to_string(),
            localized_format!("on {}", "en {}", option.network.network_name()),
            true,
        ),
        None => {
            let (net_slug, place) = rail_display(method);
            super::route_tile_body(
                asset_network_logo("btc", net_slug, 36.0),
                "They send",
                None,
                "Bitcoin".to_string(),
                localized_copy(place).to_string(),
                true,
            )
        }
    };
    let they_send = coincube_ui::widget::action::Action::new(
        iced::advanced::widget::Id::new(SENDER_PICKER_ACTION_ID),
        Container::new(they_send_body)
            .width(Length::Fill)
            .style(route_card_style),
        Message::FocusAction,
    )
    .padding(0)
    .width(Length::Fill)
    .focus_radius(8.0)
    .style(card_button_style)
    .on_press(Message::SparkReceive(SparkReceiveMessage::OpenSenderPicker));

    Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .push(Container::new(they_send).width(Length::FillPortion(1)))
        .push(text("→").size(P1_SIZE + 2).style(theme::text::secondary))
        .push(Container::new(you_receive).width(Length::FillPortion(1)))
        .into()
}

/// Focus target for the THEY SEND tile.
pub(crate) const SENDER_PICKER_ACTION_ID: &str = "spark-receive-they-send";
/// Focus target for the YOU RECEIVE tile.
pub(crate) const RECEIVER_PICKER_ACTION_ID: &str = "spark-receive-you-receive";
const RECEIVE_AS_BITCOIN_ID: &str = "spark-receive-as-bitcoin";
const SENDER_PICKER_CLOSE_ID: &str = "spark-receive-they-send-close";
const RECEIVER_PICKER_CLOSE_ID: &str = "spark-receive-you-receive-close";

/// An explicit way out of a picker for keyboard users; a click outside the
/// card still closes it through the modal's blur message. Mirrors Send's.
fn picker_close<'a>(id: &'static str, close: SparkReceiveMessage) -> Element<'a, Message> {
    Row::new()
        .push(Space::new().width(Length::Fill))
        .push(
            coincube_ui::widget::action::Action::new(
                iced::advanced::widget::Id::new(id),
                text(CopyText::Close.get()).size(P2_SIZE).bold(),
                Message::FocusAction,
            )
            .style(theme::button::transparent_border)
            .padding([8, 18])
            .on_press(Message::SparkReceive(close)),
        )
        .into()
}
const RECEIVE_AS_USD_ID: &str = "spark-receive-as-us-dollar";

/// The "YOU RECEIVE" picker: keep what arrives as bitcoin, or convert it to
/// US Dollar (USDB) as it lands. It is the wallet's auto-convert setting, so
/// the note says it applies to every payment, not just this one.
pub fn receiver_picker_modal<'a>(receive_usd: bool) -> Element<'a, Message> {
    let list = Column::new()
        .spacing(8)
        .push(
            text(localized_copy("Into your Spark wallet"))
                .size(P2_SIZE)
                .style(theme::text::secondary),
        )
        .push(receive_as_row(
            RECEIVE_AS_BITCOIN_ID,
            asset_network_logo("btc", "spark", 36.0),
            "Bitcoin",
            "SPARK",
            localized_copy("Kept as bitcoin. Your USDB stays USDB."),
            !receive_usd,
            false,
        ))
        .push(receive_as_row(
            RECEIVE_AS_USD_ID,
            asset_network_logo("usdb", "spark", 36.0),
            localized_copy("US Dollar"),
            "USDB",
            localized_copy("Arrives as bitcoin, then converts to USDB."),
            receive_usd,
            true,
        ))
        .push(
            p2_regular(localized_copy(
                "This is auto-convert, so it applies to every payment you receive. \
                 You can also change it in Wallet settings.",
            ))
            .style(theme::text::secondary),
        );
    picker_modal_card(
        coincube_ui::locale::copy("YOU RECEIVE"),
        list.push(picker_close(
            RECEIVER_PICKER_CLOSE_ID,
            SparkReceiveMessage::CloseReceiverPicker,
        )),
    )
}

#[allow(clippy::too_many_arguments)]
fn receive_as_row<'a>(
    id: &'static str,
    logo: Element<'a, Message>,
    label: &'a str,
    chip: &'a str,
    sub: &'a str,
    selected: bool,
    usd: bool,
) -> Element<'a, Message> {
    let row = Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .push(logo)
        .push(
            Column::new()
                .spacing(2)
                .width(Length::Fill)
                .push(
                    Row::new()
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(text(label).size(P1_SIZE).bold().style(theme::text::primary))
                        .push(
                            Container::new(text(chip).size(10).color(color::ORANGE))
                                .padding([2, 6])
                                .style(|_: &theme::Theme| container::Style {
                                    border: iced::Border {
                                        color: color::ORANGE,
                                        width: 1.0,
                                        radius: 6.0.into(),
                                    },
                                    ..Default::default()
                                }),
                        ),
                )
                .push(text(sub).size(P2_SIZE).style(theme::text::secondary)),
        )
        .push(if selected {
            Element::from(icon::check2_icon().size(18).color(color::ORANGE))
        } else {
            Space::new().width(Length::Fixed(18.0)).into()
        });
    coincube_ui::widget::action::Action::new(
        iced::advanced::widget::Id::new(id),
        Container::new(row)
            .padding([12, 16])
            .width(Length::Fill)
            .style(if selected {
                picker_row_selected
            } else {
                crate::app::view::shared::picker::picker_row_idle
            }),
        Message::FocusAction,
    )
    .padding(0)
    .width(Length::Fill)
    .focus_radius(8.0)
    .style(|_: &theme::Theme, _| iced::widget::button::Style {
        background: Some(Background::Color(color::TRANSPARENT)),
        border: iced::Border {
            radius: 8.0.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .on_press(Message::SparkReceive(SparkReceiveMessage::SelectReceiveAs(
        usd,
    )))
    .into()
}

/// Focus target for a Bitcoin rail's row in the THEY SEND picker.
fn rail_picker_id(method: SparkReceiveMethod) -> &'static str {
    match method {
        SparkReceiveMethod::Bolt11 => "spark-receive-method-lightning",
        SparkReceiveMethod::LightningAddress => "spark-receive-method-lightning-address",
        SparkReceiveMethod::Spark => "spark-receive-method-spark",
        SparkReceiveMethod::OnchainBitcoin => "spark-receive-method-deposit",
    }
}

/// A Bitcoin rail as a THEY SEND picker row: badge slug and the rail's name.
fn rail_picker_label(method: SparkReceiveMethod) -> (&'static str, &'static str) {
    match method {
        SparkReceiveMethod::Bolt11 => ("lightning", "Lightning invoice"),
        SparkReceiveMethod::LightningAddress => ("lightning", "Lightning address"),
        SparkReceiveMethod::Spark => ("spark", "Spark address"),
        SparkReceiveMethod::OnchainBitcoin => ("bitcoin", "On-chain address"),
    }
}

fn route_card_style(theme: &theme::Theme) -> container::Style {
    let shell = &theme.colors.shell;
    container::Style {
        background: Some(Background::Color(shell.background)),
        border: iced::Border {
            color: shell.line,
            width: 1.0,
            // The THEY SEND tile is a button, so the pair takes the square
            // 8px corner (canvas v122), matching its hover outline.
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

/// Logo for a cross-network deposit option. Only USDt has a correct coin SVG;
/// every other asset falls back to the plain network logo so we never show a
/// *wrong* coin (the row label carries the asset name). Mirrors
/// `sideshift_receive::deposit_option_logo`.
fn cross_network_logo<'a>(option: &DepositOption, size: f32) -> Element<'a, Message> {
    match option.coin {
        "usdt" => usdt_network_logo(option.network.network_slug(), size),
        "usdc" => asset_network_logo("usdc", option.network.network_slug(), size),
        _ => network_logo(option.network.network_slug())
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .into(),
    }
}

/// Human asset name from a SideShift coin slug.
fn asset_display_name(coin: &str) -> &str {
    match coin {
        "usdt" => "USDt",
        "usdc" => "USDC",
        "eth" => "Ether",
        "sol" => "Solana",
        "trx" => "Tron",
        other => other,
    }
}

/// One selectable row in the THEY SEND modal. Adapted from Liquid's
/// `receive_picker_row`.
fn spark_picker_row<'a>(
    id: Option<&'static str>,
    ico: impl Into<Element<'a, Message>>,
    label: &str,
    network: &str,
    is_selected: bool,
    on_press: Message,
) -> Element<'a, Message> {
    let mut row = Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .push(ico)
        .push(
            text(label.to_string())
                .size(14)
                .bold()
                .style(theme::text::primary),
        )
        .push(
            Container::new(text(network.to_uppercase()).size(10).color(color::ORANGE))
                .padding([2, 6])
                .style(|_: &theme::Theme| container::Style {
                    border: iced::Border {
                        color: color::ORANGE,
                        width: 1.0,
                        radius: 6.0.into(),
                    },
                    ..Default::default()
                }),
        )
        .push(Space::new().width(Length::Fill));

    if is_selected {
        row = row.push(icon::check2_icon().size(18).color(color::ORANGE));
    }

    let body = Container::new(row)
        .padding([12, 16])
        .width(Length::Fill)
        .style(if is_selected {
            picker_row_selected
        } else {
            crate::app::view::shared::picker::picker_row_idle
        });
    if let Some(id) = id {
        // A focusable Action, so the picker works from the keyboard.
        return coincube_ui::widget::action::Action::new(
            iced::advanced::widget::Id::new(id),
            body,
            Message::FocusAction,
        )
        .padding(0)
        .width(Length::Fill)
        .focus_radius(8.0)
        .style(|_: &theme::Theme, _| iced::widget::button::Style {
            background: Some(Background::Color(color::TRANSPARENT)),
            border: iced::Border {
                radius: 8.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .on_press(on_press)
        .into();
    }
    iced_button(body)
        .padding(0)
        .on_press(on_press)
        .style(|_: &theme::Theme, _| iced::widget::button::Style {
            background: Some(Background::Color(color::TRANSPARENT)),
            border: iced::Border {
                radius: 8.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .width(Length::Fill)
        .into()
}

/// Selected-row style — orange border + subtle tint.
fn picker_row_selected(theme: &theme::Theme) -> container::Style {
    let bg = match theme.mode {
        coincube_ui::theme::palette::ThemeMode::Dark => iced::color!(0x1a1a10),
        coincube_ui::theme::palette::ThemeMode::Light => iced::color!(0xFFF5E6),
    };
    container::Style {
        background: Some(Background::Color(bg)),
        border: iced::Border {
            color: color::ORANGE,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

/// The Stablecoin rail picker. Options come from the validated SideShift
/// deposit list, not from the prototype's synthetic network fixtures.
///
/// `cross` is the cross-network asset a running SideShift flow is set to. While
/// one is up it — not the Bitcoin rail underneath — is what the panel is
/// actually set to receive, so it carries the checkmark.
pub fn sender_picker_modal<'a>(
    current: SparkReceiveMethod,
    cross: Option<DepositOption>,
    network: Network,
) -> Element<'a, Message> {
    let is_mainnet = supported_on(network);

    let mut list = Column::new()
        .spacing(8)
        .push(text("Bitcoin").size(P2_SIZE).style(theme::text::secondary));
    for method in [
        SparkReceiveMethod::Bolt11,
        SparkReceiveMethod::LightningAddress,
        SparkReceiveMethod::Spark,
        SparkReceiveMethod::OnchainBitcoin,
    ] {
        let (slug, rail) = rail_picker_label(method);
        list = list.push(spark_picker_row(
            Some(rail_picker_id(method)),
            asset_network_logo("btc", slug, 36.0),
            "Bitcoin",
            localized_copy(rail),
            cross.is_none() && current == method,
            Message::SparkReceive(SparkReceiveMessage::SelectSenderRail(method)),
        ));
    }

    list = list.push(Space::new().height(Length::Fixed(6.0))).push(
        text(localized_copy("Stablecoin (cross-chain)"))
            .size(P2_SIZE)
            .style(theme::text::secondary),
    );
    if is_mainnet {
        for option in deposit_options() {
            list = list.push(spark_picker_row(
                None,
                cross_network_logo(option, 36.0),
                asset_display_name(option.coin),
                option.network.network_name(),
                cross == Some(*option),
                Message::SparkReceive(SparkReceiveMessage::SelectSenderCrossNetwork(option.key())),
            ));
        }
    } else {
        list = list.push(
            p2_regular(localized_copy(
                "Stablecoin deposits require Bitcoin mainnet.",
            ))
            .style(theme::text::secondary),
        );
    }

    // On mainnet the cross-network assets push the list past a screenful, so
    // cap + scroll. The right padding is the scrollbar's gutter — without it
    // the scroller draws on top of the rows' right edge.
    let body: Element<'a, Message> = if is_mainnet {
        scrollable(list.padding(iced::Padding {
            right: 10.0,
            ..iced::Padding::ZERO
        }))
        .height(Length::Fixed(480.0))
        .into()
    } else {
        list.into()
    };

    let body = Column::new().spacing(8).push(body).push(picker_close(
        SENDER_PICKER_CLOSE_ID,
        SparkReceiveMessage::CloseSenderPicker,
    ));
    picker_modal_card(coincube_ui::locale::copy("THEY SEND"), body)
}

fn phase_body<'a>(
    phase: &'a SparkReceivePhase,
    qr_data: Option<&'a qr_code::Data>,
    method: SparkReceiveMethod,
    amount_input: &'a str,
    description_input: &'a str,
) -> Element<'a, Message> {
    use crate::app::view::SparkReceiveMessage;

    match phase {
        // Spark and on-chain addresses are fetched as soon as the method is
        // picked; only an invoice, which takes an amount and description,
        // waits for a button.
        SparkReceivePhase::Idle | SparkReceivePhase::Generating
            if crate::app::state::spark::receive::auto_generates(method) =>
        {
            Container::new(p1_regular(localized_copy("Getting your address…")))
                .padding(16)
                .into()
        }

        SparkReceivePhase::Idle => Container::new(
            Column::new().spacing(10).push(
                button::primary(None, localized_copy("Create invoice"))
                    .on_press(Message::SparkReceive(
                        SparkReceiveMessage::GenerateRequested,
                    ))
                    .width(Length::Fixed(160.0)),
            ),
        )
        .padding(16)
        .style(theme::card::simple)
        .into(),

        SparkReceivePhase::Generating => Container::new(
            Column::new()
                .spacing(10)
                .push(p1_regular(localized_copy("Creating your invoice…"))),
        )
        .padding(16)
        .style(theme::card::simple)
        .into(),

        SparkReceivePhase::Generated(ok) => {
            // QR code block — rendered only if we successfully
            // encoded the payment request in state. Falls back to a
            // plain "QR unavailable" note if encoding failed; for
            // typical BOLT11 / BTC address payloads this branch
            // should never fire in practice.
            let qr_block: Element<'_, Message> = if let Some(qr) = qr_data {
                Container::new(QRCode::<coincube_ui::theme::Theme>::new(qr).total_size(160))
                    .align_x(iced::alignment::Horizontal::Center)
                    .into()
            } else {
                p2_regular(localized_copy("QR code unavailable for this payload.")).into()
            };

            let payment_request = ok.payment_request.clone();
            let (label, guidance) = match method {
                SparkReceiveMethod::Bolt11 => (
                    localized_copy("Lightning invoice"),
                    localized_copy("Share this invoice to receive sats via Lightning."),
                ),
                SparkReceiveMethod::LightningAddress => (
                    localized_copy("Lightning address"),
                    localized_copy("Anyone with a Lightning wallet can pay this address, even while Tenshu is closed."),
                ),
                SparkReceiveMethod::Spark => (
                    localized_copy("Spark address"),
                    localized_copy("Share this address to receive Bitcoin from another Spark wallet."),
                ),
                SparkReceiveMethod::OnchainBitcoin => (
                    localized_copy("Bitcoin deposit address"),
                    localized_copy("Deposits arrive in Spark after on-chain confirmation."),
                ),
            };
            let mut details = Column::new()
                .spacing(10)
                .width(Length::Fill)
                .push(p1_bold(label))
                .push(
                    Row::new()
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(
                            text_input("", &ok.payment_request)
                                .padding(10)
                                .width(Length::Fill),
                        )
                        .push(
                            button::primary(None, localized_copy("Copy"))
                                .on_press(Message::SparkReceive(
                                    SparkReceiveMessage::CopyPaymentRequest(payment_request),
                                ))
                                .width(Length::Fixed(80.0)),
                        ),
                )
                .push(p2_regular(guidance).style(theme::text::secondary));
            if method == SparkReceiveMethod::Bolt11 {
                details = details.push(kv_row(
                    localized_copy("Amount"),
                    if amount_input.trim().is_empty() {
                        "Any amount".to_owned()
                    } else {
                        format!("{} sats", amount_input.trim())
                    },
                ));
                if !description_input.is_empty() {
                    details = details.push(kv_row(
                        localized_copy("Description"),
                        description_input.to_owned(),
                    ));
                }
            }
            if ok.fee_sat > 0 {
                details = details.push(kv_row(
                    localized_copy("Fee"),
                    format!("{} sats", ok.fee_sat),
                ));
            }
            let ready_body: Element<'_, Message> = if qr_data.is_some() {
                Row::new()
                    .spacing(24)
                    .align_y(Alignment::Start)
                    .width(Length::Fill)
                    .push(qr_block)
                    .push(details)
                    .into()
            } else {
                Column::new()
                    .spacing(10)
                    .push(qr_block)
                    .push(details)
                    .into()
            };
            // An invoice is single-use, so offer another. The on-chain
            // address is reused for every payer; a fresh one keeps payers from
            // seeing each other's deposits. A Spark address has neither.
            let follow_up = match method {
                SparkReceiveMethod::Bolt11 => Some(
                    button::transparent_border(None, localized_copy("Create another invoice"))
                        .on_press(Message::SparkReceive(SparkReceiveMessage::Reset)),
                ),
                SparkReceiveMethod::OnchainBitcoin => Some(
                    button::transparent_border(None, localized_copy("Use a new address")).on_press(
                        Message::SparkReceive(SparkReceiveMessage::NewOnchainAddress),
                    ),
                ),
                SparkReceiveMethod::Spark | SparkReceiveMethod::LightningAddress => None,
            };
            Container::new(
                Column::new()
                    .spacing(16)
                    .push(ready_body)
                    .push_maybe(follow_up),
            )
            .padding(16)
            .style(theme::card::simple)
            .into()
        }

        SparkReceivePhase::Received { .. } => {
            // Handled by the full-screen celebration in render()
            Container::new(Column::new()).into()
        }

        SparkReceivePhase::Error(err) => Container::new(
            Column::new()
                .spacing(10)
                .push(h4_bold(localized_copy("Error")))
                .push(p1_regular(coincube_ui::locale::notice(err.clone())))
                .push(Space::new().height(Length::Fixed(8.0)))
                .push(
                    button::primary(None, localized_copy("Try again"))
                        .on_press(Message::SparkReceive(SparkReceiveMessage::Reset))
                        .width(Length::Fixed(140.0)),
                ),
        )
        .padding(16)
        .style(theme::card::simple)
        .into(),
    }
}

fn kv_row<'a>(label: &'a str, value: String) -> Element<'a, Message> {
    Row::new()
        .spacing(20)
        .push(
            Column::new()
                .width(Length::FillPortion(1))
                .push(h4_bold(label)),
        )
        .push(
            Column::new()
                .width(Length::FillPortion(3))
                .push(p1_regular(value)),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::cache::Cache;
    use crate::app::state::preferences::{BalancePrivacy, BalancePrivacyChrome};
    use crate::app::view::shared::landing::capture;

    #[test]
    fn the_they_send_picker_offers_bitcoin_rails_then_mainnet_stablecoins() {
        let mainnet = capture::inspect(
            "spark-receive-picker-mainnet",
            sender_picker_modal(SparkReceiveMethod::Bolt11, None, Network::Bitcoin),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        for label in [
            "THEY SEND",
            "LIGHTNING INVOICE",
            "LIGHTNING ADDRESS",
            "SPARK ADDRESS",
            "ON-CHAIN ADDRESS",
            "Stablecoin (cross-chain)",
            "USDt",
        ] {
            assert!(mainnet.contains(label), "mainnet picker missing {}", label);
        }

        let regtest = capture::inspect(
            "spark-receive-picker-regtest",
            sender_picker_modal(SparkReceiveMethod::Bolt11, None, Network::Regtest),
            (1440, 900),
            "light",
            theme::Theme::light(),
        );
        assert!(regtest.contains("ON-CHAIN ADDRESS"));
        assert!(regtest.contains("Stablecoin deposits require Bitcoin mainnet."));
        assert!(!regtest.contains("USDt"));
    }

    /// Master I4: Receive never implies native stablecoin receive. Off
    /// mainnet, where no conversion runs, the note stops at bitcoin.
    #[test]
    fn the_arrival_note_says_payments_arrive_as_bitcoin() {
        let mainnet = capture::inspect(
            "spark-receive-note-mainnet",
            arrival_note(Network::Bitcoin, false),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(mainnet.contains(ARRIVAL_NOTE) && mainnet.contains(ARRIVAL_NOTE_USD));
        let messages = capture::activate(
            arrival_note(Network::Bitcoin, false),
            iced::advanced::widget::Id::new(WALLET_SETTINGS_ID),
            (1024, 768),
            true,
        );
        assert!(messages.iter().any(|message| matches!(
            message,
            Message::Menu(crate::app::menu::Menu::Spark(
                crate::app::menu::SparkSubMenu::Settings(Some(
                    crate::app::menu::SparkSettingsOption::General
                ))
            ))
        )));
        let messages = capture::activate(
            arrival_note(Network::Bitcoin, false),
            iced::advanced::widget::Id::new(SWAP_ID),
            (1024, 768),
            true,
        );
        assert!(messages.iter().any(|message| matches!(
            message,
            Message::Menu(crate::app::menu::Menu::Spark(
                crate::app::menu::SparkSubMenu::Swap
            ))
        )));
        for network in [Network::Testnet, Network::Signet, Network::Regtest] {
            let note = capture::inspect(
                "spark-receive-note-test-network",
                arrival_note(network, false),
                (1024, 768),
                "dark",
                theme::Theme::dark(),
            );
            assert!(note.contains(ARRIVAL_NOTE));
            assert!(
                !note.contains("US Dollar")
                    && !note.contains("Wallet settings")
                    && !note.contains("Swap to USDB")
            );
        }
    }

    #[test]
    fn the_they_send_tile_opens_the_picker_for_rails_and_stablecoins() {
        let usdt = deposit_options()[0];
        for cross in [None, Some(usdt)] {
            for pointer in [false, true] {
                let messages = capture::activate(
                    spark_receive_cards(
                        SparkReceiveMethod::OnchainBitcoin,
                        cross,
                        false,
                        Network::Bitcoin,
                    ),
                    iced::advanced::widget::Id::new(SENDER_PICKER_ACTION_ID),
                    (1024, 768),
                    pointer,
                );
                assert!(messages.iter().any(|message| matches!(
                    message,
                    Message::SparkReceive(SparkReceiveMessage::OpenSenderPicker)
                )));
            }
        }
    }

    #[test]
    fn lightning_address_method_selects_inline_receive_state() {
        for pointer in [false, true] {
            let messages = capture::activate(
                sender_picker_modal(SparkReceiveMethod::Bolt11, None, Network::Bitcoin),
                iced::advanced::widget::Id::new("spark-receive-method-lightning-address"),
                (1024, 768),
                pointer,
            );
            assert!(messages.iter().any(|message| matches!(
                message,
                Message::SparkReceive(SparkReceiveMessage::SelectSenderRail(
                    SparkReceiveMethod::LightningAddress
                ))
            )));
        }
    }

    #[test]
    fn lightning_address_ready_and_signed_out_views_are_account_gated() {
        let address = "synthetic@coincube.io";
        let qr = qr_code::Data::new(address).expect("synthetic address fits a QR code");
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for outer in [(1024, 768), (1440, 900)] {
                let ready = capture::inspect_wide_content(
                    "spark-receive-lightning-address-ready",
                    lightning_address_body(true, Some(address), Some(&qr)),
                    outer,
                    appearance,
                    theme,
                    outer.0 - 248 - 48,
                );
                for label in ["Lightning address", "Copy", "Change in Settings"] {
                    assert!(
                        ready.contains(label),
                        "{} missing at {} {:?}",
                        label,
                        appearance,
                        outer
                    );
                }
                let ready_label = ready
                    .text
                    .iter()
                    .find(|(label, _)| label == "Lightning address")
                    .map(|(_, bounds)| *bounds)
                    .expect("lightning address ready label");
                assert!(
                    ready_label.x > 180.0,
                    "Lightning address details must sit beside the QR at {} {:?}: {:?}",
                    appearance,
                    outer,
                    ready_label,
                );
                let signed_out = capture::inspect_wide_content(
                    "spark-receive-lightning-address-signed-out",
                    lightning_address_body(false, Some(address), Some(&qr)),
                    outer,
                    appearance,
                    theme,
                    outer.0 - 248 - 48,
                );
                assert!(signed_out.contains("Sign in to Connect"));
                assert!(!signed_out.contains("Change in Settings"));
                let unclaimed = capture::inspect_wide_content(
                    "spark-receive-lightning-address-unclaimed",
                    lightning_address_body(true, None, None),
                    outer,
                    appearance,
                    theme,
                    outer.0 - 248 - 48,
                );
                assert!(unclaimed.contains("Claim in Settings"));
                assert!(!unclaimed.contains("Change in Settings"));
            }
        }
    }

    /// Spark and on-chain addresses show without a button; only an invoice
    /// is created on demand. On-chain offers a fresh address, Spark nothing.
    #[test]
    fn static_addresses_need_no_button_and_on_chain_offers_a_new_one() {
        for method in [
            SparkReceiveMethod::Spark,
            SparkReceiveMethod::OnchainBitcoin,
        ] {
            for phase in [SparkReceivePhase::Idle, SparkReceivePhase::Generating] {
                let mounted = capture::inspect(
                    "spark-receive-auto-address",
                    phase_body(&phase, None, method, "", ""),
                    (1440, 900),
                    "dark",
                    theme::Theme::dark(),
                );
                assert!(mounted.contains("Getting your address…"), "{:?}", method);
                assert!(!mounted.contains("Generate") && !mounted.contains("Create invoice"));
            }
        }
        let idle_invoice = capture::inspect(
            "spark-receive-invoice-idle",
            phase_body(
                &SparkReceivePhase::Idle,
                None,
                SparkReceiveMethod::Bolt11,
                "",
                "",
            ),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(idle_invoice.contains("Create invoice"));

        let address = "bc1qsyntheticstaticdepositaddress0000000000";
        let generated = SparkReceivePhase::Generated(coincube_spark_protocol::ReceivePaymentOk {
            payment_request: address.to_owned(),
            fee_sat: 0,
        });
        let on_chain = capture::inspect(
            "spark-receive-on-chain-ready",
            phase_body(&generated, None, SparkReceiveMethod::OnchainBitcoin, "", ""),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(on_chain.contains("Use a new address"));
        let spark = capture::inspect(
            "spark-receive-spark-ready",
            phase_body(&generated, None, SparkReceiveMethod::Spark, "", ""),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(!spark.contains("Use a new address") && !spark.contains("Create another"));
    }

    /// YOU RECEIVE is a choice where conversions run: its tile opens the
    /// picker, and picking US Dollar asks for auto-convert on.
    #[test]
    fn the_you_receive_tile_and_picker_choose_bitcoin_or_us_dollar() {
        for pointer in [false, true] {
            let messages = capture::activate(
                spark_receive_cards(SparkReceiveMethod::Bolt11, None, false, Network::Bitcoin),
                iced::advanced::widget::Id::new(RECEIVER_PICKER_ACTION_ID),
                (1024, 768),
                pointer,
            );
            assert!(messages.iter().any(|message| matches!(
                message,
                Message::SparkReceive(SparkReceiveMessage::OpenReceiverPicker)
            )));
        }
        // Off mainnet Spark only ever receives bitcoin: nothing to pick.
        let regtest = capture::inspect(
            "spark-receive-cards-regtest",
            spark_receive_cards(SparkReceiveMethod::Bolt11, None, false, Network::Regtest),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(!regtest.has_focus(iced::advanced::widget::Id::new(RECEIVER_PICKER_ACTION_ID)));

        let converted = capture::inspect(
            "spark-receive-cards-usd",
            spark_receive_cards(SparkReceiveMethod::Bolt11, None, true, Network::Bitcoin),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(converted.contains("US Dollar"));

        let picker = capture::inspect(
            "spark-receive-receiver-picker",
            receiver_picker_modal(false),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        for label in [
            "YOU RECEIVE",
            "Bitcoin",
            "US Dollar",
            "Arrives as bitcoin, then converts to USDB.",
        ] {
            assert!(picker.contains(label), "receiver picker missing {}", label);
        }
        for (id, usd) in [(RECEIVE_AS_USD_ID, true), (RECEIVE_AS_BITCOIN_ID, false)] {
            let messages = capture::activate(
                receiver_picker_modal(!usd),
                iced::advanced::widget::Id::new(id),
                (1440, 900),
                false,
            );
            assert!(messages.iter().any(|message| matches!(
                message,
                Message::SparkReceive(SparkReceiveMessage::SelectReceiveAs(picked)) if *picked == usd
            )));
        }
    }

    #[test]
    fn generated_invoice_has_integrated_request_and_real_details() {
        let request = "lnbc1syntheticreceive7q";
        let phase = SparkReceivePhase::Generated(coincube_spark_protocol::ReceivePaymentOk {
            payment_request: request.to_owned(),
            fee_sat: 0,
        });
        let qr = qr_code::Data::new(request).expect("synthetic request fits a QR code");
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for outer in [(1024, 768), (1440, 900)] {
                let mounted = capture::inspect_wide_content(
                    "spark-receive-ready",
                    phase_body(
                        &phase,
                        Some(&qr),
                        SparkReceiveMethod::Bolt11,
                        "2500",
                        "Dinner",
                    ),
                    outer,
                    appearance,
                    theme,
                    outer.0 - 248 - 48,
                );
                for label in [
                    "Lightning invoice",
                    "Copy",
                    "Amount",
                    "2500 sats",
                    "Description",
                    "Dinner",
                    "Create another invoice",
                ] {
                    assert!(
                        mounted.contains(label),
                        "{} missing at {} {:?}",
                        label,
                        appearance,
                        outer
                    );
                }
                assert!(!mounted.contains("Expires"));
                let ready_label = mounted
                    .text
                    .iter()
                    .find(|(label, _)| label == "Lightning invoice")
                    .map(|(_, bounds)| *bounds)
                    .expect("generated ready label");
                assert!(
                    ready_label.x > 180.0,
                    "generated details must sit beside the QR at {} {:?}: {:?}",
                    appearance,
                    outer,
                    ready_label,
                );
            }
        }
    }

    #[test]
    fn receive_payment_hierarchy_and_focus_hold_at_compact_and_wide_sizes() {
        let deposits = Vec::new();
        let confirmations = HashMap::new();
        let claiming = HashSet::new();
        let transactions = Vec::new();
        let quote = coincube_ui::component::quote_display::random_quote("spark-receive");
        let image =
            coincube_ui::component::quote_display::image_handle_for_context("spark-receive");
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for outer in [(1024, 768), (1440, 900)] {
                let mounted = capture::inspect_wide_content(
                    "spark-receive-payment-hierarchy",
                    SparkReceiveView {
                        backend_available: true,
                        method: SparkReceiveMethod::OnchainBitcoin,
                        amount_input: "",
                        description_input: "",
                        phase: &SparkReceivePhase::Idle,
                        qr_data: None,
                        connect_signed_in: false,
                        lightning_address: None,
                        lightning_address_qr: None,
                        pending_deposits: &deposits,
                        claiming: &claiming,
                        claim_error: None,
                        claim_notices: &HashMap::new(),
                        pending_deposit_confirmations: &confirmations,
                        network: Network::Bitcoin,
                        cube_name: "Everyday",
                        received_amount_display: "",
                        received_celebration_context: "spark-receive",
                        received_quote: &quote,
                        received_image_handle: &image,
                        recent_transactions: &transactions,
                        balance_sats: 42_000,
                        balances: crate::app::view::spark::SparkBalances {
                            btc_sats: 42_000,
                            stable: None,
                            fiat_converter: None,
                            btc_usd_price: None,
                            bitcoin_unit: BitcoinDisplayUnit::Sats,
                            masked: false,
                        },
                        bitcoin_unit: BitcoinDisplayUnit::Sats,
                        show_direction_badges: false,
                        mask_amounts: false,
                        sideshift_body: None,
                        cross_network_selected: None,
                        swap_deposit: None,
                        receive_usd: false,
                    }
                    .render(),
                    outer,
                    appearance,
                    theme,
                    outer.0 - 248 - 48,
                );
                // The title, the Cube line and the back link are in the page
                // head now, not the body.
                for moved in ["‹ Spark", "Back to Spark", "Everyday · Bitcoin mainnet"] {
                    assert!(!mounted.contains(moved), "{} left in the body", moved);
                }
                for label in [
                    "Payment details",
                    "Payments arrive as bitcoin.",
                    "Wallet settings",
                    "Available in Spark",
                    "They send",
                    "You receive",
                    "on-chain",
                ] {
                    assert!(
                        mounted.contains(label),
                        "{} missing at {} {:?}",
                        label,
                        appearance,
                        outer
                    );
                }
                // No kind toggle or method row: the THEY SEND tile's picker
                // holds every rail and stablecoin.
                for gone in ["Method", "Bitcoin address · deposit"] {
                    assert!(!mounted.contains(gone), "{} still in the body", gone);
                }
                for id in [
                    "spark-receive-kind-bitcoin",
                    "spark-receive-kind-stablecoin",
                    "spark-receive-method-lightning",
                    "spark-receive-method-deposit",
                ] {
                    assert!(
                        !mounted.has_focus(iced::advanced::widget::Id::new(id)),
                        "{} left in the body",
                        id
                    );
                }
                for id in [WALLET_SETTINGS_ID, SENDER_PICKER_ACTION_ID] {
                    assert!(
                        mounted.has_focus(iced::advanced::widget::Id::new(id)),
                        "{} not focusable",
                        id
                    );
                }
                assert!(!mounted.contains("✓"));
                let x = |label| {
                    mounted
                        .text
                        .iter()
                        .find(|(value, _)| value == label)
                        .unwrap()
                        .1
                        .x
                };
                assert!(x("They send") < x("You receive"));
            }
        }
    }

    #[test]
    fn pending_deposits_render_each_outputs_own_notice() {
        let deposits: Vec<_> = [0, 1]
            .iter()
            .copied()
            .map(|vout| DepositInfo {
                txid: "shared-tx".to_string(),
                vout,
                amount_sat: 5_000,
                is_mature: true,
                claim_error: None,
            })
            .collect();
        let notices = HashMap::from([
            (
                ("shared-tx".to_string(), 0),
                "First output waits".to_string(),
            ),
            (
                ("shared-tx".to_string(), 1),
                "Second output waits".to_string(),
            ),
            (("absent".to_string(), 0), "Absent output waits".to_string()),
        ]);
        let mounted = capture::inspect(
            "spark-pending-deposit-notices",
            pending_deposits_card(
                &deposits,
                None,
                &HashSet::new(),
                None,
                &notices,
                &HashMap::new(),
                Network::Bitcoin,
                false,
            ),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        let y = |label| {
            mounted
                .text
                .iter()
                .find(|(value, _)| value == label)
                .unwrap()
                .1
                .y
        };
        assert!(y("shared-tx:0") < y("First output waits"));
        assert!(y("First output waits") < y("shared-tx:1"));
        assert!(y("shared-tx:1") < y("Second output waits"));
        assert!(!mounted.contains("Absent output waits"));
    }

    #[test]
    fn pending_deposit_amount_follows_passive_privacy_authority() {
        let mut cache = Cache::default();
        let deposits = vec![DepositInfo {
            txid: "privacy-deposit-123456".to_string(),
            vout: 1,
            amount_sat: 76_543_219,
            is_mature: true,
            claim_error: None,
        }];
        let confirmations = HashMap::new();
        let claiming = HashSet::new();
        for privacy in [
            None,
            Some(BalancePrivacy::Unresolved),
            Some(BalancePrivacy::Hidden),
            Some(BalancePrivacy::Shown),
        ] {
            cache.balance_privacy = privacy.map(|privacy| BalancePrivacyChrome {
                privacy,
                busy: false,
                interactive: false,
            });
            let mounted = capture::inspect(
                "spark-pending-deposit-privacy",
                pending_deposits_card(
                    &deposits,
                    None,
                    &claiming,
                    None,
                    &HashMap::new(),
                    &confirmations,
                    Network::Bitcoin,
                    cache.balance_amounts_masked(),
                ),
                (1440, 900),
                "dark",
                theme::Theme::dark(),
            );
            assert!(mounted.contains("privacy-…t-123456:1"));
            assert!(mounted.contains("Claim"));
            if privacy == Some(BalancePrivacy::Shown) {
                assert!(mounted.contains("76543219 sats"));
            } else {
                assert!(mounted.contains("•••••• sats"));
                assert!(!mounted.contains("76543219"));
            }
        }
    }

    #[test]
    fn mounted_received_celebration_masks_until_shown() {
        let mut cache = Cache::default();
        let phase = SparkReceivePhase::Received {
            amount_sat: 87_654_321,
            count: 1,
        };
        let deposits = Vec::new();
        let confirmations = HashMap::new();
        let claiming = HashSet::new();
        let transactions = Vec::new();
        let quote = coincube_ui::component::quote_display::random_quote("spark-receive");
        let image =
            coincube_ui::component::quote_display::image_handle_for_context("spark-receive");
        for privacy in [
            None,
            Some(BalancePrivacy::Unresolved),
            Some(BalancePrivacy::Hidden),
            Some(BalancePrivacy::Shown),
        ] {
            cache.balance_privacy = privacy.map(|privacy| BalancePrivacyChrome {
                privacy,
                busy: false,
                interactive: false,
            });
            let mounted = capture::inspect(
                "spark-received-celebration-privacy",
                SparkReceiveView {
                    backend_available: true,
                    method: SparkReceiveMethod::Spark,
                    amount_input: "",
                    description_input: "",
                    phase: &phase,
                    qr_data: None,
                    connect_signed_in: false,
                    lightning_address: None,
                    lightning_address_qr: None,
                    pending_deposits: &deposits,
                    claiming: &claiming,
                    claim_error: None,
                    claim_notices: &HashMap::new(),
                    pending_deposit_confirmations: &confirmations,
                    network: Network::Bitcoin,
                    cube_name: "Everyday",
                    received_amount_display: "+87,654,321 sats",
                    received_celebration_context: "spark-receive",
                    received_quote: &quote,
                    received_image_handle: &image,
                    recent_transactions: &transactions,
                    balance_sats: 0,
                    balances: crate::app::view::spark::SparkBalances {
                        btc_sats: 0,
                        stable: None,
                        fiat_converter: None,
                        btc_usd_price: None,
                        bitcoin_unit: BitcoinDisplayUnit::Sats,
                        masked: cache.balance_amounts_masked(),
                    },
                    bitcoin_unit: BitcoinDisplayUnit::Sats,
                    show_direction_badges: false,
                    mask_amounts: cache.balance_amounts_masked(),
                    sideshift_body: None,
                    cross_network_selected: None,
                    swap_deposit: None,
                    receive_usd: false,
                }
                .render(),
                (1440, 900),
                "dark",
                theme::Theme::dark(),
            );
            if privacy == Some(BalancePrivacy::Shown) {
                assert!(mounted.contains("+87,654,321 sats"));
            } else {
                assert!(mounted.contains("••••••"));
                assert!(!mounted.contains("87,654,321"));
            }
        }
    }

    #[test]
    fn short_error_leaves_short_and_boundary_length_errors_unchanged() {
        let short = "bridge unavailable";
        assert_eq!(short_error(short), short);

        let boundary = "x".repeat(60);
        assert_eq!(short_error(&boundary), boundary);
    }

    #[test]
    fn short_error_truncates_long_ascii_errors() {
        let err = format!("{}tail", "x".repeat(60));

        assert_eq!(short_error(&err), format!("{}…", "x".repeat(60)));
    }

    #[test]
    fn short_error_truncates_on_char_boundaries() {
        let err = format!("{}⚡tail", "x".repeat(59));

        assert_eq!(short_error(&err), format!("{}⚡…", "x".repeat(59)));
    }

    #[test]
    fn bitcoin_rail_display_labels_are_stable() {
        // The label reads after "Bitcoin" on the THEY SEND tile.
        assert_eq!(
            rail_display(SparkReceiveMethod::Bolt11),
            ("lightning", "on Lightning")
        );
        assert_eq!(
            rail_display(SparkReceiveMethod::LightningAddress),
            ("lightning", "on Lightning")
        );
        assert_eq!(
            rail_display(SparkReceiveMethod::OnchainBitcoin),
            ("bitcoin", "on-chain")
        );
        assert_eq!(
            rail_display(SparkReceiveMethod::Spark),
            ("spark", "on Spark")
        );
    }

    #[test]
    fn cross_network_asset_display_names_are_stable() {
        assert_eq!(asset_display_name("usdt"), "USDt");
        assert_eq!(asset_display_name("usdc"), "USDC");
        assert_eq!(asset_display_name("eth"), "Ether");
        assert_eq!(asset_display_name("sol"), "Solana");
        assert_eq!(asset_display_name("trx"), "Tron");
        assert_eq!(asset_display_name("doge"), "doge");
    }

    #[test]
    fn selector_card_border_only_highlights_on_hover() {
        let theme = theme::Theme::default();
        let active = card_button_style(&theme, iced::widget::button::Status::Active);
        let hovered = card_button_style(&theme, iced::widget::button::Status::Hovered);

        assert_eq!(active.border.color, color::TRANSPARENT);
        assert_eq!(hovered.border.color, color::ORANGE);
    }

    #[test]
    fn selected_picker_row_uses_orange_border_in_both_themes() {
        for theme in [theme::Theme::dark(), theme::Theme::light()] {
            let style = picker_row_selected(&theme);
            assert_eq!(style.border.color, color::ORANGE);
            assert_eq!(style.border.width, 1.0);
        }
    }
}
