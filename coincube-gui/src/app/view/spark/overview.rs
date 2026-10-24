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
    icon::{self, receipt_icon},
    theme,
    widget::*,
};
use iced::{
    widget::{button as iced_button, container, Column, Container, Row},
    Alignment, Background, Length,
};

use crate::app::settings::display::DisplayMode;
use crate::app::state::spark::overview::SparkBalanceSnapshot;
use crate::app::view::shared::activity_row::{
    activity_row, ActivityDirection, ActivityIdentity, ActivityRow,
};
use crate::app::view::shared::landing::{amount_cell, AmountCell, AmountTier};
use crate::app::view::vault::fiat::FiatAmount;
use crate::app::view::wallet_header::{wallet_header, HeaderVariant, SyncState, WalletHeaderProps};
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
        .push(if has_unvalued_usdb {
            amount_cell::<SparkOverviewMessage>(
                AmountCell::unavailable("Combined BTC and USDB valuation needs a BTC/USD quote"),
                AmountTier::Card,
            )
        } else {
            wallet_header::<SparkOverviewMessage>(WalletHeaderProps {
                sats: total_balance,
                fiat: total_fiat,
                balance_masked: false,
                bitcoin_unit,
                variant: HeaderVariant::Landing,
                sync: SyncState::Synced,
                unconfirmed: None,
                pending_send_sats: pending_outgoing_sats,
                pending_receive_sats: pending_incoming_sats,
                display_mode,
                on_swap: Some(SparkOverviewMessage::FlipDisplayMode),
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
            AmountCell::shown(
                btc_row_amount.to_formatted_string_with_unit(bitcoin_unit),
                (!btc_fiat_str.is_empty()).then_some(btc_fiat_str),
            ),
            AmountTier::Row,
        ))
        .push(iced::widget::Space::new().width(Length::Fill))
        .push(
            button::primary(None, "Send")
                .on_press(SparkOverviewMessage::SendBtc)
                .width(Length::Fixed(90.0)),
        )
        .push(
            button::orange_outline(None, "Receive")
                .on_press(SparkOverviewMessage::ReceiveBtc)
                .width(Length::Fixed(90.0)),
        );

    let usdb_row = stable_balance.as_ref().map(|sb| {
        use crate::app::breez_spark::assets::format_token_display;
        Row::new()
            .spacing(10)
            .align_y(Alignment::Center)
            .push(text("USDB").size(P1_SIZE).bold().width(Length::Fixed(60.0)))
            .push(amount_cell::<SparkOverviewMessage>(
                AmountCell::shown(
                    format!("{} USDB", format_token_display(sb.balance, sb.decimals)),
                    None,
                ),
                AmountTier::Row,
            ))
    });
    let portfolio_card = Container::new(
        Column::new()
            .spacing(16)
            .push(total_col)
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
    )
    .padding(20)
    .width(Length::Fill)
    .style(|t| container::Style {
        background: Some(Background::Color(t.colors.cards.simple.background)),
        border: iced::Border {
            color: color::ORANGE,
            width: 0.2,
            radius: 25.0.into(),
        },
        ..Default::default()
    });

    content = content.push(portfolio_card);
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
            let amount = tx.token_display.clone().unwrap_or_else(|| {
                row_display_amount(tx).to_formatted_string_with_unit(bitcoin_unit)
            });
            let secondary = tx
                .fiat_amount
                .as_ref()
                .filter(|_| tx.method != SparkPaymentMethod::StableBalance)
                .map(|fiat| {
                    format!(
                        "{} {} · current rate",
                        fiat.to_formatted_string(),
                        fiat.currency()
                    )
                });
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
                    amount: AmountCell::shown(amount, secondary),
                    status: tx.status.into(),
                },
                false,
                open,
            ));
        }
    } else {
        content = content.push(placeholder(
            receipt_icon().size(80),
            "No transactions yet",
            "Your transaction history will appear here once you send or receive coins.",
        ));
    }

    let view_transactions_button = {
        let btn_icon = icon::history_icon()
            .size(18)
            .style(|_theme: &theme::Theme| iced::widget::text::Style {
                color: Some(color::ORANGE),
            });

        let label = text("View All Transactions")
            .size(15)
            .style(|_theme: &theme::Theme| iced::widget::text::Style {
                color: Some(color::ORANGE),
            });

        let button_content = Row::new()
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center)
            .push(btn_icon)
            .push(label);

        iced_button(Container::new(button_content).padding([10, 20]).style(
            |_theme: &theme::Theme| container::Style {
                background: Some(Background::Color(color::TRANSPARENT)),
                border: iced::Border {
                    color: color::ORANGE,
                    width: 1.5,
                    radius: 20.0.into(),
                },
                ..Default::default()
            },
        ))
        .style(|_theme: &theme::Theme, _| iced_button::Style {
            background: Some(Background::Color(color::TRANSPARENT)),
            text_color: color::ORANGE,
            border: iced::Border {
                radius: 20.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .on_press(SparkOverviewMessage::History)
    };

    content = content
        .push(iced::widget::Space::new().height(Length::Fixed(20.0)))
        .push(
            Container::new(view_transactions_button)
                .width(Length::Fill)
                .center_x(Length::Fill),
        )
        .push(iced::widget::Space::new().height(Length::Fixed(40.0)));

    content.into()
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
}
