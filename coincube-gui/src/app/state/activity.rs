//! Read-only history from the wallets in the currently open Cube.
//! Source records remain attached to each row so detail actions can hand off to
//! the owning wallet panel without guessing from a display string.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use coincube_spark_protocol::PaymentSummary;
use iced::Task;

use crate::{
    app::{
        cache::Cache,
        menu::Menu,
        message::Message,
        view::{self, activity::ActivityMessage},
        wallet::Wallet,
        wallets::{DomainPayment, DomainRefundableSwap, LiquidBackend, SparkBackend},
    },
    daemon::{
        model::{HistoryTransaction, SpendStatus, SpendTx},
        Daemon,
    },
    utils::now,
};

use super::State;

const HISTORY_LIMIT: u64 = 100;

#[derive(Debug, Clone)]
pub enum ActivityTarget {
    Vault(HistoryTransaction),
    Psbt(SpendTx),
    Spark(PaymentSummary),
    Liquid(DomainPayment),
    Refundable(DomainRefundableSwap),
}

pub struct Activity {
    liquid_backend: Option<Arc<LiquidBackend>>,
    spark_backend: Option<Arc<SparkBackend>>,
    vault: Vec<HistoryTransaction>,
    psbts: Vec<SpendTx>,
    spark: Vec<PaymentSummary>,
    liquid: Vec<DomainPayment>,
    refundables: Vec<DomainRefundableSwap>,
    pub filters: view::activity::Filters,
    pub selected_id: Option<String>,
    pub vault_loading: bool,
    pub psbt_loading: bool,
    pub spark_loading: bool,
    pub liquid_loading: bool,
    pub refundables_loading: bool,
    pub vault_error: Option<String>,
    pub psbt_error: Option<String>,
    pub spark_error: Option<String>,
    pub liquid_error: Option<String>,
    pub refundables_error: Option<String>,
    wallet: Option<Arc<Wallet>>,
    pub vault_has_more: bool,
    pub spark_has_more: bool,
    pub liquid_has_more: bool,
    vault_limit: u64,
    generation: u64,
}

impl Activity {
    pub fn new(
        liquid_backend: Option<Arc<LiquidBackend>>,
        spark_backend: Option<Arc<SparkBackend>>,
    ) -> Self {
        Self {
            liquid_backend,
            spark_backend,
            vault: Vec::new(),
            psbts: Vec::new(),
            spark: Vec::new(),
            liquid: Vec::new(),
            refundables: Vec::new(),
            filters: Default::default(),
            selected_id: None,
            vault_loading: false,
            psbt_loading: false,
            spark_loading: false,
            liquid_loading: false,
            refundables_loading: false,
            vault_error: None,
            psbt_error: None,
            spark_error: None,
            liquid_error: None,
            refundables_error: None,
            wallet: None,
            vault_has_more: false,
            spark_has_more: false,
            liquid_has_more: false,
            vault_limit: HISTORY_LIMIT,
            generation: 0,
        }
    }

