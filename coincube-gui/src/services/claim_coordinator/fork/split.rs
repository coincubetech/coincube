//! Split (#568 B2): the six-confirmation gate before step 2 and the one
//! value that authorizes it, [`ForeignStep2Authorization`].
//!
//! [`SplitPreparation`] mirrors Claim's [`Preparation`]: it owns the Split
//! journal after step 1 was submitted, and every [`SplitPreparation::check_signing`]
//! collects both chains twice with the same view. Between the two
//! collections it reads, fresh from Connect's BTCB2 Esplora, the unspent
//! outputs of every claimed prevout's address: a coin already spent on BTCB2
//! (by anyone) cannot be swept by step 2, so it refuses. A token is minted
//! only when, at the second collection's tips:
//! - the tracked (signed) step-1 txid has at least
//!   [`MIN_CONFIRMATIONS`](coincube_core::claim::MIN_CONFIRMATIONS) on
//!   Bitcoin, in the block recorded for it (a re-mined step 1 needs the
//!   reconfirmation review of the step-1 coordinator first);
//! - step 1 is absent from the fork chain;
//! - RDTS is active with more than the policy margin (36 h, D3) left;
//! - both tips equal the ones the first collection saw.
//!
//! The token is bound to the check (preparation id and revision), a short
//! monotonic deadline, the session generation and revoker, the digest of the
//! claimed prevouts and the tracked txid. It has no Clone, no serialization
//! and no public constructor, and redeeming it consumes it. A later check,
//! a generation change, a revocation (logout) or dropping the preparation
//! kills every earlier token. Redeeming it builds step 2 (B3b, [`step2`]):
//! the token must name this preparation's tracked step-1 txid too.
//!
//! Step 1's reorg handling stays with the step-1 coordinator
//! (`claim_coordinator::Coordinator`):
//! `reconcile` reports `Reorged`, `prepare_reconfirmation` reviews a step 1
//! re-mined in another block, and `prepare_resubmission` offers the exact
//! recorded bytes after a fresh preflight when it was reorged out. This gate
//! refuses in every one of those states.
use super::*;
use crate::services::{
    claim_observation::{FailureKind, FreshRead},
    split_evidence::ConnectEsplora,
};
use coincube_core::{
    foreign_split::{SplitStep1, VerifiedSplitStep1},
    miniscript::bitcoin::{consensus, Address, Network, OutPoint},
};
use std::{collections::BTreeSet, convert::TryFrom, sync::Weak};

