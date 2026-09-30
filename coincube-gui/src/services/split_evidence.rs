//! Per-outpoint chain evidence for Split step 1 that does not depend on the
//! Bitcoin-side spent state (#568 B1a).
//!
//! The two-chain inventory proves coins through the unspent-output scan, so it
//! cannot describe a coin after step 1 has spent it on Bitcoin. Restart and
//! reconstruction instead authenticate each recorded outpoint directly, with
//! fresh (`no-store`) Connect Esplora reads:
//!
//! - the previous transaction, whose txid must equal the outpoint's;
//! - its confirming block on Bitcoin and on BTCB2, each still the block at that
//!   height in the chain's best chain, both below the observed fork height and
//!   the same block on both chains (shared pre-fork history);
//! - the outpoint still unspent on BTCB2 (step 2 needs it there).
//!
//! The Bitcoin outspend is deliberately never read: a coin spent by step 1 on
//! Bitcoin authenticates the same as an unspent one. Both tips are read before
//! and after and must not move. Every read must be fresh within the caller's
//! age bound. The result is [`SplitCoin`] input for
//! `coincube_core::foreign_split`; it proves nothing about scripts (the core
//! construction checks those), grants no spend or broadcast authority and is
//! stale as soon as it is returned.

use std::{collections::BTreeSet, convert::TryFrom, fmt, time::Duration};

use async_trait::async_trait;
use coincube_core::{
    chain::ChainId,
    claim::BlockRef,
    foreign_split::{SplitBranch, SplitCoin},
    miniscript::bitcoin::{self, BlockHash, OutPoint, Transaction, Txid},
};
use reqwest::header::{HeaderMap, CACHE_CONTROL};
use serde::Deserialize;
use tokio::sync::watch;

use super::{
    claim_observation::{
        http::HttpObservationSource, CollectionContext, FailureKind, FreshRead, ObservationSource,
        TransactionObservation,
    },
    coincube::CoincubeClient,
};

/// Default freshness bound, the same as Claim's review policy.
pub const MAX_EVIDENCE_AGE_SECONDS: i64 = 90;
/// Most outpoints one call authenticates.
pub const MAX_OUTPOINTS: usize = 250;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// A previous transaction up to 1 MB, hex encoded.
const TRANSACTION_HEX_LIMIT: usize = 2 * 1_000_000;
const JSON_LIMIT: usize = 64 * 1024;

/// One recorded step-1 input: where it is and which wallet address it pays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordedOutpoint {
    pub outpoint: OutPoint,
    pub branch: SplitBranch,
    pub index: u32,
}

/// The outpoint's spent state on one chain, from a fresh read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outspend {
    Unspent,
    /// Spent, confirmed or in the mempool.
    Spent,
}

/// Fresh chain reads, one immutable Connect context. Implementations bind
/// every read to the chain asked for and fail rather than report absence.
#[async_trait]
pub trait SplitEvidenceSource: Send + Sync {
    fn now(&self) -> i64;
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
    /// The full previous transaction. Implementations need not check its txid;
    /// [`authenticate_outpoints`] does.
    async fn previous_transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<Transaction>, FailureKind>;
    async fn outspend(
        &self,
        chain: ChainId,
        outpoint: OutPoint,
    ) -> Result<FreshRead<Outspend>, FailureKind>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceFailure {
    Empty,
    TooMany,
    Duplicate,
    InvalidPolicy,
    /// A read failed, or was not fresh within the bound.
    Read(ChainId, FailureKind),
    Stale(ChainId),
    /// The served transaction is not the outpoint's.
    TxidMismatch,
    /// The previous transaction has no such output.
    MissingOutput,
    /// Absent or unconfirmed on this chain.
    NotConfirmed(ChainId),
    /// The confirming block is not the chain's block at that height.
    NotInBestChain(ChainId),
    /// Confirmed at or after the fork height.
    PostFork,
    /// The chains name different confirming blocks.
    ChainsDisagree,
    /// Already spent on BTCB2: step 2 could not spend it.
    Btcb2Spent,
    /// A tip moved while collecting.
    Changed(ChainId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceError {
    /// The outpoint being authenticated, when the failure is specific to one.
    pub outpoint: Option<OutPoint>,
    pub failure: EvidenceFailure,
}

impl fmt::Display for EvidenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.outpoint {
            Some(outpoint) => write!(
                f,
                "Coin {outpoint} could not be authenticated: {:?}",
                self.failure
            ),
            None => write!(f, "Split evidence refused: {:?}", self.failure),
        }
    }
}
impl std::error::Error for EvidenceError {}

fn refuse(outpoint: Option<OutPoint>, failure: EvidenceFailure) -> EvidenceError {
    EvidenceError { outpoint, failure }
}

/// Authenticated step-1 inputs and the tips they were read at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedOutpoints {
    pub coins: Vec<SplitCoin>,
    pub bitcoin_tip: BlockRef,
    pub btcb2_tip: BlockRef,
}

