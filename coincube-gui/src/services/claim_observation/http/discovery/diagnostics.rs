//! Identifier-free, read-only timing diagnostics for one ancestry attempt.
use super::*;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize},
    Mutex,
};
use tokio::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Stage {
    #[default]
    Starting,
    TransactionReads,
    PreForkCheck,
    RawTransaction,
    FreshTransaction,
    ChainView,
    AuthenticatedAnchor,
    RootQualification,
    FrontierRecheck,
    FinalCollection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    InProgress,
    PathFound,
    NoQualifyingAncestry,
    SearchFailed,
    RequestDeadline5s,
    CollectionDeadline30s,
    SearchDeadline90s,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::PathFound => "path_found",
            Self::NoQualifyingAncestry => "no_qualifying_ancestry",
            Self::SearchFailed => "search_failed",
            Self::RequestDeadline5s => "request_deadline_5s",
            Self::CollectionDeadline30s => "collection_deadline_30s",
            Self::SearchDeadline90s => "search_deadline_90s",
        }
    }
}

/// Read-only snapshot for a Claim UI diagnostic. No IDs, URLs, account data or
/// proof objects are retained, and an observer result cannot authorize a spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub(crate) chain: ChainId,
    pub(crate) outcome: Outcome,
    pub(crate) stage: Stage,
    pub(crate) elapsed_ms: u64,
    pub(crate) requests: usize,
    pub(crate) charged_bytes: usize,
    pub(crate) visited: usize,
    pub(crate) pruned: usize,
    pub(crate) roots: usize,
    pub(crate) shared_roots: usize,
    pub(crate) unsupported_roots: usize,
}

pub(crate) type Observer = Arc<dyn Fn(Report) + Send + Sync>;

impl Report {
    pub(crate) fn is_progress(&self) -> bool {
        self.outcome == Outcome::InProgress
    }

    pub(crate) fn is_deadline(&self) -> bool {
        matches!(
            self.outcome,
            Outcome::RequestDeadline5s
                | Outcome::CollectionDeadline30s
                | Outcome::SearchDeadline90s
        )
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}: {} at {}; elapsed={} ms; shared_requests={}; shared_charged_bytes={}; visited={}; pruned={}; roots={}; shared_roots={}; unsupported_roots={}",
            if self.chain == ChainId::Bitcoin { "Bitcoin" } else { "BTCB2" },
            self.outcome.label(),
            self.stage.label(),
            self.elapsed_ms,
            self.requests,
            self.charged_bytes,
            self.visited,
            self.pruned,
            self.roots,
            self.shared_roots,
            self.unsupported_roots,
        )
    }
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::TransactionReads => "transaction_reads",
            Self::PreForkCheck => "canonical_pruning",
            Self::RawTransaction => "raw_transaction",
            Self::FreshTransaction => "fresh_transaction",
            Self::ChainView => "chain_view",
            Self::AuthenticatedAnchor => "authenticated_anchor",
            Self::RootQualification => "root_qualification",
            Self::FrontierRecheck => "frontier_recheck",
            Self::FinalCollection => "final_collection",
        }
    }
}

#[derive(Default)]
struct ChainProgress {
    stage: Mutex<Stage>,
    visited: AtomicUsize,
    pruned: AtomicUsize,
    roots: AtomicUsize,
    shared_roots: AtomicUsize,
    unsupported_roots: AtomicUsize,
    completed: AtomicBool,
}

pub(crate) struct Trace {
    started: Instant,
    observer: Option<Observer>,
    requests: AtomicUsize,
    charged_bytes: AtomicUsize,
    chains: [ChainProgress; 2],
}

impl Trace {
    pub(crate) fn new(observer: Option<Observer>) -> Self {
        Self {
            started: Instant::now(),
            observer,
            requests: AtomicUsize::new(0),
            charged_bytes: AtomicUsize::new(0),
            chains: [ChainProgress::default(), ChainProgress::default()],
        }
    }

    fn chain(chain: ChainId) -> usize {
        usize::from(chain == ChainId::BitcoinBlake2b)
    }

