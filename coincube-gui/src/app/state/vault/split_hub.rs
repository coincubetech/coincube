//! Tools › Split coins: the whole split flow, one step at a time (Tenshu
//! canvas, `splitflow` page). The step is derived from the Vault each time
//! the tab is drawn: get a split coin, review and sign, confirmations, then
//! done or the coins still left. Only the user's choices (coins left out, the
//! method), the address to send a split coin to, the fee rate, the build in
//! flight and why the last build failed are kept here. Waiting for a split coin can take an hour, so the
//! flow lives in the tab rather than a dialog: you can leave and come back.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use coincube_core::miniscript::bitcoin::{OutPoint, Txid};
use coincube_ui::widget::Element;
use coincubed::commands::CoinStatus;
use iced::{widget::qr_code, Task};

use crate::{
    app::{
        cache::Cache,
        error::Error,
        menu::Menu,
        message::Message,
        state::State,
        view::{
            self,
            vault::split_coins::{CopyCard, KeyStatus, ReviewCoin, SplitFlow, Step, MIN_KEY_SATS},
            Message as ViewMessage, SplitMessage,
        },
        wallet::Wallet,
    },
    daemon::{
        model::{Coin, LabelItem},
        Daemon,
    },
    services::{
        chain_status::{ChainStatus, SPLIT_CONFIRMATIONS},
        split_coins::{self, Method},
    },
};

/// How far back, in blocks, a confirmed coin still being checked counts as a
/// split coin that just arrived: about a day.
const RECENT_BLOCKS: i32 = 144;

/// Unavailable fee rates retried on their own before the review offers
/// **Try again**: one failed Connect read shouldn't leave the user stuck.
const FEE_AUTO_RETRIES: u8 = 3;

/// The wait before each automatic fee-rate retry.
const FEE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

/// The facts the step is derived from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowFacts {
    /// Confirmed unspent coins still on both chains.
    pub both: Vec<OutPoint>,
    /// Unspent coins with no answer yet: not checked, still checking, a check
    /// that failed, or a signature split nothing has confirmed. Only positive
    /// evidence makes the flow done.
    pub unverified: usize,
    /// A split coin ready to spend.
    pub key: bool,
    pub key_status: KeyStatus,
    pub signature_available: bool,
    /// The fewest confirmations among this Vault's sent splits still short of
    /// [`SPLIT_CONFIRMATIONS`].
    pub sending: Option<u32>,
    /// A split built and stored that nothing has sent yet.
    pub unsigned: Option<Txid>,
    /// The both-chain coins unsent splits already spend. They wait for that
    /// split (or its record to expire); the other coins can still be split.
    pub reserved: Vec<OutPoint>,
    /// Coins this Vault's confirmed splits separated.
    pub split_before: usize,
}

/// The step the flow is on. `continuing` is the user's Split the rest.
pub fn step(facts: &FlowFacts, method: Method, continuing: bool) -> Step {
    if let Some(confirmations) = facts.sending {
        return Step::Sent { confirmations };
    }
    let available = facts
        .both
        .iter()
        .filter(|outpoint| !facts.reserved.contains(outpoint))
        .count();
    if let Some(txid) = facts.unsigned.filter(|_| available == 0) {
        return Step::Unsigned { txid };
    }
    if facts.both.is_empty() {
        return if facts.unverified > 0 {
            Step::Verifying {
                count: facts.unverified,
            }
        } else {
            Step::Done
        };
    }
    if facts.split_before > 0 && !continuing {
        return Step::Rest {
            split: facts.split_before,
        };
    }
    if method == Method::SplitKey && !facts.key {
        return Step::Key(facts.key_status);
    }
    Step::Review
}

/// Where a split coin stands while the flow waits for one. `address` is the
/// address the flow shows; a coin received there is the one being waited
/// for. Without it (after a restart), an unconfirmed incoming coin, or a
/// recent confirmed one still being checked, stands in.
pub fn key_status(cache: &Cache, address: Option<&str>) -> KeyStatus {
    key_watch(cache, address).map_or(KeyStatus::Waiting, |(_, status)| status)
}

/// The coin step one reports on, with where it stands. `None` while nothing
/// has arrived.
fn key_watch(cache: &Cache, address: Option<&str>) -> Option<(OutPoint, KeyStatus)> {
    let split_txids: HashSet<Txid> = cache
        .split_records
        .iter()
        .map(|record| record.txid)
        .collect();
    let candidates: Vec<&Coin> = cache
        .coins()
        .iter()
        .filter(|coin| coin.spend_info.is_none() && !coin.is_change && !coin.is_from_self)
        .filter(|coin| !split_txids.contains(&coin.outpoint.txid))
        .collect();
    let tip = cache.blockheight();
    let classify = |coin: &Coin| -> KeyStatus {
        let amount = coin.amount;
        let Some(height) = coin.block_height else {
            return KeyStatus::Confirming { amount };
        };
        let confirmations = (tip - height + 1).max(1) as u32;
        match cache.chain_status(&coin.outpoint) {
            ChainStatus::BothChains => KeyStatus::BothChains { amount },
            ChainStatus::CouldntVerify => KeyStatus::CouldntVerify { amount },
            // Only a coin with no answer yet: one a split is spending, or one
            // already shown as on this chain only, isn't waiting on a retry.
            ChainStatus::NotSeen | ChainStatus::NotChecked
                if cache
                    .chain_proofs
                    .get(&coin.outpoint)
                    .is_some_and(|entry| entry.is_waiting_to_retry()) =>
            {
                KeyStatus::Retrying {
                    amount,
                    stalled: cache
                        .chain_proofs
                        .get(&coin.outpoint)
                        .is_some_and(|entry| entry.retries_backed_off()),
                }
            }
            _ => KeyStatus::Checking {
                amount,
                confirmations,
            },
        }
    };
    let at_address = |coin: &&&Coin| address.is_some_and(|a| coin.address.to_string() == a);
    let incoming = |coin: &&&Coin| {
        coin.amount.to_sat() >= MIN_KEY_SATS
            && (coin.block_height.is_none()
                || (coin
                    .block_height
                    .is_some_and(|height| tip - height < RECENT_BLOCKS)
                    && matches!(
                        cache.chain_status(&coin.outpoint),
                        ChainStatus::NotChecked | ChainStatus::NotSeen
                    )))
    };
    let pending = |status: &KeyStatus| {
        matches!(
            status,
            KeyStatus::Confirming { .. } | KeyStatus::Checking { .. } | KeyStatus::Retrying { .. }
        )
    };
    // A coin still on its way (at the address, or any new incoming one)
    // wins over one that failed: after "send one from another source" the
    // replacement must show, not the coin that couldn't be a split coin.
    let statuses: Vec<(OutPoint, KeyStatus)> = candidates
        .iter()
        .filter(|coin| at_address(coin) || incoming(coin))
        .map(|coin| (coin.outpoint, classify(coin)))
        .collect();
    if let Some(watch) = statuses.iter().find(|(_, status)| pending(status)) {
        return Some(*watch);
    }
    candidates
        .iter()
        .filter(at_address)
        .map(|coin| (coin.outpoint, classify(coin)))
        .next()
}