/// The daemonless Connect production for the step-2 check: the Split step-1
/// observation source (same origin checks) and Connect's fresh BTCB2
/// unspent-output reads.
pub struct SplitForkProduction {
    source: HttpObservationSource,
    esplora: ConnectEsplora,
    /// The admitted Connect origin, `scheme://host[:port]/`. Step 2's
    /// transport must be bound to this same origin.
    origin: String,
    context: Context,
    generation: watch::Receiver<u64>,
}
impl SplitForkProduction {
    pub fn new(
        client: CoincubeClient,
        account: String,
        expected_generation: u64,
        generation: watch::Receiver<u64>,
    ) -> Result<Self, Error> {
        let esplora = ConnectEsplora::new(
            &client,
            CollectionContext {
                expected_generation,
                generation: generation.clone(),
            },
        )
        .map_err(|_| Error::InvalidBinding)?;
        let origin = reqwest::Url::parse(&client.base_url)
            .map_err(|_| Error::InvalidBinding)?
            .as_str()
            .to_owned();
        let (source, context, generation) = super::super::split::SplitProduction::new(
            client,
            account,
            expected_generation,
            generation,
            ChainId::Bitcoin,
        )?
        .into_observation();
        Ok(Self {
            source,
            esplora,
            origin,
            context,
            generation,
        })
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
}

#[async_trait]
trait SplitForkServices: Send + Sync {
    fn source(&self) -> &dyn ObservationSource;
    /// The admitted Connect origin (`scheme://host[:port]/`).
    fn origin(&self) -> &str;
    /// A fresh read of the BTCB2 unspent outputs paying `address`.
    async fn btcb2_unspent(&self, address: &str) -> Result<FreshRead<Vec<OutPoint>>, FailureKind>;
    /// A fresh read of whether `address` has any history on `chain`.
    async fn address_used(
        &self,
        chain: ChainId,
        address: &str,
    ) -> Result<FreshRead<bool>, FailureKind>;
}
#[async_trait]
impl SplitForkServices for SplitForkProduction {
    fn source(&self) -> &dyn ObservationSource {
        &self.source
    }
    fn origin(&self) -> &str {
        &self.origin
    }
    async fn btcb2_unspent(&self, address: &str) -> Result<FreshRead<Vec<OutPoint>>, FailureKind> {
        self.esplora
            .unspent_outputs(ChainId::BitcoinBlake2b, address)
            .await
    }
    async fn address_used(
        &self,
        chain: ChainId,
        address: &str,
    ) -> Result<FreshRead<bool>, FailureKind> {
        self.esplora.address_used(chain, address).await
    }
}

/// Why a step-2 check refused.
#[derive(Debug)]
pub enum SplitCheckError {
    /// The same refusals as Claim's preparation: not deep enough
    /// (`NotReady(WaitingForDepth { .. })`), reorged, RDTS margin, changed
    /// view, revoked session, stale evidence.
    Coordinator(Error),
    /// The claimed coin is not among its address's BTCB2 unspent outputs: it
    /// was spent on BTCB2 (by a third party or anyone), so step 2 cannot
    /// sweep it.
    ClaimedCoinSpent(OutPoint),
    /// Connect could not serve a fresh BTCB2 unspent read. Not a sign that a
    /// coin was spent (the indexer refuses addresses with very long
    /// histories, #615 N1).
    Unavailable(OutPoint, FailureKind),
}
impl From<Error> for SplitCheckError {
    fn from(error: Error) -> Self {
        Self::Coordinator(error)
    }
}
impl From<claim_workflow::Error> for SplitCheckError {
    fn from(error: claim_workflow::Error) -> Self {
        Self::Coordinator(Error::Journal(error))
    }
}

/// Order-independent digest of a set of outpoints. Refuses duplicates.
pub(crate) fn prevouts_digest(prevouts: &[OutPoint]) -> Option<sha256::Hash> {
    let set: BTreeSet<_> = prevouts.iter().copied().collect();
    if set.len() != prevouts.len() || set.is_empty() {
        return None;
    }
    let mut bytes = Vec::with_capacity(set.len() * 36);
    for outpoint in set {
        bytes.extend_from_slice(&consensus::serialize(&outpoint));
    }
    Some(sha256::Hash::hash(&bytes))
}

/// Step-two authority for one Split: fresh, short-lived evidence that the
/// tracked step 1 has six Bitcoin confirmations at the tip, RDTS still has
/// its margin, step 1 is absent on the fork and every claimed coin is still
/// unspent there. Only [`SplitPreparation::check_signing`] constructs it.
/// No Clone, no serialization; [`Self::redeem`] consumes it.
pub struct ForeignStep2Authorization {
    check: (u64, u64),
    /// The preparation's latest check revision; gone when it is dropped.
    latest: Weak<AtomicU64>,
    revoker: Revoker,
    generation: watch::Receiver<u64>,
    expected_generation: u64,
    not_after: Instant,
    fork_chain: ChainId,
    prevouts: sha256::Hash,
    tracked_txid: Txid,
}
impl std::fmt::Debug for ForeignStep2Authorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForeignStep2Authorization")
            .finish_non_exhaustive()
    }
}
/// Why a step-2 authorization was not redeemed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedeemError {
    /// Expired, superseded by a later check, revoked, or the session or
    /// preparation is gone.
    Stale,
    /// Another chain, generation, set of prevouts or tracked step-1 txid
    /// than the one checked.
    Mismatch,
}
/// Whether one check's evidence is still current, without its authority:
/// the same test as [`ForeignStep2Authorization::is_live`] (the check is the
/// preparation's latest, not revoked, same generation, before its
/// deadline), for display such as the "cannot replay" label (#636 P3-2).
/// It redeems nothing.
#[derive(Clone)]
pub struct Step2Liveness {
    check: u64,
    latest: Weak<AtomicU64>,
    revoker: Revoker,
    generation: watch::Receiver<u64>,
    expected_generation: u64,
    not_after: Instant,
}
impl std::fmt::Debug for Step2Liveness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Step2Liveness").finish_non_exhaustive()
    }
}
impl Step2Liveness {
    pub fn is_live(&self) -> bool {
        self.latest
            .upgrade()
            .is_some_and(|latest| latest.load(Ordering::Acquire) == self.check)
            && !self.revoker.is_revoked()
            && *self.generation.borrow() == self.expected_generation
            && self.generation.has_changed().is_ok()
            && Instant::now() < self.not_after
    }
    /// When the check's evidence lapses at the latest: the deadline
    /// [`Self::is_live`] checks. Display only, to redraw at it (S3 item 5);
    /// the evidence may lapse earlier (a later check, a revocation or a
    /// generation change).
    pub fn not_after(&self) -> Instant {
        self.not_after
    }
}
impl ForeignStep2Authorization {
    pub fn is_live(&self) -> bool {
        self.liveness().is_live()
    }
    /// The display-only liveness of this authorization's check.
    pub fn liveness(&self) -> Step2Liveness {
        Step2Liveness {
            check: self.check.1,
            latest: self.latest.clone(),
            revoker: self.revoker.clone(),
            generation: self.generation.clone(),
            expected_generation: self.expected_generation,
            not_after: self.not_after,
        }
    }
    /// The step-1 txid this authorization was checked for.
    pub fn tracked_txid(&self) -> Txid {
        self.tracked_txid
    }
    /// When this authorization's evidence lapses: the deadline of the check
    /// that minted it. Display only (the replay label); [`Self::is_live`]
    /// also needs the check, session and generation current.
    pub fn not_after(&self) -> Instant {
        self.not_after
    }
    /// Spend the authorization on exactly the checked prevouts of the fork
    /// chain, under the checked generation, for the checked step-1 txid
    /// (#626: the tracked txid is bound at redemption). One use: the value is
    /// consumed whether or not it redeems.
    pub(crate) fn redeem(
        self,
        chain: ChainId,
        generation: u64,
        prevouts: &[OutPoint],
        tracked_txid: Txid,
    ) -> Result<(), RedeemError> {
        if !self.is_live() {
            return Err(RedeemError::Stale);
        }
        if chain != self.fork_chain
            || generation != self.expected_generation
            || prevouts_digest(prevouts) != Some(self.prevouts)
            || tracked_txid != self.tracked_txid
        {
            return Err(RedeemError::Mismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
impl ForeignStep2Authorization {
    /// Test-only: a token shaped as `check_signing` mints it, for the
    /// redeemer's own tests. It stays live while the returned counter holds
    /// 1; storing anything else supersedes it, as a later check would.
    pub(crate) fn for_test(
        prevouts: &[OutPoint],
        tracked_txid: Txid,
        generation: watch::Receiver<u64>,
    ) -> (Self, Arc<AtomicU64>) {
        let latest = Arc::new(AtomicU64::new(1));
        let expected_generation = *generation.borrow();
        let token = Self {
            check: (0, 1),
            latest: Arc::downgrade(&latest),
            revoker: Revoker::new(),
            generation,
            expected_generation,
            not_after: Instant::now() + Duration::from_secs(60),
            fork_chain: ChainId::BitcoinBlake2b,
            prevouts: prevouts_digest(prevouts).expect("distinct prevouts"),
            tracked_txid,
        };
        (token, latest)
    }
    /// Test-only: revoke the token's session, as a logout would.
    pub(crate) fn revoke_for_test(&self) {
        self.revoker.revoke();
    }
}

/// Owns the Split journal after step 1 was submitted. Every new step-2
/// authorization needs another fresh check; see the module documentation.
pub struct SplitPreparation {
    id: u64,
    revision: u64,
    latest: Arc<AtomicU64>,
    context: Context,
    generation: watch::Receiver<u64>,
    controller: Controller,
    /// Each claimed prevout and the address its output pays.
    claimed: Vec<(OutPoint, String)>,
    prevouts: sha256::Hash,
    tracked_txid: Txid,
    services: Box<dyn SplitForkServices>,
    policy: CheckPolicy,
    revoker: Revoker,
    /// Step 2 (B3b): the BTCB2 tip of the latest check that minted a token.
    fork_tip: Option<coincube_core::claim::BlockRef>,
    /// The reserved target as last proven; see [`step2`].
    target: step2::TargetState,
    /// The unsigned step 2 built under a redeemed token.
    step2: Option<Arc<coincube_core::foreign_split::SplitStep2>>,
}
impl SplitPreparation {
    /// Reopen the Split journal in `directory`. `construction` and
    /// `verified` are the step 1 rebuilt from freshly authenticated coins
    /// and the recorded signed bytes verified against it (the panel's
    /// restore); both must match the journal exactly. Refused unless a
    /// submission of step 1 is recorded. Reopening restores no authority.
    #[allow(clippy::too_many_arguments)]
    pub fn resume(
        directory: &Path,
        target_cube: String,
        construction: &SplitStep1,
        verified: VerifiedSplitStep1,
        fork_height: u64,
        production: SplitForkProduction,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        let context = production.context.clone();
        let generation = production.generation.clone();
        Self::open(
            directory,
            target_cube,
            construction,
            verified,
            fork_height,
            context,
            generation,
            Box::new(production),
            policy,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn open(
        directory: &Path,
        target_cube: String,
        construction: &SplitStep1,
        verified: VerifiedSplitStep1,
        fork_height: u64,
        context: Context,
        generation: watch::Receiver<u64>,
        services: Box<dyn SplitForkServices>,
        policy: CheckPolicy,
    ) -> Result<Self, Error> {
        if !policy.valid() || construction.chain() != ChainId::Bitcoin {
            return Err(Error::Unsupported);
        }
        let mut unsigned = verified.transaction().clone();
        for input in &mut unsigned.input {
            input.script_sig = Default::default();
            input.witness.clear();
        }
        if verified.chain() != construction.chain()
            || verified.construction_txid() != construction.txid()
            || unsigned != construction.psbt().unsigned_tx
            || *generation.borrow() != context.generation
            || generation.has_changed().is_err()
        {
            return Err(Error::InvalidBinding);
        }
        let claimed = claimed_addresses(construction).ok_or(Error::InvalidBinding)?;
        let identity = claim_workflow::split_identity(target_cube, construction.source().digest());
        let mut controller =
            Controller::reopen_settling_blocking(directory, &identity, context.clone())?;
        controller.revalidate_split_construction(&context, construction, fork_height)?;
        controller.bind_recovered_split_transaction(&context, &verified)?;
        let tracked_txid = verified.transaction().compute_txid();
        let plan = controller.plan();
        if controller.recorded_split()?.is_none()
            || controller.phase() == Phase::Intent
            || controller.signed_txid() != Some(tracked_txid)
            || plan.step1_txid() != tracked_txid
            || claimed.iter().map(|(o, _)| *o).collect::<BTreeSet<_>>()
                != plan.claimed_prevouts.iter().copied().collect()
        {
            return Err(Error::InvalidBinding);
        }
        if controller.recorded_fork_submission().is_some() {
            return Err(Error::SubmissionAlreadyRecorded);
        }
        let prevouts = prevouts_digest(&plan.claimed_prevouts).ok_or(Error::InvalidBinding)?;
        Ok(Self {
            id: NEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .map_err(|_| Error::Revoked)?,
            revision: 0,
            latest: Arc::new(AtomicU64::new(0)),
            context,
            generation,
            controller,
            claimed,
            prevouts,
            tracked_txid,
            services,
            policy,
            revoker: Revoker::new(),
            fork_tip: None,
            target: step2::TargetState::default(),
            step2: None,
        })
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
    pub fn revoker(&self) -> Revoker {
        self.revoker.clone()
    }
    pub fn tracked_txid(&self) -> Txid {
        self.tracked_txid
    }
    fn current(&mut self, context: &Context) -> Result<(), Error> {
        if self.revoker.is_revoked()
            || context != &self.context
            || *self.generation.borrow() != context.generation
            || self.generation.has_changed().is_err()
        {
            self.revoker.revoke();
            self.controller.invalidate();
            return Err(Error::Revoked);
        }
        Ok(())
    }
    async fn collect(&self) -> Result<Collected, Error> {
        claim_observation::collect(
            self.services.source(),
            &self.controller.plan(),
            self.policy.observations,
            self.policy.collection_budget,
            CollectionContext {
                expected_generation: self.context.generation,
                generation: self.generation.clone(),
            },
        )
        .await
        .map(Collected::ordinary)
        .map_err(Error::Observation)
    }
    async fn claimed_unspent_on_fork(&self) -> Result<i64, SplitCheckError> {
        claimed_unspent_on_fork(self.services.as_ref(), &self.claimed, self.policy).await
    }
    /// Fresh step-2 authorization; see the module documentation. Each call
    /// supersedes every earlier authorization of this preparation, whether or
    /// not it succeeds.
    pub async fn check_signing(
        &mut self,
        context: &Context,
    ) -> Result<ForeignStep2Authorization, SplitCheckError> {
        self.current(context)?;
        self.revision = self.revision.checked_add(1).ok_or(Error::Revoked)?;
        self.latest.store(self.revision, Ordering::Release);
        self.fork_tip = None;
        let ticket = self.controller.begin_check(context)?;
        let first = self.collect().await?;
        if first.assessment != Assessment::ObservationsEligibleForPreflight {
            return Err(Error::NotReady(first.assessment).into());
        }
        let unspent_at = self.claimed_unspent_on_fork().await?;
        // The recheck: a second collection at the tips, which must be the
        // same view. A reorg or a new block in between refuses.
        let last = self.collect().await?;
        self.current(context)?;
        if !same_view(first.observations, last.observations) {
            return Err(Error::ChangedReview.into());
        }
        let plan = self.controller.plan();
        let observations = last.observations;
        if last.assessment != Assessment::ObservationsEligibleForPreflight
            || !completion_bitcoin_confirmed(&plan, observations.bitcoin)
            || observations.fork.chain != plan.fork_chain
            || observations.fork.step1_txid != self.tracked_txid
            || observations.fork.step1_presence
                != coincube_core::claim::ForkTransactionPresence::NotObserved
        {
            return Err(Error::NotReady(last.assessment).into());
        }
        let origin = Instant::now();
        let now = self.services.source().now();
        let mut remaining = self.policy.collection_budget.min(Duration::from_secs(30));
        for stamp in [
            observations.bitcoin.observed_at,
            observations.fork.observed_at,
            observations.deployment.observed_at,
            unspent_at,
        ] {
            let age = now
                .checked_sub(stamp)
                .filter(|age| *age >= 0 && stamp >= 0)
                .ok_or(Error::ExpiredEvidence)?;
            let seconds = self
                .policy
                .observations
                .max_observation_age_seconds
                .checked_sub(age)
                .and_then(|s| s.checked_sub(1))
                .filter(|s| *s > 0)
                .ok_or(Error::ExpiredEvidence)?;
            remaining = remaining.min(Duration::from_secs(seconds as u64));
        }
        let not_after = origin
            .checked_add(remaining)
            .ok_or(Error::ExpiredEvidence)?;
        let assessment = last.assessment;
        let status = last.apply(
            &mut self.controller,
            ticket,
            context,
            self.policy.observations,
            now,
        )?;
        if status != Status::Observation(Assessment::ObservationsEligibleForPreflight) {
            return Err(Error::NotReady(assessment).into());
        }
        self.current(context)?;
        self.fork_tip = Some(observations.fork.tip);
        Ok(ForeignStep2Authorization {
            check: (self.id, self.revision),
            latest: Arc::downgrade(&self.latest),
            revoker: self.revoker.clone(),
            generation: self.generation.clone(),
            expected_generation: self.context.generation,
            not_after,
            fork_chain: plan.fork_chain,
            prevouts: self.prevouts,
            tracked_txid: self.tracked_txid,
        })
    }
}
// No Drop: a dropped preparation takes its check counter with it, so every
// token it minted stops being live (`ForeignStep2Authorization::is_live`).
// `finish` revokes explicitly before handing the journal on.

/// Every claimed coin among its address's fresh BTCB2 unspent outputs: the
/// step-2 check, and a step-2 resend (P3-3). Returns the oldest read's stamp.
async fn claimed_unspent_on_fork(
    services: &dyn SplitForkServices,
    claimed: &[(OutPoint, String)],
    policy: CheckPolicy,
) -> Result<i64, SplitCheckError> {
    let mut oldest = i64::MAX;
    for (outpoint, address) in claimed {
        let read = services
            .btcb2_unspent(address)
            .await
            .map_err(|kind| SplitCheckError::Unavailable(*outpoint, kind))?;
        let now = services.source().now();
        if read.observed_at() < 0
            || !now.checked_sub(read.observed_at()).is_some_and(|age| {
                (0..=policy.observations.max_observation_age_seconds).contains(&age)
            })
        {
            return Err(SplitCheckError::Unavailable(*outpoint, FailureKind::Stale));
        }
        if !read.value().contains(outpoint) {
            return Err(SplitCheckError::ClaimedCoinSpent(*outpoint));
        }
        oldest = oldest.min(read.observed_at());
    }
    Ok(oldest)
}

/// Each claimed prevout of `construction` and the address its output pays,
/// from the construction's own txid-bound previous transactions. `None` if
/// any input has no previous output or no address.
fn claimed_addresses(construction: &SplitStep1) -> Option<Vec<(OutPoint, String)>> {
    let psbt = construction.psbt();
    psbt.unsigned_tx
        .input
        .iter()
        .zip(&psbt.inputs)
        .map(|(txin, input)| {
            let outpoint = txin.previous_output;
            let script = match (&input.non_witness_utxo, &input.witness_utxo) {
                (Some(previous), _) => {
                    if previous.compute_txid() != outpoint.txid {
                        return None;
                    }
                    previous
                        .output
                        .get(usize::try_from(outpoint.vout).ok()?)?
                        .script_pubkey
                        .clone()
                }
                (None, Some(output)) => output.script_pubkey.clone(),
                (None, None) => return None,
            };
            let address = Address::from_script(&script, Network::Bitcoin).ok()?;
            Some((outpoint, address.to_string()))
        })
        .collect()
}

pub mod step2;

#[cfg(all(test, unix))]
mod tests;
