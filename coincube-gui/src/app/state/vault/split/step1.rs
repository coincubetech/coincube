//! Split step 1 (#568 B1b): everything the panel does that is not iced state.
//!
//! - [`preconditions`]: the checks before anything is built. The destination
//!   is only a `FreshIndex::Proven` receive index, proven unused again on both
//!   chains by fresh Connect reads; the fork height and the poison's fork
//!   marker come from the authenticated anchor, which must agree with the
//!   scan; RDTS must be active with the 36 h margin
//!   (`claim::assess_deployment` under Claim's [`CHECK_POLICY`]); the Bitcoin
//!   fee comes from Connect's Bitcoin Esplora (P3). Any missing piece refuses.
//! - [`build`]: the core construction, with `locktime = ` the scanned Bitcoin
//!   tip height and that tip passed to the builder.
//! - [`import`]: combine returned PSBT files and finalize.
//! - [`restore`]: restart from a journal. The recorded outpoints are
//!   authenticated afresh ([`authenticate_outpoints`]), step 1 is rebuilt from
//!   them ([`reconstruct_split_step1`] at the current tip) and the recorded
//!   signed bytes are verified against that rebuild, so the coordinator
//!   resumes with exactly the recorded transaction. Restart never re-signs and
//!   never submits.
//! - [`check_abandon`]: the chain check that must pass before an unsubmitted
//!   journal may be deleted.
//!
//! Connect reads and the coordinator sit behind [`SplitConnect`] and
//! [`Step1Driver`] so the panel can be driven against a mocked Connect in
//! tests. The production implementations are [`ProductionConnect`] and the
//! claim coordinator's Split route (`Coordinator::create_split` /
//! `resume_split`). Every blocking call here runs off the UI thread.
//!
//! The journal directory is `<btcb2 network>/data/<wallet>/split/<digest>/`
//! under the target Cube's Vault: never a Claim pairing directory
//! (`app/claim_intent.rs` reads any `intent.json` under `<bitcoin>/…/claim`
//! as a Claim in progress).

use std::{
    convert::TryFrom,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use async_trait::async_trait;
use tokio::sync::watch;

use coincube_core::{
    chain::ChainId,
    claim::{Assessment, BlockRef, DeploymentState},
    foreign_split::{
        create_split_step1, finalize_split_step1, reconstruct_split_step1,
        verify_split_step1_transaction, FinalizeError, SplitBranch, SplitCoin, SplitInputs,
        SplitSource, SplitStep1, VerifiedSplitStep1,
    },
    miniscript::bitcoin::{
        absolute::LockTime, hashes::sha256, psbt::Psbt, secp256k1, Address, Network, OutPoint, Txid,
    },
};

use crate::app::settings::WalletId;
use crate::{
    app::{
        split_intent::SplitIntent,
        state::vault::claim::{
            describe_duration, evaluate_anchor, ConnectSession, ForkWindow, CHECK_POLICY,
            EXPIRY_MARGIN_SECONDS,
        },
    },
    dir::CoincubeDirectory,
    services::{
        claim_coordinator::{
            self, split::SplitProduction, Coordinator, Outcome, ReconfirmationReview,
            ResubmissionReview, Review, SubmissionRoute,
        },
        claim_observation::{
            http::HttpObservationSource, CollectionContext, FailureKind, ObservationSource,
            TransactionObservation,
        },
        claim_workflow::{self, Context, Controller, Phase, Status},
        foreign_scan::MAX_ADDRESSES,
        foreign_split_inventory::FreshIndex,
        split_evidence::{
            authenticate_outpoints, ConnectEsplora, ConnectSplitEvidence, EvidenceError,
            EvidenceFailure, RecordedOutpoint, SplitEvidenceSource, MAX_EVIDENCE_AGE_SECONDS,
        },
        split_fees::{bitcoin_step1_feerate, ConnectBitcoinFees},
        split_psbt_file::{self, FileError},
        split_source::split_source,
    },
};

/// Largest address-history response read for a freshness proof.
const ADDRESS_JSON_LIMIT: usize = 64 * 1024;

/// D8: the watch-only rescan fallback is deferred, so a wallet the scan cannot
/// fully prove is refused with this copy rather than rescanned.
pub const WATCH_ONLY_DEFERRED: &str = "Tenshu could not prove a fresh receive address for this wallet within the scanned range. A watch-only rescan of the wallet is not available in this version, so this wallet can't be split here.";
/// P7: a fixed (non-ranged) wallet has one address and no fresh one.
pub const FIXED_WALLET: &str = "This wallet has a single fixed address, so Split has no fresh address of the same wallet to send step 1 to. Wallets with one address can't be split in this version.";
pub const NO_PRE_FORK_COINS: &str = "No coins confirmed before the fork on both chains were found, so there is nothing to split. Coins confirmed after the fork are not part of a split.";
pub const STALE_ANCHOR: &str = "The Bitcoin Blake2b fork height Connect reports now differs from the one this split was built against. Nothing was built or sent; scan the wallet again.";
pub const DESTINATION_USED: &str = "The fresh address Split chose has been used since the scan, so it is no longer fresh. Nothing was built; scan the wallet again.";
pub const COMPLETED: &str = "This split's recorded wallet details were already removed after completion. There is nothing left to resume.";
pub const OTHER_ACCOUNT: &str = "This split was recorded under a different Connect account. Sign in with that account to continue it.";
/// Step 1 left the block it was confirmed in: step 2 is blocked (#568 B2).
pub const REORGED: &str = "Step 1 is no longer in the Bitcoin block it was confirmed in, so step 2 is blocked. Check what happened: step 1 may have been mined again in another block, or dropped from the chain.";
/// Step 1's coins were spent on Bitcoin by another transaction after a
/// reorg: this step 1 can never confirm, so a new one is needed.
pub const NEW_POISON_NEEDED: &str = "Step 1 was dropped from Bitcoin and its coins were since spent there by another transaction, so this step 1 can never confirm and step 2 stays blocked. A new step 1 is needed: scan the wallet again for a fresh inventory.";
/// #625 F2: a recorded step 1 that can't be rebuilt, never sent.
pub const UNREBUILDABLE: &str = "This split's step 1 was never sent and can't be rebuilt, so it can't be sent now. You can abandon it after a check that Bitcoin shows neither it nor any spend of its coins.";
/// #625 F2: a step 1 whose submission is recorded is never abandoned here.
pub const SUBMISSION_RECORDED: &str = "A submission of this split's step 1 is recorded, so it can't be abandoned here. The record is kept.";
/// A session that ended after an abandon or close was confirmed: nothing
/// was deleted or closed (#644 r4176212750).
pub const ENDED_BEFORE_ABANDON: &str = "The split session ended before the split was abandoned, so nothing was deleted or closed. It is recorded on this device and continues after you sign in again.";
/// #625 F2: the journal's identity or recorded inputs can't be established.
pub const UNIDENTIFIED: &str = "The coins this split recorded can't be identified on Bitcoin, so it can't be abandoned here. The record is kept.";

/// Additional navigation advice for a refused operation. Terminal domain
/// errors retain only their own instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalRecovery {
    None,
    ReopenCube,
}