/// Tools › Split coins.
pub struct SplitHubPanel {
    wallet: Arc<Wallet>,
    labels: HashMap<String, String>,
    /// Both-chain coins the user left out.
    excluded: BTreeSet<OutPoint>,
    /// The method the user chose; `None` takes the default.
    method: Option<Method>,
    /// The user chose Split the rest after an earlier split.
    continuing: bool,
    feerate: Option<u64>,
    fee_failed: bool,
    /// Automatic retries spent since the last fetch the user or a visit
    /// started.
    fee_retries: u8,
    /// Bumped by each visit and each Try again. A fee answer, or a pending
    /// automatic retry, from an older fetch is ignored, so it can't clear a
    /// newer rate or stack another lookup.
    fee_generation: u64,
    address: Option<String>,
    qr: Option<qr_code::Data>,
    address_error: Option<String>,
    working: bool,
    /// Where the build in flight is, once it says.
    stage: Option<split_coins::BuildStage>,
    error: Option<BuildError>,
}

/// Why the last build failed, with what the flow showed when it landed. The
/// message is about that method and that step, so it goes once the method
/// changes or the flow moves to another step. One about a rejected split coin
/// also goes once another split coin is offered. On step one, either goes
/// once the step reports on another coin; the same coin gaining
/// confirmations or an answer doesn't count. The App drops it after the
/// message that made it stale, whichever panel is showing, so it can't come
/// back. Only the review and step one draw it, so a failure that lands on
/// any other step isn't kept.
#[derive(Debug, Clone)]
struct BuildError {
    message: String,
    method: Method,
    /// The split coin on offer. A rejected one has already left the offer,
    /// so this is the coin offered next, or none on step one. Only a
    /// rejection is held to it.
    key: Option<OutPoint>,
    /// The step the flow was on: the review or step one.
    step: Step,
    /// The coin step one was reporting on.
    watched: Option<OutPoint>,
    /// The build rejected its split coin.
    rejected: bool,
}

/// The split coin a split would spend now.
fn offered_key(cache: &Cache) -> Option<OutPoint> {
    cache.split_keys().first().map(|(coin, _)| coin.outpoint)
}

impl SplitHubPanel {
    pub fn new(wallet: Arc<Wallet>) -> Self {
        Self {
            wallet,
            labels: HashMap::new(),
            excluded: BTreeSet::new(),
            method: None,
            continuing: false,
            feerate: None,
            fee_failed: false,
            fee_retries: 0,
            fee_generation: 0,
            address: None,
            qr: None,
            address_error: None,
            working: false,
            stage: None,
            error: None,
        }
    }

    fn signature_available(&self) -> bool {
        split_coins::signature_available(&self.wallet)
    }

    fn method(&self) -> Method {
        match self.method {
            Some(Method::Signature) | None if self.signature_available() => Method::Signature,
            _ => Method::SplitKey,
        }
    }

    pub fn facts(&self, cache: &Cache) -> FlowFacts {
        let both = cache
            .coins()
            .iter()
            .filter(|coin| coin.spend_info.is_none() && coin.block_height.is_some())
            .filter(|coin| cache.chain_status(&coin.outpoint).needs_split())
            .map(|coin| coin.outpoint)
            .collect();
        let unverified = cache
            .coins()
            .iter()
            .filter(|coin| coin.spend_info.is_none())
            .filter(|coin| {
                matches!(
                    cache.chain_status(&coin.outpoint),
                    ChainStatus::NotChecked
                        | ChainStatus::NotSeen
                        | ChainStatus::CouldntVerify
                        | ChainStatus::SplitUnconfirmed
                )
            })
            .count();
        let sent: HashSet<Txid> = cache
            .coins()
            .iter()
            .map(|coin| coin.outpoint.txid)
            .collect();
        let unsent: Vec<_> = cache
            .split_records
            .iter()
            .filter(|record| record.confirmations == 0 && !sent.contains(&record.txid))
            .collect();
        let unsigned = unsent
            .iter()
            .max_by_key(|record| record.created_at)
            .map(|record| record.txid);
        let reserved = unsent
            .iter()
            .flat_map(|record| record.inputs.iter().copied())
            .collect();
        let split_before = cache
            .split_records
            .iter()
            .filter(|record| record.confirmations >= SPLIT_CONFIRMATIONS)
            .map(|record| record.inputs.len())
            .sum();
        FlowFacts {
            both,
            unverified,
            key: !cache.split_keys().is_empty(),
            key_status: key_status(cache, self.address.as_deref()),
            signature_available: self.signature_available(),
            sending: cache.splitting.values().copied().min(),
            unsigned,
            reserved,
            split_before,
        }
    }

    /// The flow as drawn now.
    pub fn flow(&self, cache: &Cache) -> SplitFlow {
        let facts = self.facts(cache);
        let method = self.method();
        let label = |coin: &Coin| {
            self.labels
                .get(&coin.outpoint.to_string())
                .or_else(|| self.labels.get(&coin.outpoint.txid.to_string()))
                .filter(|label| !label.is_empty())
                .cloned()
                .unwrap_or_else(|| coincube_ui::locale::copy("No label").to_string())
        };
        let coins: Vec<ReviewCoin> = cache
            .coins()
            .iter()
            .filter(|coin| {
                facts.both.contains(&coin.outpoint) && !facts.reserved.contains(&coin.outpoint)
            })
            .map(|coin| ReviewCoin {
                outpoint: coin.outpoint,
                amount: coin.amount,
                label: label(coin),
                included: !self.excluded.contains(&coin.outpoint),
            })
            .collect();
        let included = coins.iter().filter(|coin| coin.included).count();
        // The estimator counts one input beyond the shared ones: the split
        // coin, or for a signature split an overestimate by one input. A
        // Vault and an imported wallet are sized from their own policy.
        let vbytes =
            split_coins::estimate_split_vbytes(&self.wallet.main_descriptor, included.max(1));
        let copy = if self.wallet.chain.is_blake2b() {
            CopyCard::ThisIsTheCopy
        } else if cache.btcb2_already_claimed {
            CopyCard::Exists
        } else if self.wallet.supports_inheritance() || self.wallet.is_imported() {
            CopyCard::Create
        } else {
            CopyCard::None
        };
        let step = step(&facts, method, self.continuing);
        let waiting_to_sign = facts
            .unsigned
            .filter(|_| !matches!(step, Step::Unsigned { .. } | Step::Sent { .. }));
        SplitFlow {
            here: self.wallet.chain,
            step,
            coins,
            key: cache.split_keys().first().map(|(coin, _)| coin.amount),
            key_verified: cache
                .split_keys()
                .first()
                .is_some_and(|(coin, _)| cache.split_key_verified(&coin.outpoint)),
            method,
            signature_available: facts.signature_available,
            address: self.address.clone(),
            address_error: self.address_error.clone(),
            feerate: self.feerate,
            fee_failed: self.fee_failed,
            vbytes,
            working: self.working,
            stage: self.stage.filter(|_| self.working),
            error: self
                .error_shown(cache, step)
                .map(|error| error.message.clone()),
            copy,
            waiting_to_sign,
            mask_amounts: cache.balance_amounts_masked(),
            unit: cache.bitcoin_unit,
        }
    }

