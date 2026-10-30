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
    /// Progress snapshots replace the retained in-flight snapshot for this
    /// chain; they are not terminal findings.
    #[cfg(test)]
    pub(crate) fn is_progress(&self) -> bool {
        self.outcome == Outcome::InProgress
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
    /// Start a per-attempt clock; paired chains share request and byte totals.
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

    /// Publish the last safe phase before an awaited request or check begins.
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

    /// Mark this chain terminal while retaining its final counters for a later
    /// outer-deadline snapshot of the other chain.
    pub(crate) fn complete(&self, chain: ChainId, outcome: Outcome) {
        self.chains[Self::chain(chain)]
            .completed
            .store(true, Ordering::Relaxed);
        self.emit(chain, outcome);
    }

    /// An outer 90-second cancellation can drop the child future before it
    /// reports an error. Emit that timeout only for a started, active chain.
    pub(crate) fn deadline_if_active(&self, chain: ChainId) {
        let progress = &self.chains[Self::chain(chain)];
        let started = *progress.stage.lock().unwrap() != Stage::Starting;
        if started && !progress.completed.swap(true, Ordering::Relaxed) {
            self.emit(chain, Outcome::SearchDeadline90s);
        }
    }

    /// Send one identifier-free terminal event to the UI and structured log.
    /// A request-specific stage overrides the chain's broader search phase.
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

    /// Offer the UI a replaceable snapshot without logging each graph step.
    fn progress(&self, chain: ChainId) {
        if let Some(observer) = &self.observer {
            observer(self.snapshot(chain, Outcome::InProgress, None));
        }
    }

    /// Capture approximate shared budget and chain-local search counters from
    /// separate atomic loads; this report is never proof material.
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
}
