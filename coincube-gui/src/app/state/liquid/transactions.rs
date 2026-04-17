use std::collections::HashMap;
use std::convert::TryInto;
use std::sync::Arc;
use std::time::{Duration, Instant};

use breez_sdk_liquid::model::RefundRequest;
use coincube_core::miniscript::bitcoin::Amount;
use coincube_ui::component::form;
use coincube_ui::component::quote_display::{self, Quote};
use coincube_ui::widget::*;
use iced::{widget::image, Task};

use crate::app::breez_liquid::assets::usdt_asset_id;
use crate::app::view::FeeratePriority;
use crate::app::wallets::{
    DomainPayment, DomainPaymentDetails, DomainPaymentDirection, DomainRefundableSwap,
    LiquidBackend,
};
use crate::app::{cache::Cache, menu::Menu, state::State};
use crate::app::{message::Message, view, wallet::Wallet};
use crate::daemon::Daemon;
use crate::export::{ImportExportMessage, ImportExportState};
use crate::services::feeestimation::fee_estimation::FeeEstimator;

/// Grace period for in-flight refund entries before they are cleaned up.
const IN_FLIGHT_GRACE: Duration = Duration::from_secs(60);

/// Tracks a refund that has been submitted but may not yet be reflected
/// in the SDK's `list_refundables()` output.
#[derive(Debug, Clone)]
pub struct InFlightRefund {
    pub refund_txid: Option<String>,
    pub submitted_at: Instant,
}

#[derive(Debug)]
enum LiquidTransactionsModal {
    None,
    Export { state: ImportExportState },
}

pub struct LiquidTransactions {
    breez_client: Arc<LiquidBackend>,
    payments: Vec<DomainPayment>,
    refundables: Vec<DomainRefundableSwap>,
    selected_payment: Option<DomainPayment>,
    selected_refundable: Option<DomainRefundableSwap>,
    loading: bool,
    balance: Amount,
    modal: LiquidTransactionsModal,
    refund_address: form::Value<String>,
    refund_feerate: form::Value<String>,
    fee_estimator: FeeEstimator,
    refunding: bool,
    pub in_flight_refunds: HashMap<String, InFlightRefund>,
    asset_filter: AssetFilter,
    empty_state_quote: Quote,
    empty_state_image_handle: image::Handle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetFilter {
    All,
    LbtcOnly,
    UsdtOnly,
}

impl LiquidTransactions {
    pub fn new(breez_client: Arc<LiquidBackend>) -> Self {
        let empty_state_quote = quote_display::random_quote("empty-wallet");
        let empty_state_image_handle = quote_display::image_handle_for_context("empty-wallet");
        Self {
            breez_client,
            payments: Vec::new(),
            refundables: Vec::new(),
            selected_payment: None,
            selected_refundable: None,
            loading: false,
            balance: Amount::ZERO,
            modal: LiquidTransactionsModal::None,
            refund_address: form::Value::default(),
            refund_feerate: form::Value::default(),
            fee_estimator: FeeEstimator::new(),
            refunding: false,
            in_flight_refunds: HashMap::new(),
            asset_filter: AssetFilter::All,
            empty_state_quote,
            empty_state_image_handle,
        }
    }

    fn reconcile_in_flight(&mut self, mut refundables: Vec<DomainRefundableSwap>) {
        let returned: std::collections::HashSet<String> =
            refundables.iter().map(|r| r.swap_address.clone()).collect();
        let now = Instant::now();
        self.in_flight_refunds.retain(|addr, entry| {
            if returned.contains(addr) {
                return true;
            }
            // Swap is no longer listed by the SDK. Normally that means the
            // refund broadcast propagated and the swap can be dropped. But
            // an optimistic entry (refund_txid == None) that's still within
            // the grace window may simply be waiting for `RefundCompleted`
            // to land — don't erase it yet, or the "Refund broadcasting…"
            // banner would disappear before the user sees it.
            entry.refund_txid.is_none() && now.duration_since(entry.submitted_at) < IN_FLIGHT_GRACE
        });
        // Carry forward any locally-known RefundableSwap whose address still
        // has a grace-window in_flight entry but that the SDK dropped. The
        // view iterates `self.refundables` to render cards and only uses
        // `in_flight_refunds` for extra metadata, so without this the
        // "Refund broadcasting…" card would vanish the instant the SDK
        // stopped listing the swap, defeating the grace window.
        for prev in std::mem::take(&mut self.refundables) {
            if !returned.contains(&prev.swap_address)
                && self.in_flight_refunds.contains_key(&prev.swap_address)
            {
                refundables.push(prev);
            }
        }
        self.refundables = refundables;
    }

