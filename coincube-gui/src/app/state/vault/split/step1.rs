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
        finalize_split_step1, reconstruct_split_step1, verify_split_step1_transaction,
        AncestrySplitStep1, AncestrySplitStep2, FinalizeError, SplitBranch, SplitCoin, SplitInputs,
        SplitSource, SplitStep1, VerifiedSplitStep1,
    },
    miniscript::bitcoin::{
        hashes::sha256, psbt::Psbt, secp256k1, Address, Network, OutPoint, Txid,
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
            http::{HttpObservationSource, OwnedSplitFunding},
            CollectionContext, FailureKind, ObservationSource, TransactionObservation,
        },
        claim_workflow::{self, Context, Controller, Phase, Status},
        foreign_scan::{Branch, BranchRange, ScanDescriptor, ScanPlan, MAX_ADDRESSES},
        foreign_split_inventory::{self, FreshIndex},
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
/// No new two-step split may build the retired 90-byte marker transaction.
pub const RETIRED_MARKER: &str = "This split requires confirmed Bitcoin-only and Bitcoin Blake2b-only funding coins in the source wallet. Signing is unavailable until both coins can be verified.";
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

/// #568 D19: whether the journal in `directory` (source `digest`) is a
/// split this Cube records as completed, so discovery passes over it and a
/// Vault can take another source afterwards. Read as the restart into
/// Completed reads it: a two-step journal for `target_cube` and `digest`
/// whose descriptors were deleted, in no dead end (here: no step-1
/// conflict at all and no #625 step-2 dead end), whose recorded step 2's
/// txid is in a `split_from` record of this digest. Anything else, an
/// unreadable or busy journal included, is not completed: a live split is
/// never hidden. Blocking (a journal read): discovery runs it off the UI
/// thread.
pub fn is_recorded_complete(
    directory: &Path,
    target_cube: &str,
    digest: sha256::Hash,
    split_from: &[crate::app::settings::SplitFromRecord],
) -> bool {
    let Ok(Some(summary)) = claim_workflow::peek_split_journal(directory) else {
        return false;
    };
    summary.two_step
        && summary.source_digest == digest
        && summary.target_cube == target_cube
        && summary.descriptors_forgotten
        && !summary.step1_conflict
        && !summary.step2_dead_end
        && summary.step2_txid.is_some_and(|txid| {
            split_from
                .iter()
                .any(|record| record.descriptor_digest == digest && record.step2_txid == txid)
        })
}

/// #568 D19: the first journal under `root` ([`discover`]) that is not a
/// completed split this Cube records ([`is_recorded_complete`]): the one a
/// Cube open resumes. Blocking: discovery runs it off the UI thread.
pub fn discover_resumable(
    root: &Path,
    target_cube: &str,
    split_from: &[crate::app::settings::SplitFromRecord],
) -> Option<(sha256::Hash, PathBuf)> {
    discover(root).into_iter().find(|(digest, directory)| {
        !is_recorded_complete(directory, target_cube, *digest, split_from)
    })
}

/// Find a checked funding reservation that has not yet produced a transaction
/// intent. Discovery does not display its addresses: the panel reloads it
/// under the live Connect account and repeats all six address-history reads.
pub fn discover_funding(root: &Path, target_cube: &str) -> Option<(sha256::Hash, PathBuf)> {
    let mut found = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let digest = sha256::Hash::from_str(&name).ok()?;
            if digest.to_string() != name {
                return None;
            }
            let directory = entry.path();
            if !std::fs::symlink_metadata(&directory).ok()?.is_dir() || is_closed(&directory) {
                return None;
            }
            if !std::fs::symlink_metadata(directory.join("split-funding.json"))
                .ok()?
                .is_file()
            {
                return None;
            }
            let saved = claim_workflow::peek_split_funding(&directory).ok()??;
            (saved.source_digest == digest && saved.target_cube == target_cube)
                .then_some((digest, directory))
        })
        .collect::<Vec<_>>();
    found.sort();
    found.into_iter().next()
}

/// #568 D15: this source was already split into this Cube's Vault (its
/// `split_from` records it).
pub const SOURCE_ALREADY_SPLIT: &str = "This wallet was already split into this Vault, so it can't be split again. Nothing was started.";
/// #568 D15: a split of this source is already recorded in this Vault.
pub const SOURCE_SPLIT_RECORDED: &str = "A split of this wallet is already recorded in this Vault on this device; it continues from there. Nothing new was started.";
/// #568 D15 with #625 F2 (A1): a split of this source was closed in this
/// Vault; its record is kept, so a new split of it is refused.
pub const SOURCE_SPLIT_CLOSED: &str = "A split of this wallet was closed in this Vault on this device, and its record is kept, so it can't be split again here. Nothing was started.";

/// The source identity (D9) of the scanned wallet: what its journal and the
/// target Cube's `split_from` are keyed by. CPU only.
pub fn source_digest(intent: &SplitIntent) -> Result<sha256::Hash, String> {
    split_source(&intent.external, intent.internal.as_ref())
        .map(|source| source.digest())
        .map_err(|error| error.to_string())
}

