//! Real Spark Receive panel — Phase 4c.
//!
//! State machine (per picked method):
//!
//! ```text
//! Idle { method } ──(Generate)──▶ Generating { method }
//!                                       │
//!                                  ┌────┴─────┐
//!                                  ▼          ▼
//!                          Generated(ok)   Error(msg)
//!                                  │          │
//!                                  └── Reset ─┘
//!                                  ▼
//!                               Idle { method }
//! ```
//!
//! The user picks a method (BOLT11 Lightning / on-chain Bitcoin),
//! optionally fills in amount + description for BOLT11, clicks
//! Generate, sees the result as a copyable text string. QR codes,
//! Lightning Address display, and the on-chain claim lifecycle all
//! land in Phase 4d.

use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::sync::Arc;

use coincube_spark_protocol::{DepositInfo, ReceivePaymentOk};
use coincube_ui::widget::Element;
use iced::{widget::qr_code, Subscription, Task};

use crate::app::cache::Cache;
use crate::app::menu::{Menu, SparkSubMenu};
use crate::app::message::Message;
use crate::app::state::{redirect, State};
use crate::app::view::spark::SparkReceiveView;
use crate::app::view::spark::SparkRecentTransaction;
use crate::app::view::FiatAmountConverter;
use crate::app::wallets::SparkBackend;

use super::esplora::DEPOSIT_MATURITY_CONFIRMATIONS;
use super::sideshift_receive::SparkSideshiftReceiveFlow;

/// Which receive flow the user has picked.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SparkReceiveMethod {
    Bolt11,
    LightningAddress,
    OnchainBitcoin,
    Spark,
}

impl SparkReceiveMethod {
    pub fn label(self) -> &'static str {
        match self {
            Self::Bolt11 => "Lightning (BOLT11)",
            Self::LightningAddress => "Lightning address",
            Self::OnchainBitcoin => "On-chain Bitcoin",
            Self::Spark => "Spark",
        }
    }
}

#[derive(Debug, Clone)]
pub enum SparkReceivePhase {
    /// User is picking/configuring a method.
    Idle,
    /// `receive_bolt11` or `receive_onchain` RPC is in flight.
    Generating,
    /// RPC succeeded — `payment_request` is the copyable result.
    Generated(ReceivePaymentOk),
    /// A `PaymentSucceeded` event arrived while the panel was in
    /// `Generated` state. Carries the running sum of all qualifying
    /// `PaymentReceived` events that have arrived since this phase
    /// was entered, plus a `count` so the celebration label can say
    /// "(2 deposits)" when multiple back-to-back receives stack up
    /// before the user dismisses the screen.
    Received { amount_sat: i64, count: u32 },
    /// RPC failed — user-visible error.
    Error(String),
}

/// Real Spark Receive panel.
pub struct SparkReceive {
    backend: Option<Arc<SparkBackend>>,
    /// The Spark wallet's unified balance in sats (BTC + Stable Balance), shown
    /// on the YOU RECEIVE card. Refreshed on reload via `get_info`; `0` until
    /// the first fetch.
    balance_sats: u64,
    /// The bitcoin balance alone and the USDB holding, for the side panel.
    btc_balance_sats: u64,
    usdb_balance: Option<coincube_spark_protocol::UsdbBalanceSnapshot>,
    /// The "receive from another network" (SideShift → BTC) sub-flow, when the
    /// user has entered it. While `Some`, it owns the panel: view, update and
    /// subscription all delegate to it. Mirrors how the Liquid Receive panel
    /// hosts its own SideShift flow.
    sideshift_flow: Option<SparkSideshiftReceiveFlow>,
    /// Currently selected method. Toggling methods resets the phase.
    pub method: SparkReceiveMethod,
    /// Whether the "THEY SEND" picker modal is open (overlays the panel).
    sender_picker_open: bool,
    /// Amount input for BOLT11. Ignored for on-chain.
    pub amount_input: String,
    /// Invoice description shown to the payer for BOLT11. Ignored for on-chain.
    pub description_input: String,
    phase: SparkReceivePhase,
    /// Pre-rendered QR code for the current `Generated` payment
    /// request. Built once when `GenerateSucceeded` fires so the view
    /// renderer doesn't have to re-encode the (potentially long)
    /// BOLT11 invoice on every frame. `None` when no invoice is on
    /// screen, or when encoding failed (unlikely for BOLT11/BTC
    /// addresses but handled gracefully).
    pub qr_data: Option<qr_code::Data>,
    /// QR for the Connect-owned address and the source value used to encode it.
    lightning_address_qr: Option<(String, qr_code::Data)>,
    /// Phase 4f: pending on-chain deposits surfaced by the SDK's
    /// `list_unclaimed_deposits` RPC. Refreshed on panel reload and on
    /// every `Event::DepositsChanged`. The view renders this as a
    /// dedicated "Pending deposits" card below the main phase body.
    pub pending_deposits: Vec<DepositInfo>,
    /// Phase 4f: tracks which deposits are currently being claimed so
    /// the UI can disable each row's button while its RPC is in flight.
    /// Keyed by `(txid, vout)`; each entry is removed when that
    /// deposit's RPC finishes (success or failure). A set, because a
    /// second deposit can be claimed before the first answers.
    pub claiming: HashSet<(String, u32)>,
    /// Phase 4f: surface a transient claim error to the user. Cleared
    /// on the next reload or successful claim.
    pub claim_error: Option<String>,
    /// Deferred claims (SDK 0.28.0+): nothing claimed yet, and the SDK
    /// keeps trying on its own. Each deposit's `(txid, vout)` keeps its
    /// own neutral note until it is retried, claimed or leaves the list.
    pub claim_notices: HashMap<(String, u32), String>,
    /// Live on-chain confirmation count per pending deposit, fetched
    /// from a public Esplora ([`esplora::fetch_confirmations`]). The
    /// SDK only tells us `is_mature: bool`, so we query Esplora
    /// ourselves to surface progress like "1 / 3 confirmations" on
    /// rows that haven't matured yet. Entries are dropped when the
    /// deposit list refreshes; missing keys render the SDK's plain
    /// "Waiting for confirmations" fallback text.
    pub pending_deposit_confirmations: HashMap<(String, u32), u32>,
    /// Phase 4f: the BOLT11 invoice string of the currently-displayed
    /// generated invoice, captured at `GenerateSucceeded` time. Used
    /// by the auto-advance handler to correlate `PaymentSucceeded`
    /// events against THIS invoice instead of accepting any
    /// incoming payment. `None` while in idle / error / received
    /// phases.
    pub displayed_invoice: Option<String>,
    /// Formatted amount string for the celebration screen.
    received_amount_display: String,
    /// Quote context key for the celebration screen (e.g. "lightning-receive").
    received_celebration_context: String,
    /// Quote and image handle for the celebration screen.
    received_quote: coincube_ui::component::quote_display::Quote,
    received_image_handle: iced::widget::image::Handle,
    /// Last few payments fetched from the bridge, rendered under the
    /// receive form. Populated on reload and after an incoming payment.
    recent_transactions: Vec<SparkRecentTransaction>,
}

impl SparkReceive {
    pub fn new(backend: Option<Arc<SparkBackend>>) -> Self {
        Self {
            backend,
            balance_sats: 0,
            btc_balance_sats: 0,
            usdb_balance: None,
            sideshift_flow: None,
            method: SparkReceiveMethod::Bolt11,
            sender_picker_open: false,
            amount_input: String::new(),
            description_input: String::new(),
            phase: SparkReceivePhase::Idle,
            qr_data: None,
            lightning_address_qr: None,
            pending_deposits: Vec::new(),
            claiming: HashSet::new(),
            claim_error: None,
            claim_notices: HashMap::new(),
            pending_deposit_confirmations: HashMap::new(),
            displayed_invoice: None,
            received_amount_display: String::new(),
            received_celebration_context: "lightning-receive".to_string(),
            received_quote: coincube_ui::component::quote_display::random_quote(
                "lightning-receive",
            ),
            received_image_handle: coincube_ui::component::quote_display::image_handle_for_context(
                "lightning-receive",
            ),
            recent_transactions: Vec::new(),
        }
    }

    pub fn phase(&self) -> &SparkReceivePhase {
        &self.phase
    }

    pub fn sync_lightning_address_qr(&mut self, address: Option<&str>) {
        self.lightning_address_qr = address.and_then(|address| {
            qr_code::Data::new(address)
                .ok()
                .map(|qr| (address.to_owned(), qr))
        });
    }

    /// The cross-network shift-status poll on its own, exposed so the app can
    /// keep it alive while the user is on another panel. A swap settles and its
    /// bitcoin is auto-claimed on a ~30-minute timeline; if the settle isn't
    /// observed until the user happens back on this screen, the deposit can
    /// already be claimed by then and the flow hangs on "Bitcoin arriving"
    /// (GlobalHome can only register the arrival while the deposit is still
    /// pending). Polling stops on its own once the shift is terminal, so this
    /// only stays live through the pre-settle window. Returns `none` when there
    /// is no active flow.
    pub fn sideshift_poll_subscription(&self) -> Subscription<Message> {
        self.sideshift_flow
            .as_ref()
            .map(|flow| flow.subscription())
            .unwrap_or_else(Subscription::none)
    }
}

