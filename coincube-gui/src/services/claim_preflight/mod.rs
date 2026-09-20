//! Dormant anonymous operator-node policy evidence, never spend permission.
use crate::services::{
    claim_observation::CollectionContext, coincube::network_anchor::AnchorState,
};
use coincube_core::{
    chain::ChainId,
    miniscript::bitcoin::{consensus, BlockHash, Transaction, Txid, Wtxid},
};
use reqwest::{
    header::{HeaderMap, CACHE_CONTROL, RETRY_AFTER},
    StatusCode,
};
use serde::{Deserialize, Serialize};
use std::convert::TryFrom;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

const MAX_RESPONSE_BYTES: usize = 16 * 1024;
const DEADLINE: Duration = Duration::from_secs(15);
pub const MAX_FUTURE_SKEW_SECONDS: i64 = 5;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreshnessPolicy {
    pub max_age_seconds: i64,
    /// Explicit caller allowance, 1..=5 seconds. No default or receipt-time restamp.
    pub max_future_skew_seconds: i64,
}
impl FreshnessPolicy {
    fn valid(self) -> bool {
        self.max_age_seconds > 0
            && (1..=MAX_FUTURE_SKEW_SECONDS).contains(&self.max_future_skew_seconds)
    }
    fn fresh(self, stamp: i64, now: i64) -> bool {
        self.valid()
            && stamp >= 0
            && now >= 0
            && now.checked_sub(stamp).is_some_and(|age| {
                age >= -self.max_future_skew_seconds && age <= self.max_age_seconds
            })
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    UnsupportedChain,
    InvalidRequest,
    InvalidResponse,
    MissingNoStore,
    ResponseTooLarge,
    Transport,
    Deadline,
    Cancelled,
    Stale,
    Http {
        status: u16,
        retry_after_seconds: Option<u64>,
    },
    Service {
        state: AnchorState,
        retry_after_seconds: Option<u64>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodePolicy {
    Accepted,
    Rejected { reason: String },
}
/// Checked binding to one submitted transaction/context. Accepted means only
/// that the configured operator node observed acceptable mempool policy then.
/// No serialization or public constructor, and no broadcast/signing capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    chain: ChainId,
    txid: Txid,
    wtxid: Wtxid,
    tip: BlockHash,
    observed_at: i64,
    generation: u64,
    policy: NodePolicy,
}
impl Evidence {
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    pub fn txid(&self) -> Txid {
        self.txid
    }
    pub fn wtxid(&self) -> Wtxid {
        self.wtxid
    }
    pub fn tip(&self) -> BlockHash {
        self.tip
    }
    pub fn observed_at(&self) -> i64 {
        self.observed_at
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn node_policy(&self) -> &NodePolicy {
        &self.policy
    }
}
/// Separate anonymous transport: never accepts an authenticated client/token,
/// never installs default account/device headers, never redirects or retries.
pub struct PreflightClient {
    client: reqwest::Client,
    origin: String,
    generation: watch::Receiver<u64>,
    expected: u64,
}
#[derive(Serialize)]
struct Request {
    transaction: String,
    tip_hash: BlockHash,
}
#[derive(Deserialize)]
struct Envelope {
    success: bool,
    data: Option<Data>,
    error: Option<ApiError>,
}
#[derive(Deserialize)]
struct ApiError {
    code: String,
}
#[derive(Deserialize)]
struct Data {
    network: String,
    state: AnchorState,
    result: Option<WireResult>,
}
#[derive(Deserialize)]
struct WireResult {
    txid: Txid,
    wtxid: Wtxid,
    tip_hash: BlockHash,
    observed_at: i64,
    allowed: bool,
    reject_reason: Option<String>,
}
fn request_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Deadline
    } else {
        Error::Transport
    }
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(-1)
}
fn route(chain: ChainId) -> Result<&'static str, Error> {
    match chain {
        ChainId::Bitcoin => Ok("bitcoin/mainnet"),
        ChainId::BitcoinBlake2b => Ok("bitcoin-blake2b/mainnet"),
        _ => Err(Error::UnsupportedChain),
    }
}
fn no_store(headers: &HeaderMap) -> bool {
    headers
        .get_all(CACHE_CONTROL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|v| v.trim().eq_ignore_ascii_case("no-store"))
}
fn retry_after(headers: &HeaderMap) -> Option<u64> {
    let mut values = headers.get_all(RETRY_AFTER).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}