    #[cfg(test)]
    pub fn test_reconcile_in_flight(&mut self, refundables: Vec<DomainRefundableSwap>) {
        self.reconcile_in_flight(refundables);
    }

    pub fn asset_filter(&self) -> AssetFilter {
        self.asset_filter
    }

    pub fn preselect(&mut self, payment: DomainPayment) {
        self.selected_payment = Some(payment);
    }

    fn calculate_balance(&self) -> Amount {
        let usdt_id = usdt_asset_id(self.breez_client.network()).unwrap_or("");
        let mut balance: i64 = 0;

        for payment in &self.payments {
            let is_usdt = is_usdt_payment(&payment.details, usdt_id);

            match self.asset_filter {
                AssetFilter::UsdtOnly if !is_usdt => continue,
                AssetFilter::LbtcOnly if is_usdt => continue,
                AssetFilter::All => {
                    // For All mode, skip USDt from balance calc since
                    // USDt amount_sat is in asset base units, not sats
                    if is_usdt {
                        continue;
                    }
                }
                _ => {}
            }

            match payment.direction {
                DomainPaymentDirection::Receive => {
                    balance += payment.amount_sat as i64;
                }
                DomainPaymentDirection::Send => {
                    balance -= payment.amount_sat as i64;
                }
            }
        }

        Amount::from_sat(balance.max(0) as u64)
    }
}

/// `true` if the payment carries the configured USDt asset id.
fn is_usdt_payment(details: &DomainPaymentDetails, usdt_id: &str) -> bool {
    matches!(
        details,
        DomainPaymentDetails::LiquidAsset { asset_id, .. }
            if !usdt_id.is_empty() && asset_id == usdt_id
    )
}

impl State for LiquidTransactions {
    fn view<'a>(&'a self, menu: &'a Menu, cache: &'a Cache) -> Element<'a, view::Message> {
        let fiat_converter = cache.fiat_price.as_ref().and_then(|p| p.try_into().ok());
        let content = if let Some(payment) = &self.selected_payment {
            view::dashboard(
                menu,
                cache,
                view::liquid::transaction_detail_view(
                    payment,
                    fiat_converter,
                    cache.bitcoin_unit,
                    usdt_asset_id(self.breez_client.network()).unwrap_or(""),
                ),
            )
        } else if let Some(refundable) = &self.selected_refundable {
            view::dashboard(
                menu,
                cache,
                view::liquid::refundable_detail_view(
                    refundable,
                    fiat_converter,
                    cache.bitcoin_unit,
                    &self.refund_address,
                    &self.refund_feerate,
                    self.refunding,
                ),
            )
        } else {
            view::dashboard(
                menu,
                cache,
                view::liquid::liquid_transactions_view(
                    &self.payments,
                    &self.refundables,
                    &self.in_flight_refunds,
                    &self.balance,
                    fiat_converter,
                    self.loading,
                    cache.bitcoin_unit,
                    usdt_asset_id(self.breez_client.network()).unwrap_or(""),
                    self.asset_filter,
                    cache.show_direction_badges,
                    &self.empty_state_quote,
                    &self.empty_state_image_handle,
                ),
            )
        };