/// #568 D15, one predicate with A1's tombstone: why the source `digest` may
/// not be split into the Vault whose Split journals are under `root`, or
/// `None`. Refused when the target Cube's `split_from` records it, when a
/// tombstone is in its journal directory (a closed split, whose record is
/// kept), or when that directory holds a journal. Any entry by either name
/// refuses, as the journal's own create does under its lock
/// (`Controller::create_split`), which stays the final check. Two
/// `symlink_metadata` calls, nothing read inside the journal.
pub fn second_split_refusal(
    root: &Path,
    split_from: &[crate::app::settings::SplitFromRecord],
    digest: sha256::Hash,
) -> Option<&'static str> {
    if split_from
        .iter()
        .any(|record| record.descriptor_digest == digest)
    {
        return Some(SOURCE_ALREADY_SPLIT);
    }
    let directory = journal_directory(root, digest);
    if std::fs::symlink_metadata(directory.join(CLOSED)).is_ok() {
        return Some(SOURCE_SPLIT_CLOSED);
    }
    if std::fs::symlink_metadata(directory.join(JOURNAL_FILE)).is_ok() {
        return Some(SOURCE_SPLIT_RECORDED);
    }
    None
}

/// The journal's intent file in a journal directory (`claim_workflow`'s
/// journal writes it).
const JOURNAL_FILE: &str = "intent.json";

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
    /// The current session's two-chain reader for exact recorded witnesses.
    /// Alternative transports cannot silently skip this read.
    fn paired_observation(&self) -> Option<&HttpObservationSource> {
        None
    }
    /// Only a live account generation can release pre-deposit instructions.
    /// Mocks and alternative transports fail closed unless they implement it.
    fn session_current(&self) -> bool {
        false
    }
    fn evidence(&self) -> &dyn SplitEvidenceSource;
    /// The fork's window from the authenticated anchor, with the fork marker.
    async fn window(&self) -> Result<ForkWindow, String>;
    async fn bitcoin_feerate(&self) -> Option<u64>;
    /// A current fee quote for the exact saved source and target addresses.
    /// This is display guidance only; signing must reprice and revalidate.
    async fn funding_fee_quote(
        &self,
        _source: &SplitSource,
        _reservation: &claim_workflow::SplitFundingReservation,
    ) -> Result<SplitFundingQuote, String> {
        Err("The two-chain funding fee quote is unavailable.".into())
    }
    /// Whether `address` has any chain or mempool history (fresh read).
    async fn address_used(&self, chain: ChainId, address: &str) -> Result<bool, FailureKind>;
    /// Recheck the exact saved source on both chains after deposits arrive.
    /// The default refuses; a mock cannot claim funding readiness by omission.
    async fn check_split_deposits(
        &self,
        _directory: PathBuf,
        _source: &SplitSource,
        _target_cube: &str,
        _target_vault: Option<&(WalletId, coincube_core::descriptors::CoincubeDescriptor)>,
    ) -> Result<SplitDepositProgress, String> {
        Err("Two-chain funding checks are unavailable for this session.".into())
    }
    /// Recheck the retained ordinary pair from a newly completed scan and
    /// create or reopen only its exact version-11 journal. This is a
    /// prerequisite, never cached signing authority; each later export or
    /// hardware dispatch must run it again. Other transports fail closed.
    #[allow(clippy::too_many_arguments)]
    async fn prepare_exact_pair_intent(
        &self,
        _directory: PathBuf,
        _source: &SplitSource,
        _target_cube: &str,
        _target_vault: Option<&(WalletId, coincube_core::descriptors::CoincubeDescriptor)>,
        _bitcoin: &AncestrySplitStep1,
        _fork: &AncestrySplitStep2,
        _rates: (u64, u64),
    ) -> Result<SplitSignerCheck, String> {
        Err("Two-chain Split signer preparation is unavailable for this session.".into())
    }
    /// Opens or creates the journal. Blocking: callers use `spawn_blocking`.
    fn open(&self, request: OpenRequest) -> Result<Box<dyn Step1Driver>, claim_coordinator::Error>;
}

/// Opaque result of the production adapter's fresh source scan, reservation
/// reload, two-chain ancestry check, exact PSBT rebuild and intent recording.
/// Holding this value alone cannot sign or submit; each dispatch asks it for
/// a short-lived permit bound to that side's current PSBT and session.
#[derive(Debug)]
#[allow(dead_code)] // The paired Split signer UI remains gated.
pub struct SplitSignerCheck {
    requalified: claim_workflow::RequalifiedInitialSplit,
    generation: watch::Receiver<u64>,
}

#[allow(dead_code)] // Consumed by the staged paired Split signer transition.
impl SplitSignerCheck {
    pub(crate) fn signer_permit(
        &self,
        side: ChainId,
        bitcoin: &AncestrySplitStep1,
        fork: &AncestrySplitStep2,
        current: &Psbt,
    ) -> Option<claim_workflow::paired_signing_permit::PairedSigningPermit> {
        self.requalified
            .signer_permit(side, bitcoin, fork, current, self.generation.clone())
    }

