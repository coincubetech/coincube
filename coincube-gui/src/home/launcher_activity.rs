//! Network activity behind the Launcher's Cubes: a rail of recent blocks with
//! the mempool queue after the tip, and up to five facts under it (block,
//! mempool, fee rate, hashrate, next halving).
//!
//! It follows the Network filter. Its source is a device preference:
//! Automatic reads the managed node when that node is already running and
//! otherwise COINCUBE | Connect's public Esplora proxy. The Launcher never
//! starts a node; a stopped node only means there is no cookie to read.
//! Nothing here moves at rest: a refresh is a message, and the one motion is
//! a short slide when a new block arrives, skipped under reduced motion.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use iced::{
    widget::{container, text, Space},
    Alignment, Border, Color, Length, Padding, Subscription, Task,
};
use serde::Deserialize;
use serde_json::{json, Value};

use coincube_ui::{
    font, theme,
    widget::{Column, Container, Element, Row},
};

use super::{
    launcher_catalog::{CatalogMessage, NetworkFilter},
    launcher_scene, Home, Message, ViewMessage,
};
use crate::{
    app::settings::{global::GlobalSettings, preferences::NetworkActivitySource},
    chain::ChainId,
    dir::CoincubeDirectory,
};

/// How often a shown network is read again.
pub(super) const REFRESH: Duration = Duration::from_secs(60);
/// The check cadence: a first read lands within one tick of the Launcher
/// showing, and a filter or source change within one tick of the change.
const TICK: Duration = Duration::from_secs(5);
/// The new block's slide.
pub(super) const ARRIVAL: Duration = Duration::from_millis(900);
/// How far the rail slides in from the right, in pixels.
const ARRIVAL_SHIFT: f32 = 26.0;
/// Blocks on the rail, tip included. Esplora's `/blocks` returns ten.
const RAIL_BLOCKS: usize = 10;
/// One block's worth of virtual bytes (4M weight units).
const BLOCK_VSIZE: f64 = 1_000_000.0;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// A mempool smaller than this reads as empty.
const EMPTY_BACKLOG: f64 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    LocalNode,
    Connect,
}

impl Via {
    pub(super) fn label(self) -> &'static str {
        match self {
            Via::LocalNode => "Local node",
            Via::Connect => "COINCUBE | Connect",
        }
    }
}

/// One read of a network. Blocks are oldest first, the tip last.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub tip: u32,
    pub tx_counts: Vec<u32>,
    pub mempool_vsize: u64,
    /// sat/vB to be included in the next block, when the source estimates it.
    pub next_block_fee: Option<f64>,
    pub difficulty: f64,
    pub via: Via,
}

/// Why nothing is shown. The dock's selector says it in words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    LocalNodeNotRunning,
    ConnectUnreachable,
    NoSource,
}

