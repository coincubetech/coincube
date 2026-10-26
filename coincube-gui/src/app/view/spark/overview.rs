//! View renderer for [`crate::app::state::spark::overview::SparkOverview`].
//!
//! Mirrors the Liquid overview layout almost exactly:
//! - Unified portfolio card with a total balance header + one asset row
//!   for BTC. No USDt (that's a Liquid-only asset) and no
//!   L-BTC branding — Spark runs on real Bitcoin.
//! - Recent transactions use the same compact row as the other landings.
//! - A "Stable" badge on the balance when the SDK reports an active
//!   Stable Balance token.

use coincube_core::miniscript::bitcoin::Amount;

use crate::app::wallets::DomainPaymentStatus;
use coincube_ui::component::text::Text;
use coincube_ui::{
    color,
    component::{amount::*, button, text::*},
    icon::receipt_icon,
    theme,
    widget::*,
};
use iced::{
    widget::{container, Column, Container, Row},
    Alignment, Background, Length,
};

use crate::app::settings::display::DisplayMode;
use crate::app::state::connect::ConnectCubePanel;
use crate::app::state::spark::overview::SparkBalanceSnapshot;
use crate::app::view::shared::activity_row::{
    activity_row, ActivityDirection, ActivityIdentity, ActivityRow,
};
use crate::app::view::shared::landing::{
    amount_cell, landing_balance_section, landing_button, landing_control_id,
    landing_subtle_button_style, AmountCell, AmountTier,
};
use crate::app::view::spark::settings::lightning_address::{
    lightning_address_body, LN_ADDRESS_DOMAIN,
};
use crate::app::view::vault::fiat::FiatAmount;
use crate::app::view::wallet_header::{wallet_header, HeaderVariant, SyncState, WalletHeaderProps};
use crate::app::view::ConnectCubeMessage;
use crate::app::view::{FiatAmountConverter, SparkOverviewMessage};

/// High-level status of the Spark backend for the current cube.
#[derive(Debug, Clone)]
pub enum SparkStatus {
    /// No Spark signer configured for this cube (or bridge spawn failed).
    Unavailable,
    /// First `get_info` is still in flight.
    Loading,
    /// Bridge returned a balance snapshot.
    Connected(SparkBalanceSnapshot),
    /// Bridge returned an error response.
    Error(String),
}

/// Which underlying Spark payment rail fulfilled a transaction. Drives
/// the asset icon on the overview row — Spark-native and Lightning
/// payments share the same wallet balance but should look different at
/// a glance.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SparkPaymentMethod {
    Lightning,
    OnChainBitcoin,
    Spark,
    /// Stable Balance conversion leg (BTC ↔ USDB). Surfaces in the
    /// transaction list whenever the SDK records the receive side of
    /// a conversion as a top-level `method=token` payment. Rendered
    /// with the "Stable" branding rather than as a bitcoin transfer
    /// — the amount is in token base units, not sats.
    StableBalance,
}

/// Row data the overview renderer needs for each recent payment.
/// Mirrors `view::liquid::RecentTransaction` plus a `token_display`
/// override that the Stable Balance USDB rows use the same way Liquid
/// uses `usdt_display` — when set, the row shows that string in place
/// of the BTC amount and skips the BTC pending-sums.
///
/// Carries the Spark payment `id` and raw `timestamp` so the detail
/// view can render a full date and a copy-to-clipboard payment ID
/// without needing to hold onto the originating `PaymentSummary`.
#[derive(Debug, Clone)]
pub struct SparkRecentTransaction {
    pub id: String,
    pub description: String,
    pub time_ago: String,
    pub timestamp: u64,
    pub amount: Amount,
    pub fees_sat: Amount,
    pub fiat_amount: Option<FiatAmount>,
    pub is_incoming: bool,
    pub status: DomainPaymentStatus,
    pub method: SparkPaymentMethod,
    /// When set, render this string instead of the BTC `amount` (e.g.
    /// "1.58 USDB"). Token-method payments populate this; bitcoin
    /// rows leave it `None`.
    pub token_display: Option<String>,
}

/// View wrapper for the Spark Overview panel.
pub struct SparkOverviewView<'a> {
    pub status: SparkStatus,
    pub recent_transactions: &'a [SparkRecentTransaction],
    pub cube_id: &'a str,
    /// An error can coexist with a retained last-success snapshot.
    pub last_error: Option<&'a str>,
    pub fiat_converter: Option<FiatAmountConverter>,
    pub bitcoin_unit: BitcoinDisplayUnit,
    pub show_direction_badges: bool,
    /// Phase 6: `true` when the SDK reports an active Stable Balance
    /// token. Rendered as a small "Stable" badge next to the balance
    /// header.
    pub stable_balance_active: Option<bool>,
    /// Reference BTC/USD price used to fold USDB into the unified
    /// portfolio total at the current rate. `None` skips the fold —
    /// matches the Liquid panel's USDt behaviour.
    pub btc_usd_price: Option<f64>,
    pub display_mode: DisplayMode,
    pub mask_amounts: bool,
}

