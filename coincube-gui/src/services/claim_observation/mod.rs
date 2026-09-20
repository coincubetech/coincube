//! Dormant, observation-only Claim prerequisite. No production transport adapter
//! or UI caller is installed. A complete assessment is never spend permission.
use async_trait::async_trait;
use coincube_core::{
    chain::ChainId,
    claim::{
        self, Assessment, BitcoinObservation, BlockRef, ClaimPlan, DeploymentObservation,
        DeploymentState, ForkObservation, ForkTransactionPresence, Poison, Policy, PreflightTips,
        TransactionLocation,
    },
    miniscript::bitcoin::{BlockHash, Txid},
};
use reqwest::header::{HeaderMap, CACHE_CONTROL};
use std::{collections::BTreeSet, time::Duration};
use tokio::sync::watch;

use super::coincube::{
    network_anchor::{AnchorState, NetworkAnchor, NetworkAnchorStatus},
    network_status::RdtsStatus,
};

pub const MAX_COLLECTION_TIME: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Plan,
    BitcoinTip,
    ForkAnchor,
    ForkIndexer,
    BitcoinTransaction,
    ForkTransaction,
    BitcoinInclusion,
    Preflight,
    Context,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    InvalidPlan,
    UnsupportedPoison,
    PoisonMissing,
    WrongChain,
    Malformed,
    FreshnessUnverified,
    Stale,
    Changed,
    Cancelled,
    Deadline,
    Http(u16),
    Unavailable,
    Anchor(AnchorState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    pub stage: Stage,
    pub kind: FailureKind,
}
fn failure(stage: Stage, kind: FailureKind) -> Failure {
    Failure { stage, kind }
}

/// Only a response explicitly acknowledging the fresh-read contract can be
/// represented here. A future adapter must supply actual response headers and
/// a conservative collection-start time, never synthesize them from a cache hit.
#[derive(Debug, Clone)]
pub struct FreshRead<T> {
    chain: ChainId,
    value: T,
    observed_at: i64,
}
impl<T> FreshRead<T> {
    pub fn from_response(
        chain: ChainId,
        value: T,
        observed_at: i64,
        headers: &HeaderMap,
    ) -> Result<Self, FailureKind> {
        let mut cache_headers = headers.get_all("x-cache").iter();
        let bypass = cache_headers
            .next()
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("BYPASS"))
            && cache_headers.next().is_none();
        let no_store = headers
            .get_all(CACHE_CONTROL)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|v| v.trim().eq_ignore_ascii_case("no-store"));
        if !bypass || !no_store {
            return Err(FailureKind::FreshnessUnverified);
        }
        if observed_at < 0 {
            return Err(FailureKind::Malformed);
        }
        Ok(Self {
            chain,
            value,
            observed_at,
        })
    }
}

/// `Absent` means a successful fresh lookup explicitly returned 404. Transport
/// or service failures must be Err(Http/Unavailable), never Absent. Even a real
/// absence does not prove chain-exclusive ancestry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionObservation {
    Absent,
    Unconfirmed { txid: Txid },
    Confirmed { txid: Txid, block: BlockRef },
}