impl Unavailable {
    pub(super) fn label(self) -> &'static str {
        match self {
            Unavailable::LocalNodeNotRunning => "Local node isn't running",
            Unavailable::ConnectUnreachable => "Connect is unreachable",
            Unavailable::NoSource => "No activity source",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ActivityMessage {
    Tick(Instant),
    Loaded {
        request: u64,
        chain: ChainId,
        source: NetworkActivitySource,
        result: Result<Snapshot, Unavailable>,
    },
    Frame(Instant),
    /// Opens or closes the dock's source menu.
    Menu(bool),
}

/// The Launcher's activity: its two device preferences and the last read.
#[derive(Debug, Clone)]
pub(super) struct ActivityState {
    pub(super) enabled: bool,
    pub(super) source: NetworkActivitySource,
    /// The last result, for the network and source it was read for.
    pub(super) shown: Option<Shown>,
    request: u64,
    in_flight: Option<u64>,
    read_at: Option<Instant>,
    /// When the shown tip advanced; the rail slides for [`ARRIVAL`].
    pub(super) arrived: Option<Instant>,
    pub(super) menu: bool,
}

impl Default for ActivityState {
    fn default() -> Self {
        Self {
            enabled: true,
            source: NetworkActivitySource::Automatic,
            shown: None,
            request: 0,
            in_flight: None,
            read_at: None,
            arrived: None,
            menu: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Shown {
    pub(super) chain: ChainId,
    pub(super) source: NetworkActivitySource,
    pub(super) result: Result<Snapshot, Unavailable>,
}

impl ActivityState {
    /// The device preferences, as saved. A file that cannot be read keeps the
    /// defaults: activity on, Automatic.
    pub(super) fn load(root: &CoincubeDirectory) -> Self {
        let mut state = Self::default();
        state.apply_preferences(root);
        state
    }

    pub(super) fn apply_preferences(&mut self, root: &CoincubeDirectory) {
        if let Ok(preferences) = GlobalSettings::load_preferences(&GlobalSettings::path(root)) {
            self.enabled = preferences.network_activity;
            self.source = preferences.network_activity_source;
        }
        if !self.enabled {
            self.menu = false;
        }
    }

    /// The result for `chain` under the current source, if one was read.
    pub(super) fn current(&self, chain: ChainId) -> Option<&Result<Snapshot, Unavailable>> {
        self.shown
            .as_ref()
            .filter(|shown| shown.chain == chain && shown.source == self.source)
            .map(|shown| &shown.result)
    }

    fn due(&self, chain: ChainId, now: Instant) -> bool {
        self.in_flight.is_none()
            && (self.current(chain).is_none()
                || self
                    .read_at
                    .is_none_or(|at| now.saturating_duration_since(at) >= REFRESH))
    }
}

/// The network the activity follows: the filtered network, Testnet4 for the
/// test networks, and Bitcoin otherwise.
pub(super) fn chain_for(filter: NetworkFilter) -> ChainId {
    match filter {
        NetworkFilter::Network(chain) => chain,
        NetworkFilter::Test => ChainId::Testnet4,
        NetworkFilter::All | NetworkFilter::Live => ChainId::Bitcoin,
    }
}

/// Blocks between halvings. Bitcoin Blake2b is taken to keep Bitcoin's
/// subsidy schedule; regtest halves every 150 blocks.
pub(super) fn halving_interval(chain: ChainId) -> u32 {
    if chain == ChainId::Regtest {
        150
    } else {
        210_000
    }
}

/// Blocks left until the next halving after `tip`.
pub(super) fn blocks_to_halving(chain: ChainId, tip: u32) -> u32 {
    let every = halving_interval(chain);
    every - tip % every
}

/// Month and year of the next halving, at ten-minute blocks from `today`.
pub(super) fn halving_month(blocks: u32, today: chrono::NaiveDate) -> String {
    let date = today + chrono::Duration::minutes(i64::from(blocks) * 10);
    date.format("%b %Y").to_string()
}

/// The hashrate a difficulty implies at ten-minute blocks.
pub(super) fn hashrate(difficulty: f64) -> String {
    const UNITS: [&str; 9] = [
        "H/s", "kH/s", "MH/s", "GH/s", "TH/s", "PH/s", "EH/s", "ZH/s", "YH/s",
    ];
    let mut value = difficulty * 4_294_967_296.0 / 600.0;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Difficulty from a compact target, for an Esplora without `difficulty`.
pub(super) fn difficulty_from_bits(bits: u32) -> f64 {
    let exponent = (bits >> 24) as i32;
    let mantissa = f64::from(bits & 0x00ff_ffff);
    if mantissa == 0.0 {
        return 0.0;
    }
    // The genesis target, 0x1d00ffff, has difficulty 1.
    (f64::from(0x00ffffu32) / mantissa) * 256f64.powi(0x1d - exponent)
}

fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A block height with thousands separators.
pub(super) fn block_label(height: u32) -> String {
    grouped(u64::from(height))
}

/// The mempool as blocks' worth of transactions.
pub(super) fn backlog_blocks(vsize: u64) -> f64 {
    vsize as f64 / BLOCK_VSIZE
}

fn fee_label(fee: f64) -> String {
    if fee >= 10.0 {
        format!("≈ {fee:.0} sat/vB")
    } else {
        format!("≈ {fee:.1} sat/vB")
    }
}

// Reading a network.

pub(super) async fn read(
    root: CoincubeDirectory,
    chain: ChainId,
    source: NetworkActivitySource,
) -> Result<Snapshot, Unavailable> {
    match source {
        NetworkActivitySource::Automatic => match local(&root, chain).await {
            Ok(snapshot) => Ok(snapshot),
            Err(error) => {
                tracing::debug!("Launcher activity: no local node for {chain}: {error}");
                connect(chain).await.map_err(|error| {
                    tracing::debug!("Launcher activity: Connect for {chain}: {error}");
                    Unavailable::NoSource
                })
            }
        },
        NetworkActivitySource::LocalNode => local(&root, chain).await.map_err(|error| {
            tracing::debug!("Launcher activity: no local node for {chain}: {error}");
            Unavailable::LocalNodeNotRunning
        }),
        NetworkActivitySource::Connect => connect(chain).await.map_err(|error| {
            tracing::debug!("Launcher activity: Connect for {chain}: {error}");
            Unavailable::ConnectUnreachable
        }),
    }
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())
}

/// COINCUBE | Connect's public Esplora proxy for `chain`. No account needed.
pub(super) fn connect_base(chain: ChainId) -> String {
    format!(
        "{}/api/v1/esplora/{}",
        crate::services::coincube_api_base_url().trim_end_matches('/'),
        crate::installer::connect_esplora_path(chain)
    )
}

#[derive(Debug, Deserialize)]
pub(super) struct EsploraBlock {
    height: u32,
    tx_count: u32,
    #[serde(default)]
    difficulty: Option<f64>,
    #[serde(default)]
    bits: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct EsploraMempool {
    vsize: u64,
}

/// Esplora's answers; `/blocks` lists the newest block first.
pub(super) fn from_esplora(
    mut blocks: Vec<EsploraBlock>,
    mempool: EsploraMempool,
    fees: &HashMap<String, f64>,
) -> Result<Snapshot, String> {
    blocks.sort_by_key(|block| block.height);
    let tip = blocks.last().ok_or("No blocks")?;
    let difficulty = tip
        .difficulty
        .or_else(|| tip.bits.map(difficulty_from_bits))
        .ok_or("No difficulty")?;
    let tip_height = tip.height;
    let skip = blocks.len().saturating_sub(RAIL_BLOCKS);
    Ok(Snapshot {
        tip: tip_height,
        tx_counts: blocks
            .iter()
            .skip(skip)
            .map(|block| block.tx_count)
            .collect(),
        mempool_vsize: mempool.vsize,
        next_block_fee: fees.get("1").copied(),
        difficulty,
        via: Via::Connect,
    })
}

async fn get<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: String,
) -> Result<T, String> {
    client
        .get(url)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|e| e.to_string())?
        .json::<T>()
        .await
        .map_err(|e| e.to_string())
}

async fn connect(chain: ChainId) -> Result<Snapshot, String> {
    let client = http()?;
    let base = connect_base(chain);
    let (blocks, mempool, fees) = tokio::try_join!(
        get::<Vec<EsploraBlock>>(&client, format!("{base}/blocks")),
        get::<EsploraMempool>(&client, format!("{base}/mempool")),
        get::<HashMap<String, f64>>(&client, format!("{base}/fee-estimates")),
    )?;
    from_esplora(blocks, mempool, &fees)
}

struct NodeRpc {
    client: reqwest::Client,
    url: String,
    user: String,
    pass: String,
}

impl NodeRpc {
    /// A JSON-RPC batch: one result per call, in order; `null` for an error.
    async fn batch(&self, calls: &[(&str, Value)]) -> Result<Vec<Value>, String> {
        let body: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(id, (method, params))| {
                json!({"jsonrpc": "1.0", "id": id, "method": method, "params": params})
            })
            .collect();
        let replies: Vec<Value> = self
            .client
            .post(&self.url)
            .basic_auth(&self.user, Some(&self.pass))
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let mut results = vec![Value::Null; calls.len()];
        for reply in replies {
            let Some(id) = reply["id"].as_u64().map(|id| id as usize) else {
                continue;
            };
            if id < results.len() && reply["error"].is_null() {
                results[id] = reply["result"].clone();
            }
        }
        Ok(results)
    }
}

/// The managed node for `chain`, when it is running: its config names the
/// RPC port, and bitcoind deletes the cookie on a clean shutdown.
async fn local(root: &CoincubeDirectory, chain: ChainId) -> Result<Snapshot, String> {
    use crate::node::bitcoind::{
        internal_bitcoind_config_path, internal_bitcoind_cookie_path,
        internal_bitcoind_datadir_for, InternalBitcoindConfig, NodeChainFamily,
    };
    let dir = internal_bitcoind_datadir_for(root, NodeChainFamily::from_chain(chain));
    let network = chain.bitcoin_network();
    let config = InternalBitcoindConfig::from_file(&internal_bitcoind_config_path(&dir))
        .map_err(|e| e.to_string())?;
    let port = config
        .networks
        .get(&network)
        .ok_or("No managed node for this network")?
        .rpc_port;
    // A few bytes on local disk; tokio's `fs` feature is not enabled here.
    let cookie = std::fs::read_to_string(internal_bitcoind_cookie_path(&dir, &network))
        .map_err(|_| "The managed node is not running".to_string())?;
    let (user, pass) = cookie
        .trim()
        .split_once(':')
        .ok_or("Unreadable node cookie")?;
    let rpc = NodeRpc {
        client: http()?,
        url: format!("http://127.0.0.1:{port}/"),
        user: user.to_string(),
        pass: pass.to_string(),
    };
    let first = rpc
        .batch(&[
            ("getblockchaininfo", json!([])),
            ("getmempoolinfo", json!([])),
            ("estimatesmartfee", json!([1])),
        ])
        .await?;
    let snapshot = from_node(&first[0], &first[1], &first[2])?;
    let oldest = snapshot.tip.saturating_sub(RAIL_BLOCKS as u32 - 1);
    let hashes = rpc
        .batch(
            &(oldest..=snapshot.tip)
                .map(|height| ("getblockhash", json!([height])))
                .collect::<Vec<_>>(),
        )
        .await?;
    let headers = rpc
        .batch(
            &hashes
                .iter()
                .filter_map(|hash| hash.as_str())
                .map(|hash| ("getblockheader", json!([hash])))
                .collect::<Vec<_>>(),
        )
        .await?;
    let tx_counts = headers
        .iter()
        .filter_map(|header| header["nTx"].as_u64())
        .map(|count| count as u32)
        .collect();
    Ok(Snapshot {
        tx_counts,
        ..snapshot
    })
}

/// The node's chain, mempool and fee answers. A node still syncing is not a
/// source: its tip is in the past.
pub(super) fn from_node(info: &Value, mempool: &Value, fee: &Value) -> Result<Snapshot, String> {
    if info["initialblockdownload"].as_bool() != Some(false) {
        return Err("The local node is still syncing".into());
    }
    let tip = info["blocks"].as_u64().ok_or("No block height")? as u32;
    let difficulty = info["difficulty"].as_f64().ok_or("No difficulty")?;
    // `bytes` is the sum of virtual sizes. `feerate` is BTC/kvB.
    let mempool_vsize = mempool["bytes"].as_u64().unwrap_or(0);
    let next_block_fee = fee["feerate"].as_f64().map(|rate| rate * 100_000.0);
    Ok(Snapshot {
        tip,
        tx_counts: Vec::new(),
        mempool_vsize,
        next_block_fee,
        difficulty,
        via: Via::LocalNode,
    })
}

// Update and subscription.

impl Home {
    pub(super) fn activity_chain(&self) -> ChainId {
        chain_for(self.catalog.filter)
    }

    /// Activity shows on the Cubes floor only, never in the List.
    pub(super) fn activity_wanted(&self) -> bool {
        self.catalog.activity.enabled
            && super::launcher_catalog::catalog_visible(self)
            && super::launcher_catalog::project(self).floor_eligible(self.catalog.mode)
    }

    pub(super) fn update_activity(&mut self, message: ActivityMessage) -> Task<Message> {
        match message {
            ActivityMessage::Tick(now) => {
                let chain = self.activity_chain();
                if !self.activity_wanted() || !self.catalog.activity.due(chain, now) {
                    return Task::none();
                }
                let activity = &mut self.catalog.activity;
                activity.request = activity.request.wrapping_add(1);
                activity.in_flight = Some(activity.request);
                let request = activity.request;
                let source = activity.source;
                Task::perform(
                    read(self.datadir_path.clone(), chain, source),
                    move |result| {
                        Message::View(ViewMessage::Catalog(CatalogMessage::Activity(
                            ActivityMessage::Loaded {
                                request,
                                chain,
                                source,
                                result,
                            },
                        )))
                    },
                )
            }
            ActivityMessage::Loaded {
                request,
                chain,
                source,
                result,
            } => {
                let activity = &mut self.catalog.activity;
                if activity.in_flight != Some(request) {
                    return Task::none();
                }
                activity.in_flight = None;
                activity.read_at = Some(Instant::now());
                let previous_tip = activity
                    .shown
                    .as_ref()
                    .filter(|shown| shown.chain == chain)
                    .and_then(|shown| shown.result.as_ref().ok())
                    .map(|snapshot| snapshot.tip);
                if let (Some(before), Ok(snapshot)) = (previous_tip, &result) {
                    if snapshot.tip > before {
                        activity.arrived = Some(Instant::now());
                    }
                }
                activity.shown = Some(Shown {
                    chain,
                    source,
                    result,
                });
                Task::none()
            }
            ActivityMessage::Frame(now) => {
                let activity = &mut self.catalog.activity;
                if activity
                    .arrived
                    .is_some_and(|at| now.saturating_duration_since(at) >= ARRIVAL)
                {
                    activity.arrived = None;
                }
                Task::none()
            }
            ActivityMessage::Menu(open) => {
                self.catalog.activity.menu = open && self.catalog.activity.enabled;
                Task::none()
            }
        }
    }

    pub(super) fn activity_subscription(&self, foreground: bool) -> Subscription<Message> {
        if !foreground || !self.activity_wanted() {
            return Subscription::none();
        }
        let tick = iced::time::every(TICK).map(|now| {
            Message::View(ViewMessage::Catalog(CatalogMessage::Activity(
                ActivityMessage::Tick(now),
            )))
        });
        if self.catalog.activity.arrived.is_some() && !self.activity_reduces_motion() {
            let frames = iced::window::frames().map(|now| {
                Message::View(ViewMessage::Catalog(CatalogMessage::Activity(
                    ActivityMessage::Frame(now),
                )))
            });
            return Subscription::batch([tick, frames]);
        }
        tick
    }

    fn activity_reduces_motion(&self) -> bool {
        self.system_reduces_motion == Some(true)
    }
}

// The band in the sky.

/// One fact under the rail.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Fact {
    pub(super) label: &'static str,
    pub(super) value: String,
    pub(super) note: String,
}

/// What the floor draws above the Cubes, owned so the view can move it into
/// its `responsive` closure.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Band {
    /// Width and height of each block, and its opacity, oldest first.
    blocks: Vec<(f32, f32, f32)>,
    tip_color: [u8; 4],
    /// The mempool queue's width, when anything waits.
    queue: Option<f32>,
    pub(super) facts: Vec<Fact>,
    arrived: Option<Instant>,
    reduce_motion: bool,
}

/// Rail, rule and facts. The band is left out below this much sky.
const BAND_HEIGHT: f32 = 96.0;
const BAND_MARGIN: f32 = 20.0;
const RULE_WIDTH: f32 = 760.0;

pub(super) fn facts(chain: ChainId, snapshot: &Snapshot, today: chrono::NaiveDate) -> Vec<Fact> {
    let backlog = backlog_blocks(snapshot.mempool_vsize);
    let waiting = backlog >= EMPTY_BACKLOG;
    let to_halving = blocks_to_halving(chain, snapshot.tip);
    let mut facts = vec![
        Fact {
            label: "BLOCK",
            value: format!("#{}", block_label(snapshot.tip)),
            note: match snapshot.via {
                Via::LocalNode => "from your node".into(),
                Via::Connect => "from Connect".into(),
            },
        },
        Fact {
            label: "MEMPOOL",
            value: if waiting {
                format!("≈ {backlog:.1} blocks")
            } else {
                "empty".into()
            },
            note: if waiting {
                "worth of txs waiting to confirm".into()
            } else {
                "nothing waiting".into()
            },
        },
    ];
    if let Some(fee) = snapshot.next_block_fee {
        facts.push(Fact {
            label: "FEE RATE",
            value: fee_label(fee),
            note: "to make the next block".into(),
        });
    }
    // Signet blocks are signed, not mined: a difficulty says nothing there.
    if chain != ChainId::Signet {
        facts.push(Fact {
            label: "HASHRATE",
            value: hashrate(snapshot.difficulty),
            note: "from difficulty".into(),
        });
    }
    facts.push(Fact {
        label: "NEXT HALVING",
        value: if chain == ChainId::Regtest {
            format!("block {}", block_label(snapshot.tip + to_halving))
        } else {
            format!("≈ {}", halving_month(to_halving, today))
        },
        note: format!("in {} blocks", block_label(to_halving)),
    });
    facts
}

/// Block bars: width and height grow with the block's transaction count,
/// and older blocks fade.
pub(super) fn bars(tx_counts: &[u32]) -> Vec<(f32, f32, f32)> {
    let most = tx_counts.iter().copied().max().unwrap_or(1).max(1) as f32;
    let count = tx_counts.len().max(1) as f32;
    let last = tx_counts.len().saturating_sub(1);
    tx_counts
        .iter()
        .enumerate()
        .map(|(index, &txs)| {
            let share = txs as f32 / most;
            let age = (last - index) as f32 / count;
            (16.0 + 14.0 * share, 5.0 + 5.0 * share, 0.9 - 0.6 * age)
        })
        .collect()
}

impl Home {
    /// The band for the current network, once a read succeeded.
    pub(super) fn activity_band(&self, light: bool) -> Option<Band> {
        if !self.activity_wanted() {
            return None;
        }
        let chain = self.activity_chain();
        let snapshot = self.catalog.activity.current(chain)?.as_ref().ok()?;
        let backlog = backlog_blocks(snapshot.mempool_vsize);
        Some(Band {
            blocks: bars(&snapshot.tx_counts),
            tip_color: launcher_scene::edge(chain, light),
            queue: (backlog >= EMPTY_BACKLOG).then(|| (backlog as f32 * 24.0).clamp(4.0, 160.0)),
            facts: facts(chain, snapshot, chrono::Local::now().date_naive()),
            arrived: self.catalog.activity.arrived,
            reduce_motion: self.activity_reduces_motion(),
        })
    }
}

fn rgba(color: [u8; 4]) -> Color {
    Color::from_rgba8(color[0], color[1], color[2], f32::from(color[3]) / 255.0)
}

fn fill(color: Color) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(color)),
        border: Border {
            radius: 1.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn fact<'a, M: 'a>(fact: &Fact) -> Element<'a, M> {
    let muted = |theme: &theme::Theme| text::Style {
        color: Some(theme.colors.shell.muted),
    };
    Column::new()
        .spacing(2)
        .align_x(Alignment::Center)
        .push(text(fact.label).size(11).font(font::REGULAR).style(muted))
        .push(
            text(fact.value.clone())
                .size(13)
                .font(font::MONO_REGULAR)
                .style(|theme: &theme::Theme| text::Style {
                    color: Some(theme.colors.shell.ink),
                }),
        )
        .push(
            text(fact.note.clone())
                .size(11)
                .font(font::REGULAR)
                .style(muted),
        )
        .into()
}