    pub fn entries(&self) -> Vec<view::activity::Entry> {
        let mut entries =
            Vec::with_capacity(self.vault.len() + self.spark.len() + self.liquid.len());
        entries.extend(self.vault.iter().cloned().map(view::activity::Entry::vault));
        entries.extend(
            self.psbts
                .iter()
                .filter(|tx| tx.status == SpendStatus::Pending)
                .cloned()
                .map(view::activity::Entry::psbt),
        );
        entries.extend(self.spark.iter().cloned().map(view::activity::Entry::spark));
        entries.extend(
            self.liquid
                .iter()
                .cloned()
                .map(view::activity::Entry::liquid),
        );
        entries.extend(
            self.refundables
                .iter()
                .cloned()
                .map(view::activity::Entry::refundable),
        );
        entries.sort_by(|a, b| {
            let priority = |timestamp: u64| if timestamp == 0 { u64::MAX } else { timestamp };
            priority(b.timestamp)
                .cmp(&priority(a.timestamp))
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut duplicate_counts = HashMap::<String, usize>::new();
        for entry in &mut entries {
            let count = duplicate_counts.entry(entry.id.clone()).or_default();
            if *count > 0 {
                entry.id.push_str(&format!(":duplicate-{count}"));
            }
            *count += 1;
        }
        entries
    }

    /// Older rows are incomplete while any paginated source still has history.
    /// The last loaded timestamp from that source is an exclusive boundary:
    /// another page may contain rows with the same timestamp.
    pub fn complete_through(&self) -> Option<u64> {
        [
            (self.vault_has_more
                && matches!(
                    self.filters.wallet,
                    view::activity::WalletFilter::All | view::activity::WalletFilter::Vault
                ))
            .then(|| {
                self.vault
                    .iter()
                    .filter_map(|tx| tx.time.map(u64::from))
                    .min()
                    .unwrap_or(u64::MAX)
            }),
            (self.spark_has_more
                && matches!(
                    self.filters.wallet,
                    view::activity::WalletFilter::All | view::activity::WalletFilter::Spark
                ))
            .then(|| {
                self.spark
                    .iter()
                    .map(|row| row.timestamp)
                    .filter(|time| *time > 0)
                    .min()
                    .unwrap_or(u64::MAX)
            }),
            (self.liquid_has_more
                && matches!(
                    self.filters.wallet,
                    view::activity::WalletFilter::All | view::activity::WalletFilter::Liquid
                ))
            .then(|| {
                self.liquid
                    .iter()
                    .map(|row| row.timestamp as u64)
                    .filter(|time| *time > 0)
                    .min()
                    .unwrap_or(u64::MAX)
            }),
        ]
        .iter()
        .flatten()
        .max()
        .copied()
    }

    fn load_more(&mut self, daemon: Option<Arc<dyn Daemon + Sync + Send>>) -> Task<Message> {
        let generation = self.generation;
        let mut tasks = Vec::new();
        if self.vault_has_more && !self.vault_loading {
            if let Some(daemon) = daemon {
                self.vault_limit += HISTORY_LIMIT;
                let limit = self.vault_limit;
                let wallet = self.wallet.clone();
                self.vault_loading = true;
                tasks.push(Task::perform(
                    async move {
                        let upper = now().as_secs().min(u32::MAX as u64) as u32;
                        let mut rows = daemon
                            .list_history_txs(0, upper, limit + 1)
                            .await
                            .map_err(|e| e.to_string())?;
                        let more = rows.len() as u64 > limit;
                        rows.truncate(limit as usize);
                        let mut pending =
                            daemon.list_pending_txs().await.map_err(|e| e.to_string())?;
                        if let Some(wallet) = wallet {
                            let known: HashSet<_> = pending.iter().map(|tx| tx.txid).collect();
                            pending.extend(wallet.synthesized_pending_history_txs(&known));
                        }
                        let known: HashSet<_> = pending.iter().map(|tx| tx.txid).collect();
                        pending.extend(rows.into_iter().filter(|tx| !known.contains(&tx.txid)));
                        Ok::<_, String>((pending, more))
                    },
                    move |result| Message::ActivityVaultLoaded(generation, result),
                ));
            }
        }
        if self.spark_has_more && !self.spark_loading {
            if let Some(spark) = self.spark_backend.clone() {
                self.spark_loading = true;
                let offset = self.spark.len() as u32;
                tasks.push(Task::perform(
                    async move {
                        spark
                            .list_payments(Some(HISTORY_LIMIT as u32 + 1), Some(offset))
                            .await
                            .map(|page| {
                                let mut rows = page.payments;
                                let more = rows.len() as u64 > HISTORY_LIMIT;
                                rows.truncate(HISTORY_LIMIT as usize);
                                (rows, more)
                            })
                            .map_err(|e| e.to_string())
                    },
                    move |result| Message::ActivitySparkLoaded(generation, result),
                ));
            }
        }
        if self.liquid_has_more && !self.liquid_loading {
            if let Some(liquid) = self.liquid_backend.clone() {
                self.liquid_loading = true;
                let offset = self.liquid.len() as u32;
                tasks.push(Task::perform(
                    async move {
                        liquid
                            .list_payments(Some(HISTORY_LIMIT as u32 + 1), Some(offset), None)
                            .await
                            .map(|mut rows| {
                                let more = rows.len() as u64 > HISTORY_LIMIT;
                                rows.truncate(HISTORY_LIMIT as usize);
                                (rows, more)
                            })
                            .map_err(|e| e.to_string())
                    },
                    move |result| Message::ActivityLiquidLoaded(generation, result),
                ));
            }
        }
        Task::batch(tasks)
    }
}

impl State for Activity {
    fn view<'a>(
        &'a self,
        menu: &'a Menu,
        cache: &'a Cache,
    ) -> coincube_ui::widget::Element<'a, view::Message> {
        view::dashboard_activity(menu, cache, view::activity::render(self, cache))
    }