    pub(crate) fn stage(&self, chain: ChainId, stage: Stage) {
        *self.chains[Self::chain(chain)].stage.lock().unwrap() = stage;
        self.progress(chain);
    }

    pub(crate) fn visited(&self, chain: ChainId, pruned: bool) {
        let progress = &self.chains[Self::chain(chain)];
        progress.visited.fetch_add(1, Ordering::Relaxed);
        if pruned {
            progress.pruned.fetch_add(1, Ordering::Relaxed);
        }
        self.progress(chain);
    }

    pub(crate) fn root(&self, chain: ChainId) {
        self.chains[Self::chain(chain)]
            .roots
            .fetch_add(1, Ordering::Relaxed);
        self.progress(chain);
    }

    pub(crate) fn disqualified_root(&self, chain: ChainId, shared: bool) {
        let progress = &self.chains[Self::chain(chain)];
        if shared {
            progress.shared_roots.fetch_add(1, Ordering::Relaxed);
        } else {
            progress.unsupported_roots.fetch_add(1, Ordering::Relaxed);
        }
        self.progress(chain);
    }

    pub(crate) fn request(&self, chain: ChainId, reserved_bytes: usize) {
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.charged_bytes
            .fetch_add(reserved_bytes, Ordering::Relaxed);
        self.progress(chain);
    }

    pub(crate) fn bytes(&self, chain: ChainId, bytes: usize) {
        self.charged_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.progress(chain);
    }

    pub(crate) fn emit(&self, chain: ChainId, outcome: Outcome) {
        self.emit_with_stage(chain, outcome, None);
    }

    pub(crate) fn complete(&self, chain: ChainId, outcome: Outcome) {
        self.chains[Self::chain(chain)]
            .completed
            .store(true, Ordering::Relaxed);
        self.emit(chain, outcome);
    }

    pub(crate) fn deadline_if_active(&self, chain: ChainId) {
        let progress = &self.chains[Self::chain(chain)];
        let started = *progress.stage.lock().unwrap() != Stage::Starting;
        if started && !progress.completed.swap(true, Ordering::Relaxed) {
            self.emit(chain, Outcome::SearchDeadline90s);
        }
    }

    pub(crate) fn emit_with_stage(
        &self,
        chain: ChainId,
        outcome: Outcome,
        request_stage: Option<Stage>,
    ) {
        let report = self.snapshot(chain, outcome, request_stage);
        if let Some(observer) = &self.observer {
            observer(report);
        }
        tracing::info!(
            target: "claim_ancestry_timing",
            chain = if chain == ChainId::Bitcoin { "bitcoin" } else { "btcb2" },
            outcome = outcome.label(),
            stage = report.stage.label(),
            elapsed_ms = report.elapsed_ms,
            shared_requests = report.requests,
            shared_charged_bytes = report.charged_bytes,
            visited = report.visited,
            pruned = report.pruned,
            roots = report.roots,
            shared_roots = report.shared_roots,
            unsupported_roots = report.unsupported_roots,
            "Claim ancestry observation"
        );
    }

    fn progress(&self, chain: ChainId) {
        if let Some(observer) = &self.observer {
            observer(self.snapshot(chain, Outcome::InProgress, None));
        }
    }