/// Authenticate every recorded outpoint afresh (see the module
/// documentation). `fork_height` must come from the authenticated BTCB2
/// network anchor. Any failure refuses the whole set.
pub async fn authenticate_outpoints(
    source: &dyn SplitEvidenceSource,
    recorded: &[RecordedOutpoint],
    fork_height: u64,
    max_age_seconds: i64,
) -> Result<AuthenticatedOutpoints, EvidenceError> {
    if recorded.is_empty() {
        return Err(refuse(None, EvidenceFailure::Empty));
    }
    if recorded.len() > MAX_OUTPOINTS {
        return Err(refuse(None, EvidenceFailure::TooMany));
    }
    if max_age_seconds <= 0 || fork_height == 0 {
        return Err(refuse(None, EvidenceFailure::InvalidPolicy));
    }
    let mut seen = BTreeSet::new();
    if let Some(duplicate) = recorded.iter().find(|coin| !seen.insert(coin.outpoint)) {
        return Err(refuse(Some(duplicate.outpoint), EvidenceFailure::Duplicate));
    }
    let fresh = |chain: ChainId, observed_at: i64, outpoint: Option<OutPoint>| {
        let now = source.now();
        match now.checked_sub(observed_at) {
            Some(age) if observed_at >= 0 && (0..=max_age_seconds).contains(&age) => Ok(()),
            _ => Err(refuse(outpoint, EvidenceFailure::Stale(chain))),
        }
    };
    let tip = |chain: ChainId| async move {
        let read = source
            .tip(chain)
            .await
            .map_err(|kind| refuse(None, EvidenceFailure::Read(chain, kind)))?;
        fresh(chain, read.observed_at(), None)?;
        Ok::<_, EvidenceError>(*read.value())
    };
    let (bitcoin_tip, btcb2_tip) = (
        tip(ChainId::Bitcoin).await?,
        tip(ChainId::BitcoinBlake2b).await?,
    );

    let mut coins = Vec::with_capacity(recorded.len());
    for coin in recorded {
        let at = Some(coin.outpoint);
        let read = |chain| move |kind| refuse(at, EvidenceFailure::Read(chain, kind));

        let previous = source
            .previous_transaction(ChainId::Bitcoin, coin.outpoint.txid)
            .await
            .map_err(read(ChainId::Bitcoin))?;
        fresh(ChainId::Bitcoin, previous.observed_at(), at)?;
        let previous = previous.value().clone();
        if previous.compute_txid() != coin.outpoint.txid {
            return Err(refuse(at, EvidenceFailure::TxidMismatch));
        }
        if usize::try_from(coin.outpoint.vout)
            .ok()
            .and_then(|vout| previous.output.get(vout))
            .is_none()
        {
            return Err(refuse(at, EvidenceFailure::MissingOutput));
        }

        let mut blocks = Vec::with_capacity(2);
        for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
            let status = source
                .transaction(chain, coin.outpoint.txid)
                .await
                .map_err(read(chain))?;
            fresh(chain, status.observed_at(), at)?;
            let block = match *status.value() {
                TransactionObservation::Confirmed { txid, block } if txid == coin.outpoint.txid => {
                    block
                }
                TransactionObservation::Confirmed { .. } => {
                    return Err(refuse(at, EvidenceFailure::TxidMismatch))
                }
                TransactionObservation::Absent | TransactionObservation::Unconfirmed { .. } => {
                    return Err(refuse(at, EvidenceFailure::NotConfirmed(chain)))
                }
            };
            if block.height >= fork_height {
                return Err(refuse(at, EvidenceFailure::PostFork));
            }
            let canonical = source
                .hash_at_height(chain, block.height)
                .await
                .map_err(read(chain))?;
            fresh(chain, canonical.observed_at(), at)?;
            if *canonical.value() != block.hash {
                return Err(refuse(at, EvidenceFailure::NotInBestChain(chain)));
            }
            blocks.push(block);
        }
        let (bitcoin_block, btcb2_block) = (blocks[0], blocks[1]);
        if bitcoin_block != btcb2_block {
            return Err(refuse(at, EvidenceFailure::ChainsDisagree));
        }

        let spent = source
            .outspend(ChainId::BitcoinBlake2b, coin.outpoint)
            .await
            .map_err(read(ChainId::BitcoinBlake2b))?;
        fresh(ChainId::BitcoinBlake2b, spent.observed_at(), at)?;
        if *spent.value() != Outspend::Unspent {
            return Err(refuse(at, EvidenceFailure::Btcb2Spent));
        }

        coins.push(SplitCoin {
            outpoint: coin.outpoint,
            branch: coin.branch,
            index: coin.index,
            previous,
            bitcoin_block: Some(bitcoin_block),
            btcb2_block: Some(btcb2_block),
        });
    }

    for (chain, before) in [
        (ChainId::Bitcoin, bitcoin_tip),
        (ChainId::BitcoinBlake2b, btcb2_tip),
    ] {
        if tip(chain).await? != before {
            return Err(refuse(None, EvidenceFailure::Changed(chain)));
        }
    }
    Ok(AuthenticatedOutpoints {
        coins,
        bitcoin_tip,
        btcb2_tip,
    })
}