    fn update(
        &mut self,
        _daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        _cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        match message {
            Message::ActivityVaultLoaded(generation, result) if generation == self.generation => {
                self.vault_loading = false;
                match result {
                    Ok((rows, more)) => {
                        self.vault = rows;
                        self.vault_has_more = more;
                        self.vault_error = None;
                    }
                    Err(error) => self.vault_error = Some(error),
                }
            }
            Message::ActivityPsbtLoaded(generation, result) if generation == self.generation => {
                self.psbt_loading = false;
                match result {
                    Ok(rows) => {
                        self.psbts = rows;
                        self.psbt_error = None;
                    }
                    Err(error) => self.psbt_error = Some(error),
                }
            }
            Message::ActivitySparkLoaded(generation, result) if generation == self.generation => {
                self.spark_loading = false;
                match result {
                    Ok((rows, more)) => {
                        if self.spark.is_empty() {
                            self.spark = rows;
                        } else {
                            self.spark.extend(rows);
                        }
                        self.spark_has_more = more;
                        self.spark_error = None;
                    }
                    Err(error) => self.spark_error = Some(error),
                }
            }
            Message::ActivityLiquidLoaded(generation, result) if generation == self.generation => {
                self.liquid_loading = false;
                match result {
                    Ok((rows, more)) => {
                        if self.liquid.is_empty() {
                            self.liquid = rows;
                        } else {
                            self.liquid.extend(rows);
                        }
                        self.liquid_has_more = more;
                        self.liquid_error = None;
                    }
                    Err(error) => self.liquid_error = Some(error),
                }
            }
            Message::ActivityRefundablesLoaded(generation, result)
                if generation == self.generation =>
            {
                self.refundables_loading = false;
                match result {
                    Ok(rows) => {
                        self.refundables = rows;
                        self.refundables_error = None;
                    }
                    Err(error) => self.refundables_error = Some(error),
                }
            }
            Message::View(view::Message::Activity(event)) => match event {
                ActivityMessage::Wallet(value) => self.filters.wallet = value,
                ActivityMessage::Direction(value) => self.filters.direction = value,
                ActivityMessage::Status(value) => self.filters.status = value,
                ActivityMessage::DateRange(value) => self.filters.date_range = value,
                ActivityMessage::Search(value) => self.filters.search = value,
                ActivityMessage::Clear => self.filters = Default::default(),
                ActivityMessage::Select(id) => self.selected_id = Some(id),
                ActivityMessage::CloseDetail => {
                    let selected = self.selected_id.take();
                    return selected.map_or_else(Task::none, |id| {
                        iced::widget::operation::focus(view::activity::row_focus_id(&id))
                    });
                }
                ActivityMessage::Retry => return self.reload(_daemon, self.wallet.clone()),
                ActivityMessage::LoadMore => return self.load_more(_daemon),
                ActivityMessage::OpenDetail(target) => {
                    return Task::done(Message::ActivityOpenDetail(target))
                }
            },
            _ => {}
        }
        Task::none()
    }