    fn snapshot(&self, chain: ChainId, outcome: Outcome, request_stage: Option<Stage>) -> Report {
        let progress = &self.chains[Self::chain(chain)];
        let stage = request_stage.unwrap_or_else(|| *progress.stage.lock().unwrap());
        let requests = self.requests.load(Ordering::Relaxed);
        let charged_bytes = self.charged_bytes.load(Ordering::Relaxed);
        let visited = progress.visited.load(Ordering::Relaxed);
        let pruned = progress.pruned.load(Ordering::Relaxed);
        let roots = progress.roots.load(Ordering::Relaxed);
        let shared_roots = progress.shared_roots.load(Ordering::Relaxed);
        let unsupported_roots = progress.unsupported_roots.load(Ordering::Relaxed);
        Report {
            chain,
            outcome,
            stage,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            requests,
            charged_bytes,
            visited,
            pruned,
            roots,
            shared_roots,
            unsupported_roots,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::claim_observation::http::tests::{ancestry_fixture, source};
    use coincube_core::miniscript::bitcoin::{
        absolute,
        consensus::{deserialize, serialize},
        script::Builder,
        transaction, Amount, ScriptBuf, Transaction, TxIn, TxOut,
    };
    use httpmock::prelude::*;
    use serde_json::json;
    use std::{str::FromStr, time::Duration};

    #[test]
    fn search_deadline_is_reported_once_only_for_an_active_chain() {
        let reports = Arc::new(Mutex::new(Vec::<Report>::new()));
        let observed = reports.clone();
        let trace = Trace::new(Some(Arc::new(move |report| {
            if !report.is_progress() {
                observed.lock().unwrap().push(report);
            }
        })));
        trace.stage(ChainId::Bitcoin, Stage::TransactionReads);
        trace.deadline_if_active(ChainId::Bitcoin);
        trace.deadline_if_active(ChainId::Bitcoin);
        trace.deadline_if_active(ChainId::BitcoinBlake2b);
        trace.complete(ChainId::Bitcoin, Outcome::SearchFailed);
        let captured = reports.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].chain, ChainId::Bitcoin);
        assert_eq!(captured[0].outcome, Outcome::SearchDeadline90s);
        assert_eq!(captured[1].outcome, Outcome::SearchFailed);
        drop(captured);
        trace.stage(ChainId::BitcoinBlake2b, Stage::FrontierRecheck);
        trace.complete(ChainId::BitcoinBlake2b, Outcome::NoQualifyingAncestry);
        trace.deadline_if_active(ChainId::BitcoinBlake2b);
        let captured = reports.lock().unwrap();
        assert_eq!(captured.len(), 3);
        assert_eq!(captured[2].chain, ChainId::BitcoinBlake2b);
        assert_eq!(captured[2].outcome, Outcome::NoQualifyingAncestry);
    }

