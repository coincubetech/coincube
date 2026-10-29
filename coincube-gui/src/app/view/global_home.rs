use breez_sdk_liquid::{bitcoin::Denomination, model::PreparePayOnchainResponse};
use std::time::Duration;

use coincube_ui::component::text::Text;
use coincube_ui::{
    color,
    component::{amount::*, button, form, spinner, text::*},
    icon::{
        arrow_down_up_icon, arrow_right, check_circle_icon, check_icon, clipboard_icon, clock_icon,
        cross_icon, cube_outline_icon, dash_circle_icon, droplet_fill_icon,
        exclamation_circle_icon, eye_icon, eye_slash_icon, key_icon, lock_icon,
        question_circle_icon, spark_icon, usd_icon, vault_icon, warning_icon, x_circle_icon,
    },
    theme,
    widget::{action::Action, *},
};
use iced::{
    widget::{Button, Column, Space},
    Alignment, Length,
};

use crate::app::breez_liquid::assets::format_usdt_display;
use crate::app::transfer_destination_loader::CubeChoice;
use crate::app::view::shared::landing::{
    amount_cell, landing_button, landing_card_style, landing_control_id, AmountCell, AmountTier,
};
use crate::app::view::wallet_header::{wallet_header, HeaderVariant, SyncState, WalletHeaderProps};
use crate::app::{menu::Menu, view::FiatAmountConverter};
use crate::app::{
    menu::{LiquidSubMenu, VaultSubMenu},
    view::message::{HomeMessage, Message},
};
use crate::chain::ChainId;
use coincube_core::miniscript::bitcoin::Amount;

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
enum WalletType {
    Liquid,
    Usdt { balance: u64, error: bool },
    Vault,
}

/// Stages a transfer passes through between `Initiated` and `Completed`.
///
/// All six directed pairs land on `PendingDeposit` from the UI's perspective —
/// the Done page (`done_page`) renders the "Pending Deposit"
/// treatment at that point and the flow exits. The home-page pending indicators
/// clear asynchronously once the destination's confirmation requirements are met.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferStage {
    Initiated,
    SwappingLbtcToBtc,
    /// Used by any→Liquid leg: after the on-chain tx confirms at the Breez
    /// peg-in swap address, Breez credits L-BTC and we transition to `Completed`.
    SwappingBtcToLbtc,
    /// The source wallet is broadcasting its on-chain tx. Short-lived — flips to
    /// `PendingDeposit` as soon as we have a txid.
    BroadcastingOnChain,
    /// The on-chain tx is broadcast and awaiting confirmations at the destination.
    /// This is the terminal state for the success-screen UX.
    PendingDeposit,
    /// Breez is forwarding the swapped BTC to the Vault address (L-BTC→Vault leg).
    SendingToVault,
    Completed,
}

#[derive(Clone, Copy, Debug)]
pub struct PendingTransfer {
    pub amount: Amount,
    pub stage: TransferStage,
}

/// A quiet, non-animated "Pending deposit" indicator rendered on a wallet card
/// while a transfer is awaiting confirmations at that destination. Confirmation
/// delays are unbounded from the UI's perspective — no progress bar, no stage
/// animation.
fn pending_deposit_card<'a>(
    pending_transfer: PendingTransfer,
    bitcoin_unit: BitcoinDisplayUnit,
    balance_masked: bool,
) -> Element<'a, Message> {
    let display_amount = pending_deposit_amount(pending_transfer, bitcoin_unit, balance_masked);
    Container::new(
        Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .push(warning_icon().size(12).style(theme::text::secondary))
            .push(
                text("Pending deposit")
                    .size(12)
                    .style(theme::text::secondary),
            )
            .push(Space::new().width(Length::Fill))
            .push(text(display_amount).size(12).color(color::ORANGE)),
    )
    .padding([10, 14])
    .style(|t| iced::widget::container::Style {
        border: iced::Border {
            color: t.colors.cards.simple.border.unwrap_or(color::GREY_4),
            width: 0.5,
            radius: 16.0.into(),
        },
        background: Some(iced::Background::Color(t.colors.cards.simple.background)),
        ..Default::default()
    })
    .into()
}

fn pending_deposit_amount(
    pending_transfer: PendingTransfer,
    bitcoin_unit: BitcoinDisplayUnit,
    balance_masked: bool,
) -> String {
    if balance_masked {
        AmountCell::masked().primary
    } else {
        format!(
            "{} {}",
            pending_transfer
                .amount
                .to_formatted_string_with_unit(bitcoin_unit),
            bitcoin_unit.label()
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn wallet_card<'a>(
    wallet_type: WalletType,
    cube_id: &str,
    balance: &Amount,
    fiat_converter: Option<FiatAmountConverter>,
    balance_masked: bool,
    has_vault: bool,
    show_recovery_path: bool,
    import_pending: bool,
    bitcoin_unit: coincube_ui::component::amount::BitcoinDisplayUnit,
    pending_vault_incoming: Option<PendingTransfer>,
    pending_send_sats: u64,
    pending_receive_sats: u64,
    display_mode: crate::app::settings::display::DisplayMode,
    // True once the underlying wallet has reported its first balance.
    // Drives the animated-dots loading placeholder and suppresses
    // pending indicators that would otherwise read alongside a
    // not-yet-authoritative value.
    loaded: bool,
    vault_alias: Option<&str>,
) -> Element<'a, Message> {
    let fiat_balance = fiat_converter.as_ref().map(|c| c.convert(*balance));

    let (icon, title, send_action, receive_action) = match wallet_type {
        WalletType::Liquid => (
            droplet_fill_icon().style(theme::text::secondary),
            "Liquid",
            Message::Menu(Menu::Liquid(LiquidSubMenu::Send)),
            Message::Menu(Menu::Liquid(LiquidSubMenu::Receive)),
        ),
        WalletType::Usdt { .. } => (
            usd_icon().style(theme::text::secondary),
            "USDt",
            Message::Menu(Menu::Liquid(LiquidSubMenu::Send)),
            Message::Menu(Menu::Liquid(LiquidSubMenu::Receive)),
        ),
        WalletType::Vault => (
            vault_icon().style(theme::text::secondary),
            "Vault",
            Message::Menu(Menu::Vault(VaultSubMenu::Send)),
            Message::Menu(Menu::Vault(VaultSubMenu::Receive)),
        ),
    };
    let overview_action = match wallet_type {
        WalletType::Vault => Message::Menu(Menu::Vault(VaultSubMenu::Overview)),
        WalletType::Liquid | WalletType::Usdt { .. } => {
            Message::Menu(Menu::Liquid(LiquidSubMenu::Overview))
        }
    };

    // USDt card renders its own balance display (not Amount-based)
    if let WalletType::Usdt {
        balance: usdt_bal,
        error: usdt_error,
    } = wallet_type
    {
        let content = Column::new()
            .spacing(12)
            .push(
                Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(usd_icon().color(color::ORANGE).size(16))
                    .push(text("USDt").color(color::ORANGE).size(14)),
            )
            .push(
                Row::new()
                    .align_y(Alignment::Center)
                    .push(
                        Column::new()
                            .spacing(4)
                            .push(if balance_masked {
                                Row::new().push(text("********").size(H2_SIZE))
                            } else if usdt_error {
                                Row::new().push(
                                    text("Balance unavailable").size(P1_SIZE).color(color::RED),
                                )
                            } else {
                                Row::new()
                                    .spacing(10)
                                    .align_y(Alignment::Center)
                                    .push(text(format_usdt_display(usdt_bal)).size(H2_SIZE).bold())
                                    .push(text("USDt").size(H2_SIZE).color(color::GREY_3))
                            })
                            .push(
                                text("Liquid Network")
                                    .size(P1_SIZE)
                                    .style(theme::text::secondary),
                            )
                            .push_maybe(
                                (!balance_masked && !usdt_error && pending_send_sats > 0).then(
                                    || {
                                        Row::new()
                                            .spacing(6)
                                            .align_y(Alignment::Center)
                                            .push(
                                                warning_icon()
                                                    .size(12)
                                                    .style(theme::text::secondary),
                                            )
                                            .push(
                                                text(format!(
                                                    "-{} USDt pending",
                                                    format_usdt_display(pending_send_sats)
                                                ))
                                                .size(P2_SIZE)
                                                .style(theme::text::secondary),
                                            )
                                    },
                                ),
                            )
                            .push_maybe(
                                (!balance_masked && !usdt_error && pending_receive_sats > 0).then(
                                    || {
                                        Row::new()
                                            .spacing(6)
                                            .align_y(Alignment::Center)
                                            .push(
                                                warning_icon()
                                                    .size(12)
                                                    .style(theme::text::secondary),
                                            )
                                            .push(
                                                text(format!(
                                                    "+{} USDt pending",
                                                    format_usdt_display(pending_receive_sats)
                                                ))
                                                .size(P2_SIZE)
                                                .style(theme::text::secondary),
                                            )
                                    },
                                ),
                            )
                            .width(Length::Fill),
                    )
                    .push(Space::new().width(Length::Fixed(8.0)))
                    .push(
                        button::primary(None, "Send")
                            .width(Length::Fixed(120.0))
                            .on_press(Message::Menu(Menu::Liquid(LiquidSubMenu::Send))),
                    )
                    .push(Space::new().width(Length::Fixed(8.0)))
                    .push(
                        button::orange_outline(None, "Receive")
                            .width(Length::Fixed(120.0))
                            .on_press(Message::Menu(Menu::Liquid(LiquidSubMenu::Receive))),
                    ),
            );
        return Container::new(content)
            .padding(20)
            .style(landing_card_style)
            .into();
    }

    let content = match wallet_type {
        WalletType::Vault if import_pending => Column::new().spacing(12).push(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(landing_button(
                    landing_control_id(cube_id, "Vault", "open"),
                    "Vault",
                    Some(vault_icon()),
                    Message::FocusAction,
                    Some(overview_action.clone()),
                    theme::button::transparent,
                ))
                .push(text("Wallet import pending").style(theme::text::secondary))
                .push(Space::new().width(Length::Fill))
                .push(landing_button(
                    landing_control_id(cube_id, "Vault", "finish-import"),
                    "Finish import",
                    None,
                    Message::FocusAction,
                    Some(Message::Menu(Menu::Vault(VaultSubMenu::Overview))),
                    theme::button::primary,
                )),
        ),
        WalletType::Vault if !has_vault => Column::new().spacing(12).push(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(landing_button(
                    landing_control_id(cube_id, "Vault", "open"),
                    "Vault",
                    Some(vault_icon()),
                    Message::FocusAction,
                    Some(overview_action.clone()),
                    theme::button::transparent,
                ))
                .push(
                    Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(Space::new().width(Length::Fill))
                        .push(
                            landing_button(
                                landing_control_id(cube_id, "Vault", "restore"),
                                "Restore from Connect",
                                None,
                                Message::FocusAction,
                                Some(Message::SetupVaultRestoreFromKit),
                                theme::button::secondary,
                            )
                            .width(Length::Fixed(200.0)),
                        )
                        .push(
                            landing_button(
                                landing_control_id(cube_id, "Vault", "create"),
                                "Create Vault",
                                None,
                                Message::FocusAction,
                                Some(Message::SetupVault),
                                theme::button::primary,
                            )
                            .width(Length::Fixed(160.0)),
                        ),
                ),
        ),
        _ => {
            // BTC currency row mirrors Spark's BTC row / Liquid's L-BTC row,
            // so Vault's Send/Receive lives at the row level rather than as
            // duplicate card-level buttons. Vault's send/receive actions
            // route through Menu navigation (`send_action`/`receive_action`).
            let btc_fiat_text =
                fiat_balance.map(|f| f.to_text().size(P2_SIZE).style(theme::text::secondary));
            let btc_row = Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(coincube_ui::image::asset_network_logo::<Message>(
                    "btc", "bitcoin", 32.0,
                ))
                .push(text("BTC").size(P1_SIZE).bold().width(Length::Fixed(60.0)))
                .push(if balance_masked {
                    Row::new().push(text("********").size(P1_SIZE))
                } else if !loaded {
                    Row::new().push(spinner::typing_text_carousel(
                        "...",
                        false,
                        Duration::from_millis(400),
                        |content| text(content).size(P1_SIZE).style(theme::text::secondary),
                    ))
                } else {
                    amount_with_size_and_unit(balance, P1_SIZE, bitcoin_unit)
                })
                .push_maybe(
                    (!balance_masked && loaded)
                        .then_some(btc_fiat_text)
                        .flatten(),
                )
                .push(Space::new().width(Length::Fill))
                .push(
                    landing_button(
                        landing_control_id(cube_id, title, "send-btc"),
                        "Send",
                        None,
                        Message::FocusAction,
                        Some(send_action),
                        theme::button::primary,
                    )
                    .width(Length::Fixed(90.0)),
                )
                .push(Space::new().width(Length::Fixed(8.0)))
                .push(
                    landing_button(
                        landing_control_id(cube_id, title, "receive-btc"),
                        "Receive",
                        None,
                        Message::FocusAction,
                        Some(receive_action),
                        theme::button::orange_outline,
                    )
                    .width(Length::Fixed(90.0)),
                );

            // Headline — animated dots while the daemon hasn't
            // reported a first balance. Pending send/receive counts
            // are zeroed in the loading branch so the wallet_header's
            // own pending indicators stay suppressed alongside the
            // placeholder.
            let header: Element<'a, Message> = if loaded {
                wallet_header::<Message>(WalletHeaderProps {
                    sats: *balance,
                    fiat: fiat_balance,
                    balance_masked,
                    bitcoin_unit,
                    variant: HeaderVariant::Card,
                    sync: SyncState::Synced,
                    unconfirmed: None,
                    pending_send_sats,
                    pending_receive_sats,
                    display_mode,
                    on_swap: Some(Message::FlipDisplayMode),
                    on_swap_focus: Some((
                        landing_control_id(cube_id, title, "flip-balance"),
                        Message::FocusAction,
                    )),
                })
                .into()
            } else {
                Column::new()
                    .spacing(4)
                    .push(spinner::typing_text_carousel(
                        "...",
                        false,
                        Duration::from_millis(400),
                        |content| text(content).size(H2_SIZE).bold(),
                    ))
                    .push(spinner::typing_text_carousel(
                        "...",
                        false,
                        Duration::from_millis(400),
                        |content| text(content).size(P1_SIZE).style(theme::text::secondary),
                    ))
                    .into()
            };

            let title_label = if matches!(wallet_type, WalletType::Vault) {
                vault_alias
                    .filter(|alias| !alias.trim().is_empty())
                    .map(|alias| format!("Vault · {alias}"))
                    .unwrap_or_else(|| title.to_owned())
            } else {
                title.to_owned()
            };
            let title_action = Action::new(
                landing_control_id(cube_id, title, "open"),
                Container::new(
                    Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(icon.size(16))
                        .push(text(title_label).font(MEDIUM)),
                )
                .padding(5),
                Message::FocusAction,
            )
            .style(theme::button::transparent)
            .on_press(overview_action);
            let content = Column::new()
                .spacing(12)
                .push(title_action)
                .push(header)
                .push(btc_row);

            // P-Overview keeps the recovery affordance on the Vault card.
            // The prototype opens the Vault landing page, where the actual
            // recovery state is explained; it does not jump into an operation.
            if show_recovery_path {
                content.push(
                    Column::new()
                        .spacing(10)
                        .push(iced::widget::rule::horizontal(1))
                        .push(
                            Action::new(
                                landing_control_id(cube_id, "Vault", "recovery-path"),
                                Container::new(
                                    Row::new()
                                        .spacing(6)
                                        .align_y(Alignment::Center)
                                        .push(clock_icon().size(16))
                                        .push(text("Recovery path").size(P1_SIZE).bold())
                                        .push(arrow_right().size(14)),
                                )
                                .padding(4),
                                Message::FocusAction,
                            )
                            .style(|t, status| {
                                let mut style = theme::button::link(t, status);
                                style.text_color = t.colors.shell.accent_ink;
                                style
                            })
                            .on_press(Message::Menu(Menu::Vault(VaultSubMenu::Overview))),
                        ),
                )
            } else {
                content
            }
        }
    };

    let content = if matches!(wallet_type, WalletType::Vault) && loaded {
        if let Some(pending_transfer) = pending_vault_incoming {
            if pending_transfer.stage != TransferStage::Completed {
                content.push(pending_deposit_card(
                    pending_transfer,
                    bitcoin_unit,
                    balance_masked,
                ))
            } else {
                content
            }
        } else {
            content
        }
    } else {
        content
    };

    Container::new(content)
        .padding(20)
        .style(landing_card_style)
        .into()
}

fn wallet_kind_icon<'a, M>(kind: WalletKind, size: f32) -> Element<'a, M>
where
    M: 'a,
{
    match kind {
        WalletKind::Liquid => droplet_fill_icon()
            .size(size)
            .style(theme::text::secondary)
            .into(),
        WalletKind::Spark => spark_icon().size(size).style(theme::text::secondary).into(),
        WalletKind::Vault => vault_icon().size(size).style(theme::text::secondary).into(),
    }
}

/// Steps of a transfer within this Cube (P-Transfer `pathSteps`).
const SAME_CUBE_STEPS: [&str; 3] = ["Wallets", "Amount", "Review"];
/// Steps of a transfer to another Cube: its choice and unlock come between
/// Wallets and Amount.
const OTHER_CUBE_STEPS: [&str; 5] = ["Wallets", "Other Cube", "Unlock", "Amount", "Review"];

/// Which page `HomeView` step 1 shows. Review and Done stay steps 2 and 3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransferPage {
    #[default]
    Wallets,
    OtherCube,
    Unlock,
    Amount,
}

/// Number of checks the other Cube's unlock runs: credential, Vault
/// identity, receive address.
pub const UNLOCK_CHECKS: u8 = 3;

/// Progress of the other Cube's in-place unlock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnlockPhase {
    /// Waiting for the PIN or passkey; `busy` while it is being checked.
    Credential { busy: bool },
    /// Checks passed so far, out of [`UNLOCK_CHECKS`].
    Checks(u8),
}

impl Default for UnlockPhase {
    fn default() -> Self {
        Self::Credential { busy: false }
    }
}

/// What the "Another Cube…" route shows. Built from the state's catalog
/// snapshot; no balance of a locked Cube is ever read.
#[derive(Clone, Copy, Debug, Default)]
pub struct OtherCubeView<'a> {
    pub choices: &'a [CubeChoice],
    pub selected: Option<&'a str>,
    pub pin: &'a str,
    pub phase: UnlockPhase,
    pub error: Option<&'a str>,
    /// The destination Cube is unlocked for this transfer.
    pub unlocked: bool,
    /// The transfer was sent and the destination Cube locked again.
    pub locked_again: bool,
}