/// Ready-address row for the Overview footprint. Editing, claiming and
/// confirmation use the existing Connect-owned component unchanged.
pub fn spark_landing_address<'a>(
    state: &'a ConnectCubePanel,
) -> Element<'a, crate::app::view::Message> {
    let address = state
        .lightning_address
        .as_ref()
        .and_then(|value| value.lightning_address.as_deref());
    if state.ln_editing
        || state.ln_change_confirm_pending.is_some()
        || state.ln_reconcile_needs_reregister.is_some()
        || address.is_none()
    {
        return lightning_address_body(state).map(crate::app::view::Message::ConnectCube);
    }

    let raw_address = address.expect("checked above");
    let address = if raw_address.contains('@') {
        raw_address.to_owned()
    } else {
        format!("{raw_address}{LN_ADDRESS_DOMAIN}")
    };
    let short_address = crate::utils::truncate_middle(&address, 15, 12);
    let row: Element<'a, ConnectCubeMessage> = Container::new(
        Row::new()
            .spacing(12)
            .align_y(Alignment::Center)
            .push(
                Column::new()
                    .spacing(4)
                    .width(Length::Fill)
                    .push(h4_bold("Lightning Address"))
                    .push(text(short_address).size(P2_SIZE)),
            )
            .push(
                button::secondary(None, "Copy")
                    .on_press(ConnectCubeMessage::CopyToClipboard(address)),
            )
            .push(
                button::secondary(None, "Change")
                    .on_press(ConnectCubeMessage::BeginEditLightningAddress),
            ),
    )
    .padding(16)
    .width(Length::Fill)
    .style(crate::app::view::shared::landing::landing_card_style)
    .into();
    row.map(crate::app::view::Message::ConnectCube)
}

/// Context below Spark's recent activity. The address form is supplied by the
/// Connect owner so its validation, confirmation, and account gate stay shared.
pub fn spark_landing_context<'a>(
    address: Element<'a, crate::app::view::Message>,
    active: Option<bool>,
    saving: bool,
    available: bool,
    cube_id: &str,
) -> Element<'a, crate::app::view::Message> {
    use crate::app::view::{Message, SparkOverviewMessage};

    let status = match (available, active, saving) {
        (false, _, _) => "Spark is unavailable",
        (_, _, true) => "Saving…",
        (_, Some(true), false) => "Stable Balance is ON",
        (_, Some(false), false) => "Stable Balance is OFF",
        (_, None, false) => "Stable Balance setting is unknown",
    };
    let next = active.map(|enabled| !enabled);
    let button = landing_button(
        landing_control_id(cube_id, "Spark", "stable-balance-toggle"),
        if active == Some(true) {
            "Turn off"
        } else {
            "Turn on"
        },
        None,
        Message::FocusAction,
        (available && !saving)
            .then_some(next)
            .flatten()
            .map(|enabled| {
                Message::SparkOverview(SparkOverviewMessage::ToggleStableBalance(enabled))
            }),
        landing_subtle_button_style,
    );
    let stable = Container::new(
        Column::new()
            .spacing(8)
            .push(h4_bold("Stable Balance"))
            .push(text("Keep a portion of your Bitcoin balance pegged to USD. Set per device; not restored with the seed. No guaranteed peg.")
                .size(P2_SIZE).style(theme::text::secondary))
            .push(
                Row::new()
                    .spacing(12)
                    .align_y(Alignment::Center)
                    .push(text(status).size(P2_SIZE))
                    .push(iced::widget::Space::new().width(Length::Fill))
                    .push(button),
            ),
    )
    .padding(20)
    .width(Length::Fill)
    .style(crate::app::view::shared::landing::landing_card_style);

    Column::new()
        .spacing(16)
        .padding([16, 16])
        .push(address)
        .push(stable)
        .into()
}