/// Anonymous, fresh-only reads from Connect's Esplora proxy for the two
/// routes the Claim observation source has no method for (previous
/// transaction bytes and outspends) and for the Bitcoin fee estimate. No
/// account header is sent, redirects are never followed, every response must
/// acknowledge the fresh-read contract (`X-Coincube-Observation: fresh`,
/// `X-Cache: BYPASS`, `Cache-Control: no-store`), and a changed generation
/// cancels an in-flight read.
pub struct ConnectEsplora {
    anonymous: reqwest::Client,
    base: String,
    expected: u64,
    generation: watch::Receiver<u64>,
}

impl ConnectEsplora {
    pub fn new(client: &CoincubeClient, context: CollectionContext) -> Result<Self, FailureKind> {
        let url = reqwest::Url::parse(&client.base_url).map_err(|_| FailureKind::Malformed)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(FailureKind::Malformed);
        }
        Ok(Self {
            anonymous: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(REQUEST_TIMEOUT)
                .build()
                .map_err(|_| FailureKind::Unavailable)?,
            base: url.as_str().trim_end_matches('/').to_owned(),
            expected: context.expected_generation,
            generation: context.generation,
        })
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
            .unwrap_or(-1)
    }

    /// One fresh GET. Only 200 is a value; 404 and every other status fail.
    pub async fn fresh(
        &self,
        chain: ChainId,
        path: &str,
        limit: usize,
    ) -> Result<FreshRead<Vec<u8>>, FailureKind> {
        let prefix = match chain {
            ChainId::Bitcoin => "bitcoin/mainnet",
            ChainId::BitcoinBlake2b => "bitcoin-blake2b/mainnet",
            _ => return Err(FailureKind::WrongChain),
        };
        let mut generation = self.generation.clone();
        if *generation.borrow() != self.expected || generation.has_changed().is_err() {
            return Err(FailureKind::Cancelled);
        }
        let cancelled = async {
            loop {
                if generation.changed().await.is_err()
                    || *generation.borrow_and_update() != self.expected
                {
                    break;
                }
            }
        };
        let read = async {
            let stamp = Self::now();
            let mut response = self
                .anonymous
                .get(format!("{}/api/v1/esplora/{prefix}/{path}", self.base))
                .header(CACHE_CONTROL, "no-cache")
                .header("X-Coincube-Observation", "fresh")
                .send()
                .await
                .map_err(|_| FailureKind::Unavailable)?;
            let status = response.status().as_u16();
            if status != 200 {
                return Err(FailureKind::Http(status));
            }
            let headers: HeaderMap = response.headers().clone();
            let mut markers = headers.get_all("x-coincube-observation").iter();
            if markers.next().and_then(|v| v.to_str().ok()) != Some("fresh")
                || markers.next().is_some()
            {
                return Err(FailureKind::FreshnessUnverified);
            }
            FreshRead::from_response(chain, (), stamp, &headers)?;
            if response.content_length().is_some_and(|n| n > limit as u64) {
                return Err(FailureKind::Malformed);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| FailureKind::Unavailable)?
            {
                if chunk.len() > limit.saturating_sub(bytes.len()) {
                    return Err(FailureKind::Malformed);
                }
                bytes.extend_from_slice(&chunk);
            }
            FreshRead::from_response(chain, bytes, stamp, &headers)
        };
        let result = tokio::select! { biased;
            _ = cancelled => Err(FailureKind::Cancelled),
            result = tokio::time::timeout(REQUEST_TIMEOUT, read) => {
                result.map_err(|_| FailureKind::Deadline)?
            }
        };
        if *self.generation.borrow() != self.expected || self.generation.has_changed().is_err() {
            return Err(FailureKind::Cancelled);
        }
        result
    }

    /// The full transaction, strict hex. The caller checks its txid.
    pub async fn transaction_bytes(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<Transaction>, FailureKind> {
        let read = self
            .fresh(chain, &format!("tx/{txid}/hex"), TRANSACTION_HEX_LIMIT)
            .await?;
        let raw = hex::decode(read.value()).map_err(|_| FailureKind::Malformed)?;
        let transaction: Transaction =
            bitcoin::consensus::deserialize(&raw).map_err(|_| FailureKind::Malformed)?;
        FreshRead::from_response(chain, transaction, read.observed_at(), &fresh_headers())
    }

    pub async fn outspend(
        &self,
        chain: ChainId,
        outpoint: OutPoint,
    ) -> Result<FreshRead<Outspend>, FailureKind> {
        #[derive(Deserialize)]
        struct Body {
            spent: bool,
        }
        let read = self
            .fresh(
                chain,
                &format!("tx/{}/outspend/{}", outpoint.txid, outpoint.vout),
                JSON_LIMIT,
            )
            .await?;
        let body: Body =
            serde_json::from_slice(read.value()).map_err(|_| FailureKind::Malformed)?;
        FreshRead::from_response(
            chain,
            if body.spent {
                Outspend::Spent
            } else {
                Outspend::Unspent
            },
            read.observed_at(),
            &fresh_headers(),
        )
    }

    /// Six-block fee estimate, rounded up, in sat/vB.
    pub async fn fee_rate(&self, chain: ChainId) -> Result<u64, FailureKind> {
        let read = self.fresh(chain, "fee-estimates", JSON_LIMIT).await?;
        let quotes: std::collections::BTreeMap<String, f64> =
            serde_json::from_slice(read.value()).map_err(|_| FailureKind::Malformed)?;
        let rate = quotes
            .get("6")
            .copied()
            .ok_or(FailureKind::Malformed)?
            .ceil();
        if !rate.is_finite() || rate < 1.0 || rate >= u64::MAX as f64 {
            return Err(FailureKind::Malformed);
        }
        Ok(rate as u64)
    }
}

