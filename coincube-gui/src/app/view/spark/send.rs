//! View renderer for [`crate::app::state::spark::send::SparkSend`].
//!
//! Phase 4c ships the minimum viable Send UI: destination input, amount
//! input, Prepare / Confirm / Try again buttons, and tiny status cards
//! for each phase of the state machine. Intentionally plain —
//! polish (fee tier picker, fiat estimate, address book, QR scanner)
//! lands in a later phase once the bridge write path has soaked a bit.

use coincube_ui::{
    color,
    component::{
        amount::{BitcoinDisplayUnit, DisplayAmount},
        button,
        text::*,
    },
    icon,
    image::{asset_logo, asset_network_logo},
    theme,
    widget::{action::Action, Column, ColumnExt, Container, Element, Row, RowExt},
};
use iced::{
    widget::{container, text_input, Space},
    Alignment, Background, Length,
};

use coincube_core::miniscript::bitcoin::{Amount, Network};

use crate::app::breez_spark::assets::stable_token_as_sats;
use crate::app::state::spark::cross_chain::{self, supported_on};
use crate::app::state::spark::send::{CrossChainContext, SparkSendPhase, SparkSendTarget};
use crate::app::view::shared::picker::{picker_modal_card, picker_row_selected};
use crate::app::view::spark::{last_tx::last_transactions_section, SparkRecentTransaction};
use crate::app::view::{Message, SparkSendMessage};

pub struct SparkSendView<'a> {
    pub backend_available: bool,
    pub destination_input: &'a str,
    pub amount_input: &'a str,
    /// Whether `amount_input` was filled from the destination invoice rather
    /// than typed. The field then shows the amount but doesn't accept edits —
    /// the invoice fixes what gets paid, so an editable field would promise a
    /// choice the payer doesn't have.
    pub amount_set_by_invoice: bool,
    pub phase: &'a SparkSendPhase,
    pub recipient_identities: &'a [crate::services::branta::RecipientIdentity],
    pub recipient_mismatch: bool,
    pub theme_mode: theme::palette::ThemeMode,
    pub sent_amount_display: &'a str,
    pub sent_celebration_context: &'a str,
    pub sent_quote: &'a coincube_ui::component::quote_display::Quote,
    pub sent_image_handle: &'a iced::widget::image::Handle,
    pub recent_transactions: &'a [SparkRecentTransaction],
    /// Unified balance (sats: BTC + Stable Balance), shown on the YOU SEND card.
    pub balance_sats: u64,
    /// Line under the Amount field when the entered amount exceeds the
    /// bitcoin balance and USDB will cover the rest — see
    /// `state::spark::send::usdb_conversion_hint`.
    pub usdb_conversion_hint: Option<String>,
    pub bitcoin_unit: BitcoinDisplayUnit,
    /// BTC/USD reference price for the cross-chain conversion-fee sats estimate.
    /// `None` when no price is known — the fee then shows in the asset only.
    pub reference_btc_usd_price: Option<f64>,
    pub show_direction_badges: bool,
    pub mask_amounts: bool,
    /// The "THEY RECEIVE" selection — drives the two-card selector and the
    /// destination placeholder.
    pub receive_target: SparkSendTarget,
    /// Bitcoin network the cube runs on — gates the stablecoin picker options
    /// (cross-chain is mainnet-only).
    pub network: Network,
    /// The cross-chain destination + routes for the current send, when there is
    /// one. Lives on the panel rather than in the phase because it must survive
    /// a failed send — a retry re-prepares from it.
    pub cross_chain_ctx: Option<&'a CrossChainContext>,
    /// Cross-chain slippage tolerance, in basis points, as typed. Empty means
    /// the SDK default. Only rendered behind the advanced disclosure.
    pub slippage_input: &'a str,
    /// Whether the advanced (slippage) disclosure is open.
    pub advanced_open: bool,
    /// Life left on the live cross-chain quote. `None` means there is no quote
    /// on screen — *not* "expired", which is its own variant. Anything other
    /// than `Valid` replaces Confirm with a re-quote.
    pub quote_countdown: Option<cross_chain::QuoteCountdown>,
}

impl<'a> SparkSendView<'a> {
    pub fn render(self) -> Element<'a, Message> {
        if !self.backend_available {
            return Column::new()
                .spacing(20)
                .push(p1_regular(
                    "Spark is not available for this cube. Set up a Spark \
                     signer to send payments.",
                ))
                .into();
        }

        // ── Full-screen celebration for successful sends ─────────────
        if matches!(self.phase, SparkSendPhase::Sent(_)) {
            return coincube_ui::component::sent_celebration_page(
                self.sent_celebration_context,
                self.sent_amount_display,
                self.sent_quote,
                self.sent_image_handle,
                "has been sent successfully.",
                Message::SparkSend(crate::app::view::SparkSendMessage::Reset),
            );
        }

        // ── Payment details card ──────────────────────────────────────
        // Route tiles, Destination, Amount and — while composing — the
        // Prepare footer, on one surface (P-Send). Each slot below stays in
        // place whatever the phase, so the inputs keep their widget state
        // when a note, a hint or the loading line comes and goes.
        let destination = text_input(
            self.receive_target.destination_placeholder(),
            self.destination_input,
        )
        .id(DESTINATION_INPUT_ID)
        .on_input(|v| {
            Message::SparkSend(crate::app::view::SparkSendMessage::DestinationInputChanged(
                v,
            ))
        })
        .padding(12);

        // The amount field follows the wallet's display unit, like the Vault
        // send form: label, placeholder, and the parse in state all switch on
        // it, so a BTC-configured wallet enters BTC and a sats one enters sats.
        let is_btc = matches!(self.bitcoin_unit, BitcoinDisplayUnit::BTC);
        let amount_label = if is_btc {
            "Amount (BTC)"
        } else {
            "Amount (sats)"
        };
        let amount_placeholder = match (self.receive_target.is_stablecoin(), is_btc) {
            (true, true) => "Amount in BTC — funded from your Bitcoin balance",
            (true, false) => "Amount in sats — funded from your Bitcoin balance",
            (false, true) => "Amount in BTC (optional for invoices with amount)",
            (false, false) => "Amount in sats (optional for invoices with amount)",
        };
        // An invoice-set amount leaves the field without `on_input`, which is
        // what makes it read-only — it still shows and can be selected, it just
        // can't be edited into something the invoice won't honour.
        let mut amount = text_input(amount_placeholder, self.amount_input)
            .id(AMOUNT_INPUT_ID)
            .padding(12);
        if !self.amount_set_by_invoice {
            amount = amount.on_input(|v| {
                Message::SparkSend(crate::app::view::SparkSendMessage::AmountInputChanged(v))
            });
        }

        // Both notes share one slot below the field, never above it.
        let amount_notes = Column::new()
            .spacing(6)
            .push_maybe(self.amount_set_by_invoice.then(|| {
                p2_regular("This invoice sets its own amount.").style(theme::text::secondary)
            }))
            .push_maybe(
                self.usdb_conversion_hint
                    .clone()
                    .filter(|_| !self.mask_amounts)
                    .map(|hint| p2_regular(hint).style(theme::text::secondary)),
            );

        // Unified balance (BTC + Stable Balance), as the YOU SEND card showed it.
        let available = Row::new()
            .spacing(6)
            .push(Space::new().width(Length::Fill))
            .push(p2_regular("Available").style(theme::text::secondary))
            .push(
                text(if self.mask_amounts {
                    "••••••".to_owned()
                } else {
                    format!(
                        "{} {}",
                        Amount::from_sat(self.balance_sats)
                            .to_formatted_string_with_unit(self.bitcoin_unit),
                        if matches!(self.bitcoin_unit, BitcoinDisplayUnit::BTC) {
                            "BTC"
                        } else {
                            "SATS"
                        }
                    )
                })
                .size(P2_SIZE)
                .style(theme::text::secondary),
            );

        // Idle and Preparing render as this card's footer. Every other phase
        // — and the recipient mismatch, whatever the phase — keeps its own
        // card below, exactly as before.
        let footer_in_card = !self.recipient_mismatch
            && matches!(self.phase, SparkSendPhase::Idle | SparkSendPhase::Preparing);
        let body = phase_body(&self);
        let (footer, below) = if footer_in_card {
            (Some(body), None)
        } else {
            (None, Some(body))
        };

        let card = Container::new(
            Column::new()
                .push(
                    Container::new(h4_bold("Payment details"))
                        .padding([18, 24])
                        .width(Length::Fill),
                )
                .push(iced::widget::rule::horizontal(1))
                .push(card_section(spark_send_cards(
                    self.receive_target,
                    self.destination_input.is_empty(),
                )))
                .push(iced::widget::rule::horizontal(1))
                .push(card_section(
                    Column::new()
                        .spacing(10)
                        .push(p1_bold("Destination"))
                        .push(destination),
                ))
                .push(iced::widget::rule::horizontal(1))
                .push(card_section(
                    Column::new()
                        .spacing(10)
                        .push(p1_bold(amount_label))
                        .push(amount)
                        .push(amount_notes)
                        .push(available),
                ))
                .push(Column::new().push_maybe(footer.map(|footer| {
                    Column::new()
                        .push(iced::widget::rule::horizontal(1))
                        .push(card_section(footer))
                }))),
        )
        .width(Length::Fill)
        .style(payment_card_style);

        let mut content = Column::new()
            .push(card)
            // ── Phase-specific body (every phase but Idle/Preparing) ──
            .push(Column::new().push_maybe(below.map(|body| {
                Column::new()
                    .push(Space::new().height(Length::Fixed(20.0)))
                    .push(body)
            })))
            .push(Space::new().height(Length::Fixed(20.0)));

        // ── Last transactions ─────────────────────────────────────────
        content = content.push(last_transactions_section(
            self.recent_transactions,
            self.bitcoin_unit,
            self.show_direction_badges,
            self.mask_amounts,
            |idx| Message::SparkSend(SparkSendMessage::SelectTransaction(idx)),
            Message::SparkSend(SparkSendMessage::History),
        ));

        content.into()
    }
}