impl<'a> SparkOverviewView<'a> {
    pub fn render(self) -> Element<'a, SparkOverviewMessage> {
        match &self.status {
            SparkStatus::Unavailable => Column::new()
                .spacing(20)
                .push(Container::new(h2("Spark Wallet")))
                .push(p1_regular(
                    "Spark is not configured for this cube yet. Set up a Spark \
                     signer and restart the app to connect the bridge.",
                ))
                .into(),
            SparkStatus::Loading => Column::new()
                .spacing(20)
                .push(Container::new(h2("Spark Wallet")))
                .push(p1_regular("Connecting to the Spark bridge…"))
                .into(),
            SparkStatus::Error(err) => Column::new()
                .spacing(20)
                .push(Container::new(h2("Spark Wallet")))
                .push(p1_regular("Spark bridge error"))
                .push(p2_regular(err.clone()))
                .into(),
            SparkStatus::Connected(snapshot) => connected_view(
                snapshot.balance_sats,
                snapshot.stable_balance.as_ref(),
                self.btc_usd_price,
                self.recent_transactions,
                self.fiat_converter,
                self.bitcoin_unit,
                self.show_direction_badges,
                self.stable_balance_active,
                self.display_mode,
                self.mask_amounts,
                self.cube_id,
                self.last_error,
            ),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn connected_view<'a>(
    balance_sats: u64,
    stable_balance: Option<&coincube_spark_protocol::StableBalanceSnapshot>,
    btc_usd_price: Option<f64>,
    recent: &'a [SparkRecentTransaction],
    fiat_converter: Option<FiatAmountConverter>,
    bitcoin_unit: BitcoinDisplayUnit,
    _show_direction_badges: bool,
    stable_balance_active: Option<bool>,
    display_mode: DisplayMode,
    mask_amounts: bool,
    cube_id: &str,
    last_error: Option<&str>,
) -> Element<'a, SparkOverviewMessage> {
    use crate::app::breez_spark::assets::stable_token_as_sats;

    let mut content = Column::new().spacing(20);

    // Fold USDB into the unified portfolio total at the current
    // BTC/USD price — same pattern as Liquid's USDt fold. The
    // Stable Balance feature promises the spendable balance stays
    // pegged to fiat; if we showed only `balance_sats` here, toggling
    // Stable Balance ON would look like the wallet was emptied.
    //
    // USDB is USD-denominated. A quote in EUR/GBP is not a USD quote;
    // only the dedicated BTC/USD quote can value this holding in sats.
    let reference_price = btc_usd_price.filter(|price| *price > 0.0 && price.is_finite());
    let usdb_as_sats = stable_balance
        .map(|sb| stable_token_as_sats(sb.balance, sb.decimals, reference_price))
        .unwrap_or(0);
    let has_unvalued_usdb =
        stable_balance.as_ref().is_some_and(|sb| sb.balance > 0) && reference_price.is_none();
    let total_balance = Amount::from_sat(balance_sats.saturating_add(usdb_as_sats));
    let total_fiat = (!has_unvalued_usdb)
        .then(|| fiat_converter.as_ref().map(|c| c.convert(total_balance)))
        .flatten();

    // Sum pending BTC (in/out) from the recent-transactions list for
    // the small "pending" indicators underneath the total. Skip
    // token-display rows so a USDB conversion entry doesn't get
    // counted as pending sats.
    let (pending_outgoing_sats, pending_incoming_sats) = pending_btc_sats(recent);

    // ── Unified portfolio card ─────────────────────────────────────────
    let total_col = Column::new()
        .spacing(4)
        .push(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(h4_bold("Balance"))
                .push_maybe((stable_balance_active == Some(true)).then(stable_badge)),
        )
        .push(if mask_amounts {
            amount_cell::<SparkOverviewMessage>(AmountCell::masked(), AmountTier::Card)
        } else if has_unvalued_usdb {
            amount_cell::<SparkOverviewMessage>(
                AmountCell::unavailable("Combined BTC and USDB valuation needs a BTC/USD quote"),
                AmountTier::Card,
            )
        } else {
            wallet_header::<SparkOverviewMessage>(WalletHeaderProps {
                sats: total_balance,
                fiat: total_fiat,
                balance_masked: mask_amounts,
                bitcoin_unit,
                variant: HeaderVariant::Landing,
                sync: SyncState::Synced,
                unconfirmed: None,
                pending_send_sats: pending_outgoing_sats,
                pending_receive_sats: pending_incoming_sats,
                display_mode,
                on_swap: Some(SparkOverviewMessage::FlipDisplayMode),
                on_swap_focus: Some((
                    landing_control_id(cube_id, "Spark", "flip-balance"),
                    SparkOverviewMessage::FocusAction,
                )),
            })
            .into()
        });

    // Show raw BTC alongside raw USDB. The combined header is only a
    // current-rate display estimate when a dedicated BTC/USD quote exists;
    // the send route keeps the SDK's own spending behavior.
    let btc_row_amount = Amount::from_sat(balance_sats);
    let btc_row_fiat = fiat_converter.as_ref().map(|c| c.convert(btc_row_amount));