impl<'a> OtherCubeView<'a> {
    fn choice(&self) -> Option<&'a CubeChoice> {
        let selected = self.selected?;
        self.choices
            .iter()
            .find(|choice| choice.cube_id == selected)
    }

    fn name(&self) -> &'a str {
        self.choice()
            .map_or("the other Cube", |choice| choice.name.as_str())
    }
}

/// Everything the dialog's pages share.
#[derive(Clone, Copy)]
struct TransferCtx<'a> {
    cube_id: &'a str,
    cube_name: &'a str,
    chain: ChainId,
    light: bool,
    direction: TransferDirection,
    has_vault: bool,
    has_spark: bool,
    has_liquid: bool,
    liquid_balance: Amount,
    spark_balance: Amount,
    vault_balance: Amount,
    fiat_converter: Option<FiatAmountConverter>,
    unit: BitcoinDisplayUnit,
    balance_masked: bool,
    other: OtherCubeView<'a>,
}

impl TransferCtx<'_> {
    fn balance(&self, kind: WalletKind) -> Amount {
        match kind {
            WalletKind::Liquid => self.liquid_balance,
            WalletKind::Spark => self.spark_balance,
            WalletKind::Vault => self.vault_balance,
        }
    }

    fn steps(&self) -> &'static [&'static str] {
        if self.direction.is_other_cube() {
            &OTHER_CUBE_STEPS
        } else {
            &SAME_CUBE_STEPS
        }
    }

    /// Index of `page` in [`Self::steps`]; Review and Done follow the pages.
    fn index(&self, page: TransferPage) -> usize {
        match (self.direction.is_other_cube(), page) {
            (_, TransferPage::Wallets) => 0,
            (true, TransferPage::OtherCube) => 1,
            (true, TransferPage::Unlock) => 2,
            (true, TransferPage::Amount) => 3,
            (false, _) => 1,
        }
    }

    fn review_index(&self) -> usize {
        self.steps().len() - 1
    }
}

fn unit_label(unit: BitcoinDisplayUnit) -> &'static str {
    match unit {
        BitcoinDisplayUnit::BTC => "BTC",
        BitcoinDisplayUnit::Sats => "SATS",
    }
}

/// "25,053 SATS", or the mask.
fn bitcoin_text(amount: Amount, unit: BitcoinDisplayUnit, masked: bool) -> String {
    if masked {
        "••••••".to_string()
    } else {
        format!(
            "{} {}",
            amount.to_formatted_string_with_unit(unit),
            unit_label(unit)
        )
    }
}

/// P-Transfer's balance column: "20.74 USD · 25,053 SATS".
fn balance_text(
    amount: Amount,
    fiat_converter: Option<FiatAmountConverter>,
    unit: BitcoinDisplayUnit,
    masked: bool,
) -> String {
    if masked {
        return "••••••".to_string();
    }
    match fiat_converter.map(|converter| converter.convert(amount)) {
        Some(fiat) => format!(
            "{} {} · {}",
            fiat.to_formatted_string(),
            fiat.currency(),
            bitcoin_text(amount, unit, false)
        ),
        None => bitcoin_text(amount, unit, false),
    }
}

fn muted<'a>(content: impl iced::widget::text::IntoFragment<'a>) -> coincube_ui::widget::Text<'a> {
    text(content).size(13).style(theme::text::secondary)
}

/// P-Transfer's `.form h2` and `.lede`.
fn page_heading<'a>(title: String, lede: Option<String>) -> Element<'a, Message> {
    Column::new()
        .spacing(4)
        .push(text(title).size(18).bold())
        .push_maybe(lede.map(muted))
        .into()
}

/// P-Transfer's `.grp`: a group after the first gets a rule above it.
fn group<'a>(
    legend: Option<String>,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    Column::new()
        .spacing(8)
        .push_maybe(legend.map(|legend| text(legend).size(14).bold()))
        .push(body)
        .into()
}

fn group_rule<'a>() -> Element<'a, Message> {
    Container::new(iced::widget::rule::horizontal(1))
        .padding([4, 0])
        .into()
}

/// P-Transfer's `.acts`: secondary on the left, primary on the right.
fn page_actions<'a>(
    back: Option<Element<'a, Message>>,
    primary: Element<'a, Message>,
) -> Element<'a, Message> {
    Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push_maybe(back)
        .push(Space::new().width(Length::Fill))
        .push(primary)
        .into()
}

/// P-Transfer's `.btn`: 36px tall, sized to its label.
fn dialog_button<'a>(
    label: impl Into<String>,
    icon: Option<coincube_ui::widget::Text<'a>>,
    primary: bool,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    Button::new(
        Row::new()
            .spacing(6)
            .align_y(Alignment::Center)
            .push_maybe(icon.map(|icon| icon.size(14)))
            .push(text(label.into()).bold()),
    )
    .padding([8, 16])
    .style(if primary {
        theme::button::primary
    } else {
        theme::button::secondary
    })
    .on_press_maybe(on_press)
    .into()
}

fn back_button<'a>(label: &'static str, on_press: Option<Message>) -> Element<'a, Message> {
    dialog_button(label, None, false, on_press)
}

fn status_line<'a>(
    icon: coincube_ui::widget::Text<'a>,
    tone: StatusTone,
    content: String,
) -> Element<'a, Message> {
    let color = move |t: &theme::Theme| iced::widget::text::Style {
        color: Some(match tone {
            StatusTone::Good => t.colors.shell.good,
            StatusTone::Warn => t.colors.shell.warn,
            StatusTone::Bad => t.colors.shell.bad,
            StatusTone::Muted => t.colors.shell.muted,
        }),
    };
    Row::new()
        .spacing(6)
        .align_y(Alignment::Center)
        .push(icon.size(14).style(color))
        .push(text(content).size(13).style(color))
        .into()
}

#[derive(Clone, Copy)]
enum StatusTone {
    Good,
    Warn,
    Bad,
    Muted,
}

/// The network coin shown beside a Cube's name.
fn network_coin<'a>(chain: ChainId, light: bool) -> Element<'a, Message> {
    iced::widget::Svg::new(iced::widget::svg::Handle::from_memory(
        crate::home::launcher_coin::widget_source(chain, 16.0, 1.0, light).into_bytes(),
    ))
    .width(Length::Fixed(16.0))
    .height(Length::Fixed(16.0))
    .into()
}

fn network_name(chain: ChainId) -> &'static str {
    crate::app::view::nav::shell::network_label(chain)
}

/// The persisted top-bar preference is the privacy floor for both Overview sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OverviewMasks {
    total: bool,
    wallets: bool,
}

impl OverviewMasks {
    fn new(global: bool, total: bool, wallets: bool) -> Self {
        Self {
            total: global || total,
            wallets: global || wallets,
        }
    }
}

fn overview_eye<'a>(
    cube_id: &str,
    section: &'static str,
    masked: bool,
    global_hidden: bool,
    toggle: HomeMessage,
) -> Element<'a, Message> {
    let glyph = if masked { eye_slash_icon() } else { eye_icon() };
    let label = if global_hidden {
        "Global Hide is on"
    } else if masked {
        "Show balance"
    } else {
        "Hide balance"
    };
    iced::widget::tooltip(
        Action::new(
            landing_control_id(cube_id, section, "balance-privacy"),
            glyph.size(16),
            Message::FocusAction,
        )
        .padding([7, 9])
        .style(theme::button::shell_nav)
        .on_press_maybe((!global_hidden).then_some(Message::Home(toggle))),
        Container::new(text(label).size(P2_SIZE))
            .padding([6, 10])
            .style(theme::container::shell_menu),
        iced::widget::tooltip::Position::Bottom,
    )
    .into()
}

/// P-Transfer's `.stepper`: one row of numbered steps above a rule. The
/// current step has an accent disc and ink label, finished steps a check in
/// the "good" colour; `active == labels.len()` marks every step finished.
fn transfer_steps<'a>(labels: &[&'static str], active: usize) -> Element<'a, Message> {
    let mut steps = Row::new().spacing(4).width(Length::Fill);
    for (index, label) in labels.iter().enumerate() {
        let current = index == active;
        let done = index < active;
        let marker: Element<'a, Message> = if done {
            check_icon()
                .size(12)
                .style(|t: &theme::Theme| iced::widget::text::Style {
                    color: Some(t.colors.shell.good),
                })
                .into()
        } else {
            text((index + 1).to_string())
                .size(12)
                .bold()
                .style(move |t: &theme::Theme| iced::widget::text::Style {
                    color: Some(if current {
                        theme::tenshu::ON_ACCENT
                    } else {
                        t.colors.shell.ink
                    }),
                })
                .into()
        };
        let marker = Container::new(marker)
            .center_x(Length::Fixed(22.0))
            .center_y(Length::Fixed(22.0))
            .style(move |t: &theme::Theme| iced::widget::container::Style {
                background: current.then_some(iced::Background::Color(t.colors.shell.accent)),
                border: iced::Border {
                    color: if current {
                        t.colors.shell.accent
                    } else if done {
                        t.colors.shell.good
                    } else {
                        t.colors.shell.line
                    },
                    width: 1.0,
                    radius: 11.0.into(),
                },
                ..Default::default()
            });
        let title = if current {
            text(*label).size(13).bold()
        } else {
            text(*label).size(13).style(theme::text::secondary)
        };
        steps = steps.push(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .width(Length::FillPortion(1))
                .push(marker)
                .push(title),
        );
    }
    Column::new()
        .spacing(16)
        .push(steps)
        .push(iced::widget::rule::horizontal(1))
        .into()
}

/// Outlined panel for the review address.
fn review_panel_style(t: &theme::Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        border: iced::Border {
            color: t.colors.shell.line,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

/// P-Transfer's `.form` card.
fn form_style(t: &theme::Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(iced::Background::Color(t.colors.shell.panel)),
        border: iced::Border {
            color: t.colors.shell.line,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    }
}

/// The dialog's own surface: the page background of P-Transfer's `.pg`.
fn dialog_style(t: &theme::Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(iced::Background::Color(t.colors.shell.background)),
        border: iced::Border {
            color: t.colors.shell.line,
            width: 1.0,
            radius: 16.0.into(),
        },
        ..Default::default()
    }
}

/// The other Cube's state above the form: open while the transfer runs,
/// locked again once it is sent.
fn other_cube_context<'a>(
    ctx: &TransferCtx<'a>,
    open: bool,
    done: bool,
) -> Option<Element<'a, Message>> {
    if !ctx.direction.is_other_cube() {
        return None;
    }
    if done && ctx.other.locked_again {
        return Some(status_line(
            lock_icon(),
            StatusTone::Good,
            format!("{} is locked again.", ctx.other.name()),
        ));
    }
    (open && ctx.other.unlocked).then(|| {
        status_line(
            lock_icon(),
            StatusTone::Warn,
            format!(
                "{} · {} · unlocked for this transfer",
                ctx.other.name(),
                network_name(ctx.chain)
            ),
        )
    })
}

/// The transfer flow as a dialog over the Overview: P-Transfer's title, Cube
/// line and stepper, then the step's form card, which scrolls when the window
/// is short. `close` is `None` while a send is in flight.
fn transfer_dialog<'a>(
    ctx: &TransferCtx<'a>,
    active: usize,
    context: Option<Element<'a, Message>>,
    close: Option<Message>,
    form: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    let header = Row::new()
        .spacing(12)
        .align_y(Alignment::Start)
        .push(
            Column::new()
                .spacing(4)
                .width(Length::Fill)
                .push(text("Transfer").size(22).bold())
                .push(
                    Row::new()
                        .spacing(6)
                        .align_y(Alignment::Center)
                        .push(network_coin(ctx.chain, ctx.light))
                        .push(muted(format!(
                            "{} · {}",
                            ctx.cube_name,
                            network_name(ctx.chain)
                        ))),
                ),
        )
        .push(
            Button::new(cross_icon().size(16).style(theme::text::secondary))
                .padding(8)
                .style(theme::button::transparent)
                .on_press_maybe(close),
        );
    Container::new(
        Container::new(
            Column::new()
                .spacing(16)
                .push(header)
                .push(transfer_steps(ctx.steps(), active))
                .push_maybe(context)
                .push(iced::widget::scrollable(
                    Container::new(
                        Container::new(form)
                            .padding(24)
                            .width(Length::Fill)
                            .style(form_style),
                    )
                    .padding(iced::Padding {
                        right: 12.0,
                        ..Default::default()
                    }),
                )),
        )
        .padding(28)
        .width(Length::Fill)
        .max_width(820)
        .style(dialog_style),
    )
    .padding(24)
    .into()
}

/// A drawn radio mark: an accent ring and dot when selected, a muted ring
/// otherwise. The row around it is the control.
fn radio_mark<'a>(selected: bool, enabled: bool) -> Element<'a, Message> {
    let dot = Container::new(Space::new())
        .width(Length::Fixed(8.0))
        .height(Length::Fixed(8.0))
        .style(move |t: &theme::Theme| iced::widget::container::Style {
            background: selected.then_some(iced::Background::Color(t.colors.shell.accent)),
            border: iced::Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    Container::new(dot)
        .center_x(Length::Fixed(16.0))
        .center_y(Length::Fixed(16.0))
        .style(move |t: &theme::Theme| iced::widget::container::Style {
            border: iced::Border {
                color: if selected {
                    t.colors.shell.accent
                } else if enabled {
                    t.colors.shell.ink
                } else {
                    t.colors.shell.line
                },
                width: 1.5,
                radius: 8.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// P-Transfer's `.opt`: radio, icon, name with an optional sub line, and a
/// trailing value. A disabled row is drawn muted and never activates.
#[allow(clippy::too_many_arguments)]
fn option_row<'a>(
    id: iced::advanced::widget::Id,
    icon: Element<'a, Message>,
    name: Element<'a, Message>,
    sub: Option<Element<'a, Message>>,
    trailing: Option<Element<'a, Message>>,
    selected: bool,
    on_press: Option<Message>,
) -> Element<'a, Message> {
    let enabled = on_press.is_some() || selected;
    let content = Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .push(radio_mark(selected, enabled))
        .push(icon)
        .push(
            Column::new()
                .spacing(1)
                .width(Length::Fill)
                .push(name)
                .push_maybe(sub),
        )
        .push_maybe(trailing);
    let row = Container::new(content)
        .padding([12, 12])
        .width(Length::Fill)
        .style(move |t| iced::widget::container::Style {
            background: selected.then_some(iced::Background::Color(t.colors.shell.selected)),
            border: iced::Border {
                radius: 8.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    Action::new(id, row, Message::FocusAction)
        .padding(0)
        .width(Length::Fill)
        .style(theme::button::transparent)
        .on_press_maybe(on_press)
        .into()
}

#[allow(clippy::too_many_arguments)]
fn transfer_wallet_row<'a>(
    cube_id: &str,
    side: PickerSide,
    kind: WalletKind,
    selected: bool,
    balance: Amount,
    fiat_converter: Option<FiatAmountConverter>,
    bitcoin_unit: BitcoinDisplayUnit,
    balance_masked: bool,
) -> Element<'a, Message> {
    let empty = balance == Amount::ZERO;
    option_row(
        landing_control_id(
            cube_id,
            kind.label(),
            match side {
                PickerSide::From => "transfer-from",
                PickerSide::To => "transfer-to",
            },
        ),
        wallet_kind_icon::<Message>(kind, 18.0),
        text(kind.label()).size(P1_SIZE).bold().into(),
        (side == PickerSide::From && empty && !balance_masked).then(|| muted("Empty").into()),
        Some(
            text(balance_text(
                balance,
                fiat_converter,
                bitcoin_unit,
                balance_masked,
            ))
            .size(13)
            .into(),
        ),
        selected,
        Some(Message::Home(HomeMessage::SelectTransferWallet(side, kind))),
    )
}

/// The selected source remains in the From list; To excludes that wallet so
/// the user cannot choose a same-wallet transfer. Selecting a source that was
/// previously the destination swaps the pair in the state layer.
#[allow(clippy::too_many_arguments)]
fn transfer_wallet_section<'a>(
    cube_id: &str,
    side: PickerSide,
    direction: TransferDirection,
    has_vault: bool,
    has_spark: bool,
    has_liquid: bool,
    liquid_balance: Amount,
    spark_balance: Amount,
    vault_balance: Amount,
    fiat_converter: Option<FiatAmountConverter>,
    bitcoin_unit: BitcoinDisplayUnit,
    balance_masked: bool,
) -> Element<'a, Message> {
    let selected = match side {
        PickerSide::From => Some(direction.from_kind()),
        PickerSide::To => (!direction.is_other_cube()).then(|| direction.to_kind()),
    };
    let mut col = Column::new().spacing(2).push(
        Container::new(
            text(match side {
                PickerSide::From => "From",
                PickerSide::To => "To",
            })
            .size(14)
            .bold(),
        )
        .padding(iced::Padding {
            bottom: 6.0,
            ..Default::default()
        }),
    );
    for (kind, balance, available) in [
        (WalletKind::Spark, spark_balance, has_spark),
        (WalletKind::Vault, vault_balance, has_vault),
        (WalletKind::Liquid, liquid_balance, has_liquid),
    ] {
        if available && (side == PickerSide::From || kind != direction.from_kind()) {
            col = col.push(transfer_wallet_row(
                cube_id,
                side,
                kind,
                Some(kind) == selected,
                balance,
                fiat_converter,
                bitcoin_unit,
                balance_masked,
            ));
        }
    }
    col.into()
}

/// Step 1: From and To, with "Another Cube…" last in To.
fn wallets_page<'a>(ctx: &TransferCtx<'a>) -> Element<'a, Message> {
    let source = ctx.direction.from_kind();
    let source_empty = ctx.balance(source) == Amount::ZERO;
    let another_enabled = ctx.has_vault && source == WalletKind::Vault;
    let another = option_row(
        landing_control_id(ctx.cube_id, "another-cube", "transfer-to"),
        cube_outline_icon()
            .size(18)
            .style(theme::text::secondary)
            .into(),
        text("Another Cube…").size(P1_SIZE).bold().into(),
        Some(
            muted(if another_enabled {
                "Unlocks for this transfer only"
            } else {
                "From the Vault only, for now"
            })
            .into(),
        ),
        None,
        ctx.direction.is_other_cube(),
        another_enabled.then_some(Message::Home(HomeMessage::SelectAnotherCube)),
    );
    let section = |side| {
        transfer_wallet_section(
            ctx.cube_id,
            side,
            ctx.direction,
            ctx.has_vault,
            ctx.has_spark,
            ctx.has_liquid,
            ctx.liquid_balance,
            ctx.spark_balance,
            ctx.vault_balance,
            ctx.fiat_converter,
            ctx.unit,
            ctx.balance_masked,
        )
    };
    let continue_note =
        source_empty.then(|| format!("{} has nothing to transfer.", source.label()));
    transfer_dialog(
        ctx,
        ctx.index(TransferPage::Wallets),
        None,
        Some(Message::Home(HomeMessage::BackToHome)),
        Column::new()
            .spacing(20)
            .push(page_heading("Transfer between wallets".into(), None))
            .push(section(PickerSide::From))
            .push(group_rule())
            .push(
                Column::new()
                    .spacing(2)
                    .push(section(PickerSide::To))
                    .push(another),
            )
            .push(
                Row::new()
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .push(dialog_button(
                        "Continue",
                        None,
                        true,
                        (!source_empty).then_some(Message::Home(HomeMessage::ContinueFromWallets)),
                    ))
                    .push_maybe(continue_note.map(muted)),
            ),
    )
}

/// Other Cube: the local catalog, every Cube locked and without balances.
fn other_cube_page<'a>(ctx: &TransferCtx<'a>) -> Element<'a, Message> {
    let mut rows = Column::new().spacing(2);
    for choice in ctx.other.choices {
        let selected = ctx.other.selected == Some(choice.cube_id.as_str());
        let tag = (choice.chain != ctx.chain).then(|| {
            Container::new(text(network_name(choice.chain)).size(12))
                .padding([0, 6])
                .style(|t: &theme::Theme| iced::widget::container::Style {
                    border: iced::Border {
                        color: t.colors.shell.muted,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                })
        });
        let sub =
            Column::new()
                .spacing(2)
                .push(muted(format!(
                    "{} · {}",
                    network_name(choice.chain),
                    if choice.has_vault {
                        "Vault"
                    } else {
                        "No Vault"
                    }
                )))
                .push_maybe(choice.refusal.map(|reason| {
                    status_line(x_circle_icon(), StatusTone::Muted, reason.to_string())
                }));
        rows = rows.push(option_row(
            landing_control_id(ctx.cube_id, &choice.cube_id, "transfer-cube"),
            network_coin(choice.chain, ctx.light),
            Row::new()
                .spacing(6)
                .align_y(Alignment::Center)
                .push(text(choice.name.clone()).size(P1_SIZE).bold())
                .push_maybe(tag)
                .into(),
            Some(sub.into()),
            Some(
                Row::new()
                    .spacing(4)
                    .align_y(Alignment::Center)
                    .push(lock_icon().size(13).style(theme::text::secondary))
                    .push(muted("Locked"))
                    .into(),
            ),
            selected,
            choice
                .refusal
                .is_none()
                .then(|| Message::Home(HomeMessage::SelectOtherCube(choice.cube_id.clone()))),
        ));
    }
    if ctx.other.choices.is_empty() {
        rows = rows.push(muted("No other Cubes on this device."));
    }
    let selected = ctx.other.choice().filter(|choice| choice.refusal.is_none());
    let continue_label = selected.map_or("Continue".to_string(), |choice| {
        format!("Continue with {}", choice.name)
    });
    transfer_dialog(
        ctx,
        ctx.index(TransferPage::OtherCube),
        None,
        Some(Message::Home(HomeMessage::BackToHome)),
        Column::new()
            .spacing(20)
            .push(page_heading(
                "Choose the other Cube".into(),
                Some(format!("{} Cubes only.", network_name(ctx.chain))),
            ))
            .push(group(Some("Other Cubes".into()), rows))
            .push_maybe(ctx.other.error.map(|error| {
                status_line(
                    exclamation_circle_icon(),
                    StatusTone::Bad,
                    error.to_string(),
                )
            }))
            .push(status_line(
                question_circle_icon(),
                StatusTone::Muted,
                "Locked Cubes show no balances.".into(),
            ))
            .push(page_actions(
                Some(back_button(
                    "Back",
                    Some(Message::Home(HomeMessage::PreviousStep)),
                )),
                dialog_button(
                    continue_label,
                    None,
                    true,
                    selected.map(|_| Message::Home(HomeMessage::ContinueWithCube)),
                ),
            )),
    )
}