impl State for SparkReceive {
    fn view<'a>(
        &'a self,
        menu: &'a Menu,
        cache: &'a Cache,
    ) -> Element<'a, crate::app::view::Message> {
        let backend_available = self.backend.is_some();
        let connect_signed_in = cache.connect_authenticated;
        let lightning_address = connect_signed_in
            .then_some(cache.lightning_address.as_deref())
            .flatten();
        // The cross-network (SideShift) flow renders *inline* below the two-card
        // selector — not as a takeover — so its refund/deposit steps sit on the
        // same page as the Bitcoin rail forms. Its messages route back through
        // `SparkSideshiftReceive`.
        let (sideshift_body, cross_network_selected) = match &self.sideshift_flow {
            Some(flow) => (
                Some(
                    crate::app::view::spark::spark_sideshift_receive_view(
                        flow,
                        cache.bitcoin_unit,
                        cache.balance_amounts_masked(),
                        settle_confirmations(
                            flow.settle_hash(),
                            flow.settle_amount_sat(),
                            &self.pending_deposits,
                            &self.pending_deposit_confirmations,
                        ),
                        cache.reduce_motion,
                    )
                    .map(crate::app::view::Message::SparkSideshiftReceive),
                ),
                Some(flow.selected()),
            ),
            None => (None, None),
        };
        // The received celebration takes the whole page, head included.
        let head = (!matches!(self.phase, SparkReceivePhase::Received { .. })).then(|| {
            crate::app::view::spark::sub_page_head(cache, "Receive").action(
                crate::app::view::spark::receive::head_action(
                    cross_network_selected.is_some(),
                    self.method,
                    cache.network,
                ),
            )
        });
        let content = crate::app::view::dashboard_payment(
            menu,
            cache,
            head,
            SparkReceiveView {
                backend_available,
                method: self.method,
                amount_input: &self.amount_input,
                description_input: &self.description_input,
                phase: &self.phase,
                qr_data: self.qr_data.as_ref(),
                connect_signed_in,
                lightning_address,
                lightning_address_qr: self.lightning_address_qr.as_ref().and_then(
                    |(source, qr)| (lightning_address == Some(source.as_str())).then_some(qr),
                ),
                pending_deposits: &self.pending_deposits,
                claiming: &self.claiming,
                claim_error: self.claim_error.as_deref(),
                claim_notices: &self.claim_notices,
                pending_deposit_confirmations: &self.pending_deposit_confirmations,
                network: cache.network,
                cube_name: &cache.cube_name,
                received_amount_display: &self.received_amount_display,
                received_celebration_context: &self.received_celebration_context,
                received_quote: &self.received_quote,
                received_image_handle: &self.received_image_handle,
                recent_transactions: &self.recent_transactions,
                balance_sats: self.balance_sats,
                balances: crate::app::view::spark::SparkBalances::new(
                    cache,
                    self.btc_balance_sats,
                    self.usdb_balance.as_ref(),
                ),
                bitcoin_unit: cache.bitcoin_unit,
                show_direction_badges: cache.show_direction_badges,
                mask_amounts: cache.balance_amounts_masked(),
                sideshift_body,
                cross_network_selected,
                swap_deposit: self
                    .sideshift_flow
                    .as_ref()
                    .and_then(|flow| {
                        settle_deposit(
                            flow.settle_hash(),
                            flow.settle_amount_sat(),
                            &self.pending_deposits,
                        )
                    })
                    .map(|d| (d.txid.as_str(), d.vout)),
            }
            .render(),
        );

        // The "THEY SEND" picker overlays the whole panel when open — same
        // pattern as Liquid Receive. Owned here because the open flag lives in
        // this state.
        if self.sender_picker_open {
            let modal_content = crate::app::view::spark::sender_picker_modal(
                self.method,
                self.sideshift_flow.as_ref().map(|flow| flow.selected()),
                cache.network,
            );
            return coincube_ui::widget::modal::Modal::new(content, modal_content)
                .on_blur(Some(crate::app::view::Message::SparkReceive(
                    crate::app::view::SparkReceiveMessage::CloseSenderPicker,
                )))
                .into();
        }
        content
    }

    fn reload(
        &mut self,
        _daemon: Option<Arc<dyn crate::daemon::Daemon + Sync + Send>>,
        _wallet: Option<Arc<crate::app::wallet::Wallet>>,
    ) -> Task<Message> {
        // Refresh the pending-deposits list whenever the panel
        // becomes active. Errors degrade silently — the rest of the
        // panel still works. Also fetch the recent-payments list so
        // the "Last transactions" section under the form is fresh.
        Task::batch(vec![
            fetch_deposits_task(self.backend.clone()),
            fetch_payments_task(self.backend.clone()),
            fetch_balance_task(self.backend.clone()),
        ])
    }

    fn subscription(&self) -> Subscription<Message> {
        // The cross-network flow now renders inline, so its shift-status poll
        // AND the deposit-confirmation poll can be live at once — batch them.
        // Esplora doesn't push, so we tick on a fixed cadence while at least one
        // immature deposit is on screen so the "X / 3 confirmations" badge keeps
        // updating between blocks (the SDK only re-emits `DepositsChanged` at
        // maturity / refund-status transitions, not on every new confirmation).
        let mut subs = Vec::new();
        if let Some(flow) = &self.sideshift_flow {
            subs.push(flow.subscription());
        }
        if self.pending_deposits.iter().any(|d| !d.is_mature) {
            subs.push(
                iced::time::every(std::time::Duration::from_secs(30)).map(|_| {
                    Message::View(crate::app::view::Message::SparkReceive(
                        crate::app::view::SparkReceiveMessage::RefreshConfirmations,
                    ))
                }),
            );
        }
        Subscription::batch(subs)
    }

    fn update(
        &mut self,
        _daemon: Option<Arc<dyn crate::daemon::Daemon + Sync + Send>>,
        cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        // Bridge-flow messages go to the flow, whenever it's up.
        if let Message::View(crate::app::view::Message::SparkSideshiftReceive(ref msg)) = message {
            let Some(flow) = &mut self.sideshift_flow else {
                return Task::none();
            };
            // `Back`/`Reset` from the flow's *entry* screen means "leave the
            // bridge", so tear it down and fall back to the ordinary receive
            // form. The arrived screen's "Done" button lands here too — once the
            // bitcoin is in the wallet the swap is finished, so treat it the same
            // way. The flow resets itself for any other phase.
            let leaving = matches!(
                msg,
                crate::app::view::SparkSideshiftReceiveMessage::Back
                    | crate::app::view::SparkSideshiftReceiveMessage::Reset
            ) && matches!(
                flow.phase(),
                super::sideshift_receive::SparkShiftPhase::Setup
                    | super::sideshift_receive::SparkShiftPhase::Arrived
            );
            if leaving {
                self.sideshift_flow = None;
                return Task::none();
            }
            return flow.update(msg);
        }

        let Message::View(crate::app::view::Message::SparkReceive(msg)) = message else {
            return Task::none();
        };

        use crate::app::view::SparkReceiveMessage;

        // The cross-network flow renders *inline*, so the two-card / THEY SEND
        // picker AND the pending-deposits card stay on screen while it's active —
        // only the Bitcoin invoice form's messages are gated out (its widgets
        // aren't shown). Let the picker, `DepositsChanged`, and the whole
        // pending-deposit lifecycle (list refresh, confirmation ticks, claim +
        // its result) through so those on-screen controls keep working mid-swap.
        if self.sideshift_flow.is_some()
            && !matches!(
                msg,
                SparkReceiveMessage::DepositsChanged
                    | SparkReceiveMessage::OpenSenderPicker
                    | SparkReceiveMessage::CloseSenderPicker
                    | SparkReceiveMessage::SelectSenderRail(_)
                    | SparkReceiveMessage::SelectSenderCrossNetwork(_)
                    | SparkReceiveMessage::ClaimDepositRequested { .. }
                    | SparkReceiveMessage::RefreshConfirmations
                    | SparkReceiveMessage::DepositConfirmationsUpdated(_)
                    | SparkReceiveMessage::PendingDepositsLoaded(_)
                    | SparkReceiveMessage::PendingDepositsFailed(_)
                    | SparkReceiveMessage::ClaimDepositSucceeded { .. }
                    | SparkReceiveMessage::ClaimDepositFailed { .. }
            )
        {
            return Task::none();
        }

        match msg {
            SparkReceiveMessage::OpenSenderPicker => {
                self.sender_picker_open = true;
                Task::none()
            }
            SparkReceiveMessage::CloseSenderPicker => {
                self.sender_picker_open = false;
                Task::none()
            }
            SparkReceiveMessage::SelectSenderRail(method) => {
                // A Bitcoin rail: clear any cross-network flow, set the method,
                // close the picker.
                self.sender_picker_open = false;
                if method == SparkReceiveMethod::LightningAddress {
                    self.sync_lightning_address_qr(
                        cache
                            .connect_authenticated
                            .then_some(cache.lightning_address.as_deref())
                            .flatten(),
                    );
                }
                if self.sideshift_flow.is_none() && self.method == method {
                    return Task::none();
                }
                self.sideshift_flow = None;
                self.method = method;
                self.phase = SparkReceivePhase::Idle;
                self.qr_data = None;
                self.displayed_invoice = None;
                Task::none()
            }
            SparkReceiveMessage::SelectSenderCrossNetwork(key) => {
                // A cross-network asset: hand off to the SideShift flow,
                // pre-selected so it skips its own asset picker. Mainnet only —
                // the picker hides these off mainnet; this is the backstop.
                self.sender_picker_open = false;
                if !crate::app::state::spark::cross_chain::supported_on(cache.network) {
                    return Task::none();
                }
                let Some(backend) = self.backend.clone() else {
                    return Task::none();
                };
                let Some(option) = crate::services::sideshift::deposit_option_by_key(&key) else {
                    return Task::none();
                };
                self.sideshift_flow =
                    Some(SparkSideshiftReceiveFlow::new_preselected(backend, option));
                Task::none()
            }
            SparkReceiveMessage::AmountInputChanged(value) => {
                self.amount_input = value;
                self.phase = SparkReceivePhase::Idle;
                self.qr_data = None;
                self.displayed_invoice = None;
                Task::none()
            }
            SparkReceiveMessage::DescriptionInputChanged(value) => {
                self.description_input = value;
                self.phase = SparkReceivePhase::Idle;
                self.qr_data = None;
                self.displayed_invoice = None;
                Task::none()
            }
            SparkReceiveMessage::GenerateRequested => {
                if self.method == SparkReceiveMethod::LightningAddress {
                    return Task::none();
                }
                let Some(backend) = self.backend.clone() else {
                    self.phase =
                        SparkReceivePhase::Error("Spark backend is not available.".to_string());
                    return Task::none();
                };
                self.phase = SparkReceivePhase::Generating;
                match self.method {
                    SparkReceiveMethod::Bolt11 => {
                        let amount_sat = if self.amount_input.trim().is_empty() {
                            None
                        } else {
                            match self.amount_input.trim().parse::<u64>() {
                                Ok(n) => Some(n),
                                Err(_) => {
                                    self.phase = SparkReceivePhase::Error(
                                        "Amount must be a whole number of sats.".to_string(),
                                    );
                                    return Task::none();
                                }
                            }
                        };
                        let description = self.description_input.clone();
                        Task::perform(
                            async move { backend.receive_bolt11(amount_sat, description, None).await },
                            |result| match result {
                                Ok(ok) => Message::View(crate::app::view::Message::SparkReceive(
                                    SparkReceiveMessage::GenerateSucceeded(ok),
                                )),
                                Err(e) => Message::View(crate::app::view::Message::SparkReceive(
                                    SparkReceiveMessage::GenerateFailed(e.to_string()),
                                )),
                            },
                        )
                    }
                    SparkReceiveMethod::OnchainBitcoin => Task::perform(
                        async move { backend.receive_onchain(None).await },
                        |result| match result {
                            Ok(ok) => Message::View(crate::app::view::Message::SparkReceive(
                                SparkReceiveMessage::GenerateSucceeded(ok),
                            )),
                            Err(e) => Message::View(crate::app::view::Message::SparkReceive(
                                SparkReceiveMessage::GenerateFailed(e.to_string()),
                            )),
                        },
                    ),
                    SparkReceiveMethod::Spark => {
                        Task::perform(async move { backend.receive_spark().await }, |result| {
                            match result {
                                Ok(ok) => Message::View(crate::app::view::Message::SparkReceive(
                                    SparkReceiveMessage::GenerateSucceeded(ok),
                                )),
                                Err(e) => Message::View(crate::app::view::Message::SparkReceive(
                                    SparkReceiveMessage::GenerateFailed(e.to_string()),
                                )),
                            }
                        })
                    }
                    SparkReceiveMethod::LightningAddress => Task::none(),
                }
            }
            SparkReceiveMessage::GenerateSucceeded(ok) => {
                // Encode the QR eagerly so the view renderer doesn't
                // re-encode on every frame.
                self.qr_data = qr_code::Data::new(&ok.payment_request).ok();
                // Only capture the payment request for BOLT11 — it's
                // the correlation key used by PaymentReceived to match
                // the event's bolt11 field against the displayed
                // invoice. For on-chain receives the payment_request
                // is a Bitcoin address (no bolt11 on the event), so
                // displayed_invoice stays None and the (None, _) =>
                // true arm in the correlation check auto-advances on
                // any incoming payment while the address is on screen.
                self.displayed_invoice = if self.method == SparkReceiveMethod::Bolt11 {
                    Some(ok.payment_request.clone())
                } else {
                    None
                };
                self.phase = SparkReceivePhase::Generated(ok);
                Task::none()
            }
            SparkReceiveMessage::GenerateFailed(err) => {
                self.qr_data = None;
                self.displayed_invoice = None;
                self.phase = SparkReceivePhase::Error(err);
                Task::none()
            }
            SparkReceiveMessage::PaymentReceived { amount_sat, bolt11 } => {
                // Accept events while either showing an invoice
                // (`Generated`) — first arrival — or already on the
                // celebration screen (`Received`) — back-to-back
                // deposits accumulate into the running total instead
                // of being silently dropped. Idle / error / generating
                // phases stay no-op.
                let already_celebrating = matches!(self.phase, SparkReceivePhase::Received { .. });
                if !already_celebrating && !matches!(self.phase, SparkReceivePhase::Generated(_)) {
                    return Task::none();
                }

                // Only incoming payments (positive amount) should
                // trigger the celebration. Outgoing events with
                // negative amounts are skipped.
                let is_incoming = amount_sat > 0;
                if !is_incoming {
                    return Task::none();
                }

                // Correlate the event with the currently displayed
                // receive method so we only celebrate the payment the
                // user is actually waiting for:
                //
                // - Bolt11 invoice displayed + matching bolt11 event:
                //   the invoice was paid → advance.
                // - Bolt11 invoice displayed + event without bolt11:
                //   unrelated non-Lightning payment → skip.
                // - No invoice displayed (on-chain flow) + event
                //   without bolt11: on-chain deposit / Spark-native
                //   transfer → advance.
                // - No invoice displayed (on-chain flow) + event with
                //   bolt11: unrelated Lightning payment → skip.
                //
                // BOLT11 comparison is case-insensitive — canonical
                // form is lowercase but some SDKs hand back mixed case.
                //
                // For follow-ups during the celebration we only
                // aggregate when the panel was generating an on-chain /
                // Spark address AND the event has no bolt11 — a BOLT11
                // invoice is single-use and unrelated Lightning
                // activity that happens to settle while the on-chain
                // celebration is on screen would otherwise inflate the
                // running total. For BOLT11 celebrations we never
                // aggregate: a second event after invoice settlement
                // is by definition a different payment.
                let matches_invoice = if already_celebrating {
                    self.method != SparkReceiveMethod::Bolt11 && bolt11.is_none()
                } else {
                    match (&self.displayed_invoice, &bolt11) {
                        (Some(displayed), Some(event_bolt11)) => {
                            displayed.eq_ignore_ascii_case(event_bolt11)
                        }
                        (Some(_), None) => false,
                        (None, None) => true,
                        (None, Some(_)) => false,
                    }
                };
                if !matches_invoice {
                    return Task::none();
                }

                self.qr_data = None;
                self.displayed_invoice = None;
                let (running_total, count) = match self.phase {
                    SparkReceivePhase::Received {
                        amount_sat: prev,
                        count,
                    } => (prev.saturating_add(amount_sat), count.saturating_add(1)),
                    _ => (amount_sat, 1),
                };
                self.received_amount_display = if count > 1 {
                    coincube_ui::localized_format!(
                        "+{} sats ({} deposits)",
                        "+{} sats ({} depósitos)",
                        running_total.unsigned_abs(),
                        count
                    )
                } else {
                    format!("+{} sats", running_total.unsigned_abs())
                };
                // Pick celebration image based on receive method.
                // Only re-roll the quote on the first arrival so the
                // imagery doesn't flicker when a follow-up deposit
                // bumps the total.
                if !already_celebrating {
                    let context = if self.method == SparkReceiveMethod::Bolt11 {
                        "lightning-receive"
                    } else {
                        "spark-receive"
                    };
                    self.received_celebration_context = context.to_string();
                    self.received_quote =
                        coincube_ui::component::quote_display::random_quote(context);
                    self.received_image_handle =
                        coincube_ui::component::quote_display::image_handle_for_context(context);
                }
                self.phase = SparkReceivePhase::Received {
                    amount_sat: running_total,
                    count,
                };
                // Surface the just-received payment in the Last
                // Transactions list the moment it arrives.
                fetch_payments_task(self.backend.clone())
            }
            SparkReceiveMessage::PendingDepositsLoaded(deposits) => {
                self.pending_deposits = deposits;
                self.claim_error = None;
                // Drop confirmation entries for deposits that left
                // the list (claimed or refunded), then kick off a
                // fresh Esplora fetch for any immature deposit still
                // in the list. Mature deposits skip the fetch — they
                // already have a Claim button and the per-confirmation
                // count is no longer useful.
                let live_keys: std::collections::HashSet<(String, u32)> = self
                    .pending_deposits
                    .iter()
                    .map(|d| (d.txid.clone(), d.vout))
                    .collect();
                // Keep every live deposit's deferral note across refreshes.
                self.claim_notices.retain(|k, _| live_keys.contains(k));
                self.pending_deposit_confirmations
                    .retain(|k, _| live_keys.contains(k));
                refresh_confirmations_task(cache.network, &self.pending_deposits)
            }
            SparkReceiveMessage::PendingDepositsFailed(err) => {
                tracing::warn!("Spark list_unclaimed_deposits failed: {}", err);
                // Don't surface as a hard error — the rest of the
                // panel still works. Just clear the displayed list.
                self.pending_deposits.clear();
                self.pending_deposit_confirmations.clear();
                Task::none()
            }
            SparkReceiveMessage::DepositConfirmationsUpdated(map) => {
                // Merge rather than replace: a partial fetch (one
                // deposit's GET failed) shouldn't wipe the confirmation
                // count for the others.
                //
                // Plain overwrite per key — we deliberately do NOT
                // `max` here. A reorg can legitimately drop a
                // deposit's confirmation count (the block at height
                // H got replaced, so the tx is back in the mempool
                // until it re-confirms), and the user should see
                // that reflected. The cost is that if the 30s
                // subscription tick fires a second fetch before the
                // first completes (possible when many immature
                // deposits serialize through Esplora's 8s per-request
                // timeout) and the slower one lands after the faster
                // one, the displayed count can briefly flicker
                // backward until the next tick corrects it — but
                // both responses are valid past-chain snapshots, so
                // it's stale display, not wrong data.
                for (key, confs) in map {
                    self.pending_deposit_confirmations.insert(key, confs);
                }
                Task::none()
            }
            SparkReceiveMessage::RefreshConfirmations => {
                refresh_confirmations_task(cache.network, &self.pending_deposits)
            }
            SparkReceiveMessage::ClaimDepositRequested { txid, vout } => {
                let Some(backend) = self.backend.clone() else {
                    return Task::none();
                };
                let deposit = (txid.clone(), vout);
                self.claim_error = None;
                // A note about another deposit stays on that row.
                self.claim_notices.remove(&deposit);
                self.claiming.insert(deposit);
                let claimed_txid = txid.clone();
                Task::perform(
                    async move { backend.claim_deposit(txid, vout).await },
                    move |result| {
                        let msg = match result {
                            Ok(ok) => {
                                crate::app::view::SparkReceiveMessage::ClaimDepositSucceeded {
                                    txid: claimed_txid,
                                    vout,
                                    ok,
                                }
                            }
                            Err(e) => crate::app::view::SparkReceiveMessage::ClaimDepositFailed {
                                txid: claimed_txid,
                                vout,
                                error: e.to_string(),
                            },
                        };
                        Message::View(crate::app::view::Message::SparkReceive(msg))
                    },
                )
            }
            SparkReceiveMessage::ClaimDepositSucceeded { txid, vout, ok } => {
                // Key everything by the deposit this result is for, not by
                // whichever claim was started last.
                let deposit = (txid, vout);
                self.claiming.remove(&deposit);
                self.claim_error = None;
                if let Some(deferral) = &ok.deferred {
                    // Nothing changed: no splash, no refetch (a reload would
                    // not show anything new), and the row stays pending.
                    self.claim_notices
                        .insert(deposit, claim_deferral_notice(deferral));
                    return Task::none();
                }
                self.claim_notices.remove(&deposit);
                let mut tasks = vec![
                    // Drop the claimed row from the pending-deposits list.
                    fetch_deposits_task(self.backend.clone()),
                    // Refresh "Last transactions" so the just-claimed deposit
                    // shows without navigating away and back — a claim fires only
                    // `DepositsChanged`, not a payment event, so nothing else
                    // repopulates the payments list here.
                    fetch_payments_task(self.backend.clone()),
                ];
                // The bitcoin just landed in the spendable balance — fire the
                // global "received" splash, same as an auto-claimed swap. A
                // pre-maturity claim (SDK 0.25.0+) settles asynchronously and
                // carries no amount yet; its `PaymentSucceeded` fires the
                // splash when the transfer lands instead.
                if let Some(amount_sat) = ok.amount_sat {
                    tasks.push(Task::done(Message::ShowReceivedCelebration {
                        context: "spark-receive".to_string(),
                        amount_sat,
                    }));
                }
                Task::batch(tasks)
            }
            SparkReceiveMessage::ClaimDepositFailed { txid, vout, error } => {
                let deposit = (txid, vout);
                self.claiming.remove(&deposit);
                self.claim_notices.remove(&deposit);
                self.claim_error = Some(error);
                Task::none()
            }
            SparkReceiveMessage::DepositsChanged => Task::batch(vec![
                fetch_deposits_task(self.backend.clone()),
                // Also refresh Last transactions: a claim (or any deposit landing)
                // surfaces a new payment, and the panel won't otherwise repopulate
                // the list until the next navigation.
                fetch_payments_task(self.backend.clone()),
            ]),
            SparkReceiveMessage::Reset => {
                self.qr_data = None;
                self.displayed_invoice = None;
                self.phase = SparkReceivePhase::Idle;
                Task::none()
            }
            SparkReceiveMessage::BalanceLoaded(balance) => {
                if let Some((btc_sats, stable)) = balance {
                    self.balance_sats =
                        super::unified_spark_balance_sats(btc_sats, stable.as_ref(), cache);
                    self.btc_balance_sats = btc_sats;
                    self.usdb_balance = stable;
                }
                Task::none()
            }
            SparkReceiveMessage::PaymentsLoaded(payments) => {
                let fiat_converter: Option<FiatAmountConverter> =
                    cache.fiat_price.as_ref().and_then(|p| p.try_into().ok());
                self.recent_transactions = payments
                    .iter()
                    .take(5)
                    .map(|p| {
                        crate::app::state::spark::overview::payment_summary_to_recent_tx(
                            p,
                            fiat_converter.as_ref(),
                        )
                    })
                    .collect();
                Task::none()
            }
            SparkReceiveMessage::PaymentsFailed(err) => {
                tracing::warn!("spark receive list_payments failed: {}", err);
                self.recent_transactions.clear();
                Task::none()
            }
            SparkReceiveMessage::SelectTransaction(idx) => {
                if let Some(payment) = self.recent_transactions.get(idx).cloned() {
                    Task::batch(vec![
                        redirect(Menu::Spark(SparkSubMenu::Transactions(None))),
                        Task::done(Message::View(crate::app::view::Message::SparkTransactions(
                            crate::app::view::SparkTransactionsMessage::Preselect(payment),
                        ))),
                    ])
                } else {
                    Task::none()
                }
            }
            SparkReceiveMessage::History => redirect(Menu::Spark(SparkSubMenu::Transactions(None))),
            SparkReceiveMessage::CopyPaymentRequest(text) => {
                if self.method == SparkReceiveMethod::LightningAddress
                    && (!cache.connect_authenticated
                        || cache.lightning_address.as_deref() != Some(text.as_str()))
                {
                    return Task::none();
                }
                // Pair the clipboard write with the global toast overlay so
                // the button gives the same feedback as every other Copy in
                // the app. The label names the rail the request belongs to —
                // an on-chain deposit address and a BOLT11 invoice look
                // nothing alike, and the confirmation is a cheap way to say
                // which one landed on the clipboard.
                let label = match self.method {
                    SparkReceiveMethod::Bolt11 => "Copied Lightning invoice to clipboard",
                    SparkReceiveMethod::LightningAddress => "Copied Lightning address to clipboard",
                    SparkReceiveMethod::OnchainBitcoin => "Copied Bitcoin address to clipboard",
                    SparkReceiveMethod::Spark => "Copied Spark address to clipboard",
                };
                Task::batch([
                    iced::clipboard::write(text),
                    Task::done(Message::View(crate::app::view::Message::ShowToast(
                        log::Level::Info,
                        label.to_string(),
                    ))),
                ])
            }
        }
    }
}