    let btc_fiat_str = btc_row_fiat
        .as_ref()
        .map(|f| format!("{} {}", f.to_formatted_string(), f.currency()))
        .unwrap_or_default();
    let btc_row = Row::new()
        .spacing(10)
        .align_y(Alignment::Center)
        .push(coincube_ui::image::asset_network_logo::<SparkOverviewMessage>("btc", "spark", 40.0))
        .push(text("BTC").size(P1_SIZE).bold().width(Length::Fixed(60.0)))
        .push(amount_cell::<SparkOverviewMessage>(
            if mask_amounts {
                AmountCell::masked()
            } else {
                AmountCell::shown(
                    format!(
                        "{} {}",
                        btc_row_amount.to_formatted_string_with_unit(bitcoin_unit),
                        bitcoin_unit.label()
                    ),
                    (!btc_fiat_str.is_empty()).then_some(btc_fiat_str),
                )
            },
            AmountTier::Row,
        ))
        .push(iced::widget::Space::new().width(Length::Fill))
        .push(
            landing_button(
                landing_control_id(cube_id, "Spark", "send-btc"),
                "Send",
                None,
                SparkOverviewMessage::FocusAction,
                Some(SparkOverviewMessage::SendBtc),
                theme::button::primary,
            )
            .width(Length::Fixed(90.0)),
        )
        .push(
            landing_button(
                landing_control_id(cube_id, "Spark", "receive-btc"),
                "Receive",
                None,
                SparkOverviewMessage::FocusAction,
                Some(SparkOverviewMessage::ReceiveBtc),
                theme::button::orange_outline,
            )
            .width(Length::Fixed(90.0)),
        );

    let usdb_row = stable_balance.as_ref().map(|sb| {
        use crate::app::breez_spark::assets::format_token_display;
        Row::new()
            .spacing(10)
            .align_y(Alignment::Center)
            .push(text("USDB").size(P1_SIZE).bold().width(Length::Fixed(60.0)))
            .push(amount_cell::<SparkOverviewMessage>(
                if mask_amounts {
                    AmountCell::masked()
                } else {
                    AmountCell::shown(
                        format!("{} USDB", format_token_display(sb.balance, sb.decimals)),
                        None,
                    )
                },
                AmountTier::Row,
            ))
    });
    let balance_section = landing_balance_section(
        Column::new()
            .spacing(16)
            .push(total_col)
            .push(iced::widget::rule::horizontal(1))
            .push(btc_row)
            .push_maybe(usdb_row)
            .push_maybe((stable_balance_active.is_none()).then(|| {
                text("Stable Balance setting is still unknown")
                    .size(P2_SIZE)
                    .style(theme::text::secondary)
            }))
            .push_maybe(
                (stable_balance_active == Some(false)
                    && stable_balance.as_ref().is_some_and(|sb| sb.balance > 0))
                .then(|| {
                    text("USDB is held while Stable Balance is off. Review Spark settings.")
                        .size(P2_SIZE)
                        .style(theme::text::warning)
                }),
            ),
    );

    content = content.push(balance_section);
    if let Some(error) = last_error {
        content = content.push(
            text(format!(
                "Last known balance · Spark refresh failed: {error}"
            ))
            .size(P2_SIZE)
            .style(theme::text::warning),
        );
    }

    // ── Recent transactions ───────────────────────────────────────────
    content = content.push(Column::new().spacing(10).push(h4_bold("Last transactions")));

    if !recent.is_empty() {
        for tx in recent {
            let open = (!tx.id.trim().is_empty()
                && recent.iter().filter(|other| other.id == tx.id).count() == 1)
                .then(|| SparkOverviewMessage::SelectTransaction(tx.id.clone()));
            let amount = (!mask_amounts).then(|| {
                tx.token_display.clone().unwrap_or_else(|| {
                    format!(
                        "{} {}",
                        row_display_amount(tx).to_formatted_string_with_unit(bitcoin_unit),
                        bitcoin_unit.label()
                    )
                })
            });
            let amount = amount.map(|amount| {
                if tx.is_incoming {
                    format!("+{amount}")
                } else {
                    format!("−{amount}")
                }
            });
            let secondary = (!mask_amounts)
                .then_some(
                    tx.fiat_amount
                        .as_ref()
                        .filter(|_| tx.method != SparkPaymentMethod::StableBalance)
                        .map(|fiat| {
                            format!(
                                "{} {} · current rate",
                                fiat.to_formatted_string(),
                                fiat.currency()
                            )
                        }),
                )
                .flatten();
            let rail = match tx.method {
                SparkPaymentMethod::Lightning => "Lightning",
                SparkPaymentMethod::OnChainBitcoin => "Bitcoin on-chain",
                SparkPaymentMethod::Spark => "Spark",
                SparkPaymentMethod::StableBalance => "Stable Balance",
            };
            content = content.push(activity_row(
                ActivityRow {
                    identity: ActivityIdentity {
                        cube: cube_id.to_owned(),
                        wallet: "Spark".into(),
                        payment: open.as_ref().map(|_| tx.id.clone()),
                    },
                    rail: Some(rail.into()),
                    date: (tx.timestamp > 0)
                        .then(|| {
                            chrono::DateTime::<chrono::Utc>::from_timestamp(tx.timestamp as i64, 0)
                        })
                        .flatten()
                        .map(|date| {
                            date.with_timezone(&chrono::Local)
                                .format("%b %-d, %H:%M")
                                .to_string()
                        })
                        .unwrap_or_else(|| "Time unknown".into()),
                    timezone_tooltip: (tx.timestamp > 0).then(|| "Local time".into()),
                    direction: if tx.is_incoming {
                        ActivityDirection::Incoming
                    } else {
                        ActivityDirection::Outgoing
                    },
                    label: tx.description.clone(),
                    wallet_caption: None,
                    amount: amount.map_or_else(AmountCell::masked, |amount| {
                        AmountCell::shown(amount, secondary)
                    }),
                    status: tx.status.into(),
                },
                false,
                open,
                SparkOverviewMessage::FocusAction,
            ));
        }
    } else {
        content = content.push(placeholder(
            receipt_icon().size(80),
            "No transactions yet",
            "Your transaction history will appear here once you send or receive coins.",
        ));
    }