    pub(crate) fn tips(&self) -> coincube_core::claim::PreflightTips {
        self.requalified.tips()
    }

    pub(crate) fn context(&self) -> &Context {
        self.requalified.context()
    }
}

/// Deposit instructions from an immutable local reservation, released only
/// after fresh Connect reads find no history for any address on either chain.
/// A later scan must still prove the deposits' chain-specific ancestry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedFundingAddresses {
    pub bitcoin_deposit: String,
    pub btcb2_deposit: String,
    pub bitcoin_return: String,
    pub fee_quote: SplitFundingQuote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitFundingQuote {
    pub bitcoin_rate_sat_vb: u64,
    pub btcb2_rate_sat_vb: u64,
    pub bitcoin_minimum_sats: u64,
    pub btcb2_minimum_sats: u64,
}

/// Display-only observation. Candidate flags never enable signing; only the
/// paired proof can set `both_verified`, and it is discarded after this read.
#[derive(Debug)]
pub struct SplitDepositProgress {
    pub bitcoin_candidate: bool,
    pub btcb2_candidate: bool,
    pub both_verified: bool,
    /// Derived from the exact paired unsigned transactions, never retained
    /// as spend authority or offered to a signer.
    pub review: Option<crate::app::view::vault::ancestry_coins::VerifiedCoinPairReview>,
    pub(super) prepared: Option<Box<PreparedSplitPair>>,
}

/// A transient exact pair retained with the proof that produced its preview.
/// It must be requalified against current chains, wallet inputs and the saved
/// reservation before any hardware or software signer sees either PSBT.
#[derive(Debug)]
pub(super) struct PreparedSplitPair {
    bitcoin: AncestrySplitStep1,
    fork: AncestrySplitStep2,
    funding: OwnedSplitFunding,
    bitcoin_rate_vb: u64,
    fork_rate_vb: u64,
}

impl PreparedSplitPair {
    fn matches_review(
        &self,
        review: &crate::app::view::vault::ancestry_coins::VerifiedCoinPairReview,
    ) -> bool {
        self.bitcoin_rate_vb > 0
            && self.fork_rate_vb > 0
            && crate::app::view::vault::ancestry_coins::CoinPairReview::from_psbts(
                self.bitcoin.psbt(),
                self.fork.psbt(),
                self.bitcoin.claimed_prevouts(),
                self.funding.bitcoin.outpoint,
                self.funding.fork.outpoint,
            )
            .is_ok_and(|coins| coins == review.coins)
    }
}

impl SplitDepositProgress {
    pub(super) fn exact_review_retained(&self) -> bool {
        self.review.as_ref().is_none_or(|review| {
            self.prepared
                .as_ref()
                .is_some_and(|pair| pair.matches_review(review))
        })
    }
}

pub async fn check_reserved_funding_addresses(
    connect: &dyn SplitConnect,
    directory: PathBuf,
    source: &SplitSource,
    target_cube: &str,
) -> Result<CheckedFundingAddresses, String> {
    if !connect.session_current() {
        return Err("The Connect session changed before the address check.".into());
    }
    let context = connect.context();
    let source = source.clone();
    let target = target_cube.to_owned();
    let expected = context.clone();
    let for_load = directory.clone();
    let source_for_load = source.clone();
    let saved = tokio::task::spawn_blocking(move || {
        claim_workflow::load_split_funding(&for_load, &source_for_load, &target, &expected)
    })
    .await
    .map_err(|_| "Reading the saved funding addresses was interrupted.".to_string())?
    .map_err(|_| "The saved funding addresses could not be verified.".to_string())?
    .ok_or_else(|| "No funding addresses were reserved for this wallet.".to_string())?;
    let [bitcoin_deposit, btcb2_deposit, bitcoin_return] = saved
        .addresses()
        .map_err(|_| "The saved funding addresses are invalid.".to_string())?;
    let addresses = [
        bitcoin_deposit.as_str(),
        btcb2_deposit.as_str(),
        bitcoin_return.as_str(),
    ];
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        for address in addresses {
            for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
                let used = connect
                    .address_used(chain, address)
                    .await
                    .map_err(|_| "Address history could not be checked on both chains.".to_string())?;
                if used && !saved.is_released() {
                    return Err("A reserved address already has chain history. Review the funding plan before sending coins.".to_string());
                }
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| "Checking both chains' address history timed out.".to_string())??;
    let fee_quote = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        connect.funding_fee_quote(&source, &saved),
    )
    .await
    .map_err(|_| "Quoting both chains' deposit fees timed out.".to_string())??;
    if !connect.session_current() || connect.context() != context {
        return Err("The Connect session changed during the address check.".into());
    }
    if !saved.is_released() {
        tokio::task::spawn_blocking(move || {
            claim_workflow::release_split_funding(&directory, &saved)
        })
        .await
        .map_err(|_| "Saving the checked funding addresses was interrupted.".to_string())?
        .map_err(|_| "The checked funding addresses could not be saved.".to_string())?;
    }
    if !connect.session_current() || connect.context() != context {
        return Err("The Connect session changed during the address check.".into());
    }
    Ok(CheckedFundingAddresses {
        bitcoin_deposit,
        btcb2_deposit,
        bitcoin_return,
        fee_quote,
    })
}