    fn reload(
        &mut self,
        daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        wallet: Option<Arc<Wallet>>,
    ) -> Task<Message> {
        if wallet.is_some() {
            self.wallet = wallet.clone();
        }
        self.generation = self.generation.wrapping_add(1);
        self.vault_limit = HISTORY_LIMIT;
        self.vault_has_more = false;
        self.spark_has_more = false;
        self.liquid_has_more = false;
        self.vault.clear();
        self.psbts.clear();
        self.spark.clear();
        self.liquid.clear();
        self.refundables.clear();
        let generation = self.generation;
        self.vault_loading = daemon.is_some();
        self.psbt_loading = daemon.is_some()
            && wallet
                .as_ref()
                .is_some_and(|wallet| wallet.supports_inheritance());
        self.spark_loading = self.spark_backend.is_some();
        self.liquid_loading = self.liquid_backend.is_some();
        self.refundables_loading = self.liquid_backend.is_some();
        self.vault_error = None;
        self.psbt_error = None;
        self.spark_error = None;
        self.liquid_error = None;
        self.refundables_error = None;
        let mut tasks = Vec::new();
        if let Some(daemon) = daemon {
            if self.psbt_loading {
                let psbt_daemon = daemon.clone();
                tasks.push(Task::perform(
                    async move {
                        psbt_daemon
                            .list_spend_transactions(None)
                            .await
                            .map_err(|e| e.to_string())
                    },
                    move |result| Message::ActivityPsbtLoaded(generation, result),
                ));
            }
            tasks.push(Task::perform(
                async move {
                    let upper = now().as_secs().min(u32::MAX as u64) as u32;
                    let mut rows = daemon
                        .list_history_txs(0, upper, HISTORY_LIMIT + 1)
                        .await
                        .map_err(|e| e.to_string())?;
                    let more = rows.len() as u64 > HISTORY_LIMIT;
                    rows.truncate(HISTORY_LIMIT as usize);
                    let mut pending = daemon.list_pending_txs().await.map_err(|e| e.to_string())?;
                    if let Some(wallet) = wallet {
                        let known: HashSet<_> = pending.iter().map(|tx| tx.txid).collect();
                        pending.extend(wallet.synthesized_pending_history_txs(&known));
                    }
                    let known: HashSet<_> = pending.iter().map(|tx| tx.txid).collect();
                    pending.extend(rows.drain(..).filter(|tx| !known.contains(&tx.txid)));
                    Ok::<_, String>((pending, more))
                },
                move |result| Message::ActivityVaultLoaded(generation, result),
            ));
        } else {
            self.vault.clear();
        }
        if let Some(spark) = self.spark_backend.clone() {
            tasks.push(Task::perform(
                async move {
                    spark
                        .list_payments(Some(HISTORY_LIMIT as u32 + 1), Some(0))
                        .await
                        .map(|page| {
                            let mut rows = page.payments;
                            let more = rows.len() as u64 > HISTORY_LIMIT;
                            rows.truncate(HISTORY_LIMIT as usize);
                            (rows, more)
                        })
                        .map_err(|e| e.to_string())
                },
                move |result| Message::ActivitySparkLoaded(generation, result),
            ));
        } else {
            self.spark.clear();
        }
        if let Some(liquid) = self.liquid_backend.clone() {
            let refund_client = liquid.clone();
            tasks.push(Task::perform(
                async move {
                    refund_client
                        .list_refundables()
                        .await
                        .map_err(|e| e.to_string())
                },
                move |result| Message::ActivityRefundablesLoaded(generation, result),
            ));
            tasks.push(Task::perform(
                async move {
                    liquid
                        .list_payments(Some(HISTORY_LIMIT as u32 + 1), Some(0), None)
                        .await
                        .map(|mut rows| {
                            let more = rows.len() as u64 > HISTORY_LIMIT;
                            rows.truncate(HISTORY_LIMIT as usize);
                            (rows, more)
                        })
                        .map_err(|e| e.to_string())
                },
                move |result| Message::ActivityLiquidLoaded(generation, result),
            ));
        } else {
            self.liquid.clear();
        }
        Task::batch(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payment(id: &str, timestamp: u64) -> PaymentSummary {
        PaymentSummary {
            id: id.into(),
            amount_sat: 1,
            fees_sat: 0,
            token_amount: None,
            token_decimals: None,
            token_ticker: None,
            timestamp,
            status: "completed".into(),
            direction: "receive".into(),
            method: "lightning".into(),
            description: None,
        }
    }

    #[test]
    fn incomplete_source_hides_older_cross_wallet_days() {
        let mut activity = Activity::new(None, None);
        activity.spark = vec![payment("new", 300), payment("oldest-loaded", 200)];
        activity.spark_has_more = true;
        assert_eq!(activity.complete_through(), Some(200));
        assert!(activity
            .entries()
            .iter()
            .any(|entry| entry.timestamp == 300));
        assert!(!activity
            .entries()
            .iter()
            .filter(|entry| {
                activity
                    .complete_through()
                    .is_none_or(|cutoff| entry.timestamp > cutoff)
            })
            .any(|entry| entry.timestamp == 200));
        activity.spark.extend([payment("older", 100)]);
        activity.spark_has_more = false;
        assert_eq!(activity.complete_through(), None);
        activity.spark_has_more = true;
        activity.filters.wallet = view::activity::WalletFilter::Vault;
        assert_eq!(activity.complete_through(), None);
    }
}