    fn graph_transaction(parents: &[OutPoint], tag: u8) -> Transaction {
        Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: parents
                .iter()
                .map(|parent| TxIn {
                    previous_output: *parent,
                    script_sig: ScriptBuf::from_bytes(vec![tag]),
                    ..TxIn::default()
                })
                .collect(),
            output: vec![TxOut {
                value: Amount::from_sat(2_000),
                script_pubkey: ScriptBuf::new(),
            }],
        }
    }

    fn fresh(server: &MockServer, path: String, body: String, delay: Duration) {
        server.mock(|when, then| {
            when.method(GET)
                .path(path)
                .header("x-coincube-observation", "fresh");
            then.status(200)
                .delay(delay)
                .header("x-cache", "BYPASS")
                .header("cache-control", "no-store")
                .header("x-coincube-observation", "fresh")
                .body(body);
        });
    }

    fn raw(server: &MockServer, path: String, tx: &Transaction, delay: Duration) {
        server.mock(|when, then| {
            when.method(GET).path(path);
            then.status(200)
                .delay(delay)
                .body(hex::encode(serialize(tx)));
        });
    }

    async fn paired_fixture(
        negative_chain: Option<ChainId>,
        read_delay: Duration,
    ) -> (
        Result<Option<CollectedSpendInputs>, DiscoveryError>,
        Vec<Report>,
    ) {
        let server = MockServer::start();
        let (source, _sender) = source(&server);
        let reports = Arc::new(Mutex::new(Vec::<Report>::new()));
        let observed = reports.clone();
        let source = source
            .with_funding_observer(Arc::new(|_, _| {}))
            .with_ancestry_timing_observer(Arc::new(move |report: Report| {
                if !report.is_progress() {
                    observed.lock().unwrap().push(report);
                }
            }));
        let (_, raw_coinbase) = ancestry_fixture();
        let mut bitcoin: Transaction = deserialize(&raw_coinbase).unwrap();
        bitcoin.input[0].script_sig = Builder::new().push_int(961640).push_int(1).into_script();
        let mut fork = bitcoin.clone();
        fork.output[0].value = Amount::from_sat(4_000);
        let out = |tx: &Transaction| OutPoint::new(tx.compute_txid(), 0);
        let missing = OutPoint::new(Txid::from_str(&"77".repeat(32)).unwrap(), 0);
        let mut large_parents = vec![missing];
        large_parents.extend(
            (1..950).map(|n| OutPoint::new(Txid::from_str(&format!("{n:064x}")).unwrap(), 0)),
        );
        let large_leaf = graph_transaction(&large_parents, 1);
        let shared_tx = graph_transaction(&[missing], 124);
        let mut leaves = vec![large_leaf.clone()];
        leaves.extend((2..=121).map(|tag| graph_transaction(&[missing], tag)));
        let selected = graph_transaction(&leaves.iter().map(out).collect::<Vec<_>>(), 122);
        let bitcoin_positive = graph_transaction(
            &leaves
                .iter()
                .map(out)
                .chain(std::iter::once(out(&bitcoin)))
                .collect::<Vec<_>>(),
            123,
        );
        let fork_positive = graph_transaction(
            &leaves
                .iter()
                .map(out)
                .chain(std::iter::once(out(&fork)))
                .collect::<Vec<_>>(),
            123,
        );
        assert!(serialize(&large_leaf).len() < claim_ancestry::MAX_TRANSACTION_BYTES);
        let large_metadata = json!({
            "txid": large_leaf.compute_txid(),
            "status": {"confirmed": true, "block_height": 961600, "block_hash": "55".repeat(32)},
            "vin": large_leaf.input.iter().map(|input| json!({
                "txid": input.previous_output.txid,
                "vout": input.previous_output.vout,
                "prevout": {"scriptpubkey": "0014".to_owned() + &"66".repeat(20),
                    "scriptpubkey_asm": "OP_0 OP_PUSHBYTES_20 ".to_owned() + &"66".repeat(20),
                    "scriptpubkey_type": "v0_p2wpkh", "value": 2_000},
                "scriptsig": "01", "scriptsig_asm": "OP_PUSHBYTES_1 01",
                "witness": [], "is_coinbase": false,
                "sequence": input.sequence.to_consensus_u32()
            })).collect::<Vec<_>>()
        })
        .to_string();
        assert!(large_metadata.len() > super::super::super::BODY_LIMIT);
        assert!(large_metadata.len() < super::super::super::TRANSACTION_METADATA_LIMIT);
        let bitcoin_tip = "11".repeat(32);
        let fork_tip = "22".repeat(32);
        let bitcoin_block = "33".repeat(32);
        let fork_block = "44".repeat(32);
        let frontier_block = "55".repeat(32);
        let bitcoin_selected_block = "66".repeat(32);
        let fork_selected_block = "88".repeat(32);
        server.mock(|when, then| {
            when.method(GET)
                .path("/api/v1/connect/networks/bitcoin-blake2b/anchor");
            then.status(200).json_body(json!({"success":true,"data":{
                "network":"bitcoin-blake2b","state":"available","anchor":{
                    "tip_hash":fork_tip,"tip_height":961840,"tip_median_time_past":10000,
                    "observed_at":source.now(),
                    "observation":{"tip_height":961840,"fork":{"height":961640,"active":true},
                        "rdts":{"state":"unsupported"}}
                }
            }}));
        });
        for (chain, tip, block, coinbase) in [
            (ChainId::Bitcoin, &bitcoin_tip, &bitcoin_block, &bitcoin),
            (ChainId::BitcoinBlake2b, &fork_tip, &fork_block, &fork),
        ] {
            let base = format!(
                "/api/v1/esplora/{}",
                HttpObservationSource::prefix(chain).unwrap()
            );
            fresh(
                &server,
                format!("{base}/blocks/tip/hash"),
                tip.clone(),
                Duration::ZERO,
            );
            fresh(
                &server,
                format!("{base}/block/{tip}/status"),
                json!({"in_best_chain":true,"height":961840}).to_string(),
                Duration::ZERO,
            );
            fresh(
                &server,
                format!("{base}/block-height/961840"),
                tip.clone(),
                Duration::ZERO,
            );
            fresh(
                &server,
                format!("{base}/block-height/961640"),
                block.clone(),
                Duration::ZERO,
            );
            fresh(
                &server,
                format!("{base}/block-height/227931"),
                "000000000000024b89b42a942fe0d9fea3bb44ab7bd1b19115dd6a759c0808b8".into(),
                Duration::ZERO,
            );
            fresh(
                &server,
                format!("{base}/block/{block}/txid/0"),
                coinbase.compute_txid().to_string(),
                Duration::ZERO,
            );
            raw(
                &server,
                format!("{base}/tx/{}/hex", coinbase.compute_txid()),
                coinbase,
                Duration::ZERO,
            );
            fresh(
                &server,
                format!("{base}/tx/{}", coinbase.compute_txid()),
                json!({"txid":coinbase.compute_txid(),"status":{"confirmed":true,
                        "block_height":961640,"block_hash":block}})
                .to_string(),
                Duration::ZERO,
            );
            fresh(
                &server,
                format!("{base}/block-height/961600"),
                frontier_block.clone(),
                Duration::ZERO,
            );
            let selected_block = if chain == ChainId::Bitcoin {
                &bitcoin_selected_block
            } else {
                &fork_selected_block
            };
            fresh(
                &server,
                format!("{base}/block-height/961660"),
                selected_block.clone(),
                Duration::ZERO,
            );
            let graph_selected = if Some(chain) == negative_chain {
                &selected
            } else if chain == ChainId::Bitcoin {
                &bitcoin_positive
            } else {
                &fork_positive
            };
            for (tx, height, hash) in std::iter::once((graph_selected, 961660, selected_block))
                .chain(leaves.iter().map(|leaf| (leaf, 961600, &frontier_block)))
            {
                raw(
                    &server,
                    format!("{base}/tx/{}/hex", tx.compute_txid()),
                    tx,
                    read_delay,
                );
                fresh(
                    &server,
                    format!("{base}/tx/{}", tx.compute_txid()),
                    if tx.compute_txid() == large_leaf.compute_txid() {
                        large_metadata.clone()
                    } else {
                        json!({"txid":tx.compute_txid(),"status":{"confirmed":true,
                                    "block_height":height,"block_hash":hash}})
                        .to_string()
                    },
                    read_delay,
                );
            }
            raw(
                &server,
                format!("{base}/tx/{}/hex", shared_tx.compute_txid()),
                &shared_tx,
                Duration::ZERO,
            );
            fresh(
                &server,
                format!("{base}/tx/{}", shared_tx.compute_txid()),
                json!({"txid":shared_tx.compute_txid(),"status":{"confirmed":true,
                    "block_height":961600,"block_hash":frontier_block}})
                .to_string(),
                Duration::ZERO,
            );
            for txid in [graph_selected.compute_txid(), shared_tx.compute_txid()] {
                fresh(
                    &server,
                    format!("{base}/tx/{txid}/outspend/0"),
                    json!({"spent":false}).to_string(),
                    Duration::ZERO,
                );
            }
        }
        let shared = [out(&shared_tx)];
        let bitcoin_candidate = if negative_chain == Some(ChainId::Bitcoin) {
            out(&selected)
        } else {
            out(&bitcoin_positive)
        };
        let fork_candidate = if negative_chain == Some(ChainId::BitcoinBlake2b) {
            out(&selected)
        } else {
            out(&fork_positive)
        };
        let result = source
            .discover_spend_inputs(
                &[bitcoin_candidate],
                &[fork_candidate],
                &shared,
                crate::app::state::vault::claim::CHECK_POLICY.observations,
            )
            .await;
        let reports = reports.lock().unwrap().clone();
        (result, reports)
    }

    #[tokio::test]
    async fn paired_search_reports_large_branch_latency_and_real_negative_on_either_chain() {
        for negative_chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
            let (result, reports) =
                paired_fixture(Some(negative_chain), Duration::from_millis(3)).await;
            assert!(matches!(result, Ok(None)), "{:?}", result);
            let positive_chain = if negative_chain == ChainId::Bitcoin {
                ChainId::BitcoinBlake2b
            } else {
                ChainId::Bitcoin
            };
            let negative = reports
                .iter()
                .find(|report| {
                    report.chain == negative_chain
                        && report.outcome == Outcome::NoQualifyingAncestry
                })
                .unwrap();
            let positive = reports
                .iter()
                .find(|report| {
                    report.chain == positive_chain && report.outcome == Outcome::PathFound
                })
                .unwrap();
            assert_eq!(
                (negative.visited, negative.pruned, negative.roots),
                (122, 121, 0)
            );
            assert_eq!(
                (positive.visited, positive.pruned, positive.roots),
                (123, 121, 1)
            );
            assert!(negative.elapsed_ms >= 600);
            assert!(negative.charged_bytes > super::super::super::BODY_LIMIT);
            assert!(negative.requests >= 6);
        }
    }

    #[tokio::test]
    async fn both_positive_paths_behind_siblings_complete_fresh_paired_collection() {
        let (result, reports) = paired_fixture(None, Duration::from_millis(3)).await;
        let inputs = result
            .expect("paired reads must succeed")
            .expect("both paths must qualify");
        assert_ne!(
            inputs.funding().bitcoin().selected(),
            inputs.funding().fork().selected()
        );
        for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
            let found = reports
                .iter()
                .find(|report| report.chain == chain && report.outcome == Outcome::PathFound)
                .unwrap();
            assert_eq!((found.visited, found.pruned, found.roots), (123, 121, 1));
        }
    }

    /// Run explicitly to guard the cumulative-latency repair with sub-five-
    /// second raw and metadata reads; canonical-height/root mocks answer
    /// immediately. Both positive paths sit behind 121 pre-fork siblings.
    /// Before overlapping the two reads per node, this same fixture hit the
    /// unchanged 90-second search deadline in 90.99 seconds.
    #[tokio::test]
    #[ignore = "380 ms raw and metadata reads exercise the real 90-second paired deadline"]
    async fn representative_latency_completes_paired_collection_within_search_deadline() {
        let (result, reports) = paired_fixture(None, Duration::from_millis(380)).await;
        assert!(matches!(result, Ok(Some(_))), "{:?}", result);
        for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
            let found = reports
                .iter()
                .find(|report| report.chain == chain && report.outcome == Outcome::PathFound)
                .unwrap();
            assert_eq!((found.visited, found.pruned, found.roots), (123, 121, 1));
            assert!(found.elapsed_ms < 90_000);
        }
        assert!(!reports
            .iter()
            .any(|report| report.outcome == Outcome::RequestDeadline5s));
        assert!(!reports
            .iter()
            .any(|report| report.outcome == Outcome::SearchDeadline90s));
    }

    #[tokio::test]
    async fn timed_out_raw_request_reports_five_second_deadline_before_search_failure() {
        let server = MockServer::start();
        let (source, _sender) = source(&server);
        let reports = Arc::new(Mutex::new(Vec::<Report>::new()));
        let observed = reports.clone();
        let source = source.with_ancestry_timing_observer(Arc::new(move |report: Report| {
            if !report.is_progress() {
                observed.lock().unwrap().push(report);
            }
        }));
        let (txid, raw) = ancestry_fixture();
        let base = format!("/api/v1/esplora/bitcoin/mainnet/tx/{txid}/hex");
        server.mock(|when, then| {
            when.method(GET).path(base);
            then.status(200)
                .delay(Duration::from_secs(6))
                .body(hex::encode(raw));
        });
        fresh(
            &server,
            format!("/api/v1/esplora/bitcoin/mainnet/tx/{txid}"),
            json!({"txid":txid,"status":{"confirmed":false}}).to_string(),
            Duration::ZERO,
        );
        let result = source
            .discover_ancestry(
                OutPoint::new(txid, 0),
                crate::app::state::vault::claim::CHECK_POLICY.observations,
            )
            .await;
        assert!(matches!(
            result,
            Err(DiscoveryError::Observation(FailureKind::Deadline))
        ));
        let reports = reports.lock().unwrap();
        assert_eq!(reports[0].outcome, Outcome::RequestDeadline5s);
        assert_eq!(reports[0].stage, Stage::RawTransaction);
        assert_eq!(reports[0].chain, ChainId::Bitcoin);
        assert!(reports[0].elapsed_ms >= 5_000);
        assert_eq!(reports.last().unwrap().outcome, Outcome::SearchFailed);
        assert!(!reports[0].to_string().contains(&txid.to_string()));
    }
}
