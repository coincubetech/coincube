//! Fresh chain observations for Split coins: input-ancestry proofs and
//! transaction depth on either chain (`services::split_coins`,
//! `services::chain_status`). An observation alone is never signer or
//! broadcast permission.
pub mod http;

use async_trait::async_trait;
use coincube_core::{
    chain::ChainId,
    claim::{BlockRef, Policy},
    miniscript::bitcoin::{BlockHash, Txid},
};
use reqwest::header::{HeaderMap, CACHE_CONTROL};
use std::time::Duration;
use tokio::sync::watch;

use super::coincube::network_anchor::{AnchorState, NetworkAnchor, NetworkAnchorStatus};

pub const MAX_COLLECTION_TIME: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    ForkAnchor,
    BitcoinInclusion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    InvalidPlan,
    UnsupportedPoison,
    /// A fresh, stable canonical view no longer maps the retained dependency
    /// root to Bitcoin at its committed height.
    AncestryRootChanged {
        observed_at: i64,
    },
    /// Fresh, stable views now expose the same coinbase root on both chains.
    AncestryRootShared {
        observed_at: i64,
    },
    WrongChain,
    Malformed,
    FreshnessUnverified,
    Stale,
    Changed,
    Cancelled,
    Deadline,
    CollectionLimit,
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
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    /// The value read, once the fresh-read contract has been acknowledged.
    pub fn value(&self) -> &T {
        &self.value
    }
    /// When the read was made, as the responder stamped it (UNIX seconds).
    pub fn observed_at(&self) -> i64 {
        self.observed_at
    }
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
/// The HTTP adapter requires explicit fresh-response acknowledgement.
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

pub(crate) fn session_current(generation: &watch::Receiver<u64>, expected: u64) -> bool {
    *generation.borrow() == expected && matches!(generation.has_changed(), Ok(false))
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

/// Input ancestry depends on the active fork and a coherent tip; the RDTS
/// window of the removed OP_RETURN construction is not required.
fn checked_ancestry_anchor(
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

#[cfg(test)]
mod tests;