    fn update_split_view(&mut self, cache: &Cache, message: SplitMessage) -> Task<Message> {
        match message {
            SplitMessage::ToggleCoin(outpoint) => {
                if !self.working && !self.excluded.remove(&outpoint) {
                    self.excluded.insert(outpoint);
                }
            }
            SplitMessage::Method(method) => {
                if !self.working && (method == Method::SplitKey || self.signature_available()) {
                    if method != self.method() {
                        self.error = None;
                    }
                    self.method = Some(method);
                }
            }
            SplitMessage::Sign => return self.sign(cache),
            SplitMessage::SplitRest => {
                self.continuing = true;
                self.excluded.clear();
            }
            SplitMessage::RetryFee => {
                if !self.working && self.fee_failed {
                    return Task::done(fetch_fee(self.start_fee_fetch()));
                }
            }
        }
        Task::none()
    }

    fn sign(&mut self, cache: &Cache) -> Task<Message> {
        let flow = self.flow(cache);
        if flow.step != Step::Review {
            return Task::none();
        }
        let Some(feerate_vb) = flow.feerate.filter(|_| flow.blocked().is_none()) else {
            return Task::none();
        };
        let coins = flow
            .coins
            .iter()
            .filter(|coin| coin.included)
            .map(|coin| coin.outpoint)
            .collect();
        let key = (flow.method == Method::SplitKey)
            .then(|| cache.split_keys().first().map(|(coin, _)| coin.outpoint))
            .flatten();
        self.working = true;
        self.stage = Some(split_coins::BuildStage::first(flow.method));
        self.error = None;
        Task::done(Message::SplitRequest {
            method: flow.method,
            coins,
            key,
            feerate_vb,
        })
    }

    fn set_address(&mut self, result: Result<String, String>) {
        match result {
            Ok(address) => {
                self.qr = qr_code::Data::new(format!("bitcoin:{}", address)).ok();
                self.address = Some(address);
                self.address_error = None;
            }
            Err(e) => self.address_error = Some(e),
        }
    }

    fn set_result(&mut self, cache: &Cache, result: Result<Box<split_coins::SplitRecord>, String>) {
        self.working = false;
        self.stage = None;
        match result {
            Ok(_) => {
                self.excluded.clear();
                self.continuing = false;
                self.error = None;
            }
            Err(message) => {
                let step = step(&self.facts(cache), self.method(), self.continuing);
                // No other step draws it: the Vault moved on during the build.
                self.error = matches!(step, Step::Review | Step::Key(_)).then(|| BuildError {
                    message,
                    method: self.method(),
                    key: offered_key(cache),
                    step,
                    watched: self.watched(cache),
                    rejected: false,
                });
            }
        }
    }

    /// The failure just reported was the build rejecting its split coin.
    pub(crate) fn split_key_was_rejected(&mut self) {
        if let Some(error) = self.error.as_mut() {
            error.rejected = true;
        }
    }

    /// The coin step one reports on.
    fn watched(&self, cache: &Cache) -> Option<OutPoint> {
        key_watch(cache, self.address.as_deref()).map(|(coin, _)| coin)
    }

    /// The build error, while it is still about what the flow shows.
    fn error_shown(&self, cache: &Cache, step: Step) -> Option<&BuildError> {
        let error = self.error.as_ref()?;
        if self.method() != error.method {
            return None;
        }
        if std::mem::discriminant(&step) != std::mem::discriminant(&error.step) {
            return None;
        }
        // On step one, only until it reports on another coin: a replacement
        // on its way isn't the coin the message is about.
        if matches!(step, Step::Key(_)) && self.watched(cache) != error.watched {
            return None;
        }
        // A failure that isn't about the split coin doesn't go because the
        // offer moved to another one.
        if error.rejected && offered_key(cache) != error.key {
            return None;
        }
        Some(error)
    }

    /// Forget a build error that is no longer about what the flow shows.
    /// The App calls this after every message it handles, whichever panel
    /// is showing, so a message that went stale can't come back when the
    /// flow happens to look the same again later. Free while no error is
    /// held; with one, it works the step out once more.
    pub(crate) fn drop_stale_error(&mut self, cache: &Cache) {
        if self.error.is_none() {
            return;
        }
        let step = step(&self.facts(cache), self.method(), self.continuing);
        if self.error_shown(cache, step).is_none() {
            self.error = None;
        }
    }

    /// A build in flight, for the App's routing test.
    #[cfg(test)]
    pub(crate) fn start_building_split_for_test(&mut self) {
        self.working = true;
    }

    /// The stage the review shows, for the App's routing test.
    #[cfg(test)]
    pub(crate) fn build_stage_for_test(&self) -> Option<split_coins::BuildStage> {
        self.stage.filter(|_| self.working)
    }

    /// `(working, error)`, for the App's routing test.
    #[cfg(test)]
    pub(crate) fn split_progress_for_test(&self) -> (bool, Option<String>) {
        (
            self.working,
            self.error.as_ref().map(|error| error.message.clone()),
        )
    }
}