/// Panel-local thin wrapper around the shared
/// [`super::fetch_payments_task`] helper — only the message variants
/// differ between the Send and Receive panels.
fn fetch_payments_task(backend: Option<Arc<SparkBackend>>) -> Task<Message> {
    super::fetch_payments_task(
        backend,
        |payments| {
            Message::View(crate::app::view::Message::SparkReceive(
                crate::app::view::SparkReceiveMessage::PaymentsLoaded(payments),
            ))
        },
        |err| {
            Message::View(crate::app::view::Message::SparkReceive(
                crate::app::view::SparkReceiveMessage::PaymentsFailed(err),
            ))
        },
    )
}

/// Spark-Receive-flavoured wrapper around [`super::fetch_balance_task`].
fn fetch_balance_task(backend: Option<Arc<SparkBackend>>) -> Task<Message> {
    super::fetch_balance_task(backend, |balance| {
        Message::View(crate::app::view::Message::SparkReceive(
            crate::app::view::SparkReceiveMessage::BalanceLoaded(balance),
        ))
    })
}

/// Kick off an Esplora confirmation-count fetch for every immature
/// deposit in `deposits`. Returns `Task::none()` when there's nothing
/// to fetch, or when `network` has no public Esplora to hit — keeps
/// the call site at `PendingDepositsLoaded` / `RefreshConfirmations`
/// branch-free.
fn refresh_confirmations_task(
    network: coincube_core::miniscript::bitcoin::Network,
    deposits: &[DepositInfo],
) -> Task<Message> {
    if !super::esplora::is_supported(network) {
        return Task::none();
    }
    let targets: Vec<(String, u32)> = deposits
        .iter()
        .filter(|d| !d.is_mature)
        .map(|d| (d.txid.clone(), d.vout))
        .collect();
    if targets.is_empty() {
        return Task::none();
    }
    Task::perform(
        super::esplora::fetch_confirmations(network, targets),
        |map| {
            Message::View(crate::app::view::Message::SparkReceive(
                crate::app::view::SparkReceiveMessage::DepositConfirmationsUpdated(map),
            ))
        },
    )
}