/// One row of the unlock checks: done, running or waiting.
fn check_row<'a>(label: String, state: std::cmp::Ordering) -> Element<'a, Message> {
    use std::cmp::Ordering::*;
    let (icon, status): (Element<'a, Message>, &str) = match state {
        Less => (
            check_circle_icon()
                .size(16)
                .style(|t: &theme::Theme| iced::widget::text::Style {
                    color: Some(t.colors.shell.good),
                })
                .into(),
            "Done",
        ),
        Equal => (
            clock_icon().size(16).style(theme::text::secondary).into(),
            "In progress",
        ),
        Greater => (
            dash_circle_icon()
                .size(16)
                .style(theme::text::secondary)
                .into(),
            "Waiting",
        ),
    };
    Column::new()
        .push(
            Row::new()
                .spacing(10)
                .align_y(Alignment::Center)
                .padding([12, 0])
                .push(icon)
                .push(text(label).width(Length::Fill))
                .push(muted(status)),
        )
        .push(iced::widget::rule::horizontal(1))
        .into()
}

/// Unlock: the other Cube's own PIN or passkey, then its checks.
fn unlock_page<'a>(ctx: &TransferCtx<'a>) -> Element<'a, Message> {
    let name = ctx.other.name();
    let passkey = ctx.other.choice().is_some_and(|choice| choice.passkey);
    let error = ctx.other.error.map(|error| {
        status_line(
            exclamation_circle_icon(),
            StatusTone::Bad,
            error.to_string(),
        )
    });
    let body: Element<'a, Message> = match ctx.other.phase {
        UnlockPhase::Credential { busy } => {
            let unlock: Element<'a, Message> = if passkey {
                dialog_button(
                    "Use passkey",
                    Some(key_icon()),
                    true,
                    (!busy).then_some(Message::Home(HomeMessage::UnlockOtherCubePasskey)),
                )
            } else {
                button::primary_loading(
                    None,
                    "Unlock",
                    busy,
                    (!ctx.other.pin.is_empty() && !busy)
                        .then_some(Message::Home(HomeMessage::UnlockOtherCube)),
                )
                .width(Length::Fixed(120.0))
                .into()
            };
            Column::new()
                .spacing(16)
                .push_maybe((!passkey).then(|| {
                    Column::new()
                        .spacing(6)
                        .push(text(format!("{name} PIN")).bold())
                        .push(
                            Container::new(
                                iced::widget::text_input("", ctx.other.pin)
                                    .secure(true)
                                    .size(18)
                                    .padding(10)
                                    .on_input_maybe((!busy).then_some(|pin| {
                                        Message::Home(HomeMessage::OtherCubePinEdited(pin))
                                    }))
                                    .on_submit_maybe(
                                        (!busy && !ctx.other.pin.is_empty())
                                            .then_some(Message::Home(HomeMessage::UnlockOtherCube)),
                                    ),
                            )
                            .width(Length::Fixed(240.0)),
                        )
                        .push(muted(format!("Not {}'s PIN.", ctx.cube_name)))
                }))
                .push_maybe(error)
                .push(
                    Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(unlock)
                        .push(Space::new().width(Length::Fill))
                        .push(back_button(
                            "Cancel",
                            Some(Message::Home(HomeMessage::PreviousStep)),
                        )),
                )
                .into()
        }
        UnlockPhase::Checks(done) => {
            let state = |row: u8| done.cmp(&row).reverse();
            let rows = Column::new()
                .push(iced::widget::rule::horizontal(1))
                .push(check_row(
                    if passkey {
                        "Passkey accepted"
                    } else {
                        "PIN accepted"
                    }
                    .into(),
                    state(0),
                ))
                .push(check_row(
                    if done > 1 {
                        format!("{name}'s Vault checked")
                    } else {
                        format!("Checking {name}'s Vault…")
                    },
                    state(1),
                ))
                .push(check_row(
                    if done > 2 {
                        "Receive address checked".to_string()
                    } else {
                        "Getting a receive address…".to_string()
                    },
                    state(2),
                ));
            Column::new()
                .spacing(16)
                .push(rows)
                .push_maybe(error)
                .push(page_actions(
                    Some(back_button(
                        "Cancel and lock",
                        Some(Message::Home(HomeMessage::PreviousStep)),
                    )),
                    dialog_button(
                        "Continue",
                        None,
                        true,
                        (done >= UNLOCK_CHECKS)
                            .then_some(Message::Home(HomeMessage::ContinueFromUnlock)),
                    ),
                ))
                .into()
        }
    };
    transfer_dialog(
        ctx,
        ctx.index(TransferPage::Unlock),
        None,
        Some(Message::Home(HomeMessage::BackToHome)),
        Column::new()
            .spacing(16)
            .push(page_heading(
                format!("Unlock {name} for this transfer only"),
                Some("Locks again when the transfer ends.".into()),
            ))
            .push(
                Row::new()
                    .spacing(6)
                    .align_y(Alignment::Center)
                    .push(network_coin(ctx.chain, ctx.light))
                    .push(muted(format!(
                        "{name} · {} · Vault",
                        network_name(ctx.chain)
                    ))),
            )
            .push(iced::widget::rule::horizontal(1))
            .push(body),
    )
}

/// The SATS/BTC switch beside the amount (P-Transfer's `.seg`).
fn unit_switch<'a>(unit: BitcoinDisplayUnit) -> Element<'a, Message> {
    let segment = |label: &'static str, value: BitcoinDisplayUnit| {
        let on = value == unit;
        Button::new(text(label).size(13).bold())
            .padding([8, 12])
            .style(move |t: &theme::Theme, status| {
                let mut style = theme::button::transparent(t, status);
                if on {
                    style.background = Some(iced::Background::Color(t.colors.shell.selected));
                    style.text_color = t.colors.shell.ink;
                } else {
                    style.text_color = t.colors.shell.muted;
                }
                style.border.radius = 6.0.into();
                style
            })
            .on_press_maybe((!on).then_some(Message::Home(HomeMessage::SetTransferUnit(value))))
    };
    Container::new(
        Row::new()
            .spacing(2)
            .push(segment("SATS", BitcoinDisplayUnit::Sats))
            .push(segment("BTC", BitcoinDisplayUnit::BTC)),
    )
    .padding(2)
    .style(|t: &theme::Theme| iced::widget::container::Style {
        border: iced::Border {
            color: t.colors.shell.line,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn parse_amount(value: &form::Value<String>, unit: BitcoinDisplayUnit) -> Option<Amount> {
    let denomination = match unit {
        BitcoinDisplayUnit::BTC => Denomination::Bitcoin,
        BitcoinDisplayUnit::Sats => Denomination::Satoshi,
    };
    (!value.value.is_empty())
        .then(|| Amount::from_str_in(&value.value, denomination).ok())
        .flatten()
}

fn fiat_estimate(amount: Amount, fiat_converter: Option<FiatAmountConverter>) -> Option<String> {
    fiat_converter.map(|converter| {
        let fiat = converter.convert(amount);
        format!("≈ {} {}", fiat.to_formatted_string(), fiat.currency())
    })
}

/// Amount: the destination, then the amount with unit switch, fiat estimate
/// and the rails' limits; Prepare fetches the destination and the quote.
fn amount_page<'a>(
    ctx: &TransferCtx<'a>,
    amount: &'a form::Value<String>,
    onchain_send_limit: Option<(u64, u64)>,
    onchain_receive_limit: Option<(u64, u64)>,
) -> Element<'a, Message> {
    let direction = ctx.direction;
    let source = direction.from_kind();
    let parsed = parse_amount(amount, ctx.unit).filter(|_| amount.valid);
    let estimate = match parsed {
        Some(parsed) if !ctx.balance_masked => fiat_estimate(parsed, ctx.fiat_converter),
        Some(_) => None,
        None => Some("Enter an amount".to_string()),
    };
    // Helper text reflects the per-direction effective minimum (§Design
    // section 4) composed with Breez's SDK limits on swap-involving legs.
    let effective_min = crate::app::state::effective_transfer_min_sat(
        direction,
        onchain_send_limit,
        onchain_receive_limit,
    );
    let effective_max = crate::app::state::effective_transfer_max_sat(
        direction,
        onchain_send_limit,
        onchain_receive_limit,
    );
    let limit = |sat| bitcoin_text(Amount::from_sat(sat), ctx.unit, false);
    let limits = match (effective_min, effective_max) {
        (Some(min), Some(max)) => format!("Minimum {} · Maximum {}", limit(min), limit(max)),
        (Some(min), None) => format!("Minimum {}", limit(min)),
        _ => "Loading limits…".to_string(),
    };
    let destination: Element<'a, Message> = if direction.is_other_cube() {
        group(
            Some(format!("Wallet in {}", ctx.other.name())),
            option_row(
                landing_control_id(ctx.cube_id, "other-cube-vault", "transfer-dest"),
                wallet_kind_icon::<Message>(WalletKind::Vault, 18.0),
                text("Vault").size(P1_SIZE).bold().into(),
                None,
                None,
                true,
                None,
            ),
        )
    } else {
        group(
            Some("To".into()),
            Row::new()
                .spacing(4)
                .push(text(direction.to_wallet()))
                .push(muted("· this Cube")),
        )
    };
    let input = match ctx.unit {
        BitcoinDisplayUnit::BTC => form::Form::new_amount_btc("0", amount, |msg| {
            Message::Home(HomeMessage::AmountEdited(msg))
        }),
        BitcoinDisplayUnit::Sats => form::Form::new_amount_sats("0", amount, |msg| {
            Message::Home(HomeMessage::AmountEdited(msg))
        }),
    }
    .size(18)
    .padding(10);
    transfer_dialog(
        ctx,
        ctx.index(TransferPage::Amount),
        other_cube_context(ctx, true, false),
        Some(Message::Home(HomeMessage::BackToHome)),
        Column::new()
            .spacing(20)
            .push(page_heading(
                "Destination and amount".into(),
                Some(format!(
                    "From {} · Available {}",
                    source.label(),
                    bitcoin_text(ctx.balance(source), ctx.unit, ctx.balance_masked)
                )),
            ))
            .push(destination)
            .push(group_rule())
            .push(
                Column::new()
                    .spacing(6)
                    .push(text("Amount").bold())
                    .push(
                        Row::new()
                            .spacing(10)
                            .align_y(Alignment::Center)
                            .push(Container::new(input).width(Length::Fixed(260.0)))
                            .push(unit_switch(ctx.unit)),
                    )
                    .push_maybe(estimate.map(muted))
                    .push(muted(limits)),
            )
            .push(page_actions(
                Some(back_button(
                    "Back",
                    Some(Message::Home(HomeMessage::PreviousStep)),
                )),
                dialog_button(
                    "Prepare",
                    None,
                    true,
                    (parsed.is_some()).then_some(Message::Home(HomeMessage::NextStep)),
                ),
            )),
    )
}

/// The route a direction takes, for Review.
fn route_text(direction: TransferDirection) -> &'static str {
    match direction {
        TransferDirection::LiquidToVault | TransferDirection::LiquidToSpark => {
            "Liquid → Bitcoin swap, then on-chain"
        }
        TransferDirection::VaultToLiquid | TransferDirection::SparkToLiquid => {
            "Bitcoin on-chain, then swap to Liquid"
        }
        TransferDirection::VaultToSpark
        | TransferDirection::SparkToVault
        | TransferDirection::VaultToOtherCube => "Bitcoin on-chain",
    }
}

/// One row of P-Transfer's `.dl`: a 180px muted label, then the value.
fn dl_row<'a>(
    label: &'static str,
    value: impl Into<Element<'a, Message>>,
    total: bool,
) -> Element<'a, Message> {
    Column::new()
        .push(
            Row::new()
                .spacing(16)
                .padding([12, 0])
                .push(
                    Container::new(if total {
                        text(label).bold()
                    } else {
                        text(label).style(theme::text::secondary)
                    })
                    .width(Length::Fixed(180.0)),
                )
                .push(Container::new(value).width(Length::Fill)),
        )
        .push(iced::widget::rule::horizontal(1))
        .into()
}

fn description_list<'a>(rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut list = Column::new().push(iced::widget::rule::horizontal(1));
    for row in rows {
        list = list.push(row);
    }
    list.into()
}

/// The full destination address with a copy control. For a wallet of this
/// Cube its label and QR code sit behind a disclosure.
fn receiving_address<'a>(
    cube_id: &str,
    addr: &'a coincube_core::miniscript::bitcoin::Address,
    labels: &'a std::collections::HashMap<String, String>,
    labels_editing: &'a std::collections::HashMap<String, form::Value<String>>,
    expanded: bool,
    details: bool,
) -> Element<'a, Message> {
    let addr_str = addr.to_string();
    let address = Container::new(
        Row::new()
            .spacing(12)
            .align_y(Alignment::Center)
            .push(
                coincube_ui::component::tenshu::mono(addr_str.clone())
                    .size(P1_SIZE)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                    .width(Length::Fill),
            )
            .push(
                Button::new(clipboard_icon().style(theme::text::secondary))
                    .padding(6)
                    .style(theme::button::transparent_border)
                    .on_press(Message::CopyWithToast(
                        addr_str.clone(),
                        "Address copied".to_string(),
                    )),
            ),
    )
    .padding([12, 16])
    .width(Length::Fill)
    .style(review_panel_style);

    let disclosure = details.then(|| {
        let editor = Column::new()
            .spacing(10)
            .push(if let Some(label) = labels_editing.get(&addr_str) {
                crate::app::view::vault::label::label_editing(
                    vec![addr_str.clone()],
                    label,
                    P1_SIZE,
                )
            } else {
                crate::app::view::vault::label::label_editable(
                    vec![addr_str.clone()],
                    labels.get(&addr_str),
                    P1_SIZE,
                )
            })
            .push(button::secondary(None, "Show QR code").on_press(Message::ShowQrCode(0)));
        coincube_ui::component::tenshu::disclosure(
            landing_control_id(cube_id, "transfer", "address-details"),
            "Address details",
            expanded,
            Message::FocusAction,
        )
        .caption("Label, QR code")
        .on_toggle(|_| Message::SelectAddress(addr.clone()))
        .content(editor)
    });

    Column::new()
        .spacing(10)
        .push(text("Receiving address").bold())
        .push(address)
        .push_maybe(disclosure)
        .into()
}