/// Why the flow stops, and whether trying again could change it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub reason: String,
    pub retry: bool,
    pub recovery: RefusalRecovery,
}

impl Refusal {
    pub fn final_(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retry: false,
            recovery: RefusalRecovery::None,
        }
    }
    pub fn retry(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            retry: true,
            recovery: RefusalRecovery::None,
        }
    }
}

/// `<btcb2 network>/data/<wallet>/split`: every Split journal of one target
/// Vault, one directory per source digest.
pub fn journal_root(datadir: &CoincubeDirectory, wallet: &WalletId) -> PathBuf {
    datadir
        .network_directory(ChainId::BitcoinBlake2b)
        .coincubed_data_directory(wallet)
        .path()
        .join("split")
}

pub fn journal_directory(root: &Path, digest: sha256::Hash) -> PathBuf {
    root.join(digest.to_string())
}

/// #625 F2 (A1 = A): the tombstone a split closed in its step-2 dead end
/// leaves in its journal directory (`step2::close`). The journal stays, with
/// the recorded signed step 2, so a new split of the same source is still
/// refused; discovery skips it. Removing this file (or the whole directory)
/// is the owner's explicit reset.
pub const CLOSED: &str = claim_workflow::SPLIT_TOMBSTONE;

/// Whether the journal in `directory` was closed: its tombstone is a
/// regular file (metadata only).
pub fn is_closed(directory: &Path) -> bool {
    std::fs::symlink_metadata(directory.join(CLOSED)).is_ok_and(|metadata| metadata.is_file())
}

/// Existing Split journals under `root`, by source digest, sorted. Only a
/// real directory named by a digest and holding a regular `intent.json`
/// counts, unless it was closed ([`CLOSED`]); symlinks and anything else
/// are ignored. Discovery reads nothing inside the journal (file metadata
/// only): opening it authenticates it.
pub fn discover(root: &Path) -> Vec<(sha256::Hash, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let digest = sha256::Hash::from_str(&name).ok()?;
            if digest.to_string() != name {
                return None;
            }
            let path = entry.path();
            let is_dir = std::fs::symlink_metadata(&path).ok()?.is_dir();
            let journal = std::fs::symlink_metadata(path.join("intent.json")).ok()?;
            (is_dir && journal.is_file() && !is_closed(&path)).then_some((digest, path))
        })
        .collect();
    found.sort();
    found
}

/// What the review shows. A display copy only: the review token stays with
/// the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewView {
    pub txid: Txid,
    pub fee_sats: u64,
    pub vsize: usize,
    pub route: SubmissionRoute,
    pub bitcoin_tip: u64,
    pub fork_tip: u64,
    /// Seconds from the fork's median-time-past to RDTS expiry.
    pub rdts_left: Option<i64>,
}

pub type RevokeHandle = Arc<dyn Fn() + Send + Sync>;

/// One recorded Split step 1 under review, submission or tracking.
#[async_trait]
pub trait Step1Driver: Send {
    fn phase(&self) -> Phase;
    /// Synchronous revocation of the coordinator and any queued submission.
    fn revoke_handle(&self) -> RevokeHandle;
    async fn review(&mut self, context: &Context) -> Result<ReviewView, claim_coordinator::Error>;
    /// Submit exactly what the last review showed. Without a live review it
    /// refuses.
    async fn submit(&mut self, context: &Context) -> Result<Outcome, claim_coordinator::Error>;
    async fn reconcile(&mut self, context: &Context) -> Result<Status, claim_coordinator::Error>;
    /// After a check found step 1 `Reorged` (#568 B2): a fresh review of
    /// what happened. Re-mined in another block: the reconfirmation to
    /// acknowledge. Dropped from the chain: the exact recorded step 1, after a
    /// fresh preflight, to send again. The review stays with the driver.
    async fn recover(&mut self, context: &Context) -> Result<Recovery, claim_coordinator::Error>;
    /// Acknowledge exactly the reconfirmation the last `recover` showed.
    async fn acknowledge(&mut self, context: &Context) -> Result<(), claim_coordinator::Error>;
    /// Send exactly the step 1 the last `recover` reviewed again. Without a
    /// live resend review it refuses.
    async fn resend(&mut self, context: &Context) -> Result<Outcome, claim_coordinator::Error>;
}