/// Fire a `list_unclaimed_deposits` RPC and translate the result into
/// the appropriate view message. Pulled out as a helper so the
/// `reload`, `ClaimDepositSucceeded`, and `DepositsChanged` paths can
/// share it without duplicating the closure boilerplate.
fn fetch_deposits_task(backend: Option<Arc<SparkBackend>>) -> Task<Message> {
    let Some(backend) = backend else {
        return Task::none();
    };
    Task::perform(
        async move { backend.list_unclaimed_deposits().await },
        |result| match result {
            Ok(ok) => Message::View(crate::app::view::Message::SparkReceive(
                crate::app::view::SparkReceiveMessage::PendingDepositsLoaded(ok.deposits),
            )),
            Err(e) => Message::View(crate::app::view::Message::SparkReceive(
                crate::app::view::SparkReceiveMessage::PendingDepositsFailed(e.to_string()),
            )),
        },
    )
}

/// What a deferred claim (SDK 0.28.0+) means for the user, under the Pending
/// deposits card. Nothing went wrong, so it says why it waits and that it
/// will be claimed without them.
pub(crate) fn claim_deferral_notice(deferral: &coincube_spark_protocol::ClaimDeferral) -> String {
    use coincube_spark_protocol::ClaimDeferral;
    match deferral {
        ClaimDeferral::MaxFeeExceeded {
            required_fee_sats,
            max_fee_sats,
        } => coincube_ui::localized_format!(
            "Not claimed yet: claiming now would cost {} sats, more than the {}-sat limit. \
             It will be claimed automatically once the deposit matures.",
            "Aún no reclamado: reclamarlo ahora costaría {} sats, más que el límite de {} sats. \
             Se reclamará automáticamente cuando el depósito madure.",
            required_fee_sats,
            max_fee_sats
        ),
        ClaimDeferral::NoEarlyClaimAvailable => coincube_ui::localized_format!(
            "Not claimed yet: it needs more confirmations. It will be claimed automatically.",
            "Aún no reclamado: necesita más confirmaciones. Se reclamará automáticamente."
        ),
        ClaimDeferral::ProviderDeclined { message } => coincube_ui::localized_format!(
            "Not claimed yet: the claim service declined ({}). It will be claimed automatically \
             once the deposit matures.",
            "Aún no reclamado: el servicio de reclamación lo rechazó ({}). Se reclamará \
             automáticamente cuando el depósito madure.",
            message
        ),
    }
}