impl PreflightClient {
    pub fn new(origin: &str, context: CollectionContext) -> Result<Self, Error> {
        let url = reqwest::Url::parse(origin).map_err(|_| Error::InvalidRequest)?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(Error::InvalidRequest);
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(DEADLINE)
            .build()
            .map_err(|_| Error::Transport)?;
        Ok(Self {
            client,
            origin: url.as_str().trim_end_matches('/').to_owned(),
            generation: context.generation,
            expected: context.expected_generation,
        })
    }
    /// Caller supplies the actual finalized transaction; this reader cannot prove
    /// final witness validity without prevouts/descriptors and does not infer it.
    pub async fn observe(
        &self,
        chain: ChainId,
        transaction: &Transaction,
        expected_tip: BlockHash,
        policy: FreshnessPolicy,
    ) -> Result<Evidence, Error> {
        let path = route(chain)?;
        if !policy.valid()
            || transaction.input.is_empty()
            || transaction.output.is_empty()
            || transaction.weight().to_wu() > 400_000
        {
            return Err(Error::InvalidRequest);
        }
        let mut generation = self.generation.clone();
        if *generation.borrow() != self.expected || generation.has_changed().is_err() {
            return Err(Error::Cancelled);
        }
        let request = Request {
            transaction: consensus::encode::serialize_hex(transaction),
            tip_hash: expected_tip,
        };
        let cancelled = async {
            loop {
                if generation.changed().await.is_err()
                    || *generation.borrow_and_update() != self.expected
                {
                    break;
                }
            }
        };
        let operation = async {
            let mut response = self
                .client
                .post(format!(
                    "{}/api/v1/esplora/{}/tx/preflight",
                    self.origin, path
                ))
                .header(CACHE_CONTROL, "no-cache")
                .json(&request)
                .send()
                .await
                .map_err(request_error)?;
            let status = response.status();
            let headers = response.headers().clone();
            if !no_store(&headers) {
                return Err(Error::MissingNoStore);
            }
            if response
                .content_length()
                .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
            {
                return Err(Error::ResponseTooLarge);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(request_error)? {
                if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
                    return Err(Error::ResponseTooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            parse(
                status,
                &headers,
                &bytes,
                chain,
                transaction,
                expected_tip,
                self.expected,
                policy,
                now(),
            )
        };
        let result = tokio::select! {biased;
            _=cancelled=>Err(Error::Cancelled),
            result=tokio::time::timeout(DEADLINE,operation)=>result.map_err(|_|Error::Deadline)?,
        };
        if *generation.borrow() != self.expected || generation.has_changed().is_err() {
            return Err(Error::Cancelled);
        }
        result
    }
}
#[allow(clippy::too_many_arguments)]
fn parse(
    status: StatusCode,
    headers: &HeaderMap,
    bytes: &[u8],
    chain: ChainId,
    tx: &Transaction,
    tip: BlockHash,
    generation: u64,
    policy: FreshnessPolicy,
    now: i64,
) -> Result<Evidence, Error> {
    let retry_after_seconds = retry_after(headers);
    if status != StatusCode::OK && status != StatusCode::SERVICE_UNAVAILABLE {
        return Err(Error::Http {
            status: status.as_u16(),
            retry_after_seconds,
        });
    }
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| Error::InvalidResponse)?;
    if status == StatusCode::SERVICE_UNAVAILABLE {
        if envelope.success
            || !envelope
                .error
                .is_some_and(|e| e.code == "SERVICE_UNAVAILABLE")
        {
            return Err(Error::InvalidResponse);
        }
        let Some(data) = envelope.data else {
            return Err(Error::Http {
                status: 503,
                retry_after_seconds,
            });
        };
        if data.network != chain.api_str()
            || data.state == AnchorState::Available
            || data.result.is_some()
        {
            return Err(Error::InvalidResponse);
        }
        return Err(Error::Service {
            state: data.state,
            retry_after_seconds,
        });
    }
    if !envelope.success || envelope.error.is_some() {
        return Err(Error::InvalidResponse);
    }
    let data = envelope.data.ok_or(Error::InvalidResponse)?;
    if data.network != chain.api_str() || data.state != AnchorState::Available {
        return Err(Error::InvalidResponse);
    }
    let result = data.result.ok_or(Error::InvalidResponse)?;
    if result.txid != tx.compute_txid()
        || result.wtxid != tx.compute_wtxid()
        || result.tip_hash != tip
    {
        return Err(Error::InvalidResponse);
    }
    if !policy.fresh(result.observed_at, now) {
        return Err(Error::Stale);
    }
    let policy = match (result.allowed, result.reject_reason) {
        (true, None) => NodePolicy::Accepted,
        (false, Some(reason))
            if !reason.is_empty()
                && reason.len() <= 80
                && reason.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                }) =>
        {
            NodePolicy::Rejected { reason }
        }
        _ => return Err(Error::InvalidResponse),
    };
    Ok(Evidence {
        chain,
        txid: result.txid,
        wtxid: result.wtxid,
        tip: result.tip_hash,
        observed_at: result.observed_at,
        generation,
        policy,
    })
}
#[cfg(test)]
mod tests;