fn phase_body<'a>(view: &SparkSendView<'a>) -> Element<'a, Message> {
    let phase = view.phase;
    let cross_chain_ctx = view.cross_chain_ctx;
    let slippage_input = view.slippage_input;
    let advanced_open = &view.advanced_open;
    let quote_countdown = view.quote_countdown.clone();
    let reference_btc_usd_price = view.reference_btc_usd_price;
    let bitcoin_unit = view.bitcoin_unit;
    let recipient_identities = view.recipient_identities;
    let recipient_mismatch = view.recipient_mismatch;
    let theme_mode = view.theme_mode;
    use crate::app::view::SparkSendMessage;
    use coincube_ui::component::amount::format_u64_as_string;

    if recipient_mismatch {
        return Container::new(
            Column::new()
                .spacing(12)
                .push(p1_regular(crate::services::branta::MISMATCH_MESSAGE))
                .push(
                    button::primary(None, "Replace payment request")
                        .on_press(Message::SparkSend(SparkSendMessage::Reset)),
                ),
        )
        .padding(16)
        .style(theme::card::simple)
        .into();
    }

    match phase {
        // Idle and Preparing are the Payment details card's footer (see
        // `render`). Prepare stays available whatever is typed: the state
        // parses the input and reports what is wrong, so the view does not
        // second-guess it with a validator of its own.
        SparkSendPhase::Idle => compose_footer(
            "Prepare gets a fee quote. Nothing is sent yet.",
            Some(Message::SparkSend(SparkSendMessage::PrepareRequested)),
        ),

        // Same slot, same Prepare — without a press, so a held key or a
        // pointer release cannot reach it while the quote is being fetched.
        SparkSendPhase::Preparing => compose_footer(
            "Preparing send… asking the Spark bridge for a fee quote.",
            None,
        ),

        // The chain/asset confirmation stop. A cross-chain address does not
        // announce its network, and USDT exists on all of them — so the
        // detected chain and asset are spelled out in words, and the user has
        // to choose a route before anything is quoted.
        SparkSendPhase::CrossChainRoutes => {
            let Some(ctx) = cross_chain_ctx else {
                // The phase can't be reached without a context — but render an
                // honest message rather than a blank card if it ever is.
                return Container::new(p1_regular("No cross-chain destination selected."))
                    .padding(16)
                    .style(theme::card::simple)
                    .into();
            };
            let (address, routes, selected) = (&ctx.address, &ctx.routes, &ctx.selected);

            let mut col = Column::new()
                .spacing(12)
                .push(h4_bold("Confirm the network"))
                .push(kv_row("Address", address.address.clone()));

            if let Some(route) = routes.get(*selected) {
                col = col.push(p1_regular(cross_chain::chain_confirmation(address, route)));
            }

            // One row per route. Kept as explicit rows rather than a dropdown:
            // the provider and destination chain are the whole decision here,
            // and a collapsed picker is the kind of thing users click past.
            // The shared `button::*` helpers take `&'static str`, and a route
            // label is built at render time — so these are hand-rolled with the
            // same theme styles rather than leaking the strings.
            let mut route_rows = Column::new().spacing(6);
            for (idx, route) in routes.iter().enumerate() {
                let label = iced::widget::text(format!(
                    "{} on {} — via {}",
                    route.asset, route.chain, route.provider
                ))
                .align_y(iced::Alignment::Center);
                let style = if idx == *selected {
                    theme::button::primary
                } else {
                    theme::button::container_border
                };
                route_rows = route_rows.push(
                    iced::widget::button(label)
                        .style(style)
                        .padding(10)
                        .width(Length::Fill)
                        .on_press(Message::SparkSend(
                            SparkSendMessage::CrossChainRouteSelected(idx),
                        )),
                );
            }
            col = col.push(route_rows);

            // Slippage lives behind a disclosure. A normal user should never
            // have to reason in basis points; the SDK default (1%) is right for
            // them, and surfacing the control by default just invites someone
            // to type a number they don't understand into a money path.
            col = col.push(
                button::transparent_border(
                    None,
                    if *advanced_open {
                        "Hide advanced"
                    } else {
                        "Advanced"
                    },
                )
                .on_press(Message::SparkSend(SparkSendMessage::ToggleAdvanced))
                .width(Length::Fixed(160.0)),
            );
            if *advanced_open {
                col = col
                    .push(p2_regular(format!(
                        "Max slippage, in basis points ({}–{}). Leave blank for the default of \
                         {} ({}%).",
                        cross_chain::MIN_SLIPPAGE_BPS,
                        cross_chain::MAX_SLIPPAGE_BPS,
                        cross_chain::DEFAULT_SLIPPAGE_BPS,
                        cross_chain::DEFAULT_SLIPPAGE_BPS as f64 / 100.0,
                    )))
                    .push(
                        text_input("100", slippage_input)
                            .on_input(|v| Message::SparkSend(SparkSendMessage::SlippageChanged(v)))
                            .padding(10),
                    );
            }

            col = col.push(Space::new().height(Length::Fixed(8.0))).push(
                Row::new()
                    .spacing(10)
                    .push(
                        button::primary(None, "Get quote")
                            .on_press(Message::SparkSend(
                                SparkSendMessage::CrossChainQuoteRequested,
                            ))
                            .width(Length::Fixed(160.0)),
                    )
                    .push(
                        button::transparent_border(None, "Cancel")
                            .on_press(Message::SparkSend(SparkSendMessage::Reset))
                            .width(Length::Fixed(120.0)),
                    ),
            );

            Container::new(col)
                .padding(16)
                .style(theme::card::simple)
                .into()
        }

        SparkSendPhase::Prepared(ok) => {
            // Cross-chain quotes are denominated in the destination asset and
            // they expire, so they get their own preview rather than being
            // squeezed into the sats-shaped one.
            if let Some(quote) = &ok.cross_chain {
                // A quote whose expiry we couldn't parse counts as expired: an
                // unreadable expiry is not evidence the quote is *good*, and
                // re-quoting costs a second where sending against an
                // unknown-age rate costs money. `None` here means there is no
                // quote at all, which can't happen inside this branch — the
                // state sets the countdown the moment a quote lands.
                let expired = !matches!(
                    quote_countdown,
                    Some(cross_chain::QuoteCountdown::Valid { .. })
                );
                let mut col = Column::new()
                    .spacing(14)
                    .push(h4_bold("Preview"))
                    .push(kv_row(
                        "They receive",
                        format!(
                            "≈ {} {}",
                            cross_chain::format_asset_amount(
                                quote.estimated_out,
                                quote.route.decimals
                            ),
                            quote.route.asset,
                        ),
                    ))
                    .push(kv_row(
                        "Network",
                        format!("{} — via {}", quote.route.chain, quote.route.provider),
                    ))
                    // The old single mixed-unit "Fee" line hid how much each part
                    // cost. Split it: a sats network fee (moving BTC to the
                    // provider) and a destination-asset conversion fee (provider
                    // spread + gas, already netted out of "They receive").
                    .push(kv_row(
                        "Network fee",
                        format!(
                            "{} sats",
                            format_u64_as_string(quote.source_transfer_fee_sats, ",")
                        ),
                    ))
                    .push(kv_row(
                        "Conversion fee",
                        conversion_fee_display(
                            quote.fee_amount,
                            quote.route.decimals,
                            &quote.route.asset,
                            reference_btc_usd_price,
                            bitcoin_unit,
                        ),
                    ))
                    // Headline: the full sats debit from the wallet, fees
                    // included. Set apart at the bottom so the user sees exactly
                    // what they'll pay — and it matches the amount the post-send
                    // celebration reports as sent.
                    .push(Space::new().height(Length::Fixed(4.0)))
                    .push(total_row(
                        "Total you send",
                        format!("{} sats", format_u64_as_string(ok.amount_sat, ",")),
                    ));

                // The countdown. Sending against a dead quote means sending
                // against a rate the provider no longer honours, so Confirm is
                // replaced — not merely disabled — once it runs out.
                col = col.push(match quote_countdown {
                    Some(cross_chain::QuoteCountdown::Valid { seconds_left }) => {
                        p2_regular(format!("Quote valid for {}s.", seconds_left))
                    }
                    _ => p1_regular("This quote has expired. Get a fresh one to continue."),
                });

                let action = if expired {
                    button::primary(None, "Get a fresh quote")
                        .on_press(Message::SparkSend(SparkSendMessage::ReQuoteRequested))
                        .width(Length::Fixed(200.0))
                } else {
                    button::primary(None, "Confirm and send")
                        .on_press(Message::SparkSend(SparkSendMessage::ConfirmRequested))
                        .width(Length::Fixed(200.0))
                };

                col = col.push(Space::new().height(Length::Fixed(8.0))).push(
                    Row::new().spacing(10).push(action).push(
                        button::transparent_border(None, "Cancel")
                            .on_press(Message::SparkSend(SparkSendMessage::Reset))
                            .width(Length::Fixed(120.0)),
                    ),
                );

                return Container::new(col)
                    .padding(16)
                    .style(theme::card::simple)
                    .into();
            }

            let identities = Column::with_children(recipient_identities.iter().enumerate().map(
                |(index, identity)| {
                    crate::app::view::shared::recipient_identity::card(
                        identity,
                        Message::SparkSend(SparkSendMessage::OpenRecipientIdentity(index)),
                        theme_mode,
                    )
                },
            ))
            .spacing(8);
            let confirm = button::primary(None, "Confirm and send")
                .on_press_maybe(
                    (!recipient_mismatch)
                        .then_some(Message::SparkSend(SparkSendMessage::ConfirmRequested)),
                )
                .width(Length::Fixed(200.0));
            Container::new(
                Column::new()
                    .spacing(14)
                    .push(h4_bold("Preview"))
                    .push(kv_row("Method", ok.method.clone()))
                    .push(kv_row("Amount", format!("{} sats", ok.amount_sat)))
                    .push(kv_row("Fee", format!("{} sats", ok.fee_sat)))
                    .push(kv_row(
                        "Total",
                        format!("{} sats", ok.amount_sat.saturating_add(ok.fee_sat)),
                    ))
                    .push_maybe(ok.method.starts_with("Spark").then(|| {
                        p2_regular(
                            "Spark transfer — instant, lower fee than Lightning or on-chain.",
                        )
                    }))
                    .push(identities)
                    .push_maybe(recipient_mismatch.then(|| p1_regular(
                        "This payment request’s address does not match its verification data. Do not send. Return and replace the payment request."
                    )))
                    .push(Space::new().height(Length::Fixed(8.0)))
                    .push(
                        Row::new()
                            .spacing(10)
                            .push(confirm)
                            .push(
                                button::transparent_border(None, "Cancel")
                                    .on_press(Message::SparkSend(SparkSendMessage::Reset))
                                    .width(Length::Fixed(120.0)),
                            ),
                    ),
            )
            .padding(16)
            .style(theme::card::simple)
            .into()
        }

        // A failed cross-chain send. What we offer here depends entirely on
        // whether a retry can double-pay — see `RetryPolicy`. When it can, the
        // user gets "check status", never "try again".
        SparkSendPhase::CrossChainFailed { message, policy } => {
            let mut actions = Row::new().spacing(10);
            if policy.may_retry() {
                actions = actions.push(
                    // Not `ConfirmRequested` (that only fires from `Prepared`).
                    // `CrossChainRetryRequested` re-sends the same retained quote
                    // — same swap id, so the BTC leg can't pay twice — or, once
                    // the quote has expired, downgrades to "check status".
                    button::primary(None, "Try again")
                        .on_press(Message::SparkSend(
                            SparkSendMessage::CrossChainRetryRequested,
                        ))
                        .width(Length::Fixed(160.0)),
                );
            } else {
                actions = actions.push(
                    button::primary(None, "Check status")
                        .on_press(Message::SparkSend(SparkSendMessage::History))
                        .width(Length::Fixed(160.0)),
                );
            }
            actions = actions.push(
                button::transparent_border(None, "Start over")
                    .on_press(Message::SparkSend(SparkSendMessage::Reset))
                    .width(Length::Fixed(140.0)),
            );

            Container::new(
                Column::new()
                    .spacing(12)
                    .push(h4_bold("Send failed"))
                    .push(p1_regular(message.clone()))
                    .push(p2_regular(policy.guidance()))
                    .push(Space::new().height(Length::Fixed(8.0)))
                    .push(actions),
            )
            .padding(16)
            .style(theme::card::simple)
            .into()
        }

        SparkSendPhase::Sending => Container::new(Column::new().spacing(10).push(p1_regular(
            "Sending… waiting for the Spark SDK to settle the payment.",
        )))
        .padding(16)
        .style(theme::card::simple)
        .into(),

        // A dispatched payment with no answer. The heading is the whole point:
        // this must never read as "failed", because the payment may well have
        // gone through — and until the check finishes the only thing offered is
        // waiting, plus a read-only look at the transaction list. Nothing that
        // could send it a second time.
        SparkSendPhase::OutcomeUnknown {
            message,
            outcome,
            checking,
            guard,
        } => {
            let guard = *guard;
            let mut content = Column::new()
                .spacing(12)
                .push(h4_bold("Payment status unconfirmed"))
                .push(p1_regular(message.clone()));

            let mut actions = Row::new().spacing(10);
            match (checking, outcome) {
                (true, _) | (_, None) => {
                    content = content.push(p2_regular(
                        "Checking your payment history to find out whether it went through. \
                         Don't send it again until this finishes.",
                    ));
                }
                (false, Some(outcome)) => {
                    content = content.push(p2_regular(outcome.guidance(guard)));
                    if outcome.may_resend(guard) {
                        actions = actions.push(
                            // Keeps the original idempotency key — see
                            // `ResendAfterUnknownRequested`. Never `Reset`,
                            // which would mint a new one and could pay twice.
                            button::primary(None, "Send again")
                                .on_press(Message::SparkSend(
                                    SparkSendMessage::ResendAfterUnknownRequested,
                                ))
                                .width(Length::Fixed(160.0)),
                        );
                    }
                    actions = actions.push(
                        button::secondary(None, "Check again")
                            .on_press(Message::SparkSend(SparkSendMessage::ReconcileRequested))
                            .width(Length::Fixed(140.0)),
                    );
                }
            }

            // Offered in both arms, and last in the row either way. It is
            // read-only — a redirect to the transaction list, no resend and no
            // new idempotency key — so it does not undercut "wait while the
            // check runs". Building it inside the resolved arm alone left the
            // checking arm with an empty action row: the one screen where the
            // user most wants to go look for their payment offered nothing to
            // press.
            actions = actions.push(
                button::transparent_border(None, "View transactions")
                    .on_press(Message::SparkSend(SparkSendMessage::History))
                    .width(Length::Fixed(180.0)),
            );

            Container::new(
                content
                    .push(Space::new().height(Length::Fixed(8.0)))
                    .push(actions),
            )
            .padding(16)
            .style(theme::card::simple)
            .into()
        }

        SparkSendPhase::Sent(_) => {
            // Handled by the full-screen celebration in render()
            Container::new(Column::new()).into()
        }

        SparkSendPhase::Error(err) => Container::new(
            Column::new()
                .spacing(10)
                .push(h4_bold("Error"))
                .push(p1_regular(err.clone()))
                .push(Space::new().height(Length::Fixed(8.0)))
                .push(
                    button::primary(None, "Try again")
                        .on_press(Message::SparkSend(SparkSendMessage::Reset))
                        .width(Length::Fixed(140.0)),
                ),
        )
        .padding(16)
        .style(theme::card::simple)
        .into(),
    }
}