impl State for SplitHubPanel {
    fn view<'a>(&'a self, menu: &'a Menu, cache: &'a Cache) -> Element<'a, ViewMessage> {
        view::dashboard(
            menu,
            cache,
            view::vault::split_coins::split_flow_view(&self.flow(cache), self.qr.as_ref()),
        )
    }

    fn update(
        &mut self,
        _daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        cache: &Cache,
        message: Message,
    ) -> Task<Message> {
        match message {
            Message::View(ViewMessage::Split(message)) => {
                return self.update_split_view(cache, message);
            }
            Message::SplitFeeRate { generation, rate } => {
                return self.set_feerate(generation, rate)
            }
            Message::SplitAddress(result) => self.set_address(result),
            Message::SplitPrepared(result) => self.set_result(cache, result),
            Message::SplitBuildStage(stage) => {
                if self.working {
                    self.stage = Some(stage);
                }
            }
            Message::Labels(Ok(labels)) => self.labels = labels,
            _ => {}
        }
        Task::none()
    }

    fn reload(
        &mut self,
        daemon: Option<Arc<dyn Daemon + Sync + Send>>,
        _wallet: Option<Arc<Wallet>>,
    ) -> Task<Message> {
        // The fee rate on every visit; one address to send a split coin to,
        // kept for the panel's life so the QR code doesn't change under the
        // user. A Vault that splits with the signature never needs one, but
        // the user can switch to a split coin on the review.
        let fetch = Task::done(Message::SplitFetch {
            fee: Some(self.start_fee_fetch()),
            address: self.address.is_none(),
        });
        let Some(daemon) = daemon else {
            return fetch;
        };
        let labels = Task::perform(
            async move {
                let coins = daemon
                    .list_coins(&[CoinStatus::Confirmed], &[])
                    .await
                    .map(|res| res.coins)
                    .map_err(Error::from)?;
                let mut targets = HashSet::<LabelItem>::new();
                for coin in coins {
                    targets.insert(LabelItem::OutPoint(coin.outpoint));
                    targets.insert(LabelItem::Txid(coin.outpoint.txid));
                }
                daemon.get_labels(&targets).await.map_err(|e| e.into())
            },
            Message::Labels,
        );
        Task::batch([fetch, labels])
    }
}

impl SplitHubPanel {
    /// A new fee fetch, superseding any still running: its generation, with
    /// a fresh run of automatic retries. "Getting the fee rate…" until it
    /// answers, so Try again can't stack another lookup meanwhile.
    fn start_fee_fetch(&mut self) -> u64 {
        self.fee_generation += 1;
        self.fee_retries = 0;
        self.fee_failed = false;
        self.fee_generation
    }

    /// A fee rate, or `None` for unavailable: retried on its own a few times,
    /// still "Getting the fee rate…" meanwhile, before the review says it's
    /// unavailable and offers **Try again**. Never a guess. An answer from a
    /// superseded fetch changes nothing.
    fn set_feerate(&mut self, generation: u64, rate: Option<u64>) -> Task<Message> {
        if generation != self.fee_generation {
            return Task::none();
        }
        self.feerate = rate;
        if rate.is_some() {
            self.fee_failed = false;
            self.fee_retries = 0;
            return Task::none();
        }
        if self.fee_retries < FEE_AUTO_RETRIES {
            self.fee_retries += 1;
            self.fee_failed = false;
            return Task::perform(
                async { tokio::time::sleep(FEE_RETRY_DELAY).await },
                move |()| fetch_fee(generation),
            );
        }
        self.fee_failed = true;
        Task::none()
    }
}

/// Fetch the fee rate only, for `generation`; the split address is kept
/// for the panel's life.
fn fetch_fee(generation: u64) -> Message {
    Message::SplitFetch {
        fee: Some(generation),
        address: false,
    }
}

impl From<SplitHubPanel> for Box<dyn State> {
    fn from(s: SplitHubPanel) -> Box<dyn State> {
        Box::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        chain::ChainId,
        services::{chain_status::ProofEntry, entangled::Entanglement},
    };
    use coincube_core::{
        claim_ancestry::{retained::RetainedPath, search::OwnedLink},
        descriptors::CoincubeDescriptor,
        miniscript::bitcoin::{
            self, absolute, consensus::serialize, script::Builder, transaction, Amount, ScriptBuf,
            Sequence, Transaction, TxIn, TxOut, Witness,
        },
    };
    use coincubed::commands::LCSpendInfo;
    use std::str::FromStr;

    const DESC: &str = "wsh(or_d(pk([f5acc2fd]tpubD6NzVbkrYhZ4YgUx2ZLNt2rLYAMTdYysCRzKoLu2BeSHKvzqPaBDvf17GeBPnExUVPkuBpx4kniP964e2MxyzzazcXLptxLXModSVCVEV1T/<0;1>/*),and_v(v:pkh([8a64f2a9]tpubD6NzVbkrYhZ4WmzFjvQrp7sDa4ECUxTi9oby8K4FZkd3XCBtEdKwUiQyYJaxiJo5y42gyDWEczrFpozEjeLxMPxjf2WtkfcbpUdfvNnozWF/<0;1>/*),older(10))))#d72le4dr";

    fn wallet() -> Arc<Wallet> {
        Arc::new(Wallet::new(
            CoincubeDescriptor::from_str(DESC).expect("fixture descriptor"),
        ))
    }

    const ADDRESS: &str = "bc1qvrl2849aggm6qry9ea7xqp2kk39j8vaa8r3cwg";

    fn coin(outpoint: OutPoint, sats: u64, height: Option<i32>) -> Coin {
        Coin {
            outpoint,
            amount: Amount::from_sat(sats),
            block_height: height,
            spend_info: None,
            is_immature: false,
            address: bitcoin::Address::from_str(ADDRESS)
                .unwrap()
                .assume_checked(),
            derivation_index: 0.into(),
            is_change: false,
            is_from_self: false,
        }
    }

    fn shared(n: u8) -> OutPoint {
        OutPoint::from_str(&format!("{}:0", format!("{:02x}", n).repeat(32))).unwrap()
    }

    fn key_proof(coinbase: &Transaction, key: OutPoint) -> ProofEntry {
        ProofEntry::Verified(
            RetainedPath::new(
                key,
                vec![OwnedLink {
                    transaction: serialize(coinbase),
                    parent_input: None,
                }],
            )
            .unwrap(),
        )
    }