/// What a reorg review found. A display copy only: the one-use review token
/// stays with the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// Mined again in `confirmed` instead of `previous`.
    Reconfirmed {
        previous: BlockRef,
        confirmed: BlockRef,
    },
    /// Dropped from the chain; the exact recorded bytes may be sent again.
    Resend(ReviewView),
}

/// Everything `Coordinator::create_split` / `resume_split` takes.
pub struct OpenRequest {
    pub directory: PathBuf,
    pub target_cube: String,
    pub construction: SplitStep1,
    pub verified: VerifiedSplitStep1,
    pub fork_height: u64,
    pub resume: bool,
}

/// The Connect side of the flow, for one account session and generation.
#[async_trait]
pub trait SplitConnect: Send + Sync {
    fn context(&self) -> Context;
    fn evidence(&self) -> &dyn SplitEvidenceSource;
    /// The fork's window from the authenticated anchor, with the fork marker.
    async fn window(&self) -> Result<ForkWindow, String>;
    async fn bitcoin_feerate(&self) -> Option<u64>;
    /// Whether `address` has any chain or mempool history (fresh read).
    async fn address_used(&self, chain: ChainId, address: &str) -> Result<bool, FailureKind>;
    /// Opens or creates the journal. Blocking: callers use `spawn_blocking`.
    fn open(&self, request: OpenRequest) -> Result<Box<dyn Step1Driver>, claim_coordinator::Error>;
}

/// The production Connect side: Claim's observation source and anchor, the
/// Split evidence and fee readers, and the daemonless Split coordinator.
pub struct ProductionConnect {
    session: ConnectSession,
    expected: u64,
    generation: watch::Receiver<u64>,
    context: Context,
    observation: HttpObservationSource,
    esplora: ConnectEsplora,
    evidence: ConnectSplitEvidence,
}

impl ProductionConnect {
    /// Refused without an account, for an unusable origin, or after the
    /// generation moved.
    pub fn new(
        session: ConnectSession,
        generation: watch::Receiver<u64>,
    ) -> Result<Self, claim_coordinator::Error> {
        let expected = *generation.borrow();
        let probe = SplitProduction::new(
            session.client.clone(),
            session.account.clone(),
            expected,
            generation.clone(),
            ChainId::Bitcoin,
        )?;
        let context = probe.context().clone();
        let collection = || CollectionContext {
            expected_generation: expected,
            generation: generation.clone(),
        };
        let observation = HttpObservationSource::new(
            session.client.clone(),
            ChainId::Bitcoin,
            ChainId::BitcoinBlake2b,
            collection(),
        )
        .map_err(|_| claim_coordinator::Error::InvalidBinding)?;
        let esplora = ConnectEsplora::new(&session.client, collection())
            .map_err(|_| claim_coordinator::Error::InvalidBinding)?;
        let evidence =
            ConnectSplitEvidence::new(session.client.clone(), expected, generation.clone())
                .map_err(|_| claim_coordinator::Error::InvalidBinding)?;
        Ok(Self {
            session,
            expected,
            generation,
            context,
            observation,
            esplora,
            evidence,
        })
    }
}

#[async_trait]
impl SplitConnect for ProductionConnect {
    fn context(&self) -> Context {
        self.context.clone()
    }
    fn evidence(&self) -> &dyn SplitEvidenceSource {
        &self.evidence
    }
    async fn window(&self) -> Result<ForkWindow, String> {
        let status = self
            .observation
            .anchor(ChainId::BitcoinBlake2b)
            .await
            .map_err(|kind| format!("{kind:?}"))?;
        let fork_height = status
            .anchor
            .as_ref()
            .and_then(|a| a.observation.fork.as_ref())
            .map(|f| f.height)
            .ok_or_else(|| "the fork's activation height is missing".to_string())?;
        let marker = self
            .observation
            .hash_at_height(ChainId::BitcoinBlake2b, fork_height)
            .await
            .map_err(|kind| format!("{kind:?}"))?;
        evaluate_anchor(status, *marker.value(), self.observation.now())
    }
    async fn bitcoin_feerate(&self) -> Option<u64> {
        let fees = ConnectBitcoinFees::new(&self.session.client)?;
        bitcoin_step1_feerate(&fees).await
    }
    async fn address_used(&self, chain: ChainId, address: &str) -> Result<bool, FailureKind> {
        address_used(&self.esplora, chain, address, self.observation.now()).await
    }
    fn open(&self, request: OpenRequest) -> Result<Box<dyn Step1Driver>, claim_coordinator::Error> {
        let production = SplitProduction::new(
            self.session.client.clone(),
            self.session.account.clone(),
            self.expected,
            self.generation.clone(),
            ChainId::Bitcoin,
        )?;
        let OpenRequest {
            directory,
            target_cube,
            construction,
            verified,
            fork_height,
            resume,
        } = request;
        let coordinator = if resume {
            Coordinator::resume_split(
                &directory,
                target_cube,
                &construction,
                verified,
                fork_height,
                production,
                CHECK_POLICY,
            )?
        } else {
            claim_workflow::prepare_directory(&directory)?;
            Coordinator::create_split(
                &directory,
                target_cube,
                &construction,
                verified,
                fork_height,
                production,
                CHECK_POLICY,
            )?
        };
        Ok(Box::new(CoordinatorDriver {
            coordinator,
            review: None,
            reconfirmation: None,
            resubmission: None,
        }))
    }
}