/// The cross-chain conversion fee for the preview: the fee in the destination
/// asset, with a sats (or BTC) approximation alongside — the fee is quoted in
/// USDT/USDC but the user spends bitcoin, so the sats figure is what compares
/// against "Total you send". Asset-only when no BTC/USD price is available.
fn conversion_fee_display(
    fee_amount: u128,
    decimals: u8,
    asset: &str,
    reference_btc_usd_price: Option<f64>,
    bitcoin_unit: BitcoinDisplayUnit,
) -> String {
    let asset_part = format!(
        "{} {}",
        cross_chain::format_asset_amount(fee_amount, decimals),
        asset,
    );
    // The sats hint needs a real BTC/USD price *and* a fee that fits the u64
    // conversion input; without either, show the asset amount alone rather than
    // a mispriced or clamped-and-understated estimate. (`sats == 0` alone can't
    // gate this — it also means a sub-1-sat fee, which is still worth showing.)
    let Some(price) = reference_btc_usd_price.filter(|p| *p > 0.0) else {
        return asset_part;
    };
    if fee_amount > u64::MAX as u128 {
        return asset_part;
    }
    // USDT/USDC are USD-pegged, so value the fee in sats the same way the wallet
    // values its Stable Balance holding.
    let sats = stable_token_as_sats(fee_amount as u64, u32::from(decimals), Some(price));
    let unit = if matches!(bitcoin_unit, BitcoinDisplayUnit::BTC) {
        "BTC"
    } else {
        "SATS"
    };
    format!(
        "{asset_part} (≈ {} {unit})",
        Amount::from_sat(sats).to_formatted_string_with_unit(bitcoin_unit),
    )
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

/// Like [`kv_row`] but with an emphasised (bold) value — for the headline
/// "total" line the eye should land on.
fn total_row<'a>(label: &'a str, value: String) -> Element<'a, Message> {
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
                .push(h4_bold(value)),
        )
        .into()
}

// ── Two-card "YOU SEND → THEY RECEIVE" selector + picker ─────────────────────

/// Logo for a send target: bitcoin rails get the BTC coin badged with the rail;
/// stablecoins get the plain coin (the destination chain isn't known until a
/// route is picked, so there's no network badge to show yet).
fn target_logo<'a>(target: SparkSendTarget, size: f32) -> Element<'a, Message> {
    match target {
        SparkSendTarget::Lightning => asset_network_logo("btc", "lightning", size),
        SparkSendTarget::OnChain => asset_network_logo("btc", "bitcoin", size),
        SparkSendTarget::Spark => asset_network_logo("btc", "spark", size),
        SparkSendTarget::Usdt => asset_logo("usdt")
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .into(),
        SparkSendTarget::Usdc => asset_logo("usdc")
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .into(),
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

// ── Payment details card: layout, actions and their stable IDs ─────────────

// Stable, purpose-specific focus targets. Targets are named by rail, never by
// list position, so a reordered or filtered picker cannot move focus (or a
// held key) onto a different target.
const DESTINATION_INPUT_ID: &str = "spark-send-destination";
const AMOUNT_INPUT_ID: &str = "spark-send-amount";
const RECEIVE_TARGET_ACTION_ID: &str = "spark-send-receive-target";
const PREPARE_ACTION_ID: &str = "spark-send-prepare";
const TARGET_PICKER_CLOSE_ID: &str = "spark-send-target-picker-close";

fn target_action_id(target: SparkSendTarget) -> &'static str {
    match target {
        SparkSendTarget::Lightning => "spark-send-target-lightning",
        SparkSendTarget::OnChain => "spark-send-target-on-chain",
        SparkSendTarget::Spark => "spark-send-target-spark",
        SparkSendTarget::Usdt => "spark-send-target-usdt",
        SparkSendTarget::Usdc => "spark-send-target-usdc",
    }
}

/// One full-width section of the Payment details card. The 24px inset also
/// leaves room for the four-pixel focus ring around an Action at its edge.
fn card_section<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    Container::new(content)
        .padding([20, 24])
        .width(Length::Fill)
        .into()
}

/// The Payment details surface: the shell panel with a hairline border.
fn payment_card_style(theme: &theme::Theme) -> container::Style {
    let shell = &theme.colors.shell;
    container::Style {
        background: Some(Background::Color(shell.panel)),
        text_color: Some(shell.ink),
        border: iced::Border {
            color: shell.line,
            width: 1.0,
            radius: 16.0.into(),
        },
        ..Default::default()
    }
}

/// A route tile. Hover only changes the outline; selection never applies here.
fn route_tile_style(theme: &theme::Theme, hovered: bool) -> (Background, iced::Border) {
    let shell = &theme.colors.shell;
    (
        Background::Color(shell.background),
        iced::Border {
            color: if hovered {
                shell.accent_ink
            } else {
                shell.line
            },
            width: 1.0,
            radius: 12.0.into(),
        },
    )
}

/// Status-independent fill for the fixed YOU SEND tile.
fn fixed_tile_style(theme: &theme::Theme) -> container::Style {
    let (background, border) = route_tile_style(theme, false);
    container::Style {
        background: Some(background),
        border,
        ..Default::default()
    }
}

/// The tappable THEY RECEIVE tile: the button draws the tile itself, so the
/// hover outline is not hidden under an inner card.
fn receive_tile_button_style(
    theme: &theme::Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    let hovered = matches!(
        status,
        iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed
    );
    let (background, border) = route_tile_style(theme, hovered);
    iced::widget::button::Style {
        background: Some(background),
        text_color: theme.colors.shell.ink,
        border,
        ..Default::default()
    }
}