/// Review: P-Transfer's summary list, the destination address, the Vault fee
/// rate where the source is the Vault, then the confirm action.
#[allow(clippy::too_many_arguments)]
fn review_page<'a>(
    ctx: &TransferCtx<'a>,
    amount: &'a form::Value<String>,
    receive_address: Option<&'a coincube_core::miniscript::bitcoin::Address>,
    destination_ready: bool,
    transfer_action_ready: bool,
    labels: &'a std::collections::HashMap<String, String>,
    labels_editing: &'a std::collections::HashMap<String, form::Value<String>>,
    address_expanded: bool,
    is_sending: bool,
    is_tx_signed: bool,
    prepare_onchain_send_response: Option<&'a PreparePayOnchainResponse>,
    vault_fees: Option<Amount>,
    transfer_feerate: &'a form::Value<String>,
    transfer_feerate_loading: Option<crate::app::view::shared::feerate_picker::FeeratePreset>,
    spark_send_fee_sat: Option<u64>,
) -> Element<'a, Message> {
    let direction = ctx.direction;
    let unit = ctx.unit;
    let parsed = parse_amount(amount, unit);
    let fees = match direction {
        TransferDirection::LiquidToVault | TransferDirection::LiquidToSpark => {
            prepare_onchain_send_response.map(|response| Amount::from_sat(response.total_fees_sat))
        }
        TransferDirection::VaultToLiquid
        | TransferDirection::VaultToSpark
        | TransferDirection::VaultToOtherCube => vault_fees,
        // Spark-sourced: `spark.prepare_send` quoted a `fee_sat` at step
        // 1→2 and the state layer stashed it on `spark_send_fee_sat`.
        TransferDirection::SparkToLiquid | TransferDirection::SparkToVault => {
            spark_send_fee_sat.map(Amount::from_sat)
        }
    };
    let with_fiat = |value: Amount| -> String {
        let bitcoin = bitcoin_text(value, unit, ctx.balance_masked);
        match fiat_estimate(value, ctx.fiat_converter).filter(|_| !ctx.balance_masked) {
            Some(fiat) => format!("{bitcoin} · {fiat}"),
            None => bitcoin,
        }
    };
    let receiving = if direction.is_other_cube() {
        format!("Vault · {} · {}", ctx.other.name(), network_name(ctx.chain))
    } else {
        format!("{} · {}", direction.to_wallet(), ctx.cube_name)
    };
    let mut rows = vec![
        dl_row(
            "Sending from",
            text(format!("{} · {}", direction.from_wallet(), ctx.cube_name)),
            false,
        ),
        dl_row("Receiving wallet", text(receiving), false),
        dl_row("Route", text(route_text(direction)), false),
    ];
    if let Some(parsed) = parsed {
        rows.push(dl_row("Amount", text(with_fiat(parsed)), false));
    }
    rows.push(dl_row(
        "Fees",
        match fees {
            Some(fees) => text(bitcoin_text(fees, unit, ctx.balance_masked)),
            None => muted("Calculating…"),
        },
        false,
    ));
    if let (Some(fees), Some(parsed)) = (fees, parsed) {
        rows.push(dl_row(
            "Total",
            text(bitcoin_text(fees + parsed, unit, ctx.balance_masked)).bold(),
            true,
        ));
    }

    let destination: Element<'a, Message> = match receive_address {
        Some(addr) => receiving_address(
            ctx.cube_id,
            addr,
            labels,
            labels_editing,
            address_expanded,
            !direction.is_other_cube(),
        ),
        None => muted("Getting the receiving address…").into(),
    };

    // §Design section 9: Vault-sourced transfers build a standard on-chain
    // Bitcoin tx, so the user controls the sats/vbyte rate. Presets fetch a
    // mempool estimate and write it into the text input.
    let feerate = (direction.from_kind() == WalletKind::Vault).then(|| {
        Column::new()
            .spacing(10)
            .push(text("Fee rate (sats/vbyte)").bold())
            .push(
                crate::app::view::shared::feerate_picker::feerate_presets_row::<Message>(
                    transfer_feerate_loading,
                    |preset| Message::Home(HomeMessage::FetchTransferFeeratePreset(preset)),
                    is_tx_signed || is_sending,
                ),
            )
            .push(crate::app::view::shared::feerate_picker::feerate_input::<
                Message,
                _,
            >(
                transfer_feerate,
                |s| Message::Home(HomeMessage::SetTransferFeerate(s)),
                is_tx_signed || is_sending,
            ))
    });

    let primary = match direction {
        // Vault-sourced: sign the on-chain tx first, then broadcast. Shared
        // signer path handles Liquid, Spark and another Cube's Vault.
        TransferDirection::VaultToLiquid
        | TransferDirection::VaultToSpark
        | TransferDirection::VaultToOtherCube => {
            if is_tx_signed {
                button::primary_loading(
                    None,
                    "Confirm transfer",
                    is_sending,
                    transfer_action_ready.then_some(Message::Home(HomeMessage::ConfirmTransfer)),
                )
            } else {
                button::primary_loading(
                    None,
                    "Sign transaction",
                    is_sending,
                    (transfer_feerate.valid && destination_ready)
                        .then_some(Message::Home(HomeMessage::SignVaultToLiquidTx)),
                )
            }
        }
        TransferDirection::LiquidToVault | TransferDirection::LiquidToSpark => {
            button::primary_loading(
                None,
                "Confirm transfer",
                is_sending,
                transfer_action_ready.then_some(Message::Home(HomeMessage::ConfirmTransfer)),
            )
        }
        // Spark-sourced: the prepare handle was fetched at step 1→2. Confirm
        // calls `spark.send_payment(handle)` synchronously from the UI's
        // perspective; stage flips to PendingDeposit on success.
        TransferDirection::SparkToLiquid | TransferDirection::SparkToVault => {
            button::primary_loading(
                None,
                "Confirm transfer",
                is_sending,
                transfer_action_ready.then_some(Message::Home(HomeMessage::ConfirmSparkSend)),
            )
        }
    };

    transfer_dialog(
        ctx,
        ctx.review_index(),
        other_cube_context(ctx, true, false),
        (!is_sending).then_some(Message::Home(HomeMessage::BackToHome)),
        Column::new()
            .spacing(20)
            .push(page_heading(
                "Review and confirm".into(),
                Some("Nothing moves until you confirm.".into()),
            ))
            .push(description_list(rows))
            .push(destination)
            .push_maybe(feerate)
            .push_maybe(direction.is_other_cube().then(|| {
                status_line(
                    lock_icon(),
                    StatusTone::Muted,
                    format!(
                        "{} locks again once sent, or if you leave.",
                        ctx.other.name()
                    ),
                )
            }))
            .push(page_actions(
                Some(back_button(
                    "Back",
                    (!is_sending).then_some(Message::Home(HomeMessage::PreviousStep)),
                )),
                primary.width(Length::Fixed(200.0)).into(),
            )),
    )
}

/// Done: the transfer was sent and is pending at its destination.
///
/// Every transfer direction settles via an on-chain hop, so no destination is
/// instant — the broadcast landing is the most the user can know synchronously.
/// The home-page pending indicator on the destination wallet picks up from here
/// and clears asynchronously once the destination's confirmation requirements
/// are met.
fn done_page<'a>(ctx: &TransferCtx<'a>, amount: &'a form::Value<String>) -> Element<'a, Message> {
    let direction = ctx.direction;
    let to = if direction.is_other_cube() {
        format!("Vault · {}", ctx.other.name())
    } else {
        format!("{} · {}", direction.to_wallet(), ctx.cube_name)
    };
    let mut rows = vec![dl_row(
        "Status",
        Row::new()
            .spacing(4)
            .align_y(Alignment::Center)
            .push(status_line(
                clock_icon(),
                StatusTone::Warn,
                "Pending".into(),
            ))
            .push(muted("· you can leave this page")),
        false,
    )];
    if let Some(parsed) = parse_amount(amount, ctx.unit) {
        rows.push(dl_row(
            "Amount",
            text(bitcoin_text(parsed, ctx.unit, ctx.balance_masked)),
            false,
        ));
    }
    rows.push(dl_row("To", text(to), false));
    transfer_dialog(
        ctx,
        ctx.steps().len(),
        other_cube_context(ctx, false, true),
        Some(Message::Home(HomeMessage::BackToHome)),
        Column::new()
            .spacing(20)
            .push(
                Row::new()
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .push(check_circle_icon().size(28).style(|t: &theme::Theme| {
                        iced::widget::text::Style {
                            color: Some(t.colors.shell.good),
                        }
                    }))
                    .push(text("Transfer sent").size(18).bold()),
            )
            .push(description_list(rows))
            .push(
                Row::new()
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .push(dialog_button(
                        "See it in Activity",
                        None,
                        true,
                        Some(Message::Home(HomeMessage::OpenTransferActivity)),
                    ))
                    .push(dialog_button(
                        "Back to Overview",
                        None,
                        false,
                        Some(Message::Home(HomeMessage::BackToHome)),
                    ))
                    .push(Space::new().width(Length::Fill))
                    .push(dialog_button(
                        "New transfer",
                        None,
                        false,
                        Some(Message::Home(HomeMessage::RestartTransfer)),
                    )),
            ),
    )
}

/// One of the three on-dashboard wallets. Used to address the source and destination
/// of a transfer independently of which specific `TransferDirection` is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletKind {
    Liquid,
    Spark,
    Vault,
}

impl WalletKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Liquid => "Liquid",
            Self::Spark => "Spark",
            Self::Vault => "Vault",
        }
    }

    /// The uppercase network badge rendered in the wallet picker rows (matching the
    /// "LIQUID"/"BITCOIN" badges used by the Liquid Send picker).
    pub fn badge(&self) -> &'static str {
        match self {
            Self::Liquid => "LIQUID",
            Self::Spark => "SPARK",
            Self::Vault => "VAULT",
        }
    }
}

/// Why a wallet-to-wallet transfer couldn't go through.
///
/// The transfer flow spans three wallets and six directions, and its ~20
/// failure points used to funnel into one `String`. That left the screen with
/// two bad options: echo the underlying SDK trace, or — once that was
/// sanitised — say "Transfer failed" to everyone, whether their Vault simply
/// wasn't open or a broadcast had just been refused.
///
/// The distinction that matters most to someone moving money is **whether
/// anything was sent**. Every variant sits on one side of that line and
/// [`TransferError::failure_point`] is what says which, so the copy can promise "your
/// funds haven't moved" only where that is actually true.
///
/// Each variant carries the technical `detail` for the log. None of it is
/// rendered — `crate::user_error` turns the variant into copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferError {
    /// A wallet this transfer needs isn't open or configured in this Cube.
    /// Reached by reconciling state the dashboard thought it had, so it is a
    /// dead end rather than something to retry.
    WalletUnavailable(WalletKind),
    /// The receiving wallet couldn't issue a deposit address.
    NoDepositAddress { to: WalletKind, detail: String },
    /// Working out the network fee, or fetching the swap limits that bound it,
    /// failed. Nothing has been prepared, let alone sent.
    FeeEstimateFailed { detail: String },
    /// The fee-rate field doesn't hold a usable value. Reachable only by
    /// bypassing the form's own validation.
    InvalidFeerate,
    /// The prepared send is gone: Spark's prepare handle is single-use, and a
    /// failed attempt consumes it. The user has to go back and re-enter the
    /// amount, which is a different instruction from "try again".
    PreparationExpired,
    /// The address the prepare step returned isn't valid for the active
    /// network. An internal invariant violation, not anything the user did or
    /// can fix — so the copy points at support rather than at a retry.
    DestinationRejected { detail: String },
    /// The send was submitted and refused. The `from` wallet is the one whose
    /// funds were being moved.
    BroadcastFailed { from: WalletKind, detail: String },
}

/// How far a transfer got before it failed — the only thing a person moving
/// money reliably wants to know first.
///
/// Distinct from [`TransferStage`], which tracks a *successful* transfer's
/// progress towards settlement. This one only ever describes a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferFailurePoint {
    /// Nothing was submitted. The user's balances are untouched, and saying so
    /// is the most useful sentence on the screen.
    BeforeSending,
    /// A send was submitted to a wallet or swap service and refused. Funds
    /// were not moved, but we stop short of the same flat reassurance: the
    /// request did reach a backend.
    WhileSending,
}

impl TransferError {
    pub fn failure_point(&self) -> TransferFailurePoint {
        match self {
            Self::BroadcastFailed { .. } => TransferFailurePoint::WhileSending,
            Self::WalletUnavailable(_)
            | Self::NoDepositAddress { .. }
            | Self::FeeEstimateFailed { .. }
            | Self::InvalidFeerate
            | Self::PreparationExpired
            | Self::DestinationRejected { .. } => TransferFailurePoint::BeforeSending,
        }
    }

    /// The technical cause, for the log only.
    pub fn detail(&self) -> String {
        match self {
            Self::WalletUnavailable(w) => format!("{} wallet unavailable", w.label()),
            Self::NoDepositAddress { to, detail } => {
                format!("no deposit address from {}: {}", to.label(), detail)
            }
            Self::FeeEstimateFailed { detail } => format!("fee estimate: {detail}"),
            Self::InvalidFeerate => "feerate field did not parse".to_string(),
            Self::PreparationExpired => "prepared send handle missing or consumed".to_string(),
            Self::DestinationRejected { detail } => {
                format!("prepared destination rejected: {detail}")
            }
            Self::BroadcastFailed { from, detail } => {
                format!("broadcast from {}: {}", from.label(), detail)
            }
        }
    }
}

/// Which side of the From/To transfer pair is being edited in the wallet picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerSide {
    From,
    To,
}

/// An ordered (from, to) pair of wallets the user can transfer between.
///
/// Exhaustive over the six legal directed pairs of the three wallets. Using an explicit
/// enum over a `struct { from: WalletKind, to: WalletKind }` gives compiler-driven
/// exhaustiveness in match arms and rules out the illegal `from == to` state at compile
/// time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferDirection {
    LiquidToVault,
    VaultToLiquid,
    LiquidToSpark,
    SparkToLiquid,
    VaultToSpark,
    SparkToVault,
    /// This Cube's Vault to the Vault of another local Bitcoin mainnet Cube,
    /// unlocked in place for this transfer only. Never built from a wallet
    /// pair: the To list's "Another Cube…" row selects it.
    VaultToOtherCube,
}

impl TransferDirection {
    pub fn pair(&self) -> (WalletKind, WalletKind) {
        match self {
            Self::LiquidToVault => (WalletKind::Liquid, WalletKind::Vault),
            Self::VaultToLiquid => (WalletKind::Vault, WalletKind::Liquid),
            Self::LiquidToSpark => (WalletKind::Liquid, WalletKind::Spark),
            Self::SparkToLiquid => (WalletKind::Spark, WalletKind::Liquid),
            Self::VaultToSpark => (WalletKind::Vault, WalletKind::Spark),
            Self::SparkToVault => (WalletKind::Spark, WalletKind::Vault),
            Self::VaultToOtherCube => (WalletKind::Vault, WalletKind::Vault),
        }
    }

    pub fn is_other_cube(&self) -> bool {
        matches!(self, Self::VaultToOtherCube)
    }

    pub fn from_kind(&self) -> WalletKind {
        self.pair().0
    }

    pub fn to_kind(&self) -> WalletKind {
        self.pair().1
    }

    /// Returns `None` if `from == to` (illegal — same wallet on both sides).
    /// Availability of each wallet on the current cube is the caller's concern.
    pub fn try_from_pair(from: WalletKind, to: WalletKind) -> Option<Self> {
        Some(match (from, to) {
            (WalletKind::Liquid, WalletKind::Vault) => Self::LiquidToVault,
            (WalletKind::Vault, WalletKind::Liquid) => Self::VaultToLiquid,
            (WalletKind::Liquid, WalletKind::Spark) => Self::LiquidToSpark,
            (WalletKind::Spark, WalletKind::Liquid) => Self::SparkToLiquid,
            (WalletKind::Vault, WalletKind::Spark) => Self::VaultToSpark,
            (WalletKind::Spark, WalletKind::Vault) => Self::SparkToVault,
            _ => return None,
        })
    }

    pub fn from_wallet(&self) -> &'static str {
        self.from_kind().label()
    }

    pub fn to_wallet(&self) -> &'static str {
        self.to_kind().label()
    }

    pub fn display(&self) -> String {
        if self.is_other_cube() {
            return format!("{} → another Cube", self.from_wallet());
        }
        format!("{} → {}", self.from_wallet(), self.to_wallet())
    }
}

pub struct GlobalViewConfig<'a> {
    pub cube_id: &'a str,
    pub liquid_balance: Amount,
    /// Spark wallet BTC balance in sats. Defaults to `ZERO` while
    /// the first `get_info` round-trip is in flight or if the
    /// bridge subprocess is currently down.
    pub spark_balance: Amount,
    pub spark_btc_balance: Amount,
    pub spark_stable_balance: Option<&'a coincube_spark_protocol::StableBalanceSnapshot>,
    pub spark_balance_error: bool,
    pub usdt_balance: u64,
    pub usdt_balance_error: bool,
    pub pending_liquid_send_sats: u64,
    pub pending_usdt_send_sats: u64,
    pub pending_liquid_receive_sats: u64,
    pub pending_usdt_receive_sats: u64,
    pub vault_pending_send_sats: u64,
    pub vault_pending_receive_sats: u64,
    pub vault_balance: Amount,
    pub imported_balance: Option<Amount>,
    pub imported_installed: bool,
    pub fiat_converter: Option<FiatAmountConverter>,
    pub balance_masked: bool,
    /// Session-local Overview section choices; global Hide overrides both.
    pub overview_total_hidden: bool,
    pub overview_wallets_hidden: bool,
    /// True until every applicable wallet has reported its first balance
    /// (or its first error). Drives the Total Balance loading placeholder
    /// so the aggregate doesn't briefly read as a misleadingly low partial.
    pub total_balance_loading: bool,
    /// True once the bridge has corroborated the Spark balance via a
    /// `Synced` event. While false the Spark card renders a `…`
    /// placeholder — avoids flashing a stale pre-sync value (e.g. zero
    /// before the session's incoming payments have landed).
    pub spark_balance_loaded: bool,
    /// True once the Liquid bridge has reported its first L-BTC
    /// balance. Drives the L-BTC row's animated-dots placeholder.
    pub liquid_balance_loaded: bool,
    /// A balance fetch failed, so the row shows "Balance unavailable" rather
    /// than the zero it never managed to overwrite.
    pub liquid_balance_error: bool,
    /// True once the Liquid bridge has reported USDt balance (or an
    /// explicit error, captured in `usdt_balance_error`). Drives the
    /// USDt row's animated-dots placeholder.
    pub usdt_balance_loaded: bool,
    /// True once the Vault daemon has produced its first cache poll
    /// (`blockheight > 0`). Drives the Vault card's animated-dots
    /// placeholder and suppresses pending indicators until the
    /// underlying balance is authoritative.
    pub vault_loaded: bool,
    /// Global fiat-native vs. bitcoin-native display preference. Mirrored
    /// from `cache.display_mode`.
    pub display_mode: crate::app::settings::display::DisplayMode,
    pub has_vault: bool,
    pub has_imported_bitcoin: bool,
    /// Whether this cube has a working Spark backend. Mirrors
    /// `spark_backend.is_some()` in the state layer. Drives the Spark card
    /// visibility and feeds [`transfer_available`].
    pub has_spark: bool,
    /// Whether the Liquid wallet is shown at all — the sunset gate AND a
    /// Liquid backend on the current network
    /// ([`features::liquid_wallet_usable`](crate::app::features::liquid_wallet_usable)).
    /// Drives the Liquid card, the Liquid row in the wallet picker, and
    /// Liquid's contribution to the Total Balance. Liquid used to be
    /// unconditional on every cube; it no longer is.
    pub has_liquid: bool,
    pub current_view: HomeView,
    pub transfer_direction: Option<TransferDirection>,
    pub transfer_from: Option<WalletKind>,
    pub transfer_to: Option<WalletKind>,
    /// When `Some`, the wallet-picker popup is open and editing the named side.
    pub wallet_picker: Option<PickerSide>,
    pub entered_amount: &'a form::Value<String>,
    pub receive_address: Option<&'a coincube_core::miniscript::bitcoin::Address>,
    /// True only when the destination was returned for the current transfer request.
    pub destination_ready: bool,
    /// Current destination plus the direction-specific prepared response/handle.
    pub transfer_action_ready: bool,
    pub receive_index: Option<&'a coincube_core::miniscript::bitcoin::bip32::ChildNumber>,
    pub labels: &'a std::collections::HashMap<String, String>,
    pub labels_editing: &'a std::collections::HashMap<String, form::Value<String>>,
    pub address_expanded: bool,
    pub bitcoin_unit: crate::app::settings::unit::BitcoinDisplayUnit,
    pub onchain_send_limit: Option<(u64, u64)>,
    pub onchain_receive_limit: Option<(u64, u64)>,
    pub is_sending: bool,
    pub is_tx_signed: bool,
    pub prepare_onchain_send_response: Option<&'a PreparePayOnchainResponse>,
    pub spend_tx_fees: Option<Amount>,
    /// Vault-sourced transfers expose a sats/vbyte input on the confirm screen.
    /// Ignored on all other directions (SDK picks the fee).
    pub transfer_feerate: &'a form::Value<String>,
    /// Which Fast/Normal/Slow preset is currently fetching a mempool-driven
    /// estimate. The button for the loading preset renders non-pressable.
    pub transfer_feerate_loading: Option<crate::app::view::shared::feerate_picker::FeeratePreset>,
    /// Spark-quoted on-chain fee for the prepared send (Spark-sourced
    /// directions only). Rendered in the Fees row on the confirm screen.
    pub spark_send_fee_sat: Option<u64>,
    /// Mirror of `pending_vault_incoming` for the Spark card. Populated by the
    /// state layer when a transfer into Spark has broadcast on-chain and is
    /// awaiting maturity + claim on the Spark side.
    pub pending_spark_incoming: Option<PendingTransfer>,
    pub pending_vault_incoming: Option<PendingTransfer>,
    /// BTC price in USD for accurate USDt→sats conversion regardless of user's fiat currency.
    pub btc_usd_price: Option<f64>,
    /// This Cube's name and chain, for Overview and the transfer dialog.
    pub cube_name: &'a str,
    pub vault_alias: Option<&'a str>,
    pub chain: ChainId,
    pub light_theme: bool,
    /// Which page step 1 of the transfer shows.
    pub transfer_page: TransferPage,
    /// The unit the transfer amount is entered and shown in.
    pub transfer_unit: BitcoinDisplayUnit,
    pub other_cube: OtherCubeView<'a>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HomeView {
    pub step: usize,
}