    fn coinbase() -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: Builder::new().push_int(962_000).push_int(1).into_script(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(30_000),
                script_pubkey: ScriptBuf::new(),
            }],
        }
    }

    /// Two both-chain coins and one verified split coin.
    fn fixture() -> (SplitHubPanel, Cache, [OutPoint; 3]) {
        let coinbase = coinbase();
        let (a, b) = (shared(1), shared(2));
        let key = OutPoint::new(coinbase.compute_txid(), 0);
        let mut cache = Cache::default();
        cache.daemon_cache.blockheight = 110;
        cache.daemon_cache.coins = vec![
            coin(a, 100_000, Some(100)),
            coin(b, 100_000, Some(100)),
            coin(key, 30_000, Some(100)),
        ];
        let now = std::time::Instant::now();
        cache.record_entanglement(a.txid, Entanglement::Entangled, now);
        cache.record_entanglement(b.txid, Entanglement::Entangled, now);
        cache.record_entanglement(key.txid, Entanglement::NotEntangled, now);
        cache.chain_proofs.insert(key, key_proof(&coinbase, key));
        (SplitHubPanel::new(wallet()), cache, [a, b, key])
    }

    fn facts() -> FlowFacts {
        FlowFacts {
            both: vec![shared(1)],
            unverified: 0,
            key: true,
            key_status: KeyStatus::Waiting,
            signature_available: false,
            sending: None,
            unsigned: None,
            reserved: Vec::new(),
            split_before: 0,
        }
    }

    #[test]
    fn the_step_follows_the_vault() {
        assert_eq!(step(&facts(), Method::SplitKey, false), Step::Review);
        let no_key = FlowFacts {
            key: false,
            ..facts()
        };
        assert_eq!(
            step(&no_key, Method::SplitKey, false),
            Step::Key(KeyStatus::Waiting)
        );
        assert_eq!(
            step(&no_key, Method::Signature, false),
            Step::Review,
            "a signature split needs no split coin"
        );
        let txid = shared(9).txid;
        assert_eq!(
            step(
                &FlowFacts {
                    unsigned: Some(txid),
                    reserved: vec![shared(1)],
                    ..facts()
                },
                Method::SplitKey,
                false
            ),
            Step::Unsigned { txid },
            "nothing else to split"
        );
        assert_eq!(
            step(
                &FlowFacts {
                    unsigned: Some(txid),
                    both: vec![shared(1), shared(2)],
                    reserved: vec![shared(1)],
                    ..facts()
                },
                Method::SplitKey,
                false
            ),
            Step::Review,
            "an unsent split holds back only its own coins"
        );
        assert_eq!(
            step(
                &FlowFacts {
                    sending: Some(3),
                    unsigned: Some(txid),
                    ..facts()
                },
                Method::SplitKey,
                false
            ),
            Step::Sent { confirmations: 3 },
            "a sent split wins over everything"
        );
        let rest = FlowFacts {
            split_before: 2,
            ..facts()
        };
        assert_eq!(
            step(&rest, Method::SplitKey, false),
            Step::Rest { split: 2 }
        );
        assert_eq!(step(&rest, Method::SplitKey, true), Step::Review);
        let nothing = FlowFacts {
            both: Vec::new(),
            ..facts()
        };
        assert_eq!(step(&nothing, Method::SplitKey, false), Step::Done);
        assert_eq!(
            step(
                &FlowFacts {
                    unverified: 2,
                    ..nothing
                },
                Method::SplitKey,
                false
            ),
            Step::Verifying { count: 2 },
            "done only on positive evidence"
        );
    }

    #[test]
    fn a_vault_with_a_split_coin_starts_at_review() {
        let (panel, cache, [a, b, _]) = fixture();
        let flow = panel.flow(&cache);
        assert_eq!(flow.step, Step::Review);
        assert!(flow.key_verified);
        assert_eq!(flow.method, Method::SplitKey, "Bitcoin splits with a coin");
        assert_eq!(flow.key, Some(Amount::from_sat(30_000)));
        assert_eq!(
            flow.coins.iter().map(|c| c.outpoint).collect::<Vec<_>>(),
            vec![a, b]
        );
        assert_eq!(flow.copy, CopyCard::Create);
    }

    /// A split coin the Vault already holds skips step one after a restart
    /// (its path loads as saved) and while Connect can't be read (its fresh
    /// check fails): the build re-qualifies it, so the flow needn't wait for
    /// the background check nor ask for another coin.
    #[test]
    fn a_split_coin_held_from_an_earlier_session_starts_at_review() {
        let (panel, mut cache, [_, _, key]) = fixture();
        let path = cache.chain_proofs[&key].path().unwrap().clone();
        cache.chain_proofs.insert(key, ProofEntry::Saved(path));
        assert_ne!(cache.chain_status(&key), ChainStatus::OnlyHere);
        let flow = panel.flow(&cache);
        assert_eq!(flow.step, Step::Review);
        assert_eq!(flow.key, Some(Amount::from_sat(30_000)));
        assert!(!flow.key_verified, "not shown as only on this chain yet");

        let failed = ProofEntry::after_check(
            cache.chain_proofs.get(&key),
            ProofEntry::failed(std::time::SystemTime::now()),
        );
        cache.chain_proofs.insert(key, failed);
        assert_eq!(panel.flow(&cache).step, Step::Review);
    }

    /// A coin whose history check couldn't finish says so, instead of
    /// "checking… under a minute", and still counts as the coin on its way.
    #[test]
    fn a_failed_check_shows_as_retrying() {
        let (panel, mut cache, [_, _, key]) = fixture();
        cache
            .chain_proofs
            .insert(key, ProofEntry::failed(std::time::SystemTime::now()));
        assert_eq!(
            panel.flow(&cache).step,
            Step::Key(KeyStatus::Retrying {
                amount: Amount::from_sat(30_000),
                stalled: false,
            })
        );
        // Down to the longest wait, it stops saying another coin isn't
        // needed: some failures never pass.
        let now = std::time::SystemTime::now();
        let stalled = (0..4).fold(ProofEntry::failed(now), |entry, _| {
            ProofEntry::after_check(Some(&entry), ProofEntry::failed(now))
        });
        cache.chain_proofs.insert(key, stalled);
        assert_eq!(
            panel.flow(&cache).step,
            Step::Key(KeyStatus::Retrying {
                amount: Amount::from_sat(30_000),
                stalled: true,
            })
        );
    }

    /// The build's fresh check rejected the split coin: it is no longer
    /// offered, so the flow asks for one instead of failing on every Sign.
    #[test]
    fn a_split_coin_the_build_rejected_sends_the_flow_back_to_step_one() {
        let (panel, mut cache, [_, _, key]) = fixture();
        let path = cache.chain_proofs[&key].path().unwrap().clone();
        cache.chain_proofs.insert(key, ProofEntry::Saved(path));
        assert_eq!(panel.flow(&cache).step, Step::Review);
        cache.split_key_rejected(key, std::time::SystemTime::now());
        let flow = panel.flow(&cache);
        assert!(matches!(flow.step, Step::Key(_)), "{:?}", flow.step);
        assert_eq!(flow.key, None);
    }

    fn reject(panel: &mut SplitHubPanel, cache: &mut Cache, key: OutPoint) {
        // The App records the rejection before the panel hears of it.
        cache.split_key_rejected(key, std::time::SystemTime::now());
        let _ = panel.update(
            None,
            cache,
            Message::SplitPrepared(Err("no longer usable".to_string())),
        );
        panel.split_key_was_rejected();
    }

    /// A second split coin. The smaller of the two is offered first.
    fn second_key(cache: &mut Cache, sats: u64) -> OutPoint {
        let mut coinbase = coinbase();
        coinbase.output[0].value = Amount::from_sat(sats);
        let key = OutPoint::new(coinbase.compute_txid(), 0);
        cache.daemon_cache.coins.push(coin(key, sats, Some(100)));
        cache.record_entanglement(
            key.txid,
            Entanglement::NotEntangled,
            std::time::Instant::now(),
        );
        cache.chain_proofs.insert(key, key_proof(&coinbase, key));
        key
    }

    /// Step one says why the split coin went. The message goes once another
    /// split coin is offered, and doesn't come back if that one goes too.
    #[test]
    fn a_rejection_shows_on_step_one_until_another_split_coin_is_offered() {
        let (mut panel, mut cache, [_, _, key]) = fixture();
        reject(&mut panel, &mut cache, key);
        let flow = panel.flow(&cache);
        assert!(matches!(flow.step, Step::Key(_)), "{:?}", flow.step);
        assert_eq!(flow.error.as_deref(), Some("no longer usable"));

        let other = second_key(&mut cache, 40_000);
        let flow = panel.flow(&cache);
        assert_eq!(flow.step, Step::Review);
        assert_eq!(flow.error, None);

        // As the App does after every message, whichever panel is showing.
        panel.drop_stale_error(&cache);
        cache.split_key_rejected(other, std::time::SystemTime::now());
        let flow = panel.flow(&cache);
        assert!(matches!(flow.step, Step::Key(_)), "{:?}", flow.step);
        assert_eq!(flow.error, None, "the old message doesn't come back");
    }

    /// A replacement on its way isn't the coin the message is about.
    #[test]
    fn a_rejection_goes_when_step_one_reports_on_another_coin() {
        let (mut panel, mut cache, [_, _, key]) = fixture();
        reject(&mut panel, &mut cache, key);
        assert!(panel.flow(&cache).error.is_some());

        cache.daemon_cache.coins.push(coin(shared(9), 50_000, None));
        let flow = panel.flow(&cache);
        assert!(
            matches!(flow.step, Step::Key(KeyStatus::Confirming { .. })),
            "{:?}",
            flow.step
        );
        assert_eq!(flow.error, None);
    }

    /// With another split coin held, the review goes on with it and still
    /// says why the build failed.
    #[test]
    fn a_rejection_with_another_split_coin_held_stays_on_the_review() {
        let (mut panel, mut cache, [_, _, key]) = fixture();
        second_key(&mut cache, 40_000);
        reject(&mut panel, &mut cache, key);
        let flow = panel.flow(&cache);
        assert_eq!(flow.step, Step::Review);
        assert_eq!(flow.key, Some(Amount::from_sat(40_000)));
        assert_eq!(flow.error.as_deref(), Some("no longer usable"));
    }

    /// A failure on a review doesn't come back on a later review: the flow
    /// left that step in between.
    #[test]
    fn a_build_error_goes_when_the_flow_leaves_its_step() {
        let (mut panel, mut cache, _) = fixture();
        let _ = panel.update(
            None,
            &cache,
            Message::SplitPrepared(Err("couldn't build".to_string())),
        );
        assert!(panel.flow(&cache).error.is_some());
        // The coins on both chains go; the split coin stays on offer.
        let coins = cache.daemon_cache.coins.clone();
        cache.daemon_cache.coins.clear();
        cache.daemon_cache.coins.push(coins[2].clone());
        assert!(panel.flow(&cache).key.is_some());
        assert_ne!(panel.flow(&cache).step, Step::Review);
        assert_eq!(panel.flow(&cache).error, None);
        panel.drop_stale_error(&cache);
        cache.daemon_cache.coins = coins;
        let flow = panel.flow(&cache);
        assert_eq!(flow.step, Step::Review);
        assert_eq!(flow.error, None, "the old message doesn't come back");
    }

    /// The message stays while step one reports on the same coin, as that
    /// coin gains confirmations, and goes when it reports on another.
    #[test]
    fn a_rejection_stays_while_step_one_reports_on_the_same_coin() {
        let (mut panel, mut cache, [_, _, key]) = fixture();
        // Another coin is already being checked when the rejection lands.
        cache
            .daemon_cache
            .coins
            .push(coin(shared(9), 50_000, Some(108)));
        reject(&mut panel, &mut cache, key);
        let before = panel.flow(&cache);
        assert!(
            matches!(before.step, Step::Key(KeyStatus::Checking { .. })),
            "{:?}",
            before.step
        );
        assert!(before.error.is_some());

        cache.daemon_cache.blockheight += 1;
        panel.drop_stale_error(&cache);
        let after = panel.flow(&cache);
        assert_ne!(after.step, before.step, "one more confirmation");
        assert!(after.error.is_some(), "still the same coin");

        cache
            .daemon_cache
            .coins
            .retain(|coin| coin.outpoint != shared(9));
        cache.daemon_cache.coins.push(coin(shared(8), 50_000, None));
        assert_eq!(panel.flow(&cache).error, None, "another coin");
    }

    /// Only the review and step one draw the message, so one that lands on
    /// another step isn't kept to show up later.
    #[test]
    fn a_build_error_that_lands_on_another_step_is_not_kept() {
        let (mut panel, mut cache, _) = fixture();
        let coins = std::mem::take(&mut cache.daemon_cache.coins);
        let _ = panel.update(
            None,
            &cache,
            Message::SplitPrepared(Err("couldn't build".to_string())),
        );
        assert_eq!(panel.split_progress_for_test(), (false, None));
        cache.daemon_cache.coins = coins;
        assert_eq!(panel.flow(&cache).error, None);
    }

    /// A rejection with nothing to mark (the error didn't land on a step
    /// that shows it) is not an error.
    #[test]
    fn a_rejection_with_no_error_held_changes_nothing() {
        let (mut panel, cache, _) = fixture();
        panel.split_key_was_rejected();
        assert_eq!(panel.flow(&cache).error, None);
    }

    /// The message is about the method the build used.
    #[test]
    fn a_build_error_goes_with_the_method_it_was_about() {
        let (mut panel, mut cache, [_, _, key]) = fixture();
        reject(&mut panel, &mut cache, key);
        panel.error.as_mut().unwrap().method = Method::Signature;
        assert_eq!(panel.flow(&cache).error, None);
    }

    /// A failure that isn't about the split coin stays on the review, also
    /// when another split coin becomes the one offered.
    #[test]
    fn a_build_error_stays_when_the_offer_moves_to_another_split_coin() {
        let (mut panel, mut cache, [_, _, key]) = fixture();
        let _ = panel.update(
            None,
            &cache,
            Message::SplitPrepared(Err("couldn't build".to_string())),
        );
        let smaller = second_key(&mut cache, 20_000);
        assert_ne!(smaller, key);
        panel.drop_stale_error(&cache);
        let flow = panel.flow(&cache);
        assert_eq!(flow.step, Step::Review);
        assert_eq!(flow.key, Some(Amount::from_sat(20_000)), "the offer moved");
        assert_eq!(flow.error.as_deref(), Some("couldn't build"));
    }

    fn fee(generation: u64, rate: Option<u64>) -> Message {
        Message::SplitFeeRate { generation, rate }
    }

    fn retry_fee() -> Message {
        Message::View(ViewMessage::Split(SplitMessage::RetryFee))
    }

    /// A visit's fee fetch, by its generation.
    fn visit(panel: &mut SplitHubPanel) -> u64 {
        let _ = panel.reload(None, None);
        panel.fee_generation
    }

    /// An unavailable fee rate is retried on its own, still "Getting the fee
    /// rate…", before the review says so and offers Try again, which starts
    /// over. A rate clears the failure.
    #[test]
    fn an_unavailable_fee_rate_retries_then_offers_try_again() {
        let (mut panel, cache, _) = fixture();
        let first = visit(&mut panel);
        for _ in 0..FEE_AUTO_RETRIES {
            let task = panel.update(None, &cache, fee(first, None));
            assert!(task.units() > 0, "retried on its own");
            assert!(!panel.flow(&cache).fee_failed, "still getting the fee rate");
        }
        let task = panel.update(None, &cache, fee(first, None));
        assert_eq!(task.units(), 0, "no more automatic retries");
        let flow = panel.flow(&cache);
        assert!(flow.fee_failed);
        assert_eq!(
            flow.blocked(),
            Some("Fee rate unavailable. Try again later.")
        );

        let task = panel.update(None, &cache, retry_fee());
        assert!(task.units() > 0, "Try again fetches the fee rate");
        assert!(!panel.flow(&cache).fee_failed);
        let again = panel.fee_generation;
        assert_ne!(again, first, "Try again is a new fetch");
        // A fresh run of automatic retries.
        let task = panel.update(None, &cache, fee(again, None));
        assert!(task.units() > 0);

        let _ = panel.update(None, &cache, fee(again, Some(4)));
        let flow = panel.flow(&cache);
        assert_eq!((flow.feerate, flow.fee_failed), (Some(4), false));
        assert_eq!(
            panel.update(None, &cache, retry_fee()).units(),
            0,
            "nothing to retry with a fee rate"
        );
    }

    /// A retry still pending from an earlier visit can't clear the rate a
    /// later visit got, nor schedule another lookup.
    #[test]
    fn a_stale_fee_answer_changes_nothing() {
        let (mut panel, cache, _) = fixture();
        let first = visit(&mut panel);
        assert!(panel.update(None, &cache, fee(first, None)).units() > 0);
        let second = visit(&mut panel);
        let _ = panel.update(None, &cache, fee(second, Some(5)));

        let task = panel.update(None, &cache, fee(first, None));
        assert_eq!(task.units(), 0, "no retry for a superseded fetch");
        let flow = panel.flow(&cache);
        assert_eq!((flow.feerate, flow.fee_failed), (Some(5), false));
        assert_eq!(flow.blocked(), None, "Sign and send stays on");
    }

    /// A visit after a failure is getting the fee rate again, so Try again
    /// is gone until that fetch answers and can't stack a second lookup.
    #[test]
    fn a_visit_after_a_failure_hides_try_again_while_it_fetches() {
        let (mut panel, cache, _) = fixture();
        let first = visit(&mut panel);
        for _ in 0..=FEE_AUTO_RETRIES {
            let _ = panel.update(None, &cache, fee(first, None));
        }
        assert!(panel.flow(&cache).fee_failed);

        let _ = visit(&mut panel);
        let flow = panel.flow(&cache);
        assert!(!flow.fee_failed);
        assert_eq!(flow.blocked(), Some("Getting the fee rate…"));
        assert_eq!(panel.update(None, &cache, retry_fee()).units(), 0);
    }

    #[test]
    fn coins_toggle_and_sign_waits_for_the_fee_then_locks() {
        let (mut panel, cache, [a, b, key]) = fixture();
        let toggle = |o| Message::View(ViewMessage::Split(SplitMessage::ToggleCoin(o)));
        let _ = panel.update(None, &cache, toggle(b));
        let flow = panel.flow(&cache);
        assert_eq!(flow.spent(), (1, Amount::from_sat(100_000)));

        let sign = || Message::View(ViewMessage::Split(SplitMessage::Sign));
        let _ = panel.update(None, &cache, sign());
        assert!(!panel.working, "no fee rate yet");
        let _ = panel.update(None, &cache, fee(0, Some(4)));
        let task = panel.update(None, &cache, sign());
        assert!(panel.working);
        assert!(task.units() > 0);
        // The method's first stage shows from the press, before the build
        // reports anything.
        let flow = panel.flow(&cache);
        assert_eq!(
            flow.stage,
            Some(split_coins::BuildStage::first(flow.method))
        );
        // Locked while building.
        let _ = panel.update(None, &cache, toggle(a));
        assert!(panel.flow(&cache).coins[0].included);
        let _ = key;

        // The build reports where it is; the result clears it.
        let _ = panel.update(
            None,
            &cache,
            Message::SplitBuildStage(split_coins::BuildStage::CheckingKey),
        );
        assert_eq!(
            panel.flow(&cache).stage,
            Some(split_coins::BuildStage::CheckingKey)
        );
        let _ = panel.update(
            None,
            &cache,
            Message::SplitPrepared(Err("Couldn't build".into())),
        );
        assert_eq!(panel.flow(&cache).stage, None);
        // A late stage after the result doesn't bring the status back.
        let _ = panel.update(
            None,
            &cache,
            Message::SplitBuildStage(split_coins::BuildStage::Building),
        );
        assert_eq!(panel.flow(&cache).stage, None);
        assert_eq!(
            panel.split_progress_for_test(),
            (false, Some("Couldn't build".into()))
        );
        assert!(!panel.flow(&cache).coins[1].included, "choices survive");
    }

    #[test]
    fn the_signature_method_needs_a_capable_vault() {
        let (mut panel, cache, _) = fixture();
        let _ = panel.update(
            None,
            &cache,
            Message::View(ViewMessage::Split(SplitMessage::Method(Method::Signature))),
        );
        assert_eq!(panel.flow(&cache).method, Method::SplitKey);
    }

    /// Without a split coin the flow asks for one, and follows the coin
    /// sent to its address: received, checking, or on both chains.
    #[test]
    fn without_a_split_coin_the_flow_follows_the_one_sent() {
        let (mut panel, mut cache, [_, _, key]) = fixture();
        cache.chain_proofs.clear();
        cache.daemon_cache.coins.retain(|coin| coin.outpoint != key);
        assert_eq!(panel.flow(&cache).step, Step::Key(KeyStatus::Waiting));

        const FRESH: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
        let _ = panel.update(None, &cache, Message::SplitAddress(Ok(FRESH.into())));
        assert!(panel.qr.is_some());
        let incoming = shared(7);
        cache.daemon_cache.coins.push(Coin {
            address: bitcoin::Address::from_str(FRESH).unwrap().assume_checked(),
            ..coin(incoming, 25_000, None)
        });
        assert_eq!(
            panel.flow(&cache).step,
            Step::Key(KeyStatus::Confirming {
                amount: Amount::from_sat(25_000)
            })
        );
        cache.daemon_cache.coins.last_mut().unwrap().block_height = Some(110);
        assert_eq!(
            panel.flow(&cache).step,
            Step::Key(KeyStatus::Checking {
                amount: Amount::from_sat(25_000),
                confirmations: 1
            })
        );
        cache.record_entanglement(
            incoming.txid,
            Entanglement::Entangled,
            std::time::Instant::now(),
        );
        let failed = panel.flow(&cache).step;
        assert_eq!(
            failed,
            Step::Key(KeyStatus::BothChains {
                amount: Amount::from_sat(25_000)
            })
        );

        // "Send one from another source": the replacement shows, not the
        // coin that failed, whether it comes to another address or the same.
        let replacement = shared(6);
        cache.daemon_cache.coins.push(Coin {
            address: bitcoin::Address::from_str("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq")
                .unwrap()
                .assume_checked(),
            ..coin(replacement, 40_000, None)
        });
        assert_eq!(
            panel.flow(&cache).step,
            Step::Key(KeyStatus::Confirming {
                amount: Amount::from_sat(40_000)
            })
        );
        cache.daemon_cache.coins.last_mut().unwrap().address =
            bitcoin::Address::from_str(FRESH).unwrap().assume_checked();
        assert_eq!(
            panel.flow(&cache).step,
            Step::Key(KeyStatus::Confirming {
                amount: Amount::from_sat(40_000)
            })
        );
    }

    /// A built split waits to be signed; once sent, the tab follows its
    /// confirmations; once confirmed with coins left, it offers the rest.
    #[test]
    fn after_signing_the_flow_follows_the_split() {
        let (mut panel, mut cache, [a, b, key]) = fixture();
        let txid = shared(8).txid;
        cache.split_records.push(split_coins::SplitRecord {
            txid,
            chain: ChainId::Bitcoin,
            method: Method::SplitKey,
            inputs: vec![a, b],
            key: Some(key),
            key_path: None,
            created_at: 0,
            confirmations: 0,
        });
        assert_eq!(panel.flow(&cache).step, Step::Unsigned { txid });

        // Spending only `a`, it holds back `a` alone: `b` can still be
        // split, under a notice pointing at the split waiting to be signed.
        cache.split_records[0].inputs = vec![a];
        let flow = panel.flow(&cache);
        assert_eq!(flow.waiting_to_sign, Some(txid));
        assert_eq!(
            flow.coins.iter().map(|c| c.outpoint).collect::<Vec<_>>(),
            vec![b]
        );
        assert!(
            matches!(flow.step, Step::Key(_)),
            "its split coin is held back too"
        );

        let output = OutPoint::new(txid, 0);
        cache.daemon_cache.coins.push(Coin {
            is_change: true,
            ..coin(output, 129_000, Some(109))
        });
        cache.splitting.insert(output, 2);
        assert_eq!(panel.flow(&cache).step, Step::Sent { confirmations: 2 });

        cache.splitting.clear();
        cache.split_records[0].confirmations = SPLIT_CONFIRMATIONS;
        for coin in cache.daemon_cache.coins.iter_mut() {
            if coin.outpoint == a || coin.outpoint == key {
                coin.spend_info = Some(LCSpendInfo {
                    txid,
                    height: Some(109),
                });
            }
        }
        let flow = panel.flow(&cache);
        assert_eq!(flow.step, Step::Rest { split: 1 });
        assert_eq!(flow.coins.len(), 1);
        assert_eq!(flow.coins[0].outpoint, b);
        let _ = panel.update(
            None,
            &cache,
            Message::View(ViewMessage::Split(SplitMessage::SplitRest)),
        );
        // The spent split coin is gone; the split output isn't proven yet.
        assert_eq!(panel.flow(&cache).step, Step::Key(KeyStatus::Waiting));
    }

    /// An imported Vault never sees the signature: it splits with a coin
    /// on both chains, and offers its copy on Bitcoin.
    #[test]
    fn imported_vault_splits_with_a_coin_and_offers_its_copy_on_bitcoin() {
        use crate::services::split_test_wallets::{imported_fork_vault, imported_vault, SHAPES};
        for shape in SHAPES {
            for wallet in [imported_vault(shape), imported_fork_vault(shape)] {
                let here = wallet.chain;
                let panel = SplitHubPanel::new(Arc::new(wallet));
                let flow = panel.flow(&Cache::default());
                assert!(!flow.signature_available, "{:?} {:?}", shape, here);
                assert_eq!(flow.method, Method::SplitKey);
                assert_eq!(
                    flow.copy,
                    if here.is_blake2b() {
                        CopyCard::ThisIsTheCopy
                    } else {
                        CopyCard::Create
                    }
                );
            }
        }
    }

    /// The tab mounts each step without a dialog, and the copy card follows
    /// whether a copy exists.
    #[test]
    fn the_tab_mounts_the_flow() {
        use crate::app::view::shared::landing::capture;
        let (panel, mut cache, _) = fixture();
        let mounted = capture::inspect(
            "vault-split-coins-tab",
            view::vault::split_coins::split_flow_view(&panel.flow(&cache), None),
            (1280, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        assert!(mounted.contains("Review the split"));
        assert!(mounted.contains("Cold storage") || mounted.contains("No label"));
        assert!(mounted.contains("Create copy"));
        cache.btcb2_already_claimed = true;
        let mounted = capture::inspect(
            "vault-split-coins-tab-copied",
            view::vault::split_coins::split_flow_view(&panel.flow(&cache), None),
            (1280, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        assert!(!mounted.contains("Create copy"));
        assert!(mounted.contains("Switch to it from the Cube menu"));
    }
}