/// Picker rows draw their surface in the inner container (selected or not),
/// so the button itself stays clear.
fn picker_action_style(
    _: &theme::Theme,
    _: iced::widget::button::Status,
) -> iced::widget::button::Style {
    iced::widget::button::Style {
        background: Some(Background::Color(color::TRANSPARENT)),
        border: iced::Border {
            radius: 12.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The card's footer while composing: one status line and Prepare. `None`
/// renders Prepare without a press — Tab skips it and no key or pointer can
/// activate it — rather than hiding it, so the footer keeps its shape.
fn compose_footer<'a>(status: &'static str, prepare: Option<Message>) -> Element<'a, Message> {
    Row::new()
        .spacing(16)
        .align_y(Alignment::Center)
        .push(
            p2_regular(status)
                .style(theme::text::secondary)
                .width(Length::Fill),
        )
        .push(
            Action::new(
                PREPARE_ACTION_ID,
                text("Prepare").size(P1_SIZE).bold(),
                Message::FocusAction,
            )
            .style(theme::button::primary)
            .padding([10, 24])
            .on_press_maybe(prepare),
        )
        .into()
}

/// The "YOU SEND (Bitcoin) → THEY RECEIVE (target)" pair. YOU SEND is fixed —
/// the Spark wallet always spends bitcoin — so only THEY RECEIVE is an Action,
/// and it opens the real rail picker.
fn spark_send_cards<'a>(target: SparkSendTarget, destination_empty: bool) -> Element<'a, Message> {
    let tile_caption =
        |label: &'static str| text(label).size(P2_SIZE).style(theme::text::secondary);
    let asset = |logo: Element<'a, Message>, label: &'static str| {
        Row::new()
            .spacing(10)
            .align_y(Alignment::Center)
            .push(logo)
            .push(text(label).size(H5_SIZE).bold().style(theme::text::primary))
    };

    let you_send = Container::new(
        Column::new()
            .spacing(8)
            .push(tile_caption("You send"))
            .push(asset(asset_network_logo("btc", "spark", 40.0), "Bitcoin"))
            .push(orange_badge("SPARK")),
    )
    .padding([14, 16])
    .width(Length::Fill)
    .height(Length::Fixed(ROUTE_TILE_HEIGHT))
    .style(fixed_tile_style);

    let they_receive = Action::new(
        RECEIVE_TARGET_ACTION_ID,
        Container::new(
            Column::new()
                .spacing(8)
                .push(
                    Row::new()
                        .push(tile_caption("They receive"))
                        .push(Space::new().width(Length::Fill))
                        .push(text("Change").size(P2_SIZE).style(|theme: &theme::Theme| {
                            iced::widget::text::Style {
                                color: Some(theme.colors.shell.accent_ink),
                            }
                        })),
                )
                .push(asset(target_logo(target, 40.0), target.label()))
                .push(
                    Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(orange_badge(target.badge()))
                        .push_maybe(destination_empty.then(|| tile_caption("Add a destination"))),
                ),
        )
        .padding([14, 16])
        .width(Length::Fill)
        .height(Length::Fixed(ROUTE_TILE_HEIGHT)),
        Message::FocusAction,
    )
    .style(receive_tile_button_style)
    .padding(0)
    .width(Length::Fill)
    .focus_radius(12.0)
    .on_press(Message::SparkSend(SparkSendMessage::OpenReceivePicker));

    Row::new()
        .spacing(16)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .push(Container::new(you_send).width(Length::FillPortion(1)))
        .push(text("→").size(H5_SIZE).style(theme::text::secondary))
        .push(Container::new(they_receive).width(Length::FillPortion(1)))
        .into()
}

const ROUTE_TILE_HEIGHT: f32 = 132.0;

/// One rail in the THEY RECEIVE picker: logo, asset, rail badge and — for the
/// current selection — a check on a tinted surface. Selection is styling only;
/// every offered rail stays pressable.
fn target_picker_row<'a>(target: SparkSendTarget, selected: bool) -> Element<'a, Message> {
    let row = Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .push(target_logo(target, 36.0))
        .push(
            text(target.label())
                .size(P1_SIZE)
                .bold()
                .style(theme::text::primary),
        )
        .push(orange_badge(target.badge()))
        .push(Space::new().width(Length::Fill))
        .push_maybe(selected.then(|| icon::check2_icon().size(18).color(color::ORANGE)));

    Action::new(
        target_action_id(target),
        Container::new(row)
            .padding([12, 16])
            .width(Length::Fill)
            .style(if selected {
                picker_row_selected
            } else {
                theme::card::simple
            }),
        Message::FocusAction,
    )
    .style(picker_action_style)
    .padding(0)
    .width(Length::Fill)
    .focus_radius(12.0)
    .on_press(Message::SparkSend(SparkSendMessage::SetReceiveTarget(
        target,
    )))
    .into()
}

/// The "THEY RECEIVE" picker modal: bitcoin rails, then (mainnet only) the
/// cross-chain stablecoins. Overlaid by the state's `view()`.
pub fn send_target_picker_modal<'a>(
    current: SparkSendTarget,
    network: Network,
) -> Element<'a, Message> {
    let row = |target: SparkSendTarget| target_picker_row(target, current == target);

    let mut list = Column::new()
        .spacing(8)
        .push(text("Bitcoin").size(P2_SIZE).style(theme::text::secondary));
    for target in [
        SparkSendTarget::Lightning,
        SparkSendTarget::OnChain,
        SparkSendTarget::Spark,
    ] {
        list = list.push(row(target));
    }

    // Cross-chain stablecoins are mainnet-only (providers/chains have no test
    // deployment), so they only appear there.
    if supported_on(network) {
        list = list.push(Space::new().height(Length::Fixed(6.0))).push(
            text("Stablecoin (cross-chain)")
                .size(P2_SIZE)
                .style(theme::text::secondary),
        );
        for target in [SparkSendTarget::Usdt, SparkSendTarget::Usdc] {
            list = list.push(row(target));
        }
    }

    // An explicit way out for keyboard users; a click outside the card still
    // closes it through the modal's blur message.
    list = list.push(Space::new().height(Length::Fixed(4.0))).push(
        Row::new().push(Space::new().width(Length::Fill)).push(
            Action::new(
                TARGET_PICKER_CLOSE_ID,
                text("Close").size(P2_SIZE).bold(),
                Message::FocusAction,
            )
            .style(theme::button::transparent_border)
            .padding([8, 18])
            .on_press(Message::SparkSend(SparkSendMessage::CloseReceivePicker)),
        ),
    );

    picker_modal_card("THEY RECEIVE", list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_fee_shows_a_sats_estimate_when_a_price_is_known() {
        // 0.097921 USDT at $65,000/BTC ≈ 150 sats.
        let s = conversion_fee_display(97_921, 6, "USDT", Some(65_000.0), BitcoinDisplayUnit::Sats);
        assert_eq!(s, "0.097921 USDT (≈ 150 SATS)");
    }

    #[test]
    fn conversion_fee_is_asset_only_without_a_price() {
        let s = conversion_fee_display(97_921, 6, "USDT", None, BitcoinDisplayUnit::Sats);
        assert_eq!(s, "0.097921 USDT");
    }

    #[test]
    fn conversion_fee_is_asset_only_with_non_positive_prices() {
        for price in [Some(0.0), Some(-1.0)] {
            let s = conversion_fee_display(97_921, 6, "USDT", price, BitcoinDisplayUnit::Sats);
            assert_eq!(s, "0.097921 USDT");
        }
    }

    #[test]
    fn conversion_fee_uses_the_selected_bitcoin_display_unit() {
        // 0.097921 USDT at $65,000/BTC = 150 sats, rendered in BTC mode.
        let s = conversion_fee_display(97_921, 6, "USDT", Some(65_000.0), BitcoinDisplayUnit::BTC);
        assert_eq!(s, "0.097921 USDT (≈ 0.00 000 150 BTC)");
    }

    #[test]
    fn conversion_fee_shows_the_hint_for_a_sub_one_sat_fee_when_priced() {
        // A tiny fee rounds to 0 sats, but with a price known the hint should
        // still show — the old `sats == 0` gate wrongly suppressed it.
        let s = conversion_fee_display(1, 6, "USDT", Some(65_000.0), BitcoinDisplayUnit::Sats);
        assert_eq!(s, "0.000001 USDT (≈ 0 SATS)");
    }

    #[test]
    fn conversion_fee_skips_the_hint_when_the_fee_overflows_u64() {
        // A fee that doesn't fit u64 must not be clamped-and-understated — drop
        // the sats hint rather than show a wrong figure.
        let s = conversion_fee_display(
            u128::MAX,
            6,
            "USDT",
            Some(65_000.0),
            BitcoinDisplayUnit::Sats,
        );
        assert!(
            !s.contains('≈'),
            "an overflowing fee must not show a clamped estimate: {}",
            s
        );
    }

    // ── R07b: the real compose presenter inside the real dashboard ──────────
    //
    // Synthetic facts only. Nothing here starts a wallet, the Spark bridge, an
    // SDK call, a session or an OS window: the production `SparkSendView` and
    // target picker are composed exactly as the state's `view()` composes them,
    // then laid out, operated and (optionally) drawn by the software renderer.

    use crate::app::cache::Cache;
    use crate::app::menu::{Menu, SparkSubMenu};
    use crate::app::state::spark::send::{ReconcileOutcome, RetryGuard};
    use coincube_spark_protocol::{CrossChainQuote, CrossChainRoute, PrepareSendOk};
    use coincube_ui::component::quote_display::Quote;
    use coincube_ui::theme::palette::ThemeMode;
    use iced::advanced::widget::operation::{self, focusable, Focusable, TextInput};
    use iced::advanced::widget::{Id, Operation, Tree};
    use iced::advanced::{clipboard, layout, overlay, renderer::Headless, Layout, Shell};
    use iced::{keyboard, mouse, Event, Rectangle, Size, Vector};

    const COMPACT: (u32, u32) = (1024, 768);
    const WIDE: (u32, u32) = (1440, 900);
    const FULL_HEIGHT: (u32, u32) = (1440, 2400);
    // Unified spendable balance includes the BTC component and synthetic USDB.
    const SYNTHETIC_BALANCE_SATS: u64 = 35_000;
    const SYNTHETIC_BITCOIN_SATS: u64 = 25_053;
    /// Shaped like the real kinds, valid as none of them. `15u` is 15 microbitcoin,
    /// or 1,500 sats, matching the ordinary prepared quote below.
    const SYNTHETIC_INVOICE: &str =
        "lnbc15u1synthetic0r07b0fixture0not0a0payable0invoice0long0enough0to0test0wrapping";
    const SYNTHETIC_CROSS_CHAIN_ADDRESS: &str =
        "0xsynthetic-r07b-usdc-cross-chain-address-not-payable";
    const SYNTHETIC_HINT: &str = "Your bitcoin balance is 25,053 sats; the rest is converted \
                                  from your synthetic USDB at the time of sending.";

    struct Fixture {
        backend_available: bool,
        destination: String,
        amount: String,
        invoice_amount: bool,
        phase: SparkSendPhase,
        mismatch: bool,
        mode: ThemeMode,
        unit: BitcoinDisplayUnit,
        hint: Option<String>,
        mask_amounts: bool,
        target: SparkSendTarget,
        network: Network,
        countdown: Option<cross_chain::QuoteCountdown>,
        quote: Quote,
        image: iced::widget::image::Handle,
    }

    impl Fixture {
        fn new(phase: SparkSendPhase) -> Self {
            Self {
                backend_available: true,
                destination: String::new(),
                amount: String::new(),
                invoice_amount: false,
                phase,
                mismatch: false,
                mode: ThemeMode::Dark,
                unit: BitcoinDisplayUnit::Sats,
                hint: None,
                mask_amounts: false,
                target: SparkSendTarget::Lightning,
                network: Network::Bitcoin,
                countdown: None,
                quote: Quote {
                    id: String::new(),
                    text: String::new(),
                    author: String::new(),
                    source: String::new(),
                    category: String::new(),
                    length: String::new(),
                    contexts: Vec::new(),
                },
                image: iced::widget::image::Handle::from_rgba(1, 1, vec![0u8; 4]),
            }
        }

        fn filled(phase: SparkSendPhase) -> Self {
            let mut fixture = Self::new(phase);
            fixture.destination = SYNTHETIC_INVOICE.to_string();
            fixture.amount = "30000".to_string();
            fixture.hint = Some(SYNTHETIC_HINT.to_string());
            fixture
        }

        fn invoice(phase: SparkSendPhase) -> Self {
            let mut fixture = Self::new(phase);
            fixture.destination = SYNTHETIC_INVOICE.to_string();
            fixture.amount = "1500".to_string();
            fixture.invoice_amount = true;
            fixture
        }

        fn ordinary_prepared() -> Self {
            Self::invoice(ordinary_prepared())
        }

        fn cross_chain_prepared() -> Self {
            let mut fixture = Self::filled(cross_chain_prepared());
            fixture.destination = SYNTHETIC_CROSS_CHAIN_ADDRESS.to_string();
            fixture.target = SparkSendTarget::Usdc;
            fixture
        }

        fn themed(mut self, mode: ThemeMode) -> Self {
            self.mode = mode;
            self
        }

        fn view(&self) -> SparkSendView<'_> {
            SparkSendView {
                backend_available: self.backend_available,
                destination_input: &self.destination,
                amount_input: &self.amount,
                amount_set_by_invoice: self.invoice_amount,
                phase: &self.phase,
                recipient_identities: &[],
                recipient_mismatch: self.mismatch,
                theme_mode: self.mode,
                sent_amount_display: "",
                sent_celebration_context: "",
                sent_quote: &self.quote,
                sent_image_handle: &self.image,
                recent_transactions: &[],
                balance_sats: SYNTHETIC_BALANCE_SATS,
                usdb_conversion_hint: self.hint.clone(),
                bitcoin_unit: self.unit,
                reference_btc_usd_price: None,
                show_direction_badges: false,
                mask_amounts: self.mask_amounts,
                receive_target: self.target,
                network: self.network,
                cross_chain_ctx: None,
                slippage_input: "",
                advanced_open: false,
                quote_countdown: self.countdown.clone(),
            }
        }

        fn cache(&self) -> Cache {
            Cache {
                network: self.network,
                theme_mode: self.mode,
                bitcoin_unit: self.unit,
                ..Default::default()
            }
        }

        fn theme(&self) -> theme::Theme {
            match self.mode {
                ThemeMode::Dark => theme::Theme::dark(),
                ThemeMode::Light => theme::Theme::light(),
            }
        }
    }

    /// The panel exactly as `SparkSend::view` builds it: the real dashboard
    /// around the real presenter, and the real picker as a blur-closing modal.
    fn screen<'a>(
        fixture: &'a Fixture,
        menu: &'a Menu,
        cache: &'a Cache,
        picker_open: bool,
    ) -> Element<'a, Message> {
        let content = crate::app::view::dashboard(menu, cache, fixture.view().render());
        if picker_open {
            return coincube_ui::widget::modal::Modal::new(
                content,
                send_target_picker_modal(fixture.target, cache.network),
            )
            .on_blur(Some(Message::SparkSend(
                SparkSendMessage::CloseReceivePicker,
            )))
            .into();
        }
        content
    }

    fn spark_send_menu() -> Menu {
        Menu::Spark(SparkSubMenu::Send)
    }

    fn headless_renderer() -> iced::Renderer {
        for bytes in coincube_ui::font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        // 14px is the app's `default_text_size` (main.rs).
        iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            coincube_ui::font::REGULAR,
            iced::Pixels(14.0),
            Some("tiny-skia"),
        ))
        .expect("software renderer for the synthetic R07b presenter")
    }

    #[derive(Default)]
    struct Probe {
        text: Vec<(String, Rectangle)>,
        focusable: Vec<(Option<Id>, Rectangle, bool)>,
        inputs: Vec<(Option<Id>, Rectangle, String)>,
    }

    impl Operation for Probe {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn text(&mut self, _id: Option<&Id>, bounds: Rectangle, value: &str) {
            self.text.push((value.to_owned(), bounds));
        }

        fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Focusable) {
            self.focusable
                .push((id.cloned(), bounds, state.is_focused()));
        }

        fn text_input(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn TextInput) {
            self.inputs
                .push((id.cloned(), bounds, state.text().to_owned()));
        }
    }

    impl Probe {
        fn contains(&self, needle: &str) -> bool {
            self.text.iter().any(|(value, _)| value.contains(needle))
        }

        fn action_ids(&self) -> Vec<Id> {
            self.focusable
                .iter()
                .filter_map(|(id, _, _)| id.clone())
                .collect()
        }

        fn bounds(&self, id: &'static str) -> Option<Rectangle> {
            let id = Id::new(id);
            self.focusable
                .iter()
                .find_map(|(actual, bounds, _)| (actual.as_ref() == Some(&id)).then_some(*bounds))
        }

        fn focused(&self) -> Vec<Id> {
            self.focusable
                .iter()
                .filter(|(_, _, focused)| *focused)
                .filter_map(|(id, _, _)| id.clone())
                .collect()
        }

        fn input_text(&self, id: &'static str) -> Option<String> {
            let id = Id::new(id);
            self.inputs
                .iter()
                .find_map(|(actual, _, text)| (actual.as_ref() == Some(&id)).then(|| text.clone()))
        }
    }

    /// Owns one laid-out screen and its retained widget tree, so a re-render
    /// diffs the same tree the way the runtime does between frames.
    struct Harness<'a> {
        element: Element<'a, Message>,
        tree: Tree,
        node: layout::Node,
        renderer: iced::Renderer,
        size: Size,
    }

    impl<'a> Harness<'a> {
        fn new(element: Element<'a, Message>, outer: (u32, u32)) -> Self {
            let renderer = headless_renderer();
            let size = Size::new(outer.0 as f32, outer.1 as f32);
            let mut element = element;
            let mut tree = Tree::new(element.as_widget());
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            Self {
                element,
                tree,
                node,
                renderer,
                size,
            }
        }

        fn rerender(&mut self, element: Element<'a, Message>) {
            self.element = element;
            self.tree.diff(self.element.as_widget());
            self.node = self.element.as_widget_mut().layout(
                &mut self.tree,
                &self.renderer,
                &layout::Limits::new(Size::ZERO, self.size),
            );
        }

        /// The runtime's whole-tree operation: the base first, then the modal
        /// overlay, which is where the open picker's rows live.
        fn operate(&mut self, operation: &mut dyn Operation) {
            self.element.as_widget_mut().operate(
                &mut self.tree,
                Layout::new(&self.node),
                &self.renderer,
                operation,
            );
            let viewport = Rectangle::with_size(self.size);
            if let Some(mut overlay) = self.element.as_widget_mut().overlay(
                &mut self.tree,
                Layout::new(&self.node),
                &self.renderer,
                &viewport,
                Vector::ZERO,
            ) {
                let node = overlay.as_overlay_mut().layout(&self.renderer, self.size);
                overlay
                    .as_overlay_mut()
                    .operate(Layout::new(&node), &self.renderer, operation);
            }
        }

        fn probe(&mut self) -> Probe {
            let mut probe = Probe::default();
            self.operate(&mut probe);
            probe
        }

        /// What `App` does with `Message::FocusAction(id)`: Iced's whole-tree
        /// focus, which also blurs every other focusable (inputs included).
        fn focus(&mut self, id: &'static str) {
            let mut current: Box<dyn Operation<()>> = Box::new(focusable::focus::<()>(Id::new(id)));
            loop {
                self.operate(&mut operation::black_box(current.as_mut()));
                match current.finish() {
                    operation::Outcome::Chain(next) => current = next,
                    _ => break,
                }
            }
        }

        fn send(&mut self, event: Event, cursor: mouse::Cursor) -> Vec<Message> {
            let mut messages = Vec::new();
            let viewport = Rectangle::with_size(self.size);
            let mut captured = false;
            if let Some(mut overlay) = self.element.as_widget_mut().overlay(
                &mut self.tree,
                Layout::new(&self.node),
                &self.renderer,
                &viewport,
                Vector::ZERO,
            ) {
                let node = overlay.as_overlay_mut().layout(&self.renderer, self.size);
                let mut shell = Shell::new(&mut messages);
                overlay.as_overlay_mut().update(
                    &event,
                    Layout::new(&node),
                    cursor,
                    &self.renderer,
                    &mut clipboard::Null,
                    &mut shell,
                );
                captured = shell.is_event_captured();
            }
            if !captured {
                self.element.as_widget_mut().update(
                    &mut self.tree,
                    &event,
                    Layout::new(&self.node),
                    cursor,
                    &self.renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &viewport,
                );
            }
            messages
        }

        fn press_at(&mut self, point: iced::Point) -> Vec<Message> {
            self.send(
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                mouse::Cursor::Available(point),
            )
        }

        fn release_at(&mut self, point: iced::Point) -> Vec<Message> {
            self.send(
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                mouse::Cursor::Available(point),
            )
        }

        fn click(&mut self, id: &'static str) -> Vec<Message> {
            let center = self
                .probe()
                .bounds(id)
                .unwrap_or_else(|| panic!("{} is not a focus target here", id))
                .center();
            let mut messages = self.press_at(center);
            messages.extend(self.release_at(center));
            messages
        }

        fn key(
            &mut self,
            named: keyboard::key::Named,
            pressed: bool,
            repeat: bool,
        ) -> Vec<Message> {
            let key = keyboard::Key::Named(named);
            let physical_key =
                keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified);
            let event = if pressed {
                keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key,
                    physical_key,
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                    text: None,
                    repeat,
                }
            } else {
                keyboard::Event::KeyReleased {
                    key: key.clone(),
                    modified_key: key,
                    physical_key,
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                }
            };
            self.send(Event::Keyboard(event), mouse::Cursor::Unavailable)
        }

        fn type_text(&mut self, value: &'static str) -> Vec<Message> {
            let key = keyboard::Key::Character(value.into());
            self.send(
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key,
                    physical_key: keyboard::key::Physical::Unidentified(
                        keyboard::key::NativeCode::Unidentified,
                    ),
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::empty(),
                    text: Some(value.into()),
                    repeat: false,
                }),
                mouse::Cursor::Unavailable,
            )
        }

        /// Nothing in the dashboard may run past the window's right edge.
        fn assert_no_horizontal_overflow(&mut self, scenario: &str) {
            let width = self.size.width;
            for (value, bounds) in &self.probe().text {
                assert!(
                    bounds.x >= -1.0 && bounds.x + bounds.width <= width + 1.0,
                    "{}: text {:?} at {:?} runs outside the {}px window",
                    scenario,
                    value,
                    bounds,
                    width
                );
            }
        }

        fn render_pixels(&mut self, scenario: &str, theme: &theme::Theme, redraw: bool) -> Vec<u8> {
            let viewport = Rectangle::with_size(self.size);
            if redraw {
                // The picker lives in Modal's separate overlay tree. Use the
                // same overlay-first event route as actual pointer/key input.
                let messages = self.send(
                    Event::Window(iced::window::Event::RedrawRequested(
                        std::time::Instant::now(),
                    )),
                    mouse::Cursor::Unavailable,
                );
                // The dashboard scroll widget may publish its current zero
                // offset on the first redraw. No other message is passive.
                assert!(
                    messages.is_empty()
                        || matches!(messages.as_slice(), [Message::Scroll(offset)] if *offset == 0.0),
                    "{}: initial redraw emitted an effectful or nonzero message {:?}",
                    scenario,
                    messages
                );
            }
            // A regression paints this same tree both before and after its
            // first redraw. Never composite a later frame over earlier pixels.
            iced::advanced::Renderer::reset(&mut self.renderer, viewport);
            let style = iced::advanced::renderer::Style {
                text_color: theme.colors.text.primary,
            };
            self.element.as_widget().draw(
                &self.tree,
                &mut self.renderer,
                theme,
                &style,
                Layout::new(&self.node),
                mouse::Cursor::Unavailable,
                &viewport,
            );
            if let Some(mut overlay) = self
                .element
                .as_widget_mut()
                .overlay(
                    &mut self.tree,
                    Layout::new(&self.node),
                    &self.renderer,
                    &viewport,
                    Vector::ZERO,
                )
                .map(overlay::Nested::new)
            {
                // Match UserInterface::draw: Nested supplies the renderer
                // layer that keeps the modal above the dashboard's batches.
                let node = overlay.layout(&self.renderer, self.size);
                overlay.draw(
                    &mut self.renderer,
                    theme,
                    &style,
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                );
            }
            let (width, height) = (self.size.width as u32, self.size.height as u32);
            self.renderer.screenshot(
                Size::new(width, height),
                1.0,
                theme.colors.general.background,
            )
        }

        /// Optional evidence: set `R07B_CAPTURE_DIR` (and `R07B_CAPTURE_HEAD`
        /// to the exact commit) to save the real rendered pixels. Without it
        /// the test only asserts. Headless pixels are not native-shell proof.
        fn capture(&mut self, scenario: &str, theme: &theme::Theme, appearance: &str) {
            let Ok(directory) = std::env::var("R07B_CAPTURE_DIR") else {
                return;
            };
            let head = std::env::var("R07B_CAPTURE_HEAD").unwrap_or_else(|_| "uncommitted".into());
            std::fs::create_dir_all(&directory).expect("create the disposable capture directory");
            let pixels = self.render_pixels(scenario, theme, true);
            let (width, height) = (self.size.width as u32, self.size.height as u32);
            let filename = format!(
                "r07b-{}-{}-{}x{}-{}-dashboard-synthetic.png",
                scenario, appearance, width, height, head
            );
            image::save_buffer(
                std::path::Path::new(&directory).join(filename),
                &pixels,
                width,
                height,
                image::ColorType::Rgba8,
            )
            .expect("save the synthetic R07b dashboard capture");
        }
    }

    /// A comparable reading of the messages a gesture produced.
    #[derive(Debug, Clone, PartialEq)]
    enum Seen {
        Focus(Id),
        Prepare,
        OpenPicker,
        ClosePicker,
        Target(SparkSendTarget),
        Destination(String),
        Amount(String),
        Other(String),
    }

    fn seen(messages: Vec<Message>) -> Vec<Seen> {
        messages
            .into_iter()
            .map(|message| match message {
                Message::FocusAction(id) => Seen::Focus(id),
                Message::SparkSend(SparkSendMessage::PrepareRequested) => Seen::Prepare,
                Message::SparkSend(SparkSendMessage::OpenReceivePicker) => Seen::OpenPicker,
                Message::SparkSend(SparkSendMessage::CloseReceivePicker) => Seen::ClosePicker,
                Message::SparkSend(SparkSendMessage::SetReceiveTarget(target)) => {
                    Seen::Target(target)
                }
                Message::SparkSend(SparkSendMessage::DestinationInputChanged(value)) => {
                    Seen::Destination(value)
                }
                Message::SparkSend(SparkSendMessage::AmountInputChanged(value)) => {
                    Seen::Amount(value)
                }
                other => Seen::Other(format!("{:?}", other)),
            })
            .collect()
    }

    fn themes() -> [ThemeMode; 2] {
        [ThemeMode::Dark, ThemeMode::Light]
    }

    fn appearance(mode: ThemeMode) -> &'static str {
        match mode {
            ThemeMode::Dark => "dark",
            ThemeMode::Light => "light",
        }
    }

    fn rendered_luma_range(pixels: &[u8], size: Size, bounds: Rectangle) -> u16 {
        let width = size.width as u32;
        let height = size.height as u32;
        let x0 = bounds.x.max(0.0).floor() as u32;
        let y0 = bounds.y.max(0.0).floor() as u32;
        let x1 = (bounds.x + bounds.width).ceil().min(size.width) as u32;
        let y1 = (bounds.y + bounds.height).ceil().min(size.height) as u32;
        assert!(
            x1 > x0 && y1 > y0 && x1 <= width && y1 <= height,
            "text bounds must be visible: {:?} in {:?}",
            bounds,
            size
        );
        let mut min = u16::MAX;
        let mut max = 0;
        for y in y0..y1 {
            for x in x0..x1 {
                let at = ((y * width + x) * 4) as usize;
                let luma = ((u32::from(pixels[at]) * 299
                    + u32::from(pixels[at + 1]) * 587
                    + u32::from(pixels[at + 2]) * 114)
                    / 1000) as u16;
                min = min.min(luma);
                max = max.max(luma);
            }
        }
        max - min
    }

    /// The YOU SEND card's former balance expression, kept verbatim so the
    /// relocated "Available" figure is checked against the original wording.
    fn original_balance_text(balance_sats: u64, bitcoin_unit: BitcoinDisplayUnit) -> String {
        format!(
            "{} {}",
            Amount::from_sat(balance_sats).to_formatted_string_with_unit(bitcoin_unit),
            if matches!(bitcoin_unit, BitcoinDisplayUnit::BTC) {
                "BTC"
            } else {
                "SATS"
            }
        )
    }

    #[test]
    fn masked_payment_card_hides_available_balance_and_conversion_hint() {
        let mut fixture = Fixture::filled(SparkSendPhase::Idle);
        fixture.mask_amounts = true;
        let menu = spark_send_menu();
        let cache = fixture.cache();
        let mut harness = Harness::new(screen(&fixture, &menu, &cache, false), COMPACT);
        let probe = harness.probe();
        assert!(probe.contains("••••••"));
        assert!(!probe.contains(&original_balance_text(
            SYNTHETIC_BALANCE_SATS,
            BitcoinDisplayUnit::Sats,
        )));
        assert!(!probe.contains(SYNTHETIC_HINT));
        assert!(probe.bounds(RECEIVE_TARGET_ACTION_ID).is_some());
    }

    #[test]
    fn compose_states_render_inside_the_dashboard_at_compact_wide_and_full_height() {
        let menu = spark_send_menu();
        for mode in themes() {
            let cases = [
                ("empty", Fixture::new(SparkSendPhase::Idle).themed(mode)),
                ("filled", Fixture::filled(SparkSendPhase::Idle).themed(mode)),
                (
                    "invoice",
                    Fixture::invoice(SparkSendPhase::Idle).themed(mode),
                ),
                (
                    "preparing",
                    Fixture::filled(SparkSendPhase::Preparing).themed(mode),
                ),
            ];
            for (scenario, fixture) in &cases {
                let cache = fixture.cache();
                let theme = fixture.theme();
                let mut sizes = vec![COMPACT, WIDE];
                if *scenario != "empty" {
                    sizes.push(FULL_HEIGHT);
                }
                for outer in sizes {
                    let mut harness = Harness::new(screen(fixture, &menu, &cache, false), outer);
                    let label = format!("{} {} {:?}", scenario, appearance(mode), outer);
                    harness.assert_no_horizontal_overflow(&label);
                    let probe = harness.probe();
                    for expected in [
                        "Payment details",
                        "You send",
                        "They receive",
                        "Destination",
                        "Amount (sats)",
                        "Available",
                    ] {
                        assert!(
                            probe.contains(expected),
                            "{}: missing {:?}",
                            label,
                            expected
                        );
                    }
                    assert!(
                        probe.contains(&original_balance_text(
                            SYNTHETIC_BALANCE_SATS,
                            BitcoinDisplayUnit::Sats
                        )),
                        "{}: the available balance must keep the original formatting",
                        label
                    );
                    assert_eq!(
                        probe.contains("Add a destination"),
                        fixture.destination.is_empty(),
                        "{}: the destination prompt follows the actual field",
                        label
                    );
                    assert!(
                        probe.input_text(DESTINATION_INPUT_ID).is_some(),
                        "{}: native destination input",
                        label
                    );
                    assert!(
                        probe.input_text(AMOUNT_INPUT_ID).is_some(),
                        "{}: native amount input",
                        label
                    );
                    let preparing = matches!(fixture.phase, SparkSendPhase::Preparing);
                    assert_eq!(
                        probe.bounds(PREPARE_ACTION_ID).is_some(),
                        !preparing,
                        "{}: Prepare is a focus target exactly when it is pressable",
                        label
                    );
                    assert_eq!(
                        probe.contains("Preparing send… asking the Spark bridge for a fee quote."),
                        preparing,
                        "{}: the existing preparation message",
                        label
                    );
                    assert!(
                        probe.contains("Prepare"),
                        "{}: the Prepare control is always drawn while composing",
                        label
                    );
                    for absent in ["Confirm and send", "Send again", "Try again", "Get quote"] {
                        assert!(
                            !probe.contains(absent),
                            "{}: compose must not offer {:?}",
                            label,
                            absent
                        );
                    }
                    assert_eq!(
                        probe.contains("This invoice sets its own amount."),
                        fixture.invoice_amount,
                        "{}: invoice note",
                        label
                    );
                    assert_eq!(
                        probe.contains("the rest is converted"),
                        fixture.hint.is_some(),
                        "{}: conversion hint",
                        label
                    );
                    harness.capture(scenario, &theme, appearance(mode));
                }
            }
        }
    }

    #[test]
    fn owned_actions_send_one_exact_intent_by_pointer_enter_and_space() {
        let menu = spark_send_menu();
        let fixture = Fixture::filled(SparkSendPhase::Idle);
        let cache = fixture.cache();
        for (id, intent) in [
            (PREPARE_ACTION_ID, Seen::Prepare),
            (RECEIVE_TARGET_ACTION_ID, Seen::OpenPicker),
        ] {
            // A pointer needs the control on screen without scrolling; the
            // keyboard path is checked at the compact minimum as well.
            for outer in [WIDE, FULL_HEIGHT] {
                let mut harness = Harness::new(screen(&fixture, &menu, &cache, false), outer);
                assert_eq!(
                    seen(harness.click(id)),
                    vec![Seen::Focus(Id::new(id)), intent.clone()],
                    "{} pointer at {:?}",
                    id,
                    outer
                );
            }
            for outer in [COMPACT, WIDE] {
                let mut harness = Harness::new(screen(&fixture, &menu, &cache, false), outer);
                harness.focus(id);
                assert_eq!(harness.probe().focused(), vec![Id::new(id)]);
                for named in [keyboard::key::Named::Enter, keyboard::key::Named::Space] {
                    assert_eq!(
                        seen(harness.key(named, true, false)),
                        vec![intent.clone()],
                        "{} {:?} press",
                        id,
                        named
                    );
                    assert!(
                        harness.key(named, true, true).is_empty(),
                        "{} {:?} auto-repeat must not repeat the intent",
                        id,
                        named
                    );
                    assert!(
                        harness.key(named, false, false).is_empty(),
                        "{} {:?} release",
                        id,
                        named
                    );
                }
            }
        }
    }

    fn with(mut fixture: Fixture, edit: impl FnOnce(&mut Fixture)) -> Fixture {
        edit(&mut fixture);
        fixture
    }

    #[test]
    fn prepare_held_across_preparing_never_fires_on_release() {
        let menu = spark_send_menu();
        let idle = Fixture::filled(SparkSendPhase::Idle);
        let preparing = Fixture::filled(SparkSendPhase::Preparing);
        let idle_cache = idle.cache();
        let preparing_cache = preparing.cache();

        // Keyboard: Enter fires once on press; while held, the phase moves to
        // Preparing (no press, no focus target) and back to Idle.
        let mut harness = Harness::new(screen(&idle, &menu, &idle_cache, false), WIDE);
        harness.focus(PREPARE_ACTION_ID);
        assert_eq!(
            seen(harness.key(keyboard::key::Named::Enter, true, false)),
            vec![Seen::Prepare]
        );
        harness.rerender(screen(&preparing, &menu, &preparing_cache, false));
        let probe = harness.probe();
        assert!(probe.bounds(PREPARE_ACTION_ID).is_none());
        assert!(
            probe.focused().is_empty(),
            "nothing keeps focus on a loading Prepare"
        );
        assert!(harness
            .key(keyboard::key::Named::Enter, true, false)
            .is_empty());
        harness.rerender(screen(&idle, &menu, &idle_cache, false));
        assert!(harness
            .key(keyboard::key::Named::Enter, false, false)
            .is_empty());
        assert!(harness.probe().focused().is_empty());

        // Pointer: a press that began on Idle and is released after a
        // Preparing pass must not complete as a click.
        let mut harness = Harness::new(screen(&idle, &menu, &idle_cache, false), WIDE);
        let center = harness.probe().bounds(PREPARE_ACTION_ID).unwrap().center();
        assert_eq!(
            seen(harness.press_at(center)),
            vec![Seen::Focus(Id::new(PREPARE_ACTION_ID))]
        );
        harness.rerender(screen(&preparing, &menu, &preparing_cache, false));
        harness.rerender(screen(&idle, &menu, &idle_cache, false));
        assert!(harness.release_at(center).is_empty());
    }

    #[test]
    fn native_inputs_edit_through_their_callbacks_and_keep_state_across_slots() {
        let menu = spark_send_menu();
        let empty = Fixture::new(SparkSendPhase::Idle);
        let cache = empty.cache();
        // The replacement view must outlive the harness that borrows it.
        let mut changed = Fixture::invoice(SparkSendPhase::Idle);
        changed.target = SparkSendTarget::Spark;
        let changed_cache = changed.cache();
        let mut harness = Harness::new(screen(&empty, &menu, &cache, false), COMPACT);

        // A pointer press on each field reaches the field itself: no parent
        // hit target covers it.
        let probe = harness.probe();
        for id in [DESTINATION_INPUT_ID, AMOUNT_INPUT_ID] {
            let center = probe
                .inputs
                .iter()
                .find_map(|(actual, bounds, _)| {
                    (actual.as_ref() == Some(&Id::new(id))).then_some(bounds.center())
                })
                .unwrap();
            assert!(harness.press_at(center).is_empty());
            harness.release_at(center);
            assert_eq!(harness.probe().focused(), vec![Id::new(id)], "{}", id);
        }

        harness.focus(DESTINATION_INPUT_ID);
        assert_eq!(
            seen(harness.type_text("l")),
            vec![Seen::Destination("l".to_string())]
        );
        harness.focus(AMOUNT_INPUT_ID);
        assert_eq!(
            seen(harness.type_text("5")),
            vec![Seen::Amount("5".to_string())]
        );

        // Pointer focus on an Action blurs the field through the same
        // whole-tree focus App applies for `FocusAction`.
        harness.focus(DESTINATION_INPUT_ID);
        let messages = seen(harness.click(RECEIVE_TARGET_ACTION_ID));
        assert_eq!(
            messages,
            vec![
                Seen::Focus(Id::new(RECEIVE_TARGET_ACTION_ID)),
                Seen::OpenPicker
            ]
        );
        harness.focus(RECEIVE_TARGET_ACTION_ID);
        assert_eq!(
            harness.probe().focused(),
            vec![Id::new(RECEIVE_TARGET_ACTION_ID)]
        );

        // Notes, hints and a new target appear without moving the inputs'
        // slots, so a focused field stays focused and keeps its value.
        harness.focus(DESTINATION_INPUT_ID);
        harness.rerender(screen(&changed, &menu, &changed_cache, false));
        let probe = harness.probe();
        assert_eq!(probe.focused(), vec![Id::new(DESTINATION_INPUT_ID)]);
        assert_eq!(
            probe.input_text(DESTINATION_INPUT_ID).as_deref(),
            Some(SYNTHETIC_INVOICE)
        );
        assert_eq!(probe.input_text(AMOUNT_INPUT_ID).as_deref(), Some("1500"));
        drop(harness);

        // An invoice-set amount shows and selects but does not edit.
        let invoice = Fixture::invoice(SparkSendPhase::Idle);
        let invoice_cache = invoice.cache();
        let mut harness = Harness::new(screen(&invoice, &menu, &invoice_cache, false), WIDE);
        harness.focus(AMOUNT_INPUT_ID);
        assert!(harness.type_text("9").is_empty());
        let probe = harness.probe();
        assert_eq!(probe.input_text(AMOUNT_INPUT_ID).as_deref(), Some("1500"));
        assert!(probe.contains("This invoice sets its own amount."));
    }

    #[test]
    fn amount_label_and_placeholder_follow_the_display_unit_and_target() {
        let menu = spark_send_menu();
        for (unit, target, label, placeholder) in [
            (
                BitcoinDisplayUnit::Sats,
                SparkSendTarget::Lightning,
                "Amount (sats)",
                "Amount in sats (optional for invoices with amount)",
            ),
            (
                BitcoinDisplayUnit::BTC,
                SparkSendTarget::OnChain,
                "Amount (BTC)",
                "Amount in BTC (optional for invoices with amount)",
            ),
            (
                BitcoinDisplayUnit::Sats,
                SparkSendTarget::Usdt,
                "Amount (sats)",
                "Amount in sats — funded from your Bitcoin balance",
            ),
            (
                BitcoinDisplayUnit::BTC,
                SparkSendTarget::Usdc,
                "Amount (BTC)",
                "Amount in BTC — funded from your Bitcoin balance",
            ),
        ] {
            let mut fixture = Fixture::new(SparkSendPhase::Idle);
            fixture.unit = unit;
            fixture.target = target;
            let cache = fixture.cache();
            let mut harness = Harness::new(screen(&fixture, &menu, &cache, false), COMPACT);
            let probe = harness.probe();
            assert!(probe.contains(label), "{:?} {:?}", unit, target);
            assert_eq!(
                probe.input_text(AMOUNT_INPUT_ID).as_deref(),
                Some(placeholder),
                "an empty field shows its placeholder"
            );
            assert_eq!(
                probe.input_text(DESTINATION_INPUT_ID).as_deref(),
                Some(target.destination_placeholder())
            );
            assert!(probe.contains(&original_balance_text(SYNTHETIC_BALANCE_SATS, unit)));
        }
    }

    #[test]
    fn picker_offers_the_real_rails_per_network_with_semantic_targets() {
        let menu = spark_send_menu();
        for (network, offered) in [
            (Network::Bitcoin, SparkSendTarget::all().to_vec()),
            (
                Network::Testnet,
                vec![
                    SparkSendTarget::Lightning,
                    SparkSendTarget::OnChain,
                    SparkSendTarget::Spark,
                ],
            ),
            (
                Network::Signet,
                vec![
                    SparkSendTarget::Lightning,
                    SparkSendTarget::OnChain,
                    SparkSendTarget::Spark,
                ],
            ),
            (
                Network::Regtest,
                vec![
                    SparkSendTarget::Lightning,
                    SparkSendTarget::OnChain,
                    SparkSendTarget::Spark,
                ],
            ),
        ] {
            let mut fixture = Fixture::new(SparkSendPhase::Idle);
            fixture.network = network;
            let cache = fixture.cache();
            let mut harness = Harness::new(screen(&fixture, &menu, &cache, true), COMPACT);
            let probe = harness.probe();
            let picker: Vec<Id> = probe
                .action_ids()
                .into_iter()
                .filter(|id| {
                    SparkSendTarget::all()
                        .iter()
                        .any(|target| *id == Id::new(target_action_id(*target)))
                        || *id == Id::new(TARGET_PICKER_CLOSE_ID)
                })
                .collect();
            let mut expected: Vec<Id> = offered
                .iter()
                .map(|target| Id::new(target_action_id(*target)))
                .collect();
            expected.push(Id::new(TARGET_PICKER_CLOSE_ID));
            assert_eq!(picker, expected, "{:?}: offered rails, in order", network);

            // Adjacent rows leave the four-pixel ring outset clear.
            let rows: Vec<Rectangle> = offered
                .iter()
                .map(|target| probe.bounds(target_action_id(*target)).unwrap())
                .collect();
            for pair in rows.windows(2) {
                assert!(
                    pair[1].y - (pair[0].y + pair[0].height) >= 4.0,
                    "{:?}: rows {:?} and {:?} would paint over a focus ring",
                    network,
                    pair[0],
                    pair[1]
                );
            }

            for target in &offered {
                let id = target_action_id(*target);
                let mut harness = Harness::new(screen(&fixture, &menu, &cache, true), COMPACT);
                assert_eq!(
                    seen(harness.click(id)),
                    vec![Seen::Focus(Id::new(id)), Seen::Target(*target)],
                    "{:?} pointer on {}",
                    network,
                    id
                );
                harness.focus(id);
                assert_eq!(
                    seen(harness.key(keyboard::key::Named::Enter, true, false)),
                    vec![Seen::Target(*target)],
                    "{:?} Enter on {}",
                    network,
                    id
                );
                harness.key(keyboard::key::Named::Enter, false, false);
            }

            let mut harness = Harness::new(screen(&fixture, &menu, &cache, true), COMPACT);
            harness.focus(TARGET_PICKER_CLOSE_ID);
            assert_eq!(
                seen(harness.key(keyboard::key::Named::Space, true, false)),
                vec![Seen::ClosePicker]
            );
        }
    }

    #[test]
    fn a_changed_or_filtered_picker_cannot_activate_another_rail() {
        let menu = spark_send_menu();
        let mainnet = Fixture::new(SparkSendPhase::Idle);
        let mainnet_cache = mainnet.cache();

        // The focused rail disappears (network without cross-chain): the held
        // focus does not slide onto whichever row now sits in its place.
        let mut regtest = Fixture::new(SparkSendPhase::Idle);
        regtest.network = Network::Regtest;
        let regtest_cache = regtest.cache();
        let usdt = target_action_id(SparkSendTarget::Usdt);
        let mut harness = Harness::new(screen(&mainnet, &menu, &mainnet_cache, true), WIDE);
        harness.focus(usdt);
        harness.rerender(screen(&regtest, &menu, &regtest_cache, true));
        assert!(harness.probe().focused().is_empty());
        assert!(harness
            .key(keyboard::key::Named::Enter, true, false)
            .is_empty());

        // The selection moves to the focused rail: focus stays on that rail
        // and Enter still names exactly it.
        let spark = target_action_id(SparkSendTarget::Spark);
        let mut selected = Fixture::new(SparkSendPhase::Idle);
        selected.target = SparkSendTarget::Spark;
        let selected_cache = selected.cache();
        let mut harness = Harness::new(screen(&mainnet, &menu, &mainnet_cache, true), WIDE);
        harness.focus(spark);
        harness.rerender(screen(&selected, &menu, &selected_cache, true));
        assert_eq!(harness.probe().focused(), vec![Id::new(spark)]);
        assert_eq!(
            seen(harness.key(keyboard::key::Named::Enter, true, false)),
            vec![Seen::Target(SparkSendTarget::Spark)]
        );
    }

    #[test]
    fn picker_captures_show_selected_and_focused_rails_in_both_themes() {
        let menu = spark_send_menu();
        for mode in themes() {
            for (scenario, network, target, focused) in [
                (
                    "picker-mainnet",
                    Network::Bitcoin,
                    SparkSendTarget::Lightning,
                    SparkSendTarget::Usdc,
                ),
                (
                    "picker-testnet",
                    Network::Testnet,
                    SparkSendTarget::Spark,
                    SparkSendTarget::OnChain,
                ),
            ] {
                let mut fixture = Fixture::new(SparkSendPhase::Idle).themed(mode);
                fixture.network = network;
                fixture.target = target;
                let cache = fixture.cache();
                let theme = fixture.theme();
                let mut harness = Harness::new(screen(&fixture, &menu, &cache, true), WIDE);
                harness.focus(target_action_id(focused));
                let probe = harness.probe();
                assert_eq!(probe.focused(), vec![Id::new(target_action_id(focused))]);
                assert!(probe.contains("THEY RECEIVE"));
                assert!(probe.contains("Close"));
                harness.assert_no_horizontal_overflow(scenario);
                let close_bounds = probe
                    .text
                    .iter()
                    .rev()
                    .find(|(value, _)| value == "Close")
                    .map(|(_, bounds)| *bounds)
                    .expect("picker Close text bounds");
                let before = harness.render_pixels(scenario, &theme, false);
                let after = harness.render_pixels(scenario, &theme, true);
                let initial_contrast = rendered_luma_range(&before, harness.size, close_bounds);
                let close_contrast = rendered_luma_range(&after, harness.size, close_bounds);
                assert!(
                    close_contrast >= 80 && close_contrast >= initial_contrast,
                    "{} {}: redraw must leave Close readable above its card (initial luma range {}, redrawn {})",
                    scenario,
                    appearance(mode),
                    initial_contrast,
                    close_contrast
                );
                assert_eq!(
                    seen(harness.click(TARGET_PICKER_CLOSE_ID)),
                    vec![
                        Seen::Focus(Id::new(TARGET_PICKER_CLOSE_ID)),
                        Seen::ClosePicker
                    ],
                    "{} {}: visible Close must remain actionable",
                    scenario,
                    appearance(mode)
                );
                harness.capture(scenario, &theme, appearance(mode));
            }
        }
    }

    fn ordinary_prepared() -> SparkSendPhase {
        SparkSendPhase::Prepared(PrepareSendOk {
            handle: "synthetic-handle".to_string(),
            amount_sat: 1_500,
            fee_sat: 3,
            method: "Bolt11Invoice".to_string(),
            cross_chain: None,
            has_token_leg: Some(false),
        })
    }

    fn cross_chain_prepared() -> SparkSendPhase {
        SparkSendPhase::Prepared(PrepareSendOk {
            handle: "synthetic-handle".to_string(),
            amount_sat: 30_000,
            fee_sat: 0,
            method: "CrossChainAddress".to_string(),
            cross_chain: Some(Box::new(CrossChainQuote {
                route: CrossChainRoute {
                    provider: "synthetic-provider".to_string(),
                    chain: "synthetic-chain".to_string(),
                    chain_id: None,
                    asset: "USDC".to_string(),
                    contract_address: None,
                    decimals: 6,
                    btc_source_supported: true,
                },
                estimated_out: 19_000_000,
                fee_amount: 97_921,
                source_transfer_fee_sats: 120,
                expires_at: "0".to_string(),
                retry_safe: true,
            })),
            has_token_leg: Some(true),
        })
    }

    #[test]
    fn synthetic_prepared_facts_match_the_quoted_scenarios() {
        let ordinary = Fixture::ordinary_prepared();
        let SparkSendPhase::Prepared(ordinary_quote) = &ordinary.phase else {
            panic!("ordinary fixture must be prepared");
        };
        assert_eq!(
            ordinary.amount.parse::<u64>(),
            Ok(ordinary_quote.amount_sat)
        );
        assert_eq!(ordinary_quote.method, "Bolt11Invoice");
        assert_eq!(ordinary.target, SparkSendTarget::Lightning);
        assert_eq!(ordinary.destination, SYNTHETIC_INVOICE);
        assert!(ordinary.destination.starts_with("lnbc15u1"));
        assert_eq!(ordinary_quote.amount_sat, 15 * 100); // 1 μBTC = 100 sats.
        assert!(ordinary.invoice_amount);
        assert!(ordinary.hint.is_none());
        assert!(ordinary_quote.amount_sat + ordinary_quote.fee_sat < SYNTHETIC_BITCOIN_SATS);

        let mismatch = with(Fixture::ordinary_prepared(), |f| f.mismatch = true);
        assert_eq!(mismatch.amount, ordinary.amount);
        assert_eq!(mismatch.destination, ordinary.destination);
        assert_eq!(mismatch.target, ordinary.target);
        assert!(mismatch.hint.is_none());

        let cross_chain = Fixture::cross_chain_prepared();
        let SparkSendPhase::Prepared(cross_chain_quote) = &cross_chain.phase else {
            panic!("cross-chain fixture must be prepared");
        };
        assert_eq!(
            cross_chain.amount.parse::<u64>(),
            Ok(cross_chain_quote.amount_sat)
        );
        assert_eq!(cross_chain_quote.method, "CrossChainAddress");
        assert_eq!(cross_chain.target, SparkSendTarget::Usdc);
        assert_eq!(cross_chain.destination, SYNTHETIC_CROSS_CHAIN_ADDRESS);
        assert_eq!(
            cross_chain_quote.cross_chain.as_ref().unwrap().route.asset,
            "USDC"
        );
        assert!(cross_chain.hint.as_deref().unwrap().contains("25,053 sats"));
        assert!(SYNTHETIC_BITCOIN_SATS < cross_chain_quote.amount_sat);
        assert!(cross_chain_quote.amount_sat < SYNTHETIC_BALANCE_SATS);
    }

    fn outcome_unknown(
        checking: bool,
        outcome: Option<ReconcileOutcome>,
        guard: RetryGuard,
    ) -> SparkSendPhase {
        SparkSendPhase::OutcomeUnknown {
            message: "Synthetic: the bridge did not answer.".to_string(),
            outcome,
            checking,
            guard,
        }
    }

    #[test]
    fn protected_phase_bodies_still_render_below_the_card_unchanged() {
        let menu = spark_send_menu();
        struct Case {
            scenario: &'static str,
            fixture: Fixture,
            present: &'static [&'static str],
            absent: &'static [&'static str],
        }
        let cases = [
            Case {
                scenario: "prepared",
                fixture: Fixture::ordinary_prepared(),
                present: &["Preview", "Bolt11Invoice", "Confirm and send", "Cancel"],
                absent: &["Get a fresh quote"],
            },
            Case {
                scenario: "mismatch",
                fixture: with(Fixture::ordinary_prepared(), |f| f.mismatch = true),
                present: &[
                    crate::services::branta::MISMATCH_MESSAGE,
                    "Replace payment request",
                ],
                absent: &["Confirm and send", "Preview"],
            },
            Case {
                scenario: "quote-valid",
                fixture: with(Fixture::cross_chain_prepared(), |f| {
                    f.countdown = Some(cross_chain::QuoteCountdown::Valid { seconds_left: 42 })
                }),
                present: &["Quote valid for 42s.", "Confirm and send", "Total you send"],
                absent: &["Get a fresh quote"],
            },
            Case {
                scenario: "quote-expired",
                fixture: with(Fixture::cross_chain_prepared(), |f| {
                    f.countdown = Some(cross_chain::QuoteCountdown::Expired)
                }),
                present: &["This quote has expired.", "Get a fresh quote"],
                absent: &["Confirm and send"],
            },
            Case {
                scenario: "quote-unknown",
                fixture: with(Fixture::cross_chain_prepared(), |f| {
                    f.countdown = Some(cross_chain::QuoteCountdown::Unknown)
                }),
                present: &["Get a fresh quote"],
                absent: &["Confirm and send"],
            },
            Case {
                scenario: "outcome-unknown-checking",
                fixture: Fixture::filled(outcome_unknown(true, None, RetryGuard::IdempotencyKey)),
                present: &[
                    "Payment status unconfirmed",
                    "Don't send it again until this finishes.",
                    "View transactions",
                ],
                absent: &["Send again", "Check again", "Try again"],
            },
            Case {
                scenario: "outcome-unknown-inconclusive",
                fixture: Fixture::filled(outcome_unknown(
                    false,
                    Some(ReconcileOutcome::Inconclusive("synthetic".to_string())),
                    RetryGuard::IdempotencyKey,
                )),
                present: &["Check again", "View transactions"],
                absent: &["Send again", "Try again"],
            },
            Case {
                scenario: "outcome-unknown-no-trace-guarded",
                fixture: Fixture::filled(outcome_unknown(
                    false,
                    Some(ReconcileOutcome::NoTrace),
                    RetryGuard::IdempotencyKey,
                )),
                present: &["Send again", "Check again", "View transactions"],
                absent: &["Try again"],
            },
            Case {
                scenario: "outcome-unknown-no-trace-unguarded",
                fixture: Fixture::filled(outcome_unknown(
                    false,
                    Some(ReconcileOutcome::NoTrace),
                    RetryGuard::None,
                )),
                present: &["Check again", "View transactions"],
                absent: &["Send again", "Try again"],
            },
        ];
        for case in &cases {
            let cache = case.fixture.cache();
            for outer in [COMPACT, WIDE] {
                let mut harness = Harness::new(screen(&case.fixture, &menu, &cache, false), outer);
                let label = format!("{} {:?}", case.scenario, outer);
                let probe = harness.probe();
                for text in case.present {
                    assert!(probe.contains(text), "{}: missing {:?}", label, text);
                }
                for text in case.absent {
                    assert!(!probe.contains(text), "{}: unexpected {:?}", label, text);
                }
                // The compose footer belongs to Idle/Preparing only: no
                // Prepare control and no Prepare status line here.
                assert!(probe.bounds(PREPARE_ACTION_ID).is_none(), "{}", label);
                assert!(
                    !probe.contains("Prepare gets a fee quote."),
                    "{}: compose footer leaked into a later phase",
                    label
                );
                assert!(probe.contains("Payment details"), "{}", label);
                harness.assert_no_horizontal_overflow(&label);
            }
        }

        // The mismatch replaces the compose footer too, whatever the phase.
        let mismatch_idle = with(Fixture::filled(SparkSendPhase::Idle), |f| f.mismatch = true);
        let cache = mismatch_idle.cache();
        let mut harness = Harness::new(screen(&mismatch_idle, &menu, &cache, false), WIDE);
        let probe = harness.probe();
        assert!(probe.contains("Replace payment request"));
        assert!(probe.bounds(PREPARE_ACTION_ID).is_none());

        // Backend unavailable keeps its early return: no card at all.
        let unavailable = with(Fixture::new(SparkSendPhase::Idle), |f| {
            f.backend_available = false
        });
        let cache = unavailable.cache();
        let mut harness = Harness::new(screen(&unavailable, &menu, &cache, false), WIDE);
        let probe = harness.probe();
        assert!(probe.contains("Spark is not available for this cube."));
        assert!(!probe.contains("Payment details"));
        assert!(probe.bounds(PREPARE_ACTION_ID).is_none());
    }

    #[test]
    fn later_phase_captures_in_both_themes() {
        let menu = spark_send_menu();
        for mode in themes() {
            let mut mismatch = Fixture::ordinary_prepared().themed(mode);
            mismatch.mismatch = true;
            for (scenario, fixture) in [
                ("prepared", Fixture::ordinary_prepared().themed(mode)),
                ("mismatch", mismatch),
                (
                    "outcome-unknown-checking",
                    Fixture::filled(outcome_unknown(true, None, RetryGuard::IdempotencyKey))
                        .themed(mode),
                ),
            ] {
                let cache = fixture.cache();
                let theme = fixture.theme();
                let mut harness = Harness::new(screen(&fixture, &menu, &cache, false), WIDE);
                harness.assert_no_horizontal_overflow(scenario);
                harness.capture(scenario, &theme, appearance(mode));
            }
        }
    }
}