    let view_transactions_button = landing_button(
        landing_control_id(cube_id, "Spark", "history"),
        "View All Transactions",
        None,
        SparkOverviewMessage::FocusAction,
        Some(SparkOverviewMessage::History),
        landing_subtle_button_style,
    );

    content = content
        .push(iced::widget::Space::new().height(Length::Fixed(20.0)))
        .push(
            Container::new(view_transactions_button)
                .width(Length::Fill)
                .center_x(Length::Fill),
        )
        .push(iced::widget::Space::new().height(Length::Fixed(40.0)));

    Container::new(content).padding([0, 16]).into()
}

fn pending_btc_sats(recent: &[SparkRecentTransaction]) -> (u64, u64) {
    let pending_outgoing_sats = recent
        .iter()
        .filter(|t| {
            !t.is_incoming
                && t.token_display.is_none()
                && matches!(t.status, DomainPaymentStatus::Pending)
        })
        .map(|t| row_display_amount(t).to_sat())
        .sum();
    let pending_incoming_sats = recent
        .iter()
        .filter(|t| {
            t.is_incoming
                && t.token_display.is_none()
                && matches!(t.status, DomainPaymentStatus::Pending)
        })
        .map(|t| row_display_amount(t).to_sat())
        .sum();

    (pending_outgoing_sats, pending_incoming_sats)
}

fn row_display_amount(tx: &SparkRecentTransaction) -> Amount {
    if tx.is_incoming {
        tx.amount
    } else {
        tx.amount + tx.fees_sat
    }
}