/// The band laid over the stage's sky, above Cubes whose tops are `sky`
/// pixels down the stage. With too little clear sky it is left out.
pub(super) fn band_layer<'a, M: 'a>(band: &Band, sky: f32, now: Instant) -> Option<Element<'a, M>> {
    if sky < BAND_HEIGHT + 2.0 * BAND_MARGIN {
        return None;
    }
    let top = ((sky - BAND_HEIGHT) / 2.0).max(BAND_MARGIN);
    let shift = match band.arrived {
        Some(at) if !band.reduce_motion => {
            let t = (now.saturating_duration_since(at).as_secs_f32() / ARRIVAL.as_secs_f32())
                .clamp(0.0, 1.0);
            (1.0 - launcher_scene::ease(t)) * ARRIVAL_SHIFT
        }
        _ => 0.0,
    };
    let last = band.blocks.len().saturating_sub(1);
    let tip_color = rgba(band.tip_color);
    // A leading space twice the shift moves the centred rail by the shift.
    let mut rail = Row::new()
        .spacing(3)
        .align_y(Alignment::End)
        .push(Space::new().width(Length::Fixed(2.0 * shift)));
    for (index, &(width, height, alpha)) in band.blocks.iter().enumerate() {
        let tip = index == last;
        rail = rail.push(
            Container::new(Space::new())
                .width(Length::Fixed(width))
                .height(Length::Fixed(height))
                .style(move |theme: &theme::Theme| {
                    fill(if tip {
                        tip_color
                    } else {
                        let mut line = theme.colors.shell.line;
                        line.a *= alpha;
                        line
                    })
                }),
        );
    }
    if let Some(queue) = band.queue {
        rail = rail.push(Space::new().width(Length::Fixed(5.0))).push(
            Container::new(Space::new())
                .width(Length::Fixed(queue))
                .height(Length::Fixed(6.0))
                .style(|theme: &theme::Theme| container::Style {
                    border: Border {
                        color: theme.colors.shell.line,
                        width: 1.0,
                        radius: 1.0.into(),
                    },
                    ..Default::default()
                }),
        );
    }
    let mut facts = Row::new().spacing(40).align_y(Alignment::Start);
    for item in &band.facts {
        facts = facts.push(fact(item));
    }
    Some(
        Container::new(
            Column::new()
                .align_x(Alignment::Center)
                .push(
                    Container::new(rail)
                        .height(Length::Fixed(12.0))
                        .align_y(iced::alignment::Vertical::Bottom),
                )
                .push(
                    Container::new(Space::new())
                        .width(Length::Fixed(RULE_WIDTH))
                        .height(Length::Fixed(1.0))
                        .style(|theme: &theme::Theme| fill(theme.colors.shell.line)),
                )
                .push(Space::new().height(Length::Fixed(14.0)))
                .push(facts),
        )
        .width(Length::Fill)
        .center_x(Length::Fill)
        .padding(Padding {
            top,
            ..Padding::ZERO
        })
        .into(),
    )
}