/// One immutable API/provider/session context for the entire collection. Anchor
/// must come from the existing authenticated typed endpoint. Transaction/indexer
/// reads use the anonymous fresh-read contract, with no JWT/device linkage or
/// fallback. Implementations must be cancellation-safe when their futures drop.
/// No concrete HTTP implementation is supplied until that API contract lands.
#[async_trait]
pub trait ObservationSource: Send + Sync {
    fn now(&self) -> i64;
    async fn anchor(&self, chain: ChainId) -> Result<NetworkAnchorStatus, FailureKind>;
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind>;
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind>;
    async fn hash_at_height(
        &self,
        chain: ChainId,
        height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind>;
}

/// Caller changes generation on account, provider, Cube/chain change or cancel.
/// Closing all senders cancels too; collection never owns a sender that could
/// accidentally keep a revoked context alive.
pub struct CollectionContext {
    pub expected_generation: u64,
    pub generation: watch::Receiver<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationBundle {
    pub bitcoin: BitcoinObservation,
    pub fork: ForkObservation,
    pub deployment: DeploymentObservation,
    pub preflight: PreflightTips,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectedAssessment {
    /// Recheck this generation when consuming the result; delivery can race logout.
    pub generation: u64,
    pub observations: ObservationBundle,
    pub assessment: Assessment,
}

fn fresh(stamp: i64, now: i64, max_age: i64) -> bool {
    stamp >= 0
        && now
            .checked_sub(stamp)
            .is_some_and(|age| age >= 0 && age <= max_age)
}
fn read<T>(
    value: FreshRead<T>,
    chain: ChainId,
    source: &dyn ObservationSource,
    policy: Policy,
    stage: Stage,
) -> Result<(T, i64), Failure> {
    if value.chain != chain {
        return Err(failure(stage, FailureKind::WrongChain));
    }
    if !fresh(
        value.observed_at,
        source.now(),
        policy.max_observation_age_seconds,
    ) {
        return Err(failure(stage, FailureKind::Stale));
    }
    Ok((value.value, value.observed_at))
}
fn validate_plan(plan: &ClaimPlan, policy: Policy, budget: Duration) -> Result<(), Failure> {
    if !matches!(
        (plan.bitcoin_chain, plan.fork_chain),
        (ChainId::Bitcoin, ChainId::BitcoinBlake2b)
            | (ChainId::Testnet4, ChainId::BitcoinBlake2bTestnet4)
    ) {
        return Err(failure(Stage::Plan, FailureKind::WrongChain));
    }
    let claimed: BTreeSet<_> = plan.claimed_prevouts.iter().copied().collect();
    let actual: BTreeSet<_> = plan.step1.input.iter().map(|i| i.previous_output).collect();
    if claimed.is_empty()
        || claimed.len() != plan.claimed_prevouts.len()
        || actual.len() != plan.step1.input.len()
        || !claimed.is_subset(&actual)
        || plan.step1.input.iter().any(|i| i.previous_output.is_null())
        || plan.step1.output.is_empty()
        || policy.max_observation_age_seconds <= 0
        || policy.expiry_margin_seconds <= 0
        || budget.is_zero()
        || budget > MAX_COLLECTION_TIME
    {
        return Err(failure(Stage::Plan, FailureKind::InvalidPlan));
    }
    if plan.poison == Poison::InputAncestry {
        return Err(failure(Stage::Plan, FailureKind::UnsupportedPoison));
    }
    if !plan
        .step1
        .output
        .iter()
        .any(|o| o.script_pubkey.is_op_return() && o.script_pubkey.len() > 83)
    {
        return Err(failure(Stage::Plan, FailureKind::PoisonMissing));
    }
    Ok(())
}

/// The anchor, unlike height-only /status, already binds RDTS and MTP to the same
/// hash. Keep that identity; no independent timestamp or schedule inference.
pub fn project_anchor(
    status: NetworkAnchorStatus,
    chain: ChainId,
    txid: Txid,
    presence: ForkTransactionPresence,
    policy: Policy,
    now: i64,
) -> Result<(ForkObservation, DeploymentObservation), Failure> {
    let anchor = checked_anchor(status, chain, policy, now)?;
    let RdtsStatus::Flagday { flagday } = anchor.observation.rdts else {
        return Err(failure(Stage::ForkAnchor, FailureKind::Malformed));
    };
    let tip = BlockRef {
        height: anchor.tip_height,
        hash: anchor.tip_hash,
    };
    Ok((
        ForkObservation {
            chain,
            step1_txid: txid,
            step1_presence: presence,
            tip,
            median_time_past: anchor.tip_median_time_past,
            observed_at: anchor.observed_at,
        },
        DeploymentObservation {
            chain,
            tip,
            state: DeploymentState::Flagday {
                height: flagday.height,
                expiry_time: flagday.expiry_time,
                active: flagday.active,
            },
            observed_at: anchor.observed_at,
        },
    ))
}
fn checked_anchor(
    status: NetworkAnchorStatus,
    chain: ChainId,
    policy: Policy,
    now: i64,
) -> Result<NetworkAnchor, Failure> {
    let bad = |kind| failure(Stage::ForkAnchor, kind);
    if !chain.is_blake2b() || status.network != chain {
        return Err(bad(FailureKind::WrongChain));
    }
    if status.state != AnchorState::Available {
        return Err(bad(if status.anchor.is_some() {
            FailureKind::Malformed
        } else {
            FailureKind::Anchor(status.state)
        }));
    }
    let a = status.anchor.ok_or_else(|| bad(FailureKind::Malformed))?;
    if a.tip_median_time_past < 0
        || a.observation.tip_height != a.tip_height
        || !a
            .observation
            .fork
            .as_ref()
            .is_some_and(|f| f.active && a.tip_height >= f.height)
        || !matches!(&a.observation.rdts, RdtsStatus::Flagday { flagday } if flagday.expiry_time > 0)
    {
        return Err(bad(FailureKind::Malformed));
    }
    if policy.max_observation_age_seconds <= 0
        || !fresh(a.observed_at, now, policy.max_observation_age_seconds)
    {
        return Err(bad(FailureKind::Stale));
    }
    Ok(a)
}

/// No retry loop: a changed view must replace the previous result and be
/// recollected by a caller. Dropping this future also drops all source futures.
pub async fn collect(
    source: &dyn ObservationSource,
    plan: &ClaimPlan,
    policy: Policy,
    budget: Duration,
    mut context: CollectionContext,
) -> Result<CollectedAssessment, Failure> {
    validate_plan(plan, policy, budget)?;
    let expected = context.expected_generation;
    if *context.generation.borrow() != expected || context.generation.has_changed().is_err() {
        return Err(failure(Stage::Context, FailureKind::Cancelled));
    }
    let cancelled = async {
        loop {
            if context.generation.changed().await.is_err()
                || *context.generation.borrow_and_update() != expected
            {
                break;
            }
        }
    };
    let result = tokio::select! {
        biased;
        _ = cancelled => Err(failure(Stage::Context, FailureKind::Cancelled)),
        result = tokio::time::timeout(budget, collect_inner(source, plan, policy, expected)) => {
            result.map_err(|_| failure(Stage::Context, FailureKind::Deadline))?
        }
    };
    if *context.generation.borrow() != expected || context.generation.has_changed().is_err() {
        return Err(failure(Stage::Context, FailureKind::Cancelled));
    }
    result
}

async fn collect_inner(
    source: &dyn ObservationSource,
    plan: &ClaimPlan,
    policy: Policy,
    generation: u64,
) -> Result<CollectedAssessment, Failure> {
    let btc = plan.bitcoin_chain;
    let fork = plan.fork_chain;
    let txid = plan.step1.compute_txid();
    let (tip, mut btc_stamp) = read(
        source
            .tip(btc)
            .await
            .map_err(|e| failure(Stage::BitcoinTip, e))?,
        btc,
        source,
        policy,
        Stage::BitcoinTip,
    )?;
    let first = checked_anchor(
        source
            .anchor(fork)
            .await
            .map_err(|e| failure(Stage::ForkAnchor, e))?,
        fork,
        policy,
        source.now(),
    )?;
    let fork_tip = BlockRef {
        height: first.tip_height,
        hash: first.tip_hash,
    };
    let mut fork_stamp = first.observed_at;
    let mut location = TransactionLocation::Unknown;
    let mut presence = ForkTransactionPresence::Unknown;
    let mut prior_btc_tx = None;
    let mut prior_fork_tx = None;
    for round in 0..2 {
        let (hash, stamp) = read(
            source
                .hash_at_height(fork, fork_tip.height)
                .await
                .map_err(|e| failure(Stage::ForkIndexer, e))?,
            fork,
            source,
            policy,
            Stage::ForkIndexer,
        )?;
        fork_stamp = fork_stamp.min(stamp);
        if hash != fork_tip.hash {
            return Err(failure(Stage::ForkIndexer, FailureKind::Changed));
        }
        let (btx, stamp) = read(
            source
                .transaction(btc, txid)
                .await
                .map_err(|e| failure(Stage::BitcoinTransaction, e))?,
            btc,
            source,
            policy,
            Stage::BitcoinTransaction,
        )?;
        btc_stamp = btc_stamp.min(stamp);
        let (ftx, stamp) = read(
            source
                .transaction(fork, txid)
                .await
                .map_err(|e| failure(Stage::ForkTransaction, e))?,
            fork,
            source,
            policy,
            Stage::ForkTransaction,
        )?;
        fork_stamp = fork_stamp.min(stamp);
        for (tx, stage) in [
            (btx, Stage::BitcoinTransaction),
            (ftx, Stage::ForkTransaction),
        ] {
            if matches!(tx, TransactionObservation::Unconfirmed { txid: id } |
                TransactionObservation::Confirmed { txid: id, .. } if id != txid)
            {
                return Err(failure(stage, FailureKind::Malformed));
            }
        }
        if round == 1 && (prior_btc_tx != Some(btx) || prior_fork_tx != Some(ftx)) {
            return Err(failure(Stage::Preflight, FailureKind::Changed));
        }
        prior_btc_tx = Some(btx);
        prior_fork_tx = Some(ftx);
        presence = if ftx == TransactionObservation::Absent {
            ForkTransactionPresence::NotObserved
        } else {
            ForkTransactionPresence::Present
        };
        location = match btx {
            TransactionObservation::Absent | TransactionObservation::Unconfirmed { .. } => {
                TransactionLocation::Unconfirmed
            }
            TransactionObservation::Confirmed { txid, block } => {
                let (hash, stamp) = read(
                    source
                        .hash_at_height(btc, block.height)
                        .await
                        .map_err(|e| failure(Stage::BitcoinInclusion, e))?,
                    btc,
                    source,
                    policy,
                    Stage::BitcoinInclusion,
                )?;
                btc_stamp = btc_stamp.min(stamp);
                if round == 1
                    && matches!(location, TransactionLocation::Confirmed { best_chain_hash_at_height, .. } if best_chain_hash_at_height != hash)
                {
                    return Err(failure(Stage::Preflight, FailureKind::Changed));
                }
                TransactionLocation::Confirmed {
                    txid,
                    block,
                    best_chain_hash_at_height: hash,
                }
            }
        };
    }
    let (after, stamp) = read(
        source
            .tip(btc)
            .await
            .map_err(|e| failure(Stage::BitcoinTip, e))?,
        btc,
        source,
        policy,
        Stage::BitcoinTip,
    )?;
    btc_stamp = btc_stamp.min(stamp);
    let last = checked_anchor(
        source
            .anchor(fork)
            .await
            .map_err(|e| failure(Stage::ForkAnchor, e))?,
        fork,
        policy,
        source.now(),
    )?;
    if tip != after
        || first.tip_hash != last.tip_hash
        || first.tip_height != last.tip_height
        || first.tip_median_time_past != last.tip_median_time_past
        || first.observation != last.observation
    {
        return Err(failure(Stage::Preflight, FailureKind::Changed));
    }
    let (mut fork_observation, deployment) = project_anchor(
        NetworkAnchorStatus {
            network: fork,
            state: AnchorState::Available,
            anchor: Some(first),
        },
        fork,
        txid,
        presence,
        policy,
        source.now(),
    )?;
    fork_observation.observed_at = fork_stamp.min(last.observed_at);
    let bitcoin = BitcoinObservation {
        chain: btc,
        tip,
        location,
        observed_at: btc_stamp,
    };
    let preflight = PreflightTips {
        bitcoin: after,
        fork: fork_tip,
    };
    let observations = ObservationBundle {
        bitcoin,
        fork: fork_observation,
        deployment,
        preflight,
    };
    let assessment = claim::assess(
        plan,
        bitcoin,
        fork_observation,
        deployment,
        policy,
        source.now(),
        Some(preflight),
    );
    Ok(CollectedAssessment {
        generation,
        observations,
        assessment,
    })
}

#[cfg(test)]
mod tests;