/// A fresh read of Connect's address summary: used when any confirmed or
/// mempool transaction touches it.
async fn address_used(
    esplora: &ConnectEsplora,
    chain: ChainId,
    address: &str,
    now: i64,
) -> Result<bool, FailureKind> {
    #[derive(serde::Deserialize)]
    struct Stats {
        tx_count: u64,
    }
    #[derive(serde::Deserialize)]
    struct Summary {
        chain_stats: Stats,
        mempool_stats: Stats,
    }
    if address.is_empty() || !address.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(FailureKind::Malformed);
    }
    let read = esplora
        .fresh(chain, &format!("address/{address}"), ADDRESS_JSON_LIMIT)
        .await?;
    let age = now.checked_sub(read.observed_at());
    if !age.is_some_and(|age| {
        (0..=CHECK_POLICY.observations.max_observation_age_seconds).contains(&age)
    }) {
        return Err(FailureKind::Stale);
    }
    let summary: Summary =
        serde_json::from_slice(read.value()).map_err(|_| FailureKind::Malformed)?;
    Ok(summary.chain_stats.tx_count > 0 || summary.mempool_stats.tx_count > 0)
}

struct CoordinatorDriver {
    coordinator: Coordinator,
    review: Option<Review>,
    reconfirmation: Option<ReconfirmationReview>,
    resubmission: Option<ResubmissionReview>,
}

impl CoordinatorDriver {
    fn clear_reviews(&mut self) {
        self.review = None;
        self.reconfirmation = None;
        self.resubmission = None;
    }
}

#[async_trait]
impl Step1Driver for CoordinatorDriver {
    fn phase(&self) -> Phase {
        self.coordinator.phase()
    }
    fn revoke_handle(&self) -> RevokeHandle {
        let revoker = self.coordinator.revoker();
        Arc::new(move || revoker.revoke())
    }
    async fn review(&mut self, context: &Context) -> Result<ReviewView, claim_coordinator::Error> {
        self.clear_reviews();
        let review = self.coordinator.prepare_review(context).await?;
        let view = review_view(review.snapshot());
        self.review = Some(review);
        Ok(view)
    }
    async fn submit(&mut self, context: &Context) -> Result<Outcome, claim_coordinator::Error> {
        let review = self
            .review
            .take()
            .ok_or(claim_coordinator::Error::InvalidReview)?;
        self.coordinator.confirm_and_submit(review, context).await
    }
    async fn reconcile(&mut self, context: &Context) -> Result<Status, claim_coordinator::Error> {
        self.clear_reviews();
        self.coordinator.reconcile(context).await
    }
    async fn recover(&mut self, context: &Context) -> Result<Recovery, claim_coordinator::Error> {
        self.clear_reviews();
        match self.coordinator.prepare_reconfirmation(context).await {
            Ok(review) => {
                let inclusion = review.inclusion();
                self.reconfirmation = Some(review);
                return Ok(Recovery::Reconfirmed {
                    previous: inclusion.previous,
                    confirmed: inclusion.confirmed,
                });
            }
            // Not mined again elsewhere: maybe dropped from the chain.
            Err(claim_coordinator::Error::NotReady(_)) => {}
            Err(error) => return Err(error),
        }
        let review = self.coordinator.prepare_resubmission(context).await?;
        let view = review_view(review.snapshot());
        self.resubmission = Some(review);
        Ok(Recovery::Resend(view))
    }
    async fn acknowledge(&mut self, context: &Context) -> Result<(), claim_coordinator::Error> {
        let review = self
            .reconfirmation
            .take()
            .ok_or(claim_coordinator::Error::InvalidReview)?;
        self.clear_reviews();
        self.coordinator
            .confirm_reconfirmation(review, context)
            .await
    }
    async fn resend(&mut self, context: &Context) -> Result<Outcome, claim_coordinator::Error> {
        let review = self
            .resubmission
            .take()
            .ok_or(claim_coordinator::Error::InvalidReview)?;
        self.clear_reviews();
        self.coordinator.confirm_resubmission(review, context).await
    }
}

fn review_view(snapshot: &claim_coordinator::ReviewSnapshot) -> ReviewView {
    let observations = &snapshot.observations;
    let rdts_left = match observations.deployment.state {
        DeploymentState::Flagday { expiry_time, .. } => {
            Some(expiry_time.saturating_sub(observations.fork.median_time_past))
        }
        _ => None,
    };
    ReviewView {
        txid: snapshot.txid,
        fee_sats: snapshot.fee_sats,
        vsize: snapshot.vsize,
        route: snapshot.route,
        bitcoin_tip: observations.bitcoin.tip.height,
        fork_tip: observations.fork.tip.height,
        rdts_left,
    }
}

/// Copy for an RDTS window that refuses the OP_RETURN poison (D3: 36 h).
pub fn rdts_refusal(assessment: Assessment, window: &ForkWindow) -> String {
    match assessment {
        Assessment::RdtsExpired => "Bitcoin Blake2b's replay protection has expired, so an OP_RETURN split is no longer possible.".to_string(),
        Assessment::ExpiryMargin => format!(
            "Bitcoin Blake2b's replay protection expires too soon for a safe split: about {} left, {} needed. Nothing was built.",
            describe_duration(window.expires_at.saturating_sub(window.median_time_past).max(0)),
            describe_duration(EXPIRY_MARGIN_SECONDS)
        ),
        Assessment::RdtsScheduled | Assessment::RdtsInactive => {
            "Bitcoin Blake2b's replay protection isn't active yet.".to_string()
        }
        other => format!("Bitcoin Blake2b's status couldn't be assessed ({other:?})."),
    }
}

/// The fresh receive address step 1 pays.
fn destination_address(source: &SplitSource, index: u32) -> Result<Address, Refusal> {
    let script = source
        .external()
        .at_derivation_index(index)
        .map_err(|_| Refusal::final_(WATCH_ONLY_DEFERRED))?
        .script_pubkey();
    Address::from_script(&script, Network::Bitcoin).map_err(|_| Refusal::final_(FIXED_WALLET))
}

/// Everything [`build`] needs, all checked.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub source: SplitSource,
    pub coins: Vec<SplitCoin>,
    pub destination: u32,
    pub address: String,
    pub window: ForkWindow,
    pub feerate_vb: u64,
    pub bitcoin_tip_height: u32,
}