/// Reopen the funding-only stage after restart. Discovery never reveals the
/// saved addresses; this call checks the target, source digest and current
/// Connect account, then repeats both-chain history reads before display.
pub async fn recover_funding_addresses(
    connect: &dyn SplitConnect,
    directory: PathBuf,
    digest: sha256::Hash,
    target_cube: &str,
    target_vault: Option<&(WalletId, coincube_core::descriptors::CoincubeDescriptor)>,
) -> Result<(SplitSource, CheckedFundingAddresses), String> {
    if !connect.session_current() {
        return Err("The Connect session changed before recovering deposit addresses.".into());
    }
    let target = target_cube.to_owned();
    let for_load = directory.clone();
    let saved = tokio::task::spawn_blocking(move || claim_workflow::peek_split_funding(&for_load))
        .await
        .map_err(|_| "Reading the funding record was interrupted.".to_string())?
        .map_err(|_| "The funding record could not be verified.".to_string())?
        .ok_or_else(|| "No funding record was found for this split.".to_string())?;
    let source = saved
        .source()
        .map_err(|_| "The saved source wallet is invalid.".to_string())?;
    if saved.source_digest != digest || !saved.matches(&source, &target, &connect.context()) {
        return Err("The funding record belongs to another wallet or Connect account.".into());
    }
    if let Some(binding) = &saved.target_binding {
        let Some((wallet_id, descriptor)) = target_vault else {
            return Err("The target Vault is unavailable for this funding record.".into());
        };
        if !binding.matches_vault(wallet_id, descriptor) {
            return Err(
                "The target Vault changed since the deposit addresses were reserved.".into(),
            );
        }
    }
    let addresses = check_reserved_funding_addresses(connect, directory, &source, &target).await?;
    Ok((source, addresses))
}

