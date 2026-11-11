//! View renderer for [`crate::app::state::spark::receive::SparkReceive`].

use coincube_ui::locale::copy as localized_copy;
use coincube_ui::locale::Text as CopyText;
use coincube_ui::localized_format;
use std::collections::{HashMap, HashSet};

use coincube_core::miniscript::bitcoin::Network;
use coincube_ui::{
    color,
    component::{
        amount::BitcoinDisplayUnit,
        button,
        tenshu::{segment, segmented},
        text::*,
    },
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

        // The Bitcoin / Stablecoin choice is in the page head (`head_action`).
        let mut form = Column::new().spacing(0).push(
            Container::new(
                Column::new()
                    .spacing(8)
                    .push(h4_bold(localized_copy("Payment details")))
                    .push(arrival_note(self.network))
                    .push_maybe((!supported_on(self.network)).then(|| {
                        p2_regular(localized_copy(
                            "Stablecoin deposits require Bitcoin mainnet.",
                        ))
                        .small()
                        .style(theme::text::secondary)
                    })),
            )
            .padding([16, 24]),
        );

        if self.cross_network_selected.is_none() {
            form = form.push(iced::widget::rule::horizontal(1)).push(
                Container::new(bitcoin_method_selector(self.method))
                    .padding([18, 24])
                    .width(Length::Fill),
            );
        }

        // Keep the Send-like route summary below the asset and method selectors.
        form = form.push(iced::widget::rule::horizontal(1)).push(
            Container::new(spark_receive_cards(
                self.method,
                self.cross_network_selected,
            ))
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
                if !matches!(self.phase, SparkReceivePhase::Generated(_)) {
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
        if !self.pending_deposits.is_empty() || self.claim_error.is_some() {
            content = content.push(pending_deposits_card(
                self.pending_deposits,
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

fn bitcoin_kind_target(stablecoin: bool, method: SparkReceiveMethod) -> SparkReceiveMethod {
    // The Bitcoin kind is already selected for every native receive method.
    // Only a return from the cross-network flow needs a default method.
    if stablecoin {
        SparkReceiveMethod::Bolt11
    } else {
        method
    }
}

/// "Payments arrive as bitcoin" (master I4): Spark receives bitcoin only, and
/// USDB appears only after a conversion. Where conversions run (mainnet), the
/// note says how to hold US Dollar instead: swap afterwards, or turn the
/// setting on in Wallet settings (P-Receive).
pub(crate) const ARRIVAL_NOTE: &str = "Payments arrive as bitcoin.";
pub(crate) const ARRIVAL_NOTE_USD: &str =
    "To hold them in US Dollar, swap to USDB afterwards or turn on auto-convert in Wallet \
     settings.";
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

fn arrival_note<'a>(network: Network) -> Element<'a, Message> {
    use crate::app::menu::{Menu, SparkSettingsOption, SparkSubMenu};
    // Conversions, and so USDB, exist where Swap does.
    let usd = crate::app::state::spark::swap::swap_available(network);
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

/// The Bitcoin / Stablecoin (cross-chain) choice among the head's actions
/// (the H-Receive board). Stablecoin is disabled off mainnet; the body says
/// why.
pub fn head_action(
    stablecoin: bool,
    method: SparkReceiveMethod,
    network: Network,
) -> Element<'static, Message> {
    segmented(
        [
            segment("spark-receive-kind-bitcoin", false, "Bitcoin"),
            segment(
                "spark-receive-kind-stablecoin",
                true,
                localized_copy("Stablecoin (cross-chain)"),
            )
            .enabled(supported_on(network)),
        ],
        Some(stablecoin),
        Message::FocusAction,
    )
    .on_select(move |choice| {
        if choice {
            Message::SparkReceive(SparkReceiveMessage::OpenSenderPicker)
        } else {
            Message::SparkReceive(SparkReceiveMessage::SelectSenderRail(bitcoin_kind_target(
                stablecoin, method,
            )))
        }
    })
    .into()
}

fn bitcoin_method_selector(current: SparkReceiveMethod) -> Element<'static, Message> {
    #[derive(Clone, Copy, PartialEq)]
    enum MethodChoice {
        Invoice,
        LightningAddress,
        SparkAddress,
        BitcoinDeposit,
    }

    let selected = match current {
        SparkReceiveMethod::Bolt11 => MethodChoice::Invoice,
        SparkReceiveMethod::LightningAddress => MethodChoice::LightningAddress,
        SparkReceiveMethod::Spark => MethodChoice::SparkAddress,
        SparkReceiveMethod::OnchainBitcoin => MethodChoice::BitcoinDeposit,
    };
    let row = segmented(
        [
            segment(
                "spark-receive-method-lightning",
                MethodChoice::Invoice,
                localized_copy("Lightning invoice"),
            ),
            segment(
                "spark-receive-method-lightning-address",
                MethodChoice::LightningAddress,
                localized_copy("Lightning address"),
            ),
            segment(
                "spark-receive-method-spark",
                MethodChoice::SparkAddress,
                localized_copy("Spark address"),
            ),
            segment(
                "spark-receive-method-deposit",
                MethodChoice::BitcoinDeposit,
                localized_copy("Bitcoin address · deposit"),
            ),
        ],
        Some(selected),
        Message::FocusAction,
    )
    .on_select(|choice| match choice {
        MethodChoice::Invoice => Message::SparkReceive(SparkReceiveMessage::SelectSenderRail(
            SparkReceiveMethod::Bolt11,
        )),
        MethodChoice::LightningAddress => Message::SparkReceive(
            SparkReceiveMessage::SelectSenderRail(SparkReceiveMethod::LightningAddress),
        ),
        MethodChoice::SparkAddress => Message::SparkReceive(SparkReceiveMessage::SelectSenderRail(
            SparkReceiveMethod::Spark,
        )),
        MethodChoice::BitcoinDeposit => Message::SparkReceive(
            SparkReceiveMessage::SelectSenderRail(SparkReceiveMethod::OnchainBitcoin),
        ),
    });
    Column::new()
        .spacing(8)
        .push(p1_bold(localized_copy("Method")))
        .push(row)
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

fn pending_deposits_card<'a>(
    deposits: &'a [DepositInfo],
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
    for deposit in deposits {
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

/// Badge slug (for the composited logo) + display label for a Bitcoin rail.
fn rail_display(method: SparkReceiveMethod) -> (&'static str, &'static str) {
    match method {
        SparkReceiveMethod::Bolt11 => ("lightning", "Lightning"),
        SparkReceiveMethod::LightningAddress => ("lightning", "Lightning"),
        SparkReceiveMethod::OnchainBitcoin => ("bitcoin", "On-chain"),
        SparkReceiveMethod::Spark => ("spark", "Spark"),
    }
}

/// Small orange-outlined pill (asset network / rail label).
fn orange_badge<'a>(label: &str) -> Element<'a, Message> {
    Container::new(text(label.to_uppercase()).size(11).color(color::ORANGE))
        .padding([2, 8])
        .style(|_: &theme::Theme| container::Style {
            border: iced::Border {
                color: color::ORANGE,
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        })
        .into()
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

/// The "THEY SEND (rail) → YOU RECEIVE (Bitcoin)" pair. YOU RECEIVE is fixed —
/// Spark only ever receives bitcoin — so only THEY SEND is tappable.
fn spark_receive_cards<'a>(
    method: SparkReceiveMethod,
    cross: Option<DepositOption>,
) -> Element<'a, Message> {
    let you_receive = Container::new(
        Column::new()
            .spacing(6)
            .push(
                text(localized_copy("YOU RECEIVE"))
                    .size(P2_SIZE)
                    .style(theme::text::secondary),
            )
            .push(
                Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(asset_network_logo("btc", "spark", 40.0))
                    .push(
                        text("Bitcoin")
                            .size(H3_SIZE)
                            .bold()
                            .style(theme::text::primary),
                    ),
            )
            .push(
                text(localized_copy("Any amount"))
                    .size(P2_SIZE)
                    .style(theme::text::secondary),
            )
            .push(orange_badge("SPARK")),
    )
    .padding(16)
    .width(Length::Fill)
    .height(Length::Fixed(160.0))
    .style(route_card_style);

    // THEY SEND reflects the current pick: a Bitcoin rail, or a cross-network
    // asset while the SideShift flow is active.
    let (they_icon, they_label, they_badge): (Element<'a, Message>, String, String) = match cross {
        Some(option) => (
            cross_network_logo(&option, 40.0),
            asset_display_name(option.coin).to_string(),
            option.network.network_name().to_string(),
        ),
        None => {
            let (net_slug, badge) = rail_display(method);
            (
                asset_network_logo("btc", net_slug, 40.0),
                "Bitcoin".to_string(),
                badge.to_string(),
            )
        }
    };
    let they_send = iced_button(
        Container::new(
            Column::new()
                .spacing(6)
                .push(
                    text(localized_copy("THEY SEND"))
                        .size(P2_SIZE)
                        .style(theme::text::secondary),
                )
                .push(
                    Row::new()
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(they_icon)
                        .push(
                            text(they_label)
                                .size(H3_SIZE)
                                .bold()
                                .style(theme::text::primary),
                        ),
                )
                .push(Space::new().height(Length::Fixed(18.0)))
                .push(orange_badge(&they_badge)),
        )
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fixed(160.0))
        .style(route_card_style),
    )
    .padding(0)
    .on_press_maybe(cross.map(|_| Message::SparkReceive(SparkReceiveMessage::OpenSenderPicker)))
    .style(card_button_style);

    Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .push(Container::new(they_send).width(Length::FillPortion(1)))
        .push(text("→").size(H3_SIZE).style(theme::text::secondary))
        .push(Container::new(you_receive).width(Length::FillPortion(1)))
        .into()
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

    iced_button(
        Container::new(row)
            .padding([12, 16])
            .width(Length::Fill)
            .style(if is_selected {
                picker_row_selected
            } else {
                crate::app::view::shared::picker::picker_row_idle
            }),
    )
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
    _current: SparkReceiveMethod,
    cross: Option<DepositOption>,
    network: Network,
) -> Element<'a, Message> {
    let is_mainnet = supported_on(network);

    let mut list = Column::new().spacing(8);

    if is_mainnet {
        list = list.push(Space::new().height(Length::Fixed(6.0))).push(
            text(localized_copy("From another network"))
                .size(P2_SIZE)
                .style(theme::text::secondary),
        );
        for option in deposit_options() {
            list = list.push(spark_picker_row(
                cross_network_logo(option, 36.0),
                asset_display_name(option.coin),
                option.network.network_name(),
                cross == Some(*option),
                Message::SparkReceive(SparkReceiveMessage::SelectSenderCrossNetwork(option.key())),
            ));
        }
    }

    // Off mainnet there are only the three rails (no scroll needed); on mainnet
    // the cross-network assets push the list past a screenful, so cap + scroll.
    // The right padding is the scrollbar's gutter — without it the scroller
    // draws on top of the rows' right edge.
    let body: Element<'a, Message> = if is_mainnet {
        scrollable(list.padding(iced::Padding {
            right: 10.0,
            ..iced::Padding::ZERO
        }))
        .height(Length::Fixed(420.0))
        .into()
    } else {
        list.into()
    };

    picker_modal_card(coincube_ui::locale::copy("Stablecoin · they send"), body)
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
        SparkReceivePhase::Idle => Container::new(
            Column::new().spacing(10).push(
                button::primary(None, localized_copy("Generate"))
                    .on_press(Message::SparkReceive(
                        SparkReceiveMessage::GenerateRequested,
                    ))
                    .width(Length::Fixed(160.0)),
            ),
        )
        .padding(16)
        .style(theme::card::simple)
        .into(),

        SparkReceivePhase::Generating => {
            Container::new(Column::new().spacing(10).push(p1_regular(localized_copy(
                "Generating… asking the Spark bridge for a payment request.",
            ))))
            .padding(16)
            .style(theme::card::simple)
            .into()
        }

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
            Container::new(
                Column::new().spacing(16).push(ready_body).push(
                    button::transparent_border(None, localized_copy("Generate another"))
                        .on_press(Message::SparkReceive(SparkReceiveMessage::Reset)),
                ),
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
    fn receive_selectors_show_supported_methods_and_mainnet_gate() {
        let methods = capture::inspect(
            "spark-receive-methods",
            bitcoin_method_selector(SparkReceiveMethod::Bolt11),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        for label in [
            "Lightning invoice",
            "Lightning address",
            "Spark address",
            "Bitcoin address · deposit",
        ] {
            assert!(methods.contains(label));
        }

        let regtest = capture::inspect(
            "spark-receive-regtest",
            head_action(false, SparkReceiveMethod::Bolt11, Network::Regtest),
            (1024, 768),
            "light",
            theme::Theme::light(),
        );
        assert!(regtest.contains("Stablecoin (cross-chain)"));
    }

    /// Master I4: Receive never implies native stablecoin receive. Off
    /// mainnet, where no conversion runs, the note stops at bitcoin.
    #[test]
    fn the_arrival_note_says_payments_arrive_as_bitcoin() {
        let mainnet = capture::inspect(
            "spark-receive-note-mainnet",
            arrival_note(Network::Bitcoin),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(mainnet.contains(ARRIVAL_NOTE) && mainnet.contains(ARRIVAL_NOTE_USD));
        let messages = capture::activate(
            arrival_note(Network::Bitcoin),
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
            arrival_note(Network::Bitcoin),
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
                arrival_note(network),
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
    fn active_bitcoin_kind_reselects_the_displayed_method() {
        for method in [
            SparkReceiveMethod::Bolt11,
            SparkReceiveMethod::LightningAddress,
            SparkReceiveMethod::Spark,
            SparkReceiveMethod::OnchainBitcoin,
        ] {
            assert_eq!(bitcoin_kind_target(false, method), method);
        }
        assert_eq!(
            bitcoin_kind_target(true, SparkReceiveMethod::OnchainBitcoin),
            SparkReceiveMethod::Bolt11,
        );
        let messages = capture::activate(
            head_action(false, SparkReceiveMethod::OnchainBitcoin, Network::Bitcoin),
            iced::advanced::widget::Id::new("spark-receive-kind-bitcoin"),
            (1024, 768),
            false,
        );
        assert!(messages.iter().any(|message| matches!(
            message,
            Message::SparkReceive(SparkReceiveMessage::SelectSenderRail(
                SparkReceiveMethod::OnchainBitcoin
            ))
        )));
    }

    #[test]
    fn lightning_address_method_selects_inline_receive_state() {
        for pointer in [false, true] {
            let messages = capture::activate(
                bitcoin_method_selector(SparkReceiveMethod::Bolt11),
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
                    "Generate another",
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
                    "Method",
                    "Bitcoin address · deposit",
                ] {
                    assert!(
                        mounted.contains(label),
                        "{} missing at {} {:?}",
                        label,
                        appearance,
                        outer
                    );
                }
                // The Bitcoin / Stablecoin choice moved into the head.
                for id in [
                    "spark-receive-kind-bitcoin",
                    "spark-receive-kind-stablecoin",
                ] {
                    assert!(
                        !mounted.has_focus(iced::advanced::widget::Id::new(id)),
                        "{} left in the body",
                        id
                    );
                }
                for id in [
                    WALLET_SETTINGS_ID,
                    "spark-receive-method-lightning",
                    "spark-receive-method-lightning-address",
                    "spark-receive-method-spark",
                    "spark-receive-method-deposit",
                ] {
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
                assert!(x("THEY SEND") < x("YOU RECEIVE"));
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
        assert_eq!(
            rail_display(SparkReceiveMethod::Bolt11),
            ("lightning", "Lightning")
        );
        assert_eq!(
            rail_display(SparkReceiveMethod::OnchainBitcoin),
            ("bitcoin", "On-chain")
        );
        assert_eq!(rail_display(SparkReceiveMethod::Spark), ("spark", "Spark"));
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