/// The checks before anything is built (see the module documentation).
pub async fn preconditions(
    connect: &dyn SplitConnect,
    intent: &SplitIntent,
) -> Result<Prepared, Refusal> {
    let source = split_source(&intent.external, intent.internal.as_ref())
        .map_err(|error| Refusal::final_(error.to_string()))?;
    let inventory = &intent.inventory;
    let destination = match inventory.fresh_receive() {
        FreshIndex::Proven(index) => index,
        FreshIndex::FixedDescriptor => return Err(Refusal::final_(FIXED_WALLET)),
        FreshIndex::NotProven => return Err(Refusal::final_(WATCH_ONLY_DEFERRED)),
    };
    let coins = inventory.splittable_coins();
    if coins.is_empty() {
        return Err(Refusal::final_(NO_PRE_FORK_COINS));
    }
    let window = connect.window().await.map_err(|reason| {
        Refusal::retry(format!(
            "Couldn't read Bitcoin Blake2b's status from Connect ({reason})."
        ))
    })?;
    if window.fork_height != inventory.fork_height() {
        return Err(Refusal::final_(STALE_ANCHOR));
    }
    if let Err(assessment) = window.rdts {
        return Err(Refusal::final_(rdts_refusal(assessment, &window)));
    }
    let feerate_vb = connect.bitcoin_feerate().await.ok_or_else(|| {
        Refusal::retry("Connect has no Bitcoin fee estimate right now, so step 1 can't be priced. Try again shortly.")
    })?;
    let address = destination_address(&source, destination)?.to_string();
    destination_unused(connect, &address).await?;
    Ok(Prepared {
        source,
        coins,
        destination,
        address,
        window,
        feerate_vb,
        bitcoin_tip_height: inventory.bitcoin_tip_height(),
    })
}

/// Fresh Connect proof that `address` has no history on either chain.
pub async fn destination_unused(connect: &dyn SplitConnect, address: &str) -> Result<(), Refusal> {
    for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
        match connect.address_used(chain, address).await {
            Ok(false) => {}
            Ok(true) => return Err(Refusal::final_(DESTINATION_USED)),
            Err(kind) => {
                return Err(Refusal::retry(format!(
                    "Connect couldn't prove the fresh address unused on {} ({kind:?}). Nothing was built or sent.",
                    chain_name(chain)
                )))
            }
        }
    }
    Ok(())
}

/// The address step 1 pays (its output 1), for the review-time proof.
pub fn construction_destination(construction: &SplitStep1) -> Option<String> {
    let output = construction.psbt().unsigned_tx.output.get(1)?;
    Address::from_script(&output.script_pubkey, Network::Bitcoin)
        .ok()
        .map(|address| address.to_string())
}

fn chain_name(chain: ChainId) -> &'static str {
    match chain {
        ChainId::Bitcoin => "Bitcoin",
        _ => "Bitcoin Blake2b",
    }
}

/// The core construction. CPU-bound: run off the UI thread.
pub fn build(prepared: &Prepared) -> Result<SplitStep1, String> {
    let tip = prepared.bitcoin_tip_height;
    let locktime = LockTime::from_height(tip)
        .map_err(|_| "The Bitcoin tip height is not a valid locktime.".to_string())?;
    create_split_step1(
        &SplitInputs {
            chain: ChainId::Bitcoin,
            source: &prepared.source,
            coins: &prepared.coins,
            fork_height: prepared.window.fork_height,
            destination: prepared.destination,
        },
        prepared.feerate_vb,
        locktime,
        tip,
        prepared.window.fork_hash,
    )
    .map_err(|error| format!("Split couldn't build step 1: {error}"))
}

/// Combining every returned file so far.
#[derive(Debug)]
pub enum Imported {
    /// Every input is satisfied: verified and ready to record.
    Complete(Box<VerifiedSplitStep1>, Box<Psbt>),
    /// Valid signatures, not yet enough for every input.
    Partial,
}

pub fn import(construction: &SplitStep1, files: &[Psbt]) -> Result<Imported, FileError> {
    let combined = split_psbt_file::combine(construction, files)?;
    let secp = secp256k1::Secp256k1::verification_only();
    match finalize_split_step1(construction, &combined, &secp) {
        Ok(verified) => Ok(Imported::Complete(Box::new(verified), Box::new(combined))),
        Err(FinalizeError::Unsatisfied) => Ok(Imported::Partial),
        Err(error) => Err(FileError::Refused(error)),
    }
}

/// A journal read back and rebuilt at restart.
#[derive(Debug)]
pub struct Restored {
    pub construction: SplitStep1,
    pub verified: VerifiedSplitStep1,
    pub fork_height: u64,
    pub phase: Phase,
    /// Each claimed prevout and the Bitcoin address it pays, for the
    /// pre-abandon chain check.
    pub claimed: Vec<(OutPoint, String)>,
    /// The claimed coins as authenticated for this restore: step 2 is built
    /// from them (#568 B3b-2b).
    pub coins: Vec<SplitCoin>,
}

fn journal_refusal(error: claim_workflow::Error) -> Refusal {
    match error {
        claim_workflow::Error::WrongIdentity => Refusal::final_(OTHER_ACCOUNT),
        claim_workflow::Error::Busy => Refusal::retry(
            "This split is open in another tab or window. Close it there, then try again.",
        ),
        other => Refusal::final_(format!(
            "The split recorded on this device couldn't be read ({other:?})."
        )),
    }
}