        match &self.modal {
            LiquidTransactionsModal::None => content,
            LiquidTransactionsModal::Export { state } => {
                use crate::app::view::Message as ViewMessage;
                use coincube_ui::component::text::*;
                use coincube_ui::widget::modal::Modal;

                let modal_content = match state {
                    ImportExportState::Ended => Column::new()
                        .spacing(20)
                        .push(text("Export successful!").size(20).bold())
                        .push(
                            coincube_ui::component::button::primary(None, "Close")
                                .width(150)
                                .on_press(ViewMessage::ImportExport(ImportExportMessage::Close)),
                        ),
                    _ => Column::new()
                        .spacing(20)
                        .push(text("Exporting payments...").size(20).bold()),
                };

                Modal::new(content, modal_content)
                    .on_blur(Some(ViewMessage::ImportExport(ImportExportMessage::Close)))
                    .into()
            }
        }
    }

    fn update(
        &mut self,
        _daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        _cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        match message {
            Message::PaymentsLoaded(Ok(payments)) => {
                self.loading = false;
                let usdt_id = usdt_asset_id(self.breez_client.network()).unwrap_or("");
                self.payments = match self.asset_filter {
                    AssetFilter::UsdtOnly => payments
                        .into_iter()
                        .filter(|p| is_usdt_payment(&p.details, usdt_id))
                        .collect(),
                    AssetFilter::LbtcOnly => payments
                        .into_iter()
                        .filter(|p| !is_usdt_payment(&p.details, usdt_id))
                        .collect(),
                    AssetFilter::All => payments,
                };
                self.balance = self.calculate_balance();
                Task::none()
            }
            Message::PaymentsLoaded(Err(e)) => {
                self.loading = false;
                Task::done(Message::View(view::Message::ShowError(e.to_string())))
            }
            Message::RefundablesLoaded(Ok(refundables)) => {
                self.refundables = refundables;
                Task::none()
            }
            Message::RefundablesLoaded(Err(e)) => {
                Task::done(Message::View(view::Message::ShowError(e.to_string())))
            }
            Message::View(view::Message::Select(i)) => {
                self.selected_payment = self.payments.get(i).cloned();
                self.selected_refundable = None;
                Task::none()
            }
            Message::View(view::Message::SelectRefundable(i)) => {
                self.selected_refundable = self.refundables.get(i).cloned();
                self.selected_payment = None;
                self.refund_address = form::Value::default();
                self.refund_feerate = form::Value::default();
                Task::none()
            }
            Message::View(view::Message::Reload) => self.reload(None, None),
            Message::View(view::Message::Close) => {
                self.selected_payment = None;
                self.selected_refundable = None;
                self.modal = LiquidTransactionsModal::None;
                self.refund_address = form::Value::default();
                self.refund_feerate = form::Value::default();
                Task::none()
            }
            Message::View(view::Message::PreselectPayment(payment)) => {
                self.selected_payment = Some(payment);
                Task::none()
            }
            Message::View(view::Message::SetAssetFilter(filter)) => {
                if self.asset_filter != filter {
                    self.asset_filter = filter;
                    // Reload with the new filter
                    return self.reload(None, None);
                }
                Task::none()
            }
            Message::View(view::Message::ImportExport(ImportExportMessage::Open)) => {
                if matches!(self.modal, LiquidTransactionsModal::None) {
                    Task::perform(
                        crate::export::get_path(
                            format!(
                                "coincube-liquid-txs-{}.csv",
                                chrono::Local::now().format("%Y-%m-%dT%H-%M-%S")
                            ),
                            true,
                        ),
                        |path| {
                            Message::View(view::Message::ImportExport(ImportExportMessage::Path(
                                path,
                            )))
                        },
                    )
                } else {
                    Task::none()
                }
            }
            Message::View(view::Message::ImportExport(ImportExportMessage::Path(Some(path)))) => {
                self.modal = LiquidTransactionsModal::Export {
                    state: ImportExportState::Started,
                };
                let breez_client = self.breez_client.client().clone();
                Task::perform(
                    async move {
                        crate::export::export_liquid_payments(
                            &tokio::sync::mpsc::unbounded_channel().0,
                            breez_client,
                            path,
                        )
                        .await
                    },
                    |result| {
                        Message::View(view::Message::ImportExport(ImportExportMessage::Progress(
                            match result {
                                Ok(_) => crate::export::Progress::Ended,
                                Err(e) => crate::export::Progress::Error(e),
                            },
                        )))
                    },
                )
            }
            Message::View(view::Message::ImportExport(ImportExportMessage::Path(None))) => {
                self.modal = LiquidTransactionsModal::None;
                Task::none()
            }
            Message::View(view::Message::ImportExport(ImportExportMessage::Progress(
                crate::export::Progress::Ended,
            ))) => {
                self.modal = LiquidTransactionsModal::Export {
                    state: ImportExportState::Ended,
                };
                Task::none()
            }
            Message::View(view::Message::ImportExport(ImportExportMessage::Progress(
                crate::export::Progress::Error(e),
            ))) => {
                self.modal = LiquidTransactionsModal::None;
                Task::done(Message::View(view::Message::ShowError(e.to_string())))
            }
            Message::View(view::Message::ImportExport(ImportExportMessage::Close)) => {
                self.modal = LiquidTransactionsModal::None;
                Task::none()
            }
            Message::View(view::Message::RefundAddressEdited(address)) => {
                self.refund_address.value = address;
                let breez_client = self.breez_client.clone();
                let addr = self.refund_address.value.clone();
                Task::perform(
                    async move {
                        let result = breez_client.validate_input(addr).await;
                        result
                    },
                    |input_type| {
                        Message::View(view::Message::RefundAddressValidated(matches!(
                            input_type,
                            Some(breez_sdk_liquid::InputType::BitcoinAddress { .. })
                        )))
                    },
                )
            }
            Message::View(view::Message::RefundAddressValidated(is_valid)) => {
                self.refund_address.valid = is_valid;
                if !is_valid && !self.refund_address.value.is_empty() {
                    self.refund_address.warning = Some("Invalid Bitcoin address");
                } else {
                    self.refund_address.warning = None;
                }
                Task::none()
            }
            Message::View(view::Message::RefundFeerateEdited(feerate)) => {
                self.refund_feerate.value = feerate;
                self.refund_feerate.valid = true;
                self.refund_feerate.warning = None;
                Task::none()
            }
            Message::View(view::Message::RefundFeeratePrioritySelected(priority)) => {
                let fee_estimator = self.fee_estimator.clone();
                Task::perform(
                    async move {
                        let rate: Option<usize> = match priority {
                            FeeratePriority::Low => {
                                let result = fee_estimator.get_low_priority_rate().await;
                                result.ok()
                            }
                            FeeratePriority::Medium => {
                                let result = fee_estimator.get_mid_priority_rate().await;
                                result.ok()
                            }
                            FeeratePriority::High => {
                                let result = fee_estimator.get_high_priority_rate().await;
                                result.ok()
                            }
                        };
                        rate
                    },
                    move |rate: Option<usize>| {
                        if let Some(rate) = rate {
                            Message::View(view::Message::RefundFeerateEdited(rate.to_string()))
                        } else {
                            Message::View(view::Message::ShowError(
                                "Failed to fetch fee rate".to_string(),
                            ))
                        }
                    },
                )
            }
            Message::View(view::Message::SubmitRefund) => {
                if let Some(refundable) = &self.selected_refundable {
                    self.refunding = true;
                    let breez_client = self.breez_client.clone();
                    let swap_address = refundable.swap_address.clone();
                    let refund_address = self.refund_address.value.clone();
                    let fee_rate = self.refund_feerate.value.parse::<u32>().unwrap_or(1);

                    let swap_address_for_msg = swap_address.clone();
                    Task::perform(
                        async move {
                            let result = breez_client
                                .refund_onchain_tx(RefundRequest {
                                    swap_address: swap_address.clone(),
                                    refund_address: refund_address.clone(),
                                    fee_rate_sat_per_vbyte: fee_rate,
                                })
                                .await;
                            result
                        },
                        move |result| Message::RefundCompleted {
                            swap_address: swap_address_for_msg.clone(),
                            result,
                        },
                    )
                } else {
                    log::error!(target: "refund_debug", "SubmitRefund called but no refundable selected");
                    Task::none()
                }
            }
            Message::RefundCompleted { result: Ok(_response), .. } => {
                self.refunding = false;
                self.selected_refundable = None;
                self.refund_address = form::Value::default();
                self.refund_feerate = form::Value::default();
                Task::done(Message::View(view::Message::Close))
            }
            Message::RefundCompleted { result: Err(e), .. } => {
                self.refunding = false;
                Task::done(Message::View(view::Message::ShowError(format!(
                    "Refund failed: {}",
                    e
                ))))
            }
            _ => Task::none(),
        }
    }

    fn reload(
        &mut self,
        _daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        _wallet: Option<Arc<Wallet>>,
    ) -> Task<Message> {
        self.loading = true;
        self.selected_payment = None;
        self.selected_refundable = None;
        let client = self.breez_client.clone();
        let client2 = self.breez_client.clone();

        Task::batch(vec![
            Task::perform(
                async move { client.list_payments(None).await },
                Message::PaymentsLoaded,
            ),
            Task::perform(
                async move { client2.list_refundables().await },
                Message::RefundablesLoaded,
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::breez_liquid::BreezClient;
    use breez_sdk_liquid::bitcoin::Network;

    fn sample_refundable(addr: &str) -> DomainRefundableSwap {
        DomainRefundableSwap {
            swap_address: addr.to_string(),
            timestamp: 0,
            amount_sat: 24_869,
        }
    }

    fn new_state() -> LiquidTransactions {
        let client = Arc::new(BreezClient::disconnected(Network::Bitcoin));
        LiquidTransactions::new(Arc::new(LiquidBackend::new(client)))
    }

    #[test]
    fn in_flight_dropped_when_sdk_no_longer_returns_it() {
        let mut state = new_state();
        state.in_flight_refunds.insert(
            "bc1q_gone".to_string(),
            InFlightRefund {
                refund_txid: Some("deadbeef".to_string()),
                submitted_at: Instant::now(),
            },
        );
        state.in_flight_refunds.insert(
            "bc1q_still".to_string(),
            InFlightRefund {
                refund_txid: None,
                submitted_at: Instant::now(),
            },
        );

        // After reconcile: only swaps still returned by the SDK survive.
        state.test_reconcile_in_flight(vec![sample_refundable("bc1q_still")]);

        assert!(state.in_flight_refunds.contains_key("bc1q_still"));
        assert!(!state.in_flight_refunds.contains_key("bc1q_gone"));
        assert_eq!(state.refundables.len(), 1);
    }

    #[test]
    fn in_flight_preserved_while_sdk_still_returns_swap() {
        let mut state = new_state();
        state.in_flight_refunds.insert(
            "bc1q_active".to_string(),
            InFlightRefund {
                refund_txid: None,
                submitted_at: Instant::now(),
            },
        );
        state.test_reconcile_in_flight(vec![sample_refundable("bc1q_active")]);
        assert!(state.in_flight_refunds.contains_key("bc1q_active"));
    }

    #[test]
    fn in_flight_card_carried_forward_when_sdk_drops_optimistic_swap() {
        // Regression: grace window preserves the in_flight entry *and* the
        // RefundableSwap, so the view (which iterates self.refundables) keeps
        // rendering the "Refund broadcasting…" card until RefundCompleted.
        let mut state = new_state();
        state.refundables = vec![sample_refundable("bc1q_racing")];
        state.in_flight_refunds.insert(
            "bc1q_racing".to_string(),
            InFlightRefund {
                refund_txid: None,
                submitted_at: Instant::now(),
            },
        );

        // SDK poll races ahead of RefundCompleted and no longer lists the swap.
        state.test_reconcile_in_flight(vec![]);

        assert!(state.in_flight_refunds.contains_key("bc1q_racing"));
        assert_eq!(state.refundables.len(), 1);
        assert_eq!(state.refundables[0].swap_address, "bc1q_racing");
    }

    #[test]
    fn in_flight_card_dropped_once_entry_removed() {
        // Carry-forward is tied to in_flight presence: once the entry is
        // dropped (e.g. txid set + absent from SDK list), the refundable
        // must also disappear.
        let mut state = new_state();
        state.refundables = vec![sample_refundable("bc1q_done")];
        state.in_flight_refunds.insert(
            "bc1q_done".to_string(),
            InFlightRefund {
                refund_txid: Some("deadbeef".to_string()),
                submitted_at: Instant::now(),
            },
        );

        state.test_reconcile_in_flight(vec![]);

        assert!(!state.in_flight_refunds.contains_key("bc1q_done"));
        assert!(state.refundables.is_empty());
    }
}