impl HomeView {
    pub fn next(&mut self) {
        self.step += 1;
    }

    pub fn previous(&mut self) {
        if self.step > 0 {
            self.step -= 1;
        }
    }

    pub fn reset(&mut self) {
        self.step = 0;
    }
}

/// The Overview, and the transfer dialog to lay over it while a transfer step
/// (1–3) is active.
pub fn global_home_view<'a>(
    config: GlobalViewConfig<'a>,
) -> (Element<'a, Message>, Option<Element<'a, Message>>) {
    let GlobalViewConfig {
        cube_id,
        liquid_balance,
        spark_balance,
        spark_btc_balance,
        spark_stable_balance,
        spark_balance_error,
        usdt_balance,
        usdt_balance_error,
        pending_liquid_send_sats,
        pending_usdt_send_sats,
        pending_liquid_receive_sats,
        pending_usdt_receive_sats,
        vault_pending_send_sats,
        vault_pending_receive_sats,
        vault_balance,
        imported_balance,
        imported_installed,
        fiat_converter,
        balance_masked,
        overview_total_hidden,
        overview_wallets_hidden,
        total_balance_loading,
        spark_balance_loaded,
        liquid_balance_loaded,
        liquid_balance_error,
        usdt_balance_loaded,
        vault_loaded,
        display_mode,
        has_vault,
        has_imported_bitcoin,
        has_spark,
        has_liquid,
        current_view,
        transfer_direction,
        transfer_from: _,
        transfer_to: _,
        wallet_picker: _,
        entered_amount,
        receive_address,
        destination_ready,
        transfer_action_ready,
        receive_index: _receive_index,
        labels,
        labels_editing,
        address_expanded,
        bitcoin_unit,
        onchain_send_limit,
        onchain_receive_limit,
        is_sending,
        is_tx_signed,
        prepare_onchain_send_response,
        spend_tx_fees,
        transfer_feerate,
        transfer_feerate_loading,
        spark_send_fee_sat,
        pending_vault_incoming,
        pending_spark_incoming,
        btc_usd_price,
        cube_name,
        vault_alias,
        chain,
        light_theme,
        transfer_page,
        transfer_unit,
        other_cube,
    } = config;

    let overview_masks = OverviewMasks::new(
        balance_masked,
        overview_total_hidden,
        overview_wallets_hidden,
    );
    let wallet_balance_masked = overview_masks.wallets;
    let total_balance_masked = overview_masks.total;

    // Step machine:
    //   0 — overview (wallet cards + page-level Transfer button)
    //   1 — Wallets, then Amount (and, to another Cube, Other Cube and
    //       Unlock in between): see `TransferPage`
    //   2 — Review
    //   3 — Done
    // Steps 1–3 are a dialog the state layer lays over the overview.
    let _ = pending_vault_incoming;
    let transfer = transfer_direction.and_then(|direction| {
        let ctx = TransferCtx {
            cube_id,
            cube_name,
            chain,
            light: light_theme,
            direction,
            has_vault,
            has_spark,
            has_liquid,
            liquid_balance,
            spark_balance,
            vault_balance,
            fiat_converter,
            unit: transfer_unit,
            balance_masked,
            other: other_cube,
        };
        match current_view.step {
            1 => Some(match transfer_page {
                TransferPage::Wallets => wallets_page(&ctx),
                TransferPage::OtherCube => other_cube_page(&ctx),
                TransferPage::Unlock => unlock_page(&ctx),
                TransferPage::Amount => amount_page(
                    &ctx,
                    entered_amount,
                    onchain_send_limit,
                    onchain_receive_limit,
                ),
            }),
            2 => Some(review_page(
                &ctx,
                entered_amount,
                receive_address,
                destination_ready,
                transfer_action_ready,
                labels,
                labels_editing,
                address_expanded,
                is_sending,
                is_tx_signed,
                prepare_onchain_send_response,
                spend_tx_fees,
                transfer_feerate,
                transfer_feerate_loading,
                spark_send_fee_sat,
            )),
            3 => Some(done_page(&ctx, entered_amount)),
            _ => None,
        }
    });

    // --- Combined Liquid card (L-BTC + USDt) ---
    // USDt is pegged to USD, so always use BTC/USD price for conversion.
    // When the user's fiat currency is USD, btc_usd_price == converter.price_per_btc().
    // When it differs (e.g. EUR), we must use the dedicated USD price to avoid mispricing.
    let usable_usd_quote = btc_usd_price.filter(|price| *price > 0.0 && price.is_finite());
    let usdt_fiat_value = usdt_balance as f64 / 1e8; // USDt base units → dollars
    let usdt_as_sats = usable_usd_quote
        .map(|btc_price_usd| (usdt_fiat_value / btc_price_usd * 1e8) as u64)
        .unwrap_or(0);
    // A gated-off Liquid wallet contributes nothing to the aggregate. Its
    // balances are zero in that state anyway (the state layer skips the
    // loaders), but stating it here keeps the Total Balance honest even if a
    // stale value ever survived the gate flipping.
    let total_sats = if has_liquid {
        liquid_balance.to_sat().saturating_add(usdt_as_sats)
    } else {
        0
    };
    let total_amount = Amount::from_sat(total_sats);
    let total_fiat = fiat_converter.as_ref().map(|c| c.convert(total_amount));
    let lbtc_fiat = fiat_converter.as_ref().map(|c| c.convert(liquid_balance));

    // Global Total Balance: aggregate across Spark + Liquid (L-BTC + USDt-as-sats) + Vault.
    // Callers pass `has_spark`/`has_vault` to gate visibility, but zero-balance
    // default values for absent wallets don't skew the total.
    // An imported Vault can report coins before its bounded history search is
    // complete. Those coins are not an authoritative balance yet.
    let imported_balance_loaded =
        has_imported_bitcoin && imported_installed && imported_balance.is_some();
    let display_vault_balance = if has_imported_bitcoin && !imported_balance_loaded {
        Amount::ZERO
    } else {
        vault_balance
    };
    // A bridge response received before Synced can contain persisted values
    // from the prior session. Neither BTC nor USDB may enter the aggregate or
    // read as a holding until the existing corroboration gate opens.
    let trusted_spark_stable_balance = spark_stable_balance.filter(|_| spark_balance_loaded);
    let spark_usdb_held = trusted_spark_stable_balance.is_some_and(|holding| holding.balance > 0);
    let spark_usdb_as_sats = trusted_spark_stable_balance
        .map(|holding| {
            crate::app::breez_spark::assets::stable_token_as_sats(
                holding.balance,
                holding.decimals,
                usable_usd_quote,
            )
        })
        .unwrap_or(0);
    let spark_display_balance = Amount::from_sat(if has_spark && spark_balance_loaded {
        spark_btc_balance
            .to_sat()
            .saturating_add(spark_usdb_as_sats)
    } else {
        0
    });
    let spark_valuation_partial = spark_usdb_held && usable_usd_quote.is_none();
    let global_total_sats = spark_display_balance
        .to_sat()
        .saturating_add(total_sats)
        .saturating_add(display_vault_balance.to_sat());
    let global_total_amount = Amount::from_sat(global_total_sats);
    let global_total_fiat = fiat_converter
        .as_ref()
        .map(|c| c.convert(global_total_amount));
    let total_partial = (has_spark && (spark_balance_error || spark_valuation_partial))
        || (has_liquid
            && (liquid_balance_error
                || usdt_balance_error
                || (usdt_balance > 0 && usable_usd_quote.is_none())));

    let orange_outline_btn =
        |wallet: &'static str, control: &'static str, msg: Message| -> Element<'a, Message> {
            landing_button(
                landing_control_id(cube_id, wallet, control),
                "Receive",
                None,
                Message::FocusAction,
                Some(msg),
                theme::button::orange_outline,
            )
            .width(Length::Fixed(90.0))
            .into()
        };

    // L-BTC asset row
    let lbtc_row = Column::new()
        .spacing(4)
        .push(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(coincube_ui::image::asset_network_logo::<Message>(
                    "lbtc", "liquid", 32.0,
                ))
                .push(
                    text("L-BTC")
                        .size(P1_SIZE)
                        .bold()
                        .width(Length::Fixed(60.0)),
                )
                .push(if wallet_balance_masked {
                    Row::new().push(text("********").size(P1_SIZE))
                } else if liquid_balance_error {
                    Row::new().push(text("Balance unavailable").size(P1_SIZE).color(color::RED))
                } else if !liquid_balance_loaded {
                    Row::new().push(spinner::typing_text_carousel(
                        "...",
                        false,
                        Duration::from_millis(400),
                        |content| text(content).size(P1_SIZE).style(theme::text::secondary),
                    ))
                } else {
                    amount_with_size_and_unit(&liquid_balance, P1_SIZE, bitcoin_unit)
                })
                .push_maybe(
                    (!wallet_balance_masked && liquid_balance_loaded && !liquid_balance_error)
                        .then(|| {
                            lbtc_fiat
                                .map(|f| f.to_text().size(P2_SIZE).style(theme::text::secondary))
                        })
                        .flatten(),
                )
                .push(Space::new().width(Length::Fill))
                .push(
                    landing_button(
                        landing_control_id(cube_id, "Liquid", "send-lbtc"),
                        "Send",
                        None,
                        Message::FocusAction,
                        Some(Message::Home(HomeMessage::SendAsset(
                            crate::app::state::liquid::send::SendAsset::Lbtc,
                        ))),
                        theme::button::primary,
                    )
                    .width(Length::Fixed(90.0)),
                )
                .push(orange_outline_btn(
                    "Liquid",
                    "receive-lbtc",
                    Message::Home(HomeMessage::ReceiveAsset(
                        crate::app::state::liquid::send::SendAsset::Lbtc,
                    )),
                )),
        )
        .push_maybe(
            (!wallet_balance_masked
                && liquid_balance_loaded
                && !liquid_balance_error
                && pending_liquid_send_sats > 0)
                .then(|| {
                    Row::new()
                        .spacing(6)
                        .align_y(Alignment::Center)
                        .push(warning_icon().size(12).style(theme::text::secondary))
                        .push(text("-").size(P2_SIZE).style(theme::text::secondary))
                        .push(amount_with_size_and_unit(
                            &Amount::from_sat(pending_liquid_send_sats),
                            P2_SIZE,
                            bitcoin_unit,
                        ))
                        .push(text("pending").size(P2_SIZE).style(theme::text::secondary))
                }),
        )
        .push_maybe(
            (!wallet_balance_masked
                && liquid_balance_loaded
                && !liquid_balance_error
                && pending_liquid_receive_sats > 0)
                .then(|| {
                    Row::new()
                        .spacing(6)
                        .align_y(Alignment::Center)
                        .push(warning_icon().size(12).style(theme::text::secondary))
                        .push(text("+").size(P2_SIZE).style(theme::text::secondary))
                        .push(amount_with_size_and_unit(
                            &Amount::from_sat(pending_liquid_receive_sats),
                            P2_SIZE,
                            bitcoin_unit,
                        ))
                        .push(text("pending").size(P2_SIZE).style(theme::text::secondary))
                }),
        );

    // USDt asset row
    let usdt_row = Column::new()
        .spacing(4)
        .push(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(coincube_ui::image::asset_network_logo::<Message>(
                    "usdt", "liquid", 32.0,
                ))
                .push(text("USDt").size(P1_SIZE).bold().width(Length::Fixed(60.0)))
                .push(if wallet_balance_masked {
                    Row::new().push(text("********").size(P1_SIZE))
                } else if usdt_balance_error {
                    Row::new().push(text("Balance unavailable").size(P1_SIZE).color(color::RED))
                } else if !usdt_balance_loaded {
                    Row::new().push(spinner::typing_text_carousel(
                        "...",
                        false,
                        Duration::from_millis(400),
                        |content| text(content).size(P1_SIZE).style(theme::text::secondary),
                    ))
                } else {
                    Row::new()
                        .spacing(6)
                        .align_y(Alignment::Center)
                        .push(text(format_usdt_display(usdt_balance)).size(P1_SIZE).bold())
                        .push(text("USDt").size(P2_SIZE).color(color::GREY_3))
                })
                .push(Space::new().width(Length::Fill))
                .push(
                    landing_button(
                        landing_control_id(cube_id, "Liquid", "send-usdt"),
                        "Send",
                        None,
                        Message::FocusAction,
                        Some(Message::Home(HomeMessage::SendAsset(
                            crate::app::state::liquid::send::SendAsset::Usdt,
                        ))),
                        theme::button::primary,
                    )
                    .width(Length::Fixed(90.0)),
                )
                .push(orange_outline_btn(
                    "Liquid",
                    "receive-usdt",
                    Message::Home(HomeMessage::ReceiveAsset(
                        crate::app::state::liquid::send::SendAsset::Usdt,
                    )),
                )),
        )
        .push_maybe(
            (!wallet_balance_masked
                && usdt_balance_loaded
                && !usdt_balance_error
                && pending_usdt_send_sats > 0)
                .then(|| {
                    Row::new()
                        .spacing(6)
                        .align_y(Alignment::Center)
                        .push(warning_icon().size(12).style(theme::text::secondary))
                        .push(
                            text(format!(
                                "-{} USDt pending",
                                format_usdt_display(pending_usdt_send_sats)
                            ))
                            .size(P2_SIZE)
                            .style(theme::text::secondary),
                        )
                }),
        )
        .push_maybe(
            (!wallet_balance_masked
                && usdt_balance_loaded
                && !usdt_balance_error
                && pending_usdt_receive_sats > 0)
                .then(|| {
                    Row::new()
                        .spacing(6)
                        .align_y(Alignment::Center)
                        .push(warning_icon().size(12).style(theme::text::secondary))
                        .push(
                            text(format!(
                                "+{} USDt pending",
                                format_usdt_display(pending_usdt_receive_sats)
                            ))
                            .size(P2_SIZE)
                            .style(theme::text::secondary),
                        )
                }),
        );

    // Combined Liquid headline (L-BTC + USDt-as-sats). Both component
    // balances must be loaded — a partial sum would be misleading.
    let liquid_card_header_loaded = liquid_balance_loaded && usdt_balance_loaded;
    let liquid_card_header: Element<'a, Message> = if liquid_card_header_loaded
        && (liquid_balance_error
            || usdt_balance_error
            || (usdt_balance > 0 && usable_usd_quote.is_none()))
    {
        amount_cell::<Message>(
            AmountCell::unavailable(
                "Combined Liquid valuation is partial; asset amounts are below",
            ),
            AmountTier::Card,
        )
    } else if liquid_card_header_loaded {
        wallet_header::<Message>(
            WalletHeaderProps::new(
                total_amount,
                total_fiat,
                wallet_balance_masked,
                bitcoin_unit,
                HeaderVariant::Card,
                display_mode,
                Some(Message::FlipDisplayMode),
            )
            .with_swap_focus(
                landing_control_id(cube_id, "Liquid", "flip-balance"),
                Message::FocusAction,
            ),
        )
        .into()
    } else {
        Column::new()
            .spacing(4)
            .push(spinner::typing_text_carousel(
                "...",
                false,
                Duration::from_millis(400),
                |content| text(content).size(H2_SIZE).bold(),
            ))
            .push(spinner::typing_text_carousel(
                "...",
                false,
                Duration::from_millis(400),
                |content| text(content).size(P1_SIZE).style(theme::text::secondary),
            ))
            .into()
    };

    let liquid_card_content = Column::new()
        .spacing(12)
        .push(landing_button(
            landing_control_id(cube_id, "Liquid", "open"),
            "Liquid",
            Some(droplet_fill_icon().size(16).style(theme::text::secondary)),
            Message::FocusAction,
            Some(Message::Menu(Menu::Liquid(LiquidSubMenu::Overview))),
            theme::button::transparent,
        ))
        .push(liquid_card_header)
        .push(lbtc_row)
        .push(usdt_row);

    let liquid_card: Element<'a, Message> = Container::new(liquid_card_content)
        .padding(20)
        .style(landing_card_style)
        .into();

    // --- Spark card (BTC row only, mirrors the Liquid layout) ---
    // Rendered above the Liquid card because Spark is the default
    // wallet for everyday Lightning UX post-Phase 5. The card only
    // surfaces when `has_spark` is true — cubes without a Spark
    // signer hide it entirely, so the entire widget tree is built
    // lazily inside `then(..)` to avoid wasted work on every render.
    let spark_card = has_spark.then(|| {
        let spark_fiat = (!spark_valuation_partial)
            .then(|| {
                fiat_converter
                    .as_ref()
                    .map(|c| c.convert(spark_display_balance))
            })
            .flatten();
        let spark_btc_fiat = fiat_converter
            .as_ref()
            .map(|c| c.convert(spark_btc_balance));
        let spark_btc_row = Row::new()
            .spacing(8)
            .align_y(Alignment::Center)
            .push(coincube_ui::image::asset_network_logo::<Message>(
                "btc", "spark", 32.0,
            ))
            .push(text("BTC").size(P1_SIZE).bold().width(Length::Fixed(60.0)))
            .push(if wallet_balance_masked {
                Row::new().push(text(AmountCell::masked().primary).size(P1_SIZE))
            } else if spark_balance_error && !spark_balance_loaded {
                Row::new().push(
                    text("Balance unavailable")
                        .size(P1_SIZE)
                        .style(theme::text::warning),
                )
            } else if !spark_balance_loaded {
                // Pre-Synced placeholder — see the comment on the
                // Spark wallet_header below for why we don't render
                // the raw value yet. Animated dots mirror the Total
                // Balance loading treatment for visual consistency.
                Row::new().push(spinner::typing_text_carousel(
                    "...",
                    false,
                    Duration::from_millis(400),
                    |content| text(content).size(P1_SIZE).style(theme::text::secondary),
                ))
            } else {
                amount_with_size_and_unit(&spark_btc_balance, P1_SIZE, bitcoin_unit)
            })
            .push_maybe(
                (!wallet_balance_masked && spark_balance_loaded)
                    .then(|| {
                        spark_btc_fiat
                            .as_ref()
                            .map(|f| f.to_text().size(P2_SIZE).style(theme::text::secondary))
                    })
                    .flatten(),
            )
            .push(Space::new().width(Length::Fill))
            .push(
                landing_button(
                    landing_control_id(cube_id, "Spark", "send-btc"),
                    "Send",
                    None,
                    Message::FocusAction,
                    Some(Message::Home(HomeMessage::SendSparkBtc)),
                    theme::button::primary,
                )
                .width(Length::Fixed(90.0)),
            )
            .push(orange_outline_btn(
                "Spark",
                "receive-btc",
                Message::Home(HomeMessage::ReceiveSparkBtc),
            ));

        // Headline balance — placeholder while the bridge hasn't
        // corroborated the Spark balance with a `Synced` event yet,
        // since the SDK can return a stale persisted value before
        // incremental sync completes. Animated dots match the Total
        // Balance loading treatment for visual consistency.
        let spark_header: Element<'a, Message> = if spark_balance_error && !spark_balance_loaded {
            amount_cell::<Message>(
                AmountCell::unavailable("Spark bridge has not returned a balance"),
                AmountTier::Card,
            )
        } else if spark_balance_loaded && spark_valuation_partial {
            amount_cell::<Message>(
                if wallet_balance_masked {
                    AmountCell::masked()
                } else {
                    AmountCell::unavailable("Combined BTC and USDB valuation needs a BTC/USD quote")
                },
                AmountTier::Card,
            )
        } else if spark_balance_loaded {
            wallet_header::<Message>(
                WalletHeaderProps::new(
                    spark_display_balance,
                    spark_fiat,
                    wallet_balance_masked,
                    bitcoin_unit,
                    HeaderVariant::Card,
                    display_mode,
                    Some(Message::FlipDisplayMode),
                )
                .with_swap_focus(
                    landing_control_id(cube_id, "Spark", "flip-balance"),
                    Message::FocusAction,
                ),
            )
            .into()
        } else {
            Column::new()
                .spacing(4)
                .push(spinner::typing_text_carousel(
                    "...",
                    false,
                    Duration::from_millis(400),
                    |content| text(content).size(H2_SIZE).bold(),
                ))
                .push(spinner::typing_text_carousel(
                    "...",
                    false,
                    Duration::from_millis(400),
                    |content| text(content).size(P1_SIZE).style(theme::text::secondary),
                ))
                .into()
        };

        let spark_card_content = Column::new()
            .spacing(12)
            .push(landing_button(
                landing_control_id(cube_id, "Spark", "open"),
                "Spark",
                Some(spark_icon().size(16).style(theme::text::secondary)),
                Message::FocusAction,
                Some(Message::Menu(Menu::Spark(
                    crate::app::menu::SparkSubMenu::Overview,
                ))),
                theme::button::transparent,
            ))
            .push(spark_header)
            .push(spark_btc_row)
            .push_maybe(spark_stable_balance.map(|holding| {
                let text_value = format!(
                    "{} USDB",
                    crate::app::breez_spark::assets::format_token_display(
                        holding.balance,
                        holding.decimals,
                    )
                );
                Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(
                        text("USDB")
                            .size(P1_SIZE)
                            .style(theme::text::secondary)
                            .width(Length::Fixed(60.0)),
                    )
                    .push(amount_cell::<Message>(
                        if wallet_balance_masked {
                            AmountCell::masked()
                        } else if spark_balance_error && !spark_balance_loaded {
                            AmountCell::unavailable("Spark bridge has not returned a balance")
                        } else if !spark_balance_loaded {
                            AmountCell::unknown("Waiting for Spark sync")
                        } else {
                            AmountCell::shown(text_value, None)
                        },
                        AmountTier::Row,
                    ))
            }))
            .push_maybe((spark_balance_error && spark_balance_loaded).then(|| {
                text("Last known Spark balance · refresh failed")
                    .size(P2_SIZE)
                    .style(theme::text::warning)
            }))
            .push_maybe(pending_spark_incoming.and_then(|pt| {
                (pt.stage != TransferStage::Completed)
                    .then(|| pending_deposit_card(pt, bitcoin_unit, wallet_balance_masked))
            }));

        let spark_card: Element<'a, Message> = Container::new(spark_card_content)
            .padding(20)
            .style(landing_card_style)
            .into();

        spark_card
    });

    let vault_card_element: Element<'a, Message> = {
        wallet_card(
            WalletType::Vault,
            cube_id,
            &display_vault_balance,
            fiat_converter,
            wallet_balance_masked,
            has_vault || (has_imported_bitcoin && imported_installed),
            has_vault,
            has_imported_bitcoin && !imported_installed,
            bitcoin_unit,
            pending_vault_incoming,
            vault_pending_send_sats,
            vault_pending_receive_sats,
            display_mode,
            if has_imported_bitcoin {
                imported_balance_loaded
            } else {
                vault_loaded
            },
            vault_alias,
        )
    };

    // Total Balance block — the fiat-first headline number above the
    // Wallets section. Before the first authoritative result, a zero is not
    // a wallet balance. A failed first read without any known component is
    // unavailable; with known components, the sum is explicitly partial.
    let any_known_balance = (has_spark && spark_balance_loaded)
        || (has_liquid
            && ((liquid_balance_loaded && !liquid_balance_error)
                || (usdt_balance_loaded && !usdt_balance_error)))
        || imported_balance_loaded
        || (has_vault && !has_imported_bitcoin && vault_loaded);
    let total_balance_label_row = Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(h3("Total Balance").bold())
        .push(overview_eye(
            cube_id,
            "Total Balance",
            overview_masks.total,
            balance_masked,
            HomeMessage::ToggleTotalBalance,
        ))
        .push_maybe(
            (total_partial && !total_balance_loading)
                .then(|| warning_icon().size(12).style(theme::text::secondary)),
        );

    let total_balance_value: Element<'a, Message> = if total_balance_loading {
        Column::new()
            .spacing(4)
            .push(spinner::typing_text_carousel(
                "...",
                false,
                Duration::from_millis(400),
                |content| text(content).size(H1_SIZE).bold(),
            ))
            .push(spinner::typing_text_carousel(
                "...",
                false,
                Duration::from_millis(400),
                |content| text(content).size(P1_SIZE).style(theme::text::secondary),
            ))
            .into()
    } else if !any_known_balance {
        amount_cell::<Message>(
            if total_balance_masked {
                AmountCell::masked()
            } else {
                AmountCell::unavailable("No wallet balance has loaded successfully")
            },
            AmountTier::Total,
        )
    } else {
        wallet_header::<Message>(
            WalletHeaderProps::new(
                global_total_amount,
                global_total_fiat,
                total_balance_masked,
                bitcoin_unit,
                HeaderVariant::Jumbo,
                display_mode,
                Some(Message::FlipDisplayMode),
            )
            .with_swap_focus(
                landing_control_id(cube_id, "Hub", "total-flip-balance"),
                Message::FocusAction,
            ),
        )
        .into()
    };

    let total_balance_block = Column::new()
        .spacing(6)
        .push(total_balance_label_row)
        .push(
            Row::new()
                .spacing(6)
                .align_y(Alignment::Center)
                .push(network_coin(chain, light_theme))
                .push(
                    text(format!("{cube_name} · {}", network_name(chain)))
                        .size(P2_SIZE)
                        .style(theme::text::secondary),
                ),
        )
        .push(total_balance_value)
        .push_maybe(
            (total_partial && !total_balance_loading && !total_balance_masked).then(|| {
                text("Partial total · one or more wallet amounts or USD valuations are unavailable")
                    .size(P2_SIZE)
                    .style(theme::text::warning)
            }),
        );

    let overview = Container::new(
        Column::new()
            .spacing(12)
            .push(total_balance_block)
            .push(Space::new().height(Length::Fixed(20.0)))
            .push(
                Row::new()
                    .spacing(8)
                    .width(Length::Fill)
                    .push(h3("Wallets").bold())
                    .push(overview_eye(
                        cube_id,
                        "Wallets",
                        overview_masks.wallets,
                        balance_masked,
                        HomeMessage::ToggleWalletBalances,
                    ))
                    .align_y(Alignment::Center),
            )
            .push_maybe(spark_card)
            .push_maybe(has_liquid.then_some(liquid_card))
            .push(vault_card_element)
            .push_maybe(
                transfer_available(has_liquid, has_vault, has_spark).then(|| {
                    // Disable Transfer while any wallet's balance hasn't been
                    // corroborated yet. The amount-entry view and wallet picker
                    // both consume the raw balance values to render available-
                    // funds hints and to cap validation; opening Transfer
                    // pre-load would surface a misleading zero / stale cap for
                    // whichever wallet hasn't finished syncing.
                    Container::new(
                        landing_button(
                            landing_control_id(cube_id, "Hub", "transfer"),
                            "Transfer",
                            Some(arrow_down_up_icon()),
                            Message::FocusAction,
                            transfer_balance_ready(
                                total_balance_loading,
                                has_spark,
                                spark_balance_loaded,
                            )
                            .then_some(Message::Home(HomeMessage::NextStep)),
                            |_t, _s| iced::widget::button::Style {
                                text_color: color::ORANGE,
                                border: iced::Border {
                                    color: color::ORANGE,
                                    width: 1.0,
                                    radius: 8.0.into(),
                                },
                                background: Some(iced::Background::Color(
                                    _t.colors.cards.simple.background,
                                )),
                                ..Default::default()
                            },
                        )
                        .width(Length::Fixed(150.0)),
                    )
                    .width(Length::Fill)
                    .center_x(Length::Fill)
                }),
            )
            .push_maybe(
                transfer_available(has_liquid, has_vault, has_spark)
                    .then(|| Space::new().height(Length::Fixed(30.0))),
            ),
    )
    .padding([0, 16])
    .into();

    (overview, transfer)
}