/// Copy for a refused outpoint authentication. A failed read is never called
/// "spent": the BTCB2 indexer refuses addresses that ever held more than its
/// UTXO limit (500 by default), which reads as a Connect error (#615 N1).
pub fn evidence_refusal(error: EvidenceError) -> Refusal {
    match error.failure {
        EvidenceFailure::Btcb2Spent => Refusal::final_(
            "A coin of this split is no longer unspent on Bitcoin Blake2b, so step 2 could not spend it. The recorded step 1 is kept.",
        ),
        EvidenceFailure::Read(chain, kind) => Refusal::retry(format!(
            "Connect couldn't serve a {} read this split needs ({kind:?}). This is a Connect or indexer limit, not a sign that a coin was spent: the Bitcoin Blake2b indexer refuses addresses that have held more than 500 outputs. Try again later.",
            chain_name(chain)
        )),
        EvidenceFailure::Stale(_) | EvidenceFailure::Changed(_) => {
            Refusal::retry("The chains moved while this split was being checked. Try again.")
        }
        EvidenceFailure::PostFork | EvidenceFailure::ChainsDisagree => {
            Refusal::final_("A coin of this split is not shared pre-fork history on both chains.")
        }
        other => Refusal::final_(format!("A coin of this split couldn't be authenticated ({other:?}).")),
    }
}

/// Where each claimed prevout pays in `source`: the previous transaction is
/// fetched (txid-checked) and its script matched against both branches up to
/// the scanner's address bound. CPU-bound search (#614 G1): off the UI thread.
async fn resolve_outpoints(
    evidence: &dyn SplitEvidenceSource,
    source: &SplitSource,
    claimed: &[OutPoint],
) -> Result<(Vec<RecordedOutpoint>, Vec<(OutPoint, String)>), Refusal> {
    let mut scripts = Vec::with_capacity(claimed.len());
    for outpoint in claimed {
        let previous = evidence
            .previous_transaction(ChainId::Bitcoin, outpoint.txid)
            .await
            .map_err(|kind| {
                evidence_refusal(EvidenceError {
                    outpoint: Some(*outpoint),
                    failure: EvidenceFailure::Read(ChainId::Bitcoin, kind),
                })
            })?;
        if previous.compute_txid() != outpoint.txid {
            return Err(evidence_refusal(EvidenceError {
                outpoint: Some(*outpoint),
                failure: EvidenceFailure::TxidMismatch,
            }));
        }
        let output = usize::try_from(outpoint.vout)
            .ok()
            .and_then(|vout| previous.output.get(vout))
            .ok_or_else(|| {
                evidence_refusal(EvidenceError {
                    outpoint: Some(*outpoint),
                    failure: EvidenceFailure::MissingOutput,
                })
            })?;
        scripts.push((*outpoint, output.script_pubkey.clone()));
    }
    let source = source.clone();
    tokio::task::spawn_blocking(move || {
        let derive = |branch: SplitBranch, index: u32| {
            let descriptor = match branch {
                SplitBranch::External => Some(source.external()),
                SplitBranch::Internal => source.internal(),
            }?;
            descriptor
                .at_derivation_index(index)
                .ok()
                .map(|d| d.script_pubkey())
        };
        let mut recorded = Vec::with_capacity(scripts.len());
        let mut addresses = Vec::with_capacity(scripts.len());
        for (outpoint, script) in scripts {
            let found = [SplitBranch::External, SplitBranch::Internal]
                .iter()
                .copied()
                .flat_map(|branch| (0..MAX_ADDRESSES).map(move |index| (branch, index)))
                .find(|(branch, index)| derive(*branch, *index).as_ref() == Some(&script));
            let (branch, index) = found.ok_or_else(|| {
                Refusal::final_("A recorded coin does not belong to the recorded wallet within the scanned range.")
            })?;
            let address = Address::from_script(&script, Network::Bitcoin)
                .map_err(|_| Refusal::final_("A recorded coin has no address."))?;
            recorded.push(RecordedOutpoint {
                outpoint,
                branch,
                index,
            });
            addresses.push((outpoint, address.to_string()));
        }
        Ok((recorded, addresses))
    })
    .await
    .map_err(|_| Refusal::retry("The split check was interrupted. Try again."))?
}