fn stable_badge<'a>() -> Element<'a, SparkOverviewMessage> {
    Container::new(
        text("Stable")
            .size(11)
            .style(|_: &theme::Theme| iced::widget::text::Style {
                color: Some(color::ORANGE),
            }),
    )
    .padding([2, 8])
    .style(|_: &theme::Theme| container::Style {
        background: Some(Background::Color(color::TRANSPARENT)),
        border: iced::Border {
            color: color::ORANGE,
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn placeholder<'a, T: Into<Element<'a, SparkOverviewMessage>>>(
    icon: T,
    title: &'a str,
    subtitle: &'a str,
) -> Element<'a, SparkOverviewMessage> {
    let content = Column::new()
        .push(icon)
        .push(text(title).style(theme::text::secondary).bold())
        .push(
            text(subtitle)
                .size(P2_SIZE)
                .style(theme::text::secondary)
                .align_x(Alignment::Center),
        )
        .spacing(16)
        .align_x(Alignment::Center);

    Container::new(content)
        .width(Length::Fill)
        .padding(60)
        .center_x(Length::Fill)
        .style(|t| container::Style {
            background: Some(iced::Background::Color(t.colors.cards.simple.background)),
            border: iced::Border {
                radius: 20.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::view::shared::landing::capture;
    use crate::app::{
        state::connect::ConnectCubePanel, view,
        view::spark::settings::lightning_address::lightning_address_confirmation,
    };
    use crate::services::coincube::LightningAddress;

    #[test]
    fn spark_landing_mounts_address_confirmation_and_stable_pending_states() {
        let mut connect = ConnectCubePanel::new(
            None,
            "synthetic-cube".into(),
            "Test Cube".into(),
            "bitcoin".into(),
            false,
        );
        connect.lightning_address = Some(LightningAddress {
            lightning_address: Some("sats@coincube.io".into()),
        });
        let snapshot = SparkBalanceSnapshot {
            balance_sats: 25_053,
            stable_balance: None,
        };
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for size in [(1024, 768), (1440, 900)] {
                let address = spark_landing_address(&connect);
                let mounted = capture::inspect(
                    "spark-context-ready",
                    Column::new()
                        .push(
                            synthetic_overview(
                                SparkStatus::Connected(snapshot.clone()),
                                &[],
                                Some(true),
                                None,
                                None,
                            )
                            .map(view::Message::SparkOverview),
                        )
                        .push(spark_landing_context(
                            address,
                            Some(true),
                            false,
                            true,
                            "synthetic-cube",
                        )),
                    size,
                    appearance,
                    theme,
                );
                assert!(mounted.contains("sats@coincube.io"));
                assert!(mounted.contains("Stable Balance is ON"));
                assert!(mounted.has_focus(landing_control_id(
                    "synthetic-cube",
                    "Spark",
                    "stable-balance-toggle"
                )));

                connect.ln_editing = true;
                connect.ln_username_input = "newname".into();
                connect.ln_change_confirm_pending = Some("newname".into());
                let editing = capture::inspect(
                    "spark-context-address-editing",
                    Column::new()
                        .push(
                            synthetic_overview(
                                SparkStatus::Connected(snapshot.clone()),
                                &[],
                                Some(true),
                                None,
                                None,
                            )
                            .map(view::Message::SparkOverview),
                        )
                        .push(spark_landing_context(
                            spark_landing_address(&connect),
                            Some(true),
                            true,
                            true,
                            "synthetic-cube",
                        )),
                    size,
                    appearance,
                    theme,
                );
                assert!(editing.contains("Change your Lightning Address"));
                assert!(editing.contains("Saving…"));
                let confirmation = capture::inspect(
                    "spark-context-address-confirmation",
                    lightning_address_confirmation(&connect)
                        .expect("pending address confirmation")
                        .map(view::Message::ConnectCube),
                    size,
                    appearance,
                    theme,
                );
                assert!(confirmation.contains("Change your Lightning Address?"));
                assert!(confirmation.contains("Yes, change it"));
                connect.ln_editing = false;
                connect.ln_change_confirm_pending = None;
            }
        }
    }

    #[test]
    fn address_confirmation_overlays_the_whole_spark_page() {
        use iced::advanced::{layout, overlay, renderer::Headless, widget::Tree, Layout};
        use iced::{Rectangle, Size, Vector};

        let mut connect = ConnectCubePanel::new(
            None,
            "synthetic-cube".into(),
            "Test Cube".into(),
            "bitcoin".into(),
            false,
        );
        connect.lightning_address = Some(LightningAddress {
            lightning_address: Some("sats@coincube.io".into()),
        });
        connect.ln_editing = true;
        connect.ln_change_confirm_pending = Some("newname".into());
        let snapshot = SparkBalanceSnapshot {
            balance_sats: 25_053,
            stable_balance: None,
        };
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for (width, height) in [(1024, 768), (1440, 900)] {
                let body = Column::new()
                    .push(
                        synthetic_overview(
                            SparkStatus::Connected(snapshot.clone()),
                            &[],
                            Some(true),
                            None,
                            None,
                        )
                        .map(view::Message::SparkOverview),
                    )
                    .push(spark_landing_context(
                        spark_landing_address(&connect),
                        Some(true),
                        false,
                        true,
                        "synthetic-cube",
                    ));
                let page = iced::widget::scrollable(body)
                    .width(Length::Fill)
                    .height(Length::Fill);
                let dialog = lightning_address_confirmation(&connect)
                    .expect("pending change")
                    .map(view::Message::ConnectCube);
                let mut element: Element<'_, view::Message> =
                    coincube_ui::widget::modal::Modal::new(page, dialog)
                        .on_blur(Some(view::Message::ConnectCube(
                            ConnectCubeMessage::DismissChangeConfirmation,
                        )))
                        .into();
                let size = Size::new(width as f32, height as f32);
                let viewport = Rectangle::with_size(size);
                let mut renderer =
                    iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
                        coincube_ui::font::REGULAR,
                        iced::Pixels(14.0),
                        Some("tiny-skia"),
                    ))
                    .expect("software renderer");
                let mut tree = Tree::new(element.as_widget());
                let node = element.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, size),
                );
                assert_eq!(node.size(), size, "modal base fills Spark page");

                let mut modal = element
                    .as_widget_mut()
                    .overlay(
                        &mut tree,
                        Layout::new(&node),
                        &renderer,
                        &viewport,
                        Vector::ZERO,
                    )
                    .map(overlay::Nested::new)
                    .expect("address confirmation overlay");
                let overlay_node = modal.layout(&renderer, size);
                let dialog_bounds = overlay_node
                    .children()
                    .next()
                    .expect("dialog layout")
                    .bounds();
                assert_eq!(overlay_node.size(), size, "whole-page scrim");
                assert!(dialog_bounds.x > 100.0 && dialog_bounds.y > 50.0);
                assert!(dialog_bounds.x + dialog_bounds.width < size.width);
                assert!(dialog_bounds.y + dialog_bounds.height < size.height);
                drop(modal);

                if let Ok(directory) = std::env::var("R06A_CAPTURE_DIR") {
                    std::fs::create_dir_all(&directory).expect("capture directory");
                    iced::advanced::Renderer::reset(&mut renderer, viewport);
                    let style = iced::advanced::renderer::Style {
                        text_color: theme.colors.text.primary,
                    };
                    element.as_widget().draw(
                        &tree,
                        &mut renderer,
                        &theme,
                        &style,
                        Layout::new(&node),
                        iced::mouse::Cursor::Unavailable,
                        &viewport,
                    );
                    if let Some(mut modal) = element
                        .as_widget_mut()
                        .overlay(
                            &mut tree,
                            Layout::new(&node),
                            &renderer,
                            &viewport,
                            Vector::ZERO,
                        )
                        .map(overlay::Nested::new)
                    {
                        let overlay_node = modal.layout(&renderer, size);
                        modal.draw(
                            &mut renderer,
                            &theme,
                            &style,
                            Layout::new(&overlay_node),
                            iced::mouse::Cursor::Unavailable,
                        );
                    }
                    let pixels = renderer.screenshot(
                        Size::new(width, height),
                        1.0,
                        theme.colors.general.background,
                    );
                    image::save_buffer(
                        std::path::Path::new(&directory).join(format!(
                            "spark-address-modal-{appearance}-{width}x{height}-synthetic.png"
                        )),
                        &pixels,
                        width,
                        height,
                        image::ColorType::Rgba8,
                    )
                    .expect("save modal capture");
                }
            }
        }
    }

    fn tx(
        id: &str,
        is_incoming: bool,
        status: DomainPaymentStatus,
        amount_sat: u64,
        fees_sat: u64,
        token_display: Option<&str>,
    ) -> SparkRecentTransaction {
        SparkRecentTransaction {
            id: id.to_string(),
            description: id.to_string(),
            time_ago: "just now".to_string(),
            timestamp: 1_700_000_000,
            amount: Amount::from_sat(amount_sat),
            fees_sat: Amount::from_sat(fees_sat),
            fiat_amount: None,
            is_incoming,
            status,
            method: if token_display.is_some() {
                SparkPaymentMethod::StableBalance
            } else {
                SparkPaymentMethod::Spark
            },
            token_display: token_display.map(str::to_string),
        }
    }

    #[test]
    fn pending_btc_sats_counts_pending_incoming_and_outgoing_totals() {
        let rows = vec![
            tx(
                "incoming",
                true,
                DomainPaymentStatus::Pending,
                1_000,
                12,
                None,
            ),
            tx(
                "outgoing",
                false,
                DomainPaymentStatus::Pending,
                2_000,
                34,
                None,
            ),
        ];

        assert_eq!(pending_btc_sats(&rows), (2_034, 1_000));
    }

    #[test]
    fn pending_btc_sats_skips_completed_failed_and_token_rows() {
        let rows = vec![
            tx(
                "complete",
                true,
                DomainPaymentStatus::Complete,
                1_000,
                0,
                None,
            ),
            tx(
                "failed",
                false,
                DomainPaymentStatus::Failed,
                2_000,
                40,
                None,
            ),
            tx(
                "token",
                true,
                DomainPaymentStatus::Pending,
                999_999,
                0,
                Some("1.23 USDB"),
            ),
        ];

        assert_eq!(pending_btc_sats(&rows), (0, 0));
    }

    #[test]
    fn row_display_amount_adds_fees_only_for_outgoing_rows() {
        assert_eq!(
            row_display_amount(&tx(
                "incoming",
                true,
                DomainPaymentStatus::Pending,
                100,
                9,
                None
            ))
            .to_sat(),
            100
        );
        assert_eq!(
            row_display_amount(&tx(
                "outgoing",
                false,
                DomainPaymentStatus::Pending,
                100,
                9,
                None
            ))
            .to_sat(),
            109
        );
    }

    fn synthetic_overview<'a>(
        status: SparkStatus,
        rows: &'a [SparkRecentTransaction],
        active: Option<bool>,
        price: Option<f64>,
        last_error: Option<&'a str>,
    ) -> Element<'a, SparkOverviewMessage> {
        SparkOverviewView {
            status,
            recent_transactions: rows,
            cube_id: "synthetic-cube",
            last_error,
            fiat_converter: None,
            bitcoin_unit: BitcoinDisplayUnit::Sats,
            show_direction_badges: true,
            stable_balance_active: active,
            btc_usd_price: price,
            display_mode: DisplayMode::BitcoinNative,
            mask_amounts: false,
        }
        .render()
    }

    #[test]
    fn hidden_overview_redacts_btc_usdb_and_activity_amounts() {
        let stable = coincube_spark_protocol::StableBalanceSnapshot {
            balance: 2_500_000,
            decimals: 6,
            ticker: "USDB".into(),
        };
        let snapshot = SparkBalanceSnapshot {
            balance_sats: 12_345_678,
            stable_balance: Some(stable),
        };
        let rows = [tx(
            "payment",
            true,
            DomainPaymentStatus::Complete,
            98_765_432,
            0,
            None,
        )];
        let mounted = capture::inspect(
            "spark-hidden-amounts",
            SparkOverviewView {
                status: SparkStatus::Connected(snapshot),
                recent_transactions: &rows,
                cube_id: "synthetic-cube",
                last_error: None,
                fiat_converter: None,
                bitcoin_unit: BitcoinDisplayUnit::Sats,
                show_direction_badges: true,
                stable_balance_active: Some(true),
                btc_usd_price: Some(50_000.0),
                display_mode: DisplayMode::BitcoinNative,
                mask_amounts: true,
            }
            .render(),
            (1024, 768),
            "dark",
            theme::Theme::dark(),
        );
        assert!(mounted.contains("••••••"));
        for amount in ["12,345,678", "98,765,432", "2.50 USDB"] {
            assert!(!mounted.contains(amount), "hidden Spark amount {}", amount);
        }
    }

    #[test]
    fn r06a_spark_headless_content_matrix() {
        let stable = coincube_spark_protocol::StableBalanceSnapshot {
            balance: 2_500_000,
            decimals: 6,
            ticker: "USDB".into(),
        };
        let snapshot = SparkBalanceSnapshot {
            balance_sats: 125_000,
            stable_balance: Some(stable),
        };
        let rows = vec![
            tx(
                "payment-a",
                true,
                DomainPaymentStatus::Complete,
                5_000,
                0,
                None,
            ),
            tx(
                "payment-b",
                false,
                DomainPaymentStatus::Pending,
                7_000,
                15,
                None,
            ),
        ];
        let adverse = [
            DomainPaymentStatus::Created,
            DomainPaymentStatus::Pending,
            DomainPaymentStatus::Complete,
            DomainPaymentStatus::Failed,
            DomainPaymentStatus::TimedOut,
            DomainPaymentStatus::Refundable,
            DomainPaymentStatus::RefundPending,
            DomainPaymentStatus::WaitingFeeAcceptance,
        ]
        .iter()
        .enumerate()
        .map(|(index, status)| {
            let mut payment = tx(
                &format!("payment-{index}"),
                index % 2 == 0,
                *status,
                99_999_999_999,
                1,
                None,
            );
            payment.description = "A long Spark payment description with a beneficiary name and contextual details that should wrap safely in compact mode".into();
            payment
        })
        .collect::<Vec<_>>();
        for (appearance, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for outer in [(1024, 768), (1440, 900)] {
                let normal = capture::inspect(
                    "spark-normal",
                    synthetic_overview(
                        SparkStatus::Connected(snapshot.clone()),
                        &rows,
                        Some(true),
                        Some(50_000.0),
                        None,
                    ),
                    outer,
                    appearance,
                    theme,
                );
                assert!(normal.contains("Last transactions"));
                assert!(normal.contains("2.50 USDB"));
                for control in ["flip-balance", "send-btc", "receive-btc", "history"] {
                    assert!(normal.has_focus(landing_control_id(
                        "synthetic-cube",
                        "Spark",
                        control
                    )));
                }
                let missing_quote = capture::inspect(
                    "spark-missing-quote-held-off",
                    synthetic_overview(
                        SparkStatus::Connected(snapshot.clone()),
                        &rows,
                        Some(false),
                        None,
                        Some("bridge refresh failed"),
                    ),
                    outer,
                    appearance,
                    theme,
                );
                assert!(missing_quote.contains("2.50 USDB"));
                assert!(missing_quote.contains("needs a BTC/USD quote"));
                assert!(missing_quote.contains("held while Stable Balance is off"));
                assert!(missing_quote.contains("Last known balance"));
                let unknown_setting = capture::inspect(
                    "spark-setting-unknown-loaded-empty",
                    synthetic_overview(
                        SparkStatus::Connected(snapshot.clone()),
                        &[],
                        None,
                        Some(50_000.0),
                        None,
                    ),
                    outer,
                    appearance,
                    theme,
                );
                assert!(unknown_setting.contains("setting is still unknown"));
                assert!(unknown_setting.contains("No transactions yet"));
                assert!(unknown_setting.contains("View All Transactions"));
                let unavailable = capture::inspect(
                    "spark-bridge-unavailable",
                    synthetic_overview(SparkStatus::Unavailable, &[], None, None, None),
                    outer,
                    appearance,
                    theme,
                );
                assert!(unavailable.contains("not configured"));
                let statuses = capture::inspect(
                    "spark-all-statuses-long-labels",
                    synthetic_overview(
                        SparkStatus::Connected(snapshot.clone()),
                        &adverse,
                        Some(true),
                        Some(50_000.0),
                        None,
                    ),
                    outer,
                    appearance,
                    theme,
                );
                assert!(statuses.contains("Refund available"));
                assert!(statuses.contains("Review fee"));
            }
        }
    }
}