#[cfg(test)]
impl ActivityState {
    /// A finished read, as if the source had answered.
    pub(super) fn show_for_test(&mut self, chain: ChainId, result: Result<Snapshot, Unavailable>) {
        self.shown = Some(Shown {
            chain,
            source: self.source,
            result,
        });
        self.read_at = Some(Instant::now());
    }
}

/// Bitcoin mainnet as the mock-ups show it, read from Connect.
#[cfg(test)]
pub(super) fn sample_snapshot() -> Snapshot {
    Snapshot {
        tip: 968_412,
        tx_counts: vec![3100, 2900, 3400, 2000, 3550, 3000, 2700, 3380, 3600, 2450],
        mempool_vsize: 3_400_000,
        next_block_fee: Some(6.2),
        difficulty: 150e12,
        via: Via::Connect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(tip: u32) -> Snapshot {
        Snapshot {
            tip,
            tx_counts: vec![3000; RAIL_BLOCKS],
            mempool_vsize: 3_400_000,
            next_block_fee: Some(6.2),
            difficulty: 150e12,
            via: Via::Connect,
        }
    }

    fn today() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 10, 28).unwrap()
    }

    #[test]
    fn follows_the_network_filter() {
        assert_eq!(chain_for(NetworkFilter::All), ChainId::Bitcoin);
        assert_eq!(chain_for(NetworkFilter::Live), ChainId::Bitcoin);
        assert_eq!(chain_for(NetworkFilter::Test), ChainId::Testnet4);
        assert_eq!(
            chain_for(NetworkFilter::Network(ChainId::BitcoinBlake2b)),
            ChainId::BitcoinBlake2b
        );
    }

    #[test]
    fn counts_blocks_to_the_next_halving() {
        assert_eq!(blocks_to_halving(ChainId::Bitcoin, 968_412), 81_588);
        assert_eq!(blocks_to_halving(ChainId::Bitcoin, 840_000), 210_000);
        assert_eq!(blocks_to_halving(ChainId::BitcoinBlake2b, 951_806), 98_194);
        assert_eq!(blocks_to_halving(ChainId::Regtest, 412), 38);
        assert_eq!(halving_month(81_588, today()), "May 2028");
    }

    #[test]
    fn hashrate_comes_from_difficulty() {
        // Difficulty 1 is 2^32 hashes a block: about 7.16 MH/s.
        assert_eq!(hashrate(1.0), "7.16 MH/s");
        assert_eq!(hashrate(150e12), "1.07 ZH/s");
        assert_eq!(hashrate(1.5e12), "10.7 EH/s");
        assert!((difficulty_from_bits(0x1d00ffff) - 1.0).abs() < 1e-9);
        // Block 840,000's target and its published difficulty.
        let d = difficulty_from_bits(0x17034219);
        assert!((d - 86_388_558_925_171.0).abs() / d < 1e-6, "{}", d);
    }

    #[test]
    fn reads_esplora_answers_oldest_first() {
        let blocks: Vec<EsploraBlock> = serde_json::from_value(json!([
            {"id": "c", "height": 102, "tx_count": 30, "difficulty": 2.0},
            {"id": "b", "height": 101, "tx_count": 20, "bits": 486604799u32},
            {"id": "a", "height": 100, "tx_count": 10, "difficulty": 1.0},
        ]))
        .unwrap();
        let mempool: EsploraMempool =
            serde_json::from_value(json!({"count": 9, "vsize": 2_200_000, "total_fee": 1}))
                .unwrap();
        let fees: HashMap<String, f64> =
            serde_json::from_value(json!({"1": 6.5, "6": 3.0})).unwrap();
        let snapshot = from_esplora(blocks, mempool, &fees).unwrap();
        assert_eq!(snapshot.tip, 102);
        assert_eq!(snapshot.tx_counts, vec![10, 20, 30]);
        assert_eq!(snapshot.mempool_vsize, 2_200_000);
        assert_eq!(snapshot.next_block_fee, Some(6.5));
        assert_eq!(snapshot.difficulty, 2.0);
        assert_eq!(snapshot.via, Via::Connect);
    }

    #[test]
    fn an_esplora_tip_without_difficulty_uses_its_bits() {
        let blocks: Vec<EsploraBlock> =
            serde_json::from_value(json!([{"height": 7, "tx_count": 1, "bits": 486604799u32}]))
                .unwrap();
        let snapshot = from_esplora(blocks, EsploraMempool { vsize: 0 }, &HashMap::new()).unwrap();
        assert!((snapshot.difficulty - 1.0).abs() < 1e-9);
        assert_eq!(snapshot.next_block_fee, None);
        assert!(from_esplora(Vec::new(), EsploraMempool { vsize: 0 }, &HashMap::new()).is_err());
    }

    #[test]
    fn a_syncing_node_is_not_a_source() {
        let syncing = json!({"blocks": 5, "difficulty": 1.0, "initialblockdownload": true});
        assert!(from_node(&syncing, &json!({}), &json!({})).is_err());
        let synced = json!({"blocks": 968_412, "difficulty": 1.0, "initialblockdownload": false});
        let snapshot = from_node(
            &synced,
            &json!({"size": 14_300, "bytes": 3_400_000}),
            &json!({"feerate": 0.00006, "blocks": 2}),
        )
        .unwrap();
        assert_eq!(snapshot.tip, 968_412);
        assert_eq!(snapshot.mempool_vsize, 3_400_000);
        assert!((snapshot.next_block_fee.unwrap() - 6.0).abs() < 1e-9);
        assert_eq!(snapshot.via, Via::LocalNode);
        // Regtest's estimatesmartfee answers with errors and no feerate.
        let no_fee = from_node(&synced, &json!({"bytes": 0}), &json!({"errors": ["x"]})).unwrap();
        assert_eq!(no_fee.next_block_fee, None);
    }

    #[test]
    fn connect_serves_blake2b_from_its_public_proxy() {
        assert!(connect_base(ChainId::BitcoinBlake2b)
            .ends_with("/api/v1/esplora/bitcoin-blake2b/mainnet"));
        assert!(connect_base(ChainId::BitcoinBlake2bTestnet4)
            .ends_with("/api/v1/esplora/bitcoin-blake2b/testnet4"));
        assert!(connect_base(ChainId::Bitcoin).ends_with("/api/v1/esplora/bitcoin/mainnet"));
    }

    #[test]
    fn facts_read_in_order() {
        let facts = facts(ChainId::Bitcoin, &snapshot(968_412), today());
        let labels: Vec<_> = facts.iter().map(|f| f.label).collect();
        assert_eq!(
            labels,
            ["BLOCK", "MEMPOOL", "FEE RATE", "HASHRATE", "NEXT HALVING"]
        );
        assert_eq!(facts[0].value, "#968,412");
        assert_eq!(facts[0].note, "from Connect");
        assert_eq!(facts[1].value, "≈ 3.4 blocks");
        assert_eq!(facts[1].note, "worth of txs waiting to confirm");
        assert_eq!(facts[2].value, "≈ 6.2 sat/vB");
        assert_eq!(facts[2].note, "to make the next block");
        assert_eq!(facts[3].value, "1.07 ZH/s");
        assert_eq!(facts[4].value, "≈ May 2028");
        assert_eq!(facts[4].note, "in 81,588 blocks");
    }

    #[test]
    fn signet_has_no_hashrate_and_regtest_names_the_halving_block() {
        let signet = facts(ChainId::Signet, &snapshot(281_044), today());
        assert!(signet.iter().all(|f| f.label != "HASHRATE"));
        let regtest = facts(ChainId::Regtest, &snapshot(412), today());
        assert_eq!(regtest.last().unwrap().value, "block 450");
        assert_eq!(regtest.last().unwrap().note, "in 38 blocks");
    }

    #[test]
    fn an_empty_mempool_says_so() {
        let mut quiet = snapshot(100);
        quiet.mempool_vsize = 0;
        quiet.next_block_fee = None;
        let facts = facts(ChainId::Testnet4, &quiet, today());
        assert_eq!(facts[1].value, "empty");
        assert_eq!(facts[1].note, "nothing waiting");
        assert!(facts.iter().all(|f| f.label != "FEE RATE"));
    }

    #[test]
    fn bars_grow_with_transactions_and_fade_with_age() {
        let bars = bars(&[1000, 2000, 4000]);
        assert_eq!(bars.len(), 3);
        assert!(bars[0].0 < bars[1].0 && bars[1].0 < bars[2].0);
        assert!(bars[0].2 < bars[2].2);
        assert_eq!(bars[2], (30.0, 10.0, 0.9));
        assert!(super::bars(&[]).is_empty());
    }

    #[test]
    fn reads_again_only_when_due() {
        let now = Instant::now();
        let mut state = ActivityState::default();
        assert!(state.due(ChainId::Bitcoin, now));
        state.shown = Some(Shown {
            chain: ChainId::Bitcoin,
            source: NetworkActivitySource::Automatic,
            result: Ok(snapshot(1)),
        });
        state.read_at = Some(now);
        assert!(!state.due(ChainId::Bitcoin, now + Duration::from_secs(5)));
        assert!(state.due(ChainId::Bitcoin, now + REFRESH));
        // Another network or source has nothing shown yet.
        assert!(state.due(ChainId::BitcoinBlake2b, now));
        state.source = NetworkActivitySource::Connect;
        assert!(state.due(ChainId::Bitcoin, now));
        assert!(state.current(ChainId::Bitcoin).is_none());
        state.in_flight = Some(1);
        assert!(!state.due(ChainId::Bitcoin, now));
    }

    #[test]
    fn the_band_needs_clear_sky() {
        let band = Band {
            blocks: vec![(20.0, 8.0, 0.9)],
            tip_color: [243, 130, 29, 153],
            queue: Some(40.0),
            facts: Vec::new(),
            arrived: None,
            reduce_motion: false,
        };
        let now = Instant::now();
        assert!(band_layer::<()>(&band, BAND_HEIGHT, now).is_none());
        assert!(band_layer::<()>(&band, BAND_HEIGHT + 2.0 * BAND_MARGIN, now).is_some());
    }
}