/// Read the journal in `directory`, authenticate its outpoints afresh, rebuild
/// step 1 at the current Bitcoin tip and verify the recorded signed bytes
/// against the rebuild. The coordinator then resumes with exactly those bytes.
///
/// A cache serving a previous transaction with another witness (#615 N2) can
/// only make this refuse: the txid binds everything the construction uses.
pub async fn restore(
    connect: &dyn SplitConnect,
    directory: &Path,
    target_cube: &str,
    digest: sha256::Hash,
) -> Result<Restored, Refusal> {
    let context = connect.context();
    let identity = claim_workflow::split_identity(target_cube.to_owned(), digest);
    let (record, unsigned, claimed, signed, phase) = {
        let controller = Controller::reopen_settling(directory, &identity, context)
            .await
            .map_err(journal_refusal)?;
        let record = controller
            .recorded_split()
            .map_err(journal_refusal)?
            .ok_or_else(|| Refusal::final_("The journal here is not a split."))?;
        let plan = controller.plan();
        let signed = controller
            .recorded_bitcoin_transaction()
            .cloned()
            .ok_or_else(|| Refusal::final_("The split journal has no signed step 1."))?;
        (
            record,
            plan.step1,
            plan.claimed_prevouts,
            signed,
            controller.phase(),
        )
        // The controller, and the journal lock, end here.
    };
    let source = record.source.ok_or_else(|| Refusal::final_(COMPLETED))?;
    if source.digest() != digest || record.source_digest != digest {
        return Err(Refusal::final_(
            "The split journal is in the wrong directory.",
        ));
    }
    let window = connect.window().await.map_err(|reason| {
        Refusal::retry(format!(
            "Couldn't read Bitcoin Blake2b's status from Connect ({reason})."
        ))
    })?;
    if window.fork_height != record.fork_height {
        return Err(Refusal::final_(STALE_ANCHOR));
    }
    let (recorded, addresses) = resolve_outpoints(connect.evidence(), &source, &claimed).await?;
    let authenticated = authenticate_outpoints(
        connect.evidence(),
        &recorded,
        record.fork_height,
        MAX_EVIDENCE_AGE_SECONDS,
    )
    .await
    .map_err(evidence_refusal)?;
    let coins = authenticated.coins.clone();
    let tip = u32::try_from(authenticated.bitcoin_tip.height)
        .map_err(|_| Refusal::final_("The Bitcoin tip height is out of range."))?;
    let fork_height = record.fork_height;
    let destination = record.destination;
    let (construction, verified) = tokio::task::spawn_blocking(move || {
        let construction = reconstruct_split_step1(
            &SplitInputs {
                chain: ChainId::Bitcoin,
                source: &source,
                coins: &authenticated.coins,
                fork_height,
                destination,
            },
            &unsigned,
            tip,
        )
        .map_err(|error| {
            Refusal::final_(format!(
                "The recorded step 1 could not be rebuilt from the chain ({error}). Nothing was sent."
            ))
        })?;
        let secp = secp256k1::Secp256k1::verification_only();
        let verified = verify_split_step1_transaction(&construction, &signed, &secp).map_err(|error| {
            Refusal::final_(format!(
                "The recorded signed step 1 does not verify ({error}). Nothing was sent."
            ))
        })?;
        Ok::<_, Refusal>((construction, verified))
    })
    .await
    .map_err(|_| Refusal::retry("The split check was interrupted. Try again."))??;
    Ok(Restored {
        construction,
        verified,
        fork_height,
        phase,
        claimed: addresses,
        coins,
    })
}

/// What an unsubmitted journal that [`restore`] finally refused still allows
/// (#625 F2): abandoning it after [`check_abandon`], nothing else. It holds
/// no construction, no signed bytes and no coordinator, so nothing can be
/// reviewed, exported or sent from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbandonOnly {
    /// The recorded signed step 1's own txid.
    pub tracked: Txid,
    /// Each recorded input and the Bitcoin address it pays.
    pub claimed: Vec<(OutPoint, String)>,
}

/// After [`restore`] refused an unsubmitted journal finally (it can't be
/// rebuilt or its signed bytes don't verify): read it again for abandonment
/// only. The journal must be this directory's Split, under this session,
/// with no submission recorded. Each recorded input's address comes from its
/// previous transaction, fetched from Bitcoin and checked against its txid,
/// not from the recorded wallet: the wallet may be what can't be rebuilt.
/// Anything that can't be established refuses, and the journal is kept.
pub async fn abandon_only(
    connect: &dyn SplitConnect,
    directory: &Path,
    target_cube: &str,
    digest: sha256::Hash,
) -> Result<AbandonOnly, Refusal> {
    let identity = claim_workflow::split_identity(target_cube.to_owned(), digest);
    let (tracked, claimed) = {
        let controller = Controller::reopen_settling(directory, &identity, connect.context())
            .await
            .map_err(journal_refusal)?;
        let record = controller
            .recorded_split()
            .map_err(journal_refusal)?
            .ok_or_else(|| Refusal::final_("The journal here is not a split."))?;
        if record.source_digest != digest {
            return Err(Refusal::final_(
                "The split journal is in the wrong directory.",
            ));
        }
        if controller.phase() != Phase::Intent {
            return Err(Refusal::final_(SUBMISSION_RECORDED));
        }
        let plan = controller.plan();
        let tracked = plan.step1_txid();
        let signed = controller
            .recorded_bitcoin_transaction()
            .map(|signed| signed.compute_txid());
        if signed != Some(tracked) || plan.claimed_prevouts.is_empty() {
            return Err(Refusal::final_(UNIDENTIFIED));
        }
        (tracked, plan.claimed_prevouts)
        // The controller, and the journal lock, end here.
    };
    let evidence = connect.evidence();
    let mut addresses = Vec::with_capacity(claimed.len());
    for outpoint in claimed {
        let previous = evidence
            .previous_transaction(ChainId::Bitcoin, outpoint.txid)
            .await
            .map_err(|kind| {
                Refusal::retry(format!(
                    "Connect couldn't read the coins this split recorded ({kind:?}), so it can't be abandoned yet. This is not a sign that a coin was spent. Try again later."
                ))
            })?;
        let address = (previous.compute_txid() == outpoint.txid)
            .then(|| usize::try_from(outpoint.vout).ok())
            .flatten()
            .and_then(|vout| previous.output.get(vout))
            .and_then(|output| Address::from_script(&output.script_pubkey, Network::Bitcoin).ok())
            .ok_or_else(|| Refusal::final_(UNIDENTIFIED))?;
        addresses.push((outpoint, address.to_string()));
    }
    Ok(AbandonOnly {
        tracked,
        claimed: addresses,
    })
}

/// Each claimed prevout of `construction` and the Bitcoin address it pays,
/// from the construction's own txid-authenticated previous transactions.
pub fn claimed_addresses(construction: &SplitStep1) -> Vec<(OutPoint, String)> {
    let psbt = construction.psbt();
    psbt.unsigned_tx
        .input
        .iter()
        .zip(&psbt.inputs)
        .filter_map(|(txin, input)| {
            let previous = input.non_witness_utxo.as_ref()?;
            let output = previous
                .output
                .get(usize::try_from(txin.previous_output.vout).ok()?)?;
            let address = Address::from_script(&output.script_pubkey, Network::Bitcoin).ok()?;
            Some((txin.previous_output, address.to_string()))
        })
        .collect()
}