/// Gate for the page-level Transfer button: a transfer needs two wallets to
/// move funds between, so at least two of the three must be present.
///
/// This used to be `has_vault || has_spark`, on the assumption that Liquid was
/// present on every cube and so only a *second* wallet was in question. The
/// Liquid sunset gate breaks that assumption — a new cube can now have Spark
/// alone, which has nothing to transfer with — so the count is now explicit.
/// Aggregate failure may end loading for display while an installed Spark
/// wallet still has no authoritative funds for the transfer picker or cap.
pub(crate) fn transfer_balance_ready(
    total_balance_loading: bool,
    has_spark: bool,
    spark_balance_loaded: bool,
) -> bool {
    !total_balance_loading && (!has_spark || spark_balance_loaded)
}

fn transfer_available(has_liquid: bool, has_vault: bool, has_spark: bool) -> bool {
    [has_liquid, has_vault, has_spark]
        .iter()
        .filter(|present| **present)
        .count()
        >= 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::view::shared::landing::capture;

    /// The Overview a step renders beneath any transfer dialog.
    fn overview(config: GlobalViewConfig<'_>) -> Element<'_, Message> {
        global_home_view(config).0
    }

    /// The dialog a transfer step (1–3) lays over the Overview.
    fn transfer_dialog_for(config: GlobalViewConfig<'_>) -> Element<'_, Message> {
        global_home_view(config)
            .1
            .expect("a transfer step renders its dialog")
    }
    use crate::{
        app::{
            cache::FiatPriceRequest,
            settings::{display::DisplayMode, unit::BitcoinDisplayUnit},
            view::shared::feerate_picker::FeeratePreset,
        },
        services::fiat::{Currency, PriceSource},
    };
    use coincube_core::miniscript::bitcoin::{bip32::ChildNumber, Address};
    use std::{collections::HashMap, str::FromStr};

    const MAINNET_ADDR: &str = "bc1qvrl2849aggm6qry9ea7xqp2kk39j8vaa8r3cwg";

    #[test]
    fn pending_deposit_card_follows_passive_privacy_on_both_home_wallets() {
        let transfer = PendingTransfer {
            amount: Amount::from_sat(98_765_432),
            stage: TransferStage::PendingDeposit,
        };
        let inspect = |masked| {
            capture::inspect(
                "home-pending-deposit",
                pending_deposit_card(transfer, BitcoinDisplayUnit::Sats, masked),
                (900, 200),
                "dark",
                theme::Theme::dark(),
            )
        };
        let hidden = inspect(true);
        assert!(hidden.contains("••••••"));
        assert!(!hidden.contains("98,765,432"));
        let shown = inspect(false);
        assert!(shown.contains("98,765,432"));
    }

    struct ViewFixture {
        entered_amount: form::Value<String>,
        invalid_amount: form::Value<String>,
        transfer_feerate: form::Value<String>,
        invalid_feerate: form::Value<String>,
        receive_address: Address,
        receive_index: ChildNumber,
        labels: HashMap<String, String>,
        labels_editing: HashMap<String, form::Value<String>>,
        fiat_converter: FiatAmountConverter,
    }

    impl ViewFixture {
        fn new() -> Self {
            let receive_address = Address::from_str(MAINNET_ADDR).unwrap().assume_checked();
            let mut labels = HashMap::new();
            labels.insert(receive_address.to_string(), "Vault receive".to_string());

            Self {
                entered_amount: form::Value {
                    value: "50000".to_string(),
                    valid: true,
                    warning: None,
                },
                invalid_amount: form::Value {
                    value: "not sats".to_string(),
                    valid: false,
                    warning: Some("Invalid amount format"),
                },
                transfer_feerate: form::Value {
                    value: "3".to_string(),
                    valid: true,
                    warning: None,
                },
                invalid_feerate: form::Value {
                    value: "0".to_string(),
                    valid: false,
                    warning: Some("Feerate must be positive"),
                },
                receive_address,
                receive_index: ChildNumber::from(0),
                labels,
                labels_editing: HashMap::new(),
                fiat_converter: FiatAmountConverter::new(
                    50_000.0,
                    Some(1_722_000_000),
                    FiatPriceRequest::new(PriceSource::Coincube, Currency::USD),
                )
                .unwrap(),
            }
        }

        fn config(
            &self,
            step: usize,
            direction: Option<TransferDirection>,
            wallet_picker: Option<PickerSide>,
        ) -> GlobalViewConfig<'_> {
            GlobalViewConfig {
                cube_id: "test-cube",
                liquid_balance: Amount::from_sat(125_000),
                spark_balance: Amount::from_sat(250_000),
                spark_btc_balance: Amount::from_sat(250_000),
                spark_stable_balance: None,
                spark_balance_error: false,
                usdt_balance: 100_000_000,
                usdt_balance_error: false,
                pending_liquid_send_sats: 1_000,
                pending_usdt_send_sats: 2_000,
                pending_liquid_receive_sats: 3_000,
                pending_usdt_receive_sats: 4_000,
                vault_pending_send_sats: 5_000,
                vault_pending_receive_sats: 6_000,
                vault_balance: Amount::from_sat(375_000),
                imported_balance: None,
                imported_installed: false,
                fiat_converter: Some(self.fiat_converter),
                balance_masked: false,
                overview_total_hidden: false,
                overview_wallets_hidden: false,
                total_balance_loading: false,
                spark_balance_loaded: true,
                liquid_balance_loaded: true,
                liquid_balance_error: false,
                usdt_balance_loaded: true,
                vault_loaded: true,
                display_mode: DisplayMode::FiatNative,
                has_vault: true,
                has_imported_bitcoin: false,
                has_spark: true,
                has_liquid: true,
                current_view: HomeView { step },
                transfer_direction: direction,
                transfer_from: direction.map(|d| d.from_kind()),
                transfer_to: direction.map(|d| d.to_kind()),
                wallet_picker,
                entered_amount: &self.entered_amount,
                receive_address: Some(&self.receive_address),
                destination_ready: true,
                transfer_action_ready: true,
                receive_index: Some(&self.receive_index),
                labels: &self.labels,
                labels_editing: &self.labels_editing,
                address_expanded: false,
                bitcoin_unit: BitcoinDisplayUnit::Sats,
                onchain_send_limit: Some((25_000, 5_000_000)),
                onchain_receive_limit: Some((25_000, 5_000_000)),
                is_sending: false,
                is_tx_signed: false,
                prepare_onchain_send_response: None,
                spend_tx_fees: None,
                transfer_feerate: &self.transfer_feerate,
                transfer_feerate_loading: None,
                spark_send_fee_sat: None,
                pending_spark_incoming: Some(PendingTransfer {
                    amount: Amount::from_sat(31_000),
                    stage: TransferStage::PendingDeposit,
                }),
                pending_vault_incoming: Some(PendingTransfer {
                    amount: Amount::from_sat(41_000),
                    stage: TransferStage::PendingDeposit,
                }),
                btc_usd_price: Some(50_000.0),
                cube_name: "Everyday",
                vault_alias: Some("Family savings"),
                chain: ChainId::Bitcoin,
                light_theme: false,
                transfer_page: TransferPage::Wallets,
                transfer_unit: BitcoinDisplayUnit::Sats,
                other_cube: OtherCubeView::default(),
            }
        }
    }

    fn directions() -> [TransferDirection; 6] {
        use TransferDirection::*;
        [
            LiquidToVault,
            VaultToLiquid,
            LiquidToSpark,
            SparkToLiquid,
            VaultToSpark,
            SparkToVault,
        ]
    }

    #[test]
    fn hub_mask_redacts_pending_deposit_amount() {
        let pending = PendingTransfer {
            amount: Amount::from_sat(31_000),
            stage: TransferStage::PendingDeposit,
        };
        let visible = pending_deposit_amount(pending, BitcoinDisplayUnit::Sats, false);
        let hidden = pending_deposit_amount(pending, BitcoinDisplayUnit::Sats, true);
        assert!(visible.contains("31"));
        assert!(visible.contains("SATS"));
        assert_eq!(hidden, AmountCell::masked().primary);
        assert!(!hidden.contains("31"));
    }

    #[test]
    fn r06a_hub_headless_content_matrix_and_presync_usdb_gate() {
        use crate::app::view::shared::landing::capture;
        let fixture = ViewFixture::new();
        let usdb = coincube_spark_protocol::StableBalanceSnapshot {
            balance: 93_420_000_000,
            decimals: 6,
            ticker: "USDB".into(),
        };
        let raw_usdb = format!(
            "{} USDB",
            crate::app::breez_spark::assets::format_token_display(usdb.balance, usdb.decimals)
        );
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for outer in [(1024, 768), (1440, 900)] {
                let normal = capture::inspect(
                    "hub-normal",
                    overview(fixture.config(0, None, None)),
                    outer,
                    appearance,
                    theme,
                );
                assert!(normal.contains("Total Balance"));
                assert!(normal.contains("Wallets"));
                for (wallet, control) in [
                    ("Spark", "open"),
                    ("Spark", "send-btc"),
                    ("Spark", "receive-btc"),
                    ("Liquid", "open"),
                    ("Liquid", "send-lbtc"),
                    ("Vault", "open"),
                    ("Vault", "send-btc"),
                ] {
                    assert!(normal.has_focus(landing_control_id("test-cube", wallet, control)));
                }

                let mut presync = fixture.config(0, None, None);
                presync.spark_stable_balance = Some(&usdb);
                presync.spark_balance_loaded = false;
                presync.total_balance_loading = true;
                let before = capture::inspect(
                    "hub-presync-usdb",
                    overview(presync),
                    outer,
                    appearance,
                    theme,
                );
                assert!(!before.contains(&raw_usdb));
                assert!(before.contains("Waiting for Spark sync"));

                let mut first_error = fixture.config(0, None, None);
                first_error.spark_stable_balance = Some(&usdb);
                first_error.spark_balance_loaded = false;
                first_error.spark_balance_error = true;
                let failed = capture::inspect(
                    "hub-presync-error",
                    overview(first_error),
                    outer,
                    appearance,
                    theme,
                );
                assert!(!failed.contains(&raw_usdb));
                assert!(failed.contains("Balance unavailable"));

                let mut trusted = fixture.config(0, None, None);
                trusted.spark_stable_balance = Some(&usdb);
                trusted.spark_balance_loaded = true;
                trusted.btc_usd_price = None;
                let shown = capture::inspect(
                    "hub-usdb-missing-quote",
                    overview(trusted),
                    outer,
                    appearance,
                    theme,
                );
                assert!(shown.contains(&raw_usdb));
                assert!(shown.contains("Partial total"));

                let mut masked = fixture.config(0, None, None);
                masked.spark_stable_balance = Some(&usdb);
                masked.balance_masked = true;
                let hidden =
                    capture::inspect("hub-masked", overview(masked), outer, appearance, theme);
                assert!(!hidden.contains(&raw_usdb));
                assert!(!hidden.contains("31,000"));

                let mut absent_vault = fixture.config(0, None, None);
                absent_vault.has_vault = false;
                absent_vault.has_imported_bitcoin = true;
                absent_vault.imported_installed = false;
                let import = capture::inspect(
                    "hub-import-pending",
                    overview(absent_vault),
                    outer,
                    appearance,
                    theme,
                );
                assert!(
                    import.contains("Wallet import pending") || import.contains("Create Vault")
                );

                let mut known_zero = fixture.config(0, None, None);
                known_zero.liquid_balance = Amount::ZERO;
                known_zero.spark_btc_balance = Amount::ZERO;
                known_zero.spark_balance = Amount::ZERO;
                known_zero.usdt_balance = 0;
                known_zero.vault_balance = Amount::ZERO;
                let zero = capture::inspect(
                    "hub-known-zero",
                    overview(known_zero),
                    outer,
                    appearance,
                    theme,
                );
                assert!(!zero.contains("No wallet balance has loaded successfully"));
            }
        }
    }

    #[test]
    fn imported_bitcoin_counts_only_after_an_installed_authoritative_balance() {
        use crate::app::view::shared::landing::capture;

        let fixture = ViewFixture::new();
        let inspect = |config| {
            capture::inspect(
                "hub-import-balance",
                overview(config),
                (1024, 768),
                "dark",
                theme::Theme::dark(),
            )
        };
        let imported = || {
            let mut config = fixture.config(0, None, None);
            config.has_vault = false;
            config.has_spark = false;
            config.has_liquid = false;
            config.has_imported_bitcoin = true;
            config.imported_installed = true;
            config.imported_balance = Some(Amount::from_sat(375_000));
            config
        };
        let known = inspect(imported());
        assert!(!known.contains("No wallet balance has loaded successfully"));

        let mut known_zero_config = imported();
        known_zero_config.imported_balance = Some(Amount::ZERO);
        known_zero_config.vault_balance = Amount::ZERO;
        let known_zero = inspect(known_zero_config);
        assert!(!known_zero.contains("No wallet balance has loaded successfully"));

        let mut pending_config = imported();
        pending_config.imported_balance = None;
        let pending = inspect(pending_config);
        assert!(pending.contains("No wallet balance has loaded successfully"));

        let mut uninstalled_config = imported();
        uninstalled_config.imported_installed = false;
        let uninstalled = inspect(uninstalled_config);
        assert!(uninstalled.contains("No wallet balance has loaded successfully"));

        let mut mixed_known_config = imported();
        mixed_known_config.has_vault = true;
        mixed_known_config.vault_loaded = false;
        let mixed_known = inspect(mixed_known_config);
        assert!(!mixed_known.contains("No wallet balance has loaded successfully"));

        let mut mixed_pending_config = imported();
        mixed_pending_config.has_vault = true;
        mixed_pending_config.imported_balance = None;
        mixed_pending_config.vault_loaded = true;
        let mixed_pending = inspect(mixed_pending_config);
        assert!(mixed_pending.contains("No wallet balance has loaded successfully"));

        let mut partial_config = imported();
        partial_config.has_spark = true;
        partial_config.spark_balance_loaded = false;
        partial_config.spark_balance_error = true;
        let partial = inspect(partial_config);
        assert!(partial.contains("Partial total"));
    }

    #[test]
    fn r06a_hub_wallet_header_pointer_and_enter_route_once_to_selected_wallet() {
        use crate::app::view::shared::landing::capture;
        let fixture = ViewFixture::new();
        for pointer in [false, true] {
            for (wallet, menu) in [
                (
                    "Spark",
                    Menu::Spark(crate::app::menu::SparkSubMenu::Overview),
                ),
                ("Liquid", Menu::Liquid(LiquidSubMenu::Overview)),
            ] {
                let messages = capture::activate(
                    overview(fixture.config(0, None, None)),
                    landing_control_id("test-cube", wallet, "open"),
                    (1024, 768),
                    pointer,
                );
                let routes = messages
                    .iter()
                    .filter(|message| matches!(message, Message::Menu(_)))
                    .collect::<Vec<_>>();
                assert_eq!(
                    routes.len(),
                    1,
                    "{wallet} pointer={pointer}: {messages:?}",
                    wallet = wallet,
                    pointer = pointer,
                    messages = messages
                );
                assert!(
                    matches!(routes[0], Message::Menu(actual) if actual == &menu),
                    "{wallet} pointer={pointer}: {messages:?}",
                    wallet = wallet,
                    pointer = pointer,
                    messages = messages
                );
            }
        }
    }

    #[test]
    fn overview_vault_recovery_footer_routes_to_landing_and_respects_privacy() {
        use crate::app::view::shared::landing::capture;

        let fixture = ViewFixture::new();
        let target = landing_control_id("test-cube", "Vault", "recovery-path");
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for outer in [(1024, 768), (1440, 900)] {
                let normal = capture::inspect(
                    "overview-vault-recovery",
                    overview(fixture.config(0, None, None)),
                    outer,
                    appearance,
                    theme,
                );
                assert!(normal.contains("Recovery path"));
                assert!(normal.has_focus(target.clone()));

                let mut masked = fixture.config(0, None, None);
                masked.balance_masked = true;
                let hidden = capture::inspect(
                    "overview-vault-recovery-masked",
                    overview(masked),
                    outer,
                    appearance,
                    theme,
                );
                assert!(hidden.contains("Recovery path"));
                assert!(!hidden.contains("250,000"));
                assert!(!hidden.contains("375,000"));

                let mut imported_only = fixture.config(0, None, None);
                imported_only.has_vault = false;
                imported_only.has_imported_bitcoin = true;
                imported_only.imported_installed = true;
                let imported = capture::inspect(
                    "overview-imported-without-vault",
                    overview(imported_only),
                    outer,
                    appearance,
                    theme,
                );
                assert!(!imported.has_focus(target.clone()));
            }

            // A tall content-only frame exposes the below-fold footer for
            // visual review while the standard compact and wide frames above
            // continue to catch width overflow and privacy regressions.
            let full_card = capture::inspect(
                "overview-vault-recovery-full-card",
                overview(fixture.config(0, None, None)),
                (1440, 1400),
                appearance,
                theme,
            );
            assert!(full_card.contains("Recovery path"));
        }

        for pointer in [false, true] {
            let messages = capture::activate(
                overview(fixture.config(0, None, None)),
                target.clone(),
                // The footer is below the fold at the minimum window size;
                // this viewport exposes it for the pointer probe.
                if pointer { (1440, 1400) } else { (1024, 768) },
                pointer,
            );
            let routes = messages
                .iter()
                .filter(|message| matches!(message, Message::Menu(_)))
                .collect::<Vec<_>>();
            assert_eq!(routes.len(), 1, "pointer={pointer}: {messages:?}");
            assert!(matches!(
                routes[0],
                Message::Menu(Menu::Vault(VaultSubMenu::Overview))
            ));
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum TransferTrigger {
        Pointer,
        Enter,
        Space,
    }

    /// Send real pointer/key events to the production Hub tree. Disabled
    /// Actions do not expose focus IDs, so pointer targeting uses the rendered
    /// Transfer text bounds rather than assuming a focusable Action exists.
    fn activate_transfer(
        content: Element<'_, Message>,
        text_bounds: iced::Rectangle,
        trigger: TransferTrigger,
    ) -> Vec<Message> {
        use coincube_ui::font;
        use iced::advanced::{
            layout,
            renderer::Headless,
            widget::{operation, operation::focusable, Operation, Tree},
            Layout,
        };
        use iced::{keyboard, mouse, Event, Rectangle, Size};

        for bytes in font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        // The Hub's Transfer action can sit below the 900px viewport. Expose
        // the same production tree in a tall headless viewport for activation;
        // the separate capture still checks the real 1440×900 geometry.
        let size = Size::new(953.0, 2400.0);
        assert!(
            text_bounds.y >= 0.0 && text_bounds.y + text_bounds.height <= size.height,
            "Transfer label at {:?} is outside activation viewport",
            text_bounds
        );
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut element: Element<'_, Message> = iced::widget::scrollable(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        if !matches!(trigger, TransferTrigger::Pointer) {
            let id = landing_control_id("test-cube", "Hub", "transfer");
            let mut operation: Box<dyn Operation<()>> = Box::new(focusable::focus(id));
            loop {
                element.as_widget_mut().operate(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &mut operation::black_box(operation.as_mut()),
                );
                match operation.finish() {
                    operation::Outcome::None | operation::Outcome::Some(()) => break,
                    operation::Outcome::Chain(next) => operation = next,
                }
            }
        }
        let cursor = if matches!(trigger, TransferTrigger::Pointer) {
            mouse::Cursor::Available(text_bounds.center())
        } else {
            mouse::Cursor::Unavailable
        };
        let events = match trigger {
            TransferTrigger::Pointer => vec![
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            TransferTrigger::Enter | TransferTrigger::Space => {
                let named = if matches!(trigger, TransferTrigger::Enter) {
                    keyboard::key::Named::Enter
                } else {
                    keyboard::key::Named::Space
                };
                let key = keyboard::Key::Named(named);
                let physical_key =
                    keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified);
                vec![
                    Event::Keyboard(keyboard::Event::KeyPressed {
                        key: key.clone(),
                        modified_key: key.clone(),
                        physical_key,
                        location: keyboard::Location::Standard,
                        modifiers: keyboard::Modifiers::empty(),
                        text: None,
                        repeat: false,
                    }),
                    Event::Keyboard(keyboard::Event::KeyReleased {
                        key: key.clone(),
                        modified_key: key,
                        physical_key,
                        location: keyboard::Location::Standard,
                        modifiers: keyboard::Modifiers::empty(),
                    }),
                ]
            }
        };
        let mut messages = Vec::new();
        for event in events {
            element.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                cursor,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut iced::advanced::Shell::new(&mut messages),
                &Rectangle::with_size(size),
            );
        }
        messages
    }

    #[test]
    fn r06a_transfer_action_requires_authoritative_spark_after_partial_total_settles() {
        use crate::app::{state::GlobalHome, view::shared::landing::capture};

        let fixture = ViewFixture::new();
        let snapshots = GlobalHome::r06a_transfer_snapshots();
        for snapshot in &snapshots {
            let mut config = fixture.config(0, None, None);
            // The adapter itself is inert (no bridge). Its real update results
            // feed this installed-Spark pure presenter without fabricating a
            // backend or claiming that State::view had a live Spark route.
            config.has_spark = snapshot.has_spark;
            config.has_liquid = !snapshot.has_spark;
            config.has_vault = true;
            config.spark_balance = snapshot.spark_balance;
            config.spark_btc_balance = snapshot.spark_balance;
            config.spark_balance_loaded = snapshot.spark_balance_loaded;
            config.spark_balance_error = snapshot.spark_balance_error;
            config.total_balance_loading = snapshot.total_balance_loading;
            let inspection = capture::inspect(
                &format!("hub-transfer-{}", snapshot.name),
                overview(config),
                (1440, 900),
                "dark",
                theme::Theme::dark(),
            );
            let text_bounds = inspection
                .text
                .iter()
                .find_map(|(value, bounds)| (value == "Transfer").then_some(*bounds))
                .expect("rendered Hub Transfer label");
            let expected = matches!(
                snapshot.name,
                "trusted-postsync" | "trusted-refresh-error" | "no-spark"
            );
            assert_eq!(
                inspection.has_focus(landing_control_id("test-cube", "Hub", "transfer")),
                expected,
                "{} focus eligibility",
                snapshot.name
            );
            for trigger in [
                TransferTrigger::Pointer,
                TransferTrigger::Enter,
                TransferTrigger::Space,
            ] {
                let mut config = fixture.config(0, None, None);
                config.has_spark = snapshot.has_spark;
                config.has_liquid = !snapshot.has_spark;
                config.has_vault = true;
                config.spark_balance = snapshot.spark_balance;
                config.spark_btc_balance = snapshot.spark_balance;
                config.spark_balance_loaded = snapshot.spark_balance_loaded;
                config.spark_balance_error = snapshot.spark_balance_error;
                config.total_balance_loading = snapshot.total_balance_loading;
                let messages = activate_transfer(overview(config), text_bounds, trigger);
                let next_steps = messages
                    .iter()
                    .filter(|message| matches!(message, Message::Home(HomeMessage::NextStep)))
                    .count();
                assert_eq!(
                    next_steps,
                    usize::from(expected),
                    "{} {trigger:?}: {messages:?}",
                    snapshot.name
                );
            }
        }

        let trusted = snapshots
            .iter()
            .find(|snapshot| snapshot.name == "trusted-postsync")
            .unwrap();
        assert_eq!(trusted.spark_balance, Amount::from_sat(12_000));
        let mut amount = fixture.config(1, Some(TransferDirection::SparkToVault), None);
        amount.spark_balance = trusted.spark_balance;
        let entered = capture::inspect(
            "hub-transfer-trusted-amount",
            transfer_dialog_for(amount),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(entered.contains("12,000"));
        // The inline source list keeps the corroborated Spark amount visible.
        let selected = capture::inspect(
            "hub-transfer-trusted-source",
            transfer_wallet_section(
                "test-cube",
                PickerSide::From,
                TransferDirection::SparkToVault,
                true,
                true,
                false,
                Amount::from_sat(0),
                trusted.spark_balance,
                Amount::from_sat(375_000),
                None,
                BitcoinDisplayUnit::Sats,
                false,
            ),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(selected.contains("From"));
        assert!(selected.contains("Spark"));
        assert!(selected.contains("12,000"));
    }

    #[test]
    fn transfer_wallet_rows_follow_global_balance_mask() {
        use crate::app::view::shared::landing::capture;

        let fixture = ViewFixture::new();
        let direction = Some(TransferDirection::LiquidToSpark);
        let render_amount = |masked| {
            let mut config = fixture.config(1, direction, None);
            config.balance_masked = masked;
            capture::inspect(
                "hub-transfer-privacy-amount",
                transfer_dialog_for(config),
                (1440, 900),
                "dark",
                theme::Theme::dark(),
            )
        };
        let hidden = render_amount(true);
        assert!(hidden.contains("••••••"));
        assert!(!hidden.contains("125,000"));
        assert!(!hidden.contains("250,000"));
        let shown = render_amount(false);
        assert!(shown.contains("125,000"));
        assert!(shown.contains("250,000"));

        let render_source_rows = |masked| {
            capture::inspect(
                "hub-transfer-privacy-source",
                transfer_wallet_section(
                    "test-cube",
                    PickerSide::From,
                    TransferDirection::LiquidToSpark,
                    true,
                    true,
                    true,
                    Amount::from_sat(125_000),
                    Amount::from_sat(250_000),
                    Amount::from_sat(375_000),
                    None,
                    BitcoinDisplayUnit::Sats,
                    masked,
                ),
                (1440, 900),
                "dark",
                theme::Theme::dark(),
            )
        };
        let hidden_picker = render_source_rows(true);
        assert!(hidden_picker.contains("••••••"));
        for balance in ["125,000", "250,000", "375,000"] {
            assert!(
                !hidden_picker.contains(balance),
                "hidden picker balance {}",
                balance
            );
        }
        let shown_picker = render_source_rows(false);
        for balance in ["125,000", "250,000", "375,000"] {
            assert!(
                shown_picker.contains(balance),
                "shown picker balance {}",
                balance
            );
        }
    }

    #[test]
    fn transfer_form_layout_and_synthetic_capture_matrix() {
        let fixture = ViewFixture::new();
        for (width, height) in [(1024, 768), (1440, 900)] {
            for (appearance, theme) in [
                ("dark", theme::Theme::dark()),
                ("light", theme::Theme::light()),
            ] {
                let snapshot = capture::inspect(
                    "transfer-wallets-amount",
                    transfer_dialog_for(fixture.config(
                        1,
                        Some(TransferDirection::SparkToVault),
                        None,
                    )),
                    (width, height),
                    appearance,
                    theme,
                );
                for label in ["Transfer between wallets", "From", "To", "Another Cube…"] {
                    assert!(
                        snapshot.contains(label),
                        "{} {}: {}",
                        appearance,
                        width,
                        label
                    );
                }
                assert!(snapshot.contains("250,000"));
                assert!(snapshot.contains("375,000"));
                assert!(snapshot.contains("25.00 USD"));
            }
        }
    }

    #[test]
    fn transfer_wallet_rows_activate_by_enter_and_space() {
        use iced::keyboard::key::Named;

        let fixture = ViewFixture::new();
        for key in [Named::Enter, Named::Space] {
            let content =
                transfer_dialog_for(fixture.config(1, Some(TransferDirection::SparkToVault), None));
            let messages = capture::activate_with_key(
                content,
                landing_control_id("test-cube", "Vault", "transfer-from"),
                (1024, 768),
                false,
                key,
            );
            assert_eq!(
                messages
                    .iter()
                    .filter(|message| {
                        matches!(
                            message,
                            Message::Home(HomeMessage::SelectTransferWallet(
                                PickerSide::From,
                                WalletKind::Vault
                            ))
                        )
                    })
                    .count(),
                1,
            );
            assert_eq!(fixture.entered_amount.value, "50000");
        }
    }

    /// Every directed pair of distinct wallets must map to exactly one
    /// `TransferDirection`, and same-wallet pairs must map to `None`.
    #[test]
    fn try_from_pair_is_total_over_distinct_pairs() {
        let kinds = [WalletKind::Liquid, WalletKind::Spark, WalletKind::Vault];
        for from in kinds {
            for to in kinds {
                match TransferDirection::try_from_pair(from, to) {
                    Some(direction) => {
                        assert_ne!(from, to, "distinct pair expected for {from:?}→{to:?}");
                        assert_eq!(direction.pair(), (from, to));
                    }
                    None => {
                        assert_eq!(
                            from, to,
                            "try_from_pair returned None for distinct pair {from:?}→{to:?}"
                        );
                    }
                }
            }
        }
    }

    /// Round-trip: every variant's `pair()` reconstructs the same variant.
    #[test]
    fn pair_roundtrip_for_all_variants() {
        use TransferDirection::*;
        for direction in [
            LiquidToVault,
            VaultToLiquid,
            LiquidToSpark,
            SparkToLiquid,
            VaultToSpark,
            SparkToVault,
        ] {
            let (from, to) = direction.pair();
            assert_eq!(
                TransferDirection::try_from_pair(from, to),
                Some(direction),
                "roundtrip failed for {direction:?}"
            );
        }
    }

    /// `from_kind()`/`to_kind()` must agree with `pair()`.
    #[test]
    fn from_to_kind_agrees_with_pair() {
        use TransferDirection::*;
        for direction in [
            LiquidToVault,
            VaultToLiquid,
            LiquidToSpark,
            SparkToLiquid,
            VaultToSpark,
            SparkToVault,
        ] {
            let (from, to) = direction.pair();
            assert_eq!(direction.from_kind(), from);
            assert_eq!(direction.to_kind(), to);
        }
    }

    /// Picker rows render a network badge — the uppercase label must be
    /// stable across changes (copy contract with the Liquid Send picker).
    #[test]
    fn wallet_kind_badges_are_uppercase() {
        assert_eq!(WalletKind::Liquid.badge(), "LIQUID");
        assert_eq!(WalletKind::Spark.badge(), "SPARK");
        assert_eq!(WalletKind::Vault.badge(), "VAULT");
    }

    #[test]
    fn wallet_kind_labels_and_direction_display_are_stable() {
        assert_eq!(WalletKind::Liquid.label(), "Liquid");
        assert_eq!(WalletKind::Spark.label(), "Spark");
        assert_eq!(WalletKind::Vault.label(), "Vault");

        for direction in directions() {
            assert_eq!(direction.from_wallet(), direction.from_kind().label());
            assert_eq!(direction.to_wallet(), direction.to_kind().label());
            assert_eq!(
                direction.display(),
                format!("{} → {}", direction.from_wallet(), direction.to_wallet())
            );
        }
    }

    #[test]
    fn home_view_step_state_moves_forward_back_and_resets() {
        let mut view = HomeView::default();

        view.previous();
        assert_eq!(view.step, 0);

        view.next();
        view.next();
        assert_eq!(view.step, 2);

        view.previous();
        assert_eq!(view.step, 1);

        view.reset();
        assert_eq!(view.step, 0);
    }

    #[test]
    fn overview_section_masks_never_override_global_hide() {
        assert_eq!(
            OverviewMasks::new(false, false, false),
            OverviewMasks {
                total: false,
                wallets: false
            }
        );
        assert_eq!(
            OverviewMasks::new(false, true, false),
            OverviewMasks {
                total: true,
                wallets: false
            }
        );
        assert_eq!(
            OverviewMasks::new(false, false, true),
            OverviewMasks {
                total: false,
                wallets: true
            }
        );
        for total in [false, true] {
            for wallets in [false, true] {
                assert_eq!(
                    OverviewMasks::new(true, total, wallets),
                    OverviewMasks {
                        total: true,
                        wallets: true
                    }
                );
            }
        }
    }

    #[test]
    fn overview_context_alias_and_section_controls_render() {
        use crate::app::view::shared::landing::capture;
        let fixture = ViewFixture::new();
        let shown = capture::inspect(
            "overview-context",
            overview(fixture.config(0, None, None)),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(shown.contains("Everyday · Bitcoin"));
        assert!(shown.contains("Vault · Family savings"));
        for section in ["Total Balance", "Wallets"] {
            assert!(shown.has_focus(landing_control_id("test-cube", section, "balance-privacy")));
        }

        let mut global_hidden = fixture.config(0, None, None);
        global_hidden.balance_masked = true;
        let hidden = capture::inspect(
            "overview-global-hide",
            overview(global_hidden),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        );
        assert!(!hidden.contains("250,000"));
        assert!(!hidden.contains("31,000"));
    }

    #[test]
    fn global_home_overview_builds_loaded_masked_loading_and_absent_wallet_states() {
        let fixture = ViewFixture::new();

        let _ = overview(fixture.config(0, None, None));

        let mut masked = fixture.config(0, None, None);
        masked.balance_masked = true;
        masked.display_mode = DisplayMode::BitcoinNative;
        let _ = overview(masked);

        let mut loading = fixture.config(0, None, None);
        loading.total_balance_loading = true;
        loading.spark_balance_loaded = false;
        loading.liquid_balance_loaded = false;
        loading.usdt_balance_loaded = false;
        loading.vault_loaded = false;
        loading.usdt_balance_error = true;
        let _ = overview(loading);

        // A failed fetch: loaded (so the aggregate settles) but flagged, so the
        // row says "Balance unavailable" rather than rendering the zero it
        // never managed to overwrite.
        let mut liquid_failed = fixture.config(0, None, None);
        liquid_failed.liquid_balance_loaded = true;
        liquid_failed.liquid_balance_error = true;
        let _ = overview(liquid_failed);

        let mut spark_only = fixture.config(0, None, None);
        spark_only.has_liquid = false;
        spark_only.has_vault = false;
        spark_only.has_spark = true;
        spark_only.pending_spark_incoming = Some(PendingTransfer {
            amount: Amount::from_sat(11_000),
            stage: TransferStage::Completed,
        });
        let _ = overview(spark_only);
    }

    #[test]
    fn global_home_transfer_form_builds_every_direction_and_available_pair() {
        let fixture = ViewFixture::new();

        for direction in directions() {
            let _ = transfer_dialog_for(fixture.config(1, Some(direction), None));
        }

        let mut no_liquid = fixture.config(1, Some(TransferDirection::VaultToSpark), None);
        no_liquid.has_liquid = false;
        let _ = transfer_dialog_for(no_liquid);
    }

    #[test]
    fn global_home_confirm_step_builds_address_fee_and_sending_variants() {
        let fixture = ViewFixture::new();

        let mut liquid_to_vault = fixture.config(2, Some(TransferDirection::LiquidToVault), None);
        let liquid_prepare = PreparePayOnchainResponse {
            receiver_amount_sat: 49_000,
            claim_fees_sat: 100,
            total_fees_sat: 1_000,
        };
        liquid_to_vault.prepare_onchain_send_response = Some(&liquid_prepare);
        let _ = transfer_dialog_for(liquid_to_vault);

        let mut vault_to_spark = fixture.config(2, Some(TransferDirection::VaultToSpark), None);
        vault_to_spark.address_expanded = true;
        vault_to_spark.spend_tx_fees = Some(Amount::from_sat(750));
        vault_to_spark.transfer_feerate_loading = Some(FeeratePreset::Fast);
        let _ = transfer_dialog_for(vault_to_spark);

        let mut vault_to_liquid_signed =
            fixture.config(2, Some(TransferDirection::VaultToLiquid), None);
        vault_to_liquid_signed.is_tx_signed = true;
        vault_to_liquid_signed.is_sending = true;
        vault_to_liquid_signed.spend_tx_fees = Some(Amount::from_sat(900));
        let _ = transfer_dialog_for(vault_to_liquid_signed);

        let mut spark_to_liquid = fixture.config(2, Some(TransferDirection::SparkToLiquid), None);
        spark_to_liquid.spark_send_fee_sat = Some(222);
        spark_to_liquid.receive_address = None;
        let _ = transfer_dialog_for(spark_to_liquid);

        let mut invalid_vault_send =
            fixture.config(2, Some(TransferDirection::VaultToLiquid), None);
        invalid_vault_send.entered_amount = &fixture.invalid_amount;
        invalid_vault_send.transfer_feerate = &fixture.invalid_feerate;
        let _ = transfer_dialog_for(invalid_vault_send);
    }

    #[test]
    fn global_home_success_and_fallback_steps_build() {
        let fixture = ViewFixture::new();

        for direction in directions() {
            let _ = transfer_dialog_for(fixture.config(3, Some(direction), None));
        }

        assert!(
            global_home_view(fixture.config(99, Some(TransferDirection::LiquidToVault), None))
                .1
                .is_none()
        );
    }

    fn inspect_dialog(scenario: &str, config: GlobalViewConfig<'_>) -> capture::Inspection {
        capture::inspect(
            scenario,
            transfer_dialog_for(config),
            (1440, 900),
            "dark",
            theme::Theme::dark(),
        )
    }

    fn cube_choice(name: &str, chain: ChainId, refusal: Option<&'static str>) -> CubeChoice {
        CubeChoice {
            cube_id: format!("{name}-id"),
            name: name.to_string(),
            chain,
            has_vault: true,
            passkey: false,
            vault_wallet_id: None,
            refusal,
        }
    }

    /// P-Transfer: Wallets, Amount, Review within this Cube; Wallets, Other
    /// Cube, Unlock, Amount, Review to another. Each page keeps its stepper.
    #[test]
    fn transfer_steps_follow_the_route() {
        let fixture = ViewFixture::new();
        let same = TransferDirection::SparkToVault;
        for (step, page) in [
            (1, TransferPage::Wallets),
            (1, TransferPage::Amount),
            (2, TransferPage::Wallets),
            (3, TransferPage::Wallets),
        ] {
            let mut config = fixture.config(step, Some(same), None);
            config.transfer_page = page;
            let snapshot = inspect_dialog("transfer-steps-same", config);
            for label in SAME_CUBE_STEPS {
                assert!(snapshot.contains(label), "{:?} {}: {}", page, step, label);
            }
            assert!(!snapshot.contains("Other Cube"), "{:?} {}", page, step);
        }
        let choices = [cube_choice("Archive", ChainId::Bitcoin, None)];
        for page in [
            TransferPage::Wallets,
            TransferPage::OtherCube,
            TransferPage::Unlock,
            TransferPage::Amount,
        ] {
            let mut config = fixture.config(1, Some(TransferDirection::VaultToOtherCube), None);
            config.transfer_page = page;
            config.other_cube.choices = &choices;
            config.other_cube.selected = Some("Archive-id");
            let snapshot = inspect_dialog("transfer-steps-other", config);
            for label in OTHER_CUBE_STEPS {
                assert!(snapshot.contains(label), "{:?}: {}", page, label);
            }
        }
    }

    #[test]
    fn transfer_pages_match_the_prototype() {
        let fixture = ViewFixture::new();
        let wallets = inspect_dialog(
            "transfer-wallets",
            fixture.config(1, Some(TransferDirection::SparkToVault), None),
        );
        for label in [
            "Transfer",
            "Everyday · Bitcoin mainnet",
            "Another Cube…",
            "Continue",
        ] {
            assert!(wallets.contains(label), "wallets: {}", label);
        }
        // Another Cube needs the Vault as source.
        assert!(wallets.contains("From the Vault only, for now"));
        assert!(!wallets.contains("◉") && !wallets.contains("Amount in sats"));

        let mut amount = fixture.config(1, Some(TransferDirection::SparkToVault), None);
        amount.transfer_page = TransferPage::Amount;
        let amount = inspect_dialog("transfer-amount", amount);
        for label in [
            "Destination and amount",
            "From Spark · Available 250,000 SATS",
            "· this Cube",
            "SATS",
            "BTC",
            "Minimum",
            "Prepare",
            "Back",
        ] {
            assert!(amount.contains(label), "amount: {}", label);
        }

        let review = inspect_dialog(
            "transfer-review",
            fixture.config(2, Some(TransferDirection::SparkToVault), None),
        );
        for label in [
            "Review and confirm",
            "Nothing moves until you confirm.",
            "Sending from",
            "Spark · Everyday",
            "Receiving wallet",
            "Route",
            "Bitcoin on-chain",
            "Fees",
            "Confirm transfer",
            "Address details",
        ] {
            assert!(review.contains(label), "review: {}", label);
        }
        assert!(review.contains(MAINNET_ADDR), "whole address");
        assert!(!review.contains("Show QR code"), "details stay collapsed");
        let mut expanded = fixture.config(2, Some(TransferDirection::SparkToVault), None);
        expanded.address_expanded = true;
        let expanded = inspect_dialog("transfer-review-expanded", expanded);
        assert!(expanded.contains("Show QR code") && expanded.contains("Vault receive"));

        let done = inspect_dialog(
            "transfer-done",
            fixture.config(3, Some(TransferDirection::SparkToVault), None),
        );
        for label in [
            "Transfer sent",
            "Pending",
            "you can leave this page",
            "See it in Activity",
            "Back to Overview",
            "New transfer",
        ] {
            assert!(done.contains(label), "done: {}", label);
        }
    }

    #[test]
    fn other_cube_pages_list_unlock_and_relock() {
        let fixture = ViewFixture::new();
        let choices = [
            cube_choice("Archive", ChainId::Bitcoin, None),
            cube_choice(
                "Lab",
                ChainId::Testnet4,
                Some("Different network: can’t receive this transfer"),
            ),
        ];
        let other = |page, step| {
            let mut config = fixture.config(step, Some(TransferDirection::VaultToOtherCube), None);
            config.transfer_page = page;
            config.other_cube.choices = &choices;
            config.other_cube.selected = Some("Archive-id");
            config
        };

        let catalog = inspect_dialog("transfer-other-cube", other(TransferPage::OtherCube, 1));
        for label in [
            "Choose the other Cube",
            "Bitcoin mainnet Cubes only.",
            "Archive",
            "Lab",
            "Different network: can’t receive this transfer",
            "Locked",
            "Locked Cubes show no balances.",
            "Continue with Archive",
        ] {
            assert!(catalog.contains(label), "catalog: {}", label);
        }

        let unlock = inspect_dialog("transfer-unlock", other(TransferPage::Unlock, 1));
        for label in [
            "Unlock Archive for this transfer only",
            "Locks again when the transfer ends.",
            "Archive PIN",
            "Not Everyday's PIN.",
            "Unlock",
            "Cancel",
        ] {
            assert!(unlock.contains(label), "unlock: {}", label);
        }
        let mut checking = other(TransferPage::Unlock, 1);
        checking.other_cube.phase = UnlockPhase::Checks(2);
        let checking = inspect_dialog("transfer-unlock-checks", checking);
        for label in [
            "PIN accepted",
            "Archive's Vault checked",
            "Getting a receive address…",
            "In progress",
            "Cancel and lock",
        ] {
            assert!(checking.contains(label), "checks: {}", label);
        }

        let mut amount = other(TransferPage::Amount, 1);
        amount.other_cube.unlocked = true;
        let amount = inspect_dialog("transfer-other-amount", amount);
        assert!(amount.contains("Wallet in Archive"));
        assert!(amount.contains("Archive · Bitcoin mainnet · unlocked for this transfer"));

        let mut review = other(TransferPage::Amount, 2);
        review.other_cube.unlocked = true;
        let review = inspect_dialog("transfer-other-review", review);
        assert!(review.contains("Vault · Archive · Bitcoin mainnet"));
        assert!(review.contains("Archive locks again once sent, or if you leave."));
        assert!(review.contains(MAINNET_ADDR));
        // Labels and QR belong to this Cube's own addresses only.
        assert!(!review.contains("Address details"));

        let mut done = other(TransferPage::Amount, 3);
        done.other_cube.locked_again = true;
        let done = inspect_dialog("transfer-other-done", done);
        assert!(done.contains("Archive is locked again."));
        assert!(done.contains("Vault · Archive"));
    }

    /// Transfer needs two wallets to move funds between: any one wallet alone
    /// has nothing to transfer with, any two will do.
    #[test]
    fn transfer_available_needs_two_wallets() {
        // (liquid, vault, spark)
        assert!(!transfer_available(false, false, false), "no wallets");
        assert!(!transfer_available(true, false, false), "liquid alone");
        assert!(!transfer_available(false, true, false), "vault alone");
        assert!(
            !transfer_available(false, false, true),
            "spark alone — the new post-sunset default cube, and the case the \
             old `has_vault || has_spark` gate got wrong"
        );

        assert!(transfer_available(true, true, false));
        assert!(transfer_available(true, false, true));
        assert!(transfer_available(false, true, true));
        assert!(transfer_available(true, true, true));
    }
}