/// Re-wraps a value decoded from an already acknowledged fresh response.
fn fresh_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("x-cache", "BYPASS".parse().expect("static header"));
    headers.insert(CACHE_CONTROL, "no-store".parse().expect("static header"));
    headers
}

/// Production source: the Claim observation source for tips, transaction
/// status and block hashes, and [`ConnectEsplora`] for the rest. Both share
/// the caller's generation.
pub struct ConnectSplitEvidence {
    observation: HttpObservationSource,
    esplora: ConnectEsplora,
}

impl ConnectSplitEvidence {
    pub fn new(
        client: CoincubeClient,
        expected: u64,
        generation: watch::Receiver<u64>,
    ) -> Result<Self, FailureKind> {
        let esplora = ConnectEsplora::new(
            &client,
            CollectionContext {
                expected_generation: expected,
                generation: generation.clone(),
            },
        )?;
        let observation = HttpObservationSource::new(
            client,
            ChainId::Bitcoin,
            ChainId::BitcoinBlake2b,
            CollectionContext {
                expected_generation: expected,
                generation,
            },
        )?;
        Ok(Self {
            observation,
            esplora,
        })
    }
}

#[async_trait]
impl SplitEvidenceSource for ConnectSplitEvidence {
    fn now(&self) -> i64 {
        self.observation.now()
    }
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        self.observation.tip(chain).await
    }
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        self.observation.transaction(chain, txid).await
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        self.observation.hash_at_height(chain, height).await
    }
    async fn previous_transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<Transaction>, FailureKind> {
        self.esplora.transaction_bytes(chain, txid).await
    }
    async fn outspend(
        &self,
        chain: ChainId,
        outpoint: OutPoint,
    ) -> Result<FreshRead<Outspend>, FailureKind> {
        self.esplora.outspend(chain, outpoint).await
    }
}

#[cfg(test)]
mod tests;