/// A version-11 intent may be durable before either signer has returned a
/// signature. Reopen its exact released reservation under the current source,
/// target Vault and Connect identity, then repeat both-chain address reads.
/// This returns the deposit screen, never a signer or proof of spendability.
pub async fn recover_unsigned_paired_funding_addresses(
    connect: &dyn SplitConnect,
    directory: PathBuf,
    source: &SplitSource,
    target_cube: &str,
    target_vault: Option<&(WalletId, coincube_core::descriptors::CoincubeDescriptor)>,
) -> Result<CheckedFundingAddresses, String> {
    let context = connect.context();
    let (wallet_id, descriptor) = target_vault
        .ok_or_else(|| "The recorded BTCB2 target Vault is unavailable.".to_string())?;
    let saved_directory = directory.clone();
    let saved_source = source.clone();
    let saved_target = target_cube.to_owned();
    let saved_context = context.clone();
    let reservation = tokio::task::spawn_blocking(move || {
        claim_workflow::load_split_funding(
            &saved_directory,
            &saved_source,
            &saved_target,
            &saved_context,
        )
    })
    .await
    .map_err(|_| "Reading the recorded funding plan was interrupted.".to_string())?
    .map_err(|_| "The recorded funding plan could not be verified.".to_string())?
    .ok_or_else(|| "The recorded split has no saved deposit addresses.".to_string())?;
    if !reservation.is_released()
        || !reservation
            .target_binding
            .as_ref()
            .is_some_and(|binding| binding.matches_vault(wallet_id, descriptor))
    {
        return Err("The recorded split deposits no longer match this BTCB2 Vault.".into());
    }
    check_reserved_funding_addresses(connect, directory, source, target_cube).await
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

fn split_funding_scan_plan(source: &SplitSource) -> Result<ScanPlan, String> {
    // Match the bounded foreign-wallet scan that first selected the receive
    // indices. A deposited output beyond this walk is unavailable, not proof
    // of zero funds or permission to choose a different address.
    const RANGE_END: u32 = 100;
    const GAP: u32 = 20;
    let branch = |kind, descriptor: String| -> Result<BranchRange, String> {
        let descriptor = ScanDescriptor::parse(kind, &descriptor)
            .map_err(|_| "The saved source descriptor cannot be scanned.".to_string())?;
        Ok(BranchRange {
            start: 0,
            end_exclusive: descriptor.end_exclusive(RANGE_END),
            descriptor,
        })
    };
    let mut branches = vec![branch(Branch::External, source.external().to_string())?];
    if let Some(internal) = source.internal() {
        branches.push(branch(Branch::Internal, internal.to_string())?);
    }
    Ok(ScanPlan {
        chain: ChainId::BitcoinBlake2b,
        branches,
        gap: GAP,
    })
}

fn split_funding_quote_for_inventory(
    source: &SplitSource,
    reservation: &claim_workflow::SplitFundingReservation,
    inventory: &foreign_split_inventory::SplitInventory,
    target_script: &coincube_core::miniscript::bitcoin::Script,
    bitcoin_rate_sat_vb: u64,
    btcb2_rate_sat_vb: u64,
) -> Result<SplitFundingQuote, String> {
    let shared = inventory.splittable_coins();
    let return_script = source
        .external()
        .at_derivation_index(reservation.indices.bitcoin_return)
        .map_err(|_| "The Bitcoin return address is invalid.".to_string())?
        .script_pubkey();
    let minimum = |index: u32,
                   destination: &coincube_core::miniscript::bitcoin::Script,
                   rate: u64,
                   chain: &str|
     -> Result<u64, String> {
        let vbytes = coincube_core::foreign_split::estimate_ancestry_signed_vbytes(
            source,
            &shared,
            inventory.fork_height(),
            index,
            destination,
        )
        .map_err(|_| format!("The {chain} deposit fee cannot be estimated for these coins."))?;
        let fee = vbytes
            .checked_mul(rate)
            .ok_or_else(|| format!("The {chain} fee quote is out of range."))?;
        let dust = source
            .external()
            .at_derivation_index(index)
            .map_err(|_| format!("The {chain} deposit address is invalid."))?
            .script_pubkey()
            .minimal_non_dust()
            .to_sat();
        Ok(fee.max(dust))
    };
    Ok(SplitFundingQuote {
        bitcoin_rate_sat_vb,
        btcb2_rate_sat_vb,
        bitcoin_minimum_sats: minimum(
            reservation.indices.bitcoin_deposit,
            return_script.as_script(),
            bitcoin_rate_sat_vb,
            "Bitcoin",
        )?,
        btcb2_minimum_sats: minimum(
            reservation.indices.btcb2_deposit,
            target_script,
            btcb2_rate_sat_vb,
            "BTCB2",
        )?,
    })
}

#[async_trait]
impl SplitConnect for ProductionConnect {
    fn context(&self) -> Context {
        self.context.clone()
    }
    fn paired_observation(&self) -> Option<&HttpObservationSource> {
        Some(&self.observation)
    }
    fn session_current(&self) -> bool {
        self.generation.has_changed().is_ok() && *self.generation.borrow() == self.expected
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
    async fn funding_fee_quote(
        &self,
        source: &SplitSource,
        reservation: &claim_workflow::SplitFundingReservation,
    ) -> Result<SplitFundingQuote, String> {
        if !self.session_current() {
            return Err("The Connect session changed before the funding quote.".into());
        }
        let target = reservation.target_binding.as_ref().ok_or_else(|| {
            "The BTCB2 destination is not bound to this funding plan.".to_string()
        })?;
        let target_script = target
            .script_pubkey()
            .map_err(|_| "The saved BTCB2 destination is invalid.".to_string())?;
        let scan = foreign_split_inventory::scan(
            self.session.client.clone(),
            split_funding_scan_plan(source)?,
            self.expected,
            self.generation.clone(),
        )
        .await
        .map_err(|error| format!("The wallet scan for the funding quote failed: {error:?}"))?;
        if !self.session_current() {
            return Err("The Connect session changed during the funding quote.".into());
        }
        let bitcoin_rate_sat_vb = self
            .bitcoin_feerate()
            .await
            .ok_or_else(|| "The current Bitcoin fee quote is unavailable.".to_string())?;
        let btcb2_fees =
            crate::services::split_fees::btcb2_fee_source(Some(self.session.client.clone()));
        let btcb2_rate_sat_vb = crate::services::foreign_psbt::btcb2_sweep_feerate(&*btcb2_fees)
            .await
            .ok_or_else(|| "The current BTCB2 fee quote is unavailable.".to_string())?;
        if !self.session_current() {
            return Err("The Connect session changed during the funding quote.".into());
        }
        split_funding_quote_for_inventory(
            source,
            reservation,
            &scan.inventory,
            target_script.as_script(),
            bitcoin_rate_sat_vb,
            btcb2_rate_sat_vb,
        )
    }
    async fn address_used(&self, chain: ChainId, address: &str) -> Result<bool, FailureKind> {
        address_used(&self.esplora, chain, address, self.observation.now()).await
    }
    async fn check_split_deposits(
        &self,
        directory: PathBuf,
        source: &SplitSource,
        target_cube: &str,
        target_vault: Option<&(WalletId, coincube_core::descriptors::CoincubeDescriptor)>,
    ) -> Result<SplitDepositProgress, String> {
        if !self.session_current() {
            return Err("The Connect session changed before checking deposits.".into());
        }
        let context = self.context();
        let plan = split_funding_scan_plan(source)?;
        let scan = foreign_split_inventory::scan(
            self.session.client.clone(),
            plan,
            self.expected,
            self.generation.clone(),
        )
        .await
        .map_err(|error| format!("The two-chain wallet scan did not complete: {error:?}"))?;
        if !self.session_current() || self.context() != context {
            return Err("The Connect session changed during the deposit scan.".into());
        }
        let source_for_load = source.clone();
        let target = target_cube.to_owned();
        let saved_directory = directory.clone();
        let saved_context = context.clone();
        let reservation = tokio::task::spawn_blocking(move || {
            claim_workflow::load_split_funding(
                &saved_directory,
                &source_for_load,
                &target,
                &saved_context,
            )
        })
        .await
        .map_err(|_| "Reading the saved deposit addresses was interrupted.".to_string())?
        .map_err(|_| "The saved deposit addresses could not be verified.".to_string())?
        .ok_or_else(|| "No saved deposit addresses were found for this wallet.".to_string())?;
        if !reservation.is_released() {
            return Err("The deposit addresses were not checked before release.".into());
        }
        if let Some(binding) = &reservation.target_binding {
            let Some((wallet_id, descriptor)) = target_vault else {
                return Err("The target Vault is unavailable for this deposit check.".into());
            };
            if !binding.matches_vault(wallet_id, descriptor) {
                return Err(
                    "The target Vault changed since the deposit addresses were reserved.".into(),
                );
            }
        }
        let reserved = scan
            .inventory
            .funding_at_reserved_addresses(reservation.indices);
        let bitcoin_candidate = !reserved.funding_candidates(ChainId::Bitcoin).is_empty();
        let btcb2_candidate = !reserved
            .funding_candidates(ChainId::BitcoinBlake2b)
            .is_empty();
        let (review, prepared) = if bitcoin_candidate && btcb2_candidate {
            let target_script = reservation
                .target_binding
                .as_ref()
                .ok_or_else(|| "The BTCB2 target reservation is missing.".to_string())?
                .script_pubkey()
                .map_err(|_| "The BTCB2 target address changed.".to_string())?;
            let bitcoin_rate = self
                .bitcoin_feerate()
                .await
                .ok_or_else(|| "The current Bitcoin fee quote is unavailable.".to_string())?;
            let fork_fees =
                crate::services::split_fees::btcb2_fee_source(Some(self.session.client.clone()));
            let fork_rate = crate::services::foreign_psbt::btcb2_sweep_feerate(&*fork_fees)
                .await
                .ok_or_else(|| "The current BTCB2 fee quote is unavailable.".to_string())?;
            if !self.session_current() || self.context() != context {
                return Err("The Connect session changed during the deposit check.".into());
            }
            let quote = split_funding_quote_for_inventory(
                source,
                &reservation,
                &scan.inventory,
                target_script.as_script(),
                bitcoin_rate,
                fork_rate,
            )?;
            let eligible = reserved.funding_covering_estimated_fees(
                quote.bitcoin_minimum_sats,
                quote.btcb2_minimum_sats,
            );
            if eligible.funding_candidates(ChainId::Bitcoin).is_empty() {
                return Err(format!(
                    "No Bitcoin deposit candidate at the reserved address covers the current minimum of {} sats. A separate eligible deposit may be sent to that same address.",
                    quote.bitcoin_minimum_sats
                ));
            }
            if eligible
                .funding_candidates(ChainId::BitcoinBlake2b)
                .is_empty()
            {
                return Err(format!(
                    "No BTCB2 deposit candidate at the reserved address covers the current minimum of {} sats. A separate eligible deposit may be sent to that same address.",
                    quote.btcb2_minimum_sats
                ));
            }
            let found = reservation
                .discover_reserved_funding(
                    directory,
                    source,
                    &context,
                    &scan.inventory,
                    &self.observation,
                    CHECK_POLICY.observations,
                    (quote.bitcoin_minimum_sats, quote.btcb2_minimum_sats),
                )
                .await
                .map_err(|error| match error {
                    claim_workflow::SplitFundingDiscoveryError::Discovery(error) =>
                        crate::app::state::vault::claim::funding_proof_refusal(error).to_string(),
                    claim_workflow::SplitFundingDiscoveryError::Changed =>
                        "The saved deposit addresses, source wallet or Connect session changed. Reopen this split and check again.".to_string(),
                    claim_workflow::SplitFundingDiscoveryError::InvalidReservation(_) =>
                        "The saved deposit reservation no longer matches this source and target Cube.".to_string(),
                })?;
            if let Some(found) = found {
                let tips = coincube_core::claim::PreflightTips {
                    bitcoin: found
                        .inputs
                        .funding()
                        .bitcoin()
                        .ancestry()
                        .pair()
                        .bitcoin()
                        .tip,
                    fork: found.inputs.funding().fork().ancestry().pair().fork().tip,
                };
                let context = crate::services::claim_observation::http::FundingContext {
                    provider: &context.provider,
                    generation: context.generation,
                    now: self.observation.now(),
                    max_age_seconds: CHECK_POLICY.observations.max_observation_age_seconds,
                    tips,
                };
                let (bitcoin, fork) = found
                    .construct_split_pair(
                        &scan.inventory,
                        source,
                        reservation.indices.bitcoin_return,
                        target_script.as_script(),
                        bitcoin_rate,
                        fork_rate,
                        context,
                    )
                    .map_err(|error| {
                        match error {
                            crate::services::claim_observation::http::SplitPairError::InvalidConstruction(
                                coincube_core::foreign_split::Error::FundingBelowFee(ChainId::Bitcoin),
                            ) => "The selected Bitcoin-only coin cannot cover the current Bitcoin transaction fee. It cannot be used for this attempt; retry when fees fall or after a separate eligible deposit is confirmed.".to_string(),
                            crate::services::claim_observation::http::SplitPairError::InvalidConstruction(
                                coincube_core::foreign_split::Error::FundingBelowFee(ChainId::BitcoinBlake2b),
                            ) => "The selected BTCB2-only coin cannot cover the current BTCB2 transaction fee. It cannot be used for this attempt; retry when fees fall or after a separate eligible deposit is confirmed.".to_string(),
                            _ => format!("The exact split transactions could not be reviewed: {error:?}"),
                        }
                    })?;
                let coins = crate::app::view::vault::ancestry_coins::CoinPairReview::from_psbts(
                    bitcoin.psbt(),
                    fork.psbt(),
                    bitcoin.claimed_prevouts(),
                    bitcoin.funding_prevout(),
                    fork.funding_prevout(),
                )
                .map_err(str::to_owned)?;
                // Constructing both transactions and their review can outlive
                // the observations that qualified the deposits. A new block
                // or an aged proof requires a fresh paired scan before the UI
                // may describe either funding input as verified.
                let (bitcoin_tip, fork_tip) = tokio::try_join!(
                    self.observation.tip(ChainId::Bitcoin),
                    self.observation.tip(ChainId::BitcoinBlake2b),
                )
                .map_err(|_| {
                    "The chain tips could not be refreshed for the split preview. Check deposits again."
                        .to_string()
                })?;
                let proof_age = self
                    .observation
                    .now()
                    .checked_sub(found.inputs.observed_at());
                if bitcoin_tip.value() != &tips.bitcoin
                    || fork_tip.value() != &tips.fork
                    || !proof_age.is_some_and(|age| {
                        (0..=CHECK_POLICY.observations.max_observation_age_seconds).contains(&age)
                    })
                {
                    return Err("The two-chain split proof changed or expired while preparing the preview. Check deposits again.".into());
                }
                let review = crate::app::view::vault::ancestry_coins::VerifiedCoinPairReview::new(
                    coins,
                    found.inputs.funding().bitcoin().block(),
                    tips.bitcoin,
                    found.inputs.funding().fork().block(),
                    tips.fork,
                )
                .map_err(str::to_owned)?;
                (
                    Some(review),
                    Some(Box::new(PreparedSplitPair {
                        bitcoin,
                        fork,
                        funding: found,
                        bitcoin_rate_vb: bitcoin_rate,
                        fork_rate_vb: fork_rate,
                    })),
                )
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };
        if !self.session_current() || self.context() != context {
            return Err("The Connect session changed during the deposit proof.".into());
        }
        Ok(SplitDepositProgress {
            bitcoin_candidate,
            btcb2_candidate,
            both_verified: review.is_some(),
            review,
            prepared,
        })
    }
    async fn prepare_exact_pair_intent(
        &self,
        directory: PathBuf,
        source: &SplitSource,
        target_cube: &str,
        target_vault: Option<&(WalletId, coincube_core::descriptors::CoincubeDescriptor)>,
        bitcoin: &AncestrySplitStep1,
        fork: &AncestrySplitStep2,
        rates: (u64, u64),
    ) -> Result<SplitSignerCheck, String> {
        if !self.session_current() {
            return Err("The Connect session changed before preparing this split.".into());
        }
        let context = self.context();
        let (wallet_id, descriptor) = target_vault
            .ok_or_else(|| "The target BTCB2 Vault is unavailable for this split.".to_string())?;
        let scan = foreign_split_inventory::scan(
            self.session.client.clone(),
            split_funding_scan_plan(source)?,
            self.expected,
            self.generation.clone(),
        )
        .await
        .map_err(|error| format!("The two-chain wallet scan did not complete: {error:?}"))?;
        if !self.session_current() || self.context() != context {
            return Err("The Connect session changed during the split scan.".into());
        }
        let saved_directory = directory.clone();
        let saved_source = source.clone();
        let saved_target = target_cube.to_owned();
        let saved_context = context.clone();
        let reservation = tokio::task::spawn_blocking(move || {
            claim_workflow::load_split_funding(
                &saved_directory,
                &saved_source,
                &saved_target,
                &saved_context,
            )
        })
        .await
        .map_err(|_| "Reading the saved split funding plan was interrupted.".to_string())?
        .map_err(|_| "The saved split funding plan could not be verified.".to_string())?
        .ok_or_else(|| "No saved split funding plan was found for this wallet.".to_string())?;
        if !reservation.is_released() {
            return Err("The deposit addresses were not checked before release.".into());
        }
        let binding = reservation
            .target_binding
            .as_ref()
            .filter(|binding| binding.matches_vault(wallet_id, descriptor))
            .ok_or_else(|| "The target BTCB2 Vault changed since deposit setup.".to_string())?;
        let target_script = binding
            .script_pubkey()
            .map_err(|_| "The saved BTCB2 destination is invalid.".to_string())?;
        let target_index = binding.receive_index();
        let bitcoin_rate = self
            .bitcoin_feerate()
            .await
            .ok_or_else(|| "The current Bitcoin fee quote is unavailable.".to_string())?;
        let fork_fees =
            crate::services::split_fees::btcb2_fee_source(Some(self.session.client.clone()));
        let fork_rate = crate::services::foreign_psbt::btcb2_sweep_feerate(&*fork_fees)
            .await
            .ok_or_else(|| "The current BTCB2 fee quote is unavailable.".to_string())?;
        if !self.session_current() || self.context() != context {
            return Err("The Connect session changed during the split fee check.".into());
        }
        if rates != (bitcoin_rate, fork_rate) {
            return Err(
                "The split fees changed. Check both deposits and review the transactions again."
                    .into(),
            );
        }
        let quote = split_funding_quote_for_inventory(
            source,
            &reservation,
            &scan.inventory,
            target_script.as_script(),
            bitcoin_rate,
            fork_rate,
        )?;
        let funding = reservation
            .requalify_exact_pair(
                directory.clone(),
                source,
                target_cube,
                (wallet_id, descriptor),
                &context,
                &scan.inventory,
                &self.observation,
                CHECK_POLICY.observations,
                (quote.bitcoin_minimum_sats, quote.btcb2_minimum_sats),
                bitcoin,
                fork,
                rates,
            )
            .await
            .map_err(|error| format!("The exact split pair is no longer verified: {error:?}"))?;
        if !self.session_current() || self.context() != context {
            return Err("The Connect session changed during split verification.".into());
        }
        let descriptor = descriptor.clone();
        let bitcoin = bitcoin.clone();
        let fork = fork.clone();
        let bitcoin_path = funding.funding().bitcoin_path.clone();
        let fork_path = funding.funding().fork_path.clone();
        let target_cube = target_cube.to_owned();
        let result = tokio::task::spawn_blocking(move || {
            let controller = Controller::create_or_reopen_ancestry_split(
                &directory,
                target_cube,
                &descriptor,
                target_index,
                &bitcoin,
                &bitcoin_path,
                &fork,
                &fork_path,
                context,
            )?;
            // The controller owns the journal lock. Release it before the
            // next independent proof or signing transition.
            drop(controller);
            Ok::<(), claim_workflow::Error>(())
        })
        .await
        .map_err(|_| "Recording the exact split intent was interrupted.".to_string())?
        .map_err(|error| format!("The exact split intent could not be recorded: {error:?}"));
        if !self.session_current() {
            return Err("The Connect session changed while recording the split intent.".into());
        }
        result.map(|()| SplitSignerCheck {
            requalified: funding,
            generation: self.generation.clone(),
        })
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

/// Obsolete deployment timing cannot authorize a new split.
pub fn rdts_refusal(_assessment: Assessment, _window: &ForkWindow) -> String {
    RETIRED_MARKER.to_string()
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
    let _ = prepared;
    Err(RETIRED_MARKER.to_string())
}

/// Legacy fixture builder for journal and reconciliation tests only. New UI
/// paths call `build`, which cannot create an OP_RETURN transaction.
#[cfg(test)]
pub(crate) fn build_legacy_for_test(prepared: &Prepared) -> Result<SplitStep1, String> {
    use coincube_core::{
        foreign_split::create_split_step1, miniscript::bitcoin::absolute::LockTime,
    };
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
    .map_err(|error| format!("Legacy split fixture could not build step 1: {error}"))
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
pub(super) async fn resolve_outpoints(
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
        // #568 B5c-1: Split's own copy wherever Claim's names a claim, the
        // Claim panel or a "Claim Cube".
        E::UnsafeLegacyAlternative => "The signed transaction retains legacy signatures that can independently spend its coins, so it was not finalized and nothing was sent. Sign again from the unsigned transaction, collecting only the signatures it needs.".to_string(),
        E::Journal(error) => {
            format!("Couldn't record this split on this device ({error:?}).")
        }
        E::Preflight(crate::services::claim_preflight::Error::BackendChanged) => {
            "The connection changed since the review, so nothing was sent. Review it again."
                .to_string()
        }
        E::SubmissionAlreadyRecorded => {
            "A submission is already recorded for this split; it can only be tracked now."
                .to_string()
        }
        E::CompletionPersistence(error) => {
            log::warn!("Unable to update the Split completion record: {error}");
            "Couldn't update this split's completion record in this Cube. Check status again."
                .to_string()
        }
        other => crate::app::state::vault::claim::describe(other),
    }
}

pub fn describe_route(route: &SubmissionRoute) -> &'static str {
    match route {
        SubmissionRoute::Connect => "Connect",
        SubmissionRoute::BitcoinNode { .. } => "Bitcoin node",
    }
}
