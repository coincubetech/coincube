//! Split (#568 B4b-1b) fork-only (`kind: Unified`) journal under the step-2
//! reconciler. The reconciler's reopen admits a fork-only record once its
//! submission is recorded, as it does a two-step one: the record reuses the
//! step-2 fields. Its reconcile, though, runs the step-1-centric observation
//! path, which refuses the fork-only plan before any read; the fork-only
//! observation path is B4b-3's. Both are pinned here so lifting either is a
//! deliberate change.
use super::*;
use crate::services::{
    claim_coordinator::fork::split::step2::SplitStep2Reconciler,
    claim_observation::{Failure, FailureKind, Stage},
    claim_workflow::UnifiedConstruction,
};
use coincube_core::{
    foreign_split::{create_split_step2, SplitStep2, SplitStep2Inputs},
    miniscript::bitcoin::{hashes::sha256, Script},
};

/// The unified sweep of the harness wallet's coins into `target` (B4b-1a's
/// shape: one output, no change), built with core's step-2 construction over
/// the same coins, and its signed transaction. It is signed `ALL` here: the
/// journal records bytes, and the `ALL|UNIFIED` request is core's finalizer's
/// (B4b-1a).
fn unified_sweep(wallet: &Wallet, target: &Script) -> (SplitStep2, Transaction) {
    let coins = coins(wallet);
    let claimed: Vec<OutPoint> = coins.iter().map(|c| c.outpoint).collect();
    let construction = create_split_step2(
        &SplitStep2Inputs {
            chain: ChainId::BitcoinBlake2b,
            source: &wallet.source,
            coins: &coins,
            fork_height: FORK,
            claimed: &claimed,
            target,
        },
        2,
        LockTime::from_height(100).unwrap(),
        100,
    )
    .unwrap();
    let secp = Secp256k1::new();
    let mut psbt = construction.psbt().clone();
    psbt.sign(&wallet.signer, &secp).unwrap();
    let signed = finalize_split_step2(&construction, &coins, &wallet.source, &psbt, &secp)
        .unwrap()
        .transaction()
        .clone();
    (construction, signed)
}

/// Services nothing here may reach: every read panics.
struct Unreachable;
#[async_trait]
impl ObservationSource for Unreachable {
    fn now(&self) -> i64 {
        unreachable!("no read is made")
    }
    async fn anchor(&self, _: ChainId) -> Result<NetworkAnchorStatus, FailureKind> {
        unreachable!("no read is made")
    }
    async fn tip(&self, _: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        unreachable!("no read is made")
    }
    async fn transaction(
        &self,
        _: ChainId,
        _: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        unreachable!("no read is made")
    }
    async fn hash_at_height(
        &self,
        _: ChainId,
        _: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        unreachable!("no read is made")
    }
}
#[async_trait]
impl SplitForkServices for Unreachable {
    fn source(&self) -> &dyn ObservationSource {
        self
    }
    fn origin(&self) -> &str {
        ORIGIN
    }
    async fn btcb2_unspent(&self, _: &str) -> Result<FreshRead<Vec<OutPoint>>, FailureKind> {
        unreachable!("no read is made")
    }
    async fn address_used(&self, _: ChainId, _: &str) -> Result<FreshRead<bool>, FailureKind> {
        unreachable!("no read is made")
    }
}

/// A fork-only journal of the harness wallet, its source digest, and its
/// signed sweep; the submission is recorded when `submitted`.
fn fork_only_journal(submitted: bool) -> (Temp, sha256::Hash, Transaction) {
    let wallet = wallet();
    let target = address(&vault(), 5).script_pubkey();
    let (construction, signed) = unified_sweep(&wallet, &target);
    let temp = Temp::new();
    let mut controller = Controller::create_unified_split(
        &temp.0,
        TARGET.into(),
        UnifiedConstruction {
            chain: ChainId::BitcoinBlake2b,
            source: &wallet.source,
            fork_height: FORK,
            target_index: 5,
            target_script: &target,
            unsigned: &construction.psbt().unsigned_tx,
        },
        context(),
    )
    .unwrap();
    assert_ne!(
        signed.compute_txid(),
        construction.txid(),
        "pkh: the txids differ"
    );
    if submitted {
        controller
            .record_unified_broadcast_intent(&context(), ChainId::BitcoinBlake2b, &signed)
            .unwrap();
    }
    (temp, wallet.source.digest(), signed)
}
fn reconciler(
    temp: &Temp,
    digest: sha256::Hash,
    sender: &watch::Sender<u64>,
) -> Result<SplitStep2Reconciler, Error> {
    SplitStep2Reconciler::open(
        &temp.0,
        TARGET.into(),
        digest,
        context(),
        sender.subscribe(),
        Box::new(Unreachable),
        policy(),
    )
}

/// B4b-1b: the reconciler's reopen admits a fork-only journal whose
/// submission is recorded, identifies that submission (the signed sweep's
/// own txid and wtxid) and holds the journal; one without a submission is
/// refused, as a two-step one is. No read is made.
#[test]
fn split_step2_reconciler_reopens_a_fork_only_journal() {
    let (sender, _) = watch::channel(7);
    let (unsubmitted, digest, _) = fork_only_journal(false);
    assert!(matches!(
        reconciler(&unsubmitted, digest, &sender),
        Err(Error::InvalidBinding)
    ));
    let (temp, digest, signed) = fork_only_journal(true);
    let reconciler = reconciler(&temp, digest, &sender).unwrap();
    assert_eq!(
        reconciler.recorded_outcome(),
        Some(Outcome::Uncertain {
            txid: signed.compute_txid(),
            wtxid: signed.compute_wtxid(),
        })
    );
    assert!(matches!(
        Controller::reopen(
            &temp.0,
            &claim_workflow::split_identity(TARGET.into(), digest),
            context()
        ),
        Err(claim_workflow::Error::Busy)
    ));
    drop(reconciler);
    let controller = Controller::reopen(
        &temp.0,
        &claim_workflow::split_identity(TARGET.into(), digest),
        context(),
    )
    .unwrap();
    assert_eq!(controller.recorded_split_step2(), Some(&signed));
}

/// B4b-1b limitation, pinned on the reconciler: `reconcile_sweep` on a
/// fork-only journal is refused by the step-2 observation path's plan check
/// (`claim_observation::collect_sweep`, step-1-centric) before any read is
/// made, and records nothing. B4b-3's fork-only observation path lifts this.
#[tokio::test]
async fn split_step2_reconcile_refuses_a_fork_only_journal_before_any_read() {
    let (sender, _) = watch::channel(7);
    let (temp, digest, signed) = fork_only_journal(true);
    let mut reconciler = reconciler(&temp, digest, &sender).unwrap();
    assert!(matches!(
        reconciler.reconcile_sweep(&context()).await,
        Err(Error::Observation(Failure {
            stage: Stage::Plan,
            kind: FailureKind::InvalidPlan,
        }))
    ));
    drop(reconciler);
    let controller = Controller::reopen(
        &temp.0,
        &claim_workflow::split_identity(TARGET.into(), digest),
        context(),
    )
    .unwrap();
    assert!(!controller.split_step2_observed());
    assert_eq!(
        controller.recorded_fork_submission().map(|s| s.txid()),
        Some(signed.compute_txid())
    );
    assert_eq!(controller.recorded_split_step2(), Some(&signed));
}