/// Before an unsubmitted journal may be deleted: the recorded step 1 is
/// absent from Bitcoin and every claimed outpoint is still among its address's
/// Bitcoin unspent outputs, so nothing (this transaction, a re-signed one, or
/// anything else) spent them, even out of band (#622).
pub async fn check_abandon(
    connect: &dyn SplitConnect,
    tracked: Txid,
    claimed: &[(OutPoint, String)],
) -> Result<(), Refusal> {
    let evidence = connect.evidence();
    let fresh = |observed_at: i64| {
        evidence
            .now()
            .checked_sub(observed_at)
            .is_some_and(|age| (0..=MAX_EVIDENCE_AGE_SECONDS).contains(&age))
    };
    let unavailable = |kind: FailureKind| {
        Refusal::retry(format!(
            "Connect couldn't check Bitcoin for this split ({kind:?}), so it can't be abandoned yet. This is not a sign that a coin was spent. Try again later."
        ))
    };
    let status = evidence
        .transaction(ChainId::Bitcoin, tracked)
        .await
        .map_err(unavailable)?;
    if !fresh(status.observed_at()) {
        return Err(unavailable(FailureKind::Stale));
    }
    if *status.value() != TransactionObservation::Absent {
        return Err(Refusal::final_(
            "This split's step 1 is on Bitcoin, so it can't be abandoned. It stays tracked here.",
        ));
    }
    for (outpoint, address) in claimed {
        let unspent = evidence
            .unspent_outputs(ChainId::Bitcoin, address)
            .await
            .map_err(unavailable)?;
        if !fresh(unspent.observed_at()) {
            return Err(unavailable(FailureKind::Stale));
        }
        if !unspent.value().contains(outpoint) {
            return Err(Refusal::final_(
                "A coin of this split has been spent on Bitcoin, possibly by this split's step 1 sent from elsewhere. It can't be abandoned; it stays tracked here.",
            ));
        }
    }
    Ok(())
}

/// After a reorg dropped step 1 and no resend could be reviewed: whether a
/// claimed coin was spent on Bitcoin by another transaction. Step 1 must be
/// fresh-read absent from Bitcoin (its own spend in the mempool is not a
/// double spend); then a claimed coin missing from its address's fresh
/// Bitcoin unspent outputs was spent by something else, and this step 1 can
/// never confirm. A read failure is never reported as spent.
pub async fn step1_double_spent(
    connect: &dyn SplitConnect,
    tracked: Txid,
    claimed: &[(OutPoint, String)],
) -> Result<bool, Refusal> {
    let evidence = connect.evidence();
    let fresh = |observed_at: i64| {
        evidence
            .now()
            .checked_sub(observed_at)
            .is_some_and(|age| (0..=MAX_EVIDENCE_AGE_SECONDS).contains(&age))
    };
    let unavailable = |kind: FailureKind| {
        Refusal::retry(format!(
            "Connect couldn't check Bitcoin after the reorg ({kind:?}). This is not a sign that a coin was spent. Try again later."
        ))
    };
    let status = evidence
        .transaction(ChainId::Bitcoin, tracked)
        .await
        .map_err(unavailable)?;
    if !fresh(status.observed_at()) {
        return Err(unavailable(FailureKind::Stale));
    }
    if *status.value() != TransactionObservation::Absent {
        return Ok(false);
    }
    for (outpoint, address) in claimed {
        let unspent = evidence
            .unspent_outputs(ChainId::Bitcoin, address)
            .await
            .map_err(unavailable)?;
        if !fresh(unspent.observed_at()) {
            return Err(unavailable(FailureKind::Stale));
        }
        if !unspent.value().contains(outpoint) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Delete an unsubmitted Split journal, descriptors included (P2). The
/// journal refuses once a submission was recorded or an inclusion seen.
/// `ended` is the panel's session flag: set by a revocation after this was
/// confirmed, it refuses (`Revoked`) under the journal's lock, right before
/// the delete (#644 r4176212750). Blocking: off the UI thread, with every
/// coordinator on it dropped first.
pub fn abandon(
    directory: &Path,
    target_cube: &str,
    digest: sha256::Hash,
    context: Context,
    ended: &std::sync::atomic::AtomicBool,
) -> Result<(), claim_workflow::Error> {
    let identity = claim_workflow::split_identity(target_cube.to_owned(), digest);
    let controller = Controller::reopen_settling_blocking(directory, &identity, context.clone())?;
    if ended.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(claim_workflow::Error::Revoked);
    }
    controller.abandon_split(&context)
}

/// User-facing copy for a coordinator refusal. Never a retry instruction for
/// an uncertain submission.
pub fn describe(error: claim_coordinator::Error) -> String {
    use claim_coordinator::Error as E;
    match error {
        E::Journal(claim_workflow::Error::WrongIdentity) => OTHER_ACCOUNT.to_string(),
        E::Journal(claim_workflow::Error::Conflict) => {
            "A split of this wallet is already recorded on this device; it continues from there."
                .to_string()
        }
        E::Revoked => "The split session ended. The split is recorded on this device and continues after you sign in again.".to_string(),
        E::Unsupported | E::InvalidBinding => {
            "This split's Connect session or chain binding is not usable. Sign in again and reopen the Cube.".to_string()
        }
        E::NotReady(Assessment::Reorged) => REORGED.to_string(),
        E::NotReady(Assessment::ExpiryMargin) => format!(
            "Bitcoin Blake2b's replay protection expires within {}. Nothing was sent.",
            describe_duration(EXPIRY_MARGIN_SECONDS)
        ),
        other => crate::app::state::vault::claim::describe(other),
    }
}

pub fn describe_route(route: &SubmissionRoute) -> &'static str {
    match route {
        SubmissionRoute::Connect => "Connect",
        SubmissionRoute::BitcoinNode { .. } => "Bitcoin node",
    }
}