/// The wallet's pending (unclaimed) deposit that is a cross-network swap's
/// settle output: by SideShift's settle txid when it has reported one, else by
/// an amount that matches exactly one deposit — never a guess between two.
fn settle_deposit<'d>(
    settle_hash: Option<&str>,
    settle_amount_sat: Option<u64>,
    deposits: &'d [DepositInfo],
) -> Option<&'d DepositInfo> {
    match settle_hash {
        Some(hash) => deposits.iter().find(|d| d.txid.eq_ignore_ascii_case(hash)),
        None => {
            let sats = settle_amount_sat?;
            let mut matches = deposits.iter().filter(|d| d.amount_sat == sats);
            match (matches.next(), matches.next()) {
                (Some(only), None) => Some(only),
                _ => None,
            }
        }
    }
}

/// Confirmations on a cross-network swap's settle transaction (see
/// [`settle_deposit`]). A matured deposit counts as fully confirmed even if its
/// exact count hasn't been fetched. `None` until the settle shows up.
fn settle_confirmations(
    settle_hash: Option<&str>,
    settle_amount_sat: Option<u64>,
    deposits: &[DepositInfo],
    confirmations: &HashMap<(String, u32), u32>,
) -> Option<u32> {
    let deposit = settle_deposit(settle_hash, settle_amount_sat, deposits)?;
    let fetched = confirmations
        .get(&(deposit.txid.clone(), deposit.vout))
        .copied();
    if deposit.is_mature {
        Some(fetched.unwrap_or(0).max(DEPOSIT_MATURITY_CONFIRMATIONS))
    } else {
        Some(fetched.unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::message::Message as AppMessage;
    use crate::app::state::State;
    use crate::app::view::{Message as ViewMessage, SparkReceiveMessage};
    use coincube_core::miniscript::bitcoin::Network;
    use coincube_spark_protocol::{PaymentSummary, UsdbBalanceSnapshot};

    fn receive_ok(payment_request: &str) -> ReceivePaymentOk {
        ReceivePaymentOk {
            payment_request: payment_request.to_string(),
            fee_sat: 7,
        }
    }

    #[test]
    fn settle_confirmations_find_the_swap_deposit_by_txid_then_by_unique_amount() {
        let mut a = deposit("aa11", 0, false);
        a.amount_sat = 31_250;
        let mut b = deposit("bb22", 1, false);
        b.amount_sat = 50_000;
        let deposits = vec![a, b];
        let confs: HashMap<(String, u32), u32> =
            [(("aa11".to_string(), 0), 1), (("bb22".to_string(), 1), 2)].into();

        // The settle txid wins, whatever the amount says.
        assert_eq!(
            settle_confirmations(Some("BB22"), Some(31_250), &deposits, &confs),
            Some(2)
        );
        // A txid that isn't (yet) a pending deposit: nothing to show.
        assert_eq!(
            settle_confirmations(Some("cc33"), Some(31_250), &deposits, &confs),
            None
        );
        // No txid: a unique exact amount match.
        assert_eq!(
            settle_confirmations(None, Some(31_250), &deposits, &confs),
            Some(1)
        );
        assert_eq!(settle_confirmations(None, None, &deposits, &confs), None);

        // Two deposits of the same amount: refuse to guess.
        let mut twin = deposit("dd44", 0, false);
        twin.amount_sat = 31_250;
        let ambiguous = vec![deposits[0].clone(), twin];
        assert_eq!(
            settle_confirmations(None, Some(31_250), &ambiguous, &confs),
            None
        );

        // Found but not yet counted reads 0; matured reads at least the threshold.
        assert_eq!(
            settle_confirmations(Some("aa11"), None, &deposits, &HashMap::new()),
            Some(0)
        );
        let mut mature = deposit("ee55", 0, true);
        mature.amount_sat = 1;
        assert_eq!(
            settle_confirmations(Some("ee55"), None, &[mature], &HashMap::new()),
            Some(DEPOSIT_MATURITY_CONFIRMATIONS)
        );
    }

    fn deposit(txid: &str, vout: u32, is_mature: bool) -> DepositInfo {
        DepositInfo {
            txid: txid.to_string(),
            vout,
            amount_sat: 12_345,
            is_mature,
            claim_error: None,
        }
    }

    fn payment(id: &str, amount_sat: i64) -> PaymentSummary {
        PaymentSummary {
            id: id.to_string(),
            amount_sat,
            fees_sat: 12,
            token_amount: None,
            token_decimals: None,
            token_ticker: None,
            timestamp: 1_700_000_000,
            status: "completed".to_string(),
            direction: "receive".to_string(),
            method: "spark".to_string(),
            description: None,
            details: Default::default(),
        }
    }

    fn usdb_balance(balance: u64, decimals: u32) -> UsdbBalanceSnapshot {
        UsdbBalanceSnapshot {
            token_identifier: "btkn1synthetic".into(),
            balance: balance.into(),
            decimals,
            ticker: "USDB".to_string(),
        }
    }

    fn update_with_cache(panel: &mut SparkReceive, cache: &Cache, msg: SparkReceiveMessage) {
        let _task = State::update(
            panel,
            None,
            cache,
            AppMessage::View(ViewMessage::SparkReceive(msg)),
        );
    }

    fn update(panel: &mut SparkReceive, msg: SparkReceiveMessage) {
        update_with_cache(panel, &Cache::default(), msg);
    }

    #[test]
    fn receive_method_labels_are_stable() {
        assert_eq!(SparkReceiveMethod::Bolt11.label(), "Lightning (BOLT11)");
        assert_eq!(
            SparkReceiveMethod::LightningAddress.label(),
            "Lightning address"
        );
        assert_eq!(
            SparkReceiveMethod::OnchainBitcoin.label(),
            "On-chain Bitcoin"
        );
        assert_eq!(SparkReceiveMethod::Spark.label(), "Spark");
    }

    #[test]
    fn lightning_address_selection_uses_only_signed_in_current_mirror() {
        let mut panel = SparkReceive::new(None);
        let mut cache = Cache {
            lightning_address: Some("synthetic@coincube.io".to_owned()),
            has_connect_session: true,
            ..Default::default()
        };
        update_with_cache(
            &mut panel,
            &cache,
            SparkReceiveMessage::SelectSenderRail(SparkReceiveMethod::LightningAddress),
        );
        assert_eq!(panel.method, SparkReceiveMethod::LightningAddress);
        assert!(panel.lightning_address_qr.is_none());
        cache.connect_authenticated = true;
        update_with_cache(
            &mut panel,
            &cache,
            SparkReceiveMessage::SelectSenderRail(SparkReceiveMethod::LightningAddress),
        );
        assert_eq!(
            panel
                .lightning_address_qr
                .as_ref()
                .map(|(value, _)| value.as_str()),
            Some("synthetic@coincube.io")
        );
        cache.connect_authenticated = false;
        update_with_cache(
            &mut panel,
            &cache,
            SparkReceiveMessage::SelectSenderRail(SparkReceiveMethod::LightningAddress),
        );
        assert!(panel.lightning_address_qr.is_none());
    }

    #[test]
    fn new_panel_starts_in_bolt11_idle_state() {
        let panel = SparkReceive::new(None);

        assert!(panel.backend.is_none());
        assert_eq!(panel.balance_sats, 0);
        assert_eq!(panel.method, SparkReceiveMethod::Bolt11);
        assert!(!panel.sender_picker_open);
        assert!(matches!(panel.phase(), SparkReceivePhase::Idle));
        assert!(panel.qr_data.is_none());
        assert!(panel.displayed_invoice.is_none());
        assert!(panel.pending_deposits.is_empty());
        assert!(panel.recent_transactions.is_empty());
    }

    #[test]
    fn generate_without_backend_surfaces_actionable_error() {
        let mut panel = SparkReceive::new(None);

        update(&mut panel, SparkReceiveMessage::GenerateRequested);

        assert!(matches!(
            panel.phase(),
            SparkReceivePhase::Error(msg) if msg == "Spark backend is not available."
        ));
    }

    #[test]
    fn input_edits_reset_generated_payment_state() {
        let mut panel = SparkReceive::new(None);

        update(
            &mut panel,
            SparkReceiveMessage::GenerateSucceeded(receive_ok("lnbc1invoice")),
        );
        assert!(matches!(panel.phase(), SparkReceivePhase::Generated(_)));
        assert!(panel.qr_data.is_some());
        assert_eq!(panel.displayed_invoice.as_deref(), Some("lnbc1invoice"));

        update(
            &mut panel,
            SparkReceiveMessage::AmountInputChanged("2500".to_string()),
        );

        assert_eq!(panel.amount_input, "2500");
        assert!(matches!(panel.phase(), SparkReceivePhase::Idle));
        assert!(panel.qr_data.is_none());
        assert!(panel.displayed_invoice.is_none());

        update(
            &mut panel,
            SparkReceiveMessage::GenerateSucceeded(receive_ok("lnbc1invoice2")),
        );
        update(
            &mut panel,
            SparkReceiveMessage::DescriptionInputChanged("for coffee".to_string()),
        );

        assert_eq!(panel.description_input, "for coffee");
        assert!(matches!(panel.phase(), SparkReceivePhase::Idle));
        assert!(panel.qr_data.is_none());
        assert!(panel.displayed_invoice.is_none());
    }

    #[test]
    fn generated_bolt11_payment_captures_invoice_for_correlation() {
        let mut panel = SparkReceive::new(None);

        update(
            &mut panel,
            SparkReceiveMessage::GenerateSucceeded(receive_ok("lnbc1invoice")),
        );

        assert!(matches!(panel.phase(), SparkReceivePhase::Generated(_)));
        assert_eq!(panel.displayed_invoice.as_deref(), Some("lnbc1invoice"));
        assert!(panel.qr_data.is_some());
    }

    #[test]
    fn generated_onchain_payment_does_not_capture_invoice() {
        let mut panel = SparkReceive::new(None);
        panel.method = SparkReceiveMethod::OnchainBitcoin;

        update(
            &mut panel,
            SparkReceiveMessage::GenerateSucceeded(receive_ok("bc1qaddress")),
        );

        assert!(matches!(panel.phase(), SparkReceivePhase::Generated(_)));
        assert!(panel.displayed_invoice.is_none());
        assert!(panel.qr_data.is_some());
    }

    #[test]
    fn failed_generation_clears_qr_and_invoice_state() {
        let mut panel = SparkReceive::new(None);
        update(
            &mut panel,
            SparkReceiveMessage::GenerateSucceeded(receive_ok("lnbc1invoice")),
        );

        update(
            &mut panel,
            SparkReceiveMessage::GenerateFailed("bridge down".to_string()),
        );

        assert!(matches!(
            panel.phase(),
            SparkReceivePhase::Error(msg) if msg == "bridge down"
        ));
        assert!(panel.qr_data.is_none());
        assert!(panel.displayed_invoice.is_none());
    }

    #[test]
    fn payment_received_ignores_idle_outgoing_and_unrelated_events() {
        let mut panel = SparkReceive::new(None);

        update(
            &mut panel,
            SparkReceiveMessage::PaymentReceived {
                amount_sat: 100,
                bolt11: None,
            },
        );
        assert!(matches!(panel.phase(), SparkReceivePhase::Idle));

        update(
            &mut panel,
            SparkReceiveMessage::GenerateSucceeded(receive_ok("lnbc1invoice")),
        );
        update(
            &mut panel,
            SparkReceiveMessage::PaymentReceived {
                amount_sat: -100,
                bolt11: Some("lnbc1invoice".to_string()),
            },
        );
        assert!(matches!(panel.phase(), SparkReceivePhase::Generated(_)));

        update(
            &mut panel,
            SparkReceiveMessage::PaymentReceived {
                amount_sat: 100,
                bolt11: Some("lnbc1other".to_string()),
            },
        );
        assert!(matches!(panel.phase(), SparkReceivePhase::Generated(_)));
    }

    #[test]
    fn payment_received_matches_bolt11_case_insensitively() {
        let mut panel = SparkReceive::new(None);

        update(
            &mut panel,
            SparkReceiveMessage::GenerateSucceeded(receive_ok("LNBC1Invoice")),
        );
        update(
            &mut panel,
            SparkReceiveMessage::PaymentReceived {
                amount_sat: 321,
                bolt11: Some("lnbc1invoice".to_string()),
            },
        );

        assert!(matches!(
            panel.phase(),
            SparkReceivePhase::Received {
                amount_sat: 321,
                count: 1
            }
        ));
        assert_eq!(panel.received_amount_display, "+321 sats");
        assert_eq!(panel.received_celebration_context, "lightning-receive");
        assert!(panel.qr_data.is_none());
        assert!(panel.displayed_invoice.is_none());
    }

    #[test]
    fn onchain_receive_events_accumulate_while_celebrating() {
        let mut panel = SparkReceive::new(None);
        panel.method = SparkReceiveMethod::OnchainBitcoin;

        update(
            &mut panel,
            SparkReceiveMessage::GenerateSucceeded(receive_ok("bc1qaddress")),
        );
        update(
            &mut panel,
            SparkReceiveMessage::PaymentReceived {
                amount_sat: 100,
                bolt11: None,
            },
        );
        update(
            &mut panel,
            SparkReceiveMessage::PaymentReceived {
                amount_sat: 50,
                bolt11: None,
            },
        );

        assert!(matches!(
            panel.phase(),
            SparkReceivePhase::Received {
                amount_sat: 150,
                count: 2
            }
        ));
        assert_eq!(panel.received_amount_display, "+150 sats (2 deposits)");
        assert_eq!(panel.received_celebration_context, "spark-receive");
    }

    #[test]
    fn reset_returns_to_idle_and_clears_generated_artifacts() {
        let mut panel = SparkReceive::new(None);
        update(
            &mut panel,
            SparkReceiveMessage::GenerateSucceeded(receive_ok("lnbc1invoice")),
        );

        update(&mut panel, SparkReceiveMessage::Reset);

        assert!(matches!(panel.phase(), SparkReceivePhase::Idle));
        assert!(panel.qr_data.is_none());
        assert!(panel.displayed_invoice.is_none());
    }

    #[test]
    fn sender_picker_opens_closes_and_rail_selection_resets_receive_state() {
        let mut panel = SparkReceive::new(None);
        update(
            &mut panel,
            SparkReceiveMessage::GenerateSucceeded(receive_ok("lnbc1invoice")),
        );

        update(&mut panel, SparkReceiveMessage::OpenSenderPicker);
        assert!(panel.sender_picker_open);

        update(&mut panel, SparkReceiveMessage::CloseSenderPicker);
        assert!(!panel.sender_picker_open);

        update(&mut panel, SparkReceiveMessage::OpenSenderPicker);
        update(
            &mut panel,
            SparkReceiveMessage::SelectSenderRail(SparkReceiveMethod::Spark),
        );

        assert!(!panel.sender_picker_open);
        assert_eq!(panel.method, SparkReceiveMethod::Spark);
        assert!(panel.sideshift_flow.is_none());
        assert!(matches!(panel.phase(), SparkReceivePhase::Idle));
        assert!(panel.qr_data.is_none());
        assert!(panel.displayed_invoice.is_none());
    }

    #[test]
    fn selecting_current_bitcoin_kind_keeps_generated_request_and_qr() {
        for (method, request) in [
            (SparkReceiveMethod::Bolt11, "lnbc1invoice"),
            (SparkReceiveMethod::Spark, "sp1qaddress"),
            (SparkReceiveMethod::OnchainBitcoin, "bc1qaddress"),
        ] {
            let mut panel = SparkReceive::new(None);
            panel.method = method;
            update(
                &mut panel,
                SparkReceiveMessage::GenerateSucceeded(receive_ok(request)),
            );
            assert!(panel.qr_data.is_some());

            update(&mut panel, SparkReceiveMessage::SelectSenderRail(method));

            assert_eq!(panel.method, method);
            assert!(
                matches!(panel.phase(), SparkReceivePhase::Generated(ok) if ok.payment_request == request)
            );
            assert!(panel.qr_data.is_some());
            if method == SparkReceiveMethod::Bolt11 {
                assert_eq!(panel.displayed_invoice.as_deref(), Some(request));
            }
        }
    }

    #[test]
    fn pending_deposits_reload_prunes_stale_confirmations_and_clears_error() {
        let mut panel = SparkReceive::new(None);
        panel.claim_error = Some("old claim error".to_string());
        panel
            .pending_deposit_confirmations
            .insert(("kept".to_string(), 0), 2);
        panel
            .pending_deposit_confirmations
            .insert(("stale".to_string(), 1), 6);

        update(
            &mut panel,
            SparkReceiveMessage::PendingDepositsLoaded(vec![deposit("kept", 0, false)]),
        );

        assert_eq!(panel.pending_deposits.len(), 1);
        assert_eq!(panel.pending_deposits[0].txid, "kept");
        assert!(panel.claim_error.is_none());
        assert_eq!(
            panel
                .pending_deposit_confirmations
                .get(&("kept".to_string(), 0)),
            Some(&2)
        );
        assert!(!panel
            .pending_deposit_confirmations
            .contains_key(&("stale".to_string(), 1)));
    }

    #[test]
    fn pending_deposit_failures_clear_list_and_confirmation_cache() {
        let mut panel = SparkReceive::new(None);
        panel.pending_deposits = vec![deposit("tx", 0, false)];
        panel
            .pending_deposit_confirmations
            .insert(("tx".to_string(), 0), 1);

        update(
            &mut panel,
            SparkReceiveMessage::PendingDepositsFailed("timeout".to_string()),
        );

        assert!(panel.pending_deposits.is_empty());
        assert!(panel.pending_deposit_confirmations.is_empty());
    }

    #[test]
    fn confirmation_updates_merge_and_can_move_backward() {
        let mut panel = SparkReceive::new(None);
        panel
            .pending_deposit_confirmations
            .insert(("a".to_string(), 0), 6);
        panel
            .pending_deposit_confirmations
            .insert(("b".to_string(), 1), 1);

        update(
            &mut panel,
            SparkReceiveMessage::DepositConfirmationsUpdated(HashMap::from([
                (("a".to_string(), 0), 3),
                (("c".to_string(), 2), 0),
            ])),
        );

        assert_eq!(
            panel
                .pending_deposit_confirmations
                .get(&("a".to_string(), 0)),
            Some(&3)
        );
        assert_eq!(
            panel
                .pending_deposit_confirmations
                .get(&("b".to_string(), 1)),
            Some(&1)
        );
        assert_eq!(
            panel
                .pending_deposit_confirmations
                .get(&("c".to_string(), 2)),
            Some(&0)
        );
    }

    #[test]
    fn claim_result_messages_update_claim_error_state() {
        let mut panel = SparkReceive::new(None);

        panel.claim_error = Some("old claim error".to_string());
        update(
            &mut panel,
            SparkReceiveMessage::ClaimDepositRequested {
                txid: "tx".to_string(),
                vout: 0,
            },
        );
        assert!(panel.claiming.is_empty());
        assert_eq!(panel.claim_error.as_deref(), Some("old claim error"));

        panel.claiming.insert(("tx".to_string(), 0));
        update(
            &mut panel,
            SparkReceiveMessage::ClaimDepositFailed {
                txid: "tx".to_string(),
                vout: 0,
                error: "claim failed".to_string(),
            },
        );
        assert!(panel.claiming.is_empty());
        assert_eq!(panel.claim_error.as_deref(), Some("claim failed"));

        panel.claiming.insert(("tx".to_string(), 0));
        panel.claim_error = Some("old".to_string());
        update(
            &mut panel,
            SparkReceiveMessage::ClaimDepositSucceeded {
                txid: "tx".to_string(),
                vout: 0,
                ok: coincube_spark_protocol::ClaimDepositOk {
                    payment_id: Some("payment".to_string()),
                    amount_sat: Some(5_000),
                    deferred: None,
                },
            },
        );
        assert!(panel.claiming.is_empty());
        assert!(panel.claim_error.is_none());
        assert!(panel.claim_notices.is_empty());
    }

    /// 0.28.0: a deferred claim leaves a neutral note, not an error, and
    /// keeps it across a reload until the deposit leaves the list.
    #[test]
    fn a_deferred_claim_leaves_a_note_until_the_deposit_is_claimed() {
        let mut panel = SparkReceive::new(None);
        panel.claiming.insert(("tx".to_string(), 0));
        update(
            &mut panel,
            SparkReceiveMessage::ClaimDepositSucceeded {
                txid: "tx".to_string(),
                vout: 0,
                ok: coincube_spark_protocol::ClaimDepositOk {
                    payment_id: None,
                    amount_sat: None,
                    deferred: Some(coincube_spark_protocol::ClaimDeferral::NoEarlyClaimAvailable),
                },
            },
        );
        assert!(panel.claiming.is_empty());
        assert!(panel.claim_error.is_none());
        let deposit_key = ("tx".to_string(), 0);
        let note = panel
            .claim_notices
            .get(&deposit_key)
            .expect("deferral note")
            .clone();
        assert!(note.contains("claimed automatically"), "{}", note);

        update(
            &mut panel,
            SparkReceiveMessage::PendingDepositsLoaded(vec![
                deposit("tx", 0, true),
                deposit("other", 1, true),
            ]),
        );
        assert_eq!(panel.claim_notices.get(&deposit_key), Some(&note));

        // The deferred deposit is claimed while another is still pending:
        // the note was about the first one only, so it goes.
        update(
            &mut panel,
            SparkReceiveMessage::PendingDepositsLoaded(vec![deposit("other", 1, true)]),
        );
        assert!(panel.claim_notices.is_empty());
    }

    #[test]
    fn deferred_claims_keep_notes_for_both_outputs() {
        for (first, second) in [(0, 1), (1, 0)] {
            let mut panel = SparkReceive::new(None);
            for vout in [first, second] {
                update(
                    &mut panel,
                    SparkReceiveMessage::ClaimDepositSucceeded {
                        txid: "shared-tx".to_string(),
                        vout,
                        ok: coincube_spark_protocol::ClaimDepositOk {
                            payment_id: None,
                            amount_sat: None,
                            deferred: Some(
                                coincube_spark_protocol::ClaimDeferral::NoEarlyClaimAvailable,
                            ),
                        },
                    },
                );
            }
            for vout in [first, second] {
                assert!(
                    panel
                        .claim_notices
                        .contains_key(&("shared-tx".to_string(), vout)),
                    "each output must keep its note in either response order"
                );
            }
            assert_eq!(panel.claim_notices.len(), 2);

            // A list refresh retains both notes; removing one output only
            // removes that output's note, even with a shared transaction id.
            update(
                &mut panel,
                SparkReceiveMessage::PendingDepositsLoaded(vec![
                    deposit("shared-tx", first, true),
                    deposit("shared-tx", second, true),
                ]),
            );
            assert_eq!(panel.claim_notices.len(), 2);
            update(
                &mut panel,
                SparkReceiveMessage::PendingDepositsLoaded(vec![deposit(
                    "shared-tx",
                    second,
                    true,
                )]),
            );
            assert_eq!(panel.claim_notices.len(), 1);
            assert!(panel
                .claim_notices
                .contains_key(&("shared-tx".to_string(), second)));
            update(
                &mut panel,
                SparkReceiveMessage::PendingDepositsLoaded(vec![]),
            );
            assert!(panel.claim_notices.is_empty());
        }
    }

    #[test]
    fn claim_lifecycle_clears_only_its_own_notice() {
        let a = ("shared-tx".to_string(), 0);
        let b = ("shared-tx".to_string(), 1);
        for message in [
            SparkReceiveMessage::ClaimDepositRequested {
                txid: a.0.clone(),
                vout: a.1,
            },
            SparkReceiveMessage::ClaimDepositSucceeded {
                txid: a.0.clone(),
                vout: a.1,
                ok: coincube_spark_protocol::ClaimDepositOk {
                    payment_id: Some("payment".to_string()),
                    amount_sat: None,
                    deferred: None,
                },
            },
            SparkReceiveMessage::ClaimDepositFailed {
                txid: a.0.clone(),
                vout: a.1,
                error: "claim failed".to_string(),
            },
        ] {
            let client = crate::app::breez_spark::SparkClient::disconnected_for_test();
            let mut panel = SparkReceive::new(Some(Arc::new(SparkBackend::new(Arc::new(client)))));
            panel.claim_notices.insert(a.clone(), "A waits".to_string());
            panel.claim_notices.insert(b.clone(), "B waits".to_string());
            update(&mut panel, message);
            assert_eq!(
                panel.claim_notices,
                HashMap::from([(b.clone(), "B waits".to_string())])
            );
        }
    }

    /// Two claims in flight: each result is keyed by the deposit it was
    /// for, whichever claim started last and whichever answers first.
    #[test]
    fn overlapping_claims_key_results_by_their_own_deposit() {
        let a = ("a".to_string(), 0);
        let b = ("b".to_string(), 1);
        let deferred = |txid: &str, vout| SparkReceiveMessage::ClaimDepositSucceeded {
            txid: txid.to_string(),
            vout,
            ok: coincube_spark_protocol::ClaimDepositOk {
                payment_id: None,
                amount_sat: None,
                deferred: Some(coincube_spark_protocol::ClaimDeferral::NoEarlyClaimAvailable),
            },
        };
        let claimed = |txid: &str, vout| SparkReceiveMessage::ClaimDepositSucceeded {
            txid: txid.to_string(),
            vout,
            ok: coincube_spark_protocol::ClaimDepositOk {
                payment_id: Some("payment".to_string()),
                amount_sat: None,
                deferred: None,
            },
        };

        // A then B started; A's deferral answers first. The note is A's,
        // and B's row stays "Claiming…".
        let mut panel = SparkReceive::new(None);
        panel.claiming.insert(a.clone());
        panel.claiming.insert(b.clone());
        update(&mut panel, deferred("a", 0));
        assert_eq!(panel.claim_notices.keys().collect::<Vec<_>>(), vec![&a]);
        assert_eq!(panel.claiming, HashSet::from([b.clone()]));
        // B then succeeds: A's note stays on A's row.
        update(&mut panel, claimed("b", 1));
        assert_eq!(panel.claim_notices.keys().collect::<Vec<_>>(), vec![&a]);
        assert!(panel.claiming.is_empty());

        // B answers first, then A's deferral: the note is still A's.
        let mut panel = SparkReceive::new(None);
        panel.claiming.insert(a.clone());
        panel.claiming.insert(b.clone());
        update(&mut panel, claimed("b", 1));
        assert_eq!(panel.claiming, HashSet::from([a.clone()]));
        update(&mut panel, deferred("a", 0));
        assert_eq!(panel.claim_notices.keys().collect::<Vec<_>>(), vec![&a]);
        assert!(panel.claiming.is_empty());

        // B failing does not drop the note about A.
        update(
            &mut panel,
            SparkReceiveMessage::ClaimDepositFailed {
                txid: "b".to_string(),
                vout: 1,
                error: "claim failed".to_string(),
            },
        );
        assert_eq!(panel.claim_notices.keys().collect::<Vec<_>>(), vec![&a]);
        assert_eq!(panel.claim_error.as_deref(), Some("claim failed"));
    }

    #[test]
    fn balance_loaded_updates_only_when_value_is_present() {
        let mut panel = SparkReceive::new(None);

        update(
            &mut panel,
            SparkReceiveMessage::BalanceLoaded(Some((123, None))),
        );
        assert_eq!(panel.balance_sats, 123);

        update(&mut panel, SparkReceiveMessage::BalanceLoaded(None));
        assert_eq!(panel.balance_sats, 123);
    }

    /// The side panel lists bitcoin and USDB apart, so the panel keeps both
    /// as loaded rather than only their unified sum.
    #[test]
    fn balance_loaded_keeps_bitcoin_and_usdb_apart_for_the_side_panel() {
        let mut panel = SparkReceive::new(None);
        let usdb = UsdbBalanceSnapshot {
            token_identifier: "btkn1synthetic".into(),
            balance: 5_000_000,
            decimals: 6,
            ticker: "USDB".into(),
        };
        update(
            &mut panel,
            SparkReceiveMessage::BalanceLoaded(Some((123, Some(usdb.clone())))),
        );
        assert_eq!(panel.btc_balance_sats, 123);
        assert_eq!(panel.usdb_balance.map(|sb| sb.balance), Some(usdb.balance));
    }

    /// H-Receive: Bitcoin / Stablecoin (cross-chain) is a head action. With
    /// no backend the body has no controls, so the choice can only be there.
    #[test]
    fn the_receive_kind_choice_is_a_head_action() {
        let panel = SparkReceive::new(None);
        let cache = Cache {
            network: Network::Bitcoin,
            ..Cache::default()
        };
        let menu = Menu::Spark(crate::app::menu::SparkSubMenu::Receive);
        let probe = crate::app::view::shared::landing::capture::inspect(
            "spark-receive-head-kind",
            panel.view(&menu, &cache),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        assert!(probe.contains("Back to Spark"));
        assert!(probe.contains("Stablecoin (cross-chain)"));
        for id in [
            "spark-receive-kind-bitcoin",
            "spark-receive-kind-stablecoin",
        ] {
            assert!(
                probe.has_focus(iced::advanced::widget::Id::new(id)),
                "{} in the head",
                id
            );
        }
    }

    #[test]
    fn balance_loaded_folds_stable_balance_using_cache_price() {
        let mut panel = SparkReceive::new(None);
        let cache = Cache {
            btc_usd_price: Some(50_000.0),
            ..Cache::default()
        };

        update_with_cache(
            &mut panel,
            &cache,
            SparkReceiveMessage::BalanceLoaded(Some((123, Some(usdb_balance(2_500_000, 6))))),
        );

        assert_eq!(panel.balance_sats, 5_123);
    }

    #[test]
    fn payments_loaded_keeps_only_five_recent_rows() {
        let mut panel = SparkReceive::new(None);
        let payments: Vec<_> = (0..7)
            .map(|i| payment(&format!("payment-{i}"), 1_000 + i))
            .collect();

        update(&mut panel, SparkReceiveMessage::PaymentsLoaded(payments));

        assert_eq!(panel.recent_transactions.len(), 5);
        assert_eq!(panel.recent_transactions[0].id, "payment-0");
        assert_eq!(panel.recent_transactions[4].id, "payment-4");
        assert_eq!(panel.recent_transactions[4].amount.to_sat(), 1_004);
    }

    #[test]
    fn payments_failed_clears_recent_transactions() {
        let mut panel = SparkReceive::new(None);
        panel.recent_transactions.push(SparkRecentTransaction {
            id: "id".to_string(),
            description: "test payment".to_string(),
            time_ago: "just now".to_string(),
            timestamp: 1,
            amount: coincube_core::miniscript::bitcoin::Amount::from_sat(1),
            fees_sat: coincube_core::miniscript::bitcoin::Amount::from_sat(0),
            fiat_amount: None,
            is_incoming: true,
            status: crate::app::wallets::DomainPaymentStatus::Complete,
            method: crate::app::view::spark::SparkPaymentMethod::Spark,
            token_display: None,
        });

        update(
            &mut panel,
            SparkReceiveMessage::PaymentsFailed("bridge down".to_string()),
        );

        assert!(panel.recent_transactions.is_empty());
    }

    #[test]
    fn cross_network_selection_without_backend_or_support_is_noop() {
        let mut panel = SparkReceive::new(None);
        panel.sender_picker_open = true;
        let unsupported = Cache {
            network: Network::Regtest,
            ..Cache::default()
        };

        update_with_cache(
            &mut panel,
            &unsupported,
            SparkReceiveMessage::SelectSenderCrossNetwork("usdt:tron".to_string()),
        );

        assert!(!panel.sender_picker_open);
        assert!(panel.sideshift_flow.is_none());
        assert_eq!(panel.method, SparkReceiveMethod::Bolt11);

        update_with_cache(
            &mut panel,
            &Cache::default(),
            SparkReceiveMessage::SelectSenderCrossNetwork("usdt:tron".to_string()),
        );

        assert!(panel.sideshift_flow.is_none());
        assert_eq!(panel.method, SparkReceiveMethod::Bolt11);
    }
}
